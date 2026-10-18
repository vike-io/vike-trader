//! `RemoteDataClient` — a `vike_data::DataClient` answered by the gatewayhub: the seam the datahub's
//! feed broker plugs into in place of an in-process venue client.
//!
//! The engine is generalised over an [`Opener`] (`crate::opener`): the dial and the handshake stay with the
//! client crate whose protocol hosts the `stream` socket (the gatewayhub's `GatewayOpener`), and the
//! engine frames, waits and backs off with the numbers that opener states. Its messages still say
//! `gatewayhub`: the one opener there is.
//!
//! One `RemoteDataClient` is ONE `stream` session (one TCP socket, one venue client at the daemon).
//! The broker holds one per `ClientKey`, so nothing one session does can gap another. Everything the
//! in-process contract promises is kept (`vike_data::live`'s module doc), and each promise names the
//! test that holds it (`crates/vike-gatewayhub-client/tests/dataclient_contract.rs`, drift register R1, which
//! runs this engine over the gatewayhub's opener):
//!
//! * **`subscribe_*` is fire-and-forget over a request/ack.** The id is allocated CLIENT-side
//!   (idempotent: a reconnect replays the same `sid`), the call sends `Sub`, and returns when the
//!   daemon acks that the in-process `subscribe_*` it forwarded returned — never "live". `Err` means
//!   the stream was not started: `Subscribe(text)` and `Unsupported(&'static str)` keep their
//!   variants and their text (the latter through [`crate::intern`], with no `vike-data` edit).
//! * **It never blocks the broker's per-venue mutex on a dead socket.** The broker holds that mutex
//!   across `subscribe_*`. A link that is down answers a transient `Err` at once; the only waits are
//!   the daemon's ack (bounded by `StreamTimings::sub_reply`) and, for the FIRST subscribe after
//!   construction or after an idle park, a dial that is already in flight (at most
//!   [`FIRST_CONNECT_WAIT`]).
//! * **Every frame is delivered INLINE, on one named reader thread, in wire order**, into the
//!   `Arc<dyn LiveDataSink>` handed over at construction. The reader also owns reconnecting, so there
//!   is exactly one thread per session.
//! * **A heartbeat deadline.** The open reply states the daemon's period; the reader arms
//!   `max(stream_read_floor, 3 × heartbeat_ms)` against the next frame. A silent link is a lost link.
//! * **A lost link is DISCLOSED, then repaired.** At the instant of loss the client synthesizes a
//!   `GapStart` for every held key (deduplicated per `(symbol, stream word)`), stamped as the next
//!   bullet says; it
//!   redials with backoff, REPLAYS every held `Sub` under its original `sid`, and on each `SubOk` of
//!   the new session synthesizes `Live { gap_started_ts_ms }` for the key — "the first frame back" is
//!   the daemon re-acknowledging the key. A replay the daemon refuses with a PERMANENT refusal
//!   ([`SubErrKind::is_permanent`]: `Unsupported`, `SymbolRejected`) leaves that key gapped for good and
//!   says so in the log, at `error` and again every `DEAD_REMINDER` while it stays so (the broker
//!   believes the subscription is live and never asks again, so nothing else would);
//!   ANY OTHER refusal (a venue start failure, a full budget, a draining session)
//!   keeps the key held and is re-sent at the reconnect backoff until it is acknowledged, because the
//!   broker still believes the subscription is live and will not ask again. A deliberate shutdown
//!   synthesizes nothing.
//! * **Silent loss is disclosed too.** The six lossless verbs carry a per-`(k, verb)` `seq`; a jump
//!   with no `Gap` before it is reported to the sink as a `GapStart`/`Live` pair and logged. A `Gap`
//!   frame is reported the same way. Nothing is ever dropped without a disclosure.
//! * **A hole is stamped with WHEN IT OPENED, never with the arrival clock.** The recorder files a
//!   `GapStart` under its `at_ts_ms` and replay orders rows by `(ts, seq)`, so a marker stamped `now`
//!   behind a slow reader lands after the survivors it should precede. A `Gap` frame carries the stamp
//!   of the last frame delivered before the hole (`from_ts_ms`); a sequence break with no `Gap` uses
//!   the stamp of the last frame this reader delivered for that `(key, verb)`. Only a verb whose frames
//!   carry no stamp at all (`book`, the folded state) falls back to the arrival clock. **The same rule
//!   stamps a LINK LOSS**: the `GapStart` of a held `(symbol, lane)` carries the stamp of the last frame
//!   this session delivered for it (falling back to the instant the lane was acknowledged, and for a
//!   lane that never was, to the instant the session opened), NOT the instant the loss was noticed -
//!   rows the daemon produced and never delivered (its queue tail on `Bye(TooSlow)`, up to a
//!   heartbeat deadline of silence) lie between the two, and a marker stamped later would leave them
//!   outside the gap it opens.
//! * **A panicking sink ends ONE subscription, never the session.** Every frame is delivered inline on
//!   the one reader thread, so a panic in a sink callback would otherwise unwind the reader and end
//!   every key of the venue for good (in-process it killed one feed thread). Each data callback runs
//!   under `catch_unwind`: the panic is logged once at `error`, the `(key, lane)` it came from is
//!   dropped from the held set (the daemon is told `Unsub`), a `GapStart` is disclosed for it (so a
//!   consumer's status shows it down, not `Live` forever) and the frames still in flight for it are
//!   dropped quietly. The session and every other key and lane keep running. A panic inside a STATUS
//!   callback (`stream_status`, `data_delay`) loses only that notification, logged at `error`.
//! * **The daemon's conflation is counted.** The four conflatable verbs (`forming`, `mark`,
//!   `bar_close_tick`, `depth`) are latest-wins in the daemon's queue when this reader is behind, and
//!   by the sink's contract that is not a loss, so no `GapStart` is invented for it. But the daemon
//!   numbers every frame before it decides what to drop, so a jump in their `seq` is the count of
//!   frames it replaced: [`RemoteDataClient::conflated`] reports it and the reader says it at `warn`
//!   at most once per `CONFLATION_REPORT` - the in-process recorder counted its dropped rows too, and
//!   a recorded `kind=depth` tape is thinner for them.
//! * **The delivery class of a subscription is the broker's to raise.** `set_delivery` (keyed by
//!   `(symbol, lane)`, as the broker holds its keys) remembers a floor that a `Sub` then carries, or
//!   sends a `Delivery` frame for a subscription already open; the same floor rides every replay, so a
//!   reconnect restores it. A raised depth lane is lossless: the daemon announces the change in frame
//!   order (`StreamDown::Delivery`) and from there this reader checks the lane's `seq` like a
//!   `quote`'s (`GapVerb::Depth`) instead of counting its skips as conflation. A daemon without the
//!   `stream_class` feature (`DataStream::delivery_class`) is answered `Unsupported`, once, and the
//!   subscription runs conflated as it always did.
//! * **The tick pump is the one INHERENT verb.** [`RemoteDataClient::subscribe_ticks`] holds a
//!   [`SubOp::Ticks`] subscription: not a `DataClient` verb (that trait has no tick verb and is not
//!   edited), but held, acknowledged, replayed and disclosed exactly like the others, under the pump's
//!   `"ticks"` link word. Its frames are ordinary quote / trade / book frames of the symbol's key.
//! * **Two-phase teardown.** `begin_shutdown` sends `BeginShutdown` and returns at once (the daemon
//!   raises the stop flags of the session's venue client); the session stays usable, as an in-process
//!   client does, because the broker's drain may end and subscribe again. `shutdown` (and `Drop`)
//!   sends `Close`, gives the reader [`SHUTDOWN_GRACE`] to see `Bye`, then breaks the socket and
//!   joins. Both are idempotent.

use std::any::Any;
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::io;
use std::net::{Shutdown, TcpStream};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock, PoisonError, Weak};
use std::thread::{JoinHandle, ThreadId};
use std::time::{Duration, Instant};

use vike_data::delivery::Delivery;
use vike_data::{DataClient, LiveDataError, LiveDataSink, StreamLabel, SubscriptionId};
use vike_model::{StreamStatus, now_ms};

use crate::frame::{is_timeout, read_capped, write_up};
use crate::opener::{DataStream, Opener, reconnect_delay, stream_deadline};
use crate::wire::{
    ByeReason, GapVerb, LINK_STREAM_WORD, StreamDown, StreamUp, SubErrKind, SubOp, WireDelivery,
    WireLane, WireStream, validate_tick_size,
};

/// The longest the FIRST `subscribe_*` after construction (or after an idle park) waits for the dial
/// already in flight. A healthy loopback daemon answers in milliseconds, so the first subscription is
/// not turned into a retry; a dead one refuses at once and the wait ends with it. After that, a link
/// that is down is a transient `Err` with no wait at all.
pub const FIRST_CONNECT_WAIT: Duration = Duration::from_secs(2);

/// The phrase every TRANSIENT refusal of a `subscribe_*` carries (a link that is down, an ack that never came, a
/// budget that is full, a draining session): the subscription was not started and the caller's next pass may
/// succeed. A refusal WITHOUT it is the peer's own answer (an unsupported verb, a spelling, a venue that
/// refused), which a retry does not change. [`is_transient_refusal`] reads it, so a caller that holds only the
/// refusal's text (the tradehub's feed arms format `{e:?}`) can tell the two apart without a second type.
pub const TRANSIENT_MARK: &str = "retried on the caller's next pass";

/// The phrase a CAP refusal carries (a daemon already hosting the most pumps, sessions or streams it is built to
/// hold): the refusal is not a link that is down, it persists until an operator raises the cap or drops a
/// subscription, and a retry meets it again. A caller that restarts itself on a transient refusal (the tradehub's
/// late arm) must not restart on this: [`is_transient_refusal`] answers `false` for any text carrying it, whatever
/// else the text says.
pub const CAP_MARK: &str = "a cap, not a link";

/// Whether `text` (a refusal of a `subscribe_*`, as `Display` or `Debug` renders it) is a TRANSIENT one: it carries
/// [`TRANSIENT_MARK`], which every link-class refusal this engine and the daemons' budgets raise does, and does NOT
/// carry [`CAP_MARK`] (a cap is persistent however it is worded).
#[must_use]
pub fn is_transient_refusal(text: &str) -> bool {
    text.contains(TRANSIENT_MARK) && !is_cap_refusal(text)
}

/// Whether `text` is a CAP refusal ([`CAP_MARK`]): the class that needs an operator. The tradehub keeps such a
/// refusal's venue UP and priceless (no restart is ordered for it) where it can, and where it cannot (a running
/// live account) ends the start with a status the supervisor does not restart on.
#[must_use]
pub fn is_cap_refusal(text: &str) -> bool {
    text.contains(CAP_MARK)
}

/// How long `shutdown` gives the reader to see the daemon's `Bye` before it breaks the socket.
pub const SHUTDOWN_GRACE: Duration = Duration::from_secs(2);

/// A session that stayed up this long resets the reconnect backoff; a daemon that accepts and
/// immediately closes is retried at the backed-off cadence, not hammered.
const HEALTHY_SESSION: Duration = Duration::from_secs(10);

/// How often a held subscription the daemon refused for good on replay ([`Phase::Dead`]) is said again,
/// at `error`, while it stays so. The refusal itself is logged once, when it happens; the broker (and the
/// recorder's subscription set) believe the subscription live and never ask again, so without a repeat
/// a feed that silently never came back would be one line in a log nobody was reading.
const DEAD_REMINDER: Duration = Duration::from_secs(600);

/// The most often the reader says, at `warn`, that the daemon conflated frames because this reader was
/// behind ([`RemoteDataClient::conflated`]). A count and a level, never one line per frame.
const CONFLATION_REPORT: Duration = Duration::from_secs(60);

/// Why `set_delivery` is declined: the daemon this session talks to did not advertise the delivery
/// class (`DataStream::delivery_class`), so it would ignore a floor it was sent. The broker warns and
/// carries on with the subscription conflated; an upgrade of the daemon is the remedy.
const NO_STREAM_CLASS: &str = "gatewayhub: the daemon does not advertise stream_class, so a delivery \
     floor cannot be honoured; upgrade the daemon";

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// How long the first `subscribe_*` after construction (or after an idle park) waits for the dial in flight: the
/// opener's own figure when it states one ([`Opener::first_dial_wait`]: a dial whose open includes a handshake
/// that is allowed longer than [`FIRST_CONNECT_WAIT`]), else [`FIRST_CONNECT_WAIT`] narrowed to the ack wait.
fn first_dial_wait(opener: &(impl Opener + ?Sized), venue: &str) -> Duration {
    opener
        .first_dial_wait(venue)
        .unwrap_or_else(|| FIRST_CONNECT_WAIT.min(opener.timings().sub_reply))
}

/// Where the session's link is.
enum LinkState {
    /// Not up: dialling, or backing off after `why`.
    Connecting { why: String },
    /// A session is open and acknowledged.
    Up,
    /// The client was shut down for good.
    Stopped,
}

/// Where one subscription stands.
enum Phase {
    /// A first-time `subscribe_*` is waiting for its ack.
    Awaiting,
    /// The daemon acknowledged it (the key id lives in the reader's per-session map).
    Granted,
    /// The daemon refused it; the waiter takes this and removes the entry.
    Refused(SubErrKind),
    /// The link was lost with the subscription held: it is replayed on the next session, and its
    /// `GapStart` (at `gap_at`) is still open. `tries` counts the transient refusals the replay has had
    /// so far, which is where its retry backoff stands.
    Replaying { gap_at: i64, tries: u32 },
    /// A replay was refused for good ([`SubErrKind::is_permanent`]): the gap stays open.
    Dead,
}

struct Entry {
    op: SubOp,
    phase: Phase,
}

/// What a waiting `subscribe_*` saw of its entry: an owned summary of [`Phase`].
enum Seen {
    Vanished,
    Awaiting,
    Granted,
    Refused(SubErrKind),
    LinkLost,
}

struct Inner {
    link: LinkState,
    subs: BTreeMap<u64, Entry>,
    stopping: bool,
    exited: bool,
    /// The last session ended idle with nothing held: the reader sleeps until the next subscribe.
    parked: bool,
    /// Until when a `subscribe_*` may wait for the dial in flight: set at construction and at an idle
    /// wake-up for [`FIRST_CONNECT_WAIT`], cleared when that dial resolves. ONE deadline for the dial, not
    /// a wait per call, so a stalled dial costs the broker's mutex that long once and not per subscribe.
    fresh_until: Option<Instant>,
    /// Symbols whose LAST subscription was unsubscribed since the reader last looked: it drops what it
    /// keeps per key for them (`trim_retired`). Filled by `unsubscribe`, drained by the reader.
    retired: Vec<String>,
    /// The delivery class floor the broker asked for, per `(symbol, lane)` (`set_delivery`): only a
    /// floor above the verbs' own is kept. Read at every `Sub` that is sent, first time or replay, so
    /// a subscription made after the call, or replayed after a link loss, carries it.
    floors: HashMap<(String, WireLane), WireDelivery>,
    /// Whether the current (or last) session's daemon serves the delivery class
    /// (`DataStream::delivery_class`); `None` until a session has opened.
    class_support: Option<bool>,
}

struct Shared {
    opener: Box<dyn Opener>,
    venue: String,
    sink: Arc<dyn LiveDataSink>,
    inner: Mutex<Inner>,
    /// Signalled on: a sub reply, a link-state change, a stop, a wake-up.
    changed: Condvar,
    /// Mirrors `Inner::stopping` for lock-free checks on the reader's hot path.
    stop: AtomicBool,
    /// The current session's write half.
    writer: Mutex<Option<TcpStream>>,
    /// The socket of a dial in flight (before the session is open), so a shutdown can break a
    /// handshake parked on a daemon that accepted and went quiet. Never written to.
    connecting: Mutex<Option<TcpStream>>,
    /// The reader thread, set when it starts: a sink callback runs ON it, and a `subscribe_*` or
    /// `shutdown` made from there would wait for an ack, or a join, only that same thread can deliver.
    reader: OnceLock<ThreadId>,
    /// `Inner::retired` has something for the reader: checked lock-free on its hot path.
    retire: AtomicBool,
    /// How many keys the current session has bound (a diagnostic, written by the reader).
    bound_keys: AtomicUsize,
    /// Conflatable frames (`forming`, `mark`, `bar_close_tick`, `depth`) the daemon superseded before
    /// they reached this reader, over the client's life: the jumps in their `seq` ([`note_conflated`]).
    conflated: AtomicU64,
}

/// A `DataClient` over one gatewayhub `stream` session. See the module doc.
pub struct RemoteDataClient {
    shared: Arc<Shared>,
    manager: Option<JoinHandle<()>>,
    next_sid: u64,
}

impl std::fmt::Debug for RemoteDataClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RemoteDataClient")
            .field("venue", &self.shared.venue)
            .field("addr", &self.shared.opener.addr())
            .finish_non_exhaustive()
    }
}

/// A non-owning view of whether a [`RemoteDataClient`]'s session is open ([`RemoteDataClient::link_probe`]).
///
/// It exists for the sink the client delivers into. The reader calls `stream_status` for a link loss
/// AFTER it has marked the session down and BEFORE it redials, with no lock held, so a sink that reads
/// `is_up` there knows the `GapStart` it is handed is the link's own and not a status the peer
/// forwarded (those arrive only on an open session). It holds a `Weak` reference, so the sink never keeps
/// the client's state alive (the client owns the sink: a strong edge back would be a cycle), and a probe
/// whose client is gone reads `false`.
#[derive(Clone)]
pub struct LinkProbe(Weak<Shared>);

impl LinkProbe {
    /// Whether the session is open and acknowledged right now (`false` once the client is dropped).
    pub fn is_up(&self) -> bool {
        self.0.upgrade().is_some_and(|sh| matches!(lock(&sh.inner).link, LinkState::Up))
    }
}

impl std::fmt::Debug for LinkProbe {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LinkProbe").finish_non_exhaustive()
    }
}

/// The reader thread's name: `gwh-md-<venue>`, kept ASCII and within the 15 bytes Linux keeps.
fn thread_name(venue: &str) -> String {
    let mut name = String::from("gwh-md-");
    name.extend(venue.chars().filter(char::is_ascii_graphic));
    name.truncate(15);
    name
}

impl RemoteDataClient {
    /// A session for `venue`, delivering into `sink`. **Never blocks and never dials on the caller's
    /// thread**: the reader thread dials, so a gatewayhub that is down at construction is a link that
    /// is down, not a failed constructor (drift register R17: no crash loop, no exit). The only `Err`
    /// is the OS refusing a thread.
    pub fn new(opener: impl Opener, venue: &str, sink: Arc<dyn LiveDataSink>) -> io::Result<Self> {
        let first_wait = first_dial_wait(&opener, venue);
        let shared = Arc::new(Shared {
            opener: Box::new(opener),
            venue: venue.to_string(),
            sink,
            inner: Mutex::new(Inner {
                link: LinkState::Connecting { why: "not dialled yet".to_string() },
                subs: BTreeMap::new(),
                stopping: false,
                exited: false,
                parked: false,
                fresh_until: Some(Instant::now() + first_wait),
                retired: Vec::new(),
                floors: HashMap::new(),
                class_support: None,
            }),
            changed: Condvar::new(),
            stop: AtomicBool::new(false),
            writer: Mutex::new(None),
            connecting: Mutex::new(None),
            reader: OnceLock::new(),
            retire: AtomicBool::new(false),
            bound_keys: AtomicUsize::new(0),
            conflated: AtomicU64::new(0),
        });
        let for_thread = Arc::clone(&shared);
        let manager =
            std::thread::Builder::new().name(thread_name(venue)).spawn(move || run(&for_thread))?;
        Ok(Self { shared, manager: Some(manager), next_sid: 1 })
    }

    /// The venue this session serves.
    pub fn venue(&self) -> &str {
        &self.shared.venue
    }

    /// Whether a session is open and acknowledged right now.
    pub fn is_up(&self) -> bool {
        matches!(lock(&self.shared.inner).link, LinkState::Up)
    }

    /// A [`LinkProbe`] for this client: [`RemoteDataClient::is_up`] for a holder that cannot hold the
    /// client itself (the sink this client delivers into).
    pub fn link_probe(&self) -> LinkProbe {
        LinkProbe(Arc::downgrade(&self.shared))
    }

    /// How many keys the CURRENT session has bound: a diagnostic. A key is dropped once the last
    /// subscription of its symbol is unsubscribed (the reader notices on its next frame, a heartbeat
    /// at the latest), so this does not grow with the symbols a long-lived client has ever touched.
    pub fn bound_keys(&self) -> usize {
        self.shared.bound_keys.load(Ordering::Relaxed)
    }

    /// How many conflatable frames (`forming`, `mark`, `bar_close_tick`, `depth`) the daemon has
    /// superseded before this client could read them, over its life: a reader that falls behind makes
    /// the daemon keep only the latest of each. By the sink's contract that is not a loss (the latest
    /// snapshot is all a live view needs, and no `GapStart` is invented for it), but it IS a thinner
    /// recorded `kind=depth` tape than the in-process feeds wrote, so it is counted here and said
    /// aloud (`CONFLATION_REPORT`) - the in-process recorder counted its dropped rows too.
    pub fn conflated(&self) -> u64 {
        self.shared.conflated.load(Ordering::Relaxed)
    }

    /// How many subscriptions this client holds (granted, or waiting to be replayed).
    pub fn held(&self) -> usize {
        lock(&self.shared.inner)
            .subs
            .values()
            .filter(|e| !matches!(e.phase, Phase::Awaiting | Phase::Refused(_)))
            .count()
    }

    /// How many of the held subscriptions the daemon refused FOR GOOD when this client replayed them
    /// after a link loss: they are not streaming and their gap markers are still open. A diagnostic
    /// (see `DEAD_REMINDER`); `0` on a healthy client.
    pub fn dead(&self) -> usize {
        lock(&self.shared.inner).subs.values().filter(|e| matches!(e.phase, Phase::Dead)).count()
    }

    fn subscribe(&mut self, op: SubOp) -> Result<SubscriptionId, LiveDataError> {
        let sh = Arc::clone(&self.shared);
        if sh.on_reader_thread() {
            return Err(LiveDataError::Subscribe(
                "gatewayhub: subscribe_* was called from inside a sink callback, which runs on the \
                 reader thread that would have to deliver its ack; call it from another thread"
                    .to_string(),
            ));
        }
        let widest = op.symbol().len().max(match &op {
            SubOp::Bars { interval, .. } => interval.len(),
            _ => 0,
        });
        let max_symbol_bytes = sh.opener.limits().max_symbol_bytes;
        if widest > max_symbol_bytes {
            return Err(LiveDataError::Subscribe(format!(
                "gatewayhub: a symbol or interval of {widest} bytes exceeds the \
                 {max_symbol_bytes}-byte framing guard; pass the venue's own spelling"
            )));
        }
        let timings = sh.opener.timings();
        let sid = self.next_sid;
        let mut inner = sh.wait_for_first_dial(lock(&sh.inner));
        match &inner.link {
            LinkState::Up => {}
            LinkState::Connecting { why } => {
                return Err(LiveDataError::Subscribe(format!(
                    "gatewayhub {}: the link is down ({why}); the subscription was not started \
                     and is retried on the caller's next pass",
                    sh.opener.addr()
                )));
            }
            LinkState::Stopped => {
                return Err(LiveDataError::Subscribe(format!(
                    "gatewayhub {}: this client was shut down (or its reader thread ended)",
                    sh.opener.addr()
                )));
            }
        }
        self.next_sid += 1;
        inner.subs.insert(sid, Entry { op: op.clone(), phase: Phase::Awaiting });
        drop(inner);

        if let Err(e) = sh.send(&sh.sub_frame(sid, op)) {
            lock(&sh.inner).subs.remove(&sid);
            return Err(LiveDataError::Subscribe(format!(
                "gatewayhub {}: the subscription could not be sent ({e}); retried on the caller's next pass",
                sh.opener.addr()
            )));
        }

        let deadline = Instant::now() + timings.sub_reply;
        let mut inner = lock(&sh.inner);
        loop {
            // An OWNED summary of the phase, so no borrow of the guard is live when the entry is removed.
            let seen = match inner.subs.get(&sid).map(|e| &e.phase) {
                None => Seen::Vanished,
                // `Replaying` is a subscription GRANTED and then caught by a link loss before this
                // waiter woke: it is held, its gap was disclosed, and the next session replays it. An
                // `Err` here would drop an entry whose `GapStart` then never gets its `Live`.
                Some(Phase::Granted | Phase::Replaying { .. }) => Seen::Granted,
                Some(Phase::Refused(kind)) => Seen::Refused(kind.clone()),
                Some(Phase::Awaiting) => Seen::Awaiting,
                Some(Phase::Dead) => Seen::LinkLost,
            };
            match seen {
                Seen::Granted => return Ok(SubscriptionId(sid)),
                Seen::Refused(kind) => {
                    inner.subs.remove(&sid);
                    return Err(kind.into_live_error());
                }
                Seen::Vanished => {
                    return Err(LiveDataError::Subscribe(
                        "gatewayhub: the subscription vanished before its ack".to_string(),
                    ));
                }
                Seen::LinkLost => {
                    inner.subs.remove(&sid);
                    return Err(LiveDataError::Subscribe(
                        "gatewayhub: the link was lost before the subscription was acknowledged; \
                         retried on the caller's next pass"
                            .to_string(),
                    ));
                }
                Seen::Awaiting => {}
            }
            let now = Instant::now();
            if now >= deadline {
                inner.subs.remove(&sid);
                drop(inner);
                // A tombstone: if the late ack ever comes, the daemon holds nothing for it.
                let _ = sh.send(&StreamUp::Unsub { sid });
                return Err(LiveDataError::Subscribe(format!(
                    "gatewayhub {}: no ack within {:?}; the subscription was abandoned and is \
                     retried on the caller's next pass",
                    sh.opener.addr(),
                    timings.sub_reply
                )));
            }
            inner = sh
                .changed
                .wait_timeout(inner, deadline - now)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }

    /// The body `shutdown` and `Drop` share. Idempotent.
    fn close(&mut self) {
        let Some(handle) = self.manager.take() else { return };
        let sh = Arc::clone(&self.shared);
        {
            let mut inner = lock(&sh.inner);
            inner.stopping = true;
            sh.stop.store(true, Ordering::SeqCst);
            sh.changed.notify_all();
        }
        if sh.on_reader_thread() {
            // Called from inside a sink callback: this thread IS the reader, so it can neither wait
            // for its own exit nor join itself. Raise the stop, break the socket, and let the reader
            // wind down as soon as the callback returns; the handle is dropped (the one case where a
            // thread is not joined, because the alternative is a self-join).
            sh.break_link();
            drop(handle);
            return;
        }
        let _ = sh.send(&StreamUp::Close);
        let deadline = Instant::now() + SHUTDOWN_GRACE;
        let mut inner = lock(&sh.inner);
        while !inner.exited {
            let now = Instant::now();
            if now >= deadline {
                break;
            }
            inner = sh
                .changed
                .wait_timeout(inner, deadline - now)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
        let exited = inner.exited;
        drop(inner);
        if !exited {
            sh.break_link();
        }
        let _ = handle.join();
    }
}

impl RemoteDataClient {
    /// **The CEX tick pump for `symbol`, sized to `tick_size`** ([`SubOp::Ticks`]): the one subscription
    /// that is not a `DataClient` verb. `vike_data::DataClient` has a lane per label and no tick verb
    /// and is a measured crate, so the verb is INHERENT here, and its callers are the strategy lane's
    /// client and the datahub's strat sessions, which hold the concrete type.
    ///
    /// Everything `subscribe_*` promises holds (a client-allocated `sid`, the ack wait, replay under the
    /// original `sid` after a link loss, a `GapStart` for the held key at the instant of loss); the
    /// frames that come back are the pump's `quote`, `trade` and `book` calls and a `stream_status`
    /// whose word is the pump's `"ticks"` link word. A `tick_size` that [`validate_tick_size`] refuses
    /// is an `Err` before anything is framed. A peer that serves no pump answers `Unsupported`, which
    /// is permanent.
    pub fn subscribe_ticks(
        &mut self,
        symbol: &str,
        tick_size: f64,
    ) -> Result<SubscriptionId, LiveDataError> {
        let tick_size = validate_tick_size(tick_size).map_err(|why| {
            LiveDataError::Subscribe(format!(
                "gatewayhub: a tick subscription for `{symbol}`: {why}"
            ))
        })?;
        self.subscribe(SubOp::Ticks { symbol: symbol.to_string(), tick_size })
    }
}

impl DataClient for RemoteDataClient {
    fn subscribe_bars(
        &mut self,
        symbol: &str,
        interval: &str,
    ) -> Result<SubscriptionId, LiveDataError> {
        self.subscribe(SubOp::Bars { symbol: symbol.to_string(), interval: interval.to_string() })
    }

    fn subscribe_quotes(&mut self, symbol: &str) -> Result<SubscriptionId, LiveDataError> {
        self.subscribe(SubOp::Quotes { symbol: symbol.to_string() })
    }

    fn subscribe_trades(&mut self, symbol: &str) -> Result<SubscriptionId, LiveDataError> {
        self.subscribe(SubOp::Trades { symbol: symbol.to_string() })
    }

    fn subscribe_book(&mut self, symbol: &str) -> Result<SubscriptionId, LiveDataError> {
        self.subscribe(SubOp::Book { symbol: symbol.to_string() })
    }

    fn subscribe_depth(&mut self, symbol: &str) -> Result<SubscriptionId, LiveDataError> {
        self.subscribe(SubOp::Depth { symbol: symbol.to_string() })
    }

    /// Unknown ids are a no-op. The `Unsub` is written and the call returns: it waits for no ack. When it
    /// was the symbol's last subscription the reader is told, and drops what it keeps for the symbol's key
    /// (a session binds a fresh key per symbol and never reuses one, so nothing else ever would).
    fn unsubscribe(&mut self, id: SubscriptionId) {
        let removed = {
            let mut inner = lock(&self.shared.inner);
            let removed = inner.subs.remove(&id.0);
            if let Some(entry) = &removed {
                let symbol = entry.op.symbol();
                // The floor was the lane's subscription's: with none left on the lane, a later
                // `set_delivery` + subscribe states it afresh, and none is kept for a symbol nobody holds.
                if let Some(lane) = entry.op.lane()
                    && !inner
                        .subs
                        .values()
                        .any(|e| e.op.symbol() == symbol && e.op.lane() == Some(lane))
                {
                    inner.floors.remove(&(symbol.to_string(), lane));
                }
                if !inner.subs.values().any(|e| e.op.symbol() == symbol) {
                    inner.retired.push(symbol.to_string());
                    self.shared.retire.store(true, Ordering::Release);
                }
            }
            removed
        };
        if removed.is_some() {
            // Best effort: a link that is down holds nothing to stop, and a replayed key is simply
            // not replayed any more.
            let _ = self.shared.send(&StreamUp::Unsub { sid: id.0 });
        }
    }

    /// Remember the floor for `(symbol, lane)` and tell a subscription already open. BEFORE the
    /// subscribe the floor rides the `Sub`; AFTER, a `Delivery` frame (the daemon applies it and tells
    /// this reader where in the frame order the lane changed). The first call on a fresh client waits
    /// for the dial already in flight, exactly as the first `subscribe_*` does and no longer, because it
    /// cannot say whether the daemon serves the class before a session has opened. `Unsupported` only
    /// when a session HAS opened against a daemon without the feature and the floor would raise
    /// something; clearing a floor never fails.
    fn set_delivery(
        &mut self,
        symbol: &str,
        lane: StreamLabel,
        class: Delivery,
    ) -> Result<(), LiveDataError> {
        let sh = Arc::clone(&self.shared);
        let lane = WireLane::from_stream_label(lane);
        let class = WireDelivery::from_delivery(class);
        let mut inner = lock(&sh.inner);
        if !sh.on_reader_thread() {
            inner = sh.wait_for_first_dial(inner);
        }
        if class > WireDelivery::Conflatable && inner.class_support == Some(false) {
            return Err(LiveDataError::Unsupported(NO_STREAM_CLASS));
        }
        let key = (symbol.to_string(), lane);
        if class > WireDelivery::Conflatable {
            inner.floors.insert(key, class);
        } else {
            inner.floors.remove(&key);
        }
        // A replaying subscription picks the floor up from the map when it is sent again; one waiting
        // for its ack or granted is told now.
        let open: Vec<u64> = inner
            .subs
            .iter()
            .filter(|(_, e)| {
                e.op.symbol() == symbol
                    && e.op.lane() == Some(lane)
                    && matches!(e.phase, Phase::Awaiting | Phase::Granted)
            })
            .map(|(sid, _)| *sid)
            .collect();
        drop(inner);
        for sid in open {
            // Best effort: a link that is down is replayed with the floor.
            let _ = sh.send(&StreamUp::Delivery { sid, class });
        }
        Ok(())
    }

    /// `Phase::Dead`: the daemon refused this held subscription for good on replay, so it is not
    /// streaming and its gap stays open. An id the client does not hold is not dead (it is gone).
    fn is_dead(&self, id: SubscriptionId) -> bool {
        matches!(lock(&self.shared.inner).subs.get(&id.0).map(|e| &e.phase), Some(Phase::Dead))
    }

    fn begin_shutdown(&mut self) {
        let _ = self.shared.send(&StreamUp::BeginShutdown);
    }

    fn shutdown(&mut self) {
        self.close();
    }
}

impl Drop for RemoteDataClient {
    fn drop(&mut self) {
        self.close();
    }
}

impl Shared {
    fn stopping(&self) -> bool {
        self.stop.load(Ordering::SeqCst)
    }

    /// Write one frame on the current session's socket. A failed write breaks the socket so the
    /// reader sees the loss at once, and the writer slot is cleared.
    fn send(&self, frame: &StreamUp) -> io::Result<()> {
        let mut writer = lock(&self.writer);
        let Some(stream) = writer.as_mut() else {
            return Err(io::Error::new(io::ErrorKind::NotConnected, "no session is open"));
        };
        match write_up(stream, frame, self.opener.limits().up_frame_ceiling_bytes) {
            Ok(()) => Ok(()),
            // Refused BEFORE its first byte (an over-long frame): the stream is in step and the
            // session is fine.
            Err(e) if e.kind() == io::ErrorKind::InvalidData => Err(e),
            Err(e) => {
                let _ = stream.shutdown(Shutdown::Both);
                *writer = None;
                Err(e)
            }
        }
    }

    /// Wait, once, for the dial already in flight: the FIRST call after construction or after an idle
    /// park (a parked reader is woken to dial). ONE deadline for the dial (`Inner::fresh_until`), not a
    /// wait per call, so a stalled dial costs the broker's mutex this long once. Returns at once when
    /// the link is up, the client is stopping, or no dial is in flight.
    fn wait_for_first_dial<'a>(
        &'a self,
        mut inner: MutexGuard<'a, Inner>,
    ) -> MutexGuard<'a, Inner> {
        if !matches!(inner.link, LinkState::Up) && !inner.stopping {
            if inner.parked {
                inner.parked = false;
                inner.fresh_until =
                    Some(Instant::now() + first_dial_wait(self.opener.as_ref(), &self.venue));
                self.changed.notify_all();
            }
            while let Some(until) = inner.fresh_until
                && !matches!(inner.link, LinkState::Up)
                && !inner.stopping
            {
                let now = Instant::now();
                if now >= until {
                    break;
                }
                inner = self
                    .changed
                    .wait_timeout(inner, until - now)
                    .unwrap_or_else(PoisonError::into_inner)
                    .0;
            }
        }
        inner
    }

    /// The `Sub` frame for `sid`: carrying the floor the broker set for its `(symbol, lane)`, if any,
    /// and if the daemon serves it. The one place a `Sub` is built, so a first send, a replay after a
    /// link loss and a retry of a refused replay all carry the same class.
    fn sub_frame(&self, sid: u64, op: SubOp) -> StreamUp {
        let class = op.lane().and_then(|lane| {
            let inner = lock(&self.inner);
            let floor = inner.floors.get(&(op.symbol().to_string(), lane)).copied()?;
            if inner.class_support == Some(false) {
                tracing::warn!(
                    venue = %self.venue,
                    symbol = op.symbol(),
                    lane = op.stream().word(),
                    "gatewayhub stream: a delivery floor was set but the daemon does not advertise \
                     stream_class; the subscription is sent without it and runs conflated"
                );
                return None;
            }
            Some(floor)
        });
        StreamUp::Sub { sid, op, class }
    }

    /// Whether the calling thread is this session's reader (a sink callback runs on it).
    fn on_reader_thread(&self) -> bool {
        self.reader.get() == Some(&std::thread::current().id())
    }

    /// Break the current socket from outside, so a reader parked in `read` returns.
    fn break_link(&self) {
        if let Some(stream) = lock(&self.writer).as_ref() {
            let _ = stream.shutdown(Shutdown::Both);
        }
        if let Some(stream) = lock(&self.connecting).as_ref() {
            let _ = stream.shutdown(Shutdown::Both);
        }
    }

    fn dial(&self) -> io::Result<DataStream> {
        let observe = |stream: &TcpStream| {
            *lock(&self.connecting) = stream.try_clone().ok();
            // A stop that raced the install: `close` set the flag BEFORE it looked at the slot, so
            // either it saw this clone or this check sees the flag.
            if self.stopping() {
                let _ = stream.shutdown(Shutdown::Both);
            }
        };
        let result = self.opener.open(&self.venue, &observe);
        *lock(&self.connecting) = None;
        result
    }

    fn set_connecting(&self, why: String) {
        let mut inner = lock(&self.inner);
        if !matches!(inner.link, LinkState::Stopped) {
            inner.link = LinkState::Connecting { why };
        }
        inner.fresh_until = None;
        self.changed.notify_all();
    }

    /// Sleep `d`, waking early on a stop.
    fn sleep(&self, d: Duration) {
        let inner = lock(&self.inner);
        let _ = self
            .changed
            .wait_timeout_while(inner, d, |i| !i.stopping)
            .unwrap_or_else(PoisonError::into_inner);
    }

    /// `GapStart` then `Live` for a hole already over: the disclosure of lost frames. `opened_ms` is when
    /// the hole OPENED on the clock the frames carry (never this reader's arrival clock: see the module
    /// doc), and it stamps both: the gap that began there ended with the next frame.
    fn disclose_hole(&self, symbol: &str, stream: &WireStream, opened_ms: i64) {
        self.status(symbol, stream.word(), StreamStatus::GapStart { at_ts_ms: opened_ms });
        self.status(
            symbol,
            stream.word(),
            StreamStatus::Live { gap_started_ts_ms: Some(opened_ms) },
        );
    }

    /// One `stream_status` call into the sink, CONTAINED: a panic in it loses this one notification
    /// (logged at `error`) and never reaches the reader, which has every other key to serve.
    fn status(&self, symbol: &str, word: &str, status: StreamStatus) {
        self.contained("stream_status", symbol, || {
            self.sink.stream_status(&self.venue, symbol, word, status);
        });
    }

    /// Run one sink call under `catch_unwind`; `true` when it panicked (already logged at `error`).
    fn contained(&self, callback: &str, symbol: &str, call: impl FnOnce()) -> bool {
        match catch_unwind(AssertUnwindSafe(call)) {
            Ok(()) => false,
            Err(payload) => {
                tracing::error!(
                    venue = %self.venue,
                    symbol,
                    callback,
                    panic = %panic_text(payload.as_ref()),
                    "gatewayhub stream: a sink callback panicked"
                );
                true
            }
        }
    }
}

/// The text of a panic payload (`&str` and `String` are what `panic!` produces).
fn panic_text(payload: &(dyn Any + Send)) -> String {
    if let Some(text) = payload.downcast_ref::<&str>() {
        (*text).to_string()
    } else if let Some(text) = payload.downcast_ref::<String>() {
        text.clone()
    } else {
        "a non-text panic payload".to_string()
    }
}

/// Why a session ended.
enum End {
    /// The client asked for it.
    Stopped,
    /// The daemon ended an idle session with nothing held; no reconnect until the next subscribe.
    Parked,
    /// The link was lost.
    Lost(String),
}

/// The reader thread: dial, serve, back off, repeat, until stopped. Owns every sink call.
/// Marks the session finished however the reader ends — a clean stop, or a panic unwinding out of the
/// reader itself (a sink panic does not: `guarded` contains it per frame, and `serve` rebuilds the session
/// around a panic in the reader's own frame handling). Without it a dead reader would leave the link `Up`
/// and every `subscribe_*` waiting out its ack.
struct ExitGuard<'a>(&'a Shared);

impl Drop for ExitGuard<'_> {
    fn drop(&mut self) {
        let mut inner = lock(&self.0.inner);
        inner.link = LinkState::Stopped;
        inner.exited = true;
        self.0.changed.notify_all();
    }
}

fn run(sh: &Arc<Shared>) {
    let _ = sh.reader.set(std::thread::current().id());
    let _exit = ExitGuard(sh);
    let t = sh.opener.timings();
    let mut attempt: u32 = 0;
    let mut last_reason = String::new();
    loop {
        {
            let mut inner = lock(&sh.inner);
            while inner.parked && !inner.stopping {
                inner = sh.changed.wait(inner).unwrap_or_else(PoisonError::into_inner);
            }
        }
        if sh.stopping() {
            break;
        }
        let began = Instant::now();
        let end = match sh.dial() {
            Ok(ds) => serve(sh, ds),
            Err(e) => {
                let why = sh.opener.failure_text(&e);
                sh.set_connecting(why.clone());
                End::Lost(why)
            }
        };
        match end {
            End::Stopped => break,
            End::Parked => {
                attempt = 0;
                continue;
            }
            End::Lost(why) => {
                if sh.stopping() {
                    break;
                }
                if began.elapsed() >= HEALTHY_SESSION {
                    attempt = 0;
                }
                let delay = reconnect_delay(t.reconnect_base, t.reconnect_max, attempt);
                // A reason is worth a `warn` when it CHANGES (a refused feature, a missing key and a
                // version skew are configuration faults an operator must see); the same reason on every
                // retry is `debug`, or a down daemon would fill the log.
                if why == last_reason {
                    tracing::debug!(venue = %sh.venue, reason = %why, retry_in = ?delay, "gatewayhub stream: not connected");
                } else {
                    tracing::warn!(venue = %sh.venue, reason = %why, retry_in = ?delay, "gatewayhub stream: not connected");
                    last_reason = why;
                }
                attempt = attempt.saturating_add(1);
                sh.sleep(delay);
            }
        }
    }
}

/// What the reader knows about one key of the CURRENT session.
struct KeyState {
    symbol: String,
    /// The `seq` the next frame of each lossless verb must carry.
    next: HashMap<GapVerb, u64>,
    /// The stamp (epoch-ms) of the last frame of each lossless verb this reader delivered: where a hole
    /// opens when the sequence breaks with no `Gap` before it. A verb whose frames carry no stamp (the
    /// folded `book`) has no entry.
    last_ts: HashMap<GapVerb, i64>,
    /// Per stream word (a bar interval or a lane label): the stamp of the last frame this session
    /// delivered for that lane, seeded with the instant the lane was acknowledged. Where a link loss
    /// opens the lane's gap ([`end_session`]).
    stamps: HashMap<String, i64>,
    /// Lane words whose sink callback panicked: their frames are dropped, quietly, from then on
    /// ([`lane_died`]).
    dead: HashSet<String>,
    /// A callback that no single lane owns (`mark_tick`, `bar_close_tick`) panicked: the whole key is
    /// dead.
    dead_key: bool,
    /// The `seq` the next frame of each CONFLATABLE verb should carry if the daemon superseded none
    /// ([`note_conflated`]).
    cnext: HashMap<Conflatable, u64>,
    /// The lanes the daemon told this reader it raised (`StreamDown::Delivery`), above the verbs' own
    /// class. A raised depth lane is checked for continuity like a `quote` and not counted as conflation.
    raised: HashMap<WireLane, WireDelivery>,
}

/// The four verbs the daemon may conflate: latest-wins per slot, so a `seq` may skip.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Conflatable {
    Forming,
    Mark,
    BarCloseTick,
    Depth,
}

impl KeyState {
    fn new(symbol: String) -> Self {
        Self {
            symbol,
            next: HashMap::new(),
            last_ts: HashMap::new(),
            stamps: HashMap::new(),
            dead: HashSet::new(),
            dead_key: false,
            cnext: HashMap::new(),
            raised: HashMap::new(),
        }
    }

    /// The daemon announced that `lane` runs at `class` from the NEXT frame of the lane on. Frames
    /// before the marker were queued under the old rule, so what the old rule expected of their `seq`
    /// must not be held against the new one: the lane's sequence check restarts here.
    fn set_class(&mut self, lane: WireLane, class: WireDelivery) {
        let was_raised = self.is_raised(lane);
        let raising = class > WireDelivery::Conflatable && !was_raised;
        let relaxing = class <= WireDelivery::Conflatable && was_raised;
        if class > WireDelivery::Conflatable {
            self.raised.insert(lane, class);
        } else {
            self.raised.remove(&lane);
        }
        match lane {
            WireLane::Depth if raising => {
                self.next.remove(&GapVerb::Depth);
                self.last_ts.remove(&GapVerb::Depth);
                self.cnext.remove(&Conflatable::Depth);
            }
            // Relaxed: the next frame is the one after the last the lossless era delivered, so the
            // frames the daemon supersedes from here on are counted from exactly there.
            WireLane::Depth if relaxing => {
                self.last_ts.remove(&GapVerb::Depth);
                match self.next.remove(&GapVerb::Depth) {
                    Some(next) => {
                        self.cnext.insert(Conflatable::Depth, next);
                    }
                    None => {
                        self.cnext.remove(&Conflatable::Depth);
                    }
                }
            }
            WireLane::Depth => {}
            // The verbs of these lanes are lossless whatever a subscriber asks: nothing to restart.
            WireLane::Quotes | WireLane::Trades | WireLane::Book => {}
        }
    }

    /// Whether `lane` is being delivered under the lossless rule because a subscriber raised it.
    fn is_raised(&self, lane: WireLane) -> bool {
        self.raised.get(&lane).is_some_and(|class| *class > WireDelivery::Conflatable)
    }

    /// A frame stamped `ts` was delivered on lane `word`.
    fn stamp(&mut self, word: &str, ts: i64) {
        match self.stamps.get_mut(word) {
            Some(at) => *at = ts,
            None => {
                self.stamps.insert(word.to_string(), ts);
            }
        }
    }

    /// The stamp a gap of lane `word` opens at on this key: the lane's own, and for the pump's link
    /// word ([`LINK_STREAM_WORD`]) the newest of ALL the key's lanes, because a [`SubOp::Ticks`]
    /// subscription's frames are quotes, trades and books, which stamp their own words
    /// ([`lane_gap_stamp`]).
    fn gap_stamp(&self, word: &str) -> Option<i64> {
        if word == LINK_STREAM_WORD {
            // The link word's OWN entry is only the instant the pump was acknowledged; the lanes'
            // entries are what its frames stamped. The newest of those, else the acknowledgement.
            let delivered = self
                .stamps
                .iter()
                .filter(|(w, _)| w.as_str() != LINK_STREAM_WORD)
                .map(|(_, at)| *at)
                .max();
            return delivered.or_else(|| self.stamps.get(word).copied());
        }
        self.stamps.get(word).copied()
    }

    /// The daemon acknowledged a subscription on lane `word`: the lane starts (again) from now, and a
    /// death it had is over.
    fn granted(&mut self, word: &str) {
        self.dead.remove(word);
        self.dead_key = false;
        if !self.stamps.contains_key(word) {
            self.stamps.insert(word.to_string(), now_ms());
        }
    }

    /// Whether frames for `lane` (`None`: a frame no lane owns) are being dropped.
    fn is_dead(&self, lane: Option<&str>) -> bool {
        self.dead_key || lane.is_some_and(|word| !self.dead.is_empty() && self.dead.contains(word))
    }
}

/// How many unsubscribed keys the reader remembers, only so that a frame still in flight for one is
/// dropped quietly instead of being counted as a stray. A bound, not a policy: past it a frame that late
/// is a stray indeed.
const RETIRED_KEYS: usize = 64;

/// The reader's per-session state; a reconnect starts it afresh, because key ids belong to the session.
#[derive(Default)]
struct Reader {
    keys: HashMap<u32, KeyState>,
    /// Frames for a key the session never bound (counted, dropped, warned once).
    unknown: u64,
    /// `(symbol, stream word)` pairs whose reopened gap this session already closed: dedup for several
    /// replayed subscriptions of one symbol and lane. Needed only while a replay is outstanding, so it
    /// is emptied once none is.
    live_sent: HashSet<(String, String)>,
    /// Keys dropped because their symbol was unsubscribed, oldest first, at most [`RETIRED_KEYS`].
    retired: VecDeque<u32>,
    /// Replays the daemon refused TRANSIENTLY, and when each is sent again (`sid` to due time).
    retry: BTreeMap<u64, Instant>,
    /// When this session opened (epoch-ms): the stamp of a hole in a lane that never delivered or was
    /// never acknowledged here, the only instant a loss can honestly be said to have begun after.
    opened_ms: i64,
    /// When a [`Phase::Dead`] subscription was last said aloud (`None`: none this session).
    reminded: Option<Instant>,
    /// The `RemoteDataClient::conflated` total already said aloud, and when ([`report_conflated`]).
    conflated_said: u64,
    conflated_at: Option<Instant>,
}

impl Reader {
    /// The earliest due retry, if any replay is waiting on one.
    fn next_retry(&self) -> Option<Instant> {
        self.retry.values().min().copied()
    }

    /// Publish the key count for `RemoteDataClient::bound_keys`.
    fn publish(&self, sh: &Shared) {
        sh.bound_keys.store(self.keys.len(), Ordering::Relaxed);
    }
}

/// One session: replay, go Up, read until it ends.
fn serve(sh: &Arc<Shared>, ds: DataStream) -> End {
    let DataStream { stream, heartbeat, delivery_class, .. } = ds;
    let t = sh.opener.timings();
    let deadline = stream_deadline(t.stream_read_floor, heartbeat);
    let writer = match stream.try_clone() {
        Ok(w) => w,
        Err(e) => {
            let why = sh.opener.failure_text(&e);
            sh.set_connecting(why.clone());
            return End::Lost(why);
        }
    };
    if let Err(e) = stream.set_read_timeout(Some(deadline)) {
        let why = sh.opener.failure_text(&e);
        sh.set_connecting(why.clone());
        return End::Lost(why);
    }
    *lock(&sh.writer) = Some(writer);
    lock(&sh.inner).class_support = Some(delivery_class);
    let mut state = Reader {
        opened_ms: now_ms(),
        conflated_said: sh.conflated.load(Ordering::Relaxed),
        ..Reader::default()
    };

    // Replay the held set under its ORIGINAL sids; the daemon dedups on them.
    let replay: Vec<(u64, SubOp)> = lock(&sh.inner)
        .subs
        .iter()
        .filter(|(_, e)| matches!(e.phase, Phase::Replaying { .. }))
        .map(|(sid, e)| (*sid, e.op.clone()))
        .collect();
    for (sid, op) in replay {
        // A key unsubscribed since the list was taken is not replayed (and a `SubOk` for one that slips
        // through is answered with an `Unsub`, below).
        let still_held = matches!(
            lock(&sh.inner).subs.get(&sid).map(|e| &e.phase),
            Some(Phase::Replaying { .. })
        );
        if !still_held {
            continue;
        }
        if let Err(e) = sh.send(&sh.sub_frame(sid, op)) {
            let why = sh.opener.failure_text(&e);
            return end_session(sh, &state, End::Lost(why));
        }
    }
    {
        let mut inner = lock(&sh.inner);
        if inner.stopping {
            drop(inner);
            return end_session(sh, &state, End::Stopped);
        }
        inner.link = LinkState::Up;
        inner.fresh_until = None;
        sh.changed.notify_all();
    }

    // The reads are buffered: a frame is a 4-byte prefix and a body, and an unbuffered socket paid two
    // `recv` syscalls for every frame. The timeout, the `peek` and the shutdown stay on the socket
    // itself (`get_ref`).
    let mut reader_stream = io::BufReader::with_capacity(READ_BUFFER_BYTES, stream);
    let mut last_frame = Instant::now();
    let end = loop {
        // A replay the daemon refused transiently is sent again once its backoff has passed; while one
        // is waiting the reader sleeps on a `peek` that wakes for it (never on a timed-out frame read,
        // which could tear a frame), otherwise it blocks in the read exactly as before.
        if let Some(end) = send_due_retries(sh, &mut state) {
            break end;
        }
        // Bytes the buffer already holds are a frame (or the start of one) that the socket's `peek` cannot
        // see: with any, the reader reads on instead of waiting.
        if let Some(at) = state.next_retry()
            && reader_stream.buffer().is_empty()
        {
            match wait_for_frame(reader_stream.get_ref(), last_frame, deadline, at) {
                Ok(Waited::Frame) => {
                    if let Err(e) = reader_stream.get_ref().set_read_timeout(Some(deadline)) {
                        break End::Lost(sh.opener.failure_text(&e));
                    }
                }
                Ok(Waited::RetryDue) => {
                    // The wait left its short timeout armed, and with no other retry pending the next
                    // read blocks under it: put back what is left of the silence deadline.
                    let left = deadline.saturating_sub(last_frame.elapsed());
                    let left = left.max(Duration::from_millis(1));
                    if let Err(e) = reader_stream.get_ref().set_read_timeout(Some(left)) {
                        break End::Lost(sh.opener.failure_text(&e));
                    }
                    continue;
                }
                Ok(Waited::Silent) => break End::Lost(silent_text(sh, deadline)),
                Err(e) if sh.stopping() => {
                    tracing::debug!(venue = %sh.venue, error = %e, "gatewayhub stream: closed");
                    break End::Stopped;
                }
                Err(e) => break End::Lost(sh.opener.failure_text(&e)),
            }
        }
        match read_capped::<_, StreamDown>(
            &mut reader_stream,
            sh.opener.limits().down_frame_ceiling_bytes,
        ) {
            Ok(frame) => {
                last_frame = Instant::now();
                if sh.retire.load(Ordering::Acquire) {
                    trim_retired(sh, &mut state);
                }
                // A panic in a sink callback is contained per lane inside `deliver` (`guarded`). One
                // that gets out of it is this client's own bug in its frame handling: the session is
                // rebuilt (every held key disclosed gapped, replayed on the new session) rather than
                // the reader unwound, which would end the venue's stream for good.
                match catch_unwind(AssertUnwindSafe(|| deliver(sh, &mut state, frame))) {
                    Ok(Some(end)) => break end,
                    Ok(None) => {}
                    Err(payload) => {
                        let why = format!(
                            "gatewayhub {}: handling a frame panicked ({}); the session is rebuilt",
                            sh.opener.addr(),
                            panic_text(payload.as_ref())
                        );
                        tracing::error!(venue = %sh.venue, "{why}");
                        break End::Lost(why);
                    }
                }
                if state.reminded.is_some() {
                    remind_dead(sh, &mut state, last_frame);
                }
                if sh.conflated.load(Ordering::Relaxed) != state.conflated_said {
                    report_conflated(sh, &mut state, last_frame);
                }
            }
            Err(e) if sh.stopping() => {
                tracing::debug!(venue = %sh.venue, error = %e, "gatewayhub stream: closed");
                break End::Stopped;
            }
            Err(e) => {
                let why = if is_timeout(&e) {
                    silent_text(sh, deadline)
                } else {
                    sh.opener.failure_text(&e)
                };
                break End::Lost(why);
            }
        }
    };
    end_session(sh, &state, end)
}

/// The reader's read-ahead: one `recv` takes every frame the socket holds, up to this much.
const READ_BUFFER_BYTES: usize = 64 * 1024;

/// Why a silent link is called lost.
fn silent_text(sh: &Shared, deadline: Duration) -> String {
    format!(
        "gatewayhub {}: no frame for {deadline:?} — the link is dead or the daemon is wedged",
        sh.opener.addr()
    )
}

/// What [`wait_for_frame`] found.
enum Waited {
    /// A frame has begun (or the peer closed: the read that follows reports it).
    Frame,
    /// Nothing yet, and a retry is due.
    RetryDue,
    /// Nothing for the whole heartbeat deadline.
    Silent,
}

/// Wait until a frame begins, a retry is due at `retry_at`, or `deadline` has passed since
/// `since` (the previous frame), whichever is first. A `peek`: no byte is consumed, so a timeout
/// can never leave the stream half-read.
fn wait_for_frame(
    stream: &TcpStream,
    since: Instant,
    deadline: Duration,
    retry_at: Instant,
) -> io::Result<Waited> {
    loop {
        let waited = since.elapsed();
        if waited >= deadline {
            return Ok(Waited::Silent);
        }
        let until_retry = retry_at.saturating_duration_since(Instant::now());
        if until_retry.is_zero() {
            return Ok(Waited::RetryDue);
        }
        let wait = (deadline - waited).min(until_retry).max(Duration::from_millis(1));
        stream.set_read_timeout(Some(wait))?;
        let mut probe = [0u8; 1];
        match stream.peek(&mut probe) {
            Ok(_) => return Ok(Waited::Frame),
            Err(e) if is_timeout(&e) || e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
}

/// Send again every transiently refused replay whose backoff has passed. `Some(end)` when the write
/// failed and the session is over. A subscription unsubscribed (or no longer replaying) since is
/// skipped; the retry is dropped either way.
fn send_due_retries(sh: &Arc<Shared>, state: &mut Reader) -> Option<End> {
    if state.retry.is_empty() {
        return None;
    }
    let now = Instant::now();
    let due: Vec<u64> =
        state.retry.iter().filter(|(_, at)| **at <= now).map(|(sid, _)| *sid).collect();
    for sid in due {
        state.retry.remove(&sid);
        let op = {
            let inner = lock(&sh.inner);
            match inner.subs.get(&sid) {
                Some(Entry { op, phase: Phase::Replaying { .. } }) => Some(op.clone()),
                _ => None,
            }
        };
        let Some(op) = op else { continue };
        if let Err(e) = sh.send(&sh.sub_frame(sid, op)) {
            return Some(End::Lost(sh.opener.failure_text(&e)));
        }
    }
    None
}

/// Drop what the reader keeps for the symbols whose last subscription was unsubscribed: their keys
/// (remembered for a while as `retired`, so frames still in flight are dropped quietly) and their
/// `live_sent` entries. A symbol subscribed again since keeps its key.
fn trim_retired(sh: &Shared, state: &mut Reader) {
    let symbols: Vec<String> = {
        let mut inner = lock(&sh.inner);
        sh.retire.store(false, Ordering::Release);
        let retired = std::mem::take(&mut inner.retired);
        retired
            .into_iter()
            .filter(|s| !inner.subs.values().any(|e| e.op.symbol() == s.as_str()))
            .collect()
    };
    for symbol in symbols {
        let ids: Vec<u32> =
            state.keys.iter().filter(|(_, key)| key.symbol == symbol).map(|(k, _)| *k).collect();
        for k in ids {
            state.keys.remove(&k);
            state.retired.push_back(k);
            if state.retired.len() > RETIRED_KEYS {
                state.retired.pop_front();
            }
        }
        state.live_sent.retain(|(s, _)| *s != symbol);
    }
    state.publish(sh);
}

/// The end of a session: clear the writer, and unless it was a deliberate stop DISCLOSE the loss —
/// `GapStart` for every held key, stamped with WHEN THE GAP OPENED (the last frame this session
/// delivered for the key's lane, see [`lane_gap_stamp`]; never the instant the loss was noticed) —
/// before the reader backs off. `state` is the session's reader state, the only place those stamps
/// live.
fn end_session(sh: &Arc<Shared>, state: &Reader, end: End) -> End {
    if let Some(stream) = lock(&sh.writer).take() {
        let _ = stream.shutdown(Shutdown::Both);
    }
    if sh.stopping() || matches!(end, End::Stopped) {
        let mut inner = lock(&sh.inner);
        inner.link = LinkState::Stopped;
        sh.changed.notify_all();
        return End::Stopped;
    }
    let mut gapped: Vec<(String, String, i64)> = Vec::new();
    let idle_and_empty;
    {
        let mut inner = lock(&sh.inner);
        for entry in inner.subs.values_mut() {
            if matches!(entry.phase, Phase::Granted) {
                let stream = entry.op.stream();
                let (symbol, word) = (entry.op.symbol(), stream.word());
                let at = lane_gap_stamp(state, symbol, word);
                if !gapped.iter().any(|(s, w, _)| s == symbol && w == word) {
                    gapped.push((symbol.to_string(), word.to_string(), at));
                }
                entry.phase = Phase::Replaying { gap_at: at, tries: 0 };
            } else if matches!(entry.phase, Phase::Awaiting) {
                entry.phase = Phase::Refused(SubErrKind::Subscribe(
                    "gatewayhub: the link was lost before the subscription was acknowledged; \
                     retried on the caller's next pass"
                        .to_string(),
                ));
            }
            // Refused (the waiter has not woken yet), Replaying (a second loss before the replay was
            // acknowledged: the FIRST gap is still the open one) and Dead stay as they are.
        }
        idle_and_empty = matches!(end, End::Parked)
            && inner.subs.values().all(|e| matches!(e.phase, Phase::Dead | Phase::Refused(_)));
        let why = match &end {
            End::Lost(why) => why.clone(),
            End::Parked => "the daemon closed an idle session".to_string(),
            End::Stopped => String::new(),
        };
        inner.link = LinkState::Connecting { why };
        inner.fresh_until = None;
        inner.parked = idle_and_empty;
        sh.changed.notify_all();
    }
    for (symbol, word, at) in &gapped {
        sh.status(symbol, word, StreamStatus::GapStart { at_ts_ms: *at });
    }
    if !gapped.is_empty() {
        tracing::warn!(
            venue = %sh.venue,
            keys = gapped.len(),
            "gatewayhub stream: link lost; GapStart disclosed for every held key, replay on reconnect"
        );
    }
    if idle_and_empty {
        End::Parked
    } else if let End::Lost(why) = end {
        End::Lost(why)
    } else {
        // An idle Bye with subscriptions still held is a loss like any other.
        End::Lost("the daemon closed the session".to_string())
    }
}

/// Where a link loss opens the gap of `(symbol, word)`: the stamp of the last frame this session
/// delivered on that lane, else the instant the lane was acknowledged (both live in the key's
/// `stamps`), else the instant the session opened. NEVER the instant the loss was noticed: rows the
/// daemon produced after the last delivered frame and never delivered (its queue tail when it ended the
/// session as too slow, or the whole silence a heartbeat deadline waits out) lie after this stamp and
/// before the detection, and a marker stamped at detection would leave them outside the gap.
///
/// ⚠ **The pump's link word owns every lane of the key.** A [`SubOp::Ticks`] subscription is disclosed
/// under `"ticks"`, but its frames are quotes, trades and books, so they stamp THEIR lanes' words and
/// the `"ticks"` entry only ever holds the instant it was acknowledged: reading just that would open
/// every pump gap at the ack, however long the pump had been delivering. For the link word the stamp
/// is the newest over all the key's lanes.
fn lane_gap_stamp(state: &Reader, symbol: &str, word: &str) -> i64 {
    state
        .keys
        .values()
        .filter(|key| key.symbol == symbol)
        .filter_map(|key| key.gap_stamp(word))
        .max()
        .unwrap_or(state.opened_ms)
}

/// Say aloud, at `error`, every held subscription the daemon refused for good on replay ([`Phase::Dead`])
/// once [`DEAD_REMINDER`] has passed since the last time; returns how many were said (`0`: nothing was
/// due, or none is held). The broker believes these live and never asks again, and the recorder's
/// subscription set the same: their open gap markers are what a consumer sees, and this is what an
/// operator reads.
fn remind_dead(sh: &Shared, state: &mut Reader, now: Instant) -> usize {
    let Some(last) = state.reminded else { return 0 };
    if now.saturating_duration_since(last) < DEAD_REMINDER {
        return 0;
    }
    state.reminded = Some(now);
    let dead: Vec<String> = lock(&sh.inner)
        .subs
        .values()
        .filter(|e| matches!(e.phase, Phase::Dead))
        .map(|e| format!("{}/{}", e.op.symbol(), e.op.stream().word()))
        .collect();
    if !dead.is_empty() {
        tracing::error!(
            venue = %sh.venue,
            count = dead.len(),
            subscriptions = ?dead.iter().take(8).collect::<Vec<_>>(),
            "gatewayhub stream: held subscriptions the daemon refused for good are NOT streaming, \
             their gaps are still open, and nothing will ask again; fix the daemon, then restart \
             the consumer"
        );
    }
    dead.len()
}

/// A conflatable frame's `seq` against what its `(key, verb)` expects next. The daemon numbers EVERY
/// frame it pushes before it decides what to drop, so a jump is frames it superseded (latest-wins)
/// because its writer had not sent them yet - this reader was behind. By the sink's contract that is not
/// a loss and it is NOT disclosed to the sink as a gap (a live view must not flash `GapStart` for a
/// latest-wins skip); it is counted ([`RemoteDataClient::conflated`]) and said aloud, throttled
/// ([`report_conflated`]), because a RECORDED `kind=depth` tape is thinner for it and the in-process
/// recorder counted every row it dropped.
fn note_conflated(sh: &Shared, key: &mut KeyState, verb: Conflatable, seq: u64) {
    let expected = key.cnext.insert(verb, seq.wrapping_add(1)).unwrap_or(1);
    if seq > expected {
        sh.conflated.fetch_add(seq - expected, Ordering::Relaxed);
    }
}

/// Say, at `warn` and at most once per [`CONFLATION_REPORT`], how many conflatable frames the daemon has
/// superseded since the last time; returns how many were said (`0`: nothing new, or said too recently).
fn report_conflated(sh: &Shared, state: &mut Reader, now: Instant) -> u64 {
    let total = sh.conflated.load(Ordering::Relaxed);
    let new = total.saturating_sub(state.conflated_said);
    if new == 0 {
        return 0;
    }
    if state.conflated_at.is_some_and(|at| now.saturating_duration_since(at) < CONFLATION_REPORT) {
        return 0;
    }
    state.conflated_said = total;
    state.conflated_at = Some(now);
    tracing::warn!(
        venue = %sh.venue,
        superseded = new,
        total,
        "gatewayhub stream: the daemon superseded conflatable frames (depth / forming / mark) because \
         this reader fell behind; a live view loses nothing, but a recorded depth tape is thinner for \
         the lag"
    );
    new
}

/// Run one DATA sink call (`call`, which may also check the frame's `seq`) for `key`'s `lane` - `None`
/// for a callback no single lane owns - and CONTAIN a panic in it: that one `(key, lane)` ends
/// ([`lane_died`]) and nothing else does. A lane that already died drops the frame without running it.
fn guarded(sh: &Shared, key: &mut KeyState, lane: Option<&str>, call: impl FnOnce(&mut KeyState)) {
    if key.is_dead(lane) {
        return;
    }
    if let Err(payload) = catch_unwind(AssertUnwindSafe(|| call(&mut *key))) {
        lane_died(sh, key, lane, &panic_text(payload.as_ref()));
    }
}

/// The sink panicked delivering a frame of `key`'s `lane`: end THAT subscription and keep the session.
/// Logged ONCE at `error`; the lane's frames are dropped from here on; its held subscription is
/// forgotten (a link loss will not replay what is dead) and the daemon told to stop streaming it; and a
/// `GapStart` is disclosed for it, stamped where its last frame left off, so a consumer's status shows
/// the lane down rather than `Live` forever. The symbol's key is given back when it was the last
/// subscription on it.
fn lane_died(sh: &Shared, key: &mut KeyState, lane: Option<&str>, why: &str) {
    match lane {
        Some(word) => {
            key.dead.insert(word.to_string());
        }
        None => key.dead_key = true,
    }
    let ended: Vec<(u64, String)> = {
        let mut inner = lock(&sh.inner);
        // A tick PUMP subscription ([`SubOp::Ticks`]) owns every lane of its key, not only the one its
        // link word names: its frames are quotes, trades and books, so a panic in any of those ends
        // the whole pump subscription (disclosed under its own `"ticks"` word), never leaves it held
        // and streaming into a lane that now drops everything.
        let sids: Vec<(u64, String)> = inner
            .subs
            .iter()
            .filter(|(_, e)| e.op.symbol() == key.symbol && matches!(e.phase, Phase::Granted))
            .filter(|(_, e)| {
                matches!(e.op, SubOp::Ticks { .. })
                    || lane.is_none_or(|l| l == e.op.stream().word())
            })
            .map(|(sid, e)| (*sid, e.op.stream().word().to_string()))
            .collect();
        for (sid, _) in &sids {
            inner.subs.remove(sid);
        }
        if !sids.is_empty() && !inner.subs.values().any(|e| e.op.symbol() == key.symbol) {
            inner.retired.push(key.symbol.clone());
            sh.retire.store(true, Ordering::Release);
        }
        sids
    };
    tracing::error!(
        venue = %sh.venue,
        symbol = %key.symbol,
        lane = lane.unwrap_or("(every lane of the key)"),
        subscriptions = ended.len(),
        panic = %why,
        "gatewayhub stream: a sink callback panicked; this subscription is ended with a GapStart, \
         the session and every other key keep running"
    );
    let mut told = HashSet::new();
    for (sid, word) in &ended {
        let _ = sh.send(&StreamUp::Unsub { sid: *sid });
        if told.insert(word.as_str()) {
            let at = key.gap_stamp(word).unwrap_or_else(now_ms);
            sh.status(&key.symbol, word, StreamStatus::GapStart { at_ts_ms: at });
        }
    }
}

/// The lane a control frame's `stream` word names for [`KeyState::is_dead`]: a bar interval or a lane
/// label; the CEX pump's link word belongs to no lane.
fn lane_of(stream: &WireStream) -> Option<&str> {
    match stream {
        WireStream::Link => None,
        other => Some(other.word()),
    }
}

/// The key of `k`, or count it as unknown (warning once per session).
fn key_of<'a>(sh: &Shared, state: &'a mut Reader, k: u32) -> Option<&'a mut KeyState> {
    if state.keys.contains_key(&k) {
        return state.keys.get_mut(&k);
    }
    if state.retired.contains(&k) {
        // In flight for a key this client dropped on purpose (its symbol was unsubscribed).
        return None;
    }
    state.unknown += 1;
    if state.unknown == 1 {
        tracing::warn!(
            venue = %sh.venue,
            k,
            "gatewayhub stream: a frame names a key this session never bound; dropped (counted)"
        );
    }
    None
}

/// Check one lossless frame's `seq` against what the key expects. A mismatch with no `Gap` before it
/// is a silent loss: disclose it, stamped with the last frame this reader delivered for the
/// `(key, verb)` (`ts` is this frame's own stamp, `None` for a verb that carries none). The hole opens
/// there, never at the arrival clock; with no stamp to go on (`book`) the arrival clock is all there is.
fn continuity(
    sh: &Shared,
    key: &mut KeyState,
    verb: GapVerb,
    seq: u64,
    stream: &WireStream,
    ts: Option<i64>,
) {
    let previous = key.next.insert(verb, seq.wrapping_add(1));
    let last = key.last_ts.get(&verb).copied();
    if let Some(ts) = ts {
        key.last_ts.insert(verb, ts);
        key.stamp(stream.word(), ts);
    }
    if let Some(expected) = previous
        && expected != seq
    {
        tracing::warn!(
            venue = %sh.venue,
            symbol = %key.symbol,
            ?verb,
            expected,
            got = seq,
            "gatewayhub stream: a lossless verb's sequence broke with no Gap; disclosed as a hole"
        );
        sh.disclose_hole(&key.symbol, stream, last.or(ts).unwrap_or_else(now_ms));
    }
}

/// Deliver one frame INLINE; `Some(end)` when the session is over.
fn deliver(sh: &Arc<Shared>, state: &mut Reader, frame: StreamDown) -> Option<End> {
    match frame {
        StreamDown::SubOk { sid, k } => {
            let mut gap = None;
            let (granted, replaying_left) = {
                let mut inner = lock(&sh.inner);
                let granted = inner.subs.get_mut(&sid).map(|entry| {
                    if let Phase::Replaying { gap_at, .. } =
                        std::mem::replace(&mut entry.phase, Phase::Granted)
                    {
                        gap = Some(gap_at);
                    }
                    (entry.op.symbol().to_string(), entry.op.stream())
                });
                if let Some((symbol, stream)) = &granted {
                    // The key is bound while the entry is marked granted, in one critical section: a
                    // `subscribe_*` woken by this ack finds its key bound (and its lane stamped), and a
                    // callback that panics on the `Live` below cannot leave frames for it unclaimed.
                    //
                    // A key id the daemon reuses for ANOTHER symbol starts a fresh sequence: carrying
                    // the old symbol's expectations over would read its first frames as holes.
                    let rebound = state.keys.get(&k).is_some_and(|key| key.symbol != *symbol);
                    if rebound || !state.keys.contains_key(&k) {
                        state.keys.insert(k, KeyState::new(symbol.clone()));
                    }
                    if let Some(key) = state.keys.get_mut(&k) {
                        key.granted(stream.word());
                    }
                }
                let replaying_left =
                    inner.subs.values().any(|e| matches!(e.phase, Phase::Replaying { .. }));
                (granted, replaying_left)
            };
            state.retry.remove(&sid);
            sh.changed.notify_all();
            if let Some((symbol, stream)) = granted {
                if let Some(gap_at) = gap
                    && state.live_sent.insert((symbol.clone(), stream.word().to_string()))
                {
                    sh.status(
                        &symbol,
                        stream.word(),
                        StreamStatus::Live { gap_started_ts_ms: Some(gap_at) },
                    );
                }
                // The dedup set is only needed while some replay is still outstanding.
                if !replaying_left {
                    state.live_sent.clear();
                }
                state.publish(sh);
            } else {
                // An ack for a subscription this client no longer holds (it was abandoned after a
                // timeout, or unsubscribed while its replay was in flight): stop what the daemon just
                // started, or it streams into a key nobody reads.
                let _ = sh.send(&StreamUp::Unsub { sid });
            }
        }
        StreamDown::SubErr { sid, err } => {
            let t = sh.opener.timings();
            let mut retry_in = None;
            let mut died = false;
            {
                let mut inner = lock(&sh.inner);
                if let Some(entry) = inner.subs.get_mut(&sid) {
                    if matches!(entry.phase, Phase::Awaiting) {
                        entry.phase = Phase::Refused(err);
                    } else if let Phase::Replaying { gap_at, tries } = entry.phase {
                        if err.is_permanent() {
                            entry.phase = Phase::Dead;
                            died = true;
                            // `error`, and said again every `DEAD_REMINDER`: the broker believes this
                            // subscription live and nothing will ever ask for it again.
                            let lane = entry.op.stream();
                            tracing::error!(
                                venue = %sh.venue,
                                sid,
                                symbol = entry.op.symbol(),
                                lane = lane.word(),
                                ?err,
                                "gatewayhub stream: the daemon refused a held subscription on replay \
                                 for good; it is NOT streaming and its gap stays open"
                            );
                        } else {
                            // The broker believes this subscription is live and will not ask again, so
                            // it stays held and is re-sent at the reconnect backoff.
                            let delay = reconnect_delay(t.reconnect_base, t.reconnect_max, tries);
                            entry.phase =
                                Phase::Replaying { gap_at, tries: tries.saturating_add(1) };
                            retry_in = Some(delay);
                            if tries == 0 {
                                tracing::warn!(
                                    venue = %sh.venue,
                                    sid,
                                    ?err,
                                    retry_in = ?delay,
                                    "gatewayhub stream: the daemon refused a held subscription on \
                                     replay; it stays held and is retried"
                                );
                            } else {
                                tracing::debug!(
                                    venue = %sh.venue,
                                    sid,
                                    ?err,
                                    retry_in = ?delay,
                                    "gatewayhub stream: a held subscription is still refused; retrying"
                                );
                            }
                        }
                    }
                }
            }
            if let Some(delay) = retry_in {
                state.retry.insert(sid, Instant::now() + delay);
            }
            if died {
                state.reminded = Some(Instant::now());
            }
            sh.changed.notify_all();
        }
        StreamDown::Key { k, symbol } => match state.keys.get(&k) {
            Some(existing) if existing.symbol != symbol => {
                return Some(End::Lost(format!(
                    "gatewayhub {}: protocol fault — key {k} was bound to a second symbol",
                    sh.opener.addr()
                )));
            }
            Some(_) => {}
            None => {
                state.keys.insert(k, KeyState::new(symbol));
                state.publish(sh);
            }
        },
        // Every DATA callback below runs under `guarded`: a panic in the sink ends that one
        // `(key, lane)` and nothing else. The lane is the frame's own (a bar interval, a lane label);
        // `mark_tick` and `bar_close_tick` name none, so a panic there ends the whole key.
        StreamDown::Seed { k, seq, interval, bars } => {
            if let Some(key) = key_of(sh, state, k) {
                let stream = WireStream::Bar(interval);
                guarded(sh, key, Some(stream.word()), |key| {
                    let ts = bars.last().map(|b| b.ts);
                    continuity(sh, key, GapVerb::Seed, seq, &stream, ts);
                    sh.sink.seed_bars(&sh.venue, &key.symbol, stream.word(), bars);
                });
            }
        }
        StreamDown::BarClose { k, seq, interval, bar } => {
            if let Some(key) = key_of(sh, state, k) {
                let stream = WireStream::Bar(interval);
                guarded(sh, key, Some(stream.word()), |key| {
                    continuity(sh, key, GapVerb::BarClose, seq, &stream, Some(bar.ts));
                    sh.sink.close_bar(&sh.venue, &key.symbol, stream.word(), bar);
                });
            }
        }
        // The four CONFLATABLE verbs may skip a `seq`, which is the daemon superseding a frame its
        // writer had not sent yet (`note_conflated`): counted, never a gap.
        StreamDown::Forming { k, seq, interval, bar } => {
            if let Some(key) = key_of(sh, state, k) {
                guarded(sh, key, Some(&interval), |key| {
                    note_conflated(sh, key, Conflatable::Forming, seq);
                    sh.sink.forming_bar(&sh.venue, &key.symbol, &interval, bar);
                });
            }
        }
        StreamDown::Mark { k, seq, px, ts } => {
            if let Some(key) = key_of(sh, state, k) {
                guarded(sh, key, None, |key| {
                    note_conflated(sh, key, Conflatable::Mark, seq);
                    sh.sink.mark_tick(&sh.venue, &key.symbol, px, ts);
                });
            }
        }
        StreamDown::BarCloseTick { k, seq, px, ts } => {
            if let Some(key) = key_of(sh, state, k) {
                guarded(sh, key, None, |key| {
                    note_conflated(sh, key, Conflatable::BarCloseTick, seq);
                    sh.sink.bar_close_tick(&sh.venue, &key.symbol, px, ts);
                });
            }
        }
        StreamDown::Depth { k, seq, tick_size, bids, asks, ts } => {
            if let Some(key) = key_of(sh, state, k) {
                // Its own frames' stamps are what a loss opens the lane's gap at.
                let stream = WireStream::Lane(WireLane::Depth);
                guarded(sh, key, Some(stream.word()), |key| {
                    if key.is_raised(WireLane::Depth) {
                        // A raised lane is lossless: a jump in `seq` with no `Gap` before it is a
                        // silent loss and is disclosed as one, like a quote's.
                        continuity(sh, key, GapVerb::Depth, seq, &stream, Some(ts));
                    } else {
                        note_conflated(sh, key, Conflatable::Depth, seq);
                        key.stamp(stream.word(), ts);
                    }
                    sh.sink.l2_snapshot(&sh.venue, &key.symbol, tick_size, bids, asks, ts);
                });
            }
        }
        StreamDown::Quote { k, seq, quote } => {
            if let Some(key) = key_of(sh, state, k) {
                let stream = WireStream::Lane(WireLane::Quotes);
                guarded(sh, key, Some(stream.word()), |key| {
                    continuity(sh, key, GapVerb::Quote, seq, &stream, Some(quote.ts));
                    sh.sink.quote(&sh.venue, &key.symbol, quote);
                });
            }
        }
        StreamDown::Trade { k, seq, trade } => {
            if let Some(key) = key_of(sh, state, k) {
                let stream = WireStream::Lane(WireLane::Trades);
                guarded(sh, key, Some(stream.word()), |key| {
                    continuity(sh, key, GapVerb::Trade, seq, &stream, Some(trade.ts));
                    sh.sink.trade(&sh.venue, &key.symbol, trade);
                });
            }
        }
        StreamDown::Book { k, seq, book } => {
            if let Some(key) = key_of(sh, state, k) {
                // A folded book carries no time of its own: the arrival clock is all a hole in it has.
                let stream = WireStream::Lane(WireLane::Book);
                guarded(sh, key, Some(stream.word()), |key| {
                    continuity(sh, key, GapVerb::Book, seq, &stream, None);
                    sh.sink.book(&sh.venue, &key.symbol, Arc::new(*book));
                });
            }
        }
        StreamDown::BookDelta { k, seq, update } => {
            if let Some(key) = key_of(sh, state, k) {
                let stream = WireStream::Lane(WireLane::Book);
                guarded(sh, key, Some(stream.word()), |key| {
                    continuity(sh, key, GapVerb::BookDelta, seq, &stream, Some(update.ts));
                    sh.sink.book_update(&sh.venue, &key.symbol, *update);
                });
            }
        }
        // The control frames carry no data, so a panic in their callbacks loses the notification only
        // (`Shared::status`, `Shared::contained`); a lane that already died hears nothing more, or a
        // later `Live` from the daemon would undo the `GapStart` its death disclosed.
        StreamDown::Status { k, stream, status } => {
            if let Some(key) = key_of(sh, state, k)
                && !key.is_dead(lane_of(&stream))
            {
                sh.status(&key.symbol, stream.word(), status);
            }
        }
        StreamDown::Delay { k, stream, delay } => {
            if let Some(key) = key_of(sh, state, k)
                && !key.is_dead(lane_of(&stream))
            {
                sh.contained("data_delay", &key.symbol, || {
                    sh.sink.data_delay(&sh.venue, &key.symbol, stream.word(), delay);
                });
            }
        }
        StreamDown::Delivery { k, lane, class } => {
            if let Some(key) = key_of(sh, state, k) {
                tracing::debug!(
                    venue = %sh.venue,
                    symbol = %key.symbol,
                    lane = lane.stream_label().as_str(),
                    ?class,
                    "gatewayhub stream: the daemon changed a lane's delivery class"
                );
                key.set_class(lane, class);
            }
        }
        StreamDown::Gap { k, verb, stream, dropped, from_seq, to_seq, from_ts_ms } => {
            if let Some(key) = key_of(sh, state, k)
                && !key.is_dead(lane_of(&stream))
            {
                tracing::warn!(
                    venue = %sh.venue,
                    symbol = %key.symbol,
                    ?verb,
                    dropped,
                    from_seq,
                    to_seq,
                    from_ts_ms,
                    "gatewayhub stream: the daemon dropped frames (overflow); disclosed as a hole"
                );
                key.next.insert(verb, to_seq);
                // The hole opens where the daemon says (the last frame delivered before it), not now:
                // the survivors behind it may be seconds older than this reader's clock.
                sh.disclose_hole(&key.symbol, &stream, from_ts_ms);
            }
        }
        StreamDown::Heartbeat => {}
        StreamDown::Bye(reason) => {
            return Some(match reason {
                // Only OUR `Close` earns this reason; one the daemon volunteers is a loss like any other.
                ByeReason::ClientClosed if sh.stopping() => End::Stopped,
                ByeReason::ClientClosed => End::Lost(format!(
                    "gatewayhub {}: the daemon closed the session unprompted",
                    sh.opener.addr()
                )),
                ByeReason::SessionIdle => End::Parked,
                ByeReason::TooSlow { lapses } => End::Lost(format!(
                    "gatewayhub {}: the daemon ended the session — this reader fell behind \
                     ({lapses} lapses)",
                    sh.opener.addr()
                )),
                ByeReason::ServerStopping => {
                    End::Lost(format!("gatewayhub {}: the daemon is stopping", sh.opener.addr()))
                }
                ByeReason::ControlLaneOverflow => End::Lost(format!(
                    "gatewayhub {}: the daemon ended the session — its control lane overflowed",
                    sh.opener.addr()
                )),
            });
        }
    }
    None
}

#[cfg(test)]
#[path = "market_tests.rs"]
mod market_tests;
