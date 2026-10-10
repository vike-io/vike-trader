//! `DatahubClient`'s remaining data-plane verbs: the chart-gap seed (`seed_series`,
//! `seed_series_classed`), the venue instrument catalog (`venue_catalog`, `serves_venue_catalog`),
//! the irreversible delete (`delete_series`) and the archive import (`import_archive`) — each
//! capability-checked and refused locally, sending nothing, when the server does not advertise it.
//! `use super::*` reaches the parent module's imports and shared helpers.
//!
//! Each verb with a SAME-function validator (`seed`, `catalog`, `archive`) calls it locally for
//! the MESSAGE, never for the enforcement: the server's door re-checks every rule.

use super::*;

impl DatahubClient {
    /// **Tell the server a chart is open on a series its store cannot paint, and let it seed one
    /// window** — [`Request::SeedSeries`].
    ///
    /// The request names a series and NOTHING else: the window, the rate, the venue set and the
    /// interval set are all the server's. A `VerbScope::Read` verb, so unlike [`Self::backfill`] it
    /// is reachable from a connection holding only an observe key
    /// (`docs/decisions/0058-a-chart-gap-fetch-is-an-observe-verb.md`), which lets the desktop fill
    /// its own chart against a keyed server.
    ///
    /// **Three local refusals, all before a frame is sent**:
    ///
    /// 1. the capability check — its message names the SWITCH rather than a rebuild, because an
    ///    unadvertised lane on a modern server almost always means `flags.datahub_chart_seed` is
    ///    unset rather than that the server is old;
    /// 2. [`crate::seed::validate_seed_interval`] and 3. [`crate::seed::validate_seed_symbol`] —
    ///    the SAME functions the server's door calls (see [`crate::seed`]'s module doc for where the
    ///    security property lives).
    ///
    /// ⚠ **`Ok(done)` with `done.armed == false` is a SUCCESS and must not be rendered as a
    /// failure.** It means the operator has not armed the lane; nothing was fetched and nothing was
    /// written. See [`SeedDone::armed`].
    pub fn seed_series(
        &mut self,
        venue: &str,
        symbol: &str,
        interval: &str,
    ) -> Result<SeedDone, String> {
        self.seed_series_classed(venue, symbol, interval, None)
    }

    /// [`Self::seed_series`], plus the caller's claim about **what KIND of instrument `symbol`
    /// names** — `docs/decisions/0061` Phase 3, negotiated by [`FEATURE_SEED_CLASS`].
    ///
    /// A second entry point rather than a fourth parameter, this workspace's idiom for this axis
    /// (`crates/bridges/bybit/src/data.rs`'s `fetch_klines_range` is
    /// `fetch_klines_range_classed(…, None)`).
    ///
    /// **A FOURTH local refusal, fired only when the class is actually named**: a server that does
    /// not advertise [`FEATURE_SEED_CLASS`] would DECODE this frame, drop the field and fetch the
    /// wrong book while reporting success. ⚠ A `None` request is sent to such a server unchanged;
    /// see that constant for why refusing it instead would be the worse bug.
    pub fn seed_series_classed(
        &mut self,
        venue: &str,
        symbol: &str,
        interval: &str,
        class: Option<vike_model::AssetClass>,
    ) -> Result<SeedDone, String> {
        if class.is_some() && !self.features.iter().any(|f| f == FEATURE_SEED_CLASS) {
            return Err(format!(
                "datahub server does not advertise `{FEATURE_SEED_CLASS}` (advertised: {:?}) — \
                 nothing was sent. This request names the instrument's KIND, and a server that \
                 predates that field would DECODE the frame, DROP the claim, route on the symbol \
                 alone and report rows written — which is the wrong-book defect this field exists \
                 to close, wearing a successful answer. Upgrade the data daemon, or ask for this \
                 series without a class claim.",
                self.features
            ));
        }
        if !self.features.iter().any(|f| f == FEATURE_SEED_SERIES) {
            return Err(format!(
                "datahub server does not advertise `{FEATURE_SEED_SERIES}` (advertised: {:?}) — \
                 nothing was sent. Either the server predates the chart-seed lane, or — far more \
                 likely — its operator has not armed it: run `vike-cli config set \
                 flags.datahub_chart_seed true` on the data daemon and restart it. It is OFF by default deliberately, because an armed \
                 lane spends this box's venue-API budget on behalf of read-only clients.",
                self.features
            ));
        }
        crate::seed::validate_seed_interval(interval)?;
        crate::seed::validate_seed_symbol(symbol)?;
        let request = Request::SeedSeries {
            venue: venue.to_string(),
            symbol: symbol.to_string(),
            interval: interval.to_string(),
            class,
        };
        write_frame(&mut self.stream, &request).map_err(|e| e.to_string())?;
        match read_frame::<_, Response>(&mut self.stream).map_err(|e| e.to_string())? {
            Response::SeriesSeeded(done) => Ok(done),
            Response::Error(msg) => Err(msg),
            other => {
                Err(format!("protocol desync: expected SeriesSeeded, got {}", resp_kind(&other)))
            }
        }
    }

    /// **Ask the server for one venue's instrument list** — [`Request::VenueCatalog`].
    ///
    /// The request names a VENUE and nothing else. A `VerbScope::Read` verb, reachable from a
    /// connection holding only an observe key
    /// (`docs/decisions/0062-a-venue-catalog-fetch-is-an-observe-verb-and-not-a-write.md`), so a
    /// desktop that links ONE venue bridge can refresh the catalogs of venues it does not link.
    ///
    /// **Two local refusals, both before a frame is sent**: the capability check (whose message
    /// names the SWITCH rather than a rebuild, as [`Self::seed_series`]' does) and
    /// [`crate::catalog::validate_catalog_venue`].
    ///
    /// ⚠ **The REFUSALS come back as `Ok`, not `Err`.** An unarmed lane, a venue with no bulk list,
    /// a credentialed venue and a venue this build does not serve are all
    /// [`crate::catalog::CatalogOutcome`] values on a successful [`crate::catalog::CatalogListing`]
    /// — see [`Response::VenueCatalog`] for why a routine question does not answer with errors.
    /// `Err` here means the venue string was malformed, the server is too old or unarmed to be
    /// asked at all, or a provider failed mid-fetch.
    pub fn venue_catalog(&mut self, venue: &str) -> Result<CatalogListing, String> {
        if !self.features.iter().any(|f| f == FEATURE_VENUE_CATALOG) {
            return Err(format!(
                "datahub server does not advertise `{FEATURE_VENUE_CATALOG}` (advertised: {:?}) — \
                 nothing was sent. Either the server predates the venue-catalog verb, or its \
                 operator has REFUSED the lane: the catalog is ON by default, and the switch is \
                 `vike-cli config set flags.venue_catalog_off true` on that box (or \
                 `VIKE_DATAHUB_VENUE_CATALOG_OFF=1`). Run `vike-cli config set \
                 flags.venue_catalog_off false` and restart it. The refusal exists because an armed \
                 lane spends that box's venue-API budget on behalf of read-only clients; it is not \
                 what bounds the cost.",
                self.features
            ));
        }
        crate::catalog::validate_catalog_venue(venue)?;
        let request = Request::VenueCatalog { venue: venue.to_string() };
        write_frame(&mut self.stream, &request).map_err(|e| e.to_string())?;
        match read_frame::<_, Response>(&mut self.stream).map_err(|e| e.to_string())? {
            Response::VenueCatalog(listing) => Ok(listing),
            Response::Error(msg) => Err(msg),
            other => {
                Err(format!("protocol desync: expected VenueCatalog, got {}", resp_kind(&other)))
            }
        }
    }

    /// Whether this server advertised the venue-catalog capability — the question a picker asks
    /// BEFORE it offers a refresh control, so an old or unarmed server produces a sentence rather
    /// than a button that always fails.
    ///
    /// ⚠ The leg that stops an un-advertised capability rendering as an EMPTY instrument list,
    /// which for `ig` and `ibkr` would be indistinguishable from the truth.
    pub fn serves_venue_catalog(&self) -> bool {
        self.features.iter().any(|f| f == FEATURE_VENUE_CATALOG)
    }

    /// **Delete stored series on the server, IRREVERSIBLY** — `dry_run: true` returns the plan and
    /// removes nothing.
    ///
    /// ⚠ The advertisement check REFUSES LOCALLY, without sending, and its message says what is
    /// missing: this verb is served only by a KEYED datahub
    /// ([`FEATURE_DELETE_SERIES`] carries why), so its absence
    /// almost always means the server holds no node keys rather than that it is old.
    ///
    /// The provenance rule is deliberately NOT duplicated here: it is the one gate that matters,
    /// and the server checks it under the series lock.
    pub fn delete_series(
        &mut self,
        selector: &SeriesSelector,
        produced_by: Option<&str>,
        dry_run: bool,
    ) -> Result<DeleteDone, String> {
        if !self.features.iter().any(|f| f == FEATURE_DELETE_SERIES) {
            return Err(format!(
                "datahub server does not advertise `{FEATURE_DELETE_SERIES}` (advertised: {:?}) — \
                 nothing was sent. This verb is served ONLY by a KEYED datahub: a server with no \
                 node keys has no way to say 'reads yes, deletes no', so it serves no delete at \
                 all. Configure VIKE_DATAHUB_OBSERVE_KEY / VIKE_DATAHUB_CONTROL_KEY on the server, \
                 or run the delete on the box with `vike-cli data hist rm --store DIR`.",
                self.features
            ));
        }
        let request = Request::DeleteSeries {
            selector: selector.clone(),
            produced_by: produced_by.map(str::to_string),
            dry_run,
        };
        write_frame(&mut self.stream, &request).map_err(|e| e.to_string())?;
        match read_frame::<_, Response>(&mut self.stream).map_err(|e| e.to_string())? {
            Response::Deleted(done) => Ok(done),
            Response::Error(msg) => Err(msg),
            other => Err(format!("protocol desync: expected Deleted, got {}", resp_kind(&other))),
        }
    }

    /// **Import one archive dataset from the SERVER's own imports directory** —
    /// [`Request::ImportArchive`]. `spec.dry_run` returns the plan and writes nothing; see
    /// [`crate::archive`] for the whole contract.
    ///
    /// **Three local refusals, all before a frame is written** (and so before a server-side
    /// directory is touched):
    ///
    /// 1. the capability check — the server must advertise [`FEATURE_ARCHIVE_IMPORT`], which only
    ///    a datahub with an import lane MOUNTED does;
    /// 2. the FORMAT check — the server must advertise `import_format=<spec.format>`
    ///    ([`crate::proto::advertised_import_formats`]); the refusal names the formats it does
    ///    import, so a misspelling is told apart from a format this server lacks;
    /// 3. [`crate::archive::validate_import_spec`] — the dataset, the bars and the window (the
    ///    server also resolves the omitted bounds this side cannot see).
    ///
    /// ⚠ **This request is SYNCHRONOUS and its read is unbounded** (the parent module's
    /// `arm_request_timeouts`): an import holds the connection until the last day is stored, which
    /// is why a decoding request is capped at [`crate::archive::IMPORT_MAX_DAYS`] days — a caller
    /// importing a long window sends one request per calendar month.
    pub fn import_archive(&mut self, spec: &ImportSpec) -> Result<ImportDone, String> {
        if !self.features.iter().any(|f| f == FEATURE_ARCHIVE_IMPORT) {
            return Err(format!(
                "datahub server does not advertise `{FEATURE_ARCHIVE_IMPORT}` (advertised: {:?}) — \
                 nothing was sent. Either the server predates the archive-import verb, or it mounts \
                 no import lane: a datahub mounts one only when its build carries an archive format \
                 and a project directory sits above it, whose `market_data/imports/` is the one \
                 place it reads archive files from.",
                self.features
            ));
        }
        let formats = advertised_import_formats(&self.features);
        if !formats.contains(&spec.format) {
            return Err(format!(
                "datahub server does not import the archive format {:?} — it advertises {formats:?} \
                 — nothing was sent. A format is matched exactly against the server's own registry; \
                 name one it advertises.",
                spec.format
            ));
        }
        validate_import_spec(spec)?;
        let request = Request::ImportArchive(spec.clone());
        write_frame(&mut self.stream, &request).map_err(|e| e.to_string())?;
        match read_frame::<_, Response>(&mut self.stream).map_err(|e| e.to_string())? {
            Response::ArchiveImported(done) => Ok(*done),
            Response::Error(msg) => Err(msg),
            other => {
                Err(format!("protocol desync: expected ArchiveImported, got {}", resp_kind(&other)))
            }
        }
    }
}
