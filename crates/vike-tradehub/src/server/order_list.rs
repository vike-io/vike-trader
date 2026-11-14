//! `server::order_list` — the door's ORDER LIST (`CommandWire::OrderList`, wave 4 of the order kinds): its values
//! ([`model_order_list`], the lowering `lower_command` makes), its engine verdict ([`order_list_refusal`], the call
//! `accept_command` and the Preview both make, as [`super::refusal::bracket_refusal`] is for a bracket) and the
//! plain orders its OPENING legs are ([`opening_orders`], [`planned_order`], [`market_opening_legs`]: every opening
//! leg meets the per-order gates, the price wait and the held-price sizing, not only the first).
//!
//! Every leg is on ONE account and ONE symbol by construction (decision 0254): the wire names them once, for the
//! whole list, and the core places every leg on that account's engine. Which account a list that names none reaches
//! is `one_account_addressed`'s (a venue of one account) and `account_refusal`'s; the engine must then trade the list's
//! symbol, because only a plain order adds a symbol to an engine.
//!
//! Every leg names the order client id its client minted (a remote submit's rule), so a retry with the same ids is
//! refused whole (`order_client_id_reuse_refusal`), and the `Ack` echoes them, comma-joined. What is the CORE's, not this door's:
//! whether the venue can hold each leg as a reduce-only exit and resize it (`vike_venues`' capability rows, wave 4's
//! whole-list refusal, decision 0219's atomic preflight).

use vike_model::accounts::account_keys::{AccountLabel, parse_wire_account, route_key_of};
use vike_model::{
    OrderList, OrderListLeg, OrderType, TimeInForce, build_order_list, lifetime_refusal,
    order_terms_refusal,
};
use vike_tradehub_client::wire::{CommandWire, OrderListLegWire, OrderListWire, OrderRequestWire};

/// The model's [`OrderList`] for a wire list, or the refusal naming the leg and the field. Refused: an empty
/// symbol, a malformed account, a side other than ±1, a size or a price that is not a finite number above zero, an
/// unknown kind or lifetime word, terms the kind cannot carry (`order_terms_refusal`, `lifetime_refusal`), and links
/// `build_order_list` cannot resolve. PURE: it holds no roster.
pub(super) fn model_order_list(l: OrderListWire) -> Result<OrderList, String> {
    if l.symbol.trim().is_empty() {
        return Err("order list: empty symbol".to_string());
    }
    let account = l
        .account
        .as_deref()
        .map(parse_wire_account)
        .transpose()
        .map_err(|e| format!("order list: `account` — {e}"))?;
    let now = vike_model::now_ms();
    let legs = l
        .legs
        .into_iter()
        .enumerate()
        .map(|(i, leg)| model_leg(leg, now).map_err(|e| format!("order list: leg {i}: {e}")))
        .collect::<Result<Vec<_>, _>>()?;
    let spec = OrderList { venue: l.venue, symbol: l.symbol, legs, account };
    // The NETWORK path's idempotency policy, a remote submit's (`lower_command`): every leg names the id its client
    // minted, so a retry names the same ones and is refused whole rather than placed twice.
    let order_client_ids = match spec.client_order_client_ids() {
        Ok(Some(order_client_ids)) => order_client_ids,
        Ok(None) => {
            return Err(
                "order list: a remote order list requires a pre-minted order_client_id on \
                        every leg"
                    .to_string(),
            );
        }
        Err(e) => return Err(format!("order list: {e}")),
    };
    // The links, by the one rule the core lowers with, over the ids the legs name.
    build_order_list(&spec, &order_client_ids).map_err(|e| format!("order list: {e}"))?;
    Ok(spec)
}

fn model_leg(leg: OrderListLegWire, now: i64) -> Result<OrderListLeg, String> {
    if leg.side != 1 && leg.side != -1 {
        return Err(format!("`side` must be 1 (buy) or -1 (sell), got {}", leg.side));
    }
    let positive = |field: &str, v: f64| {
        (v.is_finite() && v > 0.0)
            .then_some(())
            .ok_or_else(|| format!("`{field}` must be a finite number above zero, got {v}"))
    };
    positive("qty", leg.qty)?;
    leg.price.map(|p| positive("price", p)).transpose()?;
    leg.trigger_price.map(|p| positive("trigger_price", p)).transpose()?;
    let order_type =
        OrderType::parse(&leg.order_type).map_err(|e| format!("`order_type` — {e}"))?;
    // A submit's rule (`submit_terms_refusal`): the terms rule leaves a trail on another kind to its backtest meaning,
    // and the core would lower it as a trailing stop, a kind the sender did not name.
    if let Some(t) = leg.trail.filter(|_| !order_type.is_trailing()) {
        return Err(format!(
            "order terms — a trail belongs to a trailing, trailing_limit or trailing_take_profit order; \
             this {order_type} order states {t}"
        ));
    }
    let time_in_force = leg
        .time_in_force
        .as_deref()
        .map(TimeInForce::parse)
        .transpose()
        .map_err(|e| format!("`time_in_force` — {e}"))?
        .unwrap_or_default();
    if let Some(why) =
        order_terms_refusal(&order_type, leg.price, leg.trigger_price, leg.trail, None, None, None)
    {
        return Err(format!("order terms — {why}"));
    }
    if let Some(why) = lifetime_refusal(&order_type, time_in_force, None, None, now) {
        return Err(format!("`time_in_force` — {why}"));
    }
    Ok(OrderListLeg {
        side: leg.side,
        qty: leg.qty,
        order_type,
        price: leg.price,
        trigger_price: leg.trigger_price,
        trail: leg.trail,
        reduce_only: leg.reduce_only,
        time_in_force,
        parent: leg.parent,
        contingency: leg.contingency,
        linked: leg.linked,
        order_client_id: leg.order_client_id,
    })
}

/// **THE order-list verdict past the account gate**: its values ([`model_order_list`]), then whether the ONE engine
/// it reaches can hold it, judged from that engine's published block: the engine is published and says what it
/// trades, it trades the list's symbol, and, when a leg is a trigger kind, its lane can hold a stop
/// (`vike_catalog::engine_lane_holds_stop`: binance's and aster's spot lane cannot, and the venue would reject the
/// stop only after the entry filled). `None` for an admissible list and for every other verb. ONE call for the
/// command and the Preview, so they cannot answer one frame differently.
pub(crate) fn order_list_refusal(
    cmd: &CommandWire,
    blocks: &[vike_execution::VenueBlock],
) -> Option<String> {
    let CommandWire::OrderList(l) = cmd else { return None };
    if let Err(why) = model_order_list(l.clone()) {
        return Some(why);
    }
    let venue = l.venue.as_str();
    // `model_order_list` parsed the account above, so this cannot fail.
    let label = l.account.as_deref().map_or(Ok(AccountLabel::Default), parse_wire_account).ok()?;
    let key = route_key_of(venue, &label);
    let Some(engine) = blocks.iter().find(|v| v.route_key == key && !v.symbol.is_empty()) else {
        return Some(format!(
            "order list: this node publishes no engine for `{key}` (venue `{venue}`, account `{label}`) that says \
             what it trades, so it cannot tell whether that engine can hold this list. The order list was REFUSED \
             rather than sent to an engine nobody checked"
        ));
    };
    let trades = std::iter::once(&engine.symbol)
        .chain(engine.extra_symbols.iter())
        .map(|s| format!("`{s}`"))
        .collect::<Vec<_>>()
        .join(", ");
    let has_trigger =
        l.legs.iter().any(|leg| OrderType::parse(&leg.order_type).is_ok_and(|t| t.is_trigger()));
    if has_trigger && !vike_catalog::engine_lane_holds_stop(venue, &engine.symbol) {
        return Some(format!(
            "order list: this node's `{key}` engine trades {trades}, on {venue}'s spot lane, and that lane cannot \
             hold a stop yet (the venue would reject it only after the entry filled). No order list with a stop \
             leg goes to `{key}` on this node until it runs a perp-lane engine (a `{}` symbol)",
            vike_venues::PERP_SUFFIX
        ));
    }
    if !engine.trades(&l.symbol) {
        return Some(format!(
            "order list: this node's `{key}` engine trades {trades}, not `{}`. Every leg goes to that one engine, \
             and only a plain order adds a symbol to an engine, so this list would land on another instrument. \
             Send it on a symbol that engine trades",
            l.symbol
        ));
    }
    None
}

/// Every OPENING leg of the list (not reduce-only, waiting for no parent: a ladder's entry, an OTO's parent, each
/// entry of an OCO straddle) as the plain order it is, for the door's per-order gates (the held grid, the
/// rounding note, the leg-shaped refusals of the symbol plan). Never sent: the list lowers to its own
/// `OrderIntent::OrderList`. Empty when every leg reduces.
pub(super) fn opening_orders(l: &OrderListWire) -> Vec<OrderRequestWire> {
    opening_legs(l).map(|i| plain_order(l, i)).collect()
}

/// The opening leg the symbol PLAN is made for: the plan is the symbol's (one account, one symbol, so one price lane
/// and one first-price wait serve every leg), and a leg with no price of its own is the one that needs the wait, so
/// it is that leg when there is one, else the first opening leg.
pub(super) fn planned_order(l: &OrderListWire) -> Option<OrderRequestWire> {
    let mut legs = opening_orders(l);
    let priceless = legs.iter().position(|o| o.own_price().is_none()).unwrap_or(0);
    (!legs.is_empty()).then(|| legs.swap_remove(priceless))
}

/// The opening legs that are MARKET orders: the ones the door's ceiling cannot size from the frame, each sized at
/// the price the node holds instead (`market_order_to_size`, `priced_for_sizing`), as a bracket's market entry is.
pub(super) fn market_opening_legs(l: &OrderListWire) -> Vec<usize> {
    opening_legs(l)
        .filter(|&i| matches!(OrderType::parse(&l.legs[i].order_type), Ok(OrderType::Market)))
        .collect()
}

fn opening_legs(l: &OrderListWire) -> impl Iterator<Item = usize> + '_ {
    l.legs
        .iter()
        .enumerate()
        .filter(|(_, leg)| !leg.reduce_only && leg.parent.is_none())
        .map(|(i, _)| i)
}

fn plain_order(l: &OrderListWire, i: usize) -> OrderRequestWire {
    let leg = &l.legs[i];
    OrderRequestWire {
        order_client_id: leg.order_client_id.clone().unwrap_or_default(),
        venue: l.venue.clone(),
        symbol: l.symbol.clone(),
        side: leg.side,
        qty: leg.qty,
        order_type: leg.order_type.clone(),
        price: leg.price,
        trigger_price: leg.trigger_price,
        reduce_only: false,
        account: l.account.clone(),
        time_in_force: leg.time_in_force.clone(),
        gtd_expiry: None,
        display_quantity: None,
        release: None,
        trail: leg.trail,
        limit_offset: None,
        peg: None,
        attributes: None,
        trail_percent: None,
        adjust: None,
    }
}

/// Whether the list OPENS a position: any leg that is not reduce-only.
pub(super) fn order_list_opens(l: &OrderListWire) -> bool {
    l.legs.iter().any(|leg| !leg.reduce_only)
}
