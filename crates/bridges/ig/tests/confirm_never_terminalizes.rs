//! IG's dropped-confirm contract, folded through the REAL FSM.
//!
//! `crates/bridges/ig/src/exec.rs`'s submit path is two round trips: `POST /positions/otc` (or
//! `/workingorders/otc`) answers a `dealReference`, then `GET /confirms/{dealReference}` resolves
//! accept / fill / reject. The first call returning a reference is IG saying **"I have this deal"**.
//! So whatever happens to the SECOND call, the order exists at the venue and may already have
//! filled.
//!
//! ⚠ **The `Err` arm of that second call used to emit a terminal `OrderRejected`.** A dropped reply,
//! a timed-out read, or a 404 from a reference IG had not finished processing (measured shape:
//! `error.service.execution.find`) all published "this order was rejected" over an order that was
//! live. The strategy then believed it was flat while holding a position — local state made FALSE,
//! not merely stuck, and nothing downstream is looking for an order everybody agrees is finished.
//!
//! These tests pin the replacement against the same oracle the cross-bridge conformance harness
//! uses — `vike_exec::ManagedOrder::apply`, the FSM the live core folds through — so what is
//! asserted is the CLAUDE.md venue-adapter contract itself ("no order may silently vanish",
//! "exactly one terminal"), not a shape this crate invented.

use vike_exec::{ManagedOrder, OrderStatus};
use vike_ig::{IgApiError, unresolved_confirm};
use vike_model::OrderRequest;
use vike_model::events::{Event, FillEvent, OrderFilled, OrderSubmitted, TradeId};

const COID: &str = "ig-coid-1";
const DEAL_REF: &str = "2LJZQPM3Y24TYS9";

fn order() -> OrderRequest {
    OrderRequest {
        client_order_id: COID.into(),
        venue: "ig".into(),
        symbol: "CS.D.EURUSD.MINI.IP".into(),
        side: 1,
        qty: 0.1,
        order_type: "market".into(),
        ts: 1,
        ..Default::default()
    }
}

/// The two failure shapes a `/confirms` call actually produces, measured against
/// `demo-api.ig.com` on 2026-08-21: a reference IG has not finished processing (404
/// `error.service.execution.find`), and never reaching IG at all (this crate's `status == 0`
/// network marker).
fn confirm_failures() -> Vec<IgApiError> {
    vec![
        IgApiError { status: 404, message: "error.service.execution.find".into() },
        IgApiError { status: 0, message: "network error: connection reset".into() },
    ]
}

/// Fold a venue event stream exactly as `ExecutionEngine::on_event` does for one order: the bare
/// `Fill` is Account-side only (never the FSM), the wrap advances the FSM. Returns the order.
fn fold(events: &[Event]) -> ManagedOrder {
    let mut mo = ManagedOrder::new(order());
    mo.apply(&Event::OrderSubmitted(OrderSubmitted { client_order_id: COID.into(), ts: 1 }))
        .expect("Initialized -> Submitted is legal");
    for ev in events {
        if matches!(ev, Event::Fill(_)) {
            continue; // Account side; the FSM has no FillEvent transition
        }
        let _ = mo.apply(ev); // an illegal transition is an idempotent drop, as in the engine
    }
    mo
}

/// **The defect, stated as the contract.** An unanswered confirm must leave the order LIVE. A
/// terminal here is a lie about a deal IG has already acknowledged.
#[test]
fn an_unanswered_confirm_leaves_the_order_live_and_never_terminalizes_it() {
    for err in confirm_failures() {
        let evs = unresolved_confirm(COID, 1, DEAL_REF, &err);
        assert!(!evs.is_empty(), "an unanswered confirm must not VANISH either ({err})");
        assert!(
            !evs.iter().any(|e| matches!(
                e,
                Event::OrderRejected(_)
                    | Event::OrderCanceled(_)
                    | Event::OrderExpired(_)
                    | Event::OrderDenied(_)
                    | Event::OrderFilled(_)
            )),
            "no terminal event may be synthesized for a deal IG already acknowledged ({err}): \
             {evs:?}"
        );
        let mo = fold(&evs);
        assert!(
            !mo.status.is_terminal(),
            "the FSM must still call this order live after an unanswered confirm ({err}); \
             status = {:?}",
            mo.status
        );
        assert_eq!(mo.status, OrderStatus::Accepted, "the optimistic managed order ({err})");
    }
}

/// The half that makes "stuck" recoverable: because the order is still live, the fill that arrives
/// later — on the Lightstreamer `CONFIRMS` for this very `dealReference`, or from the confirm
/// watchdog's re-query — still folds it to `Filled`, with exactly one terminal.
///
/// ⚠ This is what a terminal reject destroyed. Once an order is `Rejected` the FSM refuses the
/// later fill, so the position IG really opened is never booked locally at all.
#[test]
fn a_fill_arriving_after_an_unanswered_confirm_still_reaches_exactly_one_terminal() {
    let mut evs = unresolved_confirm(
        COID,
        1,
        DEAL_REF,
        &IgApiError { status: 404, message: "error.service.execution.find".into() },
    );
    let fill = FillEvent {
        trade_id: TradeId::new("DIAAAAYB6YDK2A7").expect("a real IG dealId"),
        client_order_id: COID.into(),
        venue: "ig".to_string().into(),
        symbol: "CS.D.EURUSD.MINI.IP".to_string().into(),
        side: 1,
        last_qty: 0.1,
        last_px: 1.16784,
        commission: 0.0,
        commission_asset: String::new().into(),
        liquidity_side: String::new().into(),
        ts: 2,
        mark_price: None,
        position_side: "BOTH".to_string().into(),
    };
    evs.push(Event::Fill(fill.clone()));
    evs.push(Event::OrderFilled(OrderFilled { client_order_id: COID.into(), fill, ts: 2 }));

    let mo = fold(&evs);
    assert_eq!(mo.status, OrderStatus::Filled, "the later fill still lands");
    assert!((mo.filled_qty - 0.1).abs() < 1e-12, "and books the real size: {}", mo.filled_qty);
}

/// The optimistic accept carries **no venue order id**, deliberately: we did not learn the `dealId`,
/// and inventing one would put a value the venue never said into `venue_order_id`, where reconcile
/// reads it as truth. `vike_bridge_core::rest::resolve_ambiguous_submit`'s inconclusive arm makes
/// the identical choice for the crypto venues.
#[test]
fn the_optimistic_accept_claims_no_venue_order_id() {
    let evs = unresolved_confirm(
        COID,
        7,
        DEAL_REF,
        &IgApiError { status: 0, message: "network error: timed out".into() },
    );
    match evs.as_slice() {
        [Event::OrderAccepted(a)] => {
            assert_eq!(a.client_order_id, COID);
            assert!(a.venue_order_id.is_none(), "no id was learned, so none is claimed");
            assert_eq!(a.ts, 7, "the submit's own stamp, not a fabricated one");
        }
        other => panic!("expected exactly one OrderAccepted, got {other:?}"),
    }
}
