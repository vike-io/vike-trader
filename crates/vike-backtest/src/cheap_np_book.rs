//! Reconstructing the Polymarket L2 book from the recorded `kind=book` archive — the measurement
//! substrate the `cheap_np_depth` and `cheap_np_askgate` bins share.
//!
//! Moved here verbatim from the `cheap_np_depth` bin (PR #650) when a SECOND bin needed the same
//! fold. Two bins folding the archive two ways is exactly how a measurement stops being
//! reproducible, so the fold, the ghost repair and the integrity gate all live in one place and
//! every number either bin reports is read off the same book.
//!
//! # Ghosts — why a pure delta replay is WRONG here, and the repair
//!
//! Replaying the archive's `price_change` deltas ALONE leaves stale levels resting, and they linger
//! BELOW the true best ask — the worst possible direction for a taker-cost estimate, because a
//! sweep then walks phantom cheap liquidity and reports a price nobody could have paid. Measured on
//! one real April 2026 `btc-updown-5m` token (168,312 `price_change` rows): the replayed best ask
//! agreed with the venue's own `best_ask` column in only 21,940 of 167,874 rows, and at the next
//! published `book` frame 42 of 71 replayed levels were not in it (0 the other way — the replay
//! only ever had TOO MUCH).
//!
//! The repair is venue-authoritative rather than a heuristic: every `price_change` row carries the
//! venue's own `best_bid`/`best_ask`, which `vike_backfill::pmxt::l1_from_row` lands as a
//! `kind=quote` L1 series alongside the book. [`prune_ghost_asks`] drops every ask strictly below
//! the newest such quote — a level below the venue's own best ask provably does not exist. That
//! lifts the agreement to 158,713 of 167,874 and makes the same `book` frame reproduce exactly
//! (29 levels, 0 missing, 0 extra).
//!
//! # Gaps
//!
//! [`vike_model::L2Book`] is folded with a LOCALLY generated monotonic seq rather than the recorded
//! one: the pmxt mapper assigns `seq` per asset per HOURLY FILE, so it restarts at 0 every hour and
//! the stock `apply_delta` monotonic gate would silently drop every delta after the first hour
//! boundary. The store already returns events sorted by `(ts, seq)`, which is true feed order
//! across hours because `ts` dominates. Integrity is instead asserted the honest way: a reading is
//! only trusted when a `Snapshot` anchor precedes its cutoff ([`Folded::anchored`]), and
//! `GapStart`/`Stale` status markers invalidate the book until the next `Snapshot`.

use vike_model::{BookUpdate, BookUpdateKind, L2Book, QuoteTick, TradeTick};

/// Price grid the book is folded on. pmxt publishes `price` as a fixed scale-4 decimal, so every
/// level price is an exact multiple of 1e-4 — folding on that grid is lossless AND immune to the
/// venue's mid-stream tick-size regime changes (0.01 → 0.001 near the extremes), which a book keyed
/// on the reported `tick_size` would quantize differently on either side of the switch.
pub const PRICE_GRID: f64 = 0.0001;

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

/// Size-comparison epsilon for the checkpoint check — the archive's `size` column is a scale-6
/// decimal, so anything under half its quantum is representation noise, not a real difference.
pub const SIZE_EPS: f64 = 5e-7;

/// The book as of a cutoff, plus whether it is trustworthy there.
pub struct Folded {
    pub book: L2Book,
    /// A `Snapshot` was seen at or before the cutoff and no status marker has invalidated it since.
    /// A reading off an UNANCHORED book is deltas-without-a-base and must be discarded, never
    /// measured.
    pub anchored: bool,
    /// Book events folded.
    pub applied: usize,
    /// `GapStart`/`Stale` markers crossed (pmxt emits none; a live-recorded series can).
    pub status_markers: usize,
    /// `ts` of the newest event folded — `cutoff − last_ts` is how stale the book was.
    pub last_ts: i64,
    /// Ghost ask levels removed by [`prune_ghost_asks`].
    pub pruned: usize,
    /// Ask size deducted by [`consume_ask`] from the recorded trade tape.
    pub consumed: f64,
}

/// Deduct `qty` from the ask level at `px` — a candidate second half of the ghost repair, kept
/// OPT-IN (`--consume-trades`) because the integrity gate REJECTED it.
///
/// The hypothesis was that a match which only partially eats a level leaves its recorded size too
/// big and the archive's own `last_trade_price` rows carry the missing quantity. The
/// snapshot-checkpoint gate adjudicated it on the real April 2026 pilot and said no: applying it
/// took the exactly-reproduced intervals from 29/247 to 21/247, the top-5 from 96/247 to 78/247 and
/// the mismatched ask levels from 1,809 to 1,864. The venue DOES report match-consumed size in the
/// delta stream, so deducting the tape on top double-counts. Kept (behind the flag) rather than
/// deleted so the finding stays reproducible instead of becoming folklore.
pub fn consume_ask(book: &mut L2Book, px: f64, qty: f64) {
    if qty <= 0.0 || qty.is_nan() {
        return;
    }
    let have = book.ask_qty_at(px);
    if have <= 0.0 {
        return;
    }
    let left = (have - qty).max(0.0);
    book.apply_delta(book.last_seq + 1, &[], &[(px, left)]);
}

/// Drop every ask level strictly below `best_ask` — the GHOST REPAIR (see the module doc).
/// `best_ask` is the venue's own top of book as the archive recorded it, so an ask below it
/// provably no longer exists; the delta stream just never said so.
pub fn prune_ghost_asks(book: &mut L2Book, best_ask: f64) -> usize {
    // Spelled as a total predicate over f64 — an absent (0.0) or NaN quote is a NO-OP, never a book
    // wipe (`cheap_np.rs`'s `push_spot` uses the same idiom for the same reason).
    if best_ask <= 0.0 || best_ask.is_nan() {
        return 0;
    }
    let ghosts: Vec<f64> =
        book.top_n(1 << 16).1.into_iter().map(|(p, _)| p).take_while(|&p| p < best_ask).collect();
    for p in &ghosts {
        // A zero-size delta IS the removal verb (`L2Book::apply_side`), so the repair goes through
        // the same path a venue cancel would.
        book.apply_delta(book.last_seq + 1, &[], &[(*p, 0.0)]);
    }
    ghosts.len()
}

/// Fold `updates` (store order = `(ts, seq)` = feed order) up to `cutoff`.
///
/// `strict` selects the cutoff comparison: `true` folds `ts < cutoff` (the pre-print state),
/// `false` folds `ts <= cutoff` (the at-block state).
pub fn fold_until(
    updates: &[BookUpdate],
    l1: &[QuoteTick],
    tape: &[TradeTick],
    cutoff: i64,
    strict: bool,
) -> Folded {
    let mut book = L2Book::new(PRICE_GRID);
    let mut anchored = false;
    let mut last_ts = 0i64;
    let (mut applied, mut status_markers) = (0usize, 0usize);
    let mut pruned = 0usize;
    let mut consumed = 0.0f64;
    // A locally generated seq: see the module doc's "Gaps". The recorded seq restarts every hour.
    let mut seq: u64 = 0;
    // Merge cursor over the L1 series: every ask level below the newest venue-reported best ask at
    // or before this update's `ts` is a ghost and is dropped.
    let mut li = 0usize;
    // Merge cursor over the trade tape (see `consume_ask`).
    let mut ti = 0usize;
    for u in updates {
        let in_range = if strict { u.ts < cutoff } else { u.ts <= cutoff };
        if !in_range {
            break;
        }
        seq += 1;
        last_ts = u.ts;
        while li < l1.len() && l1[li].ts <= u.ts {
            li += 1;
        }
        // Every taker BUY at or before this update ate ask liquidity the delta stream may not have
        // reported. `is_buyer_maker == false` is the taker-buy flag (`pmxt::map_row`).
        while ti < tape.len() && tape[ti].ts <= u.ts {
            if !tape[ti].is_buyer_maker {
                let before = book.ask_qty_at(tape[ti].price);
                consume_ask(&mut book, tape[ti].price, tape[ti].size);
                consumed += before - book.ask_qty_at(tape[ti].price);
            }
            ti += 1;
        }
        match u.kind {
            BookUpdateKind::Snapshot => {
                book.apply_snapshot(seq, &u.bids, &u.asks);
                anchored = true;
                applied += 1;
            }
            BookUpdateKind::Delta => {
                book.apply_delta(seq, &u.bids, &u.asks);
                applied += 1;
            }
            // Recorded stream-health markers: the book cannot be trusted again until the re-seed
            // `Snapshot` that follows.
            BookUpdateKind::GapStart | BookUpdateKind::Stale => {
                anchored = false;
                status_markers += 1;
            }
            BookUpdateKind::LiveResume => status_markers += 1,
        }
        if li > 0 {
            pruned += prune_ghost_asks(&mut book, l1[li - 1].ask);
        }
    }
    Folded { book, anchored, applied, status_markers, last_ts, pruned, consumed }
}

/// The ask side of a trustworthy book at `cutoff`, ascending price, or `None` when the fold is not
/// anchored there (no snapshot, or a gap marker since the last one).
///
/// The ONE reading both measurement bins take, so "we only measure an anchored book" is a property
/// of the shared helper rather than a discipline each bin has to remember.
pub fn ask_ladder_at(
    updates: &[BookUpdate],
    l1: &[QuoteTick],
    tape: &[TradeTick],
    cutoff: i64,
    strict: bool,
) -> Option<Vec<(f64, f64)>> {
    let f = fold_until(updates, l1, tape, cutoff, strict);
    (f.applied > 0 && f.anchored).then(|| f.book.top_n(1 << 16).1)
}

/// Result of the delta-stream integrity check — see [`verify_checkpoints`].
#[derive(Debug, Default, Clone, Copy)]
pub struct Checkpoints {
    /// Snapshot→snapshot intervals compared (the first snapshot only seeds).
    pub checked: usize,
    /// Intervals where replaying the deltas reproduced the next snapshot's ASK side EXACTLY.
    pub exact: usize,
    /// Ask price levels that differed, summed over all intervals.
    pub level_mismatches: usize,
    /// Ask price levels compared, summed over all intervals.
    pub levels: usize,
    /// Intervals whose replayed BEST ASK price matched the published one.
    pub best_ask_ok: usize,
    /// Intervals whose replayed best FIVE ask levels matched exactly (price and size).
    pub top5_ok: usize,
}

impl Checkpoints {
    /// Fold another token's result in (the bins accumulate over every token they touch).
    pub fn merge(&mut self, o: Checkpoints) {
        self.checked += o.checked;
        self.exact += o.exact;
        self.level_mismatches += o.level_mismatches;
        self.levels += o.levels;
        self.best_ask_ok += o.best_ask_ok;
        self.top5_ok += o.top5_ok;
    }
}

/// Replay the deltas between consecutive archive `Snapshot`s and check they reproduce the next
/// snapshot — the delta stream's own integrity proof, needing no second data source.
///
/// This is the honest answer to "how did you handle gaps": rather than ASSERTING the archive is
/// complete, every interval between two published full-book frames is an independent test of it. A
/// dropped `price_change` row shows up immediately as an ask level that does not match. Comparison
/// is on the ASK side only — the side every measurement reads — on the [`PRICE_GRID`] tick, with
/// sizes compared at [`SIZE_EPS`] (the archive's own scale-6 quantum).
pub fn verify_checkpoints(
    updates: &[BookUpdate],
    l1: &[QuoteTick],
    tape: &[TradeTick],
) -> Checkpoints {
    let mut c = Checkpoints::default();
    let mut book = L2Book::new(PRICE_GRID);
    let mut seq: u64 = 0;
    let mut seeded = false;
    let mut li = 0usize;
    let mut ti = 0usize;
    for u in updates {
        seq += 1;
        while li < l1.len() && l1[li].ts <= u.ts {
            li += 1;
        }
        while ti < tape.len() && tape[ti].ts <= u.ts {
            if !tape[ti].is_buyer_maker {
                consume_ask(&mut book, tape[ti].price, tape[ti].size);
            }
            ti += 1;
        }
        match u.kind {
            BookUpdateKind::Snapshot => {
                if seeded {
                    c.checked += 1;
                    // The replayed ask side vs the one the venue just published.
                    let replayed: Vec<(f64, f64)> = book.top_n(1 << 16).1;
                    let mut published = u.asks.clone();
                    published.retain(|&(_, q)| q != 0.0);
                    published.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
                    let mut bad = 0usize;
                    let mut i = 0usize;
                    let mut j = 0usize;
                    while i < replayed.len() || j < published.len() {
                        c.levels += 1;
                        match (replayed.get(i), published.get(j)) {
                            (Some(&(rp, rq)), Some(&(pp, pq))) if (rp - pp).abs() < PRICE_GRID => {
                                if (rq - pq).abs() > SIZE_EPS {
                                    bad += 1;
                                }
                                i += 1;
                                j += 1;
                            }
                            // a level one side has and the other does not
                            (Some(&(rp, _)), Some(&(pp, _))) => {
                                bad += 1;
                                if rp < pp {
                                    i += 1
                                } else {
                                    j += 1
                                }
                            }
                            (Some(_), None) => {
                                bad += 1;
                                i += 1;
                            }
                            (None, Some(_)) => {
                                bad += 1;
                                j += 1;
                            }
                            (None, None) => break,
                        }
                    }
                    c.level_mismatches += bad;
                    if bad == 0 {
                        c.exact += 1;
                    }
                    // Depth-resolved: the measurements only ever read the TOP of the ask side, so a
                    // mismatch 40 levels deep is irrelevant to every number reported while a
                    // best-ask mismatch would invalidate all of them. Reported separately rather
                    // than averaged into one misleading "integrity %".
                    if match (replayed.first(), published.first()) {
                        (Some(&(a, _)), Some(&(b, _))) => (a - b).abs() < PRICE_GRID,
                        (None, None) => true,
                        _ => false,
                    } {
                        c.best_ask_ok += 1;
                    }
                    let same5 = replayed.len().min(5) == published.len().min(5)
                        && replayed.iter().zip(published.iter()).take(5).all(|(r, p)| {
                            (r.0 - p.0).abs() < PRICE_GRID && (r.1 - p.1).abs() <= SIZE_EPS
                        });
                    if same5 {
                        c.top5_ok += 1;
                    }
                }
                book.apply_snapshot(seq, &u.bids, &u.asks);
                seeded = true;
            }
            BookUpdateKind::Delta => {
                book.apply_delta(seq, &u.bids, &u.asks);
            }
            // A recorded gap breaks the chain: the next snapshot is a RESEED, not a checkpoint.
            BookUpdateKind::GapStart | BookUpdateKind::Stale => seeded = false,
            BookUpdateKind::LiveResume => {}
        }
        // The SAME repair the measurement fold applies — the gate must judge the book the
        // measurement actually reads, not an unrepaired one.
        if li > 0 {
            prune_ghost_asks(&mut book, l1[li - 1].ask);
        }
    }
    c
}

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

    fn bu(ts: i64, kind: BookUpdateKind, bids: &[(f64, f64)], asks: &[(f64, f64)]) -> BookUpdate {
        BookUpdate {
            ts,
            local_ts: ts,
            seq: 0,
            kind,
            tick_size: 0.01,
            bids: bids.to_vec(),
            asks: asks.to_vec(),
            symbol: "TOK".into(),
        }
    }

    /// The recorded `seq` restarts every pmxt hour file; folding must NOT drop the post-restart
    /// deltas. This is the one bug that would silently produce a frozen (and far too thin) book.
    #[test]
    fn a_recorded_seq_restart_does_not_drop_later_deltas() {
        let mut updates = vec![
            bu(1_000, BookUpdateKind::Snapshot, &[(0.19, 10.0)], &[(0.20, 100.0)]),
            bu(2_000, BookUpdateKind::Delta, &[], &[(0.21, 50.0)]),
        ];
        // hour boundary: the mapper's per-file counter restarts at 0
        updates[0].seq = 900;
        updates[1].seq = 901;
        updates.push(bu(3_000, BookUpdateKind::Delta, &[], &[(0.22, 70.0)]));
        updates.last_mut().unwrap().seq = 0;

        let f = fold_until(&updates, &[], &[], 4_000, false);
        assert!(f.anchored);
        assert_eq!(f.applied, 3, "the seq-0 delta after the restart must still fold");
        assert_eq!(f.book.ask_qty_at(0.22), 70.0);
        assert_eq!(f.book.quantity_for_price(1, 0.22), 220.0);
    }

    #[test]
    fn strict_cutoff_excludes_the_print_ts_and_inclusive_includes_it() {
        let updates = vec![
            bu(1_000, BookUpdateKind::Snapshot, &[], &[(0.20, 100.0)]),
            // the entry's own block eats the level
            bu(2_000, BookUpdateKind::Delta, &[], &[(0.20, 0.0)]),
        ];
        assert_eq!(fold_until(&updates, &[], &[], 2_000, true).book.ask_qty_at(0.20), 100.0);
        assert_eq!(fold_until(&updates, &[], &[], 2_000, false).book.ask_qty_at(0.20), 0.0);
    }

    /// The ghost repair: a level the venue's own best ask says is gone must not survive into any
    /// depth reading, and a level at or above it must be untouched.
    #[test]
    fn ghost_asks_below_the_venue_best_ask_are_pruned() {
        let mut b = L2Book::new(PRICE_GRID);
        b.apply_snapshot(1, &[], &[(0.20, 100.0), (0.22, 50.0), (0.25, 70.0)]);
        assert_eq!(prune_ghost_asks(&mut b, 0.22), 1, "only the 0.20 level is below 0.22");
        assert_eq!(b.best_ask(), Some((0.22, 50.0)));
        assert_eq!(b.quantity_for_price(1, 1.0), 120.0);
        // idempotent, and a level exactly AT the reported best ask stays
        assert_eq!(prune_ghost_asks(&mut b, 0.22), 0);
        // an absent/zero best ask is a no-op rather than a book wipe
        assert_eq!(prune_ghost_asks(&mut b, 0.0), 0);
        assert_eq!(b.quantity_for_price(1, 1.0), 120.0);
    }

    /// The repair must reach the measured book, not just the helper — folding a stream whose L1
    /// says the cheap level is gone must not leave it for a sweep to walk.
    #[test]
    fn the_fold_applies_the_ghost_repair_from_the_l1_series() {
        let updates =
            vec![bu(1_000, BookUpdateKind::Snapshot, &[], &[(0.20, 100.0), (0.25, 40.0)])];
        let no_l1 = fold_until(&updates, &[], &[], 2_000, false);
        assert_eq!(no_l1.book.best_ask(), Some((0.20, 100.0)), "unrepaired: the ghost survives");

        let l1 = vec![QuoteTick {
            ts: 1_000,
            local_ts: 1_000,
            bid: 0.19,
            ask: 0.25,
            bid_size: 0.0,
            ask_size: 0.0,
            symbol: "TOK".into(),
        }];
        let repaired = fold_until(&updates, &l1, &[], 2_000, false);
        assert_eq!(repaired.book.best_ask(), Some((0.25, 40.0)));
        assert_eq!(repaired.pruned, 1);
        assert_eq!(repaired.book.quantity_for_price(1, 1.0), 40.0, "phantom depth is gone");
    }

    /// The tape deduction: a taker buy eats the level it printed at, never goes negative, and never
    /// invents liquidity at a price the book does not show.
    #[test]
    fn a_taker_buy_deducts_from_the_ask_level_it_printed_at() {
        let mut b = L2Book::new(PRICE_GRID);
        b.apply_snapshot(1, &[], &[(0.20, 100.0), (0.21, 50.0)]);
        consume_ask(&mut b, 0.20, 30.0);
        assert_eq!(b.ask_qty_at(0.20), 70.0);
        // an oversized print empties the level rather than going negative
        consume_ask(&mut b, 0.20, 999.0);
        assert_eq!(b.ask_qty_at(0.20), 0.0);
        // `price_of` reconstitutes a level price as `tick * PRICE_GRID`, so 0.21 comes back as
        // 0.21000000000000002 — compared with a tolerance rather than pinned to that artefact.
        let (px, qty) = b.best_ask().expect("the 0.21 level is now the top of book");
        assert!((px - 0.21).abs() < PRICE_GRID / 2.0, "{px}");
        assert_eq!(qty, 50.0);
        // a price the book does not show is a no-op, not a synthesized negative level
        consume_ask(&mut b, 0.30, 10.0);
        assert_eq!(b.quantity_for_price(1, 1.0), 50.0);
    }

    /// The fold must apply it to the book the measurement reads, and only for TAKER BUYS — a taker
    /// sell hits the bid and must leave the ask side untouched.
    #[test]
    fn the_fold_consumes_ask_liquidity_only_for_taker_buys() {
        let updates = vec![bu(1_000, BookUpdateKind::Snapshot, &[], &[(0.20, 100.0)])];
        let trade = |ts: i64, buyer_maker: bool| TradeTick {
            ts,
            local_ts: ts,
            price: 0.20,
            size: 40.0,
            is_buyer_maker: buyer_maker,
            symbol: "TOK".into(),
        };
        // the snapshot is folded first, then the same-ts trade is applied on the NEXT update
        let updates2 = {
            let mut v = updates.clone();
            v.push(bu(2_000, BookUpdateKind::Delta, &[], &[]));
            v
        };
        let taker_buy = fold_until(&updates2, &[], &[trade(1_500, false)], 3_000, false);
        assert_eq!(taker_buy.book.ask_qty_at(0.20), 60.0);
        assert_eq!(taker_buy.consumed, 40.0);

        let taker_sell = fold_until(&updates2, &[], &[trade(1_500, true)], 3_000, false);
        assert_eq!(taker_sell.book.ask_qty_at(0.20), 100.0, "a taker sell hits the bid");
        assert_eq!(taker_sell.consumed, 0.0);
    }

    #[test]
    fn deltas_without_a_snapshot_are_not_anchored_and_yield_no_ladder() {
        let updates = vec![bu(1_000, BookUpdateKind::Delta, &[], &[(0.20, 100.0)])];
        let f = fold_until(&updates, &[], &[], 9_999, false);
        assert!(!f.anchored, "a book built from deltas alone is not measurable");
        assert_eq!(f.applied, 1);
        assert_eq!(ask_ladder_at(&updates, &[], &[], 9_999, false), None);
    }

    #[test]
    fn a_gap_marker_invalidates_the_anchor_until_the_next_snapshot() {
        let updates = vec![
            bu(1_000, BookUpdateKind::Snapshot, &[], &[(0.20, 100.0)]),
            bu(2_000, BookUpdateKind::GapStart, &[], &[]),
            bu(3_000, BookUpdateKind::Delta, &[], &[(0.21, 5.0)]),
        ];
        assert!(!fold_until(&updates, &[], &[], 3_500, false).anchored);
        assert_eq!(ask_ladder_at(&updates, &[], &[], 3_500, false), None);
        let mut recovered = updates.clone();
        recovered.push(bu(4_000, BookUpdateKind::Snapshot, &[], &[(0.20, 7.0)]));
        assert!(fold_until(&recovered, &[], &[], 4_500, false).anchored);
        assert!(ask_ladder_at(&recovered, &[], &[], 4_500, false).is_some());
    }

    /// The integrity gate must PASS on a complete delta stream and FAIL on one with a row removed —
    /// otherwise a green report would be worth nothing.
    #[test]
    fn checkpoint_verification_catches_a_dropped_delta() {
        let complete = vec![
            bu(1_000, BookUpdateKind::Snapshot, &[], &[(0.20, 100.0), (0.21, 50.0)]),
            bu(2_000, BookUpdateKind::Delta, &[], &[(0.20, 60.0)]),
            bu(3_000, BookUpdateKind::Delta, &[], &[(0.22, 5.0)]),
            bu(4_000, BookUpdateKind::Snapshot, &[], &[(0.20, 60.0), (0.21, 50.0), (0.22, 5.0)]),
        ];
        let ok = verify_checkpoints(&complete, &[], &[]);
        assert_eq!((ok.checked, ok.exact, ok.level_mismatches), (1, 1, 0));

        let mut dropped = complete.clone();
        dropped.remove(1); // the 0.20 → 60 delta never arrives
        let bad = verify_checkpoints(&dropped, &[], &[]);
        assert_eq!((bad.checked, bad.exact), (1, 0), "a dropped delta must not verify");
        assert_eq!(bad.level_mismatches, 1, "exactly the level whose size went stale");
    }

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
