use super::*;
use clap::Parser;

/// A representative program: valued flags, a switch and a defaulted number — the shape the bins of
/// this crate have (none takes a positional since the one-shot kline programs went, decision 0094).
#[derive(Parser, Debug, PartialEq)]
struct Flaggy {
    #[arg(long, required = true)]
    from: String,
    #[arg(long, required = true)]
    to: String,
    #[arg(long)]
    store: Option<String>,
    #[arg(long)]
    dry_run: bool,
    #[arg(long, default_value_t = 6)]
    concurrency: usize,
}

fn run(args: &[&str]) -> Result<Flaggy, CliExit> {
    try_parse_from("demo_backfill", args.iter().copied())
}

fn refused(args: &[&str]) -> CliExit {
    run(args).expect_err("this command line must be refused")
}

/// The two outcomes that are NOT a run, in both spellings each. These were the whole defect:
/// every bin in this crate answered `--help` with a usage error, a panic, or an ingest.
#[test]
fn help_and_version_are_outcomes_not_errors() {
    for flag in ["-h", "--help"] {
        let exit = refused(&[flag]);
        assert_eq!((exit.code, exit.to_stdout), (0, true), "{flag}");
        assert!(exit.text.contains("demo_backfill"), "the usage names the bin: {}", exit.text);
    }
    for flag in ["-V", "--version"] {
        let exit = refused(&[flag]);
        assert_eq!((exit.code, exit.to_stdout), (0, true), "{flag}");
        assert_eq!(exit.text, format!("demo_backfill {}\n", env!("CARGO_PKG_VERSION")), "{flag}");
    }
    // …and asked for AFTER other arguments, which is how a person actually reaches for it
    // ("what were the other flags again?"), and before the required flags are satisfied.
    let exit = refused(&["--from", "x", "--help"]);
    assert_eq!((exit.code, exit.to_stdout), (0, true));
}

/// A valid invocation is still a run — the property that keeps "exit 0 on --help" from being
/// bought by making everything exit 0, and that keeps this reader out of the way of real work.
#[test]
fn a_valid_invocation_is_a_run() {
    let ok = run(&["--from", "2026-01-01", "--to", "2026-01-02", "--dry-run"]).expect("a run");
    assert_eq!(ok.from, "2026-01-01");
    assert!(ok.dry_run);
    assert_eq!(ok.store, None);
    assert_eq!(ok.concurrency, 6, "an unmentioned optional flag keeps its default");
}

/// The negative half. A typo used to resolve to the DEFAULT and write to the wrong store in
/// silence (`arg` was a one-flag lookup blind to every token it was not asked about).
#[test]
fn an_unknown_flag_is_rejected_rather_than_ignored() {
    let exit = refused(&["--from", "a", "--to", "b", "--stroe", "/data"]);
    assert_eq!((exit.code, exit.to_stdout), (2, false));
    assert!(exit.text.contains("--stroe"), "the error names the offending argument: {}", exit.text);
    assert!(exit.text.starts_with("demo_backfill: "), "{}", exit.text);
    assert!(exit.text.contains("--store"), "and suggests the flag it meant: {}", exit.text);
    // A lowercase `-v` is verbosity everywhere else on the box, so it stays unknown here.
    assert_eq!(refused(&["--from", "a", "--to", "b", "-v"]).code, 2, "-v must stay unknown");
}

/// A valued flag consumes the token after it, so a store PATH is never counted as a stray
/// positional — and a trailing valued flag with nothing after it is a usage error, not a silent
/// default.
#[test]
fn valued_flags_consume_their_value_and_a_trailing_one_is_an_error() {
    let ok = run(&["--from", "a", "--to", "b", "--store", "/x/y"]).expect("a run");
    assert_eq!(ok.store.as_deref(), Some("/x/y"));
    let exit = refused(&["--from", "a", "--to", "b", "--store"]);
    assert_eq!(exit.code, 2);
    assert!(exit.text.contains("--store"), "{}", exit.text);
}

/// **The swallow rule.** A valued flag may not eat a FLAG: `--store --dry-run` used to triage as a
/// clean run, after which `arg` answered `Some("--dry-run")` for the store root while `has_flag`
/// still reported the toggle as set — the backfill wrote into a directory named `--dry-run` and
/// nothing anywhere disagreed.
#[test]
fn a_valued_flag_may_not_swallow_a_following_flag() {
    for line in [
        &["--from", "a", "--to", "b", "--store", "--dry-run"][..],
        &["--from", "--to", "2026-01-01"][..],
        &["--from", "a", "--to", "b", "--store", "--"][..],
    ] {
        let exit = refused(line);
        assert_eq!(exit.code, 2, "{line:?}: {}", exit.text);
    }
    // A NEGATIVE NUMBER is a real value: it reaches the typed parser, which refuses it by name
    // (`usize`), and a negative value for a text flag is still a value.
    let ok = run(&["--from", "-1", "--to", "b"]).expect("a negative number is a value");
    assert_eq!(ok.from, "-1");
    let exit = refused(&["--from", "a", "--to", "b", "--concurrency", "-1"]);
    assert!(exit.text.contains("--concurrency"), "{}", exit.text);
}

/// A malformed number is an error naming the flag, never the default (`--concurrency abc` used to
/// fall back to 6 in silence).
#[test]
fn a_malformed_number_is_an_error_rather_than_the_default() {
    let exit = refused(&["--from", "a", "--to", "b", "--concurrency", "abc"]);
    assert_eq!(exit.code, 2);
    assert!(exit.text.contains("--concurrency"), "{}", exit.text);
}

/// A bin of this crate takes no bare token: a stray one is a usage error rather than being dropped
/// on the floor.
#[test]
fn a_bare_token_is_rejected() {
    let exit = refused(&["--from", "a", "--to", "b", "stray"]);
    assert_eq!(exit.code, 2);
}

/// A repeated flag is refused (`arg` answered the FIRST one and ignored the rest), and a blank value
/// is refused (the per-flag emptiness rule is global now).
#[test]
fn a_repeated_flag_and_a_blank_value_are_refused() {
    let twice = refused(&["--from", "a", "--from", "b", "--to", "c"]);
    assert_eq!(twice.code, 2);
    assert!(twice.text.contains("--from"), "{}", twice.text);
    let blank = refused(&["--from", "a", "--to", "b", "--store", " "]);
    assert_eq!(blank.code, 2);
    assert!(blank.text.contains("--store"), "{}", blank.text);
}

/// **Decision 0155 REVERSED the old refusal of `--flag=value`.** `CliSpec::triage` called every
/// inline token an unknown argument (`an_inline_spelling_never_survives_triage_so_arg_never_sees_one`
/// pinned it), so `--store=/x` was a usage error in this crate while `vike-tradehub` and `vike-cli`
/// accepted it. Now it is read exactly like `--store /x`, for a valued flag; a switch with a value is
/// still refused, and an unknown inline flag is still unknown.
#[test]
fn an_inline_spelling_is_read_exactly_like_the_spaced_one() {
    let spaced =
        run(&["--from", "2026-01-01", "--to", "2026-01-02", "--store", "/x", "--dry-run"]).unwrap();
    let inline = run(&["--from=2026-01-01", "--to=2026-01-02", "--store=/x", "--dry-run"]).unwrap();
    assert_eq!(inline, spaced);
    // `--store=/x=y`: only the FIRST `=` splits, the value keeps the rest.
    let value = run(&["--from=a", "--to=b", "--store=/x=y"]).unwrap();
    assert_eq!(value.store.as_deref(), Some("/x=y"));
    // A switch given a value, an unknown flag and a blank inline value are all refused.
    for bad in ["--dry-run=1", "--unknown=1", "--store="] {
        let exit = refused(&["--from=a", "--to=b", bad]);
        assert_eq!(exit.code, 2, "{bad}: {}", exit.text);
    }
}

/// The 341-GB-footgun regression guard: with no `preferences.log_file_level` row, every batch
/// bin's `LogConfig` keeps its file level at `warn`, not vike-log's own `trace` default — see
/// [`BATCH_FILE_LEVEL`]'s doc.
#[test]
fn with_no_row_the_file_level_is_warn_not_trace() {
    let cfg = batch_log_config("test-bin".to_string(), &vike_config::LogFileLevel::default());
    assert_eq!(cfg.file_level, "warn");
    assert_eq!(cfg.file_prefix, "test-bin");
    // Everything else stays vike-log's own default — this helper sets file_level and the log
    // DIRECTORY (below), nothing more.
    let default = vike_log::LogConfig::default();
    assert_eq!(cfg.console_level, default.console_level);
    assert_eq!(cfg.file_enabled, default.file_enabled);
    assert!(cfg.dir.is_none(), "the CONFIG layer stays unset — this helper names no override");
    assert!(cfg.project_dir.is_none(), "no project: vike-log's <exe_dir>/logs last resort");
}

/// …and the `preferences.log_file_level` row, when the project has one, IS the level: a debug run
/// writes `debug` there instead of exporting a variable.
#[test]
fn the_row_is_the_file_level_and_the_log_home_is_the_project_s() {
    let logs = PathBuf::from("/p/settings/state/logs");
    let read = vike_config::LogFileLevel {
        level: Some("debug".to_string()),
        log_home: Some(logs.clone()),
        unread: None,
    };
    let cfg = batch_log_config("test-bin".to_string(), &read);
    assert_eq!(cfg.file_level, "debug");
    assert_eq!(cfg.project_dir, Some(logs));
}

/// A store that could not answer keeps `warn` — the fall-back is the batch default, never the
/// loader's `trace`.
#[test]
fn an_unreadable_store_keeps_warn() {
    let read =
        vike_config::LogFileLevel { unread: Some("locked".to_string()), ..Default::default() };
    assert_eq!(batch_log_config("test-bin".to_string(), &read).file_level, "warn");
    let line = read.unread_line(BATCH_FILE_LEVEL).expect("the fall-back is said");
    assert!(line.contains("`warn`") && line.contains("locked"), "{line}");
}

fn env(pairs: &[(&str, &str)]) -> std::collections::HashMap<String, String> {
    pairs.iter().map(|(k, v)| ((*k).to_string(), (*v).to_string())).collect()
}

/// A `repo_default` that exists on no machine — the INSTALLED shape, and the only way a test
/// running inside the checkout can see any rung below the dev-checkout hinge.
const NO_CHECKOUT: &str = "/definitely/not/a/real/build/machine/path/market_data/hist";

#[test]
fn explicit_wins_and_no_variable_is_a_rung() {
    let repo = default_store_root();
    // explicit --store beats everything
    assert_eq!(
        resolve(Some("/x/explicit"), &repo, None, &env(&[])).root,
        PathBuf::from("/x/explicit")
    );
    // The retired `VIKE_HIST_STORE` moves nothing (decision 0111): with no --store the repo-root
    // default answers, because these tests RUN in the checkout and the dev-checkout hinge
    // outranks the project rung there. Composed so the registry's literal sweep reads no fixture.
    let retired = env(&[(concat!("VIKE", "_HIST_STORE"), "/y/env")]);
    assert_eq!(resolve(None, &repo, None, &retired).root, repo);
    assert_eq!(resolve(None, &repo, None, &env(&[])).root, repo);
}

/// **THIS crate's call site, on the rung a `cargo test` run cannot otherwise reach** — and the
/// transposition guard for it. `resolve` is the one place this crate assembles the shared
/// precedence, and the project and per-user rungs are made unmistakably different (a scratch
/// project vs a fake `$HOME`), so a wiring that swapped them reddens on the value rather than
/// on a path suffix. Verified by mutation: swapping the two inside
/// `vike_paths::store_path::resolve_store_root_from` turns this red with the `$HOME` path.
#[test]
fn the_call_site_reaches_the_project_rung_when_this_box_has_no_checkout() {
    use vike_paths::store_path::{HOME_VAR, LOCALAPPDATA_VAR, StoreRootRung, XDG_DATA_HOME_VAR};

    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let scratch = std::env::temp_dir().join(format!("vike-backfill-store-{nanos}"));
    let project = scratch.join("proj");
    let fake_home = scratch.join("home");
    std::fs::create_dir_all(project.join("settings")).unwrap();

    let vars = env(&[
        (HOME_VAR, fake_home.to_str().unwrap()),
        (XDG_DATA_HOME_VAR, fake_home.to_str().unwrap()),
        (LOCALAPPDATA_VAR, fake_home.to_str().unwrap()),
    ]);
    let got = resolve(None, Path::new(NO_CHECKOUT), Some(&project), &vars);
    let user = vike_paths::store_path::user_data_dir_from_vars(&vars).unwrap();
    let _ = std::fs::remove_dir_all(&scratch);

    assert_eq!(got.root, project.join("market_data").join("hist"), "the PROJECT's own data folder");
    assert_eq!(got.rung, StoreRootRung::Project);
    assert_ne!(got.root, user, "the two rungs must be distinguishable in this fixture");
}

/// …and with no project above the working directory, the SAME call site falls to the per-user
/// directory. Together with the test above this pins the ORDER, not just one answer.
#[test]
fn the_call_site_falls_to_the_user_dir_without_a_project() {
    use vike_paths::store_path::{HOME_VAR, LOCALAPPDATA_VAR, StoreRootRung, XDG_DATA_HOME_VAR};

    let vars = env(&[
        (HOME_VAR, "/home/u"),
        (XDG_DATA_HOME_VAR, "/xdg"),
        (LOCALAPPDATA_VAR, "C:\\Users\\u\\AppData\\Local"),
    ]);
    // `cwd = None` — a binary that could not read its own working directory. No project can be
    // resolved from it, so the per-user directory is the only remaining answer.
    let got = resolve(None, Path::new(NO_CHECKOUT), None, &vars);
    assert_eq!(got.root, vike_paths::store_path::user_data_dir_from_vars(&vars).unwrap());
    assert_eq!(got.rung, StoreRootRung::UserDir);
}

/// **B4 at this call site:** `$VIKE_SETTINGS_DIR` relocates the project, and the store's default
/// travels with it — out of the SAME map the bins already collect, so no bin can forget it.
#[test]
fn the_call_site_honours_the_settings_dir_override() {
    let vars = env(&[("VIKE_SETTINGS_DIR", "/relocated/settings")]);
    let got = resolve(None, Path::new(NO_CHECKOUT), Some(Path::new("/somewhere/else")), &vars);
    assert_eq!(got.root, PathBuf::from("/relocated").join("market_data").join("hist"));
}

/// A retired variable REFUSES rather than configuring nothing in silence, and names what replaced
/// it; a blank one is unset. Names composed so the registry's literal sweep reads no fixture here.
#[test]
fn a_retired_variable_is_refused_by_name() {
    assert_eq!(refuse_retired_variables(&env(&[])), Ok(()));
    let hist = concat!("VIKE", "_HIST_STORE");
    assert_eq!(refuse_retired_variables(&env(&[(hist, "  ")])), Ok(()), "blank is unset");
    let why = refuse_retired_variables(&env(&[(hist, "/y/env")])).expect_err("set is refused");
    assert!(why.contains(hist) && why.contains("--store DIR"), "{why}");
    assert!(!why.contains("/y/env"), "never the value: {why}");
    let base = concat!("VIKE", "_API_BASE");
    let why = refuse_retired_variables(&env(&[(base, "http://x")])).expect_err("set is refused");
    assert!(why.contains(base) && why.contains("--api-base URL"), "{why}");
    let level = concat!("VIKE", "_LOG_FILE_LEVEL");
    let why = refuse_retired_variables(&env(&[(level, "warn")])).expect_err("set is refused");
    assert!(why.contains(level) && why.contains("preferences.log_file_level"), "{why}");
}

/// The public entry point takes `--store` as its one stated answer.
#[test]
fn store_root_takes_the_explicit_store() {
    assert_eq!(store_root(Some("/x/explicit"), &env(&[])), PathBuf::from("/x/explicit"));
}

/// **Behaviour preservation across the map lift.** A dev checkout (this repo, whose root the
/// compile-time `default_store_root` names) resolves to `<repo>/market_data/hist` regardless of the
/// platform trio — the compatibility hinge in `vike_paths::store_path::resolve_store_root`.
/// That is the path every the CI box backfill takes today, asserted on a unix-shaped AND a
/// windows-shaped environment map.
#[test]
fn a_dev_checkout_is_unaffected_by_the_platform_trio_on_either_platform_shape() {
    // Spelled through `vike_paths::store_path`'s constants, not as bare literals: that crate is
    // the ONE place these three variable names appear, and a fixture that re-spelled them here
    // would put an incidental `vike-backfill` row back on the settings registry for a read this
    // crate no longer performs.
    use vike_paths::store_path::{HOME_VAR, LOCALAPPDATA_VAR, XDG_DATA_HOME_VAR};
    let unix = env(&[(XDG_DATA_HOME_VAR, "/xdg"), (HOME_VAR, "/home/u")]);
    let windows =
        env(&[(LOCALAPPDATA_VAR, "C:\\Users\\u\\AppData\\Local"), (HOME_VAR, "C:\\Users\\u")]);
    assert_eq!(store_root(None, &unix), default_store_root());
    assert_eq!(store_root(None, &windows), default_store_root());
}

#[test]
fn parse_symbol_pairs_maps_and_defaults_bare() {
    assert_eq!(
        parse_symbol_pairs("SPX=^GSPC, VIX=^VIX ,ETH").unwrap(),
        vec![
            ("SPX".to_string(), "^GSPC".to_string()),
            ("VIX".to_string(), "^VIX".to_string()),
            ("ETH".to_string(), "ETH".to_string()),
        ]
    );
    assert!(parse_symbol_pairs("  , ,").is_err(), "no entries → Err");
}

#[test]
fn day_labels_spans_a_range_inclusive() {
    assert_eq!(
        day_labels("2026-07-26", "2026-07-27"),
        vec!["2026-07-26".to_string(), "2026-07-27".to_string()]
    );
    assert_eq!(day_labels("2026-07-27", "2026-07-27"), vec!["2026-07-27".to_string()]);
}

#[test]
fn day_labels_empty_on_bad_or_reversed_range() {
    assert!(day_labels("not-a-date", "2026-07-27").is_empty());
    assert!(day_labels("2026-07-27", "2026-07-26").is_empty(), "from after to -> empty");
}

/// [`scoped_credentials`] opens the store under the scope the bin's environment names, and a scope
/// word outside `true`/`false` is a startup refusal, never a guess. The variable names are spelled
/// through `vike_secrets`' constants for the attribution reason the function's doc gives.
#[test]
fn scoped_credentials_refuses_a_scope_word_it_cannot_read() {
    let keys = vike_secrets::KeyScope::of(["DATABENTO_API_KEY"]);
    for bogus in ["yes", "1", "demo", " true", ""] {
        let vars = env(&[(vike_secrets::NO_LIVE_KEYS_ENV, bogus)]);
        assert!(scoped_credentials(&vars, &keys).is_err(), "{bogus:?} must refuse");
    }
}

/// The declared name comes back from the store `$VIKE_SETTINGS_DIR` names; `VIKE_NO_LIVE_KEYS=true`
/// withholds, inside the read, the credential of an account whose ROW is at tier `live` (the tier
/// the operator wrote on the account, never one read out of a name); a store that exists and will
/// not open degrades to the empty scope (the live gate) carrying the unreadable finding, and an
/// undeclared name still reads as undeclared.
#[test]
fn scoped_credentials_reads_the_declared_names_under_the_scope() {
    use vike_secrets::Lookup;
    let dir = tempfile::tempdir().expect("a temp dir");
    vike_secrets::create_store(Some(dir.path().to_str().expect("a utf-8 temp path")))
        .expect("create the store");
    // The vendor key is a DEPLOYMENT row, written by name.
    vike_secrets::save_credentials_to_store_unjournalled(
        dir.path(),
        vike_secrets::Table::Credential,
        &[("DATABENTO_API_KEY".to_string(), "databento-value".to_string())],
        vike_secrets::UpsertIntent::Add,
    )
    .expect("seed the store");
    // A LIVE account's key: the account first, with its tier, then its field (decision 0156 item 6).
    let created = vike_secrets::edit_account_in_unjournalled(
        dir.path(),
        vike_secrets::AccountEdit::Create {
            venue: "binance",
            tier: vike_model::AccountTier::Live,
            label: None,
        },
    )
    .expect("create the live account");
    let id = created.after.expect("a create returns its row").id;
    let live = vike_secrets::save_owned_credentials_unjournalled(
        dir.path(),
        &vike_secrets::CredentialOwner::Account(id),
        &[("API_KEY".to_string(), "live-value".to_string())],
        vike_secrets::UpsertIntent::Add,
    )
    .expect("write the live account's key")
    .names
    .remove(0);
    let settings = dir.path().to_str().expect("a utf-8 temp path");
    let keys = vike_secrets::KeyScope::of(["DATABENTO_API_KEY", live.as_str()]);

    let full = scoped_credentials(&env(&[(vike_secrets::SETTINGS_DIR_ENV, settings)]), &keys)
        .expect("no scope word is set");
    assert!(matches!(full.get("DATABENTO_API_KEY"), Lookup::Present("databento-value")));
    assert!(matches!(full.get(&live), Lookup::Present("live-value")));
    assert!(full.get("TARDIS_API_KEY").is_undeclared());

    let narrow = scoped_credentials(
        &env(&[
            (vike_secrets::SETTINGS_DIR_ENV, settings),
            (vike_secrets::NO_LIVE_KEYS_ENV, "true"),
        ]),
        &keys,
    )
    .expect("`true` is a scope word");
    assert!(matches!(narrow.get("DATABENTO_API_KEY"), Lookup::Present("databento-value")));
    assert!(matches!(narrow.get(&live), Lookup::AbsentFromStore));
    assert_eq!(narrow.withheld, 1);

    let broken = tempfile::tempdir().expect("a temp dir");
    std::fs::create_dir_all(broken.path().join("db")).expect("the db directory");
    std::fs::write(vike_secrets::db_path_in(broken.path()), b"not a sqlite database")
        .expect("plant the garbage");
    let degraded = scoped_credentials(
        &env(&[(vike_secrets::SETTINGS_DIR_ENV, broken.path().to_str().expect("utf-8"))]),
        &keys,
    )
    .expect("an unreadable store degrades, it does not refuse");
    assert!(matches!(degraded.get("DATABENTO_API_KEY"), Lookup::AbsentFromStore));
    assert!(matches!(degraded.health, vike_secrets::credentials::StoreHealth::Unreadable(_)));
}
