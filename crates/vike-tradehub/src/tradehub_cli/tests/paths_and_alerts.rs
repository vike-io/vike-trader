//! The absent policy file.

// -- settings / policy (settings-unification Phase 6c) ---------------------------------------

/// THE no-file property, at this daemon's own edge: with no `policy` rows on the machine the
/// loader yields `Policy::default()`, whose venue-facing projection is `MountPolicy::default()`.
/// Driven through the REAL loader (`load(None)` is exactly "no home directory, no project
/// file") rather than asserting the default struct, so a default that stopped
/// being the no-file answer would fail here.
///
/// Every SCALAR field is `None` — each venue arm keeps its compiled-in literal. What ARMS a venue
/// is not a `policy` field at all since decision 0119 (an account trades at its own `account.tier`
/// while active), so the projection's `accounts` is the UNREAD directory here, and an unread
/// directory arms nothing even beside a credential set — asserted over the REAL probe the live
/// mount claims its locks from, because the equality above would hold just as well if the default
/// directory started arming on both sides.
///
/// The credential names are BUILT with `format!` fragments and no venue literal (the settings
/// registry's literal sweep reads a whole env-shaped literal as a read sighting).
#[test]
fn an_absent_policy_file_is_the_mount_default_and_arms_no_venue() {
    let settings = vike_config::load(None).unwrap();
    let mount = vike_mount::MountPolicy::from(&settings.policy);
    assert_eq!(mount, vike_mount::MountPolicy::default());
    assert_eq!(mount.market_slippage, None, "no file ⇒ each venue keeps its own literal");
    let prefix = "bybit".to_uppercase();
    let creds: std::collections::HashMap<String, String> = std::collections::HashMap::from([
        (format!("{prefix}_DEMO_API_KEY"), "k".to_string()),
        (format!("{prefix}_DEMO_API_SECRET"), "s".to_string()),
    ]);
    let armed = vike_mount::armed_live_venues(
        crate::registry::REGISTRY,
        crate::wired_markets::WIRED_MARKETS,
        &creds,
        &mount,
    );
    assert!(
        armed.is_empty(),
        "no account row must arm NOTHING — credential presence is not a gate that can act alone: \
         {armed:?}"
    );
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
