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

use std::collections::HashSet;
use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use prost::Message;

use vike_ctrader::conn::ActorHandle;
use vike_ctrader::exec::CtraderExec;
use vike_ctrader::framing::{self, FrameReader};
use vike_ctrader::proto::{
    pt, ProtoMessage, ProtoOaAccountAuthRes, ProtoOaApplicationAuthRes, ProtoOaClosePositionReq,
    ProtoOaCtidTraderAccount, ProtoOaDeal, ProtoOaErrorRes, ProtoOaExecutionEvent,
    ProtoOaExecutionType, ProtoOaGetAccountListByAccessTokenRes, ProtoOaGetTrendbarsReq,
    ProtoOaGetTrendbarsRes, ProtoOaLightSymbol, ProtoOaNewOrderReq, ProtoOaOrder, ProtoOaOrderType,
    ProtoOaPosition, ProtoOaPositionStatus, ProtoOaReconcileRes, ProtoOaSpotEvent,
    ProtoOaSubscribeSpotsRes, ProtoOaSymbol, ProtoOaSymbolByIdRes, ProtoOaSymbolsListRes,
    ProtoOaTradeData, ProtoOaTradeSide, ProtoOaTrader, ProtoOaTraderRes, ProtoOaTrendbar,
};
use vike_exec::EventSender;
use vike_model::Bar;

// The shared LiveDataSink doubles (testing-arch Phase 4d) — `NoopSink` for tests that only need
// a warm body (handshake/exec), `RecordingSink` (typed accessors: `quotes()`/`forming_bars()`/
// `seeded_bars()`) for the scripted spot-event (`tests/data_spot.rs`) and historical-seed
// (`tests/data_bars.rs`) integration tests.
// (Per-binary compilation: each integration test uses only a subset of these re-exports —
// same rationale as the file-top dead_code allow.)
#[allow(unused_imports)]
pub use vike_data::{NoopSink, RecordingSink};

/// One recorded `LiveDataSink::seed_bars` call: `(venue, symbol, interval, bars)`.
pub type SeedBarsCall = (String, String, String, Vec<Bar>);

/// A `Read`/`Write` stream that flags EOF (a `read` returning `Ok(0)`) so the serve loop can break
/// instead of busy-spinning on a closed socket — a test-local duplicate of `conn::EofStream`
/// (that one is `pub(crate)`, unreachable from `tests/`).
struct EofStream<S> {
    inner: S,
    eof: Arc<AtomicBool>,
}

impl<S> EofStream<S> {
    fn new(inner: S) -> (Self, Arc<AtomicBool>) {
        let eof = Arc::new(AtomicBool::new(false));
        (Self { inner, eof: eof.clone() }, eof)
    }
}

impl<S: Read> Read for EofStream<S> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buf)?;
        if n == 0 {
            self.eof.store(true, Ordering::SeqCst);
        }
        Ok(n)
    }
}

impl<S: Write> Write for EofStream<S> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.inner.write(buf)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

/// Which `GetAccountListByAccessToken` response the fake server scripts for a given run — lets
/// tests exercise both the happy path and the "no demo account" (`ConnError::NoAccounts`) path.
#[derive(Clone, Copy)]
pub enum AccountsScript {
    /// One demo (non-live) account, ctid=99 — the handshake happy path.
    OneDemo,
    /// One LIVE-only account, zero demo accounts — must surface `ConnError::NoAccounts`.
    OnlyLive,
}

/// A running fake cTrader server. The listener thread is detached; the test drives it purely via
/// the connecting client (`conn::connect_and_auth`) and inspects [`FakeCtrader::assert_saw`]
/// afterwards.
pub struct FakeCtrader {
    addr: SocketAddr,
    seen: Arc<Mutex<Vec<u32>>>,
    /// Decoded `(position_id, volume)` of every `ProtoOAClosePositionReq` the server received — the
    /// close-scripted serve loop records them so `tests/exec_close.rs` can assert the exact
    /// close-by-position-id legs the bridge sent. Empty for every non-close serve mode.
    closes: Arc<Mutex<Vec<(i64, i64)>>>,
    /// `position_id`s still OPEN at the fake venue — populated by [`FakeCtrader::start_close_preseeded`]
    /// and dropped as `serve_close` fully closes each, so a `close_all` flatten test can observe the
    /// venue reaching zero positions ([`FakeCtrader::open_position_count`]). Empty for every other mode.
    open_ids: Arc<Mutex<HashSet<i64>>>,
}

impl FakeCtrader {
    /// Bind an ephemeral port and serve the scripted handshake (one demo account) to the first
    /// client that connects.
    pub fn start_scripted() -> Self {
        Self::start(AccountsScript::OneDemo)
    }

    /// Bind an ephemeral port and serve the scripted handshake to the first client that connects,
    /// with the given `GetAccountListByAccessToken` response script.
    pub fn start(script: AccountsScript) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind fake ctrader");
        let addr = listener.local_addr().expect("local_addr");
        let seen = Arc::new(Mutex::new(Vec::new()));
        let seen_thread = seen.clone();
        thread::Builder::new()
            .name("fake-ctrader".into())
            .spawn(move || {
                if let Ok((stream, _peer)) = listener.accept() {
                    serve(stream, seen_thread, script);
                }
            })
            .expect("spawn fake ctrader");
        FakeCtrader {
            addr,
            seen,
            closes: Arc::new(Mutex::new(Vec::new())),
            open_ids: Arc::new(Mutex::new(HashSet::new())),
        }
    }

    /// Bind an ephemeral port and serve the full scripted handshake — then answer `RECONCILE_REQ`
    /// with SILENCE, forever, keeping the socket open and every other request answered normally.
    ///
    /// This is the ONE venue behavior that costs the client its whole reconcile deadline, which is
    /// why it exists: a rejection comes back as an `ERROR_RES` on the spot and a dead venue is EOF,
    /// so neither can pin a TIMEOUT. Used by `tests/exec_close.rs`'s
    /// `a_mute_venue_costs_the_mount_the_seed_bound_not_the_reconnect_one` to hold
    /// `conn::SEED_TIMEOUT` to the synchronous mount path.
    pub fn start_mute_reconcile() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind fake ctrader");
        let addr = listener.local_addr().expect("local_addr");
        let seen = Arc::new(Mutex::new(Vec::new()));
        let seen_thread = seen.clone();
        thread::Builder::new()
            .name("fake-ctrader-mute-reconcile".into())
            .spawn(move || {
                if let Ok((stream, _peer)) = listener.accept() {
                    serve_mute_reconcile(stream, seen_thread, AccountsScript::OneDemo);
                }
            })
            .expect("spawn fake ctrader-mute-reconcile");
        FakeCtrader {
            addr,
            seen,
            closes: Arc::new(Mutex::new(Vec::new())),
            open_ids: Arc::new(Mutex::new(HashSet::new())),
        }
    }

    /// Bind an ephemeral port and serve the scripted handshake, then reply to any `NEW_ORDER_REQ`
    /// with an `ERROR_RES` echoing the order's envelope `clientMsgId` (mirrors cTrader rejecting a
    /// submit at the protocol level) instead of the accepted/filled execution events. Used by
    /// `tests/exec_reject.rs` to prove a venue reject surfaces as a terminal `OrderRejected` so the
    /// order never sits in `Submitted` forever.
    pub fn start_reject_scripted() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind fake ctrader");
        let addr = listener.local_addr().expect("local_addr");
        let seen = Arc::new(Mutex::new(Vec::new()));
        let seen_thread = seen.clone();
        thread::Builder::new()
            .name("fake-ctrader-reject".into())
            .spawn(move || {
                if let Ok((stream, _peer)) = listener.accept() {
                    serve_reject(stream, seen_thread, AccountsScript::OneDemo);
                }
            })
            .expect("spawn fake ctrader-reject");
        FakeCtrader {
            addr,
            seen,
            closes: Arc::new(Mutex::new(Vec::new())),
            open_ids: Arc::new(Mutex::new(HashSet::new())),
        }
    }

    /// Bind an ephemeral port and serve the scripted handshake, then reply to any
    /// `GET_TRENDBARS_REQ` with an `ERROR_RES` echoing the request's envelope `clientMsgId`
    /// (empty since the F2b fix — see `conn::write_command`'s doc) instead of the scripted
    /// `GET_TRENDBARS_RES`. Used by `tests/data_bars.rs` to prove a failing NON-order request on a
    /// combined data+exec connection does NOT synthesize a phantom `Event::OrderRejected` (the F2b
    /// regression: `on_error_res` used to fire for ANY non-empty `clientMsgId`, and the old static
    /// per-verb label "gtb" would have tripped it).
    pub fn start_reject_trendbars_scripted() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind fake ctrader");
        let addr = listener.local_addr().expect("local_addr");
        let seen = Arc::new(Mutex::new(Vec::new()));
        let seen_thread = seen.clone();
        thread::Builder::new()
            .name("fake-ctrader-reject-tb".into())
            .spawn(move || {
                if let Ok((stream, _peer)) = listener.accept() {
                    serve_reject_trendbars(stream, seen_thread, AccountsScript::OneDemo);
                }
            })
            .expect("spawn fake ctrader-reject-tb");
        FakeCtrader {
            addr,
            seen,
            closes: Arc::new(Mutex::new(Vec::new())),
            open_ids: Arc::new(Mutex::new(HashSet::new())),
        }
    }

    /// Bind an ephemeral port and, for the FIRST `fail_count` connections, accept then IMMEDIATELY
    /// close the socket (no handshake reply) — a transient connect/handshake blip the client sees
    /// as `ConnError::Io` (a reset or EOF mid-handshake). The (`fail_count`+1)-th and every later
    /// connection is served the full happy-path handshake. Used by `tests/conn_retry.rs` to prove
    /// the bounded connect-retry rides out N transient failures and still connects.
    pub fn start_transient_then_serve(fail_count: usize) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind fake ctrader");
        let addr = listener.local_addr().expect("local_addr");
        let seen = Arc::new(Mutex::new(Vec::new()));
        let seen_thread = seen.clone();
        thread::Builder::new()
            .name("fake-ctrader-transient".into())
            .spawn(move || {
                let mut failed = 0usize;
                while let Ok((stream, _peer)) = listener.accept() {
                    if failed < fail_count {
                        failed += 1;
                        // Drop the socket without replying — the client's handshake read hits a
                        // reset/EOF, i.e. a TRANSIENT `ConnError::Io` the retry loop backs off on.
                        drop(stream);
                        continue;
                    }
                    serve(stream, seen_thread.clone(), AccountsScript::OneDemo);
                }
            })
            .expect("spawn fake ctrader-transient");
        FakeCtrader {
            addr,
            seen,
            closes: Arc::new(Mutex::new(Vec::new())),
            open_ids: Arc::new(Mutex::new(HashSet::new())),
        }
    }

    /// Bind an ephemeral port and answer EVERY connection's `APPLICATION_AUTH_REQ` with an
    /// auth-rejection `ERROR_RES` (`ConnError::Venue`) — a PERMANENT failure. One serve thread per
    /// connection, so a (buggy) retry would show as MULTIPLE `APPLICATION_AUTH_REQ`s; correct
    /// fail-fast behavior leaves exactly one. Used by `tests/conn_retry.rs`.
    pub fn start_reject_app_auth() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind fake ctrader");
        let addr = listener.local_addr().expect("local_addr");
        let seen = Arc::new(Mutex::new(Vec::new()));
        let seen_thread = seen.clone();
        thread::Builder::new()
            .name("fake-ctrader-reject-appauth".into())
            .spawn(move || {
                while let Ok((stream, _peer)) = listener.accept() {
                    let seen_conn = seen_thread.clone();
                    thread::spawn(move || {
                        serve_inner(
                            stream,
                            seen_conn,
                            AccountsScript::OneDemo,
                            None,
                            RejectMode::AppAuth,
                            false,
                            false,
                        );
                    });
                }
            })
            .expect("spawn fake ctrader-reject-appauth");
        FakeCtrader {
            addr,
            seen,
            closes: Arc::new(Mutex::new(Vec::new())),
            open_ids: Arc::new(Mutex::new(HashSet::new())),
        }
    }

    /// Bind an ephemeral port and serve, on EVERY connection, the scripted handshake followed by
    /// one `RECONCILE_RES` — then CLOSE. That is exactly what a cTrader server does to a socket
    /// that goes idle between reconcile passes: the first pass succeeds, and the connection is
    /// gone by the next one. Used by `tests/recon_client_revive.rs` to prove
    /// `CtraderReconClient`'s lazy idle-revive re-handshakes and completes the SECOND pass
    /// (and, with revive disabled, that the second pass genuinely fails without it).
    pub fn start_close_after_each_reconcile() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind fake ctrader");
        let addr = listener.local_addr().expect("local_addr");
        let seen = Arc::new(Mutex::new(Vec::new()));
        let seen_thread = seen.clone();
        thread::Builder::new()
            .name("fake-ctrader-idle-close".into())
            .spawn(move || {
                while let Ok((stream, _peer)) = listener.accept() {
                    // ONE serve thread per connection. The client's lazy revive opens a fresh
                    // socket and re-runs the full handshake on it BEFORE dropping the old reader
                    // (`revive_if_idle`), so during a revive TWO connections are briefly open at
                    // once; a single-threaded accept loop, still blocked serving the first, would
                    // never accept the second and its handshake would time out. Real cTrader serves
                    // connections concurrently, so the fake must too.
                    let seen_conn = seen_thread.clone();
                    thread::spawn(move || {
                        serve_drop_after(
                            stream,
                            seen_conn,
                            AccountsScript::OneDemo,
                            pt::RECONCILE_REQ,
                        );
                    });
                }
            })
            .expect("spawn fake ctrader-idle-close");
        FakeCtrader {
            addr,
            seen,
            closes: Arc::new(Mutex::new(Vec::new())),
            open_ids: Arc::new(Mutex::new(HashSet::new())),
        }
    }

    /// Bind an ephemeral port and serve the scripted handshake, then a STATEFUL exec loop that
    /// models a HEDGING account: every `NEW_ORDER_REQ` OPENS a fresh position (distinct
    /// `positionId`, incrementing `openTimestamp` so the bridge's FIFO order is deterministic) and
    /// replies `ORDER_ACCEPTED` + `ORDER_FILLED` carrying that `position` ref; every
    /// `CLOSE_POSITION_REQ` closes (or partially closes) the named position and replies a single
    /// `ORDER_FILLED` whose `deal` reduces it (opposite `tradeSide`, `filledVolume` = the requested
    /// close volume) plus the updated `position` ref (OPEN-reduced or CLOSED). The decoded close
    /// requests are recorded for [`FakeCtrader::close_requests`]. Used by `tests/exec_close.rs`.
    pub fn start_close_scripted() -> Self {
        Self::start_close_with_policy(CloseServePolicy::Fill)
    }

    /// [`Self::start_close_scripted`] with an explicit [`CloseServePolicy`] — scripts the
    /// close-failure / concurrency edges (`Hold`, `FillThenReject`, `ErrorResThenFill`).
    pub fn start_close_with_policy(policy: CloseServePolicy) -> Self {
        Self::spawn_close(policy, Vec::new(), ReconcileFidelity::Faithful)
    }

    /// [`Self::start_close_scripted`], but its `RECONCILE_RES` also carries `unreadable`
    /// **ERROR-status** position rows the bridge cannot classify.
    ///
    /// ⚠ **This exists because the server could not previously express a FAILURE, and that made a
    /// safety property untestable.** Every position row `serve_close` emits is
    /// `PositionStatusOpen`, so a reconcile answer was always readable in full and the book was
    /// always authoritative — which meant the one state that must NOT refuse under
    /// `halt_admit = "verify"` (a book rebuilt from an answer with a hole in it) could not be
    /// reached from a test at all. A rig that cannot represent the failure it is asked to rule out
    /// proves nothing. `crates/bridges/ctrader/src/positions.rs`'s `reconcile_rows` is what counts
    /// these rows, and `conn.rs`'s `rebuild_position_book` is what declines to call the result
    /// evidence.
    ///
    /// ERROR is used rather than an unknown numeric status because it is a real cTrader state
    /// (`ProtoOAPositionStatus.POSITION_STATUS_ERROR`) that this bridge genuinely cannot map to
    /// exposure — the honest shape of the hole, not a synthetic one.
    pub fn start_close_scripted_with_unreadable_positions(unreadable: usize) -> Self {
        Self::start_close_preseeded_with_unreadable_positions(&[], unreadable)
    }

    /// [`Self::start_close_preseeded`] whose `RECONCILE_RES` additionally carries `unreadable`
    /// ERROR-status rows. The combination is what isolates the HOLE arm: with a real position
    /// reported faithfully the book has positive COVERAGE of the symbol, so only the unreadable
    /// count can be what withholds the evidence flag.
    pub fn start_close_preseeded_with_unreadable_positions(
        seed: &[(i32, i64)],
        unreadable: usize,
    ) -> Self {
        Self::spawn_close(
            CloseServePolicy::Fill,
            Self::preseed_rows(seed),
            ReconcileFidelity::Unreadable(unreadable),
        )
    }

    /// ⚠ **THE TRAP RIG.** A venue that genuinely HOLDS `seed` (as [`Self::start_close_preseeded`])
    /// and whose `RECONCILE_RES` reports NONE of it — a well-formed, fully classifiable, EMPTY
    /// answer for the right account, from an account that is not flat.
    ///
    /// This is the shape a decode-completeness check cannot see: [`ReconcileFidelity::Unreadable`]
    /// injects rows the bridge fails to classify and is COUNTED, but a row the venue never sent is
    /// uncountable, so `unreadable == 0`, the book calls itself authoritative, and an absence in it
    /// becomes a manufactured `PositionEvidence::Flat` — i.e. a REFUSED exit from a position the
    /// venue is holding. `crates/bridges/ctrader/tests/exec_halt.rs`'s
    /// `verify_admits_the_exit_from_a_position_the_reconcile_answer_omitted` is the probe.
    pub fn start_close_preseeded_omitted_from_reconcile(seed: &[(i32, i64)]) -> Self {
        Self::spawn_close(
            CloseServePolicy::Fill,
            Self::preseed_rows(seed),
            ReconcileFidelity::OmitsEveryPosition,
        )
    }

    /// A venue holding `seed` whose `RECONCILE_RES` is another account's ([`OTHER_CTID`]) FLAT
    /// book. Nothing in the frame is undecodable; only the account is wrong — and an answer about
    /// another account says NOTHING about ours, so folding its emptiness in and calling the result
    /// authoritative manufactures a `Flat` out of a stranger.
    pub fn start_close_preseeded_reconcile_for_another_account(seed: &[(i32, i64)]) -> Self {
        Self::spawn_close(
            CloseServePolicy::Fill,
            Self::preseed_rows(seed),
            ReconcileFidelity::ForAnotherAccount,
        )
    }

    /// Bind an ephemeral port and serve the close-scripted handshake over a book PRE-SEEDED with
    /// `seed` positions — each `(side_sign +1/-1, volume_centi)`, assigned a sequential
    /// `position_id` from [`CLOSE_POSITION_ID_BASE`] in seed order. Unlike [`Self::start_close_scripted`]
    /// (which only ever opens positions via `NEW_ORDER_REQ`), this models positions that already
    /// exist BEFORE the client connects — the "pre-existing hedged book a fresh connect never
    /// tracked" case `CtraderExec::close_all` targets, which a reduce ORDER cannot build through the
    /// bridge (an opposite `submit` routes to CLOSE, never a coexisting hedge). Every close FILLS
    /// (`CloseServePolicy::Fill`), and [`Self::open_position_count`] tracks the book down to flat.
    /// Used by `tests/exec_close.rs`'s `close_all` flatten test.
    pub fn start_close_preseeded(seed: &[(i32, i64)]) -> Self {
        Self::spawn_close(
            CloseServePolicy::Fill,
            Self::preseed_rows(seed),
            ReconcileFidelity::Faithful,
        )
    }

    /// `(side_sign, volume_centi)` pairs → the `(position_id, trade_side, volume)` rows
    /// [`Self::spawn_close`] pre-opens, ids assigned sequentially from [`CLOSE_POSITION_ID_BASE`]
    /// in seed order.
    fn preseed_rows(seed: &[(i32, i64)]) -> Vec<(i64, i32, i64)> {
        seed.iter()
            .enumerate()
            .map(|(i, &(side_sign, volume))| {
                let position_id = CLOSE_POSITION_ID_BASE + i as i64;
                let trade_side = if side_sign < 0 {
                    ProtoOaTradeSide::Sell as i32
                } else {
                    ProtoOaTradeSide::Buy as i32
                };
                (position_id, trade_side, volume)
            })
            .collect()
    }

    /// Shared close-server spawn. `preseed` positions (`(position_id, trade_side, volume_centi)`) are
    /// already OPEN before the client connects — `serve_close` folds them into its position map and
    /// drops each from `open_ids` as a close flattens it, so [`Self::open_position_count`] observes
    /// the venue reaching flat. `fidelity` decides what each `RECONCILE_RES` says about that book —
    /// see [`ReconcileFidelity`].
    fn spawn_close(
        policy: CloseServePolicy,
        preseed: Vec<(i64, i32, i64)>,
        fidelity: ReconcileFidelity,
    ) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind fake ctrader");
        let addr = listener.local_addr().expect("local_addr");
        let seen = Arc::new(Mutex::new(Vec::new()));
        let closes = Arc::new(Mutex::new(Vec::new()));
        let open_ids = Arc::new(Mutex::new(HashSet::new()));
        {
            let mut ids = open_ids.lock().expect("open_ids lock");
            for (position_id, _, _) in &preseed {
                ids.insert(*position_id);
            }
        }
        let seen_thread = seen.clone();
        let closes_thread = closes.clone();
        let open_ids_thread = open_ids.clone();
        thread::Builder::new()
            .name("fake-ctrader-close".into())
            .spawn(move || {
                if let Ok((stream, _peer)) = listener.accept() {
                    serve_close(
                        stream,
                        seen_thread,
                        closes_thread,
                        open_ids_thread,
                        AccountsScript::OneDemo,
                        policy,
                        preseed,
                        fidelity,
                    );
                }
            })
            .expect("spawn fake ctrader-close");
        FakeCtrader { addr, seen, closes, open_ids }
    }

    /// How many pre-seeded positions are still OPEN at the fake venue — decremented as `serve_close`
    /// fully closes each. `0` once a flatten has cleared the book. Only meaningful for a
    /// [`Self::start_close_preseeded`] server (empty, hence `0`, for every other mode).
    pub fn open_position_count(&self) -> usize {
        self.open_ids.lock().expect("open_ids lock").len()
    }

    /// Poll [`Self::open_position_count`] down to `0` (or until `timeout`), then return it — bridges
    /// the actor's command-drain + close-round-trip latency without a brittle fixed sleep.
    pub fn wait_until_flat(&self, timeout: Duration) -> usize {
        let deadline = std::time::Instant::now() + timeout;
        loop {
            let n = self.open_position_count();
            if n == 0 || std::time::Instant::now() >= deadline {
                return n;
            }
            thread::sleep(Duration::from_millis(20));
        }
    }

    /// The `(position_id, volume)` of every `ProtoOAClosePositionReq` the server has received so
    /// far, in arrival order — for asserting the exact FIFO close legs the bridge sent.
    pub fn close_requests(&self) -> Vec<(i64, i64)> {
        self.closes.lock().expect("closes lock").clone()
    }

    /// Poll [`FakeCtrader::close_requests`] until it holds at least `n` entries or `timeout`
    /// elapses, then return them — bridges the actor thread's up-to-1s command-drain latency.
    pub fn wait_for_closes(&self, n: usize, timeout: Duration) -> Vec<(i64, i64)> {
        let deadline = std::time::Instant::now() + timeout;
        loop {
            let got = self.close_requests();
            if got.len() >= n || std::time::Instant::now() >= deadline {
                return got;
            }
            thread::sleep(Duration::from_millis(20));
        }
    }

    /// The address the client should connect to.
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// Assert every named payload type was received (order-independent superset check).
    pub fn assert_saw(&self, expected: &[&str]) {
        let seen = self.seen.lock().expect("seen lock");
        let names: Vec<String> = seen.iter().map(|&pt| pt_name(pt)).collect();
        for want in expected {
            assert!(
                names.iter().any(|n| n == want),
                "expected to have seen {want}, but only saw {names:?}",
            );
        }
    }

    /// Non-panicking check — every named payload type has been received so far. The actor
    /// thread's read loop can block up to its 1s socket read-timeout before draining a just-queued
    /// `Command` (see `conn::connect_and_auth`'s `set_read_timeout`), so a caller asserting on a
    /// command that was JUST sent should poll this (e.g. via [`FakeCtrader::wait_until_saw`])
    /// rather than sleep a fixed, possibly-too-short duration.
    pub fn saw_all(&self, expected: &[&str]) -> bool {
        let seen = self.seen.lock().expect("seen lock");
        let names: Vec<String> = seen.iter().map(|&pt| pt_name(pt)).collect();
        expected.iter().all(|want| names.iter().any(|n| n == want))
    }

    /// Poll [`FakeCtrader::saw_all`] until it's true or `timeout` elapses, then assert it held —
    /// bridges the actor thread's up-to-1s command-drain latency without a brittle fixed sleep.
    pub fn wait_until_saw(&self, expected: &[&str], timeout: Duration) {
        let deadline = std::time::Instant::now() + timeout;
        while !self.saw_all(expected) {
            if std::time::Instant::now() >= deadline {
                break;
            }
            thread::sleep(Duration::from_millis(20));
        }
        self.assert_saw(expected);
    }

    /// How many times a named payload type has been received so far — used by the reconnect test
    /// (`tests/conn_reconnect.rs`) to prove a SECOND full handshake happened, not just a first one.
    pub fn count_seen(&self, name: &str) -> usize {
        let seen = self.seen.lock().expect("seen lock");
        seen.iter().map(|&pt| pt_name(pt)).filter(|n| n == name).count()
    }

    /// Poll [`FakeCtrader::count_seen`] until it reaches `at_least` or `timeout` elapses, then
    /// assert it held — the reconnect analogue of [`FakeCtrader::wait_until_saw`] (a reconnect
    /// involves an actual TCP round-trip plus the actor's backoff sleep, so this needs a real
    /// wait, not a fixed one).
    pub fn wait_until_count_at_least(&self, name: &str, at_least: usize, timeout: Duration) {
        let deadline = std::time::Instant::now() + timeout;
        while self.count_seen(name) < at_least {
            if std::time::Instant::now() >= deadline {
                break;
            }
            thread::sleep(Duration::from_millis(20));
        }
        assert!(
            self.count_seen(name) >= at_least,
            "expected to see {name} at least {at_least} times, saw {}",
            self.count_seen(name)
        );
    }

    /// Bind an ephemeral port and serve a scripted handshake that DROPS the connection right
    /// after completing it (immediately after replying to `SYMBOL_BY_ID_REQ`, the last handshake
    /// step) — simulating a disconnect right as the actor's read loop is about to start. Every
    /// LATER connection (the actor's reconnect attempts) is served exactly like
    /// [`FakeCtrader::start_scripted`]. Used by the reconnect test to prove the actor backs off,
    /// reconnects, and replays the FULL handshake (a second `APPLICATION_AUTH_REQ`).
    pub fn start_reconnect_after_handshake() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind fake ctrader");
        let addr = listener.local_addr().expect("local_addr");
        let seen = Arc::new(Mutex::new(Vec::new()));
        let seen_thread = seen.clone();
        thread::Builder::new()
            .name("fake-ctrader-reconnect".into())
            .spawn(move || {
                if let Ok((stream, _peer)) = listener.accept() {
                    serve_drop_after(
                        stream,
                        seen_thread.clone(),
                        AccountsScript::OneDemo,
                        pt::SYMBOL_BY_ID_REQ,
                    );
                }
                while let Ok((stream, _peer)) = listener.accept() {
                    serve(stream, seen_thread.clone(), AccountsScript::OneDemo);
                }
            })
            .expect("spawn fake ctrader-reconnect");
        FakeCtrader {
            addr,
            seen,
            closes: Arc::new(Mutex::new(Vec::new())),
            open_ids: Arc::new(Mutex::new(HashSet::new())),
        }
    }

    /// Bind an ephemeral port and serve a scripted handshake that stays open through the FIRST
    /// `SUBSCRIBE_SPOTS_REQ` (replying to it normally, like [`FakeCtrader::start_scripted`]) but
    /// DROPS the connection right after that reply — simulating a disconnect while a subscription
    /// is active. Every LATER connection (the actor's reconnect attempts) is served exactly like
    /// [`FakeCtrader::start_scripted`]. Used by the subscription-replay test
    /// (`tests/conn_reconnect.rs`) to prove the actor's tracked `spot_subs` set (populated by
    /// `track_subscription` before the write attempt, so a subscribe issued right before an outage
    /// still survives it) is replayed as a SECOND `SUBSCRIBE_SPOTS_REQ` after reconnect.
    pub fn start_drop_after_subscribe() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind fake ctrader");
        let addr = listener.local_addr().expect("local_addr");
        let seen = Arc::new(Mutex::new(Vec::new()));
        let seen_thread = seen.clone();
        thread::Builder::new()
            .name("fake-ctrader-sub-reconnect".into())
            .spawn(move || {
                if let Ok((stream, _peer)) = listener.accept() {
                    serve_drop_after(
                        stream,
                        seen_thread.clone(),
                        AccountsScript::OneDemo,
                        pt::SUBSCRIBE_SPOTS_REQ,
                    );
                }
                while let Ok((stream, _peer)) = listener.accept() {
                    serve(stream, seen_thread.clone(), AccountsScript::OneDemo);
                }
            })
            .expect("spawn fake ctrader-sub-reconnect");
        FakeCtrader {
            addr,
            seen,
            closes: Arc::new(Mutex::new(Vec::new())),
            open_ids: Arc::new(Mutex::new(HashSet::new())),
        }
    }

    /// Bind an ephemeral port and serve a scripted handshake that DROPS the connection right after
    /// completing it (like [`FakeCtrader::start_reconnect_after_handshake`]) — but every LATER
    /// (reconnect) connection additionally answers the `RECONCILE_REQ` the actor now issues on
    /// reconnect (F3) with a scripted `ReconcileRes` carrying ONE pending order
    /// ([`RECONCILE_PENDING_COID`] → [`RECONCILE_PENDING_ORDER_ID`]). Used by the reconcile test to
    /// prove the actor rebuilds its coid→orderId map from the reconcile response after a reconnect.
    pub fn start_reconnect_with_reconcile() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind fake ctrader");
        let addr = listener.local_addr().expect("local_addr");
        let seen = Arc::new(Mutex::new(Vec::new()));
        let seen_thread = seen.clone();
        thread::Builder::new()
            .name("fake-ctrader-reconcile".into())
            .spawn(move || {
                if let Ok((stream, _peer)) = listener.accept() {
                    serve_drop_after(
                        stream,
                        seen_thread.clone(),
                        AccountsScript::OneDemo,
                        pt::SYMBOL_BY_ID_REQ,
                    );
                }
                while let Ok((stream, _peer)) = listener.accept() {
                    serve(stream, seen_thread.clone(), AccountsScript::OneDemo);
                }
            })
            .expect("spawn fake ctrader-reconcile");
        FakeCtrader {
            addr,
            seen,
            closes: Arc::new(Mutex::new(Vec::new())),
            open_ids: Arc::new(Mutex::new(HashSet::new())),
        }
    }

    /// Bind an ephemeral port and serve a scripted handshake that DROPS the connection right after
    /// completing it — but every LATER (reconnect) connection answers the reconnect's
    /// `RECONCILE_REQ` with an in-flight `EXECUTION_EVENT` (a full `ORDER_FILLED` for
    /// [`INFLIGHT_FILL_COID`]) sent BEFORE the `RECONCILE_RES`. Used by the F3 fix-1 test to prove
    /// the actor BUFFERS that exec frame during the reconcile window and replays it (reaching the
    /// ingest lane as an `Event::OrderFilled`) rather than dropping it.
    pub fn start_reconnect_with_inflight_fill() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind fake ctrader");
        let addr = listener.local_addr().expect("local_addr");
        let seen = Arc::new(Mutex::new(Vec::new()));
        let seen_thread = seen.clone();
        thread::Builder::new()
            .name("fake-ctrader-inflight-fill".into())
            .spawn(move || {
                if let Ok((stream, _peer)) = listener.accept() {
                    serve_drop_after(
                        stream,
                        seen_thread.clone(),
                        AccountsScript::OneDemo,
                        pt::SYMBOL_BY_ID_REQ,
                    );
                }
                while let Ok((stream, _peer)) = listener.accept() {
                    serve_inflight_fill(stream, seen_thread.clone(), AccountsScript::OneDemo);
                }
            })
            .expect("spawn fake ctrader-inflight-fill");
        FakeCtrader {
            addr,
            seen,
            closes: Arc::new(Mutex::new(Vec::new())),
            open_ids: Arc::new(Mutex::new(HashSet::new())),
        }
    }
}

/// The `clientOrderId`/`label` the scripted reconnect `ReconcileRes` reports as a pending order —
/// the reconcile test asserts the actor's coid→orderId map resolves this coid after reconnect.
pub const RECONCILE_PENDING_COID: &str = "recon-coid-1";
/// The venue `orderId` paired with [`RECONCILE_PENDING_COID`] in the scripted `ReconcileRes`.
pub const RECONCILE_PENDING_ORDER_ID: i64 = 777_001;

/// Read frames, record their payload type, and write the scripted reply. Loops until the client
/// disconnects (EOF) or errors — the socket stays open after the handshake so the client's actor
/// loop hits read-timeouts (not EOF) during the test window.
fn serve(stream: TcpStream, seen: Arc<Mutex<Vec<u32>>>, script: AccountsScript) {
    serve_inner(stream, seen, script, None, RejectMode::None, false, false);
}

/// Like [`serve`], but answers `RECONCILE_REQ` with NOTHING AT ALL — see
/// [`FakeCtrader::start_mute_reconcile`].
fn serve_mute_reconcile(stream: TcpStream, seen: Arc<Mutex<Vec<u32>>>, script: AccountsScript) {
    serve_inner(stream, seen, script, None, RejectMode::None, false, true);
}

/// Like [`serve`], but replies to `NEW_ORDER_REQ` with an `ERROR_RES` echoing the request's
/// `clientMsgId` (a venue submit-reject) rather than the scripted accepted/filled events.
fn serve_reject(stream: TcpStream, seen: Arc<Mutex<Vec<u32>>>, script: AccountsScript) {
    serve_inner(stream, seen, script, None, RejectMode::Orders, false, false);
}

/// Like [`serve`], but replies to `GET_TRENDBARS_REQ` with an `ERROR_RES` echoing the request's
/// `clientMsgId` rather than the scripted `GET_TRENDBARS_RES` — see
/// [`FakeCtrader::start_reject_trendbars_scripted`].
fn serve_reject_trendbars(stream: TcpStream, seen: Arc<Mutex<Vec<u32>>>, script: AccountsScript) {
    serve_inner(stream, seen, script, None, RejectMode::Trendbars, false, false);
}

/// Like [`serve`], but closes the connection right after replying to the request whose
/// `payload_type` is `drop_after` — used by [`FakeCtrader::start_reconnect_after_handshake`] to
/// simulate a mid-session disconnect deterministically (right as the handshake completes, rather
/// than at some arbitrary later point).
fn serve_drop_after(
    stream: TcpStream,
    seen: Arc<Mutex<Vec<u32>>>,
    script: AccountsScript,
    drop_after: u32,
) {
    serve_inner(stream, seen, script, Some(drop_after), RejectMode::None, false, false);
}

/// Like [`serve`], but answers `RECONCILE_REQ` with an in-flight `ORDER_FILLED` execution event
/// (for [`INFLIGHT_FILL_COID`]) sent BEFORE the `RECONCILE_RES` — see
/// [`FakeCtrader::start_reconnect_with_inflight_fill`].
fn serve_inflight_fill(stream: TcpStream, seen: Arc<Mutex<Vec<u32>>>, script: AccountsScript) {
    serve_inner(stream, seen, script, None, RejectMode::None, true, false);
}

/// Which request `serve_inner` diverts to a scripted `ERROR_RES` instead of the normal happy-path
/// reply — used to script the venue-reject integration tests (`tests/exec_reject.rs`,
/// `tests/data_bars.rs`).
#[derive(Clone, Copy, PartialEq, Eq)]
enum RejectMode {
    /// Normal scripted replies for everything.
    None,
    /// `NEW_ORDER_REQ` gets an `ERROR_RES` instead of accepted/filled execution events.
    Orders,
    /// `GET_TRENDBARS_REQ` gets an `ERROR_RES` instead of `GET_TRENDBARS_RES`.
    Trendbars,
    /// `APPLICATION_AUTH_REQ` gets an auth-rejection `ERROR_RES` instead of `APPLICATION_AUTH_RES` —
    /// the PERMANENT connect-retry case (`ConnError::Venue`, which must fail fast, never retry).
    AppAuth,
}

#[allow(clippy::too_many_arguments)]
fn serve_inner(
    stream: TcpStream,
    seen: Arc<Mutex<Vec<u32>>>,
    script: AccountsScript,
    drop_after: Option<u32>,
    reject: RejectMode,
    reconcile_inflight_fill: bool,
    mute_reconcile: bool,
) {
    stream.set_read_timeout(Some(Duration::from_secs(5))).ok();
    let mut writer = stream.try_clone().expect("clone fake stream");
    let (eof_stream, eof) = EofStream::new(stream);
    let mut reader = FrameReader::new(eof_stream);

    loop {
        match reader.next_frame() {
            Ok(Some(msg)) => {
                let payload_type = msg.payload_type;
                seen.lock().expect("seen lock").push(payload_type);
                // Mute mode: SILENCE for `RECONCILE_REQ` — no reply of any kind, not even an
                // `ERROR_RES`, while the socket stays open and every other request is answered
                // normally. That is the only shape that costs the client its full reconcile
                // deadline (a reject returns on the spot, a dead socket returns at EOF), so it is
                // the shape a timeout BOUND has to be pinned against.
                if mute_reconcile && payload_type == pt::RECONCILE_REQ {
                    continue;
                }
                // Reject mode: answer the scripted request with an ERROR_RES that echoes the
                // request's clientMsgId, so the client can correlate it (or, for the Trendbars
                // case, prove it correctly does NOT correlate it to an order). Orders/Trendbars use
                // a NON-auth `NOT_ENOUGH_MONEY` code; AppAuth uses an auth-rejection code — the
                // PERMANENT connect-retry case the client must fail fast on (`ConnError::Venue`).
                let reject_err: Option<(&str, &str)> = match reject {
                    RejectMode::None => None,
                    RejectMode::Orders => (payload_type == pt::NEW_ORDER_REQ)
                        .then_some(("NOT_ENOUGH_MONEY", "insufficient funds")),
                    RejectMode::Trendbars => (payload_type == pt::GET_TRENDBARS_REQ)
                        .then_some(("NOT_ENOUGH_MONEY", "insufficient funds")),
                    RejectMode::AppAuth => (payload_type == pt::APPLICATION_AUTH_REQ)
                        .then_some(("CH_CLIENT_AUTH_FAILURE", "invalid client credentials")),
                };
                if let Some((error_code, description)) = reject_err {
                    let err = ProtoOaErrorRes {
                        error_code: error_code.to_string(),
                        description: Some(description.to_string()),
                        ..Default::default()
                    };
                    let msg_id = msg.client_msg_id.as_deref().unwrap_or("");
                    let frame = framing::encode(pt::ERROR_RES, &err.encode_to_vec(), msg_id);
                    if writer.write_all(&frame).is_err() {
                        return;
                    }
                    let _ = writer.flush();
                    continue;
                }
                for (reply_type, body) in script_reply(&msg, script, reconcile_inflight_fill) {
                    let frame = framing::encode(reply_type, &body, "srv");
                    if writer.write_all(&frame).is_err() {
                        return;
                    }
                    let _ = writer.flush();
                }
                if drop_after == Some(payload_type) {
                    return; // simulate the connection dying right here
                }
            }
            Ok(None) => {
                if eof.load(Ordering::SeqCst) {
                    break; // client closed
                }
            }
            Err(_) => break,
        }
    }
}

/// The scripted `SPOT_EVENT`'s `timestamp` (epoch milliseconds) — a live-captured value
/// (~2026-07-14) used so `tests/data_spot.rs` can assert it flows through to `QuoteTick::ts`
/// UNCHANGED (milliseconds in, milliseconds out — no further scaling/conversion).
pub const SPOT_EVENT_TIMESTAMP_MS: i64 = 1_784_017_625_503;

/// Map a request payload type to its scripted reply frame(s) `(payload_type, encoded_body)`.
/// Most requests get exactly one reply; `SUBSCRIBE_SPOTS_REQ` gets its `SUBSCRIBE_SPOTS_RES` PLUS
/// an unsolicited `SPOT_EVENT` (EURUSD id=1, bid=113911/ask=113912, timestamp=
/// `SPOT_EVENT_TIMESTAMP_MS` — mirrors real cTrader, which pushes a technical spot event with the
/// current price right after a subscription is accepted) so `tests/data_spot.rs` can assert the
/// actor decodes+routes it to a `LiveDataSink`.
fn script_reply(
    msg: &ProtoMessage,
    script: AccountsScript,
    reconcile_inflight_fill: bool,
) -> Vec<(u32, Vec<u8>)> {
    match msg.payload_type {
        pt::APPLICATION_AUTH_REQ => {
            vec![(pt::APPLICATION_AUTH_RES, ProtoOaApplicationAuthRes::default().encode_to_vec())]
        }
        pt::GET_ACCOUNTS_BY_ACCESS_TOKEN_REQ => {
            let accounts = match script {
                AccountsScript::OneDemo => vec![ProtoOaCtidTraderAccount {
                    ctid_trader_account_id: 99,
                    is_live: Some(false),
                    trader_login: Some(1234),
                    ..Default::default()
                }],
                AccountsScript::OnlyLive => vec![ProtoOaCtidTraderAccount {
                    ctid_trader_account_id: 77,
                    is_live: Some(true),
                    trader_login: Some(5678),
                    ..Default::default()
                }],
            };
            let res = ProtoOaGetAccountListByAccessTokenRes {
                access_token: "token".to_string(),
                ctid_trader_account: accounts,
                ..Default::default()
            };
            vec![(pt::GET_ACCOUNTS_BY_ACCESS_TOKEN_RES, res.encode_to_vec())]
        }
        pt::ACCOUNT_AUTH_REQ => {
            let res = ProtoOaAccountAuthRes { ctid_trader_account_id: 99, ..Default::default() };
            vec![(pt::ACCOUNT_AUTH_RES, res.encode_to_vec())]
        }
        pt::TRADER_REQ => {
            let res = ProtoOaTraderRes {
                ctid_trader_account_id: 99,
                trader: ProtoOaTrader {
                    ctid_trader_account_id: 99,
                    money_digits: Some(FAKE_MONEY_DIGITS),
                    ..Default::default()
                },
                ..Default::default()
            };
            vec![(pt::TRADER_RES, res.encode_to_vec())]
        }
        pt::SYMBOLS_LIST_REQ => {
            let res = ProtoOaSymbolsListRes {
                ctid_trader_account_id: 99,
                symbol: vec![ProtoOaLightSymbol {
                    symbol_id: 1,
                    symbol_name: Some("EURUSD".to_string()),
                    enabled: Some(true),
                    ..Default::default()
                }],
                ..Default::default()
            };
            vec![(pt::SYMBOLS_LIST_RES, res.encode_to_vec())]
        }
        pt::SYMBOL_BY_ID_REQ => {
            let res = ProtoOaSymbolByIdRes {
                ctid_trader_account_id: 99,
                symbol: vec![ProtoOaSymbol {
                    symbol_id: 1,
                    digits: 5,
                    pip_position: 4,
                    ..Default::default()
                }],
                ..Default::default()
            };
            vec![(pt::SYMBOL_BY_ID_RES, res.encode_to_vec())]
        }
        pt::SUBSCRIBE_SPOTS_REQ => {
            let res = ProtoOaSubscribeSpotsRes { ctid_trader_account_id: 99, ..Default::default() };
            let sub_res = (pt::SUBSCRIBE_SPOTS_RES, res.encode_to_vec());
            let spot_event = ProtoOaSpotEvent {
                ctid_trader_account_id: 99,
                symbol_id: 1,
                bid: Some(113911),
                ask: Some(113912),
                timestamp: Some(SPOT_EVENT_TIMESTAMP_MS),
                ..Default::default()
            };
            vec![sub_res, (pt::SPOT_EVENT, spot_event.encode_to_vec())]
        }
        pt::NEW_ORDER_REQ => {
            // Decode the incoming order so the scripted execution events echo the client's real
            // clientOrderId/symbol/volume (mirrors cTrader: a NewOrder acks with ORDER_ACCEPTED
            // then, for a market order, ORDER_FILLED). Malformed → no reply.
            match ProtoOaNewOrderReq::decode(msg.payload.as_deref().unwrap_or(&[])) {
                Ok(order) => new_order_exec_events(&order),
                Err(_) => Vec::new(),
            }
        }
        pt::GET_TRENDBARS_REQ => {
            // Echo the request's symbolId/period back (F2 historical seed, `tests/data_bars.rs`).
            // Malformed → no reply (matches every other request's malformed-decode handling here).
            match ProtoOaGetTrendbarsReq::decode(msg.payload.as_deref().unwrap_or(&[])) {
                Ok(req) => vec![(pt::GET_TRENDBARS_RES, get_trendbars_res(&req).encode_to_vec())],
                Err(_) => Vec::new(),
            }
        }
        pt::RECONCILE_REQ => {
            // F3: answer the reconnect reconcile with ONE pending LIMIT order carrying the known
            // coid (in both the dedicated `clientOrderId` field and the `label`) + venue orderId,
            // so the client can rebuild its coid→orderId map from it.
            let order = ProtoOaOrder {
                order_id: RECONCILE_PENDING_ORDER_ID,
                trade_data: ProtoOaTradeData {
                    symbol_id: 1,
                    volume: 100,
                    trade_side: ProtoOaTradeSide::Buy as i32,
                    label: Some(RECONCILE_PENDING_COID.to_string()),
                    ..Default::default()
                },
                order_type: ProtoOaOrderType::Limit as i32,
                order_status: 1, // pending/accepted — irrelevant to the coid→orderId rebuild
                client_order_id: Some(RECONCILE_PENDING_COID.to_string()),
                limit_price: Some(1.10),
                ..Default::default()
            };
            let res = ProtoOaReconcileRes {
                ctid_trader_account_id: 99,
                order: vec![order],
                position: vec![],
                ..Default::default()
            };
            let mut frames = Vec::new();
            if reconcile_inflight_fill {
                // F3 fix 1: an EXECUTION_EVENT (a full fill) that lands DURING the reconcile
                // window, BEFORE the RECONCILE_RES — the actor must buffer + replay it, not drop
                // it. Sent first so it is read while `read_reconcile_res` is still waiting.
                frames.push((pt::EXECUTION_EVENT, inflight_fill_event().encode_to_vec()));
            }
            frames.push((pt::RECONCILE_RES, res.encode_to_vec()));
            frames
        }
        _ => Vec::new(), // heartbeats etc. — record but don't reply
    }
}

/// The two scripted trendbars `GET_TRENDBARS_REQ` gets back, returned in REVERSE-chronological
/// order (newer bar first) so `tests/data_bars.rs` can prove `conn::on_get_trendbars_res` sorts
/// ascending before calling `seed_bars` — cTrader's actual wire order is not documented/
/// guaranteed. Descaled (÷[`event_mapper::RELATIVE_PRICE_SCALE`], i.e. 1e5) OHLC:
/// - older (`utc_timestamp_in_minutes=1000`): low=1.139, open=1.13905, close=1.13912, high=1.1392,
///   volume=42.
/// - newer (`utc_timestamp_in_minutes=1005`): low=1.1395, open=1.13953, close=1.13975,
///   high=1.1398, volume=17.
pub const SEED_BAR_OLDER_MINUTES: u32 = 1000;
pub const SEED_BAR_NEWER_MINUTES: u32 = 1005;

fn get_trendbars_res(req: &ProtoOaGetTrendbarsReq) -> ProtoOaGetTrendbarsRes {
    let newer = ProtoOaTrendbar {
        volume: 17,
        period: Some(req.period),
        low: Some(113_950),
        delta_open: Some(3),
        delta_close: Some(25),
        delta_high: Some(30),
        utc_timestamp_in_minutes: Some(SEED_BAR_NEWER_MINUTES),
    };
    let older = ProtoOaTrendbar {
        volume: 42,
        period: Some(req.period),
        low: Some(113_900),
        delta_open: Some(5),
        delta_close: Some(12),
        delta_high: Some(20),
        utc_timestamp_in_minutes: Some(SEED_BAR_OLDER_MINUTES),
    };
    ProtoOaGetTrendbarsRes {
        ctid_trader_account_id: req.ctid_trader_account_id,
        period: req.period,
        trendbar: vec![newer, older], // deliberately newest-first
        symbol_id: Some(req.symbol_id),
        ..Default::default()
    }
}

/// The venue order id the fake assigns to a placed order (echoed in both execution events so the
/// client's coid→orderId map is populated and `venue_order_id` flows through).
pub const FAKE_ORDER_ID: i64 = 555_001;
/// The fill price the fake reports on the scripted `ORDER_FILLED` (absolute double — cTrader order
/// prices/deal prices are NOT 1e5-scaled, unlike the spot feed).
pub const FAKE_FILL_PRICE: f64 = 1.13950;
/// The `ProtoOATrader.money_digits` the fake's `TRADER_RES` reports — the LIVE-VERIFIED value
/// (2026-07-14 real demo account) used everywhere in this crate's tests that descale a fill's
/// commission.
pub const FAKE_MONEY_DIGITS: u32 = 2;

/// Build the scripted `ORDER_ACCEPTED` then `ORDER_FILLED` execution events for a placed order,
/// echoing its `clientOrderId`/`symbolId`/`volume`. The FILLED event carries a `deal` with the
/// full filled volume at [`FAKE_FILL_PRICE`], matching a market order that fills immediately.
fn new_order_exec_events(order: &ProtoOaNewOrderReq) -> Vec<(u32, Vec<u8>)> {
    let coid = order.client_order_id.clone().or_else(|| order.label.clone());
    let order_ref = ProtoOaOrder {
        order_id: FAKE_ORDER_ID,
        trade_data: ProtoOaTradeData {
            symbol_id: order.symbol_id,
            volume: order.volume,
            trade_side: order.trade_side,
            label: order.label.clone(),
            ..Default::default()
        },
        order_type: order.order_type,
        order_status: 1, // ACCEPTED
        client_order_id: coid.clone(),
        ..Default::default()
    };
    let accepted = ProtoOaExecutionEvent {
        ctid_trader_account_id: 99,
        execution_type: ProtoOaExecutionType::OrderAccepted as i32,
        order: Some(order_ref.clone()),
        ..Default::default()
    };
    let mut filled_order = order_ref;
    filled_order.order_status = 2; // FILLED
    let filled = ProtoOaExecutionEvent {
        ctid_trader_account_id: 99,
        execution_type: ProtoOaExecutionType::OrderFilled as i32,
        order: Some(filled_order),
        deal: Some(ProtoOaDeal {
            deal_id: 900_001,
            order_id: FAKE_ORDER_ID,
            volume: order.volume,
            filled_volume: order.volume,
            symbol_id: order.symbol_id,
            execution_timestamp: 1_784_017_625_777,
            execution_price: Some(FAKE_FILL_PRICE),
            trade_side: order.trade_side,
            ..Default::default()
        }),
        ..Default::default()
    };
    vec![
        (pt::EXECUTION_EVENT, accepted.encode_to_vec()),
        (pt::EXECUTION_EVENT, filled.encode_to_vec()),
    ]
}

// ============================ close-by-position-id scripting =====================================
//
// `serve_close` models a HEDGING account statefully: every open creates a distinct position; a
// `ProtoOAClosePositionReq` reduces the named one. Used by `tests/exec_close.rs`.

/// Base `positionId` the close server assigns to the FIRST opened position; each later open
/// increments it — so the close test can predict the ids it must see on its close legs.
pub const CLOSE_POSITION_ID_BASE: i64 = 700_001;
/// Base `positionId` for the ERROR-status rows
/// [`FakeCtrader::start_close_scripted_with_unreadable_positions`] injects into every
/// `RECONCILE_RES`. Deliberately far from [`CLOSE_POSITION_ID_BASE`] so a stray id in a failure
/// message says which kind of row produced it.
pub const UNREADABLE_POSITION_ID_BASE: i64 = 900_001;
/// The `ctidTraderAccountId` the fake venue authenticates — every faithful `RECONCILE_RES` carries
/// it, and [`ReconcileFidelity::ForAnotherAccount`] carries [`OTHER_CTID`] instead.
pub const FAKE_CTID: i64 = 99;
/// A `ctidTraderAccountId` that is NOT this connection's — the wrong-account reconcile answer.
pub const OTHER_CTID: i64 = 12_345;

/// How a `serve_close` server's `RECONCILE_RES` relates to the book the venue ACTUALLY holds.
///
/// ⚠ **This axis exists because a server that always tells the truth cannot test a rule about
/// EVIDENCE.** `serve_close` used to answer every reconcile with its own real, complete,
/// fully-classifiable book, so every property that depends on an answer being WRONG — a hole in it,
/// a position missing from it, an answer about somebody else's account — was unrepresentable from a
/// test, and a position-VERIFIED halt is exactly a rule about how much an answer may be trusted. A
/// rig that cannot express the failure it is asked to rule out proves nothing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ReconcileFidelity {
    /// The answer reports every OPEN position the venue holds, all `PositionStatusOpen`, under this
    /// connection's own [`FAKE_CTID`]. The truthful server, and the default.
    #[default]
    Faithful,
    /// [`Self::Faithful`] plus `n` extra **ERROR-status** rows the bridge cannot classify — the
    /// honest shape of a HOLE (`ProtoOAPositionStatus.POSITION_STATUS_ERROR` is a real cTrader
    /// state this bridge genuinely cannot map to exposure), not a synthetic one.
    Unreadable(usize),
    /// ⚠ **The answer OMITS every position the venue actually holds.** Well-formed, fully
    /// classifiable, correct account — and EMPTY, from an account that is not flat. Nothing about
    /// this frame is decodably wrong, which is the entire point: a completeness check that counts
    /// undecodable ROWS cannot see a row that was never sent.
    OmitsEveryPosition,
    /// The answer belongs to [`OTHER_CTID`] — ANOTHER account's (flat) book arriving on this
    /// socket. Every row in it is well-formed and there are none, so a reader that never checks
    /// `ctidTraderAccountId` folds a STRANGER'S emptiness in and calls it authoritative for us.
    ForAnotherAccount,
}

impl ReconcileFidelity {
    /// How many unclassifiable ERROR rows ride along on each answer.
    fn unreadable(self) -> usize {
        match self {
            ReconcileFidelity::Unreadable(n) => n,
            _ => 0,
        }
    }

    /// Whether the venue's REAL open positions appear in the answer at all. `false` for both
    /// misrepresentations that matter here: the answer that silently drops them, and the answer
    /// that is about somebody else's (flat) account.
    fn reports_real_positions(self) -> bool {
        !matches!(
            self,
            ReconcileFidelity::OmitsEveryPosition | ReconcileFidelity::ForAnotherAccount
        )
    }

    /// The `ctidTraderAccountId` stamped on the answer.
    fn ctid(self) -> i64 {
        match self {
            ReconcileFidelity::ForAnotherAccount => OTHER_CTID,
            _ => FAKE_CTID,
        }
    }
}
/// The absolute fill price the close server reports on a CLOSING deal (distinct from the opening
/// [`FAKE_FILL_PRICE`] so a test could tell an open fill from a close fill if it needed to).
pub const CLOSE_FILL_PRICE: f64 = 1.14050;

/// One position the stateful [`serve_close`] server remembers between frames.
struct SrvPosition {
    symbol_id: i64,
    /// `ProtoOATradeSide` raw value (BUY/SELL) — the position's own direction.
    trade_side: i32,
    /// Remaining open volume in centi-units (reduced by each close).
    volume: i64,
    open_ts: i64,
}

/// How the close server answers each `CLOSE_POSITION_REQ` (by 1-based arrival index), so a test can
/// script the failure/concurrency edges the correlation fixes cover.
#[derive(Clone, Copy)]
pub enum CloseServePolicy {
    /// Every close FILLS (the happy path — `tests/exec_close.rs`'s open/close ladders).
    Fill,
    /// Every close is HELD (recorded, never answered) — leaves the coid in flight so a SECOND reduce
    /// on the same position hits the "already closing" exclusion deterministically.
    Hold,
    /// The FIRST close fills, the SECOND (and later) rejects via an `ORDER_REJECTED` execution event
    /// — the leg-reject-after-partial-progress case.
    FillThenReject,
    /// The FIRST close is refused with a protocol `ERROR_RES` (echoing the coid), later ones fill —
    /// the wire-level failure + tracker-forget case.
    ErrorResThenFill,
}

/// What to do for the `n`-th close (1-based).
enum CloseAction {
    Fill,
    Hold,
    RejectEvent,
    ErrorRes,
}

impl CloseServePolicy {
    fn action(self, n: usize) -> CloseAction {
        match self {
            CloseServePolicy::Fill => CloseAction::Fill,
            CloseServePolicy::Hold => CloseAction::Hold,
            CloseServePolicy::FillThenReject => {
                if n == 1 {
                    CloseAction::Fill
                } else {
                    CloseAction::RejectEvent
                }
            }
            CloseServePolicy::ErrorResThenFill => {
                if n == 1 {
                    CloseAction::ErrorRes
                } else {
                    CloseAction::Fill
                }
            }
        }
    }
}

/// An `ORDER_REJECTED` execution event for a closing order on `position_id` — cTrader rejecting a
/// `ProtoOAClosePositionReq` at the execution layer. Carries the position ref (via `order.positionId`)
/// so the bridge correlates it back to the reduce coid.
fn close_reject_event(position_id: i64, order_id: i64) -> (u32, Vec<u8>) {
    let order = ProtoOaOrder {
        order_id,
        trade_data: ProtoOaTradeData::default(),
        order_type: ProtoOaOrderType::Market as i32,
        order_status: 3, // REJECTED
        position_id: Some(position_id),
        closing_order: Some(true),
        ..Default::default()
    };
    let ev = ProtoOaExecutionEvent {
        ctid_trader_account_id: 99,
        execution_type: ProtoOaExecutionType::OrderRejected as i32,
        order: Some(order),
        error_code: Some("CLOSE_REJECTED".to_string()),
        ..Default::default()
    };
    (pt::EXECUTION_EVENT, ev.encode_to_vec())
}

/// A protocol `ERROR_RES` body for a refused `ProtoOAClosePositionReq` — the serve loop encodes it
/// with the request's `clientMsgId` (the coid) so `on_error_res` correlates it. The error code
/// deliberately contains neither "AUTH" nor "TOKEN", so the actor does NOT divert it to token
/// refresh (`is_auth_error`).
fn close_error_res_body() -> (u32, Vec<u8>) {
    let err = ProtoOaErrorRes {
        error_code: "POSITION_NOT_FOUND".to_string(),
        description: Some("close rejected by venue".to_string()),
        ..Default::default()
    };
    (pt::ERROR_RES, err.encode_to_vec())
}

/// Stateful exec serve loop for the close tests: the handshake is delegated to [`script_reply`],
/// then NEW_ORDER_REQ frames OPEN positions and CLOSE_POSITION_REQ frames are answered per `policy`.
#[allow(clippy::too_many_arguments)]
fn serve_close(
    stream: TcpStream,
    seen: Arc<Mutex<Vec<u32>>>,
    closes: Arc<Mutex<Vec<(i64, i64)>>>,
    open_ids: Arc<Mutex<HashSet<i64>>>,
    script: AccountsScript,
    policy: CloseServePolicy,
    preseed: Vec<(i64, i32, i64)>,
    fidelity: ReconcileFidelity,
) {
    stream.set_read_timeout(Some(Duration::from_secs(5))).ok();
    let mut writer = stream.try_clone().expect("clone fake stream");
    let (eof_stream, eof) = EofStream::new(stream);
    let mut reader = FrameReader::new(eof_stream);

    let mut positions: std::collections::HashMap<i64, SrvPosition> =
        std::collections::HashMap::new();
    // Positions that already exist before the client connects (the pre-existing-book flatten case).
    for (position_id, trade_side, volume) in &preseed {
        positions.insert(
            *position_id,
            SrvPosition { symbol_id: 1, trade_side: *trade_side, volume: *volume, open_ts: 0 },
        );
    }
    let mut next_position_id = CLOSE_POSITION_ID_BASE + preseed.len() as i64;
    let mut next_order_id = FAKE_ORDER_ID;
    let mut next_deal_id = 900_001_i64;
    let mut next_open_ts = 1_784_000_000_000_i64;
    let mut close_count = 0usize;

    loop {
        match reader.next_frame() {
            Ok(Some(msg)) => {
                let payload_type = msg.payload_type;
                seen.lock().expect("seen lock").push(payload_type);
                // Close responses echo the request's `clientMsgId` (the coid) so an ERROR_RES
                // correlates back to the reduce; everything else uses the generic "srv" id.
                let mut reply_msg_id = "srv".to_string();
                let frames: Vec<(u32, Vec<u8>)> = match payload_type {
                    pt::NEW_ORDER_REQ => {
                        match ProtoOaNewOrderReq::decode(msg.payload.as_deref().unwrap_or(&[])) {
                            Ok(order) => {
                                let position_id = next_position_id;
                                next_position_id += 1;
                                let order_id = next_order_id;
                                next_order_id += 1;
                                let deal_id = next_deal_id;
                                next_deal_id += 1;
                                let open_ts = next_open_ts;
                                next_open_ts += 1000;
                                positions.insert(
                                    position_id,
                                    SrvPosition {
                                        symbol_id: order.symbol_id,
                                        trade_side: order.trade_side,
                                        volume: order.volume,
                                        open_ts,
                                    },
                                );
                                open_position_events(
                                    &order,
                                    position_id,
                                    order_id,
                                    deal_id,
                                    open_ts,
                                )
                            }
                            Err(_) => Vec::new(),
                        }
                    }
                    pt::CLOSE_POSITION_REQ => {
                        match ProtoOaClosePositionReq::decode(msg.payload.as_deref().unwrap_or(&[]))
                        {
                            Ok(req) => {
                                closes
                                    .lock()
                                    .expect("closes lock")
                                    .push((req.position_id, req.volume));
                                close_count += 1;
                                reply_msg_id = msg.client_msg_id.clone().unwrap_or_default();
                                let order_id = next_order_id;
                                next_order_id += 1;
                                let deal_id = next_deal_id;
                                next_deal_id += 1;
                                match policy.action(close_count) {
                                    CloseAction::Fill => {
                                        match positions.get_mut(&req.position_id) {
                                            Some(pos) => {
                                                let frames = close_position_events(
                                                    &req, pos, order_id, deal_id,
                                                );
                                                // Once fully flattened, drop it from the OPEN set so a
                                                // preseeded-book test can watch the venue reach zero.
                                                if pos.volume == 0 {
                                                    open_ids
                                                        .lock()
                                                        .expect("open_ids lock")
                                                        .remove(&req.position_id);
                                                }
                                                frames
                                            }
                                            None => Vec::new(),
                                        }
                                    }
                                    CloseAction::Hold => Vec::new(),
                                    CloseAction::RejectEvent => {
                                        vec![close_reject_event(req.position_id, order_id)]
                                    }
                                    CloseAction::ErrorRes => vec![close_error_res_body()],
                                }
                            }
                            Err(_) => Vec::new(),
                        }
                    }
                    // The STATEFUL reconcile: unlike `script_reply`'s fixed answer (one pending
                    // order, zero positions), this server knows its own book, so it reports the
                    // positions that are actually open — including any PRE-SEEDED ones that
                    // existed before the client connected. That is what makes the connect-time
                    // position seed observable end to end: without it, a fresh mount is blind to
                    // exactly the book `close_all` had to be invented for.
                    pt::RECONCILE_REQ => {
                        // ⚠ The venue's REAL book is `positions`; what it SAYS is `fidelity`'s
                        // business. `OmitsEveryPosition` sends a well-formed, fully-classifiable,
                        // EMPTY list from an account that is not flat — the one misrepresentation
                        // no row-counting completeness check can ever see.
                        let mut open: Vec<ProtoOaPosition> = if fidelity.reports_real_positions() {
                            positions
                                .iter()
                                .filter(|(_, p)| p.volume > 0)
                                .map(|(id, p)| ProtoOaPosition {
                                    position_id: *id,
                                    trade_data: ProtoOaTradeData {
                                        symbol_id: p.symbol_id,
                                        volume: p.volume,
                                        trade_side: p.trade_side,
                                        open_timestamp: Some(p.open_ts),
                                        ..Default::default()
                                    },
                                    position_status: ProtoOaPositionStatus::PositionStatusOpen
                                        as i32,
                                    swap: 0,
                                    ..Default::default()
                                })
                                .collect()
                        } else {
                            Vec::new()
                        };
                        // …and the rows the bridge CANNOT classify, when a test asked for them. A
                        // server that only ever emits OPEN makes "the answer had a hole in it"
                        // unrepresentable, and that is the one state a position-verified halt must
                        // not read as a flat account.
                        for i in 0..fidelity.unreadable() {
                            open.push(ProtoOaPosition {
                                position_id: UNREADABLE_POSITION_ID_BASE + i as i64,
                                trade_data: ProtoOaTradeData {
                                    symbol_id: 1,
                                    volume: 100_000,
                                    trade_side: ProtoOaTradeSide::Buy as i32,
                                    open_timestamp: Some(1),
                                    ..Default::default()
                                },
                                position_status: ProtoOaPositionStatus::PositionStatusError as i32,
                                swap: 0,
                                ..Default::default()
                            });
                        }
                        let res = ProtoOaReconcileRes {
                            ctid_trader_account_id: fidelity.ctid(),
                            order: vec![],
                            position: open,
                            ..Default::default()
                        };
                        vec![(pt::RECONCILE_RES, res.encode_to_vec())]
                    }
                    // Handshake / everything else → the shared happy-path scripting.
                    _ => script_reply(&msg, script, false),
                };
                for (reply_type, body) in frames {
                    let frame = framing::encode(reply_type, &body, &reply_msg_id);
                    if writer.write_all(&frame).is_err() {
                        return;
                    }
                    let _ = writer.flush();
                }
            }
            Ok(None) => {
                if eof.load(Ordering::SeqCst) {
                    break;
                }
            }
            Err(_) => break,
        }
    }
}

/// The `ORDER_ACCEPTED` then `ORDER_FILLED` a fresh OPEN produces, the FILLED carrying both the
/// `deal` and the new OPEN `position` ref (so the bridge tracks its `positionId` for later reduce
/// routing). The position materializes at fill time, so only the FILLED event carries the ref.
fn open_position_events(
    order: &ProtoOaNewOrderReq,
    position_id: i64,
    order_id: i64,
    deal_id: i64,
    open_ts: i64,
) -> Vec<(u32, Vec<u8>)> {
    let coid = order.client_order_id.clone().or_else(|| order.label.clone());
    let trade_data = ProtoOaTradeData {
        symbol_id: order.symbol_id,
        volume: order.volume,
        trade_side: order.trade_side,
        label: order.label.clone(),
        open_timestamp: Some(open_ts),
        ..Default::default()
    };
    let accepted_order = ProtoOaOrder {
        order_id,
        trade_data: trade_data.clone(),
        order_type: order.order_type,
        order_status: 1, // ACCEPTED
        client_order_id: coid.clone(),
        position_id: Some(position_id),
        ..Default::default()
    };
    let accepted = ProtoOaExecutionEvent {
        ctid_trader_account_id: 99,
        execution_type: ProtoOaExecutionType::OrderAccepted as i32,
        order: Some(accepted_order.clone()),
        ..Default::default()
    };
    let mut filled_order = accepted_order;
    filled_order.order_status = 2; // FILLED
    let position = ProtoOaPosition {
        position_id,
        trade_data,
        position_status: ProtoOaPositionStatus::PositionStatusOpen as i32,
        swap: 0,
        ..Default::default()
    };
    let filled = ProtoOaExecutionEvent {
        ctid_trader_account_id: 99,
        execution_type: ProtoOaExecutionType::OrderFilled as i32,
        order: Some(filled_order),
        deal: Some(ProtoOaDeal {
            deal_id,
            order_id,
            position_id,
            volume: order.volume,
            filled_volume: order.volume,
            symbol_id: order.symbol_id,
            execution_timestamp: 1_784_017_625_777,
            execution_price: Some(FAKE_FILL_PRICE),
            trade_side: order.trade_side,
            ..Default::default()
        }),
        position: Some(position),
        ..Default::default()
    };
    vec![
        (pt::EXECUTION_EVENT, accepted.encode_to_vec()),
        (pt::EXECUTION_EVENT, filled.encode_to_vec()),
    ]
}

/// The single `ORDER_FILLED` a close produces: a closing deal that reduces `pos` by the requested
/// volume (`tradeSide` OPPOSITE the position's own, so it nets toward flat), plus the updated
/// `position` ref (OPEN with the reduced volume, or CLOSED when fully flattened). Mutates `pos`'s
/// remaining volume in place so a second close of the same position sees the reduced size.
fn close_position_events(
    req: &ProtoOaClosePositionReq,
    pos: &mut SrvPosition,
    order_id: i64,
    deal_id: i64,
) -> Vec<(u32, Vec<u8>)> {
    let close_volume = req.volume.min(pos.volume);
    pos.volume = (pos.volume - close_volume).max(0);
    let closed = pos.volume == 0;
    let close_side = if pos.trade_side == ProtoOaTradeSide::Buy as i32 {
        ProtoOaTradeSide::Sell as i32
    } else {
        ProtoOaTradeSide::Buy as i32
    };
    let position = ProtoOaPosition {
        position_id: req.position_id,
        trade_data: ProtoOaTradeData {
            symbol_id: pos.symbol_id,
            volume: pos.volume,
            trade_side: pos.trade_side,
            open_timestamp: Some(pos.open_ts),
            ..Default::default()
        },
        position_status: if closed {
            ProtoOaPositionStatus::PositionStatusClosed as i32
        } else {
            ProtoOaPositionStatus::PositionStatusOpen as i32
        },
        swap: 0,
        ..Default::default()
    };
    let closing_order = ProtoOaOrder {
        order_id,
        trade_data: ProtoOaTradeData {
            symbol_id: pos.symbol_id,
            volume: close_volume,
            trade_side: close_side,
            ..Default::default()
        },
        order_type: ProtoOaOrderType::Market as i32,
        order_status: 2, // FILLED
        position_id: Some(req.position_id),
        closing_order: Some(true),
        ..Default::default()
    };
    let filled = ProtoOaExecutionEvent {
        ctid_trader_account_id: 99,
        execution_type: ProtoOaExecutionType::OrderFilled as i32,
        order: Some(closing_order),
        deal: Some(ProtoOaDeal {
            deal_id,
            order_id,
            position_id: req.position_id,
            volume: close_volume,
            filled_volume: close_volume,
            symbol_id: pos.symbol_id,
            execution_timestamp: 1_784_017_626_500,
            execution_price: Some(CLOSE_FILL_PRICE),
            trade_side: close_side,
            ..Default::default()
        }),
        position: Some(position),
        ..Default::default()
    };
    vec![(pt::EXECUTION_EVENT, filled.encode_to_vec())]
}

/// The coid the in-flight `ORDER_FILLED` (buffered during the reconcile window) fills — the F3
/// fix-1 test asserts an `Event::OrderFilled` for this coid reaches the ingest lane.
pub const INFLIGHT_FILL_COID: &str = "inflight-fill-coid-1";
/// The venue `orderId`/`dealId` on that in-flight fill.
pub const INFLIGHT_FILL_ORDER_ID: i64 = 888_002;
pub const INFLIGHT_FILL_DEAL_ID: i64 = 900_888;
/// The absolute fill price on that in-flight fill.
pub const INFLIGHT_FILL_PRICE: f64 = 1.14200;

/// Build the in-flight `ORDER_FILLED` execution event (EURUSD id=1) the reconnect fake sends
/// BEFORE its `RECONCILE_RES`, to prove the reconcile path buffers + replays it rather than
/// dropping it. A full fill (`ORDER_FILLED`) of the whole volume at [`INFLIGHT_FILL_PRICE`].
fn inflight_fill_event() -> ProtoOaExecutionEvent {
    let order = ProtoOaOrder {
        order_id: INFLIGHT_FILL_ORDER_ID,
        trade_data: ProtoOaTradeData {
            symbol_id: 1,
            volume: 100,
            trade_side: ProtoOaTradeSide::Buy as i32,
            label: Some(INFLIGHT_FILL_COID.to_string()),
            ..Default::default()
        },
        order_type: ProtoOaOrderType::Market as i32,
        order_status: 2, // FILLED
        client_order_id: Some(INFLIGHT_FILL_COID.to_string()),
        ..Default::default()
    };
    ProtoOaExecutionEvent {
        ctid_trader_account_id: 99,
        execution_type: ProtoOaExecutionType::OrderFilled as i32,
        order: Some(order),
        deal: Some(ProtoOaDeal {
            deal_id: INFLIGHT_FILL_DEAL_ID,
            order_id: INFLIGHT_FILL_ORDER_ID,
            volume: 100,
            filled_volume: 100,
            symbol_id: 1,
            execution_timestamp: 1_784_017_626_000,
            execution_price: Some(INFLIGHT_FILL_PRICE),
            trade_side: ProtoOaTradeSide::Buy as i32,
            ..Default::default()
        }),
        ..Default::default()
    }
}

/// Human-readable payload-type name for `assert_saw` diagnostics.
fn pt_name(payload_type: u32) -> String {
    let name = match payload_type {
        pt::HEARTBEAT_EVENT => "HEARTBEAT_EVENT",
        pt::APPLICATION_AUTH_REQ => "APPLICATION_AUTH_REQ",
        pt::APPLICATION_AUTH_RES => "APPLICATION_AUTH_RES",
        pt::ACCOUNT_AUTH_REQ => "ACCOUNT_AUTH_REQ",
        pt::ACCOUNT_AUTH_RES => "ACCOUNT_AUTH_RES",
        pt::TRADER_REQ => "TRADER_REQ",
        pt::TRADER_RES => "TRADER_RES",
        pt::SYMBOLS_LIST_REQ => "SYMBOLS_LIST_REQ",
        pt::SYMBOLS_LIST_RES => "SYMBOLS_LIST_RES",
        pt::SYMBOL_BY_ID_REQ => "SYMBOL_BY_ID_REQ",
        pt::SYMBOL_BY_ID_RES => "SYMBOL_BY_ID_RES",
        pt::GET_ACCOUNTS_BY_ACCESS_TOKEN_REQ => "GET_ACCOUNTS_BY_ACCESS_TOKEN_REQ",
        pt::GET_ACCOUNTS_BY_ACCESS_TOKEN_RES => "GET_ACCOUNTS_BY_ACCESS_TOKEN_RES",
        pt::SUBSCRIBE_SPOTS_REQ => "SUBSCRIBE_SPOTS_REQ",
        pt::SUBSCRIBE_SPOTS_RES => "SUBSCRIBE_SPOTS_RES",
        pt::UNSUBSCRIBE_SPOTS_REQ => "UNSUBSCRIBE_SPOTS_REQ",
        pt::SPOT_EVENT => "SPOT_EVENT",
        pt::SUBSCRIBE_LIVE_TRENDBAR_REQ => "SUBSCRIBE_LIVE_TRENDBAR_REQ",
        pt::UNSUBSCRIBE_LIVE_TRENDBAR_REQ => "UNSUBSCRIBE_LIVE_TRENDBAR_REQ",
        pt::GET_TRENDBARS_REQ => "GET_TRENDBARS_REQ",
        pt::GET_TRENDBARS_RES => "GET_TRENDBARS_RES",
        pt::NEW_ORDER_REQ => "NEW_ORDER_REQ",
        pt::CANCEL_ORDER_REQ => "CANCEL_ORDER_REQ",
        pt::AMEND_ORDER_REQ => "AMEND_ORDER_REQ",
        pt::CLOSE_POSITION_REQ => "CLOSE_POSITION_REQ",
        pt::RECONCILE_REQ => "RECONCILE_REQ",
        pt::RECONCILE_RES => "RECONCILE_RES",
        pt::EXECUTION_EVENT => "EXECUTION_EVENT",
        other => return format!("PT_{other}"),
    };
    name.to_string()
}

// ── The operator HALT sentinel, for the tests that must NOT inherit one ──────────────────────────

/// A sentinel path THIS test process names and never creates.
///
/// ⚠ **`CtraderExec`'s default is ARMED at the PROCESS-WIDE sentinel** (`halt_engaged`'s `None`
/// arm → `vike_bridge_core::halt::halt_path_from_env`: `VIKE_HALT_FILE`, else
/// `<project>/settings/state/HALT`, else `<exe_dir>/HALT`). That default is right for a mount — this
/// client only ever exists behind a live authenticated socket — and it makes every test that submits
/// an OPENING order depend on whether that file happens to exist on the box running it. MEASURED on
/// the CI box: with `VIKE_HALT_FILE` pointing at a real file, 13 tests in this crate went red
/// (`exec`, `exec_close`, `exec_reject`, `client_mount`); without it, green. CI is green only
/// because no runner happens to have the file. The trigger is EXACTLY the file an operator touches
/// on a live node.
///
/// Pinning, not `set_var`, is the cure: `halt_path_from_env` memoizes in a `OnceLock`, and this
/// workspace does not mutate the environment under threads. The path is unique per (process, call)
/// so parallel tests cannot collide, and the directory is deliberately NOT created — a test that
/// wants to ENGAGE a halt makes its own file, as `tests/exec_halt.rs` does.
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

/// [`CtraderExec::new`] pinned to [`unengaged_halt_sentinel`] — the constructor every NON-live test
/// in this crate that submits an opening order must use, so its verdict cannot depend on an
/// operator's kill switch lying around on the box.
pub fn exec_with_no_halt(handle: ActorHandle, events: EventSender, tag: &str) -> CtraderExec {
    CtraderExec::new(handle, events).with_halt_path(unengaged_halt_sentinel(tag))
}

/// The substring every halt-indifference test's NAME must contain, and the `--skip` filter the
/// child run below is given. Spelled once so the two cannot disagree — a mismatch would make the
/// child re-run the indifference test, which re-runs the binary, forever.
pub const HALT_INDIFFERENCE: &str = "indifferent_to_an_engaged_halt_sentinel";

/// Re-run THIS test binary with an operator HALT sentinel ENGAGED and require the SAME verdict.
///
/// ⚠ **This is the equality the pinning above exists to produce, expressed as a test so it cannot
/// silently regress.** Pinning each construction site is correct but is per-call-site discipline:
/// one future test that reaches for `CtraderExec::new` directly re-inherits the operator's kill
/// switch, passes on every clean box, and fails only where the file happens to exist. This asks the
/// question directly — *does anything in this binary change verdict when the sentinel is on?* —
/// and it asks it on EVERY box, because the sentinel it engages is one this test writes rather than
/// one the machine happened to have. That is the same lesson
/// `crates/vike-paper/tests/paper_halt_process_wide.rs` records: a proof that holds only where a
/// file already exists is not a proof.
///
/// Mechanism, deliberately with no new API and no `set_var`: libtest binaries accept their own
/// arguments, so `current_exe()` plus a child process is the whole of it. The child gets
/// `VIKE_HALT_FILE` — the FIRST rung of `crates/vike-bridge-core/src/halt.rs`'s `resolve_halt_path`,
/// so it wins over whatever else the box has — pointed at a file this function creates, and
/// `--skip HALT_INDIFFERENCE` so it does not re-enter here. `--test-threads=1` so a failure is the
/// sentinel's doing and not scheduling. `set_var` is unavailable for the usual reason: cargo runs a
/// binary's tests as threads in ONE process and `halt_path_from_env` memoizes in a `OnceLock`.
///
/// ⚠ The sentinel's directory is a BOUND `tempfile::TempDir`, and both halves of that matter. It
/// used to be `env::temp_dir().join(format!("vike-ctrader-halt-indifference-{pid}"))` plus a
/// `create_dir_all`, which leaked one directory per run — 44,840 of them under `/tmp` on the shared
/// the CI box box (measured 2026-08-25) from this idiom across the workspace — and, because a PID is
/// REUSED while the CI box runs tests as TWO users (`the CI user` for CI, `the operator` for the verification
/// lanes), hit the other user's leftover directory often enough to matter: `create_dir_all`
/// SUCCEEDS on a directory that already exists, and the `write` that engages the sentinel then
/// fails `PermissionDenied` — reddening this test for a reason that has nothing to do with halts.
/// The guard drops only after `output()` has returned, i.e. after the child has exited, so nothing
/// is removed while the child is still reading the file — and it is removed on the FAILING path
/// too, which the hand-rolled cleanup that used to sit above the assert never was.
pub fn assert_indifferent_to_an_engaged_halt_sentinel() {
    // We ARE the child. `--skip` should already have excluded this test; this is the belt to that
    // braces, because unbounded recursion is a far worse failure mode than a red assert.
    if std::env::args().any(|a| a == HALT_INDIFFERENCE) {
        return;
    }
    let dir = tempfile::Builder::new()
        .prefix("vike-ctrader-halt-indifference-")
        .tempdir()
        .expect("temp sentinel dir");
    let sentinel = dir.path().join("HALT");
    std::fs::write(&sentinel, b"").expect("engage the sentinel");
    assert!(
        sentinel.exists(),
        "the sentinel must be ON DISK before the child runs, or this test passes vacuously — the \
         exact defect it exists to prevent"
    );

    let exe = std::env::current_exe().expect("this test binary's own path");
    let out = std::process::Command::new(&exe)
        .env("VIKE_HALT_FILE", &sentinel)
        .args(["--test-threads=1", "--skip", HALT_INDIFFERENCE])
        .output()
        .expect("re-run this test binary");

    assert!(
        out.status.success(),
        "this binary's verdict CHANGED when an operator HALT sentinel was engaged. Some test here \
         constructs a `CtraderExec` without pinning its sentinel, so it inherits the operator kill \
         switch off the box — pass `common::exec_with_no_halt` (or `.with_halt_path(\
         common::unengaged_halt_sentinel(..))`) instead.\n--- child stdout ---\n{}\n--- child \
         stderr ---\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
}
