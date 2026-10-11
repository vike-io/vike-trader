//! Fixtures more than one theme of `StudioState`'s tests needs: the hermetic constructor, the
//! seeded store, the loopback compute server, and the planted Build answer.

use super::super::*;
use std::sync::Arc;
use vike_data::{DataFusionHist, HistStore};
use vike_model::Bar;

/// `StudioState::new` with the pairing vike-desktop passes for a LOCAL store: `state_dir` = the
/// store's own root (here the test's temp dir), keeping the old `store.root()` colocations
/// byte-identical. The `Arc<DataFusionHist>` → `StoreHandle` coercion happens at the call.
///
/// ⚠ Over a DEFAULT workspace rather than through `new`'s read, and that is hermeticity, not
/// style. `new` reads `workspace_read_path`, which is `<project>/settings/state` found by
/// walking up from the working directory — under `cargo test`, the CHECKOUT's own settings
/// root. Since the strategy source is persisted there, a box whose developer last left the
/// Studio in Plugin mode would start every test here in Plugin mode. Everything else this
/// constructor does (the store walk, the saved list under `state_dir`) is unchanged.
pub(super) fn state_new(dir: &tempfile::TempDir, store: Arc<DataFusionHist>) -> StudioState {
    StudioState::with_workspace(
        store,
        dir.path().to_path_buf(),
        ChatApiKeys::default(),
        None,
        QaAutorun::Off,
        StudioWorkspace::default(),
    )
}

/// Bind an ephemeral loopback listener and serve the COMPUTE verbs over `store` on a
/// detached thread; return the address a `Backend::Remote` should dial.
///
/// ⚠ **This exists because `Backend::Local` was deleted**
/// (`docs/decisions/0078-one-backtest-path-studios-local-backend-is-deleted.md`). A Studio run
/// leaves the process now, so a test that wants a RESULT needs something to answer - and the
/// DEFAULT address is a real deployed daemon's. Without this the test dialled
/// `127.0.0.1:7880` and, on the CI box where that daemon runs, got its AUTH REFUSAL back: a
/// unit test reaching a live service and reporting the rejection as its own failure.
///
/// The server is `vike_backtest::compute_server::serve_with_studio`, the same one
/// `crates/vike-studio-core/tests/studio_wire_parity.rs` drives, with no node keys - so it is
/// unauthenticated, hermetic, and bound to port 0. This is STRICTLY more coverage than the
/// in-process path it replaces: the run is now marshalled over the real wire.
pub(super) fn spawn_compute_server(store: Arc<DataFusionHist>) -> String {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
    let addr = listener.local_addr().expect("resolve assigned port").to_string();
    let handle: Arc<dyn HistStore + Send + Sync> = store;
    std::thread::spawn(move || {
        let _ = vike_backtest::compute_server::serve_with_studio(
            listener,
            handle,
            Some(vike_studio_core::studio_run_table()),
        );
    });
    addr
}

/// A temp-dir-backed store seeded with ~400 oscillating 1m bars for `binance/BTCUSDT`, so the
/// default SMA-crossover editor script has something to cross on. Returns the `TempDir` so the
/// caller binds it and keeps the backing directory alive for the test's lifetime (the fragile
/// `std::mem::forget` + shared-temp-path approach from the original plan is deliberately not
/// used here — see Task 2's `run.rs::tests::seeded_store` for the pattern this mirrors).
pub(super) fn seeded_store() -> (tempfile::TempDir, Arc<DataFusionHist>) {
    let dir = tempfile::tempdir().unwrap();
    let store = DataFusionHist::open(dir.path()).unwrap();
    let bars: Vec<Bar> = (0..400)
        .map(|i| {
            let c = 100.0 + (i % 7) as f64;
            Bar {
                ts: 60_000 * (i as i64 + 1),
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
fn ready<T>(v: T) -> Receiver<T> {
    let (tx, rx) = std::sync::mpsc::channel();
    tx.send(v).expect("plant the answer");
    rx
}

/// A build that has already ANSWERED, planted as the dispatch would leave it: `sent` is the
/// source `dispatch_plugin_build` captured, `answer` the builder's reply. The tests that only
/// need a delivered answer plant it this way; the one that is ABOUT what gets captured
/// (`an_edit_made_while_the_build_runs_is_not_recorded_as_built`) drives the real dispatch.
pub(super) fn answered_build(sent: &str, answer: Result<String, String>) -> Option<PendingBuild> {
    Some((sent.to_string(), ready(answer)))
}
