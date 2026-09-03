//! Spot/trendbar → `vike-data` live-seam mapper. Pure functions only (no I/O, no actor state) —
//! `conn::on_inbound` (Task 4) calls these once a frame is decoded and pushes the result into the
//! `LiveDataSink` given to the actor at construction. Ports nothing — cTrader Open API
//! (https://help.ctrader.com/open-api/); wire prices use [`RELATIVE_PRICE_SCALE`], NOT
//! `symbols::SymbolMap::scale` (`10^digits`) — see that constant's doc for the live evidence.

use crate::proto::{
    ProtoOaExecutionEvent, ProtoOaExecutionType, ProtoOaNewOrderReq, ProtoOaOrder,
    ProtoOaOrderType, ProtoOaSpotEvent, ProtoOaTradeSide, ProtoOaTrendbar, ProtoOaTrendbarPeriod,
};
use crate::symbols::{SymbolMap, VolumeGrid};
use vike_model::events::{
    Event, FillEvent, OrderAccepted, OrderCancelRejected, OrderCanceled, OrderExpired, OrderFilled,
    OrderModified, OrderPartiallyFilled, OrderRejected, TradeId,
};
use vike_model::{now_ms, Bar, OrderRequest};

/// The venue tag stamped onto every canonical exec `Event` this mapper emits — matches
/// `conn::VENUE` / the crate name / the `{VENUE}_...` credentials convention.
const VENUE: &str = "ctrader";

/// cTrader quotes ALL prices as integers in 1/100000 of a unit (relative price), independent of
/// the symbol's display `digits`. Verified live: EURUSD 113986->1.13986, USDJPY 16233600->162.336.
/// Order prices (Task 5) use the same scale.
pub const RELATIVE_PRICE_SCALE: f64 = 100_000.0;

/// Descale a `ProtoOASpotEvent`'s `bid`/`ask` into a float quote and resolve its `symbol_id` to a
/// name. Returns `None` if either price is absent (a technical/keepalive spot event with no fresh
/// price — cTrader sends these) or the symbol id is unknown to `symbols` (never seen in the
/// `SymbolsList`/`SymbolById` handshake responses). `symbols` is used ONLY for id->name
/// resolution here — the price divisor is the fixed [`RELATIVE_PRICE_SCALE`], not
/// `symbols.scale(id)` (which is `10^digits` and wrong for this purpose; live-verified, see
/// `RELATIVE_PRICE_SCALE`'s doc).
pub fn spot_to_quote(ev: &ProtoOaSpotEvent, symbols: &SymbolMap) -> Option<(String, f64, f64)> {
    let bid = ev.bid?;
    let ask = ev.ask?;
    let name = symbols.name_of(ev.symbol_id)?;
    Some((name.to_string(), bid as f64 / RELATIVE_PRICE_SCALE, ask as f64 / RELATIVE_PRICE_SCALE))
}

/// Map a `subscribe_bars` interval string (the `vike_data::DataClient` vocabulary, e.g. `"1m"`,
/// `"1h"`) to the wire `ProtoOATrendbarPeriod` enum. `None` for any interval cTrader has no
/// matching period for (e.g. sub-minute) — the caller (`data::CtraderData::subscribe_bars`) turns
/// that into `LiveDataError::Unsupported`.
pub fn trendbar_period_for_interval(interval: &str) -> Option<ProtoOaTrendbarPeriod> {
    use ProtoOaTrendbarPeriod::*;
    Some(match interval {
        "1m" => M1,
        "2m" => M2,
        "3m" => M3,
        "4m" => M4,
        "5m" => M5,
        "10m" => M10,
        "15m" => M15,
        "30m" => M30,
        "1h" => H1,
        "4h" => H4,
        "12h" => H12,
        "1d" => D1,
        "1w" => W1,
        "1M" => Mn1,
        _ => return None,
    })
}

/// The reverse of [`trendbar_period_for_interval`] — the interval string a sink call
/// (`seed_bars`/`close_bar`/`forming_bar`) reports for a wire `ProtoOATrendbarPeriod`.
pub fn interval_for_trendbar_period(period: ProtoOaTrendbarPeriod) -> &'static str {
    use ProtoOaTrendbarPeriod::*;
    match period {
        M1 => "1m",
        M2 => "2m",
        M3 => "3m",
        M4 => "4m",
        M5 => "5m",
        M10 => "10m",
        M15 => "15m",
        M30 => "30m",
        H1 => "1h",
        H4 => "4h",
        H12 => "12h",
        D1 => "1d",
        W1 => "1w",
        Mn1 => "1M",
    }
}

/// Approximate bar-period duration in milliseconds — used ONLY to size the historical seed
/// window (`data::CtraderData::subscribe_bars`'s `from_ts = now - N * period_ms`, F2), never
/// where a bar's own timestamp is computed (that stays `trendbar_to_bar`'s exact
/// `utc_timestamp_in_minutes * 60_000`). `Mn1` (calendar month) has no fixed length; 30 days is a
/// deliberate overestimate — a wider `from_ts` costs nothing since `GetTrendbars`'s own `count`
/// field caps the response regardless.
pub fn trendbar_period_ms(period: ProtoOaTrendbarPeriod) -> i64 {
    use ProtoOaTrendbarPeriod::*;
    const MINUTE: i64 = 60_000;
    const HOUR: i64 = 60 * MINUTE;
    const DAY: i64 = 24 * HOUR;
    match period {
        M1 => MINUTE,
        M2 => 2 * MINUTE,
        M3 => 3 * MINUTE,
        M4 => 4 * MINUTE,
        M5 => 5 * MINUTE,
        M10 => 10 * MINUTE,
        M15 => 15 * MINUTE,
        M30 => 30 * MINUTE,
        H1 => HOUR,
        H4 => 4 * HOUR,
        H12 => 12 * HOUR,
        D1 => DAY,
        W1 => 7 * DAY,
        Mn1 => 30 * DAY,
    }
}

/// Decode one `ProtoOATrendbar` (low + deltas, cTrader's delta-encoded OHLC) into a `Bar`. Prices
/// descale by the fixed [`RELATIVE_PRICE_SCALE`] (NOT `SymbolMap::scale(id)` — live-verified wrong
/// for non-5-digit symbols, see that constant's doc). Returns `None` if any of the required price
/// fields (`low`/`delta_open`/`delta_close`/`delta_high`) or the bar timestamp
/// (`utc_timestamp_in_minutes`) is absent — an incomplete trendbar entry cannot be reconstructed.
pub fn trendbar_to_bar(tb: &ProtoOaTrendbar) -> Option<Bar> {
    let low_i = tb.low?;
    let delta_open = tb.delta_open?;
    let delta_close = tb.delta_close?;
    let delta_high = tb.delta_high?;
    let minutes = tb.utc_timestamp_in_minutes?;
    let low = low_i as f64 / RELATIVE_PRICE_SCALE;
    let open = (low_i as f64 + delta_open as f64) / RELATIVE_PRICE_SCALE;
    let close = (low_i as f64 + delta_close as f64) / RELATIVE_PRICE_SCALE;
    let high = (low_i as f64 + delta_high as f64) / RELATIVE_PRICE_SCALE;
    Some(Bar {
        ts: minutes as i64 * 60_000,
        open,
        high,
        low,
        close,
        volume: tb.volume as f64,
        funding: None,
        bid: None,
        ask: None,
        symbol: None,
    })
}

/// Extract every embedded live trendbar from a `ProtoOASpotEvent` (`ProtoOASpotEvent.trendbar` —
/// populated only when the account has a live-trendbar subscription on this symbol) as
/// `(symbol_name, interval, Bar)` triples, skipping entries whose period or price fields are
/// missing/unresolvable. A spot event with no trendbar subscription active yields an empty `Vec`.
pub fn spot_to_bars(
    ev: &ProtoOaSpotEvent,
    symbols: &SymbolMap,
) -> Vec<(String, &'static str, Bar)> {
    let Some(name) = symbols.name_of(ev.symbol_id) else {
        return Vec::new();
    };
    ev.trendbar
        .iter()
        .filter_map(|tb| {
            let period = ProtoOaTrendbarPeriod::try_from(tb.period?).ok()?;
            let bar = trendbar_to_bar(tb)?;
            Some((name.to_string(), interval_for_trendbar_period(period), bar))
        })
        .collect()
}

// ============================ Task 5: order flow (exec) ============================
//
// Volume unit (LIVE-VERIFIED, cTrader demo 2026-07-14): cTrader order volume is in **centi-units**
// (1/100 of a base unit) — `volume = units × 100`. vike `OrderRequest.qty` is in base-currency
// UNITS (same interpretation as OANDA — see `oanda/src/exec.rs`, `units = round(qty)`), so
// `volume = round(qty × 100)`, then rounded to the symbol's `step_volume` and clamped to
// `[min_volume, max_volume]`. This qty→volume interpretation is the ONE piece still awaiting the
// live demo-fill smoke (Task 6) to confirm end-to-end; the centi-unit factor and the grid values
// above ARE live-verified.
//
// Order PRICES: `ProtoOaNewOrderReq.limit_price`/`stop_price` are protobuf `double`s holding the
// ABSOLUTE price (e.g. 1.23456), NOT the 1e5 relative-scaled integers the SPOT/trendbar market
// feed uses (only `relative_stop_loss`/`relative_take_profit` are 1e5-scaled). So order prices
// pass straight through, and `ProtoOaDeal.execution_price` (also a `double`) is read as an
// absolute price with NO descale — confirmed against the vendored proto (`ProtoOaNewOrderReq`
// tag 7/8, `ProtoOaDeal` tag 10 are all `double`).

/// Round `raw` centi-volume to the grid's `step_volume` and clamp to `[min_volume, max_volume]`.
/// Returns `None` when the rounded volume falls below `min_volume` (never place a sub-minimum
/// order — the caller synthesizes a terminal reject). A grid field of `0` is inert: `step 0`
/// leaves `raw` unrounded, `max 0` applies no cap, `min 0` never rejects (mirrors the
/// SymbolProperties absent-grid-is-inert rule for symbols with no `SymbolById` volume data).
fn round_volume(raw: i64, grid: VolumeGrid) -> Option<i64> {
    let stepped = if grid.step_volume > 0 {
        // round to NEAREST step (half away from zero, matching f64::round)
        let steps = (raw as f64 / grid.step_volume as f64).round() as i64;
        steps * grid.step_volume
    } else {
        raw
    };
    let clamped = if grid.max_volume > 0 { stepped.min(grid.max_volume) } else { stepped };
    if clamped < grid.min_volume {
        return None;
    }
    Some(clamped)
}

/// Map `OrderRequest.order_type` (`"market"`/`"limit"`/`"stop"`) to the wire order-type enum.
/// `None` for any unrecognized type — the caller rejects rather than silently defaulting.
fn order_type_of(order_type: &str) -> Option<ProtoOaOrderType> {
    Some(match order_type {
        "market" => ProtoOaOrderType::Market,
        "limit" => ProtoOaOrderType::Limit,
        "stop" => ProtoOaOrderType::Stop,
        _ => return None,
    })
}

/// Build a `ProtoOaNewOrderReq` from a canonical `OrderRequest`. Returns `None` (→ the exec client
/// synthesizes a terminal `OrderRejected`, never a vanished order) when: the symbol is unknown to
/// `symbols`; the `order_type` is unrecognized; `side == 0`; the volume rounds below the symbol's
/// `min_volume`; or a limit/stop order has no price. `client_order_id` is carried in BOTH `label`
/// (wire correlation) and the dedicated `client_order_id` field (FIX ClOrdID twin). Limit/stop
/// prices are the ABSOLUTE price as a `double` (see the module note above — NOT 1e5-scaled).
pub fn order_to_new_order(
    req: &OrderRequest,
    ctid: i64,
    symbols: &SymbolMap,
) -> Option<ProtoOaNewOrderReq> {
    let symbol_id = symbols.id_of(&req.symbol)?;
    let order_type = order_type_of(&req.order_type)?;
    let trade_side = match req.side {
        s if s > 0 => ProtoOaTradeSide::Buy,
        s if s < 0 => ProtoOaTradeSide::Sell,
        _ => return None, // side 0 is not a valid direction
    };
    // qty (units) → centi-units, then snap to the symbol's step/min/max grid (inert if absent).
    let raw = (req.qty * 100.0).round() as i64;
    let grid = symbols.volume_grid(symbol_id).unwrap_or_default();
    let volume = round_volume(raw, grid)?;

    // Limit needs `price`; Stop needs `trigger_price`. Absent → reject (None), never place a
    // priceless conditional. Market carries neither.
    let (limit_price, stop_price) = match order_type {
        ProtoOaOrderType::Limit => (Some(req.price?), None),
        ProtoOaOrderType::Stop => (None, Some(req.trigger_price?)),
        _ => (None, None),
    };

    Some(ProtoOaNewOrderReq {
        ctid_trader_account_id: ctid,
        symbol_id,
        order_type: order_type as i32,
        trade_side: trade_side as i32,
        volume,
        limit_price,
        stop_price,
        label: Some(req.client_order_id.clone()),
        client_order_id: Some(req.client_order_id.clone()),
        ..Default::default()
    })
}

/// The correlation id + venue order id an execution event carries: `client_order_id` from the
/// referenced order (its dedicated field, falling back to the `label` we set at submit), and the
/// venue's numeric `order_id`. Returns `("", 0)`-ish defaults when the order ref is absent.
fn order_correlation(ev: &ProtoOaExecutionEvent) -> (String, i64) {
    match &ev.order {
        Some(o) => {
            let coid = o
                .client_order_id
                .clone()
                .or_else(|| o.trade_data.label.clone())
                .unwrap_or_default();
            (coid, o.order_id)
        }
        None => (String::new(), 0),
    }
}

/// Build a `FillEvent` from the execution event's `deal` (the cTrader execution entity). `deal`
/// carries the filled volume in centi-units (`/100` → units) and an ABSOLUTE `execution_price`
/// `double` (no descale). Commission is LIVE-VERIFIED (2026-07-14, real demo fill): `deal.commission`
/// is a SIGNED integer scaled by `10^money_digits` — preferring the DEAL's own `money_digits`
/// (`ProtoOADeal.moneyDigits`, tag 17, whose wire doc says "Affects commission." directly) and
/// falling back to the account-level `ProtoOATrader.money_digits` (fetched once at connect/
/// reconnect and threaded in by the caller) only when the deal omits it. A verified sample:
/// `commission=-3`, `money_digits=2` -> `-0.03`. cTrader's wire sign is NEGATIVE for a charged fee
/// (opposite of `vike_model::events::FillEvent.commission`'s convention: `> 0` = charge/cost,
/// `< 0` = rebate — see that field's doc and `vike-exec/src/account.rs`'s `self.balance -=
/// fill.commission`), so the raw value is NEGATED here, matching the OKX mapper's
/// `commission: -fillFee` pattern.
pub fn deal_fill(
    ev: &ProtoOaExecutionEvent,
    coid: &str,
    symbols: &SymbolMap,
    money_digits: u32,
) -> Option<FillEvent> {
    let deal = ev.deal.as_ref()?;
    let symbol = symbols.name_of(deal.symbol_id).unwrap_or_default();
    let side = if deal.trade_side == ProtoOaTradeSide::Sell as i32 { -1 } else { 1 };
    let last_px = deal.execution_price.unwrap_or_else(|| {
        tracing::warn!(
            deal_id = deal.deal_id,
            order_id = deal.order_id,
            client_order_id = coid,
            "ctrader: fill deal has no execution_price — defaulting to 0.0 (would corrupt P&L)"
        );
        0.0
    });
    let digits = deal.money_digits.unwrap_or(money_digits) as i32;
    // Negate: cTrader's wire commission is negative for a charged fee; vike-model's
    // `FillEvent.commission` convention is the opposite (positive = charge/cost).
    let commission = -(deal.commission.unwrap_or(0) as f64) / 10f64.powi(digits);
    Some(FillEvent {
        // `deal_id` is a protobuf INTEGER, so `to_string()` always renders at least one digit and
        // this constructor cannot fail — unlike every JSON venue, there is no absent-string case to
        // decide a policy for. The string is unchanged from before.
        // ⚠ Pre-existing and deliberately untouched: prost defaults a missing scalar to `0`, so a
        // deal that arrived without a `deal_id` would key on `"0"` and collide with every other such
        // deal. That is a protobuf-decode concern (it needs an `Option`/presence check on the wire
        // field), not something an id newtype can see, and it is not made worse here.
        trade_id: TradeId::new(deal.deal_id.to_string())
            .expect("a protobuf i64 deal_id always renders at least one digit"),
        client_order_id: coid.to_string(),
        venue: VENUE.into(),
        symbol: symbol.into(),
        side,
        last_qty: deal.filled_volume as f64 / 100.0,
        last_px,
        commission,
        commission_asset: "".into(),
        liquidity_side: Default::default(),
        ts: deal.execution_timestamp,
        mark_price: None,
        position_side: Default::default(),
    })
}

/// Timestamp for a non-fill lifecycle event: the order's last-update time, else wall clock.
pub fn lifecycle_ts(ev: &ProtoOaExecutionEvent) -> i64 {
    ev.order.as_ref().and_then(|o| o.utc_last_update_timestamp).unwrap_or_else(now_ms)
}

/// The confirmed post-amend TOTAL qty/price an `ORDER_REPLACED` event's referenced order carries.
/// `trade_data.volume` is the authoritative new volume in centi-units (`/100.0` → units, same
/// unit convention as [`order_to_new_order`]'s qty→volume path). The new price is `limit_price`
/// for a LIMIT order, `stop_price` for a STOP order — falling back to whichever is present for any
/// other order type (e.g. STOP_LIMIT), since only one of the two is ever populated in practice.
fn order_modified_terms(order: &ProtoOaOrder) -> (Option<f64>, Option<f64>) {
    let new_qty = Some(order.trade_data.volume as f64 / 100.0);
    let new_price = match ProtoOaOrderType::try_from(order.order_type) {
        Ok(ProtoOaOrderType::Limit) => order.limit_price,
        Ok(ProtoOaOrderType::Stop) => order.stop_price,
        _ => order.limit_price.or(order.stop_price),
    };
    (new_qty, new_price)
}

/// Map a `ProtoOaExecutionEvent` to canonical `vike_model` `Event`s. Switches on `execution_type`:
/// ACCEPTED→`OrderAccepted` (with venue order id), FILLED→`[Event::Fill, OrderFilled]`, PARTIAL_FILL→
/// `[Event::Fill, OrderPartiallyFilled]` (both DUAL-PUBLISH the bare `Event::Fill` FIRST so the core
/// `Account` folds the fill into position/PnL/trades, then the wrap that drives the OMS order FSM —
/// the venue-contract pattern mirrored from binance's `event_mapper`), CANCELLED→`OrderCanceled`,
/// REJECTED→`OrderRejected` (reason=error code),
/// REPLACED→`OrderModified` (carrying the confirmed new qty/price from the order's post-amend
/// `trade_data.volume`/`limit_price`/`stop_price`, see [`order_modified_terms`]), EXPIRED→
/// `OrderExpired`, CANCEL_REJECTED→`OrderCancelRejected` (reason=error code; NON-TERMINAL — the
/// order stays live). Non-order-lifecycle types (SWAP, DEPOSIT, BONUS, etc.) and fills with no
/// `deal` yield an empty `Vec` — the emitter split: the venue side owns every state change AFTER
/// submit. `client_order_id` is pulled from the referenced order; fill prices/volumes/commission
/// are descaled via [`deal_fill`], which prefers the deal's own `money_digits` and falls back to
/// `money_digits` (the account's `ProtoOATrader.money_digits`, fetched at connect/reconnect — see
/// `conn::open_and_handshake`) only when the deal omits it.
pub fn exec_event_to_events(
    ev: &ProtoOaExecutionEvent,
    symbols: &SymbolMap,
    money_digits: u32,
) -> Vec<Event> {
    let Ok(exec_type) = ProtoOaExecutionType::try_from(ev.execution_type) else {
        return Vec::new();
    };
    let (coid, venue_order_id) = order_correlation(ev);
    let voi = || Some(venue_order_id.to_string().into());
    match exec_type {
        ProtoOaExecutionType::OrderAccepted => vec![Event::OrderAccepted(OrderAccepted {
            client_order_id: coid,
            venue_order_id: voi(),
            ts: lifecycle_ts(ev),
        })],
        ProtoOaExecutionType::OrderFilled => match deal_fill(ev, &coid, symbols, money_digits) {
            Some(fill) => {
                let ts = fill.ts;
                // DUAL-PUBLISH (venue-contract, see binance `event_mapper`): `Event::Fill` FIRST
                // (the core `Account` folds it into position/PnL/trades), then the `OrderFilled`
                // wrap that drives the OMS order FSM. Emitting only the wrap leaves `Account` blind.
                vec![
                    Event::Fill(fill.clone()),
                    Event::OrderFilled(OrderFilled { client_order_id: coid, fill, ts }),
                ]
            }
            None => Vec::new(),
        },
        ProtoOaExecutionType::OrderPartialFill => match deal_fill(ev, &coid, symbols, money_digits)
        {
            Some(fill) => {
                let ts = fill.ts;
                // DUAL-PUBLISH (same as the full-fill arm): `Event::Fill` FIRST for the `Account`
                // fold, then the `OrderPartiallyFilled` wrap for the OMS FSM.
                vec![
                    Event::Fill(fill.clone()),
                    Event::OrderPartiallyFilled(OrderPartiallyFilled {
                        client_order_id: coid,
                        fill,
                        ts,
                    }),
                ]
            }
            None => Vec::new(),
        },
        ProtoOaExecutionType::OrderCancelled => vec![Event::OrderCanceled(OrderCanceled {
            client_order_id: coid,
            reason: ev.error_code.clone().unwrap_or_default().into(),
            ts: lifecycle_ts(ev),
        })],
        ProtoOaExecutionType::OrderRejected => vec![Event::OrderRejected(OrderRejected {
            client_order_id: coid,
            reason: ev.error_code.clone().unwrap_or_default().into(),
            ts: lifecycle_ts(ev),
        })],
        ProtoOaExecutionType::OrderReplaced => {
            let (new_qty, new_price) =
                ev.order.as_ref().map(order_modified_terms).unwrap_or((None, None));
            vec![Event::OrderModified(OrderModified {
                client_order_id: coid,
                venue_order_id: voi(),
                new_qty,
                new_price,
                ts: lifecycle_ts(ev),
            })]
        }
        ProtoOaExecutionType::OrderExpired => {
            vec![Event::OrderExpired(OrderExpired { client_order_id: coid, ts: lifecycle_ts(ev) })]
        }
        ProtoOaExecutionType::OrderCancelRejected => {
            vec![Event::OrderCancelRejected(OrderCancelRejected {
                client_order_id: coid,
                reason: ev.error_code.clone().unwrap_or_default().into(),
                ts: lifecycle_ts(ev),
            })]
        }
        // SWAP / DEPOSIT_WITHDRAW / BONUS_* — not an order-lifecycle transition this seam models.
        // Left unemitted (no vanished ORDER, since these carry no order state change the OMS FSM
        // tracks).
        _ => Vec::new(),
    }
}

/// Whether an execution event drives its order to a DEFINITELY-TERMINAL state — one from which no
/// further lifecycle event can arrive for that `client_order_id`. FILLED (a full fill), CANCELLED,
/// REJECTED, and EXPIRED are terminal; ACCEPTED and PARTIAL_FILL leave the order LIVE (more events
/// to come), and REPLACED/CANCEL_REJECTED also leave it live (still working). Non-order-lifecycle
/// types (SWAP/DEPOSIT/BONUS/…) and unrecognized codes are not terminal. Single source of truth for
/// the `execution_type` classification the conn layer uses to prune its coid→orderId map (so it does
/// NOT re-derive the same match) — mirrors the terminal arms of [`exec_event_to_events`].
pub fn exec_event_is_terminal(ev: &ProtoOaExecutionEvent) -> bool {
    matches!(
        ProtoOaExecutionType::try_from(ev.execution_type),
        Ok(ProtoOaExecutionType::OrderFilled
            | ProtoOaExecutionType::OrderCancelled
            | ProtoOaExecutionType::OrderRejected
            | ProtoOaExecutionType::OrderExpired)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proto::{ProtoOaLightSymbol, ProtoOaSymbol};

    fn test_symbol_map() -> SymbolMap {
        let light = vec![ProtoOaLightSymbol {
            symbol_id: 1,
            symbol_name: Some("EURUSD".to_string()),
            ..Default::default()
        }];
        let full = vec![ProtoOaSymbol { symbol_id: 1, digits: 5, ..Default::default() }];
        SymbolMap::from_symbols(&light, &full)
    }

    /// USDJPY, `digits=3` — the live-verified regression fixture: cTrader's relative price scale
    /// is fixed at 1e5 for every symbol, NOT `10^digits` (digits=3 would wrongly give `10^3`).
    fn usdjpy_symbol_map() -> SymbolMap {
        let light = vec![ProtoOaLightSymbol {
            symbol_id: 4,
            symbol_name: Some("USDJPY".to_string()),
            ..Default::default()
        }];
        let full = vec![ProtoOaSymbol { symbol_id: 4, digits: 3, ..Default::default() }];
        SymbolMap::from_symbols(&light, &full)
    }

    #[test]
    fn descales_spot_event() {
        let syms = test_symbol_map();
        let ev = ProtoOaSpotEvent {
            symbol_id: 1,
            bid: Some(113911),
            ask: Some(113912),
            ..Default::default()
        };
        let (sym, bid, ask) = spot_to_quote(&ev, &syms).unwrap();
        assert_eq!(sym, "EURUSD");
        assert!((bid - 1.13911).abs() < 1e-9);
        assert!((ask - 1.13912).abs() < 1e-9);
    }

    /// Live-verified regression: a `digits=3` symbol (USDJPY) MUST still descale by the fixed
    /// 1e5 relative-price scale, not `10^digits` (which would wrongly yield 16233.6/16233.7). This
    /// test fails under the old `symbols.scale(id)` descale and passes under the fix.
    #[test]
    fn descales_spot_event_digits3_uses_fixed_scale_not_10_pow_digits() {
        let syms = usdjpy_symbol_map();
        let ev = ProtoOaSpotEvent {
            symbol_id: 4,
            bid: Some(16233600),
            ask: Some(16233700),
            ..Default::default()
        };
        let (sym, bid, ask) = spot_to_quote(&ev, &syms).unwrap();
        assert_eq!(sym, "USDJPY");
        assert!((bid - 162.336).abs() < 1e-6, "bid={bid}");
        assert!((ask - 162.337).abs() < 1e-6, "ask={ask}");
    }

    #[test]
    fn interval_period_roundtrip() {
        for interval in ["1m", "5m", "15m", "30m", "1h", "4h", "12h", "1d", "1w", "1M"] {
            let period = trendbar_period_for_interval(interval).expect("known interval");
            assert_eq!(interval_for_trendbar_period(period), interval);
        }
        assert!(trendbar_period_for_interval("1s").is_none());
    }

    #[test]
    fn trendbar_decodes_delta_encoded_ohlc() {
        let syms = test_symbol_map();
        let tb = ProtoOaTrendbar {
            volume: 42,
            period: Some(ProtoOaTrendbarPeriod::M1 as i32),
            low: Some(113900),
            delta_open: Some(5),
            delta_close: Some(12),
            delta_high: Some(20),
            utc_timestamp_in_minutes: Some(1000),
        };
        let ev = ProtoOaSpotEvent { symbol_id: 1, trendbar: vec![tb], ..Default::default() };
        let bars = spot_to_bars(&ev, &syms);
        assert_eq!(bars.len(), 1);
        let (sym, interval, bar) = &bars[0];
        assert_eq!(sym, "EURUSD");
        assert_eq!(*interval, "1m");
        assert_eq!(bar.ts, 1000 * 60_000);
        assert!((bar.low - 1.139).abs() < 1e-9);
        assert!((bar.open - 1.13905).abs() < 1e-9);
        assert!((bar.close - 1.13912).abs() < 1e-9);
        assert!((bar.high - 1.1392).abs() < 1e-9);
        assert_eq!(bar.volume, 42.0);
    }

    /// Live-verified regression: trendbar OHLC (low + deltas) on a `digits=3` symbol (USDJPY)
    /// also descales by the fixed 1e5 scale, not `10^digits`. Raw `low=16231200` -> `162.312`.
    #[test]
    fn trendbar_decodes_delta_encoded_ohlc_digits3_uses_fixed_scale() {
        let syms = usdjpy_symbol_map();
        let tb = ProtoOaTrendbar {
            volume: 7,
            period: Some(ProtoOaTrendbarPeriod::M1 as i32),
            low: Some(16231200),
            delta_open: Some(500),
            delta_close: Some(1200),
            delta_high: Some(2000),
            utc_timestamp_in_minutes: Some(2000),
        };
        let ev = ProtoOaSpotEvent { symbol_id: 4, trendbar: vec![tb], ..Default::default() };
        let bars = spot_to_bars(&ev, &syms);
        assert_eq!(bars.len(), 1);
        let (sym, interval, bar) = &bars[0];
        assert_eq!(sym, "USDJPY");
        assert_eq!(*interval, "1m");
        assert_eq!(bar.ts, 2000 * 60_000);
        assert!((bar.low - 162.312).abs() < 1e-6, "low={}", bar.low);
        assert!((bar.open - 162.317).abs() < 1e-6, "open={}", bar.open);
        assert!((bar.close - 162.324).abs() < 1e-6, "close={}", bar.close);
        assert!((bar.high - 162.332).abs() < 1e-6, "high={}", bar.high);
        assert_eq!(bar.volume, 7.0);
    }

    #[test]
    fn trendbar_missing_field_is_skipped() {
        let syms = test_symbol_map();
        let tb = ProtoOaTrendbar {
            volume: 1,
            period: Some(ProtoOaTrendbarPeriod::M1 as i32),
            low: None, // missing — incomplete entry
            delta_open: Some(5),
            delta_close: Some(12),
            delta_high: Some(20),
            utc_timestamp_in_minutes: Some(1000),
        };
        let ev = ProtoOaSpotEvent { symbol_id: 1, trendbar: vec![tb], ..Default::default() };
        assert!(spot_to_bars(&ev, &syms).is_empty());
    }

    #[test]
    fn spot_to_bars_unknown_symbol_is_empty() {
        let syms = test_symbol_map();
        let ev = ProtoOaSpotEvent { symbol_id: 999, ..Default::default() };
        assert!(spot_to_bars(&ev, &syms).is_empty());
    }

    #[test]
    fn trendbar_period_ms_matches_known_durations() {
        assert_eq!(trendbar_period_ms(ProtoOaTrendbarPeriod::M1), 60_000);
        assert_eq!(trendbar_period_ms(ProtoOaTrendbarPeriod::M5), 5 * 60_000);
        assert_eq!(trendbar_period_ms(ProtoOaTrendbarPeriod::H1), 3_600_000);
        assert_eq!(trendbar_period_ms(ProtoOaTrendbarPeriod::D1), 86_400_000);
    }
}
