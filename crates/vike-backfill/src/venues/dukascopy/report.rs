//! The HTTP lane's refusal and stop texts, and the skip that turns a refused chunk into a hole.

use crate::error::CollectError;

#[cfg(doc)]
use super::backfill_quotes_then_bars;
#[cfg(doc)]
use super::chunks::chunk_windows;

/// A settled chunk's store step failed with `error`. A supersede REFUSAL is that chunk's own
/// permanent problem — see [`backfill_quotes_then_bars`]'s REFUSED bullet for why the rest of the
/// request need not share it — so it is logged at error level with the remedy, remembered in
/// `refused`, and stepped over (`Ok`). Anything else is not one chunk's fault (the disk, the store
/// or a decode is failing, and the next chunk would meet it too), so it is handed back to abort the
/// request exactly as it always did (`Err`).
///
/// ⚠ The refusal is told from every other error by `DataError::is_supersede_refusal` and by nothing
/// else: only a `Data` error can be one, and a `Data` error that is not one must still abort.
pub(super) fn skip_if_refused(
    error: CollectError,
    symbol: &str,
    interval: &str,
    (from, to): (i64, i64),
    refused: &mut Vec<(i64, i64)>,
) -> Result<(), CollectError> {
    match error {
        CollectError::Data(store) if store.is_supersede_refusal() => {
            tracing::error!(
                "dukascopy {symbol} [{from}, {to}]: chunk SKIPPED — {store}. {}",
                refusal_remedy(symbol, interval)
            );
            refused.push((from, to));
            Ok(())
        }
        other => Err(other),
    }
}

/// What an operator does about a chunk the store refused to supersede, spelled ONCE so the error
/// logged as the chunk is skipped and the error the request ends in cannot drift apart.
///
/// ⚠ The command is the one `backfill_quotes_then_bars`'s doc gives in prose, and its
/// `--produced-by dukascopy` is the bare literal for the reason that doc gives: a series holding a
/// provisional or stamped-canonical key refuses `dukascopy:` with the colon.
pub(super) fn refusal_remedy(symbol: &str, interval: &str) -> String {
    format!(
        "The refusal is PERMANENT for that window — retrying it refuses the same way — and only an \
         operator can clear it: delete this symbol's dukascopy quote series and its {interval} bar \
         series (`vike-cli data hist rm --kind quote --venue dukascopy --symbol {symbol}` and the \
         same with `--kind bar --interval {interval}`, each asserting `--produced-by dukascopy` — \
         the bare literal, since `dukascopy:` with the colon is itself refused on a series holding \
         a provisional key) and re-backfill."
    )
}

/// The error a request ends in when [`skip_if_refused`] skipped chunks: EVERY skipped chunk by its
/// bounds, what became of the rest, and the remedy — because this is all a caller that only sees
/// the reply ever learns, the log lines having been written on the daemon's box.
///
/// It says "wrote no bars for them" and nothing about their ticks, because both halves of the
/// supersede can refuse and only one leaves ticks behind: a QUOTE-side refusal writes nothing at
/// all, a BAR-side one leaves the chunk's ticks stored and its bars not. "No bars" is true of both.
pub(super) fn refusal_report(
    symbol: &str,
    interval: &str,
    (start_ms, end_ms): (i64, i64),
    (chunks, bars_total): (usize, usize),
    refused: &[(i64, i64)],
) -> String {
    let skipped =
        refused.iter().map(|(from, to)| format!("[{from}, {to}]")).collect::<Vec<_>>().join(", ");
    format!(
        "dukascopy {symbol} [{start_ms}, {end_ms}] {interval}: the store REFUSED to supersede a \
         provisional entry it could no longer remove exactly (folded into a multi-key part, e.g. by \
         background compaction), so those chunks were SKIPPED and this request wrote no {interval} \
         bars for them — chunks skipped, {} of {chunks}: {skipped}. The other chunks ran to their \
         end, writing {bars_total} {interval} bars. {}",
        refused.len(),
        refusal_remedy(symbol, interval),
    )
}

/// [`CollectError::Stopped`]'s text for a request asked to stop before the chunk `(from, to)` of a
/// window cut into `chunk`-long pieces: the boundary, what the `chunks` before it wrote, that
/// repeating the request resumes there — and EVERY chunk a supersede refusal skipped earlier, with
/// the remedy, because this is all a caller that reads only the reply ever learns, and a stop must
/// never hide a hole the request had already made.
///
/// The total is counted from the grid rather than by walking [`chunk_windows`], so naming it costs
/// nothing however long the window is; `i128`, so a window at the edge of `i64` cannot overflow it.
pub(super) fn stopped_report(
    symbol: &str,
    interval: &str,
    (start_ms, end_ms): (i64, i64),
    chunk: i64,
    (from, to): (i64, i64),
    (chunks, quote_rows, bars): (usize, usize, usize),
    refused: &[(i64, i64)],
) -> String {
    let cell = |ms: i64| i128::from(ms.div_euclid(chunk));
    let total = (cell(end_ms) - cell(start_ms) + 1).max(1);
    let holes = if refused.is_empty() {
        String::new()
    } else {
        let skipped = refused
            .iter()
            .map(|(from, to)| format!("[{from}, {to}]"))
            .collect::<Vec<_>>()
            .join(", ");
        format!(
            " But {} of those {chunks} chunk(s) were SKIPPED before the stop, because the store \
             REFUSED to supersede them, and hold no {interval} bars: {skipped}. {}",
            refused.len(),
            refusal_remedy(symbol, interval),
        )
    };
    format!(
        "dukascopy {symbol} [{start_ms}, {end_ms}] {interval}: asked to stop, and stopped before \
         chunk {} of {total} [{from}, {to}] — nothing failed. The {chunks} chunk(s) before it ran, \
         writing {bars} {interval} bars and {quote_rows} quote rows; what they wrote stays stored. \
         This chunk and every one after it were never fetched: repeating the request resumes \
         here.{holes}",
        chunks + 1,
    )
}
