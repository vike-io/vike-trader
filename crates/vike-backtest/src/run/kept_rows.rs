//! **The KEPT instrument rows a run reads its per-symbol facts from**: the datahub's lists
//! (`<project>/instruments/<venue>.json`, which only the datahub reads and writes:
//! `docs/decisions/0084-only-the-datahub-touches-the-store.md`), asked ONCE per run by the door that resolves the
//! profile (`ProfileBacktest::read_kept_rows`) through the run's own store (`vike_data::HistoryStore::kept_instruments`,
//! the datahub's `Request::InstrumentLists`). Four readers:
//!
//! - an input's coverage: its market's session class (`crate::run::data_plan`'s trading calendar over
//!   `vike_catalog::trading_calendar`);
//! - the ledger guard: a symbol's fiat quote (`vike_catalog::kept_fiat_quote`);
//! - the coin ledger: an inverse contract's face where only its row states it (`vike_catalog::kept_contract_value`);
//! - the strategy's tick where neither `[risk]` nor a dated properties record states one ([`KeptRows::ticks`], the
//!   owner's ruling of 2026-11-19): the row's TICK alone, its lot and minimums unread.
//!
//! A series with no kept row keeps today's answer for each reader (class unknown, the quote and the face its name
//! gives) and the run's data warnings name it; a binance COIN-M or okx inverse series, whose face no name gives, is
//! refused (`crate::run::run_ledger_coin`). A store that serves no kept list (`Ok(None)`: a test's store, the
//! lab's, anything but the datahub's handle) hands nothing and warns nothing, so such a run is what it was before the
//! lists reached a backtest, but for that refusal.
//!
//! A door that knows the venues its run reads (`crate::run::data_plan::kept_venues`: its series, the
//! inputs the strategy declares, its conversion pairs and option chain) asks for those venues' rows alone
//! (`vike_data::HistoryStore::kept_instruments_and_splits_of`), and every lookup after is of one of them, so it answers
//! what the whole lists answer. A sandboxed child asks its parent the same way
//! (`crates/vike-backtesthub/src/strategy_sandbox/pipe_store.rs`'s `PipeHistStore`).
//!
//! # The names (decision 0290)
//!
//! A profile's symbols, a strategy's inputs and a conversion pair name an instrument by its USER name, typed in any
//! ASCII case (`bybit` `btcusdt.p`); the series read is its STORE name ([`KeptRows::store_name`]: the one row of
//! the venue's kept list the name names, `vike_catalog::names_of_user_name`). A name its venue's list carries no row
//! of, or no list at all, is refused before the run in the translator's words; a polymarket name is an outcome's
//! token id (`vike_catalog::is_outcome_token`), which no list needs to carry; the user venue's series (decision 0272)
//! is no venue's instrument and is read as typed. Where no list was served ([`KeptRows::lists`] `None`: a test's
//! store, a datahub too old to keep them) every name is read as typed, as before decision 0290. Every fact lookup
//! after that is by the store name (`vike_catalog::kept_row_of_store_name`).
//!
//! # The operator's share splits ([`KeptRows::splits`], decision 0289)
//!
//! Served by the datahub with the same lists (`vike_data::HistoryStore::stock_splits`), asked by the same doors, of the
//! same venues. A run reading a series that has a split from the venue's own
//! candles adjusts it ([`KeptRows::adjust_for_splits`], as TradingView shows a share's history): every bar earlier than
//! a split has its prices divided by the ratio and its volume multiplied by it, later splits compounding, and the
//! report says so in one line per split. The stored bars are never rewritten. A series with no split, a lane's bars
//! and a file's bars (which may come adjusted already) are read as they are, and a store that serves no splits adjusts
//! nothing, so every run that names no split's series is what it was.

use std::sync::Arc;

use vike_data::store::store_kind::USER_VENUE;
use vike_data::{HistoryStore, HistoryStoreError};
use vike_marketdata::Bar;
use vike_model::{Instrument, StockSplit};

use crate::HarnessError;
use crate::run::profile::ProfileData;
use crate::run::tick_replay::ProfileDataSeries;

/// What a run holds of the kept lists (`ProfileBacktest::kept`). `Default`: nothing asked, nothing warned.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct KeptRows {
    /// The kept row of each of the run's series that has one, and every row of a venue whose row can name a session
    /// class (`vike_catalog::session_venue`), because a strategy's inputs on such a venue are named only by the built
    /// strategy. In the lists' order.
    pub rows: Vec<Instrument>,
    /// The run's sentences about what the rows do not cover: added to its data warnings (`crate::run::data_plan::enforce`).
    pub warnings: Vec<String>,
    /// The operator's share splits, every series' (an input's series is named only by the built strategy): what
    /// [`Self::adjust_for_splits`] reads. Empty: nothing is adjusted.
    pub splits: Vec<StockSplit>,
    /// Every kept row the store served, every venue's (an input's venue is named only by the built strategy): what
    /// [`Self::store_name`] reads a typed name against. `None`: no list was served, and a name is read as typed.
    pub lists: Option<Arc<Vec<Instrument>>>,
}

impl KeptRows {
    /// **Ask `store` for the kept lists and keep what a run of `series` (`(venue, symbol)`) reads** ([`Self::of`]),
    /// and for the operator's share splits ([`Self::with_splits`]): of `venues` alone
    /// (`crate::run::data_plan::kept_venues`), every venue's for `None`. A store that serves none is
    /// [`Self::default`]; one that fails is no rows and the failure as the warning.
    pub fn read(
        store: &dyn HistoryStore,
        series: &[(String, String)],
        venues: Option<&[String]>,
    ) -> Self {
        let (answer, splits) = match venues {
            Some(venues) => store.kept_instruments_and_splits_of(venues),
            None => store.kept_instruments_and_splits(),
        };
        let needing = Self::needing_rows(store, answer.as_ref().map(Option::as_deref), series);
        Self::answered(answer, &needing).with_splits(splits.as_ref().map(Option::as_deref))
    }

    /// **`series` without the ones whose facts the store RECORDED beside their rows and no kept row names** (PM-2):
    /// a recorded polymarket outcome token's facts row (`kind=properties`, classed a prediction market, written by
    /// the datahub's recorder) is that token's row for a run, and an outcome token's kept row would add nothing a
    /// reader here asks (its quote is the venue's USDC, it has no face and no fee lane), so such a series is not named
    /// missing. Rows recorded before the facts were have none, and stay named. A store that serves no kept list, or
    /// fails to, keeps every series.
    pub fn needing_rows(
        store: &dyn HistoryStore,
        answer: Result<Option<&[Instrument]>, &HistoryStoreError>,
        series: &[(String, String)],
    ) -> Vec<(String, String)> {
        let Ok(Some(all)) = answer else { return series.to_vec() };
        let recorded = |venue: &str, symbol: &str| {
            store.properties_as_of(venue, symbol, i64::MAX).ok().flatten().is_some_and(|facts| {
                facts.asset_class == Some(vike_model::AssetClass::PredictionMarket)
            })
        };
        series
            .iter()
            .filter(|(v, s)| {
                vike_catalog::kept_row_of_store_name(all, v, s).is_ok() || !recorded(v, s)
            })
            .cloned()
            .collect()
    }

    /// These rows with the store's share splits (`vike_data::HistoryStore::stock_splits`'s answer): all of them, a store
    /// that serves none leaves none, and one that fails leaves none with the failure as a warning.
    #[must_use]
    pub fn with_splits(
        mut self,
        answer: Result<Option<&[StockSplit]>, &HistoryStoreError>,
    ) -> Self {
        match answer {
            Ok(None) => {}
            Ok(Some(all)) => self.splits = all.to_vec(),
            Err(e) => self.warnings.push(format!(
                "the operator's share splits could not be read ({e}), so this run reads every series \
                 unadjusted"
            )),
        }
        self
    }

    /// **`bars` of `symbol` on `venue`, split-adjusted in place**: each bar earlier than a split of the series
    /// ([`StockSplit::effective_ms`], 00:00 UTC of its date) has its open, high, low, close, bid and ask divided by
    /// the product of those splits' ratios and its volume multiplied by it. The answer is the report line of each split
    /// that moved a bar (`split-adjusted: AAPLUSUSD 2020-08-31 ×4`). No split of the series changes nothing.
    ///
    /// # Errors
    /// [`HarnessError::Data`] for a split of the series that is no split (a row read back that does not hold): the
    /// run is refused rather than run on prices the operator said are not the series'.
    pub fn adjust_for_splits(
        &self,
        venue: &str,
        symbol: &str,
        bars: &mut [Bar],
    ) -> Result<Vec<String>, HarnessError> {
        let splits = self.splits_of(venue, symbol)?;
        if splits.is_empty() {
            return Ok(Vec::new());
        }
        for bar in bars.iter_mut() {
            let factor: f64 =
                splits.iter().filter(|(at, _)| bar.timestamp < *at).map(|(_, s)| s.ratio).product();
            if factor != 1.0 {
                bar.open /= factor;
                bar.high /= factor;
                bar.low /= factor;
                bar.close /= factor;
                bar.bid = bar.bid.map(|p| p / factor);
                bar.ask = bar.ask.map(|p| p / factor);
                bar.volume *= factor;
            }
        }
        let first = bars.first().map(|b| b.timestamp);
        Ok(splits
            .iter()
            .filter(|(at, _)| first.is_some_and(|t| t < *at))
            .map(|(_, s)| format!("split-adjusted: {} {} ×{}", s.symbol, s.date, s.ratio))
            .collect())
    }

    /// The splits of `symbol` on `venue`, each with the instant it takes effect.
    ///
    /// # Errors
    /// [`HarnessError::Data`] naming a split of the series that is no split.
    pub(crate) fn splits_of(
        &self,
        venue: &str,
        symbol: &str,
    ) -> Result<Vec<(i64, &StockSplit)>, HarnessError> {
        self.splits
            .iter()
            .filter(|s| s.venue == venue && s.symbol == symbol)
            .map(|s| {
                s.effective_ms().map(|at| (at, s)).map_err(|why| {
                    HarnessError::Data(format!(
                        "the split of {symbol} on {venue} dated {:?} is no split ({why}), so the run is refused \
                         rather than run unadjusted: correct it with `vike-cli data splits`",
                        s.date
                    ))
                })
            })
            .collect()
    }

    /// [`Self::read`] over the store's answer already in hand (`vike_data::HistoryStore::kept_instruments`'s), its
    /// rows MOVED into [`Self::lists`], never copied (a datahub serves ~24k of them).
    pub fn answered(
        answer: Result<Option<Vec<Instrument>>, HistoryStoreError>,
        series: &[(String, String)],
    ) -> Self {
        match answer {
            Ok(None) => Self::default(),
            Ok(Some(all)) => Self::of_owned(all, series),
            Err(e) => Self {
                warnings: vec![format!(
                    "the datahub's instrument lists could not be read ({e}), so this run reads every \
                     symbol's fiat quote and coin-margined face off its name and every input's market \
                     class is unknown"
                )],
                ..Self::default()
            },
        }
    }

    /// The rows of `all` a run of `series` (each by its STORE name) reads ([`Self::rows`]), `all` as
    /// [`Self::lists`], and one warning naming each series none of them is the row of
    /// (`vike_catalog::kept_row_of_store_name` over the kept rows, the lookup every reader makes: `AAPL.CFD` for
    /// dukascopy's `AAPL.US/USD`).
    #[must_use]
    pub fn of(all: &[Instrument], series: &[(String, String)]) -> Self {
        Self::of_owned(all.to_vec(), series)
    }

    /// [`Self::of`] over rows the run owns, moved into [`Self::lists`].
    fn of_owned(all: Vec<Instrument>, series: &[(String, String)]) -> Self {
        let found: Vec<&Instrument> = series
            .iter()
            .filter_map(|(v, s)| vike_catalog::kept_row_of_store_name(&all, v, s).ok())
            .collect();
        let rows: Vec<Instrument> = all
            .iter()
            .filter(|r| {
                vike_catalog::session_venue(&r.venue) || found.iter().any(|f| std::ptr::eq(*f, *r))
            })
            .cloned()
            .collect();
        let mut missing: Vec<String> = Vec::new();
        for (venue, symbol) in series {
            let named = format!("{symbol} on {venue}");
            if vike_catalog::kept_row_of_store_name(&rows, venue, symbol).is_err()
                && !missing.contains(&named)
            {
                missing.push(named);
            }
        }
        let warnings = if missing.is_empty() {
            Vec::new()
        } else {
            vec![format!(
                "no kept instrument row for {} (the datahub's <project>/instruments/<venue>.json lists \
                 none), so this run reads its fiat quote, its fee lane and whether it is coin-margined \
                 off its name (a broker's name that states no currency is refused)",
                missing.join(", ")
            )]
        };
        Self { rows, warnings, splits: Vec::new(), lists: Some(Arc::new(all)) }
    }

    /// The kept row of the STORE name `symbol` on `venue` among [`Self::rows`] (`vike_catalog::kept_row_of_store_name`).
    #[must_use]
    pub fn row(&self, venue: &str, symbol: &str) -> Option<&Instrument> {
        vike_catalog::kept_row_of_store_name(&self.rows, venue, symbol).ok()
    }

    /// **The STORE name a typed `name` of `venue` reads** (decision 0290; the module doc's "The names"): the
    /// store name of the one row of [`Self::lists`] whose user name it is, in any ASCII case, or whose store name it
    /// is exactly (a series the store lists, as the Studio's slice picker names it: a builder-dex `XYZ-TSLAUSDC.P`);
    /// a polymarket token id and a user-venue name as typed; every name as typed where no list was served.
    ///
    /// # Errors
    /// The translator's sentence: no list of `venue`, no row the name names, or two. A row the grammar names no
    /// instrument of (deribit's coin-settled option, which its door refuses) answers in that row's own words.
    pub fn store_name(&self, venue: &str, name: &str) -> Result<String, String> {
        store_name_in(self.lists.as_deref().map(Vec::as_slice), venue, name)
    }

    /// The fiat quote of `symbol` on `venue`: its kept row's where the row states a fiat one, else its name's;
    /// `vike_model::FiatQuote::Unknown` for a broker's symbol of no known currency and `Minor` for a row quoted in a
    /// minor unit (`vike_catalog::kept_fiat_quote`).
    #[must_use]
    pub fn fiat_quote(&self, venue: &str, symbol: &str) -> Option<vike_model::FiatQuote> {
        vike_catalog::kept_fiat_quote(venue, symbol, self.row(venue, symbol))
    }

    /// **Each of `series`' tick from its CURRENT kept row**, under the slot the strategy trades it as
    /// (`ProfileDataSeries::leg_name`): what the engine answers a strategy's tick read with where the run's `[risk]`
    /// states none and no dated properties record covers the bar (`vike_simulator::StrategyEngineConfig::kept_ticks`,
    /// the owner's ruling of 2026-11-19). The row's TICK alone: its lot, minimum quantity and minimum notional are
    /// not read. A series no kept row covers, or whose row states no tick, has none. Both `StrategyEngineConfig`
    /// sites of `crate::run::dispatch` read it.
    #[must_use]
    pub fn ticks(&self, series: &[ProfileDataSeries]) -> Vec<(String, f64)> {
        let tick = |s: &ProfileDataSeries| {
            let tick = self.row(&s.venue, &s.symbol)?.properties.tick_size;
            (tick > 0.0).then(|| (s.leg_name(), tick))
        };
        series.iter().filter_map(tick).collect()
    }
}

/// [`KeptRows::store_name`] against `lists`, the store's kept rows (`None`: none served, `name` as typed).
fn store_name_in(lists: Option<&[Instrument]>, venue: &str, name: &str) -> Result<String, String> {
    let Some(all) = lists else { return Ok(name.to_string()) };
    if venue == USER_VENUE {
        return Ok(name.to_string());
    }
    if venue == "polymarket" {
        return if vike_catalog::is_outcome_token(name) {
            Ok(name.to_string())
        } else {
            Err(format!(
                "`{name}` is no polymarket outcome token id (20 or more digits): a polymarket series \
                 is filed under its outcome's token id, the one name of the position (decision 0290)"
            ))
        };
    }
    vike_catalog::names_of_user_name(all, venue, name)
        .or_else(|why| vike_catalog::names_of_store_name(all, venue, name.trim()).map_err(|_| why))
        .map(|names| vike_paths::store_path::store_name_of_user_name(&names.user))
        .map_err(|why| {
            // The venue's own spelling of a row is no user name: say the row's, or why the row has none.
            let spelled = all.iter().find(|r| r.venue == venue && r.raw_symbol == name.trim());
            match spelled.map(|row| (row, vike_catalog::user_name_of_row(row))) {
                Some((row, Ok(user))) => {
                    format!("{why}; {venue}'s own spelling `{}` is named `{user}`", row.raw_symbol)
                }
                Some((_, Err(own))) => own,
                None => why,
            }
        })
}

/// **A profile's symbols read as decision 0290 reads a name** (the module doc's "The names"): each of `data`'s
/// becomes its STORE name against `lists`, the store's kept rows (`None`: as typed). `ProfileBacktest::read_kept_rows`
/// calls it once, before the run fans out, so the run reads, plans, addresses and reports the store's series.
///
/// # Errors
/// [`HarnessError::Data`] naming the first symbol its venue's kept list names no instrument by.
pub(crate) fn read_store_names(
    data: &mut ProfileData,
    lists: Option<&[Instrument]>,
) -> Result<(), HarnessError> {
    let read = |venue: &str, symbol: &mut String| {
        *symbol = store_name_in(lists, venue, symbol).map_err(|why| {
            HarnessError::Data(format!(
                "data symbol {symbol:?} on {venue} is refused before the run: {why}"
            ))
        })?;
        Ok::<(), HarnessError>(())
    };
    let venue = data.venue.clone().unwrap_or_default();
    for symbol in &mut data.symbols {
        read(&venue, symbol)?;
    }
    for series in &mut data.series {
        read(&series.venue, &mut series.symbol)?;
    }
    Ok(())
}

#[cfg(test)]
#[path = "kept_rows_tests.rs"]
mod kept_rows_tests;
