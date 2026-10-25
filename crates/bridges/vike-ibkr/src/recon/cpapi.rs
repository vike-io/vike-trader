//! IBKR `ReconClient` (recon-breadth: IBKR was the one live-mountable venue with NO report-fetch
//! seam) — the venue-facing report seam (`vike_exec::recon::ReconClient`) over the **cpapi**
//! (Client Portal Web API) REST surface. This is genuinely new: IBKR's existing "reconcile" is the
//! [`crate::exec::event_mapper`] FSM fill-modelling (exec-before-open synthesis, coid⇄orderId cleanup) —
//! the LIVE order-event correctness path — NOT a `fetch_*` report seam the reconcile engine diffs
//! against local state. This module is that seam.
//!
//! ## The CPAPI reconcile client, for a cpapi exec ONLY
//! This is the reconcile client of a start whose exec backend is `cpapi`: it talks to the SAME
//! Client Portal Gateway at `cpapi_url` the exec does, so it is the exec's own session. A SOCKET
//! exec gets the socket twin instead (`crate::recon::socket::IbkrSocketReconClient`, another API
//! client id on the exec's IB Gateway): `crate::adapter`'s `start_with` picks by `cfg.backend`, on both
//! tiers, and never builds this client beside a socket exec — a Client Portal session next to an IB
//! Gateway one on the same account evicts one of them (`docs/ops/ibkr-socket-gateway.md` §5, decision
//! 0140). [`connect`](IbkrReconClient::connect) does not check the backend itself; the start's
//! choice is the guard.
//!
//! cpapi's report fetch is four flat REST endpoints — orders (`GET /iserver/account/orders`),
//! positions (`GET /portfolio/{acct}/positions/{page}`), fills (`GET /iserver/account/trades`),
//! balance (`GET /portfolio/{acct}/ledger`) — each returning plain JSON a pure parser maps straight
//! to the report types, over the crate's existing pure-Rust transport stack ([`CpapiRest`], the same
//! rate-gated loopback-insecure `ureq` agent the exec side uses). The reconcile connection is its OWN
//! dedicated [`CpapiRest`] (its own HTTP agent, never the exec transport), the same "dedicated
//! reconcile reads" discipline binance/deribit/ctrader use.
//!
//! ## Per-symbol, conId-filtered (the ctrader/binance-perp shape)
//! One [`IbkrReconClient`] per (account, canonical vike symbol e.g. `AAPL.SMART.USD`). The symbol's
//! numeric `conId` is resolved ONCE at [`IbkrReconClient::connect`] (via `secdef/search`, reusing
//! the transport's [`crate::transport::cpapi_decode::decode_conid`]); every report fetch then filters
//! the account-wide rows to that conId and stamps the canonical symbol back. A single-symbol client
//! mirrors ctrader's mounted-symbol filter and binance-perp's per-symbol positionRisk — the
//! reconcile driver runs one per engine instrument. Positions synthesize a FLAT zero row when the
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
//! proven, EXCEPT one orders-list row (paper, 2026-10-17): it has no `totalSize` and no `status`, so the quantity
//! is read from `sizeAndFills`/`remainingQuantity`/`filledQuantity` and the status from `order_ccp_status`
//! (`parse_order_reports`). The `/trades`, positions and ledger rows stay uncaptured.

use vike_bridge_core::json::{get_f64, get_f64_opt, json_num};
use vike_exec::recon::ReconClient;
use vike_model::events::{LiquiditySide, PositionSide, TradeId};
use vike_model::{FillReport, OrderStatus, OrderStatusReport, OrderType, PositionStatusReport};

use crate::config::IbkrConfig;
use crate::contract::parse_simplified;
use crate::error::IbkrError;
use crate::order::OrderStatusKind;
use crate::transport::CpapiRest;
use crate::transport::cpapi_decode::{decode_conid, row_client_order_id};

/// The venue key stamped on every report row.
pub const VENUE: &str = "ibkr";

// --- pure parsers (over the raw cpapi JSON bodies — the fixture-tested units) --------------------

/// `side` string → signed side. cpapi spells it `"BUY"`/`"SELL"` on the orders endpoint but
/// `"B"`/`"S"` (and `"BOT"`/`"SLD"`) on the trades endpoint — a leading `B` (any case) is a buy,
/// everything else a sell. Never panics on an empty/unknown value (→ sell, the conservative -1).
fn side_sign(s: &str) -> i32 {
    if s.trim().to_ascii_uppercase().starts_with('B') { 1 } else { -1 }
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

/// Map a cpapi/IB order-status string onto the FSM [`OrderStatus`] `recon::diff::diff` reads,
/// reusing the ONE IB-status classifier ([`OrderStatusKind::from_ib`]) the exec path already speaks.
/// Every word IB documents, explicitly:
///
/// - `PreSubmitted`, `Submitted`, `PendingSubmit`, `ApiPending` → `Accepted`, or `PartiallyFilled`
///   when fill progress is `0 < filled < total` (the SAME fill-progress split ctrader/hyperliquid
///   use for a venue whose resting status has no distinct partial value).
/// - `PendingCancel` → `PendingCancel`; `Filled` → `Filled`; `Cancelled`/`Canceled` and
///   `ApiCancelled`/`ApiCanceled` → `Canceled`.
/// - `Inactive` → `Rejected` (a non-open terminal — the order is neither resting nor filled), and
///   the self-describing `Rejected` with it.
///
/// A word outside that list is `Rejected` too: the classifier folds it into `Inactive`, exactly as
/// the exec path reads it, so recon and exec agree. It is WARNED, so an unknown word is visible.
pub fn normalize_order_status(raw: &str, filled: f64, total: f64) -> OrderStatus {
    let resting = if filled > 0.0 && filled < total {
        OrderStatus::PartiallyFilled
    } else {
        OrderStatus::Accepted
    };
    match OrderStatusKind::from_ib(raw) {
        OrderStatusKind::Filled => OrderStatus::Filled,
        OrderStatusKind::Cancelled | OrderStatusKind::ApiCancelled => OrderStatus::Canceled,
        OrderStatusKind::PendingCancel => OrderStatus::PendingCancel,
        OrderStatusKind::PreSubmitted
        | OrderStatusKind::Submitted
        | OrderStatusKind::PendingSubmit
        | OrderStatusKind::ApiPending => resting,
        OrderStatusKind::Inactive => {
            if raw != "Inactive" && raw != "Rejected" {
                tracing::warn!(
                    venue = VENUE,
                    status = raw,
                    "unknown order status word — reporting the order as Rejected"
                );
            }
            OrderStatus::Rejected
        }
    }
}

/// Map a cpapi `orderType` onto [`OrderType`]. cpapi reports it in words (`Limit`, `Market`,
/// `Stop`, `Stop Limit`) and accepts the IB codes the exec builder sends (`LMT`, `MKT`, `STP`,
/// `STP LMT`); both spellings, in any case, map to their twins. Any other kind (`TRAIL`, `MIT`,
/// `MOC`, …) has none and stays [`OrderType::Unrecognized`] lower-cased, as every report spelled it
/// before the type existed.
pub(crate) fn normalize_order_type(raw: &str) -> OrderType {
    let word = raw.to_ascii_lowercase();
    match word.as_str() {
        "limit" | "lmt" => OrderType::Limit,
        "market" | "mkt" => OrderType::Market,
        "stop" | "stp" => OrderType::Stop,
        "stop limit" | "stp lmt" => OrderType::StopLimit,
        _ => OrderType::Unrecognized(word),
    }
}

/// The account a cpapi row names: orders carry `acct`, trades `account` (some builds
/// `accountCode`). `""` when the row names none.
fn row_account(row: &serde_json::Value) -> String {
    for key in ["acct", "account", "accountCode"] {
        let v = str_field(row, key);
        if !v.is_empty() {
            return v;
        }
    }
    String::new()
}

/// Whether a row is this client's account's: a row that NAMES another account is not, a row that
/// names none is kept. `/iserver/account/orders` and `/iserver/account/trades` are account-FREE
/// paths that answer for every account the session holds, so on a multi-account login this filter
/// is what keeps another account's orders and fills out of this account's reports.
fn row_is_account(row: &serde_json::Value, account: &str) -> bool {
    let named = row_account(row);
    named.is_empty() || named == account
}

/// The `(filled, total)` of one orders-list row. ⚠ The LIVE row (paper, 2026-10-17) has NO `totalSize` and NO
/// `status`: it carries `sizeAndFills` (`"filled/total"`, `"0/1"`), `remainingQuantity` and `filledQuantity`.
/// The total is the LARGEST of `filledQuantity + remainingQuantity` (both numbers), the `sizeAndFills` total and the
/// old `totalSize`: a partly filled order that was then CANCELLED reads `remainingQuantity` 0, so the sum alone would
/// understate it as the filled part, while the row's own size field still carries the order's quantity. The filled is
/// `filledQuantity`, else the `sizeAndFills` filled. `sizeAndFills` is a display
/// string (thousands commas are dropped; a part that does not parse is absent, never `0`).
fn row_filled_and_total(o: &serde_json::Value) -> (f64, f64) {
    let (size_filled, size_total) = o
        .get("sizeAndFills")
        .and_then(|v| v.as_str())
        .and_then(|s| s.split_once('/'))
        .map_or((None, None), |(f, t)| {
            let part = |p: &str| p.replace([',', ' '], "").parse::<f64>().ok();
            (part(f), part(t))
        });
    let filled = get_f64_opt(o, "filledQuantity").or(size_filled).unwrap_or(0.0);
    let from_remaining = get_f64_opt(o, "remainingQuantity")
        .zip(get_f64_opt(o, "filledQuantity"))
        .map(|(remaining, filled)| remaining + filled);
    let total = [from_remaining, size_total, get_f64_opt(o, "totalSize")]
        .into_iter()
        .flatten()
        .fold(0.0, f64::max);
    (filled, total)
}

/// The status of an orders-LIST row. The live row's word is `order_ccp_status` (`PreSubmitted` on the capture);
/// `status` is the old spelling and the fallback. A word this table does not know is `Rejected`, exactly as the exec
/// path reads it (the owner's ruling: an unknown IBKR status is never resting), via [`normalize_order_status`].
/// A row that names NO word is read off its quantities, because `recon::recover_orders` would re-register a
/// resting order: filled == total > 0 is `Filled`, nothing remaining (filled < total) is `Canceled` (it ended
/// part-filled or unfilled, and is never `Accepted`); only a row that still shows a remainder, or says nothing of
/// one, is reported RESTING (`Accepted`, or `PartiallyFilled` on progress) and WARNED.
fn row_status(o: &serde_json::Value, filled: f64, total: f64) -> OrderStatus {
    let word = ["order_ccp_status", "status"]
        .into_iter()
        .map(|k| str_field(o, k))
        .find(|w| !w.is_empty())
        .unwrap_or_default();
    if !word.is_empty() {
        return normalize_order_status(&word, filled, total);
    }
    let nothing_remaining = get_f64_opt(o, "remainingQuantity").is_some_and(|r| r <= 0.0);
    if total > 0.0 && filled >= total {
        return OrderStatus::Filled;
    }
    if nothing_remaining {
        tracing::warn!(
            venue = VENUE,
            "an open-orders row names no status word and has nothing remaining: reporting the order as canceled"
        );
        return OrderStatus::Canceled;
    }
    tracing::warn!(
        venue = VENUE,
        "an open-orders row names no status word: reporting the order as resting"
    );
    if filled > 0.0 && filled < total {
        OrderStatus::PartiallyFilled
    } else {
        OrderStatus::Accepted
    }
}

/// `GET /iserver/account/orders` `{"orders":[..]}` → `OrderStatusReport`, filtered to `conid` and
/// to `account` (a row whose `acct` names another account is dropped, `row_is_account`), and
/// stamped with the canonical `symbol`. `orderId` → `venue_order_id`; `order_ref`/`cOID` →
/// `client_order_id`; `side` (`BUY`/`SELL`) → ±1; `orderType` mapped (`normalize_order_type`);
/// qty and filled from `sizeAndFills`/`remainingQuantity`/`filledQuantity` (`row_filled_and_total`; the old
/// `totalSize` is the fallback), status from `order_ccp_status` (`row_status`; the old `status` is the fallback);
/// `avgPrice` → avg_px; `lastExecutionTime_r` (epoch-ms) → ts.
pub fn parse_order_reports(
    body: &str,
    account: &str,
    conid: i64,
    symbol: &str,
) -> Result<Vec<OrderStatusReport>, String> {
    let v: serde_json::Value = serde_json::from_str(body).map_err(|e| e.to_string())?;
    let rows = v.get("orders").and_then(|o| o.as_array()).ok_or("expected an `orders` array")?;
    Ok(rows
        .iter()
        .filter(|o| row_conid(o) == conid && row_is_account(o, account))
        .map(|o| {
            let (filled_qty, qty) = row_filled_and_total(o);
            OrderStatusReport {
                venue: VENUE.to_string(),
                symbol: symbol.to_string(),
                venue_order_id: json_id(o.get("orderId")).into(),
                client_order_id: row_client_order_id(o),
                side: side_sign(&str_field(o, "side")),
                order_type: normalize_order_type(&str_field(o, "orderType")),
                qty,
                filled_qty,
                avg_px: get_f64(o, "avgPrice"),
                status: row_status(o, filled_qty, qty),
                ts: o.get("lastExecutionTime_r").and_then(|t| t.as_i64()).unwrap_or(0),
            }
        })
        .collect())
}

/// `GET /iserver/account/trades` `[..]` → `FillReport`, filtered to `conid` and to `account` (a
/// row whose `account`/`accountCode` names another account is dropped), and stamped with
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
pub fn parse_fill_reports(
    body: &str,
    account: &str,
    conid: i64,
    symbol: &str,
) -> Result<Vec<FillReport>, String> {
    let v: serde_json::Value = serde_json::from_str(body).map_err(|e| e.to_string())?;
    let rows = v.as_array().ok_or("expected a JSON array")?;
    let mut skipped = 0usize;
    let out: Vec<FillReport> = rows
        .iter()
        .filter(|t| row_conid(t) == conid && row_is_account(t, account))
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
                if c.is_empty() { "USD".to_string() } else { c }
            };
            Some(FillReport {
                venue: VENUE.to_string(),
                symbol: symbol.to_string(),
                trade_id,
                venue_order_id: json_id(t.get("orderId")).into(),
                client_order_id: row_client_order_id(t),
                side: side_sign(&str_field(t, "side")),
                last_qty: get_f64(t, "size"),
                last_px: get_f64(t, "price"),
                commission: get_f64(t, "commission"),
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
/// defaults (IBKR has no per-position isolated wallet). ONE page: [`collect_position_pages`] pages
/// the whole portfolio and synthesizes the flat zero row only after the LAST page.
pub fn parse_position_reports(
    body: &str,
    conid: i64,
    symbol: &str,
) -> Result<Vec<PositionStatusReport>, String> {
    position_page(body, conid, symbol).map(|(_, matched)| matched)
}

/// One positions page: its row count (what [`collect_position_pages`] decides "last page" by) and
/// the rows matching `conid`.
fn position_page(
    body: &str,
    conid: i64,
    symbol: &str,
) -> Result<(usize, Vec<PositionStatusReport>), String> {
    let v: serde_json::Value = serde_json::from_str(body).map_err(|e| e.to_string())?;
    let rows = v.as_array().ok_or("expected a JSON array")?;
    let matched = rows
        .iter()
        .filter(|p| row_conid(p) == conid)
        .map(|p| PositionStatusReport {
            venue: VENUE.to_string(),
            symbol: symbol.to_string(),
            position_side: PositionSide::Both,
            qty: get_f64(p, "position"), // ALREADY SIGNED
            avg_px: p.get("avgPrice").and_then(json_num).unwrap_or_else(|| get_f64(p, "avgCost")),
            ts: 0, // the positions row carries no per-position timestamp
            margin_mode: vike_model::MarginMode::default(),
            isolated_margin: None,
            delta: None,
        })
        .collect();
    Ok((rows.len(), matched))
}

/// The smallest page the CP Gateway is documented to serve (`CpapiRest::positions`: 30 rows). A
/// page with FEWER rows is the last; assuming the smallest size means a gateway serving larger
/// pages costs one extra (empty) request, never a missed page.
pub const POSITIONS_PAGE_ROWS: usize = 30;

/// How many pages one positions read may fetch before it is an `Err` (3000 positions at the
/// documented page size): a bound on a gateway that never serves a short page.
pub const MAX_POSITION_PAGES: u32 = 100;

/// The whole portfolio's rows for `conid`, paging `fetch_page(0)`, `fetch_page(1)`, … until a page
/// has fewer than [`POSITIONS_PAGE_ROWS`] rows (an empty page included) or a page holds the conId
/// (a portfolio lists a contract once). Only then, with no match, is the answer the FLAT zero row:
/// a position on page 1 must never read as flat because page 0 was full. A page that fails to
/// fetch or parse fails the whole read (past the last readable page there is no answer, not a flat
/// one), and so does running past [`MAX_POSITION_PAGES`].
pub fn collect_position_pages(
    mut fetch_page: impl FnMut(u32) -> Result<String, String>,
    conid: i64,
    symbol: &str,
) -> Result<Vec<PositionStatusReport>, String> {
    for page in 0..MAX_POSITION_PAGES {
        let body = fetch_page(page).map_err(|e| format!("cpapi positions page {page}: {e}"))?;
        let (rows, matched) = position_page(&body, conid, symbol)
            .map_err(|e| format!("cpapi positions page {page}: {e}"))?;
        if !matched.is_empty() {
            return Ok(matched);
        }
        if rows < POSITIONS_PAGE_ROWS {
            return Ok(vec![PositionStatusReport::flat(VENUE, symbol)]);
        }
    }
    Err(format!(
        "cpapi positions: no short page within {MAX_POSITION_PAGES} pages, so the read has no end"
    ))
}

/// `GET /portfolio/{acct}/ledger` `{"USD":{"cashbalance":..},"BASE":{..}}` → the quote `currency`'s
/// `cashbalance`: CASH (settled + unsettled) in that currency, NOT equity — no position value is
/// in it. This is the figure the BALANCE seed is written from.
///
/// The `BASE` row (every currency converted into the account's base currency) is read ONLY when
/// the specific currency key is absent AND the BASE row names its base currency (its `currency`
/// field, when that is a code rather than the word `BASE`) as `currency`. Otherwise the answer is
/// `None` (no seed): a converted total in another currency is not this currency's cash. ⚠ The
/// BASE row's `currency` field is not live-verified to carry a code at all; when it does not, there
/// is no fallback. Malformed JSON is a hard `Err`, never a panic.
pub fn parse_balance(body: &str, currency: &str) -> Result<Option<f64>, String> {
    let v: serde_json::Value = serde_json::from_str(body).map_err(|e| e.to_string())?;
    let cash = |key: &str| v.get(key).and_then(|c| c.get("cashbalance")).and_then(json_num);
    if let Some(c) = cash(currency) {
        return Ok(Some(c));
    }
    let base_names = v
        .get("BASE")
        .and_then(|b| b.get("currency"))
        .and_then(|c| c.as_str())
        .map(str::trim)
        .filter(|c| !c.eq_ignore_ascii_case("BASE"));
    Ok(match base_names {
        Some(base) if base.eq_ignore_ascii_case(currency) => cash("BASE"),
        _ => None,
    })
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
    /// The `cfg.account` every report is scoped to (the session holds it: checked at connect).
    account: String,
    /// Canonical vike symbol (e.g. `AAPL.SMART.USD`) stamped on every report row.
    symbol: String,
    /// Numeric conId the report rows are filtered to (resolved once at [`Self::connect`]).
    conid: i64,
    /// Quote currency for the ledger balance read (the contract's `currency`, e.g. `USD`).
    currency: String,
}

/// Whether the `/iserver/accounts` answer (`{"accounts":["DU…",…],…}`, live-verified) lists
/// `account`. A session that does not hold the configured account would answer every report for
/// another account (or none), so the client refuses to connect rather than report a complete-
/// looking flat book for an account it cannot see.
pub fn session_holds_account(accounts: &serde_json::Value, account: &str) -> Result<(), String> {
    let listed: Vec<&str> = accounts
        .get("accounts")
        .and_then(|a| a.as_array())
        .ok_or("cpapi /iserver/accounts carried no `accounts` array")?
        .iter()
        .filter_map(|a| a.as_str())
        .map(str::trim)
        .collect();
    if listed.contains(&account) {
        Ok(())
    } else {
        Err(format!(
            "the CP Gateway session does not hold the configured IBKR account ({} account(s) \
             listed); reconcile stays unwired rather than report another account's book",
            listed.len()
        ))
    }
}

impl IbkrReconClient {
    /// Open a DEDICATED cpapi reconcile client: build a fresh [`CpapiRest`] over `cfg.cpapi_url`/
    /// `cfg.account`, probe readiness (`tickle` — gateway up + browser-authenticated), require the
    /// session to hold `cfg.account` ([`session_holds_account`]), prime the portfolio endpoints
    /// (`GET /portfolio/accounts`, which IBKR documents as a prerequisite of
    /// `/portfolio/{acct}/…`; ⚠ unverified that the gateway needs it — a failure is one `warn!`, not
    /// a refusal, because positions answered without it on 2026-08-23), and resolve `symbol`'s conId
    /// via `secdef/search`. `Err` when the symbol is unmappable, the gateway is
    /// unreachable/unauthenticated, the account is not the session's, or the conId cannot be
    /// resolved — the reconcile driver then leaves this venue-symbol unwired, exec unaffected (the
    /// same graceful degradation every other absent-recon path uses). Never shares the exec side's
    /// transport.
    pub fn connect(cfg: &IbkrConfig, symbol: &str) -> Result<IbkrReconClient, IbkrError> {
        let contract = parse_simplified(symbol).map_err(IbkrError::Unsupported)?;
        // ⚠ The SAME refusal `transport::cpapi::CpapiTransport::resolve_conid` makes, for the same
        // reason and with a sharper consequence: this client filters EVERY report row to the one
        // conId it resolves here, so a first-hit pick would reconcile the exec side's December
        // future against (say) March's — quietly, and `PositionDrift` is the one kind `hybrid`
        // auto-applies. Refusing leaves this venue-symbol UNWIRED (the documented degradation,
        // exec unaffected), which is the honest answer: no reconcile beats a wrong one.
        if contract.conid_search_is_ambiguous() {
            return Err(IbkrError::Unsupported(format!(
                "IBKR reconcile cannot pin {symbol}: its identity includes an expiry, and \
                 cpapi's secdef/search is keyed on symbol+secType alone — the conId it would \
                 return names another contract month. This venue-symbol stays unreconciled."
            )));
        }
        let rest = CpapiRest::new(&cfg.cpapi_url, &cfg.account);
        rest.tickle().map_err(|e| IbkrError::Connect(format!("cpapi tickle: {}", e.msg)))?;
        let accounts = rest
            .accounts()
            .map_err(|e| IbkrError::Connect(format!("cpapi accounts: {}", e.msg)))?;
        session_holds_account(&accounts, &cfg.account).map_err(IbkrError::Unsupported)?;
        if let Err(e) = rest.portfolio_accounts() {
            tracing::warn!(
                target: "vike_ibkr::recon",
                error = %e.msg,
                "cpapi /portfolio/accounts priming failed; the positions/ledger reads are tried anyway"
            );
        }
        let search = rest
            .secdef_search(&contract.symbol, contract.sec_type.as_ib_code())
            .map_err(|e| IbkrError::Connect(format!("cpapi secdef_search: {}", e.msg)))?;
        let conid = decode_conid(&search)
            .ok_or_else(|| IbkrError::Unsupported(format!("no conId for IBKR symbol {symbol}")))?;
        Ok(IbkrReconClient {
            rest,
            account: cfg.account.clone(),
            symbol: symbol.to_string(),
            conid,
            currency: contract.currency,
        })
    }
}

impl ReconClient for IbkrReconClient {
    /// `_since` is a no-op: `/iserver/account/orders` reports only the CURRENTLY live set (no time
    /// filter) — a closed order that fell out of it is what the fill/position reports reconcile.
    fn fetch_order_status_reports(&self, _since: i64) -> Result<Vec<OrderStatusReport>, String> {
        let body = self.rest.open_orders().map_err(|e| e.msg)?.to_string();
        parse_order_reports(&body, &self.account, self.conid, &self.symbol)
    }

    /// `_since` is a no-op: the cpapi `/trades` endpoint returns a fixed recent-executions window
    /// (typically the last day) with no server-side time filter — the reconcile lookback is bounded
    /// by that window, not by `since`.
    fn fetch_fill_reports(&self, _since: i64) -> Result<Vec<FillReport>, String> {
        let body = self.rest.executions().map_err(|e| e.msg)?.to_string();
        parse_fill_reports(&body, &self.account, self.conid, &self.symbol)
    }

    /// EVERY positions page, filtered to this symbol's conId ([`collect_position_pages`]). cpapi
    /// OMITS a closed position (no zero row), so no match on any page synthesizes a FLAT `qty == 0`
    /// row — load-bearing: `recon::diff::diff` needs the row PRESENT to detect a stale LOCAL
    /// position the venue has since closed (the same synthesize-flat contract ctrader/okx use).
    fn fetch_position_status_reports(&self) -> Result<Vec<PositionStatusReport>, String> {
        collect_position_pages(
            |page| self.rest.positions(page).map(|v| v.to_string()).map_err(|e| e.msg),
            self.conid,
            &self.symbol,
        )
    }

    /// The account cash ledger's `cashbalance` for this contract's quote currency
    /// ([`parse_balance`]: cash, not equity; the BASE row only when it IS that currency) — the
    /// account's realized cash truth `diff_balance` reconciles against.
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
/// Wired by `IbkrVenueAdapter` in `crates/bridges/vike-ibkr/src/adapter.rs`, LAZILY behind
/// reconciliation, and only for a cpapi exec (the socket twin is
/// `crate::recon::socket::socket_recon_client`).
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

    /// A config pointed at a port nothing listens on. Every assertion below is on a guard that
    /// runs BEFORE `CpapiRest::new`/`tickle`, so no test here opens a socket — if one ever did,
    /// it would hang or fail rather than quietly pass, which is the right direction.
    fn unreachable_cfg() -> IbkrConfig {
        IbkrConfig {
            tier: vike_model::AccountTier::Demo,
            backend: vike_venues::IbkrBackend::Cpapi,
            host: "127.0.0.1".into(),
            port: 1,
            client_id: 1,
            account: "TEST".into(),
            cpapi_url: "https://127.0.0.1:1".into(),
            mktdata_type: crate::config::MktDataType::default(),
            data_client_id: 2,
            recon_client_id: 3,
            extended_hours: false,
        }
    }

    /// **Exec and reconcile must not read one canonical two ways.** `crate::exec::run_exec`
    /// submits against `parse_simplified`'s contract; this client filters EVERY report row to the
    /// conId it resolves from the SAME parse, so a disagreement here means the order is placed on
    /// one contract and reconciled against another — and `PositionDrift`, the one divergence kind
    /// `hybrid` auto-applies, would rewrite local size onto the wrong contract's number.
    ///
    /// Two directions are provable with no gateway, and both are:
    /// 1. a canonical exec REFUSES is refused here, with the parser's own sentence verbatim;
    /// 2. a canonical exec ACCEPTS whose identity carries an EXPIRY is refused here rather than
    ///    pinned to whatever `secdef/search` happens to list first.
    #[test]
    fn reconcile_reads_a_canonical_exactly_as_exec_does() {
        let cfg = unreachable_cfg();
        for refused in ["ESZ5.GLOBEX.USD", "CL.NYMEX.USD", "AAPL.SMART.NASDAQ", "AAPL.SMART"] {
            let exec_side = parse_simplified(refused)
                .expect_err("exec refuses this canonical — the fixture is stale if it parses");
            match IbkrReconClient::connect(&cfg, refused) {
                Err(IbkrError::Unsupported(msg)) => assert_eq!(
                    msg, exec_side,
                    "{refused}: reconcile must refuse with exec's own sentence"
                ),
                Err(e) => panic!("{refused}: reconcile refused for the wrong reason: {e}"),
                Ok(_) => panic!("{refused}: reconcile CONNECTED to a canonical exec refuses"),
            }
        }
        // 2. The expiry-bearing contract exec CAN place.
        let fut = parse_simplified("ES.GLOBEX.USD.FUT.20251219").expect("exec accepts this");
        assert!(fut.conid_search_is_ambiguous());
        match IbkrReconClient::connect(&cfg, "ES.GLOBEX.USD.FUT.20251219") {
            Err(IbkrError::Unsupported(msg)) => {
                assert!(msg.contains("another contract month"), "names the hazard: {msg}")
            }
            Err(e) => panic!("reconcile refused for the wrong reason: {e}"),
            Ok(_) => panic!("reconcile pinned an expiry-bearing conId by first-hit search"),
        }
    }

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
        assert_eq!(normalize_order_status("Submitted", 0.0, 10.0), OrderStatus::Accepted);
        assert_eq!(normalize_order_status("Submitted", 4.0, 10.0), OrderStatus::PartiallyFilled);
        assert_eq!(normalize_order_status("Filled", 10.0, 10.0), OrderStatus::Filled);
        assert_eq!(normalize_order_status("Cancelled", 0.0, 10.0), OrderStatus::Canceled);
        assert_eq!(normalize_order_status("Inactive", 0.0, 10.0), OrderStatus::Rejected);
        // a word IB does not document is Rejected, as the exec path reads it (`OrderStatusKind::from_ib`)
        assert_eq!(normalize_order_status("WarnState", 0.0, 10.0), OrderStatus::Rejected);
    }
}
