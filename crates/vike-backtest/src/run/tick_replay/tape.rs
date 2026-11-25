//! A tick replay's two halves: PREPARE ([`prepare_tape`]: the lane scans, the heard streams, the refusals that read
//! no tick, the merge, the R6 bar seed and its guard, into a [`TickTape`]) and RUN ([`run_tape`]: the engine params
//! the loader forces, `StrategyEngine::new` + `run_ticks` over a BORROWED cut of a tape, `super::tapes::TapeCut`).
//! The parent's module doc is the replay's contract; `super::tapes` is who holds a tape between runs.

use std::sync::{Arc, OnceLock};

use vike_analytics::TradingRecord;
use vike_data::{HistoryStore, HistoryStoreError};
use vike_marketdata::{
    Bar, L2BookUpdate, TickQuote, TickTrade, consolidate_quotes, consolidate_trades,
};
use vike_model::{Strategy, TickMark};
use vike_simulator::{FillModelKind, OrderBrokerSimulator, StrategyEngine, TickEvent};

use super::tapes::TapeCut;
use super::{
    ProfileDataSeries, SeriesKind, TickReplayConfig, TickReplayError, arrival_timestamp,
    heard_delay, heard_events, lanes_read, merge_ticks, merge_ticks_by_arrival, planned_lane_reads,
    properties_source, traded_legs, with_feed_status, with_leg_contract_values,
};
use crate::run::KeptRows;

/// What a tick replay runs, prepared: each slot's merged events (the heard streams after them), each slot's
/// seed bars, and what the data held wrong. `run_ticks` only reads it.
pub(crate) struct TickTape {
    pub(super) ticks: Vec<(String, Vec<TickEvent>)>,
    pub(super) bars: Vec<(String, Vec<Bar>)>,
    pub(super) data_warnings: Vec<String>,
}

/// Per-symbol `(symbol, quotes, trades, book events)` rows, already scanned from the store — the
/// shape [`prepare_tape`] hands to [`tape_of`]. A named alias, not an inline tuple
/// type, to keep clippy's `type_complexity` lint quiet across the call sites that spell it.
pub(super) type TickReplayRows = Vec<(String, Vec<TickQuote>, Vec<TickTrade>, Vec<L2BookUpdate>)>;

/// The words of a series with no event in its window: it did not run, and the report leaves it out.
pub(super) fn sparse_symbol_warning(symbol: &str) -> String {
    format!(
        "{symbol}: no quotes, trades or book events in this window, so it did not run and \
         this report does not include it"
    )
}

/// The refusals of `config` that read no tick: a bad seed interval, a symbol the ledger cannot book, an
/// unvalued coin-margined contract. [`prepare_tape`] asks them between the scans and the merge, and a run that
/// borrows a held tape asks them again of its own params.
pub(super) fn refuse_unrunnable(
    config: &TickReplayConfig,
    kept: &KeptRows,
) -> Result<(), TickReplayError> {
    // Validate up front: `consolidate_quotes`/`consolidate_trades` compute each bucket start via
    // `ts.rem_euclid(step_ms)`, which panics on `step_ms == 0` and is nonsensical for negative
    // steps. Catch it here rather than letting that panic surface from inside the per-symbol loop
    // below.
    if let Some(step) = config.seed_bar_interval_ms
        && step <= 0
    {
        return Err(TickReplayError::InvalidBarInterval(step));
    }
    // The simulator books one ledger currency: a symbol not quoted in the ledger's (`params.account_currency`, the
    // dollar when unset) is refused here, as the harness refuses it before it gets this far, unless a conversion
    // pair of the params books it (decision 0243). Each quote is read as a profile run reads it: its KEPT row's
    // (`kept`), else its name's (`crate::run::refuse_off_ledger_kept`).
    let series = config.resolved_series();
    let booked = &config.params.conversions;
    crate::run::refuse_off_ledger_kept(
        kept,
        series
            .iter()
            .filter(|s| !crate::run::data_plan::books(booked, &s.leg_name()))
            .map(|s| (s.venue.as_str(), s.symbol.as_str())),
        config.params.account_currency.as_deref(),
    )
    .map_err(|e| TickReplayError::ForeignQuote(e.message().to_string()))?;
    // A coin-margined contract (its KEPT row's margining, else its name's) is booked only by the value its caller
    // handed the engine (a profile run's coin ledger, `crate::run::run_ledger_coin`); one without it would
    // book as linear dollars.
    let valued = |symbol: &str| config.params.contract_values.iter().any(|(s, _)| s == symbol);
    let inverse = |s: &ProfileDataSeries| {
        vike_catalog::kept_is_inverse_contract(&s.venue, &s.symbol, kept.row(&s.venue, &s.symbol))
    };
    crate::run::refuse_inverse_contracts(
        series
            .iter()
            .filter(|s| !valued(&s.symbol) && inverse(s))
            .map(|s| (s.venue.as_str(), s.symbol.as_str())),
    )
    .map_err(|e| TickReplayError::InverseContract(e.message().to_string()))
}

/// The tape of each symbol's already-obtained `(TickQuote, TickTrade, L2BookUpdate)` rows (ts-ascending within
/// each `Vec`, per the `HistoryStore` contract): the sparse-symbol skip, [`merge_ticks`], the R6 in-process bar
/// seed and the `MisalignedSeededBars` guard. `heard` is each heard series' events ([`heard_events`], a traded
/// leg's touch included), labelled for its warning, run after the symbol streams so a heard event delivered in
/// the same millisecond as a market event follows it.
pub(super) fn tape_of(
    rows_by_symbol: TickReplayRows,
    heard: Vec<(String, Vec<TickEvent>)>,
    config: &TickReplayConfig,
) -> Result<TickTape, TickReplayError> {
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
            data_warnings.push(sparse_symbol_warning(&symbol));
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
        let mut ticks = if config.feed_latency {
            merge_ticks_by_arrival(quotes, trades, books)
        } else {
            merge_ticks(quotes, trades, books)
        };
        // The mounted series (the first slot, the order seam's `""`) is the one whose feed status a
        // live mount hears (`TickReplayConfig::feed_status`).
        if config.feed_status && ticks_by_symbol.is_empty() {
            ticks = with_feed_status(ticks);
        }
        ticks_by_symbol.push((symbol.clone(), ticks));
        bars_by_symbol.push((symbol, bars));
    }
    for (label, events) in heard {
        if events.is_empty() {
            data_warnings.push(format!(
                "{label}: nothing to hear in this window, so the strategy heard nothing from it"
            ));
        } else {
            ticks_by_symbol.push((label, events));
        }
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
    Ok(TickTape { ticks: ticks_by_symbol, bars: bars_by_symbol, data_warnings })
}

/// `super::replay_ticks_kept`'s RUN half: forcing `fill_model`/`default_venue`, the traded legs and their contract
/// values, the snapped grid, and the `StrategyEngine::new` + `run_ticks` call over `cut`, which it only borrows (a
/// held tape whole, or a walk window's range of each stream), so one tape serves every run of a request.
pub(super) fn run_tape<S: Strategy<OrderBrokerSimulator>>(
    store: Arc<dyn HistoryStore + Send + Sync>,
    strategy: S,
    mut config: TickReplayConfig,
    cut: &TapeCut,
) -> Result<TradingRecord, TickReplayError> {
    let first_error: Arc<OnceLock<String>> = Arc::default();
    if config.snap_to_properties {
        config.params.properties = Some(properties_source(store, Arc::clone(&first_error)));
    }
    let series = config.resolved_series();
    // R: tick replay is the L1 spread-crossing tier by DEFAULT, tagged to exactly one venue. Respect
    // an explicit `L2Book` (depth-capped) choice the caller set — that tier degrades to `TickEvent`
    // wherever no book exists, so it is safe on any tick tape — but never leave a `Bar` model on the
    // tick lane. `default_venue` is always overwritten (tick replay is single-venue).
    if config.params.fill_model != FillModelKind::L2Book {
        config.params.fill_model = FillModelKind::Tick;
    }
    config.params.default_venue = Some(config.venue.clone());
    // The references the run trades are its foreign legs: they fill on their own events and reach no
    // hook, their touch heard on the delayed `heard` streams.
    config.params.reference_legs =
        traded_legs(&config.reference).map(|s| (s.symbol, s.venue)).collect();
    // A leg named by its account books at its symbol's contract value (its slot is the leg's name).
    config.params.contract_values =
        with_leg_contract_values(std::mem::take(&mut config.params.contract_values), &series);

    let mut engine = StrategyEngine::new(cut.bars(), strategy, config.params);
    let mut result = engine.run_ticks(&cut.streams());
    result.data_warnings = cut.data_warnings();
    match first_error.get() {
        Some(why) => Err(TickReplayError::Data(HistoryStoreError::Query(why.clone()))),
        None => Ok(result),
    }
}

/// `super::replay_ticks_kept`'s PREPARE half: the lane scans (`derived` taking a series' place), the account legs'
/// renaming, the heard streams, then [`refuse_unrunnable`] (with `kept`; `None` for a walk's tape, whose every
/// window refuses its own) and [`tape_of`]. ⚠ Everything it reads is `super::tapes`' key: a new input joins
/// `TapeKey` in the same change.
pub(super) fn prepare_tape(
    store: &Arc<dyn HistoryStore + Send + Sync>,
    config: &TickReplayConfig,
    kept: Option<&KeptRows>,
    mut derived: Vec<(String, Vec<TickQuote>)>,
) -> Result<TickTape, TickReplayError> {
    let series = config.resolved_series();
    // Every lane read below, named to the store AHEAD (`vike_data::HistoryStore::prefetch`: a sandboxed child asks
    // its hub for all of them at once, and the hub fetches them together). Each is still made, in this order, by its
    // verb, so the replay's refusals keep their order.
    store.prefetch(&planned_lane_reads(config, &series, &derived));
    let mut rows: TickReplayRows = Vec::with_capacity(series.len() + derived.len());
    let range = config.range;
    let scan = |venue: &str, symbol: &str, kind: SeriesKind, prints: bool| {
        let [quotes, trades, books] = lanes_read(kind, prints);
        let quotes = if quotes { store.scan_quotes(venue, symbol, range)? } else { Vec::new() };
        let trades = if trades { store.scan_trades(venue, symbol, range)? } else { Vec::new() };
        let books = if books { store.scan_book_updates(venue, symbol, range)? } else { Vec::new() };
        Ok::<_, HistoryStoreError>((quotes, trades, books))
    };
    for s in &series {
        if let Some(at) = derived.iter().position(|(symbol, _)| *symbol == s.symbol) {
            let (symbol, quotes) = derived.remove(at);
            rows.push((symbol, quotes, Vec::new(), Vec::new()));
            continue;
        }
        let (mut quotes, mut trades, mut books) = scan(&s.venue, &s.symbol, s.kind, true)?;
        // A series naming its account is the leg `SYMBOL@account` (decision 0293): `run_ticks` routes a tick
        // to its slot by the tick's own symbol, so each tick names the leg.
        let slot = s.leg_name();
        if s.account.is_some() {
            let tick_slot = vike_marketdata::Symbol::new(&slot);
            quotes.iter_mut().for_each(|q| q.symbol = tick_slot);
            trades.iter_mut().for_each(|t| t.symbol = tick_slot);
            books.iter_mut().for_each(|b| b.symbol.clone_from(&slot));
        }
        rows.push((slot, quotes, trades, books));
    }
    rows.extend(
        derived.into_iter().map(|(symbol, quotes)| (symbol, quotes, Vec::new(), Vec::new())),
    );
    // A heard series in the order this box received it (an unstamped one stays in venue order), each
    // event heard at its arrival clock plus the stream's delay (the module doc's "Heard streams").
    let received = |(quotes, trades, books)| merge_ticks_by_arrival(quotes, trades, books);
    let mut heard: Vec<(String, Vec<TickEvent>)> = Vec::new();
    // A reference is heard as its touch alone; one the run trades fills as its slot above and is heard
    // off that slot's rows.
    for r in &config.reference {
        let lanes = match rows.iter().find(|(symbol, ..)| r.trade && *symbol == r.symbol) {
            Some((_, quotes, _, books)) => (quotes.clone(), Vec::new(), books.clone()),
            None => scan(&r.venue, &r.symbol, r.kind, false)?,
        };
        let ticks = received(lanes);
        let stream = format!("data.reference {:?} on {:?}", r.symbol, r.venue);
        let delay = heard_delay(&stream, r.delay_ms, &ticks)?;
        let venue = &r.venue;
        let touch = |quote: TickQuote| TickEvent::Reference {
            venue: venue.clone(),
            heard_at: arrival_timestamp(quote.timestamp, quote.local_timestamp)
                .saturating_add_unsigned(delay),
            quote: Box::new(quote),
        };
        let events = heard_events(ticks, &r.symbol, touch, |_| {
            unreachable!("a reference's prints are unread")
        });
        heard.push((format!("reference {} {}", r.venue, r.symbol), events));
    }
    for m in &config.mark {
        let ticks = received(scan(&m.venue, &m.symbol, m.kind, true)?);
        let stream = format!("data.mark {:?} on {:?}", m.symbol, m.venue);
        let delay = heard_delay(&stream, m.delay_ms, &ticks)?;
        let mark = |price: f64, timestamp: i64, local: i64| TickEvent::Mark {
            mark: TickMark { symbol: m.symbol.clone(), price, timestamp },
            heard_at: arrival_timestamp(timestamp, local).saturating_add_unsigned(delay),
        };
        let touch = |q: TickQuote| mark(q.mid(), q.timestamp, q.local_timestamp);
        let print = |t: TickTrade| mark(t.price, t.timestamp, t.local_timestamp);
        let events = heard_events(ticks, &m.symbol, touch, print);
        heard.push((format!("mark {} {}", m.venue, m.symbol), events));
    }
    if let Some(kept) = kept {
        refuse_unrunnable(config, kept)?;
    }
    tape_of(rows, heard, config)
}
