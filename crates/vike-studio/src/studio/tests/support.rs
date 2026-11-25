//! Fixtures more than one theme of `StudioState`'s tests needs: the hermetic constructor, the
//! seeded store, the loopback backtesthub, and the planted Build answer.

use super::super::*;
use std::sync::Arc;
use vike_data::HistoryStore;
use vike_marketdata::Bar;
use vike_store::HistoryStoreDataFusion;

/// `StudioState::new_with_qa` with the pairing vike-desktop passes for a LOCAL store: `state_dir` = the
/// store's own root (here the test's temp dir), keeping the old `store.root()` colocations
/// byte-identical. The `Arc<HistoryStoreDataFusion>` → `Arc<dyn HistoryStore + Send + Sync>` coercion happens at the call.
///
/// ⚠ Over a DEFAULT workspace rather than through `new`'s read, and that is hermeticity, not
/// style. `new` reads `workspace_read_path`, which is `<project>/settings/state` found by
/// walking up from the working directory — under `cargo test`, the CHECKOUT's own settings
/// root. Since the strategy source is persisted there, a box whose developer last left the
/// Studio in Strategy file mode would start every test here in Strategy file mode. Everything else this
/// constructor does (the store walk, the saved list under `state_dir`) is unchanged.
///
/// ⚠ Its backend is port 1 (nothing binds it, so a run's dial is refused at once), never
/// `StudioRunBackend::default()`'s `DEFAULT_BACKTEST_ADDRESS`: the CI box runs the PRODUCTION backtesthub there,
/// beside the lanes these tests run on, and a test that dispatches without serving its own
/// ([`spawn_backtesthub_server`]) dialled it.
pub(super) fn state_new(
    dir: &tempfile::TempDir,
    store: Arc<HistoryStoreDataFusion>,
) -> StudioState {
    let mut state = StudioState::with_workspace(
        store,
        dir.path().to_path_buf(),
        StudioChatApiKeys::default(),
        None,
        StudioAutorun::Off,
        StudioWorkspace::default(),
    );
    state.backend = StudioRunBackend::Source { address: "127.0.0.1:1".to_string() };
    state
}

/// Bind an ephemeral loopback listener and serve the COMPUTE verbs over `store` on a
/// detached thread; return the address a `StudioRunBackend::Source` should dial.
///
/// ⚠ **This exists because `StudioRunBackend::Local` was deleted**
/// (`docs/decisions/0078-one-backtest-path-studios-local-backend-is-deleted.md`). A Studio run
/// leaves the process now, so a test that wants a RESULT needs something to answer - and the
/// DEFAULT address is a real deployed daemon's. Without this the test dialled
/// `127.0.0.1:7880` and, on the CI box where that daemon runs, got its AUTH REFUSAL back: a
/// unit test reaching a live service and reporting the rejection as its own failure.
///
/// The server is `vike_backtesthub::backtesthub_server::serve`, the same one
/// `crates/vike-backtesthub/tests/studio_wire_parity.rs` drives, with no hub keys - so it is
/// unauthenticated, hermetic, and bound to port 0. This is STRICTLY more coverage than the
/// in-process path it replaces: the run is now marshalled over the real wire.
pub(super) fn spawn_backtesthub_server(store: Arc<HistoryStoreDataFusion>) -> String {
    spawn_backtesthub_server_keyed(store, None)
}

/// [`spawn_backtesthub_server`] KEYED by `keys` (a server's: an access list it admits devices by,
/// decision 0288) — the shape of every deployed backtesthub since the cutover.
pub(super) fn spawn_backtesthub_server_keyed(
    store: Arc<HistoryStoreDataFusion>,
    keys: Option<vike_hub_protocol::auth::HubKeys>,
) -> String {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
    let address = listener.local_addr().expect("resolve assigned port").to_string();
    let handle: Arc<dyn HistoryStore + Send + Sync> = store;
    std::thread::spawn(move || {
        // Its `.rs` strategy files are built by their compiled stand-ins (a shipped daemon hands
        // them to a sandboxed child): this suite tests the shell and the wire, not the sandbox.
        let study = vike_backtesthub::backtesthub_server::StudyDirs {
            strategies: vike_backtesthub::studio_run::StrategyFiles::stand_in(
                vike_strategy_lab::stand_in,
            ),
            ..vike_backtesthub::backtesthub_server::StudyDirs::working_directory()
        };
        let _ = vike_backtesthub::backtesthub_server::serve_authed(
            listener,
            handle,
            study,
            keys,
            vike_backtesthub::named_run::NamedRunLane::DISARMED,
            vike_hub_protocol::limits::SERVER_IDLE_READ_TIMEOUT,
        );
    });
    address
}

/// A temp-dir-backed store seeded with ~400 oscillating 1m bars for `binance/BTCUSDT`, so the
/// default SMA-crossover editor script has something to cross on. Returns the `TempDir` so the
/// caller binds it and keeps the backing directory alive for the test's lifetime (the fragile
/// `std::mem::forget` + shared-temp-path approach from the original plan is deliberately not
/// used here — see Task 2's `run.rs::tests::seeded_store` for the pattern this mirrors).
pub(super) fn seeded_store() -> (tempfile::TempDir, Arc<HistoryStoreDataFusion>) {
    let dir = tempfile::tempdir().unwrap();
    let store = HistoryStoreDataFusion::open(dir.path()).unwrap();
    let bars: Vec<Bar> = (0..400)
        .map(|i| {
            let c = 100.0 + (i % 7) as f64;
            Bar {
                timestamp: 60_000 * (i as i64 + 1),
                open: c,
                high: c,
                low: c,
                close: c,
                volume: 0.0,
                funding: None,
                bid: None,
                ask: None,
                symbol: Some("BTCUSDT".into()),
            }
        })
        .collect();
    store.append_bars("binance", "BTCUSDT", "1m", &bars, None).unwrap();
    (dir, Arc::new(store))
}

/// A channel already carrying `v` — a delivered worker answer, with no thread and no timing.
pub(super) fn ready<T>(v: T) -> Receiver<T> {
    let (tx, rx) = std::sync::mpsc::channel();
    tx.send(v).expect("plant the answer");
    rx
}

/// A build that has already ANSWERED, planted as the dispatch would leave it: `sent` is the
/// source `dispatch_rust_build` captured, `answer` the builder's reply. The tests that only
/// need a delivered answer plant it this way; the one that is ABOUT what gets captured
/// (`an_edit_made_while_the_build_runs_is_not_recorded_as_built`) drives the real dispatch.
pub(super) fn answered_build(
    sent: &str,
    answer: Result<String, String>,
) -> Option<StudioWorkerRustBuildRequest> {
    Some((sent.to_string(), ready(answer)))
}
