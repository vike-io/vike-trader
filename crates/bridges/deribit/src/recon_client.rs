//! Deribit `ReconClient` (Task 10 of the reconciliation-engine feature) — the venue-facing
//! report seam (`vike_exec::recon::ReconClient`) for options, reconciling against:
//!
//!   `private/get_open_orders_by_instrument` (order reports) — an order the venue reports with no
//!       `label` was never placed by vike (`label` IS our `client_order_id`, see
//!       `client.rs::build_order_params`), but [`parse_open_orders`] KEEPS the row and maps the
//!       empty/absent label to `client_order_id: None` rather than dropping it — matching
//!       binance's externally-placed-order handling. `recon::diff::diff` can only classify orders
//!       that are present in the report slice, so surfacing (not skipping) is what lets
//!       `Divergence::UnknownOrder` fire for deribit. (`reconcile.rs::build_orders` is a SEPARATE,
//!       unrelated arm-time `ManagedOrder` snapshot builder that still skips these — that's fine,
//!       it never feeds `diff::diff`.)
//!   `private/get_positions` (currency-scoped, ALL kinds — options AND futures/perps — filtered
//!       client-side to this instrument). The `kind` filter was dropped (Wave 5d) so a mounted
//!       perp/future row and its coin `delta` actually come back; a `kind:"option"` filter dropped
//!       every perp/future row before it could be seen. `size` is ALREADY SIGNED (negative =
//!       short) — never re-signed by `direction`, exactly as `reconcile.rs::build_positions`
//!       documents. Deribit also reports a per-position `delta` in COIN units — surfaced onto the
//!       report (correct for an inverse contract; folded into net greeks as `coin_delta × spot`).
//!       Deribit's position row carries no per-row timestamp, so [`parse_positions`] takes the
//!       fetch-time `now_ms` as a plain parameter (kept pure/testable) rather than reading the
//!       clock itself.
//!   `private/get_user_trades_by_instrument` (fill reports — NEW: `reconcile.rs` never needed
//!       fills, only `history.rs`'s audit-A3 resync did, via the sibling `get_user_trades`
//!       helper on `DeribitRest`). `direction` buy/sell -> side ±1 (same mapping as orders),
//!       `amount`/`price` -> `last_qty`/`last_px`, `fee`/`fee_currency` ->
//!       `commission`/`commission_asset`, and Deribit's own `"M"`/`"T"` `liquidity` spelling ->
//!       `LiquiditySide` (a THIRD spelling, distinct from both binance's boolean flags and the
//!       wire's own lowercase `"maker"`/`"taker"` `LiquiditySide::from(&str)`). A trade row's
//!       `label`, when present, maps straight to `client_order_id` (Deribit echoes it, unlike
//!       binance's `myTrades`/`userTrades`, which echo no client id at all).
//!
//! Every fetch delegates to a PURE free function (`parse_*`) over the raw JSON-RPC RESULT
//! payload — the same "just the unwrapped `result` value" shape `reconcile.rs`'s
//! `build_positions`/`build_orders` already take, and what `client.rs::private_result` already
//! unwraps. These are the fixture-tested units (`tests/offline/recon_client_parse.rs`, no network); the
//! live path re-serializes the fetched `Value` to a string once and hands it to the pure parser
//! — the same small, deliberate cost `vike-binance`'s `recon_client.rs` documents.
//!
//! `DeribitReconClient` reuses `DeribitRest::private_result` (`pub(crate)`) — the existing signed
//! JSON-RPC entry point over an authed order WS (`Mutex<DeribitOrderTransport>`) — and no new
//! HTTP/WS/crypto stack. It is constructed via [`DeribitReconClient::connect`] with its OWN
//! dedicated authed socket (NOT the exec side's), so a reconcile fetch never contends the exec
//! order-transport `Mutex` and can never delay an order submit; see that fn's doc for the rationale
//! (this deviates from an earlier "share the exec connection" intent — deliberately). `new` still
//! accepts a caller-supplied `Arc<DeribitRest>` for tests / a future shared-socket path — ⚠ but
//! that client now OWNS the transport it is handed, because it RE-DIALS it.
//!
//! That socket lifecycle is the other half of this module, and it is tested offline:
//! `tests/offline/recon_client_redial.rs` drives this client against a local `ws://` Deribit
//! stand-in (`crate::transport::DeribitOrderTransport` takes its `ws_url` as a constructor
//! parameter) that closes the connection mid-session, and asserts the next fetch re-dials and
//! succeeds. See [`DeribitReconClient::call`] for why that retry lives here and may never move
//! anywhere the exec path can reach it.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use serde_json::{Value, json};

use vike_bridge_core::Credentials;
use vike_bridge_core::json::{get_f64 as num, json_num, parse_rows};
use vike_bridge_core::transport::{E_TIMEOUT_AMBIGUOUS, VenueApiError};
use vike_exec::recon::ReconClient;
use vike_model::events::{LiquiditySide, PositionSide, TradeId};
use vike_model::{
    FeeSchedule, FillReport, OrderStatusReport, PositionStatusReport, SymbolProperties, now_ms,
};

use crate::client::DeribitRest;
use crate::transport::{DeribitOrderTransport, TESTNET_WS, is_dead_socket_error};

const VENUE: &str = "deribit";

// --- pure parsers ------------------------------------------------------------------------------

/// Deribit ids (`order_id`) arrive as JSON strings or numbers — twin of the inline match already
/// used in `reconcile.rs::build_orders` / `client.rs`'s `submit_order` order-id extraction.
fn id_string(v: Option<&Value>) -> String {
    match v {
        Some(Value::String(s)) => s.clone(),
        Some(other) => other.to_string(),
        None => String::new(),
    }
}

/// `direction` -> signed side. Matches `reconcile.rs::build_orders`'s exact mapping.
fn side_from_direction(direction: &str) -> i32 {
    if direction == "buy" { 1 } else { -1 }
}

/// Deribit's `get_user_trades_by_instrument` spells the liquidity flag `"M"`/`"T"` — NOT the wire
/// lowercase `"maker"`/`"taker"` `LiquiditySide::from(&str)` parses, and NOT binance's boolean
/// `isMaker`/`maker` flags either. A third, venue-specific spelling.
fn deribit_liquidity(raw: &str) -> LiquiditySide {
    match raw {
        "M" => LiquiditySide::Maker,
        "T" => LiquiditySide::Taker,
        _ => LiquiditySide::Unknown,
    }
}

/// `private/get_open_orders_by_instrument` rows -> `OrderStatusReport`. Every open order is
/// KEPT — an empty/absent `label` (externally-placed — see the module doc) maps to
/// `client_order_id: None` instead of being dropped, mirroring binance's `parse_order_row`, so
/// `recon::diff::diff` can route it to `Divergence::UnknownOrder`. Status is derived from
/// `filled_amount` exactly as `reconcile.rs` already seeds `ManagedOrder::status` on arm
/// (`filled > 0.0 -> PARTIALLY_FILLED`, else `ACCEPTED`) — this endpoint only ever reports
/// currently-open orders, so no terminal status can appear here.
pub fn parse_open_orders(body: &str) -> Result<Vec<OrderStatusReport>, String> {
    parse_rows(body, parse_order_row)
}

fn parse_order_row(row: &Value) -> OrderStatusReport {
    let label = row.get("label").and_then(|l| l.as_str()).unwrap_or("");
    let client_order_id = if label.is_empty() { None } else { Some(label.to_string()) };
    let symbol = row.get("instrument_name").and_then(|s| s.as_str()).unwrap_or("").to_string();
    let side = side_from_direction(row.get("direction").and_then(|d| d.as_str()).unwrap_or(""));
    let order_type = row.get("order_type").and_then(|t| t.as_str()).unwrap_or("limit").to_string();
    let qty = num(row, "amount");
    let filled_qty = num(row, "filled_amount");
    let avg_px = num(row, "average_price");
    let ts = row
        .get("last_update_timestamp")
        .and_then(|t| t.as_i64())
        .or_else(|| row.get("creation_timestamp").and_then(|t| t.as_i64()))
        .unwrap_or(0);
    OrderStatusReport {
        venue: VENUE.to_string(),
        symbol,
        venue_order_id: id_string(row.get("order_id")).into(),
        client_order_id,
        side,
        order_type,
        qty,
        filled_qty,
        avg_px,
        status: if filled_qty > 0.0 { "PARTIALLY_FILLED" } else { "ACCEPTED" }.to_string(),
        ts,
    }
}

/// `private/get_positions` rows -> `PositionStatusReport`, filtered to `symbol` (the endpoint is
/// currency+kind scoped, not instrument scoped — mirrors `reconcile.rs::build_positions`'s
/// filter). `size` is ALREADY SIGNED — never re-signed by `direction` (same guard
/// `reconcile.rs` documents). Options are one-way -> `PositionSide::Both` (mirrors
/// `reconcile.rs`'s `position_sides` staying empty). Deribit's row carries no timestamp; `now_ms`
/// is threaded in by the caller so this parser stays pure/deterministic under test. A flat
/// (`size == 0`) row is KEPT, not filtered — `recon::diff::diff` needs the report row present to
/// detect "local still shows a position the venue has since closed", same rationale as binance's
/// `parse_perp_position_risk`.
pub fn parse_positions(
    body: &str,
    symbol: &str,
    now_ms: i64,
) -> Result<Vec<PositionStatusReport>, String> {
    let v: Value = serde_json::from_str(body).map_err(|e| e.to_string())?;
    let rows = v.as_array().ok_or_else(|| "expected a JSON array".to_string())?;
    Ok(rows
        .iter()
        .filter(|row| row.get("instrument_name").and_then(|n| n.as_str()) == Some(symbol))
        .map(|row| PositionStatusReport {
            venue: VENUE.to_string(),
            symbol: symbol.to_string(),
            position_side: PositionSide::Both,
            qty: num(row, "size"), // ALREADY SIGNED
            avg_px: num(row, "average_price"),
            ts: now_ms,
            // Deribit is cross-only (portfolio margin = a maintenance-rate variation within
            // cross, see `vike_model::venue_margin_support`) — the defaults are the truth here.
            margin_mode: Default::default(),
            isolated_margin: None,
            // Deribit's per-position `delta` in COIN units — correct for an INVERSE contract,
            // whose `size` above is USD NOTIONAL (perp/future) rather than coin. Surfaced so a
            // linear perp/future hedge leg folds into net portfolio greeks as `coin_delta × spot`
            // (Wave 5d) instead of re-deriving coin exposure from the USD-notional `size`. Absent
            // (`None`) when the row carries no `delta`; harmless for options (their delta is priced
            // from the chain, not this field).
            delta: row.get("delta").and_then(json_num),
        })
        .collect())
}

/// `private/get_user_trades_by_instrument` (`trades` array, already unwrapped by the caller) rows
/// -> `FillReport`. See the module doc for the field mapping, incl. Deribit's `"M"`/`"T"`
/// liquidity spelling and the `label` -> `client_order_id` passthrough.
///
/// A row with no `trade_id` is DROPPED, not reported. A `FillReport` with no `trade_id` is worse than
/// a missing one: `vike_exec::recon::diff` decides "already known" by looking the id up in
/// `seen_trade_ids`, so an id-less row can never match and instead FABRICATES a `MissingFill`
/// divergence — one of the two kinds the `hybrid` policy AUTO-APPLIES
/// (`vike_ops::reconcile_config::auto_applied_kinds`), i.e. it would book the fill a second time with
/// no operator in front of it. Dropping is the safe direction: the worst case is a real fill going
/// unreconciled until a pass sees it with its id. Deribit documents `trade_id` on every trade row.
pub fn parse_user_trades(body: &str) -> Result<Vec<FillReport>, String> {
    // `flatten` is the row DROP — see this fn's doc for why reporting it would be worse.
    let rows = parse_rows(body, |row| {
        let symbol = row.get("instrument_name").and_then(|s| s.as_str()).unwrap_or("").to_string();
        let side = side_from_direction(row.get("direction").and_then(|d| d.as_str()).unwrap_or(""));
        let client_order_id = row
            .get("label")
            .and_then(|l| l.as_str())
            .filter(|l| !l.is_empty())
            .map(|l| l.to_string());
        let trade_id = match TradeId::new(
            row.get("trade_id").and_then(|t| t.as_str()).unwrap_or(""),
        ) {
            Ok(id) => id,
            Err(_) => {
                tracing::warn!(
                    venue = VENUE,
                    symbol = %symbol,
                    "user-trades row carries no `trade_id` — skipping the FillReport; an id-less \
                     row can never match `seen_trade_ids`, so it would fabricate a MissingFill \
                     divergence that `hybrid` auto-applies (booking the fill twice)"
                );
                return None;
            }
        };
        Some(FillReport {
            venue: VENUE.to_string(),
            symbol,
            trade_id,
            venue_order_id: id_string(row.get("order_id")).into(),
            client_order_id,
            side,
            last_qty: num(row, "amount"),
            last_px: num(row, "price"),
            commission: num(row, "fee"),
            commission_asset: row
                .get("fee_currency")
                .and_then(|c| c.as_str())
                .unwrap_or("")
                .to_string(),
            liquidity_side: deribit_liquidity(
                row.get("liquidity").and_then(|l| l.as_str()).unwrap_or(""),
            ),
            ts: row.get("timestamp").and_then(|t| t.as_i64()).unwrap_or(0),
        })
    })?;
    Ok(rows.into_iter().flatten().collect())
}

/// `private/get_account_summary` result -> the currency's `balance` (raw wallet cash — realized,
/// EXCLUDING unrealized PnL from open option positions). Deribit's `equity` field is the
/// position-inclusive, mark-to-market figure instead; `balance` is picked here to match the OTHER
/// three venues' choice of raw wallet/cash balance (binance `balance`, bybit `walletBalance`, okx
/// `cashBal` — none fold unrealized PnL in), so `ReconClient::fetch_balance` means the same thing
/// across every venue this crate's siblings implement it for. `None` when the field is absent;
/// malformed JSON (not an object) is a hard `Err`, never a panic.
pub fn parse_account_summary_balance(body: &str) -> Result<Option<f64>, String> {
    let v: Value = serde_json::from_str(body).map_err(|e| e.to_string())?;
    if !v.is_object() {
        return Err("expected a JSON object".to_string());
    }
    Ok(v.get("balance").and_then(json_num))
}

/// `public/get_instrument` result -> live [`FeeSchedule`] from `maker_commission`/
/// `taker_commission` (fractions of the underlying). Deribit fees are genuinely
/// PER-INSTRUMENT-class (options 0.03% maker==taker vs futures 0% maker / 0.05% taker), NOT
/// per-account — the live layer is scoped to the mounted symbol's instrument (no fee-catalog).
/// `Ok(None)` when the fields are absent (fail-soft → static default); a non-object body is a hard
/// `Err`, never a panic.
pub fn parse_fee_rate(body: &str) -> Result<Option<FeeSchedule>, String> {
    let v: Value = serde_json::from_str(body).map_err(|e| e.to_string())?;
    if !v.is_object() {
        return Err("expected a JSON object".to_string());
    }
    let maker = v.get("maker_commission").and_then(json_num);
    let taker = v.get("taker_commission").and_then(json_num);
    Ok(match (maker, taker) {
        (Some(m), Some(t)) => Some(FeeSchedule::from_fractions(m, t)),
        _ => None,
    })
}

// --- the client ----------------------------------------------------------------------------

const PATH_OPEN_ORDERS: &str = "private/get_open_orders_by_instrument";
const PATH_POSITIONS: &str = "private/get_positions";
const PATH_USER_TRADES: &str = "private/get_user_trades_by_instrument";
const PATH_ACCOUNT_SUMMARY: &str = "private/get_account_summary";

/// One `ReconClient` per instrument, over a `DeribitRest` whose order-WS transport this client
/// OWNS — see [`DeribitReconClient::connect`] for why it is a dedicated socket rather than the
/// exec side's (the module doc's original "no second connection" intent was deliberately reversed
/// there). Owning it is what licenses [`DeribitReconClient::call`]'s re-dial.
pub struct DeribitReconClient {
    rest: Arc<DeribitRest>,
    /// Log latch for the re-dial path, so ONE dead socket costs ONE line instead of one per
    /// attempt. Set when the outage is first reported, cleared (with a recovery line) by the next
    /// successful call. The incident this whole path exists for logged 795 identical lines from a
    /// single close — the cure must not reproduce that at a different layer.
    socket_down: AtomicBool,
}

impl DeribitReconClient {
    /// ⚠ The client TAKES OVER `rest`'s order-WS transport: [`DeribitReconClient::call`] re-dials
    /// it (`connect()`, which closes the prior socket) on a transport failure. Handing in the
    /// `Arc<DeribitRest>` the EXEC side drives would therefore re-open the order socket underneath
    /// a live order path — so the "future shared-socket path" this constructor was left open for
    /// is now a decision that has to be made HERE, not merely wired up.
    pub fn new(rest: Arc<DeribitRest>) -> Self {
        DeribitReconClient { rest, socket_down: AtomicBool::new(false) }
    }

    /// Build a DEDICATED-socket recon client for `(creds, symbol)` on Deribit TESTNET (demo): open a
    /// fresh authed order-WS, blocking on the JSON-RPC auth, and wrap it in its own [`DeribitRest`].
    /// `None` on auth/connect failure (the reconcile stays unwired for this venue — same graceful
    /// degradation as every other absent-recon path).
    ///
    /// DELIBERATE deviation from this module's original "share the exec side's ONE authed socket"
    /// note: recon uses its OWN connection so its (occasional) blocking report fetches NEVER take the
    /// exec side's order-transport `Mutex` — an order submit is never delayed behind a reconcile
    /// round-trip. This mirrors the resync supervisor (which already opens a 2nd authed socket) and
    /// every other venue's "recon uses a fresh transport, never shares exec's" convention. The venue
    /// report reads (`private/get_open_orders`/`get_positions`/`get_user_trades`/`get_account_summary`)
    /// need no order-formatting grid, so a default [`SymbolProperties`] is fine (only `symbol` +
    /// `currency` are read by the fetch paths).
    pub fn connect(creds: &Credentials, symbol: &str) -> Option<Self> {
        let mut transport =
            DeribitOrderTransport::new(TESTNET_WS, &creds.api_key, &creds.api_secret, None);
        if let Err(e) = transport.connect() {
            tracing::warn!(
                target: "vike_deribit::recon",
                %symbol,
                error = %e,
                "recon order-WS auth failed; deribit reconcile disabled (exec unaffected)"
            );
            return None;
        }
        // currency = the leg before the first '-' (BTC-PERPETUAL -> BTC), matching `exec::currency_of`.
        let currency = symbol.split('-').next().unwrap_or(symbol).to_uppercase();
        let rest = DeribitRest::new(transport, symbol, SymbolProperties::default(), &currency);
        Some(DeribitReconClient::new(Arc::new(rest)))
    }

    /// Every recon fetch below goes through here, and it is the ONE place in this crate that
    /// re-dials a dead order-WS.
    ///
    /// ⚠ **WHY THE RETRY IS HERE AND MAY NOT MOVE DOWN INTO THE TRANSPORT.** Every method this
    /// client sends is an idempotent READ (`private/get_open_orders_by_instrument`,
    /// `private/get_positions`, `private/get_user_trades_by_instrument`,
    /// `private/get_account_summary`, `public/get_instrument`), so re-sending one cannot place,
    /// cancel, double or otherwise move anything. The EXEC path's calls are not: a re-sent
    /// `private/buy` after an ambiguous failure is a SECOND ORDER, which is the entire reason
    /// `crates/bridges/deribit/src/transport.rs`'s `call` classifies a response timeout as
    /// [`E_TIMEOUT_AMBIGUOUS`] and hands the decision up. Putting this in
    /// `DeribitOrderTransport::call` — or anywhere `crates/bridges/deribit/src/exec.rs` reaches —
    /// would silently arm blind order retries.
    ///
    /// **What it fixes** (the CI box, 2026-08-23): the transport leaves a failed socket in place and
    /// `connect()` runs exactly once, at mount, so the first venue-side close disabled deribit
    /// reconcile for the life of the daemon — 795 identical failures over a day, while the daemon
    /// reported healthy. Every other socket in this crate already has a reconnect lifecycle; this
    /// one had none.
    ///
    /// **ONE retry, no loop.** The reconcile driver re-runs the whole pass every
    /// `VIKE_RECONCILE_INTERVAL_MS` (~60s), which IS the retry cadence, so a genuinely dead venue
    /// costs one failed pass rather than a spin — and a re-dial that fails returns the original
    /// error rather than a connect error, because the original is what the operator needs to see.
    fn call(&self, method: &str, params: &Value) -> Result<Value, String> {
        match self.rest.private_result(method, params) {
            Ok(v) => {
                self.mark_healthy();
                Ok(v)
            }
            Err(e) if earns_a_redial(&e) => self.redial_and_retry(method, params, &e),
            Err(e) => Err(describe(&e)),
        }
    }

    /// Re-open the order-WS (`connect()` is idempotent and re-authenticates — it closes the prior
    /// socket first, which is what finally drops the dead one) and re-send the request ONCE.
    fn redial_and_retry(
        &self,
        method: &str,
        params: &Value,
        first: &VenueApiError,
    ) -> Result<Value, String> {
        // The latch, not the attempt, decides whether this is logged: one line per OUTAGE.
        if !self.socket_down.swap(true, Ordering::Relaxed) {
            tracing::warn!(
                target: "vike_deribit::recon",
                symbol = %self.rest.symbol,
                method,
                code = first.code,
                error = %first.msg,
                "recon order-WS unusable — re-dialing; further failures stay silent until it recovers"
            );
        }
        if let Err(e) = self.rest.transport.lock().unwrap().connect() {
            // Still latched: the next pass retries, silently, ~60s from now.
            return Err(format!("{} (re-dial failed: {e})", describe(first)));
        }
        let out = self.rest.private_result(method, params).map_err(|e| describe(&e))?;
        self.mark_healthy();
        Ok(out)
    }

    /// Clear the log latch, announcing the recovery exactly once.
    fn mark_healthy(&self) {
        if self.socket_down.swap(false, Ordering::Relaxed) {
            tracing::info!(
                target: "vike_deribit::recon",
                symbol = %self.rest.symbol,
                "recon order-WS re-dialed — deribit reconcile is live again"
            );
        }
    }
}

/// Which failures earn a re-dial — the policy [`crate::transport::is_dead_socket_error`]
/// deliberately refuses to state for the transport.
///
/// A DEFINITE socket death, plus [`E_TIMEOUT_AMBIGUOUS`]. The timeout is included for two reasons,
/// and the first is what makes it safe: on an idempotent READ there is no ambiguity to respect —
/// the sentinel exists so the EXEC path never blind-retries an order that the venue may already
/// have filled, and none of the methods this predicate's callers send can move anything. The
/// second is that without it one failure shape could never heal: a black-holed socket (TCP still
/// open, venue silent) never produces a send error, so every pass would time out at
/// `request_timeout` forever, which is the same never-recovers defect this whole path fixes,
/// wearing a different error.
///
/// A venue JSON-RPC error (`10009 not_enough_funds`, `13009 unauthorized`, `-32602 Invalid
/// params`) is deliberately NOT here: the venue ANSWERED, so the socket is alive, and re-dialing
/// would churn a fresh TCP+TLS+auth handshake against a credit pool that
/// `crates/bridges/deribit/src/ratelimit.rs` documents as the roster's only zero-margin meter —
/// while hiding a real API refusal behind a reconnect that cannot fix it.
///
/// ⚠ **`pub(crate)`, and the READ-ONLY licence is a PRECONDITION on every caller, not a property
/// of the predicate.** It says "a fresh socket might fix this"; it says NOTHING about whether the
/// failed request may be sent a second time — the caller's own methods decide that, exactly as
/// `crates/bridges/deribit/src/transport.rs`'s `is_dead_socket_error` splits the two questions.
/// The second caller is `crates/bridges/deribit/src/exec.rs`'s `A3Resync`, whose socket carries
/// `public/auth` plus two `get_*` history reads and nothing else; that type's doc argues its own
/// admission. A caller that can send `private/buy` may NOT use this — see
/// `crates/bridges/deribit/src/client.rs`'s `dispatch_submit`, which re-dials on
/// `is_dead_socket_error` and then re-QUERIES rather than re-sending.
pub(crate) fn earns_a_redial(err: &VenueApiError) -> bool {
    is_dead_socket_error(err) || err.code == E_TIMEOUT_AMBIGUOUS
}

/// The `ReconClient` trait's error is a `String`; this is the spelling every fetch below has
/// always surfaced (`[code] message`).
fn describe(err: &VenueApiError) -> String {
    format!("[{}] {}", err.code, err.msg)
}

// --- the factory -----------------------------------------------------------------------------

/// The venue -> `ReconClient` factory (ReconFactory seam, wave-2 task 6) — a thin type-erasing
/// wrapper over [`DeribitReconClient::connect`], moved verbatim from `vike_mount::make_engine`'s
/// `("deribit", Some(c))` arm (see that arm's comment for why Deribit needs its OWN blocking authed
/// order-WS `connect` rather than the pure stateless-REST shape every other bridge's factory
/// builds). `None` on auth/connect failure — exactly the same graceful degradation `connect`
/// already documents, now reachable through the same `recon_client` name every other bridge uses.
pub fn recon_client(creds: &Credentials, symbol: &str) -> Option<Box<dyn ReconClient>> {
    DeribitReconClient::connect(creds, symbol).map(|rc| Box::new(rc) as Box<dyn ReconClient>)
}

impl ReconClient for DeribitReconClient {
    /// `since` is a no-op here, for the SAME reason as binance's: `get_open_orders_by_instrument`
    /// has no time filter and reports only the CURRENTLY open set.
    fn fetch_order_status_reports(&self, _since: i64) -> Result<Vec<OrderStatusReport>, String> {
        let result = self
            .call(PATH_OPEN_ORDERS, &json!({"instrument_name": self.rest.symbol, "type": "all"}))?;
        parse_open_orders(&result.to_string())
    }

    fn fetch_fill_reports(&self, since: i64) -> Result<Vec<FillReport>, String> {
        let mut params = json!({
            "instrument_name": self.rest.symbol,
            "count": 1000,
            "sorting": "asc",
        });
        if since > 0 {
            params["start_timestamp"] = json!(since);
        }
        let result = self.call(PATH_USER_TRADES, &params)?;
        let trades = result.get("trades").cloned().unwrap_or_else(|| json!([]));
        parse_user_trades(&trades.to_string())
    }

    fn fetch_position_status_reports(&self) -> Result<Vec<PositionStatusReport>, String> {
        // No `kind` filter: fetch ALL kinds (options AND futures/perpetuals) for the currency, so a
        // mounted perp/future position row (with its coin `delta`) actually comes back — a
        // `kind:"option"` filter dropped every perp/future row before it could be seen (Wave 5d).
        // Options are still returned (they are part of the all-kinds response); `parse_positions`
        // then filters client-side to the mounted `symbol` exactly as before.
        let result = self.call(PATH_POSITIONS, &json!({"currency": self.rest.currency}))?;
        parse_positions(&result.to_string(), &self.rest.symbol, now_ms())
    }

    /// Balance meaning (pinned per the task brief): the currency's raw `balance` from
    /// `private/get_account_summary` — NOT `equity` (see the parser's doc comment). A light,
    /// currency-scoped account read — reuses the same `private_result` JSON-RPC entry point over
    /// the existing authed order WS as every other fetch here (no new connection).
    fn fetch_balance(&self) -> Result<Option<f64>, String> {
        let result = self.call(PATH_ACCOUNT_SUMMARY, &json!({"currency": self.rest.currency}))?;
        parse_account_summary_balance(&result.to_string())
    }

    /// Live per-instrument fee rates (fee model 4/5): `public/get_instrument` for the mounted
    /// symbol — `maker_commission`/`taker_commission` (fractions of underlying). A public method
    /// sent over the recon client's existing authed order-WS (no new connection, no signing
    /// needed). `Ok(None)` keeps the static default (fail-soft). Deribit fees are per-instrument,
    /// so this is deliberately scoped to `self.rest.symbol` — not a fee-catalog.
    fn fetch_fee_rates(&self) -> Result<Option<FeeSchedule>, String> {
        let result =
            self.call("public/get_instrument", &json!({"instrument_name": self.rest.symbol}))?;
        parse_fee_rate(&result.to_string())
    }
}
