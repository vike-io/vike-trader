//! A modify the venue CANNOT do is refused OUT LOUD, never accepted and swallowed.
//!
//! Every adapter without a native amend takes `ExecutionClient::modify`'s default no-op. Before
//! `ExecutionEngine::modify_order` consulted the amend table, a modify on such a venue was accepted
//! at every door and then did nothing at the venue and published nothing: the resting order kept
//! its terms and the operator could not tell a lost modify from a done one.
//!
//! The roster is DERIVED from `vike_venues::amend_semantics`, not listed here, so a venue that
//! flips to a real amend leaves this suite's refusal set without an edit, and a new
//! `Unsupported` row is covered on arrival. Every assertion is on what reached the venue
//! (`RecordingClient::modifies`) and on the event the engine published.

use vike_exec::execution_engine::MODIFY_UNSUPPORTED_REASON;
use vike_exec::testing::RecordingClient;
use vike_exec::{EventBus, ExecutionEngine, Outbox};
use vike_model::OrderRequest;
use vike_model::events::{Event, OrderAccepted, OrderSubmitted};
use vike_venues::AmendSemantics;

use crate::support::EngineBuilder;

fn limit_buy(venue: &str, coid: &str) -> OrderRequest {
    OrderRequest {
        client_order_id: coid.into(),
        venue: venue.into(),
        symbol: "EURUSD".into(),
        side: 1,
        qty: 1000.0,
        order_type: vike_model::OrderType::Limit,
        price: Some(0.9),
        ts: 1,
        ..Default::default()
    }
}

/// An engine labelled `venue`, with one order resting ACCEPTED (so the modify path is reachable;
/// a SUBMITTED or terminal order is a documented no-op).
fn engine_with_resting_order(venue: &str, coid: &str) -> ExecutionEngine<RecordingClient> {
    let mut e =
        EngineBuilder { venue: venue.into(), symbol: "EURUSD".into(), ..Default::default() }
            .build();
    let mut outbox = Outbox::default();
    e.submit_order(&limit_buy(venue, coid), 0, &mut outbox);
    assert_eq!(e.client.submissions.len(), 1, "precondition: the order must be admitted");
    let mut bus = EventBus::new();
    bus.publish(
        Event::OrderSubmitted(OrderSubmitted { client_order_id: coid.into(), ts: 0 }),
        &mut e,
    );
    bus.publish(
        Event::OrderAccepted(OrderAccepted {
            client_order_id: coid.into(),
            venue_order_id: Some("v1".into()),
            ts: 1,
        }),
        &mut e,
    );
    e
}

fn modify_rejections(outbox: &Outbox) -> Vec<(String, String)> {
    outbox
        .0
        .iter()
        .filter_map(|e| match e {
            Event::OrderModifyRejected(r) => {
                Some((r.client_order_id.clone(), r.reason.to_string()))
            }
            _ => None,
        })
        .collect()
}

/// THE CONTRACT: for every roster venue whose declared amend convention is `Unsupported`, a modify
/// publishes exactly one NON-TERMINAL `OrderModifyRejected` naming the venue, and nothing reaches
/// the client.
#[test]
fn a_modify_on_a_venue_with_no_amend_is_an_explicit_rejection() {
    let mut covered = 0;
    for &venue in vike_model::VENUES {
        if vike_venues::amend_semantics(venue) != AmendSemantics::Unsupported {
            continue;
        }
        covered += 1;
        let mut e = engine_with_resting_order(venue, "c1");
        let mut outbox = Outbox::default();
        e.modify_order("c1", None, Some(0.95), 2, &mut outbox);

        assert!(
            e.client.modifies.is_empty(),
            "{venue}: a modify the venue cannot do must not be forwarded to the no-op adapter"
        );
        let rejected = modify_rejections(&outbox);
        assert_eq!(rejected.len(), 1, "{venue}: exactly one rejection, got {rejected:?}");
        assert_eq!(rejected[0].0, "c1");
        assert!(
            rejected[0].1.contains(MODIFY_UNSUPPORTED_REASON) && rejected[0].1.starts_with(venue),
            "{venue}: the reason must name the venue and the cause, got {:?}",
            rejected[0].1
        );
    }
    assert!(covered > 0, "the roster has no Unsupported venue: this test proved nothing");
}

/// The rejection is NON-TERMINAL: folded back through the bus, the order still rests and can still
/// be cancelled.
#[test]
fn the_refusal_leaves_the_order_live() {
    let mut e = engine_with_resting_order("ig", "c1");
    let mut outbox = Outbox::default();
    e.modify_order("c1", Some(5.0), None, 2, &mut outbox);
    let mut bus = EventBus::new();
    for ev in outbox.0 {
        bus.publish(ev, &mut e);
    }
    assert!(
        e.registry.get("c1").is_some_and(|o| o.status.is_modifiable()),
        "a refused modify must leave the order resting and modifiable"
    );
}

/// The COMPLEMENT: a venue with a real amend still forwards it (the refusal is not a blanket).
#[test]
fn a_venue_with_an_amend_still_forwards_the_modify() {
    for venue in ["binance", "hyperliquid", "ctrader"] {
        let mut e = engine_with_resting_order(venue, "c1");
        let mut outbox = Outbox::default();
        e.modify_order("c1", None, Some(0.95), 2, &mut outbox);
        assert_eq!(e.client.modifies.len(), 1, "{venue}: the modify must reach the client");
        assert!(modify_rejections(&outbox).is_empty(), "{venue}: nothing to refuse");
    }
}

/// A venue string the table does not know (`sim`, a paper id) answers `Unknown`, which is NOT a
/// refusal: only a declared `Unsupported` is.
#[test]
fn an_unrecognised_venue_string_is_not_refused() {
    let mut e = engine_with_resting_order("sim", "c1");
    let mut outbox = Outbox::default();
    e.modify_order("c1", None, Some(0.95), 2, &mut outbox);
    assert_eq!(e.client.modifies.len(), 1);
    assert!(modify_rejections(&outbox).is_empty());
}

/// ⚠ A PAPER MOUNT carries the REAL venue string (`vike_tradehub_core::make_engine`), so the engine
/// is labelled `oanda`-or-`ig` while the client is the paper exchange, which amends. The CLIENT's
/// declared convention answers first, so the paper mount is not refused for its label.
#[test]
fn a_client_that_declares_an_amend_is_not_refused_for_its_venue_label() {
    let mut e = engine_with_resting_order("ig", "c1");
    e.client.declared_amend_semantics = Some(AmendSemantics::InPlaceRemaining);
    let mut outbox = Outbox::default();
    e.modify_order("c1", None, Some(0.95), 2, &mut outbox);
    assert_eq!(e.client.modifies.len(), 1, "the paper exchange amends whatever it is labelled");
    assert!(modify_rejections(&outbox).is_empty());
}
