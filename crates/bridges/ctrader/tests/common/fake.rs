//! The `FakeCtrader` handle: one constructor per scripted venue behaviour, and its serve entry points.

use std::collections::HashSet;
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use vike_ctrader::proto::{ProtoOaTradeSide, pt};

use super::close_script::{
    CLOSE_POSITION_ID_BASE, CloseServePolicy, ReconcileFidelity, serve_close,
};
use super::serve::{AcceptedToken, RejectMode, pt_name, serve_inner};

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
    /// `a_mute_venue_costs_the_start_the_seed_bound_not_the_reconnect_one` to hold
    /// `conn::SEED_TIMEOUT` to the synchronous start path.
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

    /// Bind an ephemeral port and serve the scripted handshake, except that the three broker
    /// reference-list requests (symbol categories, asset classes, assets) each get an `ERROR_RES`.
    /// Used by `tests/handshake.rs` to prove a broker that declines them still completes the
    /// handshake, its symbols uncategorised.
    pub fn start_reject_reference_lists() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind fake ctrader");
        let addr = listener.local_addr().expect("local_addr");
        let seen = Arc::new(Mutex::new(Vec::new()));
        let seen_thread = seen.clone();
        thread::Builder::new()
            .name("fake-ctrader-reject-ref".into())
            .spawn(move || {
                if let Ok((stream, _peer)) = listener.accept() {
                    serve_inner(
                        stream,
                        seen_thread,
                        AccountsScript::OneDemo,
                        None,
                        RejectMode::ReferenceLists,
                        false,
                        false,
                        None,
                    );
                }
            })
            .expect("spawn fake ctrader-reject-ref");
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
                            None,
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

    /// Bind an ephemeral port and serve the full scripted handshake on EVERY connection (one serve
    /// thread each), except that `ACCOUNT_AUTH_REQ` is admitted only when it carries the access
    /// token the returned [`AcceptedToken`] currently names — any other gets the superseded-token
    /// `ERROR_RES` a venue sends once a grant was refreshed. The handle switches the admitted token
    /// while the server runs and lists every token presented. Used by `tests/shared_grant.rs` to
    /// model "a sibling socket rotated the grant" without a network or an OAuth endpoint.
    pub fn start_gated_on_access_token(initial: &str) -> (Self, AcceptedToken) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind fake ctrader");
        let addr = listener.local_addr().expect("local_addr");
        let seen = Arc::new(Mutex::new(Vec::new()));
        let seen_thread = seen.clone();
        let gate = AcceptedToken::new(initial);
        let gate_thread = gate.clone();
        thread::Builder::new()
            .name("fake-ctrader-gated".into())
            .spawn(move || {
                while let Ok((stream, _peer)) = listener.accept() {
                    let seen_conn = seen_thread.clone();
                    let gate_conn = gate_thread.clone();
                    thread::spawn(move || {
                        serve_inner(
                            stream,
                            seen_conn,
                            AccountsScript::OneDemo,
                            None,
                            RejectMode::None,
                            false,
                            false,
                            Some(gate_conn),
                        );
                    });
                }
            })
            .expect("spawn fake ctrader-gated");
        let fake = FakeCtrader {
            addr,
            seen,
            closes: Arc::new(Mutex::new(Vec::new())),
            open_ids: Arc::new(Mutex::new(HashSet::new())),
        };
        (fake, gate)
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

/// Read frames, record their payload type, and write the scripted reply. Loops until the client
/// disconnects (EOF) or errors — the socket stays open after the handshake so the client's actor
/// loop hits read-timeouts (not EOF) during the test window.
fn serve(stream: TcpStream, seen: Arc<Mutex<Vec<u32>>>, script: AccountsScript) {
    serve_inner(stream, seen, script, None, RejectMode::None, false, false, None);
}

/// Like [`serve`], but answers `RECONCILE_REQ` with NOTHING AT ALL — see
/// [`FakeCtrader::start_mute_reconcile`].
fn serve_mute_reconcile(stream: TcpStream, seen: Arc<Mutex<Vec<u32>>>, script: AccountsScript) {
    serve_inner(stream, seen, script, None, RejectMode::None, false, true, None);
}

/// Like [`serve`], but replies to `NEW_ORDER_REQ` with an `ERROR_RES` echoing the request's
/// `clientMsgId` (a venue submit-reject) rather than the scripted accepted/filled events.
fn serve_reject(stream: TcpStream, seen: Arc<Mutex<Vec<u32>>>, script: AccountsScript) {
    serve_inner(stream, seen, script, None, RejectMode::Orders, false, false, None);
}

/// Like [`serve`], but replies to `GET_TRENDBARS_REQ` with an `ERROR_RES` echoing the request's
/// `clientMsgId` rather than the scripted `GET_TRENDBARS_RES` — see
/// [`FakeCtrader::start_reject_trendbars_scripted`].
fn serve_reject_trendbars(stream: TcpStream, seen: Arc<Mutex<Vec<u32>>>, script: AccountsScript) {
    serve_inner(stream, seen, script, None, RejectMode::Trendbars, false, false, None);
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
    serve_inner(stream, seen, script, Some(drop_after), RejectMode::None, false, false, None);
}

/// Like [`serve`], but answers `RECONCILE_REQ` with an in-flight `ORDER_FILLED` execution event
/// (for [`INFLIGHT_FILL_COID`]) sent BEFORE the `RECONCILE_RES` — see
/// [`FakeCtrader::start_reconnect_with_inflight_fill`].
fn serve_inflight_fill(stream: TcpStream, seen: Arc<Mutex<Vec<u32>>>, script: AccountsScript) {
    serve_inner(stream, seen, script, None, RejectMode::None, true, false, None);
}
