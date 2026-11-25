//! Engine 0 is not always its venue's DEFAULT account, and an order on it must still be recorded.
//!
//! `order_client_id_venue` is written only for an order routed to a non-primary engine: engine 0 was assumed to
//! be the venue's default account, whose orders every fallback (`route_event`'s venue rung,
//! `held_release_engine`, each `unwrap_or(0)`) re-derives from the venue string. The node builds the
//! DEFAULT engines first and the labelled ones after, so engine 0 is a LABELLED account exactly when
//! no default account runs at boot (decisions 0156, 0172), and then a fill of its order carries no
//! row to find it by. [`CoreThread::order_client_id_row_needed`] is the one predicate; these tests drive its
//! three consumers (the single submit, the batch arm, the held-exit release) and pin the control:
//! a default account at engine 0 keeps `order_client_id_venue` empty.

use super::*;
use vike_model::events::{FillEvent, PositionSide, TradeId};

/// `binance#ALT` at engine 0 (seed 1_000) and `second` at engine 1 (seed 2_000), both claiming
/// [`SYMBOL`] (so the symbol rung cannot tell them apart).
fn alt_then(second: &str) -> CoreThread<RecordingClient> {
    core_of(
        engine("binance#ALT", 1_000.0),
        vec![(2_000.0, engine(second, 2_000.0))],
        CoreConfig { seed_cash: 1_000.0, ..CoreConfig::default() },
    )
}

/// The venue's default account does not run: `binance#ALT` is engine 0 and its lane twin
/// `binance#PERP` engine 1.
fn labelled_pair_core() -> CoreThread<RecordingClient> {
    alt_then("binance#PERP")
}

/// …and the default account started LATER (`add_engine_hot` appends it): `binance#ALT` at 0, the
/// default `binance` at 1, so the venue string now names engine 1.
fn labelled_then_default_core() -> CoreThread<RecordingClient> {
    alt_then("binance")
}

/// A venue fill of one order, as a bridge emits it: the canonical venue, no account.
fn fill_of(order_client_id: &str, trade_id: &'static str) -> Event {
    Event::Fill(FillEvent {
        trade_id: TradeId::from(trade_id),
        order_client_id: order_client_id.to_string(),
        venue: CANON.into(),
        symbol: SYMBOL.into(),
        side: 1,
        last_qty: 1.0,
        last_price: 100.0,
        commission: 0.0,
        commission_asset: String::new().into(),
        liquidity_side: "taker".to_string().into(),
        timestamp: 0,
        mark_price: Some(100.0),
        position_side: PositionSide::Both,
    })
}

fn held(core: &CoreThread<RecordingClient>, index: usize) -> f64 {
    crate::runtime::test_support::held_at(core, index, CANON, SYMBOL)
}

/// ⚠ **THE HEADLINE.** A fill of a labelled order at engine 0 reaches engine 0. Without a row it
/// fell through every rung of `route_event` (no stamped key, no row, an ambiguous symbol) to the
/// "foreign fill on a several-account venue" refusal and nothing folded: the order filled at the
/// venue and the core never booked it.
#[test]
fn a_labelled_order_at_engine_zero_has_its_fill_attributed_to_it() {
    let mut core = labelled_pair_core();
    assert!(core.multi_account, "two engines of one exchange");
    assert_eq!(core.engine.route_key, "binance#ALT", "the harness puts a LABELLED account at 0");
    assert_eq!(core.engine_index_for_venue_symbol(CANON, SYMBOL), None, "the symbol cannot decide");

    let order_client_ids =
        core.apply_intent(OrderIntent::Submit(Box::new(open_order_for(CANON, alt()))), 1);
    assert_eq!(order_client_ids.len(), 1, "named account, so minted: {order_client_ids:?}");
    assert_eq!(core.engine(0).client.submissions.len(), 1, "precondition: the order reached ALT");

    // The behaviour first, the mechanism after: what the operator sees is a fill that books.
    let fill = fill_of(&order_client_ids[0], "z-1");
    assert_eq!(core.route_event(&fill), Some(0), "its own engine, not an unattributed fill");
    core.dispatch(Ingest::Event(fill));
    assert_eq!(held(&core, 0), 1.0, "booked into ALT's account");
    assert_eq!(held(&core, 1), 0.0, "…and not into the twin's");
    assert_eq!(
        core.order_client_id_venue.get(&order_client_ids[0]),
        Some(&0),
        "the row a non-primary engine gets"
    );
}

/// The same hole a STRATEGY's batch reaches: the batch arm mints and journals its legs and submits
/// them to engine 0 in one call, so it never re-enters `lower_submit`.
#[test]
fn a_batch_at_a_labelled_engine_zero_records_every_leg() {
    let mut core = labelled_pair_core();
    let order_client_ids = core.apply_intent_routed(
        OrderIntent::SubmitBatch(vec![open_order(CANON), open_order(CANON)]),
        1,
        OrderCancelIntent::Unspecified,
        EngineRoute::Engine(0),
    );
    assert_eq!(order_client_ids.len(), 2, "both legs minted: {order_client_ids:?}");
    assert_eq!(core.engine(0).client.submissions.len(), 2, "precondition: one batch, on engine 0");
    for order_client_id in &order_client_ids {
        assert_eq!(
            core.order_client_id_venue.get(order_client_id),
            Some(&0),
            "leg {order_client_id} has its row"
        );
        assert_eq!(core.route_event(&fill_of(order_client_id, "z-2")), Some(0));
    }
}

/// ⚠ **THE MONEY SHAPE.** A held OTO child of a labelled order at engine 0 is released onto engine
/// 0, not onto the venue's DEFAULT account that started later at another index
/// (`held_release_engine` re-derives the engine from the venue string when there is no row).
#[test]
fn a_held_exit_of_a_labelled_engine_zero_is_released_onto_that_engine() {
    let mut core = labelled_then_default_core();
    assert!(core.multi_account);

    let mut child = open_order_for(CANON, alt());
    child.order_client_id = "alt-child".into();
    child.parent_order_id = Some("alt-parent".into());
    let order_client_ids = core.apply_intent(OrderIntent::Submit(Box::new(child)), 1);
    assert_eq!(order_client_ids, vec!["alt-child".to_string()]);
    assert!(core.held_orders.contains_key("alt-child"), "precondition: held pending its parent");
    assert_eq!(submitted_anywhere(&core), 0, "nothing went live yet");

    core.drive_contingency_on_fill("alt-parent", 1.0, 100.0, true, 2);
    assert!(!core.held_orders.contains_key("alt-child"), "released");
    assert_eq!(core.engine(0).client.submissions.len(), 1, "onto ALT, the account that named it");
    assert_eq!(
        core.engine(1).client.submissions.len(),
        0,
        "NOT onto the default account's book, which holds no position to reduce"
    );
}

/// **THE CONTROL: a default account at engine 0 records nothing, byte for byte as before.** A
/// single-account node, and the two-account node whose engine 0 is the default one: an order for
/// engine 0 leaves `order_client_id_venue` empty, an order for the labelled engine 1 still gets its row.
#[test]
fn a_default_engine_zero_keeps_order_client_id_venue_empty() {
    let mut single = single_account_core();
    assert!(!single.order_client_id_row_needed(0));
    let order_client_ids = single.apply_intent(OrderIntent::Submit(Box::new(open_order(CANON))), 1);
    assert_eq!(order_client_ids.len(), 1);
    assert!(single.order_client_id_venue.is_empty(), "single account: no row for engine 0");

    let mut two = ticket_core();
    assert!(!two.order_client_id_row_needed(0), "engine 0 is the default account");
    assert!(two.order_client_id_row_needed(1), "a non-primary engine always records");
    let on_default = two
        .apply_intent(OrderIntent::Submit(Box::new(open_order_for(CANON, default_account()))), 1);
    assert_eq!(on_default.len(), 1);
    assert!(two.order_client_id_venue.is_empty(), "the default account at engine 0: still no row");
    let on_alt = two.apply_intent(OrderIntent::Submit(Box::new(open_order_for(CANON, alt()))), 2);
    assert_eq!(two.order_client_id_venue.get(&on_alt[0]), Some(&1), "ALT keeps its row");
    assert_eq!(two.order_client_id_venue.len(), 1);

    assert!(
        labelled_pair_core().order_client_id_row_needed(0),
        "…and only a labelled engine 0 records"
    );
}

/// **A bracket records its rows by the same predicate, however it reached engine 0.** A bracket a MOUNT routed
/// onto a labelled engine 0 (`EngineRoute::Engine(0)`, the spec naming no account) records all three legs, as
/// one that NAMED the account does; a bracket on a default engine 0, named or not, records nothing, as before.
#[test]
fn a_bracket_records_its_rows_by_order_client_id_row_needed_however_it_reached_engine_zero() {
    let spec = |account| vike_model::OrderBracket {
        venue: CANON.to_string(),
        symbol: SYMBOL.to_string(),
        side: 1,
        qty: 1.0,
        entry_price: Some(100.0),
        stop_loss: 90.0,
        take_profit: 110.0,
        account,
        order_client_id: None,
    };
    let mut labelled = labelled_pair_core();
    let order_client_ids = labelled.apply_intent_routed(
        OrderIntent::Bracket(Box::new(spec(None))),
        1,
        OrderCancelIntent::Unspecified,
        EngineRoute::Engine(0),
    );
    assert_eq!(
        order_client_ids.len(),
        3,
        "the mount-routed bracket minted its legs: {order_client_ids:?}"
    );
    for order_client_id in &order_client_ids {
        assert_eq!(
            labelled.order_client_id_venue.get(order_client_id),
            Some(&0),
            "{order_client_id} is recorded against engine 0"
        );
    }

    let mut default_at_zero = ticket_core();
    for (n, account) in [None, Some(default_account())].into_iter().enumerate() {
        let route = if account.is_some() { EngineRoute::Payload } else { EngineRoute::Engine(0) };
        let order_client_ids = default_at_zero.apply_intent_routed(
            OrderIntent::Bracket(Box::new(spec(account))),
            1 + n as i64,
            OrderCancelIntent::Unspecified,
            route,
        );
        assert_eq!(order_client_ids.len(), 3, "bracket {n} minted: {order_client_ids:?}");
    }
    assert!(
        default_at_zero.order_client_id_venue.is_empty(),
        "a default engine 0 records no bracket row"
    );
}
