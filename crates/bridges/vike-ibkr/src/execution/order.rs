//! Order mapping: vike `OrderRequest` → an `IbkrOrderSpec` (the plain field set the socket backend
//! hands to `ibapi::Order`), plus the typed IB order-status enum with `is_terminal()`/`is_active()`
//! that drives inflight-map cleanup in the event mapper (Task 7). Pure + fixture-tested; no I/O.
//!
//! A kind this bridge builds no IB order for (a word outside the six, a kind on a product IB does not
//! take it for, an auction lifetime on a kind that has none) is REFUSED by [`map_order_request`] in
//! words, never sent: it used to go out as a MARKET order.
//!
//! The wave-2 kinds, in TWS's codes (`https://interactivebrokers.github.io/tws-api/basic_orders.html`,
//! and IBKR's current reference `https://www.ibkrguides.com/traderworkstation/order-types.htm`):
//! - a `take_profit` with no price is `MIT` (the trigger as `auxPrice`), with one `LIT` (the limit as
//!   `lmtPrice`, the trigger as `auxPrice`): vike's if-touched kind is IB's, a buy below the market;
//! - a `market_to_limit` is `MTL`, unpriced (IB: not on `CASH`);
//! - an AUCTION lifetime is two different things at IB: the open is the TIF `OPG` on a `MKT`/`LMT`
//!   (market-on-open, limit-on-open), the close is the ORDER TYPE `MOC`/`LOC` sent `DAY`. IB takes
//!   both on `STK` and `FUT` only (of the products this bridge claims), and limit-on-open on `STK`
//!   only (IB lists it for CFD, STK, OPT, WAR, not FUT: refused on a future until the paper smoke says
//!   otherwise). IB's `AUC` lifetime is the opening auction's `market_to_limit` (`MTL` + TIF `AUC`, the
//!   attributes wave): its remainder rests on as a limit at the auction's price, sent with no `lmtPrice`;
//! - `OrderRequest::display_qty` is IB's `displaySize` (an iceberg's shown slice), a whole number of
//!   units, on a plain limit order only; a display of ZERO is IB's `hidden` (the order shows nothing),
//!   with `displaySize` unset, on a STOCK only (IB applies it to NASDAQ-routed stock orders).
//!
//! The attributes wave (decision 0267; ibapi 5.2.0's `Order` fields, the TWS API's `basic_orders` and
//! `adjustable_stops` pages). Each is set only when the request carries it, so every other order is the one
//! this mapping always built:
//! - `OrderRequest::attributes` on a plain `LMT`: all-or-none is `allOrNone`, sweep-to-fill `sweepToFill` and a
//!   discretionary amount `discretionaryAmt`, the last two on a STOCK only (IB: STK, CFD, WAR; vike has no CFD);
//! - a `trailing_take_profit` is TWS's `TRAIL MIT` (no `limit_offset`), the trail as `auxPrice`, no `lmtPrice`, no
//!   `trailStopPrice`, on a STOCK only (accepted on the paper gateway 2026-11-09,
//!   `tests/ibkr_smokes/ibkr_attributes_smoke.rs`); one WITH a `limit_offset` would be `TRAIL LIT`, which IB refuses
//!   without a starting stop, so it is refused here (the wave-3 bullets below);
//! - `OrderRequest::trail_percent` is `trailingPercent` in place of the `auxPrice` amount, on the trailing codes
//!   built (`TRAIL`, `TRAIL MIT`: both accepted on paper 2026-11-09; ibapi's `TrailBy::Percent`);
//! - `OrderRequest::adjust` is IB's adjustable stop on a `STP`/`STP LMT` ([`IbkrAdjust`]): `triggerPrice` its
//!   level, `adjustedOrderType` `STP`/`STP LMT`/`TRAIL` and the target's prices, `adjustableTrailingUnit` 0
//!   for an amount and 100 for a percent (ibapi: "Live-verified: TWS takes 0 (amount) and 100 (percent), and
//!   rejects 1"). IB's samples attach it to a parent; a standalone one is the smoke's question;
//! - a TIME release (`OrderRequest::release` with no price condition) that reaches the bridge is IB's
//!   `goodAfterTime`: IB holds it (the owner's answer A of 2026-11-08). The core sends one only to an ACCOUNT
//!   whose bridge holds releases (`crate::config::IbkrConfig::native_time_release`, published per account: the
//!   socket yes, the Client Portal no); a release with a price condition is the core's, always, and refused here
//!   as a net;
//! - a `GTD` order's `gtd_expiry` is `goodTillDate`. Both instants go through ONE formatter, [`ibkr_utc`].
//!
//! The wave-3 trailing kinds (the same two references, and `https://interactivebrokers.github.io/tws-api/order_conditions.html`):
//! - a `trailing` is `TRAIL`, its trail (an amount in price, never a percent) as `auxPrice` ("the trailing
//!   amount ... specified in the auxPrice field"), with no `trailStopPrice`: IB starts the stop from the market, as
//!   vike's trailing law does (accepted on the paper gateway 2026-11-09);
//! - a `trailing_limit` would be `TRAIL LIMIT` (the trail as `auxPrice`, `limit_offset` as `lmtPriceOffset`), and it
//!   is REFUSED, as is a `TRAIL LIT`: IB wants their STARTING stop (`trailStopPrice`; paper 2026-11-09: `[321] Please
//!   enter a stop price`, and ibapi 5.2.0's `trailing_stop_limit` takes one), which only a market price gives, and
//!   this bridge has none when it builds the order;
//! - an ACTIVATION (a trailing kind's `trigger_price`) would be IB's price CONDITION on that order
//!   ([`IbkrOrderSpec::activation`], `transport/socket.rs`'s `activation_condition`, built as IB's own sample builds
//!   one), and it is REFUSED: the `TRAIL` sent behind one on the paper gateway 2026-11-09 got no order status in 20 s
//!   while the same `TRAIL` with none was accepted at once. The transport keeps the condition for the run that proves it.
//!
//! The wave-5 `pegged` order (the same references, and the TWS API's
//! `https://interactivebrokers.github.io/tws-api/basic_orders.html` and `.../ibkrats.html`, read 2026-11-08),
//! never a `lmtPrice` (vike's peg states no cap). Of the four [`OrderPegAnchor`]s only `Mid` is built: it is the one IB
//! answered on the paper gateway (2026-11-09, `tests/ibkr_smokes/ibkr_pegged_smoke.rs`). The offset rides `auxPrice` in
//! IB'S sign, where vike's is positive when aggressive:
//! - `Mid` is `PEG MID` ("Buy order price = Midpoint price - offset"): `auxPrice` is the NEGATED offset;
//! - `Market` (`PEG MKT`) is REFUSED: IB held the one sent with no order status for 20 s (its cancel was answered);
//! - `Primary` (`REL`) is REFUSED: the one sent got no answer to its submit or its cancel;
//! - `Best` (`PEG BEST`, IBKR ATS only) is REFUSED: IB's sample and ibapi 5.2.0's `peg_best` state a limit price
//!   (`lmtPrice`), which a vike peg does not name, and the one sent without it got no answer to its submit or its
//!   cancel.
//!
//! The offset reaches this mapping already on the venue's grid, rounded toward the passive side by the exec
//! (`crate::instrument::tick_grid`'s `peg_offset_on_grid`), and a pegged order priced off delayed data never reaches it
//! (`crate::execution::delayed_order`).

use crate::instrument::contract::{IbkrContract, SecType};
use vike_model::{OrderPegAnchor, OrderRequest, OrderStopAdjustTo, OrderType, TimeInForce};

/// IB order status, typed (replaces IB's magic status strings). `is_terminal()` drives order client id⇄orderId
/// map removal.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum OrderStatusKind {
    ApiPending,
    PendingSubmit,
    PendingCancel,
    PreSubmitted,
    Submitted,
    /// A working order with a warning on it (below).
    WarnState,
    ApiCancelled,
    Cancelled,
    Filled,
    Inactive,
}

impl OrderStatusKind {
    /// The ONE classifier of IB's status words, for both backends and both paths.
    ///
    /// Two words name a live order and came to this table on the owner's ruling of 2026-11-07, each read from IBKR's Web API
    /// reference, the "Order Status Value" table
    /// (`https://www.interactivebrokers.com/docs/web-api/v1/endpoints/order-monitoring/order-status-value`):
    /// - `WarnState`: "Order has a specific warning message such as for basket orders." The order works; it carries a warning.
    /// - `PreCancelled`: "a cancellation request has been accepted by the system but ... currently the request is not being
    ///   recognized, due to system, exchange or other issues." The order is not cancelled: it reads as `PendingCancel`.
    ///
    /// Not status words, and so not here: `Sort_By_Time` and the lower-case `pending_submit` family are the same endpoint's
    /// `filters` values, which that page says differ from the field's format.
    ///
    /// ⚠ **A word truly unknown stays `Inactive`, which reads as over** (the owner's ruling of 2026-10-11: an unknown IBKR
    /// status is never resting; it is pinned in `tests/ibkr_reconcile_parse.rs`).
    pub fn from_ibkr(s: &str) -> OrderStatusKind {
        match s {
            "ApiPending" => OrderStatusKind::ApiPending,
            "PendingSubmit" => OrderStatusKind::PendingSubmit,
            "PendingCancel" | "PreCancelled" => OrderStatusKind::PendingCancel,
            "PreSubmitted" => OrderStatusKind::PreSubmitted,
            "Submitted" => OrderStatusKind::Submitted,
            "WarnState" => OrderStatusKind::WarnState,
            "ApiCancelled" | "ApiCanceled" => OrderStatusKind::ApiCancelled,
            "Cancelled" | "Canceled" => OrderStatusKind::Cancelled,
            "Filled" => OrderStatusKind::Filled,
            _ => OrderStatusKind::Inactive,
        }
    }
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            OrderStatusKind::Filled | OrderStatusKind::Cancelled | OrderStatusKind::ApiCancelled
        )
    }
    pub fn is_active(self) -> bool {
        matches!(
            self,
            OrderStatusKind::PreSubmitted
                | OrderStatusKind::Submitted
                | OrderStatusKind::PendingSubmit
                | OrderStatusKind::WarnState
        )
    }
}

/// The plain field set the socket backend copies onto an `ibapi::Order`. Keeping it a pure struct
/// makes order construction fixture-testable without the ibapi types. `Default` is the empty order: every
/// attribute off, so a literal names only what it sets.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct IbkrOrderSpec {
    pub action: &'static str, // "BUY" | "SELL"
    pub total_qty: f64,
    // "MKT" | "LMT" | "STP" | "STP LMT" | "MIT" | "LIT" | "MTL" | "MOC" | "LOC" | "TRAIL" | "TRAIL MIT" |
    // "PEG MID" (the mapper refuses "TRAIL LIMIT", "TRAIL LIT" and the other pegs: module doc)
    pub order_type: &'static str,
    pub lmt_price: Option<f64>,
    // the trigger of a stop, a stop-limit or an if-touched order; the trail amount of a trailing code (`None` for a
    // percent trail); a pegged order's offset in IB's sign
    pub aux_price: Option<f64>,
    pub tif: &'static str, // "GTC" | "DAY" | "IOC" | "FOK" | "GTD" | "OPG" | "AUC"
    pub order_ref: String, // = order_client_id (round-trips as the order client id tag)
    pub display_size: Option<i32>, // an iceberg's shown slice (`displaySize`); `None` shows it all
    pub lmt_price_offset: Option<f64>, // a TRAIL LIMIT's / TRAIL LIT's `lmtPriceOffset`; `None` on every other order
    /// A trailing order's activation level: the price condition the order is held behind (module doc). `None` on
    /// every other order, and on a trailing order that trails at once.
    pub activation: Option<f64>,
    /// `hidden`: a limit order the venue shows none of (a display of zero).
    pub hidden: bool,
    /// `allOrNone`.
    pub all_or_none: bool,
    /// `sweepToFill`.
    pub sweep_to_fill: bool,
    /// `discretionaryAmt`; `None` leaves IB's 0.
    pub discretionary_amt: Option<f64>,
    /// `trailingPercent`, in place of the `auxPrice` amount of a trailing code.
    pub trailing_percent: Option<f64>,
    /// `goodAfterTime`, [`ibkr_utc`]'s spelling: a time release IB holds.
    pub good_after_time: Option<String>,
    /// `goodTillDate` of a `GTD` order, [`ibkr_utc`]'s spelling.
    pub good_till_date: Option<String>,
    /// IB's adjustable stop.
    pub adjust: Option<IbkrAdjust>,
}

/// IB's adjustable stop on a `STP` / `STP LMT` (`OrderRequest::adjust`): once the price penetrates
/// `trigger_price` the order becomes `order_type` at these prices, once.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct IbkrAdjust {
    /// `triggerPrice`: the adjustment's level.
    pub trigger_price: f64,
    /// `adjustedOrderType`: "STP" | "STP LMT" | "TRAIL".
    pub order_type: &'static str,
    /// `adjustedStopPrice`.
    pub stop_price: f64,
    /// `adjustedStopLimitPrice` of a "STP LMT" target.
    pub stop_limit_price: Option<f64>,
    /// `adjustedTrailingAmount` of a "TRAIL" target: an amount in price, or a percent (`trailing_unit` 100).
    pub trailing_amount: Option<f64>,
    /// `adjustableTrailingUnit`: 0 for an amount, 100 for a percent.
    pub trailing_unit: i32,
}

/// Epoch ms as IB's UTC instant, `yyyyMMdd-HH:mm:ss` (ibapi's `good_till_date` doc: "or UTC
/// `yyyyMMdd-HH:mm:ss`"): the ONE spelling of `goodAfterTime` and `goodTillDate`, to the second (a
/// sub-second rest is dropped, so a release goes no earlier than asked).
pub fn ibkr_utc(ms: i64) -> String {
    let secs = ms.div_euclid(1000) + i64::from(ms.rem_euclid(1000) != 0);
    let (y, mo, d) = vike_utility::calendar::civil_from_days(secs.div_euclid(86_400));
    let s = secs.rem_euclid(86_400);
    format!("{y:04}{mo:02}{d:02}-{:02}:{:02}:{:02}", s / 3600, s % 3600 / 60, s % 60)
}

/// This venue's row of the ONE cross-venue TIF authority ([`vike_venues::venue_time_in_force::venue_time_in_force`]),
/// consumed; a TIF flip is a one-line row edit there. GTD: the socket sends its date
/// (`goodTillDate` from `req.gtd_expiry`, [`ibkr_utc`]), the row mapping it behind the paper smoke;
/// the cpapi order body carries no date field and refuses one by name.
fn time_in_force_of(req: &OrderRequest) -> &'static str {
    vike_venues::venue_time_in_force::venue_time_in_force("ibkr", req.time_in_force)
        .wire()
        .unwrap_or("GTC")
}

/// `price_magnifier` (bonds) divides the display price into IB wire units; 1 for everything else.
fn apply_magnifier(px: f64, price_magnifier: i32) -> f64 {
    if price_magnifier > 1 { px / price_magnifier as f64 } else { px }
}

/// Why a trailing order with a limit offset (`TRAIL LIMIT`, `TRAIL LIT`) is refused (module doc).
fn no_starting_stop(code: &str) -> String {
    format!(
        "IB's `{code}` needs a starting stop price (`trailStopPrice`; on the paper gateway 2026-11-09 IB refused \
         one sent without it: [321] \"Please enter a stop price\"), which is the market price less or plus the \
         trail, and this bridge has no market price when it builds the order; a trailing order with no limit \
         offset (`TRAIL`, `TRAIL MIT`) needs none"
    )
}

/// The IB order for `req`, or why this bridge builds none (the reason of a terminal `OrderRejected`).
/// A stop-limit is TWS's `STP LMT`: `lmtPrice` is the limit (`price`), `auxPrice` the trigger. The
/// wave-2 kinds and the auction lifetimes are the module doc's.
pub fn map_order_request(
    req: &OrderRequest,
    contract: &IbkrContract,
    price_magnifier: i32,
) -> Result<IbkrOrderSpec, String> {
    let action = if req.side >= 0 { "BUY" } else { "SELL" };
    let px = |p: Option<f64>| p.map(|p| apply_magnifier(p, price_magnifier));
    let refused = |why: String| Err(format!("ibkr: {why}; nothing was sent"));
    let product = contract.sec_type.as_ibkr_code();
    let stock = contract.sec_type == SecType::Stk;
    let tif = req.time_in_force;
    // The opening auction's market-to-limit, IB's `AUC` (module doc).
    let auction_to_limit = tif == TimeInForce::AtOpen && req.order_type == OrderType::MarketToLimit;
    if tif.is_auction() {
        if !matches!(req.order_type, OrderType::Market | OrderType::Limit) && !auction_to_limit {
            return refused(format!(
                "the time in force `{}` belongs to a market or a limit order (at-open: or a market_to_limit \
                 one); this is a {} order",
                tif.as_word(),
                req.order_type
            ));
        }
        if !matches!(contract.sec_type, SecType::Stk | SecType::Fut) {
            return refused(format!(
                "IB takes an auction order (`{}`) on a stock or a future only; this contract is a {product}",
                tif.as_word()
            ));
        }
        if tif == TimeInForce::AtOpen && req.order_type == OrderType::Limit && !stock {
            return refused(format!(
                "IB lists a limit-on-open order (`LMT` with TIF `OPG`) for a stock, not a {product}"
            ));
        }
    }
    if req.order_type.is_trailing() && req.trigger_price.is_some() {
        return refused(
            "a trailing order's activation is IB's price condition, and IB did not answer one on the paper \
             gateway 2026-11-09 (a `TRAIL` behind its condition got no order status in 20 s; the same `TRAIL` \
             with none was accepted at once): until a paper run shows IB takes it, this bridge refuses an \
             activation; place the trailing order without one"
                .to_string(),
        );
    }
    let attributes = req.attributes.unwrap_or_default();
    if !stock {
        let stock_only = [
            // IB applies `hidden` to NASDAQ-routed stock orders only: elsewhere it would rest shown.
            (req.display_quantity == Some(0.0), "a hidden order (`hidden`, a display of 0)"),
            (attributes.sweep_to_fill, "a sweep-to-fill order (`sweepToFill`)"),
            (
                attributes.discretionary_amount.is_some(),
                "a discretionary amount (`discretionaryAmt`)",
            ),
            (
                req.order_type == OrderType::TrailingTakeProfit,
                "a trailing if-touched order (`TRAIL MIT` / `TRAIL LIT`)",
            ),
        ];
        if let Some((_, what)) = stock_only.iter().find(|(asked, _)| *asked) {
            return refused(format!(
                "IB takes {what} on a stock only; this contract is a {product}"
            ));
        }
    }
    let (order_type, lmt_price, aux_price) = match &req.order_type {
        OrderType::Market if tif == TimeInForce::AtClose => ("MOC", None, None),
        OrderType::Market => ("MKT", None, None),
        OrderType::Limit if tif == TimeInForce::AtClose => ("LOC", px(req.price), None),
        OrderType::Limit => ("LMT", px(req.price), None),
        OrderType::Stop => ("STP", None, px(req.trigger_price)),
        OrderType::StopLimit => ("STP LMT", px(req.price), px(req.trigger_price)),
        OrderType::TakeProfit if req.price.is_none() => ("MIT", None, px(req.trigger_price)),
        OrderType::TakeProfit => ("LIT", px(req.price), px(req.trigger_price)),
        OrderType::MarketToLimit if contract.sec_type == SecType::Cash => {
            return refused(format!(
                "IB takes no market-to-limit (`MTL`) order on a {product} contract"
            ));
        }
        OrderType::MarketToLimit => ("MTL", None, None),
        OrderType::Trailing => ("TRAIL", None, px(req.trail)),
        OrderType::TrailingLimit => return refused(no_starting_stop("TRAIL LIMIT")),
        OrderType::Pegged => {
            let Some(peg) = req.peg else {
                return refused("a pegged order needs its peg".to_string());
            };
            let unproven = |what: &str, seen: &str| {
                refused(format!(
                    "IB's {what} is not proven: on the paper gateway 2026-11-09 {seen}; until a paper run \
                     shows IB takes it this bridge refuses it (a mid peg, `PEG MID`, works)"
                ))
            };
            let offset = match peg.anchor {
                // `0.0 - x`, not `-x`: an offset of 0 must go out as 0, never as IB's `-0`
                OrderPegAnchor::Mid => 0.0 - peg.offset,
                OrderPegAnchor::Market => {
                    return unproven(
                        "pegged-to-market order (`PEG MKT`)",
                        "IB held one with no order status for 20 s (only its cancel was answered)",
                    );
                }
                OrderPegAnchor::Primary => {
                    return unproven(
                        "relative order (`REL`, the `primary` peg)",
                        "one got no answer to its submit or to its cancel",
                    );
                }
                OrderPegAnchor::Best => {
                    return unproven(
                        "pegged-to-best order (`PEG BEST`, IBKR ATS)",
                        "one got no answer to its submit or to its cancel, and IB's sample and ibapi 5.2.0's \
                         `peg_best` state a limit price (`lmtPrice`), which a vike peg does not name",
                    );
                }
            };
            ("PEG MID", None, px(Some(offset)))
        }
        OrderType::TrailingTakeProfit if req.limit_offset.is_none() => {
            ("TRAIL MIT", None, px(req.trail))
        }
        OrderType::TrailingTakeProfit => return refused(no_starting_stop("TRAIL LIT")),
        // The capability preflight refuses this first; this is the net for a caller with no core in front.
        kind @ OrderType::Unrecognized(_) => {
            return Err(format!(
                "ibkr: the order kind `{}` is not one this bridge builds (market, limit, stop, stop_limit, \
                 take_profit, market_to_limit, trailing, trailing_limit, pegged, trailing_take_profit); \
                 nothing was sent",
                kind.as_str()
            ));
        }
    };
    let trailing = req.order_type.is_trailing();
    // A plain limit (not an auction's) is the one order a display, a hidden order or an attribute rides.
    let plain_limit = order_type == "LMT" && !tif.is_auction();
    let in_auction = if tif.is_auction() { " in an auction" } else { "" };
    // A display of zero is IB's `hidden` (module doc), with `displaySize` unset.
    let hidden = req.display_quantity == Some(0.0);
    let display_size = match req.display_quantity {
        None => None,
        Some(_) if !plain_limit => {
            return refused(format!(
                "{} is sent on a plain limit order only; this is IB's `{order_type}`{in_auction}",
                if hidden {
                    "a hidden order (a display of 0)"
                } else {
                    "a display size (an iceberg)"
                }
            ));
        }
        Some(_) if hidden => None,
        Some(shown) if shown >= 1.0 && shown.fract() == 0.0 && shown <= f64::from(i32::MAX) => {
            Some(shown as i32)
        }
        Some(shown) => {
            return refused(format!(
                "the display size {shown} is not a whole number of units of at least 1 (IB's `displaySize`), \
                 nor 0 (a hidden order)"
            ));
        }
    };
    if req.attributes.is_some() && !plain_limit {
        return refused(format!(
            "an order attribute (all-or-none, sweep-to-fill, discretion) is sent on a plain limit order \
             only; this is IB's `{order_type}`{in_auction}"
        ));
    }
    let adjust = match &req.adjust {
        None => None,
        Some(_) if !matches!(order_type, "STP" | "STP LMT") => {
            return refused(format!(
                "an adjustment is sent on a stop or a stop-limit order only (IB's adjustable stop); this is \
                 IB's `{order_type}`"
            ));
        }
        Some(a) => {
            let (target, stop_limit_price, trailing_amount, trailing_unit) = match a.to {
                OrderStopAdjustTo::Stop { .. } => ("STP", None, None, 0),
                OrderStopAdjustTo::StopLimit { limit, .. } => ("STP LMT", px(Some(limit)), None, 0),
                OrderStopAdjustTo::Trailing { trail_percent: Some(p), .. } => {
                    ("TRAIL", None, Some(p), 100)
                }
                OrderStopAdjustTo::Trailing { trail: Some(t), .. } => {
                    ("TRAIL", None, px(Some(t)), 0)
                }
                OrderStopAdjustTo::Trailing { .. } => {
                    return refused("an adjustment to a trailing stop needs its trail".to_string());
                }
            };
            Some(IbkrAdjust {
                trigger_price: apply_magnifier(a.at, price_magnifier),
                order_type: target,
                stop_price: apply_magnifier(a.to.stop(), price_magnifier),
                stop_limit_price,
                trailing_amount,
                trailing_unit,
            })
        }
    };
    let good_after_time = match req.release {
        None => None,
        Some(release) if release.price.is_some() => {
            return refused(
                "a release with a price condition is held by the core and never sent to IB (its \
                 `goodAfterTime` is a time alone); this one reached the bridge"
                    .to_string(),
            );
        }
        Some(release) => Some(ibkr_utc(release.at)),
    };
    // The open is IB's TIF `OPG` (an opening market-to-limit's `AUC`); the close is the order type (`MOC`/`LOC`,
    // above), sent DAY.
    let wire_time_in_force = match tif {
        TimeInForce::AtOpen if auction_to_limit => "AUC",
        TimeInForce::AtOpen => "OPG",
        TimeInForce::AtClose => "DAY",
        _ => time_in_force_of(req),
    };
    let good_till_date = match (wire_time_in_force, req.gtd_expiry) {
        ("GTD", Some(expiry)) => Some(ibkr_utc(expiry)),
        ("GTD", None) => {
            return refused(
                "a good-till-date order needs its expiry (IB's `goodTillDate`)".to_string(),
            );
        }
        _ => None,
    };
    Ok(IbkrOrderSpec {
        action,
        total_qty: req.qty,
        order_type,
        lmt_price,
        aux_price,
        tif: wire_time_in_force,
        order_ref: req.order_client_id.clone(),
        display_size,
        // The two codes with a limit offset and every activation are refused above, until a paper run proves them.
        lmt_price_offset: None,
        activation: None,
        hidden,
        all_or_none: attributes.all_or_none,
        sweep_to_fill: attributes.sweep_to_fill,
        discretionary_amt: px(attributes.discretionary_amount),
        trailing_percent: if trailing { req.trail_percent } else { None },
        good_after_time,
        good_till_date,
        adjust,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::instrument::contract::parse_simplified;
    use vike_model::OrderRequest;

    fn req(side: i32, ty: &str, price: Option<f64>) -> OrderRequest {
        OrderRequest {
            order_client_id: "order-client-id-1".into(),
            venue: "ibkr".into(),
            symbol: "AAPL.SMART.USD".into(),
            side,
            qty: 10.0,
            order_type: vike_model::OrderType::from_stored(ty),
            price,
            trigger_price: None,
            ..Default::default()
        }
    }

    #[test]
    fn market_buy_maps() {
        let c = parse_simplified("AAPL.SMART.USD").unwrap();
        let spec = map_order_request(&req(1, "market", None), &c, 1).unwrap();
        assert_eq!(spec.action, "BUY");
        assert_eq!(spec.order_type, "MKT");
        assert_eq!(spec.total_qty, 10.0);
        assert_eq!(spec.order_ref, "order-client-id-1"); // order_client_id rides IB orderRef
        assert!(spec.lmt_price.is_none());
    }

    #[test]
    fn limit_sell_maps_price() {
        let c = parse_simplified("AAPL.SMART.USD").unwrap();
        let spec = map_order_request(&req(-1, "limit", Some(191.25)), &c, 1).unwrap();
        assert_eq!(spec.action, "SELL");
        assert_eq!(spec.order_type, "LMT");
        assert_eq!(spec.lmt_price, Some(191.25));
    }

    #[test]
    fn stop_maps_aux_price() {
        let c = parse_simplified("AAPL.SMART.USD").unwrap();
        let mut r = req(-1, "stop", None);
        r.trigger_price = Some(180.0);
        let spec = map_order_request(&r, &c, 1).unwrap();
        assert_eq!(spec.order_type, "STP");
        assert_eq!(spec.aux_price, Some(180.0));
    }

    /// A stop-limit is TWS's `STP LMT`: the limit rides `lmtPrice`, the trigger `auxPrice`.
    #[test]
    fn stop_limit_maps_the_limit_and_the_trigger() {
        let c = parse_simplified("AAPL.SMART.USD").unwrap();
        let mut r = req(1, "stop_limit", Some(201.5));
        r.trigger_price = Some(200.0);
        let spec = map_order_request(&r, &c, 1).unwrap();
        assert_eq!(spec.order_type, "STP LMT");
        assert_eq!(spec.lmt_price, Some(201.5), "the limit is the order's price");
        assert_eq!(spec.aux_price, Some(200.0), "the trigger is the aux price");
        assert_eq!(
            (spec.action, spec.tif, spec.order_ref.as_str()),
            ("BUY", "GTC", "order-client-id-1")
        );
    }

    /// A take-profit is IB's if-touched order: `MIT` with no price (the trigger as `auxPrice`, no `lmtPrice`), `LIT`
    /// with one (the limit as `lmtPrice`, the trigger as `auxPrice`), each under the lifetime it was given.
    #[test]
    fn a_take_profit_is_mit_without_a_price_and_lit_with_one() {
        let c = parse_simplified("AAPL.SMART.USD").unwrap();
        let mut mit = req(1, "take_profit", None);
        mit.trigger_price = Some(180.0);
        let spec = map_order_request(&mit, &c, 1).unwrap();
        assert_eq!((spec.order_type, spec.lmt_price, spec.aux_price), ("MIT", None, Some(180.0)));
        assert_eq!((spec.action, spec.tif, spec.display_size), ("BUY", "GTC", None));

        let mut lit = req(-1, "take_profit", Some(221.0));
        lit.trigger_price = Some(220.0);
        let spec = map_order_request(&lit, &c, 1).unwrap();
        assert_eq!(
            (spec.order_type, spec.lmt_price, spec.aux_price),
            ("LIT", Some(221.0), Some(220.0))
        );
        assert_eq!(spec.action, "SELL");
    }

    /// A market-to-limit is IB's `MTL`, unpriced; IB takes none on a currency pair, so a `CASH` one is refused by name.
    #[test]
    fn a_market_to_limit_is_mtl_and_is_refused_on_a_currency_pair() {
        let c = parse_simplified("AAPL.SMART.USD").unwrap();
        let spec = map_order_request(&req(1, "market_to_limit", None), &c, 1).unwrap();
        assert_eq!((spec.order_type, spec.lmt_price, spec.aux_price), ("MTL", None, None));
        assert_eq!(spec.tif, "GTC");

        let fx = parse_simplified("EUR.USD.IDEALPRO").unwrap();
        let why = map_order_request(&req(1, "market_to_limit", None), &fx, 1).expect_err("CASH");
        assert!(
            why.contains("`MTL`") && why.contains("CASH") && why.contains("nothing was sent"),
            "{why}"
        );
    }

    /// **The auction lifetimes**: the open is TIF `OPG` on a `MKT` or `LMT` (market- and limit-on-open) and TIF `AUC` on
    /// an `MTL`, the close is the ORDER TYPE `MOC`/`LOC` sent `DAY`. Any other kind, a product IB takes no auction order on,
    /// and a limit-on-open on a future are refused by name.
    #[test]
    fn the_auction_lifetimes_are_opg_and_moc_or_loc() {
        use vike_model::TimeInForce::{AtClose, AtOpen};
        let c = parse_simplified("AAPL.SMART.USD").unwrap();
        let built = |ty: &str, price: Option<f64>, tif| {
            let mut r = req(1, ty, price);
            r.time_in_force = tif;
            let spec = map_order_request(&r, &c, 1).unwrap();
            (spec.order_type, spec.lmt_price, spec.tif)
        };
        assert_eq!(built("market", None, AtOpen), ("MKT", None, "OPG"));
        assert_eq!(built("limit", Some(150.0), AtOpen), ("LMT", Some(150.0), "OPG"));
        assert_eq!(built("market", None, AtClose), ("MOC", None, "DAY"));
        assert_eq!(built("limit", Some(150.0), AtClose), ("LOC", Some(150.0), "DAY"));
        let es = parse_simplified("ES.GLOBEX.USD.FUT.20251219").unwrap();
        let mut moc = req(-1, "market", None);
        moc.time_in_force = AtClose;
        assert_eq!(
            map_order_request(&moc, &es, 1).unwrap().order_type,
            "MOC",
            "a future takes one"
        );

        let mut stop = req(1, "stop", None);
        stop.trigger_price = Some(180.0);
        stop.time_in_force = AtOpen;
        let why = map_order_request(&stop, &c, 1).expect_err("a stop has no auction");
        assert!(why.contains("`at-open`") && why.contains("stop"), "{why}");
        let fx = parse_simplified("EUR.USD.IDEALPRO").unwrap();
        let mut moo = req(1, "market", None);
        moo.time_in_force = AtOpen;
        let why = map_order_request(&moo, &fx, 1).expect_err("CASH has no auction");
        assert!(why.contains("CASH") && why.contains("nothing was sent"), "{why}");

        // The opening auction's market-to-limit is IB's `MTL` with TIF `AUC`, unpriced, on a stock or a future; the
        // close has none, and a limit-on-open on a future is refused (IB lists LOO for stocks, not futures).
        assert_eq!(built("market_to_limit", None, AtOpen), ("MTL", None, "AUC"));
        let mut auc = req(1, "market_to_limit", None);
        auc.time_in_force = AtOpen;
        assert_eq!(map_order_request(&auc, &es, 1).unwrap().tif, "AUC", "a future takes one");
        auc.time_in_force = AtClose;
        let why = map_order_request(&auc, &c, 1).expect_err("no closing market-to-limit");
        assert!(why.contains("`at-close`") && why.contains("market_to_limit"), "{why}");
        let mut loo = req(1, "limit", Some(5000.0));
        loo.time_in_force = AtOpen;
        let why = map_order_request(&loo, &es, 1).expect_err("no LOO on a future");
        assert!(
            why.contains("`OPG`") && why.contains("FUT") && why.contains("nothing was sent"),
            "{why}"
        );
        let mut moo_es = req(1, "market", None);
        moo_es.time_in_force = AtOpen;
        assert_eq!(map_order_request(&moo_es, &es, 1).unwrap().tif, "OPG", "a future's MOO stays");

        // The venue_time_in_force row admits a lifetime; where it maps one, its wire is the TIF this bridge sends.
        for (tif, ty) in [(AtOpen, "market"), (AtClose, "limit")] {
            if let Some(wire) =
                vike_venues::venue_time_in_force::venue_time_in_force("ibkr", tif).wire()
            {
                assert_eq!(built(ty, Some(1.0), tif).2, wire, "{tif:?}");
            }
        }
    }

    /// **A display size is IB's `displaySize`**, a whole number of units on a plain limit order; `None` leaves it
    /// unset (every order before the field), and 0 is a hidden order (the next test). Anything else is refused by name,
    /// never sent at its full size.
    #[test]
    fn a_display_size_rides_a_plain_limit_order_as_a_whole_number() {
        let c = parse_simplified("AAPL.SMART.USD").unwrap();
        let mut iceberg = req(1, "limit", Some(150.0));
        iceberg.display_quantity = Some(2.0);
        let spec = map_order_request(&iceberg, &c, 1).unwrap();
        assert_eq!((spec.display_size, spec.hidden), (Some(2), false));
        assert_eq!(
            map_order_request(&req(1, "limit", Some(150.0)), &c, 1).unwrap().display_size,
            None
        );

        for shown in [-1.0, 1.5, f64::NAN, 1e12] {
            let r = OrderRequest { display_quantity: Some(shown), ..iceberg.clone() };
            let why = map_order_request(&r, &c, 1).expect_err("not a whole display");
            assert!(
                why.contains("displaySize") && why.contains("nothing was sent"),
                "{shown}: {why}"
            );
        }
        let market = OrderRequest { display_quantity: Some(2.0), ..req(1, "market", None) };
        assert!(map_order_request(&market, &c, 1).expect_err("market").contains("`MKT`"));
        let loo = OrderRequest { time_in_force: vike_model::TimeInForce::AtOpen, ..iceberg };
        assert!(map_order_request(&loo, &c, 1).expect_err("auction").contains("in an auction"));
    }

    /// **The trailing kinds** (wave 3, as the paper gateway answered them on 2026-11-09): a `trailing` is `TRAIL`, the trail
    /// as `auxPrice`, no `lmtPrice`, no activation. A `trailing_limit` (`TRAIL LIMIT`) is refused by name: IB wants its
    /// starting stop (`[321] Please enter a stop price`), which this bridge cannot know. A trailing order with an
    /// activation (its `trigger_price`, IB's price condition) is refused by name, whatever its kind. Every other kind keeps
    /// both fields `None`, a stop's trigger and a backtest-only `trail` included, so its order is the one it always was.
    #[test]
    fn a_trailing_is_trail_and_a_trailing_limit_or_an_activation_is_refused() {
        let c = parse_simplified("AAPL.SMART.USD").unwrap();
        let mut trailing = req(-1, "trailing", None);
        trailing.trail = Some(2.5);
        let spec = map_order_request(&trailing, &c, 1).unwrap();
        assert_eq!(
            (
                spec.order_type,
                spec.lmt_price,
                spec.aux_price,
                spec.lmt_price_offset,
                spec.activation
            ),
            ("TRAIL", None, Some(2.5), None, None)
        );
        assert_eq!((spec.action, spec.tif, spec.display_size), ("SELL", "GTC", None));

        let mut trailing_limit = req(1, "trailing_limit", None);
        trailing_limit.trail = Some(2.5);
        trailing_limit.limit_offset = Some(0.25);
        let why = map_order_request(&trailing_limit, &c, 1).expect_err("no starting stop");
        assert!(
            why.contains("`TRAIL LIMIT`")
                && why.contains("trailStopPrice")
                && why.contains("Please enter a stop price")
                && why.contains("no market price")
                && why.contains("nothing was sent"),
            "{why}"
        );
        for activated in [
            OrderRequest { trigger_price: Some(170.0), ..trailing.clone() },
            OrderRequest { trigger_price: Some(170.0), ..trailing_limit },
            OrderRequest {
                trigger_price: Some(170.0),
                order_type: OrderType::TrailingTakeProfit,
                ..trailing.clone()
            },
        ] {
            let why = map_order_request(&activated, &c, 1).expect_err("an activation");
            assert!(
                why.contains("activation")
                    && why.contains("price condition")
                    && why.contains("nothing was sent"),
                "{why}"
            );
        }

        let mut stop = req(-1, "stop", None);
        stop.trigger_price = Some(180.0);
        stop.trail = Some(2.5);
        let spec = map_order_request(&stop, &c, 1).unwrap();
        assert_eq!(
            (spec.order_type, spec.aux_price, spec.lmt_price_offset, spec.activation),
            ("STP", Some(180.0), None, None)
        );
    }

    /// **The pegged order** (wave 5, the module doc, as the paper gateway answered it on 2026-11-09): a mid peg is `PEG MID`,
    /// no `lmtPrice`, the offset NEGATED into `auxPrice` (IB's positive offset is passive there), a zero offset never
    /// negative. Every other anchor (`PEG MKT`, `REL`, `PEG BEST`, on any route) is refused by name with what the gateway
    /// did; a pegged order with no peg is refused, never sent as something else.
    #[test]
    fn a_mid_peg_is_peg_mid_with_its_offset_in_ibs_sign_and_every_other_anchor_is_refused() {
        use vike_model::{OrderPeg, OrderPegAnchor};
        let c = parse_simplified("AAPL.SMART.USD").unwrap();
        let ats = parse_simplified("AAPL.IBKRATS.USD.STK").unwrap();
        let pegged = |side, anchor, offset| OrderRequest {
            peg: Some(OrderPeg { anchor, offset }),
            ..req(side, "pegged", None)
        };
        let built = |r: &OrderRequest, c| {
            let spec = map_order_request(r, c, 1).unwrap();
            (spec.order_type, spec.lmt_price, spec.aux_price)
        };
        assert_eq!(
            built(&pegged(1, OrderPegAnchor::Mid, 0.02), &c),
            ("PEG MID", None, Some(-0.02))
        );
        assert_eq!(
            built(&pegged(-1, OrderPegAnchor::Mid, -0.05), &c),
            ("PEG MID", None, Some(0.05))
        );
        let zero = built(&pegged(1, OrderPegAnchor::Mid, 0.0), &c).2.unwrap();
        assert!(zero == 0.0 && zero.is_sign_positive(), "a zero offset is +0, got {zero}");
        let spec = map_order_request(&pegged(1, OrderPegAnchor::Mid, 0.01), &c, 1).unwrap();
        assert_eq!(
            (spec.action, spec.tif, spec.display_size, spec.activation),
            ("BUY", "GTC", None, None)
        );

        for (anchor, code, route) in [
            (OrderPegAnchor::Market, "`PEG MKT`", &c),
            (OrderPegAnchor::Primary, "`REL`", &c),
            (OrderPegAnchor::Best, "`PEG BEST`", &ats),
            (OrderPegAnchor::Best, "`PEG BEST`", &c),
        ] {
            let why = map_order_request(&pegged(1, anchor, 0.01), route, 1).expect_err(code);
            assert!(
                why.contains(code)
                    && why.contains("not proven")
                    && why.contains("2026-11-09")
                    && why.contains("`PEG MID`")
                    && why.contains("nothing was sent"),
                "{why}"
            );
        }
        let bare = req(1, "pegged", None);
        let why = map_order_request(&bare, &c, 1).expect_err("no peg");
        assert!(why.contains("needs its peg") && why.contains("nothing was sent"), "{why}");
    }

    /// A kind this bridge does not build is refused in words, never sent as MARKET (the old `_ => MKT`).
    #[test]
    fn a_kind_this_bridge_does_not_build_is_refused_by_name_not_sent_as_market() {
        let c = parse_simplified("AAPL.SMART.USD").unwrap();
        for word in ["moc", "trail"] {
            let mut r = req(1, word, Some(190.0));
            r.trigger_price = Some(189.0);
            let why = map_order_request(&r, &c, 1).expect_err(word);
            assert!(
                why.contains(&format!("`{word}`")) && why.contains("nothing was sent"),
                "{why}"
            );
        }
    }

    /// Equivalence gate for the `venue_time_in_force` routing: the four recorded IB TIF strings, asserted
    /// BOTH through the mapped spec and against this venue's row of the cross-venue table
    /// (byte-for-byte). Post-only is refused BY NAME before the wire (`deny_unsupported_time_in_force`: IB has no
    /// such order). Good-till-date: the socket sends its date (`goodTillDate`, [`ibkr_utc`]) where the row
    /// maps `GTD` (it does, behind the paper smoke), and a GTD with no expiry is refused; were the row
    /// `Unsupported` the submit path would refuse it by name. The operator can ask for both since
    /// `trade order submit --tif/--post-only`.
    #[test]
    fn time_in_force_matches_the_venue_time_in_force_row() {
        use vike_model::TimeInForce::{Day, Fok, Gtc, Gtd, Ioc, PostOnly};
        let c = parse_simplified("AAPL.SMART.USD").unwrap();
        for (tif, want) in [(Gtc, "GTC"), (Ioc, "IOC"), (Fok, "FOK"), (Day, "DAY")] {
            let mut r = req(1, "limit", Some(100.0));
            r.time_in_force = tif;
            let spec = map_order_request(&r, &c, 1).unwrap();
            assert_eq!((spec.tif, spec.good_till_date), (want, None), "{tif:?}");
            assert_eq!(
                vike_venues::venue_time_in_force::venue_time_in_force("ibkr", tif).wire(),
                Some(want),
                "table row {tif:?}"
            );
        }
        let mut post_only = req(1, "limit", Some(100.0));
        post_only.time_in_force = PostOnly;
        assert_eq!(
            vike_venues::venue_time_in_force::venue_time_in_force("ibkr", PostOnly).wire(),
            None
        );
        assert!(
            vike_venues::venue_time_in_force::deny_unsupported_time_in_force("ibkr", &post_only)
                .is_some()
        );

        let gtd = OrderRequest {
            time_in_force: Gtd,
            gtd_expiry: Some(1_794_234_600_000),
            ..req(1, "limit", Some(100.0))
        };
        match vike_venues::venue_time_in_force::venue_time_in_force("ibkr", Gtd).wire() {
            Some(wire) => {
                let spec = map_order_request(&gtd, &c, 1).unwrap();
                assert_eq!(
                    (spec.tif, spec.good_till_date.as_deref()),
                    (wire, Some("20261109-14:30:00"))
                );
                let dateless = OrderRequest { gtd_expiry: None, ..gtd.clone() };
                let why = map_order_request(&dateless, &c, 1).expect_err("no expiry");
                assert!(why.contains("goodTillDate") && why.contains("nothing was sent"), "{why}");
            }
            None => assert!(
                vike_venues::venue_time_in_force::deny_unsupported_time_in_force("ibkr", &gtd)
                    .is_some(),
                "an unmapped GTD is refused by name before it reaches the wire"
            ),
        }
    }

    /// **IB's UTC instant**: `yyyyMMdd-HH:mm:ss`, a sub-second rest rounded UP (a release goes no earlier than asked).
    #[test]
    fn ibkr_utc_spells_the_instant_to_the_second_rounding_up() {
        assert_eq!(ibkr_utc(0), "19700101-00:00:00");
        assert_eq!(ibkr_utc(1_794_234_600_000), "20261109-14:30:00");
        assert_eq!(ibkr_utc(1_794_234_600_001), "20261109-14:30:01");
        assert_eq!(ibkr_utc(1_803_859_199_000), "20270228-23:59:59");
        assert_eq!(ibkr_utc(1_803_859_199_500), "20270301-00:00:00");
    }

    /// **The attributes ride a plain limit**: all-or-none is `allOrNone`, sweep-to-fill `sweepToFill` and a
    /// discretionary amount `discretionaryAmt`; the last two on a stock only (IB: STK), all-or-none on any product. On
    /// anything but a plain limit (a market, an auction's) they are refused by name, never dropped; and an order with
    /// none keeps every new field empty (the order it always was).
    #[test]
    fn the_attributes_ride_a_plain_limit_and_two_of_them_a_stock_only() {
        use vike_model::OrderAttributes;
        let c = parse_simplified("AAPL.SMART.USD").unwrap();
        let es = parse_simplified("ES.GLOBEX.USD.FUT.20251219").unwrap();
        let with = |attributes: OrderAttributes, ty: &str| OrderRequest {
            attributes: Some(attributes),
            ..req(1, ty, Some(150.0))
        };
        let all = OrderAttributes {
            all_or_none: true,
            sweep_to_fill: true,
            discretionary_amount: Some(0.5),
        };
        let spec = map_order_request(&with(all, "limit"), &c, 1).unwrap();
        assert_eq!(
            (spec.order_type, spec.all_or_none, spec.sweep_to_fill, spec.discretionary_amt),
            ("LMT", true, true, Some(0.5))
        );
        let plain = map_order_request(&req(1, "limit", Some(150.0)), &c, 1).unwrap();
        assert_eq!(
            plain,
            IbkrOrderSpec {
                action: "BUY",
                total_qty: 10.0,
                order_type: "LMT",
                lmt_price: Some(150.0),
                tif: "GTC",
                order_ref: "order-client-id-1".into(),
                ..Default::default()
            },
            "an order with no new term sets none"
        );

        let aon = OrderAttributes { all_or_none: true, ..Default::default() };
        assert!(map_order_request(&with(aon, "limit"), &es, 1).unwrap().all_or_none);
        for (one, word) in [
            (OrderAttributes { sweep_to_fill: true, ..Default::default() }, "`sweepToFill`"),
            (
                OrderAttributes { discretionary_amount: Some(0.5), ..Default::default() },
                "`discretionaryAmt`",
            ),
        ] {
            let why = map_order_request(&with(one, "limit"), &es, 1).expect_err(word);
            assert!(
                why.contains(word) && why.contains("stock only") && why.contains("FUT"),
                "{why}"
            );
        }
        let why = map_order_request(&with(aon, "market"), &c, 1).expect_err("a market");
        assert!(why.contains("plain limit") && why.contains("`MKT`"), "{why}");
        let loc =
            OrderRequest { time_in_force: vike_model::TimeInForce::AtClose, ..with(aon, "limit") };
        assert!(map_order_request(&loc, &c, 1).expect_err("LOC").contains("`LOC`"));
    }

    /// **A display of zero is IB's `hidden`**, `displaySize` unset, on a plain limit of a STOCK only (IB applies it to
    /// NASDAQ-routed stock orders; elsewhere the order would rest shown).
    #[test]
    fn a_display_of_zero_is_a_hidden_limit_with_no_display_size() {
        let c = parse_simplified("AAPL.SMART.USD").unwrap();
        let hidden = OrderRequest { display_quantity: Some(0.0), ..req(1, "limit", Some(150.0)) };
        let spec = map_order_request(&hidden, &c, 1).unwrap();
        assert_eq!((spec.order_type, spec.hidden, spec.display_size), ("LMT", true, None));
        for other in ["ES.GLOBEX.USD.FUT.20251219", "EUR.USD.IDEALPRO"] {
            let why =
                map_order_request(&hidden, &parse_simplified(other).unwrap(), 1).expect_err(other);
            assert!(why.contains("`hidden`") && why.contains("stock only"), "{why}");
        }
        let stop = OrderRequest { trigger_price: Some(180.0), ..req(1, "stop", None) };
        let why = map_order_request(&OrderRequest { display_quantity: Some(0.0), ..stop }, &c, 1)
            .expect_err("a stop");
        assert!(why.contains("hidden") && why.contains("`STP`"), "{why}");
        let loo = OrderRequest { time_in_force: vike_model::TimeInForce::AtOpen, ..hidden };
        assert!(map_order_request(&loo, &c, 1).expect_err("LOO").contains("in an auction"));
    }

    /// **The trailing take-profit is TWS's `TRAIL MIT`** (no limit offset), the trail as `auxPrice`, no `lmtPrice`, no
    /// activation, on a stock only; with a limit offset it would be `TRAIL LIT`, refused by name like `TRAIL LIMIT` (IB wants
    /// its starting stop). **A percent trail** is `trailingPercent` with no `auxPrice`, on the two codes built (`TRAIL`,
    /// `TRAIL MIT`).
    #[test]
    fn a_trailing_take_profit_is_trail_mit_and_a_percent_trail_is_trailing_percent() {
        let c = parse_simplified("AAPL.SMART.USD").unwrap();
        let ttp = OrderRequest { trail: Some(2.5), ..req(-1, "trailing_take_profit", None) };
        let shape = |r: &OrderRequest| {
            let s = map_order_request(r, &c, 1).unwrap();
            (
                s.order_type,
                s.lmt_price,
                s.aux_price,
                s.lmt_price_offset,
                s.trailing_percent,
                s.activation,
            )
        };
        assert_eq!(shape(&ttp), ("TRAIL MIT", None, Some(2.5), None, None, None));
        let lit = OrderRequest { limit_offset: Some(0.25), ..ttp.clone() };
        let why = map_order_request(&lit, &c, 1).expect_err("TRAIL LIT");
        assert!(why.contains("`TRAIL LIT`") && why.contains("trailStopPrice"), "{why}");
        let es = parse_simplified("ES.GLOBEX.USD.FUT.20251219").unwrap();
        let why = map_order_request(&ttp, &es, 1).expect_err("a future");
        assert!(why.contains("`TRAIL MIT`") && why.contains("stock only"), "{why}");

        for (ty, code) in [("trailing", "TRAIL"), ("trailing_take_profit", "TRAIL MIT")] {
            let pct = OrderRequest { trail_percent: Some(1.5), ..req(1, ty, None) };
            assert_eq!(shape(&pct), (code, None, None, None, Some(1.5), None), "{code}");
        }
        for (ty, code) in
            [("trailing_limit", "`TRAIL LIMIT`"), ("trailing_take_profit", "`TRAIL LIT`")]
        {
            let pct = OrderRequest {
                trail_percent: Some(1.5),
                limit_offset: Some(0.25),
                ..req(1, ty, None)
            };
            let why = map_order_request(&pct, &c, 1).expect_err(code);
            assert!(why.contains(code) && why.contains("trailStopPrice"), "{why}");
        }
        let stop = OrderRequest {
            trigger_price: Some(180.0),
            trail_percent: Some(1.5),
            ..req(-1, "stop", None)
        };
        assert_eq!(map_order_request(&stop, &c, 1).unwrap().trailing_percent, None);
    }

    /// **An adjustable stop is IB's**: `triggerPrice` its level, `adjustedOrderType` the target, its prices, and
    /// `adjustableTrailingUnit` 0 for a trail in price, 100 for a percent; on a `STP` / `STP LMT` only.
    #[test]
    fn an_adjustable_stop_carries_its_level_and_its_target() {
        use vike_model::{OrderStopAdjust, OrderStopAdjustTo};
        let c = parse_simplified("AAPL.SMART.USD").unwrap();
        let adjusted = |ty: &str, to| OrderRequest {
            trigger_price: Some(180.0),
            price: (ty == "stop_limit").then_some(179.0),
            adjust: Some(OrderStopAdjust { at: 200.0, to }),
            ..req(-1, ty, None)
        };
        let ibkr = |r: &OrderRequest| map_order_request(r, &c, 1).unwrap().adjust.unwrap();
        let at =
            |order_type, stop_price, stop_limit_price, trailing_amount, trailing_unit| IbkrAdjust {
                trigger_price: 200.0,
                order_type,
                stop_price,
                stop_limit_price,
                trailing_amount,
                trailing_unit,
            };
        assert_eq!(
            ibkr(&adjusted("stop", OrderStopAdjustTo::Stop { stop: 195.0 })),
            at("STP", 195.0, None, None, 0)
        );
        assert_eq!(
            ibkr(&adjusted(
                "stop_limit",
                OrderStopAdjustTo::StopLimit { stop: 195.0, limit: 194.5 }
            )),
            at("STP LMT", 195.0, Some(194.5), None, 0)
        );
        let trail =
            OrderStopAdjustTo::Trailing { stop: 195.0, trail: Some(2.0), trail_percent: None };
        assert_eq!(ibkr(&adjusted("stop", trail)), at("TRAIL", 195.0, None, Some(2.0), 0));
        let pct =
            OrderStopAdjustTo::Trailing { stop: 195.0, trail: None, trail_percent: Some(1.0) };
        assert_eq!(ibkr(&adjusted("stop", pct)), at("TRAIL", 195.0, None, Some(1.0), 100));
        let spec = map_order_request(&adjusted("stop", pct), &c, 1).unwrap();
        assert_eq!(
            (spec.order_type, spec.aux_price),
            ("STP", Some(180.0)),
            "the stop itself is unchanged"
        );

        let limit = OrderRequest {
            adjust: Some(OrderStopAdjust {
                at: 200.0,
                to: OrderStopAdjustTo::Stop { stop: 195.0 },
            }),
            ..req(-1, "limit", Some(190.0))
        };
        let why = map_order_request(&limit, &c, 1).expect_err("a limit");
        assert!(why.contains("adjustment") && why.contains("`LMT`"), "{why}");
    }

    /// **A time release is IB's `goodAfterTime`** (IB holds it), in [`ibkr_utc`]'s spelling; a release with a price
    /// condition is the core's alone and is refused here, never sent on its time.
    #[test]
    fn a_time_release_is_good_after_time_and_a_price_condition_is_never_sent() {
        use vike_model::{OrderRelease, OrderReleasePrice, OrderReleasePriceSide};
        let c = parse_simplified("AAPL.SMART.USD").unwrap();
        let timed = OrderRequest {
            release: Some(OrderRelease { at: 1_794_234_600_000, price: None }),
            ..req(1, "limit", Some(150.0))
        };
        let spec = map_order_request(&timed, &c, 1).unwrap();
        assert_eq!(
            (spec.order_type, spec.tif, spec.good_after_time.as_deref()),
            ("LMT", "GTC", Some("20261109-14:30:00"))
        );
        let priced = OrderRequest {
            release: Some(OrderRelease {
                at: 1_794_234_600_000,
                price: Some(OrderReleasePrice {
                    symbol: "AAPL.SMART.USD".into(),
                    level: 200.0,
                    side: OrderReleasePriceSide::Above,
                    venue: None,
                }),
            }),
            ..timed
        };
        let why = map_order_request(&priced, &c, 1).expect_err("a price condition");
        assert!(why.contains("price condition") && why.contains("goodAfterTime"), "{why}");
    }

    #[test]
    fn status_terminality() {
        assert!(OrderStatusKind::Filled.is_terminal());
        assert!(OrderStatusKind::Cancelled.is_terminal());
        assert!(OrderStatusKind::ApiCancelled.is_terminal());
        assert!(OrderStatusKind::Submitted.is_active());
        assert!(!OrderStatusKind::Submitted.is_terminal());
        assert_eq!(OrderStatusKind::from_ibkr("PreSubmitted"), OrderStatusKind::PreSubmitted);
    }

    /// The two words of a live order the socket and the Client Portal share with the recon table, and the unknown one the
    /// owner's ruling reads as over. `WarnState` and `PreCancelled` used to fold into `Inactive`.
    #[test]
    fn the_words_of_a_live_order_are_live_and_an_unknown_one_is_not() {
        let warn = OrderStatusKind::from_ibkr("WarnState");
        assert!(warn.is_active() && !warn.is_terminal());
        assert_eq!(OrderStatusKind::from_ibkr("PreCancelled"), OrderStatusKind::PendingCancel);
        assert_eq!(OrderStatusKind::from_ibkr("SomeNewIbWord"), OrderStatusKind::Inactive);
        assert_eq!(OrderStatusKind::from_ibkr("pending_submit"), OrderStatusKind::Inactive);
    }
}
