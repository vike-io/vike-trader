// Shared across several independently-compiled integration-test binaries (handshake.rs,
// data_mapper.rs, data_spot.rs, ...); each only exercises a subset of these test-support items,
// so per-binary dead-code analysis flags the rest — expected, not a real dead-code smell.
#![allow(dead_code)]
//! Shared test-only in-process fake cTrader server, used by every integration test that needs to
//! exercise `conn::connect_and_auth` end-to-end (handshake.rs today; Tasks 4/5 add more). Binds
//! `127.0.0.1:0` (PLAINTEXT — no TLS), reads framed `ProtoMessage`s, records each `payload_type`,
//! and replies with scripted response frames the caller scripts via [`FakeCtrader::script`]/
//! [`FakeCtrader::start_scripted`]. Not a production component; it exists purely so `conn`'s
//! handshake can be exercised without a live socket. Built only against `vike_ctrader`'s PUBLIC
//! API (this is `tests/`, so it cannot reach `pub(crate)` items like `conn::EofStream` — a small
//! local EOF-tracking wrapper is duplicated below instead).

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use vike_ctrader::conn::ActorHandle;
use vike_ctrader::exec::CtraderExec;
use vike_exec::{EventSender, Ingest};
use vike_marketdata::Bar;
use vike_model::OrderRequest;
use vike_model::events::Event;

mod close_script;
mod fake;
mod foreign_book;
mod serve;

// The shared LiveDataSink doubles (testing-arch Phase 4d) — `NoopSink` for tests that only need
// a warm body (handshake/exec), `RecordingSink` (typed accessors: `quotes()`/`forming_bars()`/
// `seeded_bars()`) for the scripted spot-event (`tests/data_spot.rs`) and historical-seed
// (`tests/data_bars.rs`) integration tests.
// (Per-binary compilation: each integration test uses only a subset of these re-exports —
// same rationale as the file-top dead_code allow.)
#[expect(unused_imports)]
pub use {
    close_script::{CLOSE_POSITION_ID_BASE, CloseServePolicy},
    fake::{AccountsScript, FakeCtrader},
    foreign_book::ForeignBook,
    serve::{
        AcceptedToken, AmendReply, FAKE_FILL_PRICE, FAKE_ORDER_ID, INFLIGHT_FILL_COID,
        INFLIGHT_FILL_PRICE, RECONCILE_PENDING_COID, RECONCILE_PENDING_ORDER_ID,
        SEED_BAR_NEWER_MINUTES, SEED_BAR_OLDER_MINUTES, SPOT_EVENT_TIMESTAMP_MS,
    },
    vike_data::{NoopSink, RecordingSink},
};

/// One recorded `LiveDataSink::seed_bars` call: `(venue, symbol, interval, bars)`.
pub type SeedBarsCall = (String, String, String, Vec<Bar>);

// ── The operator HALT sentinel, for the tests that submit an opening order ───────────────────────

/// A sentinel path THIS test process names and never creates.
///
/// ⚠ **This used to be the cure for a hazard that no longer exists, and it is kept for a different
/// reason.** `CtraderExec`'s default was ARMED at the PROCESS-WIDE sentinel
/// (`vike_boot::halt::halt_path_from_env`), which made every test that submits an OPENING
/// order depend on whether that file happened to exist on the box running it — MEASURED on the CI box, 13
/// tests in this crate went red with a real file at the resolved path, and CI was green only
/// because no runner had one. Decision 0099 removed the default: a client is handed its sentinel by
/// its start (`CtraderExec::with_halt_path`) and consults nothing process-wide, so no file on the box
/// can change a verdict here, and the re-run harness that proved it
/// (`assert_indifferent_to_an_engaged_halt_sentinel`) is gone with the hazard.
///
/// What this still buys is a WIRED client: one built with no path logs `HALT KILL SWITCH IS NOT
/// WIRED` at `error` the first time it is asked, which is the right noise for a start that forgot
/// and the wrong noise for a test that did not. The path is unique per (process, call) so parallel
/// tests cannot collide, and the directory is deliberately NOT created — a test that wants to ENGAGE
/// a halt makes its own file, as `tests/exec_halt.rs` does.
///
/// ⚠ **This one NAMES a path and touches the disk nowhere**, which is why it is still a bare
/// `PathBuf` while every sibling in this crate moved to a bound `tempfile::TempDir`: with no
/// `create_dir_all` there is nothing to leak and nothing for a reused PID to collide with, and the
/// halt stays unengaged because the FILE is absent whether or not its parent exists. Do not "fix"
/// it by minting a temp directory — the guard would have to be RETURNED, and every call site
/// (through [`exec_with_no_halt`], across most of this crate's test binaries) wants a client, not a
/// tuple. Do not COPY it either: the moment a caller creates the path, it is the leaking idiom
/// `crates/bridges/ctrader/tests/exec_halt.rs`'s `unique_sentinel` describes.
pub fn unengaged_halt_sentinel(tag: &str) -> PathBuf {
    static N: AtomicU64 = AtomicU64::new(0);
    let n = N.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir()
        .join(format!("vike-ctrader-no-halt-{}-{n}-{tag}", std::process::id()))
        .join("HALT")
}

/// [`CtraderExec::new`] handed [`unengaged_halt_sentinel`] — the constructor every NON-live test in
/// this crate that submits an opening order uses, so the client is wired the way a start wires it
/// and its verdict cannot depend on a kill switch.
pub fn exec_with_no_halt(handle: ActorHandle, events: EventSender, tag: &str) -> CtraderExec {
    CtraderExec::new(handle, events).with_halt_path(unengaged_halt_sentinel(tag))
}

// ------------------------------------------------------------------------------------------------
// Orders and the ingest lane: what the exec suites submit, and how they read the answer
// ------------------------------------------------------------------------------------------------

/// A EURUSD market order in `qty` UNITS (the fake's grid is unconstrained, so any qty round-trips).
pub fn market(coid: &str, side: i32, qty: f64) -> OrderRequest {
    OrderRequest {
        client_order_id: coid.into(),
        venue: "ctrader".into(),
        symbol: "EURUSD".into(),
        side,
        qty,
        order_type: vike_model::OrderType::Market,
        ..Default::default()
    }
}

/// [`market`] as a 1,000-unit BUY: the side and qty the single-order suites pinned as constants.
pub fn market_buy(coid: &str) -> OrderRequest {
    market(coid, 1, 1000.0)
}

/// Drain everything currently on the ingest lane into a flat `Vec<Event>`. The receiver is a tokio
/// mpsc; `try_recv` is non-blocking. The types are fully qualified, so a caller's file imports only
/// what its own code names.
macro_rules! drain {
    ($rx:expr) => {{
        let mut out: Vec<::vike_model::events::Event> = Vec::new();
        while let Ok(ing) = $rx.try_recv() {
            if let ::vike_exec::Ingest::Event(e) = ing {
                out.push(e);
            }
        }
        out
    }};
}
#[allow(unused_imports, clippy::allow_attributes)] // unused in a binary that never drains
pub(crate) use drain;

/// Poll the ingest lane until `want` events have accumulated (or a 5s deadline), returning them all.
pub fn collect(rx: &mut tokio::sync::mpsc::Receiver<Ingest>, want: usize) -> Vec<Event> {
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut got: Vec<Event> = Vec::new();
    while got.len() < want && Instant::now() < deadline {
        got.extend(drain!(rx));
        if got.len() < want {
            std::thread::sleep(Duration::from_millis(20));
        }
    }
    got
}

/// Drain and DISCARD every event for `coid` until its terminal `OrderFilled` is seen, so an OPEN's
/// position is TRACKED before the next step: `plan_reduce` reads the position map, and an order
/// submitted before the open filled would find it empty and route to OPEN.
pub fn wait_open_filled(rx: &mut tokio::sync::mpsc::Receiver<Ingest>, coid: &str) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        for e in drain!(rx) {
            if let Event::OrderFilled(f) = e
                && f.client_order_id == coid
            {
                return;
            }
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("open {coid} never filled");
}
