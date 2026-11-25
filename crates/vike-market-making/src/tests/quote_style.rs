//! The quoting-style registry + own-order-book filtration (audit mm3) as pure functions: each
//! [`QuoteStyle`] priced off a fixture book, the `Mid` bit-for-bit reduction, and `filter_own`.
//! The mounted maker on the L1 and L2 lanes moved with it to the spread maker file's tests
//! (`strategies/market_making/spread_maker.rs`).

use super::*;
use vike_marketdata::L2BookLevel;

// ---- quoting-style registry + own-order-book filtration (audit mm3) ----

// Each style derives the expected prices from ONE fixture L2 book (tick grid 1.0). This is the
// whole registry in isolation — no runtime, no broker.
#[test]
fn each_style_prices_from_a_fixture_book() {
    let book = BookView {
        tick_size: 1.0,
        bids: vec![
            L2BookLevel::new(100.0, 5.0),
            L2BookLevel::new(99.0, 7.0),
            L2BookLevel::new(98.0, 9.0),
        ], // best-first
        asks: vec![
            L2BookLevel::new(101.0, 4.0),
            L2BookLevel::new(102.0, 6.0),
            L2BookLevel::new(103.0, 8.0),
        ],
    };
    // Mid: mid = 100.5, ± half_spread(0.25)
    let (b, a) = book.priced(QuoteStyle::Mid, 0.25, 1).unwrap();
    assert_eq!(b.to_bits(), 100.25_f64.to_bits(), "Mid bid = mid − half_spread");
    assert_eq!(a.to_bits(), 100.75_f64.to_bits(), "Mid ask = mid + half_spread");
    // Join: at the touch
    let (b, a) = book.priced(QuoteStyle::Join, 0.25, 1).unwrap();
    assert_eq!(b.to_bits(), 100.0_f64.to_bits(), "Join bid = best bid");
    assert_eq!(a.to_bits(), 101.0_f64.to_bits(), "Join ask = best ask");
    // Top: one tick IN FRONT
    let (b, a) = book.priced(QuoteStyle::Top, 0.25, 1).unwrap();
    assert_eq!(b.to_bits(), 101.0_f64.to_bits(), "Top bid = best bid + tick");
    assert_eq!(a.to_bits(), 100.0_f64.to_bits(), "Top ask = best ask − tick");
    // Depth 1 → the SECOND level each side; Depth 2 → the third; beyond clamps to the deepest
    let (b, a) = book.priced(QuoteStyle::Depth, 0.25, 1).unwrap();
    assert_eq!(b.to_bits(), 99.0_f64.to_bits(), "Depth 1 bid = bids[1]");
    assert_eq!(a.to_bits(), 102.0_f64.to_bits(), "Depth 1 ask = asks[1]");
    let (b, a) = book.priced(QuoteStyle::Depth, 0.25, 2).unwrap();
    assert_eq!(b.to_bits(), 98.0_f64.to_bits(), "Depth 2 bid = bids[2]");
    assert_eq!(a.to_bits(), 103.0_f64.to_bits(), "Depth 2 ask = asks[2]");
    let (b, a) = book.priced(QuoteStyle::Depth, 0.25, 99).unwrap();
    assert_eq!(b.to_bits(), 98.0_f64.to_bits(), "Depth clamps to the deepest bid level");
    assert_eq!(a.to_bits(), 103.0_f64.to_bits(), "Depth clamps to the deepest ask level");
    // a one-sided book cannot be quoted two-sided
    let half = BookView { tick_size: 1.0, bids: vec![L2BookLevel::new(100.0, 5.0)], asks: vec![] };
    assert!(half.priced(QuoteStyle::Mid, 0.25, 1).is_none(), "missing ask side → None");
}

// The DEFAULT `Mid` reproduces the original `0.5·(bid+ask) ± half_spread` formula BIT-FOR-BIT,
// for several bid/ask/half_spread triples (incl. a Polymarket-shaped 0..1 book).
#[test]
fn mid_style_reproduces_todays_formula_bit_for_bit() {
    for &(bid, ask, hs) in &[(100.0, 100.2, 0.5), (99.0, 100.0, 0.5), (0.41, 0.44, 0.005)] {
        let book = BookView::from_quote(&quote(0, bid, ask), 0.0);
        let (b, a) = book.priced(QuoteStyle::Mid, hs, 1).unwrap();
        let mid = 0.5 * (bid + ask); // the exact original expression
        assert_eq!(b.to_bits(), (mid - hs).to_bits(), "bid bit-identical to mid − hs");
        assert_eq!(a.to_bits(), (mid + hs).to_bits(), "ask bit-identical to mid + hs");
    }
}

// Own-order filtration removes our resting size and SHIFTS the derived best when we were alone at
// the touch; Join on the filtered book then quotes the GENUINE market, not our own quote.
#[test]
fn own_order_filtration_shifts_the_derived_best() {
    let book = BookView {
        tick_size: 0.5,
        bids: vec![L2BookLevel::new(100.0, 3.0), L2BookLevel::new(99.5, 8.0)],
        asks: vec![L2BookLevel::new(100.5, 2.0), L2BookLevel::new(101.0, 9.0)],
    };
    // we rest the ENTIRE best bid (100.0, size 3) and best ask (100.5, size 2)
    let f = filter_own(&book, Some((100.0, 3.0)), Some((100.5, 2.0)));
    assert_eq!(f.bids.len(), 1, "our whole best-bid level was removed");
    assert_eq!(f.asks.len(), 1, "our whole best-ask level was removed");
    assert_eq!(f.bids[0].price.to_bits(), 99.5_f64.to_bits(), "best bid shifts past our own");
    assert_eq!(f.asks[0].price.to_bits(), 101.0_f64.to_bits(), "best ask shifts past our own");
    let (b, a) = f.priced(QuoteStyle::Join, 0.0, 1).unwrap();
    assert_eq!(b.to_bits(), 99.5_f64.to_bits(), "Join now leans on the genuine next bid");
    assert_eq!(a.to_bits(), 101.0_f64.to_bits(), "Join now leans on the genuine next ask");
}

// When we are NOT alone at a level, filtration only REDUCES its size — the best price is unchanged.
#[test]
fn own_order_filtration_reduces_size_when_not_alone() {
    let book = BookView {
        tick_size: 0.5,
        bids: vec![L2BookLevel::new(100.0, 10.0)],
        asks: vec![L2BookLevel::new(100.5, 10.0)],
    };
    let f = filter_own(&book, Some((100.0, 4.0)), None);
    assert_eq!(
        f.bids[0].price.to_bits(),
        100.0_f64.to_bits(),
        "best price unchanged when not alone"
    );
    assert_eq!(f.bids[0].qty.to_bits(), 6.0_f64.to_bits(), "our size subtracted from the level");
    assert_eq!(f.asks[0].qty.to_bits(), 10.0_f64.to_bits(), "untouched side is unchanged");
}

// Filtration needs a tick grid to match a float price to a level; with an unknown grid it is inert.
#[test]
fn filtration_is_inert_without_a_tick_grid() {
    let book = BookView {
        tick_size: 0.0,
        bids: vec![L2BookLevel::new(100.0, 3.0)],
        asks: vec![L2BookLevel::new(101.0, 3.0)],
    };
    let f = filter_own(&book, Some((100.0, 3.0)), Some((101.0, 3.0)));
    assert_eq!(f.bids.len(), 1, "no grid → no match → nothing removed (bids)");
    assert_eq!(f.asks.len(), 1, "no grid → no match → nothing removed (asks)");
}
