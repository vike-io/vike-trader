//! OANDA transactions-stream decode — the delayed LIMIT/STOP fills the order-POST response can't
//! carry. `GET /v3/accounts/{acct}/transactions/stream` delivers one JSON transaction per line
//! (plus HEARTBEATs); `ORDER_FILL` → a fill, `ORDER_CANCEL` → a cancel. Pure decode; the persistent
//! chunked-GET transport is the driver's job (mirrors the WS decode seams).
//!
//! The fill references the order by `orderID` (venue id) and echoes the order's
//! `clientExtensions.id` when set (vike sets it = client_order_id), so both ids are available.

use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::ops::ControlFlow;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex, PoisonError};
use std::time::{Duration, Instant};

use vike_bridge_core::transport::ErrorKind;
use vike_bridge_core::user_data::sleep_unless_stopped;
use vike_exec::EventSender;
use vike_model::accounts::account_keys::AccountLabel;
use vike_model::events::{Event, FillEvent, OrderCanceled, OrderFilled, TradeId};
use vike_venues::venue_timings;

use crate::config::OandaConfig;
use crate::financing::{FinancingLane, read_home_currency};
use crate::recon_client::{AUTH_RETRY_PAUSES, read_past_401, require_array};
use crate::rest::{OandaApiError, OandaRest};
use crate::symbols::SubmittedSymbols;

const VENUE: &str = "oanda";

/// How many opens the venue may reject TERMINALLY, back to back, before this lane gives up, and the
/// count of failed opens of any kind past which [`StreamLink`] calls the stream down.
///
/// A bound rather than stop-on-first because a single rejection can be a momentary venue- or
/// edge-side answer, and a bound rather than none because a genuinely bad request answers
/// the same way forever: unbounded retry is what made this failure invisible. Counted since the
/// last SUCCESSFUL open, NOT since the last failure of any kind — a 500 interleaved between
/// rejections says nothing about them, and resetting on one would let `403, 500, 403, 500, …`
/// loop past the bound, which is the shape being closed.
///
/// ⚠ **A 401 never spends this budget's end.** OANDA answers it now and then to a token it accepts
/// (`crate::recon_client`'s module doc has the measurements, up to eight minutes in a row), so three
/// in a row, about two seconds apart, ended resting fills and venue-side cancels until a restart and
/// blamed the credentials for it. Past this many a 401 is asked again every [`REFUSED_REOPEN_EVERY`].
const MAX_TERMINAL_OPENS: u32 = 3;

/// The wait between opens once OANDA has refused [`MAX_TERMINAL_OPENS`] of them with a 401 in a row. A dead
/// token costs a few requests per wait for as long as the node runs; a fill during the gap arrives at most this
/// much later than the refusal's end, through the reopen's backfill.
const REFUSED_REOPEN_EVERY: Duration = Duration::from_secs(30);

/// The lane's waits on a 401; a loopback test passes them shortened.
#[derive(Debug, Clone, Copy)]
struct AuthWaits {
    /// Between opens, past the budget.
    reopen: Duration,
    /// Inside the backfill's read ([`read_past_401`]).
    read: [Duration; 2],
}

const AUTH_WAITS: AuthWaits = AuthWaits { reopen: REFUSED_REOPEN_EVERY, read: AUTH_RETRY_PAUSES };

/// What a stream that is not open costs while the lane keeps asking. True because of the lane's floor
/// ([`stream_transactions`]): the stream counts as reopened only once what it missed has been read.
const REOPENING_EFFECT: &str = "orders still go out; resting fills and venue-side cancels arrive when \
                                the stream reopens, which the bridge keeps trying by itself";

/// What it costs while the lane has no floor yet: a fill before the first answer lies below the floor.
const NO_FLOOR_EFFECT: &str = "orders still go out; the stream waits for OANDA to state the account's last \
                               transaction, which the bridge keeps asking by itself. Resting fills and venue-side \
                               cancels after that answer arrive when the stream opens; one before it reaches the \
                               engine only as a reconcile row, held under `quarantine`";

/// What the lane's end costs (a rejection that is not a 401).
const GAVE_UP_EFFECT: &str = "orders still go out; resting fills and venue-side cancels will NOT arrive \
                              for the rest of this session: fix the account's OANDA credentials and restart";

/// Why an account's transactions stream is not open ([`StreamLink`]). Words and counters, never a value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamDown {
    /// The last failed open, described.
    pub why: String,
    /// What this costs the account's orders.
    pub effect: &'static str,
    /// The opens that failed since the last one that worked.
    pub failed_opens: u32,
    /// The wait before the next open; zero when the lane has ended.
    pub retry_in: Duration,
}

/// **An account's transactions stream, as the lane says it is**: `None` while it is open or has failed fewer than
/// three opens in a row. A reading for the gatewayhub's status verb (`TradeLane::venue_link`, through
/// `crates/vike-gatewayhub/src/trade/adapters/oanda_link.rs`), which the tradehub's doctor shows: the lease stays up
/// through a stream that is down, so nothing else can say so. It changes no order's fate.
#[derive(Debug, Clone, Default)]
pub struct StreamLink(Arc<Mutex<Option<StreamDown>>>);

impl StreamLink {
    /// The stream's state now.
    #[must_use]
    pub fn down(&self) -> Option<StreamDown> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    /// What the lane writes; `pub` only so the gatewayhub's report tests can script it.
    #[doc(hidden)]
    pub fn set(&self, state: Option<StreamDown>) {
        *self.0.lock().unwrap_or_else(PoisonError::into_inner) = state;
    }
}

/// The [`StreamLink`] of `account`: the one cell its adapter's stream lane writes and its status report reads, because
/// a start hands its host a client and no slot for this (the twin of `vike_dukascopy::quotes::process_bus`).
/// shortcut: one entry per account label for the life of the process, bounded by the directory; a lane resets its
/// cell when it starts, so an entry of a stopped adapter reads as nothing once no adapter reports it.
#[must_use]
pub fn stream_link(account: &AccountLabel) -> StreamLink {
    static LINKS: LazyLock<Mutex<HashMap<String, StreamLink>>> = LazyLock::new(Mutex::default);
    let mut links = LINKS.lock().unwrap_or_else(PoisonError::into_inner);
    links.entry(account.to_string()).or_default().clone()
}

fn client_order_id(v: &serde_json::Value) -> String {
    v.pointer("/clientExtensions/id")
        .and_then(|c| c.as_str())
        .or_else(|| v.get("clientOrderID").and_then(|c| c.as_str()))
        .or_else(|| v.get("orderID").and_then(|c| c.as_str()))
        .unwrap_or_default()
        .to_string()
}

/// Whether an `ORDER_CANCEL` is the cancel half of an order REPLACE: OANDA stamps the replaced order's
/// cancel with `replacedByOrderID` and the reason `CLIENT_REQUEST_REPLACED`. Either marks it; a
/// plain cancel carries neither.
///
/// ⚠ A replace done OUTSIDE this process (the OANDA web platform, another API client) is skipped the
/// same way, so the platform's order stays resting under its old terms and venue id until reconcile
/// reads the venue's open orders; the alternative, terminating a possibly still-resting order,
/// is the worse error for a replacement that keeps the client id.
pub(crate) fn is_replace_cancel(v: &serde_json::Value) -> bool {
    v.get("replacedByOrderID").is_some()
        || v.get("reason").and_then(|r| r.as_str()) == Some("CLIENT_REQUEST_REPLACED")
}

/// Parse an `ORDER_FILL` transaction into its [`FillEvent`] (shared by the stream and the
/// order-POST response mapper so a delayed fill and an inline fill build identically).
///
/// `None` when the transaction carries no `id`. That field is OANDA's transaction id — unique and
/// monotonic per account, present on every v20 transaction — and it is this venue's ONLY per-fill
/// identity: `orderID` is shared by every fill of a multi-fill order, and `time` is not an identity
/// at all, so there is nothing here to synthesize a replay-stable id FROM. The honest answer is
/// therefore to refuse the frame rather than mint one.
///
/// ⚠ This used to read `s("id").unwrap_or_default()`, i.e. an absent `id` became `""` — and an empty
/// `trade_id` did not dedup badly, it SKIPPED `ExecutionEngine`'s dedup entirely, so every
/// reconnect replay of that fill re-booked its commission and realized PnL. This venue replays by
/// design (the A3 `sinceid` backfill re-decodes the exact missed transactions through this very
/// function), so the id being real is what makes that backfill safe.
pub(super) fn fill_from_transaction(
    v: &serde_json::Value,
    symbols: &SubmittedSymbols,
) -> Option<FillEvent> {
    let s = |k: &str| v.get(k).and_then(|x| x.as_str());
    let ts = s("time").and_then(|t| t.parse::<f64>().ok()).map_or(0, |secs| (secs * 1000.0) as i64);
    let coid = client_order_id(v);
    let units: f64 = s("units").and_then(|u| u.parse().ok()).unwrap_or(0.0);
    let price: f64 = s("price").and_then(|p| p.parse().ok()).unwrap_or(0.0);
    let commission = crate::financing::fill_charges(v);
    let trade_id = match TradeId::new(s("id").unwrap_or_default()) {
        Ok(t) => t,
        Err(_) => {
            tracing::warn!(
                venue = VENUE,
                %coid,
                "ORDER_FILL transaction carries no `id` — dropping the fill rather than folding an \
                 un-dedupable one (it would re-book on every reconnect backfill)"
            );
            return None;
        }
    };
    let symbol = symbols.label(&coid, s("instrument").unwrap_or_default());
    Some(FillEvent {
        trade_id,
        client_order_id: coid,
        venue: VENUE.to_string().into(),
        symbol: symbol.into(),
        side: if units >= 0.0 { 1 } else { -1 },
        last_qty: units.abs(),
        last_px: price,
        commission,
        commission_asset: String::new().into(),
        liquidity_side: String::new().into(),
        ts,
        mark_price: None,
        position_side: vike_model::events::PositionSide::Both,
    })
}

/// Decode one transaction-stream line into the vike events it implies. An `ORDER_FILL`
/// dual-publishes the bare [`Event::Fill`] (the Account folds it into position/PnL) AND the
/// [`Event::OrderFilled`] wrap (the order FSM applies it) — the same contract the crypto WS
/// mappers honor, so a live fill folds identically to a replayed one. `ORDER_CANCEL` → one
/// [`Event::OrderCanceled`], EXCEPT the cancel half of an order REPLACE (`replacedByOrderID`),
/// which ends nothing: the replacement keeps the client id. HEARTBEAT / echoes → empty.
///
/// A fill carries the symbol its order was SUBMITTED under (`symbols`, see [`crate::symbols`]), the
/// venue's own spelling when nobody recorded the coid.
pub fn decode_transaction_events(v: &serde_json::Value, symbols: &SubmittedSymbols) -> Vec<Event> {
    let s = |k: &str| v.get(k).and_then(|x| x.as_str());
    let ts = s("time").and_then(|t| t.parse::<f64>().ok()).map_or(0, |secs| (secs * 1000.0) as i64);
    match s("type") {
        Some("ORDER_FILL") => {
            // An id-less fill yields NO events at all — not a half-published pair. The bare Fill
            // and its wrap must stand or fall together (they are the Account's copy and the FSM's
            // copy of one execution), so a refused frame drops both.
            let Some(fill) = fill_from_transaction(v, symbols) else { return Vec::new() };
            let wrap = Event::OrderFilled(OrderFilled {
                client_order_id: fill.client_order_id.clone(),
                fill: fill.clone(),
                ts: fill.ts,
            });
            vec![Event::Fill(fill), wrap]
        }
        // The cancel half of an order REPLACE ends the REPLACED order, not the logical one: the
        // replacement carries the same client id (`crate::exec::build_replace_body`) and the
        // `OrderModified` from the replace reply is what the platform folds. Emitting it as an
        // `OrderCanceled` would terminate an order that is still resting.
        Some("ORDER_CANCEL") if is_replace_cancel(v) => Vec::new(),
        Some("ORDER_CANCEL") => vec![Event::OrderCanceled(OrderCanceled {
            client_order_id: client_order_id(v),
            reason: s("reason").unwrap_or_default().to_string().into(),
            ts,
        })],
        _ => Vec::new(), // HEARTBEAT, ORDER_CREATE echoes, etc.
    }
}

/// The transactions-stream agent. Hand-built for the same reason [`crate::market_feed`]'s is: a
/// chunked GET cannot live under `vike_bridge_core::http::blocking_agent`'s global request timeout.
///
/// ⚠ **`http_status_as_error(false)` is this lane's own decision, stated here beside it** — the
/// two chunked-GET lanes each state their agent policy rather than sharing code, and this one used
/// to differ: without it a 401/403 arrives as a transport ERROR indistinguishable from a dead
/// route, which is how a rejected open came to be retried forever with nothing logged. Non-2xx as
/// a RESPONSE is what lets [`open_failure_kind`] tell "the token is wrong" from "the network
/// blinked", and only the first of those is worth giving up on.
///
/// ⚠ **Three bounds, three different waits, all read from the exec lane of
/// `vike_venues::venue_timings::OANDA`** (the row carries each one's reason): `dial_timeout`
/// bounds the connect so a black-holed route can never hold the exec event lane for the OS's own
/// SYN ladder; `response_timeout` bounds the wait for the response HEAD, which is NOT the dial (the
/// two are both 10 s today and are separate facts); `idle_threshold` is `timeout_recv_body`, the
/// bound on each read wait of the chunked body (4 missed ~5 s heartbeats), the same mechanism the
/// quote lane's agent in `crate::market_feed` uses.
///
/// ⚠ **The head wait is `timeout_send_request`, NEVER `timeout_recv_response`.** In ureq 3.4.0 the
/// body-read deadline is the minimum of `now + recv_body` and `<time the response head arrived> +
/// recv_response` (`Timeout::RecvBody` lists `RecvResponse` as its preceding check), so a
/// `recv_response` of 10 s kills every successfully opened stream ~10 s after its head despite
/// heartbeats every ~5 s. `Timeout::RecvResponse` also checks `SendRequest` (the instant the
/// request left, plus `send_request`), which bounds the wait for the head and which the body check
/// never consults: the same bound, without capping the body.
///
/// `exec` is the exec lane of `vike_venues::venue_timings::OANDA` on every production path
/// ([`stream_transactions`]); a loopback test passes the row with its bounds shortened.
fn transactions_agent(exec: venue_timings::Lane) -> ureq::Agent {
    ureq::Agent::config_builder()
        .http_status_as_error(false)
        // No redirect: a 3xx on the credentialed open is a non-2xx response, never a second request.
        .max_redirects(0)
        .timeout_connect(Some(exec.dial_timeout()))
        .timeout_send_request(Some(exec.response_timeout()))
        .timeout_recv_body(Some(exec.idle_threshold()))
        .user_agent("vike-trader-rust")
        .build()
        .new_agent()
}

/// Classify one failed stream open into the SHARED taxonomy
/// (`vike_bridge_core::transport::ErrorKind`) rather than a vocabulary local to this venue:
/// `Some(status)` is the venue's own answer, `None` a pre-send transport failure (the `code == 0`
/// case that taxonomy already spells [`ErrorKind::Network`]).
///
/// The verdict this lane acts on is [`ErrorKind::is_terminal`] — "re-issuing as-is cannot help".
/// 401/403 → [`ErrorKind::Auth`] is terminal and bounded by [`MAX_TERMINAL_OPENS`]; 429/5xx and a
/// dead route are retryable and loop unchanged. An unmapped status falls to `Unknown`, which that
/// taxonomy makes terminal BY DESIGN — the safe posture is to stop and say so, never to spin.
fn open_failure_kind(status: Option<u16>) -> ErrorKind {
    match status {
        Some(s) => ErrorKind::from_http_status(s),
        None => ErrorKind::Network,
    }
}

/// Describe a failed stream open (either chunked-GET lane: this one and the pricing stream in
/// `crate::market_feed`) WITHOUT echoing the URL (it carries the account id). A non-2xx is surfaced BY
/// STATUS, a TIMEOUT by the bound that fired and the lane's configured value, and anything else stays the
/// generic transport line.
///
/// Why a timeout is its own sentence: on 2026-10-18 the OANDA practice stream hosts (behind Cloudflare)
/// answered about one open in three and left the rest without a response head until the wait ran out
/// (then an HTTP 520), and the journal said only "transport error on stream open", which hid the cause.
/// ureq 3.4.0 reports the head wait as `Timeout::SendRequest` (`timeout_send_request` is what both agents
/// set, and `RecvResponse` checks it), so both variants read as the head wait; `Connect` is the dial.
/// `lane` is the row the agent was built from, so the number named is the bound that actually applied.
pub(crate) fn describe_dial_error(e: ureq::Error, lane: venue_timings::Lane) -> String {
    match e {
        ureq::Error::StatusCode(401) => {
            "HTTP 401 on stream open (OANDA says this now and then to a good token; if it lasts: bad or \
             expired token / wrong account)"
                .to_string()
        }
        ureq::Error::StatusCode(code) => {
            format!("HTTP {code} on stream open (401/403: bad or expired token / wrong account)")
        }
        ureq::Error::Timeout(ureq::Timeout::Connect) => format!(
            "no TCP connection within {} s (the OANDA stream host did not accept)",
            lane.dial_timeout().as_secs_f64()
        ),
        ureq::Error::Timeout(ureq::Timeout::SendRequest | ureq::Timeout::RecvResponse) => format!(
            "no response head within {} s (the OANDA stream host did not answer)",
            lane.response_timeout().as_secs_f64()
        ),
        ureq::Error::Timeout(which) => format!("{which} timeout on stream open"),
        _ => "transport error on stream open".to_string(),
    }
}

/// Persistent transactions-stream driver: hold `GET /v3/accounts/{id}/transactions/stream`, decode
/// each line via [`decode_transaction_events`], and emit fills/cancels via `events` until `stop`.
/// Reconnects on disconnect, after the exec row's fixed pause. Blocks — run on a dedicated thread.
/// HEARTBEATs (~5s) keep the read loop ticking so `stop` is honoured; a silently dead connection is
/// bounded by `timeout_recv_body`.
///
/// **A REJECTED open is bounded and audible.** Every failed open is logged with its status and its
/// [`open_failure_kind`] classification; a retryable one (429/5xx/unreachable) backs off and loops
/// as before, while [`MAX_TERMINAL_OPENS`] consecutive terminal ones (a 403 or another status the taxonomy
/// makes terminal) log at `error!` and END the lane. This function previously retried a
/// rejection forever with no log at all: the exec thread's REST path kept working, so MARKET orders
/// filled normally while every delayed LIMIT/STOP fill and every venue-side cancel silently stopped
/// arriving — a failure no test in this crate can see (its stream coverage is offline decode) and
/// no operator could either.
///
/// **A 401 keeps the lane** ([`MAX_TERMINAL_OPENS`]'s note): past the budget it says one `error!`, asks again every
/// [`REFUSED_REOPEN_EVERY`] and says one line when an open works. Both that outage and any other run of failed opens
/// are stated on `link` ([`StreamLink`]), which the tradehub's doctor reads.
///
/// **What the gap costs, for any lost stream, this one included.** Orders still go out on the REST path, and a MARKET
/// order's fill rides its reply. A resting order stays `Accepted`, which the stuck-order ladder (pre-ack orders only)
/// does not touch; its fill or cancel waits for the reopen's backfill below, and reconcile's own `sinceid` read names
/// it meanwhile (held under `quarantine`, and the engine's `trade_id` dedup absorbs the second copy). Neither dead-man
/// hears this lane: it discloses no `StreamStatus` and the transport's silence is the market lanes'.
///
/// Audit A3: the stream carries only transactions from connect-time and never replays, so a
/// terminal (fill/cancel) that lands while no stream is open is lost unless it is read back. So EVERY open, the first
/// too, backfills `GET /transactions/sinceid?id={floor}` — the EXACT missed transactions — before its first line,
/// through the SAME decode path, and the core's `trade_id`/FSM dedup absorbs the overlap with what the stream or an
/// order POST also delivered.
///
/// **The floor is the lane's own**, a local of this thread, so nothing else can move it:
/// - **Seeded once, from `GET /summary`'s `lastTransactionID`, before the first open**, so that open's backfill reads
///   what lands between the two. A heartbeat has no `id`, so the lines alone leave a lane that adopted an order with
///   no floor at all. A seed that fails is a failed open and is asked again before the next one: no line is read
///   without a floor (the line's id would become it, past the gap behind it). A fill before the first seed that
///   answers lies below the floor like a fill made while the node was down, and reconcile names it ([`NO_FLOOR_EFFECT`]).
/// - **Moved only by this lane's lines and by a backfill that answered.** Never by an order POST, whose
///   `lastTransactionID` can lie past a fill nobody read. `last_seen` is the EXEC path's watermark, the floor of an
///   ambiguous submit's scan (`crate::exec`'s `ensure_watermark`): the lane still advances it and never reads it.
/// - **A failed backfill is a failed open**: the response is dropped unread, the lane pauses and reopens, and the link
///   says the stream is down. The cost, on purpose: during a REST outage the live lines wait too, but no fill is skipped.
///
/// A REST read never ends the lane, whatever it answers: only a rejected stream open does, so a 401 keeps the slow
/// clock and anything else retries on the fixed pause.
pub(crate) fn stream_transactions(
    config: OandaConfig,
    events: EventSender,
    stop: Arc<AtomicBool>,
    last_seen: Arc<AtomicU64>,
    symbols: SubmittedSymbols,
    link: StreamLink,
) {
    stream_transactions_on(
        venue_timings::OANDA.exec(),
        AUTH_WAITS,
        config,
        events,
        stop,
        last_seen,
        symbols,
        link,
    );
}

/// [`stream_transactions`] on the bounds of `exec` — the exec lane of `vike_venues::venue_timings::OANDA` from
/// [`stream_transactions`], its only production caller — and on `waits`; a loopback test passes them with the
/// bounds it waits out shortened, so it does not sit through the production seconds.
fn stream_transactions_on(
    exec: venue_timings::Lane,
    waits: AuthWaits,
    config: OandaConfig,
    events: EventSender,
    stop: Arc<AtomicBool>,
    last_seen: Arc<AtomicU64>,
    symbols: SubmittedSymbols,
    link: StreamLink,
) {
    let agent = transactions_agent(exec);
    let rest = OandaRest::new(config.api_token.clone()); // bounded (30s global) backfill transport
    let url =
        format!("{}/v3/accounts/{}/transactions/stream", config.stream_base, config.account_id);
    let bearer = format!("Bearer {}", config.api_token);
    let mut floor = None;
    let mut failed_opens = 0_u32;
    let mut terminal_opens = 0_u32;
    let mut refused_opens = 0_u32;
    let mut refusal_said = false;
    // The cash a fill does not carry (`crate::financing`): booked here and only here, once per transaction.
    let mut financing = FinancingLane::default();
    link.set(None);

    while !stop.load(Ordering::Relaxed) {
        // Before the open: its backfill then reads what lands between this answer and the stream.
        let known = floor.map_or_else(|| read_floor(&rest, &config, waits), Ok);
        floor = known.as_ref().ok().copied();
        let opened = match agent
            .get(&url)
            .header("Authorization", &bearer)
            .header("Accept-Datetime-Format", "UNIX")
            .call()
        {
            // `http_status_as_error(false)`: a rejection arrives HERE, as a response carrying its
            // status, so it is classified rather than blurred into the transport arm below.
            Ok(r) => {
                let status = r.status().as_u16();
                if (200..300).contains(&status) {
                    Ok(r)
                } else {
                    Err((Some(status), describe_dial_error(ureq::Error::StatusCode(status), exec)))
                }
            }
            // pre-send / connect / TLS / a head that never came: no status exists
            Err(e) => Err((None, describe_dial_error(e, exec))),
        };
        // A3 resync: the stream is already open (so nothing after it is missed); it counts as open once what lies
        // behind it is read, and the backfill + stream overlap is deduped by the core.
        let opened = opened.and_then(|resp| {
            let since = known?;
            let gap = resync_gap(
                &rest,
                &config,
                &events,
                since,
                &last_seen,
                &symbols,
                &mut financing,
                waits,
            )?;
            Ok((resp, gap))
        });
        let resp = match opened {
            Ok((_, ControlFlow::Break(()))) => return, // core gone during backfill
            Ok((r, ControlFlow::Continue(read_to))) => {
                floor = floor.max(read_to);
                // The token works; any earlier rejection is not standing.
                (failed_opens, terminal_opens, refused_opens) = (0, 0, 0);
                link.set(None);
                if std::mem::take(&mut refusal_said) {
                    tracing::warn!(
                        target: "vike_oanda::stream",
                        venue = VENUE,
                        "transactions stream accepted again after the 401s; what the gap held has been read back"
                    );
                }
                r
            }
            Err((status, cause)) => {
                failed_opens += 1;
                let kind = open_failure_kind(status);
                let mut pause = exec.reconnect().initial;
                let refused = status == Some(401);
                refused_opens += u32::from(refused);
                if refused && refused_opens >= MAX_TERMINAL_OPENS {
                    pause = waits.reopen;
                    if !std::mem::replace(&mut refusal_said, true) {
                        tracing::error!(
                            target: "vike_oanda::stream",
                            venue = VENUE,
                            attempts = refused_opens,
                            retry_s = pause.as_secs(),
                            "transactions stream (its open, or a read that open needs) still answered 401. OANDA says \
                             this now and then to a good \
                             token, so this lane keeps asking on the slow clock and says one line when it is back. \
                             Until then resting LIMIT/STOP fills and venue-side cancels wait (the reopen reads them \
                             back); orders still go out. If this lasts for hours, OANDA refuses this account's \
                             API_KEY or ACCOUNT_ID."
                        );
                    }
                } else if refused {
                    tracing::warn!(
                        target: "vike_oanda::stream",
                        venue = VENUE,
                        status,
                        kind = %kind,
                        error = %cause,
                        attempts = refused_opens,
                        "transactions stream answered 401; asking again"
                    );
                } else if !kind.is_terminal() {
                    // Throttle, venue 5xx, unreachable host: retrying as-is is the right answer,
                    // so this arm keeps looping exactly as before — it is now merely AUDIBLE.
                    tracing::warn!(
                        target: "vike_oanda::stream",
                        venue = VENUE,
                        status,
                        kind = %kind,
                        error = %cause,
                        "transactions-stream open failed; backing off and retrying"
                    );
                } else {
                    terminal_opens += 1;
                    if terminal_opens >= MAX_TERMINAL_OPENS {
                        // ⚠ The asymmetry is the reason this line is an `error!` and spells the
                        // consequence out: MARKET orders keep working (their fill rides the order
                        // POST reply), so the venue looks healthy from the exec thread while every
                        // resting LIMIT/STOP fill and every venue-side cancel stops arriving and
                        // the orders sit accepted forever.
                        tracing::error!(
                            target: "vike_oanda::stream",
                            venue = VENUE,
                            status,
                            kind = %kind,
                            error = %cause,
                            attempts = terminal_opens,
                            "transactions-stream open REJECTED and not retryable (403: bad or \
                             expired token / wrong account) — giving up on this lane. Delayed \
                             LIMIT/STOP fills and venue-side cancels will NOT arrive for the rest \
                             of this session; MARKET fills are unaffected (they ride the order POST \
                             reply). Fix the account's OANDA credentials (API_KEY / ACCOUNT_ID) \
                             and restart."
                        );
                        link.set(Some(StreamDown {
                            why: cause,
                            effect: GAVE_UP_EFFECT,
                            failed_opens,
                            retry_in: Duration::ZERO,
                        }));
                        return;
                    }
                    tracing::warn!(
                        target: "vike_oanda::stream",
                        venue = VENUE,
                        status,
                        kind = %kind,
                        error = %cause,
                        attempts = terminal_opens,
                        max_attempts = MAX_TERMINAL_OPENS,
                        "transactions-stream open rejected; retrying within the bounded budget"
                    );
                }
                if failed_opens >= MAX_TERMINAL_OPENS {
                    link.set(Some(StreamDown {
                        why: cause,
                        effect: if floor.is_some() { REOPENING_EFFECT } else { NO_FLOOR_EFFECT },
                        failed_opens,
                        retry_in: pause,
                    }));
                }
                // The exec lane's fixed wait between opens (`reconnect.initial`; the row is fixed), or the
                // slow clock of a 401 past the budget.
                sleep_unless_stopped(&stop, pause);
                continue;
            }
        };
        let reader = BufReader::new(resp.into_body().into_reader());
        let opened_at = Instant::now();
        let mut read_error = None;
        for line in reader.lines() {
            if stop.load(Ordering::Relaxed) {
                return;
            }
            let line = match line {
                Ok(l) => l,
                Err(e) => {
                    read_error = Some(e); // recv timeout / disconnect → reconnect
                    break;
                }
            };
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(line) {
                crate::exec::note_max_id(&last_seen, &v, "id"); // the exec path's watermark
                let mut evs = decode_transaction_events(&v, &symbols);
                evs.extend(financing.events(&v, &mut || read_home_currency(&rest, &config)));
                if publish(&events, &symbols, evs).is_break() {
                    return; // core gone
                }
                let id = v.get("id").and_then(|id| id.as_str()).and_then(|id| id.parse().ok());
                floor = floor.max(id);
            }
        }
        // The read loop ended with `stop` clear: the body ended (`read_error` is `None`) or a read
        // failed. Say which, and how long the stream lived, so a stream cut short is audible; then
        // wait the row's fixed stop-aware pause, so a venue that closes on accept is never
        // redialled hot.
        tracing::info!(
            target: "vike_oanda::stream",
            venue = VENUE,
            lived = ?opened_at.elapsed(),
            error = ?read_error,
            "transactions-stream read loop ended; reopening after the fixed pause"
        );
        sleep_unless_stopped(&stop, exec.reconnect().initial);
    }
}

/// Publish what this lane decoded, in order. An event of an order whose submit the exec thread has not
/// answered yet is kept back ([`SubmittedSymbols::park`]) and goes out behind the exec thread's own
/// events for it: OANDA writes a market order's `ORDER_FILL` before the POST's reply is read, and a
/// fill that reaches the engine ahead of the `OrderAccepted` meets a `Submitted` order, which the FSM
/// refuses (`dropped_terminal_on_live`). `Break` when the core is gone.
fn publish(events: &EventSender, symbols: &SubmittedSymbols, evs: Vec<Event>) -> ControlFlow<()> {
    for ev in evs {
        // `settle` runs per event, as the event goes out: a parked fill must not drop its order's
        // symbol before the exec thread's own copy of the fill has been labelled with it.
        let Some(ev) = symbols.park(ev) else { continue };
        symbols.settle(std::slice::from_ref(&ev));
        if events.blocking_send(ev).is_err() {
            return ControlFlow::Break(());
        }
    }
    ControlFlow::Continue(())
}

/// A failed open: the status to judge it by, and its description.
type OpenFailure = (Option<u16>, String);

/// A failed REST read of an open, in the open's terms. Only a 401 keeps its status, for the slow clock: everything
/// else reads as retryable, because only a rejected stream OPEN may end the lane ([`stream_transactions`]).
fn rest_failure(what: &str, e: &OandaApiError) -> OpenFailure {
    ((e.status == 401).then_some(401), format!("{what}: {e}"))
}

/// The lane's floor: the account's last transaction now ([`stream_transactions`]). A 401 is asked again
/// ([`read_past_401`]), and a body that states no id is a failed read.
fn read_floor(
    rest: &OandaRest,
    config: &OandaConfig,
    waits: AuthWaits,
) -> Result<u64, OpenFailure> {
    const WHAT: &str = "the read of the stream's floor";
    let path = format!("/v3/accounts/{}/summary", config.account_id);
    let v = read_past_401(rest, &config.rest_base, &path, "", WHAT, &waits.read)
        .map_err(|e| rest_failure(WHAT, &e))?;
    v.pointer("/account/lastTransactionID")
        .or_else(|| v.get("lastTransactionID"))
        .and_then(|id| id.as_str())
        .and_then(|id| id.parse().ok())
        .ok_or_else(|| (None, format!("{WHAT}: OANDA answered without a lastTransactionID")))
        .inspect(|floor| {
            tracing::info!(
                target: "vike_oanda::stream",
                venue = VENUE,
                floor,
                "transactions stream: every open reads back from this floor"
            );
        })
}

/// Backfill and replay what a stream open missed: `GET /transactions/sinceid?id={since}` (exclusive of `since`),
/// mapped through the live decode path. `Continue` carries the id the body reads up to, which the caller's floor moves
/// to; `Break` means the core went away mid-replay (caller stops). A 401 is asked again ([`read_past_401`]: the refusal
/// the stream just came out of can outlast its open), and any failed read is a failed open, a 200 with no
/// `transactions` array included: its `lastTransactionID` would move the floor past what it did not state.
fn resync_gap(
    rest: &OandaRest,
    config: &OandaConfig,
    events: &EventSender,
    since: u64,
    last_seen: &Arc<AtomicU64>,
    symbols: &SubmittedSymbols,
    financing: &mut FinancingLane,
    waits: AuthWaits,
) -> Result<ControlFlow<(), Option<u64>>, OpenFailure> {
    const WHAT: &str = "the read of what the stream missed";
    let path = format!("/v3/accounts/{}/transactions/sinceid", config.account_id);
    let query = format!("id={since}");
    let resp = read_past_401(rest, &config.rest_base, &path, &query, WHAT, &waits.read)
        .map_err(|e| rest_failure(WHAT, &e))?;
    require_array(&resp, "transactions", WHAT).map_err(|why| (None, why))?;
    let read_to = crate::history::max_transaction_id(&resp);
    if let Some(max) = read_to {
        last_seen.fetch_max(max, Ordering::Relaxed);
    }
    let mut evs = crate::history::map_transactions_since(&resp, symbols);
    // The gap's financing goes through the SAME lane as the live line, so a rollover the reopened
    // stream also delivered is booked once.
    for tx in resp.get("transactions").and_then(|t| t.as_array()).into_iter().flatten() {
        evs.extend(financing.events(tx, &mut || read_home_currency(rest, config)));
    }
    if publish(events, symbols, evs).is_break() {
        return Ok(ControlFlow::Break(()));
    }
    Ok(ControlFlow::Continue(read_to))
}

#[path = "stream_tests.rs"]
#[cfg(test)]
mod stream_tests;
