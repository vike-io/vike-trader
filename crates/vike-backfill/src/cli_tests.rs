use super::*;

fn argv(v: &[&str]) -> Vec<String> {
    std::iter::once("prog").chain(v.iter().copied()).map(str::to_string).collect()
}

/// A representative spec: valued flags, a toggle, and no positionals — the shape 5 of the
/// 10 bins have.
const FLAGGY: CliSpec = CliSpec {
    bin: "demo_backfill",
    usage: "usage: demo_backfill --from D --to D [--store DIR] [--dry-run]",
    valued: &["--from", "--to", "--store"],
    toggles: &["--dry-run"],
    positionals: 0,
};

/// …and the positional shape `dukascopy_backfill` had (the five one-shot kline programs shared it
/// until docs/decisions/0094 deleted them; `dukascopy_backfill` itself went the same way, per the
/// same record).
const POSITIONAL: CliSpec = CliSpec {
    bin: "demo_klines",
    usage: "usage: demo_klines <ROOT> <SYM> <IV> <START> <END>",
    valued: &[],
    toggles: &[],
    positionals: 5,
};

/// The two outcomes that are NOT a run, in both spellings each. These were the whole defect:
/// every bin in this crate answered `--help` with a usage error, a panic, or an ingest.
#[test]
fn help_and_version_are_outcomes_not_errors() {
    for flag in ["-h", "--help"] {
        assert_eq!(FLAGGY.triage(&argv(&[flag])), Ok(Parsed::Help), "{flag}");
        assert_eq!(POSITIONAL.triage(&argv(&[flag])), Ok(Parsed::Help), "{flag}");
    }
    for flag in ["-V", "--version"] {
        assert_eq!(FLAGGY.triage(&argv(&[flag])), Ok(Parsed::Version), "{flag}");
        assert_eq!(POSITIONAL.triage(&argv(&[flag])), Ok(Parsed::Version), "{flag}");
    }
    // …and asked for AFTER other arguments, which is how a person actually reaches for it
    // ("what were the other flags again?").
    assert_eq!(FLAGGY.triage(&argv(&["--from", "x", "--help"])), Ok(Parsed::Help));
}

/// A valid invocation is still a run — the property that keeps "exit 0 on --help" from being
/// bought by making everything exit 0, and that keeps this triage out of the way of real work.
#[test]
fn a_valid_invocation_triages_as_a_run() {
    assert_eq!(FLAGGY.triage(&argv(&[])), Ok(Parsed::Run));
    assert_eq!(
        FLAGGY.triage(&argv(&["--from", "2026-01-01", "--to", "2026-01-02", "--dry-run"])),
        Ok(Parsed::Run)
    );
    assert_eq!(POSITIONAL.triage(&argv(&["/d", "BTC", "1m", "0", "1"])), Ok(Parsed::Run));
}

/// The negative half. `arg`/`has_flag` are one-flag lookups blind to every token they were not
/// asked about, so a typo used to resolve to the DEFAULT and write to the wrong store in
/// silence.
#[test]
fn an_unknown_flag_is_rejected_rather_than_ignored() {
    let err = FLAGGY.triage(&argv(&["--stroe", "/data"])).expect_err("a typo must not run");
    assert!(err.contains("--stroe"), "the error names the offending argument: {err}");
    // `--flag=value` too: `arg` never understood that spelling and silently ignored it.
    assert!(FLAGGY.triage(&argv(&["--store=/x"])).is_err(), "--flag=value is not supported");
    // A lowercase `-v` is verbosity everywhere else on the box, so it stays unknown here.
    assert!(FLAGGY.triage(&argv(&["-v"])).is_err(), "-v must stay unknown");
}

/// A valued flag consumes the token after it, so a store PATH is never counted as a stray
/// positional — and a trailing valued flag with nothing after it is a usage error, not a
/// silent `None` (which is what `arg` returned, leaving the bin to use its default).
#[test]
fn valued_flags_consume_their_value_and_a_trailing_one_is_an_error() {
    assert_eq!(FLAGGY.triage(&argv(&["--store", "/x/y"])), Ok(Parsed::Run));
    let err = FLAGGY.triage(&argv(&["--store"])).expect_err("a trailing valued flag");
    assert!(err.contains("--store"), "{err}");
}

/// **The swallow rule, at the gate every bin in this crate runs first.** A valued flag may not
/// eat a FLAG: `--store --dry-run` used to triage as a clean run, after which `arg` answered
/// `Some("--dry-run")` for the store root while `has_flag` still reported the toggle as set —
/// the backfill wrote into a directory named `--dry-run` and nothing anywhere disagreed.
///
/// Both messages name the FLAG that went unfed, and the swallow message names the eaten token
/// too, so the diagnostic is about the operator's mistake rather than about whatever landed in
/// flag position afterwards.
#[test]
fn a_valued_flag_may_not_swallow_a_following_flag() {
    for line in [
        &["--store", "--dry-run"][..],
        &["--from", "--to", "2026-01-01"][..],
        &["--store", "--"][..],
    ] {
        let err = FLAGGY.triage(&argv(line)).expect_err("a flag is not a value");
        assert!(err.contains(line[0]), "the message names the unfed flag: {err}");
        assert!(err.contains(line[1]), "…and the token it would have eaten: {err}");
    }
    // The rule is `--`, not `-`: a NEGATIVE NUMBER is a real value and still reaches the bin.
    assert_eq!(FLAGGY.triage(&argv(&["--from", "-1"])), Ok(Parsed::Run));
    assert_eq!(FLAGGY.triage(&argv(&["--store", "-h"])), Ok(Parsed::Run), "the residual");
}

/// [`flag_value`] is the same rule as a function, for `eod_backfill` — the one bin in this crate
/// that parses its own argv rather than reading it back with [`arg`]. An ABSENT optional flag
/// never reaches it — that is what keeps an unmentioned `--store` on its default instead of
/// becoming an error.
#[test]
fn flag_value_refuses_a_missing_and_a_flag_shaped_value() {
    assert_eq!(flag_value("--store", Some("/x".to_string())).unwrap(), "/x");
    assert_eq!(flag_value("--store", Some("-1".to_string())).unwrap(), "-1", "a negative value");
    assert_eq!(flag_value("--store", Some(String::new())).unwrap(), "", "emptiness is per-flag");
    let missing = flag_value("--store", None).expect_err("no token at all");
    assert!(missing.contains("--store"), "{missing}");
    let swallowed =
        flag_value("--store", Some("--interval".to_string())).expect_err("a flag is not a value");
    assert!(swallowed.contains("--store") && swallowed.contains("--interval"), "{swallowed}");
    assert!(is_flag_token("--x") && !is_flag_token("-x") && !is_flag_token("x"));
}

/// Positional bins accept exactly as many bare tokens as they document, and one more is a
/// usage error rather than being dropped on the floor.
#[test]
fn extra_positionals_are_rejected_and_a_flaggy_bin_takes_none() {
    assert!(POSITIONAL.triage(&argv(&["/d", "BTC", "1m", "0", "1", "extra"])).is_err());
    assert!(
        FLAGGY.triage(&argv(&["stray"])).is_err(),
        "a bin with no positionals must reject a bare token"
    );
}

/// The 341-GB-footgun regression guard: every batch bin's `LogConfig` must default its file
/// level to `warn`, not vike-log's own `trace` default — see [`log_config`]'s doc. This does
/// NOT assert the env override still wins; that precedence is `vike_log::file_level_directive`'s
/// own test (`file_level_precedence_env_over_cfg`), unchanged and untouched here.
#[test]
fn log_config_defaults_file_level_to_warn_not_trace() {
    let cfg = log_config("test-bin");
    assert_eq!(cfg.file_level, "warn");
    assert_eq!(cfg.file_prefix, "test-bin");
    // Everything else stays vike-log's own default — this helper narrows file_level and the
    // log DIRECTORY (below), nothing more.
    let default = vike_log::LogConfig::default();
    assert_eq!(cfg.console_level, default.console_level);
    assert_eq!(cfg.file_enabled, default.file_enabled);
    assert!(cfg.dir.is_none(), "the CONFIG layer stays unset — this helper names no override");
}

/// …and the DEFAULT log DIRECTORY is the project's, not `<exe_dir>/logs`. Asserted by SHAPE,
/// because the answer depends on where the harness runs: under `cargo test` a project resolves
/// (the repo above this crate) and it must end `settings/state/logs`; with no project above the
/// working directory it is `None`, which is vike-log's `<exe_dir>/logs` last resort — the
/// pre-repoint behaviour, deliberately kept.
#[test]
fn log_config_defaults_the_directory_to_the_project_state_dir() {
    let cfg = log_config("test-bin");
    match &cfg.project_dir {
        Some(dir) => {
            let tail = PathBuf::from(vike_model::state_path::PROJECT_SETTINGS_DIR)
                .join(vike_model::state_path::STATE_SUBDIR)
                .join(vike_model::state_path::LOGS_SUBDIR);
            assert!(
                dir.ends_with(&tail),
                "{} must end with {} — the one directory the program writes into",
                dir.display(),
                tail.display()
            );
        }
        None => { /* no project above the CWD: the <exe_dir>/logs last resort, unchanged */ }
    }
}

#[test]
fn arg_finds_value_after_flag() {
    let args: Vec<String> =
        ["prog", "--store", "/x", "--flag"].iter().map(|s| s.to_string()).collect();
    assert_eq!(arg(&args, "--store"), Some("/x".to_string()));
    assert_eq!(arg(&args, "--missing"), None);
    assert_eq!(arg(&args, "--flag"), None, "trailing flag has no value");
}

#[test]
fn has_flag_is_presence_only() {
    let args: Vec<String> =
        ["prog", "--dry-run", "--store", "/x"].iter().map(|s| s.to_string()).collect();
    assert!(has_flag(&args, "--dry-run"));
    assert!(!has_flag(&args, "--nope"));
    // The distinction from `arg`: a valueless flag followed by another flag must NOT be read
    // as carrying that next token as its value.
    assert_eq!(arg(&args, "--dry-run"), Some("--store".to_string()));
}

fn env(pairs: &[(&str, &str)]) -> std::collections::HashMap<String, String> {
    pairs.iter().map(|(k, v)| ((*k).to_string(), (*v).to_string())).collect()
}

/// A `repo_default` that exists on no machine — the INSTALLED shape, and the only way a test
/// running inside the checkout can see any rung below the dev-checkout hinge.
const NO_CHECKOUT: &str = "/definitely/not/a/real/build/machine/path/market_data/hist";

#[test]
fn env_var_wins_over_default_but_loses_to_explicit() {
    let repo = default_store_root();
    // explicit --store beats everything
    assert_eq!(
        resolve(Some("/x/explicit"), &repo, None, &env(&[("VIKE_HIST_STORE", "/y/env")])).root,
        PathBuf::from("/x/explicit")
    );
    // $VIKE_HIST_STORE is honored when there is no --store — THE drift fix: pmxt/databento/
    // tardis previously ignored it and fell through to a CWD-relative default.
    assert_eq!(
        resolve(None, &repo, None, &env(&[("VIKE_HIST_STORE", "/y/env")])).root,
        PathBuf::from("/y/env")
    );
    // neither → the repo-root default, because these tests RUN in the checkout and the
    // dev-checkout hinge outranks the project rung there.
    assert_eq!(resolve(None, &repo, None, &env(&[])).root, repo);
}

/// **THIS crate's call site, on the rung a `cargo test` run cannot otherwise reach** — and the
/// transposition guard for it. `resolve` is the one place this crate assembles the shared
/// precedence, and the project and per-user rungs are made unmistakably different (a scratch
/// project vs a fake `$HOME`), so a wiring that swapped them reddens on the value rather than
/// on a path suffix. Verified by mutation: swapping the two inside
/// `vike_model::store_path::resolve_store_root_from` turns this red with the `$HOME` path.
#[test]
fn the_call_site_reaches_the_project_rung_when_this_box_has_no_checkout() {
    use vike_model::store_path::{HOME_VAR, LOCALAPPDATA_VAR, StoreRootRung, XDG_DATA_HOME_VAR};

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
    let user = vike_model::store_path::user_data_dir_from_vars(&vars).unwrap();
    let _ = std::fs::remove_dir_all(&scratch);

    assert_eq!(got.root, project.join("market_data").join("hist"), "the PROJECT's own data folder");
    assert_eq!(got.rung, StoreRootRung::Project);
    assert_ne!(got.root, user, "the two rungs must be distinguishable in this fixture");
}

/// …and with no project above the working directory, the SAME call site falls to the per-user
/// directory. Together with the test above this pins the ORDER, not just one answer.
#[test]
fn the_call_site_falls_to_the_user_dir_without_a_project() {
    use vike_model::store_path::{HOME_VAR, LOCALAPPDATA_VAR, StoreRootRung, XDG_DATA_HOME_VAR};

    let vars = env(&[
        (HOME_VAR, "/home/u"),
        (XDG_DATA_HOME_VAR, "/xdg"),
        (LOCALAPPDATA_VAR, "C:\\Users\\u\\AppData\\Local"),
    ]);
    // `cwd = None` — a binary that could not read its own working directory. No project can be
    // resolved from it, so the per-user directory is the only remaining answer.
    let got = resolve(None, Path::new(NO_CHECKOUT), None, &vars);
    assert_eq!(got.root, vike_model::store_path::user_data_dir_from_vars(&vars).unwrap());
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

/// The public entry point reads `VIKE_HIST_STORE` out of the SAME map it forwards, so a bin
/// that collected the process environment once gets the historical precedence exactly.
#[test]
fn store_root_reads_the_override_out_of_the_supplied_map() {
    assert_eq!(store_root(None, &env(&[("VIKE_HIST_STORE", "/y/env")])), PathBuf::from("/y/env"));
    assert_eq!(
        store_root(Some("/x/explicit"), &env(&[("VIKE_HIST_STORE", "/y/env")])),
        PathBuf::from("/x/explicit")
    );
}

/// **Behaviour preservation across the map lift.** A dev checkout (this repo, whose root the
/// compile-time `default_store_root` names) resolves to `<repo>/market_data/hist` regardless of the
/// platform trio — the compatibility hinge in `vike_model::store_path::resolve_store_root`.
/// That is the path every the CI box backfill takes today, asserted on a unix-shaped AND a
/// windows-shaped environment map.
#[test]
fn a_dev_checkout_is_unaffected_by_the_platform_trio_on_either_platform_shape() {
    // Spelled through `vike_model::store_path`'s constants, not as bare literals: that crate is
    // the ONE place these three variable names appear, and a fixture that re-spelled them here
    // would put an incidental `vike-backfill` row back on the settings registry for a read this
    // crate no longer performs.
    use vike_model::store_path::{HOME_VAR, LOCALAPPDATA_VAR, XDG_DATA_HOME_VAR};
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
