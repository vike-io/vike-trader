//! The pure window arithmetic: settledness, the chunk grid, whole buckets and archive-day cuts.

use vike_model::{MS_PER_DAY, QuoteTick};

use super::PUBLICATION_MARGIN_MS;
#[cfg(doc)]
use super::{archive_owned_days, backfill_quotes_then_bars};

/// The start of every UTC day `[from, to]` touches that COULD carry an archive day key — one starting
/// at or after the epoch (an archive day is a calendar date from 1970 on) whose last millisecond is
/// representable — ascending; none for an inverted window, which therefore stays the one piece it
/// always was. The day indices come from `div_euclid`, so a window starting before 1970 finds its
/// days like any other.
pub(super) fn archive_days_touched((from, to): (i64, i64)) -> Vec<i64> {
    if from > to {
        return Vec::new();
    }
    (from.div_euclid(MS_PER_DAY).max(0)..=to.div_euclid(MS_PER_DAY))
        .filter_map(|index| index.checked_mul(MS_PER_DAY))
        .filter(|day| day.checked_add(MS_PER_DAY - 1).is_some())
        .collect()
}

/// `[from, to]` with every `owned` UTC day cut out, as inclusive pieces in order — the parts of a
/// chunk the HTTP lane may still fetch and store. `owned` is ascending ([`archive_owned_days`]).
///
/// With NOTHING owned the answer is exactly `[(from, to)]`, an inverted window included, which is
/// what keeps a chunk the archive never touched byte-identical to the lane before the archive
/// existed. With every day owned it is empty. Checked arithmetic, so a day at the edge of `i64` ends
/// the cut rather than overflowing it.
pub(super) fn unowned_pieces((from, to): (i64, i64), owned: &[i64]) -> Vec<(i64, i64)> {
    if owned.is_empty() {
        return vec![(from, to)];
    }
    let mut pieces = Vec::new();
    let mut cursor = Some(from);
    for &day in owned {
        let Some(at) = cursor else { break };
        if day > at {
            pieces.push((at, (day - 1).min(to)));
        }
        cursor = day.checked_add(MS_PER_DAY).map(|next| next.max(at));
    }
    if let Some(at) = cursor
        && at <= to
    {
        pieces.push((at, to));
    }
    pieces
}

/// What was `fetched` piece by piece, cut down to `pieces` — the chunk's pieces RE-COMPUTED under the
/// day-owner lock, after an import stored one of its days while the download ran. The archive only
/// GAINS days, so every new piece lies inside a fetched one: it takes that piece's ticks inside its
/// own bounds, under its own bounds, and a fetched piece the archive now holds whole is dropped. A
/// new piece inside no fetched one was never fetched because its own key was already spent, and it
/// is stored already.
pub(super) fn clip_to(
    fetched: Vec<((i64, i64), Vec<QuoteTick>)>,
    pieces: &[(i64, i64)],
) -> Vec<((i64, i64), Vec<QuoteTick>)> {
    let mut out = Vec::new();
    for ((fetched_from, fetched_to), quotes) in fetched {
        for &(from, to) in pieces.iter().filter(|(f, t)| fetched_from <= *f && *t <= fetched_to) {
            let kept = quotes.iter().filter(|q| (from..=to).contains(&q.ts)).cloned().collect();
            out.push(((from, to), kept));
        }
    }
    out
}

/// Whether a chunk whose fetched cut ends at `to` is SETTLED at `now_ms` — its last millisecond
/// more than [`PUBLICATION_MARGIN_MS`] old — and so takes the shared keys its own bounds give it.
pub(super) fn settled(to: i64, now_ms: i64) -> bool {
    to < now_ms.saturating_sub(PUBLICATION_MARGIN_MS)
}

/// The chunk [`backfill_quotes_then_bars`] cuts a window into, in milliseconds: the most whole
/// `step` bars that fit in a UTC day ([`MS_PER_DAY`]), or one bar when a bar is longer than a day.
/// Always a whole number of bars, which is what puts every chunk edge on a bar edge. `step > 0`, so
/// the product is at most a day and cannot overflow.
pub(super) fn chunk_len(step: i64) -> i64 {
    step.max((MS_PER_DAY / step) * step)
}

/// `[start_ms, end_ms]` cut at every multiple of `chunk` inside it (the grid anchored at epoch 0),
/// as inclusive sub-windows in order — lazily, so a window of any length costs one pair at a time.
/// A window inside one chunk comes back whole, as the one pair `(start_ms, end_ms)` — an inverted
/// window too. The chunk's END is computed rather than its start, so a window at `i64::MIN` never
/// needs the chunk start below it; `rem_euclid` rather than `%`, so a pre-1970 stamp finds its
/// chunk like any other; saturating, so the last chunk below `i64::MAX` ends at `end_ms` instead of
/// overflowing. `chunk > 0`.
pub(super) fn chunk_windows(
    start_ms: i64,
    end_ms: i64,
    chunk: i64,
) -> impl Iterator<Item = (i64, i64)> {
    let mut next = Some(start_ms);
    std::iter::from_fn(move || {
        let from = next?;
        let to = from.saturating_add(chunk - 1 - from.rem_euclid(chunk)).min(end_ms);
        // `to < end_ms` is what keeps `to + 1` from overflowing: a window reaching `i64::MAX`
        // stops here instead.
        next = if to < end_ms { Some(to + 1) } else { None };
        Some((from, to))
    })
}

/// The widest run of WHOLE `step` buckets inside the inclusive window `[start_ms, end_ms]`, as an
/// inclusive `(first, last)` — `None` when not one bucket fits. The start rounds UP onto the `step`
/// grid and `end_ms + 1` rounds DOWN onto it (the exclusive end of the last whole bucket).
/// `rem_euclid` rather than `%`, so a pre-1970 stamp rounds the same way as any other; checked
/// arithmetic, so a window at the edge of `i64` answers `None` instead of overflowing. `step > 0`.
pub(super) fn whole_buckets(start_ms: i64, end_ms: i64, step: i64) -> Option<(i64, i64)> {
    let first = match start_ms.rem_euclid(step) {
        0 => start_ms,
        rem => start_ms.checked_add(step - rem)?,
    };
    let end_excl = end_ms.saturating_add(1);
    let end_excl = end_excl.checked_sub(end_excl.rem_euclid(step))?;
    if end_excl > first { Some((first, end_excl - 1)) } else { None }
}

/// The last millisecond of the archive day starting at `day` — `None` unless `day` is a UTC midnight
/// at or after 1970 whose last millisecond is representable.
pub(super) fn archive_day_end(day: i64) -> Option<i64> {
    if day < 0 || day.rem_euclid(MS_PER_DAY) != 0 {
        return None;
    }
    day.checked_add(MS_PER_DAY - 1)
}
