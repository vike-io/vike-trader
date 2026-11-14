//! cTrader `ReconcileClient` (ReconFactory seam, wave-2 task 6) — the venue-facing report seam
//! (`vike_execution::recon::ReconcileClient`) over a DEDICATED authed connection [`ReconcileClientCtrader::connect`]
//! opens via the SAME handshake [`crate::transport::connection::open_and_handshake`] runs for the exec/data actor
//! (mirrors Deribit's own dedicated-socket `ReconcileClient`: a reconcile fetch never contends the
//! exec side's ONE multiplexed socket, and a resync burst here can never delay an order write).
//!
//! Reconciles against:
//!   `ProtoOAReconcileReq`/`Res` (order + position reports) — the SAME reconcile payload
//!   [`crate::transport::connection`]'s `reconcile_after_reconnect` already decodes on every reconnect (F3); this
//!   client just exposes it through the trait instead of only the reconnect-recovery inline path.
//!   Neither report carries a per-symbol filter on the wire (`ProtoOAReconcileReq` has none), so
//!   both are filtered client-side to the engine symbol; [`position_reports`] answers the flat row
//!   when the symbol is absent — cTrader (like OKX/Alpaca) omits a CLOSED position entirely rather
//!   than sending a zero-volume row, so a stale local position stays detectable.
//!
//! ## Position report keys ([`position_reports`])
//! The venue lists one row per `positionId` (a HEDGED account can hold several per symbol, on both sides),
//! while the engine's book is ONE signed net per `(symbol, BOTH)`: every cTrader fill is published with the
//! default `position_side` (`feed::decode::deal_fill`), and `vike_execution::recon::diff` matches a report to the
//! book by `(symbol, position_side)` EXACTLY. A report spelled `Long`/`Short` therefore never matched, and an
//! open position, hedged or not, held an `OrphanLocalPosition` (the book's `BOTH` row, "no venue row") plus a
//! `PositionOnlyExternal` (the venue's `Long` row) on every pass until it closed. The fetch now reports a
//! symbol's positions as ONE `Both` row with the sign in `qty` (IG's convention), and keeps the directional
//! rows only for a symbol that holds BOTH directions (a locked or partly hedged pair), which one number cannot
//! hold and which stays a held, visible divergence.
//!   `ProtoOADealListReq`/`Res` (fill reports — historical deals by timestamp range; `since` maps
//!   directly to `from_timestamp`, both already epoch-ms, no unit conversion needed, unlike
//!   Alpaca's RFC3339 gap).
//!
//! ## Admitted symbols (any-symbol, decision 0173)
//! The client is built on the MOUNTED symbol, and [`ReconcileClient::admit_symbol`] tells it each symbol the
//! account's engine was admitted on demand. All three wire reads are account-wide (`RECONCILE_REQ` carries
//! every order and position, `DEAL_LIST_REQ` every deal), so a pass reads the SAME requests as before and
//! filters the answer on the mounted symbol FIRST and each admitted one after it: orders and positions by
//! name (an admitted symbol with no open position reports a synthesized flat row, as the mounted one does),
//! fills by symbol id out of the one deal list. With nothing admitted a pass is byte-identical to what it
//! was. A pass that cannot read a symbol it claims FAILS whole (the restore planner reads an order a
//! successful report lacks as gone): an admitted name the account's symbol list no longer carries, or two
//! names that differ only by case, and a deal list still truncated (`has_more`) after `DEAL_PAGE_ROUNDS`
//! requests (only paged when something is admitted: each next request starts, or ends, at the previous
//! page's edge; the mounted-only pass reads the first chunk, as ever). The told names are upper-case
//! ([`ReconcileAdmittedSymbols`]); a broker's names are case-sensitive, so each is matched case-insensitively
//! against the handshake's list and the report carries the broker's own spelling, which is the one the
//! engine trades (the door sends the name exactly as the facts listed it).
//!
//! **A fill report carries our order client id from the deal's `label`** ([`deal_order_client_ids`]). `ProtoOADeal.label` (tag 18, "Label
//! field value from corresponding order", max 100) came with the venue's Release 100 (spotware/openapi-proto-messages
//! `a8824dc`, 2026-11-04), after the vendored pin `3fd8bdd`, so the generated [`ProtoOaDeal`] lacks it and the deal
//! list's bytes are read a second time for it. `crate::feed::decode`'s `order_to_new_order` sends the order client id as the
//! order's label, so a deal of ours names it; a leg of a split exit (`<parent>L<n>`) reports its parent while the
//! exec's position book still holds the group (`crate::execution::exit_legs`), or, after a gatewayhub restart emptied the table,
//! while `<parent>` reads as one of our v2 ids. ⚠ A deal carries no `clientOrderId`, so `connection::reconcile`'s
//! `node_placed_order_client_id` test (both fields equal) cannot be made, and a label is OURS only by `label_order_client_id`'s one rule:
//! a leg, a v2 id (`vike_model`'s `parse_order_client_id`), or an order client id the bridge KNOWS (it placed it, or a restore taught it
//! through `adopt_restored`: `crate::execution::positions::PositionsById::know_order_client_id`), which covers an operator's own id and one
//! minted before v2. Anything else, a label typed in the platform or a cAlgo bot's, no label or an empty one, is
//! `None`: the "externally-placed order" convention every venue's parser uses for an absent client id, so a foreign
//! deal is never folded as one of our orders. `parse_orders` applies the same rule to its label fallback, maps a
//! resting leg to its exit the same way, and reports the resting legs of one exit as one row.
//!
//! ⚠ **A CLOSING deal is named by its own order's `clientOrderId`, never by its label**
//! (`ReconcileClientCtrader::name_closing_deals`). The order a `ProtoOAClosePositionReq` makes carries the label of
//! the POSITION it closes, so the closing deal of a flatten read as the OPENER's fill: the CI box, 2026-11-09, demo account
//! 5, deal 958656727 held as a `MissingFill` under the buy that opened the position while the book held it under the
//! close. The venue's order list names that order by the close's envelope id (the same 2026-11-09 run), and a bound
//! exit's by its own order client id. The exec's order-client-id-to-order map cannot stand in: it forgets an order at its terminal event, a
//! gatewayhub restart empties it, and an unanswered close was never in it.
//!
//! **A pass waits while the exec's reconnect settles a dead socket** ([`SETTLING`]): the same run's pass at 21:23:09.75
//! came 4 s before the settle booked the close, and raised the missed fill, a drift and an orphaned order for it.
//!
//! **Known simplification**: `fetch_order_status_reports` and `fetch_position_status_reports`
//! each issue their OWN `ProtoOAReconcileReq` round trip even though both reports come off the
//! SAME response — no cross-method caching (the `ReconcileClient` trait gives each fetch no way to
//! share state with its siblings within one pass). A rare periodic/on-reconnect sync doubling one
//! cheap request is an acceptable trade-off; revisit if this becomes a real polling hot path.
//!
//! **No live fee-rate lane**: cTrader's Open API exposes no per-account maker/taker rate over this
//! protocol, so `fetch_fee_rates` stays the trait default (`Ok(None)`).
//!
//! **The balance IS read** ([`parse_balance`], `ReconcileClient::fetch_balance`): one `ProtoOATraderReq`
//! per call, because without it the engine of a cTrader account never left its configured seed cash
//! (the trait default says "this venue reports none") and its buying-power gate judged every order
//! against that seed. It is refused (`Err`, the seed stays and the node logs `balance fetch failed`)
//! for a deposit currency other than USD, which the bare number cannot say.
//!
//! **Idle connection (was the MUST-FIX-BEFORE-MOUNT gate).** This socket sends no heartbeats —
//! `HEARTBEAT_INTERVAL` (10s, see [`crate::transport::connection`]) is kept only by the exec actor's own loop — and
//! it sits idle between reconcile passes, while `conn`'s module doc records that cTrader
//! disconnects idle sockets. At the default `VIKE_RECONCILE_INTERVAL_MS` (60s) cadence it would
//! therefore be server-closed before the second pass, failing every later fetch. Resolved by
//! LAZY REVIVE rather than a keepalive: a fetch that finds the connection idle longer than
//! `IDLE_REVIVE_AFTER` re-runs the handshake before its request. See
//! [`ReconcileClientCtrader`]'s own doc for why that beats a heartbeat thread and why it costs no more
//! than a per-pass reconnect. Proven by `tests/offline/reconcile_client_revive.rs` against a fake server that
//! closes after every pass — no live venue, no real idle wait.
//!
//! Every wire->report mapping delegates to a PURE free function (`parse_*`) over the
//! ALREADY-DECODED prost types (cTrader is protobuf, not REST/JSON) — fixture-tested
//! (`tests/offline/reconcile_client_parse.rs`, constructs the prost structs directly, no network).

pub(crate) mod venue_read;

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use prost::Message;

use vike_bridge_core::reconcile_admitted::ReconcileAdmittedSymbols;
use vike_execution::recon::ReconcileClient;
use vike_model::events::{LiquiditySide, TradeId};
use vike_model::{
    ContractValue, FillReport, OrderStatus, OrderStatusReport, OrderType, PositionLeg,
    PositionStatusReport,
};

use crate::execution::positions::{
    PositionsById, is_v2_order_client_id, open_rows, reconcile_rows,
};
use crate::feed::decode::stop_limit_price;
use crate::instrument::symbols::SymbolMap;
use crate::transport::connection::{self, ConnectionConfig, PositionsByIdShared, Reader};
use crate::transport::proto::{
    ProtoMessage, ProtoOaDeal, ProtoOaDealListReq, ProtoOaDealListRes, ProtoOaDealStatus,
    ProtoOaGetPositionUnrealizedPnLReq, ProtoOaGetPositionUnrealizedPnLRes, ProtoOaOrder,
    ProtoOaOrderListReq, ProtoOaOrderListRes, ProtoOaOrderStatus, ProtoOaOrderType,
    ProtoOaPosition, ProtoOaReconcileReq, ProtoOaReconcileRes, ProtoOaTradeSide, ProtoOaTrader,
    ProtoOaTraderReq, ProtoOaTraderRes, pt,
};

const VENUE: &str = "ctrader";

// --- pure parsers (over already-decoded prost types, not raw wire bytes) ------------------------

/// `tradeSide` -> signed side: `SELL` -> `-1`, anything else (`BUY`) -> `+1`. Matches
/// `feed::decode::deal_fill`'s own `deal.trade_side == ProtoOaTradeSide::Sell as i32` check.
fn side_sign(trade_side: i32) -> i32 {
    if trade_side == ProtoOaTradeSide::Sell as i32 { -1 } else { 1 }
}

/// The `ProtoOAOrderType` as an [`OrderType`]: `MARKET`, `LIMIT`, `STOP` and `STOP_LIMIT` are their
/// twins; `MARKET_RANGE` (a market order bounded by a slippage range) and `STOP_LOSS_TAKE_PROFIT` (a
/// position's protection order) have none and stay [`OrderType::Unrecognized`] with the enum's own
/// wire name lower-cased, as every report spelled them before the type existed. A raw value the
/// generated enum does not know is WARNED and kept as an empty `Unrecognized` — never panics.
fn order_type_of(order_type: i32) -> OrderType {
    match ProtoOaOrderType::try_from(order_type) {
        Ok(ProtoOaOrderType::Market) => OrderType::Market,
        Ok(ProtoOaOrderType::Limit) => OrderType::Limit,
        Ok(ProtoOaOrderType::Stop) => OrderType::Stop,
        Ok(ProtoOaOrderType::StopLimit) => OrderType::StopLimit,
        Ok(t @ (ProtoOaOrderType::MarketRange | ProtoOaOrderType::StopLossTakeProfit)) => {
            OrderType::Unrecognized(t.as_str_name().to_ascii_lowercase())
        }
        Err(_) => {
            tracing::warn!(venue = VENUE, order_type, "unknown ProtoOAOrderType value");
            OrderType::Unrecognized(String::new())
        }
    }
}

/// Map a `ProtoOAOrder` onto the FSM [`OrderStatus`] — every `ProtoOAOrderStatus` value,
/// explicitly. `ORDER_STATUS_ACCEPTED` (the only non-terminal status) is further split by fill
/// progress ([`OrderStatus::from_fill_progress`]): any `executedVolume` -> `PartiallyFilled`, its
/// whole `volume` included, because the row still rests (B-16, the owner's ruling), the one rule of
/// every venue whose resting-order status has no distinct "partially filled" value; `FILLED`,
/// `REJECTED`, `EXPIRED` and `CANCELLED` are their twins. A raw value the generated enum does not know is WARNED and read as `Accepted`:
/// live, the conservative direction — `diff` raises no `MissingTerminal` on it, adoption synthesizes
/// no cumulative fill, and the restore planner keeps the order rather than declaring it gone.
fn order_status_of(order: &ProtoOaOrder) -> OrderStatus {
    match ProtoOaOrderStatus::try_from(order.order_status) {
        Ok(ProtoOaOrderStatus::OrderStatusAccepted) => {
            OrderStatus::from_fill_progress(order.executed_volume.unwrap_or(0) as f64)
        }
        Ok(ProtoOaOrderStatus::OrderStatusFilled) => OrderStatus::Filled,
        Ok(ProtoOaOrderStatus::OrderStatusRejected) => OrderStatus::Rejected,
        Ok(ProtoOaOrderStatus::OrderStatusExpired) => OrderStatus::Expired,
        Ok(ProtoOaOrderStatus::OrderStatusCancelled) => OrderStatus::Canceled,
        Err(_) => {
            tracing::warn!(
                venue = VENUE,
                order_status = order.order_status,
                "unknown ProtoOAOrderStatus value — reporting the order as live (Accepted)"
            );
            OrderStatus::Accepted
        }
    }
}

/// The exit a split exit's leg `order_client_id` belongs to: the exec's table first (`crate::execution::exit_legs`), while `legs` holds the
/// group; else the spelling `<parent>L<n>` when `<parent>` reads as one of our v2 ids, which no v2 id itself ends
/// like (its roles are `EN`, `SL`, `TP<n>`, `M<n>`, its generations `R<n>`). The spelling is how a leg that rests
/// across a gatewayhub restart, whose table the restart emptied, still names its exit. `None` for any other order client id.
fn exit_of_leg<'a>(order_client_id: &'a str, legs: Option<&'a PositionsById>) -> Option<&'a str> {
    if let Some(exit) = legs.and_then(|book| book.exit_leg_parent(order_client_id)) {
        return Some(exit);
    }
    let (parent, n) = order_client_id.rsplit_once('L')?;
    (!n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()) && is_v2_order_client_id(parent))
        .then_some(parent)
}

/// **The one rule for a LABEL this bridge reads back: the order client id it names when it is OURS.** A leg of a split exit
/// reports its exit ([`exit_of_leg`]); any other label is ours when it reads as one of our v2 ids, or when the bridge
/// KNOWS it (`PositionsById::know_order_client_id`: it placed it, or a restore taught it), which is what makes an operator's own
/// id or one minted before v2 ours. Anything else (no text, "my hedge", `EXT-…`, a lower-case word that only looks
/// like an id minted before v2, an id-shaped text under a letter no source has) is `None`, foreign, never folded as
/// one of our orders (a foreign order is its owner's, on every venue).
fn label_order_client_id(label: &str, legs: Option<&PositionsById>) -> Option<String> {
    if let Some(exit) = exit_of_leg(label, legs) {
        return Some(exit.to_string());
    }
    let ours =
        is_v2_order_client_id(label) || legs.is_some_and(|book| book.knows_order_client_id(label));
    ours.then(|| label.to_string())
}

/// One more resting leg of a split exit folded into its exit's row (every leg rests at the exit's terms, so type,
/// side and prices stay the first leg's): the sizes add up, the fill price is the size-weighted one, the newest `ts`
/// wins, and a live row with any summed progress reads `PartiallyFilled` ([`order_status_of`]'s split).
fn merge_leg(row: &mut OrderStatusReport, leg: &OrderStatusReport) {
    let filled = row.filled_qty + leg.filled_qty;
    if filled > 0.0 {
        // cTrader's FX/CFD symbols are linear: the size-weighted average, through the one averaging law.
        row.average_price = ContractValue::linear(1.0).average_price(
            row.filled_qty,
            row.average_price,
            leg.filled_qty,
            leg.average_price,
        );
    }
    row.qty += leg.qty;
    row.filled_qty = filled;
    row.timestamp = row.timestamp.max(leg.timestamp);
    if !row.status.is_terminal() && filled > 0.0 {
        row.status = OrderStatus::PartiallyFilled;
    }
}

/// `ProtoOAReconcileRes.order` rows -> `OrderStatusReport`, resolving each row's numeric
/// `symbolId` to the vike symbol name via `symbols`. `order_client_id` prefers the dedicated
/// `clientOrderId` field, falling back to `tradeData.label` — the SAME precedence
/// `connection::reconcile_order_order_client_id` already uses to rebuild the order-client-id->orderId map on reconnect. The dedicated
/// field is read as it comes (the node writes its order client id there, `node_placed_order_client_id`); a label alone counts only by
/// `label_order_client_id`'s rule. A leg of a split exit reports its exit, as its fills do ([`deal_order_client_ids`]), and the resting
/// legs of one exit are ONE row (`merge_leg`: the exit's remaining size is all of them, which a restore that keeps one
/// row per order client id would otherwise understate). `volume`/`executedVolume` are in CENTS (divide by 100 for real units,
/// per the wire doc).
pub fn parse_orders(
    orders: &[ProtoOaOrder],
    symbols: &SymbolMap,
    legs: Option<&PositionsById>,
) -> Vec<OrderStatusReport> {
    let mut reports: Vec<OrderStatusReport> = Vec::with_capacity(orders.len());
    // exit order client id -> the row its first resting leg made
    let mut exit_rows: HashMap<String, usize> = HashMap::new();
    for o in orders {
        let named = o.client_order_id.as_deref().filter(|c| !c.is_empty());
        let exit = named.or(o.trade_data.label.as_deref()).and_then(|id| exit_of_leg(id, legs));
        let order_client_id = match (exit, named) {
            (Some(exit), _) => Some(exit.to_string()),
            (None, Some(order_client_id)) => Some(order_client_id.to_string()),
            (None, None) => {
                o.trade_data.label.as_deref().and_then(|label| label_order_client_id(label, legs))
            }
        };
        let report = order_report(o, symbols, order_client_id);
        if let Some(exit) = exit {
            match exit_rows.get(exit) {
                Some(&row) if reports[row].symbol == report.symbol => {
                    merge_leg(&mut reports[row], &report);
                    continue;
                }
                Some(_) => {}
                None => {
                    exit_rows.insert(exit.to_string(), reports.len());
                }
            }
        }
        reports.push(report);
    }
    reports
}

/// One `ProtoOAReconcileRes.order` row as a report under `order_client_id` ([`parse_orders`]).
fn order_report(
    o: &ProtoOaOrder,
    symbols: &SymbolMap,
    order_client_id: Option<String>,
) -> OrderStatusReport {
    let order_type = order_type_of(o.order_type);
    // The terms `feed::decode::order_to_new_order` sends, read back: a LIMIT's `limitPrice`, a
    // STOP's `stopPrice`, a STOP_LIMIT's `stopPrice` and the limit its `slippageInPoints` rebuilds.
    let positive = |p: Option<f64>| p.filter(|v| v.is_finite() && *v > 0.0);
    let (price, trigger_price) = match order_type {
        OrderType::Limit => (positive(o.limit_price), None),
        OrderType::Stop => (None, positive(o.stop_price)),
        OrderType::StopLimit => (positive(stop_limit_price(o, symbols)), positive(o.stop_price)),
        _ => (None, None),
    };
    OrderStatusReport {
        venue: VENUE.to_string(),
        symbol: symbols.name_of(o.trade_data.symbol_id).unwrap_or_default().to_string(),
        venue_order_id: o.order_id.to_string().into(),
        order_client_id,
        side: side_sign(o.trade_data.trade_side),
        order_type,
        qty: o.trade_data.volume as f64 / 100.0,
        filled_qty: o.executed_volume.unwrap_or(0) as f64 / 100.0,
        average_price: o.execution_price.unwrap_or(0.0),
        status: order_status_of(o),
        timestamp: o.utc_last_update_timestamp.unwrap_or(0),
        price,
        trigger_price,
        trail: None,
        trail_percent: None,
    }
}

/// **One symbol's open positions in the shape the engine's book keys** (module doc, "Position report
/// keys"): the `ProtoOAReconcileRes.position` rows of `symbol_id` that carry exposure, read by the ONE
/// reading the exec book is seeded with (`crate::execution::positions`' `open_rows`: OPEN with a positive volume),
/// each a [`PositionLeg`], netted by the shared [`PositionStatusReport::from_legs`]. A leg's size is
/// `tradeData.volume` (UNSIGNED, in cents) signed by `tradeData.tradeSide` (`SELL` -> negative, the
/// unsigned-magnitude-plus-side convention Bybit's/Alpaca's parsers use), its price the position's VWAP
/// entry price (a real double, no rescale), its stamp the last update.
///
/// A symbol that holds at most one direction comes back as ONE `Both` row, `qty` the signed sum, `avg_px`
/// the volume-weighted entry price (`0.0`, "no usable price", when any leg lacks one: `resolve` then
/// refuses to book at it), `ts` the newest leg's. A symbol that holds BOTH directions (a locked or partly
/// hedged pair, which one net number would hide) comes back as one `Long` and one `Short` row, each summed
/// the same way: the book has no such key, so each stays a held `PositionOnlyExternal`, and the rows still
/// sum to the net, which is what the core reads as the venue's net position. A CLOSED row, or the empty
/// CREATED position a pending order makes, carries no exposure; no exposure left is the flat row cTrader's
/// omission of a closed position calls for.
///
/// `Err` when a row of the symbol is one this build cannot classify (an ERROR position, a status the
/// vendored proto has no word for, an OPEN row with no volume): read as an absence it would report the
/// symbol flatter than the venue holds it, the same hole that keeps the exec book from calling itself
/// evidence, so the pass claims nothing about the symbol (a failed fetch skips the pass, as `SETTLING` does).
pub fn position_reports(
    positions: &[ProtoOaPosition],
    symbol_id: i64,
    symbol: &str,
) -> Result<Vec<PositionStatusReport>, String> {
    let (open, unreadable) =
        open_rows(positions.iter().filter(|p| p.trade_data.symbol_id == symbol_id));
    if unreadable > 0 {
        return Err(format!(
            "ctrader recon: {unreadable} position row(s) of {symbol} carry a status this build cannot \
             classify (not an OPEN position with a volume, nor a CLOSED or CREATED one), so its position \
             cannot be claimed read"
        ));
    }
    let legs: Vec<PositionLeg> = open
        .iter()
        .map(|p| {
            let units = p.trade_data.volume as f64 / 100.0;
            PositionLeg {
                size: if side_sign(p.trade_data.trade_side) < 0 { -units } else { units },
                price: p.price.unwrap_or(0.0),
                timestamp_ms: p.utc_last_update_timestamp.unwrap_or(0),
            }
        })
        .collect();
    // cTrader's FX/CFD symbols are linear: the size-weighted average, through the one averaging law.
    Ok(PositionStatusReport::from_legs(VENUE, symbol, &legs, ContractValue::linear(1.0)))
}

/// One RAW open position from a reconcile, INCLUDING the numeric `position_id` that
/// [`position_reports`]' `PositionStatusReport` output drops (it has no position-id field). This is
/// the input [`crate::execution::ExecutionClientCtrader::close_all`] needs to issue a `ProtoOAClosePositionReq` per
/// position — the flatten path for PRE-EXISTING positions this session never tracked (a fresh
/// connect's exec position-map is empty, so a reduce ORDER would OPEN a hedge against it instead of
/// closing). `side` is `+1` long / `-1` short; `volume` is centi-units (the wire scale).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawPosition {
    pub position_id: i64,
    pub symbol: String,
    pub side: i32,
    pub volume: i64,
}

/// `ProtoOAReconcileRes.position` rows -> [`RawPosition`], resolving each row's `symbolId` to its
/// vike name and — unlike [`position_reports`] — KEEPING the `position_id`. Only the rows the exec book
/// tracks are kept (`crate::execution::positions`' `reconcile_rows`, the ONE reading of these rows): an OPEN
/// position with a positive volume. A CLOSED row, the empty CREATED position a pending order makes, and a
/// row this build cannot classify are no flatten target, exactly as the exec book never routes a close
/// to them. Pure over already-decoded prost types, like every other `parse_*` here.
pub fn parse_raw_positions(positions: &[ProtoOaPosition], symbols: &SymbolMap) -> Vec<RawPosition> {
    reconcile_rows(positions)
        .0
        .into_iter()
        .map(|(position_id, p)| RawPosition {
            position_id,
            symbol: symbols.name_of(p.symbol_id).unwrap_or_default().to_string(),
            side: p.side,
            volume: p.volume,
        })
        .collect()
}

/// **One open position's UNREALIZED PnL as the venue states it**, in the account's deposit currency
/// (`ProtoOAGetPositionUnrealizedPnLRes`, descaled by the RESPONSE's own `moneyDigits`, a required field: no
/// default is assumed). READ-ONLY and for VERIFICATION only: the multi-currency demo drill holds a larger
/// position (10,000 units) and sets the venue's figure beside the engine's, because a 1,000-unit round trip
/// books whole cents and cannot tell orientation, units and rate effects apart. Nothing in a reconcile pass or
/// the engine reads it.
#[derive(Debug, Clone, PartialEq)]
pub struct UnrealizedPnl {
    pub position_id: i64,
    /// The broker's name for the position's symbol, `""` when the position is not in the reconcile reply.
    pub symbol: String,
    /// `+1` long, `-1` short, `0` when the position is not in the reconcile reply.
    pub side: i32,
    /// The position's size in base units (the wire's centi-units divided by 100), `0` when unknown.
    pub units: f64,
    /// `grossUnrealizedPnL` descaled: "denoted in the account deposit currency".
    pub gross: f64,
    /// `netUnrealizedPnL` descaled: "does not include potential closing commission".
    pub net: f64,
    /// The exponent the integers were descaled by.
    pub money_digits: u32,
}

impl UnrealizedPnl {
    /// One greppable line, the format the demo drill reads (`key=value` pairs, no spaces inside a value).
    #[must_use]
    pub fn line(&self) -> String {
        format!(
            "CTPNL position={} sym={} side={} units={} gross={} net={} digits={}",
            self.position_id,
            if self.symbol.is_empty() { "?" } else { &self.symbol },
            self.side,
            self.units,
            self.gross,
            self.net,
            self.money_digits,
        )
    }
}

/// `ProtoOAGetPositionUnrealizedPnLRes` rows -> [`UnrealizedPnl`], each joined by position id to the open
/// positions `open` of the same moment (symbol, side, size). Pure over already-decoded types.
pub fn unrealized_pnls_of(
    res: &ProtoOaGetPositionUnrealizedPnLRes,
    open: &[RawPosition],
) -> Vec<UnrealizedPnl> {
    let scale = 10f64.powi(i32::try_from(res.money_digits).unwrap_or(i32::MAX));
    res.position_unrealized_pn_l
        .iter()
        .map(|row| {
            let held = open.iter().find(|p| p.position_id == row.position_id);
            UnrealizedPnl {
                position_id: row.position_id,
                symbol: held.map(|p| p.symbol.clone()).unwrap_or_default(),
                side: held.map_or(0, |p| p.side),
                units: held.map_or(0.0, |p| p.volume as f64 / 100.0),
                gross: row.gross_unrealized_pn_l as f64 / scale,
                net: row.net_unrealized_pn_l as f64 / scale,
                money_digits: res.money_digits,
            }
        })
        .collect()
}

/// `ProtoOADealListRes.deal` rows -> `FillReport`, filtered to `symbol_id` (the request carries no
/// per-symbol filter) and to executed deals only (`FILLED`/`PARTIALLY_FILLED` — a `REJECTED`/
/// `INTERNALLY_REJECTED`/`ERROR` deal never actually executed volume). Commission descales by
/// `moneyDigits` (preferring the deal's own, falling back to the account-level `money_digits`
/// passed in) and is NEGATED — cTrader's wire commission is negative for a charged fee, the
/// opposite of this report's `> 0 == cost` convention — the SAME descale+negate
/// `feed::decode::deal_fill` already applies to the live fill stream, with the same venue conversion fee
/// added on a foreign-quoted close. `order_client_id` is the deal's entry in `order_client_ids`
/// ([`deal_order_client_ids`]), `None` without one; `commission_asset` is always `""` (the deal itself carries no
/// currency field) and `liquidity_side` is always `Unknown` (no maker/taker flag on the wire).
pub fn parse_fills(
    deals: &[ProtoOaDeal],
    order_client_ids: &HashMap<i64, String>,
    symbol_id: i64,
    symbol: &str,
    money_digits: u32,
) -> Vec<FillReport> {
    deals
        .iter()
        .filter(|d| d.symbol_id == symbol_id)
        .filter(|d| {
            matches!(
                ProtoOaDealStatus::try_from(d.deal_status),
                Ok(ProtoOaDealStatus::Filled) | Ok(ProtoOaDealStatus::PartiallyFilled)
            )
        })
        .map(|d| {
            let digits = d.money_digits.unwrap_or(money_digits) as i32;
            // Plus the venue's conversion fee on a foreign-quoted close, as the live fill carries it.
            let commission = -(d.commission.unwrap_or(0) as f64) / 10f64.powi(digits)
                + crate::execution::close_detail::conversion_fee_of(d, money_digits);
            FillReport {
                venue: VENUE.to_string(),
                symbol: symbol.to_string(),
                // Integer wire field — cannot render empty (see `crate::feed::decode`'s
                // `deal_to_fill` for the same reasoning and the prost `0`-default residual).
                trade_id: TradeId::new(d.deal_id.to_string())
                    .expect("a protobuf i64 deal_id always renders at least one digit"),
                venue_order_id: d.order_id.to_string().into(),
                order_client_id: order_client_ids.get(&d.deal_id).cloned(),
                side: side_sign(d.trade_side),
                last_qty: d.filled_volume as f64 / 100.0,
                last_price: d.execution_price.unwrap_or(0.0),
                commission,
                commission_asset: String::new(),
                liquidity_side: LiquiditySide::Unknown,
                timestamp: d.execution_timestamp,
            }
        })
        .collect()
}

/// The two fields of `ProtoOADeal` [`deal_order_client_ids`] reads, at their wire tags (`dealId` 1, `label` 18).
/// shortcut: a hand overlay of a field the vendored proto predates (module doc); when `proto/` is re-vendored at
/// Release 100 or later, read the generated `ProtoOaDeal::label` and delete these two types.
#[derive(Clone, PartialEq, prost::Message)]
struct LabelledDeal {
    #[prost(int64, required, tag = "1")]
    deal_id: i64,
    #[prost(string, optional, tag = "18")]
    label: Option<String>,
}

/// `ProtoOADealListRes` with only its deals (`deal` 3), each read as a [`LabelledDeal`].
#[derive(Clone, PartialEq, prost::Message)]
struct LabelledDealList {
    #[prost(message, repeated, tag = "3")]
    deal: Vec<LabelledDeal>,
}

/// **deal id -> the order client id its order was placed under**, from a `ProtoOADealListRes` payload: the deal's `label` read
/// by `label_order_client_id` (a leg of a split exit reports that exit; any other label must read as one of our v2 ids, or be a
/// order client id `legs` knows). A deal with no label, an empty one or a foreign one has no entry (its fill is `None`, foreign).
/// Bytes the overlay cannot read give no entries, never an error: the generated decode of the same bytes is what the
/// pass stands on.
pub fn deal_order_client_ids(payload: &[u8], legs: Option<&PositionsById>) -> HashMap<i64, String> {
    let deals = LabelledDealList::decode(payload).map(|l| l.deal).unwrap_or_default();
    deals
        .into_iter()
        .filter_map(|d| Some((d.deal_id, label_order_client_id(d.label.as_deref()?, legs)?)))
        .collect()
}

/// **The account's cash, as the engine's ledger may hold it**: `ProtoOATrader.balance` descaled by the
/// trader's OWN `moneyDigits` (an absent one is refused in words, never read as a scale of 0:
/// `connection::handshake::require_money_digits`, the handshake's own refusal), when the account's deposit
/// currency is USD.
///
/// ⚠ **`deposit` is the whole fence, and it fails CLOSED.** A reconcile balance is a bare number that
/// carries no currency (`ReconcileClient::fetch_balance`), and the core adopts it into a ledger it assumes
/// is in the account's money: 1,000,000 JPY adopted as 1,000,000 dollars would let the buying-power gate
/// admit a hundred and fifty times the account. So a deposit that is not USD, or that the broker's asset
/// list does not name, is an `Err` the caller reports (`balance fetch failed`, the seed stays), never an
/// `Ok(None)` that reads like a venue with no balance. shortcut: USD only, because that is the one
/// currency the engine's ledger is guaranteed to share without the node having anchored it; lift it
/// when the report carries its currency.
pub fn parse_balance(trader: &ProtoOaTrader, deposit: Option<&str>) -> Result<f64, String> {
    match deposit {
        Some(currency) if currency.trim().eq_ignore_ascii_case("USD") => {}
        Some(currency) => {
            return Err(format!(
                "ctrader recon: the account's deposit currency is {currency}, and a reconcile balance \
                 carries no currency, so it is not adopted into the engine's USD ledger"
            ));
        }
        None => {
            return Err("ctrader recon: the account's deposit currency is unknown (the broker's \
                        asset list does not name it), so its balance is not adopted"
                .to_string());
        }
    }
    let digits = connection::handshake::require_money_digits(trader.money_digits)
        .map_err(|e| e.to_string())?;
    Ok(trader.balance as f64 / 10f64.powi(digits as i32))
}

/// The admitted `name` (upper-cased by [`ReconcileAdmittedSymbols`]) as the broker spells it, with its id: the ONE
/// symbol of `symbols` whose name matches without case. `Err` when none does (the broker dropped it from
/// the list) or several do (two names differing only by case: the report could not say which was meant),
/// because a pass that quietly skipped it would claim a symbol it never read.
fn resolve_admitted(symbols: &SymbolMap, name: &str) -> Result<(String, i64), String> {
    let mut hits = symbols.names().filter(|n| n.eq_ignore_ascii_case(name));
    match (hits.next(), hits.next()) {
        (Some(hit), None) => symbols
            .id_of(hit)
            .map(|id| (hit.to_string(), id))
            .ok_or_else(|| format!("ctrader recon: admitted symbol {name} has no id")),
        (None, _) => Err(format!(
            "ctrader recon: admitted symbol {name} is not in the account's symbol list, so its \
             orders, fills and position cannot be read"
        )),
        (Some(_), Some(_)) => Err(format!(
            "ctrader recon: admitted symbol {name} matches more than one name of the account's \
             symbol list (they differ only by case), so which one to read is not known"
        )),
    }
}

// --- the client ----------------------------------------------------------------------------

/// Re-handshake the dedicated connection once it has been idle this long. MUST stay below
/// cTrader's server-side idle-disconnect tolerance; [`connection::HEARTBEAT_INTERVAL`] (10s) is the
/// cadence the exec actor keeps its own socket alive at, so reviving at the same age is
/// conservative by construction — this connection is re-opened no later than the point the actor
/// would have had to send a keepalive to hold it.
const IDLE_REVIVE_AFTER: Duration = connection::HEARTBEAT_INTERVAL;

/// The most deal-list requests one fill pass makes when the list says `has_more` and a symbol is admitted.
const DEAL_PAGE_ROUNDS: usize = 10;

const DEAL_LIST_TRUNCATED: &str = "ctrader recon: the deal list has more rows than one reply carries \
    (`has_more`) and paging did not reach its end, so the fills of every admitted symbol cannot be claimed read";

/// What a list fetch answers while the exec's book says a dead socket's reconnect is settling
/// (`PositionsById::is_settling`): the venue already did what the book has not heard yet, and the settle is about to
/// book it. A fetch error, so the manager skips the pass; the next one reads.
pub const SETTLING: &str = "ctrader recon: the exec socket died and its reconnect has not yet read the venue's \
    lists for what it left unanswered, so this pass would set the venue against a book that has not heard it; it \
    waits for the next pass";

/// The window after a TRUNCATED page of deals, or `None` when the page cannot move it (empty). The reply's
/// order is unmeasured, so the direction is read off the page: oldest first (the page ends at its newest
/// row) the next window STARTS at that row; newest first the next window ENDS at the oldest row. The
/// boundary instant is in both windows, so the caller drops a deal it already holds by id.
fn next_deal_window(
    page: &[ProtoOaDeal],
    (from, to): (Option<i64>, Option<i64>),
) -> Option<(Option<i64>, Option<i64>)> {
    let (first, last) = (page.first()?.execution_timestamp, page.last()?.execution_timestamp);
    let stamps = page.iter().map(|d| d.execution_timestamp);
    if first <= last { Some((stamps.max(), to)) } else { Some((from, stamps.min())) }
}

/// The live connection + everything the handshake resolved with it. Grouped under ONE mutex (not
/// a bare `Mutex<Reader>`) because an idle revive replaces the socket AND re-resolves the symbol
/// map/ctid/moneyDigits together — they must never be observed from different handshakes.
struct ReconcileConnection {
    reader: Reader,
    ctid: i64,
    symbols: SymbolMap,
    money_digits: u32,
    symbol_id: i64,
    /// When the last successful exchange completed — the idle clock [`IDLE_REVIVE_AFTER`] reads.
    last_used: Instant,
}

/// One reconcile client per (account, venue-symbol), holding its OWN dedicated authed connection
/// — see the module doc for why it doesn't share the exec/data actor's socket. The mutex is
/// because `ReconcileClient`'s methods take `&self` but reading/writing the socket needs `&mut`.
///
/// **Idle handling (the MUST-FIX-BEFORE-MOUNT the module doc flagged):** this socket sends no
/// heartbeats and sits idle between reconcile passes, and cTrader disconnects idle sockets — so at
/// the default 60s `ReconcileDriver` cadence it would be server-closed before the second pass, failing
/// every later fetch. Resolved by REVIVING the connection lazily rather than keeping it warm: a
/// fetch that finds the connection older than [`IDLE_REVIVE_AFTER`] re-runs the handshake first.
///
/// Chosen over a heartbeat thread deliberately. A keepalive writer would have to share this same
/// mutex with an in-flight `call`, whose `read_until` can legitimately block for the full
/// per-response wait (the exec lane's `response_timeout` in
/// `vike_venues::venue_timings::CTRADER`, 15s) — longer than the 10s cadence it would need
/// to keep, so it could not honor its own interval without interleaving a write into a half-read
/// exchange. Lazy revive needs no thread, no shared clock, and nothing to stop on drop. It also
/// costs no more than a per-pass reconnect would: the three `fetch_*` calls of one pass run
/// microseconds apart, so only the FIRST revives and its siblings reuse that fresh connection.
pub struct ReconcileClientCtrader {
    state: Mutex<ReconcileConnection>,
    /// The connection config every revive handshakes with. A `Mutex` because a revive FOLLOWS the
    /// process's shared grant ([`ConnectionConfig::sync_grant`]) and so rewrites the pair it carries —
    /// the client used to keep the copy it was built with, which a sibling's rotation made stale
    /// for good (every later pass failed its handshake until the daemon restarted). Locked only by
    /// a revive, under `state` (that order), for the length of its handshake; never during a request.
    ///
    /// ⚠ **This client never SPENDS the refresh token**: it only adopts what the exec/data actors
    /// rotated. A pass that finds the grant lapsed with no sibling having renewed it fails and is
    /// reported, exactly as before.
    config: Mutex<ConnectionConfig>,
    symbol: String,
    idle_revive_after: Duration,
    /// The symbols the account's engine admitted on demand: every pass reads them beside the mounted
    /// one (the module doc).
    admitted: ReconcileAdmittedSymbols,
    /// The exec's position book, whose split-exit table maps a leg's fill to its exit and whose known order client ids make a
    /// label that is no v2 id ours ([`deal_order_client_ids`]); `None` for a client built without an exec (the credential probe,
    /// a smoke).
    legs: Option<PositionsByIdShared>,
}

impl ReconcileClientCtrader {
    /// Open a DEDICATED authed connection for reconcile reads: the SAME handshake
    /// [`connection::open_and_handshake`] runs for exec/data (`config.account_id`, when set, skips account
    /// discovery — the same "never re-discover, always pass the resolved ctid" convention
    /// reconnect uses). `None` on handshake failure OR when `symbol` isn't in the discovered
    /// symbol list — reconcile stays unwired for this venue, exec unaffected (the same graceful
    /// degradation every other absent-recon path uses).
    pub fn connect(config: &ConnectionConfig, symbol: &str) -> Option<Self> {
        Self::connect_with_idle_revive(config, symbol, IDLE_REVIVE_AFTER)
    }

    /// [`Self::connect`] with an explicit idle-revive age. Exposed so a test can force the revive
    /// path deterministically (`Duration::ZERO` revives on every call) without sleeping out a real
    /// idle interval, and so an operator can tighten the age for a venue that disconnects sooner.
    pub fn connect_with_idle_revive(
        config: &ConnectionConfig,
        symbol: &str,
        idle_revive_after: Duration,
    ) -> Option<Self> {
        // The pair a sibling socket of this process holds NOW, not the snapshot `config` was built from.
        let mut config = config.clone();
        config.sync_grant();
        let (reader, ctid, symbols, money_digits) =
            connection::open_and_handshake(&config, config.account_id).ok()?;
        let symbol_id = symbols.id_of(symbol)?;
        Some(ReconcileClientCtrader {
            state: Mutex::new(ReconcileConnection {
                reader,
                ctid,
                symbols,
                money_digits,
                symbol_id,
                last_used: Instant::now(),
            }),
            config: Mutex::new(config),
            symbol: symbol.to_string(),
            idle_revive_after,
            admitted: ReconcileAdmittedSymbols::default(),
            legs: None,
        })
    }

    /// The `(name, symbol id)` pairs a pass reads on THIS handshake's symbol list: the mounted symbol
    /// first, then each admitted one the list resolves (module doc). A name already in the set under
    /// another case is read once.
    fn read_set(&self, st: &ReconcileConnection) -> Result<Vec<(String, i64)>, String> {
        let mut set = vec![(self.symbol.clone(), st.symbol_id)];
        for name in self.admitted.symbols() {
            let (resolved, id) = resolve_admitted(&st.symbols, &name)?;
            if !set.iter().any(|(_, held)| *held == id) {
                set.push((resolved, id));
            }
        }
        Ok(set)
    }

    /// Re-handshake `st` in place when it has gone idle past [`Self::idle_revive_after`]; a
    /// still-fresh connection is left untouched. The resolved `ctid` is passed back as the FIXED
    /// account id so a revive can never re-discover and silently authorize a DIFFERENT account —
    /// the same invariant `open_and_handshake`'s own doc states for reconnect.
    fn revive_if_idle(&self, st: &mut ReconcileConnection) -> Result<(), String> {
        if st.last_used.elapsed() < self.idle_revive_after {
            return Ok(());
        }
        let (reader, ctid, symbols, money_digits) = {
            let mut config =
                self.config.lock().map_err(|_| "ctrader recon config poisoned".to_string())?;
            // Follow the grant: a sibling's rotation since the last pass must not leave this
            // handshake presenting the access token the venue already ended.
            config.sync_grant();
            connection::open_and_handshake(&config, Some(st.ctid)).map_err(|e| e.to_string())?
        };
        let symbol_id = symbols
            .id_of(&self.symbol)
            .ok_or_else(|| format!("ctrader recon: symbol {} absent after revive", self.symbol))?;
        *st = ReconcileConnection {
            reader,
            ctid,
            symbols,
            money_digits,
            symbol_id,
            last_used: Instant::now(),
        };
        Ok(())
    }

    /// Blocking request/response over the dedicated connection: revive it if it has gone idle,
    /// then encode+write `req` as `req_type` and read frames until `res_type` (or an
    /// `ERROR_RES`/timeout) arrives — the SAME `send`/`read_until` primitives
    /// `connection::reconcile_after_reconnect` uses, reused rather than reinvented.
    fn call<SeenRequest: Message, Res: Message + Default>(
        &self,
        st: &mut ReconcileConnection,
        req_type: u32,
        req: &SeenRequest,
        res_type: u32,
    ) -> Result<Res, String> {
        let msg = self.exchange(st, req_type, req, res_type)?;
        connection::decode_payload(&msg).map_err(|e| e.to_string())
    }

    /// [`Self::call`]'s round trip with the reply left undecoded: [`Self::deal_page`] reads its bytes twice.
    fn exchange<SeenRequest: Message>(
        &self,
        st: &mut ReconcileConnection,
        req_type: u32,
        req: &SeenRequest,
        res_type: u32,
    ) -> Result<ProtoMessage, String> {
        self.revive_if_idle(st)?;
        connection::send(&mut st.reader, req_type, req, "").map_err(|e| e.to_string())?;
        let msg = connection::read_until(&mut st.reader, res_type).map_err(|e| e.to_string())?;
        // Only a COMPLETED exchange refreshes the idle clock: a failed one leaves the connection
        // suspect, so the next fetch revives rather than retrying down the same dead socket.
        st.last_used = Instant::now();
        Ok(msg)
    }

    /// Lock the connection for one fetch. Separated from [`Self::call`] so a fetch that needs both
    /// a response AND the handshake-resolved fields (`symbols`, `money_digits`, `symbol_id`) reads
    /// them from the SAME guard the call ran under — after a revive they are freshly resolved.
    fn locked(&self) -> Result<std::sync::MutexGuard<'_, ReconcileConnection>, String> {
        self.state.lock().map_err(|_| "ctrader recon connection poisoned".to_string())
    }

    /// One `ProtoOADealListReq` over the window `(from, to)` (epoch ms, either bound optional), with the order client id of each
    /// of its deals that names one ([`deal_order_client_ids`]).
    fn deal_page(
        &self,
        st: &mut ReconcileConnection,
        (from_timestamp, to_timestamp): (Option<i64>, Option<i64>),
    ) -> Result<(ProtoOaDealListRes, HashMap<i64, String>), String> {
        let req = ProtoOaDealListReq {
            ctid_trader_account_id: st.ctid,
            from_timestamp,
            to_timestamp,
            ..Default::default()
        };
        let msg = self.exchange(st, pt::DEAL_LIST_REQ, &req, pt::DEAL_LIST_RES)?;
        let page = connection::decode_payload(&msg).map_err(|e| e.to_string())?;
        let book = self.legs.as_ref().and_then(|legs| legs.lock().ok());
        Ok((page, deal_order_client_ids(msg.payload.as_deref().unwrap_or(&[]), book.as_deref())))
    }

    /// [`SETTLING`] while the exec's book says a dead socket's reconnect has not handed on the venue's lists yet.
    fn not_settling(&self) -> Result<(), String> {
        let book = self.legs.as_ref().and_then(|legs| legs.lock().ok());
        if book.is_some_and(|b| b.is_settling()) { Err(SETTLING.to_string()) } else { Ok(()) }
    }

    /// **Each CLOSING deal of a read symbol is named by its own order's `clientOrderId`** (by `label_order_client_id`'s rule), or by
    /// none when the order list does not hold its order (a `hasMore` page): never by its label, which a close by
    /// position id inherits from the position it closes (module doc). One `ProtoOAOrderListReq` around those deals'
    /// stamps; none when the window has no closing deal, so such a pass reads exactly what it read before.
    /// shortcut: one page of the order list, as `connection::reconcile`'s settle reads it; page it if a name goes missing.
    fn name_closing_deals(
        &self,
        st: &mut ReconcileConnection,
        deals: &[ProtoOaDeal],
        read: &[(String, i64)],
        order_client_ids: &mut HashMap<i64, String>,
    ) -> Result<(), String> {
        let closing: Vec<&ProtoOaDeal> = deals
            .iter()
            .filter(|d| d.close_position_detail.is_some())
            .filter(|d| read.iter().any(|(_, id)| *id == d.symbol_id))
            .collect();
        let stamps = closing.iter().map(|d| d.execution_timestamp);
        let (Some(from), Some(to)) = (stamps.clone().min(), stamps.max()) else { return Ok(()) };
        let req = ProtoOaOrderListReq {
            ctid_trader_account_id: st.ctid,
            from_timestamp: Some((from - connection::reconcile::LIST_WINDOW_MARGIN_MS).max(0)),
            to_timestamp: Some(to + connection::reconcile::LIST_WINDOW_MARGIN_MS),
            ..Default::default()
        };
        let listed: ProtoOaOrderListRes =
            self.call(st, pt::ORDER_LIST_REQ, &req, pt::ORDER_LIST_RES)?;
        let book = self.legs.as_ref().and_then(|legs| legs.lock().ok());
        for d in closing {
            let own = listed
                .order
                .iter()
                .find(|o| o.order_id == d.order_id)
                .and_then(|o| o.client_order_id.as_deref())
                .filter(|id| !id.is_empty())
                .and_then(|id| label_order_client_id(id, book.as_deref()));
            match own {
                Some(order_client_id) => order_client_ids.insert(d.deal_id, order_client_id),
                None => order_client_ids.remove(&d.deal_id),
            };
        }
        Ok(())
    }

    fn reconcile(&self, st: &mut ReconcileConnection) -> Result<ProtoOaReconcileRes, String> {
        let req = ProtoOaReconcileReq { ctid_trader_account_id: st.ctid, ..Default::default() };
        self.call(st, pt::RECONCILE_REQ, &req, pt::RECONCILE_RES)
    }

    /// The account's currently-OPEN positions for this client's engine symbol as RAW legs — each
    /// carrying the numeric `position_id` that [`ReconcileClient::fetch_position_status_reports`] drops.
    /// The input an operator flatten ([`crate::execution::ExecutionClientCtrader::close_all`]) needs to close every
    /// PRE-EXISTING position by id (a reduce ORDER can't reach them — the exec position-map only
    /// tracks this session's own fills). Reconciles over the dedicated socket (idle-revived like
    /// every other fetch), filtered to `self.symbol`; `Ok(vec![])` when flat in this symbol.
    pub fn open_positions(&self) -> Result<Vec<RawPosition>, String> {
        let mut st = self.locked()?;
        let res = self.reconcile(&mut st)?;
        Ok(parse_raw_positions(&res.position, &st.symbols)
            .into_iter()
            .filter(|p| p.symbol == self.symbol)
            .collect())
    }

    /// **Every pending order of the account, on every symbol** (an operator's read, `crate::reconcile::venue_read`): the ONE
    /// `ProtoOAReconcileReq` a pass makes, through [`parse_orders`], WITHOUT the pass's filter to the symbols it reads.
    pub fn all_open_orders(&self) -> Result<Vec<OrderStatusReport>, String> {
        let mut st = self.locked()?;
        let res = self.reconcile(&mut st)?;
        let book = self.legs.as_ref().and_then(|legs| legs.lock().ok());
        Ok(parse_orders(&res.order, &st.symbols, book.as_deref()))
    }

    /// **What the venue booked on every CLOSING deal since `since`** (epoch ms), ACCOUNT-wide and oldest
    /// first: the `ProtoOAClosePositionDetail` of each (`crate::execution::close_detail`). READ-ONLY and for
    /// VERIFICATION only (the multi-currency demo drill sets the engine's realized PnL beside it): nothing
    /// in a reconcile pass reads it. One `ProtoOADealListReq`; a reply that says `has_more` is an error,
    /// never a quietly short list.
    pub fn close_details(
        &self,
        since: i64,
    ) -> Result<Vec<crate::execution::close_detail::CloseDetail>, String> {
        let mut st = self.locked()?;
        let (page, _) = self.deal_page(&mut st, (Some(since).filter(|s| *s > 0), None))?;
        if page.has_more {
            return Err("ctrader recon: the deal list has more rows than one reply carries \
                        (`has_more`); narrow the window"
                .to_string());
        }
        Ok(crate::execution::close_detail::close_details_of(
            &page.deal,
            &st.symbols,
            st.money_digits,
        ))
    }

    /// **The venue's own unrealized PnL of every open position** (`ProtoOAGetPositionUnrealizedPnLReq`),
    /// ACCOUNT-wide, each joined to its symbol, side and size by a reconcile read of the same moment.
    /// READ-ONLY and for VERIFICATION only ([`UnrealizedPnl`]); nothing in a reconcile pass reads it.
    pub fn unrealized_pnls(&self) -> Result<Vec<UnrealizedPnl>, String> {
        let mut st = self.locked()?;
        let req = ProtoOaGetPositionUnrealizedPnLReq {
            ctid_trader_account_id: st.ctid,
            ..Default::default()
        };
        let res: ProtoOaGetPositionUnrealizedPnLRes = self.call(
            &mut st,
            pt::GET_POSITION_UNREALIZED_PNL_REQ,
            &req,
            pt::GET_POSITION_UNREALIZED_PNL_RES,
        )?;
        let open = self.reconcile(&mut st)?;
        Ok(unrealized_pnls_of(&res, &parse_raw_positions(&open.position, &st.symbols)))
    }
}

impl ReconcileClient for ReconcileClientCtrader {
    /// `_since` is a no-op here: `ProtoOAReconcileRes` reports only the CURRENTLY pending set
    /// (mirrors Bybit's `order/realtime`) — a fully-closed order that fell out of this set is
    /// what the fill/position reports (not this one) reconcile.
    fn fetch_order_status_reports(&self, _since: i64) -> Result<Vec<OrderStatusReport>, String> {
        self.not_settling()?;
        let mut st = self.locked()?;
        let res = self.reconcile(&mut st)?;
        let set = self.read_set(&st)?;
        let book = self.legs.as_ref().and_then(|legs| legs.lock().ok());
        Ok(parse_orders(&res.order, &st.symbols, book.as_deref())
            .into_iter()
            .filter(|o| set.iter().any(|(name, _)| *name == o.symbol))
            .collect())
    }

    fn fetch_fill_reports(&self, since: i64) -> Result<Vec<FillReport>, String> {
        self.not_settling()?;
        let mut st = self.locked()?;
        let mut window = (Some(since).filter(|s| *s > 0), None);
        let (mut page, mut order_client_ids) = self.deal_page(&mut st, window)?;
        let set = self.read_set(&st)?;
        // The deal list is account-wide and capped: with an admitted symbol in play a reply that says it
        // has more rows than it carries may be missing that symbol's fills, so the list is PAGED (the next
        // window starts at the last page's edge, at most `DEAL_PAGE_ROUNDS` requests) and the pass fails
        // only if it is still truncated, rather than claim them (the mounted-only pass has always read the
        // first chunk, unchanged: one request, no paging).
        let mut deals: Vec<ProtoOaDeal> = Vec::new();
        let mut rounds = 0;
        loop {
            rounds += 1;
            let paging = page.has_more && set.len() > 1;
            let next = if paging { next_deal_window(&page.deal, window) } else { None };
            if deals.is_empty() {
                deals = std::mem::take(&mut page.deal);
            } else {
                // The boundary instant is in both windows: a deal already held is not read twice.
                let known = deals.len();
                let held: std::collections::HashSet<i64> =
                    deals.iter().map(|d| d.deal_id).collect();
                let fresh = std::mem::take(&mut page.deal);
                deals.extend(fresh.into_iter().filter(|d| !held.contains(&d.deal_id)));
                // A page with nothing new cannot move the window: still truncated.
                if paging && deals.len() == known {
                    return Err(DEAL_LIST_TRUNCATED.to_string());
                }
            }
            if !paging {
                break;
            }
            let Some(next) = next.filter(|_| rounds < DEAL_PAGE_ROUNDS) else {
                return Err(DEAL_LIST_TRUNCATED.to_string());
            };
            window = next;
            let (next_page, more) = self.deal_page(&mut st, window)?;
            page = next_page;
            order_client_ids.extend(more);
        }
        self.name_closing_deals(&mut st, &deals, &set, &mut order_client_ids)?;
        Ok(set
            .iter()
            .flat_map(|(name, id)| {
                parse_fills(&deals, &order_client_ids, *id, name, st.money_digits)
            })
            .collect())
    }

    fn fetch_position_status_reports(&self) -> Result<Vec<PositionStatusReport>, String> {
        self.not_settling()?;
        let mut st = self.locked()?;
        let res = self.reconcile(&mut st)?;
        let set = self.read_set(&st)?;
        // Per symbol read: its open positions as the book keys them, or the flat row cTrader's omission calls for.
        let mut reports = Vec::new();
        for (name, id) in &set {
            reports.extend(position_reports(&res.position, *id, name)?);
        }
        Ok(reports)
    }

    /// Tell this client the engine admitted `symbol` (any-symbol, decision 0173): every later pass reads
    /// its orders, fills and position too. Idempotent, never fetches; the list is bounded and upper-cased
    /// ([`ReconcileAdmittedSymbols`]), and matched against the broker's own spelling at read time.
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

    /// The account's cash from a FRESH `ProtoOATraderReq` on every call ([`parse_balance`]): the snapshot
    /// the handshake already read would be stale after the first fill, and the core's balance diff would
    /// raise a drift against it. `Err` (never `Ok(None)`) on anything the number cannot be trusted
    /// through, so a failed read keeps the engine's seed AND says so (`balance fetch failed`).
    fn fetch_balance(&self) -> Result<Option<f64>, String> {
        let mut st = self.locked()?;
        let req = ProtoOaTraderReq { ctid_trader_account_id: st.ctid, ..Default::default() };
        let res: ProtoOaTraderRes = self.call(&mut st, pt::TRADER_REQ, &req, pt::TRADER_RES)?;
        // After the call: a revive inside it re-resolved `st.symbols`, and the deposit name is read off it.
        parse_balance(&res.trader, st.symbols.deposit_currency()).map(Some)
    }

    /// **The broker's own commission on the MOUNTED symbol** (FEES-AUDIT 2026-11-08), as the handshake's
    /// `ProtoOASymbol` states it (`crate::instrument::symbols::CommissionTerms::fee_schedule`): the account's fee
    /// schedule for that symbol, in place of the static `ctrader` row, which is one measured account's
    /// guess. No request of its own: the socket is revived first when idle, so the terms are the ones a
    /// handshake read. `Ok(None)` when the symbol states no rate; `Err` in words for terms no schedule
    /// carries (the static row then stands).
    fn fetch_fee_rates(&self) -> Result<Option<vike_model::FeeSchedule>, String> {
        let mut st = self.locked()?;
        self.revive_if_idle(&mut st)?;
        let id = st.symbol_id;
        let Some(terms) = st.symbols.commission(id) else { return Ok(None) };
        let lot_units = st.symbols.volume_grid(id).and_then(|g| g.lot_units());
        terms
            .fee_schedule(lot_units, st.symbols.quote_asset(id))
            .map_err(|why| format!("ctrader {}: {why}", self.symbol))
    }
}

// --- the factory -----------------------------------------------------------------------------

/// The venue -> `ReconcileClient` factory (ReconFactory seam, wave-2 task 6): opens
/// [`ReconcileClientCtrader`]'s own dedicated authed connection from an already-resolved
/// [`crate::config::CtraderConfig`]. `None` on handshake failure — reconcile stays unwired for
/// this venue, exec unaffected (same as [`ReconcileClientCtrader::connect`]'s doc).
///
/// **WIRED into [`crate::adapter::live_runtime_for_account`]** (decision 0088 B5) — called by
/// [`crate::adapter::VenueAdapterCtrader`] — which calls this LAZILY, gated on its own
/// `recon_enabled` parameter, because this factory CONNECTS during construction: a start whose
/// reconcile gate answered NO must not pay a second protobuf/TLS handshake. The preflight
/// credential probe defers it the same way: [`crate::adapter::VenueAdapterCtrader`]'s
/// `credential_probe` calls this factory inside the probe's closure (moved there from
/// `crates/vike-tradehub-core/src/startup.rs` with the venue-adapter contract). cTrader reconciles on the
/// periodic INTERVAL only — no `reconcile_trigger`.
///
/// ⚠ **That sentence said "an unset `VIKE_RECONCILE`" until S2, and the flip is a real cost on this
/// venue rather than a wording change.** Reconciliation is now ON by default for a start that arms
/// a live venue account (`docs/decisions/0043-reconcile-is-on-by-default-under-quarantine.md`), so
/// an armed cTrader start opens this SECOND authenticated socket at every start and holds it for
/// the process lifetime — a serial, blocking handshake on the startup path, and a second consumer
/// of the account's session/connection limits. `VIKE_RECONCILE_OFF=1` is the refusal that keeps the
/// venue armed and skips it.
///
/// ⚠ This line used to say "not yet wired ... cTrader is not a live-mounted venue today", which it
/// had stopped being. A factory's header is the WRONG place to read that from, because a start arm
/// can adopt a factory without touching it — read the arm. `crates/bridges/ig/CLAUDE.md` carries
/// the family rule: every venue enrolled in the reconcile roster after its factory was written
/// inherited the same stale sentence.
///
/// `legs` is the exec's position book (`ActorHandle::positions`), so a leg's reconcile fill reports its split exit's
/// order client id ([`deal_order_client_ids`]); `None` where no exec runs beside it (the credential probe).
pub fn recon_client(
    config: &crate::config::CtraderConfig,
    symbol: &str,
    legs: Option<PositionsByIdShared>,
) -> Option<Box<dyn ReconcileClient>> {
    ReconcileClientCtrader::connect(&config.to_connection_config(), symbol)
        .map(|c| Box::new(ReconcileClientCtrader { legs, ..c }) as Box<dyn ReconcileClient>)
}

#[path = "reconcile_tests.rs"]
#[cfg(test)]
mod reconcile_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::proto::ProtoOaLightSymbol;

    fn map(names: &[&str]) -> SymbolMap {
        let light: Vec<ProtoOaLightSymbol> = names
            .iter()
            .enumerate()
            .map(|(i, n)| ProtoOaLightSymbol {
                symbol_id: i as i64 + 1,
                symbol_name: Some((*n).to_string()),
                ..Default::default()
            })
            .collect();
        SymbolMap::from_symbols(&light, &[])
    }

    /// An admitted name (upper-cased by the list) resolves to the broker's own spelling and id; one the
    /// list lacks, or two the list cannot tell apart, is an error naming the symbol - never a skip.
    #[test]
    fn an_admitted_name_resolves_to_the_brokers_spelling_or_is_an_error() {
        let symbols = map(&["EURUSD", "Wheat_H6", "GBPUSD", "gbpusd"]);
        assert_eq!(resolve_admitted(&symbols, "EURUSD"), Ok(("EURUSD".to_string(), 1)));
        assert_eq!(
            resolve_admitted(&symbols, "WHEAT_H6"),
            Ok(("Wheat_H6".to_string(), 2)),
            "the report carries the broker's case"
        );
        let missing = resolve_admitted(&symbols, "AUDUSD").expect_err("not listed");
        assert!(missing.contains("AUDUSD") && missing.contains("not in the account's symbol list"));
        let twin = resolve_admitted(&symbols, "GBPUSD").expect_err("two names differ by case");
        assert!(twin.contains("differ only by case"), "{twin}");
    }

    /// DOCS-SHAPED: a 10,000-unit long USDJPY position (1,000,000 centi-units) whose venue-stated unrealized PnL
    /// is 66.62 dollars gross and 66.00 net on an account whose RESPONSE states `moneyDigits` 2 (`6662` on the
    /// wire): the rows are joined to the open position by id, descaled by the response's own exponent, and a
    /// row whose position the reconcile read lacks keeps its figures with no symbol.
    #[test]
    fn the_venues_unrealized_pnl_is_joined_to_its_position_and_descaled_by_the_responses_digits() {
        use crate::transport::proto::ProtoOaPositionUnrealizedPnL;
        let row = |position_id, gross, net| ProtoOaPositionUnrealizedPnL {
            position_id,
            gross_unrealized_pn_l: gross,
            net_unrealized_pn_l: net,
        };
        let res = ProtoOaGetPositionUnrealizedPnLRes {
            ctid_trader_account_id: 99,
            position_unrealized_pn_l: vec![row(7, 6662, 6600), row(8, -150, -190)],
            money_digits: 2,
            ..Default::default()
        };
        let open = [RawPosition {
            position_id: 7,
            symbol: "USDJPY".to_string(),
            side: 1,
            volume: 1_000_000,
        }];
        let got = unrealized_pnls_of(&res, &open);
        assert_eq!(got.len(), 2);
        assert_eq!(
            (got[0].symbol.as_str(), got[0].side, got[0].units, got[0].gross, got[0].net),
            ("USDJPY", 1, 10_000.0, 66.62, 66.0)
        );
        assert_eq!((got[1].symbol.as_str(), got[1].side, got[1].gross), ("", 0, -1.5));
        assert_eq!(
            got[0].line(),
            "CTPNL position=7 sym=USDJPY side=1 units=10000 gross=66.62 net=66 digits=2"
        );
        // Another exponent descales by itself: no digit count is assumed.
        let wide = ProtoOaGetPositionUnrealizedPnLRes {
            position_unrealized_pn_l: vec![row(7, 6_662_000, 0)],
            money_digits: 5,
            ..Default::default()
        };
        assert_eq!(unrealized_pnls_of(&wide, &open)[0].gross, 66.62);
    }
}
