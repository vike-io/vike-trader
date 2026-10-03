use super::*;
use crate::exit::Exit;

/// A named engine is taken VERBATIM and never probed — an operator who typed a path must fail
/// on that path, not silently get a different binary from a lower rung.
#[test]
fn an_explicit_engine_wins_and_is_not_probed() {
    let named = locate(Some("/nowhere/at/all/backtest"), Some(Path::new("/some/project")));
    assert_eq!(named, Engine::standalone("/nowhere/at/all/backtest"));
}

/// With nothing on disk, the answer is the BARE NAME, so the OS resolves it on `PATH` rather
/// than this module reimplementing that lookup.
#[test]
fn nothing_on_disk_falls_through_to_the_bare_name() {
    let scratch = tempfile::tempdir().expect("tempdir");
    let found = locate(None, Some(scratch.path()));
    assert_eq!(found, Engine::standalone(ENGINE_BIN), "no probe may invent an absolute path");
}

/// …and a real file under `<project>/bin/` is found there, ahead of the `PATH` fallback.
#[test]
fn the_project_bin_directory_answers_when_it_holds_an_engine() {
    let scratch = tempfile::tempdir().expect("tempdir");
    let bin = scratch.path().join(vike_model::state_path::PROJECT_BIN_DIR);
    std::fs::create_dir_all(&bin).expect("create bin");
    let engine = bin.join(format!("{ENGINE_BIN}{}", std::env::consts::EXE_SUFFIX));
    std::fs::write(&engine, b"not really an engine").expect("plant");

    assert_eq!(locate(None, Some(scratch.path())), Engine::standalone(engine));
}

/// The fold, rung by rung. ⚠ `2` is deliberately NOT a usage error here even though the engine
/// calls it one: that code is overloaded on the engine side (a failed venue fetch and an
/// unopenable store both return it), and [`fold_status`]'s doc carries the whole argument. Only
/// `0` is a success; everything else, a signal included, is the unclassified-failure rung.
#[test]
fn the_childs_status_folds_onto_this_crates_ladder() {
    assert!(fold_status(Some(0), "backtest").is_ok());
    assert_eq!(fold_status(Some(2), "backtest").unwrap_err().exit, Exit::Failed);
    assert_eq!(fold_status(Some(1), "backtest").unwrap_err().exit, Exit::Failed);
    assert_eq!(fold_status(Some(101), "backtest").unwrap_err().exit, Exit::Failed);
    assert!(
        fold_status(Some(2), "data").unwrap_err().msg.contains("exited 2"),
        "the code is NAMED rather than asserted a cause for"
    );
    let signalled = fold_status(None, "backtest").unwrap_err();
    assert_eq!(signalled.exit, Exit::Failed);
    assert!(signalled.msg.contains("backtest"), "names the verb the user typed: {}", signalled.msg);
}

/// THE MULTICALL RUNG, and the ORDER that makes it safe to have added.
///
/// A standalone `backtest` still wins wherever one exists — that is the compatibility
/// guarantee, and it is asserted here rather than assumed, because the rung was inserted ABOVE
/// the `PATH` fallback and a wrong order would silently prefer the dispatcher on every box that
/// has both. Only when no standalone engine is on disk does `vike-backend` answer, and then the
/// selector is part of the command rather than part of the path.
#[test]
fn the_multicall_answers_only_when_no_standalone_engine_is_on_disk() {
    let scratch = tempfile::tempdir().expect("tempdir");
    let bin = scratch.path().join(vike_model::state_path::PROJECT_BIN_DIR);
    std::fs::create_dir_all(&bin).expect("bin dir");

    // Nothing on disk: the bare name, unchanged by this rung existing.
    assert_eq!(locate(None, Some(scratch.path())), Engine::standalone(ENGINE_BIN));

    // Only the multicall: it answers, WITH the tool selector.
    let multicall = bin.join(format!("{MULTICALL_BIN}{}", std::env::consts::EXE_SUFFIX));
    std::fs::write(&multicall, b"#!/bin/sh\n").expect("write multicall");
    let found = locate(None, Some(scratch.path()));
    assert_eq!(
        found,
        Engine {
            program: multicall.clone(),
            lead: vec![ENGINE_BIN.to_string()],
            user_data_dir: None
        }
    );
    assert!(
        found.display().ends_with(&format!("{MULTICALL_BIN} {ENGINE_BIN}")),
        "a message must name the TOOL, not just the dispatcher: {}",
        found.display()
    );

    // Both present: the standalone wins, and carries no selector.
    let standalone = bin.join(format!("{ENGINE_BIN}{}", std::env::consts::EXE_SUFFIX));
    std::fs::write(&standalone, b"#!/bin/sh\n").expect("write engine");
    assert_eq!(
        locate(None, Some(scratch.path())),
        Engine::standalone(&standalone),
        "a box with a real engine must behave exactly as it did before the multicall rung"
    );
}

/// What [`Engine::with_user_data_dir`] puts in the child's environment, read off the `Command`
/// itself: the directory under the one variable when it is given, and NOTHING when it is not —
/// not a removal, not a blank — so a spawn that was never told keeps its inherited environment
/// byte for byte. `crates/vike-cli/tests/backtest_cli.rs`'s
/// `the_local_engine_is_told_the_user_data_directory_backtest_ls_reads` is the end-to-end half.
#[test]
fn the_user_data_directory_reaches_the_childs_environment_and_nothing_else_does() {
    let told = |e: &Engine| -> Vec<(String, Option<String>)> {
        e.command()
            .get_envs()
            .map(|(k, v)| {
                (k.to_string_lossy().into_owned(), v.map(|v| v.to_string_lossy().into_owned()))
            })
            .collect()
    };

    let untold = Engine::standalone("/opt/backtest");
    assert!(told(&untold).is_empty(), "an engine never told must set nothing: {untold:?}");
    assert!(
        told(&untold.clone().with_user_data_dir(None)).is_empty(),
        "`None` must set nothing either"
    );

    let dir = Path::new("/srv/project/user_data");
    let engine = Engine::standalone("/opt/backtest").with_user_data_dir(Some(dir));
    assert_eq!(
        told(&engine),
        vec![(
            vike_model::state_path::USER_DATA_DIR_ENV.to_string(),
            Some(dir.display().to_string())
        )]
    );
    // …and the program and argv are untouched by it: telling the child where its runs go is
    // not a different engine.
    assert_eq!(engine.display(), "/opt/backtest");
}

/// A non-UTF-8 directory is NOT passed, because the engine's `std::env::vars()` sweep panics on
/// one — and such a directory can only have come from the walk the child repeats itself.
#[cfg(unix)]
#[test]
fn a_non_utf8_user_data_directory_is_left_to_the_childs_own_walk() {
    use std::os::unix::ffi::OsStrExt;
    let dir = Path::new(std::ffi::OsStr::from_bytes(b"/srv/\xff/user_data"));
    let engine = Engine::standalone("/opt/backtest").with_user_data_dir(Some(dir));
    assert_eq!(engine.command().get_envs().count(), 0, "a value the child would panic on");
}

/// The PRE-RENAME multicall name is not a rung. Rung 4 looked for `vike` from the rename until
/// 2026-09-26, and this is the test that would have failed: a file by that name is passed over,
/// and with nothing else on disk the answer is still the bare name.
#[test]
fn a_binary_named_by_the_pre_rename_spelling_is_not_the_multicall() {
    let scratch = tempfile::tempdir().expect("tempdir");
    let bin = scratch.path().join(vike_model::state_path::PROJECT_BIN_DIR);
    std::fs::create_dir_all(&bin).expect("bin dir");
    let old = bin.join(format!("vike{}", std::env::consts::EXE_SUFFIX));
    std::fs::write(&old, b"#!/bin/sh\n").expect("write the pre-rename name");
    assert_eq!(
        locate(None, Some(scratch.path())),
        Engine::standalone(ENGINE_BIN),
        "`vike` is the pre-rename name (v0.1.21 and earlier); no current release ships it"
    );
}

/// The multicall FILE name is the backend package's name: `crates/vike/Cargo.toml` declares no
/// `[[bin]]`, so cargo names the executable — and the release names its asset — after the
/// package. Read from that manifest at run time, so renaming the package reddens this before a
/// release ships a binary rung 4 cannot see, which is the failure [`MULTICALL_BIN`] ended.
#[test]
fn the_multicall_file_name_is_the_backend_package_name() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../vike/Cargo.toml");
    let text =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    let manifest: toml::Table =
        text.parse().unwrap_or_else(|e| panic!("parse {}: {e}", path.display()));
    let name = manifest
        .get("package")
        .and_then(|p| p.get("name"))
        .and_then(toml::Value::as_str)
        .unwrap_or_else(|| panic!("{} names no [package] name", path.display()));
    assert_eq!(name, MULTICALL_BIN, "rung 4 looks for the file this manifest builds");
    assert!(
        manifest.get("bin").is_none(),
        "a [[bin]] in {} would name the executable instead of the package",
        path.display()
    );
}

/// A missing engine is a CONNECT-class failure whose message names the binary and says how to
/// get one — the same disposition as an unreachable datahub, and for the same reason.
#[test]
fn a_missing_engine_is_a_connect_failure_that_says_what_to_do() {
    let scratch = tempfile::tempdir().expect("tempdir");
    let absent = scratch.path().join("definitely-not-an-engine");
    let e = run(&Engine::standalone(&absent), &["--profile", "x.toml"], "backtest").unwrap_err();
    assert_eq!(e.exit, Exit::Connect);
    assert!(e.msg.contains(ENGINE_BIN), "names the engine: {}", e.msg);
    assert!(e.msg.contains("--engine"), "names the way out: {}", e.msg);
}

/// No project above the working directory means no project scratch — the caller must refuse
/// rather than reach for the system temp directory.
#[test]
fn scratch_needs_a_project() {
    assert!(scratch_root(None).is_none());
    // A root that does not exist is answered, not created and not an error: resolving a path is
    // not the same as staging into it, and the sweep folded into this call is best-effort.
    let absent = tempfile::tempdir().expect("tempdir");
    assert_eq!(
        scratch_root(Some(absent.path())),
        Some(absent.path().join(vike_model::state_path::PROJECT_TMP_DIR))
    );
}

/// …and resolving the root PRUNES it. The property is the one `vike_model::scratch`'s module
/// doc insists on: `ScratchDir`'s `Drop` cannot bound a population it never created, so a run
/// killed rather than unwound (Ctrl-C, an OOM kill) leaves an entry nothing else would ever
/// remove. Folding the sweep into the resolver is what makes forgetting it impossible.
#[test]
fn resolving_the_scratch_root_bounds_what_earlier_runs_abandoned() {
    let project = tempfile::tempdir().expect("tempdir");
    let tmp = project.path().join(vike_model::state_path::PROJECT_TMP_DIR);
    std::fs::create_dir_all(&tmp).expect("create tmp");
    let planted = vike_model::scratch::DEFAULT_MAX_SCRATCH_ENTRIES + 5;
    for i in 0..planted {
        std::fs::create_dir(tmp.join(format!("abandoned-{i}"))).expect("plant");
    }

    let root = scratch_root(Some(project.path())).expect("a project resolves a scratch root");
    assert_eq!(root, tmp);
    let left = std::fs::read_dir(&tmp).expect("read tmp").count();
    assert_eq!(
        left,
        vike_model::scratch::DEFAULT_MAX_SCRATCH_ENTRIES,
        "{planted} abandoned entries must be pruned to the retention, not left to grow"
    );
}
