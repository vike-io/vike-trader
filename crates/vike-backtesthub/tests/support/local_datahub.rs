//! A KEY-LESS loopback datahub over one store directory, served from a thread of the test process —
//! the local route every history reader takes since the owner closed the local READ door on
//! 2026-09-25 (decision 0084's amendment).
//!
//! The shipped `backtesthub` binary refuses `--store DIR` and reads every byte of history through a
//! datahub, so a test that drives it over a scratch store needs one in front of that store. This is
//! the production shape in miniature, not a double: the real `vike_datahub::serve` over a
//! real `HistoryStoreDataFusion`, dialled by the child through its `config.datahub_addr` row (written by
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
//! ⚠ **A hub may start BEFORE its store is seeded.** `HistoryStoreDataFusion` holds no catalogue in memory —
//! every read resolves the published manifest afresh — so a [`seed_demo`] written straight into
//! the directory after this call is visible to the next request. That is the recorder-writes,
//! datahub-reads arrangement production runs, and the tests whose subject is a store's contents
//! MOVING between two runs depend on it: both runs must dial the SAME hub, because the search
//! identity records the hub's address as the run's store.
//!
//! Declared ONCE, by the `engine_cli` group binary (`tests/engine_cli.rs`), for its two suites that
//! need it (`optimizer_cli`, `search_persist_cli`): a file under `tests/support/` is no test target of
//! its own.

use std::net::TcpListener;
use std::path::Path;
use std::sync::Arc;

use vike_data::HistoryStore;
use vike_store::HistoryStoreDataFusion;

/// Serve `store` on a fresh loopback port and return the address to hand the child.
///
/// `HistoryStoreDataFusion::open` CREATES the root when it is absent, which is what lets a test start the
/// hub over a store path nothing has written yet — the empty-store searches most of these tests run.
pub fn serve(store: &Path) -> String {
    let hist = HistoryStoreDataFusion::open(store)
        .unwrap_or_else(|e| panic!("open the scratch store {}: {e}", store.display()));
    let store: Arc<dyn HistoryStore + Send + Sync> = Arc::new(hist);
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind a loopback port for the datahub");
    let addr = listener.local_addr().expect("the bound address").to_string();
    std::thread::spawn(move || {
        let _ = vike_datahub::serve(listener, store);
    });
    addr
}

/// One keyed bar written into `store`, as a backfill of `(venue, symbol, interval)` would: `key` is
/// the ingest commit key, so each new key is a new write the store's own record can see.
pub fn write_bar(
    store: &Path,
    venue: &str,
    symbol: &str,
    interval: &str,
    funding: Option<f64>,
    key: &str,
) {
    let hist = HistoryStoreDataFusion::open(store)
        .unwrap_or_else(|e| panic!("open the scratch store {}: {e}", store.display()));
    let ts = 1_735_689_600_000;
    let bar = vike_marketdata::Bar {
        funding,
        ..vike_marketdata::Bar::ohlcv(ts, 1.0, 1.0, 1.0, 1.0, 1.0)
    };
    hist.append_bars(venue, symbol, interval, &[bar], Some(key)).expect("the bar is written");
}

/// Write the test tape into `store` and return the rows written: the closed-form tape the fixtures'
/// profiles name, through `vike_data::demo` (behind `test-support`), the one writer of it. No shipped
/// verb writes it since the owner's ruling of 2026-11-13 (no demo or fake data in the product).
pub fn seed_demo(store: &Path) -> usize {
    let hist = HistoryStoreDataFusion::open(store)
        .unwrap_or_else(|e| panic!("open the scratch store {}: {e}", store.display()));
    let seeded = vike_data::demo::seed(&hist).expect("seed the demo tape");
    seeded.iter().map(|done| done.rows).sum()
}
