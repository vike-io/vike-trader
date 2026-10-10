//! The absent policy file.

// -- settings / policy (settings-unification Phase 6c) ---------------------------------------

/// THE no-file property, at this daemon's own edge: with no `policy` rows on the machine the
/// loader yields `Policy::default()`, whose venue-facing projection is `MountPolicy::default()`.
/// Driven through the REAL loader (`load(None)` is exactly "no home directory, no project
/// file") rather than asserting the default struct, so a default that stopped
/// being the no-file answer would fail here.
///
/// ⚠ This test's NAME used to be `..._leaves_every_venue_on_its_own_literal`, and that is no
/// longer what the default means. Every SCALAR field is still `None` — each venue arm keeps its
/// compiled-in literal — but `venues` defaults to `paper` for every venue, so a daemon with no
/// `policy.venues` row mounts ALL PAPER. Asserted here rather than left to the equality, because the
/// equality would hold just as well if the default flipped to `live` on both sides.
#[test]
fn an_absent_policy_file_is_the_mount_default_and_arms_no_venue() {
    let settings = vike_config::load(None).unwrap();
    let mount = vike_mount::MountPolicy::from(&settings.policy);
    assert_eq!(mount, vike_mount::MountPolicy::default());
    assert_eq!(mount.market_slippage, None, "no file ⇒ each venue keeps its own literal");
    for venue in vike_model::VENUES {
        assert_eq!(
            mount.venue_mode(venue),
            vike_config::VenueMode::Paper,
            "{venue}: no policy.venues row must arm NOTHING — credential presence is no longer a gate \
                 that can act alone"
        );
    }
    // ⚠ This asserted `warnings.is_empty()`, and that emptiness was the register entry
    // `docs/ops/kill-switches.md` carried as "a missing settings directory is silently
    // uncapped": the VALUES above are right and the operator was told nothing about why they
    // are the compiled-in defaults. The values did not move; only the silence did.
    assert_eq!(
        settings.warnings,
        vec![vike_config::NO_SETTINGS_DIRECTORY_WARNING.to_string()],
        "a daemon that resolved no project must SAY its ceilings are defaults: {settings:?}"
    );
}
