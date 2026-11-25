//! Binance-grammar REST order-history → events, for the audit-A3 post-reconnect resync. Shared by
//! vike-binance (`crate::execution::resync`) and vike-aster (`vike_aster::execution::resync`) — Aster's order/trade JSON
//! is a Binance fork (identical field names/shapes), so both venues call straight into this, each
//! passing its own `venue`.
//!
//! On a WS reconnect a terminal (fill/cancel/expire/reject) that landed during the reconnect
//! window is lost — the private WS does not replay. The resync supervisor fetches
//! `allOrders` (recent order states) + `myTrades`/`userTrades` (recent fills) and REPLAYS them
//! through the normal event lane; the core's `trade_id` dedup (`seen_trade_ids`/`seen_fsm_trade_ids`)
//! drops everything already seen and applies only the gap events. Mirrors the WS mapper's
//! dual-publish (bare `FillEvent` + `OrderPartiallyFilled`/`OrderFilled` wrap) so a replayed fill
//! folds byte-identically to a live one.
//!
//! **Broker-prefix strip (unified cross-venue attribution, task 6, INBOUND half).** `clientOrderId`
//! on an `allOrders` row is the SAME venue-stored id the WS mapper decodes, so it is passed through
//! [`crate::family::execution::order_map::strip_broker_order_client_id_prefix`] here too — see
//! `crate::family::execution::event_mapper::spot`'s module doc for the full rationale.

use serde_json::Value;
// `get_str_boolless as s` — the shared Bool-LESS `str(x)` coercion (a bool yields `""`, never
// Python's `"True"`; the side/maker booleans are read via `crate::family::recon`'s union readers).
// Deliberately NOT `get_str`, which carries `json_str`'s `Bool` arm — see `get_str_boolless`'s doc.
use vike_bridge_core::json::{
    get_f64 as f, get_fee_f64 as fee_f, get_i64 as i, get_str_boolless as s,
};
use vike_bridge_core::terminal_events;
use vike_bridge_core::transport::{ErrorKind, VenueApiError};
use vike_model::events::LiquiditySide;
use vike_model::events::{
    Event, FillEvent, OrderCanceled, OrderExpired, OrderRejected, PositionLiquidated, TradeId,
};

use crate::family::execution::order_word::OrderWord;
use crate::family::execution::spot_fee::spot_fee_in_quote;

/// A history trade row's `id` as a [`TradeId`], or `None` for a row that carries none.
///
/// Both replay mappers below skip such a row (see [`map_history`]'s doc for the trade-off). It is a
/// shared helper only so the two loops cannot drift on the verdict.
///
/// The id is [`crate::family::fill_trade_id`]'s: bare for a fill of the `mounted` symbol (`symbol` IS
/// `mounted` for every replay that has always existed), `SYMBOL:id` for a fill of any other symbol of the
/// account, exactly as the user-data stream and the reconcile read of an admitted symbol spell it, so a
/// replayed foreign fill is a KNOWN trade id when the stream folded it.
fn trade_id_of(
    t: &Value,
    venue: &str,
    order_client_id: &str,
    symbol: &str,
    mounted: &str,
) -> Option<TradeId> {
    match crate::family::fill_trade_id(&s(t, "id"), symbol, mounted) {
        Ok(id) => Some(id),
        Err(_) => {
            tracing::warn!(
                venue,
                order_client_id = %order_client_id,
                "history trade row carries no `id` (tradeId) — skipping the replayed fill; with no \
                 dedup key the core's `seen_trade_ids` cannot tell it from the live fill it \
                 overlaps, so replaying it would double-book commission and realized PnL"
            );
            None
        }
    }
}

/// Replay recent spot order history as events (audit A3). `all_orders` is the
/// `GET /api/v3/allOrders` array, `my_trades` the spot account-trade array — Binance's
/// `GET /api/v3/myTrades` or Aster's `GET /api/v3/userTrades`, whose rows are futures-shaped and
/// are read through the family union grammar (see [`crate::family::recon::fill_side`]). Emits, per order:
/// its fills (bare `FillEvent` + `OrderPartiallyFilled`/`OrderFilled` wrap, mirroring the WS
/// mapper), then a non-fill terminal (`OrderCanceled`/`OrderExpired`/`OrderRejected`) if the order
/// ended that way. Still-open orders (`NEW`/live `PARTIALLY_FILLED`/`PENDING_CANCEL`: [`OrderWord`])
/// emit only their fills. The core dedups the overlap; only gap events apply.
///
/// A trade row with no `id` is SKIPPED (see [`trade_id_of`]). ⚠ That is deliberately allowed to cost
/// the wrap: skipping the LAST fill of a `FILLED` order means this replay emits no `OrderFilled`, so
/// the order stays non-terminal in the FSM until the live stream or a later pass terminalizes it.
/// A stuck-open order is recoverable (the confirm-grace watchdog pokes it, and recon reports it as
/// `MissingTerminal`); a double-booked fill is money that is already wrong. Binance/Aster document
/// `id` on every `myTrades`/`userTrades` row, so this is a malformed-response path, not a shape the
/// venue has.
///
/// Re-exported as `map_binance_history` by binance; aster's `map_aster_history` calls it with no link
/// id. `link_id` is the broker link id the account's orders carry (`None` = none configured).
pub fn map_history(
    all_orders: &Value,
    my_trades: &Value,
    venue: &str,
    symbol: &str,
    link_id: Option<&str>,
) -> Vec<Event> {
    map_history_of(all_orders, my_trades, venue, symbol, symbol, link_id)
}

/// [`map_history`] for the history of `symbol` when the engine is mounted on `mounted`: the same events,
/// with a fill of any symbol but the mounted one carrying the symbol-qualified trade id (`SYMBOL:id`,
/// [`trade_id_of`]) the user-data stream spells for it. `symbol == mounted` is [`map_history`] itself.
pub fn map_history_of(
    all_orders: &Value,
    my_trades: &Value,
    venue: &str,
    symbol: &str,
    mounted: &str,
    link_id: Option<&str>,
) -> Vec<Event> {
    let orders = all_orders.as_array().cloned().unwrap_or_default();
    let trades = my_trades.as_array().cloned().unwrap_or_default();

    // Group trades by venue orderId, sorted by trade id ascending (fill order).
    let mut trades_by_order: std::collections::HashMap<i64, Vec<&Value>> =
        std::collections::HashMap::new();
    for t in &trades {
        trades_by_order.entry(i(t, "orderId")).or_default().push(t);
    }
    for v in trades_by_order.values_mut() {
        v.sort_by_key(|t| i(t, "id"));
    }

    // A spot cancel-replace modify rests the replacement under the SAME order client id as the order it
    // cancelled, so `allOrders` lists the old (CANCELED) row beside its live successor. The
    // order client id's newest incarnation decides: an older row's CANCELED is not a cancel of the order
    // (replaying it would terminate a live order). Fills are keyed by `orderId` and unaffected.
    let mut newest_order_id_by_order_client_id: std::collections::HashMap<String, i64> =
        std::collections::HashMap::new();
    for o in &orders {
        let order_client_id =
            crate::family::execution::order_map::strip_broker_order_client_id_prefix(
                &s(o, "clientOrderId"),
                link_id,
            )
            .to_string();
        let e = newest_order_id_by_order_client_id.entry(order_client_id).or_insert(i64::MIN);
        *e = (*e).max(i(o, "orderId"));
    }

    let mut events = Vec::new();
    for o in &orders {
        let oid = i(o, "orderId");
        let order_client_id =
            crate::family::execution::order_map::strip_broker_order_client_id_prefix(
                &s(o, "clientOrderId"),
                link_id,
            )
            .to_string();
        let status = OrderWord::parse(&s(o, "status"));
        let update_ts = i(o, "updateTime");

        // Replay this order's fills as bare FillEvent + wrap (mirrors the WS mapper's dual-publish).
        if let Some(order_trades) = trades_by_order.get(&oid) {
            let n = order_trades.len();
            for (index, t) in order_trades.iter().enumerate() {
                let Some(trade_id) = trade_id_of(t, venue, &order_client_id, symbol, mounted)
                else {
                    continue;
                };
                let last_price_value = f(t, "price");
                // The family UNION grammar, NOT `isBuyer`/`isMaker` alone: Aster's spot
                // `/api/v3/userTrades` rows are futures-shaped (`side`/`maker`), and this
                // mapper replays them for the aster spot lane too. See
                // [`crate::family::recon::fill_side`] for why reading one spelling silently
                // signed every aster spot fill as a SELL.
                let side = crate::family::recon::fill_side(t);
                // The fee in QUOTE units and a BUY's quantity net of the base coin it paid, as the live
                // executionReport books them (see `spot_fee`): the engine keeps the FIRST delivery of a
                // trade id, so the legs must agree.
                let fee = spot_fee_in_quote(
                    symbol,
                    f(t, "qty"),
                    fee_f(t, "commission", venue),
                    &s(t, "commissionAsset"),
                    last_price_value,
                    side > 0,
                    venue,
                );
                let fill = FillEvent {
                    trade_id,
                    // a fill of an order placed by hand carries no client id, as the stream's (the spot rule)
                    order_client_id: crate::family::execution::order_map::spot_fill_order_client_id(
                        &order_client_id,
                    ),
                    venue: venue.to_string().into(),
                    symbol: symbol.to_string().into(),
                    side,
                    last_qty: fee.net_qty,
                    last_price: last_price_value,
                    commission: fee.commission,
                    commission_asset: fee.commission_asset.into(),
                    liquidity_side: LiquiditySide::from_maker(crate::family::recon::fill_is_maker(
                        t,
                    )),
                    timestamp: i(t, "time"),
                    mark_price: None,
                    position_side: vike_model::events::PositionSide::Both,
                };
                // OrderFilled only on the LAST trade of a fully-FILLED order; else PartiallyFilled.
                let ts = fill.timestamp;
                let is_filled = status == OrderWord::Filled && index + 1 == n;
                events.extend(terminal_events(order_client_id.clone(), fill, ts, is_filled));
            }
        }

        // Non-fill terminals (a FILLED order is already terminalized by its OrderFilled wrap above).
        let superseded = newest_order_id_by_order_client_id
            .get(&order_client_id)
            .is_some_and(|newest| *newest > oid);
        match status {
            // superseded = replaced by a cancel-replace modify; the live successor owns the order client id
            OrderWord::Canceled if superseded => {}
            OrderWord::Canceled => events.push(Event::OrderCanceled(OrderCanceled {
                order_client_id,
                reason: "reconcile: order closed during reconnect gap".to_string().into(),
                timestamp: update_ts,
                post_only_would_cross: false,
                refusal_id: None,
            })),
            OrderWord::Expired => events
                .push(Event::OrderExpired(OrderExpired { order_client_id, timestamp: update_ts })),
            OrderWord::Rejected => events.push(Event::OrderRejected(OrderRejected {
                cause: None,
                order_client_id,
                reason: "reconcile: order rejected during reconnect gap".to_string().into(),
                timestamp: update_ts,
                refusal_id: vike_model::events::refusal_id_of(
                    vike_model::OrderRefusalId::VenueRefused.word(),
                ),
            })),
            // Still open, or ended by the fill wraps above. `PENDING_CANCEL` is open: a fill that
            // beats the cancel must still land, and there is no `Canceled -> Filled` edge.
            OrderWord::New
            | OrderWord::LiveOther
            | OrderWord::PartiallyFilled
            | OrderWord::Filled
            | OrderWord::PendingCancel
            | OrderWord::Unknown => {}
        }
    }
    events
}

/// **Which foreign label this family's replay reads on a lane** (the `admit` of
/// `vike_bridge_core::execution_client::resync::replay_foreign`, shared by binance and aster, spot and perp): only
/// the mounted lane has a user-data stream, so the door never admits a symbol of the OTHER lane (a `.P` label on a
/// spot engine and back), and a touched label the router refused must not cost a request. A label is on the futures
/// lane by its `.P` or as a dated contract's bare name (`vike_venues::is_dated`: decision 0290's `BTCUSDT_261225`,
/// which a spot book never lists; binance's own lane name `BTCUSDT_261225.P` carries the `.P` as well). The exchange
/// symbol a read names is the label's `vike_venues::split_perp` head.
pub fn on_lane(label: &str, perp: bool) -> bool {
    (vike_venues::split_perp(label).1 || vike_venues::is_dated(label)) == perp
}

/// **What says "slow down" on this family's history reads** (the `slow_down` of
/// `vike_bridge_core::execution_client::resync::replay_foreign`): the transport's rate-limit class
/// (`ErrorKind::RateLimited` under the family's table, `crate::transport::error_codes::TAXONOMY`: 429, 418, -1003, -1008,
/// -1015). The walk ends after that symbol, so the reconnect of fifty accounts
/// at once cannot turn into an IP ban; whatever is left unread is reconcile's.
pub fn says_slow_down(e: &VenueApiError) -> bool {
    e.kind_with(|e| crate::transport::error_codes::TAXONOMY.lookup(e)) == ErrorKind::RateLimited
}

/// Replay recent USDⓈ-M futures history (audit A3), the perp twin of [`map_history`].
/// `all_orders` = fapi `allOrders`, `user_trades` = fapi `userTrades`. fapi trade fields differ
/// from spot: `side` (BUY/SELL, not `isBuyer`), `maker` (not `isMaker`), `positionSide`
/// (BOTH/LONG/SHORT). Mirrors the WS [`crate::family::execution::event_mapper::perp::map_perp`] fill conventions so a
/// replayed fill folds byte-identically.
///
/// `symbol` is the caller's engine SERIES symbol (`BTCUSDT.P`), and it is the label every replayed
/// event carries. That is the same string the stream now labels the same trade with (the frame's
/// `s` + `.P`), because both REST endpoints are queried for the engine symbol alone — so a trade
/// the stream already folded arrives here with the SAME id AND the same label, and
/// `Account::apply_fill` refuses it as a re-delivery instead of booking it twice.
///
/// **A venue LIQUIDATION replays as a `PositionLiquidated`, never as a fill** — exactly what the
/// stream emits for it (`crate::family::execution::event_mapper::perp`'s `is_liquidation_order_client_id`). The two are deduped by
/// DIFFERENT sets: a liquidation by the engine's `seen_liq_ids`, a fill by the account's ledger,
/// and neither sees the other. Before the stream's label was fixed (2026-10-03) its liquidation
/// was dropped and this replay's FILL was the only copy that folded; once the stream's liquidation
/// folds, a fill replayed for the same trade would close the already-closed position again and
/// open the opposite side. Whether `allOrders` lists the venue's `autoclose-` orders is not
/// measured in this tree; mirroring the stream makes the answer not matter.
///
/// Re-exported as `map_binance_perp_history` by binance; aster's `map_aster_perp_history` calls it with
/// no link id. `link_id` is the broker link id the account's orders carry (`None` = none configured).
pub fn map_perp_history(
    all_orders: &Value,
    user_trades: &Value,
    venue: &str,
    symbol: &str,
    link_id: Option<&str>,
) -> Vec<Event> {
    map_perp_history_of(all_orders, user_trades, venue, symbol, symbol, link_id)
}

/// [`map_perp_history`] for the history of the series `symbol` when the engine is mounted on `mounted`
/// (both series labels, `.P` included): a fill or a liquidation of any symbol but the mounted one carries
/// the symbol-qualified trade id (`SYMBOL:id`, [`trade_id_of`]) the user-data stream spells for it.
/// `symbol == mounted` is [`map_perp_history`] itself.
pub fn map_perp_history_of(
    all_orders: &Value,
    user_trades: &Value,
    venue: &str,
    symbol: &str,
    mounted: &str,
    link_id: Option<&str>,
) -> Vec<Event> {
    let orders = all_orders.as_array().cloned().unwrap_or_default();
    let trades = user_trades.as_array().cloned().unwrap_or_default();

    let mut trades_by_order: std::collections::HashMap<i64, Vec<&Value>> =
        std::collections::HashMap::new();
    for t in &trades {
        trades_by_order.entry(i(t, "orderId")).or_default().push(t);
    }
    for v in trades_by_order.values_mut() {
        v.sort_by_key(|t| i(t, "id"));
    }

    let mut events = Vec::new();
    for o in &orders {
        let oid = i(o, "orderId");
        let order_client_id =
            crate::family::execution::order_map::strip_broker_order_client_id_prefix(
                &s(o, "clientOrderId"),
                link_id,
            )
            .to_string();
        let status = OrderWord::parse(&s(o, "status"));
        let update_ts = i(o, "updateTime");

        let liquidation =
            crate::family::execution::event_mapper::perp::is_liquidation_order_client_id(
                &order_client_id,
            );
        if let Some(order_trades) = trades_by_order.get(&oid) {
            let n = order_trades.len();
            for (index, t) in order_trades.iter().enumerate() {
                // Same skip-on-missing-`id` verdict as the spot twin — see [`trade_id_of`].
                let Some(trade_id) = trade_id_of(t, venue, &order_client_id, symbol, mounted)
                else {
                    continue;
                };
                let position_side = crate::family::position_side_of(
                    t.get("positionSide").and_then(|x| x.as_str()),
                    venue,
                );
                if liquidation {
                    // The stream's liquidation arm, field for field: `qty`/`price`/`commission` are
                    // the `l`/`L`/`n` it reads, and the id is the `t` it keys `seen_liq_ids` on.
                    events.push(Event::PositionLiquidated(PositionLiquidated {
                        venue: venue.to_string().into(),
                        symbol: symbol.to_string().into(),
                        position_side,
                        qty: f(t, "qty"),
                        liquidation_price: f(t, "price"),
                        fee: fee_f(t, "commission", venue),
                        timestamp: i(t, "time"),
                        trade_id: trade_id.as_str().into(),
                        route_key: None, // stamped at the mount, as on the stream
                        fee_currency: None,
                    }));
                    continue; // liquidation -> PositionLiquidated ONLY, never a fill or a wrap
                }
                let mut fill = FillEvent {
                    trade_id,
                    order_client_id: order_client_id.clone(),
                    venue: venue.to_string().into(),
                    symbol: symbol.to_string().into(),
                    // Same family union as the spot twin above — byte-identical here, since a
                    // fapi row always carries `side`/`maker` and never the `is*` spelling.
                    side: crate::family::recon::fill_side(t),
                    last_qty: f(t, "qty"),
                    last_price: f(t, "price"),
                    commission: fee_f(t, "commission", venue),
                    commission_asset: s(t, "commissionAsset").into(),
                    liquidity_side: LiquiditySide::from_maker(crate::family::recon::fill_is_maker(
                        t,
                    )),
                    timestamp: i(t, "time"),
                    mark_price: None,
                    position_side,
                };
                // A fee in a third asset (BNB) is priced into the quote, as on the stream.
                if let Some((commission, quote)) =
                    crate::family::execution::spot_fee::perp_fee_in_quote(
                        fill.symbol.as_str(),
                        fill.commission,
                        fill.commission_asset.as_str(),
                        venue,
                    )
                {
                    fill.commission = commission;
                    fill.commission_asset = quote.into();
                }
                let ts = fill.timestamp;
                let is_filled = status == OrderWord::Filled && index + 1 == n;
                events.extend(terminal_events(order_client_id.clone(), fill, ts, is_filled));
            }
        }
        match status {
            OrderWord::Canceled => events.push(Event::OrderCanceled(OrderCanceled {
                order_client_id,
                reason: "reconcile: order closed during reconnect gap".to_string().into(),
                timestamp: update_ts,
                post_only_would_cross: false,
                refusal_id: None,
            })),
            OrderWord::Expired => events
                .push(Event::OrderExpired(OrderExpired { order_client_id, timestamp: update_ts })),
            OrderWord::Rejected => events.push(Event::OrderRejected(OrderRejected {
                cause: None,
                order_client_id,
                reason: "reconcile: order rejected during reconnect gap".to_string().into(),
                timestamp: update_ts,
                refusal_id: vike_model::events::refusal_id_of(
                    vike_model::OrderRefusalId::VenueRefused.word(),
                ),
            })),
            // As the spot twin: still open (`PENDING_CANCEL` included), or ended by the wraps above.
            OrderWord::New
            | OrderWord::LiveOther
            | OrderWord::PartiallyFilled
            | OrderWord::Filled
            | OrderWord::PendingCancel
            | OrderWord::Unknown => {}
        }
    }
    events
}

#[cfg(test)]
#[path = "resync_tests.rs"]
mod resync_tests;
