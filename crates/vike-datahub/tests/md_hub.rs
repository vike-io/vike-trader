//! The market-data HUB suite (§8 item 15) — driven over a scripted `DataClient` double, with no
//! network and no venue.
//!
//! # ⚠ It runs on the DEFAULT build, and that is a property worth stating
//!
//! `crate::md` is FEATURE-FREE (only `md::venues::build_market_client`'s per-venue arms are gated),
//! so this file compiles and RUNS in the derived roster lane on every PR rather than only in the new
//! `live-feeds` lane. That matters because a suite behind a feature is a suite that reads green when
//! its lane is mis-spelled: `feature_lane_coverage.rs` asks "is it ever BUILT", not "are its tests
//! RUN".
//!
//! # Two structural properties the whole suite rests on
//!
//! 1. **The publish tick and the reconcile pass are CALLABLE FUNCTIONS**
//!    (`MdHub::publish_tick`, `MdHub::reconcile`), not threads with timers. Every property here is a
//!    statement about *what one tick produced*; against a free-running 100 ms thread each becomes
//!    sleep-and-hope, which on a loaded the CI box runner is the flake shape this repo has already paid
//!    for twice.
//! 2. **The reconcile clock is INJECTED** (`reconcile(now_ms)`, `SessionGuard::release_at`).
//!    `MD_LINGER` is 60 s; a suite that read the wall clock inside the hub could test it only by
//!    sleeping a minute or by not testing it.
//!
//! # ⚠ What this suite deliberately does NOT prove
//!
//! **No venue wire is exercised.** Every test runs over `ScriptedFeed`. Whether binance's
//! `subscribe_depth` actually produces what the hub assumes is proven by the live smokes and by
//! nothing here — §12.5 is the standing warning, where the binance depth series in the store turned
//! out to be a reconnect artefact nobody noticed for forty days, and a scripted double would have
//! reproduced the INTENDED cadence perfectly. A green here is "the hub is correct", never "the
//! market-data plane is verified".

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use vike_data::live::{DataClient, LiveDataError, LiveDataSink, StreamStatus, SubscriptionId};
use vike_datahub::md::hub::MdKey;
use vike_datahub::md::{MD_LINGER, MD_TAPE_CAP, MarketClientBuilder, MdHub};
use vike_datahub_client::market::{MdFrame, MdLane, MdSpec};
use vike_datahub_client::proto::Response;
use vike_model::TradeTick;

// ------------------------------------------------------------------------------------------------
// The double
// ------------------------------------------------------------------------------------------------

/// Every `DataClient` call the hub made, in order.
///
/// ⚠ ORDER matters and is asserted on, not just membership: §5.3's two-phase teardown claims
/// `begin_shutdown` comes BEFORE the first join, and the §0 teardown hazard is precisely a
/// `begin_shutdown` appearing where only per-key `unsubscribe`s belong.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Call {
    Client(String),
    Depth(String, String),
    Book(String, String),
    Trades(String, String),
    Unsubscribe(String, u64),
    BeginShutdown(String),
    Shutdown(String),
}

#[derive(Clone, Default)]
struct Log(Arc<Mutex<Vec<Call>>>);

impl Log {
    fn push(&self, c: Call) {
        self.0.lock().unwrap().push(c);
    }
    fn all(&self) -> Vec<Call> {
        self.0.lock().unwrap().clone()
    }
    fn count(&self, pred: impl Fn(&Call) -> bool) -> usize {
        self.all().iter().filter(|c| pred(c)).count()
    }
}

struct ScriptedFeed {
    venue: String,
    log: Log,
    next: Arc<AtomicU64>,
    /// When set, every `subscribe_*` fails — the "a failed venue subscribe leaves no phantom
    /// refcount and no phantom sub_id" case.
    fail: bool,
}

impl ScriptedFeed {
    fn issue(&mut self) -> Result<SubscriptionId, LiveDataError> {
        if self.fail {
            return Err(LiveDataError::Subscribe("scripted failure".into()));
        }
        Ok(SubscriptionId(self.next.fetch_add(1, Ordering::AcqRel)))
    }
}

impl DataClient for ScriptedFeed {
    fn subscribe_bars(&mut self, _s: &str, _i: &str) -> Result<SubscriptionId, LiveDataError> {
        Err(LiveDataError::Unsupported("scripted feed serves no bars"))
    }
    fn subscribe_quotes(&mut self, _s: &str) -> Result<SubscriptionId, LiveDataError> {
        Err(LiveDataError::Unsupported("scripted feed serves no quotes"))
    }
    fn subscribe_trades(&mut self, symbol: &str) -> Result<SubscriptionId, LiveDataError> {
        self.log.push(Call::Trades(self.venue.clone(), symbol.into()));
        self.issue()
    }
    fn subscribe_book(&mut self, symbol: &str) -> Result<SubscriptionId, LiveDataError> {
        self.log.push(Call::Book(self.venue.clone(), symbol.into()));
        self.issue()
    }
    fn subscribe_depth(&mut self, symbol: &str) -> Result<SubscriptionId, LiveDataError> {
        self.log.push(Call::Depth(self.venue.clone(), symbol.into()));
        self.issue()
    }
    fn unsubscribe(&mut self, id: SubscriptionId) {
        self.log.push(Call::Unsubscribe(self.venue.clone(), id.0));
    }
    fn begin_shutdown(&mut self) {
        self.log.push(Call::BeginShutdown(self.venue.clone()));
    }
    fn shutdown(&mut self) {
        self.log.push(Call::Shutdown(self.venue.clone()));
    }
}

fn builder(log: Log, fail: bool) -> MarketClientBuilder {
    let next = Arc::new(AtomicU64::new(1));
    Box::new(move |venue: &str, _sink: Arc<dyn LiveDataSink>| {
        log.push(Call::Client(venue.to_string()));
        Ok(Box::new(ScriptedFeed {
            venue: venue.to_string(),
            log: log.clone(),
            next: Arc::clone(&next),
            fail,
        }) as Box<dyn DataClient + Send>)
    })
}

/// A hub over the double, serving the venues this suite names.
fn hub(log: &Log) -> Arc<MdHub> {
    MdHub::new(
        builder(log.clone(), false),
        vec!["binance".into(), "polymarket".into(), "bybit".into()],
    )
}

fn spec(venue: &str, symbol: &str, lane: MdLane) -> MdSpec {
    MdSpec { venue: venue.into(), symbol: symbol.into(), lane, depth_levels: None }
}

fn now() -> i64 {
    vike_model::now_ms()
}

/// Decode one framed mailbox payload back into the `MdFrame` it carries. The publisher writes
/// through the wire's own `write_frame`, so a length prefix rides in front.
fn decode(bytes: &[u8]) -> MdFrame {
    assert!(bytes.len() > 4, "a framed payload carries a 4-byte length prefix");
    let resp: Response = serde_json::from_slice(&bytes[4..]).expect("decode Response");
    match resp {
        Response::Md(f) => *f,
        other => panic!("a stream mailbox carries only Response::Md, got {other:?}"),
    }
}

/// Drain a session's mailbox into decoded frames, in delivery order, synthesizing the writer's
/// `TapeGap` exactly where `run_market_writer` would.
fn drain(mb: &vike_datahub::md::mailbox::Mailbox) -> Vec<MdFrame> {
    use vike_datahub::md::mailbox::Recv;
    let mut out = Vec::new();
    loop {
        match mb.recv_timeout(std::time::Duration::from_millis(1)) {
            Recv::Frame { bytes, owed_gap } => {
                if let Some((key, dropped, from_seq, to_seq)) = owed_gap {
                    out.push(MdFrame::TapeGap {
                        venue: key.venue.clone(),
                        symbol: key.symbol.clone(),
                        dropped,
                        from_seq,
                        to_seq,
                    });
                }
                out.push(decode(&bytes));
            }
            _ => return out,
        }
    }
}

// ------------------------------------------------------------------------------------------------
// The suite's own floor
// ------------------------------------------------------------------------------------------------

/// ⚠ **The floor that stops this suite decaying into checking nothing** — the
/// `graceful_stop_pin.rs` `the_pin_has_a_non_empty_input` pattern, which exists because three gates
/// in this repo turned out to be unable to fail at all. It asserts the harness can construct a hub,
/// acquire a key, drive a venue subscription and produce a frame; if THIS goes red, every "asserts
/// an absence" test below is meaningless rather than passing.
#[test]
fn the_suite_has_a_non_empty_input() {
    let log = Log::default();
    let h = hub(&log);
    let mut g = h.open_session().expect("open a session");
    let s = spec("binance", "BTCUSDT.P", MdLane::Depth);
    h.acquire(g.id(), &s).expect("binance depth is servable");
    let r = h.reconcile(now());
    assert_eq!(r.started, 1, "the reconciler started the venue subscription: {r:?}");
    h.sink().l2_snapshot("binance", "BTCUSDT.P", 0.1, vec![(1.0, 2.0)], vec![(1.1, 3.0)], 7);
    let tick = h.publish_tick();
    assert_eq!(tick.frames_produced, 1, "one dirty key produced one frame: {tick:?}");
    assert_eq!(drain(g.mailbox()).len(), 1);
    g.release_at(now());
}

// ------------------------------------------------------------------------------------------------
// T1 — one venue subscription per key, however many subscribers
// ------------------------------------------------------------------------------------------------

/// **The ruling-2 property.** Two sessions on one key are ONE `subscribe_depth`.
///
/// ⚠ Non-vacuity: `calls.len() == 1` ALONE passes against a hub where session B's acquire silently
/// FAILED, and against one that subscribed the wrong symbol. So the assertions are exact call
/// CONTENTS, **and** session B's acquire returning `Ok`, **and** the venue count staying one after a
/// second reconcile. The acceptance assertion is not redundant with the call-count one: it is the
/// only thing separating "correctly shared" from "silently refused".
///
/// Mutation: make `reconcile` subscribe unconditionally instead of only when `sub_id` is `None` →
/// 1 becomes 2, red.
#[test]
fn a_second_subscriber_does_not_resubscribe_the_venue() {
    let log = Log::default();
    let h = hub(&log);
    let s = spec("binance", "BTCUSDT.P", MdLane::Depth);

    let mut a = h.open_session().unwrap();
    h.acquire(a.id(), &s).expect("session A accepted");
    h.reconcile(now());

    let mut b = h.open_session().unwrap();
    h.acquire(b.id(), &s).expect("session B must be ACCEPTED, not silently refused");
    h.reconcile(now());

    let depth: Vec<Call> = log.all().into_iter().filter(|c| matches!(c, Call::Depth(..))).collect();
    assert_eq!(
        depth,
        vec![Call::Depth("binance".into(), "BTCUSDT.P".into())],
        "exactly ONE venue subscription, for exactly that symbol: {depth:?}"
    );
    assert_eq!(log.count(|c| matches!(c, Call::Client(_))), 1, "and ONE venue client");

    // The third leg: one SESSION acquiring the key twice (two DOM windows on one symbol) is still
    // one subscription, and one release does not take the other's ladder away.
    h.acquire(a.id(), &s).expect("re-acquire in the same session");
    h.reconcile(now());
    assert_eq!(log.count(|c| matches!(c, Call::Depth(..))), 1);
    a.release_at(now());
    h.reconcile(now());
    assert_eq!(
        log.count(|c| matches!(c, Call::Unsubscribe(..))),
        0,
        "session B still holds the key — nothing may be unsubscribed"
    );
    b.release_at(now());
}

// ------------------------------------------------------------------------------------------------
// T2 — release, linger, reap; and the panic path
// ------------------------------------------------------------------------------------------------

/// **T2a — a PANICKING writer releases**, because the release is a `Drop` and not a statement.
///
/// ⚠ This leg asserts an ABSENCE (nothing was unsubscribed yet) and would pass against a hub that
/// does nothing at all. `a_reap_past_the_linger_unsubscribes_per_key` is what rescues it: the same
/// key, the same harness, one `reconcile` later, the unsubscribe MUST appear. Neither leg is worth
/// writing without the other.
///
/// ⚠ It also depends on the hub's poison-recovery discipline: the panic below poisons nothing here,
/// but a hub using `.expect("poisoned")` anywhere would make a real writer panic turn every later
/// assertion into a panic, and the failure would read as a broken test rather than a broken
/// teardown.
#[test]
fn a_panicking_writer_releases_every_key_through_drop() {
    let log = Log::default();
    let h = hub(&log);
    let k1 = spec("binance", "BTCUSDT.P", MdLane::Depth);
    let k2 = spec("binance", "BTCUSDT.P", MdLane::Trades);
    let at = now();

    let hub2 = Arc::clone(&h);
    let unwound = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
        let g = hub2.open_session().unwrap();
        hub2.acquire(g.id(), &k1).unwrap();
        hub2.acquire(g.id(), &k2).unwrap();
        hub2.reconcile(at);
        panic!("writer fault");
    }));
    assert!(unwound.is_err(), "the guard: the closure must actually unwind");

    assert_eq!(log.count(|c| matches!(c, Call::Depth(..) | Call::Trades(..))), 2);
    // Both keys are at zero, and NOTHING is torn down — they are in LINGER, not reaped.
    h.reconcile(at + 1);
    assert_eq!(
        log.count(|c| matches!(c, Call::Unsubscribe(..) | Call::BeginShutdown(_))),
        0,
        "a released key lingers; it is not torn down at once: {:?}",
        log.all()
    );
}

/// **T2b — the reap, and the §0 TEARDOWN HAZARD as an assertion.**
///
/// A reap that touches SOME of a venue's keys must call `unsubscribe` per key and **never**
/// `begin_shutdown`: `crates/vike-data/src/live.rs`'s `FeedRegistry::raise_stops` — which is what
/// every venue's `begin_shutdown` IS — raises the stop flag of EVERY subscription that client owns.
/// A partial reap using it would stop the OTHER keys' threads while the registry still held their
/// ids, so the hub would report itself subscribed and deliver nothing. §5.3 specifies exactly that
/// two-phase teardown for a multi-key reap, and this test is why it is not implemented literally.
#[test]
fn a_partial_reap_unsubscribes_per_key_and_never_raises_every_stop() {
    let log = Log::default();
    let h = hub(&log);
    let keep = spec("binance", "ETHUSDT.P", MdLane::Depth);
    let go = spec("binance", "BTCUSDT.P", MdLane::Depth);
    let at = now();

    let mut keeper = h.open_session().unwrap();
    h.acquire(keeper.id(), &keep).unwrap();
    let mut leaver = h.open_session().unwrap();
    h.acquire(leaver.id(), &go).unwrap();
    h.reconcile(at);
    assert_eq!(log.count(|c| matches!(c, Call::Depth(..))), 2);

    leaver.release_at(at);
    let r = h.reconcile(at + MD_LINGER.as_millis() as i64 + 1);
    assert_eq!(r.stopped, 1, "exactly the reaped key: {r:?}");
    assert_eq!(r.clients_dropped, 0, "the venue client stays — another key is still live");
    assert_eq!(log.count(|c| matches!(c, Call::Unsubscribe(..))), 1);
    assert_eq!(
        log.count(|c| matches!(c, Call::BeginShutdown(_) | Call::Shutdown(_))),
        0,
        "⚠ a PARTIAL reap must never raise every stop flag this client owns: {:?}",
        log.all()
    );
    keeper.release_at(at);
}

/// ...and the OTHER half: when the reap set IS the client's whole live set, the two-phase idiom is
/// legitimate and is used — `begin_shutdown` BEFORE `shutdown`, asserted on ORDER, because that is
/// the only place the raise-all-then-join win is available and the only place it is safe.
#[test]
fn a_whole_venue_reap_raises_stops_before_it_joins_and_drops_the_client() {
    let log = Log::default();
    let h = hub(&log);
    let a = spec("binance", "BTCUSDT.P", MdLane::Depth);
    let b = spec("binance", "BTCUSDT.P", MdLane::Trades);
    let at = now();

    let mut g = h.open_session().unwrap();
    h.acquire(g.id(), &a).unwrap();
    h.acquire(g.id(), &b).unwrap();
    h.reconcile(at);
    g.release_at(at);
    let r = h.reconcile(at + MD_LINGER.as_millis() as i64 + 1);

    assert_eq!(r.stopped, 2);
    assert_eq!(r.clients_dropped, 1, "the venue client itself is dropped: {r:?}");
    let teardown: Vec<Call> = log
        .all()
        .into_iter()
        .filter(|c| matches!(c, Call::BeginShutdown(_) | Call::Shutdown(_)))
        .collect();
    assert_eq!(
        teardown,
        vec![Call::BeginShutdown("binance".into()), Call::Shutdown("binance".into())],
        "phase one raises every flag, THEN phase two joins: {teardown:?}"
    );
    assert_eq!(
        log.count(|c| matches!(c, Call::Unsubscribe(..))),
        0,
        "a whole-client reap does not also pay a per-key timeout"
    );
}

/// **T2c — re-acquiring inside the linger CLEARS the deadline.**
///
/// Catches a `reap` that stores an ABSOLUTE deadline at release and forgets to clear it on
/// re-acquire: a DOM window toggled off and on then loses its book 60 s later, in the exact series
/// somebody just asked for.
#[test]
fn a_key_reacquired_inside_the_linger_is_never_reaped() {
    let log = Log::default();
    let h = hub(&log);
    let s = spec("binance", "BTCUSDT.P", MdLane::Depth);
    let at = now();

    let mut g = h.open_session().unwrap();
    h.acquire(g.id(), &s).unwrap();
    h.reconcile(at);
    g.release_at(at);

    // Halfway through the linger: nothing yet.
    h.reconcile(at + MD_LINGER.as_millis() as i64 / 2);
    assert_eq!(log.count(|c| matches!(c, Call::Unsubscribe(..))), 0);

    // Re-acquire, then walk PAST the original deadline. Still nothing.
    let mut g2 = h.open_session().unwrap();
    h.acquire(g2.id(), &s).unwrap();
    h.reconcile(at + MD_LINGER.as_millis() as i64 + 5_000);
    assert_eq!(
        log.count(|c| matches!(c, Call::Unsubscribe(..) | Call::Shutdown(_))),
        0,
        "the re-acquire must clear the absolute deadline: {:?}",
        log.all()
    );
    // ...and the key was never re-subscribed either: the survivor was left strictly alone.
    assert_eq!(log.count(|c| matches!(c, Call::Depth(..))), 1);
    g2.release_at(at);
}

// ------------------------------------------------------------------------------------------------
// T4 — the tape gap, and the §7.2 backstop
// ------------------------------------------------------------------------------------------------

/// **A hub-side tape overflow emits `TapeGap` BEFORE the next `Trades` frame**, and the seq
/// arithmetic is checked against an INDEPENDENT witness.
///
/// ⚠ Three non-vacuity floors, and the third is the one a careless implementation defeats:
///
/// 1. `dropped > 0` — a tape that never overflowed produces no gap and every ordering assertion
///    above it is vacuous.
/// 2. the batch is exactly `MD_TAPE_CAP` — a tape that silently grew UNBOUNDED also produces no gap,
///    reads green on absence-style assertions, and is the memory bug §12.6's budget depends on not
///    having.
/// 3. `dropped` is checked against the count the TEST emitted, not only against a range the
///    implementation reported. `to_seq - from_seq == dropped` alone is TAUTOLOGICAL if an
///    implementation computes `to_seq` as `from_seq + dropped`; two paths to the same number is what
///    makes neither one circular.
///
/// The mutation this catches and nothing else does: assign `seq` AFTER the drop decision instead of
/// before. Every disclosed number stays self-consistent and every other test in this file still
/// passes.
#[test]
fn a_tape_overflow_emits_its_gap_before_the_next_batch() {
    let log = Log::default();
    let h = hub(&log);
    let s = spec("binance", "BTCUSDT.P", MdLane::Trades);
    let key = MdKey::of(&s);
    let mut g = h.open_session().unwrap();
    h.acquire(g.id(), &s).unwrap();
    h.reconcile(now());

    const EXTRA: usize = 50;
    let sink = h.sink();
    for i in 0..(MD_TAPE_CAP + EXTRA) {
        sink.trade(
            "binance",
            "BTCUSDT.P",
            TradeTick {
                ts: i as i64,
                local_ts: 0,
                price: 1.0,
                size: 1.0,
                is_buyer_maker: false,
                symbol: "BTCUSDT.P".into(),
            },
        );
    }
    let tick = h.publish_tick();
    assert_eq!(tick.frames_produced, 1, "one key, one tick, one frame: {tick:?}");

    let frames = drain(g.mailbox());
    assert_eq!(frames.len(), 2, "the gap and the batch, in that order: {frames:?}");
    match &frames[0] {
        MdFrame::TapeGap { dropped, from_seq, to_seq, venue, symbol } => {
            assert_eq!(venue, &key.venue);
            assert_eq!(symbol, &key.symbol);
            assert!(*dropped > 0, "floor 1: the tape must actually have overflowed");
            assert_eq!(*dropped, EXTRA as u64, "floor 3: the count the TEST emitted");
            assert!(to_seq >= from_seq, "{from_seq}..{to_seq}");
        }
        other => panic!("the gap must come FIRST, before the batch it precedes: {other:?}"),
    }
    match &frames[1] {
        MdFrame::Trades { ticks, .. } => {
            assert_eq!(ticks.len(), MD_TAPE_CAP, "floor 2: the tape is BOUNDED, not merely lossy");
            // §12.4's second finding, held as a property: the hub tape holds a SYMBOL-LESS tick,
            // because a 78-character polymarket token id turns 64 B into ~142 B and 64 keys into
            // 37 MB, breaking the memory budget on its own.
            assert!(
                ticks.iter().all(|t| t.symbol.is_empty()),
                "the hub tape must drop the per-tick symbol — the envelope carries it once"
            );
        }
        other => panic!("expected the batch after the gap: {other:?}"),
    }
    g.release_at(now());
}

/// The wire `seq` is CONTIGUOUS across a key's delivered frames when nothing dropped — the other
/// half of §7.2, and the baseline a jump is judged against.
#[test]
fn the_wire_seq_is_contiguous_with_no_drops() {
    let log = Log::default();
    let h = hub(&log);
    let s = spec("binance", "BTCUSDT.P", MdLane::Depth);
    let mut g = h.open_session().unwrap();
    h.acquire(g.id(), &s).unwrap();
    h.reconcile(now());

    let sink = h.sink();
    let mut seqs = Vec::new();
    for i in 0..5 {
        sink.l2_snapshot("binance", "BTCUSDT.P", 0.1, vec![(1.0, 1.0)], vec![(2.0, 1.0)], i);
        h.publish_tick();
        for f in drain(g.mailbox()) {
            if let MdFrame::Depth(b) = f {
                seqs.push(b.seq);
            }
        }
    }
    assert_eq!(seqs, vec![1, 2, 3, 4, 5], "strictly +1, assigned by the publisher: {seqs:?}");
    g.release_at(now());
}

// ------------------------------------------------------------------------------------------------
// T5 — the attach path
// ------------------------------------------------------------------------------------------------

/// **A STALE key sends its `Status` and NO snapshot on attach** (§5.2 step 5).
///
/// The consequence of getting this wrong is precise, and it is the worst outcome a market-data
/// display has: §7.4 makes the CLIENT stamp a LOCAL receipt on arrival, so a two-minute-old book
/// written into `BookStore` reads LIVE for a whole `DOM_STALE_MS` window — a populated, fresh-looking
/// ladder for a market that stopped ticking two minutes ago, indistinguishable from a quiet market.
///
/// ⚠ This test asserts an ABSENCE, so leg (b) — the SAME key, the SAME harness, status `Live`, the
/// snapshot DOES arrive — is what stops it certifying an outage. Without (b), (a) is worthless.
#[test]
fn attach_sends_status_first_and_the_book_only_when_live() {
    let log = Log::default();
    let h = hub(&log);
    let s = spec("binance", "BTCUSDT.P", MdLane::Depth);
    let key = MdKey::of(&s);
    let mut g = h.open_session().unwrap();
    h.acquire(g.id(), &s).unwrap();
    h.reconcile(now());
    let sink = h.sink();
    sink.l2_snapshot("binance", "BTCUSDT.P", 0.1, vec![(1.0, 1.0)], vec![(2.0, 1.0)], 5);

    // (a) GapStart — status only, though the hub's slot HOLDS a book.
    sink.stream_status("binance", "BTCUSDT.P", "depth", StreamStatus::GapStart { at_ts_ms: 1 });
    let f = h.attach_frames(&key, now());
    assert_eq!(f.len(), 1, "a gapped key attaches with its STATUS and no book: {f:?}");
    assert!(matches!(f[0], MdFrame::Status { .. }), "...and the status IS there: {f:?}");

    // (c) STALE — the variant a hub that special-cases only GapStart would leak a book on. That is
    // the silently-failed-resubscribe case `DEPTH_FRESHNESS_THRESHOLD` exists to catch, and the
    // exact failure §12.5 found running unnoticed for forty days.
    sink.stream_status(
        "binance",
        "BTCUSDT.P",
        "depth",
        StreamStatus::Stale { newest_data_ts_ms: 1, now_ms: 2 },
    );
    let f = h.attach_frames(&key, now());
    assert_eq!(f.len(), 1, "a STALE key attaches with its status and no book: {f:?}");

    // (b) LIVE — the same key, the same harness: the snapshot DOES arrive, and AFTER the status.
    sink.stream_status(
        "binance",
        "BTCUSDT.P",
        "depth",
        StreamStatus::Live { gap_started_ts_ms: None },
    );
    let f = h.attach_frames(&key, now());
    assert_eq!(f.len(), 2, "live: status THEN book: {f:?}");
    assert!(matches!(f[0], MdFrame::Status { .. }), "the status is FIRST: {f:?}");
    assert!(matches!(f[1], MdFrame::Depth(_)), "...and the book follows it: {f:?}");
    g.release_at(now());
}

/// (d) LIVE but with an EMPTY slot: exactly one frame — the status. The client then shows "waiting
/// for first book" instead of an empty ladder that reads as a real, thin market.
#[test]
fn a_live_key_with_no_book_yet_attaches_with_its_status_alone() {
    let log = Log::default();
    let h = hub(&log);
    let s = spec("binance", "BTCUSDT.P", MdLane::Depth);
    let mut g = h.open_session().unwrap();
    h.acquire(g.id(), &s).unwrap();
    h.sink().stream_status(
        "binance",
        "BTCUSDT.P",
        "depth",
        StreamStatus::Live { gap_started_ts_ms: None },
    );
    let f = h.attach_frames(&MdKey::of(&s), now());
    assert_eq!(f.len(), 1, "{f:?}");
    g.release_at(now());
}

/// **T8 — a reconnecting subscriber is NEVER handed the tape it missed** (§7.3 rule 4).
///
/// The server keeps no replay buffer, so a reconnect must be a HOLE, never a duplicate: handing a
/// new subscriber prints from before it existed, with no gap marker, double-counts every reconnect
/// into `crates/vike-app-core/src/orderflow.rs`'s `OrderflowAgg`, which has no per-trade dedup.
/// §5.2 step 5 specifies status-then-BOOK for attach and says nothing about the tape; this is that
/// silence, closed.
///
/// ⚠ Paired with the POSITIVE: the key's live tape DOES flow to the new subscriber from the next
/// tick onward. Otherwise "received no ticks" passes against a broken attach.
#[test]
fn a_new_subscriber_is_never_handed_the_tape_it_missed() {
    let log = Log::default();
    let h = hub(&log);
    let s = spec("binance", "BTCUSDT.P", MdLane::Trades);
    let key = MdKey::of(&s);
    // A resident key, so the tape accumulates with no session attached at all.
    h.add_resident(&s).expect("a served venue on a supported lane");
    h.reconcile(now());
    let sink = h.sink();
    for i in 0..10 {
        sink.trade(
            "binance",
            "BTCUSDT.P",
            TradeTick {
                ts: i,
                local_ts: 0,
                price: 1.0,
                size: 1.0,
                is_buyer_maker: false,
                symbol: String::new(),
            },
        );
    }
    // ⚠ A tick with NO subscriber CONSUMES the tape. That is the structural half of the property:
    // the server keeps no replay buffer because it never accumulates one, so there is nothing for a
    // later attach to leak. `frames_produced` is zero — the work happened, nothing was sent.
    let idle = h.publish_tick();
    assert_eq!(idle.keys_walked, 1, "the dirty key was walked: {idle:?}");
    assert_eq!(idle.frames_produced, 0, "...and nothing was sent to nobody: {idle:?}");

    let mut g = h.open_session().unwrap();
    h.acquire(g.id(), &s).unwrap();
    let attached = h.attach_frames(&key, now());
    assert!(
        attached.iter().all(|f| !matches!(f, MdFrame::Trades { .. })),
        "the accumulated tape must NOT be replayed to a new subscriber: {attached:?}"
    );

    // ...and the POSITIVE: from the next tick, live prints DO flow.
    sink.trade(
        "binance",
        "BTCUSDT.P",
        TradeTick {
            ts: 99,
            local_ts: 0,
            price: 1.0,
            size: 1.0,
            is_buyer_maker: false,
            symbol: String::new(),
        },
    );
    h.publish_tick();
    let frames = drain(g.mailbox());
    assert!(
        frames.iter().any(|f| matches!(f, MdFrame::Trades { .. })),
        "live prints reach the new subscriber from the next tick: {frames:?}"
    );
    // ...and exactly the ONE live print, with NO gap marker: the ten it never saw are not a hole in
    // ITS tape, they are prints from before it existed.
    match frames.iter().find(|f| matches!(f, MdFrame::Trades { .. })).unwrap() {
        MdFrame::Trades { ticks, .. } => assert_eq!(ticks.len(), 1, "{ticks:?}"),
        _ => unreachable!(),
    }
    assert!(
        frames.iter().all(|f| !matches!(f, MdFrame::TapeGap { .. })),
        "a subscriber is not owed a gap for prints that predate it: {frames:?}"
    );
    g.release_at(now());
}

// ------------------------------------------------------------------------------------------------
// T6c — the lane LABEL, which admission gates cannot see
// ------------------------------------------------------------------------------------------------

/// **A `Depth` subscription's frames decode as `MdFrame::Depth`, NEVER `MdFrame::Book`** — and the
/// polymarket inverse.
///
/// ⚠ This is the test a reviewer is most likely to call redundant with the caps gate, and it is the
/// only one that catches the mutation that matters. `Depth` and `Book` are separate VARIANTS over
/// the SAME payload struct, so nothing in the type system stops the publisher putting a
/// Depth-sourced snapshot into an `MdFrame::Book`. Swap the variant constructor in the publisher's
/// Depth arm: the admission gates stay green, this goes red alone. Without it §7.5's claim is
/// untested on the only path that actually carries data — and letting a conflating lane wear the
/// book's name is what lets a maker-fill backtest report fills it could never have got.
#[test]
fn a_conflating_lane_never_wears_the_lossless_lanes_name() {
    let log = Log::default();
    let h = hub(&log);
    let sink = h.sink();

    let depth = spec("binance", "BTCUSDT.P", MdLane::Depth);
    let mut g = h.open_session().unwrap();
    h.acquire(g.id(), &depth).unwrap();
    h.reconcile(now());
    sink.l2_snapshot("binance", "BTCUSDT.P", 0.1, vec![(1.0, 1.0)], vec![(2.0, 1.0)], 1);
    h.publish_tick();
    let frames = drain(g.mailbox());
    assert!(!frames.is_empty(), "the guard: the depth lane must have produced something");
    assert!(
        frames.iter().all(|f| !matches!(f, MdFrame::Book(_))),
        "a DEPTH subscription must never yield a Book frame: {frames:?}"
    );
    assert!(frames.iter().any(|f| matches!(f, MdFrame::Depth(_))), "{frames:?}");
    g.release_at(now());

    // ...and the inverse, on the one venue that serves the lossless lane.
    let book = spec("polymarket", "12345", MdLane::Book);
    let mut p = h.open_session().unwrap();
    h.acquire(p.id(), &book).expect("polymarket serves the Book lane");
    h.reconcile(now());
    sink.book("polymarket", "12345", Arc::new(vike_model::L2Book::new(0.001)));
    h.publish_tick();
    let frames = drain(p.mailbox());
    assert!(!frames.is_empty(), "the guard: the book lane must have produced something");
    assert!(
        frames.iter().all(|f| !matches!(f, MdFrame::Depth(_))),
        "a BOOK subscription must never yield a Depth frame: {frames:?}"
    );
    p.release_at(now());
}

// ------------------------------------------------------------------------------------------------
// T6b — the SERVING gate, and it must refuse BEFORE it subscribes
// ------------------------------------------------------------------------------------------------

/// A lane the venue's declared caps do not serve is refused with `require_live_verb`'s OWN string,
/// **and the double records ZERO subscribe calls** — a hub that refused AFTER subscribing has
/// already opened the socket and spent venue budget.
///
/// String EQUALITY, not `contains("book")`: equality is what makes the wire's refusal set
/// structurally unable to drift from `crates/vike-model/src/venue_caps.rs`'s declared rows.
#[test]
fn an_unsupported_lane_is_refused_with_the_matrixs_own_words_and_no_venue_call() {
    use vike_datahub_client::market::MdRefusal;
    let log = Log::default();
    let h = hub(&log);
    let mut g = h.open_session().unwrap();

    let bad = spec("binance", "BTCUSDT.P", MdLane::Book);
    let err = h.acquire(g.id(), &bad).expect_err("binance declares no lossless book lane");
    let want = vike_data::require_live_verb("binance", vike_model::LiveVerb::Book)
        .expect_err("the matrix must refuse it")
        .to_string();
    assert_eq!(err, MdRefusal::LaneUnsupported(want));
    assert!(err.is_permanent(), "a capability refusal is permanent — the client must not retry");

    // The inverse, on the venue whose partition runs the other way.
    let bad2 = spec("polymarket", "12345", MdLane::Depth);
    assert!(matches!(
        h.acquire(g.id(), &bad2).expect_err("polymarket declares no depth lane"),
        MdRefusal::LaneUnsupported(_)
    ));

    h.reconcile(now());
    assert_eq!(
        log.all(),
        Vec::new(),
        "a refused spec must open NO venue client and make NO subscribe call: {:?}",
        log.all()
    );
    g.release_at(now());
}

/// An UNKNOWN venue and a venue this BUILD does not serve get DIFFERENT refusals, in that order of
/// precedence — cheapest and most permanent first.
#[test]
fn an_unknown_venue_and_an_unserved_one_are_different_refusals() {
    use vike_datahub_client::market::MdRefusal;
    let log = Log::default();
    let h = hub(&log);
    let mut g = h.open_session().unwrap();
    assert_eq!(
        h.acquire(g.id(), &spec("kalshi", "X", MdLane::Trades)).unwrap_err(),
        MdRefusal::UnknownVenue
    );
    // `okx` IS a roster venue and IS servable by the caps matrix, but this hub was not built for it.
    assert!(matches!(
        h.acquire(g.id(), &spec("okx", "BTC-USDT-SWAP", MdLane::Depth)).unwrap_err(),
        MdRefusal::VenueNotServed(_)
    ));
    g.release_at(now());
}

// ------------------------------------------------------------------------------------------------
// T10 — the resident set
// ------------------------------------------------------------------------------------------------

/// **A RESIDENT key is never reaped through client release**, while a NON-resident key at zero IS
/// reaped in the SAME `reconcile` call.
///
/// ⚠ The second half is the non-vacuity floor: without it, "nothing was reaped" passes for the wrong
/// reason. And the failure it catches is the one tier R exists to prevent — a janitor that treats
/// residents like on-demand keys silently retires the operator's declared set the moment the last
/// desktop closes.
#[test]
fn a_resident_key_outlives_every_client_while_an_on_demand_one_is_reaped() {
    let log = Log::default();
    let h = hub(&log);
    let res = spec("binance", "BTCUSDT.P", MdLane::Depth);
    let dem = spec("binance", "ETHUSDT.P", MdLane::Depth);
    let at = now();

    h.add_resident(&res).expect("a served venue on a supported lane");
    let mut g = h.open_session().unwrap();
    h.acquire(g.id(), &dem).unwrap();
    h.reconcile(at);
    assert_eq!(log.count(|c| matches!(c, Call::Depth(..))), 2);

    g.release_at(at);
    let r = h.reconcile(at + MD_LINGER.as_millis() as i64 + 1);
    assert_eq!(r.stopped, 1, "exactly the ON-DEMAND key: {r:?}");
    let unsubs: Vec<Call> =
        log.all().into_iter().filter(|c| matches!(c, Call::Unsubscribe(..))).collect();
    assert_eq!(unsubs.len(), 1, "and only one: {unsubs:?}");
    assert_eq!(
        log.count(|c| matches!(c, Call::Shutdown(_))),
        0,
        "the resident key keeps the venue client alive"
    );
    // ...and the resident key is STILL there and still serving.
    h.sink().l2_snapshot("binance", "BTCUSDT.P", 0.1, vec![(1.0, 1.0)], vec![(2.0, 1.0)], 9);
    let mut g2 = h.open_session().unwrap();
    h.acquire(g2.id(), &res).unwrap();
    h.reconcile(at + MD_LINGER.as_millis() as i64 + 2);
    assert_eq!(
        log.count(|c| matches!(c, Call::Depth(..))),
        2,
        "a resident key is never re-subscribed — it was never stopped"
    );
    g2.release_at(at);
}

// ------------------------------------------------------------------------------------------------
// A failed venue subscribe leaves no phantom
// ------------------------------------------------------------------------------------------------

/// A `subscribe_*` that FAILS must leave the key WANTED with no subscription id, so the next pass
/// retries it — never a key the hub believes is live. That is §6.1's "connects, reports healthy,
/// delivers nothing" failure, produced by bookkeeping rather than by a socket.
#[test]
fn a_failed_venue_subscribe_leaves_no_phantom_subscription() {
    let log = Log::default();
    let h = MdHub::new(builder(log.clone(), true), vec!["binance".into()]);
    let mut g = h.open_session().unwrap();
    h.acquire(g.id(), &spec("binance", "BTCUSDT.P", MdLane::Depth)).unwrap();

    let r = h.reconcile(now());
    assert_eq!(r.started, 0, "nothing started: {r:?}");
    assert_eq!(r.failed.len(), 1, "and the failure is REPORTED, not swallowed: {r:?}");

    // The next pass RETRIES: the key is still wanted, so the attempt count grows.
    let r2 = h.reconcile(now());
    assert_eq!(r2.failed.len(), 1, "still wanted, still retried: {r2:?}");
    assert_eq!(log.count(|c| matches!(c, Call::Depth(..))), 2, "two attempts, no phantom");
    g.release_at(now());
}

// ------------------------------------------------------------------------------------------------
// The depth fold
// ------------------------------------------------------------------------------------------------

/// A key's effective depth is the MAX over its live subscribers, clamped to the ceiling — the
/// property §6.1's one-encode-per-key rule forces, and the consequence
/// `crate::md::MD_MAILBOX_BYTES` exists to absorb.
#[test]
fn a_keys_depth_is_the_max_over_its_subscribers_and_is_clamped() {
    use vike_datahub_client::market::MD_DEPTH_LEVELS_CEILING;
    let log = Log::default();
    let h = hub(&log);
    let shallow = MdSpec { depth_levels: Some(5), ..spec("binance", "BTCUSDT.P", MdLane::Depth) };
    let deep = MdSpec { depth_levels: Some(9_999), ..shallow.clone() };

    let mut a = h.open_session().unwrap();
    let acc = h.acquire(a.id(), &shallow).unwrap();
    assert_eq!(acc.depth_levels, Some(5), "the echo is AUTHORITATIVE, not the request");
    let mut b = h.open_session().unwrap();
    let acc = h.acquire(b.id(), &deep).unwrap();
    assert_eq!(
        acc.depth_levels,
        Some(MD_DEPTH_LEVELS_CEILING),
        "a request above the ceiling is CLAMPED and the client LEARNS the number"
    );
    h.reconcile(now());

    // 300 levels a side in, ceiling out: the frame is cut by the server, not by the venue.
    let levels: Vec<(f64, f64)> = (0..300).map(|i| (100.0 - i as f64, 1.0)).collect();
    let asks: Vec<(f64, f64)> = (0..300).map(|i| (200.0 + i as f64, 1.0)).collect();
    h.sink().l2_snapshot("binance", "BTCUSDT.P", 0.1, levels, asks, 1);
    h.publish_tick();
    for f in drain(a.mailbox()) {
        if let MdFrame::Depth(bk) = f {
            assert_eq!(bk.bids.len(), MD_DEPTH_LEVELS_CEILING as usize);
            assert!(bk.bids[0].0 > bk.bids[1].0, "bids DESCEND, best first");
            assert!(bk.asks[0].0 < bk.asks[1].0, "asks ASCEND, best first");
        }
    }
    a.release_at(now());
    b.release_at(now());
}

// ------------------------------------------------------------------------------------------------
// The CTRL lane and the attach burst
// ------------------------------------------------------------------------------------------------

/// **A MULTI-KEY ATTACH MUST NOT CLOSE THE CONNECTION IT IS ATTACHING.**
///
/// `crate::server`'s `run_market_writer` pushes one `attach_frames` `Status` per accepted key BEFORE
/// it enters its drain loop, and it is itself the mailbox's only consumer — so nothing drains during
/// that burst and the CTRL lane sees `accepted.len()` frames back to back. At `MD_MAILBOX_CTRL = 16`
/// that was a DETERMINISTIC kill of every session holding 17 or more specs: push 17 set `must_close`
/// and the writer's first statement is `if mailbox.must_close()`, so the peer got a `Bye` before one
/// data frame — well under the advertised `MD_MAX_SPECS_PER_SESSION` of 64.
///
/// ⚠ Two non-vacuity floors. The key count must EXCEED the old constant (16) or the test passes on
/// the bug, and every `Status` must come back out — a lane that silently dropped them would satisfy
/// "not closed" for the wrong reason.
#[test]
fn a_multi_key_attach_never_overflows_the_control_lane() {
    use vike_datahub::md::hub::push_attach_frame;
    use vike_datahub::md::{MD_MAILBOX_CTRL, MD_MAX_KEYS_PER_VENUE};

    let log = Log::default();
    let h = hub(&log);
    let mut g = h.open_session().unwrap();
    // The most keys this harness can reach: 8 symbols x 2 servable lanes on each of the three
    // venues, which is MD_MAX_KEYS_PER_VENUE on every one of them.
    let mut accepted = Vec::new();
    for (venue, lanes) in [
        ("binance", [MdLane::Depth, MdLane::Trades]),
        ("bybit", [MdLane::Depth, MdLane::Trades]),
        ("polymarket", [MdLane::Book, MdLane::Trades]),
    ] {
        for i in 0..8 {
            for lane in lanes {
                let s = spec(venue, &format!("SYM{i}"), lane);
                accepted.push(h.acquire(g.id(), &s).expect("a servable lane inside every cap"));
            }
        }
    }
    assert_eq!(
        accepted.len(),
        3 * MD_MAX_KEYS_PER_VENUE as usize,
        "the harness must reach a MULTI-key session or the burst below proves nothing"
    );
    assert!(
        accepted.len() > 16,
        "floor: the burst must EXCEED the old MD_MAILBOX_CTRL of 16, or this passes on the bug"
    );
    assert!(
        accepted.len() <= MD_MAILBOX_CTRL,
        "...and stay inside the relation the constant holds"
    );

    // Exactly what `run_market_writer` does, in the same order, with NO drain in between.
    let at = now();
    for s in &accepted {
        let key = MdKey::of(s);
        for frame in h.attach_frames(&key, at) {
            push_attach_frame(g.mailbox(), &key, frame);
        }
    }
    assert!(
        !g.mailbox().must_close(),
        "the attach burst CLOSED the connection it was attaching — the peer would get a Bye before \
         one data frame"
    );
    let frames = drain(g.mailbox());
    let statuses = frames.iter().filter(|f| matches!(f, MdFrame::Status { .. })).count();
    assert_eq!(statuses, accepted.len(), "every accepted key's Status survived: {frames:?}");
    g.release_at(now());
}

/// The byte price of the CTRL lane is a MEASURED fact, not an assumption.
///
/// `vike_datahub::md::MD_CTRL_FRAME_CEILING_BYTES` is a term in `MD_MAILBOX_BYTES`' compile-time
/// assertion — `MD_MAX_SPECS_PER_SESSION * MD_FRAME_CEILING_BYTES + MD_MAILBOX_CTRL * this <= the
/// byte bound` — so a `Status` frame wider than it silently falsifies that assertion and, through
/// it, the 32 MB plane ceiling
/// `docs/decisions/0052-a-market-data-subscription-is-an-observe-verb.md` rests on.
///
/// # ⚠ It used to measure ONE example and compare it to nothing else
///
/// The original body framed polymarket's 78-character token id and asserted `<= 384`. That is a
/// witness, not a bound: NOTHING made any other symbol fit, and the wire had no symbol length
/// limit at all — so a 10,000-character symbol produced a ctrl frame twenty-six times the declared
/// ceiling and the assertion above it became false from the wire. The ceiling is now an
/// ARITHMETIC consequence of two independently-pinned terms, and this test pins both:
///
/// 1. **the ENVELOPE** — everything a `Status` frame costs with an EMPTY symbol, at the widest
///    venue slug, lane word and status spelling the roster can produce. Pinned by EQUALITY against
///    `vike_datahub::md::MD_STATUS_ENVELOPE_CEILING_BYTES`, not `<=`, so a wire change that moves
///    the envelope in EITHER direction is caught rather than silently eating the slack the symbol
///    bound was derived from.
/// 2. **the SYMBOL** — a symbol at exactly `MD_MAX_SYMBOL_BYTES` made entirely of `"`, i.e.
///    serde_json's worst expansion of a control-free string. That exercises the `2x` factor
///    `vike_datahub::md::MD_STATUS_ENVELOPE_CEILING_BYTES`' sibling assertion asserts in prose.
///
/// Leg 3 keeps the original measurement as a REGRESSION witness: the real polymarket token id at
/// its hard maximum still frames where it always did.
#[test]
fn a_status_frame_fits_the_declared_ctrl_ceiling() {
    use vike_datahub::md::{MD_CTRL_FRAME_CEILING_BYTES, MD_STATUS_ENVELOPE_CEILING_BYTES};
    use vike_datahub_client::market::{MD_MAX_SYMBOL_BYTES, WireStreamStatus};
    use vike_datahub_client::proto::write_frame;

    fn framed(venue: &str, symbol: String, lane: MdLane, status: WireStreamStatus) -> usize {
        let frame = MdFrame::Status { venue: venue.into(), symbol, lane, status };
        let mut buf = Vec::new();
        write_frame(&mut buf, &Response::Md(Box::new(frame))).expect("frame a Status");
        buf.len()
    }

    // 1. THE ENVELOPE, at every extreme this plane can reach: the longest `vike_model::VENUES`
    //    slug, the longest lane word, and the two-field `Stale` status with both stamps spelled at
    //    their widest (`i64::MIN` is 20 characters).
    let widest_venue =
        vike_model::VENUES.iter().max_by_key(|v| v.len()).expect("the roster is non-empty");
    let envelope = framed(
        widest_venue,
        String::new(),
        MdLane::Trades,
        WireStreamStatus::Stale { newest_data_ts_ms: i64::MIN, now_ms: i64::MIN },
    );
    assert_eq!(
        envelope, MD_STATUS_ENVELOPE_CEILING_BYTES,
        "the empty-symbol Status envelope is {envelope} B and the declared ceiling is \
         {MD_STATUS_ENVELOPE_CEILING_BYTES} B. This is pinned by EQUALITY: the symbol bound was \
         DERIVED from `MD_CTRL_FRAME_CEILING_BYTES - this`, so moving it either way means \
         re-deriving MD_MAX_SYMBOL_BYTES rather than editing this number"
    );

    // 2. THE SYMBOL, at the bound and at serde_json's worst control-free expansion. Every `"`
    //    costs two bytes, which is the factor the assertion in `md/mod.rs` budgets for.
    let worst = framed(
        widest_venue,
        "\"".repeat(MD_MAX_SYMBOL_BYTES),
        MdLane::Trades,
        WireStreamStatus::Stale { newest_data_ts_ms: i64::MIN, now_ms: i64::MIN },
    );
    assert!(
        worst <= MD_CTRL_FRAME_CEILING_BYTES,
        "a symbol at MD_MAX_SYMBOL_BYTES made entirely of quote characters frames to {worst} B \
         against a declared ceiling of {MD_CTRL_FRAME_CEILING_BYTES} B"
    );
    assert!(
        worst > envelope,
        "floor: the symbol must actually be ON the frame, or this leg measures the envelope twice"
    );

    // 3. The original witness — the longest symbol any roster venue can actually spell.
    let real = framed(
        "polymarket",
        "7".repeat(78),
        MdLane::Book,
        WireStreamStatus::Stale { newest_data_ts_ms: 1_757_500_000_000, now_ms: 1_757_500_060_000 },
    );
    assert!(
        real <= MD_CTRL_FRAME_CEILING_BYTES,
        "the widest REAL Status is {real} B against a declared ceiling of \
         {MD_CTRL_FRAME_CEILING_BYTES} B — re-derive MD_MAILBOX_BYTES' assertion before raising it"
    );
}

// ------------------------------------------------------------------------------------------------
// The resident set is CHECKED and CAPPED
// ------------------------------------------------------------------------------------------------

/// **A resident row that cannot be served is refused AT DECLARATION, not retried forever.**
///
/// `md::parse_resident_set` validates only the three-field shape and the lane word, so
/// `notavenue:X:depth` and a real venue on a lane its declared caps do not serve both PARSE. Before
/// this, `add_resident` inserted them anyway — and a resident entry is permanently `wanted`, so
/// `reconcile` phase 1 retried it every pass and the `md-reconcile` loop logged the failure every
/// `MD_REAP_INTERVAL` for the life of the process, with no line naming the row.
///
/// The fourth leg is the non-vacuity floor: a GOOD row on the same hub is still accepted.
#[test]
fn a_resident_row_that_cannot_be_served_is_refused_by_name() {
    let log = Log::default();
    let h = hub(&log);

    let bad_venue = h.add_resident(&spec("notavenue", "X", MdLane::Depth));
    assert!(bad_venue.is_err(), "an unknown venue must be refused");

    let unserved = h.add_resident(&spec("okx", "BTCUSDT.P", MdLane::Depth));
    assert!(unserved.is_err(), "a real venue this build does not link must be refused");

    let bad_lane = h.add_resident(&spec("binance", "BTCUSDT.P", MdLane::Book));
    let why = bad_lane.expect_err("binance declares book: false — the matrix must refuse it");
    assert!(why.contains("book") || why.contains("Book"), "the refusal names the lane: {why}");

    h.add_resident(&spec("binance", "BTCUSDT.P", MdLane::Depth))
        .expect("floor: a GOOD row on the same hub is still accepted");
    // ...and nothing unservable is in the registry to be retried: the ONE reconcile pass subscribes
    // exactly the good row.
    let r = h.reconcile(now());
    assert!(r.failed.is_empty(), "a refused row must not become an endless retry: {r:?}");
    assert_eq!(r.started, 1, "exactly the good row: {r:?}");
}

/// **The resident set is BOUNDED — per venue and in total.**
///
/// `MD_MAX_KEYS_PER_VENUE`'s own doc says "RESIDENT INCLUDED" and only `acquire` kept it, so a fat
/// `VIKE_DATAHUB_LIVE_RESIDENT` armed N venue subscriptions with N unbounded: memory (§12.6's hub
/// term, the LARGER of the two in the 32 MB claim) and venue budget (200 resident binance depth keys
/// is ~2,000 weight/min of re-seed against a 2,400/min IP budget shared with the order-signing
/// daemon).
#[test]
fn the_resident_set_is_capped_per_venue_and_in_total() {
    use vike_datahub::md::{MD_MAX_KEYS_PER_VENUE, MD_MAX_KEYS_RESIDENT};
    let log = Log::default();
    let h = hub(&log);

    // Fill ONE venue to its cap on a single lane, then ask for one more.
    let mut pinned = 0u32;
    for i in 0..MD_MAX_KEYS_PER_VENUE {
        h.add_resident(&spec("binance", &format!("SYM{i}"), MdLane::Depth))
            .unwrap_or_else(|e| panic!("row {i} must fit inside the cap: {e}"));
        pinned += 1;
    }
    assert_eq!(pinned, MD_MAX_KEYS_PER_VENUE, "floor: the cap was actually reached");
    let why = h
        .add_resident(&spec("binance", "ONE_TOO_MANY", MdLane::Depth))
        .expect_err("the per-venue cap must refuse the next one");
    assert!(why.contains("MD_MAX_KEYS_PER_VENUE"), "the refusal names its number: {why}");

    // ...and the TOTAL cap bites on a DIFFERENT venue, which the per-venue cap cannot see. The rows
    // above already spent the process-wide budget (the two constants are equal today, so one full
    // venue is one full process — the assertion is on WHICH cap answers, not on the arithmetic).
    let why = h
        .add_resident(&spec("polymarket", "TOKEN", MdLane::Book))
        .expect_err("the total resident cap must refuse a row on an EMPTY venue");
    assert!(why.contains("MD_MAX_KEYS_RESIDENT"), "the refusal names its number: {why}");
    assert!(pinned >= MD_MAX_KEYS_RESIDENT, "floor: the total budget was actually spent");
}

// ------------------------------------------------------------------------------------------------
// The depth fold comes back DOWN
// ------------------------------------------------------------------------------------------------

/// **A key's depth is refolded from the subscribers that REMAIN**, so one client's ceiling-depth
/// request does not retire the default for everyone else.
///
/// `StreamEntry::depth` was a `fetch_max` and nothing else. Since `publish_tick` serializes ONCE per
/// key at that depth, a single deep subscriber inflated the frame for EVERY subscriber of the key —
/// §12.4's measured 2.74 KB became 10.48 KB — and it never came back down. On a RESIDENT key, which
/// is never reaped, that was permanent for the life of the process.
///
/// ⚠ The resident leg is the one that matters most and the one a `fetch_max` cannot pass: the
/// operator's DECLARED depth must survive as a floor while the deep client's request does not.
#[test]
fn a_deep_subscriber_leaving_gives_the_key_its_shallow_depth_back() {
    let log = Log::default();
    let h = hub(&log);
    let res = MdSpec { depth_levels: Some(10), ..spec("binance", "BTCUSDT.P", MdLane::Depth) };
    h.add_resident(&res).expect("a served venue on a supported lane");

    let shallow = MdSpec { depth_levels: Some(20), ..res.clone() };
    let deep = MdSpec { depth_levels: Some(200), ..res.clone() };
    let mut a = h.open_session().unwrap();
    let mut b = h.open_session().unwrap();
    h.acquire(a.id(), &shallow).unwrap();
    h.acquire(b.id(), &deep).unwrap();
    h.reconcile(now());

    let bids: Vec<(f64, f64)> = (0..300).map(|i| (100.0 - i as f64, 1.0)).collect();
    let asks: Vec<(f64, f64)> = (0..300).map(|i| (200.0 + i as f64, 1.0)).collect();
    let cut = |g: &vike_datahub::md::SessionGuard| -> usize {
        h.sink().l2_snapshot("binance", "BTCUSDT.P", 0.1, bids.clone(), asks.clone(), 1);
        h.publish_tick();
        drain(g.mailbox())
            .into_iter()
            .find_map(|f| match f {
                MdFrame::Depth(bk) => Some(bk.bids.len()),
                _ => None,
            })
            .expect("a depth frame")
    };

    assert_eq!(cut(&a), 200, "while the deep subscriber is here, the key is cut at ITS depth");
    // The deep one leaves. The shallow one's frame must come back DOWN.
    b.release_at(now());
    assert_eq!(cut(&a), 20, "the fold is over the subscribers that REMAIN, not a high-water mark");
    // ...and the RESIDENT floor survives the last client leaving.
    a.release_at(now());
    let mut c = h.open_session().unwrap();
    h.acquire(c.id(), &MdSpec { depth_levels: Some(5), ..res.clone() }).unwrap();
    assert_eq!(cut(&c), 10, "the operator's DECLARED resident depth is a floor");
    c.release_at(now());
}

// ------------------------------------------------------------------------------------------------
// The SYMBOL is validated — the fifth field, which nothing looked at
// ------------------------------------------------------------------------------------------------

/// **An OVER-LENGTH symbol is refused at the door and makes no venue call.**
///
/// Before this, `MdHub::acquire` validated the venue, whether this build serves it, the lane and
/// three caps — and never the symbol. A client subscribing with a 10,000-character symbol got an
/// `Ok`: the key entered the registry, `poke()` woke the reconciler, and the next pass called
/// `subscribe_depth(venue, <10 KB>)` on the real venue. The ctrl frame that key then produces is
/// far over `vike_datahub::md::MD_CTRL_FRAME_CEILING_BYTES`, which is a TERM in `MD_MAILBOX_BYTES`'
/// compile-time assertion — so one wire field invalidated the 32 MB ceiling
/// `docs/decisions/0052-a-market-data-subscription-is-an-observe-verb.md` rests on.
///
/// `log.all() == Vec::new()` after a `reconcile` is this suite's existing proof that a refusal
/// happened BEFORE any venue call — the shape
/// `an_unsupported_lane_is_refused_with_the_matrixs_own_words_and_no_venue_call` already uses.
#[test]
fn an_over_length_symbol_is_refused_at_the_door_and_makes_no_venue_call() {
    use vike_datahub_client::market::{MD_MAX_SYMBOL_BYTES, MdRefusal};
    let log = Log::default();
    let h = hub(&log);
    let mut g = h.open_session().unwrap();

    let huge = "A".repeat(10_000);
    let err = h
        .acquire(g.id(), &spec("binance", &huge, MdLane::Depth))
        .expect_err("an unbounded symbol is a per-request cost the CLIENT names");
    let MdRefusal::SymbolRejected(why) = &err else {
        panic!("expected MdRefusal::SymbolRejected, got {err:?}");
    };
    assert!(why.contains("10000"), "the refusal names the length it was given: {why}");
    assert!(why.contains(&MD_MAX_SYMBOL_BYTES.to_string()), "...and the cap it exceeded: {why}");
    assert!(
        !why.contains(&huge),
        "...and does NOT echo the symbol back — a refusal that quotes an unbounded field is the \
         same unbounded cost wearing a log line: {} chars",
        why.len()
    );
    assert!(
        err.is_permanent(),
        "a symbol this long can never become legal — the client must not hold it in a desired set \
         and retry it forever"
    );

    // ...and the bound is a BOUND, not a magnitude check: one byte over is refused too.
    let over_by_one = "A".repeat(MD_MAX_SYMBOL_BYTES + 1);
    assert!(
        matches!(
            h.acquire(g.id(), &spec("binance", &over_by_one, MdLane::Depth)),
            Err(MdRefusal::SymbolRejected(_))
        ),
        "MD_MAX_SYMBOL_BYTES + 1 must be refused"
    );

    h.reconcile(now());
    assert_eq!(
        log.all(),
        Vec::new(),
        "a refused spec must open NO venue client and make NO subscribe call: {:?}",
        log.all()
    );
    g.release_at(now());
}

/// **A BLANK symbol is refused** — the sibling of the blank `--produced-by` this PR closes on the
/// delete verb, wearing the other hat. `acquire` accepted `symbol: ""`, the key was admitted, and
/// `reconcile` phase 1 called `subscribe_depth("")` on the real venue.
///
/// ⚠ It is also an ASYMMETRY this removes: `vike_datahub::md::parse_resident_set` has always
/// refused an empty symbol in the operator's OWN declaration, while the network client's request
/// was not checked at all — the daemon judged itself more strictly than it judged a stranger.
#[test]
fn a_blank_symbol_is_refused() {
    use vike_datahub_client::market::MdRefusal;
    let log = Log::default();
    let h = hub(&log);
    let mut g = h.open_session().unwrap();

    for blank in ["", "   ", "\t"] {
        match h.acquire(g.id(), &spec("binance", blank, MdLane::Depth)) {
            Err(MdRefusal::SymbolRejected(why)) => {
                assert!(why.contains("BLANK"), "the refusal names what was wrong: {why}");
            }
            other => panic!("blank {blank:?} must be refused, got {other:?}"),
        }
    }

    h.reconcile(now());
    assert_eq!(log.all(), Vec::new(), "{:?}", log.all());
    g.release_at(now());
}

/// **An ASCII CONTROL BYTE in a symbol is refused**, and that rule is part of the BOUND rather than
/// hygiene: serde_json escapes a byte below `0x20` as a six-byte `\u00XX`, so without this rule the
/// worst-case expansion factor `MD_MAX_SYMBOL_BYTES` was derived against is 6 rather than 2 and the
/// derivation does not hold. The rule is deliberately the NARROWEST one that makes it hold — space,
/// the quote character, the backslash and every non-ASCII byte stay legal, because a venue this
/// plane does not yet serve may spell a real instrument with them (an IBKR OSI local symbol carries
/// padding spaces).
#[test]
fn a_control_byte_in_a_symbol_is_refused() {
    use vike_datahub_client::market::MdRefusal;
    let log = Log::default();
    let h = hub(&log);
    let mut g = h.open_session().unwrap();

    let err = h
        .acquire(g.id(), &spec("binance", "BTC\u{1}USDT", MdLane::Depth))
        .expect_err("a control byte breaks the escape factor the bound was derived against");
    assert!(matches!(err, MdRefusal::SymbolRejected(_)), "{err:?}");

    // ...and the NON-vacuity floor for the narrowness: a space is legal.
    h.acquire(g.id(), &spec("binance", "BTC USDT", MdLane::Depth))
        .expect("a space is NOT a control byte — refusing it would refuse a real OSI symbol");

    g.release_at(now());
}

/// **THE FLOOR THE BOUND MUST CLEAR** — the longest symbol this wire can actually carry is still
/// ACCEPTED, and still produces a real venue subscription.
///
/// A polymarket CLOB token id is a `uint256` spelled in decimal, and `2^256 - 1` is exactly 78
/// digits — so 78 is a MAXIMUM by construction rather than a measurement, and §12.3 of
/// `docs/superpowers/specs/2026-09-09-datahub-market-data-wire-design.md` could not have measured a
/// 79. This is the leg that would catch a bound chosen under it, which is the way a length limit
/// goes wrong in the direction nobody tests.
#[test]
fn a_polymarket_token_id_at_the_hard_maximum_is_still_accepted() {
    let log = Log::default();
    let h = hub(&log);
    let mut g = h.open_session().unwrap();

    let token = "7".repeat(78);
    h.acquire(g.id(), &spec("polymarket", &token, MdLane::Book))
        .expect("78 digits is the widest a uint256 token id can be — it must be SERVED");
    let r = h.reconcile(now());
    assert_eq!(r.started, 1, "...and it really subscribes: {r:?}");
    assert!(
        log.all().contains(&Call::Book("polymarket".into(), token.clone())),
        "the venue was asked for the symbol VERBATIM: {:?}",
        log.all()
    );
    g.release_at(now());
}

/// **The operator's OWN resident declaration faces the same validator**, so the daemon cannot admit
/// through `VIKE_DATAHUB_LIVE_RESIDENT` what it refuses on the wire. The refusal stays a `String`
/// there for the reason `MdHub::add_resident`'s own doc gives: a resident row never crosses the
/// wire, and a wire variant nothing can see would widen a client-facing classification.
#[test]
fn a_resident_row_faces_the_same_symbol_validator() {
    use vike_datahub_client::market::MD_MAX_SYMBOL_BYTES;
    let log = Log::default();
    let h = hub(&log);

    let why = h
        .add_resident(&spec("binance", &"A".repeat(MD_MAX_SYMBOL_BYTES + 1), MdLane::Depth))
        .expect_err("a resident row is a subscription like any other");
    assert!(why.contains(&MD_MAX_SYMBOL_BYTES.to_string()), "{why}");

    // The floor: a GOOD row on the same hub is still accepted, and nothing unservable is left to
    // be retried forever.
    h.add_resident(&spec("binance", "BTCUSDT.P", MdLane::Depth)).expect("a good row");
    let r = h.reconcile(now());
    assert!(r.failed.is_empty(), "{r:?}");
    assert_eq!(r.started, 1, "{r:?}");
}

// ------------------------------------------------------------------------------------------------
// T16 — the SECOND door into the registry: `MdUpdate` on an already-live session
// ------------------------------------------------------------------------------------------------

/// Seed one depth key with a book AND a `Live` status, so an attach for it legitimately owes a
/// snapshot rather than a status alone. Without the status leg `attach_frames` returns status-only
/// by contract and the tests below would prove nothing about the book.
fn seed_live_depth(h: &MdHub, venue: &str, symbol: &str, levels: usize) {
    let sink = h.sink();
    let bids: Vec<(f64, f64)> = (0..levels).map(|i| (100.0 - i as f64, 1.0)).collect();
    let asks: Vec<(f64, f64)> = (0..levels).map(|i| (200.0 + i as f64, 1.0)).collect();
    sink.l2_snapshot(venue, symbol, 0.1, bids, asks, 7);
    sink.stream_status(venue, symbol, "depth", StreamStatus::Live { gap_started_ts_ms: None });
}

/// Where a `Status` naming exactly this key sits in a drained run, if it is there at all.
///
/// ⚠ Matched on the KEY FIELDS, never on the frame kind alone: this repository has been bitten by
/// an assertion that matched a substring of the wrong answer, and every test below runs on a hub
/// holding a SECOND key whose frames are the wrong answer in exactly that way.
fn status_at(frames: &[MdFrame], venue: &str, symbol: &str, lane: MdLane) -> Option<usize> {
    frames.iter().position(|f| match f {
        MdFrame::Status { venue: v, symbol: s, lane: l, .. } => {
            v.as_str() == venue && s.as_str() == symbol && *l == lane
        }
        _ => false,
    })
}

/// Where a `Depth` snapshot naming exactly this key sits in a drained run, if it is there at all.
fn depth_at(frames: &[MdFrame], venue: &str, symbol: &str) -> Option<usize> {
    frames.iter().position(|f| match f {
        MdFrame::Depth(bk) => bk.venue == venue && bk.symbol == symbol,
        _ => false,
    })
}

/// **A KEY ADDED TO AN ALREADY-LIVE SESSION IS ATTACHED IMMEDIATELY — it does not wait for that
/// venue's next update.**
///
/// There are TWO doors into the registry and until this landed only one of them attached.
/// `MdSubscribe` goes through `crate::server`'s `run_market_writer`, which pushes `attach_frames`
/// per accepted key before it enters its drain loop; `MdUpdate` — **which is the path every DOM
/// window after the first takes** (`crates/vike-app-core/src/md_session.rs`'s `push_update`, on a
/// fresh short-lived connection, because the stream socket has left its read loop) — went through
/// `MdHub::update`, which bumped the refcount and pushed nothing.
///
/// `publish_tick` skips any entry whose `dirty` bit is clear and `acquire` never sets it, so the
/// adding session received **no status and no snapshot until that venue's next update** — seconds
/// to minutes on a quiet polymarket instrument, and the whole of §12.5's reconnect state on binance
/// depth. Worse than blank: `MdSession::gapped` is populated only by a `Status` frame, so a key that
/// receives NOTHING is not gapped and the Connections tool counts it LIVE over an empty ladder —
/// precisely the failure that field's own doc says it exists to prevent, reached through a door
/// that doc did not know about.
///
/// ⚠ **Neither `publish_tick` nor the sink is touched after the update.** That is the whole
/// assertion: a fix that marks the key dirty instead of attaching it delivers at the next tick, in
/// the wrong ORDER (no `Status` — `publish_tick` emits one only when `status_dirty` is set), with no
/// `Live` gate on the book, and nothing at all when the key has no book yet.
///
/// Three non-vacuity floors, because "a frame arrived" is cheap to satisfy by accident:
/// 1. the ordinary path DID deliver first (`publish_tick` filled both mailboxes);
/// 2. both mailboxes were drained EMPTY going in, so nothing below can be a leftover;
/// 3. the frames are matched on this key's venue/symbol/lane, not on kind — session A is holding a
///    second key whose frames would satisfy a kind-only assertion.
///
/// Mutation: delete the attach push from `MdHub::update` -> red on the missing `Status`.
#[test]
fn a_key_added_to_a_live_session_attaches_without_waiting_for_a_venue_update() {
    let log = Log::default();
    let h = hub(&log);
    let one = spec("binance", "BTCUSDT.P", MdLane::Depth);
    let two = spec("binance", "ETHUSDT.P", MdLane::Depth);

    let mut a = h.open_session().unwrap();
    let mut b = h.open_session().unwrap();
    h.acquire(a.id(), &one).expect("binance depth is servable");
    h.acquire(b.id(), &two).expect("binance depth is servable");
    h.reconcile(now());
    seed_live_depth(&h, "binance", "BTCUSDT.P", 4);
    seed_live_depth(&h, "binance", "ETHUSDT.P", 4);
    h.publish_tick();

    // Floor 1 — the ordinary path works on this harness, so an absence below means something.
    assert!(
        !drain(a.mailbox()).is_empty(),
        "the FIRST window's key must flow on the ordinary path, or every assertion below is vacuous"
    );
    assert!(!drain(b.mailbox()).is_empty(), "...and so must the second session's");
    // Floor 2 — nothing is left over, so the frames drained after the update cannot be leftovers.
    assert!(a.mailbox().is_empty(), "session A's mailbox must be drained EMPTY going in");
    assert!(b.mailbox().is_empty(), "session B's mailbox must be drained EMPTY going in");

    // THE SECOND DOM WINDOW. No venue update follows it and `publish_tick` is NOT called again.
    let (accepted, refused, released) = h.update(a.id(), std::slice::from_ref(&two), &[], now());
    assert_eq!(accepted.len(), 1, "the add was accepted: {accepted:?} / refused {refused:?}");
    assert!(released.is_empty(), "{released:?}");

    let frames = drain(a.mailbox());
    let s = status_at(&frames, "binance", "ETHUSDT.P", MdLane::Depth).unwrap_or_else(|| {
        panic!(
            "the ADDING session received no Status for the key it just added — a DOM window after \
             the first paints a blank ladder while the Connections tool reads it as live: {frames:?}"
        )
    });
    let d = depth_at(&frames, "binance", "ETHUSDT.P").unwrap_or_else(|| {
        panic!("...and no snapshot either, though the hub holds a LIVE book for it: {frames:?}")
    });
    assert!(s < d, "§5.2 step 5: the STATUS comes first, then the book: {frames:?}");

    // ⚠ AND IT IS NOT A BROADCAST. Session B already holds this key and asked for nothing, so it
    // receives nothing — the §6.1 guard that separates this fix from marking the key dirty, which
    // would republish the book to every subscriber of it on the next tick.
    assert!(
        b.mailbox().is_empty(),
        "a session that already held the key was republished to: {:?}",
        drain(b.mailbox())
    );
    a.release_at(now());
    b.release_at(now());
}

/// **A DUPLICATE `add` OF A KEY THIS SESSION ALREADY HOLDS AT THE SAME DEPTH PUSHES NOTHING** — the
/// bound that keeps the attach above from being #1753 wearing a new producer.
///
/// `MdHub::acquire` returns `Ok` for a key the session already holds (`already_held_by_session`
/// only suppresses the refcount bump), so `update`'s add loop cannot tell a duplicate from a new
/// key by the result alone. An attach push per ACCEPTED spec therefore costs one CTRL frame per
/// spec SENT rather than per state CHANGE — and `vike_datahub::md::MD_MAILBOX_CTRL` is 128 while a
/// request may legally name 64. Three such requests back to back, with no stall, no slow link and
/// no venue event, set `must_close` and the peer gets `MdBye::ControlLaneOverflow` — which is
/// `docs/decisions/0052`'s decision 2 (*"bounded by server constants, never by the request"*)
/// false again on exactly the term #1753 restored.
///
/// The gate is that the push follows a CHANGE to this session's holding, not an acceptance.
///
/// ⚠ Non-vacuity: the session must genuinely hold a MULTI-key set that was genuinely delivered
/// first, or "nothing arrived" passes against a hub that attaches nothing at all.
#[test]
fn a_duplicate_add_of_a_held_key_pushes_nothing_and_cannot_overflow_the_control_lane() {
    use vike_datahub::md::{MD_MAILBOX_CTRL, MD_MAX_KEYS_PER_VENUE};

    let log = Log::default();
    let h = hub(&log);
    let mut g = h.open_session().unwrap();
    let mut held = Vec::new();
    for (venue, lanes) in [
        ("binance", [MdLane::Depth, MdLane::Trades]),
        ("bybit", [MdLane::Depth, MdLane::Trades]),
        ("polymarket", [MdLane::Book, MdLane::Trades]),
    ] {
        for i in 0..8 {
            for lane in lanes {
                let s = spec(venue, &format!("SYM{i}"), lane);
                held.push(h.acquire(g.id(), &s).expect("a servable lane inside every cap"));
            }
        }
    }
    assert_eq!(held.len(), 3 * MD_MAX_KEYS_PER_VENUE as usize, "the harness must reach a big set");
    h.reconcile(now());

    // The floor: this session's keys DO deliver, so an empty mailbox below is a decision rather
    // than a broken harness.
    for i in 0..8 {
        seed_live_depth(&h, "binance", &format!("SYM{i}"), 2);
    }
    h.publish_tick();
    assert!(!drain(g.mailbox()).is_empty(), "the ordinary path delivered nothing — vacuous");
    assert!(g.mailbox().is_empty(), "drained empty going in");

    // Three full re-sends of the SAME already-held set. 3 x 48 = 144 CTRL frames against a lane of
    // 128 if every accepted spec were attached.
    assert!(3 * held.len() > MD_MAILBOX_CTRL, "the floor: the burst must EXCEED the ctrl lane");
    for round in 1..=3 {
        let (accepted, refused, released) = h.update(g.id(), &held, &[], now());
        assert_eq!(accepted.len(), held.len(), "round {round}: still accepted {refused:?}");
        assert!(released.is_empty(), "round {round}: {released:?}");
        assert!(
            !g.mailbox().must_close(),
            "round {round}: a re-send of the session's OWN held set closed its connection"
        );
        assert!(
            g.mailbox().is_empty(),
            "round {round}: a duplicate add pushed frames for keys nothing changed about: {:?}",
            drain(g.mailbox())
        );
    }
    g.release_at(now());
}

/// **A SESSION JOINING AN EXISTING KEY AT A LARGER DEPTH SEES THE DEEPER BOOK IMMEDIATELY** — the
/// depth half of the same defect, closed by the same push and with no `mark_dirty` anywhere.
///
/// `MdHub::acquire` raises `StreamEntry::depth` and settles it, and NEITHER marks the entry
/// dirty — so without an attach the joining session waits for that venue's next update before it
/// sees the cut it asked for. `attach_frames` cuts its snapshot at `effective_depth()`, which
/// `acquire` has already raised by the time the push runs, so the deeper frame rides the same
/// attach that fixes the blank ladder.
///
/// ⚠ **The residual is DECLARED, not closed, and it is asserted here rather than left implied**:
/// the key's OTHER subscribers keep receiving the previous cut until the next venue update. They
/// asked for LESS — depth is a per-key max — so a windfall arriving one tick late is not a loss,
/// and marking the entry dirty to deliver it would republish the key to every subscriber of it on
/// every window open and every window close, which is the §6.1 cost this whole fix is shaped to
/// avoid. `StreamEntry::settle_depth`'s own doc carries the argument.
#[test]
fn a_session_joining_an_existing_key_deeper_attaches_at_the_deeper_cut() {
    let log = Log::default();
    let h = hub(&log);
    let shallow = MdSpec { depth_levels: Some(5), ..spec("binance", "BTCUSDT.P", MdLane::Depth) };
    let deep = MdSpec { depth_levels: Some(60), ..shallow.clone() };

    let mut b = h.open_session().unwrap();
    h.acquire(b.id(), &shallow).unwrap();
    h.reconcile(now());
    seed_live_depth(&h, "binance", "BTCUSDT.P", 100);
    h.publish_tick();

    // The floor: B's own frame is cut at FIVE, so 60 below is a different number arrived at by the
    // fold rather than the harness's default.
    let first = drain(b.mailbox());
    match first.iter().find(|f| matches!(f, MdFrame::Depth(_))) {
        Some(MdFrame::Depth(bk)) => assert_eq!(bk.bids.len(), 5, "B's own cut: {first:?}"),
        _ => panic!("B received no book on the ordinary path: {first:?}"),
    }
    assert!(b.mailbox().is_empty(), "drained empty going in");

    let mut a = h.open_session().unwrap();
    let (accepted, refused, _) = h.update(a.id(), std::slice::from_ref(&deep), &[], now());
    assert_eq!(accepted.len(), 1, "{accepted:?} / {refused:?}");

    let frames = drain(a.mailbox());
    let d = depth_at(&frames, "binance", "BTCUSDT.P")
        .unwrap_or_else(|| panic!("the deeper joiner received no snapshot at all: {frames:?}"));
    match &frames[d] {
        MdFrame::Depth(bk) => assert_eq!(
            bk.bids.len(),
            60,
            "the joiner was handed the PREVIOUS cut and must wait for a venue update for the depth \
             it asked for: {frames:?}"
        ),
        other => panic!("{other:?}"),
    }
    // The declared residual, pinned: the shallow holder is not republished to.
    assert!(
        b.mailbox().is_empty(),
        "the key's OTHER subscriber was republished to: {:?}",
        drain(b.mailbox())
    );
    a.release_at(now());
    b.release_at(now());
}

/// **A REMOVED KEY'S QUEUED BOOK LEAVES THE MAILBOX WITH IT.**
///
/// `vike_datahub::md::mailbox::Mailbox`'s BOOK lane is the one lane nothing can evict —
/// `enforce_bounds` evicts only from the tape, and its comment says why: *"`MD_MAILBOX_BYTES`'
/// compile-time assertion is what guarantees a full session of ceiling-depth books fits"*. That
/// assertion multiplies `MD_FRAME_CEILING_BYTES` by `MD_MAX_SPECS_PER_SESSION`, so it rests
/// entirely on the invariant `book_slots.len() <= MD_MAX_SPECS_PER_SESSION` — and `update`'s
/// remove path broke it: a removed key's already-queued slot was never dropped, so a single
/// `remove 64 + add 64` left 128 unevictable slots, 1.86x the byte bound, with nothing able to
/// reclaim them. Attaching on update makes it reachable INSIDE one request rather than one tick
/// later, which is why it is closed here rather than declared.
///
/// It is also the right answer on its own terms: a book for a key the client has just unsubscribed
/// is routed into `crates/vike-app-core/src/md_session.rs`'s `BookStore` by `(venue, symbol)`
/// regardless of the served set, repopulating a store entry the window that wanted it just dropped.
///
/// ⚠ The TAPE is deliberately left alone — it is bounded and evictable, so it cannot break the
/// assertion, and the mailbox's owed gaps are a disclosure that must survive.
///
/// ⚠ **WHAT THIS TEST CAN AND CANNOT SEE.** It drives `update` and `publish_tick` sequentially on
/// one thread, so it proves the RECLAIM and nothing about the race beside it: reclaiming a slot is
/// worth nothing while a publish tick can enqueue a new one for the same key a moment later, and a
/// per-tick target snapshot says the session still holds the key for the whole remainder of that
/// tick. The other half is `MdHub::fanout`, which re-reads the session table under that table's own
/// lock at push time — a LOCK-ORDERING property, provable by reading the two call sites and not by
/// a single-threaded assertion, which is why it is stated on that function rather than pretended at
/// here. A timing test for it would flake on a loaded runner and, if it ever regressed, would hang
/// or pass at random rather than go red: the same reason `mailbox::PushOutcome` asserts
/// never-blocks as a TYPE property instead of with a stopwatch.
#[test]
fn a_removed_keys_queued_book_leaves_the_mailbox_with_it() {
    let log = Log::default();
    let h = hub(&log);
    let one = spec("binance", "BTCUSDT.P", MdLane::Depth);
    let two = spec("binance", "ETHUSDT.P", MdLane::Depth);
    let mut g = h.open_session().unwrap();
    h.acquire(g.id(), &one).unwrap();
    h.acquire(g.id(), &two).unwrap();
    h.reconcile(now());
    seed_live_depth(&h, "binance", "BTCUSDT.P", 4);
    seed_live_depth(&h, "binance", "ETHUSDT.P", 4);
    h.publish_tick();
    drain(g.mailbox());

    // A BOOK-ONLY re-dirty (no `stream_status`, so no ctrl frame rides along), then a tick with NO
    // drain after it: two book slots are queued, which is the state a writer one tick behind is in.
    let sink = h.sink();
    sink.l2_snapshot("binance", "BTCUSDT.P", 0.1, vec![(1.0, 1.0)], vec![(2.0, 1.0)], 8);
    sink.l2_snapshot("binance", "ETHUSDT.P", 0.1, vec![(1.0, 1.0)], vec![(2.0, 1.0)], 8);
    h.publish_tick();
    assert_eq!(
        g.mailbox().len(),
        2,
        "the floor: two book slots must be queued or the removal below proves nothing"
    );

    h.update(g.id(), &[], std::slice::from_ref(&two), now());

    let frames = drain(g.mailbox());
    assert!(
        depth_at(&frames, "binance", "ETHUSDT.P").is_none(),
        "a book for the key this session just REMOVED is still queued against the one bound nothing \
         can evict: {frames:?}"
    );
    assert!(
        depth_at(&frames, "binance", "BTCUSDT.P").is_some(),
        "...and the key it still holds must be untouched: {frames:?}"
    );
    g.release_at(now());
}

/// **ONE KEY NAMED SIXTY-FOUR TIMES AT SIXTY-FOUR DEPTHS IS ONE ATTACH** — the half of the
/// ctrl-lane bound that `MdHub::acquire_changed`'s `changed` gate cannot supply, because a spec
/// carries a depth and a KEY does not.
///
/// `MdKey::of` ignores `depth_levels` while `MdSpec::resolved_depth` maps every `Some(n)` in
/// `[1, MD_DEPTH_LEVELS_CEILING]` to its own number, so one key named at 64 distinct depths is 64
/// ACCEPTED specs — `already_held_by_session` suppresses the session cap for a key already held —
/// and 64 `changed == true` answers, each `insert` returning the PREVIOUS spec's depth. Attaching
/// from a `Vec` therefore pushes 64 CTRL frames for ONE key, and `crate::server`'s
/// `refuse_an_oversized_spec_list` is no defence: it caps the LENGTH at
/// `MD_MAX_SPECS_PER_SESSION` and leaves dedup to the client (*"drop any duplicate spec"* is advice
/// in the refusal text, not enforcement). Three such requests against a wedged writer reach 192 on
/// a 128-deep `MD_MAILBOX_CTRL` — the exact bound `acquire_changed`'s doc claims the `changed` gate
/// restores, breached through the term that gate does not cover.
///
/// The sibling test above re-sends an IDENTICAL set, where `insert` returns the same depth and the
/// `changed` gate alone holds. This one is the case that gate answers `true` to every time.
///
/// ⚠ The single attach must also carry the DEEPEST cut asked for, not the first or the last one
/// processed: `attach_frames` runs after the add loop and cuts at `effective_depth()`, which
/// `acquire` has by then folded to the max over the whole request. Asserting the COUNT without the
/// CUT would pass against a fix that pushed the wrong one of the 64.
///
/// Mutation: make `attach` a `Vec<MdKey>` again -> red on the frame count.
#[test]
fn one_key_named_at_many_depths_attaches_once_at_the_deepest_cut() {
    use vike_datahub::md::MD_MAX_SPECS_PER_SESSION;

    let log = Log::default();
    let h = hub(&log);
    let base = spec("binance", "BTCUSDT.P", MdLane::Depth);
    let mut g = h.open_session().unwrap();
    h.acquire(g.id(), &MdSpec { depth_levels: Some(1), ..base.clone() }).unwrap();
    h.reconcile(now());
    seed_live_depth(&h, "binance", "BTCUSDT.P", 120);
    h.publish_tick();

    // The floor: the ordinary path delivers on this harness, and it delivers the SHALLOW cut — so
    // the deep number below is arrived at by the fold rather than by the seed's own length.
    let first = drain(g.mailbox());
    match first.iter().find(|f| matches!(f, MdFrame::Depth(_))) {
        Some(MdFrame::Depth(bk)) => {
            assert_eq!(bk.bids.len(), 1, "the session's own cut: {first:?}")
        }
        _ => panic!("no book on the ordinary path — every assertion below is vacuous: {first:?}"),
    }
    assert!(g.mailbox().is_empty(), "drained empty going in");

    let deepest = MD_MAX_SPECS_PER_SESSION as u16 + 1;
    let many: Vec<MdSpec> =
        (2..=deepest).map(|d| MdSpec { depth_levels: Some(d), ..base.clone() }).collect();
    assert_eq!(many.len(), MD_MAX_SPECS_PER_SESSION as usize, "a request-legal list of one key");

    let (accepted, refused, released) = h.update(g.id(), &many, &[], now());
    assert_eq!(accepted.len(), many.len(), "every spec is accepted: {refused:?}");
    assert!(released.is_empty(), "{released:?}");
    assert!(
        !g.mailbox().must_close(),
        "one key at many depths closed the connection on its own control lane"
    );

    let frames = drain(g.mailbox());
    assert_eq!(
        frames.len(),
        2,
        "ONE key changed, so ONE status and ONE snapshot are owed — a push per accepted SPEC makes \
         the ctrl cost a function of what the client SENT rather than of what changed: {frames:?}"
    );
    let s = status_at(&frames, "binance", "BTCUSDT.P", MdLane::Depth)
        .unwrap_or_else(|| panic!("no Status for the key that changed: {frames:?}"));
    let d = depth_at(&frames, "binance", "BTCUSDT.P")
        .unwrap_or_else(|| panic!("no snapshot for the key that changed: {frames:?}"));
    assert!(s < d, "§5.2 step 5: the STATUS comes first, then the book: {frames:?}");
    match &frames[d] {
        MdFrame::Depth(bk) => assert_eq!(
            bk.bids.len(),
            deepest as usize,
            "the one attach must carry the DEEPEST cut the request asked for: {frames:?}"
        ),
        other => panic!("{other:?}"),
    }
    g.release_at(now());
}

/// **TWO SESSIONS HOLDING ONE KEY ARE BOTH SERVED BY ONE SERIALIZATION** — the fan-out guard for
/// `MdHub::fanout`, which replaced `publish_tick`'s per-tick target snapshot with a membership test
/// taken under the session table's own lock at push time.
///
/// The snapshot was stale for the whole remainder of a tick, which let a concurrent `MdUpdate`
/// remove re-create the book slot `MdHub::update` had just reclaimed — see `fanout`'s own doc for
/// why that is a bound and not a tidiness. The RACE is a lock-ordering property no single-threaded
/// test can see; what a test can hold is that the rewrite still reaches every holder of the key and
/// nobody else, which is the property a wrong membership test would break loudly.
///
/// ⚠ Non-vacuity: a THIRD session holds a different key on the same venue, so "pushed to everyone"
/// fails here rather than passing as a wider version of the right answer.
#[test]
fn one_serialization_reaches_every_holder_of_the_key_and_no_one_else() {
    let log = Log::default();
    let h = hub(&log);
    let shared = spec("binance", "BTCUSDT.P", MdLane::Depth);
    let other = spec("binance", "ETHUSDT.P", MdLane::Depth);

    let mut a = h.open_session().unwrap();
    let mut b = h.open_session().unwrap();
    let mut c = h.open_session().unwrap();
    h.acquire(a.id(), &shared).unwrap();
    h.acquire(b.id(), &shared).unwrap();
    h.acquire(c.id(), &other).unwrap();
    h.reconcile(now());
    seed_live_depth(&h, "binance", "BTCUSDT.P", 4);
    let r = h.publish_tick();

    let fa = drain(a.mailbox());
    let fb = drain(b.mailbox());
    let fc = drain(c.mailbox());
    assert!(depth_at(&fa, "binance", "BTCUSDT.P").is_some(), "A holds the key: {fa:?}");
    assert!(depth_at(&fb, "binance", "BTCUSDT.P").is_some(), "B holds it too: {fb:?}");
    assert!(
        depth_at(&fc, "binance", "BTCUSDT.P").is_none(),
        "C holds a DIFFERENT key and was served this one anyway: {fc:?}"
    );
    // §6.1: one serialization per dirty key per tick, however many hold it. The tick produced a
    // status and a book for ONE key — two frames — and handed the same `Arc` to both holders.
    assert_eq!(r.frames_produced, 2, "one status + one book, serialized once each: {r:?}");

    // ...and a session that GIVES the key up is not served it on the next tick.
    h.update(b.id(), &[], std::slice::from_ref(&shared), now());
    let sink = h.sink();
    sink.l2_snapshot("binance", "BTCUSDT.P", 0.1, vec![(1.0, 1.0)], vec![(2.0, 1.0)], 9);
    h.publish_tick();
    assert!(depth_at(&drain(a.mailbox()), "binance", "BTCUSDT.P").is_some(), "A still holds it");
    assert!(
        b.mailbox().is_empty(),
        "a session that removed the key was served it anyway: {:?}",
        drain(b.mailbox())
    );
    a.release_at(now());
    b.release_at(now());
    c.release_at(now());
}
