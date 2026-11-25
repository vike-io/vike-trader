//! **FXCM's rebuild after `SessionLost`** (decision 0164 verdict 7(b), amended 2026-11-21, the owner's "q1-q6 agree" to
//! PLAN.md Q4): the second user of the daemon's one session supervisor, chosen by its BRIDGE's declaration
//! (`VenueAdapter::link_probe`) and never by a venue name. The probe is the REAL `vike_fxcm::account::venue_link`'s
//! `FxcmSessionProbe` over a cell this test writes exactly as the exec thread does (`from_status` of the SDK's status), behind
//! the REAL `VenueLinkWatch` on an injected FX clock, supervised by the real session over a scripted lane whose start does
//! what fxcm's does to the cell (it claims it for its exec thread) and announces what its exec thread announces once its
//! session opens.
//!
//! What it pins: only a FINAL SDK state (`SessionLost`, `Disconnected`) takes the adapter down, and the SDK's own
//! `Reconnecting` never does; on a weekday the dead adapter is torn down, the lease ended at the loss and the session
//! rebuilt in a new epoch with a reconcile owed; a server that drops each login before it is up gets a GROWING wait; a
//! late word of the old exec thread takes the rebuilt adapter down never; at Friday's close and over the weekend NOTHING
//! is attempted while the market is closed, and the state says `FX market closed: login at <open>`; at the open ONE start
//! is made, then the supervisor's backoff, and a failed one says its own error, not the closed market; a bridge that
//! announces it learned the venue is not held, and one that never does is HELD.
//! Live proof is the FXCM demo at the FX open (Sunday 21:00 UTC); no SDK is linked here.
#![cfg(feature = "trade")]

use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use vike_bridge_core::venue_link::VenueLink;
use vike_execution::EventSender;
use vike_execution::recon::ReconcileClient;
use vike_fxcm::account::venue_link::{FxcmSessionProbe, from_status};
use vike_gatewayhub::budget::ledger::Budget;
use vike_gatewayhub::session::reconnect::VenueLinkReconnectSchedule;
use vike_gatewayhub::testkit::trade::{SocketlessHub, TradeLaneScript};
use vike_gatewayhub::testkit::trade_rig::{self, hub_config};
use vike_gatewayhub::trade::adapters::link_watch::{TierNow, VenueLinkWatch};
use vike_gatewayhub::trade::lane::{
    AdapterRefusal, StartedAdapter, TradeLane, TradeLaneTarget, VenueLinkWatchSource,
};
use vike_gatewayhub::trade::lease::{TradeLeaseHold, TradeLeaseHoldFlow, TradeLeaseHoldNext};
use vike_gatewayhub::trade::session::{
    AccountSessions, TradeLeaseOpenAcceptance, TradeLeaseOpenRefusal, TradeLeaseOpenRequest,
};
use vike_gatewayhub_client::proto::{
    AdapterReply, AdapterStart, ArmingTableWire, PreflightAnswer, PreflightStep, TradeDown, TradeUp,
};
use vike_log::poll::wait_until;
use vike_model::events::Event;
use vike_model::{AccountTier, ArmingBlock, Clock, EventRingCursor, OrderRequest};

const WAIT: Duration = Duration::from_secs(8);

/// A probe every 20 ms, the first failed restore waits 40 ms, growing to 160 ms, and one `Connected` sighting is healthy
/// (production's 30 s span is pinned in passes by `reconnect_tests.rs`'s `a_session_up_for_less_than_the_probation_climbs_the_backoff`).
const FAST: VenueLinkReconnectSchedule = VenueLinkReconnectSchedule {
    probe_every: Duration::from_millis(20),
    first_wait: Duration::from_millis(40),
    max_wait: Duration::from_millis(160),
    probation: Duration::ZERO,
};

/// How long a started bridge has to announce that it learned the venue before its session is held.
const LEARN_BUDGET: Duration = Duration::from_millis(200);

/// The words the FX week says while it is closed, naming Sunday 2026-11-22's open.
const CLOSED: &str = "FX market closed: login at 2026-11-22T21:00:00Z";

fn at(stamp: &str) -> i64 {
    vike_model::time::parse_utc_timestamp_ms(stamp).expect("an instant")
}

fn wednesday() -> i64 {
    at("2026-11-18T12:00:00Z")
}

// ---- the lane: fxcm's start, as it touches the cell and announces its learning -------------------------------

/// A lane over the scripted one whose start does what fxcm's does: it CLAIMS the account's cell for its exec thread (a
/// new exec thread's word is not known yet, and an earlier one's lands nowhere) and, when `learns`, its exec thread
/// announces `ExecLink{up:true}` once its session opens. The starts listed in `refuse` (1-based) do not come up, as a
/// start whose login never reached FXCM; while `drops`, a start's session is dropped by the server before it is up.
struct FxcmLane {
    inner: TradeLaneScript,
    cell: VenueLink,
    learns: AtomicBool,
    drops: AtomicBool,
    refuse: Vec<usize>,
    starts: Mutex<Vec<Instant>>,
    /// Each started exec thread's claim on the cell, in start order.
    writers: Mutex<Vec<VenueLink>>,
}

impl FxcmLane {
    fn attempts(&self) -> usize {
        self.starts.lock().unwrap_or_else(PoisonError::into_inner).len()
    }

    fn attempted_at(&self) -> Vec<Instant> {
        self.starts.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    /// The claim of the `n`th exec thread that started (0-based; a refused start has none).
    fn writer(&self, n: usize) -> VenueLink {
        self.writers.lock().unwrap_or_else(PoisonError::into_inner)[n].clone()
    }
}

impl TradeLane for FxcmLane {
    fn venues(&self) -> Vec<String> {
        self.inner.venues()
    }
    fn account_tier(&self, venue: &str, account: Option<&str>) -> Result<AccountTier, ArmingBlock> {
        self.inner.account_tier(venue, account)
    }
    fn arming_table(&self) -> ArmingTableWire {
        self.inner.arming_table()
    }
    fn preflight(&self, venues: &[String], step: PreflightStep) -> PreflightAnswer {
        self.inner.preflight(venues, step)
    }
    fn start_adapter(
        &self,
        target: &TradeLaneTarget,
        start: &AdapterStart,
        events: EventSender,
        trigger: Option<Sender<()>>,
    ) -> Result<StartedAdapter, AdapterRefusal> {
        let n = {
            let mut starts = self.starts.lock().unwrap_or_else(PoisonError::into_inner);
            starts.push(Instant::now());
            starts.len()
        };
        if self.refuse.contains(&n) {
            return Err(AdapterRefusal {
                block_word: "adapter failed".into(),
                refusal_text: "fxcm reconcile: the ForexConnect session did not open".into(),
            });
        }
        // `spawn_execution_client_reporting`: what an earlier client of the account said is history.
        let writer = self.cell.claim();
        self.writers.lock().unwrap_or_else(PoisonError::into_inner).push(writer.clone());
        let started = self.inner.start_adapter(target, start, events, trigger)?;
        if self.learns.load(Ordering::SeqCst) {
            let adapter = self.inner.adapter("fxcm", None).expect("the adapter just started");
            assert!(adapter.emit_execution_link(true), "the session hears the announcement");
        }
        if self.drops.load(Ordering::SeqCst) {
            writer.set(from_status(Some(6)), None);
        }
        Ok(started)
    }
    fn record_identity(
        &self,
        target: &TradeLaneTarget,
        bound_tier: AccountTier,
        recon: Option<&dyn ReconcileClient>,
    ) -> Result<(), String> {
        self.inner.record_identity(target, bound_tier, recon)
    }
    fn resolves_by_order_client_id(&self, venue: &str) -> bool {
        self.inner.resolves_by_order_client_id(venue)
    }
    fn link_watch(
        &self,
        target: &TradeLaneTarget,
        start: &AdapterStart,
    ) -> Option<Arc<dyn VenueLinkWatchSource>> {
        self.inner.link_watch(target, start)
    }
}

// ---- the rig ---------------------------------------------------------------------------------------------------

/// A running supervised fxcm session at `demo` on the FX clock `now`: the lease open and the adapter started.
struct Up {
    lane: Arc<FxcmLane>,
    hub: SocketlessHub,
    now: Arc<AtomicI64>,
    rows: Arc<Mutex<Result<AccountTier, ArmingBlock>>>,
    first: TradeLeaseOpenAcceptance,
}

impl Up {
    /// The latest exec thread's word on the account's cell, the SDK's `status`.
    fn sdk(&self, status: i32) {
        let writers = self.lane.writers.lock().unwrap_or_else(PoisonError::into_inner);
        writers.last().expect("a started exec thread").set(from_status(Some(status)), None);
    }

    fn set_clock(&self, stamp: &str) {
        self.now.store(at(stamp), Ordering::SeqCst);
    }

    fn adapter_up(&self) -> bool {
        self.hub.session_status("fxcm", None).is_some_and(|s| s.adapter_up)
    }
}

fn up_with(now: i64, learns: bool, refuse: &[usize]) -> Up {
    let cell = VenueLink::default();
    let now = Arc::new(AtomicI64::new(now));
    let rows = Arc::new(Mutex::new(Ok(AccountTier::Demo)));
    let tier_now: TierNow = {
        let rows = Arc::clone(&rows);
        Box::new(move || *rows.lock().unwrap_or_else(PoisonError::into_inner))
    };
    let clock: Arc<dyn Clock + Send + Sync> = {
        let now = Arc::clone(&now);
        Arc::new(move || now.load(Ordering::SeqCst))
    };
    // The REAL watch over the REAL fxcm probe, with the lane's calendar; only the cadence is the test's.
    let watch = VenueLinkWatch::new(Box::new(FxcmSessionProbe::new(cell.clone())), tier_now)
        .with_schedule(FAST)
        .with_calendar("fxcm", Some(clock));
    let lane = Arc::new(FxcmLane {
        inner: TradeLaneScript::new(&["fxcm"]).with_link_watch("fxcm", Arc::new(watch)),
        cell,
        learns: AtomicBool::new(learns),
        drops: AtomicBool::new(false),
        refuse: refuse.to_vec(),
        starts: Mutex::new(Vec::new()),
        writers: Mutex::new(Vec::new()),
    });
    let hub = SocketlessHub(AccountSessions::new(
        Arc::clone(&lane) as Arc<dyn TradeLane>,
        hub_config(),
        Arc::new(Budget::new()),
    ));
    // Read by each start's learning watch: a bridge that never announces is held after this.
    hub.set_learning_budget(LEARN_BUDGET);
    let first = open(&hub, "tradehub-1", None);
    assert!(matches!(start(&first.lease, 7), AdapterReply::Running { .. }));
    let up = Up { lane, hub, now, rows, first };
    up.sdk(3);
    assert!(up.adapter_up());
    up
}

fn up_at(now: i64) -> Up {
    up_with(now, true, &[])
}

fn request(client_id: &str, resume: Option<EventRingCursor>) -> TradeLeaseOpenRequest {
    TradeLeaseOpenRequest {
        client_id: client_id.to_string(),
        venue: "fxcm".to_string(),
        account: None,
        tier: AccountTier::Demo,
        resume,
        takeover: false,
        key_id: Some("nk-test".to_string()),
        closer: Arc::new(|| {}),
    }
}

fn open(
    hub: &AccountSessions,
    client_id: &str,
    resume: Option<EventRingCursor>,
) -> TradeLeaseOpenAcceptance {
    hub.open(request(client_id, resume)).expect("the open is accepted")
}

fn refusal_text(answer: Result<TradeLeaseOpenAcceptance, TradeLeaseOpenRefusal>) -> String {
    match answer {
        Err(TradeLeaseOpenRefusal::Error(text)) => text,
        Err(other) => panic!("expected a plain refusal with words, got {other:?}"),
        Ok(_) => panic!("the open was accepted"),
    }
}

fn start(lease: &Arc<TradeLeaseHold>, rid: u64) -> AdapterReply {
    let (got, reply) = crate::adapter_start(lease, "fxcm", rid, false, Duration::from_secs(2));
    assert_eq!(got, rid);
    reply
}

fn order(order_client_id: &str) -> OrderRequest {
    OrderRequest { venue: "fxcm".into(), ..trade_rig::order(order_client_id) }
}

/// Reads a lease's frames on its own thread (the server's connection does) until its `Bye`.
fn drain(lease: Arc<TradeLeaseHold>) -> (Arc<Mutex<Vec<TradeDown>>>, std::thread::JoinHandle<()>) {
    let frames = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&frames);
    let thread = std::thread::spawn(move || {
        loop {
            match lease.next(Duration::from_millis(50)) {
                TradeLeaseHoldNext::Frame(f) => {
                    sink.lock().unwrap_or_else(PoisonError::into_inner).push(f)
                }
                TradeLeaseHoldNext::Bye(_) | TradeLeaseHoldNext::Closed => {
                    lease.finish();
                    return;
                }
                TradeLeaseHoldNext::Idle => {}
            }
        }
    });
    (frames, thread)
}

fn rejected_with(frames: &Mutex<Vec<TradeDown>>, needle: &str) -> bool {
    frames.lock().unwrap_or_else(PoisonError::into_inner).iter().any(|f| {
        matches!(f, TradeDown::Event { event: Event::OrderRejected(r), .. } if r.reason.contains(needle))
    })
}

// ---- the weekday rebuild ---------------------------------------------------------------------------------------

/// **Only a FINAL state is a loss**: up, `Connecting` and the SDK's own `Reconnecting` leave the adapter alone (the
/// SDK's reconnect is never raced); `SessionLost` on a weekday tears it down at once, ends the lease AT THE LOSS,
/// refuses a redial with words while it is down, and rebuilds the adapter in a NEW epoch with a reconcile owed.
#[test]
fn a_session_lost_on_a_weekday_is_rebuilt_in_a_new_epoch_and_reconnecting_never_is() {
    let up = up_at(wednesday());
    let epoch = up.first.reply.gateway_epoch;
    let a1 = up.lane.inner.adapter("fxcm", None).expect("the first adapter");
    let (_frames, conn) = drain(Arc::clone(&up.first.lease));

    for status in [1, 4, 7, 3] {
        up.sdk(status);
        std::thread::sleep(FAST.probe_every * 10);
        assert!(!a1.is_detached() && up.adapter_up(), "status {status} takes no adapter down");
    }
    assert_eq!(up.lane.attempts(), 1);

    up.sdk(6);
    assert!(wait_until(WAIT, || a1.is_detached()), "the dead adapter is torn down at the loss");
    assert!(wait_until(WAIT, || conn.is_finished()), "the lease is ended at the loss");
    assert!(wait_until(WAIT, || up.lane.attempts() == 2 && up.adapter_up()), "rebuilt at once");

    let second = open(
        &up.hub,
        "tradehub-1",
        Some(EventRingCursor { gateway_epoch: epoch, sequence: 1, timestamp_ms: 0 }),
    );
    assert_ne!(second.reply.gateway_epoch, epoch, "the rebuilt adapter's events are a new ring");
    let mut saw_reconcile = false;
    for _ in 0..2 {
        if let TradeLeaseHoldNext::Frame(TradeDown::ReconcileNow { .. }) =
            second.lease.next(Duration::from_secs(2))
        {
            saw_reconcile = true;
        }
    }
    assert!(saw_reconcile, "a reconcile is owed to the rebuilt session");
    assert!(matches!(start(&second.lease, 8), AdapterReply::Running { .. }));
    assert_eq!(up.lane.attempts(), 2, "the client's start found the rebuilt adapter running");
}

/// **A server that accepts each login and drops it before the session is up gets a GROWING wait** (fxcm's start returns
/// before its login, so each restore used to start the backoff over: a rebuild every 2-4 s, for ever, each a login that
/// may count towards a lockout). The first loss is rebuilt at once; each later one waits one probe plus the supervisor's
/// backoff, 40, 80, then 160 ms (lower bounds: a loaded box only waits longer). The exact sequence, in passes, is
/// `reconnect_tests.rs`'s `a_session_that_drops_before_it_says_up_climbs_the_backoff`.
#[test]
fn a_server_that_accepts_and_drops_each_login_gets_a_growing_wait() {
    let up = up_at(wednesday());
    let (_frames, _conn) = drain(Arc::clone(&up.first.lease));
    up.lane.drops.store(true, Ordering::SeqCst);
    up.sdk(6);
    assert!(wait_until(WAIT, || up.lane.attempts() >= 5), "the supervisor keeps rebuilding");
    let starts = up.lane.attempted_at();
    let gaps: Vec<Duration> = starts.windows(2).map(|w| w[1] - w[0]).collect();
    let waits = [FAST.first_wait, FAST.first_wait * 2, FAST.max_wait];
    for (gap, wait) in gaps[1..4].iter().zip(waits) {
        assert!(*gap >= FAST.probe_every + wait, "each rebuild waits longer: {gaps:?}");
    }
}

/// **A late word of the OLD exec thread does not tear the rebuilt adapter down** (a thread that outlived the teardown's
/// bounded join wrote its last `SessionLost` after the rebuilt start reset the cell, and the supervisor tore the healthy
/// new adapter down once): each start CLAIMS the cell, and an earlier claim writes nothing.
#[test]
fn a_late_session_lost_from_the_old_exec_thread_does_not_tear_the_new_adapter_down() {
    let up = up_at(wednesday());
    let (_frames, _conn) = drain(Arc::clone(&up.first.lease));
    let old = up.lane.writer(0);
    up.sdk(6);
    assert!(wait_until(WAIT, || up.lane.attempts() == 2 && up.adapter_up()), "rebuilt");
    let rebuilt = up.lane.inner.adapter("fxcm", None).expect("the rebuilt adapter");
    up.sdk(3);
    old.set(from_status(Some(6)), None);
    std::thread::sleep(FAST.probe_every * 10);
    assert!(!rebuilt.is_detached() && up.adapter_up(), "the old thread's word took nothing down");
    assert_eq!(up.lane.attempts(), 2, "nothing was rebuilt for it");
}

// ---- the FX weekend --------------------------------------------------------------------------------------------

/// **A `SessionLost` at Friday's close is NOT a failure**: the adapter is torn down, but NOTHING is started while the
/// market is closed (the rows are still read), and the state says the calendar's words, which a redial reads; at the
/// open exactly ONE start is made and the session is back.
#[test]
fn a_session_lost_at_fridays_close_makes_no_attempt_until_the_open_and_then_one() {
    let up = up_at(at("2026-11-20T21:30:00Z"));
    let a1 = up.lane.inner.adapter("fxcm", None).expect("the first adapter");
    let (_frames, _conn) = drain(Arc::clone(&up.first.lease));
    up.set_clock("2026-11-20T22:00:30Z");
    up.sdk(6);
    assert!(wait_until(WAIT, || a1.is_detached()), "the dead adapter is torn down");
    assert!(
        wait_until(WAIT, || {
            let text = refusal_text(up.hub.open(request("tradehub-1", None)));
            text.contains(CLOSED)
        }),
        "the state says the market is closed and when the login is"
    );
    // Saturday: still nothing, however many passes.
    up.set_clock("2026-11-21T12:00:00Z");
    std::thread::sleep(FAST.probe_every * 25);
    assert_eq!(up.lane.attempts(), 1, "no start while the FX market is closed");
    assert!(!up.adapter_up());

    up.set_clock("2026-11-22T21:00:01Z");
    assert!(wait_until(WAIT, || up.adapter_up()), "the open brings the session back");
    std::thread::sleep(FAST.probe_every * 10);
    assert_eq!(up.lane.attempts(), 2, "ONE start at the open");
}

/// **A loss ON the weekend, and the account deactivated meanwhile**: the rows still decide during the hold, so the
/// session is given up with no start ever made.
#[test]
fn the_rows_saying_stop_during_the_weekend_hold_give_up_with_no_start() {
    let up = up_at(at("2026-11-21T09:00:00Z"));
    let a1 = up.lane.inner.adapter("fxcm", None).expect("the first adapter");
    let (_frames, _conn) = drain(Arc::clone(&up.first.lease));
    up.sdk(0);
    assert!(wait_until(WAIT, || a1.is_detached()), "`Disconnected` is final too");
    *up.rows.lock().unwrap_or_else(PoisonError::into_inner) = Err(ArmingBlock::AccountInactive);
    assert!(
        wait_until(WAIT, || up.hub.open(request("tradehub-1", None)).is_ok()),
        "a session given up lets the client open (and start it afresh once the account runs)"
    );
    up.set_clock("2026-11-22T21:00:01Z");
    std::thread::sleep(FAST.probe_every * 10);
    assert_eq!(up.lane.attempts(), 1, "nothing was started for an account the rows stopped");
}

/// **At the open there is one login, then the normal backoff**: a start refused after the open is tried again after
/// the supervisor's first wait, then twice that (each a lower bound: a loaded box only waits longer).
#[test]
fn a_refused_start_after_the_open_backs_off_from_the_first_wait() {
    let up = up_with(at("2026-11-21T09:00:00Z"), true, &[2, 3]);
    let a1 = up.lane.inner.adapter("fxcm", None).expect("the first adapter");
    let (_frames, _conn) = drain(Arc::clone(&up.first.lease));
    up.sdk(6);
    assert!(wait_until(WAIT, || a1.is_detached()));
    std::thread::sleep(FAST.probe_every * 10);
    assert_eq!(up.lane.attempts(), 1, "held over the weekend");
    up.set_clock("2026-11-22T21:00:01Z");
    assert!(
        wait_until(WAIT, || up.lane.attempts() == 4 && up.adapter_up()),
        "up after two refusals"
    );
    let starts = up.lane.attempted_at();
    assert!(starts[2] - starts[1] >= FAST.first_wait, "{:?}", starts[2] - starts[1]);
    assert!(starts[3] - starts[2] >= FAST.first_wait * 2, "{:?}", starts[3] - starts[2]);
}

/// **A failed first restore after the open says ITS error, not "market closed"**: the hold wrote the calendar's words
/// into the state, and they used to outlive the open until a restore succeeded, so every reject and `trade health` said
/// the market was closed while it was open and the login was failing.
#[test]
fn a_failed_first_restore_after_the_open_shows_the_restore_error_not_market_closed() {
    let refused: Vec<usize> = (2..=400).collect();
    let up = up_with(at("2026-11-21T09:00:00Z"), true, &refused);
    let a1 = up.lane.inner.adapter("fxcm", None).expect("the first adapter");
    let (_frames, _conn) = drain(Arc::clone(&up.first.lease));
    up.sdk(6);
    assert!(wait_until(WAIT, || a1.is_detached()));
    let words = || refusal_text(up.hub.open(request("tradehub-1", None)));
    assert!(wait_until(WAIT, || words().contains(CLOSED)), "held over the weekend");
    up.set_clock("2026-11-22T21:00:01Z");
    assert!(
        wait_until(WAIT, || words().contains("the ForexConnect session did not open")),
        "the failed restore's error is the state's words"
    );
    let text = words();
    assert!(!text.contains("FX market closed"), "{text}");
}

// ---- the learning ----------------------------------------------------------------------------------------------

/// **fxcm announces that it learned the venue on every start, and is never held for it**; a bridge that never
/// announces is HELD past the learning budget, with words, and its orders are sent nowhere.
#[test]
fn a_bridge_that_announces_its_learning_is_not_held_and_one_that_never_does_is() {
    for learns in [true, false] {
        let up = up_with(wednesday(), learns, &[]);
        let (frames, _conn) = drain(Arc::clone(&up.first.lease));
        std::thread::sleep(LEARN_BUDGET * 2);
        assert_eq!(
            up.first.lease.handle(TradeUp::Submit { command_sequence: 1, order: order("c-learn") }),
            TradeLeaseHoldFlow::Continue
        );
        let adapter = up.lane.inner.adapter("fxcm", None).expect("the adapter");
        if learns {
            assert!(
                wait_until(WAIT, || adapter
                    .submitted()
                    .iter()
                    .any(|o| o.order_client_id == "c-learn")),
                "a bridge that learned takes the order"
            );
        } else {
            assert!(
                wait_until(WAIT, || rejected_with(&frames, "FXCM session not learned")),
                "a bridge that never learned holds the session with words"
            );
            assert!(adapter.submitted().is_empty(), "nothing was sent");
        }
    }
}
