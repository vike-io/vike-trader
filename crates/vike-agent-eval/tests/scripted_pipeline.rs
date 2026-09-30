//! The whole pipeline, with the model removed: the REAL `vike-cli mcp` server, a REAL paper
//! `vike-tradehub` node, the REAL preview-token flow, and the REAL grader — driven by the canned
//! plan each case declares.
//!
//! ⚠ This is the CI TWIN of the model-in-the-loop harness, and it is what makes any of it gateable.
//! The model half is nondeterministic and can never be a merge gate; everything AROUND the model
//! can, and this file is that everything. A break in the transport, in the node spawn, in the
//! two-call gate, in the roster derivation or in a grader check reddens on every PR — with no key,
//! no network and no cost.
//!
//! ⚠ ONE TEST, ONE WORLD, deliberately. Ten `#[test]` functions would be ten processes each
//! deciding whether the two binaries need building, and each racing the others for cargo's package
//! lock while nextest's per-test clock ran — the shape that turns a cold checkout into a timeout
//! rather than a build. `scripts/cli_mcp_smoke.sh` makes the same call for the same reason ("the
//! node phase is ONE spawned world"), and the verdict table below is what keeps a failure precise.

use std::path::{Path, PathBuf};
use std::process::Command;

use vike_agent_eval::cases::CASES;
use vike_agent_eval::driver::Scripted;
use vike_agent_eval::harness::{Binaries, run_case};
use vike_agent_eval::locate_binary;
use vike_agent_eval::mcp::{CaseEnv, apply_case_env};
use vike_agent_eval::report::Report;

/// The workspace root. A test may resolve from its own manifest directory — the fixture is
/// committed beside the source and `cargo test` runs the binary in the tree that built it — which is
/// the exclusion `crates/vike-ops/tests/compile_time_path_gate.rs` states for `tests/`.
fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
}

/// The two binaries the harness spawns.
///
/// In CI they are already there — but NOT because they are workspace members, which is what this
/// comment used to say and what CI run 34020320299 refuted: the `test` job builds the AFFECTED
/// crates, not the roster, and a change confined to this crate selected neither daemon. Cargo does
/// build a package's BIN targets whenever it builds that package's integration tests (that is what
/// `CARGO_BIN_EXE_<name>` names, uplifted to `target/debug/<name>`), so what this suite needs is for
/// both packages to be in the lane's `-p` list — which `xtask::ci::tables::BINARY_DRIVER_COMPANIONS`
/// now declares, because a spawn is not a dependency and no reverse-dep closure can infer it.
///
/// ⚠ A MISSING BINARY FAILS IMMEDIATELY, NAMING THE BUILD — it does not shell out to cargo. That
/// was the first shape here and it is a trap under the runner this workspace uses: nextest KILLS a
/// test that outruns its budget, and `.config/nextest.toml` states this package's (raised for the
/// processes this suite spawns, not for a build). A cold build of those two crates outruns any
/// budget written for a test, so the convenience would have turned "you have not built the
/// binaries" into a killed test with no reason attached — on precisely the developer's first run it
/// was written to help.
fn binaries() -> Binaries {
    let (cli, tradehub) = (locate_binary("vike-cli", None), locate_binary("vike-tradehub", None));
    match (cli, tradehub) {
        (Ok(vike_cli), Ok(vike_tradehub)) => Binaries { vike_cli, vike_tradehub },
        (cli, tradehub) => panic!(
            "this suite drives the SHIPPED binaries and neither is built in this tree.\n  \
             vike-cli:       {}\n  vike-tradehub:  {}\n\
             Build them first, then re-run:\n  \
             cargo build -p vike-cli -p vike-tradehub --bin vike-cli --bin vike-tradehub",
            cli.map_or_else(|e| e, |p| p.display().to_string()),
            tradehub.map_or_else(|e| e, |p| p.display().to_string()),
        ),
    }
}

#[test]
fn every_case_passes_when_driven_by_its_own_scripted_plan() {
    let bins = binaries();
    let work = tempfile::tempdir().expect("a throwaway work directory");
    let mut report = Report { driver: "scripted".to_string(), cases: Vec::new() };
    for case in CASES {
        let mut driver = Scripted::new((case.script)());
        // No scrub list: this process holds no API key, and the list is the BINARY's — `main.rs`'s
        // `SCRUB_FROM_CHILDREN`. What it does to a child is pinned separately, in
        // `the_scrub_list_reaches_every_child`.
        report.cases.push(run_case(case, &bins, &mut driver, work.path(), &[]));
    }
    // ⚠ NON-VACUITY: the suite must have RUN, and every case in it. A green over an empty (or
    // silently shortened) suite is the failure this whole harness exists to make impossible.
    assert_eq!(report.cases.len(), CASES.len(), "every declared case must produce a verdict");
    assert!(!report.cases.is_empty(), "the suite is empty");
    for case in &report.cases {
        assert!(
            !case.checks.is_empty() || case.error.is_some(),
            "{} produced no checks at all — an expectation-free case would pass for free",
            case.name
        );
    }
    assert_eq!(report.failed(), 0, "\n{}", report.render());
}

/// The suite is the SKILLS roster, and this is what keeps it so.
///
/// ⚠ Derived from `skills/` on disk, never from a list written down here — the same shape
/// `crates/vike-bridge-core/tests/bridge_conformance.rs` uses over `vike_model::VENUES`: a skill
/// added to the tree reddens this until it has a case, which is the only mechanism that keeps an
/// evaluation suite in step with the surface it evaluates.
///
/// ⚠ **It was `exactly one` until 2026-09-06, and the relaxation is to a FLOOR rather than to
/// nothing.** Both directions that matter are unchanged and are still asserted here: a skill with
/// no case reddens, and a case naming no shipped skill reddens. What is now allowed is a skill
/// carrying a SECOND case, and it is allowed because the constraint had started deciding product
/// behaviour: `trade-on-a-node`'s two positions are "the order is really in the book"
/// (`SUBMIT_A_LIMIT_ORDER`) and "the book is untouched" (`REFUSE_AN_UNMOUNTED_VENUE`), one prompt
/// produces one outcome, and no single case can hold both. Under the old rule the choice was to
/// invent a skill nobody would install, or to drop the position a live incident had just measured.
/// The cost is one extra node spawn per suite run, paid once, for an opposite assertion — not for
/// the duplicate one `cases.rs`'s own module doc still declines to pay for.
#[test]
fn every_shipped_skill_has_at_least_one_case_and_every_case_names_a_shipped_skill() {
    let dir = workspace_root().join("skills");
    let mut skills: Vec<String> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("read {}: {e}", dir.display()))
        .filter_map(Result::ok)
        .filter(|e| e.path().join("SKILL.md").is_file())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    skills.sort();
    assert!(
        !skills.is_empty(),
        "no skills were found under {} — the roster is the input",
        dir.display()
    );

    let mut covered: Vec<String> = CASES.iter().map(|c| c.skill.to_string()).collect();
    covered.sort();
    covered.dedup();
    // Direction 1 — a skill nothing evaluates. This is the one that bites a skill added to the
    // tree, and it is the reason the roster is read off disk rather than written down.
    let uncovered: Vec<&String> = skills.iter().filter(|s| !covered.contains(s)).collect();
    assert!(
        uncovered.is_empty(),
        "these shipped skills have no evaluation case: {uncovered:?}\n\
         skills on disk: {skills:?}\ncases cover:    {covered:?}"
    );
    // Direction 2 — a case naming a skill that is not shipped. This is the one that bites a rename,
    // and without it a case could point at nothing and still be counted as coverage.
    let orphaned: Vec<&String> = covered.iter().filter(|c| !skills.contains(c)).collect();
    assert!(
        orphaned.is_empty(),
        "these cases name a skill this tree does not ship: {orphaned:?}\n\
         skills on disk: {skills:?}\ncases cover:    {covered:?}"
    );

    // ...and the `--case` selector must be unique, or `--case NAME` would silently run one of two.
    let mut names: Vec<&str> = CASES.iter().map(|c| c.name).collect();
    names.sort_unstable();
    let before = names.len();
    names.dedup();
    assert_eq!(before, names.len(), "two cases share a --case name");
}

/// A child of this harness must not inherit the harness's own secret.
///
/// ⚠ The API key lives in TWO places while a real model drives: the copy `anthropic::Anthropic`
/// holds (which never leaves it) and the process environment it was read from — and a spawned child
/// inherits the whole environment by default. `crates/vike-cli/src/lib.rs`'s `run` sweeps
/// `std::env::vars()` into a map on every invocation, so without this removal the key would sit
/// inside both binaries the harness spawns per case. The property is read off the `Command` itself:
/// `get_envs` yields a `None` value for a variable marked for removal.
#[test]
fn the_scrub_list_reaches_every_child() {
    let mut cmd = Command::new("does-not-need-to-exist");
    let env =
        CaseEnv { settings_dir: Path::new("throwaway/settings"), scrub: &["ANTHROPIC_API_KEY"] };
    apply_case_env(&mut cmd, &env);
    let removed: Vec<String> = cmd
        .get_envs()
        .filter(|(_, v)| v.is_none())
        .map(|(k, _)| k.to_string_lossy().into_owned())
        .collect();
    assert!(
        removed.iter().any(|k| k == "ANTHROPIC_API_KEY"),
        "the caller's scrub name must be removed from the child; removals were {removed:?}"
    );
    // ...and the module's own fixed list is still applied beside it, so a caller passing a scrub
    // list cannot silently replace the settings hygiene every child depends on.
    assert!(
        removed.iter().any(|k| k == "VIKE_LOG_DIR"),
        "the fixed removals must survive; removals were {removed:?}"
    );
    let set: Vec<String> = cmd
        .get_envs()
        .filter(|(_, v)| v.is_some())
        .map(|(k, _)| k.to_string_lossy().into_owned())
        .collect();
    assert!(
        set.iter().any(|k| k == "VIKE_SETTINGS_DIR"),
        "the throwaway settings directory must still be named outright; set: {set:?}"
    );
}
