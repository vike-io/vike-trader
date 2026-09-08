//! `data.vike.io` archive backfill — Polymarket full L2 (`book_events`), taker tape (`trades`),
//! and derived top-of-book (`l1_quotes`), one UTC day per Hive-style Parquet partition:
//! `{base}/venue=polymarket/date=YYYY-MM-DD/{book_events,trades,l1_quotes}.parquet`, plus an open
//! `manifest.json` (no key) listing available dates + per-stream schema + sizes. Auth is a single
//! `X-API-Key` header (workspace `.env` key `VIKE_ARCHIVE_API_KEY` — this module takes it as a
//! plain `Option<String>` parameter; ONLY the `vike_archive_backfill` bin reads the env/`.env`, per
//! the repo's "libraries take config as parameters" rule).
//!
//! **The differentiator: ranged-HTTP row-group pruning, not whole-file download.** A `book_events`
//! day runs ~7.4 GB (465M rows, 453 row groups per the vendor's own numbers), but the file is
//! served with `Accept-Ranges: bytes` (verified live: HTTP 206 on an explicit `Range` header AND on
//! a suffix range, `Content-Range` echoes the true total size) and rows are CLUSTERED by
//! `token_id` with real per-row-group min/max statistics on that column. [`HttpRangeReader`]
//! implements `parquet::file::reader::ChunkReader` (this workspace's pinned `parquet` — reached via
//! `datafusion::parquet`, ONE arrow version, no new dep — is 58.3.0, whose `ChunkReader` trait is
//! exactly the "footer-then-column-chunks" random-access seam the `parquet` crate itself was built
//! around): `ParquetRecordBatchReaderBuilder::try_new` calls `get_bytes` itself to pull the footer
//! (last 8 bytes → footer length → footer, verified ~1.17 MB in 2 range reads) and, after
//! `.with_row_groups(selected)`, only the column chunks of the SELECTED row groups — never the
//! whole file. **No whole-file-download fallback was needed**: this trait is directly implementable
//! over a plain blocking ranged GET, so the "if impractical, fall back" contingency in this
//! feature's design brief did not trigger; a filtered single-token scan costs one row group
//! (tens of MB) rather than the whole day.
//!
//! [`select_row_groups`] does the pruning: for each row group, if the `token_id` column's own
//! min/max statistics (byte-lexicographic — the same comparison Parquet's own stats are defined
//! over for `BYTE_ARRAY`/`Utf8` columns, and the ONLY one that stays correct regardless of the
//! file's actual sort order, since a row group's min/max are always true bounds over that group's
//! own rows) bracket ANY requested token's bytes, the group is kept; a missing/absent-stats column
//! is included defensively (never silently drops rows). Column pruning (skipping `condition_id`/
//! `is_snapshot`, which this port never decodes) is a further available optimization NOT
//! implemented here — every row group read still pulls every column's chunk; scope was kept to the
//! row-group axis, the documented differentiator.
//!
//! Real physical Parquet schema (verified against the vendor's own free daily sample at
//! `/archive/samples/*.parquet` — NOT the manifest's ClickHouse-logical type names, which differ
//! from what actually lands in Arrow):
//!
//! | stream | column | Arrow physical type | vike_model field | notes |
//! |---|---|---|---|---|
//! | book_events | `token_id` | `Utf8` | `BookUpdate.symbol` | per-row — a scan mixes many tokens |
//! | book_events | `ts` / `local_ts` | `Int64` | `.ts` / `.local_ts` | |
//! | book_events | `seq` | `UInt64` | `.seq` | |
//! | book_events | `event_type` | **`Binary`** (NOT Utf8 — ClickHouse `Enum8` exports as raw bytes) | → `BookUpdateKind` | `book`→Snapshot, `price_change`→Delta, `status`→Gap/Stale/LiveResume via `status`, `trade`/`tick_size_change` skipped |
//! | book_events | `side` | **`Binary`** | delta bids/asks placement | `buy`→bids, `sell`→asks (taker-side convention) |
//! | book_events | `price` | `Decimal128(9,4)` | level price | `÷1e4` |
//! | book_events | `size` | `Decimal128(18,6)` | level size | `÷1e6` |
//! | book_events | `bids` / `asks` | `Utf8` | snapshot levels | plain-number JSON `[[price,size],...]` (NOT pmxt's stringified variant) |
//! | book_events | `tick_size` | `Decimal128(9,4)` | `.tick_size` | `÷1e4` |
//! | book_events | `status` | `Utf8` | gap/stale/live_resume label | only read on `event_type="status"` |
//! | trades | `token_id` | `Utf8` | `TradeTick.symbol` | |
//! | trades | `ts` / `local_ts` | `Int64` | `.ts` / `.local_ts` | |
//! | trades | `price` / `size` | **`Float64`** (already double — not Decimal here) | `.price` / `.size` | |
//! | trades | `side` | **`Utf8`** (plain string here, unlike book_events' enum) | `.is_buyer_maker` | taker side: `"sell"` → `true` |
//! | l1_quotes | `token_id` | `Utf8` | `QuoteTick.symbol` | |
//! | l1_quotes | `ts` / `local_ts` | `Int64` | `.ts` / `.local_ts` | |
//! | l1_quotes | `bid` / `ask` | `Decimal128(9,4)` | `.bid` / `.ask` | `÷1e4` |
//! | l1_quotes | `bid_size` / `ask_size` | `Decimal128(18,6)` | `.bid_size` / `.ask_size` | `÷1e6` |
//!
//! `condition_id`/`is_snapshot` exist on the wire but are never decoded (no `vike_model` field
//! needs them). Ingest is idempotent per `vikearchive:{kind}:{date}:rg{row_group_idx}` commit keys
//! (mirrors pmxt's `pmxt:{kind}:{asset}:{hour}` idiom, but WITHOUT the per-symbol component — safe
//! because `HistStore` commit-key dedup is scoped to the series directory, i.e. per
//! `(venue, symbol, kind)` already, so the same key reused across every token_id in one row group
//! collides with nothing). The row-group suffix (rather than one key per whole date) is
//! load-bearing for the streaming design below, not cosmetic — see [`ingest_stream_over`]'s doc.
//!
//! **Streaming ingest (memory-bounded, not whole-file buffered).** [`ArchiveClient::ingest_stream`]
//! processes row groups ONE AT A TIME: for each selected row-group index, it builds a fresh
//! `ParquetRecordBatchReader` scoped to JUST that group (via `.with_row_groups(vec![idx])`, reusing
//! one cached `ArrowReaderMetadata` footer so re-scoping never re-fetches it), decodes it, appends
//! its rows to the `HistStore` immediately, and drops everything before moving to the next index —
//! so resident memory is bounded by one row group's decoded rows (tens of MB per the module intro),
//! never by the whole file or the whole date range. This replaced an earlier version that grouped
//! EVERY row of the ENTIRE reader into one `HashMap<String, Vec<_>>` before writing anything —
//! `per_symbol` was populated by draining the full `for batch in reader` loop first, so nothing hit
//! disk and resident memory scaled with (row groups × rows) until the whole stream had been decoded
//! (measured live: a ~9.3 GB-compressed two-day book_events pull grew RSS to 6 GB with the store at
//! 0 bytes on disk for 11+ minutes). The per-row-group commit key is the necessary
//! consequence: a single shared per-date key would make every write AFTER the first one for a given
//! symbol a silent no-op (the store's `commit_rows` treats a repeated commit key as "already done,
//! Ok(0)" — see `vike-data`'s `datafusion_hist.rs`), since one token's rows can straddle two
//! adjacent row groups in a sorted-by-token_id file. Net effect for a fully-completed run is
//! identical rows in the store either way; the finer key also makes a partial run resumable
//! row-group-by-row-group rather than only date-by-date.
//!
//! **Parallel local-file ingest (`ingest_local_file_parallel`).** The `--file` path (measured:
//! 1m44.8s / 98% CPU / one of 32 cores / 500 MB peak RSS for one BTC-5m `book_events` day,
//! 74.4M rows) is strictly SERIAL — no network latency left to hide behind (that was `--file`'s
//! own fix), so the remaining wall-clock is decode+encode CPU, one core's worth. This function
//! fans the SAME row-group-at-a-time pipeline (`ingest_book`/`ingest_trades`/`ingest_quotes`,
//! identical decode + commit-key logic) across up to `jobs` OS threads via a dedicated
//! `rayon::ThreadPool` (never the global pool — width is an explicit CLI knob, `--jobs N` on
//! `vike_archive_backfill`, not `RAYON_NUM_THREADS`). [`chunk_row_groups`] splits the selected
//! row groups into `jobs` CONTIGUOUS chunks, one per worker, preserving the file's own
//! `token_id`-sorted row-group order within each chunk: since a token can straddle at most two
//! ADJACENT row groups (never more — see the streaming-ingest paragraph above), a contiguous
//! split means only the `jobs - 1` chunk-boundary tokens can ever see writes from two different
//! workers: every other token's rows are written by exactly one worker end-to-end, so
//! `DataFusionHist`'s per-series file lock (`SeriesLock`, scoped to one `kind=/venue=/symbol=`
//! directory) sees real cross-worker contention only at those few boundaries, not on every
//! commit. Memory stays bounded by `jobs` — the pool has EXACTLY `jobs` threads, each holding at
//! most one row group's decoded rows at a time (the same one-row-group bound the serial path
//! holds), so at most `jobs` row groups are ever resident together; `jobs <= 1` degenerates to a
//! single chunk covering every row group on one thread, decoding/committing in the EXACT same
//! order as [`ingest_local_file`] (byte-identical store output). Progress callbacks fire in
//! COMPLETION order across workers, not file order — [`RowGroupProgress::index`]'s doc covers
//! the distinction. See `.superpowers/sdd/2026-07-28-poly-mm-latency-batch/
//! ingest-parallel-report.md` for the measured scaling and where it plateaus.
//!
//! **A real concurrency bug this parallelization surfaced (fixed, not theoretical):**
//! [`LocalFileReader`] originally delegated straight to `parquet::file::reader`'s own
//! `impl ChunkReader for File`, which reads via `self.try_clone()?.seek(start)?.read(...)`. That
//! is safe SERIALLY (one row group's reads always finish before the next starts) but NOT under
//! concurrency: `File::try_clone` shares the underlying open-file-description's cursor with the
//! original handle, so two worker threads racing a `seek`+`read` pair over clones of the ONE
//! `Arc<File>` this type wraps can splice each other's reads — one thread's `seek` moves the
//! cursor a second thread is mid-`read` against. This reproduced live on the CI box the first time
//! `ingest_local_file_parallel` ran with `--jobs 4`: a corrupted read fed the zstd decoder garbage
//! and decode failed with "External: Unknown frame descriptor" (never seen at `--jobs 1`). Fixed
//! by reading via `read_at`/`seek_read` (true positional syscalls — see [`read_at`] and
//! [`PositionedReader`]) instead, which never touch the shared cursor at all.

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{Cursor, Read};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use bytes::Bytes;
use datafusion::arrow::array::{
    BinaryArray, Decimal128Array, Float64Array, Int64Array, StringArray, UInt64Array,
};
use datafusion::arrow::record_batch::RecordBatch;
use datafusion::parquet::arrow::arrow_reader::{
    ArrowReaderMetadata, ArrowReaderOptions, ParquetRecordBatchReader,
    ParquetRecordBatchReaderBuilder,
};
use datafusion::parquet::errors::ParquetError;
use datafusion::parquet::file::metadata::ParquetMetaData;
use datafusion::parquet::file::reader::{ChunkReader, Length};
use rayon::ThreadPoolBuilder;
use rayon::prelude::*;

use vike_data::{BulkIngestSession, DataFusionHist, HistStore};
use vike_model::{BookUpdate, BookUpdateKind, QuoteTick, TradeTick};

use crate::error::CollectError;

/// The only venue this archive publishes.
pub const VENUE: &str = "polymarket";
/// The default `data.vike.io` archive base (no trailing slash).
pub const DEFAULT_BASE: &str = "https://data.vike.io/archive";
/// The auth header name (verified against the live manifest's own `auth.header` field).
const API_KEY_HEADER: &str = "X-API-Key";

/// `book_events`' fixed decimal scales (spec-fixed per the archive's published schema, mirroring
/// pmxt's own `PRICE_SCALE_DIVISOR`/`SIZE_SCALE_DIVISOR` convention in `crate::pmxt::ingest`):
/// `price`/`best_bid`/`best_ask`/`tick_size` are scale-4, `size` is scale-6. `l1_quotes` reuses the
/// same two divisors for `bid`/`ask` (scale-4) and `bid_size`/`ask_size` (scale-6).
const PRICE_SCALE_DIVISOR: f64 = 10_000.0;
const SIZE_SCALE_DIVISOR: f64 = 1_000_000.0;

/// Vendor prefix for error messages (mirrors `crate::arrowutil`'s `ctx` convention).
const CTX: &str = "vike-archive";

// ---- stream naming / URL builders --------------------------------------------------------------

/// One of the archive's three per-date Parquet streams.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Stream {
    Book,
    Trade,
    Quote,
}

impl Stream {
    /// The Parquet file's base name under `venue=polymarket/date=.../`.
    pub fn file_name(self) -> &'static str {
        match self {
            Stream::Book => "book_events",
            Stream::Trade => "trades",
            Stream::Quote => "l1_quotes",
        }
    }

    /// The `HistStore`/commit-key `kind` label this stream ingests as.
    pub fn kind(self) -> &'static str {
        match self {
            Stream::Book => "book",
            Stream::Trade => "trade",
            Stream::Quote => "quote",
        }
    }

    /// Parse a `--kind`-style label (`book`/`trade`/`quote`) — the CLI's own vocabulary, the
    /// inverse of [`Stream::kind`].
    pub fn from_kind(s: &str) -> Option<Self> {
        match s {
            "book" => Some(Stream::Book),
            "trade" => Some(Stream::Trade),
            "quote" => Some(Stream::Quote),
            _ => None,
        }
    }

    /// All three streams, in a fixed order — the `--kind all` expansion.
    pub fn all() -> [Stream; 3] {
        [Stream::Book, Stream::Trade, Stream::Quote]
    }
}

/// The full URL of one date/stream's Parquet partition.
pub fn stream_url(base: &str, date: &str, stream: Stream) -> String {
    format!("{base}/venue={VENUE}/date={date}/{}.parquet", stream.file_name())
}

/// The open (no key) manifest URL.
pub fn manifest_url(base: &str) -> String {
    format!("{base}/manifest.json")
}

// ---- manifest (pure parse; no I/O) -------------------------------------------------------------

/// One stream's advertised size within a manifest date entry. `rows` is `null` in the real manifest
/// today (vendor doesn't publish row counts), hence `Option`.
#[derive(Debug, Clone, PartialEq, serde::Deserialize)]
pub struct StreamInfo {
    pub bytes: u64,
    #[serde(default)]
    pub rows: Option<u64>,
}

/// One available UTC day.
#[derive(Debug, Clone, PartialEq, serde::Deserialize)]
pub struct ManifestDate {
    pub date: String,
    pub streams: HashMap<String, StreamInfo>,
}

/// The archive manifest — only the fields this crate consumes are declared; unknown top-level keys
/// (`schema`, `auth`, `free_sample`, `partitioning`, `generated_at`, `note`, …) are ignored by
/// serde's default (non-`deny_unknown_fields`) behavior, so a manifest schema addition never breaks
/// this parser.
#[derive(Debug, Clone, PartialEq, serde::Deserialize)]
pub struct Manifest {
    pub venue: String,
    pub dates: Vec<ManifestDate>,
    #[serde(default)]
    pub range_requests: bool,
}

/// Pure JSON→`Manifest` decode (no I/O) — fixture-tested against a trimmed real-shape sample below.
pub fn parse_manifest(json: &str) -> Result<Manifest, CollectError> {
    serde_json::from_str(json)
        .map_err(|e| CollectError::Fetch(format!("{CTX} manifest parse: {e}")))
}

// ---- v1 discovery API (family + flat layouts; the PREFERRED way to resolve a dataset URL) -------
//
// `data.vike.io` now publishes a second on-disk layout alongside the original flat one this module
// started with: `venue=polymarket/asset=<asset>/tenor=<tenor>/date=<date>/{stream}.parquet` — one
// asset+tenor per file, ~2.5x smaller than the flat superset for a customer who only wants e.g.
// BTC 5-minute markets (measured: 1.48 GB / 86.9M rows for one family-day vs ~3.67 GB of
// row-group-pruned flat superset for the same tokens). Two new `/v1/` routes exist so a client
// never has to guess or re-derive either layout's shape:
//
//   GET {discovery_base}/v1/archive/datasets?asset=..&tenor=..&stream=..&from=..&to=..  — list
//   GET {discovery_base}/v1/archive/url?date=..&stream=..[&asset=..&tenor=..]           — resolve one
//
// **Design choice: discovery is the PRIMARY (and only intentional) path for family URLs, not a
// hand-built `{base}/venue=polymarket/asset=X/tenor=Y/date=Z/{stream}.parquet` string** — the
// server is the single source of truth for both layouts, and the flat layout ALREADY changed shape
// once (flat-only -> flat+family) without this client's knowledge until this very PR. A client
// that only ever reads the `url` field discovery returns cannot drift the next time the layout
// changes again. [`family_stream_url`] below exists ONLY as a narrow fallback for when the
// discovery call itself fails outright (network error, non-2xx, unparsable body) — never for a
// discovery response that came back clean but says "no such dataset" (that is a real absence, not
// an outage, and stays a hard error). This mirrors the flat path's own precedent: `stream_url`'s
// plain string-building was kept exactly as-is (no discovery involved) so existing flat callers
// have zero behavior change — the fallback path here reuses that same "construct the documented
// layout directly" idiom, just parameterized with `asset`/`tenor`.

/// One row of `GET /v1/archive/datasets` (or the single-dataset shape `GET /v1/archive/url`
/// returns): `{venue, layout, asset?, tenor?, date, stream, bytes, rows?, url}`. `asset`/`tenor`
/// are OMITTED (not `null`) by the server for flat rows — `#[serde(default)]` decodes that as
/// `None` rather than failing.
#[derive(Debug, Clone, PartialEq, serde::Deserialize)]
pub struct Dataset {
    pub venue: String,
    pub layout: String,
    #[serde(default)]
    pub asset: Option<String>,
    #[serde(default)]
    pub tenor: Option<String>,
    pub date: String,
    pub stream: String,
    pub bytes: u64,
    #[serde(default)]
    pub rows: Option<u64>,
    pub url: String,
}

/// `GET /v1/archive/datasets`' envelope. `total` reflects the full FILTERED set, not just the
/// returned page (`datasets.len() <= limit`, both server-side pagination knobs).
#[derive(Debug, Clone, PartialEq, serde::Deserialize)]
pub struct DatasetsPage {
    pub datasets: Vec<Dataset>,
    pub total: u64,
    pub limit: u64,
    pub offset: u64,
}

/// Filters for `GET /v1/archive/datasets` — every field optional/combinable, mirroring the live
/// route's own query parameters. `Default` is the fully-unfiltered listing (empty query string).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DatasetFilter {
    pub asset: Option<String>,
    pub tenor: Option<String>,
    pub stream: Option<Stream>,
    pub from: Option<String>,
    pub to: Option<String>,
    pub limit: Option<u32>,
    pub offset: Option<u32>,
}

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

/// Pure JSON→[`DatasetsPage`] decode (no I/O) — fixture-tested below.
pub fn parse_datasets_page(json: &str) -> Result<DatasetsPage, CollectError> {
    serde_json::from_str(json)
        .map_err(|e| CollectError::Fetch(format!("{CTX} datasets parse: {e}")))
}

/// Pure JSON→[`Dataset`] decode (no I/O) — fixture-tested below.
pub fn parse_dataset(json: &str) -> Result<Dataset, CollectError> {
    serde_json::from_str(json).map_err(|e| CollectError::Fetch(format!("{CTX} dataset parse: {e}")))
}

/// The discovery API's root, derived from the archive base by stripping its trailing `/archive`
/// (`https://data.vike.io/archive` -> `https://data.vike.io`, so `/v1/archive/...` sits next to
/// `/archive/...` on the same host). A caller-overridden base that doesn't end in `/archive` passes
/// through unchanged rather than guessing further — the caller can always override
/// `--discovery-base` explicitly in that case.
pub fn default_discovery_base(archive_base: &str) -> String {
    archive_base.strip_suffix("/archive").unwrap_or(archive_base).to_string()
}

/// The family-partitioned URL, mirroring [`stream_url`]'s flat-layout shape one-for-one:
/// `{base}/venue=polymarket/asset=<asset>/tenor=<tenor>/date=<date>/{stream}.parquet`. This is the
/// FALLBACK construction, reached only when a discovery call fails outright — see this section's
/// module doc for why discovery is preferred.
pub fn family_stream_url(
    base: &str,
    asset: &str,
    tenor: &str,
    date: &str,
    stream: Stream,
) -> String {
    format!(
        "{base}/venue={VENUE}/asset={asset}/tenor={tenor}/date={date}/{}.parquet",
        stream.file_name()
    )
}

/// `discovery_base`+`asset`+`tenor` always travel together for a family ingest call — bundled here
/// so [`ArchiveClient::ingest_family_stream`]/[`ArchiveClient::ingest_family_stream_bulk`] stay
/// under clippy's `too_many_arguments` gate rather than each taking three more loose `&str`s.
/// Borrowed, not owned: callers already hold these for the whole ingest call (CLI argv strings).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FamilyTarget<'a> {
    pub discovery_base: &'a str,
    pub asset: &'a str,
    pub tenor: &'a str,
}

// ---- ranged-HTTP ChunkReader --------------------------------------------------------------------

/// A `parquet::file::reader::ChunkReader` over one archive Parquet file, reached entirely through
/// blocking `Range:`-headered GETs (`ureq`, the workspace-pinned blocking HTTP stack — no new
/// dep). Built once per (date, stream); `len()` comes from a single `HEAD` at construction, and
/// every subsequent `get_bytes` issues exactly one ranged GET for the requested span. Cloning the
/// underlying `ureq::Agent` is cheap (pool-backed), so this struct is cheap to construct per file —
/// and cheap to `Clone` outright (`derive`d below): the streaming ingest ([`ingest_stream_over`])
/// clones one already-`open`ed reader once per selected row group, rather than re-`HEAD`ing the URL.
#[derive(Clone)]
pub struct HttpRangeReader {
    agent: ureq::Agent,
    url: String,
    api_key: Option<String>,
    length: u64,
}

impl HttpRangeReader {
    /// `HEAD`s `url` to learn its total length (`Content-Length`), then returns a reader ready for
    /// ranged `get_bytes` calls. The `X-API-Key` header (if `api_key` is `Some`) rides both the
    /// `HEAD` and every subsequent ranged `GET` — harmless on the open `manifest.json`/`samples/`
    /// endpoints, required on the date-partitioned files.
    pub fn open(
        agent: ureq::Agent,
        url: String,
        api_key: Option<String>,
    ) -> Result<Self, CollectError> {
        let mut req = agent.head(&url);
        if let Some(k) = &api_key {
            req = req.header(API_KEY_HEADER, k);
        }
        let resp = req.call().map_err(|e| CollectError::Fetch(format!("{CTX} HEAD {url}: {e}")))?;
        let status = resp.status().as_u16();
        if !(200..300).contains(&status) {
            return Err(CollectError::Fetch(format!("{CTX} HEAD {url}: HTTP {status}")));
        }
        let length: u64 = resp
            .headers()
            .get("Content-Length")
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.parse().ok())
            .ok_or_else(|| {
                CollectError::Fetch(format!("{CTX} HEAD {url}: missing/unparsable Content-Length"))
            })?;
        Ok(Self { agent, url, api_key, length })
    }
}

impl Length for HttpRangeReader {
    fn len(&self) -> u64 {
        self.length
    }
}

/// A conservative cap on how many bytes [`HttpRangeReader::get_read`] eagerly fetches from its
/// requested `start` offset. Parquet's synchronous page reader (`serialized_reader.rs`'s
/// `SerializedPageReader`, the offset-based state used whenever a file carries no page/offset
/// index — true of every real archive file, which is why this was reachable) calls `get_read`
/// ONLY to read one page's HEADER at a time — a Thrift-compact-encoded struct, always a tiny
/// fraction of a page even with embedded column statistics — before switching to a PRECISE
/// `get_bytes(offset, exact_len)` call for that page's actual (possibly multi-MB) data. The
/// earlier implementation instead fetched EVERYTHING from `start` to end-of-file on EVERY
/// `get_read` call (`length - start` bytes): for a page near the start of a multi-GB archive
/// file, "the rest of the file" is nearly the WHOLE FILE, materialized into one `Bytes` buffer —
/// repeated once per page. This (not the per-symbol accumulation the rest of this module's
/// streaming rewrite fixes) turned out to be the actual root cause of the reported unbounded-RSS
/// defect: decode-time memory scaled with total file size, not with how much data a `get_read`
/// caller actually consumed. 1 MiB is a wide safety margin over any realistic page header while
/// keeping the eager-fetch window trivially bounded regardless of file or row-group size; the
/// call cadence (one `get_read` per page) is UNCHANGED from before — only the size fetched is.
const GET_READ_WINDOW_BYTES: u64 = 1024 * 1024;

/// The mean row group of the largest archive stream, from the vendor's own numbers quoted in this
/// module's intro: a `book_events` day is ~7.4 GB over 453 row groups.
const MEAN_ROW_GROUP_BYTES: u64 = 7_400_000_000 / 453; // ~16.3 MB

/// The ceiling on ONE [`HttpRangeReader::get_bytes`] buffer — and the reason the response body is
/// read through [`Read::take`] rather than a bare `read_to_end`.
///
/// `length` is not ours. It originates in the file's own Parquet footer, and
/// `ParquetMetaDataReader::parse_metadata` validates it against exactly one thing:
/// `ChunkReader::len()` — which for this reader is whatever `Content-Length` the server answered
/// our `HEAD` with ([`HttpRangeReader::open`]). A host serving a hostile or merely corrupt file
/// therefore controls BOTH sides of that check, and it used to flow straight into
/// `Vec::with_capacity(length)` — which does NOT return an error when the allocator refuses; it
/// calls `handle_alloc_error` and ABORTS the process — followed by a `read_to_end` with no bound
/// at all, which would keep growing the buffer for as long as the server kept sending. The guard
/// below therefore runs BEFORE the allocation and before the socket is dialled: placed after
/// `with_capacity` it would be unreachable in exactly the case it exists for.
///
/// `--base` defaults to first-party `data.vike.io`, so this hardens against a hostile or
/// misconfigured OVERRIDE, not a live exploit. It is the same unbounded-RSS family as
/// [`GET_READ_WINDOW_BYTES`] above, which this module already had to fix once.
///
/// Sized from what a legitimate request actually is, not from a round number. Parquet's
/// synchronous path calls `get_bytes` for exactly three things: the 8-byte footer tail, the footer
/// metadata (~1.17 MB, verified live — module intro), and ONE data page at a time
/// (`serialized_reader.rs`'s `SerializedPageReader`). A page sits inside a column chunk, which sits
/// inside a row group, so ONE ROW GROUP is a true upper bound on any legitimate single call. 16x
/// the measured mean row group covers a badly skewed group, or a future archive written with much
/// larger ones, while still refusing a request two orders of magnitude past anything this reader
/// has ever legitimately made.
const MAX_CHUNK_BYTES: u64 = 16 * MEAN_ROW_GROUP_BYTES; // ~261 MB

/// The cap must stay clear of the working range: an edit that tightened it to within a skewed row
/// group would start refusing legitimate page reads. Compile error, not a runtime one.
const _: () = assert!(MAX_CHUNK_BYTES > 8 * MEAN_ROW_GROUP_BYTES);

impl ChunkReader for HttpRangeReader {
    type T = Cursor<Bytes>;

    fn get_read(&self, start: u64) -> datafusion::parquet::errors::Result<Self::T> {
        let window = GET_READ_WINDOW_BYTES.min(self.length.saturating_sub(start));
        Ok(Cursor::new(self.get_bytes(start, window as usize)?))
    }

    fn get_bytes(&self, start: u64, length: usize) -> datafusion::parquet::errors::Result<Bytes> {
        if length == 0 {
            return Ok(Bytes::new());
        }
        // BEFORE the allocation and before the dial — see [`MAX_CHUNK_BYTES`]. A clean `Err` here
        // is the whole point: `Vec::with_capacity` on an implausible length aborts the process
        // instead of returning, so a caller would never get the chance to handle it.
        if length as u64 > MAX_CHUNK_BYTES {
            return Err(ParquetError::General(format!(
                "{CTX} range GET {}: refusing a {length}-byte chunk at offset {start} (cap \
                 {MAX_CHUNK_BYTES}) — the file's Parquet footer or the server's Content-Length \
                 ({}) is implausible",
                self.url, self.length
            )));
        }
        let end = start + length as u64 - 1;
        let range = format!("bytes={start}-{end}");
        let mut req = self.agent.get(&self.url).header("Range", &range);
        if let Some(k) = &self.api_key {
            req = req.header(API_KEY_HEADER, k);
        }
        let mut resp = req.call().map_err(|e| {
            ParquetError::General(format!("{CTX} range GET {} [{range}]: {e}", self.url))
        })?;
        let status = resp.status().as_u16();
        // 206 Partial Content is the expected case; a plain 200 (server ignored the Range header
        // and sent the whole body) is tolerated too — the length check below still catches a
        // mismatch, just as an EOF rather than a silently-wrong slice.
        if status != 206 && status != 200 {
            return Err(ParquetError::General(format!(
                "{CTX} range GET {} [{range}]: HTTP {status}",
                self.url
            )));
        }
        // `length` is capped above, so `with_capacity` can no longer abort. `take(length)` is what
        // bounds the OTHER direction: a server that answers a small `Range` with a huge body can no
        // longer grow this buffer past what we actually asked for. The short-body check below is
        // unchanged and still catches the opposite failure.
        let mut buf = Vec::with_capacity(length);
        resp.body_mut()
            .as_reader()
            .take(length as u64)
            .read_to_end(&mut buf)
            .map_err(|e| ParquetError::General(format!("{CTX} read body {}: {e}", self.url)))?;
        if buf.len() != length {
            return Err(ParquetError::EOF(format!(
                "{CTX} range GET {} [{range}]: expected {length} bytes, got {}",
                self.url,
                buf.len()
            )));
        }
        Ok(Bytes::from(buf))
    }
}

// ---- local-file ChunkReader (the `--file` path: same pipeline, no network) ----------------------

/// A `ChunkReader` over a Parquet file already sitting on local disk — the local-ingest twin of
/// [`HttpRangeReader`]. Feeds the EXACT SAME [`ingest_stream_over`]/[`ingest_stream_bulk_over`]
/// row-group-at-a-time pipeline (decode → map → commit/stage) as the HTTP path, so a
/// locally-downloaded file and a range-fetched URL for the same (date, stream) produce
/// byte-identical store output — the reader is the only thing that differs. Wrapping in
/// `Arc<File>` (which IS unconditionally `Clone`, regardless of what `File` itself is — only a
/// fallible `try_clone`) satisfies the streaming core's `T: ChunkReader + Clone` bound, sharing one
/// open file description across row groups exactly as `HttpRangeReader::clone` shares one pooled
/// connection.
///
/// **Reads are positional (`read_at`/`seek_read`), NOT `parquet::file::reader`'s own
/// `impl ChunkReader for File`** (`self.try_clone()?.seek(start)?.read(...)`) — deliberately: a
/// `File::try_clone()` (`dup(2)` on unix, `DuplicateHandle` on Windows) shares the SAME
/// open-file-description, and therefore the SAME file-offset cursor, as the handle it cloned from.
/// That's harmless for the SERIAL local-file path (one row group's reads always complete before
/// the next begins — no concurrent seeks to race), but [`ingest_local_file_parallel`]'s worker
/// threads all clone the ONE `Arc<File>` this struct wraps and issue seeks/reads concurrently: one
/// thread's `seek` can move the shared cursor a second thread is mid-`read` against, silently
/// splicing two threads' reads together. Reproduced live on the CI box the first time this module ran
/// with `--jobs 4` (never under `--jobs 1`, and only intermittently, per the race): a Parquet page
/// bytes were read at the WRONG offset and decompression failed with "External: Unknown frame
/// descriptor". `read_at`/`seek_read` take the offset as an explicit syscall argument and never
/// touch the shared cursor — safe for any number of concurrent callers over the same `Arc<File>`.
#[derive(Clone)]
pub struct LocalFileReader(Arc<File>);

impl LocalFileReader {
    /// Opens `path` for reading. No row data is read here — like [`HttpRangeReader::open`], the
    /// footer and every row group are read lazily by the Parquet reader built on top of this type.
    pub fn open(path: &Path) -> Result<Self, CollectError> {
        let file = File::open(path).map_err(|e| {
            CollectError::Fetch(format!("{CTX} open local file {}: {e}", path.display()))
        })?;
        Ok(Self(Arc::new(file)))
    }
}

impl Length for LocalFileReader {
    fn len(&self) -> u64 {
        Length::len(&*self.0)
    }
}

/// Positional read at `pos` into `buf`, returning the number of bytes read (`0` at EOF) — never
/// touches the file's shared cursor (see [`LocalFileReader`]'s doc for why that matters under
/// concurrent access). `unix`'s `read_at`/Windows' `seek_read` are both true pread-style syscalls.
#[cfg(unix)]
fn read_at(file: &File, buf: &mut [u8], pos: u64) -> std::io::Result<usize> {
    std::os::unix::fs::FileExt::read_at(file, buf, pos)
}

#[cfg(windows)]
fn read_at(file: &File, buf: &mut [u8], pos: u64) -> std::io::Result<usize> {
    std::os::windows::fs::FileExt::seek_read(file, buf, pos)
}

/// A `Read` over a shared `Arc<File>`, advancing an OWN (per-instance, not OS-shared) position via
/// [`read_at`] on each call — the concurrency-safe replacement for `BufReader<File>`'s
/// shared-cursor `seek`+`read`. Returned by [`LocalFileReader::get_read`]; every instance owns its
/// `pos` independently, so `N` of these live concurrently over the same `Arc<File>` (one per
/// in-flight row group under [`ingest_local_file_parallel`]) without interfering.
pub struct PositionedReader {
    file: Arc<File>,
    pos: u64,
}

impl Read for PositionedReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = read_at(&self.file, buf, self.pos)?;
        self.pos += n as u64;
        Ok(n)
    }
}

impl ChunkReader for LocalFileReader {
    type T = PositionedReader;

    fn get_read(&self, start: u64) -> datafusion::parquet::errors::Result<Self::T> {
        Ok(PositionedReader { file: self.0.clone(), pos: start })
    }

    fn get_bytes(&self, start: u64, length: usize) -> datafusion::parquet::errors::Result<Bytes> {
        let mut buf = vec![0u8; length];
        let mut filled = 0usize;
        while filled < length {
            let n = read_at(&self.0, &mut buf[filled..], start + filled as u64).map_err(|e| {
                ParquetError::General(format!(
                    "{CTX} local file read at offset {}: {e}",
                    start + filled as u64
                ))
            })?;
            if n == 0 {
                return Err(ParquetError::EOF(format!(
                    "{CTX} local file: expected {length} bytes at offset {start}, got only {filled}"
                )));
            }
            filled += n;
        }
        Ok(Bytes::from(buf))
    }
}

// ---- row-group pruning (pure over already-loaded metadata; unit-tested with synthetic files) ----

/// Which row-group indices of `md` can possibly contain any of `tokens`' rows, using the
/// `token_id` column's per-row-group min/max statistics. `None` (no filter) selects every row
/// group. An explicit-but-empty token set selects none. Byte-lexicographic comparison — the exact
/// comparison Parquet defines for `BYTE_ARRAY`/`Utf8` column statistics, and correct regardless of
/// whether the file happens to be globally sorted: a row group's own min/max are always true
/// bounds over that group's own rows, so exclusion never drops a real match. Falls back to
/// "include everything" whenever pruning data is unavailable (no `token_id` column found, or a
/// row group's column carries no statistics) — pruning is an optimization, never a filter that can
/// silently lose rows.
pub fn select_row_groups(md: &ParquetMetaData, tokens: Option<&HashSet<String>>) -> Vec<usize> {
    let total = md.num_row_groups();
    let Some(tokens) = tokens else {
        return (0..total).collect();
    };
    if tokens.is_empty() {
        return Vec::new();
    }
    let Some(col_idx) =
        md.file_metadata().schema_descr().columns().iter().position(|c| c.name() == "token_id")
    else {
        return (0..total).collect(); // no token_id column found — can't prune, include everything
    };
    (0..total)
        .filter(|&i| {
            let rg = md.row_group(i);
            let Some(stats) = rg.column(col_idx).statistics() else {
                return true; // no stats on this group's token_id chunk — include defensively
            };
            let (Some(min), Some(max)) = (stats.min_bytes_opt(), stats.max_bytes_opt()) else {
                return true;
            };
            tokens.iter().any(|t| {
                let tb = t.as_bytes();
                min <= tb && tb <= max
            })
        })
        .collect()
}

// ---- Arrow column decode (pure; unit-tested against synthetic batches) --------------------------

fn str_col<'a>(b: &'a RecordBatch, name: &str) -> Result<&'a StringArray, CollectError> {
    b.column_by_name(name)
        .and_then(|c| c.as_any().downcast_ref::<StringArray>())
        .ok_or_else(|| CollectError::Fetch(format!("{CTX} parquet: missing/!string column {name}")))
}

fn i64_col<'a>(b: &'a RecordBatch, name: &str) -> Result<&'a Int64Array, CollectError> {
    b.column_by_name(name)
        .and_then(|c| c.as_any().downcast_ref::<Int64Array>())
        .ok_or_else(|| CollectError::Fetch(format!("{CTX} parquet: missing/!int64 column {name}")))
}

fn u64_col<'a>(b: &'a RecordBatch, name: &str) -> Result<&'a UInt64Array, CollectError> {
    b.column_by_name(name)
        .and_then(|c| c.as_any().downcast_ref::<UInt64Array>())
        .ok_or_else(|| CollectError::Fetch(format!("{CTX} parquet: missing/!uint64 column {name}")))
}

fn f64_col<'a>(b: &'a RecordBatch, name: &str) -> Result<&'a Float64Array, CollectError> {
    b.column_by_name(name).and_then(|c| c.as_any().downcast_ref::<Float64Array>()).ok_or_else(
        || CollectError::Fetch(format!("{CTX} parquet: missing/!float64 column {name}")),
    )
}

fn decimal_col<'a>(b: &'a RecordBatch, name: &str) -> Result<&'a Decimal128Array, CollectError> {
    b.column_by_name(name).and_then(|c| c.as_any().downcast_ref::<Decimal128Array>()).ok_or_else(
        || CollectError::Fetch(format!("{CTX} parquet: missing/!decimal128 column {name}")),
    )
}

/// `event_type`/`side` on `book_events` decode as Arrow `Binary` on the FLAT layout — ClickHouse's
/// `Enum8`/`Enum` Parquet export writes raw bytes without a UTF8 logical-type annotation (verified
/// against the real sample file). **The FAMILY layout re-encodes the identical two columns as plain
/// `Utf8` instead** (live-verified via `clickhouse-local DESCRIBE TABLE` against a real family
/// `book_events` file during this port's own the CI box ingest run: `event_type String`, `side String`)
/// — the family export's `clickhouse-local` classify/strip/move pipeline
/// (`archive-family-partition-report.md`) evidently re-materializes these columns through a
/// different codec path than the flat layout's direct `SELECT ... FORMAT Parquet`, even though both
/// layouts carry "the same 16 real columns" per that report's own schema check. [`StrOrBinCol`]
/// decodes either physical type transparently so `book_updates_from_batch` works unmodified across
/// both layouts — this is NOT a typo or a regression in the vendor's export, just a genuine
/// physical-type divergence between the two pipelines this client must tolerate. `trades`' own
/// `side` column is plain `Utf8` on both layouts (see [`str_col`]).
enum StrOrBinCol<'a> {
    Utf8(&'a StringArray),
    Bin(&'a BinaryArray),
}

impl StrOrBinCol<'_> {
    /// UTF-8(-lossy for the `Binary` case) decode of one row (malformed bytes degrade to `""`
    /// rather than panicking — one bad row must not fail a whole scan).
    fn value(&self, i: usize) -> &str {
        match self {
            StrOrBinCol::Utf8(a) => a.value(i),
            StrOrBinCol::Bin(a) => std::str::from_utf8(a.value(i)).unwrap_or(""),
        }
    }
}

fn str_or_bin_col<'a>(b: &'a RecordBatch, name: &str) -> Result<StrOrBinCol<'a>, CollectError> {
    let col = b
        .column_by_name(name)
        .ok_or_else(|| CollectError::Fetch(format!("{CTX} parquet: missing column {name}")))?;
    if let Some(a) = col.as_any().downcast_ref::<StringArray>() {
        return Ok(StrOrBinCol::Utf8(a));
    }
    if let Some(a) = col.as_any().downcast_ref::<BinaryArray>() {
        return Ok(StrOrBinCol::Bin(a));
    }
    Err(CollectError::Fetch(format!("{CTX} parquet: column {name} is neither string nor binary")))
}

fn dec(arr: &Decimal128Array, i: usize, divisor: f64) -> f64 {
    arr.value(i) as f64 / divisor
}

/// Decode a `[[price,size],...]` JSON depth column (plain numbers — NOT pmxt's stringified-number
/// variant `crate::pmxt::map::parse_levels` handles). Malformed JSON degrades to empty rather than
/// erroring — one bad row must not fail a whole scan.
fn parse_levels_json(json: &str) -> Vec<(f64, f64)> {
    if json.is_empty() {
        return Vec::new();
    }
    serde_json::from_str::<Vec<(f64, f64)>>(json).unwrap_or_default()
}

/// Decode one `book_events` batch into [`BookUpdate`]s. `symbol` is read PER ROW from `token_id` —
/// an unfiltered (or coarsely row-group-pruned) scan mixes many tokens in one batch.
fn book_updates_from_batch(b: &RecordBatch) -> Result<Vec<BookUpdate>, CollectError> {
    let token_id = str_col(b, "token_id")?;
    let ts = i64_col(b, "ts")?;
    let local_ts = i64_col(b, "local_ts")?;
    let seq = u64_col(b, "seq")?;
    let event_type = str_or_bin_col(b, "event_type")?;
    let side = str_or_bin_col(b, "side")?;
    let price = decimal_col(b, "price")?;
    let size = decimal_col(b, "size")?;
    let bids = str_col(b, "bids")?;
    let asks = str_col(b, "asks")?;
    let tick_size = decimal_col(b, "tick_size")?;
    let status = str_col(b, "status")?;

    let mut out = Vec::with_capacity(b.num_rows());
    for i in 0..b.num_rows() {
        let kind = match event_type.value(i) {
            "book" => BookUpdateKind::Snapshot,
            "price_change" => BookUpdateKind::Delta,
            "status" => match status.value(i) {
                "gap_start" => BookUpdateKind::GapStart,
                "stale" => BookUpdateKind::Stale,
                "live_resume" => BookUpdateKind::LiveResume,
                _ => continue, // unrecognized status label — skip rather than guess
            },
            // "trade" -> decoded separately by `trades_from_batch`; "tick_size_change" carries no
            // `BookUpdate` of its own (mirrors pmxt's mapper: state-only, no row).
            _ => continue,
        };
        let (row_bids, row_asks) = match kind {
            BookUpdateKind::Snapshot => {
                (parse_levels_json(bids.value(i)), parse_levels_json(asks.value(i)))
            }
            BookUpdateKind::Delta => {
                let level = (dec(price, i, PRICE_SCALE_DIVISOR), dec(size, i, SIZE_SCALE_DIVISOR));
                match side.value(i) {
                    "buy" => (vec![level], Vec::new()),
                    "sell" => (Vec::new(), vec![level]),
                    _ => (Vec::new(), Vec::new()), // "none" / unexpected — a degenerate empty delta
                }
            }
            _ => (Vec::new(), Vec::new()), // GapStart / Stale / LiveResume carry no levels
        };
        out.push(BookUpdate {
            ts: ts.value(i),
            local_ts: local_ts.value(i),
            seq: seq.value(i),
            kind,
            tick_size: dec(tick_size, i, PRICE_SCALE_DIVISOR),
            bids: row_bids,
            asks: row_asks,
            symbol: token_id.value(i).to_string(),
        });
    }
    Ok(out)
}

/// Decode one `trades` batch into [`TradeTick`]s. `side` here is the TAKER side, plain `Utf8`
/// (unlike `book_events`' `Binary`-encoded enum) — `"sell"` means the taker sold, i.e. the buyer
/// was the resting maker (`is_buyer_maker = true`), the same taker-side convention
/// `crate::backtest_bridge::trades_from_batch` inverts for the live ClickHouse tape.
fn trades_from_batch(b: &RecordBatch) -> Result<Vec<TradeTick>, CollectError> {
    let token_id = str_col(b, "token_id")?;
    let ts = i64_col(b, "ts")?;
    let local_ts = i64_col(b, "local_ts")?;
    let price = f64_col(b, "price")?;
    let size = f64_col(b, "size")?;
    let side = str_col(b, "side")?;

    let mut out = Vec::with_capacity(b.num_rows());
    for i in 0..b.num_rows() {
        out.push(TradeTick {
            ts: ts.value(i),
            local_ts: local_ts.value(i),
            price: price.value(i),
            size: size.value(i),
            is_buyer_maker: side.value(i) == "sell",
            symbol: token_id.value(i).to_string(),
        });
    }
    Ok(out)
}

/// Decode one `l1_quotes` batch into [`QuoteTick`]s.
fn quotes_from_batch(b: &RecordBatch) -> Result<Vec<QuoteTick>, CollectError> {
    let token_id = str_col(b, "token_id")?;
    let ts = i64_col(b, "ts")?;
    let local_ts = i64_col(b, "local_ts")?;
    let bid = decimal_col(b, "bid")?;
    let ask = decimal_col(b, "ask")?;
    let bid_size = decimal_col(b, "bid_size")?;
    let ask_size = decimal_col(b, "ask_size")?;

    let mut out = Vec::with_capacity(b.num_rows());
    for i in 0..b.num_rows() {
        out.push(QuoteTick {
            ts: ts.value(i),
            local_ts: local_ts.value(i),
            bid: dec(bid, i, PRICE_SCALE_DIVISOR),
            ask: dec(ask, i, PRICE_SCALE_DIVISOR),
            bid_size: dec(bid_size, i, SIZE_SCALE_DIVISOR),
            ask_size: dec(ask_size, i, SIZE_SCALE_DIVISOR),
            symbol: token_id.value(i).to_string(),
        });
    }
    Ok(out)
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
        // `http_status_as_error(false)`: read statuses ourselves (a 401/404 is reported with
        // context, not turned into an opaque transport error) — the same posture `crate::http::send`
        // takes.
        let agent = ureq::Agent::config_builder().http_status_as_error(false).build().new_agent();
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
                    %date, kind = stream.kind(), %asset, %tenor,
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
        let kind = stream.kind();
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
        let kind = stream.kind();
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
        let kind = stream.kind();
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
        let kind = stream.kind();
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

// ---- local-file ingest entrypoints (the `--file` path — no `ArchiveClient` needed: no agent, no
// base URL, no API key) ----------------------------------------------------------------------------

/// Ingest one local Parquet file — already downloaded to disk — for one (date, stream), through the
/// SAME streaming pipeline [`ArchiveClient::ingest_stream`] uses ([`ingest_stream_over`]),
/// substituting [`LocalFileReader`] for [`HttpRangeReader`] as the only difference: no `HEAD`, no
/// per-row-group ranged `GET`s, no Cloudflare round trips — a local `seek`+`read` (backed by the OS
/// page cache) stands in for what would otherwise be one HTTP request per row group. Same
/// idempotent per-row-group commit keys, same one-row-group-resident-at-a-time memory bound, same
/// progress logging cadence. `date`/`stream` are supplied by the caller rather than inferred from
/// `path` — see `vike_archive_backfill`'s module doc for why (a real downloaded file's name has no
/// obligation to match the archive's own `{stream}.parquet` convention — the file this feature was
/// built to measure, `btc5m_2026-07-28.parquet`, is a case in point).
pub fn ingest_local_file(
    store: &DataFusionHist,
    path: &Path,
    date: &str,
    stream: Stream,
    tokens: Option<&HashSet<String>>,
) -> Result<usize, CollectError> {
    let reader = LocalFileReader::open(path)?;
    let kind = stream.kind();
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
                "vike-archive: local-file ingest progress"
            );
        }
    })
}

/// BULK ingest fanned across `jobs` workers — the composition of [`ingest_local_file_bulk`]'s
/// staging win with [`ingest_local_file_parallel`]'s fan-out, which previously could not be had
/// together.
///
/// **Why this could not simply reuse one session.** [`BulkIngestSession`] takes `&mut self`, so a
/// `--bulk --jobs N` run silently degraded to serial. The obvious fix — share one session behind a
/// lock — would serialize the very staging it exists to parallelize. The real obstacle is subtler
/// and is a CORRECTNESS one: a session stamps its commit keys `{key_prefix}:w{window_idx}` from its
/// OWN counter, which starts at 0. N sessions sharing one prefix would therefore mint the SAME key
/// (`prefix:w0`, `prefix:w1`, …) for DIFFERENT row sets, and `commit_rows_bulk` treats an
/// already-seen key as a durable no-op — so the second worker to reach a given series would have
/// its rows silently DROPPED. Not a race; a guaranteed, quiet loss.
///
/// So each worker gets its own session under its own key namespace, `{key_prefix}:j{chunk}`. Keys
/// are then unique by construction, and each worker's `window_idx` stays private.
///
/// **Chunking is what makes per-worker sessions cheap.** [`chunk_row_groups`] splits CONTIGUOUSLY,
/// and the file is `token_id`-sorted, so a token straddles at most one chunk boundary: nearly every
/// token's rows land wholly inside one worker, hence one session, hence ONE commit for that series
/// — the collapse the bulk profile exists for is preserved per worker rather than defeated. At most
/// `jobs - 1` tokens (the boundary ones) pay a second commit.
///
/// ⚠ **A resumed run must use the same `--jobs`.** Bulk idempotency is by commit KEY, and the keys
/// now embed the chunk index, which is a function of `(selected row groups, jobs)`. Re-running a
/// crashed `--jobs 4` import as `--jobs 8` re-chunks, mints different keys, and re-writes rows that
/// were already committed — DUPLICATES, not a no-op, because this store never dedups by row value.
/// This is an extension of the constraint bulk already had (the same `BulkConfig` window, since
/// flush boundaries decide which rows share a key), not a new class of hazard — but it is now one
/// more flag that must match on a retry.
///
/// Returns rows staged (not necessarily yet committed); each worker flushes its own trailing window
/// before returning, so unlike [`ingest_local_file_bulk`] the caller does NOT flush afterwards —
/// there is no single session left to flush.
#[allow(clippy::too_many_arguments)]
pub fn ingest_local_file_bulk_parallel(
    store: &DataFusionHist,
    path: &Path,
    date: &str,
    stream: Stream,
    tokens: Option<&HashSet<String>>,
    key_prefix: &str,
    jobs: usize,
    cfg: vike_data::BulkConfig,
) -> Result<usize, CollectError> {
    let reader = LocalFileReader::open(path)?;
    let kind = stream.kind();
    let arrow_meta =
        ArrowReaderMetadata::load(&reader, ArrowReaderOptions::default()).map_err(|e| {
            CollectError::Fetch(format!("{CTX} parquet open {date}/{}: {e}", stream.file_name()))
        })?;
    let md = arrow_meta.metadata().clone();
    let selected = select_row_groups(&md, tokens);
    let total = selected.len();
    let chunks = chunk_row_groups(&selected, jobs);
    let pool_width = chunks.len().max(1);
    let start = Instant::now();
    let rows_cumulative = AtomicUsize::new(0);
    let groups_done = AtomicUsize::new(0);

    let pool = ThreadPoolBuilder::new()
        .num_threads(pool_width)
        .thread_name(|i| format!("vike-archive-bulk-{i}"))
        .build()
        .map_err(|e| {
            CollectError::Fetch(format!("{CTX} rayon pool ({pool_width} threads): {e}"))
        })?;

    pool.install(|| {
        chunks.par_iter().enumerate().try_for_each(
            |(chunk_idx, chunk)| -> Result<(), CollectError> {
                // One session per worker, under its own key namespace (see the ⚠ above).
                let mut session = store.bulk_session(cfg);
                let worker_prefix = format!("{key_prefix}:j{chunk_idx}");
                for &rg_idx in chunk {
                    let rg_reader = ParquetRecordBatchReaderBuilder::new_with_metadata(
                        reader.clone(),
                        arrow_meta.clone(),
                    )
                    .with_row_groups(vec![rg_idx])
                    .build()
                    .map_err(|e| {
                        CollectError::Fetch(format!(
                            "{CTX} parquet reader {date}/{} rg{rg_idx}: {e}",
                            stream.file_name()
                        ))
                    })?;
                    let rows_this_group = match stream {
                        Stream::Book => stage_book(&mut session, rg_reader, tokens)?,
                        Stream::Trade => stage_trades(&mut session, rg_reader, tokens)?,
                        Stream::Quote => stage_quotes(&mut session, rg_reader, tokens)?,
                    };
                    session.end_batch(&worker_prefix).map_err(|e| {
                        CollectError::Fetch(format!(
                            "{CTX} bulk flush {date}/{}: {e}",
                            stream.file_name()
                        ))
                    })?;
                    let cumulative = rows_cumulative.fetch_add(rows_this_group, Ordering::Relaxed)
                        + rows_this_group;
                    let index = groups_done.fetch_add(1, Ordering::Relaxed);
                    let is_boundary = index == 0 || index + 1 == total;
                    if is_boundary || (index + 1).is_multiple_of(PROGRESS_LOG_EVERY) {
                        tracing::info!(
                            %date,
                            kind,
                            jobs,
                            row_groups_done = index + 1,
                            total_row_groups = total,
                            bytes_this_group = md.row_group(rg_idx).compressed_size(),
                            rows_this_group,
                            rows_cumulative = cumulative,
                            elapsed_ms = start.elapsed().as_millis() as u64,
                            "vike-archive: local-file BULK parallel ingest progress (completion order, not file order)"
                        );
                    }
                }
                // Each worker flushes its OWN trailing window — there is no shared session for the
                // caller to flush afterwards.
                session.flush(&worker_prefix).map_err(|e| {
                    CollectError::Fetch(format!(
                        "{CTX} bulk final flush {date}/{}: {e}",
                        stream.file_name()
                    ))
                })?;
                Ok(())
            },
        )
    })?;

    Ok(rows_cumulative.load(Ordering::Relaxed))
}

/// BULK/offline write-profile twin of [`ingest_local_file`] — the same relationship
/// [`ArchiveClient::ingest_stream_bulk`] has to [`ArchiveClient::ingest_stream`]: each row group's
/// decoded rows are STAGED into `session` instead of committed straight to the store. See
/// [`ArchiveClient::ingest_stream_bulk`]'s doc for the flush contract this shares verbatim — the
/// caller must still call `session.flush(key_prefix)` once more after this returns to durably
/// commit the trailing partial window.
pub fn ingest_local_file_bulk(
    session: &mut BulkIngestSession<'_>,
    path: &Path,
    date: &str,
    stream: Stream,
    tokens: Option<&HashSet<String>>,
    key_prefix: &str,
) -> Result<usize, CollectError> {
    let reader = LocalFileReader::open(path)?;
    let kind = stream.kind();
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
                    "vike-archive: local-file bulk ingest progress (staged; not necessarily flushed yet)"
                );
            }
        },
    )
}

/// How often [`ArchiveClient::ingest_stream`] logs progress at INFO (every Nth row group, plus
/// always the first and last) — terse enough not to spam a run over hundreds of row groups, while
/// still proving from the outside that the process is alive and making progress (the defect this
/// whole streaming rewrite fixes: previously nothing was observable — and nothing was written to
/// disk — until the entire file had been decoded).
const PROGRESS_LOG_EVERY: usize = 25;

/// One row group's ingest result, reported to [`ArchiveClient::ingest_stream`]'s progress callback.
/// `index` is 0-based position within the SELECTED (pruned) row groups, not the raw Parquet row
/// group index (that one is folded into `compressed_bytes`' lookup only, not exposed — callers care
/// about "how far through this run", not the file's raw row-group numbering). Under
/// [`ingest_local_file_parallel`] specifically, `index` is COMPLETION order (a shared atomic
/// counter incremented as each worker finishes a group) rather than the group's position in the
/// file — several workers finish concurrently, so "which row group is this" in file order isn't a
/// single well-ordered number anymore; completion order still gives every value in `0..total`
/// exactly once, so the existing "first/last/every-Nth" progress-logging idiom keeps working
/// unchanged.
#[derive(Debug, Clone, Copy)]
pub struct RowGroupProgress {
    pub index: usize,
    pub total: usize,
    pub compressed_bytes: i64,
    pub rows_this_group: usize,
    pub rows_cumulative: usize,
    pub elapsed_ms: u64,
}

// ---- parallel local-file ingest (the `--jobs N` fan-out; LOCAL FILE ONLY — see the module doc's
// "Parallel local-file ingest" section for the full design and why the HTTP path is untouched) ----

/// Split `selected` (row-group indices, already in the file's own `token_id`-sorted order — see
/// [`select_row_groups`]) into up to `jobs` CONTIGUOUS chunks of as-equal-as-possible length (the
/// remainder distributed one-per-chunk to the first chunks) — the fan-out unit
/// [`ingest_local_file_parallel`] hands one chunk to each worker thread. Contiguous (not
/// round-robin/interleaved) is load-bearing: since a token can straddle at most two ADJACENT row
/// groups, a contiguous split means at most one token sits on each of the `jobs - 1` chunk
/// boundaries — every other token's rows land entirely within one chunk, hence one worker,
/// avoiding cross-worker writes to the same store series. `jobs == 0` is treated as 1 (never
/// returns zero chunks for a non-empty `selected`); `jobs` above `selected.len()` clamps down to
/// `selected.len()` (one row group per chunk, never an empty chunk). An empty `selected` (a token
/// filter that matched nothing) returns no chunks at all.
fn chunk_row_groups(selected: &[usize], jobs: usize) -> Vec<Vec<usize>> {
    if selected.is_empty() {
        return Vec::new();
    }
    let jobs = jobs.max(1).min(selected.len());
    let base = selected.len() / jobs;
    let rem = selected.len() % jobs;
    let mut out = Vec::with_capacity(jobs);
    let mut start = 0;
    for i in 0..jobs {
        let len = base + usize::from(i < rem);
        out.push(selected[start..start + len].to_vec());
        start += len;
    }
    out
}

/// The parallel twin of [`ingest_stream_over`]: `T: Sync` (rather than that function's plain
/// `Clone`) because `reader`/`arrow_meta` are shared BY REFERENCE across a `rayon::ThreadPool`'s
/// worker threads, each cloning its own handle before scoping to a row group — cheap for
/// [`LocalFileReader`] (an `Arc<File>` clone) exactly as it is for the serial path.
/// [`chunk_row_groups`] partitions the selected row groups into `jobs` contiguous chunks; each
/// pool thread drains its OWN chunk serially (one row group resident at a time, identical to the
/// serial core), so at most `jobs` row groups are ever mid-decode together. `on_row_group` is
/// called from whichever worker thread finishes a group (see [`RowGroupProgress::index`]'s doc
/// for the completion-order caveat this implies) — `Fn + Sync`, not the serial core's `FnMut`,
/// because it is invoked concurrently. `jobs <= 1` builds a ONE-thread pool over a SINGLE chunk
/// covering every selected row group in file order — the exact decode/commit sequence
/// [`ingest_stream_over`] itself would produce, so a `jobs=1` caller gets byte-identical output.
fn ingest_local_over_parallel<T>(
    reader: T,
    store: &DataFusionHist,
    date: &str,
    stream: Stream,
    tokens: Option<&HashSet<String>>,
    jobs: usize,
    on_row_group: impl Fn(RowGroupProgress) + Sync,
) -> Result<usize, CollectError>
where
    T: ChunkReader + Clone + Sync + 'static,
{
    let arrow_meta =
        ArrowReaderMetadata::load(&reader, ArrowReaderOptions::default()).map_err(|e| {
            CollectError::Fetch(format!("{CTX} parquet open {date}/{}: {e}", stream.file_name()))
        })?;
    let md = arrow_meta.metadata().clone();
    let selected = select_row_groups(&md, tokens);
    let total = selected.len();
    let chunks = chunk_row_groups(&selected, jobs);
    let pool_width = chunks.len().max(1);
    let start = Instant::now();
    let rows_cumulative = AtomicUsize::new(0);
    let groups_done = AtomicUsize::new(0);

    let pool = ThreadPoolBuilder::new()
        .num_threads(pool_width)
        .thread_name(|i| format!("vike-archive-ingest-{i}"))
        .build()
        .map_err(|e| {
            CollectError::Fetch(format!("{CTX} rayon pool ({pool_width} threads): {e}"))
        })?;

    pool.install(|| {
        chunks.par_iter().try_for_each(|chunk| -> Result<(), CollectError> {
            for &rg_idx in chunk {
                let rg_reader = ParquetRecordBatchReaderBuilder::new_with_metadata(
                    reader.clone(),
                    arrow_meta.clone(),
                )
                .with_row_groups(vec![rg_idx])
                .build()
                .map_err(|e| {
                    CollectError::Fetch(format!(
                        "{CTX} parquet reader {date}/{} rg{rg_idx}: {e}",
                        stream.file_name()
                    ))
                })?;
                let commit_key = format!("vikearchive:{}:{date}:rg{rg_idx}", stream.kind());
                let rows_this_group = match stream {
                    Stream::Book => ingest_book(rg_reader, store, tokens, &commit_key)?,
                    Stream::Trade => ingest_trades(rg_reader, store, tokens, &commit_key)?,
                    Stream::Quote => ingest_quotes(rg_reader, store, tokens, &commit_key)?,
                };
                let cumulative =
                    rows_cumulative.fetch_add(rows_this_group, Ordering::Relaxed) + rows_this_group;
                let index = groups_done.fetch_add(1, Ordering::Relaxed);
                on_row_group(RowGroupProgress {
                    index,
                    total,
                    compressed_bytes: md.row_group(rg_idx).compressed_size(),
                    rows_this_group,
                    rows_cumulative: cumulative,
                    elapsed_ms: start.elapsed().as_millis() as u64,
                });
                // `rg_reader` (and whatever it decoded) drops HERE, before this worker's next row
                // group — the same one-row-group-resident-per-thread bound the serial path holds,
                // just multiplied by `pool_width` workers instead of held to exactly one.
            }
            Ok(())
        })
    })?;

    Ok(rows_cumulative.load(Ordering::Relaxed))
}

/// Ingest one local Parquet file using up to `jobs` worker threads fanned across row groups — the
/// parallel twin of [`ingest_local_file`]. See the module doc's "Parallel local-file ingest"
/// section for the full design (fan-out unit, memory bound, why write contention stays low) and
/// [`chunk_row_groups`]/[`ingest_local_over_parallel`] for the mechanics. `jobs` is clamped to at
/// least 1 by [`chunk_row_groups`]; a caller wanting the OLD byte-for-byte serial behavior should
/// pass `jobs = 1` (identical decode/commit order to [`ingest_local_file`]) rather than call that
/// function directly, though both remain available and behave identically at `jobs = 1`.
pub fn ingest_local_file_parallel(
    store: &DataFusionHist,
    path: &Path,
    date: &str,
    stream: Stream,
    tokens: Option<&HashSet<String>>,
    jobs: usize,
) -> Result<usize, CollectError> {
    let reader = LocalFileReader::open(path)?;
    let kind = stream.kind();
    ingest_local_over_parallel(reader, store, date, stream, tokens, jobs, |p: RowGroupProgress| {
        let is_boundary = p.index == 0 || p.index + 1 == p.total;
        if is_boundary || (p.index + 1).is_multiple_of(PROGRESS_LOG_EVERY) {
            tracing::info!(
                %date,
                kind,
                jobs,
                row_groups_done = p.index + 1,
                total_row_groups = p.total,
                bytes_this_group = p.compressed_bytes,
                rows_this_group = p.rows_this_group,
                rows_cumulative = p.rows_cumulative,
                elapsed_ms = p.elapsed_ms,
                "vike-archive: local-file parallel ingest progress (completion order, not file order)"
            );
        }
    })
}

/// The memory-bounded streaming core behind [`ArchiveClient::ingest_stream`]: processes `reader`'s
/// selected row groups ONE AT A TIME — build a row-group-scoped `ParquetRecordBatchReader` (reusing
/// one cached `ArrowReaderMetadata` footer, so re-scoping to a new group never re-fetches it),
/// decode, append to `store`, drop, repeat — so at most one row group's decoded rows are ever
/// resident. Generic over `T: ChunkReader + Clone` so it is unit-testable over a plain in-memory
/// `Bytes` file (no network involved), exactly like [`select_row_groups`]'s own end-to-end reader
/// test below; [`ArchiveClient::ingest_stream`] is the only real caller, over `HttpRangeReader`.
///
/// Idempotency is scoped PER ROW GROUP (`vikearchive:{kind}:{date}:rg{idx}`), not per date: a single
/// date-wide commit key would make the store's `commit_rows` treat every write after the FIRST one
/// for a given symbol as "already committed" (`Ok(0)`, no-op) — silently dropping that symbol's rows
/// from every row group after its first, since one token's rows can straddle two adjacent row
/// groups in a file sorted by `token_id`. See the module doc's "Streaming ingest" section.
fn ingest_stream_over<T: ChunkReader + Clone + 'static>(
    reader: T,
    store: &DataFusionHist,
    date: &str,
    stream: Stream,
    tokens: Option<&HashSet<String>>,
    mut on_row_group: impl FnMut(RowGroupProgress),
) -> Result<usize, CollectError> {
    let arrow_meta =
        ArrowReaderMetadata::load(&reader, ArrowReaderOptions::default()).map_err(|e| {
            CollectError::Fetch(format!("{CTX} parquet open {date}/{}: {e}", stream.file_name()))
        })?;
    let md = arrow_meta.metadata().clone();
    let selected = select_row_groups(&md, tokens);
    let total = selected.len();
    let start = Instant::now();
    let mut rows_cumulative = 0usize;
    for (index, &rg_idx) in selected.iter().enumerate() {
        // `.with_row_groups(vec![rg_idx])` scopes this reader to EXACTLY one row group's column
        // chunks; `reader.clone()`/`arrow_meta.clone()` are both cheap (pool-backed `ureq::Agent` /
        // `Bytes`, and an `Arc`-backed footer respectively) — no new HTTP HEAD, no footer re-fetch.
        let rg_reader =
            ParquetRecordBatchReaderBuilder::new_with_metadata(reader.clone(), arrow_meta.clone())
                .with_row_groups(vec![rg_idx])
                .build()
                .map_err(|e| {
                    CollectError::Fetch(format!(
                        "{CTX} parquet reader {date}/{} rg{rg_idx}: {e}",
                        stream.file_name()
                    ))
                })?;
        let commit_key = format!("vikearchive:{}:{date}:rg{rg_idx}", stream.kind());
        let rows_this_group = match stream {
            Stream::Book => ingest_book(rg_reader, store, tokens, &commit_key)?,
            Stream::Trade => ingest_trades(rg_reader, store, tokens, &commit_key)?,
            Stream::Quote => ingest_quotes(rg_reader, store, tokens, &commit_key)?,
        };
        rows_cumulative += rows_this_group;
        on_row_group(RowGroupProgress {
            index,
            total,
            compressed_bytes: md.row_group(rg_idx).compressed_size(),
            rows_this_group,
            rows_cumulative,
            elapsed_ms: start.elapsed().as_millis() as u64,
        });
        // `rg_reader` (and whatever it decoded) is dropped HERE, before the next iteration ever
        // opens a new row group's reader — the load-bearing property this whole rewrite exists for.
    }
    Ok(rows_cumulative)
}

/// The BULK/offline write-profile twin of [`ingest_stream_over`]: identical row-group-at-a-time
/// decode (same reader re-scoping, same one-row-group-resident-at-a-time memory bound), but each
/// row group's decoded rows are STAGED into `session` (via `end_batch`, so the session's own
/// [`vike_data::BulkConfig`] window decides when a real commit fires) instead of committed
/// straight to `store` one call per symbol per row group. `key_prefix` feeds `end_batch`/`flush`'s
/// own commit-key windowing (see [`ArchiveClient::ingest_stream_bulk`]'s doc); this function does
/// NOT flush the trailing partial window itself — the caller does that once after every stream in
/// a run (letting a caller batch several streams into one session before the final flush).
fn ingest_stream_bulk_over<T: ChunkReader + Clone + 'static>(
    reader: T,
    session: &mut BulkIngestSession<'_>,
    date: &str,
    stream: Stream,
    tokens: Option<&HashSet<String>>,
    key_prefix: &str,
    mut on_row_group: impl FnMut(RowGroupProgress),
) -> Result<usize, CollectError> {
    let arrow_meta =
        ArrowReaderMetadata::load(&reader, ArrowReaderOptions::default()).map_err(|e| {
            CollectError::Fetch(format!("{CTX} parquet open {date}/{}: {e}", stream.file_name()))
        })?;
    let md = arrow_meta.metadata().clone();
    let selected = select_row_groups(&md, tokens);
    let total = selected.len();
    let start = Instant::now();
    let mut rows_cumulative = 0usize;
    for (index, &rg_idx) in selected.iter().enumerate() {
        let rg_reader =
            ParquetRecordBatchReaderBuilder::new_with_metadata(reader.clone(), arrow_meta.clone())
                .with_row_groups(vec![rg_idx])
                .build()
                .map_err(|e| {
                    CollectError::Fetch(format!(
                        "{CTX} parquet reader {date}/{} rg{rg_idx}: {e}",
                        stream.file_name()
                    ))
                })?;
        let rows_this_group = match stream {
            Stream::Book => stage_book(session, rg_reader, tokens)?,
            Stream::Trade => stage_trades(session, rg_reader, tokens)?,
            Stream::Quote => stage_quotes(session, rg_reader, tokens)?,
        };
        // one logical "batch" per row group — the session flushes on its own window, not here
        session.end_batch(key_prefix).map_err(|e| {
            CollectError::Fetch(format!("{CTX} bulk flush {date}/{}: {e}", stream.file_name()))
        })?;
        rows_cumulative += rows_this_group;
        on_row_group(RowGroupProgress {
            index,
            total,
            compressed_bytes: md.row_group(rg_idx).compressed_size(),
            rows_this_group,
            rows_cumulative,
            elapsed_ms: start.elapsed().as_millis() as u64,
        });
        // `rg_reader` (and whatever it decoded) is dropped HERE, exactly like the live path — the
        // BUFFERED rows it staged into `session` remain resident until the next flush, bounded by
        // `session`'s own `BulkConfig` window, never by the whole file.
    }
    Ok(rows_cumulative)
}

fn keep(tokens: Option<&HashSet<String>>, symbol: &str) -> bool {
    tokens.is_none_or(|allow| allow.contains(symbol))
}

// ---- decode ONE row group's reader into a per-symbol map (shared by the LIVE and BULK commit
// paths below — only what happens to the decoded map differs between the two profiles) ----------

fn decode_book_row_group(
    reader: ParquetRecordBatchReader,
    tokens: Option<&HashSet<String>>,
) -> Result<HashMap<String, Vec<BookUpdate>>, CollectError> {
    let mut per_symbol: HashMap<String, Vec<BookUpdate>> = HashMap::new();
    for batch in reader {
        let batch =
            batch.map_err(|e| CollectError::Fetch(format!("{CTX} book_events batch: {e}")))?;
        for u in book_updates_from_batch(&batch)? {
            if keep(tokens, &u.symbol) {
                per_symbol.entry(u.symbol.clone()).or_default().push(u);
            }
        }
    }
    Ok(per_symbol)
}

fn decode_trades_row_group(
    reader: ParquetRecordBatchReader,
    tokens: Option<&HashSet<String>>,
) -> Result<HashMap<String, Vec<TradeTick>>, CollectError> {
    let mut per_symbol: HashMap<String, Vec<TradeTick>> = HashMap::new();
    for batch in reader {
        let batch = batch.map_err(|e| CollectError::Fetch(format!("{CTX} trades batch: {e}")))?;
        for t in trades_from_batch(&batch)? {
            if keep(tokens, &t.symbol) {
                per_symbol.entry(t.symbol.clone()).or_default().push(t);
            }
        }
    }
    Ok(per_symbol)
}

fn decode_quotes_row_group(
    reader: ParquetRecordBatchReader,
    tokens: Option<&HashSet<String>>,
) -> Result<HashMap<String, Vec<QuoteTick>>, CollectError> {
    let mut per_symbol: HashMap<String, Vec<QuoteTick>> = HashMap::new();
    for batch in reader {
        let batch =
            batch.map_err(|e| CollectError::Fetch(format!("{CTX} l1_quotes batch: {e}")))?;
        for q in quotes_from_batch(&batch)? {
            if keep(tokens, &q.symbol) {
                per_symbol.entry(q.symbol.clone()).or_default().push(q);
            }
        }
    }
    Ok(per_symbol)
}

// ---- LIVE commit: one `HistStore::append_*` call per symbol per row group (unchanged path) -----

fn ingest_book(
    reader: ParquetRecordBatchReader,
    store: &DataFusionHist,
    tokens: Option<&HashSet<String>>,
    commit_key: &str,
) -> Result<usize, CollectError> {
    let per_symbol = decode_book_row_group(reader, tokens)?;
    let mut total = 0usize;
    for (symbol, updates) in per_symbol {
        total += store.append_book_updates(VENUE, &symbol, &updates, Some(commit_key))?;
    }
    Ok(total)
}

fn ingest_trades(
    reader: ParquetRecordBatchReader,
    store: &DataFusionHist,
    tokens: Option<&HashSet<String>>,
    commit_key: &str,
) -> Result<usize, CollectError> {
    let per_symbol = decode_trades_row_group(reader, tokens)?;
    let mut total = 0usize;
    for (symbol, trades) in per_symbol {
        total += store.append_trades(VENUE, &symbol, &trades, Some(commit_key))?;
    }
    Ok(total)
}

fn ingest_quotes(
    reader: ParquetRecordBatchReader,
    store: &DataFusionHist,
    tokens: Option<&HashSet<String>>,
    commit_key: &str,
) -> Result<usize, CollectError> {
    let per_symbol = decode_quotes_row_group(reader, tokens)?;
    let mut total = 0usize;
    for (symbol, quotes) in per_symbol {
        total += store.append_quotes(VENUE, &symbol, &quotes, Some(commit_key))?;
    }
    Ok(total)
}

// ---- BULK commit: stage into the session; the session's own window decides when a real commit
// happens (see `vike_data::bulk`'s module doc). Decoded row counts are returned so the caller can
// still report per-row-group progress the same way the live path does. --------------------------

fn stage_book(
    session: &mut BulkIngestSession<'_>,
    reader: ParquetRecordBatchReader,
    tokens: Option<&HashSet<String>>,
) -> Result<usize, CollectError> {
    let per_symbol = decode_book_row_group(reader, tokens)?;
    let mut total = 0usize;
    for (symbol, updates) in per_symbol {
        total += updates.len();
        session.stage_book_updates(VENUE, &symbol, &updates);
    }
    Ok(total)
}

fn stage_trades(
    session: &mut BulkIngestSession<'_>,
    reader: ParquetRecordBatchReader,
    tokens: Option<&HashSet<String>>,
) -> Result<usize, CollectError> {
    let per_symbol = decode_trades_row_group(reader, tokens)?;
    let mut total = 0usize;
    for (symbol, trades) in per_symbol {
        total += trades.len();
        session.stage_trades(VENUE, &symbol, &trades);
    }
    Ok(total)
}

fn stage_quotes(
    session: &mut BulkIngestSession<'_>,
    reader: ParquetRecordBatchReader,
    tokens: Option<&HashSet<String>>,
) -> Result<usize, CollectError> {
    let per_symbol = decode_quotes_row_group(reader, tokens)?;
    let mut total = 0usize;
    for (symbol, quotes) in per_symbol {
        total += quotes.len();
        session.stage_quotes(VENUE, &symbol, &quotes);
    }
    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::*;
    use datafusion::arrow::array::{Array, ArrayRef};
    use datafusion::arrow::datatypes::{DataType, Field, Schema};
    use datafusion::parquet::arrow::ArrowWriter;
    use datafusion::parquet::file::properties::WriterProperties;
    use vike_data::{BulkConfig, TsRange};

    // ---- Stream / URL builders -----------------------------------------------------------------

    #[test]
    fn stream_file_names_and_kinds_round_trip() {
        for s in Stream::all() {
            assert_eq!(Stream::from_kind(s.kind()), Some(s));
        }
        assert_eq!(Stream::Book.file_name(), "book_events");
        assert_eq!(Stream::Trade.file_name(), "trades");
        assert_eq!(Stream::Quote.file_name(), "l1_quotes");
        assert_eq!(Stream::from_kind("garbage"), None);
    }

    #[test]
    fn stream_url_matches_the_hive_layout() {
        let url = stream_url(DEFAULT_BASE, "2026-07-27", Stream::Book);
        assert_eq!(
            url,
            "https://data.vike.io/archive/venue=polymarket/date=2026-07-27/book_events.parquet"
        );
        assert_eq!(manifest_url(DEFAULT_BASE), "https://data.vike.io/archive/manifest.json");
    }

    // ---- v1 discovery: pure URL/query builders (no network) -----------------------------------

    #[test]
    fn default_discovery_base_strips_the_archive_suffix() {
        assert_eq!(default_discovery_base(DEFAULT_BASE), "https://data.vike.io");
        // A base that doesn't end in `/archive` passes through unchanged rather than guessing.
        assert_eq!(
            default_discovery_base("https://data.vike.io/other"),
            "https://data.vike.io/other"
        );
    }

    #[test]
    fn family_stream_url_matches_the_documented_layout() {
        let url = family_stream_url(DEFAULT_BASE, "btc", "5m", "2026-07-27", Stream::Book);
        assert_eq!(
            url,
            "https://data.vike.io/archive/venue=polymarket/asset=btc/tenor=5m/date=2026-07-27/book_events.parquet"
        );
    }

    #[test]
    fn datasets_query_is_empty_when_every_filter_is_none() {
        assert_eq!(datasets_query(&DatasetFilter::default()), "");
    }

    #[test]
    fn datasets_query_builds_only_the_set_filters_in_order() {
        let f = DatasetFilter {
            asset: Some("btc".to_string()),
            tenor: Some("5m".to_string()),
            stream: Some(Stream::Book),
            from: Some("2026-07-26".to_string()),
            to: Some("2026-07-27".to_string()),
            limit: Some(50),
            offset: Some(10),
        };
        assert_eq!(
            datasets_query(&f),
            "?asset=btc&tenor=5m&stream=book_events&from=2026-07-26&to=2026-07-27&limit=50&offset=10"
        );
        // A partial filter only emits the fields that are actually set.
        let partial = DatasetFilter { asset: Some("eth".to_string()), ..Default::default() };
        assert_eq!(datasets_query(&partial), "?asset=eth");
    }

    #[test]
    fn resolve_query_appends_asset_tenor_only_when_both_present() {
        assert_eq!(
            resolve_query("2026-07-27", Stream::Book, None, None),
            "?date=2026-07-27&stream=book_events"
        );
        assert_eq!(
            resolve_query("2026-07-27", Stream::Book, Some("btc"), Some("5m")),
            "?date=2026-07-27&stream=book_events&asset=btc&tenor=5m"
        );
    }

    // ---- v1 discovery: JSON decode (pure; fixtures matching the real live-verified shapes) -----

    const DATASETS_PAGE_FIXTURE: &str = r#"{
        "datasets": [
            {
                "venue": "polymarket", "layout": "flat", "date": "2026-07-27",
                "stream": "book_events", "bytes": 7402961414, "rows": null,
                "url": "https://data.vike.io/archive/venue=polymarket/date=2026-07-27/book_events.parquet"
            },
            {
                "venue": "polymarket", "layout": "family", "asset": "btc", "tenor": "5m",
                "date": "2026-07-27", "stream": "book_events", "bytes": 1477132119, "rows": 86945860,
                "url": "https://data.vike.io/archive/venue=polymarket/asset=btc/tenor=5m/date=2026-07-27/book_events.parquet"
            }
        ],
        "total": 75, "limit": 200, "offset": 0
    }"#;

    #[test]
    fn parse_datasets_page_decodes_flat_rows_without_asset_tenor_and_family_rows_with_them() {
        let page = parse_datasets_page(DATASETS_PAGE_FIXTURE).unwrap();
        assert_eq!(page.total, 75);
        assert_eq!(page.limit, 200);
        assert_eq!(page.offset, 0);
        assert_eq!(page.datasets.len(), 2);

        let flat = &page.datasets[0];
        assert_eq!(flat.layout, "flat");
        assert_eq!(flat.asset, None, "flat rows omit asset, not null-but-present");
        assert_eq!(flat.tenor, None);
        assert_eq!(flat.rows, None, "vendor publishes null row counts for flat rows");

        let family = &page.datasets[1];
        assert_eq!(family.layout, "family");
        assert_eq!(family.asset.as_deref(), Some("btc"));
        assert_eq!(family.tenor.as_deref(), Some("5m"));
        assert_eq!(family.bytes, 1_477_132_119);
        assert_eq!(family.rows, Some(86_945_860));
    }

    #[test]
    fn parse_datasets_page_rejects_garbage() {
        assert!(parse_datasets_page("not json").is_err());
    }

    /// The exact real response shape captured live against the production archive
    /// (`GET /v1/archive/url?asset=btc&tenor=5m&date=2026-07-27&stream=book_events`).
    const RESOLVED_DATASET_FIXTURE: &str = r#"{
        "asset":"btc","bytes":1477132119,"date":"2026-07-27","layout":"family","rows":86945860,
        "tenor":"5m","stream":"book_events","venue":"polymarket",
        "url":"https://data.vike.io/archive/venue=polymarket/asset=btc/tenor=5m/date=2026-07-27/book_events.parquet"
    }"#;

    #[test]
    fn parse_dataset_decodes_the_real_resolved_shape() {
        let d = parse_dataset(RESOLVED_DATASET_FIXTURE).unwrap();
        assert_eq!(d.venue, "polymarket");
        assert_eq!(d.layout, "family");
        assert_eq!(d.asset.as_deref(), Some("btc"));
        assert_eq!(d.tenor.as_deref(), Some("5m"));
        assert_eq!(d.date, "2026-07-27");
        assert_eq!(d.stream, "book_events");
        assert_eq!(d.bytes, 1_477_132_119);
        assert_eq!(d.rows, Some(86_945_860));
        assert_eq!(
            d.url,
            "https://data.vike.io/archive/venue=polymarket/asset=btc/tenor=5m/date=2026-07-27/book_events.parquet"
        );
    }

    #[test]
    fn parse_dataset_rejects_garbage() {
        assert!(parse_dataset("not json").is_err());
    }

    /// The selection precedence [`ArchiveClient::resolve_family_url`] implements: when discovery is
    /// unreachable, it MUST fall back to the exact string [`family_stream_url`] builds — never
    /// panic, never silently ingest nothing. `127.0.0.1:1` is an unassigned port with nothing
    /// listening, so the connect fails immediately and deterministically without touching the
    /// internet — an offline-safe way to unit-test the real fallback wiring (not just the pure
    /// builder function in isolation).
    #[test]
    fn resolve_family_url_falls_back_to_the_constructed_path_when_discovery_is_unreachable() {
        let client = ArchiveClient::new(DEFAULT_BASE, None);
        let target =
            FamilyTarget { discovery_base: "http://127.0.0.1:1", asset: "btc", tenor: "5m" };
        let url = client.resolve_family_url(&target, "2026-07-27", Stream::Book);
        assert_eq!(
            url,
            family_stream_url(DEFAULT_BASE, "btc", "5m", "2026-07-27", Stream::Book),
            "an unreachable discovery endpoint must fall back to the directly-constructed URL"
        );
    }

    // ---- manifest parse (pure; a trimmed real-shape fixture) -----------------------------------

    const MANIFEST_FIXTURE: &str = r#"{
        "venue": "polymarket",
        "generated_at": "2026-07-28T21:19:30Z",
        "partitioning": "venue=polymarket/date=YYYY-MM-DD/{book_events,trades,l1_quotes}.parquet",
        "schema": {"book_events": [{"name": "token_id", "type": "String"}]},
        "auth": {"scheme": "header", "header": "X-API-Key"},
        "range_requests": true,
        "free_sample": {"path": "/archive/samples/"},
        "dates": [
            {
                "date": "2026-07-27",
                "streams": {
                    "book_events": {"rows": null, "bytes": 7402961414},
                    "trades": {"rows": null, "bytes": 15490280},
                    "l1_quotes": {"rows": null, "bytes": 617280628}
                }
            },
            {
                "date": "2026-07-26",
                "streams": {
                    "book_events": {"rows": null, "bytes": 6537528677},
                    "trades": {"rows": null, "bytes": 14202938},
                    "l1_quotes": {"rows": null, "bytes": 466315902}
                }
            }
        ]
    }"#;

    #[test]
    fn parse_manifest_decodes_dates_and_stream_sizes_ignoring_unknown_top_level_fields() {
        let m = parse_manifest(MANIFEST_FIXTURE).unwrap();
        assert_eq!(m.venue, "polymarket");
        assert!(m.range_requests);
        assert_eq!(m.dates.len(), 2);
        assert_eq!(m.dates[0].date, "2026-07-27");
        let book = &m.dates[0].streams["book_events"];
        assert_eq!(book.bytes, 7_402_961_414);
        assert_eq!(book.rows, None, "vendor publishes null row counts today");
        assert_eq!(m.dates[1].streams["trades"].bytes, 14_202_938);
    }

    #[test]
    fn parse_manifest_rejects_garbage() {
        assert!(parse_manifest("not json").is_err());
    }

    // ---- level JSON decode ----------------------------------------------------------------------

    #[test]
    fn parse_levels_json_handles_numbers_empty_and_garbage() {
        assert_eq!(parse_levels_json(""), Vec::<(f64, f64)>::new());
        assert_eq!(
            parse_levels_json("[[0.5,100.0],[0.49,50.0]]"),
            vec![(0.5, 100.0), (0.49, 50.0)]
        );
        assert_eq!(parse_levels_json("not json"), Vec::<(f64, f64)>::new());
    }

    // ---- decimal scale decode ---------------------------------------------------------------------

    #[test]
    fn decimal_scales_decode_price_and_size() {
        let arr = Decimal128Array::from(vec![1390_i128, 5_208_325_i128])
            .with_precision_and_scale(18, 6)
            .unwrap();
        assert!((dec(&arr, 0, PRICE_SCALE_DIVISOR) - 0.139).abs() < 1e-12);
        assert!((dec(&arr, 1, SIZE_SCALE_DIVISOR) - 5.208325).abs() < 1e-12);
    }

    // ---- book_events -> BookUpdate (synthetic RecordBatch, no Parquet round trip) --------------

    type BookRow<'a> =
        (&'a str, i64, i64, u64, &'a str, &'a str, f64, f64, &'a str, &'a str, f64, &'a str);

    fn book_events_batch(rows: &[BookRow<'_>]) -> RecordBatch {
        let schema = Schema::new(vec![
            Field::new("token_id", DataType::Utf8, false),
            Field::new("ts", DataType::Int64, false),
            Field::new("local_ts", DataType::Int64, false),
            Field::new("seq", DataType::UInt64, false),
            Field::new("event_type", DataType::Binary, false),
            Field::new("side", DataType::Binary, false),
            Field::new("price", DataType::Decimal128(9, 4), false),
            Field::new("size", DataType::Decimal128(18, 6), false),
            Field::new("bids", DataType::Utf8, false),
            Field::new("asks", DataType::Utf8, false),
            Field::new("tick_size", DataType::Decimal128(9, 4), false),
            Field::new("status", DataType::Utf8, false),
        ]);
        let price = Decimal128Array::from(
            rows.iter().map(|r| (r.6 * 10_000.0).round() as i128).collect::<Vec<_>>(),
        )
        .with_precision_and_scale(9, 4)
        .unwrap();
        let size = Decimal128Array::from(
            rows.iter().map(|r| (r.7 * 1_000_000.0).round() as i128).collect::<Vec<_>>(),
        )
        .with_precision_and_scale(18, 6)
        .unwrap();
        let tick_size = Decimal128Array::from(
            rows.iter().map(|r| (r.10 * 10_000.0).round() as i128).collect::<Vec<_>>(),
        )
        .with_precision_and_scale(9, 4)
        .unwrap();
        RecordBatch::try_new(
            Arc::new(schema),
            vec![
                Arc::new(StringArray::from(rows.iter().map(|r| r.0).collect::<Vec<_>>())),
                Arc::new(Int64Array::from(rows.iter().map(|r| r.1).collect::<Vec<_>>())),
                Arc::new(Int64Array::from(rows.iter().map(|r| r.2).collect::<Vec<_>>())),
                Arc::new(UInt64Array::from(rows.iter().map(|r| r.3).collect::<Vec<_>>())),
                Arc::new(BinaryArray::from(
                    rows.iter().map(|r| r.4.as_bytes()).collect::<Vec<_>>(),
                )),
                Arc::new(BinaryArray::from(
                    rows.iter().map(|r| r.5.as_bytes()).collect::<Vec<_>>(),
                )),
                Arc::new(price) as ArrayRef,
                Arc::new(size) as ArrayRef,
                Arc::new(StringArray::from(rows.iter().map(|r| r.8).collect::<Vec<_>>())),
                Arc::new(StringArray::from(rows.iter().map(|r| r.9).collect::<Vec<_>>())),
                Arc::new(tick_size) as ArrayRef,
                Arc::new(StringArray::from(rows.iter().map(|r| r.11).collect::<Vec<_>>())),
            ],
        )
        .unwrap()
    }

    /// The FAMILY-layout twin of [`book_events_batch`]: identical row shape, but `event_type`/
    /// `side` are `Utf8` instead of `Binary` — the real, live-verified physical-type divergence
    /// between the flat and family `book_events` exports (see [`StrOrBinCol`]'s doc). Proves
    /// [`book_updates_from_batch`] decodes the family layout's encoding too, not just the flat one.
    fn book_events_batch_family_utf8(rows: &[BookRow<'_>]) -> RecordBatch {
        let schema = Schema::new(vec![
            Field::new("token_id", DataType::Utf8, false),
            Field::new("ts", DataType::Int64, false),
            Field::new("local_ts", DataType::Int64, false),
            Field::new("seq", DataType::UInt64, false),
            Field::new("event_type", DataType::Utf8, false),
            Field::new("side", DataType::Utf8, false),
            Field::new("price", DataType::Decimal128(9, 4), false),
            Field::new("size", DataType::Decimal128(18, 6), false),
            Field::new("bids", DataType::Utf8, false),
            Field::new("asks", DataType::Utf8, false),
            Field::new("tick_size", DataType::Decimal128(9, 4), false),
            Field::new("status", DataType::Utf8, false),
        ]);
        let price = Decimal128Array::from(
            rows.iter().map(|r| (r.6 * 10_000.0).round() as i128).collect::<Vec<_>>(),
        )
        .with_precision_and_scale(9, 4)
        .unwrap();
        let size = Decimal128Array::from(
            rows.iter().map(|r| (r.7 * 1_000_000.0).round() as i128).collect::<Vec<_>>(),
        )
        .with_precision_and_scale(18, 6)
        .unwrap();
        let tick_size = Decimal128Array::from(
            rows.iter().map(|r| (r.10 * 10_000.0).round() as i128).collect::<Vec<_>>(),
        )
        .with_precision_and_scale(9, 4)
        .unwrap();
        RecordBatch::try_new(
            Arc::new(schema),
            vec![
                Arc::new(StringArray::from(rows.iter().map(|r| r.0).collect::<Vec<_>>())),
                Arc::new(Int64Array::from(rows.iter().map(|r| r.1).collect::<Vec<_>>())),
                Arc::new(Int64Array::from(rows.iter().map(|r| r.2).collect::<Vec<_>>())),
                Arc::new(UInt64Array::from(rows.iter().map(|r| r.3).collect::<Vec<_>>())),
                Arc::new(StringArray::from(rows.iter().map(|r| r.4).collect::<Vec<_>>())),
                Arc::new(StringArray::from(rows.iter().map(|r| r.5).collect::<Vec<_>>())),
                Arc::new(price) as ArrayRef,
                Arc::new(size) as ArrayRef,
                Arc::new(StringArray::from(rows.iter().map(|r| r.8).collect::<Vec<_>>())),
                Arc::new(StringArray::from(rows.iter().map(|r| r.9).collect::<Vec<_>>())),
                Arc::new(tick_size) as ArrayRef,
                Arc::new(StringArray::from(rows.iter().map(|r| r.11).collect::<Vec<_>>())),
            ],
        )
        .unwrap()
    }

    #[test]
    fn family_layouts_utf8_event_type_and_side_decode_identically_to_flats_binary_encoding() {
        let rows: Vec<BookRow<'_>> = vec![
            (
                "TOKA",
                1_700_000_000_000,
                1_700_000_000_003,
                1,
                "book",
                "none",
                0.0,
                0.0,
                "[[0.5,100.0]]",
                "[[0.51,80.0]]",
                0.01,
                "",
            ),
            ("TOKA", 1, 2, 5, "price_change", "buy", 0.42, 7.0, "", "", 0.01, ""),
            ("TOKA", 1, 2, 6, "price_change", "sell", 0.60, 0.0, "", "", 0.01, ""),
            ("TOKA", 9, 10, 0, "status", "none", 0.0, 0.0, "", "", 0.0, "gap_start"),
        ];
        let flat = book_updates_from_batch(&book_events_batch(&rows)).unwrap();
        let family = book_updates_from_batch(&book_events_batch_family_utf8(&rows)).unwrap();
        // `BookUpdate` derives no `PartialEq` (vike-model), so compare via `Debug` — still a
        // precise structural comparison, just not the `==` operator.
        assert_eq!(
            format!("{flat:?}"),
            format!("{family:?}"),
            "the same logical rows must decode identically regardless of layout"
        );
        assert_eq!(flat.len(), 4);
    }

    #[test]
    fn snapshot_row_decodes_full_depth_and_per_row_symbol() {
        let b = book_events_batch(&[(
            "TOKA",
            1_700_000_000_000,
            1_700_000_000_003,
            1,
            "book",
            "none",
            0.0,
            0.0,
            "[[0.5,100.0]]",
            "[[0.51,80.0]]",
            0.01,
            "",
        )]);
        let out = book_updates_from_batch(&b).unwrap();
        assert_eq!(out.len(), 1);
        let u = &out[0];
        assert_eq!(u.kind, BookUpdateKind::Snapshot);
        assert_eq!(u.ts, 1_700_000_000_000);
        assert_eq!(u.local_ts, 1_700_000_000_003);
        assert_eq!(u.seq, 1);
        assert_eq!(u.bids, vec![(0.5, 100.0)]);
        assert_eq!(u.asks, vec![(0.51, 80.0)]);
        assert!((u.tick_size - 0.01).abs() < 1e-12);
        assert_eq!(u.symbol, "TOKA");
    }

    #[test]
    fn delta_row_decodes_the_populated_side_from_decimal_columns() {
        let b = book_events_batch(&[
            ("TOKA", 1, 2, 5, "price_change", "buy", 0.42, 7.0, "", "", 0.01, ""),
            ("TOKA", 1, 2, 6, "price_change", "sell", 0.60, 0.0, "", "", 0.01, ""),
        ]);
        let out = book_updates_from_batch(&b).unwrap();
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].kind, BookUpdateKind::Delta);
        assert!((out[0].bids[0].0 - 0.42).abs() < 1e-9);
        assert!((out[0].bids[0].1 - 7.0).abs() < 1e-9);
        assert!(out[0].asks.is_empty());
        assert!(out[1].bids.is_empty());
        assert!((out[1].asks[0].0 - 0.60).abs() < 1e-9);
    }

    #[test]
    fn status_rows_map_to_the_right_kind_with_empty_levels() {
        let b = book_events_batch(&[
            ("TOKA", 9, 10, 0, "status", "none", 0.0, 0.0, "", "", 0.0, "gap_start"),
            ("TOKA", 9, 11, 0, "status", "none", 0.0, 0.0, "", "", 0.0, "stale"),
            ("TOKA", 9, 12, 0, "status", "none", 0.0, 0.0, "", "", 0.0, "live_resume"),
        ]);
        let out = book_updates_from_batch(&b).unwrap();
        assert_eq!(out.len(), 3);
        assert_eq!(out[0].kind, BookUpdateKind::GapStart);
        assert_eq!(out[1].kind, BookUpdateKind::Stale);
        assert_eq!(out[2].kind, BookUpdateKind::LiveResume);
        assert!(out.iter().all(|u| u.bids.is_empty() && u.asks.is_empty()));
    }

    #[test]
    fn trade_and_tick_size_change_rows_are_skipped() {
        let b = book_events_batch(&[
            ("TOKA", 1, 2, 0, "trade", "sell", 0.5, 1.0, "", "", 0.0, ""),
            ("TOKA", 1, 2, 0, "tick_size_change", "none", 0.0, 0.0, "", "", 0.02, ""),
        ]);
        let out = book_updates_from_batch(&b).unwrap();
        assert!(out.is_empty(), "trade/tick_size_change carry no BookUpdate: {out:?}");
    }

    #[test]
    fn mixed_tokens_in_one_batch_keep_their_own_symbol() {
        let b = book_events_batch(&[
            ("TOKA", 1, 1, 0, "book", "none", 0.0, 0.0, "[]", "[]", 0.01, ""),
            ("TOKB", 2, 2, 0, "book", "none", 0.0, 0.0, "[]", "[]", 0.01, ""),
        ]);
        let out = book_updates_from_batch(&b).unwrap();
        assert_eq!(out[0].symbol, "TOKA");
        assert_eq!(out[1].symbol, "TOKB");
    }

    // ---- trades -> TradeTick ----------------------------------------------------------------------

    fn trades_batch(rows: &[(&str, i64, i64, f64, f64, &str)]) -> RecordBatch {
        let schema = Schema::new(vec![
            Field::new("token_id", DataType::Utf8, false),
            Field::new("ts", DataType::Int64, false),
            Field::new("local_ts", DataType::Int64, false),
            Field::new("price", DataType::Float64, false),
            Field::new("size", DataType::Float64, false),
            Field::new("side", DataType::Utf8, false),
        ]);
        RecordBatch::try_new(
            Arc::new(schema),
            vec![
                Arc::new(StringArray::from(rows.iter().map(|r| r.0).collect::<Vec<_>>())),
                Arc::new(Int64Array::from(rows.iter().map(|r| r.1).collect::<Vec<_>>())),
                Arc::new(Int64Array::from(rows.iter().map(|r| r.2).collect::<Vec<_>>())),
                Arc::new(Float64Array::from(rows.iter().map(|r| r.3).collect::<Vec<_>>())),
                Arc::new(Float64Array::from(rows.iter().map(|r| r.4).collect::<Vec<_>>())),
                Arc::new(StringArray::from(rows.iter().map(|r| r.5).collect::<Vec<_>>())),
            ],
        )
        .unwrap()
    }

    #[test]
    fn trades_from_batch_inverts_the_taker_side_convention() {
        let b = trades_batch(&[
            ("TOKA", 1_700_000_000_100, 1_700_000_000_101, 0.95, 3.0, "sell"),
            ("TOKA", 1_700_000_000_200, 1_700_000_000_201, 0.10, 1.0, "buy"),
        ]);
        let out = trades_from_batch(&b).unwrap();
        assert_eq!(out.len(), 2);
        assert!(out[0].is_buyer_maker, "side=sell -> taker sold -> is_buyer_maker=true");
        assert_eq!(out[0].price, 0.95);
        assert_eq!(out[0].size, 3.0);
        assert_eq!(out[0].symbol, "TOKA");
        assert!(!out[1].is_buyer_maker, "side=buy -> taker bought -> is_buyer_maker=false");
    }

    // ---- l1_quotes -> QuoteTick -------------------------------------------------------------------

    #[test]
    fn quotes_from_batch_decodes_decimal_l1() {
        let schema = Schema::new(vec![
            Field::new("token_id", DataType::Utf8, false),
            Field::new("ts", DataType::Int64, false),
            Field::new("local_ts", DataType::Int64, false),
            Field::new("bid", DataType::Decimal128(9, 4), false),
            Field::new("ask", DataType::Decimal128(9, 4), false),
            Field::new("bid_size", DataType::Decimal128(18, 6), false),
            Field::new("ask_size", DataType::Decimal128(18, 6), false),
        ]);
        let bid = Decimal128Array::from(vec![4_400_i128]).with_precision_and_scale(9, 4).unwrap();
        let ask = Decimal128Array::from(vec![4_700_i128]).with_precision_and_scale(9, 4).unwrap();
        let bid_size =
            Decimal128Array::from(vec![10_000_000_i128]).with_precision_and_scale(18, 6).unwrap();
        let ask_size =
            Decimal128Array::from(vec![8_000_000_i128]).with_precision_and_scale(18, 6).unwrap();
        let b = RecordBatch::try_new(
            Arc::new(schema),
            vec![
                Arc::new(StringArray::from(vec!["TOKA"])),
                Arc::new(Int64Array::from(vec![1_700_000_000_200_i64])),
                Arc::new(Int64Array::from(vec![1_700_000_000_201_i64])),
                Arc::new(bid) as ArrayRef,
                Arc::new(ask) as ArrayRef,
                Arc::new(bid_size) as ArrayRef,
                Arc::new(ask_size) as ArrayRef,
            ],
        )
        .unwrap();
        let out = quotes_from_batch(&b).unwrap();
        assert_eq!(out.len(), 1);
        assert!((out[0].bid - 0.44).abs() < 1e-9);
        assert!((out[0].ask - 0.47).abs() < 1e-9);
        assert!((out[0].bid_size - 10.0).abs() < 1e-9);
        assert!((out[0].ask_size - 8.0).abs() < 1e-9);
        assert_eq!(out[0].symbol, "TOKA");
    }

    // ---- row-group pruning: build a REAL multi-row-group Parquet file, decode metadata back ------

    /// Writes `token_ids` (one row per id, one row group per row via `set_max_row_group_size(1)`)
    /// as a minimal single-column-plus-companions Parquet file entirely in memory, and returns the
    /// bytes — the "build a tiny RecordBatch, write to bytes, decode back" fixture the design brief
    /// asked for, exercising the REAL Parquet writer's row-group statistics (not hand-built stats).
    fn write_single_column_parquet(token_ids: &[&str]) -> Bytes {
        let schema = Arc::new(Schema::new(vec![Field::new("token_id", DataType::Utf8, false)]));
        let batch = RecordBatch::try_new(
            schema.clone(),
            vec![Arc::new(StringArray::from(token_ids.to_vec())) as ArrayRef],
        )
        .unwrap();
        let props = WriterProperties::builder().set_max_row_group_row_count(Some(1)).build();
        let mut buf = Vec::new();
        {
            let mut writer = ArrowWriter::try_new(&mut buf, schema, Some(props)).unwrap();
            writer.write(&batch).unwrap();
            writer.close().unwrap();
        }
        Bytes::from(buf)
    }

    #[test]
    fn select_row_groups_prunes_to_the_matching_groups_only() {
        // Distinct, sorted token ids -> one row group per id (max_row_group_size=1), each group's
        // min==max==that id — an unambiguous test of the byte-range containment check.
        let ids = ["TOK_A", "TOK_B", "TOK_C", "TOK_D"];
        let bytes = write_single_column_parquet(&ids);
        let builder = ParquetRecordBatchReaderBuilder::try_new(bytes).unwrap();
        let md = builder.metadata();
        assert_eq!(md.num_row_groups(), 4, "one row group per row (max_row_group_size=1)");

        let mut want = HashSet::new();
        want.insert("TOK_B".to_string());
        let selected = select_row_groups(md, Some(&want));
        assert_eq!(selected, vec![1], "only TOK_B's own row group is selected");

        // No filter -> every row group.
        assert_eq!(select_row_groups(md, None), vec![0, 1, 2, 3]);

        // A token absent from the file -> no row group matches (assuming it sorts outside every
        // group's [min,max] — true here since "TOK_Z" > every group's max byte-wise).
        let mut absent = HashSet::new();
        absent.insert("TOK_Z".to_string());
        assert!(select_row_groups(md, Some(&absent)).is_empty());

        // Empty filter set -> nothing selected (never "everything").
        assert!(select_row_groups(md, Some(&HashSet::new())).is_empty());
    }

    #[test]
    fn select_row_groups_reads_end_to_end_through_the_real_reader() {
        // Full round trip: write -> build reader with ONLY the pruned row groups -> decode -> the
        // resulting rows are exactly (and only) the requested token's.
        let ids = ["TOK_A", "TOK_B", "TOK_C"];
        let bytes = write_single_column_parquet(&ids);
        let builder = ParquetRecordBatchReaderBuilder::try_new(bytes).unwrap();
        let mut want = HashSet::new();
        want.insert("TOK_C".to_string());
        let selected = select_row_groups(builder.metadata(), Some(&want));
        let reader = builder.with_row_groups(selected).build().unwrap();
        let mut seen = Vec::new();
        for batch in reader {
            let batch = batch.unwrap();
            let col = batch.column(0).as_any().downcast_ref::<StringArray>().unwrap();
            for i in 0..batch.num_rows() {
                seen.push(col.value(i).to_string());
            }
        }
        assert_eq!(seen, vec!["TOK_C".to_string()]);
    }

    // ---- streaming ingest: memory-bounded per-row-group flush (the defect this PR fixes) --------

    /// Writes `rows` as a `book_events`-shaped Parquet file (the same 12-column schema
    /// [`book_events_batch`] builds), forced to split into row groups of at most
    /// `max_rows_per_group` rows each (`ArrowWriter` auto-splits a single `write()` call across
    /// row-group boundaries once `WriterProperties::set_max_row_group_row_count` is set — the same
    /// mechanism [`write_single_column_parquet`] already relies on above).
    fn write_book_events_parquet(rows: &[BookRow<'_>], max_rows_per_group: usize) -> Bytes {
        let batch = book_events_batch(rows);
        let schema = batch.schema();
        let props = WriterProperties::builder()
            .set_max_row_group_row_count(Some(max_rows_per_group))
            .build();
        let mut buf = Vec::new();
        {
            let mut writer = ArrowWriter::try_new(&mut buf, schema, Some(props)).unwrap();
            writer.write(&batch).unwrap();
            writer.close().unwrap();
        }
        Bytes::from(buf)
    }

    /// The headline proof for this PR: [`ingest_stream_over`] flushes ONE row group at a time — not
    /// after draining the whole file. Three distinct tokens, one row group each
    /// (`max_rows_per_group=1`): the progress callback must fire exactly once per row group, in
    /// order, with a running cumulative count — and, load-bearing, EACH row group's data must
    /// already be durable in the store by the time its own callback fires (checked with a real
    /// `HistStore::scan_book_updates` from INSIDE the callback) — proving the write happens before
    /// the next row group is ever opened, not batched at the end. This is the structural substitute
    /// for an in-process RSS assertion (impractical here); the actual peak-RSS measurement lives in
    /// the PR's live the CI box proof, not in this unit test.
    #[test]
    fn ingest_stream_over_flushes_each_row_group_before_moving_to_the_next() {
        let rows: Vec<BookRow<'_>> = vec![
            (
                "TOK_A",
                10,
                10,
                1,
                "book",
                "none",
                0.0,
                0.0,
                "[[0.5,10.0]]",
                "[[0.51,10.0]]",
                0.01,
                "",
            ),
            (
                "TOK_B",
                20,
                20,
                1,
                "book",
                "none",
                0.0,
                0.0,
                "[[0.4,20.0]]",
                "[[0.41,20.0]]",
                0.01,
                "",
            ),
            (
                "TOK_C",
                30,
                30,
                1,
                "book",
                "none",
                0.0,
                0.0,
                "[[0.3,30.0]]",
                "[[0.31,30.0]]",
                0.01,
                "",
            ),
        ];
        let bytes = write_book_events_parquet(&rows, 1);
        let probe = ParquetRecordBatchReaderBuilder::try_new(bytes.clone()).unwrap();
        assert_eq!(probe.metadata().num_row_groups(), 3, "one row group per row (limit=1)");

        let dir = tempfile::tempdir().unwrap();
        let store = DataFusionHist::open(dir.path()).unwrap();
        let expected_symbol = ["TOK_A", "TOK_B", "TOK_C"];

        let mut seen = Vec::new();
        let total = ingest_stream_over(bytes, &store, "2026-07-27", Stream::Book, None, |p| {
            // At the moment THIS callback runs, the row group just processed must already be on
            // disk — scanning here (not after the whole run) is what makes this an incremental,
            // not an end-of-run, assertion.
            let sym = expected_symbol[p.index];
            let scanned = store.scan_book_updates(VENUE, sym, TsRange::all()).unwrap();
            assert_eq!(
                scanned.len(),
                1,
                "row group {} ({sym})'s row must already be durable when its own callback fires",
                p.index
            );
            seen.push((p.index, p.total, p.rows_this_group, p.rows_cumulative));
        })
        .unwrap();

        assert_eq!(total, 3);
        assert_eq!(
            seen,
            vec![(0, 3, 1, 1), (1, 3, 1, 2), (2, 3, 1, 3)],
            "one callback per row group, in order, with a running cumulative total"
        );
    }

    /// The correctness proof for the per-row-group commit-key redesign: TWO rows for the SAME
    /// token, forced into TWO separate row groups. A single date-wide commit key (the pre-fix
    /// scheme) would make the SECOND row's write a silent no-op — `HistStore`'s `commit_rows`
    /// treats a repeated commit key as "already committed" and returns `Ok(0)` without writing
    /// anything (see `vike-data`'s `datafusion_hist.rs`) — so a naive per-row-group split with the
    /// OLD shared key would have silently dropped every row after a symbol's first row group. This
    /// asserts both rows survive.
    #[test]
    fn ingest_stream_over_keeps_both_rows_when_one_symbol_spans_two_row_groups() {
        let rows: Vec<BookRow<'_>> = vec![
            ("TOK_A", 1, 1, 1, "book", "none", 0.0, 0.0, "[[0.5,10.0]]", "[[0.51,10.0]]", 0.01, ""),
            ("TOK_A", 2, 2, 2, "book", "none", 0.0, 0.0, "[[0.6,11.0]]", "[[0.61,11.0]]", 0.01, ""),
        ];
        let bytes = write_book_events_parquet(&rows, 1);
        let probe = ParquetRecordBatchReaderBuilder::try_new(bytes.clone()).unwrap();
        assert_eq!(
            probe.metadata().num_row_groups(),
            2,
            "max_rows_per_group=1 over 2 rows -> 2 groups"
        );

        let dir = tempfile::tempdir().unwrap();
        let store = DataFusionHist::open(dir.path()).unwrap();
        let mut calls = 0usize;
        let total = ingest_stream_over(bytes, &store, "2026-07-27", Stream::Book, None, |_p| {
            calls += 1;
        })
        .unwrap();

        assert_eq!(calls, 2, "one callback per row group");
        assert_eq!(total, 2, "both rows written — neither silently dropped by a shared commit key");
        let scanned = store.scan_book_updates(VENUE, "TOK_A", TsRange::all()).unwrap();
        assert_eq!(
            scanned.len(),
            2,
            "both row groups' rows for TOK_A must be present, not just the first"
        );
    }

    /// A token filter still prunes row groups (and drops unrelated rows within a kept group) when
    /// ingesting through the new streaming path — the filtering behavior itself is unchanged,
    /// only WHEN writes happen changed.
    #[test]
    fn ingest_stream_over_respects_a_token_filter() {
        let rows: Vec<BookRow<'_>> = vec![
            ("TOK_A", 1, 1, 1, "book", "none", 0.0, 0.0, "[[0.5,10.0]]", "[[0.51,10.0]]", 0.01, ""),
            ("TOK_B", 2, 2, 1, "book", "none", 0.0, 0.0, "[[0.4,20.0]]", "[[0.41,20.0]]", 0.01, ""),
        ];
        let bytes = write_book_events_parquet(&rows, 1);
        let dir = tempfile::tempdir().unwrap();
        let store = DataFusionHist::open(dir.path()).unwrap();
        let mut want = HashSet::new();
        want.insert("TOK_B".to_string());

        let total =
            ingest_stream_over(bytes, &store, "2026-07-27", Stream::Book, Some(&want), |_| {})
                .unwrap();
        assert_eq!(total, 1, "only TOK_B's row is written");
        assert!(store.scan_book_updates(VENUE, "TOK_A", TsRange::all()).unwrap().is_empty());
        assert_eq!(store.scan_book_updates(VENUE, "TOK_B", TsRange::all()).unwrap().len(), 1);
    }

    // ---- local-file ChunkReader: proves the reader is the ONLY difference from the URL path -----

    /// Opening a path that doesn't exist reports a `CollectError`, not a panic — same "one bad
    /// file must not crash the whole run" posture the rest of this module holds elsewhere.
    #[test]
    fn local_file_reader_open_reports_a_missing_path() {
        let path = std::env::temp_dir().join("vike_archive_no_such_local_file.parquet");
        assert!(!path.exists());
        assert!(LocalFileReader::open(&path).is_err());
    }

    /// The headline correctness proof for the `--file` feature: the SAME Parquet bytes ingested
    /// once through [`LocalFileReader`] (the new local path) and once through a plain `Bytes`
    /// reader (standing in for [`HttpRangeReader`] — both are nothing more than `ChunkReader`
    /// impls feeding the identical [`ingest_stream_over`] pipeline, and `Bytes` is exactly the
    /// reader type every other streaming test in this module already exercises that pipeline
    /// with) must decode into byte-identical mapped rows, proving the reader swap is the ONLY
    /// difference between the two ingest paths — nothing about decode/mapping/commit-keying
    /// changed.
    #[test]
    fn local_file_reader_and_a_bytes_reader_ingest_identical_rows() {
        let rows: Vec<BookRow<'_>> = vec![
            ("TOK_A", 1, 1, 1, "book", "none", 0.0, 0.0, "[[0.5,10.0]]", "[[0.51,10.0]]", 0.01, ""),
            ("TOK_A", 2, 2, 2, "price_change", "buy", 0.42, 7.0, "", "", 0.01, ""),
            ("TOK_B", 3, 3, 1, "book", "none", 0.0, 0.0, "[[0.4,20.0]]", "[[0.41,20.0]]", 0.01, ""),
        ];
        let bytes = write_book_events_parquet(&rows, 1);

        // "URL-equivalent" path: ingest straight from the in-memory `Bytes` reader.
        let url_dir = tempfile::tempdir().unwrap();
        let url_store = DataFusionHist::open(url_dir.path()).unwrap();
        let url_total =
            ingest_stream_over(bytes.clone(), &url_store, "2026-07-27", Stream::Book, None, |_| {})
                .unwrap();

        // Local-file path: the same bytes, written to a real file on disk, opened through
        // `LocalFileReader` — the only thing that differs from the block above.
        let file_dir = tempfile::tempdir().unwrap();
        let file_path = file_dir.path().join("book_events.parquet");
        std::fs::write(&file_path, &bytes).unwrap();
        let local_store_dir = tempfile::tempdir().unwrap();
        let local_store = DataFusionHist::open(local_store_dir.path()).unwrap();
        let local_reader = LocalFileReader::open(&file_path).unwrap();
        assert_eq!(
            local_reader.len(),
            bytes.len() as u64,
            "LocalFileReader sees the real file size"
        );
        let local_total = ingest_stream_over(
            local_reader,
            &local_store,
            "2026-07-27",
            Stream::Book,
            None,
            |_| {},
        )
        .unwrap();

        assert_eq!(url_total, local_total, "both readers decode the same row count");
        assert_eq!(url_total, 3);

        for sym in ["TOK_A", "TOK_B"] {
            let from_url = url_store.scan_book_updates(VENUE, sym, TsRange::all()).unwrap();
            let from_local = local_store.scan_book_updates(VENUE, sym, TsRange::all()).unwrap();
            assert_eq!(
                from_url.len(),
                from_local.len(),
                "same row count for {sym} regardless of reader"
            );
            for (a, b) in from_url.iter().zip(from_local.iter()) {
                assert_eq!(a.ts, b.ts, "{sym}: ts must match");
                assert_eq!(a.local_ts, b.local_ts, "{sym}: local_ts must match");
                assert_eq!(a.seq, b.seq, "{sym}: seq must match");
                assert_eq!(a.kind, b.kind, "{sym}: kind must match");
                assert_eq!(a.tick_size, b.tick_size, "{sym}: tick_size must match");
                assert_eq!(a.bids, b.bids, "{sym}: bids must match");
                assert_eq!(a.asks, b.asks, "{sym}: asks must match");
                assert_eq!(a.symbol, b.symbol, "{sym}: symbol must match");
            }
        }
    }

    /// The bulk (`--file --bulk`) local-ingest twin of the test above: staging through
    /// [`ingest_local_file_bulk`]/[`BulkIngestSession`] must land the SAME rows as the plain
    /// (non-bulk) local-file path — the bulk profile only changes WHEN a commit happens, never
    /// WHAT gets committed.
    #[test]
    fn ingest_local_file_bulk_matches_the_plain_local_file_path() {
        let rows: Vec<BookRow<'_>> = vec![
            ("TOK_A", 1, 1, 1, "book", "none", 0.0, 0.0, "[[0.5,10.0]]", "[[0.51,10.0]]", 0.01, ""),
            ("TOK_A", 2, 2, 2, "price_change", "buy", 0.42, 7.0, "", "", 0.01, ""),
        ];
        let bytes = write_book_events_parquet(&rows, 1);
        let file_dir = tempfile::tempdir().unwrap();
        let file_path = file_dir.path().join("book_events.parquet");
        std::fs::write(&file_path, &bytes).unwrap();

        let plain_dir = tempfile::tempdir().unwrap();
        let plain_store = DataFusionHist::open(plain_dir.path()).unwrap();
        let plain_total =
            ingest_local_file(&plain_store, &file_path, "2026-07-27", Stream::Book, None).unwrap();

        let bulk_dir = tempfile::tempdir().unwrap();
        let bulk_store = DataFusionHist::open(bulk_dir.path()).unwrap();
        let mut session = bulk_store.bulk_session(BulkConfig::default());
        let key_prefix = "vikearchive:book:2026-07-27:bulk";
        let bulk_staged = ingest_local_file_bulk(
            &mut session,
            &file_path,
            "2026-07-27",
            Stream::Book,
            None,
            key_prefix,
        )
        .unwrap();
        session.flush(key_prefix).unwrap();

        assert_eq!(plain_total, 2);
        assert_eq!(bulk_staged, 2, "bulk path decodes the same row count");
        let plain_rows = plain_store.scan_book_updates(VENUE, "TOK_A", TsRange::all()).unwrap();
        let bulk_rows = bulk_store.scan_book_updates(VENUE, "TOK_A", TsRange::all()).unwrap();
        assert_eq!(plain_rows.len(), bulk_rows.len(), "same committed row count either way");
        assert_eq!(bulk_rows.len(), 2);
    }

    /// A `BookUpdate` reduced to the fields that carry its content — `(ts, seq, bids, asks)`.
    /// `BookUpdate` itself has no `PartialEq`, so equality assertions compare this instead.
    type BookLens = (i64, u64, Vec<(f64, f64)>, Vec<(f64, f64)>);

    /// Bulk + fan-out must commit exactly what serial bulk commits — across MANY tokens and MANY
    /// row groups, so the work genuinely spreads over several chunks.
    ///
    /// This is the test the whole per-worker key namespace exists for. With a shared key prefix
    /// every worker's session would number its windows `w0`, `w1`, … from its own zero, minting the
    /// SAME key for DIFFERENT rows; `commit_rows_bulk` treats a seen key as durably committed and
    /// returns `Ok(0)`, so the second worker to reach a series would lose its rows silently. That
    /// failure shows up here as a short row count, with no error anywhere.
    #[test]
    fn bulk_parallel_commits_exactly_what_serial_bulk_commits() {
        let ids = [
            "TOK_A", "TOK_B", "TOK_C", "TOK_D", "TOK_E", "TOK_F", "TOK_G", "TOK_H", "TOK_I",
            "TOK_J", "TOK_K", "TOK_L",
        ];
        // Two rows per token; one row group per row, so 24 groups fan across the workers and most
        // tokens straddle a group boundary (the contiguous-chunk property under real pressure).
        let rows: Vec<BookRow<'_>> = ids
            .iter()
            .enumerate()
            .flat_map(|(i, id)| {
                let ts = (i as i64 + 1) * 10;
                vec![
                    (
                        *id,
                        ts,
                        ts,
                        1u64,
                        "book",
                        "none",
                        0.0,
                        0.0,
                        "[[0.5,10.0]]",
                        "[[0.51,10.0]]",
                        0.01,
                        "",
                    ),
                    (*id, ts + 1, ts + 1, 2u64, "price_change", "buy", 0.42, 7.0, "", "", 0.01, ""),
                ]
            })
            .collect();
        let bytes = write_book_events_parquet(&rows, 1);
        let file_dir = tempfile::tempdir().unwrap();
        let file_path = file_dir.path().join("book_events.parquet");
        std::fs::write(&file_path, &bytes).unwrap();
        let key_prefix = "vikearchive:book:2026-07-27:bulk";

        let serial_dir = tempfile::tempdir().unwrap();
        let serial_store = DataFusionHist::open(serial_dir.path()).unwrap();
        let mut session = serial_store.bulk_session(BulkConfig::default());
        let serial_staged = ingest_local_file_bulk(
            &mut session,
            &file_path,
            "2026-07-27",
            Stream::Book,
            None,
            key_prefix,
        )
        .unwrap();
        session.flush(key_prefix).unwrap();

        let par_dir = tempfile::tempdir().unwrap();
        let par_store = DataFusionHist::open(par_dir.path()).unwrap();
        let par_staged = ingest_local_file_bulk_parallel(
            &par_store,
            &file_path,
            "2026-07-27",
            Stream::Book,
            None,
            key_prefix,
            4,
            BulkConfig::default(),
        )
        .unwrap();

        assert_eq!(serial_staged, par_staged, "same rows decoded either way");
        // `BookUpdate` has no `PartialEq`, so compare on the fields that carry the content:
        // (ts, seq, levels). Both sides come back in the store's documented (ts, seq) order.
        let lens = |v: &[BookUpdate]| -> Vec<BookLens> {
            v.iter().map(|u| (u.ts, u.seq, u.bids.clone(), u.asks.clone())).collect()
        };
        for id in ids {
            let s = serial_store.scan_book_updates(VENUE, id, TsRange::all()).unwrap();
            let p = par_store.scan_book_updates(VENUE, id, TsRange::all()).unwrap();
            assert_eq!(
                lens(&s),
                lens(&p),
                "{id}: parallel bulk must commit exactly the serial bulk rows"
            );
            assert!(!p.is_empty(), "{id}: committed nothing — a dropped key would look like this");
        }
    }

    /// `--jobs 1` through the parallel bulk path is the serial bulk path: one chunk, one session,
    /// file order. Guards the degenerate case so the bin can route on `jobs > 1` alone.
    #[test]
    fn bulk_parallel_with_one_job_equals_serial_bulk() {
        let rows: Vec<BookRow<'_>> = vec![
            ("TOK_A", 1, 1, 1, "book", "none", 0.0, 0.0, "[[0.5,10.0]]", "[[0.51,10.0]]", 0.01, ""),
            ("TOK_A", 2, 2, 2, "price_change", "buy", 0.42, 7.0, "", "", 0.01, ""),
        ];
        let bytes = write_book_events_parquet(&rows, 1);
        let file_dir = tempfile::tempdir().unwrap();
        let file_path = file_dir.path().join("book_events.parquet");
        std::fs::write(&file_path, &bytes).unwrap();

        let dir = tempfile::tempdir().unwrap();
        let store = DataFusionHist::open(dir.path()).unwrap();
        let staged = ingest_local_file_bulk_parallel(
            &store,
            &file_path,
            "2026-07-27",
            Stream::Book,
            None,
            "vikearchive:book:2026-07-27:bulk",
            1,
            BulkConfig::default(),
        )
        .unwrap();
        assert_eq!(staged, 2);
        assert_eq!(store.scan_book_updates(VENUE, "TOK_A", TsRange::all()).unwrap().len(), 2);
    }

    // ---- chunk_row_groups: the fan-out unit's pure partitioning ----------------------------------

    #[test]
    fn chunk_row_groups_splits_as_evenly_as_possible_and_stays_contiguous_in_order() {
        let selected: Vec<usize> = (10..20).collect(); // 10 items, deliberately not 0-based
        let chunks = chunk_row_groups(&selected, 3);
        let lens: Vec<usize> = chunks.iter().map(Vec::len).collect();
        assert_eq!(lens, vec![4, 3, 3], "remainder goes to the first chunks");
        // Every original index appears exactly once, in order, across the concatenated chunks.
        let flat: Vec<usize> = chunks.into_iter().flatten().collect();
        assert_eq!(flat, selected, "contiguous split preserves file order end to end");
    }

    #[test]
    fn chunk_row_groups_clamps_jobs_above_selected_len() {
        let selected = vec![5, 6, 7];
        let chunks = chunk_row_groups(&selected, 100);
        assert_eq!(chunks.len(), 3, "never more chunks than row groups");
        assert_eq!(chunks, vec![vec![5], vec![6], vec![7]]);
    }

    #[test]
    fn chunk_row_groups_treats_zero_jobs_as_one() {
        let selected = vec![1, 2, 3];
        assert_eq!(chunk_row_groups(&selected, 0), vec![selected.clone()]);
        assert_eq!(chunk_row_groups(&selected, 1), vec![selected]);
    }

    #[test]
    fn chunk_row_groups_empty_selection_is_no_chunks() {
        assert!(chunk_row_groups(&[], 8).is_empty());
    }

    // ---- ingest_local_file_parallel: must equal the serial path, INCLUDING under real cross-worker
    // contention on a token whose two rows straddle a chunk boundary -----------------------------

    /// A dozen distinct single-row tokens (one row group each, `max_rows_per_group=1`) ingested
    /// once through the serial [`ingest_local_file`] and once through
    /// [`ingest_local_file_parallel`] with `jobs=4` must land the identical row count AND the
    /// identical per-symbol rows — the headline "parallel == serial" correctness proof the
    /// measurement plan requires.
    #[test]
    fn ingest_local_file_parallel_matches_the_serial_path_row_counts_and_content() {
        let ids = [
            "TOK_A", "TOK_B", "TOK_C", "TOK_D", "TOK_E", "TOK_F", "TOK_G", "TOK_H", "TOK_I",
            "TOK_J", "TOK_K", "TOK_L",
        ];
        let rows: Vec<BookRow<'_>> = ids
            .iter()
            .enumerate()
            .map(|(i, sym)| {
                let ts = (i + 1) as i64;
                (
                    *sym,
                    ts,
                    ts,
                    1u64,
                    "book",
                    "none",
                    0.0,
                    0.0,
                    "[[0.5,10.0]]",
                    "[[0.51,10.0]]",
                    0.01,
                    "",
                )
            })
            .collect();
        let bytes = write_book_events_parquet(&rows, 1);
        let probe = ParquetRecordBatchReaderBuilder::try_new(bytes.clone()).unwrap();
        assert_eq!(probe.metadata().num_row_groups(), ids.len(), "one row group per token");

        let file_dir = tempfile::tempdir().unwrap();
        let file_path = file_dir.path().join("book_events.parquet");
        std::fs::write(&file_path, &bytes).unwrap();

        let serial_dir = tempfile::tempdir().unwrap();
        let serial_store = DataFusionHist::open(serial_dir.path()).unwrap();
        let serial_total =
            ingest_local_file(&serial_store, &file_path, "2026-07-28", Stream::Book, None).unwrap();

        let parallel_dir = tempfile::tempdir().unwrap();
        let parallel_store = DataFusionHist::open(parallel_dir.path()).unwrap();
        let parallel_total = ingest_local_file_parallel(
            &parallel_store,
            &file_path,
            "2026-07-28",
            Stream::Book,
            None,
            4,
        )
        .unwrap();

        assert_eq!(serial_total, ids.len());
        assert_eq!(parallel_total, serial_total, "parallel path writes the same row count");

        for sym in ids {
            let from_serial = serial_store.scan_book_updates(VENUE, sym, TsRange::all()).unwrap();
            let from_parallel =
                parallel_store.scan_book_updates(VENUE, sym, TsRange::all()).unwrap();
            assert_eq!(from_serial.len(), from_parallel.len(), "{sym}: same row count");
            assert_eq!(from_serial.len(), 1);
            assert_eq!(from_serial[0].ts, from_parallel[0].ts, "{sym}: ts must match");
        }
    }

    /// The parallel counterpart of
    /// `ingest_stream_over_keeps_both_rows_when_one_symbol_spans_two_row_groups`: FOUR row groups
    /// (one row each), `jobs=2` so [`chunk_row_groups`] hands worker 0 row groups `[0, 1]` and
    /// worker 1 row groups `[2, 3]`. `TOK_A`'s two rows sit in row groups 1 and 2 — exactly
    /// STRADDLING the chunk boundary — so this is the one case where two DIFFERENT worker threads
    /// really do call `DataFusionHist::append_book_updates` for the SAME symbol concurrently,
    /// exercising `SeriesLock`'s file-lock contention path for real (not just asserting the
    /// design on paper). Both of `TOK_A`'s rows must survive — the store's per-series lock must
    /// serialize the two concurrent manifest read-modify-writes correctly, not lose one.
    #[test]
    fn ingest_local_file_parallel_keeps_both_rows_when_one_symbol_spans_a_chunk_boundary() {
        let rows: Vec<BookRow<'_>> = vec![
            ("TOK_X", 1, 1, 1, "book", "none", 0.0, 0.0, "[[0.1,1.0]]", "[[0.11,1.0]]", 0.01, ""),
            ("TOK_A", 2, 2, 1, "book", "none", 0.0, 0.0, "[[0.5,10.0]]", "[[0.51,10.0]]", 0.01, ""),
            ("TOK_A", 3, 3, 2, "book", "none", 0.0, 0.0, "[[0.6,11.0]]", "[[0.61,11.0]]", 0.01, ""),
            ("TOK_Y", 4, 4, 1, "book", "none", 0.0, 0.0, "[[0.2,2.0]]", "[[0.21,2.0]]", 0.01, ""),
        ];
        let bytes = write_book_events_parquet(&rows, 1);
        let probe = ParquetRecordBatchReaderBuilder::try_new(bytes.clone()).unwrap();
        assert_eq!(probe.metadata().num_row_groups(), 4, "one row group per row");
        // Sanity-check the boundary this test relies on: jobs=2 over 4 groups -> [0,1] / [2,3].
        assert_eq!(chunk_row_groups(&[0, 1, 2, 3], 2), vec![vec![0, 1], vec![2, 3]]);

        let file_dir = tempfile::tempdir().unwrap();
        let file_path = file_dir.path().join("book_events.parquet");
        std::fs::write(&file_path, &bytes).unwrap();

        let dir = tempfile::tempdir().unwrap();
        let store = DataFusionHist::open(dir.path()).unwrap();
        let total =
            ingest_local_file_parallel(&store, &file_path, "2026-07-28", Stream::Book, None, 2)
                .unwrap();

        assert_eq!(total, 4, "all four rows written — none dropped by the cross-worker boundary");
        let tok_a = store.scan_book_updates(VENUE, "TOK_A", TsRange::all()).unwrap();
        assert_eq!(tok_a.len(), 2, "both of TOK_A's rows survive concurrent cross-worker writes");
        let tok_x = store.scan_book_updates(VENUE, "TOK_X", TsRange::all()).unwrap();
        let tok_y = store.scan_book_updates(VENUE, "TOK_Y", TsRange::all()).unwrap();
        assert_eq!(tok_x.len(), 1);
        assert_eq!(tok_y.len(), 1);
    }

    /// `jobs=1` must be byte-for-byte the same decode/commit order as the serial
    /// [`ingest_local_file`] — a caller that wants the old behavior back gets it exactly, not just
    /// "close enough". Uses a token spanning two row groups (like the serial-path test of the same
    /// name) so a hypothetical broken single-chunk split would still be caught.
    #[test]
    fn ingest_local_file_parallel_with_jobs_one_matches_ingest_local_file_exactly() {
        let rows: Vec<BookRow<'_>> = vec![
            ("TOK_A", 1, 1, 1, "book", "none", 0.0, 0.0, "[[0.5,10.0]]", "[[0.51,10.0]]", 0.01, ""),
            ("TOK_A", 2, 2, 2, "book", "none", 0.0, 0.0, "[[0.6,11.0]]", "[[0.61,11.0]]", 0.01, ""),
        ];
        let bytes = write_book_events_parquet(&rows, 1);
        let file_dir = tempfile::tempdir().unwrap();
        let file_path = file_dir.path().join("book_events.parquet");
        std::fs::write(&file_path, &bytes).unwrap();

        let serial_dir = tempfile::tempdir().unwrap();
        let serial_store = DataFusionHist::open(serial_dir.path()).unwrap();
        let serial_total =
            ingest_local_file(&serial_store, &file_path, "2026-07-27", Stream::Book, None).unwrap();

        let parallel_dir = tempfile::tempdir().unwrap();
        let parallel_store = DataFusionHist::open(parallel_dir.path()).unwrap();
        let parallel_total = ingest_local_file_parallel(
            &parallel_store,
            &file_path,
            "2026-07-27",
            Stream::Book,
            None,
            1,
        )
        .unwrap();

        assert_eq!(serial_total, 2);
        assert_eq!(parallel_total, 2);
        let from_serial = serial_store.scan_book_updates(VENUE, "TOK_A", TsRange::all()).unwrap();
        let from_parallel =
            parallel_store.scan_book_updates(VENUE, "TOK_A", TsRange::all()).unwrap();
        assert_eq!(from_serial.len(), 2);
        assert_eq!(from_parallel.len(), 2);
        for (a, b) in from_serial.iter().zip(from_parallel.iter()) {
            assert_eq!(a.ts, b.ts);
            assert_eq!(a.seq, b.seq);
        }
    }

    /// A token filter still prunes row groups (and drops unrelated rows within a kept group) under
    /// the parallel path — mirrors `ingest_stream_over_respects_a_token_filter` for the serial core.
    #[test]
    fn ingest_local_file_parallel_respects_a_token_filter() {
        let rows: Vec<BookRow<'_>> = vec![
            ("TOK_A", 1, 1, 1, "book", "none", 0.0, 0.0, "[[0.5,10.0]]", "[[0.51,10.0]]", 0.01, ""),
            ("TOK_B", 2, 2, 1, "book", "none", 0.0, 0.0, "[[0.4,20.0]]", "[[0.41,20.0]]", 0.01, ""),
        ];
        let bytes = write_book_events_parquet(&rows, 1);
        let file_dir = tempfile::tempdir().unwrap();
        let file_path = file_dir.path().join("book_events.parquet");
        std::fs::write(&file_path, &bytes).unwrap();

        let dir = tempfile::tempdir().unwrap();
        let store = DataFusionHist::open(dir.path()).unwrap();
        let mut want = HashSet::new();
        want.insert("TOK_B".to_string());

        let total = ingest_local_file_parallel(
            &store,
            &file_path,
            "2026-07-27",
            Stream::Book,
            Some(&want),
            2,
        )
        .unwrap();
        assert_eq!(total, 1, "only TOK_B's row is written");
        assert!(store.scan_book_updates(VENUE, "TOK_A", TsRange::all()).unwrap().is_empty());
        assert_eq!(store.scan_book_updates(VENUE, "TOK_B", TsRange::all()).unwrap().len(), 1);
    }

    // ---- HttpRangeReader: the get_bytes buffer is bounded whatever the server claims ------------
    //
    // These drive a LOOPBACK origin that lies, because the whole point of the cap is that the two
    // numbers parquet cross-checks — the footer's `metadata_len` and `ChunkReader::len()` — both
    // come from the server. Every assertion is on bytes the client actually BUFFERED (the returned
    // `Bytes`) or on whether a request was dialled at all, never merely on "an error came back":
    // the pre-cap code also returned an error in two of these three cases, just after allocating.

    /// A minimal HTTP/1.1 origin on loopback. `HEAD` answers `claimed_len` as `Content-Length` —
    /// the number a hostile or misconfigured host controls, and the ONLY thing parquet's own footer
    /// validation checks a `get_bytes` length against. `GET` parses `Range: bytes=A-B` and replies
    /// `206` with whatever `body_for(a, b)` returns, which the caller is free to make SHORTER or
    /// LONGER than the range asked for. `gets` counts dialled range requests, so a test can assert
    /// that a refusal happened before any socket was opened.
    struct LyingOrigin {
        url: String,
        gets: Arc<AtomicUsize>,
    }

    fn spawn_lying_origin(
        claimed_len: u64,
        body_for: impl Fn(u64, u64) -> Vec<u8> + Send + Sync + 'static,
    ) -> LyingOrigin {
        use std::io::Write as _;
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let port = listener.local_addr().unwrap().port();
        let gets = Arc::new(AtomicUsize::new(0));
        let gets_srv = Arc::clone(&gets);

        // Daemon thread: the test process exits without joining it. Each connection is answered
        // once and closed (`Connection: close`), so there is no keep-alive state to get wrong.
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut sock) = stream else { break };
                // Read just the request head — everything up to the blank line.
                let mut head = Vec::new();
                let mut byte = [0u8; 1];
                while !head.ends_with(b"\r\n\r\n") {
                    match sock.read(&mut byte) {
                        Ok(1) => head.push(byte[0]),
                        _ => break,
                    }
                }
                let head = String::from_utf8_lossy(&head).to_string();
                if head.starts_with("HEAD ") {
                    let _ = write!(
                        sock,
                        "HTTP/1.1 200 OK\r\nContent-Length: {claimed_len}\r\n\
                         Accept-Ranges: bytes\r\nConnection: close\r\n\r\n"
                    );
                    continue;
                }
                gets_srv.fetch_add(1, Ordering::SeqCst);
                // `Range: bytes=A-B` — the reader always sends an explicit closed range.
                let (a, b) = head
                    .split("bytes=")
                    .nth(1)
                    .and_then(|r| r.split_whitespace().next())
                    .and_then(|r| r.split_once('-'))
                    .and_then(|(a, b)| Some((a.trim().parse().ok()?, b.trim().parse().ok()?)))
                    .unwrap_or((0u64, 0u64));
                let body = body_for(a, b);
                let _ = write!(
                    sock,
                    "HTTP/1.1 206 Partial Content\r\nContent-Range: bytes {a}-{b}/{claimed_len}\r\n\
                     Content-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                // The over-send case abandons this write once the client stops reading; a broken
                // pipe here is the EXPECTED outcome, not a test failure.
                let _ = sock.write_all(&body);
            }
        });

        LyingOrigin { url: format!("http://127.0.0.1:{port}/book_events.parquet"), gets }
    }

    fn test_agent() -> ureq::Agent {
        ureq::Agent::config_builder().http_status_as_error(false).build().new_agent()
    }

    /// A server that answers a small `Range` with a MUCH larger body cannot grow the buffer past
    /// what was asked for. This is the case `Read::take` exists for, and the one that most cleanly
    /// separates the two implementations: with the bare `read_to_end` this test buffered all 8 MiB
    /// and then failed the `buf.len() != length` check, so it would have gone RED on the exact
    /// symptom (an 8 MiB allocation for a 4 KiB request) rather than on a cosmetic difference.
    #[test]
    fn get_bytes_stops_at_the_requested_length_when_the_server_over_sends() {
        const ASKED: usize = 4096;
        const OFFERED: usize = 8 * 1024 * 1024;
        let origin = spawn_lying_origin(64 * 1024 * 1024, |_a, _b| vec![0xABu8; OFFERED]);
        let reader = HttpRangeReader::open(test_agent(), origin.url.clone(), None)
            .expect("HEAD the lying origin");

        let got = reader.get_bytes(0, ASKED).expect("a bounded read still succeeds");

        // The direct measurement of bytes buffered: exactly what we asked for, out of 8 MiB
        // offered. 2048x less than the server was willing to hand over.
        assert_eq!(got.len(), ASKED, "buffer must be bounded by the requested length");
        assert!(got.iter().all(|b| *b == 0xAB), "and it is the server's actual bytes");
    }

    /// The opposite failure — a server that claims more than it sends — must still be a clean,
    /// bounded `Err`, and must not be papered over by the `take` above into a silently short slice.
    #[test]
    fn get_bytes_errors_bounded_when_the_server_under_sends() {
        const ASKED: usize = 4096;
        const SENT: usize = 100;
        let origin = spawn_lying_origin(64 * 1024 * 1024, |_a, _b| vec![0x11u8; SENT]);
        let reader = HttpRangeReader::open(test_agent(), origin.url.clone(), None)
            .expect("HEAD the lying origin");

        let err = reader.get_bytes(0, ASKED).expect_err("a short body is an error");

        assert!(matches!(err, ParquetError::EOF(_)), "short body is EOF, not General: {err}");
        // The error names how much actually arrived — i.e. the buffer stopped at the 100 bytes the
        // server sent, not at the 4096 it claimed.
        let msg = err.to_string();
        assert!(msg.contains("expected 4096 bytes, got 100"), "unexpected message: {msg}");
    }

    /// An absurd `Content-Length` is refused BEFORE anything is allocated or dialled. `gets == 0`
    /// is the assertion that matters: it proves the cap short-circuits ahead of
    /// `Vec::with_capacity`, which on a length this size does not return an `Err` at all — it calls
    /// `handle_alloc_error` and aborts the process.
    #[test]
    fn get_bytes_refuses_an_absurd_length_before_allocating_or_dialling() {
        let absurd = MAX_CHUNK_BYTES + 1;
        let origin = spawn_lying_origin(1 << 40, |_a, _b| Vec::new());
        let reader = HttpRangeReader::open(test_agent(), origin.url.clone(), None)
            .expect("HEAD the lying origin");
        assert_eq!(reader.len(), 1 << 40, "the reader believes the server's Content-Length");

        let err = reader.get_bytes(0, absurd as usize).expect_err("must refuse");

        assert_eq!(origin.gets.load(Ordering::SeqCst), 0, "no request may be dialled");
        let msg = err.to_string();
        assert!(msg.contains("refusing"), "unexpected message: {msg}");
        assert!(msg.contains(&MAX_CHUNK_BYTES.to_string()), "message must name the cap: {msg}");
    }

    /// The end-to-end shape this cap exists for: a hostile FILE, not a hostile caller. The last 8
    /// bytes of a Parquet file are `[metadata_len: u32 LE][b"PAR1"]`, and
    /// `ParquetMetaDataReader::parse_metadata` accepts any `metadata_len` that fits inside
    /// `ChunkReader::len()` — which here is the server's own `Content-Length`. A host controlling
    /// both therefore walks parquet straight into `get_bytes(_, 4_294_967_295)`. Without the cap
    /// that is a 4 GiB `Vec::with_capacity`; with it, a clean `Err` and no second request.
    #[test]
    fn a_hostile_footer_cannot_drive_a_multi_gigabyte_read() {
        let claimed_len: u64 = 5_000_000_000;
        let mut footer = Vec::new();
        footer.extend_from_slice(&u32::MAX.to_le_bytes()); // metadata_len = 4_294_967_295
        footer.extend_from_slice(b"PAR1");
        let origin = spawn_lying_origin(claimed_len, move |a, b| {
            // Only the footer tail is ever served; any other range gets nothing, which is enough
            // because the cap must fire on the very next call.
            if a == claimed_len - 8 && b == claimed_len - 1 { footer.clone() } else { Vec::new() }
        });
        let reader = HttpRangeReader::open(test_agent(), origin.url.clone(), None)
            .expect("HEAD the lying origin");

        let err = ParquetRecordBatchReaderBuilder::try_new(reader)
            .err()
            .expect("a 4 GiB footer must not be honoured");

        let msg = err.to_string();
        assert!(msg.contains("refusing"), "the cap must be what stopped it, got: {msg}");
        assert!(msg.contains("4294967295"), "and it must name the refused size: {msg}");
        // Exactly one range GET: the 8-byte footer tail. The 4 GiB follow-up never left the process.
        assert_eq!(origin.gets.load(Ordering::SeqCst), 1, "the huge read was never dialled");
    }

    // ---- live network smokes (never run in CI; manual only) --------------------------------------

    /// Proves ranged reads actually work end-to-end against the REAL archive: opens the free,
    /// keyless daily sample (`/archive/samples/book_events.parquet`, no `X-API-Key` needed) with
    /// [`HttpRangeReader`], reads the footer-only metadata, and decodes ONE row group. Always
    /// `#[ignore]`d — needs network, which unit tests must not require — run manually:
    /// `cargo test -p vike-backfill --features vike-archive --lib vike_archive::tests::live_range_read_against_free_sample -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn live_range_read_against_free_sample() {
        let agent = ureq::Agent::config_builder().http_status_as_error(false).build().new_agent();
        let url = "https://data.vike.io/archive/samples/book_events.parquet".to_string();
        let reader = HttpRangeReader::open(agent, url, None).expect("HEAD the free sample");
        assert!(reader.len() > 0);
        let builder = ParquetRecordBatchReaderBuilder::try_new(reader).expect("footer-only open");
        assert!(builder.metadata().num_row_groups() >= 1);
        let mut reader = builder.build().unwrap();
        let batch = reader.next().expect("at least one batch").unwrap();
        let decoded = book_updates_from_batch(&batch).expect("decode the real schema");
        println!("live sample: decoded {} book updates from one batch", decoded.len());
    }

    /// Proves the manifest + a keyed per-date footer-only plan against the REAL archive.
    /// Double-gated (network + `VIKE_ARCHIVE_API_KEY`): self-skips when the key is absent — the
    /// same idiom the venue demo smokes use. Run manually:
    /// `cargo test -p vike-backfill --features vike-archive --lib vike_archive::tests::live_manifest_and_plan -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn live_manifest_and_plan() {
        let Some(key) = std::env::var("VIKE_ARCHIVE_API_KEY").ok() else {
            eprintln!("VIKE_ARCHIVE_API_KEY not set — skipping live archive smoke");
            return;
        };
        let client = ArchiveClient::new(DEFAULT_BASE, Some(key));
        let manifest = client.fetch_manifest().expect("fetch the real manifest");
        assert!(!manifest.dates.is_empty());
        let date = &manifest.dates[0].date;
        let mut want = HashSet::new();
        // An arbitrary sample token id seen in the free sample fixture — a real archive run would
        // narrow to a token the operator actually cares about.
        want.insert(
            "7715333804644496306161804929604508138595580668391178416151948416889614694288"
                .to_string(),
        );
        let plan = client.plan_stream(date, Stream::Book, Some(&want)).expect("plan a real date");
        println!(
            "live plan {date}/book_events: {}/{} row groups, {} / {} compressed bytes",
            plan.selected_row_groups,
            plan.total_row_groups,
            plan.selected_compressed_bytes,
            plan.total_compressed_bytes
        );
    }

    /// Proves the real `/v1/archive/datasets` + `/v1/archive/url` discovery routes end-to-end
    /// against the live production host, and that the URL they hand back is directly openable by
    /// [`HttpRangeReader`] (never re-derived by this client). Double-gated (network +
    /// `VIKE_ARCHIVE_API_KEY`): self-skips when the key is absent. Run manually:
    /// `cargo test -p vike-backfill --features vike-archive --lib vike_archive::tests::live_discover_and_resolve_btc_5m -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn live_discover_and_resolve_btc_5m() {
        let Some(key) = std::env::var("VIKE_ARCHIVE_API_KEY").ok() else {
            eprintln!("VIKE_ARCHIVE_API_KEY not set — skipping live discovery smoke");
            return;
        };
        let client = ArchiveClient::new(DEFAULT_BASE, Some(key.clone()));
        let discovery_base = default_discovery_base(DEFAULT_BASE);

        let filter = DatasetFilter {
            asset: Some("btc".to_string()),
            tenor: Some("5m".to_string()),
            ..Default::default()
        };
        let page = client.discover_datasets(&discovery_base, &filter).expect("discover btc/5m");
        assert!(!page.datasets.is_empty(), "expected at least one btc/5m dataset row");
        assert!(page.datasets.iter().all(|d| d.layout == "family"));
        let date = page.datasets[0].date.clone();

        let resolved = client
            .resolve_dataset(&discovery_base, &date, Stream::Book, Some("btc"), Some("5m"))
            .expect("resolve one btc/5m dataset");
        assert_eq!(resolved.asset.as_deref(), Some("btc"));
        assert_eq!(resolved.tenor.as_deref(), Some("5m"));

        // The resolved URL must be directly openable — prove it with a real ranged HEAD/GET,
        // exactly like `live_range_read_against_free_sample` does for the flat/keyless sample.
        let agent = ureq::Agent::config_builder().http_status_as_error(false).build().new_agent();
        let reader = HttpRangeReader::open(agent, resolved.url.clone(), Some(key))
            .expect("open the discovered family URL");
        assert!(reader.len() > 0);
        println!(
            "live discovery: {date}/book_events resolved to {} ({} bytes, layout={})",
            resolved.url, resolved.bytes, resolved.layout
        );
    }
}
