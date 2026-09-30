/// POLYMARKET'S OWN COMPOSITION, all four rows — the venue that is gated TWICE, pinned as a
/// matrix so neither gate can be dropped without a red test.
///
/// The whole matrix is the assertion rather than the one `true` row: dropping the master gate
/// (the shipped spelling until 2026-09-06, `poly_reconcile` alone) leaves row 2 green and only
/// row 3 red, and dropping the venue gate leaves row 3 green and only row 2 red. Asserting the
/// `true` row alone would pass with either gate removed, which is the shape of test this
/// workspace treats as a bug.
///
/// It is a pure function precisely because the real arm cannot be exercised in either
/// direction: reaching it needs a real Polygon key and a reachable CLOB (the same reason
/// `vike_bridge_core::venue_mount::recon_if_enabled` is extracted). What CANNOT be asserted here
/// is that the ARM calls it — that rests on review, and on
/// `polymarket_without_the_gates_is_inert_and_offline`, which runs the real arm with the master
/// gate ON and both venue gates off.
#[cfg(feature = "polymarket")]
#[test]
fn polymarket_wants_a_recon_client_only_when_both_gates_are_on() {
    assert!(
        super::poly_recon_wanted(true, true),
        "master gate on + flags.poly_reconcile ⇒ the venue's reconcile client is built — since S2 \
             the master gate is on by DEFAULT for a live mount, so this is the row a box carrying \
             flags.poly_reconcile on and no VIKE_RECONCILE now takes"
    );
    assert!(
        !super::poly_recon_wanted(true, false),
        "the venue gate is still an act nobody else needs: no POLY_RECONCILE ⇒ Polymarket \
             reconciles nothing, whatever the master gate says"
    );
    assert!(
        !super::poly_recon_wanted(false, true),
        "THE REGRESSION ROW: a refused or paper mount (VIKE_RECONCILE_OFF=1, or nothing armed \
             live) must do NO authenticated Polymarket work — the arm used to build the client \
             here anyway and let the driver-less root drop it"
    );
    assert!(!super::poly_recon_wanted(false, false), "neither gate ⇒ nothing, as before");
}
