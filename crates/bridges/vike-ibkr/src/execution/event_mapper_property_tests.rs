//! Property harness for the IBKR fold: [`EventMapper`] (and the [`OrderVenueIdAllocator`] it resolves
//! through) fed a short random sequence of the normalized inbounds every backend produces —
//! `orderStatus`, `execDetails`, `commissionReport`, `error`, the open-order rebind and the
//! stranded-fill sweep — interleaved with submits. The property is TOTALITY: hostile ids, sizes,
//! prices, timestamps and status words may fold to nothing, but they must never panic the exec
//! thread and one inbound must never fabricate an event flood.
//!
//! This lives beside the mapper (not under `tests/`) because `event_mapper` and `id_registry` are
//! crate-private: the only public route to them is `run_execution_for_test` over a threaded
//! `IbkrTransportFake`, which polls with real sleeps and cannot run 256 deterministic cases. The wire
//! decoders that PRODUCE these inbounds (`cpapi_decode`, the recon parsers) are covered through the
//! public API in `tests/decoder_never_panics.rs`.
//!
//! `NextValidId` is deliberately NOT one of the ops of the sequence property: it has its own test
//! below, because `OrderVenueIdAllocator::next_order_id` overflows an `i32` for one value the gateway can send
//! and that known failure would otherwise mask every other counterexample the sequence could find.

use super::*;
use crate::execution::registry::OrderVenueIdAllocator;
use proptest::prelude::*;
use vike_bridge_core::json_arb::{arb_f64, arb_i64};

/// The order client ids / `orderRef`s the sequences use (the last is EMPTY: an order with no order client id).
const REFS: [&str; 4] = ["c-A", "c-B", "c-C", ""];
/// The execution ids the sequences use (the last is EMPTY: an id-less execution).
const EXECUTION_IDS: [&str; 4] = ["e1", "e2", "e3", ""];

#[derive(Debug, Clone)]
enum Op {
    Submit {
        order_client_id: usize,
        qty: f64,
    },
    Status {
        order_id: i32,
        order_ref: usize,
        status: &'static str,
        filled: f64,
        avg: f64,
    },
    Execution {
        order_id: i32,
        order_ref: usize,
        exec_id: usize,
        side_buy: bool,
        shares: f64,
        price: f64,
        ts: i64,
    },
    Commission {
        exec_id: usize,
        commission: f64,
    },
    Error {
        code: i32,
        order_id: i32,
    },
    Rebind {
        order_id: i32,
        order_ref: usize,
    },
    Sweep {
        now: i64,
        grace: i64,
    },
}

/// Mostly the ids the submits allocate (101..), sometimes 0, -1 or anything.
fn arb_order_id() -> impl Strategy<Value = i32> {
    prop_oneof![6 => 101i32..=104, 1 => Just(0i32), 1 => Just(-1i32), 1 => any::<i32>()]
}

/// Every status word `OrderStatusKind::from_ibkr` knows, plus the ones it must tolerate.
fn arb_status() -> impl Strategy<Value = &'static str> {
    prop::sample::select(vec![
        "Submitted",
        "PreSubmitted",
        "PendingSubmit",
        "ApiPending",
        "Filled",
        "Cancelled",
        "Canceled",
        "ApiCancelled",
        "PendingCancel",
        "PreCancelled",
        "WarnState",
        "Inactive",
        "",
        "garbage",
    ])
}

/// Codes of each refusal set (submit, cancel, amend), the connectivity and advisory codes, and anything.
fn arb_code() -> impl Strategy<Value = i32> {
    prop_oneof![
        6 => prop::sample::select(vec![
            110, 200, 201, 203, 321, 10052, 10289, 10293, 135, 10147, 104, 2102, 326, 1100, 1101, 1102,
            2103, 2104, 2110, 2150,
        ]),
        1 => any::<i32>(),
    ]
}

fn arb_op() -> impl Strategy<Value = Op> {
    prop_oneof![
        2 => (0usize..4, arb_f64()).prop_map(|(order_client_id, qty)| Op::Submit { order_client_id, qty }),
        3 => (arb_order_id(), 0usize..4, arb_status(), arb_f64(), arb_f64()).prop_map(
            |(order_id, order_ref, status, filled, avg)| Op::Status {
                order_id,
                order_ref,
                status,
                filled,
                avg,
            }
        ),
        4 => (
            arb_order_id(),
            0usize..4,
            0usize..4,
            any::<bool>(),
            arb_f64(),
            arb_f64(),
            arb_i64(),
        )
            .prop_map(|(order_id, order_ref, exec_id, side_buy, shares, price, ts)| Op::Execution {
                order_id,
                order_ref,
                exec_id,
                side_buy,
                shares,
                price,
                ts,
            }),
        3 => (0usize..4, arb_f64())
            .prop_map(|(exec_id, commission)| Op::Commission { exec_id, commission }),
        2 => (arb_code(), arb_order_id()).prop_map(|(code, order_id)| Op::Error { code, order_id }),
        1 => (arb_order_id(), 0usize..4)
            .prop_map(|(order_id, order_ref)| Op::Rebind { order_id, order_ref }),
        1 => (arb_i64(), arb_i64()).prop_map(|(now, grace)| Op::Sweep { now, grace }),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    /// A random op sequence folds through ONE mapper without a panic, and no single inbound emits
    /// a flood (a cancel flushes the order's parked fills, at most one per `execDetails` so far).
    #[test]
    fn mapper_op_sequences_never_panic(ops in prop::collection::vec(arb_op(), 1..12)) {
        let mut ids = OrderVenueIdAllocator::default();
        ids.on_next_valid_id(101);
        let mut m = EventMapper::new(ids);
        for op in &ops {
            let events = match op {
                Op::Submit { order_client_id, qty } => {
                    let id = m.ids_mut().next_order_id().unwrap();
                    m.ids_mut().bind(id, REFS[*order_client_id]);
                    m.on_submit(id, *qty);
                    Vec::new()
                }
                Op::Status { order_id, order_ref, status, filled, avg } => {
                    m.on_order_status(IbkrOrderStatus {
                        order_id: *order_id,
                        order_ref: REFS[*order_ref].to_string(),
                        status: (*status).to_string(),
                        filled: *filled,
                        average_fill_price: *avg,
                        ..Default::default()
                    })
                }
                Op::Execution { order_id, order_ref, exec_id, side_buy, shares, price, ts } => {
                    m.on_execution_details(IbkrExecutionDetails {
                        order_id: *order_id,
                        order_ref: REFS[*order_ref].to_string(),
                        exec_id: EXECUTION_IDS[*exec_id].to_string(),
                        symbol: "AAPL.SMART.USD".to_string(),
                        side_buy: *side_buy,
                        shares: *shares,
                        price: *price,
                        ts: *ts,
                        ..Default::default()
                    })
                }
                Op::Commission { exec_id, commission } => {
                    m.on_commission_report(IbkrCommissionReport {
                        exec_id: EXECUTION_IDS[*exec_id].to_string(),
                        commission: *commission,
                        currency: "USD".to_string(),
                    })
                }
                Op::Error { code, order_id } => m.on_error(*code, *order_id, "venue message"),
                Op::Rebind { order_id, order_ref } => {
                    m.rebind_open_order(*order_id, REFS[*order_ref])
                }
                Op::Sweep { now, grace } => {
                    let _ = m.sweep_pending(*now, *grace);
                    Vec::new()
                }
            };
            prop_assert!(events.len() <= 64, "event flood: {} events from {:?}", events.len(), op);
            let _ = m.live_order_client_ids();
        }
    }

    /// The order-id allocator over every `nextValidId` the handshake can deliver.
    ///
    /// The deterministic reproducer (the handshake value is read straight off the wire and
    /// `execution::fold_inbound` hands it to `on_next_valid_id` unclamped) is
    /// `next_valid_id_i32_max_then_one_submit_does_not_overflow`: `i32::MAX` is handed out once, then `None`.
    #[test]
    fn id_registry_survives_any_next_valid_id(
        seeds in prop::collection::vec(
            prop_oneof![
                Just(i32::MAX),
                Just(i32::MAX - 1),
                Just(i32::MIN),
                Just(0i32),
                any::<i32>(),
            ],
            1..6,
        ),
    ) {
        let mut r = OrderVenueIdAllocator::default();
        for seed in seeds {
            r.on_next_valid_id(seed);
            // `None` once the id space is spent is the answer, not a panic.
            let _ = r.next_order_id();
            let _ = r.next_order_id();
        }
    }
}

/// Reproducer for `id_registry_survives_any_next_valid_id` (it used to overflow, then to reuse the id).
#[test]
fn next_valid_id_i32_max_then_one_submit_does_not_overflow() {
    let mut r = OrderVenueIdAllocator::default();
    r.on_next_valid_id(i32::MAX);
    assert_eq!(r.next_order_id(), Some(i32::MAX));
    assert_eq!(r.next_order_id(), None);
}
