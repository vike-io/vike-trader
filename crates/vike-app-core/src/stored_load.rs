//! The Stored grid's inventory WALK, spelled over the `HistStore` TRAIT — the load half of the
//! #1378 seam close (see [`crate::stored_mode`] for the decision half).
//!
//! One function walks a store's catalog into the Data-Manager's render inputs: `inventory()` →
//! [`build_tree`], then one `series_gaps` probe per listed series into a [`GapMap`] (cheap —
//! manifest-only on both backends, no Parquet scan; over the wire it is one small RPC per series).
//! `vike-app`'s `refresh_stored` calls it with a LOCAL `DataFusionHist` or a REMOTE
//! `RemoteHistStore` depending on [`crate::stored_mode::stored_mode`] — the same walk either way,
//! so the two modes cannot drift in tree shape, and the walk itself is CI-tested here against a
//! seeded trait-store (which `main.rs`, compile-checked only, could never give it).
//!
//! Degrade contract (moved verbatim from `refresh_stored`'s worker): an inventory failure is an
//! `Err` the caller logs and renders as an empty tree; a PER-SERIES gap-probe failure skips that
//! one series' entry (logged here) rather than failing the whole refresh.
//!
//! The cross-kind partial-day map is a SECOND fold ([`load_partials`]) rather than part of this
//! walk, but it is now spelled over the trait exactly like this one is: `coverage_report` used to
//! be a concrete `DataFusionHist` method that only a local caller could reach, and spec §6-Q2
//! promoted it onto the trait with a wire verb behind it, so BOTH callers run the same fold over
//! whichever store they hold. It stays a separate function because its failure mode is separate:
//! an unanswerable coverage report degrades to a rendered NOTE
//! ([`crate::stored_mode::PARTIALS_UNSERVED`]) while the tree still renders, whereas an
//! unanswerable inventory means there is no grid at all.

use crate::inventory::{build_tree, VenueNode};
use vike_data::HistStore;
use vike_data_manager::{partial_days_from_coverage, GapMap, PartialDayMap};

/// Walk `store`'s catalog through the TRAIT verbs into the Stored grid's
/// `(tree, per-series gap map)` — see the module doc for the shared-walk argument and the degrade
/// contract. A gap-free series gets NO map entry (an absent key already reads as "no known gaps").
pub fn load_stored_tree(store: &dyn HistStore) -> Result<(Vec<VenueNode>, GapMap), String> {
    let inv = store.inventory().map_err(|e| format!("inventory scan: {e}"))?;
    let mut gaps = GapMap::new();
    for (id, _cov) in &inv {
        match store.series_gaps(id) {
            Ok(ranges) if !ranges.is_empty() => {
                let key = vike_data_manager::SeriesKey {
                    venue: id.venue.clone(),
                    symbol: id.symbol.clone(),
                    kind: id.kind.clone(),
                    interval: id.interval.clone(),
                };
                gaps.insert(key, ranges);
            }
            Ok(_) => {} // no gaps -> omit (an absent key reads as "no known gaps")
            Err(e) => tracing::warn!(
                "Stored inventory: series_gaps({}/{}/{}) failed: {e}",
                id.venue,
                id.symbol,
                id.kind
            ),
        }
    }
    Ok((build_tree(inv), gaps))
}

/// The Stored grid's cross-kind PARTIAL-day map, walked through the TRAIT verb — the §6-Q2 sibling
/// of [`load_stored_tree`], and the one fold BOTH the local and the remote arm now run.
///
/// `store.coverage_report()` is a manifest fold on either backend (no Parquet scan, one small RPC
/// over the wire), and [`partial_days_from_coverage`] is the same pure rendering step in both
/// cases — which is exactly the property worth having: the map a remote grid draws is folded from
/// the same value type, by the same function, as the map a local grid draws. There is no second
/// shape for the two modes to drift apart in.
///
/// An `Err` means the store could not answer AT ALL — over the wire, that is the negotiation
/// refusal a server older than the verb produces (`RemoteHistStore` surfaces it rather than
/// inheriting the trait's empty default, precisely so this stays distinguishable). The caller
/// renders [`crate::stored_mode::PARTIALS_UNSERVED`] for it and keeps the rest of the grid; it
/// must NOT substitute an empty map, which would claim "nothing is partial".
pub fn load_partials(store: &dyn HistStore) -> Result<PartialDayMap, String> {
    let report = store.coverage_report().map_err(|e| format!("coverage report: {e}"))?;
    Ok(partial_days_from_coverage(&report))
}

#[cfg(test)]
mod tests {
    use super::*;
    use vike_data::{DataError, ExecFillRow, ExecOrderRow, SeriesCoverage, SeriesId, TsRange};
    use vike_data_manager::SeriesKey;
    use vike_model::{Bar, BookUpdate, EquitySample, QuoteTick, SymbolProperties, TradeTick};

    /// A seeded catalog-only `HistStore` double: answers the two verbs the walk uses
    /// (`inventory`, `series_gaps`); every other required verb is unreachable and says so.
    struct FakeCatalogStore {
        inv: Result<Vec<(SeriesId, SeriesCoverage)>, String>,
        gaps: Vec<(SeriesId, Vec<(i64, i64)>)>,
        /// A planted per-series `series_gaps` failure (the degrade-contract probe).
        failing_gap_probe: Option<SeriesId>,
        /// The §6-Q2 cross-kind report this store answers with. `Err` stands in for BOTH ways a
        /// store cannot answer: a `RemoteHistStore` whose peer predates the verb (refused
        /// client-side by the capability check) and a read that failed outright.
        coverage: Result<Vec<vike_data::InstrumentCoverage>, String>,
    }

    impl Default for FakeCatalogStore {
        fn default() -> Self {
            Self {
                inv: Ok(Vec::new()),
                gaps: Vec::new(),
                failing_gap_probe: None,
                coverage: Ok(Vec::new()),
            }
        }
    }

    fn off_walk(verb: &str) -> DataError {
        DataError::Query(format!("FakeCatalogStore: {verb} is not part of the stored walk"))
    }

    impl HistStore for FakeCatalogStore {
        fn inventory(&self) -> Result<Vec<(SeriesId, SeriesCoverage)>, DataError> {
            self.inv.clone().map_err(DataError::Query)
        }
        fn series_gaps(&self, id: &SeriesId) -> Result<Vec<(i64, i64)>, DataError> {
            if self.failing_gap_probe.as_ref() == Some(id) {
                return Err(DataError::Query("planted gap-probe failure".into()));
            }
            Ok(self
                .gaps
                .iter()
                .find(|(gid, _)| gid == id)
                .map(|(_, ranges)| ranges.clone())
                .unwrap_or_default())
        }
        fn coverage_report(&self) -> Result<Vec<vike_data::InstrumentCoverage>, DataError> {
            self.coverage.clone().map_err(DataError::Query)
        }

        // ---- everything below is unreachable for the walk ----
        fn load_bars(
            &self,
            _v: &str,
            _s: &str,
            _i: &str,
            _r: TsRange,
        ) -> Result<Vec<Bar>, DataError> {
            Err(off_walk("load_bars"))
        }
        fn scan_quotes(
            &self,
            _v: &str,
            _s: &str,
            _r: TsRange,
        ) -> Result<Vec<QuoteTick>, DataError> {
            Err(off_walk("scan_quotes"))
        }
        fn scan_trades(
            &self,
            _v: &str,
            _s: &str,
            _r: TsRange,
        ) -> Result<Vec<TradeTick>, DataError> {
            Err(off_walk("scan_trades"))
        }
        fn append_bars(
            &self,
            _v: &str,
            _s: &str,
            _i: &str,
            _b: &[Bar],
            _k: Option<&str>,
        ) -> Result<usize, DataError> {
            Err(off_walk("append_bars"))
        }
        fn append_quotes(
            &self,
            _v: &str,
            _s: &str,
            _t: &[QuoteTick],
            _k: Option<&str>,
        ) -> Result<usize, DataError> {
            Err(off_walk("append_quotes"))
        }
        fn append_trades(
            &self,
            _v: &str,
            _s: &str,
            _t: &[TradeTick],
            _k: Option<&str>,
        ) -> Result<usize, DataError> {
            Err(off_walk("append_trades"))
        }
        fn append_book_updates(
            &self,
            _v: &str,
            _s: &str,
            _u: &[BookUpdate],
            _k: Option<&str>,
        ) -> Result<usize, DataError> {
            Err(off_walk("append_book_updates"))
        }
        fn scan_book_updates(
            &self,
            _v: &str,
            _s: &str,
            _r: TsRange,
        ) -> Result<Vec<BookUpdate>, DataError> {
            Err(off_walk("scan_book_updates"))
        }
        fn append_symbol_properties(
            &self,
            _v: &str,
            _s: &str,
            _rows: &[(i64, SymbolProperties)],
            _k: Option<&str>,
        ) -> Result<usize, DataError> {
            Err(off_walk("append_symbol_properties"))
        }
        fn scan_symbol_properties(
            &self,
            _v: &str,
            _s: &str,
            _r: TsRange,
        ) -> Result<Vec<(i64, SymbolProperties)>, DataError> {
            Err(off_walk("scan_symbol_properties"))
        }
        fn append_equity(
            &self,
            _v: &str,
            _s: &str,
            _rows: &[EquitySample],
            _k: Option<&str>,
        ) -> Result<usize, DataError> {
            Err(off_walk("append_equity"))
        }
        fn scan_equity(
            &self,
            _v: &str,
            _s: &str,
            _r: TsRange,
        ) -> Result<Vec<EquitySample>, DataError> {
            Err(off_walk("scan_equity"))
        }
        fn append_exec_fills(
            &self,
            _v: &str,
            _s: &str,
            _rows: &[ExecFillRow],
            _k: Option<&str>,
        ) -> Result<usize, DataError> {
            Err(off_walk("append_exec_fills"))
        }
        fn scan_exec_fills(&self, _v: &str, _s: &str) -> Result<Vec<ExecFillRow>, DataError> {
            Err(off_walk("scan_exec_fills"))
        }
        fn append_exec_orders(
            &self,
            _v: &str,
            _s: &str,
            _rows: &[ExecOrderRow],
            _k: Option<&str>,
        ) -> Result<usize, DataError> {
            Err(off_walk("append_exec_orders"))
        }
        fn scan_exec_orders(&self, _v: &str, _s: &str) -> Result<Vec<ExecOrderRow>, DataError> {
            Err(off_walk("scan_exec_orders"))
        }
        fn resample_quotes_to_bars(
            &self,
            _v: &str,
            _s: &str,
            _i: &str,
            _r: TsRange,
            _k: Option<&str>,
        ) -> Result<usize, DataError> {
            Err(off_walk("resample_quotes_to_bars"))
        }
        fn resample_trades_to_bars(
            &self,
            _v: &str,
            _s: &str,
            _i: &str,
            _r: TsRange,
            _k: Option<&str>,
        ) -> Result<usize, DataError> {
            Err(off_walk("resample_trades_to_bars"))
        }
    }

    fn sid(kind: &str, venue: &str, sym: &str, iv: Option<&str>) -> SeriesId {
        SeriesId::per_symbol(kind, venue, sym, iv.map(Into::into))
    }
    fn cov(rows: u64, bytes: u64, a: i64, b: i64) -> SeriesCoverage {
        SeriesCoverage { first_ts: a, last_ts: b, rows, bytes, parts: 1, dates: 1 }
    }
    fn key(kind: &str, venue: &str, sym: &str, iv: Option<&str>) -> SeriesKey {
        SeriesKey {
            venue: venue.into(),
            symbol: sym.into(),
            kind: kind.into(),
            interval: iv.map(Into::into),
        }
    }

    fn small_fixture() -> Vec<(SeriesId, SeriesCoverage)> {
        vec![
            (sid("bar", "binance", "BTCUSDT", Some("1m")), cov(10, 100, 1_000, 2_000)),
            (sid("trade", "binance", "BTCUSDT", None), cov(5, 50, 1_000, 1_500)),
            (sid("bar", "okx", "ETH-USDT", Some("5m")), cov(7, 70, 1_100, 2_100)),
        ]
    }

    /// THE equality pin of the seam close: walking a seeded TRAIT store yields byte-identically
    /// the `VenueNode` tree the local arm's fold (`build_tree` over the same inventory) yields —
    /// so a remote grid renders exactly what a local grid over the same data renders.
    #[test]
    fn the_trait_walk_yields_the_tree_the_local_fold_yields_for_identical_data() {
        let inv = small_fixture();
        let store = FakeCatalogStore {
            inv: Ok(inv.clone()),
            gaps: vec![(sid("bar", "okx", "ETH-USDT", Some("5m")), vec![(1_200, 1_300)])],
            failing_gap_probe: None,
            coverage: Ok(Vec::new()),
        };
        let (tree, gaps) = load_stored_tree(&store).expect("seeded walk");
        assert_eq!(tree, build_tree(inv), "remote walk and local fold must agree on the tree");
        assert_eq!(gaps.len(), 1, "exactly the one gappy series gets an entry");
        assert_eq!(gaps[&key("bar", "okx", "ETH-USDT", Some("5m"))], vec![(1_200, 1_300)]);
    }

    /// An absent key already reads as "no known gaps" downstream, so a gap-free series must not
    /// insert an empty entry (byte-preserves `refresh_stored`'s original omit-empty behavior).
    #[test]
    fn a_gap_free_series_gets_no_gap_map_entry() {
        let store = FakeCatalogStore {
            inv: Ok(small_fixture()),
            gaps: Vec::new(),
            failing_gap_probe: None,
            coverage: Ok(Vec::new()),
        };
        let (tree, gaps) = load_stored_tree(&store).expect("seeded walk");
        assert_eq!(tree.len(), 2, "both venues present");
        assert!(gaps.is_empty(), "no gaps anywhere ⇒ an empty map, not empty entries");
    }

    /// The degrade contract: one series' failing gap probe skips THAT entry, never the refresh —
    /// the tree stays complete and every other series' gaps still land.
    #[test]
    fn a_failing_gap_probe_skips_that_series_never_the_walk() {
        let inv = small_fixture();
        let store = FakeCatalogStore {
            inv: Ok(inv.clone()),
            gaps: vec![
                (sid("bar", "binance", "BTCUSDT", Some("1m")), vec![(1_400, 1_600)]),
                (sid("bar", "okx", "ETH-USDT", Some("5m")), vec![(1_200, 1_300)]),
            ],
            failing_gap_probe: Some(sid("bar", "okx", "ETH-USDT", Some("5m"))),
            coverage: Ok(Vec::new()),
        };
        let (tree, gaps) = load_stored_tree(&store).expect("a per-series failure is not fatal");
        assert_eq!(tree, build_tree(inv), "the tree survives a gap-probe failure whole");
        assert_eq!(gaps.len(), 1, "only the healthy series' gaps land");
        assert_eq!(gaps[&key("bar", "binance", "BTCUSDT", Some("1m"))], vec![(1_400, 1_600)]);
    }

    /// An inventory failure IS fatal to the load (there is nothing to render) — surfaced as `Err`
    /// naming the cause, which the caller logs and renders as an empty tree.
    #[test]
    fn an_inventory_failure_is_an_err_naming_the_cause() {
        let store = FakeCatalogStore {
            inv: Err("planted inventory failure".into()),
            gaps: Vec::new(),
            failing_gap_probe: None,
            coverage: Ok(Vec::new()),
        };
        let err = load_stored_tree(&store).expect_err("no inventory ⇒ no load");
        assert!(err.contains("planted inventory failure"), "the cause must survive: {err}");
    }

    // ---- the §6-Q2 partial-day fold ------------------------------------------------------------

    /// One instrument recording BOTH trade and quote, where day 2 has trades and no quotes — the
    /// smallest shape that makes `partial_days` non-empty (a report with one recorded kind can
    /// never disagree with itself, so a single-kind fixture would pass vacuously).
    fn partial_fixture() -> Vec<vike_data::InstrumentCoverage> {
        vike_data::coverage::join_coverage(&[
            (sid("trade", "binance", "BTCUSDT", None), vec![0, 1, 2]),
            (sid("quote", "binance", "BTCUSDT", None), vec![0, 1]),
        ])
    }

    /// The fold a REMOTE grid runs is the fold a LOCAL grid runs: `load_partials` over a store that
    /// answers the trait verb equals `partial_days_from_coverage` over the same report. This is the
    /// app-core half of the wire-adds-nothing property the composed datahub test proves end to end.
    #[test]
    fn the_partial_fold_equals_the_direct_fold_over_the_same_report() {
        let report = partial_fixture();
        let store = FakeCatalogStore { coverage: Ok(report.clone()), ..Default::default() };
        let folded = load_partials(&store).expect("a store that answers coverage");
        assert_eq!(folded, partial_days_from_coverage(&report), "one fold, whichever store");
        assert!(!folded.is_empty(), "the fixture must actually produce a partial day");
    }

    /// A store that cannot answer is an `Err`, NOT an empty map. The distinction is the whole
    /// honesty of the column: an empty map renders as "nothing is partial", which is a claim, while
    /// the `Err` is what the caller turns into `stored_mode::PARTIALS_UNSERVED`.
    #[test]
    fn an_unanswerable_coverage_report_is_an_err_not_an_empty_map() {
        let store = FakeCatalogStore {
            coverage: Err("does not advertise `coverage`".into()),
            ..Default::default()
        };
        let err = load_partials(&store).expect_err("an unanswerable report must not fold to empty");
        assert!(err.contains("coverage"), "the cause must survive for the log line: {err}");
    }

    /// A store that answers with an EMPTY report is a different fact from one that cannot answer,
    /// and must stay one: it folds to an empty map through `Ok`, so the column renders blank
    /// (correctly — nothing is partial) instead of showing the unserved note.
    #[test]
    fn an_empty_report_folds_to_an_empty_map_through_ok() {
        let store = FakeCatalogStore::default();
        assert!(load_partials(&store).expect("an empty report is still an answer").is_empty());
    }
}
