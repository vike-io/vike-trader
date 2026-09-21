//! The `ETXTBSY` retry's own proof — `tests/common/mod.rs`'s `output_retrying_etxtbsy`.
//!
//! A retry is the one cure that can be WORSE than the flake it treats: a predicate drawn too wide
//! turns a genuinely missing binary into a timeout and then reports it as somebody else's race, and
//! `data_cli.rs`'s `a_missing_engine_is_the_connect_rung` — which exists to prove that exact
//! failure reaches the operator — would be proving nothing. So the predicate's NARROWNESS is not a
//! remark in a doc comment; it is the property this file measures.
//!
//! What is held here:
//!
//! * the cross-process needle really is the errno `std` renders, and `ENOENT` is not a prefix of it
//!   ([`the_needle_is_the_errno_std_renders_and_enoent_is_not_a_prefix_of_it`]);
//! * a missing binary fails on the FIRST attempt, in a fraction of the retry budget
//!   ([`a_missing_binary_fails_promptly_and_is_never_retried`]);
//! * an ordinary non-zero exit is an ANSWER, returned untouched and just as promptly
//!   ([`an_ordinary_failure_is_an_answer_not_a_race`]);
//! * the retry genuinely happens, and a PERSISTENT `ETXTBSY` is louder than before rather than
//!   quietly returned ([`a_persistent_busy_is_retried_and_then_panics_rather_than_being_returned`]);
//! * the happy path still works ([`a_planted_engine_runs_and_its_output_comes_back`]);
//! * and "a new case joins the cure by construction" is a CHECK rather than a hope
//!   ([`a_case_that_plants_an_engine_may_not_spawn_the_cli_itself`]).
//!
//! ⚠ The timing assertions are FRACTIONS of `common::retry_budget()`, never wall-clock numbers, so
//! tuning `ATTEMPTS` or `FIRST_BACKOFF` cannot silently make "promptly" mean "after a second". They
//! are also one-sided in the safe direction: `thread::sleep` may overshoot under load and never
//! returns early, so a "did retry" LOWER bound cannot flake low, and an "was prompt" UPPER bound is
//! a quarter of a budget that no non-retrying path spends any of.
//!
//! ⚠ The whole file is unix-gated because `ETXTBSY` is a POSIX rule about a file open for write and
//! Windows has no equivalent for `Command::new` to hit. That costs nothing: no Windows TEST runs
//! anywhere in this repo — the Windows witness is a `cargo check`, which this file still gets.
#![cfg(unix)]

mod common;

use std::path::{Path, PathBuf};
use std::process::Command;

// ── the predicate ───────────────────────────────────────────────────────────────────────────────

/// The needle `common::child_reported_etxtbsy` matches is re-derived from `std` itself on every
/// run rather than trusted as a comment: face 2 can only ever read an errno out of TEXT, so if
/// std's rendering stopped ending in `(os error <n>)` the retry would silently stop covering the
/// half of the sites where `vike-cli` — not this process — is the one that execs.
///
/// The second half is the trap that makes this worth a test at all. `(os error 2` is a PREFIX of
/// `(os error 26)`, and errno 2 is `ENOENT`: a MISSING engine, which several cases deliberately
/// produce and assert the connect rung on. A needle that dropped the closing paren would retry
/// every one of them through the whole budget and then panic — turning this crate's most
/// deterministic assertions into its slowest flake.
#[test]
fn the_needle_is_the_errno_std_renders_and_enoent_is_not_a_prefix_of_it() {
    let busy = std::io::Error::from_raw_os_error(common::ETXTBSY).to_string();
    assert!(
        busy.contains(&common::etxtbsy_needle()),
        "std renders errno {} as {busy:?}, which no longer carries the needle {:?} — the child's \
         own diagnostic is the ONLY way this errno crosses a process boundary here, so this \
         breaking means the retry silently covers just the sites this process spawns",
        common::ETXTBSY,
        common::etxtbsy_needle(),
    );

    // ENOENT — a missing engine, the failure the connect-rung cases prove.
    let missing = std::io::Error::from_raw_os_error(2).to_string();
    assert!(
        !missing.contains(&common::etxtbsy_needle()),
        "a MISSING binary ({missing:?}) must not match the busy needle {:?}: it is a real failure, \
         and retrying it would replace an instant, correct diagnostic with a budget-long panic",
        common::etxtbsy_needle(),
    );
}

/// A genuinely missing binary fails on attempt ONE — the property that keeps this helper from being
/// worse than the flake.
///
/// The spawn errors with `ENOENT`, which the predicate does not match, so the helper panics
/// immediately and says so in as many words. Both halves are asserted: WHAT it said (so a future
/// widening of the predicate cannot pass this by panicking for the other reason) and HOW LONG it
/// took (so it cannot pass by retrying to exhaustion and panicking at the end).
#[test]
fn a_missing_binary_fails_promptly_and_is_never_retried() {
    let scratch = tempfile::tempdir().expect("tempdir");
    let absent = scratch.path().join("no-such-program");
    assert!(!absent.exists(), "the fixture must genuinely not be there");

    let started = std::time::Instant::now();
    let panicked = catch_panic(|| {
        common::output_retrying_etxtbsy("a missing program", || Command::new(&absent));
    });
    let elapsed = started.elapsed();

    assert!(
        panicked.contains("NOT the ETXTBSY"),
        "the panic must say the failure was NOT the race, so nobody reads it as this flake: \
         {panicked}"
    );
    assert!(
        panicked.contains("Nothing was retried"),
        "…and must say no retry happened: {panicked}"
    );

    assert!(
        elapsed < common::retry_budget() / 4,
        "a missing binary must fail promptly: took {elapsed:?}, and one pass through the retry \
         budget is {:?}. Anything near the budget means the predicate matched ENOENT and this \
         helper is now hiding real failures behind a wait.",
        common::retry_budget(),
    );
}

/// An ordinary non-zero exit is an ANSWER. The helper must hand it back on the first attempt with
/// its streams untouched — most cases in `data_cli.rs`/`backtest_cli.rs` assert on exactly this
/// shape (an engine that exits 2, an unclassified code folded to 1), and a helper that retried
/// FAILURES rather than the race would multiply every one of them by the attempt cap.
#[test]
fn an_ordinary_failure_is_an_answer_not_a_race() {
    let scratch = tempfile::tempdir().expect("tempdir");
    let engine = common::plant_engine(
        scratch.path(),
        "fails-honestly",
        "#!/bin/sh\necho 'a real diagnostic' >&2\nexit 2\n",
    );

    let started = std::time::Instant::now();
    let out = common::output_retrying_etxtbsy("an honest failure", || Command::new(engine.path()));
    let elapsed = started.elapsed();

    assert_eq!(out.status.code(), Some(2), "the child's own code comes back");
    assert_eq!(
        String::from_utf8_lossy(&out.stderr).trim(),
        "a real diagnostic",
        "…and its stderr verbatim"
    );
    assert!(
        elapsed < common::retry_budget() / 4,
        "an ordinary failure must not be retried: took {elapsed:?} against a budget of {:?}",
        common::retry_budget(),
    );
}

/// The retry really retries, and a PERSISTENT busy is loud.
///
/// A stand-in prints the face-2 marker on every run, so the helper can never see a clean answer.
/// Two things must then be true, and together they make a persistent descriptor leak MORE visible
/// than it was before this helper existed rather than less:
///
/// * it PANICS rather than returning the busy outcome as though it were the command's answer — a
///   silent return is how a real leak would get mistaken for a flake and waited out forever;
/// * it spent the backoff getting there, which is the only positive evidence in this file that the
///   retry path is reached at all. Every other case here proves the helper NOT retrying.
#[test]
fn a_persistent_busy_is_retried_and_then_panics_rather_than_being_returned() {
    let scratch = tempfile::tempdir().expect("tempdir");
    // The shape `crates/vike-cli/src/cmd/engine.rs`'s `cannot_spawn` emits when the engine it was
    // pointed at cannot be exec'd — the `io::Error` rendered with `Display`, errno and all.
    let script = format!(
        "#!/bin/sh\necho 'cannot run the backtest engine (/x): Text file busy {}' >&2\nexit 3\n",
        common::etxtbsy_needle(),
    );
    let engine = common::plant_engine(scratch.path(), "always-busy", &script);
    let path = engine.path().to_path_buf();

    let started = std::time::Instant::now();
    let panicked = catch_panic(move || {
        common::output_retrying_etxtbsy("a persistent busy", || Command::new(&path));
    });
    let elapsed = started.elapsed();

    assert!(
        panicked.contains("still ETXTBSY after"),
        "a persistent busy must PANIC, not come back as an answer: {panicked}"
    );
    assert!(
        panicked.contains("descriptor genuinely held open for write"),
        "…and must point at the real diagnosis rather than at this flake: {panicked}"
    );
    assert!(
        panicked.contains(&format!("attempt {}", common::ATTEMPTS)),
        "…carrying every attempt it made, the last one included: {panicked}"
    );
    assert!(
        elapsed >= common::retry_budget(),
        "it must actually have slept the budget ({:?}) before giving up; took {elapsed:?}. \
         `thread::sleep` never returns early, so a value under the budget means attempts were \
         SKIPPED rather than that the box was fast",
        common::retry_budget(),
    );
}

/// The happy path, so the proofs above cannot all be passing on a helper that never returns an
/// answer at all.
#[test]
fn a_planted_engine_runs_and_its_output_comes_back() {
    let scratch = tempfile::tempdir().expect("tempdir");
    let engine = common::plant_engine(scratch.path(), "echoes", "#!/bin/sh\necho \"argv: $*\"\n");

    let out = common::output_retrying_etxtbsy("the happy path", || {
        let mut c = Command::new(engine.path());
        c.arg("--one").arg("--two");
        c
    });

    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "argv: --one --two");
    assert!(
        engine.arg().ends_with("echoes"),
        "`arg()` is the utf-8 path the cases pass to `--engine`: {}",
        engine.arg()
    );
}

// ── "by construction", as a check ───────────────────────────────────────────────────────────────

/// The test files whose planted-engine cases must go through a retrying runner.
///
/// ⚠ Keyed on BASENAME, resolved under this crate's own `tests/` — deliberately, because a
/// repo-root-relative string would key this gate on `crates/vike-cli/` as well, and a path-keyed
/// row is the thing that reddens a working gate twice over when a file moves (a stale row here, an
/// unscanned file there). A RENAME of either basename still reddens this, and the read below says
/// so in its own failure message rather than leaving the next author to work it out.
const SUBJECTS: [&str; 2] = ["data_cli.rs", "backtest_cli.rs"];

fn tests_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests")
}

/// **"By construction" is enforced here, not merely hoped for.**
///
/// `backtest_cli.rs`'s DOMINANT idiom is a bare `Command::new(env!("CARGO_BIN_EXE_vike-cli"))` —
/// more than a dozen of them, every one legitimate (they deliberately run with the ambient
/// environment, which several of them are asserting about) and none of them touching a planted
/// engine. A new planted-engine case that copied that idiom instead of the file's own runner would
/// silently opt out of the retry, which is precisely how two cases came to be uncured on
/// 2026-09-14 while `CHANGELOG.md`'s `[0.1.10]` had already diagnosed the race a release earlier.
///
/// So the rule is a check: a top-level fn that PLANTS an engine may not spawn `vike-cli` itself.
/// Both files reach the shipped binary through one runner, and both runners retry.
#[test]
fn a_case_that_plants_an_engine_may_not_spawn_the_cli_itself() {
    let mut offenders = Vec::new();
    for name in SUBJECTS {
        let path = tests_dir().join(name);
        let src = std::fs::read_to_string(&path).unwrap_or_else(|e| {
            panic!(
                "read {}: {e}\nIf this file was RENAMED, re-key `SUBJECTS` — a planted-engine case \
                 in an unscanned file is exactly what this gate exists to catch.",
                path.display()
            )
        });
        offenders.extend(offenders_in(name, &src));
    }

    assert!(
        offenders.is_empty(),
        "these cases plant an executable and then spawn `vike-cli` directly, so the ETXTBSY retry \
         never sees them:\n  {}\nUse the file's own runner (`run` / `run_cli`), which goes through \
         `common::output_retrying_etxtbsy`.",
        offenders.join("\n  ")
    );
}

/// Every fn in `src` that calls the shared plant AND spawns the shipped binary itself.
///
/// Comment lines are stripped first: chunking on top-level `fn` carries the NEXT item's doc comment
/// into the previous item's chunk, so a doc that merely mentions either needle would otherwise be
/// read as code. This is a text scan and says so — it recognises a top-level `fn` by column zero,
/// which is what `cargo fmt` (CI's first gate) guarantees for an item and never for a nested one.
///
/// ⚠ The LEADING newline is not cosmetic. Splitting on `"\nfn "` cannot see a `fn` at byte zero, so
/// a file (or a fixture) whose very first line is one keeps that item fused to the preamble chunk
/// and renders its NAME with the `fn ` still attached. Caught by
/// [`the_gate_bites_on_a_planted_case_that_spawns_directly`] on its first run against a real
/// compiler, which is the whole reason that proof exists: the gate itself was green either way,
/// because no file in the tree today starts with a `fn`.
fn offenders_in(file: &str, src: &str) -> Vec<String> {
    const PLANT: &str = "common::plant_engine(";
    const RAW_SPAWN: &str = "Command::new(env!(\"CARGO_BIN_EXE_vike-cli\"))";

    let stripped: Vec<&str> = src.lines().filter(|l| !l.trim_start().starts_with("//")).collect();
    let code = format!("\n{}", stripped.join("\n"));

    code.split("\nfn ")
        .filter(|item| item.contains(PLANT) && item.contains(RAW_SPAWN))
        .map(|item| {
            let name = item.split('(').next().unwrap_or("<unnamed>").trim();
            format!("{file}'s `{name}`")
        })
        .collect()
}

/// The gate above can actually BITE — proven on a planted source string rather than by trusting
/// that it would, because a scanner whose needles no longer match anything passes forever.
///
/// Both directions are measured on the same fixture shape: the offending spelling is caught, the
/// correct one is not, and a doc comment that merely NAMES both needles is not mistaken for code.
#[test]
fn the_gate_bites_on_a_planted_case_that_spawns_directly() {
    let offending = "\
fn a_case_that_opts_out() {
    let e = common::plant_engine(d, \"backtest\", S);
    let out = Command::new(env!(\"CARGO_BIN_EXE_vike-cli\")).arg(e.arg()).output();
}
";
    assert_eq!(
        offenders_in("fixture.rs", offending),
        vec!["fixture.rs's `a_case_that_opts_out`".to_string()],
        "the gate must catch a planted case that spawns the CLI itself"
    );

    let correct = "\
fn a_case_that_uses_the_runner() {
    let e = common::plant_engine(d, \"backtest\", S);
    let out = run_cli(&settings, &[\"backtest\", \"run\", \"--engine\", e.arg()]);
}
";
    assert!(
        offenders_in("fixture.rs", correct).is_empty(),
        "a planted case going through the retrying runner is not an offender"
    );

    let only_in_a_doc = "\
/// Mentions common::plant_engine( and Command::new(env!(\"CARGO_BIN_EXE_vike-cli\")) in prose.
fn a_case_that_only_talks_about_them() {
    let out = run_cli(&settings, &[\"backtest\"]);
}
";
    assert!(
        offenders_in("fixture.rs", only_in_a_doc).is_empty(),
        "a doc comment naming both needles is prose, not a site — chunking on top-level `fn` \
         carries the next item's doc into the previous item's chunk, so this is the false positive \
         the comment strip exists to prevent"
    );
}

// ── plumbing ────────────────────────────────────────────────────────────────────────────────────

/// Run `f`, expecting it to panic, and return the panic's message.
///
/// The default hook is muted for the duration: these cases panic ON PURPOSE, and a run whose output
/// carries two unexplained backtraces teaches the next reader to ignore backtraces.
fn catch_panic(f: impl FnOnce() + std::panic::UnwindSafe) -> String {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let caught = std::panic::catch_unwind(f);
    std::panic::set_hook(previous);

    let payload = caught.expect_err("the call was supposed to panic and did not");
    if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else if let Some(s) = payload.downcast_ref::<&str>() {
        (*s).to_string()
    } else {
        panic!("the panic payload was neither a String nor a &str");
    }
}
