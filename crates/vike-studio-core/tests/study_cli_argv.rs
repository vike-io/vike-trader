//! The POSITIVE argv pins of decision 0155 for the `vike-study` bin: every command line the
//! hand-rolled exact-token reader accepted must still be accepted, with the same meaning.
//!
//! Written against that reader FIRST and run unchanged against `clap`'s. A case asserts only what
//! is true of both: a marker that proves the argv got PAST the reader (an error printed by the step
//! after it), never the wording of a usage refusal.
//!
//! ⚠ The `--flag=value` spelling is NOT pinned in the cases above: the old reader ignored it and `clap` reads it
//! (the owner's ruling in decision 0155), which is a change on purpose and is tested at the bottom of
//! this file.
//!
//! Whole-file gated on `study-cli` exactly as the bin is (`required-features`), because
//! `env!("CARGO_BIN_EXE_vike-study")` names a bin that does not exist without it.
#![cfg(feature = "study-cli")]

use std::process::Command;

/// `vike-study` spawned with `args` in a scratch directory whose settings directory is EMPTY, so
/// the history route is the compiled default and no real settings are read. Exit code and stderr.
fn study(args: &[&str]) -> (Option<i32>, String, String) {
    let dir = tempfile::tempdir().expect("a scratch directory");
    let settings = tempfile::tempdir().expect("an empty settings directory");
    let out = Command::new(env!("CARGO_BIN_EXE_vike-study"))
        .current_dir(dir.path())
        .env("VIKE_SETTINGS_DIR", settings.path())
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("run vike-study {args:?}: {e}"));
    (
        out.status.code(),
        String::from_utf8_lossy(&out.stdout).to_string(),
        String::from_utf8_lossy(&out.stderr).to_string(),
    )
}

/// The window is judged AFTER the argv was read: both of these are the reader's acceptance plus a
/// refusal of the VALUE.
#[test]
fn a_study_command_line_is_read_and_its_window_is_judged() {
    let (code, _, stderr) = study(&[
        "--study",
        "s",
        "--recipe",
        "r.toml",
        "--from",
        "nonsense",
        "--to",
        "also-nonsense",
    ]);
    assert_eq!(code, Some(1), "{stderr}");
    assert!(stderr.contains("--from/--to must be"), "{stderr}");

    let (code, _, stderr) = study(&[
        "--study",
        "s",
        "--recipe",
        "r.toml",
        "--from",
        "2026-02-01",
        "--to",
        "2026-01-01",
    ]);
    assert_eq!(code, Some(1), "{stderr}");
    assert!(stderr.contains("--to must be after --from"), "{stderr}");
}

/// Every flag the tool takes, on a real (empty) recipe: the run reaches the history route and says
/// where it reads from, which is printed only after the whole command line was read and the recipe
/// parsed. The trainer path does not exist; whatever the run does about that comes later.
#[test]
fn every_study_flag_is_read_up_to_the_history_route() {
    let work = tempfile::tempdir().expect("a scratch directory");
    let recipe = work.path().join("r.toml");
    std::fs::write(&recipe, "").expect("write an empty recipe");
    let recipe = recipe.to_str().expect("a UTF-8 path").to_string();
    let scratch = work.path().join("scratch");
    let scratch = scratch.to_str().expect("a UTF-8 path").to_string();
    let runs = work.path().join("runs");
    let runs = runs.to_str().expect("a UTF-8 path").to_string();
    let trainer = work.path().join("no-such-lightgbm");
    let trainer = trainer.to_str().expect("a UTF-8 path").to_string();

    for args in [
        vec![
            "--study",
            "s",
            "--recipe",
            recipe.as_str(),
            "--from",
            "2026-01-01",
            "--to",
            "2026-02-01",
        ],
        vec![
            "--study",
            "s",
            "--recipe",
            recipe.as_str(),
            "--from",
            "2026-01-01T05",
            "--to",
            "1785906000",
            "--lightgbm",
            trainer.as_str(),
            "--scratch",
            scratch.as_str(),
            "--runs-root",
            runs.as_str(),
        ],
    ] {
        let (code, _, stderr) = study(&args);
        assert!(
            code.is_some_and(|c| c != 0),
            "{args:?} ends in a refusal (no such study): {stderr}"
        );
        assert!(
            stderr.contains("history from"),
            "{args:?} must be read in full and reach the history route; stderr: {stderr}"
        );
    }
}

/// `--store` is REFUSED BY NAME in both spellings the old reader saw — after the recipe is read.
#[test]
fn the_store_flag_is_still_refused_by_name() {
    let work = tempfile::tempdir().expect("a scratch directory");
    let recipe = work.path().join("r.toml");
    std::fs::write(&recipe, "").expect("write an empty recipe");
    let recipe = recipe.to_str().expect("a UTF-8 path").to_string();
    for store in [vec!["--store", "/srv/hist"], vec!["--store=/srv/hist"]] {
        let mut args = vec![
            "--study",
            "s",
            "--recipe",
            recipe.as_str(),
            "--from",
            "2026-01-01",
            "--to",
            "2026-02-01",
        ];
        args.extend(store.iter().copied());
        let (code, _, stderr) = study(&args);
        assert_eq!(code, Some(1), "{store:?}: {stderr}");
        assert!(stderr.contains("no longer reads history locally"), "{store:?}: {stderr}");
    }
}

/// `-h` and `--help` print the usage on stdout and exit 0.
#[test]
fn help_is_a_success_on_stdout() {
    for flag in ["-h", "--help"] {
        let (code, stdout, stderr) = study(&[flag]);
        assert_eq!(code, Some(0), "{flag}: {stderr}");
        assert!(stdout.contains("--study"), "{flag}: {stdout}");
    }
}

/// A command line missing a required flag is refused and the refusal names the flag.
#[test]
fn a_missing_required_flag_is_refused_and_named() {
    let (code, _, stderr) = study(&["--study", "s", "--recipe", "r.toml", "--from", "2026-01-01"]);
    assert!(code.is_some_and(|c| c != 0), "{stderr}");
    assert!(stderr.contains("--to"), "{stderr}");
}

/// ⚠ **The CHANGE on purpose (decision 0155): `--flag=value` is read.** The hand-rolled reader
/// matched only `--flag value`, so every one of these was "all required" before; the owner ruled
/// the inline spelling accepted on every program. The window is judged, so the line got past the
/// reader.
#[test]
fn the_inline_flag_spelling_is_read_now() {
    let (code, _, stderr) =
        study(&["--study=s", "--recipe=r.toml", "--from=2026-02-01", "--to=2026-01-01"]);
    assert_eq!(code, Some(1), "{stderr}");
    assert!(stderr.contains("--to must be after --from"), "{stderr}");
}

/// A repeated flag and an unknown one are refused (they were silently first-wins / ignored).
#[test]
fn a_repeated_or_unknown_flag_is_refused() {
    let base = ["--study", "s", "--recipe", "r.toml", "--from", "2026-01-01", "--to", "2026-02-01"];
    let mut repeated = base.to_vec();
    repeated.extend(["--study", "t"]);
    let (code, _, stderr) = study(&repeated);
    assert_eq!(code, Some(2), "{stderr}");
    let mut unknown = base.to_vec();
    unknown.push("--bogus");
    let (code, _, stderr) = study(&unknown);
    assert_eq!(code, Some(2), "{stderr}");
    assert!(stderr.contains("--bogus"), "{stderr}");
}
