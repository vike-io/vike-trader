use vike_core::LiveBroker;
use vike_model::{BookLevel, L2Book, QuoteStyle, Strategy};

use crate::{OwnSide, SpreadMaker};

/// A bare in-crate `LiveBroker` at a given EVENT ts — mirrors `lib.rs`/`strategy_impl.rs`'s own
/// test helpers of the same shape (this module can't reach those private copies).
fn broker(now: i64) -> LiveBroker {
    LiveBroker {
        positions: Vec::new(),
        prices: Vec::new(),
        bar_views: Vec::new(),
        position: 0.0,
        price: 0.0,
        equity: 0.0,
        bars: std::sync::Arc::new(Vec::new()),
        index: 0,
        now,
        multiplier: 1.0,
        lot_size: 0.0,
        submissions: Vec::new(),
        modifications: Vec::new(),
        cancels: Vec::new(),
        brackets: Vec::new(),
        conditionals: Vec::new(),
        mass_cancel: false,
    }
}

/// The price the tagged order was submitted at.
fn submit_px(b: &LiveBroker, tag: &str) -> f64 {
    let s = b.submissions.iter().find(|s| s.tag.as_deref() == Some(tag)).expect("a submit");
    s.price.expect("limit submit has a price")
}

/// The price the tagged order was re-priced to.
fn modify_px(b: &LiveBroker, tag: &str) -> f64 {
    let m = b.modifications.iter().find(|m| m.tag == tag).expect("a modify");
    m.new_price.expect("re-price has a price")
}

/// Every buffered verb as a comparable `(kind, tag, price, qty)` tape — the equivalence probe.
fn tape(b: &LiveBroker) -> Vec<(&'static str, String, Option<u64>, Option<u64>)> {
    let submits = b.submissions.iter().map(|s| {
        (
            "submit",
            s.tag.clone().unwrap_or_default(),
            s.price.map(f64::to_bits),
            Some(s.qty.to_bits()),
        )
    });
    let modifies = b.modifications.iter().map(|m| {
        ("modify", m.tag.clone(), m.new_price.map(f64::to_bits), m.new_qty.map(f64::to_bits))
    });
    submits.chain(modifies).collect()
}

/// bids `[(100.0, 5.0), (99.5, 8.0)]` / asks `[(100.5, 5.0), (101.0, 8.0)]` on a 0.5 grid —
/// the top level is exactly our quote size, so once we rest there we ARE the whole best level.
fn l2() -> L2Book {
    let mut book = L2Book::new(0.5);
    book.apply_snapshot(
        1,
        &[BookLevel::new(100.0, 5.0), BookLevel::new(99.5, 8.0)],
        &[BookLevel::new(100.5, 5.0), BookLevel::new(101.0, 8.0)],
    );
    book
}

/// A `Join` maker quoting exactly the top level's size, with the ladder on at `buffer`.
fn ladder_maker(buffer: i64) -> SpreadMaker {
    SpreadMaker::new(5.0, 0.0)
        .with_quote_style(QuoteStyle::Join, 0, 0.0)
        .with_own_order_book(0.5, buffer)
}

// THE RACE, end to end. Inside the accepted-buffer our just-placed order is NOT yet echoed by
// the public feed, so filtration must leave the displayed depth alone; once the buffer elapses
// it subtracts and the derived best shifts to the genuine next level. The contrast maker (same
// config, ZERO buffer) shifts immediately — proving the buffer is what makes the difference.
#[test]
fn accepted_buffer_holds_the_public_book_then_subtracts_after_it() {
    let book = l2();
    let mut mm = ladder_maker(1_000);

    // tick 1 @ ts 0: nothing of ours rests yet → Join at the touch
    let mut b0 = broker(0);
    mm.on_order_book(&mut b0, &book);
    assert_eq!(submit_px(&b0, "bid").to_bits(), 100.0_f64.to_bits(), "first quote joins touch");
    assert_eq!(submit_px(&b0, "ask").to_bits(), 100.5_f64.to_bits(), "ask joins the touch too");

    // tick 2 @ ts 500 — INSIDE the buffer (accepted at 0, public at 1000). Our size is not in
    // the feed yet, so subtracting it would invent a phantom-empty level: the maker must still
    // see the full public book and re-quote at the SAME touch.
    let mut b1 = broker(500);
    mm.on_order_book(&mut b1, &book);
    assert_eq!(
        modify_px(&b1, "bid").to_bits(),
        100.0_f64.to_bits(),
        "inside the buffer: public depth is NOT double-subtracted"
    );
    assert_eq!(modify_px(&b1, "ask").to_bits(), 100.5_f64.to_bits(), "same on the ask side");

    // tick 3 @ ts 1000 — the buffer has elapsed, so our order IS assumed public: filtration
    // removes the level we wholly own and Join re-prices onto the genuine next level.
    let mut b2 = broker(1_000);
    mm.on_order_book(&mut b2, &book);
    assert_eq!(
        modify_px(&b2, "bid").to_bits(),
        99.5_f64.to_bits(),
        "buffer elapsed: our own level is subtracted, best shifts"
    );
    assert_eq!(modify_px(&b2, "ask").to_bits(), 101.0_f64.to_bits(), "ask shifts out too");

    // CONTRAST: the identical maker with a ZERO buffer shifts at the very next tick instead.
    let mut zero = ladder_maker(0);
    zero.on_order_book(&mut broker(0), &book);
    let mut z1 = broker(500);
    zero.on_order_book(&mut z1, &book);
    assert_eq!(
        modify_px(&z1, "bid").to_bits(),
        99.5_f64.to_bits(),
        "zero buffer trusts the ack at once — the buffer alone caused the hold above"
    );
}

// A zero-buffer ladder must reproduce the pre-existing single-snapshot filtration EXACTLY: the
// ladder is a strictly richer representation, not a different policy. Bit-for-bit over the
// whole buffered verb tape, across a multi-tick run that moves our order between levels.
#[test]
fn zero_buffer_ladder_matches_the_snapshot_path_bit_for_bit() {
    let book = l2();
    let mut ladder = ladder_maker(0);
    let mut snapshot = SpreadMaker::new(5.0, 0.0)
        .with_quote_style(QuoteStyle::Join, 0, 0.0)
        .with_own_order_filtration();

    for ts in [10, 20, 30, 40] {
        let (mut a, mut b) = (broker(ts), broker(ts));
        ladder.on_order_book(&mut a, &book);
        snapshot.on_order_book(&mut b, &book);
        assert_eq!(tape(&a), tape(&b), "tick {ts}: ladder and snapshot agree bit-for-bit");
    }
}

// The ladder is OFF by default and the snapshot opt-in does NOT turn it on — so both
// pre-existing configurations keep their exact prior code path.
#[test]
fn ladder_is_off_by_default_and_snapshot_filtration_does_not_enable_it() {
    let plain = SpreadMaker::new(1.0, 0.5);
    assert!(plain.own_book().is_none(), "no ladder by default");
    assert!(!plain.params().filter_own, "and filtration is off by default");

    let snapshot = SpreadMaker::new(1.0, 0.5).with_own_order_filtration();
    assert!(snapshot.own_book().is_none(), "snapshot filtration uses no ladder");
    assert!(snapshot.params().filter_own, "but filtration is on");

    // the ladder builder implies filtration — one call is the whole opt-in
    let ladder = ladder_maker(100);
    assert!(ladder.own_book().is_some(), "ladder mounted");
    assert!(ladder.params().filter_own, "and filtration turned on with it");
}

// A runtime driving a REAL venue partial fill through `own_book_mut` shrinks what filtration
// subtracts, so the maker stops treating a level it only partly owns as wholly its own.
#[test]
fn runtime_driven_partial_fill_shrinks_the_subtraction() {
    let book = l2();
    let mut mm = ladder_maker(0);
    mm.on_order_book(&mut broker(0), &book);
    assert_eq!(
        mm.own_book().and_then(|f| f.book.get("bid")).map(|o| o.qty.to_bits()),
        Some(5.0_f64.to_bits()),
        "the maker's own bid is tracked at full size"
    );

    // the venue partially fills 2.0 of our 5.0 — a runtime folds it into the ladder
    assert!(
        mm.own_book_mut().expect("ladder mounted").book.on_partial_fill("bid", 2.0),
        "the tagged order is known to the ladder"
    );

    // now only 3.0 of the 5.0 top level is ours, so 2.0 of genuine market depth remains: the
    // level SURVIVES filtration and Join keeps quoting the touch (without the fill it would
    // have been wholly ours and dropped, shifting the quote to 99.5).
    let mut b1 = broker(10);
    mm.on_order_book(&mut b1, &book);
    assert_eq!(
        modify_px(&b1, "bid").to_bits(),
        100.0_f64.to_bits(),
        "partial fill leaves real depth at the level, so the best does not shift"
    );
}

// A ladder built on an unknown (non-positive) grid is INERT — it tracks nothing and subtracts
// nothing, so the maker prices straight off the feed. The same no-grid rule the snapshot path
// already follows, and the documented footgun of the ladder's fixed grid.
#[test]
fn inert_no_grid_ladder_subtracts_nothing() {
    let book = l2();
    let mut mm = SpreadMaker::new(5.0, 0.0)
        .with_quote_style(QuoteStyle::Join, 0, 0.0)
        .with_own_order_book(0.0, 0);

    mm.on_order_book(&mut broker(0), &book);
    assert!(mm.own_book().expect("mounted").book.is_empty(), "no grid ⇒ nothing tracked");

    let mut b1 = broker(10);
    mm.on_order_book(&mut b1, &book);
    assert_eq!(
        modify_px(&b1, "bid").to_bits(),
        100.0_f64.to_bits(),
        "an inert ladder never subtracts — the maker keeps joining the raw touch"
    );
}

// A suppression PULL removes the quote from the ladder, so the depth we no longer have resting
// stops being subtracted from the public book.
#[test]
fn a_pulled_quote_leaves_the_ladder() {
    let book = l2();
    // breaker armed so a single 5.0 bid fill trips the bid side and pulls that quote
    let mut mm = SpreadMaker::new(5.0, 0.0)
        .with_quote_style(QuoteStyle::Join, 0, 0.0)
        .with_own_order_book(0.5, 0)
        .with_fill_breaker(1_000, 2.5, 5_000);

    mm.on_order_book(&mut broker(0), &book);
    assert!(mm.own_book().expect("mounted").book.contains("bid"), "bid rests in the ladder");

    // a big same-side fill trips the bid breaker; the next tick pulls that side
    let fill = vike_model::Fill {
        side: 1,
        size: 5.0,
        price: 100.0,
        fee: 0.0,
        ts: 10,
        is_maker: true,
        symbol: String::new(),
    };
    Strategy::<LiveBroker>::on_fill(&mut mm, &mut broker(10), &fill);

    let mut b1 = broker(20);
    mm.on_order_book(&mut b1, &book);
    assert!(b1.cancels.iter().any(|t| t == "bid"), "the suppressed side is canceled");
    assert!(
        !mm.own_book().expect("mounted").book.contains("bid"),
        "and it leaves the ladder, so its size is no longer subtracted"
    );
    // the ask side is untouched by the bid-side pull — it still rests and is still tracked.
    // NOTE it has RE-PRICED 100.5 → 101.0: unsuppressed, it re-quoted this tick, and with the
    // zero buffer its own 5.0 at 100.5 was subtracted, wholly emptying that level so `Join`
    // moved to the next genuine ask. So its ladder entry moved levels with it.
    let own = mm.own_book().expect("mounted");
    assert!(own.book.contains("ask"), "the ask still rests");
    assert_eq!(
        own.book.qty_at(OwnSide::Ask, 101.0, &own.filter_at(20)).to_bits(),
        5.0_f64.to_bits(),
        "the ask's own size is tracked at the level it re-priced onto"
    );
    assert_eq!(
        own.book.qty_at(OwnSide::Ask, 100.5, &own.filter_at(20)).to_bits(),
        0.0_f64.to_bits(),
        "and nothing is left behind at the level it vacated"
    );
}
