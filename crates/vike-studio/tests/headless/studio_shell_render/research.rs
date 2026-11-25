//! The Research pane, a thin client of the backtest server (studies run ONLY there, the owner's ruling
//! of 2026-11-19): the server's studies and runs over the real wire, and a server the pane cannot
//! use — unreachable, or dialled by the Named backend's Read key — said in words with Run disarmed.
//! The study RESULT screen is `crates/vike-studio/src/studio/tests/research.rs`'s, which can pose the
//! central panel on it.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use egui_kittest::Harness;
use vike_data::HistoryStore;
use vike_runs::{RunConfig, RunManifest};
use vike_studio::{
    STUDY_NEEDS_COMPUTE, StudioChatApiKeys, StudioRunBackend, StudioState, StudioTab,
};
use vike_studio_core::{STUDY_METRICS_NOTE, STUDY_RUN_KIND};

use super::support::{all_text, harness_over, is_disabled, run_button, settle};
use crate::common::{empty_store, state};

/// An address nothing binds: a dial to it is refused at once.
const DEAD: &str = "127.0.0.1:1";

/// Serve the backtest plane over `store` on an ephemeral loopback port, its saved runs under
/// `runs_root`, unauthenticated; return the address. (Its study folder is the working directory's
/// project's, `vike_backtesthub::study_sandbox::studies_root`, so what it holds is asserted nowhere.)
fn serve_with_runs(store: Arc<dyn HistoryStore + Send + Sync>, runs_root: PathBuf) -> String {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
    let address = listener.local_addr().expect("resolve assigned port").to_string();
    std::thread::spawn(move || {
        let study = vike_backtesthub::backtesthub_server::StudyDirs {
            runs_root,
            ..vike_backtesthub::backtesthub_server::StudyDirs::working_directory()
        };
        let _ = vike_backtesthub::backtesthub_server::serve_authed(
            listener,
            store,
            study,
            None,
            vike_backtesthub::named_run::NamedRunLane::DISARMED,
            vike_hub_protocol::limits::SERVER_IDLE_READ_TIMEOUT,
        );
    });
    address
}

/// One saved study run under `runs_root`, written by `crates/vike-runs/src/write.rs`'s `write_run`,
/// with a metric in its manifest that must never reach the runs list.
fn plant_study_run(runs_root: &Path, run_id: &str) {
    let manifest = RunManifest {
        schema: 1,
        run_id: run_id.to_string(),
        kind: STUDY_RUN_KIND.to_string(),
        produced_by: "vike-backend".to_string(),
        started_at: "2026-08-24T09:15:04Z".to_string(),
        finished_at: "2026-08-24T09:15:16Z".to_string(),
        git_sha: None,
        strategy_sdk: None,
        fingerprint: None,
        config: RunConfig { path: None, name: None },
        detail: serde_json::json!({
            "study": "vol",
            "metrics": [{ "name": "sharpe", "value": 1234.5, "nonfinite": null }],
            "metrics_note": STUDY_METRICS_NOTE,
        }),
        kept_rows: Vec::new(),
    };
    let dir = runs_root.join(run_id);
    std::fs::create_dir_all(&dir).expect("run folder");
    vike_runs::write_run(&dir, &manifest, &serde_json::json!({})).expect("the run's documents");
}

/// Run frames until `done` holds over the rendered text (the server's answer has landed), then
/// settle and return that text. A list on its way asks for no IMMEDIATE repaint, so each `run`
/// returns after a frame and the wait is this loop's.
fn render_until(
    h: &mut Harness<'static, StudioState>,
    done: impl Fn(&[String]) -> bool,
) -> Vec<String> {
    for _ in 0..400 {
        h.run();
        if done(all_text(h).as_slice()) {
            settle(h);
            return all_text(h);
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!("the server's answer never reached the pane within 4s; rendered: {:?}", all_text(h))
}

/// **The pane lists what the SERVER holds, over the real wire.** Opening the Research tool dials
/// the backtest server the Studio uses: the top line names it and its study folder, the runs list
/// is the server's (its runs root as the path line, a study run under its `study` badge), and the
/// study's Sharpe in that run's manifest reaches no row (R6).
#[test]
fn the_research_pane_lists_the_backtest_servers_studies_and_runs() {
    let (dir, store) = empty_store();
    let runs = tempfile::tempdir().expect("runs folder");
    let runs_root = runs.path().join("runs");
    plant_study_run(&runs_root, "1756000000-1-0");
    let address = serve_with_runs(store.clone(), runs_root.clone());
    let mut st = state(&store, &dir, StudioTab::Research, StudioChatApiKeys::default());
    st.backend = StudioRunBackend::Source { address: address.clone() };
    let mut h = harness_over(st);

    // Both lists have answered once the top line (the studies) and the runs root (the runs) show.
    let top = format!("Studies run on the backtest server {address}. Its study folder: ");
    let root = runs_root.display().to_string();
    let text = render_until(&mut h, |text| {
        text.iter().any(|t| t.starts_with(&top)) && text.contains(&root)
    });
    assert!(text.iter().any(|t| t == "1756000000-1-0"), "the server's run; rendered: {text:?}");
    assert!(text.iter().any(|t| t == STUDY_RUN_KIND), "...under its kind; rendered: {text:?}");
    assert!(
        !text.iter().any(|t| t.contains("1234")),
        "a study's numbers never reach the shared runs list; rendered: {text:?}"
    );
    let rows = h.state().research.runs();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].kind, STUDY_RUN_KIND);
}

/// **A server the pane cannot use is said in words, and Run study is disarmed.** Unreachable: the
/// dial's own error in both lists' place. The Named backend (its Read key only): the studies list is
/// still asked, and the runs list and Run say they need the compute connection.
#[test]
fn a_server_the_pane_cannot_use_is_said_in_words_and_arms_no_study_run() {
    let (dir, store) = empty_store();
    let failed = format!("{DEAD} failed");

    let mut st = state(&store, &dir, StudioTab::Research, StudioChatApiKeys::default());
    st.backend = StudioRunBackend::Source { address: DEAD.to_string() };
    let mut h = harness_over(st);
    let text =
        render_until(&mut h, |text| text.iter().filter(|t| t.contains(&failed)).count() >= 2);
    assert!(
        is_disabled(&h, &run_button("Run study")),
        "no listed study, no run; rendered: {text:?}"
    );

    let mut st = state(&store, &dir, StudioTab::Research, StudioChatApiKeys::default());
    st.backend = StudioRunBackend::Named { address: DEAD.to_string() };
    let mut h = harness_over(st);
    let text = render_until(&mut h, |text| text.iter().any(|t| t.contains(&failed)));
    assert!(
        text.iter().any(|t| t == STUDY_NEEDS_COMPUTE),
        "the runs list needs the Write dial and says so; rendered: {text:?}"
    );
    assert!(is_disabled(&h, &run_button("Run study")), "...and so does Run study");
}
