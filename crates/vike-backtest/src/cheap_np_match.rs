//! Anchoring an ON-CHAIN Polymarket entry to the recorded CLOB tape — the measurement's matching
//! rule, kept apart from the archive it reads.
//!
//! # Why this is not in `vike_data::cheap_np_book`
//!
//! That module answers "what stood in the recorded book at time T": the delta fold, the
//! venue-authoritative ghost repair, and the checkpoint gate that tests the archive against its own
//! published full-book frames. Every one of those is a property of the DATA, and `vike-data` owns
//! the `kind=` layouts they read.
//!
//! What lives here answers a different question — "which CLOB print corresponds to THIS on-chain
//! trade" — and it answers it with TOLERANCES chosen for one measurement. A different study would
//! choose different ones. Those numbers rode into `vike-data` with the fold when the Polymarket
//! cluster left the simulator, which put a measurement's matching heuristics in a crate 34 others
//! depend on. This module is the correction.
//!
//! ⚠ Gated with the two bins that are its only callers. `cheap_np_depth` and `cheap_np_askgate`
//! both declare `required-features = ["datafusion-store"]`, so an ungated module here would repeat
//! the misfiling this split exists to undo — which is exactly what `cheap_np_book` was before it
//! moved (ungated, while both its consumers were gated).

use vike_model::TradeTick;

/// Polygon block time in ms — the fallback anchor steps back exactly one block from the on-chain
/// stamp so an unmatched entry is still read PRE-entry.
pub const POLYGON_BLOCK_MS: i64 = 2_000;

/// How far from the on-chain block stamp the L2 trade tape is searched for the entry print.
/// Polygon block time is 2 s, so ±4 s is two blocks either side — wide enough to absorb the
/// block-vs-CLOB clock offset, narrow enough that an unrelated same-price print of the same token
/// is not preferred over the real one (the nearest match wins regardless).
pub const TRADE_MATCH_WINDOW_MS: i64 = 4_000;

/// Price tolerance when matching an on-chain print to the recorded CLOB tape. Half a cent: under
/// the venue's own coarse tick (0.01), so a match can never cross a price level, but wide enough to
/// absorb the amount-derived off-grid prices the on-chain tape carries.
pub const PRICE_MATCH_TOL: f64 = 0.005;

/// Anchor the on-chain entry stamp `ts_ms` to the recorded CLOB tape: the nearest print within
/// [`TRADE_MATCH_WINDOW_MS`] whose price is within [`PRICE_MATCH_TOL`] of `px`.
///
/// The entry stamp is a **Polygon block timestamp** (whole seconds, one stamp shared by every print
/// in the block) while the book stream carries the venue's own millisecond CLOB clock, so reading
/// the book at the block stamp reads a book the entry's own block has already eaten. `Some(ts)` is
/// the matched CLOB stamp; `None` means fall back one block (see [`POLYGON_BLOCK_MS`]).
///
/// The price tolerance is NOT slack for its own sake: an on-chain trade's `price` is derived from
/// the maker/taker AMOUNTS of the settled order, so a partial fill books an off-grid value
/// (`0.31324237288135592` is a real April row) that no CLOB print equals exactly.
pub fn match_clob_print(tape: &[TradeTick], ts_ms: i64, px: f64) -> Option<i64> {
    tape.iter()
        .filter(|t| {
            (t.price - px).abs() <= PRICE_MATCH_TOL && (t.ts - ts_ms).abs() <= TRADE_MATCH_WINDOW_MS
        })
        .min_by_key(|t| (t.ts - ts_ms).abs())
        .map(|t| t.ts)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The block-vs-CLOB anchor: nearest print wins, an unrelated price does not match, and a
    /// stamp with nothing near it falls back (`None`).
    #[test]
    fn the_clob_anchor_takes_the_nearest_price_matched_print() {
        let t = |ts: i64, price: f64| TradeTick {
            ts,
            local_ts: ts,
            price,
            size: 1.0,
            is_buyer_maker: false,
            symbol: "TOK".into(),
        };
        let tape = vec![t(9_000, 0.28), t(10_400, 0.2802), t(10_900, 0.28), t(30_000, 0.28)];
        assert_eq!(match_clob_print(&tape, 10_500, 0.28), Some(10_400), "nearest wins");
        // an off-grid on-chain price still matches inside half a cent
        assert_eq!(match_clob_print(&tape, 10_400, 0.2831), Some(10_400));
        assert_eq!(match_clob_print(&tape, 10_500, 0.50), None, "wrong price: no match");
        assert_eq!(match_clob_print(&tape, 60_000, 0.28), None, "outside the block window");
    }
}
