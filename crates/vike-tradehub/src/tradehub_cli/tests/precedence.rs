//! The rows suite: each settings row this binary reads reaches the live mount it configures, and
//! nothing but the row decides it (decision 0111: every one of these keys is a row, and its retired
//! variable refuses startup).

use super::*;

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

/// The reconcile family is the ROWS and nothing else (decision 0111): `daemon_reconcile_settings`
/// carries each `config.reconcile_*` row and both `flags.reconcile_*` switches through unchanged,
/// and `build_reconcile_config` reads them as the knobs they are.
#[test]
fn the_reconcile_settings_are_the_rows() {
    let config = vike_config::Config {
        reconcile_policy: Some(vike_model::ReconcilePolicyName::ExternalQuarantine),
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
    let recon = daemon_reconcile_settings(flags, &config);
    assert_eq!(
        recon,
        reconcile_config::ReconcileSettings {
            policy: Some(vike_model::ReconcilePolicyName::ExternalQuarantine),
            interval_ms: Some(30_000),
            audit_ms: Some(0),
            lookback_ms: Some(7_200_000),
            startup_delay_ms: Some(500),
            balance_tolerance_abs: Some(2.5),
            balance_tolerance_rel: Some(0.001),
            generate_missing: true,
            balance: true,
        }
    );
    let config_value = reconcile_config::build_reconcile_config(&recon, HashMap::new());
    assert_eq!(config_value.interval, Some(std::time::Duration::from_millis(30_000)));
    assert_eq!(
        config_value.audit_interval, None,
        "a `0` row is the reader's own 'no audit' spelling"
    );
    assert_eq!(config_value.lookback_ms, 7_200_000);
    assert_eq!(config_value.startup_delay, std::time::Duration::from_millis(500));
    assert_eq!(config_value.balance_tol.abs_floor, 2.5);
    assert_eq!(config_value.balance_tol.rel_frac, 0.001);
}

/// No reconcile rows at all is the all-default value — and its policy is `quarantine`, the
/// quarantine-first default [`reconcile_config::parse_policy`] owns.
#[test]
fn no_reconcile_rows_are_the_quarantine_first_defaults() {
    let recon =
        daemon_reconcile_settings(vike_config::Flags::default(), &vike_config::Config::default());
    assert_eq!(recon, reconcile_config::ReconcileSettings::default());
    // `ReconcilePolicy` has no `PartialEq`; its `Debug` is the whole value.
    assert_eq!(
        format!("{:?}", reconcile_config::parse_policy(recon.policy)),
        format!(
            "{:?}",
            reconcile_config::parse_policy(Some(vike_model::ReconcilePolicyName::Quarantine))
        ),
    );
}

/// Does the `vike_core::CoreConfig { .. }` literal in `src` (the live mount's) assign
/// `restore_orders_off` from the resolved `flags.reconcile_restore_off`? Whitespace-insensitive, so
/// a rustfmt reflow cannot redden it; anything else (a literal `false`, another flag, the line
/// deleted or moved out of the literal) fails.
fn live_core_literal_restores_from_the_row(src: &str) -> bool {
    let canon = |s: &str| s.chars().filter(|c| !c.is_whitespace()).collect::<String>();
    let Some(start) = src.find("core_config: vike_core::CoreConfig {") else { return false };
    let Some(len) = src[start..].find("..vike_core::CoreConfig::default()") else { return false };
    canon(&src[start..start + len]).contains("restore_orders_off:flags.reconcile_restore_off,")
}

/// **`flags.reconcile_restore_off` is the order-restore refusal's only source, and it reaches the
/// live core's `CoreConfig::restore_orders_off`.** Three halves, because the live `CoreConfig` is
/// built inside `live_runtime_with` (which spawns the core), so no test can read the built value back:
///
/// 1. the loader resolves the ROW — absent is `false` (restore ON, the default), a `true` row is
///    `true`, and the variable that never carried it refuses startup naming the row;
/// 2. the live mount's `CoreConfig` literal assigns `restore_orders_off` from that resolved flag
///    (a text pin over the real source — the `CONSUMPTION` row's needle only proves the line exists
///    somewhere in the file, this proves it sits in the literal and reads the right flag);
/// 3. the pin can fail: it rejects a literal `false`, another flag, and a line outside the literal.
#[test]
fn the_restore_refusal_is_the_row_and_reaches_the_live_core_config() {
    // 1 — the row, absent and written.
    let bare = vike_config::load(None).unwrap();
    assert!(!bare.flags.reconcile_restore_off, "unset ⇒ restore ON — `false` is the default");
    let (dir, rows) = settings_dir_with_flag("reconcile_restore_off", true);
    assert!(load_flag_row(&dir, &rows).flags.reconcile_restore_off, "a true row refuses restore");
    let (dir, rows) = settings_dir_with_flag("reconcile_restore_off", false);
    assert!(!load_flag_row(&dir, &rows).flags.reconcile_restore_off, "a false row restores");
    let env = HashMap::from([(
        vike_config::flags::RECONCILE_RESTORE_OFF_ENV.to_string(),
        "1".to_string(),
    )]);
    let refusal = vike_config::refuse_removed_env(&env).expect_err("the variable refuses startup");
    assert!(
        refusal.contains("flags.reconcile_restore_off"),
        "the refusal names the row: {refusal}"
    );

    // 2 — the live mount's literal reads it.
    const LIVE_RUNTIME: &str = include_str!("../live_runtime.rs");
    assert!(
        live_core_literal_restores_from_the_row(LIVE_RUNTIME),
        "`live_runtime_with`'s `CoreConfig` literal must set `restore_orders_off: \
         flags.reconcile_restore_off` — without it the operator's off switch is a row nothing reads"
    );

    // 3 — the pin can fail.
    let literal = |body: &str| {
        format!(
            "core_config: vike_core::CoreConfig {{ seed_cash: 1.0, {body} ..vike_core::CoreConfig::default() }}"
        )
    };
    assert!(live_core_literal_restores_from_the_row(&literal(
        "restore_orders_off: flags.reconcile_restore_off,"
    )));
    for wrong in ["restore_orders_off: false,", "restore_orders_off: flags.reconcile_off,", ""] {
        assert!(!live_core_literal_restores_from_the_row(&literal(wrong)), "{wrong:?}");
    }
    let outside = format!(
        "{} let restore_orders_off = flags.reconcile_restore_off; restore_orders_off: flags.reconcile_restore_off,",
        literal("")
    );
    assert!(!live_core_literal_restores_from_the_row(&outside), "outside the literal is not it");
}

/// Does the `vike_core::CoreConfig { .. }` literal in `src` (the live mount's) assign
/// `restore_venue_gate` through `reconcile_config::restore_venue_gate` over the reconcile VERDICT
/// binding, `reconcile_gate`? Whitespace-insensitive, like its sibling above.
fn live_core_literal_gates_restore_on_the_verdict(src: &str) -> bool {
    let canon = |s: &str| s.chars().filter(|c| !c.is_whitespace()).collect::<String>();
    let Some(start) = src.find("core_config: vike_core::CoreConfig {") else { return false };
    let Some(len) = src[start..].find("..vike_core::CoreConfig::default()") else { return false };
    canon(&src[start..start + len])
        .contains("restore_venue_gate:reconcile_config::restore_venue_gate(reconcile_gate),")
}

/// **The order-restore venue gate follows the reconcile verdict, and it does so in the live core's
/// `CoreConfig`.** The live `CoreConfig` is built inside `live_runtime_with` (which spawns the core),
/// so no test can read the built value back; the pure half (`restore_venue_gate` closes the gate
/// for a refusal and for nothing else) is `reconcile_config_tests.rs`'s, and this is the wiring
/// half, as a text pin over the real source. Reverting the line leaves a box that refused reconcile
/// with the declared table's gate again: the core mutes the strategy's tagged resubmit for a pass
/// that cannot come, and every other test stays green. The pin can fail: it rejects the table
/// itself, a literal verdict, no line, and a line outside the literal.
#[test]
fn the_restore_gate_follows_the_reconcile_verdict_in_the_live_core_config() {
    const LIVE_RUNTIME: &str = include_str!("../live_runtime.rs");
    assert!(
        live_core_literal_gates_restore_on_the_verdict(LIVE_RUNTIME),
        "`live_runtime_with`'s `CoreConfig` literal must set `restore_venue_gate: \
         reconcile_config::restore_venue_gate(reconcile_gate)` — without it a refused reconcile keeps \
         the declared table's gate and the core mutes a strategy for a pass that cannot come"
    );

    let literal = |body: &str| {
        format!(
            "core_config: vike_core::CoreConfig {{ seed_cash: 1.0, {body} ..vike_core::CoreConfig::default() }}"
        )
    };
    assert!(live_core_literal_gates_restore_on_the_verdict(&literal(
        "restore_venue_gate: reconcile_config::restore_venue_gate(reconcile_gate),"
    )));
    for wrong in [
        "",
        "restore_venue_gate: vike_venues::venue_restore::restores_orders,",
        "restore_venue_gate: reconcile_config::restore_venue_gate(ReconcileGate::LiveDefault),",
    ] {
        assert!(!live_core_literal_gates_restore_on_the_verdict(&literal(wrong)), "{wrong:?}");
    }
    let outside = format!(
        "{} let g = reconcile_config::restore_venue_gate(reconcile_gate); restore_venue_gate: reconcile_config::restore_venue_gate(reconcile_gate),",
        literal("")
    );
    assert!(
        !live_core_literal_gates_restore_on_the_verdict(&outside),
        "outside the literal is not it"
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

// ---------------------------------------------------------------------------------------------
// THE TRADING-MOUNT BATCH — `MountConfigRows`, the `config.*` rows the mounts read
// ---------------------------------------------------------------------------------------------

/// Does `src`, from the first `from` to the next `until` after it, assign `body`? Whitespace-
/// insensitive, so a rustfmt reflow cannot redden it; a different value, the line deleted, or the
/// line moved out of the literal fails. The generalisation of
/// [`live_core_literal_restores_from_the_row`] to any literal the live mount builds: the literals
/// sit inside `live_runtime_with`, which spawns a core, so no test can read the built value back.
fn literal_assigns(src: &str, from: &str, until: &str, body: &str) -> bool {
    let canon = |s: &str| s.chars().filter(|c| !c.is_whitespace()).collect::<String>();
    let Some(start) = src.find(from) else { return false };
    let Some(len) = src[start..].find(until) else { return false };
    canon(&src[start..start + len]).contains(&canon(body))
}

/// **A box without the rows mounts exactly as it did before them**, and a box with them hands the
/// mounts exactly those values. `MountConfigRows::from_config` is the only reader of the rows
/// (`CONSUMPTION` names its lines), `MountConfigRows::default` is "no rows", and the two agree.
#[test]
fn the_mount_rows_are_the_config_rows_and_none_is_today() {
    assert_eq!(
        MountConfigRows::from_config(&vike_config::Config::default()),
        MountConfigRows::default(),
        "no rows resolves to the default value — the mounts' own fallbacks"
    );
    assert_eq!(
        MountConfigRows::default().paper_slippage,
        None,
        "no row keeps the compiled slippage"
    );
    assert_eq!(
        MountConfigRows::default().preflight_disk,
        vike_tradehub_core::preflight::PreflightDiskThresholds::default(),
        "no rows keep the compiled disk floors"
    );

    assert_eq!(
        MountConfigRows::default().inflight_confirm,
        None,
        "no row leaves the fast rung off"
    );
    assert_eq!(MountConfigRows::default().state_save, None, "no row leaves only the shutdown save");

    let config = vike_config::Config {
        paper_slippage: Some(0.001),
        preflight_disk_warn_mib: Some(8_192),
        preflight_disk_fail_mib: Some(2_048),
        inflight_confirm_ms: Some(2_000),
        strategy_state_save_ms: Some(60_000),
        ..vike_config::Config::default()
    };
    assert_eq!(
        MountConfigRows::from_config(&config),
        MountConfigRows {
            paper_slippage: Some(0.001),
            paper_slippage_table: None,
            preflight_disk: vike_tradehub_core::preflight::PreflightDiskThresholds::from_rows(
                Some(8_192),
                Some(2_048)
            ),
            inflight_confirm: Some(Duration::from_millis(2_000)),
            state_save: Some(Duration::from_millis(60_000)),
        }
    );

    // One disk row alone moves that floor and leaves the other compiled.
    let warn_only = vike_config::Config {
        preflight_disk_warn_mib: Some(8_192),
        ..vike_config::Config::default()
    };
    let compiled = vike_tradehub_core::preflight::PreflightDiskThresholds::default();
    let rows = MountConfigRows::from_config(&warn_only);
    assert_eq!(rows.preflight_disk.fail_bytes, compiled.fail_bytes);
    assert_eq!(rows.preflight_disk.warn_bytes, 8_192 * 1024 * 1024);

    // The table row is read as the table it states, through the reading the loader judged it by.
    let table = vike_config::Config {
        paper_slippage_table: Some(
            "model = \"market_impact\"\nbase = 0.0002\nwindow = 60".to_string(),
        ),
        ..vike_config::Config::default()
    };
    let read = MountConfigRows::from_config(&table).paper_slippage_table.expect("the table");
    assert_eq!(
        (read.model.as_str(), read.base, read.window),
        ("market_impact", Some(0.0002), Some(60))
    );
    assert_eq!(MountConfigRows::default().paper_slippage_table, None, "no row is the flat figure");
}

/// **`config.paper_slippage` and the two `config.preflight_disk_*_mib` rows reach the live mount's
/// `AdapterPolicy`.** The policy is built inside `live_runtime_with`, so this is a text pin over the
/// real source: the `AdapterPolicy` literal sets each field from the `rows` parameter. It can fail: a
/// literal `None` or default, another field, and a line outside the literal are all refused.
#[test]
fn the_paper_and_disk_rows_reach_the_live_mounts_policy() {
    assert_the_live_literal_sets_each_row(
        "let mut adapter_policy = vike_tradehub_core::AdapterPolicy {",
        "..vike_tradehub_core::AdapterPolicy::from(policy)",
        ("AdapterPolicy", "mount"),
        ("accounts,", "};"),
        [
            (
                "paper_slippage: rows.paper_slippage,",
                ["paper_slippage: None,", "paper_slippage: 0.0002,"],
            ),
            (
                "paper_slippage_table: rows.paper_slippage_table.clone(),",
                ["paper_slippage_table: None,", "paper_slippage_table: Default::default(),"],
            ),
            (
                "preflight_disk: rows.preflight_disk,",
                [
                    "preflight_disk: Default::default(),",
                    "preflight_disk: vike_tradehub_core::preflight::PreflightDiskThresholds::default(),",
                ],
            ),
        ],
    );
}

/// **The `flags.preflight_skip` row reaches the live mount's `AdapterPolicy`**, the one place the core reads
/// the skip from (boot and every hot start). `AdapterPolicy::from(policy)` fills the field `false`, so a
/// literal that never sets it compiles and silently loses the operator's skip: the preflight still runs, the
/// safe side, but the row is a lie. This pin goes red when the line is absent, hard-coded, or outside the literal.
#[test]
fn the_preflight_skip_row_reaches_the_live_mounts_policy() {
    assert_the_live_literal_sets_each_row(
        "let mut adapter_policy = vike_tradehub_core::AdapterPolicy {",
        "..vike_tradehub_core::AdapterPolicy::from(policy)",
        ("AdapterPolicy", "mount"),
        ("accounts,", "};"),
        [(
            "preflight_skip: flags.preflight_skip,",
            ["preflight_skip: false,", "preflight_skip: true,"],
        )],
    );
}

/// The text pin the `live_runtime_with` literal tests share: the `name` literal (from `from` to
/// `until`) sets each row's `want`, or the row is read and dropped and the `keeper` keeps its default.
/// The pin can fail: over a synthetic literal (its other field `filler`, closed by `close`) each `wrong`
/// spelling, no spelling at all and the line outside the literal are refused.
fn assert_the_live_literal_sets_each_row<const N: usize>(
    from: &str,
    until: &str,
    (name, keeper): (&str, &str),
    (filler, close): (&str, &str),
    rows: [(&str, [&str; 2]); N],
) {
    const LIVE_RUNTIME: &str = include_str!("../live_runtime.rs");
    for (want, wrong) in rows {
        assert!(
            literal_assigns(LIVE_RUNTIME, from, until, want),
            "`live_runtime_with`'s `{name}` literal must set `{want}` — without it the row is \
             read and then dropped, and the {keeper} keeps its default"
        );
        let literal = |body: &str| format!("{from} {filler} {body} {until} {close}");
        assert!(literal_assigns(&literal(want), from, until, want));
        for body in wrong.into_iter().chain([""]) {
            assert!(!literal_assigns(&literal(body), from, until, want), "{body:?}");
        }
        let outside = format!("{} let _ = {want}", literal(""));
        assert!(!literal_assigns(&outside, from, until, want), "outside the literal is not it");
    }
}

/// **A cadence row is milliseconds, and `0` — like no row — is OFF.** A `Some(0)` handed to the
/// core as `Duration::ZERO` would arm a timer that is due the instant it is set, so every cadence
/// in [`MountConfigRows`] goes through this one conversion.
#[test]
fn a_cadence_row_is_milliseconds_and_zero_is_off() {
    assert_eq!(cadence_from_ms(None), None, "no row");
    assert_eq!(cadence_from_ms(Some(0)), None, "the row's own off word");
    assert_eq!(cadence_from_ms(Some(1_000)), Some(Duration::from_secs(1)));
    assert_eq!(cadence_from_ms(Some(u32::MAX)), Some(Duration::from_millis(u64::from(u32::MAX))));

    let off = vike_config::Config {
        inflight_confirm_ms: Some(0),
        strategy_state_save_ms: Some(0),
        ..vike_config::Config::default()
    };
    assert_eq!(
        MountConfigRows::from_config(&off),
        MountConfigRows::default(),
        "a `0` row is the default"
    );
}

/// **`config.inflight_confirm_ms` and `config.strategy_state_save_ms` reach the live core's
/// `CoreConfig`** (`inflight_confirm` and `state_save`) — a text pin over `live_runtime_with`'s
/// `CoreConfig` literal (it spawns a core, so no test reads the built value back), the twin of
/// [`the_restore_refusal_is_the_row_and_reaches_the_live_core_config`]'s. It can fail: a literal
/// `None`, a hard-coded cadence and a line outside the literal are refused.
#[test]
fn the_cadence_rows_reach_the_live_core_config() {
    assert_the_live_literal_sets_each_row(
        "core_config: vike_core::CoreConfig {",
        "..vike_core::CoreConfig::default()",
        ("CoreConfig", "core"),
        ("seed_cash: 1.0,", "}"),
        [
            (
                "inflight_confirm: rows.inflight_confirm,",
                ["inflight_confirm: None,", "inflight_confirm: Some(Duration::from_secs(2)),"],
            ),
            (
                "state_save: rows.state_save,",
                ["state_save: None,", "state_save: Some(Duration::from_secs(30)),"],
            ),
        ],
    );
}

/// **`config.strategy_state_save_ms` reaches the paper arm's `StrategyCorePaperConfig::state_save` too** —
/// the same cadence the live arm reads, so a rehearsal of a crash loses what the live node would.
/// A text pin over `start_paper`'s options literal in the spine (the builders it feeds spawn a
/// core); it can fail like its live twin above.
#[test]
fn the_state_save_row_reaches_the_paper_arms_options() {
    const SPINE: &str = include_str!("../../tradehub_cli.rs");
    const FROM: &str = "let opts = vike_tradehub_core::StrategyCorePaperConfig {";
    const UNTIL: &str = "..Default::default()";
    const WANT: &str = "state_save: MountConfigRows::from_config(&settings.config).state_save,";
    assert!(
        literal_assigns(SPINE, FROM, UNTIL, WANT),
        "`start_paper`'s `StrategyCorePaperConfig` literal must set `{WANT}` — without it the paper arm \
         keeps the clean-shutdown save only while the live arm saves on the row's cadence"
    );
    let literal = |body: &str| format!("{FROM} risk_limits, {body} {UNTIL} }};");
    assert!(literal_assigns(&literal(WANT), FROM, UNTIL, WANT));
    for wrong in ["state_save: None,", "state_save: Some(Duration::from_secs(30)),", ""] {
        assert!(!literal_assigns(&literal(wrong), FROM, UNTIL, WANT), "{wrong:?}");
    }
    let outside = format!("{} let _ = {WANT}", literal(""));
    assert!(!literal_assigns(&outside, FROM, UNTIL, WANT), "outside the literal is not it");
}

/// **A fast in-flight confirm at or above the effective `submit_ack_timeout` is warned about, and
/// nothing else is.** The band the rung acts in is `[inflight_confirm, submit_ack_timeout)`: a
/// cadence at the timeout leaves it empty. No row, no timeout (the ladder off — the band is open
/// ended), and a cadence below the timeout all say nothing.
#[test]
fn an_inflight_confirm_at_or_above_the_ack_timeout_is_warned_about() {
    let ms = |n: u64| Some(Duration::from_millis(n));
    let warn = live_runtime::inflight_confirm_dead_band;

    assert_eq!(warn(None, ms(30_000)), None, "no row says nothing");
    assert_eq!(warn(ms(2_000), None), None, "no timeout: the band is open-ended");
    assert_eq!(warn(None, None), None);
    assert_eq!(warn(ms(2_000), ms(30_000)), None, "below the timeout: the band is open");
    assert_eq!(warn(ms(29_999), ms(30_000)), None, "one millisecond below still acts");

    for inflight in [30_000, 30_001, 60_000] {
        let why = warn(ms(inflight), ms(30_000)).unwrap_or_else(|| panic!("{inflight} must warn"));
        assert!(why.contains("config.inflight_confirm_ms"), "names the row: {why}");
        assert!(why.contains(&format!("is {inflight} ms")), "states the cadence: {why}");
        assert!(why.contains("30000 ms"), "states the timeout it met: {why}");
        assert!(why.contains("NEVER act"), "says what it means: {why}");
        assert!(
            why.contains("config set config.inflight_confirm_ms 0"),
            "names the off word: {why}"
        );
    }
    // A profile that raised the timeout opens the band again.
    assert_eq!(warn(ms(60_000), ms(120_000)), None);
}
