//! The session / market-hours law, backtest side (law-map T10).
//!
//! ONE law, served from `vike_model::session`'s rows, keyed PER SYMBOL and consulted by every fill
//! lane through `EngineParams::session_gate`. These gates pin the things that matter:
//!
//! 1. **The default is inert.** `session_gate: false` — and `true` with no calendar for a symbol,
//!    and `true` on a 24/7 market — all produce byte-identical runs. Every parity/golden suite runs
//!    with the gate off, so this is the property that keeps them untouched.
//! 2. **When on, NOTHING fills outside a symbol's session.** Resting orders defer (they are not
//!    canceled and not lost — they fill at the reopen), the tagged maker lane is skipped, and an
//!    armed protective stop does NOT fire over the weekend; it fires at the reopen, into the gap.
//!    This holds on the frozen fill lanes AND the opt-in `queue_model` tick lanes — a closed market
//!    crosses no queued resting limit and no queued maker tag either (the `queued_*` gates below).
//! 3. **Keyed by (venue, ASSET CLASS), resolved PER SYMBOL.** Venue-alone keying is the defect the
//!    redo fixes: Alpaca trades cash equities AND 24/7 crypto on one venue. The engine resolves a
//!    calendar per symbol from `EngineParams::session_calendars`, which a caller builds with
//!    `vike_catalog::session_calendar_for(venue, asset_class)` — so one run gates each instrument by
//!    its OWN market, which a single `default_venue` per run cannot.
//!
//! The chosen semantics — **defer the fill, keep processing the bar** — deliberately mirrors how
//! the engine already treats a no-trade bar (see `stale_price_wait.rs`): the strategy still sees
//! every bar, indicators still fold, only the FILL is refused. Skipping the bar entirely would
//! hide the tape from the strategy and silently desynchronize any indicator built on it.

use indexmap::IndexMap;
use vike_backtest::engine::Tick;
use vike_backtest::{EngineParams, QueueModelKind, StrategyEngine};
use vike_catalog::{session_calendar_for, AssetClass};
use vike_model::session::{CRYPTO_24_7, US_EQUITY_REGULAR};
use vike_model::{
    days_from_civil, Bar, BookUpdate, BookUpdateKind, Broker, Fill, HftBroker, L2Book,
    SessionCalendar, Strategy, TradeTick,
};

const SYM: &str = "EURUSD";

/// Epoch-ms for a UTC instant, via the crate's own civil-calendar math.
fn ms(y: i64, mo: u32, d: u32, h: i64) -> i64 {
    days_from_civil(y, mo, d) * 86_400_000 + h * 3_600_000
}

fn bar(ts: i64, o: f64, h: f64, l: f64) -> Bar {
    Bar {
        ts,
        open: o,
        high: h,
        low: l,
        close: o,
        volume: 100.0,
        funding: None,
        bid: None,
        ask: None,
        symbol: None,
    }
}

/// A tape straddling the FX weekend. 2024-01-05 is a Friday, 2024-01-07 the Sunday.
/// Each bar's open is distinct so an assertion names exactly which bar filled.
///
/// idx  when (UTC)          FX session
///  0   Fri 20:00           OPEN
///  1   Fri 21:00           OPEN
///  2   Fri 22:00           CLOSED (the week closes at Fri 22:00 UTC)
///  3   Sat 12:00           CLOSED
///  4   Sun 20:00           CLOSED
///  5   Sun 21:00           OPEN   (the DST-union reopen)
///  6   Mon 00:00           OPEN
fn weekend_tape() -> Vec<Bar> {
    vec![
        bar(ms(2024, 1, 5, 20), 100.0, 101.0, 99.0),
        bar(ms(2024, 1, 5, 21), 110.0, 111.0, 109.0),
        bar(ms(2024, 1, 5, 22), 120.0, 121.0, 119.0),
        bar(ms(2024, 1, 6, 12), 130.0, 131.0, 129.0),
        bar(ms(2024, 1, 7, 20), 140.0, 141.0, 139.0),
        bar(ms(2024, 1, 7, 21), 150.0, 151.0, 149.0),
        bar(ms(2024, 1, 8, 0), 160.0, 161.0, 159.0),
    ]
}

/// Costless params (no fees/slippage) so fill prices assert exactly, keyed by `default_venue`.
fn params(venue: Option<&str>, gate: bool) -> EngineParams {
    EngineParams {
        cash: 1_000_000.0,
        default_venue: venue.map(str::to_string),
        session_gate: gate,
        ..EngineParams::default()
    }
}

/// Costless params with an explicit PER-SYMBOL calendar map (no `default_venue`).
fn params_calendars(cals: &[(&str, SessionCalendar)]) -> EngineParams {
    let mut session_calendars = IndexMap::new();
    for (s, c) in cals {
        session_calendars.insert((*s).to_string(), *c);
    }
    EngineParams {
        cash: 1_000_000.0,
        session_gate: true,
        session_calendars,
        ..EngineParams::default()
    }
}

/// Trades 1.0 market on bar `at` in direction `side`, optionally with a protective stop.
/// Collects fills.
struct TradeOnce {
    at: usize,
    side: i32,
    stop: Option<f64>,
    fills: Vec<Fill>,
}

impl Strategy<vike_backtest::SimBroker> for TradeOnce {
    fn on_bar(&mut self, ctx: &mut vike_backtest::SimBroker, _bar: &Bar) {
        if ctx.index == self.at {
            ctx.submit(SYM, self.side, 1.0, 0.0, false, self.stop);
        }
    }
    fn on_fill(&mut self, _ctx: &mut vike_backtest::SimBroker, fill: &Fill) {
        self.fills.push(fill.clone());
    }
}

fn run(venue: Option<&str>, gate: bool, at: usize) -> StrategyEngine<TradeOnce> {
    run_side(venue, gate, at, 1, None)
}

fn run_side(
    venue: Option<&str>,
    gate: bool,
    at: usize,
    side: i32,
    stop: Option<f64>,
) -> StrategyEngine<TradeOnce> {
    let mut eng = StrategyEngine::new(
        vec![(SYM.into(), weekend_tape())],
        TradeOnce { at, side, stop, fills: Vec::new() },
        params(venue, gate),
    );
    eng.run();
    eng
}

// ---- 1. the default is inert ----

/// The baseline the whole feature is measured against: with the gate OFF the market order
/// submitted on bar 1 fills at bar 2's open — a bar that is squarely inside the FX weekend.
/// This is the pre-feature behavior, and it is what the gate changes.
#[test]
fn gate_off_fills_straight_through_the_weekend() {
    let eng = run(Some("dukascopy"), false, 1);
    let fills = &eng.strategy.fills;
    assert_eq!(fills.len(), 1, "{fills:?}");
    assert_eq!(fills[0].price, 120.0, "bar 2 open — Friday 22:00 UTC, the venue is shut");
    assert_eq!(fills[0].ts, ms(2024, 1, 5, 22));
    assert_eq!(eng.core.session_deferrals, 0, "gate off ⇒ never counted");
}

/// Three configurations that must all be byte-identical to the gate-off baseline: the gate off,
/// the gate ON but with no calendar for the symbol (nothing to look up ⇒ fail-permissive always
/// open), and the gate ON for a 24/7 venue. This is the property every parity/golden suite relies
/// on — they all run with `session_gate: false`, whose code path is an EMPTY per-symbol session
/// vec (`SimBroker::session_closed` returns `false` from one `Vec::get`).
#[test]
fn inert_configurations_match_the_ungated_run_exactly() {
    let baseline = run(Some("dukascopy"), false, 1);
    let base_fills = baseline.strategy.fills.clone();
    let base_equity = baseline.core.equity_now();

    for (label, venue, gate) in [
        ("no venue key, gate on", None, true),
        ("crypto venue, gate on", Some("binance"), true),
        ("no venue key, gate off", None, false),
    ] {
        let eng = run(venue, gate, 1);
        let fills = &eng.strategy.fills;
        assert_eq!(fills.len(), base_fills.len(), "{label}");
        for (a, b) in fills.iter().zip(&base_fills) {
            assert_eq!((a.price, a.ts, a.size), (b.price, b.ts, b.size), "{label}");
        }
        assert_eq!(eng.core.equity_now(), base_equity, "{label}: equity must be identical");
        assert_eq!(eng.core.session_deferrals, 0, "{label}: an inert gate never defers");
    }
}

// ---- 2. the gate refuses fills outside the session ----

/// The law: a market order submitted just before the FX close does not fill into the weekend. It
/// is NOT canceled and NOT lost — it rests across bars 2/3/4 and fills at bar 5, the Sunday 21:00
/// reopen, at that bar's price.
#[test]
fn market_order_defers_across_the_weekend_and_fills_at_the_reopen() {
    let eng = run(Some("dukascopy"), true, 1);
    let fills = &eng.strategy.fills;
    assert_eq!(fills.len(), 1, "the order is deferred, never dropped: {fills:?}");
    assert_eq!(fills[0].price, 150.0, "bar 5 open — Sunday 21:00 UTC, the reopen");
    assert_eq!(fills[0].ts, ms(2024, 1, 7, 21));
    // one deferral per closed bar it rested through (bars 2, 3, 4)
    assert_eq!(eng.core.session_deferrals, 3);
    assert_eq!(eng.core.position_of(SYM).size, 1.0);
}

/// The counter is mirrored onto the result so the harness/bin entry points — which drop the
/// engine — can still tell "the venue was shut" from "the strategy never traded".
#[test]
fn session_deferrals_reaches_the_backtest_result() {
    let mut eng = StrategyEngine::new(
        vec![(SYM.into(), weekend_tape())],
        TradeOnce { at: 1, side: 1, stop: None, fills: Vec::new() },
        params(Some("dukascopy"), true),
    );
    let res = eng.run();
    assert_eq!(res.session_deferrals, 3);
    assert_eq!(res.stale_deferrals, 0, "the staleness discipline is a separate, unset knob");

    let mut off = StrategyEngine::new(
        vec![(SYM.into(), weekend_tape())],
        TradeOnce { at: 1, side: 1, stop: None, fills: Vec::new() },
        params(Some("dukascopy"), false),
    );
    assert_eq!(off.run().session_deferrals, 0, "0 whenever the gate is off");
}

/// An order submitted while the venue is ALREADY shut still rests and fills at the reopen — the
/// gate refuses fills, it does not refuse order intake (which would be a live-side rejection law,
/// deliberately not built here).
#[test]
fn order_submitted_while_closed_still_rests_and_fills_at_the_reopen() {
    let eng = run(Some("dukascopy"), true, 3); // submitted on Saturday
    let fills = &eng.strategy.fills;
    assert_eq!(fills.len(), 1, "{fills:?}");
    assert_eq!(fills[0].price, 150.0, "the Sunday reopen, not the next (closed) bar");
    // A market order submitted during bar 3 is first offered a fill on bar 4 (Sun 20:00, still
    // closed → one deferral) and takes bar 5's open.
    assert_eq!(eng.core.session_deferrals, 1);
}

/// An armed protective stop does NOT fire over the weekend. The staleness discipline deliberately
/// exempts protective stops (a stale price must not strand live risk); a CLOSED venue is a
/// different fact — it physically cannot fill one. The stop stays armed and fires at the reopen,
/// at that bar's price, through the same adverse-gap arm the trigger law already uses.
///
/// The tape rises monotonically, so the breach case is a SHORT: enter on bar 0 → fills bar 1's
/// open @110 with the stop at 125 (`high >= stop` closes a short). Bar 2's high is 121 — no
/// breach; bar 3 (Sat 12:00) highs 131, THROUGH the stop, but the venue is shut.
#[test]
fn protective_stop_does_not_fire_over_the_weekend() {
    let eng = run_side(Some("dukascopy"), true, 0, -1, Some(125.0));
    let fills = &eng.strategy.fills;
    assert_eq!(fills.len(), 2, "entry + stop close: {fills:?}");
    assert_eq!(fills[0].price, 110.0, "entry at bar 1 open");
    assert_ne!(fills[1].ts, ms(2024, 1, 6, 12), "a shut venue cannot fill a protective stop");
    // It fires on the first in-session bar instead — bar 5, the Sunday 21:00 reopen — and that
    // bar OPENS at 150, already through the 125 stop, so the fill is the adverse gapped open.
    assert_eq!(fills[1].ts, ms(2024, 1, 7, 21), "it fires at the reopen");
    assert_eq!(fills[1].price, 150.0, "into the gap, not back at the stop level");
    assert_eq!(eng.core.position_of(SYM).size, 0.0, "the stop closed the position");
    // bars 2/3/4 each refused the armed stop a pass
    assert_eq!(eng.core.session_deferrals, 3);
}

/// The SAME entry and stop with the gate OFF: it fires inside the weekend, at a price no venue
/// was open to print. This is the divergence the gate exists to remove, pinned so the behavior
/// change is visible rather than implicit.
#[test]
fn ungated_protective_stop_fires_inside_the_weekend() {
    let eng = run_side(Some("dukascopy"), false, 0, -1, Some(125.0));
    let fills = &eng.strategy.fills;
    assert_eq!(fills.len(), 2, "{fills:?}");
    assert_eq!(fills[1].ts, ms(2024, 1, 6, 12), "ungated: the stop fires on the Saturday bar");
    assert_eq!(fills[1].price, 130.0, "at the gapped Saturday open");
    assert_eq!(eng.core.session_deferrals, 0);
}

// ---- 3. keyed by (venue, asset-class), resolved per SYMBOL ----

/// The venue-only default still classes single-session venues: crypto (binance) never gates, FX
/// (dukascopy) gates the weekend — the SAME tape and strategy, different `default_venue`.
#[test]
fn venue_only_default_classes_single_session_venues() {
    // crypto: never closed ⇒ fills at bar 2, exactly like the ungated run
    let crypto = run(Some("binance"), true, 1);
    assert_eq!(crypto.strategy.fills[0].price, 120.0);
    assert_eq!(crypto.core.session_deferrals, 0);

    // FX: the weekend defers to the Sunday reopen
    let fx = run(Some("dukascopy"), true, 1);
    assert_eq!(fx.strategy.fills[0].price, 150.0);
    assert!(fx.core.session_deferrals > 0);
}

/// THE redo fix, end-to-end: ONE run, ONE venue (alpaca), TWO asset classes, each symbol gated by
/// its OWN market — which a single `default_venue` per run cannot express. The per-symbol
/// calendars come from the real `vike_catalog::session_calendar_for(venue, asset_class)` resolver,
/// so the whole (venue, asset-class) → per-symbol chain is exercised, not just the engine half.
///
/// Aligned tape (both symbols), distinct price levels so a fill names its symbol:
///   idx  when (UTC)            equity (13:30–21:00 wkdays)   crypto (24/7)
///    0   Fri 2024-01-05 15:00  OPEN                          OPEN
///    1   Sat 2024-01-06 15:00  CLOSED                        OPEN
///    2   Mon 2024-01-08 15:00  OPEN                          OPEN
#[test]
fn alpaca_crypto_and_equity_gate_independently_in_one_run() {
    // resolved through the real (venue, asset-class) keying site
    let crypto_cal = session_calendar_for("alpaca", AssetClass::CryptoSpot);
    let equity_cal = session_calendar_for("alpaca", AssetClass::Equity);
    assert_eq!(crypto_cal, CRYPTO_24_7, "resolver: alpaca crypto is 24/7");
    assert_eq!(equity_cal, US_EQUITY_REGULAR, "resolver: alpaca equity is the US session");

    let crypto_tape = vec![
        bar(ms(2024, 1, 5, 15), 100.0, 101.0, 99.0),
        bar(ms(2024, 1, 6, 15), 110.0, 111.0, 109.0),
        bar(ms(2024, 1, 8, 15), 120.0, 121.0, 119.0),
    ];
    let equity_tape = vec![
        bar(ms(2024, 1, 5, 15), 200.0, 201.0, 199.0),
        bar(ms(2024, 1, 6, 15), 210.0, 211.0, 209.0),
        bar(ms(2024, 1, 8, 15), 220.0, 221.0, 219.0),
    ];

    /// Buys 1.0 market of each named symbol exactly once, at step 0.
    struct BuyBothAtZero {
        syms: Vec<String>,
        submitted: bool,
        fills: Vec<Fill>,
    }
    impl Strategy<vike_backtest::SimBroker> for BuyBothAtZero {
        fn on_bar(&mut self, ctx: &mut vike_backtest::SimBroker, _bar: &Bar) {
            if ctx.index == 0 && !self.submitted {
                for s in &self.syms {
                    ctx.submit(s, 1, 1.0, 0.0, false, None);
                }
                self.submitted = true;
            }
        }
        fn on_fill(&mut self, _ctx: &mut vike_backtest::SimBroker, fill: &Fill) {
            self.fills.push(fill.clone());
        }
    }

    let mut eng = StrategyEngine::new(
        vec![("BTCUSD".into(), crypto_tape), ("AAPL".into(), equity_tape)],
        BuyBothAtZero {
            syms: vec!["BTCUSD".into(), "AAPL".into()],
            submitted: false,
            fills: Vec::new(),
        },
        params_calendars(&[("BTCUSD", crypto_cal), ("AAPL", equity_cal)]),
    );
    eng.run();

    let by = |sym: &str| -> Vec<&Fill> {
        eng.strategy.fills.iter().filter(|f| f.symbol == sym).collect()
    };
    // crypto: submitted bar 0, fills at bar 1's open (Saturday is open for crypto) @110
    let btc = by("BTCUSD");
    assert_eq!(btc.len(), 1, "crypto fills once: {btc:?}");
    assert_eq!(btc[0].price, 110.0, "crypto fills on the Saturday bar — its market never closes");
    assert_eq!(btc[0].ts, ms(2024, 1, 6, 15));
    // equity: submitted bar 0, Saturday (bar 1) is CLOSED → defers, fills at Monday (bar 2) @220
    let aapl = by("AAPL");
    assert_eq!(aapl.len(), 1, "equity fills once: {aapl:?}");
    assert_eq!(aapl[0].price, 220.0, "equity skips the closed Saturday and fills Monday");
    assert_eq!(aapl[0].ts, ms(2024, 1, 8, 15));
    // exactly one deferral — the equity's Saturday pass; the crypto never deferred
    assert_eq!(eng.core.session_deferrals, 1, "only the equity leg deferred, and only once");
    assert_eq!(eng.core.position_of("BTCUSD").size, 1.0);
    assert_eq!(eng.core.position_of("AAPL").size, 1.0);
}

/// A per-symbol override BEATS the `default_venue` calendar: same tape, `default_venue = binance`
/// (24/7) would never gate, but an override pins the FX week onto the symbol and it defers exactly
/// as the FX run does. Proves the override hook is the authority, not the venue.
#[test]
fn per_symbol_override_beats_the_venue_default() {
    let mut eng = StrategyEngine::new(
        vec![(SYM.into(), weekend_tape())],
        TradeOnce { at: 1, side: 1, stop: None, fills: Vec::new() },
        EngineParams {
            cash: 1_000_000.0,
            default_venue: Some("binance".into()), // 24/7 by venue…
            session_gate: true,
            // …but the per-symbol override says this symbol runs the FX week
            session_calendars: {
                let mut m = IndexMap::new();
                m.insert(SYM.to_string(), vike_model::session::FX_WEEK);
                m
            },
            ..EngineParams::default()
        },
    );
    eng.run();
    assert_eq!(eng.strategy.fills[0].price, 150.0, "the override gated the weekend, not binance");
    assert_eq!(eng.core.session_deferrals, 3);
}

/// The read-only strategy surface (`SimBroker::symbol_is_open`) agrees with the gate that is
/// actually refusing the fills, and is always `true` when the gate is off — so a strategy reading
/// it is never misled into thinking a default run is session-aware.
#[test]
fn symbol_is_open_read_surface_tracks_the_gate() {
    struct Probe {
        seen: Vec<(usize, bool)>,
    }
    impl Strategy<vike_backtest::SimBroker> for Probe {
        fn on_bar(&mut self, ctx: &mut vike_backtest::SimBroker, _bar: &Bar) {
            self.seen.push((ctx.index, ctx.symbol_is_open(SYM)));
        }
    }

    let mut on = StrategyEngine::new(
        vec![(SYM.into(), weekend_tape())],
        Probe { seen: Vec::new() },
        params(Some("dukascopy"), true),
    );
    on.run();
    assert_eq!(
        on.strategy.seen,
        vec![(0, true), (1, true), (2, false), (3, false), (4, false), (5, true), (6, true)],
        "the read surface must mirror the FX week exactly"
    );

    let mut off = StrategyEngine::new(
        vec![(SYM.into(), weekend_tape())],
        Probe { seen: Vec::new() },
        params(Some("dukascopy"), false),
    );
    off.run();
    assert!(off.strategy.seen.iter().all(|(_, open)| *open), "gate off ⇒ always reports open");
}

// ---- 4. the gate covers the opt-in queue_model TICK lanes, not just the frozen fill path ----
//
// The frozen bar/tick fill lanes gate through `defer_fill`; the two `queue_model` tick twins —
// the queued resting LIMIT branch and the whole tagged-maker `fill_tagged_queued` lane — did NOT,
// so a resting limit and a maker quote could fill outside their symbol's session whenever
// `queue_model` and `session_gate` were BOTH on. These gates pin that both queued lanes now honor
// the same session law as their frozen twins, and that the fix is inert when the gate is off.

const SYM_EQ: &str = "AAPL";

/// A US-equity book snapshot: 10 resting on the 99 bid (the queued front), 5 on the 101 ask.
fn eq_book(ts: i64) -> Tick {
    Tick::Book(BookUpdate {
        ts,
        local_ts: 0,
        seq: 1,
        kind: BookUpdateKind::Snapshot,
        tick_size: 0.01,
        bids: vec![(99.0, 10.0)],
        asks: vec![(101.0, 5.0)],
        symbol: SYM_EQ.to_string(),
    })
}

/// A trade that prints at 98.5 — strictly THROUGH a 99 bid. A strict cross fills the queue in
/// full regardless of the front (see `queue_model`'s composition rule), so the ONLY thing that can
/// hold this fill back is the session gate. That isolation is deliberate: it proves the session
/// gate, not the queue gate, is what defers.
fn eq_trade_through(ts: i64) -> Tick {
    Tick::Trade(TradeTick {
        ts,
        local_ts: 0,
        price: 98.5,
        size: 5.0,
        is_buyer_maker: false,
        symbol: SYM_EQ.to_string(),
    })
}

/// Rests ONE untagged buy limit AND one tagged maker bid at the same price on the first book, then
/// records every fill with its maker flag so the two lanes can be told apart.
struct RestLimitAndTag {
    price: f64,
    size: f64,
    submitted: bool,
    fills: Vec<(i64, f64, f64, bool)>, // (ts, size, price, is_maker)
}

impl Strategy<vike_backtest::SimBroker> for RestLimitAndTag {
    fn on_order_book(&mut self, b: &mut vike_backtest::SimBroker, _book: &L2Book) {
        if !self.submitted {
            self.submitted = true;
            Broker::submit_limit(b, SYM_EQ, 1, self.size, self.price);
            b.submit_limit_tagged("bid", 1, self.size, self.price);
        }
    }
    fn on_fill(&mut self, _b: &mut vike_backtest::SimBroker, f: &Fill) {
        self.fills.push((f.ts, f.size, f.price, f.is_maker));
    }
}

/// The queued tick run behind both gates below: `queue_model` on, `session_gate` = `gate`, the
/// per-symbol US-equity calendar resolved through the real `(venue, asset-class)` site. The tape
/// submits inside Friday's session, offers a fill on the CLOSED Saturday, then on the reopened
/// Monday — all via `run_ticks`, the only path that reaches the queued lanes.
fn run_queued_session(gate: bool) -> StrategyEngine<RestLimitAndTag> {
    let cal = session_calendar_for("alpaca", AssetClass::Equity);
    let mut session_calendars = IndexMap::new();
    session_calendars.insert(SYM_EQ.to_string(), cal); // ignored when the gate is off
    let params = EngineParams {
        cash: 1_000_000.0,
        session_gate: gate,
        session_calendars,
        queue_model: Some(QueueModelKind::RiskAdverse),
        ..EngineParams::default()
    };
    let strat = RestLimitAndTag { price: 99.0, size: 1.0, submitted: false, fills: Vec::new() };
    let mut eng = StrategyEngine::new(vec![(SYM_EQ.to_string(), Vec::new())], strat, params);
    let ticks = vec![
        eq_book(ms(2024, 1, 5, 15)), // Fri 15:00 — OPEN: submit the limit + the tag
        eq_trade_through(ms(2024, 1, 6, 15)), // Sat 15:00 — CLOSED: a cross that must NOT fill
        eq_trade_through(ms(2024, 1, 8, 15)), // Mon 15:00 — OPEN: the same cross fills both
    ];
    eng.run_ticks(&[(SYM_EQ.to_string(), ticks)]);
    eng
}

/// THE gate that was missing: with `queue_model` AND `session_gate` both on, a queued resting
/// limit and a tagged maker quote each refuse the Saturday cross and fill only on the Monday
/// reopen. Before the fix the queued lanes ignored the session gate and both filled on Saturday —
/// a market shut for the weekend. Both fill as MAKERS (an untagged resting limit books maker by
/// order kind, same as the tag). That the limit's `pending` AND the tag map both empty at the end
/// proves each lane filled its own order; the two deferrals on the closed bar prove each lane —
/// the queued-limit branch and `fill_tagged_queued` — refused the Saturday cross independently.
#[test]
fn queued_limit_and_tag_defer_outside_session_and_fill_at_the_reopen() {
    let eng = run_queued_session(true);
    let fills = &eng.strategy.fills;
    let reopen = (ms(2024, 1, 8, 15), 1.0, 98.5, true);
    assert_eq!(fills.len(), 2, "both lanes fill exactly once, at the reopen: {fills:?}");
    assert!(
        fills.iter().all(|&f| f == reopen),
        "every fill is the Monday reopen cross, as a maker — nothing on the closed Saturday: {fills:?}"
    );
    assert_eq!(eng.core.position_of(SYM_EQ).size, 2.0, "both buys landed");
    assert!(eng.core.sym[0].pending.is_empty(), "the queued limit filled (its lane was gated)");
    assert!(eng.core.sym[0].tagged.is_empty(), "the queued tag filled (its lane was gated)");
    // the Saturday cross deferred the limit lane AND the tagged lane, once each
    assert_eq!(eng.core.session_deferrals, 2);
}

/// The gate-OFF control on the SAME tape and stream — only `session_gate` flips. Both queued lanes
/// fill on the Saturday cross, a bar no venue was open to print. This is the divergence the fix
/// removes, and it doubles as the in-test mutation contrast: the fill moves from Saturday (gate
/// off) to Monday (gate on) purely because of the gate.
#[test]
fn queued_lanes_gate_off_fill_straight_through_the_closed_saturday() {
    let eng = run_queued_session(false);
    let fills = &eng.strategy.fills;
    let saturday = (ms(2024, 1, 6, 15), 1.0, 98.5, true);
    assert_eq!(fills.len(), 2, "{fills:?}");
    assert!(
        fills.iter().all(|&f| f == saturday),
        "gate off: both queued lanes fill Saturday: {fills:?}"
    );
    assert_eq!(eng.core.position_of(SYM_EQ).size, 2.0);
    assert_eq!(eng.core.session_deferrals, 0, "gate off ⇒ never counted");
}
