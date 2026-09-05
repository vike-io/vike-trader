//! Backtesting the `vike-mm` `SpreadMaker` through the sim engine — the step-3 deliverable:
//! `SimBroker` now `impl HftBroker`, and resting TAGGED limit quotes fill on a crossing move via
//! [`StrategyEngine::fill_tagged`]. These are BEHAVIORAL / plumbing checks: does the maker rest a
//! two-sided quote, get filled when the market crosses it, capture the spread on a round-trip, skew
//! its sizes with inventory, and pull-then-resume a side under the fill-rate breaker?
//!
//! FILL MODEL — SIMPLE CROSSING (see `engine::StrategyEngine::fill_tagged` for the full contract):
//! a resting bid (buy limit) fills when the market moves at-or-below its price; a resting ask at-or-
//! above. It reuses the golden-gated `order_fill_price` (Bar model) `OrderKind::Limit` branch, so it
//! can never drift from the untagged fill math. It is deliberately NOT queue-position aware: a quote
//! fills the instant price touches it (as if always first in the queue), in full, with a free
//! modify-in-place. That makes sim maker fills/PnL an OPTIMISTIC bound — full L2 queue-position
//! modeling is the R8 follow-up (`L2BookFillModel`). Do not read these PnLs as a fill-rate forecast.
//!
//! The maker is driven off L1 QUOTE ticks (its `on_quote_tick` lane), replayed through `run_ticks`.
//! Fee/slippage are zero (default `EngineParams`), so every asserted price/size/PnL is exact.

use vike_backtest::{BacktestResult, EngineParams, StrategyEngine, Tick};
use vike_mm::SpreadMaker;
use vike_model::{
    AsParams, HorizonMode, KappaMode, OrderKind, QuoteStyle, QuoteTick, VarianceMode,
};

const SYM: &str = "TOK";

/// One L1 quote at `ts` (ms). The maker's `Mid` style quotes off `mid = (bid + ask) / 2`.
fn q(ts: i64, bid: f64, ask: f64) -> Tick {
    Tick::Quote(QuoteTick {
        ts,
        local_ts: 0,
        bid,
        ask,
        bid_size: 1.0,
        ask_size: 1.0,
        symbol: SYM.to_string(),
    })
}

/// Mount `maker` on a single-symbol sim engine and replay `ticks` through the tick path. Returns the
/// engine (read its resting `tagged` quotes + net position back) and the run result (trades/fills).
fn run_maker(
    maker: SpreadMaker,
    ticks: Vec<Tick>,
    mirror: bool,
) -> (StrategyEngine<SpreadMaker>, BacktestResult) {
    let params = EngineParams { mirror, ..Default::default() };
    let mut eng = StrategyEngine::new(vec![(SYM.to_string(), Vec::new())], maker, params);
    let result = eng.run_ticks(&[(SYM.to_string(), ticks)]);
    (eng, result)
}

/// The maker rests a two-sided quote: after one stable quote it has a resting bid at `mid − h` and a
/// resting ask at `mid + h`, each of base size, and no position (nothing has crossed).
#[test]
fn maker_rests_two_sided_quotes() {
    // qty 1, half-spread 0.5; two identical mid=100 quotes (market never moves → no fills).
    let (eng, result) =
        run_maker(SpreadMaker::new(1.0, 0.5), vec![q(1, 99.9, 100.1), q(2, 99.9, 100.1)], false);

    let tagged = &eng.core.sym[0].tagged;
    assert_eq!(tagged.len(), 2, "a two-sided maker rests exactly a bid and an ask");

    let bid = tagged.get("bid").expect("bid rests");
    assert_eq!(bid.kind, OrderKind::Limit);
    assert_eq!(bid.side, 1);
    assert_eq!(bid.size, 1.0);
    assert_eq!(bid.price, Some(99.5)); // mid 100 − half_spread 0.5

    let ask = tagged.get("ask").expect("ask rests");
    assert_eq!(ask.kind, OrderKind::Limit);
    assert_eq!(ask.side, -1);
    assert_eq!(ask.size, 1.0);
    assert_eq!(ask.price, Some(100.5)); // mid 100 + half_spread 0.5

    assert_eq!(eng.core.sym[0].pos.size, 0.0, "nothing crossed → flat");
    assert!(result.trades.is_empty(), "no round-trips yet");
}

/// A resting bid fills when the market crosses DOWN through it (the maker goes long as a maker), and
/// the offsetting ask then fills on the way back UP — a completed round-trip that captures the
/// spread. Both fills are MAKER fills.
#[test]
fn resting_bid_fills_then_ask_completes_round_trip() {
    // qty 1, half-spread 1.0 → first quote (mid 100) rests bid@99, ask@101.
    let ticks = vec![
        q(1, 99.5, 100.5),  // mid 100 → rest bid@99, ask@101
        q(2, 98.5, 99.5),   // mid 99  → crosses the bid@99 (fill, long +1); ask re-quotes to 100
        q(3, 100.5, 101.5), // mid 101 → crosses the ask@100 (fill, flat) — round-trip closed
    ];
    let (eng, result) = run_maker(SpreadMaker::new(1.0, 1.0), ticks, true);

    // Round-trip closed → flat, and exactly ONE completed trade capturing the 99→101 spread.
    assert_eq!(eng.core.sym[0].pos.size, 0.0, "round-trip returns to flat");
    assert_eq!(result.trades.len(), 1);
    let trade = &result.trades[0];
    assert_eq!(trade.entry_price, 99.0);
    assert_eq!(trade.exit_price, 101.0);
    assert_eq!(trade.pnl, 2.0, "bought @99 as a maker, sold @101 → +2 gross");
    assert!(trade.is_long, "the maker bought first (long round-trip)");

    // Both legs were MAKER fills (passive resting limits), buy first then sell.
    let fills = eng.core.mirror_fills.as_ref().expect("mirror enabled");
    assert_eq!(fills.len(), 2, "one bid fill + one ask fill");
    assert_eq!(fills[0].side, 1);
    assert_eq!(fills[0].price, 99.0);
    assert!(fills[0].is_maker, "the resting bid fills as a MAKER");
    assert_eq!(fills[1].side, -1);
    assert_eq!(fills[1].price, 101.0);
    assert!(fills[1].is_maker, "the resting ask fills as a MAKER");
}

/// Inventory-skew shapes BOTH quote sizes: with a non-zero `target_inventory` the maker, even while
/// flat, leans toward its target — a bigger bid than ask when the target is above the position
/// (wants to BUY toward it). Pure `skew_multipliers` arithmetic, observed through the resting sizes.
#[test]
fn inventory_skew_shapes_both_quote_sizes() {
    // target +1, band 2, skew 0.5, flat position → imbalance = (0 − 1)/2 = −0.5;
    // bid_mult = 1 − 0.5·(−0.5) = 1.25, ask_mult = 1 + 0.5·(−0.5) = 0.75.
    let maker = SpreadMaker::new(1.0, 0.5).with_skew(1.0, 2.0, 0.5);
    let (eng, _) = run_maker(maker, vec![q(1, 99.9, 100.1)], false);

    let tagged = &eng.core.sym[0].tagged;
    let bid = tagged.get("bid").expect("bid rests");
    let ask = tagged.get("ask").expect("ask rests");
    assert_eq!(bid.size, 1.25, "bid grows: the maker leans to BUY toward its +1 target");
    assert_eq!(ask.size, 0.75, "ask shrinks symmetrically");
    assert!(bid.size > ask.size, "skew biases toward the target");
    // prices are unaffected by skew (skew shapes SIZES, the style prices)
    assert_eq!(bid.price, Some(99.5));
    assert_eq!(ask.price, Some(100.5));
}

/// Inventory-skew RESPONDS to a real filled position: once a bid fill leaves the maker long, it
/// re-quotes a LARGER offsetting ask (to sell the inventory back down). Uses skew target 0 so the
/// long position itself drives the skew.
#[test]
fn inventory_skew_responds_to_filled_position() {
    // qty 1, half-spread 0.5, skew(target 0, band 1, intensity 0.5).
    let maker = SpreadMaker::new(1.0, 0.5).with_skew(0.0, 1.0, 0.5);
    let ticks = vec![
        q(1, 99.9, 100.1), // mid 100 → rest bid@99.5, ask@100.5 (flat → both size 1.0)
        q(2, 99.4, 99.6),  // mid 99.5 → crosses bid@99.5 (long +1); ask re-quotes skewed
    ];
    let (eng, _) = run_maker(maker, ticks, false);

    assert_eq!(eng.core.sym[0].pos.size, 1.0, "the bid filled → maker is long 1");
    let tagged = &eng.core.sym[0].tagged;
    // long +1 with band 1, skew 0.5 → ask_mult = 1 + 0.5·1 = 1.5.
    let ask = tagged.get("ask").expect("ask still rests");
    assert_eq!(ask.size, 1.5, "long inventory grows the offsetting ask (lean to sell down)");
    // The filled bid is not re-placed: the maker only MODIFIES a live side and this one went
    // terminal on the fill (its tag was retired). Re-arming a filled side is the breaker's job
    // (next test). This mirrors the live tag→coid semantics (modify on a gone tag is a no-op).
    assert!(!tagged.contains_key("bid"), "a filled side is not silently re-quoted");
}

/// The per-side fill-rate breaker RESPONDS: after a one-sided bid fill trips it, the bid is
/// suppressed for the cooldown and then RE-QUOTED once it expires. We contrast a breaker maker with
/// a plain maker on the SAME stream — only the breaker maker ends with a resting bid again, proving
/// the suppress→cooldown→resume cycle actually fired (a plain maker leaves the filled side down).
#[test]
fn fill_breaker_suppresses_then_resumes_the_hit_side() {
    // Stream: rest (t1), cross the bid (t2 → trips the breaker, arms cooldown to t=3000),
    // stay within cooldown (t2500), then past it (t3500). Market recovers to mid 99.6 so a
    // non-suppressed maker WOULD re-bid — isolating the breaker's suppression as the cause.
    let stream = || {
        vec![
            q(1000, 99.9, 100.1), // mid 100.0 → rest bid@99.5, ask@100.5
            q(2000, 99.4, 99.6),  // mid 99.5 → cross bid@99.5 (long +1)
            q(2500, 99.5, 99.7),  // mid 99.6, still within cooldown (< 3000)
            q(3500, 99.5, 99.7),  // mid 99.6, cooldown expired (>= 3000)
        ]
    };

    // window 100_000ms, trip at net 1.0 (a single 1.0 bid fill trips it), cooldown 1000ms.
    let brk = SpreadMaker::new(1.0, 0.5).with_fill_breaker(100_000, 1.0, 1000);
    let (brk_eng, _) = run_maker(brk, stream(), false);

    let plain = SpreadMaker::new(1.0, 0.5); // identical, breaker OFF
    let (plain_eng, _) = run_maker(plain, stream(), false);

    // Both went long on the bid fill and hold it (the ask never crossed).
    assert_eq!(brk_eng.core.sym[0].pos.size, 1.0);
    assert_eq!(plain_eng.core.sym[0].pos.size, 1.0);

    // The breaker maker RE-QUOTED its bid after the cooldown expired; the plain maker never does.
    assert!(
        brk_eng.core.sym[0].tagged.contains_key("bid"),
        "breaker maker resumes the bid after the suppression cooldown",
    );
    assert!(
        !plain_eng.core.sym[0].tagged.contains_key("bid"),
        "plain maker leaves the filled side down (no suppress/resume cycle)",
    );
    // Both keep quoting the ask throughout (only the bid side was hit / suppressed).
    assert!(brk_eng.core.sym[0].tagged.contains_key("ask"));
    assert!(plain_eng.core.sym[0].tagged.contains_key("ask"));
}

// ---- Avellaneda–Stoikov pricing layer (audit mm-quote), mounted through the sim engine ----
//
// These drive the A-S maker over synthetic 0–1 (Polymarket-shaped) price ticks and assert the
// pricing PROPERTIES that need no PnL: it quotes around the reservation price, skews with filled
// inventory, and tightens toward the 0/1 walls (the bounded-variance hallmark). A deterministic
// config — PureBernoulli variance (so V = p(1−p), no σ warmup) + a constant horizon (no resolution
// blackout) + a FIXED κ — makes every quoted price exact. `q_scale = 1` so the position IS q_norm.

/// The deterministic A-S config used by the mounted backtest checks (tick grid set separately, on
/// the maker, via `with_quote_style`).
fn as_bt_params(gamma: f64) -> AsParams {
    AsParams {
        gamma,
        horizon_mode: HorizonMode::ConstantTau,
        variance_mode: VarianceMode::PureBernoulli,
        kappa_mode: KappaMode::Fixed,
        kappa_default: 50.0,
        q_scale: 1.0,
        min_standoff_ticks: 1.0,
        resolution_ts: None,
        resolution_blackout_ms: 0,
        ..AsParams::default()
    }
}

/// An A-S maker on the 0.01 (Polymarket) grid: `with_quote_style(Mid, 1, 0.01)` sets the L1 tick
/// grid the A-S snap/wall-clamp reads; `Mid`'s `half_spread` is unused once A-S prices.
fn as_maker(gamma: f64) -> SpreadMaker {
    SpreadMaker::new(1.0, 0.5)
        .with_quote_style(QuoteStyle::Mid, 1, 0.01)
        .with_avellaneda_stoikov(as_bt_params(gamma))
}

/// The A-S maker rests a two-sided quote SYMMETRIC around the fair mid when flat: a resting bid and
/// ask equidistant from 0.50, both on the 0.01 grid, strictly inside the walls — the reservation
/// price equals the mid at zero inventory, so the only asymmetry (there is none here) would be skew.
#[test]
fn avellaneda_stoikov_maker_quotes_symmetrically_around_the_mid_when_flat() {
    // stable mid 0.50, no crossing → no fills, maker stays flat
    let (eng, result) = run_maker(as_maker(0.5), vec![q(1, 0.49, 0.51), q(2, 0.49, 0.51)], false);

    let tagged = &eng.core.sym[0].tagged;
    let bid = tagged.get("bid").expect("bid rests").price.expect("priced");
    let ask = tagged.get("ask").expect("ask rests").price.expect("priced");

    assert!(bid < 0.5 && 0.5 < ask, "quotes straddle the fair mid: {bid}/{ask}");
    // symmetric about 0.50 (flat ⇒ r == s), so bid + ask == 1.0 on this grid
    assert!(((0.5 - bid) - (ask - 0.5)).abs() < 1e-9, "equidistant from the mid: {bid}/{ask}");
    // on the 0.01 grid
    assert!(((bid / 0.01).round() - bid / 0.01).abs() < 1e-6, "bid on grid: {bid}");
    assert!(((ask / 0.01).round() - ask / 0.01).abs() < 1e-6, "ask on grid: {ask}");
    assert_eq!(eng.core.sym[0].pos.size, 0.0, "nothing crossed → flat");
    assert!(result.trades.is_empty(), "no round-trips");
}

/// Filled inventory SKEWS the quotes: once a resting bid fills and the maker is long, the reservation
/// price drops below the mid, so the resting ask is re-priced LOWER than a FLAT maker's ask at the
/// SAME mid (the maker leans to sell its inventory back down). We compare the long maker's ask at mid
/// 0.41 against a flat maker's ask at mid 0.41, so the ONLY difference is the inventory. (Note the
/// long run is read right after the fill: a further recovery would simply cross that aggressive ask
/// and close the round-trip — the maker working as intended.)
#[test]
fn avellaneda_stoikov_maker_skews_the_ask_down_when_long() {
    // FLAT baseline at mid 0.41: a stable quote, nothing crosses → ask rests at the flat A-S price.
    let (flat_eng, _) = run_maker(as_maker(0.5), vec![q(1, 0.40, 0.42)], false);
    assert_eq!(flat_eng.core.sym[0].pos.size, 0.0, "baseline stays flat");
    let ask_flat = flat_eng.core.sym[0].tagged.get("ask").expect("flat ask").price.expect("priced");

    // LONG run: rest at mid 0.50, then a dip to mid 0.41 crosses the resting bid (~0.42) → long +1,
    // and the ask is re-priced at the SAME mid 0.41 but now carrying the inventory skew.
    let long_ticks = vec![
        q(1, 0.49, 0.51), // mid 0.50 → rest the A-S bid (~0.42) / ask
        q(2, 0.40, 0.42), // mid 0.41 crosses the bid → long +1; ask re-quotes skewed at mid 0.41
    ];
    let (long_eng, _) = run_maker(as_maker(0.5), long_ticks, false);
    assert_eq!(long_eng.core.sym[0].pos.size, 1.0, "the resting bid filled → maker is long 1");
    let ask_long = long_eng.core.sym[0].tagged.get("ask").expect("long ask").price.expect("priced");

    // same mid (0.41), differing only in inventory: long ⇒ the ask is pulled DOWN toward the mid.
    assert!(ask_long < ask_flat, "long inventory skews the ask DOWN: {ask_long} !< {ask_flat}");
    assert!(ask_long > 0.41, "but the posted ask never crosses the fair mid (standoff clamp)");
}

/// The Bernoulli-bounded variance TIGHTENS the spread toward the 0/1 walls: a flat maker at mid 0.90
/// (near the upper wall, `V = 0.9·0.1 = 0.09`) quotes a NARROWER spread than at mid 0.50
/// (`V = 0.25`, the peak). This is the 0–1 adaptation's signature — risk (and thus spread) vanishes
/// as the outcome becomes near-certain.
#[test]
fn avellaneda_stoikov_maker_spread_tightens_toward_the_walls() {
    let (mid_eng, _) = run_maker(as_maker(0.5), vec![q(1, 0.49, 0.51), q(2, 0.49, 0.51)], false);
    let mid_t = &mid_eng.core.sym[0].tagged;
    let mid_spread =
        mid_t.get("ask").unwrap().price.unwrap() - mid_t.get("bid").unwrap().price.unwrap();

    let (wall_eng, _) = run_maker(as_maker(0.5), vec![q(1, 0.89, 0.91), q(2, 0.89, 0.91)], false);
    let wall_t = &wall_eng.core.sym[0].tagged;
    let wall_bid = wall_t.get("bid").unwrap().price.unwrap();
    let wall_ask = wall_t.get("ask").unwrap().price.unwrap();
    let wall_spread = wall_ask - wall_bid;

    assert!(wall_bid < 0.9 && 0.9 < wall_ask, "still straddles the mid near the wall");
    assert!(
        wall_spread < mid_spread,
        "spread tighter near the wall: {wall_spread} !< {mid_spread}"
    );
}
