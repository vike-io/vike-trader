//! `HistoryStoreSynthetic`: a READ-ONLY, in-memory `HistoryStore` over generated data, under the venue
//! [`SYNTHETIC_VENUE`]: what a study or a test hands a backtest to run it on synthetic data
//! (`vike_analytics::synthetic` generates bar paths and order books; a study hands this store to
//! `vike_user_research::StudySim::run_one`, a test to `vike_backtest::run::run_backtest` or
//! `vike_backtest::run::tick_replay::replay_ticks`).
//!
//! Generated prices are a function of a seed, not market data, so they get a venue of their own, for
//! the reason `crate::demo`'s module doc gives for its `demo` venue: a read of any other venue is
//! REFUSED, so a profile never runs a synthetic path as a real venue's tape. And the store lives in
//! memory only: no synthetic row is ever written to a user's store, which since the owner's ruling of
//! 2026-11-13 holds real data only (the same module doc).
//!
//! It holds any number of `(symbol, interval)` bar series ([`HistoryStoreSynthetic::with_series`]),
//! each on its interval's exact grid with no hole, and any number of order books
//! ([`HistoryStoreSynthetic::with_book`]): one symbol's book updates, quotes and trades, the three
//! streams a tick replay reads. Another series of the synthetic venue is the missing series' empty
//! answer; every write, every lane and every properties and account read is refused with the reason,
//! as `crate::HistoryStoreCsv` refuses them.

use vike_marketdata::interval::bar_step_ms;
use vike_marketdata::{Bar, L2BookUpdate, L2BookUpdateKind, TickQuote, TickTrade};

use crate::store::hist::{HistoryStore, HistoryStoreError, TimestampRange};
use crate::store::series::{SeriesCoverage, SeriesId};
use vike_model::StoreKindId;

/// The venue every synthetic series is held and read under. NOT a real venue (the module doc).
pub const SYNTHETIC_VENUE: &str = "synthetic";

/// Generated bar series and order books in memory, read under [`SYNTHETIC_VENUE`] (the module doc).
#[derive(Debug, Clone, Default)]
pub struct HistoryStoreSynthetic {
    series: Vec<(SeriesId, Vec<Bar>)>,
    books: Vec<SyntheticBook>,
}

/// One symbol's three tick streams, every row stamped with that symbol.
#[derive(Debug, Clone)]
struct SyntheticBook {
    symbol: String,
    updates: Vec<L2BookUpdate>,
    quotes: Vec<TickQuote>,
    trades: Vec<TickTrade>,
}

impl HistoryStoreSynthetic {
    /// This store with `bars` added as the series `(SYNTHETIC_VENUE, symbol, interval)`.
    ///
    /// # Errors
    /// [`HistoryStoreError::Query`] when `interval` has no fixed width, when two bars are not exactly
    /// one interval apart (in ascending order), or when the store already holds that series.
    pub fn with_series(
        mut self,
        symbol: &str,
        interval: &str,
        bars: Vec<Bar>,
    ) -> Result<Self, HistoryStoreError> {
        let Some(step_ms) = bar_step_ms(interval) else {
            return Err(HistoryStoreError::Query(format!(
                "the interval {interval:?} has no fixed width, so a synthetic path cannot be held on it"
            )));
        };
        let one_step = |w: &[Bar]| w[1].timestamp.checked_sub(w[0].timestamp) == Some(step_ms);
        if let Some(i) = bars.windows(2).position(|w| !one_step(w)) {
            return Err(HistoryStoreError::Query(format!(
                "synthetic {symbol} {interval}: bars {i} and {} (at {} and {}) are not {step_ms} ms \
                 apart, and a path on {interval} holds one bar every {step_ms} ms, ascending, with \
                 no hole",
                i + 1,
                bars[i].timestamp,
                bars[i + 1].timestamp
            )));
        }
        let id = SeriesId::per_symbol(
            StoreKindId::Bar,
            SYNTHETIC_VENUE,
            symbol,
            Some(interval.to_string()),
        );
        if self.series.iter().any(|(held, _)| *held == id) {
            return Err(HistoryStoreError::Query(format!(
                "this synthetic store already holds {symbol} {interval}"
            )));
        }
        self.series.push((id, bars));
        Ok(self)
    }

    /// This store with `symbol`'s order book: its `updates` (a `Snapshot` first, then contiguous
    /// `sequence` numbers), `quotes` and `trades`, each in time order; every row is stamped `symbol`.
    ///
    /// # Errors
    /// [`HistoryStoreError::Query`] when the updates do not open with a snapshot or skip a sequence
    /// number, when a stream goes back in time, or when the store already holds a book of `symbol`.
    pub fn with_book(
        mut self,
        symbol: &str,
        mut updates: Vec<L2BookUpdate>,
        mut quotes: Vec<TickQuote>,
        mut trades: Vec<TickTrade>,
    ) -> Result<Self, HistoryStoreError> {
        let refuse =
            |why: String| Err(HistoryStoreError::Query(format!("synthetic {symbol}: {why}")));
        let Some(first) = updates.first() else {
            return refuse("a book needs at least its snapshot".to_string());
        };
        if first.kind != L2BookUpdateKind::Snapshot {
            return refuse(format!("a book opens with a snapshot, not a {:?}", first.kind));
        }
        let start = first.sequence;
        if let Some(i) =
            (0_u64..).zip(&updates).position(|(i, u)| Some(u.sequence) != start.checked_add(i))
        {
            return refuse(format!("update {i} breaks the contiguous sequence from {start}"));
        }
        for (name, times) in [
            ("updates", updates.iter().map(|u| u.timestamp).collect::<Vec<_>>()),
            ("quotes", quotes.iter().map(|q| q.timestamp).collect()),
            ("trades", trades.iter().map(|t| t.timestamp).collect()),
        ] {
            if let Some(i) = times.windows(2).position(|w| w[1] < w[0]) {
                return refuse(format!("its {name} go back in time after row {i}"));
            }
        }
        if self.books.iter().any(|b| b.symbol == symbol) {
            return refuse("this synthetic store already holds its book".to_string());
        }
        updates.iter_mut().for_each(|u| u.symbol = symbol.to_string());
        quotes.iter_mut().for_each(|q| q.symbol = symbol.into());
        trades.iter_mut().for_each(|t| t.symbol = symbol.into());
        self.books.push(SyntheticBook { symbol: symbol.to_string(), updates, quotes, trades });
        Ok(self)
    }

    /// The book of `symbol` under `venue`: `None` when it holds none, `Err` for another venue.
    fn book(&self, venue: &str, symbol: &str) -> Result<Option<&SyntheticBook>, HistoryStoreError> {
        refuse_another_venue(venue)?;
        Ok(self.books.iter().find(|b| b.symbol == symbol))
    }

    /// Every series this store holds, with its coverage: the bar series, then each book's three.
    fn holdings(&self) -> Vec<(SeriesId, SeriesCoverage)> {
        let bars = self
            .series
            .iter()
            .map(|(id, bars)| (id.clone(), coverage(bars.iter().map(|b| b.timestamp), bars.len())));
        let books = self.books.iter().flat_map(|book| {
            let id = |kind| SeriesId::per_symbol(kind, SYNTHETIC_VENUE, book.symbol.clone(), None);
            [
                (
                    id(StoreKindId::Book),
                    coverage(book.updates.iter().map(|u| u.timestamp), book.updates.len()),
                ),
                (
                    id(StoreKindId::Quote),
                    coverage(book.quotes.iter().map(|q| q.timestamp), book.quotes.len()),
                ),
                (
                    id(StoreKindId::Trade),
                    coverage(book.trades.iter().map(|t| t.timestamp), book.trades.len()),
                ),
            ]
        });
        bars.chain(books).collect()
    }
}

/// A series' coverage from its time-ordered timestamps; `bytes`, `parts` and `dates` are a store's
/// on-disk facts and stay 0, as `crate::HistoryStoreCsv`'s do.
fn coverage(mut timestamps: impl DoubleEndedIterator<Item = i64>, rows: usize) -> SeriesCoverage {
    match (timestamps.next(), timestamps.next_back()) {
        (Some(first), last) => SeriesCoverage {
            first_timestamp: first,
            last_timestamp: last.unwrap_or(first),
            rows: rows as u64,
            ..Default::default()
        },
        (None, _) => SeriesCoverage::default(),
    }
}

/// `Ok` for the synthetic venue; every other venue is refused (the module doc).
fn refuse_another_venue(venue: &str) -> Result<(), HistoryStoreError> {
    if venue == SYNTHETIC_VENUE {
        return Ok(());
    }
    Err(HistoryStoreError::Query(format!(
        "a synthetic store holds generated data under the venue {SYNTHETIC_VENUE:?} only, never \
         {venue:?}'s: name {SYNTHETIC_VENUE:?} as the profile's [data] venue"
    )))
}

/// The rows of `rows` whose timestamp is inside `range` (inclusive).
fn in_range<T: Clone>(rows: &[T], range: TimestampRange, timestamp: impl Fn(&T) -> i64) -> Vec<T> {
    rows.iter()
        .filter(|r| {
            let t = timestamp(r);
            range.start.is_none_or(|s| t >= s) && range.end.is_none_or(|e| t <= e)
        })
        .cloned()
        .collect()
}

/// The refusal of every verb a store of generated data does not serve, for `crate::hist_store_stubs!`.
fn refused(verb: &'static str) -> HistoryStoreError {
    HistoryStoreError::Unsupported(format!(
        "{verb}: a synthetic store holds generated bar series and order books only and writes nothing"
    ))
}

impl HistoryStore for HistoryStoreSynthetic {
    /// The series' bars in `range` (inclusive), ts-ascending; another series of [`SYNTHETIC_VENUE`] is
    /// empty, another venue REFUSED.
    fn load_bars(
        &self,
        venue: &str,
        symbol: &str,
        interval: &str,
        range: TimestampRange,
    ) -> Result<Vec<Bar>, HistoryStoreError> {
        refuse_another_venue(venue)?;
        let held = self
            .series
            .iter()
            .find(|(id, _)| id.symbol == symbol && id.interval.as_deref() == Some(interval));
        Ok(held.map(|(_, bars)| in_range(bars, range, |b| b.timestamp)).unwrap_or_default())
    }

    /// The book's quotes in `range`; a symbol with no book has none.
    fn scan_quotes(
        &self,
        venue: &str,
        symbol: &str,
        range: TimestampRange,
    ) -> Result<Vec<TickQuote>, HistoryStoreError> {
        let book = self.book(venue, symbol)?;
        Ok(book.map(|b| in_range(&b.quotes, range, |q| q.timestamp)).unwrap_or_default())
    }

    /// The book's trades in `range`; a symbol with no book has none.
    fn scan_trades(
        &self,
        venue: &str,
        symbol: &str,
        range: TimestampRange,
    ) -> Result<Vec<TickTrade>, HistoryStoreError> {
        let book = self.book(venue, symbol)?;
        Ok(book.map(|b| in_range(&b.trades, range, |t| t.timestamp)).unwrap_or_default())
    }

    /// The book's updates in `range`; a symbol with no book has none.
    fn scan_book_updates(
        &self,
        venue: &str,
        symbol: &str,
        range: TimestampRange,
    ) -> Result<Vec<L2BookUpdate>, HistoryStoreError> {
        let book = self.book(venue, symbol)?;
        Ok(book.map(|b| in_range(&b.updates, range, |u| u.timestamp)).unwrap_or_default())
    }

    fn list_series(&self) -> Result<Vec<SeriesId>, HistoryStoreError> {
        Ok(self.holdings().into_iter().map(|(id, _)| id).collect())
    }

    /// Each series and its coverage. A bar series holds no hole, so the trait's empty `series_gaps`
    /// is its answer.
    fn inventory(&self) -> Result<Vec<(SeriesId, SeriesCoverage)>, HistoryStoreError> {
        Ok(self.holdings())
    }

    crate::hist_store_stubs!(refuse(refused): writes, scan_symbol_properties, scan_equity,
        scan_exec_fills, scan_exec_orders);
}

#[path = "synthetic_store_tests.rs"]
#[cfg(test)]
mod synthetic_store_tests;
