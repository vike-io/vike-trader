//! The precedence suite: the environment beats the file, as a test.

use super::*;

// ---------------------------------------------------------------------------------------------
// THE PRECEDENCE SUITE — "the environment beats the file", as a test rather than a doc comment
//
// Every key the unread-settings sweep wired is read by a library several frames below this
// binary, out of a map this binary FILLS. So "which source wins" stopped being a property of
// one `env::var` call and became a property of a fold — and a fold is exactly the kind of thing
// a doc comment can claim and the code can contradict. `docs/decisions/0054` states the
// constraint (one image, many containers, configured by `Environment=` lines); the CI box's live
// reconcile policy and balance-seed flag sit in a root-owned systemd drop-in, and a file value
// that beat those would silently change how a live trading daemon folds position divergences.
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
    env: &HashMap<String, String>,
) -> vike_config::Settings {
    vike_config::load_with_source(
        Some(dir.path()),
        vike_config::StoreLayer::Rows { rows, adopted: None },
        env,
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

/// **THE BLOCKER'S REGRESSION TEST, and the MAJOR'S, table-driven over every folded key.**
///
/// For each key: its `flags` row says `true`, the process environment says `"0"`, and the
/// credential store — the third source nobody modelled — says `"1"`. The environment must win.
///
/// It drives the REAL `vike_config::load`, so the file-then-env half is the shipped resolution
/// and not a re-implementation, and then the REAL fold. Every key is `FoldTier::Resolved`: the
/// value is OVERWRITTEN, so the map carries `"0"` and the reader downstream sees the
/// environment's answer — including for `flags.preflight_skip`, a SAFETY OVERRIDE, where a store
/// line standing would be widening one. (It asserted a second `FoldTier` variant,
/// `CredentialStoreFirst` — the fold mode that left a credential line standing — for the two
/// Polymarket gates until decision 0095's review closed it.) A flag whose variable decision
/// 0095 retired has no environment layer and is skipped below —
/// `a_retired_flag_variable_is_refused_not_folded` and
/// `no_credential_store_line_survives_the_fold_for_any_key` cover those.
#[test]
fn a_process_env_value_beats_a_file_value_for_every_wired_key() {
    for (var, _, FoldTier::Resolved) in folded_flag_rows(vike_config::Flags::default()) {
        // Decision 0095: a flag whose variable was RETIRED has no environment value to lose to — the
        // variable refuses startup instead; `a_retired_flag_variable_is_refused_not_folded` pins it.
        if !vike_config::flags::FLAG_REGISTRY.iter().any(|m| m.env == var && m.reads_env()) {
            continue;
        }
        let key = flags_key_for(var);
        let (dir, rows) = settings_dir_with_flag(key, true);
        let env = HashMap::from([(var.to_string(), "0".to_string())]);
        let settings = load_flag_row(&dir, &rows, &env);

        // Layer one: the environment beat the row inside `vike_config::load_with_source`.
        assert!(!resolved_row(settings.flags, var), "{var}=0 must beat a `{key} = true` row");

        // Layer two: the fold did not hand it back. `vars` carries the hostile credential line.
        let mut vars = HashMap::from([(var.to_string(), "1".to_string())]);
        fold_flags_into_vars(settings.flags, &mut vars);
        assert_eq!(
            vars.get(var).map(String::as_str),
            Some("0"),
            "{var}: an exported `0` must survive the fold — a `1` here is a credential file \
             outranking the environment on a key whose reader never had that tier"
        );
    }
}

/// Decision 0095: a folded flag whose variable was RETIRED has NO environment layer — a set
/// variable refuses startup instead of folding, and the row alone reaches the map. Derived from the
/// fold's own table and `FlagMeta::reads_env`, so a flag retired later is covered the day it is.
#[test]
fn a_retired_flag_variable_is_refused_not_folded() {
    let retired: Vec<&str> = folded_flag_rows(vike_config::Flags::default())
        .into_iter()
        .map(|(var, _, _)| var)
        .filter(|var| {
            !vike_config::flags::FLAG_REGISTRY.iter().any(|m| m.env == *var && m.reads_env())
        })
        .collect();
    // Anti-vacuity: the Polymarket gates were the first retired, so an empty set means the
    // derivation broke, not that nothing is retired.
    assert!(retired.contains(&vike_config::flags::POLY_EXEC_ENV), "{retired:?}");
    for var in retired {
        let env = HashMap::from([(var.to_string(), "1".to_string())]);
        assert!(vike_config::refuse_removed_env(&env).is_err(), "{var}=1 must refuse startup");
        let key = flags_key_for(var);
        let (dir, rows) = settings_dir_with_flag(key, false);
        let settings = load_flag_row(&dir, &rows, &env);
        assert!(!resolved_row(settings.flags, var), "{var}=1 must not arm `{key}`");
    }
}

/// The wiring still WORKS — the half a "make the environment win" change could break by
/// over-correcting. Nothing exported, its `flags` row says `true`, and every folded key reaches
/// the map as `"1"`.
#[test]
fn a_file_value_reaches_the_map_when_the_environment_is_silent() {
    for (var, _, _) in folded_flag_rows(vike_config::Flags::default()) {
        let key = flags_key_for(var);
        let (dir, rows) = settings_dir_with_flag(key, true);
        let settings = load_flag_row(&dir, &rows, &HashMap::new());
        let mut vars = HashMap::new();
        fold_flags_into_vars(settings.flags, &mut vars);
        assert_eq!(
            vars.get(var).map(String::as_str),
            Some("1"),
            "{var}: a `{key} = true` row must reach the map — that IS the wiring"
        );
    }
}

/// The one SAFETY OVERRIDE that keeps an environment layer — `flags.preflight_skip` — carried to
/// the map: exported `=0` plus a `true` row must resolve to REFUSE. (The withdraw override lost its
/// environment layer to decision 0095; `the_withdraw_override_is_the_row_alone` is its test.)
///
/// It asserts the exact string, because `vike_mount::startup` lives in a crate this one does not
/// depend on directly — that side of the chain is proved in
/// `crates/vike-mount/src/preflight_skip_precedence_tests.rs`'s
/// `the_preflight_is_not_skipped_when_the_environment_says_zero`, which takes this `"0"` as
/// its input.
#[test]
fn a_safety_override_refuses_when_the_environment_says_zero_and_the_file_says_true() {
    let var = vike_config::flags::PREFLIGHT_SKIP_ENV;
    let key = flags_key_for(var);
    let (dir, rows) = settings_dir_with_flag(key, true);
    let env = HashMap::from([(var.to_string(), "0".to_string())]);
    let settings = load_flag_row(&dir, &rows, &env);
    // The hostile credential line again: this is the shape that armed a live-money gate.
    let mut vars = HashMap::from([(var.to_string(), "1".to_string())]);
    fold_flags_into_vars(settings.flags, &mut vars);
    assert_eq!(
        vars.get(var).map(String::as_str),
        Some("0"),
        "{var} must be disarmed in the map the reader consults"
    );
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
        let settings = load_flag_row(&dir, &rows, &exported);
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

/// The reconcile family's own precedence, which is a property of the BASE MAP rather than of a
/// tier: `daemon_recon_env_from` starts from the process env, so an exported value is already
/// present and the `or_insert` cannot replace it.
#[test]
fn the_process_env_beats_the_resolved_flag_in_the_reconcile_family() {
    let on = vike_config::Flags {
        reconcile_balance: true,
        reconcile_generate_missing: true,
        ..vike_config::Flags::default()
    };
    for var in [
        vike_config::flags::RECONCILE_BALANCE_ENV,
        vike_config::flags::RECONCILE_GENERATE_MISSING_ENV,
    ] {
        // The unit file says `0` and the resolved flag says `true` — which cannot actually
        // happen through `vike_config::load` (the env layer would have made it false), and is
        // asserted anyway: this function must not be the place that could re-widen it.
        let exported = HashMap::from([(var.to_string(), "0".to_string())]);
        let env = daemon_recon_env_from(on, &vike_config::Config::default(), exported);
        assert_eq!(
            env.get(var).map(String::as_str),
            Some("0"),
            "{var}: an `Environment=` line in a systemd drop-in must survive the fold"
        );
        // ...and with nothing exported, the file's `true` reaches the family's one map.
        let from_file = daemon_recon_env_from(on, &vike_config::Config::default(), HashMap::new());
        assert_eq!(from_file.get(var).map(String::as_str), Some("1"), "{var}: the wiring works");
    }
}

/// The seven `config.reconcile_*` rows (decision 0111, phase P3): each lands under its variable
/// only where the process env has none, so a `reconcile.conf` line still wins, and each lands in
/// the variable's own spelling, so `build_recon_config` reads the row exactly as it reads the
/// variable.
#[test]
fn the_process_env_beats_every_reconcile_row_and_the_row_fills_its_absence() {
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
    let flags = vike_config::Flags::default();
    let rows = reconcile_rows(&config);
    assert!(rows.iter().all(|(_, v)| v.is_some()), "every row was written: {rows:?}");
    for (var, row) in rows {
        let exported = HashMap::from([(var.to_string(), "from-the-unit".to_string())]);
        let env = daemon_recon_env_from(flags, &config, exported);
        assert_eq!(env.get(var).map(String::as_str), Some("from-the-unit"), "{var}: env wins");
        let env = daemon_recon_env_from(flags, &config, HashMap::new());
        assert_eq!(env.get(var), row.as_ref(), "{var}: the row fills the variable's absence");
    }
    // ...and the family's one map parses them as the variables they stand in for.
    let cfg = reconcile_config::build_recon_config(
        &daemon_recon_env_from(flags, &config, HashMap::new()),
        HashMap::new(),
    );
    assert_eq!(cfg.interval, Some(std::time::Duration::from_millis(30_000)));
    assert_eq!(cfg.audit_interval, None, "a `0` row is the reader's own 'no audit' spelling");
    assert_eq!(cfg.lookback_ms, 7_200_000);
    assert_eq!(cfg.startup_delay, std::time::Duration::from_millis(500));
    assert_eq!(cfg.balance_tol.abs_floor, 2.5);
    assert_eq!(cfg.balance_tol.rel_frac, 0.001);
}

/// The policy ROW lands BEFORE the quarantine-first default, so the default fills only a policy
/// neither the environment nor the row named — and with no row the default is still `quarantine`.
#[test]
fn a_policy_row_lands_before_the_quarantine_first_default() {
    let flags = vike_config::Flags::default();
    let hybrid = vike_config::Config {
        reconcile_policy: Some("hybrid".to_string()),
        ..vike_config::Config::default()
    };
    let env = daemon_recon_env_from(flags, &hybrid, HashMap::new());
    assert_eq!(env.get("VIKE_RECONCILE_POLICY").map(String::as_str), Some("hybrid"));
    let env = daemon_recon_env_from(flags, &vike_config::Config::default(), HashMap::new());
    assert_eq!(env.get("VIKE_RECONCILE_POLICY").map(String::as_str), Some("quarantine"));
}

/// `config.tradehub_control_rate` (decision 0111, phase P3): the variable when it is SET — even to
/// a value the resolver then rejects, which was always its answer — else the row, in the
/// variable's spelling.
#[test]
fn the_control_rate_variable_beats_the_row() {
    use settings::control_rate_input;
    assert_eq!(control_rate_input(Some("7".into()), Some(3.0)).as_deref(), Some("7"));
    assert_eq!(control_rate_input(Some("garbage".into()), Some(3.0)).as_deref(), Some("garbage"));
    assert_eq!(control_rate_input(None, Some(3.0)).as_deref(), Some("3"));
    assert_eq!(control_rate_input(None, Some(0.5)).as_deref(), Some("0.5"));
    assert_eq!(control_rate_input(None, None), None);
    let limits = server::control::ControlLimitsConfig::from_policy(
        None,
        control_rate_input(None, Some(3.0)).as_deref(),
    );
    assert_eq!(limits.rate_per_sec, 3.0, "the resolver reads the row like the variable");
}

/// `config.journal_snapshot_every` rides `config.journal_dir`'s fold: the variable wins, the row
/// fills its absence.
#[test]
fn the_process_env_beats_config_journal_snapshot_every() {
    let exported = HashMap::from([("VIKE_JOURNAL_SNAPSHOT_EVERY".to_string(), "64".to_string())]);
    let vars = journal_vars_from(None, Some(2048), exported);
    assert_eq!(vars.get("VIKE_JOURNAL_SNAPSHOT_EVERY").map(String::as_str), Some("64"));
    let vars = journal_vars_from(None, Some(2048), HashMap::new());
    assert_eq!(vars.get("VIKE_JOURNAL_SNAPSHOT_EVERY").map(String::as_str), Some("2048"));
    let vars = journal_vars_from(None, None, HashMap::new());
    assert!(!vars.contains_key("VIKE_JOURNAL_SNAPSHOT_EVERY"));
}

/// `config.journal_dir`, the one wired key that is not a flag: an exported `VIKE_JOURNAL_DIR`
/// beats `config.journal_dir`, and the setting's value lands only where the variable is absent.
#[test]
fn the_process_env_beats_config_journal_dir() {
    let from_file = std::path::Path::new("/srv/from-config-toml");
    let exported = HashMap::from([(
        vike_config::config::JOURNAL_DIR_ENV.to_string(),
        "/srv/from-the-unit-file".to_string(),
    )]);
    let vars = journal_vars_from(Some(from_file), None, exported);
    assert_eq!(
        vars.get(vike_config::config::JOURNAL_DIR_ENV).map(String::as_str),
        Some("/srv/from-the-unit-file"),
        "an `Environment=VIKE_JOURNAL_DIR=...` line beats config.journal_dir"
    );
    // ...and the file reaches an otherwise-silent environment, which is the wiring itself.
    let vars = journal_vars_from(Some(from_file), None, HashMap::new());
    assert_eq!(
        vars.get(vike_config::config::JOURNAL_DIR_ENV).map(String::as_str),
        Some("/srv/from-config-toml")
    );
    // No key at all when neither source answers — `journal_config_from` then returns `None`,
    // the byte-identical WAL-off default.
    let vars = journal_vars_from(None, None, HashMap::new());
    assert!(!vars.contains_key(vike_config::config::JOURNAL_DIR_ENV));
}
