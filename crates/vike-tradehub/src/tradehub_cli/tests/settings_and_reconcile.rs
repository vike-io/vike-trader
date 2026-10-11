//! Loader warnings, and the reconcile-on-restart gate and policy.

use super::*;

/// A loader warning is SURFACED, never swallowed. `main` emits exactly what
/// [`settings_warning_lines`] returns, so proving a warning survives that step proves the
/// daemon logs it — the failure this guards is the loader resolving something the operator did
/// not write while they believe their own file is in force.
///
/// ⚠ The warning is HAND-STUFFED, and stays so on purpose. The loader HAS a producer again
/// (`vike_config::NO_SETTINGS_DIRECTORY_WARNING`), and driving this through it would test the
/// producer rather than this daemon's half — the `settings: ` prefix and the verbatim text
/// reaching the log. `an_absent_policy_file_is_the_mount_default_and_arms_no_venue` above is
/// where the real producer is asserted at this daemon's edge; this stays a text that no
/// producer emits, so it cannot go green on a coincidence of wording.
#[test]
fn a_loader_warning_is_surfaced_not_swallowed() {
    let mut settings = vike_config::Settings::default();
    settings.warnings.push("preferences.something was resolved to 0.5".to_string());

    let lines = settings_warning_lines(&settings);
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(lines[0].starts_with("settings: "), "{}", lines[0]);
    assert!(lines[0].contains("preferences.something"), "names the key: {}", lines[0]);
    assert!(lines[0].contains("0.5"), "carried verbatim, not summarised: {}", lines[0]);
}

/// The quiet path stays quiet: nothing to resolve ⇒ nothing emitted, so a warn line in a
/// daemon's log always means something actually happened.
#[test]
fn a_clean_load_emits_no_settings_lines() {
    assert!(settings_warning_lines(&vike_config::Settings::default()).is_empty());
}

// -- reconcile-on-restart: the S2 default-ON gate and its refusal -------------------------

/// A `flags` settings row for one field, `true`.
fn flag_row(field: &str) -> vike_secrets::StoredSettings {
    vike_secrets::StoredSettings {
        settings: vec![vike_secrets::SettingRow {
            section: "flags".to_string(),
            key: field.to_string(),
            value: "true".to_string(),
        }],
        ..Default::default()
    }
}

/// [`vike_config::load_with_source`] over `rows` in a throwaway settings directory.
fn load_rows(rows: &vike_secrets::StoredSettings) -> vike_config::Settings {
    let dir = tempfile::tempdir().expect("temp settings dir");
    vike_config::load_with_source(
        Some(dir.path()),
        vike_config::StoreLayer::Rows { rows, adopted: None },
        &vike_config::CliOverrides::default(),
    )
    .unwrap()
}

/// The reconcile FORCE-ON flag reads the FLAGS ROW, and the row is its only source.
///
/// ⚠ **What this flag MEANS changed with S2.** `flags.reconcile` is no longer the default answer: a
/// mount that arms a live venue account reconciles without it (see the composed test below). What
/// it still does is force the driver on where the armed-live probe reports nothing, and that is
/// what a `flags` row has to keep being able to do.
///
/// Driven through the REAL `vike_config::load_with_source` rather than a hand-built `Flags`,
/// because the thing being asserted is the LOADER's resolution. The variable that used to override
/// the row refuses startup now (decision 0111), and that refusal is asserted here too.
#[test]
fn the_reconcile_gate_reads_the_flags_row() {
    // Neither flag set by default: the gate then rests entirely on the armed-live probe.
    let bare = vike_config::load(None).unwrap();
    assert!(!bare.flags.reconcile, "unset ⇒ no FORCE-on");
    assert!(!bare.flags.reconcile_off, "unset ⇒ not refused — `false` is the guarded state");

    // The ROW arms it.
    assert!(load_rows(&flag_row("reconcile")).flags.reconcile, "a row must arm the force-on gate");

    // …and the variable that used to outrank the row refuses startup instead.
    let on = HashMap::from([(vike_config::flags::RECONCILE_ENV.to_string(), "1".to_string())]);
    assert!(vike_config::refuse_removed_env(&on).is_err(), "VIKE_RECONCILE must refuse startup");
}

/// The REFUSAL is a written `flags.reconcile_off` row — durable, and the one layer an operator
/// has (decision 0111: the variable that took it on or off for one run refuses startup).
#[test]
fn the_reconcile_refusal_is_the_row() {
    assert!(load_rows(&flag_row("reconcile_off")).flags.reconcile_off);
    let env = HashMap::from([(vike_config::flags::RECONCILE_OFF_ENV.to_string(), "1".to_string())]);
    assert!(
        vike_config::refuse_removed_env(&env).is_err(),
        "VIKE_RECONCILE_OFF must refuse startup"
    );
}

/// **THE S2 property at this daemon's own edge**, composed over the REAL probe this mount uses
/// (`vike_mount::armed_live_venues`) rather than a hand-picked count: a mount that arms a venue
/// account reconciles with NOTHING set, and a mount that arms none does not. The two halves
/// share one credential map and differ only in the `account` row (decision 0119: an account trades
/// at its own tier while active), which is the lever an operator actually has.
///
/// Both credential names are BUILT with `format!` fragments and no venue literal, per the note
/// on the credentialed-data tests below: the settings-registry literal sweep reads a whole
/// env-shaped literal as a read sighting and would demand a `SETTINGS` row for this crate.
#[test]
fn a_live_armed_mount_reconciles_with_nothing_set_and_a_paper_one_does_not() {
    let venue = "bybit";
    let prefix = venue.to_uppercase();
    let creds: HashMap<String, String> = HashMap::from([
        (format!("{prefix}_DEMO_API_KEY"), "k".to_string()),
        (format!("{prefix}_DEMO_API_SECRET"), "s".to_string()),
    ]);

    // NO account row — an unread directory, the default. Credentials present and ignored.
    let paper = vike_mount::MountPolicy::default();
    let none_armed = vike_mount::armed_live_venues(
        crate::registry::REGISTRY,
        crate::wired_markets::WIRED_MARKETS,
        &creds,
        &paper,
    );
    assert!(none_armed.is_empty(), "no account row arms nothing: {none_armed:?}");
    assert!(
        !reconcile_config::reconcile_gate(false, false, none_armed.len()).enabled(),
        "a PAPER mount must build no reconcile driver — that is what keeps the default from \
             meaning `every process now talks to a venue`"
    );

    // …the same store with one ACTIVE `demo` row for the venue's default account.
    let armed_policy = vike_mount::MountPolicy::default().with_account(
        venue,
        &vike_model::accounts::account_keys::AccountLabel::Default,
        vike_config::VenueMode::Demo,
    );
    let armed = vike_mount::armed_live_venues(
        crate::registry::REGISTRY,
        crate::wired_markets::WIRED_MARKETS,
        &creds,
        &armed_policy,
    );
    assert!(armed.contains(&venue.to_string()), "the active demo row must arm {venue}: {armed:?}");
    let gate = reconcile_config::reconcile_gate(false, false, armed.len());
    assert_eq!(gate, reconcile_config::ReconcileGate::LiveDefault);
    assert!(gate.enabled(), "an armed account reconciles with NOTHING set — the S2 default");

    // …and the operator can still refuse it without disarming the venue.
    assert!(!reconcile_config::reconcile_gate(false, true, armed.len()).enabled());
}

/// QUARANTINE-FIRST: with no `config.reconcile_policy` row the daemon runs `quarantine`, so even
/// a local-origin divergence (`MissingFill`) is HELD — a default-on live daemon auto-folds NOTHING
/// (CLAUDE.md's rule: `hybrid` auto-applies `PositionDrift` on an incomplete position fetch,
/// rewriting position size and booking realized PnL at the venue's price).
///
/// ⚠ The default lives in `crate::reconcile_config::parse_policy`. This test stays HERE because
/// the claim it makes is about THIS DAEMON's effective policy — the thing an operator reads off
/// `docs/ops/tradehub-the CI box.md` — not about the helper.
#[test]
fn daemon_reconcile_policy_defaults_to_quarantine() {
    use vike_exec::recon::{DivergenceKind, ReconMode};
    let recon =
        daemon_recon_settings(vike_config::Flags::default(), &vike_config::Config::default());
    let cfg = reconcile_config::build_recon_config(&recon, HashMap::new());
    assert_eq!(cfg.policy.default, ReconMode::Quarantine);
    assert_eq!(cfg.policy.mode_for(DivergenceKind::MissingFill), ReconMode::Quarantine);
    assert!(
        crate::reconcile_config::auto_applied_kinds(&cfg.policy).is_empty(),
        "the daemon's own default must fold NOTHING without an operator"
    );
}

/// An operator who writes `config.reconcile_policy = hybrid` is honored verbatim — the
/// quarantine-first default only fills an ABSENT row.
#[test]
fn daemon_reconcile_policy_honors_the_row() {
    use vike_exec::recon::{DivergenceKind, ReconMode};
    let config = vike_config::Config {
        reconcile_policy: Some("hybrid".to_string()),
        ..vike_config::Config::default()
    };
    let recon = daemon_recon_settings(vike_config::Flags::default(), &config);
    let cfg = reconcile_config::build_recon_config(&recon, HashMap::new());
    // hybrid auto-synthesizes a local-origin MissingFill (the quarantine default holds it).
    assert_eq!(cfg.policy.mode_for(DivergenceKind::MissingFill), ReconMode::Synthesize);
}
