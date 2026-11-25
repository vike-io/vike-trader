//! A DENIED order is listed, typed, on the published snapshot (`CoreSnapshot::denied_orders`, filled by
//! `journaling.rs`'s `note_denied`): a denied order never enters a registry, so no order row carries it, and the
//! tradehub's alert mount reads this list to tell each veto once.
//!
//! White-box on a synchronous core: a strategy's order goes through `apply_intent` (the lowering a mount's intent
//! takes), an operator's through `dispatch` (the call a `Command` from the door makes), and both meet the gate of a
//! HALTED engine. The drain's own refusal is pinned in `strategy_order_broker.rs`.

use super::*;
use vike_execution::testing::{RecordingClient, engine_on};
use vike_model::{OrderDenial, OrderDenialId, OrderRequest};

use crate::runtime::journaling::DENIED_ORDERS_KEPT;
use crate::runtime::test_support::core_of;

const VENUE: &str = "bybit";
const SYMBOL: &str = "BTCUSDT";

/// A core whose one engine is HALTED, so the gate vetoes every opening order.
fn halted() -> CoreThread<RecordingClient> {
    let mut c = core_of(
        engine_on(VENUE, SYMBOL, RecordingClient::default()),
        Vec::new(),
        CoreConfig::default(),
    );
    c.engine.trading_state = TradingState::Halted;
    c
}

/// An opening limit buy under `order_client_id`.
fn buy(order_client_id: &str) -> Box<OrderRequest> {
    Box::new(OrderRequest {
        order_client_id: order_client_id.into(),
        venue: VENUE.into(),
        symbol: SYMBOL.into(),
        side: 1,
        qty: 1.0,
        price: Some(100.0),
        order_type: vike_model::OrderType::Limit,
        ..Default::default()
    })
}

/// `(sequence, order client id)` of every listed denial, oldest first.
fn listed(c: &CoreThread<RecordingClient>) -> Vec<(u64, String)> {
    let list = c.denied_orders.as_deref().unwrap_or_default();
    list.iter().map(|d| (d.sequence, d.event.order_client_id.clone())).collect()
}

/// The gate's veto of a strategy's order and of an operator's (the door already said Ack) are each listed with the
/// venue and symbol of the refused order, the refusal's catalog id and its words, numbered in order; the publish
/// hands the core's own list on.
#[test]
fn a_strategy_veto_and_a_door_veto_are_listed_typed_and_published() {
    let mut c = halted();

    c.apply_intent(OrderIntent::Submit(buy("strategy-1")), 7);
    c.dispatch(Ingest::Command(Command::Order(OrderIntent::Submit(buy("door-1")))));

    assert!(c.engine.client.submissions.is_empty(), "premise: nothing reached the venue");
    assert_eq!(listed(&c), [(1, "strategy-1".to_string()), (2, "door-1".to_string())]);
    let list = c.denied_orders.clone().expect("listed");
    for denied in list.iter() {
        assert_eq!((denied.venue.as_str(), denied.symbol.as_str()), (VENUE, SYMBOL));
        assert_eq!(denied.event.refusal_id.as_deref(), Some(OrderDenialId::Halted.word()));
        assert_eq!(denied.event.reason, OrderDenial::Halted.to_string());
    }

    c.publish_guarded();
    let published = c.snapshot.load_full().denied_orders.clone().expect("published");
    assert!(Arc::ptr_eq(&published, &list), "one refcount, no copy");
}

/// The list keeps the newest [`DENIED_ORDERS_KEPT`] and the sequence goes on past the ones that rolled off, so a
/// reader that last saw an older one still tells the new ones apart.
#[test]
fn the_list_keeps_the_newest_and_numbers_on() {
    let mut c = halted();
    let denied = DENIED_ORDERS_KEPT + 3;

    for n in 1..=denied {
        c.apply_intent(OrderIntent::Submit(buy(&format!("o-{n}"))), 0);
    }

    let kept = listed(&c);
    assert_eq!(kept.len(), DENIED_ORDERS_KEPT);
    assert_eq!(kept.first(), Some(&(4, "o-4".to_string())));
    assert_eq!(kept.last(), Some(&(denied as u64, format!("o-{denied}"))));
}

/// Nothing denied: nothing listed, and the publish carries `None` (no refcount).
#[test]
fn an_accepted_order_lists_nothing() {
    let mut c = halted();
    c.engine.trading_state = TradingState::Active;

    c.apply_intent(OrderIntent::Submit(buy("ok-1")), 0);

    assert_eq!(c.engine.client.submissions.len(), 1, "premise: the order was sent");
    assert!(c.denied_orders.is_none());
    c.publish_guarded();
    assert!(c.snapshot.load().denied_orders.is_none());
}
