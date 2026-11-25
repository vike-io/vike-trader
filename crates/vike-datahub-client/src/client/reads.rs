//! `DatahubClient`'s store-read verbs: the RPC twins of `HistoryStore`'s reads — bars, quotes, trades,
//! book updates, depth, cohort, perp metrics, option chains, published forecasts, equity, exec fills and point-in-time
//! properties — plus the store-metadata verbs (`list_series`, `inventory`, `series_facts`, `series_gaps`,
//! `coverage_report`). `use super::*` reaches the parent module's imports and shared helpers.
//!
//! Every verb here surfaces a transport failure, a server-side `Response::Error`, a refusal frame (shown
//! `error [<id>]: <text>`, `crate::refusals::shown`) and a protocol desync through the ONE `Err(String)` channel.

use super::*;
use crate::proto::{
    BAR_COLUMNS, BOOK_UPDATE_COLUMNS, BarColumnsBody, CHAIN_COLUMNS, COHORT_COLUMNS, ColumnRow,
    ColumnsMagic, DEPTH_COLUMNS, EQUITY_COLUMNS, EXECUTION_FILL_COLUMNS, EXECUTION_ORDER_COLUMNS,
    FEATURE_BAR_COLUMNS, FEATURE_BAR_RESAMPLE, FEATURE_EXECUTION_SERIES, FEATURE_SCAN_COLUMNS,
    FEATURE_SCAN_FORECASTS, FEATURE_SERIES_DAYS, FEATURE_SERIES_DAYS_KINDS, FEATURE_TICK_COLUMNS,
    FINANCIAL_FACT_COLUMNS, FORECAST_COLUMNS, INSTITUTIONAL_HOLDING_COLUMNS, PERP_METRIC_COLUMNS,
    QUOTE_COLUMNS, TEXT_COLUMNS, TEXT_SCORE_COLUMNS, TRADE_COLUMNS, decode_columns_into,
    plain_columns,
};
use vike_data::{BarDay, ExecForecastRow, ExecutionSeries};
use vike_model::StoreKindId;

impl DatahubClient {
    /// Read derived OHLCV bars for `(venue, symbol, interval)` in `range` — the RPC twin of
    /// `HistoryStore::load_bars`.
    ///
    /// ⚠ **`None` on a bound is UNBOUNDED on that side** — `TimestampRange`'s own meaning. Bounding the
    /// request is the CALLER's business, which is why `crates/vike-cli/src/cmd/data/hist/get.rs`
    /// requires a window before it ever reaches here.
    pub fn load_bars(
        &mut self,
        venue: &str,
        symbol: &str,
        interval: &str,
        range: TimestampRange,
        limit: Option<u32>,
    ) -> Result<Vec<Bar>, String> {
        self.load_bars_request(venue, symbol, interval, range, limit, None)
    }

    /// [`Self::load_bars`] of ONE `source=` lane. ⚠ Refused locally, nothing sent, by a server without
    /// [`FEATURE_BAR_SOURCE`], which would answer the venue's candles as the lane's bars.
    pub fn load_bars_sourced(
        &mut self,
        venue: &str,
        source: &str,
        symbol: &str,
        interval: &str,
        range: TimestampRange,
        limit: Option<u32>,
    ) -> Result<Vec<Bar>, String> {
        self.require_bar_source()?;
        self.load_bars_request(venue, symbol, interval, range, limit, Some(source))
    }

    fn load_bars_request(
        &mut self,
        venue: &str,
        symbol: &str,
        interval: &str,
        TimestampRange { start, end }: TimestampRange,
        limit: Option<u32>,
        source: Option<&str>,
    ) -> Result<Vec<Bar>, String> {
        // Every bar read asks for the columns of a server that answers them (the owner's "JSON only at the edges"
        // of 2026-11-19): the same rows, no float as text. An older server is asked for JSON as before.
        let columns = self.serves_bar_columns().then(|| self.bar_columns_body());
        let request = Request::LoadBars {
            venue: venue.to_string(),
            symbol: symbol.to_string(),
            interval: interval.to_string(),
            start,
            end,
            limit,
            source: source.map(str::to_string),
            columns,
            resample_to: None,
        };
        if columns.is_some() {
            let mut bars = Vec::new();
            self.columns_exchange(&request, &BAR_COLUMNS, &mut bars)?;
            return Ok(bars);
        }
        match self.exchange_shown(&request)? {
            Response::Bars(bars) => Ok(bars),
            Response::Error(msg) => Err(msg),
            other => Err(format!("protocol desync: expected Bars, got {}", response_kind(&other))),
        }
    }

    /// Whether this server answers a bar read as COLUMNS ([`FEATURE_BAR_COLUMNS`]).
    pub fn serves_bar_columns(&self) -> bool {
        self.features().iter().any(|f| f == FEATURE_BAR_COLUMNS)
    }

    /// The columns body this connection's own bar reads ask for: LZ4 where it negotiated LZ4 (a thin link,
    /// `BodyCodec::MsgpackLz4`), else plain, which costs no CPU at either end.
    pub fn bar_columns_body(&self) -> BarColumnsBody {
        match self.body_codec() {
            BodyCodec::MsgpackLz4 => BarColumnsBody::Lz4,
            BodyCodec::Json | BodyCodec::Msgpack => BarColumnsBody::Plain,
        }
    }

    /// [`Self::load_bars`] (or, naming `source`, [`Self::load_bars_sourced`]) answered as COLUMNS: the same
    /// request with `columns` set, its reply ONE binary body ([`crate::proto::decode_bar_columns_into`]) instead of
    /// `Response::Bars` JSON. The same rows; no float crosses as text at either end. `body` asks for it plain or
    /// LZ4-compressed ([`BarColumnsBody`]); the server may answer plain to either, and this reads both.
    ///
    /// ⚠ Refused locally, nothing sent, by a server without [`FEATURE_BAR_COLUMNS`] (and, naming a lane,
    /// without [`FEATURE_BAR_SOURCE`]). A refusal comes back as `Response::Error` JSON, told from the columns
    /// by its first byte ([`crate::proto::BAR_COLUMNS_MAGIC`]'s NUL); a body that is neither, or columns that do
    /// not decode, is an error, never fewer bars.
    pub fn load_bar_columns(
        &mut self,
        venue: &str,
        source: Option<&str>,
        symbol: &str,
        interval: &str,
        range: TimestampRange,
        limit: Option<u32>,
        body: BarColumnsBody,
    ) -> Result<Vec<Bar>, String> {
        let mut bars = Vec::new();
        self.load_bar_columns_into(venue, source, symbol, interval, range, limit, body, &mut bars)?;
        Ok(bars)
    }

    /// [`Self::load_bar_columns`] APPENDING the page to `out` (a pager's whole read), so no page is built apart and
    /// copied after; the count appended. A reply that is refused or does not decode appends nothing.
    pub fn load_bar_columns_into(
        &mut self,
        venue: &str,
        source: Option<&str>,
        symbol: &str,
        interval: &str,
        range: TimestampRange,
        limit: Option<u32>,
        body: BarColumnsBody,
        out: &mut Vec<Bar>,
    ) -> Result<usize, String> {
        let request =
            self.bar_columns_request(venue, source, symbol, interval, range, limit, body)?;
        self.columns_exchange(&request, &BAR_COLUMNS, out)
    }

    /// [`Self::load_bar_columns_into`]'s page as its PLAIN body ([`plain_columns`]), not its rows: what a reader that
    /// keeps the columns as they came asks (`HistoryStoreDatahub`'s `read_columns`). The same refusals; nothing is
    /// decoded.
    pub fn load_bar_columns_plain(
        &mut self,
        venue: &str,
        source: Option<&str>,
        symbol: &str,
        interval: &str,
        range: TimestampRange,
        limit: Option<u32>,
        body: BarColumnsBody,
    ) -> Result<Vec<u8>, String> {
        let request =
            self.bar_columns_request(venue, source, symbol, interval, range, limit, body)?;
        plain_columns(self.columns_reply(&request, &BAR_COLUMNS)?, &BAR_COLUMNS)
    }

    /// A bar read that asks for columns, refused locally (nothing sent) by a server without
    /// [`FEATURE_BAR_COLUMNS`] or, naming a lane, without [`FEATURE_BAR_SOURCE`].
    fn bar_columns_request(
        &self,
        venue: &str,
        source: Option<&str>,
        symbol: &str,
        interval: &str,
        range: TimestampRange,
        limit: Option<u32>,
        body: BarColumnsBody,
    ) -> Result<Request, String> {
        if !self.serves_bar_columns() {
            return Err(format!(
                "datahub server does not advertise `{FEATURE_BAR_COLUMNS}` (advertised: {:?}) — nothing \
                 was sent. Ask it for JSON bars (`load_bars`), or upgrade the data daemon this client \
                 dials (`config.datahub_addr`).",
                self.features()
            ));
        }
        if source.is_some() {
            self.require_bar_source()?;
        }
        Ok(Request::LoadBars {
            venue: venue.to_string(),
            symbol: symbol.to_string(),
            interval: interval.to_string(),
            start: range.start,
            end: range.end,
            limit,
            source: source.map(str::to_string),
            columns: Some(body),
            resample_to: None,
        })
    }

    /// Whether this server folds a bar read into a coarser interval ([`FEATURE_BAR_RESAMPLE`]).
    pub fn serves_bar_resample(&self) -> bool {
        self.features().iter().any(|f| f == FEATURE_BAR_RESAMPLE)
    }

    /// The bars of `interval` over `range` folded by the DATAHUB into bars of the coarser interval `to`
    /// (`Request::LoadBars`' `resample_to`): one reply, the whole range, as columns when `body` is `Some` and
    /// the server answers them, else JSON.
    ///
    /// ⚠ Refused locally, nothing sent, by a server without [`FEATURE_BAR_RESAMPLE`]: an older one ignores
    /// the field and answers every stored row of the range.
    pub fn load_bars_resampled(
        &mut self,
        venue: &str,
        symbol: &str,
        interval: &str,
        range: TimestampRange,
        to: &str,
        body: Option<BarColumnsBody>,
    ) -> Result<Vec<Bar>, String> {
        if !self.serves_bar_resample() {
            return Err(format!(
                "datahub server does not advertise `{FEATURE_BAR_RESAMPLE}` (advertised: {:?}) — nothing \
                 was sent. Read the stored bars instead (`load_bars`), or upgrade the data daemon this \
                 client dials (`config.datahub_addr`).",
                self.features()
            ));
        }
        let columns = body.filter(|_| self.serves_bar_columns());
        let request = Request::LoadBars {
            venue: venue.to_string(),
            symbol: symbol.to_string(),
            interval: interval.to_string(),
            start: range.start,
            end: range.end,
            limit: None,
            source: None,
            columns,
            resample_to: Some(to.to_string()),
        };
        if columns.is_some() {
            let mut bars = Vec::new();
            self.columns_exchange(&request, &BAR_COLUMNS, &mut bars)?;
            return Ok(bars);
        }
        match self.exchange_shown(&request)? {
            Response::Bars(bars) => Ok(bars),
            Response::Error(msg) => Err(msg),
            other => Err(format!("protocol desync: expected Bars, got {}", response_kind(&other))),
        }
    }

    /// One range read that asked for `columns`: its binary body decoded onto `out` as `magic`'s reply (the count
    /// appended), or the refusal it was answered with (in the form this client writes). A body that opens with a NUL
    /// and no msgpack magic is columns, and one under another reply's magic is refused by the decoder, never read as
    /// these rows.
    pub(super) fn columns_exchange<R: ColumnRow>(
        &mut self,
        request: &Request,
        magic: &ColumnsMagic,
        out: &mut Vec<R>,
    ) -> Result<usize, String> {
        decode_columns_into(&self.columns_reply(request, magic)?, magic, out)
    }

    /// [`Self::columns_exchange`]'s reply as it came, not decoded: the columns body, or the refusal it was answered with.
    fn columns_reply(
        &mut self,
        request: &Request,
        magic: &ColumnsMagic,
    ) -> Result<Vec<u8>, String> {
        let reply = self
            .conn
            .exchange_frame_raw(request_kind(request), reply_wait(request).default_bound(), request)
            .map_err(|e| e.to_string())?;
        // A msgpack refusal opens with a NUL too, under its own magic.
        if reply.first() == Some(&0) && BodyCodec::of(&reply) == BodyCodec::Json {
            return Ok(reply);
        }
        let name = magic.name;
        // A refusal comes in the form this client writes (JSON, or the msgpack it negotiated).
        match vike_hub_protocol::frame::decode_body::<Response>(&reply) {
            Ok(Response::Error(msg)) => Err(msg),
            Ok(Response::Refused(refusal)) => Err(crate::refusals::shown(&refusal)),
            Ok(other) => Err(format!(
                "protocol desync: expected {name} columns, got {}",
                response_kind(&other)
            )),
            Err(e) => Err(format!(
                "the reply to a {name}-columns read is neither {name} columns nor a reply frame ({} \
                 bytes): {e}",
                reply.len()
            )),
        }
    }

    /// The `columns` every other row-shaped scan asks of this server (`scan_cohort` and its siblings,
    /// `strategy_forecasts`): [`Self::bar_columns_body`] where it advertises [`FEATURE_SCAN_COLUMNS`], else `None`, the
    /// JSON reply as before (the owner's "JSON only at the edges" of 2026-11-19).
    pub(super) fn scan_columns_body(&self) -> Option<BarColumnsBody> {
        self.features().iter().any(|f| f == FEATURE_SCAN_COLUMNS).then(|| self.bar_columns_body())
    }

    /// A scan that asked for columns, its rows decoded as `magic`'s reply: [`Self::columns_exchange`] into a new `Vec`.
    pub(super) fn columns_rows<R: ColumnRow>(
        &mut self,
        request: &Request,
        magic: &ColumnsMagic,
    ) -> Result<Vec<R>, String> {
        let mut rows = Vec::new();
        self.columns_exchange(request, magic, &mut rows)?;
        Ok(rows)
    }

    /// Whether this server answers the tick-level scans as COLUMNS ([`FEATURE_TICK_COLUMNS`]).
    pub fn serves_tick_columns(&self) -> bool {
        self.features().iter().any(|f| f == FEATURE_TICK_COLUMNS)
    }

    /// A tick-level scan answered as COLUMNS, APPENDED to `out` (the count appended): `scan` is the
    /// `Request::ScanQuotes`, `ScanTrades`, `ScanBookUpdates`, `ScanDepth` or `ScanPerpMetrics` to send, its
    /// `columns` set, and `R` its row type. The same rows the JSON verb answers ([`Self::scan_quotes`] and its
    /// siblings), as [`Self::load_bar_columns_into`] is for bars; a reply that is refused or does not decode
    /// appends nothing.
    ///
    /// ⚠ Refused locally, nothing sent, by a server without [`FEATURE_TICK_COLUMNS`] (which would ignore the
    /// field and answer JSON), by a request that asks no columns, and, naming a quote lane, by a server without
    /// [`FEATURE_SOURCE`].
    pub fn scan_columns_into<R: ColumnRow>(
        &mut self,
        scan: &Request,
        out: &mut Vec<R>,
    ) -> Result<usize, String> {
        let magic = self.tick_columns_magic(scan)?;
        self.columns_exchange(scan, magic, out)
    }

    /// [`Self::scan_columns_into`]'s page as its PLAIN body ([`plain_columns`]), not its rows: what a reader that keeps
    /// the columns as they came asks (`HistoryStoreDatahub`'s `read_columns`). The same refusals; nothing is decoded.
    pub fn scan_columns_plain(&mut self, scan: &Request) -> Result<Vec<u8>, String> {
        let magic = self.tick_columns_magic(scan)?;
        plain_columns(self.columns_reply(scan, magic)?, magic)
    }

    /// The columns reply `scan` is answered with, [`Self::scan_columns_into`]'s local refusals first (nothing sent).
    fn tick_columns_magic(&self, scan: &Request) -> Result<&'static ColumnsMagic, String> {
        if !self.serves_tick_columns() {
            return Err(format!(
                "datahub server does not advertise `{FEATURE_TICK_COLUMNS}` (advertised: {:?}) — \
                 nothing was sent. Ask it for JSON rows (`scan_quotes` and its siblings), or upgrade \
                 the data daemon this client dials (`config.datahub_addr`).",
                self.features()
            ));
        }
        let magic = match scan {
            Request::ScanQuotes { columns: Some(_), source, .. } => {
                if source.is_some() {
                    self.require_source()?;
                }
                &QUOTE_COLUMNS
            }
            Request::ScanTrades { columns: Some(_), .. } => &TRADE_COLUMNS,
            Request::ScanBookUpdates { columns: Some(_), .. } => &BOOK_UPDATE_COLUMNS,
            Request::ScanDepth { columns: Some(_), .. } => &DEPTH_COLUMNS,
            Request::ScanPerpMetrics { columns: Some(_), .. } => &PERP_METRIC_COLUMNS,
            other => {
                return Err(format!(
                    "{} is not a tick-level scan that asks for columns — nothing was sent",
                    request_kind(other)
                ));
            }
        };
        Ok(magic)
    }

    /// Read L1 quotes for `(venue, symbol)` in `range` — the RPC twin of `HistoryStore::scan_quotes`.
    ///
    /// ⚠ **`None` on a bound is UNBOUNDED on that side**, and this verb answers in ONE frame —
    /// so an unbounded scan of a busy tick series is a frame `write_frame` refuses server-side
    /// against [`vike_hub_protocol::frame::MAX_FRAME_LEN`]. Bounding the request is the CALLER's business:
    /// `crates/vike-cli/src/cmd/data/hist/export.rs` walks a range in fixed wall-clock windows for
    /// exactly this reason.
    pub fn scan_quotes(
        &mut self,
        venue: &str,
        symbol: &str,
        range: TimestampRange,
        limit: Option<u32>,
    ) -> Result<Vec<TickQuote>, String> {
        self.scan_quotes_request(venue, symbol, range, limit, None)
    }

    /// [`Self::scan_quotes`] of ONE `source=` lane. ⚠ Refused locally, nothing sent, by a server without
    /// [`FEATURE_SOURCE`], which would answer the archive's rows as the lane's.
    pub fn scan_quotes_sourced(
        &mut self,
        venue: &str,
        source: &str,
        symbol: &str,
        range: TimestampRange,
        limit: Option<u32>,
    ) -> Result<Vec<TickQuote>, String> {
        self.require_source()?;
        self.scan_quotes_request(venue, symbol, range, limit, Some(source))
    }

    fn scan_quotes_request(
        &mut self,
        venue: &str,
        symbol: &str,
        TimestampRange { start, end }: TimestampRange,
        limit: Option<u32>,
        source: Option<&str>,
    ) -> Result<Vec<TickQuote>, String> {
        let request = Request::ScanQuotes {
            venue: venue.to_string(),
            symbol: symbol.to_string(),
            start,
            end,
            limit,
            source: source.map(str::to_string),
            columns: None,
        };
        match self.exchange_shown(&request)? {
            Response::Quotes(quotes) => Ok(quotes),
            Response::Error(msg) => Err(msg),
            other => {
                Err(format!("protocol desync: expected Quotes, got {}", response_kind(&other)))
            }
        }
    }

    /// Read executed trades for `(venue, symbol)` in `range` — the RPC twin of
    /// `HistoryStore::scan_trades`. [`Self::scan_quotes`]' unbounded-frame warning applies unchanged, and the
    /// trade tape overruns a frame SOONEST.
    pub fn scan_trades(
        &mut self,
        venue: &str,
        symbol: &str,
        TimestampRange { start, end }: TimestampRange,
        limit: Option<u32>,
    ) -> Result<Vec<TickTrade>, String> {
        let request = Request::ScanTrades {
            venue: venue.to_string(),
            symbol: symbol.to_string(),
            start,
            end,
            limit,
            columns: None,
        };
        match self.exchange_shown(&request)? {
            Response::Trades(trades) => Ok(trades),
            Response::Error(msg) => Err(msg),
            other => {
                Err(format!("protocol desync: expected Trades, got {}", response_kind(&other)))
            }
        }
    }

    /// Read raw L2 book updates for `(venue, symbol)` in `range` — the RPC twin of
    /// `HistoryStore::scan_book_updates`, and the first of the SIX verbs
    /// `docs/decisions/0084-only-the-datahub-touches-the-store.md` asks this wire to grow.
    ///
    /// ⚠ [`Self::scan_quotes`]' UNBOUNDED-FRAME warning applies HARDEST here: a lossless book
    /// lane outruns [`vike_hub_protocol::frame::MAX_FRAME_LEN`] sooner than any other kind in the store.
    pub fn scan_book_updates(
        &mut self,
        venue: &str,
        symbol: &str,
        TimestampRange { start, end }: TimestampRange,
        limit: Option<u32>,
    ) -> Result<Vec<L2BookUpdate>, String> {
        let request = Request::ScanBookUpdates {
            venue: venue.to_string(),
            symbol: symbol.to_string(),
            start,
            end,
            limit,
            columns: None,
        };
        match self.exchange_shown(&request)? {
            Response::BookUpdates(rows) => Ok(rows),
            Response::Error(msg) => Err(msg),
            other => {
                Err(format!("protocol desync: expected BookUpdates, got {}", response_kind(&other)))
            }
        }
    }

    /// Read the CONFLATING depth lane for `(venue, symbol)` in `range` — the RPC twin of
    /// `HistoryStore::scan_depth`.
    ///
    /// ⚠ The same ROW TYPE as [`Self::scan_book_updates`], a different LANE (`kind=` partition);
    /// [`Request::ScanDepth`] argues why they do not share a reply variant, and the desync arm
    /// below makes a mix-up loud instead of plausible.
    pub fn scan_depth(
        &mut self,
        venue: &str,
        symbol: &str,
        TimestampRange { start, end }: TimestampRange,
        limit: Option<u32>,
    ) -> Result<Vec<L2BookUpdate>, String> {
        let request = Request::ScanDepth {
            venue: venue.to_string(),
            symbol: symbol.to_string(),
            start,
            end,
            limit,
            columns: None,
        };
        match self.exchange_shown(&request)? {
            Response::Depth(rows) => Ok(rows),
            Response::Error(msg) => Err(msg),
            other => Err(format!("protocol desync: expected Depth, got {}", response_kind(&other))),
        }
    }

    /// Read cohort rows for `(venue, asset)` in `range` — the RPC twin of `HistoryStore::scan_cohort`.
    ///
    /// ⚠ The second argument is an **ASSET, not a symbol**, which is the trait's own spelling and
    /// the one place this family's signature shape breaks. See [`Request::ScanCohort`].
    pub fn scan_cohort(
        &mut self,
        venue: &str,
        asset: &str,
        range: TimestampRange,
        limit: Option<u32>,
    ) -> Result<Vec<CohortRow>, String> {
        let columns = self.scan_columns_body();
        let request = Request::ScanCohort {
            venue: venue.to_string(),
            asset: asset.to_string(),
            start: range.start,
            end: range.end,
            limit,
            columns,
        };
        if columns.is_some() {
            return self.columns_rows(&request, &COHORT_COLUMNS);
        }
        match self.exchange_shown(&request)? {
            Response::Cohort(rows) => Ok(rows),
            Response::Error(msg) => Err(msg),
            other => {
                Err(format!("protocol desync: expected Cohort, got {}", response_kind(&other)))
            }
        }
    }

    /// Read perpetual-swap metric rows for `(venue, symbol)` in `range` — the RPC twin of
    /// `HistoryStore::scan_perp_metrics`.
    pub fn scan_perp_metrics(
        &mut self,
        venue: &str,
        symbol: &str,
        range: TimestampRange,
        limit: Option<u32>,
    ) -> Result<Vec<PerpMetricRow>, String> {
        let request = Request::ScanPerpMetrics {
            venue: venue.to_string(),
            symbol: symbol.to_string(),
            start: range.start,
            end: range.end,
            limit,
            columns: None,
        };
        match self.exchange_shown(&request)? {
            Response::PerpMetrics(rows) => Ok(rows),
            Response::Error(msg) => Err(msg),
            other => {
                Err(format!("protocol desync: expected PerpMetrics, got {}", response_kind(&other)))
            }
        }
    }

    /// Read the text history of `(text_kind, collector)` RECEIVED in `range`, capped SOFTLY at `limit`
    /// — the RPC twin of `HistoryStore::scan_texts_capped`.
    ///
    /// ⚠ **Refuses LOCALLY, sending nothing, against a server that does not advertise
    /// [`FEATURE_SCAN_TEXTS`]** (decision 0112's client leg): such a server would answer
    /// `Response::Error` for an unknown verb, and saying so here names the cause instead.
    pub fn scan_texts(
        &mut self,
        text_kind: &str,
        collector: &str,
        range: TimestampRange,
        limit: Option<u32>,
    ) -> Result<Vec<TextRow>, String> {
        if !self.serves_texts() {
            return Err(format!(
                "this datahub does not advertise `{FEATURE_SCAN_TEXTS}` (advertised: {:?}) — \
                 nothing was sent: it predates the text-history read. Upgrade vike-datahub.",
                self.features()
            ));
        }
        let columns = self.scan_columns_body();
        let request = Request::ScanTexts {
            text_kind: text_kind.to_string(),
            collector: collector.to_string(),
            start: range.start,
            end: range.end,
            limit,
            columns,
        };
        if columns.is_some() {
            return self.columns_rows(&request, &TEXT_COLUMNS);
        }
        match self.exchange_shown(&request)? {
            Response::Texts(rows) => Ok(rows),
            Response::Error(msg) => Err(msg),
            other => Err(format!("protocol desync: expected Texts, got {}", response_kind(&other))),
        }
    }

    /// Read company `symbol`'s reported XBRL values (`venue` is `sec`) KNOWABLE in `range`, capped
    /// SOFTLY at `limit` — the RPC twin of `HistoryStore::scan_financial_facts_capped`. Refuses
    /// LOCALLY, sending nothing, against a server that does not advertise
    /// [`FEATURE_SCAN_SEC_TABLES`].
    pub fn scan_financial_facts(
        &mut self,
        venue: &str,
        symbol: &str,
        range: TimestampRange,
        limit: Option<u32>,
    ) -> Result<Vec<FinancialFactRow>, String> {
        self.refuse_without_sec_tables()?;
        let columns = self.scan_columns_body();
        let request = Request::ScanFinancialFacts {
            venue: venue.to_string(),
            symbol: symbol.to_string(),
            start: range.start,
            end: range.end,
            limit,
            columns,
        };
        if columns.is_some() {
            return self.columns_rows(&request, &FINANCIAL_FACT_COLUMNS);
        }
        match self.exchange_shown(&request)? {
            Response::FinancialFacts(rows) => Ok(rows),
            Response::Error(msg) => Err(msg),
            other => Err(format!(
                "protocol desync: expected FinancialFacts, got {}",
                response_kind(&other)
            )),
        }
    }

    /// Read manager `symbol`'s Form 13F lines (`venue` is `sec`) KNOWABLE in `range`, capped SOFTLY at
    /// `limit` — the RPC twin of `HistoryStore::scan_institutional_holdings_capped`. Refuses LOCALLY
    /// against a server that does not advertise [`FEATURE_SCAN_SEC_TABLES`].
    pub fn scan_institutional_holdings(
        &mut self,
        venue: &str,
        symbol: &str,
        range: TimestampRange,
        limit: Option<u32>,
    ) -> Result<Vec<InstitutionalHoldingRow>, String> {
        self.refuse_without_sec_tables()?;
        let columns = self.scan_columns_body();
        let request = Request::ScanInstitutionalHoldings {
            venue: venue.to_string(),
            symbol: symbol.to_string(),
            start: range.start,
            end: range.end,
            limit,
            columns,
        };
        if columns.is_some() {
            return self.columns_rows(&request, &INSTITUTIONAL_HOLDING_COLUMNS);
        }
        match self.exchange_shown(&request)? {
            Response::InstitutionalHoldings(rows) => Ok(rows),
            Response::Error(msg) => Err(msg),
            other => Err(format!(
                "protocol desync: expected InstitutionalHoldings, got {}",
                response_kind(&other)
            )),
        }
    }

    /// The local refusal of an SEC table read against a server without [`FEATURE_SCAN_SEC_TABLES`].
    fn refuse_without_sec_tables(&self) -> Result<(), String> {
        if self.features().iter().any(|f| f == FEATURE_SCAN_SEC_TABLES) {
            return Ok(());
        }
        Err(format!(
            "this datahub does not advertise `{FEATURE_SCAN_SEC_TABLES}` (advertised: {:?}) — \
             nothing was sent: it predates the SEC table reads. Upgrade vike-datahub.",
            self.features()
        ))
    }

    /// Whether this server advertised [`FEATURE_SCAN_TEXTS`] — the text-history read.
    pub fn serves_texts(&self) -> bool {
        self.features().iter().any(|f| f == FEATURE_SCAN_TEXTS)
    }

    /// Read option-chain snapshot rows for `(venue, underlying)` in `range` — the RPC twin of
    /// `HistoryStore::scan_chain`.
    pub fn scan_chain(
        &mut self,
        venue: &str,
        underlying: &str,
        range: TimestampRange,
        limit: Option<u32>,
    ) -> Result<Vec<ChainRow>, String> {
        let columns = self.scan_columns_body();
        let request = Request::ScanChain {
            venue: venue.to_string(),
            underlying: underlying.to_string(),
            start: range.start,
            end: range.end,
            limit,
            columns,
        };
        if columns.is_some() {
            return self.columns_rows(&request, &CHAIN_COLUMNS);
        }
        match self.exchange_shown(&request)? {
            Response::Chain(rows) => Ok(rows),
            Response::Error(msg) => Err(msg),
            other => Err(format!("protocol desync: expected Chain, got {}", response_kind(&other))),
        }
    }

    /// Read the scores of `(text_kind, model_name)` whose texts were RECEIVED in `range`, capped
    /// SOFTLY at `limit` — the RPC twin of `HistoryStore::scan_text_scores_capped`. Refuses LOCALLY,
    /// sending nothing, against a server without [`FEATURE_SCAN_TEXT_SCORES`], as [`Self::scan_texts`]
    /// does.
    pub fn scan_text_scores(
        &mut self,
        text_kind: &str,
        model_name: &str,
        range: TimestampRange,
        limit: Option<u32>,
    ) -> Result<Vec<TextScoreRow>, String> {
        if !self.features().iter().any(|f| f == FEATURE_SCAN_TEXT_SCORES) {
            return Err(format!(
                "this datahub does not advertise `{FEATURE_SCAN_TEXT_SCORES}` (advertised: {:?}) — \
                 nothing was sent: it predates the text-score read. Upgrade vike-datahub.",
                self.features()
            ));
        }
        let columns = self.scan_columns_body();
        let request = Request::ScanTextScores {
            text_kind: text_kind.to_string(),
            model_name: model_name.to_string(),
            start: range.start,
            end: range.end,
            limit,
            columns,
        };
        if columns.is_some() {
            return self.columns_rows(&request, &TEXT_SCORE_COLUMNS);
        }
        match self.exchange_shown(&request)? {
            Response::TextScores(rows) => Ok(rows),
            Response::Error(msg) => Err(msg),
            other => {
                Err(format!("protocol desync: expected TextScores, got {}", response_kind(&other)))
            }
        }
    }

    /// Read the live strategies' published forecasts of `(venue, symbol)` DECIDED in `range`, capped
    /// SOFTLY at `limit` — the RPC twin of `HistoryStore::scan_forecasts_capped` on the data plane (an
    /// Observe key; the strategy lane's twin is [`Self::strategy_forecasts`]).
    ///
    /// ⚠ **Refuses LOCALLY, sending nothing, against a server that does not advertise
    /// [`FEATURE_SCAN_FORECASTS`]** (decision 0112's client leg), as [`Self::scan_texts`] does.
    pub fn scan_forecasts(
        &mut self,
        venue: &str,
        symbol: &str,
        range: TimestampRange,
        limit: Option<u32>,
    ) -> Result<Vec<ExecForecastRow>, String> {
        if !self.features().iter().any(|f| f == FEATURE_SCAN_FORECASTS) {
            return Err(format!(
                "this datahub does not advertise `{FEATURE_SCAN_FORECASTS}` (advertised: {:?}) — \
                 nothing was sent: it predates the data plane's forecast read. Upgrade vike-datahub.",
                self.features()
            ));
        }
        let columns = self.scan_columns_body();
        let request = Request::ScanForecasts {
            venue: venue.to_string(),
            symbol: symbol.to_string(),
            start: range.start,
            end: range.end,
            limit,
            columns,
        };
        if columns.is_some() {
            return self.columns_rows(&request, &FORECAST_COLUMNS);
        }
        match self.exchange_shown(&request)? {
            Response::Forecasts(rows) => Ok(rows),
            Response::Error(msg) => Err(msg),
            other => {
                Err(format!("protocol desync: expected Forecasts, got {}", response_kind(&other)))
            }
        }
    }

    /// Read the stored equity curve for `(venue, symbol)` in `range` — the RPC twin of
    /// `HistoryStore::scan_equity`.
    pub fn scan_equity(
        &mut self,
        venue: &str,
        symbol: &str,
        range: TimestampRange,
        limit: Option<u32>,
    ) -> Result<Vec<EquitySample>, String> {
        let columns = self.scan_columns_body();
        let request = Request::ScanEquity {
            venue: venue.to_string(),
            symbol: symbol.to_string(),
            start: range.start,
            end: range.end,
            limit,
            columns,
        };
        if columns.is_some() {
            return self.columns_rows(&request, &EQUITY_COLUMNS);
        }
        match self.exchange_shown(&request)? {
            Response::Equity(rows) => Ok(rows),
            Response::Error(msg) => Err(msg),
            other => {
                Err(format!("protocol desync: expected Equity, got {}", response_kind(&other)))
            }
        }
    }

    /// Read the WHOLE Tier-2 exec FILL log for `(venue, symbol)` — the RPC twin of
    /// `HistoryStore::scan_exec_fills`. Sends the frame this verb has always had (no window), which
    /// every server that advertises `scan_exec_fills` answers.
    pub fn scan_exec_fills(
        &mut self,
        venue: &str,
        symbol: &str,
    ) -> Result<Vec<ExecFillRow>, String> {
        self.execution_fills_request(Request::ScanExecFills {
            venue: venue.to_string(),
            symbol: symbol.to_string(),
            start: None,
            end: None,
            limit: None,
            columns: self.scan_columns_body(),
        })
    }

    /// Read the exec FILL log for `(venue, symbol)` over `range`, capped SOFTLY at `limit` — the
    /// RPC twin of `HistoryStore::scan_execution_fills_head`.
    ///
    /// ⚠ **Refuses LOCALLY, sending nothing, against a server that does not advertise
    /// [`FEATURE_SCAN_EXECUTION_FILLS_RANGE`]**: an older server would decode the window and IGNORE it,
    /// answering the whole series as if it were the window — a bound accepted and ignored. A caller
    /// that can live without the window asks [`Self::serves_execution_fills_range`] first and falls back
    /// to [`Self::scan_exec_fills`].
    pub fn scan_execution_fills_range(
        &mut self,
        venue: &str,
        symbol: &str,
        range: TimestampRange,
        limit: Option<u32>,
    ) -> Result<Vec<ExecFillRow>, String> {
        if !self.serves_execution_fills_range() {
            return Err(format!(
                "this datahub does not advertise `{FEATURE_SCAN_EXECUTION_FILLS_RANGE}` (advertised: \
                 {:?}) — nothing was sent: it would answer the whole exec-fill series and ignore the \
                 window. Read the whole series, or upgrade vike-datahub.",
                self.features()
            ));
        }
        self.execution_fills_request(Request::ScanExecFills {
            venue: venue.to_string(),
            symbol: symbol.to_string(),
            start: range.start,
            end: range.end,
            limit,
            columns: self.scan_columns_body(),
        })
    }

    /// The one send-and-match both fill reads share: as columns when the request asks them.
    fn execution_fills_request(&mut self, request: Request) -> Result<Vec<ExecFillRow>, String> {
        if matches!(request, Request::ScanExecFills { columns: Some(_), .. }) {
            return self.columns_rows(&request, &EXECUTION_FILL_COLUMNS);
        }
        match self.exchange_shown(&request)? {
            Response::ExecFills(rows) => Ok(rows),
            Response::Error(msg) => Err(msg),
            other => {
                Err(format!("protocol desync: expected ExecFills, got {}", response_kind(&other)))
            }
        }
    }

    /// Read the Tier-2 ORDER-LIFECYCLE log for `(venue, symbol)` over `range`, capped SOFTLY at
    /// `limit` — the RPC twin of `HistoryStore::scan_execution_orders_head`.
    ///
    /// ⚠ **Refuses LOCALLY, sending nothing, against a server that does not advertise
    /// [`FEATURE_SCAN_EXECUTION_ORDERS`]** (decision 0112's client leg): such a server would answer
    /// `Response::Error` for an unknown verb, and saying so here names the cause instead.
    pub fn scan_exec_orders(
        &mut self,
        venue: &str,
        symbol: &str,
        range: TimestampRange,
        limit: Option<u32>,
    ) -> Result<Vec<ExecOrderRow>, String> {
        if !self.serves_execution_orders() {
            return Err(format!(
                "this datahub does not advertise `{FEATURE_SCAN_EXECUTION_ORDERS}` (advertised: {:?}) — \
                 nothing was sent: it predates the order-history read. Upgrade vike-datahub.",
                self.features()
            ));
        }
        let columns = self.scan_columns_body();
        let request = Request::ScanExecOrders {
            venue: venue.to_string(),
            symbol: symbol.to_string(),
            start: range.start,
            end: range.end,
            limit,
            columns,
        };
        if columns.is_some() {
            return self.columns_rows(&request, &EXECUTION_ORDER_COLUMNS);
        }
        match self.exchange_shown(&request)? {
            Response::ExecOrders(rows) => Ok(rows),
            Response::Error(msg) => Err(msg),
            other => {
                Err(format!("protocol desync: expected ExecOrders, got {}", response_kind(&other)))
            }
        }
    }

    /// Whether this server advertised [`FEATURE_SCAN_EXECUTION_FILLS_RANGE`] — a windowed exec-fill read.
    pub fn serves_execution_fills_range(&self) -> bool {
        self.features().iter().any(|f| f == FEATURE_SCAN_EXECUTION_FILLS_RANGE)
    }

    /// Whether this server advertised [`FEATURE_SCAN_EXECUTION_ORDERS`] — the order-lifecycle read.
    pub fn serves_execution_orders(&self) -> bool {
        self.features().iter().any(|f| f == FEATURE_SCAN_EXECUTION_ORDERS)
    }

    /// Whether this server advertised [`FEATURE_EXECUTIONS_RECORDED`] — it records the live executions a
    /// tradehub streams. ⚠ `false` is AMBIGUOUS (an older build, or a store that refuses writes): that
    /// constant's doc says what a reader may conclude from it.
    pub fn serves_executions_recorded(&self) -> bool {
        self.features().iter().any(|f| f == FEATURE_EXECUTIONS_RECORDED)
    }

    /// Point-in-time properties for `(venue, symbol)` at or before `ts` — the RPC twin of
    /// `HistoryStore::properties_as_of`. `Ok(None)` = nothing recorded at or before `ts`.
    pub fn properties_as_of(
        &mut self,
        venue: &str,
        symbol: &str,
        ts: i64,
    ) -> Result<Option<SymbolProperties>, String> {
        let request = Request::PropertiesAsOf {
            venue: venue.to_string(),
            symbol: symbol.to_string(),
            timestamp: ts,
        };
        match self.exchange_shown(&request)? {
            // Unbox on the way out — the box was a server-side enum-size optimization only.
            Response::Properties(properties) => Ok(properties.map(|b| *b)),
            Response::Error(msg) => Err(msg),
            other => {
                Err(format!("protocol desync: expected Properties, got {}", response_kind(&other)))
            }
        }
    }

    /// Enumerate every stored series (`HistoryStore::list_series` over RPC).
    pub fn list_series(&mut self) -> Result<Vec<SeriesId>, String> {
        match self.exchange_shown(&Request::ListSeries)? {
            Response::SeriesList(v) => Ok(v),
            Response::Error(msg) => Err(msg),
            other => {
                Err(format!("protocol desync: expected SeriesList, got {}", response_kind(&other)))
            }
        }
    }

    /// Every stored series with its cheap coverage (`HistoryStore::inventory` over RPC).
    pub fn inventory(&mut self) -> Result<Vec<(SeriesId, SeriesCoverage)>, String> {
        match self.exchange_shown(&Request::Inventory)? {
            Response::Inventory(v) => Ok(v),
            Response::Error(msg) => Err(msg),
            other => {
                Err(format!("protocol desync: expected Inventory, got {}", response_kind(&other)))
            }
        }
    }

    /// Whether this server advertised [`FEATURE_EXECUTION_SERIES`] — the execution-history catalog of a window.
    pub fn serves_execution_series(&self) -> bool {
        self.features().iter().any(|f| f == FEATURE_EXECUTION_SERIES)
    }

    /// The execution-history series of `venue` and `symbol` whose span reaches `range`, with the filters'
    /// choices — the RPC twin of `HistoryStore::execution_series`. ⚠ Refused LOCALLY, sending nothing, unless
    /// the server advertised [`FEATURE_EXECUTION_SERIES`].
    pub fn execution_series(
        &mut self,
        venue: Option<&str>,
        symbol: Option<&str>,
        range: TimestampRange,
    ) -> Result<ExecutionSeries, String> {
        if !self.serves_execution_series() {
            return Err(format!(
                "this datahub does not advertise `{FEATURE_EXECUTION_SERIES}` (advertised: {:?}) — \
                 nothing was sent: it predates the execution-history catalog.",
                self.features()
            ));
        }
        let request = Request::ExecutionSeries {
            venue: venue.map(str::to_string),
            symbol: symbol.map(str::to_string),
            start: range.start,
            end: range.end,
        };
        match self.exchange_shown(&request)? {
            Response::ExecutionSeries(answer) => Ok(answer),
            Response::Error(msg) => Err(msg),
            other => Err(format!(
                "protocol desync: expected ExecutionSeries, got {}",
                response_kind(&other)
            )),
        }
    }

    /// The UTC days a read of `range` reads, each stamped — the RPC twin of `HistoryStore::series_days`. A BAR read
    /// (`kind` [`StoreKindId::Bar`]) crosses with no `kind`, the frame it always was. `Ok(None)`, sending nothing,
    /// from a datahub that does not advertise [`FEATURE_SERIES_DAYS`] (it predates the verb), and for any other kind
    /// from one that does not advertise [`FEATURE_SERIES_DAYS_KINDS`]: it would skip the unknown `kind` and answer the
    /// BAR days. A reader then keeps nothing.
    pub fn series_days(
        &mut self,
        kind: StoreKindId,
        venue: &str,
        source: Option<&str>,
        symbol: &str,
        interval: Option<&str>,
        range: TimestampRange,
    ) -> Result<Option<Vec<BarDay>>, String> {
        let advertises = |feature: &str| self.features().iter().any(|f| f == feature);
        if !advertises(FEATURE_SERIES_DAYS) {
            return Ok(None);
        }
        let kind = match kind {
            StoreKindId::Bar => None,
            other if advertises(FEATURE_SERIES_DAYS_KINDS) => Some(other),
            _ => return Ok(None),
        };
        let request = Request::SeriesDays {
            venue: venue.to_string(),
            source: source.map(str::to_string),
            symbol: symbol.to_string(),
            interval: interval.map(str::to_string),
            start: range.start,
            end: range.end,
            kind,
        };
        match self.exchange_shown(&request)? {
            Response::SeriesDays(days) => Ok(days),
            Response::Error(msg) => Err(msg),
            other => {
                Err(format!("protocol desync: expected SeriesDays, got {}", response_kind(&other)))
            }
        }
    }

    /// One series' coverage plus its commit keys — the RPC twin of `HistoryStore::series_facts`.
    ///
    /// ⚠ 0084's SEVENTH verb (that record counted the TRAIT's six; this one was inherent to
    /// `HistoryStoreDataFusion`). Without it a wire-routed backtest loses its data fingerprint and still
    /// reads green.
    pub fn series_facts(&mut self, id: &SeriesId) -> Result<(SeriesCoverage, Vec<String>), String> {
        let request = Request::SeriesFacts { id: id.clone() };
        match self.exchange_shown(&request)? {
            Response::SeriesFacts(f) => Ok(*f),
            Response::Error(msg) => Err(msg),
            other => {
                Err(format!("protocol desync: expected SeriesFacts, got {}", response_kind(&other)))
            }
        }
    }

    /// The gap ranges in one series' recorded span (`HistoryStore::series_gaps` over RPC).
    pub fn series_gaps(&mut self, id: &SeriesId) -> Result<Vec<(i64, i64)>, String> {
        let request = Request::SeriesGaps { id: id.clone() };
        match self.exchange_shown(&request)? {
            Response::SeriesGaps(v) => Ok(v),
            Response::Error(msg) => Err(msg),
            other => {
                Err(format!("protocol desync: expected SeriesGaps, got {}", response_kind(&other)))
            }
        }
    }

    /// The CROSS-KIND coverage report (`HistoryStore::coverage_report` over RPC): every instrument with
    /// its `trade`/`quote`/`book`/`depth` day sets lined up, which is what the Data Manager's
    /// "Partial" column folds
    /// ([`InstrumentCoverage::partial_days`](vike_data::InstrumentCoverage::partial_days)).
    ///
    /// ⚠ Refused CLIENT-SIDE, sending nothing, unless the server advertised [`FEATURE_COVERAGE`].
    /// EVERY server built from this protocol's `serve` advertises it (a plain trait verb, not a
    /// mounted table), so a refusal means exactly one thing: the peer predates the verb. The caller
    /// renders that as an honest note, never an empty column — an empty `Ok` would say "nothing is
    /// partial", a different and possibly false fact.
    pub fn coverage_report(&mut self) -> Result<Vec<InstrumentCoverage>, String> {
        if !self.features().iter().any(|f| f == FEATURE_COVERAGE) {
            return Err(format!(
                "datahub server does not advertise `{FEATURE_COVERAGE}` (advertised: {:?}) — \
                 nothing was sent. The cross-kind coverage report needs a newer server; this verb \
                 is capability-negotiated, not version-gated.",
                self.features()
            ));
        }
        match self.exchange_shown(&Request::Coverage)? {
            Response::Coverage(v) => Ok(v),
            Response::Error(msg) => Err(msg),
            other => {
                Err(format!("protocol desync: expected Coverage, got {}", response_kind(&other)))
            }
        }
    }
}
