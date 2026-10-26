//! Engine 0 is not always its venue's DEFAULT account, and an order on it must still be recorded.
//!
//! `coid_venue` is written only for an order routed to a non-primary engine: engine 0 was assumed to
//! be the venue's default account, whose orders every fallback (`route_event`'s venue rung,
//! `held_release_engine`, each `unwrap_or(0)`) re-derives from the venue string. The node builds the
//! DEFAULT engines first and the labelled ones after, so engine 0 is a LABELLED account exactly when
//! no default account runs at boot (decisions 0156, 0172), and then a fill of its order carries no
//! row to find it by. [`CoreThread::coid_row_needed`] is the one predicate; these tests drive its
//! three consumers (the single submit, the batch arm, the held-exit release) and pin the control:
//! a default account at engine 0 keeps `coid_venue` empty.

use super::*;
use vike_model::events::{FillEvent, PositionSide, TradeId};

/// The venue's default account does not run: `binance#ALT` is engine 0 and its lane twin
/// `binance#PERP` engine 1, both claiming [`SYMBOL`] (so the symbol rung cannot tell them apart).
fn labelled_pair_core() -> CoreThread<RecordingClient> {
    core_of(
        engine("binance#ALT", 1_000.0),
        vec![(2_000.0, engine("binance#PERP", 2_000.0))],
        CoreConfig { seed_cash: 1_000.0, ..CoreConfig::default() },
    )
}

/// …and the default account started LATER (`add_engine_hot` appends it): `binance#ALT` at 0, the
/// default `binance` at 1, so the venue string now names engine 1.
fn labelled_then_default_core() -> CoreThread<RecordingClient> {
    core_of(
        engine("binance#ALT", 1_000.0),
        vec![(2_000.0, engine("binance", 2_000.0))],
        CoreConfig { seed_cash: 1_000.0, ..CoreConfig::default() },
    )
}

/// A venue fill of one order, as a bridge emits it: the canonical venue, no account.
fn fill_of(coid: &str, trade_id: &'static str) -> Event {
    Event::Fill(FillEvent {
        trade_id: TradeId::from(trade_id),
        client_order_id: coid.to_string(),
        venue: CANON.into(),
        symbol: SYMBOL.into(),
        side: 1,
        last_qty: 1.0,
        last_px: 100.0,
        commission: 0.0,
        commission_asset: String::new().into(),
        liquidity_side: "taker".to_string().into(),
        ts: 0,
        mark_price: Some(100.0),
        position_side: PositionSide::Both,
    })
}

fn held(core: &CoreThread<RecordingClient>, idx: usize) -> f64 {
    core.eng(idx)
        .account
        .positions
        .get(&(ustr::Ustr::from(CANON), ustr::Ustr::from(SYMBOL), PositionSide::Both))
        .map(|p| p.size)
        .unwrap_or(0.0)
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
    assert_eq!(core.engine_idx_for_venue_symbol(CANON, SYMBOL), None, "the symbol cannot decide");

    let coids = core.apply_intent(OrderIntent::Submit(Box::new(open_order_for(CANON, alt()))), 1);
    assert_eq!(coids.len(), 1, "named account, so minted: {coids:?}");
    assert_eq!(core.eng(0).client.submissions.len(), 1, "precondition: the order reached ALT");

    // The behaviour first, the mechanism after: what the operator sees is a fill that books.
    let fill = fill_of(&coids[0], "z-1");
    assert_eq!(core.route_event(&fill), Some(0), "its own engine, not an unattributed fill");
    core.dispatch(Ingest::Event(fill));
    assert_eq!(held(&core, 0), 1.0, "booked into ALT's account");
    assert_eq!(held(&core, 1), 0.0, "…and not into the twin's");
    assert_eq!(core.coid_venue.get(&coids[0]), Some(&0), "the row a non-primary engine gets");
}

/// The same hole a STRATEGY's batch reaches: the batch arm mints and journals its legs and submits
/// them to engine 0 in one call, so it never re-enters `lower_submit`.
#[test]
fn a_batch_at_a_labelled_engine_zero_records_every_leg() {
    let mut core = labelled_pair_core();
    let coids = core.apply_intent_routed(
        OrderIntent::SubmitBatch(vec![open_order(CANON), open_order(CANON)]),
        1,
        CancelIntent::Unspecified,
        EngineRoute::Engine(0),
    );
    assert_eq!(coids.len(), 2, "both legs minted: {coids:?}");
    assert_eq!(core.eng(0).client.submissions.len(), 2, "precondition: one batch, on engine 0");
    for coid in &coids {
        assert_eq!(core.coid_venue.get(coid), Some(&0), "leg {coid} has its row");
        assert_eq!(core.route_event(&fill_of(coid, "z-2")), Some(0));
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
    child.client_order_id = "alt-child".into();
    child.parent_order_id = Some("alt-parent".into());
    let coids = core.apply_intent(OrderIntent::Submit(Box::new(child)), 1);
    assert_eq!(coids, vec!["alt-child".to_string()]);
    assert!(core.held_orders.contains_key("alt-child"), "precondition: held pending its parent");
    assert_eq!(submitted_anywhere(&core), 0, "nothing went live yet");

    core.drive_contingency_on_fill("alt-parent", 2);
    assert!(!core.held_orders.contains_key("alt-child"), "released");
    assert_eq!(core.eng(0).client.submissions.len(), 1, "onto ALT, the account that named it");
    assert_eq!(
        core.eng(1).client.submissions.len(),
        0,
        "NOT onto the default account's book, which holds no position to reduce"
    );
}

/// **THE CONTROL: a default account at engine 0 records nothing, byte for byte as before.** A
/// single-account node, and the two-account node whose engine 0 is the default one: an order for
/// engine 0 leaves `coid_venue` empty, an order for the labelled engine 1 still gets its row.
#[test]
fn a_default_engine_zero_keeps_coid_venue_empty() {
    let mut single = single_account_core();
    assert!(!single.coid_row_needed(0));
    let coids = single.apply_intent(OrderIntent::Submit(Box::new(open_order(CANON))), 1);
    assert_eq!(coids.len(), 1);
    assert!(single.coid_venue.is_empty(), "single account: no row for engine 0");

    let mut two = ticket_core();
    assert!(!two.coid_row_needed(0), "engine 0 is the default account");
    assert!(two.coid_row_needed(1), "a non-primary engine always records");
    let on_default =
        two.apply_intent(OrderIntent::Submit(Box::new(open_order_for(CANON, default_acct()))), 1);
    assert_eq!(on_default.len(), 1);
    assert!(two.coid_venue.is_empty(), "the default account at engine 0: still no row");
    let on_alt = two.apply_intent(OrderIntent::Submit(Box::new(open_order_for(CANON, alt()))), 2);
    assert_eq!(two.coid_venue.get(&on_alt[0]), Some(&1), "ALT keeps its row");
    assert_eq!(two.coid_venue.len(), 1);

    assert!(labelled_pair_core().coid_row_needed(0), "…and only a labelled engine 0 records");
}

/// **A bracket records its rows by the same predicate, however it reached engine 0.** A bracket a MOUNT routed
/// onto a labelled engine 0 (`EngineRoute::Engine(0)`, the spec naming no account) records all three legs, as
/// one that NAMED the account does; a bracket on a default engine 0, named or not, records nothing, as before.
#[test]
fn a_bracket_records_its_rows_by_coid_row_needed_however_it_reached_engine_zero() {
    let spec = |account| vike_model::BracketSpec {
        venue: CANON.to_string(),
        symbol: SYMBOL.to_string(),
        side: 1,
        qty: 1.0,
        entry_price: Some(100.0),
        stop_loss: 90.0,
        take_profit: 110.0,
        account,
    };
    let mut labelled = labelled_pair_core();
    let coids = labelled.apply_intent_routed(
        OrderIntent::Bracket(Box::new(spec(None))),
        1,
        CancelIntent::Unspecified,
        EngineRoute::Engine(0),
    );
    assert_eq!(coids.len(), 3, "the mount-routed bracket minted its legs: {coids:?}");
    for coid in &coids {
        assert_eq!(labelled.coid_venue.get(coid), Some(&0), "{coid} is recorded against engine 0");
    }

    let mut default_at_zero = ticket_core();
    for (n, account) in [None, Some(default_acct())].into_iter().enumerate() {
        let route = if account.is_some() { EngineRoute::Payload } else { EngineRoute::Engine(0) };
        let coids = default_at_zero.apply_intent_routed(
            OrderIntent::Bracket(Box::new(spec(account))),
            1 + n as i64,
            CancelIntent::Unspecified,
            route,
        );
        assert_eq!(coids.len(), 3, "bracket {n} minted: {coids:?}");
    }
    assert!(default_at_zero.coid_venue.is_empty(), "a default engine 0 records no bracket row");
}
