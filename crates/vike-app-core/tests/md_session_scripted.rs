//! `MdSession` driven end-to-end against an IN-PROCESS FAKE DATAHUB — a `TcpListener` on an
//! ephemeral loopback port speaking the REAL `vike_datahub_client::proto` (design §9 item 9).
//!
//! The pattern is `crates/vike-app-core/tests/backfill_wire_scripted.rs`'s, for its reason: proving
//! **"nothing was sent"** needs a server that can COUNT, and proving the request stream needs one
//! that can RECORD. Loopback + in-memory only — no store on disk, no venue, no GUI, no GPU.
//!
//! What is pinned here, in the order the design lists it, plus THREE the design does not list and
//! that this client must not ship without:
//!
//! 1. an UNADVERTISED server is refused CLIENT-side, with zero frames after the handshake, and the
//!    status line names the fix;
//! 2. each `MdRefusal` half lands where it belongs — permanent out of the desired set and
//!    synchronously `Unsupported` from then on, a cap left wanted and retried;
//! 3. a `TapeGap` discards the buffered tape AND bumps the epoch;
//! 4. a stale key produces a status and NO book;
//! 5. a book lands in `BookStore` with a LOCAL receipt, provably not the frame's `venue_ts`;
//! 6. `unsubscribe` returns with no I/O at all;
//! 7. ⚠ **THE KILL PROOF for the symbol re-stamp** — a `Trades` frame whose ticks carry the EMPTY
//!    symbol the hub strips must land in `TradeStore` under the ENVELOPE's symbol. The natural
//!    implementation (hand the tick to the sink unchanged) fails this and nothing else in the tree
//!    would ever notice;
//! 8. a mid-stream `Bye` ends the reader without wedging the session;
//! 9. ⚠ **THE KILL PROOF for the desired/served diff** — a server that CLAMPS the depth (which the
//!    real one always does: the client sends `None` and gets `Some(50)` back) must still converge,
//!    or the reconciler dials the server once per pass for ever. Every other test here uses a fake
//!    that echoes the request verbatim, which makes the two sets equal by any comparison and hides
//!    this entirely.

use std::collections::HashMap;
use std::io::ErrorKind;
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use vike_app_core::data_sink::{BookStore, TradeStore};
use vike_app_core::md_session::{self, MdMount};
use vike_data::{DataClient, LiveDataError};
use vike_datahub_client::{
    FEATURE_MARKET_DATA, MdBye, MdFrame, MdLane, MdRefusal, MdSessionId, MdSpec, PROTO_VERSION,
    Request, Response, WireStreamStatus, md_venue_feature, read_frame, write_frame,
};
use vike_model::TradeTick;

/// How long a test waits for a background thread to do a thing before calling it a failure. The
/// session's own schedule is `feed_lifecycle::retry_backoff`, whose FIRST step is one second, so
/// anything this bound covers is a first-attempt event; nothing here waits out a backoff.
const SETTLE: Duration = Duration::from_secs(5);

/// How long the fake waits between two PUSHED frames, so a test can observe the state BETWEEN them.
///
/// A quarter of [`SETTLE`]'s tolerance and fifty of `until`'s 5 ms poll intervals, which is the
/// margin: this box runs these tests beside everything else, and a gap sized to "usually enough"
/// is a flake with a schedule. It costs one interval per extra frame in the handful of tests that
/// push more than one.
const PUSH_GAP: Duration = Duration::from_millis(250);

/// Poll `f` until it answers `true`, or fail naming `what`. A spin rather than a sleep: every
/// property under test is produced by another thread, and a fixed sleep is either a flake or a
/// wasted second.
fn until(what: &str, f: impl Fn() -> bool) {
    let deadline = Instant::now() + SETTLE;
    while Instant::now() < deadline {
        if f() {
            return;
        }
        thread::sleep(Duration::from_millis(5));
    }
    panic!("timed out waiting for: {what}");
}

/// What the fake server does after answering `MdSubscribe`. Returns the frames to push, in order.
///
/// ⚠ `Sync` as well as `Send`: the fake accepts CONNECTIONS in a loop and hands each one its own
/// thread (a session's `MdUpdate` dials a second connection, so one script genuinely serves
/// several at once), and the `Arc<Script>` they share is only `Send` if what it wraps is `Sync`.
type Script = Box<dyn Fn(&[MdSpec]) -> ScriptedAnswer + Send + Sync>;

/// One scripted `MdSubscribed` answer plus the frames to push after it.
struct ScriptedAnswer {
    accepted: Vec<MdSpec>,
    refused: Vec<(MdSpec, MdRefusal)>,
    push: Vec<MdFrame>,
}

impl ScriptedAnswer {
    fn accept_all(specs: &[MdSpec], push: Vec<MdFrame>) -> ScriptedAnswer {
        ScriptedAnswer { accepted: specs.to_vec(), refused: Vec::new(), push }
    }
}

/// A handle onto a running fake datahub.
struct Fake {
    addr: SocketAddr,
    /// Every `Request` the server read, in arrival order, reported once the client disconnects.
    seen: mpsc::Receiver<Vec<String>>,
    /// How many connections were accepted — the "nothing was sent" counter's coarse twin.
    accepted: Arc<AtomicUsize>,
}

/// Spawn a fake datahub advertising `features`. It accepts connections for ever; each one gets the
/// handshake, then either the `MdSubscribe` mode switch (answered from `script`, after which it
/// PUSHES and stops reading) or an ordinary positional reply.
fn spawn_fake(features: Vec<String>, script: Script) -> Fake {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
    let addr = listener.local_addr().expect("resolve assigned port");
    let (tx, seen) = mpsc::channel();
    let accepted = Arc::new(AtomicUsize::new(0));
    let counter = accepted.clone();
    thread::spawn(move || {
        let script = Arc::new(script);
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            counter.fetch_add(1, Ordering::SeqCst);
            let features = features.clone();
            let script = script.clone();
            let tx = tx.clone();
            thread::spawn(move || {
                let mut kinds = Vec::new();
                match read_frame::<_, Request>(&mut stream) {
                    Ok(Request::Hello { .. }) => kinds.push("Hello".to_string()),
                    _ => return,
                }
                if write_frame(
                    &mut stream,
                    &Response::Welcome { proto_version: PROTO_VERSION, features, nonce: None },
                )
                .is_err()
                {
                    return;
                }
                // Set once the socket has MODE-SWITCHED: the server has left its read loop and this
                // connection is write-only for ever, exactly as `run_market_writer` does.
                let mut switched = false;
                while let Ok(request) = read_frame::<_, Request>(&mut stream) {
                    match request {
                        Request::MdSubscribe { specs } => {
                            kinds.push(format!("MdSubscribe({})", specs.len()));
                            let answer = script(&specs);
                            let _ = write_frame(
                                &mut stream,
                                &Response::MdSubscribed {
                                    session: MdSessionId(7),
                                    accepted: answer.accepted,
                                    refused: answer.refused,
                                    heartbeat_ms: 15_000,
                                },
                            );
                            for (i, frame) in answer.push.into_iter().enumerate() {
                                // ⚠ A GAP BETWEEN PUSHED FRAMES, and it is what makes an
                                // INTERMEDIATE state observable at all. Written back-to-back on
                                // loopback, a book frame and the `Bye` that drops it again are
                                // applied by the reader microseconds apart, so a test polling for
                                // "the book arrived, then it went" can only ever see the end state
                                // — `a_mid_stream_bye_drops_the_books_and_the_session_redials`
                                // failed on exactly that and had never passed. This is a property
                                // of the FAKE, not of the client: a real venue does not emit a
                                // snapshot and a disconnect in the same microsecond either.
                                if i > 0 {
                                    thread::sleep(PUSH_GAP);
                                }
                                if write_frame(&mut stream, &Response::Md(Box::new(frame))).is_err()
                                {
                                    break;
                                }
                            }
                            switched = true;
                            break;
                        }
                        Request::MdUpdate { add, remove, .. } => {
                            kinds.push(format!("MdUpdate(+{} -{})", add.len(), remove.len()));
                            // ⚠ THE SCRIPT ANSWERS HERE TOO, and a blind `accepted: add` was both a
                            // fidelity gap and a RACE. A script that refuses a key with a CAP means
                            // "this server has no room", which is a STATE, not a one-shot: the
                            // reconciler's very next pass re-sends the key as an `add`, and a fake
                            // that accepted it there flipped `served` from empty to full within a
                            // millisecond or two of the `MdSubscribed` that refused it. Any test
                            // asserting on the refused state was then reading a value that had
                            // already moved — the assertion passed or failed on scheduling.
                            let answer = script(&add);
                            let _ = write_frame(
                                &mut stream,
                                &Response::MdUpdated {
                                    accepted: answer.accepted,
                                    refused: answer.refused,
                                    released: remove,
                                },
                            );
                        }
                        other => {
                            kinds.push(format!("{other:?}"));
                            let _ = write_frame(
                                &mut stream,
                                &Response::Error("this fake serves market data only".to_string()),
                            );
                        }
                    }
                }
                let _ = tx.send(kinds);
                // Hold a switched socket OPEN and quiet. Closing it would look like a fault and put
                // the client into its reconnect ladder, which is a different test's subject.
                //
                // Parked on a READ rather than spinning on a sleep: the client writes nothing more
                // on this socket (that is the mode switch), so this blocks until the client shuts
                // its end down — which is exactly what `stop_and_join` does — and then the thread
                // ends. A `while switched { sleep }` spin was the first spelling and clippy is
                // right to refuse it: the condition it loops on can never change.
                if switched {
                    let _ = read_frame::<_, Request>(&mut stream);
                }
            });
        }
    });
    Fake { addr, seen, accepted }
}

/// Everything a test holds: the stores the session writes into, the mount it produced, and a wake
/// counter proving the producer actually woke the frame thread.
struct Harness {
    books: Arc<BookStore>,
    trades: Arc<TradeStore>,
    mount: MdMount,
    wakes: Arc<AtomicUsize>,
}

impl Harness {
    /// Build a session with NO credentials and NO node-key store.
    ///
    /// ⚠ `settings_dir` is an EMPTY `tempfile::tempdir()`, and neither `None` nor a fixed temp path
    /// would do. `None` is a different test: `datahub_observe_keys`' second rung calls
    /// `vike_secrets::resolve_node_keys`, whose `None` means *walk up from the working directory for
    /// a project marker* — so on a box that has one, this suite would open the developer's real
    /// `node.env` and its answer would depend on whose checkout it ran in. A FIXED name under the
    /// system temp directory is what `crates/vike-ops/tests/temp_path_gate.rs` refuses, and for a
    /// reason this box has already paid: CI and agents run as different users, whoever creates the
    /// directory first owns it, and every later run under the other user fails permanently. A
    /// `tempdir` is unique AND self-deleting; both files resolve NotFound inside it, which
    /// `vike_secrets::resolve` answers with an empty store, silently.
    fn new() -> Harness {
        let books = Arc::new(BookStore::default());
        let trades = Arc::new(TradeStore::default());
        let wakes = Arc::new(AtomicUsize::new(0));
        let w = wakes.clone();
        // ⚠ BOUND, not `.path()` off a temporary: dropping the `TempDir` deletes the directory, and
        // the resolution below happens inside `build`.
        let no_store = tempfile::tempdir().expect("a scratch settings directory");
        let mount = md_session::build(
            no_store.path().to_str(),
            &HashMap::new(),
            "VIKE_DATAHUB_OBSERVE_KEY",
            books.clone(),
            trades.clone(),
            move || {
                w.fetch_add(1, Ordering::SeqCst);
            },
        );
        Harness { books, trades, mount, wakes }
    }

    fn feed(&mut self, venue: &str) -> &mut Box<dyn DataClient + Send> {
        self.mount.feeds.get_mut(venue).expect("every LOCAL_FEED_VENUES slug has a feed")
    }

    fn status(&self, venue: &str) -> String {
        self.mount.feed_statuses[venue].lock().unwrap().clone()
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        // Every test tears the session down explicitly rather than leaking two threads per test
        // binary. `stop_and_join` is the same call `App::run_bounded_teardown` makes.
        self.mount.session.stop_and_join();
    }
}

fn book_frame(venue: &str, symbol: &str, venue_ts: i64) -> MdFrame {
    MdFrame::Depth(vike_datahub_client::BookSnapshot {
        venue: venue.to_string(),
        symbol: symbol.to_string(),
        tick_size: 0.5,
        bids: vec![(100.0, 1.0)],
        asks: vec![(100.5, 2.0)],
        venue_ts,
        venue_seq: 42,
        seq: 1,
    })
}

/// A tick as the HUB produces it: **the symbol is EMPTY**. `md/hub.rs`'s `push_trade` sets
/// `tick.symbol = String::new()` before the tape, and the envelope carries the symbol once.
fn stripped_tick(ts: i64, price: f64, size: f64) -> TradeTick {
    TradeTick { ts, local_ts: 0, price, size, is_buyer_maker: false, symbol: String::new() }
}

fn md_features() -> Vec<String> {
    let mut f = vec!["load_bars".to_string(), FEATURE_MARKET_DATA.to_string()];
    for v in ["binance", "bybit", "okx", "aster", "hyperliquid", "polymarket"] {
        f.push(md_venue_feature(v));
    }
    f
}

// ── 1. the local capability refusal ──────────────────────────────────────────────────────────────

/// **A server that does not advertise `market_data` is refused CLIENT-SIDE, and the refusal is
/// PERMANENT the way `FeedRetries` needs it to be.**
///
/// The first `subscribe_depth` is accepted OPTIMISTICALLY — nothing has handshaked yet, and a
/// premature `Unsupported` would be recorded as `RetryState::Refused` and never re-asked. Once the
/// reconciler has learned the advertisement, every later `subscribe_*` is refused synchronously with
/// no wire traffic at all, and the per-venue status line names the fix.
#[test]
fn a_server_with_no_market_data_plane_is_refused_locally_after_the_first_handshake() {
    // Advertises `load_bars` and nothing else — an older datahub, or one built without `live-feeds`.
    let fake = spawn_fake(
        vec!["load_bars".to_string()],
        Box::new(|specs| ScriptedAnswer::accept_all(specs, Vec::new())),
    );
    let mut h = Harness::new();
    assert!(
        h.feed("binance").subscribe_depth("BTCUSDT").is_ok(),
        "with no advertisement yet, the intent must be accepted rather than refused for ever"
    );
    h.mount.session.set_addr(Some(&fake.addr.to_string()));

    until("the status line to name the missing plane", || {
        h.status("binance").contains("live-feeds")
    });
    match h.feed("okx").subscribe_depth("BTC-USDT-SWAP") {
        Err(LiveDataError::Unsupported(msg)) => {
            assert!(msg.contains("live-feeds"), "the refusal must name the fix, got {msg:?}")
        }
        other => {
            panic!("expected a synchronous Unsupported once the caps were cached, got {other:?}")
        }
    }
    // ⚠ NOTHING was ever sent past the handshake: this server was never asked to subscribe.
    let seen = fake.seen.recv_timeout(SETTLE).expect("the fake reports what it read");
    assert_eq!(
        seen,
        vec!["Hello".to_string()],
        "a frame was sent to a server that cannot serve it"
    );
}

/// The venue half of the same rule: a server that serves market data but advertises no
/// `md_venue=polymarket` refuses that venue locally, and still serves the ones it advertises.
#[test]
fn a_venue_the_server_does_not_advertise_is_refused_locally() {
    let features =
        vec!["load_bars".to_string(), FEATURE_MARKET_DATA.to_string(), md_venue_feature("binance")];
    let fake =
        spawn_fake(features, Box::new(|specs| ScriptedAnswer::accept_all(specs, Vec::new())));
    let mut h = Harness::new();
    h.feed("binance").subscribe_depth("BTCUSDT").expect("optimistic before the handshake");
    h.mount.session.set_addr(Some(&fake.addr.to_string()));

    until("the advertisement to be cached", || h.status("binance").contains("live"));
    match h.feed("polymarket").subscribe_book("0xtoken") {
        Err(LiveDataError::Unsupported(_)) => {}
        other => panic!("an unadvertised venue must be refused locally, got {other:?}"),
    }
    assert!(
        h.feed("binance").subscribe_trades("ETHUSDT").is_ok(),
        "...and an advertised one must still be served"
    );
}

/// **The STATIC matrix is consulted first, before anything is cached and with no server at all.**
/// binance declares `book: false` and polymarket declares `depth: false`
/// (`vike_model::venue_caps`), so `require_live_verb` refuses each with no wire traffic — which is
/// the whole reason `FeedRetries` can classify these as `Refused` and never retry them.
#[test]
fn the_static_capability_matrix_refuses_before_any_connection_exists() {
    let mut h = Harness::new();
    assert!(
        matches!(h.feed("binance").subscribe_book("BTCUSDT"), Err(LiveDataError::Unsupported(_))),
        "binance declares no lossless book lane"
    );
    assert!(
        matches!(
            h.feed("polymarket").subscribe_depth("0xtoken"),
            Err(LiveDataError::Unsupported(_))
        ),
        "polymarket declares no depth-snapshot lane"
    );
}

/// **`subscribe_bars`/`subscribe_quotes` are DECLARED refusals, not accidents — and they must NOT
/// be routed through the venue matrix.** `caps_for("binance").live_data.bars` is `true` and
/// hyperliquid declares `quotes: true`, so a `require_live_verb` route would answer `Ok` and open a
/// wire request for a lane `MdLane` has no variant for.
#[test]
fn the_bar_and_quote_lanes_are_declared_refusals_on_every_venue() {
    let mut h = Harness::new();
    for venue in ["binance", "bybit", "okx", "aster", "hyperliquid", "polymarket"] {
        match h.feed(venue).subscribe_bars("X", "1m") {
            Err(LiveDataError::Unsupported(msg)) => {
                assert!(msg.contains("bar lane"), "{venue}: {msg:?}")
            }
            other => panic!("{venue}: subscribe_bars must be a declared refusal, got {other:?}"),
        }
        match h.feed(venue).subscribe_quotes("X") {
            Err(LiveDataError::Unsupported(msg)) => {
                assert!(msg.contains("quotes lane"), "{venue}: {msg:?}")
            }
            other => panic!("{venue}: subscribe_quotes must be a declared refusal, got {other:?}"),
        }
    }
}

// ── 2. the two refusal halves ────────────────────────────────────────────────────────────────────

/// **A PERMANENT wire refusal leaves the desired set; a CAP refusal stays in it.**
///
/// `MdRefusal::is_permanent` is the split, and it is what decides whether a later `subscribe_*` is
/// answered synchronously (permanent — so `FeedRetries` records `Refused`) or optimistically
/// accepted again (a cap frees up when another window closes).
#[test]
fn a_permanent_refusal_is_remembered_and_a_cap_refusal_is_not() {
    let fake = spawn_fake(
        md_features(),
        Box::new(|specs| ScriptedAnswer {
            accepted: Vec::new(),
            refused: specs
                .iter()
                .map(|s| {
                    let why = match s.lane {
                        MdLane::Trades => MdRefusal::KeyCapTotal { held: 64, cap: 64 },
                        _ => MdRefusal::VenueNotServed("binance".to_string()),
                    };
                    (s.clone(), why)
                })
                .collect(),
            push: Vec::new(),
        }),
    );
    let mut h = Harness::new();
    h.feed("bybit").subscribe_depth("BTCUSDT").expect("optimistic");
    h.feed("bybit").subscribe_trades("BTCUSDT").expect("optimistic");
    h.mount.session.set_addr(Some(&fake.addr.to_string()));

    until("the refusals to land", || h.status("bybit").contains("refused"));
    assert!(
        matches!(h.feed("bybit").subscribe_depth("BTCUSDT"), Err(LiveDataError::Unsupported(_))),
        "a PERMANENT refusal must be answered synchronously from then on"
    );
    assert!(
        h.feed("bybit").subscribe_trades("BTCUSDT").is_ok(),
        "a CAP refusal must NOT be remembered as permanent — a cap frees up"
    );
}

// ── 3-5, 7. the frame routing ────────────────────────────────────────────────────────────────────

/// **A book frame lands in `BookStore` with a LOCAL receipt** — §7.4. Scripted with a `venue_ts`
/// far in the past, so a client that stamped the wire time would store a receipt the DOM reads as
/// permanently stale, and this assertion is what tells the two apart.
#[test]
fn a_book_frame_lands_with_a_local_receipt_not_the_wire_timestamp() {
    const ANCIENT: i64 = 1_500_000_000_000; // 2017 — decades of DOM_STALE_MS ago
    let fake = spawn_fake(
        md_features(),
        Box::new(|specs| {
            ScriptedAnswer::accept_all(specs, vec![book_frame("binance", "BTCUSDT", ANCIENT)])
        }),
    );
    let mut h = Harness::new();
    h.feed("binance").subscribe_depth("BTCUSDT").expect("optimistic");
    h.mount.session.set_addr(Some(&fake.addr.to_string()));

    until("the book to arrive", || h.books.get("binance", "BTCUSDT").is_some());
    let (book, receipt) = h.books.get("binance", "BTCUSDT").expect("the book is there");
    assert!(book.best_bid().is_some(), "the ladder rebuilt from the wire's raw levels");
    assert!(
        receipt > ANCIENT + 1_000_000_000,
        "the receipt is the WIRE timestamp ({receipt}) — every ladder would read permanently stale"
    );
    assert!(h.wakes.load(Ordering::SeqCst) > 0, "the producer must wake the frame thread");
}

/// ⚠⚠ **THE KILL PROOF.** The hub STRIPS every tick's symbol (`md/hub.rs`'s `push_trade`) and the
/// envelope carries it once; `GuiFeedSink::trade` keys `TradeStore` by `trade.symbol`. A reader
/// that hands the tick through unchanged puts every print on every venue under `(venue, "")`, where
/// `sync_from_core`'s drain — which drains under each aggregator's real `(venue, symbol)` — finds
/// nothing. The tick/volume and orderflow charts then stay empty for ever, with no error, no log
/// and nothing else in this tree that would notice.
#[test]
fn trade_ticks_are_re_stamped_from_the_envelope_symbol() {
    let fake = spawn_fake(
        md_features(),
        Box::new(|specs| {
            ScriptedAnswer::accept_all(
                specs,
                vec![MdFrame::Trades {
                    venue: "binance".to_string(),
                    symbol: "BTCUSDT".to_string(),
                    ticks: vec![stripped_tick(1_000, 100.0, 1.0), stripped_tick(1_001, 101.0, 2.0)],
                    seq: 1,
                }],
            )
        }),
    );
    let mut h = Harness::new();
    h.feed("binance").subscribe_trades("BTCUSDT").expect("optimistic");
    h.mount.session.set_addr(Some(&fake.addr.to_string()));

    until("the prints to arrive", || !h.trades.drain("binance", "BTCUSDT").is_empty());
    assert!(
        h.trades.drain("binance", "").is_empty(),
        "prints landed under the EMPTY symbol — the re-stamp is missing and every tick/volume and \
         orderflow chart on this key would stay silently empty"
    );
}

/// **A `TapeGap` discards what is buffered AND bumps the epoch**, in that order — §7.3 rule 3. The
/// epoch is the only route the reader has to the aggregators, which live on the frame thread.
#[test]
fn a_tape_gap_discards_the_buffered_tape_and_bumps_the_epoch() {
    let fake = spawn_fake(
        md_features(),
        Box::new(|specs| {
            ScriptedAnswer::accept_all(
                specs,
                vec![
                    MdFrame::Trades {
                        venue: "binance".to_string(),
                        symbol: "BTCUSDT".to_string(),
                        ticks: vec![stripped_tick(1_000, 100.0, 1.0)],
                        seq: 1,
                    },
                    MdFrame::TapeGap {
                        venue: "binance".to_string(),
                        symbol: "BTCUSDT".to_string(),
                        dropped: 12,
                        from_seq: 1,
                        to_seq: 14,
                    },
                ],
            )
        }),
    );
    let mut h = Harness::new();
    h.feed("binance").subscribe_trades("BTCUSDT").expect("optimistic");
    let key = ("binance".to_string(), "BTCUSDT".to_string());
    h.mount.session.set_addr(Some(&fake.addr.to_string()));

    // ⚠ The subscribe itself bumps the epoch once (a reconnect is a hole — §7.3 rule 4), so the
    // assertion is that the GAP bumps it AGAIN, not that it reaches any particular number.
    until("the gap to be disclosed", || h.mount.session.tape_gap_epoch(&key.0, &key.1) >= 2);
    assert!(
        h.trades.drain("binance", "BTCUSDT").is_empty(),
        "the prints buffered before the hole must be DISCARDED, not folded"
    );
}

/// **A stale book key produces a status and NO book** — §7.3 rule 1. A ladder that is known to be
/// wrong is worse than an empty one, and `BookStore::remove` is what makes "one key" expressible.
#[test]
fn a_stale_status_removes_that_keys_book_and_leaves_its_neighbour_alone() {
    let fake = spawn_fake(
        md_features(),
        Box::new(|specs| {
            ScriptedAnswer::accept_all(
                specs,
                vec![
                    book_frame("binance", "BTCUSDT", 1_700_000_000_000),
                    book_frame("binance", "ETHUSDT", 1_700_000_000_000),
                    MdFrame::Status {
                        venue: "binance".to_string(),
                        symbol: "BTCUSDT".to_string(),
                        lane: MdLane::Depth,
                        status: WireStreamStatus::Stale {
                            newest_data_ts_ms: 1_700_000_000_000,
                            now_ms: 1_700_000_060_000,
                        },
                    },
                ],
            )
        }),
    );
    let mut h = Harness::new();
    h.feed("binance").subscribe_depth("BTCUSDT").expect("optimistic");
    h.feed("binance").subscribe_depth("ETHUSDT").expect("optimistic");
    h.mount.session.set_addr(Some(&fake.addr.to_string()));

    until("the stale key's book to be dropped", || {
        h.books.get("binance", "ETHUSDT").is_some() && h.books.get("binance", "BTCUSDT").is_none()
    });
    assert!(
        h.books.get("binance", "ETHUSDT").is_some(),
        "clearing one key's book must not blank every other ladder and cockpit"
    );
}

// ── 6. unsubscribe does no I/O ───────────────────────────────────────────────────────────────────

/// **`unsubscribe` returns without touching the network**, and neither does `subscribe_*`: the
/// frame thread only ever mutates the desired set and pokes a `Condvar`. Proven by the ABSENCE of a
/// connection — the session has no address, so a dial would be impossible anyway; what this pins is
/// that no call BLOCKS or panics without one, which is the property a frozen GUI would violate.
#[test]
fn subscribe_and_unsubscribe_do_no_io_at_all() {
    let fake =
        spawn_fake(md_features(), Box::new(|specs| ScriptedAnswer::accept_all(specs, Vec::new())));
    let mut h = Harness::new();
    let started = Instant::now();
    let id = h.feed("binance").subscribe_depth("BTCUSDT").expect("optimistic");
    h.feed("binance").unsubscribe(id);
    assert!(
        started.elapsed() < Duration::from_millis(250),
        "a frame-thread call took {:?} — that is a frozen GUI",
        started.elapsed()
    );
    assert_eq!(
        fake.accepted.load(Ordering::SeqCst),
        0,
        "the session dialled a server it was never pointed at"
    );
    // ...and with an empty desired set it must not dial even once it HAS an address: opening a
    // connection to say "nothing, thanks" spends one of the server's stream slots for no reason.
    h.mount.session.set_addr(Some(&fake.addr.to_string()));
    thread::sleep(Duration::from_millis(300));
    assert_eq!(
        fake.accepted.load(Ordering::SeqCst),
        0,
        "an empty desired set must not open a connection"
    );
}

// ── 8. a mid-stream Bye ──────────────────────────────────────────────────────────────────────────

/// **A `Bye` ends the reader and the session re-dials** rather than wedging. The book held on the
/// dead connection is FORGOTTEN on the way out (§7.3 rule 4's opening bracket, synthesized locally
/// because the server that would have sent it is the one that went away) — never resumed across a
/// socket.
#[test]
fn a_mid_stream_bye_drops_the_books_and_the_session_redials() {
    let fake = spawn_fake(
        md_features(),
        Box::new(|specs| {
            ScriptedAnswer::accept_all(
                specs,
                vec![
                    book_frame("binance", "BTCUSDT", 1_700_000_000_000),
                    MdFrame::Bye(MdBye::TooSlow { lapses: 9 }),
                ],
            )
        }),
    );
    let mut h = Harness::new();
    h.feed("binance").subscribe_depth("BTCUSDT").expect("optimistic");
    h.mount.session.set_addr(Some(&fake.addr.to_string()));

    until("the book to arrive", || h.books.get("binance", "BTCUSDT").is_some());
    until("the Bye to drop it again", || h.books.get("binance", "BTCUSDT").is_none());
    assert!(
        h.status("binance").contains("reconnecting"),
        "the status must SAY the link went away, not silently show an empty ladder: {}",
        h.status("binance")
    );
}

// ── 9. the reconciler CONVERGES against a clamping server ────────────────────────────────────────

/// **THE HOT-LOOP KILL PROOF.** A server that CLAMPS the depth must still leave the desired set and
/// the served set comparing EQUAL, or the reconciler re-sends `MdUpdate` for ever.
///
/// This is the sharpest footgun in the client half and it is invisible to every other test here,
/// because the obvious fake — `ScriptedAnswer::accept_all`, which echoes the request verbatim —
/// makes the two sets equal by ANY comparison. The real server does not echo: `MdSpec::depth_levels`
/// is `None` on the wire out (the client asks for the server's own default rather than inflating
/// every subscriber's frame) and `MdSubscribedInfo::accepted` comes back carrying the server's
/// AUTHORITATIVE spec with the depth RESOLVED — `Some(MD_DEPTH_LEVELS_DEFAULT)`. `MdSpec` derives
/// `Eq`/`Hash` over all four fields, so a set diff written the natural way differs on every key for
/// ever and hammers the server with one dial per pass, at the backoff floor, with nothing visibly
/// broken at either end. `MdSpec::key()` — which deliberately omits the depth — is the answer, and
/// this test is what makes removing it fail.
///
/// The proof is the CONNECTION COUNT. The stream socket goes write-only at `MdSubscribe`, so every
/// `MdUpdate` must dial its own short-lived connection: one accept is a converged session, and a
/// diverging one climbs without bound.
#[test]
fn a_clamping_server_converges_and_does_not_hot_loop_md_update() {
    let fake = spawn_fake(
        md_features(),
        Box::new(|specs| {
            // Exactly what the real server does: resolve `None` to its own default and echo the
            // RESOLVED spec back as authoritative.
            let clamped: Vec<MdSpec> = specs
                .iter()
                .map(|s| MdSpec { depth_levels: Some(s.resolved_depth()), ..s.clone() })
                .collect();
            ScriptedAnswer { accepted: clamped, refused: Vec::new(), push: Vec::new() }
        }),
    );
    let mut h = Harness::new();
    h.feed("binance").subscribe_depth("BTCUSDT").expect("optimistic");
    h.mount.session.set_addr(Some(&fake.addr.to_string()));

    until("the stream to be accepted", || fake.accepted.load(Ordering::SeqCst) >= 1);
    // Long enough for many passes of a diverging reconciler: `retry_backoff`'s floor is 1 s, and a
    // set that never converges is re-diffed on every poke as well as on every park expiry.
    thread::sleep(Duration::from_millis(1_500));
    assert_eq!(
        fake.accepted.load(Ordering::SeqCst),
        1,
        "the reconciler kept re-sending MdUpdate against a server that had already accepted the \
         key — the desired/served diff is comparing the CLAMPED depth instead of MdSpec::key()"
    );
}

// ── teardown ─────────────────────────────────────────────────────────────────────────────────────

/// ⚠ **`shutdown` is called ONCE PER VENUE, in PARALLEL, on ONE session** —
/// `App::run_bounded_teardown` fans it over every `App::feeds` entry at once. Unguarded, five of
/// the six would panic on a double `JoinHandle::join`. This drives the real fan-out shape and
/// requires it to complete well inside that teardown's 1500 ms budget.
#[test]
fn a_parallel_shutdown_across_every_feed_is_safe_and_fast() {
    let fake = spawn_fake(
        md_features(),
        Box::new(|specs| {
            ScriptedAnswer::accept_all(specs, vec![book_frame("binance", "BTCUSDT", 1)])
        }),
    );
    let mut h = Harness::new();
    h.feed("binance").subscribe_depth("BTCUSDT").expect("optimistic");
    h.mount.session.set_addr(Some(&fake.addr.to_string()));
    until("the stream to be live", || h.books.get("binance", "BTCUSDT").is_some());

    let started = Instant::now();
    let handles: Vec<_> = std::mem::take(&mut h.mount.feeds)
        .into_values()
        .map(|mut f| thread::spawn(move || f.shutdown()))
        .collect();
    for handle in handles {
        handle.join().expect("a concurrent shutdown panicked — the double join is unguarded");
    }
    assert!(
        started.elapsed() < Duration::from_millis(1_500),
        "teardown took {:?}, past App::run_bounded_teardown's whole budget — the reader is being \
         waited out at its 45s read deadline instead of woken by TcpStream::shutdown",
        started.elapsed()
    );
}

/// The fake's own floor: it really does speak the protocol, so a green above is not a green over a
/// server that answered nothing. Without this, every "nothing was sent" assertion could pass
/// against a listener that never completed a handshake.
#[test]
fn the_fake_datahub_really_serves_the_handshake() {
    let fake =
        spawn_fake(md_features(), Box::new(|specs| ScriptedAnswer::accept_all(specs, Vec::new())));
    let mut stream = TcpStream::connect(fake.addr).expect("connect to the fake");
    stream.set_read_timeout(Some(SETTLE)).expect("arm a deadline");
    write_frame(&mut stream, &Request::Hello { proto_version: PROTO_VERSION }).expect("send Hello");
    match read_frame::<_, Response>(&mut stream) {
        Ok(Response::Welcome { features, .. }) => {
            assert!(features.iter().any(|f| f == FEATURE_MARKET_DATA))
        }
        other => panic!("the fake did not answer a Welcome: {other:?}"),
    }
    // A verb it does not serve is a clean `Response::Error` on a LIVE connection — leg (3) of the
    // capability contract, and the reason `md_subscribe` hands the client back intact.
    write_frame(&mut stream, &Request::ListSeries).expect("send an unserved verb");
    match read_frame::<_, Response>(&mut stream) {
        Ok(Response::Error(_)) => {}
        Err(e) if e.kind() == ErrorKind::WouldBlock || e.kind() == ErrorKind::TimedOut => {
            panic!("the fake stopped answering — the connection was not kept positional")
        }
        other => panic!("expected a clean Error, got {other:?}"),
    }
    assert_eq!(fake.accepted.load(Ordering::SeqCst), 1);
}

/// The unused-field guard: `Harness::wakes` and `Fake::seen` are read by the tests above, but a
/// future edit that stops reading one would leave a dead field rather than a failure. Named here so
/// the omission is loud.
#[test]
fn the_harness_reports_what_the_tests_assert_on() {
    let h = Harness::new();
    assert_eq!(h.wakes.load(Ordering::SeqCst), 0, "a fresh session wakes nobody");
    assert_eq!(h.mount.feeds.len(), 6, "one feed per LOCAL_FEED_VENUES slug");
    assert_eq!(h.mount.feed_statuses.len(), 6, "...and one status line each");
    assert!(
        h.mount.feed_statuses.values().all(|s| !s.lock().unwrap().is_empty()),
        "a venue with no status line renders `Unknown` in the Connections tool"
    );
}

// ── 10. the status vocabulary is PARSED ──────────────────────────────────────────────────────────

/// **NO STATUS LINE THIS SESSION WRITES MAY READ `Connected` WHILE NOTHING IS FLOWING.**
///
/// `vike_model::feed_status::parse_feed_status` is an ORDERED substring classifier — `disconnected`,
/// then `fault|error|failed`, then `connected|live|streaming|subscribed`, then `connect*` — and the
/// Connections tool renders whatever it answers. That makes the word `live` a wire format, not
/// prose, and this session has two lines that naturally contain it and must not:
///
/// * the no-plane refusal, whose whole job is to name `VIKE_DATAHUB_LIVE` and `live-feeds` — so the
///   line carrying it is prefixed `error:`, which is tested one rung EARLIER;
/// * `{live}/{wanted} stream(s) live`, which at `0/1` said Connected about a venue serving nothing,
///   and said it again as `reconnecting (the link dropped) — 0/2 stream(s) live`.
///
/// Both were live defects. A comment cannot hold this; only reading it back through the real
/// classifier can.
#[test]
fn a_status_line_never_reads_connected_while_no_stream_is_serving() {
    use vike_model::feed_status::{ConnectionState, parse_feed_status};

    // A server with no market-data plane at all: the refusal NAMES the two `live`-bearing tokens.
    let fake = spawn_fake(
        vec!["load_bars".to_string()],
        Box::new(|specs| ScriptedAnswer::accept_all(specs, Vec::new())),
    );
    let mut h = Harness::new();
    h.feed("binance").subscribe_depth("BTCUSDT").expect("optimistic");
    h.mount.session.set_addr(Some(&fake.addr.to_string()));

    until("the no-plane refusal to reach the status line", || {
        h.status("binance").contains("live-feeds")
    });
    let line = h.status("binance");
    assert_eq!(
        parse_feed_status(&line),
        ConnectionState::Error,
        "a datahub with NO market-data plane rendered as {:?} in the Connections tool: {line:?}",
        parse_feed_status(&line)
    );

    // ...and the same rule on the other venues, whose lines this session also rewrote.
    for venue in ["okx", "polymarket"] {
        let line = h.status(venue);
        assert_ne!(
            parse_feed_status(&line),
            ConnectionState::Connected,
            "{venue} rendered Connected against a server with no plane: {line:?}"
        );
    }
}

/// The other half of the vocabulary rule, and — because the two share a fixture — the CAP ladder's
/// kill proof as well.
///
/// A venue that WANTS keys and is serving none must not read `Connected`: the `0/{wanted}` arm,
/// which used to render `0/1 stream(s) live` and be classified Connected. The fake refuses
/// everything with a CAP, the RETRYABLE half, so the specs stay wanted and `served` stays empty.
///
/// ⚠ That state is also the one that exposes a hot loop, and reaching it needed the fake to be made
/// honest first: its `MdUpdate` arm used to accept whatever it was sent, so a capped key became
/// served within a millisecond or two and the state under test evaporated. With the script
/// answering both verbs, the cap PERSISTS — which is what a real cap does — and the second
/// assertion below can measure the dial rate it produces.
#[test]
fn a_venue_wanting_keys_and_serving_none_does_not_read_connected() {
    use vike_model::feed_status::{ConnectionState, parse_feed_status};

    let fake = spawn_fake(
        md_features(),
        Box::new(|specs| ScriptedAnswer {
            accepted: Vec::new(),
            refused: specs
                .iter()
                .map(|s| (s.clone(), MdRefusal::KeyCapTotal { held: 64, cap: 64 }))
                .collect(),
            push: Vec::new(),
        }),
    );
    let mut h = Harness::new();
    h.feed("bybit").subscribe_depth("BTCUSDT").expect("optimistic");
    h.mount.session.set_addr(Some(&fake.addr.to_string()));

    until("the capped subscribe to be answered", || h.status("bybit").contains("0/1"));
    let line = h.status("bybit");
    assert_ne!(
        parse_feed_status(&line),
        ConnectionState::Connected,
        "a venue serving NOTHING rendered Connected: {line:?}"
    );

    // ⚠ **AND THE SECOND HOT-LOOP KILL PROOF.** A cap stays WANTED by design, so `diff()` never
    // empties against this server — and a control ladder reset on a successful REPLY rather than on
    // a converged SET re-sends the key with no delay at all, one fresh connection per loop
    // iteration, for ever, against the server that just said it had no room. `retry_backoff` is
    // 1 s, 2 s, 4 s…, so a correct client spends a handful of dials in this window and a broken one
    // spends thousands.
    let before = fake.accepted.load(Ordering::SeqCst);
    thread::sleep(Duration::from_millis(1_200));
    let dials = fake.accepted.load(Ordering::SeqCst) - before;
    assert!(
        dials <= 4,
        "the reconciler opened {dials} connections in 1.2 s for ONE capped key — the control \
         ladder is being reset on the reply instead of on convergence"
    );
}

/// **THE ARM THAT WAS MISSED: a venue with NOTHING subscribed.** It is not an edge case, it is the
/// steady state — `reconcile_loop` renders it through `refresh_statuses("idle")` on the
/// `desired_specs().is_empty()` branch, which every venue is in from the first frame an address
/// resolves until a DOM window or chart opens on it.
///
/// ⚠ The two tests above could not reach it: BOTH call `subscribe_depth` before `set_addr`, so
/// `wanted >= 1` on the venue they assert about and the zero arm never executes. It read
/// `{link} — nothing subscribed on this venue`, and `subscribed` is a rung-3 keyword — so at launch
/// ALL SIX venues rendered **Connected** with no socket open at all, and after one binance ladder
/// opened the other five stayed that way for the session. Prefixing the link does not save it:
/// rung 1's `idle` test is `==`, not `contains`.
#[test]
fn a_venue_with_nothing_subscribed_never_reads_connected() {
    use vike_model::feed_status::{ConnectionState, parse_feed_status};

    let fake =
        spawn_fake(md_features(), Box::new(|specs| ScriptedAnswer::accept_all(specs, Vec::new())));
    let h = Harness::new();
    // No `subscribe_*` at all — exactly what the shell does before a window is opened.
    h.mount.session.set_addr(Some(&fake.addr.to_string()));

    until("the reconciler to render the idle state", || {
        h.status("binance").contains("no streams wanted")
    });
    for venue in vike_app_core::split_plane::LOCAL_FEED_VENUES {
        let line = h.status(venue);
        assert_ne!(
            parse_feed_status(&line),
            ConnectionState::Connected,
            "{venue} rendered Connected with nothing subscribed and no socket open: {line:?}"
        );
    }
    // ...and NOTHING was dialled to say so: a session wanting no keys opens no connection.
    assert_eq!(
        fake.accepted.load(Ordering::SeqCst),
        0,
        "a session with an empty desired set must not open a connection"
    );
}

/// **THE OTHER MISSED ARM: a PERMANENT refusal whose own text carries a rung-3 keyword.**
///
/// `MdRefusal::LaneUnsupported` forwards `vike_data::require_live_verb`'s words verbatim — every one
/// of which names `VenueCaps.live_data`, i.e. `live` — and the arm interpolated `{why:?}`. So a
/// venue this server permanently refused rendered **Connected**, and because that arm outranked
/// every other it kept saying so while the link was down. The fixture the suite already had used
/// `KeyCapTotal`, whose `Debug` carries no keyword, so this arm had no classifier test at all.
#[test]
fn a_lane_refusal_carrying_the_word_live_does_not_read_connected() {
    use vike_model::feed_status::{ConnectionState, parse_feed_status};

    let fake = spawn_fake(
        md_features(),
        Box::new(|specs| ScriptedAnswer {
            accepted: Vec::new(),
            refused: specs
                .iter()
                .map(|s| {
                    (
                        s.clone(),
                        // The server's REAL text for this refusal — `hub.rs`'s `acquire` builds it
                        // as `MdRefusal::LaneUnsupported(e.to_string())` from `require_live_verb`.
                        MdRefusal::LaneUnsupported(
                            "live-data verb unsupported: venue's declared VenueCaps.live_data \
                             serves no L2 depth-snapshot lane"
                                .to_string(),
                        ),
                    )
                })
                .collect(),
            push: Vec::new(),
        }),
    );
    let mut h = Harness::new();
    h.feed("binance").subscribe_depth("BTCUSDT").expect("optimistic");
    h.mount.session.set_addr(Some(&fake.addr.to_string()));

    until("the permanent refusal to reach the status line", || {
        h.status("binance").contains("refused")
    });
    let line = h.status("binance");
    assert!(
        !line.contains("live"),
        "the server's own words were interpolated into a PARSED string: {line:?}"
    );
    assert_ne!(
        parse_feed_status(&line),
        ConnectionState::Connected,
        "a permanently refused venue rendered Connected: {line:?}"
    );
}

// ── 11. the bracket, the blip and the poisoned advertisement ─────────────────────────────────────

/// **§7.3 RULE 4'S BRACKET ON AN ADDRESS CHANGE — the previous server's ladder must be FORGOTTEN.**
///
/// `MdSession::set_addr`'s own doc promises it ("a CHANGE tears the stream down and applies §7.3
/// rule 4's bracket"), and it did not run: the reconciler cleared `served` three statements after
/// `begin_stream_teardown()`, while `open_gap_bracket` — which the READER runs, and whose entire
/// input is `served` — needs a kernel wake-up first. The reconciler won essentially always, `held`
/// was empty, and the bracket removed no book, bumped no epoch and logged nothing. Nothing else in
/// this binary clears `BookStore`, so the old server's ladder stayed painted for ever (flagged STALE
/// after `DOM_STALE_MS`, i.e. readable).
#[test]
fn an_address_change_drops_the_previous_servers_books() {
    let a = spawn_fake(
        md_features(),
        Box::new(|specs| {
            ScriptedAnswer::accept_all(specs, vec![book_frame("binance", "BTCUSDT", 1)])
        }),
    );
    // B accepts and pushes NOTHING, so any book present after the re-point came from A.
    let b =
        spawn_fake(md_features(), Box::new(|specs| ScriptedAnswer::accept_all(specs, Vec::new())));
    let mut h = Harness::new();
    h.feed("binance").subscribe_depth("BTCUSDT").expect("optimistic");
    h.mount.session.set_addr(Some(&a.addr.to_string()));
    until("A's book to land", || h.books.get("binance", "BTCUSDT").is_some());

    h.mount.session.set_addr(Some(&b.addr.to_string()));
    until("the re-point to forget A's book", || h.books.get("binance", "BTCUSDT").is_none());
    until("the session to reconnect to B", || h.status("binance").contains(&b.addr.to_string()));
    assert!(
        h.books.get("binance", "BTCUSDT").is_none(),
        "B pushed no book, so a present one is A's — carried across a re-point"
    );
}

/// **A `None` ADDRESS IS NOT A RE-POINT.** `observe_bridge` clears its `advertised_datahub` cell the
/// moment the tradehub link drops and restores it only after the next handshake, a `DIAL_BACKOFF`
/// later — and on a box with no explicit `config.datahub_addr` that advertisement is the WHOLE
/// address ladder. So every tradehub blip arrived here as `set_addr(None)`, which the address-change
/// arm treated as a different server: stream torn down, `Caps` thrown away, every permanent refusal
/// re-opened, every venue's line rewritten to name a misconfiguration that does not exist. The
/// datahub is a different PROCESS and did not go anywhere.
#[test]
fn a_cleared_advertisement_does_not_tear_the_stream_down() {
    let fake = spawn_fake(
        md_features(),
        Box::new(|specs| {
            ScriptedAnswer::accept_all(specs, vec![book_frame("binance", "BTCUSDT", 1)])
        }),
    );
    let mut h = Harness::new();
    h.feed("binance").subscribe_depth("BTCUSDT").expect("optimistic");
    h.mount.session.set_addr(Some(&fake.addr.to_string()));
    until("the book to land", || h.books.get("binance", "BTCUSDT").is_some());
    let dials = fake.accepted.load(Ordering::SeqCst);

    // The blip: the bridge drops its advertisement for a moment and puts it back.
    h.mount.session.set_addr(None);
    thread::sleep(PUSH_GAP);
    h.mount.session.set_addr(Some(&fake.addr.to_string()));
    thread::sleep(PUSH_GAP);

    assert!(
        h.books.get("binance", "BTCUSDT").is_some(),
        "a tradehub blip dropped the DOM ladder of a datahub that never went anywhere"
    );
    assert_eq!(
        fake.accepted.load(Ordering::SeqCst),
        dials,
        "a blip re-dialled the datahub — the stream was torn down and rebuilt for nothing"
    );
    assert!(
        !h.status("binance").contains("no datahub configured"),
        "a blip rewrote the status line to name a misconfiguration that does not exist: {:?}",
        h.status("binance")
    );
}

/// **A PLANE-LESS SERVER MUST NOT POISON THE SESSION FOR EVERY LATER ADDRESS.**
///
/// `dial` caches the advertisement BEFORE it refuses — deliberately, so `want` can refuse locally
/// rather than re-attempting for ever — but the clear was keyed on `connected_addr`, which a refused
/// dial never sets. So `Caps { market_data: false }` survived an address change, `want`'s rung 2
/// answered `Unsupported` against the NEW server too, and `FeedRetries` records that as
/// `RetryState::Refused` and never re-asks: every DOM window opened in that window was dead for the
/// life of the process. `Caps`' own doc asserts the opposite ("It IS cleared when the address
/// changes"), which was false in exactly the case where the cached value was harmful.
#[test]
fn a_plane_less_servers_advertisement_does_not_survive_a_repoint() {
    let none = spawn_fake(
        vec!["load_bars".to_string()],
        Box::new(|specs| ScriptedAnswer::accept_all(specs, Vec::new())),
    );
    let good =
        spawn_fake(md_features(), Box::new(|specs| ScriptedAnswer::accept_all(specs, Vec::new())));
    let mut h = Harness::new();
    h.feed("binance").subscribe_depth("BTCUSDT").expect("optimistic");
    h.mount.session.set_addr(Some(&none.addr.to_string()));
    until("the no-plane refusal to be cached", || h.status("binance").contains("live-feeds"));
    assert!(
        matches!(
            h.feed("okx").subscribe_depth("BTC-USDT-SWAP"),
            Err(LiveDataError::Unsupported(_))
        ),
        "the cached advertisement must refuse locally while it describes THIS server"
    );

    h.mount.session.set_addr(Some(&good.addr.to_string()));
    until("the session to connect to the SECOND server", || {
        h.status("binance").contains(&good.addr.to_string())
    });
    assert!(
        h.feed("okx").subscribe_depth("BTC-USDT-SWAP").is_ok(),
        "the plane-less server's advertisement outlived the address that produced it — every \
         subscribe made from here is a `FeedRetries::Refused` that is never re-asked"
    );
}

/// **A `Status::GapStart` EMPTIES THE LADDER, SO THE STATUS LINE MUST STOP SAYING `live`.**
///
/// `served` is the server's ACCEPTED set and a gap does not change it, so the last arm rendered
/// `1/1 stream(s) live` — Connected — over a ladder this client had just removed. An operator
/// staring at a blank DOM was told by the Connections tool that the venue was fine.
#[test]
fn a_gapped_book_lane_stops_reading_connected() {
    use vike_model::feed_status::{ConnectionState, parse_feed_status};

    let fake = spawn_fake(
        md_features(),
        Box::new(|specs| {
            ScriptedAnswer::accept_all(
                specs,
                vec![
                    book_frame("binance", "BTCUSDT", 1),
                    MdFrame::Status {
                        venue: "binance".to_string(),
                        symbol: "BTCUSDT".to_string(),
                        lane: MdLane::Depth,
                        status: WireStreamStatus::GapStart { at_ts_ms: 5 },
                    },
                ],
            )
        }),
    );
    let mut h = Harness::new();
    h.feed("binance").subscribe_depth("BTCUSDT").expect("optimistic");
    h.mount.session.set_addr(Some(&fake.addr.to_string()));

    until("the book to land and then be dropped by the gap", || {
        h.books.get("binance", "BTCUSDT").is_none() && h.status("binance").contains("0/1")
    });
    let line = h.status("binance");
    assert_ne!(
        parse_feed_status(&line),
        ConnectionState::Connected,
        "a venue whose ladder was just emptied still read Connected: {line:?}"
    );
}

/// **A `Status::GapStart` ON THE TRADES LANE MARKS A TAPE GAP.** Nothing else discloses a
/// VENUE-side hole in a tape: it increments no `entry.dropped`, so the server emits no `TapeGap`;
/// the wire `seq` counts published frames rather than venue ticks, so §7.2's backstop sees nothing.
/// Folding across it leaves `OrderflowAgg` — no per-trade dedup, no gap concept — understating that
/// bar's cells and every CVD value after it, permanently and silently.
///
/// ⚠ This WIDENS §7.3 rule 1, which names the book lanes only. It is deliberate and is flagged in
/// the report; the alternative reading, that a venue-side tape hole is undisclosed by design,
/// contradicts §6.2's "Loss here must be impossible to hide".
#[test]
fn a_gap_on_the_trades_lane_bumps_the_tape_epoch() {
    let fake = spawn_fake(
        md_features(),
        Box::new(|specs| {
            ScriptedAnswer::accept_all(
                specs,
                vec![MdFrame::Status {
                    venue: "binance".to_string(),
                    symbol: "BTCUSDT".to_string(),
                    lane: MdLane::Trades,
                    status: WireStreamStatus::GapStart { at_ts_ms: 5 },
                }],
            )
        }),
    );
    let mut h = Harness::new();
    h.feed("binance").subscribe_trades("BTCUSDT").expect("optimistic");
    h.mount.session.set_addr(Some(&fake.addr.to_string()));

    // The subscribe itself bumps once (§7.3 rule 4's closing half for the tape), so the assertion
    // is that the STATUS moved it again rather than that it is non-zero.
    until("the subscribe's own bump", || h.mount.session.tape_gap_epoch("binance", "BTCUSDT") >= 1);
    until("the venue-side gap to be disclosed to the aggregators", || {
        h.mount.session.tape_gap_epoch("binance", "BTCUSDT") >= 2
    });
}
