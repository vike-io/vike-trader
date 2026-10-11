//! The xEMM SAFETY layer — every way the maker stops quoting, and the one read it must never make.
//!
//! A cross-exchange maker's characteristic failure is not a bad price, it is quoting confidently
//! off information it no longer has: a frozen reference feed, an anchor that has moved, an exposure
//! it cannot see. Each test here drives one of those to its verdict.
//!
//! The first test is the structural one: `the_maker_never_reads_position_price_or_equity_through_the_broker`
//! pins that the maker's output is BIT-IDENTICAL against a broker whose every read surface is
//! poisoned. That is what lets the whole strategy own its inventory in a ledger instead of trusting
//! three live-runtime read paths that are all silently wrong for a foreign-venue leg.

use super::*;
use crate::{HaltReason, XemmMaker};
use vike_model::{FeedStatus, XemmParams};

const MAKER: &str = "BTC";
const HEDGE: &str = "BTC-USDT-SWAP";
const REF_VENUE: &str = "okx";

fn ref_q(ts: i64, bid: f64, ask: f64) -> QuoteTick {
    QuoteTick { ts, local_ts: 0, bid, ask, bid_size: 1.0, ask_size: 1.0, symbol: HEDGE.into() }
}
fn own_q(ts: i64, bid: f64, ask: f64) -> QuoteTick {
    QuoteTick { ts, local_ts: 0, bid, ask, bid_size: 1.0, ask_size: 1.0, symbol: MAKER.into() }
}
fn fill_of(symbol: &str, side: i32, size: f64, ts: i64) -> Fill {
    Fill { side, size, price: 100.0, fee: 0.0, ts, is_maker: true, symbol: symbol.into() }
}

fn maker() -> XemmMaker {
    XemmMaker::new(MAKER, HEDGE, 1.0, 0.001, 0.00065, 0.01).with_naked_bands(1e9, 1e9)
}

fn warm(m: &mut XemmMaker, ts: i64) {
    m.on_reference_quote(&mut broker(0.0, ts), REF_VENUE, &ref_q(ts, 100.0, 100.5));
    m.on_quote_tick(&mut broker(0.0, ts), &own_q(ts, 100.0, 100.02));
}

/// A bar every field of which is unusable — so a maker that reads HISTORY, not just the scalars,
/// still moves under [`poisoned`].
fn poison_bar(ts: i64) -> vike_model::Bar {
    vike_model::Bar {
        ts,
        open: f64::NAN,
        high: -1e300,
        low: f64::NAN,
        close: -1e300,
        volume: f64::NAN,
        funding: None,
        bid: None,
        ask: None,
        symbol: None,
    }
}

/// A broker whose EVERY read surface is poisoned — the scalars, and the per-symbol tables a
/// declared-multi mount carries (`bar_views` included, which also makes this the declared-mount
/// shape: an uncarried symbol reads EMPTY here rather than falling through to the scalars).
/// If the maker consults any of them the emitted prices move.
fn poisoned(now: i64) -> LiveBroker {
    LiveBroker {
        positions: vec![(MAKER.into(), -1e300), (HEDGE.into(), f64::NAN)],
        prices: vec![(MAKER.into(), f64::NAN), (HEDGE.into(), -1e300)],
        bar_views: vec![
            (MAKER.into(), std::sync::Arc::new(vec![poison_bar(now)])),
            (HEDGE.into(), std::sync::Arc::new(vec![poison_bar(now)])),
        ],
        position: f64::NAN,
        price: -1e300,
        equity: f64::NAN,
        bars: std::sync::Arc::new(Vec::new()),
        index: 0,
        now,
        multiplier: f64::NAN,
        lot_size: f64::NAN,
        submissions: Vec::new(),
        modifications: Vec::new(),
        cancels: Vec::new(),
        brackets: Vec::new(),
        conditionals: Vec::new(),
        mass_cancel: false,
    }
}

// --- G-1: THE STRUCTURAL PIN --------------------------------------------------------------------

/// The maker must read NO inventory, price or equity through the `Broker`/`HftBroker` seam.
///
/// This is not stylistic, and ⚠ the reason CHANGED rather than went away. Two of the three read
/// paths this doc used to name — `declared_views` resolving a declared leg against the MOUNT's
/// engine, and `dispatch_applied_fills` (the hook a hedge fires from) building its broker with EMPTY
/// per-symbol tables — were live-runtime defects, and both are fixed
/// (`crates/vike-core/tests/wiring/multi_symbol_reads.rs`). The third is intrinsic: `HftBroker::position`
/// takes NO symbol and returns the DISPATCHING engine's scalar, so on a hedge-fill dispatch it
/// confidently describes the wrong book. And no broker read of any shape can see a sent-but-unacked
/// hedge, which is the quantity `crates/vike-mm/src/xemm/hedge.rs`'s `HedgeLedger` computes a safe
/// retry from.
///
/// Driving the whole hook surface against a poisoned broker and getting BIT-IDENTICAL output is the
/// pin that catches a future refactor quietly reintroducing one of those reads.
#[test]
fn the_maker_never_reads_position_price_or_equity_through_the_broker() {
    fn drive(mut b: impl FnMut(i64) -> LiveBroker) -> Vec<(Option<String>, Option<f64>, f64)> {
        let mut m = maker().with_skew(0.0, 10.0, 0.8);
        m.on_reference_quote(&mut b(1_000), REF_VENUE, &ref_q(1_000, 100.0, 100.5));
        m.on_quote_tick(&mut b(1_000), &own_q(1_000, 100.0, 100.02));
        m.on_fill(&mut b(1_100), &fill_of(MAKER, 1, 3.0, 1_100));
        m.on_trade_tick(
            &mut b(1_200),
            &TradeTick {
                ts: 1_200,
                local_ts: 0,
                price: 100.1,
                size: 1.0,
                is_buyer_maker: false,
                symbol: MAKER.into(),
            },
        );
        let mut last = b(1_300);
        m.on_reference_quote(&mut last, REF_VENUE, &ref_q(1_300, 100.2, 100.7));
        // everything the maker asked the venue for, in order.
        let mut out: Vec<(Option<String>, Option<f64>, f64)> =
            last.submissions.iter().map(|s| (s.tag.clone(), s.price, s.qty)).collect();
        out.extend(
            last.modifications
                .iter()
                .map(|m| (Some(m.tag.clone()), m.new_price, m.new_qty.unwrap_or(0.0))),
        );
        out
    }

    let clean = drive(|now| broker(0.0, now));
    let dirty = drive(poisoned);
    assert!(!clean.is_empty(), "precondition: the maker actually emitted something to compare");
    assert_eq!(
        clean.len(),
        dirty.len(),
        "the poisoned broker changed HOW MANY orders were sent — a read leaked in"
    );
    for (c, d) in clean.iter().zip(dirty.iter()) {
        assert_eq!(c.0, d.0, "tag differs");
        assert_eq!(
            c.1.map(f64::to_bits),
            d.1.map(f64::to_bits),
            "PRICE differs under a poisoned broker: the maker read the broker's price/position"
        );
        assert_eq!(
            c.2.to_bits(),
            d.2.to_bits(),
            "SIZE differs: the maker read the broker's position"
        );
    }
}

// --- G-2 / G-3 / G-7: freshness ------------------------------------------------------------------

/// A REFERENCE touch older than `max_ref_age_ms` halts and pulls BOTH sides. This bound is strictly
/// stronger than a feed-status subscription: it also catches a feed that freezes without ever
/// reporting a disconnect, which is the failure a socket-liveness check cannot see.
#[test]
fn a_stale_reference_touch_pulls_both_sides() {
    let mut m = maker().with_freshness(2_000, 1_000_000, 1_000_000);
    warm(&mut m, 1_000);
    let mut b0 = broker(0.0, 1_000);
    m.on_quote_tick(&mut b0, &own_q(1_000, 100.0, 100.02));
    assert!(submitted(&b0, "bid") || modified(&b0, "bid"), "precondition: quoting");
    // the maker venue keeps ticking; the reference does not.
    let mut b = broker(0.0, 4_000);
    m.on_quote_tick(&mut b, &own_q(4_000, 100.0, 100.02));
    assert_eq!(m.halt_reason(), Some(HaltReason::ReferenceStale));
    assert!(canceled(&b, "bid") && canceled(&b, "ask"), "both quotes come off");
}

/// A stale OWN touch is just as fatal: `passive_clamp` anchors on it, so without a fresh one the
/// maker cannot prove its quote is non-marketable — and an unprovable quote is not emitted.
#[test]
fn a_stale_own_touch_pulls_both_sides() {
    let mut m = maker().with_freshness(1_000_000, 2_000, 1_000_000);
    warm(&mut m, 1_000);
    // the reference keeps ticking; the maker venue's own book does not.
    let mut b = broker(0.0, 4_000);
    m.on_reference_quote(&mut b, REF_VENUE, &ref_q(4_000, 100.0, 100.5));
    assert_eq!(m.halt_reason(), Some(HaltReason::OwnTouchStale));
    assert!(canceled(&b, "bid") && canceled(&b, "ask"));
}

/// The ALL-LANES-QUIET watchdog. When BOTH venues go silent no market lane fires at all, so the
/// per-venue age bounds cannot speak. The wall-clock `on_schedule` sweep is the one lane that still
/// runs — and it takes the quotes off.
#[test]
fn a_total_feed_outage_is_caught_by_the_schedule_sweep() {
    let mut m = maker().with_freshness(1_000_000, 1_000_000, 5_000);
    warm(&mut m, 1_000);
    let mut b = broker(0.0, 20_000);
    m.on_schedule(&mut b, "sweep");
    assert_eq!(m.halt_reason(), Some(HaltReason::FeedImpaired), "nothing has ticked for 19s");
    assert!(canceled(&b, "bid") && canceled(&b, "ask"));
}

/// A non-`Live` feed status on the MAKER venue halts and pulls immediately — no need to wait for an
/// age bound when the data layer has already said the stream is down.
#[test]
fn a_dead_maker_feed_halts_and_pulls() {
    for status in [FeedStatus::Disconnected, FeedStatus::Stale] {
        let mut m = maker();
        warm(&mut m, 1_000);
        let mut b = broker(0.0, 1_100);
        m.on_feed_status(&mut b, status);
        assert_eq!(m.halt_reason(), Some(HaltReason::FeedImpaired), "{status:?}");
        assert!(canceled(&b, "bid") && canceled(&b, "ask"), "{status:?}: both quotes come off");
    }
    // ...and a `Live` status is inert: it never places, cancels or un-halts anything.
    let mut m = maker();
    warm(&mut m, 1_000);
    let mut b = broker(0.0, 1_100);
    m.on_feed_status(&mut b, FeedStatus::Live);
    assert_eq!(m.halt_reason(), None);
    assert!(b.cancels.is_empty() && b.submissions.is_empty(), "a Live status emits nothing");
}

// --- G-5 / G-6: the naked bands ------------------------------------------------------------------

/// The SOFT band withholds the side that would GROW the exposure and leaves the reducing side
/// quoting, so the maker trades its way back toward flat rather than stopping dead.
#[test]
fn the_soft_naked_band_suppresses_only_the_growing_side() {
    // hedging off (ratio 0) so the exposure persists and the band is what is under test.
    let mut m = maker().with_naked_bands(2.0, 1e9);
    m.apply_params(&XemmParams { hedge_ratio: 0.0, ..m.params() });
    warm(&mut m, 1_000);
    // Accumulate +3 on the maker leg: net LONG past the 2.0 band ⇒ stop BUYING. The reaction lands
    // in the FILL's own dispatch — `on_fill` re-quotes immediately rather than waiting for the next
    // market tick, because the exposure it just created is what the band is measuring.
    let mut b = broker(0.0, 1_100);
    m.on_fill(&mut b, &fill_of(MAKER, 1, 3.0, 1_100));
    assert!(canceled(&b, "bid"), "long past the band ⇒ the BID is pulled");
    assert!(!canceled(&b, "ask"), "the reducing side keeps quoting: {:?}", b.cancels);
    assert!(modified(&b, "ask") || submitted(&b, "ask"), "and is actively maintained");
    // ...and it STAYS withheld on subsequent ticks (nothing re-places it while the band holds).
    let mut b2 = broker(0.0, 1_200);
    m.on_quote_tick(&mut b2, &own_q(1_200, 100.0, 100.02));
    assert!(!submitted(&b2, "bid") && !modified(&b2, "bid"), "the bid stays off the book");
    assert!(modified(&b2, "ask"), "while the ask keeps being maintained");
}

/// The HARD band halts, pulls, and fires a `taker_flatten` impulse for the EXCESS over the soft
/// band — the flatten-the-excess law, matched exactly against the pure helper so the two can never
/// disagree about how much to cut.
#[test]
fn the_hard_naked_band_halts_and_flattens_the_excess() {
    let mut m = maker().with_naked_bands(2.0, 4.0);
    m.apply_params(&XemmParams { hedge_ratio: 0.0, ..m.params() });
    warm(&mut m, 1_000);
    let mut b = broker(0.0, 1_100);
    m.on_fill(&mut b, &fill_of(MAKER, 1, 6.0, 1_100)); // 6 naked, hard band 4

    assert_eq!(m.halt_reason(), Some(HaltReason::NakedHardBand));
    assert!(canceled(&b, "bid") && canceled(&b, "ask"), "both quotes come off");
    let expected = crate::taker_flatten::taker_flatten(6.0, 2.0).expect("over the soft band");
    let flat: Vec<_> = b
        .submissions
        .iter()
        .filter(|s| s.tag.is_none())
        .map(|s| (s.symbol.clone(), s.side, s.qty))
        .collect();
    assert_eq!(
        flat,
        vec![(Some(HEDGE.to_string()), -1, expected.1)],
        "one flatten market on the HEDGE leg, cutting the excess over the SOFT band"
    );
    // FIRE-ONCE: repeated ticks while halted must not spam the taker venue.
    let mut b2 = broker(0.0, 1_200);
    m.on_schedule(&mut b2, "sweep");
    assert!(
        b2.submissions.iter().all(|s| s.tag.is_some()),
        "the flatten impulse fires once per halt, not once per tick: {:?}",
        submits_dbg(&b2)
    );
}

// --- G-8 / G-9: the model guard and the symptom guard --------------------------------------------

/// A basis outside the band halts: it means the pair has decoupled, the mount names the wrong
/// instrument, or a feed is lying. One number catches all three, and a halt is the only honest
/// response to "I do not know which".
#[test]
fn a_basis_outside_the_band_halts_and_pulls() {
    // A 10 ms half-life so ONE observation dominates the estimate — this test is about the BAND,
    // not about how fast the EWMA converges (that is pinned in `basis.rs`'s own tests).
    let mut m = maker().with_basis_band(50.0, 10, 0.5);
    warm(&mut m, 1_000);
    // the maker venue re-prices 2% below the reference — ~220 bp, far outside the 50 bp band.
    let mut b = broker(0.0, 1_100);
    m.on_quote_tick(&mut b, &own_q(1_100, 98.0, 98.02));
    assert_eq!(m.halt_reason(), Some(HaltReason::BasisOutOfBand));
    assert!(canceled(&b, "bid") && canceled(&b, "ask"));
    assert!(m.basis().is_some_and(|v| v < -0.01), "the estimate is published: {:?}", m.basis());
}

/// The one-sided-fill breaker fires on the SYMPTOM — repeated same-side maker fills — regardless of
/// whether the pricing model is right, which is what bounds the cost of a wrong assumption. It
/// suppresses ONE side; the other keeps quoting. (Mirrors `tests/breaker.rs` for `SpreadMaker`.)
#[test]
fn one_sided_maker_flow_trips_the_breaker() {
    let mut m = maker().with_fill_breaker(1_000, 2.5, 5_000);
    m.apply_params(&XemmParams { hedge_ratio: 0.0, ..m.params() });
    warm(&mut m, 1_000);
    let mut b = broker(0.0, 1_010);
    for t in [1_010, 1_020, 1_030] {
        b = broker(0.0, t);
        m.on_fill(&mut b, &fill_of(MAKER, 1, 1.0, t));
    }
    assert!(canceled(&b, "bid"), "3 net bid fills in the window suppress the BID");
    assert!(!canceled(&b, "ask"), "the ask side is untouched: {:?}", b.cancels);
    // and it stays withheld for the cooldown.
    let mut b2 = broker(0.0, 1_040);
    m.on_quote_tick(&mut b2, &own_q(1_040, 100.0, 100.02));
    assert!(!submitted(&b2, "bid") && !modified(&b2, "bid"), "still suppressed");
    assert!(modified(&b2, "ask"), "the other side keeps quoting throughout");
}

/// HEDGE-leg fills must NOT feed the breaker: they are the strategy's own mechanical offset, so
/// netting them would cancel every real one-sided signal to exactly zero and the guard would never
/// fire. (Same three maker fills as above, but each hedged — the breaker must still trip.)
#[test]
fn hedge_fills_do_not_cancel_the_breakers_signal() {
    let mut m = maker().with_fill_breaker(1_000, 2.5, 5_000);
    warm(&mut m, 1_000);
    let mut b = broker(0.0, 1_010);
    for t in [1_010, 1_020, 1_030] {
        b = broker(0.0, t);
        m.on_fill(&mut b, &fill_of(MAKER, 1, 1.0, t));
        m.on_fill(&mut broker(0.0, t), &fill_of(HEDGE, -1, 1.0, t)); // perfectly hedged
    }
    assert_eq!(m.naked_exposure().to_bits(), 0.0_f64.to_bits(), "precondition: fully hedged");
    assert!(
        canceled(&b, "bid"),
        "the breaker must see the MAKER flow only — folding the hedges in would net it to zero"
    );
}

// --- G-10 / G-11 / G-12: the halt latch and the off-path controls --------------------------------

/// A halt LATCHES: nothing is placed until it clears, and with `resume_after_halt_ms == 0` (the
/// default) it never clears on its own. A restart-by-accident must not be the way a halted maker
/// resumes.
#[test]
fn a_halt_latches_and_never_auto_resumes_by_default() {
    let mut m = maker().with_freshness(2_000, 1_000_000, 1_000_000);
    warm(&mut m, 1_000);
    m.on_quote_tick(&mut broker(0.0, 5_000), &own_q(5_000, 100.0, 100.02)); // reference stale
    assert_eq!(m.halt_reason(), Some(HaltReason::ReferenceStale));
    // even with BOTH feeds healthy again, the latch holds.
    let mut b = broker(0.0, 6_000);
    m.on_reference_quote(&mut b, REF_VENUE, &ref_q(6_000, 100.0, 100.5));
    m.on_quote_tick(&mut b, &own_q(6_000, 100.0, 100.02));
    assert_eq!(m.halt_reason(), Some(HaltReason::ReferenceStale), "the latch holds");
    assert!(b.submissions.is_empty(), "and nothing is placed: {:?}", submits_dbg(&b));
}

/// An operator who opts into auto-resume gets it — and the maker quotes again once the feeds are
/// genuinely healthy. This is the control that proves the latch above is the DEFAULT, not the only
/// behaviour.
#[test]
fn an_opted_in_resume_window_lets_the_maker_quote_again() {
    let mut m = maker().with_freshness(2_000, 1_000_000, 1_000_000).with_resume_after(1_000);
    warm(&mut m, 1_000);
    m.on_quote_tick(&mut broker(0.0, 5_000), &own_q(5_000, 100.0, 100.02));
    assert_eq!(m.halt_reason(), Some(HaltReason::ReferenceStale));
    let mut b = broker(0.0, 7_000);
    m.on_reference_quote(&mut b, REF_VENUE, &ref_q(7_000, 100.0, 100.5));
    m.on_quote_tick(&mut b, &own_q(7_000, 100.0, 100.02));
    assert_eq!(m.halt_reason(), None, "the resume window expired and both feeds are fresh");
    assert!(submitted(&b, "bid") && submitted(&b, "ask"), "and it quotes again");
}

/// OFF-PATH CONTROL: with NO reference touch the maker places nothing at all — it never falls back
/// to pricing off the venue it rests on, which would not be a cross-exchange maker but an unpriced
/// one.
#[test]
fn no_reference_quote_means_no_orders() {
    let mut m = maker();
    let mut b = broker(0.0, 1_000);
    for ts in [1_000, 2_000, 3_000] {
        m.on_quote_tick(&mut b, &own_q(ts, 100.0, 100.02));
    }
    assert!(
        b.submissions.is_empty(),
        "a maker with no reference quotes nothing: {:?}",
        submits_dbg(&b)
    );
    assert_eq!(
        m.halt_reason(),
        None,
        "a cold start is not a FAULT — nobody should be paged for it"
    );
}

/// OFF-PATH CONTROL: the maker never uses `mass_cancel`. In the live runtime it is scoped to the
/// DISPATCHING series, so from a hedge-fill dispatch it would target the taker venue and cancel
/// nothing on the maker venue — a pull that silently does not pull.
#[test]
fn the_maker_never_mass_cancels() {
    let mut m = maker().with_freshness(2_000, 2_000, 2_000);
    warm(&mut m, 1_000);
    let mut b = broker(0.0, 9_000);
    m.on_schedule(&mut b, "sweep"); // drives every guard into a halt + pull
    m.on_fill(&mut b, &fill_of(MAKER, 1, 1.0, 9_000));
    m.on_fill(&mut b, &fill_of(HEDGE, -1, 1.0, 9_000));
    assert!(!b.mass_cancel, "pulls are per-tag; mass_cancel is scoped to the wrong series");
}

// --- G-13 / G-14 / G-15: the emission invariants carried over from SpreadMaker --------------------

/// A PULL is never tolerance-gated. Even with a tolerance so wide that no re-price would ever be
/// sent, a quote that must come off the book comes off.
#[test]
fn a_pull_is_never_tolerance_gated() {
    let mut m = maker()
        .with_refresh_tolerance(10_000.0, 10_000.0)
        .with_freshness(2_000, 1e9 as i64, 1e9 as i64);
    warm(&mut m, 1_000);
    let mut b = broker(0.0, 5_000);
    m.on_quote_tick(&mut b, &own_q(5_000, 100.0, 100.02));
    assert!(canceled(&b, "bid") && canceled(&b, "ask"), "a 100% tolerance cannot strand a quote");
}

/// A FILL invalidates that side's snapshot so the size TOP-UP is re-issued. `SideState::own` is the
/// maker's INTENDED quote, not the venue-side remainder, so after a PARTIAL fill an
/// intended-vs-target compare reads "no change" and the top-up would be silently lost — the maker
/// would quote less size than configured, forever, with an anti-churn tolerance configured.
///
/// ⚠ The top-up lands in the FILL's OWN dispatch, not on the next market tick: unlike `SpreadMaker`
/// (whose `on_fill` only sets the flag), this maker re-quotes inside `on_fill` because that is
/// where it also fires the hedge. So the flag is set and consumed in the same call, and the
/// following tick is legitimately skipped again.
#[test]
fn a_partial_fill_re_issues_the_top_up_in_the_fills_own_dispatch() {
    let mut m = maker().with_refresh_tolerance(10_000.0, 10_000.0);
    warm(&mut m, 1_000);
    // an identical tick IS skipped entirely by the huge tolerance...
    let mut quiet = broker(0.0, 1_100);
    m.on_quote_tick(&mut quiet, &own_q(1_100, 100.0, 100.02));
    assert!(
        !modified(&quiet, "bid"),
        "precondition: the tolerance does skip an unchanged re-quote"
    );
    // ...but a fill on that side is not.
    let mut b = broker(0.0, 1_200);
    m.on_fill(&mut b, &fill_of(MAKER, 1, 0.4, 1_200));
    assert!(modified(&b, "bid"), "the filled side is re-issued at its full configured size");
    assert_eq!(
        b.modifications.iter().find(|m| m.tag == "bid").and_then(|m| m.new_qty).map(f64::to_bits),
        Some(1.0_f64.to_bits()),
        "and the size is the CONFIGURED one, not the post-fill remainder"
    );
}

/// A REFERENCE move re-prices IN PLACE — modify, never cancel/replace — so the venue queue position
/// survives. This is also the proof that the reference lane is the maker's primary re-quote clock:
/// venue A never ticked between the two assertions.
#[test]
fn a_reference_move_re_prices_in_place() {
    let mut m = maker();
    warm(&mut m, 1_000);
    let before = {
        let mut b = broker(0.0, 1_000);
        m.on_reference_quote(&mut b, REF_VENUE, &ref_q(1_000, 100.0, 100.5));
        b
    };
    assert!(
        modified(&before, "bid") || submitted(&before, "bid"),
        "precondition: a bid is resting"
    );
    let mut b = broker(0.0, 1_100);
    m.on_reference_quote(&mut b, REF_VENUE, &ref_q(1_100, 99.0, 99.5));
    assert!(modified(&b, "bid") && modified(&b, "ask"), "both sides re-price off B's move alone");
    assert!(b.cancels.is_empty(), "never cancel/replace — queue position is the maker's asset");
    assert!(b.submissions.is_empty(), "and nothing is re-submitted");
}
