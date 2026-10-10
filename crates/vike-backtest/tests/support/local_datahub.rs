//! A KEY-LESS loopback datahub over one store directory, served from a thread of the test process —
//! the local route every history reader takes since the owner closed the local READ door on
//! 2026-09-25 (decision 0084's amendment).
//!
//! The shipped `backtest` binary refuses `--store DIR` and reads every byte of history through a
//! datahub, so a test that drives it over a scratch store needs one in front of that store. This is
//! the production shape in miniature, not a double: the real `vike_datahub::serve` over a
//! real `DataFusionHist`, dialled by the child through its `config.datahub_addr` row (written by
//! `engine.rs`'s `Scratch::dial`), exactly as `vike-backend datahub --store DIR` serves an
//! operator's files.
//!
//! ⚠ **Port 0, never the default `127.0.0.1:7878`.** Test binaries run concurrently on shared
//! runners, and a the CI box lane shares its box with the real datahub on that port — a test that bound
//! it would either fail or, worse, read a real store.
//!
//! ⚠ **The server thread is DETACHED and lives as long as the test process.** `serve` never returns
//! for a TCP listener, and nothing here needs it to: one test binary is one process, and the
//! listener goes when it does.
//!
//! ⚠ **A hub may start BEFORE its store is seeded.** `DataFusionHist` holds no catalogue in memory —
//! every read resolves the published manifest afresh — so a `data seed-demo` written straight into
//! the directory after this call is visible to the next request. That is the recorder-writes,
//! datahub-reads arrangement production runs, and the tests whose subject is a store's contents
//! MOVING between two runs depend on it: both runs must dial the SAME hub, because the search
//! identity records the hub's address as the run's store.
//!
//! Included with `#[path]` by each test binary that needs it (`optimizer_cli.rs`,
//! `search_persist_cli.rs`): a file under `tests/support/` is no test target of its own.

use std::net::TcpListener;
use std::path::Path;
use std::sync::Arc;

use vike_data::{DataFusionHist, HistStore};

/// Serve `store` on a fresh loopback port and return the address to hand the child.
///
/// `DataFusionHist::open` CREATES the root when it is absent, which is what lets a test start the
/// hub over a store path nothing has written yet — the empty-store searches most of these tests run.
pub fn serve(store: &Path) -> String {
    let hist = DataFusionHist::open(store)
        .unwrap_or_else(|e| panic!("open the scratch store {}: {e}", store.display()));
    let store: Arc<dyn HistStore + Send + Sync> = Arc::new(hist);
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind a loopback port for the datahub");
    let addr = listener.local_addr().expect("the bound address").to_string();
    std::thread::spawn(move || {
        let _ = vike_datahub::serve(listener, store);
    });
    addr
}
