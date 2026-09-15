//! The maker's UNPRICED-BOOK hold, tested as behaviour: `requote`'s
//! `let Some((bid_px, ask_px)) = priced else { return }` may not be silent.
//!
//! ## Why this family exists — a measured cost, not a hypothetical
//!
//! On 2026-08-17 the CI box's live `spread_maker` (bybit BTCUSDT, demo) ran for hours emitting
//! `orders:0 working:0 fees:0.0 fault:null trading_state:"Active"` across ~112,000 summary `seq`
//! and **zero** non-summary log lines. Every field an operator can see said the daemon was healthy;
//! it had placed nothing at all.
//!
//! The investigation cost was paid entirely in the silence. `#1336` had just shipped the break-even
//! refusal, whose arithmetic said it MUST be engaged on that mount (a 60-tick `$6.00` ceiling
//! against a `$12.61` break-even), so the refusal was the obvious suspect. It was not the cause —
//! and the only thing that proved it was the ABSENCE of its warn, because
//! [`AsState::price`](crate::avellaneda::AsState) returns at `fair_value(view)?` **before** reaching
//! `note_fee_floor`. The guard built to make holds attributable was itself sitting downstream of an
//! unattributable one. The real cause was upstream of the maker entirely: the venue's market-data
//! socket was connected but subscribed to nothing (39 KB in five hours, a 1:1 send/receive ratio —
//! ping/pong keepalive, no `orderbook.50` data), so `requote` was never reaching a priced book.
//!
//! So the requirement these tests encode is not "return `None`" — it is **an operator can tell a
//! holding maker from a healthy one, and is pointed at the feed rather than at the tuning**. A test
//! asserting only the return value passes with the `warn!` deleted.
//!
//! ## Why edge-triggered
//!
//! `requote` is the vike-core hot fold, whose rule is *instrument per-order boundaries and fault
//! transitions only* (CLAUDE.md). A per-tick warn here is an unbounded log write on the money path
//! driven by a book feed — the same shape that once wrote a 341 GB journal on a live box. Both
//! edges are therefore latched and O(1) per episode, and
//! `a_hold_warns_once_per_episode_never_once_per_tick` is a PERFORMANCE contract as much as a
//! legibility one.

use super::*;

/// The the CI box grid: bybit BTCUSDT on its real `0.1` tick.
const TICK: f64 = 0.1;
const MID: f64 = 63_050.0;

/// A healthy two-sided view — one level a side, straddling [`MID`].
fn two_sided() -> BookView {
    BookView {
        tick_size: TICK,
        bids: vec![(MID - 0.5 * TICK, 5.0)],
        asks: vec![(MID + 0.5 * TICK, 5.0)],
    }
}

/// The pathological view: bids arrive, asks never do. This is what a half-subscribed or
/// partially-applied venue feed hands the maker, and what `fair_value` refuses to price.
fn bids_only() -> BookView {
    BookView { tick_size: TICK, bids: vec![(MID - 0.5 * TICK, 5.0)], asks: Vec::new() }
}

/// A plain `Mid`-style maker on the the CI box grid — no A-S layer, so this exercises the
/// `view.priced(..)` arm of `requote` rather than the A-S one. Both flow through the same
/// `let Some(..) = priced else` return, which is the point: the hold warn covers BOTH.
fn maker() -> SpreadMaker {
    SpreadMaker::new(0.001, 0.5)
}

/// **THE HEADLINE — a maker that cannot price says so.** Reddens with the `warn!` in
/// `SpreadMaker::note_no_quote` deleted, while every pre-existing one-sided-book test stays green,
/// because none of them asserts anything an operator could see.
#[test]
fn an_unpriceable_book_is_announced_not_silently_held() {
    let mut mm = maker();
    let mut b = broker(0.0, 0);
    let logs = captured_logs(|| mm.requote(&mut b, &bids_only(), 0));

    assert!(
        b.submissions.is_empty() && b.modifications.is_empty(),
        "the hold itself is unchanged — nothing may be placed or amended: {:?}",
        submits_dbg(&b)
    );
    assert!(logs.contains("maker HOLDING"), "…but it must be ANNOUNCED — got: {logs:?}");
    // The operator must be pointed at the FEED. Getting this wrong is what cost the 2026-08-17
    // investigation its first hours: the visible symptom invited a tuning hypothesis, and the
    // cause was a venue subscription delivering nothing.
    assert!(logs.contains("ask_levels=0"), "the warn must name the EMPTY side — got: {logs:?}");
    assert!(
        logs.contains("bid_levels=1"),
        "…and the populated one, so 'no data at all' is distinguishable from 'one side missing' \
         — got: {logs:?}"
    );
    assert!(
        logs.contains("FEED"),
        "…and must send the reader to the feed before the tuning — got: {logs:?}"
    );
}

/// EDGE-triggered. See this module's header: a per-tick warn on a book feed is an unbounded write
/// on the hot fold, so "exactly one" is a performance contract, not tidiness.
#[test]
fn a_hold_warns_once_per_episode_never_once_per_tick() {
    let mut mm = maker();
    let mut b = broker(0.0, 0);
    let logs = captured_logs(|| {
        for ts in 0..64 {
            mm.requote(&mut b, &bids_only(), ts);
        }
    });
    assert_eq!(
        logs.matches("maker HOLDING").count(),
        1,
        "64 unpriceable ticks must produce exactly ONE warn — got: {logs:?}"
    );
}

/// The NEGATIVE control, and the half that keeps the headline honest: a maker that CAN price says
/// nothing. Without it, a `warn!` fired unconditionally on every tick would satisfy the test above.
#[test]
fn a_healthy_maker_says_nothing() {
    let mut mm = maker();
    let mut b = broker(0.0, 0);
    let logs = captured_logs(|| mm.requote(&mut b, &two_sided(), 0));

    assert!(!b.submissions.is_empty(), "the control must actually quote: {:?}", submits_dbg(&b));
    assert!(
        !logs.contains("maker HOLDING") && !logs.contains("maker RESUMING"),
        "a working maker must be silent on the hot path — got: {logs:?}"
    );
}

/// The EXIT edge. Without it a recovered maker keeps looking permanently broken in the journal,
/// which is its own operator-facing lie — the mirror of the defect this family closes.
#[test]
fn recovery_is_announced_so_a_warn_cannot_outlive_its_cause() {
    let mut mm = maker();
    let mut b = broker(0.0, 0);
    let logs = captured_logs(|| {
        mm.requote(&mut b, &bids_only(), 0);
        mm.requote(&mut b, &two_sided(), 1);
    });
    assert!(logs.contains("maker HOLDING"), "the entry edge — got: {logs:?}");
    assert!(logs.contains("maker RESUMING"), "…and the exit edge — got: {logs:?}");
}

/// A re-entered hold warns AGAIN — the latch must reset on recovery, not fire once per process.
/// A latch that never rearms would report the first outage of a session and silently swallow every
/// later one, which is the same silence with extra steps.
#[test]
fn a_second_outage_warns_again() {
    let mut mm = maker();
    let mut b = broker(0.0, 0);
    let logs = captured_logs(|| {
        mm.requote(&mut b, &bids_only(), 0);
        mm.requote(&mut b, &two_sided(), 1);
        mm.requote(&mut b, &bids_only(), 2);
    });
    assert_eq!(
        logs.matches("maker HOLDING").count(),
        2,
        "two distinct outages are two distinct episodes — got: {logs:?}"
    );
}

/// ⚠ The two HOLD warns must not both announce the SAME hold.
///
/// When the break-even refusal is the cause, `AsState::note_fee_floor` has already said so WITH the
/// numbers that make it actionable (`break_even_half_spread` vs `max_half_spread`). A generic
/// "no two-sided quote" line on top would bury the specific one — so `note_no_quote` stays silent
/// and does not latch. This pins that arrangement; see `SpreadMaker::note_no_quote`'s doc.
#[test]
fn the_fee_floor_refusal_is_not_double_reported() {
    let mut mm = maker();
    // Arm the the CI box mount exactly: 2-tick floor, 60-tick ($6.00) ceiling, bybit's 4 bp round trip
    // — a $12.61 break-even the ceiling cannot reach, so A-S refuses on a perfectly healthy book.
    mm = mm.with_avellaneda_stoikov(AsParams {
        variance_mode: VarianceMode::RawLocal,
        horizon_mode: HorizonMode::ConstantTau,
        price_domain: PriceDomain::Unbounded,
        min_half_spread_ticks: 2.0,
        max_half_spread_ticks: 60.0,
        round_trip_fee_rate: Some(4e-4),
        gamma: 5e-4,
        q_scale: 3e-2,
        ..AsParams::default()
    });
    let mut b = broker(0.0, 0);
    let logs = captured_logs(|| mm.requote(&mut b, &two_sided(), 0));

    assert!(b.submissions.is_empty(), "the refusal still refuses: {:?}", submits_dbg(&b));
    assert_eq!(
        logs.matches("maker HOLDING").count(),
        1,
        "exactly ONE hold message, the specific one — got: {logs:?}"
    );
    assert!(
        logs.contains("break_even_half_spread"),
        "…and it must be the FEE-FLOOR message, which carries the numbers — got: {logs:?}"
    );
    assert!(
        !logs.contains("ask_levels"),
        "…not the generic unpriced-book one, whose book fields are irrelevant here — got: {logs:?}"
    );
}
