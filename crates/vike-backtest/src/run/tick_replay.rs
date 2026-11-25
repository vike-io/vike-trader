//! Tick-replay loader: turns `vike-data` `HistoryStore`-recorded quote/trade/book rows into the
//! existing [`vike_simulator::StrategyEngine::run_ticks`] input, producing a
//! [`vike_analytics::TradingRecord`] — the offline twin of the live tick recorder.
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
//! (R2 — no engine LOGIC changes). A symbol with ZERO quotes, ZERO trades, AND ZERO book events in range
//! is sparse-tolerant: `tracing::warn!` + skip, not an error — an all-sparse window degrades to an
//! empty-but-valid `TradingRecord` (`run_ticks` on `[]`).
//!
//! ⚠ R2 read "no engine changes" until 2026-11-20, when the owner said yes to relaxing it for the INPUT'S TYPE
//! alone: `run_ticks` takes any slice of events (`T: AsRef<[TickEvent]>`), so a tick walk-forward's window is a
//! borrowed range of the one prepared tape (`crate::walkforward::ticks`). Its fold, every fill and every number
//! are unchanged.
//!
//! There is NO streaming twin of this loader. R5 described one as an opt-in for large windows
//! (`replay_ticks_streaming`, over `HistoryStoreDataFusion::scan_quotes_stream`/`scan_trades_stream`) and it
//! was built, then DELETED with its stream scans on 2026-10-01: nothing outside its own test ever
//! called it, it still collected each stream into a `Vec` before merging (so it was never O(batch)
//! end-to-end), and those scans read a symbol's per-symbol directory ALONE, so a symbol held in a
//! `group=` directory replayed as if it had no quotes or trades. A streaming loader built later has
//! to read every layout a symbol can live in, as `HistoryStore::scan_quotes_capped` does.
//! A replay is two halves (`tape`'s module doc): PREPARE (the scans, the heard streams, the merge, the R6 bar seed
//! and the `MisalignedSeededBars` guard, into a tape) and RUN (`StrategyEngine::new` + `run_ticks` over a BORROWED
//! cut of it). A parameter search prepares each tape once and every point borrows it, and a tick walk-forward
//! prepares ONE tape and every window runs a range of it ([`tapes`]'s module doc); a single run prepares its own.
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
//!
//! # Heard streams ([`TickReplayConfig::reference`], [`TickReplayConfig::mark`])
//!
//! Another venue's event reaches this box after a hop its venue stamp does not show. A recorded event
//! carries the receive time of the box that recorded it (`local_timestamp`), and a heard stream is
//! delivered in that order and AT that time ([`tick_arrival_timestamp`]): the hop that box measured, so
//! the stream needs no key. A stream holding any event with no receive time (archive or REST data,
//! `local_timestamp` 0) must state `delay_ms`, or the replay is refused in words
//! ([`TickReplayError::HeardDelay`]); a stated `delay_ms` is added to every event's arrival clock (its
//! venue stamp where nothing was recorded), stamped events included. The delay moves when an event is
//! heard, in the merge and on the strategy's clock (`vike_simulator::TickEvent::heard_at`), and the
//! event keeps its venue stamp. A traded leg fills on its own events at its venue's stamps and is heard
//! the same way.

use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, Mutex, OnceLock};

use vike_data::{HistoryStore, HistoryStoreError, PlannedRead, TimestampRange};
use vike_marketdata::{
    L2Book, L2BookUpdate, L2BookUpdateKind, TickQuote, TickTrade, apply_book_event,
};
use vike_model::StoreKindId;
use vike_model::{FeedStatus, Strategy, SymbolProperties};

use vike_analytics::TradingRecord;
use vike_simulator::{OrderBrokerSimulator, StrategyEngineConfig, TickEvent, book_touch};

use crate::run::KeptRows;

// A replay's two halves, prepare and run.
mod tape;
// A request's prepared tapes, shared by the runs of a search or a walk.
pub(crate) mod tapes;

use tape::{prepare_tape, refuse_unrunnable, run_tape};
use tapes::{TapeCut, TickTapes};

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
            Some((_, 0)) => out.extend(bi.next().map(|b| TickEvent::Book(Box::new(b)))),
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
    tick.timestamp()
}

/// A tick's recorded MACHINE receive stamp (`0` = never stamped: fixtures, backfill, parquet parts
/// written before the `local_timestamp` column existed, and a mark, flow or feed-status event, which
/// carries none).
pub fn tick_local_timestamp(tick: &TickEvent) -> i64 {
    match tick {
        TickEvent::Quote(q) => q.local_timestamp,
        TickEvent::Reference { quote, .. } => quote.local_timestamp,
        TickEvent::Trade(t) => t.local_timestamp,
        TickEvent::Book(b) => b.local_timestamp,
        TickEvent::Mark { .. } | TickEvent::Flow(_) | TickEvent::FeedStatus { .. } => 0,
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
    arrival_timestamp(tick_venue_timestamp(tick), tick_local_timestamp(tick))
}

/// [`tick_arrival_timestamp`]'s rule on the two stamps of one event: its `venue` stamp and the `local`
/// receive stamp of the box that recorded it.
fn arrival_timestamp(venue: i64, local: i64) -> i64 {
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
/// array maps onto it 1:1 with no parallel type (see `run::profile::ProfileData`).
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
#[cfg_attr(feature = "profile-schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct ProfileDataSeries {
    pub venue: String,
    pub symbol: String,
    #[serde(default)]
    pub kind: SeriesKind,
    /// The ACCOUNT a strategy's leg on this series trades, `venue/LABEL` or the bare venue, for a run that holds
    /// one user name on two venues (binance and bybit `BTCUSDT.P`): the series is then the leg the strategy names
    /// `SYMBOL@venue/LABEL` (decision 0293), in its orders, reads and fills. Absent: the series is its symbol.
    #[serde(default)]
    pub account: Option<String>,
}

impl ProfileDataSeries {
    /// A whole-lane series (`SeriesKind::Tick`) — the shape the legacy `venue` × `symbols`
    /// expansion produces.
    pub fn new(venue: impl Into<String>, symbol: impl Into<String>) -> Self {
        ProfileDataSeries {
            venue: venue.into(),
            symbol: symbol.into(),
            kind: SeriesKind::Tick,
            account: None,
        }
    }

    /// The name the run's strategy trades this series under, its SLOT in the simulator (decision 0293):
    /// `SYMBOL@account` for a series that names its account, else its symbol. The store is read by the symbol.
    pub fn leg_name(&self) -> String {
        match &self.account {
            Some(account) => format!("{}@{account}", self.symbol),
            None => self.symbol.clone(),
        }
    }
}

/// `values` (each symbol's `vike_model::ContractValue`, by the store's symbol) with the same value under the
/// SLOT of each series of `series` that names its account (decision 0293), which the simulator books by. A run
/// whose series name no account is unchanged.
pub(crate) fn with_leg_contract_values(
    mut values: Vec<(String, vike_model::ContractValue)>,
    series: &[ProfileDataSeries],
) -> Vec<(String, vike_model::ContractValue)> {
    for s in series.iter().filter(|s| s.account.is_some()) {
        if let Some(value) = values.iter().find(|(symbol, _)| *symbol == s.symbol).map(|(_, v)| *v)
        {
            values.push((s.leg_name(), value));
        }
    }
    values
}

/// One series of ANOTHER venue whose touch the strategy hears on `Strategy::on_reference_quote`
/// (`[[data.reference]]`, [`TickReplayConfig::reference`]): live, a leg the mount declared on a venue
/// other than its own (`vike_core::MountLeg::at`). Its quotes are heard as they are, and its book as
/// its top (`vike_simulator::book_touch`), never as `on_quote_tick` or `on_order_book`, each one when
/// the recording box received it, plus [`Self::delay_ms`] (the module doc's "Heard streams").
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
#[cfg_attr(feature = "profile-schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct ProfileDataReference {
    /// The venue the touch comes from; never the run's own.
    pub venue: String,
    /// The symbol as that venue's series is stored. It may share its name with a series of the run.
    pub symbol: String,
    /// Which lanes are heard: `quote`, `book` or `tick` (both); a reference is a touch, so never `trade`.
    #[serde(default)]
    pub kind: SeriesKind,
    /// The run also TRADES it: an order naming its symbol fills on its own events, as a hedge leg on
    /// that venue does live, and its prints fill but reach no hook. Its symbol is then one the slice
    /// holds once (the order seam names a symbol, not a venue). Off: heard only.
    #[serde(default)]
    pub trade: bool,
    /// Milliseconds added to when each touch is heard: its recorded receive time, else its venue's
    /// stamp. The touch keeps its stamp, and a traded leg still fills on it. Absent: no addition, which
    /// the replay refuses for a stream holding an event with no receive time (archive or REST data).
    #[serde(default)]
    pub delay_ms: Option<u64>,
}

/// One series whose price the strategy hears on `Strategy::on_mark` as an underlying's mark
/// (`[[data.mark]]`, [`TickReplayConfig::mark`]; live: the mount's `underlying_symbol`): a quote's mid, a
/// print's price, a book's mid, each one when the recording box received it, plus [`Self::delay_ms`].
/// Never traded.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
#[cfg_attr(feature = "profile-schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct ProfileDataMark {
    /// The venue the mark comes from.
    pub venue: String,
    /// The symbol as that venue's series is stored.
    pub symbol: String,
    /// Which lanes are heard: `quote`, `trade`, `book` or `tick` (all three).
    #[serde(default)]
    pub kind: SeriesKind,
    /// Milliseconds added to when each mark is heard: its source event's recorded receive time, else
    /// its venue's stamp. The mark keeps its stamp. Absent: no addition, which the replay refuses for a
    /// stream holding an event with no receive time (archive or REST data).
    #[serde(default)]
    pub delay_ms: Option<u64>,
}

/// The references a run also trades, as the series they are: each one a symbol slot of the run.
pub fn traded_legs(
    reference: &[ProfileDataReference],
) -> impl Iterator<Item = ProfileDataSeries> + '_ {
    reference.iter().filter(|r| r.trade).map(|r| ProfileDataSeries {
        venue: r.venue.clone(),
        symbol: r.symbol.clone(),
        kind: r.kind,
        account: None,
    })
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
    /// nonsensical for negative steps; `refuse_unrunnable` validates this up front and returns
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
    ///   ([`crate::run::profile::ProfileBacktest::validate`] rejects that at profile-load time.)
    /// - **`snap_to_properties` remains single-venue.** The PIT grid is looked up under
    ///   `StrategyEngineConfig::default_venue`, which is one string per run — so the two are rejected
    ///   in combination by the profile validator rather than silently reading the wrong venue's
    ///   grid.
    pub series: Option<Vec<ProfileDataSeries>>,
    /// Opt-in: the other venues' series the strategy hears on `Strategy::on_reference_quote`
    /// ([`ProfileDataReference`]), each event when the recording box received it plus the stream's
    /// [`ProfileDataReference::delay_ms`] (the module doc's "Heard streams"; the event keeps the venue's
    /// stamp). One it also trades is a symbol slot of the run (in [`Self::resolved_series`],
    /// `vike_simulator::StrategyEngineConfig::reference_legs`) that fills on its own events at the venue's
    /// stamps and is heard the same way. Empty (the default): nothing is heard this way, byte-identical.
    pub reference: Vec<ProfileDataReference>,
    /// Opt-in: the series the strategy hears on `Strategy::on_mark` as an underlying's mark
    /// ([`ProfileDataMark`]), each when the recording box received it plus [`ProfileDataMark::delay_ms`].
    /// Heard only, never traded. Empty (the default): byte-identical.
    pub mark: Vec<ProfileDataMark>,
    /// Opt-in (default `false`): the mounted series' recorded stream-health markers (its book lane's
    /// `GapStart`, `Stale`, `LiveResume`, what the recorder writes for each live `StreamStatus`) are
    /// heard on `Strategy::on_feed_status` as `Disconnected`, `Stale`, `Live`, right after the marker.
    /// The book lane is the only stream-health record a store keeps, so a series without one hears none.
    pub feed_status: bool,
}

impl TickReplayConfig {
    /// The series this config actually loads as the run's symbol slots: [`Self::series`] verbatim
    /// when set, else the frozen `venue` × [`Self::symbols`] whole-lane expansion, then each
    /// reference the run also trades ([`traded_legs`]).
    pub fn resolved_series(&self) -> Vec<ProfileDataSeries> {
        let mut out = match &self.series {
            Some(series) => series.clone(),
            None => self.symbols.iter().map(|s| ProfileDataSeries::new(&self.venue, s)).collect(),
        };
        out.extend(traded_legs(&self.reference));
        out
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
    /// steps; `refuse_unrunnable` checks this up front and returns this error instead of letting
    /// that panic surface. Carries the offending value.
    InvalidBarInterval(i64),
    /// A symbol of the replay cannot be booked in the replay's ledger currency
    /// (`StrategyEngineConfig::account_currency`, the dollar when unset): its name states another quote, or none
    /// on a ledger that is not the dollar (`crate::run::refuse_foreign_quoted_symbols`, which
    /// carries the sentence and the reason). Carries that sentence.
    ForeignQuote(String),
    /// A symbol of the replay is a coin-margined contract and the replay's params carry no contract
    /// value for it, so the simulator would book it as linear dollars
    /// (`crate::run::refuse_inverse_contracts`, which carries the sentence and the reason).
    /// Carries that sentence.
    InverseContract(String),
    /// A heard stream ([`TickReplayConfig::reference`], [`TickReplayConfig::mark`]) holds an event with
    /// no recorded receive time and states no `delay_ms` (`heard_delay`), so when this box would have
    /// heard it is unknown. Carries that sentence.
    HeardDelay(String),
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
            TickReplayError::ForeignQuote(why)
            | TickReplayError::InverseContract(why)
            | TickReplayError::HeardDelay(why) => write!(f, "tick replay: {why}"),
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
            | TickReplayError::InverseContract(_)
            | TickReplayError::HeardDelay(_) => None,
        }
    }
}

impl From<HistoryStoreError> for TickReplayError {
    fn from(e: HistoryStoreError) -> Self {
        TickReplayError::Data(e)
    }
}

/// The recorded stream-health marker's `FeedStatus`: the inverse of the recorder's map of each live
/// `StreamStatus` to a book-lane marker (`vike_model::StreamStatus::feed_status`'s three words).
fn marker_feed_status(kind: L2BookUpdateKind) -> Option<FeedStatus> {
    match kind {
        L2BookUpdateKind::GapStart => Some(FeedStatus::Disconnected),
        L2BookUpdateKind::Stale => Some(FeedStatus::Stale),
        L2BookUpdateKind::LiveResume => Some(FeedStatus::Live),
        L2BookUpdateKind::Snapshot | L2BookUpdateKind::Delta => None,
    }
}

/// `ticks` with a `TickEvent::FeedStatus` right after each stream-health marker of its book lane.
fn with_feed_status(ticks: Vec<TickEvent>) -> Vec<TickEvent> {
    // Sized once, the markers' statuses counted in: one more event a marker never doubles the tape.
    let markers = ticks
        .iter()
        .filter(|t| matches!(t, TickEvent::Book(b) if marker_feed_status(b.kind).is_some()))
        .count();
    let mut out = Vec::with_capacity(ticks.len() + markers);
    for tick in ticks {
        let status = match &tick {
            TickEvent::Book(b) => marker_feed_status(b.kind).map(|s| (s, b.timestamp)),
            _ => None,
        };
        out.push(tick);
        if let Some((status, timestamp)) = status {
            out.push(TickEvent::FeedStatus { status, timestamp });
        }
    }
    out
}

/// A heard series' merged tape (`ticks`, in delivery order) as the events the strategy hears: each quote
/// through `touch`, each applied book's top (`vike_simulator::book_touch`, on the series' own book) through
/// `touch`, each print through `print`. A book event that leaves no two-sided book is heard as nothing.
fn heard_events(
    ticks: Vec<TickEvent>,
    symbol: &str,
    touch: impl Fn(TickQuote) -> TickEvent,
    print: impl Fn(TickTrade) -> TickEvent,
) -> Vec<TickEvent> {
    let mut book: Option<L2Book> = None;
    let mut out = Vec::with_capacity(ticks.len());
    for tick in ticks {
        match tick {
            TickEvent::Quote(q) => out.push(touch(q)),
            TickEvent::Trade(t) => out.push(print(t)),
            TickEvent::Book(b) => {
                if apply_book_event(&mut book, &b, b.tick_size, |x| x)
                    && let Some(q) = book
                        .as_ref()
                        .and_then(|book| book_touch(book, symbol, b.timestamp, b.local_timestamp))
                {
                    out.push(touch(q));
                }
            }
            heard => out.push(heard),
        }
    }
    out
}

/// The milliseconds a heard `stream`'s events are heard after their arrival clock
/// ([`arrival_timestamp`]): its stated `delay_ms`, else none when every one of its `ticks` carries the
/// receive time of the box that recorded it. A stream with any event that carries none and no
/// `delay_ms` is refused: when this box would have heard it is unknown, and its venue's stamp is sooner
/// than any box could.
fn heard_delay(
    stream: &str,
    delay_ms: Option<u64>,
    ticks: &[TickEvent],
) -> Result<u64, TickReplayError> {
    let unstamped = ticks.iter().filter(|t| tick_local_timestamp(t) <= 0).count();
    match delay_ms {
        Some(delay) => Ok(delay),
        None if unstamped == 0 => Ok(0),
        None => Err(TickReplayError::HeardDelay(format!(
            "{stream}: {unstamped} of its {} events carry no local receive time (archive or REST \
             data), so when this box would have heard them is unknown and their venue's stamp is \
             sooner than any box could: state its delay_ms, the milliseconds an event takes from that \
             venue to this box. Nothing ran",
            ticks.len()
        ))),
    }
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
///
/// A replay no profile resolved asks `store` for the KEPT instrument rows of its series' venues itself, as a
/// profile's door does (`KeptRows::answered` over `vike_data::HistoryStore::kept_instruments_and_splits_of`), and
/// reads each symbol's fiat quote and margining off them, the name only where no row covers it (the run's data
/// warnings name each such series). A store that keeps no lists (a test's) answers as a profile run over it does; one
/// that FAILS to serve them refuses the replay in words, never reads the names in their place.
pub fn replay_ticks<S: Strategy<OrderBrokerSimulator>>(
    store: Arc<dyn HistoryStore + Send + Sync>,
    strategy: S,
    config: TickReplayConfig,
) -> Result<TradingRecord, TickReplayError> {
    let series: Vec<(String, String)> =
        config.resolved_series().into_iter().map(|s| (s.venue, s.symbol)).collect();
    // Only the venues it reads: every lookup after is of a series' own venue, and the whole lists are ~24k rows.
    let mut venues: Vec<String> = series.iter().map(|(venue, _)| venue.clone()).collect();
    venues.sort();
    venues.dedup();
    let kept = match store.kept_instruments_and_splits_of(&venues).0 {
        Ok(lists) => KeptRows::answered(Ok(lists), &series),
        Err(e) => {
            return Err(TickReplayError::Data(HistoryStoreError::Query(format!(
                "the tick replay is refused: the store's kept instrument lists could not be read ({e}), \
                 and a replay reads each symbol's fiat quote and whether it is coin-margined off its kept \
                 row, never off its name in their place. Nothing ran"
            ))));
        }
    };
    let mut result = replay_ticks_kept(store, strategy, config, &kept, Vec::new(), None)?;
    result.data_warnings.extend(kept.warnings);
    Ok(result)
}

/// Which lanes of one series a replay reads, as `[quotes, trades, books]`: those `kind` admits, its trades left
/// unread when `prints` is false (a reference is heard by its touch alone).
fn lanes_read(kind: SeriesKind, prints: bool) -> [bool; 3] {
    [kind.wants_quotes(), prints && kind.wants_trades(), kind.wants_books()]
}

/// The lane reads [`replay_ticks_kept`] makes of `config`, in its order: each series' (a series one of `derived`
/// names takes those events and reads nothing), each reference's it does not hear off a traded slot, each mark's.
fn planned_lane_reads(
    config: &TickReplayConfig,
    series: &[ProfileDataSeries],
    derived: &[(String, Vec<TickQuote>)],
) -> Vec<PlannedRead> {
    let range = config.range;
    let mut reads = Vec::new();
    let mut lanes = |venue: &str, symbol: &str, kind: SeriesKind, prints: bool| {
        let [quotes, trades, books] = lanes_read(kind, prints);
        let (venue, symbol) = (venue.to_string(), symbol.to_string());
        if quotes {
            reads.push(PlannedRead::Quotes { venue: venue.clone(), symbol: symbol.clone(), range });
        }
        if trades {
            reads.push(PlannedRead::Trades { venue: venue.clone(), symbol: symbol.clone(), range });
        }
        if books {
            reads.push(PlannedRead::BookUpdates { venue, symbol, range });
        }
    };
    // The replay's slots, as it fills them: a derived series takes its events (each derived entry once), every other
    // series is scanned under its leg's name, and the derived symbols no series named follow.
    let mut unclaimed: Vec<&str> = derived.iter().map(|(symbol, _)| symbol.as_str()).collect();
    let mut slots: Vec<String> = Vec::with_capacity(series.len() + derived.len());
    for s in series {
        if let Some(at) = unclaimed.iter().position(|symbol| *symbol == s.symbol) {
            slots.push(unclaimed.remove(at).to_string());
            continue;
        }
        lanes(&s.venue, &s.symbol, s.kind, true);
        slots.push(s.leg_name());
    }
    slots.extend(unclaimed.into_iter().map(str::to_string));
    for r in &config.reference {
        if !(r.trade && slots.contains(&r.symbol)) {
            lanes(&r.venue, &r.symbol, r.kind, false);
        }
    }
    for m in &config.mark {
        lanes(&m.venue, &m.symbol, m.kind, true);
    }
    reads
}

/// [`replay_ticks`] over KEPT rows its caller already holds: a profile run's (`ProfileBacktest::kept`, asked once
/// by the door that resolved the profile), so the replay reads every fact the run's other guards read.
///
/// `derived` is quote events the caller made from another stored kind (`[data.options]`: an option leg's chain
/// rows, an underlying's bars), per symbol: a series of the config named there takes those events and is not
/// scanned; every other symbol named there joins the replay after the series, in `derived`'s order. Empty: the
/// replay [`replay_ticks`] runs.
///
/// `tapes` is the REQUEST's prepared tapes (a parameter search's, [`tapes`]' module doc): the tape this config's
/// data builds is borrowed from it when another point prepared it, and prepared into it otherwise. `None` (a
/// single run): the run prepares its own tape and drops it, as it always did.
pub(crate) fn replay_ticks_kept<S: Strategy<OrderBrokerSimulator>>(
    store: Arc<dyn HistoryStore + Send + Sync>,
    strategy: S,
    config: TickReplayConfig,
    kept: &KeptRows,
    derived: Vec<(String, Vec<TickQuote>)>,
    tapes: Option<&TickTapes>,
) -> Result<TradingRecord, TickReplayError> {
    let cut = match tapes {
        None => TapeCut::whole(Arc::new(prepare_tape(&store, &config, Some(kept), derived)?)),
        Some(tapes) => {
            let prepare = |derived| prepare_tape(&store, &config, Some(kept), derived);
            let cut = tapes.tape(&config, derived, prepare)?;
            // The tape may be another run's: this run's own params are judged here.
            refuse_unrunnable(&config, kept)?;
            cut
        }
    };
    run_tape(store, strategy, config, &cut)
}

/// A tick walk-forward's ONE tape: `config`'s data over its whole (bounded) range, prepared with no refusal (each
/// window's own run refuses what its params cannot book), held by a [`TickTapes`] every window borrows a range of.
pub(crate) fn prepare_walk(
    store: &Arc<dyn HistoryStore + Send + Sync>,
    config: &TickReplayConfig,
) -> Result<TickTapes, TickReplayError> {
    Ok(TickTapes::over_walk(config, prepare_tape(store, config, None, Vec::new())?))
}

#[path = "tick_replay_tests.rs"]
#[cfg(test)]
mod tick_replay_tests;
