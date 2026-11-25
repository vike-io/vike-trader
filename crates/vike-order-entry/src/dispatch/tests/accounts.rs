//! Multi-account addressing: tradability, attribution, cancels and the wire form (spec §4.2).

use super::*;
use std::assert_matches;

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
        let mut s = binance_snapshot();
        s.portfolio.venues = vec![sub_block];
        assert!(!tradable(&s, &btc()), "no block carries route key `binance`");
        let mut i = OrderEntryDispatchInputs::default();
        i.trade_actions.push((btc(), place(TradeWindowOrigin::Ticket, None)));
        i.trade_actions.push((btc(), TradeWindowAction::ClosePosition));
        let plan = plan_dispatch(i, &OrderLimits::default(), &s, 1, 0, None);
        assert!(plan.commands.is_empty(), "nothing may reach engine 0: {:?}", plan.commands);
        assert_eq!(plan.rejects.len(), 2);
        assert!(plan.rejects.iter().all(|r| r.reason == OrderEntryDispatchRejectReason::NotTraded));
    }
}

/// …and the orders of an account the server does not run are nobody's: a window addressed to it
/// cancels nothing, even on an older node whose orders name no account (where they would read as
/// the default account's and be SUB's).
#[test]
fn an_account_the_server_does_not_run_owns_no_orders() {
    let mut s = binance_snapshot();
    s.portfolio.venues = vec![older("binance#SUB", AccountLabel::parse("SUB").ok())];
    s.orders = vec![std::sync::Arc::new(order_view("s-1", None, 1))];
    let mut i = OrderEntryDispatchInputs::default();
    i.trade_actions.push((btc(), TradeWindowAction::CancelAll));
    i.trade_actions.push((btc(), TradeWindowAction::CancelSide(1)));
    let plan = plan_dispatch(i, &OrderLimits::default(), &s, 1, 0, None);
    assert!(plan.commands.is_empty(), "SUB's order is not this window's: {:?}", plan.commands);
}

/// A window's order carries the account it names (spec §4.2).
#[test]
fn a_window_order_carries_its_account() {
    let s = with_sub_account(&["BTCUSDT"]);
    let mut i = OrderEntryDispatchInputs::default();
    i.trade_actions.push((sub_account("BTCUSDT"), place(TradeWindowOrigin::Ticket, None)));
    let plan = plan_dispatch(i, &OrderLimits::default(), &s, 1, 0, None);
    match plan.commands.as_slice() {
        [Command::Order(OrderIntent::Submit(req))] => {
            assert_eq!(req.account.as_ref().map(ToString::to_string).as_deref(), Some("SUB"));
            assert_eq!(req.venue, "binance");
        }
        other => panic!("one submit, got {other:?}"),
    }
}

/// A symbol an account that keeps to its own symbol does not trade is refused before any command
/// (Review Focus 2): a PAPER account, or a venue the table keeps to one symbol.
#[test]
fn an_untradable_address_is_refused_before_any_command() {
    let mut paper = with_sub_account(&["ETHUSDT"]);
    paper.portfolio.venues[1].tier = Some(vike_model::AccountTier::Paper);
    let mut deribit = with_sub_account(&["ETHUSDT"]);
    deribit.portfolio.venues[1].venue = "deribit".into();
    deribit.portfolio.venues[1].admits_symbols = false;
    for s in [paper, deribit] {
        let mut i = OrderEntryDispatchInputs::default();
        i.trade_actions.push((sub_account("BTCUSDT"), place(TradeWindowOrigin::Ladder, None)));
        let plan = plan_dispatch(i, &OrderLimits::default(), &s, 1, 0, None);
        assert!(plan.commands.is_empty());
        assert_eq!(plan.rejects[0].reason, OrderEntryDispatchRejectReason::NotTraded);
    }
}

/// Orders follow the picked symbol: a DEMO or LIVE account of a venue the table follows takes an
/// order on a symbol its engine was not started on, and the order carries THAT symbol and the
/// account to the wire, spot and perpetual alike.
#[test]
fn a_demo_or_live_account_takes_the_picked_symbol_and_the_order_names_it() {
    for tier in [vike_model::AccountTier::Demo, vike_model::AccountTier::Live] {
        let mut s = with_sub_account(&["ETHUSDT"]);
        s.portfolio.venues[1].tier = Some(tier);
        for symbol in ["BTCUSDT", "BTCUSDT.P", "SOLUSDT"] {
            assert!(tradable(&s, &sub_account(symbol)), "{tier:?} {symbol}");
            let mut i = OrderEntryDispatchInputs::default();
            i.trade_actions.push((sub_account(symbol), place(TradeWindowOrigin::Ticket, None)));
            let plan = plan_dispatch(i, &OrderLimits::default(), &s, 1, 0, None);
            match plan.commands.as_slice() {
                [Command::Order(OrderIntent::Submit(req))] => {
                    assert_eq!(req.symbol, symbol, "the picked symbol reaches the wire command");
                    assert_eq!(req.venue, "binance");
                    assert_eq!(
                        req.account.as_ref().map(ToString::to_string).as_deref(),
                        Some("SUB")
                    );
                }
                other => panic!("{tier:?} {symbol}: one submit, got {other:?}"),
            }
        }
    }
}

/// A TP/SL bracket stays on the symbol the engine was started on: the node judges whether the lane
/// holds the stop-loss from the ENGINE's symbol, so on another symbol the verdict is about a
/// different instrument. The window says the same before the click (`bracket_why`).
#[test]
fn a_bracket_on_another_symbol_than_the_engines_is_refused() {
    let s = perp_snapshot();
    let exits = Some(TradeWindowTicketExits { take_profit: 160.0, stop_loss: 140.0 });
    let eth = TradeWindowAddress { symbol: "ETHUSDT.P".into(), ..btc_perp() };
    assert!(tradable(&s, &eth), "the order itself follows the symbol");
    assert!(!bracket_on_mounted_symbol(&s, &eth));
    assert!(bracket_on_mounted_symbol(&s, &btc_perp()));
    let mut i = OrderEntryDispatchInputs::default();
    i.trade_actions.push((eth, place(TradeWindowOrigin::Ticket, exits)));
    let plan = plan_dispatch(i, &OrderLimits::default(), &s, 1, 0, None);
    assert!(plan.commands.is_empty(), "{:?}", plan.commands);
    assert_eq!(plan.rejects[0].reason, OrderEntryDispatchRejectReason::BracketNeedsMountedSymbol);
}

/// An older node names no symbol: only the primary market trades, and only on the primary
/// (default) account's block.
#[test]
fn an_older_node_trades_only_its_primary_on_the_default_account() {
    let mut s = binance_snapshot();
    s.portfolio.venues =
        vec![older("binance", None), older("binance#SUB", AccountLabel::parse("SUB").ok())];
    assert!(tradable(&s, &btc()), "the primary market on the default account");
    let eth = TradeWindowAddress { symbol: "ETHUSDT".into(), ..btc() };
    assert!(!tradable(&s, &eth), "another symbol");
    assert!(
        !tradable(&s, &sub_account("BTCUSDT")),
        "a named account that is not the primary block"
    );
}

/// A priced order type without its price is refused, never sent at MARKET: the old ticket's
/// `unwrap_or` habit would have turned a Limit with no price into a market order.
#[test]
fn a_priced_order_without_a_price_is_refused_not_sent_at_market() {
    let s = binance_snapshot();
    for order_type in [TradeWindowTicketKind::Limit, TradeWindowTicketKind::Stop] {
        for exits in [None, Some(TradeWindowTicketExits { take_profit: 160.0, stop_loss: 140.0 })] {
            let act = ticket(order_type, 1.0, None, exits);
            let mut i = OrderEntryDispatchInputs::default();
            i.trade_actions.push((btc(), act.clone()));
            let plan = plan_dispatch(i, &OrderLimits::default(), &s, 0, 0, None);
            assert!(plan.commands.is_empty(), "{act:?} planned {:?}", plan.commands);
            assert_eq!(plan.rejects.len(), 1, "{act:?}");
            // The REASON, not just the silence: a Stop with exits would also be refused by the
            // bracket rule, so only the reason proves the price check ran.
            assert_eq!(
                plan.rejects[0].reason,
                OrderEntryDispatchRejectReason::MissingPrice,
                "{act:?}"
            );
        }
    }
}

/// **Post only becomes the order's lifetime on the way out, and rides the wire the CLI's `--post-only`
/// rides.** The ticket's post-only limit buy is planned as a `Submit` whose `time_in_force` is
/// `PostOnly`, and the desktop's lift writes it as the `post-only` word; the same order without the
/// flag is a plain GTC limit and its frame carries no lifetime key at all (byte-identical to before).
#[test]
fn a_post_only_ticket_order_goes_out_as_a_post_only_limit_order() {
    let s = binance_snapshot();
    let buy = |post_only| TradeWindowAction::Place {
        side: 1,
        order_type: TradeWindowTicketKind::Limit,
        price: Some(150.0),
        quantity: 2.0,
        reduce_only: false,
        post_only,
        exits: None,
        origin: TradeWindowOrigin::Ticket,
    };
    for (post_only, tif, word) in [
        (false, vike_model::TimeInForce::Gtc, None),
        (true, vike_model::TimeInForce::PostOnly, Some("post-only")),
    ] {
        let mut i = OrderEntryDispatchInputs::default();
        i.trade_actions.push((btc(), buy(post_only)));
        let plan = plan_dispatch(i, &OrderLimits::default(), &s, 1, 0, None);
        assert!(plan.rejects.is_empty(), "{post_only}: {:?}", plan.rejects);
        let [command @ Command::Order(OrderIntent::Submit(req))] = plan.commands.as_slice() else {
            panic!("{post_only}: one submit, got {:?}", plan.commands);
        };
        assert_eq!(req.time_in_force, tif, "{post_only}");
        assert_eq!(req.order_type, vike_model::OrderType::Limit, "{post_only}");
        match vike_tradehub_client::command_lift::wire_from_command(command) {
            Some(vike_tradehub_client::wire::CommandWire::Submit(wire)) => {
                assert_eq!(wire.time_in_force.as_deref(), word, "{post_only}: the wire word");
                assert_eq!(wire.gtd_expiry, None);
            }
            other => panic!("{post_only}: a submit lifts to a submit, got {other:?}"),
        }
    }
}

/// A post-only order that carries TP/SL exits is REFUSED, with its own reason: a bracket's entry is
/// built from a `OrderBracket`, which has no lifetime, so planning it would send an entry that takes
/// liquidity under a ticket that said maker-only. The control is the same bracket without the flag,
/// which is admitted as a bracket.
#[test]
fn a_post_only_order_with_exits_is_refused_never_sent_without_the_flag() {
    let s = perp_snapshot();
    let exits = Some(TradeWindowTicketExits { take_profit: 160.0, stop_loss: 140.0 });
    let bracket = |post_only| TradeWindowAction::Place {
        side: 1,
        order_type: TradeWindowTicketKind::Limit,
        price: Some(150.0),
        quantity: 2.0,
        reduce_only: false,
        post_only,
        exits,
        origin: TradeWindowOrigin::Ticket,
    };
    let mut i = OrderEntryDispatchInputs::default();
    i.trade_actions.push((btc_perp(), bracket(false)));
    let plan = plan_dispatch(i, &OrderLimits::default(), &s, 1, 0, None);
    assert_matches!(plan.commands.as_slice(), [Command::Order(OrderIntent::Bracket(_))], "CONTROL");

    let mut i = OrderEntryDispatchInputs::default();
    i.trade_actions.push((btc_perp(), bracket(true)));
    let plan = plan_dispatch(i, &OrderLimits::default(), &s, 1, 0, None);
    assert!(plan.commands.is_empty(), "{:?}", plan.commands);
    assert_eq!(plan.rejects.len(), 1);
    assert_eq!(plan.rejects[0].reason, OrderEntryDispatchRejectReason::PostOnlyHasNoBracket);
    assert_eq!(
        (plan.rejects[0].source, plan.rejects[0].venue.as_str()),
        (OrderEntryDispatchSource::TradeBracket, "binance")
    );
}

/// A bracket is refused wherever it cannot be honest: on an engine whose lane cannot hold the
/// stop-loss (a labelled account's and a venue's second-engine default included: a bracket NAMES its
/// account, so a second account on the venue is no reason), for a Stop entry and for a reduce-only
/// entry (Ruling R3, widened by I6; spec §3.6; I-1 of the final review, slice B).
#[test]
fn a_bracket_is_refused_where_it_cannot_be_honest() {
    let exits = Some(TradeWindowTicketExits { take_profit: 160.0, stop_loss: 140.0 });
    let cases = [
        (
            with_sub_account(&["BTCUSDT"]),
            sub_account("BTCUSDT"),
            place(TradeWindowOrigin::Ticket, exits),
            OrderEntryDispatchRejectReason::BracketNeedsPerpEngine,
        ),
        (
            with_sub_account(&["BTCUSDT"]),
            btc(),
            place(TradeWindowOrigin::Ticket, exits),
            OrderEntryDispatchRejectReason::BracketNeedsPerpEngine,
        ),
        (
            binance_snapshot(),
            btc(),
            place(TradeWindowOrigin::Ticket, exits),
            OrderEntryDispatchRejectReason::BracketNeedsPerpEngine,
        ),
        (
            perp_snapshot(),
            btc_perp(),
            place_with(TradeWindowOrigin::Ticket, TradeWindowTicketKind::Stop, false, exits),
            OrderEntryDispatchRejectReason::BracketNeedsMarketOrLimit,
        ),
        (
            perp_snapshot(),
            btc_perp(),
            place_with(TradeWindowOrigin::Ticket, TradeWindowTicketKind::Limit, true, exits),
            OrderEntryDispatchRejectReason::BracketNeedsOpeningEntry,
        ),
    ];
    for (s, address, act, want) in cases {
        let mut i = OrderEntryDispatchInputs::default();
        i.trade_actions.push((address, act));
        let plan = plan_dispatch(i, &OrderLimits::default(), &s, 1, 0, None);
        assert!(plan.commands.is_empty(), "{want:?}");
        assert_eq!(plan.rejects[0].reason, want);
        assert_eq!(plan.rejects[0].source, OrderEntryDispatchSource::TradeBracket);
    }
    // …and the positive control: the default account of a single-engine venue, on an engine whose
    // lane holds the stop-loss, takes it.
    let mut i = OrderEntryDispatchInputs::default();
    i.trade_actions.push((btc_perp(), place(TradeWindowOrigin::Ticket, exits)));
    let plan = plan_dispatch(i, &OrderLimits::default(), &perp_snapshot(), 1, 0, None);
    assert_matches!(
        plan.commands.as_slice(),
        [Command::Order(OrderIntent::Bracket(_))],
        "{:?}",
        plan.commands
    );
}

/// A bracket NAMES the account its window addresses, exactly as a plain order does (`wire_account`): a
/// labelled account on the perp lane (the shape of a lane twin) carries its label, the default account of
/// a venue that runs a second engine carries `DEFAULT`, and the default account of a single-engine venue
/// carries NOTHING, so a node from before the field still receives the frame it always did.
#[test]
fn a_bracket_carries_the_account_its_window_addresses() {
    let exits = Some(TradeWindowTicketExits { take_profit: 160.0, stop_loss: 140.0 });
    let planned = |s: &CoreSnapshot, address: TradeWindowAddress| -> Option<String> {
        let mut i = OrderEntryDispatchInputs::default();
        i.trade_actions.push((address, place(TradeWindowOrigin::Ticket, exits)));
        let plan = plan_dispatch(i, &OrderLimits::default(), s, 1, 0, None);
        match plan.commands.as_slice() {
            [Command::Order(OrderIntent::Bracket(spec))] => {
                Some(spec.account.as_ref().map_or_else(String::new, ToString::to_string))
            }
            other => panic!("one bracket, got {other:?} / {:?}", plan.rejects),
        }
    };
    // SUB's engine is on the perp lane (the default engine is spot, so only SUB takes a bracket).
    let mut labelled = with_sub_account(&["BTCUSDT.P"]);
    assert_eq!(planned(&labelled, sub_account("BTCUSDT.P")).as_deref(), Some("SUB"));
    // The DEFAULT account of that venue on the perp lane: another engine exists, so it is named.
    labelled.portfolio.venues[0].symbol = "BTCUSDT.P".into();
    assert_eq!(planned(&labelled, btc_perp()).as_deref(), Some("DEFAULT"));
    // A single-engine venue: nothing is named, the frame is byte-identical to the pre-field one.
    assert_eq!(planned(&perp_snapshot(), btc_perp()).as_deref(), Some(""));
}

/// M-6 of the final review (slice B): a stopped core and a HALTED account trade nothing in the
/// dispatcher, exactly as in the window. `tradable` itself refuses them, so a Place, a ladder click,
/// a Close and a Reverse plan nothing and are refused, not dropped (a Close under a halt included:
/// the window offers none there, and the dispatcher now agrees with it). A cancel still goes, and
/// a REDUCING account still trades.
#[test]
fn a_stopped_core_or_a_halted_account_trades_nothing_but_still_cancels() {
    let base = || {
        let mut s = binance_snapshot();
        s.portfolio.venues[0].positions = vec![position_view("binance", "BTCUSDT", 1.0)];
        s.orders = vec![std::sync::Arc::new(order_view("d-1", None, 1))];
        s
    };
    let mut faulted = base();
    faulted.fault = Some("handler panicked".into());
    let mut halted = base();
    halted.portfolio.venues[0].trading_state = vike_model::TradingState::Halted;
    for (what, s, why) in [
        ("a stopped core", &faulted, OrderEntryDispatchRejectReason::CoreStopped),
        ("a halted account", &halted, OrderEntryDispatchRejectReason::AccountHalted),
    ] {
        assert!(!tradable(s, &btc()), "{what}");
        for act in [
            place(TradeWindowOrigin::Ticket, None),
            place(TradeWindowOrigin::Ladder, None),
            TradeWindowAction::ClosePosition,
            TradeWindowAction::Reverse,
        ] {
            let mut i = OrderEntryDispatchInputs::default();
            i.trade_actions.push((btc(), act.clone()));
            let plan = plan_dispatch(i, &OrderLimits::default(), s, 1, 0, None);
            assert!(plan.commands.is_empty(), "{what}: {act:?} planned {:?}", plan.commands);
            assert_eq!(plan.rejects.len(), 1, "{what}: {act:?} is refused, never dropped");
            assert_eq!(plan.rejects[0].reason, why, "{what}: {act:?} says why");
        }
        let mut i = OrderEntryDispatchInputs::default();
        i.trade_actions.push((btc(), TradeWindowAction::CancelAll));
        let plan = plan_dispatch(i, &OrderLimits::default(), s, 1, 0, None);
        assert_eq!(cancelled(&plan), ["d-1"], "{what}: a cancel still goes");
    }
    let mut reducing = base();
    reducing.portfolio.venues[0].trading_state = vike_model::TradingState::Reducing;
    assert!(tradable(&reducing, &btc()), "a reducing account still trades");
    let mut i = OrderEntryDispatchInputs::default();
    i.trade_actions.push((btc(), TradeWindowAction::ClosePosition));
    let plan = plan_dispatch(i, &OrderLimits::default(), &reducing, 1, 0, None);
    assert_eq!(submit_count(&plan), 1, "a reducing account closes: {:?}", plan.rejects);
}

/// I-1 of the final review (slice B), step 2: on the shipped daemon's shape — ONE default binance
/// engine mounted on spot `BTCUSDT` — a TP/SL bracket is refused HERE, through `OrderEntryDispatchPlanner::reject`,
/// so the refusal carries the window's address and lands on its own strip; the node would refuse
/// it too, but only into the status bar's control segment. The rule is the node's, read off the
/// ENGINE's mounted symbol (`vike_catalog::engine_lane_holds_stop`): a perp engine takes the same
/// bracket, a plain order on the spot engine still goes, and an engine that does not say what it
/// trades (an older node, which predates the bracket command anyway) is not judged here.
#[test]
fn a_bracket_on_an_engine_whose_lane_holds_no_stop_is_refused_here() {
    let exits = Some(TradeWindowTicketExits { take_profit: 160.0, stop_loss: 140.0 });
    let plan_one = |s: &CoreSnapshot, address: TradeWindowAddress, act: TradeWindowAction| {
        let mut i = OrderEntryDispatchInputs::default();
        i.trade_actions.push((address, act));
        plan_dispatch(i, &OrderLimits::default(), s, 1, 0, None)
    };
    let plan = plan_one(&binance_snapshot(), btc(), place(TradeWindowOrigin::Ticket, exits));
    assert!(plan.commands.is_empty(), "{:?}", plan.commands);
    assert_eq!(
        plan.rejects,
        vec![OrderEntryDispatchReject {
            source: OrderEntryDispatchSource::TradeBracket,
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            account: None,
            reason: OrderEntryDispatchRejectReason::BracketNeedsPerpEngine,
        }],
        "refused at the window's own address"
    );
    let plan = plan_one(&perp_snapshot(), btc_perp(), place(TradeWindowOrigin::Ticket, exits));
    assert_matches!(
        plan.commands.as_slice(),
        [Command::Order(OrderIntent::Bracket(_))],
        "a perp engine takes it: {:?}",
        plan.rejects
    );
    let plan = plan_one(&binance_snapshot(), btc(), place(TradeWindowOrigin::Ticket, None));
    assert_eq!(submit_count(&plan), 1, "TP/SL's rule only: {:?}", plan.rejects);
    let older = snapshot_with_position("binance", "BTCUSDT", 1.0);
    let plan = plan_one(&older, btc(), place(TradeWindowOrigin::Ticket, exits));
    assert!(plan.rejects.is_empty(), "an engine that does not say is not judged: {plan:?}");
    assert_eq!(
        OrderEntryDispatchRejectReason::BracketNeedsPerpEngine.to_string(),
        "a TP/SL bracket on this venue needs an engine on its perp market: the exchange cannot \
         hold the stop-loss on its spot market"
    );
}

/// Plan one intent at `address` on `s`.
fn one_with(
    s: &CoreSnapshot,
    address: TradeWindowAddress,
    act: TradeWindowAction,
) -> OrderEntryDispatchPlan {
    let mut i = OrderEntryDispatchInputs::default();
    i.trade_actions.push((address, act));
    plan_dispatch(i, &OrderLimits::default(), s, 1, 0, None)
}

/// The glue's way in: an account TEXT becomes an address, and text that is not a label refuses the
/// whole address rather than reading as the DEFAULT account (review Minor 2).
#[test]
fn a_trade_address_parses_its_account_or_refuses() {
    assert_eq!(TradeWindowAddress::parse("binance", None, "BTCUSDT"), Some(btc()));
    assert_eq!(
        TradeWindowAddress::parse("binance", Some("SUB"), "BTCUSDT"),
        Some(sub_account("BTCUSDT"))
    );
    assert_eq!(
        TradeWindowAddress::parse("binance", Some("DEFAULT"), "BTCUSDT"),
        Some(btc()),
        "the wire's DEFAULT is the default account, stored as None"
    );
    // empty, lowercase, the separator, a hyphen, and longer than `MAX_LABEL_LEN` (24)
    for bad in ["", "sub", "A_B", "S-B", "ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789"] {
        assert_eq!(
            TradeWindowAddress::parse("binance", Some(bad), "BTCUSDT"),
            None,
            "{bad:?} is not a label, so it names no account at all"
        );
    }
}

/// Close and Reverse act on the WINDOW account's position, and their order names that account
/// (Review Focus 1, Minor 20): the default account is long 1 and SUB is long 3.
#[test]
fn close_and_reverse_act_on_the_window_accounts_position() {
    let mut s = binance_snapshot();
    s.portfolio.venues = vec![
        vike_execution::VenueBlock {
            positions: vec![position_view("binance", "BTCUSDT", 1.0)],
            ..block("binance", None)
        },
        vike_execution::VenueBlock {
            positions: vec![position_view("binance", "BTCUSDT", 3.0)],
            ..block("binance#SUB", AccountLabel::parse("SUB").ok())
        },
    ];
    for (address, account, close_quantity) in
        [(btc(), "DEFAULT", 1.0), (sub_account("BTCUSDT"), "SUB", 3.0)]
    {
        let mut i = OrderEntryDispatchInputs::default();
        i.trade_actions.push((address.clone(), TradeWindowAction::ClosePosition));
        i.trade_actions.push((address, TradeWindowAction::Reverse));
        let plan = plan_dispatch(i, &OrderLimits::default(), &s, 1, 0, None);
        match plan.commands.as_slice() {
            [
                Command::Order(OrderIntent::Submit(close)),
                Command::Order(OrderIntent::Submit(rev)),
            ] => {
                assert_eq!(close.qty, close_quantity, "{account}: close its own position");
                assert_eq!(rev.qty, close_quantity * 2.0, "{account}: reverse its own position");
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
    use vike_tradehub_client::command_lift::wire_from_command;
    use vike_tradehub_client::wire::CommandWire;
    let mut s = binance_snapshot();
    s.portfolio.venues = vec![
        vike_execution::VenueBlock {
            positions: vec![position_view("binance", "BTCUSDT", 1.0)],
            ..block("binance", None)
        },
        vike_execution::VenueBlock {
            positions: vec![position_view("binance", "BTCUSDT", -2.0)],
            ..block("binance#SUB", AccountLabel::parse("SUB").ok())
        },
    ];
    s.orders = vec![
        std::sync::Arc::new(order_view("d-1", None, 1)),
        std::sync::Arc::new(order_view("d-2", None, -1)),
        std::sync::Arc::new(order_view("s-1", AccountLabel::parse("SUB").ok(), 1)),
    ];
    for (address, want) in [(btc(), Some("DEFAULT")), (sub_account("BTCUSDT"), Some("SUB"))] {
        let actions = vec![
            place(TradeWindowOrigin::Ticket, None),
            place(TradeWindowOrigin::Ladder, None),
            TradeWindowAction::Modify { order_client_id: "d-1".into(), new_price: 99.0 },
            TradeWindowAction::Cancel(vec!["d-1".into(), "d-2".into()]),
            TradeWindowAction::CancelSide(1),
            TradeWindowAction::CancelAll,
            TradeWindowAction::ClosePosition,
            TradeWindowAction::Reverse,
        ];
        for act in actions {
            let label = format!("{act:?} at {address:?}");
            let mut i = OrderEntryDispatchInputs::default();
            i.trade_actions.push((address.clone(), act));
            let plan = plan_dispatch(i, &OrderLimits::default(), &s, 1, 0, None);
            assert!(!plan.commands.is_empty(), "{label}: planned nothing");
            for command in &plan.commands {
                match wire_from_command(command) {
                    Some(CommandWire::Submit(req)) => {
                        assert_eq!(req.account.as_deref(), want, "{label}")
                    }
                    Some(_) => {}
                    None => panic!("{label}: no wire form for {command:?}"),
                }
            }
        }
    }
}

/// ⚠ **A non-finite entry price never reaches the wire as a bracket** (carry m-1 of the
/// node-bracket review, ported from the old ticket's test onto the Trade window's intents).
/// `OrderBracketWire::entry_price` serializes `Some(NaN)`/`Some(±inf)` as `null`, and `null` is a
/// MARKET entry on the node — so the only guard is the SENDER's, and the sender is this planner:
/// `admit_bracket` validates the entry leg, which refuses the non-finite price. The refusal carries
/// the window's account, as every Trade window refusal does.
#[test]
fn a_non_finite_entry_price_produces_no_bracket_command() {
    let s = perp_snapshot();
    let exits = Some(TradeWindowTicketExits { take_profit: 120.0, stop_loss: 90.0 });
    for price in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        let mut i = OrderEntryDispatchInputs::default();
        i.trade_actions
            .push((btc_perp(), ticket(TradeWindowTicketKind::Limit, 1.0, Some(price), exits)));
        let plan = plan_dispatch(i, &OrderLimits::default(), &s, 0, 0, None);
        assert!(
            !plan.commands.iter().any(|c| matches!(c, Command::Order(OrderIntent::Bracket(_)))),
            "a {price} entry price must not become a bracket the lift would send at market: {:?}",
            plan.commands
        );
        assert_eq!(
            plan.rejects.iter().map(|r| r.reason).collect::<Vec<_>>(),
            vec![OrderEntryDispatchRejectReason::Preview(
                OrderLimitsError::NonFiniteQuantityOrPrice
            )],
            "{price}"
        );
        assert_eq!(plan.rejects[0].source, OrderEntryDispatchSource::TradeBracket, "{price}");
    }
    // …while a MARKET entry with TP and SL is a bracket entering at market, by design.
    let mut i = OrderEntryDispatchInputs::default();
    i.trade_actions.push((btc_perp(), ticket(TradeWindowTicketKind::Market, 1.0, None, exits)));
    let plan = plan_dispatch(i, &OrderLimits::default(), &s, 0, 0, None);
    match &plan.commands[..] {
        [Command::Order(OrderIntent::Bracket(spec))] => assert_eq!(spec.entry_price, None),
        other => panic!("a Market entry with TP/SL is a market-entry bracket: {other:?}"),
    }
}

#[cfg(test)]
mod attribution;
#[cfg(test)]
mod lane_twin;
