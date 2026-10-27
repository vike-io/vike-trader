//! Every submit source is capped: the gate, the `classify` pin, brackets, close and reverse.

use super::*;
use std::assert_matches;

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
        let plan = plan_dispatch(one_intent_for(source), &limits, &s, 0, 7, None);
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
        let plan = plan_dispatch(one_intent_for(source), &limits, &s, 0, 7, None);
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
    let plan = plan_dispatch(inputs, &limits, &s, 0, 0, None);

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
            account: None,
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
    let plan = plan_dispatch(inputs, &OrderLimits::default(), &s, 0, 0, None);
    for cmd in &plan.commands {
        if let Command::Order(intent) = cmd {
            assert_matches!(
                classify(intent),
                Class::Capped | Class::NotAnOrderWrite,
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
            (empty.clone(), ticket(TicketKind::Market, 1.0, None, None)),
            (
                empty.clone(),
                ticket(
                    TicketKind::Limit,
                    1.0,
                    Some(1.5),
                    Some(Exits { take_profit: 2.0, stop_loss: 1.0 }),
                ),
            ),
            (empty, TradeAction::ClosePosition),
            (
                TradeAddress { symbol: String::new(), ..btc() },
                ticket(TicketKind::Market, 1.0, None, None),
            ),
        ],
        ..DispatchInputs::default()
    };
    let plan = plan_dispatch(inputs, &OrderLimits::default(), &binance_snap(), 0, 0, None);
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
        trade_actions: vec![(primary(&s), ticket(TicketKind::Limit, 0.2, Some(50_000.0), None))],
        ..DispatchInputs::default()
    };
    let plan = plan_dispatch(inputs, &limits, &s, 0, 0, None);
    assert_eq!(submit_count(&plan), 0);
    assert_eq!(plan.rejects.len(), 1);
    assert_eq!(plan.rejects[0].source, SubmitSource::Trade);
    assert_matches!(
        plan.rejects[0].reason,
        DispatchRejectReason::Preview(OrderReject::NotionalAboveMax { .. })
    );
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
    grid.insert("BTC-28MAR25-100000-C".to_string(), vike_model::ContractValue::linear(100.0));
    s.portfolio.venues.push(vike_exec::VenueBlock {
        venue: "deribit".into(),
        route_key: "deribit".into(),
        contract_values: std::sync::Arc::new(grid),
        ..Default::default()
    });
    assert_eq!(
        s.contract_value_of("deribit", "BTC-28MAR25-100000-C"),
        vike_model::ContractValue::linear(100.0)
    );

    let inputs = DispatchInputs {
        trade_actions: vec![(primary(&s), ticket(TicketKind::Limit, 2.0, Some(150.0), None))],
        ..DispatchInputs::default()
    };
    let plan = plan_dispatch(inputs, &limits, &s, 0, 0, None);
    assert_eq!(submit_count(&plan), 0, "2 × 150 × 100 = 30_000 must not pass a 1_000 cap");
    assert_matches!(
        plan.rejects[0].reason,
        DispatchRejectReason::Preview(OrderReject::NotionalAboveMax { notional, .. })
            if notional == 30_000.0
    );

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
            TicketKind::Limit,
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
    let plan = plan_dispatch(inputs, &limits, &s, 0, 0, None);
    assert_eq!(submit_count(&plan), 1);
    assert_plan_respects_limits(&plan, &limits, &s);

    // … and an extra zero on the take-profit (5_000 > 1_000) refuses the WHOLE bracket.
    let inputs = DispatchInputs {
        trade_actions: vec![(primary(&s), bracket(500_000.0))],
        ..DispatchInputs::default()
    };
    let plan = plan_dispatch(inputs, &limits, &s, 0, 0, None);
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
        trade_actions: vec![(primary(&s), ticket(TicketKind::Limit, 1.0, Some(100.0), exits))],
        ..DispatchInputs::default()
    };
    let plan = plan_dispatch(inputs, &OrderLimits::default(), &s, 0, 0, None);
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
            (primary(&s), ticket(TicketKind::Limit, 1.0, Some(100.0), exits)),
            (primary(&s), ticket(TicketKind::Market, 1.0, None, None)),
        ],
        ..DispatchInputs::default()
    };
    let plan = plan_dispatch(inputs, &OrderLimits::default(), &s, 10, 2, None);
    assert_eq!(plan.next_win_n, 11, "bracket mints nothing; the plain ticket mints one");

    // a REFUSED ticket still burns its coid
    let inputs = DispatchInputs {
        trade_actions: vec![(primary(&s), ticket(TicketKind::Limit, 1.0, Some(100.0), None))],
        ..DispatchInputs::default()
    };
    let plan = plan_dispatch(inputs, &capped(1.0), &s, 10, 2, None);
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
                ticket(TicketKind::Market, 1.0, None, None),
            ),
            (sub("BTCUSDT"), ticket(TicketKind::Market, 1.0, None, None)),
            (btc_perp(), ticket(TicketKind::Limit, 1.0, None, None)),
        ],
        ..DispatchInputs::default()
    };
    let plan = plan_dispatch(inputs, &OrderLimits::default(), &s, 10, 2, None);
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
        s.orders.push(vike_exec::OrderView {
            venue: venue.into(),
            symbol: symbol.into(),
            ..order_view(coid, None, side)
        });
    }
    let inputs = DispatchInputs {
        trade_actions: vec![(btc(), TradeAction::CancelSide(1)), (btc(), TradeAction::CancelAll)],
        ..DispatchInputs::default()
    };
    let plan = plan_dispatch(inputs, &OrderLimits::default(), &s, 0, 0, None);
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
        plan_dispatch(inputs, &OrderLimits::default(), &s, 0, 0, None).commands.len()
    };
    let allowed =
        vike_model::VENUES.iter().copied().find(|&v| vike_venues::caps_for(v).allows_modify());
    let denied =
        vike_model::VENUES.iter().copied().find(|&v| !vike_venues::caps_for(v).allows_modify());
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
    let plan = plan_dispatch(inputs, &OrderLimits::default(), &s, 0, 0, None);
    match &plan.commands[..] {
        [Command::Order(OrderIntent::Submit(close)), Command::Order(OrderIntent::Submit(rev))] => {
            assert_eq!(close.side, -1, "closing a long SELLs");
            assert_eq!(close.qty, 2.0);
            assert!(close.reduce_only);
            assert_eq!(close.order_type, vike_model::OrderType::Market);
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
    assert!(plan_dispatch(inputs, &OrderLimits::default(), &flat, 0, 0, None).commands.is_empty());
}

/// The emission ORDER the command lane sees: the Trade window's intents in drain order → the
/// Account window's cancels → cockpit → options → option cancels.
#[test]
fn emission_order_is_preserved() {
    let s = binance_snap();
    let inputs = DispatchInputs {
        trade_actions: vec![
            (btc(), ticket(TicketKind::Market, 1.0, None, None)),
            (
                btc(),
                TradeAction::Place {
                    side: 1,
                    order_type: TicketKind::Limit,
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
            kind: vike_marketdata::OptionKind::Call,
            strike: 1.0,
        }],
        opt_cancels: vec!["oc".into()],
    };
    let plan = plan_dispatch(inputs, &OrderLimits::default(), &s, 0, 0, None);
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
// Client order ids: unique per session, deterministic without one
// -----------------------------------------------------------------------------------------

/// The ids of the orders a plan submits, in order.
fn submitted(plan: &DispatchPlan) -> Vec<String> {
    plan.commands
        .iter()
        .filter_map(|c| match c {
            Command::Order(OrderIntent::Submit(r)) => Some(r.client_order_id.clone()),
            _ => None,
        })
        .collect()
}

/// One frame per id prefix (`ui`, `dom`, `poly`, `opt`) through `mint`, the way the shell hands the
/// planner its mint, with the window counter at `next_win_n`.
fn one_order_per_prefix(mint: &mut ClientOrderIdGenerator, next_win_n: u32) -> Vec<String> {
    let s = snap_with_position("binance", "BTCUSDT", 2.0);
    [SubmitSource::Trade, SubmitSource::Dom, SubmitSource::Cockpit, SubmitSource::Options]
        .into_iter()
        .flat_map(|source| {
            let inputs = one_intent_for(source);
            let limits = OrderLimits::default();
            submitted(&plan_dispatch(inputs, &limits, &s, next_win_n, 0, Some(&mut *mint)))
        })
        .collect()
}

/// ⚠ **A new desktop session must not reuse the ids of the last one.** The window counter starts at
/// the window count at every launch, so two launches with the same layout mint the same
/// `ui-1-0`-style ids; the node's door refuses a `Submit` whose id an earlier order carries, so the
/// first orders of the second session were refused as repeats. With a session mint the ids differ.
#[test]
fn a_new_session_does_not_repeat_the_ids_of_the_last_one() {
    let last = one_order_per_prefix(&mut ClientOrderIdGenerator::new(Some("aaaaaaaa")), 3);
    let this = one_order_per_prefix(&mut ClientOrderIdGenerator::new(Some("bbbbbbbb")), 3);
    assert_eq!(last.len(), 4, "one order per prefix: {last:?}");
    assert_eq!(this.len(), 4, "one order per prefix: {this:?}");
    for id in &this {
        assert!(!last.contains(id), "the new session repeated {id}: {last:?} then {this:?}");
    }
}

/// ...and neither does one session repeat itself when the window counter goes BACK: File → Open
/// Workspace and a named layout's Load set `next_win_n` to the new layout's window count in the
/// middle of a session, so ids numbered from it came round again.
#[test]
fn ids_do_not_repeat_when_the_window_counter_goes_back() {
    let mut mint = ClientOrderIdGenerator::new(Some("cafef00d"));
    let mut seen = Vec::new();
    for next_win_n in [5, 5, 2, 0, 2] {
        seen.extend(one_order_per_prefix(&mut mint, next_win_n));
    }
    let mut unique = seen.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(unique.len(), seen.len(), "an id was minted twice in one session: {seen:?}");
}

/// The session mint spells `{prefix}{session}{seq}` and still advances the window counter by one per
/// id, as the counter's other readers (`DispatchPlan::next_win_n`) expect; with no mint the
/// deterministic `{prefix}-{next_win_n}-{shot_n}` is untouched, so the offscreen goldens and
/// `VIKE_SHOT` never depend on a random session.
#[test]
fn without_a_mint_the_ids_keep_the_historical_spelling() {
    let s = snap_with_position("binance", "BTCUSDT", 2.0);
    let limits = OrderLimits::default();

    let plan = plan_dispatch(one_intent_for(SubmitSource::Trade), &limits, &s, 4, 9, None);
    assert_eq!(submitted(&plan), ["ui-4-9"]);
    assert_eq!(plan.next_win_n, 5);

    let mut mint = ClientOrderIdGenerator::new(Some("cafef00d"));
    let plan = plan_dispatch(one_intent_for(SubmitSource::Dom), &limits, &s, 4, 9, Some(&mut mint));
    assert_eq!(submitted(&plan), ["domcafef00d0"]);
    assert_eq!(plan.next_win_n, 5, "the window counter advances once per id in both modes");
}

/// The id must be taken VERBATIM by every venue: alphanumeric (OKX refuses anything else, and
/// Binance's broker-prefix inverse splits on the last `-`) and inside the tightest per-venue budget,
/// which is Binance behind the longest broker code. Tested with the longest prefix at the largest
/// sequence that still leaves every venue its floor of sequence digits.
#[test]
fn an_id_fits_the_tightest_venue_budget() {
    use vike_model::orders::client_order_id::is_valid_crypto_coid;
    use vike_venues::venue_coid_budget::{CoidWire, MIN_SEQ_DIGITS, coid_budget_for};

    let tightest = vike_model::VENUES
        .iter()
        .filter_map(|v| match coid_budget_for(v) {
            CoidWire::Verbatim { max_len, adapter_overhead, .. } => {
                Some(max_len - adapter_overhead)
            }
            CoidWire::NotOnTheWire => None,
        })
        .min()
        .expect("the roster has a venue that carries the id");

    // The last four ids of a session that minted `10^(floor + 1) - 1` orders: nine digits each.
    let last = 10u64.pow(MIN_SEQ_DIGITS as u32 + 1) - 1;
    let mut mint = ClientOrderIdGenerator::resume("cafef00d".into(), last - 3);
    let ids = one_order_per_prefix(&mut mint, 0);
    assert_eq!(ids.iter().map(String::len).max(), Some("poly".len() + 8 + 9), "{ids:?}");
    for id in ids {
        assert!(is_valid_crypto_coid(&id), "{id} is not a valid crypto coid");
        let len = id.len();
        assert!(len <= tightest, "{id} is {len} characters; the tightest venue takes {tightest}");
    }
}
