//! options lane gate: the opt-in `EngineParams::option_specs` option-expiry cash settlement.
//! At/after an option contract's `expiry_ts` a held position is closed to CASH at its intrinsic
//! value — CALL `max(0, underlying − strike)`, PUT `max(0, strike − underlying)` per contract — by
//! the SAME fee-free settlement fill the binary-resolution lane uses (recorded in
//! `SimBroker::settlements`); `option_specs: None` — and a configured source whose option never
//! expires within the run — leaves the run byte-identical. The underlying settle price is the
//! engine's own mark for the option's `underlying` symbol, which is why every case below holds the
//! underlying FLAT (so the one-bar top-of-step mark lag is immaterial to the asserted intrinsic).
//! Mirrors the run-construction pattern in `resolution_settlement.rs`.

use vike_backtest::{
    EngineParams, OptionExpirySource, OptionRight, OptionSpec, SimBroker, StrategyEngine, Tick,
};
use vike_model::{Bar, Fill, QuoteTick, Strategy};

const OPT: &str = "OPT"; // the held option instrument (its bars carry the premium)
const UNDER: &str = "UNDER"; // the underlying (its bars carry the settle price — held flat below)
const T0: i64 = 1_700_000_000_000;
const STEP_MS: i64 = 60_000;

/// Flat-OHLC bars for one symbol at `closes[i]` (open == high == low == close), so a market order
/// fills at an unambiguous price and the underlying mark is unambiguous at every ts.
fn mk_bars(sym: &str, closes: &[f64]) -> Vec<Bar> {
    closes
        .iter()
        .enumerate()
        .map(|(i, &c)| Bar {
            ts: T0 + i as i64 * STEP_MS,
            open: c,
            high: c,
            low: c,
            close: c,
            volume: 0.0,
            funding: None,
            bid: None,
            ask: None,
            symbol: Some(sym.to_string()),
        })
        .collect()
}

/// Buys `size` OPT once (raw market, fills at OPT's NEXT bar open) and holds. The `submitted` latch
/// is REQUIRED: `on_bar` fires once per SYMBOL per step (two symbols here), so an unguarded submit
/// would double the position.
struct BuyAndHold {
    size: f64,
    submitted: bool,
    fills_seen: Vec<Fill>,
}

impl Strategy<SimBroker> for BuyAndHold {
    fn on_bar(&mut self, ctx: &mut SimBroker, _bar: &Bar) {
        if !self.submitted {
            self.submitted = true;
            ctx.submit(OPT, 1, self.size, 0.0, true, None);
        }
    }
    fn on_fill(&mut self, _ctx: &mut SimBroker, fill: &Fill) {
        self.fills_seen.push(fill.clone());
    }
}

fn buy_and_hold(size: f64) -> BuyAndHold {
    BuyAndHold { size, submitted: false, fills_seen: Vec::new() }
}

/// A single-option source: `OPT` is the option `spec`, everything else (the underlying) is `None`.
fn option_source(spec: OptionSpec) -> OptionExpirySource {
    Box::new(move |sym: &str| if sym == OPT { Some(spec.clone()) } else { None })
}

fn call(strike: f64, expiry_ts: i64) -> OptionSpec {
    OptionSpec { strike, expiry_ts, right: OptionRight::Call, underlying: UNDER.to_string() }
}

fn put(strike: f64, expiry_ts: i64) -> OptionSpec {
    OptionSpec { strike, expiry_ts, right: OptionRight::Put, underlying: UNDER.to_string() }
}

fn engine(
    opt_closes: &[f64],
    under_closes: &[f64],
    strat: BuyAndHold,
    params: EngineParams,
) -> StrategyEngine<BuyAndHold> {
    StrategyEngine::new(
        vec![
            (OPT.to_string(), mk_bars(OPT, opt_closes)),
            (UNDER.to_string(), mk_bars(UNDER, under_closes)),
        ],
        strat,
        params,
    )
}

/// An ITM call settles to intrinsic and closes: underlying 120, strike 100 ⇒ intrinsic 20. The
/// buy (10 @ premium 5) settles full-qty at 20 by a fee-free fill; PnL/cash are exact.
#[test]
fn itm_call_settles_to_intrinsic_and_closes() {
    // idx0: submit (market fills at OPT idx1 open = 5.0). Expiry at idx3's ts.
    let expiry = T0 + 3 * STEP_MS;
    let params = EngineParams {
        cash: 1_000.0,
        option_specs: Some(option_source(call(100.0, expiry))),
        ..Default::default()
    };
    let mut e =
        engine(&[5.0, 5.0, 5.0, 5.0], &[120.0, 120.0, 120.0, 120.0], buy_and_hold(10.0), params);
    let r = e.run();

    // the option position settled to cash and closed
    assert_eq!(e.core.position_of(OPT).size, 0.0, "expired option must be flat");
    assert!(e.core.pending_of(OPT).is_empty(), "no resting orders survive expiry");

    // ONE distinct settlement record: full qty at the intrinsic payout 20, closing a long
    assert_eq!(e.core.settlements.len(), 1);
    let s = &e.core.settlements[0];
    assert_eq!((s.symbol.as_str(), s.payout, s.qty, s.side, s.ts), (OPT, 20.0, 10.0, -1, expiry));

    // ONE closed trade: entry 5.0, exit at the 20.0 intrinsic, fee-free settlement
    assert_eq!(r.trades.len(), 1);
    let t = &r.trades[0];
    assert_eq!(t.entry_price, 5.0);
    assert_eq!(t.exit_price, 20.0);
    assert_eq!(t.size, 10.0);
    assert_eq!(t.fees, 0.0, "settlement adds NO fee (fee-free run)");
    assert_eq!(t.pnl, (20.0 - 5.0) * 10.0); // gross price pnl, mult 1
    assert!(t.is_long);

    // cash: 1000 − buy notional (10·5) + settlement proceeds (10·20)
    assert_eq!(e.core.cash, 1_000.0 - 10.0 * 5.0 + 10.0 * 20.0);
    assert_eq!(r.final_equity, e.core.cash, "flat book: equity == cash");

    // the strategy observed the settlement through on_fill: a fee-free full close at intrinsic
    let last = e.strategy.fills_seen.last().expect("settlement delivered via on_fill");
    assert_eq!(
        (last.side, last.size, last.price, last.fee, last.is_maker),
        (-1, 10.0, 20.0, 0.0, false)
    );
}

/// An OTM put settles to 0 and closes: underlying 120, strike 100 ⇒ `max(0, 100−120) = 0`. The
/// whole premium is lost, cash gets no proceeds.
#[test]
fn otm_put_settles_to_zero_and_closes() {
    let expiry = T0 + 3 * STEP_MS;
    let params = EngineParams {
        cash: 1_000.0,
        option_specs: Some(option_source(put(100.0, expiry))),
        ..Default::default()
    };
    let mut e =
        engine(&[3.0, 3.0, 3.0, 3.0], &[120.0, 120.0, 120.0, 120.0], buy_and_hold(10.0), params);
    let r = e.run();

    assert_eq!(e.core.position_of(OPT).size, 0.0);
    assert_eq!(e.core.settlements.len(), 1);
    assert_eq!(e.core.settlements[0].payout, 0.0);
    assert_eq!(r.trades.len(), 1);
    assert_eq!(r.trades[0].exit_price, 0.0);
    assert_eq!(r.trades[0].pnl, (0.0 - 3.0) * 10.0); // pure (gross) stake loss
    assert_eq!(e.core.cash, 1_000.0 - 10.0 * 3.0); // no proceeds from a worthless option
}

/// Settlement fires only at/AFTER `expiry_ts` — never before. (a) an expiry AT a bar's ts settles
/// on THAT bar (the `>=` boundary is inclusive); (b) an expiry one ms LATER skips that bar and
/// settles on the next; (c) an expiry beyond the tape (with no end-of-run as-of) never settles.
#[test]
fn settlement_only_fires_at_or_after_expiry() {
    let opt = [5.0, 5.0, 5.0, 5.0, 5.0];
    let under = [120.0, 120.0, 120.0, 120.0, 120.0];
    let bar3 = T0 + 3 * STEP_MS;
    let bar4 = T0 + 4 * STEP_MS;

    // (a) expiry exactly at bar 3 ⇒ settles on bar 3 (inclusive)
    let pa = EngineParams {
        cash: 1_000.0,
        option_specs: Some(option_source(call(100.0, bar3))),
        ..Default::default()
    };
    let mut ea = engine(&opt, &under, buy_and_hold(10.0), pa);
    ea.run();
    assert_eq!(ea.core.settlements.len(), 1);
    assert_eq!(ea.core.settlements[0].ts, bar3, "settled on the expiry bar, not before");
    assert_eq!(ea.core.position_of(OPT).size, 0.0);

    // (b) expiry one ms after bar 3 ⇒ bar 3 is BEFORE expiry (no settle), bar 4 is at/after
    let pb = EngineParams {
        cash: 1_000.0,
        option_specs: Some(option_source(call(100.0, bar3 + 1))),
        ..Default::default()
    };
    let mut eb = engine(&opt, &under, buy_and_hold(10.0), pb);
    eb.run();
    assert_eq!(eb.core.settlements.len(), 1);
    assert_eq!(
        eb.core.settlements[0].ts, bar4,
        "the strict boundary: bar 3 < expiry, bar 4 settles"
    );

    // (c) expiry far beyond the tape, no end-of-run as-of ⇒ never settles, position held
    let pc = EngineParams {
        cash: 1_000.0,
        option_specs: Some(option_source(call(100.0, T0 + 1_000 * STEP_MS))),
        ..Default::default()
    };
    let mut ec = engine(&opt, &under, buy_and_hold(10.0), pc);
    let rc = ec.run();
    assert!(ec.core.settlements.is_empty(), "an unexpired option is never settled");
    assert_eq!(ec.core.position_of(OPT).size, 10.0, "the position is held unchanged past run end");
    assert!(!ec.core.is_resolved(OPT));
    assert!(rc.trades.is_empty());
}

/// OFF / byte-identical: a configured `option_specs` source whose option NEVER expires within the
/// run (expiry far past the tape, no end-of-run as-of) is bit-for-bit identical to `None` — same
/// equity curve, trades, cash, and (empty) settlements, position held unchanged. The `resolved`
/// latch being ALLOCATED (vs empty) when a source is present must not perturb any f64.
#[test]
fn option_source_that_never_expires_is_byte_identical_to_none() {
    let opt = [5.0, 5.0, 5.0, 5.0, 5.0];
    let under = [120.0, 120.0, 120.0, 120.0, 120.0];
    let mk = |option_specs: Option<OptionExpirySource>| {
        let params = EngineParams { cash: 1_000.0, option_specs, ..Default::default() };
        engine(&opt, &under, buy_and_hold(10.0), params)
    };
    let mut base = mk(None);
    let rb = base.run();
    let mut cfg = mk(Some(option_source(call(100.0, T0 + 1_000 * STEP_MS))));
    let rc = cfg.run();

    assert_eq!(
        rb.equity_curve.iter().map(|v| v.to_bits()).collect::<Vec<_>>(),
        rc.equity_curve.iter().map(|v| v.to_bits()).collect::<Vec<_>>(),
        "equity curve must be bit-identical to the source-less run"
    );
    assert_eq!(rb.n_trades, rc.n_trades);
    assert_eq!(base.core.cash.to_bits(), cfg.core.cash.to_bits());
    assert!(cfg.core.settlements.is_empty());
    assert_eq!(base.core.position_of(OPT).size, cfg.core.position_of(OPT).size);
    assert_eq!(cfg.core.position_of(OPT).size, 10.0); // both runs hold the option, unsettled
}

/// The END-OF-RUN sweep: a tape that stops BEFORE `expiry_ts` still settles when the opt-in
/// `option_expiry_end_ts` as-of is at/after the expiry — the "hold to expiry over a short window"
/// case, the analog of the binary lane's `resolution_end_ts`. Marked at the last known underlying.
#[test]
fn option_expiry_end_ts_force_settles_option_expiring_after_tape() {
    let last_bar_ts = T0 + 4 * STEP_MS;
    let expiry = last_bar_ts + 86_400_000; // a full day after the tape stops
    let params = EngineParams {
        cash: 1_000.0,
        option_specs: Some(option_source(call(100.0, expiry))),
        option_expiry_end_ts: Some(expiry), // as-of AT expiry ⇒ the sweep settles it
        ..Default::default()
    };
    let mut e = engine(
        &[5.0, 5.0, 5.0, 5.0, 5.0],
        &[120.0, 120.0, 120.0, 120.0, 120.0],
        buy_and_hold(10.0),
        params,
    );
    let r = e.run();

    // no in-loop event ts reached expiry; the end-of-run sweep settled at the as-of
    assert_eq!(e.core.settlements.len(), 1, "the end-of-run sweep settled the held option");
    let s = &e.core.settlements[0];
    assert_eq!((s.payout, s.qty, s.side, s.ts), (20.0, 10.0, -1, expiry));
    assert_eq!(e.core.position_of(OPT).size, 0.0);
    assert_eq!(r.trades.len(), 1);
    assert_eq!(r.trades[0].exit_price, 20.0);
    assert_eq!(e.core.cash, 1_000.0 - 10.0 * 5.0 + 10.0 * 20.0);
    // the curve's final sample is re-pointed at the post-settlement equity — same sample COUNT
    assert_eq!(r.equity_curve.len(), 5);
    assert_eq!(*r.equity_curve.last().unwrap(), r.final_equity);
    assert_eq!(r.final_equity, e.core.cash, "flat book: equity == cash");
}

/// The expiry latch (shared with the binary lane): an `on_fill` re-entry on the settlement fill
/// must NOT re-open an expired option. The settlement clears the books and latches the symbol
/// resolved BEFORE firing `on_fill`, so orders submitted from inside it are refused on both lanes.
struct ReenterOnSettlement {
    intrinsic: f64,
    submitted: bool,
    reentry_attempts: usize,
}

impl Strategy<SimBroker> for ReenterOnSettlement {
    fn on_bar(&mut self, ctx: &mut SimBroker, _bar: &Bar) {
        if !self.submitted {
            self.submitted = true;
            ctx.submit(OPT, 1, 10.0, 0.0, true, None);
        }
    }
    fn on_fill(&mut self, ctx: &mut SimBroker, fill: &Fill) {
        // the settlement fill: fee-free, at the intrinsic payout (distinct from the 5.0 entry)
        if fill.fee == 0.0 && fill.price == self.intrinsic && self.reentry_attempts == 0 {
            self.reentry_attempts += 1;
            ctx.submit(OPT, 1, 25.0, 0.0, true, None);
            SimBroker::submit_limit(ctx, OPT, 1, 25.0, 0.5, 0.0, true, None);
        }
    }
}

#[test]
fn expired_option_refuses_reentry_from_on_fill() {
    // expiry at idx3; bars continue to idx5, so a re-opened position WOULD keep trading
    let expiry = T0 + 3 * STEP_MS;
    let params = EngineParams {
        cash: 1_000.0,
        option_specs: Some(option_source(call(100.0, expiry))),
        ..Default::default()
    };
    let strat = ReenterOnSettlement { intrinsic: 20.0, submitted: false, reentry_attempts: 0 };
    let mut e = StrategyEngine::new(
        vec![
            (OPT.to_string(), mk_bars(OPT, &[5.0, 5.0, 5.0, 5.0, 5.0, 5.0])),
            (UNDER.to_string(), mk_bars(UNDER, &[120.0, 120.0, 120.0, 120.0, 120.0, 120.0])),
        ],
        strat,
        params,
    );
    let r = e.run();

    // the re-entry path really was taken — otherwise this proves nothing
    assert_eq!(e.strategy.reentry_attempts, 1, "the re-entry must actually have fired");
    // ...and every re-entry order was refused: still flat, nothing resting, ONE settlement
    assert!(e.core.is_resolved(OPT), "the option is latched resolved at expiry");
    assert_eq!(e.core.position_of(OPT).size, 0.0, "an expired option cannot be re-opened");
    assert!(e.core.pending_of(OPT).is_empty(), "refused orders never rest");
    assert_eq!(e.core.settlements.len(), 1, "settled exactly once — no re-open, no re-settle");
    assert_eq!(r.trades.len(), 1);
    assert_eq!((r.trades[0].entry_price, r.trades[0].exit_price), (5.0, 20.0));
    assert_eq!(e.core.cash, 1_000.0 - 10.0 * 5.0 + 10.0 * 20.0);
}

/// `contract_size` is the option symbol's own engine multiplier (`EngineParams::multipliers`),
/// applied by the shared settlement fold exactly as on entry — so the payoff
/// `max(0, u − K)·contract_size·qty` needs no second knob. Here contract_size 100: buy 1 @ 5,
/// settle 1 @ intrinsic 20, both scaled by 100.
#[test]
fn contract_size_via_multiplier_scales_settlement() {
    let expiry = T0 + 3 * STEP_MS;
    let params = EngineParams {
        cash: 10_000.0,
        multipliers: vec![(OPT.to_string(), 100.0)], // the option's contract size
        option_specs: Some(option_source(call(100.0, expiry))),
        ..Default::default()
    };
    let mut e =
        engine(&[5.0, 5.0, 5.0, 5.0], &[120.0, 120.0, 120.0, 120.0], buy_and_hold(1.0), params);
    let r = e.run();

    assert_eq!(e.core.position_of(OPT).size, 0.0);
    assert_eq!(e.core.settlements.len(), 1);
    // the recorded payout is the PER-CONTRACT intrinsic (20); the ×100 contract size is the fold's
    assert_eq!(e.core.settlements[0].payout, 20.0);
    // cash: 10000 − 1·5·100 (buy) + 1·20·100 (settle)
    assert_eq!(e.core.cash, 10_000.0 - 1.0 * 5.0 * 100.0 + 1.0 * 20.0 * 100.0);
    assert_eq!(r.trades[0].pnl, (20.0 - 5.0) * 1.0 * 100.0); // gross pnl includes the multiplier
}

/// The TICK path settles too (the `run_ticks` wiring — twin of the binary lane's tick settlement):
/// an OPT position opened directly is closed at intrinsic on the first OPT tick at-or-after expiry,
/// before that tick's fill phase. The underlying is a second symbol held flat at 120, so the
/// interleaved-tick ordering is immaterial to the intrinsic (20). Asserts the settlement facts
/// (flat, one record at the intrinsic, exit at 20) — not the ask-crossing entry price.
#[test]
fn tick_path_settles_option_at_expiry() {
    let expiry = T0 + 2 * STEP_MS;
    let params = EngineParams {
        cash: 1_000.0,
        option_specs: Some(option_source(call(100.0, expiry))),
        ..Default::default()
    };
    // empty bar series — the tick path drives everything off `run_ticks` (bars unused)
    let mut e = StrategyEngine::new(
        vec![(OPT.to_string(), Vec::new()), (UNDER.to_string(), Vec::new())],
        buy_and_hold(10.0),
        params,
    );
    // integer ±1 spread so the projected mid is EXACTLY px (integer f64s sum exactly) — the
    // settlement reads `sym[UNDER].price` (the projected mid `quote_tick_to_bar` makes), so a
    // fractional spread would carry f64 rounding into the asserted intrinsic. A market buy fills at
    // the ask; the entry price is not asserted here.
    let quote = |sym: &str, ts: i64, px: f64| {
        Tick::Quote(QuoteTick {
            ts,
            local_ts: 0,
            bid: px - 1.0,
            ask: px + 1.0,
            bid_size: 100.0,
            ask_size: 100.0,
            symbol: sym.to_string(),
        })
    };
    // open the OPT position directly (on_bar never fires on the tick path), then feed ticks that
    // cross the expiry ts; the underlying is held flat at 120 so its mark is 120 at every tick
    e.core.submit(OPT, 1, 10.0, 0.0, true, None);
    let opt_ticks =
        vec![quote(OPT, T0, 5.0), quote(OPT, T0 + STEP_MS, 5.0), quote(OPT, expiry, 5.0)];
    let under_ticks = vec![
        quote(UNDER, T0, 120.0),
        quote(UNDER, T0 + STEP_MS, 120.0),
        quote(UNDER, expiry, 120.0),
    ];
    let r = e.run_ticks(&[(OPT.to_string(), opt_ticks), (UNDER.to_string(), under_ticks)]);

    assert_eq!(e.core.position_of(OPT).size, 0.0, "tick-path settlement flattens the option");
    assert_eq!(e.core.settlements.len(), 1, "settled exactly once (idempotent after flat)");
    assert_eq!(e.core.settlements[0].ts, expiry);
    assert_eq!(e.core.settlements[0].payout, 20.0);
    assert!(e.core.pending_of(OPT).is_empty());
    assert_eq!(r.trades.len(), 1);
    assert_eq!(r.trades[0].exit_price, 20.0);
}
