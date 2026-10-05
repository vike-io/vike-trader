//! THE PRECEDENCE LAW, tested where it lives. Every one of these would have caught a caller
//! that wrote the slot without naming its concept — which is now impossible to express,
//! because `marks` is private and `set_mark_from` is the only door.

use super::*;

fn acct() -> Account {
    Account::new(1.0, "sim", None, BalanceMode::Delta)
}

#[test]
fn a_fresh_venue_mark_owns_the_slot_against_a_candle_close() {
    let mut a = acct();
    assert!(a.set_mark_from("sim", "BTC", 100.0, MarkSource::VenueMark, 1_000));
    assert!(
        !a.set_mark_from("sim", "BTC", 90.0, MarkSource::BarClose, 5_000),
        "the close must be REFUSED while the mark is fresh"
    );
    assert_eq!(a.mark_of("sim", "BTC"), Some(100.0));
    assert_eq!(a.mark_provenance("sim", "BTC"), Some((MarkSource::VenueMark, 1_000)));
}

#[test]
fn a_fresh_venue_mark_owns_the_slot_against_a_trade_tick_too() {
    // The tick lane (`drive_strategy_tick`) is a SEPARATE writer from the bar lane, and it
    // fires far more often. Rounds 1 and 2 guarded only the bar lane.
    let mut a = acct();
    a.set_mark_from("sim", "BTC", 100.0, MarkSource::VenueMark, 1_000);
    assert!(!a.set_mark_from("sim", "BTC", 99.0, MarkSource::TradeTick, 2_000));
    assert_eq!(a.mark_of("sim", "BTC"), Some(100.0));
}

#[test]
fn a_venue_mark_never_blocks_another_venue_mark() {
    let mut a = acct();
    a.set_mark_from("sim", "BTC", 100.0, MarkSource::VenueMark, 1_000);
    assert!(a.set_mark_from("sim", "BTC", 101.0, MarkSource::VenueMark, 1_500));
    assert!(a.set_mark_from("sim", "BTC", 102.0, MarkSource::ReconcileMark, 1_600));
    assert_eq!(a.mark_of("sim", "BTC"), Some(102.0));
}

#[test]
fn a_reconcile_mark_owns_the_slot_exactly_like_a_streamed_one() {
    // The deliberate decision (round 3): `ExecReport::position_mark_px` IS the venue's mark,
    // so it ranks with the streamed one — on EVERY venue, including those with no mark stream.
    let mut a = acct();
    a.set_mark_from("sim", "BTC", 100.0, MarkSource::ReconcileMark, 1_000);
    assert!(!a.set_mark_from("sim", "BTC", 90.0, MarkSource::BarClose, 2_000));
    assert_eq!(a.mark_of("sim", "BTC"), Some(100.0));
}

#[test]
fn a_silent_mark_stream_hands_the_slot_back_after_the_window() {
    // The documented degradation path: valuation must fall back to the next-best concept,
    // never freeze at a mark whose stream died.
    let mut a = acct();
    a.set_mark_from("sim", "BTC", 100.0, MarkSource::VenueMark, 1_000);
    assert!(!a.set_mark_from("sim", "BTC", 90.0, MarkSource::BarClose, 11_000), "age == window");
    assert!(
        a.set_mark_from("sim", "BTC", 90.0, MarkSource::BarClose, 11_001),
        "one ms past the window the close reclaims the slot"
    );
    assert_eq!(a.mark_of("sim", "BTC"), Some(90.0));
    assert_eq!(a.mark_provenance("sim", "BTC"), Some((MarkSource::BarClose, 11_001)));
}

// --- PER-SOURCE WINDOWS (round 4): reconcile marks own for a longer, cadence-sized window ---

/// (a) A reconciled STREAM-LESS venue: the reconcile mark holds the slot CONTINUOUSLY across
/// the reconcile cadence (default 60s), so candle closes raining in every couple of seconds
/// never alternate it away. Under the pre-round-4 shared 10s window the slot flipped
/// reconcile-mark → closes → next pass every minute — the alternation this window kills.
#[test]
fn a_reconcile_mark_holds_the_slot_across_the_reconcile_cadence() {
    let mut a = acct();
    // pass 1 at t=0
    a.set_mark_from("sim", "BTC", 100.0, MarkSource::ReconcileMark, 0);
    // closes arrive every ~2s for a full minute — none may take the slot
    for t in (1_000..=59_000).step_by(2_000) {
        assert!(
            !a.set_mark_from("sim", "BTC", 90.0, MarkSource::BarClose, t as i64),
            "a close must not displace the reconcile mark within its window (t={t})"
        );
    }
    assert_eq!(a.mark_of("sim", "BTC"), Some(100.0), "one concept, continuously");
    // pass 2 at t=60_000 refreshes ownership for another window
    assert!(a.set_mark_from("sim", "BTC", 101.0, MarkSource::ReconcileMark, 60_000));
    assert!(!a.set_mark_from("sim", "BTC", 90.0, MarkSource::BarClose, 119_000));
    assert_eq!(a.mark_of("sim", "BTC"), Some(101.0));
}

/// (b) The degradation law for the reconcile source: once reconcile STOPS for longer than
/// `reconcile_staleness_ms` (150s default), the candle close reclaims the slot — valuation
/// never freezes at a mark whose reconcile passes have ceased.
#[test]
fn a_stopped_reconcile_hands_the_slot_back_after_its_window() {
    let mut a = acct();
    a.set_mark_from("sim", "BTC", 100.0, MarkSource::ReconcileMark, 1_000);
    assert!(
        !a.set_mark_from("sim", "BTC", 90.0, MarkSource::BarClose, 151_000),
        "age == window: still owned"
    );
    assert!(
        a.set_mark_from("sim", "BTC", 90.0, MarkSource::BarClose, 151_001),
        "one ms past the reconcile window the close reclaims the slot"
    );
    assert_eq!(a.mark_of("sim", "BTC"), Some(90.0));
}

/// (c) A STREAMED venue mark still uses the SHORT window even though the (longer) reconcile
/// window is also configured — the window is keyed off the CURRENT OWNER's source. This is the
/// byte-identical-to-head guarantee for streamed-mark venues: the reconcile window must never
/// leak into the streamed-mark path.
#[test]
fn a_streamed_mark_keeps_the_short_window_regardless_of_the_reconcile_window() {
    let mut a = acct();
    a.set_mark_staleness_ms(10_000);
    a.set_reconcile_staleness_ms(150_000);
    a.set_mark_from("sim", "BTC", 100.0, MarkSource::VenueMark, 1_000);
    // a close reclaims at 10s+1 — the STREAMED window, not the reconcile one
    assert!(!a.set_mark_from("sim", "BTC", 90.0, MarkSource::BarClose, 11_000));
    assert!(a.set_mark_from("sim", "BTC", 90.0, MarkSource::BarClose, 11_001));
    assert_eq!(a.mark_of("sim", "BTC"), Some(90.0));
}

/// The "keyed off the current owner" nuance: a reconcile mark REPLACED by a streamed mark
/// immediately reverts to the short streamed window (a venue mark never blocks another venue
/// mark, and the fresher streamed mark then owns under the shorter horizon).
#[test]
fn a_streamed_mark_replacing_a_reconcile_mark_reverts_to_the_short_window() {
    let mut a = acct();
    a.set_mark_from("sim", "BTC", 100.0, MarkSource::ReconcileMark, 0);
    assert!(a.set_mark_from("sim", "BTC", 101.0, MarkSource::VenueMark, 1_000));
    assert!(!a.set_mark_from("sim", "BTC", 90.0, MarkSource::BarClose, 11_000));
    assert!(a.set_mark_from("sim", "BTC", 90.0, MarkSource::BarClose, 11_001));
    assert_eq!(a.mark_of("sim", "BTC"), Some(90.0));
}

#[test]
fn ownership_is_per_symbol_and_per_venue() {
    let mut a = acct();
    a.set_mark_from("sim", "BTC", 100.0, MarkSource::VenueMark, 1_000);
    assert!(a.set_mark_from("sim", "ETH", 50.0, MarkSource::BarClose, 1_000));
    assert!(a.set_mark_from("other", "BTC", 42.0, MarkSource::BarClose, 1_000));
    assert_eq!(a.mark_of("sim", "BTC"), Some(100.0));
    assert_eq!(a.mark_of("sim", "ETH"), Some(50.0));
    assert_eq!(a.mark_of("other", "BTC"), Some(42.0));
}

#[test]
fn a_venue_with_no_mark_at_all_is_byte_identical_to_last_write_wins() {
    // The preservation claim, stated as a test: with no venue mark ever written, every close
    // and tick lands, in order, exactly as the removed `set_mark` did.
    let mut a = acct();
    for (i, px) in [100.0_f64, 101.0, 99.5, 103.25].iter().enumerate() {
        assert!(a.set_mark_from("sim", "BTC", *px, MarkSource::BarClose, i as i64 * 60_000));
    }
    assert_eq!(a.mark_of("sim", "BTC").map(f64::to_bits), Some(103.25_f64.to_bits()));
}

#[test]
fn a_zero_window_restores_pure_last_write_wins() {
    let mut a = acct();
    a.set_mark_staleness_ms(0);
    a.set_mark_from("sim", "BTC", 100.0, MarkSource::VenueMark, 1_000);
    assert!(a.set_mark_from("sim", "BTC", 90.0, MarkSource::BarClose, 1_001));
    assert_eq!(a.mark_of("sim", "BTC"), Some(90.0));
}

#[test]
fn a_restored_account_starts_with_an_unowned_slot() {
    // Provenance is deliberately not snapshotted: replay must never inherit a stale owner
    // that would silently refuse every close for the rest of the session.
    let mut a = acct();
    a.set_mark_from("sim", "BTC", 100.0, MarkSource::VenueMark, 1_000);
    let restored = Account::restore(&a.snapshot());
    assert_eq!(restored.mark_of("sim", "BTC"), Some(100.0), "the price survives");
    assert_eq!(restored.mark_provenance("sim", "BTC"), None, "the ownership does not");
    let mut restored = restored;
    assert!(restored.set_mark_from("sim", "BTC", 90.0, MarkSource::BarClose, 1_001));
}
