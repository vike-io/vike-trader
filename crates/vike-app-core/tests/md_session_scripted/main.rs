//! `MdSession` driven end-to-end against an IN-PROCESS FAKE DATAHUB — a `TcpListener` on an
//! ephemeral loopback port speaking the REAL `vike_datahub_client::proto` (design §9 item 9).
//!
//! The pattern is `crates/vike-app-core/tests/backfill_wire_scripted.rs`'s, for its reason: proving
//! **"nothing was sent"** needs a server that can COUNT, and proving the request stream needs one
//! that can RECORD. Loopback + in-memory only — no store on disk, no venue, no GUI, no GPU.
//!
//! ⚠ **No feed exists until a server has been heard from.** `md_session::build` hands the shell EMPTY
//! feed and status maps and `MdSession::sync_mount` fills them from the server's advertisement, which
//! the reconciler learns by a handshake-only PROBE. So a test that wants a feed starts with
//! [`Harness::attach`] (point the session at a fake, wait for the probe, mount what it advertised);
//! the fake counts that probe as a connection, and `mount_from_handshake` pins the probe itself.
//!
//! What is pinned here, in the order the design lists it, plus THREE the design does not list and
//! that this client must not ship without:
//!
//! 1. an UNADVERTISED server mounts no feed, is sent zero frames after the handshake, and the link
//!    line names the fix;
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

use std::assert_matches;
use std::collections::HashMap;
use std::io::ErrorKind;
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use vike_app_core::data::data_sink::{BookStore, TradeStore};
use vike_app_core::data::md_session::{self, MdMount};
use vike_data::{DataClient, LiveDataError};
use vike_datahub_client::{
    FEATURE_MARKET_DATA, MdBye, PROTO_VERSION,
    market::{MdFrame, MdLane, MdRefusal, MdSessionId, MdSpec},
    md_venue_feature,
    proto::{Request, Response, read_frame, write_frame},
};
use vike_marketdata::BookLevel;
use vike_marketdata::TradeTick;
use vike_model::StreamStatus;

#[path = "../common/mod.rs"]
mod common;
mod lifecycle;
mod mount_from_handshake;
mod refusals_and_frames;
mod repoint_and_gaps;
mod status_line;

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
/// wasted second. `FnMut` so a poll may itself drive the frame thread's half (`Harness::sync`).
fn until(what: &str, mut f: impl FnMut() -> bool) {
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
                if !common::accept_keyless(&mut stream, features) {
                    return;
                }
                let mut kinds = vec!["Hello".to_string()];
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
    /// `vike_secrets::CredentialStore::node_keys` over `vike_secrets::workspace_settings_dir_from`,
    /// whose `None` means *walk up from the working directory for
    /// a project marker* — so on a box that has one, this suite would open the developer's real
    /// settings database and its answer would depend on whose checkout it ran in. A FIXED name under the
    /// system temp directory is what `crates/vike-ops/tests/hygiene/temp_path_gate.rs` refuses, and for a
    /// reason this box has already paid: CI and agents run as different users, whoever creates the
    /// directory first owns it, and every later run under the other user fails permanently. A
    /// `tempdir` is unique AND self-deleting; it holds no settings database, which
    /// `vike_secrets::CredentialStore::node_keys` answers with an empty map, silently.
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

    /// The frame thread's half of the mount: what `vike-desktop` does once per frame, beside
    /// `set_addr`.
    fn sync(&mut self) {
        self.mount.session.sync_mount(&mut self.mount.feeds, &mut self.mount.feed_statuses);
    }

    /// Point the session at `fake`, wait for the reconciler's PROBE to learn what it advertises (it
    /// has nothing else to dial for: nothing is wanted yet), and mount what was advertised. Nothing
    /// more is asked of the server — a feed is mounted, not subscribed.
    fn attach(&mut self, fake: &Fake) {
        self.mount.session.set_addr(Some(&fake.addr.to_string()));
        let session = self.mount.session.clone();
        until("the probe to learn the advertisement", || session.caps().is_some());
        self.sync();
    }

    fn feed(&mut self, venue: &str) -> &mut Box<dyn DataClient + Send> {
        self.mount
            .feeds
            .get_mut(venue)
            .expect("the venue was advertised and mounted (`Harness::attach` first)")
    }

    fn status(&self, venue: &str) -> String {
        self.mount.feed_statuses[venue].lock().unwrap().clone()
    }

    /// The venues currently mounted, sorted — what a test compares against what a server advertised.
    fn mounted(&self) -> Vec<&'static str> {
        let mut venues: Vec<&'static str> = self.mount.feeds.keys().copied().collect();
        venues.sort_unstable();
        venues
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
        bids: vec![BookLevel::new(100.0, 1.0)],
        asks: vec![BookLevel::new(100.5, 2.0)],
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

/// The handshake of a datahub that serves market data for exactly `venues` (`md_venue=` entries, in
/// the order given).
fn features_for(venues: &[&str]) -> Vec<String> {
    let mut f = vec!["load_bars".to_string(), FEATURE_MARKET_DATA.to_string()];
    f.extend(venues.iter().copied().map(md_venue_feature));
    f
}

/// A datahub serving the six venues the earlier suites were written against.
fn md_features() -> Vec<String> {
    features_for(&["binance", "bybit", "okx", "aster", "hyperliquid", "polymarket"])
}
