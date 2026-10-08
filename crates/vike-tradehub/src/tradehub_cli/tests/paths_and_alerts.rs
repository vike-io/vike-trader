//! Where the alerts rules file resolves, and the absent policy file.

use super::*;

// -- alerting: where the rules file is looked for --------------------------------------------

/// The rules file resolves in exactly TWO places and nowhere else: `$VIKE_ALERTS` when it
/// names one, else `<state_dir>/alerts.json`. With NEITHER there is no path at all — which is
/// what makes [`maybe_mount_alerts`] log the OFF line instead of quietly reading a file
/// somewhere the operator was never told about.
///
/// The third assert is the one that bites: a resolver that reached for a directory of its own
/// — beside the executable, the working directory, a home — would answer `Some` there.
#[test]
fn the_alerts_file_resolves_only_from_the_override_or_the_state_directory() {
    let state = Path::new("/tmp/vike-state");
    assert_eq!(
        alerts_path_in(None, Some(state)),
        Some(state.join("alerts.json")),
        "no override ⇒ the state directory, joined with the library's own basename"
    );
    assert_eq!(
        alerts_path_in(Some("/etc/vike/rules.json"), Some(state)),
        Some(PathBuf::from("/etc/vike/rules.json")),
        "an explicit $VIKE_ALERTS names the file outright"
    );
    assert_eq!(
        alerts_path_in(None, None),
        None,
        "no override and no state directory ⇒ NO path, never one of this resolver's own \
             invention"
    );
    // A blank override is an unset one in every shell that produced it.
    for blank in [Some(""), Some("   ")] {
        assert_eq!(alerts_path_in(blank, Some(state)), Some(state.join("alerts.json")));
        assert_eq!(alerts_path_in(blank, None), None, "and blank cannot conjure one either");
    }
}

// -- settings / policy (settings-unification Phase 6c) ---------------------------------------

/// THE no-file property, at this daemon's own edge: with no `policy` rows on the machine the
/// loader yields `Policy::default()`, whose venue-facing projection is `MountPolicy::default()`.
/// Driven through the REAL loader (`load(None, &{})` is exactly "no home directory, no project
/// file, no environment") rather than asserting the default struct, so a default that stopped
/// being the no-file answer would fail here.
///
/// ⚠ This test's NAME used to be `..._leaves_every_venue_on_its_own_literal`, and that is no
/// longer what the default means. Every SCALAR field is still `None` — each venue arm keeps its
/// compiled-in literal — but `venues` defaults to `paper` for every venue, so a daemon with no
/// `policy.venues` row mounts ALL PAPER. Asserted here rather than left to the equality, because the
/// equality would hold just as well if the default flipped to `live` on both sides.
#[test]
fn an_absent_policy_file_is_the_mount_default_and_arms_no_venue() {
    let settings = vike_config::load(None, &HashMap::new()).unwrap();
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
