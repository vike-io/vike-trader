//! Stream naming, URL builders, the manifest and the v1 discovery API (pure; no I/O).

use std::collections::HashMap;

use vike_data::StoreKindId;
use vike_store::archive_store::VENUE;

use super::CTX;
use crate::error::CollectError;

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

    /// The store kind this stream ingests as — and so the `kind` label its commit keys carry.
    pub fn kind(self) -> StoreKindId {
        match self {
            Stream::Book => StoreKindId::Book,
            Stream::Trade => StoreKindId::Trade,
            Stream::Quote => StoreKindId::Quote,
        }
    }

    /// The stream that ingests `kind`, or `None` for a kind the archive does not carry — the
    /// inverse of [`Stream::kind`].
    pub fn from_kind(kind: StoreKindId) -> Option<Self> {
        match kind {
            StoreKindId::Book => Some(Stream::Book),
            StoreKindId::Trade => Some(Stream::Trade),
            StoreKindId::Quote => Some(Stream::Quote),
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
/// so `ArchiveClient::ingest_family_stream`/`ArchiveClient::ingest_family_stream_bulk` stay
/// under clippy's `too_many_arguments` gate rather than each taking three more loose `&str`s.
/// Borrowed, not owned: callers already hold these for the whole ingest call (CLI argv strings).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FamilyTarget<'a> {
    pub discovery_base: &'a str,
    pub asset: &'a str,
    pub tenor: &'a str,
}

#[cfg(test)]
#[path = "datasets_tests.rs"]
mod datasets_tests;
