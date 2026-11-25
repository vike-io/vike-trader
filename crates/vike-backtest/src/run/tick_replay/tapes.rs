//! A REQUEST's prepared tick tapes (the owner's "yes now" of 2026-11-20 to A1): what a tick replay runs, read,
//! merged and bar-seeded ONCE per data input and borrowed by every point of a parameter search. Until then
//! each point decoded, seeded and merged its own copy of the same tape, 41–46% of a point's CPU and one
//! ~404 MB `Vec<TickEvent>` a day per worker.
//!
//! - **The key is the DATA half of a [`TickReplayConfig`] and nothing else** ([`TapeKey`]): two configs with an
//!   equal key build the same tape over one store, whatever their strategy or engine params. The engine
//!   params are read by the RUN half only, and the refusals that read them (`super::tape::refuse_unrunnable`)
//!   are asked again of every run that borrows a tape. ⚠ Anything `super::tape::prepare_tape` reads later joins
//!   the key in the same change: [`TapeKey::over`] names every field of the config, so a new field does not
//!   compile until it is put on one side. The derived events' floats are compared by their BITS, so a NaN is
//!   the same input as itself.
//! - **Single flight.** The first run that needs a key prepares it; the others wait on it and borrow the
//!   same tape. A preparation that fails keeps nothing, and the next waiter prepares again (and fails as
//!   that run alone would have).
//! - **A walk's ONE tape** ([`TickTapes::over_walk`], the owner's yes of 2026-11-20 to the tick walk-forward):
//!   a run whose config is the walk's but for a time range inside it replays a CUT of the walk's tape, each
//!   stream's events in that range (binary-searched on the venue stamp, so events sharing a stamp are never
//!   split) with that range's seed bars. It is the tape a run over that range alone prepares, because a walk
//!   holds one series in venue order and cuts only on its seed step ([`WalkTape::cut`]); any other request
//!   prepares its own, as a search's would.
//! - **One per request, never process-wide.** The search driver holds one
//!   (`crate::search::optimize::SearchEvaluatorStore`) over its one store, and a tick walk-forward one
//!   (`crate::walkforward::ticks`); each dies with its request: a hub runs a `.rhai` sweep in its own process. A
//!   single run holds none and prepares its own tape, as it always did.

use std::ops::Range;
use std::sync::{Arc, Mutex};

use vike_data::TimestampRange;
use vike_marketdata::interval::bar_open;
use vike_marketdata::{Bar, TickQuote, TickTrade, consolidate_trades};
use vike_simulator::TickEvent;

use super::tape::{TickTape, sparse_symbol_warning};
use super::{ProfileDataMark, ProfileDataReference, ProfileDataSeries, TickReplayConfig};

/// Everything a [`TickTape`] is built from, over one store. Compared whole, not hashed: a key holds a few
/// series, and a sweep one key.
#[derive(PartialEq)]
struct TapeKey {
    /// The run's venue (a sparse symbol's warning names it).
    venue: String,
    /// Each slot's `(venue, symbol, kind, account)`: `symbols` × `venue`, or `series`, then the traded
    /// references ([`TickReplayConfig::resolved_series`]).
    series: Vec<ProfileDataSeries>,
    range: TimestampRange,
    seed_bar_interval_ms: Option<i64>,
    feed_latency: bool,
    feed_status: bool,
    /// Each heard reference's venue, symbol, kind, `trade` and `delay_ms`.
    reference: Vec<ProfileDataReference>,
    /// Each heard mark's venue, symbol, kind and `delay_ms`.
    mark: Vec<ProfileDataMark>,
    /// The quote events its caller made from another stored kind (`[data.options]`' legs), event for event.
    derived: DerivedEvents,
}

impl TapeKey {
    fn of(config: &TickReplayConfig, derived: &[(String, Vec<TickQuote>)]) -> TapeKey {
        TapeKey::over(config, derived, config.range)
    }

    /// [`TapeKey::of`] with `range` in the config's place: a walk's key, which each of its windows' configs
    /// matches but for its own range.
    fn over(
        config: &TickReplayConfig,
        derived: &[(String, Vec<TickQuote>)],
        range: TimestampRange,
    ) -> TapeKey {
        // Every field, named: a field added to the config is put in the key or out of it here, or nothing builds.
        let TickReplayConfig {
            venue,
            // both read through `resolved_series`
            symbols: _,
            series: _,
            // the caller's, above
            range: _,
            seed_bar_interval_ms,
            // the RUN half's: the engine, and the instrument grid it snaps fills to
            params: _,
            snap_to_properties: _,
            feed_latency,
            reference,
            mark,
            feed_status,
        } = config;
        TapeKey {
            venue: venue.clone(),
            series: config.resolved_series(),
            range,
            seed_bar_interval_ms: *seed_bar_interval_ms,
            feed_latency: *feed_latency,
            feed_status: *feed_status,
            reference: reference.clone(),
            mark: mark.clone(),
            derived: DerivedEvents(derived.to_vec()),
        }
    }
}

/// [`TapeKey`]'s derived events, compared by their floats' BITS: a NaN is the same input as itself, so the points
/// of a search over a chain holding one still share one tape.
struct DerivedEvents(Vec<(String, Vec<TickQuote>)>);

impl PartialEq for DerivedEvents {
    fn eq(&self, other: &Self) -> bool {
        // Every field, named: a field added to the quote is compared here, or nothing builds.
        let bits = |q: &TickQuote| {
            let TickQuote { timestamp, local_timestamp, bid, ask, bid_size, ask_size, symbol } = q;
            let floats = [bid, ask, bid_size, ask_size].map(|f| f.to_bits());
            (*timestamp, *local_timestamp, floats, *symbol)
        };
        let same = |(a, x): &(String, Vec<TickQuote>), (b, y): &(String, Vec<TickQuote>)| {
            a == b && x.len() == y.len() && x.iter().zip(y).all(|(p, q)| bits(p) == bits(q))
        };
        self.0.len() == other.0.len() && self.0.iter().zip(&other.0).all(|(a, b)| same(a, b))
    }
}

/// One slot a key's tape is prepared into: locked while it is prepared, so the runs that need it wait.
type TapeSlot = Arc<Mutex<Option<Arc<TickTape>>>>;

/// What one run replays: a held tape, whole, or a walk window's cut of it ([`WalkTape::cut`]).
pub(crate) struct TapeCut {
    tape: Arc<TickTape>,
    window: Option<TapeWindow>,
}

/// A walk window's part of the walk's tape: each stream that holds an event in the window, by its index and the
/// range of its events, with the window's seed bars and its warnings (a stream with none is said, not run).
struct TapeWindow {
    streams: Vec<(usize, Range<usize>)>,
    bars: Vec<(String, Vec<Bar>)>,
    data_warnings: Vec<String>,
}

impl TapeCut {
    /// `tape` whole: what a single run, or a search's point, replays.
    pub(super) fn whole(tape: Arc<TickTape>) -> TapeCut {
        TapeCut { tape, window: None }
    }

    /// The streams `run_ticks` replays, each a BORROWED range of the tape.
    pub(super) fn streams(&self) -> Vec<(String, &[TickEvent])> {
        fn part(stream: &(String, Vec<TickEvent>), range: Range<usize>) -> (String, &[TickEvent]) {
            (stream.0.clone(), &stream.1[range])
        }
        match &self.window {
            None => self.tape.ticks.iter().map(|s| part(s, 0..s.1.len())).collect(),
            Some(w) => w
                .streams
                .iter()
                .map(|(i, range)| part(&self.tape.ticks[*i], range.clone()))
                .collect(),
        }
    }

    /// The seed bars the engine starts from (owned: `StrategyEngine::new` takes them).
    pub(super) fn bars(&self) -> Vec<(String, Vec<Bar>)> {
        self.window.as_ref().map_or_else(|| self.tape.bars.clone(), |w| w.bars.clone())
    }

    /// What the data held wrong over what this run replays.
    pub(super) fn data_warnings(&self) -> Vec<String> {
        let whole = &self.tape.data_warnings;
        self.window.as_ref().map_or_else(|| whole.clone(), |w| w.data_warnings.clone())
    }

    /// The held tape this cut is of.
    #[cfg(test)]
    pub(super) fn tape(&self) -> &Arc<TickTape> {
        &self.tape
    }
}

/// A tick walk-forward's ONE tape and what cutting it needs (the module doc's "A walk's ONE tape").
struct WalkTape {
    key: TapeKey,
    tape: Arc<TickTape>,
    /// The walk's range, both ends inclusive, as its config states it.
    start: i64,
    end: i64,
    /// Whether each market stream holds a quote: its whole seed bars are then the quotes', and a window without
    /// one seeds from its trades, as a run over that window alone does.
    quoted: Vec<bool>,
}

impl WalkTape {
    /// The window `[start, end]` (both inclusive) of the walk's tape, or `None` when a cut would not be the tape a
    /// run over that window alone prepares: a bound inside a seed bucket the walk's tape holds whole, whose bar
    /// that run would seed from part of it.
    fn cut(&self, start: i64, end: i64) -> Option<TapeWindow> {
        let step = self.key.seed_bar_interval_ms;
        let on_step = |t: i64| step.is_none_or(|s| t.rem_euclid(s) == 0);
        if !(start <= self.start || on_step(start)) || !(end >= self.end || on_step(end + 1)) {
            return None;
        }
        // Each stream a series with its own bars and the whole range warning nothing: else a run over the window
        // prepares its own (the walk's pre-flight makes this the case).
        let tape = &self.tape;
        if !tape.data_warnings.is_empty() || tape.bars.len() != tape.ticks.len() {
            return None;
        }
        let mut window =
            TapeWindow { streams: Vec::new(), bars: Vec::new(), data_warnings: Vec::new() };
        for (i, (symbol, events)) in self.tape.ticks.iter().enumerate() {
            let at = |t: i64| events.partition_point(|e| e.timestamp() < t);
            let range = at(start)..events.partition_point(|e| e.timestamp() <= end);
            if range.is_empty() {
                window.data_warnings.push(sparse_symbol_warning(symbol));
                continue;
            }
            let bars = match step {
                None => Vec::new(),
                Some(step) => {
                    let part = &events[range.clone()];
                    if self.quoted[i] && !part.iter().any(|e| matches!(e, TickEvent::Quote(_))) {
                        // Rare: the window holds no quote of a series that has some, so its bars are its trades'.
                        let trades: Vec<TickTrade> = part
                            .iter()
                            .filter_map(|e| match e {
                                TickEvent::Trade(t) => Some(t.clone()),
                                _ => None,
                            })
                            .collect();
                        consolidate_trades(&trades, step)
                    } else {
                        let whole = &self.tape.bars[i].1;
                        let first = whole.partition_point(|b| b.timestamp < bar_open(start, step));
                        whole[first..whole.partition_point(|b| b.timestamp <= end)].to_vec()
                    }
                }
            };
            window.streams.push((i, range));
            window.bars.push((symbol.clone(), bars));
        }
        Some(window)
    }
}

/// A request's tapes by [`TapeKey`], and a walk's one tape (the module doc).
#[derive(Default)]
pub(crate) struct TickTapes {
    tapes: Mutex<Vec<(TapeKey, TapeSlot)>>,
    walk: Option<WalkTape>,
}

impl TickTapes {
    /// The tapes of a tick walk-forward over `config`'s range: `tape` is that whole range's
    /// (`super::prepare_walk`), every window of it a cut. The walk holds ONE market series in venue order (its
    /// pre-flight refuses heard streams, option legs and arrival order); `config.range` is bounded.
    pub(super) fn over_walk(config: &TickReplayConfig, tape: TickTape) -> TickTapes {
        let quoted = tape
            .ticks
            .iter()
            .map(|(_, events)| events.iter().any(|e| matches!(e, TickEvent::Quote(_))))
            .collect();
        let walk = WalkTape {
            key: TapeKey::of(config, &[]),
            start: config.range.start.unwrap_or(i64::MIN),
            end: config.range.end.unwrap_or(i64::MAX),
            tape: Arc::new(tape),
            quoted,
        };
        TickTapes { tapes: Mutex::default(), walk: Some(walk) }
    }

    /// How many PRICED events (a quote or a trade: what the engine's warm-up counts) the walk's tape holds in
    /// `[start, end]`, both inclusive: what its driver judges each window by. `0` outside a walk.
    pub(crate) fn walk_priced_events(&self, start: i64, end: i64) -> usize {
        let Some(walk) = &self.walk else { return 0 };
        let priced = |e: &&TickEvent| matches!(e, TickEvent::Quote(_) | TickEvent::Trade(_));
        let count = |events: &Vec<TickEvent>| {
            let from = events.partition_point(|e| e.timestamp() < start);
            let to = events.partition_point(|e| e.timestamp() <= end);
            events[from..to.max(from)].iter().filter(priced).count()
        };
        walk.tape.ticks.iter().map(|(_, events)| count(events)).sum()
    }

    /// The tape of `config` and `derived`: a cut of the walk's tape when `config` is the walk's but for a
    /// range inside it, else the one an earlier run prepared, else `prepare(derived)`'s, kept for the next. A
    /// failed preparation keeps nothing.
    pub(super) fn tape<E>(
        &self,
        config: &TickReplayConfig,
        derived: Vec<(String, Vec<TickQuote>)>,
        prepare: impl FnOnce(Vec<(String, Vec<TickQuote>)>) -> Result<TickTape, E>,
    ) -> Result<TapeCut, E> {
        if let Some(walk) = &self.walk
            && let (Some(start), Some(end)) = (config.range.start, config.range.end)
            && start >= walk.start
            && end <= walk.end
            && TapeKey::over(config, &derived, walk.key.range) == walk.key
            && let Some(window) = walk.cut(start, end)
        {
            return Ok(TapeCut { tape: Arc::clone(&walk.tape), window: Some(window) });
        }
        let key = TapeKey::of(config, &derived);
        let slot = {
            let mut tapes = self.tapes.lock().unwrap_or_else(|e| e.into_inner());
            match tapes.iter().find(|(k, _)| *k == key) {
                Some((_, slot)) => Arc::clone(slot),
                None => {
                    let slot = TapeSlot::default();
                    tapes.push((key, Arc::clone(&slot)));
                    slot
                }
            }
        };
        // Held across the preparation: a second run of this key waits here, then borrows. ⚠ So `prepare` must
        // not enter the search's rayon pool (nothing it calls does): a worker waiting on its own join could steal
        // another point and lock this slot a second time.
        let mut held = slot.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(tape) = held.as_ref() {
            return Ok(TapeCut::whole(Arc::clone(tape)));
        }
        let tape = Arc::new(prepare(derived)?);
        *held = Some(Arc::clone(&tape));
        Ok(TapeCut::whole(tape))
    }

    /// How many tapes this request prepared and holds (a walk's one not counted).
    #[cfg(test)]
    pub(crate) fn prepared(&self) -> usize {
        let tapes = self.tapes.lock().unwrap_or_else(|e| e.into_inner());
        tapes.iter().filter(|(_, slot)| slot.lock().is_ok_and(|t| t.is_some())).count()
    }
}

#[path = "tapes_tests.rs"]
#[cfg(test)]
mod tapes_tests;
