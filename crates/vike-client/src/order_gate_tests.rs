//! The one order gate, verb by verb, over frames built in code: the decision both doors take, pinned once
//! (`vike-cli` and `vike-mcp` may not link each other, decision 0252; their parity is
//! `crates/vike-cli/tests/shipped_cli/cli_mcp_parity.rs`'s).

use serde_json::json;
use vike_model::TradingState;
use vike_tradehub_client::wire::{
    ArmingNotRunningViewWire, CommandWire, CoreSnapshotWire, OrderBracketWire, OrderRequestWire,
    OrderViewWire, PositionViewWire, VenueBlockWire,
};

use super::{NodeLink, VenueCheck, judge, reads_the_frame};
use crate::refusals::{OrderGateRefusal, OrderGateRefusalId};
use crate::verbs::{
    self, ComboLegArg, Guardrail, GuardrailCapabilities, OrderListHead, oco_list, oto_list,
};

/// This box's cap in every row: 1, so any priced order of size is over it.
const CAP: GuardrailCapabilities = GuardrailCapabilities { max_qty: None, max_notional: Some(1.0) };

/// A node that routes by venue and has the one-account door: the routing rule's facts never refuse.
const NEW_NODE: NodeLink = NodeLink { routes_by_venue: Some(true), one_account_door: Some(true) };

fn block(route_key: &str, symbols: &[&str]) -> VenueBlockWire {
    let (venue, account) =
        route_key.split_once('#').map_or((route_key, None), |(v, a)| (v, Some(a)));
    let mut block: VenueBlockWire = serde_json::from_value(json!({
        "venue": venue, "balance": 0.0, "realized_pnl": 0.0, "fees_paid": 0.0, "funding_paid": 0.0,
        "equity": 0.0, "unrealized": 0.0, "missing_prices": 0, "margin_used": 0.0, "free_buying_power": 0.0,
        "trading_state": "Active", "positions": [],
    }))
    .expect("a block");
    block.account = account.map(str::to_string);
    block.route_key = route_key.to_string();
    block.symbols = symbols.iter().map(|s| (*s).to_string()).collect();
    block
}

/// `block` holding a net `size` of `symbol` (long positive).
fn holding(mut block: VenueBlockWire, symbol: &str, size: f64) -> VenueBlockWire {
    block.positions.push(PositionViewWire {
        venue: block.venue.clone(),
        symbol: symbol.into(),
        position_side: if size < 0.0 { "SHORT" } else { "LONG" }.into(),
        size: size.abs(),
        average_price: 100.0,
        unrealized: 0.0,
        leverage: 1.0,
        liquidation_price: 0.0,
        mark_source: None,
    });
    block
}

fn frame_of(blocks: Vec<VenueBlockWire>) -> CoreSnapshotWire {
    let mut frame = CoreSnapshotWire::empty();
    frame.sequence = 1;
    frame.venues = blocks;
    frame
}

/// One bybit account, trading BTCUSDT and ETHUSDT, long `long` BTCUSDT.
fn bybit(long: f64) -> CoreSnapshotWire {
    let book = block("bybit", &["BTCUSDT", "ETHUSDT"]);
    frame_of(vec![if long == 0.0 { book } else { holding(book, "BTCUSDT", long) }])
}

fn submit(
    venue: &str,
    account: Option<&str>,
    side: i32,
    qty: f64,
    price: Option<f64>,
) -> CommandWire {
    CommandWire::Submit(OrderRequestWire {
        order_client_id: "C1".into(),
        venue: venue.into(),
        symbol: "BTCUSDT".into(),
        side,
        qty,
        order_type: if price.is_some() { "limit" } else { "market" }.into(),
        price,
        trigger_price: None,
        reduce_only: false,
        account: account.map(str::to_string),
        time_in_force: None,
        gtd_expiry: None,
        display_quantity: None,
        release: None,
        trail: None,
        limit_offset: None,
        peg: None,
        attributes: None,
        trail_percent: None,
        adjust: None,
    })
}

fn head(venue: &str, side: i32, qty: f64) -> OrderListHead {
    OrderListHead { venue: venue.into(), account: None, symbol: "BTCUSDT".into(), side, qty }
}

fn resting(id: &str, side: i32, qty: f64, order_type: &str, price: Option<f64>) -> OrderViewWire {
    serde_json::from_value(json!({
        "order_client_id": id, "venue": "bybit", "symbol": "BTCUSDT", "side": side, "qty": qty,
        "order_type": order_type, "price": price, "trigger_price": null, "status": "ACCEPTED",
        "venue_order_id": null, "filled_qty": 0.0, "average_fill_price": 0.0,
    }))
    .expect("an order")
}

fn modify(id: &str, new_qty: Option<f64>, new_price: Option<f64>) -> CommandWire {
    CommandWire::Modify { order_client_id: id.into(), new_qty, new_price, new_trigger_price: None }
}

/// The id `judge` refused with, `None` for a pass.
fn refused(
    cmd: &CommandWire,
    frame: Result<&CoreSnapshotWire, &str>,
) -> Option<OrderGateRefusalId> {
    judge(cmd, CAP, frame, NEW_NODE).err().map(|refusal| refusal.id())
}

fn over(cmd: &CommandWire, frame: &CoreSnapshotWire) -> bool {
    refused(cmd, Ok(frame)) == Some(OrderGateRefusalId::OverMaxNotional)
}

#[test]
fn a_submit_over_the_ceiling_is_refused_and_a_covered_close_is_not() {
    let frame = bybit(5.0);
    let buy = submit("bybit", None, 1, 1.0, Some(100.0));
    let refusal = judge(&buy, CAP, Ok(&frame), NEW_NODE).expect_err("100 is over 1");
    assert_eq!(refusal, OrderGateRefusal::OverMaxNotional { notional: 100.0, max: 1.0 });
    assert_eq!(
        refusal.rendered(),
        "error [order.edge.over_max_notional]: not sent: notional 100 is over this box's \
         policy.max_notional_per_order 1; nothing went to the node"
    );
    // A covered close is never measured, whatever its size: no ceiling blocks an exit.
    let close = submit("bybit", None, -1, 5.0, Some(100.0));
    assert_eq!(
        judge(&close, CAP, Ok(&frame), NEW_NODE),
        Ok((Guardrail::NoSize, VenueCheck::Mounted))
    );
    // A sell past the long flips it: an open, sized whole.
    assert!(over(&submit("bybit", None, -1, 6.0, Some(100.0)), &frame));
    let (guard, check) =
        judge(&submit("bybit", None, 1, 0.005, Some(100.0)), CAP, Ok(&frame), NEW_NODE)
            .expect("0.5");
    assert_eq!(
        (guard.to_json()["within_limits"].clone(), check),
        (json!(true), VenueCheck::Mounted)
    );
}

#[test]
fn a_bracket_is_refused_by_its_largest_priced_leg() {
    let frame = bybit(0.0);
    let bracket = |qty, entry, stop_loss, take_profit| {
        CommandWire::Bracket(OrderBracketWire {
            order_client_id: None,
            venue: "bybit".into(),
            symbol: "BTCUSDT".into(),
            side: 1,
            qty,
            entry_price: entry,
            stop_loss,
            take_profit,
            account: None,
        })
    };
    let refusal = judge(&bracket(1.0, Some(0.5), 0.4, 110.0), CAP, Ok(&frame), NEW_NODE);
    assert_eq!(
        refusal.err(),
        Some(OrderGateRefusal::OverMaxNotional { notional: 110.0, max: 1.0 })
    );
    assert!(refused(&bracket(1.0, Some(0.5), 0.4, 0.9), Ok(&frame)).is_none(), "every leg within");
    // A market entry: an exit over the cap is refused, exits within it say nothing of the entry.
    assert!(over(&bracket(1.0, None, 0.4, 110.0), &frame));
    assert!(refused(&bracket(1.0, None, 0.4, 0.9), Ok(&frame)).is_none());
}

#[test]
fn an_order_list_is_refused_by_its_largest_leg_and_an_exit_pair_is_not() {
    let long = bybit(5.0);
    let opening =
        oco_list(head("bybit", 1, 1.0), 100.0, Some(120.0), None, None, false).expect("an oco");
    let refusal = judge(&CommandWire::OrderList(opening), CAP, Ok(&long), NEW_NODE);
    assert_eq!(
        refusal.err(),
        Some(OrderGateRefusal::OverMaxNotional { notional: 120.0, max: 1.0 })
    );
    // D4: a take-profit and a stop, both reduce-only, each covered by the long: a CLOSE, never measured.
    let exit =
        oco_list(head("bybit", -1, 5.0), 110.0, Some(90.0), None, None, true).expect("an exit oco");
    assert_eq!(
        judge(&CommandWire::OrderList(exit.clone()), CAP, Ok(&long), NEW_NODE),
        Ok((Guardrail::NoSize, VenueCheck::Mounted))
    );
    // The same pair on a flat book, or past the position, opens: measured.
    assert!(over(&CommandWire::OrderList(exit), &bybit(0.0)));
    let past =
        oco_list(head("bybit", -1, 6.0), 110.0, Some(90.0), None, None, true).expect("an oco");
    assert!(over(&CommandWire::OrderList(past), &long));
    // A list whose legs are not all reduce-only is no exit, even under the position.
    let plain =
        oco_list(head("bybit", -1, 5.0), 110.0, Some(90.0), None, None, false).expect("an oco");
    assert!(over(&CommandWire::OrderList(plain), &long));
}

/// **An exit list with a TRAILING reduce-only leg covered by the position is still a close, never sized** (plan P4
/// #15): the trailing leg goes through the one gate like every leg, and the door's close rule (every leg reduce-only and
/// covered) reads it as it reads a stop. On a flat book the same list opens and its priced leg is measured; with no
/// priced leg at all (a trailing stop beside nothing priced) nothing is invented to measure.
#[test]
fn an_exit_list_with_a_covered_trailing_leg_is_a_close_and_never_sized() {
    let long = bybit(5.0);
    let oco = |qty, reduce_only| {
        let list = oco_list(head("bybit", -1, qty), 110.0, None, None, Some(5.0), reduce_only);
        CommandWire::OrderList(list.expect("a trailing exit oco"))
    };
    assert_eq!(
        judge(&oco(5.0, true), CAP, Ok(&long), NEW_NODE),
        Ok((Guardrail::NoSize, VenueCheck::Mounted)),
        "a covered trailing exit is a close"
    );
    assert!(over(&oco(5.0, true), &bybit(0.0)), "on a flat book it opens: its target is measured");
    assert!(over(&oco(6.0, true), &long), "past the position it opens");
    assert!(over(&oco(5.0, false), &long), "a leg that is not reduce-only is no exit");
    // An OTO's trailing exit: the entry opens and is measured at its limit; the trailing leg names no price.
    let oto = oto_list(head("bybit", 1, 1.0), Some(100.0), None, None, Some(5.0)).expect("an oto");
    assert!(over(&CommandWire::OrderList(oto), &long), "the entry is measured at its limit");
}

#[test]
fn a_combo_is_refused_by_its_largest_leg_limit_and_a_net_combo_is_not_checked() {
    let frame = bybit(0.0);
    let leg = |symbol: &str, ratio, limit| ComboLegArg { symbol: symbol.into(), ratio, limit };
    let combo = |legs, net| {
        let combo = verbs::combo_of("bybit".into(), None, 1, 2.0, legs, net, Default::default())
            .expect("a combo");
        CommandWire::Combo(combo)
    };
    // D2: each leg `|ratio| x qty` at its own limit, the largest leg (2 x 100 = 200), not the sum.
    let priced = combo(vec![leg("BTCUSDT", 1, Some(100.0)), leg("ETHUSDT", -1, Some(50.0))], None);
    let refusal = judge(&priced, CAP, Ok(&frame), NEW_NODE);
    assert_eq!(
        refusal.err(),
        Some(OrderGateRefusal::OverMaxNotional { notional: 200.0, max: 1.0 })
    );
    let small = combo(vec![leg("BTCUSDT", 1, Some(0.2)), leg("ETHUSDT", -1, Some(0.1))], None);
    assert!(refused(&small, Ok(&frame)).is_none(), "0.4 is within 1");
    // A net combo states no size per leg: not checked, sent, and the node judges it at its own prices.
    let net = combo(vec![leg("BTCUSDT", 1, None), leg("ETHUSDT", -1, None)], Some(5.0));
    let (guard, _) = judge(&net, CAP, Ok(&frame), NEW_NODE).expect("not checked is not refused");
    assert_eq!(guard.to_json()["within_limits"], json!(null), "{}", guard.to_json());
}

#[test]
fn a_modify_is_measured_from_the_resting_order_in_all_three_shapes() {
    let mut frame = bybit(5.0);
    frame.orders = vec![
        resting("BUY9", 1, 1.0, "limit", Some(0.5)),
        resting("SELL8", -1, 5.0, "limit", Some(0.5)),
    ];
    // (a) a new qty and a new price: at those.
    assert!(over(&modify("BUY9", Some(1.0), Some(100.0)), &frame));
    // (b) a new qty, no price: at the resting price, 10 x 0.5.
    let refusal = judge(&modify("BUY9", Some(10.0), None), CAP, Ok(&frame), NEW_NODE);
    assert_eq!(refusal.err(), Some(OrderGateRefusal::OverMaxNotional { notional: 5.0, max: 1.0 }));
    // (c) a new price only: at the resting qty, 1 x 100. Nobody measured this before the core.
    let refusal = judge(&modify("BUY9", None, Some(100.0)), CAP, Ok(&frame), NEW_NODE);
    assert_eq!(
        refusal.err(),
        Some(OrderGateRefusal::OverMaxNotional { notional: 100.0, max: 1.0 })
    );
    assert!(refused(&modify("BUY9", None, Some(0.6)), Ok(&frame)).is_none(), "0.6 is within 1");
    // A resting close stays a close at any price, and past the position it opens.
    assert_eq!(
        judge(&modify("SELL8", None, Some(1_000.0)), CAP, Ok(&frame), NEW_NODE),
        Ok((Guardrail::NoSize, VenueCheck::NoVenue))
    );
    assert!(over(&modify("SELL8", Some(6.0), None), &frame));
    assert!(
        reads_the_frame(&modify("BUY9", None, Some(1.0))),
        "a price-only modify reads the frame"
    );
}

#[test]
fn no_exit_or_mode_verb_is_ever_refused_by_the_ceiling() {
    let mut frame = bybit(1_000.0);
    frame.orders = vec![resting("SELL8", -1, 1_000.0, "limit", Some(50_000.0))];
    let bybit = || "bybit".to_string();
    let exits = [
        CommandWire::Cancel("SELL8".into()),
        CommandWire::Cancel("NOSUCH".into()),
        CommandWire::MassCancel { venue: Some(bybit()), symbol: None, account: None },
        CommandWire::MassCancel { venue: None, symbol: None, account: None },
        CommandWire::Flatten { venue: bybit(), symbol: "BTCUSDT".into(), account: None },
        CommandWire::MarketExit { venue: Some(bybit()), account: None },
        CommandWire::MarketExit { venue: None, account: None },
        CommandWire::SetTradingState(TradingState::Halted),
        CommandWire::SetAccountTradingState {
            venue: bybit(),
            account: None,
            state: TradingState::Reducing,
        },
        CommandWire::ResumeVenue { venue: bybit(), force: false },
        CommandWire::UnadmitSymbol { venue: bybit(), account: None, symbol: "BTCUSDT".into() },
        // The orders that close: the whole long at its price, and the exit pair over it.
        submit("bybit", None, -1, 1_000.0, Some(50_000.0)),
        CommandWire::OrderList(
            oco_list(head("bybit", -1, 1_000.0), 60_000.0, Some(40_000.0), None, None, true)
                .expect("an oco"),
        ),
        modify("SELL8", None, Some(60_000.0)),
    ];
    for cmd in exits {
        for frame in [Ok(&frame), Err("the node's frame could not be read")] {
            assert!(judge(&cmd, CAP, frame, NEW_NODE).is_ok(), "{cmd:?} on {frame:?}");
        }
    }
    // The venue-less panic button is never refused, not even by a node that routes nothing.
    let old = NodeLink { routes_by_venue: Some(false), one_account_door: Some(false) };
    let panic = CommandWire::MarketExit { venue: None, account: None };
    assert_eq!(
        judge(&panic, CAP, Err("no frame"), old),
        Ok((Guardrail::NoSize, VenueCheck::NoVenue))
    );
    assert!(!reads_the_frame(&panic) && !reads_the_frame(&CommandWire::Cancel("C1".into())));
}

#[test]
fn an_order_this_box_cannot_price_is_not_refused() {
    let mut frame = bybit(0.0);
    frame.orders = vec![resting("MKT7", 1, 10.0, "market", None)];
    let not_checked = |cmd: &CommandWire, frame: Result<&CoreSnapshotWire, &str>| {
        let (guard, _) = judge(cmd, CAP, frame, NEW_NODE).expect("not checked is not refused");
        assert_eq!(guard.to_json()["within_limits"], json!(null), "{cmd:?}: {}", guard.to_json());
    };
    // V1: a market order on a linear contract, a symbol the engine does not trade, no frame at all.
    not_checked(&submit("bybit", None, 1, 10.0, None), Ok(&frame));
    let CommandWire::Submit(mut sol) = submit("bybit", None, 1, 10.0, Some(100.0)) else {
        unreachable!()
    };
    sol.symbol = "SOLUSDT".into();
    not_checked(&CommandWire::Submit(sol), Ok(&frame));
    not_checked(
        &submit("bybit", None, 1, 10.0, Some(100.0)),
        Err("the node's frame could not be read"),
    );
    // V3: a list that opens at market, its priced exit within the cap.
    let oto = oto_list(head("bybit", 1, 1.0), None, Some(0.5), None, None).expect("an oto");
    not_checked(&CommandWire::OrderList(oto), Ok(&frame));
    // V4: a combo with a leg the frame carries no value for.
    let legs = vec![
        ComboLegArg { symbol: "BTCUSDT".into(), ratio: 1, limit: Some(100.0) },
        ComboLegArg { symbol: "SOLUSDT".into(), ratio: -1, limit: Some(100.0) },
    ];
    let combo = verbs::combo_of("bybit".into(), None, 1, 2.0, legs, None, Default::default());
    not_checked(&CommandWire::Combo(combo.expect("a combo")), Ok(&frame));
    // V5: a qty raise of a resting market order (no standing price), and an order the frame does not hold.
    not_checked(&modify("MKT7", Some(100.0), None), Ok(&frame));
    assert!(refused(&modify("NOSUCH", None, Some(100.0)), Ok(&frame)).is_none());
}

#[test]
fn the_routing_rule_answers_alike_for_every_verb_that_names_a_venue() {
    let frame = frame_of(vec![
        block("okx", &["BTCUSDT"]),
        block("bybit#DEMO", &["BTCUSDT"]),
        block("bybit#LIVE", &["BTCUSDT"]),
        block("binance", &["BTCUSDT"]),
        block("binance#PERP", &["BTCUSDT.P"]),
    ]);
    let tiny = |venue: &str, account: Option<&str>| submit(venue, account, 1, 0.001, Some(1.0));
    // Every verb that names a venue, on a venue the node does not mount: one refusal, whatever the verb.
    let named = |venue: &str| -> Vec<CommandWire> {
        let v = || venue.to_string();
        vec![
            tiny(venue, None),
            CommandWire::Bracket(OrderBracketWire {
                order_client_id: None,
                venue: v(),
                symbol: "BTCUSDT".into(),
                side: 1,
                qty: 0.001,
                entry_price: Some(1.0),
                stop_loss: 0.5,
                take_profit: 2.0,
                account: None,
            }),
            CommandWire::OrderList(vike_tradehub_client::wire::OrderListWire {
                venue: v(),
                ..oco_list(head("okx", 1, 0.001), 1.0, Some(2.0), None, None, false)
                    .expect("an oco")
            }),
            CommandWire::MassCancel { venue: Some(v()), symbol: None, account: None },
            CommandWire::MarketExit { venue: Some(v()), account: None },
            CommandWire::Flatten { venue: v(), symbol: "BTCUSDT".into(), account: None },
            CommandWire::SetAccountTradingState {
                venue: v(),
                account: None,
                state: TradingState::Halted,
            },
            CommandWire::ResumeVenue { venue: v(), force: false },
            CommandWire::UnadmitSymbol { venue: v(), account: None, symbol: "BTCUSDT".into() },
            CommandWire::MountStrategy {
                venue: v(),
                account: None,
                symbol: "BTCUSDT".into(),
                interval: "1m".into(),
                controller_id: None,
                file: Some("s.rs".into()),
                name: None,
                rhai: None,
                params: json!({}),
            },
        ]
    };
    for (venue, want) in [
        ("node", Some(OrderGateRefusalId::VenueNotMounted)),
        // EXACT: routing compares with `==`, so a case or a space routes to nothing.
        ("Okx", Some(OrderGateRefusalId::VenueNotMounted)),
        ("okx ", Some(OrderGateRefusalId::VenueNotMounted)),
        ("", Some(OrderGateRefusalId::VenueBlank)),
        ("okx", None),
    ] {
        for cmd in named(venue) {
            assert_eq!(refused(&cmd, Ok(&frame)), want, "{venue:?}: {cmd:?}");
        }
    }
    // A bare venue of two accounts is refused on every verb that names no account, in the door's sentence; the
    // venue resume is the whole exchange's and names no book.
    for cmd in named("bybit") {
        let want = match &cmd {
            CommandWire::ResumeVenue { .. } => None,
            _ => Some(OrderGateRefusalId::BareVenueOfSeveralAccounts),
        };
        assert_eq!(refused(&cmd, Ok(&frame)), want, "{cmd:?}");
    }
    let Err(OrderGateRefusal::BareVenueOfSeveralAccounts { sentence }) =
        judge(&tiny("bybit", None), CAP, Ok(&frame), NEW_NODE)
    else {
        panic!("a bare venue of two accounts");
    };
    assert!(sentence.contains("bybit/DEMO") && sentence.contains("bybit/LIVE"), "{sentence}");
    // A named account: mounted passes, `DEFAULT` is the bare venue's key and never ambiguous, another is refused.
    assert_eq!(refused(&tiny("bybit", Some("LIVE")), Ok(&frame)), None);
    assert_eq!(refused(&tiny("okx", Some("DEFAULT")), Ok(&frame)), None);
    for account in ["HEDGE", "live", "not a label!"] {
        assert_eq!(
            refused(&tiny("bybit", Some(account)), Ok(&frame)),
            Some(OrderGateRefusalId::AccountNotMounted),
            "{account}"
        );
    }
    // A lane twin is one account's second lane: a default order goes where the door puts it.
    let CommandWire::Submit(mut perp) = tiny("binance", None) else { unreachable!() };
    perp.symbol = "BTCUSDT.P".into();
    assert_eq!(refused(&CommandWire::Submit(perp), Ok(&frame)), None);
    assert_eq!(refused(&tiny("binance", None), Ok(&frame)), None);

    // Rule 4: no evidence refuses only against a node that does not route by venue.
    let old_router = NodeLink { routes_by_venue: Some(false), one_account_door: Some(true) };
    let empty = CoreSnapshotWire::empty();
    for unread in [Err("no frame"), Ok(&empty)] {
        let okx = tiny("okx", None);
        assert_eq!(judge(&okx, CAP, unread, NEW_NODE).map(|(_, c)| c), Ok(VenueCheck::Unverified));
        assert_eq!(
            judge(&okx, CAP, unread, NodeLink::default()).map(|(_, c)| c),
            Ok(VenueCheck::Unverified)
        );
        let refusal = judge(&okx, CAP, unread, old_router).expect_err("a node that routes nothing");
        assert_eq!(refusal.id(), OrderGateRefusalId::NodeDoesNotRoute);
        assert!(refusal.to_string().contains("PRIMARY"), "{refusal}");
    }
    // ...and a frame that shows the venue answers for such a node too.
    assert_eq!(
        judge(&tiny("okx", None), CAP, Ok(&frame), old_router).map(|(_, c)| c),
        Ok(VenueCheck::Mounted)
    );

    // Rule 5: a bare-venue submit to a node without the one-account door, wherever its venue is.
    let old_door = NodeLink { routes_by_venue: Some(true), one_account_door: Some(false) };
    for frame in [Ok(&frame), Err("no frame")] {
        let refusal = judge(&tiny("okx", None), CAP, frame, old_door).expect_err("bare submit");
        assert_eq!(refusal, OrderGateRefusal::BareSubmitToOlderNode { venue: "okx".into() });
        assert!(judge(&tiny("okx", Some("DEFAULT")), CAP, frame, old_door).is_ok(), "a named book");
        let flatten =
            CommandWire::Flatten { venue: "okx".into(), symbol: "BTCUSDT".into(), account: None };
        assert!(judge(&flatten, CAP, frame, old_door).is_ok(), "only a submit");
    }
    // Rule 1: a verb with no venue is never checked.
    assert_eq!(
        judge(&CommandWire::Cancel("C1".into()), CAP, Ok(&frame), old_door).map(|(_, c)| c),
        Ok(VenueCheck::NoVenue)
    );
}

/// **A stopped account still makes a venue ambiguous, and counts before the first fold** (decision 0254): the same
/// words must not slide onto the other account because one stopped. With no engine block and one row or none, the
/// frame is no evidence.
#[test]
fn a_bare_venue_is_counted_over_its_not_running_accounts_too() {
    let stopped = |key: &str| ArmingNotRunningViewWire {
        venue: "bybit".into(),
        account: key.split_once('#').map(|(_, label)| label.to_string()),
        route_key: key.into(),
        tier: None,
        reason: "no credentials".into(),
        detail: None,
        block_word: None,
        refusal_id: None,
    };
    type Row<'a> = (&'a [&'a str], &'a [&'a str], Result<VenueCheck, OrderGateRefusalId>);
    let rows: [Row<'_>; 7] = [
        (&[], &[], Ok(VenueCheck::Unverified)),
        (&[], &["bybit#DEMO"], Ok(VenueCheck::Unverified)),
        (&[], &["bybit#DEMO", "bybit#LIVE"], Err(OrderGateRefusalId::BareVenueOfSeveralAccounts)),
        (&["bybit#LIVE"], &["bybit#DEMO"], Err(OrderGateRefusalId::BareVenueOfSeveralAccounts)),
        (&["bybit#DEMO", "bybit#LIVE"], &[], Err(OrderGateRefusalId::BareVenueOfSeveralAccounts)),
        (&["bybit#LIVE"], &[], Ok(VenueCheck::Mounted)),
        (&["okx"], &["bybit#DEMO"], Ok(VenueCheck::Mounted)),
    ];
    let cmd = CommandWire::MarketExit { venue: Some("bybit".into()), account: None };
    for (blocks, not_running, want) in rows {
        let mut frame = frame_of(blocks.iter().map(|key| block(key, &["BTCUSDT"])).collect());
        frame.not_running = not_running.iter().map(|key| stopped(key)).collect();
        let got = judge(&cmd, CAP, Ok(&frame), NEW_NODE).map(|(_, c)| c).map_err(|r| r.id());
        assert_eq!(got, want, "{blocks:?} {not_running:?}");
    }
}

#[test]
fn a_pinned_order_client_id_outside_the_charset_is_refused_by_every_builder() {
    let long = "A".repeat(33);
    for bad in ["MYPINNED-001", "", "with space", long.as_str()] {
        let refusal = verbs::order_client_id_refusal(bad).expect("outside the charset");
        assert_eq!(refusal.id(), OrderGateRefusalId::OrderClientIdCharset, "{bad:?}");
        assert!(refusal.rendered().contains(verbs::ORDER_CLIENT_ID_CHARSET), "{refusal}");
    }
    assert_eq!(verbs::order_client_id_refusal("Pinned001"), None);
    // The list builder both doors pin leg ids through asks it, so neither door can forget it.
    let list =
        oco_list(head("bybit", -1, 1.0), 110.0, Some(90.0), None, None, true).expect("an oco");
    let why = verbs::with_leg_order_client_ids(list.clone(), &["Good1".into(), "BAD-2".into()])
        .expect_err("a leg id outside the charset");
    assert!(why.starts_with("error [order.edge.order_client_id_charset]: \"BAD-2\""), "{why}");
    assert!(verbs::with_leg_order_client_ids(list, &["Good1".into(), "Good2".into()]).is_ok());
}
