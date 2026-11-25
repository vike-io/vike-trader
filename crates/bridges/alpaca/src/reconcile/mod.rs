//! Alpaca `ReconcileClient` (ReconFactory seam) — the venue-facing report seam
//! (`vike_execution::recon::ReconcileClient`) over the SAME Bearer `AlpacaTransport` client [`crate::execution`]
//! already builds, reconciling against the Broker API:
//!
//!   `GET /v1/trading/accounts/{account_id}/orders` (order reports) — `status=all` plus a
//!       `symbols=` filter scopes the response to the engine symbol SERVER-SIDE (unlike every
//!       other venue's "currently open only" report, Alpaca's `orders` endpoint returns the
//!       WHOLE account's order history when unfiltered, so the symbol filter is load-bearing,
//!       not just an optimization). [`read_order_pages`] reads it to the end, newest first, at
//!       `limit=500` (the documented maximum; the default page is 50).
//!   `GET /v1/accounts/activities/FILL` (fill reports — Alpaca's per-fill "trade activity" feed;
//!       this endpoint has no server-side symbol filter, so [`parse_fills`] filters client-side).
//!       [`read_fill_pages`] reads it from `since` to the end, oldest first, at `page_size=100`
//!       (the documented maximum without `date`).
//!   `GET /v1/trading/accounts/{account_id}/positions` (position reports — also no per-symbol
//!       filter; [`parse_positions`] filters client-side and synthesizes a flat row for the
//!       engine symbol when it's absent — Alpaca omits a symbol entirely once its position is
//!       flat, the same convention `vike_okx::reconcile::parse_positions` already documents).
//!   `GET /v1/trading/accounts/{account_id}/account` (`fetch_balance` — the account's `cash`,
//!       the "wallet balance, not available/equity" convention every other venue's
//!       `fetch_balance` already picks).
//!
//! **Symbol spelling.** A USER name's wire form is its kept row's (`BTCUSD` -> `BTC/USD`, `crate::names::wire_of`,
//! read at every pass, so a name with no row fails the pass in words naming the fix) but every report row this
//! client returns carries the USER name (whatever `symbol` the caller started), never the wire form —
//! `recon::diff::diff` matches a position report's `.symbol` directly against `ReconcileLocalView`'s keys,
//! so a report row spelled `"BTC/USD"` against local state keyed `"BTCUSD"` would silently never
//! match. [`parse_orders`]/[`parse_fills`]/[`parse_positions`] therefore take BOTH the Alpaca wire
//! symbol (to filter/match rows) and the vike symbol (written into every returned row).
//!
//! **`since` bounds the fill read, not the order read.** The fill read asks for `after=` the
//! RFC3339 second of `since` (floored, so the window only widens; `since <= 0` asks from the
//! start). The order read ignores it: it must carry every OPEN order however old (the restore
//! planner reads an order a successful report lacks as gone, and `vike_execution::recon::diff` raises
//! `OrphanLocalOrder`), and `status=all` keeps the terminal history it always reported, so it is
//! the symbols' whole order history. A read that cannot finish FAILS: past [`ORDER_PAGE_CAP`] /
//! [`FILL_PAGE_CAP`] pages the fetch is an `Err` naming the cap, never a short list that reads as
//! complete (the rule `crates/bridges/ctrader/src/reconcile/mod.rs`'s `DEAL_PAGE_ROUNDS` follows).
//!
//! **No live fee-rate lane:** the Broker API does not document a per-account maker/taker rate
//! endpoint (US equities/crypto here are commission-passthrough, not maker/taker), so
//! `fetch_fee_rates` stays the trait default (`Ok(None)`) — same as every un-wired venue.
//!
//! Every fetch delegates to a PURE free function (`parse_*`) over the raw JSON body, and the two
//! paged reads to a pure pager over a page-fetch closure (`read_*_pages`) — the fixture-tested
//! units (`tests/reconcile_client_parse.rs`, no network) against captured Broker API shapes
//! (`alpacahq/alpaca-docs`, verified 2026-07-20; the paging parameters re-read 2026-11-10).

pub(crate) mod venue_read;

use vike_bridge_core::json::{parse_rows, str_f64};
use vike_bridge_core::order_status::OrderStatusTable;
use vike_bridge_core::reconcile_admitted::ReconcileAdmittedSymbols;
use vike_bridge_core::side::sign_default_buy;
use vike_execution::recon::{ReconcileClient, ReconcileFetchError};
use vike_model::events::{LiquiditySide, PositionSide, TradeId};
use vike_model::time::utc_rfc3339;
use vike_model::{
    FillReport, MarginMode, OrderStatus, OrderStatusReport, OrderType, PositionStatusReport,
};
use vike_utility::calendar::{MissingZone, parse_rfc3339_ms};

use crate::config::AlpacaConfig;
use crate::refusals::{AlpacaNameRefusal, AlpacaReconcileRefusal};
use crate::transport::AlpacaTransport;

const VENUE: &str = "alpaca";

// --- pure parsers ------------------------------------------------------------------------------

// A wire numeric field arrives as a JSON STRING (Alpaca quotes every decimal) or `null`: an order's or
// a position's quantity or price is read with `str_f64(v.get(key)).unwrap_or(0.0)`, so `null`/absent/unparseable
// folds to `0.0` (notional-only orders carry no `qty`, for instance) rather than erroring the whole row. A FILL
// row without both is skipped instead ([`parse_fills`]).

/// An order TERM read off the same quoted-decimal wire ([`str_f64`]), but a term the row does not
/// state is `None`: absent, `null`, unparseable, non-finite or not above zero, never `0.0`.
fn opt_str_f64(v: &serde_json::Value, key: &str) -> Option<f64> {
    str_f64(v.get(key)).filter(|x| x.is_finite() && *x > 0.0)
}

/// An order row's `(price, trigger_price, trail, trail_percent)`, by its kind word, in
/// `vike_model::OrderStatusReport`'s meaning: `limit` states `price` (`limit_price`), `stop` states
/// `trigger_price` (`stop_price`), `stop_limit` both, and `trailing_stop` its distance, `trail` from
/// `trail_price` (dollars) or `trail_percent` (`1.0` = 1%, the unit vike's field takes, so neither is
/// converted). A trailing stop's `stop_price` is the venue's MOVING stop (derived from `hwm` and the
/// trail) and the report's `trigger_price` is an ACTIVATION price, which Alpaca has none of, so it
/// stays `None`. `market` and any other word state nothing.
fn order_terms(
    o: &serde_json::Value,
    kind: &str,
) -> (Option<f64>, Option<f64>, Option<f64>, Option<f64>) {
    match kind {
        "limit" => (opt_str_f64(o, "limit_price"), None, None, None),
        "stop" => (None, opt_str_f64(o, "stop_price"), None, None),
        "stop_limit" => (opt_str_f64(o, "limit_price"), opt_str_f64(o, "stop_price"), None, None),
        "trailing_stop" => {
            (None, None, opt_str_f64(o, "trail_price"), opt_str_f64(o, "trail_percent"))
        }
        _ => (None, None, None, None),
    }
}

/// Map an Alpaca order `status` onto the FSM [`OrderStatus`] — every status the Broker/Trading API
/// documents, explicitly:
///
/// - Still resting or in flight → `Accepted`: `new`, `accepted`, `pending_new`,
///   `accepted_for_bidding`, `pending_replace`, `replaced`, `stopped`, `suspended`, `calculated`,
///   and `held` (a bracket/OTO child leg parked until its parent fills).
/// - `partially_filled` → `PartiallyFilled`; `filled` → `Filled`; `expired` → `Expired`;
///   `rejected` → `Rejected`.
/// - `pending_cancel` → `PendingCancel`: still live until Alpaca says canceled, as binance and vike-ibkr read
///   it (owner ruling L3: a cancel that then fails can still fill).
/// - `canceled` and `done_for_day` → `Canceled` (`done_for_day` will not fill again today).
///
/// A word outside that list is WARNED and read as `Accepted`
/// (`vike_bridge_core::order_status::unknown_status_word`).
fn normalize_order_status(raw: &str) -> OrderStatus {
    ORDER_STATUS.status_of(VENUE, raw)
}

/// The words of [`normalize_order_status`].
const ORDER_STATUS: OrderStatusTable = OrderStatusTable {
    words: &[
        ("new", OrderStatus::Accepted),
        ("accepted", OrderStatus::Accepted),
        ("pending_new", OrderStatus::Accepted),
        ("accepted_for_bidding", OrderStatus::Accepted),
        ("pending_replace", OrderStatus::Accepted),
        ("replaced", OrderStatus::Accepted),
        ("stopped", OrderStatus::Accepted),
        ("suspended", OrderStatus::Accepted),
        ("calculated", OrderStatus::Accepted),
        ("held", OrderStatus::Accepted),
        ("partially_filled", OrderStatus::PartiallyFilled),
        ("filled", OrderStatus::Filled),
        ("canceled", OrderStatus::Canceled),
        ("pending_cancel", OrderStatus::PendingCancel),
        ("done_for_day", OrderStatus::Canceled),
        ("expired", OrderStatus::Expired),
        ("rejected", OrderStatus::Rejected),
    ],
};

/// Map an Alpaca `order_type` onto [`OrderType`]: `market`, `limit`, `stop` and `stop_limit` are
/// their twins (the same words the exec builder sends). `trailing_stop`, or any other kind, has no
/// twin and stays [`OrderType::Unrecognized`] lower-cased, as every report spelled it before the
/// type existed.
fn normalize_order_type(raw: &str) -> OrderType {
    match raw {
        "market" => OrderType::Market,
        "limit" => OrderType::Limit,
        "stop" => OrderType::Stop,
        "stop_limit" => OrderType::StopLimit,
        other => OrderType::Unrecognized(other.to_ascii_lowercase()),
    }
}

/// `GET /v1/trading/accounts/{account_id}/orders?symbols=…` rows -> `OrderStatusReport`. Every
/// row is kept — an empty/absent `client_order_id` maps to `None` (an externally-placed order),
/// mirroring binance/bybit/OKX. `order_type` prefers the `order_type` field, falling back to
/// `type` (the Broker API documents both, normally identical), and the same word picks the terms
/// the row states ([`order_terms`]). `ts` is `updated_at`.
pub fn parse_orders(
    body: &str,
    alpaca_symbol: &str,
    symbol: &str,
) -> Result<Vec<OrderStatusReport>, String> {
    let v: serde_json::Value = serde_json::from_str(body).map_err(|e| e.to_string())?;
    let rows = v.as_array().ok_or_else(|| "expected a JSON array".to_string())?;
    Ok(rows
        .iter()
        .filter(|o| o.get("symbol").and_then(|s| s.as_str()) == Some(alpaca_symbol))
        .map(|o| {
            let order_client_id = o
                .get("client_order_id")
                .and_then(|c| c.as_str())
                .filter(|c| !c.is_empty())
                .map(|c| c.to_string());
            let kind = o
                .get("order_type")
                .or_else(|| o.get("type"))
                .and_then(|t| t.as_str())
                .unwrap_or("");
            let order_type = normalize_order_type(kind);
            let (price, trigger_price, trail, trail_percent) = order_terms(o, kind);
            let ts = o
                .get("updated_at")
                .and_then(|t| t.as_str())
                .and_then(|t| parse_rfc3339_ms(t, MissingZone::Refuse))
                .unwrap_or(0);
            OrderStatusReport {
                venue: VENUE.to_string(),
                symbol: symbol.to_string(),
                venue_order_id: o.get("id").and_then(|s| s.as_str()).unwrap_or("").into(),
                order_client_id,
                side: sign_default_buy(o.get("side").and_then(|s| s.as_str()).unwrap_or("")),
                order_type,
                qty: str_f64(o.get("qty")).unwrap_or(0.0),
                filled_qty: str_f64(o.get("filled_qty")).unwrap_or(0.0),
                average_price: str_f64(o.get("filled_avg_price")).unwrap_or(0.0),
                status: normalize_order_status(
                    o.get("status").and_then(|s| s.as_str()).unwrap_or(""),
                ),
                timestamp: ts,
                price,
                trigger_price,
                trail,
                trail_percent,
            }
        })
        .collect())
}

/// `GET /v1/accounts/activities/FILL` rows -> `FillReport`, filtered to `alpaca_symbol` (this
/// endpoint has no server-side symbol filter). `activity_type == "FILL"` is asserted defensively
/// (the path already scopes to FILL activities, but a future caller might hit the unscoped
/// `/v1/accounts/activities` endpoint instead). No commission/liquidity fields are documented on
/// this activity shape, so `commission: 0.0`/`commission_asset: ""`/`liquidity_side: Unknown` —
/// the same "field genuinely absent from the wire" fail-soft every other venue's parser uses.
/// `trade_id` is the `execution_id` after the activity id's `::` (the key the stream's fill was
/// booked under). `ts` is `transaction_time`.
pub fn parse_fills(
    body: &str,
    alpaca_symbol: &str,
    symbol: &str,
) -> Result<Vec<FillReport>, String> {
    let v: serde_json::Value = serde_json::from_str(body).map_err(|e| e.to_string())?;
    let rows = v.as_array().ok_or_else(|| "expected a JSON array".to_string())?;
    Ok(rows
        .iter()
        .filter(|r| r.get("activity_type").and_then(|t| t.as_str()) == Some("FILL"))
        .filter(|r| r.get("symbol").and_then(|s| s.as_str()) == Some(alpaca_symbol))
        // SKIP the row, do not fail the batch and do not admit an id-less report. An activity `id`
        // is Alpaca's documented per-fill identity and the key `vike_execution::recon::diff` looks up in
        // `seen_trade_ids` to decide whether the fold thread already booked this fill. An EMPTY id
        // can never match, so it does not merely go unrecognised — it manufactures a `MissingFill`
        // divergence, and `MissingFill` is one of the two kinds the `hybrid` policy AUTO-APPLIES:
        // the fill gets synthesized and booked a second time with no operator in front of it.
        // Skipping is the safe direction (no divergence, so nothing is auto-applied); failing the
        // whole batch would discard the good rows with the bad one.
        // ⚠ Was `.unwrap_or("")`.
        .filter_map(|f| {
            // The activity `id` is `<transaction time>::<execution_id>`, and the fold booked the fill
            // under the trade-update stream's bare `execution_id` (the CI box sandbox, 2026-11-10:
            // `20261110140324323::060aa12f-…` against a booked `060aa12f-…`). `diff` matches a fill by
            // trade id alone, so the whole id raised a `MissingFill` for EVERY fill this daemon booked,
            // and confirming one would book it twice. An id without `::` is kept whole.
            let raw = f.get("id").and_then(|s| s.as_str()).unwrap_or("");
            let execution_id = raw.rsplit_once("::").map_or(raw, |(_, exec)| exec);
            let trade_id = match TradeId::new(execution_id) {
                Ok(t) => t,
                Err(_) => {
                    tracing::warn!(
                        venue = VENUE,
                        %symbol,
                        "FILL activity row carries no `id` — skipping it; an id-less fill report \
                         would fabricate a MissingFill divergence that `hybrid` auto-applies"
                    );
                    return None;
                }
            };
            // A fill always states both, unlike an order row: a defaulted 0 would be booked at price 0.
            let (Some(last_qty), Some(last_price)) = (opt_str_f64(f, "qty"), opt_str_f64(f, "price"))
            else {
                tracing::error!(
                    venue = VENUE,
                    %symbol,
                    %trade_id,
                    "FILL activity row states no positive `qty` or `price` — skipping it rather than \
                     reporting a fill at 0"
                );
                return None;
            };
            Some(FillReport {
                venue: VENUE.to_string(),
                symbol: symbol.to_string(),
                trade_id,
                venue_order_id: f.get("order_id").and_then(|s| s.as_str()).unwrap_or("").into(),
                // Activities carry no order-client-id field at all (unlike the orders endpoint).
                order_client_id: None,
                side: sign_default_buy(f.get("side").and_then(|s| s.as_str()).unwrap_or("")),
                last_qty,
                last_price,
                commission: 0.0,
                commission_asset: String::new(),
                liquidity_side: LiquiditySide::Unknown,
                timestamp: f
                    .get("transaction_time")
                    .and_then(|t| t.as_str())
                    .and_then(|t| parse_rfc3339_ms(t, MissingZone::Refuse))
                    .unwrap_or(0),
            })
        })
        .collect())
}

/// `GET /v1/trading/accounts/{account_id}/positions` rows -> `PositionStatusReport`, filtered to
/// `alpaca_symbol` (no server-side symbol filter documented). `qty` is UNSIGNED on the wire — the
/// sign comes from `side` (`"long"`/`"short"`), mirroring Bybit's unsigned-`size`-plus-`side`
/// convention. The row is keyed `PositionSide::Both`, sign in `qty`: alpaca nets one position per
/// symbol and every fill folds under `Both` (`crate::execution::event_mapper`), and `vike_execution::recon::diff`
/// matches `(symbol, position_side)` exactly, so a `Long` row read as an `OrphanLocalPosition` plus a
/// `PositionOnlyExternal` of the same share (the CI box, 2026-11-10). An absent/non-matching row
/// synthesizes ONE flat (`qty: 0.0`, `PositionSide::Both`)
/// row for `symbol` — Alpaca omits a symbol entirely once its position is flat (no `qty: "0"` row
/// sent), the same gap `vike_okx::reconcile::parse_positions` documents and works around, so a
/// stale local position stays detectable. `ts` is always `0` — the position object carries no
/// per-row timestamp.
pub fn parse_positions(
    body: &str,
    alpaca_symbol: &str,
    symbol: &str,
) -> Result<Vec<PositionStatusReport>, String> {
    let v: serde_json::Value = serde_json::from_str(body).map_err(|e| e.to_string())?;
    let rows = v.as_array().ok_or_else(|| "expected a JSON array".to_string())?;
    let matched: Vec<PositionStatusReport> = rows
        .iter()
        .filter(|p| p.get("symbol").and_then(|s| s.as_str()) == Some(alpaca_symbol))
        .map(|p| {
            let is_short = p
                .get("side")
                .and_then(|s| s.as_str())
                .is_some_and(|s| s.eq_ignore_ascii_case("short"));
            let unsigned_qty = str_f64(p.get("qty")).unwrap_or(0.0).abs();
            PositionStatusReport {
                venue: VENUE.to_string(),
                symbol: symbol.to_string(),
                position_side: PositionSide::Both,
                qty: if is_short { -unsigned_qty } else { unsigned_qty },
                average_price: str_f64(p.get("avg_entry_price")).unwrap_or(0.0),
                timestamp: 0,
                margin_mode: MarginMode::default(),
                isolated_margin: None,
                delta: None,
            }
        })
        .collect();
    if matched.is_empty() {
        return Ok(vec![PositionStatusReport::flat(VENUE, symbol)]);
    }
    Ok(matched)
}

/// `GET /v1/trading/accounts/{account_id}/account` -> the account's `cash` (total tradable cash —
/// NOT `equity`, which includes unrealized P&L on open positions), matching every other venue's
/// "wallet balance, not available/equity" `fetch_balance` convention. `None` when the field is
/// absent (fail-soft); malformed JSON is a hard `Err`.
pub fn parse_balance(body: &str) -> Result<Option<f64>, String> {
    let v: serde_json::Value = serde_json::from_str(body).map_err(|e| e.to_string())?;
    if !v.is_object() {
        return Err("expected a JSON object".to_string());
    }
    Ok(v.get("cash").and_then(|c| c.as_str()).and_then(|s| s.parse().ok()))
}

// --- pure pagers -------------------------------------------------------------------------------

/// Most rows one order page returns: `limit`'s documented maximum (its default is 50).
pub const ORDER_PAGE_LIMIT: usize = 500;
/// Order pages one read may take before it fails: 10 000 orders on the read symbols.
// shortcut: the order read is the symbols' WHOLE history (`status=all`, no window), so an account
// past this many orders fails every pass; upgrade to a whole `status=open` read plus a
// `status=all&after=<since>` read when one gets there.
pub const ORDER_PAGE_CAP: usize = 20;
/// Most rows one fill-activity page returns: `page_size`'s documented maximum (and default)
/// when no `date` is given.
pub const FILL_PAGE_SIZE: usize = 100;
/// Fill pages one read may take before it fails: 10 000 fills inside one lookback.
pub const FILL_PAGE_CAP: usize = 100;

/// The whole order history of `symbols` (the comma-joined wire symbols), newest first, as ONE JSON
/// array body for [`parse_orders`]. `get` fetches one page of `GET …/orders` given its query: each
/// asks for `status=all`, `symbols`, `limit=`[`ORDER_PAGE_LIMIT`] and `direction=desc`, and every
/// page after the first adds `until=` the previous page's last (oldest) row's `submitted_at`,
/// verbatim. A row already read is dropped by `id`. The read ends at a page shorter than the
/// limit; an `Err` from `get`, a full page whose last row has no `submitted_at`, or a full page
/// still coming after [`ORDER_PAGE_CAP`] pages is an `Err`, never a short list.
pub fn read_order_pages(
    symbols: &str,
    mut get: impl FnMut(&str) -> Result<String, String>,
) -> Result<String, AlpacaReconcileRefusal> {
    let mut rows: Vec<serde_json::Value> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let mut until: Option<String> = None;
    for _ in 0..ORDER_PAGE_CAP {
        let mut query = vec![
            ("status", "all".to_string()),
            ("symbols", symbols.to_string()),
            ("limit", ORDER_PAGE_LIMIT.to_string()),
            ("direction", "desc".to_string()),
        ];
        // `until` is documented EXCLUSIVE ("submitted until this timestamp (exclusive.)"): a row
        // submitted in the same microsecond as the previous page's oldest row, and not on that page,
        // is never read. The `id` de-duplication covers a venue that reads it inclusively instead.
        if let Some(u) = &until {
            query.push(("until", u.clone()));
        }
        let page = page_rows(&mut get, &query)?;
        let full = page.len() >= ORDER_PAGE_LIMIT;
        until = page
            .last()
            .and_then(|o| o.get("submitted_at"))
            .and_then(|t| t.as_str())
            .map(str::to_string);
        for o in page {
            let id = o.get("id").and_then(|i| i.as_str()).map(str::to_string);
            if id.is_none_or(|id| seen.insert(id)) {
                rows.push(o);
            }
        }
        if !full {
            return Ok(serde_json::Value::Array(rows).to_string());
        }
        if until.is_none() {
            return Err(AlpacaReconcileRefusal::OrderPageNoCursor);
        }
    }
    Err(AlpacaReconcileRefusal::OrderPagesCapped)
}

/// One page's rows: `get` of the encoded `query`, its failure unchanged, then the shared row reader's.
fn page_rows(
    get: &mut impl FnMut(&str) -> Result<String, String>,
    query: &[(&str, String)],
) -> Result<Vec<serde_json::Value>, AlpacaReconcileRefusal> {
    let body = get(&vike_bridge_core::urlencode(query))
        .map_err(|why| AlpacaReconcileRefusal::PageUnread { why })?;
    parse_rows(&body, serde_json::Value::clone)
        .map_err(|why| AlpacaReconcileRefusal::PageNotRows { why })
}

/// Every `FILL` activity of `account_id` from `since` on, oldest first, as ONE JSON array body for
/// [`parse_fills`]. `get` fetches one page of `GET /v1/accounts/activities/FILL` given its query:
/// each asks for `account_id`, `direction=asc`, `page_size=`[`FILL_PAGE_SIZE`] and, when
/// `since > 0`, `after=` the RFC3339 second of `since` (floored: the documented spelling is
/// `YYYY-MM-DDTHH:MM:SSZ`, and a floor only widens the window); every page after the first adds
/// `page_token=` the previous page's last `id` (documented: with `asc`, "results will begin with
/// the activity immediately after the one specified"). Ends and fails as [`read_order_pages`]
/// does, at [`FILL_PAGE_CAP`].
pub fn read_fill_pages(
    account_id: &str,
    since: i64,
    mut get: impl FnMut(&str) -> Result<String, String>,
) -> Result<String, AlpacaReconcileRefusal> {
    let mut rows: Vec<serde_json::Value> = Vec::new();
    let mut token: Option<String> = None;
    for _ in 0..FILL_PAGE_CAP {
        let mut query = vec![
            ("account_id", account_id.to_string()),
            ("direction", "asc".to_string()),
            ("page_size", FILL_PAGE_SIZE.to_string()),
        ];
        if since > 0 {
            query.push(("after", utc_rfc3339(since / 1000)));
        }
        if let Some(t) = &token {
            query.push(("page_token", t.clone()));
        }
        let page = page_rows(&mut get, &query)?;
        let full = page.len() >= FILL_PAGE_SIZE;
        token = page.last().and_then(|f| f.get("id")).and_then(|i| i.as_str()).map(str::to_string);
        rows.extend(page);
        if !full {
            return Ok(serde_json::Value::Array(rows).to_string());
        }
        if token.is_none() {
            return Err(AlpacaReconcileRefusal::FillPageNoCursor);
        }
    }
    Err(AlpacaReconcileRefusal::FillPagesCapped)
}

// --- the client ----------------------------------------------------------------------------

/// One reconcile client per (account, venue-symbol) — the SAME `AlpacaTransport` Bearer seam
/// the exec client ([`crate::execution::spawn`]) uses, dedicated to reconcile reads. `symbol` is the
/// USER name (e.g. `"AAPL"`, `"BTCUSD"`); its Alpaca wire form is its kept row's, read at every pass
/// ([`Self::pairs`]).
pub struct ReconcileClientAlpaca {
    rest: AlpacaTransport,
    base: String,
    account_id: String,
    symbol: String,
    /// The symbols the account's engine admitted on demand (any-symbol, decision 0173), in the engine's
    /// spelling: every pass reads them beside the mounted one. Alpaca's three reads already span the
    /// whole account (orders take a `symbols=` list, fills and positions are filtered here), so
    /// admitting a symbol widens a filter and adds no request.
    admitted: ReconcileAdmittedSymbols,
}

impl ReconcileClientAlpaca {
    pub fn new(
        rest: AlpacaTransport,
        base: impl Into<String>,
        account_id: impl Into<String>,
        symbol: impl Into<String>,
    ) -> Self {
        ReconcileClientAlpaca {
            rest,
            base: base.into(),
            account_id: account_id.into(),
            symbol: symbol.into(),
            admitted: ReconcileAdmittedSymbols::default(),
        }
    }

    /// The `(wire symbol, engine symbol)` pairs a pass reads: the mounted symbol first, then each admitted
    /// one, each wire symbol its kept row's (`crate::names::wire_of`). A symbol whose WIRE form is already in
    /// the list is skipped (two spellings of one instrument), so no report row is produced twice.
    ///
    /// # Errors
    /// A symbol with no kept row, in words naming the fix: the pass fails rather than report a short list.
    fn pairs(&self) -> Result<Vec<(String, String)>, AlpacaNameRefusal> {
        let mut pairs: Vec<(String, String)> = Vec::new();
        for symbol in std::iter::once(self.symbol.clone()).chain(self.admitted.symbols()) {
            let (wire, _) = crate::names::wire_of(&symbol)?;
            if !pairs.iter().any(|(w, _)| *w == wire) {
                pairs.push((wire, symbol));
            }
        }
        Ok(pairs)
    }
}

impl ReconcileClient for ReconcileClientAlpaca {
    /// `_since` is unused on purpose: the read is the symbols' whole order history (module doc),
    /// paged by [`read_order_pages`]. ONE read carries every symbol the pass reads (`symbols=A,B`),
    /// and each is parsed out of the same rows.
    fn fetch_order_status_reports(&self, _since: i64) -> Result<Vec<OrderStatusReport>, String> {
        let path = format!("/v1/trading/accounts/{}/orders", self.account_id);
        let pairs = self.pairs().map_err(|refusal| refusal.to_string())?;
        let wire = pairs.iter().map(|(w, _)| w.as_str()).collect::<Vec<_>>().join(",");
        let body = read_order_pages(&wire, |query| {
            self.rest
                .get(&self.base, &path, query)
                .map(|v| v.to_string())
                .map_err(|e| e.to_string())
        })
        .map_err(|refusal| refusal.to_string())?;
        let mut reports = Vec::new();
        for (wire, symbol) in &pairs {
            reports.extend(parse_orders(&body, wire, symbol)?);
        }
        Ok(reports)
    }

    /// Every fill activity from `since` on, paged by [`read_fill_pages`].
    fn fetch_fill_reports(&self, since: i64) -> Result<Vec<FillReport>, String> {
        let pairs = self.pairs().map_err(|refusal| refusal.to_string())?;
        let body = read_fill_pages(&self.account_id, since, |query| {
            self.rest
                .get(&self.base, "/v1/accounts/activities/FILL", query)
                .map(|v| v.to_string())
                .map_err(|e| e.to_string())
        })
        .map_err(|refusal| refusal.to_string())?;
        let mut reports = Vec::new();
        for (wire, symbol) in &pairs {
            reports.extend(parse_fills(&body, wire, symbol)?);
        }
        Ok(reports)
    }

    fn fetch_position_status_reports(&self) -> Result<Vec<PositionStatusReport>, String> {
        let pairs = self.pairs().map_err(|refusal| refusal.to_string())?;
        let path = format!("/v1/trading/accounts/{}/positions", self.account_id);
        let body = self.rest.get(&self.base, &path, "").map_err(|e| e.to_string())?.to_string();
        let mut reports = Vec::new();
        for (wire, symbol) in &pairs {
            reports.extend(parse_positions(&body, wire, symbol)?);
        }
        Ok(reports)
    }

    /// Tell this client the engine admitted `symbol` (any-symbol, decision 0173): every later pass reads
    /// its orders, fills and position too. A user name is upper case already (`AAPL`, `BTCUSD`).
    fn admit_symbol(&self, symbol: &str) {
        self.admitted.admit_logged(symbol, &self.symbol, VENUE, "a symbol");
    }

    /// The engine gave `symbol` back (decision 0205): the next pass stops reading it.
    fn forget_symbol(&self, symbol: &str) {
        self.admitted.forget_logged(symbol, VENUE, "a symbol");
    }

    fn admitted_symbols(&self) -> Option<Vec<String>> {
        Some(self.admitted.symbols())
    }

    /// Balance meaning (pinned per the task brief): the account's `cash` from
    /// `/v1/trading/accounts/{account_id}/account` — see [`parse_balance`].
    ///
    /// The credential probe's read (`crate::adapter`'s `credential_probe`): a failure carries its
    /// `VenueApiHttpError::kind`'s outcome, so one a later try can pass (no answer, a 5xx, a 429) WARNS instead of
    /// failing the keys (owner, 2026-11-16; a 5xx since 2026-11-17), and only a terminal one refuses them.
    fn fetch_balance(&self) -> Result<Option<f64>, ReconcileFetchError> {
        let path = format!("/v1/trading/accounts/{}/account", self.account_id);
        let response = self
            .rest
            .get(&self.base, &path, "")
            .map_err(|e| ReconcileFetchError::new(e.kind().failure_outcome(), e.to_string()))?;
        parse_balance(&response.to_string()).map_err(ReconcileFetchError::unclassified)
    }
}

// --- the factory -----------------------------------------------------------------------------

/// The venue -> `ReconcileClient` factory (ReconFactory seam). Builds a FRESH
/// `TokenSource`/`AlpacaTransport` dedicated to reconcile reads — a second OAuth2 client-credentials
/// lifecycle, isolated from the exec side's own (mirrors the "one fresh REST client instance per
/// purpose" idiom every credentialed venue's factory already follows). `symbol` is the
/// USER name, NOT Alpaca's slashed crypto wire form (see the module doc). Always `Some` —
/// construction here is pure/infallible (no network); `Option` is kept so the return type matches
/// every other bridge's `recon_client`.
///
/// **WIRED into `crates/bridges/alpaca/src/adapter.rs`'s `VenueAdapterAlpaca`** — its `start` calls
/// this EAGERLY on every credentialed start (unlike the lazily-built venues, construction here is
/// pure and network-free, so there is nothing to defer), and its `credential_probe` reuses the same
/// factory to build this venue's preflight credential probe. Alpaca reconciles on the periodic
/// INTERVAL only — no `reconcile_trigger`.
///
/// ⚠ This line used to say "not yet wired ... Alpaca is not a live-mounted venue today", which it
/// had stopped being. A factory's header is the WRONG place to read that from, because a start arm
/// can adopt a factory without touching it — read the arm. `crates/bridges/ig/CLAUDE.md` carries
/// the family rule: every venue enrolled in the reconcile roster after its factory was written
/// inherited the same stale sentence.
pub fn recon_client(config: &AlpacaConfig, symbol: &str) -> Option<Box<dyn ReconcileClient>> {
    let token = std::sync::Arc::new(crate::transport::sign::TokenSource::new(
        config.client_id.expose().clone(),
        config.client_secret.expose().clone(),
        config.hosts.authx.to_string(),
    ));
    let rest = AlpacaTransport::new(token);
    Some(Box::new(ReconcileClientAlpaca::new(
        rest,
        config.hosts.broker.clone(),
        config.account_id.clone(),
        symbol.to_string(),
    )))
}

#[cfg(test)]
mod admitted_tests {
    use super::*;

    fn client(mounted: &str) -> ReconcileClientAlpaca {
        // Construction does no network work (the token is minted on first use).
        let token = std::sync::Arc::new(crate::transport::sign::TokenSource::new(
            "id".into(),
            "secret".into(),
            "http://127.0.0.1:9".into(),
        ));
        ReconcileClientAlpaca::new(
            AlpacaTransport::new(token),
            "http://127.0.0.1:9",
            "acct",
            mounted,
        )
    }

    /// The mounted symbol is read first, an admitted one after it, each under its kept row's wire name
    /// (`BTCUSD` is `BTC/USD`) and reported under the user name.
    #[test]
    fn an_admitted_symbol_is_read_beside_the_mounted_one_under_its_kept_rows_wire_name() {
        crate::names::hold_test_rows();
        let c = client("AAPL");
        assert_eq!(c.admitted_symbols(), Some(vec![]), "told nothing yet");
        c.admit_symbol("msft");
        c.admit_symbol("MSFT");
        c.admit_symbol("AAPL");
        c.admit_symbol("BTCUSD");
        assert_eq!(
            c.pairs().expect("every name has its row"),
            vec![
                ("AAPL".to_string(), "AAPL".to_string()),
                ("MSFT".to_string(), "MSFT".to_string()),
                ("BTC/USD".to_string(), "BTCUSD".to_string()),
            ]
        );
        assert_eq!(
            c.admitted_symbols().expect("a claim"),
            ["MSFT", "BTCUSD"],
            "the claim lists what was told, the mounted symbol and repeats excluded"
        );
    }

    /// A name with no kept row fails the pass in words naming the fix: no short list reads as complete.
    #[test]
    fn a_name_without_a_kept_row_fails_the_pass_with_the_fix() {
        crate::names::hold_test_rows();
        let c = client("AAPL");
        c.admit_symbol("GBTC");
        let refusal = c.pairs().expect_err("GBTC has no row");
        assert_eq!(refusal.id(), crate::refusals::AlpacaNameRefusalId::NotKept, "{refusal}");
        let why = c.fetch_position_status_reports().expect_err("refused before any read");
        assert_eq!(why, refusal.to_string());
    }
}
