//! Tick-replay loader: turns `vike-data` `HistStore`-recorded quote/trade/book rows into the
//! existing [`crate::engine::StrategyEngine::run_ticks`] input, producing a
//! [`crate::result::BacktestResult`] — the offline twin of the live tick recorder.
//! Spec: `docs/superpowers/specs/2026-07-08-tick-replay-design.md` (Phase 1, decisions R1–R9);
//! book support added by `docs/superpowers/plans/2026-07-11-book-recording-replay.md` (Task 9).
//!
//! Scope: quote + trade + recorded L2 book events. R3's original "quote + trade only — the store
//! has no L2 book series" note is SUPERSEDED now that the store records `BookUpdate`s
//! (`HistStore::scan_book_updates`) and `Tick`/`StrategyEngine::run_ticks` fold them into
//! `Strategy::on_order_book` (Task 8) — book strategies are backtestable end-to-end from recorded
//! data. `run_ticks`/`Tick`/`FillModelKind` themselves stay feature-free (R9); this whole module
//! is gated behind the `hist-replay` Cargo feature so a default `cargo test/clippy -p vike-backtest`
//! never compiles it. Under `hist-replay` the module compiles against the `HistStore` TRAIT and is
//! DataFusion-free — only `replay_ticks_streaming` (behind the `datafusion-store` feature) names
//! the concrete `DataFusionHist` and touches the DataFusion dep.
//!
//! The pure core is [`merge_ticks`], the per-symbol 3-way merge of one symbol's already
//! ts-ascending `scan_quotes` + `scan_trades` + `scan_book_updates` rows into a single ts-ordered
//! `Vec<Tick>`. **Tie-break at equal ts: Book, then Quote, then Trade** — book first because live
//! derived quotes FOLLOW the book application that produced them (the polymarket live emission
//! order), and quote-before-trade preserves the original R4 documented policy. [`merge_quote_trade`]
//! is kept as a thin 2-way back-compat wrapper (`merge_ticks(q, t, vec![])`) so its pre-existing
//! callers/tests are unaffected.
//!
//! [`replay_ticks`] is the bounded reference loader (R5): for each symbol in
//! [`TickReplayConfig::symbols`] it scans `vike_data::HistStore::scan_quotes`/`scan_trades`/
//! `scan_book_updates` over [`TickReplayConfig::range`], merges them via [`merge_ticks`],
//! optionally seeds closed-bar context via the in-process `vike_model::consolidate_quotes`/
//! `consolidate_trades` (R6 — never `resample_*_to_bars`, no store round-trip; book events never
//! seed bars), and hands everything to the EXISTING [`crate::engine::StrategyEngine::run_ticks`]
//! (R2 — no engine changes). A symbol with ZERO quotes, ZERO trades, AND ZERO book events in range
//! is sparse-tolerant: `tracing::warn!` + skip, not an error — an all-sparse window degrades to an
//! empty-but-valid `BacktestResult` (`run_ticks` on `[]`).
//!
//! `replay_ticks_streaming` is the concrete-`DataFusionHist` opt-in (R5): it obtains quote/trade
//! rows via `DataFusionHist::scan_quotes_stream`/`scan_trades_stream` (an O(batch) decode of each
//! row group) instead of the `HistStore` trait's bounded `scan_quotes`/`scan_trades`. **v1 scope
//! note:** each stream is still collected into a `Vec` before [`merge_ticks`] runs, so the loader
//! as a whole is NOT yet O(batch) end-to-end — the point of this function is API shape +
//! bit-identical results against [`replay_ticks`] (see `streaming_equals_bounded_bit_eq` in
//! `tests/hist_replay.rs`), not the memory win. A true lazy stream merge (consuming iterators
//! without collecting first) is a noted follow-up. Book events have no streaming twin yet — this
//! loader reuses the same bounded `HistStore::scan_book_updates` call as [`replay_ticks`]; a
//! `scan_book_updates_stream` is a noted v1-scope follow-up, matching the quote/trade note above.
//! Both loaders share one core, [`replay_ticks_core`]: they differ ONLY in how they obtain each
//! symbol's `(Vec<QuoteTick>, Vec<TradeTick>, Vec<BookUpdate>)` rows; the merge, the R6 bar seed,
//! the `MisalignedSeededBars` guard, and the `StrategyEngine::new` + `run_ticks` call are one
//! function.
//!
//! # Feed-latency replay (opt-in: [`TickReplayConfig::feed_latency`], default `false`)
//!
//! Recorded ticks carry TWO clocks: the venue's `ts` and the machine's `local_ts` (the shipped
//! dual-timestamp capture; `0` means never stamped — fixtures, backfill, pre-`local_ts` parquet
//! parts). The DEFAULT replay orders every symbol's stream by the venue clock alone, so a replayed
//! strategy sees each quote/trade/book event the instant the VENUE stamped it — earlier than any
//! real consumer ever could. On a proxied feed (Polymarket through the Dublin hop) that
//! systematically flatters every backtest: the strategy reacts to prints it could not yet have had.
//!
//! `feed_latency: true` orders each symbol's merged stream by the ARRIVAL clock
//! ([`tick_arrival_ts`]) instead, so the strategy observes ticks in the sequence a live consumer
//! really received them. What it deliberately does NOT do is retag anything: every `Tick` keeps its
//! own venue `ts`, and that is the only stamp `StrategyEngine::run_ticks` uses to match resting
//! orders, advance `core.now`, settle, and stamp the equity curve. **Matching stays on venue time —
//! the venue matched when it matched, and that is ground truth.** Only DELIVERY ORDER moves.
//!
//! The arrival clock, with both edge cases DOCUMENTED and pinned by tests:
//! - `local_ts <= 0` (missing / never stamped): fall back to the venue `ts`, i.e. that tick keeps
//!   exactly today's position. A partly-stamped tape degrades gracefully rather than collapsing
//!   every unstamped tick to the front of the run.
//! - `local_ts < ts` (CLOCK SKEW — a machine cannot receive a tick before the venue stamped it):
//!   **clamped UP to the venue `ts`**. Trusting the skewed stamp would deliver the tick EARLIER
//!   than the venue published it, which is precisely the look-ahead this mode exists to remove, so
//!   the clamp is the only conservative direction. Modelled feed latency is therefore
//!   `max(local_ts, ts) - ts >= 0`, never negative.
//!
//! The re-order is a STABLE sort of the venue-ts merge, so ticks sharing an arrival stamp keep the
//! Book-then-Quote-then-Trade tie-break and their original relative order. With every `local_ts`
//! zero (or skewed and therefore clamped) the sort key IS the venue ts the sequence is already
//! sorted by, and a stable sort of an already-sorted sequence is the identity — which is why an
//! unstamped tape replays byte-identically in either mode.
//!
//! SCOPE (v1, stated honestly): the re-order is applied PER SYMBOL, to each symbol's merged stream
//! before it reaches `run_ticks`. `run_ticks` then k-way merges the per-symbol streams by VENUE ts,
//! so on a MULTI-symbol replay the cross-symbol interleaving stays venue-ordered even in this mode
//! — only the within-symbol order reflects arrival. Single-symbol replay (the Polymarket case this
//! was built for) is fully arrival-ordered. Lifting the multi-symbol case needs an engine change,
//! which R2 forbids here; it is a noted follow-up.
//!
//! KNOWN CONSEQUENCE, stated rather than hidden: because delivery order changes while each tick's
//! `ts` does not, a genuinely lagging tape makes `run_ticks`'s per-tick clock (`core.now`, and the
//! `equity_ts` stamps it pushes) NON-MONOTONIC in this mode — that is the honest shape of "the
//! consumer saw it later", not a bug. Every default-path consumer of that clock is inert
//! (`deliver_due_*` no-op unless a `latency_model` is armed, `check_resolution_at` unless a
//! resolution source is configured, `settle_variation_margin` unless a cadence is set), so plain
//! feed-latency replay is unaffected. COMBINING `feed_latency` with those opt-in time-driven arms
//! is deliberately out of scope here and is NOT covered by these tests.

use std::fmt;
use std::sync::Arc;

use vike_data::{DataError, HistStore, TsRange};
// The concrete backend is named ONLY by `replay_ticks_streaming` (the DataFusionHist stream loader),
// which is itself behind `datafusion-store` — so a plain `hist-replay` build compiles neither this
// import nor that function, keeping the module DataFusion-free.
#[cfg(feature = "datafusion-store")]
use vike_data::DataFusionHist;
use vike_model::{
    consolidate_quotes, consolidate_trades, Bar, BookUpdate, QuoteTick, Strategy, SymbolProperties,
    TradeTick,
};

use crate::engine::{EngineParams, FillModelKind, SimBroker, StrategyEngine, Tick};
use crate::result::BacktestResult;

/// Merge one symbol's ts-sorted quotes, trades, and recorded book events into a single
/// ts-ordered `Tick` stream. Equal-ts tie-break: **Book, then Quote, then Trade** — live
/// derived quotes FOLLOW the book application that produced them (polymarket pump emission
/// order), and quote-before-trade preserves the pre-book documented policy (R4). Inputs must
/// each be ts-ascending (the HistStore scans guarantee it); linear 3-way step, not a sort.
///
/// MOVES each element into the output rather than cloning it: this function already OWNS its
/// three vectors and drops them on return, so the clone it used to do was pure waste — one
/// `String` allocation per quote/trade and three (symbol + both `Vec<Level>`) per book event, on a
/// tape that is routinely 100M+ rows. Same values, same order, same tie-break: the peeked keys and
/// the arm they select are unchanged, only the way the element reaches `out` is.
pub fn merge_ticks(
    quotes: Vec<QuoteTick>,
    trades: Vec<TradeTick>,
    books: Vec<BookUpdate>,
) -> Vec<Tick> {
    let mut out = Vec::with_capacity(quotes.len() + trades.len() + books.len());
    let mut qi = quotes.into_iter().peekable();
    let mut ti = trades.into_iter().peekable();
    let mut bi = books.into_iter().peekable();
    loop {
        // candidate (ts, priority): Book=0 < Quote=1 < Trade=2
        let b = bi.peek().map(|x| (x.ts, 0u8));
        let qu = qi.peek().map(|x| (x.ts, 1u8));
        let tr = ti.peek().map(|x| (x.ts, 2u8));
        let best = [b, qu, tr].into_iter().flatten().min();
        match best {
            None => break,
            // each `next()` is `Some` — the arm was selected by that same iterator's `peek()`
            Some((_, 0)) => out.extend(bi.next().map(Tick::Book)),
            Some((_, 1)) => out.extend(qi.next().map(Tick::Quote)),
            Some((_, _)) => out.extend(ti.next().map(Tick::Trade)),
        }
    }
    out
}

/// Back-compat 2-way wrapper (pre-book callers/tests): quote-before-trade at equal ts.
pub fn merge_quote_trade(quotes: Vec<QuoteTick>, trades: Vec<TradeTick>) -> Vec<Tick> {
    merge_ticks(quotes, trades, Vec::new())
}

/// A tick's VENUE stamp — the clock `StrategyEngine::run_ticks` matches resting orders on and
/// advances `core.now` with. Feed-latency mode never changes it (see the module doc).
pub fn tick_venue_ts(tick: &Tick) -> i64 {
    match tick {
        Tick::Quote(q) => q.ts,
        Tick::Trade(t) => t.ts,
        Tick::Book(b) => b.ts,
    }
}

/// A tick's recorded MACHINE receive stamp (`0` = never stamped: fixtures, backfill, parquet parts
/// written before the `local_ts` column existed).
pub fn tick_local_ts(tick: &Tick) -> i64 {
    match tick {
        Tick::Quote(q) => q.local_ts,
        Tick::Trade(t) => t.local_ts,
        Tick::Book(b) => b.local_ts,
    }
}

/// The effective ARRIVAL clock of a tick — when a live consumer could first have acted on it, and
/// the ordering key of feed-latency replay. `max(local_ts, venue_ts)`, with an unstamped
/// (`local_ts <= 0`) tick falling back to its venue ts.
///
/// The `max` IS the documented clock-skew clamp: `local_ts < ts` is physically impossible (a
/// machine cannot receive a tick before the venue stamped it), and honouring such a stamp would
/// deliver the tick EARLIER than the venue published it — the exact look-ahead this mode removes.
/// So the modelled feed latency `tick_arrival_ts(t) - tick_venue_ts(t)` is always `>= 0`.
pub fn tick_arrival_ts(tick: &Tick) -> i64 {
    let venue = tick_venue_ts(tick);
    let local = tick_local_ts(tick);
    if local <= 0 {
        venue
    } else {
        local.max(venue)
    }
}

/// [`merge_ticks`], re-ordered by [`tick_arrival_ts`] — the feed-latency delivery sequence.
///
/// A STABLE sort of the venue-ts merge, so equal-arrival ticks keep the Book/Quote/Trade tie-break
/// and their original relative order, and an all-unstamped input (every key = its venue ts, already
/// ascending) comes back exactly as [`merge_ticks`] produced it. No tick's own `ts` is touched:
/// matching stays on venue time, only delivery order changes (module doc).
pub fn merge_ticks_by_arrival(
    quotes: Vec<QuoteTick>,
    trades: Vec<TradeTick>,
    books: Vec<BookUpdate>,
) -> Vec<Tick> {
    let mut out = merge_ticks(quotes, trades, books);
    out.sort_by_key(tick_arrival_ts);
    out
}

/// Which recorded LANES of a series to load. [`SeriesKind::Tick`] (the default) is the frozen
/// behaviour — quotes, trades and book events, merged by [`merge_ticks`]. The narrow variants
/// exist because a CROSS-VENUE replay ([`TickReplayConfig::series`]) generally mixes series of
/// different natures: a Polymarket outcome token contributes its taker TAPE while a BTC spot
/// reference series contributes QUOTES only, and pulling a lane that the driving strategy never
/// asked for silently changes the event sequence it sees.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SeriesKind {
    /// quotes + trades + book events (today's behaviour)
    #[default]
    Tick,
    /// `kind=quote` rows only
    Quote,
    /// `kind=trade` rows only
    Trade,
    /// `kind=book` rows only
    Book,
}

impl SeriesKind {
    /// Does this lane filter admit `kind=quote` rows?
    pub fn wants_quotes(self) -> bool {
        matches!(self, SeriesKind::Tick | SeriesKind::Quote)
    }
    /// Does this lane filter admit `kind=trade` rows?
    pub fn wants_trades(self) -> bool {
        matches!(self, SeriesKind::Tick | SeriesKind::Trade)
    }
    /// Does this lane filter admit `kind=book` rows?
    pub fn wants_books(self) -> bool {
        matches!(self, SeriesKind::Tick | SeriesKind::Book)
    }
}

/// One `(venue, symbol)` series to replay, plus its lane filter — the CROSS-VENUE unit
/// ([`TickReplayConfig::series`]). Deserializable so a `BacktestProfile`'s `[[data.series]]`
/// array maps onto it 1:1 with no parallel type (see `harness::profile::DataCfg`).
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SeriesRef {
    pub venue: String,
    pub symbol: String,
    #[serde(default)]
    pub kind: SeriesKind,
}

impl SeriesRef {
    /// A whole-lane series (`SeriesKind::Tick`) — the shape the legacy `venue` × `symbols`
    /// expansion produces.
    pub fn new(venue: impl Into<String>, symbol: impl Into<String>) -> Self {
        SeriesRef { venue: venue.into(), symbol: symbol.into(), kind: SeriesKind::Tick }
    }
}

/// Config for [`replay_ticks`]/`replay_ticks_streaming`: which `(venue, symbols, range)` to load
/// from the `HistStore`, whether (R6) to seed closed-bar context, and the engine parameters the
/// caller controls.
pub struct TickReplayConfig {
    /// The run's DEFAULT venue: the `venue` half of the `venue` × `symbols` expansion, and — in
    /// every mode, including cross-venue — the `EngineParams::default_venue` tag the loader
    /// forces (see [`TickReplayConfig::params`]).
    pub venue: String,
    /// Symbols to replay, all under [`Self::venue`] — IGNORED when [`Self::series`] is `Some`
    /// (the cross-venue form supersedes this expansion). Multiple symbols are always fine when `seed_bar_interval_ms` is
    /// `None`. With seeding on, `consolidate_quotes`/`consolidate_trades` bucket each symbol's
    /// ticks independently, so two symbols generally end up with bar series of DIFFERENT
    /// lengths — `StrategyEngine::new` asserts all symbol series share one length, so
    /// `replay_ticks` checks this itself and returns `ReplayError::MisalignedSeededBars` instead
    /// of driving that assert into a panic. Phase-1 limitation: bar-seeding is supported for
    /// single-symbol replay only (or the coincidental case where every symbol's consolidated bar
    /// count happens to match).
    pub symbols: Vec<String>,
    pub range: TsRange,
    /// `Some(step_ms)` seeds closed bars via `consolidate_quotes`/`consolidate_trades` on the
    /// scanned ticks (R6); `None` leaves an empty bar context (no `on_bar` firing — `run_ticks`
    /// never calls it regardless). See the `symbols` doc above: with multiple symbols this is
    /// currently single-symbol-safe only — `replay_ticks` returns
    /// `ReplayError::MisalignedSeededBars` rather than panicking when consolidation produces
    /// mismatched per-symbol bar counts.
    ///
    /// **Must be `> 0` when `Some`.** `consolidate_quotes`/`consolidate_trades` compute each
    /// bucket start via `ts.rem_euclid(step_ms)`, which panics on `step_ms == 0` and is
    /// nonsensical for negative steps; `replay_ticks_core` validates this up front and returns
    /// `ReplayError::InvalidBarInterval` instead of letting that panic surface.
    pub seed_bar_interval_ms: Option<i64>,
    /// Caller sets cash/fees/sizer/etc.; the loader OVERWRITES `fill_model` (→
    /// `FillModelKind::Tick`) and `default_venue` (→ `Some(cfg.venue)`) unconditionally, since a
    /// tick replay only makes sense under the L1 spread-crossing fill tier tagged to one venue.
    /// Note: `default_venue` only tags the *seeded bars'* instrument id (via `StrategyEngine::
    /// new`'s `format_instrument`) — `run_ticks` itself routes each tick by its own bare
    /// `tick.symbol`, never by the venue-tagged bar symbol.
    pub params: EngineParams,
    /// Opt-in (default `false`): snap replayed fills to the point-in-time instrument grid recorded
    /// in the store. When `true`, the loader sets `params.properties` to a closure over
    /// `HistStore::properties_as_of(venue, symbol, tick.ts)`, so a fill rounds price→tick / size→step
    /// and gates opening fills below min_qty/min_notional against the grid in effect at that ts
    /// (see `EngineParams.properties` / PR-2a). `false` = raw replay, byte-identical to before this
    /// field existed. Any `params.properties` the caller already set is OVERWRITTEN when this is `true`.
    pub snap_to_properties: bool,
    /// Opt-in (default `false`): deliver each symbol's ticks to the STRATEGY in recorded ARRIVAL
    /// order (`local_ts`, clamped / fallen back per [`tick_arrival_ts`]) instead of venue order, so
    /// a replayed strategy cannot see the book earlier than a live consumer could have.
    ///
    /// Order MATCHING is untouched: every tick keeps its own venue `ts`, which is the only stamp
    /// `StrategyEngine::run_ticks` matches resting orders / advances `core.now` / settles / stamps
    /// the equity curve with. `false` = today's venue-ordered replay, byte-identical. See the
    /// module doc's "Feed-latency replay" section for the skew clamp, the unstamped fallback, and
    /// the multi-symbol scope note.
    pub feed_latency: bool,
    /// Opt-in CROSS-VENUE series list (port backlog G4). `None` (the default) = today's
    /// `venue` × [`Self::symbols`] expansion, byte-identical. `Some` REPLACES that expansion
    /// entirely: each [`SeriesRef`] names its OWN venue and its own lane filter, so one replay
    /// can carry e.g. Polymarket outcome-token trades together with a BTC spot quote series.
    ///
    /// The ENGINE was never the constraint here — `StrategyEngine::run_ticks` routes each tick
    /// by its own payload `symbol` — so this is purely a loader-side widening. Two consequences
    /// the caller owns:
    /// - **Symbols must stay unique across the list.** `run_ticks` resolves a tick to a symbol
    ///   slot by name, so two series sharing a symbol on different venues would collide.
    ///   ([`crate::harness::BacktestProfile::validate`] rejects that at profile-load time.)
    /// - **`snap_to_properties` remains single-venue.** The PIT grid is looked up under
    ///   `EngineParams::default_venue`, which is one string per run — so the two are rejected
    ///   in combination by the profile validator rather than silently reading the wrong venue's
    ///   grid.
    pub series: Option<Vec<SeriesRef>>,
}

impl TickReplayConfig {
    /// The series this config actually loads: [`Self::series`] verbatim when set, else the
    /// frozen `venue` × [`Self::symbols`] whole-lane expansion.
    pub fn resolved_series(&self) -> Vec<SeriesRef> {
        match &self.series {
            Some(series) => series.clone(),
            None => self.symbols.iter().map(|s| SeriesRef::new(&self.venue, s)).collect(),
        }
    }
}

/// Errors from [`replay_ticks`]/`replay_ticks_streaming` — wraps a `HistStore` scan failure.
#[derive(Debug)]
pub enum ReplayError {
    Data(DataError),
    /// Multi-symbol replay with `seed_bar_interval_ms: Some(_)` produced per-symbol consolidated
    /// bar series of different lengths (one entry per symbol in `TickReplayConfig::symbols`
    /// order). `StrategyEngine::new` asserts every symbol's bar series shares one length (R2
    /// forbids relaxing that invariant in `engine.rs`), so `replay_ticks` checks this itself and
    /// returns this error instead of driving that assert into a panic.
    /// `consolidate_quotes`/`consolidate_trades` emit one bar per non-empty time bucket, so two
    /// symbols with different tick density generally diverge in bar count. This is a Phase-1
    /// limitation: single-symbol bar-seeded replay is fully supported; multi-symbol bar-seeded
    /// replay awaits a future relaxation of the engine's per-symbol bar-length invariant.
    MisalignedSeededBars {
        lengths: Vec<usize>,
    },
    /// `TickReplayConfig::seed_bar_interval_ms` was `Some(step)` with `step <= 0`.
    /// `consolidate_quotes`/`consolidate_trades` compute each bucket start via
    /// `ts.rem_euclid(step_ms)`, which panics on `step_ms == 0` and is nonsensical for negative
    /// steps; `replay_ticks_core` checks this up front and returns this error instead of letting
    /// that panic surface. Carries the offending value.
    InvalidBarInterval(i64),
}

impl fmt::Display for ReplayError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ReplayError::Data(e) => write!(f, "tick replay: {e}"),
            ReplayError::MisalignedSeededBars { lengths } => write!(
                f,
                "tick replay: seed_bar_interval_ms produced mismatched per-symbol bar counts \
                 {lengths:?} — multi-symbol bar-seeded replay requires every symbol's \
                 consolidated bar series to share one length, which consolidate_quotes/\
                 consolidate_trades does not guarantee across symbols of differing tick \
                 density; this is a Phase-1 limitation (single-symbol seeding is supported, \
                 multi-symbol seeding awaits StrategyEngine's per-symbol bar-length relaxation)"
            ),
            ReplayError::InvalidBarInterval(step) => write!(
                f,
                "tick replay: seed_bar_interval_ms must be > 0 when Some, got {step} — \
                 consolidate_quotes/consolidate_trades bucket ticks via \
                 ts.rem_euclid(step_ms), which panics on 0 and is nonsensical for negative steps"
            ),
        }
    }
}

impl std::error::Error for ReplayError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ReplayError::Data(e) => Some(e),
            ReplayError::MisalignedSeededBars { .. } => None,
            ReplayError::InvalidBarInterval(_) => None,
        }
    }
}

impl From<DataError> for ReplayError {
    fn from(e: DataError) -> Self {
        ReplayError::Data(e)
    }
}

/// Per-symbol `(symbol, quotes, trades, book events)` rows, already scanned/streamed from the
/// store — the shape both [`replay_ticks`] and `replay_ticks_streaming` hand to
/// [`replay_ticks_core`]. A named alias, not an inline tuple type, to keep clippy's
/// `type_complexity` lint quiet across the three call sites that spell it.
type SymbolTickRows = Vec<(String, Vec<QuoteTick>, Vec<TradeTick>, Vec<BookUpdate>)>;

/// Shared core for [`replay_ticks`]/`replay_ticks_streaming`: takes each symbol's already-
/// obtained `(QuoteTick, TradeTick, BookUpdate)` rows (ts-ascending within each `Vec`, per the
/// `HistStore`/stream contract) and does everything the two loaders have in common — the
/// sparse-symbol skip, [`merge_ticks`], the R6 in-process bar seed, the `MisalignedSeededBars`
/// guard, forcing `fill_model`/`default_venue`, and the `StrategyEngine::new` + `run_ticks` call.
/// The two public entry points differ ONLY in how `rows_by_symbol` is produced (bounded
/// `HistStore` scan vs `DataFusionHist` stream-collect for quotes/trades; both use the same
/// bounded `scan_book_updates` for books).
fn replay_ticks_core<S: Strategy<SimBroker>>(
    rows_by_symbol: SymbolTickRows,
    strategy: S,
    mut cfg: TickReplayConfig,
) -> Result<BacktestResult, ReplayError> {
    // Validate up front: `consolidate_quotes`/`consolidate_trades` compute each bucket start via
    // `ts.rem_euclid(step_ms)`, which panics on `step_ms == 0` and is nonsensical for negative
    // steps. Catch it here rather than letting that panic surface from inside the per-symbol loop
    // below.
    if let Some(step) = cfg.seed_bar_interval_ms {
        if step <= 0 {
            return Err(ReplayError::InvalidBarInterval(step));
        }
    }

    let mut ticks_by_symbol: Vec<(String, Vec<Tick>)> = Vec::new();
    let mut bars_by_symbol: Vec<(String, Vec<Bar>)> = Vec::new();

    for (symbol, quotes, trades, books) in rows_by_symbol {
        if quotes.is_empty() && trades.is_empty() && books.is_empty() {
            tracing::warn!(
                venue = %cfg.venue,
                symbol = %symbol,
                "replay_ticks: no quotes, trades, or book events in range — skipping sparse symbol"
            );
            continue;
        }
        // Bar-seeding is unchanged by book support: book events never seed bars (quotes-else-
        // trades, per R6).
        let bars = match cfg.seed_bar_interval_ms {
            Some(step) if !quotes.is_empty() => consolidate_quotes(&quotes, step),
            Some(step) => consolidate_trades(&trades, step),
            None => Vec::new(),
        };
        // Feed-latency mode (opt-in) re-orders the SAME merged ticks by their recorded arrival
        // clock; `false` takes the frozen venue-ordered merge, byte-identical to before the flag
        // existed. Neither branch rewrites a tick's own `ts` — matching stays on venue time.
        let ticks = if cfg.feed_latency {
            merge_ticks_by_arrival(quotes, trades, books)
        } else {
            merge_ticks(quotes, trades, books)
        };
        ticks_by_symbol.push((symbol.clone(), ticks));
        bars_by_symbol.push((symbol, bars));
    }

    // Guard the latent `StrategyEngine::new` panic ("all symbol series must have the same
    // length (aligned)", engine.rs — R2 forbids touching it): `consolidate_quotes`/
    // `consolidate_trades` bucket each symbol independently, so multiple seeded symbols
    // generally produce bar series of DIFFERENT lengths. With `seed_bar_interval_ms: None` every
    // series is `vec![]` (equal length, no guard needed); single-symbol seeding is always fine.
    // NOTE: this length-set must be computed exactly like engine.rs's own assert (no zero-filter)
    // — engine.rs asserts on the RAW per-symbol lengths, so a guard that filters out zero-length
    // series before counting distinct lengths could pass (e.g. `{N}` from a `{0, N}` situation)
    // while the engine's own assert still sees `{0, N}` and panics, defeating the guard's purpose.
    if cfg.seed_bar_interval_ms.is_some() {
        let lengths: std::collections::BTreeSet<usize> =
            bars_by_symbol.iter().map(|(_, bars)| bars.len()).collect();
        if lengths.len() > 1 {
            return Err(ReplayError::MisalignedSeededBars {
                lengths: bars_by_symbol.iter().map(|(_, bars)| bars.len()).collect(),
            });
        }
    }

    // R: tick replay is the L1 spread-crossing tier by DEFAULT, tagged to exactly one venue. Respect
    // an explicit `L2Book` (depth-capped) choice the caller set — that tier degrades to `Tick`
    // wherever no book exists, so it is safe on any tick tape — but never leave a `Bar` model on the
    // tick lane. `default_venue` is always overwritten (tick replay is single-venue).
    if cfg.params.fill_model != FillModelKind::L2Book {
        cfg.params.fill_model = FillModelKind::Tick;
    }
    cfg.params.default_venue = Some(cfg.venue.clone());

    let mut eng = StrategyEngine::new(bars_by_symbol, strategy, cfg.params);
    Ok(eng.run_ticks(&ticks_by_symbol))
}

/// The `EngineParams.properties` closure over a store's recorded PIT properties: `properties_as_of(venue,
/// symbol, ts)`, with any scan error logged and treated as "no record" (`None`) so a replay never
/// fails on a filter lookup. Public so a caller can build a filter source over any store handle.
#[allow(clippy::type_complexity)] // mirrors the `EngineParams.properties` field type
pub fn properties_source(
    store: Arc<dyn HistStore + Send + Sync>,
) -> Arc<dyn Fn(&str, &str, i64) -> Option<SymbolProperties> + Send + Sync> {
    Arc::new(move |venue: &str, symbol: &str, ts: i64| {
        match store.properties_as_of(venue, symbol, ts) {
            Ok(f) => f,
            Err(e) => {
                tracing::warn!(venue, symbol, ts, error = %e, "properties_as_of failed in replay (treating as no record)");
                None
            }
        }
    })
}

/// Bounded reference path (R5): scans the whole range into memory. Takes an owned `Arc` store
/// handle so `snap_to_properties` can clone it into the `'static` `EngineParams.properties` closure.
/// See the module doc for the per-symbol pipeline (scan → merge → optional bar seed → `run_ticks`).
pub fn replay_ticks<S: Strategy<SimBroker>>(
    store: Arc<dyn HistStore + Send + Sync>,
    strategy: S,
    mut cfg: TickReplayConfig,
) -> Result<BacktestResult, ReplayError> {
    let series = cfg.resolved_series();
    let mut rows: SymbolTickRows = Vec::with_capacity(series.len());
    for s in &series {
        let quotes = if s.kind.wants_quotes() {
            store.scan_quotes(&s.venue, &s.symbol, cfg.range)?
        } else {
            Vec::new()
        };
        let trades = if s.kind.wants_trades() {
            store.scan_trades(&s.venue, &s.symbol, cfg.range)?
        } else {
            Vec::new()
        };
        let books = if s.kind.wants_books() {
            store.scan_book_updates(&s.venue, &s.symbol, cfg.range)?
        } else {
            Vec::new()
        };
        rows.push((s.symbol.clone(), quotes, trades, books));
    }
    if cfg.snap_to_properties {
        cfg.params.properties = Some(properties_source(store));
    }
    replay_ticks_core(rows, strategy, cfg)
}

/// Concrete-`DataFusionHist` streaming opt-in (R5): same per-symbol pipeline as [`replay_ticks`]
/// (via the shared [`replay_ticks_core`]) but obtains each symbol's rows through
/// `DataFusionHist::scan_quotes_stream`/`scan_trades_stream` instead of the `HistStore` trait's
/// bounded `scan_quotes`/`scan_trades`. See the module doc for the v1 scope note (streams are
/// still collected to a `Vec` before merging — the memory win is a follow-up, this task is API +
/// result parity).
///
/// Behind `datafusion-store`: it names the concrete `DataFusionHist` and calls its
/// backend-specific stream scanners (NOT on the `HistStore` trait), so a trait-only `hist-replay`
/// build does not compile it — unlike [`replay_ticks`], which is trait-generic and always available.
#[cfg(feature = "datafusion-store")]
pub fn replay_ticks_streaming<S: Strategy<SimBroker>>(
    store: Arc<DataFusionHist>,
    strategy: S,
    mut cfg: TickReplayConfig,
) -> Result<BacktestResult, ReplayError> {
    let series = cfg.resolved_series();
    let mut rows: SymbolTickRows = Vec::with_capacity(series.len());
    for s in &series {
        let quotes: Vec<QuoteTick> = if s.kind.wants_quotes() {
            store
                .scan_quotes_stream(&s.venue, &s.symbol, cfg.range)?
                .collect::<Result<Vec<_>, DataError>>()?
        } else {
            Vec::new()
        };
        let trades: Vec<TradeTick> = if s.kind.wants_trades() {
            store
                .scan_trades_stream(&s.venue, &s.symbol, cfg.range)?
                .collect::<Result<Vec<_>, DataError>>()?
        } else {
            Vec::new()
        };
        // No streaming twin for book events yet (v1 scope note above) — reuse the same bounded
        // scan the non-streaming loader uses.
        let books = if s.kind.wants_books() {
            store.scan_book_updates(&s.venue, &s.symbol, cfg.range)?
        } else {
            Vec::new()
        };
        rows.push((s.symbol.clone(), quotes, trades, books));
    }
    if cfg.snap_to_properties {
        let s: Arc<dyn HistStore + Send + Sync> = store;
        cfg.params.properties = Some(properties_source(s));
    }
    replay_ticks_core(rows, strategy, cfg)
}

#[cfg(test)]
mod tests {
    use super::*;
    use vike_model::BookUpdateKind;

    fn quote(ts: i64, bid: f64) -> QuoteTick {
        QuoteTick {
            ts,
            local_ts: 0,
            bid,
            ask: bid + 1.0,
            bid_size: 1.0,
            ask_size: 1.0,
            symbol: "TKN".to_string(),
        }
    }

    fn trade(ts: i64, price: f64) -> TradeTick {
        TradeTick {
            ts,
            local_ts: 0,
            price,
            size: 1.0,
            is_buyer_maker: false,
            symbol: "TKN".to_string(),
        }
    }

    #[test]
    fn merge_interleaves_by_ts() {
        let quotes = vec![quote(1, 10.0), quote(3, 30.0)];
        let trades = vec![trade(2, 20.0), trade(4, 40.0)];
        let merged = merge_quote_trade(quotes, trades);
        assert_eq!(merged.len(), 4);
        match &merged[0] {
            Tick::Quote(q) => {
                assert_eq!(q.ts, 1);
                assert_eq!(q.bid, 10.0);
            }
            other => panic!("expected Quote at index 0, got {other:?}"),
        }
        match &merged[1] {
            Tick::Trade(t) => {
                assert_eq!(t.ts, 2);
                assert_eq!(t.price, 20.0);
            }
            other => panic!("expected Trade at index 1, got {other:?}"),
        }
        match &merged[2] {
            Tick::Quote(q) => {
                assert_eq!(q.ts, 3);
                assert_eq!(q.bid, 30.0);
            }
            other => panic!("expected Quote at index 2, got {other:?}"),
        }
        match &merged[3] {
            Tick::Trade(t) => {
                assert_eq!(t.ts, 4);
                assert_eq!(t.price, 40.0);
            }
            other => panic!("expected Trade at index 3, got {other:?}"),
        }
    }

    #[test]
    fn merge_equal_ts_quote_first() {
        let quotes = vec![quote(5, 99.0)];
        let trades = vec![trade(5, 100.0)];
        let merged = merge_quote_trade(quotes, trades);
        assert_eq!(merged.len(), 2);
        match &merged[0] {
            Tick::Quote(q) => {
                assert_eq!(q.ts, 5);
                assert_eq!(q.bid, 99.0);
            }
            other => panic!("expected Quote to precede Trade at equal ts, got {other:?}"),
        }
        match &merged[1] {
            Tick::Trade(t) => {
                assert_eq!(t.ts, 5);
                assert_eq!(t.price, 100.0);
            }
            other => panic!("expected Trade second at equal ts, got {other:?}"),
        }
    }

    #[test]
    fn merge_one_empty() {
        // empty trades -> all quotes, order preserved
        let quotes = vec![quote(1, 10.0), quote(2, 20.0)];
        let merged = merge_quote_trade(quotes, vec![]);
        assert_eq!(merged.len(), 2);
        match &merged[0] {
            Tick::Quote(q) => assert_eq!(q.ts, 1),
            other => panic!("expected Quote, got {other:?}"),
        }
        match &merged[1] {
            Tick::Quote(q) => assert_eq!(q.ts, 2),
            other => panic!("expected Quote, got {other:?}"),
        }

        // empty quotes -> all trades, order preserved
        let trades = vec![trade(3, 30.0), trade(4, 40.0)];
        let merged = merge_quote_trade(vec![], trades);
        assert_eq!(merged.len(), 2);
        match &merged[0] {
            Tick::Trade(t) => assert_eq!(t.ts, 3),
            other => panic!("expected Trade, got {other:?}"),
        }
        match &merged[1] {
            Tick::Trade(t) => assert_eq!(t.ts, 4),
            other => panic!("expected Trade, got {other:?}"),
        }
    }

    #[test]
    fn merge_both_empty() {
        let merged = merge_quote_trade(vec![], vec![]);
        assert!(merged.is_empty());
    }

    /// Same helpers as `quote`/`trade` but with an explicit machine receive stamp.
    fn quote_at(ts: i64, local_ts: i64, bid: f64) -> QuoteTick {
        QuoteTick { local_ts, ..quote(ts, bid) }
    }

    fn trade_at(ts: i64, local_ts: i64, price: f64) -> TradeTick {
        TradeTick { local_ts, ..trade(ts, price) }
    }

    /// The arrival clock's three documented cases, at the source.
    #[test]
    fn arrival_clock_falls_back_clamps_and_lags() {
        // unstamped -> venue ts (today's position)
        let unstamped = Tick::Quote(quote_at(1_000, 0, 10.0));
        assert_eq!(tick_venue_ts(&unstamped), 1_000);
        assert_eq!(tick_local_ts(&unstamped), 0);
        assert_eq!(tick_arrival_ts(&unstamped), 1_000);

        // a real lag -> the recorded receive stamp
        let lagged = Tick::Trade(trade_at(1_000, 1_250, 10.0));
        assert_eq!(tick_arrival_ts(&lagged), 1_250);

        // CLOCK SKEW (local before venue — physically impossible) -> clamped UP to the venue ts,
        // so the modelled latency is never negative and never a look-ahead.
        let skewed = Tick::Quote(quote_at(1_000, 400, 10.0));
        assert_eq!(tick_arrival_ts(&skewed), 1_000, "skewed local_ts clamps up to the venue ts");
        assert!(tick_arrival_ts(&skewed) - tick_venue_ts(&skewed) >= 0);

        // a negative stamp is treated as "never stamped", not as a huge negative arrival
        let negative = Tick::Quote(quote_at(1_000, -5, 10.0));
        assert_eq!(tick_arrival_ts(&negative), 1_000);
    }

    /// OFF-PATH PROOF at the merge level: with every `local_ts` unstamped (the whole existing
    /// fixture corpus), the arrival merge IS the venue merge — same ticks, same order, same
    /// tie-break. A stable sort whose key equals the already-ascending venue ts is the identity.
    #[test]
    fn arrival_merge_equals_venue_merge_when_unstamped() {
        let quotes = vec![quote(1, 10.0), quote(3, 30.0), quote(3, 31.0)];
        let trades = vec![trade(2, 20.0), trade(3, 33.0)];
        let books = vec![BookUpdate {
            ts: 3,
            local_ts: 0,
            seq: 1,
            kind: BookUpdateKind::Snapshot,
            tick_size: 0.01,
            bids: vec![(0.4, 1.0)],
            asks: vec![(0.6, 1.0)],
            symbol: "TKN".to_string(),
        }];

        let venue = merge_ticks(quotes.clone(), trades.clone(), books.clone());
        let arrival = merge_ticks_by_arrival(quotes, trades, books);
        assert_eq!(venue.len(), arrival.len());
        for (v, a) in venue.iter().zip(&arrival) {
            assert_eq!(tick_venue_ts(v), tick_venue_ts(a));
            assert_eq!(
                std::mem::discriminant(v),
                std::mem::discriminant(a),
                "same tick KIND at the same position (Book/Quote/Trade tie-break preserved)"
            );
        }
    }

    /// ON with a lagging stamp: the LATE tick is delivered after the one that arrived first, even
    /// though the venue stamped it earlier — and its own venue `ts` is untouched (matching is still
    /// venue time).
    #[test]
    fn arrival_merge_reorders_a_lagging_tick() {
        // Venue order: q@1000, q@2000. Arrival order: q@2000 (recv 2100) BEFORE q@1000 (recv 5000).
        let quotes = vec![quote_at(1_000, 5_000, 10.0), quote_at(2_000, 2_100, 20.0)];
        let merged = merge_ticks_by_arrival(quotes, vec![], vec![]);
        assert_eq!(merged.len(), 2);
        assert_eq!(tick_venue_ts(&merged[0]), 2_000, "the tick that ARRIVED first goes first");
        assert_eq!(tick_arrival_ts(&merged[0]), 2_100);
        assert_eq!(tick_venue_ts(&merged[1]), 1_000, "venue ts is NOT rewritten by the re-order");
        assert_eq!(tick_arrival_ts(&merged[1]), 5_000);
    }

    /// A PARTLY stamped tape: the unstamped tick falls back to its venue ts and keeps its place;
    /// only the stamped, genuinely-late tick moves.
    #[test]
    fn arrival_merge_mixes_stamped_and_unstamped_cleanly() {
        let quotes = vec![
            quote_at(1_000, 0, 10.0),     // unstamped -> arrival 1000
            quote_at(2_000, 9_000, 20.0), // 7s late   -> arrival 9000
            quote_at(3_000, 0, 30.0),     // unstamped -> arrival 3000
        ];
        let merged = merge_ticks_by_arrival(quotes, vec![], vec![]);
        let order: Vec<i64> = merged.iter().map(tick_venue_ts).collect();
        assert_eq!(order, vec![1_000, 3_000, 2_000], "only the late stamped tick moves");
    }

    /// A SKEWED stamp must not jump the queue: clamped to its venue ts, the tick stays exactly
    /// where the venue-ordered merge put it.
    #[test]
    fn arrival_merge_clamps_skew_instead_of_delivering_early() {
        let quotes = vec![
            quote_at(1_000, 0, 10.0),
            // local_ts claims it arrived at 500, BEFORE the 1000-stamped tick above; clamping to
            // its own venue ts (2000) keeps it second instead of promoting it to first.
            quote_at(2_000, 500, 20.0),
        ];
        let merged = merge_ticks_by_arrival(quotes, vec![], vec![]);
        let order: Vec<i64> = merged.iter().map(tick_venue_ts).collect();
        assert_eq!(order, vec![1_000, 2_000], "a skewed stamp never delivers a tick early");
    }

    #[test]
    fn merge_ticks_equal_ts_book_quote_trade_order() {
        let books = vec![BookUpdate {
            ts: 5,
            local_ts: 0,
            seq: 1,
            kind: BookUpdateKind::Snapshot,
            tick_size: 0.01,
            bids: vec![(0.4, 1.0)],
            asks: vec![(0.6, 1.0)],
            symbol: "TKN".to_string(),
        }];
        let merged = merge_ticks(vec![quote(5, 99.0)], vec![trade(5, 100.0)], books);
        assert!(matches!(merged[0], Tick::Book(_)), "book first at equal ts");
        assert!(matches!(merged[1], Tick::Quote(_)));
        assert!(matches!(merged[2], Tick::Trade(_)));
    }
}
