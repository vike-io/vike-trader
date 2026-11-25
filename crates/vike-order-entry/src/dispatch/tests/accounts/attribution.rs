//! Which orders a window owns: one cancel per order, a default window on a two-engine venue, every
//! node shape, an older node that cannot attribute, `owns_order` clause by clause, and an order that
//! follows its address, not the primary.

use super::*;

/// Cancel all pulls only the window account's orders on its symbol.
#[test]
fn cancel_all_pulls_only_this_accounts_orders() {
    let mut s = with_sub_account(&["BTCUSDT"]);
    s.orders = vec![
        std::sync::Arc::new(order_view("d-1", None, 1)),
        std::sync::Arc::new(order_view("s-1", AccountLabel::parse("SUB").ok(), 1)),
    ];
    for (address, want) in [(sub_account("BTCUSDT"), "s-1"), (btc(), "d-1")] {
        let mut i = OrderEntryDispatchInputs::default();
        i.trade_actions.push((address, TradeWindowAction::CancelAll));
        let plan = plan_dispatch(i, &OrderLimits::default(), &s, 1, 0, None);
        assert_eq!(cancelled(&plan), [want]);
        assert_eq!(plan.commands.len(), 1);
    }
}

/// An order whose account reads `Some(Default)` (a reader that rebuilt it from a wire `DEFAULT`)
/// is the DEFAULT account's order, exactly as an account-less one is (Minor 19).
#[test]
fn a_named_default_order_is_the_default_accounts() {
    let mut s = with_sub_account(&["BTCUSDT"]);
    s.orders = vec![
        std::sync::Arc::new(order_view("d-1", Some(AccountLabel::Default), 1)),
        std::sync::Arc::new(order_view("s-1", AccountLabel::parse("SUB").ok(), 1)),
    ];
    let mut i = OrderEntryDispatchInputs::default();
    i.trade_actions.push((btc(), TradeWindowAction::CancelAll));
    let plan = plan_dispatch(i, &OrderLimits::default(), &s, 1, 0, None);
    assert_eq!(cancelled(&plan), ["d-1"]);
}

/// Every window cancel is one `Cancel` per order id, never a `CancelBatch`: the desktop's lift has no
/// wire form for a batch, so a batch would be dropped with a log line (Ruling R8, final review I-2).
#[test]
fn window_cancels_are_one_cancel_per_order() {
    let mut s = with_sub_account(&["BTCUSDT"]);
    s.orders = vec![
        std::sync::Arc::new(order_view("a", None, 1)),
        std::sync::Arc::new(order_view("b", None, 1)),
        std::sync::Arc::new(order_view("c", None, -1)),
    ];
    let cases = [
        (TradeWindowAction::Cancel(vec!["a".into(), "b".into()]), 2),
        (TradeWindowAction::CancelSide(1), 2),
        (TradeWindowAction::CancelAll, 3),
    ];
    for (act, want) in cases {
        let mut i = OrderEntryDispatchInputs::default();
        i.trade_actions.push((btc(), act));
        let plan = plan_dispatch(i, &OrderLimits::default(), &s, 1, 0, None);
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
    let mut one = binance_snapshot();
    one.portfolio.venues = vec![block("binance", None)];
    assert_eq!(wire_account(&one, &btc()), None, "one engine: None is unambiguous");
    let mut two = binance_snapshot();
    two.portfolio.venues =
        vec![block("binance", None), block("binance#SUB", AccountLabel::parse("SUB").ok())];
    assert_eq!(
        wire_account(&two, &btc()),
        Some(AccountLabel::Default),
        "two engines: name the default"
    );
    assert_eq!(wire_account(&two, &sub_account("BTCUSDT")), AccountLabel::parse("SUB").ok());
    let named_default = TradeWindowAddress { account: Some(AccountLabel::Default), ..btc() };
    assert_eq!(
        wire_account(&one, &named_default),
        None,
        "a `Some(Default)` address is the default account, spelled as one"
    );
}

/// **The desktop's send path always resolves ONE account, over the CI box's four shapes** (decision 0254): a window
/// names its account, and the wire leaves it out only on a venue of one engine, which is the one account the
/// node's door accepts a bare venue for. The twin's shape names DEFAULT, which the door routes by lane.
#[test]
fn a_window_order_resolves_to_one_account_on_every_node_shape() {
    let label = |l: &str| AccountLabel::parse(l).ok();
    let node = |venue: &str, engines: &[(&str, Option<&str>)]| {
        let mut s = binance_snapshot();
        s.portfolio.venues = engines
            .iter()
            .map(|(key, account)| vike_execution::VenueBlock {
                venue: venue.into(),
                ..block(key, account.and_then(label))
            })
            .collect();
        s
    };
    let wire = |s: &CoreSnapshot, venue: &str, account: Option<&str>| {
        let address = TradeWindowAddress {
            venue: venue.into(),
            account: account.and_then(label),
            symbol: "BTC".into(),
        };
        wire_account(s, &address)
    };
    assert_eq!(wire(&node("deribit", &[("deribit", None)]), "deribit", None), None);
    let twin = node("binance", &[("binance", None), ("binance#PERP", Some("PERP"))]);
    assert_eq!(wire(&twin, "binance", None), Some(AccountLabel::Default));
    let hl = node("hyperliquid", &[("hyperliquid", None), ("hyperliquid#LIVE", Some("LIVE"))]);
    assert_eq!(wire(&hl, "hyperliquid", None), Some(AccountLabel::Default));
    assert_eq!(wire(&hl, "hyperliquid", Some("LIVE")), label("LIVE"));
    let bybit = node("bybit", &[("bybit#DEMO", Some("DEMO")), ("bybit#LIVE", Some("LIVE"))]);
    assert_eq!(wire(&bybit, "bybit", Some("DEMO")), label("DEMO"));
    assert_eq!(wire(&bybit, "bybit", Some("LIVE")), label("LIVE"));
}

/// On an older node (no `tier` on its blocks) a venue with two blocks cannot attribute its orders:
/// the window sends no account-scoped cancel there, from EITHER account's window (Ruling R9). The
/// DEFAULT window is the money-relevant direction: every unattributed order reads as the default
/// account's, so it would pull SUB's `s-1` with its own.
#[test]
fn an_older_node_does_not_attribute_orders_so_no_account_scoped_cancel_goes_out() {
    let mut s = binance_snapshot();
    s.portfolio.venues =
        vec![older("binance", None), older("binance#SUB", AccountLabel::parse("SUB").ok())];
    s.orders = vec![
        std::sync::Arc::new(order_view("d-1", None, 1)),
        std::sync::Arc::new(order_view("s-1", None, 1)),
    ];
    assert!(!orders_name_their_account(&s, "binance"));
    for address in [btc(), sub_account("BTCUSDT")] {
        for act in [TradeWindowAction::CancelAll, TradeWindowAction::CancelSide(1)] {
            let label = format!("{act:?} at {address:?}");
            let mut i = OrderEntryDispatchInputs::default();
            i.trade_actions.push((address.clone(), act));
            let plan = plan_dispatch(i, &OrderLimits::default(), &s, 1, 0, None);
            assert!(plan.commands.is_empty(), "{label}: {:?}", plan.commands);
            assert_eq!(
                plan.rejects[0].reason,
                OrderEntryDispatchRejectReason::OrdersUnattributed,
                "{label}"
            );
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
    let mut s = binance_snapshot();
    s.portfolio.venues =
        vec![older("binance", None), older("binance#SUB", AccountLabel::parse("SUB").ok())];
    s.orders = vec![
        std::sync::Arc::new(order_view("d-1", None, 1)),
        std::sync::Arc::new(order_view("s-1", None, -1)),
    ];
    for address in [btc(), sub_account("BTCUSDT")] {
        let ids: Vec<&str> =
            window_orders(&s, &address).map(|o| o.order_client_id.as_str()).collect();
        assert!(ids.is_empty(), "{address:?} took {ids:?} on a node that attributes nothing");
    }
    // …and the positive control: the same orders on a node that publishes `tier` are attributed,
    // each to the account it names (`None` is the default account's).
    s.portfolio.venues =
        vec![block("binance", None), block("binance#SUB", AccountLabel::parse("SUB").ok())];
    s.orders = vec![
        std::sync::Arc::new(order_view("d-1", None, 1)),
        std::sync::Arc::new(order_view("s-1", AccountLabel::parse("SUB").ok(), -1)),
    ];
    let ids = |address: &TradeWindowAddress| -> Vec<String> {
        window_orders(&s, address).map(|o| o.order_client_id.clone()).collect()
    };
    assert_eq!(ids(&btc()), ["d-1"]);
    assert_eq!(ids(&sub_account("BTCUSDT")), ["s-1"]);
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
        vike_execution::OrderView { venue: "bybit".into(), ..order_view("v", None, 1) },
        vike_execution::OrderView { symbol: "ETHUSDT".into(), ..order_view("y", None, 1) },
        vike_execution::OrderView { status: OrderStatus::Filled, ..order_view("f", None, 1) },
    ]
    .into_iter()
    .map(std::sync::Arc::new)
    .collect::<Vec<_>>();
    let node = |blocks: Vec<vike_execution::VenueBlock>| {
        let mut s = binance_snapshot();
        s.portfolio.venues = blocks;
        s.portfolio.venues.push(vike_execution::VenueBlock {
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
    let other = || TradeWindowAddress {
        venue: "binance".into(),
        account: AccountLabel::parse("OTHER").ok(),
        symbol: "BTCUSDT".into(),
    };
    let cases: [(&str, &CoreSnapshot, TradeWindowAddress, &[&str]); 9] = [
        // the default account owns its own, done ones included; not SUB's, OTHER's, bybit's or
        // ETHUSDT's
        ("modern", &modern, btc(), &["d", "f"]),
        ("modern", &modern, sub_account("BTCUSDT"), &["s"]),
        // an account the server does not run owns nothing, even an order that names it
        ("modern", &modern, other(), &[]),
        ("one labelled", &one_labelled, btc(), &[]),
        // the one engine owns every order of its venue and symbol, whatever account each names
        ("one labelled", &one_labelled, sub_account("BTCUSDT"), &["d", "s", "x", "f"]),
        ("one labelled", &one_labelled, other(), &[]),
        // fail-closed: nothing can be attributed, so nothing is owned
        ("R9", &unattributed, btc(), &[]),
        ("R9", &unattributed, sub_account("BTCUSDT"), &[]),
        ("R9", &unattributed, other(), &[]),
    ];
    for (name, snapshot, address, want) in cases {
        let owns = owns_order(snapshot, &address);
        let owned: Vec<&str> = snapshot
            .orders
            .iter()
            .filter(|o| owns(o))
            .map(|o| o.order_client_id.as_str())
            .collect();
        assert_eq!(owned, want, "{name} {:?}", address.account);
        // …and window_orders is exactly that, minus the done order
        let live: Vec<&str> =
            window_orders(snapshot, &address).map(|o| o.order_client_id.as_str()).collect();
        let want_live: Vec<&str> = want.iter().copied().filter(|c| *c != "f").collect();
        assert_eq!(live, want_live, "{name} {:?}: the live subset", address.account);
    }
}

/// On an older node whose ONLY engine on a venue is labelled, every order of that venue is that
/// engine's even though each order names no account: the SUB window pulls them (Minor 1 of the
/// review; it used to match none of its own orders and cancel nothing), and a default-account
/// window — an account the server does not run — still owns none.
#[test]
fn a_one_engine_venue_attributes_every_order_to_its_engine() {
    let mut s = binance_snapshot();
    s.portfolio.venues = vec![older("binance#SUB", AccountLabel::parse("SUB").ok())];
    s.orders = vec![
        std::sync::Arc::new(order_view("s-1", None, 1)),
        std::sync::Arc::new(order_view("s-2", None, -1)),
    ];
    assert!(orders_name_their_account(&s, "binance"));
    let sub_address = sub_account("BTCUSDT");
    let ids: Vec<&str> =
        window_orders(&s, &sub_address).map(|o| o.order_client_id.as_str()).collect();
    assert_eq!(ids, ["s-1", "s-2"]);
    let mut i = OrderEntryDispatchInputs::default();
    i.trade_actions.push((sub_account("BTCUSDT"), TradeWindowAction::CancelAll));
    i.trade_actions.push((btc(), TradeWindowAction::CancelAll));
    let plan = plan_dispatch(i, &OrderLimits::default(), &s, 1, 0, None);
    assert_eq!(cancelled(&plan), ["s-1", "s-2"], "SUB's window pulls SUB's; the default's none");

    // a node that publishes `tier` names each order's account, so the order's own word is used
    s.portfolio.venues = vec![block("binance#SUB", AccountLabel::parse("SUB").ok())];
    s.orders = vec![std::sync::Arc::new(order_view("s-1", AccountLabel::parse("SUB").ok(), 1))];
    assert_eq!(window_orders(&s, &sub_address).count(), 1);
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
    let mut s = binance_snapshot();
    let with = |order_client_id: &str, side: i32, status: OrderStatus| vike_execution::OrderView {
        status,
        ..order_view(order_client_id, None, side)
    };
    s.orders = vec![
        std::sync::Arc::new(with("filled", 1, OrderStatus::Filled)),
        std::sync::Arc::new(with("canceled", 1, OrderStatus::Canceled)),
        std::sync::Arc::new(with("rejected", -1, OrderStatus::Rejected)),
        std::sync::Arc::new(with("denied", 1, OrderStatus::Denied)),
        std::sync::Arc::new(with("expired", -1, OrderStatus::Expired)),
        std::sync::Arc::new(with("liquidated", 1, OrderStatus::Liquidated)),
        std::sync::Arc::new(with("accepted", 1, OrderStatus::Accepted)),
        std::sync::Arc::new(with("partial", -1, OrderStatus::PartiallyFilled)),
        // Positive controls beyond the two resting states (A1's review minor): an order the venue
        // has not acknowledged yet, and one whose cancel is in flight, are still live — a filter
        // that kept only Accepted/PartiallyFilled would drop both, and Cancel all would leave them.
        std::sync::Arc::new(with("submitted", 1, OrderStatus::Submitted)),
        std::sync::Arc::new(with("pending-cancel", -1, OrderStatus::PendingCancel)),
    ];
    let address = btc();
    let live: Vec<&str> = window_orders(&s, &address).map(|o| o.order_client_id.as_str()).collect();
    assert_eq!(live, ["accepted", "partial", "submitted", "pending-cancel"], "the markers' rule");
    for (act, want) in [
        (TradeWindowAction::CancelAll, vec!["accepted", "partial", "submitted", "pending-cancel"]),
        (TradeWindowAction::CancelSide(1), vec!["accepted", "submitted"]),
        (TradeWindowAction::CancelSide(-1), vec!["partial", "pending-cancel"]),
    ] {
        let mut i = OrderEntryDispatchInputs::default();
        i.trade_actions.push((btc(), act.clone()));
        let plan = plan_dispatch(i, &OrderLimits::default(), &s, 1, 0, None);
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
    let mut s = binance_snapshot();
    s.portfolio.venues.push(vike_execution::VenueBlock {
        venue: "bybit".into(),
        route_key: "bybit".into(),
        symbol: "ETHUSDT".into(),
        tier: Some(vike_model::AccountTier::Live),
        positions: vec![position_view("bybit", "ETHUSDT", 1.0)],
        ..Default::default()
    });
    assert_eq!((s.venue.as_str(), s.symbol.as_str()), ("binance", "BTCUSDT"));
    let eth = TradeWindowAddress { venue: "bybit".into(), account: None, symbol: "ETHUSDT".into() };
    let exits = Some(TradeWindowTicketExits { take_profit: 160.0, stop_loss: 140.0 });
    let actions = [
        place(TradeWindowOrigin::Ticket, None),
        place(TradeWindowOrigin::Ladder, None),
        place(TradeWindowOrigin::Ticket, exits),
        TradeWindowAction::ClosePosition,
        TradeWindowAction::Reverse,
    ];
    for act in actions {
        let mut i = OrderEntryDispatchInputs::default();
        i.trade_actions.push((eth.clone(), act.clone()));
        let plan = plan_dispatch(i, &OrderLimits::default(), &s, 1, 0, None);
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
