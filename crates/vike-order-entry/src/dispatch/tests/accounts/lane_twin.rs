//! The perpetual lane twin (decision 0184): a perpetual at the default account is addressed to the
//! twin only while it is up, a real account under the twin's label is not, and the twin's book is
//! where close, reverse and cancel-all act.

use super::*;

/// The shipped daemon's shape plus the second engine a binance demo account gets when it states a margin
/// leverage: `binance` on spot `BTCUSDT` and `binance#PERP` on `BTCUSDT.P`, both demo.
fn with_perp_lane() -> CoreSnapshot {
    let mut s = binance_snapshot();
    let perp = AccountLabel::parse("PERP").ok();
    s.portfolio.venues[0].tier = Some(vike_model::AccountTier::Demo);
    s.portfolio.venues.push(vike_execution::VenueBlock {
        symbol: "BTCUSDT.P".into(),
        tier: Some(vike_model::AccountTier::Demo),
        ..block("binance#PERP", perp)
    });
    s
}

fn perp_lane() -> TradeWindowAddress {
    TradeWindowAddress {
        venue: "binance".into(),
        account: AccountLabel::parse("PERP").ok(),
        symbol: "BTCUSDT.P".into(),
    }
}

/// **F2 of the twin review: the window never places a perpetual at the default account while the account
/// that runs the perpetual lane is another one.** The node's door would put such an order on that account's
/// engine on its own (`lane_addressed`), and the window reads the position, the markers and the cancels
/// off the address's own route key, so the order would fill in a book the window shows nothing of and the
/// trader could buy again, blind. A Place, a Close and a Reverse at the default address are refused with
/// the reason, never sent; at the lane account's OWN address everything is keyed by its route key and
/// goes out naming it; and nothing changes for a spot symbol, a venue without a lane account, an engine
/// already on the perpetual lane, or another venue.
#[test]
fn a_perpetual_at_the_default_account_is_refused_while_another_account_runs_the_perp_lane() {
    let s = with_perp_lane();
    assert_eq!(perp_lane_account(&s, &btc_perp()).as_deref(), Some("PERP"));
    assert!(!tradable(&s, &btc_perp()));
    for act in [
        place(TradeWindowOrigin::Ticket, None),
        TradeWindowAction::ClosePosition,
        TradeWindowAction::Reverse,
    ] {
        let mut i = OrderEntryDispatchInputs::default();
        i.trade_actions.push((btc_perp(), act.clone()));
        let plan = plan_dispatch(i, &OrderLimits::default(), &s, 1, 0, None);
        assert!(plan.commands.is_empty(), "{act:?} planned {:?}", plan.commands);
        assert_eq!(plan.rejects.len(), 1, "{act:?} is refused, never dropped");
        assert_eq!(
            plan.rejects[0].reason,
            OrderEntryDispatchRejectReason::PerpLaneIsAnotherAccount
        );
    }
    assert!(
        OrderEntryDispatchRejectReason::PerpLaneIsAnotherAccount
            .to_string()
            .contains("account menu"),
        "the refusal says where to go"
    );

    // The lane account's own address: tradable, and every lookup is its own.
    let mut s = with_perp_lane();
    s.portfolio.venues[1].positions = vec![position_view("binance", "BTCUSDT.P", 0.5)];
    let mut resting = order_view("p-1", AccountLabel::parse("PERP").ok(), 1);
    resting.symbol = "BTCUSDT.P".into();
    s.orders = vec![std::sync::Arc::new(resting)];
    assert!(tradable(&s, &perp_lane()));
    let one = |act: TradeWindowAction| {
        let mut i = OrderEntryDispatchInputs::default();
        i.trade_actions.push((perp_lane(), act));
        plan_dispatch(i, &OrderLimits::default(), &s, 1, 0, None)
    };
    let placed = one(place(TradeWindowOrigin::Ticket, None));
    match placed.commands.as_slice() {
        [Command::Order(OrderIntent::Submit(req))] => {
            assert_eq!(req.symbol, "BTCUSDT.P");
            assert_eq!(req.account.as_ref().map(ToString::to_string).as_deref(), Some("PERP"));
        }
        other => panic!("one submit at the lane account, got {other:?} / {:?}", placed.rejects),
    }
    let closed = one(TradeWindowAction::ClosePosition);
    match closed.commands.as_slice() {
        [Command::Order(OrderIntent::Submit(req))] => {
            assert_eq!((req.side, req.qty, req.reduce_only), (-1, 0.5, true));
            assert_eq!(req.account.as_ref().map(ToString::to_string).as_deref(), Some("PERP"));
        }
        other => panic!("one reduce-only exit of the lane's position, got {other:?}"),
    }
    assert_eq!(cancelled(&one(TradeWindowAction::CancelAll)), ["p-1"], "its own resting order");

    // Nothing else changes.
    let spot = one_with(&with_perp_lane(), btc(), place(TradeWindowOrigin::Ticket, None));
    match spot.commands.as_slice() {
        [Command::Order(OrderIntent::Submit(req))] => {
            assert_eq!(req.account, Some(AccountLabel::Default), "a spot order is as it was");
        }
        other => panic!("a spot symbol at the default account still goes, got {other:?}"),
    }
    assert_eq!(perp_lane_account(&binance_snapshot(), &btc_perp()), None, "no lane account runs");
    assert_eq!(
        perp_lane_account(&perp_snapshot(), &btc_perp()),
        None,
        "the engine IS on the perp lane"
    );
    assert_eq!(perp_lane_account(&with_perp_lane(), &perp_lane()), None, "a named account");
    assert_eq!(perp_lane_account(&with_perp_lane(), &btc()), None, "a spot symbol");
    let mut bybit = with_perp_lane();
    for b in &mut bybit.portfolio.venues {
        b.venue = "bybit".into();
        b.route_key = b.route_key.replace("binance", "bybit");
    }
    let at_bybit = TradeWindowAddress { venue: "bybit".into(), ..btc_perp() };
    assert_eq!(
        perp_lane_account(&bybit, &at_bybit),
        None,
        "a single-lane venue has no lane account"
    );
}

/// **A Trade window addresses the lane twin for a perpetual (`lane_twin_account`).** The decision is the
/// door's own (`lane_addressed`), asked of the published blocks: a DEFAULT account, a `.P` symbol, the twin's
/// route key AND mount symbol published. Everything else stays where the trader put it, and an operator's own
/// account on the perpetual lane (not the twin, which the door does not reroute to) is still refused at the
/// default address rather than addressed.
#[test]
fn a_perpetual_at_the_default_account_is_addressed_to_the_lane_twin_only_while_it_is_up() {
    let s = with_perp_lane();
    for account in [None, Some("DEFAULT")] {
        for symbol in ["BTCUSDT.P", "ethusdt.p", " ETHUSDT.P "] {
            assert_eq!(
                lane_twin_account(&s, "binance", account, symbol, None),
                Some("PERP"),
                "{account:?} {symbol}"
            );
        }
    }
    let stays = |s: &CoreSnapshot, venue, account, symbol| {
        assert_eq!(
            lane_twin_account(s, venue, account, symbol, None),
            None,
            "{venue} {account:?} {symbol}"
        );
    };
    stays(&s, "binance", None, "BTCUSDT");
    stays(&s, "binance", Some("PERP"), "BTCUSDT.P");
    stays(&s, "binance", Some("SUB"), "BTCUSDT.P");
    stays(&s, "binance", Some("not a label!"), "BTCUSDT.P");
    stays(&s, "binance", None, "");
    stays(&s, "bybit", None, "BTCUSDT.P");
    stays(&binance_snapshot(), "binance", None, "BTCUSDT.P");
    stays(&perp_snapshot(), "binance", None, "BTCUSDT.P");

    // A block that carries the twin's label but is mounted off the twin's lane (a spot symbol) is not the twin
    // (the table names the lane, `.P`, and the symbol is the parent row's own: owner, 2026-11-13).
    let mut elsewhere = with_perp_lane();
    elsewhere.portfolio.venues[1].symbol = "ETHUSDT".into();
    stays(&elsewhere, "binance", None, "BTCUSDT.P");

    // An operator's own account on the perpetual lane: refused at the default address, never addressed.
    let mut alt = with_perp_lane();
    alt.portfolio.venues[1].route_key = "binance#ALT".into();
    alt.portfolio.venues[1].account = AccountLabel::parse("ALT").ok();
    stays(&alt, "binance", None, "BTCUSDT.P");
    assert_eq!(perp_lane_account(&alt, &btc_perp()).as_deref(), Some("ALT"));
    assert!(!tradable(&alt, &btc_perp()));
}

/// **A real account that spells the twin's label is not the twin** (v0.1.80 review b, finding 2). The
/// label and the mount symbol are all a block shows, and an older real `PERP` account (the "second account
/// row" workaround, made before the label was reserved) has both. The node's directory lists a real account
/// under the label, which a derived twin is in no row of: at the tier the table opens the twin at (demo AND,
/// since the owner opened it on 2026-10-19, live) that marker is what tells the two apart, and a `paper`
/// engine is never the twin. The refusal stays: the default address is turned back, naming the account that
/// runs the perpetual lane.
#[test]
fn a_real_account_under_the_twins_label_is_not_addressed_for_the_default_account() {
    use vike_tradehub_client::wire::{AccountDirectoryAccountWire, AccountDirectoryWire};

    let live = |tier| {
        let mut s = with_perp_lane();
        s.portfolio.venues[1].tier = Some(tier);
        s
    };
    assert_eq!(
        lane_twin_account(&live(vike_model::AccountTier::Demo), "binance", None, "BTCUSDT.P", None),
        Some("PERP"),
        "the derived twin runs at the tier the table opens"
    );
    // The owner opened the twin at `live` too (2026-10-19), and at `live` a block of the twin's shape is not
    // enough (review r2, finding 7): a real `live` account under the label has the twin's label, symbol and
    // tier to the letter. A directory that did NOT answer (`None`) is no evidence, so it is NOT the twin;
    // the directory must answer and list no account under the label (below).
    assert_eq!(
        lane_twin_account(&live(vike_model::AccountTier::Live), "binance", None, "BTCUSDT.P", None),
        None,
        "a live engine is never judged to be the derived twin by the blocks alone"
    );
    // A `paper` one never is.
    let paper = live(vike_model::AccountTier::Paper);
    assert_eq!(lane_twin_account(&paper, "binance", None, "BTCUSDT.P", None), None, "paper");
    assert_eq!(perp_lane_account(&paper, &btc_perp()).as_deref(), Some("PERP"), "paper");
    assert!(!tradable(&paper, &btc_perp()), "paper: the default address is turned back");
    // An engine that does not say its tier (an older node) is not judged to be the twin.
    let mut unsaid = with_perp_lane();
    unsaid.portfolio.venues[1].tier = None;
    assert_eq!(lane_twin_account(&unsaid, "binance", None, "BTCUSDT.P", None), None);

    let row = |label: Option<&str>| AccountDirectoryAccountWire {
        id: 7,
        venue: "binance".into(),
        label: label.map(str::to_string),
        tier: "demo".into(),
        venue_account_id: None,
    };
    let directory = |accounts| AccountDirectoryWire { venues: Vec::new(), accounts };
    let s = with_perp_lane();
    assert_eq!(
        lane_twin_account(&s, "binance", None, "BTCUSDT.P", Some(&directory(vec![row(None)]))),
        Some("PERP"),
        "the one stored account is the default one: the twin is derived from it"
    );
    assert_eq!(
        lane_twin_account(
            &s,
            "binance",
            None,
            "BTCUSDT.P",
            Some(&directory(vec![row(None), row(Some("PERP"))]))
        ),
        None,
        "a stored row under the label is a real account, not the derived twin"
    );
    // …at the `live` tier too, where the directory is the ONLY thing that tells them apart: a directory
    // that lists a real account under the label is not the twin; one that lists only the default row is.
    let s = live(vike_model::AccountTier::Live);
    let real_live = directory(vec![row(None), row(Some("PERP"))]);
    assert_eq!(lane_twin_account(&s, "binance", None, "BTCUSDT.P", Some(&real_live)), None);
    assert_eq!(perp_lane_account(&s, &btc_perp()).as_deref(), Some("PERP"));
    assert!(!tradable(&s, &btc_perp()), "the default address is turned back");
    assert_eq!(
        lane_twin_account(&s, "binance", None, "BTCUSDT.P", Some(&directory(vec![row(None)]))),
        Some("PERP"),
        "positive evidence: the node answered and holds no real PERP row"
    );
    // A legacy real LIVE `PERP` account (the "second account row" workaround, made before the label was
    // reserved): a `live` block under the twin's label on the perpetual symbol, a stored row under the label.
    let legacy = AccountDirectoryAccountWire { tier: "live".into(), ..row(Some("PERP")) };
    assert_eq!(
        lane_twin_account(&s, "binance", None, "BTCUSDT.P", Some(&directory(vec![legacy]))),
        None,
        "the default account's perpetual order is not sent to another real account's keys"
    );
}

/// **A MAINNET lane twin is addressed and traded by the window exactly as a demo one** (the owner opened
/// binance's `live` tier for it, 2026-10-19): both blocks `live`, a perpetual at the default account is
/// addressed to the twin, a spot symbol stays, and an order at the twin's own address names `PERP` on the wire.
#[test]
fn a_live_lane_twin_is_addressed_and_traded_like_a_demo_one() {
    use vike_tradehub_client::wire::{AccountDirectoryAccountWire, AccountDirectoryWire};

    let mut s = with_perp_lane();
    for block in &mut s.portfolio.venues {
        block.tier = Some(vike_model::AccountTier::Live);
    }
    // At `live` the node's directory must have answered, with the default row and no PERP row (finding 7).
    let directory = AccountDirectoryWire {
        venues: Vec::new(),
        accounts: vec![AccountDirectoryAccountWire {
            id: 3,
            venue: "binance".into(),
            label: None,
            tier: "live".into(),
            venue_account_id: None,
        }],
    };
    let dir = Some(&directory);
    for account in [None, Some("DEFAULT")] {
        assert_eq!(lane_twin_account(&s, "binance", account, "BTCUSDT.P", dir), Some("PERP"));
        assert_eq!(lane_twin_account(&s, "binance", account, " ethusdt.p ", dir), Some("PERP"));
        assert_eq!(lane_twin_account(&s, "binance", account, "BTCUSDT", dir), None);
        assert_eq!(
            lane_twin_account(&s, "binance", account, "BTCUSDT.P", None),
            None,
            "no answer from the node's directory is no evidence at live"
        );
    }
    assert_eq!(perp_lane_account(&s, &btc_perp()).as_deref(), Some("PERP"));
    assert!(!tradable(&s, &btc_perp()), "the default address still refuses an unrouted perpetual");
    assert!(tradable(&s, &perp_lane()));
    let mut i = OrderEntryDispatchInputs::default();
    i.trade_actions.push((perp_lane(), place(TradeWindowOrigin::Ticket, None)));
    let plan = plan_dispatch(i, &OrderLimits::default(), &s, 1, 0, None);
    match plan.commands.as_slice() {
        [Command::Order(OrderIntent::Submit(req))] => {
            assert_eq!(req.symbol, "BTCUSDT.P");
            assert_eq!(req.account.as_ref().map(ToString::to_string).as_deref(), Some("PERP"));
        }
        other => panic!("one submit at the live twin, got {other:?} / {:?}", plan.rejects),
    }
}

/// **Close, Reverse and Cancel all on the twin's position, at the address the window routes to.** The lookups
/// are by route key `binance#PERP`: the default engine's own book (a position and a resting order on the same
/// symbol) is never read, and every order names the twin on the wire.
#[test]
fn close_reverse_and_cancel_all_act_on_the_twins_book_at_the_routed_address() {
    let mut s = with_perp_lane();
    s.portfolio.venues[0].positions = vec![position_view("binance", "BTCUSDT.P", 9.0)];
    s.portfolio.venues[1].positions = vec![position_view("binance", "BTCUSDT.P", 0.5)];
    let mut twins = order_view("p-1", AccountLabel::parse("PERP").ok(), 1);
    twins.symbol = "BTCUSDT.P".into();
    let mut defaults = order_view("d-1", None, 1);
    defaults.symbol = "BTCUSDT.P".into();
    s.orders = vec![std::sync::Arc::new(defaults), std::sync::Arc::new(twins)];

    let routed = lane_twin_account(&s, "binance", None, "BTCUSDT.P", None);
    let address = TradeWindowAddress::parse("binance", routed, "BTCUSDT.P").expect("an address");
    assert_eq!(address.route_key(), "binance#PERP");
    let exit = |act: TradeWindowAction| match one_with(&s, address.clone(), act).commands.as_slice()
    {
        [Command::Order(OrderIntent::Submit(req))] => (**req).clone(),
        other => panic!("one submit, got {other:?}"),
    };
    let close = exit(TradeWindowAction::ClosePosition);
    assert_eq!(
        (close.side, close.qty, close.reduce_only),
        (-1, 0.5, true),
        "the twin's 0.5, not 9"
    );
    let reverse = exit(TradeWindowAction::Reverse);
    assert_eq!((reverse.side, reverse.qty, reverse.reduce_only), (-1, 1.0, false));
    for req in [&close, &reverse] {
        assert_eq!(req.account.as_ref().map(ToString::to_string).as_deref(), Some("PERP"));
        assert_eq!((req.venue.as_str(), req.symbol.as_str()), ("binance", "BTCUSDT.P"));
    }
    let placed = exit(place(TradeWindowOrigin::Ticket, None));
    assert_eq!(placed.account.as_ref().map(ToString::to_string).as_deref(), Some("PERP"));
    assert_eq!(cancelled(&one_with(&s, address.clone(), TradeWindowAction::CancelAll)), ["p-1"]);
    assert_eq!(cancelled(&one_with(&s, address, TradeWindowAction::CancelSide(1))), ["p-1"]);

    // The ownership rule the markers and the cancels share accepts the twin's order at this address only.
    let owns_at = |address: &TradeWindowAddress| {
        let owns = owns_order(&s, address);
        s.orders.iter().filter(|o| owns(o)).map(|o| o.order_client_id.clone()).collect::<Vec<_>>()
    };
    assert_eq!(owns_at(&perp_lane()), ["p-1"]);
    assert_eq!(owns_at(&btc_perp()), ["d-1"], "the default address owns the default engine's");
}
