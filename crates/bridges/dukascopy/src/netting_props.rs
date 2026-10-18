//! "Arbitrary input never panics" harness for the PRIVATE netting shadow of this module: the
//! [`ShadowBook`] the sidecar reader thread folds every bare fill into and checks every `position`
//! line against. Its inputs are wire values — the fills the sidecar reports and the `position`
//! envelope's `(size, avg_px, ts)`, decoded by [`crate::proto::parse_envelope`] — so a hostile
//! sidecar line reaches it. The protocol decoders are covered in
//! `crates/bridges/dukascopy/tests/decoder_never_panics.rs`; the shadow is reachable from outside the
//! crate only through a spawned child process, so it gets a sibling unit file in the
//! `netting_tests.rs` style.
//!
//! The property is TOTALITY over a SEQUENCE: any interleaving of fills and position lines, with
//! `NaN` / infinite / huge / zero sizes and prices, never panics the reader thread, and a
//! `Corrected` verdict always carries exactly the close + reopen pair of bare fills. Beyond that
//! only the law the module states is asserted, over FINITE inputs: a corrected line is idempotent
//! (the same line again is `InSync`).

use super::*;
use proptest::prelude::*;

/// A fill or a `position` line, as the reader thread meets them.
#[derive(Debug, Clone)]
enum Op {
    Fill { symbol: &'static str, side: i32, qty: f64, px: f64 },
    Line { symbol: &'static str, size: f64, avg: f64, ts: i64 },
}

fn num() -> impl Strategy<Value = f64> {
    prop_oneof![
        4 => -1.0e6f64..1.0e6,
        2 => prop::sample::select(vec![0.0, -0.0, 1_000.0, -1_000.0, 1e-3, 1e-9, 1e300, -1e300]),
        1 => Just(f64::NAN),
        1 => Just(f64::INFINITY),
        1 => Just(f64::NEG_INFINITY),
        1 => any::<f64>(),
    ]
}

fn symbol() -> impl Strategy<Value = &'static str> {
    prop::sample::select(vec!["EURUSD", "USDJPY", ""])
}

fn op() -> impl Strategy<Value = Op> {
    prop_oneof![
        (symbol(), prop_oneof![Just(1), Just(-1), Just(0), any::<i32>()], num(), num())
            .prop_map(|(symbol, side, qty, px)| Op::Fill { symbol, side, qty, px }),
        (symbol(), num(), num(), any::<i64>()).prop_map(|(symbol, size, avg, ts)| Op::Line {
            symbol,
            size,
            avg,
            ts
        }),
    ]
}

fn fill(symbol: &str, side: i32, qty: f64, px: f64) -> FillEvent {
    FillEvent {
        trade_id: "t".into(),
        client_order_id: "c".into(),
        venue: "dukascopy".into(),
        symbol: symbol.into(),
        side,
        last_qty: qty,
        last_px: px,
        commission: 0.0,
        commission_asset: "".into(),
        liquidity_side: LiquiditySide::Taker,
        ts: 1,
        mark_price: None,
        position_side: PositionSide::Both,
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    /// Any sequence of fills and position lines into ONE book: no panic, and a correction is
    /// always the close + reopen pair of bare fills.
    #[test]
    fn shadow_book_survives_any_fill_and_line_sequence(ops in prop::collection::vec(op(), 1..12)) {
        let mut book = ShadowBook::default();
        for op in &ops {
            match op {
                Op::Fill { symbol, side, qty, px } => book.fold_fill(&fill(symbol, *side, *qty, *px)),
                Op::Line { symbol, size, avg, ts } => {
                    if let Reanchor::Corrected(legs) = book.reanchor(symbol, *size, *avg, *ts) {
                        prop_assert_eq!(legs.len(), 2);
                        prop_assert!(legs.iter().all(|e| matches!(e, Event::Fill(_))));
                    }
                }
            }
        }
    }

    /// Over FINITE, venue-plausible inputs a corrected line is idempotent: the same line again is
    /// `InSync` (the module doc's "idempotent by construction").
    #[test]
    fn a_corrected_line_is_idempotent(
        fills in prop::collection::vec(
            (prop_oneof![Just(1), Just(-1)], (1u32..1_000).prop_map(|n| f64::from(n) * 1_000.0), 0.5f64..200.0),
            1..8,
        ),
        venue_avg in 0.5f64..200.0,
        ts in any::<i64>(),
    ) {
        let mut book = ShadowBook::default();
        for (side, qty, px) in &fills {
            book.fold_fill(&fill("EURUSD", *side, *qty, *px));
        }
        let local_size = book.positions.get("EURUSD").map_or(0.0, |s| s.size);
        if let Reanchor::Corrected(_) = book.reanchor("EURUSD", local_size, venue_avg, ts) {
            prop_assert_eq!(book.reanchor("EURUSD", local_size, venue_avg, ts), Reanchor::InSync);
        }
    }
}
