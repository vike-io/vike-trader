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

use vike_model::{BookLevel, BookUpdate, BookUpdateKind, L2Book, QuoteTick, TradeTick};

/// Price grid the book is folded on. pmxt publishes `price` as a fixed scale-4 decimal, so every
/// level price is an exact multiple of 1e-4 — folding on that grid is lossless AND immune to the
/// venue's mid-stream tick-size regime changes (0.01 → 0.001 near the extremes), which a book keyed
/// on the reported `tick_size` would quantize differently on either side of the switch.
pub const PRICE_GRID: f64 = 0.0001;

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
    book.apply_delta(book.last_seq + 1, &[], &[BookLevel::new(px, left)]);
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
    let ghosts: Vec<f64> = book
        .top_n(1 << 16)
        .1
        .into_iter()
        .map(|BookLevel { price: p, .. }| p)
        .take_while(|&p| p < best_ask)
        .collect();
    for p in &ghosts {
        // A zero-size delta IS the removal verb (`L2Book::apply_side`), so the repair goes through
        // the same path a venue cancel would.
        book.apply_delta(book.last_seq + 1, &[], &[BookLevel::new(*p, 0.0)]);
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
) -> Option<Vec<BookLevel>> {
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
                    let replayed: Vec<BookLevel> = book.top_n(1 << 16).1;
                    let mut published = u.asks.clone();
                    published.retain(|&BookLevel { qty: q, .. }| q != 0.0);
                    published.sort_by(|a, b| a.price.partial_cmp(&b.price).unwrap());
                    let mut bad = 0usize;
                    let mut i = 0usize;
                    let mut j = 0usize;
                    while i < replayed.len() || j < published.len() {
                        c.levels += 1;
                        match (replayed.get(i), published.get(j)) {
                            (
                                Some(&BookLevel { price: rp, qty: rq }),
                                Some(&BookLevel { price: pp, qty: pq }),
                            ) if (rp - pp).abs() < PRICE_GRID => {
                                if (rq - pq).abs() > SIZE_EPS {
                                    bad += 1;
                                }
                                i += 1;
                                j += 1;
                            }
                            // a level one side has and the other does not
                            (
                                Some(&BookLevel { price: rp, .. }),
                                Some(&BookLevel { price: pp, .. }),
                            ) => {
                                bad += 1;
                                if rp < pp { i += 1 } else { j += 1 }
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
                        (Some(&BookLevel { price: a, .. }), Some(&BookLevel { price: b, .. })) => {
                            (a - b).abs() < PRICE_GRID
                        }
                        (None, None) => true,
                        _ => false,
                    } {
                        c.best_ask_ok += 1;
                    }
                    let same5 = replayed.len().min(5) == published.len().min(5)
                        && replayed.iter().zip(published.iter()).take(5).all(|(r, p)| {
                            (r.price - p.price).abs() < PRICE_GRID
                                && (r.qty - p.qty).abs() <= SIZE_EPS
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

#[path = "cheap_np_book_tests.rs"]
#[cfg(test)]
mod cheap_np_book_tests;
