//! Spot/trendbar → `vike-data` live-seam mapper. Pure functions only (no I/O, no actor state) —
//! `transport::connection::on_inbound` (Task 4) calls these once a frame is decoded and pushes the result into the
//! `DataClientSink` given to the actor at construction. Ports nothing — cTrader Open API
//! (https://help.ctrader.com/open-api/); wire prices use [`RELATIVE_PRICE_SCALE`], NOT
//! `instrument::symbols::SymbolMap::scale` (`10^digits`) — see that constant's doc for the live evidence.

use crate::instrument::symbols::{SymbolMap, VolumeGrid};
use crate::transport::proto::{
    ProtoOaAmendOrderReq, ProtoOaExecutionEvent, ProtoOaExecutionType, ProtoOaNewOrderReq,
    ProtoOaOrder, ProtoOaOrderType, ProtoOaSpotEvent, ProtoOaTradeSide, ProtoOaTrendbar,
    ProtoOaTrendbarPeriod,
};
use vike_marketdata::Bar;
use vike_model::events::{
    Event, FillEvent, FundingEvent, OrderAccepted, OrderCancelRejected, OrderCanceled,
    OrderExpired, OrderModified, OrderRejected, PositionSide, TradeId,
};
use vike_model::{OrderRequest, OrderType, TimeInForce, now_ms};

/// The venue tag stamped onto every canonical exec `Event` this mapper emits — matches
/// `transport::connection::VENUE` / the crate name / the `{VENUE}_...` credentials convention.
const VENUE: &str = "ctrader";

/// cTrader quotes ALL prices as integers in 1/100000 of a unit (relative price), independent of
/// the symbol's display `digits`. Verified live: EURUSD 113986->1.13986, USDJPY 16233600->162.336.
/// Order prices (Task 5) use the same scale.
pub const RELATIVE_PRICE_SCALE: f64 = 100_000.0;

/// Descale a `ProtoOASpotEvent`'s `bid`/`ask` into a float quote and resolve its `symbol_id` to the USER
/// name the quote is keyed by (`SymbolMap::user_name_of`, decision 0290). Returns `None` if either price is
/// absent (a technical/keepalive spot event with no fresh price — cTrader sends these) or the symbol id is
/// unknown to `symbols` (never seen in the
/// `SymbolsList`/`SymbolById` handshake responses). `symbols` is used ONLY for id->name
/// resolution here — the price divisor is the fixed [`RELATIVE_PRICE_SCALE`], not
/// `symbols.scale(id)` (which is `10^digits` and wrong for this purpose; live-verified, see
/// `RELATIVE_PRICE_SCALE`'s doc).
pub fn spot_to_quote(event: &ProtoOaSpotEvent, symbols: &SymbolMap) -> Option<(String, f64, f64)> {
    let bid = event.bid?;
    let ask = event.ask?;
    let name = symbols.user_name_of(event.symbol_id)?;
    Some((name, bid as f64 / RELATIVE_PRICE_SCALE, ask as f64 / RELATIVE_PRICE_SCALE))
}

/// Map a `subscribe_bars` interval string (the `vike_data::DataClient` vocabulary, e.g. `"1m"`,
/// `"1h"`) to the wire `ProtoOATrendbarPeriod` enum. `None` for any interval cTrader has no
/// matching period for (e.g. sub-minute) — the caller (`feed::DataClientBusSpecHoldCtrader::ask`) turns
/// that into `DataClientError::Unsupported`.
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
/// window (`feed::DataClientBusSpecHoldCtrader::ask`'s `from_timestamp = now - N * period_ms`, F2) and to tell
/// the seed's still-forming bar (`crate::feed::bar`, which takes a month at 31 days), never
/// where a bar's own timestamp is computed (that stays `trendbar_to_bar`'s exact
/// `utc_timestamp_in_minutes * 60_000`). `Mn1` (calendar month) has no fixed length; 30 days is a
/// deliberate overestimate — a wider `from_timestamp` costs nothing since `GetTrendbars`'s own `count`
/// field caps the response regardless.
pub fn trendbar_period_ms(period: ProtoOaTrendbarPeriod) -> i64 {
    use ProtoOaTrendbarPeriod::*;
    use vike_model::{MS_PER_DAY, MS_PER_HOUR, MS_PER_MINUTE, MS_PER_WEEK};
    match period {
        M1 => MS_PER_MINUTE,
        M2 => 2 * MS_PER_MINUTE,
        M3 => 3 * MS_PER_MINUTE,
        M4 => 4 * MS_PER_MINUTE,
        M5 => 5 * MS_PER_MINUTE,
        M10 => 10 * MS_PER_MINUTE,
        M15 => 15 * MS_PER_MINUTE,
        M30 => 30 * MS_PER_MINUTE,
        H1 => MS_PER_HOUR,
        H4 => 4 * MS_PER_HOUR,
        H12 => 12 * MS_PER_HOUR,
        D1 => MS_PER_DAY,
        W1 => MS_PER_WEEK,
        Mn1 => 30 * MS_PER_DAY,
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
        timestamp: minutes as i64 * 60_000,
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
/// `(user name, interval, Bar)` triples, skipping entries whose period or price fields are
/// missing/unresolvable. A spot event with no trendbar subscription active yields an empty `Vec`.
pub fn spot_to_bars(
    event: &ProtoOaSpotEvent,
    symbols: &SymbolMap,
) -> Vec<(String, &'static str, Bar)> {
    if event.trendbar.is_empty() {
        return Vec::new(); // the common spot event: no name to look up
    }
    let Some(name) = symbols.user_name_of(event.symbol_id) else {
        return Vec::new();
    };
    event
        .trendbar
        .iter()
        .filter_map(|tb| {
            let period = ProtoOaTrendbarPeriod::try_from(tb.period?).ok()?;
            let bar = trendbar_to_bar(tb)?;
            Some((name.clone(), interval_for_trendbar_period(period), bar))
        })
        .collect()
}

// ============================ Task 5: order flow (exec) ============================
//
// Volume unit (LIVE-VERIFIED, cTrader demo 2026-07-14): cTrader order volume is in **centi-units**
// (1/100 of a base unit) — `volume = units × 100`. vike `OrderRequest.qty` is in base-currency
// UNITS (same interpretation as OANDA — see `oanda/src/execution/mod.rs`, `units = round(qty)`), so
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
///
/// `down` floors to the step below instead of the nearest step: an AMEND must never carry a volume
/// larger than the quantity the pre-trade gate judged (the nearest step can be above it).
fn round_volume(raw: i64, grid: VolumeGrid, down: bool) -> Option<i64> {
    let stepped = if grid.step_volume > 0 {
        let steps = if down {
            raw.div_euclid(grid.step_volume)
        } else {
            // round to NEAREST step (half away from zero, matching f64::round)
            (raw as f64 / grid.step_volume as f64).round() as i64
        };
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

/// Map `OrderRequest.order_type` (market / limit / stop / stop_limit) to the wire order-type enum.
/// `None` for every other kind (take_profit and an unrecognised word) — the caller rejects rather
/// than silently defaulting.
fn order_type_of(order_type: &OrderType) -> Option<ProtoOaOrderType> {
    Some(match order_type {
        OrderType::Market => ProtoOaOrderType::Market,
        OrderType::Limit => ProtoOaOrderType::Limit,
        OrderType::Stop => ProtoOaOrderType::Stop,
        OrderType::StopLimit => ProtoOaOrderType::StopLimit,
        // ctrader's `MARKET_RANGE` is not a market-to-limit (the venue matrix), so it stays refused;
        // so do the trailing kinds (its `trailingStopLoss` is a flag on a position's stop loss)
        OrderType::TakeProfit
        | OrderType::MarketToLimit
        | OrderType::Trailing
        | OrderType::TrailingLimit
        | OrderType::Pegged
        | OrderType::TrailingTakeProfit
        | OrderType::Unrecognized(_) => {
            return None;
        }
    })
}

/// The terminal reason a `stop_limit` is refused with when its limit is SHORT of its trigger (a buy's
/// below it, a sell's above it). cTrader carries a stop-limit's limit as `slippageInPoints`, an
/// unsigned distance PAST the stop price in the order's direction, so that limit cannot be said on
/// its wire.
pub const STOP_LIMIT_LIMIT_SHORT_OF_THE_TRIGGER: &str = "ctrader: a stop_limit's limit must be at \
     or past its trigger in the order's direction (a buy's at or above it, a sell's at or below it): \
     the venue takes the limit as a slippage distance past the stop price";

/// The terminal reason a `stop_limit` is refused with when its limit is not a whole number of the
/// symbol's points from its trigger, or the count does not fit the venue's `int32` field.
pub const STOP_LIMIT_LIMIT_OFF_THE_POINTS: &str = "ctrader: a stop_limit's limit must be a whole \
     number of the symbol's points (its price step) from its trigger: the venue takes the distance \
     as an integer slippageInPoints";

/// The terminal reason a `stop_limit` is refused with when the handshake read no digits for its
/// symbol: the point its limit distance is counted in is unknown, and never guessed.
pub const STOP_LIMIT_POINT_UNKNOWN: &str = "ctrader: a stop_limit cannot be sent for this symbol: \
     the handshake read no digits for it, so the point its limit distance is counted in is unknown";

/// The terminal reason a `stop_limit` with any lifetime but good-till-cancelled is refused with: this
/// bridge sends no `timeInForce`, so the venue would rest it GTC whatever it asked for.
pub const STOP_LIMIT_LIFETIME_UNSUPPORTED: &str = "ctrader: a stop_limit rests good-till-cancelled \
     here (this bridge sends the venue no time in force), so any other lifetime is refused";

/// The f64 noise allowed, in points, on a limit's distance from its trigger: two prices near 1e5 at
/// five digits subtract with about 3e-6 points of error, and a price really off the grid is off by more.
const POINT_NOISE: f64 = 1e-4;

/// A `stop_limit`'s limit as cTrader's `slippageInPoints`: the distance from `trigger` to `limit` in
/// the symbol's points, counted in the order's direction (`side > 0` a buy, whose limit is at or above
/// the trigger). Refused by name: a limit short of the trigger
/// ([`STOP_LIMIT_LIMIT_SHORT_OF_THE_TRIGGER`]), a distance that is not a whole number of points or does
/// not fit `int32` ([`STOP_LIMIT_LIMIT_OFF_THE_POINTS`], a non-finite price too), and a symbol with no
/// digits on record ([`STOP_LIMIT_POINT_UNKNOWN`]). A limit AT the trigger is 0 points.
///
/// ⚠ The point is read as `10^-digits` (`SymbolMap::scale`, the step `SymbolMap::round_price` snaps
/// to): the proto says only "slippage distance", and no fill has measured it. On a five-digit symbol
/// it is `1e-5` whichever way it is read.
pub fn stop_limit_slippage(
    side: i32,
    trigger: f64,
    limit: f64,
    symbol_id: i64,
    symbols: &SymbolMap,
) -> Result<i32, &'static str> {
    let scale = symbols.scale(symbol_id);
    // `terms` is present exactly when the handshake's `SymbolById` gave the symbol its digits.
    if symbols.terms(symbol_id).is_none() || scale < 1.0 || !scale.is_finite() {
        return Err(STOP_LIMIT_POINT_UNKNOWN);
    }
    let points = (limit - trigger) * f64::from(side.signum()) * scale;
    if points < -POINT_NOISE {
        return Err(STOP_LIMIT_LIMIT_SHORT_OF_THE_TRIGGER);
    }
    let whole = points.round();
    if !points.is_finite() || (points - whole).abs() > POINT_NOISE {
        return Err(STOP_LIMIT_LIMIT_OFF_THE_POINTS);
    }
    i32::try_from(whole as i64).map_err(|_| STOP_LIMIT_LIMIT_OFF_THE_POINTS)
}

/// Build a `ProtoOaNewOrderReq` from a canonical `OrderRequest`. Returns `None` (→ the exec client
/// synthesizes a terminal `OrderRejected`, never a vanished order) when: the symbol is unknown to
/// `symbols`; the `order_type` is unrecognized; `side == 0`; the volume rounds below the symbol's
/// `min_volume`; a limit/stop/stop-limit order has no price; a stop-limit's limit is one
/// [`stop_limit_slippage`] refuses; or a stop-limit asks a lifetime other than GTC
/// ([`STOP_LIMIT_LIFETIME_UNSUPPORTED`]). `order_client_id` is carried in BOTH `label`
/// (wire correlation) and the dedicated `order_client_id` field (FIX ClOrdID twin). Limit/stop
/// prices are the ABSOLUTE price as a `double` (see the module note above — NOT 1e5-scaled), snapped to
/// the symbol's `digits` by `SymbolMap::round_price`. A stop-limit is `STOP_LIMIT` with `stopPrice` the
/// trigger and its limit as `slippageInPoints` ([`stop_limit_slippage`]); it carries no `limitPrice`,
/// which the venue takes on a LIMIT order only.
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
    let volume = round_volume(raw, grid, false)?;

    // Limit needs `price`; Stop needs `trigger_price`; StopLimit needs both. Absent → reject (None),
    // never place a priceless conditional. Market carries neither.
    // The wire price is snapped to the symbol's `digits` (`SymbolMap::round_price`): the gate's
    // tick rounding leaves f64 noise the venue refuses as "more digits than symbol allows".
    let (limit_price, stop_price, slippage_in_points) = match order_type {
        ProtoOaOrderType::Limit => (Some(symbols.round_price(symbol_id, req.price?)), None, None),
        ProtoOaOrderType::Stop => {
            (None, Some(symbols.round_price(symbol_id, req.trigger_price?)), None)
        }
        ProtoOaOrderType::StopLimit => {
            if req.time_in_force != TimeInForce::Gtc {
                return None; // STOP_LIMIT_LIFETIME_UNSUPPORTED
            }
            let trigger = req.trigger_price?;
            let points =
                stop_limit_slippage(req.side, trigger, req.price?, symbol_id, symbols).ok()?;
            (None, Some(symbols.round_price(symbol_id, trigger)), Some(points))
        }
        _ => (None, None, None),
    };

    Some(ProtoOaNewOrderReq {
        ctid_trader_account_id: ctid,
        symbol_id,
        order_type: order_type as i32,
        trade_side: trade_side as i32,
        volume,
        limit_price,
        stop_price,
        slippage_in_points,
        label: Some(req.order_client_id.clone()),
        client_order_id: Some(req.order_client_id.clone()),
        ..Default::default()
    })
}

/// Build the `ProtoOaAmendOrderReq` for a resting `order` (`new_qty` in units → centi-units;
/// `new_price` absolute). Which price field the amend carries depends on the resting order's type
/// (a STOP reprices `stop_price`, every other type the limit leg), mirroring [`order_to_new_order`];
/// the price is snapped to the symbol's `digits` by `SymbolMap::round_price` exactly as a new
/// order's is (an unknown symbol leaves it untouched).
///
/// A STOP_LIMIT's `new_price` is its LIMIT (`vike_model::modified_price_is_trigger`), which the venue
/// holds as `slippageInPoints` past the stop: the amend carries the new distance from the order's
/// trigger ([`stop_limit_slippage`], refused by name as at submit), and RESTATES `stopPrice` (the
/// trigger) and, on a size-only amend, the current distance. The proto does not say what an absent
/// field of an amend means, so a stop-limit amend never leaves either of its two terms to that.
///
/// A new quantity goes through the SAME volume grid a new order does (`round_volume`: to the symbol's
/// `step_volume`, clamped to `[min_volume, max_volume]`), because the venue refuses an off-grid amend
/// (`TRADING_BAD_VOLUME`) exactly as it refuses an off-grid order. An amend whose size falls below the symbol's
/// minimum is `Err` (the caller answers `OrderModifyRejected`), never a sub-minimum volume on the wire; a
/// symbol with no grid on record leaves the size as asked, like a submit.
pub fn amend_order_req(
    order: &OrderRequest,
    ctid: i64,
    order_id: i64,
    new_qty: Option<f64>,
    new_price: Option<f64>,
    symbols: &SymbolMap,
) -> Result<ProtoOaAmendOrderReq, String> {
    let symbol_id = symbols.id_of(&order.symbol);
    let snapped = new_price.map(|p| match symbol_id {
        Some(id) => symbols.round_price(id, p),
        None => p,
    });
    let (limit_price, stop_price, slippage_in_points) = match order.order_type {
        OrderType::Stop => (None, snapped, None),
        OrderType::StopLimit => {
            let id = symbol_id.ok_or(STOP_LIMIT_POINT_UNKNOWN)?;
            let (Some(trigger), Some(limit)) = (order.trigger_price, new_price.or(order.price))
            else {
                return Err("ctrader: a stop_limit amend needs the resting order's trigger and \
                            limit prices"
                    .to_string());
            };
            let points = stop_limit_slippage(order.side, trigger, limit, id, symbols)?;
            (None, Some(symbols.round_price(id, trigger)), Some(points))
        }
        _ => (snapped, None, None), // limit (and default) reprice the limit leg
    };
    let volume = match new_qty {
        None => None,
        Some(q) => {
            let raw = (q * 100.0).round() as i64;
            let grid = symbol_id.and_then(|id| symbols.volume_grid(id)).unwrap_or_default();
            Some(round_volume(raw, grid, true).ok_or_else(|| {
                format!(
                    "ctrader: the new quantity {q} is below the symbol's minimum order volume \
                     ({} units)",
                    grid.min_volume as f64 / 100.0
                )
            })?)
        }
    };
    Ok(ProtoOaAmendOrderReq {
        ctid_trader_account_id: ctid,
        order_id,
        volume,
        limit_price,
        stop_price,
        slippage_in_points,
        ..Default::default()
    })
}

/// The correlation id + venue order id an execution event carries: `order_client_id` from the
/// referenced order (its dedicated field, falling back to the `label` we set at submit), and the
/// venue's numeric `order_id`. Returns `("", 0)`-ish defaults when the order ref is absent.
fn order_correlation(event: &ProtoOaExecutionEvent) -> (String, i64) {
    match &event.order {
        Some(o) => {
            let order_client_id = o
                .client_order_id
                .clone()
                .or_else(|| o.trade_data.label.clone())
                .unwrap_or_default();
            (order_client_id, o.order_id)
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
/// `< 0` = rebate — see that field's doc and `vike-execution/src/account.rs`'s `self.balance -=
/// fill.commission`), so the raw value is NEGATED here, matching the OKX mapper's
/// `commission: -fillFee` pattern.
///
/// **A closing deal of a foreign-quoted pair also carries the venue's conversion fee** (decision 0183: the
/// engine books the quote-currency PnL at the conversion pair's rate, and the venue charges
/// `ProtoOAClosePositionDetail.pnlConversionFee` on top, in the deposit currency): it is added to the fill's
/// `commission` ([`crate::execution::close_detail::conversion_fee_of`], a cost), in the account's own currency like the
/// commission itself (`commission_asset` stays empty), so the engine's balance follows the venue's. A deal
/// with no detail or no fee is exactly what it was.
///
/// `None` for a deal with no `execution_price` or no filled volume (`vike_bridge_core::event_map::fill_is_bookable`):
/// read as 0 it would book the fill at price 0, so neither the bare fill nor its wrap is sent.
pub fn deal_fill(
    event: &ProtoOaExecutionEvent,
    order_client_id: &str,
    symbols: &SymbolMap,
    money_digits: u32,
) -> Option<FillEvent> {
    let deal = event.deal.as_ref()?;
    // The USER name the engine books the fill under (decision 0290), the broker's own for a row no kept row names.
    let symbol = symbols.user_name_of(deal.symbol_id).unwrap_or_default();
    let side = if deal.trade_side == ProtoOaTradeSide::Sell as i32 { -1 } else { 1 };
    // No `execution_price` (or no volume) is refused at the end, by the shared rule's ERROR.
    let last_price_value = deal.execution_price.unwrap_or(0.0);
    let digits = deal.money_digits.unwrap_or(money_digits);
    // Negate: cTrader's wire commission is negative for a charged fee; vike-model's
    // `FillEvent.commission` convention is the opposite (positive = charge/cost).
    let conversion_fee = crate::execution::close_detail::conversion_fee_of(deal, money_digits);
    if conversion_fee != 0.0 {
        // Booked as a cost whatever sign the wire carries, and counted: it is still unmeasured, and a broker
        // that already folds it into `commission` or the gross profit would have it counted twice.
        crate::execution::close_detail::note_unmeasured_booking("pnlConversionFee");
    }
    let commission = -(deal.commission.unwrap_or(0) as f64)
        / crate::execution::close_detail::money_scale(digits)
        + conversion_fee;
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
        order_client_id: order_client_id.to_string(),
        venue: VENUE.into(),
        symbol: symbol.into(),
        side,
        last_qty: deal.filled_volume as f64 / 100.0,
        last_price: last_price_value,
        commission,
        commission_asset: "".into(),
        liquidity_side: Default::default(),
        timestamp: deal.execution_timestamp,
        mark_price: None,
        position_side: Default::default(),
    })
    .filter(vike_bridge_core::event_map::fill_is_bookable)
}

/// **The swap a CLOSING deal realized, as the funding event that books it** (the figure is read by
/// [`crate::execution::close_detail::swap_of`], which states the sign decision): `None` for an opening deal, a close with no swap, a deal whose symbol the
/// list does not carry (an event for a symbol no engine holds routes nowhere) and a swap that is not a finite
/// number. The amount is in the deposit currency, which `symbols` names from the trader's asset (`None` when
/// the handshake did not read it: an engine holding a foreign-quoted symbol then leaves the event out and
/// counts it, an account-quoted one books it as stated).
///
/// Only the swap of a CLOSE is booked. A `SWAP` execution event is the position's accruing, unrealized swap
/// (`ProtoOAPosition.swap` is a running total) and the broker may or may not debit it from the balance before
/// the close; the closing deal's figure is the whole of it either way, so booking both would count it twice and
/// booking the accrual needs a per-position memory that a restart loses. The cost: during a hold the
/// engine's cash does not move by the swap, and a reconcile past its tolerance re-seeds it.
///
/// A deal reaches this function once per execution event; the caller guards a repeated deal id
/// (`crate::execution::positions::CloseTracker::first_swap_of_deal`), because `apply_funding` is not idempotent.
pub fn deal_swap_funding(
    event: &ProtoOaExecutionEvent,
    symbols: &SymbolMap,
    money_digits: u32,
) -> Option<FundingEvent> {
    let deal = event.deal.as_ref()?;
    let amount = crate::execution::close_detail::swap_of(deal, money_digits);
    if amount == 0.0 || !amount.is_finite() {
        return None;
    }
    let symbol = symbols.user_name_of(deal.symbol_id).filter(|s| !s.is_empty())?;
    Some(FundingEvent {
        venue: VENUE.into(),
        symbol: symbol.into(),
        position_side: PositionSide::Both,
        funding_rate: 0.0,
        amount,
        mark_price: None,
        timestamp: deal.execution_timestamp,
        route_key: None,
        currency: symbols.deposit_currency().map(Into::into),
    })
}

/// Timestamp for a non-fill lifecycle event: the order's last-update time, else wall clock.
pub fn lifecycle_timestamp(event: &ProtoOaExecutionEvent) -> i64 {
    event.order.as_ref().and_then(|o| o.utc_last_update_timestamp).unwrap_or_else(now_ms)
}

/// The confirmed post-amend TOTAL qty/price an `ORDER_REPLACED` event's referenced order carries.
/// `trade_data.volume` is the authoritative new volume in centi-units (`/100.0` → units, same
/// unit convention as [`order_to_new_order`]'s qty→volume path). The new price is `limit_price`
/// for a LIMIT order, `stop_price` for a STOP order — falling back to whichever is present for any
/// other order type, since only one of the two is ever populated in practice. A STOP_LIMIT's is its
/// LIMIT (the engine's column for a stop-limit's modified price, `vike_model::modified_price_is_trigger`),
/// rebuilt from the venue's own terms by [`stop_limit_price`], and never its stop price.
fn order_modified_terms(order: &ProtoOaOrder, symbols: &SymbolMap) -> (Option<f64>, Option<f64>) {
    let new_qty = Some(order.trade_data.volume as f64 / 100.0);
    let new_price = match ProtoOaOrderType::try_from(order.order_type) {
        Ok(ProtoOaOrderType::Limit) => order.limit_price,
        Ok(ProtoOaOrderType::Stop) => order.stop_price,
        Ok(ProtoOaOrderType::StopLimit) => stop_limit_price(order, symbols),
        _ => order.limit_price.or(order.stop_price),
    };
    (new_qty, new_price)
}

/// A resting STOP_LIMIT's limit from the venue's own terms: `stopPrice` plus `slippageInPoints` of the
/// symbol's points in the order's direction (the inverse of [`stop_limit_slippage`]), snapped to the
/// symbol's digits. `None` when either term is absent or the symbol's point is unknown.
pub(crate) fn stop_limit_price(order: &ProtoOaOrder, symbols: &SymbolMap) -> Option<f64> {
    let id = order.trade_data.symbol_id;
    let scale = symbols.scale(id);
    if symbols.terms(id).is_none() || scale < 1.0 || !scale.is_finite() {
        return None;
    }
    let side =
        if order.trade_data.trade_side == ProtoOaTradeSide::Sell as i32 { -1.0 } else { 1.0 };
    let limit = order.stop_price? + side * order.slippage_in_points? as f64 / scale;
    Some(symbols.round_price(id, limit))
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
/// submit. `order_client_id` is pulled from the referenced order; fill prices/volumes/commission
/// are descaled via [`deal_fill`], which prefers the deal's own `money_digits` and falls back to
/// `money_digits` (the account's `ProtoOATrader.money_digits`, fetched at connect/reconnect — see
/// `transport::connection::open_and_handshake`) only when the deal omits it.
pub fn execution_event_to_events(
    event: &ProtoOaExecutionEvent,
    symbols: &SymbolMap,
    money_digits: u32,
) -> Vec<Event> {
    let Ok(exec_type) = ProtoOaExecutionType::try_from(event.execution_type) else {
        return Vec::new();
    };
    let (order_client_id, venue_order_id) = order_correlation(event);
    let voi = || Some(venue_order_id.to_string().into());
    match exec_type {
        ProtoOaExecutionType::OrderAccepted => vec![Event::OrderAccepted(OrderAccepted {
            order_client_id,
            venue_order_id: voi(),
            timestamp: lifecycle_timestamp(event),
        })],
        // DUAL-PUBLISH (venue-contract, `vike_bridge_core::terminal_events`): `Event::Fill` FIRST (the
        // core `Account` folds it into position/PnL/trades), then the `OrderFilled` /
        // `OrderPartiallyFilled` wrap that drives the OMS order FSM. Emitting only the wrap leaves
        // `Account` blind.
        ProtoOaExecutionType::OrderFilled | ProtoOaExecutionType::OrderPartialFill => {
            match deal_fill(event, &order_client_id, symbols, money_digits) {
                Some(fill) => {
                    let ts = fill.timestamp;
                    let is_filled = exec_type == ProtoOaExecutionType::OrderFilled;
                    vike_bridge_core::terminal_events(order_client_id, fill, ts, is_filled)
                }
                None => Vec::new(),
            }
        }
        ProtoOaExecutionType::OrderCancelled => vec![Event::OrderCanceled(OrderCanceled {
            order_client_id,
            reason: event.error_code.clone().unwrap_or_default().into(),
            timestamp: lifecycle_timestamp(event),
            post_only_would_cross: false,
            refusal_id: None,
        })],
        ProtoOaExecutionType::OrderRejected => vec![Event::OrderRejected(OrderRejected {
            cause: None,
            order_client_id,
            reason: event.error_code.clone().unwrap_or_default().into(),
            timestamp: lifecycle_timestamp(event),
            refusal_id: vike_model::events::refusal_id_of(
                vike_model::OrderRefusalId::VenueRefused.word(),
            ),
        })],
        ProtoOaExecutionType::OrderReplaced => {
            let (new_qty, new_price) = event
                .order
                .as_ref()
                .map(|o| order_modified_terms(o, symbols))
                .unwrap_or((None, None));
            vec![Event::OrderModified(OrderModified {
                order_client_id,
                venue_order_id: voi(),
                new_qty,
                new_price,
                new_trigger_price: None,
                timestamp: lifecycle_timestamp(event),
            })]
        }
        ProtoOaExecutionType::OrderExpired => {
            vec![Event::OrderExpired(OrderExpired {
                order_client_id,
                timestamp: lifecycle_timestamp(event),
            })]
        }
        ProtoOaExecutionType::OrderCancelRejected => {
            vec![Event::OrderCancelRejected(OrderCancelRejected {
                order_client_id,
                reason: event.error_code.clone().unwrap_or_default().into(),
                timestamp: lifecycle_timestamp(event),
                refusal_id: vike_model::events::refusal_id_of(
                    vike_model::OrderRefusalId::VenueRefused.word(),
                ),
            })]
        }
        // SWAP / DEPOSIT_WITHDRAW / BONUS_* — not an order-lifecycle transition this seam models.
        // Left unemitted (no vanished ORDER, since these carry no order state change the OMS FSM
        // tracks).
        _ => Vec::new(),
    }
}

/// Whether an execution event drives its order to a DEFINITELY-TERMINAL state — one from which no
/// further lifecycle event can arrive for that `order_client_id`. FILLED (a full fill), CANCELLED,
/// REJECTED, and EXPIRED are terminal; ACCEPTED and PARTIAL_FILL leave the order LIVE (more events
/// to come), and REPLACED/CANCEL_REJECTED also leave it live (still working). Non-order-lifecycle
/// types (SWAP/DEPOSIT/BONUS/…) and unrecognized codes are not terminal. Single source of truth for
/// the `execution_type` classification the conn layer uses to prune its order client id→orderId map (so it does
/// NOT re-derive the same match) — mirrors the terminal arms of [`execution_event_to_events`].
pub fn execution_event_is_terminal(event: &ProtoOaExecutionEvent) -> bool {
    matches!(
        ProtoOaExecutionType::try_from(event.execution_type),
        Ok(ProtoOaExecutionType::OrderFilled
            | ProtoOaExecutionType::OrderCancelled
            | ProtoOaExecutionType::OrderRejected
            | ProtoOaExecutionType::OrderExpired)
    )
}

#[path = "decode_tests.rs"]
#[cfg(test)]
mod decode_tests;
