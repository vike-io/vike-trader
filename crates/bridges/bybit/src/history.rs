//! Bybit V5 REST kline history (spot AND linear perp) — the ONE kline fetch + JSON→[`Bar`] mapper
//! for Bybit.
//!
//! The Bybit twin of binance's `data` module: a pure, fixture-tested `parse_bybit_klines` map and a
//! paged, rate-limited `fetch_klines_range`. Bybit differs from Binance in four ways this module
//! localises:
//!   1. **Envelope + ordering.** The body is the V5 `{retCode, retMsg, result:{list}}` envelope, and
//!      `result.list` is sorted **newest-first** as 7-element string arrays
//!      `[startTime, open, high, low, close, volume, turnover]`. `parse_bybit_klines` maps each row
//!      and **reverses** to ascending-by-startTime — the store's contract (and Binance's order).
//!   2. **Interval codes.** Bybit encodes intervals as bare minute counts / letter codes
//!      ("1m"→"1", "1h"→"60", "1d"→"D"); [`interval_code`] maps the binance-style input. The store
//!      key keeps the ORIGINAL interval string so series line up across venues.
//!   3. **Rate-limit signalling.** Bybit returns HTTP 200 for business errors, so a throttle trip
//!      shows up as `retCode` 10006 ("too many visits") / 10018 ("IP rate limit") in the body — plus
//!      HTTP 429/403 for a hard Cloudflare/IP block. Both are treated as retryable.
//!   4. **END-ANCHORED paging.** `/v5/market/kline` does NOT honor `start` once the window exceeds
//!      `limit`: it silently returns the NEWEST `limit` rows ending at `end` and answers
//!      `retCode: 0`, so the truncation looks like a healthy success. MEASURED live 2026-08-04
//!      against `category=linear&symbol=BTCUSDT&interval=1`: a 60-minute window (fits under the cap)
//!      came back as 60 rows whose oldest row IS the requested `start`; a 30-day window (43,200
//!      minutes) came back as exactly 1000 rows whose oldest row was ~16.6 h before `end` — the
//!      requested `start` ignored entirely. A binance-style FORWARD pager therefore self-terminates
//!      after ONE page: its first request comes back at the far END of the window, the cursor jumps
//!      past `end_ms`, and a 30-day backfill silently writes ~2% of the data.
//!
//!      The cure is deribit's/okx's pattern, not binance's — walk `end` DOWN
//!      (`end = oldest_returned_kline - 1`) until the window start is covered or a page comes back
//!      empty, then sort ascending and dedup. `start` stays the REAL window start on every page (the
//!      venue clamps it itself; that clamp is exactly what the backward walk undoes). Termination is
//!      driven by OBSERVED ticks — [`MAX_LIMIT`] is a request parameter, never a stop condition.
//!      `end = oldest - 1` is contiguous, not lossy (verified live: the page after one whose oldest
//!      row was `1785803400000`, requested with `end=1785803399999`, came back newest-first from
//!      `1785803340000` — exactly one minute older, no gap).
//!
//!      ONE accepted residual of that termination rule (documented, not silently swallowed —
//!      deribit's and okx's pagers have the identical shape): an EMPTY page stops the walk, so a
//!      hypothetical hole in the middle of a window would hide anything older than it. In practice an
//!      empty page means "older than this symbol's listing", which is exactly where the walk should
//!      stop.
//!
//!      The walk is the shared one, so the truncation trap is gated WITHOUT network I/O:
//!      [`page_walk_backward`](vike_data::source::page_walk_backward) is the rule (`end` one ms below
//!      each page's oldest row) over an INJECTED page fetcher — a test that only exercises a single
//!      page cannot see this bug at all — and
//!      [`kline_page_walk_backward`](vike_bridge_core::klines::kline_page_walk_backward) runs it paced,
//!      for this module's kline pager.
//!
//! Endpoint: `GET <base>/v5/market/kline?category=&symbol=&interval=&start=&end=&limit=1000`, `<base>` the
//! venue's `live` `history` `rest` row (`https://api.bybit.com` in the seed; [`KlineSourceBybit::from_urls`]
//! looks it up) — `category` is one of [`Category`]'s three values, chosen by [`route_target`] (Bybit V5
//! unifies spot/linear/inverse under ONE REST base, unlike Binance's separate fapi host).
//! Only `fetch_klines_range` does network I/O; `parse_bybit_klines` is a pure, fixture-tested map.
//!
//! ## ⚠ 5. A bare symbol can name TWO Trading books here, and an unclaimed one is now REFUSED
//!
//! Phase 0 of `docs/decisions/0061-an-instrument-names-its-kind.md`. Bybit lists `BTCUSD` as a
//! Trading SPOT pair **and** a Trading inverse perpetual; the spot side is vestigial (four of six
//! bars zero-volume against a price ~$70 stale) and the bare→spot rule sent every caller there in
//! silence. [`route_target`] is the whole decision and [`ambiguous_bare_symbol_refusal`] the answer
//! it gives; the predicate, the two counts that sized it and its residual live in
//! [`crate::instrument::listing`].
//!
//! Three properties worth carrying before you edit anything here:
//!
//!   * **Only the caller who named NOTHING is refused.** `Some(class)` routes on the claim and is
//!     never refused for ambiguity — that is what [`fetch_klines_range_classed`] is for.
//!   * **`.P` says PERPETUAL, not inverse — and that is now true of the CODE, not only the prose.**
//!     This bullet used to warn that `BTCUSD.P` reached the inverse book by two accidents: a `USD`
//!     rather than `USDT` quote asset, and bybit resolving a `category=linear` request leniently.
//!     **Neither is in the routing path any more.** A perpetual claim names the PRODUCT and
//!     [`crate::instrument::listing::perp_book`] asks the VENUE which of its two derivative books carries
//!     it, so a `.P` symbol reaches `category=inverse` because the venue LISTS it there, never
//!     because of how it is spelled. Unchanged: the suffix is still not a way to SELECT inverse, and
//!     [`ambiguous_bare_symbol_refusal`] still refuses to recommend it.
//!   * **The instrument's own words are carried**: `crates/bridges/bybit/src/catalog.rs`'s
//!     `parse_perp`/`parse_inverse` type each row as a `vike_model::Contract` (margining from
//!     `contractType`, settle asset from `settleCoin`) and keep `contractType` verbatim on
//!     `vike_model::Instrument`'s `contract_type`. ⚠ Nothing in this module reads either, and that
//!     stays deliberate: the route here is a listing MEMBERSHIP
//!     (`crates/bridges/bybit/src/instrument/listing.rs`) — a venue-local fact, not a second reading of the
//!     core vocabulary — so the contract stays IDENTIFICATION, for rendering and auditing.
//!
//! ## ⚠ 6. Three categories, and the third one is why `rest_category` stopped taking a `bool`
//!
//! Phase 4 of the same record. [`Category`] is the three books, [`PerpListingAnswer`] is what the venue's own
//! instrument listings say about one symbol, and [`route_target`] is still the whole decision — five
//! arms now instead of four. The point-5 refusal is untouched: a caller who names NOTHING on an
//! ambiguous bare symbol gets the same message byte for byte. What changed is that a caller who
//! names `CryptoPerp` — or picks the perp row out of the catalog — is now SERVED rather than sent to
//! `category=linear` and rescued by the venue's leniency.
//!
//! ⚠ **Producing `inverse` is not what made the inverse TAPE reachable**, and this module would be
//! overclaiming if it said so: MEASURED 2026-09-16, the kline endpoint resolves `category`
//! leniently in both directions and served the right rows either way. What the third value buys is
//! a request that says what it means, and a decision that [`crate::feed`] can share — the WS
//! is STRICT per host, and there the same choice is the difference between a live chart and a
//! reconnect loop.
//!
//! ## Rate-limit strategy (paged `fetch_klines_range`)
//! Bybit's public REST is IP-limited (~600 req / 5 s per IP — generous), so the pager stays well
//! under it: a fixed [`PAGE_DELAY`] throttle between pages, and on a rate-limit signal (retCode
//! 10006/10018 or HTTP 429/403) an exponential backoff that HONORS `Retry-After`, retrying the same
//! page under the shared history policy (`vike_bridge_core::retry::RetryBackoffPolicy::HISTORY_PAGE`: 6
//! retries, 1 s doubling to 60 s): one [`JsonPageSpec`] per endpoint ([`KLINES_PAGE`],
//! [`FUNDING_PAGE`]) carries the words and the rate-limit statuses and codes, and
//! `vike_bridge_core::retry::json_page_fetch` asks and judges each page. Pages BACKWARD (see point 4
//! above): `limit=1000` with `start` pinned to the real window start, moving `end` one ms below each
//! page's oldest kline.
//!
//! ## The pager MEASURES its pages (and still sleeps [`PAGE_DELAY`])
//! Bybit publishes no request-weight budget, so there is nothing to discover and nothing to pace a
//! fraction of — but that never stopped us timing our own round trips, and until now nobody had. The
//! sibling venue was MEASURED on the CI box 2026-08-04 (okx: 476–486 ms per page against a hardcoded
//! 200 ms delay) and this venue is the same shape: the constant is not even the dominant term of the
//! gap the venue actually sees.
//!
//! So the walk is the shared paced backward kline walk
//! ([`kline_page_walk_backward`](vike_bridge_core::klines::kline_page_walk_backward) on a
//! `kline_page_pacer` over [`PAGE_DELAY`]): the sleep runs through
//! [`vike_bridge_core::pacer::Pacer::fallback`] and every page is wall-clocked into
//! [`vike_bridge_core::pacer::Pacer::observe_request`]. Two things follow, and a third deliberately
//! does not:
//!   * the backfill prints ONE `info` ETA line after its first page, instead of going silent for
//!     minutes in a way indistinguishable from a hang;
//!   * [`fetch_klines_range_paced_classed`] hands the observation back so a caller can persist it
//!     and an operator can SEE what a page costs here (none has persisted it since
//!     docs/decisions/0094 deleted `vike-backfill`'s pace file);
//!   * ⚠ **the delay does not change.** `Pacer::fallback::next_delay` returns exactly [`PAGE_DELAY`]
//!     whatever is observed (pinned by that module's tests and by
//!     `the_pager_measures_but_never_changes_the_page_delay` below). With no published ceiling, a
//!     delay derived from a stopwatch alone would be the same un-checkable hardcoded guess in newer
//!     clothes. Acting on the measurement is a separate change with its own measurement.

use std::time::Duration;

use vike_bridge_core::http::get_raw;
use vike_bridge_core::klines::{kline_page_pacer, kline_page_walk_backward, string_row_to_bar};
use vike_bridge_core::retry::{JsonPageSpec, RetryBackoffPolicy, json_page_fetch};
use vike_marketdata::Bar;
use vike_model::rate_limits::PaceSample;
use vike_transport::http::{body_head, json_body};
use vike_utility::errors::CarriedRefusal;
use vike_venues::venue_urls::{UrlChannel, UrlProtocol, UrlRefusal, UrlType, VenueUrls};

use crate::refusals::BybitHistoryRefusal;

/// This crate's canonical venue id — the key `vike_catalog::addressing_for` is looked up by.
///
/// ⚠ Spelled as a literal rather than read from [`crate::execution::rest::VENUE`] deliberately: that const
/// lives behind the default-on `execution` feature, and this module is on the FEEDS side of the seam
/// (`scripts/ci_feature_suite.sh`'s `bridges-feeds` arm compiles it with `--no-default-features`),
/// so reaching for it would make a keyless market-data build fail to compile. `catalog.rs`'s
/// `CatalogProvider::venue` spells it the same way for the same reason.
/// `the_venue_id_is_the_one_the_catalog_answers_with` pins the two equal.
const VENUE: &str = "bybit";

/// `GET /v5/market/kline`, on the `live` `history` `rest` base.
const PATH_KLINE: &str = "/v5/market/kline";
/// Bybit spot category — where an unclaimed, unambiguous bare symbol routes. The two derivative
/// spellings live in [`rest_category`] beside it; this one keeps its own const because the URL
/// tests below compare against it by name.
const CATEGORY: &str = "spot";
/// Bybit's hard cap on klines returned by a single request.
const MAX_LIMIT: usize = 1000;

// ---- paged-backfill rate-limit knobs (see the module rate-limit strategy) ----------------------
/// Fixed throttle between successive page requests (keeps us far under ~600 req/5 s per IP).
///
/// The VALUE comes from `vike_venues::venue_rate_limits`'s `BYBIT` row rather than from a literal
/// here: Bybit runs no request-weight meter and publishes no machine-readable rate-limit metadata,
/// so this delay IS the whole pacing story for history on this venue — which makes it a per-venue
/// fact worth declaring beside the other venues' rather than a local constant nothing can compare.
/// The table models that absence as `History::Unweighted` (no ceiling, no soft limit) instead of
/// inventing a number; the ~600 req/5 s figure above is documented prose, never a payload.
const PAGE_DELAY: Duration = vike_venues::venue_rate_limits::BYBIT.history.page_delay();
/// Bybit rate-limit business codes (arrive with HTTP 200): "too many visits" / "IP rate limit".
const RATE_LIMIT_CODES: [i64; 2] = [10006, 10018];

/// Map a binance-style interval ("1m","1h","1d",…) to Bybit's V5 interval code ("1","60","D",…), from
/// the venue's word table in `vike_catalog::venue_interval_word` (the one table, whose membership is the
/// catalog's interval row too). Errors on an unsupported interval rather than guessing. Note "1M"
/// (month) vs "1m" (minute) is case-sensitive, matching the binance-style input convention.
/// `pub(crate)` so the live kline feed ([`crate::feed`]) builds the `kline.<code>.<SYMBOL>` WS
/// topic from the same map.
pub(crate) fn interval_code(interval: &str) -> Result<&'static str, BybitHistoryRefusal> {
    vike_catalog::venue_interval_word(VENUE, interval)
        .ok_or_else(|| BybitHistoryRefusal::UnsupportedInterval { interval: interval.to_string() })
}

/// Pull ascending-by-startTime [`Bar`]s out of a parsed V5 kline envelope: require `retCode == 0`,
/// map `result.list` (newest-first) and reverse. Shared by the pure map and the pager.
fn bars_from_envelope(v: &serde_json::Value) -> Result<Vec<Bar>, String> {
    let ret_code = v.get("retCode").and_then(serde_json::Value::as_i64);
    if ret_code != Some(0) {
        let msg = v.get("retMsg").and_then(serde_json::Value::as_str).unwrap_or("").to_string();
        return Err(BybitHistoryRefusal::KlinesRetCode { code: ret_code, msg }.to_string());
    }
    let list = v
        .get("result")
        .and_then(|r| r.get("list"))
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| BybitHistoryRefusal::KlinesNoList.to_string())?;
    let mut bars = list
        .iter()
        .map(|row| {
            row.as_array()
                .ok_or_else(|| BybitHistoryRefusal::KlineRowNotArray.to_string())
                .and_then(|r| string_row_to_bar(r, "bybit kline", "startTime"))
        })
        .collect::<Result<Vec<Bar>, String>>()?;
    bars.reverse(); // Bybit serves newest-first; the store wants ascending by startTime
    Ok(bars)
}

/// Pure map: a Bybit V5 klines response body → `Vec<Bar>` ascending by startTime. No network — this
/// is the fixture-tested seam.
pub fn parse_bybit_klines(body: &str) -> Result<Vec<Bar>, String> {
    bars_from_envelope(&json_body(body, "bybit kline")?)
}

/// Build the `GET /v5/market/kline` URL on `base` for the given bounds and limit. `category` is
/// `"spot"` or `"linear"` — a PARAMETER, not the const, so a perp backfill and the spot one cannot drift.
fn klines_url(
    base: &str,
    category: &str,
    symbol: &str,
    interval_code: &str,
    start_ms: i64,
    end_ms: i64,
    limit: usize,
) -> String {
    format!(
        "{base}{PATH_KLINE}?category={category}&symbol={symbol}&interval={interval_code}&start={start_ms}&end={end_ms}&limit={limit}"
    )
}

/// A readable body's rate-limit `retCode` (one of [`RATE_LIMIT_CODES`], which Bybit answers with HTTP 200), as
/// the note the exhaustion error carries.
fn rate_limit_code(v: &serde_json::Value) -> Option<String> {
    let code = v.get("retCode").and_then(serde_json::Value::as_i64).unwrap_or(-1);
    RATE_LIMIT_CODES.contains(&code).then(|| format!("retCode {code} (rate limited)"))
}

/// One `/v5/market/kline` page, as `vike_bridge_core::retry::json_page_fetch` asks and judges it: HTTP 429/418/403
/// (an IP block too: Bybit fronts REST with Cloudflare) or a `retCode` in [`RATE_LIMIT_CODES`] waits (honoring
/// `Retry-After`) and asks the same page again.
const KLINES_PAGE: JsonPageSpec = JsonPageSpec {
    label: "bybit klines",
    json_what: "bybit kline",
    headers: &[],
    limited: &[429, 418, 403],
    business_limit: rate_limit_code,
    backoff: RetryBackoffPolicy::HISTORY_PAGE,
};

/// Fetch ONE page under [`KLINES_PAGE`]. Returns the page's bars ascending by startTime.
fn fetch_page(
    agent: &ureq::Agent,
    base: &str,
    category: &str,
    symbol: &str,
    interval_code: &str,
    start_ms: i64,
    end_ms: i64,
) -> Result<Vec<Bar>, String> {
    let url = klines_url(base, category, symbol, interval_code, start_ms, end_ms, MAX_LIMIT);
    json_page_fetch(agent, &url, &KLINES_PAGE, bars_from_envelope)
}

/// **The three books bybit's V5 REST serves, as this crate names them** — the return of
/// [`route_target`] and the input to every URL built here.
///
/// It is venue-LOCAL vocabulary on purpose. `vike_catalog`'s `addressing` module doc forbids
/// `linear`/`inverse` appearing in the core crate at all (*"the vocabulary names the PRODUCT, never
/// the venue's word for the route"*), and `docs/decisions/0061-an-instrument-names-its-kind.md`
/// verdict 1 refuses to split `AssetClass`'s perp variant to carry the difference. So the
/// translation from a PRODUCT claim to a ROUTE happens exactly here, one venue wide.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Category {
    /// `category=spot` — the spot pair.
    Spot,
    /// `category=linear` — the quote-settled derivative book.
    Linear,
    /// `category=inverse` — the coin-settled derivative book.
    Inverse,
}

impl Category {
    /// The venue's own spelling, for the `category=` query parameter.
    #[must_use]
    pub fn wire(self) -> &'static str {
        rest_category(self)
    }

    /// **The book the venue's own category word names** (`spot`, `linear`, `inverse`: [`Self::wire`] read back), or
    /// `None` for a word of no book this crate addresses (`option`, a word the venue added).
    #[must_use]
    pub fn of_word(word: &str) -> Option<Self> {
        [Category::Spot, Category::Linear, Category::Inverse].into_iter().find(|b| b.wire() == word)
    }

    /// **The book a kept row says**: its `wire.group`, the venue's own category word (`crate::catalog` stamps it,
    /// spelled by [`Self::wire`]); a row kept before that field was stamped says it by its TYPED contract (a spot
    /// pair, or a derivative's margining, the very facts the stamp is read from: `inverse` is
    /// `vike_model::Contract::is_inverse`, so an option is never in it). `None` for a row of neither.
    #[must_use]
    pub fn of_row(row: &vike_model::Instrument) -> Option<Self> {
        use vike_model::{Contract, Margining};
        if let Some(group) = row.wire.group.as_deref() {
            return Self::of_word(group);
        }
        match &row.contract {
            Contract::CryptoSpot => Some(Category::Spot),
            contract if contract.is_inverse() => Some(Category::Inverse),
            contract if contract.margining() == Some(Margining::Linear) => Some(Category::Linear),
            _ => None,
        }
    }
}

/// The `category=` value for one [`Category`] — the ONE place that turns this crate's routing
/// decision into the venue's word, so the URL-building tests below and the real fetches never
/// drift. Mirrors binance's `rest_klines_base(perp)`, widened from a `bool` to three answers by
/// `docs/decisions/0061-an-instrument-names-its-kind.md` phase 4.
///
/// ⚠ **Producing `inverse` here is not what makes the inverse tape reachable, and nobody should
/// read it that way.** MEASURED live 2026-09-16: `/v5/market/kline` resolves `category` LENIENTLY
/// for derivatives in BOTH directions — `category=linear&symbol=BTCUSD` returns the inverse tape
/// byte-identically to `category=inverse&symbol=BTCUSD`, and `category=inverse&symbol=BTCUSDT`
/// returns the linear one. The endpoint knows "spot" and "the derivative of that name", and nothing
/// finer. What the third value buys is that the REQUEST says what it MEANS: the leniency is a
/// property of the venue's API rather than a claim this code is entitled to rely on, the crate's
/// own `route_target` ⚠ says so, and the WS is not lenient at all (see
/// `crates/bridges/bybit/src/feed/mod.rs`'s `websocket_host`, where the same decision picks between
/// three STRICT hosts and picking wrong is a `handler not found` or a silently empty book).
fn rest_category(book: Category) -> &'static str {
    match book {
        Category::Spot => CATEGORY,
        Category::Linear => "linear",
        Category::Inverse => "inverse",
    }
}

/// Build the `GET /v5/market/kline` URL on `base` for the live feed's latest-N warmup (no start/end
/// bounds). `category` is `"spot"` or `"linear"` — pulled out so the URL-building test below and the
/// real fetch can never drift apart.
fn latest_url(
    base: &str,
    category: &str,
    symbol: &str,
    interval_code: &str,
    limit: usize,
) -> String {
    format!(
        "{base}{PATH_KLINE}?category={category}&symbol={symbol}&interval={interval_code}&limit={limit}"
    )
}

/// The live feed's warmup seed: the newest `limit` klines (no time bound), the last of which is the
/// still-forming candle — Bybit serves the in-progress candle as the newest `result.list` row, so
/// after [`parse_bybit_klines`] reverses to ascending it is the last bar. A single un-throttled
/// request — the Bybit twin of binance's `fetch_klines_latest`. `book` names the category (the same
/// endpoint host, just a different query category — Bybit V5 unifies spot/linear/inverse under one
/// REST base, unlike Binance's separate fapi host); the response shape is identical for all three,
/// so `parse_bybit_klines` is unchanged. Network I/O. Errors on an unsupported `interval`. `base` is the
/// `live` `history` `rest` row the caller resolved (the kline endpoint's one host).
///
/// ⚠ It takes the RESOLVED [`Category`] rather than a symbol, because its caller
/// (`crates/bridges/bybit/src/feed/bar.rs`'s `bars_plan`) has already resolved one to pick its WS
/// host, and a seed that re-derived the route could disagree with the socket it is seeding. The
/// `bool` this parameter used to be is exactly the disagreement that made a `.P` inverse symbol seed
/// from one book and stream from another.
pub fn fetch_klines_latest(
    base: &str,
    symbol: &str,
    interval: &str,
    limit: usize,
    book: Category,
) -> Result<Vec<Bar>, String> {
    let code = interval_code(interval).map_err(|refusal| refusal.to_string())?;
    let agent = vike_transport::http::blocking_agent();
    let url = latest_url(base, book.wire(), symbol, code, limit);
    let raw = get_raw(&agent, &url, "bybit klines", &[])?;
    if !(200..300).contains(&raw.status) {
        let head = body_head(&raw.body);
        return Err(BybitHistoryRefusal::KlinesHttp { status: raw.status, head }.to_string());
    }
    parse_bybit_klines(&raw.body)
}

/// **What the venue's own instrument listings say about ONE symbol** — the injected half of
/// [`route_target`], so the whole routing decision stays pure and assertable without a socket.
///
/// Both fields are FACTS READ OFF THE VENUE, never inferences from the symbol's shape.
/// [`crate::instrument::listing`] derives them (once per process) and [`range_target`] is where the two
/// halves meet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PerpListingAnswer {
    /// Does the venue list this bare symbol Trading under BOTH spot and inverse? The collision that
    /// refuses an unclaimed caller — `crates/bridges/bybit/src/instrument/listing.rs` carries the
    /// predicate, the counts that sized it and its residual.
    pub bare_is_ambiguous: bool,
    /// Which derivative book a PERPETUAL claim on this symbol names.
    pub perp_book: crate::instrument::listing::PerpListing,
}

impl PerpListingAnswer {
    /// The answer for a symbol whose listings were never consulted — spot-unambiguous, and a
    /// perpetual claim routing to linear. It is exactly this crate's behaviour before the listings
    /// were read, so a test that means "nothing was looked up" says so in one word.
    #[must_use]
    pub fn unconsulted() -> Self {
        Self {
            bare_is_ambiguous: false,
            perp_book: crate::instrument::listing::PerpListing::Unlisted,
        }
    }
}

/// **The whole routing decision, PURE** — `(category, wire_symbol)` or a refusal, given the
/// caller's symbol, the class the caller CLAIMED (if any), and what the venue's own instrument
/// listings say about that symbol.
///
/// `docs/decisions/0061-an-instrument-names-its-kind.md` verdict 3: *"the seam takes
/// `Option<AssetClass>`, and a missing claim on an AMBIGUOUS venue symbol is a REFUSAL … the value
/// that was missing is not a third product, it is the absence of a claim."* Every arm of that is
/// here, and [`PerpListingAnswer`] is INJECTED so the whole decision is assertable without network I/O — which
/// is what [`range_target`]'s extraction was for in the first place.
///
/// The five arms, in order:
///
/// 1. **A claim that this venue's data path cannot address** (`Equity`, `Fx`, …) is refused rather
///    than coerced. `vike_catalog::addressing_for` is the authority, not a list here.
/// 2. **A claim that CONTRADICTS the symbol's own suffix** (`BTCUSDT.P` claimed as spot) is refused.
///    Two claims disagreeing is not a case with a right answer.
/// 3. **A claim** otherwise routes on the claim, whatever the symbol looks like.
/// 4. **No claim**: a `.P` suffix IS the claim (perpetual — see the ⚠ below), and a bare symbol
///    routes to spot UNLESS the venue lists it under inverse too, which is the refusal.
/// 5. **A perpetual claim, however it arrived, is then routed to a BOOK** by the venue's listings —
///    [`Category::Linear`] or [`Category::Inverse`]. A symbol the venue lists under both derivative
///    categories is refused; there is no such symbol today, which is the point (see
///    `crate::instrument::listing::PerpListing::Both`).
///
/// ⚠ **`.P` says PERPETUAL. It does NOT say inverse, and nothing here should be read as saying it
/// does.** This used to be a warning about a bug: `BTCUSD.P` reached bybit's inverse book only
/// because the quote asset happens to be `USD` rather than `USDT` and because bybit RESOLVES a
/// `category=linear` kline request leniently. **Both accidents are now out of the routing path.** A
/// perpetual claim selects a PRODUCT, arm 5 asks the venue which BOOK carries that product, and the
/// suffix no longer decides a category anywhere in this crate. What survives unchanged is the rule
/// the warning protected: the suffix is not a way to SELECT inverse, and
/// [`ambiguous_bare_symbol_refusal`] still refuses to recommend it — a caller who means the
/// perpetual says `CryptoPerp` (or picks the perp row), and the venue says which book that is.
fn route_target(
    symbol: &str,
    claimed: Option<vike_model::AssetClass>,
    listed: PerpListingAnswer,
) -> Result<(Category, &str), BybitHistoryRefusal> {
    use vike_model::AssetClass;
    let (wire, suffixed) = vike_catalog::split_perp_at(VENUE, symbol);
    let Some(class) = claimed else {
        if suffixed {
            // The suffix is the claim: perpetual. WHICH perpetual book is the venue's answer.
            return Ok((perp_category(wire, listed.perp_book)?, wire));
        }
        if listed.bare_is_ambiguous {
            return Err(ambiguous_bare_symbol_refusal(wire));
        }
        return Ok((Category::Spot, wire));
    };
    if !vike_catalog::addressing_for(VENUE).addresses(class) {
        return Err(BybitHistoryRefusal::ClassNotAddressed {
            classes: vike_catalog::addressing_for(VENUE).classes,
            class,
            symbol: symbol.to_string(),
        });
    }
    let wants_perp = matches!(class, AssetClass::CryptoPerp | AssetClass::CryptoFuture);
    if suffixed && !wants_perp {
        return Err(BybitHistoryRefusal::SuffixContradictsClaim {
            symbol: symbol.to_string(),
            suffix: vike_venues::PERP_SUFFIX,
            class,
        });
    }
    if wants_perp {
        return Ok((perp_category(wire, listed.perp_book)?, wire));
    }
    Ok((Category::Spot, wire))
}

/// Arm 5 of [`route_target`]: the venue's derivative BOOK for a symbol a perpetual claim already
/// selected. Pure — the membership question was answered in [`crate::instrument::listing`].
fn perp_category(
    wire: &str,
    book: crate::instrument::listing::PerpListing,
) -> Result<Category, BybitHistoryRefusal> {
    use crate::instrument::listing::PerpListing;
    match book {
        PerpListing::Inverse => Ok(Category::Inverse),
        // `Unlisted` routes with `Linear`, deliberately and separately: see that variant's own doc.
        // No route is right for a symbol the venue lists nowhere, and this is the one this crate
        // has always taken, so an unknown symbol keeps failing by NAME at the venue rather than
        // acquiring a new failure mode here.
        PerpListing::Linear | PerpListing::Unlisted => Ok(Category::Linear),
        PerpListing::Both => {
            Err(BybitHistoryRefusal::BothDerivativeBooks { wire: wire.to_string() })
        }
    }
}

/// The refusal an unclaimed, ambiguous bare symbol gets. Its job is to leave the operator with an
/// ACT, so it names the two classes rather than saying "ambiguous".
///
/// ⚠ It deliberately does NOT offer the `.P` spelling as the way to reach the inverse book. See
/// [`route_target`]'s own ⚠: that spelling lands there by the venue's leniency and by `USD` rather
/// than `USDT` being the quote asset, which is precisely the implicit encoding 0061 exists to
/// remove. Recommending it would document the bug as the cure.
fn ambiguous_bare_symbol_refusal(wire: &str) -> BybitHistoryRefusal {
    BybitHistoryRefusal::AmbiguousBareSymbol { wire: wire.to_string() }
}

/// The `(category, wire_symbol)` pair a range fetch routes to — extracted out of
/// [`fetch_klines_range`] so the routing DECISION is assertable without network I/O.
///
/// Testing [`rest_category`] directly proves nothing about this: that helper has understood `perp`
/// since the live warmup seed was written, and the bug was that `fetch_klines_range` never asked
/// it. This function is what the fetcher actually calls, so a test against it fails if the fetcher
/// is ever re-pinned to `CATEGORY` — and if the fetcher stops calling it, `dead_code` trips the
/// `-D warnings` merge gate.
///
/// The impure half of [`route_target`]: it resolves [`PerpListingAnswer`] from the venue's own instrument
/// lists ([`crate::instrument::listing`]) and does so **only for the question that can change the outcome**.
/// A list that cannot be read is an ERROR rather than a fall-through: "not proven" and "proven"
/// must not reach the same code path.
///
/// The two lookups are asked independently, and each is skipped when its answer cannot matter:
///
///   * `bare_is_ambiguous` is asked only of an UNCLAIMED, UNSUFFIXED symbol at a venue
///     `vike_catalog::addressing_for` has not measured unambiguous. A claim or a `.P` answers it.
///   * `perp_book` is asked only when a PERPETUAL is what is being routed — by claim or by suffix.
///     A spot route never reaches the listings at all.
///
/// ⚠ **The second lookup is new, and it is why a `.P` symbol now opens a socket where it used to
/// route with none.** That cost is argued at [`crate::instrument::listing`]'s "What it costs"; the thing to
/// carry here is that the two lookups share ONE cached derivation, so a process that does either
/// pays once. `listings` is the `live` `data` `rest` base those keyless lists are read at.
///
/// ⚠ **A process that keeps the venue's list routes by the name's KEPT row instead** (decision 0290: the
/// gatewayhub's feeds and the tradehub, `vike_catalog::kept_list`): `symbol` is a user name, its book and the
/// venue's name of it are the row's ([`crate::instrument::class::venue_symbol`]), and a name the list carries no row
/// of is refused in words, never routed by its shape. `BTCUSDT.P` is the linear book whatever the bare `BTCUSDT`
/// is, `BTCUSDC.P` asks for `BTCPERP` and `ETHUSDT_261106` for `ETHUSDT-06NOV26`. A claim the row contradicts is
/// refused like a claim the suffix contradicts. Only a process that keeps no list (the datahub's backfill, a tool)
/// reads the venue's listings as below.
fn range_target(
    listings: &str,
    symbol: &str,
    claimed: Option<vike_model::AssetClass>,
) -> Result<(Category, String), CarriedRefusal> {
    use vike_model::AssetClass;
    if vike_catalog::kept_list(VENUE).is_some() {
        let (wire, book) = crate::instrument::class::venue_symbol(symbol)?;
        if let Some(class) = claimed {
            let derivative = matches!(class, AssetClass::CryptoPerp | AssetClass::CryptoFuture);
            if derivative == (book == Category::Spot) {
                return Err(BybitHistoryRefusal::KeptRowContradictsClaim {
                    symbol: symbol.to_string(),
                    book: book.wire(),
                    wire,
                    class,
                }
                .carried());
            }
        }
        return Ok((book, wire));
    }
    let (wire, suffixed) = vike_catalog::split_perp_at(VENUE, symbol);
    let wants_perp =
        suffixed || matches!(claimed, Some(AssetClass::CryptoPerp | AssetClass::CryptoFuture));
    let needs_ambiguity_proof =
        claimed.is_none() && !suffixed && vike_catalog::addressing_for(VENUE).must_claim();
    let listed = PerpListingAnswer {
        bare_is_ambiguous: if needs_ambiguity_proof {
            crate::instrument::listing::bare_symbol_is_ambiguous(listings, wire)?
        } else {
            false
        },
        perp_book: if wants_perp {
            crate::instrument::listing::perp_book_for(listings, wire)?
        } else {
            crate::instrument::listing::PerpListing::Unlisted
        },
    };
    route_target(symbol, claimed, listed)
        .map(|(book, wire)| (book, wire.to_string()))
        .map_err(|refusal| refusal.carried())
}

/// **The LIVE feed's route** — `(wire_symbol, category)` for a symbol arriving at a subscription,
/// or the same refusal the history path gives.
///
/// This is [`range_target`] under a name the feed can call, and the whole point is that it IS
/// [`range_target`]: `crates/bridges/bybit/src/feed/mod.rs` used to read `.P` as "linear" through
/// its own `perp_split`, so one venue crate read one string two ways — the contradiction
/// `docs/decisions/0061-an-instrument-names-its-kind.md` phase 1 PINNED and phase 4 removes. A
/// symbol the store refuses to write is now a symbol the socket refuses to stream, by construction
/// rather than by two functions agreeing.
///
/// The live path passes NO class: a subscription carries a symbol string and nothing else today, so
/// a bare ambiguous symbol is refused here exactly as it is for history, and a `.P` spelling is how
/// a perpetual is named. (When 0061's later phases carry a class to the subscription seam, this
/// grows the parameter `fetch_klines_range_classed` already has.)
///
/// # Errors
///
/// The collision refusal, the both-books refusal, or the venue's own error when the instrument
/// listings (read at `listings`, the `live` `data` `rest` base) cannot be read. Network I/O on the
/// first call per process only. A process that keeps the venue's list answers from the name's kept row
/// ([`range_target`]'s ⚠), with no network.
pub fn live_route(listings: &str, symbol: &str) -> Result<(String, Category), String> {
    let (book, wire) = range_target(listings, symbol, None).map_err(|why| why.to_string())?;
    Ok((wire, book))
}

/// Test-only reach into the pure routing decision from a SIBLING module.
///
/// It exists for ONE caller — `crates/bridges/bybit/src/catalog.rs`'s
/// `an_inverse_row_reaches_the_picker_only_if_it_routes` — and the reason it is a shim rather than
/// that test re-deriving the answer is the whole point of that test: it must feed a minted
/// instrument to the REAL decision, so that widening the catalog while this module still answers
/// "linear" reddens. A copy of the routing logic in a test crate would pass in exactly that state.
#[cfg(test)]
pub(crate) fn route_for_test(
    symbol: &str,
    perp_book: crate::instrument::listing::PerpListing,
) -> Result<(&str, Category), BybitHistoryRefusal> {
    let (book, wire) =
        route_target(symbol, None, PerpListingAnswer { bare_is_ambiguous: false, perp_book })?;
    Ok((wire, book))
}

/// Test-only: [`live_route`]'s answer over INJECTED listings, so the live feed's tests reach the real
/// refusal of an unclaimed ambiguous bare symbol without reading the venue's.
#[cfg(test)]
pub(crate) fn live_route_listed_for_test(
    symbol: &str,
    listed: PerpListingAnswer,
) -> Result<(&str, Category), BybitHistoryRefusal> {
    let (book, wire) = route_target(symbol, None, listed)?;
    Ok((wire, book))
}

/// Fetch closed-kline history for the inclusive `[start_ms, end_ms]` window, walking `end` BACKWARD
/// through Bybit's end-anchored, silently-truncating 1000-rows/response cap (module doc, point 4).
/// Returns bars ascending by startTime, de-duplicated.
///
/// A `symbol` carrying [`vike_venues::PERP_SUFFIX`] routes to `category=linear` and is stripped
/// before it reaches the wire; the CALLER's suffixed symbol remains the store/series key, so a
/// perp's series can never collide with its spot twin's. Network I/O. Errors on an unsupported
/// `interval`.
///
/// [`fetch_klines_range_paced_classed`] with nothing seeded, no class claimed and the measurement
/// dropped, so this 4-arg shape — the one every backfill seam binds — issues the identical requests
/// on the identical sleeps.
///
/// ⚠ **This is the NO-CLAIM entry point, so it is the one that can now REFUSE.** A bare symbol the
/// venue lists under both spot and inverse comes back as an `Err` rather than a spot tape — see
/// [`route_target`]. Callers that KNOW the class should bind [`fetch_klines_range_classed`]
/// instead; a claim is never refused for ambiguity.
pub fn fetch_klines_range(
    src: &KlineSourceBybit,
    symbol: &str,
    interval: &str,
    start_ms: i64,
    end_ms: i64,
) -> Result<Vec<Bar>, CarriedRefusal> {
    fetch_klines_range_classed(src, symbol, interval, start_ms, end_ms, None)
}

/// This venue's row in the kline registry (`KLINE_SOURCES`): the store-free fetch half of a
/// backfill. The ingest it feeds is `crates/vike-backfill/src/kline_source.rs`'s
/// `backfill_kline_source`.
///
/// ⚠ Bybit's `/v5/market/kline` is END-ANCHORED and pages BACKWARD (module doc, point 4): a window
/// wider than 1000 intervals is normal here and is NOT silently clipped, because
/// [`fetch_klines_range`] already walks `end` down rather than trusting the venue's `start` clamp.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KlineSourceBybit {
    /// The kline endpoint's base: the `live` `history` `rest` row.
    history: String,
    /// The keyless instrument listings the route reads ([`range_target`]): the `live` `data` `rest` row.
    listings: String,
}

impl KlineSourceBybit {
    /// The source over the venue's rows, its two bases resolved ONCE here.
    ///
    /// # Errors
    /// The refusal naming the missing row.
    pub fn from_urls(urls: &VenueUrls) -> Result<Self, UrlRefusal> {
        Ok(KlineSourceBybit {
            history: urls
                .url(UrlType::Live, UrlChannel::History, UrlProtocol::Rest, "")?
                .to_string(),
            listings: urls.url(UrlType::Live, UrlChannel::Data, UrlProtocol::Rest, "")?.to_string(),
        })
    }
}

impl vike_data::source::KlineSource for KlineSourceBybit {
    fn venue(&self) -> &str {
        VENUE
    }

    /// The bridge's own paged `fetch_klines_range` verbatim, after an interval [`interval_code`] has
    /// no code for is refused (`Refused`: the venue is not asked).
    fn fetch(
        &self,
        symbol: &str,
        interval: &str,
        start_ms: i64,
        end_ms: i64,
    ) -> Result<Vec<Bar>, vike_data::source::SourceError> {
        interval_code(interval)
            .map_err(|refusal| vike_data::source::SourceError::Refused(refusal.carried()))?;
        fetch_klines_range(self, symbol, interval, start_ms, end_ms)
            .map_err(vike_data::source::SourceError::Fetch)
    }
}

/// [`fetch_klines_range`] with the caller's asset-class CLAIM — the seam
/// `docs/decisions/0061-an-instrument-names-its-kind.md` verdict 3 specifies, spelled as an
/// `Option<AssetClass>` because most callers genuinely have no class to give.
///
/// `Some(class)` routes on the claim and is never refused for ambiguity; `None` is exactly
/// [`fetch_klines_range`] and can be. This is the entry point 0061's later phases bind as the class
/// reaches the bridges — the chart's window tag through `StoredBarRequest`, the seed request's
/// optional field, and the backfill CLI's own argument.
pub fn fetch_klines_range_classed(
    src: &KlineSourceBybit,
    symbol: &str,
    interval: &str,
    start_ms: i64,
    end_ms: i64,
    class: Option<vike_model::AssetClass>,
) -> Result<Vec<Bar>, CarriedRefusal> {
    fetch_klines_range_paced_classed(src, symbol, interval, start_ms, end_ms, None, class)
        .map(|(bars, _pace)| bars)
}

/// [`fetch_klines_range`] with the run's PACE measurement carried in and out: `seed` is a previous
/// run's observation (or `None`, which is byte-identical), and the second element of the return is
/// THIS run's — `None` unless a page was actually timed, so a run that fetched nothing cannot report
/// a fabricated pace.
///
/// ⚠ Neither direction moves a single sleep. A seed on a `Fixed` pacer buys exactly one thing: an
/// ETA from page ZERO instead of page one (`Pacer::next_delay` still answers [`PAGE_DELAY`] — see
/// this module's "The pager MEASURES its pages"). The reported sample's `per_request_weight` is
/// likewise inert here: Bybit publishes no weight counter, so the field carries the pacer's seed
/// constant and steers nothing — `request_ms` is the number an operator wants.
///
/// The CALLER owns any persistence; nothing in a bridge crate reads or writes operator state.
///
/// It also carries the caller's asset-class CLAIM (see [`fetch_klines_range_classed`]) — the
/// full-fidelity entry point the other shapes delegate to, so the routing decision and the pacing
/// policy exist exactly once.
pub fn fetch_klines_range_paced_classed(
    src: &KlineSourceBybit,
    symbol: &str,
    interval: &str,
    start_ms: i64,
    end_ms: i64,
    seed: Option<&PaceSample>,
    class: Option<vike_model::AssetClass>,
) -> Result<(Vec<Bar>, Option<PaceSample>), CarriedRefusal> {
    let (book, wire) = range_target(&src.listings, symbol, class)?;
    let category = book.wire();
    let code = interval_code(interval).map_err(|refusal| refusal.carried())?;
    let agent = vike_transport::http::blocking_agent();
    let pacer = kline_page_pacer(VENUE, PAGE_DELAY, seed);
    // `start_ms` is pinned on every page: the venue clamps it, and undoing that clamp is the whole point of moving
    // `page_end`. The caller's suffixed `symbol` names the pace line; the venue's `wire` name is what is asked.
    Ok(kline_page_walk_backward(VENUE, symbol, interval, start_ms, end_ms, pacer, |page_end| {
        fetch_page(&agent, &src.history, category, &wire, code, start_ms, page_end)
    })?)
}

// --- funding-rate source -------------------------------------------------------------------------

/// Bybit V5 funding-rate history (keyless public GET) on the `live` `data` `rest` base: one endpoint
/// for the linear and the inverse book, `category=` naming which.
const PATH_FUNDING_HISTORY: &str = "/v5/market/funding/history";
/// Rows per funding page. MEASURED 2026-11-08: `limit=500` answers 200 rows, so 200 is the cap.
const FUNDING_LIMIT: usize = 200;

/// Pull ascending-by-ts funding points out of a parsed `/v5/market/funding/history` envelope:
/// `retCode == 0` or an `Err`, then `result.list` (newest first) of `{symbol, fundingRate (decimal
/// string), fundingRateTimestamp (epoch ms as a STRING)}`, reversed. A row missing either field or
/// failing to parse is skipped, as `vike_data::source::parse_funding_rate_rows` skips one. Bybit
/// publishes no premium and no mark here, so both are `None`.
fn funding_from_envelope(
    v: &serde_json::Value,
) -> Result<Vec<vike_marketdata::FundingRatePoint>, String> {
    let ret_code = v.get("retCode").and_then(serde_json::Value::as_i64);
    if ret_code != Some(0) {
        let msg = v.get("retMsg").and_then(serde_json::Value::as_str).unwrap_or("").to_string();
        return Err(BybitHistoryRefusal::FundingRetCode { code: ret_code, msg }.to_string());
    }
    let list = v
        .get("result")
        .and_then(|r| r.get("list"))
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| BybitHistoryRefusal::FundingNoList.to_string())?;
    let mut points: Vec<vike_marketdata::FundingRatePoint> = list
        .iter()
        .filter_map(|row| {
            let ts = row.get("fundingRateTimestamp")?;
            let ts_ms = ts.as_str().and_then(|s| s.parse::<i64>().ok()).or_else(|| ts.as_i64())?;
            let rate = row.get("fundingRate")?.as_str()?.parse::<f64>().ok()?;
            Some(vike_marketdata::FundingRatePoint {
                timestamp_ms: ts_ms,
                rate,
                premium: None,
                mark: None,
            })
        })
        .collect();
    points.reverse();
    Ok(points)
}

/// Pure map: a `/v5/market/funding/history` body → funding points ascending by ts. No network — the
/// fixture-tested seam.
pub fn parse_bybit_funding(body: &str) -> Result<Vec<vike_marketdata::FundingRatePoint>, String> {
    funding_from_envelope(&json_body(body, "bybit funding")?)
}

/// One `/v5/market/funding/history` page: [`KLINES_PAGE`]'s rules under the funding words.
const FUNDING_PAGE: JsonPageSpec =
    JsonPageSpec { label: "bybit funding", json_what: "bybit funding", ..KLINES_PAGE };

/// One funding page ending at `end_ms`, under [`FUNDING_PAGE`] (the kline pager's rate-limit policy).
fn fetch_funding_page(
    agent: &ureq::Agent,
    base: &str,
    category: &str,
    wire: &str,
    start_ms: i64,
    end_ms: i64,
) -> Result<Vec<vike_marketdata::FundingRatePoint>, String> {
    let url = format!(
        "{base}{PATH_FUNDING_HISTORY}?category={category}&symbol={wire}&startTime={start_ms}\
         &endTime={end_ms}&limit={FUNDING_LIMIT}"
    );
    json_page_fetch(agent, &url, &FUNDING_PAGE, funding_from_envelope)
}

/// This venue's market funding-rate source, a row of the gatewayhub's `FUNDING_SOURCES`: the
/// perpetuals of BOTH derivative books (a linear `BTCUSDT.P`, an inverse `BTCUSD.P`), each on the
/// cadence the venue settles that symbol at (8h for most, 4h or 1h for some; the history lists each
/// settlement, so the cadence is the points' spacing and a re-cadenced symbol stays right).
///
/// ⚠ **The endpoint is END-ANCHORED**, like the kline one (module doc, point 4). MEASURED 2026-11-08
/// against `category=linear&symbol=BTCUSDT`: a window of 94 settlements came back whole; a window of
/// two years came back as the NEWEST 200 rows ending at `endTime`, `startTime` ignored, `retCode: 0`.
/// So it pages BACKWARD (`vike_data::source::page_walk_backward`), and `endTime = oldest - 1`
/// is contiguous (the next page's newest row was one 8h settlement older).
///
/// The store spells a perpetual with [`vike_venues::PERP_SUFFIX`] (the kline lane files its perp
/// bars under that key, and a bare name is the SPOT pair on this data plane), so a bare symbol is
/// REFUSED before anything is asked: spot has no funding, and a funding series filed under the spot
/// spelling would sit where no perp backtest looks. A `.P` symbol takes its book from the venue's
/// listings ([`range_target`]), the kline path's own rule, so `BTCUSD.P` reads `category=inverse`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FundingRateSourceBybit {
    /// The `live` `data` `rest` base: the funding history AND the listings the route reads.
    base: String,
}

impl FundingRateSourceBybit {
    /// The source over the venue's rows, its base resolved ONCE here.
    ///
    /// # Errors
    /// The refusal naming the missing row.
    pub fn from_urls(urls: &VenueUrls) -> Result<Self, UrlRefusal> {
        Ok(FundingRateSourceBybit {
            base: urls.url(UrlType::Live, UrlChannel::Data, UrlProtocol::Rest, "")?.to_string(),
        })
    }
}

impl vike_data::source::FundingRateSource for FundingRateSourceBybit {
    fn venue(&self) -> &str {
        VENUE
    }

    fn fetch(
        &self,
        symbol: &str,
        start_ms: i64,
        end_ms: i64,
    ) -> Result<Vec<vike_marketdata::FundingRatePoint>, vike_data::source::SourceError> {
        use vike_data::source::SourceError;
        if !vike_catalog::split_perp_at(VENUE, symbol).1 {
            let suffix = vike_venues::PERP_SUFFIX;
            let refusal =
                BybitHistoryRefusal::FundingNoPerpetual { symbol: symbol.to_string(), suffix };
            return Err(SourceError::Refused(refusal.carried()));
        }
        let (book, wire) = range_target(&self.base, symbol, None).map_err(SourceError::Fetch)?;
        let agent = vike_transport::http::blocking_agent();
        let mut first = true;
        let stuck = |at: vike_data::source::PageWalkStuck| {
            SourceError::Fetch(at.text(VENUE, symbol, vike_data::source::FUNDING_INTERVAL).into())
        };
        vike_data::source::page_walk_backward(start_ms, end_ms, stuck, |page_end| {
            if !first {
                std::thread::sleep(PAGE_DELAY);
            }
            first = false;
            fetch_funding_page(&agent, &self.base, book.wire(), &wire, start_ms, page_end)
                .map_err(|why| SourceError::Fetch(why.into()))
        })
    }
}

/// **Test support, never a door**: hold a kept bybit list in this test process (`vike_catalog::hold_kept_list`), a row
/// of every book shape the tests trade, keyed and typed as `crate::catalog` keeps them (a spot pair bare, a perpetual
/// `.P`, a dated contract under the venue's own name with its delivery time; the base and quote the venue states; the
/// book in `wire.group`), so decision 0290's translator names each: `BTCUSDT` the spot pair, `BTCUSDT.P` the linear
/// perpetual, `BTCUSDC.P` the venue's `BTCPERP`, `BTCUSD.P` the inverse one, `ETHUSDT_261106` the venue's
/// `ETHUSDT-06NOV26`, `BTCUSD_261225` its `BTCUSDZ26`. A test of a path that reads a name
/// (`crate::instrument::class::venue_symbol`) calls it first; a second call changes nothing.
#[cfg(any(test, feature = "test-support"))]
pub fn kept_list_for_tests() {
    use vike_model::{
        Contract, ContractFuture, ContractFutureDelivery, ContractFutureFamily, ContractPerp,
        Instrument, InstrumentVenueIds, Margining,
    };
    let book = |category: Category| InstrumentVenueIds {
        group: Some(category.wire().to_string()),
        ..InstrumentVenueIds::default()
    };
    let settle = |margining: Margining, base: &str, quote: &str| {
        let (book, coin) = if margining == Margining::Inverse {
            (Category::Inverse, base)
        } else {
            (Category::Linear, quote)
        };
        (book, coin.to_string())
    };
    let spot = |(key, base, quote): (&str, &str, &str)| {
        Instrument::new(VENUE, key, base, quote, Contract::CryptoSpot)
            .with_wire(book(Category::Spot))
    };
    let perp = |(key, base, quote): (&str, &str, &str), margining: Margining| {
        let (category, coin) = settle(margining, base, quote);
        let contract = Contract::Perp(ContractPerp {
            margining,
            settle: coin,
            value_currency: None,
            funding_interval_ms: None,
            settling_ms: None,
        });
        Instrument::new(VENUE, format!("{key}.P"), base, quote, contract).with_wire(book(category))
    };
    // A dated contract delivers at 08:00 UTC of its day: `expiry_ms`.
    let dated = |(key, base, quote, expiry_ms): (&str, &str, &str, i64), margining: Margining| {
        let (category, coin) = settle(margining, base, quote);
        let contract = Contract::Future(ContractFuture {
            margining,
            settle: coin,
            value_currency: None,
            expiry_ms,
            tenor: None,
            delivery: ContractFutureDelivery::Unknown,
            family: ContractFutureFamily::Crypto,
        });
        Instrument::new(VENUE, key, base, quote, contract).with_wire(book(category))
    };
    let mut rows: Vec<Instrument> = [
        ("BTCUSDT", "BTC", "USDT"),
        ("ETHUSDT", "ETH", "USDT"),
        ("DOGEUSDT", "DOGE", "USDT"),
        ("SOLUSDT", "SOL", "USDT"),
        ("XRPUSDT", "XRP", "USDT"),
        ("BTCUSD", "BTC", "USD"),
        ("ETHUSD", "ETH", "USD"),
    ]
    .into_iter()
    .map(spot)
    .collect();
    rows.extend(
        [
            ("BTCUSDT", "BTC", "USDT"),
            ("ETHUSDT", "ETH", "USDT"),
            ("DOGEUSDT", "DOGE", "USDT"),
            ("SOLUSDT", "SOL", "USDT"),
            ("XRPUSDT", "XRP", "USDT"),
            ("1000PEPEUSDT", "1000PEPE", "USDT"),
            ("BTCPERP", "BTC", "USDC"),
            ("ETHPERP", "ETH", "USDC"),
        ]
        .into_iter()
        .map(|row| perp(row, Margining::Linear)),
    );
    rows.extend(
        [
            ("BTCUSD", "BTC", "USD"),
            ("ETHUSD", "ETH", "USD"),
            ("XRPUSD", "XRP", "USD"),
            ("ADAUSD", "ADA", "USD"),
            ("HYPEUSD", "HYPE", "USD"),
            ("SOLUSD", "SOL", "USD"),
            ("DOTUSD", "DOT", "USD"),
        ]
        .into_iter()
        .map(|row| perp(row, Margining::Inverse)),
    );
    rows.extend(
        [
            ("ETHUSDT-06NOV26", "ETH", "USDT", 1_793_952_000_000),
            ("ETHUSDT-27NOV26", "ETH", "USDT", 1_795_766_400_000),
            ("BTCUSDT-27NOV26", "BTC", "USDT", 1_795_766_400_000),
            ("BTC-27DEC26", "BTC", "USDC", 1_798_358_400_000),
        ]
        .into_iter()
        .map(|row| dated(row, Margining::Linear)),
    );
    rows.extend(
        [
            ("BTCUSDZ26", "BTC", "USD", 1_798_185_600_000),
            ("ETHUSDZ26", "ETH", "USD", 1_798_185_600_000),
            ("BTCUSDH26", "BTC", "USD", 1_774_598_400_000),
            ("BTCUSDH27", "BTC", "USD", 1_806_048_000_000),
            ("ETHUSDH27", "ETH", "USD", 1_806_048_000_000),
        ]
        .into_iter()
        .map(|row| dated(row, Margining::Inverse)),
    );
    vike_catalog::hold_kept_list(VENUE, rows);
}

#[path = "history_tests.rs"]
#[cfg(test)]
mod history_tests;
