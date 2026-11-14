//! The loopback backtesthub this crate's tests (`backtest_plane.rs`, `backtesthub_profile_roundtrip.rs`),
//! vike-cli's backtest/search suites and vike-studio-core's wire-parity suite each spawned by hand until
//! 2026-10-31, and the research bins' `--json` provenance pin, compiled once here behind `test-support`
//! (only DEV edges turn it on: this crate's own, vike-cli's, vike-studio-core's).

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use vike_backtest::harness::strategy_builder::StandIn;
use vike_data::{HistoryStore, HistoryStoreMemory};

use crate::backtesthub_server::{StudyDirs, serve_authed};
use crate::named_run::NamedRunLane;
use crate::studio_run::StrategyFiles;

/// Bind an ephemeral loopback listener, spawn the REAL backtesthub (`serve_authed`, key-less, the named
/// run disarmed) over a fresh in-memory store on a detached thread, and return the assigned address.
/// Every backtesthub serves the Studio runners and the study runner too, so this one serves all of them.
///
/// ⚠ Its `.rs` strategy files are built by `stand_in` (`StrategyFiles::stand_in`; every caller hands
/// `vike_strategies_check::stand_in`, which this library cannot name), where a shipped daemon hands
/// them to a sandboxed child: these suites test the wire and the harness, not the sandbox.
pub fn spawn_server(stand_in: StandIn) -> SocketAddr {
    spawn_server_over(Arc::new(HistoryStoreMemory::new()), stand_in)
}

/// [`spawn_server`] over the caller's `store`, which is MOVED into the serve thread (a parity test keeps
/// its own clone for the local run it compares against).
pub fn spawn_server_over(
    store: Arc<dyn HistoryStore + Send + Sync>,
    stand_in: StandIn,
) -> SocketAddr {
    vike_log::loopback::spawn_on_loopback("backtesthub-test-server", move |listener| {
        let _ = serve_with_stand_ins(listener, store, stand_in);
    })
}

/// A REAL hub over `store`: its `.rs` strategy files, read under `strategies`, run in a sandboxed child
/// (`StrategyFiles::hub`), as a shipped daemon's do. The child program is
/// `crate::strategy_sandbox::use_program`'s.
pub fn spawn_hub_over(
    store: Arc<dyn HistoryStore + Send + Sync>,
    strategies: PathBuf,
) -> SocketAddr {
    vike_log::loopback::spawn_on_loopback("backtesthub-test-hub", move |listener| {
        let study = StudyDirs {
            strategies: StrategyFiles::hub(strategies),
            ..StudyDirs::working_directory()
        };
        let _ = serve_authed(
            listener,
            store,
            study,
            None,
            NamedRunLane::DISARMED,
            vike_hub_protocol::limits::SERVER_IDLE_READ_TIMEOUT,
        );
    })
}

/// The answer THIS process gives `request` with the strategy file loaded HERE (`build`, the file's
/// `text`): the dispatch and the strategy files the sandboxed child answers with
/// (`crate::strategy_sandbox::serve_child`), in this process. The in-process half of a sandbox parity test.
pub fn answer_in_this_process(
    request: vike_backtesthub_client::proto::Request,
    store: &Arc<dyn HistoryStore + Send + Sync>,
    build: crate::studio_run::StrategyFileBuild,
    text: String,
) -> vike_backtesthub_client::proto::Response {
    let study = StudyDirs {
        strategies: crate::strategy_sandbox::loaded_strategy_files(&request, Ok(build), text),
        ..StudyDirs::working_directory()
    };
    crate::backtesthub_server::dispatch::handle_request_here(
        request,
        store,
        &study,
        NamedRunLane::DISARMED,
        None,
    )
}

/// The answer a SANDBOXED child gives `request`, its `.rs` file read under `strategies` as a hub reads it:
/// the daemon's own route, without the socket. What the run-cost measurement drives. `None` when the
/// request runs no readable `.rs` file there.
pub fn answer_in_a_child(
    request: &vike_backtesthub_client::proto::Request,
    store: &Arc<dyn HistoryStore + Send + Sync>,
    strategies: PathBuf,
) -> Option<vike_backtesthub_client::proto::Response> {
    let (_, source) =
        crate::strategy_sandbox::rust_file_of(request, &StrategyFiles::hub(strategies))?;
    Some(crate::strategy_sandbox::run_in_child(request, &source, store, None))
}

/// The test entry `crate::backtesthub_server::serve` is (key-less, the named run disarmed, the working
/// directory's folders), with its `.rs` strategy files built by `stand_in`.
pub fn serve_with_stand_ins(
    listener: std::net::TcpListener,
    store: Arc<dyn HistoryStore + Send + Sync>,
    stand_in: StandIn,
) -> std::io::Result<()> {
    let study = StudyDirs {
        strategies: StrategyFiles::stand_in(stand_in),
        ..StudyDirs::working_directory()
    };
    serve_authed(
        listener,
        store,
        study,
        None,
        NamedRunLane::DISARMED,
        vike_hub_protocol::limits::SERVER_IDLE_READ_TIMEOUT,
    )
}

/// **A research bin's `--json` document SAYS which percentile convention produced its numbers**,
/// asserted against the bin's own SOURCE, `bin_source` (the caller reads its own file), because `main`
/// assembles that document out of a dozen locals and no unit test can call it — the device
/// `crates/vike-tradehub/tests/source_gates/live_lock_claim_order_gate.rs` applies to the composition
/// roots, with that gate's rule: if this reddens because the emitting site was legitimately
/// re-spelled, UPDATE the needle, never delete the test.
///
/// The needle is ASSEMBLED rather than written out, so the joined string appears in no file and can
/// only match the emitting site — a literal would let a test pass on its own text, the
/// self-reference hazard `crates/vike-ops/tests/docs/unrun_command_gate.rs` declares for itself. And
/// EXACTLY one match is required, which fails a typo'd needle at zero (so the test cannot go vacuous)
/// and a marker duplicated onto a per-anchor lane or a per-entry row at two: the stamp belongs at RUN
/// level. The stamp must then name the percentile convention actually in force.
pub fn assert_json_stamps_its_percentile_once(bin_source: &str) {
    let key = "stats_provenance";
    let needle = format!("\"{key}\": binutil::{key}()");
    assert_eq!(
        bin_source.matches(&needle).count(),
        1,
        "the --json summary must stamp `{needle}` exactly once, at RUN level"
    );
    assert_eq!(
        crate::binutil::stats_provenance()["percentile_method"],
        serde_json::json!(vike_analytics::metrics::PERCENTILE_METHOD),
        "the stamp must name the percentile convention the bin's numbers come from"
    );
}
