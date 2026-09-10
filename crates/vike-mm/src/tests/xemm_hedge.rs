//! The xEMM HEDGE lane — the #997 regression surface, and the most dangerous code in the crate.
//!
//! A maker fill on venue A creates a DIRECTIONAL exposure that only an order on venue B can close.
//! Every test here pins one way that can go wrong in a way no other test would catch:
//!
//! - the hedge must NAME its symbol and carry NO tag, because the runtime resolves a declared leg's
//!   venue BY SYMBOL and a tagged submit carries `symbol: None` — a tagged hedge would be placed on
//!   the MAKER venue, DOUBLING the exposure it was sent to close;
//! - a hedge-leg fill drains on the TAKER venue's series, so a tagged verb from that lane looks up
//!   the wrong tag-registry key and silently leaves a real quote resting on the maker venue;
//! - a retry must re-send the RESIDUAL, not a remembered delta, or a late original fill doubles the
//!   position while the maker believes it is flat.

use super::*;
use crate::XemmMaker;

const MAKER: &str = "BTC"; // the mount's own symbol, on the maker venue
const HEDGE: &str = "BTC-USDT-SWAP"; // the declared leg, on the taker venue
const REF_VENUE: &str = "okx";

/// A reference (taker-venue) touch.
fn ref_q(ts: i64, bid: f64, ask: f64) -> QuoteTick {
    QuoteTick { ts, local_ts: 0, bid, ask, bid_size: 1.0, ask_size: 1.0, symbol: HEDGE.into() }
}

/// A maker-venue touch.
fn own_q(ts: i64, bid: f64, ask: f64) -> QuoteTick {
    QuoteTick { ts, local_ts: 0, bid, ask, bid_size: 1.0, ask_size: 1.0, symbol: MAKER.into() }
}

fn fill_of(symbol: &str, side: i32, size: f64, ts: i64) -> Fill {
    Fill { side, size, price: 100.0, fee: 0.0, ts, is_maker: true, symbol: symbol.into() }
}

/// A maker with the naked bands opened wide, so the HEDGE mechanics are what is under test rather
/// than the exposure guards (those get their own file). 10 bp edge, 6.5 bp fee, 0.01 grid.
fn maker() -> XemmMaker {
    XemmMaker::new(MAKER, HEDGE, 1.0, 0.001, 0.00065, 0.01).with_naked_bands(1e9, 1e9)
}

/// Feed both venues' touches so the maker is warm and quoting: reference 100.00/100.50, maker
/// venue 100.00/100.02 (a tight local book straddling the reference bid).
fn warm(m: &mut XemmMaker, ts: i64) {
    m.on_reference_quote(&mut broker(0.0, ts), REF_VENUE, &ref_q(ts, 100.0, 100.5));
    m.on_quote_tick(&mut broker(0.0, ts), &own_q(ts, 100.0, 100.02));
}

/// Every UNTAGGED submission as `(symbol, side, qty)` — the hedge/flatten orders. Tagged ones are
/// the maker's own resting quotes and are excluded.
fn hedges(b: &LiveBroker) -> Vec<(Option<String>, i32, f64)> {
    b.submissions
        .iter()
        .filter(|s| s.tag.is_none())
        .map(|s| (s.symbol.clone(), s.side, s.qty))
        .collect()
}

// --- H-1 / H-5: the hedge-leg lane may touch NOTHING but the symbol-carrying market verb --------

/// THE SINGLE MOST IMPORTANT TEST IN THE FILE. A fill on the HEDGE leg is dispatched — and drained
/// — on the TAKER venue's series. From there:
///
/// - a TAGGED verb keys `{mount}|TAKER_VENUE|HEDGE_SYM|bid`, which the maker's own quotes were
///   never registered under, so `cancel_tagged` becomes a silent no-op that leaves a REAL order
///   resting on the maker venue;
/// - a SYMBOL-LESS submit resolves to the hedge symbol and is placed on the TAKER venue.
///
/// So this lane may emit exactly one shape: a symbol-carrying, untagged market order.
#[test]
fn a_hedge_leg_fill_emits_only_symbol_carrying_untagged_orders() {
    let mut m = maker();
    warm(&mut m, 1_000);
    // A maker BUY of 5 owes a hedge SELL of 5; let it fire, then partially fill it.
    m.on_fill(&mut broker(0.0, 1_100), &fill_of(MAKER, 1, 5.0, 1_100));
    let mut b = broker(0.0, 1_200);
    m.on_fill(&mut b, &fill_of(HEDGE, -1, 2.0, 1_200));

    assert!(
        b.cancels.is_empty(),
        "a hedge-leg fill must never cancel a tagged quote: {:?}",
        b.cancels
    );
    assert!(
        b.modifications.is_empty(),
        "a hedge-leg fill must never re-price a tagged quote: {:?}",
        modifies_dbg(&b)
    );
    for s in &b.submissions {
        assert!(
            s.tag.is_none(),
            "a hedge-leg fill buffered a TAGGED submission ({:?}) — it would be placed on the \
             taker venue and be uncancellable from the maker lane",
            s.tag
        );
        assert_eq!(
            s.symbol.as_deref(),
            Some(HEDGE),
            "every order from the hedge lane must NAME the hedge symbol; a symbol-less one \
             resolves to whatever series dispatched it"
        );
    }
}

// --- H-2: a maker fill fires exactly one hedge, correctly sized and sided ------------------------

/// The core claim: one maker fill ⇒ one market order on the HEDGE symbol, opposite side, sized
/// bit-for-bit by `hedge_qty(size, ratio)`.
#[test]
fn a_maker_fill_fires_exactly_one_market_hedge_naming_the_hedge_symbol() {
    let mut m = maker();
    warm(&mut m, 1_000);
    let mut b = broker(0.0, 1_100);
    m.on_fill(&mut b, &fill_of(MAKER, 1, 5.0, 1_100));

    let h = hedges(&b);
    assert_eq!(h.len(), 1, "exactly one hedge order, got {h:?}");
    let (symbol, side, qty) = &h[0];
    assert_eq!(symbol.as_deref(), Some(HEDGE), "the hedge NAMES its leg — that is what routes it");
    assert_eq!(*side, -1, "a maker BUY is hedged by a taker SELL");
    assert_eq!(qty.to_bits(), 5.0_f64.to_bits(), "fully hedged at ratio 1.0, bit-for-bit");
    assert_eq!(m.leg_positions(), (5.0, 0.0), "the ledger books the maker leg immediately");
    assert_eq!(
        m.naked_exposure().to_bits(),
        5.0_f64.to_bits(),
        "and reports it as naked until B fills"
    );
    // the mirror: a maker SELL is hedged by a taker BUY.
    let mut m2 = maker();
    warm(&mut m2, 1_000);
    let mut b2 = broker(0.0, 1_100);
    m2.on_fill(&mut b2, &fill_of(MAKER, -1, 2.5, 1_100));
    assert_eq!(hedges(&b2), vec![(Some(HEDGE.to_string()), 1, 2.5)]);
}

// --- H-3: partial hedge fills leave only the residual -------------------------------------------

/// A partially-filled hedge owes the REMAINDER on its next send, and nothing once it completes.
#[test]
fn a_partial_hedge_fill_leaves_only_the_residual_owed() {
    let mut m = maker();
    warm(&mut m, 1_000);
    m.on_fill(&mut broker(0.0, 1_100), &fill_of(MAKER, 1, 5.0, 1_100)); // fires SELL 5
    m.on_fill(&mut broker(0.0, 1_200), &fill_of(HEDGE, -1, 2.0, 1_200)); // 2 of 5 land
    assert_eq!(m.naked_exposure().to_bits(), 3.0_f64.to_bits(), "3 still naked");
    // the retry (after the timeout) sends exactly the 3 that remain — never the original 5.
    let mut b = broker(0.0, 10_000);
    m.on_schedule(&mut b, "sweep");
    let h = hedges(&b);
    assert_eq!(h, vec![(Some(HEDGE.to_string()), -1, 3.0)], "the retry re-sends the RESIDUAL");
    // completing it settles: nothing further is sent, ever.
    m.on_fill(&mut broker(0.0, 10_100), &fill_of(HEDGE, -1, 3.0, 10_100));
    let mut b2 = broker(0.0, 20_000);
    m.on_schedule(&mut b2, "sweep");
    assert!(hedges(&b2).is_empty(), "a settled hedge sends nothing: {:?}", hedges(&b2));
    assert_eq!(m.naked_exposure().to_bits(), 0.0_f64.to_bits(), "and reports flat");
}

// --- H-4: THE TARGET-NOT-DELTA LAW --------------------------------------------------------------

/// Fire, time out, retry — and THEN have the ORIGINAL land late. A delta ledger would have
/// decremented once and sent twice, doubling the position while reporting flat. Because the residual
/// is DERIVED from the two legs' positions, the late fill shrinks it and the retry cannot double
/// anything.
#[test]
fn a_hedge_retry_never_doubles_the_position() {
    let mut m = maker();
    warm(&mut m, 1_000);
    let mut b0 = broker(0.0, 1_100);
    m.on_fill(&mut b0, &fill_of(MAKER, 1, 5.0, 1_100));
    assert_eq!(hedges(&b0), vec![(Some(HEDGE.to_string()), -1, 5.0)], "fire #1");

    // the ack never comes; the safety sweep re-fires the same residual after the timeout.
    let mut b1 = broker(0.0, 10_000);
    m.on_schedule(&mut b1, "sweep");
    assert_eq!(hedges(&b1), vec![(Some(HEDGE.to_string()), -1, 5.0)], "fire #2 — same residual");

    // NOW the original lands, late.
    m.on_fill(&mut broker(0.0, 10_100), &fill_of(HEDGE, -1, 5.0, 10_100));
    assert_eq!(m.naked_exposure().to_bits(), 0.0_f64.to_bits(), "the late fill settles the target");

    // a third sweep sends NOTHING: there is no third order to double with.
    let mut b2 = broker(0.0, 20_000);
    m.on_schedule(&mut b2, "sweep");
    assert!(
        hedges(&b2).is_empty(),
        "a settled ledger must send nothing — a delta ledger would still owe 5 here: {:?}",
        hedges(&b2)
    );

    // and if the RETRY's own fill also lands, the over-hedge is owed BACK rather than ignored.
    m.on_fill(&mut broker(0.0, 20_100), &fill_of(HEDGE, -1, 5.0, 20_100));
    let mut b3 = broker(0.0, 30_000);
    m.on_schedule(&mut b3, "sweep");
    assert_eq!(
        hedges(&b3),
        vec![(Some(HEDGE.to_string()), 1, 5.0)],
        "an over-hedge is unwound, not silently carried"
    );
}

// --- H-6: an unattributable fill fails CLOSED ---------------------------------------------------

/// A fill for NEITHER leg means the maker can no longer account for its own inventory. It halts —
/// but emits NOTHING from that hook, because the dispatch series of an unattributable fill is
/// unknown and a tagged cancel from the wrong series is a silent no-op. The pull lands on the next
/// maker-venue lane.
#[test]
fn a_fill_for_neither_leg_halts_and_the_next_tick_pulls() {
    let mut m = maker();
    warm(&mut m, 1_000);
    let mut b = broker(0.0, 1_100);
    assert!(submitted(&b, "bid") || m.halt_reason().is_none(), "precondition: running");
    m.on_fill(&mut b, &fill_of("DOGEUSDT", 1, 1.0, 1_100));
    assert_eq!(m.halt_reason(), Some(crate::HaltReason::UnknownFillSymbol));
    assert!(
        b.submissions.is_empty() && b.cancels.is_empty(),
        "the unknown-fill hook emits nothing"
    );
    // the next maker-venue tick takes both quotes off.
    let mut b2 = broker(0.0, 1_200);
    m.on_quote_tick(&mut b2, &own_q(1_200, 100.0, 100.02));
    assert!(canceled(&b2, "bid") && canceled(&b2, "ask"), "the halt pulls both sides");
}

/// A fill with an EMPTY symbol is the MAKER leg, following the `""` means "my mount" convention the
/// order verbs use. Live fills always carry a real symbol, so this only affects test/paper doubles —
/// but reading it as "unknown" would halt every paper rehearsal.
#[test]
fn an_empty_fill_symbol_is_the_maker_leg() {
    let mut m = maker();
    warm(&mut m, 1_000);
    let mut b = broker(0.0, 1_100);
    m.on_fill(&mut b, &fill_of("", 1, 5.0, 1_100));
    assert_eq!(m.halt_reason(), None, "an empty symbol is not an unknown one");
    assert_eq!(hedges(&b), vec![(Some(HEDGE.to_string()), -1, 5.0)], "and it hedges normally");
}

// --- H-7 / H-8: the two ways hedging stops -------------------------------------------------------

/// `hedge_ratio == 0.0` is the deliberately-unhedged configuration: it fires NO order rather than a
/// zero-size one. The exposure is still fully visible, and the naked bands still measure it.
#[test]
fn a_zero_hedge_ratio_fires_nothing_but_still_reports_the_exposure() {
    let mut m = maker().with_naked_bands(1e9, 1e9);
    m.apply_params(&vike_model::XemmParams { hedge_ratio: 0.0, ..m.params() });
    warm(&mut m, 1_000);
    let mut b = broker(0.0, 1_100);
    m.on_fill(&mut b, &fill_of(MAKER, 1, 5.0, 1_100));
    assert!(hedges(&b).is_empty(), "ratio 0 ⇒ no hedge order: {:?}", hedges(&b));
    assert_eq!(m.naked_exposure().to_bits(), 5.0_f64.to_bits(), "the exposure is fully visible");
}

/// A residual that survives its whole retry budget is a REAL open position the taker venue has
/// refused to close. The maker halts (`HedgeUnfilled`) and pulls both quotes rather than continuing
/// to accumulate on top of it.
#[test]
fn attempt_exhaustion_halts_and_pulls() {
    // Freshness opened wide: this test drives the clock far ahead to expire the hedge timeout, and
    // a stale-feed halt would mask the `HedgeUnfilled` one under test.
    let mut m = maker().with_hedge_discipline(1_000, 2, 0.0).with_freshness(0, 0, 0);
    warm(&mut m, 1_000);
    m.on_fill(&mut broker(0.0, 1_100), &fill_of(MAKER, 1, 5.0, 1_100)); // send #1
    let mut b1 = broker(0.0, 5_000);
    m.on_schedule(&mut b1, "sweep"); // send #2 — budget now spent
    assert_eq!(hedges(&b1).len(), 1, "the second send happens");
    assert_eq!(m.halt_reason(), None, "and does not itself halt");

    let mut b2 = broker(0.0, 9_000);
    m.on_schedule(&mut b2, "sweep");
    assert!(hedges(&b2).is_empty(), "no third send: the budget is spent");
    assert_eq!(m.halt_reason(), Some(crate::HaltReason::HedgeUnfilled));
    assert!(canceled(&b2, "bid") && canceled(&b2, "ask"), "and both quotes come off");
}
