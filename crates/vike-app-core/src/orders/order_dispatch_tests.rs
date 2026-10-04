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
        vec![vike_core::VenueBlock { symbol: "BTCUSDT.P".into(), ..block("binance", None) }];
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
    order_type: OrderType,
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
    order_type: OrderType,
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
    place_with(origin, OrderType::Limit, false, exits)
}

/// One engine's block on binance, as a node that publishes mode and symbols names it.
fn block(route_key: &str, account: Option<AccountLabel>) -> vike_core::VenueBlock {
    vike_core::VenueBlock {
        venue: "binance".into(),
        account,
        route_key: route_key.into(),
        symbol: "BTCUSDT".into(),
        mode: Some(vike_exec::EngineMode::Live),
        ..Default::default()
    }
}

/// The same block as an OLDER node publishes it: no `mode` and no symbol.
fn older(route_key: &str, account: Option<AccountLabel>) -> vike_core::VenueBlock {
    vike_core::VenueBlock { symbol: String::new(), mode: None, ..block(route_key, account) }
}

/// A snapshot with a `binance#SUB` block that trades `symbols`, AFTER the primary default
/// binance block: `CoreSnapshot::build` always publishes the primary engine first.
fn with_sub(symbols: &[&str]) -> CoreSnapshot {
    let mut s = binance_snap();
    s.portfolio.venues.push(vike_core::VenueBlock {
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

fn order_view(coid: &str, account: Option<AccountLabel>, side: i32) -> vike_core::OrderView {
    vike_core::OrderView {
        client_order_id: coid.into(),
        venue: "binance".into(),
        account,
        symbol: "BTCUSDT".into(),
        side,
        qty: 1.0,
        order_type: "limit".into(),
        price: Some(99.0),
        trigger_price: None,
        status: vike_exec::OrderStatus::Accepted,
        venue_order_id: None,
        filled_qty: 0.0,
        avg_fill_px: 0.0,
    }
}

/// A one-way (`BOTH`) position of `size` on `(venue, symbol)`.
fn position_view(venue: &str, symbol: &str, size: f64) -> vike_core::PositionView {
    vike_core::PositionView {
        venue: venue.to_string(),
        symbol: symbol.to_string(),
        position_side: "BOTH".to_string(),
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
                is_call: true,
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
    s.portfolio.venues.push(vike_core::VenueBlock {
        venue: venue.to_string(),
        route_key: venue.to_string(),
        positions: vec![position_view(venue, symbol, size)],
        ..Default::default()
    });
    s
}

/// **THE GATE.** Every submit source — the Trade window's ticket, bracket, ladder and exit included —
/// is subject to the limits. Driven one source at a time so a failure names the path.
///
/// The lever differs by shape and that is deliberate: a PRICED order (ticket limit / bracket /
/// ladder place / cockpit / options) trips `max_notional`; a market EXIT carries no price, so the
/// same over-size intent trips `max_qty` instead. Both are limits from the same `OrderLimits`,
/// which is the property under test — "this path consults `OrderLimits`", not "this path
/// happens to have a price".
#[test]
fn every_submit_source_is_capped() {
    let limits = OrderLimits { max_notional: 100.0, max_qty: 1.0, ..OrderLimits::default() };
    for &source in SubmitSource::ALL {
        let s = snap_with_position("binance", "BTCUSDT", 2.0);
        let plan = plan_dispatch(one_intent_for(source), &limits, &s, 0, 7);
        assert_eq!(
            submit_count(&plan),
            0,
            "{source:?} ({}) emitted an order write past the cap",
            source.label()
        );
        assert_eq!(plan.rejects.len(), 1, "{source:?} must report exactly one reject");
        assert_eq!(plan.rejects[0].source, source);
        assert_plan_respects_limits(&plan, &limits, &s);
    }
}

/// The same roster under the PERMISSIVE default: every source still produces its order, so the
/// gate above is proving the cap and not merely that the paths are broken.
#[test]
fn every_submit_source_still_trades_under_permissive_limits() {
    let limits = OrderLimits::default();
    for &source in SubmitSource::ALL {
        let s = snap_with_position("binance", "BTCUSDT", 2.0);
        let plan = plan_dispatch(one_intent_for(source), &limits, &s, 0, 7);
        assert_eq!(
            submit_count(&plan),
            1,
            "{source:?} ({}) produced no order under permissive limits",
            source.label()
        );
        assert!(plan.rejects.is_empty(), "{source:?} rejected under permissive limits");
        assert_plan_respects_limits(&plan, &limits, &s);
    }
}

/// All sources at once, all over-cap: NOTHING order-shaped escapes, and the cancels still flow —
/// a cap must not silently swallow a risk-REDUCING verb.
#[test]
fn a_whole_frame_of_over_cap_intents_emits_no_order_write() {
    let limits = OrderLimits { max_notional: 100.0, max_qty: 1.0, ..OrderLimits::default() };
    let s = snap_with_position("binance", "BTCUSDT", 2.0);
    let mut inputs = DispatchInputs {
        account_cancels: vec!["c-1".into()],
        opt_cancels: vec!["c-2".into()],
        ..DispatchInputs::default()
    };
    for &source in SubmitSource::ALL {
        let one = one_intent_for(source);
        inputs.trade_actions.extend(one.trade_actions);
        inputs.cockpit_cmds.extend(one.cockpit_cmds);
        inputs.opt_orders.extend(one.opt_orders);
    }
    let plan = plan_dispatch(inputs, &limits, &s, 0, 0);

    assert_eq!(submit_count(&plan), 0, "an order write escaped the cap");
    assert_eq!(plan.rejects.len(), SubmitSource::ALL.len());
    assert_plan_respects_limits(&plan, &limits, &s);

    // the risk-reducing verbs are untouched
    assert_eq!(cancelled(&plan).len(), 2, "cancels must never be capped away");
}

/// How this module treats one `vike_exec::OrderIntent` kind.
#[derive(Debug, PartialEq)]
enum Class {
    /// An order write this module emits — it goes through `admit`/`admit_bracket` and is
    /// therefore subject to [`OrderLimits`].
    Capped,
    /// Not an order write: no qty/price to measure (cancels, queries, disarms).
    NotAnOrderWrite,
    /// An order write this module does NOT cap, with the reason it cannot yet.
    ExemptWithReason(&'static str),
    /// This module never emits it (a runtime/strategy-lowered verb).
    NotEmittedHere,
}

/// THE CLASSIFICATION, with **NO WILDCARD ARM** — this is the compile-time pin. A new
/// `vike_exec::OrderIntent` variant fails to compile right here until someone decides, in
/// writing, whether the UI dispatch caps it, must not, or never emits it. That is what stops a
/// new order verb from silently inheriting "uncapped".
fn classify(intent: &OrderIntent) -> Class {
    match intent {
        OrderIntent::Submit(_) | OrderIntent::Bracket(_) => Class::Capped,
        OrderIntent::Cancel(_)
        | OrderIntent::CancelBatch(_)
        | OrderIntent::Confirm(_)
        | OrderIntent::MassCancel { .. }
        | OrderIntent::DisarmConditional { .. } => Class::NotAnOrderWrite,
        OrderIntent::SubmitBatch(_)
        | OrderIntent::Flatten { .. }
        | OrderIntent::MarketExit { .. }
        | OrderIntent::ArmConditional(_)
        | OrderIntent::Combo(_) => Class::NotEmittedHere,
        OrderIntent::Modify { .. } => Class::ExemptWithReason(
            "the Trade window's drag-to-reprice. Repricing DOES change notional, but the intent \
                 carries only {coid, new_price}: capping it means resolving the resting order's qty out \
                 of the snapshot and deciding what happens to an order that no longer fits — a \
                 behavior change, not an extraction. Declared here so the hole is visible in CI \
                 instead of being an omission nobody wrote down.",
        ),
    }
}

/// The classification is exercised (not merely written): the two verbs this module emits as
/// order writes classify `Capped`, the exempt one states a real reason, and a cancel is not an
/// order write.
#[test]
fn order_intent_capping_is_classified() {
    assert_eq!(classify(&OrderIntent::Submit(Box::<OrderRequest>::default())), Class::Capped);
    assert_eq!(
        classify(&OrderIntent::Bracket(Box::new(vike_model::BracketSpec {
            venue: String::new(),
            symbol: String::new(),
            side: 1,
            qty: 0.0,
            entry_price: None,
            stop_loss: 0.0,
            take_profit: 0.0,
        }))),
        Class::Capped
    );
    assert_eq!(
        classify(&OrderIntent::Cancel(String::new())),
        Class::NotAnOrderWrite,
        "a cancel only reduces exposure — capping it would be a footgun, not a guard"
    );
    match classify(&OrderIntent::Modify {
        client_order_id: String::new(),
        new_qty: None,
        new_price: None,
    }) {
        Class::ExemptWithReason(why) => {
            assert!(why.len() > 80, "an exemption must state a real reason, not a shrug")
        }
        other => panic!("Modify's declared residual changed shape: {other:?}"),
    }

    // EVERY intent this module actually emits is `Capped` or `NotAnOrderWrite` — never an
    // unclassified escape. Driven off a real, fully-populated frame.
    let s = snap_with_position("binance", "BTCUSDT", 2.0);
    let mut inputs = DispatchInputs {
        account_cancels: vec!["c-1".into()],
        opt_cancels: vec!["c-2".into()],
        ..DispatchInputs::default()
    };
    for &source in SubmitSource::ALL {
        let one = one_intent_for(source);
        inputs.trade_actions.extend(one.trade_actions);
        inputs.cockpit_cmds.extend(one.cockpit_cmds);
        inputs.opt_orders.extend(one.opt_orders);
    }
    let plan = plan_dispatch(inputs, &OrderLimits::default(), &s, 0, 0);
    for cmd in &plan.commands {
        if let Command::Order(intent) = cmd {
            assert!(
                matches!(classify(intent), Class::Capped | Class::NotAnOrderWrite),
                "the dispatch emitted an intent it does not claim to cap: {intent:?}"
            );
        }
    }
}

/// An observer's pre-first-frame placeholder (`WireSnapshot::empty()` → empty venue/symbol) seeds
/// an EMPTY address, which is not a routable market: the order is refused with
/// `NoRoutableMarket`, before `tradable`, and never misrouted (C1, I5).
#[test]
fn trade_window_refuses_an_unroutable_address() {
    let s = snap("", "");
    let empty = primary(&s);
    assert!(!tradable(&s, &empty), "an empty address is never tradable");
    let inputs = DispatchInputs {
        trade_actions: vec![
            (empty.clone(), ticket(OrderType::Market, 1.0, None, None)),
            (
                empty.clone(),
                ticket(
                    OrderType::Limit,
                    1.0,
                    Some(1.5),
                    Some(Exits { take_profit: 2.0, stop_loss: 1.0 }),
                ),
            ),
            (empty, TradeAction::ClosePosition),
            (
                TradeAddress { symbol: String::new(), ..btc() },
                ticket(OrderType::Market, 1.0, None, None),
            ),
        ],
        ..DispatchInputs::default()
    };
    let plan = plan_dispatch(inputs, &OrderLimits::default(), &binance_snap(), 0, 0);
    assert_eq!(submit_count(&plan), 0);
    assert_eq!(plan.rejects.len(), 4);
    for r in &plan.rejects {
        assert_eq!(r.reason, DispatchRejectReason::NoRoutableMarket);
    }
    // an unroutable order consumes NO coid
    assert_eq!(plan.next_win_n, 0);
}

/// THE BUG THIS MODULE EXISTS FOR, stated as the pre-fix behaviour it inverts: a Trade-window
/// ticket whose notional is 10x the configured per-order ceiling (the
/// `policy.max_notional_per_order` row — it was `VIKE_MAX_ORDER_NOTIONAL` before
/// settings-unification Phase 5) used
/// to reach the command lane unexamined. It is now refused, with the reason named.
#[test]
fn trade_window_over_notional_is_refused() {
    let limits = capped(1_000.0);
    let s = binance_snap();
    let inputs = DispatchInputs {
        // 0.2 × 50_000 = 10_000 > 1_000
        trade_actions: vec![(primary(&s), ticket(OrderType::Limit, 0.2, Some(50_000.0), None))],
        ..DispatchInputs::default()
    };
    let plan = plan_dispatch(inputs, &limits, &s, 0, 0);
    assert_eq!(submit_count(&plan), 0);
    assert_eq!(plan.rejects.len(), 1);
    assert_eq!(plan.rejects[0].source, SubmitSource::Trade);
    assert!(matches!(
        plan.rejects[0].reason,
        DispatchRejectReason::Preview(OrderReject::NotionalAboveMax { .. })
    ));
    assert_plan_respects_limits(&plan, &limits, &s);
}

/// The CONTRACT MULTIPLIER is honoured on the Trade path too — the precise mistake the earlier
/// `order_entry` bug made. A deribit-primary snapshot with a 100x grid entry makes a ticket
/// that measures 300 bare measure 30,000 for real, so a 1,000 cap must block it.
#[test]
fn trade_window_measures_notional_with_the_contract_multiplier() {
    let limits = capped(1_000.0);
    let mut s = snap("deribit", "BTC-28MAR25-100000-C");
    let mut grid = indexmap::IndexMap::new();
    grid.insert("BTC-28MAR25-100000-C".to_string(), 100.0);
    s.portfolio.venues.push(vike_core::VenueBlock {
        venue: "deribit".into(),
        route_key: "deribit".into(),
        multipliers: std::sync::Arc::new(grid),
        ..Default::default()
    });
    assert_eq!(s.multiplier_of("deribit", "BTC-28MAR25-100000-C"), 100.0);

    let inputs = DispatchInputs {
        trade_actions: vec![(primary(&s), ticket(OrderType::Limit, 2.0, Some(150.0), None))],
        ..DispatchInputs::default()
    };
    let plan = plan_dispatch(inputs, &limits, &s, 0, 0);
    assert_eq!(submit_count(&plan), 0, "2 × 150 × 100 = 30_000 must not pass a 1_000 cap");
    assert!(matches!(
        plan.rejects[0].reason,
        DispatchRejectReason::Preview(OrderReject::NotionalAboveMax { notional, .. })
            if notional == 30_000.0
    ));

    // …and the bare (multiplier-blind) measure would have PASSED it — the pinned pre-fix hole.
    assert_eq!(
        order_entry::validate(
            &order_entry::build_order_request(
                &OrderTicket::limit("deribit", "BTC-28MAR25-100000-C", 1, 2.0, 150.0),
                "c".into()
            ),
            &limits
        ),
        Ok(())
    );
}

/// A bracket is refused when ANY of its three lowered legs breaches — here the TAKE-PROFIT
/// leg, whose price is the fat-fingered one while the entry fits. Refusal is atomic: no leg
/// is emitted, so nothing is stranded.
#[test]
fn bracket_is_refused_when_an_exit_leg_breaches() {
    let limits = capped(1_000.0);
    let s = perp_snap();
    let bracket = |tp: f64| {
        // entry: 0.01 × 100 = 1.0
        ticket(
            OrderType::Limit,
            0.01,
            Some(100.0),
            Some(Exits { take_profit: tp, stop_loss: 90.0 }),
        )
    };
    // 0.01 × 50_000 = 500 … that one passes …
    let inputs = DispatchInputs {
        trade_actions: vec![(primary(&s), bracket(50_000.0))],
        ..DispatchInputs::default()
    };
    let plan = plan_dispatch(inputs, &limits, &s, 0, 0);
    assert_eq!(submit_count(&plan), 1);
    assert_plan_respects_limits(&plan, &limits, &s);

    // … and an extra zero on the take-profit (5_000 > 1_000) refuses the WHOLE bracket.
    let inputs = DispatchInputs {
        trade_actions: vec![(primary(&s), bracket(500_000.0))],
        ..DispatchInputs::default()
    };
    let plan = plan_dispatch(inputs, &limits, &s, 0, 0);
    assert_eq!(submit_count(&plan), 0, "a breaching exit leg must refuse the whole bracket");
    assert_eq!(plan.rejects.len(), 1);
    assert_eq!(plan.rejects[0].source, SubmitSource::TradeBracket);
    assert_plan_respects_limits(&plan, &limits, &s);
}

/// A non-finite stop-loss is caught by the bracket's leg validation — the exact shape a plain
/// `validate` on the entry alone would have let through (the entry has no NaN in it).
#[test]
fn bracket_with_a_non_finite_exit_is_refused() {
    let s = perp_snap();
    let exits = Some(Exits { take_profit: 120.0, stop_loss: f64::NAN });
    let inputs = DispatchInputs {
        trade_actions: vec![(primary(&s), ticket(OrderType::Limit, 1.0, Some(100.0), exits))],
        ..DispatchInputs::default()
    };
    let plan = plan_dispatch(inputs, &OrderLimits::default(), &s, 0, 0);
    assert_eq!(submit_count(&plan), 0);
    assert_eq!(
        plan.rejects[0].reason,
        DispatchRejectReason::Preview(OrderReject::NonFiniteQtyOrPrice)
    );
}

/// A bracket consumes NO `next_win_n` (the runtime mints its three coids), matching the
/// pre-extraction site — while a plain ticket that reaches the local preview consumes exactly one,
/// whether the preview ADMITS or REFUSES it (the old sites minted before validating). A ticket
/// refused BEFORE the preview — an empty address, an untradable one, a priced type with no price —
/// mints none.
#[test]
fn coid_counter_advances_exactly_as_before() {
    let s = perp_snap();
    let exits = Some(Exits { take_profit: 120.0, stop_loss: 90.0 });
    let inputs = DispatchInputs {
        trade_actions: vec![
            (primary(&s), ticket(OrderType::Limit, 1.0, Some(100.0), exits)),
            (primary(&s), ticket(OrderType::Market, 1.0, None, None)),
        ],
        ..DispatchInputs::default()
    };
    let plan = plan_dispatch(inputs, &OrderLimits::default(), &s, 10, 2);
    assert_eq!(plan.next_win_n, 11, "bracket mints nothing; the plain ticket mints one");

    // a REFUSED ticket still burns its coid
    let inputs = DispatchInputs {
        trade_actions: vec![(primary(&s), ticket(OrderType::Limit, 1.0, Some(100.0), None))],
        ..DispatchInputs::default()
    };
    let plan = plan_dispatch(inputs, &capped(1.0), &s, 10, 2);
    assert_eq!(submit_count(&plan), 0);
    assert_eq!(plan.next_win_n, 11);

    // …while one refused before the preview burns none. The priced-type row is on the snapshot's
    // own perp market (`btc_perp`), so it reaches the PRICE check: on `btc()` it is refused as
    // not traded first, and "a priced type with no price mints no coid" went unexercised (the FW2
    // review).
    let inputs = DispatchInputs {
        trade_actions: vec![
            (
                TradeAddress { symbol: String::new(), ..btc() },
                ticket(OrderType::Market, 1.0, None, None),
            ),
            (sub("BTCUSDT"), ticket(OrderType::Market, 1.0, None, None)),
            (btc_perp(), ticket(OrderType::Limit, 1.0, None, None)),
        ],
        ..DispatchInputs::default()
    };
    let plan = plan_dispatch(inputs, &OrderLimits::default(), &s, 10, 2);
    let reasons: Vec<_> = plan.rejects.iter().map(|r| r.reason).collect();
    assert_eq!(
        reasons,
        [
            DispatchRejectReason::NoRoutableMarket,
            DispatchRejectReason::NotTraded,
            DispatchRejectReason::MissingPrice
        ],
        "an address, an account and a price refusal"
    );
    assert_eq!(plan.next_win_n, 10, "an address or price refusal mints no coid");
}

/// The window's non-order actions: `CancelSide`/`CancelAll` scope to the window's own venue and
/// symbol, and `Modify` is gated on the venue's declared caps, as the DOM's inline block was.
#[test]
fn cancel_scoping_and_modify_gating_follow_the_address() {
    let mut s = binance_snap();
    for (coid, venue, symbol, side) in [
        ("a", "binance", "BTCUSDT", 1),
        ("b", "binance", "BTCUSDT", -1),
        ("c", "binance", "ETHUSDT", 1),
        ("d", "bybit", "BTCUSDT", 1),
    ] {
        s.orders.push(vike_core::OrderView {
            venue: venue.into(),
            symbol: symbol.into(),
            ..order_view(coid, None, side)
        });
    }
    let inputs = DispatchInputs {
        trade_actions: vec![(btc(), TradeAction::CancelSide(1)), (btc(), TradeAction::CancelAll)],
        ..DispatchInputs::default()
    };
    let plan = plan_dispatch(inputs, &OrderLimits::default(), &s, 0, 0);
    assert_eq!(
        cancelled(&plan),
        ["a", "a", "b"],
        "CancelSide(+1) then CancelAll, each on this venue+symbol only"
    );

    // Modify is gated on the venue's declared caps — a venue that declares it emits the
    // command, one that does not emits nothing. Driven off the canonical roster so this stays
    // true as `venue_caps.rs` changes.
    let modify_case = |venue: &str| {
        let addr = TradeAddress { venue: venue.to_string(), account: None, symbol: "X".into() };
        let inputs = DispatchInputs {
            trade_actions: vec![(addr, TradeAction::Modify { coid: "a".into(), new_price: 1.0 })],
            ..DispatchInputs::default()
        };
        plan_dispatch(inputs, &OrderLimits::default(), &s, 0, 0).commands.len()
    };
    let allowed =
        vike_model::VENUES.iter().copied().find(|&v| vike_model::caps_for(v).allows_modify());
    let denied =
        vike_model::VENUES.iter().copied().find(|&v| !vike_model::caps_for(v).allows_modify());
    if let Some(v) = allowed {
        assert_eq!(modify_case(v), 1, "{v} declares modify");
    }
    if let Some(v) = denied {
        assert_eq!(modify_case(v), 0, "{v} declares no native modify");
    }
}

/// The exit legs keep their historical shape: a CLOSE is reduce-only at |position|, a REVERSE is
/// not reduce-only and doubles the qty, both on the OPPOSITE side.
#[test]
fn close_and_reverse_keep_their_shape() {
    let s = snap_with_position("binance", "BTCUSDT", 2.0); // long 2
    let inputs = DispatchInputs {
        trade_actions: vec![(btc(), TradeAction::ClosePosition), (btc(), TradeAction::Reverse)],
        ..DispatchInputs::default()
    };
    let plan = plan_dispatch(inputs, &OrderLimits::default(), &s, 0, 0);
    match &plan.commands[..] {
        [Command::Order(OrderIntent::Submit(close)), Command::Order(OrderIntent::Submit(rev))] => {
            assert_eq!(close.side, -1, "closing a long SELLs");
            assert_eq!(close.qty, 2.0);
            assert!(close.reduce_only);
            assert_eq!(close.order_type, "market");
            assert_eq!(rev.side, -1);
            assert_eq!(rev.qty, 4.0, "reverse doubles");
            assert!(!rev.reduce_only);
        }
        other => panic!("expected two market exits, got {other:?}"),
    }
    // a flat book emits nothing
    let flat = snap_with_position("binance", "BTCUSDT", 0.0);
    let inputs = DispatchInputs {
        trade_actions: vec![(btc(), TradeAction::ClosePosition)],
        ..DispatchInputs::default()
    };
    assert!(plan_dispatch(inputs, &OrderLimits::default(), &flat, 0, 0).commands.is_empty());
}

/// The emission ORDER the command lane sees: the Trade window's intents in drain order → the
/// Account window's cancels → cockpit → options → option cancels.
#[test]
fn emission_order_is_preserved() {
    let s = binance_snap();
    let inputs = DispatchInputs {
        trade_actions: vec![
            (btc(), ticket(OrderType::Market, 1.0, None, None)),
            (
                btc(),
                TradeAction::Place {
                    side: 1,
                    order_type: OrderType::Limit,
                    price: Some(1.0),
                    qty: 1.0,
                    reduce_only: false,
                    exits: None,
                    origin: Origin::Ladder,
                },
            ),
        ],
        account_cancels: vec!["tc".into()],
        cockpit_cmds: vec![CockpitCmd::Submit {
            token: "TOK".into(),
            side: 1,
            price: Some(0.5),
            qty: 1.0,
        }],
        opt_orders: vec![OptOrderTicket {
            instrument: "OPT".into(),
            side: 1,
            price: 1.0,
            qty: 1.0,
            is_call: true,
            strike: 1.0,
        }],
        opt_cancels: vec!["oc".into()],
    };
    let plan = plan_dispatch(inputs, &OrderLimits::default(), &s, 0, 0);
    let coids: Vec<String> = plan
        .commands
        .iter()
        .map(|c| match c {
            Command::Order(OrderIntent::Submit(r)) => r.client_order_id.clone(),
            Command::Order(OrderIntent::Cancel(c)) => format!("cancel:{c}"),
            other => format!("{other:?}"),
        })
        .collect();
    assert_eq!(
        coids,
        vec![
            "ui-0-0".to_string(),
            "dom-1-0".into(),
            "cancel:tc".into(),
            "poly-2-0".into(),
            "opt-3-0".into(),
            "cancel:oc".into(),
        ]
    );
}

// -----------------------------------------------------------------------------------------
// Every Trade window intent carries its venue, account and symbol (spec §4.2)
// -----------------------------------------------------------------------------------------

/// ⚠ **C1, the money-safety case.** A default account the server does not run is NOT tradable,
/// even where its venue's only engine is a labelled one: an account-less order there reaches
/// engine 0 (`vike_core`'s `route_of` fallback), which is ANOTHER account. The trader would click
/// Buy on the default account and the order would fill on SUB.
#[test]
fn a_default_account_the_server_does_not_run_is_not_tradable() {
    for sub_block in [
        block("binance#SUB", AccountLabel::parse("SUB").ok()),
        older("binance#SUB", AccountLabel::parse("SUB").ok()),
    ] {
        let mut s = binance_snap();
        s.portfolio.venues = vec![sub_block];
        assert!(!tradable(&s, &btc()), "no block carries route key `binance`");
        let mut i = DispatchInputs::default();
        i.trade_actions.push((btc(), place(Origin::Ticket, None)));
        i.trade_actions.push((btc(), TradeAction::ClosePosition));
        let plan = plan_dispatch(i, &OrderLimits::default(), &s, 1, 0);
        assert!(plan.commands.is_empty(), "nothing may reach engine 0: {:?}", plan.commands);
        assert_eq!(plan.rejects.len(), 2);
        assert!(plan.rejects.iter().all(|r| r.reason == DispatchRejectReason::NotTraded));
    }
}

/// …and the orders of an account the server does not run are nobody's: a window addressed to it
/// cancels nothing, even on an older node whose orders name no account (where they would read as
/// the default account's and be SUB's).
#[test]
fn an_account_the_server_does_not_run_owns_no_orders() {
    let mut s = binance_snap();
    s.portfolio.venues = vec![older("binance#SUB", AccountLabel::parse("SUB").ok())];
    s.orders = vec![order_view("s-1", None, 1)];
    let mut i = DispatchInputs::default();
    i.trade_actions.push((btc(), TradeAction::CancelAll));
    i.trade_actions.push((btc(), TradeAction::CancelSide(1)));
    let plan = plan_dispatch(i, &OrderLimits::default(), &s, 1, 0);
    assert!(plan.commands.is_empty(), "SUB's order is not this window's: {:?}", plan.commands);
}

/// A window's order carries the account it names (spec §4.2).
#[test]
fn a_window_order_carries_its_account() {
    let s = with_sub(&["BTCUSDT"]);
    let mut i = DispatchInputs::default();
    i.trade_actions.push((sub("BTCUSDT"), place(Origin::Ticket, None)));
    let plan = plan_dispatch(i, &OrderLimits::default(), &s, 1, 0);
    match plan.commands.as_slice() {
        [Command::Order(OrderIntent::Submit(req))] => {
            assert_eq!(req.account.as_ref().map(ToString::to_string).as_deref(), Some("SUB"));
            assert_eq!(req.venue, "binance");
        }
        other => panic!("one submit, got {other:?}"),
    }
}

/// A symbol the account does not trade is refused before any command (Review Focus 2).
#[test]
fn an_untradable_address_is_refused_before_any_command() {
    let s = with_sub(&["ETHUSDT"]);
    let mut i = DispatchInputs::default();
    i.trade_actions.push((sub("BTCUSDT"), place(Origin::Ladder, None)));
    let plan = plan_dispatch(i, &OrderLimits::default(), &s, 1, 0);
    assert!(plan.commands.is_empty());
    assert_eq!(plan.rejects[0].reason, DispatchRejectReason::NotTraded);
}

/// An older node names no symbol: only the primary market trades, and only on the primary
/// (default) account's block.
#[test]
fn an_older_node_trades_only_its_primary_on_the_default_account() {
    let mut s = binance_snap();
    s.portfolio.venues =
        vec![older("binance", None), older("binance#SUB", AccountLabel::parse("SUB").ok())];
    assert!(tradable(&s, &btc()), "the primary market on the default account");
    let eth = TradeAddress { symbol: "ETHUSDT".into(), ..btc() };
    assert!(!tradable(&s, &eth), "another symbol");
    assert!(!tradable(&s, &sub("BTCUSDT")), "a named account that is not the primary block");
}

/// A priced order type without its price is refused, never sent at MARKET: the old ticket's
/// `unwrap_or` habit would have turned a Limit with no price into a market order.
#[test]
fn a_priced_order_without_a_price_is_refused_not_sent_at_market() {
    let s = binance_snap();
    for order_type in [OrderType::Limit, OrderType::Stop] {
        for exits in [None, Some(Exits { take_profit: 160.0, stop_loss: 140.0 })] {
            let act = ticket(order_type, 1.0, None, exits);
            let mut i = DispatchInputs::default();
            i.trade_actions.push((btc(), act.clone()));
            let plan = plan_dispatch(i, &OrderLimits::default(), &s, 0, 0);
            assert!(plan.commands.is_empty(), "{act:?} planned {:?}", plan.commands);
            assert_eq!(plan.rejects.len(), 1, "{act:?}");
            // The REASON, not just the silence: a Stop with exits would also be refused by the
            // bracket rule, so only the reason proves the price check ran.
            assert_eq!(plan.rejects[0].reason, DispatchRejectReason::MissingPrice, "{act:?}");
        }
    }
}

/// A bracket is refused wherever it cannot be honest: on a named account, on the DEFAULT account
/// of a venue that runs a second one (`vike_model::BracketSpec` names no account, and the node's
/// bracket command serves only a venue's single default book), on an engine whose lane cannot hold
/// the stop-loss, for a Stop entry and for a reduce-only entry (Ruling R3, widened by I6; spec §3.6;
/// I-1 of the final review, slice B).
#[test]
fn a_bracket_is_refused_where_it_cannot_be_honest() {
    let exits = Some(Exits { take_profit: 160.0, stop_loss: 140.0 });
    let cases = [
        (
            with_sub(&["BTCUSDT"]),
            sub("BTCUSDT"),
            place(Origin::Ticket, exits),
            DispatchRejectReason::BracketOnNamedAccount,
        ),
        (
            with_sub(&["BTCUSDT"]),
            btc(),
            place(Origin::Ticket, exits),
            DispatchRejectReason::BracketOnNamedAccount,
        ),
        (
            binance_snap(),
            btc(),
            place(Origin::Ticket, exits),
            DispatchRejectReason::BracketNeedsPerpEngine,
        ),
        (
            perp_snap(),
            btc_perp(),
            place_with(Origin::Ticket, OrderType::Stop, false, exits),
            DispatchRejectReason::BracketNeedsMarketOrLimit,
        ),
        (
            perp_snap(),
            btc_perp(),
            place_with(Origin::Ticket, OrderType::Limit, true, exits),
            DispatchRejectReason::BracketNeedsOpeningEntry,
        ),
    ];
    for (s, addr, act, want) in cases {
        let mut i = DispatchInputs::default();
        i.trade_actions.push((addr, act));
        let plan = plan_dispatch(i, &OrderLimits::default(), &s, 1, 0);
        assert!(plan.commands.is_empty(), "{want:?}");
        assert_eq!(plan.rejects[0].reason, want);
        assert_eq!(plan.rejects[0].source, SubmitSource::TradeBracket);
    }
    // …and the positive control: the default account of a single-engine venue, on an engine whose
    // lane holds the stop-loss, takes it.
    let mut i = DispatchInputs::default();
    i.trade_actions.push((btc_perp(), place(Origin::Ticket, exits)));
    let plan = plan_dispatch(i, &OrderLimits::default(), &perp_snap(), 1, 0);
    assert!(
        matches!(plan.commands.as_slice(), [Command::Order(OrderIntent::Bracket(_))]),
        "{:?}",
        plan.commands
    );
}

/// M-6 of the final review (slice B): a stopped core and a HALTED account trade nothing in the
/// dispatcher, exactly as in the window. `tradable` itself refuses them, so a Place, a ladder click,
/// a Close and a Reverse plan nothing and are refused, not dropped (a Close under a halt included:
/// the window offers none there, and the dispatcher now agrees with it). A cancel still goes, and
/// a REDUCING account still trades.
#[test]
fn a_stopped_core_or_a_halted_account_trades_nothing_but_still_cancels() {
    let base = || {
        let mut s = binance_snap();
        s.portfolio.venues[0].positions = vec![position_view("binance", "BTCUSDT", 1.0)];
        s.orders = vec![order_view("d-1", None, 1)];
        s
    };
    let mut faulted = base();
    faulted.fault = Some("handler panicked".into());
    let mut halted = base();
    halted.portfolio.venues[0].trading_state = vike_exec::TradingState::Halted;
    for (what, s, why) in [
        ("a stopped core", &faulted, DispatchRejectReason::CoreStopped),
        ("a halted account", &halted, DispatchRejectReason::AccountHalted),
    ] {
        assert!(!tradable(s, &btc()), "{what}");
        for act in [
            place(Origin::Ticket, None),
            place(Origin::Ladder, None),
            TradeAction::ClosePosition,
            TradeAction::Reverse,
        ] {
            let mut i = DispatchInputs::default();
            i.trade_actions.push((btc(), act.clone()));
            let plan = plan_dispatch(i, &OrderLimits::default(), s, 1, 0);
            assert!(plan.commands.is_empty(), "{what}: {act:?} planned {:?}", plan.commands);
            assert_eq!(plan.rejects.len(), 1, "{what}: {act:?} is refused, never dropped");
            assert_eq!(plan.rejects[0].reason, why, "{what}: {act:?} says why");
        }
        let mut i = DispatchInputs::default();
        i.trade_actions.push((btc(), TradeAction::CancelAll));
        let plan = plan_dispatch(i, &OrderLimits::default(), s, 1, 0);
        assert_eq!(cancelled(&plan), ["d-1"], "{what}: a cancel still goes");
    }
    let mut reducing = base();
    reducing.portfolio.venues[0].trading_state = vike_exec::TradingState::Reducing;
    assert!(tradable(&reducing, &btc()), "a reducing account still trades");
    let mut i = DispatchInputs::default();
    i.trade_actions.push((btc(), TradeAction::ClosePosition));
    let plan = plan_dispatch(i, &OrderLimits::default(), &reducing, 1, 0);
    assert_eq!(submit_count(&plan), 1, "a reducing account closes: {:?}", plan.rejects);
}

/// I-1 of the final review (slice B), step 2: on the shipped daemon's shape — ONE default binance
/// engine mounted on spot `BTCUSDT` — a TP/SL bracket is refused HERE, through `Planner::reject`,
/// so the refusal carries the window's address and lands on its own strip; the node would refuse
/// it too, but only into the status bar's control segment. The rule is the node's, read off the
/// ENGINE's mounted symbol (`vike_catalog::engine_lane_holds_stop`): a perp engine takes the same
/// bracket, a plain order on the spot engine still goes, and an engine that does not say what it
/// trades (an older node, which predates the bracket command anyway) is not judged here.
#[test]
fn a_bracket_on_an_engine_whose_lane_holds_no_stop_is_refused_here() {
    let exits = Some(Exits { take_profit: 160.0, stop_loss: 140.0 });
    let plan_one = |s: &CoreSnapshot, addr: TradeAddress, act: TradeAction| {
        let mut i = DispatchInputs::default();
        i.trade_actions.push((addr, act));
        plan_dispatch(i, &OrderLimits::default(), s, 1, 0)
    };
    let plan = plan_one(&binance_snap(), btc(), place(Origin::Ticket, exits));
    assert!(plan.commands.is_empty(), "{:?}", plan.commands);
    assert_eq!(
        plan.rejects,
        vec![DispatchReject {
            source: SubmitSource::TradeBracket,
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            account: None,
            reason: DispatchRejectReason::BracketNeedsPerpEngine,
        }],
        "refused at the window's own address"
    );
    let plan = plan_one(&perp_snap(), btc_perp(), place(Origin::Ticket, exits));
    assert!(
        matches!(plan.commands.as_slice(), [Command::Order(OrderIntent::Bracket(_))]),
        "a perp engine takes it: {:?}",
        plan.rejects
    );
    let plan = plan_one(&binance_snap(), btc(), place(Origin::Ticket, None));
    assert_eq!(submit_count(&plan), 1, "TP/SL's rule only: {:?}", plan.rejects);
    let older = snap_with_position("binance", "BTCUSDT", 1.0);
    let plan = plan_one(&older, btc(), place(Origin::Ticket, exits));
    assert!(plan.rejects.is_empty(), "an engine that does not say is not judged: {plan:?}");
    assert_eq!(
        DispatchRejectReason::BracketNeedsPerpEngine.to_string(),
        "a TP/SL bracket on this venue needs an engine on its perp market: the exchange cannot \
         hold the stop-loss on its spot market"
    );
}

/// Cancel all pulls only the window account's orders on its symbol.
#[test]
fn cancel_all_pulls_only_this_accounts_orders() {
    let mut s = with_sub(&["BTCUSDT"]);
    s.orders =
        vec![order_view("d-1", None, 1), order_view("s-1", AccountLabel::parse("SUB").ok(), 1)];
    for (addr, want) in [(sub("BTCUSDT"), "s-1"), (btc(), "d-1")] {
        let mut i = DispatchInputs::default();
        i.trade_actions.push((addr, TradeAction::CancelAll));
        let plan = plan_dispatch(i, &OrderLimits::default(), &s, 1, 0);
        assert_eq!(cancelled(&plan), [want]);
        assert_eq!(plan.commands.len(), 1);
    }
}

/// An order whose account reads `Some(Default)` (a reader that rebuilt it from a wire `DEFAULT`)
/// is the DEFAULT account's order, exactly as an account-less one is (Minor 19).
#[test]
fn a_named_default_order_is_the_default_accounts() {
    let mut s = with_sub(&["BTCUSDT"]);
    s.orders = vec![
        order_view("d-1", Some(AccountLabel::Default), 1),
        order_view("s-1", AccountLabel::parse("SUB").ok(), 1),
    ];
    let mut i = DispatchInputs::default();
    i.trade_actions.push((btc(), TradeAction::CancelAll));
    let plan = plan_dispatch(i, &OrderLimits::default(), &s, 1, 0);
    assert_eq!(cancelled(&plan), ["d-1"]);
}

/// Every window cancel is one `Cancel` per order id, never a `CancelBatch`: the desktop's lift has no
/// wire form for a batch, so a batch would be dropped with a log line (Ruling R8, final review I-2).
#[test]
fn window_cancels_are_one_cancel_per_order() {
    let mut s = with_sub(&["BTCUSDT"]);
    s.orders = vec![order_view("a", None, 1), order_view("b", None, 1), order_view("c", None, -1)];
    let cases = [
        (TradeAction::Cancel(vec!["a".into(), "b".into()]), 2),
        (TradeAction::CancelSide(1), 2),
        (TradeAction::CancelAll, 3),
    ];
    for (act, want) in cases {
        let mut i = DispatchInputs::default();
        i.trade_actions.push((btc(), act));
        let plan = plan_dispatch(i, &OrderLimits::default(), &s, 1, 0);
        assert_eq!(plan.commands.len(), want);
        assert!(
            plan.commands.iter().all(|c| matches!(c, Command::Order(OrderIntent::Cancel(_)))),
            "one Cancel per order id, never a CancelBatch"
        );
    }
}

/// The default account is named `DEFAULT` only where its venue runs more than one engine (Ruling R8).
#[test]
fn a_default_account_window_names_default_only_on_a_two_engine_venue() {
    let mut one = binance_snap();
    one.portfolio.venues = vec![block("binance", None)];
    assert_eq!(wire_account(&one, &btc()), None, "one engine: None is unambiguous");
    let mut two = binance_snap();
    two.portfolio.venues =
        vec![block("binance", None), block("binance#SUB", AccountLabel::parse("SUB").ok())];
    assert_eq!(
        wire_account(&two, &btc()),
        Some(AccountLabel::Default),
        "two engines: name the default"
    );
    assert_eq!(wire_account(&two, &sub("BTCUSDT")), AccountLabel::parse("SUB").ok());
    let named_default = TradeAddress { account: Some(AccountLabel::Default), ..btc() };
    assert_eq!(
        wire_account(&one, &named_default),
        None,
        "a `Some(Default)` address is the default account, spelled as one"
    );
}

/// On an older node (no `mode` on its blocks) a venue with two blocks cannot attribute its orders:
/// the window sends no account-scoped cancel there, from EITHER account's window (Ruling R9). The
/// DEFAULT window is the money-relevant direction: every unattributed order reads as the default
/// account's, so it would pull SUB's `s-1` with its own.
#[test]
fn an_older_node_does_not_attribute_orders_so_no_account_scoped_cancel_goes_out() {
    let mut s = binance_snap();
    s.portfolio.venues =
        vec![older("binance", None), older("binance#SUB", AccountLabel::parse("SUB").ok())];
    s.orders = vec![order_view("d-1", None, 1), order_view("s-1", None, 1)];
    assert!(!orders_name_their_account(&s, "binance"));
    for addr in [btc(), sub("BTCUSDT")] {
        for act in [TradeAction::CancelAll, TradeAction::CancelSide(1)] {
            let label = format!("{act:?} at {addr:?}");
            let mut i = DispatchInputs::default();
            i.trade_actions.push((addr.clone(), act));
            let plan = plan_dispatch(i, &OrderLimits::default(), &s, 1, 0);
            assert!(plan.commands.is_empty(), "{label}: {:?}", plan.commands);
            assert_eq!(plan.rejects[0].reason, DispatchRejectReason::OrdersUnattributed, "{label}");
        }
    }
    s.portfolio.venues =
        vec![block("binance", None), block("binance#SUB", AccountLabel::parse("SUB").ok())];
    assert!(
        orders_name_their_account(&s, "binance"),
        "a node that publishes mode names its orders' accounts"
    );
    s.portfolio.venues = vec![older("binance", None)];
    assert!(orders_name_their_account(&s, "binance"), "one block: every order is its engine's");
}

/// ⚠ **`window_orders` FAILS CLOSED on its own** where orders cannot be attributed (A1's review
/// minor, the F wave's ruling): on an older two-block node every order arrives naming no account, so
/// an unguarded rule would hand the DEFAULT window both accounts' orders — markers on someone else's
/// orders, and a Cancel all that pulls them. The dispatcher and the glue each ask
/// `orders_name_their_account` first, but a caller that forgets must get nothing, not the wrong book.
#[test]
fn window_orders_answers_nothing_where_orders_cannot_be_attributed() {
    let mut s = binance_snap();
    s.portfolio.venues =
        vec![older("binance", None), older("binance#SUB", AccountLabel::parse("SUB").ok())];
    s.orders = vec![order_view("d-1", None, 1), order_view("s-1", None, -1)];
    for addr in [btc(), sub("BTCUSDT")] {
        let ids: Vec<&str> = window_orders(&s, &addr).map(|o| o.client_order_id.as_str()).collect();
        assert!(ids.is_empty(), "{addr:?} took {ids:?} on a node that attributes nothing");
    }
    // …and the positive control: the same orders on a node that publishes `mode` are attributed,
    // each to the account it names (`None` is the default account's).
    s.portfolio.venues =
        vec![block("binance", None), block("binance#SUB", AccountLabel::parse("SUB").ok())];
    s.orders =
        vec![order_view("d-1", None, 1), order_view("s-1", AccountLabel::parse("SUB").ok(), -1)];
    let ids = |addr: &TradeAddress| -> Vec<String> {
        window_orders(&s, addr).map(|o| o.client_order_id.clone()).collect()
    };
    assert_eq!(ids(&btc()), ["d-1"]);
    assert_eq!(ids(&sub("BTCUSDT")), ["s-1"]);
}

/// **`owns_order`, clause by clause** — the one attribution rule the status strip (live or done) and
/// `window_orders` (live only) share, against an EXPLICIT table rather than against itself: each
/// fixture order is excluded by exactly one clause, so dropping any clause — attributed, account
/// run, venue, symbol, account — changes some row. Three nodes: a modern one (mode published), an
/// older one whose venue has a single labelled engine, and an older two-block one (Ruling R9).
#[test]
fn owns_order_attributes_clause_by_clause() {
    use vike_exec::OrderStatus;
    let sub_label = || AccountLabel::parse("SUB").ok();
    let orders = vec![
        order_view("d", None, 1),
        order_view("s", sub_label(), 1),
        order_view("x", AccountLabel::parse("OTHER").ok(), 1),
        vike_core::OrderView { venue: "bybit".into(), ..order_view("v", None, 1) },
        vike_core::OrderView { symbol: "ETHUSDT".into(), ..order_view("y", None, 1) },
        vike_core::OrderView { status: OrderStatus::Filled, ..order_view("f", None, 1) },
    ];
    let node = |blocks: Vec<vike_core::VenueBlock>| {
        let mut s = binance_snap();
        s.portfolio.venues = blocks;
        s.portfolio.venues.push(vike_core::VenueBlock {
            venue: "bybit".into(),
            route_key: "bybit".into(),
            mode: Some(vike_exec::EngineMode::Live),
            ..Default::default()
        });
        s.orders = orders.clone();
        s
    };
    let modern = node(vec![block("binance", None), block("binance#SUB", sub_label())]);
    let one_labelled = node(vec![older("binance#SUB", sub_label())]);
    let unattributed = node(vec![older("binance", None), older("binance#SUB", sub_label())]);
    let other = || TradeAddress {
        venue: "binance".into(),
        account: AccountLabel::parse("OTHER").ok(),
        symbol: "BTCUSDT".into(),
    };
    let cases: [(&str, &CoreSnapshot, TradeAddress, &[&str]); 9] = [
        // the default account owns its own, done ones included; not SUB's, OTHER's, bybit's or
        // ETHUSDT's
        ("modern", &modern, btc(), &["d", "f"]),
        ("modern", &modern, sub("BTCUSDT"), &["s"]),
        // an account the server does not run owns nothing, even an order that names it
        ("modern", &modern, other(), &[]),
        ("one labelled", &one_labelled, btc(), &[]),
        // the one engine owns every order of its venue and symbol, whatever account each names
        ("one labelled", &one_labelled, sub("BTCUSDT"), &["d", "s", "x", "f"]),
        ("one labelled", &one_labelled, other(), &[]),
        // fail-closed: nothing can be attributed, so nothing is owned
        ("R9", &unattributed, btc(), &[]),
        ("R9", &unattributed, sub("BTCUSDT"), &[]),
        ("R9", &unattributed, other(), &[]),
    ];
    for (name, snap, addr, want) in cases {
        let owns = owns_order(snap, &addr);
        let owned: Vec<&str> =
            snap.orders.iter().filter(|o| owns(o)).map(|o| o.client_order_id.as_str()).collect();
        assert_eq!(owned, want, "{name} {:?}", addr.account);
        // …and window_orders is exactly that, minus the done order
        let live: Vec<&str> =
            window_orders(snap, &addr).map(|o| o.client_order_id.as_str()).collect();
        let want_live: Vec<&str> = want.iter().copied().filter(|c| *c != "f").collect();
        assert_eq!(live, want_live, "{name} {:?}: the live subset", addr.account);
    }
}

/// On an older node whose ONLY engine on a venue is labelled, every order of that venue is that
/// engine's even though each order names no account: the SUB window pulls them (Minor 1 of the
/// review; it used to match none of its own orders and cancel nothing), and a default-account
/// window — an account the server does not run — still owns none.
#[test]
fn a_one_engine_venue_attributes_every_order_to_its_engine() {
    let mut s = binance_snap();
    s.portfolio.venues = vec![older("binance#SUB", AccountLabel::parse("SUB").ok())];
    s.orders = vec![order_view("s-1", None, 1), order_view("s-2", None, -1)];
    assert!(orders_name_their_account(&s, "binance"));
    let sub_addr = sub("BTCUSDT");
    let ids: Vec<&str> = window_orders(&s, &sub_addr).map(|o| o.client_order_id.as_str()).collect();
    assert_eq!(ids, ["s-1", "s-2"]);
    let mut i = DispatchInputs::default();
    i.trade_actions.push((sub("BTCUSDT"), TradeAction::CancelAll));
    i.trade_actions.push((btc(), TradeAction::CancelAll));
    let plan = plan_dispatch(i, &OrderLimits::default(), &s, 1, 0);
    assert_eq!(cancelled(&plan), ["s-1", "s-2"], "SUB's window pulls SUB's; the default's none");

    // a node that publishes `mode` names each order's account, so the order's own word is used
    s.portfolio.venues = vec![block("binance#SUB", AccountLabel::parse("SUB").ok())];
    s.orders = vec![order_view("s-1", AccountLabel::parse("SUB").ok(), 1)];
    assert_eq!(window_orders(&s, &sub_addr).count(), 1);
}

/// **Only LIVE orders are the window's** — the snapshot publishes the whole order registry and
/// nothing removes a done order from it, so a session's filled, cancelled and rejected orders sit
/// beside the resting ones. Each window cancel is one `Cancel` per id behind a bounded client queue
/// and the node's rate limit, so cancels for done orders, queued oldest first, would crowd out the
/// live ones (review IMPORTANT 1). Cancel all and Cancel side pull ONLY the live orders, and the
/// shared `window_orders` the window's markers read answers the same.
#[test]
fn window_cancels_pull_only_live_orders() {
    use vike_exec::OrderStatus;
    let mut s = binance_snap();
    let with = |coid: &str, side: i32, status: OrderStatus| vike_core::OrderView {
        status,
        ..order_view(coid, None, side)
    };
    s.orders = vec![
        with("filled", 1, OrderStatus::Filled),
        with("canceled", 1, OrderStatus::Canceled),
        with("rejected", -1, OrderStatus::Rejected),
        with("denied", 1, OrderStatus::Denied),
        with("expired", -1, OrderStatus::Expired),
        with("liquidated", 1, OrderStatus::Liquidated),
        with("accepted", 1, OrderStatus::Accepted),
        with("partial", -1, OrderStatus::PartiallyFilled),
        // Positive controls beyond the two resting states (A1's review minor): an order the venue
        // has not acknowledged yet, and one whose cancel is in flight, are still live — a filter
        // that kept only Accepted/PartiallyFilled would drop both, and Cancel all would leave them.
        with("submitted", 1, OrderStatus::Submitted),
        with("pending-cancel", -1, OrderStatus::PendingCancel),
    ];
    let addr = btc();
    let live: Vec<&str> = window_orders(&s, &addr).map(|o| o.client_order_id.as_str()).collect();
    assert_eq!(live, ["accepted", "partial", "submitted", "pending-cancel"], "the markers' rule");
    for (act, want) in [
        (TradeAction::CancelAll, vec!["accepted", "partial", "submitted", "pending-cancel"]),
        (TradeAction::CancelSide(1), vec!["accepted", "submitted"]),
        (TradeAction::CancelSide(-1), vec!["partial", "pending-cancel"]),
    ] {
        let mut i = DispatchInputs::default();
        i.trade_actions.push((btc(), act.clone()));
        let plan = plan_dispatch(i, &OrderLimits::default(), &s, 1, 0);
        assert_eq!(cancelled(&plan), want, "{act:?}");
        assert_eq!(plan.commands.len(), want.len(), "{act:?}: nothing but those cancels");
    }
}

/// **An admitted order follows the window's VENUE and SYMBOL, never the snapshot's primary** (review
/// IMPORTANT 2: every other admitted order in this suite is binance/BTCUSDT, which IS the primary,
/// so the old misroute would stay green there). The primary is binance/BTCUSDT; the window trades
/// bybit/ETHUSDT on bybit's one default engine. Every order write — ticket, ladder, bracket, close
/// and reverse — must name bybit/ETHUSDT.
#[test]
fn a_window_order_follows_its_address_not_the_primary() {
    let mut s = binance_snap();
    s.portfolio.venues.push(vike_core::VenueBlock {
        venue: "bybit".into(),
        route_key: "bybit".into(),
        symbol: "ETHUSDT".into(),
        mode: Some(vike_exec::EngineMode::Live),
        positions: vec![position_view("bybit", "ETHUSDT", 1.0)],
        ..Default::default()
    });
    assert_eq!((s.venue.as_str(), s.symbol.as_str()), ("binance", "BTCUSDT"));
    let eth = TradeAddress { venue: "bybit".into(), account: None, symbol: "ETHUSDT".into() };
    let exits = Some(Exits { take_profit: 160.0, stop_loss: 140.0 });
    let actions = [
        place(Origin::Ticket, None),
        place(Origin::Ladder, None),
        place(Origin::Ticket, exits),
        TradeAction::ClosePosition,
        TradeAction::Reverse,
    ];
    for act in actions {
        let mut i = DispatchInputs::default();
        i.trade_actions.push((eth.clone(), act.clone()));
        let plan = plan_dispatch(i, &OrderLimits::default(), &s, 1, 0);
        assert!(plan.rejects.is_empty(), "{act:?}: {:?}", plan.rejects);
        match plan.commands.as_slice() {
            [Command::Order(OrderIntent::Submit(req))] => {
                assert_eq!(
                    (req.venue.as_str(), req.symbol.as_str()),
                    ("bybit", "ETHUSDT"),
                    "{act:?}"
                );
            }
            [Command::Order(OrderIntent::Bracket(spec))] => {
                assert_eq!(
                    (spec.venue.as_str(), spec.symbol.as_str()),
                    ("bybit", "ETHUSDT"),
                    "{act:?}"
                );
            }
            other => panic!("{act:?}: one order write, got {other:?}"),
        }
    }
}

/// The glue's way in: an account TEXT becomes an address, and text that is not a label refuses the
/// whole address rather than reading as the DEFAULT account (review Minor 2).
#[test]
fn a_trade_address_parses_its_account_or_refuses() {
    assert_eq!(TradeAddress::parse("binance", None, "BTCUSDT"), Some(btc()));
    assert_eq!(TradeAddress::parse("binance", Some("SUB"), "BTCUSDT"), Some(sub("BTCUSDT")));
    assert_eq!(
        TradeAddress::parse("binance", Some("DEFAULT"), "BTCUSDT"),
        Some(btc()),
        "the wire's DEFAULT is the default account, stored as None"
    );
    // empty, lowercase, the separator, a hyphen, and longer than `MAX_LABEL_LEN` (24)
    for bad in ["", "sub", "A_B", "S-B", "ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789"] {
        assert_eq!(
            TradeAddress::parse("binance", Some(bad), "BTCUSDT"),
            None,
            "{bad:?} is not a label, so it names no account at all"
        );
    }
}

/// Close and Reverse act on the WINDOW account's position, and their order names that account
/// (Review Focus 1, Minor 20): the default account is long 1 and SUB is long 3.
#[test]
fn close_and_reverse_act_on_the_window_accounts_position() {
    let mut s = binance_snap();
    s.portfolio.venues = vec![
        vike_core::VenueBlock {
            positions: vec![position_view("binance", "BTCUSDT", 1.0)],
            ..block("binance", None)
        },
        vike_core::VenueBlock {
            positions: vec![position_view("binance", "BTCUSDT", 3.0)],
            ..block("binance#SUB", AccountLabel::parse("SUB").ok())
        },
    ];
    for (addr, account, close_qty) in [(btc(), "DEFAULT", 1.0), (sub("BTCUSDT"), "SUB", 3.0)] {
        let mut i = DispatchInputs::default();
        i.trade_actions.push((addr.clone(), TradeAction::ClosePosition));
        i.trade_actions.push((addr, TradeAction::Reverse));
        let plan = plan_dispatch(i, &OrderLimits::default(), &s, 1, 0);
        match plan.commands.as_slice() {
            [
                Command::Order(OrderIntent::Submit(close)),
                Command::Order(OrderIntent::Submit(rev)),
            ] => {
                assert_eq!(close.qty, close_qty, "{account}: close its own position");
                assert_eq!(rev.qty, close_qty * 2.0, "{account}: reverse its own position");
                for req in [close, rev] {
                    assert_eq!(
                        req.account.as_ref().map(ToString::to_string).as_deref(),
                        Some(account)
                    );
                }
            }
            other => panic!("{account}: a close and a reverse, got {other:?}"),
        }
    }
}

/// Every command a window intent plans lifts to a wire frame, and a Submit's frame names the account
/// the address names (final review I-1 and I-2: the lift used to drop the account, and has no form
/// for a batch). Close and Reverse included (Minor 20).
#[test]
fn every_window_command_has_a_wire_form_and_a_submit_names_its_account() {
    use crate::backend::tradehub_control::wire_from_command;
    use vike_tradehub_client::WireCommand;
    let mut s = binance_snap();
    s.portfolio.venues = vec![
        vike_core::VenueBlock {
            positions: vec![position_view("binance", "BTCUSDT", 1.0)],
            ..block("binance", None)
        },
        vike_core::VenueBlock {
            positions: vec![position_view("binance", "BTCUSDT", -2.0)],
            ..block("binance#SUB", AccountLabel::parse("SUB").ok())
        },
    ];
    s.orders = vec![
        order_view("d-1", None, 1),
        order_view("d-2", None, -1),
        order_view("s-1", AccountLabel::parse("SUB").ok(), 1),
    ];
    for (addr, want) in [(btc(), Some("DEFAULT")), (sub("BTCUSDT"), Some("SUB"))] {
        let actions = vec![
            place(Origin::Ticket, None),
            place(Origin::Ladder, None),
            TradeAction::Modify { coid: "d-1".into(), new_price: 99.0 },
            TradeAction::Cancel(vec!["d-1".into(), "d-2".into()]),
            TradeAction::CancelSide(1),
            TradeAction::CancelAll,
            TradeAction::ClosePosition,
            TradeAction::Reverse,
        ];
        for act in actions {
            let label = format!("{act:?} at {addr:?}");
            let mut i = DispatchInputs::default();
            i.trade_actions.push((addr.clone(), act));
            let plan = plan_dispatch(i, &OrderLimits::default(), &s, 1, 0);
            assert!(!plan.commands.is_empty(), "{label}: planned nothing");
            for cmd in &plan.commands {
                match wire_from_command(cmd) {
                    Some(WireCommand::Submit(req)) => {
                        assert_eq!(req.account.as_deref(), want, "{label}")
                    }
                    Some(_) => {}
                    None => panic!("{label}: no wire form for {cmd:?}"),
                }
            }
        }
    }
}

/// ⚠ **A non-finite entry price never reaches the wire as a bracket** (carry m-1 of the
/// node-bracket review, ported from the old ticket's test onto the Trade window's intents).
/// `WireBracketSpec::entry_price` serializes `Some(NaN)`/`Some(±inf)` as `null`, and `null` is a
/// MARKET entry on the node — so the only guard is the SENDER's, and the sender is this planner:
/// `admit_bracket` validates the entry leg, which refuses the non-finite price. The refusal carries
/// the window's account, as every Trade window refusal does.
#[test]
fn a_non_finite_entry_price_produces_no_bracket_command() {
    let s = perp_snap();
    let exits = Some(Exits { take_profit: 120.0, stop_loss: 90.0 });
    for price in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        let mut i = DispatchInputs::default();
        i.trade_actions.push((btc_perp(), ticket(OrderType::Limit, 1.0, Some(price), exits)));
        let plan = plan_dispatch(i, &OrderLimits::default(), &s, 0, 0);
        assert!(
            !plan.commands.iter().any(|c| matches!(c, Command::Order(OrderIntent::Bracket(_)))),
            "a {price} entry price must not become a bracket the lift would send at market: {:?}",
            plan.commands
        );
        assert_eq!(
            plan.rejects.iter().map(|r| r.reason).collect::<Vec<_>>(),
            vec![DispatchRejectReason::Preview(OrderReject::NonFiniteQtyOrPrice)],
            "{price}"
        );
        assert_eq!(plan.rejects[0].source, SubmitSource::TradeBracket, "{price}");
    }
    // …while a MARKET entry with TP and SL is a bracket entering at market, by design.
    let mut i = DispatchInputs::default();
    i.trade_actions.push((btc_perp(), ticket(OrderType::Market, 1.0, None, exits)));
    let plan = plan_dispatch(i, &OrderLimits::default(), &s, 0, 0);
    match &plan.commands[..] {
        [Command::Order(OrderIntent::Bracket(spec))] => assert_eq!(spec.entry_price, None),
        other => panic!("a Market entry with TP/SL is a market-entry bracket: {other:?}"),
    }
}
