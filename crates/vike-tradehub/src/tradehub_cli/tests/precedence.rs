//! The fold suite: the resolved row is the only source of every key this binary folds.

use super::*;

// ---------------------------------------------------------------------------------------------
// THE FOLD SUITE — "the row is the only source", as a test rather than a doc comment
//
// Every key the unread-settings sweep wired is read by a library several frames below this
// binary, out of a map this binary FILLS. So "which source wins" stopped being a property of
// one `env::var` call and became a property of a fold — and a fold is exactly the kind of thing
// a doc comment can claim and the code can contradict. Decision 0111 made every one of these keys
// a row: the variable of each refuses startup, so the only other source a fold could let through
// is a credential-store line of the same name, and the tests below hold that it cannot.
// ---------------------------------------------------------------------------------------------

/// The `flags` KEY for a folded variable, from the registry rather than from a second
/// hand-written mapping — `vike_config::flags::FLAG_REGISTRY` is the authority that the field
/// and the variable belong together, and `crates/vike-config/tests/flag_registry.rs` gates it
/// both ways.
fn flags_key_for(var: &str) -> &'static str {
    vike_config::flags::FLAG_REGISTRY
        .iter()
        .find(|m| m.env == var)
        .unwrap_or_else(|| panic!("{var} is folded but has no FLAG_REGISTRY row"))
        .field
}

/// A settings tree and a `flags` settings row for one key. `docs/decisions/0086`: there is no
/// `flags.toml` for it to be a line of any more — `vike_config::load`/`load_with_cli` consult no
/// rows at all, so every caller below drives `load_with_source` with the row directly.
fn settings_dir_with_flag(
    key: &str,
    value: bool,
) -> (tempfile::TempDir, vike_secrets::StoredSettings) {
    let dir = tempfile::tempdir().expect("a temp settings dir");
    let rows = vike_secrets::StoredSettings {
        settings: vec![vike_secrets::SettingRow {
            section: "flags".to_string(),
            key: key.to_string(),
            value: value.to_string(),
        }],
        ..Default::default()
    };
    (dir, rows)
}

/// [`vike_config::load_with_source`] with `rows` as the settings database's answer — the one
/// call that still consults them.
fn load_flag_row(
    dir: &tempfile::TempDir,
    rows: &vike_secrets::StoredSettings,
) -> vike_config::Settings {
    vike_config::load_with_source(
        Some(dir.path()),
        vike_config::StoreLayer::Rows { rows, adopted: None },
        &vike_config::CliOverrides::default(),
    )
    .unwrap_or_else(|e| panic!("the settings must load: {e}"))
}

/// The resolved value of ONE folded row, read back out of the same table the fold iterates.
fn resolved_row(flags: vike_config::Flags, var: &str) -> bool {
    folded_flag_rows(flags)
        .into_iter()
        .find(|(n, _, _)| *n == var)
        .map(|(_, on, _)| on)
        .expect("the row is in the table it came from")
}

/// **Every folded variable REFUSES startup, and a set one moves nothing** (decisions 0095 and
/// 0111): the row is the only source. Derived from the fold's own table, so a key folded later is
/// covered the day it is.
#[test]
fn every_folded_flag_variable_is_refused_not_folded() {
    let folded: Vec<&str> = folded_flag_rows(vike_config::Flags::default())
        .into_iter()
        .map(|(var, _, _)| var)
        .collect();
    // Anti-vacuity: the Polymarket gates are folded, so an empty set means the derivation broke.
    assert!(folded.contains(&vike_config::flags::POLY_EXEC_ENV), "{folded:?}");
    for var in folded {
        let env = HashMap::from([(var.to_string(), "1".to_string())]);
        assert!(vike_config::refuse_removed_env(&env).is_err(), "{var}=1 must refuse startup");
        let key = flags_key_for(var);
        let (dir, rows) = settings_dir_with_flag(key, false);
        let settings = load_flag_row(&dir, &rows);
        assert!(!resolved_row(settings.flags, var), "{var}=1 must not arm `{key}`");
    }
}

/// The wiring WORKS: its `flags` row says `true`, and every folded key reaches the map as `"1"`.
#[test]
fn a_row_value_reaches_the_map() {
    for (var, _, _) in folded_flag_rows(vike_config::Flags::default()) {
        let key = flags_key_for(var);
        let (dir, rows) = settings_dir_with_flag(key, true);
        let settings = load_flag_row(&dir, &rows);
        let mut vars = HashMap::new();
        fold_flags_into_vars(settings.flags, &mut vars);
        assert_eq!(
            vars.get(var).map(String::as_str),
            Some("1"),
            "{var}: a `{key} = true` row must reach the map — that IS the wiring"
        );
    }
}

/// Decision 0095: the withdraw override has no environment layer — `flags.allow_withdraw_keys` is
/// its only source. The fold writes the row's answer OVER whatever the credential store carried
/// (`FoldTier::Resolved`), so a stale store line cannot arm it, an exported variable changes
/// nothing here (it refuses startup instead), and the mount-path reader sees exactly the row.
#[test]
fn the_withdraw_override_is_the_row_alone() {
    let var = vike_config::flags::ALLOW_WITHDRAW_KEYS_ENV;
    let exported = HashMap::from([(var.to_string(), "0".to_string())]);
    assert!(vike_config::refuse_removed_env(&exported).is_err(), "{var} must refuse startup");
    for row in [true, false] {
        let (dir, rows) = settings_dir_with_flag(flags_key_for(var), row);
        let settings = load_flag_row(&dir, &rows);
        let mut vars = HashMap::from([(var.to_string(), "1".to_string())]);
        fold_flags_into_vars(settings.flags, &mut vars);
        assert_eq!(
            vike_bridge_core::key_permissions::allow_withdraw_keys(&vars),
            row,
            "row = {row}: the fold must carry the row, never the store's `1` or the export's `0`"
        );
    }
}

/// **The resolved flag is the SOLE source of every folded key — a credential-store line of ANY
/// spelling neither survives the fold nor changes what it writes.**
///
/// This replaced `the_credential_store_tier_is_exactly_the_refused_arming_set`, which held that
/// exactly the keys whose store line is "refused at startup" may keep the store as a tier. That
/// pairing was the bug (decision 0095's review): the refusal's value grammar is narrower than the
/// readers', so a `1 x` row armed real-money Polymarket exec over `flags.poly_exec = false` without
/// tripping it. The property worth holding is the one that does not depend on the refusal at all —
/// every hostile spelling, over both a false and a true flag, ends up as the flag's own value.
#[test]
fn no_credential_store_line_survives_the_fold_for_any_key() {
    let all_on = vike_config::Flags {
        poly_exec: true,
        poly_reconcile: true,
        hyperliquid_hip3: true,
        record_properties: true,
        allow_withdraw_keys: true,
        preflight_skip: true,
        ..Default::default()
    };
    for flags in [vike_config::Flags::default(), all_on] {
        for (var, resolved, FoldTier::Resolved) in folded_flag_rows(flags) {
            for hostile in ["1", "0", "1 x", "1 # arm it", "true", "garbage", ""] {
                let mut vars = HashMap::from([(var.to_string(), hostile.to_string())]);
                fold_flags_into_vars(flags, &mut vars);
                assert_eq!(
                    vars.get(var).map(String::as_str),
                    Some(flag_wire_value(resolved).as_str()),
                    "{var}: a credential line `{hostile}` must not survive the fold — the \
                     resolved flag ({resolved}) is the only thing that may reach the reader"
                );
            }
        }
    }
}

/// **A credential row can neither ARM nor VETO the two Polymarket gates** — the exposure decision
/// 0095's review found: `POLY_EXEC=1 x` passes `vike_config::refuse_credential_file_arming` (its
/// grammar is "the text before `#`, trimmed, is exactly `1`") yet the reader takes the first token,
/// so the row armed real-money exec over `flags.poly_exec = false`; and a `POLY_EXEC=0` row
/// silently vetoed a true flag. With `FoldTier::Resolved` the map carries the flag's value whatever
/// the store held.
#[test]
fn a_credential_row_can_neither_arm_nor_veto_the_polymarket_gates() {
    use vike_config::flags::{POLY_EXEC_ENV, POLY_RECONCILE_ENV};
    let only = |var: &str, on: bool| match var {
        POLY_EXEC_ENV => vike_config::Flags { poly_exec: on, ..Default::default() },
        POLY_RECONCILE_ENV => vike_config::Flags { poly_reconcile: on, ..Default::default() },
        other => panic!("not a Polymarket gate: {other}"),
    };
    for var in [POLY_EXEC_ENV, POLY_RECONCILE_ENV] {
        // Flag OFF, and a row that ARMS in the reader: the map must say off.
        for arming in ["1", "1 x", "1 # arm it"] {
            let mut vars = HashMap::from([(var.to_string(), arming.to_string())]);
            fold_flags_into_vars(only(var, false), &mut vars);
            assert_eq!(
                vars.get(var).map(String::as_str),
                Some("0"),
                "{var}: a credential row `{arming}` armed a gate whose flag is off"
            );
        }
        // Flag ON, and a row that would VETO: the flag arms it.
        let mut vars = HashMap::from([(var.to_string(), "0".to_string())]);
        fold_flags_into_vars(only(var, true), &mut vars);
        assert_eq!(
            vars.get(var).map(String::as_str),
            Some("1"),
            "{var}: a credential row `0` vetoed a flag that is on"
        );
    }
}

/// The same, through the REAL readers the mount consults — and the sanity half that keeps the test
/// honest: the raw `1 x` row DOES arm the reader and DOES slip the boot refusal, so this fails if
/// the exposure it closes ever stops being real (and the fix stops being the thing under test).
#[cfg(feature = "polymarket")]
#[test]
fn the_polymarket_readers_see_the_flag_and_not_the_credential_row() {
    use vike_config::flags::{POLY_EXEC_ENV, POLY_RECONCILE_ENV};
    let sneaky = HashMap::from([
        (POLY_EXEC_ENV.to_string(), "1 x".to_string()),
        (POLY_RECONCILE_ENV.to_string(), "1 x".to_string()),
    ]);
    assert!(vike_polymarket::poly_exec_enabled(&sneaky), "the raw row arms the exec gate");
    assert!(vike_polymarket::poly_reconcile_enabled(&sneaky), "…and the reconcile gate");
    assert!(
        vike_config::refuse_credential_file_arming(&sneaky).is_ok(),
        "…while the boot refusal's narrower grammar lets it through — the hole"
    );

    let mut vars = sneaky.clone();
    fold_flags_into_vars(vike_config::Flags::default(), &mut vars);
    assert!(!vike_polymarket::poly_exec_enabled(&vars), "a false flag disarms the exec gate");
    assert!(!vike_polymarket::poly_reconcile_enabled(&vars), "…and the reconcile gate");

    let mut vars = HashMap::from([
        (POLY_EXEC_ENV.to_string(), "0".to_string()),
        (POLY_RECONCILE_ENV.to_string(), "0".to_string()),
    ]);
    let on = vike_config::Flags { poly_exec: true, poly_reconcile: true, ..Default::default() };
    fold_flags_into_vars(on, &mut vars);
    assert!(vike_polymarket::poly_exec_enabled(&vars), "a true flag arms it over a `0` row");
    assert!(vike_polymarket::poly_reconcile_enabled(&vars), "…and the reconcile gate");
}

/// **The `data_only` disclosure, argued rather than assumed.** The fold runs BEFORE
/// `withhold_venue_credentials`, which strips `vars` by `{VENUE}_` prefix and reports the count
/// in the operator-facing warning — so a folded key whose NAME began with an eligible venue's
/// prefix would be counted as one more withheld CREDENTIAL than the store ever held.
///
/// None does, today: the eligible set is `crate::config::DATA_ONLY_VENUES`
/// (alpaca/ctrader/ig/oanda — a `data_only` hyperliquid or polymarket mount is refused at
/// profile load), and no folded key starts with any of those prefixes. This pins it from BOTH
/// tables, so either a new folded key or a new eligible venue reddens here instead of quietly
/// making a disclosure line wrong.
#[test]
fn no_folded_flag_key_collides_with_a_data_only_venue_prefix() {
    for (var, _, _) in folded_flag_rows(vike_config::Flags::default()) {
        for venue in crate::config::DATA_ONLY_VENUES {
            let prefix = format!("{}_", venue.to_uppercase());
            assert!(
                !var.starts_with(&prefix),
                "{var} starts with `{prefix}`, so a `data_only = true` {venue} mount would \
                     strip it and COUNT it as a withheld credential in the startup disclosure — \
                     fold it after the withhold, or rename the key"
            );
        }
    }
}

/// The reconcile family is the ROWS and nothing else (decision 0111): `daemon_recon_settings`
/// carries each `config.reconcile_*` row and both `flags.reconcile_*` switches through unchanged,
/// and `build_recon_config` reads them as the knobs they are.
#[test]
fn the_reconcile_settings_are_the_rows() {
    let config = vike_config::Config {
        reconcile_policy: Some("external-quarantine".to_string()),
        reconcile_interval_ms: Some(30_000),
        reconcile_audit_ms: Some(0),
        reconcile_lookback_ms: Some(7_200_000),
        reconcile_startup_delay_ms: Some(500),
        reconcile_balance_tol_abs: Some(2.5),
        reconcile_balance_tol_rel: Some(0.001),
        ..vike_config::Config::default()
    };
    let flags = vike_config::Flags {
        reconcile_balance: true,
        reconcile_generate_missing: true,
        ..vike_config::Flags::default()
    };
    let recon = daemon_recon_settings(flags, &config);
    assert_eq!(
        recon,
        reconcile_config::ReconSettings {
            policy: Some("external-quarantine".to_string()),
            interval_ms: Some(30_000),
            audit_ms: Some(0),
            lookback_ms: Some(7_200_000),
            startup_delay_ms: Some(500),
            balance_tol_abs: Some(2.5),
            balance_tol_rel: Some(0.001),
            generate_missing: true,
            balance: true,
        }
    );
    let cfg = reconcile_config::build_recon_config(&recon, HashMap::new());
    assert_eq!(cfg.interval, Some(std::time::Duration::from_millis(30_000)));
    assert_eq!(cfg.audit_interval, None, "a `0` row is the reader's own 'no audit' spelling");
    assert_eq!(cfg.lookback_ms, 7_200_000);
    assert_eq!(cfg.startup_delay, std::time::Duration::from_millis(500));
    assert_eq!(cfg.balance_tol.abs_floor, 2.5);
    assert_eq!(cfg.balance_tol.rel_frac, 0.001);
}

/// No reconcile rows at all is the all-default value — and its policy is `quarantine`, the
/// quarantine-first default [`reconcile_config::parse_policy`] owns.
#[test]
fn no_reconcile_rows_are_the_quarantine_first_defaults() {
    let recon =
        daemon_recon_settings(vike_config::Flags::default(), &vike_config::Config::default());
    assert_eq!(recon, reconcile_config::ReconSettings::default());
    // `ReconPolicy` has no `PartialEq`; its `Debug` is the whole value.
    assert_eq!(
        format!("{:?}", reconcile_config::parse_policy(recon.policy.as_deref())),
        format!("{:?}", reconcile_config::parse_policy(Some("quarantine"))),
    );
}

/// `config.tradehub_control_rate` is the rate's only source: the row, rendered in the resolver's
/// own grammar, else nothing.
#[test]
fn the_control_rate_is_the_row() {
    use settings::control_rate_input;
    assert_eq!(control_rate_input(Some(3.0)).as_deref(), Some("3"));
    assert_eq!(control_rate_input(Some(0.5)).as_deref(), Some("0.5"));
    assert_eq!(control_rate_input(None), None);
    let limits = server::control::ControlLimitsConfig::from_policy(
        None,
        control_rate_input(Some(3.0)).as_deref(),
    );
    assert_eq!(limits.rate_per_sec, 3.0, "the resolver reads the row");
}

/// `config.journal_dir` and `config.journal_snapshot_every` are the WAL rung's only sources: the
/// directory turns the journal on with the row's cadence, and no directory row is journaling OFF.
#[test]
fn the_journal_rung_is_the_two_rows() {
    let rung = crate::profile_rows::JournalRung {
        dir: Some(std::path::PathBuf::from("/srv/wal")),
        snapshot_every: Some(64),
    };
    let journal = crate::profile_rows::journal_config_for(None, &rung).expect("a directory row");
    assert_eq!(journal.dir, std::path::PathBuf::from("/srv/wal"));
    assert_eq!(journal.snapshot_every, 64);

    let default_cadence = crate::profile_rows::JournalRung { snapshot_every: None, ..rung };
    let journal = crate::profile_rows::journal_config_for(None, &default_cadence).expect("a dir");
    assert_eq!(
        journal.snapshot_every,
        vike_core::JournalConfig::at("/srv/wal").snapshot_every,
        "no cadence row keeps the default"
    );

    let off = crate::profile_rows::JournalRung { dir: None, snapshot_every: Some(64) };
    assert!(crate::profile_rows::journal_config_for(None, &off).is_none(), "no dir row = off");
}
