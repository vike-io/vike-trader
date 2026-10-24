//! FXCM `ReconClient` (ReconFactory seam) — the venue-facing report seam
//! (`vike_exec::recon::ReconClient`) over the ForexConnect O2G table snapshots, mapping them into the
//! normalized reports `recon::diff` reconciles against local state.
//!
//! ## What ForexConnect surfaces (and how it maps)
//! Unlike the JSON-REST venues (oanda/ig), FXCM has no wire payload: the source rows are O2G table
//! rows read over the native SDK. The [`sys`](crate::sys) shim snapshots two tables into JSON arrays
//! (`fc_orders` / `fc_trades`, read-only, each a FRESH refresh-table request for the tradable
//! account: the login-time snapshot never holds Orders or Trades), and `sys::FxcmSession::account`
//! (its balance refreshed the same way) gives balance. A read that fails is an `Err`, never an empty
//! array: an empty Trades snapshot is read as a FLAT position, so "empty" must mean the venue said
//! so. The mapping:
//! - orders — the **Orders** table (resting entry/limit/stop working orders) → [`OrderStatusReport`].
//!   A working order is unfilled by definition, so `filled_qty`/`avg_px` are `0` and the status
//!   normalizes to `ACCEPTED` unless the row is terminal (the same "pending only" shape ig/bybit use).
//! - positions — the **Trades** table (open positions), net-folded per instrument into ONE signed
//!   [`PositionStatusReport`] (`PositionSide::Both`, sign in `qty`), `avg_px` the size-weighted mean
//!   of the open rates. No matched row → a synthesized flat row (load-bearing, see below).
//! - fills — the SAME **Trades** table, one [`FillReport`] per open trade. Each row's `trade_id` is
//!   the id the async fill lane ([`crate::event_mapper`], `Trades(Insert)`) already reports, so the
//!   reconcile engine's `trade_id` dedup lines up: a fill the live lane already booked is dropped, a
//!   gap fill the live lane missed surfaces with its real venue id. (Closed/historical trades are
//!   deliberately NOT read — that would be a different id space and risk double-booking.)
//! - balance — the tradable account's `balance` from the existing `fc_account` read (home-currency
//!   cash truth `recon::diff_balance` reconciles against).
//!
//! ## Per-symbol, instrument-filtered (the oanda/ig shape)
//! One [`FxcmReconClient`] per (account, canonical vike symbol e.g. `EURUSD`). Every table row carries
//! its FXCM instrument (`"EUR/USD"`), reverse-mapped by [`from_fxcm_instrument`](crate::event_mapper::from_fxcm_instrument)
//! and filtered to the engine symbol, stamping the CANONICAL symbol back onto each row so it matches
//! local state. When the symbol has no open trade the position parser synthesizes a FLAT zero row, so
//! `recon::diff` can still detect a stale LOCAL position the venue has since closed — the same
//! synthesize-flat contract oanda/ig/okx use.
//!
//! ## Admitted pairs (any-symbol, decision 0173)
//! The client is built on the MOUNTED symbol, and [`ReconClient::admit_symbol`] tells it every pair the account's
//! engine was admitted on demand (the price lane makes that reachable for this venue). The Orders and Trades
//! tables are whole-account snapshots, so ONE read per kind serves every symbol: each pass parses that body once
//! per symbol it was told (`parse_orders` / `parse_fills` / `parse_positions`, the mounted pair first), the flat-row
//! rule included, and a failed read fails the whole fetch (the restore planner reads an order a successful report
//! lacks as gone). Without this a lost-reply market order, or a restart with a position open, on an admitted pair
//! left the engine flat while the venue held the position. The list is [`AdmittedSymbols`] (append-only, the
//! engine's own cap) and `RECON_READS_ADMITTED` names this venue.
//!
//! ## `client_order_id` is always `None`
//! ForexConnect echoes no client-supplied id on the Orders/Trades rows (the exec side routes fills by
//! the venue order id it captured at placement, not by a round-tripped coid), so every report carries
//! `None` — the "externally-placed order" convention every venue parser uses for an absent client id.
//!
//! ## `since` is a no-op
//! The Orders/Trades tables are point-in-time snapshots of the CURRENT open set — there is no time
//! filter — so the trait's `since` is ignored (the same no-op oanda's `/orders` and ig's
//! `/workingorders` take). A closed order/trade has already left the table; its execution was booked
//! live and is not re-reconciled here.
//!
//! ## No live fee lane
//! FXCM's cost is spread + per-trade commission (already carried on the fill report), with no
//! per-account maker/taker rate endpoint — so `fetch_fee_rates` stays the trait default (`Ok(None)`)
//! and the caller keeps the static [`vike_venues::fee_schedule_for`] default (fail-soft).
//!
//! ## Threading (why a dedicated session thread, not a shared handle)
//! A [`FxcmSession`] owns a native ForexConnect handle and is deliberately neither `Send` nor `Sync`,
//! but `ReconClient: Send`. So — exactly like the exec half ([`crate::exec`]) — the session lives on
//! ONE dedicated thread that is logged in AND used there (the live-verified threading model), and
//! [`FxcmReconClient`] is a `Send` request/reply handle over an `mpsc` channel. No session handle
//! ever crosses a thread, so no `unsafe` is needed here (the crate's `unsafe` stays confined to the
//! FFI in [`sys`](crate::sys)).
//!
//! Every wire→report mapping is a PURE free function (`parse_*`) over the shim's JSON array string,
//! fixture-tested offline in `tests/fxcm_reconcile_parse.rs` (synthetic bodies — no SDK, no network);
//! that pure layer is the proven deliverable. The live extraction (the C++ table snapshots + the FFI
//! calls) is exercised only by the `#[ignore]`d, SDK+cred-gated `tests/fxcm_reconcile_smoke.rs`.

use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use serde_json::Value;

use vike_exec::recon::{AdmittedSymbols, ReconClient};
use vike_model::events::{LiquiditySide, PositionSide, TradeId};
use vike_model::{
    FillReport, MarginMode, OrderStatus, OrderStatusReport, OrderType, PositionStatusReport,
};

use crate::config::FxcmConfig;
use crate::event_mapper::from_fxcm_instrument;
use crate::home_conversion::PropertiesRead;
use crate::reads::{AccountReads, SessionReads};
use crate::redial::{Attempt, CREDENTIALS_REFUSED, Redial, pause_after, refuses_credentials};
use crate::sys::FxcmSession;

/// The venue key stamped on every report row.
pub const VENUE: &str = "fxcm";

// --- pure field helpers (the shim emits every scalar as a native JSON number/string) -------------

/// A present string field, `""` when absent/non-string (never panics).
fn s<'a>(v: &'a Value, key: &str) -> &'a str {
    v.get(key).and_then(|x| x.as_str()).unwrap_or("")
}

/// A numeric field, `0.0` when absent/non-numeric.
fn num(v: &Value, key: &str) -> f64 {
    v.get(key).and_then(Value::as_f64).unwrap_or(0.0)
}

/// ForexConnect `BuySell` code → signed side: `"S"` → `-1`, anything else (`"B"`) → `+1`. The inverse
/// of [`crate::sys::Side::code`], case-insensitive to match the exec fill lane's `side == Some("S")`.
fn side_of(buysell: &str) -> i32 {
    if buysell.eq_ignore_ascii_case("S") { -1 } else { 1 }
}

/// Does this row's FXCM instrument (`"EUR/USD"`) reverse-map to the engine canonical symbol
/// (`"EURUSD"`)? Reuses the exec lane's [`from_fxcm_instrument`] so the two agree on the mapping.
fn matches_symbol(instrument: &str, symbol: &str) -> bool {
    from_fxcm_instrument(instrument) == symbol
}

/// Map an FXCM order `type` code onto [`OrderType`]: `LE`/`L` → `Limit`, `SE`/`S`/`STE` → `Stop`.
/// Any other code (market orders do not rest in the Orders table) has no twin and stays
/// [`OrderType::Unrecognized`] lower-cased as-is, as every report spelled it before the type
/// existed. Pure — fixture-tested.
pub fn normalize_order_type(code: &str) -> OrderType {
    match code {
        "LE" | "L" => OrderType::Limit,
        "SE" | "S" | "STE" => OrderType::Stop,
        other => OrderType::Unrecognized(other.to_ascii_lowercase()),
    }
}

/// Map an FXCM order `status` code onto the FSM [`OrderStatus`] `recon::diff` reads. The Orders table
/// holds WORKING orders, so the working/pending codes — `W` waiting, `P` in process, `I` dealer
/// intervention, `Q` requoted, `U` pending calculated, `D` — are `Accepted`; the terminal codes that
/// can momentarily appear are mapped out: `F` → `Filled`, `C` → `Canceled`, `R` → `Rejected`. Pure —
/// fixture-tested.
///
/// A code outside that list is WARNED and read as `Accepted` (a resting order, mirroring oanda's
/// `normalize_order_state`): live, the conservative direction — `diff` raises no `MissingTerminal`
/// on it, adoption synthesizes no cumulative fill, and the restore planner keeps the order rather
/// than declaring it gone.
pub fn normalize_order_status(code: &str) -> OrderStatus {
    match code {
        "W" | "P" | "I" | "Q" | "U" | "D" => OrderStatus::Accepted,
        "F" => OrderStatus::Filled,
        "C" => OrderStatus::Canceled,
        "R" => OrderStatus::Rejected,
        other => {
            tracing::warn!(
                venue = VENUE,
                status = other,
                "unknown order status code — reporting the order as live (Accepted)"
            );
            OrderStatus::Accepted
        }
    }
}

// --- pure parsers (over the shim's JSON array strings — the fixture-tested units) ----------------

/// The Orders-table snapshot JSON → [`OrderStatusReport`]s, filtered to `symbol` and stamped with the
/// canonical symbol. `order_id` → `venue_order_id`; `client_order_id` is always `None` (see the
/// module doc); `buysell` → side; `type` normalized; `amount` → `qty`; a working order is unfilled so
/// `filled_qty`/`avg_px` are `0`; `status` normalized. `ts` is `0` (a table row carries no placement
/// time here). A malformed body is a hard `Err` (never a panic).
pub fn parse_orders(body: &str, symbol: &str) -> Result<Vec<OrderStatusReport>, String> {
    let v: Value = serde_json::from_str(body).map_err(|e| e.to_string())?;
    let rows = v.as_array().ok_or("expected a JSON array of order rows")?;
    Ok(rows
        .iter()
        .filter(|o| matches_symbol(s(o, "instrument"), symbol))
        .map(|o| OrderStatusReport {
            venue: VENUE.to_string(),
            symbol: symbol.to_string(),
            venue_order_id: s(o, "order_id").into(),
            client_order_id: None,
            side: side_of(s(o, "buysell")),
            order_type: normalize_order_type(s(o, "type")),
            qty: num(o, "amount"),
            filled_qty: 0.0,
            avg_px: 0.0,
            status: normalize_order_status(s(o, "status")),
            ts: 0,
        })
        .collect())
}

/// The Trades-table snapshot JSON → ONE net [`PositionStatusReport`] for `symbol`. FXCM is a
/// NET/one-way FX account, so every matched open trade is summed: `buysell` gives each leg's sign,
/// `amount` its size, and the NET is their signed sum. `avg_px` is the size-weighted mean of the
/// matched `open_rate`s. `position_side` is [`PositionSide::Both`] with the sign carried in `qty`
/// (load-bearing: the exec fill lane publishes `position_side == "BOTH"`, so local net positions are
/// keyed `(symbol, "BOTH")`, and `recon::diff` matches on that exact key). No matched row → a
/// synthesized FLAT zero row so a stale local position stays detectable.
pub fn parse_positions(body: &str, symbol: &str) -> Result<Vec<PositionStatusReport>, String> {
    let v: Value = serde_json::from_str(body).map_err(|e| e.to_string())?;
    let rows = v.as_array().ok_or("expected a JSON array of trade rows")?;

    let mut net_qty = 0.0_f64;
    let mut weighted_rate = 0.0_f64;
    let mut abs_size = 0.0_f64;
    let mut matched = false;

    for t in rows {
        if !matches_symbol(s(t, "instrument"), symbol) {
            continue;
        }
        matched = true;
        let size = num(t, "amount").abs();
        let rate = num(t, "open_rate");
        net_qty += if side_of(s(t, "buysell")) < 0 { -size } else { size };
        weighted_rate += size * rate;
        abs_size += size;
    }

    if !matched {
        return Ok(vec![flat_position(symbol)]);
    }
    let avg_px = if abs_size != 0.0 { weighted_rate / abs_size } else { 0.0 };
    Ok(vec![PositionStatusReport {
        venue: VENUE.to_string(),
        symbol: symbol.to_string(),
        position_side: PositionSide::Both,
        qty: net_qty,
        avg_px,
        ts: 0,
        margin_mode: MarginMode::default(),
        isolated_margin: None,
        delta: None,
    }])
}

/// The Trades-table snapshot JSON → [`FillReport`]s (one per open trade), filtered to `symbol`.
/// `trade_id` (the id the async fill lane reports, so the reconcile dedup aligns) → `trade_id`;
/// `order_id` → `venue_order_id`; `buysell` → side; `amount` → `last_qty`; `open_rate` → `last_px`;
/// `commission` → `commission`. `commission_asset` is empty (the row carries no currency — same as
/// the exec fill lane) and `liquidity_side` is `Unknown` (FX has no maker/taker flag). `ts` is `0`
/// (matching the exec lane's `"ts":0`); the `trade_id`, not ts, is the dedup key.
///
/// A row with no `trade_id` is **SKIPPED**, not reported with an empty id — the same decision
/// `crate::event_mapper` makes for a shim fill envelope, and for a sharper reason. This module's own
/// doc is that the Trades-table `trade_id` is the SAME id the async fill lane reports, so the
/// reconcile dedup lines up; an id-less row has nothing to line up with. It cannot match
/// `seen_trade_ids`, so it does not merely fail to dedup — it FABRICATES a `MissingFill` divergence,
/// and `MissingFill` is one of the two kinds the `hybrid` policy AUTO-APPLIES, which books the trade
/// again unattended. Nothing is synthesized: `order_id` is per-ORDER, and JForex-style FX rows share
/// an order across trades, so an order-keyed report would collapse distinct trades into one.
pub fn parse_fills(body: &str, symbol: &str) -> Result<Vec<FillReport>, String> {
    let v: Value = serde_json::from_str(body).map_err(|e| e.to_string())?;
    let rows = v.as_array().ok_or("expected a JSON array of trade rows")?;
    let out: Vec<FillReport> = rows
        .iter()
        .filter(|t| matches_symbol(s(t, "instrument"), symbol))
        .filter_map(|t| {
            let trade_id = TradeId::new(s(t, "trade_id")).ok()?;
            Some(FillReport {
                venue: VENUE.to_string(),
                symbol: symbol.to_string(),
                trade_id,
                venue_order_id: s(t, "order_id").into(),
                client_order_id: None,
                side: side_of(s(t, "buysell")),
                last_qty: num(t, "amount").abs(),
                last_px: num(t, "open_rate"),
                commission: num(t, "commission"),
                commission_asset: String::new(),
                liquidity_side: LiquiditySide::Unknown,
                ts: 0,
            })
        })
        .collect();
    let skipped =
        rows.iter().filter(|t| matches_symbol(s(t, "instrument"), symbol)).count() - out.len();
    if skipped > 0 {
        tracing::warn!(
            venue = VENUE,
            skipped,
            symbol,
            "reconcile Trades rows carry no `trade_id` — skipped; an id-less FillReport cannot \
             match `seen_trade_ids` and would manufacture a MissingFill divergence that `hybrid` \
             auto-applies (double-booking the trade)"
        );
    }
    Ok(out)
}

/// The synthetic flat row for a symbol with no open trade — load-bearing: `recon::diff` needs the row
/// PRESENT to detect a stale LOCAL position the venue has since closed.
fn flat_position(symbol: &str) -> PositionStatusReport {
    PositionStatusReport::flat(VENUE, symbol)
}

// --- the client (session on one dedicated thread; a Send request/reply handle) -------------------

/// One reconcile query for the session thread to serve.
enum Query {
    Orders,
    Trades,
    Balance,
    /// The login's system properties (the account's currency: decision 0183). Read-only, local.
    SystemProperties,
    /// The CURRENT Offers table (a conversion rate and a symbol's grid: decision 0183). Read-only.
    Offers,
}

/// The session thread's reply to a [`Query`].
enum Reply {
    /// A table snapshot JSON array string (Orders / Trades / Offers) or object (system properties).
    Json(String),
    /// The account balance (home-currency cash).
    Balance(f64),
    /// The installed shim exports no such read (it predates it): the entry point's name.
    Missing(&'static str),
}

/// A query plus the oneshot channel the session thread answers on.
struct Request {
    query: Query,
    reply: Sender<Result<Reply, String>>,
    /// When the asker stops waiting (`None` = it waits for the answer however long it takes: the reconcile
    /// worker's own passes). The session thread skips a request that is past it: nobody is listening, and
    /// serving it would spend a venue refresh on an answer that goes nowhere (a wedged-then-recovered thread
    /// would otherwise work through every ask that timed out meanwhile).
    expires: Option<Instant>,
}

/// Serve `rx` until its senders are gone, answering each request with `answer`; a request whose asker has
/// already given up is dropped unanswered. The body of the session thread, apart from the login, so a
/// scripted `answer` can drive it.
fn serve(rx: &Receiver<Request>, mut answer: impl FnMut(Query) -> Result<Reply, String>) {
    while let Ok(req) = rx.recv() {
        if req.expires.is_some_and(|by| Instant::now() >= by) {
            continue;
        }
        // The requester may have given up (dropped its receiver) — that is not our error.
        let _ = req.reply.send(answer(req.query));
    }
}

/// The dedicated session-thread body: log in ONCE, then serve reconcile queries until the handle
/// (and thus the request sender) drops. Login failure — bad creds OR no shim loaded (no
/// ForexConnect SDK; the "stub build" this named until #1720) — exits immediately; the
/// pending/next request then sees a disconnected reply channel, which is how
/// [`FxcmReconClient::connect`]'s probe returns `None`. The session drops at end of scope →
/// `fc_logout`. Retrying a login that failed is NOT this thread's business but the caller's
/// ([`crate::redial::Redial`] opens a fresh client per attempt); `retry` only says that this attempt is
/// one of those, so that a failure is told at `warn` and not again at `error`. A login the venue refused
/// over its CREDENTIALS ([`crate::redial::refuses_credentials`]) is told at `error` either way and
/// reported on `refused`, so that the caller does not retry it.
fn run_session(config: FxcmConfig, rx: Receiver<Request>, retry: bool, refused: Sender<()>) {
    let session =
        match FxcmSession::login(&config.user, &config.password, &config.url, &config.connection) {
            Ok(s) => s,
            // No session → the balance probe in `connect` fails → this venue-symbol's reconcile
            // stays unwired. ⚠ The `Err(_)` used to discard the error entirely, so a reconcile that
            // never ran and a reconcile that ran clean were the same thing in the log. The exec
            // half answers a dead session with a reject an operator can see; this half has no
            // command lane to answer on, so the log line IS the answer.
            Err(e) if refuses_credentials(&e.to_string()) => {
                tracing::error!(
                    target: "vike_fxcm",
                    venue = VENUE,
                    reason = %e,
                    "fxcm reconcile: the ForexConnect session did not open — {CREDENTIALS_REFUSED}"
                );
                // Sent BEFORE this thread ends: the probe's `Err` (the request queue dropping) can
                // only follow it.
                let _ = refused.send(());
                return;
            }
            Err(e) if retry => {
                tracing::warn!(
                    target: "vike_fxcm",
                    venue = VENUE,
                    reason = %e,
                    "fxcm reconcile: the ForexConnect session still did not open; the next attempt \
                     follows the growing pause"
                );
                return;
            }
            Err(e) => {
                tracing::error!(
                    target: "vike_fxcm",
                    venue = VENUE,
                    reason = %e,
                    "fxcm reconcile: the ForexConnect session did not open, so this venue-symbol \
                     reconciles NOTHING until a login succeeds (a start retries it with a growing \
                     pause; exec is unaffected)"
                );
                return;
            }
        };
    serve(&rx, |query| match query {
        Query::Orders => session.orders_json().map(Reply::Json).map_err(|e| e.to_string()),
        Query::Trades => session.trades_json().map(Reply::Json).map_err(|e| e.to_string()),
        Query::Balance => {
            session.account().map(|(_, bal)| Reply::Balance(bal)).map_err(|e| e.to_string())
        }
        Query::SystemProperties => session
            .system_properties_json()
            .map(|json| json.map_or(Reply::Missing("fc_system_properties"), Reply::Json))
            .map_err(|e| e.to_string()),
        Query::Offers => session
            .offers_json()
            .map(|json| json.map_or(Reply::Missing("fc_offers"), Reply::Json))
            .map_err(|e| e.to_string()),
    });
    // `session` drops here → fc_logout
}

/// One reconcile client for a started (account, canonical symbol). Holds a `Send` request sender to
/// the dedicated session thread (see the module doc's threading note) and that thread's
/// [`JoinHandle`](std::thread::JoinHandle); dropping it drops the sender, which ends the thread and
/// logs the session out, and then WAITS for that to finish. `ReconClient`'s methods take `&self` —
/// the sender is `Send + Sync`, so no lock is needed.
///
/// ## ⚠ Why the handle is kept, and why dropping the sender is not enough
///
/// The handle used to be discarded (`.spawn(…).ok()?`) and this type had no `Drop`, so the session
/// thread was DETACHED. `tests/fxcm_reconcile_smoke.rs` then SIGSEGV'd at teardown, 100%
/// reproducibly, after every assertion had passed — `cargo test` exiting 101 on a test that
/// succeeded — while `fxcm_live_smoke`, whose exec client joins its thread through
/// `ExecActor::stop`, exited 0 in the same session against the same account.
///
/// The backtrace (gdb on the CI box with the SDK staged) is what settles it, and it is a three-thread
/// picture:
///
/// * **the main thread** was inside `_dl_call_fini` → `__cxa_finalize` →
///   `httplib::CertificateTrustedStorageGuard::~CertificateTrustedStorageGuard()` — i.e. `main` had
///   returned and the process was running the SDK libraries' own static destructors;
/// * **this session thread** was still inside `fc_logout` → `StatusListener::wait`, waiting for the
///   logout's `Disconnected` callback;
/// * **the thread that actually faulted** was one of ForexConnect's OWN workers
///   (`gstool3::AThread::threadRunner`), inside `log4cplus::Logger::isEnabledFor` — still logging,
///   against a `log4cplus` whose static state the finalizers above were tearing down.
///
/// So the crash is not "a detached Rust thread races the exit"; it is **the SDK's internal worker
/// threads outliving `main` because the logout that stops them had not finished**. The exec half
/// never showed it because joining is exactly what orders the two: `fc_logout` completes, the
/// session releases, ForexConnect's workers are gone, and only then does `main` return. The join
/// below buys that same ordering here, which is why it is a fix rather than a way to make the smoke
/// look green — the hypothesis it replaces (a race with static destructors that a join would merely
/// hide) is disproven by the middle bullet: this thread had not finished logging out.
///
/// ⚠ The ORDER inside `drop` is load-bearing: the sender must go FIRST. `run_session` loops on
/// `rx.recv()` and only leaves it when every sender is gone, so joining while still holding one
/// deadlocks forever. That is why `tx` is an `Option` — there is no other way to drop a field early
/// — and the only place it becomes `None`.
pub struct FxcmReconClient {
    /// The request port of the session thread: the ONE holder of the sender (a query clones it for the
    /// instant of its `send` and drops the clone before it waits), shared with the registry
    /// ([`crate::reads`]) so the gatewayhub's facts lane can ask the same session. `Drop` closes it first.
    port: Arc<Port>,
    /// The session thread, kept so [`Drop`] can wait for `fc_logout` to finish. See the type doc.
    session: Option<std::thread::JoinHandle<()>>,
    /// Canonical vike symbol (e.g. `EURUSD`) every report row is filtered to and stamped with.
    symbol: String,
    /// The registry row this client put in [`crate::reads`] under its account's label, taken back out in
    /// `Drop` (only if it is still the registered one).
    registered: Option<(String, Arc<AccountReads>)>,
    /// The pairs the account's engine was admitted on demand and told this client (`admit_symbol`): every pass
    /// reads them beside `symbol` (the module doc's "Admitted pairs").
    admitted: AdmittedSymbols,
}

/// The session thread's request port: a sender that can be CLOSED from outside (so the registry's copy of
/// the port cannot keep the thread alive past the client's drop), and the [`SessionReads`] the facts lane
/// uses.
struct Port {
    tx: Mutex<Option<Sender<Request>>>,
}

impl Port {
    /// Send one query to the session thread and block for its reply. `Err` when the session thread is gone
    /// (login failed / no shim / dropped) or the port was closed. Waits however long the session takes: the
    /// login probe and the reconcile worker's passes, which have no deadline of their own. The gatewayhub's
    /// facts lane asks through [`Self::ask_within`].
    fn query(&self, query: Query) -> Result<Reply, String> {
        self.ask(query, None).and_then(|answer| answer)
    }

    /// [`Self::query`] that gives up after `within` of the session thread not answering (the wedged-session
    /// case: it is mid-FFI-call and answers nothing), with the request marked expired so that thread skips it
    /// if it ever comes back to it. The deadline covers the WHOLE call: the queue behind earlier requests, the
    /// venue read and the reply. The OUTER `Err` is "nobody answered" (timeout, thread gone, port closed); the
    /// inner `Result` is the session's own answer, a failed read included: the two are different facts (a
    /// venue that says it cannot state something is not a session that said nothing).
    fn ask_within(&self, query: Query, within: Duration) -> Result<Result<Reply, String>, String> {
        self.ask(query, Some(within))
    }

    fn ask(&self, query: Query, within: Option<Duration>) -> Result<Result<Reply, String>, String> {
        let by = within.map(|within| Instant::now() + within);
        // `None` only after `close`, i.e. inside `Drop`, where nothing can still be calling this through the
        // client; a registry caller racing the drop gets the same "gone" answer the send failure produces.
        let tx = self
            .tx
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
            .ok_or_else(|| "fxcm reconcile client is being dropped".to_string())?;
        // one-shot: at most 1 message (the session thread's reply to this one query).
        let (reply_tx, reply_rx) = mpsc::channel();
        tx.send(Request { query, reply: reply_tx, expires: by })
            .map_err(|_| "fxcm reconcile session thread is gone".to_string())?;
        // The clone goes BEFORE the wait: a queued request is still served after the last sender is gone,
        // and a clone held through the wait would keep the session thread's `recv` open and make `Drop`'s
        // join wait on a caller that is itself waiting on the thread.
        drop(tx);
        let heard = match by {
            None => reply_rx.recv().map_err(|_| RecvTimeoutError::Disconnected),
            Some(by) => reply_rx.recv_timeout(by.saturating_duration_since(Instant::now())),
        };
        match heard {
            Ok(answer) => Ok(answer),
            Err(RecvTimeoutError::Disconnected) => {
                Err("fxcm reconcile session dropped the reply".to_string())
            }
            Err(RecvTimeoutError::Timeout) => Err(format!(
                "fxcm reconcile session did not answer within {} s (it is busy or stuck in a call into the \
                 venue's library); nothing is assumed about the answer",
                within.map_or(0, |w| w.as_secs())
            )),
        }
    }

    /// Drop the sender: `run_session`'s `rx.recv()` then fails once the queue is empty and the thread leaves.
    fn close(&self) {
        *self.tx.lock().unwrap_or_else(PoisonError::into_inner) = None;
    }
}

/// The text of a read the installed shim cannot do, naming the file and the remedy.
fn missing_export(symbol: &str) -> String {
    format!(
        "the shim this process opened ({}) exports no `{symbol}`; reinstall the shim built from this \
         release with `just fxcm-package <root> <shim>`",
        crate::sdk_shim_path()
            .map_or_else(|| "none".to_string(), crate::loader::describe_shim_path)
    )
}

impl SessionReads for Port {
    fn system_properties(&self) -> PropertiesRead {
        match self.ask_within(Query::SystemProperties, crate::reads::SYSTEM_PROPERTIES_WAIT) {
            // Nobody answered: not the venue's silence, so nothing is assumed over it.
            Err(why) => PropertiesRead::Failed(why),
            Ok(Ok(Reply::Json(json))) => PropertiesRead::Json(json),
            Ok(Ok(Reply::Missing(symbol))) => PropertiesRead::Unreadable(missing_export(symbol)),
            Ok(Ok(Reply::Balance(_))) => PropertiesRead::Failed(
                "fxcm reconcile: expected the system properties, got a balance".to_string(),
            ),
            // The session answered that the venue's library could not give them: the API cannot say.
            Ok(Err(why)) => PropertiesRead::Unreadable(why),
        }
    }

    fn offers(&self) -> Result<String, String> {
        match self.ask_within(Query::Offers, crate::reads::OFFERS_WAIT)?? {
            Reply::Json(json) => Ok(json),
            Reply::Missing(symbol) => Err(missing_export(symbol)),
            Reply::Balance(_) => {
                Err("fxcm reconcile: expected the Offers table, got a balance".into())
            }
        }
    }
}

impl Drop for FxcmReconClient {
    fn drop(&mut self) {
        // 0. Leave the registry: the facts lane must not be handed a port whose session is going away.
        if let Some((account, entry)) = self.registered.take() {
            crate::reads::unregister(&account, &entry);
        }
        // 1. Drop the sender so `run_session`'s `rx.recv()` fails and the loop leaves — which is
        //    what drops `FxcmSession` and calls `fc_logout`. Joining before this deadlocks.
        self.port.close();
        // 2. THEN wait for that logout to finish, so ForexConnect's own worker threads are stopped
        //    before this process can reach `__cxa_finalize`. See the type doc for the backtrace.
        //    A panicked session thread is reported and otherwise ignored: the caller is already
        //    dropping this client, and there is nothing left to fail.
        if let Some(handle) = self.session.take()
            && handle.join().is_err()
        {
            tracing::warn!(
                venue = VENUE,
                "the fxcm reconcile session thread panicked; its ForexConnect logout may not \
                     have completed"
            );
        }
    }
}

impl FxcmReconClient {
    /// Spawn the dedicated session thread (which logs in) and validate the login with a balance
    /// probe. `None` when the thread failed to spawn, or the probe fails because login failed / no
    /// shim is loaded — reconcile stays unwired for this venue-symbol, exec unaffected (the same
    /// graceful degradation the exec half's own login uses as the live gate).
    pub fn connect(config: &FxcmConfig, symbol: &str) -> Option<FxcmReconClient> {
        Self::open(config, symbol, None, false, crate::reads::next_generation()).ok()
    }

    /// [`Self::connect`] for the account labelled `account` (`""` = the default account): the client also
    /// registers its session's read port in [`crate::reads`] under that label, so the gatewayhub's facts lane
    /// can ask this SAME session for the account's currency and the current Offers table (decision 0183)
    /// instead of opening another login. The registration ends when the client is dropped.
    pub fn connect_for(
        config: &FxcmConfig,
        symbol: &str,
        account: &str,
    ) -> Option<FxcmReconClient> {
        Self::open(config, symbol, Some(account), false, crate::reads::next_generation()).ok()
    }

    /// One attempt. `retry`: this is a later attempt of [`recon_client_redialing_for`], so a failed
    /// login is a `warn` line (the first failure was the `error`). `generation`: the start's
    /// ([`crate::reads::next_generation`]), under which the session registers: a start newer than this one
    /// that registered meanwhile keeps its place, and this client then simply stays unregistered.
    ///
    /// # Errors
    /// `Err(true)`: the venue REFUSED the login credentials ([`refuses_credentials`]), which no retry cures.
    /// `Err(false)`: any other failure to open (the thread, the login, the probe).
    fn open(
        config: &FxcmConfig,
        symbol: &str,
        account: Option<&str>,
        retry: bool,
        generation: u64,
    ) -> Result<FxcmReconClient, bool> {
        let (tx, rx) = mpsc::channel::<Request>();
        let (refused_tx, refused) = mpsc::channel::<()>();
        let cfg = config.clone();
        // ⚠ The `JoinHandle` is KEPT, not discarded. It used to be `.spawn(…).ok()?`, which left the
        // ForexConnect session thread detached and let the process reach its static destructors
        // while that thread was still inside `fc_logout` — see the SIGSEGV backtrace on
        // [`FxcmReconClient`].
        let session = std::thread::Builder::new()
            .name("fxcm-recon".into())
            .spawn(move || run_session(cfg, rx, retry, refused_tx))
            .map_err(|_| false)?;
        let mut client = FxcmReconClient {
            port: Arc::new(Port { tx: Mutex::new(Some(tx)) }),
            session: Some(session),
            symbol: symbol.to_string(),
            registered: None,
            admitted: AdmittedSymbols::default(),
        };
        // Probe: a balance query round-trips only if the session thread logged in. A disconnected
        // reply channel (thread already exited) → `Err` → `None`. Mirrors oanda's `/summary` probe.
        if client.query(Query::Balance).is_err() {
            // The thread reports a refused credential BEFORE it ends, which is what made the probe fail.
            return Err(refused.try_recv().is_ok());
        }
        // The session is up. Offer it to the facts lane, and decide the account's currency NOW so that
        // how it is known (the venue's word, or the ruling's assumed USD) is said once, at the start.
        if let Some(account) = account {
            let reads: Arc<dyn SessionReads> = Arc::clone(&client.port) as Arc<dyn SessionReads>;
            match crate::reads::register_generation(account, reads, generation) {
                Some(entry) => {
                    if let Err(why) = entry.account_currency() {
                        tracing::warn!(venue = VENUE, "{why}");
                    }
                    client.registered = Some((account.to_string(), entry));
                }
                None => tracing::warn!(
                    venue = VENUE,
                    "fxcm reconcile: the session opened after a NEWER start of this account had already \
                     registered its own, so this one stays unregistered (it reconciles, and is dropped \
                     with its start)"
                ),
            }
        }
        Ok(client)
    }

    /// Send one query to the session thread and block for its reply. `Err` when the session thread is
    /// gone (login failed / no shim / dropped) — the caller (a `fetch_*` method) surfaces it as a
    /// failed pass, never a panic.
    fn query(&self, query: Query) -> Result<Reply, String> {
        self.port.query(query)
    }

    /// A table-snapshot query, unwrapping the [`Reply::Json`] string (a balance reply here is a
    /// protocol bug, surfaced as `Err` rather than silently mis-parsed).
    fn table_json(&self, query: Query) -> Result<String, String> {
        match self.query(query)? {
            Reply::Json(s) => Ok(s),
            Reply::Balance(_) | Reply::Missing(_) => {
                Err("fxcm reconcile: expected a table snapshot, got something else".into())
            }
        }
    }
}

impl FxcmReconClient {
    /// The mounted symbol, then every admitted one: the pairs each pass parses a table body for.
    fn read_symbols(&self) -> Vec<String> {
        std::iter::once(self.symbol.clone()).chain(self.admitted.symbols()).collect()
    }

    /// `parse` over `body` once per symbol this client reads, the reports in that order. The body is read ONCE
    /// (a whole-account snapshot), so no per-symbol read can fail separately: one failure is the whole fetch's.
    fn parse_each<T>(
        &self,
        body: &str,
        parse: impl Fn(&str, &str) -> Result<Vec<T>, String>,
    ) -> Result<Vec<T>, String> {
        let mut out = Vec::new();
        for symbol in self.read_symbols() {
            out.extend(parse(body, &symbol)?);
        }
        Ok(out)
    }
}

impl ReconClient for FxcmReconClient {
    /// `_since` is a no-op — the Orders table is the current resting set (see the module doc). The mounted pair's
    /// orders and every admitted pair's.
    fn fetch_order_status_reports(&self, _since: i64) -> Result<Vec<OrderStatusReport>, String> {
        self.parse_each(&self.table_json(Query::Orders)?, parse_orders)
    }

    /// `_since` is a no-op — the Trades table is the current open set. Each open trade is one fill,
    /// keyed by the same `trade_id` the live fill lane reports (so the reconcile dedup aligns). The mounted
    /// pair's and every admitted pair's.
    fn fetch_fill_reports(&self, _since: i64) -> Result<Vec<FillReport>, String> {
        self.parse_each(&self.table_json(Query::Trades)?, parse_fills)
    }

    /// The Trades table net-folded per symbol read (the mounted pair, then each admitted one); no matched row
    /// synthesizes a flat row for that symbol (see [`parse_positions`]).
    fn fetch_position_status_reports(&self) -> Result<Vec<PositionStatusReport>, String> {
        self.parse_each(&self.table_json(Query::Trades)?, parse_positions)
    }

    /// The engine admitted `symbol` on demand (any-symbol, decision 0173): every pass reads it too, with the
    /// request shape the mounted pair already uses (the same whole-table read). Idempotent, append-only, bounded
    /// by the engine's own cap, never fetches (it may be called from a thread that must not wait on a venue).
    fn admit_symbol(&self, symbol: &str) {
        if self.admitted.admit(symbol, &self.symbol) {
            tracing::info!(
                venue = VENUE,
                symbol,
                "reconcile: the engine admitted a pair; every pass now reads it too"
            );
        }
    }

    /// The pairs this client reads beyond the mounted one: a CLAIM (an empty list is "told nothing"), because the
    /// whole-table read really spans every symbol it was told.
    fn admitted_symbols(&self) -> Option<Vec<String>> {
        Some(self.admitted.symbols())
    }

    /// The tradable account's home-currency balance (`fc_account`) — the realized cash truth
    /// `recon::diff_balance` reconciles against.
    fn fetch_balance(&self) -> Result<Option<f64>, String> {
        match self.query(Query::Balance)? {
            Reply::Balance(b) => Ok(Some(b)),
            Reply::Json(_) | Reply::Missing(_) => Ok(None),
        }
    }
}

// --- the factory ---------------------------------------------------------------------------------

/// The venue → `ReconClient` factory (the ReconFactory seam) for FXCM: opens [`FxcmReconClient`]'s
/// own dedicated reconcile session (never the exec side's) from an already-resolved [`FxcmConfig`].
/// `None` on any login failure (bad creds / unreachable / no shim loaded, i.e. no ForexConnect SDK)
/// — reconcile stays unwired for this venue-symbol, exec unaffected.
///
/// [`crate::adapter::FxcmVenueAdapter`]'s start calls it, LAZILY behind reconciliation; it was
/// `vike_tradehub_core::make_engine`'s `("fxcm", _)` arm until the venue-adapter contract
/// (docs/decisions/0096) moved that arm into this crate.
/// ⚠ This said "**Not yet wired into `vike_tradehub_core::make_engine`** — that (and the live demo
/// validation) is a separate follow-up" until 2026-09-28; the arm has called it since FXCM joined
/// the reconciled set, and `tests/fxcm_reconcile_smoke.rs` is the live demo proof, run by hand on
/// a box with the SDK staged. (A "stub build" in the failure list above became "no shim loaded"
/// the same day: #1720 deleted the stub on 2026-09-09.)
pub fn recon_client(config: &FxcmConfig, symbol: &str) -> Option<Box<dyn ReconClient>> {
    FxcmReconClient::connect(config, symbol).map(|c| Box::new(c) as Box<dyn ReconClient>)
}

/// [`recon_client`] for the account labelled `account` (`""` = the default): the client also registers its
/// session with [`crate::reads`] so the gatewayhub's facts lane can read the account's currency and the
/// current Offers table off this same session (see [`FxcmReconClient::connect_for`]). The adapter's start
/// calls this one.
pub fn recon_client_for(
    config: &FxcmConfig,
    symbol: &str,
    account: &str,
) -> Option<Box<dyn ReconClient>> {
    FxcmReconClient::connect_for(config, symbol, account)
        .map(|c| Box::new(c) as Box<dyn ReconClient>)
}

/// [`recon_client_for`] for a LIVE START: a login that fails does not leave the account without
/// reconciliation until a restart. The first attempt is [`recon_client_for`]'s, and its failure is
/// the one `error` line; a client is then returned anyway, answering every read with an error (a
/// failed pass changes nothing, and the node says so once) while a thread tries the login again at
/// `crate::redial::pause_after`'s growing pauses, one `warn` line per attempt. The attempt that
/// opens registers the session in [`crate::reads`] and reads the account's currency exactly as a
/// start that opened first time does (`crate::redial` has the contract).
///
/// `None` when the box has no ForexConnect shim (that answer is cached for the whole process, the loader
/// opens the shim once, so there is nothing to retry), and when the venue REFUSED the login credentials
/// (`crate::redial::refuses_credentials`: a retry cannot cure a rotated password and each one is another
/// failed login): reconcile stays unwired for the mount, as before any retry existed. A retry that later
/// meets a refused credential ends the retrying and keeps answering the error.
/// [`recon_client_for`] stays the ONE-SHOT form the live smokes use, so a dead demo account still
/// reddens them at the connect.
pub fn recon_client_redialing_for(
    config: &FxcmConfig,
    symbol: &str,
    account: &str,
) -> Option<Box<dyn ReconClient>> {
    // This start's place in the order of starts, taken BEFORE its first attempt (which can take 30 s) and
    // used by every retry: a retry that opens after a newer start registered does not replace it.
    let generation = crate::reads::next_generation();
    match FxcmReconClient::open(config, symbol, Some(account), false, generation) {
        Ok(client) => return Some(Box::new(client)),
        Err(true) => return None,
        Err(false) => {}
    }
    if !crate::sdk_available() {
        return None;
    }
    let (config, mount, account) = (config.clone(), symbol.to_string(), account.to_string());
    Some(Box::new(Redial::start(
        symbol,
        move || match FxcmReconClient::open(&config, &mount, Some(&account), true, generation) {
            Ok(client) => Attempt::Opened(client),
            Err(false) => Attempt::TryAgain,
            Err(true) => Attempt::GiveUp,
        },
        pause_after,
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    // Inline sanity for the tiny helpers; the exhaustive body-parsing coverage (synthetic JSON per
    // table) lives in `tests/fxcm_reconcile_parse.rs`, mirroring the oanda/ig recon parse tests.

    #[test]
    fn side_of_reads_the_buysell_code() {
        assert_eq!(side_of("B"), 1);
        assert_eq!(side_of("S"), -1);
        assert_eq!(side_of("s"), -1, "case-insensitive, matching the exec fill lane");
        assert_eq!(side_of(""), 1, "absent/unknown → the conservative buy default, never panics");
    }

    #[test]
    fn order_type_normalizes() {
        assert_eq!(normalize_order_type("LE"), OrderType::Limit);
        assert_eq!(normalize_order_type("L"), OrderType::Limit);
        assert_eq!(normalize_order_type("SE"), OrderType::Stop);
        assert_eq!(normalize_order_type("S"), OrderType::Stop);
        assert_eq!(normalize_order_type("STE"), OrderType::Stop);
        assert_eq!(
            normalize_order_type("OM"),
            OrderType::Unrecognized("om".into()),
            "unknown code → lower-cased as-is"
        );
    }

    #[test]
    fn order_status_normalizes_working_to_accepted() {
        for working in ["W", "P", "I", "Q", "U", "D"] {
            assert_eq!(normalize_order_status(working), OrderStatus::Accepted, "{working}");
        }
        assert_eq!(normalize_order_status("F"), OrderStatus::Filled);
        assert_eq!(normalize_order_status("C"), OrderStatus::Canceled);
        assert_eq!(normalize_order_status("R"), OrderStatus::Rejected);
        assert_eq!(normalize_order_status("whatever"), OrderStatus::Accepted, "unknown → live");
    }

    #[test]
    fn matches_symbol_reverse_maps_the_instrument() {
        assert!(matches_symbol("EUR/USD", "EURUSD"));
        assert!(matches_symbol("xau/usd", "XAUUSD"));
        assert!(!matches_symbol("GBP/USD", "EURUSD"));
    }

    // The no-shim live gate: `connect` spawns a session thread whose login returns `Unavailable`,
    // so the balance probe fails and the factory returns `None`. Every CI box is this box, so CI
    // exercises the connect→None graceful-degradation path.
    //
    // ⚠ The skip used to be `#[cfg(not(fcsdk))]` — a COMPILE-time gate, so on the one box with the
    // SDK staged this test did not exist. It is a runtime check now, for the same reason everything
    // else in this crate is: whether the SDK is reachable is a property of the box the test RUNS
    // on. On a box that HAS the shim this would attempt a live login (credentials, network), which
    // is `crates/bridges/fxcm/tests/fxcm_reconcile_smoke.rs`'s job and not a unit test's.
    #[test]
    fn connect_returns_none_without_the_sdk() {
        if crate::sdk_available() {
            return;
        }
        let config = FxcmConfig {
            user: "u".into(),
            password: "p".into(),
            url: "http://localhost".into(),
            connection: "Demo".into(),
        };
        assert!(FxcmReconClient::connect(&config, "EURUSD").is_none());
        assert!(recon_client(&config, "EURUSD").is_none());
        // ...and a box with no shim is not redialled: the loader's answer is cached for the process.
        assert!(recon_client_redialing_for(&config, "EURUSD", "").is_none());
    }

    // ---- the request port against a session thread the test scripts (finding 5 of the second review) --------

    /// A port whose far end the TEST holds: with the receiver alive and never served it is a MUTE session
    /// (a thread stuck in a call into the venue's library answers nothing, and its queue still accepts).
    fn mute_port() -> (Port, Receiver<Request>) {
        let (tx, rx) = mpsc::channel();
        (Port { tx: Mutex::new(Some(tx)) }, rx)
    }

    /// **A mute session costs the facts asker its deadline, not for ever** (`Port::query` used to wait on
    /// `recv()`), and says so in words that make no claim about the answer.
    #[test]
    fn a_mute_session_costs_the_asker_its_deadline_and_not_for_ever() {
        let (port, _held) = mute_port();
        let started = Instant::now();
        let why =
            port.ask_within(Query::Offers, Duration::from_millis(60)).err().expect("a timeout");
        assert!(why.contains("did not answer within"), "{why}");
        assert!(started.elapsed() < Duration::from_secs(10));
    }

    /// The facts lane's reads on a mute session end as an ASK THAT WAS NOT ANSWERED: for the account's currency
    /// that is [`PropertiesRead::Failed`] (nothing is assumed), never the API's silence.
    #[test]
    fn a_port_with_no_session_behind_it_is_unanswered_not_silent() {
        let (port, held) = mute_port();
        drop(held); // the session thread is gone
        assert!(matches!(port.system_properties(), PropertiesRead::Failed(_)));
        assert!(port.offers().unwrap_err().contains("gone"));
        port.close();
        assert!(matches!(port.system_properties(), PropertiesRead::Failed(_)));
    }

    /// A served session still answers, through both the deadline-less path (the probe and the reconcile
    /// passes) and the facts path, and a read the session reports as FAILED stays the API's word (the inner
    /// result), distinct from "nobody answered".
    #[test]
    fn a_served_session_answers_both_paths_and_keeps_a_failed_read_apart() {
        let (port, rx) = mute_port();
        let server = std::thread::spawn(move || {
            serve(&rx, |query| match query {
                Query::Balance => Ok(Reply::Balance(1.5)),
                Query::SystemProperties => Err("no login rules".to_string()),
                _ => Ok(Reply::Json("[]".to_string())),
            });
        });
        assert!(
            matches!(port.query(Query::Balance), Ok(Reply::Balance(b)) if (b - 1.5).abs() < 1e-12)
        );
        assert!(matches!(
            port.ask_within(Query::Offers, Duration::from_secs(10)),
            Ok(Ok(Reply::Json(body))) if body == "[]"
        ));
        assert!(
            matches!(port.system_properties(), PropertiesRead::Unreadable(why) if why == "no login rules"),
            "the venue's library failed the read: that is the API's silence, not a missing answer"
        );
        port.close();
        server.join().expect("the thread leaves once the port is closed");
    }

    /// **A request nobody waits for any more is skipped**, so a session thread that comes back from a stuck
    /// call does not spend a venue refresh on every ask that timed out meanwhile.
    #[test]
    fn the_session_thread_skips_a_request_whose_asker_gave_up() {
        let (tx, rx) = mpsc::channel();
        let (stale_tx, stale_rx) = mpsc::channel();
        let (live_tx, live_rx) = mpsc::channel();
        tx.send(Request { query: Query::Offers, reply: stale_tx, expires: Some(Instant::now()) })
            .unwrap();
        tx.send(Request { query: Query::Balance, reply: live_tx, expires: None }).unwrap();
        drop(tx);
        let mut served = 0;
        serve(&rx, |_| {
            served += 1;
            Ok(Reply::Balance(2.0))
        });
        assert_eq!(served, 1, "only the request somebody still waits for reaches the venue");
        assert!(stale_rx.try_recv().is_err(), "the expired one was dropped unanswered");
        assert!(live_rx.try_recv().expect("answered").is_ok());
    }

    // ---- the admitted pairs (any-symbol, decision 0173): a client over a session the test scripts ------------
    //
    // DOCS-SHAPED tables (the shapes `fixtures` in `tests/fxcm_reconcile_parse.rs` use); nothing here has met a
    // ForexConnect session.

    fn order_row(order_id: &str, instrument: &str, buysell: &str, amount: f64) -> Value {
        serde_json::json!({
            "order_id": order_id, "instrument": instrument, "buysell": buysell, "type": "LE",
            "amount": amount, "status": "W"
        })
    }

    fn trade_row(trade_id: &str, instrument: &str, buysell: &str, amount: f64, rate: f64) -> Value {
        serde_json::json!({
            "trade_id": trade_id, "order_id": format!("o-{trade_id}"), "instrument": instrument,
            "buysell": buysell, "amount": amount, "open_rate": rate, "commission": 0.0
        })
    }

    /// A reconcile client mounted on `symbol` over a scripted session: the Orders and Trades tables it answers
    /// (`None` = the read FAILS with the venue's words), the Balance probe answered.
    fn client_over(
        symbol: &str,
        orders: Option<Vec<Value>>,
        trades: Option<Vec<Value>>,
    ) -> FxcmReconClient {
        let (port, rx) = mute_port();
        let server = std::thread::spawn(move || {
            serve(&rx, |query| match query {
                Query::Orders => orders
                    .as_ref()
                    .map(|rows| Reply::Json(Value::Array(rows.clone()).to_string()))
                    .ok_or_else(|| "the Orders refresh failed".to_string()),
                Query::Trades => trades
                    .as_ref()
                    .map(|rows| Reply::Json(Value::Array(rows.clone()).to_string()))
                    .ok_or_else(|| "the Trades refresh failed".to_string()),
                Query::Balance => Ok(Reply::Balance(1000.0)),
                Query::SystemProperties | Query::Offers => Ok(Reply::Json("[]".to_string())),
            });
        });
        FxcmReconClient {
            port: Arc::new(port),
            session: Some(server),
            symbol: symbol.to_string(),
            registered: None,
            admitted: AdmittedSymbols::default(),
        }
    }

    fn symbols_of<T>(rows: &[T], symbol: impl Fn(&T) -> String) -> Vec<String> {
        let mut out: Vec<String> = rows.iter().map(symbol).collect();
        out.sort();
        out
    }

    /// **Every pass reads the pairs the engine admitted, with the mounted one** (the review's F-2): the Orders and
    /// Trades tables are whole-account snapshots, so one read serves every symbol and each told symbol is parsed
    /// out of it. Before this the client read the mounted symbol only, so a lost-reply market order or a restart on
    /// an admitted pair left the engine flat while the venue held the position.
    #[test]
    fn every_pass_reads_the_admitted_pairs_beside_the_mounted_one() {
        let client = client_over(
            "EURUSD",
            Some(vec![
                order_row("o1", "EUR/USD", "B", 1000.0),
                order_row("o2", "USD/JPY", "S", 2000.0),
                order_row("o3", "GBP/USD", "B", 3000.0),
            ]),
            Some(vec![
                trade_row("t1", "EUR/USD", "B", 1000.0, 1.10),
                trade_row("t2", "USD/JPY", "S", 3000.0, 150.0),
                trade_row("t3", "USD/JPY", "S", 1000.0, 151.0),
            ]),
        );
        assert_eq!(
            client.admitted_symbols(),
            Some(Vec::new()),
            "a claim, and 'told nothing' is one"
        );
        let orders = client.fetch_order_status_reports(0).expect("orders");
        assert_eq!(
            symbols_of(&orders, |o| o.symbol.clone()),
            ["EURUSD"],
            "nothing admitted: the mounted pair only"
        );

        client.admit_symbol("usdjpy");
        client.admit_symbol("USDJPY"); // idempotent
        client.admit_symbol("EURUSD"); // the mounted pair is always read: not a second place
        assert_eq!(client.admitted_symbols(), Some(vec!["USDJPY".to_string()]));

        let orders = client.fetch_order_status_reports(0).expect("orders");
        assert_eq!(
            symbols_of(&orders, |o| o.symbol.clone()),
            ["EURUSD", "USDJPY"],
            "GBPUSD was never told"
        );
        let fills = client.fetch_fill_reports(0).expect("fills");
        assert_eq!(symbols_of(&fills, |f| f.symbol.clone()), ["EURUSD", "USDJPY", "USDJPY"]);
        let positions = client.fetch_position_status_reports().expect("positions");
        assert_eq!(
            symbols_of(&positions, |p| p.symbol.clone()),
            ["EURUSD", "USDJPY"],
            "one NET row per pair"
        );
        let yen = positions.iter().find(|p| p.symbol == "USDJPY").expect("the admitted pair's row");
        assert!((yen.qty - -4000.0).abs() < 1e-9, "two short trades net to -4000: {}", yen.qty);
        assert!((yen.avg_px - (3000.0 * 150.0 + 1000.0 * 151.0) / 4000.0).abs() < 1e-9);
    }

    /// An admitted pair with NO open trade still gets its FLAT row: `recon::diff` needs the row PRESENT to see a
    /// stale local position the venue has since closed (the mounted pair's rule, now every told pair's).
    #[test]
    fn an_admitted_pair_with_no_open_trade_reports_flat() {
        let client = client_over("EURUSD", Some(Vec::new()), Some(Vec::new()));
        client.admit_symbol("USDJPY");
        let positions = client.fetch_position_status_reports().expect("positions");
        assert_eq!(symbols_of(&positions, |p| p.symbol.clone()), ["EURUSD", "USDJPY"]);
        assert!(positions.iter().all(|p| p.qty == 0.0), "{positions:?}");
    }

    /// The restore planner reads an order a SUCCESSFUL report lacks as gone, so a failed read of the table fails
    /// the whole fetch (never a report that silently omits the admitted pairs), and the list it claims is unchanged.
    #[test]
    fn a_failed_read_fails_the_whole_fetch_whatever_was_admitted() {
        let client = client_over("EURUSD", None, None);
        client.admit_symbol("USDJPY");
        assert!(
            client.fetch_order_status_reports(0).unwrap_err().contains("Orders refresh failed")
        );
        assert!(client.fetch_fill_reports(0).unwrap_err().contains("Trades refresh failed"));
        assert!(client.fetch_position_status_reports().is_err());
        assert_eq!(client.admitted_symbols(), Some(vec!["USDJPY".to_string()]));
    }
}
