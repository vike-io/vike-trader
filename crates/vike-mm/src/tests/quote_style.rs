//! The quoting-style registry + own-order-book filtration (audit mm3): each [`QuoteStyle`] priced
//! off a fixture book, the `Mid` bit-for-bit reduction, and filtration on both the L1 and L2 lanes.

use super::*;

// ---- quoting-style registry + own-order-book filtration (audit mm3) ----

// Each style derives the expected prices from ONE fixture L2 book (tick grid 1.0). This is the
// whole registry in isolation — no runtime, no broker.
#[test]
fn each_style_prices_from_a_fixture_book() {
    let book = BookView {
        tick_size: 1.0,
        bids: vec![(100.0, 5.0), (99.0, 7.0), (98.0, 9.0)], // best-first
        asks: vec![(101.0, 4.0), (102.0, 6.0), (103.0, 8.0)],
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
    let half = BookView { tick_size: 1.0, bids: vec![(100.0, 5.0)], asks: vec![] };
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

// End-to-end on the L1 lane: a DEFAULT maker (Mid, filtration OFF) quotes off the feed exactly as
// before — the submitted prices equal the original formula bit-for-bit, and the new knobs are all
// at their inert defaults.
#[test]
fn default_maker_quotes_mid_off_the_feed_unchanged() {
    let mut mm = SpreadMaker::new(1.0, 0.5);
    assert_eq!(mm.cfg.style, QuoteStyle::Mid, "Mid is the default style");
    assert!(!mm.cfg.filter_own, "filtration is OFF by default");
    let mut b0 = broker(0.0, 1);
    mm.on_quote_tick(&mut b0, &quote(1, 100.0, 100.2));
    let (bid_px, _) = submit_at(&b0, "bid");
    let (ask_px, _) = submit_at(&b0, "ask");
    let mid = 0.5 * (100.0_f64 + 100.2);
    assert_eq!(bid_px.to_bits(), (mid - 0.5).to_bits(), "bid unchanged from the mid formula");
    assert_eq!(ask_px.to_bits(), (mid + 0.5).to_bits(), "ask unchanged from the mid formula");
}

// Own-order filtration removes our resting size and SHIFTS the derived best when we were alone at
// the touch; Join on the filtered book then quotes the GENUINE market, not our own quote.
#[test]
fn own_order_filtration_shifts_the_derived_best() {
    let book = BookView {
        tick_size: 0.5,
        bids: vec![(100.0, 3.0), (99.5, 8.0)],
        asks: vec![(100.5, 2.0), (101.0, 9.0)],
    };
    // we rest the ENTIRE best bid (100.0, size 3) and best ask (100.5, size 2)
    let f = filter_own(&book, Some((100.0, 3.0)), Some((100.5, 2.0)));
    assert_eq!(f.bids.len(), 1, "our whole best-bid level was removed");
    assert_eq!(f.asks.len(), 1, "our whole best-ask level was removed");
    assert_eq!(f.bids[0].0.to_bits(), 99.5_f64.to_bits(), "best bid shifts past our own");
    assert_eq!(f.asks[0].0.to_bits(), 101.0_f64.to_bits(), "best ask shifts past our own");
    let (b, a) = f.priced(QuoteStyle::Join, 0.0, 1).unwrap();
    assert_eq!(b.to_bits(), 99.5_f64.to_bits(), "Join now leans on the genuine next bid");
    assert_eq!(a.to_bits(), 101.0_f64.to_bits(), "Join now leans on the genuine next ask");
}

// When we are NOT alone at a level, filtration only REDUCES its size — the best price is unchanged.
#[test]
fn own_order_filtration_reduces_size_when_not_alone() {
    let book = BookView { tick_size: 0.5, bids: vec![(100.0, 10.0)], asks: vec![(100.5, 10.0)] };
    let f = filter_own(&book, Some((100.0, 4.0)), None);
    assert_eq!(f.bids[0].0.to_bits(), 100.0_f64.to_bits(), "best price unchanged when not alone");
    assert_eq!(f.bids[0].1.to_bits(), 6.0_f64.to_bits(), "our size subtracted from the level");
    assert_eq!(f.asks[0].1.to_bits(), 10.0_f64.to_bits(), "untouched side is unchanged");
}

// Filtration needs a tick grid to match a float price to a level; with an unknown grid it is inert.
#[test]
fn filtration_is_inert_without_a_tick_grid() {
    let book = BookView { tick_size: 0.0, bids: vec![(100.0, 3.0)], asks: vec![(101.0, 3.0)] };
    let f = filter_own(&book, Some((100.0, 3.0)), Some((101.0, 3.0)));
    assert_eq!(f.bids.len(), 1, "no grid → no match → nothing removed (bids)");
    assert_eq!(f.asks.len(), 1, "no grid → no match → nothing removed (asks)");
}

// The style COMPOSES with BOTH the skew (sizes) and the breaker (which sides quote): a `Join`
// maker prices at the touch while the skew still shapes the sizes and the breaker still pulls the
// over-hit side.
#[test]
fn quote_style_composes_with_skew_and_breaker() {
    let mut mm = SpreadMaker::new(2.0, 0.5)
        .with_quote_style(QuoteStyle::Join, 0, 1.0)
        .with_skew(0.0, 4.0, 0.5)
        .with_fill_breaker(1000, 2.5, 5000);
    // long inventory (3): Join prices AT the touch, skew shrinks the bid + grows the ask SIZE
    let mut b0 = broker(3.0, 100);
    mm.on_quote_tick(&mut b0, &quote(100, 100.0, 101.0));
    let (bid_px, bid_qty) = submit_at(&b0, "bid");
    let (ask_px, ask_qty) = submit_at(&b0, "ask");
    assert_eq!(bid_px.to_bits(), 100.0_f64.to_bits(), "Join → bid at the touch");
    assert_eq!(ask_px.to_bits(), 101.0_f64.to_bits(), "Join → ask at the touch");
    assert!(bid_qty < 2.0, "skew still shrinks the bid size when long: {bid_qty}");
    assert!(ask_qty > 2.0, "skew still grows the ask size when long: {ask_qty}");
    // a one-sided bid run trips the breaker → the over-hit bid is pulled under the Join style too
    for t in [110, 120, 130] {
        mm.on_fill(&mut broker(3.0, t), &a_fill(1, 1.0, t));
    }
    let mut b1 = broker(3.0, 140);
    mm.on_quote_tick(&mut b1, &quote(140, 100.0, 101.0));
    assert!(canceled(&b1, "bid"), "breaker pulls the over-hit bid regardless of quote style");
    assert!(modified(&b1, "ask"), "the un-hit ask keeps quoting in place");
}

// On the L2 book lane, `Depth` rests genuinely deeper — one level INTO the book each side.
#[test]
fn on_order_book_depth_style_rests_deeper() {
    let mut mm = SpreadMaker::new(1.0, 0.0).with_quote_style(QuoteStyle::Depth, 1, 0.0);
    let mut book = L2Book::new(0.5);
    book.apply_snapshot(
        1,
        &[(100.0, 5.0), (99.5, 8.0), (99.0, 3.0)],
        &[(100.5, 4.0), (101.0, 9.0), (101.5, 2.0)],
    );
    let mut b0 = broker(0.0, 10);
    mm.on_order_book(&mut b0, &book);
    assert_eq!(submit_at(&b0, "bid").0.to_bits(), 99.5_f64.to_bits(), "Depth 1 into the bids");
    assert_eq!(submit_at(&b0, "ask").0.to_bits(), 101.0_f64.to_bits(), "Depth 1 into the asks");
}

// On the L2 book lane, own-order filtration shifts the best across two updates: once we are the
// whole top level (quoting the feed we consume), the next update leans on the genuine next level
// instead of joining our own quote.
#[test]
fn on_order_book_own_filtration_shifts_best_when_alone() {
    // qty 5.0 == the top level's size, so after we rest there we ARE the whole best level.
    let mut mm = SpreadMaker::new(5.0, 0.0)
        .with_quote_style(QuoteStyle::Join, 0, 0.0)
        .with_own_order_filtration();
    let mut book = L2Book::new(0.5);
    book.apply_snapshot(1, &[(100.0, 5.0), (99.5, 8.0)], &[(100.5, 5.0), (101.0, 8.0)]);
    // update 1: no own orders yet → Join at the touch, rests both
    let mut b0 = broker(0.0, 10);
    mm.on_order_book(&mut b0, &book);
    assert_eq!(submit_at(&b0, "bid").0.to_bits(), 100.0_f64.to_bits(), "first quote joins touch");
    // update 2 (same book still shows our top-of-book): filtration removes our own best level, so
    // Join re-prices onto the genuine next bid (99.5), never joining itself.
    let mut b1 = broker(0.0, 20);
    mm.on_order_book(&mut b1, &book);
    assert_eq!(modify_px(&b1, "bid").to_bits(), 99.5_f64.to_bits(), "filter → don't join our own");
}
