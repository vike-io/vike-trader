//! `backtest --optimizer grid|euler|tpe|genetic` — the argv triage gate for the parameter-search
//! flags.
//!
//! Ruling 13 of `docs/superpowers/specs/2026-09-09-optimizer-trait-design.md` refused a separate
//! `optimize` verb: the search flags stay on `backtest` and the word "optimizer" lives in the FLAG.
//! Ruling 14 made the profile POSITIONAL. This file is the shipped-binary half of both, and of the
//! four defects the hand-written flag ladder carried — every one of which exited **0**, which is
//! why nothing here can be gated on an exit code alone.
//!
//! What the ladder did, measured on `origin/main` before this file existed:
//!
//! * **(a)** `--optimizer tpe --search bogus` ran tpe and exited 0. The `--optimizer` arm returned
//!   before `--search` was ever parsed, so a bogus searcher name was accepted.
//! * **(b)** `--search euler --trials 0` exited 0, ignoring a `0` the tpe arm treats as fatal. One
//!   flag value, two opposite fates, decided by a flag the operator may not have connected to it.
//! * **(c)** `--search grid --euler-depth 99` exited 0 silently — a flag meaningful only to euler,
//!   accepted and discarded by the grid. So did `--euler-depth abc`: the malformed-value check sat
//!   inside the branch that was not taken.
//! * **(d)** `--optimizer=tpe` **ran the grid**, printed a full ranked report and exited 0.
//!   `crates/vike-analytics/src/binutil.rs`'s `arg` matched an exact token only, so the inline
//!   spelling matched nothing at all. The worst of the four: not a refusal, a DIFFERENT ANSWER.
//! * **(e)**, not in the original list and found while reading: every one of these flags sat inside
//!   `if profile.is_paramscan()`, so `backtest run.toml --optimizer tpe --trials 500` ran ONE ordinary
//!   backtest and exited 0 — 500 trials of Bayesian search asked for, one run delivered, no
//!   diagnostic.
//!
//! ⚠ **Most tests here need no store and no profile**, and that is a PROPERTY rather than a
//! convenience: argv is triaged before any history is asked for, so a mistyped flag costs no
//! datahub round trip. `a_refused_flag_never_dials_the_datahub` is what makes that assertable rather
//! than assumed.
//!
//! ⚠ **The tests that RUN a search serve their scratch store through a real datahub**
//! (`tests/support/local_datahub.rs`), because the binary refuses `--store DIR` since the owner
//! closed the local READ door on 2026-09-25 (decision 0084). Every OTHER invocation is pointed at
//! [`NO_DATAHUB`], an address nothing listens on, so a test that reached for history by accident
//! fails loudly instead of reading whatever answers on the default `127.0.0.1:7878` — which on a
//! the CI box lane is the box's REAL datahub.
//!
//! ⚠ **Every child runs in a scratch PROJECT of its own, never in the checkout**
//! (`tests/support/engine.rs`, the one spawn this crate's tests may use). The engine saves each
//! finished run and search under the project its working directory resolves, and this file's
//! children inherited `cargo test`'s — so for as long as nothing pinned it, every run here was saved
//! into the checkout it was built in.
//!
//! ⚠ **One test here is the WIDENING'S OWN AUDIT and reaches past the search flags.** Fixing
//! (d) meant teaching `arg` the `--flag=value` spelling, and that is not the pure widening it looks
//! like: for a flag whose ABSENCE carries meaning — a wildcard dimension, a lower resolver rung, a
//! required-flag refusal — a `--flag=` token that every caller previously IGNORED now answers
//! `Some("")` and changes the answer. `--store=` was caught while the fix was written (it would
//! have minted a store in the working directory); `--produced-by=` on the IRREVERSIBLE `data rm`
//! verb was not, and it is the one that deletes data. Hence
//! `a_blank_produced_by_is_refused_rather_than_asserting_nothing`, in this file rather than a new
//! one, because the hazard belongs to the widening rather than to that verb.
//!
//! ⚠ **The last section is the FOURTH method, and it tests a rule the first four defects never
//! had to express.** `--seed` is the first flag in [`METHOD_FLAGS`]-land with more than one owner
//! (tpe AND genetic), so the ownership table that makes those defects stay fixed had to widen
//! without weakening: a single-owner flag must still refuse exactly as it did, and a two-owner one
//! must refuse everywhere outside its pair. Both directions are asserted there, and so is the one
//! place the two methods deliberately DIFFER — genetic refuses to run without a seed where tpe
//! defaults to 0.
//!
//! `#![cfg(feature = "datafusion-store")]` because the `backtest` bin carries
//! `required-features = ["datafusion-store"]`: without it the binary is not built and
//! `env!("CARGO_BIN_EXE_backtest")` would not COMPILE. That gate is also why this is its own test
//! binary rather than a member of a grouped binary (the shape `crates/vike-sim/tests/parity.rs`
//! is, which left this crate with the simulator) — `crates/vike-backtest/CLAUDE.md`'s grouping
//! rule. CI runs it in the `datafusion-store` lane (`scripts/ci_feature_suite.sh`).
#![cfg(feature = "datafusion-store")]

use std::io::ErrorKind;
use std::net::TcpListener;
use std::path::Path;
use std::process::Output;

#[path = "support/engine.rs"]
mod engine;
#[path = "support/local_datahub.rs"]
mod local_datahub;

// An address nothing listens on, handed to every run that names no hub of its own — see the module
// doc for why the default would be the wrong answer. Owned by `tests/support/engine.rs`, whose
// spawn dials it by default.
use engine::NO_DATAHUB;

fn run(args: &[&str]) -> Output {
    run_via(NO_DATAHUB, args)
}

/// [`run`], with the child's history read from the datahub at `hub`.
///
/// ⚠ **The child runs in a FRESH scratch project that is dropped when this returns**
/// (`tests/support/engine.rs`'s `Scratch`), so a run or a search it persists lands there and goes
/// with it. This spawned the engine bare until 2026-09-26: the child inherited `cargo test`'s
/// working directory, resolved the CHECKOUT as its project, and every lane checkout had filled with
/// thousands of this file's runs. [`run_via_persists_into_a_scratch_that_is_gone_when_it_returns`]
/// is what keeps that visible.
fn run_via(hub: &str, args: &[&str]) -> Output {
    let scratch = engine::Scratch::project();
    scratch.dial(hub);
    scratch.engine().args(args).output().unwrap_or_else(|e| panic!("run backtest {args:?}: {e}"))
}

/// A listener that serves NOTHING and only witnesses: a child pointed at it that connects leaves a
/// pending connection in its backlog, which [`assert_never_dialled`] then finds. The kernel
/// completes the handshake whether or not anything calls `accept`, so a dial cannot hide.
fn silent_hub() -> (TcpListener, String) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("a loopback port");
    listener.set_nonblocking(true).expect("a non-blocking witness");
    let addr = listener.local_addr().expect("the bound address").to_string();
    (listener, addr)
}

fn assert_never_dialled(witness: &TcpListener, what: &str) {
    match witness.accept() {
        Err(e) if e.kind() == ErrorKind::WouldBlock => {}
        Ok(_) => panic!("{what} must be refused before any history is asked for, but it DIALLED"),
        Err(e) => panic!("the witness listener failed: {e}"),
    }
}

fn stderr_of(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).to_string()
}

/// Assert a FLAG-LEVEL refusal: exit 2, nothing on stdout, no `USAGE` dump, and every needle
/// named on stderr.
///
/// ⚠ **The `usage:` assertion is the load-bearing one, and it was added because five of these
/// tests passed against the UNFIXED binary.** Exit 2 proves nothing here — a nonexistent profile
/// also exits 2 — and neither do the needles on their own: the old binary answered every argv
/// below with `--profile <path> is required` followed by the whole `USAGE` const, and that const
/// contains the literal strings `--optimizer`, `--search`, `--trials`, `--euler-depth`, `grid`,
/// `euler` and `tpe`. A `contains` check over a wall of help text is a test that cannot fail.
///
/// So the shape is also a DESIGN decision, stated here because this is where it is enforced: a
/// refusal that names ONE flag answers with one line naming that flag and the fix, never with the
/// help text. The `USAGE` dump stays on the two arms where the operator genuinely supplied nothing
/// to talk about — a bare invocation and a bad `--addr` — which is what
/// `crates/vike-backtest/tests/help_cli.rs`'s `a_missing_profile_still_exits_non_zero_on_stderr`
/// pins from the other side.
fn refuses(args: &[&str], needles: &[&str]) {
    refuses_via(NO_DATAHUB, args, needles);
}

/// [`refuses`], with the child pointed at `hub` — a [`silent_hub`] witness, in the tests whose
/// subject is that a refusal dials nothing.
fn refuses_via(hub: &str, args: &[&str], needles: &[&str]) {
    let out = run_via(hub, args);
    let stderr = stderr_of(&out);
    assert_eq!(
        out.status.code(),
        Some(2),
        "`backtest {args:?}` must exit 2 (a usage error); stderr: {stderr:?}"
    );
    assert!(
        out.stdout.is_empty(),
        "…and print nothing on stdout: {:?}",
        String::from_utf8_lossy(&out.stdout)
    );
    assert!(
        !stderr.contains("usage:"),
        "`backtest {args:?}` must answer with the ONE flag it is refusing, not with the whole \
         USAGE block — every needle below appears somewhere in that block, so a refusal that \
         dumps it makes this assertion unfalsifiable; stderr: {stderr:?}"
    );
    for needle in needles {
        assert!(
            stderr.contains(needle),
            "`backtest {args:?}` must name {needle:?} in its refusal; stderr: {stderr:?}"
        );
    }
}

/// ⚠ **[`run_via`]'s child persists into a SCRATCH project, and that scratch is gone by the time
/// [`run_via`] returns.** Asserted rather than assumed, because the failure it guards is SILENT: an
/// unpinned child resolves its project by walking up from `cargo test`'s working directory — this
/// crate's — finds the checkout, and saves every run into `<checkout>/user_data/runs` at exit 0, in
/// a test that passes. That is what this file did until 2026-09-26.
///
/// A single run over an empty store is the cheapest invocation that PERSISTS, and the engine names
/// where on stderr (`run saved to <dir>`), so this reads the answer off the child rather than
/// guessing at it. Two assertions about that directory — not under the checkout, and no longer on
/// disk — and a leaked run fails BOTH. Requiring the `saved to` line is what keeps the pair from
/// passing vacuously: a child that resolved no project at all would save nothing and print no path.
#[test]
fn run_via_persists_into_a_scratch_that_is_gone_when_it_returns() {
    let (_dir, profile, store) = scratch(SINGLE_PROFILE);
    let hub = local_datahub::serve(Path::new(&store));
    let out = run_via(&hub, &[&profile]);
    let stderr = stderr_of(&out);
    assert!(out.status.success(), "a single run over an empty store exits 0; stderr: {stderr:?}");

    let saved = stderr
        .lines()
        .find_map(|line| line.strip_prefix("backtest: run saved to "))
        .map(|dir| Path::new(dir.trim()).to_path_buf())
        .unwrap_or_else(|| {
            panic!(
                "the run must PERSIST and say where, or this test sees nothing; stderr: {stderr:?}"
            )
        });
    let checkout =
        std::fs::canonicalize(Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join(".."))
            .expect("the checkout this test was built in");
    assert!(
        !saved.starts_with(&checkout),
        "the engine saved a run INTO THE CHECKOUT ({}) — its working directory or its project \
         variables were not pinned to a scratch",
        saved.display()
    );
    assert!(
        !saved.exists(),
        "the run must have lived in run_via's own scratch project and gone with it, but {} is \
         still on disk",
        saved.display()
    );
}

/// A profile with a two-point numeric `[sweep]` grid over `buy_hold`. Numeric and
/// type-homogeneous, so euler and tpe both ACCEPT it — a non-numeric axis would make them refuse
/// for a reason that has nothing to do with what is under test.
const PARAMSCAN_PROFILE: &str = r#"
name = "optimizer-cli-fixture"

[data]
venue = "demo"
symbols = ["BTCUSDT"]
kind = "bar"
interval = "1h"
from = "2025-01-01T00"
to = "2025-07-01T00"

[engine]
cash = 10000.0

[strategy]
name = "buy_hold"
[strategy.params]
size = 1.0
symbol = "BTCUSDT"

[sweep]
size = [1.0, 2.0]
"#;

/// The same profile with its `[sweep]` table removed — a single-point run, for defect (e).
const SINGLE_PROFILE: &str = r#"
name = "optimizer-cli-single"

[data]
venue = "demo"
symbols = ["BTCUSDT"]
kind = "bar"
interval = "1h"
from = "2025-01-01T00"
to = "2025-07-01T00"

[engine]
cash = 10000.0

[strategy]
name = "buy_hold"
[strategy.params]
size = 1.0
symbol = "BTCUSDT"
"#;

/// A scratch directory holding one profile file, plus the store path beside it. `tempfile`, never a
/// fixed name under the system temp directory — `crates/vike-ops/tests/hygiene/temp_path_gate.rs` says why.
fn scratch(profile: &str) -> (tempfile::TempDir, String, String) {
    let dir = tempfile::tempdir().expect("a scratch directory");
    let path = dir.path().join("profile.toml");
    std::fs::write(&path, profile).expect("write the fixture profile");
    let store = dir.path().join("store");
    (
        dir,
        path.to_str().expect("a UTF-8 scratch path").to_string(),
        store.to_str().expect("a UTF-8 scratch path").to_string(),
    )
}

#[path = "optimizer_cli/data_export.rs"]
mod data_export;
#[path = "optimizer_cli/data_repair.rs"]
mod data_repair;
#[path = "optimizer_cli/flags.rs"]
mod flags;
#[path = "optimizer_cli/genetic.rs"]
mod genetic;
