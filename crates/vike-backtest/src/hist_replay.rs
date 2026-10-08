//! Tick-replay loader: turns `vike-data` `HistStore`-recorded quote/trade/book rows into the
//! existing [`vike_sim::StrategyEngine::run_ticks`] input, producing a
//! [`vike_analytics::BacktestResult`] — the offline twin of the live tick recorder.
//! Spec: `docs/superpowers/specs/2026-07-08-tick-replay-design.md` (Phase 1, decisions R1–R9);
//! book support added by `docs/superpowers/plans/2026-07-11-book-recording-replay.md`.
//!
//! Scope: quote + trade + recorded L2 book events. R3's original "quote + trade only — the store
//! has no L2 book series" note is SUPERSEDED now that the store records `BookUpdate`s
//! (`HistStore::scan_book_updates`) and `Tick`/`StrategyEngine::run_ticks` fold them into
//! `Strategy::on_order_book` — book strategies are backtestable end-to-end from recorded
//! data. `run_ticks`/`Tick`/`FillModelKind` themselves stay feature-free (R9); this whole module
//! is a DEFAULT build of this crate (⚠ was gated behind the `hist-replay` Cargo feature until the
//! 2026-09-27 feature collapse made it unconditional), and it compiles against the `HistStore`
//! TRAIT and is DataFusion-free — nothing in it names the concrete `DataFusionHist`.
//!
//! The pure core is [`merge_ticks`], the per-symbol 3-way merge of one symbol's already
//! ts-ascending `scan_quotes` + `scan_trades` + `scan_book_updates` rows into a single ts-ordered
//! `Vec<Tick>`. **Tie-break at equal ts: Book, then Quote, then Trade** — book first because live
//! derived quotes FOLLOW the book application that produced them (the polymarket live emission
//! order), and quote-before-trade preserves the original R4 documented policy. [`merge_quote_trade`]
//! is kept as a thin 2-way back-compat wrapper (`merge_ticks(q, t, vec![])`) so its pre-existing
//! callers/tests are unaffected.
//!
//! [`replay_ticks`] is the loader (R5 — bounded: each symbol's whole range is scanned into
//! memory): for each symbol in
//! [`TickReplayConfig::symbols`] it scans `vike_data::HistStore::scan_quotes`/`scan_trades`/
//! `scan_book_updates` over [`TickReplayConfig::range`], merges them via [`merge_ticks`],
//! optionally seeds closed-bar context via the in-process `vike_model::consolidate_quotes`/
//! `consolidate_trades` (R6 — never `resample_*_to_bars`, no store round-trip; book events never
//! seed bars), and hands everything to the EXISTING [`vike_sim::StrategyEngine::run_ticks`]
//! (R2 — no engine changes). A symbol with ZERO quotes, ZERO trades, AND ZERO book events in range
//! is sparse-tolerant: `tracing::warn!` + skip, not an error — an all-sparse window degrades to an
//! empty-but-valid `BacktestResult` (`run_ticks` on `[]`).
//!
//! There is NO streaming twin of this loader. R5 described one as an opt-in for large windows
//! (`replay_ticks_streaming`, over `DataFusionHist::scan_quotes_stream`/`scan_trades_stream`) and it
//! was built, then DELETED with its stream scans on 2026-10-01: nothing outside its own test ever
//! called it, it still collected each stream into a `Vec` before merging (so it was never O(batch)
//! end-to-end), and those scans read a symbol's per-symbol directory ALONE, so a symbol held in a
//! `group=` directory replayed as if it had no quotes or trades. A streaming loader built later has
//! to read every layout a symbol can live in, as `HistStore::scan_quotes_capped` does.
//! Everything after the scan lives in one function, [`replay_ticks_core`]: the merge, the R6 bar
//! seed, the `MisalignedSeededBars` guard, and the `StrategyEngine::new` + `run_ticks` call.
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
use vike_model::{
    Bar, BookUpdate, QuoteTick, Strategy, SymbolProperties, TradeTick, consolidate_quotes,
    consolidate_trades,
};

use vike_analytics::BacktestResult;
use vike_sim::{EngineParams, FillModelKind, SimBroker, StrategyEngine, Tick};

/// Merge one symbol's ts-sorted quotes, trades, and recorded book events into a single
/// ts-ordered `Tick` stream. Equal-ts tie-break: **Book, then Quote, then Trade** — live
/// derived quotes FOLLOW the book application that produced them (polymarket pump emission
/// order), and quote-before-trade preserves the pre-book documented policy (R4). Inputs must
/// each be ts-ascending (the HistStore scans guarantee it); linear 3-way step, not a sort.
///
/// MOVES each element into the output rather than cloning it: this function already OWNS its
/// three vectors and drops them on return, so the clone it used to do was pure waste — one
/// `String` allocation per quote/trade and three (symbol + both `Vec<BookLevel>`) per book event, on a
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
    if local <= 0 { venue } else { local.max(venue) }
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

/// Config for [`replay_ticks`]: which `(venue, symbols, range)` to load
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

/// Errors from [`replay_ticks`] — wraps a `HistStore` scan failure.
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

/// Per-symbol `(symbol, quotes, trades, book events)` rows, already scanned from the store — the
/// shape [`replay_ticks`] hands to [`replay_ticks_core`]. A named alias, not an inline tuple
/// type, to keep clippy's `type_complexity` lint quiet across the call sites that spell it.
type SymbolTickRows = Vec<(String, Vec<QuoteTick>, Vec<TradeTick>, Vec<BookUpdate>)>;

/// Everything [`replay_ticks`] does after the scan: takes each symbol's already-obtained
/// `(QuoteTick, TradeTick, BookUpdate)` rows (ts-ascending within each `Vec`, per the `HistStore`
/// contract) and does the sparse-symbol skip, [`merge_ticks`], the R6 in-process bar seed, the
/// `MisalignedSeededBars` guard, forcing `fill_model`/`default_venue`, and the
/// `StrategyEngine::new` + `run_ticks` call.
fn replay_ticks_core<S: Strategy<SimBroker>>(
    rows_by_symbol: SymbolTickRows,
    strategy: S,
    mut cfg: TickReplayConfig,
) -> Result<BacktestResult, ReplayError> {
    // Validate up front: `consolidate_quotes`/`consolidate_trades` compute each bucket start via
    // `ts.rem_euclid(step_ms)`, which panics on `step_ms == 0` and is nonsensical for negative
    // steps. Catch it here rather than letting that panic surface from inside the per-symbol loop
    // below.
    if let Some(step) = cfg.seed_bar_interval_ms
        && step <= 0
    {
        return Err(ReplayError::InvalidBarInterval(step));
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

/// Bounded path (R5): scans the whole range into memory. Takes an owned `Arc` store
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

#[path = "hist_replay_tests.rs"]
#[cfg(test)]
mod hist_replay_tests;
