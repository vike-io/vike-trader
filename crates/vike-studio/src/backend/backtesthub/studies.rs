//! The backtest server's studies and saved runs (the owner's ruling of 2026-11-19: studies run ONLY on
//! the server): the Research pane's study list, its runs list a page at a time, a Run study, and the
//! first page of one run's artifact, each a dial on a worker thread. The study list is a Read dial
//! ([`connect_observe`]: it describes a folder, as the strategy-file list does); the runs, an artifact
//! and a run are Write dials with this PC's device key ([`connect`]): only a client that may run
//! something reads results back. Each answer is the client's typed wire value
//! (`crates/vike-backtesthub-client/src/study.rs`); an older server's refusal is the client's own
//! sentence, nothing sent.

use std::sync::mpsc::Receiver;

use vike_backtesthub_client::proto::StudyRunRequestWire;
use vike_backtesthub_client::study::{
    RUNS_PAGE_SIZE, WireRunArtifact, WireRunPage, WireStudyListing, WireStudyReport,
};
use vike_hub_protocol::auth::HubKeys;
use vike_studio_core::spawn_outcome;

use super::{connect, connect_observe};

/// The server's studies, both tiers, with their recipe file names, on a worker. An OBSERVE dial.
pub(crate) fn spawn_backtesthub_studies(
    address: String,
    keys: Option<HubKeys>,
) -> Receiver<Result<WireStudyListing, String>> {
    spawn_outcome(move || {
        connect_observe(&address, keys.as_ref()).map_err(|e| e.to_string())?.list_studies()
    })
}

/// Page `page` of the server's saved runs (0 is the newest), on a worker. A Write dial with this PC's
/// device key.
pub(crate) fn spawn_backtesthub_runs_page(
    address: String,
    key: Option<HubKeys>,
    page: usize,
) -> Receiver<Result<WireRunPage, String>> {
    spawn_outcome(move || {
        connect(&address, key.as_ref()).map_err(|e| e.to_string())?.list_runs(page, RUNS_PAGE_SIZE)
    })
}

/// Run one study on the server, on a worker: its report, saved or not (the request asks for an
/// unsaved result's numbers), or the refusal in the server's words. A Write dial with this PC's
/// device key; the wait is the study's own, so it is not bounded here.
pub(crate) fn spawn_backtesthub_study_run(
    address: String,
    key: Option<HubKeys>,
    request: StudyRunRequestWire,
) -> Receiver<Result<WireStudyReport, String>> {
    spawn_outcome(move || {
        let json =
            connect(&address, key.as_ref()).map_err(|e| e.to_string())?.run_study(&request)?;
        serde_json::from_str(&json)
            .map_err(|e| format!("the backtest server's study report could not be read: {e}"))
    })
}

/// The first page of the artifact `file` of the saved run `run_id`, on a worker. A Write dial with
/// this PC's device key.
pub(crate) fn spawn_backtesthub_run_artifact(
    address: String,
    key: Option<HubKeys>,
    run_id: String,
    file: String,
) -> Receiver<Result<WireRunArtifact, String>> {
    spawn_outcome(move || {
        connect(&address, key.as_ref())
            .map_err(|e| e.to_string())?
            .read_run_artifact(&run_id, &file, 0, 0)
    })
}
