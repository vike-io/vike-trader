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
//! ⚠ ONE TEST, deliberately — and since 2026-10-03, its cases run CONCURRENTLY inside it. Every
//! case already stood up a world of its own (`crates/vike-agent-eval/src/harness.rs`'s `run_case`:
//! its own project folder, its own node on its own port, its own MCP server), so nothing ordered
//! them but the `for` loop — and run one after another they made this the longest single test in
//! CI: MEASURED 55.7 s on a the latency box lane and 56.3 s in CI, of which the six node-bearing cases were
//! 55 s (4.4-12.4 s each; the other nine take ~0.06 s). Run together, the test costs about its
//! slowest case. Two things keep that honest, and both are why this stays ONE test rather than
//! sixteen:
//!
//!   * **Ports.** A case's node port and its dead-datahub port read free to a probe until somebody
//!     binds them — so two concurrent cases handed one port would have one case's MCP server
//!     dialling the other's daemon, refused at the handshake (each case's keys are minted into its
//!     own store) with an auth denial that says nothing about ports, or a "dead" datahub port that
//!     is another case's live node. `crates/vike-agent-eval/src/node.rs`'s `pick_port` keeps a
//!     per-PROCESS ledger of what it has handed out, which is exactly the guarantee this needs and
//!     exactly why the cases must share a process: sixteen nextest tests are sixteen processes, and
//!     no ledger reaches across them. [`concurrent_picks_never_share_a_port`] is its proof.
//!   * **Verdicts.** Each case's verdict comes back from its own thread and is gathered in `CASES`
//!     order, so the report reads exactly as the sequential one did and a failure still names its
//!     case in the table below.
//!
//! (The original reason for one test — ten processes each building the two binaries and racing for
//! cargo's package lock — went away when [`binaries`] stopped building and started failing fast
//! with the build command instead.) `scripts/cli_mcp_smoke.sh` keeps its node phase as one spawned
//! world for its own reasons.

use std::path::{Path, PathBuf};
use std::process::Command;

use vike_agent_eval::cases::CASES;
use vike_agent_eval::driver::Scripted;
use vike_agent_eval::harness::{Binaries, run_case};
use vike_agent_eval::locate_binary;
use vike_agent_eval::mcp::{CaseEnv, apply_case_env};
use vike_agent_eval::node::{NODE_KEY_NAMES, pick_port};
use vike_agent_eval::report::{CaseVerdict, Report};

// Same spelling as `crates/vike-ops/tests/common/repo.rs`'s `workspace_root` (keeps the `..`); the
// `parent()` twins, e.g. `crates/vike-catalog/tests/baseline_artifact.rs`'s `repo_root`, do not.
/// The workspace root. A test may resolve from its own manifest directory — the fixture is
/// committed beside the source and `cargo test` runs the binary in the tree that built it — which is
/// the exclusion `crates/vike-ops/tests/hygiene/compile_time_path_gate.rs` states for `tests/`.
fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
}

/// The two binaries the harness spawns.
///
/// In CI they are already there, because cargo builds a package's BIN targets whenever it builds
/// that package's integration tests (that is what `CARGO_BIN_EXE_<name>` names, uplifted to
/// `target/debug/<name>`) and the `test` job's `-p` list names both packages. ⚠ This read "because
/// they are workspace members" until CI run 34020320299 refuted it: the job then BUILT only the
/// affected crates, a change confined to this crate selected neither daemon, and
/// `xtask::ci::tables::roster::BINARY_DRIVER_COMPANIONS` was written to put them in the list. The job now
/// builds the WHOLE roster (`xtask::ci::plan::Plan::roster`) and only RUNS the affected crates' tests, so
/// both binaries are built on every run (measured — that table's doc carries it), and the table no
/// longer adds anything to the lane: their own tests do not run on a change confined to this crate.
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
    // Every case on its own thread, every verdict joined back in `CASES` order — see the module
    // doc for why concurrency is safe here and why it stays inside this one test.
    let cases: Vec<CaseVerdict> = std::thread::scope(|scope| {
        let running: Vec<_> = CASES
            .iter()
            .map(|case| {
                let (bins, work) = (&bins, work.path());
                scope.spawn(move || {
                    let mut driver = Scripted::new((case.script)());
                    // No scrub list: this process holds no API key, and the list is the BINARY's —
                    // `main.rs`'s `SCRUB_FROM_CHILDREN`. What it does to a child is pinned
                    // separately, in `the_scrub_list_reaches_every_child`.
                    run_case(case, bins, &mut driver, work, &[])
                })
            })
            .collect();
        running
            .into_iter()
            .zip(CASES)
            .map(|(thread, case)| {
                thread.join().unwrap_or_else(|_| {
                    panic!("the harness thread running case {} panicked", case.name)
                })
            })
            .collect()
    });
    let report = Report { driver: "scripted".to_string(), cases };
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

/// **Concurrent cases are never handed one port** — the property that makes running
/// [`every_case_passes_when_driven_by_its_own_scripted_plan`]'s cases together safe.
///
/// A thousand picks from eight threads at once must all differ. The probe alone cannot promise
/// that: nothing here binds what it picks, so every port stays "free" to every later probe, and a
/// thousand draws from the ~12,700 ports below Linux's default ephemeral floor repeat ~39 times by
/// the birthday arithmetic. Only `pick_port`'s in-process ledger makes them distinct — remove it
/// (`crates/vike-agent-eval/src/node.rs`'s `handed_out`) and this goes red; that mutation was RUN.
#[test]
fn concurrent_picks_never_share_a_port() {
    const THREADS: usize = 8;
    const PICKS: usize = 125;
    let picked: Vec<u16> = std::thread::scope(|scope| {
        let pickers: Vec<_> = (0..THREADS)
            .map(|_| {
                scope.spawn(|| {
                    (0..PICKS)
                        .map(|_| pick_port().expect("a free loopback port"))
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        pickers.into_iter().flat_map(|t| t.join().expect("a picker thread")).collect()
    });
    assert_eq!(picked.len(), THREADS * PICKS, "every pick must return a port");
    let mut distinct = picked.clone();
    distinct.sort_unstable();
    distinct.dedup();
    assert_eq!(
        distinct.len(),
        picked.len(),
        "{} of {} picks repeated a port this process had already handed out — two concurrent \
         cases would share a node or point a dead datahub at a live one",
        picked.len() - distinct.len(),
        picked.len()
    );
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
/// inherits the whole environment by default. `crates/vike-cli/src/dispatch.rs`'s `run` sweeps
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
    // ...and no node key reaches a child from the environment, in either direction: an inherited
    // one is REMOVED (it would beat the pair `backend setup` minted into the store), and none is
    // SET (the clients read the store, the same rows the daemon reads).
    for name in NODE_KEY_NAMES {
        assert!(
            removed.iter().any(|k| k == name),
            "{name} must be removed from every child; removals were {removed:?}"
        );
        assert!(
            !set.iter().any(|k| k == name),
            "{name} must not be handed to a child; set: {set:?}"
        );
    }
}
