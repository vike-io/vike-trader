//! `data.vike.io` **events API** ingest — the low-latency, no-ClickHouse-no-download counterpart to
//! [`crate::vike_archive`]'s bulk Parquet path: `GET https://data.vike.io/v1/events?token_id=<id>&
//! from_ms=<ms>&to_ms=<ms>&limit=<n>[&cursor=<c>]`, paged JSON, one row per book/trade/status event —
//! the exact rows a customer with no ClickHouse and no economical multi-GB day-file download wants,
//! for the ONE token they care about. Auth is the same `X-API-Key` header, same
//! `VIKE_ARCHIVE_API_KEY` workspace `.env` key (this module takes it as a plain `Option<String>`
//! parameter — only the `events_api_backfill` bin reads env/`.env`, per the repo's "libraries take
//! config as parameters" rule). Lives behind the EXISTING `vike-archive` feature (no new feature) —
//! `ureq` (workspace-pinned blocking HTTP; no new dep) is all this needs, `bytes`/Parquet are unused
//! here but already compiled in under that feature for the sibling module.
//!
//! **Row shape (verified live 2026-07-28 against a real `btc-5m` token)**: `ts, local_ts, seq,
//! event_type, is_snapshot, side, price, size, best_bid, best_ask, bids, asks, status`. This is the
//! JSON twin of the recorder's `book_events` table — SAME field names, JSON instead of Parquet,
//! live-served instead of exported. ⚠ The authoritative column list used to be `backtest_bridge`'s
//! module doc, and that file was DELETED on 2026-09-20 with the rest of this crate's ClickHouse
//! surface (`crates/vike-data/src/store/backtest_store.rs` carries why). What survives in this
//! tree is the ARCHIVE's shape rather than the database's:
//! `crates/vike-backfill/src/vike_archive.rs`'s module doc for the flat date-partitioned layout and
//! `crates/vike-data/src/store/archive_store.rs`'s for the family day-file one. Three things this
//! endpoint does
//! DIFFERENTLY from every sibling ingest path, all verified against real traffic, not assumed:
//!
//! 1. **`bids`/`asks` already carry the single-level ladder on a `price_change` (delta) row** — e.g.
//!    `side="sell", price=0.61, size=56` arrives with `asks="[[0.61,56.0]]", bids=""`, INCLUDING a
//!    zero-size removal (`bids="[[0.38,0.0]]"` alongside `size=0`). The archive Parquet path and the
//!    ClickHouse `book_events` table both leave `bids`/`asks` empty on a delta row and require the
//!    caller to reconstruct a one-element ladder from `price`/`size`/`side` — this endpoint has
//!    already done that reconstruction server-side. So [`book_update_from_row`] parses `bids`/`asks`
//!    UNIFORMLY for both `Snapshot` and `Delta` kinds; it never touches `price`/`size` for a book row
//!    (only [`trade_from_row`] does).
//! 2. **`best_bid`/`best_ask` are always `0.0` in production today** (verified: >10,000 consecutive
//!    rows of a real, actively-traded `btc-5m` market, 2026-07-28, zero non-zero sightings — not a
//!    quiet market; the same window's `book` snapshot carries a live ask ladder from 0.46 to 0.99).
//!    The field exists on the wire (reserved for a future L1-quote lane) but is not populated
//!    server-side yet. [`quote_from_row`] is written to derive a [`QuoteTick`] the moment either
//!    field goes non-zero, but **today it always returns `None`** — stated plainly per the design
//!    brief's instruction, not papered over: an events-API-sourced store has NO `quote` (`l1`) lane
//!    until the server starts populating these fields.
//! 3. **No `tick_size` field exists on the wire at all** (unlike the archive Parquet's `tick_size`
//!    column) — and this is NOT a cosmetically-inert gap, live-proof-verified (2026-07-28,
//!    `poly_mm_batch` against a real ingested store): `vike_sim`'s engine replay constructs
//!    the L2 book as `L2Book::new(snapshot.tick_size)`, and `L2Book::new` falls back to `1.0` for
//!    any non-positive input. A real Polymarket outcome token trades in `[0.0, 1.0]` at roughly a
//!    `0.01` tick — quantizing it at `1.0` collapses EVERY price into one of two buckets (`0.0` or
//!    `1.0`), corrupting the book beyond usability for any tick-size-sensitive maker. Confirmed via
//!    a direct `L2Book` fold of real ingested rows: at the real ~`0.01` granularity the book holds
//!    sane multi-level state (`best_bid ≈ 0.99`); at `tick_size = 0.0` (→ internally `1.0`) the SAME
//!    rows collapse to `best_bid = 1.0`. Live consequence: over a real 26-market slice, EVERY
//!    tick-sensitive config (`spread_maker`/`gueant_maker`, all 12 sweep points, both the `l1` and
//!    `l2` fill lanes) traded **zero** times, while `trailing_scalper` (which prices off the trade
//!    tape, not book tick granularity) traded normally (254-262 fills, ≈+0.76%, ≈46% win rate) over
//!    the identical store — proof the underlying book+trade DATA is sound and the failure is
//!    specifically the missing tick metadata. [`book_update_from_row`] therefore defaults
//!    `BookUpdate.tick_size` to [`FALLBACK_TICK_SIZE`] (`0.01`, Polymarket's standard tick, matching
//!    every observed real price level in this dataset — e.g. the `0.01`-`0.99` ladder cited above)
//!    rather than `0.0`, so a fresh ingest never hits the `L2Book::new` degenerate-fallback path.
//!    This is still a real, stated gap (a market on a genuinely finer/coarser tick would be
//!    misquantized) — just a far less harmful default than `0.0`, which is actively wrong for
//!    every market on this venue, not merely imprecise for an unusual one.
//!
//! **Multi-token variant, detected not assumed**: a parallel PR on the poly-l2-recorder side is
//! adding `token_ids=a,b,c` (capped per request; response rows gain a `token_id` field; cursor
//! becomes `local_ts:seq:token_id`). Verified live 2026-07-28: NOT yet deployed — `token_ids=` on
//! this venue's live endpoint returns `HTTP 400 {"error":"get_events requires token_id"}`.
//! [`EventsClient::probe_multi_token`] does the one-request detection call this module doc promises
//! rather than assuming either way; [`EventRow::token_id`] parses the field whenever present so a
//! multi-token response decodes correctly the day it ships, no code change needed; [`paginate`]
//! never interprets `next_cursor` at all (2-part or 3-part — it is opaque, round-tripped verbatim to
//! the next request exactly as the server issued it), so the cursor-shape change is inert here too.
//!
//! Ingest mirrors [`crate::vike_archive`]'s per-symbol grouping + idempotent commit-key idiom, keyed
//! `eventsapi:{kind}:{token}:{day}` (WITH the token component, unlike `vike_archive`'s date-only key —
//! this endpoint is fetched one token at a time by design, so the key must disambiguate tokens
//! sharing a store, whereas `vike_archive` ingests one venue-wide date file at a time).

use std::collections::HashMap;

use vike_data::store::archive_store::parse_levels_json;
use vike_data::{DataFusionHist, HistStore};
use vike_model::{BookUpdate, BookUpdateKind, QuoteTick, TradeTick};

use crate::error::CollectError;

/// The only venue this endpoint serves today.
pub const VENUE: &str = "polymarket";
/// The live events API base (no trailing slash, no `/v1` — callers get the versioned path from
/// [`events_url_single`]/[`events_url_multi`]).
pub const DEFAULT_BASE: &str = "https://data.vike.io";
/// The auth header name (same as [`crate::vike_archive::ArchiveClient`]).
const API_KEY_HEADER: &str = "X-API-Key";
/// Vendor prefix for error messages (mirrors the sibling ingest modules' `CTX` convention).
const CTX: &str = "events-api";
/// [`book_update_from_row`]'s `tick_size` fallback — this endpoint has no wire field for it at all
/// (see the module doc's point 3). `0.01` is Polymarket's standard outcome-token tick (matches
/// every observed real price level in this dataset), chosen specifically because `0.0` is actively
/// harmful, not merely imprecise: `vike_sim`'s engine `L2Book::new` falls back to `1.0` for
/// any non-positive tick, which quantizes a `[0.0, 1.0]`-bounded market into two buckets and breaks
/// every tick-size-sensitive maker (live-proof-verified — see the module doc).
pub const FALLBACK_TICK_SIZE: f64 = 0.01;

// ---- wire schema (pure deserialize; no I/O) -----------------------------------------------------

/// One `/v1/events` row, verbatim wire shape. `is_snapshot` arrives as a JSON `0`/`1` (not a JSON
/// bool) — decoded as `u8` and compared `!= 0` at use sites, never assumed. `token_id` is `None` on
/// every row today (the single-token endpoint doesn't echo it back); the multi-token variant adds it
/// per row — parsed opportunistically so decode is correct the day that ships (see the module doc).
#[derive(Debug, Clone, PartialEq, serde::Deserialize)]
pub struct EventRow {
    pub ts: i64,
    #[serde(default)]
    pub local_ts: i64,
    #[serde(default)]
    pub seq: u64,
    pub event_type: String,
    #[serde(default)]
    pub is_snapshot: u8,
    #[serde(default)]
    pub side: String,
    #[serde(default)]
    pub price: f64,
    #[serde(default)]
    pub size: f64,
    #[serde(default)]
    pub best_bid: f64,
    #[serde(default)]
    pub best_ask: f64,
    #[serde(default)]
    pub bids: String,
    #[serde(default)]
    pub asks: String,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub token_id: Option<String>,
}

impl EventRow {
    /// `true` on a full-depth snapshot row — mirrors `is_snapshot != 0`, the one place this crate
    /// reads the numeric flag directly (decode otherwise keys off `event_type`, matching the
    /// archive/ClickHouse siblings).
    pub fn is_snapshot(&self) -> bool {
        self.is_snapshot != 0
    }
}

/// One `/v1/events` page.
#[derive(Debug, Clone, PartialEq, serde::Deserialize)]
pub struct EventsResponse {
    pub events: Vec<EventRow>,
    pub has_more: bool,
    #[serde(default)]
    pub next_cursor: Option<String>,
}

/// Pure JSON → [`EventsResponse`] decode (no I/O).
pub fn parse_events_response(json: &str) -> Result<EventsResponse, CollectError> {
    serde_json::from_str(json).map_err(|e| CollectError::Fetch(format!("{CTX} page parse: {e}")))
}

// ---- URL builders (pure; unit-tested) -------------------------------------------------------------

/// `token_id=` single-token page URL. `cursor`, when given, is round-tripped VERBATIM (raw colons —
/// verified live the server accepts an unencoded `local_ts:seq` cursor on the wire).
pub fn events_url_single(
    base: &str,
    token_id: &str,
    from_ms: i64,
    to_ms: i64,
    limit: u32,
    cursor: Option<&str>,
) -> String {
    let mut url = format!(
        "{base}/v1/events?token_id={token_id}&from_ms={from_ms}&to_ms={to_ms}&limit={limit}"
    );
    if let Some(c) = cursor {
        url.push_str("&cursor=");
        url.push_str(c);
    }
    url
}

/// `token_ids=a,b,c` multi-token page URL (the not-yet-live variant — see the module doc). Built
/// unconditionally so [`EventsClient::probe_multi_token`] can attempt it and detect support live
/// rather than the module assuming either way.
pub fn events_url_multi(
    base: &str,
    token_ids: &[String],
    from_ms: i64,
    to_ms: i64,
    limit: u32,
    cursor: Option<&str>,
) -> String {
    let joined = token_ids.join(",");
    let mut url = format!(
        "{base}/v1/events?token_ids={joined}&from_ms={from_ms}&to_ms={to_ms}&limit={limit}"
    );
    if let Some(c) = cursor {
        url.push_str("&cursor=");
        url.push_str(c);
    }
    url
}

/// How many `:`-separated parts a cursor carries — 2 (`local_ts:seq`, today's single-token shape) or
/// 3 (`local_ts:seq:token_id`, the multi-token shape). Purely diagnostic: [`paginate`] never
/// interprets a cursor, only round-trips it, so an unexpected part count is not an error here — a
/// caller MAY log it, nothing more.
pub fn cursor_part_count(cursor: &str) -> usize {
    cursor.split(':').count()
}

// ---- row decode (pure; unit-tested) ----------------------------------------------------------------

/// The symbol to file a row under: the row's OWN `token_id` when present (multi-token mode),
/// else `fallback` (the token this request was made for — single-token mode, where the API never
/// echoes the id back).
fn row_symbol(row: &EventRow, fallback: &str) -> String {
    row.token_id.clone().unwrap_or_else(|| fallback.to_string())
}

/// Decode one row into a [`BookUpdate`], or `None` for a row kind that carries no book state
/// (`"trade"` — see [`trade_from_row`] — or an unrecognized `status` label, skipped defensively
/// rather than guessed at). `bids`/`asks` are parsed UNIFORMLY for `Snapshot` AND `Delta` — verified
/// live that a `price_change` row already carries its one-element ladder in `bids`/`asks` (see the
/// module doc point 1), so `price`/`size`/`side` are never read here. `tick_size` has no wire field on
/// this endpoint and is defaulted to [`FALLBACK_TICK_SIZE`] (module doc point 3 — a stated,
/// live-proof-verified gap, not a guess).
pub fn book_update_from_row(row: &EventRow, fallback_symbol: &str) -> Option<BookUpdate> {
    let kind = match row.event_type.as_str() {
        "book" => BookUpdateKind::Snapshot,
        "price_change" => BookUpdateKind::Delta,
        "status" => match row.status.as_str() {
            "gap_start" => BookUpdateKind::GapStart,
            "stale" => BookUpdateKind::Stale,
            "live_resume" => BookUpdateKind::LiveResume,
            _ => return None, // unrecognized status label — skip rather than guess
        },
        // "trade" -> trade_from_row's job; anything else is unrecognized -> skip.
        _ => return None,
    };
    let (bids, asks) = match kind {
        BookUpdateKind::GapStart | BookUpdateKind::Stale | BookUpdateKind::LiveResume => {
            (Vec::new(), Vec::new())
        }
        _ => (parse_levels_json(&row.bids), parse_levels_json(&row.asks)),
    };
    Some(BookUpdate {
        ts: row.ts,
        local_ts: row.local_ts,
        seq: row.seq,
        kind,
        tick_size: FALLBACK_TICK_SIZE, // no wire field on this endpoint — see the module doc's point 3
        bids,
        asks,
        symbol: row_symbol(row, fallback_symbol),
    })
}

/// Decode one `event_type="trade"` row into a [`TradeTick`]. `side` is the TAKER side — `"sell"`
/// means the taker sold, i.e. the resting maker bought (`is_buyer_maker = true`), the SAME inversion
/// [`crate::vike_archive::trades_from_batch`] applies — and that the deleted
/// `backtest_bridge::trades_from_batch` applied to the live ClickHouse tape, which is where the
/// convention was first pinned. Kept identical here so an events-API-sourced store is
/// byte-comparable to the archive-sourced one over the same window.
pub fn trade_from_row(row: &EventRow, fallback_symbol: &str) -> TradeTick {
    TradeTick {
        ts: row.ts,
        local_ts: row.local_ts,
        price: row.price,
        size: row.size,
        is_buyer_maker: row.side == "sell",
        symbol: row_symbol(row, fallback_symbol),
    }
}

/// Derive a [`QuoteTick`] from `best_bid`/`best_ask` — `None` when BOTH are `0.0`, the sentinel this
/// endpoint uses for "not populated" today (verified live, see the module doc's point 2: every row of
/// a real, active market carries `best_bid == best_ask == 0.0`). `bid_size`/`ask_size` have no wire
/// field on this endpoint either and default to `0.0` — the same documented-gap posture as
/// `tick_size` above; a caller that needs sized L1 quotes must use the archive's `l1_quotes` stream
/// or the ClickHouse bridge instead, not this endpoint, until the server populates these fields.
pub fn quote_from_row(row: &EventRow, fallback_symbol: &str) -> Option<QuoteTick> {
    if row.best_bid == 0.0 && row.best_ask == 0.0 {
        return None;
    }
    Some(QuoteTick {
        ts: row.ts,
        local_ts: row.local_ts,
        bid: row.best_bid,
        ask: row.best_ask,
        bid_size: 0.0,
        ask_size: 0.0,
        symbol: row_symbol(row, fallback_symbol),
    })
}

// ---- paging (pure loop over an injected page-fetch fn; unit-tested with synthetic pages) ---------

/// Fetch-and-decode totals for one paged pull — the "how many round trips, how many bytes" the
/// `--dry-run`-adjacent reporting and the live-proof measurement both want.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FetchStats {
    pub requests: usize,
    pub bytes: u64,
}

impl std::ops::AddAssign for FetchStats {
    fn add_assign(&mut self, other: Self) {
        self.requests += other.requests;
        self.bytes += other.bytes;
    }
}

/// Page to completion over `fetch_page` (`cursor -> (body, bytes_received)`), accumulating every
/// row and req/byte count. Stops when a page reports `has_more: false`, OR — defensively, never an
/// infinite loop — when `has_more: true` but `next_cursor` is absent (a malformed/degenerate
/// response; the rows already collected are still returned, `Ok`, not an error). Network-free: the
/// real client wires `fetch_page` to a `ureq` GET; tests wire it to a canned in-memory sequence.
pub fn paginate<F>(mut fetch_page: F) -> Result<(Vec<EventRow>, FetchStats), CollectError>
where
    F: FnMut(Option<&str>) -> Result<(String, u64), CollectError>,
{
    let mut events = Vec::new();
    let mut stats = FetchStats::default();
    let mut cursor: Option<String> = None;
    loop {
        let (body, bytes) = fetch_page(cursor.as_deref())?;
        stats.requests += 1;
        stats.bytes += bytes;
        let resp = parse_events_response(&body)?;
        events.extend(resp.events);
        if !resp.has_more {
            break;
        }
        match resp.next_cursor {
            Some(next) => cursor = Some(next),
            None => break, // has_more true but no cursor — defensive stop, not a hang
        }
    }
    Ok((events, stats))
}

// ---- ingest (per-symbol grouping + idempotent commit keys) ---------------------------------------

/// Which of the three derived series to ingest — the `--kind` CLI vocabulary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KindsMask {
    pub book: bool,
    pub trade: bool,
    pub quote: bool,
}

impl KindsMask {
    pub fn all() -> Self {
        Self { book: true, trade: true, quote: true }
    }

    /// Parse a `--kind` value (`book`/`trade`/`quote`/`all`) — `None` on an unrecognized label.
    pub fn from_kind(s: &str) -> Option<Self> {
        match s {
            "all" => Some(Self::all()),
            "book" => Some(Self { book: true, trade: false, quote: false }),
            "trade" => Some(Self { book: false, trade: true, quote: false }),
            "quote" => Some(Self { book: false, trade: false, quote: true }),
            _ => None,
        }
    }
}

/// Rows ingested per series (0 on a fully-deduped re-run of the same commit key).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct IngestCounts {
    pub book: usize,
    pub trade: usize,
    pub quote: usize,
}

impl std::ops::AddAssign for IngestCounts {
    fn add_assign(&mut self, other: Self) {
        self.book += other.book;
        self.trade += other.trade;
        self.quote += other.quote;
    }
}

/// Decode + ingest `rows` (already fetched — this fn does no I/O) into `store`, grouping by symbol
/// first (a multi-token page mixes several tokens; a single-token page's rows all fall under
/// `fallback_symbol`). Idempotent per `eventsapi:{kind}:{symbol}:{day}` (see the module doc) — an
/// identical re-run of the same `(symbol, day)` slice appends 0 rows on every series whose commit
/// key was already recorded, proven in `tests::ingest_rows_is_idempotent_per_symbol_and_day`.
pub fn ingest_rows(
    store: &DataFusionHist,
    fallback_symbol: &str,
    day: &str,
    rows: &[EventRow],
    kinds: &KindsMask,
) -> Result<IngestCounts, CollectError> {
    let mut books: HashMap<String, Vec<BookUpdate>> = HashMap::new();
    let mut trades: HashMap<String, Vec<TradeTick>> = HashMap::new();
    let mut quotes: HashMap<String, Vec<QuoteTick>> = HashMap::new();

    for row in rows {
        if kinds.book
            && let Some(u) = book_update_from_row(row, fallback_symbol)
        {
            books.entry(u.symbol.clone()).or_default().push(u);
        }
        if kinds.trade && row.event_type == "trade" {
            let t = trade_from_row(row, fallback_symbol);
            trades.entry(t.symbol.clone()).or_default().push(t);
        }
        if kinds.quote
            && let Some(q) = quote_from_row(row, fallback_symbol)
        {
            quotes.entry(q.symbol.clone()).or_default().push(q);
        }
    }

    let mut counts = IngestCounts::default();
    for (symbol, updates) in books {
        let key = format!("eventsapi:book:{symbol}:{day}");
        counts.book += store.append_book_updates(VENUE, &symbol, &updates, Some(&key))?;
    }
    for (symbol, ticks) in trades {
        let key = format!("eventsapi:trade:{symbol}:{day}");
        counts.trade += store.append_trades(VENUE, &symbol, &ticks, Some(&key))?;
    }
    for (symbol, ticks) in quotes {
        let key = format!("eventsapi:quote:{symbol}:{day}");
        counts.quote += store.append_quotes(VENUE, &symbol, &ticks, Some(&key))?;
    }
    Ok(counts)
}

// ---- the HTTP client -------------------------------------------------------------------------------

/// Blocking client over one events-API base + optional API key. Cheap to construct AND cheap to
/// clone (the `ureq::Agent` is pool-backed, `Clone` mirrors [`crate::vike_archive::ArchiveClient`]'s
/// own `agent.clone()` use) — the `events_api_backfill` bin clones one per worker thread rather than
/// sharing a reference, sidestepping any question of whether a given `ureq` release's `Agent` is
/// `Sync`.
#[derive(Clone)]
pub struct EventsClient {
    agent: ureq::Agent,
    base: String,
    api_key: Option<String>,
}

impl EventsClient {
    /// `api_key` rides every request (the endpoint is never open, unlike the archive's
    /// `manifest.json`). Callers resolve `VIKE_ARCHIVE_API_KEY` themselves (see the module doc) —
    /// this constructor never touches the environment.
    pub fn new(base: impl Into<String>, api_key: Option<String>) -> Self {
        let agent = ureq::Agent::config_builder().http_status_as_error(false).build().new_agent();
        Self { agent, base: base.into(), api_key }
    }

    fn get(&self, url: &str) -> Result<(String, u64), CollectError> {
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
        let bytes = body.len() as u64;
        Ok((body, bytes))
    }

    /// Page one `token_id` window to completion.
    pub fn fetch_all_single(
        &self,
        token_id: &str,
        from_ms: i64,
        to_ms: i64,
        page_limit: u32,
    ) -> Result<(Vec<EventRow>, FetchStats), CollectError> {
        paginate(|cursor| {
            let url = events_url_single(&self.base, token_id, from_ms, to_ms, page_limit, cursor);
            self.get(&url)
        })
    }

    /// Page one `token_ids=` window to completion — the not-yet-live multi-token variant (see the
    /// module doc). Callers should only reach for this after [`Self::probe_multi_token`] confirms
    /// support; used directly it simply surfaces whatever the server returns (today: `HTTP 400`).
    pub fn fetch_all_multi(
        &self,
        token_ids: &[String],
        from_ms: i64,
        to_ms: i64,
        page_limit: u32,
    ) -> Result<(Vec<EventRow>, FetchStats), CollectError> {
        paginate(|cursor| {
            let url = events_url_multi(&self.base, token_ids, from_ms, to_ms, page_limit, cursor);
            self.get(&url)
        })
    }

    /// The one-request detection call this module's design brief asks for: try `token_ids=` over
    /// `sample_tokens` (2+ recommended) with a tiny `limit`; `true` only on a genuine HTTP success —
    /// any error (the live `HTTP 400 {"error":"get_events requires token_id"}` observed 2026-07-28,
    /// a network failure, a decode failure) means "not supported today", never guessed either way.
    /// Never panics, never retries — one probe, one verdict; the caller falls back to per-token
    /// [`Self::fetch_all_single`] on `false`.
    pub fn probe_multi_token(&self, sample_tokens: &[String], from_ms: i64, to_ms: i64) -> bool {
        if sample_tokens.is_empty() {
            return false;
        }
        let url = events_url_multi(&self.base, sample_tokens, from_ms, to_ms, 1, None);
        match self.get(&url) {
            Ok((body, _)) => parse_events_response(&body).is_ok(),
            Err(_) => false,
        }
    }
}

#[path = "events_api_tests.rs"]
#[cfg(test)]
mod events_api_tests;
