//! OKX `ReconcileClient` (Task 11 of the reconciliation-engine feature) — the venue-facing report
//! seam (`vike_execution::recon::ReconcileClient`) for the V5 SWAP perp, reconciling against:
//!
//!   `GET /api/v5/trade/orders-pending` (order reports — CURRENTLY open only, no time filter;
//!   mirrors `rest::VenueRestOkx`'s own `PATH_POSITIONS`/history endpoints).
//!   `GET /api/v5/trade/orders-algo-pending` (the same order report's second half: the resting
//!   conditional STOPS and TAKE-PROFITS, which `orders-pending` is not documented to list; `ordType`
//!   is REQUIRED), and, while okx's row lists `trailing`, once more, account-wide, with
//!   `ordType=move_order_stop` for the resting TRAILING orders (`ordType` combines only `conditional`
//!   with `oco`).
//!   `GET /api/v5/trade/fills` (fill reports; `begin` is the Unix-ms lower bound when `since > 0`;
//!   paged by `billId`, owner ruling F-2).
//!   `GET /api/v5/trade/orders-algo-history` (`state=effective`, read only when a pass has fills: the
//!   stops that FIRED, whose `ordIdList` names the child order each one spawned. `trade/fills` rows
//!   carry the child's own `clOrdId` and no algo field, so this join files a fired stop's fill under
//!   the stop's client id, [`name_algo_children`]; the fired TRAILING orders are one more,
//!   account-wide read with `ordType=move_order_stop`, behind the same gate).
//!   `GET /api/v5/account/positions` (position reports — contracts→base via `ct_val`, below).
//!
//! Every fetch delegates to a PURE free function (`parse_*`) over the raw JSON `data` array —
//! these are the fixture-tested units (`tests/offline/reconcile_client_parse.rs`, no network). Signed results
//! are unwrapped (`transport::client::unwrap_okx`) to the bare `data` array and re-serialized to a string
//! once, handed to the pure parser: the same small, deliberate cost `vike_binance::reconcile`
//! pays to keep every parser directly testable against a literal wire body.
//!
//! **Contracts → base (the tricky bit):** every OKX SWAP size field (`sz`/`accFillSz` on orders,
//! `fillSz` on fills, `pos` on positions) is quoted in CONTRACTS, not base units — see
//! `rest::VenueRestOkx::to_base` (`contracts * ct_val`, a plain float multiply). That shape was
//! PORTED from the retired Python app's `_to_base`; this line used to say it "matches" that
//! function, which nothing can check any more — no Python survives in this tree
//! (`docs/decisions/0021-python-oracle-retired-vike-is-the-reference.md`). What holds the multiply
//! today is the frozen `fixtures/r6/okx.json` replay in
//! `crates/bridges/okx/tests/offline/r6_okx_parity.rs`, whose
//! `production_reads_the_frozen_rows_through_the_reconcile_client` pins `contracts × ct_val → base`.
//! PRICE fields (`avgPx`/`fillPx`) are already quote-per-base and are
//! NEVER rescaled. `pos` on `/api/v5/account/positions` is ALREADY SIGNED (long > 0, short < 0),
//! same as Binance's `positionAmt` — never re-signed by `posSide`.
//!
//! **Flat positions:** unlike Binance's `positionRisk` (which always echoes every symbol,
//! including a `positionAmt: "0"` row), OKX's `/api/v5/account/positions` OMITS a symbol entirely
//! once its position is flat — no `pos: "0"` row is sent. `recon::diff::diff` iterates the REPORT
//! list to find a local position's counterpart, so a silently-omitted row would make "local still
//! shows a position OKX has since closed" invisible. [`parse_positions`] closes that gap: an empty
//! `data` array synthesizes ONE flat (`qty: 0.0`) row for the queried symbol, so a
//! stale local position is still detectable. Any row OKX DOES send (even a defensive `pos: "0"`
//! one) is kept verbatim — never filtered — for the same reason Binance's parser keeps its flat
//! rows.
//!
//! **Admitted instruments (decisions 0173 and 0174):** an account's engine trades any SWAP of the
//! exchange, admitted on demand, and a restart must see the orders, fills and positions it left there.
//! The client is TOLD each admitted instrument (`ReconcileClient::admit_symbol`, a list bounded by the
//! engine's own cap) and every pass then reads the mounted instrument FIRST, with the requests it
//! always made and the `ctVal` it was built with, and each admitted one after it:
//!
//! * the paged `orders-pending` and the paged `orders-algo-pending`, per instrument (`instId`);
//! * the paged `trade/fills` per instrument, and the fired-stop naming per instrument that has fills (one
//!   failure count per PASS, however many instruments fail it);
//! * `positions`: ONE account-wide `instType=SWAP` read split per instrument client-side (the endpoint
//!   meters 10 requests per 2 seconds), a positive flat row for an instrument with no row.
//!
//! **Every size is scaled by the ROW'S OWN `ctVal`**: an admitted instrument's is read once from
//! `public/instruments` (`ReconcileClientOkx::ct_val_of`) and kept, because a contract of another
//! instrument is another amount of the base asset. Any read failing fails the WHOLE fetch (the restore
//! planner reads an order a successful report lacks as gone), with ONE exception: an instrument the
//! venue no longer lists (`51001`, or no SWAP row) holds nothing and is read as such with a warning,
//! because the admitted list never shrinks and one delisted instrument must not blind the account.
//! `admitted_symbols` is therefore a real claim, and `vike_gatewayhub_client::trade::RECONCILE_READS_ADMITTED`
//! lists okx. **A read made for an ADMITTED instrument** (its instruments row, `orders-pending`, its stop walk,
//! `trade/fills`) is judged by the shared rule (`ADMITTED_READ`): `51001` there too is the venue no longer listing it,
//! so an instrument delisted AFTER its contract size was cached holds nothing as well, rather than failing every pass
//! (owner ruling 2026-11-12), and the operator is told ONCE per instrument, at ERROR, with the account
//! (`ReconcileAdmittedSymbols::not_listed_logged`); a rate limit starts the pass-wide quiet period (owner ruling B-5):
//! that fetch fails as before, and every fetch of the next minute refuses before it asks. The mounted instrument's
//! reads and the account-wide position reads keep their old failure alone. Nothing is synthesized for a delisted
//! instrument's orders: one the engine still holds is left as it is, and the pass reports it as an orphan local
//! order.
//!
//! `ReconcileClientOkx` reuses the SAME `OkxV5Signer`/`RestTransportJson` seam as `rest::VenueRestOkx` (no
//! new HTTP/signing stack): `orders-pending`/`positions` ride the normal `signed` path (the same
//! one `rest::VenueRestOkx::call` already uses for order reads),
//! while `fills` — a history-shaped scan, same class as Binance's `myTrades`/`userTrades` — rides
//! `signed_requery`, the SAME short-timeout, rate-gate-EXEMPT path `rest::VenueRestOkx::
//! get_orders_history`/`get_fills_history` already use for the crate's audit-A3 resync, so a
//! reconcile pass can't blow the per-IP order-rate budget submit/cancel depend on.

pub(crate) mod snapshot;
// An operator's read of the account's open orders (the adapter's `venue_read`): signed GETs, so exec-plane.
pub(crate) mod venue_read;

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Instant;

use serde_json::{Value, json};

use crate::signer::OkxV5Signer;
use vike_bridge_core::json::{json_int, json_number, parse_rows};
use vike_bridge_core::order_status::OrderStatusTable;
use vike_bridge_core::ratelimit::RateGatePause;
use vike_bridge_core::reconcile_admitted::{
    ReconcileAdmittedReadSpec, ReconcileAdmittedSymbols, ReconcileReadError, not_backing_off,
};
use vike_bridge_core::reconcile_naming::{ALGO_NAMING_STRICT_FAILURES, StrictThenDegrade};
use vike_bridge_core::side::sign_default_sell;
use vike_bridge_core::signer::VenueClock;
use vike_bridge_core::transport::json::RestTransportJson;
use vike_execution::recon::{ReconcileClient, ReconcileFetchError};
use vike_model::events::{LiquiditySide, TradeId};
use vike_model::{
    FeeSchedule, FillReport, MarginMode, OrderStatus, OrderStatusReport, OrderType,
    PositionStatusReport,
};

use crate::execution::rest;
use crate::instrument::InstrumentType;
use crate::refusals::OkxAccountRefusal;
use crate::transport::client::unwrap_okx;
use vike_bridge_core::transport::{ErrorKind, VenueApiError};

const VENUE: &str = "okx";

/// How a read made for ONE admitted instrument is judged (`vike_bridge_core::reconcile_admitted`): `51001` ("instrument
/// ID does not exist", [`INSTRUMENT_NOT_LISTED`]) is the venue no longer listing it, so that read is no rows (owner
/// ruling 2026-11-12: an instrument delisted after its contract size was cached failed every pass for good); a rate
/// limit (`50011`, `50061` or HTTP `429`: the taxonomy's `RateLimited`) makes every fetch of the next
/// `RATE_LIMIT_BACKOFF` refuse before it asks (owner ruling B-5); anything else fails the whole fetch, as before. A
/// venue error's text is its `Display`, as every read of this client has always said it.
const ADMITTED_READ: ReconcileAdmittedReadSpec = ReconcileAdmittedReadSpec {
    not_listed: |e| e.code == INSTRUMENT_NOT_LISTED,
    rate_limited: |e| {
        e.kind_with(|e| crate::transport::error_codes::TAXONOMY.lookup(e)) == ErrorKind::RateLimited
    },
    text: |e| e.to_string(),
};

/// A failed read as this client's pass error: a venue error's `Display`, a parse failure as written.
fn read_text(fail: ReconcileReadError) -> String {
    fail.into_text(|e| e.to_string())
}

pub const PATH_ORDERS_PENDING: &str = "/api/v5/trade/orders-pending";
pub const PATH_FILLS: &str = "/api/v5/trade/fills";
pub const PATH_TRADE_FEE: &str = "/api/v5/account/trade-fee";

/// The venue's code for an instrument id it does not list (`public/instruments` of a delisted or
/// unknown id). The one answer for which an ADMITTED instrument is read as holding nothing.
const INSTRUMENT_NOT_LISTED: i64 = 51001;

/// Rows asked per `orders-algo-pending` request: the venue's maximum (`limit` is at most 100, default
/// 100). Asked explicitly, so that a short page can only mean "the last one".
const ALGO_PAGE_LIMIT: usize = 100;

/// Rows asked of an `orders-pending` read: the venue's maximum (`limit` is at most 100, default 100). The
/// account-wide operator read (`ReconcileClientOkx::account_open_orders`) is ONE page, a full one said as cut, not
/// walked; the reconcile read of one instrument walks every page ([`PLAIN_PAGES`]).
const PENDING_PAGE_LIMIT: usize = 100;

/// Pages the stop read follows before it FAILS: 40 x 100 = 4000 resting stops on ONE instrument. The
/// vendor's own cap on pending TP/SL orders is 100 per instrument (its trading-rules page), so a
/// second page is a safety net against that number being wrong, and this bound is for a venue that
/// never ends its chain.
const ALGO_MAX_PAGES: usize = 40;

/// Rows asked per `trade/fills` request: the venue's maximum (`limit` is at most 100, default 100).
const FILLS_PAGE_LIMIT: usize = 100;

/// Pages one instrument's fill read follows before it FAILS: 40 x 100 = 4000 fills in one reconcile window (a day
/// by default), far above what this account trades on one instrument.
const FILLS_MAX_PAGES: usize = 40;

/// Pages the per-instrument `orders-pending` read follows before it FAILS: 40 x 100 = 4000 plain orders on ONE
/// instrument, far above the vendor's per-instrument pending cap, so the bound is for a venue that never ends its
/// chain.
const PENDING_MAX_PAGES: usize = 40;

/// The facts of ONE paged `orders-*-pending` read ([`ReconcileClientOkx::pending_pages`]): its path, the id its rows
/// carry (the `after` cursor: the venue answers the records EARLIER than it), the row parser, the rows a page is
/// asked for and the pages the walk follows before it fails.
#[derive(Clone, Copy)]
struct PendingPages {
    path: &'static str,
    id: &'static str,
    parse: fn(&str, f64) -> Result<Vec<OrderStatusReport>, String>,
    page_limit: usize,
    max_pages: usize,
}

/// The resting stops, take-profits and trailing orders.
const ALGO_PAGES: PendingPages = PendingPages {
    path: rest::PATH_ORDERS_ALGO_PENDING,
    id: "algoId",
    parse: parse_algo_pending,
    page_limit: ALGO_PAGE_LIMIT,
    max_pages: ALGO_MAX_PAGES,
};

/// The plain resting orders of one instrument (owner ruling F-1: the read used to be ONE page with no `limit`,
/// so a 101st order on an instrument was missing from a "complete" report and the restore planner read it gone).
const PLAIN_PAGES: PendingPages = PendingPages {
    path: PATH_ORDERS_PENDING,
    id: "ordId",
    parse: parse_orders_pending,
    page_limit: PENDING_PAGE_LIMIT,
    max_pages: PENDING_MAX_PAGES,
};

// --- pure parsers --------------------------------------------------------------------------

/// Map a raw OKX order `state` onto the FSM [`OrderStatus`] `recon::diff::diff` reads — every state
/// the `orders-pending` / order endpoints document, explicitly: `live` → `Accepted`;
/// `partially_filled` → `PartiallyFilled`; `filled` → `Filled`; `canceled` and `mmp_canceled` (pulled
/// by market-maker protection) → `Canceled`. `mmp_canceled` used to be upper-cased to a word outside
/// the FSM and then read as a LIVE order by every consumer.
///
/// A word outside that list is WARNED and read as `Accepted`
/// (`vike_bridge_core::order_status::unknown_status_word`).
fn normalize_order_status(raw: &str) -> OrderStatus {
    ORDER_STATUS.status_of(VENUE, raw)
}

/// The words of [`normalize_order_status`].
const ORDER_STATUS: OrderStatusTable = OrderStatusTable {
    words: &[
        ("live", OrderStatus::Accepted),
        ("partially_filled", OrderStatus::PartiallyFilled),
        ("filled", OrderStatus::Filled),
        ("canceled", OrderStatus::Canceled),
        ("mmp_canceled", OrderStatus::Canceled),
    ],
};

/// The words of an `orders-algo-pending` row's `state` ([`parse_algo_pending`]).
const ALGO_STATUS: OrderStatusTable = OrderStatusTable {
    words: &[
        ("canceled", OrderStatus::Canceled),
        ("order_failed", OrderStatus::Rejected),
        ("live", OrderStatus::Accepted),
        ("pause", OrderStatus::Accepted),
    ],
};

/// Map a raw OKX `ordType` onto [`OrderType`]. OKX carries the time-in-force IN the order type, so
/// every resting-price kind is a `Limit` (`limit`, `post_only`, `fok`, `ioc` — the three the exec
/// builder sends for GTC/FOK/IOC — plus `mmp`, `mmp_and_post_only`, `op_fok` and `elp`); `market`
/// and `optimal_limit_ioc` (a futures/swap market order with a protective best-price limit) are
/// `Market`. Any other word has no twin and stays [`OrderType::Unrecognized`] lower-cased, as every
/// report spelled it before the type existed.
fn normalize_order_type(raw: &str) -> OrderType {
    match raw {
        "limit" | "post_only" | "fok" | "ioc" | "mmp" | "mmp_and_post_only" | "op_fok" | "elp" => {
            OrderType::Limit
        }
        "market" | "optimal_limit_ioc" => OrderType::Market,
        other => OrderType::Unrecognized(other.to_ascii_lowercase()),
    }
}

/// The client id a report row is filed under: `algoClOrdId` (the stop's, ours) on a fired stop's CHILD
/// order, else `clOrdId` — [`crate::execution::event_mapper::row_order_client_id`], the one reading of the pair.
fn order_client_id(v: &Value) -> Option<String> {
    Some(crate::execution::event_mapper::row_order_client_id(v)).filter(|c| !c.is_empty())
}

/// `GET /api/v5/trade/orders-pending` rows -> `OrderStatusReport`. `sz`/`accFillSz` are CONTRACTS
/// — rescaled to base via `ct_val` (see module doc). `avgPx` is already quote-per-base.
pub fn parse_orders_pending(body: &str, ct_val: f64) -> Result<Vec<OrderStatusReport>, String> {
    parse_rows(body, |o| {
        let symbol = o.get("instId").and_then(|s| s.as_str()).unwrap_or("").to_string();
        let venue_order_id = o.get("ordId").and_then(|s| s.as_str()).unwrap_or("").to_string();
        let side = sign_default_sell(o.get("side").and_then(|s| s.as_str()).unwrap_or(""));
        let order_type =
            normalize_order_type(o.get("ordType").and_then(|t| t.as_str()).unwrap_or(""));
        let sz = o.get("sz").and_then(json_number).unwrap_or(0.0);
        let acc_fill_sz = o.get("accFillSz").and_then(json_number).unwrap_or(0.0);
        let avg_px = o.get("avgPx").and_then(json_number).unwrap_or(0.0);
        let raw_status = o.get("state").and_then(|s| s.as_str()).unwrap_or("");
        let ts = o.get("uTime").and_then(json_int).unwrap_or(0);
        // a resting-price kind's limit is `px` (a market row's is "")
        let price = o
            .get("px")
            .and_then(json_number)
            .filter(|p| p.is_finite() && *p > 0.0 && order_type == OrderType::Limit);
        OrderStatusReport {
            venue: VENUE.to_string(),
            symbol,
            venue_order_id: venue_order_id.into(),
            order_client_id: order_client_id(o),
            side,
            order_type,
            qty: sz * ct_val,
            filled_qty: acc_fill_sz * ct_val,
            average_price: avg_px,
            status: normalize_order_status(raw_status),
            timestamp: ts,
            price,
            trigger_price: None,
            trail: None,
            trail_percent: None,
        }
    })
}

/// The kind of ONE `conditional` algo row (an `orders-algo-pending`, `orders-algo-history` or
/// `order-algo` row) and its limit price, read back the way `execution/rest/stop_algo.rs`'s
/// `build_stop_algo_params` writes them. A row whose only priced trigger is the TAKE-PROFIT side
/// (`tpTriggerPx`) is a `TakeProfit`, its limit `tpOrdPx` when that is a price (`-1` fires a market
/// order: no limit). Any other row is the stop-loss side's: a priced `slOrdPx` is a `StopLimit` with that
/// limit, `-1` (market) and anything that is not a price a `Stop`, as every row read before the take-profit.
/// A row with both triggers is the stop-loss's by the venue's own rule ("When placing net TP/SL order
/// (ordType=conditional) and both take-profit and stop-loss parameters are sent, only stop-loss logic will
/// be performed", <https://www.okx.com/docs-v5/en/#order-book-trading-algo-trading-post-place-algo-order>);
/// the field words are the "Algo order list" table's
/// (<https://www.okx.com/docs-v5/en/#order-book-trading-algo-trading-get-algo-order-list>). A
/// `move_order_stop` row (the same tables' "Trailing order") is a `Trailing` with no limit, whatever
/// else it carries, its distance a price (`callbackSpread`) or a percent (`callbackRatio`) alike: it
/// fires a market order.
pub fn conditional_kind(row: &Value) -> (OrderType, Option<f64>) {
    if row.get("ordType").and_then(Value::as_str) == Some("move_order_stop") {
        return (OrderType::Trailing, None);
    }
    let px = |k: &str| {
        row.get(k).and_then(Value::as_str).and_then(|t| t.parse::<f64>().ok()).filter(|p| *p > 0.0)
    };
    if px("slTriggerPx").is_none() && px("tpTriggerPx").is_some() {
        return (OrderType::TakeProfit, px("tpOrdPx"));
    }
    match px("slOrdPx") {
        Some(limit) => (OrderType::StopLimit, Some(limit)),
        None => (OrderType::Stop, None),
    }
}

/// `GET /api/v5/trade/orders-algo-pending` rows (untriggered `conditional` STOPS and TAKE-PROFITS, and
/// `move_order_stop` TRAILING orders) -> `OrderStatusReport`. The venue id is the `algoId`, the client id
/// the `algoClOrdId`; `sz` is CONTRACTS (rescaled by `ct_val`); a resting trigger has no fill or average
/// price yet. The kind is [`conditional_kind`]'s: `Stop`, `StopLimit` (a priced `slOrdPx`), `TakeProfit`
/// or `Trailing`. The list is untriggered
/// orders by definition, so a state outside `live`/`pause`/`canceled`/`order_failed` is WARNED and
/// read as live (`Accepted`) — the conservative direction, as [`normalize_order_status`].
///
/// `order_failed` (the trigger fired and the order it placed was refused: nothing rests, nothing
/// filled; the word `order-algo` showed on the demo account on 2026-10-24) is a TERMINAL `Rejected`,
/// never a live stop. The vendor lists only `live`/`pause` on this endpoint, so the arm is reached
/// only if the venue ever breaks that; the failed stop a cancel meets is `confirm_algo_gone`'s.
pub fn parse_algo_pending(body: &str, ct_val: f64) -> Result<Vec<OrderStatusReport>, String> {
    parse_rows(body, |o| {
        let text = |k: &str| o.get(k).and_then(|s| s.as_str()).unwrap_or("");
        let status = ALGO_STATUS.status_of(VENUE, text("state"));
        let (order_type, price) = conditional_kind(o);
        // The terms `execution/rest/stop_algo.rs` writes, read back ("" and "-1" are none): the side's trigger
        // (`tpTriggerPx` for a take-profit, `slTriggerPx` otherwise); a trailing order's activation
        // `activePx` and its distance, `callbackSpread` (a price) or `callbackRatio` (a fraction:
        // shifted two places in its decimal text, so "0.07" is exactly 7 percent).
        let px = |k: &str| o.get(k).and_then(json_number).filter(|p| p.is_finite() && *p > 0.0);
        let (trigger_price, trail, trail_percent) = match order_type {
            OrderType::Trailing => (
                px("activePx"),
                px("callbackSpread"),
                Some(text("callbackRatio"))
                    .filter(|r| !r.is_empty())
                    .and_then(|r| format!("{r}e2").parse::<f64>().ok())
                    .filter(|p| p.is_finite() && *p > 0.0),
            ),
            OrderType::TakeProfit => (px("tpTriggerPx"), None, None),
            _ => (px("slTriggerPx"), None, None),
        };
        OrderStatusReport {
            venue: VENUE.to_string(),
            symbol: text("instId").to_string(),
            venue_order_id: text("algoId").to_string().into(),
            order_client_id: Some(text("algoClOrdId")).filter(|c| !c.is_empty()).map(String::from),
            side: sign_default_sell(text("side")),
            order_type,
            qty: o.get("sz").and_then(json_number).unwrap_or(0.0) * ct_val,
            filled_qty: 0.0,
            average_price: 0.0,
            status,
            timestamp: o.get("uTime").and_then(json_int).unwrap_or(0),
            price,
            trigger_price,
            trail,
            trail_percent,
        }
    })
}

/// The `trade/fills` `subType`s that are an ordinary trade of the account's own order (OKX V5, "Get transaction details":
/// 1 buy, 2 sell, 3 open long, 4 open short, 5 close long, 6 close short).
const TRADE_SUB_TYPES: [&str; 6] = ["1", "2", "3", "4", "5", "6"];

/// **Whether a `trade/fills` or `fills-history` row is a fill of the account's own order** ([`TRADE_SUB_TYPES`]): the
/// ONE rule of [`parse_fills`] (reconcile) and `crate::execution::resync`'s `map_okx_history` (the resync replay), so the two cannot
/// drift. Both lists also carry the account's liquidation (`subType` 100–107) and ADL (125–128) executions, which the
/// order stream books as `PositionLiquidated` only (`crate::execution::event_mapper`'s `LIQUIDATION_CATEGORIES`), and deliveries,
/// transfers, conversions and block, spread or move-position trades, none of them an order of this bridge: those are
/// dropped silently, as routine. A row with NO `subType` is dropped too (OKX documents it on every row; bybit drops a row
/// with no `execType` alike), said once per process: a fill reported or replayed with no type could be one of those.
pub(crate) fn is_ordinary_trade(row: &Value) -> bool {
    match row.get("subType").and_then(Value::as_str).filter(|s| !s.is_empty()) {
        Some(sub) => TRADE_SUB_TYPES.contains(&sub),
        None => {
            static SAID: std::sync::Once = std::sync::Once::new();
            SAID.call_once(|| {
                tracing::warn!(
                    venue = VENUE,
                    "a fills row carries no `subType`, so it is neither reported to reconcile nor replayed: without \
                     it a liquidation or ADL execution (booked as `PositionLiquidated`) cannot be told from a fill \
                     (said once per process)"
                );
            });
            false
        }
    }
}

/// `GET /api/v5/trade/fills` rows -> `FillReport`. `fillSz` is CONTRACTS — rescaled to base via
/// `ct_val`. Commission: OKX `fee` is NEGATIVE for a charge, so `commission = -fee` (positive
/// cost / negative rebate) — same sign convention `execution::event_mapper::map_okx_order` already applies
/// to the live WS fill stream.
///
/// A row with no `tradeId` is DROPPED, not reported. A `FillReport` with no `trade_id` is worse than
/// a missing one: `vike_execution::recon::diff` decides "already known" by looking the id up in
/// `seen_trade_ids`, so an id-less row can never match and instead FABRICATES a `MissingFill`
/// divergence — one of the two kinds the `hybrid` policy AUTO-APPLIES
/// (`vike_tradehub::reconcile_config::auto_applied_kinds`), i.e. it would book the fill a second time with
/// no operator in front of it. Dropping is the safe direction: the worst case is a real fill going
/// unreconciled until a pass sees it with its id. OKX documents `tradeId` on every `fills` row.
///
/// ⚠ **Only an ordinary trade is a fill** ([`is_ordinary_trade`]). `trade/fills` also lists the account's liquidation
/// (`subType` 100–107) and ADL (125–128) executions, which the order stream books as `PositionLiquidated` only
/// (`crate::execution::event_mapper`'s `LIQUIDATION_CATEGORIES`), so their `tradeId` sits in the engine's `seen_liq_ids` and never in
/// `seen_trade_ids`: reported here, each was an order-client-id-less `MissingFill` that `hybrid` auto-applies, closing the position a
/// second time. A delivery (112/113), a transfer, a conversion and a block, spread or move-position trade are no order of
/// this bridge either, and a row with no `subType` is dropped as well.
pub fn parse_fills(body: &str, ct_val: f64) -> Result<Vec<FillReport>, String> {
    // `flatten` is the row DROP — see this fn's doc for why reporting it would be worse.
    let rows = parse_rows(body, |f| {
        if !is_ordinary_trade(f) {
            return None;
        }
        let symbol = f.get("instId").and_then(|s| s.as_str()).unwrap_or("").to_string();
        let trade_id = match TradeId::new(f.get("tradeId").and_then(|s| s.as_str()).unwrap_or("")) {
            Ok(id) => id,
            Err(_) => {
                tracing::warn!(
                    venue = VENUE,
                    symbol = %symbol,
                    "trade/fills row carries no `tradeId` — skipping the FillReport; an id-less \
                     row can never match `seen_trade_ids`, so it would fabricate a MissingFill \
                     divergence that `hybrid` auto-applies (booking the fill twice)"
                );
                return None;
            }
        };
        let venue_order_id = f.get("ordId").and_then(|s| s.as_str()).unwrap_or("").to_string();
        let side = sign_default_sell(f.get("side").and_then(|s| s.as_str()).unwrap_or(""));
        let last_qty = f.get("fillSz").and_then(json_number).unwrap_or(0.0) * ct_val;
        let last_price_value = f.get("fillPx").and_then(json_number).unwrap_or(0.0);
        let commission = -vike_bridge_core::json::get_fee_f64(f, "fee", VENUE);
        let commission_asset = f.get("feeCcy").and_then(|c| c.as_str()).unwrap_or("").to_string();
        // a spot BUY paid in its base coin, restated net exactly as the stream and the replay restate it
        let (last_qty, commission, commission_asset) = crate::instrument::spot_buy_net(
            &symbol,
            side,
            last_qty,
            last_price_value,
            commission,
            &commission_asset,
        )
        .unwrap_or((last_qty, commission, commission_asset));
        let exec_type = f.get("execType").and_then(|e| e.as_str()).unwrap_or("");
        let ts = f.get("ts").and_then(json_int).unwrap_or(0);
        Some(FillReport {
            venue: VENUE.to_string(),
            symbol,
            trade_id,
            venue_order_id: venue_order_id.into(),
            order_client_id: order_client_id(f),
            side,
            last_qty,
            last_price: last_price_value,
            commission,
            commission_asset,
            liquidity_side: LiquiditySide::from_maker(exec_type == "M"),
            timestamp: ts,
        })
        .filter(vike_bridge_core::event_map::fill_report_is_bookable)
    })?;
    Ok(rows.into_iter().flatten().collect())
}

/// `GET /api/v5/trade/orders-algo-history` rows (`state=effective`: the stops that FIRED) -> the child
/// order each one spawned: the child's `ordId` (the row's `ordIdList`, plus its `ordId`, "latest order
/// id") -> the stop's `algoClOrdId`. A `trade/fills` row names its order by `ordId` and carries no
/// algo field, so this is the join that files a fired stop's fill under the stop's own client id
/// ([`name_algo_children`]). A row with no `algoClOrdId` (a stop placed by hand) names nothing.
pub fn parse_algo_children(body: &str) -> Result<HashMap<String, String>, String> {
    let rows: Vec<Vec<(String, String)>> = parse_rows(body, |o| {
        let order_client_id = o.get("algoClOrdId").and_then(Value::as_str).unwrap_or("");
        let ids = o
            .get("ordIdList")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .chain(o.get("ordId").and_then(Value::as_str))
            .filter(|id| !id.is_empty());
        ids.filter(|_| !order_client_id.is_empty())
            .map(|id| (id.to_string(), order_client_id.to_string()))
            .collect()
    })?;
    Ok(rows.into_iter().flatten().collect())
}

/// File each fill whose order is a fired stop's child under the STOP's client id. `trade/fills` carries
/// the child's own `clOrdId` (`O4038740198576748544`, the venue's), which no order of ours has, so
/// unnamed the fill is a `MissingFill` for an order the book never heard of; named, it joins the
/// stop. Fills of any other order are left as they were.
pub fn name_algo_children(fills: &mut [FillReport], children: &HashMap<String, String>) {
    for fill in fills {
        if let Some(order_client_id) = children.get(fill.venue_order_id.as_str()) {
            fill.order_client_id = Some(order_client_id.clone());
        }
    }
}

/// `GET /api/v5/account/positions` rows -> `PositionStatusReport`. `pos` is ALREADY SIGNED
/// CONTRACTS (long > 0, short < 0 in hedge mode; signed net in one-way mode) — rescaled to base
/// via `ct_val`, never re-signed by `posSide`. An empty `data` array synthesizes ONE flat row for
/// `symbol` (see module doc — OKX omits flat positions rather than sending a `pos: "0"` row); any
/// row OKX DOES send (including a defensive `pos: "0"`) is kept, not filtered.
pub fn parse_positions(
    body: &str,
    symbol: &str,
    ct_val: f64,
) -> Result<Vec<PositionStatusReport>, String> {
    let rows = parse_rows(body, |p| {
        let row_symbol = p.get("instId").and_then(|s| s.as_str()).unwrap_or("").to_string();
        let side = crate::execution::event_mapper::pside(p);
        let contracts = p.get("pos").and_then(json_number).unwrap_or(0.0); // already signed
        let avg_px = p.get("avgPx").and_then(json_number).unwrap_or(0.0);
        let ts = p.get("uTime").and_then(json_int).unwrap_or(0);
        let (margin_mode, isolated_margin) = parse_mgn_mode(p);
        PositionStatusReport {
            venue: VENUE.to_string(),
            symbol: row_symbol,
            position_side: side,
            qty: contracts * ct_val,
            average_price: avg_px,
            timestamp: ts,
            margin_mode,
            isolated_margin,
            delta: None,
        }
    })?;
    if rows.is_empty() {
        return Ok(vec![PositionStatusReport::flat(VENUE, symbol)]);
    }
    Ok(rows)
}

/// OKX position-row margin-mode read (margin-mode step-2, read-side only), read by
/// [`parse_positions`]:
/// `mgnMode` `"isolated"` ⇒ `Isolated` + the position's `margin` balance (OKX documents `margin`
/// as "margin balance, applicable to isolated" — an empty string on cross rows, which `json_number`
/// already yields `None` for); anything else — `"cross"` or an absent field — is the fail-safe
/// `Cross`/`None`, byte-identical to pre-field behavior. The REQUEST side now honors
/// `OrderRequest.margin_mode` via `rest::swap_td_mode` (unset = the historical `"cross"`).
pub fn parse_mgn_mode(p: &Value) -> (MarginMode, Option<f64>) {
    let isolated = p
        .get("mgnMode")
        .and_then(|m| m.as_str())
        .is_some_and(|m| m.eq_ignore_ascii_case("isolated"));
    if isolated {
        (MarginMode::Isolated, p.get("margin").and_then(json_number))
    } else {
        (MarginMode::Cross, None)
    }
}

/// `GET /api/v5/account/balance` `data[].details[]` rows -> the account's USDT `cashBal` (total
/// cash — free + frozen; NOT `availBal`, which excludes funds currently locked by open orders/
/// positions) — the pure, fixture-tested parser this `ReconcileClient::fetch_balance` override
/// delegates to. `None` when no USDT detail row is present;
/// malformed JSON (not an array) is a hard `Err`, never a panic.
pub fn parse_balance(body: &str) -> Result<Option<f64>, String> {
    parse_balance_of(body, "USDT")
}

/// [`parse_balance`] for the `cashBal` of ANY currency's detail row: USDT for a linear mount, the settle
/// coin's for a coin-margined one (decision 0258: that engine's books are kept in the coin, and `cashBal`
/// is the coin's cash, its realized PnL, fees and funding already in it; `upl` is not). `None` when the
/// answer lists no row for `currency`.
pub fn parse_balance_of(body: &str, currency: &str) -> Result<Option<f64>, String> {
    let v: Value = serde_json::from_str(body).map_err(|e| e.to_string())?;
    let rows = v.as_array().ok_or_else(|| "expected a JSON array".to_string())?;
    for account in rows {
        if let Some(details) = account.get("details").and_then(|d| d.as_array())
            && let Some(row) =
                details.iter().find(|d| d.get("ccy").and_then(|c| c.as_str()) == Some(currency))
        {
            return Ok(row.get("cashBal").and_then(json_number));
        }
    }
    Ok(None)
}

/// `GET /api/v5/account/trade-fee` `data[0]` -> live [`FeeSchedule`]. OKX reports fee rates
/// **negative for a charge** (their sign convention: negative = fee-you-pay, positive = rebate),
/// so the parser NEGATES — the same `commission = -fee` normalization [`parse_fills`] already
/// applies to the live fill stream — leaving `commission > 0` a cost, `< 0` a rebate. For a
/// USDT-margined SWAP the effective rate is `makerU`/`takerU` (falls back to `maker`/`taker` when
/// those are absent/empty). `Ok(None)` when no row/rate is present (fail-soft → static default).
pub fn parse_fee_rate(body: &str) -> Result<Option<FeeSchedule>, String> {
    let v: Value = serde_json::from_str(body).map_err(|e| e.to_string())?;
    let rows = v.as_array().ok_or_else(|| "expected a JSON array".to_string())?;
    Ok(rows.first().and_then(|r| {
        let maker_raw = r
            .get("makerU")
            .and_then(json_number)
            .or_else(|| r.get("maker").and_then(json_number))?;
        let taker_raw = r
            .get("takerU")
            .and_then(json_number)
            .or_else(|| r.get("taker").and_then(json_number))?;
        Some(FeeSchedule::from_fractions(-maker_raw, -taker_raw))
    }))
}

/// [`parse_fee_rate`] for a SPOT pair in fee group `group` (the `groupId` of its `public/instruments`
/// row): that group's `feeGroup` row, negated the same way. The answer's top-level `maker`/`taker` are
/// group 1's, while OKX prices each pair at its own group's rate (the demo's SPOT answer lists 25
/// groups, taker 0 to 40 bps). `Ok(None)` when the answer lists no row for `group`.
pub fn parse_spot_fee_rate(body: &str, group: &str) -> Result<Option<FeeSchedule>, String> {
    let v: Value = serde_json::from_str(body).map_err(|e| e.to_string())?;
    let rows = v.as_array().ok_or_else(|| "expected a JSON array".to_string())?;
    Ok(rows.first().and_then(|r| {
        let groups = r.get("feeGroup")?.as_array()?;
        let row =
            groups.iter().find(|g| g.get("groupId").and_then(Value::as_str) == Some(group))?;
        let maker = row.get("maker").and_then(json_number)?;
        let taker = row.get("taker").and_then(json_number)?;
        Some(FeeSchedule::from_fractions(-maker, -taker))
    }))
}

/// [`parse_fee_rate`] for a COIN-MARGINED contract: `maker`/`taker`, the fields OKX states for the
/// crypto-margined FUTURES and SWAP contracts (`makerU`/`takerU` are the USDT-margined ones), negated the
/// same way. `Ok(None)` when either is absent.
pub fn parse_coin_fee_rate(body: &str) -> Result<Option<FeeSchedule>, String> {
    let v: Value = serde_json::from_str(body).map_err(|e| e.to_string())?;
    let rows = v.as_array().ok_or_else(|| "expected a JSON array".to_string())?;
    Ok(rows.first().and_then(|r| {
        let maker = r.get("maker").and_then(json_number)?;
        let taker = r.get("taker").and_then(json_number)?;
        Some(FeeSchedule::from_fractions(-maker, -taker))
    }))
}

/// **`reports` as they leave the bridge** (decision 0290): each `symbol` respelled from the instId to its USER name
/// (`crate::instrument::user_name_of`). The reads between speak instIds.
pub(crate) fn user_named<R>(mut reports: Vec<R>, symbol: fn(&mut R) -> &mut String) -> Vec<R> {
    for report in &mut reports {
        let name = symbol(report);
        *name = crate::instrument::user_name_of(name);
    }
    reports
}

// --- the factory -------------------------------------------------------------------------------

/// The venue -> `ReconcileClient` factory (ReconFactory seam, wave-2 task 6) — moved verbatim from
/// `vike_tradehub_core::build_recon_client`'s `"okx"` match arm (called from `crate::adapter` since
/// docs/decisions/0096): a FRESH stateless-HMAC `ureq` REST client dedicated to reconcile reads,
/// reusing the SAME `Credentials` the venue's `ExecutionClient`/`spawn_with_recorder` already
/// builds from. `ct_val` is the caller's already-fetched contracts->base scale factor, threaded in rather
/// than re-fetched here — see `crate::adapter`'s `VenueAdapterOkx::start`, which fetches it once and reuses
/// it for both the exec grid and this recon client. `None` (the start's read failed, or a probe that reads
/// only the balance): the client reads it itself before it scales a size, never a guessed one. Uses `net`'s
/// REST base (demo and mainnet share the host), with the `x-simulated-trading` header selected by
/// `net.mainnet` — the SAME addresses and type (decision 0095: `live` means mainnet) the start
/// resolved ONCE ([`crate::execution::rest::OkxNet::resolve`]) and threaded into the exec path, so reconcile
/// stays in lockstep with exec instead of re-reading anything for itself. Always `Some` —
/// construction here is pure/infallible (no network); `Option` is kept so the return type matches
/// every other bridge's `recon_client` (deribit's genuinely can fail).
pub fn recon_client(
    creds: &vike_secrets::credentials::Credentials,
    symbol: &str,
    ct_val: impl Into<Option<f64>>,
    net: &crate::execution::rest::OkxNet,
) -> Option<Box<dyn ReconcileClient>> {
    reconcile_client_sharing(creds, symbol, ct_val, net, Arc::default(), &VenueClock::default(), "")
}

/// [`recon_client`] over an admitted-instrument list the CALLER also holds: the adapter hands the same
/// `Arc` to the funding poll (`crate::execution::funding::OkxFundingPoller::with_admitted`), so the instrument the
/// engine tells this client about (`ReconcileClient::admit_symbol`) is the one whose funding bills fold.
/// `clock` is the start's signing clock (owner ruling B-4, `crate::transport::client::rest_transport_on`), so the
/// account's reconcile reads sign on the time its orders do. `account` is the account's label, for the lines an
/// operator acts on.
pub fn reconcile_client_sharing(
    creds: &vike_secrets::credentials::Credentials,
    symbol: &str,
    ct_val: impl Into<Option<f64>>,
    net: &crate::execution::rest::OkxNet,
    admitted: Arc<ReconcileAdmittedSymbols>,
    clock: &VenueClock,
    account: &str,
) -> Option<Box<dyn ReconcileClient>> {
    Some(Box::new(
        ReconcileClientOkx::new(
            OkxV5Signer::new(creds, clock.ms()),
            // Reconcile reads follow the SAME resolved addresses and type the exec path binds to: the
            // shared REST host stays, but the `x-simulated-trading` header is dropped on mainnet. Demo
            // keeps the header, byte-identical to before the switch existed.
            crate::transport::client::rest_transport_on(
                crate::execution::rest::simulated(net.mainnet),
                clock,
            )
            .with_rate_gate(crate::transport::ratelimit::rest_rate_gate()),
            net.rest.as_str(),
            symbol.to_string(),
            ct_val,
        )
        .with_admitted(admitted)
        .with_account(account),
    ))
}

// --- the client --------------------------------------------------------------------------------

/// One reconcile client per venue-symbol — the SAME per-symbol shape `rest::VenueRestOkx` uses,
/// but carrying only what a report fetch needs (signer + transport + base_url + symbol + ct_val;
/// no order-formatting `SymbolProperties`/leverage) — and, since the engine admits instruments on
/// demand (decision 0173), every instrument it was told about (`ReconcileClient::admit_symbol`), each read
/// with ITS OWN `ctVal` (see the module doc's "Admitted instruments").
pub struct ReconcileClientOkx<T: RestTransportJson> {
    signer: OkxV5Signer,
    transport: T,
    base_url: String,
    /// instId form: BTC-USDT-SWAP
    symbol: String,
    /// base units per contract — the contracts→base rescale factor (see module doc). `None`: not read
    /// yet, so [`Self::mounted_ct_val`] reads it before any size is scaled (no guessed value, owner
    /// 2026-11-13).
    ct_val: Option<f64>,
    /// The instruments the engine admitted beyond `symbol`, as OKX spells them (upper-case `instId`).
    /// Shared with the funding poll when the adapter built both ([`reconcile_client_sharing`]).
    admitted: Arc<ReconcileAdmittedSymbols>,
    /// Whether the previous pass's mode watch found the account in long/short mode (`None`: no read
    /// yet, or the network is not the simulated one and nothing is read). See [`Self::watch_mode`].
    long_short_seen: Mutex<Option<bool>>,
    /// `ctVal` of each admitted instrument already asked for (a row's contract size does not change
    /// for the life of the client). Only a usable linear row is kept, never a failure.
    contract_vals: Mutex<HashMap<String, f64>>,
    /// The size step (`lotSz`) of each admitted SPOT pair, kept with its `ctVal` from the same row: a holding
    /// under one step is flat (`PositionStatusReport::spot_holding`).
    spot_steps: Mutex<HashMap<String, f64>>,
    /// **The account id, remembered after the first ask.**
    ///
    /// ⚠ Unlike binance, nothing here can fill it as a side effect: no other call on this client
    /// reads `/api/v5/account/config`, so the first `fetch_account_identity` IS the request. The
    /// cache therefore saves the REPEATS — a reconcile pass asking again — rather than the first
    /// call. `vike_execution::recon::ReconcileClient::fetch_account_identity` carries why this is the one
    /// value on the trait that may be cached at all.
    identity: std::sync::OnceLock<Option<String>>,
    /// How the stop-history reads have been going across passes (the client lives for the process): decision
    /// 0192's strict-then-degraded rule, `vike_bridge_core::reconcile_naming`'s ([`StrictThenDegrade`]).
    naming: Mutex<StrictThenDegrade>,
    /// The pass-wide quiet period after a read made for an admitted instrument was answered a rate limit
    /// ([`ADMITTED_READ`]).
    pause: RateGatePause,
    /// The account this client reads, for the lines an operator acts on ([`Self::with_account`]; empty when the
    /// builder named none, a one-shot read that admits nothing).
    account: String,
    /// Whether a pass makes the two TRAILING reads ([`Self::resting_trailing`], [`Self::trailing_children`]):
    /// [`rest::trailing_listed`] at construction, so a box whose okx row does not list `trailing` asks
    /// exactly what it asked before the kind existed. [`Self::with_trailing`] sets it by hand.
    trailing: bool,
    /// The settle coin of a COIN-MARGINED mounted instrument (`BTC-USD-SWAP` settles in `BTC`), `None` for
    /// every other: that engine keeps its books in the coin (decision 0258), so the balance this client
    /// reports is the coin's `cashBal`, the fee rate is the crypto-margined one, and an admitted instrument
    /// must be a coin-margined contract of the same coin, counted in contracts (its factor is 1.0, as the
    /// mounted one's is: the start builds this client with 1.0). Read off the instId's KEPT row's user name
    /// (`vike_catalog::inverse_settle_coin`, held to okx's rows by `catalog.rs`'s
    /// `the_name_rule_agrees_with_every_row`); an instId no kept row names is no coin-margined mount.
    coin: Option<String>,
}

impl<T: RestTransportJson> ReconcileClientOkx<T> {
    pub fn new(
        signer: OkxV5Signer,
        transport: T,
        base_url: impl Into<String>,
        symbol: impl Into<String>,
        ct_val: impl Into<Option<f64>>,
    ) -> Self {
        let symbol = symbol.into();
        // the catalog's rule reads the USER name of the instId's kept row (decision 0290)
        let coin = vike_catalog::kept_user_name_of_raw(VENUE, &symbol)
            .and_then(|user| vike_catalog::inverse_settle_coin(VENUE, &user));
        ReconcileClientOkx {
            signer,
            transport,
            base_url: base_url.into(),
            symbol,
            coin,
            ct_val: ct_val.into(),
            admitted: Arc::default(),
            long_short_seen: Mutex::new(None),
            contract_vals: Mutex::new(HashMap::new()),
            spot_steps: Mutex::new(HashMap::new()),
            identity: std::sync::OnceLock::new(),
            naming: Mutex::new(StrictThenDegrade::default()),
            pause: RateGatePause::default(),
            account: String::new(),
            trailing: rest::trailing_listed(),
        }
    }

    /// Make (`true`) or skip (`false`) the trailing reads whatever okx's row lists: the offline suites pin
    /// both sides of the gate with it.
    #[must_use]
    pub fn with_trailing(mut self, trailing: bool) -> Self {
        self.trailing = trailing;
        self
    }

    /// Keep the admitted-instrument list in `admitted` instead of a private one, so the holder of the
    /// other end (the funding poll) sees every instrument this client is told.
    #[must_use]
    pub fn with_admitted(mut self, admitted: Arc<ReconcileAdmittedSymbols>) -> Self {
        self.admitted = admitted;
        self
    }

    /// Name the account this client reads (its label), for the lines an operator acts on.
    #[must_use]
    pub fn with_account(mut self, account: &str) -> Self {
        self.account = account.to_string();
        self
    }

    /// **The mode watch** (known-limitations "does not watch the mode afterwards"): once per pass, read the
    /// account's position mode and SAY when it is long/short while the engine runs, which the start probe
    /// (`crate::account::modes::settle_modes`) cannot see. Only a CHANGE speaks (`watch_verdict`), so a
    /// pass costs one signed read and a log line only on a flip. It writes nothing and never fails the
    /// pass: an unreadable mode is a debug line. Same gate as the start probe: the SIMULATED network only
    /// (`RestTransportJson::network`); a live account is never read for a mode and costs no request.
    fn watch_mode(&self) {
        let word = match crate::account::modes::read_position_mode(
            &self.transport,
            &self.signer,
            &self.base_url,
        ) {
            Ok(Some(word)) => word,
            Ok(None) => return,
            Err(why) => {
                tracing::debug!(venue = VENUE, %why, "reconcile: the account's position mode could not be read");
                return;
            }
        };
        let mut seen = self.long_short_seen.lock().unwrap_or_else(PoisonError::into_inner);
        let (verdict, now) = crate::account::modes::watch_verdict(*seen, &word);
        *seen = now;
        match verdict {
            crate::account::modes::WatchVerdict::Fine => {}
            crate::account::modes::WatchVerdict::LongShort => tracing::error!(
                venue = VENUE, symbol = %self.symbol,
                "OKX will refuse this account's orders: the account is in long/short position mode and this \
                 adapter orders net positions (the engine's start switches a FLAT demo account to net mode, \
                 so the mode changed after the start or the start could not switch it). Close its positions, \
                 cancel its orders, then switch the account's engine off and on (account.active) and the \
                 start switches the mode"
            ),
            crate::account::modes::WatchVerdict::BackToNet => tracing::warn!(
                venue = VENUE, symbol = %self.symbol,
                "the account is back in net_mode: its orders are accepted again"
            ),
        }
    }

    /// `ctVal` of the admitted instrument `instrument` (upper-case): the cached one, else ONE public
    /// `instruments` read. `Ok(None)` = the venue does not list it as a SWAP any more (code `51001`
    /// or no row): it holds nothing, so the pass reads it as such with a warning and the account's
    /// other reads still go through (the admitted list never shrinks, so a delisted instrument must not
    /// blind the pass for good). `Err` = the read failed, or the row is one this adapter does not size
    /// (inverse, `ctMult` other than 1, no usable `ctVal`; the door never admits one, so it is a venue
    /// change): the whole fetch fails, because the restore planner reads an order a report lacks as gone.
    fn ct_val_of(&self, instrument: &str) -> Result<Option<f64>, String> {
        if let Some(known) =
            self.contract_vals.lock().unwrap_or_else(PoisonError::into_inner).get(instrument)
        {
            return Ok(Some(*known));
        }
        // the instrument's own `instType` (a spot pair and a dated future are admitted besides the swaps); an id of
        // no known form is refused in words, never read as a SWAP
        let class = InstrumentType::of_inst_id_or_refusal(instrument)?;
        let params =
            [("instType", class.inst_type().to_string()), ("instId", instrument.to_string())];
        let data = match self
            .transport
            .public(&self.base_url, rest::PATH_INSTRUMENTS, &params)
            .and_then(unwrap_okx)
        {
            Ok(data) => data,
            Err(e) if e.code == INSTRUMENT_NOT_LISTED => return Ok(None),
            Err(e) => {
                // An admitted instrument's read (owner ruling B-5: a rate limit starts the pass-wide pause).
                let why = self.judged::<()>(instrument, Err(e.into())).err().unwrap_or_default();
                return Err(format!(
                    "public/instruments for {instrument} could not be read: {why}"
                ));
            }
        };
        let Some(row) =
            rest::parse_okx_perp_instruments(&json!({ "data": data })).get(instrument).cloned()
        else {
            return Ok(None);
        };
        // a coin-margined mount reads contracts of its own coin at 1.0, and nothing else (the exec fence)
        if let Some(coin) = &self.coin {
            crate::execution::coin_fence(instrument, &row, coin)?;
            self.contract_vals
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .insert(instrument.to_string(), 1.0);
            return Ok(Some(1.0));
        }
        if let Some(why) = crate::execution::inverse_swap_refusal(instrument, Some(&row)) {
            return Err(why);
        }
        let usable = row.ct_val.is_finite() && row.ct_val > 0.0;
        if !usable || row.multiplied() {
            return Err(format!(
                "{instrument}: no usable contract size (ctVal {}, ctMult {}); its reports cannot be scaled",
                row.ct_val, row.ct_mult
            ));
        }
        if class == InstrumentType::Spot {
            self.spot_steps
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .insert(instrument.to_string(), row.properties.step_size);
        }
        self.contract_vals
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(instrument.to_string(), row.ct_val);
        Ok(Some(row.ct_val))
    }

    /// The MOUNTED instrument's `ctVal`: the value the client was built with, or, when the start could not
    /// read it, ONE `public/instruments` read ([`Self::ct_val_of`], kept once it answers). A read that fails
    /// fails the pass, and an instrument the venue does not list has no size to scale by: never a guess.
    fn mounted_ct_val(&self) -> Result<f64, String> {
        if let Some(known) = self.ct_val {
            return Ok(known);
        }
        self.ct_val_of(&self.symbol)?.ok_or_else(|| {
            format!(
                "{}: public/instruments has no row for it, so its contract size is unknown and its \
                 reports cannot be scaled",
                self.symbol
            )
        })
    }

    /// Every instrument this pass reads with the `ctVal` its contract counts are scaled by: the mounted
    /// one FIRST ([`Self::mounted_ct_val`]: the value the client was built with when it has one), then each
    /// admitted one. With nothing admitted and the mounted value known, no request is made.
    fn read_set(&self) -> Result<Vec<(String, f64)>, String> {
        let mut set = vec![(self.symbol.clone(), self.mounted_ct_val()?)];
        for instrument in self.admitted.symbols() {
            // An id of no known form names no class to read: it holds nothing to this pass, said once in words, and
            // never blinds the pass nor is read as a SWAP (OKX-1).
            if let Err(why) = InstrumentType::of_inst_id_or_refusal(&instrument) {
                self.say_not_listed(&instrument, &why.to_string());
                continue;
            }
            match self.ct_val_of(&instrument)? {
                Some(ct_val) => set.push((instrument, ct_val)),
                None => {
                    self.say_not_listed(&instrument, "public/instruments has no row for it");
                    tracing::warn!(
                        venue = VENUE, symbol = %instrument,
                        "reconcile: okx does not list this admitted instrument under its own instType; \
                         reading it as holding nothing"
                    );
                }
            }
        }
        Ok(set)
    }

    /// The normal signed GET path — the same one `rest::VenueRestOkx::call` already uses for order
    /// reads. Returns the unwrapped `data` array, re-serialized.
    fn signed(&self, path: &str, params: &[(&str, Value)]) -> Result<String, String> {
        self.signed_venue(path, params).map_err(|e| e.to_string())
    }

    /// [`Self::signed`] keeping the venue's error whole, for a read [`ADMITTED_READ`] judges.
    fn signed_venue(&self, path: &str, params: &[(&str, Value)]) -> Result<String, VenueApiError> {
        self.transport
            .signed(&self.base_url, path, "GET", params, &self.signer)
            .and_then(unwrap_okx)
            .map(|v| v.to_string())
    }

    /// The short-timeout, rate-gate-EXEMPT path — the SAME one `rest::VenueRestOkx::
    /// get_orders_history`/`get_fills_history` already use for the crate's audit-A3 resync, so a
    /// reconcile pass over the heavier fills scan can't blow the per-IP order-rate budget.
    fn signed_requery(&self, path: &str, params: &[(&str, Value)]) -> Result<String, String> {
        self.signed_requery_venue(path, params).map_err(|e| e.to_string())
    }

    /// [`Self::signed_requery`] keeping the venue's error whole, for a read [`ADMITTED_READ`] judges.
    fn signed_requery_venue(
        &self,
        path: &str,
        params: &[(&str, Value)],
    ) -> Result<String, VenueApiError> {
        self.transport
            .signed_requery(&self.base_url, path, "GET", params, &self.signer)
            .and_then(unwrap_okx)
            .map(|v| v.to_string())
    }

    /// One instrument's read in a pass: the MOUNTED instrument's fails as it always did; an ADMITTED one's is
    /// judged by [`ADMITTED_READ`] (owner ruling B-5: a rate limit starts the pass-wide pause). Either way the
    /// error's text is the one this client has always reported.
    fn judged<R>(
        &self,
        instrument: &str,
        read: Result<Vec<R>, ReconcileReadError>,
    ) -> Result<Vec<R>, String> {
        if instrument.eq_ignore_ascii_case(&self.symbol) {
            return read.map_err(read_text);
        }
        if let Err(ReconcileReadError::Venue(e)) = &read
            && (ADMITTED_READ.not_listed)(e)
        {
            self.say_not_listed(instrument, &e.to_string());
        }
        ADMITTED_READ.admitted_rows(VENUE, instrument, &self.pause, read)
    }

    /// The operator is told ONCE per instrument that okx no longer lists the admitted `instrument` (the shared words,
    /// `ReconcileAdmittedSymbols::not_listed_logged`, at ERROR, with this client's account).
    fn say_not_listed(&self, instrument: &str, why: &str) {
        self.admitted.not_listed_logged(instrument, VENUE, &self.account, "an instrument", why);
    }

    fn naming(&self) -> MutexGuard<'_, StrictThenDegrade> {
        self.naming.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The fired stops of `instrument` and the child order each spawned (`orders-algo-history`,
    /// `state=effective`, the newest [`ALGO_PAGE_LIMIT`] stops of the instrument): the join
    /// [`name_algo_children`] applies. The `Err` is the sentence the strict policy below reports.
    fn stop_children(&self, instrument: &str) -> Result<HashMap<String, String>, String> {
        let params = vec![
            ("ordType", json!("conditional")),
            ("state", json!("effective")),
            ("instId", json!(instrument)),
            ("limit", json!(ALGO_PAGE_LIMIT.to_string())),
        ];
        self.signed_requery(rest::PATH_ORDERS_ALGO_HISTORY, &params)
            .and_then(|body| parse_algo_children(&body))
            .map_err(|why| {
                format!(
                    "orders-algo-history (the fired stops) could not be read: {why}; the fills \
                     are refused rather than reported under the venue's child order ids"
                )
            })
    }

    /// The fired TRAILING orders of the whole account and the child each spawned (`orders-algo-history`,
    /// `ordType=move_order_stop`, `state=effective`, the newest [`ALGO_PAGE_LIMIT`]): ONE read per pass, no
    /// `instId`, because the endpoint meters 20 requests per 2 s per account (the vendor's rate line) and a
    /// second read per instrument would double a pass's share of it. `ordType` cannot join `conditional`
    /// in one request (only `conditional` and `oco` combine, the table says). The children's `ordId`s are
    /// the venue's, unique across instruments, so the join applies to every instrument's fills.
    fn trailing_children(&self) -> Result<HashMap<String, String>, String> {
        let params = vec![
            ("ordType", json!("move_order_stop")),
            ("state", json!("effective")),
            ("limit", json!(ALGO_PAGE_LIMIT.to_string())),
        ];
        self.signed_requery(rest::PATH_ORDERS_ALGO_HISTORY, &params)
            .and_then(|body| parse_algo_children(&body))
            .map_err(|why| format!("orders-algo-history (the fired trailing orders): {why}"))
    }

    /// File the fills of fired stops' CHILD orders under the stop's own client id
    /// ([`name_algo_children`]): one [`Self::stop_children`] read per instrument that HAS fills this pass,
    /// none for a pass with no fills. With the trailing reads on ([`Self::with_trailing`]), one
    /// [`Self::trailing_children`] read for the account follows; it is NOT strict: its failure is logged
    /// and leaves only the fired trailing orders' fills unnamed, so an unmeasured request can never fail
    /// the fill read.
    ///
    /// STRICT for the first [`ALGO_NAMING_STRICT_FAILURES`] failing passes since the last good read:
    /// okx claims to list trigger orders, so a fill list handed over with a fired stop's fills still
    /// under the venue's child order id would let the restore planner read a stop that did its job as
    /// gone after the quiet passes. The failure fails the fill read, which skips this venue's leg for
    /// the pass (like a failed `orders-algo-pending`); from the next failure on the pass goes ahead
    /// with those fills unnamed and ONE plain WARN an hour, so a broken endpoint cannot blind the
    /// account for good. The policy counts PASSES: however many instruments' reads fail in one pass,
    /// that is one failure (the first one's sentence). ⚠ RESIDUAL, as on Binance: while degraded, a stop
    /// that fired and filled can be read as cancelled after the quiet passes, and a stop older than the
    /// newest hundred fired ones stays unnamed even on a good read (no lookup of the child order by id,
    /// as Binance has).
    fn name_stop_children(
        &self,
        per_instrument: &mut [(String, Vec<FillReport>)],
    ) -> Result<(), String> {
        let mut failure: Option<String> = None;
        for (instrument, fills) in per_instrument.iter_mut().filter(|(_, fills)| !fills.is_empty())
        {
            match self.stop_children(instrument) {
                Ok(children) => name_algo_children(fills, &children),
                Err(why) => {
                    failure.get_or_insert(why);
                }
            }
        }
        if self.trailing && per_instrument.iter().any(|(_, fills)| !fills.is_empty()) {
            match self.trailing_children() {
                Ok(children) => {
                    for (_, fills) in per_instrument.iter_mut() {
                        name_algo_children(fills, &children);
                    }
                }
                // shortcut: a fired trailing order's fill goes out under the venue's child id while this
                // read fails (a MissingFill for an order the book never heard of), upgrade with a strict
                // policy of its own once the demo has measured the request
                Err(why) => tracing::warn!(
                    venue = VENUE, %why,
                    "the fired trailing orders could not be read; their fills are reported under the \
                     venue's child order ids this pass"
                ),
            }
        }
        let Some(why) = failure else {
            self.naming().succeeded();
            return Ok(());
        };
        if self.naming().failed(Instant::now(), why)? {
            tracing::warn!(
                venue = VENUE,
                "orders-algo-history has stayed unreadable for more than \
                 {ALGO_NAMING_STRICT_FAILURES} passes: the fills of a stop that fired are \
                 reported under the venue's own child order id until it reads again"
            );
        }
        Ok(())
    }

    /// `orders-algo-pending` for ONE instrument, EVERY page of it: the venue lists at most
    /// [`ALGO_PAGE_LIMIT`] rows a request, and `after` (here the `algoId` of the last row seen) asks
    /// for the records EARLIER than it (the vendor's words). The restore planner reads an order a
    /// successful report lacks as gone, so a walk that stopped short must FAIL, never return what it
    /// has: the `ReconcileClient` seam has no "this report may be truncated" claim (decision 0192 names
    /// `maybe_truncated` as a reopener), hence the error.
    ///
    /// ⚠ NOT measured on a venue: that the rows come newest first (the last row of a page is then the
    /// oldest, the cursor the next page needs) and that a chain of more than one page follows
    /// `after` as documented; only the single-page answer was. The repeated-`algoId` failure below
    /// is what keeps a wrong guess from passing as a complete list.
    ///
    /// The walk ends on a SHORT page (fewer rows than asked). It fails on: a page past
    /// [`ALGO_MAX_PAGES`]; an `algoId` that comes back a second time (a venue that ignored `after`,
    /// or sorts the other way, would otherwise yield a "complete" list with rows missing); a FULL
    /// page whose last row carries no `algoId` (nothing to ask `after` from); and any page failing.
    fn resting_stops(
        &self,
        instrument: &str,
        ct_val: f64,
    ) -> Result<Vec<OrderStatusReport>, ReconcileReadError> {
        self.resting_algo_orders(
            &[("ordType", json!("conditional")), ("instId", json!(instrument))],
            ct_val,
            false,
        )
    }

    /// `trade/fills` for ONE instrument since `since` (0 = no lower bound), EVERY page of it, each fill scaled by
    /// `ct_val` (owner ruling F-2: it used to be one page, read as the whole window). The venue lists at most
    /// [`FILLS_PAGE_LIMIT`] rows a request, newest first, and `after` (a row's `billId`) asks for the records
    /// EARLIER than it. A page is full by its ROW count, before [`parse_fills`] drops what is no ordinary trade.
    /// The walk ends on a short page and fails on a full page whose last row has no `billId`, a `billId` that
    /// comes back (a venue that ignored `after`), [`FILLS_MAX_PAGES`] pages and any page failing.
    fn fill_pages(
        &self,
        instrument: &str,
        since: i64,
        ct_val: f64,
    ) -> Result<Vec<FillReport>, ReconcileReadError> {
        let refused = ReconcileReadError::Parse;
        let mut fills = Vec::new();
        let mut after: Option<String> = None;
        let mut seen = std::collections::HashSet::new();
        for _ in 0..FILLS_MAX_PAGES {
            let mut params: Vec<(&str, Value)> = vec![("instId", json!(instrument))];
            if since > 0 {
                params.push(("begin", json!(since.to_string())));
            }
            params.push(("limit", json!(FILLS_PAGE_LIMIT.to_string())));
            if let Some(bill) = &after {
                params.push(("after", json!(bill)));
            }
            let body = self.signed_requery_venue(PATH_FILLS, &params)?;
            fills.extend(parse_fills(&body, ct_val).map_err(refused)?);
            let rows: Vec<Value> =
                serde_json::from_str(&body).map_err(|e| refused(e.to_string()))?;
            if rows.len() < FILLS_PAGE_LIMIT {
                return Ok(fills);
            }
            let bill = rows.last().and_then(|r| r.get("billId")).and_then(Value::as_str);
            let Some(bill) = bill.filter(|b| !b.is_empty()) else {
                return Err(refused(format!(
                    "a full trade/fills page of {instrument} ends in a row with no billId, so there is no \
                     cursor to ask the next page from: the read is refused rather than reported as \
                     complete"
                )));
            };
            if !seen.insert(bill.to_string()) {
                return Err(refused(format!(
                    "trade/fills of {instrument}: billId {bill} came back on a second page: the read is \
                     refused rather than reported as complete"
                )));
            }
            after = Some(bill.to_string());
        }
        Err(refused(format!(
            "trade/fills of {instrument} is still paging after {FILLS_MAX_PAGES} pages of \
             {FILLS_PAGE_LIMIT} rows: the read is refused rather than reported as complete"
        )))
    }

    /// The resting TRAILING orders (`ordType=move_order_stop`) of every instrument `set` reads, from ONE
    /// account-wide paged walk ([`Self::resting_algo_orders`], no `instId`: the endpoint meters 20 requests
    /// per 2 s per account, and a second walk per instrument would double a pass's share of it). Each row
    /// is scaled by ITS instrument's `ctVal`; a row of an instrument `set` does not read is not this
    /// client's to report (positions drop it alike), and a row that is not a trailing order is the
    /// conditional walk's (kept out, so a venue that ignored the `ordType` filter cannot report a stop
    /// twice).
    fn resting_trailing(&self, set: &[(String, f64)]) -> Result<Vec<OrderStatusReport>, String> {
        let rows = self
            .resting_algo_orders(&[("ordType", json!("move_order_stop"))], 1.0, false)
            .map_err(read_text)?;
        Ok(rows
            .into_iter()
            .filter(|r| r.order_type == OrderType::Trailing)
            .filter_map(|mut r| {
                let (_, ct_val) = set
                    .iter()
                    .find(|(instrument, _)| instrument.eq_ignore_ascii_case(&r.symbol))?;
                r.qty *= ct_val;
                Some(r)
            })
            .collect())
    }

    /// **Every open order of the ACCOUNT** (an operator's read, `crate::reconcile::venue_read`; the reconcile reads stay per
    /// instrument): `orders-pending` with no `instId` and no `instType` (every class's resting orders, one page of
    /// [`PENDING_PAGE_LIMIT`]), then the account-wide paged walks of `orders-algo-pending` for the resting stops,
    /// stop-limits and take-profits (`ordType=conditional`) and the trailing orders (`move_order_stop`, its rows kept to
    /// the trailing ones, as [`Self::resting_trailing`] keeps them). A size is the venue's own (`ct_val` 1.0: contracts
    /// on a swap or a future, the coin on a spot pair): no instrument's `ctVal` is read for an operator's read. Any read
    /// failing fails it, on the short-timeout path. The `bool` is `true` when the plain page came back full: the venue
    /// may hold more than it listed.
    pub(crate) fn account_open_orders(&self) -> Result<(Vec<OrderStatusReport>, bool), String> {
        let body = self
            .signed_requery(
                PATH_ORDERS_PENDING,
                &[("limit", json!(PENDING_PAGE_LIMIT.to_string()))],
            )
            .map_err(|why| {
                let list = "orders-pending".to_string();
                OkxAccountRefusal::OpenOrdersUnread { list, why }.to_string()
            })?;
        let mut reports = parse_orders_pending(&body, 1.0)?;
        let more = reports.len() >= PENDING_PAGE_LIMIT;
        let algo = |ord_type: &str| {
            self.resting_algo_orders(&[("ordType", json!(ord_type))], 1.0, true).map_err(|e| {
                let list = format!("orders-algo-pending ({ord_type})");
                OkxAccountRefusal::OpenOrdersUnread { list, why: read_text(e) }.to_string()
            })
        };
        reports.extend(algo("conditional")?);
        reports.extend(
            algo("move_order_stop")?.into_iter().filter(|r| r.order_type == OrderType::Trailing),
        );
        Ok((reports, more))
    }

    /// The paged `orders-algo-pending` walk of [`Self::resting_stops`] over the request `scope`
    /// (`ordType`, and the `instId` when it names one), each page's rows scaled by `ct_val`; `short`
    /// takes the short-timeout path. [`Self::pending_pages`] over [`ALGO_PAGES`].
    fn resting_algo_orders(
        &self,
        scope: &[(&'static str, Value)],
        ct_val: f64,
        short: bool,
    ) -> Result<Vec<OrderStatusReport>, ReconcileReadError> {
        self.pending_pages(&ALGO_PAGES, scope, ct_val, short)
    }

    /// One `orders-*-pending` endpoint (`pages`), EVERY page of it, over the request `scope`, each page's rows
    /// scaled by `ct_val`; `short` takes the short-timeout path. The rule is [`Self::resting_stops`]' doc: the
    /// walk ends on a short page and fails on a page past the cap, an id that comes back a second time, a full
    /// page whose last row has no id, and any page failing. A page the venue refused keeps its error whole
    /// ([`ReconcileReadError::Venue`], for [`ADMITTED_READ`]); a walk that cannot finish is a
    /// [`ReconcileReadError::Parse`] in its own words.
    fn pending_pages(
        &self,
        pages: &PendingPages,
        scope: &[(&'static str, Value)],
        ct_val: f64,
        short: bool,
    ) -> Result<Vec<OrderStatusReport>, ReconcileReadError> {
        let PendingPages { path, id, parse, page_limit, max_pages } = *pages;
        let refused = ReconcileReadError::Parse;
        let mut rows: Vec<OrderStatusReport> = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for _ in 0..max_pages {
            let mut params = scope.to_vec();
            params.push(("limit", json!(page_limit.to_string())));
            if let Some(last) = rows.last() {
                params.push(("after", json!(last.venue_order_id.as_str())));
            }
            let body = if short {
                self.signed_requery_venue(path, &params)?
            } else {
                self.signed_venue(path, &params)?
            };
            let page = parse(&body, ct_val).map_err(refused)?;
            let full = page.len() >= page_limit;
            if let Some(again) = page
                .iter()
                .map(|s| s.venue_order_id.as_str())
                .find(|row_id| !row_id.is_empty() && !seen.insert(row_id.to_string()))
            {
                return Err(refused(format!(
                    "{id} {again} came back on a second page after {} rows: the read is refused \
                     rather than reported as complete",
                    rows.len()
                )));
            }
            rows.extend(page);
            if !full {
                return Ok(rows);
            }
            if rows.last().is_some_and(|s| s.venue_order_id.is_empty()) {
                return Err(refused(format!(
                    "a full page of {} rows ends in a row with no {id}, so there is no cursor to \
                     ask the next page from: the read is refused rather than reported as complete",
                    rows.len()
                )));
            }
        }
        Err(refused(format!(
            "still paging after {max_pages} pages of {page_limit} rows: the read is \
             refused rather than reported as complete"
        )))
    }
}

impl<T: RestTransportJson + Send> ReconcileClient for ReconcileClientOkx<T> {
    /// `since` is a no-op here: `orders-pending` has no time filter and reports only the
    /// CURRENTLY open set — a closed order that fell out of this set is exactly what the fill/
    /// position reports (not this one) reconcile (mirrors Binance's `openOrders`).
    ///
    /// **Resting STOPS are a second read** (`orders-algo-pending`, `ordType=conditional`): `orders-pending`
    /// is not documented to list algo orders, and a report without them would have the restore planner
    /// declare every resting stop `gone`. The algo read is STRICT — a failed one fails the whole report,
    /// so the planner never reads a hidden stop as gone.
    ///
    /// ⚠ **Strict on purpose, and the price is real.** The seam has no "partial listing" channel:
    /// `crates/vike-execution/src/recon/restore.rs`'s `plan_restore` trusts that a fetch it is handed
    /// SUCCEEDED and reads an order the report lacks as gone, so ordinary reports without the stops
    /// would retire a stop that still rests. A failed algo read therefore skips this venue's whole
    /// leg (fills, positions and the balance seed too, `crates/vike-tradehub-core/src/reconcile_manager/manager.rs`'s
    /// `run_pass_at`), with the error below as the reason. The request shape (`ordType` + `instId`, no
    /// `instType`) was accepted by the demo on 2026-10-23: the smoke listed its stop with that shape
    /// (and panics on a refusal); the stop read now also sends `limit=100`, a documented optional
    /// parameter the demo has not yet seen on this path. It PAGES (`resting_stops`): a chain that
    /// cannot finish fails the whole report, like a failed page.
    ///
    /// **Every admitted instrument is read the same way** (decision 0173; `read_set`): the mounted
    /// instrument first, then each one the engine was told, one paged `orders-pending` walk and one paged stop
    /// read apiece, each report scaled by ITS instrument's `ctVal`. Any read failing fails the whole report.
    ///
    /// **The plain read walks every page** ([`PLAIN_PAGES`], owner ruling F-1): `limit=100` and `after` the
    /// last row's `ordId`, by the stop walk's rule. It used to be ONE page with no `limit`, the venue's 100 by
    /// default, so the 101st order of an instrument was missing from a report the planner trusts as complete.
    /// ⚠ Not measured on a venue past one page (no account holds 100 plain orders on one instrument).
    ///
    /// **Resting TRAILING orders are a third read, once per pass, with the trailing reads on**
    /// ([`Self::with_trailing`], okx's row listing `trailing`; off, the pass asks exactly what it always
    /// did). `ordType=move_order_stop` cannot share the stops' request; [`Self::resting_trailing`] is
    /// account-wide and paged, each row scaled by its own instrument's `ctVal`. ⚠ The account-wide request
    /// shape (no `instId`) is the table's (both optional) and NOT yet measured on the demo, so its failure
    /// is LOGGED and leaves out the trailing rows alone: an unmeasured request can never blind the leg.
    fn fetch_order_status_reports(&self, _since: i64) -> Result<Vec<OrderStatusReport>, String> {
        not_backing_off(VENUE, &self.pause)?;
        let mut reports = Vec::new();
        let set = self.read_set()?;
        for (instrument, ct_val) in &set {
            let pending =
                self.pending_pages(&PLAIN_PAGES, &[("instId", json!(instrument))], *ct_val, false);
            reports.extend(self.judged(instrument, pending)?);
            let stops =
                self.judged(instrument, self.resting_stops(instrument, *ct_val)).map_err(|e| {
                    format!("orders-algo-pending (the resting stops) could not be listed: {e}")
                })?;
            reports.extend(stops);
        }
        if self.trailing {
            match self.resting_trailing(&set) {
                Ok(trailing) => reports.extend(trailing),
                // shortcut: the node's trigger claim is static (`RECONCILE_LISTS_TRIGGER_ORDERS`), so while this
                // read fails a restart can retire a resting trailing order after the quiet passes, upgrade
                // with a per-kind claim on the seam (or a strict read once the demo has measured it)
                Err(why) => tracing::warn!(
                    venue = VENUE, %why,
                    "orders-algo-pending (the resting trailing orders) could not be listed; this \
                     pass reports no trailing order"
                ),
            }
        }
        Ok(user_named(reports, |r| &mut r.symbol))
    }

    /// One paged `trade/fills` walk per instrument (`read_set`, [`Self::fill_pages`]), each scaled by its own `ctVal`; the
    /// fired-stop naming follows once for the pass (`name_stop_children`).
    fn fetch_fill_reports(&self, since: i64) -> Result<Vec<FillReport>, String> {
        not_backing_off(VENUE, &self.pause)?;
        let mut per_instrument = Vec::new();
        for (instrument, ct_val) in self.read_set()? {
            let read = self.fill_pages(&instrument, since, ct_val);
            let mut fills = self.judged(&instrument, read)?;
            // The engine's fill ledger (`seen_trade_ids`) holds a fill of an instrument but the mounted one
            // under the id the stream spells for it, `INSTID:tradeId`; the bare id would read as a
            // `MissingFill` for a fill already folded (which `hybrid` books a second time).
            if !instrument.eq_ignore_ascii_case(&self.symbol) {
                for f in &mut fills {
                    f.trade_id =
                        crate::execution::event_mapper::scoped_trade_id(&f.trade_id, &instrument);
                }
            }
            per_instrument.push((instrument, fills));
        }
        self.name_stop_children(&mut per_instrument)?;
        let fills = per_instrument.into_iter().flat_map(|(_, fills)| fills).collect();
        Ok(user_named(fills, |r| &mut r.symbol))
    }

    /// With nothing admitted, the one `positions` read of the mounted instrument it always was. With
    /// admitted instruments, ONE account-wide `instType=SWAP` read, split per instrument client-side:
    /// the endpoint meters 10 requests per 2 seconds, which a read per instrument (up to seventeen)
    /// would overrun. A row of an instrument the engine did not admit is dropped, as the
    /// instrument-filtered read never returned it; an instrument with no row gets the positive flat row
    /// ([`parse_positions`]), so a position the venue closed is still seen, and so does an admitted
    /// instrument the venue no longer lists (it holds nothing). A SPOT pair has no position row: its
    /// holding is its base coin's `cashBal`, ONE `account/balance` read for every spot pair of the pass,
    /// reported as a `Cash` floor row (the spot rule; the coins beyond the engine's are the user's).
    ///
    /// Each pass also runs the mode watch ([`Self::watch_mode`]): the position leg is the one read every
    /// pass makes, and a mode flip is what a position's `posSide` is about.
    fn fetch_position_status_reports(&self) -> Result<Vec<PositionStatusReport>, String> {
        not_backing_off(VENUE, &self.pause)?;
        self.watch_mode();
        let admitted = self.admitted.symbols();
        if admitted.is_empty() {
            let ct_val = self.mounted_ct_val()?;
            let body = self.signed(
                rest::PATH_POSITIONS,
                &[("instType", json!("SWAP")), ("instId", json!(self.symbol))],
            )?;
            return parse_positions(&body, &self.symbol, ct_val)
                .map(|reports| user_named(reports, |r| &mut r.symbol));
        }
        let set = self.read_set()?;
        // One account-wide read per class that has an instrument in the set: the venue files a position
        // under its own `instType` (a dated future is NOT in the SWAP read, MEASURED 2026-10-30), and a
        // SPOT pair has no position row at all (`instType=SPOT` is refused, `51000`). The mounted
        // instrument is a SWAP, so that read is always made, as it was.
        let mut rows: Vec<Value> = Vec::new();
        for class in [InstrumentType::Swap, InstrumentType::Futures] {
            if set
                .iter()
                .any(|(instrument, _)| InstrumentType::of_inst_id(instrument) == Some(class))
            {
                let body =
                    self.signed(rest::PATH_POSITIONS, &[("instType", json!(class.inst_type()))])?;
                rows.extend(serde_json::from_str::<Vec<Value>>(&body).map_err(|e| e.to_string())?);
            }
        }
        let mut reports = Vec::new();
        let mut spot: Vec<&str> = Vec::new();
        for (instrument, ct_val) in &set {
            // A spot pair has no position row: its holding is the account's balance of the base coin, read
            // below for every spot pair at once.
            if InstrumentType::of_inst_id(instrument) == Some(InstrumentType::Spot) {
                spot.push(instrument.as_str());
                continue;
            }
            let mine: Vec<&Value> = rows
                .iter()
                .filter(|r| {
                    r.get("instId")
                        .and_then(Value::as_str)
                        .is_some_and(|s| s.eq_ignore_ascii_case(instrument))
                })
                .collect();
            let mine = serde_json::to_string(&mine).map_err(|e| e.to_string())?;
            reports.extend(parse_positions(&mine, instrument, *ct_val)?);
        }
        // An admitted instrument the venue does not list holds no position: a flat row. A spot pair it does
        // not list still has its coin in the wallet, so it joins the wallet read below instead.
        for gone in admitted.iter().filter(|a| !set.iter().any(|(instrument, _)| instrument == *a))
        {
            if InstrumentType::of_inst_id(gone) == Some(InstrumentType::Spot) {
                spot.push(gone.as_str());
            } else {
                reports.push(PositionStatusReport::flat(VENUE, gone));
            }
        }
        // The spot rule (owner 2026-11-11): a spot pair's holding is its BASE coin's `cashBal`, one read for
        // every spot pair of the pass, reported as a `Cash` FLOOR row with no price through the one spot
        // constructor (flat under the pair's `lotSz`). The coins beyond what the engine bought are the user's
        // and raise nothing; the row is what keeps the orphan sweep from reading the engine's own spot holding
        // as an `OrphanLocalPosition`.
        if !spot.is_empty() {
            let base =
                |instrument: &str| instrument.split('-').next().unwrap_or(instrument).to_string();
            let mut coins: Vec<String> = spot.iter().map(|instrument| base(instrument)).collect();
            coins.sort();
            coins.dedup();
            let body = self.signed(rest::PATH_ACCOUNT, &[("ccy", json!(coins.join(",")))])?;
            let steps = self.spot_steps.lock().unwrap_or_else(PoisonError::into_inner).clone();
            for instrument in spot {
                let held = parse_balance_of(&body, &base(instrument))?.unwrap_or(0.0);
                let step = steps.get(instrument).copied().unwrap_or(0.0);
                let ts = vike_model::now_ms();
                reports.push(PositionStatusReport::spot_holding(VENUE, instrument, held, step, ts));
            }
        }
        Ok(user_named(reports, |r| &mut r.symbol))
    }

    /// Balance meaning (pinned per the task brief): the account's USDT `cashBal` from
    /// `/api/v5/account/balance` (total cash, not available). A light, unfiltered account read —
    /// rides the normal `signed` (rate-gated) path, same as `orders-pending`/`positions` above.
    ///
    /// ⚠ **A COIN-MARGINED mount reports its SETTLE COIN's `cashBal`** (decision 0258: the engine keeps its
    /// books in that coin and adopts this figure), from the same request.
    ///
    /// A failed request carries its outcome under okx's own table (`crate::transport::error_codes::TAXONOMY`):
    /// `50013` (system busy) and `50026` (system error) are not now, never a refused key.
    fn fetch_balance(&self) -> Result<Option<f64>, ReconcileFetchError> {
        let body = self.signed_venue(rest::PATH_ACCOUNT, &[]).map_err(|e| {
            vike_bridge_core::error_kind::read_error(
                Some(&crate::transport::error_codes::TAXONOMY),
                &e,
            )
        })?;
        let balance = match &self.coin {
            Some(coin) => parse_balance_of(&body, coin),
            None => parse_balance(&body),
        };
        balance.map_err(ReconcileFetchError::unclassified)
    }

    /// Live account-actual fee rates (fee model 4/5): `/api/v5/account/trade-fee` for the mounted
    /// instrument's class — negated per OKX's sign convention (see [`parse_fee_rate`]). A light,
    /// unfiltered account read — rides the normal `signed` (rate-gated) path. `Ok(None)` keeps
    /// the static default.
    ///
    /// ⚠ **`instType` ALONE — passing `instId` alongside it is a 400 on this endpoint.** OKX
    /// applies `instId` to SPOT/MARGIN only; combined with `instType=SWAP` it rejects the pair
    /// outright rather than ignoring the narrower key. MEASURED against the live demo endpoint
    /// (`x-simulated-trading: 1`) while bringing up the first real tradehub CEX start:
    ///
    /// ```text
    /// GET /api/v5/account/trade-fee?instType=SWAP&instId=BTC-USDT-SWAP
    ///   -> HTTP 400  {"code":"50016","msg":"instId and instType don't match","data":[]}
    /// GET /api/v5/account/trade-fee?instType=SWAP
    ///   -> HTTP 200  {"code":"0", "data":[{"instType":"SWAP","maker":"-0.0002","taker":"-0.0005",
    ///                                      "makerU":"-0.0002","takerU":"-0.0005",...}]}
    /// ```
    ///
    /// The failure was SILENT in the way that matters: `fetch_fee_rates` is a fail-soft read, so the
    /// error fell through to the static default fee schedule and every reconcile pass kept working
    /// while never once using the account's real rates. A maker strategy sized against the wrong
    /// fee is the whole edge on this venue.
    ///
    /// This does NOT narrow the answer. The rates returned are the account's SWAP tier, and
    /// [`parse_fee_rate`] reads `makerU`/`takerU` — the USDT-margined rates, which is what
    /// `BTC-USDT-SWAP` settles in. ⚠ The sibling `fetch_position_status_reports` sends the SAME pair
    /// and is CORRECT: `/api/v5/account/positions` does accept `instType` + `instId` together
    /// (verified `code: 0` in the same session). The rule is per-endpoint, not per-venue — do not
    /// "fix" that one by symmetry.
    ///
    /// The `instType` is the MOUNTED instrument's own class ([`InstrumentType::of_inst_id_or_refusal`]): a spot pair
    /// asks `SPOT` and is priced at its own fee group's row ([`parse_spot_fee_rate`]), the group read off
    /// the pair's `public/instruments` row (`groupId`); a dated future asks `FUTURES`. It used to ask `SWAP`
    /// whatever was mounted, so a spot or dated mount was priced at the swap tier. This bridge mounts no
    /// option.
    ///
    /// A COIN-MARGINED mount reads the same answer's crypto-margined rates ([`parse_coin_fee_rate`]).
    fn fetch_fee_rates(&self) -> Result<Option<FeeSchedule>, String> {
        let class = InstrumentType::of_inst_id_or_refusal(&self.symbol)?;
        let body = self.signed(PATH_TRADE_FEE, &[("instType", json!(class.inst_type()))])?;
        if self.coin.is_some() {
            return parse_coin_fee_rate(&body);
        }
        if class != InstrumentType::Spot {
            return parse_fee_rate(&body);
        }
        let params = [("instType", class.inst_type().to_string()), ("instId", self.symbol.clone())];
        let data = self
            .transport
            .public(&self.base_url, rest::PATH_INSTRUMENTS, &params)
            .and_then(unwrap_okx)
            .map_err(|e| {
                format!("public/instruments for {} could not be read: {e}", self.symbol)
            })?;
        let group = data.get(0).and_then(|r| r.get("groupId")).and_then(Value::as_str);
        let group = group.filter(|g| !g.is_empty()).ok_or_else(|| {
            format!("public/instruments states no fee group (groupId) for {}", self.symbol)
        })?;
        parse_spot_fee_rate(&body, group)
    }

    /// **Tell this client the engine admitted `symbol`** (decision 0173): the next pass reads that
    /// instrument too, with its own `ctVal` (the module doc's "Admitted instruments"). Only the
    /// list is touched here; the `ctVal` read is the pass's, never this call's (it may come from a
    /// thread that must not wait on the venue).
    ///
    /// `symbol` is a USER name (decision 0290): the list holds its kept row's instId, the name every read, the funding
    /// poll and the replays of this account ask by (the SAME `Arc`, `crate::adapter`). A name no kept row answers is
    /// held by nothing, said at WARN: no read could name it.
    fn admit_symbol(&self, symbol: &str) {
        match crate::instrument::inst_id_of(symbol) {
            Ok(inst) => {
                self.admitted.admit(&inst, &self.symbol);
            }
            Err(why) => {
                tracing::warn!(venue = VENUE, %symbol, "{why}; the reconcile pass does not read it")
            }
        }
    }

    /// The engine gave `symbol` back (decision 0205): the next pass stops reading it.
    fn forget_symbol(&self, symbol: &str) {
        if let Ok(inst) = crate::instrument::inst_id_of(symbol) {
            self.admitted.forget(&inst);
        }
    }

    /// **The instruments this client reads beyond the mounted one**: every one it was told (the list is
    /// bounded by the engine's own cap), by its USER name. A CLAIM, so it names only what a pass really reads. The
    /// restore planner may retire a resting order on a claimed instrument as "gone" when the report lacks it,
    /// which is right only because [`Self::fetch_order_status_reports`] reads that instrument.
    fn admitted_symbols(&self) -> Option<Vec<String>> {
        Some(
            self.admitted
                .symbols()
                .iter()
                .map(|inst| crate::instrument::user_name_of(inst))
                .collect(),
        )
    }

    /// **CLAIMS that its open-order read lists the venue's trigger orders** (decision 0192's kind claim:
    /// the restore planner may call an absent stop gone, after the quiet passes, only for a client that
    /// says so). It does: `fetch_order_status_reports` reads `orders-algo-pending` (STRICT, and paged
    /// by `resting_stops`: a failed or unfinished read fails the report, never returns a short one) and
    /// types every row as the kind it was placed as: `OrderType::StopLimit` for a priced `slOrdPx`,
    /// `OrderType::TakeProfit` for a take-profit row, else `OrderType::Stop` ([`conditional_kind`]); with
    /// okx's row listing `trailing`, the resting trailing orders are the account-wide `move_order_stop` read
    /// beside it (`resting_trailing`, each an `OrderType::Trailing`; NOT strict while unmeasured, its
    /// `shortcut:` says what that costs), and a fired one's child fills are named the same way
    /// (`trailing_children`). The other half of the proof is the fill side: a stop that FIRED while the node was down
    /// is not pending, and its child's fills are named by the stop's own id (`name_stop_children`,
    /// strict for three failing passes), so a stop that did its job is `filled_while_down` and not
    /// silence. The harness row proves the parsed stop
    /// (`crates/vike-bridge-core/tests/bridge_conformance/venues.rs`'s `okx_algo_open_rows`); the node
    /// answers from `RECONCILE_LISTS_TRIGGER_ORDERS`, which a harness test holds equal to the proven claimants.
    fn lists_trigger_orders(&self) -> bool {
        true
    }

    /// **Which okx account is this key?** — `/api/v5/account/config`'s `uid`, asked once per client.
    ///
    /// ⚠ **The venue answers a SECOND field this seam does not carry.** `mainUid` is the parent,
    /// and nothing in this tree reads it. The trait carries ONE string, and the one it carries is the
    /// BOOK — `uid`, *this* account, the value that belongs in `vike_secrets::AccountRow::venue_account_id`.
    ///
    /// The cache is the one the trait permits and nothing more: an ERROR is never remembered, so an
    /// expired key is re-discovered on the next pass and at every restart.
    fn fetch_account_identity(&self) -> Result<Option<String>, String> {
        if let Some(cached) = self.identity.get() {
            return Ok(cached.clone());
        }
        let uid = crate::account::identity::account_identity(
            &self.transport,
            &self.signer,
            &self.base_url,
        )
        .map_err(|e| e.to_string())?
        .map(|id| id.uid);
        // `set` races only with another thread asking at the same moment, and both computed the
        // same answer from the same endpoint — so a loser is a discarded duplicate, never a
        // disagreement. Read the winner back rather than returning the local value, so every
        // caller in that race sees ONE id.
        let _ = self.identity.set(uid);
        Ok(self.identity.get().cloned().flatten())
    }
}

#[cfg(test)]
mod identity_tests {
    use super::*;
    use vike_bridge_core::signer::SignerJson;
    use vike_bridge_core::transport::VenueApiError;
    use vike_bridge_core::transport::json::RestTransportJson;
    use vike_secrets::credentials::Credentials;

    /// A transport that RECORDS every signed path. The recording is the whole test: the claim
    /// `fetch_account_identity` makes is *one request per client*, and only a count can check it.
    struct CountingTransport {
        paths: std::sync::Mutex<Vec<String>>,
        answer: Value,
    }

    impl CountingTransport {
        fn new(answer: Value) -> Self {
            CountingTransport { paths: std::sync::Mutex::new(Vec::new()), answer }
        }
        fn paths(&self) -> Vec<String> {
            self.paths.lock().expect("not poisoned").clone()
        }
    }

    impl RestTransportJson for CountingTransport {
        fn signed(
            &self,
            _base_url: &str,
            path: &str,
            _method: &str,
            _params: &[(&str, Value)],
            _signer: &dyn SignerJson,
        ) -> Result<Value, VenueApiError> {
            self.paths.lock().expect("not poisoned").push(path.to_string());
            Ok(self.answer.clone())
        }

        vike_bridge_core::no_public_call!(unreachable: "the identity read is SIGNED — a public call here would be a wrong endpoint");
    }

    fn client(answer: Value) -> ReconcileClientOkx<CountingTransport> {
        let creds = Credentials::for_test("k", "s", Some("p"));
        ReconcileClientOkx::new(
            OkxV5Signer::new(&creds, || 0),
            CountingTransport::new(answer),
            "https://example.invalid",
            "BTC-USDT-SWAP",
            0.01,
        )
    }

    fn envelope(rows: Value) -> Value {
        json!({"code": "0", "msg": "", "data": rows})
    }

    /// ⚠ **THE CLAIM THIS METHOD MAKES.** Two asks, ONE signed request — and it is
    /// `/api/v5/account/config`, the endpoint nothing else on this client touches, so the count is
    /// unambiguous about which call was saved.
    #[test]
    fn a_second_ask_costs_no_second_request() {
        let c = client(envelope(json!([{"uid": "42", "mainUid": "42"}])));
        assert_eq!(c.fetch_account_identity(), Ok(Some("42".into())));
        assert_eq!(c.fetch_account_identity(), Ok(Some("42".into())));
        assert_eq!(
            c.transport.paths(),
            vec![crate::account::identity::PATH_ACCOUNT_CONFIG.to_string()]
        );
    }

    /// The BOOK is this account, not its parent — a sub-account must not be recorded as its master,
    /// which is exactly the collision `crates/vike-gatewayhub/src/trade/adapters/book_identity.rs`
    /// exists to break.
    #[test]
    fn a_sub_account_reports_its_own_uid_not_the_parents() {
        let c = client(envelope(json!([{"uid": "222", "mainUid": "111"}])));
        assert_eq!(c.fetch_account_identity(), Ok(Some("222".into())));
    }

    /// ⚠ **okx claims exactly the instruments it was told, and telling it fetches nothing**: the claim
    /// is what lets the restore planner retire a resting order on an admitted instrument the report
    /// lacks, so it names only what a pass reads. It is told USER names and claims them back (decision 0290): the
    /// mounted instrument, a repeat (any case), an empty name, a name with no kept row and an instId typed as a name
    /// are not added (the list's cap is `ReconcileAdmittedSymbols`', pinned in its own tests).
    #[test]
    fn it_claims_each_admitted_instrument_it_is_told_and_fetches_nothing() {
        crate::instrument::kept_list_for_tests();
        let c = client(envelope(json!([])));
        assert_eq!(c.admitted_symbols(), Some(Vec::new()), "told nothing: a claim of nothing");
        c.admit_symbol("ETHUSDT.P");
        c.admit_symbol("ethusdt.p");
        c.admit_symbol("BTCUSDT.P");
        c.admit_symbol("");
        c.admit_symbol("NOSUCHUSDT.P");
        c.admit_symbol("XRP-USDT-SWAP");
        assert_eq!(c.admitted_symbols(), Some(vec!["ETHUSDT.P".to_string()]));
        assert!(c.transport.paths().is_empty(), "telling it fetches nothing");
    }

    /// A venue that answered NOTHING is remembered as *not yet known* rather than re-asked in a
    /// loop — same fail-soft rule the parser has.
    #[test]
    fn an_empty_answer_is_remembered_as_unknown() {
        let c = client(envelope(json!([])));
        assert_eq!(c.fetch_account_identity(), Ok(None));
        assert_eq!(c.fetch_account_identity(), Ok(None));
        assert_eq!(c.transport.paths().len(), 1);
    }

    /// ⚠ **AN ERROR IS NEVER CACHED.** This is what keeps an EXPIRED key discoverable: a bybit key
    /// answered *Your api key has expired* on 2026-09-20 and the preflight degraded that venue to
    /// paper. A remembered failure would retire that check at the first flap.
    #[test]
    fn an_error_is_re_asked_rather_than_remembered() {
        let c = client(json!({"code": "50113", "msg": "Invalid Sign", "data": []}));
        assert!(c.fetch_account_identity().is_err());
        assert!(c.fetch_account_identity().is_err());
        assert_eq!(c.transport.paths().len(), 2, "the second ask must reach the venue again");
    }
}

#[cfg(test)]
mod mode_watch_tests {
    use super::*;
    use vike_bridge_core::signer::SignerJson;
    use vike_bridge_core::transport::VenueApiError;
    use vike_bridge_core::transport::json::RestTransportJson;
    use vike_secrets::credentials::Credentials;

    /// Answers `account/config` with the scripted `posMode` words in order (the last repeats) and
    /// `positions` with no rows, and records every signed path: the claims are about WHICH requests a
    /// pass makes, and only the list can check that.
    struct ModeVenue {
        simulated: bool,
        modes: Vec<&'static str>,
        paths: Mutex<Vec<String>>,
    }

    impl RestTransportJson for ModeVenue {
        fn signed(
            &self,
            _base_url: &str,
            path: &str,
            _method: &str,
            _params: &[(&str, Value)],
            _signer: &dyn SignerJson,
        ) -> Result<Value, VenueApiError> {
            let mut paths = self.paths.lock().expect("not poisoned");
            let asked = paths
                .iter()
                .filter(|p| *p == crate::account::identity::PATH_ACCOUNT_CONFIG)
                .count();
            paths.push(path.to_string());
            let rows = match path {
                crate::account::identity::PATH_ACCOUNT_CONFIG => {
                    json!([{ "posMode": self.modes[asked.min(self.modes.len() - 1)] }])
                }
                rest::PATH_POSITIONS => json!([]),
                other => panic!("the position leg made a request nobody scripted: {other}"),
            };
            Ok(json!({"code": "0", "msg": "", "data": rows}))
        }

        vike_bridge_core::no_public_call!("the position leg is SIGNED only: a public call");

        fn network(&self) -> vike_model::VenueNetwork {
            use vike_model::VenueNetwork::{Mainnet, Testnet};
            if self.simulated { Testnet } else { Mainnet }
        }
    }

    fn creds() -> Credentials {
        Credentials::for_test("k", "s", Some("p"))
    }

    fn client(simulated: bool, modes: Vec<&'static str>) -> ReconcileClientOkx<ModeVenue> {
        ReconcileClientOkx::new(
            OkxV5Signer::new(&creds(), || 0),
            ModeVenue { simulated, modes, paths: Mutex::new(Vec::new()) },
            "https://example.invalid",
            "BTC-USDT-SWAP",
            0.01,
        )
    }

    fn config_reads(c: &ReconcileClientOkx<ModeVenue>) -> usize {
        let paths = c.transport.paths.lock().expect("not poisoned");
        paths.iter().filter(|p| *p == crate::account::identity::PATH_ACCOUNT_CONFIG).count()
    }

    /// ⚠ **THE CLAIM OF THE MODE WATCH.** On the simulated network every position leg of a pass reads
    /// `account/config` once and remembers whether the account is long/short, so a flip while the engine
    /// runs is seen at the next pass (net, long/short, long/short, net). It writes nothing (the venue
    /// would panic on an unscripted POST) and never fails the pass.
    #[test]
    fn each_pass_reads_the_mode_once_and_remembers_a_flip() {
        let c = client(true, vec!["net_mode", "long_short_mode", "long_short_mode", "net_mode"]);
        let wants = [Some(false), Some(true), Some(true), Some(false)];
        for (pass, want) in wants.into_iter().enumerate() {
            c.fetch_position_status_reports().expect("the watch never fails the pass");
            assert_eq!(*c.long_short_seen.lock().unwrap(), want, "pass {pass}");
            assert_eq!(config_reads(&c), pass + 1, "one mode read per pass");
        }
    }

    /// A LIVE account is never read for a mode (the start probe's gate, owner ruling 2026-10-26): the
    /// pass makes its position read and no `account/config` request at all.
    #[test]
    fn a_live_account_is_not_read_for_a_mode() {
        let c = client(false, vec!["long_short_mode"]);
        c.fetch_position_status_reports().expect("the position leg works");
        assert_eq!(config_reads(&c), 0);
        assert_eq!(*c.long_short_seen.lock().unwrap(), None);
    }

    /// A mode word outside the two known ones changes nothing and is not guessed at.
    #[test]
    fn an_unknown_mode_word_is_not_remembered() {
        let c = client(true, vec!["long_short_mode", "multi_currency"]);
        c.fetch_position_status_reports().expect("pass 1");
        c.fetch_position_status_reports().expect("pass 2");
        assert_eq!(*c.long_short_seen.lock().unwrap(), Some(true), "the unknown word left it");
    }

    /// The admitted list the adapter hands the funding poll IS the list this client is told: an
    /// admission through the client shows in the shared `Arc`, and the client reads it back.
    #[test]
    fn an_admission_reaches_the_list_the_funding_poll_shares() {
        crate::instrument::kept_list_for_tests();
        let shared = Arc::<ReconcileAdmittedSymbols>::default();
        let net = crate::test_support::demo_net();
        let clock = VenueClock::default();
        let boxed = reconcile_client_sharing(
            &creds(),
            "BTC-USDT-SWAP",
            0.01,
            &net,
            shared.clone(),
            &clock,
            "a",
        )
        .expect("built");
        // told USER names; the shared list (the funding poll's) holds the instIds, the claim speaks user names
        boxed.admit_symbol("ethusdt.p");
        boxed.admit_symbol("BTCUSDT.P"); // the mounted one is never in it
        assert_eq!(shared.symbols(), vec!["ETH-USDT-SWAP".to_string()]);
        assert_eq!(boxed.admitted_symbols(), Some(vec!["ETHUSDT.P".to_string()]));
    }
}
