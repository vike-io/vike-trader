//! End-to-end tests for the `vike-cli backtest` command (headless two-layer plan, PR-1).
//!
//! Hermetic and loopback-only: spawn the REAL [`vike_backtest::compute_server::serve`] over an in-memory
//! `MemHistStore` on an ephemeral `127.0.0.1:0` port (the exact spawn pattern of
//! `crates/vike-datahub/tests/roundtrip.rs`), then run the SHIPPED bin via `CARGO_BIN_EXE_vike-cli`
//! as `vike-cli backtest …` against that address. No prod store, no external network.
//!
//! These assert the PLUMBING — a valid profile prints a well-formed JSON report carrying the
//! profile's `name`, and a malformed profile exits non-zero with a message on stderr. `MemHistStore`
//! is an inert stub whose `load_bars` returns empty, so the run closes zero trades; we assert the
//! request -> engine -> report -> response -> print path, NOT non-empty results.

use std::io::Write;
use std::net::{SocketAddr, TcpListener};
use std::sync::Arc;
use std::thread;

use vike_data::{HistStore, MemHistStore};
// ⚠ The COMPUTE server, not `vike_datahub::serve`. Ruling 7 of
// `docs/superpowers/specs/2026-09-09-datahub-market-data-wire-design.md` moved the `Run*` verbs to
// `vike-backend backtest --addr`, so pointing this harness at the data daemon would now prove a
// wrong-plane refusal while claiming to prove the command works.
use vike_backtest::compute_server::serve;

/// The planted-engine plant and the `ETXTBSY` retry that survives spawning one. See that module's
/// doc for the race and for why matching the errno — and only the errno — is what keeps the retry
/// from hiding a genuinely missing engine, which this file has a case about
/// ([`local_without_the_engine_binary_says_so`]).
mod common;

/// A minimal, valid bar-mode profile carrying a distinctive `name`. Over an empty `MemHistStore` it
/// loads no bars (zero trades), which is enough to exercise the whole path; the `name` is what we
/// assert survives into the report JSON. Mirrors the TOML shape in the vike-datahub roundtrip test.
const PROFILE_NAME: &str = "vike_cli_backtest_smoke";
const MINIMAL_BAR_PROFILE: &str = r#"
name = "vike_cli_backtest_smoke"

[data]
venue = "binance"
symbols = ["BTCUSDT"]
kind = "bar"
interval = "1d"
from = "0"
to = "100000"

[engine]
cash = 1000.0

[strategy]
name = "buy_hold"
[strategy.params]
size = 1.0
symbol = "BTCUSDT"
"#;

/// Bind an ephemeral loopback listener, spawn `serve` over a fresh in-memory store on a detached
/// thread, and return the assigned address for the CLI to connect to.
fn spawn_server() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
    let addr = listener.local_addr().expect("resolve assigned port");
    let store: Arc<dyn HistStore + Send + Sync> = Arc::new(MemHistStore::new());
    thread::spawn(move || {
        let _ = serve(listener, store);
    });
    addr
}

/// Write `contents` to a uniquely-named temp file that DELETES ITSELF when the returned handle drops
/// — on a passing test and on a failing assertion's unwind alike.
///
/// ⚠ This said "the OS reclaims the temp dir, so no explicit cleanup", which is false wherever
/// `TMPDIR` points at a directory nothing sweeps (the CI runners and the verification lanes):
/// 7,665 `vike_cli_bt_*.toml` were found in one on 2026-10-03, seven per run.
fn write_temp_profile(contents: &str, tag: &str) -> tempfile::TempPath {
    let mut f = tempfile::Builder::new()
        .prefix(&format!("vike_cli_bt_{tag}_"))
        .suffix(".toml")
        .tempfile()
        .expect("create temp profile");
    f.write_all(contents.as_bytes()).expect("write temp profile");
    f.into_temp_path()
}

#[path = "backtest_cli/local.rs"]
mod local;
#[path = "backtest_cli/remote.rs"]
mod remote;
