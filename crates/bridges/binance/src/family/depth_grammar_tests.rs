use super::*;

/// `wss://fstream.binance.com/ws/btcusdt@depth@100ms`, captured 2026-09-10 alongside the
/// `fapi/v1/depth` snapshot below, in the driver's own order: dial, buffer, then seed.
const FUT_SEED_LAST_UPDATE_ID: u64 = 11_522_761_644_988;
/// The FIRST post-seed futures diff: it STRADDLES the snapshot (`U <= lastUpdateId <= u`) and
/// its `pu` points at an event that predates the snapshot, so nothing can chain onto the seed.
const FUT_1: (u64, u64, u64) = (11_522_761_639_921, 11_522_761_649_439, 11_522_761_639_668);
/// The SECOND — the frame the old spot rule read as a gap on every single session.
const FUT_2: (u64, u64, u64) = (11_522_761_649_867, 11_522_761_660_049, 11_522_761_649_439);
/// The THIRD, so the chain is proven past one link.
const FUT_3: (u64, u64, u64) = (11_522_761_660_132, 11_522_761_675_787, 11_522_761_660_049);

/// `wss://sstream.asterdex.com/ws/btcusdt@depth@100ms` — aster's SPOT plane, which carries `pu`
/// like a futures stream and is non-contiguous in `U`.
const ASTER_SPOT_1: (u64, u64, u64) = (6_364_062_904, 6_364_062_911, 6_364_062_859);
const ASTER_SPOT_2: (u64, u64, u64) = (6_364_062_966, 6_364_063_001, 6_364_062_911);

/// `wss://stream.binance.com:9443/ws/btcusdt@depth@100ms` — binance SPOT, no `pu`, and `U` IS
/// the previous `u + 1`.
const SPOT_1: (u64, u64) = (99_940_725_260, 99_940_725_373);
const SPOT_2: (u64, u64) = (99_940_725_374, 99_940_726_242);

/// A futures-shaped `depthUpdate` payload (the `data` object `route_frame` hands on).
fn futures_frame((first_u, final_u, prev_u): (u64, u64, u64), bid_qty: f64) -> Value {
    serde_json::json!({
        "e": "depthUpdate", "E": 1_757_500_000_000_i64, "s": "BTCUSDT",
        "U": first_u, "u": final_u, "pu": prev_u,
        "b": [["60000.0", bid_qty.to_string()]], "a": []
    })
}

/// The spot twin — the SAME payload minus `pu`, which is the whole of the difference.
fn spot_frame((first_u, final_u): (u64, u64), bid_qty: f64) -> Value {
    serde_json::json!({
        "e": "depthUpdate", "E": 1_757_500_000_000_i64, "s": "BTCUSDT",
        "U": first_u, "u": final_u,
        "b": [["60000.0", bid_qty.to_string()]], "a": []
    })
}

fn seeded(seq: u64) -> L2Book {
    let mut b = L2Book::new(0.1);
    b.apply_snapshot(seq, &[BookLevel::new(60000.0, 1.0)], &[BookLevel::new(60001.0, 1.0)]);
    b
}

/// The best bid's QUANTITY — every fixture writes its own qty at one price, so this is what
/// says which diff (if any) actually folded.
fn best_bid_qty(book: &L2Book) -> f64 {
    book.best_bid().expect("the seeded book always has a bid").qty
}

/// **THE DEFECT.** A real USDⓈ-M seed followed by its real first three diffs must fold all
/// three. Before the `pu` arm, diff two answered [`DepthOutcome::Gap`] — which
/// `super::market_feed`'s `depth_main` maps to `BookOp::Gap`, which
/// `vike_bridge_core::depth::run_depth_session` returns as `Err`, which `run_depth_feed` pays
/// `DEPTH_BACKOFF` for and re-seeds. Forever: 0.41 updates/s recorded into
/// `kind=depth/venue=binance/symbol=BTCUSDT.P` for forty days against the ~10/s the stream
/// actually carries.
///
/// The middle assertion is what stops this test from being satisfiable by a friendlier fixture:
/// diff two must genuinely VIOLATE the spot rule, or it proves nothing about the futures one.
///
/// MUTATION PROOF: delete the `Some(prev_final_u) =>` arm of `apply_depth_event`'s `contiguous`
/// match (so both grammars take the spot test) and this goes red on diff two.
#[test]
fn the_second_real_futures_diff_after_a_seed_folds_instead_of_gapping() {
    let mut book = seeded(FUT_SEED_LAST_UPDATE_ID);

    assert_eq!(
        apply_depth_event(&mut book, &futures_frame(FUT_1, 2.0)),
        DepthOutcome::Applied,
        "the first post-seed futures diff straddles lastUpdateId and must fold"
    );
    assert_eq!(book.last_seq, FUT_1.1);

    assert!(
        FUT_2.0 > book.last_seq + 1,
        "this fixture must VIOLATE the spot contiguity rule ({} vs {}), or it certifies \
             nothing about the futures grammar",
        FUT_2.0,
        book.last_seq + 1
    );
    assert_eq!(
        apply_depth_event(&mut book, &futures_frame(FUT_2, 3.0)),
        DepthOutcome::Applied,
        "a futures diff whose `pu` chains onto the applied anchor is CONTIGUOUS, not a gap — \
             this is the frame that reconnect-looped the binance perp depth lane for forty days"
    );
    assert_eq!(book.last_seq, FUT_2.1);

    assert_eq!(
        apply_depth_event(&mut book, &futures_frame(FUT_3, 4.0)),
        DepthOutcome::Applied,
        "the chain must hold past one link"
    );
    assert_eq!(book.last_seq, FUT_3.1);
    assert_eq!(best_bid_qty(&book).to_bits(), 4.0_f64.to_bits(), "the last diff's qty won");
}

/// …and the arm is not a hole: a genuinely DROPPED futures frame still gaps, because the
/// survivor's `pu` names the frame that vanished AND its span starts past the anchor.
///
/// MUTATION PROOF: relax arm two to `first_u <= book.last_seq + 1` (the spot span test) and
/// this still passes; relax it to an unconditional `true` and it goes red — which is why the
/// assertion below also pins that the book was NOT mutated.
#[test]
fn a_dropped_futures_frame_still_gaps() {
    let mut book = seeded(FUT_SEED_LAST_UPDATE_ID);
    assert_eq!(apply_depth_event(&mut book, &futures_frame(FUT_1, 2.0)), DepthOutcome::Applied);
    let anchor = book.last_seq;

    // FUT_2 never arrives. FUT_3's `pu` is FUT_2's `u`, not the anchor, and its span opens
    // above the anchor — a real hole, and the caller must re-snapshot.
    assert_eq!(
        apply_depth_event(&mut book, &futures_frame(FUT_3, 9.0)),
        DepthOutcome::Gap,
        "a broken `pu` chain over a forward span is a real gap and must still resync"
    );
    assert_eq!(book.last_seq, anchor, "a gapped frame must not move the anchor");
    assert_eq!(best_bid_qty(&book).to_bits(), 2.0_f64.to_bits(), "…nor fold into the book");
}

/// A futures frame entirely at or below the anchor is STALE, not a gap — that arm is what drops
/// the WS backlog buffered while the REST seed was in flight, and it must stay ahead of the
/// grammar split (it is exactly what `L2Book::apply_delta` would refuse anyway, so admitting it
/// would make `Applied` a lie).
#[test]
fn a_pre_seed_futures_frame_is_stale_not_a_gap() {
    let mut book = seeded(FUT_SEED_LAST_UPDATE_ID);
    let backlog = (FUT_SEED_LAST_UPDATE_ID - 900, FUT_SEED_LAST_UPDATE_ID - 400, FUT_1.2);
    assert_eq!(apply_depth_event(&mut book, &futures_frame(backlog, 7.0)), DepthOutcome::Stale);
    assert_eq!(book.last_seq, FUT_SEED_LAST_UPDATE_ID);
}

/// **Aster's SPOT plane speaks the futures grammar** — 100/100 frames carry `pu` and `U` is the
/// previous `u + 1` on only 18/99 (measured with the same probe, same box, same day). So the
/// venue that was described as "Binance-verbatim" was mis-synced on the plane nobody suspected,
/// and the frame-shape discriminator fixes it with no aster-specific code at all.
#[test]
fn a_real_aster_spot_frame_chains_on_pu_like_a_futures_stream() {
    let mut book = seeded(ASTER_SPOT_1.1);
    assert!(
        ASTER_SPOT_2.0 > book.last_seq + 1,
        "the aster SPOT fixture must violate the spot rule too, or it proves nothing"
    );
    assert_eq!(
        apply_depth_event(&mut book, &futures_frame(ASTER_SPOT_2, 5.0)),
        DepthOutcome::Applied,
        "aster spot chains on `pu`; reading it as binance-spot gapped it every cycle"
    );
    assert_eq!(book.last_seq, ASTER_SPOT_2.1);
}

/// The SPOT grammar is BYTE-IDENTICAL to before: a frame with no `pu` takes the unchanged
/// `U <= last_seq + 1` test, in all three of its outcomes.
#[test]
fn the_spot_grammar_is_unchanged_when_no_pu_is_present() {
    let mut book = seeded(SPOT_1.1);

    // contiguous — `U` is exactly the previous `u + 1`, which is what binance spot really sends
    assert_eq!(SPOT_2.0, SPOT_1.1 + 1, "the captured spot pair really is contiguous");
    assert_eq!(apply_depth_event(&mut book, &spot_frame(SPOT_2, 6.0)), DepthOutcome::Applied);
    assert_eq!(book.last_seq, SPOT_2.1);

    // stale — a replayed frame entirely at or below the anchor
    assert_eq!(apply_depth_event(&mut book, &spot_frame(SPOT_1, 6.0)), DepthOutcome::Stale);
    assert_eq!(book.last_seq, SPOT_2.1);

    // gap — a spot span that opens past `u + 1`. ⚠ The SAME ids with a `pu` naming the anchor
    // would now APPLY, which is the whole point of the discriminator.
    let anchor = book.last_seq;
    let jumped = (anchor + 50, anchor + 90);
    assert_eq!(apply_depth_event(&mut book, &spot_frame(jumped, 6.0)), DepthOutcome::Gap);
    assert_eq!(
        apply_depth_event(&mut book, &futures_frame((jumped.0, jumped.1, anchor), 6.0)),
        DepthOutcome::Applied,
        "the discriminator must actually discriminate — the same span with a chaining `pu` is \
             contiguous, or this whole change is inert"
    );
}
