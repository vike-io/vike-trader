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

/// The reconcile FORCE-ON flag reads the FLAGS ROW, and the environment still wins over it.
///
/// ⚠ **What this flag MEANS changed with S2, and the test name did not, deliberately** — it is
/// still "the reconcile gate reads the row" (a `flags.toml` FILE until `docs/decisions/0086`).
/// `flags.reconcile` is no longer the default answer: a mount that arms a live venue account
/// reconciles without it (see the composed test below). What it still does is force the driver
/// on where the armed-live probe reports nothing, and that is what a `flags` row has to keep
/// being able to do.
///
/// Driven through the REAL `vike_config::load_with_source` over a throwaway settings directory
/// rather than a hand-built `Flags`, because the thing being asserted is the LOADER's
/// precedence, and a hand-built value would assert nothing about it — `load`/`load_with_cli`
/// consult no rows at all any more (`vike_config::load`'s own doc), so this is the lowest-level
/// call that still can. The env keys come from `vike_config`'s own constants so a rename cannot
/// leave this test passing against a name nothing reads.
#[test]
fn the_reconcile_gate_reads_the_flags_row_and_the_env_still_wins() {
    let dir = tempfile::tempdir().expect("temp settings dir");
    let rows = flag_row("reconcile");
    let load_rows = |env: &HashMap<String, String>| {
        vike_config::load_with_source(
            Some(dir.path()),
            vike_config::StoreLayer::Rows { rows: &rows, adopted: None },
            env,
            &vike_config::CliOverrides::default(),
        )
        .unwrap()
    };

    // Neither flag set by default: the gate then rests entirely on the armed-live probe.
    let bare = vike_config::load(None, &HashMap::new()).unwrap();
    assert!(!bare.flags.reconcile, "unset ⇒ no FORCE-on");
    assert!(!bare.flags.reconcile_off, "unset ⇒ not refused — `false` is the guarded state");

    // The ROW arms it. This is the half that did nothing before the wiring.
    let from_row = load_rows(&HashMap::new());
    assert!(from_row.flags.reconcile, "a `flags.reconcile` row must arm the force-on gate");

    // …and the environment still outranks the row, in BOTH directions.
    let off = HashMap::from([(vike_config::flags::RECONCILE_ENV.to_string(), "0".to_string())]);
    let overridden = load_rows(&off);
    assert!(!overridden.flags.reconcile, "env must override a row `true`");

    let on = HashMap::from([(vike_config::flags::RECONCILE_ENV.to_string(), "1".to_string())]);
    assert!(vike_config::load(None, &on).unwrap().flags.reconcile);
}

/// The REFUSAL is reachable from both layers an operator has — a written `flags.reconcile_off`
/// row and a one-run exported variable — because a default-on behaviour that can only be
/// refused from a row cannot be refused at 3am, and one that can only be refused from the
/// environment cannot be refused durably on a box whose unit somebody else owns.
#[test]
fn the_reconcile_refusal_is_reachable_from_the_row_and_from_the_environment() {
    let dir = tempfile::tempdir().expect("temp settings dir");
    let rows = flag_row("reconcile_off");
    assert!(
        vike_config::load_with_source(
            Some(dir.path()),
            vike_config::StoreLayer::Rows { rows: &rows, adopted: None },
            &HashMap::new(),
            &vike_config::CliOverrides::default(),
        )
        .unwrap()
        .flags
        .reconcile_off
    );

    let env = HashMap::from([(vike_config::flags::RECONCILE_OFF_ENV.to_string(), "1".to_string())]);
    assert!(vike_config::load(None, &env).unwrap().flags.reconcile_off);

    // …and the environment can take a deployed refusal back off for one run.
    let back_on =
        HashMap::from([(vike_config::flags::RECONCILE_OFF_ENV.to_string(), "0".to_string())]);
    assert!(
        !vike_config::load_with_source(
            Some(dir.path()),
            vike_config::StoreLayer::Rows { rows: &rows, adopted: None },
            &back_on,
            &vike_config::CliOverrides::default(),
        )
        .unwrap()
        .flags
        .reconcile_off
    );
}

/// **THE S2 property at this daemon's own edge**, composed over the REAL probe this mount uses
/// (`vike_mount::armed_live_venues`) rather than a hand-picked count: a mount that arms a venue
/// account reconciles with NOTHING set, and a mount that arms none does not. The two halves
/// share one credential map and differ only in the arming CEILING, which is the lever an
/// operator actually has.
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

    // PAPER ceiling — the shipped default for every venue. Credentials present and ignored.
    let paper = vike_mount::MountPolicy::default();
    let none_armed = vike_mount::armed_live_venues(
        crate::registry::REGISTRY,
        crate::wired_markets::WIRED_MARKETS,
        &creds,
        &paper,
    );
    assert!(none_armed.is_empty(), "a paper ceiling arms nothing: {none_armed:?}");
    assert!(
        !reconcile_config::reconcile_gate(false, false, none_armed.len()).enabled(),
        "a PAPER mount must build no reconcile driver — that is what keeps the default from \
             meaning `every process now talks to a venue`"
    );

    // …the same store under a ceiling that permits the demo tier.
    let armed_policy = vike_mount::MountPolicy {
        venues: vike_config::VenuePolicy::default().declare(venue, vike_config::VenueMode::Demo),
        ..Default::default()
    };
    let armed = vike_mount::armed_live_venues(
        crate::registry::REGISTRY,
        crate::wired_markets::WIRED_MARKETS,
        &creds,
        &armed_policy,
    );
    assert!(armed.contains(&venue.to_string()), "the ceiling must arm {venue}: {armed:?}");
    let gate = reconcile_config::reconcile_gate(false, false, armed.len());
    assert_eq!(gate, reconcile_config::ReconcileGate::LiveDefault);
    assert!(gate.enabled(), "an armed account reconciles with NOTHING set — the S2 default");

    // …and the operator can still refuse it without disarming the venue.
    assert!(!reconcile_config::reconcile_gate(false, true, armed.len()).enabled());
}

/// QUARANTINE-FIRST: with `VIKE_RECONCILE_POLICY` unset the daemon folds in `quarantine`, so
/// even a local-origin divergence (`MissingFill`) is HELD — a default-on live daemon auto-folds
/// NOTHING (CLAUDE.md's rule: `hybrid` auto-applies `PositionDrift` on an incomplete position
/// fetch, rewriting position size and booking realized PnL at the venue's price). Contrast the
/// `hybrid` reference default, which synthesizes it below.
///
/// ⚠ The fold itself now lives in `crate::reconcile_config::quarantine_first_default` (the
/// GUI's live mount needs the same pairing, and was running `hybrid`). This test stays HERE
/// because the claim it makes is about THIS DAEMON's effective policy — the thing an operator
/// reads off `docs/ops/tradehub-the CI box.md` — not about the helper.
#[test]
fn daemon_reconcile_policy_defaults_to_quarantine() {
    use vike_exec::recon::{DivergenceKind, ReconMode};
    let env = reconcile_config::quarantine_first_default(HashMap::new());
    let cfg = reconcile_config::build_recon_config(&env, HashMap::new());
    assert_eq!(cfg.policy.default, ReconMode::Quarantine);
    assert_eq!(cfg.policy.mode_for(DivergenceKind::MissingFill), ReconMode::Quarantine);
    assert!(
        crate::reconcile_config::auto_applied_kinds(&cfg.policy).is_empty(),
        "the daemon's own default must fold NOTHING without an operator"
    );
}

/// An operator who sets `VIKE_RECONCILE_POLICY=hybrid` is honored verbatim — the quarantine-first
/// default only fills an UNSET value.
#[test]
fn daemon_reconcile_policy_honors_explicit_override() {
    use vike_exec::recon::{DivergenceKind, ReconMode};
    let env = reconcile_config::quarantine_first_default(HashMap::from([(
        "VIKE_RECONCILE_POLICY".to_string(),
        "hybrid".to_string(),
    )]));
    let cfg = reconcile_config::build_recon_config(&env, HashMap::new());
    // hybrid auto-synthesizes a local-origin MissingFill (the quarantine default holds it).
    assert_eq!(cfg.policy.mode_for(DivergenceKind::MissingFill), ReconMode::Synthesize);
}
