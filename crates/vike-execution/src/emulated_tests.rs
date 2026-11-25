use super::*;
use vike_model::OrderEventKind;
use vike_model::strategy::OrderBrokerMock;
use vike_venues::OrderEmulationKind;

fn fill(size: f64) -> Fill {
    Fill {
        side: 1,
        size,
        price: 100.0,
        fee: 0.0,
        timestamp: 0,
        is_maker: true,
        symbol: String::new(),
        tag: None,
    }
}

/// The children a law placed through `OrderBrokerMock`'s seam, as `(tag, side, qty, price)`: each a TAGGED
/// plain limit on the mount's own symbol (`""`).
pub(crate) fn tagged_limits(b: &OrderBrokerMock) -> Vec<(String, i32, f64, f64)> {
    let own = b.placed.iter().filter(|(symbol, tag, ..)| symbol.is_empty() && !tag.is_empty());
    own.filter_map(|(_, tag, side, qty, order, reduce_only)| match order {
        OrderPlacement::Limit { price } if !reduce_only => Some((tag.clone(), *side, *qty, *price)),
        _ => None,
    })
    .collect()
}

fn event(tag: Option<&str>, kind: OrderEventKind) -> OrderEvent {
    OrderEvent { order_client_id: "c1".into(), tag: tag.map(str::to_string), kind }
}

fn chaser() -> OrderEmulationChaseConfig {
    OrderEmulationChaseConfig {
        side: 1,
        qty: 2.0,
        anchor: OrderPegAnchor::Best,
        offset: 0.0,
        refresh_threshold: 0.0,
        max_chase: None,
        timeout_ms: None,
    }
}

/// The law's child tag IS the route's word, so the engine that stamps the word on its children's
/// events stamps the tag the law's slot keys on.
#[test]
fn each_laws_tag_is_its_routes_word() {
    assert_eq!(chase::TAG, OrderEmulationKind::Chase.as_word());
    assert_eq!(iceberg::TAG, OrderEmulationKind::Iceberg.as_word());
}

#[test]
fn the_anchor_prices_each_side_and_the_offset_sign_is_aggressive_or_passive() {
    let (bid, ask) = (100.0, 102.0);
    assert_eq!(OrderPegAnchor::Best.price(1, bid, ask, 0.5), Some(100.5));
    assert_eq!(OrderPegAnchor::Best.price(-1, bid, ask, 0.5), Some(101.5));
    assert_eq!(OrderPegAnchor::Best.price(1, bid, ask, -0.5), Some(99.5), "negative is passive");
    assert_eq!(OrderPegAnchor::Mid.price(1, bid, ask, 0.0), Some(101.0));
    assert_eq!(OrderPegAnchor::Mid.price(-1, bid, ask, 0.25), Some(100.75));
    assert_eq!(OrderPegAnchor::Best.price(1, 0.0, ask, 0.0), None, "no bid side");
    assert_eq!(OrderPegAnchor::Mid.price(1, 103.0, ask, 0.0), None, "crossed book");
    assert_eq!(OrderPegAnchor::Best.price(1, bid, f64::NAN, 0.0), None);
    assert_eq!(OrderPegAnchor::Best.price(1, bid, ask, -100.0), None, "a price at or below zero");
    assert_eq!(OrderPegAnchor::parse("best"), Some(OrderPegAnchor::Best));
    assert_eq!(OrderPegAnchor::parse("mid"), Some(OrderPegAnchor::Mid));
    assert_eq!(OrderPegAnchor::parse("Mid"), None);
}

#[test]
fn the_configs_refuse_terms_that_cannot_run_in_words() {
    assert_eq!(chaser().validate(), Ok(()));
    let bad = [
        OrderEmulationChaseConfig { side: 0, ..chaser() },
        OrderEmulationChaseConfig { qty: 0.0, ..chaser() },
        OrderEmulationChaseConfig { offset: f64::INFINITY, ..chaser() },
        OrderEmulationChaseConfig { refresh_threshold: -1.0, ..chaser() },
        OrderEmulationChaseConfig { max_chase: Some(0.0), ..chaser() },
        OrderEmulationChaseConfig { timeout_ms: Some(0), ..chaser() },
    ];
    for c in bad {
        assert!(c.validate().is_err(), "{c:?} must be refused");
    }
    let ice = OrderEmulationIcebergConfig { side: -1, total: 10.0, visible: 2.0, price: 50.0 };
    assert_eq!(ice.validate(), Ok(()));
    for c in [
        OrderEmulationIcebergConfig { visible: 11.0, ..ice },
        OrderEmulationIcebergConfig { visible: 0.0, ..ice },
        OrderEmulationIcebergConfig { price: 0.0, ..ice },
        OrderEmulationIcebergConfig { total: f64::NAN, ..ice },
    ] {
        assert!(c.validate().is_err(), "{c:?} must be refused");
    }
}

#[test]
fn a_slot_places_once_reprices_in_place_without_resizing_and_cancels_by_its_tag() {
    let mut b = OrderBrokerMock::default();
    let mut slot = OrderEmulationChildSlot::default();
    assert!(slot.place(&mut b, "chase", 1, 2.0, 100.0));
    assert!(!slot.place(&mut b, "chase", 1, 2.0, 101.0), "a live tag is never submitted again");
    assert_eq!(tagged_limits(&b), vec![("chase".to_string(), 1, 2.0, 100.0)]);
    slot.reprice(&mut b, "chase", 100.0);
    assert!(b.modified.is_empty(), "an unchanged price sends nothing");
    slot.reprice(&mut b, "chase", 100.5);
    assert_eq!(b.modified, vec![("chase".to_string(), None, Some(100.5))]);
    slot.cancel(&mut b, "chase");
    assert_eq!(b.cancelled, vec!["chase".to_string()]);
    assert_eq!(slot, OrderEmulationChildSlot::default());
    slot.reprice(&mut b, "chase", 99.0);
    assert_eq!(b.modified.len(), 1, "a free slot re-prices nothing");
}

#[test]
fn a_slot_frees_when_its_size_has_filled_or_its_order_ends() {
    let mut b = OrderBrokerMock::default();
    let mut slot = OrderEmulationChildSlot::default();
    slot.place(&mut b, "slice", 1, 1.0, 100.0);
    slot.on_fill(&fill(0.7));
    slot.on_fill(&fill(0.2));
    assert!(slot.live);
    slot.on_fill(&fill(0.1));
    assert!(!slot.live, "0.7 + 0.2 + 0.1 fills a 1.0 child though the float sum is below 1.0");
    slot.place(&mut b, "slice", 1, 1.0, 100.0);
    let canceled = event(Some("slice"), OrderEventKind::Canceled { reason: String::new() });
    assert!(slot.on_order_event("slice", &canceled));
    assert!(!slot.live);
}

#[test]
fn a_tagged_child_the_saved_state_predates_is_adopted_and_then_repriced_not_resubmitted() {
    let mut b = OrderBrokerMock::default();
    let mut slot = OrderEmulationChildSlot::default();
    assert!(!slot.on_order_event("chase", &event(Some("bid"), OrderEventKind::Accepted)));
    assert!(!slot.on_order_event("chase", &event(None, OrderEventKind::Accepted)));
    assert!(!slot.live, "a foreign or untagged order is not ours");
    assert!(slot.on_order_event("chase", &event(Some("chase"), OrderEventKind::Accepted)));
    assert_eq!((slot.live, slot.price, slot.qty), (true, None, None), "adopted, terms unknown");
    slot.on_fill(&fill(5.0));
    assert!(slot.live, "an adopted child of unknown size is freed only by its terminal event");
    slot.reprice(&mut b, "chase", 100.0);
    assert!(tagged_limits(&b).is_empty());
    assert_eq!(b.modified, vec![("chase".to_string(), None, Some(100.0))]);
    let saved = serde_json::to_value(&slot).unwrap();
    assert_eq!(
        serde_json::from_value::<OrderEmulationChildSlot>(saved).unwrap(),
        slot,
        "save_state carries the slot, its child's id included"
    );
    assert!(slot.on_order_event("chase", &event(Some("chase"), OrderEventKind::Filled)));
    assert!(!slot.live);
}

/// P1: a new child refused because the tag is BUSY (an older order still works under it) is no end of
/// the slot: the order behind the tag is its child, ADOPTED with its terms and id unknown, so the
/// executor re-prices it and places nothing (freed, the slot would place again and be refused on
/// every hook). Not the slot's event, so no executor stops on it.
#[test]
fn a_busy_refusal_adopts_the_child_behind_the_tag_and_it_is_repriced_not_placed_again() {
    let mut b = OrderBrokerMock::default();
    let mut slot = OrderEmulationChildSlot::default();
    assert!(slot.place(&mut b, "chase", 1, 2.0, 100.0));
    let busy = OrderEventKind::Denied { reason: vike_model::tag_busy_reason("chase") };
    assert!(!slot.on_order_event("chase", &event(Some("chase"), busy)), "no executor stops on it");
    assert_eq!((slot.live, slot.price, slot.qty), (true, None, None), "adopted, terms unknown");
    assert!(!slot.place(&mut b, "chase", 1, 2.0, 100.5), "nothing is placed again");
    slot.reprice(&mut b, "chase", 100.5);
    assert_eq!(b.modified, vec![("chase".to_string(), None, Some(100.5))], "it is re-priced");
    assert_eq!(tagged_limits(&b).len(), 1, "the one refused place only");
    // the adopted child's own event teaches its id; its terminal frees the slot
    assert!(slot.on_order_event("chase", &event(Some("chase"), OrderEventKind::Filled)));
    assert!(!slot.live);
}

/// Every child goes out under the one tag, so the slot matches an event by its ORDER ID: a late
/// event of the previous child, or of any child but the one it learned, neither frees nor ends it.
#[test]
fn a_late_event_of_an_older_child_neither_frees_nor_ends_the_current_one() {
    let event =
        |id: &str, kind| OrderEvent { order_client_id: id.into(), tag: Some("slice".into()), kind };
    let mut b = OrderBrokerMock::default();
    let mut slot = OrderEmulationChildSlot::default();
    slot.place(&mut b, "slice", 1, 1.0, 100.0);
    assert!(slot.on_order_event("slice", &event("c1", OrderEventKind::Accepted)), "learns c1");
    slot.on_fill(&fill(1.0));
    assert!(slot.live, "a child named by id is freed by its own terminal, not by a fill's size");
    assert!(slot.on_order_event("slice", &event("c1", OrderEventKind::Filled)));
    slot.place(&mut b, "slice", 1, 1.0, 100.0);
    for late in [OrderEventKind::Filled, OrderEventKind::Canceled { reason: String::new() }] {
        assert!(
            !slot.on_order_event("slice", &event("c1", late)),
            "the previous child's straggler"
        );
    }
    assert!(slot.live, "child 2 still rests");
    assert!(slot.on_order_event("slice", &event("c2", OrderEventKind::Accepted)), "learns c2");
    assert!(
        !slot
            .on_order_event("slice", &event("c9", OrderEventKind::Rejected { reason: "x".into() }))
    );
    assert!(slot.live);
    assert!(slot.on_order_event("slice", &event("c2", OrderEventKind::Filled)));
    assert!(!slot.live);
}

/// A slot restored from saved state FREE: before the core names the tag, a market hook places
/// nothing, because a child placed after the last save may be resting; the tagged event adopts it.
#[test]
fn a_restored_slot_places_nothing_until_the_core_names_its_tag() {
    let mut b = OrderBrokerMock { now: 1_000, ..Default::default() };
    let mut slot = OrderEmulationChildSlot::default();
    slot.restored();
    assert!(!slot.confirmed(&mut b, "chase", ADOPTION_DEADLINE_MS), "a quote before the event");
    assert!(!slot.place(&mut b, "chase", 1, 2.0, 100.0));
    assert!(
        tagged_limits(&b).is_empty(),
        "nothing goes out beside a child the saved state predates"
    );
    assert!(!serde_json::to_value(&slot).unwrap().to_string().contains("await"), "never saved");
    assert!(slot.on_order_event("chase", &event(Some("chase"), OrderEventKind::Accepted)));
    assert!(slot.live, "adopted");
    assert!(slot.confirmed(&mut b, "chase", ADOPTION_DEADLINE_MS), "the core named it");
    assert!(b.cancelled.is_empty());

    // Restored LIVE and then named by the core: the child stays, at its saved price.
    let mut slot = OrderEmulationChildSlot {
        live: true,
        price: Some(100.0),
        qty: Some(2.0),
        ..Default::default()
    };
    slot.restored();
    slot.reprice(&mut b, "chase", 100.5);
    assert_eq!(b.modified.len(), 1, "a restored live child may still be re-priced");
    assert!(slot.on_order_event("chase", &event(Some("chase"), OrderEventKind::Accepted)));
    assert!(slot.confirmed(&mut b, "chase", ADOPTION_DEADLINE_MS));
    assert_eq!((slot.live, slot.price), (true, Some(100.5)));
}

/// No event within the deadline: the restored child is taken as gone, pulled by its tag first (a
/// no-op for a tag the core no longer knows) so the next child can never stand beside it.
#[test]
fn the_adoption_deadline_releases_a_restored_slot_after_pulling_its_tag() {
    let mut b = OrderBrokerMock { now: 1_000, ..Default::default() };
    let mut slot = OrderEmulationChildSlot {
        live: true,
        price: Some(100.0),
        qty: Some(2.0),
        ..Default::default()
    };
    slot.restored();
    assert!(!slot.confirmed(&mut b, "chase", 5_000), "the clock starts at the first hook");
    b.now = 5_999;
    assert!(!slot.confirmed(&mut b, "chase", 5_000));
    b.now = 6_000;
    assert!(!slot.confirmed(&mut b, "chase", 5_000), "the deadline: pulled, nothing placed yet");
    assert_eq!(b.cancelled, vec!["chase".to_string()]);
    assert!(!slot.live && slot.price.is_none());
    // The live core drains a hook's submits before its cancels: the child goes out on the next hook.
    assert!(slot.confirmed(&mut b, "chase", 5_000));
    assert!(slot.place(&mut b, "chase", 1, 2.0, 100.0));
    assert_eq!(tagged_limits(&b).len(), 1);

    // Cancelling while it awaits pulls what the tag may still name, live or not.
    let mut slot = OrderEmulationChildSlot::default();
    slot.restored();
    slot.cancel(&mut b, "chase");
    assert_eq!(b.cancelled.len(), 2);
}

/// A LATE first pass, after the deadline gave the restored child up. The core refuses the next child
/// while the restored order still holds the tag: that refusal is not the slot's (no executor stops on
/// it) and the slot awaits the child again, which the pass's adoption then hands back. Without the
/// refusal (the pass came before the next hook), the given-up child's own event takes it back too,
/// while its straggler is still refused once a new child rests.
#[test]
fn a_late_pass_hands_back_the_restored_child_the_deadline_gave_up() {
    let event =
        |id: &str, kind| OrderEvent { order_client_id: id.into(), tag: Some("slice".into()), kind };
    let denied = || OrderEventKind::Denied { reason: RESTORED_TAG_DENY_REASON.into() };
    let given_up = |b: &mut OrderBrokerMock| {
        let mut slot = OrderEmulationChildSlot::default();
        slot.on_order_event("slice", &event("c1", OrderEventKind::Accepted));
        slot.restored();
        assert!(!slot.confirmed(b, "slice", 5_000));
        b.now += 5_000;
        assert!(!slot.confirmed(b, "slice", 5_000), "the deadline gives c1 up");
        slot
    };

    let mut b = OrderBrokerMock { now: 1_000, ..Default::default() };
    let mut slot = given_up(&mut b);
    assert!(slot.confirmed(&mut b, "slice", 5_000) && slot.place(&mut b, "slice", 1, 1.0, 100.0));
    assert!(!slot.on_order_event("slice", &event("refused-1", denied())), "not the slot's refusal");
    assert!(!slot.live && !slot.confirmed(&mut b, "slice", 5_000), "it awaits c1 again");
    assert!(
        slot.on_order_event("slice", &event("c1", OrderEventKind::Accepted)),
        "the pass adopts c1"
    );
    assert!(slot.live && slot.confirmed(&mut b, "slice", 5_000));
    assert!(!slot.place(&mut b, "slice", 1, 1.0, 100.0), "no twin beside it");
    assert!(
        slot.on_order_event("slice", &event("c1", OrderEventKind::Filled)),
        "its death is its own"
    );
    assert!(!slot.live && slot.place(&mut b, "slice", 1, 1.0, 100.0), "the next child goes out");

    let mut b = OrderBrokerMock { now: 1_000, ..Default::default() };
    let mut slot = given_up(&mut b);
    assert!(slot.on_order_event("slice", &event("c1", OrderEventKind::Accepted)), "taken back");
    assert!(slot.live);
    assert!(slot.on_order_event("slice", &event("c1", OrderEventKind::Filled)) && !slot.live);
    let mut slot = given_up(&mut b);
    assert!(slot.confirmed(&mut b, "slice", 5_000) && slot.place(&mut b, "slice", 1, 1.0, 100.0));
    assert!(
        !slot.on_order_event("slice", &event("c1", OrderEventKind::Accepted)),
        "c2 rests: stale"
    );
    let other = OrderEventKind::Denied { reason: "risk: too big".into() };
    assert!(slot.on_order_event("slice", &event("c2", other)), "any other refusal is the slot's");
}

/// **A state that does not ENCODE is saved as one that does not read, never as none** (decision 0275 item
/// 7's amendment): the journal's document is [`UNSAVED_STATE`], not the `null` a fresh parent restores
/// from, so after a restart each law ends its parent (and pulls its child on the next hook) instead of
/// working the whole size again beside the child still resting.
#[test]
fn a_state_that_does_not_encode_is_saved_as_one_that_ends_the_parent_at_the_restart() {
    struct Unencodable;
    impl Serialize for Unencodable {
        fn serialize<S: serde::Serializer>(&self, _: S) -> Result<S::Ok, S::Error> {
            Err(serde::ser::Error::custom("refused"))
        }
    }
    let document = saved_state(&Unencodable, chase::TAG, "parent-1")
        .to_json_value()
        .expect("the journal keeps it as a JSON document");
    assert_eq!(document, serde_json::Value::from(UNSAVED_STATE), "never the journal's `null`");

    let request = OrderRequest {
        order_client_id: "parent-1".into(),
        side: 1,
        qty: 2.0,
        price: Some(100.0),
        display_quantity: Some(1.0),
        ..Default::default()
    };
    for route in [OrderEmulationKind::Chase, OrderEmulationKind::Iceberg] {
        let mut law = OrderEmulationLaw::of(&request, route, i64::MAX);
        assert_eq!(law.refusal(), None, "{route:?} works the parent");
        law.hooks::<OrderBrokerMock>().load_state(&StrategyState::from_json_value(&document));
        assert!(law.refusal().is_some(), "{route:?}: the restore ends the parent");
    }
}
