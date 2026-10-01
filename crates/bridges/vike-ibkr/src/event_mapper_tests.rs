use super::*;
use crate::id_registry::IdRegistry;
use vike_model::events::Event;

fn mapper_with(coid: &str, order_id: i32) -> EventMapper {
    let mut ids = IdRegistry::default();
    ids.on_next_valid_id(order_id);
    let alloc = ids.next_order_id();
    assert_eq!(alloc, order_id);
    ids.bind(order_id, coid);
    EventMapper::new(ids)
}

/// An execDetails with an EMPTY `execId` is refused at ingress: no events, and nothing buffered,
/// so its commissionReport cannot later join and mint a fill either.
///
/// This gates the `TradeId::new` guard in `on_exec_details`, not the type. Reverting it to a
/// permissive id turns both asserted emptinesses into fills — and the hazard is worse than one
/// un-dedupable fill: `emitted`/`pending`/`commissions` are ALL keyed on the exec id, so several
/// id-less executions would share one `""` slot, overwriting each other's halves and making a
/// `reqExecutions` replay (which returns the whole day) re-emit or swallow at random.
#[test]
fn exec_details_without_an_exec_id_is_refused_at_ingress() {
    let mut m = mapper_with("coid-A", 101);
    let evs = m.on_exec_details(IbExecDetails {
        order_id: 101,
        order_ref: "coid-A".into(),
        exec_id: String::new(),
        symbol: "AAPL.SMART.USD".into(),
        side_buy: true,
        shares: 10.0,
        price: 190.0,
        ts: 5,
    });
    assert!(evs.is_empty(), "an id-less execution emits nothing, not even the synth accept");

    // ...and nothing was buffered, so the matching commission cannot mint a fill afterwards.
    let joined = m.on_commission_report(IbCommissionReport {
        exec_id: String::new(),
        commission: 1.0,
        currency: "USD".into(),
    });
    assert!(joined.is_empty(), "no pending half exists to join: {joined:?}");
}

#[test]
fn fill_before_accept_synthesizes_accepted_then_filled() {
    // A fresh mapper whose id is bound but never saw an ACCEPTED status: an execDetails must
    // synthesize OrderAccepted, then (after the commission joins) OrderFilled.
    let mut m = mapper_with("coid-A", 101);
    let evs = m.on_exec_details(IbExecDetails {
        order_id: 101,
        order_ref: "coid-A".into(),
        exec_id: "e1".into(),
        symbol: "AAPL.SMART.USD".into(),
        side_buy: true,
        shares: 10.0,
        price: 190.0,
        ts: 5,
    });
    // Accepted is emitted immediately; the fill waits for the commission join.
    assert!(matches!(evs.as_slice(), [Event::OrderAccepted(_)]));
    let evs2 = m.on_commission_report(IbCommissionReport {
        exec_id: "e1".into(),
        commission: 1.0,
        currency: "USD".into(),
    });
    match evs2.as_slice() {
        [Event::OrderFilled(f)] => {
            assert_eq!(f.client_order_id, "coid-A");
            assert_eq!(f.fill.last_qty, 10.0);
            assert_eq!(f.fill.commission, 1.0);
            assert_eq!(f.fill.trade_id, "e1");
        }
        other => panic!("expected OrderFilled, got {other:?}"),
    }
}

#[test]
fn exec_details_buffered_until_commission_joins_by_exec_id() {
    let mut m = mapper_with("coid-A", 101);
    // Pre-accept via a Submitted status so no synth is needed.
    let _ = m.on_order_status(IbOrderStatus {
        order_id: 101,
        order_ref: "coid-A".into(),
        status: "Submitted".into(),
        filled: 0.0,
        avg_fill_price: 0.0,
    });
    let e = m.on_exec_details(IbExecDetails {
        order_id: 101,
        order_ref: "coid-A".into(),
        exec_id: "e9".into(),
        symbol: "AAPL.SMART.USD".into(),
        side_buy: false,
        shares: 4.0,
        price: 191.0,
        ts: 7,
    });
    assert!(e.is_empty(), "fill must NOT emit before its commission joins");
    let joined = m.on_commission_report(IbCommissionReport {
        exec_id: "e9".into(),
        commission: 0.5,
        currency: "USD".into(),
    });
    assert!(matches!(joined.as_slice(), [Event::OrderFilled(_)]));
}

#[test]
fn advisory_code_emits_nothing() {
    let mut m = mapper_with("coid-A", 101);
    assert!(m.on_error(2104, 101, "Market data farm connection is OK").is_empty());
    assert!(m.on_error(2137, 101, "advisory").is_empty());
}

#[test]
fn order_rejection_code_emits_reject_and_forgets() {
    let mut m = mapper_with("coid-A", 101);
    let evs = m.on_error(201, 101, "Order rejected - reason: insufficient buying power");
    assert!(matches!(evs.as_slice(), [Event::OrderRejected(_)]));
}

#[test]
fn idless_async_rejection_routes_to_sole_unacked_order() {
    // ibapi's global order_update_stream drops the order id from order-rejection Notices, so an
    // async hard rejection (delivered AFTER submit_order returned Ok) arrives as
    // on_error(code, 0, msg). With exactly one order still unacked, it must resolve to that
    // order and emit OrderRejected — the no-order-vanishes contract.
    let mut m = mapper_with("coid-A", 101);
    m.on_submit(101, 1.0);
    let evs = m.on_error(201, 0, "Order rejected - insufficient buying power");
    match evs.as_slice() {
        [Event::OrderRejected(r)] => assert_eq!(r.client_order_id, "coid-A"),
        other => panic!("expected OrderRejected, got {other:?}"),
    }
    // Idempotent: after the reject the order is forgotten (zero unacked) → a duplicate id-less
    // notice synthesizes no second terminal.
    assert!(m.on_error(201, 0, "duplicate").is_empty());
}

#[test]
fn idless_async_rejection_ambiguous_when_multiple_unacked() {
    // Two orders in flight: an id-less rejection cannot be safely attributed, so NO terminal is
    // synthesized here (the core submit-ack watchdog is the backstop). Never corrupt a sibling.
    let mut ids = IdRegistry::default();
    ids.on_next_valid_id(101);
    let a = ids.next_order_id();
    ids.bind(a, "coid-A");
    let b = ids.next_order_id();
    ids.bind(b, "coid-B");
    let mut m = EventMapper::new(ids);
    m.on_submit(a, 1.0);
    m.on_submit(b, 1.0);
    assert!(m.on_error(201, 0, "ambiguous").is_empty());
}

#[test]
fn idless_async_rejection_ignored_after_order_acked() {
    // Once the sole in-flight order goes active it leaves the unacked set, so a stray id-less
    // rejection no longer misattributes to it (an accepted order rejects via a resolvable
    // OrderStatus, not this path).
    let mut m = mapper_with("coid-A", 101);
    m.on_submit(101, 1.0);
    let _ = m.on_order_status(IbOrderStatus {
        order_id: 101,
        order_ref: "coid-A".into(),
        status: "Submitted".into(),
        filled: 0.0,
        avg_fill_price: 0.0,
    });
    assert!(m.on_error(201, 0, "late").is_empty());
}

#[test]
fn rebind_of_unacked_order_emits_accepted_once() {
    // Mid-flight order: core submitted it (unacked), IB accepted server-side, but the confirming
    // OrderStatus was lost in a socket blip. On reconnect IB replays it as an OpenOrder →
    // rebind_open_order MUST emit exactly one OrderAccepted to unstick the FSM, and a later
    // Submitted status must NOT re-emit a second (idempotent).
    let mut m = mapper_with("coid-A", 101);
    m.on_submit(101, 1.0);
    let evs = m.rebind_open_order(101, "coid-A");
    match evs.as_slice() {
        [Event::OrderAccepted(a)] => {
            assert_eq!(a.client_order_id, "coid-A");
            assert_eq!(a.venue_order_id.as_deref(), Some("101"));
        }
        other => panic!("expected exactly one OrderAccepted, got {other:?}"),
    }
    // A subsequent active status for the same order does NOT emit a duplicate accept.
    let dup = m.on_order_status(IbOrderStatus {
        order_id: 101,
        order_ref: "coid-A".into(),
        status: "Submitted".into(),
        filled: 0.0,
        avg_fill_price: 0.0,
    });
    assert!(dup.is_empty(), "second accept must not be emitted (idempotent)");
}

#[test]
fn rebind_of_external_order_emits_nothing_and_stays_resolvable() {
    // An EXTERNAL order (never submitted this process → not in unacked, e.g. opened in a prior
    // session): rebind emits NOTHING, but the id map is seeded so a later Cancelled still
    // resolves to its coid and emits exactly one OrderCanceled.
    let mut ids = IdRegistry::default();
    ids.on_next_valid_id(55);
    let mut m = EventMapper::new(ids);
    let evs = m.rebind_open_order(55, "coid-ext");
    assert!(evs.is_empty(), "external rebind must emit nothing");
    let cancel = m.on_order_status(IbOrderStatus {
        order_id: 55,
        order_ref: "coid-ext".into(),
        status: "Cancelled".into(),
        filled: 0.0,
        avg_fill_price: 0.0,
    });
    match cancel.as_slice() {
        [Event::OrderCanceled(c)] => assert_eq!(c.client_order_id, "coid-ext"),
        other => panic!("expected exactly one OrderCanceled, got {other:?}"),
    }
}

#[test]
fn terminal_status_cleans_the_map() {
    let mut m = mapper_with("coid-A", 101);
    let evs = m.on_order_status(IbOrderStatus {
        order_id: 101,
        order_ref: "coid-A".into(),
        status: "Cancelled".into(),
        filled: 0.0,
        avg_fill_price: 0.0,
    });
    assert!(matches!(evs.as_slice(), [Event::OrderCanceled(_)]));
    // After a terminal, the id is forgotten → a late duplicate resolves to nothing.
    let dup = m.on_order_status(IbOrderStatus {
        order_id: 101,
        order_ref: "".into(),
        status: "Cancelled".into(),
        filled: 0.0,
        avg_fill_price: 0.0,
    });
    assert!(dup.is_empty());
}

fn mapper_submitted(coid: &str, order_id: i32, qty: f64) -> EventMapper {
    let mut m = mapper_with(coid, order_id);
    m.on_submit(order_id, qty);
    // ack it so fills don't need exec-before-open synthesis noise in these tests
    let _ = m.on_order_status(IbOrderStatus {
        order_id,
        order_ref: coid.into(),
        status: "Submitted".into(),
        filled: 0.0,
        avg_fill_price: 0.0,
    });
    m
}

fn commission(m: &mut EventMapper, exec_id: &str) -> Vec<Event> {
    m.on_commission_report(IbCommissionReport {
        exec_id: exec_id.into(),
        commission: 0.0,
        currency: "USD".into(),
    })
}
fn exec(m: &mut EventMapper, order_id: i32, coid: &str, exec_id: &str, shares: f64) -> Vec<Event> {
    m.on_exec_details(IbExecDetails {
        order_id,
        order_ref: coid.into(),
        exec_id: exec_id.into(),
        symbol: "AAPL.SMART.USD".into(),
        side_buy: true,
        shares,
        price: 190.0,
        ts: 1,
    })
}

#[test]
fn multi_fill_emits_partial_then_final_and_forgets() {
    let mut m = mapper_submitted("coid-A", 101, 10.0);
    let _ = exec(&mut m, 101, "coid-A", "e1", 4.0);
    assert!(matches!(commission(&mut m, "e1").as_slice(), [Event::OrderPartiallyFilled(_)]));
    let _ = exec(&mut m, 101, "coid-A", "e2", 6.0);
    assert!(matches!(commission(&mut m, "e2").as_slice(), [Event::OrderFilled(_)]));
    // forgotten: a stray later exec for order 101 resolves to nothing
    assert!(exec(&mut m, 101, "", "e3", 1.0).is_empty());
}

#[test]
fn single_full_fill_is_terminal_and_forgets() {
    let mut m = mapper_submitted("coid-A", 101, 5.0);
    let _ = exec(&mut m, 101, "coid-A", "e1", 5.0);
    assert!(matches!(commission(&mut m, "e1").as_slice(), [Event::OrderFilled(_)]));
    assert!(
        m.on_order_status(IbOrderStatus {
            order_id: 101,
            order_ref: "".into(),
            status: "Filled".into(),
            filled: 5.0,
            avg_fill_price: 190.0,
        })
        .is_empty()
    );
}

#[test]
fn unknown_total_fill_falls_back_to_terminal_filled() {
    // No on_submit(total) → external/unknown → keep current OrderFilled-terminal behaviour.
    // (order_id 101, not the plan text's illustrative 55: mapper_with asserts the allocated id
    // round-trips, and IdRegistry::next_order_id floors every allocation at 101.)
    let mut m = mapper_with("coid-X", 101);
    let _ = m.on_order_status(IbOrderStatus {
        order_id: 101,
        order_ref: "coid-X".into(),
        status: "Submitted".into(),
        filled: 0.0,
        avg_fill_price: 0.0,
    });
    let _ = exec(&mut m, 101, "coid-X", "e1", 3.0);
    assert!(matches!(commission(&mut m, "e1").as_slice(), [Event::OrderFilled(_)]));
}

#[test]
fn partial_then_cancel_is_valid_and_forgets() {
    let mut m = mapper_submitted("coid-A", 101, 10.0);
    let _ = exec(&mut m, 101, "coid-A", "e1", 4.0);
    assert!(matches!(commission(&mut m, "e1").as_slice(), [Event::OrderPartiallyFilled(_)]));
    let c = m.on_order_status(IbOrderStatus {
        order_id: 101,
        order_ref: "coid-A".into(),
        status: "Cancelled".into(),
        filled: 4.0,
        avg_fill_price: 190.0,
    });
    assert!(matches!(c.as_slice(), [Event::OrderCanceled(_)]));
    // forgotten
    assert!(
        m.on_order_status(IbOrderStatus {
            order_id: 101,
            order_ref: "".into(),
            status: "Cancelled".into(),
            filled: 4.0,
            avg_fill_price: 0.0,
        })
        .is_empty()
    );
}

#[test]
fn cancel_with_commission_in_flight_flushes_partial_then_cancels_and_dedups() {
    let mut m = mapper_submitted("coid-A", 101, 10.0);
    let _ = exec(&mut m, 101, "coid-A", "e1", 4.0); // execDetails, NO commission yet
    let out = m.on_order_status(IbOrderStatus {
        order_id: 101,
        order_ref: "coid-A".into(),
        status: "Cancelled".into(),
        filled: 4.0,
        avg_fill_price: 190.0,
    });
    // flush emits the partial (commission 0) BEFORE the cancel
    match out.as_slice() {
        [Event::OrderPartiallyFilled(p), Event::OrderCanceled(c)] => {
            assert_eq!(p.client_order_id, "coid-A");
            assert_eq!(p.fill.commission, 0.0);
            assert_eq!(c.client_order_id, "coid-A");
        }
        other => panic!("expected [PartiallyFilled, Canceled], got {other:?}"),
    }
    // the late real commission for e1 is a no-op (its fill already emitted → no double emit)
    assert!(commission(&mut m, "e1").is_empty());
    // …and it is NOT parked either: `emitted` is checked before the park, so a dedup'd
    // commission cannot linger in the reverse index.
    assert!(m.commissions.is_empty());
}

// -----------------------------------------------------------------------------------------
// Commission-before-exec: the arrival order IBKR explicitly permits
// (`crates/bridges/vike-ibkr/vendor/ibapi/src/orders/mod.rs`'s `CommissionReport`). Before the
// reverse index existed, the commission was discarded on arrival AND its execution then
// stranded in `pending` forever, so the fill was never emitted at all.
// -----------------------------------------------------------------------------------------

#[test]
fn commission_before_exec_emits_exactly_one_fill_instead_of_dropping_it() {
    let mut m = mapper_submitted("coid-A", 101, 10.0);
    let parked = m.on_commission_report(IbCommissionReport {
        exec_id: "e1".into(),
        commission: 1.25,
        currency: "USD".into(),
    });
    assert!(parked.is_empty(), "the commission alone emits nothing — it is parked, not dropped");
    let evs = m.on_exec_details(IbExecDetails {
        order_id: 101,
        order_ref: "coid-A".into(),
        exec_id: "e1".into(),
        symbol: "AAPL.SMART.USD".into(),
        side_buy: true,
        shares: 10.0,
        price: 190.0,
        ts: 5,
    });
    match evs.as_slice() {
        [Event::OrderFilled(f)] => {
            assert_eq!(f.client_order_id, "coid-A");
            assert_eq!(f.fill.trade_id, "e1");
            assert_eq!(f.fill.last_qty, 10.0);
            assert_eq!(f.fill.last_px, 190.0);
            // the parked half's values are carried onto the fill, not defaulted away
            assert_eq!(f.fill.commission, 1.25);
            assert_eq!(f.fill.commission_asset, "USD");
            assert_eq!(f.fill.ts, 5);
        }
        other => panic!("expected exactly one OrderFilled, got {other:?}"),
    }
    assert!(m.commissions.is_empty(), "the park is consumed by the join");
    assert!(m.pending.is_empty(), "nothing strands in pending");
}

#[test]
fn both_arrival_orders_produce_an_identical_fill() {
    // The join is by exec_id, never by arrival, so the two orderings must be indistinguishable
    // downstream — the property the vendored ibapi docs prescribe.
    let fill_of = |commission_first: bool| -> FillEvent {
        let mut m = mapper_submitted("coid-A", 101, 3.0);
        let c =
            IbCommissionReport { exec_id: "e1".into(), commission: 0.75, currency: "USD".into() };
        let d = IbExecDetails {
            order_id: 101,
            order_ref: "coid-A".into(),
            exec_id: "e1".into(),
            symbol: "AAPL.SMART.USD".into(),
            side_buy: false,
            shares: 3.0,
            price: 188.5,
            ts: 42,
        };
        let evs = if commission_first {
            assert!(m.on_commission_report(c).is_empty());
            m.on_exec_details(d)
        } else {
            assert!(m.on_exec_details(d).is_empty());
            m.on_commission_report(c)
        };
        match evs.as_slice() {
            [Event::OrderFilled(f)] => f.fill.clone(),
            other => panic!("expected exactly one OrderFilled, got {other:?}"),
        }
    };
    assert_eq!(fill_of(true), fill_of(false));
}

#[test]
fn commission_before_exec_still_synthesizes_the_accept_first() {
    // Exec-before-open AND commission-before-exec at once: the accept must still lead, so the
    // FSM sees a valid [Accepted, Filled] pair from the single call.
    let mut m = mapper_with("coid-A", 101);
    assert!(commission(&mut m, "e1").is_empty());
    let evs = exec(&mut m, 101, "coid-A", "e1", 10.0);
    match evs.as_slice() {
        [Event::OrderAccepted(a), Event::OrderFilled(f)] => {
            assert_eq!(a.client_order_id, "coid-A");
            assert_eq!(f.client_order_id, "coid-A");
        }
        other => panic!("expected [OrderAccepted, OrderFilled], got {other:?}"),
    }
}

#[test]
fn commission_first_partials_account_identically_to_exec_first() {
    // Both legs reversed: `fill_state`'s partial-vs-final accounting must be unchanged, since
    // it advances in the shared `join_fill` rather than in either arrival path.
    let mut m = mapper_submitted("coid-A", 101, 10.0);
    assert!(commission(&mut m, "e1").is_empty());
    assert!(matches!(
        exec(&mut m, 101, "coid-A", "e1", 4.0).as_slice(),
        [Event::OrderPartiallyFilled(_)]
    ));
    assert!(commission(&mut m, "e2").is_empty());
    assert!(matches!(exec(&mut m, 101, "coid-A", "e2", 6.0).as_slice(), [Event::OrderFilled(_)]));
    // forgotten on the final fill, exactly as in the exec-first ordering
    assert!(exec(&mut m, 101, "", "e3", 1.0).is_empty());
}

#[test]
fn duplicate_commission_does_not_double_emit_a_fill() {
    let mut m = mapper_submitted("coid-A", 101, 5.0);
    let _ = exec(&mut m, 101, "coid-A", "e1", 5.0);
    assert!(matches!(commission(&mut m, "e1").as_slice(), [Event::OrderFilled(_)]));
    // IB re-sends the same commissionReport. The fill for e1 already emitted, so the duplicate
    // is DROPPED at the top of `on_commission_report` — emitting nothing and, unlike before the
    // `emitted` index existed, not PARKING either (a park here would leak one `commissions`
    // entry per re-delivered commission, which a `reqExecutions` replay produces by the dozen).
    assert!(commission(&mut m, "e1").is_empty());
    assert!(commission(&mut m, "e1").is_empty());
    assert!(m.commissions.is_empty(), "a commission for an emitted fill must not park");
}

#[test]
fn parked_commission_is_released_once_its_exec_proves_unroutable() {
    // The DEFINITIVE expiry: the execution arrives but resolves to no local order, so the pair
    // can never join and the park must not be held for the process lifetime.
    let mut m = mapper_submitted("coid-A", 101, 5.0);
    assert!(commission(&mut m, "ext-1").is_empty());
    assert_eq!(m.commissions.len(), 1);
    let evs = exec(&mut m, 999, "", "ext-1", 3.0); // order 999 was never bound
    assert!(evs.is_empty(), "an unroutable execution still emits nothing");
    assert!(m.commissions.is_empty(), "the park is released, not held forever");
    assert!(m.pending.is_empty());
}

#[test]
fn parked_commissions_are_capped_and_evict_the_oldest_first() {
    // The BACKSTOP expiry: the key is a venue-supplied exec_id, so the map is bounded.
    let mut m = mapper_submitted("coid-A", 101, 1.0);
    for i in 0..MAX_PARKED_COMMISSIONS {
        assert!(commission(&mut m, &format!("orphan-{i}")).is_empty());
    }
    assert_eq!(m.commissions.len(), MAX_PARKED_COMMISSIONS);
    // One more park evicts the OLDEST, never the newest (which is the one still mid-race).
    assert!(commission(&mut m, "newest").is_empty());
    assert_eq!(m.commissions.len(), MAX_PARKED_COMMISSIONS, "the cap holds");
    assert!(!m.commissions.contains_key("orphan-0"), "the oldest park is the one evicted");
    assert!(m.commissions.contains_key("orphan-1"));
    assert!(m.commissions.contains_key("newest"), "the newest park survives");
    // The surviving park still joins normally — the cap does not break the live pair.
    assert!(matches!(
        exec(&mut m, 101, "coid-A", "newest", 1.0).as_slice(),
        [Event::OrderFilled(_)]
    ));
}

// -----------------------------------------------------------------------------------------
// The MIRROR leak (#949's documented residual): an execution whose commissionReport never
// arrives, on an order that is never cancelled, was held forever and its fill NEVER emitted —
// position and realized PnL silently short. Cured by RECOVERING the real commission from the
// venue (`sweep_pending` → the exec loop's `reqExecutions`), never by inventing one.
// -----------------------------------------------------------------------------------------

#[test]
fn a_fresh_pending_entry_is_not_swept_before_the_grace_window() {
    let mut m = mapper_submitted("coid-A", 101, 10.0);
    let _ = exec(&mut m, 101, "coid-A", "e1", 10.0);
    // The first sweep only STAMPS the entry (the mapper has no clock, so it cannot have been
    // stamped at arrival) — it can never fire on the same pass that discovers it.
    assert!(m.sweep_pending(1_000, 15_000).is_empty());
    assert!(m.sweep_pending(1_000 + 14_999, 15_000).is_empty(), "one ms short of the window");
    assert_eq!(m.pending.len(), 1, "the sweep removes nothing");
}

#[test]
fn an_ordinary_pair_that_joins_promptly_is_never_swept() {
    let mut m = mapper_submitted("coid-A", 101, 10.0);
    let _ = exec(&mut m, 101, "coid-A", "e1", 10.0);
    assert!(matches!(commission(&mut m, "e1").as_slice(), [Event::OrderFilled(_)]));
    // Nothing buffered ⇒ no re-request is ever provoked on the happy path, at any age.
    assert!(m.sweep_pending(i64::MAX / 2, 15_000).is_empty());
}

#[test]
fn a_stranded_execution_is_reported_for_recovery_then_escalated_once() {
    let mut m = mapper_submitted("coid-A", 101, 10.0);
    let _ = exec(&mut m, 101, "coid-A", "e1", 10.0);
    let mut now = 0;
    m.sweep_pending(now, 15_000); // stamp
    // Each attempt restarts the entry's clock, so the ladder is one request per grace window.
    for attempt in 1..=MAX_PENDING_RECOVERY_ATTEMPTS {
        now += 15_000;
        let s = m.sweep_pending(now, 15_000);
        assert_eq!(s.recover, vec!["e1".to_string()], "attempt {attempt}");
        assert!(s.stranded.is_empty(), "attempt {attempt} must not escalate yet");
    }
    // Budget spent → ONE stranded row carrying everything an operator needs to see what the
    // platform is short.
    now += 15_000;
    let s = m.sweep_pending(now, 15_000);
    assert!(s.recover.is_empty(), "no fourth request");
    assert_eq!(
        s.stranded,
        vec![StrandedFill {
            exec_id: "e1".into(),
            client_order_id: "coid-A".into(),
            order_id: 101,
            symbol: "AAPL.SMART.USD".into(),
            side: 1,
            shares: 10.0,
            price: 190.0,
        }]
    );
    // …and never again: the escalation is reported once, not once per second forever.
    now += 15_000;
    assert!(m.sweep_pending(now, 15_000).is_empty());
}

#[test]
fn a_stranded_execution_is_never_evicted_so_a_late_commission_still_emits_its_fill() {
    // The #949 constraint this design is built around: evicting a `pending` entry would lose
    // THE FILL, so the sweep annotates and never removes. Even past escalation the entry is
    // still joinable — and it joins with the REAL commission, never a fabricated 0.0.
    let mut m = mapper_submitted("coid-A", 101, 10.0);
    let _ = exec(&mut m, 101, "coid-A", "e1", 10.0);
    let mut now = 0;
    for _ in 0..(MAX_PENDING_RECOVERY_ATTEMPTS + 3) {
        m.sweep_pending(now, 15_000);
        now += 15_000;
    }
    assert_eq!(m.pending.len(), 1, "escalation holds the entry, it does not drop it");
    match m
        .on_commission_report(IbCommissionReport {
            exec_id: "e1".into(),
            commission: 1.75,
            currency: "USD".into(),
        })
        .as_slice()
    {
        [Event::OrderFilled(f)] => {
            assert_eq!(f.fill.commission, 1.75, "the REAL commission, not a fabricated 0.0");
            assert_eq!(f.fill.last_qty, 10.0);
        }
        other => panic!("expected exactly one OrderFilled, got {other:?}"),
    }
    assert!(m.pending.is_empty());
}

#[test]
fn the_venue_replay_of_a_stranded_pair_emits_the_fill_with_the_real_commission() {
    // The whole recovery round trip, as the exec loop drives it: the commission is lost, the
    // sweep asks for it, IBKR re-delivers BOTH halves, and the ordinary join emits the fill.
    let mut m = mapper_submitted("coid-A", 101, 10.0);
    assert!(exec(&mut m, 101, "coid-A", "e1", 10.0).is_empty(), "no fill without a commission");
    m.sweep_pending(0, 15_000);
    assert_eq!(m.sweep_pending(15_000, 15_000).recover, vec!["e1".to_string()]);
    // `reqExecutions` replays the execution AND its commission report (in either order — the
    // join is by exec_id, never by arrival).
    assert!(exec(&mut m, 101, "coid-A", "e1", 10.0).is_empty(), "replayed exec re-buffers");
    match m
        .on_commission_report(IbCommissionReport {
            exec_id: "e1".into(),
            commission: 2.5,
            currency: "USD".into(),
        })
        .as_slice()
    {
        [Event::OrderFilled(f)] => {
            assert_eq!(f.fill.trade_id, "e1");
            assert_eq!(f.fill.commission, 2.5);
        }
        other => panic!("expected exactly one OrderFilled, got {other:?}"),
    }
    assert!(m.pending.is_empty(), "nothing strands after the recovery");
}

// -----------------------------------------------------------------------------------------
// Replay SAFETY: `reqExecutions` returns the whole day indiscriminately, so every re-delivery
// path has to be inert for a fill that already emitted. These are the tests that keep the
// recovery from being worse than the disease.
// -----------------------------------------------------------------------------------------

#[test]
fn replaying_an_emitted_partial_does_not_advance_fill_state_and_lose_later_executions() {
    // THE regression guard. Order of 10: e1=4 emits a partial, e2=3 strands. The replay
    // re-delivers e1 as well. If the replayed e1 re-entered `pending` and re-joined,
    // `fill_state.filled` would reach 4+4+3 = 11 ≥ 10, so e2 would be classified FINAL — the
    // order forgotten while 3 shares are still working, and every later execution silently
    // dropped. With the `emitted` index, e2 is correctly still a PARTIAL.
    let mut m = mapper_submitted("coid-A", 101, 10.0);
    let _ = exec(&mut m, 101, "coid-A", "e1", 4.0);
    assert!(matches!(commission(&mut m, "e1").as_slice(), [Event::OrderPartiallyFilled(_)]));
    let _ = exec(&mut m, 101, "coid-A", "e2", 3.0); // commission lost → stranded

    // The replay: both halves of the already-emitted e1 come back. Both are no-ops.
    assert!(exec(&mut m, 101, "coid-A", "e1", 4.0).is_empty(), "replayed exec must not buffer");
    assert!(commission(&mut m, "e1").is_empty(), "replayed commission must not emit");
    assert!(m.commissions.is_empty(), "…and must not park either");
    assert_eq!(m.pending.len(), 1, "only the genuinely stranded e2 is buffered");

    // e2's replayed commission now joins — and is still a PARTIAL (4+3 = 7 < 10).
    assert!(matches!(commission(&mut m, "e2").as_slice(), [Event::OrderPartiallyFilled(_)]));
    // Proof the order was NOT forgotten: a third execution still resolves and completes it.
    let _ = exec(&mut m, 101, "coid-A", "e3", 3.0);
    assert!(matches!(commission(&mut m, "e3").as_slice(), [Event::OrderFilled(_)]));
}

#[test]
fn replaying_a_fully_emitted_pair_emits_nothing_in_either_arrival_order() {
    for commission_first in [false, true] {
        let mut m = mapper_submitted("coid-A", 101, 5.0);
        let _ = exec(&mut m, 101, "coid-A", "e1", 5.0);
        assert!(matches!(commission(&mut m, "e1").as_slice(), [Event::OrderFilled(_)]));
        if commission_first {
            assert!(commission(&mut m, "e1").is_empty());
            assert!(exec(&mut m, 101, "coid-A", "e1", 5.0).is_empty());
        } else {
            assert!(exec(&mut m, 101, "coid-A", "e1", 5.0).is_empty());
            assert!(commission(&mut m, "e1").is_empty());
        }
        assert!(m.pending.is_empty(), "commission_first={commission_first}");
        assert!(m.commissions.is_empty(), "commission_first={commission_first}");
    }
}

#[test]
fn emitted_index_is_capped_and_evicts_the_oldest_first() {
    // Bounded like `commissions`, for the same venue-supplied-key reason — but eviction here
    // is safe: the worst case is a duplicate fill event, which `vike_exec::ExecutionEngine`
    // dedups by `trade_id`. Cap the map by driving that many complete fills.
    let mut m = mapper_with("coid-A", 101);
    for i in 0..MAX_EMITTED_EXEC_IDS {
        // No `on_submit` ⇒ unknown total ⇒ each fill is terminal and forgets the order, so
        // rebind before the next one.
        m.ids_mut().bind(101, "coid-A");
        let id = format!("x{i}");
        let _ = exec(&mut m, 101, "coid-A", &id, 1.0);
        assert!(matches!(commission(&mut m, &id).as_slice(), [Event::OrderFilled(_)]));
    }
    assert_eq!(m.emitted.len(), MAX_EMITTED_EXEC_IDS);
    m.ids_mut().bind(101, "coid-A");
    let _ = exec(&mut m, 101, "coid-A", "newest", 1.0);
    assert!(matches!(commission(&mut m, "newest").as_slice(), [Event::OrderFilled(_)]));
    assert_eq!(m.emitted.len(), MAX_EMITTED_EXEC_IDS, "the cap holds");
    assert!(!m.emitted.contains_key("x0"), "the oldest record is the one evicted");
    assert!(m.emitted.contains_key("x1"));
    assert!(m.emitted.contains_key("newest"));
}

#[test]
fn a_cancel_flushed_partial_is_recorded_as_emitted_and_survives_a_replay() {
    // The `flushed` set #949 introduced folded into `emitted`. It kept its original job (the
    // late REAL commission for a cancel-flushed partial is a no-op) and gained non-consumption,
    // so a `reqExecutions` replay of that same pair cannot resurrect it either.
    let mut m = mapper_submitted("coid-A", 101, 10.0);
    let _ = exec(&mut m, 101, "coid-A", "e1", 4.0); // execDetails, NO commission yet
    let out = m.on_order_status(IbOrderStatus {
        order_id: 101,
        order_ref: "coid-A".into(),
        status: "Cancelled".into(),
        filled: 4.0,
        avg_fill_price: 190.0,
    });
    assert!(matches!(out.as_slice(), [Event::OrderPartiallyFilled(_), Event::OrderCanceled(_)]));
    // Late real commission: still a no-op, still not parked (the #949 assertions).
    assert!(commission(&mut m, "e1").is_empty());
    assert!(m.commissions.is_empty());
    // NEW: the whole pair replayed — also inert, where a consuming set would have re-armed.
    assert!(exec(&mut m, 101, "coid-A", "e1", 4.0).is_empty());
    assert!(commission(&mut m, "e1").is_empty());
    assert!(m.pending.is_empty());
    assert!(m.commissions.is_empty());
}
