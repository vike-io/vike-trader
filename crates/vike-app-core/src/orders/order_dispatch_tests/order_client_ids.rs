//! The v2 ids a desktop session mints (decision 0270): `G`, the block's shape and account code, the
//! intent off the frame, a bracket's stem, and the dispatch order unchanged.

use super::*;
use vike_model::orders::client_id::{
    OrderClientIdParts, OrderClientIdReading, OrderClientIdStem, parse_order_client_id,
};

/// `id` read as a v2 id, or a panic naming it.
fn v2(id: &str) -> OrderClientIdParts<'_> {
    match parse_order_client_id(id) {
        Some(OrderClientIdReading::V2(p)) => p,
        other => panic!("{id} is not a v2 id: {other:?}"),
    }
}

/// The ids of the orders a plan submits, in order.
fn ids(plan: &DispatchPlan) -> Vec<String> {
    plan.commands
        .iter()
        .filter_map(|c| match c {
            Command::Order(OrderIntent::Submit(r)) => Some(r.order_client_id.clone()),
            _ => None,
        })
        .collect()
}

/// An id's unique part as the number it renders.
fn unique(id: &str) -> u64 {
    u64::from_str_radix(v2(id).unique, 36).expect("base36")
}

/// One frame of `inputs` through a fresh v2 session.
fn v2_plan(inputs: DispatchInputs, s: &CoreSnapshot) -> DispatchPlan {
    let mut mint = OrderClientIdGenerator::new_v2(None);
    plan_dispatch(inputs, &OrderLimits::default(), s, 3, 0, Some(&mut mint))
}

/// Every submit path stamps `G` (manual GUI order entry is its own source, every panel), no strategy,
/// and the intent of what it does: the exit of a long closes it, every buy here opens. A node that
/// publishes no account code and no shape gets `00` and the full shape; the window counter still
/// advances once per id.
#[test]
fn every_submit_path_of_a_v2_session_mints_a_g_id() {
    let s = snap_with_position("binance", "BTCUSDT", 2.0);
    for (source, intent) in [
        (SubmitSource::Trade, OrderClientIdIntent::OpenLong),
        (SubmitSource::Dom, OrderClientIdIntent::OpenLong),
        (SubmitSource::DomExit, OrderClientIdIntent::CloseLong),
        (SubmitSource::Cockpit, OrderClientIdIntent::OpenLong),
        (SubmitSource::Options, OrderClientIdIntent::OpenLong),
    ] {
        let plan = v2_plan(one_intent_for(source), &s);
        let [id] = ids(&plan).try_into().expect("one order");
        let p = v2(&id);
        assert_eq!(
            (p.src, p.strat, p.account),
            (
                OrderClientIdSource::Gui,
                OrderClientIdStrategyCode::NONE,
                Some(OrderClientIdAccountCode::NONE)
            ),
            "{id}"
        );
        assert_eq!((p.intent, p.role, p.generation), (intent, None, 0), "{source:?}: {id}");
        assert_eq!((p.origin, id.len()), (None, 21), "no tag, the full shape: {id}");
        assert_eq!(plan.next_win_n, 4, "{source:?}");
    }
}

/// The block the order REACHES decides the shape and the account code: the default account's
/// compact shape carries no code, the `SUB` account's full one carries its own.
#[test]
fn the_block_an_order_reaches_decides_its_shape_and_account_code() {
    let mut s = with_sub(&["BTCUSDT"]);
    s.portfolio.venues[0].account_code = Some(OrderClientIdAccountCode::of_id(7));
    s.portfolio.venues[0].order_client_id_shape =
        Some(OrderClientIdShape { account: false, u_width: 11, max_gen: 0, tag: false });
    s.portfolio.venues[1].account_code = Some(OrderClientIdAccountCode::of_id(11));
    let inputs = DispatchInputs {
        trade_actions: vec![
            (btc(), place(Origin::Ticket, None)),
            (sub("BTCUSDT"), place(Origin::Ticket, None)),
        ],
        ..DispatchInputs::default()
    };
    let minted = ids(&v2_plan(inputs, &s));
    let (default, labelled) = (v2(&minted[0]), v2(&minted[1]));
    assert_eq!((default.account, default.unique.len()), (None, 11), "{}", minted[0]);
    assert_eq!(
        (labelled.account, labelled.unique.len()),
        (Some(OrderClientIdAccountCode::of_id(11)), 12)
    );
}

/// The intent reads the account's position in the symbol: on a perpetual held long, a Close is a
/// reduce-only sell (`CL`) and a Reverse a plain sell that opens a short (`FL`).
#[test]
fn the_intent_reads_the_position_the_order_meets() {
    let s = snap_with_position("binance", "BTCUSDT.P", 2.0);
    let inputs = DispatchInputs {
        trade_actions: vec![
            (btc_perp(), TradeAction::ClosePosition),
            (btc_perp(), TradeAction::Reverse),
        ],
        ..DispatchInputs::default()
    };
    let intents: Vec<_> = ids(&v2_plan(inputs, &s)).iter().map(|id| v2(id).intent).collect();
    assert_eq!(intents, [OrderClientIdIntent::CloseLong, OrderClientIdIntent::PlainCloseLong]);
}

/// The bracket's stem as the plan sends it.
fn stem_of(plan: &DispatchPlan) -> Option<String> {
    plan.commands.iter().find_map(|c| match c {
        Command::Order(OrderIntent::Bracket(spec)) => Some(spec.order_client_id.clone()),
        _ => None,
    })?
}

/// A TP/SL bracket names its group stem only from a v2 session to a node that takes one: `G`, no
/// strategy, the account code, ONE sequence number (the next order's unique part is one past it),
/// and no `next_win_n`. A node without `bracket-stem`, a v1 session and the deterministic spelling
/// send it with no id, as before.
#[test]
fn a_bracket_names_its_stem_only_where_the_node_takes_it() {
    let s = snap_with_position("binance", "BTCUSDT", 2.0);
    let limits = OrderLimits::default();
    let frame = |bracket_stem| {
        let mut i = one_intent_for(SubmitSource::TradeBracket);
        i.trade_actions.extend(one_intent_for(SubmitSource::Trade).trade_actions);
        DispatchInputs { bracket_stem, ..i }
    };
    let plan = v2_plan(frame(true), &s);
    let stem = stem_of(&plan).expect("a stem");
    assert!(OrderClientIdStem::given(&stem).is_some() && stem.starts_with("G000000"), "{stem}");
    let [after] = ids(&plan).try_into().expect("the plain order");
    assert_eq!(unique(&after), u64::from_str_radix(&stem[7..], 36).expect("base36") + 1);
    assert_eq!(plan.next_win_n, 4, "the stem takes no window number");

    assert_eq!(stem_of(&v2_plan(frame(false), &s)), None, "a node without the word");
    let mut v1 = OrderClientIdGenerator::new(Some("cafef00d"));
    let plan = plan_dispatch(frame(true), &limits, &s, 3, 0, Some(&mut v1));
    assert_eq!((stem_of(&plan), ids(&plan)), (None, vec!["uicafef00d0".to_string()]));
    assert_eq!(stem_of(&plan_dispatch(frame(true), &limits, &s, 3, 0, None)), None);
}

/// A v2 session keeps the emission order (`emission_order_is_preserved`'s frame): the Trade window,
/// the Account window's cancels, the cockpit, the options; its ids are `G` and minted in that order.
#[test]
fn a_v2_session_keeps_the_emission_order() {
    let s = binance_snap();
    let inputs = DispatchInputs {
        trade_actions: vec![
            (btc(), ticket(TicketKind::Market, 1.0, None, None)),
            (btc(), place(Origin::Ladder, None)),
        ],
        account_cancels: vec!["tc".into()],
        cockpit_commands: vec![CockpitCommand::Submit {
            token: "TOK".into(),
            side: 1,
            price: Some(0.5),
            qty: 1.0,
        }],
        opt_orders: one_intent_for(SubmitSource::Options).opt_orders,
        opt_cancels: vec!["oc".into()],
        ..DispatchInputs::default()
    };
    let plan = v2_plan(inputs, &s);
    let kinds: Vec<String> = plan
        .commands
        .iter()
        .map(|c| match c {
            Command::Order(OrderIntent::Submit(r)) => {
                format!("{}:{}", r.venue, &r.order_client_id[..1])
            }
            Command::Order(OrderIntent::Cancel(c)) => format!("cancel:{c}"),
            other => format!("{other:?}"),
        })
        .collect();
    assert_eq!(
        kinds,
        ["binance:G", "binance:G", "cancel:tc", "polymarket:G", "deribit:G", "cancel:oc"]
    );
    let u: Vec<u64> = ids(&plan).iter().map(|id| unique(id)).collect();
    assert!(u.windows(2).all(|w| w[1] == w[0] + 1), "one sequence, in order: {u:?}");
}
