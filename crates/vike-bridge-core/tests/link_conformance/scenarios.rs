//! The scenarios, each over one covered row and answering `Ok` or why it failed, and the rig that runs a row's REAL
//! pieces through the shared pump (`run_user`, with `run_user_resync` on a history venue) on a virtual clock with the
//! row's REAL durations: its reconnect wait, idle threshold, keepalive cadence and history lag, never retyped.

use std::collections::{BTreeSet, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use serde_json::Value;
use tracing::Level;
use vike_binance::family::execution::user_data::WebsocketSessionUserStreamKeyed;
use vike_bridge_core::exec_actor::{ExecutionClientActorCommand, FillSentinel, run_loop};
use vike_bridge_core::pump_clock::VirtualClock;
use vike_bridge_core::rest::{OrderCancelAck, VenueRest};
use vike_bridge_core::scripted::{
    UserScriptHeard, UserScriptLog, UserScriptStep, WebsocketSessionUserStreamScript,
};
use vike_bridge_core::scripted_rest::LaggedHistory;
use vike_bridge_core::transport::VenueApiError;
use vike_bridge_core::venue_link::{VenueLinkState, venue_link};
use vike_bridge_core::websocket::WebsocketSessionUserStreamTungstenite;
use vike_bridge_core::websocket_session::spec::CONNECT_10S;
use vike_bridge_core::websocket_session::user::reconnect::RECONNECT_LOG;
use vike_bridge_core::websocket_session::user::stream_link::{report_on_this_thread, stream_link};
use vike_bridge_core::websocket_session::user::{
    RESYNC_SETTLE, USER_DATA_POLL, WebsocketSessionUserError, WebsocketSessionUserKeepalive,
    WebsocketSessionUserMessage, WebsocketSessionUserOpenOutcome, run_user, run_user_resync,
    run_user_with_notice,
};
use vike_execution::testing::RecordingClient;
use vike_execution::{
    Account, BalanceMode, Engine, EventBusOutbox, EventHandler, RiskGate, event_channel,
};
use vike_log::capture::{CapturedEvent, captured};
use vike_log::loopback::serve_replies;
use vike_model::events::{Event, FillEvent};
use vike_model::feed_status::DataClientConnectionState;
use vike_model::{OrderRequest, OrderType, RiskLimits};
use vike_venues::reconnect::ReconnectBackoff;
use vike_venues::venue_connection::{
    VenueConnectionAnnouncedClose, VenueConnectionPathReport, VenueConnectionRecovery,
};
use vike_venues::venue_timings::VenueTimingsLane;

use super::fake_private_websocket::FakePrivateWebsocket;
use super::sessions::Proof;

/// The ladder the row's exec lane declares (every pump-driven one runs 1 s → 30 s); `run_user` takes it whole, unspread
/// (the cells pin the schedule).
fn pump_ladder(row: &Row) -> Result<ReconnectBackoff, String> {
    let reconnect = timings(row)?.reconnect.ok_or("the row declares no reconnect")?;
    Ok(ReconnectBackoff::from_row(&reconnect).without_jitter())
}
use super::venues::{CursorRead, Expiry, Fill, Keepalive, LinkLane, LinkVenue, Recovery, Sentinel};
use super::{Row, Scenario};

/// A check that answers the cell's failure instead of unwinding, so every row's outcome is reported.
macro_rules! check {
    ($cond:expr, $($arg:tt)*) => {
        if $cond {
        } else {
            return Err(format!($($arg)*));
        }
    };
}

/// Whether a scenario runs on a covered row.
pub(super) enum Cell {
    Runs,
    /// The row's shape has nothing for it to test, with why.
    NotApplicable(String),
    /// It applies and is not covered, with the shape that does not fit.
    Deferred(String),
    /// It applies where no scripted stream reaches, and the venue's own suite proves it on its seam: the tests `by`
    /// (`link_conformance.rs`'s `every_cell_proven_on_its_seam_names_a_test_that_exists` finds each), and what they pin.
    Proven {
        by: &'static [Proof],
        pins: &'static str,
    },
}

/// What `scenario` is on `row`: the impl's own answer first (`LinkVenue::cell`, the one way an impl defers a cell), then
/// the row's recovery kind decides S7, its announced close S3 and its path report S2 and S5, the impl's answers decide
/// S4, S8 and S11, and S12 runs.
pub(super) fn cell(venue: &dyn LinkVenue, row: &Row, scenario: Scenario) -> Cell {
    if let Some(cell) = venue.cell(scenario) {
        return cell;
    }
    let exec = row.connection().exec;
    let recovery = exec.recovery;
    match scenario {
        Scenario::AnnouncedClose => match row.connection().exec.announced_close {
            // An impl's hook on a row that does not say `Handled` runs, so the scenario names the stale claim.
            _ if venue.notice().is_some() => Cell::Runs,
            VenueConnectionAnnouncedClose::Handled { .. } => Cell::Runs,
            VenueConnectionAnnouncedClose::Ignored { notice } => Cell::Deferred(format!(
                "the venue announces `{notice}` and the bridge parses nothing: a declared gap"
            )),
            VenueConnectionAnnouncedClose::NoneKnown => {
                Cell::NotApplicable("the row knows no notice the venue sends before a close".into())
            }
        },
        Scenario::LaggingHistory if recovery != VenueConnectionRecovery::HistoryWindow => {
            Cell::NotApplicable(format!("its recovery is {recovery:?}: no history read to lag"))
        }
        Scenario::ServerClose | Scenario::Stall
            if exec.path_report != VenueConnectionPathReport::SharedPump =>
        {
            Cell::Deferred(format!(
                "it reads the order path's state off the pump's stream link, and the row states its path on a cell of \
                 its own (`path_report` {:?})",
                exec.path_report
            ))
        }
        Scenario::Expiry => match venue.expiry() {
            Expiry::NotApplicable(why) => Cell::NotApplicable(why.to_string()),
            _ => Cell::Runs,
        },
        Scenario::BusyCommands => match venue.sentinel() {
            Sentinel::OwnLoop(why) => Cell::NotApplicable(why.to_string()),
            Sentinel::RunLoop(_) => Cell::Runs,
        },
        Scenario::Keepalive => match venue.keepalive() {
            Keepalive::Deferred(why) => Cell::Deferred(why.to_string()),
            Keepalive::OnItsSeam { by, pins } => Cell::Proven { by, pins },
            _ => Cell::Runs,
        },
        Scenario::ReconcileRace => match (recovery, row.connection().exec.nudges_reconcile) {
            (_, false) => Cell::NotApplicable("the row pokes no reconcile on a reopen".into()),
            (VenueConnectionRecovery::SnapshotOnSubscribe, true) => Cell::Runs,
            (_, true) => Cell::NotApplicable(
                "its nudge is the replay supervisor's, after the schedule's last replay \
                 (`crates/vike-bridge-core/src/websocket_session/user/resync_supervisor_tests.rs`'s \
                 `a_history_lag_replays_twice_then_nudges_reconcile_after_the_last`)"
                    .into(),
            ),
        },
        _ => Cell::Runs,
    }
}

/// Run `scenario` on `row`.
pub(super) fn run(venue: &dyn LinkVenue, row: &Row, scenario: Scenario) -> Result<(), String> {
    match scenario {
        Scenario::Drop => drop_mid_session(venue, row),
        Scenario::ServerClose => server_close(venue, row),
        Scenario::AnnouncedClose => announced_close(venue, row),
        Scenario::Expiry => expiry(venue, row),
        Scenario::Stall => silent_stall(venue, row),
        Scenario::GapFill => gap_fill(venue, row),
        Scenario::LaggingHistory => lagging_history(venue, row),
        Scenario::BusyCommands => busy_commands(venue, row),
        Scenario::ReconcileRace => reconcile_race(venue, row),
        Scenario::DoubleDelivery => double_delivery(venue, row),
        Scenario::Keepalive => keepalive_cadence(venue, row),
        Scenario::DialBound => dial_bound(venue, row),
    }
}

// ===================================================================================================
// The rig.
// ===================================================================================================

/// One scripted session.
struct Session {
    steps: Vec<UserScriptStep>,
    /// The venue answers every frame the pump sends (its pong to our ping).
    answers_sends: bool,
}

impl Session {
    /// A session whose venue answers nothing the pump sends.
    fn quiet(steps: Vec<UserScriptStep>) -> Self {
        Self { steps, answers_sends: false }
    }
}

/// What a run is scripted to meet: the sessions, in order (the pump stops at the end of the last), and each fill the
/// venue's history (or its subscribe snapshot) lists, from its virtual time on.
struct Script {
    sessions: Vec<Session>,
    listed: Vec<(Duration, Fill)>,
}

/// Which producer delivered an event.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Source {
    /// The pump's own stream (a snapshot venue's subscribe snapshot included).
    Stream,
    /// The reconnect replay, or a cursor venue's read at the open.
    Replay,
}

struct Seen {
    at: Duration,
    source: Source,
    event: Event,
}

/// What the pump and its replay did, each at its virtual time since the clock's birth.
#[derive(Default)]
struct Run {
    seen: Vec<Seen>,
    /// Every call of the open.
    opens: Vec<Duration>,
    /// The order path's state at every open and every decoded frame: `(when, state, why it is down)`.
    states: Vec<(Duration, DataClientConnectionState, String)>,
    reconnects: Vec<Duration>,
    /// Every read of the venue's history or subscribe snapshot.
    reads: Vec<Duration>,
    nudges: usize,
    /// Each nudge the venue's own pump hooks sent (a snapshot venue's): how many frames were decoded by then.
    nudged_after: Vec<usize>,
    heard: Vec<(Duration, UserScriptHeard)>,
    /// The pump thread's log lines.
    lines: Vec<CapturedEvent>,
    decoded: Vec<Duration>,
    /// The pump's auth error, if it gave up.
    gave_up: Option<String>,
}

fn lock<T>(cell: &Mutex<T>) -> MutexGuard<'_, T> {
    cell.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A route key no other run shares: the cell its pump reports to.
fn route_key(row: &Row) -> String {
    static RUNS: AtomicUsize = AtomicUsize::new(0);
    format!("link-conformance/{}/{}", row.label(), RUNS.fetch_add(1, Ordering::Relaxed))
}

/// The order path's state of the account `key` now, and why it is down.
fn link_state(key: &str) -> (DataClientConnectionState, String) {
    let state = venue_link(key).state();
    let why = match &state {
        VenueLinkState::Down(down) => down.why.clone(),
        _ => String::new(),
    };
    (state.connection_state(), why)
}

/// The keepalive the venue's pump runs, at the row's cadence, recording each frame on the scripted stream.
fn keepalive_hook(
    venue: &dyn LinkVenue,
    exec: &VenueTimingsLane,
) -> Result<Option<WebsocketSessionUserKeepalive<WebsocketSessionUserStreamScript>>, String> {
    match (venue.keepalive(), exec.keepalive_every) {
        (Keepalive::Sends(payload), Some(every)) => {
            let mut sent = 0u64;
            let hook = move |stream: &mut WebsocketSessionUserStreamScript| {
                sent += 1;
                let _ = stream.send_text(&payload(sent));
            };
            Ok(Some((every.value, Box::new(hook))))
        }
        (Keepalive::Sends(_), None) => {
            Err("the impl sends a keepalive frame and the row declares no `keepalive_every`".into())
        }
        (Keepalive::ServerPings, Some(every)) => Err(format!(
            "the row declares a {:?} `keepalive_every` and the impl says the server pings",
            every.value
        )),
        (Keepalive::ServerPings | Keepalive::Deferred(_) | Keepalive::OnItsSeam { .. }, _) => {
            Ok(None)
        }
    }
}

/// Run `venue`'s REAL decoder and notice hook (and its replay, on a history venue) through `run_user_with_notice`
/// (and `run_user_resync`) over the scripted sessions, as `vike_bridge_core::websocket_session::user`'s
/// `spawn_user_with_resync` (`spawn_user` for a snapshot venue) runs them: its poll, ceiling, settle and failed-open pace, the row's idle threshold and history lag,
/// the venue's keepalive at the row's cadence, the reconcile nudge where the row says, on threads that share one
/// virtual clock.
fn rig(
    venue: &dyn LinkVenue,
    row: &Row,
    script: impl FnOnce(&LinkLane) -> Script,
) -> Result<Run, String> {
    let exec = timings(row)?;
    let connection = row.connection().exec;
    let lane = venue.lane();
    let script = script(&lane);
    let LinkLane { decode: venue_decode, recovery, .. } = lane;
    let notice = venue.notice().map(|notice| notice.hook);
    // What an open brings back before its first frame: a snapshot venue's subscribe snapshot, a cursor venue's read
    // past its cursor or the venue's re-send from the cursor the open asked from.
    let (replay, snapshot, cursor, resend) = match (connection.recovery, recovery) {
        (VenueConnectionRecovery::HistoryWindow, Recovery::History(map)) => {
            (Some(map), None, None, None)
        }
        (VenueConnectionRecovery::SnapshotOnSubscribe, Recovery::Snapshot { frame, nudge }) => {
            (None, Some((frame, nudge)), None, None)
        }
        (VenueConnectionRecovery::Cursor, Recovery::Cursor(CursorRead::Read(read))) => {
            (None, None, Some(read), None)
        }
        (VenueConnectionRecovery::Cursor, Recovery::Cursor(CursorRead::Resend { resend, .. })) => {
            (None, None, None, Some(resend))
        }
        (kind, _) => {
            return Err(format!(
                "the row recovers a gap by {kind:?} and the impl hands over another kind of recovery"
            ));
        }
    };
    let keepalive = keepalive_hook(venue, &exec)?;
    let clock = VirtualClock::new(if replay.is_some() { 2 } else { 1 });
    let history = LaggedHistory::new();
    let born = clock.now();
    for (from, fill) in script.listed {
        history.lists_from(born + from, fill);
    }
    let rec = Arc::new(Mutex::new(Run::default()));
    let stop = Arc::new(AtomicBool::new(false));
    let generation = Arc::new(AtomicU64::new(0));
    let watched = Arc::downgrade(&generation);
    let (nudge, nudged) = mpsc::channel::<()>();
    let key = route_key(row);
    let idle = exec.idle_threshold.map(|idle| idle.value);
    // The venue's spec hands `run_user` its row's ladder (1 s -> 30 s on every pump-driven row, a fixed 2 s on alpaca's),
    // with the modes its spec adds (`LinkVenue::ladder`). Unspread here: the cells pin the row's schedule on the virtual
    // clock, and the spread is `vike_venues::reconnect`'s own test.
    let ladder =
        venue.ladder(&exec.reconnect.ok_or("the row declares no reconnect")?).without_jitter();
    let spawn_ms = vike_model::now_ms();

    let pump = {
        let (clock, rec, stop, history) =
            (Arc::clone(&clock), Arc::clone(&rec), Arc::clone(&stop), history.clone());
        // A snapshot venue's pump nudges reconcile by the venue's own hooks, handed the trigger where the row says (each
        // nudge timed here, on the pump thread); a history venue's replay nudges after it.
        let (trigger, venue_nudged) = mpsc::channel::<()>();
        let trigger = connection.nudges_reconcile.then_some(trigger);
        let mut sessions: VecDeque<Session> = script.sessions.into();
        std::thread::Builder::new()
            .name(format!("link-pump {}", row.label()))
            .spawn(move || {
                let _seat = clock.enter();
                report_on_this_thread(Some(stream_link(&key)));
                let at = || clock.offset(clock.now());
                let sample = || {
                    let (when, (state, why)) = (at(), link_state(&key));
                    lock(&rec).states.push((when, state, why));
                };
                let (mut reopened, frame_seen) =
                    snapshot.as_ref().map(|(_, nudge)| nudge(trigger)).unzip();
                let heard = || {
                    for () in venue_nudged.try_iter() {
                        let mut rec = lock(&rec);
                        let frames = rec.decoded.len();
                        rec.nudged_after.push(frames);
                    }
                };
                let log = UserScriptLog::default();
                let open = || {
                    let when = at();
                    lock(&rec).opens.push(when);
                    sample();
                    let Some(session) = sessions.pop_front() else {
                        return WebsocketSessionUserOpenOutcome::Stopped;
                    };
                    let mut steps = session.steps;
                    // A cursor venue's open reads past its cursor before the first frame, and what it reads goes out first.
                    if let Some(cursor) = &cursor {
                        let read = cursor(&history.read());
                        lock(&rec).seen.extend(read.into_iter().map(|event| Seen {
                            at: when,
                            source: Source::Replay,
                            event,
                        }));
                    }
                    if let Some((snapshot, _)) = &snapshot {
                        let frame = snapshot(&history.read());
                        steps.insert(
                            0,
                            UserScriptStep::Read(Ok(WebsocketSessionUserMessage::Text(frame))),
                        );
                    }
                    if let Some(resend) = &resend {
                        let sent = resend(&history.read()).into_iter().map(text);
                        steps.splice(0..0, sent);
                    }
                    let mut stream = WebsocketSessionUserStreamScript::from_steps(steps)
                        .logged(Arc::clone(&log));
                    if session.answers_sends {
                        stream = stream.answering_sends();
                    }
                    if sessions.is_empty() {
                        stream = stream.stopping(Arc::clone(&stop));
                    }
                    WebsocketSessionUserOpenOutcome::Ready(stream)
                };
                let decode = |frame: &Value| {
                    let events = venue_decode(frame);
                    if let Some(seen) = &frame_seen {
                        seen(frame);
                    }
                    let when = at();
                    lock(&rec).decoded.push(when);
                    heard();
                    sample();
                    events
                };
                let emit = |event: Event| {
                    let when = at();
                    lock(&rec).seen.push(Seen { at: when, source: Source::Stream, event });
                    true
                };
                // It owns the strong half of the generation, so the replay ends once the pump does.
                let on_reconnect = {
                    let (rec, clock) = (Arc::clone(&rec), Arc::clone(&clock));
                    move || {
                        generation.fetch_add(1, Ordering::Relaxed);
                        let when = clock.offset(clock.now());
                        lock(&rec).reconnects.push(when);
                        if let Some(reopened) = reopened.as_mut() {
                            reopened();
                        }
                        heard();
                    }
                };
                let (ended, lines) = captured(|| {
                    run_user_with_notice(
                        open,
                        decode,
                        notice,
                        emit,
                        &stop,
                        USER_DATA_POLL,
                        ladder,
                        keepalive,
                        on_reconnect,
                        idle,
                        Some(RECONNECT_LOG),
                    )
                });
                let heard: Vec<_> =
                    lock(&log).iter().map(|(t, h)| (clock.offset(*t), h.clone())).collect();
                let mut rec = lock(&rec);
                rec.lines = lines;
                rec.gave_up = ended.err().map(|e| e.0);
                rec.heard = heard;
            })
            .map_err(|e| format!("spawn the pump: {e}"))?
    };
    let replay = match replay {
        None => None,
        Some(map) => {
            let (clock, rec, stop, history) =
                (Arc::clone(&clock), Arc::clone(&rec), Arc::clone(&stop), history.clone());
            let lag = exec.history_lag.map(|lag| lag.value);
            let on_reconcile = connection.nudges_reconcile.then(|| nudge.clone());
            let thread = std::thread::Builder::new()
                .name(format!("link-replay {}", row.label()))
                .spawn(move || {
                    let _seat = clock.enter();
                    run_user_resync(
                        watched,
                        0,
                        spawn_ms,
                        &stop,
                        USER_DATA_POLL,
                        RESYNC_SETTLE,
                        lag,
                        || true,
                        || map(&history.read()),
                        |event| {
                            let when = clock.offset(clock.now());
                            lock(&rec).seen.push(Seen { at: when, source: Source::Replay, event });
                            true
                        },
                        on_reconcile,
                    );
                })
                .map_err(|e| format!("spawn the replay: {e}"))?;
            Some(thread)
        }
    };
    drop(nudge);
    pump.join().map_err(|_| "the pump thread panicked".to_string())?;
    if let Some(replay) = replay {
        replay.join().map_err(|_| "the replay thread panicked".to_string())?;
    }
    let mut run = std::mem::take(&mut *lock(&rec));
    run.reads = history.reads().into_iter().map(|at| clock.offset(at)).collect();
    run.nudges = nudged.try_iter().count() + run.nudged_after.len();
    Ok(run)
}

// ===================================================================================================
// Shared pieces of the scenarios.
// ===================================================================================================

/// When a scripted session drops, in the scenarios that drop one.
const DROP_AT: Duration = Duration::from_secs(10);

/// A scenario fill. Stamped an hour ahead of the clock, so no history floor (`is_pre_spawn`, hyperliquid's first
/// snapshot) takes it for one older than the process.
fn fill(n: u32) -> Fill {
    Fill { n, ts: vike_model::now_ms() + 3_600_000 }
}

fn text(frame: String) -> UserScriptStep {
    UserScriptStep::Read(Ok(WebsocketSessionUserMessage::Text(frame)))
}

/// The socket fails under the session.
fn reset() -> UserScriptStep {
    UserScriptStep::Read(Err(WebsocketSessionUserError::Closed("connection reset by peer".into())))
}

fn timings(row: &Row) -> Result<VenueTimingsLane, String> {
    row.timings().ok_or_else(|| "the row declares no exec timings".to_string())
}

/// The row's first reconnect wait.
fn first_wait(exec: &VenueTimingsLane) -> Result<Duration, String> {
    exec.reconnect
        .map(|reconnect| reconnect.initial)
        .ok_or_else(|| "the row declares no reconnect".into())
}

fn history_lag(exec: &VenueTimingsLane) -> Option<Duration> {
    exec.history_lag.map(|lag| lag.value)
}

/// How long a last session stays up after a re-open: the whole replay schedule and some.
fn tail(exec: &VenueTimingsLane) -> Duration {
    RESYNC_SETTLE + history_lag(exec).unwrap_or_default() + 3 * USER_DATA_POLL
}

/// The two sessions of a drop: the first with `before` and then a reset at [`DROP_AT`], the second up for [`tail`].
fn dropped_once(exec: &VenueTimingsLane, before: Vec<UserScriptStep>) -> Vec<Session> {
    let mut first = before;
    first.extend([UserScriptStep::Stall(DROP_AT), reset()]);
    vec![Session::quiet(first), Session::quiet(vec![UserScriptStep::Stall(tail(exec))])]
}

fn fills(run: &Run) -> Vec<(&Seen, &FillEvent)> {
    run.seen
        .iter()
        .filter_map(|seen| match &seen.event {
            Event::Fill(fill) => Some((seen, fill)),
            _ => None,
        })
        .collect()
}

/// The deduped state transitions.
fn transitions(run: &Run) -> Vec<(DataClientConnectionState, String)> {
    let mut out: Vec<(DataClientConnectionState, String)> = Vec::new();
    for (_, state, why) in &run.states {
        if out.last() != Some(&(*state, why.clone())) {
            out.push((*state, why.clone()));
        }
    }
    out
}

/// The pump's own log lines (the venue decoder's are its own business).
fn pump_lines(run: &Run) -> impl Iterator<Item = &CapturedEvent> {
    run.lines.iter().filter(|line| line.target.starts_with("vike_bridge_core"))
}

/// One fill was scripted: every delivery of it names one trade id and one symbol, and they fold, through a REAL
/// `Engine` started on that symbol with the order registered first (a spot fill of an order the engine never held is a
/// hand trade, not ours), to the position of ONE fill.
fn folded_once(venue: &str, run: &Run) -> Result<(), String> {
    let delivered = fills(run);
    let Some((_, first)) = delivered.first() else {
        return Err("no fill was delivered at all".into());
    };
    let ids: BTreeSet<&str> = delivered.iter().map(|(_, f)| f.trade_id.as_str()).collect();
    check!(ids.len() == 1, "one fill was scripted and the run delivered trade ids {ids:?}");
    let symbols: BTreeSet<&str> = delivered.iter().map(|(_, f)| f.symbol.as_str()).collect();
    check!(symbols.len() == 1, "the deliveries of one fill name the symbols {symbols:?}");
    let symbol = first.symbol.as_str().to_string();
    let mut engine = Engine::new(
        Account::new(1.0, venue, None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        RecordingClient::default(),
        venue,
        &symbol,
    );
    let mut outbox = EventBusOutbox::default();
    let order = OrderRequest {
        order_client_id: first.order_client_id.clone(),
        venue: venue.into(),
        symbol: symbol.clone(),
        side: first.side,
        qty: first.last_qty,
        order_type: OrderType::Limit,
        price: Some(first.last_price),
        timestamp: 1,
        ..Default::default()
    };
    engine.submit_order(&order, 1, &mut outbox);
    check!(
        engine.registry.contains_key(&first.order_client_id),
        "the RiskGate refused the scenario order {:?}",
        first.order_client_id
    );
    for seen in &run.seen {
        engine.on_event(&seen.event, &mut outbox);
    }
    let held: f64 = engine
        .local_view()
        .positions
        .iter()
        .filter(|((held, _), _)| *held == symbol)
        .map(|(_, qty)| *qty)
        .sum();
    let one = f64::from(first.side) * first.last_qty;
    check!(
        (held - one).abs() < 1e-9,
        "{} deliveries of trade {} folded to a position of {held}; one fill is {one}",
        delivered.len(),
        first.trade_id.as_str()
    );
    Ok(())
}

// ===================================================================================================
// The scenarios.
// ===================================================================================================

/// S1: a fill on the stream, then the socket fails. The re-open comes the row's first wait after the drop, `on_reconnect`
/// fires once at it, the replay schedule is the row's, reconcile is nudged as the row says, and the fill folds once.
fn drop_mid_session(venue: &dyn LinkVenue, row: &Row) -> Result<(), String> {
    let exec = timings(row)?;
    let first_wait = first_wait(&exec)?;
    let f1 = fill(1);
    let run = rig(venue, row, |lane| Script {
        sessions: dropped_once(&exec, vec![text((lane.fill_frame)(f1))]),
        listed: vec![(USER_DATA_POLL, f1)],
    })?;
    check!(run.opens.len() == 2, "{} opens for one drop: {:?}", run.opens.len(), run.opens);
    let reopen = run.opens[1];
    check!(
        reopen == DROP_AT + first_wait,
        "re-opened at {reopen:?}: the drop came at {DROP_AT:?} and the row's first wait is {first_wait:?}"
    );
    check!(
        run.reconnects == [reopen],
        "on_reconnect fired at {:?}, not once at the re-open",
        run.reconnects
    );
    match row.connection().exec.recovery {
        VenueConnectionRecovery::HistoryWindow => {
            let lag = history_lag(&exec);
            let schedule = 1 + usize::from(lag.is_some());
            check!(
                run.reads.len() == schedule,
                "{:?} replays, the row's schedule is {schedule}",
                run.reads
            );
            let first = run.reads[0];
            check!(
                first >= reopen + RESYNC_SETTLE && first <= reopen + RESYNC_SETTLE + USER_DATA_POLL,
                "the first replay came at {first:?}, the settle after the re-open at {reopen:?}"
            );
            if let Some(lag) = lag {
                check!(
                    run.reads[1] == first + lag,
                    "the second replay came at {:?}, {lag:?} after the first",
                    run.reads[1]
                );
            }
        }
        _ => check!(
            run.reads.len() == 2,
            "one subscribe snapshot or cursor read per open, read at {:?}",
            run.reads
        ),
    }
    let nudges = usize::from(row.connection().exec.nudges_reconcile);
    check!(run.nudges == nudges, "reconcile nudged {} times; the row says {nudges}", run.nudges);
    folded_once(venue.venue(), &run)
}

/// S2: the venue ends the session with a close frame (hyperliquid testnet's `1000 Expired`). The order path reads a
/// reopen (`Connecting`) between two `Connected`, never `Error`; the close's code is in the words; the pump says it at
/// `info`.
fn server_close(venue: &dyn LinkVenue, row: &Row) -> Result<(), String> {
    let exec = timings(row)?;
    let (f1, f2) = (fill(1), fill(2));
    let run = rig(venue, row, |lane| Script {
        sessions: vec![
            Session::quiet(vec![
                text((lane.fill_frame)(f1)),
                UserScriptStep::Stall(Duration::from_secs(5)),
                UserScriptStep::Close { code: 1000, reason: "Expired".into() },
            ]),
            Session::quiet(vec![text((lane.fill_frame)(f2)), UserScriptStep::Stall(tail(&exec))]),
        ],
        listed: vec![(USER_DATA_POLL, f1), (Duration::from_secs(6), f2)],
    })?;
    let states = transitions(&run);
    check!(
        !states.iter().any(|(state, _)| *state == DataClientConnectionState::Error),
        "a server close read as Error: {states:?}"
    );
    let closed = states
        .iter()
        .position(|(state, why)| {
            *state == DataClientConnectionState::Connecting && why.contains("1000 Expired")
        })
        .ok_or_else(|| format!("no Connecting naming the close `1000 Expired`: {states:?}"))?;
    let connected = |states: &[(DataClientConnectionState, String)]| {
        states.iter().any(|(state, _)| *state == DataClientConnectionState::Connected)
    };
    check!(
        connected(&states[..closed]) && connected(&states[closed..]),
        "not Connected on both sides of the close: {states:?}"
    );
    let loud: Vec<&CapturedEvent> = pump_lines(&run)
        .filter(|line| line.level == Level::WARN || line.level == Level::ERROR)
        .collect();
    check!(loud.is_empty(), "a routine close was said above info: {loud:?}");
    check!(
        pump_lines(&run).any(|line| line.message.contains("closed mid-session")
            && line.field("error").is_some_and(|words| words.contains("1000 Expired"))),
        "no info line names the close: {:?}",
        run.lines
    );
    Ok(())
}

/// S3: on a session that lived past the ladder's reset (twice the row's ceiling, kept up by server pings inside its
/// idle limit), the venue sends its venue-native notice; the venue's REAL hook reads it and the pump reconnects at
/// once. The order path reads `Connecting` naming the row's notice between two `Connected`, never `Error`; the re-open
/// comes at the notice itself with `on_reconnect` once there; the replay schedule and the reconcile nudge are the row's;
/// the pump says it at `info`; the streamed fill folds once.
fn announced_close(venue: &dyn LinkVenue, row: &Row) -> Result<(), String> {
    let VenueConnectionAnnouncedClose::Handled { notice: declared } =
        row.connection().exec.announced_close
    else {
        return Err("the impl hands over a notice hook and the row does not say `Handled`".into());
    };
    let notice = venue.notice().ok_or_else(|| {
        format!("the row handles `{declared}` and the impl hands over no notice hook")
    })?;
    let exec = timings(row)?;
    let reconnect = exec.reconnect.ok_or("the row declares no reconnect")?;
    let beat = exec.idle_threshold.ok_or("the row declares no idle_threshold")?.value / 3;
    let ping = || UserScriptStep::Read(Ok(WebsocketSessionUserMessage::Ping(Vec::new())));
    let f1 = fill(1);
    let mut lived = Duration::ZERO;
    let run = rig(venue, row, |lane| {
        let mut first = vec![text((lane.fill_frame)(f1))];
        while lived < 2 * reconnect.max {
            first.extend([UserScriptStep::Stall(beat), ping()]);
            lived += beat;
        }
        first.push(text(notice.frame.to_string()));
        // `{}` decodes to nothing on every venue: it only lets the reopened session read `Connected`.
        let second = vec![text("{}".to_string()), UserScriptStep::Stall(tail(&exec))];
        Script {
            sessions: vec![Session::quiet(first), Session::quiet(second)],
            listed: vec![(USER_DATA_POLL, f1)],
        }
    })?;
    check!(run.opens.len() == 2, "{} opens for one notice: {:?}", run.opens.len(), run.opens);
    let reopen = run.opens[1];
    check!(
        reopen == run.opens[0] + lived,
        "re-opened at {reopen:?}; the notice came at {:?}, after a session of {lived:?}",
        run.opens[0] + lived
    );
    check!(
        run.reconnects == [reopen],
        "on_reconnect fired at {:?}, not once at the re-open",
        run.reconnects
    );
    let states = transitions(&run);
    check!(
        !states.iter().any(|(state, _)| *state == DataClientConnectionState::Error),
        "an announced close read as Error: {states:?}"
    );
    let announced = states
        .iter()
        .position(|(state, why)| {
            *state == DataClientConnectionState::Connecting && why.contains(declared)
        })
        .ok_or_else(|| format!("no Connecting naming `{declared}`: {states:?}"))?;
    let connected = |states: &[(DataClientConnectionState, String)]| {
        states.iter().any(|(state, _)| *state == DataClientConnectionState::Connected)
    };
    check!(
        connected(&states[..announced]) && connected(&states[announced..]),
        "not Connected on both sides of the notice: {states:?}"
    );
    match row.connection().exec.recovery {
        VenueConnectionRecovery::HistoryWindow => {
            let schedule = 1 + usize::from(history_lag(&exec).is_some());
            check!(
                run.reads.len() == schedule,
                "{:?} replays, the row's schedule is {schedule}",
                run.reads
            );
            let first = run.reads[0];
            check!(
                first >= reopen + RESYNC_SETTLE && first <= reopen + RESYNC_SETTLE + USER_DATA_POLL,
                "the first replay came at {first:?}, the settle after the re-open at {reopen:?}"
            );
        }
        _ => {
            check!(run.reads.len() == 2, "one subscribe snapshot per open, read at {:?}", run.reads)
        }
    }
    let nudges = usize::from(row.connection().exec.nudges_reconcile);
    check!(run.nudges == nudges, "reconcile nudged {} times; the row says {nudges}", run.nudges);
    let loud: Vec<&CapturedEvent> = pump_lines(&run)
        .filter(|line| line.level == Level::WARN || line.level == Level::ERROR)
        .collect();
    check!(loud.is_empty(), "an announced close was said above info: {loud:?}");
    check!(
        pump_lines(&run).any(|line| line.message.contains("announced")
            && line.field("notice").is_some_and(|words| words.contains(declared))),
        "no info line names the notice: {:?}",
        run.lines
    );
    folded_once(venue.venue(), &run)
}

/// S4, by what the venue's key is.
fn expiry(venue: &dyn LinkVenue, row: &Row) -> Result<(), String> {
    match venue.expiry() {
        Expiry::RefusedLogin { replies, open } => refused_login(row, replies, open),
        Expiry::ListenKey { open } => listen_key_expired(row, open),
        Expiry::ClosedAtOnce => closed_at_once(venue, row),
        Expiry::NotApplicable(why) => Err(format!("a scenario that does not run here ran: {why}")),
    }
}

/// S4, a login venue: the REAL open meets the venue's refusal of the key; the pump gives up at once (one open, no
/// reconnect loop) and the order path reads `Error`.
fn refused_login(
    row: &Row,
    replies: &'static [&'static str],
    open: fn(
        &str,
        &AtomicBool,
    ) -> WebsocketSessionUserOpenOutcome<WebsocketSessionUserStreamTungstenite>,
) -> Result<(), String> {
    let ladder = pump_ladder(row)?;
    let venue = FakePrivateWebsocket::spawn(replies);
    let url = venue.url();
    let key = route_key(row);
    let pump = std::thread::spawn(move || {
        report_on_this_thread(Some(stream_link(&key)));
        let stop = AtomicBool::new(false);
        let mut opens = 0usize;
        let ended = run_user(
            || {
                opens += 1;
                open(&url, &stop)
            },
            |_| Vec::new(),
            |_| true,
            &stop,
            USER_DATA_POLL,
            ladder,
            None,
            || {},
            None,
            Some(RECONNECT_LOG),
        );
        (ended, opens, link_state(&key))
    });
    let (ended, opens, (state, why)) =
        pump.join().map_err(|_| "the pump thread panicked".to_string())?;
    let refusal = ended.err().ok_or("the pump kept running after the venue refused the key")?;
    check!(
        opens == 1,
        "{opens} opens after a refused key: a refusal must never be retried in a loop"
    );
    check!(
        state == DataClientConnectionState::Error,
        "the order path reads {state:?} ({why}) after the venue refused the key ({})",
        refusal.0
    );
    Ok(())
}

/// S4, a listenKey venue: the venue says on the stream that the key expired; the session ends as a reopen
/// (`Connecting`), and the REAL re-open mints a new key, a reopen wait after the first open (never a hot loop).
fn listen_key_expired(
    row: &Row,
    open: fn(
        &str,
        &str,
        &AtomicBool,
    ) -> WebsocketSessionUserOpenOutcome<WebsocketSessionUserStreamKeyed>,
) -> Result<(), String> {
    let first_wait = first_wait(&timings(row)?)?;
    let ladder = pump_ladder(row)?;
    let minted = |n: u32| (200, format!(r#"{{"listenKey":"key-{n}"}}"#));
    let (rest, posts) = serve_replies("", vec![minted(1), minted(2), minted(3)]);
    let venue = FakePrivateWebsocket::spawn(&[]);
    let websocket = venue.url();
    let key = route_key(row);
    let stop = Arc::new(AtomicBool::new(false));
    let pump = {
        let stop = Arc::clone(&stop);
        std::thread::spawn(move || {
            report_on_this_thread(Some(stream_link(&key)));
            let mut opens: Vec<(Instant, (DataClientConnectionState, String))> = Vec::new();
            let ended = run_user(
                || {
                    opens.push((Instant::now(), link_state(&key)));
                    open(&rest, &websocket, &stop)
                },
                |_| Vec::new(),
                |_| true,
                &stop,
                USER_DATA_POLL,
                ladder,
                None,
                || {},
                None,
                Some(RECONNECT_LOG),
            );
            (ended, opens)
        })
    };
    venue.wait_ready()?;
    venue.push(r#"{"e":"listenKeyExpired","E":1}"#);
    let deadline = Instant::now() + Duration::from_secs(20);
    while lock(&posts).len() < 2 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    stop.store(true, Ordering::SeqCst);
    let (ended, opens) = pump.join().map_err(|_| "the pump thread panicked".to_string())?;
    let posts = lock(&posts).clone();
    check!(ended.is_ok(), "the pump gave up on an expired key: {:?}", ended.err());
    check!(posts.len() >= 2, "the re-open minted no new key: the venue heard {posts:?}");
    check!(posts.iter().all(|line| line.starts_with("POST")), "a key is minted by POST: {posts:?}");
    check!(opens.len() >= 2, "no re-open after the venue said the key expired");
    let (first, (second, (state, why))) = (opens[0].0, &opens[1]);
    check!(
        second.duration_since(first) >= first_wait,
        "re-opened {:?} after the first open, under the row's first wait {first_wait:?}",
        second.duration_since(first)
    );
    check!(
        *state == DataClientConnectionState::Connecting && why.contains("listenKey expired"),
        "an expired key read as {state:?} ({why}), not a reopen naming it"
    );
    Ok(())
}

/// S4, a venue with no login answer (polymarket: the subscribe is the login): a key it no longer takes ends each session
/// at once, and the pump reopens on the doubling ladder from the row's first wait to its ceiling, never faster.
fn closed_at_once(venue: &dyn LinkVenue, row: &Row) -> Result<(), String> {
    let exec = timings(row)?;
    let reconnect = exec.reconnect.ok_or("the row declares no reconnect")?;
    let closes = 5u32;
    let refused =
        || UserScriptStep::Close { code: 1008, reason: "the key is no longer accepted".into() };
    let run = rig(venue, row, |_| {
        let mut sessions: Vec<Session> =
            (0..closes).map(|_| Session::quiet(vec![refused()])).collect();
        sessions.push(Session::quiet(vec![UserScriptStep::Stall(USER_DATA_POLL)]));
        Script { sessions, listed: Vec::new() }
    })?;
    let gaps: Vec<Duration> = run.opens.windows(2).map(|pair| pair[1] - pair[0]).collect();
    let ladder: Vec<Duration> =
        (0..closes).map(|step| (reconnect.initial * 2u32.pow(step)).min(reconnect.max)).collect();
    check!(gaps == ladder, "re-opens {gaps:?} apart; the row's ladder is {ladder:?}");
    Ok(())
}

/// S5: the socket stays open and says nothing. The watchdog trips the row's `idle_threshold` after the last frame (at
/// least twice the keepalive), the order path reads `Error`, then `Connected` on the reopened session. On a venue whose
/// ladder spares a silence (`ReconnectBackoff::silence_spared`, no documented heartbeat cadence) the trip is a reopen:
/// `Connecting`, never `Error`, said at `info`.
fn silent_stall(venue: &dyn LinkVenue, row: &Row) -> Result<(), String> {
    let exec = timings(row)?;
    let first_wait = first_wait(&exec)?;
    let idle = exec
        .idle_threshold
        .ok_or("the row declares no idle_threshold: an exec lane on the shared pump runs the stall watchdog")?
        .value;
    if let Some(every) = exec.keepalive_every {
        check!(
            idle >= 2 * every.value,
            "the idle threshold {idle:?} is under twice the keepalive {:?}",
            every.value
        );
    }
    let (f1, f2) = (fill(1), fill(2));
    let run = rig(venue, row, |lane| Script {
        sessions: vec![
            Session::quiet(vec![
                text((lane.fill_frame)(f1)),
                UserScriptStep::Stall(idle + 3 * USER_DATA_POLL),
            ]),
            Session::quiet(vec![text((lane.fill_frame)(f2)), UserScriptStep::Stall(tail(&exec))]),
        ],
        listed: vec![(USER_DATA_POLL, f1)],
    })?;
    check!(run.opens.len() == 2, "{} opens for one stall: {:?}", run.opens.len(), run.opens);
    let tripped = run.opens[1] - first_wait;
    let last_frame =
        run.decoded.iter().copied().filter(|at| *at < tripped).max().unwrap_or_default();
    check!(
        tripped >= last_frame + idle && tripped <= last_frame + idle + USER_DATA_POLL,
        "the watchdog tripped {:?} after the last frame; the row's idle threshold is {idle:?}",
        tripped - last_frame
    );
    let states = transitions(&run);
    let spared = exec.reconnect.is_some_and(|reconnect| venue.ladder(&reconnect).silence_spared());
    let (read, level, words) = if spared {
        (DataClientConnectionState::Connecting, Level::INFO, "quiet past its idle bound")
    } else {
        (DataClientConnectionState::Error, Level::WARN, "silent stall")
    };
    let down = states
        .iter()
        .position(|(state, why)| *state == read && why.starts_with("no frame from the venue"))
        .ok_or_else(|| format!("a silent stall never read as {read:?}: {states:?}"))?;
    check!(
        states[down..].iter().any(|(state, _)| *state == DataClientConnectionState::Connected),
        "{read:?} latched after the reopened session: {states:?}"
    );
    check!(
        !spared || states.iter().all(|(state, _)| *state != DataClientConnectionState::Error),
        "a silence the ladder spares read as Error: {states:?}"
    );
    check!(
        pump_lines(&run).any(|line| line.level == level && line.message.contains(words)),
        "no {level} line says the stall: {:?}",
        run.lines
    );
    Ok(())
}

/// S6: a fill lands while the socket is down, on neither session. The row's recovery brings it back, once: the first
/// replay on a history venue, the re-open's subscribe snapshot on a snapshot venue, on a cursor venue the re-open's
/// read past its cursor or its re-send from the cursor (whose stream carried the order's acceptance before the drop: a
/// re-sending cursor resumes from an event, and a stream that has seen none is a declared gap of its row).
fn gap_fill(venue: &dyn LinkVenue, row: &Row) -> Result<(), String> {
    let exec = timings(row)?;
    let f1 = fill(1);
    let run = rig(venue, row, |lane| Script {
        sessions: dropped_once(
            &exec,
            match &lane.recovery {
                Recovery::Cursor(CursorRead::Resend { accepted, .. }) => vec![text(accepted(f1))],
                _ => Vec::new(),
            },
        ),
        listed: vec![(DROP_AT, f1)],
    })?;
    let delivered = fills(&run);
    let Some((first, _)) = delivered.first() else {
        return Err(format!(
            "the fill the gap held never came back (history read at {:?})",
            run.reads
        ));
    };
    match row.connection().exec.recovery {
        VenueConnectionRecovery::HistoryWindow => check!(
            first.source == Source::Replay && run.reads.first() == Some(&first.at),
            "the gap's fill came by {:?} at {:?}, not by the first replay at {:?}",
            first.source,
            first.at,
            run.reads.first()
        ),
        _ => check!(
            run.opens.get(1) == Some(&first.at),
            "the gap's fill came at {:?}, not in the re-open's snapshot or cursor read at {:?}",
            first.at,
            run.opens.get(1)
        ),
    }
    folded_once(venue.venue(), &run)
}

/// S7 (a history venue): the history lists a fill of the gap late. Listed `history_lag` after the first replay's settle,
/// the second replay brings it back; listed later, or on a row with no `history_lag`, no replay does: reconcile's.
fn lagging_history(venue: &dyn LinkVenue, row: &Row) -> Result<(), String> {
    let exec = timings(row)?;
    let reopen = DROP_AT + first_wait(&exec)?;
    let f1 = fill(1);
    let listed_from = |at: Duration| {
        rig(venue, row, |_| Script {
            sessions: dropped_once(&exec, Vec::new()),
            listed: vec![(at, f1)],
        })
    };
    let Some(lag) = history_lag(&exec) else {
        let run = listed_from(reopen + RESYNC_SETTLE + 2 * USER_DATA_POLL)?;
        check!(run.reads.len() == 1, "{:?} replays on a row with no history lag", run.reads);
        check!(
            fills(&run).is_empty(),
            "a fill listed after the one replay was delivered: the schedule is not the row's"
        );
        return Ok(());
    };
    check!(
        lag > USER_DATA_POLL,
        "a history lag of {lag:?} is under the pump's poll: the replays cannot tell it"
    );
    let run = listed_from(reopen + RESYNC_SETTLE + lag)?;
    check!(run.reads.len() == 2, "{:?} replays; the row's lag asks for two", run.reads);
    let delivered = fills(&run);
    check!(
        delivered.first().is_some_and(|(seen, _)| Some(&seen.at) == run.reads.get(1)),
        "a fill listed {lag:?} late was not brought back by the second replay at {:?}: {:?}",
        run.reads.get(1),
        delivered.iter().map(|(seen, f)| (seen.at, f.trade_id.as_str())).collect::<Vec<_>>()
    );
    folded_once(venue.venue(), &run)?;
    let late = listed_from(reopen + RESYNC_SETTLE + lag + 2 * USER_DATA_POLL)?;
    check!(
        fills(&late).is_empty(),
        "a fill listed after the second replay was delivered: the schedule is not the row's"
    );
    Ok(())
}

/// The venue end of S8: every amend is a no-op, as `VenueRest`'s default is.
struct Quiet;

impl VenueRest for Quiet {
    fn submit_order(&self, _request: &OrderRequest) -> Vec<Event> {
        Vec::new()
    }
    fn cancel_order(&self, _order_client_id: &str) -> Result<OrderCancelAck, VenueApiError> {
        Ok(OrderCancelAck::Canceled)
    }
}

/// S8: an amend a second for three gap-sentinel windows. The sentinel is a fixed deadline: it replays the venue's
/// history once per window while the commands come, the first a window after the first command.
fn busy_commands(venue: &dyn LinkVenue, row: &Row) -> Result<(), String> {
    let Sentinel::RunLoop(window) = venue.sentinel() else {
        return Err("a scenario that does not apply ran: the venue runs its own exec loop".into());
    };
    let Recovery::History(map) = venue.lane().recovery else {
        return Err("a `run_loop` venue replays its history; the impl hands over a snapshot".into());
    };
    let clock = VirtualClock::new(2);
    let history = LaggedHistory::new();
    history.lists_from(clock.now(), fill(1));
    let (commands, received) = mpsc::channel();
    let windows = 3u32;
    let exec_thread = {
        let (clock, history) = (Arc::clone(&clock), history.clone());
        let name = venue.venue();
        std::thread::Builder::new()
            .name(format!("link-exec {}", row.label()))
            .spawn(move || {
                let _seat = clock.enter();
                let (events, _ingest) = event_channel(1024);
                run_loop(
                    &Quiet,
                    &events,
                    &received,
                    || map(&history.read()),
                    FillSentinel::reading(window, name, "link-conformance"),
                );
            })
            .map_err(|e| format!("spawn the exec thread: {e}"))?
    };
    let driver = {
        let clock = Arc::clone(&clock);
        std::thread::spawn(move || {
            let _seat = clock.enter();
            let order = OrderRequest {
                order_client_id: "c1".into(),
                symbol: "link-conformance".into(),
                side: 1,
                qty: 1.0,
                order_type: OrderType::Limit,
                price: Some(50_000.0),
                ..Default::default()
            };
            let every = Duration::from_secs(1);
            let count = u32::try_from(window.as_secs()).expect("a small window") * windows;
            for _ in 0..count {
                let _ = commands.send(ExecutionClientActorCommand::Modify {
                    order: Box::new(order.clone()),
                    new_qty: None,
                    new_price: Some(50_001.0),
                    new_trigger_price: None,
                    queued_at: clock.now(),
                });
                clock.sleep(every);
            }
            clock.sleep(2 * window);
            let _ = commands.send(ExecutionClientActorCommand::Shutdown);
        })
    };
    driver.join().map_err(|_| "the command driver panicked".to_string())?;
    exec_thread.join().map_err(|_| "the exec thread panicked".to_string())?;
    let reads: Vec<Duration> = history.reads().into_iter().map(|at| clock.offset(at)).collect();
    for k in 1..=windows {
        let due = window * k;
        let slack = Duration::from_millis(10);
        check!(
            reads.get(k as usize - 1).is_some_and(|at| *at >= due && *at <= due + slack),
            "replays at {reads:?} under an amend a second: the fixed deadline puts replay {k} at {due:?}"
        );
    }
    Ok(())
}

/// S9, the race half (a snapshot venue): a fill lands while the socket is down and the re-open's subscribe snapshot
/// carries it. The venue's own nudge pokes reconcile once, after that snapshot is decoded, never at the re-open, so the
/// pass it starts does not read the venue against a book the gap's fill has not reached. The persistence half (a
/// one-pass divergence is never held) is venue-neutral: `crates/vike-core/src/runtime/tests/recon_held/persist.rs`'s
/// `a_race_the_next_pass_heals_is_never_held`.
fn reconcile_race(venue: &dyn LinkVenue, row: &Row) -> Result<(), String> {
    let exec = timings(row)?;
    let run = rig(venue, row, |_| Script {
        sessions: dropped_once(&exec, Vec::new()),
        listed: vec![(DROP_AT, fill(1))],
    })?;
    let reopen = *run.opens.get(1).ok_or("no re-open after the drop")?;
    let snapshot = run
        .decoded
        .iter()
        .position(|at| *at >= reopen)
        .ok_or("the re-open's snapshot was never decoded")?;
    check!(
        run.nudged_after == [snapshot + 1],
        "reconcile nudged after {:?} decoded frames; the re-open's snapshot is frame {}: once, after it",
        run.nudged_after,
        snapshot + 1
    );
    folded_once(venue.venue(), &run)
}

/// S10: a fill comes on the stream, then again in the replay (or the re-open's subscribe snapshot). The stream's and
/// the history's parsers name it with the same trade id and symbol, and it folds once.
fn double_delivery(venue: &dyn LinkVenue, row: &Row) -> Result<(), String> {
    let exec = timings(row)?;
    let f1 = fill(1);
    let run = rig(venue, row, |lane| Script {
        sessions: dropped_once(&exec, vec![text((lane.fill_frame)(f1))]),
        listed: vec![(USER_DATA_POLL, f1)],
    })?;
    let delivered = fills(&run);
    let Some((streamed, first)) = delivered.first().copied() else {
        return Err("the stream frame decoded to no fill".into());
    };
    check!(streamed.source == Source::Stream, "the first delivery came by {:?}", streamed.source);
    let again: Vec<&FillEvent> = delivered[1..]
        .iter()
        .filter(|(_, f)| f.order_client_id == first.order_client_id)
        .map(|(_, f)| *f)
        .collect();
    if row.connection().exec.recovery == VenueConnectionRecovery::HistoryWindow {
        check!(
            !again.is_empty(),
            "the replay did not carry the streamed fill of {}",
            first.order_client_id
        );
    }
    for other in &again {
        check!(
            other.trade_id == first.trade_id && other.symbol == first.symbol,
            "the stream names the fill {} on {} and the history {} on {}",
            first.trade_id.as_str(),
            first.symbol.as_str(),
            other.trade_id.as_str(),
            other.symbol.as_str()
        );
    }
    folded_once(venue.venue(), &run)
}

/// S11: the venue's keepalive frame at the row's `keepalive_every` on a session the venue keeps answering, or a pong
/// to each server ping and nothing of our own.
fn keepalive_cadence(venue: &dyn LinkVenue, row: &Row) -> Result<(), String> {
    let exec = timings(row)?;
    match venue.keepalive() {
        Keepalive::Sends(payload) => {
            let every = exec
                .keepalive_every
                .ok_or("the impl sends a keepalive the row declares no cadence for")?
                .value;
            let run = rig(venue, row, |_| Script {
                sessions: vec![Session {
                    steps: vec![UserScriptStep::Stall(3 * every + USER_DATA_POLL)],
                    answers_sends: true,
                }],
                listed: Vec::new(),
            })?;
            check!(
                run.opens.len() == 1,
                "a session the venue kept answering was cut: opens {:?}",
                run.opens
            );
            let opened = run.opens[0];
            let sent: Vec<(Duration, String)> = run
                .heard
                .iter()
                .filter_map(|(at, heard)| match heard {
                    UserScriptHeard::Text(text) => Some((*at - opened, text.clone())),
                    UserScriptHeard::Pong => None,
                })
                .collect();
            let want: Vec<(Duration, String)> =
                (1..=3u32).map(|n| (every * n, payload(u64::from(n)))).collect();
            check!(
                sent == want,
                "sent {sent:?}; the row's cadence and the venue's payload are {want:?}"
            );
            Ok(())
        }
        Keepalive::ServerPings => {
            let idle = exec.idle_threshold.ok_or("the row declares no idle_threshold")?.value;
            let ping = || UserScriptStep::Read(Ok(WebsocketSessionUserMessage::Ping(Vec::new())));
            let quiet = || UserScriptStep::Stall(idle / 3);
            let run = rig(venue, row, |_| Script {
                sessions: vec![Session::quiet(vec![
                    ping(),
                    quiet(),
                    ping(),
                    quiet(),
                    ping(),
                    quiet(),
                ])],
                listed: Vec::new(),
            })?;
            check!(
                run.opens.len() == 1,
                "a session the server kept pinging was cut: opens {:?}",
                run.opens
            );
            let pongs =
                run.heard.iter().filter(|(_, heard)| *heard == UserScriptHeard::Pong).count();
            let ours = run.heard.len() - pongs;
            check!(
                pongs == 3 && ours == 0,
                "{pongs} pongs to 3 server pings, and {ours} frames of our own"
            );
            Ok(())
        }
        Keepalive::Deferred(why) => Err(format!("a deferred cell ran: {why}")),
        Keepalive::OnItsSeam { pins, .. } => Err(format!("a cell proven on its seam ran: {pins}")),
    }
}

/// S12 (wall clock: a socket): the venue's REAL open, dialled at a peer that accepts and says nothing, is cut at its
/// window, the row's `dial_timeout` or `CONNECT_10S`.
fn dial_bound(venue: &dyn LinkVenue, row: &Row) -> Result<(), String> {
    let exec = timings(row)?;
    let window = exec.dial_timeout.map_or(CONNECT_10S, |dial| dial.value);
    let began = Instant::now();
    let dial = venue.dial_silent_peer();
    let took = began.elapsed();
    check!(
        took + Duration::from_millis(500) >= window,
        "the silent peer was cut after {took:?}, well inside the {window:?} window ({})",
        dial.failure
    );
    check!(
        took <= window + Duration::from_secs(5),
        "the silent peer held the dial {took:?}; its window is {window:?} ({})",
        dial.failure
    );
    Ok(())
}
