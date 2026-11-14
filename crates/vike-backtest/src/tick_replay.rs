//! Tick-replay loader: turns `vike-data` `HistoryStore`-recorded quote/trade/book rows into the
//! existing [`vike_simulator::StrategyEngine::run_ticks`] input, producing a
//! [`vike_analytics::BacktestResult`] — the offline twin of the live tick recorder.
//! Spec: `docs/superpowers/specs/2026-07-08-tick-replay-design.md` (Phase 1, decisions R1–R9);
//! book support added by `docs/superpowers/plans/2026-07-11-book-recording-replay.md`.
//!
//! Scope: quote + trade + recorded L2 book events. R3's original "quote + trade only — the store
//! has no L2 book series" note is SUPERSEDED now that the store records `L2BookUpdate`s
//! (`HistoryStore::scan_book_updates`) and `TickEvent`/`StrategyEngine::run_ticks` fold them into
//! `Strategy::on_order_book` — book strategies are backtestable end-to-end from recorded
//! data. `run_ticks`/`TickEvent`/`FillModelKind` themselves stay feature-free (R9); this whole module
//! is a DEFAULT build of this crate (⚠ was gated behind the `hist-replay` Cargo feature until the
//! 2026-09-27 feature collapse made it unconditional), and it compiles against the `HistoryStore`
//! TRAIT and is DataFusion-free — nothing in it names the concrete `HistoryStoreDataFusion`.
//!
//! The pure core is [`merge_ticks`], the per-symbol 3-way merge of one symbol's already
//! ts-ascending `scan_quotes` + `scan_trades` + `scan_book_updates` rows into a single ts-ordered
//! `Vec<TickEvent>`. **Tie-break at equal ts: Book, then Quote, then Trade** — book first because live
//! derived quotes FOLLOW the book application that produced them (the polymarket live emission
//! order), and quote-before-trade preserves the original R4 documented policy. [`merge_quote_trade`]
//! is kept as a thin 2-way back-compat wrapper (`merge_ticks(q, t, vec![])`) so its pre-existing
//! callers/tests are unaffected.
//!
//! [`replay_ticks`] is the loader (R5 — bounded: each symbol's whole range is scanned into
//! memory): for each symbol in
//! [`TickReplayConfig::symbols`] it scans `vike_data::HistoryStore::scan_quotes`/`scan_trades`/
//! `scan_book_updates` over [`TickReplayConfig::range`], merges them via [`merge_ticks`],
//! optionally seeds closed-bar context via the in-process `vike_marketdata::consolidate_quotes`/
//! `consolidate_trades` (R6 — never `resample_*_to_bars`, no store round-trip; book events never
//! seed bars), and hands everything to the EXISTING [`vike_simulator::StrategyEngine::run_ticks`]
//! (R2 — no engine changes). A symbol with ZERO quotes, ZERO trades, AND ZERO book events in range
//! is sparse-tolerant: `tracing::warn!` + skip, not an error — an all-sparse window degrades to an
//! empty-but-valid `BacktestResult` (`run_ticks` on `[]`).
//!
//! There is NO streaming twin of this loader. R5 described one as an opt-in for large windows
//! (`replay_ticks_streaming`, over `HistoryStoreDataFusion::scan_quotes_stream`/`scan_trades_stream`) and it
//! was built, then DELETED with its stream scans on 2026-10-01: nothing outside its own test ever
//! called it, it still collected each stream into a `Vec` before merging (so it was never O(batch)
//! end-to-end), and those scans read a symbol's per-symbol directory ALONE, so a symbol held in a
//! `group=` directory replayed as if it had no quotes or trades. A streaming loader built later has
//! to read every layout a symbol can live in, as `HistoryStore::scan_quotes_capped` does.
//! Everything after the scan lives in one function, [`replay_ticks_core`]: the merge, the R6 bar
//! seed, the `MisalignedSeededBars` guard, and the `StrategyEngine::new` + `run_ticks` call.
//!
//! # Feed-latency replay (opt-in: [`TickReplayConfig::feed_latency`], default `false`)
//!
//! Recorded ticks carry TWO clocks: the venue's `ts` and the machine's `local_timestamp` (the shipped
//! dual-timestamp capture; `0` means never stamped — fixtures, backfill, pre-`local_timestamp` parquet
//! parts). The DEFAULT replay orders every symbol's stream by the venue clock alone, so a replayed
//! strategy sees each quote/trade/book event the instant the VENUE stamped it — earlier than any
//! real consumer ever could. On a proxied feed (Polymarket through the Dublin hop) that
//! systematically flatters every backtest: the strategy reacts to prints it could not yet have had.
//!
//! `feed_latency: true` orders each symbol's merged stream by the ARRIVAL clock
//! ([`tick_arrival_timestamp`]) instead, so the strategy observes ticks in the sequence a live consumer
//! really received them. What it deliberately does NOT do is retag anything: every `TickEvent` keeps its
//! own venue `ts`, and that is the only stamp `StrategyEngine::run_ticks` uses to match resting
//! orders, advance `core.now`, settle, and stamp the equity curve. **Matching stays on venue time —
//! the venue matched when it matched, and that is ground truth.** Only DELIVERY ORDER moves.
//!
//! The arrival clock, with both edge cases DOCUMENTED and pinned by tests:
//! - `local_timestamp <= 0` (missing / never stamped): fall back to the venue `ts`, i.e. that tick keeps
//!   exactly today's position. A partly-stamped tape degrades gracefully rather than collapsing
//!   every unstamped tick to the front of the run.
//! - `local_timestamp < ts` (CLOCK SKEW — a machine cannot receive a tick before the venue stamped it):
//!   **clamped UP to the venue `ts`**. Trusting the skewed stamp would deliver the tick EARLIER
//!   than the venue published it, which is precisely the look-ahead this mode exists to remove, so
//!   the clamp is the only conservative direction. Modelled feed latency is therefore
//!   `max(local_timestamp, ts) - ts >= 0`, never negative.
//!
//! The re-order is a STABLE sort of the venue-ts merge, so ticks sharing an arrival stamp keep the
//! Book-then-Quote-then-Trade tie-break and their original relative order. With every `local_timestamp`
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
//! `equity_timestamp` stamps it pushes) NON-MONOTONIC in this mode — that is the honest shape of "the
//! consumer saw it later", not a bug. Every default-path consumer of that clock is inert
//! (`deliver_due_*` no-op unless a `latency_model` is armed, `check_resolution_at` unless a
//! resolution source is configured, `settle_variation_margin` unless a cadence is set), so plain
//! feed-latency replay is unaffected. COMBINING `feed_latency` with those opt-in time-driven arms
//! is deliberately out of scope here and is NOT covered by these tests.

use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, Mutex, OnceLock};

use vike_data::{HistoryStore, HistoryStoreError, StoreKindId, TimestampRange};
use vike_marketdata::{
    Bar, L2BookUpdate, TickQuote, TickTrade, consolidate_quotes, consolidate_trades,
};
use vike_model::{Strategy, SymbolProperties};

use vike_analytics::BacktestResult;
use vike_simulator::{
    FillModelKind, OrderBrokerSimulator, StrategyEngine, StrategyEngineConfig, TickEvent,
};

/// Merge one symbol's ts-sorted quotes, trades, and recorded book events into a single
/// ts-ordered `TickEvent` stream. Equal-ts tie-break: **Book, then Quote, then Trade** — live
/// derived quotes FOLLOW the book application that produced them (polymarket pump emission
/// order), and quote-before-trade preserves the pre-book documented policy (R4). Inputs must
/// each be ts-ascending (the HistoryStore scans guarantee it); linear 3-way step, not a sort.
///
/// MOVES each element into the output rather than cloning it: this function already OWNS its
/// three vectors and drops them on return, so the clone it used to do was pure waste — one
/// `String` allocation per quote/trade and three (symbol + both `Vec<L2BookLevel>`) per book event, on a
/// tape that is routinely 100M+ rows. Same values, same order, same tie-break: the peeked keys and
/// the arm they select are unchanged, only the way the element reaches `out` is.
pub fn merge_ticks(
    quotes: Vec<TickQuote>,
    trades: Vec<TickTrade>,
    books: Vec<L2BookUpdate>,
) -> Vec<TickEvent> {
    let mut out = Vec::with_capacity(quotes.len() + trades.len() + books.len());
    let mut qi = quotes.into_iter().peekable();
    let mut ti = trades.into_iter().peekable();
    let mut bi = books.into_iter().peekable();
    loop {
        // candidate (ts, priority): Book=0 < Quote=1 < Trade=2
        let b = bi.peek().map(|x| (x.timestamp, 0u8));
        let qu = qi.peek().map(|x| (x.timestamp, 1u8));
        let tr = ti.peek().map(|x| (x.timestamp, 2u8));
        let best = [b, qu, tr].into_iter().flatten().min();
        match best {
            None => break,
            // each `next()` is `Some` — the arm was selected by that same iterator's `peek()`
            Some((_, 0)) => out.extend(bi.next().map(TickEvent::Book)),
            Some((_, 1)) => out.extend(qi.next().map(TickEvent::Quote)),
            Some((_, _)) => out.extend(ti.next().map(TickEvent::Trade)),
        }
    }
    out
}

/// Back-compat 2-way wrapper (pre-book callers/tests): quote-before-trade at equal ts.
pub fn merge_quote_trade(quotes: Vec<TickQuote>, trades: Vec<TickTrade>) -> Vec<TickEvent> {
    merge_ticks(quotes, trades, Vec::new())
}

/// A tick's VENUE stamp — the clock `StrategyEngine::run_ticks` matches resting orders on and
/// advances `core.now` with. Feed-latency mode never changes it (see the module doc).
pub fn tick_venue_timestamp(tick: &TickEvent) -> i64 {
    match tick {
        TickEvent::Quote(q) => q.timestamp,
        TickEvent::Trade(t) => t.timestamp,
        TickEvent::Book(b) => b.timestamp,
    }
}

/// A tick's recorded MACHINE receive stamp (`0` = never stamped: fixtures, backfill, parquet parts
/// written before the `local_timestamp` column existed).
pub fn tick_local_timestamp(tick: &TickEvent) -> i64 {
    match tick {
        TickEvent::Quote(q) => q.local_timestamp,
        TickEvent::Trade(t) => t.local_timestamp,
        TickEvent::Book(b) => b.local_timestamp,
    }
}

/// The effective ARRIVAL clock of a tick — when a live consumer could first have acted on it, and
/// the ordering key of feed-latency replay. `max(local_timestamp, venue_timestamp)`, with an unstamped
/// (`local_timestamp <= 0`) tick falling back to its venue ts.
///
/// The `max` IS the documented clock-skew clamp: `local_timestamp < ts` is physically impossible (a
/// machine cannot receive a tick before the venue stamped it), and honouring such a stamp would
/// deliver the tick EARLIER than the venue published it — the exact look-ahead this mode removes.
/// So the modelled feed latency `tick_arrival_timestamp(t) - tick_venue_timestamp(t)` is always `>= 0`.
pub fn tick_arrival_timestamp(tick: &TickEvent) -> i64 {
    let venue = tick_venue_timestamp(tick);
    let local = tick_local_timestamp(tick);
    if local <= 0 { venue } else { local.max(venue) }
}

/// [`merge_ticks`], re-ordered by [`tick_arrival_timestamp`] — the feed-latency delivery sequence.
///
/// A STABLE sort of the venue-ts merge, so equal-arrival ticks keep the Book/Quote/Trade tie-break
/// and their original relative order, and an all-unstamped input (every key = its venue ts, already
/// ascending) comes back exactly as [`merge_ticks`] produced it. No tick's own `ts` is touched:
/// matching stays on venue time, only delivery order changes (module doc).
pub fn merge_ticks_by_arrival(
    quotes: Vec<TickQuote>,
    trades: Vec<TickTrade>,
    books: Vec<L2BookUpdate>,
) -> Vec<TickEvent> {
    let mut out = merge_ticks(quotes, trades, books);
    out.sort_by_key(tick_arrival_timestamp);
    out
}

/// Which recorded LANES of a series to load. [`SeriesKind::Tick`] (the default) is the frozen
/// behaviour — quotes, trades and book events, merged by [`merge_ticks`]. The narrow variants
/// exist because a CROSS-VENUE replay ([`TickReplayConfig::series`]) generally mixes series of
/// different natures: a Polymarket outcome token contributes its taker TAPE while a BTC spot
/// reference series contributes QUOTES only, and pulling a lane that the driving strategy never
/// asked for silently changes the event sequence it sees.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Deserialize)]
#[cfg_attr(feature = "profile-schema", derive(schemars::JsonSchema))]
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
    /// The store kinds this lane filter admits, in load order — the view this subset is OF.
    /// [`SeriesKind::Tick`] is not a store kind: it is the three tick lanes together.
    pub fn store_kinds(self) -> &'static [StoreKindId] {
        match self {
            SeriesKind::Tick => &StoreKindId::TICK_LANES,
            SeriesKind::Quote => &[StoreKindId::Quote],
            SeriesKind::Trade => &[StoreKindId::Trade],
            SeriesKind::Book => &[StoreKindId::Book],
        }
    }
    /// Does this lane filter admit `kind=quote` rows?
    pub fn wants_quotes(self) -> bool {
        self.store_kinds().contains(&StoreKindId::Quote)
    }
    /// Does this lane filter admit `kind=trade` rows?
    pub fn wants_trades(self) -> bool {
        self.store_kinds().contains(&StoreKindId::Trade)
    }
    /// Does this lane filter admit `kind=book` rows?
    pub fn wants_books(self) -> bool {
        self.store_kinds().contains(&StoreKindId::Book)
    }
}

/// One `(venue, symbol)` series to replay, plus its lane filter — the CROSS-VENUE unit
/// ([`TickReplayConfig::series`]). Deserializable so a `ProfileBacktest`'s `[[data.series]]`
/// array maps onto it 1:1 with no parallel type (see `harness::profile::ProfileData`).
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
#[cfg_attr(feature = "profile-schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct ProfileDataSeries {
    pub venue: String,
    pub symbol: String,
    #[serde(default)]
    pub kind: SeriesKind,
}

impl ProfileDataSeries {
    /// A whole-lane series (`SeriesKind::Tick`) — the shape the legacy `venue` × `symbols`
    /// expansion produces.
    pub fn new(venue: impl Into<String>, symbol: impl Into<String>) -> Self {
        ProfileDataSeries { venue: venue.into(), symbol: symbol.into(), kind: SeriesKind::Tick }
    }
}

/// Config for [`replay_ticks`]: which `(venue, symbols, range)` to load
/// from the `HistoryStore`, whether (R6) to seed closed-bar context, and the engine parameters the
/// caller controls.
pub struct TickReplayConfig {
    /// The run's DEFAULT venue: the `venue` half of the `venue` × `symbols` expansion, and — in
    /// every mode, including cross-venue — the `StrategyEngineConfig::default_venue` tag the loader
    /// forces (see [`TickReplayConfig::params`]).
    pub venue: String,
    /// Symbols to replay, all under [`Self::venue`] — IGNORED when [`Self::series`] is `Some`
    /// (the cross-venue form supersedes this expansion). Multiple symbols are always fine when `seed_bar_interval_ms` is
    /// `None`. With seeding on, `consolidate_quotes`/`consolidate_trades` bucket each symbol's
    /// ticks independently, so two symbols generally end up with bar series of DIFFERENT
    /// lengths — `StrategyEngine::new` asserts all symbol series share one length, so
    /// `replay_ticks` checks this itself and returns `TickReplayError::MisalignedSeededBars` instead
    /// of driving that assert into a panic. Phase-1 limitation: bar-seeding is supported for
    /// single-symbol replay only (or the coincidental case where every symbol's consolidated bar
    /// count happens to match).
    pub symbols: Vec<String>,
    pub range: TimestampRange,
    /// `Some(step_ms)` seeds closed bars via `consolidate_quotes`/`consolidate_trades` on the
    /// scanned ticks (R6); `None` leaves an empty bar context (no `on_bar` firing — `run_ticks`
    /// never calls it regardless). See the `symbols` doc above: with multiple symbols this is
    /// currently single-symbol-safe only — `replay_ticks` returns
    /// `TickReplayError::MisalignedSeededBars` rather than panicking when consolidation produces
    /// mismatched per-symbol bar counts.
    ///
    /// **Must be `> 0` when `Some`.** `consolidate_quotes`/`consolidate_trades` compute each
    /// bucket start via `ts.rem_euclid(step_ms)`, which panics on `step_ms == 0` and is
    /// nonsensical for negative steps; `replay_ticks_core` validates this up front and returns
    /// `TickReplayError::InvalidBarInterval` instead of letting that panic surface.
    pub seed_bar_interval_ms: Option<i64>,
    /// Caller sets cash/fees/sizer/etc.; the loader OVERWRITES `fill_model` (→
    /// `FillModelKind::Tick`) and `default_venue` (→ `Some(config.venue)`) unconditionally, since a
    /// tick replay only makes sense under the L1 spread-crossing fill tier tagged to one venue.
    /// Note: `default_venue` only tags the *seeded bars'* instrument id (via `StrategyEngine::
    /// new`'s `format_instrument`) — `run_ticks` itself routes each tick by its own bare
    /// `tick.symbol`, never by the venue-tagged bar symbol.
    pub params: StrategyEngineConfig,
    /// Opt-in (default `false`): snap replayed fills to the point-in-time instrument grid recorded
    /// in the store. When `true`, the loader sets `params.properties` to a closure over
    /// `HistoryStore::properties_as_of(venue, symbol, tick.ts)`, so a fill rounds price→tick / size→step
    /// and gates opening fills below min_qty/min_notional against the grid in effect at that ts
    /// (see `StrategyEngineConfig.properties` / PR-2a). `false` = raw replay, byte-identical to before this
    /// field existed. Any `params.properties` the caller already set is OVERWRITTEN when this is `true`.
    pub snap_to_properties: bool,
    /// Opt-in (default `false`): deliver each symbol's ticks to the STRATEGY in recorded ARRIVAL
    /// order (`local_timestamp`, clamped / fallen back per [`tick_arrival_timestamp`]) instead of venue order, so
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
    /// entirely: each [`ProfileDataSeries`] names its OWN venue and its own lane filter, so one replay
    /// can carry e.g. Polymarket outcome-token trades together with a BTC spot quote series.
    ///
    /// The ENGINE was never the constraint here — `StrategyEngine::run_ticks` routes each tick
    /// by its own payload `symbol` — so this is purely a loader-side widening. Two consequences
    /// the caller owns:
    /// - **Symbols must stay unique across the list.** `run_ticks` resolves a tick to a symbol
    ///   slot by name, so two series sharing a symbol on different venues would collide.
    ///   ([`crate::harness::ProfileBacktest::validate`] rejects that at profile-load time.)
    /// - **`snap_to_properties` remains single-venue.** The PIT grid is looked up under
    ///   `StrategyEngineConfig::default_venue`, which is one string per run — so the two are rejected
    ///   in combination by the profile validator rather than silently reading the wrong venue's
    ///   grid.
    pub series: Option<Vec<ProfileDataSeries>>,
}

impl TickReplayConfig {
    /// The series this config actually loads: [`Self::series`] verbatim when set, else the
    /// frozen `venue` × [`Self::symbols`] whole-lane expansion.
    pub fn resolved_series(&self) -> Vec<ProfileDataSeries> {
        match &self.series {
            Some(series) => series.clone(),
            None => self.symbols.iter().map(|s| ProfileDataSeries::new(&self.venue, s)).collect(),
        }
    }
}

/// Errors from [`replay_ticks`] — wraps a `HistoryStore` scan failure.
#[derive(Debug)]
pub enum TickReplayError {
    Data(HistoryStoreError),
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
    /// A symbol of the replay cannot be booked in the replay's ledger currency
    /// (`StrategyEngineConfig::account_currency`, the dollar when unset): its name states another quote, or none
    /// on a ledger that is not the dollar (`crate::harness::refuse_foreign_quoted_symbols`, which
    /// carries the sentence and the reason). Carries that sentence.
    ForeignQuote(String),
    /// A symbol of the replay is a coin-margined contract and the replay's params carry no contract
    /// value for it, so the simulator would book it as linear dollars
    /// (`crate::harness::refuse_inverse_contracts`, which carries the sentence and the reason).
    /// Carries that sentence.
    InverseContract(String),
}

impl fmt::Display for TickReplayError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TickReplayError::Data(e) => write!(f, "tick replay: {e}"),
            TickReplayError::MisalignedSeededBars { lengths } => write!(
                f,
                "tick replay: seed_bar_interval_ms produced mismatched per-symbol bar counts \
                 {lengths:?} — multi-symbol bar-seeded replay requires every symbol's \
                 consolidated bar series to share one length, which consolidate_quotes/\
                 consolidate_trades does not guarantee across symbols of differing tick \
                 density; this is a Phase-1 limitation (single-symbol seeding is supported, \
                 multi-symbol seeding awaits StrategyEngine's per-symbol bar-length relaxation)"
            ),
            TickReplayError::InvalidBarInterval(step) => write!(
                f,
                "tick replay: seed_bar_interval_ms must be > 0 when Some, got {step} — \
                 consolidate_quotes/consolidate_trades bucket ticks via \
                 ts.rem_euclid(step_ms), which panics on 0 and is nonsensical for negative steps"
            ),
            TickReplayError::ForeignQuote(why) | TickReplayError::InverseContract(why) => {
                write!(f, "tick replay: {why}")
            }
        }
    }
}

impl std::error::Error for TickReplayError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            TickReplayError::Data(e) => Some(e),
            TickReplayError::MisalignedSeededBars { .. } => None,
            TickReplayError::InvalidBarInterval(_)
            | TickReplayError::ForeignQuote(_)
            | TickReplayError::InverseContract(_) => None,
        }
    }
}

impl From<HistoryStoreError> for TickReplayError {
    fn from(e: HistoryStoreError) -> Self {
        TickReplayError::Data(e)
    }
}

/// Per-symbol `(symbol, quotes, trades, book events)` rows, already scanned from the store — the
/// shape [`replay_ticks`] hands to [`replay_ticks_core`]. A named alias, not an inline tuple
/// type, to keep clippy's `type_complexity` lint quiet across the call sites that spell it.
type TickReplayRows = Vec<(String, Vec<TickQuote>, Vec<TickTrade>, Vec<L2BookUpdate>)>;

/// Everything [`replay_ticks`] does after the scan: takes each symbol's already-obtained
/// `(TickQuote, TickTrade, L2BookUpdate)` rows (ts-ascending within each `Vec`, per the `HistoryStore`
/// contract) and does the sparse-symbol skip, [`merge_ticks`], the R6 in-process bar seed, the
/// `MisalignedSeededBars` guard, forcing `fill_model`/`default_venue`, and the
/// `StrategyEngine::new` + `run_ticks` call.
fn replay_ticks_core<S: Strategy<OrderBrokerSimulator>>(
    rows_by_symbol: TickReplayRows,
    strategy: S,
    mut config: TickReplayConfig,
) -> Result<BacktestResult, TickReplayError> {
    // Validate up front: `consolidate_quotes`/`consolidate_trades` compute each bucket start via
    // `ts.rem_euclid(step_ms)`, which panics on `step_ms == 0` and is nonsensical for negative
    // steps. Catch it here rather than letting that panic surface from inside the per-symbol loop
    // below.
    if let Some(step) = config.seed_bar_interval_ms
        && step <= 0
    {
        return Err(TickReplayError::InvalidBarInterval(step));
    }
    // The simulator books one ledger currency: a symbol the name says is not quoted in the ledger's
    // (`params.account_currency`, the dollar when unset) is refused here, as the harness refuses it before it
    // gets this far, unless a conversion pair of the params books it (decision 0243).
    // shortcut: the NAME only (a replay is handed no kept row); a profile run's guard reads the kept row first
    // (`crate::harness::run::refuse_off_ledger_series`), so a symbol whose row fits the ledger and whose name
    // says another fiat is refused here in today's words. Hand the replay the run's quotes if a list holds one.
    let booked = &config.params.conversions;
    crate::harness::refuse_foreign_quoted_symbols(
        rows_by_symbol.iter().map(|r| r.0.as_str()).filter(|s| !crate::data_plan::books(booked, s)),
        config.params.account_currency.as_deref(),
    )
    .map_err(|e| TickReplayError::ForeignQuote(e.message().to_string()))?;
    // A coin-margined contract is booked only by the value its caller handed the engine (a profile run's
    // coin ledger, `crate::harness::run::run_ledger_coin`); one without it would book as linear dollars.
    let series = config.resolved_series();
    let valued = |symbol: &str| config.params.contract_values.iter().any(|(s, _)| s == symbol);
    crate::harness::refuse_inverse_contracts(
        series.iter().filter(|s| !valued(&s.symbol)).map(|s| (s.venue.as_str(), s.symbol.as_str())),
    )
    .map_err(|e| TickReplayError::InverseContract(e.message().to_string()))?;

    let mut ticks_by_symbol: Vec<(String, Vec<TickEvent>)> = Vec::new();
    let mut bars_by_symbol: Vec<(String, Vec<Bar>)> = Vec::new();
    let mut data_warnings: Vec<String> = Vec::new();

    for (symbol, quotes, trades, books) in rows_by_symbol {
        if quotes.is_empty() && trades.is_empty() && books.is_empty() {
            tracing::warn!(
                venue = %config.venue,
                symbol = %symbol,
                "replay_ticks: no quotes, trades, or book events in range — skipping sparse symbol"
            );
            data_warnings.push(format!(
                "{symbol}: no quotes, trades or book events in this window, so it did not run and \
                 this report does not include it"
            ));
            continue;
        }
        // Bar-seeding is unchanged by book support: book events never seed bars (quotes-else-
        // trades, per R6).
        let bars = match config.seed_bar_interval_ms {
            Some(step) if !quotes.is_empty() => consolidate_quotes(&quotes, step),
            Some(step) => consolidate_trades(&trades, step),
            None => Vec::new(),
        };
        // Feed-latency mode (opt-in) re-orders the SAME merged ticks by their recorded arrival
        // clock; `false` takes the frozen venue-ordered merge, byte-identical to before the flag
        // existed. Neither branch rewrites a tick's own `ts` — matching stays on venue time.
        let ticks = if config.feed_latency {
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
    if config.seed_bar_interval_ms.is_some() {
        let lengths: std::collections::BTreeSet<usize> =
            bars_by_symbol.iter().map(|(_, bars)| bars.len()).collect();
        if lengths.len() > 1 {
            return Err(TickReplayError::MisalignedSeededBars {
                lengths: bars_by_symbol.iter().map(|(_, bars)| bars.len()).collect(),
            });
        }
    }

    // R: tick replay is the L1 spread-crossing tier by DEFAULT, tagged to exactly one venue. Respect
    // an explicit `L2Book` (depth-capped) choice the caller set — that tier degrades to `TickEvent`
    // wherever no book exists, so it is safe on any tick tape — but never leave a `Bar` model on the
    // tick lane. `default_venue` is always overwritten (tick replay is single-venue).
    if config.params.fill_model != FillModelKind::L2Book {
        config.params.fill_model = FillModelKind::Tick;
    }
    config.params.default_venue = Some(config.venue.clone());

    let mut engine = StrategyEngine::new(bars_by_symbol, strategy, config.params);
    let mut result = engine.run_ticks(&ticks_by_symbol);
    result.data_warnings = data_warnings;
    Ok(result)
}

/// [`properties_source`]'s kept answer for one symbol, and the span of lookups it is the answer to.
#[derive(Clone, Copy)]
struct TickReplayStandingGrid {
    from: i64,
    to: i64,
    answer: Option<SymbolProperties>,
}

impl TickReplayStandingGrid {
    fn covers(&self, ts: i64) -> bool {
        (self.from..=self.to).contains(&ts)
    }
}

/// The `StrategyEngineConfig.properties` closure over a store's recorded PIT properties: `properties_as_of(venue,
/// symbol, ts)`. Public so a caller can build a filter source over any store handle.
///
/// ⚠ **A failed lookup is kept in `first_error`, and the caller reads it after the run and refuses
/// the result.** The engine's seam answers an `Option` and cannot stop, so the failure still reads as
/// "no record" for the rest of the run: that fill skipped grid rounding and `min_qty`/`min_notional`,
/// and a failure at run start zeroes the venue hold for the whole run (`OrderBrokerSimulator::taker_hold_table_ns`).
/// Reported as a clean result, those fills are a number nobody can tell was never checked. `Ok(None)` is
/// the true "no record" and leaves `first_error` empty.
///
/// ⚠ **Once `first_error` is set the store is not asked again.** The run is already refused, and a dead
/// datahub would otherwise be dialled again for every later lookup of a run that only has to end.
///
/// **A lookup is one round trip to the datahub, and a run makes several per fill, so an answer is kept
/// while it provably stands.** The trait answers a grid and not when it was seen, so that is proved by
/// asking again at the end of the lookup's UTC day: the tape holds at most one row per symbol per UTC day
/// (`vike_data::PropertiesRecorder::record`'s commit key), so the same answer at both ends is the answer
/// everywhere between. Where the two differ the day's row is still ahead, and nothing is kept: that day
/// asks per lookup. A failed second ask keeps nothing either and is no finding, because no fill read it.
#[expect(clippy::type_complexity)] // mirrors the `StrategyEngineConfig.properties` field type
pub fn properties_source(
    store: Arc<dyn HistoryStore + Send + Sync>,
    first_error: Arc<OnceLock<String>>,
) -> Arc<dyn Fn(&str, &str, i64) -> Option<SymbolProperties> + Send + Sync> {
    use vike_model::time::MS_PER_DAY;
    let standing: Mutex<HashMap<(String, String), TickReplayStandingGrid>> = Mutex::default();
    Arc::new(move |venue: &str, symbol: &str, ts: i64| {
        if first_error.get().is_some() {
            return None;
        }
        let key = (venue.to_string(), symbol.to_string());
        let kept = standing.lock().unwrap_or_else(|e| e.into_inner()).get(&key).copied();
        if let Some(grid) = kept.filter(|g| g.covers(ts)) {
            return grid.answer;
        }
        match store.properties_as_of(venue, symbol, ts) {
            Ok(answer) => {
                let day_end =
                    ts.div_euclid(MS_PER_DAY).saturating_add(1).saturating_mul(MS_PER_DAY) - 1;
                let at_end = store.properties_as_of(venue, symbol, day_end);
                if at_end.is_ok_and(|at_end| at_end == answer) {
                    let grid = TickReplayStandingGrid { from: ts, to: day_end, answer };
                    standing.lock().unwrap_or_else(|e| e.into_inner()).insert(key, grid);
                }
                answer
            }
            Err(e) => {
                tracing::warn!(venue, symbol, ts, error = %e, "properties_as_of failed in replay (the run will be refused when it ends)");
                first_error.get_or_init(|| {
                    format!(
                        "engine.snap_to_properties is on and the instrument properties of {venue} \
                         {symbol} at {ts} could not be read ({e}). That lookup is a fill's tick, \
                         step and minimum size checks, or at the start of a run the venue hold, so \
                         the run is refused rather than reported without them"
                    )
                });
                None
            }
        }
    })
}

/// Bounded path (R5): scans the whole range into memory. Takes an owned `Arc` store
/// handle so `snap_to_properties` can clone it into the `'static` `StrategyEngineConfig.properties` closure.
/// See the module doc for the per-symbol pipeline (scan → merge → optional bar seed → `run_ticks`).
pub fn replay_ticks<S: Strategy<OrderBrokerSimulator>>(
    store: Arc<dyn HistoryStore + Send + Sync>,
    strategy: S,
    mut config: TickReplayConfig,
) -> Result<BacktestResult, TickReplayError> {
    let series = config.resolved_series();
    let mut rows: TickReplayRows = Vec::with_capacity(series.len());
    for s in &series {
        let quotes = if s.kind.wants_quotes() {
            store.scan_quotes(&s.venue, &s.symbol, config.range)?
        } else {
            Vec::new()
        };
        let trades = if s.kind.wants_trades() {
            store.scan_trades(&s.venue, &s.symbol, config.range)?
        } else {
            Vec::new()
        };
        let books = if s.kind.wants_books() {
            store.scan_book_updates(&s.venue, &s.symbol, config.range)?
        } else {
            Vec::new()
        };
        rows.push((s.symbol.clone(), quotes, trades, books));
    }
    let first_error: Arc<OnceLock<String>> = Arc::default();
    if config.snap_to_properties {
        config.params.properties = Some(properties_source(store, Arc::clone(&first_error)));
    }
    let result = replay_ticks_core(rows, strategy, config)?;
    match first_error.get() {
        Some(why) => Err(TickReplayError::Data(HistoryStoreError::Query(why.clone()))),
        None => Ok(result),
    }
}

#[path = "tick_replay_tests.rs"]
#[cfg(test)]
mod tick_replay_tests;
