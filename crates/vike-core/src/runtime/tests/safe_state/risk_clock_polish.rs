//! The polish of the risk clock found by the third review (`docs/ops/known-limitations-2026-10.md`): a feed that
//! gaps at the age bound is ONE skip episode, the latch values a stale engine at the newest price the board
//! holds, a limit close priced through the market counts as a close in flight, and the stuck-order ladder
//! reaches a further account of a mounted venue.

use super::drawdown_latch::{latch_note, two_leg_core};
use super::mount_budget::clock_budget_core;
use super::timers_price_board::{clock_config, clock_core, margin, mounted_skip_notes};
use super::*;

// ---- (a) the hold-down on the recovered side of a skip episode ---------------------------------------------

/// One clock sweep of the mounted `sim` engine at `now`, its only quote 1 s old (`fresh`) or 61 s old, which is
/// past the 60 s age bound, with a price that breaches nothing.
fn sweep_with_quote(core: &mut CoreThread<RecordingClient>, now: i64, fresh: bool) {
    let ts = if fresh { now - 1_000 } else { now - 61_000 };
    core.engine.price_board.set_quote("sim", "BTCUSDT", 100.0, 100.5, ts);
    core.drive_due_timers(now);
}

/// ⚠ **A feed that gaps for about as long as the age bound is ONE skip episode.** The engine flips between stale
/// and fresh at every other sweep. It used to end the episode at the first fresh sweep, so each gap was a new
/// episode: a WARN and a ring line per gap, enough to crowd fills and rejects out of the 64-line ring. An episode
/// now ends only after the engine has stayed fresh for three clock intervals, and the next outage is then new.
#[test]
fn a_feed_that_gaps_at_the_age_bound_is_one_skip_episode_not_one_per_gap() {
    let mut core = clock_core(clock_config(Some(margin()), None, &[], true));
    set_position(&mut core.engine, "sim", "BTCUSDT", 10.0, 100.0);
    core.arm_boundary_timers(0);

    // sweeps at 30 100, 60 100, ... (the clock re-arms 30 s after the instant it ran)
    let pattern = [false, true, false, true, false, true, true, true, true];
    for (k, fresh) in pattern.into_iter().enumerate() {
        sweep_with_quote(&mut core, 30_100 + 30_000 * k as i64, fresh);
        assert_eq!(
            mounted_skip_notes(&core),
            1,
            "sweep {k}: the gaps are the one episode: {:?}",
            core.recent
        );
    }
    // fresh at 180 100, 210 100, 240 100 and 270 100: the hold-down (90 s) is over, the episode with it
    assert!(core.price_only_skipped.is_empty(), "the episode ended: {:?}", core.price_only_skipped);
    assert!(core.price_only_recovering.is_empty());

    sweep_with_quote(&mut core, 300_100, false);
    assert_eq!(mounted_skip_notes(&core), 2, "a NEW outage is said again: {:?}", core.recent);
}

/// Only the SAYING waits. An engine found fresh again during the hold-down is priced and acted on at once: a
/// breach on its fresh quote is liquidated at that very sweep.
#[test]
fn the_hold_down_delays_only_the_saying_never_the_margin_call() {
    let mut core = clock_core(clock_config(Some(margin()), None, &[], true));
    set_position(&mut core.engine, "sim", "BTCUSDT", 10.0, 100.0);
    core.arm_boundary_timers(0);
    sweep_with_quote(&mut core, 30_100, false);
    assert_eq!(mounted_skip_notes(&core), 1);
    assert!(core.engine.client.submissions.is_empty(), "nothing off a stale quote");

    // fresh again, and the market is at 40: long 10 at 100 on a seed of 100 is past its maintenance
    core.engine.price_board.set_quote("sim", "BTCUSDT", 40.0, 40.5, 59_000);
    core.drive_due_timers(60_100);
    assert_eq!(core.engine.client.submissions.len(), 1, "liquidated at once: {:?}", core.recent);
    assert!(core.price_only_skipped.contains_key(&0), "…while the episode is still held open");
}

// ---- (b) the latch values a stale engine at the newest price the board holds -------------------------------

/// ⚠ **A stale engine is valued at the NEWEST price on its board, not at the highest-priority one.** okx holds a
/// long 10 at 100. Its board carries a venue mark of 100 stamped at t=1 (a reconcile pass wrote it at boot) and a
/// quote of 70 stamped at 60 s; at 200 s both are past the 60 s bound. The permissive view took the mark (first
/// present slot, any age): no loss, the latch stayed Active. The newest price is the quote: a loss of 300 on a
/// capital of 2 000 trips a 10% latch, and the skip line names the age of THAT price (140 s), not of the mark.
#[test]
fn the_latch_values_a_stale_engine_at_its_newest_price_not_its_highest_priority_one() {
    let mut core = two_leg_core(("bybit", "BTCUSDT"), ("okx", "ETHUSDT"), (1_000.0, 1_000.0));
    core.config.max_drawdown = Some(0.1);
    set_position(&mut core.extra_engines[0].1, "okx", "ETHUSDT", 10.0, 100.0);
    core.extra_engines[0].1.price_board.set_mark("okx", "ETHUSDT", 100.0, 1);
    core.extra_engines[0].1.price_board.set_quote("okx", "ETHUSDT", 70.0, 70.5, 60_000);
    core.arm_boundary_timers(0);

    core.drive_due_timers(200_000);
    assert_eq!(core.engine.trading_state, TradingState::Reducing, "{:?}", core.recent);
    assert!(latch_note(&core).contains("own_pnl=-300.00"), "{}", latch_note(&core));
    let skip = core.recent.iter().find(|e| e.contains("SKIP the okx engine")).expect("said");
    assert!(skip.contains("ETHUSDT is 140 s old"), "the age of the number used: {skip}");
}

// ---- (c) a reduce-only LIMIT close priced through the market is a close in flight ---------------------------

/// A live reduce-only LIMIT of `qty` on `side` at `price`, registered straight into engine 0 as `submit_order`
/// leaves it (live, nothing filled).
fn rest_limit_close(
    core: &mut CoreThread<RecordingClient>,
    coid: &str,
    side: i32,
    qty: f64,
    price: f64,
) {
    core.engine.registry.insert(
        coid.into(),
        vike_model::ManagedOrder::new(vike_model::OrderRequest {
            client_order_id: coid.into(),
            venue: "sim".into(),
            symbol: "BTCUSDT".into(),
            side,
            qty,
            order_type: vike_model::OrderType::Limit,
            price: Some(price),
            reduce_only: true,
            ..Default::default()
        }),
    );
}

/// ⚠ **A reduce-only LIMIT priced through the market fills now, so it is cover; one resting far away is not.** The
/// market is 40. A sell at 500 is a take-profit (not counted, as before); a sell at 39 and one at exactly 40 cross
/// the bid and count; on the other side a buy at 41 crosses the ask and counts for a BUY close only. With no
/// price for the symbol no limit counts. The latch's flatten, which nets these, then asks for nothing more.
#[test]
fn a_reduce_only_limit_priced_through_the_market_counts_as_a_close_in_flight() {
    let mut core = clock_budget_core(true, None);
    core.engine.price_board.set_mark("sim", "BTCUSDT", 40.0, 25_000);
    let out =
        |core: &CoreThread<RecordingClient>, side| core.closes_in_flight(0, "BTCUSDT", side, None);

    rest_limit_close(&mut core, "far", -1, 1.0, 500.0);
    assert_eq!(out(&core, -1), 0.0, "a take-profit far from the market is not cover");

    rest_limit_close(&mut core, "through", -1, 0.75, 39.0);
    assert!((out(&core, -1) - 0.75).abs() < 1e-12, "a sell below the bid fills now");
    rest_limit_close(&mut core, "at", -1, 0.25, 40.0);
    assert!((out(&core, -1) - 1.0).abs() < 1e-12, "…and one AT the bid");

    rest_limit_close(&mut core, "buy", 1, 2.0, 41.0);
    assert!((out(&core, 1) - 2.0).abs() < 1e-12, "a buy above the ask closes a short");
    assert!((out(&core, -1) - 1.0).abs() < 1e-12, "…and is no cover for the sell side");

    // the flatten nets what crosses: the long 1 is already covered by the two sells
    assert_eq!(core.flatten_qty(0, "BTCUSDT", -1, 1.0), 0.0, "{:?}", core.recent);

    // no price for the symbol: a limit cannot be placed against the market, so it is not counted
    let mut bare = clock_budget_core(true, None);
    rest_limit_close(&mut bare, "through", -1, 0.75, 39.0);
    bare.engine.price_board = Default::default();
    assert_eq!(bare.closes_in_flight(0, "BTCUSDT", -1, None), 0.0);
}

// ---- (e) the clock keeps its cadence: re-armed from the scheduled instant ------------------------------------

/// A tiny deterministic generator (a 64-bit LCG), so the "measurement" below is the same on every run.
struct Lcg(u64);

impl Lcg {
    /// Uniform in `lo..=hi`.
    fn between(&mut self, lo: i64, hi: i64) -> i64 {
        self.0 =
            self.0.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
        lo + ((self.0 >> 33) as i64) % (hi - lo + 1)
    }
}

/// The instants at which the risk clock FIRED when the drain-loop boundary was reached at `boundaries` (ascending
/// ms), until `cycles` firings. `drive_due_timers` stamps the engines' clock only when a timer is due, so a sentinel
/// written before the call tells a firing from a no-op. The clock is the only timer armed.
fn clock_firings(boundaries: impl Iterator<Item = i64>, cycles: usize) -> Vec<i64> {
    let mut core = core_with(CoreConfig {
        price_only: Some(PriceOnlyConfig {
            venues: std::collections::BTreeSet::new(),
            sweep_interval: std::time::Duration::from_secs(30),
            max_price_age: std::time::Duration::from_secs(60),
            price_source: std::collections::BTreeMap::new(),
        }),
        ..CoreConfig::default()
    });
    core.arm_boundary_timers(0);
    let mut fired = Vec::with_capacity(cycles);
    for t in boundaries {
        core.engine.now_ms = -1;
        core.drive_due_timers(t);
        if core.engine.now_ms == t {
            fired.push(t);
            if fired.len() == cycles {
                break;
            }
        }
    }
    fired
}

/// `(widest departure of a gap from 30 000 ms, the whole run's drift from the 30 000 ms schedule)`.
fn gap_and_drift(fired: &[i64]) -> (i64, i64) {
    let worst = fired.windows(2).map(|w| (w[1] - w[0] - 30_000).abs()).max().expect("two firings");
    let drift = fired[fired.len() - 1] - fired[0] - 30_000 * (fired.len() as i64 - 1);
    (worst, drift)
}

/// ⚠ **THE MEASUREMENT behind "the real gap between two risk sweeps is about the interval".** The REAL core and wheel,
/// driven at the boundaries two kinds of core reach: a BUSY one (a message every 1 to 20 ms, so a boundary every few
/// ms) and an IDLE one reached only by the waker thread, `spawn.rs`'s `waker_tick` (half the 30 s interval, 15 s
/// per wake, plus 0 to 3 ms of sleep overshoot each). 200 firings each, 100 minutes.
///
/// Re-armed from the instant it RAN (the old rule), the busy clock drifted by the wheel's 50 ms rounding (about 25 ms
/// per cycle, 72 s a day) and the idle clock ran EVERY 45 s: the wheel rounds a deadline up to its tick, a boundary
/// on the exact millisecond finds the timer a fraction of a tick short, and the next wake is half an interval later.
/// Re-armed from the SCHEDULED instant, every gap is the interval to within the boundary's own lateness and the
/// run does not drift, busy or idle.
#[test]
fn the_risk_clock_keeps_its_cadence_on_a_busy_and_on_an_idle_core() {
    let mut rng = Lcg(7);
    let mut now = 0;
    let busy = std::iter::from_fn(move || {
        now += rng.between(1, 20);
        Some(now)
    });
    let (busy_worst, busy_drift) = gap_and_drift(&clock_firings(busy, 200));

    let mut rng = Lcg(11);
    let mut wake = 0;
    let idle = std::iter::from_fn(move || {
        wake += 15_000 + rng.between(0, 3);
        Some(wake)
    });
    let fired = clock_firings(idle, 200);
    let (idle_worst, idle_drift) = gap_and_drift(&fired);

    // both measurements first, so a failure shows the whole picture (these are the figures the docs quote)
    eprintln!(
        "risk clock over 200 sweeps: busy worst gap {busy_worst} ms off 30 s, drift {busy_drift} ms; \
         idle worst gap {idle_worst} ms off 30 s, drift {idle_drift} ms (first firings {:?})",
        &fired[..4]
    );
    assert!(busy_worst <= 25, "busy: a gap strayed {busy_worst} ms from 30 s");
    assert!(busy_drift.abs() <= 25, "busy: the clock drifted {busy_drift} ms over 200 sweeps");
    assert!(idle_worst <= 10, "idle: a gap strayed {idle_worst} ms from 30 s");
    assert!(idle_drift.abs() <= 1_000, "idle: the clock drifted {idle_drift} ms over 200 sweeps");
}

/// A timer a whole cadence or more behind restarts from the boundary instead of running the missed firings back
/// to back: a boundary reached 100 s after the first deadline fires the clock ONCE, and the next firing is one
/// cadence after THAT boundary, not three catch-up firings in a row.
#[test]
fn a_clock_a_whole_cadence_behind_skips_the_missed_firings() {
    let fired =
        clock_firings([100_000, 100_010, 100_020, 129_990, 130_000, 130_010].into_iter(), 3);
    assert_eq!(
        fired,
        vec![100_000, 130_000],
        "once at the late boundary, then one cadence after it"
    );
}
