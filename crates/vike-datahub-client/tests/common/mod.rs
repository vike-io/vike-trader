//! The ONE spelling of the loopback server this crate's integration binaries drive: the REAL
//! `vike_datahub::serve` over a caller-supplied store, on an ephemeral port. Each user declares it
//! with `mod common;` — a `tests/` SUBDIRECTORY module is not a test target, so this file builds no
//! binary and runs no test of its own.
//!
//! Every binary that declares it calls everything in it, which is why it carries no
//! `allow(dead_code)`: a helper only SOME binaries call belongs in its own file under
//! `tests/support/`, `#[path]`-included by each caller. A server whose wiring DIFFERS per file
//! (`serve_with_backfill` with a table, `serve_authed` with keys or a mounted hub, the hand-rolled
//! fakes) stays local: each file pins its own.
//!
//! `vike-datahub` is a DEV-dependency of this crate (layer 65 over 25), so this is the crate's own
//! copy of `crates/vike-datahub/tests/common/mod.rs`'s `spawn_server`, with the same signature.

use std::net::{SocketAddr, TcpListener};
use std::sync::Arc;
use std::thread;

use vike_data::HistStore;
use vike_datahub::serve;

/// Bind an ephemeral loopback listener, spawn the REAL `serve` over `store` on a detached thread,
/// and return the assigned address for a client or a raw stream to connect to.
///
/// `store` is the one thing the former per-file copies differed in: `remote_roundtrip.rs` hands
/// it a seeded `MemHistStore`, every other caller a fresh `Arc::new(MemHistStore::new())`. `serve`
/// mounts no backfill table, no node keys, no market-data hub, no seed or catalog lane and no
/// import lane, which is exactly the server several suites exist to drive. The serve loop runs
/// for the lifetime of the test process; its `Result` is only `Err` on an impossible listener
/// close, which nothing here asserts.
pub fn spawn_server(store: Arc<dyn HistStore + Send + Sync>) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
    let addr = listener.local_addr().expect("resolve assigned port");
    thread::spawn(move || {
        let _ = serve(listener, store);
    });
    addr
}
