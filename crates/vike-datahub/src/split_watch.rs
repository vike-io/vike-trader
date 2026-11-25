//! **The split warning: a stored share series' overnight jump that looks like a split no split row covers** (the
//! owner's ruling of 2026-11-17; `docs/decisions/0289-a-backtest-split-adjusts-a-share-series-from-the-operators-split-rows.md`'s
//! amendment of that day).
//!
//! Split rows are filled only at the operator's refresh (`crate::stock_splits`, no timer). A split that lands later
//! shows in the data first, so this module watches what the store writes: when a share series' first price of a UTC
//! day sits within its tolerance of one of [`SPLIT_RATIOS`] (or an inverse, a reverse split) against the
//! previous stored day's last price, and no `stock_split` row (any source) names that venue, symbol and date, it logs
//! one warning ([`SplitWarning::message`]: the split in words and the refresh command) and keeps it for
//! `Request::SplitWarnings`, the Data Manager's read, which drops it once a row covers it.
//!
//! * **Where, and why there.** In the store's write, the one place every writer of a series meets: the archive import,
//!   the backfill rows, the recorder's lanes, the start history and a user's upload all append through the ONE
//!   `vike_store::HistoryStoreDataFusion` this daemon serves, and the boot shows it to the watch
//!   (`vike_store::PriceWatch`, [`SplitWatch::price_watch`]). The Data Manager's coverage and gap passes read
//!   manifests only, never a price.
//! * **What it costs a write.** One lock, one map lookup and one day comparison per append: the same day of a series
//!   goes on with nothing more. A new day reads the series' class (its KEPT row, `vike_catalog::kept_list`, through
//!   `vike_model::Contract::is_share`; never its name) and makes one comparison. The store is read (outside the lock,
//!   at most [`LOOKBACK_DAYS`] back) only when the previous price is not the watch's own: the series' first write in
//!   this process, a write older than the last one, or one more than a calendar day on.
//! * **What it never does.** It writes no split row and asks no vendor: the refresh stays the one writer.
//! * ⚠ **The warned set is MEMORY**, with the open warnings: a warning is a prompt to run one command and its line is in
//!   the log; a row of `vike.db` would be a second record of what the split rows already decide. A restart forgets the
//!   open ones (a day already stored is not written again), and the next write of a later day of that series compares
//!   only that day.
//! * A day is a UTC day: a share whose session crosses 00:00 UTC is compared at midnight, not at its open.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Mutex, PoisonError};

use vike_data::{HistoryStore, TimestampRange};
use vike_datahub_client::catalog::SplitWarning;
use vike_marketdata::{Bar, TickQuote};
use vike_model::{MS_PER_DAY, utc_day};

/// The splits a jump is judged against, `(new shares, old shares, relative tolerance)`; each is also judged inverted
/// (a reverse split). 2-for-1 and above take 3%; 3-for-2 only 1%, because a real one lands within rounding of 1.5 and
/// a share that loses about a third overnight on its earnings would otherwise read as one.
pub(crate) const SPLIT_RATIOS: [(u32, u32, f64); 7] = [
    (2, 1, 0.03),
    (3, 1, 0.03),
    (4, 1, 0.03),
    (5, 1, 0.03),
    (10, 1, 0.03),
    (20, 1, 0.03),
    (3, 2, 0.01),
];

/// How far back the previous stored price is looked for: a long weekend and a holiday with room to spare. A series
/// that stored nothing in that span is compared with nothing.
const LOOKBACK_DAYS: i64 = 7;

/// **The split `before → after` looks like**, `(new shares, old shares)`, or `None`: the price before divided by the
/// price after is new shares per old share (1400 → 140 is a 10-for-1 split, 1 → 10 a 1-for-10 reverse one).
pub(crate) fn split_like(before: f64, after: f64) -> Option<(u32, u32)> {
    if !(before.is_finite() && after.is_finite() && before > 0.0 && after > 0.0) {
        return None;
    }
    let moved = before / after;
    SPLIT_RATIOS
        .iter()
        .flat_map(|&(new, old, tolerance)| [(new, old, tolerance), (old, new, tolerance)])
        .find(|&(new, old, tolerance)| {
            (moved / (f64::from(new) / f64::from(old)) - 1.0).abs() <= tolerance
        })
        .map(|(new, old, _)| (new, old))
}

/// One stored row's instant and its first and last price: a bar's open and close, a quote's mid.
pub trait Priced {
    fn at(&self) -> i64;
    fn first(&self) -> f64;
    fn last(&self) -> f64;
}

impl Priced for Bar {
    fn at(&self) -> i64 {
        self.timestamp
    }
    fn first(&self) -> f64 {
        self.open
    }
    fn last(&self) -> f64 {
        self.close
    }
}

impl Priced for TickQuote {
    fn at(&self) -> i64 {
        self.timestamp
    }
    fn first(&self) -> f64 {
        self.mid()
    }
    fn last(&self) -> f64 {
        self.mid()
    }
}

/// The series one write stored: `interval` is a bar series', `None` the quote series; `source` a `source=` lane.
#[derive(Clone, Copy)]
pub struct WrittenSeries<'a> {
    pub venue: &'a str,
    pub source: Option<&'a str>,
    pub symbol: &'a str,
    pub interval: Option<&'a str>,
}

impl WrittenSeries<'_> {
    fn key(&self) -> String {
        let lane = self.interval.unwrap_or("quote");
        format!(
            "{}\u{1f}{}\u{1f}{}\u{1f}{lane}",
            self.venue,
            self.source.unwrap_or(""),
            self.symbol
        )
    }

    /// A share or a CFD on a share, by its kept row; a series with no kept row is none.
    fn is_share(&self) -> bool {
        vike_catalog::kept_list(self.venue).is_some_and(|rows| {
            vike_catalog::kept_row_of_store_name(&rows, self.venue, self.symbol)
                .is_ok_and(|row| row.contract.is_share())
        })
    }

    /// The last price stored before `at` within [`LOOKBACK_DAYS`], and its day. Quotes are read a day at a time
    /// back, so a busy tape reads the one day it needs.
    fn previous_price(&self, store: &dyn HistoryStore, at: i64) -> Option<(i64, f64)> {
        let floor = at - LOOKBACK_DAYS * MS_PER_DAY;
        let (venue, symbol) = (self.venue, self.symbol);
        if let Some(interval) = self.interval {
            let range = TimestampRange::of(floor, at - 1);
            let bars = match self.source {
                None => store.load_bars(venue, symbol, interval, range),
                Some(lane) => {
                    store.load_bars_sourced_capped(venue, lane, symbol, interval, range, None)
                }
            };
            return bars.ok()?.last().map(|bar| (utc_day(bar.timestamp), bar.close));
        }
        let mut end = at - 1;
        while end >= floor {
            let start = (utc_day(end) * MS_PER_DAY).max(floor);
            let range = TimestampRange::of(start, end);
            let quotes = match self.source {
                None => store.scan_quotes(venue, symbol, range),
                Some(lane) => store.scan_quotes_sourced_capped(venue, lane, symbol, range, None),
            };
            if let Some(quote) = quotes.ok()?.last() {
                return Some((utc_day(quote.timestamp), quote.mid()));
            }
            end = start - 1;
        }
        None
    }
}

/// A series' last write as the watch saw it.
#[derive(Clone, Copy)]
struct Cursor {
    day: i64,
    at: i64,
    price: f64,
    share: bool,
}

#[derive(Default)]
struct WatchState {
    series: HashMap<String, Cursor>,
    /// The open warnings by `(venue, symbol, date)`: also the warned set, so a date warns once.
    warnings: BTreeMap<(String, String, String), SplitWarning>,
}

/// **The datahub's split watch**: the open warnings and each series' last stored price. One per process, built by the
/// boot over the settings store the split rows are read from.
pub struct SplitWatch {
    settings: Option<vike_secrets::CredentialStore>,
    state: Mutex<WatchState>,
}

impl SplitWatch {
    /// A watch that reads the split rows from `settings` (`None`: no row covers anything).
    #[must_use]
    pub fn new(settings: Option<vike_secrets::CredentialStore>) -> Self {
        Self { settings, state: Mutex::default() }
    }

    /// **What the store shows its writes to** (`vike_store::HistoryStoreDataFusion::watch_prices`).
    #[cfg(feature = "datafusion-serve")]
    pub fn price_watch(self: &std::sync::Arc<Self>) -> vike_store::PriceWatch {
        let watch = std::sync::Arc::clone(self);
        std::sync::Arc::new(move |store: &dyn HistoryStore, write: &vike_store::PriceWrite<'_>| {
            let series = |interval| WrittenSeries {
                venue: write.venue,
                source: write.source,
                symbol: write.symbol,
                interval,
            };
            match write.prices {
                vike_store::WrittenPrices::Bars { interval, bars } => {
                    watch.observe(store, series(Some(interval)), bars);
                }
                vike_store::WrittenPrices::Quotes(quotes) => {
                    watch.observe(store, series(None), quotes);
                }
            }
        })
    }

    /// **One write of `series`**, its `rows` as stored (time-ascending, as every writer appends them): the module
    /// doc's rule, on the writer's thread.
    pub fn observe<R: Priced>(
        &self,
        store: &dyn HistoryStore,
        series: WrittenSeries<'_>,
        rows: &[R],
    ) {
        let (Some(first), Some(last)) = (rows.first(), rows.last()) else { return };
        let (first_day, last_day) = (utc_day(first.at()), utc_day(last.at()));
        let key = series.key();
        let cursor = {
            let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            match state.series.get_mut(&key) {
                // The same day of the series goes on: nothing to compare.
                Some(c) if c.day == first_day && last_day == first_day && first.at() >= c.at => {
                    (c.at, c.price) = (last.at(), last.last());
                    return;
                }
                other => other.copied(),
            }
        };
        let share = series.is_share();
        let mut jumps = Vec::new();
        if share {
            let mut before = match cursor {
                Some(c) if c.share && first.at() >= c.at && first_day - c.day <= 1 => {
                    Some((c.day, c.price))
                }
                _ => series.previous_price(store, first.at()),
            };
            for row in if first_day == last_day { &rows[..1] } else { rows } {
                let day = utc_day(row.at());
                if let Some((prior, price)) = before
                    && day > prior
                {
                    jumps.push((day, price, row.first()));
                }
                before = Some((day, row.last()));
            }
        }
        let mut found: Vec<SplitWarning> = jumps
            .into_iter()
            .filter_map(|(day, price_before, price_after)| {
                let (new_shares, old_shares) = split_like(price_before, price_after)?;
                Some(SplitWarning {
                    venue: series.venue.to_string(),
                    symbol: series.symbol.to_string(),
                    date: vike_model::time::epoch_ms_to_utc_date(day * MS_PER_DAY),
                    new_shares,
                    old_shares,
                    price_before,
                    price_after,
                })
            })
            .collect();
        if !found.is_empty() {
            let rows = self.split_rows();
            found.retain(|w| !covered(&rows, w));
        }
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        // The LAST write, not the newest: an import of older days chains day by day as a live tape does.
        state
            .series
            .insert(key, Cursor { day: last_day, at: last.at(), price: last.last(), share });
        for warning in found {
            let at = (warning.venue.clone(), warning.symbol.clone(), warning.date.clone());
            if let std::collections::btree_map::Entry::Vacant(open) = state.warnings.entry(at) {
                tracing::warn!(venue = %warning.venue, "{}", warning.message());
                open.insert(warning);
            }
        }
    }

    /// **The open warnings**, oldest date first: every warning no split row covers now (read at the ask, so a refresh
    /// that wrote the row ends it).
    #[must_use]
    pub fn open(&self) -> Vec<SplitWarning> {
        let mut open: Vec<SplitWarning> = {
            let state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            state.warnings.values().cloned().collect()
        };
        if !open.is_empty() {
            let rows = self.split_rows();
            open.retain(|w| !covered(&rows, w));
        }
        open.sort_by(|a, b| (&a.date, &a.venue, &a.symbol).cmp(&(&b.date, &b.venue, &b.symbol)));
        open
    }

    /// Every split row of the settings store, any source; none without a store, and none (logged) when it cannot be
    /// read, so a warning shows rather than hides.
    fn split_rows(&self) -> Vec<vike_model::StockSplit> {
        let Some(location) =
            self.settings.as_ref().and_then(vike_secrets::CredentialStore::location)
        else {
            return Vec::new();
        };
        vike_secrets::stock_split::read_stock_splits(location.settings_dir()).unwrap_or_else(|e| {
            tracing::error!("split warning: the split rows could not be read: {e}");
            Vec::new()
        })
    }
}

/// Whether a split row names the warning's venue, symbol and date.
fn covered(rows: &[vike_model::StockSplit], warning: &SplitWarning) -> bool {
    rows.iter()
        .any(|r| r.venue == warning.venue && r.symbol == warning.symbol && r.date == warning.date)
}

// Over the real store, which the serving build links (this crate's self dev-dependency turns it on for every test).
#[path = "split_watch_tests.rs"]
#[cfg(all(test, feature = "datafusion-serve"))]
mod split_watch_tests;
