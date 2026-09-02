//! FXCM `ReconClient` (ReconFactory seam) — the venue-facing report seam
//! (`vike_exec::recon::ReconClient`) over the ForexConnect O2G table snapshots, mapping them into the
//! normalized reports `recon::diff` reconciles against local state.
//!
//! ## What ForexConnect surfaces (and how it maps)
//! Unlike the JSON-REST venues (oanda/ig), FXCM has no wire payload: the source rows are O2G table
//! rows read over the native SDK. The [`sys`](crate::sys) shim snapshots two tables into JSON arrays
//! (`fc_orders` / `fc_trades`, read-only, mirroring the Accounts/Offers reads the shim already does),
//! and the account row already read by [`sys::FxcmSession::account`] gives balance:
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
//! and filtered to the mounted symbol, stamping the CANONICAL symbol back onto each row so it matches
//! local state. When the symbol has no open trade the position parser synthesizes a FLAT zero row, so
//! `recon::diff` can still detect a stale LOCAL position the venue has since closed — the same
//! synthesize-flat contract oanda/ig/okx use.
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
//! and the caller keeps the static [`vike_model::fee_schedule_for`] default (fail-soft).
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

use std::sync::mpsc::{self, Receiver, Sender};

use serde_json::Value;

use vike_exec::recon::ReconClient;
use vike_model::events::{LiquiditySide, PositionSide, TradeId};
use vike_model::{FillReport, MarginMode, OrderStatusReport, PositionStatusReport};

use crate::config::FxcmConfig;
use crate::event_mapper::from_fxcm_instrument;
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
    if buysell.eq_ignore_ascii_case("S") {
        -1
    } else {
        1
    }
}

/// Does this row's FXCM instrument (`"EUR/USD"`) reverse-map to the mounted canonical symbol
/// (`"EURUSD"`)? Reuses the exec lane's [`from_fxcm_instrument`] so the two agree on the mapping.
fn matches_symbol(instrument: &str, symbol: &str) -> bool {
    from_fxcm_instrument(instrument) == symbol
}

/// Normalize an FXCM order `type` code to the report's lowercase `order_type` vocabulary.
/// `LE`/`L` → `limit`, `SE`/`S`/`STE` → `stop`; any other code is lower-cased as-is (market orders
/// don't rest in the Orders table). Pure — fixture-tested.
pub fn normalize_order_type(code: &str) -> String {
    match code {
        "LE" | "L" => "limit".to_string(),
        "SE" | "S" | "STE" => "stop".to_string(),
        other => other.to_ascii_lowercase(),
    }
}

/// Normalize an FXCM order `status` code to the `OrderStatus` FSM vocabulary `recon::diff` reads. The
/// Orders table holds WORKING orders, so the working/pending codes (`W`/`I`/`P`/`Q`/`U`/`D`/…) fold to
/// `ACCEPTED`; only the terminal codes that can momentarily appear are mapped out. Pure —
/// fixture-tested. Default `ACCEPTED` (a resting order), mirroring oanda's `normalize_order_state`.
pub fn normalize_order_status(code: &str) -> String {
    match code {
        "F" => "FILLED",
        "C" => "CANCELED",
        "R" => "REJECTED",
        _ => "ACCEPTED",
    }
    .to_string()
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
}

/// The session thread's reply to a [`Query`].
enum Reply {
    /// A table snapshot JSON array string (Orders / Trades).
    Json(String),
    /// The account balance (home-currency cash).
    Balance(f64),
}

/// A query plus the oneshot channel the session thread answers on.
struct Request {
    query: Query,
    reply: Sender<Result<Reply, String>>,
}

/// The dedicated session-thread body: log in ONCE, then serve reconcile queries until the handle
/// (and thus the request sender) drops. Login failure — bad creds OR a stub build with no
/// ForexConnect SDK — exits immediately; the pending/next request then sees a disconnected reply
/// channel, which is how [`FxcmReconClient::connect`]'s probe returns `None`. The session drops at
/// end of scope → `fc_logout`.
fn run_session(config: FxcmConfig, rx: Receiver<Request>) {
    let session =
        match FxcmSession::login(&config.user, &config.password, &config.url, &config.connection) {
            Ok(s) => s,
            Err(_) => return, // no session (bad creds / stub build) → probe fails → connect returns None
        };
    while let Ok(req) = rx.recv() {
        let reply = match req.query {
            Query::Orders => session.orders_json().map(Reply::Json).map_err(|e| e.to_string()),
            Query::Trades => session.trades_json().map(Reply::Json).map_err(|e| e.to_string()),
            Query::Balance => {
                session.account().map(|(_, bal)| Reply::Balance(bal)).map_err(|e| e.to_string())
            }
        };
        // The requester may have given up (dropped its receiver) — that is not our error.
        let _ = req.reply.send(reply);
    }
    // `session` drops here → fc_logout
}

/// One reconcile client for a mounted (account, canonical symbol). Holds a `Send` request sender to
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
    tx: Option<Sender<Request>>,
    /// The session thread, kept so [`Drop`] can wait for `fc_logout` to finish. See the type doc.
    session: Option<std::thread::JoinHandle<()>>,
    /// Canonical vike symbol (e.g. `EURUSD`) every report row is filtered to and stamped with.
    symbol: String,
}

impl Drop for FxcmReconClient {
    fn drop(&mut self) {
        // 1. Drop the sender so `run_session`'s `rx.recv()` fails and the loop leaves — which is
        //    what drops `FxcmSession` and calls `fc_logout`. Joining before this deadlocks.
        self.tx = None;
        // 2. THEN wait for that logout to finish, so ForexConnect's own worker threads are stopped
        //    before this process can reach `__cxa_finalize`. See the type doc for the backtrace.
        //    A panicked session thread is reported and otherwise ignored: the caller is already
        //    dropping this client, and there is nothing left to fail.
        if let Some(handle) = self.session.take() {
            if handle.join().is_err() {
                tracing::warn!(
                    venue = VENUE,
                    "the fxcm reconcile session thread panicked; its ForexConnect logout may not \
                     have completed"
                );
            }
        }
    }
}

impl FxcmReconClient {
    /// Spawn the dedicated session thread (which logs in) and validate the login with a balance
    /// probe. `None` when the thread failed to spawn, or the probe fails because login failed / this
    /// is a stub build — reconcile stays unwired for this venue-symbol, exec unaffected (the same
    /// graceful degradation the exec half's own login uses as the live gate).
    pub fn connect(config: &FxcmConfig, symbol: &str) -> Option<FxcmReconClient> {
        let (tx, rx) = mpsc::channel::<Request>();
        let cfg = config.clone();
        // ⚠ The `JoinHandle` is KEPT, not discarded. It used to be `.spawn(…).ok()?`, which left the
        // ForexConnect session thread detached and let the process reach its static destructors
        // while that thread was still inside `fc_logout` — see the SIGSEGV backtrace on
        // [`FxcmReconClient`].
        let session = std::thread::Builder::new()
            .name("fxcm-recon".into())
            .spawn(move || run_session(cfg, rx))
            .ok()?;
        let client =
            FxcmReconClient { tx: Some(tx), session: Some(session), symbol: symbol.to_string() };
        // Probe: a balance query round-trips only if the session thread logged in. A disconnected
        // reply channel (thread already exited) → `Err` → `None`. Mirrors oanda's `/summary` probe.
        match client.query(Query::Balance) {
            Ok(_) => Some(client),
            Err(_) => None,
        }
    }

    /// Send one query to the session thread and block for its reply. `Err` when the session thread is
    /// gone (login failed / stub build / dropped) — the caller (a `fetch_*` method) surfaces it as a
    /// failed pass, never a panic.
    fn query(&self, query: Query) -> Result<Reply, String> {
        let (reply_tx, reply_rx) = mpsc::channel();
        // `tx` is `None` only inside `Drop`, where nothing can still be calling this — so the arm
        // is unreachable in practice and is mapped to the same "the thread is gone" error the send
        // failure produces, rather than unwrapped.
        self.tx
            .as_ref()
            .ok_or_else(|| "fxcm reconcile client is being dropped".to_string())?
            .send(Request { query, reply: reply_tx })
            .map_err(|_| "fxcm reconcile session thread is gone".to_string())?;
        match reply_rx.recv() {
            Ok(inner) => inner,
            Err(_) => Err("fxcm reconcile session dropped the reply".to_string()),
        }
    }

    /// A table-snapshot query, unwrapping the [`Reply::Json`] string (a balance reply here is a
    /// protocol bug, surfaced as `Err` rather than silently mis-parsed).
    fn table_json(&self, query: Query) -> Result<String, String> {
        match self.query(query)? {
            Reply::Json(s) => Ok(s),
            Reply::Balance(_) => {
                Err("fxcm reconcile: expected a table snapshot, got a balance".into())
            }
        }
    }
}

impl ReconClient for FxcmReconClient {
    /// `_since` is a no-op — the Orders table is the current resting set (see the module doc).
    fn fetch_order_status_reports(&self, _since: i64) -> Result<Vec<OrderStatusReport>, String> {
        parse_orders(&self.table_json(Query::Orders)?, &self.symbol)
    }

    /// `_since` is a no-op — the Trades table is the current open set. Each open trade is one fill,
    /// keyed by the same `trade_id` the live fill lane reports (so the reconcile dedup aligns).
    fn fetch_fill_reports(&self, _since: i64) -> Result<Vec<FillReport>, String> {
        parse_fills(&self.table_json(Query::Trades)?, &self.symbol)
    }

    /// The Trades table net-folded to this symbol; no matched row synthesizes a flat row (see
    /// [`parse_positions`]).
    fn fetch_position_status_reports(&self) -> Result<Vec<PositionStatusReport>, String> {
        parse_positions(&self.table_json(Query::Trades)?, &self.symbol)
    }

    /// The tradable account's home-currency balance (`fc_account`) — the realized cash truth
    /// `recon::diff_balance` reconciles against.
    fn fetch_balance(&self) -> Result<Option<f64>, String> {
        match self.query(Query::Balance)? {
            Reply::Balance(b) => Ok(Some(b)),
            Reply::Json(_) => Ok(None),
        }
    }
}

// --- the factory ---------------------------------------------------------------------------------

/// The venue → `ReconClient` factory (the ReconFactory seam) for FXCM: opens [`FxcmReconClient`]'s
/// own dedicated reconcile session (never the exec side's) from an already-resolved [`FxcmConfig`].
/// `None` on any login failure (bad creds / unreachable / stub build with no ForexConnect SDK) —
/// reconcile stays unwired for this venue-symbol, exec unaffected.
///
/// **Not yet wired into `vike_mount::make_engine`** — that (and the live demo validation) is a
/// separate follow-up, as for the other venues' factories (oanda/ig). This factory is the seam that
/// follow-up calls.
pub fn recon_client(config: &FxcmConfig, symbol: &str) -> Option<Box<dyn ReconClient>> {
    FxcmReconClient::connect(config, symbol).map(|c| Box::new(c) as Box<dyn ReconClient>)
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
        assert_eq!(normalize_order_type("LE"), "limit");
        assert_eq!(normalize_order_type("L"), "limit");
        assert_eq!(normalize_order_type("SE"), "stop");
        assert_eq!(normalize_order_type("S"), "stop");
        assert_eq!(normalize_order_type("STE"), "stop");
        assert_eq!(normalize_order_type("OM"), "om", "unknown code → lower-cased as-is");
    }

    #[test]
    fn order_status_normalizes_working_to_accepted() {
        assert_eq!(normalize_order_status("W"), "ACCEPTED");
        assert_eq!(normalize_order_status("I"), "ACCEPTED");
        assert_eq!(normalize_order_status("U"), "ACCEPTED");
        assert_eq!(normalize_order_status("F"), "FILLED");
        assert_eq!(normalize_order_status("C"), "CANCELED");
        assert_eq!(normalize_order_status("R"), "REJECTED");
        assert_eq!(normalize_order_status("whatever"), "ACCEPTED");
    }

    #[test]
    fn matches_symbol_reverse_maps_the_instrument() {
        assert!(matches_symbol("EUR/USD", "EURUSD"));
        assert!(matches_symbol("xau/usd", "XAUUSD"));
        assert!(!matches_symbol("GBP/USD", "EURUSD"));
    }

    // The stub-build (no ForexConnect SDK) live gate: `connect` spawns a session thread whose login
    // returns `Unavailable`, so the balance probe fails and the factory returns `None`. Gated to the
    // stub build — under a real SDK this would attempt a live login (creds/network). This IS the
    // default/CI build, so CI exercises the connect→None graceful-degradation path.
    #[cfg(not(fcsdk))]
    #[test]
    fn connect_returns_none_without_the_sdk() {
        let config = FxcmConfig {
            user: "u".into(),
            password: "p".into(),
            url: "http://localhost".into(),
            connection: "Demo".into(),
        };
        assert!(FxcmReconClient::connect(&config, "EURUSD").is_none());
        assert!(recon_client(&config, "EURUSD").is_none());
    }
}
