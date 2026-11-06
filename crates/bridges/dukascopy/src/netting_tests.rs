use super::*;

fn fill(side: i32, qty: f64, px: f64) -> FillEvent {
    fill_sym("EURUSD", side, qty, px)
}

/// `fill` with the symbol as a parameter — the shadow book is keyed per symbol, so a
/// cross-symbol test needs to fold onto a second key.
fn fill_sym(symbol: &'static str, side: i32, qty: f64, px: f64) -> FillEvent {
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

fn legs(r: Reanchor) -> Vec<FillEvent> {
    match r {
        Reanchor::Corrected(events) => events
            .into_iter()
            .map(|e| match e {
                Event::Fill(f) => f,
                other => panic!("re-anchor legs must be bare fills, got {other:?}"),
            })
            .collect(),
        other => panic!("expected Corrected, got {other:?}"),
    }
}

/// Drive one book to the brief's A/B state and return its re-anchor legs. Used to replay the
/// IDENTICAL history on a FRESH book — i.e. to simulate a process restart.
fn ab_scenario_legs() -> Vec<FillEvent> {
    let mut book = ShadowBook::default();
    book.fold_fill(&fill(-1, 1000.0, 100.0));
    book.fold_fill(&fill(-1, 1000.0, 110.0));
    book.fold_fill(&fill(1, 1000.0, 108.0));
    legs(book.reanchor("EURUSD", -1000.0, 110.0, 3))
}

/// THE RESTART COLLISION. The id used to be `NETRA-{symbol}:{seq}` off `ShadowBook.seq`, an
/// IN-PROCESS counter that restarts at 0 every process — while the engine's `seen_trade_ids` does
/// NOT (it is restored from `EngineSnapshot::seen_trade_ids`; the GUI's local core, `vike-app`'s,
/// also seeded it from `vike_data::exec_index::recent_seen_trade_ids` until that core went). So
/// session A minted `NETRA-EURUSD:1` and
/// journalled it; after a restart session B minted `NETRA-EURUSD:1` for a genuinely DIFFERENT
/// attribution difference and the engine DROPPED it as a replay. That is the mirror image of the
/// empty-id defect: not a duplicate waved through, but two distinct fills collapsed into one —
/// and it is silent, because the shadow book has no dedup of its own (module doc) so it folds the
/// leg the `Account` refused, and the two diverge until a `SizeMismatch` that is never auto-healed.
///
/// The fix makes the id a pure function of the leg's own content, so this test asserts BOTH
/// halves: the same frame re-derives the same id (dedup still works across a restart), and a
/// different re-anchor gets a different id (distinct fills stay distinct).
#[test]
fn a_reanchor_id_is_stable_across_a_restart_and_distinct_between_different_reanchors() {
    // Two independent books = two process lifetimes over the same venue history.
    let first = ab_scenario_legs();
    let second = ab_scenario_legs();
    let ids = |v: &[FillEvent]| v.iter().map(|f| f.trade_id.to_string()).collect::<Vec<_>>();
    assert_eq!(
        ids(&first),
        ids(&second),
        "a re-anchor id must be REPLAY-STABLE: the same venue position frame must re-derive the \
             same trade_id after a restart, or the engine's dedup cannot recognise it"
    );

    // ...and the two legs of one pair stay distinct. They are only ever emitted when the sizes
    // already agree, so close and open share qty AND px — `side` is what separates them.
    assert_ne!(first[0].trade_id, first[1].trade_id, "close and open legs must differ");

    // A DIFFERENT re-anchor (different venue basis) must not collide with the first.
    let mut other = ShadowBook::default();
    other.fold_fill(&fill(-1, 1000.0, 100.0));
    other.fold_fill(&fill(-1, 1000.0, 120.0));
    other.fold_fill(&fill(1, 1000.0, 108.0));
    let different = legs(other.reanchor("EURUSD", -1000.0, 120.0, 3));
    for a in &first {
        for b in &different {
            assert_ne!(
                a.trade_id, b.trade_id,
                "two genuinely different re-anchors must never share a trade_id — the engine \
                     would drop the second as a replay and silently desync the shadow"
            );
        }
    }

    // The counter is gone: nothing about the id may depend on how many legs a session has minted.
    // Folding an unrelated symbol's re-anchor FIRST must not shift EURUSD's ids.
    let mut interleaved = ShadowBook::default();
    interleaved.fold_fill(&fill_sym("USDJPY", -1, 1000.0, 150.0));
    let _ = interleaved.reanchor("USDJPY", -1000.0, 155.0, 1);
    interleaved.fold_fill(&fill(-1, 1000.0, 100.0));
    interleaved.fold_fill(&fill(-1, 1000.0, 110.0));
    interleaved.fold_fill(&fill(1, 1000.0, 108.0));
    assert_eq!(
        ids(&legs(interleaved.reanchor("EURUSD", -1000.0, 110.0, 3))),
        ids(&first),
        "an id must not depend on session-local leg ORDER — that was the counter's whole defect"
    );
}

/// The brief's A/B example: short 1000@100 (A) + 1000@110 (B) → blend 105; buy 1000@108
/// netted against A. Venue truth: realized −8/unit, remainder 1000 short @110. The re-anchor
/// legs close 1000@110 (realizing the −5/unit attribution difference on top of the blend's
/// −3/unit) and reopen 1000 short @110.
#[test]
fn partial_netted_close_reanchors_to_the_venue_basis() {
    let mut book = ShadowBook::default();
    book.fold_fill(&fill(-1, 1000.0, 100.0));
    assert_eq!(book.reanchor("EURUSD", -1000.0, 100.0, 1), Reanchor::InSync);
    book.fold_fill(&fill(-1, 1000.0, 110.0));
    // Signed-weighted venue avg of two whole orders == the blend: still in sync.
    assert_eq!(book.reanchor("EURUSD", -2000.0, 105.0, 2), Reanchor::InSync);
    // The netted close leg (buy 1000@108) — blend realizes at 105, venue at A's 100.
    book.fold_fill(&fill(1, 1000.0, 108.0));
    let legs = legs(book.reanchor("EURUSD", -1000.0, 110.0, 3));
    assert_eq!(legs.len(), 2);
    // Close the blended short 1000@105 at 110 → realized (110−105)·(−1000) = −5000.
    assert_eq!((legs[0].side, legs[0].last_qty, legs[0].last_px), (1, 1000.0, 110.0));
    // Reopen short 1000 at the venue's true remaining basis 110.
    assert_eq!((legs[1].side, legs[1].last_qty, legs[1].last_px), (-1, 1000.0, 110.0));
    assert_ne!(legs[0].trade_id, legs[1].trade_id);
    assert!(legs[0].trade_id.starts_with("NETRA-EURUSD:"), "{}", legs[0].trade_id);
    // Idempotent: the shadow now sits at the venue truth; the same line is a no-op.
    assert_eq!(book.reanchor("EURUSD", -1000.0, 110.0, 4), Reanchor::InSync);
}

/// A FULL netted close has no attribution drift (the blend's total equals the per-order
/// sum), so the flat position line must synthesize nothing.
#[test]
fn full_close_is_in_sync_flat() {
    let mut book = ShadowBook::default();
    book.fold_fill(&fill(-1, 1000.0, 100.0));
    book.fold_fill(&fill(-1, 1000.0, 110.0));
    book.fold_fill(&fill(1, 2000.0, 108.0));
    assert_eq!(book.reanchor("EURUSD", 0.0, 0.0, 3), Reanchor::InSync);
}

/// The long-side mirror: long 1000@100 + 1000@110, sell 1000@108 netted against the @100
/// order → venue remainder long 1000@110; the close leg realizes +5000 over the blend.
#[test]
fn long_side_mirror_reanchors() {
    let mut book = ShadowBook::default();
    book.fold_fill(&fill(1, 1000.0, 100.0));
    book.fold_fill(&fill(1, 1000.0, 110.0));
    book.fold_fill(&fill(-1, 1000.0, 108.0));
    let legs = legs(book.reanchor("EURUSD", 1000.0, 110.0, 3));
    assert_eq!((legs[0].side, legs[0].last_qty, legs[0].last_px), (-1, 1000.0, 110.0));
    assert_eq!((legs[1].side, legs[1].last_qty, legs[1].last_px), (1, 1000.0, 110.0));
}

#[test]
fn size_mismatch_is_surfaced_never_healed() {
    let mut book = ShadowBook::default();
    book.fold_fill(&fill(-1, 1000.0, 100.0));
    match book.reanchor("EURUSD", -2000.0, 105.0, 2) {
        Reanchor::SizeMismatch { local_size, venue_size } => {
            assert_eq!(local_size, -1000.0);
            assert_eq!(venue_size, -2000.0);
        }
        other => panic!("expected SizeMismatch, got {other:?}"),
    }
    // The shadow is untouched: the mismatch keeps surfacing until real fills explain it.
    match book.reanchor("EURUSD", -2000.0, 105.0, 3) {
        Reanchor::SizeMismatch { .. } => {}
        other => panic!("expected persistent SizeMismatch, got {other:?}"),
    }
}

#[test]
fn first_line_without_fills_adopts_baseline() {
    let mut book = ShadowBook::default();
    assert_eq!(book.reanchor("EURUSD", -1000.0, 110.0, 1), Reanchor::Baseline);
    // Adopted: the same line is now in sync, no synthesis.
    assert_eq!(book.reanchor("EURUSD", -1000.0, 110.0, 2), Reanchor::InSync);
}

/// Ulp-level avg dust (Java weighted-average vs Rust incremental blend arithmetic) must not
/// trigger a correction — the tolerance is far below a real drift (pips) but above dust.
#[test]
fn ulp_dust_is_in_sync() {
    let mut book = ShadowBook::default();
    book.fold_fill(&fill(-1, 1000.0, 1.1));
    book.fold_fill(&fill(-1, 1000.0, 1.2));
    let blended = book.positions["EURUSD"].avg_px;
    let dusted = blended + blended * 1e-13;
    assert_eq!(book.reanchor("EURUSD", -2000.0, dusted, 2), Reanchor::InSync);
}

/// Per-symbol independence: a drift on one symbol never touches another's shadow.
#[test]
fn symbols_are_independent() {
    let mut book = ShadowBook::default();
    book.fold_fill(&fill(-1, 1000.0, 100.0));
    let mut jpy = fill(-1, 1000.0, 155.0);
    jpy.symbol = "USDJPY".into();
    book.fold_fill(&jpy);
    assert_eq!(book.reanchor("USDJPY", -1000.0, 155.0, 2), Reanchor::InSync);
    assert_eq!(book.reanchor("EURUSD", -1000.0, 100.0, 2), Reanchor::InSync);
}
