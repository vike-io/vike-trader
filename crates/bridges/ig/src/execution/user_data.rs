//! IG Lightstreamer trade-update stream (TLCP text mode) — IG's async fill/terminal lane, which
//! the sync `/confirms` call cannot carry (a resting working order fills LATER).
//!
//! Flow: `POST {endpoint}/lightstreamer/create_session.txt` opens a streaming text body; the first
//! `CONOK,<sessionId>,...,<controlLink>` line names the session, then `POST .../control.txt`
//! subscribes to `TRADE:{accountId}` over the `CONFIRMS OPU WOU` schema, and update lines stream
//! back. The `OPU` field of an update that says a position OPENED is decoded by the PURE
//! [`crate::execution::event_mapper`] (dual-publish fill) and pushed to the core through the shared
//! `vike_bridge_core::exec_actor::submit_fence::SubmitFence`, which holds it behind the order's own answer while its
//! submit is in flight ([`events_for_line`] says why that field and no other).
//!
//! Audit A3: the subscription requests a SNAPSHOT (`LS_snapshot=true`), so on a reconnect the
//! current trade state is re-delivered — a fill/terminal that landed during the drop is recovered,
//! and the core's `trade_id`/FSM dedup absorbs the overlap with anything already seen. Blocks — run
//! on a dedicated thread. NOTE: transport correctness (framing, rebind, keepalive) is verifiable
//! only against a live IG Lightstreamer server; the decode/parse layer is unit-tested.

use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use vike_bridge_core::exec_actor::submit_fence::SubmitFence;
use vike_bridge_core::execution_client::order_client_id::FifoMap;
use vike_bridge_core::websocket_session::user::sleep_unless_stopped;
use vike_execution::EventSender;
use vike_model::events::Event;
use vike_venues::venue_timings;

use crate::execution::event_mapper::{decode_trade_confirm, parse_update_line};
use crate::history::prices::parse_ig_time_utc;
// ⚠ ONE handshake, ONE spelling. This lane used to carry its own `LS_PROTOCOL`, `form` and
// `urlencode` — behaviourally identical copies of `feed/decode/lightstreamer.rs`'s, whose own `form` doc had
// claimed since it was written that `crate::stream` shared it. That untrue claim IS the defect
// shape: two spellings of one wire drifted, and `LS_cid` is where the drift landed. Take a TLCP
// constant or `form` from `feed/decode/lightstreamer.rs` (whose percent-encoding is
// `vike_utility::percent::percent_encode`, decision 0141); never re-spell one here.
use crate::account::venue_link::SessionLink;
use crate::feed::decode::lightstreamer::{LS_CID, LS_PROTOCOL, form};
use crate::transport::IgSession;

mod retry;

#[cfg(doc)]
use retry::MAX_REFUSED_HANDSHAKES;
use retry::{DialOutcome, RetrySay, RetryState};

/// What the exec thread learned at submit about ONE order, for the lanes that run afterwards.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Pending {
    /// The `dealReference` IG answered the submit POST with — the ONLY key `GET /confirms/{ref}`
    /// accepts, and therefore the only way to ask IG about this order again.
    pub deal_ref: String,
    /// Whether the submit was a MARKET deal (a position open, which fills inline) rather than a
    /// working order. `execution::map_confirm` needs it to decide whether a confirm carries a fill.
    pub market: bool,
}

/// The exec thread's submit-time knowledge, shared with everything that has to ask about an order
/// LATER. Both directions are kept because the two readers ask opposite questions and neither can
/// answer with a scan: the Lightstreamer driver holds a `dealReference` and needs the order client id, while
/// [`vike_execution::ExecutionClient::confirm`] holds an order client id and needs the `dealReference`.
///
/// ⚠ **This used to be the `by_ref` half alone**, which is why an IG order whose `/confirms` call
/// failed could never be re-asked about: nothing in the process could turn its order client id back into the
/// one key IG's confirm endpoint takes.
#[derive(Debug, Default)]
pub(crate) struct DealRefs {
    by_ref: HashMap<String, String>,
    /// Bounded by the order each order client id was first recorded in (the shared [`FifoMap`]): what [`MAX_DEAL_REFS`] evicts by.
    by_order_client_id: FifoMap<Pending, MAX_DEAL_REFS>,
    /// The references of cancels this process has not confirmed yet ([`Self::record_stream_only`]), the only ones whose
    /// streamed `CONFIRMS` the lane reads ([`events_for_line`]).
    stream_only: std::collections::HashSet<String>,
}

/// How many orders' references are kept. The map lives as long as the process and every submit
/// adds to it, so it is bounded: past this the OLDEST order's entries go (a long-settled order;
/// the confirm watchdog and the stream only ask about recent ones).
const MAX_DEAL_REFS: usize = 4096;

impl DealRefs {
    /// Record a submitted order's `dealReference` in BOTH directions. Called by the exec thread the
    /// moment the submit POST returns, i.e. BEFORE the confirm is fetched — so a fill that lands in
    /// between still correlates.
    pub(crate) fn record(&mut self, order_client_id: &str, deal_ref: &str, market: bool) {
        self.by_ref.insert(deal_ref.to_string(), order_client_id.to_string());
        let pending = Pending { deal_ref: deal_ref.to_string(), market };
        if let Some((oldest, _)) =
            self.by_order_client_id.insert(order_client_id.to_string(), pending)
        {
            self.forget(&oldest);
        }
    }

    /// Drop everything known about `order_client_id`, in both directions (a REJECTED order: nothing will ever
    /// be asked about it again). Its slot in the [`MAX_DEAL_REFS`] bound is freed at once (owner, 2026-11-12).
    pub(crate) fn forget(&mut self, order_client_id: &str) {
        self.by_order_client_id.remove(order_client_id);
        self.by_ref.retain(|deal_ref, c| {
            if c == order_client_id {
                self.stream_only.remove(deal_ref);
            }
            c != order_client_id
        });
    }

    /// Teach the STREAM alone that `deal_ref` belongs to `order_client_id`, leaving `order_client_id`'s submit-time
    /// [`Pending`] untouched. For a reference minted by a LATER request about the same order (a
    /// cancel): recording it through [`Self::record`] would overwrite the `order_client_id -> dealReference`
    /// half the confirm watchdog re-asks IG with.
    pub(crate) fn record_stream_only(&mut self, order_client_id: &str, deal_ref: &str) {
        self.by_ref.insert(deal_ref.to_string(), order_client_id.to_string());
        self.stream_only.insert(deal_ref.to_string());
    }

    /// Teach the stream's ROUTE alone (`deal_ref -> order_client_id`) for an order a previous process placed (the exec client's
    /// `adopt_restored`): its `OPU` then reaches it exactly as it would have in that process. Neither the confirm
    /// watchdog's [`Pending`] (whether the order was a market deal is not known here) nor a [`Self::record_stream_only`]
    /// read of a `CONFIRMS` comes with it. Not counted by [`MAX_DEAL_REFS`]: the re-teaching is bounded at the daemon.
    pub(crate) fn record_route(&mut self, order_client_id: &str, deal_ref: &str) {
        self.by_ref.insert(deal_ref.to_string(), order_client_id.to_string());
    }

    /// The order whose unconfirmed cancel minted `deal_ref`, once: whichever of the stream and the cancel's own
    /// `/confirms` ladder takes it first settles the cancel, and the other finds nothing.
    pub(crate) fn take_stream_only(&mut self, deal_ref: &str) -> Option<String> {
        self.stream_only.remove(deal_ref).then(|| self.by_ref.remove(deal_ref)).flatten()
    }

    /// Whether the stream still waits for `deal_ref`'s confirm: `false` once it has ended that cancel's order.
    pub(crate) fn awaits_stream(&self, deal_ref: &str) -> bool {
        self.stream_only.contains(deal_ref)
    }

    /// The order a streamed update belongs to, by the `dealReference` it echoes.
    pub(crate) fn order_client_id_for(&self, deal_ref: &str) -> Option<String> {
        self.by_ref.get(deal_ref).cloned()
    }

    /// What a re-confirm of `order_client_id` needs. `None` = this process never got a `dealReference` for
    /// that order, so there is nothing IG can be asked.
    pub(crate) fn pending_for(&self, order_client_id: &str) -> Option<Pending> {
        self.by_order_client_id.as_map().get(order_client_id).cloned()
    }
}

/// Shared submit-time state (populated by exec at submit) — see [`DealRefs`].
pub(crate) type OrderVenueIdRegistryDealReference = Arc<Mutex<DealRefs>>;

/// Our fixed subscription id (one subscription per session). Stream-local on purpose: the
/// market-data lane takes its `sub_id` as a parameter because it opens many, this lane opens one.
const SUB_ID: u32 = 1;

/// Parameters the driver needs, sourced from a logged-in [`IgSession`] the driver KEEPS.
///
/// ⚠ **The session is held, not sampled.** This used to be three `String`s including a
/// `password` snapshot taken once at spawn; when IG expired the tokens behind it, every reconnect
/// re-presented the same dead pair and the fill lane stayed down for the life of the process with
/// nothing in the log saying so. `IgSession::ls_password` renders the CURRENT pair, so reading it
/// per dial is what makes [`IgSession::relogin`] reach this lane at all.
pub(crate) struct LightstreamerDial {
    pub session: Arc<IgSession>,
    pub endpoint: String,
    pub account_id: String,
    /// Where each dial's outcome and each line read are stated (`crate::account::venue_link`).
    pub link: SessionLink,
    /// The fence the exec thread's submit arm holds (`SubmitFence::enter`): every event a line implies passes it
    /// ([`SubmitFence::deliver`]), so a market deal's `OPU`, which reaches the stream within milliseconds of IG's
    /// answer and before the exec thread has read the order's `/confirms`, goes out AFTER the order's own answer
    /// (`Fill` and `OrderFilled` ahead of `OrderAccepted` was `terminal event dropped on a live order
    /// (out-of-order)`, the first GBPUSD order of the 2026-10-28 round trips).
    pub fence: Arc<SubmitFence>,
}

/// The trade stream's agent: `timeout_recv_body` is the exec lane's `idle_threshold` in
/// `vike_venues::venue_timings::IG`, the bound on each read WAIT of the chunked body (the
/// stream's server `PROBE` lines are its keepalive), and `timeout_connect` the lane's `dial_timeout`
/// (owner ruling B-13: a host that answers no SYN no longer holds the thread for the OS connect
/// timeout). No handshake bound is set: the row declares none, and this builder must not invent one.
fn trade_stream_agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        // No redirect: the create_session POST carries the CST/XST pair, which ureq would replay to a 3xx `Location` (`run_once` refuses a non-2xx).
        .max_redirects(0)
        .timeout_connect(Some(venue_timings::IG.exec().dial_timeout()))
        .timeout_recv_body(Some(venue_timings::IG.exec().idle_threshold()))
        .user_agent("vike-trader-rust")
        .build()
        .new_agent()
}

/// Drive the IG Lightstreamer TRADE stream until `stop`, reconnecting on any drop.
///
/// ⚠ **A refused handshake and a dropped socket are NOT the same failure and are not treated the
/// same.** `CONERR` means the credentials this dial presented were rejected — which, for a session
/// whose tokens simply aged out, is fixed by [`IgSession::relogin`] and by nothing else. A
/// transport drop means the network moved; re-logging-in on one would spend the account's login
/// budget on an outage. The refusal path therefore re-authenticates (bounded by
/// [`MAX_REFUSED_HANDSHAKES`]) and the transport path does not.
///
/// ⚠ **Past that bound the driver keeps dialling but stops SHOUTING, and the two are separate
/// decisions.** This doc used to claim it "stops pretending the next attempt is routine"; it stopped
/// re-logging-in and never stopped LOGGING, so a rejected account wrote an `error` line per second
/// forever (`vike_bridge_core::websocket_session::user::reconnect`'s `RECONNECT_LOG` carries the measurement). Every retry-and-log decision
/// now belongs to [`RetryState::plan`], which is pure and unit-tested; this loop only dials, renders
/// and sleeps.
pub(crate) fn stream_trades(
    params: LightstreamerDial,
    deal_refs: OrderVenueIdRegistryDealReference,
    events: EventSender,
    stop: Arc<AtomicBool>,
) {
    let agent = trade_stream_agent();

    let started = Instant::now();
    let mut retry = RetryState::default();
    while !stop.load(Ordering::Relaxed) {
        let (outcome, error) = match run_once(&agent, &params, &deal_refs, &events, &stop) {
            Ok(()) => (DialOutcome::Ok, String::new()),
            Err(RunErr::CoreGone) => return, // ingest closed — nothing to reconnect for
            Err(RunErr::Dropped(e)) => (DialOutcome::Dropped, e),
            Err(RunErr::NeverEstablished(e)) => (DialOutcome::NeverEstablished, e),
            Err(RunErr::Refused(e)) => (DialOutcome::Refused, e),
        };
        // A stopping driver says nothing: the cell may be its successor's by now.
        if outcome != DialOutcome::Ok && !stop.load(Ordering::Relaxed) {
            params.link.stream_lost(&error);
        }
        let plan = retry.plan(outcome, started.elapsed());
        match plan.say {
            RetrySay::Nothing => {}
            RetrySay::Dropped => {
                tracing::warn!(target: "vike_ig::stream", error = %error, "LS trade stream dropped; reconnecting");
            }
            RetrySay::DialFailed { attempt, next_in } => {
                tracing::warn!(
                    target: "vike_ig::stream",
                    error = %error,
                    attempt,
                    retry_in_s = plan.sleep.as_secs(),
                    next_update_in_s = next_in.as_secs(),
                    "LS trade stream could not open a session at all — no working-order fill or \
                     cancel can arrive while this holds. Dialling continues with exponential \
                     backoff; this line repeats on a DECAYING heartbeat, NOT once per attempt, so \
                     expect the next update in `next_update_in_s` and not sooner"
                );
            }
            RetrySay::DialStillFailing { attempt, down_for, next_in } => {
                tracing::warn!(
                    target: "vike_ig::stream",
                    error = %error,
                    attempt,
                    down_for_s = down_for.as_secs(),
                    retry_in_s = plan.sleep.as_secs(),
                    next_update_in_s = next_in.as_secs(),
                    "LS trade stream STILL cannot open a session — every dial since the line above \
                     has failed before the stream began"
                );
            }
            RetrySay::RefusedRetryingLogin { attempt } => {
                tracing::warn!(
                    target: "vike_ig::stream",
                    error = %error,
                    attempt,
                    "LS trade stream REFUSED our credentials; re-authenticating the IG session \
                     before the next dial"
                );
            }
            RetrySay::FillLaneDown { streak, next_in } => {
                tracing::error!(
                    target: "vike_ig::stream",
                    error = %error,
                    streak,
                    retry_in_s = plan.sleep.as_secs(),
                    next_update_in_s = next_in.as_secs(),
                    "LS trade stream refused the handshake again after a fresh login, so the cause \
                     is NOT an expired token. The IG fill lane is DOWN: working-order fills and \
                     cancels will not arrive. What this driver knows stops there — read the \
                     CONERR code in `error` for the cause, e.g. 71 = the LS_cid this client sends \
                     is not licensed (a CLIENT defect, nothing to do with the account), 1/2 = the \
                     credentials or adapter set were rejected. Dialling continues with exponential \
                     backoff; this line repeats on a DECAYING heartbeat, NOT once per retry, so \
                     expect the next update in `next_update_in_s` and not sooner"
                );
            }
            RetrySay::FillLaneStillDown { streak, down_for, next_in } => {
                tracing::error!(
                    target: "vike_ig::stream",
                    error = %error,
                    streak,
                    down_for_s = down_for.as_secs(),
                    retry_in_s = plan.sleep.as_secs(),
                    next_update_in_s = next_in.as_secs(),
                    "LS trade stream fill lane STILL DOWN — IG has refused every handshake since \
                     the line above. Working-order fills and cancels are not arriving"
                );
            }
        }
        if plan.relogin {
            // Recovery, not a guess: `relogin` no-ops when a sibling already refreshed, and logs
            // its own failure at `error`.
            let _ = params.session.relogin(params.session.token_generation());
        }
        // A stream that dropped is not dialled again while the FX market is closed: one wait to the open (owner
        // ruling 2026-11-13).
        sleep_unless_stopped(
            &stop,
            vike_bridge_core::fx_login::held_pause(super::VENUE, plan.sleep),
        );
    }
}

/// Why a session run ended.
///
/// ⚠ **The transport half is TWO variants, and which one a site constructs is a disk-safety
/// decision.** They were one `Transient` for both, warned about on every turn at a flat delay,
/// which is correct for a mid-stream drop and a warn-a-second flood for an unreachable gateway —
/// see [`DialOutcome`]. Construct [`RunErr::Dropped`] only from INSIDE the streaming read loop;
/// everything up to and including the subscribe POST is [`RunErr::NeverEstablished`].
enum RunErr {
    /// The core ingest closed — stop entirely, there is nothing to reconnect for.
    CoreGone,
    /// A read failed after the stream was already running. Retry with the SAME credentials.
    Dropped(String),
    /// The dial produced no subscribed session. Retry with the SAME credentials, backing off.
    NeverEstablished(String),
    /// The server REFUSED the handshake (`CONERR`, or anything else where a session was not
    /// established). Re-authenticate before retrying — this is what token expiry looks like here.
    Refused(String),
}

/// One session lifetime: create → subscribe (with snapshot) → read until drop/stop.
fn run_once(
    agent: &ureq::Agent,
    params: &LightstreamerDial,
    deal_refs: &OrderVenueIdRegistryDealReference,
    events: &EventSender,
    stop: &Arc<AtomicBool>,
) -> Result<(), RunErr> {
    // 1. create_session — streaming response.
    let create_url =
        format!("{}/lightstreamer/create_session.txt?LS_protocol={LS_PROTOCOL}", params.endpoint);
    // Read the password PER DIAL (never cached in `params`): after a re-login this is the only way
    // the fresh CST/XST pair reaches the wire.
    let create_body = create_session_body(&params.account_id, &params.session.ls_password());
    let response = agent
        .post(&create_url)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .send(create_body.as_bytes())
        .map_err(|e| RunErr::NeverEstablished(format!("create_session failed: {e}")))?;
    // Redirects are off (`trade_stream_agent`), so a 3xx arrives as a response: refuse it by status.
    if !(200..300).contains(&response.status().as_u16()) {
        return Err(RunErr::NeverEstablished(format!(
            "create_session: HTTP {}",
            response.status().as_u16()
        )));
    }
    let reader = BufReader::new(response.into_body().into_reader());
    let mut lines = reader.lines();

    // First control line must be CONOK,<sessionId>,<reqLimit>,<keepalive>,<controlLink>.
    let first = lines
        .next()
        .ok_or_else(|| RunErr::NeverEstablished("create_session: empty response".into()))?
        .map_err(|e| RunErr::NeverEstablished(format!("create_session read: {e}")))?;
    let (session_id, control_link) = parse_conok(&first).ok_or_else(|| {
        // ⚠ The classification, not just the message, is the fix: a `CONERR` here is IG telling us
        // the CST/XST pair we presented is dead, which the driver can cure. Anything else that is
        // not a CONOK is a protocol surprise we cannot cure by logging in again.
        let msg = format!("create_session: expected CONOK, got: {first}");
        if is_conerr(&first) { RunErr::Refused(msg) } else { RunErr::NeverEstablished(msg) }
    })?;
    let control_base = if control_link.is_empty() || control_link == "*" {
        params.endpoint.clone()
    } else {
        rebase_host(&params.endpoint, &control_link)
    };

    // 2. subscribe to TRADE:{account} WITH SNAPSHOT (audit A3 recovery on reconnect).
    let control_url = format!("{control_base}/lightstreamer/control.txt?LS_protocol={LS_PROTOCOL}");
    let sub_body = subscribe_body(&session_id, &params.account_id);
    // ⚠ NEVER-ESTABLISHED even though a `CONOK` arrived: the self-limiting argument that lets a
    // drop warn per turn is that the session STREAMED, and this one never did. A gateway that
    // CONOKs and then refuses `control.txt` turns this loop as fast as an unreachable one does.
    let sub = agent
        .post(&control_url)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .send(sub_body.as_bytes())
        .map_err(|e| RunErr::NeverEstablished(format!("subscribe failed: {e}")))?;
    if !(200..300).contains(&sub.status().as_u16()) {
        return Err(RunErr::NeverEstablished(format!("subscribe: HTTP {}", sub.status().as_u16())));
    }
    params.link.stream_answered();

    // 3. read update lines until stop/drop.
    let session = vike_bridge_core::link_history::VenueLinkRecord::of(events, "trade-updates");
    for line in lines {
        if stop.load(Ordering::Relaxed) {
            return Ok(());
        }
        let line = match line {
            Ok(line) => line,
            Err(e) => {
                let why = format!("stream read: {e}");
                session.ended(&why);
                return Err(RunErr::Dropped(why));
            }
        };
        // Any line, a `PROBE` heartbeat included, is IG answering.
        params.link.stream_answered();
        let line = line.trim();
        if line.is_empty() || line == "PROBE" {
            continue; // keepalive
        }
        if line.starts_with("LOOP") || line.starts_with("END") {
            session.ended(line);
            return Ok(()); // server asked us to rebind / ended — reconnect
        }
        for event in events_for_line(line, deal_refs, vike_model::now_ms()) {
            if !params.fence.deliver(event) {
                return Err(RunErr::CoreGone);
            }
        }
    }
    session.ended("the stream's body ended");
    Ok(())
}

/// The events one line of the TRADE stream implies for an order of ours: none for a line that is not an update of
/// our subscription, whose `OPU` is absent, not JSON or not a position OPENING, or whose `dealReference` no submit
/// recorded and no restart's re-teaching taught ([`DealRefs::record_route`]): not one of ours, or the submit has not
/// recorded the ref yet.
///
/// ⚠ **Only the `OPU` field (schema order `CONFIRMS OPU WOU`, field 1), and only `status` `OPEN`.** Captured on the
/// demo gateway 2026-10-30 (`event_mapper.rs`'s `parse_update_line` has the three shapes): (1) a position CLOSE
/// sends an `OPU` with `status` `DELETED` carrying the ENTRY order's `dealReference`; read as a deal confirm that
/// is `OrderCanceled` for an order that FILLED (the "cancel after the fill" of the 2026-10-28 round trips, on
/// both order client ids). It is not an order cancel, so it names no event. (2) A `CONFIRMS` with `status` `OPEN` is the
/// confirm of a working order's CREATION as well as of a position's open (both: `dealStatus` `ACCEPTED`,
/// `affectedDeals` `OPENED`, a `level` and a `size`), so decoding `CONFIRMS` would book a FILL for a limit order
/// that merely rests; a deal this process sent is answered by the `/confirms` read of its own submit
/// (`execution/mod.rs`'s `map_confirm`, `lifecycle.rs`'s `map_cancel_confirm`), so the stream's copy adds nothing but
/// that hazard. (3) A `WOU` `DELETED` is a working order's cancel OR its fill and says which only beside the
/// `OPU`. What is left is the position OPENED, which is how a working order's fill, or a market deal whose
/// answer was lost, reaches the core. NOT measured: how a working order's fill reads here (a buy-stop entry
/// rested from 19:50 UTC until the FX close and never triggered); it is the `OPU` as before this change, which
/// decoded the same field by accident.
///
/// Events for an order whose submit is still in flight wait behind its own answer: the caller delivers them through
/// the exec start's fence ([`LightstreamerDial::fence`]).
///
/// `ts` is the `OPU`'s own `timestamp` (`"2026-10-30T19:47:04.546"`, UTC, measured against the box's clock on
/// 2026-10-30), else `now`, the receipt time: never 0 (the lane once read a confirm's `date` with `as_i64`, which a
/// string never is, and every streamed event was stamped 1970, seen in the hist store's `exec_fill` rows).
///
/// ⚠ **The one `CONFIRMS` this lane reads is the `DELETED` of a cancel whose own `/confirms` has not answered yet**
/// (`crate::execution::lifecycle::cancel_working_order` teaches its reference, [`DealRefs::record_stream_only`], as soon as IG
/// answers the DELETE, BEFORE its ladder runs: IG streams that confirm within moments, the ladder takes seconds): only that
/// reference, only `dealStatus` `ACCEPTED`, so hazard (2) above cannot reach it. Whichever of the two answers first ends
/// the order, once ([`DealRefs::take_stream_only`]). A refusal of that cancel is no end of the order, so it is passed over.
fn events_for_line(
    line: &str,
    deal_refs: &OrderVenueIdRegistryDealReference,
    now: i64,
) -> Vec<Event> {
    let Some(update) = parse_update_line(line) else { return Vec::new() };
    if update.sub_id != SUB_ID {
        return Vec::new();
    }
    if let Some(events) = cancel_confirmed_by_stream(&update, deal_refs, now) {
        return events;
    }
    let Some(Some(opu)) = update.fields.get(1) else { return Vec::new() };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(opu) else { return Vec::new() };
    if v.get("status").and_then(|s| s.as_str()) != Some("OPEN") {
        return Vec::new();
    }
    let deal_ref = v.get("dealReference").and_then(|d| d.as_str()).unwrap_or_default();
    let Some(order_client_id) = deal_refs.lock().unwrap().order_client_id_for(deal_ref) else {
        return Vec::new();
    };
    decode_trade_confirm(&v, &order_client_id, position_ts(&v, now))
}

/// The `OrderCanceled` the `CONFIRMS` field of `update` proves ([`events_for_line`]'s last paragraph), `None` for a line
/// that is not the deletion of a working order whose cancel this process has not confirmed yet.
fn cancel_confirmed_by_stream(
    update: &crate::execution::event_mapper::LightstreamerUpdateTrade,
    deal_refs: &OrderVenueIdRegistryDealReference,
    now: i64,
) -> Option<Vec<Event>> {
    let confirm: serde_json::Value =
        serde_json::from_str(update.fields.first()?.as_deref()?).ok()?;
    let text = |k: &str| confirm.get(k).and_then(|v| v.as_str());
    if text("dealStatus") != Some("ACCEPTED") || text("status") != Some("DELETED") {
        return None;
    }
    let mut refs = deal_refs.lock().unwrap();
    let order_client_id = refs.take_stream_only(text("dealReference")?)?;
    tracing::info!(
        target: "vike_ig::stream", %order_client_id,
        "the trade stream confirms a working order's cancel its own /confirms did not answer"
    );
    Some(decode_trade_confirm(&confirm, &order_client_id, now))
}

/// The `OPU`'s `timestamp` in epoch ms (UTC, milliseconds kept: `.5` is 500), else `now`.
fn position_ts(opu: &serde_json::Value, now: i64) -> i64 {
    let Some(text) = opu.get("timestamp").and_then(|t| t.as_str()) else { return now };
    let seconds = parse_ig_time_utc(text);
    if seconds == 0 {
        return now;
    }
    let fraction: String = match text.as_bytes().get(19) {
        Some(b'.') => text[20..].chars().take_while(char::is_ascii_digit).take(3).collect(),
        _ => String::new(),
    };
    seconds + format!("{fraction:0<3}").parse::<i64>().unwrap_or(0)
}

/// The `create_session.txt` form body for the TRADE stream.
///
/// ⚠ **Extracted so the WIRE can be pinned.** Inline in [`run_once`] these bytes were reachable
/// only through a live `ureq` POST, so nothing in the tree ever compared them to the specification
/// or to the market-data lane that speaks the same protocol — which is how a wrong `LS_cid` sat
/// here unnoticed. Mirrors `crate::feed::decode::lightstreamer::create_session_request`, which is the WS
/// transport's twin and cannot be reused directly (it emits the WS request-name framing, not an
/// HTTP form body).
fn create_session_body(account_id: &str, ls_password: &str) -> String {
    form(&[
        ("LS_adapter_set", "DEFAULT"),
        ("LS_user", account_id),
        ("LS_password", ls_password),
        // ⚠ LICENSING, not identity. See the test that pins this byte-for-byte.
        ("LS_cid", LS_CID),
    ])
}

/// The `control.txt` `LS_op=add` form body subscribing `TRADE:{account}` WITH SNAPSHOT
/// (audit A3 recovery on reconnect). Extracted for the same reason as
/// [`create_session_body`].
fn subscribe_body(session_id: &str, account_id: &str) -> String {
    form(&[
        ("LS_session", session_id),
        ("LS_reqId", "1"),
        ("LS_op", "add"),
        ("LS_subId", &SUB_ID.to_string()),
        ("LS_data_adapter", "DEFAULT"),
        ("LS_group", &format!("TRADE:{account_id}")),
        ("LS_schema", "CONFIRMS OPU WOU"),
        ("LS_mode", "DISTINCT"),
        ("LS_snapshot", "true"),
    ])
}

/// Whether a control line is Lightstreamer's `CONERR` — the server REFUSING the session request.
/// Split out (rather than inlined as a `starts_with`) so the credential-expiry classification the
/// reconnect loop keys off is one named, tested predicate.
fn is_conerr(line: &str) -> bool {
    line.trim_start().starts_with("CONERR")
}

/// Parse `CONOK,<sessionId>,<reqLimit>,<keepalive>,<controlLink>` → `(sessionId, controlLink)`.
fn parse_conok(line: &str) -> Option<(String, String)> {
    let rest = line.strip_prefix("CONOK,")?;
    let mut parts = rest.split(',');
    let session_id = parts.next()?.to_string();
    let _request_limit = parts.next()?;
    let _keepalive = parts.next()?;
    let control_link = parts.next().unwrap_or("").to_string();
    Some((session_id, control_link))
}

/// Replace the host of `endpoint` with `host` (a Lightstreamer controlLink), keeping the scheme.
fn rebase_host(endpoint: &str, host: &str) -> String {
    match endpoint.split_once("://") {
        Some((scheme, _)) => format!("{scheme}://{host}"),
        None => format!("https://{host}"),
    }
}

#[path = "user_data_tests.rs"]
#[cfg(test)]
mod user_data_tests;

#[path = "user_data_property_tests.rs"]
#[cfg(test)]
mod user_data_property_tests;
