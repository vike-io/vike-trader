//! The paper book's slippage-table terms (`client/slippage.rs`): a book given none is the flat
//! fraction, byte for byte, and keeps no bar; a book given the impact model charges a taker the
//! model's cost over the bars that closed before the fill, from a window it keeps bounded, and a
//! maker the flat fraction alone. The bit-for-bit identity with the simulator is
//! `crates/vike-simulator/tests/r7_gate.rs`'s `a_slippage_table_charges_the_same_on_paper_and_in_the_engine`.

use super::*;
use vike_execution::ExecutionClient;
use vike_fills::market_impact::{
    ImpactInputs, ImpactModel, ImpactModelAlmgrenChriss, ImpactTerms, ProfileEngineSlippageTerms,
    window_stats_before,
};

const SYMBOL: &str = "BTCUSDT";
const FLAT: f64 = 0.0002;

/// Bar `i` of a zig-zag series with a varying volume, stamped one minute apart.
fn bar_at(i: usize) -> Bar {
    let close = 100.0 + if i.is_multiple_of(2) { 1.0 } else { -1.5 } * (1.0 + i as f64 / 10.0);
    Bar {
        timestamp: i as i64 * 60_000,
        open: close,
        high: close,
        low: close,
        close,
        volume: 10.0 + (i % 3) as f64 * 4.0,
        funding: None,
        bid: None,
        ask: None,
        symbol: None,
    }
}

fn impact_book(window: usize) -> (ExecutionClientPaper, Arc<dyn ImpactModel>) {
    let model: Arc<dyn ImpactModel> = Arc::new(ImpactModelAlmgrenChriss::default());
    let terms = ProfileEngineSlippageTerms {
        fraction: FLAT,
        amount: 0.0,
        impact: Some((Arc::clone(&model), window)),
    };
    let book = ExecutionClientPaper::new("paper", SYMBOL, 0.0, 0.0, 0.0).with_slippage_terms(terms);
    (book, model)
}

/// No terms: the `slippage` field itself for every fill, and no bar kept.
#[test]
fn a_book_given_no_terms_is_the_flat_fraction_and_keeps_no_bar() {
    let mut c = ExecutionClientPaper::new("paper", SYMBOL, FLAT, 0.0, 0.0);
    for i in 0..30 {
        c.on_bar(&bar_at(i));
    }
    assert!(c.impact.is_none(), "a book given no terms arms no model");
    let ts = 30 * 60_000;
    assert_eq!(c.slippage_for(SYMBOL, 5.0, ts, false).to_bits(), FLAT.to_bits());
    assert_eq!(c.slippage_for(SYMBOL, 5.0, ts, true).to_bits(), FLAT.to_bits());
}

/// A taker pays the flat fraction plus the model over the last `window` bars that closed before the
/// fill, measured from the bounded window the book kept; a maker pays the flat fraction alone; too
/// few bars charge the flat fraction alone.
#[test]
fn a_taker_pays_the_model_over_the_closed_bars_and_a_maker_does_not() {
    let window = 4;
    let (mut c, model) = impact_book(window);
    let series: Vec<Bar> = (0..40).map(bar_at).collect();
    c.on_bar(&series[0]);
    c.on_bar(&series[1]);
    // two bars, the second the one a fill at its open falls inside: one closed bar, no return
    assert_eq!(c.slippage_for(SYMBOL, 5.0, series[1].timestamp, false).to_bits(), FLAT.to_bits());
    for b in &series[2..] {
        c.on_bar(b);
    }
    let last = series.last().expect("bars");
    let stats = window_stats_before(&series, last.timestamp, window).expect("measurable");
    let want = FLAT
        + model.impact_fraction_for(
            &ImpactInputs { qty: 5.0, average_volume: stats.average_volume, sigma: stats.sigma },
            ImpactTerms::Both,
        );
    assert!(want > FLAT, "the fixture must produce a real charge");
    assert_eq!(c.slippage_for(SYMBOL, 5.0, last.timestamp, false).to_bits(), want.to_bits());
    assert_eq!(c.slippage_for(SYMBOL, 5.0, last.timestamp, true).to_bits(), FLAT.to_bits());
    // another symbol's fill reads that symbol's bars, of which this book holds none
    assert_eq!(c.slippage_for("ETHUSDT", 5.0, last.timestamp, false).to_bits(), FLAT.to_bits());
}
