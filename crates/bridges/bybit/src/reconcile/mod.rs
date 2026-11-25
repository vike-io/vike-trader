//! Bybit `ReconcileClient` (Task 12 of the reconciliation-engine feature) — the venue-facing report
//! seam (`vike_execution::recon::ReconcileClient`) for the linear-perp category this crate trades,
//! reconciling against:
//!
//!   `GET /v5/order/realtime` — resting + recently-closed orders (order reports). Like binance's
//!   `openOrders`, this is the venue's CURRENTLY-open/recent view with no time filter, so `since`
//!   is a no-op here (a fully-closed order that fell out of this set is exactly what the fill/
//!   position reports, not this one, reconcile). It PAGES: `limit=50`, then `nextPageCursor`
//!   followed while pages come back full; a chain that repeats a cursor or outruns its page cap
//!   FAILS the read, because the restore planner reads an order a successful report lacks as gone.
//!   It is asked with NO `orderFilter`, which the demo host answers with the resting conditional orders
//!   beside the active ones (`orderFilter=Order` lists only the active ones, `StopOrder` only the
//!   conditional ones: measured 2026-10-24), typed by `conditional_order_type`; that is what lets
//!   this client CLAIM it lists trigger orders (`ReconcileClient::lists_trigger_orders`, decision 0192).
//!   `GET /v5/execution/list` — fill history (fill reports); `since` maps to `startTime`. It PAGES
//!   by the order read's rule (`limit=100`, `nextPageCursor`; owner ruling F-2).
//!   `GET /v5/position/list` — net position per (symbol, positionIdx) leg (position reports).
//!   `size` on the wire is UNSIGNED — the sign comes from `side` ("Buy"/"Sell"), and the hedge
//!   `positionIdx` gives `position_side` ([`parse_positions`]). Every row is kept, including a
//!   flat (`size == 0`) leg: `recon::diff::diff` needs the report row PRESENT to detect "local
//!   still shows a position the venue has since closed" (mirrors the binance `ReconcileClient`'s
//!   Task 7 doc'd choice); dropping flat rows would hide that divergence.
//!
//! Every fetch delegates to a PURE free function (`parse_*`) over the raw `result` JSON body
//! (the `{retCode, retMsg, result}` V5 envelope already unwrapped by [`unwrap_envelope`]) — these
//! are the fixture-tested units (`tests/offline/reconcile_client_parse.rs`, no network).
//!
//! `ReconcileClientBybit` reuses the SAME `RestTransportJson`/`BybitV5Signer` seam as
//! `execution::rest::VenueRestBybit` (no new HTTP/signing stack): `order/realtime`/`position/list` ride the
//! normal `signed` path (the same one `VenueRestBybit::call` already uses for order/position
//! reads), while `execution/list` rides `signed_requery` — the SAME short-timeout,
//! rate-gate-EXEMPT path the crate's audit-A3 resync (`get_execution_history`) already uses for
//! this exact endpoint, so a resync burst can't blow the order-rate budget submit/cancel depend on.

// An operator's read of the account's balance (`VenueAdapter::venue_read`): a signed GET through `transport`.
pub mod venue_read;

use std::sync::{Arc, Mutex, PoisonError};

use serde_json::{Value, json};

use crate::signer::BybitV5Signer;
use vike_bridge_core::json::{json_int, json_number, json_str};
use vike_bridge_core::order_status::OrderStatusTable;
use vike_bridge_core::ratelimit::RateGatePause;
use vike_bridge_core::reconcile_admitted::{
    ReconcileAdmittedReadSpec, ReconcileAdmittedSymbols, ReconcileReadError, not_backing_off,
};
use vike_bridge_core::side::sign_default_sell;
use vike_bridge_core::signer::VenueClock;
use vike_bridge_core::transport::{ErrorKind, VenueApiError};
use vike_execution::recon::{ReconcileClient, ReconcileFetchError};
use vike_model::events::{LiquiditySide, TradeId};
use vike_model::{
    FeeSchedule, FillReport, MarginMode, OrderStatus, OrderStatusReport, OrderType,
    PositionStatusReport,
};

use crate::account::leverage::PATH_ACCOUNT_INFO;
use crate::execution::rest::{
    PATH_ACCOUNT, PATH_EXECUTION_LIST, PATH_ORDER_REALTIME, PATH_POSITION_LIST,
};
use crate::transport::client::{FailedRequest, request_words, unwrap_envelope};
use vike_bridge_core::transport::json::RestTransportJson;

const VENUE: &str = "bybit";
const PATH_FEE_RATE: &str = "/v5/account/fee-rate";

/// Rows asked per `order/realtime` request: the venue's maximum (`limit` is 1..=50, default 20).
const ORDERS_PAGE_LIMIT: usize = 50;

/// Pages one symbol's open-order read follows before it FAILS: 40 x 50 = 2000 rows, far above what
/// rests on one linear symbol, so the bound is for a venue that never ends its cursor chain.
const ORDERS_MAX_PAGES: usize = 40;

/// Rows asked per `execution/list` request: the venue's maximum (`limit` is 1..=100, default 50).
const FILLS_PAGE_LIMIT: usize = 100;

/// Pages one symbol's fill read follows before it FAILS: 40 x 100 = 4000 executions in one reconcile window
/// (a day by default), far above what this account trades on one symbol.
const FILLS_MAX_PAGES: usize = 40;

/// The facts of ONE paged read of the V5 cursor grammar ([`ReconcileClientBybit::cursor_pages`]): its path, its name
/// in an error, the parser of a page, the rows a page is asked for and the pages the walk follows before it fails.
struct CursorPages<R> {
    path: &'static str,
    name: &'static str,
    parse: fn(&str) -> Result<Vec<R>, String>,
    page_limit: usize,
    max_pages: usize,
}

impl<R> Clone for CursorPages<R> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<R> Copy for CursorPages<R> {}

/// The open and recently closed orders of one book.
const ORDER_PAGES: CursorPages<OrderStatusReport> = CursorPages {
    path: PATH_ORDER_REALTIME,
    name: "order/realtime",
    parse: parse_orders,
    page_limit: ORDERS_PAGE_LIMIT,
    max_pages: ORDERS_MAX_PAGES,
};

/// The executions of one symbol since the window's start (owner ruling F-2).
const FILL_PAGES: CursorPages<FillReport> = CursorPages {
    path: PATH_EXECUTION_LIST,
    name: "execution/list",
    parse: parse_fills,
    page_limit: FILLS_PAGE_LIMIT,
    max_pages: FILLS_MAX_PAGES,
};

/// The books an account's open orders are listed in, one `order/realtime` walk each
/// (`ReconcileClientBybit::account_open_orders`): a `linear` list needs a symbol, a base coin or a settle coin (the venue's
/// page), so it is asked per settle coin (bybit's `venue_currency` row); the `inverse`, `spot` and `option` lists take
/// the category alone (an option is refused by name on submit, but one placed elsewhere still rests on the account).
const ACCOUNT_BOOKS: [(&str, Option<&str>); 5] = [
    ("linear", Some(vike_venues::venue_currency::BYBIT.settle_coins[0])),
    ("linear", Some(vike_venues::venue_currency::BYBIT.settle_coins[1])),
    ("inverse", None),
    ("spot", None),
    ("option", None),
];

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

/// The page's `nextPageCursor`, `None` when absent, `null` or empty (the venue's three spellings of
/// "no more"). Read off the same `result` body [`parse_orders`] takes.
fn next_cursor(body: &str) -> Option<String> {
    let v: Value = serde_json::from_str(body).ok()?;
    v.get("nextPageCursor")?.as_str().filter(|c| !c.is_empty()).map(str::to_string)
}

/// Undo the venue's percent-encoding of a cursor (`id%3Ats%2C...`). The signer encodes every GET
/// value once, and the venue wants its cursor back as it spelled it, so the signer must be handed the
/// DECODED text: the wire then carries the very `%3A`/`%2C` the venue sent, not `%253A`. A `%` not
/// followed by two hex digits stays as it is. `crate::execution::funding`'s poller pages the transaction log through it too.
pub(crate) fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let escaped = (bytes[i] == b'%')
            .then(|| s.get(i + 1..i + 3))
            .flatten()
            .filter(|h| h.bytes().all(|c| c.is_ascii_hexdigit()))
            .and_then(|h| u8::from_str_radix(h, 16).ok());
        match escaped {
            Some(b) => {
                out.push(b);
                i += 3;
            }
            None => {
                out.push(bytes[i]);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Map a raw V5 `orderStatus` onto the FSM [`OrderStatus`] `recon::diff::diff` reads — every word
/// the V5 order endpoints document, explicitly:
///
/// - `New`, `Created` (accepted by the system, not yet through the matching engine), `Active` (the
///   legacy USDC-contract spelling of `New`) and `Untriggered` (a resting conditional waiting on its
///   trigger) → `Accepted`: all still open.
/// - `Triggered` → `Triggered`; `PartiallyFilled` → `PartiallyFilled`; `Filled` → `Filled`.
/// - `Cancelled`, `PartiallyFilledCanceled` (partially filled, then canceled) and `Deactivated` (the
///   venue's word for a conditional "cancelled before [it is] triggered", by us or by the venue) →
///   `Canceled`, the word the stream and `execution::resync::map_bybit_history` use for the same rows.
/// - `Rejected` → `Rejected`.
///
/// A word outside that list is WARNED and read as `Accepted`
/// (`vike_bridge_core::order_status::unknown_status_word`).
fn normalize_order_status(raw: &str) -> OrderStatus {
    ORDER_STATUS.status_of(VENUE, raw)
}

/// The words of [`normalize_order_status`].
const ORDER_STATUS: OrderStatusTable = OrderStatusTable {
    words: &[
        ("New", OrderStatus::Accepted),
        ("Created", OrderStatus::Accepted),
        ("Active", OrderStatus::Accepted),
        ("Untriggered", OrderStatus::Accepted),
        ("Triggered", OrderStatus::Triggered),
        ("PartiallyFilled", OrderStatus::PartiallyFilled),
        ("Filled", OrderStatus::Filled),
        ("Cancelled", OrderStatus::Canceled),
        ("PartiallyFilledCanceled", OrderStatus::Canceled),
        ("Deactivated", OrderStatus::Canceled),
        ("Rejected", OrderStatus::Rejected),
    ],
};

/// Map a raw V5 `orderType` onto [`OrderType`]: `Limit` → `Limit`, `Market` → `Market` (a
/// conditional's trigger kind rides the separate `stopOrderType`, which [`conditional_order_type`]
/// reads on top of this).
/// Any other word (V5's `UNKNOWN`) has no twin and stays [`OrderType::Unrecognized`] lower-cased,
/// as every report spelled it before the type existed.
fn normalize_order_type(raw: &str) -> OrderType {
    match raw {
        "Limit" => OrderType::Limit,
        "Market" => OrderType::Market,
        other => OrderType::Unrecognized(other.to_ascii_lowercase()),
    }
}

/// Type a CONDITIONAL row: `order/realtime` (no `orderFilter`) lists a resting conditional with the
/// plain `orderType` of what it fires AS (`Market`/`Limit`) and the reason it waits in `stopOrderType`.
/// `base` is [`normalize_order_type`]'s answer; `side` is the row's ±1 and `trigger_direction` its
/// `triggerDirection` (`1` rise, `2` fall, `0` none).
///
/// - `StopLoss`, `PartialStopLoss` → `Stop` (`StopLimit` when it fires as a `Limit`).
/// - `Stop`, a plain conditional order (the one row measured on the demo host, 2026-10-24: a
///   reduce-only `Market` sell, `triggerDirection` 2) → the same, but only when it fires in the LOSS
///   direction of the exit it is, the one `execution/rest/mod.rs`'s `build_order_params` places for a stop: a BUY
///   trigger on a RISE (`1`), a SELL trigger on a FALL (`2`).
/// - `Stop` in the PROFIT direction (a BUY on a fall, `2`; a SELL on a rise, `1`: what `build_order_params`
///   places for a `take_profit`) → `TakeProfit`, whether it fires as a `Market` (market-if-touched) or a
///   `Limit` (limit-if-touched; the report carries no price, so the one kind names both). A fade entry the
///   operator placed is the same order in vike's words (an if-touched order), and it is no stop: not cover.
/// - FAILS CLOSED: a missing or `0` direction cannot say which way the row fires, so a `Stop` with one
///   stays `base` (the warning stays, the safe direction). A SPOT row's `0` is read off its `basePrice`
///   first ([`trigger_direction`]).
/// - `TakeProfit`, `PartialTakeProfit` → `TakeProfit`.
/// - `TrailingStop` has no twin among ours: `Unrecognized("trailing_stop")`, the spelling binance's
///   `trailing_stop_market` gets, which the stop-cover rule reads as a stop.
///
/// Everything else keeps `base`, as it always did: the empty word of a plain order, `MmRateClose` (a
/// margin-rate close, not a price trigger), the spot-only `tpslOrder`/`OcoOrder`/`BidirectionalTpslOrder`
/// (this client reads `category=linear`) and any word the docs do not list. A `base` that is not
/// `Market`/`Limit` (V5's `UNKNOWN`) is never retyped. Only `Stop` was recorded; the other words are the
/// documented ones (`/v5/enum`'s `stopOrderType`), not captures.
fn conditional_order_type(
    stop_order_type: &str,
    trigger_direction: i64,
    side: i32,
    base: OrderType,
) -> OrderType {
    if !matches!(base, OrderType::Market | OrderType::Limit) {
        return base;
    }
    let stop = if base == OrderType::Limit { OrderType::StopLimit } else { OrderType::Stop };
    let (loss_direction, profit_direction) = if side > 0 { (1, 2) } else { (2, 1) };
    match stop_order_type {
        "StopLoss" | "PartialStopLoss" => stop,
        "Stop" if trigger_direction == loss_direction => stop,
        "Stop" if trigger_direction == profit_direction => OrderType::TakeProfit,
        "TakeProfit" | "PartialTakeProfit" => OrderType::TakeProfit,
        "TrailingStop" => OrderType::Unrecognized("trailing_stop".to_string()),
        _ => base,
    }
}

/// A row's `triggerDirection` (`1` rise, `2` fall, `0` none). A SPOT conditional states `0` (and
/// `triggerBy` ""), but carries `basePrice`, the last price when it was placed (demo host, 2026-11-08,
/// `bybit_spot_conditional_smoke`): a `triggerPrice` above it fires on a rise (`1`), below it on a fall
/// (`2`), so the row types as a linear row with that direction would. A stated direction is the venue's
/// and wins; with no `basePrice`, or a trigger equal to it, the answer stays `0` (fails closed).
fn trigger_direction(o: &Value) -> i64 {
    let stated = o.get("triggerDirection").and_then(json_int).unwrap_or(0);
    let price = |key: &str| o.get(key).and_then(json_number).filter(|p| *p > 0.0);
    match (stated, price("triggerPrice"), price("basePrice")) {
        (0, Some(trigger), Some(base)) if trigger > base => 1,
        (0, Some(trigger), Some(base)) if trigger < base => 2,
        (stated, ..) => stated,
    }
}

/// `GET /v5/order/realtime` rows -> `OrderStatusReport`. `qty`/`cumExecQty`/`avgPrice` are
/// echoed directly (unlike binance spot, no average-price derivation needed). An empty
/// `orderLinkId` normalizes to `None` (an externally-placed order the venue never echoed our id
/// for) — same convention as the binance `ReconcileClient`. A resting conditional row is typed by
/// [`conditional_order_type`] (`Stop`, `StopLimit`, `TakeProfit`…) from `stopOrderType`, with its
/// `triggerPrice` and, on a `Limit` row, its `price`; the report type has no `reduceOnly` field, so
/// that is not carried.
pub fn parse_orders(body: &str) -> Result<Vec<OrderStatusReport>, String> {
    let rows = list_rows(body)?;
    Ok(rows
        .iter()
        .map(|o| {
            let order_client_id = o
                .get("orderLinkId")
                .and_then(|c| c.as_str())
                .filter(|c| !c.is_empty())
                .map(|c| c.to_string());
            let side = sign_default_sell(o.get("side").and_then(|s| s.as_str()).unwrap_or(""));
            let base =
                normalize_order_type(o.get("orderType").and_then(|t| t.as_str()).unwrap_or(""));
            let limited = base == OrderType::Limit;
            let order_type = conditional_order_type(
                o.get("stopOrderType").and_then(|t| t.as_str()).unwrap_or(""),
                trigger_direction(o),
                side,
                base,
            );
            // The terms the row states ("0" is none): `price` is a limit only on a `Limit` row (a
            // market row's `price` is the venue's slippage cap, not ours), `triggerPrice` a
            // conditional's level; a kind with no twin (`trailing_stop`) states nothing.
            let positive =
                |key: &str| o.get(key).and_then(json_number).filter(|v| v.is_finite() && *v > 0.0);
            let known = !matches!(order_type, OrderType::Unrecognized(_));
            let price = positive("price").filter(|_| known && limited);
            let trigger_price = positive("triggerPrice").filter(|_| known);
            OrderStatusReport {
                venue: VENUE.to_string(),
                symbol: o.get("symbol").and_then(|s| s.as_str()).unwrap_or("").to_string(),
                venue_order_id: o.get("orderId").map(json_str).unwrap_or_default().into(),
                order_client_id,
                side,
                order_type,
                qty: o.get("qty").and_then(json_number).unwrap_or(0.0),
                filled_qty: o.get("cumExecQty").and_then(json_number).unwrap_or(0.0),
                average_price: o.get("avgPrice").and_then(json_number).unwrap_or(0.0),
                status: normalize_order_status(
                    o.get("orderStatus").and_then(|s| s.as_str()).unwrap_or(""),
                ),
                timestamp: o.get("updatedTime").and_then(json_int).unwrap_or(0),
                price,
                trigger_price,
                trail: None,
                trail_percent: None,
            }
        })
        .collect())
}

/// Whether one `execution/list` row is a fill ([`parse_fills`]'s rule): `execType` `Trade` only. A row with no `execType`
/// is dropped as well (Bybit V5 documents it on every row, so such a row is malformed), and that is said once per process;
/// a known non-trade kind (`Funding` every eight hours) is routine and says nothing.
fn is_trade_row(row: &Value) -> bool {
    match row.get("execType").and_then(Value::as_str).filter(|t| !t.is_empty()) {
        Some(kind) => kind == "Trade",
        None => {
            static SAID: std::sync::Once = std::sync::Once::new();
            SAID.call_once(|| {
                tracing::warn!(
                    venue = VENUE,
                    "an execution/list row carries no `execType`; it is not reported as a fill (only a `Trade` row is), \
                     and this is said once per process"
                );
            });
            false
        }
    }
}

/// `GET /v5/execution/list` rows -> `FillReport`. Unlike binance's `myTrades`/`userTrades`, Bybit
/// DOES echo `orderLinkId` on every execution row (see `execution::resync::execution_fill`), so
/// `order_client_id` is populated straight from the wire here rather than left `None`.
///
/// A row with no `execId` is DROPPED (`filter_map`), not reported. A `FillReport` with no `trade_id`
/// is worse than a missing one: `vike_execution::recon::diff` decides "already known" by looking the id
/// up in `seen_trade_ids`, so an id-less row can never match and instead FABRICATES a `MissingFill`
/// divergence — one of the two kinds the `hybrid` policy AUTO-APPLIES
/// (`vike_tradehub::reconcile_config::auto_applied_kinds`), i.e. it would book the fill a second time
/// with no operator in front of it. Dropping is the safe direction: the worst case is a real fill
/// going unreconciled until a pass sees it with its id. Bybit V5 documents `execId` on every row.
///
/// ⚠ **Only a `Trade` row is a fill** (the rule of `crate::execution::resync`'s `map_bybit_history` and
/// `crate::execution::event_mapper`'s `map_execution`). The list also carries `Funding` rows (qty = the position's size, price = the
/// funding mark, no `orderLinkId`), `BustTrade`/`AdlTrade` (the event path books those as `PositionLiquidated`, never a
/// fill) and the rest of V5's `execType` words (`Delivery`, `Settle`, `BlockTrade`, `MovePosition`), none of which an
/// order of this bridge makes (`crates/bridges/bybit/CLAUDE.md`: a delivery is not folded). Reported as fills, a `Funding` row raised a `MissingFill` with no order client id at every funding
/// instant on the CI box's bybit demo account (00:00 and 08:00 UTC, 2026-11-09), which `hybrid` would AUTO-APPLY as a phantom
/// trade every eight hours. A liquidation's position change still reaches reconcile, as a position drift.
pub fn parse_fills(body: &str) -> Result<Vec<FillReport>, String> {
    let rows = list_rows(body)?;
    Ok(rows
        .iter()
        .filter(|f| is_trade_row(f))
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
                order_client_id: f
                    .get("orderLinkId")
                    .and_then(|c| c.as_str())
                    .filter(|c| !c.is_empty())
                    .map(|c| c.to_string()),
                side: sign_default_sell(f.get("side").and_then(|s| s.as_str()).unwrap_or("")),
                last_qty: f.get("execQty").and_then(json_number).unwrap_or(0.0),
                last_price: f.get("execPrice").and_then(json_number).unwrap_or(0.0),
                commission: vike_bridge_core::json::get_fee_f64(f, "execFee", VENUE),
                commission_asset: f
                    .get("feeCurrency")
                    .and_then(|c| c.as_str())
                    .unwrap_or("")
                    .to_string(),
                liquidity_side: LiquiditySide::from_maker(maker),
                timestamp: f.get("execTime").and_then(json_int).unwrap_or(0),
            })
            .filter(vike_bridge_core::event_map::fill_report_is_bookable)
        })
        .collect())
}

/// `GET /v5/position/list` rows -> `PositionStatusReport`. `size` is UNSIGNED on the wire — the
/// SIGN comes from `side` ("Buy" -> +1, else -1); a flat (`size == 0`) row's sign is forced to `0.0`
/// (never a signed zero) rather than trusting whatever `side` a flat leg happens to carry. Every
/// row is kept, including that flat leg — see the module doc for why filtering it out would hide
/// a real divergence here.
pub fn parse_positions(body: &str) -> Result<Vec<PositionStatusReport>, String> {
    let rows = list_rows(body)?;
    Ok(rows
        .iter()
        .map(|p| {
            let size = p.get("size").and_then(json_number).unwrap_or(0.0).abs();
            let sign =
                if p.get("side").and_then(|s| s.as_str()) == Some("Buy") { 1.0 } else { -1.0 };
            let qty = if size == 0.0 { 0.0 } else { sign * size };
            PositionStatusReport {
                venue: VENUE.to_string(),
                symbol: p.get("symbol").and_then(|s| s.as_str()).unwrap_or("").to_string(),
                position_side: crate::execution::event_mapper::pside_from_index(p),
                qty,
                average_price: p.get("avgPrice").and_then(json_number).unwrap_or(0.0),
                timestamp: p.get("updatedTime").and_then(json_int).unwrap_or(0),
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

/// **A spot pair's holding as a position** (decision 0231): the `walletBalance` of its BASE coin from `GET
/// /v5/account/wallet-balance` (`coin=<base>`), booked `Cash` (fully funded, never liquidated, as a Hyperliquid spot
/// pair is). The report names the pair by its user name (`DOGEUSDT`, decision 0290). A quantity under ONE step of the
/// pair (dust a fee or a transfer left) is a FLAT row, and a coin the answer does not list is none held: like a
/// derivative's, the row must be present for `recon::diff` to see a locally open position the venue has since
/// flattened. The wallet states no cost basis, so `avg_px` is `0.0` (an adopted holding has no entry price).
/// A body with no `list` is malformed (`Err`), never read as flat. The row is the one spot constructor's
/// (`PositionStatusReport::spot_holding`); the coin's figure is [`parse_wallet_balance_of`]'s.
pub fn spot_holding(
    body: &str,
    symbol: &str,
    base: &str,
    step: f64,
    ts: i64,
) -> Result<PositionStatusReport, String> {
    let held = parse_wallet_balance_of(body, base)?.unwrap_or(0.0);
    Ok(PositionStatusReport::spot_holding(VENUE, symbol, held, step, ts))
}

/// A row of ONE instrument's read is named by the USER name `user` it was asked for (decision 0290: a linear
/// `BTCUSDT` row of a `BTCUSDT.P` read is `BTCUSDT.P`) when it names the venue's `wire`; a row naming another venue
/// symbol keeps it, as before (the mounted read passes such a row through, an admitted read drops it).
fn user_named(row_symbol: &mut String, wire: &str, user: &str) {
    if row_symbol.eq_ignore_ascii_case(wire) {
        *row_symbol = user.to_string();
    }
}

/// [`crate::execution::event_mapper::net_spot_base_fee`]'s rule over a reconcile [`FillReport`] of a SPOT pair
/// (`symbol` already its user name, `DOGEUSDT`).
fn net_spot_fill_report(fill: &mut FillReport) {
    if let Some(net) = crate::execution::event_mapper::SpotNet::of(
        &fill.symbol,
        true,
        fill.side,
        &fill.commission_asset,
        fill.commission,
        fill.last_qty,
        fill.last_price,
    ) {
        fill.last_qty = net.qty;
        fill.commission = net.commission;
        fill.commission_asset = net.asset;
    }
}

/// Bybit v5 `tradeMode` (`0` = cross, `1` = isolated; number OR string on the wire) ->
/// [`MarginMode`] (margin-mode step-2, read-side only). Anything other than an explicit `1` —
/// including an absent field (UTA portfolio accounts omit a meaningful tradeMode) — is the
/// fail-safe `Cross`, byte-identical to pre-field behavior. Read by [`parse_positions`].
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
/// excludes margin currently locked by open positions/orders) — the pure, fixture-tested parser
/// this `ReconcileClient::fetch_balance` override delegates to. `None` when no USDT coin row is present;
/// malformed JSON (or a missing `list` array) is a hard `Err`, never a panic.
pub fn parse_wallet_balance(body: &str) -> Result<Option<f64>, String> {
    parse_wallet_balance_of(body, "USDT")
}

/// [`parse_wallet_balance`] for the `walletBalance` of ANY coin's row: the USDT one for a linear mount, the
/// settle coin's for a coin-margined one (decision 0258: that engine's books are kept in the coin, and
/// `walletBalance` is the coin's cash, its realized PnL, fees and funding already in it). `None` when the
/// answer lists no row for `coin`.
pub fn parse_wallet_balance_of(body: &str, coin: &str) -> Result<Option<f64>, String> {
    let v: Value = serde_json::from_str(body).map_err(|e| e.to_string())?;
    let list = v
        .get("list")
        .and_then(|l| l.as_array())
        .ok_or_else(|| "expected a `list` array".to_string())?;
    for account in list {
        if let Some(coins) = account.get("coin").and_then(|c| c.as_array())
            && let Some(row) =
                coins.iter().find(|c| c.get("coin").and_then(|x| x.as_str()) == Some(coin))
        {
            return Ok(row.get("walletBalance").and_then(json_number));
        }
    }
    Ok(None)
}

/// The `accountType` of the unified wallet.
pub const UNIFIED: &str = "UNIFIED";
/// The `accountType` of the separate inverse-derivatives wallet of a classic or UTA 1.0 account.
pub const CONTRACT: &str = "CONTRACT";

/// **The wallet a coin-margined (inverse) contract's coin is kept in**, from the `result` of `GET /v5/account/info`
/// (`unifiedMarginStatus`; `bybit-exchange.github.io/docs/v5/acct-mode`, read 2026-11-09): a classic account (`1`) and
/// UTA 1.0 (`3`, `4` its pro version) trade inverse contracts in a separate account, [`CONTRACT`]; UTA 2.0 (`5`, `6`)
/// inside the unified wallet, where `CONTRACT` is not supported. Any other answer, or none, is an `Err`: the wallet
/// is never guessed. PURE.
pub fn inverse_wallet_of(
    account_info: &Value,
) -> Result<&'static str, crate::refusals::BybitAccountRefusal> {
    match account_info.get("unifiedMarginStatus").and_then(json_int) {
        Some(1 | 3 | 4) => Ok(CONTRACT),
        Some(5 | 6) => Ok(UNIFIED),
        status => Err(crate::refusals::BybitAccountRefusal::UnknownAccountVersion {
            status: status.map_or_else(|| "absent".to_string(), |s| s.to_string()),
        }),
    }
}

/// Whether the venue refused an `accountType` because the account keeps the unified wallet ALONE. Measured on the
/// demo host 2026-11-09 (`tests/bybit_smokes/bybit_venue_read_smoke.rs`): its account states `unifiedMarginStatus`
/// 3 (UTA 1.0) and answers `accountType=CONTRACT`, with or without `coin`, `10001` "accountType only support
/// UNIFIED.". The venue itself names the one wallet, so that is where the inverse book is; any other refusal fails
/// the read.
pub fn only_unified(e: &VenueApiError) -> bool {
    e.code == 10001 && e.msg.to_ascii_lowercase().contains("only support unified")
}

/// Whether the venue WITHHELD the account's fee rate rather than refused the read: `10001` with an EMPTY message.
/// MEASURED on the demo host for every linear and inverse spelling
/// (`crates/bridges/bybit/tests/bybit_smokes/bybit_inverse_smoke.rs`'s `probe_fee_rate`, 2026-11-08, and
/// `category=linear&symbol=BTCUSDT`), and an empty message again at the CI box's v0.1.102
/// start (2026-11-14) for both demo accounts (`BTCUSDT` linear, `BTCUSD` inverse), while `spot` and the mainnet
/// account answer. A `10001` WITH a message is a parameter error: a failure.
fn fee_rate_withheld(e: &VenueApiError) -> bool {
    e.code == 10001 && e.msg.trim().is_empty()
}

/// `GET /v5/account/fee-rate` `result.list[0]` -> live [`FeeSchedule`] from
/// `makerFeeRate`/`takerFeeRate` (fractions, e.g. `"0.00055"`). `Ok(None)` when the list is empty
/// or a rate is absent (fail-soft → static default); a missing `list` is a hard `Err` (via
/// [`list_rows`]).
pub fn parse_fee_rate(body: &str) -> Result<Option<FeeSchedule>, String> {
    let rows = list_rows(body)?;
    Ok(rows.first().and_then(|r| {
        let maker = r.get("makerFeeRate").and_then(json_number)?;
        let taker = r.get("takerFeeRate").and_then(json_number)?;
        Some(FeeSchedule::from_fractions(maker, taker))
    }))
}

// --- the factory -----------------------------------------------------------------------------

/// The venue -> `ReconcileClient` factory (ReconFactory seam, wave-2 task 6) — moved verbatim from
/// `vike_tradehub_core::build_recon_client`'s `"bybit"` match arm (called from `crate::adapter` since
/// docs/decisions/0096): a FRESH stateless-HMAC `ureq` REST
/// client dedicated to reconcile reads, reusing the SAME `Credentials` the venue's
/// `ExecutionClient`/`spawn_with_recorder` already builds from. Bybit's reconcile REST binds to
/// `rest_base` — the SAME [`crate::execution::rest::BybitUrls::rest`] (decision 0095: `live` means mainnet) the
/// start threaded into the exec path, resolved ONCE by `crate::adapter`'s `VenueAdapterBybit::start`
/// from the account's rows, so reconcile stays in lockstep with exec instead of re-reading anything
/// for itself. Always `Some` —
/// construction here is pure/infallible (no network); `Option` is kept so the return type matches
/// every other bridge's `recon_client` (deribit's genuinely can fail).
pub fn recon_client(
    creds: &vike_secrets::credentials::Credentials,
    symbol: &str,
    rest_base: &str,
) -> Option<Box<dyn ReconcileClient>> {
    reconcile_client_sharing(creds, symbol, rest_base, Arc::default(), &VenueClock::default())
}

/// [`recon_client`] over an admitted-symbol list the CALLER also holds: the adapter hands the same `Arc`
/// to the exec thread, so the symbol the engine tells this client about (`ReconcileClient::admit_symbol`) is
/// also one whose history the reconnect-resync replays. `clock` is the start's signing clock (owner ruling B-4,
/// `crate::transport::client::rest_transport_on`), so the account's reconcile reads sign on the time its orders do.
pub fn reconcile_client_sharing(
    creds: &vike_secrets::credentials::Credentials,
    symbol: &str,
    rest_base: &str,
    admitted: Arc<ReconcileAdmittedSymbols>,
    clock: &VenueClock,
) -> Option<Box<dyn ReconcileClient>> {
    // Reconcile reads go to the SAME host the exec path binds to, so a mainnet start reconciles
    // against the mainnet account and a demo start against the demo one.
    Some(Box::new(
        ReconcileClientBybit::new(
            BybitV5Signer::new(creds, clock.ms()),
            crate::transport::client::rest_transport_on(clock)
                .with_rate_gate(crate::transport::ratelimit::rest_rate_gate()),
            rest_base,
            symbol.to_string(),
        )
        .with_admitted(admitted),
    ))
}

// --- the client ----------------------------------------------------------------------------

/// One reconcile client per venue-symbol — the SAME per-symbol shape `execution::rest::VenueRestBybit` uses,
/// but carrying only what a report fetch needs (signer + transport + base_url + symbol; no
/// order-formatting `SymbolProperties`/leverage).
pub struct ReconcileClientBybit<T: RestTransportJson> {
    pub signer: BybitV5Signer,
    pub transport: T,
    pub base_url: String,
    pub symbol: String,
    /// The symbols the engine admitted beyond `symbol` (`ReconcileClient::admit_symbol`), USER names as the
    /// engine spells them (decision 0290). ONE list, shared with the exec thread (`with_admitted`): the symbols
    /// this client is told are the ones the reconnect-resync replays (decision 0174's reopener).
    admitted: Arc<ReconcileAdmittedSymbols>,
    /// The pass-wide pause after a rate limit on an admitted read.
    pause: RateGatePause,
    /// A SPOT pair's `(base coin, quantity step)` from its `instruments-info` row, read once per pair (decision
    /// 0231): the holding of a pair is the wallet row of its base coin, and a quantity under one step is flat.
    spot_rows: Mutex<std::collections::HashMap<String, (String, f64)>>,
    /// The settle coin of a COIN-MARGINED mounted symbol (`BTCUSD` and `BTCUSDZ26` settle in `BTC`), `None` for
    /// every other: that engine keeps its books in the coin (decision 0258), so the balance this client reports
    /// is the coin's wallet and the fee rate is read in the inverse book. Read off the NAME, like every book this
    /// client reads in (`vike_catalog::inverse_settle_coin`, held to the venue's rows by `catalog_tests`).
    coin: Option<String>,
    /// The `accountType` that `coin`'s wallet is read in ([`inverse_wallet_of`]), kept once known: asked of
    /// `account/info` at the first balance read of the start (the balance seed), and turned to [`UNIFIED`] when the
    /// venue answers that it keeps no other wallet ([`only_unified`]). A failed or unknown answer keeps nothing.
    inverse_wallet: Mutex<Option<&'static str>>,
    /// Whether a fee-rate read that brought no rate (failed or withheld) has been said already
    /// ([`ReconcileClient::fetch_fee_rates`]).
    fee_read_failed_said: std::sync::atomic::AtomicBool,
}

impl<T: RestTransportJson> ReconcileClientBybit<T> {
    pub fn new(
        signer: BybitV5Signer,
        transport: T,
        base_url: impl Into<String>,
        symbol: impl Into<String>,
    ) -> Self {
        let symbol = symbol.into();
        let coin = vike_catalog::inverse_settle_coin("bybit", &symbol);
        ReconcileClientBybit {
            signer,
            transport,
            base_url: base_url.into(),
            symbol,
            admitted: Arc::default(),
            pause: RateGatePause::default(),
            spot_rows: Mutex::default(),
            coin,
            inverse_wallet: Mutex::default(),
            fee_read_failed_said: std::sync::atomic::AtomicBool::new(false),
        }
    }

    /// The wallet the inverse book's coin is read in: the kept answer, else `account/info`'s ([`inverse_wallet_of`]).
    /// A read that fails, or a status this bridge does not know, refuses the balance read in words.
    fn inverse_wallet(&self) -> Result<&'static str, String> {
        if let Some(kept) = *self.inverse_wallet.lock().unwrap_or_else(PoisonError::into_inner) {
            return Ok(kept);
        }
        let info = self.result(PATH_ACCOUNT_INFO, &[]).map_err(|e| {
            format!(
                "{e}: the wallet of the inverse book is not guessed, so its balance is not read"
            )
        })?;
        let info: Value = serde_json::from_str(&info).map_err(|e| e.to_string())?;
        let wallet = inverse_wallet_of(&info).map_err(|refusal| refusal.to_string())?;
        tracing::info!(
            venue = VENUE, symbol = %self.symbol, unified_margin_status = %info["unifiedMarginStatus"], wallet,
            "the inverse book's coin is read in this wallet (accountType)"
        );
        *self.inverse_wallet.lock().unwrap_or_else(PoisonError::into_inner) = Some(wallet);
        Ok(wallet)
    }

    /// The `(category, venue symbol, is spot)` a USER name is read in: its KEPT row's book and venue name
    /// (`crate::instrument::class::venue_symbol`, decision 0290): `BTCUSDT.P` is `linear` `BTCUSDT`, the pair
    /// `BTCUSDT` `spot` `BTCUSDT`. Never a guessed book: a read in the wrong one answers an empty list, which the
    /// restore planner reads as "gone". With no kept row, the MOUNTED name fails the read in words, and an ADMITTED
    /// one (`None`) reads as holding nothing, said at WARN, as a symbol the venue no longer lists does
    /// ([`ADMITTED_READ`]): the list dropped it.
    fn book_of(
        &self,
        symbol: &str,
    ) -> Result<Option<(&'static str, String, bool)>, ReconcileReadError<FailedRequest>> {
        match crate::instrument::class::venue_symbol(symbol) {
            Ok((wire, book)) => {
                Ok(Some((book.wire(), wire, book == crate::history::Category::Spot)))
            }
            Err(why) if !symbol.eq_ignore_ascii_case(&self.symbol) => {
                tracing::warn!(
                    venue = VENUE, %symbol,
                    "reconcile: an admitted symbol has no kept row ({why}); reading it as holding nothing"
                );
                Ok(None)
            }
            Err(why) => Err(ReconcileReadError::Parse(why)),
        }
    }

    /// A spot pair's `(base coin, quantity step)`: its `instruments-info` row, read once and kept.
    fn spot_row(&self, pair: &str) -> Result<(String, f64), ReconcileReadError<FailedRequest>> {
        if let Some(kept) = self.spot_rows.lock().unwrap_or_else(PoisonError::into_inner).get(pair)
        {
            return Ok(kept.clone());
        }
        let body = self.result_raw(
            crate::execution::rest::PATH_INSTRUMENTS,
            &[("category", json!("spot")), ("symbol", json!(pair))],
        )?;
        let body: Value =
            serde_json::from_str(&body).map_err(|e| ReconcileReadError::Parse(e.to_string()))?;
        let row = body.get("list").and_then(Value::as_array).and_then(|l| l.first()).ok_or_else(
            || ReconcileReadError::Parse(format!("{VENUE} lists no spot pair {pair}")),
        )?;
        let base = row.get("baseCoin").and_then(Value::as_str).unwrap_or("").to_string();
        let step =
            row.get("lotSizeFilter").and_then(|f| f.get("basePrecision")).and_then(json_number);
        match (base.is_empty(), step) {
            (false, Some(step)) if step > 0.0 => {
                self.spot_rows
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .insert(pair.to_string(), (base.clone(), step));
                Ok((base, step))
            }
            _ => Err(ReconcileReadError::Parse(format!(
                "{VENUE} spot pair {pair}: the row carries no base coin or quantity step"
            ))),
        }
    }

    /// Keep the admitted-symbol list in `admitted` instead of a private one, so the holder of the other
    /// end (the exec thread's resync and gap-sentinel) sees every symbol this client is told.
    #[must_use]
    pub fn with_admitted(mut self, admitted: Arc<ReconcileAdmittedSymbols>) -> Self {
        self.admitted = admitted;
        self
    }

    /// `order/realtime` for ONE symbol, EVERY page of it: the venue lists at most [`ORDERS_PAGE_LIMIT`]
    /// rows a request, so a busy symbol is walked by `nextPageCursor` until the venue says it is done.
    /// The restore planner reads an order a successful report lacks as gone, so a read that stopped
    /// short must FAIL, never return what it has: a repeated cursor and [`ORDERS_MAX_PAGES`] are errors
    /// (and so is any page failing, a rate limit included, which [`ADMITTED_READ`] judges as before).
    ///
    /// The walk ends on: an empty page, a SHORT page (fewer than the limit asked: the venue still
    /// hands a cursor on the last page, measured on the demo host 2026-10-24, so the cursor alone
    /// would cost a second request every pass), or no cursor. Only a full page with a cursor goes on.
    fn orders_of(
        &self,
        symbol: &str,
    ) -> Result<Vec<OrderStatusReport>, ReconcileReadError<FailedRequest>> {
        let Some((category, wire, _)) = self.book_of(symbol)? else { return Ok(Vec::new()) };
        let scope = [("category", json!(category)), ("symbol", json!(wire))];
        let mut reports = self.order_pages(&scope, symbol, false)?;
        for report in &mut reports {
            user_named(&mut report.symbol, &wire, symbol);
        }
        Ok(reports)
    }

    /// **Every open order of the ACCOUNT** (an operator's read, `crate::reconcile::venue_read`; the reconcile reads above stay
    /// per symbol): one [`Self::order_pages`] walk per book of [`ACCOUNT_BOOKS`], no `orderFilter` (so a resting
    /// conditional is listed beside the active orders, as the reconcile read is), each row named by its USER name, the
    /// kept row of its book's venue name (`crate::instrument::class::user_symbol_or_wire`: a linear `BTCUSDT` is
    /// `BTCUSDT.P`, a spot one the pair `BTCUSDT`). Any walk failing fails the read, a venue error as [`FailedRequest`] says it (the
    /// request names the book). On the short-timeout path, as the balance read: an operator's read stalls a gatewayhub
    /// thread five seconds a request at most.
    pub(crate) fn account_open_orders(&self) -> Result<Vec<OrderStatusReport>, String> {
        let mut reports = Vec::new();
        for (category, settle_coin) in ACCOUNT_BOOKS {
            let mut scope = vec![("category", json!(category))];
            scope.extend(settle_coin.map(|coin| ("settleCoin", json!(coin))));
            let book =
                format!("{category}{}", settle_coin.map(|c| format!(" {c}")).unwrap_or_default());
            let rows = self.order_pages(&scope, &book, true).map_err(|fail| match fail {
                ReconcileReadError::Venue(e) => e.to_string(),
                ReconcileReadError::Parse(why) => format!("{VENUE} order/realtime ({book}): {why}"),
            })?;
            let book = crate::history::Category::of_word(category);
            reports.extend(rows.into_iter().map(|mut r| {
                r.symbol = crate::instrument::class::user_symbol_or_wire(book, &r.symbol);
                r
            }));
        }
        Ok(reports)
    }

    /// The paged `order/realtime` walk of [`Self::orders_of`] over the request `scope` (the `category` and the symbol
    /// or settle coin it names), `what` naming the scope in an error; `short` takes the short-timeout path.
    fn order_pages(
        &self,
        scope: &[(&str, Value)],
        what: &str,
        short: bool,
    ) -> Result<Vec<OrderStatusReport>, ReconcileReadError<FailedRequest>> {
        self.cursor_pages(&ORDER_PAGES, scope, what, short)
    }

    /// One `pages` endpoint, EVERY page of it, over the request `scope`, `what` naming the scope in an error;
    /// `short` takes the short-timeout path. The rule is [`Self::orders_of`]' doc: the walk ends on an empty or
    /// SHORT page (fewer ROWS than asked, before the parser drops any) or no cursor, and fails on a repeated cursor,
    /// a chain past the page cap and any page failing.
    fn cursor_pages<R>(
        &self,
        pages: &CursorPages<R>,
        scope: &[(&str, Value)],
        what: &str,
        short: bool,
    ) -> Result<Vec<R>, ReconcileReadError<FailedRequest>> {
        let CursorPages { path, name, parse, page_limit, max_pages } = *pages;
        let mut reports = Vec::new();
        let mut spent: Vec<String> = Vec::new();
        for _ in 0..max_pages {
            let mut params = scope.to_vec();
            params.push(("limit", json!(page_limit)));
            if let Some(cursor) = spent.last() {
                params.push(("cursor", json!(percent_decode(cursor))));
            }
            let body = if short {
                self.result_requery_raw(path, &params)?
            } else {
                self.result_raw(path, &params)?
            };
            let page = parse(&body).map_err(ReconcileReadError::Parse)?;
            let full = list_rows(&body).map_err(ReconcileReadError::Parse)?.len() >= page_limit;
            reports.extend(page);
            let Some(next) = next_cursor(&body).filter(|_| full) else { return Ok(reports) };
            if spent.contains(&next) {
                return Err(ReconcileReadError::Parse(format!(
                    "{VENUE} {name} for {what} repeated a page cursor after {} rows: \
                     the read is refused rather than reported as complete",
                    reports.len()
                )));
            }
            spent.push(next);
        }
        Err(ReconcileReadError::Parse(format!(
            "{VENUE} {name} for {what} is still paging after {max_pages} pages of \
             {page_limit} rows: the read is refused rather than reported as complete"
        )))
    }

    /// `execution/list` for ONE symbol since `since` (0 = no lower bound), EVERY page of it ([`FILL_PAGES`], owner
    /// ruling F-2): it used to be one page with no `limit` (the venue's 50 by default) and no cursor, so a busy
    /// window's 51st fill was missing from a report read as complete.
    fn fills_of(
        &self,
        symbol: &str,
        since: i64,
    ) -> Result<Vec<FillReport>, ReconcileReadError<FailedRequest>> {
        let Some((category, wire, spot)) = self.book_of(symbol)? else { return Ok(Vec::new()) };
        let mut params: Vec<(&str, Value)> =
            vec![("category", json!(category)), ("symbol", json!(wire))];
        if since > 0 {
            params.push(("startTime", json!(since)));
        }
        let mut fills = self.cursor_pages(&FILL_PAGES, &params, symbol, true)?;
        for fill in &mut fills {
            user_named(&mut fill.symbol, &wire, symbol);
            if spot {
                net_spot_fill_report(fill);
            }
        }
        Ok(fills)
    }

    /// `position/list` for ONE symbol (the flat row stays present). A SPOT pair has no position: its holding is the
    /// wallet row of its base coin ([`spot_holding`]).
    fn positions_of(
        &self,
        symbol: &str,
    ) -> Result<Vec<PositionStatusReport>, ReconcileReadError<FailedRequest>> {
        let Some((category, wire, spot)) = self.book_of(symbol)? else { return Ok(Vec::new()) };
        if spot {
            let (base, step) = self.spot_row(&wire)?;
            let body = self.result_raw(
                PATH_ACCOUNT,
                &[("accountType", json!("UNIFIED")), ("coin", json!(base))],
            )?;
            return spot_holding(&body, symbol, &base, step, vike_model::now_ms())
                .map(|r| vec![r])
                .map_err(ReconcileReadError::Parse);
        }
        let body = self.result_raw(
            PATH_POSITION_LIST,
            &[("category", json!(category)), ("symbol", json!(wire))],
        )?;
        let mut reports = parse_positions(&body).map_err(ReconcileReadError::Parse)?;
        for report in &mut reports {
            user_named(&mut report.symbol, &wire, symbol);
        }
        Ok(reports)
    }

    /// The normal signed GET path, envelope-unwrapped to `result` and re-serialized once for the
    /// pure parsers — the same `signed` + `unwrap_envelope` pairing `VenueRestBybit::call` already
    /// uses for order/position reads. A failure is [`FailedRequest`]'s words.
    fn result(&self, path: &str, params: &[(&str, Value)]) -> Result<String, String> {
        self.result_raw(path, params).map_err(|e| e.to_string())
    }

    /// [`Self::result`] keeping the venue's error (code and message) beside the request, for a read that judges it.
    fn result_raw(&self, path: &str, params: &[(&str, Value)]) -> Result<String, FailedRequest> {
        self.transport
            .signed(&self.base_url, path, "GET", params, &self.signer)
            .and_then(unwrap_envelope)
            .map(|v| v.to_string())
            .map_err(|error| FailedRequest::new("GET", path, params, error))
    }

    /// The short-timeout, rate-gate-EXEMPT path — the SAME one the crate's audit-A3 resync
    /// (`get_execution_history`) already uses for this exact heavier history endpoint, so a
    /// resync burst can't blow the per-symbol order-rate budget submit/cancel depend on.
    fn result_requery_raw(
        &self,
        path: &str,
        params: &[(&str, Value)],
    ) -> Result<String, FailedRequest> {
        self.transport
            .signed_requery(&self.base_url, path, "GET", params, &self.signer)
            .and_then(unwrap_envelope)
            .map(|v| v.to_string())
            .map_err(|error| FailedRequest::new("GET", path, params, error))
    }
}

/// The judgement of ONE admitted symbol's read (`vike_bridge_core::reconcile_admitted`), on bybit's facts. A symbol
/// bybit no longer lists answers a parameter error naming the symbol (`10001`, "symbol invalid"): it holds nothing,
/// so that is no rows, with a warning, and the account's other reads still go through (one delisted symbol must not
/// blind the pass for good: the list never shrinks). A rate limit (the taxonomy's `RateLimited`, or the Cloudflare IP
/// block bybit answers as `403`) starts the pass-wide pause. Anything else fails the whole fetch, as before. Each is
/// judged on the venue's code and message alone; a venue error's TEXT is [`FailedRequest`]'s (it was the message
/// alone, which the demo host sends empty). ⚠ The `10001` wording is bybit's documented one, not live-proven for a
/// delisted linear symbol.
const ADMITTED_READ: ReconcileAdmittedReadSpec<FailedRequest> = ReconcileAdmittedReadSpec {
    not_listed: |e| e.error.code == 10001 && e.error.msg.to_ascii_lowercase().contains("symbol"),
    rate_limited: |e| {
        e.error.kind_with(|e| crate::transport::error_codes::TAXONOMY.lookup(e))
            == ErrorKind::RateLimited
    },
    text: |e| e.to_string(),
};

/// A failed read as its pass error ([`FailedRequest`]'s words for a venue error).
fn read_text(fail: ReconcileReadError<FailedRequest>) -> String {
    fail.into_text(|e| e.to_string())
}

impl<T: RestTransportJson + Send> ReconcileClient for ReconcileClientBybit<T> {
    /// `since` is a no-op here: `/v5/order/realtime` has no time filter and reports only the
    /// CURRENTLY open + recently-closed set — a fully-closed order that fell out of this set is
    /// exactly what the fill/position reports (not this one) reconcile.
    ///
    /// **Every ADMITTED symbol is read too**, one `order/realtime?symbol=` walk each (every page of it,
    /// `orders_of`), the mounted symbol's own request shape. The `settleCoin`-wide form would be one call but is a different report (it
    /// lists open orders only, where this one also carries recently closed ones), and any failure
    /// fails the whole fetch: the restore planner reads an order a successful report lacks as gone.
    /// With none admitted the request list is the mounted symbol's alone.
    fn fetch_order_status_reports(&self, _since: i64) -> Result<Vec<OrderStatusReport>, String> {
        not_backing_off(VENUE, &self.pause)?;
        let mut reports = self.orders_of(&self.symbol).map_err(read_text)?;
        for symbol in self.admitted.symbols() {
            let rows = ADMITTED_READ.admitted_rows(
                VENUE,
                &symbol,
                &self.pause,
                self.orders_of(&symbol),
            )?;
            reports.extend(rows.into_iter().filter(|r| r.symbol.eq_ignore_ascii_case(&symbol)));
        }
        Ok(reports)
    }

    /// Bybit's `execId` is unique account-wide and the user-data stream folds it bare, so an
    /// admitted symbol's fill keeps its id as is (unlike binance's per-symbol `t`).
    fn fetch_fill_reports(&self, since: i64) -> Result<Vec<FillReport>, String> {
        not_backing_off(VENUE, &self.pause)?;
        let mut reports = self.fills_of(&self.symbol, since).map_err(read_text)?;
        for symbol in self.admitted.symbols() {
            let rows = ADMITTED_READ.admitted_rows(
                VENUE,
                &symbol,
                &self.pause,
                self.fills_of(&symbol, since),
            )?;
            reports.extend(rows.into_iter().filter(|r| r.symbol.eq_ignore_ascii_case(&symbol)));
        }
        Ok(reports)
    }

    fn fetch_position_status_reports(&self) -> Result<Vec<PositionStatusReport>, String> {
        not_backing_off(VENUE, &self.pause)?;
        let mut reports = self.positions_of(&self.symbol).map_err(read_text)?;
        for symbol in self.admitted.symbols() {
            let rows = ADMITTED_READ.admitted_rows(
                VENUE,
                &symbol,
                &self.pause,
                self.positions_of(&symbol),
            )?;
            reports.extend(rows.into_iter().filter(|r| r.symbol.eq_ignore_ascii_case(&symbol)));
        }
        Ok(reports)
    }

    /// **Claims to list trigger orders** (decision 0192), so the restore planner may read a restored
    /// stop this client's report does not list as gone once the quiet passes have counted. What the
    /// claim rests on, each pinned by a test: the read sends no `orderFilter`, so a resting conditional
    /// is listed beside the active orders (measured on the demo host); it pages and FAILS rather than
    /// truncating (`orders_of`); the one trigger kind `submit` places is the stop, which
    /// `conditional_order_type` types back from `stopOrderType` (the conformance harness' resting-kind
    /// row, `tests/offline/claim_prereqs.rs`); and a stop that fired while the node was down shows as a
    /// fill carrying its order client id (`execution/list` echoes `orderLinkId`; measured on the demo account), which
    /// the planner joins before it ever reads silence.
    fn lists_trigger_orders(&self) -> bool {
        true
    }

    /// Remember that the engine admitted `symbol`, its USER name as the engine spells it (decision 0290: `.P` is part
    /// of the name, `BTCUSDT.P` and the pair `BTCUSDT` are two instruments), so every later pass reads it too, in its
    /// kept row's book. The mounted symbol, an empty one, a repeat and a 17th are ignored
    /// (`ReconcileAdmittedSymbols::admit`).
    fn admit_symbol(&self, symbol: &str) {
        self.admitted.admit_logged(&symbol.to_ascii_uppercase(), &self.symbol, VENUE, "a symbol");
    }

    /// Forget a symbol the engine gave back (decision 0205), spelled as `admit_symbol` takes it.
    fn forget_symbol(&self, symbol: &str) {
        self.admitted.forget_logged(&symbol.to_ascii_uppercase(), VENUE, "a symbol");
    }

    /// The symbols this client was told and reads on every pass beyond the mounted one: a real claim,
    /// which the gatewayhub names in its reconcile reply (`gw_trade_reconcile_read`) so the node claims only
    /// what the daemon's client reads.
    fn admitted_symbols(&self) -> Option<Vec<String>> {
        Some(self.admitted.symbols())
    }

    /// Balance meaning (pinned per the task brief): the UNIFIED account's USDT `walletBalance`
    /// from `/v5/account/wallet-balance`, matching
    /// binance perp's "wallet balance, not available" choice (Bybit's UNIFIED account has no
    /// separate spot/perp split the way binance does). A light, unfiltered account read — rides
    /// the normal `signed`/`result` (rate-gated) path, same as `order/realtime`/`position/list`.
    ///
    /// ⚠ **A COIN-MARGINED mount reports its SETTLE COIN's `walletBalance`** (decision 0258: the engine keeps its
    /// books in that coin and adopts this figure), asked for that coin alone (`coin=`), in the wallet the account's
    /// version keeps an inverse book in ([`inverse_wallet_of`]: `CONTRACT` on a classic or UTA 1.0 account,
    /// `UNIFIED` on UTA 2.0). An account that states UTA 1.0 but answers `CONTRACT` with "only support UNIFIED"
    /// (the demo host, measured 2026-11-09: [`only_unified`]) is read in the unified wallet, the one the venue
    /// names. An unreadable or unknown version FAILS the read: no wallet is guessed. On a unified wallet the venue
    /// may draw such a contract's margin from the other collateral (decision 0231's capture), which this coin figure
    /// does not count.
    ///
    /// A failed request carries its outcome under bybit's own table (`crate::transport::error_codes::TAXONOMY`): the
    /// Cloudflare `403` (an IP block) is not now and `10000` outcome unknown, never a refused key.
    fn fetch_balance(&self) -> Result<Option<f64>, ReconcileFetchError> {
        let judged = |e: FailedRequest| {
            let kind = vike_bridge_core::error_kind::classify_venue(
                Some(&crate::transport::error_codes::TAXONOMY),
                &e.error,
            );
            ReconcileFetchError::new(kind.failure_outcome(), e.to_string())
        };
        let Some(coin) = self.coin.as_deref() else {
            let body = self
                .result_raw(PATH_ACCOUNT, &[("accountType", json!(UNIFIED))])
                .map_err(judged)?;
            return parse_wallet_balance(&body).map_err(ReconcileFetchError::unclassified);
        };
        let wallet = self.inverse_wallet().map_err(ReconcileFetchError::unclassified)?;
        let read = |wallet: &str| {
            self.result_raw(PATH_ACCOUNT, &[("accountType", json!(wallet)), ("coin", json!(coin))])
        };
        let body = match read(wallet) {
            Err(e) if wallet == CONTRACT && only_unified(&e.error) => {
                tracing::info!(
                    venue = VENUE, symbol = %self.symbol, answer = %e.error.msg,
                    "the account states a separate inverse wallet, but the venue keeps the unified wallet alone: \
                     the inverse book's coin is read there"
                );
                *self.inverse_wallet.lock().unwrap_or_else(PoisonError::into_inner) = Some(UNIFIED);
                read(UNIFIED)
            }
            other => other,
        }
        .map_err(judged)?;
        parse_wallet_balance_of(&body, coin).map_err(ReconcileFetchError::unclassified)
    }

    /// Live account-actual fee rates (fee model 4/5): `/v5/account/fee-rate` for the mounted
    /// symbol in its kept row's book under the venue's name (`linear`; `inverse` for a coin-margined mount; `spot`
    /// for a pair, whose rate is not the linear one) — `makerFeeRate`/`takerFeeRate`
    /// fractions. A light, unfiltered account read — rides the normal `signed`/`result_raw`
    /// (rate-gated) path. `Ok(None)` keeps the static default.
    ///
    /// A failed read is said at WARN, once per client, in [`FailedRequest`]'s words (the request, the venue's
    /// code and message): it was the message alone, and the demo host's message is EMPTY, so the caller
    /// (`resolve_fee_schedule`) warned `error=` with nothing after it. ⚠ That empty `10001`
    /// ([`fee_rate_withheld`]) is the venue keeping the rate to itself, not a failure: it is said at
    /// INFO, once, and answers `Ok(None)`, which keeps the same static row.
    fn fetch_fee_rates(&self) -> Result<Option<FeeSchedule>, String> {
        let (wire, book) = crate::instrument::class::venue_symbol(&self.symbol)?;
        let book = book.wire();
        let params = [("category", json!(book)), ("symbol", json!(wire))];
        let read = match self.result_raw(PATH_FEE_RATE, &params) {
            Ok(body) => parse_fee_rate(&body)
                .map_err(|why| format!("{}: {why}", request_words("GET", PATH_FEE_RATE, &params))),
            Err(e) if fee_rate_withheld(&e.error) => {
                if !self.fee_read_failed_said.swap(true, std::sync::atomic::Ordering::Relaxed) {
                    tracing::info!(
                        venue = VENUE, symbol = %self.symbol, category = book, host = %self.base_url,
                        "the venue keeps this account's fee rate to itself in this book (an empty `10001`, the demo \
                         host's answer to every linear and inverse read): this mount is priced by the static bybit \
                         fee row"
                    );
                }
                return Ok(None);
            }
            Err(e) => Err(e.to_string()),
        };
        if let Err(why) = &read
            && !self.fee_read_failed_said.swap(true, std::sync::atomic::Ordering::Relaxed)
        {
            tracing::warn!(
                venue = VENUE, symbol = %self.symbol, category = book, %why,
                "the account's own fee rate could not be read: this mount is priced by the static bybit fee row"
            );
        }
        read
    }
}
