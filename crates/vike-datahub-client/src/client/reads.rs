//! `DatahubClient`'s store-read verbs: the RPC twins of `HistStore`'s reads — bars, quotes, trades,
//! book updates, depth, cohort, perp metrics, equity, exec fills and point-in-time properties —
//! plus the store-metadata verbs (`list_series`, `inventory`, `series_facts`, `series_gaps`,
//! `coverage_report`).
//!
//! Split out of `client.rs`'s one `impl DatahubClient` by concern (behaviour byte-identical; the
//! methods moved verbatim). `use super::*` brings in the parent module's imports and items, so
//! nothing about resolution changes.

use super::*;

impl DatahubClient {
    /// Read derived OHLCV bars for `(venue, symbol, interval)` in `range` — the RPC twin of
    /// `HistStore::load_bars`. A transport failure or a server-side [`Response::Error`] surfaces
    /// through the ONE `Err(String)` channel.
    ///
    /// ⚠ A THIN WRAPPER over [`Self::load_bars_ms`] since `vike-cli` grew a row verb — the request
    /// is built in one place and the desync sentence is spelled once. See that method for why the
    /// second signature exists at all.
    pub fn load_bars(
        &mut self,
        venue: &str,
        symbol: &str,
        interval: &str,
        range: TsRange,
        limit: Option<u32>,
    ) -> Result<Vec<Bar>, String> {
        self.load_bars_ms(venue, symbol, interval, range.start, range.end, limit)
    }

    /// [`Self::load_bars`] with the range spelled as two EPOCH-MS bounds instead of a
    /// [`TsRange`] — same verb, same wire, no protocol arm.
    ///
    /// ⚠ **It exists for a TYPE WALL, not for convenience, and the wall is real.** `TsRange` is
    /// declared in `vike-data`, which `vike-cli` takes as a DEV-dependency only (that crate's
    /// `Cargo.toml` argues every edge, and CI's `light-consumers` lane checks the result), so no
    /// production path there can NAME the parameter [`Self::load_bars`] requires. The wall is a
    /// property of this SIGNATURE and not of the wire:
    /// [`Request::LoadBars`] already carries `start`/`end` as plain
    /// `Option<i64>`, "decomposed because `TsRange` is not serde in vike-data" — its own words. So
    /// this method closes the gap with no new variant, no version bump and no capability string,
    /// which is what lets `vike-cli data hist get` talk to a datahub that predates it.
    ///
    /// ⚠ **`None` on a bound is UNBOUNDED on that side** — `TsRange`'s own meaning, carried
    /// through unchanged. A caller that means "everything" passes two `None`s and gets the whole
    /// series; bounding the request is the CALLER's business, which is why
    /// `crates/vike-cli/src/cmd/data/hist/get.rs` requires a window before it ever reaches here.
    pub fn load_bars_ms(
        &mut self,
        venue: &str,
        symbol: &str,
        interval: &str,
        start: Option<i64>,
        end: Option<i64>,
        limit: Option<u32>,
    ) -> Result<Vec<Bar>, String> {
        let request = Request::LoadBars {
            venue: venue.to_string(),
            symbol: symbol.to_string(),
            interval: interval.to_string(),
            start,
            end,
            limit,
        };
        write_frame(&mut self.stream, &request).map_err(|e| e.to_string())?;
        match read_frame::<_, Response>(&mut self.stream).map_err(|e| e.to_string())? {
            Response::Bars(bars) => Ok(bars),
            Response::Error(msg) => Err(msg),
            other => Err(format!("protocol desync: expected Bars, got {}", resp_kind(&other))),
        }
    }

    /// Read L1 quotes for `(venue, symbol)` in `range` — the RPC twin of `HistStore::scan_quotes`.
    ///
    /// ⚠ A THIN WRAPPER over [`Self::scan_quotes_ms`] since `vike-cli` grew a remote `export`, for
    /// the reason [`Self::load_bars`] states one method up: the request is built in one place and
    /// the desync sentence is spelled once.
    pub fn scan_quotes(
        &mut self,
        venue: &str,
        symbol: &str,
        range: TsRange,
        limit: Option<u32>,
    ) -> Result<Vec<QuoteTick>, String> {
        self.scan_quotes_ms(venue, symbol, range.start, range.end, limit)
    }

    /// [`Self::scan_quotes`] with the range spelled as two EPOCH-MS bounds instead of a
    /// [`TsRange`] — same verb, same wire, no protocol arm.
    ///
    /// ⚠ **The TYPE WALL, not convenience** — [`Self::load_bars_ms`] carries the whole argument and
    /// this is its second instance: `TsRange` is declared in `vike-data`, which `vike-cli` takes as
    /// a DEV-dependency only, so no production path there can NAME the parameter
    /// [`Self::scan_quotes`] requires. [`Request::ScanQuotes`] already carries `start`/`end` as
    /// plain `Option<i64>`, so this closes the gap with no variant, no version bump and no
    /// capability string.
    ///
    /// ⚠ **`None` on a bound is UNBOUNDED on that side**, and this verb answers in ONE frame with
    /// no row cap — so an unbounded scan of a busy tick series is a frame `write_frame` refuses
    /// server-side against [`crate::proto::MAX_FRAME_LEN`]. Bounding the request is the CALLER's
    /// business: `crates/vike-cli/src/cmd/data/hist/export.rs` walks a range in fixed wall-clock
    /// windows for exactly this reason, and names the flag that lowers the step when a window is
    /// still too big.
    pub fn scan_quotes_ms(
        &mut self,
        venue: &str,
        symbol: &str,
        start: Option<i64>,
        end: Option<i64>,
        limit: Option<u32>,
    ) -> Result<Vec<QuoteTick>, String> {
        let request = Request::ScanQuotes {
            venue: venue.to_string(),
            symbol: symbol.to_string(),
            start,
            end,
            limit,
        };
        write_frame(&mut self.stream, &request).map_err(|e| e.to_string())?;
        match read_frame::<_, Response>(&mut self.stream).map_err(|e| e.to_string())? {
            Response::Quotes(quotes) => Ok(quotes),
            Response::Error(msg) => Err(msg),
            other => Err(format!("protocol desync: expected Quotes, got {}", resp_kind(&other))),
        }
    }

    /// Read executed trades for `(venue, symbol)` in `range` — the RPC twin of
    /// `HistStore::scan_trades`.
    ///
    /// ⚠ A THIN WRAPPER over [`Self::scan_trades_ms`], for [`Self::scan_quotes`]'s reason.
    pub fn scan_trades(
        &mut self,
        venue: &str,
        symbol: &str,
        range: TsRange,
        limit: Option<u32>,
    ) -> Result<Vec<TradeTick>, String> {
        self.scan_trades_ms(venue, symbol, range.start, range.end, limit)
    }

    /// [`Self::scan_trades`] with the range spelled as two EPOCH-MS bounds instead of a
    /// [`TsRange`] — same verb, same wire, no protocol arm. [`Self::scan_quotes_ms`] carries the
    /// type-wall argument and the unbounded-frame warning; both apply here unchanged, and the
    /// trade tape is the lane where a wide window overruns a frame SOONEST.
    pub fn scan_trades_ms(
        &mut self,
        venue: &str,
        symbol: &str,
        start: Option<i64>,
        end: Option<i64>,
        limit: Option<u32>,
    ) -> Result<Vec<TradeTick>, String> {
        let request = Request::ScanTrades {
            venue: venue.to_string(),
            symbol: symbol.to_string(),
            start,
            end,
            limit,
        };
        write_frame(&mut self.stream, &request).map_err(|e| e.to_string())?;
        match read_frame::<_, Response>(&mut self.stream).map_err(|e| e.to_string())? {
            Response::Trades(trades) => Ok(trades),
            Response::Error(msg) => Err(msg),
            other => Err(format!("protocol desync: expected Trades, got {}", resp_kind(&other))),
        }
    }

    /// Read raw L2 book updates for `(venue, symbol)` in `range` — the RPC twin of
    /// `HistStore::scan_book_updates`, and the first of the SIX verbs
    /// `docs/decisions/0084-only-the-datahub-touches-the-store.md` asks this wire to grow.
    ///
    /// ⚠ **No `_ms` twin, and that is a measured omission rather than an inconsistency.**
    /// [`Self::scan_quotes_ms`] exists because `vike-cli` takes `vike-data` as a DEV-dependency
    /// only, so no production path there can NAME a [`TsRange`] — a type wall, not a convenience.
    /// The consumers these six verbs exist for (`vike-backtest`, `vike-user-research`,
    /// `vike-report`) all take `vike-data` as a NORMAL dependency and already spell `TsRange` at
    /// every call site, so an epoch-ms twin here would be public surface nothing calls. Adding one
    /// later is a five-line wrapper over the same variant, needing no protocol arm.
    ///
    /// ⚠ [`Self::scan_quotes_ms`]'s UNBOUNDED-FRAME warning applies here unchanged and applies
    /// HARDEST: a lossless book lane outruns [`crate::proto::MAX_FRAME_LEN`] sooner than any other
    /// kind in the store. Bounding the request is the caller's business.
    pub fn scan_book_updates(
        &mut self,
        venue: &str,
        symbol: &str,
        range: TsRange,
        limit: Option<u32>,
    ) -> Result<Vec<BookUpdate>, String> {
        let request = Request::ScanBookUpdates {
            venue: venue.to_string(),
            symbol: symbol.to_string(),
            start: range.start,
            end: range.end,
            limit,
        };
        write_frame(&mut self.stream, &request).map_err(|e| e.to_string())?;
        match read_frame::<_, Response>(&mut self.stream).map_err(|e| e.to_string())? {
            Response::BookUpdates(rows) => Ok(rows),
            Response::Error(msg) => Err(msg),
            other => {
                Err(format!("protocol desync: expected BookUpdates, got {}", resp_kind(&other)))
            }
        }
    }

    /// Read the CONFLATING depth lane for `(venue, symbol)` in `range` — the RPC twin of
    /// `HistStore::scan_depth`.
    ///
    /// ⚠ It returns the same ROW TYPE as [`Self::scan_book_updates`] and is a different LANE: the
    /// two read different `kind=` partitions, and [`Request::ScanDepth`] argues why they do not
    /// share a reply variant. The desync arm below is what makes a mix-up loud instead of
    /// plausible.
    pub fn scan_depth(
        &mut self,
        venue: &str,
        symbol: &str,
        range: TsRange,
        limit: Option<u32>,
    ) -> Result<Vec<BookUpdate>, String> {
        let request = Request::ScanDepth {
            venue: venue.to_string(),
            symbol: symbol.to_string(),
            start: range.start,
            end: range.end,
            limit,
        };
        write_frame(&mut self.stream, &request).map_err(|e| e.to_string())?;
        match read_frame::<_, Response>(&mut self.stream).map_err(|e| e.to_string())? {
            Response::Depth(rows) => Ok(rows),
            Response::Error(msg) => Err(msg),
            other => Err(format!("protocol desync: expected Depth, got {}", resp_kind(&other))),
        }
    }

    /// Read cohort rows for `(venue, asset)` in `range` — the RPC twin of `HistStore::scan_cohort`.
    ///
    /// ⚠ The second argument is an **ASSET, not a symbol**, which is the trait's own spelling and
    /// the one place this family's signature shape breaks. See [`Request::ScanCohort`].
    pub fn scan_cohort(
        &mut self,
        venue: &str,
        asset: &str,
        range: TsRange,
        limit: Option<u32>,
    ) -> Result<Vec<CohortRow>, String> {
        let request = Request::ScanCohort {
            venue: venue.to_string(),
            asset: asset.to_string(),
            start: range.start,
            end: range.end,
            limit,
        };
        write_frame(&mut self.stream, &request).map_err(|e| e.to_string())?;
        match read_frame::<_, Response>(&mut self.stream).map_err(|e| e.to_string())? {
            Response::Cohort(rows) => Ok(rows),
            Response::Error(msg) => Err(msg),
            other => Err(format!("protocol desync: expected Cohort, got {}", resp_kind(&other))),
        }
    }

    /// Read perpetual-swap metric rows for `(venue, symbol)` in `range` — the RPC twin of
    /// `HistStore::scan_perp_metrics`.
    pub fn scan_perp_metrics(
        &mut self,
        venue: &str,
        symbol: &str,
        range: TsRange,
        limit: Option<u32>,
    ) -> Result<Vec<PerpMetricRow>, String> {
        let request = Request::ScanPerpMetrics {
            venue: venue.to_string(),
            symbol: symbol.to_string(),
            start: range.start,
            end: range.end,
            limit,
        };
        write_frame(&mut self.stream, &request).map_err(|e| e.to_string())?;
        match read_frame::<_, Response>(&mut self.stream).map_err(|e| e.to_string())? {
            Response::PerpMetrics(rows) => Ok(rows),
            Response::Error(msg) => Err(msg),
            other => {
                Err(format!("protocol desync: expected PerpMetrics, got {}", resp_kind(&other)))
            }
        }
    }

    /// Read the stored equity curve for `(venue, symbol)` in `range` — the RPC twin of
    /// `HistStore::scan_equity`.
    pub fn scan_equity(
        &mut self,
        venue: &str,
        symbol: &str,
        range: TsRange,
        limit: Option<u32>,
    ) -> Result<Vec<EquitySample>, String> {
        let request = Request::ScanEquity {
            venue: venue.to_string(),
            symbol: symbol.to_string(),
            start: range.start,
            end: range.end,
            limit,
        };
        write_frame(&mut self.stream, &request).map_err(|e| e.to_string())?;
        match read_frame::<_, Response>(&mut self.stream).map_err(|e| e.to_string())? {
            Response::Equity(rows) => Ok(rows),
            Response::Error(msg) => Err(msg),
            other => Err(format!("protocol desync: expected Equity, got {}", resp_kind(&other))),
        }
    }

    /// Read the Tier-2 exec FILL log for `(venue, symbol)` — the RPC twin of
    /// `HistStore::scan_exec_fills`.
    ///
    /// ⚠ **No range, because the trait method takes none.** Every other verb in this family carries
    /// `start`/`end`; this one would have to invent a bound the store cannot honour, and a bound
    /// that is accepted and ignored is worse than one that was never offered.
    pub fn scan_exec_fills(
        &mut self,
        venue: &str,
        symbol: &str,
    ) -> Result<Vec<ExecFillRow>, String> {
        let request =
            Request::ScanExecFills { venue: venue.to_string(), symbol: symbol.to_string() };
        write_frame(&mut self.stream, &request).map_err(|e| e.to_string())?;
        match read_frame::<_, Response>(&mut self.stream).map_err(|e| e.to_string())? {
            Response::ExecFills(rows) => Ok(rows),
            Response::Error(msg) => Err(msg),
            other => Err(format!("protocol desync: expected ExecFills, got {}", resp_kind(&other))),
        }
    }

    /// Point-in-time properties for `(venue, symbol)` at or before `ts` — the RPC twin of
    /// `HistStore::properties_as_of`. `Ok(None)` = nothing recorded at or before `ts`.
    pub fn properties_as_of(
        &mut self,
        venue: &str,
        symbol: &str,
        ts: i64,
    ) -> Result<Option<SymbolProperties>, String> {
        let request =
            Request::PropertiesAsOf { venue: venue.to_string(), symbol: symbol.to_string(), ts };
        write_frame(&mut self.stream, &request).map_err(|e| e.to_string())?;
        match read_frame::<_, Response>(&mut self.stream).map_err(|e| e.to_string())? {
            // Unbox on the way out — the box was a server-side enum-size optimization only.
            Response::Properties(props) => Ok(props.map(|b| *b)),
            Response::Error(msg) => Err(msg),
            other => {
                Err(format!("protocol desync: expected Properties, got {}", resp_kind(&other)))
            }
        }
    }

    /// Enumerate every stored series (`HistStore::list_series` over RPC — PR-6 store metadata).
    pub fn list_series(&mut self) -> Result<Vec<SeriesId>, String> {
        write_frame(&mut self.stream, &Request::ListSeries).map_err(|e| e.to_string())?;
        match read_frame::<_, Response>(&mut self.stream).map_err(|e| e.to_string())? {
            Response::SeriesList(v) => Ok(v),
            Response::Error(msg) => Err(msg),
            other => {
                Err(format!("protocol desync: expected SeriesList, got {}", resp_kind(&other)))
            }
        }
    }

    /// Every stored series with its cheap coverage (`HistStore::inventory` over RPC — PR-6).
    pub fn inventory(&mut self) -> Result<Vec<(SeriesId, SeriesCoverage)>, String> {
        write_frame(&mut self.stream, &Request::Inventory).map_err(|e| e.to_string())?;
        match read_frame::<_, Response>(&mut self.stream).map_err(|e| e.to_string())? {
            Response::Inventory(v) => Ok(v),
            Response::Error(msg) => Err(msg),
            other => Err(format!("protocol desync: expected Inventory, got {}", resp_kind(&other))),
        }
    }

    /// The gap ranges in one series' recorded span (`HistStore::series_gaps` over RPC — PR-6).
    /// One series' coverage plus its commit keys — the RPC twin of `HistStore::series_facts`.
    ///
    /// ⚠ 0084's SEVENTH verb: that record counted six because it measured the `HistStore` TRAIT,
    /// and this one was INHERENT to `DataFusionHist` and therefore invisible to the count. Without
    /// it a wire-routed backtest loses its data fingerprint and still reads green.
    pub fn series_facts(&mut self, id: &SeriesId) -> Result<(SeriesCoverage, Vec<String>), String> {
        let request = Request::SeriesFacts { id: id.clone() };
        write_frame(&mut self.stream, &request).map_err(|e| e.to_string())?;
        match read_frame::<_, Response>(&mut self.stream).map_err(|e| e.to_string())? {
            Response::SeriesFacts(f) => Ok(*f),
            Response::Error(msg) => Err(msg),
            other => {
                Err(format!("protocol desync: expected SeriesFacts, got {}", resp_kind(&other)))
            }
        }
    }

    pub fn series_gaps(&mut self, id: &SeriesId) -> Result<Vec<(i64, i64)>, String> {
        let request = Request::SeriesGaps { id: id.clone() };
        write_frame(&mut self.stream, &request).map_err(|e| e.to_string())?;
        match read_frame::<_, Response>(&mut self.stream).map_err(|e| e.to_string())? {
            Response::SeriesGaps(v) => Ok(v),
            Response::Error(msg) => Err(msg),
            other => {
                Err(format!("protocol desync: expected SeriesGaps, got {}", resp_kind(&other)))
            }
        }
    }

    /// The CROSS-KIND coverage report (`HistStore::coverage_report` over RPC — split-plane spec
    /// §6 Q2): every instrument with its `trade`/`quote`/`book`/`depth` day sets lined up, which is
    /// what the Data Manager's "Partial" column folds
    /// ([`InstrumentCoverage::partial_days`](vike_data::InstrumentCoverage::partial_days)).
    ///
    /// ⚠ CAPABILITY-CHECKED, NOT VERSION-CHECKED, exactly like [`Self::backfill`]: the verb shipped
    /// without a `PROTO_VERSION` bump, so the version handshake cannot protect it. The method
    /// refuses CLIENT-SIDE — sending nothing — unless the server's `Welcome` advertised
    /// [`FEATURE_COVERAGE`]. Unlike backfill, EVERY server built from this protocol's `serve`
    /// advertises it (it is a plain trait verb, not a mounted table), so a refusal here means
    /// exactly one thing: the peer predates the verb. The caller renders that as an honest note,
    /// never an empty column — an empty `Ok` would say "nothing is partial", which is a different
    /// and possibly false fact.
    pub fn coverage_report(&mut self) -> Result<Vec<InstrumentCoverage>, String> {
        if !self.features.iter().any(|f| f == FEATURE_COVERAGE) {
            return Err(format!(
                "datahub server does not advertise `{FEATURE_COVERAGE}` (advertised: {:?}) — \
                 nothing was sent. The cross-kind coverage report needs a newer server; this verb \
                 is capability-negotiated, not version-gated.",
                self.features
            ));
        }
        write_frame(&mut self.stream, &Request::Coverage).map_err(|e| e.to_string())?;
        match read_frame::<_, Response>(&mut self.stream).map_err(|e| e.to_string())? {
            Response::Coverage(v) => Ok(v),
            Response::Error(msg) => Err(msg),
            other => Err(format!("protocol desync: expected Coverage, got {}", resp_kind(&other))),
        }
    }
}
