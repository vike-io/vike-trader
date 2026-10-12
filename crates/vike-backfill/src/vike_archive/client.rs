//! The blocking `ArchiveClient`: manifest, discovery, the `--dry-run` plan and the HTTP ingest entrypoints.

use std::collections::HashSet;

use datafusion::parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use vike_data::store::archive_store::select_row_groups;
use vike_data::{BulkIngestSession, DataFusionHist};

use super::datasets::{
    Dataset, DatasetFilter, DatasetsPage, FamilyTarget, Manifest, Stream, family_stream_url,
    manifest_url, parse_dataset, parse_datasets_page, parse_manifest, stream_url,
};
use super::readers::HttpRangeReader;
use super::{
    API_KEY_HEADER, CTX, PROGRESS_LOG_EVERY, RowGroupProgress, ingest_stream_bulk_over,
    ingest_stream_over,
};
use crate::error::CollectError;

/// Pure query-string builder for [`DatasetFilter`] — unit-tested without network. Every field
/// `None` builds an empty string (the unfiltered listing), never a bare `?`.
fn datasets_query(f: &DatasetFilter) -> String {
    let mut parts = Vec::new();
    if let Some(a) = &f.asset {
        parts.push(format!("asset={a}"));
    }
    if let Some(t) = &f.tenor {
        parts.push(format!("tenor={t}"));
    }
    if let Some(s) = f.stream {
        parts.push(format!("stream={}", s.file_name()));
    }
    if let Some(v) = &f.from {
        parts.push(format!("from={v}"));
    }
    if let Some(v) = &f.to {
        parts.push(format!("to={v}"));
    }
    if let Some(v) = f.limit {
        parts.push(format!("limit={v}"));
    }
    if let Some(v) = f.offset {
        parts.push(format!("offset={v}"));
    }
    if parts.is_empty() { String::new() } else { format!("?{}", parts.join("&")) }
}

/// Pure query-string builder for `GET /v1/archive/url`: `date`+`stream` always present;
/// `asset`+`tenor` both appended when `Some` (a family resolve), both omitted otherwise (a flat
/// resolve) — never a partial pair, matching the server's own "both or neither" contract.
fn resolve_query(date: &str, stream: Stream, asset: Option<&str>, tenor: Option<&str>) -> String {
    let mut parts = vec![format!("date={date}"), format!("stream={}", stream.file_name())];
    if let (Some(a), Some(t)) = (asset, tenor) {
        parts.push(format!("asset={a}"));
        parts.push(format!("tenor={t}"));
    }
    format!("?{}", parts.join("&"))
}

// ---- the HTTP client + ingest entrypoint ---------------------------------------------------------

/// Row-group pruning stats for one (date, stream) — the `--dry-run` "how many row groups would
/// actually be read vs the full file" report.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PrunePlan {
    pub total_row_groups: usize,
    pub selected_row_groups: usize,
    pub total_compressed_bytes: i64,
    pub selected_compressed_bytes: i64,
}

/// Blocking client over one archive base URL + optional API key. Cheap to construct (the
/// `ureq::Agent` is pool-backed); typically one instance per CLI run.
pub struct ArchiveClient {
    agent: ureq::Agent,
    base: String,
    api_key: Option<String>,
}

impl ArchiveClient {
    /// `api_key` rides every request this client makes (harmless on the open `manifest.json`/
    /// `samples/` endpoints; required on the date-partitioned files). Callers resolve
    /// `VIKE_ARCHIVE_API_KEY` themselves (see the module doc) — this constructor never touches
    /// the environment.
    pub fn new(base: impl Into<String>, api_key: Option<String>) -> Self {
        // The collectors' one agent: statuses are read by us (a 401/404 is reported with context,
        // not turned into an opaque transport error) and the only bounds are connect + per-read
        // idle, never a whole-request ceiling (the archive parts are large). Its module doc is the
        // authority.
        let agent = crate::http::collector_agent();
        Self { agent, base: base.into(), api_key }
    }

    /// Shared authenticated GET-and-read-body helper: the `X-API-Key` header, status check, body
    /// read — factored out so [`Self::fetch_manifest`] and the new `/v1/` discovery calls
    /// ([`Self::discover_datasets`]/[`Self::resolve_dataset`]) reuse one error-shape convention
    /// instead of three copies of it.
    fn get_json(&self, url: &str) -> Result<String, CollectError> {
        let mut req = self.agent.get(url);
        if let Some(k) = &self.api_key {
            req = req.header(API_KEY_HEADER, k);
        }
        let mut resp =
            req.call().map_err(|e| CollectError::Fetch(format!("{CTX} GET {url}: {e}")))?;
        let status = resp.status().as_u16();
        let body = resp
            .body_mut()
            .read_to_string()
            .map_err(|e| CollectError::Fetch(format!("{CTX} read {url}: {e}")))?;
        if !(200..300).contains(&status) {
            let head: String = body.chars().take(200).collect();
            return Err(CollectError::Fetch(format!("{CTX} GET {url}: HTTP {status}: {head}")));
        }
        Ok(body)
    }

    /// GET the open `manifest.json` and parse it.
    pub fn fetch_manifest(&self) -> Result<Manifest, CollectError> {
        let body = self.get_json(&manifest_url(&self.base))?;
        parse_manifest(&body)
    }

    /// `GET {discovery_base}/v1/archive/datasets{filter}` — the discovery listing, spanning BOTH
    /// on-disk layouts (a flat row omits `asset`/`tenor`; a family row carries both). Prefer this
    /// over guessing either layout's path shape by hand — see this module's "v1 discovery API"
    /// section doc for why.
    pub fn discover_datasets(
        &self,
        discovery_base: &str,
        filter: &DatasetFilter,
    ) -> Result<DatasetsPage, CollectError> {
        let url = format!("{discovery_base}/v1/archive/datasets{}", datasets_query(filter));
        let body = self.get_json(&url)?;
        parse_datasets_page(&body)
    }

    /// `GET {discovery_base}/v1/archive/url?date=...&stream=...[&asset=...&tenor=...]` — resolves
    /// EXACTLY one dataset. `asset`+`tenor` both `Some` resolves the family layout; both `None`
    /// resolves flat; the server itself rejects a partial pair as ambiguous (400 -> a plain `Err`
    /// here, never guessed at client-side).
    pub fn resolve_dataset(
        &self,
        discovery_base: &str,
        date: &str,
        stream: Stream,
        asset: Option<&str>,
        tenor: Option<&str>,
    ) -> Result<Dataset, CollectError> {
        let url =
            format!("{discovery_base}/v1/archive/url{}", resolve_query(date, stream, asset, tenor));
        let body = self.get_json(&url)?;
        parse_dataset(&body)
    }

    /// Resolve the URL to fetch for ONE family (asset, tenor) dataset: discovery first (the server
    /// is the single source of truth for the layout — see the module doc), falling back to
    /// [`family_stream_url`]'s directly-constructed path ONLY when the discovery call itself fails
    /// outright (network error, non-2xx, unparsable body) — never on a discovery response that
    /// came back clean saying "no such dataset" (that stays a hard `Err`, surfaced by the caller
    /// trying to open the resulting URL and getting a real 404/401 back).
    fn resolve_family_url(&self, target: &FamilyTarget<'_>, date: &str, stream: Stream) -> String {
        let FamilyTarget { discovery_base, asset, tenor } = *target;
        match self.resolve_dataset(discovery_base, date, stream, Some(asset), Some(tenor)) {
            Ok(d) => d.url,
            Err(e) => {
                tracing::warn!(
                    %date, kind = stream.kind().as_str(), %asset, %tenor,
                    "vike-archive: discovery resolve failed ({e}), falling back to the \
                     directly-constructed family URL"
                );
                family_stream_url(&self.base, asset, tenor, date, stream)
            }
        }
    }

    /// Open a footer-only `ParquetRecordBatchReaderBuilder` over one (date, stream) — a `HEAD` +
    /// the footer's two ranged GETs (~1.17 MB total, verified), never the row data.
    fn open_builder(
        &self,
        date: &str,
        stream: Stream,
    ) -> Result<ParquetRecordBatchReaderBuilder<HttpRangeReader>, CollectError> {
        let url = stream_url(&self.base, date, stream);
        let reader = HttpRangeReader::open(self.agent.clone(), url.clone(), self.api_key.clone())?;
        ParquetRecordBatchReaderBuilder::try_new(reader).map_err(|e| {
            CollectError::Fetch(format!("{CTX} parquet open {date}/{}: {e}", stream.file_name()))
        })
    }

    /// The `--dry-run` row-group-pruning rehearsal for one (date, stream): opens the footer only
    /// (no row data), computes which row groups `tokens` would select, and reports the compressed
    /// bytes of the full file vs the selected subset.
    pub fn plan_stream(
        &self,
        date: &str,
        stream: Stream,
        tokens: Option<&HashSet<String>>,
    ) -> Result<PrunePlan, CollectError> {
        let builder = self.open_builder(date, stream)?;
        let md = builder.metadata();
        let total_row_groups = md.num_row_groups();
        let selected = select_row_groups(md, tokens);
        let total_compressed_bytes: i64 =
            (0..total_row_groups).map(|i| md.row_group(i).compressed_size()).sum();
        let selected_compressed_bytes: i64 =
            selected.iter().map(|&i| md.row_group(i).compressed_size()).sum();
        Ok(PrunePlan {
            total_row_groups,
            selected_row_groups: selected.len(),
            total_compressed_bytes,
            selected_compressed_bytes,
        })
    }

    /// Ingest one (date, stream) into `store` under `venue=polymarket`, STREAMING one Parquet row
    /// group at a time (see the module doc's "Streaming ingest" section) — memory stays bounded by
    /// one row group's decoded rows regardless of how many days/tokens/row groups the whole run
    /// spans. `tokens` — when `Some` — first prunes row groups, then filters individual decoded rows
    /// (row-group pruning is conservative: a kept group may still carry OTHER tokens' rows mixed
    /// in). Idempotent per `vikearchive:{kind}:{date}:rg{idx}` commit keys (one per row group, NOT
    /// one per date — see [`ingest_stream_over`]'s doc for why). Logs progress at INFO periodically
    /// (`PROGRESS_LOG_EVERY` row groups, plus the first and last) via `tracing`. Returns total rows
    /// written across every selected row group.
    pub fn ingest_stream(
        &self,
        store: &DataFusionHist,
        date: &str,
        stream: Stream,
        tokens: Option<&HashSet<String>>,
    ) -> Result<usize, CollectError> {
        let url = stream_url(&self.base, date, stream);
        let reader = HttpRangeReader::open(self.agent.clone(), url, self.api_key.clone())?;
        let kind = stream.kind().as_str();
        ingest_stream_over(reader, store, date, stream, tokens, |p: RowGroupProgress| {
            let is_boundary = p.index == 0 || p.index + 1 == p.total;
            if is_boundary || (p.index + 1).is_multiple_of(PROGRESS_LOG_EVERY) {
                tracing::info!(
                    %date,
                    kind,
                    row_group = p.index + 1,
                    total_row_groups = p.total,
                    bytes_this_group = p.compressed_bytes,
                    rows_this_group = p.rows_this_group,
                    rows_cumulative = p.rows_cumulative,
                    elapsed_ms = p.elapsed_ms,
                    "vike-archive: ingest progress"
                );
            }
        })
    }

    /// BULK/offline write-profile twin of [`Self::ingest_stream`] — same row-group-at-a-time decode,
    /// but instead of committing directly it STAGES each row group's decoded rows into `session`
    /// (a caller-owned [`BulkIngestSession`], see `vike_data::bulk`'s module doc), which collapses
    /// many row groups' worth of one series' rows into ONE commit per window instead of one commit
    /// per row group per symbol — the fix the ingest-profile measurement ranked #1 (see this
    /// module's doc). `key_prefix` becomes `{key_prefix}:w{N}` on whichever flush(es) this call's
    /// row groups happen to trigger — pass the SAME prefix across a re-run of the same (date,
    /// stream, token-filter) invocation so a crash-recovered retry lands on the same window
    /// boundaries and skips (no-ops) whatever already committed. The caller MUST call
    /// `session.flush(key_prefix)` once more after every stream in a run to durably commit the
    /// final partial window — this function does not do so itself (a caller ingesting several
    /// streams into ONE session may prefer to keep batching across streams rather than force a
    /// flush boundary between them). Returns total DECODED rows across every selected row group
    /// (matching [`Self::ingest_stream`]'s "rows written" contract as closely as a staged, not yet
    /// necessarily flushed, count can — see [`BulkIngestSession::total_rows_written`] for what has
    /// actually landed durably so far).
    pub fn ingest_stream_bulk(
        &self,
        session: &mut BulkIngestSession<'_>,
        date: &str,
        stream: Stream,
        tokens: Option<&HashSet<String>>,
        key_prefix: &str,
    ) -> Result<usize, CollectError> {
        let url = stream_url(&self.base, date, stream);
        let reader = HttpRangeReader::open(self.agent.clone(), url, self.api_key.clone())?;
        let kind = stream.kind().as_str();
        ingest_stream_bulk_over(
            reader,
            session,
            date,
            stream,
            tokens,
            key_prefix,
            |p: RowGroupProgress| {
                let is_boundary = p.index == 0 || p.index + 1 == p.total;
                if is_boundary || (p.index + 1).is_multiple_of(PROGRESS_LOG_EVERY) {
                    tracing::info!(
                        %date,
                        kind,
                        row_group = p.index + 1,
                        total_row_groups = p.total,
                        bytes_this_group = p.compressed_bytes,
                        rows_this_group = p.rows_this_group,
                        rows_cumulative = p.rows_cumulative,
                        elapsed_ms = p.elapsed_ms,
                        "vike-archive: bulk ingest progress (staged; not necessarily flushed yet)"
                    );
                }
            },
        )
    }

    /// The FAMILY-layout twin of [`Self::ingest_stream`]: same memory-bounded row-group-at-a-time
    /// streaming decode, but the URL is resolved via [`Self::resolve_family_url`] (discovery
    /// first, the directly-constructed [`family_stream_url`] as a fallback) instead of the flat
    /// [`stream_url`] builder. `tokens` behaves identically to the flat path (row-group pruning
    /// then per-row filtering); commit keys stay `vikearchive:{kind}:{date}:rg{idx}` — the SAME
    /// idempotency scheme, since a family file's row groups need exactly the same per-group
    /// commit-key granularity for the reason [`ingest_stream_over`]'s doc explains (one symbol can
    /// straddle two adjacent row groups).
    pub fn ingest_family_stream(
        &self,
        store: &DataFusionHist,
        target: &FamilyTarget<'_>,
        date: &str,
        stream: Stream,
        tokens: Option<&HashSet<String>>,
    ) -> Result<usize, CollectError> {
        let FamilyTarget { asset, tenor, .. } = *target;
        let url = self.resolve_family_url(target, date, stream);
        let reader = HttpRangeReader::open(self.agent.clone(), url, self.api_key.clone())?;
        let kind = stream.kind().as_str();
        ingest_stream_over(reader, store, date, stream, tokens, |p: RowGroupProgress| {
            let is_boundary = p.index == 0 || p.index + 1 == p.total;
            if is_boundary || (p.index + 1).is_multiple_of(PROGRESS_LOG_EVERY) {
                tracing::info!(
                    %date,
                    kind,
                    %asset,
                    %tenor,
                    row_group = p.index + 1,
                    total_row_groups = p.total,
                    bytes_this_group = p.compressed_bytes,
                    rows_this_group = p.rows_this_group,
                    rows_cumulative = p.rows_cumulative,
                    elapsed_ms = p.elapsed_ms,
                    "vike-archive: family ingest progress"
                );
            }
        })
    }

    /// The FAMILY-layout twin of [`Self::ingest_stream_bulk`] — same relationship
    /// [`Self::ingest_family_stream`] has to [`Self::ingest_stream`]: URL resolution goes through
    /// discovery-then-fallback instead of the flat builder; everything else (staging into
    /// `session`, the caller's `flush` obligation, the crash-safety contract) is identical.
    pub fn ingest_family_stream_bulk(
        &self,
        session: &mut BulkIngestSession<'_>,
        target: &FamilyTarget<'_>,
        date: &str,
        stream: Stream,
        tokens: Option<&HashSet<String>>,
        key_prefix: &str,
    ) -> Result<usize, CollectError> {
        let FamilyTarget { asset, tenor, .. } = *target;
        let url = self.resolve_family_url(target, date, stream);
        let reader = HttpRangeReader::open(self.agent.clone(), url, self.api_key.clone())?;
        let kind = stream.kind().as_str();
        ingest_stream_bulk_over(
            reader,
            session,
            date,
            stream,
            tokens,
            key_prefix,
            |p: RowGroupProgress| {
                let is_boundary = p.index == 0 || p.index + 1 == p.total;
                if is_boundary || (p.index + 1).is_multiple_of(PROGRESS_LOG_EVERY) {
                    tracing::info!(
                        %date,
                        kind,
                        %asset,
                        %tenor,
                        row_group = p.index + 1,
                        total_row_groups = p.total,
                        bytes_this_group = p.compressed_bytes,
                        rows_this_group = p.rows_this_group,
                        rows_cumulative = p.rows_cumulative,
                        elapsed_ms = p.elapsed_ms,
                        "vike-archive: family bulk ingest progress (staged; not necessarily \
                         flushed yet)"
                    );
                }
            },
        )
    }
}

#[cfg(test)]
#[path = "client_tests.rs"]
mod client_tests;
