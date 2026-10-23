//! Bybit `ReconClient` (Task 12 of the reconciliation-engine feature) — the venue-facing report
//! seam (`vike_exec::recon::ReconClient`) for the linear-perp category this crate trades,
//! reconciling against:
//!
//!   `GET /v5/order/realtime` — resting + recently-closed orders (order reports). Like binance's
//!   `openOrders`, this is the venue's CURRENTLY-open/recent view with no time filter, so `since`
//!   is a no-op here (a fully-closed order that fell out of this set is exactly what the fill/
//!   position reports, not this one, reconcile).
//!   `GET /v5/execution/list` — fill history (fill reports); `since` maps to `startTime`.
//!   `GET /v5/position/list` — net position per (symbol, positionIdx) leg (position reports).
//!   `size` on the wire is UNSIGNED — the sign comes from `side` ("Buy"/"Sell"), exactly like
//!   `perp::BybitPerpRest::reconcile_positions`, whose Buy/Sell→sign + hedge `positionIdx`→
//!   `position_side` derivation this factors out as [`parse_positions`] EXCEPT it keeps every row
//!   including a flat (`size == 0`) leg: `recon::diff::diff` needs the report row PRESENT to
//!   detect "local still shows a position the venue has since closed" (mirrors the binance
//!   `ReconClient`'s Task 7 doc'd choice; `reconcile_positions`'s own `if size == 0.0 { continue }`
//!   filter is right for THAT snapshot's `ReconcileSnapshot` shape but wrong for this one).
//!
//! Every fetch delegates to a PURE free function (`parse_*`) over the raw `result` JSON body
//! (the `{retCode, retMsg, result}` V5 envelope already unwrapped by [`unwrap_envelope`]) — these
//! are the fixture-tested units (`tests/offline/recon_client_parse.rs`, no network).
//!
//! `BybitReconClient` reuses the SAME `BybitTransport`/`BybitV5Signer` seam as
//! `perp::BybitPerpRest` (no new HTTP/signing stack): `order/realtime`/`position/list` ride the
//! normal `signed` path (the same one `BybitPerpRest::call` already uses for order/position
//! reads), while `execution/list` rides `signed_requery` — the SAME short-timeout,
//! rate-gate-EXEMPT path the crate's audit-A3 resync (`get_execution_history`) already uses for
//! this exact endpoint, so a resync burst can't blow the order-rate budget submit/cancel depend on.

use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use vike_bridge_core::json::{json_int, json_num, json_str};
use vike_bridge_core::signer::BybitV5Signer;
use vike_bridge_core::transport::{ErrorKind, VenueApiError};
use vike_exec::recon::{AdmittedSymbols, ReconClient};
use vike_model::events::{LiquiditySide, TradeId};
use vike_model::{
    FeeSchedule, FillReport, MarginMode, OrderStatus, OrderStatusReport, OrderType,
    PositionStatusReport,
};

use crate::perp::{PATH_ACCOUNT, PATH_EXECUTION_LIST, PATH_ORDER_REALTIME, PATH_POSITION_LIST};
use crate::transport::{BybitTransport, unwrap_envelope};

const VENUE: &str = "bybit";
const PATH_FEE_RATE: &str = "/v5/account/fee-rate";

// --- pure parsers ------------------------------------------------------------------------------

/// Every V5 `result` object is `{"list": [...], ...}` — pull the row array out, erroring (never
/// panicking) on a missing/malformed `list`.
fn list_rows(body: &str) -> Result<Vec<Value>, String> {
    let v: Value = serde_json::from_str(body).map_err(|e| e.to_string())?;
    v.get("list")
        .and_then(|l| l.as_array())
        .cloned()
        .ok_or_else(|| "expected a `list` array".to_string())
}

fn side_sign(s: &str) -> i32 {
    if s.eq_ignore_ascii_case("Buy") { 1 } else { -1 }
}

/// Map a raw V5 `orderStatus` onto the FSM [`OrderStatus`] `recon::diff::diff` reads — every word
/// the V5 order endpoints document, explicitly:
///
/// - `New`, `Created` (accepted by the system, not yet through the matching engine), `Active` (the
///   legacy USDC-contract spelling of `New`) and `Untriggered` (a resting conditional waiting on its
///   trigger) → `Accepted`: all still open.
/// - `Triggered` → `Triggered`; `PartiallyFilled` → `PartiallyFilled`; `Filled` → `Filled`.
/// - `Cancelled` and `PartiallyFilledCanceled` (partially filled, then canceled) → `Canceled`.
/// - `Rejected` → `Rejected`.
/// - `Deactivated` (a conditional that never triggered and was pulled) → `Expired`, matching
///   `history::map_bybit_history`'s convention for the same status.
///
/// A word outside that list is WARNED and read as `Accepted`: live, the conservative direction —
/// `diff` raises no `MissingTerminal` on it, adoption synthesizes no cumulative fill, and the restore
/// planner keeps the order rather than declaring it gone.
fn normalize_order_status(raw: &str) -> OrderStatus {
    match raw {
        "New" | "Created" | "Active" | "Untriggered" => OrderStatus::Accepted,
        "Triggered" => OrderStatus::Triggered,
        "PartiallyFilled" => OrderStatus::PartiallyFilled,
        "Filled" => OrderStatus::Filled,
        "Cancelled" | "PartiallyFilledCanceled" => OrderStatus::Canceled,
        "Rejected" => OrderStatus::Rejected,
        "Deactivated" => OrderStatus::Expired,
        other => {
            tracing::warn!(
                venue = VENUE,
                status = other,
                "unknown orderStatus word — reporting the order as live (Accepted)"
            );
            OrderStatus::Accepted
        }
    }
}

/// Map a raw V5 `orderType` onto [`OrderType`]: `Limit` → `Limit`, `Market` → `Market` (a
/// conditional's trigger kind rides the separate `stopOrderType`, which this report does not read).
/// Any other word (V5's `UNKNOWN`) has no twin and stays [`OrderType::Unrecognized`] lower-cased,
/// as every report spelled it before the type existed.
fn normalize_order_type(raw: &str) -> OrderType {
    match raw {
        "Limit" => OrderType::Limit,
        "Market" => OrderType::Market,
        other => OrderType::Unrecognized(other.to_ascii_lowercase()),
    }
}

/// `GET /v5/order/realtime` rows -> `OrderStatusReport`. `qty`/`cumExecQty`/`avgPrice` are
/// echoed directly (unlike binance spot, no average-price derivation needed). An empty
/// `orderLinkId` normalizes to `None` (an externally-placed order the venue never echoed our id
/// for) — same convention as the binance `ReconClient`.
pub fn parse_orders(body: &str) -> Result<Vec<OrderStatusReport>, String> {
    let rows = list_rows(body)?;
    Ok(rows
        .iter()
        .map(|o| {
            let client_order_id = o
                .get("orderLinkId")
                .and_then(|c| c.as_str())
                .filter(|c| !c.is_empty())
                .map(|c| c.to_string());
            OrderStatusReport {
                venue: VENUE.to_string(),
                symbol: o.get("symbol").and_then(|s| s.as_str()).unwrap_or("").to_string(),
                venue_order_id: o.get("orderId").map(json_str).unwrap_or_default().into(),
                client_order_id,
                side: side_sign(o.get("side").and_then(|s| s.as_str()).unwrap_or("")),
                order_type: normalize_order_type(
                    o.get("orderType").and_then(|t| t.as_str()).unwrap_or(""),
                ),
                qty: o.get("qty").and_then(json_num).unwrap_or(0.0),
                filled_qty: o.get("cumExecQty").and_then(json_num).unwrap_or(0.0),
                avg_px: o.get("avgPrice").and_then(json_num).unwrap_or(0.0),
                status: normalize_order_status(
                    o.get("orderStatus").and_then(|s| s.as_str()).unwrap_or(""),
                ),
                ts: o.get("updatedTime").and_then(json_int).unwrap_or(0),
            }
        })
        .collect())
}

/// `GET /v5/execution/list` rows -> `FillReport`. Unlike binance's `myTrades`/`userTrades`, Bybit
/// DOES echo `orderLinkId` on every execution row (see `history::execution_fill`), so
/// `client_order_id` is populated straight from the wire here rather than left `None`.
///
/// A row with no `execId` is DROPPED (`filter_map`), not reported. A `FillReport` with no `trade_id`
/// is worse than a missing one: `vike_exec::recon::diff` decides "already known" by looking the id
/// up in `seen_trade_ids`, so an id-less row can never match and instead FABRICATES a `MissingFill`
/// divergence — one of the two kinds the `hybrid` policy AUTO-APPLIES
/// (`vike_tradehub::reconcile_config::auto_applied_kinds`), i.e. it would book the fill a second time
/// with no operator in front of it. Dropping is the safe direction: the worst case is a real fill
/// going unreconciled until a pass sees it with its id. Bybit V5 documents `execId` on every row.
pub fn parse_fills(body: &str) -> Result<Vec<FillReport>, String> {
    let rows = list_rows(body)?;
    Ok(rows
        .iter()
        .filter_map(|f| {
            let maker = f.get("isMaker").and_then(|b| b.as_bool()).unwrap_or(false);
            let trade_id = match TradeId::new(f.get("execId").map(json_str).unwrap_or_default()) {
                Ok(id) => id,
                Err(_) => {
                    tracing::warn!(
                        venue = VENUE,
                        symbol = %f.get("symbol").and_then(|s| s.as_str()).unwrap_or(""),
                        "execution/list row carries no `execId` — skipping the FillReport; an \
                         id-less row can never match `seen_trade_ids`, so it would fabricate a \
                         MissingFill divergence that `hybrid` auto-applies (booking it twice)"
                    );
                    return None;
                }
            };
            Some(FillReport {
                venue: VENUE.to_string(),
                symbol: f.get("symbol").and_then(|s| s.as_str()).unwrap_or("").to_string(),
                trade_id,
                venue_order_id: f.get("orderId").map(json_str).unwrap_or_default().into(),
                client_order_id: f
                    .get("orderLinkId")
                    .and_then(|c| c.as_str())
                    .filter(|c| !c.is_empty())
                    .map(|c| c.to_string()),
                side: side_sign(f.get("side").and_then(|s| s.as_str()).unwrap_or("")),
                last_qty: f.get("execQty").and_then(json_num).unwrap_or(0.0),
                last_px: f.get("execPrice").and_then(json_num).unwrap_or(0.0),
                commission: f.get("execFee").and_then(json_num).unwrap_or(0.0),
                commission_asset: f
                    .get("feeCurrency")
                    .and_then(|c| c.as_str())
                    .unwrap_or("")
                    .to_string(),
                liquidity_side: if maker { LiquiditySide::Maker } else { LiquiditySide::Taker },
                ts: f.get("execTime").and_then(json_int).unwrap_or(0),
            })
        })
        .collect())
}

/// `GET /v5/position/list` rows -> `PositionStatusReport`. `size` is UNSIGNED on the wire — the
/// SIGN comes from `side` ("Buy" -> +1, else -1), exactly like
/// `perp::BybitPerpRest::reconcile_positions`; a flat (`size == 0`) row's sign is forced to `0.0`
/// (never a signed zero) rather than trusting whatever `side` a flat leg happens to carry. Every
/// row is kept, including that flat leg — see the module doc for why filtering it out (as
/// `reconcile_positions`'s own snapshot builder does) would hide a real divergence here.
pub fn parse_positions(body: &str) -> Result<Vec<PositionStatusReport>, String> {
    let rows = list_rows(body)?;
    Ok(rows
        .iter()
        .map(|p| {
            let size = p.get("size").and_then(json_num).unwrap_or(0.0).abs();
            let sign =
                if p.get("side").and_then(|s| s.as_str()) == Some("Buy") { 1.0 } else { -1.0 };
            let qty = if size == 0.0 { 0.0 } else { sign * size };
            PositionStatusReport {
                venue: VENUE.to_string(),
                symbol: p.get("symbol").and_then(|s| s.as_str()).unwrap_or("").to_string(),
                position_side: crate::event_mapper::pside_from_idx(p),
                qty,
                avg_px: p.get("avgPrice").and_then(json_num).unwrap_or(0.0),
                ts: p.get("updatedTime").and_then(json_int).unwrap_or(0),
                margin_mode: parse_trade_mode(p),
                // Bybit v5 has no verified per-position isolated-WALLET field (`positionIM` is
                // plain initial margin, present for cross too; `positionBalance` is undocumented
                // for UTA) — deliberately left un-parsed rather than mislabeled. None always.
                // LIVE-PROBED 2026-07-19 (`tests/bybit_isolated_wallet_probe.rs`): demo blocks
                // both isolation paths (10032/110073), a CROSS row carries `positionBalance:"0"`
                // anyway, and current docs deprecate `positionBalance` (= `positionIM`) and
                // `tradeMode` (always 0 on UTA) — so None stays the verified pin.
                isolated_margin: None,
                delta: None,
            }
        })
        .collect())
}

/// Bybit v5 `tradeMode` (`0` = cross, `1` = isolated; number OR string on the wire) ->
/// [`MarginMode`] (margin-mode step-2, read-side only). Anything other than an explicit `1` —
/// including an absent field (UTA portfolio accounts omit a meaningful tradeMode) — is the
/// fail-safe `Cross`, byte-identical to pre-field behavior. Shared by [`parse_positions`] and
/// `perp::BybitPerpRest::reconcile_positions`'s snapshot builder.
pub fn parse_trade_mode(p: &Value) -> MarginMode {
    let isolated = match p.get("tradeMode") {
        Some(Value::Number(n)) => n.as_i64() == Some(1),
        Some(Value::String(s)) => s == "1",
        _ => false,
    };
    if isolated { MarginMode::Isolated } else { MarginMode::Cross }
}

/// `GET /v5/account/wallet-balance` {accountType:UNIFIED} `result.list[].coin[]` rows -> the
/// UNIFIED account's USDT `walletBalance` (total wallet cash — NOT `availableToWithdraw`, which
/// excludes margin currently locked by open positions/orders). Same field
/// `perp::BybitPerpRest::fetch_usdt_balance` already extracts for the legacy
/// `ReconcileSnapshot.balance` — factored out here as the pure, fixture-tested parser this
/// `ReconClient::fetch_balance` override delegates to. `None` when no USDT coin row is present;
/// malformed JSON (or a missing `list` array) is a hard `Err`, never a panic.
pub fn parse_wallet_balance(body: &str) -> Result<Option<f64>, String> {
    let v: Value = serde_json::from_str(body).map_err(|e| e.to_string())?;
    let list = v
        .get("list")
        .and_then(|l| l.as_array())
        .ok_or_else(|| "expected a `list` array".to_string())?;
    for acct in list {
        if let Some(coins) = acct.get("coin").and_then(|c| c.as_array())
            && let Some(row) =
                coins.iter().find(|c| c.get("coin").and_then(|x| x.as_str()) == Some("USDT"))
        {
            return Ok(row.get("walletBalance").and_then(json_num));
        }
    }
    Ok(None)
}

/// `GET /v5/account/fee-rate` `result.list[0]` -> live [`FeeSchedule`] from
/// `makerFeeRate`/`takerFeeRate` (fractions, e.g. `"0.00055"`). `Ok(None)` when the list is empty
/// or a rate is absent (fail-soft → static default); a missing `list` is a hard `Err` (via
/// [`list_rows`]).
pub fn parse_fee_rate(body: &str) -> Result<Option<FeeSchedule>, String> {
    let rows = list_rows(body)?;
    Ok(rows.first().and_then(|r| {
        let maker = r.get("makerFeeRate").and_then(json_num)?;
        let taker = r.get("takerFeeRate").and_then(json_num)?;
        Some(FeeSchedule::from_fractions(maker, taker))
    }))
}

// --- the factory -----------------------------------------------------------------------------

/// The venue -> `ReconClient` factory (ReconFactory seam, wave-2 task 6) — moved verbatim from
/// `vike_tradehub_core::build_recon_client`'s `"bybit"` match arm (called from `crate::adapter` since
/// docs/decisions/0096): a FRESH stateless-HMAC `ureq` REST
/// client dedicated to reconcile reads, reusing the SAME `Credentials` the venue's
/// `ExecutionClient`/`spawn_with_recorder` already builds from. Bybit's reconcile REST binds to
/// [`crate::perp::endpoints`]`(mainnet)` — `mainnet` being the SAME already-resolved ceiling
/// verdict (decision 0095: `live` means mainnet) the start threaded into the exec path, resolved
/// ONCE by `crate::adapter`'s `BybitVenueAdapter::start`, so reconcile stays in lockstep with exec
/// instead of re-reading anything for itself. Always `Some` —
/// construction here is pure/infallible (no network); `Option` is kept so the return type matches
/// every other bridge's `recon_client` (deribit's genuinely can fail).
pub fn recon_client(
    creds: &vike_secrets::credentials::Credentials,
    symbol: &str,
    mainnet: bool,
) -> Option<Box<dyn ReconClient>> {
    // Reconcile reads follow the SAME demo/mainnet host switch the exec path binds to, so a
    // mainnet start reconciles against the mainnet account; `false` ⇒ demo host, byte-identical to
    // before the switch existed.
    let (rest_base, _) = crate::perp::endpoints(mainnet);
    Some(Box::new(BybitReconClient::new(
        BybitV5Signer::new(creds, vike_model::now_ms),
        crate::transport::UreqBybitTransport::new()
            .with_rate_gate(crate::ratelimit::rest_rate_gate()),
        rest_base,
        symbol.to_string(),
    )))
}

// --- the client ----------------------------------------------------------------------------

/// One reconcile client per venue-symbol — the SAME per-symbol shape `perp::BybitPerpRest` uses,
/// but carrying only what a report fetch needs (signer + transport + base_url + symbol; no
/// order-formatting `SymbolProperties`/leverage).
pub struct BybitReconClient<T: BybitTransport> {
    pub signer: BybitV5Signer,
    pub transport: T,
    pub base_url: String,
    pub symbol: String,
    /// The symbols the engine admitted beyond `symbol` (`ReconClient::admit_symbol`), bare, as the
    /// bybit engine spells them.
    admitted: AdmittedSymbols,
    /// The pass-wide pause after a rate limit on an admitted read.
    pause: Pause,
}

impl<T: BybitTransport> BybitReconClient<T> {
    pub fn new(
        signer: BybitV5Signer,
        transport: T,
        base_url: impl Into<String>,
        symbol: impl Into<String>,
    ) -> Self {
        BybitReconClient {
            signer,
            transport,
            base_url: base_url.into(),
            symbol: symbol.into(),
            admitted: AdmittedSymbols::default(),
            pause: Pause::default(),
        }
    }

    /// `order/realtime` for ONE symbol: the request the client has always made for its own.
    fn orders_of(&self, symbol: &str) -> Result<Vec<OrderStatusReport>, ReadFail> {
        let body = self.result_raw(
            PATH_ORDER_REALTIME,
            &[("category", json!("linear")), ("symbol", json!(symbol))],
        )?;
        parse_orders(&body).map_err(ReadFail::Parse)
    }

    /// `execution/list` for ONE symbol since `since` (0 = no lower bound).
    fn fills_of(&self, symbol: &str, since: i64) -> Result<Vec<FillReport>, ReadFail> {
        let mut params: Vec<(&str, Value)> =
            vec![("category", json!("linear")), ("symbol", json!(symbol))];
        if since > 0 {
            params.push(("startTime", json!(since)));
        }
        let body = self.result_requery_raw(PATH_EXECUTION_LIST, &params)?;
        parse_fills(&body).map_err(ReadFail::Parse)
    }

    /// `position/list` for ONE symbol (the flat row stays present).
    fn positions_of(&self, symbol: &str) -> Result<Vec<PositionStatusReport>, ReadFail> {
        let body = self.result_raw(
            PATH_POSITION_LIST,
            &[("category", json!("linear")), ("symbol", json!(symbol))],
        )?;
        parse_positions(&body).map_err(ReadFail::Parse)
    }

    /// Judge the read of ONE admitted symbol. A symbol bybit no longer lists answers a parameter error
    /// naming the symbol (`10001`, "symbol invalid"): it holds nothing, so that is no rows, with a
    /// warning, and the account's other reads still go through (one delisted symbol must not blind the
    /// pass for good: the list never shrinks). A rate limit (the taxonomy's `RateLimited`, or the
    /// Cloudflare IP block bybit answers as `403`) starts the pass-wide pause. Anything else fails the
    /// whole fetch, as before. ⚠ The `10001` wording is bybit's documented one, not live-proven for a
    /// delisted linear symbol.
    fn admitted_rows<R>(
        &self,
        symbol: &str,
        read: Result<Vec<R>, ReadFail>,
    ) -> Result<Vec<R>, String> {
        match read {
            Err(ReadFail::Venue(e))
                if e.code == 10001 && e.msg.to_ascii_lowercase().contains("symbol") =>
            {
                tracing::warn!(
                    venue = VENUE, %symbol,
                    "reconcile: bybit does not list this admitted symbol ({}); reading it as \
                     holding nothing",
                    e.msg
                );
                Ok(Vec::new())
            }
            Err(ReadFail::Venue(e)) if e.kind() == ErrorKind::RateLimited || e.code == 403 => {
                self.pause.start();
                tracing::warn!(
                    venue = VENUE, %symbol,
                    "reconcile: rate limited reading an admitted symbol ({}); the pass is silent \
                     for {}s",
                    e.msg,
                    RATE_LIMIT_BACKOFF.as_secs()
                );
                Err(e.msg)
            }
            other => other.map_err(String::from),
        }
    }

    /// `Err` while the pass is paused after a rate limit on an admitted symbol. Only a client with
    /// admitted symbols ever pauses, so with none this is always `Ok`.
    fn not_backing_off(&self) -> Result<(), String> {
        match self.pause.left() {
            Some(left) => Err(format!(
                "{VENUE} reconcile is backing off for {}s after a rate-limit answer on an admitted \
                 symbol",
                left.as_secs() + 1
            )),
            None => Ok(()),
        }
    }

    /// The normal signed GET path, envelope-unwrapped to `result` and re-serialized once for the
    /// pure parsers — the same `signed` + `unwrap_envelope` pairing `BybitPerpRest::call` already
    /// uses for order/position reads.
    fn result(&self, path: &str, params: &[(&str, Value)]) -> Result<String, String> {
        self.result_raw(path, params).map_err(|e| e.msg)
    }

    /// [`Self::result`] keeping the venue's error (code and message), for a read that judges it.
    fn result_raw(&self, path: &str, params: &[(&str, Value)]) -> Result<String, VenueApiError> {
        let resp = self.transport.signed(&self.base_url, path, "GET", params, &self.signer)?;
        unwrap_envelope(resp).map(|v| v.to_string())
    }

    /// The short-timeout, rate-gate-EXEMPT path — the SAME one the crate's audit-A3 resync
    /// (`get_execution_history`) already uses for this exact heavier history endpoint, so a
    /// resync burst can't blow the per-symbol order-rate budget submit/cancel depend on.
    fn result_requery_raw(
        &self,
        path: &str,
        params: &[(&str, Value)],
    ) -> Result<String, VenueApiError> {
        let resp =
            self.transport.signed_requery(&self.base_url, path, "GET", params, &self.signer)?;
        unwrap_envelope(resp).map(|v| v.to_string())
    }
}

/// How long a pass stays silent after an admitted read was answered a rate limit. The transport
/// keeps no `Retry-After`, so the pause is fixed. (A scheduling clock, kept in this venue crate:
/// `vike-exec`, which owns the admitted list, reads no clock.)
const RATE_LIMIT_BACKOFF: Duration = Duration::from_secs(60);

/// The pass-wide pause: nothing is sent until it lapses. Never started with nothing admitted.
#[derive(Debug, Default)]
struct Pause(Mutex<Option<Instant>>);

impl Pause {
    fn start(&self) {
        *self.0.lock().unwrap_or_else(PoisonError::into_inner) =
            Some(Instant::now() + RATE_LIMIT_BACKOFF);
    }

    /// How much longer the pause lasts, or `None` when there is none (or it lapsed).
    fn left(&self) -> Option<Duration> {
        let until = (*self.0.lock().unwrap_or_else(PoisonError::into_inner))?;
        until.checked_duration_since(Instant::now()).filter(|left| !left.is_zero())
    }
}

/// Why one report read failed: the venue answered an error (kept whole, so a read of an admitted
/// symbol can judge its code) or its body did not parse. Both reach the caller as the same text the
/// mounted symbol's read has always produced (the venue's message).
enum ReadFail {
    Venue(VenueApiError),
    Parse(String),
}

impl From<VenueApiError> for ReadFail {
    fn from(e: VenueApiError) -> Self {
        ReadFail::Venue(e)
    }
}

impl From<ReadFail> for String {
    fn from(f: ReadFail) -> String {
        match f {
            ReadFail::Venue(e) => e.msg,
            ReadFail::Parse(s) => s,
        }
    }
}

impl<T: BybitTransport + Send> ReconClient for BybitReconClient<T> {
    /// `since` is a no-op here: `/v5/order/realtime` has no time filter and reports only the
    /// CURRENTLY open + recently-closed set — a fully-closed order that fell out of this set is
    /// exactly what the fill/position reports (not this one) reconcile.
    ///
    /// **Every ADMITTED symbol is read too**, one `order/realtime?symbol=` each, the mounted symbol's
    /// own request shape. The `settleCoin`-wide form would be one call but is a different report (it
    /// lists open orders only, where this one also carries recently closed ones), and any failure
    /// fails the whole fetch: the restore planner reads an order a successful report lacks as gone.
    /// With none admitted the request list is the mounted symbol's alone.
    fn fetch_order_status_reports(&self, _since: i64) -> Result<Vec<OrderStatusReport>, String> {
        self.not_backing_off()?;
        let mut reports = self.orders_of(&self.symbol).map_err(String::from)?;
        for symbol in self.admitted.symbols() {
            let rows = self.admitted_rows(&symbol, self.orders_of(&symbol))?;
            reports.extend(rows.into_iter().filter(|r| r.symbol.eq_ignore_ascii_case(&symbol)));
        }
        Ok(reports)
    }

    /// Bybit's `execId` is unique account-wide and the user-data stream folds it bare, so an
    /// admitted symbol's fill keeps its id as is (unlike binance's per-symbol `t`).
    fn fetch_fill_reports(&self, since: i64) -> Result<Vec<FillReport>, String> {
        self.not_backing_off()?;
        let mut reports = self.fills_of(&self.symbol, since).map_err(String::from)?;
        for symbol in self.admitted.symbols() {
            let rows = self.admitted_rows(&symbol, self.fills_of(&symbol, since))?;
            reports.extend(rows.into_iter().filter(|r| r.symbol.eq_ignore_ascii_case(&symbol)));
        }
        Ok(reports)
    }

    fn fetch_position_status_reports(&self) -> Result<Vec<PositionStatusReport>, String> {
        self.not_backing_off()?;
        let mut reports = self.positions_of(&self.symbol).map_err(String::from)?;
        for symbol in self.admitted.symbols() {
            let rows = self.admitted_rows(&symbol, self.positions_of(&symbol))?;
            reports.extend(rows.into_iter().filter(|r| r.symbol.eq_ignore_ascii_case(&symbol)));
        }
        Ok(reports)
    }

    /// Remember that the engine admitted `symbol` (bybit's engine spelling is the bare symbol; a
    /// `.P` is tolerated and stripped), so every later pass reads it too. The mounted symbol, an
    /// empty one, a repeat and a 17th are ignored (`AdmittedSymbols::admit`).
    fn admit_symbol(&self, symbol: &str) {
        let upper = symbol.to_ascii_uppercase();
        let bare = vike_catalog::split_perp(&upper).0;
        if self.admitted.admit(bare, &self.symbol) {
            tracing::info!(
                venue = VENUE, symbol = %bare,
                "reconcile: the engine admitted a symbol; every pass now reads it too"
            );
        }
    }

    /// Balance meaning (pinned per the task brief): the UNIFIED account's USDT `walletBalance`
    /// from `/v5/account/wallet-balance` — same field `perp::fetch_usdt_balance` uses, matching
    /// binance perp's "wallet balance, not available" choice (Bybit's UNIFIED account has no
    /// separate spot/perp split the way binance does). A light, unfiltered account read — rides
    /// the normal `signed`/`result` (rate-gated) path, same as `order/realtime`/`position/list`.
    fn fetch_balance(&self) -> Result<Option<f64>, String> {
        let body = self.result(PATH_ACCOUNT, &[("accountType", json!("UNIFIED"))])?;
        parse_wallet_balance(&body)
    }

    /// Live account-actual fee rates (fee model 4/5): `/v5/account/fee-rate` for this linear
    /// symbol — `makerFeeRate`/`takerFeeRate` fractions. A light, unfiltered account read — rides
    /// the normal `signed`/`result` (rate-gated) path. `Ok(None)` keeps the static default.
    fn fetch_fee_rates(&self) -> Result<Option<FeeSchedule>, String> {
        let body = self.result(
            PATH_FEE_RATE,
            &[("category", json!("linear")), ("symbol", json!(self.symbol))],
        )?;
        parse_fee_rate(&body)
    }
}
