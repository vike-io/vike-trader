//! The paper fallback's HALT arming, asserted on the CONSTRUCTED book.

/// Every paper fallback in `make_engine` arms the operator HALT sentinel.
///
/// A paper book is a MOUNT (the live gate's fallback), so `touch <project>/settings/state/HALT`
/// must reach it. Before `paper_client`, eleven fallback arms armed nothing: an all-paper daemon
/// silently observed no HALT file.
///
/// ⚠ Asserts on the CONSTRUCTED book, not the source text: a text gate cannot tell an armed
/// construction from an unarmed one. Drop `.with_halt_path(..)` from `paper_client` -> red.
#[test]
fn the_paper_fallback_every_venue_arm_uses_is_halt_armed() {
    let client = super::paper_client("binance", "BTCUSDT", vike_model::FeeSchedule::Free);
    assert!(
        client.halt_path().is_some(),
        "make_engine's paper fallback must arm the HALT sentinel — an operator rehearses the \
         kill switch on exactly this mount"
    );
}
