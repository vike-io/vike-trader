//! The Research pane and the study surface: the host gate, a real study run's own result surface,
//! and a refusal disclosed as a study failure. Sibling tests named in doc links below live in `panes`.

use std::assert_matches;
use std::time::Duration;

use vike_studio::{
    ChatApiKeys, NO_HOST, RightTab, STUDY_METRICS_NOTE, STUDY_RESULT_TITLE, STUDY_RUN_KIND,
    StudyHost,
};

use crate::common::{seeded_store, state};
use crate::support::{all_text, harness_over, has_button, is_disabled, run_button, settle, shell};

// ======================= the Research pane and the study surface =======================

/// One rhai study, written into a throwaway `user_data/` tree the way a user's own project holds
/// one: `research/studies/rhai/<name>/<name>.rhai`.
///
/// The tier is deliberately the INTERPRETED one and not by preference: the COMPILED tier resolves
/// through a registry `crates/vike-user-research`'s `build.rs` generates by scanning
/// `<workspace>/user_data/research/studies/rust/` at BUILD time, which in every CI checkout is
/// absent — so a compiled study cannot be planted by a test at all, and the interpreted tier is
/// the whole of what a runner can be proven on here. That is the same argument
/// `crates/vike-studio-core/tests/rhai_study_pipeline.rs` makes for its own fixture tree.
fn user_data_with_a_study(name: &str, src: &str) -> (tempfile::TempDir, StudyHost) {
    let tmp = tempfile::tempdir().expect("temp dir");
    let user_data = tmp.path().join("user_data");
    let studies_root = user_data.join("research").join("studies");
    let dir = studies_root.join("rhai").join(name);
    std::fs::create_dir_all(&dir).expect("study folder");
    std::fs::write(dir.join(format!("{name}.rhai")), src).expect("entry file");
    let host = StudyHost {
        studies_root,
        runs_root: user_data.join("runs"),
        scratch: tmp.path().join("tmp"),
        produced_by: "vike-app".to_string(),
        git_sha: None,
    };
    (tmp, host)
}

/// A study that reads the seeded bars and records what it found — the ordinary shape, and enough
/// to give the result surface a metric to render.
const COUNTING_STUDY: &str = r#"
fn run(ctx, params) {
    let bars = ctx.bars("binance", "BTCUSDT", "1m");
    let out = outcome();
    out.metric("n_bars", bars.len());
    return out;
}
"#;

/// **The host gate, both directions.** A Studio with no project above it offers no study dispatch
/// and SAYS why; one with a project and a study in it arms the button.
///
/// Paired the way [`an_empty_store_arms_no_run_affordance_and_a_seeded_one_arms_them`] is, and for
/// the same reason: a gate hard-wired either way fails one half. The negative half asserts the
/// button is ABSENT rather than disabled, because with no host there is no runs directory to mint
/// into and no studies root to list — the pane renders its refusal instead of a dead control.
///
/// ⚠ Neither half CLICKS: a click spawns a real study on a worker thread.
#[test]
fn a_studio_with_no_project_offers_no_study_run_and_a_hosted_one_arms_it() {
    let (dir, store) = seeded_store();
    let mut h = shell(&store, &dir, RightTab::Research);
    settle(&mut h);
    let text = all_text(&h);
    assert!(
        text.iter().any(|t| t.contains(NO_HOST)),
        "a Studio with nowhere to put a run must SAY so; rendered: {text:?}"
    );
    assert!(
        !has_button(&h, &run_button("Run study")),
        "no host, no dispatch — and not a dead button either"
    );

    let (_tmp, host) = user_data_with_a_study("counting", COUNTING_STUDY);
    let mut st = state(&store, &dir, RightTab::Research, ChatApiKeys::default());
    st.research.host = Some(host);
    let mut h = harness_over(st);
    settle(&mut h);
    assert!(!is_disabled(&h, &run_button("Run study")), "a listed study must arm the dispatch");
    let text = all_text(&h);
    assert!(
        text.iter().any(|t| t == "counting"),
        "...and the study must be listed BY NAME; rendered: {text:?}"
    );
    assert!(
        text.iter().any(|t| t == "rhai"),
        "...with the tier badge that says which binaries can run it; rendered: {text:?}"
    );
}

/// **The study result surface, over a REAL study run** — the sibling of
/// [`the_results_pane_renders_every_tab_of_a_real_backtest`], and the R6 claim asserted where a
/// reader actually meets it.
///
/// Three independent things, and the third is the one this whole design exists for:
///
/// 1. the study's own numbers are on screen, under a heading that names them as a study's;
/// 2. `vike_studio_core::STUDY_METRICS_NOTE` — the sentence the library writes into every study
///    run's manifest — is rendered VERBATIM beside them, so the reader is told what they are not
///    comparable with;
/// 3. **not one cell of the backtest results surface is on screen at the same time.**
///    `crates/vike-studio/src/panes/results.rs`'s `perf_cells` labels are the marker set, because those
///    are exactly the strings a shared column would have put a study's Sharpe next to.
///
/// The run goes through `start_study` plus a bounded `poll` loop — the real dispatch path, so what
/// is rendered came out of `vike_studio_core::run_study_plan` and landed in `user_data/runs`
/// rather than being a fixture this file invented.
#[test]
fn a_study_run_renders_its_own_surface_and_never_a_backtests_columns() {
    let (dir, store) = seeded_store();
    let (_tmp, host) = user_data_with_a_study("counting", COUNTING_STUDY);
    let runs_root = host.runs_root.clone();
    let mut st = state(&store, &dir, RightTab::Research, ChatApiKeys::default());
    st.research.host = Some(host);
    st.research.refresh();

    st.start_study();
    for _ in 0..400 {
        st.poll();
        if st.study_last.is_some() {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    // Copied OUT of the state before the harness takes ownership of it — `harness_over` moves
    // `st`, so a borrow held across it would not compile.
    let (run_id, kind) = match &st.study_last {
        Some(Ok(run)) => (run.run_id.clone(), run.manifest.kind.clone()),
        other => panic!("the study must finish inside the 4s budget and succeed: {other:?}"),
    };
    assert_eq!(kind, STUDY_RUN_KIND, "one kind, whichever tier produced it");
    assert!(runs_root.join(&run_id).is_dir(), "the run is on disk under user_data/runs");

    let mut h = harness_over(st);
    settle(&mut h);
    let text = all_text(&h);

    assert!(
        text.iter().any(|t| t == STUDY_RESULT_TITLE),
        "the central panel must be the STUDY surface; rendered: {text:?}"
    );
    assert!(
        text.iter().any(|t| t == "n_bars"),
        "the study's own metric must be on screen; rendered: {text:?}"
    );
    assert!(
        text.iter().any(|t| t.contains(STUDY_METRICS_NOTE)),
        "the not-comparable-with-a-backtest note must be rendered VERBATIM; rendered: {text:?}"
    );
    for backtest_cell in ["Profit factor", "Max drawdown", "Final equity", "Win rate"] {
        assert!(
            !text.iter().any(|t| t == backtest_cell),
            "{backtest_cell:?} is a backtest column and must not share the frame with a study's \
             numbers; rendered: {text:?}"
        );
    }

    // ...and the run it just minted joins the SHARED list, told apart by kind.
    let rows = h.state().research.runs();
    assert_eq!(rows.len(), 1, "poll() must re-scan the runs after a study lands");
    assert_eq!(rows[0].kind, STUDY_RUN_KIND);
    assert_eq!(rows[0].run_id, run_id);
}

/// **A study that REFUSES is disclosed, and disclosed as a study's failure.**
///
/// A script with no `fn run(ctx, params)` is `RhaiStudy::new`'s named refusal, and the whole point
/// of routing it through `Self::error_state` is that the title says WHICH action failed — the same
/// correction `crates/vike-studio/src/studio/shell.rs`'s central panel already made for sweeps and
/// walk-forwards. A refusal that rendered as an empty panel would be the silence this workspace
/// keeps paying for.
#[test]
fn a_study_that_refuses_is_shown_as_a_study_failure_and_mints_no_run() {
    let (dir, store) = seeded_store();
    let (_tmp, host) = user_data_with_a_study("broken", "fn nope() { 1 }\n");
    let runs_root = host.runs_root.clone();
    let mut st = state(&store, &dir, RightTab::Research, ChatApiKeys::default());
    st.research.host = Some(host);
    st.research.refresh();

    st.start_study();
    for _ in 0..400 {
        st.poll();
        if st.study_last.is_some() {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_matches!(st.study_last, Some(Err(_)), "a script with no entry must refuse");
    assert!(!runs_root.exists(), "a refusal must not mint a run directory");

    let mut h = harness_over(st);
    settle(&mut h);
    let text = all_text(&h);
    assert!(
        text.iter().any(|t| t == "Study failed"),
        "the panel must name WHICH action failed; rendered: {text:?}"
    );
    assert!(
        text.iter().any(|t| t.contains("fn run(ctx, params)")),
        "...and carry the runner's own words, which name the entry the study is missing;          rendered: {text:?}"
    );
    assert!(
        !text.iter().any(|t| t == STUDY_RESULT_TITLE),
        "a refusal is not a result; rendered: {text:?}"
    );
}
