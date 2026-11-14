//! **The KEPT instrument rows a run reads its per-symbol facts from**: the datahub's lists
//! (`<project>/instruments/<venue>.json`, which only the datahub reads and writes:
//! `docs/decisions/0084-only-the-datahub-touches-the-store.md`), asked ONCE per run by the door that resolves the
//! profile (`ProfileBacktest::read_kept_rows`) through the run's own store (`vike_data::HistoryStore::kept_instruments`,
//! the datahub's `Request::InstrumentLists`). Three readers:
//!
//! - an input's coverage: its market's session class (`crate::data_plan`'s trading calendar over
//!   `vike_catalog::trading_calendar`);
//! - the ledger guard: a symbol's fiat quote (`vike_catalog::kept_fiat_quote`);
//! - the coin ledger: an inverse contract's face where only its row states it (`vike_catalog::kept_contract_value`).
//!
//! A series with no kept row keeps today's answer for each reader (class unknown, the quote and the face its name
//! gives) and the run's data warnings name it. A store that serves no kept list (`Ok(None)`: a test's store, the
//! lab's, anything but the datahub's handle) hands nothing and warns nothing, so such a run is what it was before the
//! lists reached a backtest.
//!
//! A sandboxed child is handed [`KeptRows::rows`] in its start message and its piped store answers them back
//! (`crates/vike-backtesthub/src/strategy_sandbox/pipe_store.rs`'s `PipeHistStore`): [`KeptRows::of`] over its own
//! rows is those rows again, so the child reads every fact the parent would.

use vike_data::{HistoryStore, HistoryStoreError};
use vike_model::Instrument;

/// What a run holds of the kept lists (`ProfileBacktest::kept`). `Default`: nothing asked, nothing warned.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct KeptRows {
    /// The kept row of each of the run's series that has one, and every row of a venue whose row can name a session
    /// class (`vike_catalog::session_venue`), because a strategy's inputs on such a venue are named only by the built
    /// strategy. In the lists' order.
    pub rows: Vec<Instrument>,
    /// The run's sentences about what the rows do not cover: added to its data warnings (`crate::data_plan::enforce`).
    pub warnings: Vec<String>,
}

impl KeptRows {
    /// **Ask `store` for the kept lists and keep what a run of `series` (`(venue, symbol)`) reads** ([`Self::of`]).
    /// A store that serves none is [`Self::default`]; one that fails is no rows and the failure as the warning.
    pub fn read(store: &dyn HistoryStore, series: &[(String, String)]) -> Self {
        Self::answered(store.kept_instruments().as_ref().map(Option::as_deref), series)
    }

    /// [`Self::read`] over the store's answer already in hand (`vike_data::HistoryStore::kept_instruments`'s): what
    /// a host asks once for many runs reads each run's rows from (a study's sims).
    pub fn answered(
        answer: Result<Option<&[Instrument]>, &HistoryStoreError>,
        series: &[(String, String)],
    ) -> Self {
        match answer {
            Ok(None) => Self::default(),
            Ok(Some(all)) => Self::of(all, series),
            Err(e) => Self {
                rows: Vec::new(),
                warnings: vec![format!(
                    "the datahub's instrument lists could not be read ({e}), so this run reads every \
                     symbol's fiat quote and coin-margined face off its name and every input's market \
                     class is unknown"
                )],
            },
        }
    }

    /// The rows of `all` a run of `series` reads ([`Self::rows`]), and one warning naming each series none of them
    /// is the row of (`vike_catalog::kept_row` over the kept rows, the lookup every reader makes).
    #[must_use]
    pub fn of(all: &[Instrument], series: &[(String, String)]) -> Self {
        let found: Vec<&Instrument> =
            series.iter().filter_map(|(v, s)| vike_catalog::kept_row(all, v, s)).collect();
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
            if vike_catalog::kept_row(&rows, venue, symbol).is_none() && !missing.contains(&named) {
                missing.push(named);
            }
        }
        let warnings = if missing.is_empty() {
            Vec::new()
        } else {
            vec![format!(
                "no kept instrument row for {} (the datahub's <project>/instruments/<venue>.json lists \
                 none), so this run reads its fiat quote and, for a coin-margined contract, its face and, \
                 on ctrader, its fee lane off its name",
                missing.join(", ")
            )]
        };
        Self { rows, warnings }
    }

    /// The kept row of `symbol` on `venue` among [`Self::rows`] (`vike_catalog::kept_row`).
    #[must_use]
    pub fn row(&self, venue: &str, symbol: &str) -> Option<&Instrument> {
        vike_catalog::kept_row(&self.rows, venue, symbol)
    }

    /// The fiat quote of `symbol` on `venue`: its kept row's where the row states a fiat one, else its name's
    /// (`vike_catalog::kept_fiat_quote`).
    #[must_use]
    pub fn fiat_quote(&self, venue: &str, symbol: &str) -> Option<&'static str> {
        vike_catalog::kept_fiat_quote(symbol, self.row(venue, symbol))
    }
}
