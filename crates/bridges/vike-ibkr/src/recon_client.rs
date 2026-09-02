//! IBKR `ReconClient` (recon-breadth: IBKR was the one live-mountable venue with NO report-fetch
//! seam) — the venue-facing report seam (`vike_exec::recon::ReconClient`) over the **cpapi**
//! (Client Portal Web API) REST surface. This is genuinely new: IBKR's existing "reconcile" is the
//! [`crate::event_mapper`] FSM fill-modelling (exec-before-open synthesis, coid⇄orderId cleanup) —
//! the LIVE order-event correctness path — NOT a `fetch_*` report seam the reconcile engine diffs
//! against local state. This module is that seam.
//!
//! ## Why cpapi (not the socket backend)
//! cpapi gives the cleanest report fetch: four flat REST endpoints — orders
//! (`GET /iserver/account/orders`), positions (`GET /portfolio/{acct}/positions/{page}`), fills
//! (`GET /iserver/account/trades`), balance (`GET /portfolio/{acct}/ledger`) — each returning
//! plain JSON a pure parser maps straight to the report types. It is also the validated-Linux
//! backend, and it rides the crate's existing pure-Rust transport stack ([`CpapiRest`], the same
//! rate-gated loopback-insecure `ureq` agent the exec side uses), so it pulls in NO new dep. The
//! socket (ibapi) backend would need a live-order-update subscription to surface open orders, which
//! is a poorer fit for a periodic blocking fetch. The reconcile connection is its OWN dedicated
//! [`CpapiRest`] (never shares the exec transport), the same "dedicated reconcile reads" discipline
//! binance/deribit/ctrader use.
//!
//! ## Per-symbol, conId-filtered (the ctrader/binance-perp shape)
//! One [`IbkrReconClient`] per (account, canonical vike symbol e.g. `AAPL.SMART.USD`). The symbol's
//! numeric `conId` is resolved ONCE at [`IbkrReconClient::connect`] (via `secdef/search`, reusing
//! the transport's [`crate::transport::cpapi_decode::decode_conid`]); every report fetch then filters
//! the account-wide rows to that conId and stamps the canonical symbol back. A single-symbol client
//! mirrors ctrader's mounted-symbol filter and binance-perp's per-symbol positionRisk — the
//! reconcile driver runs one per mounted instrument. Positions synthesize a FLAT zero row when the
//! symbol is absent (cpapi, like OKX/ctrader, OMITS a closed position rather than sending a zero
//! row), so `recon::diff::diff` can still detect a stale LOCAL position the venue has since closed.
//!
//! ## Wire shapes LIVE-VERIFIED (2026-08-23); the PARSERS remain the proven deliverable
//! Every wire→report mapping is a PURE free function (`parse_*`) over the raw JSON body, fixture-
//! tested offline in `tests/ibkr_reconcile_parse.rs` (synthetic bodies — no network). The four
//! endpoints those parsers read have now been exercised against the CI box's live authenticated gateway
//! (DUQ186573) and answer the shapes the fixtures assume — `transport/cpapi/endpoints.rs`'s module
//! doc carries the response table, and `tests/ibkr_reconcile_smoke.rs` is the read-only vehicle that
//! runs the parsers over them end to end. Status normalization reuses the ONE IB-status vocabulary
//! ([`crate::order::OrderStatusKind::from_ib`]) the exec path already speaks.
//!
//! ⚠ The live pass was against a FLAT account, so `orders`/`trades`/`positions` were verified as
//! reachable and correctly-shaped-when-empty; the POPULATED row shapes are still only fixture-
//! proven. A reconcile run with a real open position is what would retire that.

use vike_bridge_core::json::json_num;
use vike_exec::recon::ReconClient;
use vike_model::events::{LiquiditySide, PositionSide, TradeId};
use vike_model::{FillReport, OrderStatusReport, PositionStatusReport};

use crate::config::IbkrConfig;
use crate::contract::parse_simplified;
use crate::error::IbkrError;
use crate::order::OrderStatusKind;
use crate::transport::cpapi_decode::decode_conid;
use crate::transport::CpapiRest;

/// The venue key stamped on every report row.
pub const VENUE: &str = "ibkr";

// --- pure parsers (over the raw cpapi JSON bodies — the fixture-tested units) --------------------

/// `side` string → signed side. cpapi spells it `"BUY"`/`"SELL"` on the orders endpoint but
/// `"B"`/`"S"` (and `"BOT"`/`"SLD"`) on the trades endpoint — a leading `B` (any case) is a buy,
/// everything else a sell. Never panics on an empty/unknown value (→ sell, the conservative -1).
fn side_sign(s: &str) -> i32 {
    if s.trim().to_ascii_uppercase().starts_with('B') {
        1
    } else {
        -1
    }
}

/// The row's numeric `conid` (cpapi returns it as a JSON number on orders/positions/trades; accept a
/// string too for robustness, the same string-or-number tolerance [`decode_conid`] needed live).
/// Absent/unparseable → `0` (never matches a real resolved conId, so such a row is filtered out).
fn row_conid(row: &serde_json::Value) -> i64 {
    row.get("conid")
        .and_then(|c| c.as_i64().or_else(|| c.as_str()?.trim().parse().ok()))
        .unwrap_or(0)
}

fn str_field(row: &serde_json::Value, key: &str) -> String {
    row.get(key).and_then(|v| v.as_str()).unwrap_or("").to_string()
}

fn num_field(row: &serde_json::Value, key: &str) -> f64 {
    row.get(key).and_then(json_num).unwrap_or(0.0)
}

/// Empty string → `None` (an externally-placed order the venue never echoed our client id for),
/// preferring cpapi's `order_ref` and falling back to the WS-frame `cOID` spelling.
fn client_order_id(row: &serde_json::Value) -> Option<String> {
    for key in ["order_ref", "cOID"] {
        let v = str_field(row, key);
        if !v.is_empty() {
            return Some(v);
        }
    }
    None
}

/// Normalize a cpapi/IB order-status string to the `OrderStatus::parse` FSM vocabulary
/// `recon::diff::diff` reads, reusing the ONE IB-status classifier
/// ([`OrderStatusKind::from_ib`]) the exec path already speaks. An ACTIVE status
/// (`Submitted`/`PreSubmitted`/`PendingSubmit`) with fill progress `0 < filled < total` derives
/// `PARTIALLY_FILLED` (the SAME fill-progress split ctrader/hyperliquid use for a venue whose
/// resting status has no distinct partial value). `Inactive`/unknown → `REJECTED` (a non-open
/// terminal — the safe reconcile classification for an order that is neither resting nor filled).
pub fn normalize_order_status(raw: &str, filled: f64, total: f64) -> String {
    match OrderStatusKind::from_ib(raw) {
        OrderStatusKind::Filled => "FILLED",
        OrderStatusKind::Cancelled | OrderStatusKind::ApiCancelled => "CANCELED",
        OrderStatusKind::PendingCancel => "PENDING_CANCEL",
        OrderStatusKind::PreSubmitted
        | OrderStatusKind::Submitted
        | OrderStatusKind::PendingSubmit
        | OrderStatusKind::ApiPending => {
            if filled > 0.0 && filled < total {
                "PARTIALLY_FILLED"
            } else {
                "ACCEPTED"
            }
        }
        OrderStatusKind::Inactive => "REJECTED",
    }
    .to_string()
}

/// `GET /iserver/account/orders` `{"orders":[..]}` → `OrderStatusReport`, filtered to `conid` and
/// stamped with the canonical `symbol`. `orderId` → `venue_order_id`; `order_ref`/`cOID` →
/// `client_order_id`; `side` (`BUY`/`SELL`) → ±1; `orderType` lower-cased; `totalSize` → qty,
/// `filledQuantity` → filled; `avgPrice` → avg_px; `lastExecutionTime_r` (epoch-ms) → ts.
pub fn parse_order_reports(
    body: &str,
    conid: i64,
    symbol: &str,
) -> Result<Vec<OrderStatusReport>, String> {
    let v: serde_json::Value = serde_json::from_str(body).map_err(|e| e.to_string())?;
    let rows = v.get("orders").and_then(|o| o.as_array()).ok_or("expected an `orders` array")?;
    Ok(rows
        .iter()
        .filter(|o| row_conid(o) == conid)
        .map(|o| {
            let qty = num_field(o, "totalSize");
            let filled_qty = num_field(o, "filledQuantity");
            let raw_status = str_field(o, "status");
            OrderStatusReport {
                venue: VENUE.to_string(),
                symbol: symbol.to_string(),
                venue_order_id: json_id(o.get("orderId")).into(),
                client_order_id: client_order_id(o),
                side: side_sign(&str_field(o, "side")),
                order_type: str_field(o, "orderType").to_ascii_lowercase(),
                qty,
                filled_qty,
                avg_px: num_field(o, "avgPrice"),
                status: normalize_order_status(&raw_status, filled_qty, qty),
                ts: o.get("lastExecutionTime_r").and_then(|t| t.as_i64()).unwrap_or(0),
            }
        })
        .collect())
}

/// `GET /iserver/account/trades` `[..]` → `FillReport`, filtered to `conid` and stamped with
/// the canonical `symbol`. `execution_id` → `trade_id`; `side` (`B`/`S`) → ±1; `size`/`price` →
/// qty/px; `commission`(+`commission_currency`) best-effort (the /trades row often omits it → 0.0 /
/// `"USD"`); `trade_time_r` (epoch-ms) → ts. `liquidity_side` is always `Unknown` (cpapi /trades
/// carries no maker/taker flag); `venue_order_id` is read when present (some gateway builds omit it
/// on this endpoint) — matching then falls back to `client_order_id`, the same convention every
/// externally-sourced fill uses.
///
/// A row with no `execution_id` is **SKIPPED**, not reported with an empty id. That id is the key
/// `vike_exec::recon::diff` looks up in `seen_trade_ids`; an id-less report can never match, so it
/// does not merely fail to dedup — it FABRICATES a `MissingFill` divergence, and `MissingFill` is
/// one of the two kinds the `hybrid` policy AUTO-APPLIES, so the invented divergence books the fill
/// again unattended. Skipping costs at most one pass of visibility (the next pass re-reads the same
/// window). Nothing is synthesized: `orderId` is per-ORDER, so an orderId-keyed report would
/// collapse a partially-filled order's legs into one and lose real economics.
pub fn parse_fill_reports(body: &str, conid: i64, symbol: &str) -> Result<Vec<FillReport>, String> {
    let v: serde_json::Value = serde_json::from_str(body).map_err(|e| e.to_string())?;
    let rows = v.as_array().ok_or("expected a JSON array")?;
    let mut skipped = 0usize;
    let out: Vec<FillReport> = rows
        .iter()
        .filter(|t| row_conid(t) == conid)
        .filter_map(|t| {
            let trade_id = match TradeId::new(str_field(t, "execution_id")) {
                Ok(t) => t,
                Err(_) => {
                    skipped += 1;
                    return None;
                }
            };
            let commission_asset = {
                let c = str_field(t, "commission_currency");
                if c.is_empty() {
                    "USD".to_string()
                } else {
                    c
                }
            };
            Some(FillReport {
                venue: VENUE.to_string(),
                symbol: symbol.to_string(),
                trade_id,
                venue_order_id: json_id(t.get("orderId")).into(),
                client_order_id: client_order_id(t),
                side: side_sign(&str_field(t, "side")),
                last_qty: num_field(t, "size"),
                last_px: num_field(t, "price"),
                commission: num_field(t, "commission"),
                commission_asset,
                liquidity_side: LiquiditySide::Unknown,
                ts: t.get("trade_time_r").and_then(|x| x.as_i64()).unwrap_or(0),
            })
        })
        .collect();
    if skipped > 0 {
        tracing::warn!(
            venue = VENUE,
            skipped,
            conid,
            "reconcile /iserver/account/trades rows carry no `execution_id` — skipped; an id-less \
             FillReport cannot match `seen_trade_ids` and would manufacture a MissingFill \
             divergence that `hybrid` auto-applies (double-booking the fill)"
        );
    }
    Ok(out)
}

/// `GET /portfolio/{acct}/positions/{page}` `[..]` → `PositionStatusReport`, filtered to `conid` and
/// stamped with the canonical `symbol`. `position` is ALREADY SIGNED (long > 0, short < 0);
/// `avgPrice` (fallback `avgCost`) → avg_px. IBKR positions are NET (one-way, account-level Reg-T
/// margin) so `position_side` is `Both` and the SIGN of qty carries direction — the same net/spot
/// convention binance-perp's one-way rows use. `margin_mode`/`isolated_margin` stay the Cross/None
/// defaults (IBKR has no per-position isolated wallet). When NO row matches, the caller synthesizes
/// a flat zero row (see [`IbkrReconClient::fetch_position_status_reports`]).
pub fn parse_position_reports(
    body: &str,
    conid: i64,
    symbol: &str,
) -> Result<Vec<PositionStatusReport>, String> {
    let v: serde_json::Value = serde_json::from_str(body).map_err(|e| e.to_string())?;
    let rows = v.as_array().ok_or("expected a JSON array")?;
    Ok(rows
        .iter()
        .filter(|p| row_conid(p) == conid)
        .map(|p| PositionStatusReport {
            venue: VENUE.to_string(),
            symbol: symbol.to_string(),
            position_side: PositionSide::Both,
            qty: num_field(p, "position"), // ALREADY SIGNED
            avg_px: p.get("avgPrice").and_then(json_num).unwrap_or_else(|| num_field(p, "avgCost")),
            ts: 0, // the positions row carries no per-position timestamp
            margin_mode: vike_model::MarginMode::default(),
            isolated_margin: None,
            delta: None,
        })
        .collect())
}

/// `GET /portfolio/{acct}/ledger` `{"USD":{"cashbalance":..},"BASE":{..}}` → the quote `currency`'s
/// `cashbalance` (settled + unsettled cash — the account's realized cash truth in that currency),
/// falling back to the `BASE` (account base-currency) row when the specific currency key is absent.
/// `None` when neither is present. Malformed JSON is a hard `Err`, never a panic.
pub fn parse_balance(body: &str, currency: &str) -> Result<Option<f64>, String> {
    let v: serde_json::Value = serde_json::from_str(body).map_err(|e| e.to_string())?;
    let cash = |key: &str| v.get(key).and_then(|c| c.get("cashbalance")).and_then(json_num);
    Ok(cash(currency).or_else(|| cash("BASE")))
}

/// An id that may arrive as a JSON number OR string → its string form (`""` when absent) — the twin
/// of the family recon `json_id`, so `orderId: 123` and `orderId: "123"` both round-trip.
fn json_id(v: Option<&serde_json::Value>) -> String {
    match v {
        Some(serde_json::Value::String(s)) => s.clone(),
        Some(serde_json::Value::Number(n)) => n.to_string(),
        _ => String::new(),
    }
}

// --- the client ----------------------------------------------------------------------------------

/// One reconcile client per (account, canonical vike symbol), holding its OWN dedicated
/// [`CpapiRest`] (never the exec transport's) and the symbol's resolved `conId`. See the module doc
/// for why cpapi + per-symbol + conId-filter is the shape. `ReconClient`'s methods take `&self` and
/// the underlying `ureq` agent is `Send + Sync`, so no interior mutability is needed.
pub struct IbkrReconClient {
    rest: CpapiRest,
    /// Canonical vike symbol (e.g. `AAPL.SMART.USD`) stamped on every report row.
    symbol: String,
    /// Numeric conId the report rows are filtered to (resolved once at [`Self::connect`]).
    conid: i64,
    /// Quote currency for the ledger balance read (the contract's `currency`, e.g. `USD`).
    currency: String,
}

impl IbkrReconClient {
    /// Open a DEDICATED cpapi reconcile client: build a fresh [`CpapiRest`] over `cfg.cpapi_url`/
    /// `cfg.account`, probe readiness (`tickle` — gateway up + browser-authenticated), and resolve
    /// `symbol`'s conId via `secdef/search`. `Err` when the symbol is unmappable, the gateway is
    /// unreachable/unauthenticated, or the conId cannot be resolved — the reconcile driver then
    /// leaves this venue-symbol unwired, exec unaffected (the same graceful degradation every other
    /// absent-recon path uses). Never shares the exec side's transport.
    pub fn connect(cfg: &IbkrConfig, symbol: &str) -> Result<IbkrReconClient, IbkrError> {
        let contract = parse_simplified(symbol)
            .ok_or_else(|| IbkrError::Unsupported(format!("unmappable IBKR symbol: {symbol}")))?;
        let rest = CpapiRest::new(&cfg.cpapi_url, &cfg.account);
        rest.tickle().map_err(|e| IbkrError::Connect(format!("cpapi tickle: {}", e.msg)))?;
        let search = rest
            .secdef_search(&contract.symbol, contract.sec_type.as_ib_code())
            .map_err(|e| IbkrError::Connect(format!("cpapi secdef_search: {}", e.msg)))?;
        let conid = decode_conid(&search)
            .ok_or_else(|| IbkrError::Unsupported(format!("no conId for IBKR symbol {symbol}")))?;
        Ok(IbkrReconClient { rest, symbol: symbol.to_string(), conid, currency: contract.currency })
    }
}

impl ReconClient for IbkrReconClient {
    /// `_since` is a no-op: `/iserver/account/orders` reports only the CURRENTLY live set (no time
    /// filter) — a closed order that fell out of it is what the fill/position reports reconcile.
    fn fetch_order_status_reports(&self, _since: i64) -> Result<Vec<OrderStatusReport>, String> {
        let body = self.rest.open_orders().map_err(|e| e.msg)?.to_string();
        parse_order_reports(&body, self.conid, &self.symbol)
    }

    /// `_since` is a no-op: the cpapi `/trades` endpoint returns a fixed recent-executions window
    /// (typically the last day) with no server-side time filter — the reconcile lookback is bounded
    /// by that window, not by `since`.
    fn fetch_fill_reports(&self, _since: i64) -> Result<Vec<FillReport>, String> {
        let body = self.rest.executions().map_err(|e| e.msg)?.to_string();
        parse_fill_reports(&body, self.conid, &self.symbol)
    }

    /// Page-0 positions filtered to this symbol's conId. cpapi OMITS a closed position (no zero
    /// row), so an empty match synthesizes a FLAT `qty == 0` row — load-bearing: `recon::diff::diff`
    /// needs the row PRESENT to detect a stale LOCAL position the venue has since closed (the same
    /// synthesize-flat contract ctrader/okx use).
    fn fetch_position_status_reports(&self) -> Result<Vec<PositionStatusReport>, String> {
        let body = self.rest.positions(0).map_err(|e| e.msg)?.to_string();
        let matched = parse_position_reports(&body, self.conid, &self.symbol)?;
        if matched.is_empty() {
            return Ok(vec![PositionStatusReport::flat(VENUE, &self.symbol)]);
        }
        Ok(matched)
    }

    /// The account cash ledger's `cashbalance` for this contract's quote currency (falling back to
    /// the base-currency row) — the account's realized cash truth `diff_balance` reconciles against.
    fn fetch_balance(&self) -> Result<Option<f64>, String> {
        let body = self.rest.ledger().map_err(|e| e.msg)?.to_string();
        parse_balance(&body, &self.currency)
    }
}

// --- the factory ---------------------------------------------------------------------------------

/// The venue → `ReconClient` factory (the ReconFactory seam) for IBKR: opens [`IbkrReconClient`]'s
/// own dedicated cpapi reconcile client from an already-resolved [`IbkrConfig`]. `None` on any
/// connect failure (unmappable symbol / unreachable gateway / unresolved conId) — reconcile stays
/// unwired for this venue-symbol, exec unaffected.
///
/// **Not yet wired into `vike_mount::make_engine`** — that is a separate follow-up (as for the other
/// venues' factories), and it needs a running CP Gateway to live-validate. This factory is the seam
/// that follow-up calls.
pub fn recon_client(cfg: &IbkrConfig, symbol: &str) -> Option<Box<dyn ReconClient>> {
    match IbkrReconClient::connect(cfg, symbol) {
        Ok(c) => Some(Box::new(c) as Box<dyn ReconClient>),
        Err(e) => {
            tracing::warn!(target: "vike_ibkr::recon", symbol, error = %e, "IBKR recon client unwired");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // A couple of inline sanity checks for the tiny helpers; the exhaustive body-parsing coverage
    // (synthetic JSON per endpoint) lives in `tests/ibkr_reconcile_parse.rs`, mirroring the
    // ctrader/binance recon parse tests.

    #[test]
    fn side_sign_handles_both_spellings() {
        assert_eq!(side_sign("BUY"), 1);
        assert_eq!(side_sign("B"), 1);
        assert_eq!(side_sign("BOT"), 1);
        assert_eq!(side_sign("SELL"), -1);
        assert_eq!(side_sign("S"), -1);
        assert_eq!(side_sign("SLD"), -1);
        assert_eq!(side_sign(""), -1); // conservative default, never panics
    }

    #[test]
    fn active_with_partial_progress_is_partially_filled() {
        assert_eq!(normalize_order_status("Submitted", 0.0, 10.0), "ACCEPTED");
        assert_eq!(normalize_order_status("Submitted", 4.0, 10.0), "PARTIALLY_FILLED");
        assert_eq!(normalize_order_status("Filled", 10.0, 10.0), "FILLED");
        assert_eq!(normalize_order_status("Cancelled", 0.0, 10.0), "CANCELED");
        assert_eq!(normalize_order_status("Inactive", 0.0, 10.0), "REJECTED");
    }
}
