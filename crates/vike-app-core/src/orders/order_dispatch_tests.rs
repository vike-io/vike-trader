use super::*;
use vike_panels::trade::Exits;

/// A snapshot whose PRIMARY market is `(venue, symbol)` and whose multiplier grid is empty
/// (every symbol resolves to `multiplier_default`). It publishes NO engine block, so no address is
/// tradable on it: the shape of an observer's pre-first-frame placeholder.
fn snap(venue: &str, symbol: &str) -> CoreSnapshot {
    CoreSnapshot::empty(venue, symbol)
}

/// The binance/BTCUSDT primary, with the one engine block a node publishes for it (the default
/// account, trading `BTCUSDT`).
fn binance_snap() -> CoreSnapshot {
    let mut s = snap("binance", "BTCUSDT");
    s.portfolio.venues = vec![block("binance", None)];
    s
}

/// The binance/BTCUSDT.P primary: one default engine mounted on the PERP lane, the only binance
/// engine whose lane holds a bracket's stop-loss (`vike_catalog::engine_lane_holds_stop`). Every
/// test here that needs an ADMITTED bracket on binance runs on it; [`binance_snap`]'s spot engine is
/// the shipped daemon's shape, where a bracket is refused (I-1 of the final review, slice B).
fn perp_snap() -> CoreSnapshot {
    let mut s = snap("binance", "BTCUSDT.P");
    s.portfolio.venues =
        vec![vike_exec::VenueBlock { symbol: "BTCUSDT.P".into(), ..block("binance", None) }];
    s
}

/// The default account's address on [`perp_snap`]'s perp market.
fn btc_perp() -> TradeAddress {
    TradeAddress { symbol: "BTCUSDT.P".into(), ..btc() }
}

fn capped(max_notional: f64) -> OrderLimits {
    OrderLimits { max_notional, ..OrderLimits::default() }
}

/// The snapshot's PRIMARY address: its venue and symbol on the default account, where the old
/// ticket sent every order.
fn primary(s: &CoreSnapshot) -> TradeAddress {
    TradeAddress { venue: s.venue.clone(), account: None, symbol: s.symbol.clone() }
}

fn btc() -> TradeAddress {
    TradeAddress { venue: "binance".into(), account: None, symbol: "BTCUSDT".into() }
}

fn sub(symbol: &str) -> TradeAddress {
    TradeAddress {
        venue: "binance".into(),
        account: AccountLabel::parse("SUB").ok(),
        symbol: symbol.into(),
    }
}

/// A ticket buy with the fields the rewritten pre-window tests vary.
fn ticket(
    order_type: TicketKind,
    qty: f64,
    price: Option<f64>,
    exits: Option<Exits>,
) -> TradeAction {
    TradeAction::Place {
        side: 1,
        order_type,
        price,
        qty,
        reduce_only: false,
        exits,
        origin: Origin::Ticket,
    }
}

/// A buy of 2 @ 150 (notional 300), as the roster test has always driven, with the three fields
/// the refusal tests vary. A helper because struct-update syntax is not available on an enum
/// variant (E0436).
fn place_with(
    origin: Origin,
    order_type: TicketKind,
    reduce_only: bool,
    exits: Option<Exits>,
) -> TradeAction {
    TradeAction::Place {
        side: 1,
        order_type,
        price: Some(150.0),
        qty: 2.0,
        reduce_only,
        exits,
        origin,
    }
}

/// A limit buy of 2 @ 150 (notional 300).
fn place(origin: Origin, exits: Option<Exits>) -> TradeAction {
    place_with(origin, TicketKind::Limit, false, exits)
}

/// One engine's block on binance, as a node that publishes mode and symbols names it.
fn block(route_key: &str, account: Option<AccountLabel>) -> vike_exec::VenueBlock {
    vike_exec::VenueBlock {
        venue: "binance".into(),
        account,
        route_key: route_key.into(),
        symbol: "BTCUSDT".into(),
        mode: Some(vike_exec::EngineMode::Live),
        ..Default::default()
    }
}

/// The same block as an OLDER node publishes it: no `mode` and no symbol.
fn older(route_key: &str, account: Option<AccountLabel>) -> vike_exec::VenueBlock {
    vike_exec::VenueBlock { symbol: String::new(), mode: None, ..block(route_key, account) }
}

/// A snapshot with a `binance#SUB` block that trades `symbols`, AFTER the primary default
/// binance block: `vike_core::snapshot::build` always publishes the primary engine first.
fn with_sub(symbols: &[&str]) -> CoreSnapshot {
    let mut s = binance_snap();
    s.portfolio.venues.push(vike_exec::VenueBlock {
        venue: "binance".into(),
        account: AccountLabel::parse("SUB").ok(),
        route_key: "binance#SUB".into(),
        symbol: symbols.first().map(|s| s.to_string()).unwrap_or_default(),
        extra_symbols: symbols.iter().skip(1).map(|s| s.to_string()).collect(),
        mode: Some(vike_exec::EngineMode::Live),
        ..Default::default()
    });
    s
}

fn order_view(coid: &str, account: Option<AccountLabel>, side: i32) -> vike_exec::OrderView {
    vike_exec::OrderView {
        data_delay: None,
        client_order_id: coid.into(),
        venue: "binance".into(),
        account,
        symbol: "BTCUSDT".into(),
        side,
        qty: 1.0,
        order_type: vike_model::OrderType::Limit,
        price: Some(99.0),
        trigger_price: None,
        status: vike_model::OrderStatus::Accepted,
        venue_order_id: None,
        filled_qty: 0.0,
        avg_fill_px: 0.0,
    }
}

/// A one-way (`BOTH`) position of `size` on `(venue, symbol)`.
fn position_view(venue: &str, symbol: &str, size: f64) -> vike_exec::PositionView {
    vike_exec::PositionView {
        venue: venue.to_string(),
        symbol: symbol.to_string(),
        position_side: vike_model::events::PositionSide::Both,
        size,
        avg_px: 150.0,
        unrealized: 0.0,
        mark_source: None,
        leverage: 1.0,
        liq_price: 0.0,
        margin_mode: vike_model::MarginMode::Cross,
        isolated_margin: None,
    }
}

/// THE OUTPUT INVARIANT, asserted by every test below: no `Submit`/`Bracket` command in a plan
/// may breach `limits`. This is what makes a FUTURE unvalidated path fail rather than pass —
/// it inspects the plan's commands, not the code that produced them.
fn assert_plan_respects_limits(plan: &DispatchPlan, limits: &OrderLimits, s: &CoreSnapshot) {
    for cmd in &plan.commands {
        match cmd {
            Command::Order(OrderIntent::Submit(req)) => {
                let m = s.multiplier_of(&req.venue, &req.symbol);
                assert_eq!(
                    order_entry::validate_with_multiplier(req, limits, m),
                    Ok(()),
                    "an emitted Submit breaches the local cap: {req:?}"
                );
            }
            Command::Order(OrderIntent::SubmitBatch(reqs)) => {
                for req in reqs {
                    let m = s.multiplier_of(&req.venue, &req.symbol);
                    assert_eq!(
                        order_entry::validate_with_multiplier(req, limits, m),
                        Ok(()),
                        "an emitted SubmitBatch leg breaches the local cap: {req:?}"
                    );
                }
            }
            Command::Order(OrderIntent::Bracket(spec)) => {
                let m = s.multiplier_of(&spec.venue, &spec.symbol);
                for leg in vike_model::build_bracket(spec, "e", "s", "t").iter() {
                    assert_eq!(
                        order_entry::validate_with_multiplier(leg, limits, m),
                        Ok(()),
                        "an emitted Bracket leg breaches the local cap: {leg:?}"
                    );
                }
            }
            _ => {}
        }
    }
}

fn submit_count(plan: &DispatchPlan) -> usize {
    plan.commands
        .iter()
        .filter(|c| {
            matches!(
                c,
                Command::Order(
                    OrderIntent::Submit(_) | OrderIntent::SubmitBatch(_) | OrderIntent::Bracket(_)
                )
            )
        })
        .count()
}

/// The client order ids a plan cancels, in order.
fn cancelled(plan: &DispatchPlan) -> Vec<String> {
    plan.commands
        .iter()
        .filter_map(|c| match c {
            Command::Order(OrderIntent::Cancel(coid)) => Some(coid.clone()),
            _ => None,
        })
        .collect()
}

/// Build a one-intent [`DispatchInputs`] that drives EXACTLY `source`, with an order whose
/// notional is 300 (`qty 2 × price 150`) — over the 100 cap the roster test uses, under the
/// permissive default.
///
/// NO WILDCARD ARM: a new [`SubmitSource`] variant fails to compile right here until it is
/// given an input shape, which is what stops a future submit path from being added without
/// being driven by the gate.
fn one_intent_for(source: SubmitSource) -> DispatchInputs {
    let mut i = DispatchInputs::default();
    match source {
        SubmitSource::Trade => i.trade_actions.push((btc(), place(Origin::Ticket, None))),
        SubmitSource::TradeBracket => i.trade_actions.push((
            btc(),
            place(Origin::Ticket, Some(Exits { take_profit: 150.0, stop_loss: 140.0 })),
        )),
        SubmitSource::Dom => i.trade_actions.push((btc(), place(Origin::Ladder, None))),
        // A market exit has no price, so notional is not the lever here — `max_qty` is.
        SubmitSource::DomExit => i.trade_actions.push((btc(), TradeAction::ClosePosition)),
        SubmitSource::Cockpit => {
            i.cockpit_cmds.push(CockpitCmd::Submit {
                token: "TOK".into(),
                side: 1,
                price: Some(150.0),
                qty: 2.0,
            });
        }
        SubmitSource::Options => {
            i.opt_orders.push(OptOrderTicket {
                instrument: "BTC-28MAR25-100000-C".into(),
                side: 1,
                price: 150.0,
                qty: 2.0,
                kind: vike_marketdata::OptionKind::Call,
                strike: 100_000.0,
            });
        }
    }
    i
}

/// A snapshot carrying a `(venue, symbol)` position of `size` on its one engine, published as an
/// OLDER node publishes it (no symbol, no mode), so the window's exit path has something to close.
fn snap_with_position(venue: &str, symbol: &str, size: f64) -> CoreSnapshot {
    let mut s = snap(venue, symbol);
    s.portfolio.venues.push(vike_exec::VenueBlock {
        venue: venue.to_string(),
        route_key: venue.to_string(),
        positions: vec![position_view(venue, symbol, size)],
        ..Default::default()
    });
    s
}

#[path = "order_dispatch_tests/caps.rs"]
#[cfg(test)]
mod caps;

#[path = "order_dispatch_tests/accounts.rs"]
#[cfg(test)]
mod accounts;
