//! A `.rhai` strategy FILE (decision 0024's rails): the path rule, the numeric overrides, the read's
//! refusals, the override check against the script's own `param(…)` calls, and the audit pair.

use super::*;

/// A profile-shaped TOML over a hyperliquid mount whose `[strategy]` table holds `selection_line`.
fn rhai_profile_toml(selection_line: &str, params: &str) -> String {
    format!("venue = \"hyperliquid\"\nsymbol = \"BTC\"\n[strategy]\n{selection_line}\n{params}")
}

/// The header every `.rhai` file of this suite carries: it may run live and reads `size`.
const SIZE_HEADER: &str = "\
// ```toml
// [strategy]
// runs_on = \"live\"
// [params.size]
// type = \"number\"
// ```
";

#[test]
fn a_rhai_profile_parses_and_validates() {
    let p = ProfileDaemon::from_toml_str(&rhai_profile_toml(
        "file = \"trend/thing.rhai\"",
        "[strategy.params]\nsize = 2.0\n",
    ))
    .expect("a rhai profile parses and validates without touching the filesystem");
    assert_eq!(p.strategy_name(), "thing", "a file is known by its stem");
    assert_eq!(p.strategy_language(), Some(vike_strategy::file::scan::StrategyFileLanguage::Rhai));
}

/// The two RETIRED selection keys are refused at load with the one-line fix, whatever sits beside
/// them, `name` first; nothing rewrites the stored text.
#[test]
fn the_retired_name_and_rhai_keys_are_refused_with_the_fix() {
    for (line, needle, fix) in [
        ("name = \"grid\"", "`[strategy] name = \"grid\"` is retired", "grid.rs"),
        (
            "name = \"grid\"\nrhai = \"thing.rhai\"",
            "`[strategy] name = \"grid\"` is retired",
            "file = ",
        ),
        (
            "name = \"grid\"\nfile = \"grid.rs\"",
            "`[strategy] name = \"grid\"` is retired",
            "file = ",
        ),
        // The backtest registry's inline-script name: retired like any other name.
        ("name = \"rhai\"", "`[strategy] name = \"rhai\"` is retired", "file = "),
        (
            "rhai = \"thing.rhai\"",
            "`[strategy] rhai = \"thing.rhai\"` is retired",
            "`strategies/` folder",
        ),
        ("rhai = \"thing.rhai\"\nfile = \"thing.rhai\"", "is retired", "file = "),
    ] {
        let err = ProfileDaemon::from_toml_str(&rhai_profile_toml(line, "")).unwrap_err();
        assert!(err.contains(needle), "{line:?}: {err}");
        assert!(err.contains(fix), "{line:?} spells the fix: {err}");
    }
}

/// The path rule, at load: a path the strategies folder cannot hold (empty, absolute, climbing out,
/// not a `.rs` or `.rhai` file) is refused before any file is looked for.
#[test]
fn a_file_path_the_strategies_folder_cannot_hold_is_refused_at_load() {
    for (file, needle) in [
        ("", "is not a path inside the strategies folder"),
        ("/srv/strategies/thing.rhai", "is not a path inside the strategies folder"),
        ("../thing.rhai", "is not a path inside the strategies folder"),
        ("trend/../../thing.rhai", "is not a path inside the strategies folder"),
        ("thing.py", "is neither a .rs nor a .rhai file"),
        ("thing", "is neither a .rs nor a .rhai file"),
    ] {
        let err = ProfileDaemon::from_toml_str(&rhai_profile_toml(&format!("file = {file:?}"), ""))
            .expect_err("refused at load");
        assert!(err.contains(needle), "{file:?}: {err}");
    }
}

/// The script arm keeps the daemon's no-silently-ignored-key posture at the VALUE level: a
/// non-numeric override can never apply (`param` takes an f64), so it is refused at LOAD — for a
/// `.rhai` file only: a `.rs` file's reader takes the types its header declares.
#[test]
fn a_non_numeric_rhai_param_is_refused_at_load() {
    let err = ProfileDaemon::from_toml_str(&rhai_profile_toml(
        "file = \"thing.rhai\"",
        "[strategy.params]\nsize = \"2\"\n",
    ))
    .unwrap_err();
    assert!(err.contains("size"), "names the offending key: {err}");
    assert!(err.contains("NUMBER"), "states the requirement: {err}");
    ProfileDaemon::from_toml_str(&rhai_profile_toml(
        "file = \"thing.rs\"",
        "[strategy.params]\nsize = \"2\"\n",
    ))
    .expect("the numeric rule is a script's: a `.rs` file's header judges its types at resolve");
}

/// The resolve reads the file, and a missing one fails NAMING THE PATH — before any core
/// spawns, same as every other resolve failure.
#[test]
fn a_missing_script_file_is_a_resolve_error_naming_the_path() {
    let dir = tempfile::tempdir().expect("an empty strategies folder");
    let p = ProfileDaemon::from_toml_str(&rhai_profile_toml(
        "file = \"no-such-dir/no-such-script.rhai\"",
        "",
    ))
    .expect("validates — the file is read at resolve, not at load");
    let err = match p.resolve_mount(dir.path(), false) {
        Err(e) => e,
        Ok(_) => panic!("a missing script file must fail the resolve"),
    };
    assert!(err.contains("no-such-script.rhai"), "names the path: {err}");
}

/// `file` under `root` resolved as a profile's strategy, on a thread of its own: an `Err` when the
/// resolve answered inside the deadline with a refusal, and a panic when it never answered (a read
/// that blocks).
fn resolve_within_deadline(root: &std::path::Path, file: &str) -> String {
    let p = ProfileDaemon::from_toml_str(&rhai_profile_toml(&format!("file = {file:?}"), ""))
        .expect("validates — the file is read at resolve, not at load");
    let root = root.to_path_buf();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let outcome = p.resolve_mount(&root, false).map(|_| ());
        let _ = tx.send(outcome);
    });
    match rx.recv_timeout(std::time::Duration::from_secs(5)) {
        Ok(outcome) => outcome.expect_err("this file is not a script"),
        Err(e) => panic!("the resolve never answered, so it blocked on the read: {e}"),
    }
}

/// A FIFO named like a script is REFUSED, never opened: the open blocks until a writer comes, and on
/// a runtime mount that is the core's fold thread, so every account would stall.
#[cfg(unix)]
#[test]
fn a_fifo_named_like_a_script_is_refused_without_opening_it() {
    let dir = tempfile::tempdir().expect("temp dir");
    let fifo = dir.path().join("pipe.rhai");
    let made = std::process::Command::new("mkfifo").arg(&fifo).status().expect("mkfifo runs");
    assert!(made.success(), "mkfifo failed");
    let err = resolve_within_deadline(dir.path(), "pipe.rhai");
    assert!(err.contains("not a regular file"), "says what is wrong: {err}");
    assert!(err.contains("pipe.rhai"), "names the path: {err}");
}

/// A folder is refused in the same words as a FIFO, rather than in the platform's.
#[test]
fn a_folder_named_like_a_script_is_refused() {
    let dir = tempfile::tempdir().expect("temp dir");
    std::fs::create_dir(dir.path().join("folder.rhai")).expect("folder");
    let err = resolve_within_deadline(dir.path(), "folder.rhai");
    assert!(err.contains("not a regular file"), "says what is wrong: {err}");
}

/// A file past the cap is refused before it is read, naming the cap. One byte under it is a script
/// like any other: the cap is the only thing between the two.
#[test]
fn a_script_over_the_cap_is_refused_and_one_at_the_cap_compiles() {
    const CAP: usize = 1024 * 1024;
    let head = format!("{SIZE_HEADER}fn on_bar() {{}}\n");
    let at_cap_source = format!("{head}{}", " ".repeat(CAP - head.len()));
    let over_cap_source = format!("{at_cap_source} ");
    let dir = test_strategy_files::folder(&[
        ("at-cap.rhai", at_cap_source.as_str()),
        ("over-cap.rhai", over_cap_source.as_str()),
    ]);
    let p = ProfileDaemon::from_toml_str(&rhai_profile_toml("file = \"at-cap.rhai\"", ""))
        .expect("validates");
    assert!(p.resolve_mount(dir.path(), false).is_ok(), "a script at the cap still compiles");

    let err = resolve_within_deadline(dir.path(), "over-cap.rhai");
    assert!(err.contains("larger than"), "names the cap: {err}");
}

/// A file whose header is missing or broken is refused at resolve, naming the file and the reason.
#[test]
fn a_script_without_a_readable_header_is_refused_naming_the_file() {
    let dir = test_strategy_files::folder(&[
        ("bare.rhai", "fn on_bar() {}\n"),
        ("broken.rhai", "// ```toml\n// [strategy]\n// runs_on = \"everywhere\"\n// ```\n"),
    ]);
    for file in ["bare.rhai", "broken.rhai"] {
        let p = ProfileDaemon::from_toml_str(&rhai_profile_toml(&format!("file = {file:?}"), ""))
            .expect("validates");
        let err = match p.resolve_mount(dir.path(), false) {
            Err(e) => e,
            Ok(_) => panic!("{file} has no header it can be judged by"),
        };
        assert!(err.starts_with(&format!("strategy file {file:?}: ")), "names the file: {err}");
    }
}

/// The resolve refuses an override the script's own top level never asks for, keyed on
/// `vike_rhai::discover_params` — even when the header declares it, because a header and its script
/// can drift apart. A key the header does not declare is the header's refusal, in its words.
#[test]
fn a_rhai_override_the_script_never_asks_for_is_refused_at_resolve() {
    let source = |header_keys: &str| {
        format!(
            "// ```toml\n// [strategy]\n{header_keys}// ```\n\
             const SIZE = param(\"size\", 1.0);\nfn on_bar(strategy) {{ \
             if strategy.position() == 0.0 {{ \
             strategy.order(\"buy\", OrderSide::Long).size(SIZE); }} }}\n"
        )
    };
    let drifted =
        source("// [params.size]\n// type = \"number\"\n// [params.sizee]\n// type = \"number\"\n");
    let declares_size = source("// [params.size]\n// type = \"number\"\n");
    let dir = test_strategy_files::folder(&[
        ("drifted.rhai", drifted.as_str()),
        ("declares-size.rhai", declares_size.as_str()),
    ]);
    let resolve = |file: &str| {
        let p = ProfileDaemon::from_toml_str(&rhai_profile_toml(
            &format!("file = {file:?}"),
            "[strategy.params]\nsizee = 2.0\n",
        ))
        .expect("validates — key names are checked at resolve, against the file");
        match p.resolve_mount(dir.path(), false) {
            Err(e) => e,
            Ok(_) => panic!("{file}: an unasked-for override must fail the resolve"),
        }
    };
    let err = resolve("drifted.rhai");
    assert!(err.contains("never asks for these") && err.contains("sizee"), "{err}");
    assert!(err.contains("size (default 1)"), "names the script's own knobs: {err}");
    let err = resolve("declares-size.rhai");
    assert!(err.contains("does not read these `[strategy.params]` keys: sizee"), "{err}");
    assert!(err.contains("It reads: size (a number (integer or float))."), "{err}");
}

/// The happy path: a script resolves into the `Script` arm, carrying the audit pair — the file as
/// the profile named it and the sha256 of the bytes that were actually read — so the INFO audit
/// line's claim is assertable without a log subscriber.
#[test]
fn a_rhai_profile_resolves_to_a_script_mount_carrying_the_audit_hash() {
    let dir =
        test_strategy_files::folder(&[("trend/resolves.rhai", test_strategy_files::BUY_HOLD)]);
    let p = ProfileDaemon::from_toml_str(&rhai_profile_toml(
        "file = \"trend/resolves.rhai\"",
        "[strategy.params]\nsize = 3.0\n",
    ))
    .expect("validates");
    match p.resolve_mount(dir.path(), false).expect("the script compiles and mounts") {
        MountResolvedStrategy::Script { path, sha256, .. } => {
            assert_eq!(path, "trend/resolves.rhai", "the file as the profile named it");
            assert_eq!(
                sha256,
                vike_hub_protocol::hash::sha256_hex(test_strategy_files::BUY_HOLD.as_bytes()),
                "the hash is of the bytes read"
            );
        }
        MountResolvedStrategy::Sandboxed { .. } => {
            panic!("a `.rhai` file must resolve in this process")
        }
    }
    // ...and the boxing wrapper `main` calls accepts it like any other strategy.
    assert!(p.resolve_strategy(dir.path(), false).is_ok());
}

/// The effective-params line reports the file plus the overrides that DID land (the resolve refuses
/// any other kind), never the raw table.
#[test]
fn the_rhai_effective_params_line_reports_path_and_overrides() {
    let dir = test_strategy_files::folder(&[("thing.rhai", test_strategy_files::BUY_HOLD)]);
    let line = |params: &str| {
        let p = ProfileDaemon::from_toml_str(&rhai_profile_toml("file = \"thing.rhai\"", params))
            .expect("validates");
        p.resolve_mount(dir.path(), false).expect("resolves").effective_params().to_string()
    };
    assert_eq!(line("[strategy.params]\nsize = 3.0\n"), "script=thing.rhai size=3");
    assert_eq!(
        line(""),
        "script=thing.rhai (no overrides — every param() runs its script default)",
        "an override-free mount says so rather than claiming knobs"
    );
}

/// `validate_for_live` is strategy-agnostic and a rhai profile rides it unchanged: the wired
/// hyperliquid/BTC pair passes, a foreign symbol on the same venue is refused.
#[test]
fn a_rhai_profile_gets_the_same_live_verdicts_as_any_other() {
    let ok = ProfileDaemon::from_toml_str(&rhai_profile_toml("file = \"thing.rhai\"", ""))
        .expect("validates");
    assert!(
        ok.validate_for_live(&crate::testutil::live_box()).is_ok(),
        "hyperliquid/BTC is a live-wired pair"
    );

    let foreign = ProfileDaemon::from_toml_str(
        "venue = \"hyperliquid\"\nsymbol = \"NOPE\"\n[strategy]\nfile = \"thing.rhai\"\n",
    )
    .expect("validates");
    assert!(
        foreign.validate_for_live(&crate::testutil::live_box()).is_err(),
        "a foreign symbol is still refused"
    );
}
