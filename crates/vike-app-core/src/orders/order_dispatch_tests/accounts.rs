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
    for order_type in [TicketKind::Limit, TicketKind::Stop] {
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
            place_with(Origin::Ticket, TicketKind::Stop, false, exits),
            DispatchRejectReason::BracketNeedsMarketOrLimit,
        ),
        (
            perp_snap(),
            btc_perp(),
            place_with(Origin::Ticket, TicketKind::Limit, true, exits),
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
    assert_matches!(
        plan.commands.as_slice(),
        [Command::Order(OrderIntent::Bracket(_))],
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
    halted.portfolio.venues[0].trading_state = vike_model::TradingState::Halted;
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
    reducing.portfolio.venues[0].trading_state = vike_model::TradingState::Reducing;
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
    assert_matches!(
        plan.commands.as_slice(),
        [Command::Order(OrderIntent::Bracket(_))],
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

/// On an older node (no `tier` on its blocks) a venue with two blocks cannot attribute its orders:
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
    // …and the positive control: the same orders on a node that publishes `tier` are attributed,
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
    use vike_model::OrderStatus;
    let sub_label = || AccountLabel::parse("SUB").ok();
    let orders = vec![
        order_view("d", None, 1),
        order_view("s", sub_label(), 1),
        order_view("x", AccountLabel::parse("OTHER").ok(), 1),
        vike_exec::OrderView { venue: "bybit".into(), ..order_view("v", None, 1) },
        vike_exec::OrderView { symbol: "ETHUSDT".into(), ..order_view("y", None, 1) },
        vike_exec::OrderView { status: OrderStatus::Filled, ..order_view("f", None, 1) },
    ];
    let node = |blocks: Vec<vike_exec::VenueBlock>| {
        let mut s = binance_snap();
        s.portfolio.venues = blocks;
        s.portfolio.venues.push(vike_exec::VenueBlock {
            venue: "bybit".into(),
            route_key: "bybit".into(),
            tier: Some(vike_model::AccountTier::Live),
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

    // a node that publishes `tier` names each order's account, so the order's own word is used
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
    use vike_model::OrderStatus;
    let mut s = binance_snap();
    let with = |coid: &str, side: i32, status: OrderStatus| vike_exec::OrderView {
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
    s.portfolio.venues.push(vike_exec::VenueBlock {
        venue: "bybit".into(),
        route_key: "bybit".into(),
        symbol: "ETHUSDT".into(),
        tier: Some(vike_model::AccountTier::Live),
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
        vike_exec::VenueBlock {
            positions: vec![position_view("binance", "BTCUSDT", 1.0)],
            ..block("binance", None)
        },
        vike_exec::VenueBlock {
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
    use vike_tradehub_client::wire::WireCommand;
    let mut s = binance_snap();
    s.portfolio.venues = vec![
        vike_exec::VenueBlock {
            positions: vec![position_view("binance", "BTCUSDT", 1.0)],
            ..block("binance", None)
        },
        vike_exec::VenueBlock {
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
        i.trade_actions.push((btc_perp(), ticket(TicketKind::Limit, 1.0, Some(price), exits)));
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
    i.trade_actions.push((btc_perp(), ticket(TicketKind::Market, 1.0, None, exits)));
    let plan = plan_dispatch(i, &OrderLimits::default(), &s, 0, 0);
    match &plan.commands[..] {
        [Command::Order(OrderIntent::Bracket(spec))] => assert_eq!(spec.entry_price, None),
        other => panic!("a Market entry with TP/SL is a market-entry bracket: {other:?}"),
    }
}
