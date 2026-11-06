//! Backward-paging aggTrades backfill: the paging loop, its stop reason, the REST entry points.

use crate::family::FamilySpec;

use super::{AggTrade, fetch_agg_trades_from_id, fetch_agg_trades_latest};

// -------------------------------------------------------------------------------------------
// Backward-paging backfill: a pure paging loop over `fetch_agg_trades_from_id` (injected as
// `fetch` so it's testable without the network — see each venue's `tests`).
// -------------------------------------------------------------------------------------------

/// Why a backward backfill walk stopped — the answer the pre-#937-producer-half code erased by
/// turning every fetch error into an empty page (module doc).
///
/// The distinction the operator actually acts on is `Capped` vs `Failed`: both mean history older
/// than the oldest emitted trade is missing from this walk, but for opposite reasons. `Capped` is a
/// deliberate budget decision — the venue served every page it was asked for, and raising
/// `max_pages` (or narrowing `earliest_ts`) reaches the rest. `Failed` is data LOSS — the venue
/// still has those trades, this call just failed to read them, so re-running the identical request
/// is the fix.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackfillStop {
    /// The requested window is fully covered — either a fetched page reached back to `earliest_ts`,
    /// or there is nothing older to fetch (an empty page, or the id floor at 1). Nothing is missing.
    Complete,
    /// `max_pages` pages were fetched before `earliest_ts` was reached: more history EXISTS at the
    /// venue and was deliberately not requested.
    Capped,
    /// A page fetch failed and the walk stopped there, carrying the fetch's error verbatim.
    /// Everything older than the oldest emitted trade is MISSING (not absent), and the pages already
    /// collected were still emitted.
    Failed(String),
}

impl BackfillStop {
    /// True when history older than the oldest emitted trade is missing from this walk — i.e. the
    /// walk did NOT reach `earliest_ts`. This is the historical `truncated` flag, widened from
    /// "the cap fired" to "for any reason".
    pub fn is_truncated(&self) -> bool {
        !matches!(self, BackfillStop::Complete)
    }

    /// True when re-running the IDENTICAL request could plausibly complete it — i.e. the walk was
    /// cut short by an error rather than by its own budget. `Capped` is deliberately NOT retryable:
    /// an identical re-run would cap at the same page.
    pub fn is_retryable(&self) -> bool {
        matches!(self, BackfillStop::Failed(_))
    }
}

/// What one backward-paging backfill actually did: how many pages it kept, and why it stopped.
///
/// `#[must_use]`: erasing this value is precisely the bug this type exists to prevent, and NOTHING
/// in the tree discards it any more — the one deliberate exception, a `()`-returning compat
/// wrapper each venue face re-exported, was retired once no caller took it.
#[derive(Debug, Clone, PartialEq, Eq)]
#[must_use]
pub struct BackfillOutcome {
    /// Pages successfully fetched AND kept — the historical first element of the
    /// [`backfill_agg_trades_backward`] return pair, unchanged in meaning. On a
    /// [`BackfillStop::Failed`] this counts the pages that DID arrive, not the one that failed.
    pub pages: u32,
    /// Why the walk stopped.
    pub stop: BackfillStop,
}

/// Page aggTrades backward from `before_id` (an EXCLUSIVE upper bound — typically `min_live_id`, so
/// every emitted id stays strictly below the oldest trade the live feed already covers; see
/// global-constraints.md's no-double-count invariant) until a fetched page's earliest ts is at or
/// before `earliest_ts` (that page is the last one needed: emit only its trades with
/// `ts >= earliest_ts`, then stop WITHOUT fetching any further page), or the next `from_id` would
/// hit the id floor (1), or `max_pages` pages have been fetched (truncated = true).
/// `fetch(symbol, from_id)` returns up to 1000 aggTrades ascending by id (id >= from_id). Returns
/// `(pages_fetched, truncated_by_cap)`.
///
/// Paging: the first page uses `from_id = before_id.saturating_sub(1000).max(1)` (so its ids span
/// `[from_id, from_id + 999]`, i.e. max id `before_id - 1 < before_id` — no overlap with anything
/// at/after `before_id`); after a kept page, `before_id` advances to that page's `first().id` and
/// the next `from_id` is recomputed from it the same way. Pages are fetched newest-first
/// (descending) but every page is internally ascending, and the whole `emit` sequence must read
/// oldest-first overall — so pages are buffered and replayed in reverse once the stop condition is
/// known, rather than emitted as they're fetched.
///
/// **Boundary note (why `<=`, not `<`, decides the ts stop):** once a fetched page's minimum ts is
/// *at or before* `earliest_ts`, every older page beyond it can only have even smaller ts (ids/ts
/// are monotonic), so nothing further is needed — stopping on `<=` avoids an extra probe fetch that
/// would immediately discard 100% of its page (`ts >= earliest_ts` would keep nothing from it) just
/// to confirm what the current page's minimum already proves. A strict `<` would require fetching
/// that wasted extra page whenever `earliest_ts` lands exactly on a page's first id.
///
/// **Id-floor defensive filter:** the "no overlap by construction" argument above assumes
/// `from_id = before_id - 1000` exactly. Near the id floor, `.max(1)` clamps `from_id` to `1`
/// instead, so the fetched page can reach ids at/above the current `before_id` (e.g.
/// `before_id = 500` clamps to `from_id = 1`, and a page spans up to id `1000`). Trades with
/// `id >= before_id` are filtered out of every page before it's considered — a no-op in the common
/// (unclamped) case, and required for correctness (no duplicate/overlapping emission) in the
/// clamped one. The `debug_assert!` below re-affirms the invariant on every emitted trade rather
/// than only trusting construction.
///
/// **Infallible-`fetch` compat shim.** This is
/// [`backfill_agg_trades_backward_reported`] with an error-free page source, kept verbatim (same
/// name, same params, same `(pages, truncated_by_cap)` pair) because both venues' unit tests script
/// `fetch` as a plain closure. With no error path, [`BackfillStop::Failed`] is unreachable, so
/// `truncated` still means exactly what it always did — the `max_pages` cap fired. **Production must
/// use the reporting variant**: a `Vec`-returning `fetch` has no way to say "this page failed", and
/// squeezing a REST error through it (`unwrap_or_default`) is the bug the module doc describes.
pub fn backfill_agg_trades_backward(
    symbol: &str,
    before_id: u64,
    earliest_ts: i64,
    max_pages: u32,
    fetch: &dyn Fn(&str, u64) -> Vec<AggTrade>,
    emit: &mut dyn FnMut(AggTrade),
) -> (u32, bool) {
    let infallible = |s: &str, id: u64| -> Result<Vec<AggTrade>, String> { Ok(fetch(s, id)) };
    let out = backfill_agg_trades_backward_reported(
        symbol,
        before_id,
        earliest_ts,
        max_pages,
        &infallible,
        emit,
    );
    (out.pages, out.stop == BackfillStop::Capped)
}

/// The FALLIBLE page source [`backfill_agg_trades_backward_reported`] pages through: given
/// `(symbol, from_id)`, either the (up to 1000, ascending-by-id) aggTrades with `id >= from_id`, or
/// the error that page failed with — the shape of [`fetch_agg_trades_from_id`].
///
/// A named alias rather than the inline `&dyn Fn(&str, u64) -> Result<Vec<AggTrade>, String>`
/// because that inline form trips `clippy::type_complexity`, whose remedy is exactly this. The
/// `'a` is explicit (not left to object-lifetime defaulting) so a caller can pass a closure
/// borrowing locals — which every scripted test, and the production wrapper's own adapter, do.
pub type AggTradePageFetch<'a> = dyn Fn(&str, u64) -> Result<Vec<AggTrade>, String> + 'a;

/// The real backward-paging loop (see [`backfill_agg_trades_backward`] above for the full paging /
/// boundary / id-floor contract, which is unchanged): identical walk, but `fetch` may FAIL and the
/// stop reason is reported rather than flattened to one `bool`.
///
/// A `fetch` error ends the walk with [`BackfillStop::Failed`] — it is emphatically **not** treated
/// as an empty page, which is how the pre-fix code fabricated a clean end-of-history out of a 429.
/// Pages already collected are still emitted (oldest-first, as always): they are good data adjacent
/// to `before_id`, and discarding them on the way out would only add a second loss to the first.
pub fn backfill_agg_trades_backward_reported(
    symbol: &str,
    before_id: u64,
    earliest_ts: i64,
    max_pages: u32,
    fetch: &AggTradePageFetch<'_>,
    emit: &mut dyn FnMut(AggTrade),
) -> BackfillOutcome {
    if before_id <= 1 {
        return BackfillOutcome { pages: 0, stop: BackfillStop::Complete };
    }
    let mut cur_before = before_id;
    let mut pages = 0u32;
    let mut stop = BackfillStop::Complete;
    // Pages collected newest-fetched-first; replayed in reverse below so the overall `emit`
    // sequence is oldest-first (each individual page is already ascending by id/ts).
    let mut kept: Vec<Vec<AggTrade>> = Vec::new();

    loop {
        let from_id = cur_before.saturating_sub(1000).max(1);
        let page = match fetch(symbol, from_id) {
            Ok(p) => p,
            // The whole point: an error stops the walk as `Failed`, never as the `break` an empty
            // page takes below. Older history is missing, not absent.
            Err(e) => {
                stop = BackfillStop::Failed(e);
                break;
            }
        };
        if page.is_empty() {
            break;
        }
        // Defensive id-floor filter — see doc comment above.
        let page: Vec<AggTrade> = page.into_iter().filter(|t| t.id < cur_before).collect();
        if page.is_empty() {
            break;
        }
        pages += 1;

        let page_min_ts = page.first().map(|t| t.tick.ts).unwrap_or(i64::MAX);
        if page_min_ts <= earliest_ts {
            kept.push(page.into_iter().filter(|t| t.tick.ts >= earliest_ts).collect());
            break;
        }

        cur_before = page.first().map(|t| t.id).unwrap_or(cur_before);
        let hit_floor = from_id <= 1;
        kept.push(page);
        if pages == max_pages {
            stop = BackfillStop::Capped;
            break;
        }
        if hit_floor {
            break;
        }
    }

    for t in kept.into_iter().rev().flatten() {
        debug_assert!(t.id < before_id, "backfill emitted id >= before_id (live overlap)");
        emit(t);
    }
    BackfillOutcome { pages, stop }
}

/// Production wrapper around [`backfill_agg_trades_backward_reported`]: wires the pure
/// backward-paging loop to the real REST fetch (`fetch_agg_trades_from_id`, `limit=1000` per page,
/// which is exactly why the loop's `fetch` is fallible here) and hands `emit`
/// just the mapped [`vike_marketdata::TradeTick`] — the [`AggTrade`] id itself is only needed
/// internally, for the no-overlap/no-double-count bookkeeping the pure loop already does.
///
/// `before_id` must be the live trades feed's `market_feed::Feeds::earliest_live_ids` entry for
/// `symbol` (an EXCLUSIVE upper bound — see `global-constraints.md`'s no-double-count invariant:
/// every id this emits is `< before_id`, so it can never overlap what the live feed already
/// covers).
///
/// A REST fetch failure ends the walk as [`BackfillStop::Failed`] and is **not** confused with the
/// end of history (module doc) — the pages already collected are still emitted, and the failure is
/// both returned and `warn!`ed. Logs (does not panic or propagate) on either truncating stop, with a
/// DIFFERENT message per cause — `global-constraints.md`'s "a `max_pages` cap (300) that logs on
/// truncation", plus the error arm that used to log nothing at all.
///
/// A venue face re-exports this behind its own thin `pub fn agg_trades_backfill_reported` that
/// supplies the [`FamilySpec`]. Binance's is the only one today (`crate::trades`, off its `SPEC`
/// const); aster's face carries none — it would build one from its `env`, the way `spec` does.
pub fn agg_trades_backfill_reported(
    spec: &FamilySpec,
    symbol: &str,
    before_id: u64,
    earliest_ts: i64,
    max_pages: u32,
    emit: &mut dyn FnMut(vike_marketdata::TradeTick),
) -> BackfillOutcome {
    let fetch = |s: &str, from_id: u64| fetch_agg_trades_from_id(spec, s, from_id, 1000);
    let out = backfill_agg_trades_backward_reported(
        symbol,
        before_id,
        earliest_ts,
        max_pages,
        &fetch,
        &mut |t| emit(t.tick),
    );
    // Two truncating causes, two messages: a reader must be able to tell "raise the cap" from
    // "this symbol lost history and should be refetched" from the log line alone.
    match &out.stop {
        BackfillStop::Complete => {}
        BackfillStop::Capped => tracing::warn!(
            target: "vike_binance::family::trades",
            venue = spec.venue,
            symbol,
            pages = out.pages,
            max_pages,
            "aggTrades backfill hit the max_pages cap before reaching earliest_ts (partial history)"
        ),
        BackfillStop::Failed(e) => tracing::warn!(
            target: "vike_binance::family::trades",
            venue = spec.venue,
            symbol,
            pages = out.pages,
            max_pages,
            error = %e,
            "aggTrades backfill STOPPED ON A REST ERROR before reaching earliest_ts — history older \
             than the emitted pages is MISSING, not absent; this request is worth retrying"
        ),
    }
    out
}

/// The current latest aggTrade `(id, ts)` for `symbol`, via a single un-throttled `limit=1` REST
/// call — wraps [`fetch_agg_trades_latest`], same shared agent/status-split. `None` on a fetch error
/// or an empty response (e.g. an unknown symbol). Exists purely so a smoke test can derive a real,
/// always-current `before_id`/`earliest_ts` pair instead of a hand-picked constant that would go
/// stale; nothing in production calls it.
pub fn latest_agg_trade(spec: &FamilySpec, symbol: &str) -> Option<(u64, i64)> {
    // Spot-only helper (orderflow's backfill boundary is spot-only): `symbol` is both the exchange
    // symbol and the label, `is_perp = false`.
    let trades = fetch_agg_trades_latest(spec, symbol, symbol, false, 1).ok()?;
    let last = trades.last()?;
    Some((last.id, last.tick.ts))
}
