//! The permissive-grid assertion of the mount binaries: `mount_roster.rs` (every roster venue, the
//! default lane) and the feature-on `ibkr_mount.rs`, `fxcm_mount.rs` and `polymarket_mount.rs`, each
//! of which `#[path]`-includes it (a bare `mod` would resolve beside the binary root, not here).

/// The permissive default a paper (or failed-pre-fetch) mount MUST carry: `from_properties` was
/// NOT applied, so every grid field stays `None` (= unconstrained). `im_requirement` is the one
/// field `make_engine` always sets (`Some(1.0)`, the conservative 1× buying-power default), so it
/// is deliberately not asserted here. This is the "falls back to permissive on absent
/// properties/creds" half of the RiskGate-property-grid contract — asserted for EVERY roster venue
/// by `all_roster_venues_absent_creds_stay_paper_and_inert`, and for the feature-on mounts by their
/// own binaries (ibkr's connect-failure demotion, fxcm's refused live mount, polymarket's inert
/// double gate). `venue` is threaded in so a failure names which venue's grid came back
/// non-permissive.
pub fn assert_permissive_grid(venue: &str, limits: &vike_model::RiskLimits) {
    assert_eq!(limits.tick_size, None, "{venue}: permissive grid has no tick constraint");
    assert_eq!(limits.lot_size, None, "{venue}: permissive grid has no lot constraint");
    assert_eq!(limits.min_qty, None, "{venue}: permissive grid has no min-qty constraint");
    assert_eq!(limits.min_notional, None, "{venue}: permissive grid has no notional constraint");
    // …and no PER-SYMBOL grid either. Asserted here so the whole roster carries it (this helper
    // is called from the roster-parameterized inert-default test): a mount that declares no leg
    // must leave `grid_by_symbol` empty, which is what keeps `RiskLimits::grid_for` returning
    // the scalars verbatim and keeps the serialized limits — hence
    // `vike_exec::state_hash` — byte-identical to before that map had a
    // producer. See `vike_mount::symbol_grid`'s module doc.
    assert!(
        limits.grid_by_symbol.is_empty(),
        "{venue}: a mount with no declared legs must carry no per-symbol grid"
    );
}
