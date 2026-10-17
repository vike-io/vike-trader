//! IG Lightstreamer trade-update stream (TLCP text mode) — IG's async fill/terminal lane, which
//! the sync `/confirms` call cannot carry (a resting working order fills LATER).
//!
//! Flow: `POST {endpoint}/lightstreamer/create_session.txt` opens a streaming text body; the first
//! `CONOK,<sessionId>,...,<controlLink>` line names the session, then `POST .../control.txt`
//! subscribes to `TRADE:{accountId}` over the `CONFIRMS OPU WOU` schema, and update lines stream
//! back. Each `CONFIRMS` field is decoded by the PURE [`crate::exec::event_mapper`] (dual-publish fill / cancel
//! / reject) and pushed to the core.
//!
//! Audit A3: the subscription requests a SNAPSHOT (`LS_snapshot=true`), so on a reconnect the
//! current trade state is re-delivered — a fill/terminal that landed during the drop is recovered,
//! and the core's `trade_id`/FSM dedup absorbs the overlap with anything already seen. Blocks — run
//! on a dedicated thread. NOTE: transport correctness (framing, rebind, keepalive) is verifiable
//! only against a live IG Lightstreamer server; the decode/parse layer is unit-tested.

use std::collections::{HashMap, VecDeque};
use std::io::{BufRead, BufReader};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use vike_bridge_core::user_data::sleep_unless_stopped;
use vike_exec::EventSender;
use vike_venues::venue_timings;

use crate::exec::event_mapper::{decode_trade_confirm, parse_update_line};
// ⚠ ONE handshake, ONE spelling. This lane used to carry its own `LS_PROTOCOL`, `form` and
// `urlencode` — behaviourally identical copies of `lightstreamer.rs`'s, whose own `form` doc had
// claimed since it was written that `crate::stream` shared it. That untrue claim IS the defect
// shape: two spellings of one wire drifted, and `LS_cid` is where the drift landed. Take a TLCP
// constant or `form` from `lightstreamer.rs` (whose percent-encoding is
// `vike_util::percent::percent_encode`, decision 0141); never re-spell one here.
use crate::lightstreamer::{LS_CID, LS_PROTOCOL, form};
use crate::rest::IgSession;

mod retry;

#[cfg(doc)]
use retry::{DOWN_HEARTBEAT, MAX_REFUSED_HANDSHAKES};
use retry::{DialOutcome, RetryState, Say};

/// What the exec thread learned at submit about ONE order, for the lanes that run afterwards.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Pending {
    /// The `dealReference` IG answered the submit POST with — the ONLY key `GET /confirms/{ref}`
    /// accepts, and therefore the only way to ask IG about this order again.
    pub deal_ref: String,
    /// Whether the submit was a MARKET deal (a position open, which fills inline) rather than a
    /// working order. `exec::map_confirm` needs it to decide whether a confirm carries a fill.
    pub market: bool,
}

/// The exec thread's submit-time knowledge, shared with everything that has to ask about an order
/// LATER. Both directions are kept because the two readers ask opposite questions and neither can
/// answer with a scan: the Lightstreamer driver holds a `dealReference` and needs the coid, while
/// [`vike_exec::ExecutionClient::confirm`] holds a coid and needs the `dealReference`.
///
/// ⚠ **This used to be the `by_ref` half alone**, which is why an IG order whose `/confirms` call
/// failed could never be re-asked about: nothing in the process could turn its coid back into the
/// one key IG's confirm endpoint takes.
#[derive(Debug, Default)]
pub(crate) struct DealRefs {
    by_ref: HashMap<String, String>,
    by_coid: HashMap<String, Pending>,
    /// Coids in the order they were first recorded, oldest first: what [`MAX_DEAL_REFS`] evicts by.
    order: VecDeque<String>,
}

/// How many orders' references are kept. The map lives as long as the process and every submit
/// adds to it, so it is bounded: past this the OLDEST order's entries go (a long-settled order;
/// the confirm watchdog and the stream only ask about recent ones).
const MAX_DEAL_REFS: usize = 4096;

impl DealRefs {
    /// Record a submitted order's `dealReference` in BOTH directions. Called by the exec thread the
    /// moment the submit POST returns, i.e. BEFORE the confirm is fetched — so a fill that lands in
    /// between still correlates.
    pub(crate) fn record(&mut self, coid: &str, deal_ref: &str, market: bool) {
        self.by_ref.insert(deal_ref.to_string(), coid.to_string());
        let pending = Pending { deal_ref: deal_ref.to_string(), market };
        if self.by_coid.insert(coid.to_string(), pending).is_none() {
            self.order.push_back(coid.to_string());
            while self.order.len() > MAX_DEAL_REFS {
                if let Some(oldest) = self.order.pop_front() {
                    self.forget(&oldest);
                }
            }
        }
    }

    /// Drop everything known about `coid`, in both directions (a REJECTED order: nothing will ever
    /// be asked about it again). Its slot in the eviction queue is left to age out.
    pub(crate) fn forget(&mut self, coid: &str) {
        self.by_coid.remove(coid);
        self.by_ref.retain(|_, c| c != coid);
    }

    /// Teach the STREAM alone that `deal_ref` belongs to `coid`, leaving `coid`'s submit-time
    /// [`Pending`] untouched. For a reference minted by a LATER request about the same order (a
    /// cancel): recording it through [`Self::record`] would overwrite the `coid -> dealReference`
    /// half the confirm watchdog re-asks IG with.
    pub(crate) fn record_stream_only(&mut self, coid: &str, deal_ref: &str) {
        self.by_ref.insert(deal_ref.to_string(), coid.to_string());
    }

    /// The order a streamed `CONFIRMS` belongs to, by the `dealReference` it echoes.
    pub(crate) fn coid_for(&self, deal_ref: &str) -> Option<String> {
        self.by_ref.get(deal_ref).cloned()
    }

    /// What a re-confirm of `coid` needs. `None` = this process never got a `dealReference` for
    /// that order, so there is nothing IG can be asked.
    pub(crate) fn pending_for(&self, coid: &str) -> Option<Pending> {
        self.by_coid.get(coid).cloned()
    }
}

/// Shared submit-time state (populated by exec at submit) — see [`DealRefs`].
pub(crate) type DealRefMap = Arc<Mutex<DealRefs>>;

/// `client_order_id <-> dealId`, the venue identity of a RESTING order, in both directions.
///
/// ⚠ **This used to be a plain local of the exec thread's `run`**, which is why it was filled by the
/// submit's `/confirms` reply and by nothing else: an order whose confirm 404'd, or one the
/// confirm WATCHDOG later resolved, never got a `dealId` here and was uncancellable for good. It
/// is shared (like [`DealRefs`], beside which it lives) so the watchdog's `confirm_order` fills it
/// too, and so the reconcile client — which sees working orders by `dealId` only — can name the
/// local order a live one belongs to instead of reporting it as externally placed.
///
/// In memory and never persisted: an order that predates a restart is not in it.
#[derive(Debug, Default)]
pub(crate) struct DealIds {
    by_coid: HashMap<String, String>,
    by_deal: HashMap<String, String>,
}

impl DealIds {
    /// A fresh, empty, shareable map.
    pub(crate) fn shared() -> DealIdMap {
        Arc::new(Mutex::new(Self::default()))
    }

    /// Record `coid`'s `dealId`. A coid re-recorded under a new id drops its stale reverse entry.
    pub(crate) fn record(&mut self, coid: &str, deal_id: &str) {
        let previous = self.by_coid.insert(coid.to_string(), deal_id.to_string());
        if let Some(stale) = previous.filter(|old| old != deal_id) {
            self.by_deal.remove(&stale);
        }
        self.by_deal.insert(deal_id.to_string(), coid.to_string());
    }

    /// The `dealId` of `coid`'s resting order — what cancel and amend address it by.
    pub(crate) fn deal_id_for(&self, coid: &str) -> Option<String> {
        self.by_coid.get(coid).cloned()
    }

    /// The local order a venue `dealId` belongs to.
    pub(crate) fn coid_for(&self, deal_id: &str) -> Option<String> {
        self.by_deal.get(deal_id).cloned()
    }

    /// Forget `coid` (its order is gone from IG), in both directions.
    pub(crate) fn forget(&mut self, coid: &str) {
        if let Some(deal_id) = self.by_coid.remove(coid) {
            self.by_deal.remove(&deal_id);
        }
    }
}

/// The map the exec thread, the confirm watchdog and the reconcile client share — see [`DealIds`].
pub(crate) type DealIdMap = Arc<Mutex<DealIds>>;

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
pub(crate) struct LsParams {
    pub session: Arc<IgSession>,
    pub endpoint: String,
    pub account_id: String,
}

/// The trade stream's agent: `timeout_recv_body` is the exec lane's `idle_threshold` in
/// `vike_venues::venue_timings::IG`, the bound on each read WAIT of the chunked body (the
/// stream's server `PROBE` lines are its keepalive). No dial or handshake bound is set: the row
/// declares none, and this builder must not invent one.
fn trade_stream_agent() -> ureq::Agent {
    ureq::Agent::config_builder()
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
/// forever (see [`DOWN_HEARTBEAT`] for the measurement). Every retry-and-log decision
/// now belongs to [`RetryState::plan`], which is pure and unit-tested; this loop only dials, renders
/// and sleeps.
pub(crate) fn stream_trades(
    params: LsParams,
    deal_refs: DealRefMap,
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
        let plan = retry.plan(outcome, started.elapsed());
        match plan.say {
            Say::Nothing => {}
            Say::Dropped => {
                tracing::warn!(target: "vike_ig::stream", error = %error, "LS trade stream dropped; reconnecting");
            }
            Say::DialFailed { attempt, next_in } => {
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
            Say::DialStillFailing { attempt, down_for, next_in } => {
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
            Say::RefusedRetryingLogin { attempt } => {
                tracing::warn!(
                    target: "vike_ig::stream",
                    error = %error,
                    attempt,
                    "LS trade stream REFUSED our credentials; re-authenticating the IG session \
                     before the next dial"
                );
            }
            Say::FillLaneDown { streak, next_in } => {
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
            Say::FillLaneStillDown { streak, down_for, next_in } => {
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
        sleep_unless_stopped(&stop, plan.sleep);
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
    params: &LsParams,
    deal_refs: &DealRefMap,
    events: &EventSender,
    stop: &Arc<AtomicBool>,
) -> Result<(), RunErr> {
    // 1. create_session — streaming response.
    let create_url =
        format!("{}/lightstreamer/create_session.txt?LS_protocol={LS_PROTOCOL}", params.endpoint);
    // Read the password PER DIAL (never cached in `params`): after a re-login this is the only way
    // the fresh CST/XST pair reaches the wire.
    let create_body = create_session_body(&params.account_id, &params.session.ls_password());
    let resp = agent
        .post(&create_url)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .send(create_body.as_bytes())
        .map_err(|e| RunErr::NeverEstablished(format!("create_session failed: {e}")))?;
    let reader = BufReader::new(resp.into_body().into_reader());
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
    agent
        .post(&control_url)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .send(sub_body.as_bytes())
        .map_err(|e| RunErr::NeverEstablished(format!("subscribe failed: {e}")))?;

    // 3. read update lines until stop/drop.
    for line in lines {
        if stop.load(Ordering::Relaxed) {
            return Ok(());
        }
        let line = line.map_err(|e| RunErr::Dropped(format!("stream read: {e}")))?;
        let line = line.trim();
        if line.is_empty() || line == "PROBE" {
            continue; // keepalive
        }
        if line.starts_with("LOOP") || line.starts_with("END") {
            return Ok(()); // server asked us to rebind / ended — reconnect
        }
        let Some(update) = parse_update_line(line) else {
            continue;
        };
        if update.sub_id != SUB_ID {
            continue;
        }
        // Schema order is `CONFIRMS OPU WOU`; the fill/terminal lane is CONFIRMS (field 0).
        let Some(Some(confirms)) = update.fields.first() else {
            continue;
        };
        let Ok(v) = serde_json::from_str::<serde_json::Value>(confirms) else {
            continue;
        };
        let deal_ref = v.get("dealReference").and_then(|d| d.as_str()).unwrap_or_default();
        let Some(coid) = deal_refs.lock().unwrap().coid_for(deal_ref) else {
            continue; // not one of ours (or submit hasn't recorded the ref yet)
        };
        let ts = v.get("date").and_then(|d| d.as_i64()).unwrap_or(0);
        for ev in decode_trade_confirm(&v, &coid, ts) {
            if events.blocking_send(ev).is_err() {
                return Err(RunErr::CoreGone);
            }
        }
    }
    Ok(())
}

/// The `create_session.txt` form body for the TRADE stream.
///
/// ⚠ **Extracted so the WIRE can be pinned.** Inline in [`run_once`] these bytes were reachable
/// only through a live `ureq` POST, so nothing in the tree ever compared them to the specification
/// or to the market-data lane that speaks the same protocol — which is how a wrong `LS_cid` sat
/// here unnoticed. Mirrors `crate::lightstreamer::create_session_request`, which is the WS
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
    let _req_limit = parts.next()?;
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

#[path = "stream_tests.rs"]
#[cfg(test)]
mod stream_tests;

#[path = "stream_props.rs"]
#[cfg(test)]
mod stream_props;
