//! Shared test-only in-process fake cTrader server, used by every integration test that needs to
//! exercise `transport::connection::connect_and_auth` end-to-end. Binds
//! `127.0.0.1:0` (PLAINTEXT — no TLS), reads framed `ProtoMessage`s, records each `payload_type`,
//! and replies with scripted response frames the caller scripts via `FakeCtrader::script`/
//! [`FakeCtrader::start_scripted`]. Not a production component; it exists purely so `conn`'s
//! handshake can be exercised without a live socket. It wraps its side of each socket in the
//! connection's own `crate::transport::connection::stream::EofStream`.
//!
//! Behind `test-support`, which only this crate's self dev-dependency turns on: it lived in
//! `tests/common/`, where each of the twenty-four binaries that declared `mod common;` compiled its
//! own copy; here the library compiles it ONCE and each binary imports `vike_ctrader::test_support::…`.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use crate::execution::ExecutionClientCtrader;
use crate::transport::connection::ActorHandle;
use vike_execution::{EventSender, Ingest};
use vike_marketdata::Bar;
use vike_model::OrderRequest;
use vike_model::events::Event;

mod close_script;
mod fake;
mod foreign_book;
mod serve;
mod symbol_maps;
mod symbol_state_script;
mod trendbar_outage;

// The shared DataClientSink doubles (testing-arch Phase 4d) — `NoopSink` for tests that only need
// a warm body (handshake/exec), `RecordingSink` (typed accessors: `quotes()`/`forming_bars()`/
// `seeded_bars()`) for the scripted spot-event (`tests/offline/data_spot.rs`) and historical-seed
// (`tests/offline/data_bars.rs`) integration tests.
pub use {
    close_script::{
        CLOSE_POSITION_ID_BASE, CloseServePolicy, ExitLookup, GONE_POSITION_ID,
        RESTING_EXIT_ORDER_CLIENT_ID, RESTING_EXIT_ORDER_ID, UNHEARD_DEAL_ID,
        UNHEARD_OPENER_DEAL_ID, UNHEARD_OPENER_ORDER_ID, UNHEARD_ORDER_ID, UnheardOrder,
        deal_list_frame,
    },
    fake::{AccountsScript, FakeCtrader, ReconnectGate},
    foreign_book::ForeignBook,
    serve::{
        AcceptedToken, AmendReply, FAKE_FILL_PRICE, FAKE_ORDER_ID, INFLIGHT_FILL_ORDER_CLIENT_ID,
        INFLIGHT_FILL_PRICE, RECONCILE_PENDING_ORDER_CLIENT_ID, RECONCILE_PENDING_ORDER_ID,
        SEED_BAR_NEWER_MINUTES, SEED_BAR_OLDER_MINUTES, SPOT_EVENT_TIMESTAMP_MS, TrendbarsFault,
    },
    symbol_maps::{
        KEPT_FOR_TESTS, ListedSymbol, eurusd_symbol_map, keep_the_accounts_listing,
        kept_list_for_tests, listed, listed_rows, usdjpy_symbol_map,
    },
    symbol_state_script::SymbolStateControl,
    trendbar_outage::TrendbarOutage,
    vike_data::{NoopSink, RecordingSink},
};

/// One recorded `DataClientSink::seed_bars` call: `(venue, symbol, interval, bars)`.
pub type SeedBarsCall = (String, String, String, Vec<Bar>);

// ── The operator HALT sentinel, for the tests that submit an opening order ───────────────────────

/// A sentinel path THIS test process names and never creates.
///
/// ⚠ **This used to be the cure for a hazard that no longer exists, and it is kept for a different
/// reason.** `ExecutionClientCtrader`'s default was ARMED at the PROCESS-WIDE sentinel
/// (`vike_boot::halt::halt_path_from_env`), which made every test that submits an OPENING
/// order depend on whether that file happened to exist on the box running it — MEASURED on the CI box, 13
/// tests in this crate went red with a real file at the resolved path, and CI was green only
/// because no runner had one. Decision 0099 removed the default: a client is handed its sentinel by
/// its start (`ExecutionClientCtrader::with_halt_path`) and consults nothing process-wide, so no file on the box
/// can change a verdict here, and the re-run harness that proved it
/// (`assert_indifferent_to_an_engaged_halt_sentinel`) is gone with the hazard.
///
/// What this still buys is a WIRED client: one built with no path logs `HALT KILL SWITCH IS NOT
/// WIRED` at `error` the first time it is asked, which is the right noise for a start that forgot
/// and the wrong noise for a test that did not. The path is unique per (process, call) so parallel
/// tests cannot collide, and the directory is deliberately NOT created — a test that wants to ENGAGE
/// a halt makes its own file, as `tests/offline/execution_halt.rs` does.
///
/// ⚠ **This one NAMES a path and touches the disk nowhere**, which is why it is still a bare
/// `PathBuf` while every sibling in this crate moved to a bound `tempfile::TempDir`: with no
/// `create_dir_all` there is nothing to leak and nothing for a reused PID to collide with, and the
/// halt stays unengaged because the FILE is absent whether or not its parent exists. Do not "fix"
/// it by minting a temp directory — the guard would have to be RETURNED, and every call site
/// (through [`execution_with_no_halt`], across most of this crate's test binaries) wants a client, not a
/// tuple. Do not COPY it either: the moment a caller creates the path, it is the leaking idiom
/// `crates/bridges/ctrader/tests/offline/execution_halt.rs`'s `unique_sentinel` describes.
///
/// `root` is a PARAMETER (every caller passes `std::env::temp_dir()`): this is `src/`, and a library
/// may not read the system temp directory itself (`crates/vike-ops/tests/hygiene/system_temp_gate.rs`).
pub fn unengaged_halt_sentinel(root: &Path, tag: &str) -> PathBuf {
    static N: AtomicU64 = AtomicU64::new(0);
    let n = N.fetch_add(1, Ordering::Relaxed);
    root.join(format!("vike-ctrader-no-halt-{}-{n}-{tag}", std::process::id())).join("HALT")
}

/// [`ExecutionClientCtrader::new`] handed [`unengaged_halt_sentinel`] under `root` — the constructor every
/// NON-live test in this crate that submits an opening order uses, so the client is wired the way a
/// start wires it and its verdict cannot depend on a kill switch.
pub fn execution_with_no_halt(
    handle: ActorHandle,
    events: EventSender,
    root: &Path,
    tag: &str,
) -> ExecutionClientCtrader {
    ExecutionClientCtrader::new(handle, events).with_halt_path(unengaged_halt_sentinel(root, tag))
}

// ------------------------------------------------------------------------------------------------
// Orders and the ingest lane: what the exec suites submit, and how they read the answer
// ------------------------------------------------------------------------------------------------

/// A EURUSD market order in `qty` UNITS (the fake's grid is unconstrained, so any qty round-trips).
pub fn market(order_client_id: &str, side: i32, qty: f64) -> OrderRequest {
    OrderRequest {
        order_client_id: order_client_id.into(),
        venue: "ctrader".into(),
        symbol: "EURUSD".into(),
        side,
        qty,
        order_type: vike_model::OrderType::Market,
        ..Default::default()
    }
}

/// [`market`] as a 1,000-unit BUY: the side and qty the single-order suites pinned as constants.
pub fn market_buy(order_client_id: &str) -> OrderRequest {
    market(order_client_id, 1, 1000.0)
}

/// Drain everything currently on the ingest lane into a flat `Vec<Event>`. The receiver is a tokio
/// mpsc; `try_recv` is non-blocking. The types are fully qualified, so a caller's file imports only
/// what its own code names. Exported, so a test names it `vike_ctrader::drain`.
#[macro_export]
macro_rules! drain {
    ($rx:expr) => {{
        let mut out: Vec<::vike_model::events::Event> = Vec::new();
        while let Ok(ing) = $rx.try_recv() {
            if let ::vike_execution::Ingest::Event(e) = ing {
                out.push(e);
            }
        }
        out
    }};
}

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

/// Drain and DISCARD every event for `order_client_id` until its terminal `OrderFilled` is seen, so an OPEN's
/// position is TRACKED before the next step: `plan_reduce` reads the position map, and an order
/// submitted before the open filled would find it empty and route to OPEN.
pub fn wait_open_filled(rx: &mut tokio::sync::mpsc::Receiver<Ingest>, order_client_id: &str) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        for e in drain!(rx) {
            if let Event::OrderFilled(f) = e
                && f.order_client_id == order_client_id
            {
                return;
            }
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("open {order_client_id} never filled");
}

/// The DEMO addresses of the release's seed (`vike_venues::venue_urls::VENUE_URL_SEED`): what a box's
/// `venue_url` table holds until an operator moves a row, and so what every smoke dials.
pub fn demo_endpoints() -> crate::config::CtraderUrls {
    let seed = vike_venues::venue_urls::VenueUrls::seed(crate::transport::connection::VENUE);
    crate::config::ctrader_host(&seed, vike_model::AccountTier::Demo)
        .expect("the release's seed holds ctrader's demo rows")
}

/// An OAuth token pair `access`/`refresh`, valid for the venue's 30 days.
pub fn token(access: &str, refresh: &str) -> crate::transport::oauth::Token {
    crate::transport::oauth::Token {
        access_token: access.to_string(),
        refresh_token: refresh.to_string(),
        expires_in: 2_592_000,
    }
}

/// Every terminal rejection reason waiting on the ingest lane, in arrival order (drains it).
pub fn rejected(rx: &mut tokio::sync::mpsc::Receiver<Ingest>) -> Vec<String> {
    let mut out = Vec::new();
    while let Ok(ing) = rx.try_recv() {
        if let Ingest::Event(Event::OrderRejected(r)) = ing {
            out.push(r.reason.to_string());
        }
    }
    out
}
