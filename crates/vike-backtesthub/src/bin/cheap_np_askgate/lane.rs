//! The lanes' per-signal helpers: the rested-order watch, both live prices.

use vike_backtesthub::research::cheap_np_book::{PRICE_GRID, ask_ladder_at};
use vike_marketdata::{L2Book, L2BookLevel, L2BookUpdate, TickQuote, apply_book_event};
use vike_model::ContractValue;

use super::as_book;

/// Did the market come back to `limit` before the window resolved? A resting BUY at `limit` is hit
/// when a seller crosses down to it, which the recorded book shows as its best ask reaching
/// `limit` or better. Folded forward from `after_timestamp` to the window close.
///
/// Deliberately a SUFFICIENT-not-necessary test: it can only under-count (a fill that happened
/// entirely between two recorded frames is invisible), so a rested order it reports as never
/// crossing really did sit unfilled in the recorded book.
///
/// The book is the BACKTEST's (`vike_marketdata::apply_book_event`, the recorded chain's one rule,
/// on [`PRICE_GRID`]): a hole in the recorded sequence, an orphan delta or a stream-health marker
/// drops it until the next snapshot, so a cross the simulator could not see is not counted here
/// either.
pub(super) fn rested_order_crossed(
    updates: &[L2BookUpdate],
    l1: &[TickQuote],
    after_timestamp: i64,
    until_timestamp: i64,
    limit: f64,
) -> bool {
    // Re-fold from the start (the book has to be correct, not just recent), then watch.
    let mut slot: Option<L2Book> = None;
    let mut li = 0usize;
    for u in updates {
        if u.timestamp > until_timestamp {
            break;
        }
        while li < l1.len() && l1[li].timestamp <= u.timestamp {
            li += 1;
        }
        apply_book_event(&mut slot, u, PRICE_GRID, |book| book);
        let Some(book) = slot.as_mut() else { continue };
        if li > 0 {
            // The ghost repair keeps the chain's sequence (`cheap_np_book`'s module doc, "Gaps").
            vike_backtesthub::research::cheap_np_book::prune_ghost_asks(book, l1[li - 1].ask);
        }
        if u.timestamp >= after_timestamp
            && let Some(L2BookLevel { price: px, .. }) = book.best_ask()
            && px <= limit
        {
            return true;
        }
    }
    false
}

/// Why the `--live-ms` lanes exist, and why they are NOT the ask gate.
///
/// The θ-gated `ask` / `ask + hold` lanes above model what the strategy *should* do: re-score the
/// edge at the price actually available and refuse the trade when it no longer clears θ. The LIVE
/// Dublin bot does not do that. Read from its own source
/// (`vike_db_data_jobs/trading/fair_value_bot/`):
///
/// * `cheap_bot.py::_cheap_tape_entry` — *"ignore the resting book ask, fire on the FIRST qualifying
///   cheap-band taker BUY print per window"*. So the live ENTRY gate is the **print** gate; the
///   resting ask plays no part in whether a signal is taken.
/// * `strategy.py::delayed_fill` — *"Take EVERY signal that fired, at WHATEVER ask is resting now …
///   there is NO band gate: the only non-fill is an empty book (no ask to buy at any price)."*
///
/// That is the whole explanation for the rejection-rate gap (live refuses <0.6%, the θ-gated ask
/// lane refuses a majority): **they are not the same rule.** A lane that reproduces the live law has
/// to price at the unconditional best resting ask — no limit, no θ re-check — and count only an
/// empty book as a miss. That is exactly what these lanes do, at whatever offsets `--live-ms` names
/// (the live bot's own `d_measured_ms` is the offset to use: ~954 ms for its `delayed +0s` lane,
/// ~4229 ms for `delayed +2s`).
///
/// The live bot also fills at TOP OF BOOK only (`best_ask`, `shares = min(1, displayed_size)`),
/// never walking the ladder — which at its 1-share size is the same thing a walk would return, so
/// the taker price with no limit reproduces it exactly rather than approximating it.
///
/// Returns the price paid, or `None` for the live bot's ONE non-fill: an empty book.
pub(super) fn live_law_price(
    books: &[L2BookUpdate],
    l1: &[TickQuote],
    at_timestamp: i64,
    size: f64,
) -> Option<Option<f64>> {
    // Outer `None` = no trustworthy book at all (a DATA gap — never reported as a strategy result).
    // Inner `None` = a trustworthy book that is EMPTY on the ask side, which is the live 'empty'.
    let ladder = ask_ladder_at(books, l1, &[], at_timestamp, false)?;
    Some(ContractValue::default().taker_price(&as_book(&ladder), 1, size, None))
}

/// The venue's OWN recorded top-of-book ask as of `at_timestamp` — the archive's `best_ask` column, not a
/// price derived from folding its deltas.
///
/// This exists because the two disagree, and the disagreement is measurable rather than theoretical.
/// The live bot fills from `self.book.best.get(token)` — the WS feed's own best ask — which is exactly
/// what this column records. Our folded ladder reconstructs the same quantity from snapshot+delta
/// replay, and `verify_checkpoints` already reports that it reproduces the recorded best ask on only
/// ~90% of snapshot intervals. So a lane priced off the fold and a lane priced off this column are
/// two independent estimates of the SAME live quantity, and reporting both makes any reconstruction
/// bias visible instead of silently charging it to the strategy.
///
/// `None` when no quote has been recorded at or before `at_timestamp`, or the recorded ask is non-positive.
pub(super) fn l1_ask_at(l1: &[TickQuote], at_timestamp: i64) -> Option<f64> {
    let i = l1.partition_point(|q| q.timestamp <= at_timestamp);
    l1[..i].iter().rev().find(|q| q.ask > 0.0).map(|q| q.ask)
}
