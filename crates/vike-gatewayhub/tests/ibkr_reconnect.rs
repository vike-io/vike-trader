//! **IBKR's reconnect after the gateway's daily logoff** (decision 0164 verdict 7(b), wave 2 W14 and W14b),
//! against a FAKE GATEWAY SOCKET: a loopback listener the test closes and reopens (a flag: the socket stays
//! bound, so no neighbour can take its port), probed by the real `IbkrWatch` (a gateway the test closed
//! reads as down without asking the port: `FakeGatewayWatch`), supervised by the real session over a
//! `ScriptedTrade` lane.
//!
//! What it pins: a closed gateway takes the adapter down, ENDS THE LEASE AT THE LOSS (so the platform is
//! told the venue is gone, not 85 s later) and refuses a client's redial with words until the session is
//! back; every command queued at the loss is answered, never dispatched to the dying adapter; the port
//! coming back rebuilds the adapter in a NEW epoch and owes a reconcile; the account rows are re-read on
//! EVERY pass (an `active = 0` flip, a changed tier or the venue-scoped two-tier conflict of decision 0140
//! stops the reconnect and says why, but a rows READ that failed only waits); a SCRIPTED bridge's report that
//! its link is gone is acted on at once (the IBKR bridge sends it at its stream's death; the probes are the fallback);
//! a rebuild over orders the daemon never saw finish HOLDS the session (the rebuilt bridge is blind to
//! them: W14c); what a rebuilt adapter pushes at connect reaches the first lease; a stop does not wait for
//! a venue login; a LIVE rebuild asks the rows again right before the start; a refused restore backs off;
//! a retired session is never revived.
#![cfg(feature = "trade")]

use std::net::TcpListener;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use vike_config::ArmingBlock;
use vike_exec::EventSender;
use vike_exec::recon::ReconClient;
use vike_gatewayhub::budget::ledger::Budget;
use vike_gatewayhub::session::reconnect::Schedule;
use vike_gatewayhub::testkit::trade::{ExecCall, ScriptedTrade};
use vike_gatewayhub::trade::TradeConfig;
use vike_gatewayhub::trade::ibkr::{IbkrGateway, IbkrWatch, TierNow};
use vike_gatewayhub::trade::lane::{
    AdapterRefusal, LinkWatch, StartedAdapter, TradeLane, TradeTarget,
};
use vike_gatewayhub::trade::lease::{Flow, Lease, Next};
use vike_gatewayhub::trade::session::{OpenRefusal, OpenRequest, Opened, TradeHub};
use vike_gatewayhub_client::proto::{
    AdapterReply, AdapterStart, ArmingTable, DetachPhase, PreflightAnswer, PreflightStep, TradeBye,
    TradeDown, TradeUp, WireCursor,
};
use vike_model::events::{Event, OrderAccepted, OrderCanceled};
use vike_model::{AccountTier, OrderRequest};
use vike_venues::IbkrBackend;

#[allow(dead_code, clippy::allow_attributes)]
#[path = "support/trade_rig.rs"]
mod trade_rig;

use trade_rig::{start_args, wait_until};

const WAIT: Duration = Duration::from_secs(8);

/// The wait for a REBUILT adapter in the shared setups (`rebuilt`, `rebuilt_at`): a loss, a probe, the restore and
/// the start behind it, on a box running at many times its core count (CI's runners: load ~160). Condition-based,
/// so a quiet box never waits it out.
const REBUILD_WAIT: Duration = Duration::from_secs(30);

/// A probe every 20 ms, the first failed restore waits 40 ms, growing to 160 ms.
const FAST: Schedule = Schedule {
    probe_every: Duration::from_millis(20),
    first_wait: Duration::from_millis(40),
    max_wait: Duration::from_millis(160),
};

/// A probe every 30 s: a loss that is acted on inside the test's wait was NOT found by the probe.
const SLOW: Schedule = Schedule {
    probe_every: Duration::from_secs(30),
    first_wait: Duration::from_millis(40),
    max_wait: Duration::from_millis(160),
};

fn cfg() -> TradeConfig {
    TradeConfig {
        heartbeat: Duration::from_millis(100),
        read_floor: Duration::from_millis(300),
        ring_events: 64,
        dedup_coids: 64,
        command_queue: 64,
        control_queue: 64,
        event_channel: 64,
        max_leases: 8,
        orphan_ttl: Duration::from_secs(600),
        recon_progress: Duration::from_millis(100),
        recon_page_bytes: 4096,
        fence_drain: Duration::from_millis(500),
        account_ccy_retry_after: vike_gatewayhub::trade::ACCOUNT_CCY_RETRY_AFTER,
        state_dir: None,
        halt_path: None,
    }
}

// ---- the fake gateway ----------------------------------------------------------------------------------

/// A loopback listener standing in for IB Gateway's API port. Closing it is the daily logoff.
///
/// The listener stays BOUND for the fake's whole life and `open` is the gateway's state, which
/// [`FakeGatewayWatch`] reads in place of the port's answer. It used to be dropped at the logoff and
/// re-bound at the return, and a loaded box takes the freed number: another test's listener or an
/// ephemeral client port (`127.0.0.1:0` is every neighbour's bind) holds it, and `reopen` waited on a
/// stranger for as long as the stranger lived. MEASURED 2026-10-18 on a the latency box lane, 30 parallel runs of
/// this file: 1 re-bind in 780 found its port taken, for 2.9 s, which is the test's whole 8 s `WAIT` on
/// a longer-lived neighbour. Nothing reads the closed gateway through the socket, so nothing is lost.
///
/// A gateway that is up ACCEPTS ([`Self::accept_pending`]). A listener nobody accepts from keeps every probe's
/// connection in its accept queue, which holds 129 (the std backlog of 128, plus one), and the kernel then drops
/// the next SYN: every later connect hangs to the probe's 1 s timeout and reads as a gateway that is gone. The
/// re-bind at each logoff had hidden that (a fresh listener, an empty queue); a listener kept for the fake's life
/// meets it, because `FAST`'s probe every 20 ms fills the queue in 2.6 s, which a rig that reaches its loss
/// after 3 s outlasts. MEASURED 2026-10-18 on a the latency box lane: the real-client rigs of `real_bridge` answered 129
/// probes and then every connect timed out, so the gateway that came back was never found.
struct FakeGateway {
    port: u16,
    listener: TcpListener,
    open: AtomicBool,
}

impl FakeGateway {
    fn start() -> Arc<Self> {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind a loopback port");
        listener.set_nonblocking(true).expect("a non-blocking listener");
        let port = listener.local_addr().expect("the port").port();
        Arc::new(Self { port, listener, open: AtomicBool::new(true) })
    }

    /// Accept, and drop at once, every connection waiting: what a gateway that is up does with a probe.
    fn accept_pending(&self) {
        while self.listener.accept().is_ok() {}
    }

    fn close(&self) {
        self.open.store(false, Ordering::SeqCst);
    }

    fn is_open(&self) -> bool {
        self.open.load(Ordering::SeqCst)
    }

    fn reopen(&self) {
        self.open.store(true, Ordering::SeqCst);
    }

    fn gateway(&self) -> IbkrGateway {
        IbkrGateway { backend: IbkrBackend::Socket, host: "127.0.0.1".into(), port: self.port }
    }
}

// ---- the account rows ----------------------------------------------------------------------------------

type RowsAnswer = Result<AccountTier, ArmingBlock>;
/// A scripted answer: given the read's index.
type RowsScript = Box<dyn Fn(usize) -> RowsAnswer + Send>;

/// What the account table says RIGHT NOW, and how often it was read.
struct Rows {
    now: Mutex<RowsAnswer>,
    /// When set, answers instead of `now` (it is given the read's index).
    script: Mutex<Option<RowsScript>>,
    reads: AtomicUsize,
}

impl Rows {
    fn new(now: RowsAnswer) -> Arc<Self> {
        Arc::new(Self {
            now: Mutex::new(now),
            script: Mutex::new(None),
            reads: AtomicUsize::new(0),
        })
    }

    fn set(&self, now: RowsAnswer) {
        *self.now.lock().unwrap_or_else(PoisonError::into_inner) = now;
    }

    fn scripted(&self, answer: impl Fn(usize) -> RowsAnswer + Send + 'static) {
        *self.script.lock().unwrap_or_else(PoisonError::into_inner) = Some(Box::new(answer));
    }

    /// Back to `now`: the script is dropped.
    fn unscript(&self) {
        *self.script.lock().unwrap_or_else(PoisonError::into_inner) = None;
    }

    /// From the NEXT read on the table says `answer`, and the gateway comes BACK inside that very read.
    ///
    /// The loss, the rows and the gateway's return are then ordered by the supervisor's own reads and not
    /// by the test's clock: the pass that learns the rows now say no is the one that finds the gateway
    /// reachable, so a supervisor that ignored the verdict would restore (a test that set the rows and
    /// reopened the gateway from its own thread left a window between the pass's read of the rows and its
    /// probe of the port, which a loaded box stretched until the old answer and the new gateway met).
    fn answer_with_the_gateway_back(&self, gw: &Arc<FakeGateway>, answer: RowsAnswer) {
        let gw = Arc::clone(gw);
        let back = AtomicBool::new(false);
        self.scripted(move |_| {
            if !back.swap(true, Ordering::SeqCst) {
                gw.reopen();
            }
            answer
        });
    }

    fn reads(&self) -> usize {
        self.reads.load(Ordering::SeqCst)
    }

    fn tier_now(self: &Arc<Self>) -> TierNow {
        let rows = Arc::clone(self);
        Box::new(move || {
            let n = rows.reads.fetch_add(1, Ordering::SeqCst);
            if let Some(script) =
                rows.script.lock().unwrap_or_else(PoisonError::into_inner).as_ref()
            {
                return script(n);
            }
            *rows.now.lock().unwrap_or_else(PoisonError::into_inner)
        })
    }
}

/// The REAL [`IbkrWatch`], except that a gateway the test has CLOSED is unreachable whatever the port says.
///
/// A closed [`FakeGateway`] keeps its listener (see there), so its port still answers a connect: the verdict is
/// the fake's own state, taken before the probe (a closed gateway is not probed) and again after it (a gateway
/// closed meanwhile is down). An open gateway is still probed over a real socket, and accepts the probe.
struct FakeGatewayWatch {
    gw: Arc<FakeGateway>,
    real: IbkrWatch,
}

impl LinkWatch for FakeGatewayWatch {
    fn reachable(&self) -> Result<(), String> {
        let closed = || Err("the fake gateway is closed".to_string());
        if !self.gw.is_open() {
            return closed();
        }
        let probe = self.real.reachable();
        self.gw.accept_pending();
        if self.gw.is_open() { probe } else { closed() }
    }

    fn tier_now(&self) -> Result<AccountTier, ArmingBlock> {
        self.real.tier_now()
    }

    fn schedule(&self) -> Schedule {
        self.real.schedule()
    }
}

fn watch_on(gw: &Arc<FakeGateway>, rows: &Arc<Rows>, schedule: Schedule) -> Arc<dyn LinkWatch> {
    let real = IbkrWatch::new(gw.gateway(), rows.tier_now()).with_schedule(schedule);
    Arc::new(FakeGatewayWatch { gw: Arc::clone(gw), real })
}

fn watch(gw: &Arc<FakeGateway>, rows: &Arc<Rows>) -> Arc<dyn LinkWatch> {
    watch_on(gw, rows, FAST)
}

// ---- the hub -------------------------------------------------------------------------------------------

fn hub_over(lane: impl TradeLane + 'static) -> Arc<TradeHub> {
    TradeHub::new(Arc::new(lane), cfg(), Arc::new(Budget::new()))
}

fn request_at(client_id: &str, tier: AccountTier, resume: Option<WireCursor>) -> OpenRequest {
    OpenRequest {
        client_id: client_id.to_string(),
        venue: "ibkr".to_string(),
        account: None,
        tier,
        resume,
        takeover: false,
        key_id: Some("nk-test".to_string()),
        closer: Arc::new(|| {}),
    }
}

fn request(client_id: &str, resume: Option<WireCursor>) -> OpenRequest {
    request_at(client_id, AccountTier::Demo, resume)
}

fn open(hub: &TradeHub, client_id: &str, resume: Option<WireCursor>) -> Opened {
    hub.open(request(client_id, resume)).expect("the open is accepted")
}

/// The words of an open the daemon refused, or a failure naming what happened instead.
fn refusal_text(answer: Result<Opened, OpenRefusal>) -> String {
    match answer {
        Err(OpenRefusal::Error(text)) => text,
        Err(other) => panic!("expected a plain refusal with words, got {other:?}"),
        Ok(_) => panic!("the open was accepted"),
    }
}

fn order(coid: &str) -> OrderRequest {
    OrderRequest { venue: "ibkr".into(), ..trade_rig::order(coid) }
}

fn expect_frame(lease: &Lease) -> TradeDown {
    match lease.next(Duration::from_secs(2)) {
        Next::Frame(frame) => frame,
        other => panic!("expected a frame, got {other:?}"),
    }
}

/// Start the adapter the way a client does and read the answer.
fn start(lease: &Arc<Lease>, rid: u64) -> AdapterReply {
    let mut s = start_args();
    s.rid = rid;
    s.route_key = "ibkr".into();
    assert_eq!(lease.handle(TradeUp::AdapterStart(s)), Flow::Continue);
    match expect_frame(lease) {
        TradeDown::AdapterReply { rid: got, reply } => {
            assert_eq!(got, rid);
            *reply
        }
        other => panic!("{other:?}"),
    }
}

/// Plays the connection thread of a lease: writes (collects) every frame, ends at the `Bye`, and calls
/// `finish`, the way the server's connection does.
struct Conn {
    frames: Arc<Mutex<Vec<TradeDown>>>,
    thread: JoinHandle<Option<TradeBye>>,
}

impl Conn {
    /// Wait until a reject or advisory whose reason contains `needle` was written.
    fn wait_reason(&self, needle: &str) -> bool {
        wait_until(WAIT, || {
            let frames = self.frames.lock().unwrap_or_else(PoisonError::into_inner).clone();
            reasons(&frames).iter().any(|r| r.contains(needle))
        })
    }

    /// Whether the connection has ended (the lease was told `Bye`).
    fn ended(&self) -> bool {
        self.thread.is_finished()
    }

    /// The frames written and the closing `Bye`, once the connection has ended.
    fn join(self) -> std::thread::Result<(Vec<TradeDown>, Option<TradeBye>)> {
        let bye = self.thread.join()?;
        let frames = self.frames.lock().unwrap_or_else(PoisonError::into_inner).clone();
        Ok((frames, bye))
    }
}

fn connection(lease: Arc<Lease>) -> Conn {
    connection_paced(lease, Duration::ZERO)
}

/// [`connection`] whose writer takes `pace` to write each frame (a slow socket).
fn connection_paced(lease: Arc<Lease>, pace: Duration) -> Conn {
    let frames = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&frames);
    let thread = std::thread::spawn(move || {
        loop {
            match lease.next(Duration::from_millis(50)) {
                Next::Frame(f) => {
                    sink.lock().unwrap_or_else(PoisonError::into_inner).push(f);
                    std::thread::sleep(pace);
                }
                Next::Bye(b) => {
                    lease.finish();
                    return Some(b);
                }
                Next::Idle => {}
                Next::Closed => {
                    lease.finish();
                    return None;
                }
            }
        }
    });
    Conn { frames, thread }
}

fn adapter_up(hub: &TradeHub) -> bool {
    hub.session_status("ibkr", None).is_some_and(|s| s.adapter_up)
}

/// The reasons of every terminal reject and advisory in `frames`.
fn reasons(frames: &[TradeDown]) -> Vec<String> {
    frames
        .iter()
        .filter_map(|f| match f {
            TradeDown::Event { event: Event::OrderRejected(r), .. } => Some(r.reason.to_string()),
            TradeDown::Event { event: Event::OrderCancelRejected(r), .. } => {
                Some(r.reason.to_string())
            }
            TradeDown::Event { event: Event::OrderModifyRejected(r), .. } => {
                Some(r.reason.to_string())
            }
            _ => None,
        })
        .collect()
}

/// The coids of every terminal reject in `frames`.
fn rejected(frames: &[TradeDown]) -> Vec<String> {
    frames
        .iter()
        .filter_map(|f| match f {
            TradeDown::Event { event: Event::OrderRejected(r), .. } => {
                Some(r.client_order_id.to_string())
            }
            _ => None,
        })
        .collect()
}

/// A running supervised session at `demo`: the gateway up, the lease open and the adapter started.
struct Up {
    gw: Arc<FakeGateway>,
    rows: Arc<Rows>,
    lane: ScriptedTrade,
    hub: Arc<TradeHub>,
    first: Opened,
}

fn up_on(schedule: Schedule) -> Up {
    let gw = FakeGateway::start();
    let rows = Rows::new(Ok(AccountTier::Demo));
    let lane =
        ScriptedTrade::new(&["ibkr"]).with_link_watch("ibkr", watch_on(&gw, &rows, schedule));
    let hub = hub_over(lane.clone());
    let first = open(&hub, "tradehub-1", None);
    assert!(matches!(start(&first.lease, 7), AdapterReply::Live { .. }));
    assert!(adapter_up(&hub));
    Up { gw, rows, lane, hub, first }
}

fn up() -> Up {
    up_on(FAST)
}

#[test]
fn a_closed_gateway_ends_the_lease_at_the_loss_and_the_returning_one_rebuilds_it_in_a_new_epoch() {
    let Up { gw, lane, hub, first, .. } = up();
    let epoch = first.reply.gw_epoch;
    let a1 = lane.adapter("ibkr", None).expect("the first adapter");
    let conn = connection(Arc::clone(&first.lease));

    // The daily logoff: the port closes.
    gw.close();
    assert!(wait_until(WAIT, || !adapter_up(&hub)), "the loss is seen");
    assert!(wait_until(WAIT, || a1.is_detached()), "the dead adapter is torn down at once");

    // The platform is told AT THE LOSS (the lease ends, so the client discloses ExecLink down and the
    // strategies' `Disconnected` now), not when the venue is back ~85 s later.
    assert!(
        wait_until(WAIT, || conn.ended()),
        "the lease is ended at the loss, not at the restore"
    );
    assert_eq!(lane.starts(), 1, "nothing was started while the port is closed");

    // A client that redials meanwhile is refused with words (an error it retries on its own schedule,
    // which keeps its link reporting down), and the refusal does not retire the session.
    let text = refusal_text(hub.open(request("tradehub-1", None)));
    assert!(text.contains("re-establishing"), "{text}");
    assert_eq!(hub.sessions(), 1, "a refused open must not retire the session being rebuilt");
    let (_, bye) = conn.join().expect("the connection thread");
    assert_eq!(bye, Some(TradeBye::ServerStopping));

    // The port answers again.
    gw.reopen();
    assert!(wait_until(WAIT, || lane.starts() == 2), "the adapter is rebuilt");
    assert!(wait_until(WAIT, || adapter_up(&hub)));

    // The client redials with the cursor of the old epoch: it is a NEW epoch, answered as no cursor,
    // and a reconcile is owed.
    let second = open(&hub, "tradehub-1", Some(WireCursor { gw_epoch: epoch, seq: 1, ts_ms: 0 }));
    assert_ne!(second.reply.gw_epoch, epoch, "the rebuilt adapter's events are a new ring");
    let mut saw_gap = false;
    let mut saw_reconcile = false;
    for _ in 0..2 {
        match expect_frame(&second.lease) {
            TradeDown::ReplayUnavailable { .. } => saw_gap = true,
            TradeDown::ReconcileNow { .. } => saw_reconcile = true,
            other => panic!("unexpected first frame {other:?}"),
        }
    }
    assert!(saw_gap && saw_reconcile, "ReplayUnavailable and ReconcileNow lead the new lease");

    // The client's own start finds the rebuilt adapter running: no third start.
    assert!(matches!(start(&second.lease, 8), AdapterReply::Live { .. }));
    assert_eq!(lane.starts(), 2);
    assert_eq!(
        second.lease.handle(TradeUp::Submit { cseq: 1, order: order("c-up") }),
        Flow::Continue
    );
    let a2 = lane.adapter("ibkr", None).expect("the second adapter");
    assert!(wait_until(WAIT, || a2.submitted().iter().any(|o| o.client_order_id == "c-up")));
}

/// [`start`] from a node that asks the start to read the account's currency (`read_account_ccy`, an ibkr account
/// that will convert), and the currency the reply carries.
fn start_reading_currency(lease: &Arc<Lease>, rid: u64, reads: bool) -> Option<String> {
    let mut s = start_args();
    s.rid = rid;
    s.route_key = "ibkr".into();
    s.read_account_ccy = reads;
    assert_eq!(lease.handle(TradeUp::AdapterStart(s)), Flow::Continue);
    match expect_frame(lease) {
        TradeDown::AdapterReply { reply, .. } => match *reply {
            AdapterReply::Live { account_ccy, .. } => account_ccy,
            other => panic!("{other:?}"),
        },
        other => panic!("{other:?}"),
    }
}

/// A node's start, a loss and a restore, then the node's SAME start again: the currency the reply carries and how
/// often the lane was asked for it (finding 1 of the second IBKR currency review). A restore asks the venue
/// nothing (its reply goes to nobody), so the reply it stores carries none: a repeated start that replayed that
/// would anchor a tradehub restarted after any nightly re-login to nothing.
fn currency_after_a_restore(reads: bool) -> (Option<String>, usize, usize) {
    let gw = FakeGateway::start();
    let rows = Rows::new(Ok(AccountTier::Demo));
    let lane = ScriptedTrade::new(&["ibkr"])
        .with_link_watch("ibkr", watch(&gw, &rows))
        .with_account_currency("ibkr", "EUR")
        .with_budgeted_currency_read("ibkr");
    let hub = hub_over(lane.clone());
    let first = open(&hub, "tradehub-1", None);
    let before = start_reading_currency(&first.lease, 7, reads);
    // (The scripted lane states its currency whatever the start asks; the real one reads it only when asked.)
    assert_eq!(before.as_deref(), Some("EUR"), "the first start is answered by the lane");
    let asks_before = lane.account_currency_asks();
    gw.close();
    assert!(wait_until(WAIT, || !adapter_up(&hub)), "the loss is seen");
    gw.reopen();
    assert!(wait_until(REBUILD_WAIT, || lane.starts() == 2 && adapter_up(&hub)), "the rebuild");
    assert_eq!(lane.account_currency_asks(), asks_before, "a restore asks the venue nothing");
    let second = open(&hub, "tradehub-1", None);
    let after = start_reading_currency(&second.lease, 8, reads);
    assert_eq!(lane.starts(), 2, "the running adapter answered: no third start");
    // The same start once more: the answer a repeat got is the one it keeps (no second read of a scarce budget).
    assert_eq!(start_reading_currency(&second.lease, 9, reads), after);
    (after, asks_before, lane.account_currency_asks())
}

#[test]
fn a_repeated_start_after_a_restore_reads_the_currency_the_restore_did_not() {
    let (after, asks_before, asks_after) = currency_after_a_restore(true);
    assert_eq!(
        after.as_deref(),
        Some("EUR"),
        "the restore's reply stated none: the repeat asks again"
    );
    assert_eq!(
        asks_after,
        asks_before + 1,
        "one read for the repeat, the next repeat is answered from it"
    );
}

#[test]
fn a_repeated_start_after_a_restore_that_never_asked_for_the_currency_still_asks_nothing() {
    let (after, asks_before, asks_after) = currency_after_a_restore(false);
    assert_eq!(after, None);
    assert_eq!(
        asks_after, asks_before,
        "a start that did not ask to read the currency spends nothing"
    );
}

#[test]
fn an_open_while_the_daemon_is_bringing_the_session_back_is_refused_with_words() {
    let Up { gw, lane, hub, .. } = up();
    gw.close();
    assert!(wait_until(WAIT, || !adapter_up(&hub)));
    let text = refusal_text(hub.open(request("tradehub-2", None)));
    assert!(text.contains("re-establishing"), "{text}");
    assert!(text.contains("ibkr"), "the words name the venue: {text}");
    assert_eq!(lane.starts(), 1, "the refused open started nothing");
}

#[test]
fn the_rows_are_read_on_every_pass_while_the_session_is_down() {
    let Up { gw, rows, hub, first, .. } = up();
    let _conn = connection(Arc::clone(&first.lease));
    gw.close();
    assert!(wait_until(WAIT, || !adapter_up(&hub)));
    let before = rows.reads();
    // Five more passes, however long the box takes to make them (a 20 ms probe each; a loaded one makes fewer
    // than five in a fixed 300 ms).
    assert!(
        wait_until(WAIT, || rows.reads() >= before + 5),
        "the predicate re-reads the rows on every pass, not once ({before} -> {})",
        rows.reads()
    );
}

/// After the reconnect gave up, the client redials into the stopped session (the open is accepted: the
/// account's own state is judged by `account_tier`, and a client must be able to start it afresh once the
/// operator re-activates it) and every command it sends is answered with the stop's words.
///
/// No clock orders the steps: the rows turn to `answer` and the gateway returns inside the supervisor's own
/// read of them ([`Rows::answer_with_the_gateway_back`]), and the redial is retried until the daemon accepts
/// it, which it does only once the session has GIVEN UP (it is refused while the session is being brought
/// back). A supervisor that restored instead would have started the adapter again by then.
///
/// ⚠ **The stopped session is on a clock this test does not own: the orphan reaper.** From the moment the
/// supervisor has given up, the session has no adapter, no supervisor and (the lease ended at the loss) no
/// lease, which is what the reaper retires at its next tick (500 ms). A redial that lands after that opens a
/// FRESH session (link up, no adapter), which answers a command "no venue adapter is running" and not with
/// the stop's words: the daemon's right answer for a session that no longer exists, and not the thing under
/// test. MEASURED 2026-10-18 on a the latency box lane at 48 copies of this file at once on two cores: 19 of 145 runs
/// failed in these three tests, every one sampled with that reject (and in a second pass 6 of 48, each one
/// a redial that found a fresh session: the epochs differed). So the redial is made the moment the daemon
/// accepts it (before the first connection is joined), the session it reached is told by its epoch (a fresh
/// session mints its own), and a redial that found a fresh one runs the scenario again.
fn gave_up_then_reopened(answer: RowsAnswer, words: &[&str], reject_needle: &str) {
    const ATTEMPTS: usize = 5;
    for attempt in 1..=ATTEMPTS {
        let Up { gw, rows, lane, hub, first } = up();
        let conn = connection(Arc::clone(&first.lease));
        gw.close();
        assert!(wait_until(WAIT, || !adapter_up(&hub)));
        rows.answer_with_the_gateway_back(&gw, answer);
        let second = redial_until_accepted(&hub, "tradehub-1");
        assert!(wait_until(WAIT, || conn.ended()), "the lease was ended at the loss");
        conn.join().expect("the connection thread");
        if second.reply.gw_epoch != first.reply.gw_epoch {
            // The reaper retired the stopped session before the redial: not the session under test.
            assert!(
                attempt < ATTEMPTS,
                "the reaper took the stopped session {ATTEMPTS} times running"
            );
            continue;
        }

        assert_eq!(
            lane.starts(),
            1,
            "no attempt is made once the rows say the account does not run"
        );
        assert!(!adapter_up(&hub));
        let conn2 = connection(Arc::clone(&second.lease));
        assert_eq!(
            second.lease.handle(TradeUp::Submit { cseq: 1, order: order("c-stopped") }),
            Flow::Continue
        );
        let heard = conn2.wait_reason(reject_needle);
        assert!(
            heard,
            "the stopped session answered the command with the stop's words; frames {:?}, status {:?}, starts {}",
            conn2.frames.lock().unwrap_or_else(PoisonError::into_inner),
            hub.session_status("ibkr", None),
            lane.starts()
        );
        second.lease.close();
        let (frames, _) = conn2.join().expect("the connection thread");
        let said = reasons(&frames);
        assert!(
            said.iter().any(|r| words.iter().all(|w| r.contains(w))),
            "the reject says the reconnect stopped and why: {said:?}"
        );
        return;
    }
}

/// Ask for a lease until the daemon accepts it: a session that is down and still being brought back refuses
/// the open with words, one that has given up (or is up) accepts it.
fn redial_until_accepted(hub: &TradeHub, client_id: &str) -> Opened {
    let mut opened = None;
    assert!(
        wait_until(WAIT, || {
            opened = hub.open(request(client_id, None)).ok();
            opened.is_some()
        }),
        "the daemon never accepted the redial"
    );
    opened.expect("accepted above")
}

#[test]
fn active_zero_stops_the_reconnect_and_says_why() {
    gave_up_then_reopened(
        Err(ArmingBlock::AccountInactive),
        &["not re-establishing", "none is active"],
        "not re-establishing",
    );
}

#[test]
fn the_venue_scoped_two_tier_conflict_stops_it_and_says_why() {
    // Decision 0140: ibkr rides ONE session per box; an active demo row beside an active live row of any
    // label stops every ibkr account.
    gave_up_then_reopened(
        Err(ArmingBlock::TierConflict),
        &["ONE session per box", "different tiers"],
        "ONE session per box",
    );
}

#[test]
fn a_tier_that_changed_while_it_was_down_stops_it() {
    gave_up_then_reopened(
        Ok(AccountTier::Live),
        &["now state tier live", "ran at demo"],
        "now state tier live",
    );
}

#[test]
fn an_active_flip_while_the_gateway_is_up_is_not_a_reconnect_and_changes_nothing() {
    // The predicate gates RECONNECTING; it does not tear a healthy session down.
    let Up { rows, lane, hub, gw, first } = up();
    let _held = (&gw, &first);
    rows.set(Err(ArmingBlock::AccountInactive));
    std::thread::sleep(Duration::from_millis(300));
    assert!(adapter_up(&hub));
    assert_eq!(lane.starts(), 1);
}

/// Review of W14, finding 5: one failed read of the account table during the outage ended the supervision
/// for good ("not in the store" read as "deactivated"). A read that FAILED waits and asks again; only the
/// rows' own answer decides.
#[test]
fn an_unreadable_account_table_does_not_end_the_supervision() {
    let Up { gw, rows, lane, hub, first } = up();
    let _conn = connection(Arc::clone(&first.lease));
    gw.close();
    assert!(wait_until(WAIT, || !adapter_up(&hub)));
    // The store is locked for a while, and the gateway is back meanwhile. Several passes read the rows,
    // each finding the gateway reachable (the count is of READS, not of a clock: a loaded box runs fewer
    // passes in the same time).
    let before = rows.reads();
    rows.answer_with_the_gateway_back(&gw, Err(ArmingBlock::AccountNotInStore));
    assert!(wait_until(WAIT, || rows.reads() >= before + 4), "the passes keep asking the rows");
    assert_eq!(lane.starts(), 1, "nothing is restored while the rows cannot be read");
    assert!(!adapter_up(&hub));
    // The store opens again and says the account runs: the supervision was still there.
    rows.unscript();
    rows.set(Ok(AccountTier::Demo));
    assert!(
        wait_until(WAIT, || lane.starts() == 2 && adapter_up(&hub)),
        "the session is rebuilt once the rows answer"
    );
}

#[test]
fn an_unreadable_account_table_followed_by_an_inactive_account_still_stops_it() {
    let Up { gw, rows, lane, hub, first } = up();
    let _conn = connection(Arc::clone(&first.lease));
    gw.close();
    assert!(wait_until(WAIT, || !adapter_up(&hub)));
    let before = rows.reads();
    rows.answer_with_the_gateway_back(&gw, Err(ArmingBlock::AccountNotInStore));
    assert!(wait_until(WAIT, || rows.reads() >= before + 4), "the passes keep asking the rows");
    rows.unscript();
    rows.set(Err(ArmingBlock::AccountInactive));
    // The redial is accepted only once the session has given up (see `gave_up_then_reopened`).
    let _redial = redial_until_accepted(&hub, "tradehub-1");
    assert_eq!(lane.starts(), 1, "a definitive 'inactive' is a stop");
    assert!(!adapter_up(&hub));
}

/// A lane that refuses the `refuse`-th starts and delegates the rest, recording when each start was asked.
struct Flaky {
    inner: ScriptedTrade,
    starts: Mutex<Vec<Instant>>,
    refuse: std::ops::RangeInclusive<usize>,
    /// How long every start after the first takes (a login that is slow to answer).
    slow: Duration,
    /// An event the adapter pushes the moment it connects, on every start after the first (a bridge that
    /// replays what it finds at the venue).
    push: Option<Event>,
    /// Whether that adapter also announces `ExecLink{up:true}` the moment it connects (a bridge that learned
    /// the venue at once), before the daemon has installed it.
    announce_learned: bool,
}

impl TradeLane for Flaky {
    fn venues(&self) -> Vec<String> {
        self.inner.venues()
    }
    fn account_tier(&self, venue: &str, account: Option<&str>) -> Result<AccountTier, ArmingBlock> {
        self.inner.account_tier(venue, account)
    }
    fn arming_table(&self) -> ArmingTable {
        self.inner.arming_table()
    }
    fn preflight(&self, venues: &[String], step: PreflightStep) -> PreflightAnswer {
        self.inner.preflight(venues, step)
    }
    fn start_adapter(
        &self,
        target: &TradeTarget,
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
                refusal_text: "the gateway is up and not logged in yet".into(),
            });
        }
        if n > 1 {
            std::thread::sleep(self.slow);
            if let Some(event) = &self.push {
                events.blocking_send(event.clone()).expect("the daemon's side is up");
            }
            if self.announce_learned {
                let update: vike_exec::ExecLinkUpdate = serde_json::from_value(serde_json::json!({
                    "route_key": "ibkr", "venue": "ibkr", "up": true, "at_ts_ms": 0
                }))
                .expect("an ExecLinkUpdate is these four fields");
                events.exec_link(update).expect("the daemon's side is up");
                // Let the forwarder take it BEFORE this start returns and the daemon installs the adapter:
                // the test is about an announcement that beats the install.
                std::thread::sleep(Duration::from_millis(200));
            }
        }
        self.inner.start_adapter(target, start, events, trigger)
    }
    fn record_identity(
        &self,
        target: &TradeTarget,
        bound_tier: AccountTier,
        recon: Option<&dyn ReconClient>,
    ) -> Result<(), String> {
        self.inner.record_identity(target, bound_tier, recon)
    }
    fn resolves_by_coid(&self, venue: &str) -> bool {
        self.inner.resolves_by_coid(venue)
    }
    fn link_watch(&self, target: &TradeTarget, start: &AdapterStart) -> Option<Arc<dyn LinkWatch>> {
        self.inner.link_watch(target, start)
    }
    fn recon_degraded(&self, venue: &str) -> Option<String> {
        self.inner.recon_degraded(venue)
    }
}

#[test]
fn a_restore_the_venue_refuses_is_retried_with_a_growing_wait() {
    let gw = FakeGateway::start();
    let rows = Rows::new(Ok(AccountTier::Demo));
    let scripted = ScriptedTrade::new(&["ibkr"]).with_link_watch("ibkr", watch(&gw, &rows));
    // Start 1 is the client's; starts 2 to 4 (the first three restores) are refused; the fifth works.
    let lane = Arc::new(Flaky {
        inner: scripted,
        starts: Mutex::new(Vec::new()),
        refuse: 2..=4,
        slow: Duration::ZERO,
        push: None,
        announce_learned: false,
    });
    let hub =
        TradeHub::new(Arc::clone(&lane) as Arc<dyn TradeLane>, cfg(), Arc::new(Budget::new()));
    let first = open(&hub, "tradehub-1", None);
    assert!(matches!(start(&first.lease, 7), AdapterReply::Live { .. }));
    let conn = connection(Arc::clone(&first.lease));
    gw.close();
    assert!(wait_until(WAIT, || !adapter_up(&hub)));
    gw.reopen();
    assert!(wait_until(WAIT, || adapter_up(&hub)), "the fourth restore comes up");
    conn.join().expect("the connection thread");
    let starts = lane.starts.lock().unwrap_or_else(PoisonError::into_inner).clone();
    assert_eq!(starts.len(), 5, "one client start and four restores");
    let gaps: Vec<Duration> = starts.windows(2).skip(1).map(|w| w[1] - w[0]).collect();
    // Waits owed after a refused restore: 40 ms, 80 ms, 160 ms; a pass is 20 ms, so allow its slack.
    assert!(gaps[0] >= Duration::from_millis(35), "{gaps:?}");
    assert!(gaps[1] >= Duration::from_millis(75), "{gaps:?}");
    assert!(gaps[2] >= Duration::from_millis(150), "{gaps:?}");
}

#[test]
fn a_venue_without_a_watch_is_not_supervised() {
    let gw = FakeGateway::start();
    let lane = ScriptedTrade::new(&["ibkr"]);
    let hub = hub_over(lane.clone());
    let first = open(&hub, "tradehub-1", None);
    assert!(matches!(start(&first.lease, 7), AdapterReply::Live { .. }));
    gw.close();
    std::thread::sleep(Duration::from_millis(200));
    assert!(adapter_up(&hub));
    assert_eq!(lane.starts(), 1);
}

#[test]
fn a_retired_session_is_never_revived() {
    let Up { gw, lane, hub, first, .. } = up();
    assert_eq!(first.lease.handle(TradeUp::Detach { phase: DetachPhase::Final }), Flow::End);
    first.lease.close();
    first.lease.finish();
    assert_eq!(hub.sessions(), 0, "the final detach retired the session");
    gw.close();
    std::thread::sleep(Duration::from_millis(150));
    gw.reopen();
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(lane.starts(), 1, "a detached account's gateway coming back starts nothing");
}

#[test]
fn a_stop_while_the_session_is_down_does_not_hang() {
    let Up { gw, hub, first, .. } = up();
    let conn = connection(Arc::clone(&first.lease));
    gw.close();
    assert!(wait_until(WAIT, || !adapter_up(&hub)));
    let started = Instant::now();
    hub.shutdown();
    assert!(started.elapsed() < Duration::from_secs(6), "the stop was not held by the supervisor");
    conn.join().expect("the connection thread");
}

#[test]
fn a_client_start_after_the_reconnect_gave_up_starts_afresh() {
    let Up { gw, rows, lane, hub, first } = up();
    let conn = connection(Arc::clone(&first.lease));
    gw.close();
    assert!(wait_until(WAIT, || !adapter_up(&hub)));
    rows.answer_with_the_gateway_back(&gw, Err(ArmingBlock::AccountInactive));
    assert!(wait_until(WAIT, || conn.ended()), "the lease was ended at the loss");
    first.lease.close();
    conn.join().expect("the connection thread");
    // The operator re-activates the account and the tradehub starts the adapter again. (The redial is
    // accepted only once the session has given up: see `gave_up_then_reopened`.)
    let second = redial_until_accepted(&hub, "tradehub-1");
    assert_eq!(lane.starts(), 1, "the rows said no with the gateway back: nothing was restored");
    assert!(matches!(start(&second.lease, 11), AdapterReply::Live { .. }));
    assert_eq!(lane.starts(), 2, "the client's own start is not refused by the old giving up");
    assert!(adapter_up(&hub));
}

/// **End to end with the REAL client**: the platform is told at the LOSS, not at the restore. The
/// client's `ExecLink` goes down (and `Disconnected` per mounted key) while the gateway is still away, and
/// stays down for as long as the daemon refuses its redial; it comes up (and `Live`) only when the venue
/// session is back. The new epoch makes the client reconcile, and the adapter it finds is the rebuilt one.
#[test]
fn the_real_client_is_told_at_the_loss_and_again_at_the_restore() {
    use vike_exec::Ingest;
    use vike_gatewayhub::testkit::gateway::TradeKit;
    use vike_gatewayhub_client::trade::{CoreLinkSignal, MountedKeys, TradeAccount, TradeLink};
    use vike_model::FeedStatus;

    let gw = FakeGateway::start();
    let rows = Rows::new(Ok(AccountTier::Demo));
    let lane = ScriptedTrade::new(&["ibkr"]).with_link_watch("ibkr", watch(&gw, &rows));
    let rig = trade_rig::rig_with(lane.clone(), trade_rig::quick_config(), TradeKit::default());

    let (tx, mut rx) = vike_exec::event_channel(4096);
    let events = tx.routed("ibkr");
    let account = TradeAccount {
        client_id: "tradehub-1".to_string(),
        venue: "ibkr".to_string(),
        label: None,
        tier: AccountTier::Demo,
        route_key: "ibkr".to_string(),
    };
    let keys = MountedKeys::new();
    keys.add("ibkr", "EUR.IDEALPRO.USD");
    let signal = CoreLinkSignal::new(&events, &account, keys);
    let session = TradeLink::new(rig.gw.trade_link(), account, events)
        .with_timings(trade_rig::fast_trade_timings())
        .with_signal(Arc::new(signal))
        .open()
        .expect("the lease opens");
    assert!(matches!(
        session.start_adapter(start_args()).expect("answered"),
        AdapterReply::Live { .. }
    ));
    assert!(session.status().expect("status").adapter_up);

    fn note(seen: &mut Vec<(&'static str, bool)>, ingest: Ingest) {
        match ingest {
            Ingest::ExecLink(u) => seen.push(("exec_link", u.up)),
            Ingest::StreamStatus(s) if s.status == FeedStatus::Disconnected => {
                seen.push(("stream", false));
            }
            Ingest::StreamStatus(s) if s.status == FeedStatus::Live => {
                seen.push(("stream", true));
            }
            _ => {}
        }
    }
    let mut seen: Vec<(&'static str, bool)> = Vec::new();
    let mut drain = |seen: &mut Vec<(&'static str, bool)>| {
        while let Ok(i) = rx.try_recv() {
            note(seen, i);
        }
    };
    drain(&mut seen);
    assert!(seen.is_empty(), "the first connection announces nothing: {seen:?}");

    // The gateway goes away: the platform hears of it NOW, with the gateway still gone.
    gw.close();
    let deadline = Instant::now() + WAIT;
    while Instant::now() < deadline
        && !(seen.contains(&("exec_link", false)) && seen.contains(&("stream", false)))
    {
        drain(&mut seen);
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(
        seen.contains(&("exec_link", false)) && seen.contains(&("stream", false)),
        "ExecLink down and Disconnected arrive while the gateway is still away: {seen:?}"
    );
    assert_eq!(lane.starts(), 1, "the gateway has not come back yet");

    // The client redials and is refused for as long as the venue is away: it must not say `Live`.
    std::thread::sleep(Duration::from_millis(1_200));
    drain(&mut seen);
    assert!(
        !seen.contains(&("exec_link", true)) && !seen.contains(&("stream", true)),
        "no link-up is announced while the venue session is down: {seen:?}"
    );

    // The venue is back: ExecLink up, Live, a reconcile on the new epoch.
    gw.reopen();
    let deadline = Instant::now() + WAIT;
    while Instant::now() < deadline
        && !(seen.contains(&("exec_link", true)) && seen.contains(&("stream", true)))
    {
        drain(&mut seen);
        std::thread::sleep(Duration::from_millis(10));
    }
    let pos = |what: (&'static str, bool)| seen.iter().position(|s| *s == what);
    assert!(
        pos(("exec_link", false)) < pos(("exec_link", true)) && pos(("exec_link", false)).is_some(),
        "ExecLink down, then up: {seen:?}"
    );
    assert!(
        pos(("stream", false)) < pos(("stream", true)) && pos(("stream", false)).is_some(),
        "Disconnected, then Live, per mounted key: {seen:?}"
    );
    assert_eq!(
        seen.iter().filter(|s| **s == ("exec_link", false)).count(),
        1,
        "one loss, one ExecLink down: {seen:?}"
    );
    assert!(wait_until(WAIT, || session.status().is_ok_and(|s| s.adapter_up)));
    assert_eq!(
        lane.starts(),
        2,
        "the client's redial found the rebuilt adapter and started no third"
    );
}

/// An open that arrives while the restore's start is inside the venue's (slow) login is refused AT ONCE:
/// it used to wait on the session's open lock for the whole login, and so did the reaper and the daemon's
/// stop. The command arriving in the same window is the lease's, which no longer exists: the client is
/// refused with words and retries on its own schedule.
#[test]
fn an_open_during_a_slow_rebuild_is_refused_at_once_and_does_not_wait_for_the_login() {
    let gw = FakeGateway::start();
    let rows = Rows::new(Ok(AccountTier::Demo));
    let scripted = ScriptedTrade::new(&["ibkr"]).with_link_watch("ibkr", watch(&gw, &rows));
    let lane = Arc::new(Flaky {
        inner: scripted.clone(),
        starts: Mutex::new(Vec::new()),
        refuse: 0..=0,
        slow: Duration::from_millis(1_500),
        push: None,
        announce_learned: false,
    });
    let hub =
        TradeHub::new(Arc::clone(&lane) as Arc<dyn TradeLane>, cfg(), Arc::new(Budget::new()));
    let first = open(&hub, "tradehub-1", None);
    assert!(matches!(start(&first.lease, 7), AdapterReply::Live { .. }));
    let conn = connection(Arc::clone(&first.lease));
    gw.close();
    assert!(wait_until(WAIT, || !adapter_up(&hub)));
    gw.reopen();
    // The restore's start is asked and takes 1.5 s.
    assert!(wait_until(WAIT, || {
        lane.starts.lock().unwrap_or_else(PoisonError::into_inner).len() == 2
    }));
    let asked = Instant::now();
    let text = refusal_text(hub.open(request("tradehub-1", None)));
    assert!(
        asked.elapsed() < Duration::from_millis(700),
        "the open waited {:?} on the login",
        asked.elapsed()
    );
    assert!(text.contains("re-establishing"), "{text}");
    assert!(wait_until(WAIT, || adapter_up(&hub)), "the rebuild completes");
    conn.join().expect("the connection thread");
    // The client's redial now succeeds: it finds the rebuilt session.
    let _second = open(&hub, "tradehub-1", None);
}

#[test]
fn a_session_with_no_client_attached_is_still_brought_back_and_not_reaped_meanwhile() {
    let Up { gw, lane, hub, first, .. } = up();
    first.lease.close();
    first.lease.finish();
    assert_eq!(hub.sessions(), 1, "the orphan keeps its adapter for the orphan timeout");
    gw.close();
    assert!(wait_until(WAIT, || !adapter_up(&hub)));
    // Longer than the reaper's tick: a session waiting to be rebuilt is not an orphan with no adapter.
    std::thread::sleep(Duration::from_millis(1_300));
    assert_eq!(hub.sessions(), 1, "the supervised session was not reaped while it is down");
    gw.reopen();
    assert!(
        wait_until(WAIT, || lane.starts() == 2 && adapter_up(&hub)),
        "and it is rebuilt (starts {}, status {:?}, sessions {})",
        lane.starts(),
        hub.session_status("ibkr", None),
        hub.sessions()
    );
    // The next client finds a new epoch and the reconcile it is owed.
    let second = open(
        &hub,
        "tradehub-1",
        Some(WireCursor { gw_epoch: first.reply.gw_epoch, seq: 1, ts_ms: 0 }),
    );
    assert_ne!(second.reply.gw_epoch, first.reply.gw_epoch);
    let mut owed = false;
    for _ in 0..2 {
        if matches!(expect_frame(&second.lease), TradeDown::ReconcileNow { .. }) {
            owed = true;
        }
    }
    assert!(owed, "the reconcile owed while nobody was attached is the new lease's first frame");
}

// ---- review of W14: the bridge's own report, the window of the loss, a refused start, a live rebuild ----

/// W14 doubt 1 / review finding 2: a bridge that DISCLOSES its death says its link is gone (`ExecLink{up:false}` on
/// its event lane). The scripted adapter plays it (the real bridge loop sends the same
/// `ExecLink`, `crates/bridges/vike-ibkr/tests/ibkr_lifecycle.rs`). The probe period here is 30 s, so a session
/// that is rebuilt inside the test's wait was found by the REPORT, not by a probe, and the gateway's port never even closed.
#[test]
fn the_bridge_reporting_its_link_down_takes_the_session_down_at_once() {
    vike_log::test_init();
    let Up { lane, hub, first, gw: _gw, .. } = up_on(SLOW);
    let a1 = lane.adapter("ibkr", None).expect("the first adapter");
    let conn = connection(Arc::clone(&first.lease));
    assert!(a1.emit_exec_link(false), "the bridge discloses its death");
    assert!(wait_until(WAIT, || a1.is_detached()), "the dead adapter is torn down at the report");
    assert!(wait_until(WAIT, || conn.ended()), "and the platform is told: the lease is ended");
    assert!(
        wait_until(WAIT, || lane.starts() == 2 && adapter_up(&hub)),
        "the session is rebuilt, not left for a probe 30 s away"
    );
}

/// Review finding 4: commands queued behind the one in the bridge's hands when the loss is seen used to
/// run against the dying bridge. They are answered instead: a submit with the terminal reject that says
/// NOT sent (which is true: it never left the daemon), written to the client before the lease ends.
#[test]
fn commands_queued_at_the_loss_are_answered_not_sent() {
    let Up { gw, lane, hub, first, .. } = up();
    let a1 = lane.adapter("ibkr", None).expect("the first adapter");
    // The first submit blocks inside the exec client (the bridge is busy) until the gate opens.
    let gate = Arc::new((Mutex::new(false), Condvar::new()));
    let hold = Arc::clone(&gate);
    a1.on_submit(move |_, _| {
        let (open, cv) = &*hold;
        let mut open = open.lock().unwrap_or_else(PoisonError::into_inner);
        let deadline = Instant::now() + WAIT;
        while !*open && Instant::now() < deadline {
            open = cv
                .wait_timeout(open, Duration::from_millis(50))
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    });
    let conn = connection_paced(Arc::clone(&first.lease), Duration::from_millis(120));
    for (cseq, coid) in [(1, "c-1"), (2, "c-2"), (3, "c-3")] {
        assert_eq!(
            first.lease.handle(TradeUp::Submit { cseq, order: order(coid) }),
            Flow::Continue
        );
    }
    assert!(wait_until(WAIT, || a1.submitted().len() == 1), "c-1 is in the bridge's hands");

    gw.close();
    assert!(wait_until(WAIT, || !adapter_up(&hub)), "the loss is seen");
    // The bridge's call returns; the queue behind it must not be handed to it.
    {
        let (open, cv) = &*gate;
        *open.lock().unwrap_or_else(PoisonError::into_inner) = true;
        cv.notify_all();
    }
    assert!(wait_until(WAIT, || conn.ended()), "the lease ends at the loss");
    let (frames, _) = conn.join().expect("the connection thread");
    let said = rejected(&frames);
    assert!(said.contains(&"c-2".to_string()) && said.contains(&"c-3".to_string()), "{said:?}");
    assert!(
        reasons(&frames).iter().filter(|r| r.contains("NOT sent")).count() >= 2,
        "the queued orders are rejected with the words that they were not sent"
    );
    assert!(
        !said.contains(&"c-1".to_string()),
        "the order in the bridge's hands is not judged here"
    );
    let sent: Vec<String> = a1.submitted().iter().map(|o| o.client_order_id.clone()).collect();
    assert_eq!(sent, ["c-1"], "nothing queued behind it reached the dying adapter");
}

/// Review finding 4, second half: the teardown used to pre-empt the bridge's own naming of the orders in
/// flight. A loss the PROBE found waits a bounded moment for a bridge that REPORTS (scripted here; today's
/// IBKR bridge reports nothing, so in production this wait always runs out) before the adapter is torn down.
#[test]
fn a_loss_found_by_the_probe_waits_for_the_bridge_to_name_its_orders_before_the_teardown() {
    vike_log::test_init();
    // A probe every 100 ms: the second failed probe is the loss; the grace is five probe periods.
    const PROBE_100: Schedule = Schedule {
        probe_every: Duration::from_millis(100),
        first_wait: Duration::from_millis(40),
        max_wait: Duration::from_millis(160),
    };
    let Up { gw, lane, first, hub: _hub, .. } = up_on(PROBE_100);
    let a1 = lane.adapter("ibkr", None).expect("the first adapter");
    let conn = connection(Arc::clone(&first.lease));
    let reported = Arc::new(AtomicBool::new(false));
    {
        let (reported, a1) = (Arc::clone(&reported), a1.clone());
        gw.close();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(350));
            reported.store(true, Ordering::SeqCst);
            a1.emit_exec_link(false);
        });
    }
    assert!(wait_until(WAIT, || a1.is_detached()), "the adapter is torn down");
    assert!(
        reported.load(Ordering::SeqCst),
        "the bridge was given the moment to report before it was torn down"
    );
    assert!(wait_until(WAIT, || conn.ended()));
}

/// Review finding 9: a client's DIFFERENT `AdapterStart` tears the old adapter down, the venue refuses
/// the new one, and the old supervisor used to stay: a session with no adapter and a live supervisor
/// that nothing reaps and nothing can restore.
#[test]
fn a_refused_client_start_leaves_no_supervisor_behind() {
    let gw = FakeGateway::start();
    let rows = Rows::new(Ok(AccountTier::Demo));
    let scripted = ScriptedTrade::new(&["ibkr"]).with_link_watch("ibkr", watch(&gw, &rows));
    // Start 1 is the client's first; start 2 (a different start) is refused.
    let lane = Arc::new(Flaky {
        inner: scripted,
        starts: Mutex::new(Vec::new()),
        refuse: 2..=2,
        slow: Duration::ZERO,
        push: None,
        announce_learned: false,
    });
    let hub =
        TradeHub::new(Arc::clone(&lane) as Arc<dyn TradeLane>, cfg(), Arc::new(Budget::new()));
    let first = open(&hub, "tradehub-1", None);
    assert!(matches!(start(&first.lease, 7), AdapterReply::Live { .. }));

    let mut different = start_args();
    different.rid = 8;
    different.route_key = "ibkr".into();
    different.symbol = "EUR.IDEALPRO.USD".into();
    assert_eq!(first.lease.handle(TradeUp::AdapterStart(different)), Flow::Continue);
    match expect_frame(&first.lease) {
        TradeDown::AdapterReply { reply, .. } => {
            assert!(matches!(*reply, AdapterReply::NotRunning { .. }), "{reply:?}")
        }
        other => panic!("{other:?}"),
    }
    // The client goes away. A session with no adapter, no lease and no supervisor is an orphan.
    first.lease.close();
    first.lease.finish();
    assert!(
        wait_until(WAIT, || hub.sessions() == 0),
        "the orphan with no adapter is reaped: no supervisor is left holding it"
    );
}

/// Review finding 6: a LIVE rebuild is unattended, so the rows are asked once more right before the
/// start, not only at the top of the pass. The first read after the port returns says live, the one
/// right before the start says the account was deactivated: no live adapter is started.
#[test]
fn a_live_rebuild_asks_the_rows_again_right_before_the_start() {
    let gw = FakeGateway::start();
    let rows = Rows::new(Ok(AccountTier::Live));
    let lane = ScriptedTrade::new(&["ibkr"])
        .with_tier("ibkr", None, Ok(AccountTier::Live))
        .with_link_watch("ibkr", watch(&gw, &rows));
    let hub = hub_over(lane.clone());
    let first = hub
        .open(request_at("tradehub-1", AccountTier::Live, None))
        .map_err(|e| format!("{e:?}"))
        .expect("the live open is accepted");
    assert!(matches!(start(&first.lease, 7), AdapterReply::Live { .. }));
    let _conn = connection(Arc::clone(&first.lease));
    assert!(adapter_up(&hub));

    gw.close();
    assert!(wait_until(WAIT, || !adapter_up(&hub)));
    // Once the port is back: the first read (the pass's verdict) says live, every later one says the
    // account is no longer active.
    let after = Arc::new(AtomicUsize::new(0));
    {
        let (gw, after) = (Arc::clone(&gw), Arc::clone(&after));
        rows.scripted(move |_| {
            if gw.is_open() && after.fetch_add(1, Ordering::SeqCst) >= 1 {
                Err(ArmingBlock::AccountInactive)
            } else {
                Ok(AccountTier::Live)
            }
        });
    }
    gw.reopen();
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(
        lane.starts(),
        1,
        "no live adapter is started after the rows said the account stopped"
    );
    assert!(!adapter_up(&hub));
}

#[test]
fn a_live_session_is_rebuilt_unattended_while_the_account_is_still_active() {
    let gw = FakeGateway::start();
    let rows = Rows::new(Ok(AccountTier::Live));
    let lane = ScriptedTrade::new(&["ibkr"])
        .with_tier("ibkr", None, Ok(AccountTier::Live))
        .with_link_watch("ibkr", watch(&gw, &rows));
    let hub = hub_over(lane.clone());
    let first = hub
        .open(request_at("tradehub-1", AccountTier::Live, None))
        .map_err(|e| format!("{e:?}"))
        .expect("the live open is accepted");
    assert!(matches!(start(&first.lease, 7), AdapterReply::Live { .. }));
    let _conn = connection(Arc::clone(&first.lease));
    gw.close();
    assert!(wait_until(WAIT, || !adapter_up(&hub)));
    gw.reopen();
    assert!(wait_until(WAIT, || lane.starts() == 2 && adapter_up(&hub)));
}

/// Review finding 9: the reconcile a restore owes went to `inner.lease` if one was still set, even when
/// that lease's connection was already going away (it never finished inside the fence), and was lost
/// there. It now waits for the next lease like any request made while nobody is attached.
#[test]
fn the_reconcile_a_restore_owes_is_not_sent_into_a_lease_that_is_going_away() {
    let Up { gw, lane, hub, first, .. } = up();
    // The connection is closed but never finished: no thread plays its writer, so the session still holds
    // it when the loss and the restore come.
    first.lease.close();
    gw.close();
    assert!(wait_until(WAIT, || !adapter_up(&hub)), "the loss is seen");
    gw.reopen();
    assert!(
        wait_until(WAIT, || lane.starts() == 2 && adapter_up(&hub)),
        "and the session is rebuilt"
    );
    first.lease.finish();

    let second = open(
        &hub,
        "tradehub-1",
        Some(WireCursor { gw_epoch: first.reply.gw_epoch, seq: 1, ts_ms: 0 }),
    );
    let mut owed = false;
    for _ in 0..2 {
        if matches!(expect_frame(&second.lease), TradeDown::ReconcileNow { .. }) {
            owed = true;
        }
    }
    assert!(owed, "the reconcile owed at the restore is the new lease's first frame, not lost");
}

// ---- W14c: the rebuilt bridge is blind to the orders of the one before it; the first events of a rebuild ----

/// A hub over a `Flaky` lane (never refusing) whose restores take `slow` and whose rebuilt adapter pushes
/// `push` the moment it connects.
fn flaky_hub(
    gw: &Arc<FakeGateway>,
    rows: &Arc<Rows>,
    slow: Duration,
    push: Option<Event>,
    announce_learned: bool,
) -> (Arc<Flaky>, ScriptedTrade, Arc<TradeHub>) {
    let scripted = ScriptedTrade::new(&["ibkr"]).with_link_watch("ibkr", watch(gw, rows));
    let lane = Arc::new(Flaky {
        inner: scripted.clone(),
        starts: Mutex::new(Vec::new()),
        refuse: 0..=0,
        slow,
        push,
        announce_learned,
    });
    let hub =
        TradeHub::new(Arc::clone(&lane) as Arc<dyn TradeLane>, cfg(), Arc::new(Budget::new()));
    (lane, scripted, hub)
}

/// A venue event that finishes `coid` (the venue cancelled it).
fn cancelled(coid: &str) -> Event {
    Event::OrderCanceled(OrderCanceled {
        client_order_id: coid.to_string(),
        reason: "".into(),
        ts: 0,
    })
}

/// Wait until the ring holds at least `n` events (the forwarder has stamped them).
fn ring_holds(hub: &TradeHub, n: u64) -> bool {
    wait_until(WAIT, || hub.session_status("ibkr", None).is_some_and(|s| s.last_seq >= n))
}

/// Take the gateway away and bring it back with the daemon rebuilding the adapter; return the second
/// lease, opened on the old cursor, with its two leading frames (`ReplayUnavailable`, `ReconcileNow`)
/// already read.
fn lose_and_rebuild(
    gw: &FakeGateway,
    hub: &TradeHub,
    lane: &Flaky,
    first: &Opened,
    conn: Conn,
) -> Opened {
    gw.close();
    assert!(wait_until(WAIT, || !adapter_up(hub)), "the loss is seen");
    assert!(wait_until(WAIT, || conn.ended()), "the lease ends at the loss");
    gw.reopen();
    assert!(
        wait_until(REBUILD_WAIT, || {
            lane.starts.lock().unwrap_or_else(PoisonError::into_inner).len() == 2 && adapter_up(hub)
        }),
        "the adapter is rebuilt"
    );
    let second = open(
        hub,
        "tradehub-1",
        Some(WireCursor { gw_epoch: first.reply.gw_epoch, seq: 1, ts_ms: 0 }),
    );
    for _ in 0..2 {
        assert!(matches!(
            expect_frame(&second.lease),
            TradeDown::ReplayUnavailable { .. } | TradeDown::ReconcileNow { .. }
        ));
    }
    second
}

/// **The rebuilt adapter cannot cancel, modify or follow an order the lost one placed** (the IBKR bridge
/// does not learn open orders), and an engine must never believe it can. A session rebuilt over an order it
/// sent and never saw finish is HELD: a submit is a terminal reject, a cancel and a modify are advisories,
/// all with the words that say why, and none reaches the adapter. A query still goes through.
#[test]
fn a_rebuild_over_orders_that_may_rest_holds_the_session_and_refuses_every_order_verb() {
    let gw = FakeGateway::start();
    let rows = Rows::new(Ok(AccountTier::Demo));
    let (lane, scripted, hub) = flaky_hub(&gw, &rows, Duration::ZERO, None, false);
    let first = open(&hub, "tradehub-1", None);
    assert!(matches!(start(&first.lease, 7), AdapterReply::Live { .. }));
    let a1 = scripted.adapter("ibkr", None).expect("the first adapter");
    for (cseq, coid) in [(1, "c-rest"), (2, "c-done")] {
        assert_eq!(
            first.lease.handle(TradeUp::Submit { cseq, order: order(coid) }),
            Flow::Continue
        );
    }
    assert!(wait_until(WAIT, || a1.submitted().len() == 2));
    // One of the two finishes (the venue cancelled it); the other rests, or is in flight: the daemon never
    // saw it finish.
    assert!(a1.emit(cancelled("c-done")));
    assert!(ring_holds(&hub, 1));
    let conn = connection(Arc::clone(&first.lease));

    let second = lose_and_rebuild(&gw, &hub, &lane, &first, conn);
    assert!(matches!(start(&second.lease, 8), AdapterReply::Live { .. }));
    let a2 = scripted.adapter("ibkr", None).expect("the rebuilt adapter");
    let conn2 = connection(Arc::clone(&second.lease));

    let handle = |up: TradeUp| assert_eq!(second.lease.handle(up), Flow::Continue);
    handle(TradeUp::Submit { cseq: 1, order: order("c-new") });
    handle(TradeUp::Cancel {
        cseq: 2,
        coid: "c-rest".into(),
        intent: vike_gatewayhub_client::proto::WireCancelIntent::RiskOff,
    });
    handle(TradeUp::Modify {
        cseq: 3,
        order: order("c-rest"),
        new_qty: Some(2.0),
        new_price: None,
    });
    handle(TradeUp::SubmitBatch { cseq: 4, orders: vec![order("c-b1"), order("c-b2")] });
    handle(TradeUp::Confirm { cseq: 5, coid: "c-rest".into() });
    assert!(conn2.wait_reason("resting orders from before the outage are not manageable"));
    assert!(wait_until(WAIT, || {
        let frames = conn2.frames.lock().unwrap_or_else(PoisonError::into_inner).clone();
        reasons(&frames).len() == 5
    }));
    let frames = conn2.frames.lock().unwrap_or_else(PoisonError::into_inner).clone();
    let said = rejected(&frames);
    for coid in ["c-new", "c-b1", "c-b2"] {
        assert!(said.contains(&coid.to_string()), "{coid} is rejected: {said:?}");
    }
    let words = reasons(&frames);
    assert_eq!(words.len(), 5, "three rejects, a cancel advisory, a modify advisory: {words:?}");
    assert!(
        words
            .iter()
            .all(|w| w.contains("1 order(s)") && w.contains("c-rest") && w.contains("NOT sent")),
        "the words name the one order that may rest, and that nothing was sent: {words:?}"
    );
    assert!(
        !words.iter().any(|w| w.contains("c-done")),
        "a finished order is not named: {words:?}"
    );
    // Two calls, in this order (the handover, then the query): waiting for the first would read a half-written list.
    assert!(
        wait_until(WAIT, || a2.calls().len() >= 2),
        "the handover and the query reached the adapter: {:?}",
        a2.calls()
    );
    assert_eq!(
        a2.calls(),
        vec![
            // The handover (finding 1 of the third IBKR review): the rebuilt client is taught the symbol of every
            // order the ledger holds, before anything else reaches it.
            ExecCall::AdoptRestored(vec![("c-rest".to_string(), "BTCUSDT".to_string())]),
            ExecCall::Confirm("c-rest".to_string())
        ],
        "the handover, then only the query reached the rebuilt adapter, no order verb"
    );
}

/// A rebuild over orders that all finished is an ordinary rebuild: the rebuilt adapter takes orders.
#[test]
fn a_rebuild_over_orders_that_all_finished_is_not_held() {
    let gw = FakeGateway::start();
    let rows = Rows::new(Ok(AccountTier::Demo));
    let (lane, scripted, hub) = flaky_hub(&gw, &rows, Duration::ZERO, None, false);
    let first = open(&hub, "tradehub-1", None);
    assert!(matches!(start(&first.lease, 7), AdapterReply::Live { .. }));
    let a1 = scripted.adapter("ibkr", None).expect("the first adapter");
    assert_eq!(
        first.lease.handle(TradeUp::Submit { cseq: 1, order: order("c-gone") }),
        Flow::Continue
    );
    assert!(wait_until(WAIT, || a1.submitted().len() == 1));
    assert!(a1.emit(cancelled("c-gone")));
    assert!(ring_holds(&hub, 1));
    let conn = connection(Arc::clone(&first.lease));

    let second = lose_and_rebuild(&gw, &hub, &lane, &first, conn);
    assert!(matches!(start(&second.lease, 8), AdapterReply::Live { .. }));
    let a2 = scripted.adapter("ibkr", None).expect("the rebuilt adapter");
    assert_eq!(
        second.lease.handle(TradeUp::Submit { cseq: 1, order: order("c-next") }),
        Flow::Continue
    );
    assert!(
        wait_until(WAIT, || a2.submitted().iter().any(|o| o.client_order_id == "c-next")),
        "nothing may rest, so the rebuilt adapter takes the order"
    );
}

/// F4 (re-review of Part B): the fills the rebuilt bridge recovers for an order that finished while it was down
/// arrive as PARTIAL fills (it does not know the total). They add up to the quantity the order asked for, so the
/// ledger retires it, and a later rebuild is an ordinary one instead of held for ever.
#[test]
fn a_rebuild_over_an_order_whose_recovered_fills_add_up_to_its_quantity_is_not_held() {
    let gw = FakeGateway::start();
    let rows = Rows::new(Ok(AccountTier::Demo));
    let (lane, scripted, hub) = flaky_hub(&gw, &rows, Duration::ZERO, None, false);
    let first = open(&hub, "tradehub-1", None);
    assert!(matches!(start(&first.lease, 7), AdapterReply::Live { .. }));
    let a1 = scripted.adapter("ibkr", None).expect("the first adapter");
    assert_eq!(
        first.lease.handle(TradeUp::Submit { cseq: 1, order: order("c-pieces") }),
        Flow::Continue
    );
    assert!(wait_until(WAIT, || a1.submitted().len() == 1));
    // `order()` asks for 1.0: two pieces, one of them replayed.
    assert!(a1.emit(partial_fill("c-pieces", "t1", 0.4)));
    assert!(a1.emit(partial_fill("c-pieces", "t1", 0.4)));
    assert!(a1.emit(partial_fill("c-pieces", "t2", 0.6)));
    assert!(ring_holds(&hub, 3));
    let conn = connection(Arc::clone(&first.lease));

    let second = lose_and_rebuild(&gw, &hub, &lane, &first, conn);
    assert!(matches!(start(&second.lease, 8), AdapterReply::Live { .. }));
    let a2 = scripted.adapter("ibkr", None).expect("the rebuilt adapter");
    assert_eq!(
        second.lease.handle(TradeUp::Submit { cseq: 1, order: order("c-next") }),
        Flow::Continue
    );
    assert!(
        wait_until(WAIT, || a2.submitted().iter().any(|o| o.client_order_id == "c-next")),
        "the order's fills add up to its quantity: nothing may rest, so the rebuilt adapter takes the order"
    );
}

fn partial_fill(coid: &str, trade_id: &str, qty: f64) -> Event {
    Event::OrderPartiallyFilled(vike_model::events::OrderPartiallyFilled {
        client_order_id: coid.to_string(),
        fill: vike_model::events::FillEvent {
            trade_id: vike_model::events::TradeId::new(trade_id).expect("a trade id"),
            client_order_id: coid.to_string(),
            venue: "ibkr".into(),
            symbol: "BTCUSDT".into(),
            side: 1,
            last_qty: qty,
            last_px: 100.0,
            commission: 0.0,
            commission_asset: "USD".into(),
            liquidity_side: Default::default(),
            ts: 0,
            mark_price: None,
            position_side: Default::default(),
        },
        ts: 0,
    })
}

/// Review of W14b, finding 3: the swap to the new epoch came AFTER the rebuilt bridge had pushed what it
/// found at the venue, so those events went to the old ring and were never delivered. The ring is swapped
/// before the adapter exists, and the first lease starts at its first event.
#[test]
fn what_the_rebuilt_adapter_pushes_the_moment_it_connects_reaches_the_first_lease() {
    let gw = FakeGateway::start();
    let rows = Rows::new(Ok(AccountTier::Demo));
    let (lane, _scripted, hub) =
        flaky_hub(&gw, &rows, Duration::ZERO, Some(trade_rig::accepted("c-replayed")), false);
    let first = open(&hub, "tradehub-1", None);
    assert!(matches!(start(&first.lease, 7), AdapterReply::Live { .. }));
    let conn = connection(Arc::clone(&first.lease));

    let second = lose_and_rebuild(&gw, &hub, &lane, &first, conn);
    let got = expect_frame(&second.lease);
    assert!(
        matches!(
            &got,
            TradeDown::Event { event: Event::OrderAccepted(a), .. } if a.client_order_id == "c-replayed"
        ),
        "the replay the rebuilt adapter pushed at connect is the first lease's first event: {got:?}"
    );
}

/// **The first-wins memory dies with the ring it guards** (`OnceMemory`, cleared where `link_restore` swaps the
/// ring, BEFORE the adapter exists). The first adapter accepted `c-rest` in the old epoch; the rebuilt bridge
/// REBINDS the same coid the moment it connects, before any lease is attached (an open without a replayable
/// cursor clears the memory too, but only after this event has been judged), and the client's resolution of
/// an order it holds UNKNOWN waits for exactly that accept. Taken for a repeat of the lost adapter's, it would
/// be dropped and the first lease of the new epoch would never hear it.
#[test]
fn what_the_rebuilt_adapter_rebinds_at_connect_is_not_taken_for_a_repeat_of_the_lost_adapters() {
    let gw = FakeGateway::start();
    let rows = Rows::new(Ok(AccountTier::Demo));
    let (lane, scripted, hub) =
        flaky_hub(&gw, &rows, Duration::ZERO, Some(named_accepted("c-rest")), false);
    let first = open(&hub, "tradehub-1", None);
    assert!(matches!(start(&first.lease, 7), AdapterReply::Live { .. }));
    let a1 = scripted.adapter("ibkr", None).expect("the first adapter");
    assert!(a1.emit(named_accepted("c-rest")), "the first adapter accepted c-rest");
    let conn = connection(Arc::clone(&first.lease));

    let second = lose_and_rebuild(&gw, &hub, &lane, &first, conn);
    let got = expect_frame(&second.lease);
    assert!(
        matches!(
            &got,
            TradeDown::Event { event: Event::OrderAccepted(a), .. } if a.client_order_id == "c-rest"
        ),
        "the rebind the rebuilt adapter pushed at connect is the first lease's first event: {got:?}"
    );
}

/// Review of W14b, finding 10: the restore held the adapter lock through the venue's login, so the daemon's
/// stop (which takes it to tear the adapter down) waited for a login that can take as long as the gateway
/// takes to answer. The login holds no lock; the restore installs the adapter under the lock afterwards
/// and, finding the session retired, tears the adapter down itself.
#[test]
fn a_stop_during_a_slow_login_does_not_wait_for_it_and_leaves_no_adapter_behind() {
    let gw = FakeGateway::start();
    let rows = Rows::new(Ok(AccountTier::Demo));
    let (lane, scripted, hub) = flaky_hub(&gw, &rows, Duration::from_millis(2_500), None, false);
    let first = open(&hub, "tradehub-1", None);
    assert!(matches!(start(&first.lease, 7), AdapterReply::Live { .. }));
    let conn = connection(Arc::clone(&first.lease));
    gw.close();
    assert!(wait_until(WAIT, || !adapter_up(&hub)));
    gw.reopen();
    // The restore's start is asked and is now inside the login.
    assert!(wait_until(WAIT, || {
        lane.starts.lock().unwrap_or_else(PoisonError::into_inner).len() == 2
    }));
    let stopped = Instant::now();
    hub.shutdown();
    assert!(
        stopped.elapsed() < Duration::from_millis(1_000),
        "the stop waited {:?} for the login",
        stopped.elapsed()
    );
    conn.join().expect("the connection thread");
    // The login finishes later: the adapter it built is torn down by the restore, not left running.
    assert!(wait_until(WAIT, || scripted.starts() == 2), "the slow login completes");
    let a2 = scripted.adapter("ibkr", None).expect("the adapter the login built");
    assert!(wait_until(WAIT, || a2.is_detached()), "and it is torn down, not left behind");
}

/// The same blindness without an outage: a client's DIFFERENT `AdapterStart` replaces the running bridge by a
/// new one, which knows none of the orders the old one placed. A supervised session holds in that case too.
#[test]
fn a_different_start_that_replaces_the_bridge_over_resting_orders_holds_the_session_too() {
    let gw = FakeGateway::start();
    let rows = Rows::new(Ok(AccountTier::Demo));
    let (_lane, scripted, hub) = flaky_hub(&gw, &rows, Duration::ZERO, None, false);
    let first = open(&hub, "tradehub-1", None);
    assert!(matches!(start(&first.lease, 7), AdapterReply::Live { .. }));
    let a1 = scripted.adapter("ibkr", None).expect("the first adapter");
    assert_eq!(
        first.lease.handle(TradeUp::Submit { cseq: 1, order: order("c-rest") }),
        Flow::Continue
    );
    assert!(wait_until(WAIT, || a1.submitted().len() == 1));

    let mut different = start_args();
    different.rid = 8;
    different.route_key = "ibkr".into();
    different.symbol = "EUR.IDEALPRO.USD".into();
    assert_eq!(first.lease.handle(TradeUp::AdapterStart(different)), Flow::Continue);
    // The submit above was acknowledged first (a CmdAck); the start's answer follows.
    loop {
        match expect_frame(&first.lease) {
            TradeDown::CmdAck { .. } => {}
            TradeDown::AdapterReply { reply, .. } => {
                assert!(matches!(*reply, AdapterReply::Live { .. }), "{reply:?}");
                break;
            }
            other => panic!("{other:?}"),
        }
    }
    let a2 = scripted.adapter("ibkr", None).expect("the new adapter");
    let conn = connection(Arc::clone(&first.lease));
    assert_eq!(
        first.lease.handle(TradeUp::Submit { cseq: 2, order: order("c-new") }),
        Flow::Continue
    );
    assert!(conn.wait_reason("not manageable"), "the new bridge is blind to c-rest: held");
    assert!(a2.submitted().is_empty(), "and nothing reached it");
}

// ---- W14c B: the bridge reports its own death, and its learning of the venue releases the hold ------------

/// Submit `coid` repeatedly (a fresh coid per try) until the rebuilt adapter RECEIVES one: the moment the
/// session stopped holding. Returns whether it did within the wait.
fn released(
    lease: &Arc<Lease>,
    adapter: &vike_gatewayhub::testkit::trade::ScriptedAdapter,
) -> bool {
    let mut n = 0u64;
    wait_until(WAIT, || {
        n += 1;
        let _ =
            lease.handle(TradeUp::Submit { cseq: 100 + n, order: order(&format!("c-try-{n}")) });
        std::thread::sleep(Duration::from_millis(40));
        !adapter.submitted().is_empty()
    })
}

/// The hold exists because a rebuilt bridge could not manage the orders of the one before it. A bridge
/// that has learned the venue says so (`ExecLink{up:true}`), and the session takes order verbs again.
#[test]
fn a_held_session_is_released_when_the_rebuilt_bridge_reports_it_has_learned_the_venue() {
    let gw = FakeGateway::start();
    let rows = Rows::new(Ok(AccountTier::Demo));
    let (lane, scripted, hub) = flaky_hub(&gw, &rows, Duration::ZERO, None, false);
    let first = open(&hub, "tradehub-1", None);
    assert!(matches!(start(&first.lease, 7), AdapterReply::Live { .. }));
    let a1 = scripted.adapter("ibkr", None).expect("the first adapter");
    assert_eq!(
        first.lease.handle(TradeUp::Submit { cseq: 1, order: order("c-rest") }),
        Flow::Continue
    );
    assert!(wait_until(WAIT, || a1.submitted().len() == 1));
    let conn = connection(Arc::clone(&first.lease));

    let second = lose_and_rebuild(&gw, &hub, &lane, &first, conn);
    assert!(matches!(start(&second.lease, 8), AdapterReply::Live { .. }));
    let a2 = scripted.adapter("ibkr", None).expect("the rebuilt adapter");
    let conn2 = connection(Arc::clone(&second.lease));

    // Held: a submit is refused with the words and the adapter receives nothing.
    assert_eq!(
        second.lease.handle(TradeUp::Submit { cseq: 1, order: order("c-held") }),
        Flow::Continue
    );
    assert!(conn2.wait_reason("not manageable"));
    assert!(a2.submitted().is_empty());

    // The rebuilt bridge reports it has learned the venue.
    assert!(a2.emit_exec_link(true));
    assert!(released(&second.lease, &a2), "the session takes order verbs again");
}

/// A bridge that learned the venue BEFORE the daemon finished installing it (its announcement beats the
/// install) is never held in the first place.
#[test]
fn a_bridge_that_reported_learned_before_it_was_installed_is_not_held() {
    let gw = FakeGateway::start();
    let rows = Rows::new(Ok(AccountTier::Demo));
    let (lane, scripted, hub) = flaky_hub(&gw, &rows, Duration::ZERO, None, true);
    let first = open(&hub, "tradehub-1", None);
    assert!(matches!(start(&first.lease, 7), AdapterReply::Live { .. }));
    let a1 = scripted.adapter("ibkr", None).expect("the first adapter");
    assert_eq!(
        first.lease.handle(TradeUp::Submit { cseq: 1, order: order("c-rest") }),
        Flow::Continue
    );
    assert!(wait_until(WAIT, || a1.submitted().len() == 1));
    let conn = connection(Arc::clone(&first.lease));

    let second = lose_and_rebuild(&gw, &hub, &lane, &first, conn);
    assert!(matches!(start(&second.lease, 8), AdapterReply::Live { .. }));
    let a2 = scripted.adapter("ibkr", None).expect("the rebuilt adapter");
    assert_eq!(
        second.lease.handle(TradeUp::Submit { cseq: 1, order: order("c-new") }),
        Flow::Continue
    );
    assert!(
        wait_until(WAIT, || a2.submitted().iter().any(|o| o.client_order_id == "c-new")),
        "no hold: the bridge had already said it learned the venue"
    );
}

/// The bridge's report that its link is down is acted on at once (a probe 30 s away does not matter), and
/// is consumed by the daemon: it reaches no client as an event.
#[test]
fn the_bridge_reporting_its_death_is_consumed_by_the_daemon_and_forwarded_nowhere() {
    vike_log::test_init();
    let Up { lane, hub, first, gw: _gw, .. } = up_on(SLOW);
    let a1 = lane.adapter("ibkr", None).expect("the first adapter");
    let conn = connection(Arc::clone(&first.lease));
    assert!(a1.emit_exec_link(true), "a link-up of the first adapter changes nothing");
    assert!(a1.emit_exec_link(false));
    assert!(wait_until(WAIT, || a1.is_detached()), "torn down at the report");
    assert!(wait_until(WAIT, || lane.starts() == 2 && adapter_up(&hub)), "and rebuilt");
    let (frames, _) = conn.join().expect("the connection thread");
    assert!(
        frames.iter().all(|f| !matches!(f, TradeDown::Event { .. })),
        "no ExecLink reached the client as an event: {frames:?}"
    );
}

/// **The REAL bridge's exec loop** (over its no-gateway transport double) under the gatewayhub's session.
#[cfg(feature = "ibkr")]
mod real_bridge {
    use vike_ibkr::run_exec_for_test;
    use vike_ibkr::testing::{FAKE_CLIENT_ID, FakeTransport, ScriptedInbound, SnapshotMode};

    use super::*;

    /// A lane that starts the real bridge loop, one scripted transport per generation of the adapter.
    struct BridgeLane {
        inner: ScriptedTrade,
        /// Builds generation `n`'s transport (1-based).
        script: Box<dyn Fn(usize) -> FakeTransport + Send + Sync>,
        made: Mutex<Vec<FakeTransport>>,
        /// Build the client the way production does (`IbkrExecutionClient`: the bridge holds its learning's
        /// executions replay for its host's first `adopt_restored`) instead of the bare actor.
        real_client: bool,
    }

    impl BridgeLane {
        fn transport(&self, generation: usize) -> FakeTransport {
            self.made.lock().unwrap_or_else(PoisonError::into_inner)[generation - 1].clone()
        }
    }

    impl TradeLane for BridgeLane {
        fn venues(&self) -> Vec<String> {
            self.inner.venues()
        }
        fn account_tier(
            &self,
            venue: &str,
            account: Option<&str>,
        ) -> Result<AccountTier, ArmingBlock> {
            self.inner.account_tier(venue, account)
        }
        fn arming_table(&self) -> ArmingTable {
            self.inner.arming_table()
        }
        fn preflight(&self, venues: &[String], step: PreflightStep) -> PreflightAnswer {
            self.inner.preflight(venues, step)
        }
        fn start_adapter(
            &self,
            target: &TradeTarget,
            _start: &AdapterStart,
            events: EventSender,
            _trigger: Option<Sender<()>>,
        ) -> Result<StartedAdapter, AdapterRefusal> {
            let n = self.made.lock().unwrap_or_else(PoisonError::into_inner).len() + 1;
            let fake = (self.script)(n);
            self.made.lock().unwrap_or_else(PoisonError::into_inner).push(fake.clone());
            let client: Box<dyn vike_exec::ExecutionClient + Send> = if self.real_client {
                let client = vike_ibkr::client_for_test(events, Box::new(fake.clone()));
                // The PRODUCTION ordering, with no pause: the start returns only once the bridge has done all
                // it can do ahead of its host. With the replay held for the handover that is: parked on it. A
                // bridge that did not hold it has asked for the replay and folded it by now, which is exactly
                // how the symbol teach arrived AFTER the replay on a 23:45 rebuild.
                assert!(
                    wait_until(WAIT, || {
                        client.awaiting_handover()
                            || (*fake.execution_requests().lock().unwrap() > 0
                                && fake.undelivered() == 0)
                    }),
                    "the bridge neither parked its replay nor ran it"
                );
                Box::new(client)
            } else {
                Box::new(run_exec_for_test(events, Box::new(fake)))
            };
            Ok(StartedAdapter {
                bound_tier: target.tier,
                grid: None,
                contract_value: None,
                margin_mode: None,
                leg_grids: Vec::new(),
                client,
                recon: None,
            })
        }
        fn record_identity(
            &self,
            target: &TradeTarget,
            bound_tier: AccountTier,
            recon: Option<&dyn ReconClient>,
        ) -> Result<(), String> {
            self.inner.record_identity(target, bound_tier, recon)
        }
        fn resolves_by_coid(&self, venue: &str) -> bool {
            self.inner.resolves_by_coid(venue)
        }
        fn link_watch(
            &self,
            target: &TradeTarget,
            start: &AdapterStart,
        ) -> Option<Arc<dyn LinkWatch>> {
            self.inner.link_watch(target, start)
        }
        fn recon_degraded(&self, venue: &str) -> Option<String> {
            self.inner.recon_degraded(venue)
        }
    }

    fn aapl(coid: &str) -> OrderRequest {
        OrderRequest { symbol: "AAPL.SMART.USD".into(), ..order(coid) }
    }

    /// What generation 1 does: the connect seed, and an accepted order for whatever is placed.
    fn first_generation(coid: &'static str) -> FakeTransport {
        let mut fake = FakeTransport::new().with_socket_identity();
        fake.script_socket_connect(101);
        fake.on_place_order(move |order_id| {
            vec![ScriptedInbound::OrderStatus {
                order_id,
                order_ref: coid.into(),
                status: "Submitted".into(),
            }]
        });
        fake
    }

    struct Rig {
        gw: Arc<FakeGateway>,
        lane: Arc<BridgeLane>,
        hub: Arc<TradeHub>,
        first: Opened,
    }

    /// A session over the real bridge, with `c-rest` placed and accepted by generation 1.
    fn rig(second_generation: impl Fn() -> FakeTransport + Send + Sync + 'static) -> Rig {
        rig_with("AAPL.SMART.USD", false, second_generation)
    }

    /// [`rig`] with `c-rest` on `symbol`, and the client built the way production builds it when `real_client`.
    fn rig_with(
        symbol: &'static str,
        real_client: bool,
        second_generation: impl Fn() -> FakeTransport + Send + Sync + 'static,
    ) -> Rig {
        let gw = FakeGateway::start();
        let rows = Rows::new(Ok(AccountTier::Demo));
        let scripted = ScriptedTrade::new(&["ibkr"]).with_link_watch("ibkr", watch(&gw, &rows));
        let lane =
            Arc::new(BridgeLane {
                inner: scripted,
                script: Box::new(move |n| {
                    if n == 1 { first_generation("c-rest") } else { second_generation() }
                }),
                made: Mutex::new(Vec::new()),
                real_client,
            });
        let hub =
            TradeHub::new(Arc::clone(&lane) as Arc<dyn TradeLane>, cfg(), Arc::new(Budget::new()));
        let first = open(&hub, "tradehub-1", None);
        assert!(matches!(start(&first.lease, 7), AdapterReply::Live { .. }));
        assert_eq!(
            first.lease.handle(TradeUp::Submit {
                cseq: 1,
                order: OrderRequest { symbol: symbol.into(), qty: 10.0, ..order("c-rest") }
            }),
            Flow::Continue
        );
        let g1 = lane.transport(1);
        assert!(wait_until(WAIT, || !g1.place_calls().lock().unwrap().is_empty()));
        Rig { gw, lane, hub, first }
    }

    /// Lose the gateway, bring it back, and open the second lease on the rebuilt session; returns it
    /// with its connection thread.
    fn rebuilt(rig: &Rig) -> (Opened, Conn) {
        rebuilt_keeping(rig, &mut Vec::new())
    }

    /// [`rebuilt`], keeping the frames the restored lease leads with (the carried events of the rebuilt bridge
    /// among them) in `kept`.
    fn rebuilt_keeping(rig: &Rig, kept: &mut Vec<TradeDown>) -> (Opened, Conn) {
        let conn = connection(Arc::clone(&rig.first.lease));
        rig.gw.close();
        assert!(wait_until(WAIT, || !adapter_up(&rig.hub)), "the loss is seen");
        assert!(wait_until(WAIT, || conn.ended()), "the lease ends at the loss");
        rig.gw.reopen();
        assert!(
            wait_until(WAIT, || {
                rig.lane.made.lock().unwrap_or_else(PoisonError::into_inner).len() == 2
                    && adapter_up(&rig.hub)
            }),
            "the adapter is rebuilt"
        );
        let second = open(
            &rig.hub,
            "tradehub-1",
            Some(WireCursor { gw_epoch: rig.first.reply.gw_epoch, seq: 1, ts_ms: 0 }),
        );
        assert!(matches!(start_keeping(&second.lease, 8, kept), AdapterReply::Live { .. }));
        let conn2 = connection(Arc::clone(&second.lease));
        (second, conn2)
    }

    /// `start`, skipping the frames a restored lease leads with (the gap, the owed reconcile, the carried
    /// events of the rebuilt bridge).
    fn start_keeping(lease: &Arc<Lease>, rid: u64, kept: &mut Vec<TradeDown>) -> AdapterReply {
        let mut s = start_args();
        s.rid = rid;
        s.route_key = "ibkr".into();
        assert_eq!(lease.handle(TradeUp::AdapterStart(s)), Flow::Continue);
        for _ in 0..10 {
            match expect_frame(lease) {
                TradeDown::AdapterReply { reply, .. } => return *reply,
                other => kept.push(other),
            }
        }
        panic!("no AdapterReply");
    }

    fn cancel(lease: &Arc<Lease>, cseq: u64, coid: &str) {
        assert_eq!(
            lease.handle(TradeUp::Cancel {
                cseq,
                coid: coid.into(),
                intent: vike_gatewayhub_client::proto::WireCancelIntent::RiskOff,
            }),
            Flow::Continue
        );
    }

    /// **The release, end to end.** Generation 2 lists the order generation 1 placed. The rebuilt bridge
    /// rebinds it, announces the connection learned, and the daemon lifts the hold: the cancel of the order
    /// reaches the transport under IB's own id, and a new order is placed.
    #[test]
    fn a_rebuilt_bridge_that_learned_the_order_releases_the_hold_and_can_cancel_it() {
        let rig = rig(|| {
            let mut fake = FakeTransport::new().with_socket_identity();
            fake.script_socket_connect(102);
            fake.on_request_open_orders(|| vec![ScriptedInbound::venue_order(101, "c-rest", 10.0)]);
            fake
        });
        let (second, conn2) = rebuilt(&rig);
        let g2 = rig.lane.transport(2);
        let cancels = g2.cancel_calls();

        // The bridge's rebinding is what the first lease sees first (the carried events).
        // The cancel works once the hold is released: try until it reaches the transport.
        let mut n = 0u64;
        assert!(
            wait_until(WAIT, || {
                n += 1;
                cancel(&second.lease, n, "c-rest");
                std::thread::sleep(Duration::from_millis(40));
                !cancels.lock().unwrap().is_empty()
            }),
            "the cancel of the learned order reaches the transport"
        );
        assert_eq!(cancels.lock().unwrap().as_slice(), &[101], "under IB's own order id");
        assert_eq!(
            second.lease.handle(TradeUp::Submit { cseq: 500, order: aapl("c-new") }),
            Flow::Continue
        );
        let places = g2.place_calls();
        assert!(wait_until(WAIT, || !places.lock().unwrap().is_empty()), "a new order is placed");
        assert_eq!(places.lock().unwrap().as_slice(), &[102], "above the learned order's id");
        drop(conn2);
    }

    /// **The fallback.** Generation 2's open-order request FAILS, so the bridge never announces: the
    /// hold stays, a cancel and a submit are refused with the words, and nothing reaches the transport.
    #[test]
    fn a_rebuilt_bridge_that_failed_to_learn_keeps_the_session_held() {
        let rig = rig(|| {
            let mut fake = FakeTransport::new().with_socket_identity();
            fake.script_socket_connect(102);
            fake.open_orders_end(SnapshotMode::Failed);
            fake
        });
        let (second, conn2) = rebuilt(&rig);
        let g2 = rig.lane.transport(2);
        std::thread::sleep(Duration::from_millis(700)); // long past any announcement
        cancel(&second.lease, 1, "c-rest");
        assert_eq!(
            second.lease.handle(TradeUp::Submit { cseq: 2, order: aapl("c-new") }),
            Flow::Continue
        );
        assert!(conn2.wait_reason("not manageable"));
        assert!(wait_until(WAIT, || {
            let frames = conn2.frames.lock().unwrap_or_else(PoisonError::into_inner).clone();
            reasons(&frames).len() >= 2
        }));
        let frames = conn2.frames.lock().unwrap_or_else(PoisonError::into_inner).clone();
        assert!(
            reasons(&frames).iter().all(|r| r.contains("not manageable")),
            "both are refused by the HOLD, not by the bridge: {:?}",
            reasons(&frames)
        );
        assert!(g2.cancel_calls().lock().unwrap().is_empty(), "no cancel was sent");
        assert!(g2.place_calls().lock().unwrap().is_empty(), "no order was sent");
    }

    /// A coid the venue no longer lists FINISHED while the connection was down. The bridge learned the
    /// venue (nothing is listed), so the hold lifts; a cancel of the unlisted coid is refused by the bridge
    /// with its own words (nothing is sent), and a new order is placed.
    #[test]
    fn an_order_the_rebuilt_bridge_does_not_find_is_finished_and_the_session_is_released() {
        let rig = rig(|| {
            let mut fake = FakeTransport::new().with_socket_identity();
            fake.script_socket_connect(102);
            fake
        });
        let (second, _conn2) = rebuilt(&rig);
        let g2 = rig.lane.transport(2);
        let places = g2.place_calls();
        let mut n = 0u64;
        assert!(wait_until(WAIT, || {
            n += 1;
            let _ = second
                .lease
                .handle(TradeUp::Submit { cseq: 100 + n, order: aapl(&format!("c-new-{n}")) });
            std::thread::sleep(Duration::from_millis(40));
            !places.lock().unwrap().is_empty()
        }));
        // The submits above were answered with the HOLD's words until it lifted (under load, several), and
        // those words may still be in flight to the connection. Frames reach it in the order the daemon stamped
        // them, so everything the hold said before the cancel precedes the bridge's answer to the cancel: only
        // what is said FROM that answer on can show the hold is back.
        cancel(&second.lease, 900, "c-rest");
        // The bridge's OWN refusal (not the hold's, which is lifted): the order is not known to this connection.
        assert!(
            _conn2.wait_reason("no venue order is known for this client order id"),
            "the bridge refuses the cancel of an unlisted coid with its own words"
        );
        let frames = _conn2.frames.lock().unwrap_or_else(PoisonError::into_inner).clone();
        let words = reasons(&frames);
        let answer = words
            .iter()
            .position(|r| r.contains("no venue order is known for this client order id"))
            .expect("the bridge's refusal was just read");
        assert!(
            !words[answer..].iter().any(|r| r.contains("not manageable")),
            "and the hold is gone: {:?}",
            &words[answer..]
        );
        assert!(
            g2.cancel_calls().lock().unwrap().is_empty(),
            "nothing to cancel: it is not listed"
        );
    }

    /// **R1, the blind decoder, end to end.** The rebuilt bridge's open-order rows are all tagged and none is
    /// attributed to this client (a decoder that left the client id empty looks exactly like that). The
    /// bridge does NOT announce it learned, so the hold STAYS.
    #[test]
    fn a_rebuilt_bridge_whose_decoder_attributes_nothing_to_us_keeps_the_session_held() {
        let rig = rig(|| {
            let mut fake = FakeTransport::new().with_socket_identity();
            fake.script_socket_connect(102);
            fake.on_request_open_orders(|| {
                vec![ScriptedInbound::VenueOrder {
                    order_id: 101,
                    order_ref: "c-rest".into(),
                    client_id: 0, // the decoder did not fill it
                    account: String::new(),
                    total_qty: 10.0,
                    status: "Submitted".into(),
                    filled_qty: 0.0,
                }]
            });
            fake
        });
        let (second, conn2) = rebuilt(&rig);
        let g2 = rig.lane.transport(2);
        std::thread::sleep(Duration::from_millis(700));
        cancel(&second.lease, 1, "c-rest");
        assert!(conn2.wait_reason("not manageable"), "held: the bridge never said it learned");
        assert!(g2.cancel_calls().lock().unwrap().is_empty());
    }

    // ---- W14f, finding 1: the symbol teach must reach the rebuilt bridge BEFORE the replay ----

    /// An engine that mounts `mounted` and holds `c-rest` (a buy of 10 on `mounted`), the way the tradehub's
    /// does, with nothing behind it: only its account is looked at.
    struct NullClient;

    impl vike_exec::ExecutionClient for NullClient {
        fn submit(&mut self, _request: &OrderRequest) {}
        fn cancel(&mut self, _client_order_id: &str) {}
    }

    /// The position the REAL engine reads after the venue session was rebuilt over `c-rest` (placed on `mounted`
    /// by generation 1, 4 of its 10 filled while the gateway was down) and the rebuilt bridge replayed the day's
    /// executions, the venue echoing `echoed` as the instrument. Everything the first lease received after the
    /// rebuild, the carried events included, is folded into the engine in order.
    ///
    /// The ORDERING is production's and nothing is paused: the lane returns the client only once the bridge has
    /// run all it can run ahead of its host (`BridgeLane::start_adapter`), and the teach, if any, is the
    /// session's own (`start_adapter_locked`).
    fn position_after_the_rebuild(mounted: &'static str, echoed: &'static str) -> f64 {
        use vike_exec::{Account, BalanceMode, EventHandler, ExecutionEngine, Outbox, RiskGate};
        use vike_model::RiskLimits;
        use vike_model::events::PositionSide;

        let rig = rig_with(mounted, true, move || {
            let mut fake = FakeTransport::new().with_socket_identity();
            fake.script_socket_connect(102);
            fake.on_request_open_orders(|| vec![ScriptedInbound::venue_order(101, "c-rest", 10.0)]);
            fake.on_request_executions(move || {
                vec![
                    ScriptedInbound::ExecutionOf {
                        order_id: 101,
                        order_ref: "c-rest".into(),
                        exec_id: "e1".into(),
                        symbol: echoed.into(),
                        shares: 4.0,
                        price: 190.0,
                        client_id: FAKE_CLIENT_ID,
                    },
                    ScriptedInbound::Commission { exec_id: "e1".into(), commission: 1.0 },
                ]
            });
            fake
        });
        let mut kept = Vec::new();
        let (_second, conn2) = rebuilt_keeping(&rig, &mut kept);
        let filled = |frames: &[TradeDown]| {
            frames.iter().any(|f| {
                matches!(f, TradeDown::Event { event: Event::OrderPartiallyFilled(p), .. }
                    if p.client_order_id == "c-rest")
            })
        };
        assert!(
            wait_until(WAIT, || {
                filled(&kept)
                    || filled(&conn2.frames.lock().unwrap_or_else(PoisonError::into_inner))
            }),
            "the rebuilt bridge's replay of the fill reaches the first lease"
        );
        let mut frames = kept;
        frames.extend(conn2.frames.lock().unwrap_or_else(PoisonError::into_inner).iter().cloned());

        let mut engine = ExecutionEngine::new(
            Account::new(1.0, "ibkr", None, BalanceMode::Delta),
            RiskGate::new(RiskLimits::new()),
            NullClient,
            "ibkr",
            mounted,
        );
        let mut outbox = Outbox::default();
        let placed = OrderRequest { symbol: mounted.into(), qty: 10.0, ..order("c-rest") };
        engine.submit_order(&placed, 1, &mut outbox);
        for frame in &frames {
            if let TradeDown::Event { event, .. } = frame {
                engine.on_event(event, &mut outbox);
            }
        }
        engine.position_size(PositionSide::Both)
    }

    /// **Finding 1, a stock.** IB echoes the execution on the exchange it ran on (`AAPL.NASDAQ.USD`), the order and
    /// the engine are `AAPL.SMART.USD`. The rebuilt bridge replays the fill BEFORE any client reteach could arrive
    /// (the tradehub's own `adopt_restored` comes seconds later); the gatewayhub's handover teaches the symbol it
    /// sent the order under first, so the fill is booked. Without it the engine dropped it in silence for good.
    #[test]
    fn a_stock_fill_the_rebuilt_bridge_replays_is_booked_under_the_symbol_the_gatewayhub_taught() {
        assert!(
            (position_after_the_rebuild("AAPL.SMART.USD", "AAPL.NASDAQ.USD") - 4.0).abs() < 1e-9
        );
    }

    /// **Finding 1, forex.** The tree writes `EUR.USD.IDEALPRO`; the exec contract reconstructs `EUR.IDEALPRO.USD`.
    #[test]
    fn a_forex_fill_the_rebuilt_bridge_replays_is_booked_under_the_symbol_the_gatewayhub_taught() {
        assert!(
            (position_after_the_rebuild("EUR.USD.IDEALPRO", "EUR.IDEALPRO.USD") - 4.0).abs() < 1e-9
        );
    }

    /// The control: an echo equal to the order's symbol is booked with or without a teach.
    #[test]
    fn a_fill_replayed_under_the_same_symbol_is_booked_too() {
        assert!(
            (position_after_the_rebuild("AAPL.SMART.USD", "AAPL.SMART.USD") - 4.0).abs() < 1e-9
        );
    }

    // ---- W14f, finding 2: a bridge that never learns the venue is a state, not a journal line ----

    /// A first start whose bridge cannot learn (the open-order request fails every time): the adapter looks up,
    /// but within the learning budget the session is HELD with words that say what failed and what to do, and
    /// every order verb is answered with them. (It used to be a session that looked Live with an `error!` in the
    /// journal and nothing else.)
    #[test]
    fn a_bridge_that_never_learns_the_venue_holds_its_session_with_words() {
        let gw = FakeGateway::start();
        let rows = Rows::new(Ok(AccountTier::Demo));
        let scripted = ScriptedTrade::new(&["ibkr"]).with_link_watch("ibkr", watch(&gw, &rows));
        let lane = Arc::new(BridgeLane {
            inner: scripted,
            script: Box::new(|_| {
                let mut fake = FakeTransport::new().with_socket_identity();
                fake.script_socket_connect(101);
                fake.open_orders_end(SnapshotMode::Failed);
                fake
            }),
            made: Mutex::new(Vec::new()),
            real_client: false,
        });
        let hub =
            TradeHub::new(Arc::clone(&lane) as Arc<dyn TradeLane>, cfg(), Arc::new(Budget::new()));
        hub.set_learning_budget(Duration::from_millis(400));
        let first = open(&hub, "tradehub-1", None);
        assert!(matches!(start(&first.lease, 7), AdapterReply::Live { .. }));
        let conn = connection(Arc::clone(&first.lease));
        // The BARE actor learns only when it folds the connect seed's `Connected` (a few loop turns after its
        // thread starts, longer under load), and until then a submit is placed. Production never submits
        // that early: the real client is handed over only after the bridge parked its replay or ran it
        // (`BridgeLane::start_adapter`, `real_client`). So the test waits for the learning to have BEGUN, which
        // is the moment the open orders are asked for.
        assert!(
            wait_until(WAIT, || {
                lane.made.lock().unwrap_or_else(PoisonError::into_inner).len() == 1
                    && *lane.transport(1).open_order_requests().lock().unwrap() > 0
            }),
            "the bridge began learning the venue (asked for the open orders)"
        );
        // The handover released the bridge, whose first learning fails: a bridge that cannot learn refuses a
        // submit with its own words, then, past the budget, the session does with the hold's.
        let mut n = 0u64;
        assert!(
            wait_until(WAIT, || {
                n += 1;
                let _ = first
                    .lease
                    .handle(TradeUp::Submit { cseq: n, order: aapl(&format!("c-try-{n}")) });
                std::thread::sleep(Duration::from_millis(50));
                let frames = conn.frames.lock().unwrap_or_else(PoisonError::into_inner).clone();
                reasons(&frames).iter().any(|r| r.contains("has not reported that it learned"))
            }),
            "the session is held with the learning watch's words"
        );
        let frames = conn.frames.lock().unwrap_or_else(PoisonError::into_inner).clone();
        let words = reasons(&frames);
        let held = words.iter().find(|r| r.contains("has not reported that it learned")).unwrap();
        assert!(held.contains("restart the gatewayhub") && held.contains("NOT sent"), "{held}");
        assert!(!held.contains('\n') && !held.contains("  "), "one plain line: {held:?}");
        assert!(
            words.iter().any(|r| r.contains("has not learned the orders this client already has")),
            "and before the budget the bridge refused with its own words: {words:?}"
        );
        assert!(lane.transport(1).place_calls().lock().unwrap().is_empty(), "nothing was placed");
    }
}

// ---- review 18, R1: the hold lifts only over an ACCOUNTED ledger (demo: or with the rest named) ---------

/// Everything `up()` + two orders + a rebuild needs, at `tier`: the first lease with `c-rest` and `c-gone`
/// sent (neither finished), the gateway lost and back, the second lease open with its leading frames read,
/// and the rebuilt adapter. The tier of the account, the rows and the lease are all `tier`.
struct Rebuilt {
    // Kept ALIVE for the test: dropping the hub shuts the session down, dropping the gateway closes its port.
    _gw: Arc<FakeGateway>,
    _scripted: ScriptedTrade,
    _hub: Arc<TradeHub>,
    second: Opened,
    a2: vike_gatewayhub::testkit::trade::ScriptedAdapter,
    conn2: Conn,
}

fn rebuilt_at(tier: AccountTier) -> Rebuilt {
    let gw = FakeGateway::start();
    let rows = Rows::new(Ok(tier));
    let scripted = ScriptedTrade::new(&["ibkr"])
        .with_tier("ibkr", None, Ok(tier))
        .with_link_watch("ibkr", watch(&gw, &rows));
    let lane = Arc::new(Flaky {
        inner: scripted.clone(),
        starts: Mutex::new(Vec::new()),
        refuse: 0..=0,
        slow: Duration::ZERO,
        push: None,
        announce_learned: false,
    });
    let hub =
        TradeHub::new(Arc::clone(&lane) as Arc<dyn TradeLane>, cfg(), Arc::new(Budget::new()));
    let first =
        hub.open(request_at("tradehub-1", tier, None)).map_err(|e| format!("{e:?}")).unwrap();
    assert!(matches!(start(&first.lease, 7), AdapterReply::Live { .. }));
    let a1 = scripted.adapter("ibkr", None).expect("the first adapter");
    for (cseq, coid) in [(1, "c-rest"), (2, "c-gone")] {
        assert_eq!(
            first.lease.handle(TradeUp::Submit { cseq, order: order(coid) }),
            Flow::Continue
        );
    }
    assert!(wait_until(WAIT, || a1.submitted().len() == 2));
    let conn = connection(Arc::clone(&first.lease));
    gw.close();
    assert!(wait_until(WAIT, || !adapter_up(&hub)), "the loss is seen");
    assert!(wait_until(WAIT, || conn.ended()), "the lease ends at the loss");
    gw.reopen();
    assert!(
        wait_until(REBUILD_WAIT, || {
            lane.starts.lock().unwrap_or_else(PoisonError::into_inner).len() == 2
                && adapter_up(&hub)
        }),
        "the adapter is rebuilt"
    );
    let second = hub
        .open(request_at(
            "tradehub-1",
            tier,
            Some(WireCursor { gw_epoch: first.reply.gw_epoch, seq: 1, ts_ms: 0 }),
        ))
        .map_err(|e| format!("{e:?}"))
        .unwrap();
    for _ in 0..2 {
        assert!(matches!(
            expect_frame(&second.lease),
            TradeDown::ReplayUnavailable { .. } | TradeDown::ReconcileNow { .. }
        ));
    }
    assert!(matches!(start(&second.lease, 8), AdapterReply::Live { .. }));
    let a2 = scripted.adapter("ibkr", None).expect("the rebuilt adapter");
    let conn2 = connection(Arc::clone(&second.lease));
    Rebuilt { _gw: gw, _scripted: scripted, _hub: hub, second, a2, conn2 }
}

/// An `OrderAccepted` that names the venue's order, the shape a bridge states for an order it holds.
fn named_accepted(coid: &str) -> Event {
    Event::OrderAccepted(OrderAccepted {
        client_order_id: coid.to_string(),
        venue_order_id: Some(format!("v-{coid}").into()),
        ts: 5,
    })
}

/// `true` once the rebuilt adapter RECEIVES an order (the session stopped holding).
fn takes_orders(r: &Rebuilt) -> bool {
    released(&r.second.lease, &r.a2)
}

/// **R1, demo.** The bridge announced it learned the venue, but it accepted only `c-rest`: `c-gone` is in the
/// daemon's ledger and the bridge never mentioned it. On the DEMO tier the hold lifts (the order finished
/// during the outage, or the bridge could not see it) and the unaccounted coid is named and forgotten.
#[test]
fn on_demo_the_hold_lifts_over_an_order_the_bridge_did_not_find_and_forgets_it() {
    let r = rebuilt_at(AccountTier::Demo);
    assert!(r.a2.emit(trade_rig::accepted("c-rest")), "the bridge rebound c-rest");
    assert!(r.a2.emit_exec_link(true), "and says it learned the venue");
    assert!(takes_orders(&r), "demo: lifted, with c-gone named at error");
}

/// **R1, live.** The same, on the LIVE tier: an unaccounted order keeps the session held, and the words
/// name it. A live session rebuilds unattended; the hold is its only barrier.
#[test]
fn on_live_the_hold_stays_over_an_order_the_bridge_did_not_find() {
    let r = rebuilt_at(AccountTier::Live);
    assert!(r.a2.emit(trade_rig::accepted("c-rest")));
    assert!(r.a2.emit_exec_link(true));
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(
        r.second.lease.handle(TradeUp::Submit { cseq: 50, order: order("c-new") }),
        Flow::Continue
    );
    assert!(r.conn2.wait_reason("not manageable"), "still held");
    let frames = r.conn2.frames.lock().unwrap_or_else(PoisonError::into_inner).clone();
    let words = reasons(&frames);
    assert!(
        words.iter().any(|w| w.contains("c-gone") && !w.contains("c-rest")),
        "the words name the unaccounted order, not the accounted one: {words:?}"
    );
    assert!(r.a2.submitted().is_empty(), "nothing reached the adapter");
}

/// Every ledger order accounted (the bridge rebound both): lifted on live too, nothing named.
#[test]
fn on_live_the_hold_lifts_when_the_bridge_accounted_for_every_order() {
    let r = rebuilt_at(AccountTier::Live);
    assert!(r.a2.emit(trade_rig::accepted("c-rest")));
    assert!(r.a2.emit(trade_rig::accepted("c-gone")));
    assert!(r.a2.emit_exec_link(true));
    assert!(takes_orders(&r));
}

/// An order that reached a terminal state after the rebuild is accounted too (the bridge knew it).
#[test]
fn a_terminal_event_after_the_rebuild_accounts_for_its_order() {
    let r = rebuilt_at(AccountTier::Live);
    assert!(r.a2.emit(trade_rig::accepted("c-rest")));
    assert!(r.a2.emit(cancelled("c-gone")));
    assert!(r.a2.emit_exec_link(true));
    assert!(takes_orders(&r));
}
