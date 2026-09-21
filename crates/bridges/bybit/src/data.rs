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
//!      The pure seams are `pub` so the truncation trap is gated WITHOUT network I/O:
//!      [`next_page_step`] is the "given this page's ticks, what is the next `end`" decision, and
//!      [`walk_backward_pages`] drives the whole walk over an INJECTED page fetcher — a test that
//!      only exercises a single page cannot see this bug at all.
//!
//! Endpoint: `GET https://api.bybit.com/v5/market/kline?category=&symbol=&interval=&start=&end=&limit=1000`
//! — `category` is one of [`Category`]'s three values, chosen by [`route_target`] (Bybit V5 unifies
//! spot/linear/inverse under ONE REST base, unlike Binance's separate fapi host).
//! Only `fetch_klines_range` does network I/O; `parse_bybit_klines` is a pure, fixture-tested map.
//!
//! ## ⚠ 5. A bare symbol can name TWO Trading books here, and an unclaimed one is now REFUSED
//!
//! Phase 0 of `docs/decisions/0061-an-instrument-names-its-kind.md`. Bybit lists `BTCUSD` as a
//! Trading SPOT pair **and** a Trading inverse perpetual; the spot side is vestigial (four of six
//! bars zero-volume against a price ~$70 stale) and the bare→spot rule sent every caller there in
//! silence. [`route_target`] is the whole decision and [`ambiguous_bare_symbol_refusal`] the answer
//! it gives; the predicate, the two counts that sized it and its residual live in
//! [`crate::instruments`].
//!
//! Three properties worth carrying before you edit anything here:
//!
//!   * **Only the caller who named NOTHING is refused.** `Some(class)` routes on the claim and is
//!     never refused for ambiguity — that is what [`fetch_klines_range_classed`] is for.
//!   * **`.P` says PERPETUAL, not inverse — and that is now true of the CODE, not only the prose.**
//!     This bullet used to warn that `BTCUSD.P` reached the inverse book by two accidents: a `USD`
//!     rather than `USDT` quote asset, and bybit resolving a `category=linear` request leniently.
//!     **Neither is in the routing path any more.** A perpetual claim names the PRODUCT and
//!     [`crate::instruments::perp_book`] asks the VENUE which of its two derivative books carries
//!     it, so a `.P` symbol reaches `category=inverse` because the venue LISTS it there, never
//!     because of how it is spelled. Unchanged: the suffix is still not a way to SELECT inverse, and
//!     [`ambiguous_bare_symbol_refusal`] still refuses to recommend it.
//!   * **The instrument's own words are carried**: `crates/bridges/bybit/src/catalog.rs`'s
//!     `parse_perp`/`parse_inverse` put the venue's `contractType`/`settleCoin` onto
//!     `vike_catalog::Instrument`'s `contract_type`/`settle_asset`. ⚠ Nothing in this module reads
//!     them, and that stays deliberate: 0061 names a ROUTER reading `settle_asset` as the trigger
//!     that would split the perp variant. The route here is a listing MEMBERSHIP
//!     (`crates/bridges/bybit/src/instruments.rs`) — a venue-local fact, not a second reading of the
//!     core vocabulary — so those two fields stay IDENTIFICATION, for rendering and auditing.
//!
//! ## ⚠ 6. Three categories, and the third one is why `rest_category` stopped taking a `bool`
//!
//! Phase 4 of the same record. [`Category`] is the three books, [`Listed`] is what the venue's own
//! instrument listings say about one symbol, and [`route_target`] is still the whole decision — five
//! arms now instead of four. The point-5 refusal is untouched: a caller who names NOTHING on an
//! ambiguous bare symbol gets the same message byte for byte. What changed is that a caller who
//! names `CryptoPerp` — or picks the perp row out of the catalog — is now SERVED rather than sent to
//! `category=linear` and rescued by the venue's leniency.
//!
//! ⚠ **Producing `inverse` is not what made the inverse TAPE reachable**, and this module would be
//! overclaiming if it said so: MEASURED 2026-09-16, the kline endpoint resolves `category`
//! leniently in both directions and served the right rows either way. What the third value buys is
//! a request that says what it means, and a decision that [`crate::market_feed`] can share — the WS
//! is STRICT per host, and there the same choice is the difference between a live chart and a
//! reconnect loop.
//!
//! ## Rate-limit strategy (paged `fetch_klines_range`)
//! Bybit's public REST is IP-limited (~600 req / 5 s per IP — generous), so the pager stays well
//! under it: a fixed [`PAGE_DELAY`] throttle between pages, and on a rate-limit signal (retCode
//! 10006/10018 or HTTP 429/403) an exponential backoff that HONORS `Retry-After`, retrying the same
//! page up to [`MAX_RATE_LIMIT_RETRIES`]. Pages BACKWARD (see point 4 above): `limit=1000` with
//! `start` pinned to the real window start, moving `end` one ms below each page's oldest kline.
//!
//! ## The pager MEASURES its pages (and still sleeps [`PAGE_DELAY`])
//! Bybit publishes no request-weight budget, so there is nothing to discover and nothing to pace a
//! fraction of — but that never stopped us timing our own round trips, and until now nobody had. The
//! sibling venue was MEASURED on the CI box 2026-08-04 (okx: 476–486 ms per page against a hardcoded
//! 200 ms delay) and this venue is the same shape: the constant is not even the dominant term of the
//! gap the venue actually sees.
//!
//! So the sleep now runs through [`vike_bridge_core::pacer::Pacer::fallback`] and every page is
//! wall-clocked into [`vike_bridge_core::pacer::Pacer::observe_request`]. Two things follow, and a
//! third deliberately does not:
//!   * the backfill prints ONE `info` ETA line after its first page, instead of going silent for
//!     minutes in a way indistinguishable from a hang;
//!   * [`fetch_klines_range_paced`] hands the observation back so `vike_backfill::pace_book` can
//!     persist it and an operator can SEE what a page costs here;
//!   * ⚠ **the delay does not change.** `Pacer::fallback::next_delay` returns exactly [`PAGE_DELAY`]
//!     whatever is observed (pinned by that module's tests and by
//!     `the_pager_measures_but_never_changes_the_page_delay` below). With no published ceiling, a
//!     delay derived from a stopwatch alone would be the same un-checkable hardcoded guess in newer
//!     clothes. Acting on the measurement is a separate change with its own measurement.

use std::time::Duration;

use vike_bridge_core::http::{body_head, get_raw};
use vike_bridge_core::klines::kline_to_bar;
use vike_bridge_core::pacer::{Pacer, remaining_pages};
use vike_bridge_core::retry::{BackoffPolicy, Verdict, retry_rate_limited};
use vike_model::Bar;
use vike_model::rate_limits::PaceSample;

/// This crate's canonical venue id — the key `vike_catalog::addressing_for` is looked up by.
///
/// ⚠ Spelled as a literal rather than read from [`crate::perp::VENUE`] deliberately: that const
/// lives behind the default-on `exec` feature, and this module is on the FEEDS side of the seam
/// (`scripts/ci_feature_suite.sh`'s `bridges-feeds` arm compiles it with `--no-default-features`),
/// so reaching for it would make a keyless market-data build fail to compile. `catalog.rs`'s
/// `CatalogProvider::venue` spells it the same way for the same reason.
/// `the_venue_id_is_the_one_the_catalog_answers_with` pins the two equal.
const VENUE: &str = "bybit";

const REST_KLINES: &str = "https://api.bybit.com/v5/market/kline";
/// Bybit spot category — where an unclaimed, unambiguous bare symbol routes. The two derivative
/// spellings live in [`rest_category`] beside it; this one keeps its own const because the URL
/// tests below compare against it by name.
const CATEGORY: &str = "spot";
/// Bybit's hard cap on klines returned by a single request.
const MAX_LIMIT: usize = 1000;

// ---- paged-backfill rate-limit knobs (see the module rate-limit strategy) ----------------------
/// Fixed throttle between successive page requests (keeps us far under ~600 req/5 s per IP).
///
/// The VALUE comes from `vike_model::venue_rate_limits`'s `BYBIT` row rather than from a literal
/// here: Bybit runs no request-weight meter and publishes no machine-readable rate-limit metadata,
/// so this delay IS the whole pacing story for history on this venue — which makes it a per-venue
/// fact worth declaring beside the other venues' rather than a local constant nothing can compare.
/// The table models that absence as `History::Unweighted` (no ceiling, no soft limit) instead of
/// inventing a number; the ~600 req/5 s figure above is documented prose, never a payload.
const PAGE_DELAY: Duration = vike_model::venue_rate_limits::BYBIT.history.page_delay();
/// Bounded retries for a single page that keeps getting rate-limited.
const MAX_RATE_LIMIT_RETRIES: u32 = 6;
/// First backoff when the server sends no `Retry-After` (doubles each retry, capped).
const INITIAL_BACKOFF: Duration = Duration::from_secs(1);
/// Ceiling on any single backoff / `Retry-After` sleep.
const MAX_BACKOFF: Duration = Duration::from_secs(60);
/// Bybit rate-limit business codes (arrive with HTTP 200): "too many visits" / "IP rate limit".
const RATE_LIMIT_CODES: [i64; 2] = [10006, 10018];

/// Map a binance-style interval ("1m","1h","1d",…) to Bybit's V5 interval code ("1","60","D",…).
/// Errors on an unsupported interval rather than guessing. Note "1M" (month) vs "1m" (minute) is
/// case-sensitive, matching the binance-style input convention. `pub(crate)` so the live kline feed
/// ([`crate::market_feed`]) builds the `kline.<code>.<SYMBOL>` WS topic from the same map.
pub(crate) fn interval_code(interval: &str) -> Result<&'static str, String> {
    Ok(match interval {
        "1m" => "1",
        "3m" => "3",
        "5m" => "5",
        "15m" => "15",
        "30m" => "30",
        "1h" => "60",
        "2h" => "120",
        "4h" => "240",
        "6h" => "360",
        "12h" => "720",
        "1d" => "D",
        "1w" => "W",
        "1M" => "M",
        other => return Err(format!("bybit: unsupported interval {other:?}")),
    })
}

/// Map one Bybit kline row (`[startTime, open, high, low, close, volume, turnover]`, all decimal
/// strings) → a [`Bar`]. Preserves each string's exact f64 bit pattern via `<f64 as FromStr>` — no
/// rounding, no arithmetic. Volume defaults to 0.0 on a parse miss (mirrors the binance mapper).
fn row_to_bar(r: &[serde_json::Value]) -> Result<Bar, String> {
    let s = |i: usize, name: &str| -> Result<&str, String> {
        r.get(i)
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| format!("bybit kline {name} not str"))
    };
    let t = s(0, "startTime")?
        .parse::<i64>()
        .map_err(|e| format!("bybit kline startTime parse: {e}"))?;
    let f = |i: usize, name: &str| -> Result<f64, String> {
        s(i, name)?.parse::<f64>().map_err(|e| format!("bybit kline {name} parse: {e}"))
    };
    let o = f(1, "open")?;
    let h = f(2, "high")?;
    let l = f(3, "low")?;
    let c = f(4, "close")?;
    let v = r
        .get(5)
        .and_then(serde_json::Value::as_str)
        .and_then(|s| s.parse::<f64>().ok())
        .unwrap_or(0.0);
    Ok(kline_to_bar(t, o, h, l, c, v))
}

/// Pull ascending-by-startTime [`Bar`]s out of a parsed V5 kline envelope: require `retCode == 0`,
/// map `result.list` (newest-first) and reverse. Shared by the pure map and the pager.
fn bars_from_envelope(v: &serde_json::Value) -> Result<Vec<Bar>, String> {
    let ret_code = v.get("retCode").and_then(serde_json::Value::as_i64);
    if ret_code != Some(0) {
        let msg = v.get("retMsg").and_then(serde_json::Value::as_str).unwrap_or("");
        return Err(format!("bybit klines retCode {ret_code:?}: {msg}"));
    }
    let list = v
        .get("result")
        .and_then(|r| r.get("list"))
        .and_then(serde_json::Value::as_array)
        .ok_or("bybit klines: missing result.list")?;
    let mut bars = list
        .iter()
        .map(|row| {
            row.as_array()
                .ok_or_else(|| "bybit kline row not array".to_string())
                .and_then(|r| row_to_bar(r))
        })
        .collect::<Result<Vec<Bar>, String>>()?;
    bars.reverse(); // Bybit serves newest-first; the store wants ascending by startTime
    Ok(bars)
}

/// Pure map: a Bybit V5 klines response body → `Vec<Bar>` ascending by startTime. No network — this
/// is the fixture-tested seam.
pub fn parse_bybit_klines(body: &str) -> Result<Vec<Bar>, String> {
    let v: serde_json::Value =
        serde_json::from_str(body).map_err(|e| format!("bybit kline json: {e}"))?;
    bars_from_envelope(&v)
}

/// Build the `GET /v5/market/kline` URL for the given bounds and limit. `category` is `"spot"` or
/// `"linear"` — a PARAMETER, not the const, so a perp backfill and the spot one cannot drift.
fn klines_url(
    category: &str,
    symbol: &str,
    interval_code: &str,
    start_ms: i64,
    end_ms: i64,
    limit: usize,
) -> String {
    format!(
        "{REST_KLINES}?category={category}&symbol={symbol}&interval={interval_code}&start={start_ms}&end={end_ms}&limit={limit}"
    )
}

/// Fetch ONE page with the rate-limit policy: on HTTP 429/418/403 or retCode 10006/10018, honor
/// `Retry-After` (else exponential backoff) and retry the same page up to
/// [`MAX_RATE_LIMIT_RETRIES`]. Returns the page's bars ascending by startTime.
fn fetch_page_rate_limited(
    agent: &ureq::Agent,
    category: &str,
    symbol: &str,
    interval_code: &str,
    start_ms: i64,
    end_ms: i64,
) -> Result<Vec<Bar>, String> {
    let url = klines_url(category, symbol, interval_code, start_ms, end_ms, MAX_LIMIT);
    let policy = BackoffPolicy {
        max_retries: MAX_RATE_LIMIT_RETRIES,
        initial: INITIAL_BACKOFF,
        max: MAX_BACKOFF,
    };
    // The backoff cadence is the shared driver; the classification (which statuses/retCodes mean
    // rate-limited) stays Bybit-specific here.
    retry_rate_limited(policy, "bybit klines", || {
        let raw = get_raw(agent, &url, "bybit klines", &[])?;
        // HTTP-level rate limit / IP block (Bybit fronts REST with Cloudflare).
        if matches!(raw.status, 429 | 418 | 403) {
            return Ok(Verdict::RateLimited {
                retry_after: raw.retry_after,
                note: format!("HTTP {} (rate limited)", raw.status),
            });
        }
        if !(200..300).contains(&raw.status) {
            return Err(format!("bybit klines HTTP {}: {}", raw.status, body_head(&raw.body)));
        }
        let v: serde_json::Value =
            serde_json::from_str(&raw.body).map_err(|e| format!("bybit kline json: {e}"))?;
        // Business-level rate limit (Bybit returns HTTP 200 for these).
        let code = v.get("retCode").and_then(serde_json::Value::as_i64).unwrap_or(-1);
        if RATE_LIMIT_CODES.contains(&code) {
            return Ok(Verdict::RateLimited {
                retry_after: raw.retry_after,
                note: format!("retCode {code} (rate limited)"),
            });
        }
        bars_from_envelope(&v).map(Verdict::Done)
    })
}

/// **The three books bybit's V5 REST serves, as this crate names them** — the return of
/// [`route_target`] and the input to every URL built here.
///
/// It is venue-LOCAL vocabulary on purpose. `vike_catalog`'s `addressing` module doc forbids
/// `linear`/`inverse` appearing in the core crate at all (*"the vocabulary names the PRODUCT, never
/// the venue's word for the route"*), and `docs/decisions/0061-an-instrument-names-its-kind.md`
/// verdict 1 refuses to split `AssetClass`'s perp variant to carry the difference. So the
/// translation from a PRODUCT claim to a ROUTE happens exactly here, one venue wide.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
/// `crates/bridges/bybit/src/market_feed.rs`'s `ws_host`, where the same decision picks between
/// three STRICT hosts and picking wrong is a `handler not found` or a silently empty book).
fn rest_category(book: Category) -> &'static str {
    match book {
        Category::Spot => CATEGORY,
        Category::Linear => "linear",
        Category::Inverse => "inverse",
    }
}

/// Build the `GET /v5/market/kline` URL for the live feed's latest-N warmup (no start/end bounds).
/// `category` is `"spot"` or `"linear"` — pulled out so the URL-building test below and the real
/// fetch can never drift apart.
fn latest_url(category: &str, symbol: &str, interval_code: &str, limit: usize) -> String {
    format!(
        "{REST_KLINES}?category={category}&symbol={symbol}&interval={interval_code}&limit={limit}"
    )
}

/// The live feed's warmup seed: the newest `limit` klines (no time bound), the last of which is the
/// still-forming candle — Bybit serves the in-progress candle as the newest `result.list` row, so
/// after [`parse_bybit_klines`] reverses to ascending it is the last bar. A single un-throttled
/// request — the Bybit twin of binance's `fetch_klines_latest`. `book` names the category (the same
/// endpoint host, just a different query category — Bybit V5 unifies spot/linear/inverse under one
/// REST base, unlike Binance's separate fapi host); the response shape is identical for all three,
/// so `parse_bybit_klines` is unchanged. Network I/O. Errors on an unsupported `interval`.
///
/// ⚠ It takes the RESOLVED [`Category`] rather than a symbol, because its caller
/// (`crates/bridges/bybit/src/market_feed.rs`'s `feed_main`) has already resolved one to pick its WS
/// host, and a seed that re-derived the route could disagree with the socket it is seeding. The
/// `bool` this parameter used to be is exactly the disagreement that made a `.P` inverse symbol seed
/// from one book and stream from another.
pub fn fetch_klines_latest(
    symbol: &str,
    interval: &str,
    limit: usize,
    book: Category,
) -> Result<Vec<Bar>, String> {
    let code = interval_code(interval)?;
    let agent = vike_bridge_core::http::blocking_agent();
    let url = latest_url(book.wire(), symbol, code, limit);
    let raw = get_raw(&agent, &url, "bybit klines", &[])?;
    if !(200..300).contains(&raw.status) {
        return Err(format!("bybit klines HTTP {}: {}", raw.status, body_head(&raw.body)));
    }
    parse_bybit_klines(&raw.body)
}

/// **What the venue's own instrument listings say about ONE symbol** — the injected half of
/// [`route_target`], so the whole routing decision stays pure and assertable without a socket.
///
/// Both fields are FACTS READ OFF THE VENUE, never inferences from the symbol's shape.
/// [`crate::instruments`] derives them (once per process) and [`range_target`] is where the two
/// halves meet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Listed {
    /// Does the venue list this bare symbol Trading under BOTH spot and inverse? The collision that
    /// refuses an unclaimed caller — `crates/bridges/bybit/src/instruments.rs` carries the
    /// predicate, the counts that sized it and its residual.
    pub bare_is_ambiguous: bool,
    /// Which derivative book a PERPETUAL claim on this symbol names.
    pub perp_book: crate::instruments::PerpBook,
}

impl Listed {
    /// The answer for a symbol whose listings were never consulted — spot-unambiguous, and a
    /// perpetual claim routing to linear. It is exactly this crate's behaviour before the listings
    /// were read, so a test that means "nothing was looked up" says so in one word.
    #[must_use]
    pub fn unconsulted() -> Self {
        Self { bare_is_ambiguous: false, perp_book: crate::instruments::PerpBook::Unlisted }
    }
}

/// **The whole routing decision, PURE** — `(category, wire_symbol)` or a refusal, given the
/// caller's symbol, the class the caller CLAIMED (if any), and what the venue's own instrument
/// listings say about that symbol.
///
/// `docs/decisions/0061-an-instrument-names-its-kind.md` verdict 3: *"the seam takes
/// `Option<AssetClass>`, and a missing claim on an AMBIGUOUS venue symbol is a REFUSAL … the value
/// that was missing is not a third product, it is the absence of a claim."* Every arm of that is
/// here, and [`Listed`] is INJECTED so the whole decision is assertable without network I/O — which
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
///    `crate::instruments::PerpBook::Both`).
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
    listed: Listed,
) -> Result<(Category, &str), String> {
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
        return Err(format!(
            "bybit: this venue's kline path addresses {:?}, and {class:?} is not one of them \
             (symbol {symbol:?})",
            vike_catalog::addressing_for(VENUE).classes
        ));
    }
    let wants_perp = matches!(class, AssetClass::CryptoPerp | AssetClass::CryptoFuture);
    if suffixed && !wants_perp {
        return Err(format!(
            "bybit: {symbol:?} carries the {:?} perpetual marker but the caller claimed \
             {class:?} — two claims that disagree, so neither is obeyed. Drop the suffix or drop \
             the claim.",
            vike_catalog::PERP_SUFFIX
        ));
    }
    if wants_perp {
        return Ok((perp_category(wire, listed.perp_book)?, wire));
    }
    Ok((Category::Spot, wire))
}

/// Arm 5 of [`route_target`]: the venue's derivative BOOK for a symbol a perpetual claim already
/// selected. Pure — the membership question was answered in [`crate::instruments`].
fn perp_category(wire: &str, book: crate::instruments::PerpBook) -> Result<Category, String> {
    use crate::instruments::PerpBook;
    match book {
        PerpBook::Inverse => Ok(Category::Inverse),
        // `Unlisted` routes with `Linear`, deliberately and separately: see that variant's own doc.
        // No route is right for a symbol the venue lists nowhere, and this is the one this crate
        // has always taken, so an unknown symbol keeps failing by NAME at the venue rather than
        // acquiring a new failure mode here.
        PerpBook::Linear | PerpBook::Unlisted => Ok(Category::Linear),
        PerpBook::Both => Err(format!(
            "bybit lists {wire:?} Trading under BOTH category=linear AND category=inverse, so a \
             perpetual claim on it names two books and neither is being guessed. This cannot \
             happen on the venue as MEASURED 2026-09-16 (`linear` and `inverse` share no symbol); \
             it firing means the listings changed, and `crates/bridges/bybit/src/instruments.rs`'s \
             route predicate needs a fact this call does not have."
        )),
    }
}

/// The refusal an unclaimed, ambiguous bare symbol gets. Its job is to leave the operator with an
/// ACT, so it names the two classes rather than saying "ambiguous".
///
/// ⚠ It deliberately does NOT offer the `.P` spelling as the way to reach the inverse book. See
/// [`route_target`]'s own ⚠: that spelling lands there by the venue's leniency and by `USD` rather
/// than `USDT` being the quote asset, which is precisely the implicit encoding 0061 exists to
/// remove. Recommending it would document the bug as the cure.
fn ambiguous_bare_symbol_refusal(wire: &str) -> String {
    format!(
        "bybit lists {wire:?} as a Trading SPOT pair AND a Trading INVERSE PERPETUAL, and this \
         call named no asset class — so which book was meant cannot be decided here, and it is not \
         being guessed. The caller must name the class: CryptoSpot for the spot pair, CryptoPerp \
         for the perpetual (`vike_bybit::data::fetch_klines_range_classed`). This is worth naming \
         rather than defaulting because the spot side of such a collision is the near-dead one — \
         MEASURED on `BTCUSD`: four of six bars carried zero volume against a price ~$70 off the \
         perpetual's, on a listing whose status is nonetheless `Trading`. \
         ⚠ Do NOT reach for the `.P` suffix here: it says PERPETUAL, not inverse. It happens to \
         land on the inverse book for this symbol because the quote asset is USD rather than USDT \
         and because bybit resolves the request leniently — a property of the venue's API, not a \
         claim anybody made."
    )
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
/// The impure half of [`route_target`]: it resolves [`Listed`] from the venue's own instrument
/// lists ([`crate::instruments`]) and does so **only for the question that can change the outcome**.
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
/// route with none.** That cost is argued at [`crate::instruments`]'s "What it costs"; the thing to
/// carry here is that the two lookups share ONE cached derivation, so a process that does either
/// pays once.
fn range_target(
    symbol: &str,
    claimed: Option<vike_model::AssetClass>,
) -> Result<(Category, &str), String> {
    use vike_model::AssetClass;
    let (wire, suffixed) = vike_catalog::split_perp_at(VENUE, symbol);
    let wants_perp =
        suffixed || matches!(claimed, Some(AssetClass::CryptoPerp | AssetClass::CryptoFuture));
    let needs_ambiguity_proof =
        claimed.is_none() && !suffixed && vike_catalog::addressing_for(VENUE).must_claim();
    let listed = Listed {
        bare_is_ambiguous: if needs_ambiguity_proof {
            crate::instruments::bare_symbol_is_ambiguous(wire)?
        } else {
            false
        },
        perp_book: if wants_perp {
            crate::instruments::perp_book_for(wire)?
        } else {
            crate::instruments::PerpBook::Unlisted
        },
    };
    route_target(symbol, claimed, listed)
}

/// **The LIVE feed's route** — `(wire_symbol, category)` for a symbol arriving at a subscription,
/// or the same refusal the history path gives.
///
/// This is [`range_target`] under a name the feed can call, and the whole point is that it IS
/// [`range_target`]: `crates/bridges/bybit/src/market_feed.rs` used to read `.P` as "linear" through
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
/// listings cannot be read. Network I/O on the first call per process only.
pub fn live_route(symbol: &str) -> Result<(&str, Category), String> {
    let (book, wire) = range_target(symbol, None)?;
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
    perp_book: crate::instruments::PerpBook,
) -> Result<(&str, Category), String> {
    let (book, wire) = route_target(symbol, None, Listed { bare_is_ambiguous: false, perp_book })?;
    Ok((wire, book))
}

/// What the backward page walk does after one page — the pure decision [`next_page_step`] returns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PageStep {
    /// Request the next (older) page with this `end`.
    Next(i64),
    /// The walk is complete: the window start was reached, the page was empty, or the cursor failed
    /// to move backward.
    Done,
}

/// Pure: given the startTimes a page returned, the window `start_ms`, and the `end` (`page_end_ms`)
/// that produced it, decide the next `end` — or stop.
///
/// This is the ONE decision that makes Bybit's end-anchored truncation survivable, hoisted out of
/// the network path so it can be gated by a test (the twin of deribit's `next_page_step` and of the
/// extraction [`range_target`] made for the perp-category decision). Three ways to be
/// [`PageStep::Done`]:
///
/// - the page was empty (history ran out before the requested start);
/// - its oldest kline already reached `start_ms` (the window is fully covered);
/// - the next `end` would NOT be strictly older than the one that produced this page — a cursor that
///   cannot move backward would otherwise spin forever (reachable if the venue ever answers with
///   klines NEWER than the `end` asked for).
///
/// Note what is absent: no page-count / `MAX_LIMIT` comparison. A short page is NOT a stop signal —
/// termination is driven purely by OBSERVED startTimes, so a venue-side change to the 1000-row cap
/// cannot silently truncate a backfill again.
///
/// Uses the page's MINIMUM startTime rather than its first element, so wire order is not assumed —
/// this is what keeps the helper correct whether it is handed Bybit's raw newest-first rows or the
/// ascending bars [`bars_from_envelope`] already reversed.
pub fn next_page_step(page_ticks: &[i64], start_ms: i64, page_end_ms: i64) -> PageStep {
    let Some(&oldest) = page_ticks.iter().min() else {
        return PageStep::Done; // empty page — nothing older to walk toward
    };
    if oldest <= start_ms {
        return PageStep::Done; // the window start is covered
    }
    let next_end = oldest.saturating_sub(1);
    if next_end >= page_end_ms {
        return PageStep::Done; // the cursor must move strictly backward
    }
    PageStep::Next(next_end)
}

/// The BACKWARD page walk itself, over an INJECTED page fetcher — pure, and the reason the
/// truncation trap is testable without network I/O.
///
/// `fetch_page(end)` returns one page's bars (any order). The walk seeds `end = end_ms`, folds each
/// page through [`next_page_step`], keeps only bars inside the inclusive `[start_ms, end_ms]`
/// window, and finally sorts ascending + dedups by `ts` (pages arrive newest-block first, and a
/// boundary bar can repeat if the venue's bucketing ever overlaps).
///
/// It never sleeps: the inter-page throttle belongs to the caller's fetcher closure (see
/// [`fetch_klines_range`]), so a test walks many pages instantly.
pub fn walk_backward_pages<F>(
    start_ms: i64,
    end_ms: i64,
    mut fetch_page: F,
) -> Result<Vec<Bar>, String>
where
    F: FnMut(i64) -> Result<Vec<Bar>, String>,
{
    let mut out: Vec<Bar> = Vec::new();
    if end_ms < start_ms {
        return Ok(out); // empty window: never touch the venue
    }
    let mut page_end = end_ms;
    loop {
        let page = fetch_page(page_end)?;
        if page.is_empty() {
            break;
        }
        let ticks: Vec<i64> = page.iter().map(|b| b.ts).collect();
        for b in page {
            if start_ms <= b.ts && b.ts <= end_ms {
                out.push(b);
            }
        }
        match next_page_step(&ticks, start_ms, page_end) {
            PageStep::Next(next_end) => page_end = next_end,
            PageStep::Done => break,
        }
    }
    out.sort_by_key(|b| b.ts);
    out.dedup_by_key(|b| b.ts);
    Ok(out)
}

/// Fetch closed-kline history for the inclusive `[start_ms, end_ms]` window, walking `end` BACKWARD
/// through Bybit's end-anchored, silently-truncating 1000-rows/response cap (module doc, point 4).
/// Returns bars ascending by startTime, de-duplicated.
///
/// A `symbol` carrying [`vike_catalog::PERP_SUFFIX`] routes to `category=linear` and is stripped
/// before it reaches the wire; the CALLER's suffixed symbol remains the store/series key, so a
/// perp's series can never collide with its spot twin's. Network I/O. Errors on an unsupported
/// `interval`.
///
/// [`fetch_klines_range_paced`] with nothing seeded and the measurement dropped, so this 4-arg shape
/// — the one every backfill seam binds — issues the identical requests on the identical sleeps.
///
/// ⚠ **This is the NO-CLAIM entry point, so it is the one that can now REFUSE.** A bare symbol the
/// venue lists under both spot and inverse comes back as an `Err` rather than a spot tape — see
/// [`route_target`]. Callers that KNOW the class should bind [`fetch_klines_range_classed`]
/// instead; a claim is never refused for ambiguity.
pub fn fetch_klines_range(
    symbol: &str,
    interval: &str,
    start_ms: i64,
    end_ms: i64,
) -> Result<Vec<Bar>, String> {
    fetch_klines_range_classed(symbol, interval, start_ms, end_ms, None)
}

/// [`fetch_klines_range`] with the caller's asset-class CLAIM — the seam
/// `docs/decisions/0061-an-instrument-names-its-kind.md` verdict 3 specifies, spelled as an
/// `Option<AssetClass>` because most callers genuinely have no class to give.
///
/// `Some(class)` routes on the claim and is never refused for ambiguity; `None` is exactly
/// [`fetch_klines_range`] and can be. This is the entry point 0061's later phases bind as the class
/// reaches the bridges — the chart's window tag through `StoreBarRequest`, the seed request's
/// optional field, and the backfill CLI's own argument.
pub fn fetch_klines_range_classed(
    symbol: &str,
    interval: &str,
    start_ms: i64,
    end_ms: i64,
    class: Option<vike_model::AssetClass>,
) -> Result<Vec<Bar>, String> {
    fetch_klines_range_paced_classed(symbol, interval, start_ms, end_ms, None, class)
        .map(|(bars, _pace)| bars)
}

/// The pacer the pager runs on: a FALLBACK pacer over [`PAGE_DELAY`] (Bybit publishes no budget to
/// discover), optionally seeded with a previous run's observation.
///
/// Hoisted out of [`fetch_klines_range_paced`] for exactly the reason binance's `pacer_for` is: this
/// two-line function IS the venue's whole pacing policy, and a test must be able to prove it never
/// speeds the venue up — under any seed, after any observation — without opening a socket.
fn page_pacer(seed: Option<&PaceSample>) -> Pacer {
    let mut pacer = Pacer::fallback(PAGE_DELAY);
    if let Some(sample) = seed {
        let applied = pacer.seed(sample);
        tracing::debug!(
            target: "vike_bybit::data",
            applied,
            seed_request_ms = sample.request_ms,
            "bybit klines: persisted pace offered to the pacer (ETA only — the delay is fixed)"
        );
    }
    pacer
}

/// [`fetch_klines_range`] with the run's PACE measurement carried in and out: `seed` is a previous
/// run's observation (or `None`, which is byte-identical), and the second element of the return is
/// THIS run's — `None` unless a page was actually timed, so a run that fetched nothing cannot report
/// a fabricated pace.
///
/// ⚠ Neither direction moves a single sleep. A seed on a `Fixed` pacer buys exactly one thing: an
/// ETA from page ZERO instead of page one (`Pacer::next_delay` still answers [`PAGE_DELAY`] — see
/// this module's "The pager MEASURES its pages"). The persisted record's `per_request_weight` is
/// likewise inert here: Bybit publishes no weight counter, so the field carries the pacer's seed
/// constant and steers nothing — `request_ms` is the number an operator wants.
///
/// The CALLER owns the persistence (`vike_backfill::pace_book`); nothing in a bridge crate reads or
/// writes operator state.
pub fn fetch_klines_range_paced(
    symbol: &str,
    interval: &str,
    start_ms: i64,
    end_ms: i64,
    seed: Option<&PaceSample>,
) -> Result<(Vec<Bar>, Option<PaceSample>), String> {
    fetch_klines_range_paced_classed(symbol, interval, start_ms, end_ms, seed, None)
}

/// [`fetch_klines_range_paced`] carrying the caller's asset-class CLAIM as well as the pace
/// measurement — the full-fidelity entry point both other shapes delegate to, so the routing
/// decision and the pacing policy exist exactly once.
pub fn fetch_klines_range_paced_classed(
    symbol: &str,
    interval: &str,
    start_ms: i64,
    end_ms: i64,
    seed: Option<&PaceSample>,
    class: Option<vike_model::AssetClass>,
) -> Result<(Vec<Bar>, Option<PaceSample>), String> {
    let (book, wire) = range_target(symbol, class)?;
    let category = book.wire();
    let code = interval_code(interval)?;
    let agent = vike_bridge_core::http::blocking_agent();
    let mut pacer = page_pacer(seed);
    let mut first = true;
    // The pace report is emitted ONCE, after the first page that actually returned rows.
    let mut report_pace = true;
    let bars = walk_backward_pages(start_ms, end_ms, |page_end| {
        if !first {
            // Identical to the `PAGE_DELAY` this line used to sleep directly — `Pacer::fallback`
            // exists here to MEASURE, not to steer.
            std::thread::sleep(pacer.next_delay()); // throttle between pages, never before the first
        }
        first = false;
        // Wall-clock the request: it is the half of the real inter-request gap the pacer cannot see
        // for itself, and on this venue it DOMINATES the sleep. It spans `fetch_page_rate_limited`,
        // so a page that was rate-limited and retried reports its retry sleeps as "request time"
        // too — bounded and self-correcting (see `Pacer::observe_request`), and un-separable without
        // reaching inside the shared retry driver.
        //
        // `start_ms` is pinned on every page: the venue clamps it, and undoing that clamp is the
        // whole point of moving `page_end`.
        let started = std::time::Instant::now();
        let page = fetch_page_rate_limited(&agent, category, wire, code, start_ms, page_end)?;
        let elapsed = started.elapsed();
        // Observe unconditionally: the time was spent whether or not rows came back.
        pacer.observe_request(None, elapsed);
        // ONE line per backfill, not per page. `remaining_pages` takes the OBSERVED page size rather
        // than `MAX_LIMIT` — the same discipline `next_page_step` applies to termination, where a
        // row-count comparison is deliberately absent: a venue-side change to the cap shows up in
        // the ETA instead of silently multiplying it.
        if report_pace && !page.is_empty() {
            report_pace = false;
            let pages = remaining_pages(page_end.saturating_sub(start_ms), interval, page.len());
            if let Some((pages, eta)) = pages.zip(pages.and_then(|n| pacer.eta(n))) {
                tracing::info!(
                    target: "vike_bybit::data",
                    symbol,
                    interval,
                    request_ms = elapsed.as_millis() as u64,
                    page_rows = page.len() as u64,
                    page_delay_ms = pacer.next_delay().as_millis() as u64,
                    remaining_pages = pages,
                    eta_secs = eta.as_secs(),
                    "bybit kline backfill: measured page cost on the fixed page delay"
                );
            }
        }
        Ok(page)
    })?;
    Ok((bars, pacer.measured()))
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::instruments::PerpBook;
    use vike_model::AssetClass;

    /// What the venue's listings said, spelled at the call site so each routing test declares the
    /// FACTS it is routing on rather than inheriting a default.
    fn listed(bare_is_ambiguous: bool, perp_book: PerpBook) -> Listed {
        Listed { bare_is_ambiguous, perp_book }
    }

    /// The FETCHER's own routing decision — the gate the URL-shaping tests cannot provide, because
    /// they build a URL from `rest_category` directly and that helper was already perp-capable
    /// before this fix. Re-pinning `fetch_klines_range` to `CATEGORY` fails HERE.
    ///
    /// ⚠ The `.P` suffix says PERPETUAL. WHICH perpetual book is the venue's answer, injected here
    /// as [`PerpBook`] — see [`route_target`]'s ⚠.
    #[test]
    fn range_target_routes_a_perp_symbol_to_linear_with_the_suffix_stripped() {
        assert_eq!(
            route_target("BTCUSDT.P", None, listed(false, PerpBook::Linear)).unwrap(),
            (Category::Linear, "BTCUSDT")
        );
    }

    /// **THE PHASE-4 ROUTE, and the one assertion the whole branch exists for.** A symbol the venue
    /// lists on `category=inverse` reaches `category=inverse` — through a CLAIM, through the `.P`
    /// suffix, and through both together. Nothing here reads the symbol's shape: flip the injected
    /// [`PerpBook`] and the same three calls answer `linear`.
    ///
    /// ⚠ This is the test the mutation proof targets. `rest_category` falling back to `"linear"`
    /// for [`Category::Inverse`] reddens it, and so does `route_target` dropping arm 5.
    #[test]
    fn an_inverse_listed_perpetual_routes_to_the_inverse_category() {
        let inverse = listed(true, PerpBook::Inverse);
        assert_eq!(
            route_target("BTCUSD", Some(AssetClass::CryptoPerp), inverse).unwrap(),
            (Category::Inverse, "BTCUSD"),
            "a CryptoPerp claim on a symbol the venue lists inverse must reach the inverse book"
        );
        assert_eq!(
            route_target("BTCUSD.P", None, inverse).unwrap(),
            (Category::Inverse, "BTCUSD"),
            "the suffix says PERPETUAL and the VENUE says which perpetual book"
        );
        assert_eq!(
            route_target("BTCUSD.P", Some(AssetClass::CryptoPerp), inverse).unwrap(),
            (Category::Inverse, "BTCUSD")
        );
        // ...and the category the URL is actually built from.
        assert_eq!(Category::Inverse.wire(), "inverse");

        // The SAME symbol with the SAME claim, routed by a DIFFERENT venue listing. This is what
        // makes the assertion above about the listing rather than about `BTCUSD`.
        assert_eq!(
            route_target("BTCUSD", Some(AssetClass::CryptoPerp), listed(true, PerpBook::Linear))
                .unwrap(),
            (Category::Linear, "BTCUSD")
        );
    }

    /// ⚠ **26 of the venue's 28 inverse listings have no spot twin at all** (MEASURED 2026-09-16:
    /// `spot ∩ inverse` = `{BTCUSD, ETHUSD}`), so for them there is nothing ambiguous and a caller
    /// must not be asked to name anything. A bare, unclaimed, UNAMBIGUOUS symbol still routes to
    /// spot — bybit lists no bare `XRPUSD` spot pair, so that is a venue-side rejection by name —
    /// while the perpetual spellings reach the inverse book with no ceremony.
    #[test]
    fn an_inverse_only_symbol_needs_no_claim_to_reach_its_book() {
        let inverse_only = listed(false, PerpBook::Inverse);
        assert_eq!(
            route_target("XRPUSD.P", None, inverse_only).unwrap(),
            (Category::Inverse, "XRPUSD")
        );
        assert_eq!(
            route_target("XRPUSD", Some(AssetClass::CryptoPerp), inverse_only).unwrap(),
            (Category::Inverse, "XRPUSD")
        );
        assert_eq!(
            route_target("XRPUSD", None, inverse_only).unwrap(),
            (Category::Spot, "XRPUSD"),
            "a BARE symbol is still a spot claim: the perp book is only consulted for a perp"
        );
    }

    /// A symbol the venue lists under BOTH derivative categories is REFUSED, not guessed. Empty on
    /// the live venue today — which is what makes this a gate on the measurement rather than a
    /// restatement of it.
    #[test]
    fn a_symbol_in_both_derivative_books_is_refused() {
        let err = route_target("BOTHUSD.P", None, listed(false, PerpBook::Both))
            .expect_err("two derivative books, no claim that separates them");
        assert!(err.contains("BOTHUSD"), "the message must name the symbol: {err}");
        assert!(err.contains("category=linear") && err.contains("category=inverse"), "{err}");
        // A CLAIM does not rescue it either: `CryptoPerp` is what is already ambiguous here.
        assert!(
            route_target("BOTHUSD", Some(AssetClass::CryptoPerp), listed(false, PerpBook::Both))
                .is_err()
        );
    }

    /// A symbol the venue lists in NEITHER derivative category routes to `linear` — byte-identical
    /// to this crate's behaviour before the lookup existed, so a typo keeps failing at the venue by
    /// NAME rather than acquiring a new local failure mode. The variant is separate from
    /// `PerpBook::Linear` so "never measured" is legible in a stack trace.
    #[test]
    fn an_unlisted_perpetual_keeps_todays_linear_route() {
        assert_eq!(
            route_target("NOSUCHUSDT.P", None, Listed::unconsulted()).unwrap(),
            (Category::Linear, "NOSUCHUSDT")
        );
    }

    /// A bare symbol the venue does NOT list under inverse still resolves the spot category and is
    /// passed through untouched.
    ///
    /// ⚠ **This is the 290-symbol case and it must never be refused.** MEASURED 2026-09-16: bybit
    /// lists 290 symbols under both spot and LINEAR, and bare→spot is correct for every one of them
    /// — the linear perp already has its own `.P` spelling. `crates/bridges/bybit/src/instruments.rs`
    /// carries the predicate that keeps them out of the refusal.
    #[test]
    fn range_target_routes_a_bare_symbol_to_spot_unchanged() {
        assert_eq!(
            route_target("BTCUSDT", None, listed(false, PerpBook::Unlisted)).unwrap(),
            (Category::Spot, "BTCUSDT")
        );
    }

    /// **THE PHASE-0 REFUSAL, asserted with no network I/O at all** — `bare_is_ambiguous` is the
    /// injected half, which is exactly what `range_target`'s extraction exists for.
    ///
    /// The MEASURED case: bybit lists `BTCUSD` as a Trading spot pair and a Trading inverse
    /// perpetual, the spot tape is vestigial, and before this the guess was silent.
    #[test]
    fn an_unclaimed_ambiguous_bare_symbol_is_refused_rather_than_guessed() {
        let err = route_target("BTCUSD", None, listed(true, PerpBook::Inverse))
            .expect_err("an ambiguous bare symbol with no claim must not route");
        assert!(err.contains("BTCUSD"), "the message must name the symbol: {err}");
        assert!(err.contains("INVERSE PERPETUAL"), "{err}");
        assert!(err.contains("CryptoSpot") && err.contains("CryptoPerp"), "{err}");
    }

    /// ⚠ **The refusal must not recommend the suffix as the way to reach an inverse book.** That
    /// spelling works today by the venue's leniency plus `USD` rather than `USDT` being the quote
    /// asset — the implicit encoding 0061 exists to remove — so the message says what `.P` actually
    /// means and points at the CLASS instead. This test is what stops a later "helpful" edit turning
    /// the signpost back into the bug.
    #[test]
    fn the_refusal_does_not_sell_the_perp_suffix_as_an_inverse_selector() {
        let err =
            route_target("BTCUSD", None, listed(true, PerpBook::Inverse)).expect_err("refused");
        assert!(
            err.contains("says PERPETUAL, not inverse"),
            "the message must correct the suffix's meaning rather than recommend it: {err}"
        );
        assert!(
            !err.contains("BTCUSD.P"),
            "naming the suffixed spelling as the answer recommends relying on quote-asset shape \
             plus venue leniency: {err}"
        );
    }

    /// **A CLAIM ends the ambiguity — there is nothing left to refuse.** The same symbol, the same
    /// venue answer, routed both ways by what the caller named.
    ///
    /// ⚠ The perp half now lands on `inverse` rather than `linear`, and that is the WHOLE phase-4
    /// change in one line: before, a `CryptoPerp` claim on this symbol was served the right tape
    /// only because bybit resolves a `category=linear` request for it leniently.
    #[test]
    fn a_claim_resolves_the_ambiguous_symbol_both_ways() {
        assert_eq!(
            route_target("BTCUSD", Some(AssetClass::CryptoPerp), listed(true, PerpBook::Inverse))
                .unwrap(),
            (Category::Inverse, "BTCUSD")
        );
        assert_eq!(
            route_target("BTCUSD", Some(AssetClass::CryptoSpot), listed(true, PerpBook::Inverse))
                .unwrap(),
            (Category::Spot, "BTCUSD")
        );
    }

    /// A class this venue's kline path cannot address is REFUSED rather than coerced onto a
    /// category. `vike_catalog::addressing_for` is the authority, so this fails if that row shrinks.
    #[test]
    fn a_class_this_venue_cannot_address_is_refused() {
        for class in [AssetClass::Equity, AssetClass::Fx, AssetClass::Option] {
            let err = route_target("BTCUSDT", Some(class), Listed::unconsulted())
                .expect_err("a class this venue cannot address must be refused, not coerced");
            assert!(err.contains("addresses"), "{err}");
        }
        // ...and the two it CAN address are not refused.
        for class in [AssetClass::CryptoSpot, AssetClass::CryptoPerp] {
            assert!(
                route_target("BTCUSDT", Some(class), Listed::unconsulted()).is_ok(),
                "{class:?}"
            );
        }
    }

    /// Two claims that disagree — a `.P` symbol claimed as spot — is refused rather than one of them
    /// silently winning.
    #[test]
    fn a_claim_contradicting_the_suffix_is_refused() {
        let err = route_target("BTCUSDT.P", Some(AssetClass::CryptoSpot), Listed::unconsulted())
            .expect_err("a suffix and a spot claim disagree");
        assert!(err.contains("disagree"), "{err}");
        // ...and the agreeing combination routes.
        assert_eq!(
            route_target(
                "BTCUSDT.P",
                Some(AssetClass::CryptoPerp),
                listed(false, PerpBook::Linear)
            )
            .unwrap(),
            (Category::Linear, "BTCUSDT")
        );
    }

    /// The venue id this module looks its addressing row up by must be the one the catalog provider
    /// answers with — two literals, pinned equal, because the row this routes on is keyed on it and
    /// a typo would silently reach `VenueAddressing::UNCLASSIFIED`.
    #[test]
    fn the_venue_id_is_the_one_the_catalog_answers_with() {
        use vike_catalog::CatalogProvider;
        assert_eq!(VENUE, crate::catalog::BybitCatalog.venue());
        assert_ne!(
            vike_catalog::addressing_for(VENUE),
            vike_catalog::VenueAddressing::UNCLASSIFIED,
            "this venue must have a NAMED addressing row, or `must_claim` answers from the \
             refusing fallback rather than from a measurement"
        );
        assert!(
            vike_catalog::addressing_for(VENUE).must_claim(),
            "bybit's row is the measured ambiguity — if this flips, the instrument-list read below \
             stops happening and the refusal is dead code"
        );
    }

    /// A `.P` symbol and a claimed symbol are never refused for AMBIGUITY, which is how
    /// `range_target` skips the spot-collision lookup entirely for them.
    ///
    /// ⚠ **This test's scope shrank, and that is worth saying rather than quietly editing.** It used
    /// to prove a `.P` symbol needed NO listing read at all. Phase 4 made a perpetual claim consult
    /// the venue for its BOOK, so what survives is the narrower and still load-bearing claim: the
    /// `bare_is_ambiguous` half changes nothing for a suffix or a claim. The cost of the other half
    /// is argued at `crates/bridges/bybit/src/instruments.rs`'s "What it costs".
    #[test]
    fn a_suffix_or_a_claim_needs_no_ambiguity_answer() {
        // ⚠ **THE NO-REGRESSION ASSERTION for the one spelling that already worked.** `BTCUSD.P`
        // reached the perpetual's tape before any of this landed and must keep doing so — the
        // refusal above is scoped to the caller who named NOTHING, and a suffix is a name.
        //
        // What CHANGED under it is the reason: this used to resolve `category=linear` and be rescued
        // by the venue's leniency, and now it resolves `category=inverse` because the venue lists
        // the symbol there. The spelling is still not RECOMMENDED anywhere an operator reads — `.P`
        // says PERPETUAL, and the class (or the picker's perp row) is the way to mean one.
        let inverse = listed(true, PerpBook::Inverse);
        assert_eq!(route_target("BTCUSD.P", None, inverse).unwrap(), (Category::Inverse, "BTCUSD"));
        assert!(route_target("BTCUSD", Some(AssetClass::CryptoSpot), inverse).is_ok());
        // ...and the ambiguity flag genuinely changes nothing for either.
        assert_eq!(
            route_target("BTCUSD.P", None, listed(true, PerpBook::Inverse)).unwrap(),
            route_target("BTCUSD.P", None, listed(false, PerpBook::Inverse)).unwrap()
        );
        assert_eq!(
            route_target("BTCUSD", Some(AssetClass::CryptoSpot), listed(true, PerpBook::Inverse))
                .unwrap(),
            route_target("BTCUSD", Some(AssetClass::CryptoSpot), listed(false, PerpBook::Inverse))
                .unwrap()
        );
    }

    /// A previous run's record, shaped like the MEASURED sibling page (the CI box, 2026-08-04, okx:
    /// ~480 ms). `budget_per_min: None` because this venue publishes none — which is also what
    /// confines the record to another fallback pacer.
    fn bybit_record(request_ms: u64) -> PaceSample {
        PaceSample { request_ms, per_request_weight: 1.0, budget_per_min: None, samples: 20 }
    }

    /// ⚠ THE load-bearing test of this change: the pager MEASURES and the delay does not move.
    /// [`PAGE_DELAY`] is what the walk slept before the pacer existed, and it is bit-for-bit what
    /// `next_delay()` answers after — unseeded, seeded, and after observations at every scale.
    ///
    /// Asserted against the CONST rather than a literal `200`, so retuning the constant (a separate
    /// change, with its own measurement) moves this test with it instead of leaving it stale.
    #[test]
    fn the_pager_measures_but_never_changes_the_page_delay() {
        let mut p = page_pacer(None);
        assert!(!p.is_discovered(), "bybit publishes no budget to discover");
        assert_eq!(p.next_delay(), PAGE_DELAY);
        assert_eq!(p.eta(1_000), None, "nothing timed ⇒ no ETA, not one guessed from the sleep");

        for _ in 0..4 {
            p.observe_request(None, Duration::from_millis(480));
        }
        assert_eq!(p.next_delay(), PAGE_DELAY, "the measured 480ms must not move the sleep");
        assert!(!p.should_cool_down(), "and no budget ⇒ no target to cross");

        // A seed is likewise ETA-only: it can never make this pager faster.
        let mut seeded = page_pacer(Some(&bybit_record(480)));
        assert_eq!(seeded.next_delay(), PAGE_DELAY);
        seeded.observe_request(Some(9_999), Duration::from_secs(30));
        assert_eq!(seeded.next_delay(), PAGE_DELAY, "nor can any observation, at any scale");
    }

    /// What the measurement BUYS: 30 days of 1m klines is 43 of Bybit's 1000-row pages, whose real
    /// cost is ~20s of request time on top of 8.6s of sleep — a number the 200ms constant alone
    /// could never have produced, and the one the pager's ETA line now prints after page one.
    #[test]
    fn a_measured_page_yields_the_eta_the_fixed_delay_could_not() {
        let mut p = page_pacer(None);
        p.observe_request(None, Duration::from_millis(480));
        let month = 30 * 86_400_000i64;
        let pages = remaining_pages(month, "1m", MAX_LIMIT).expect("1m parses");
        assert_eq!(pages, 43, "30 days of 1m klines at 1000 rows/page");
        let eta = p.eta(pages).expect("a timed page yields an ETA");
        // 43 x (480ms + 200ms) = 29.24s. The sleep-only view would have claimed 8.6s.
        assert!(
            eta > Duration::from_millis(29_000) && eta < Duration::from_millis(29_500),
            "expected ~29s for the measured shape, got {eta:?}"
        );
        // A SEEDED pacer answers the same before page one has even been fetched.
        assert_eq!(page_pacer(Some(&bybit_record(480))).eta(pages), Some(eta));
        // ...and this run, having measured, hands its own observation back out to be persisted.
        let m = p.measured().expect("a fixed pager still times its pages");
        assert_eq!(m.request_ms, 480);
        assert_eq!(m.budget_per_min, None, "nothing discovered ⇒ nothing claimed");
        assert!(m.is_usable());
    }

    /// A minimal newest-first V5 envelope with two klines (so the reversal is exercised).
    const TWO_ROWS: &str = r#"{"retCode":0,"retMsg":"OK","result":{"category":"spot","symbol":"BTCUSDT","list":[
        ["1700000060000","27010.25","27100.00","27000.00","27080.10","8.10000000","219000.00"],
        ["1700000000000","27000.10","27050.50","26980.00","27010.25","12.34567800","333012.50"]
    ]},"retExtInfo":{},"time":1700000200000}"#;

    #[test]
    fn parse_reverses_to_ascending_and_maps_exactly() {
        let bars = parse_bybit_klines(TWO_ROWS).unwrap();
        assert_eq!(bars.len(), 2);
        // newest-first list → ascending bars: the LAST list row is bars[0].
        assert_eq!(bars[0].ts, 1_700_000_000_000);
        assert_eq!(bars[1].ts, 1_700_000_060_000);
        assert!(bars[0].ts < bars[1].ts, "ascending by startTime after reversal");
        assert_eq!(bars[0].open.to_bits(), 27000.10_f64.to_bits());
        assert_eq!(bars[0].high.to_bits(), 27050.50_f64.to_bits());
        assert_eq!(bars[0].low.to_bits(), 26980.00_f64.to_bits());
        assert_eq!(bars[0].close.to_bits(), 27010.25_f64.to_bits());
        assert_eq!(bars[0].volume.to_bits(), 12.345678_f64.to_bits());
        assert!(bars[0].symbol.is_none() && bars[0].funding.is_none());
    }

    #[test]
    fn parse_empty_list_is_empty() {
        let body = r#"{"retCode":0,"retMsg":"OK","result":{"category":"spot","symbol":"BTCUSDT","list":[]},"retExtInfo":{},"time":1}"#;
        assert!(parse_bybit_klines(body).unwrap().is_empty());
    }

    #[test]
    fn parse_nonzero_retcode_errors() {
        let body =
            r#"{"retCode":10001,"retMsg":"params error","result":{},"retExtInfo":{},"time":1}"#;
        assert!(parse_bybit_klines(body).is_err());
    }

    #[test]
    fn parse_rejects_short_row() {
        let body = r#"{"retCode":0,"retMsg":"OK","result":{"list":[["1700000000000","1","2","3"]]},"time":1}"#;
        assert!(parse_bybit_klines(body).is_err());
    }

    #[test]
    fn interval_code_maps_or_errors() {
        assert_eq!(interval_code("1m").unwrap(), "1");
        assert_eq!(interval_code("1h").unwrap(), "60");
        assert_eq!(interval_code("1d").unwrap(), "D");
        assert_eq!(interval_code("1M").unwrap(), "M");
        assert!(interval_code("7s").is_err());
    }

    #[test]
    fn klines_url_has_category_and_bounds() {
        let url = klines_url(CATEGORY, "BTCUSDT", "1", 10, 20, 1000);
        assert_eq!(
            url,
            "https://api.bybit.com/v5/market/kline?category=spot&symbol=BTCUSDT&interval=1&start=10&end=20&limit=1000"
        );
    }

    /// The perp suffix selects a derivative category AND is stripped from the wire symbol. The
    /// BOOK comes from the venue's listings (`route_target` arm 5), so both are asserted here.
    #[test]
    fn perp_suffix_selects_a_derivative_category_and_strips_the_wire_symbol() {
        let (wire, perp) = vike_catalog::split_perp("BTCUSDT.P");
        assert_eq!(wire, "BTCUSDT");
        assert!(perp);
        let url = klines_url(Category::Linear.wire(), wire, "1", 10, 20, 1000);
        assert!(url.contains("category=linear"), "a linear-listed perp must use linear: {url}");
        assert!(url.contains("symbol=BTCUSDT&"), "the .P must not reach the wire: {url}");
        assert!(!url.contains(".P"), "the .P must not reach the wire: {url}");
    }

    /// ⚠ **The third category, spelled all the way to the URL.** This is the assertion that fails if
    /// [`rest_category`]'s `Inverse` arm is re-pinned to `"linear"` — the URL builder and the
    /// routing decision share one enum, so neither can drift from the other.
    #[test]
    fn an_inverse_route_builds_an_inverse_url() {
        let (wire, perp) = vike_catalog::split_perp("BTCUSD.P");
        assert!(perp);
        let url = klines_url(Category::Inverse.wire(), wire, "1", 10, 20, 1000);
        assert_eq!(
            url,
            "https://api.bybit.com/v5/market/kline?category=inverse&symbol=BTCUSD&interval=1&start=10&end=20&limit=1000"
        );
        assert_eq!(Category::Inverse.wire(), "inverse");
        assert_ne!(
            Category::Inverse.wire(),
            Category::Linear.wire(),
            "the two derivative books must not collapse to one category string"
        );
    }

    /// A bare symbol is unchanged — spot URLs stay byte-identical to the pre-change form.
    #[test]
    fn a_bare_symbol_still_builds_the_spot_url() {
        let (wire, perp) = vike_catalog::split_perp("BTCUSDT");
        assert!(!perp);
        assert_eq!(
            klines_url(Category::Spot.wire(), wire, "1", 10, 20, 1000),
            "https://api.bybit.com/v5/market/kline?category=spot&symbol=BTCUSDT&interval=1&start=10&end=20&limit=1000"
        );
    }

    // ---- the end-anchored truncation trap (module doc, point 4) --------------------------------

    const MIN: i64 = 60_000;
    const T0: i64 = 1_700_000_000_000;
    /// 2.5 pages at the 1000-row cap — enough that a single-page test would pass while the window
    /// came back 60% empty.
    const BARS: i64 = 2_500;

    fn synthetic_bar(ts: i64) -> Bar {
        kline_to_bar(ts, 100.0, 101.0, 99.0, 100.5, 1.0)
    }

    /// One page from a fake venue that behaves EXACTLY like Bybit's `/v5/market/kline`: it **ignores
    /// `start` entirely** and answers the newest [`MAX_LIMIT`] klines at or below `page_end`, out of
    /// a synthetic series of [`BARS`] one-minute klines beginning at [`T0`].
    ///
    /// Returned ASCENDING — i.e. already reversed, exactly what [`bars_from_envelope`] hands the
    /// walk — so these tests cannot accidentally paper over a double-reverse. The newest-first RAW
    /// order is covered separately by `the_walk_is_independent_of_wire_order_within_a_page`.
    fn end_anchored_page(page_end: i64) -> Vec<Bar> {
        if page_end < T0 {
            return Vec::new(); // older than the series' first kline
        }
        let newest_i = ((page_end - T0) / MIN).min(BARS - 1);
        let oldest_i = (newest_i - MAX_LIMIT as i64 + 1).max(0);
        (oldest_i..=newest_i).map(|i| synthetic_bar(T0 + i * MIN)).collect()
    }

    /// THE load-bearing test: a multi-page window over a venue that reproduces the live failure.
    ///
    /// The pre-fix forward pager walked `start` upward, so its FIRST request came back at the far
    /// END of the window, the cursor jumped past `end_ms`, and the loop exited after one page —
    /// MEASURED on the CI box 2026-08-04 as 1,001 rows for a 43,200-minute request. A single-page test
    /// cannot see any of that; this one fails outright against the old pager.
    #[test]
    fn the_walk_goes_backward_and_returns_a_multi_page_window_whole() {
        let start = T0;
        let end = T0 + (BARS - 1) * MIN;

        // First prove the FAKE really is faithful: one end-anchored request for the whole window
        // answers only the newest cap-many rows, and its oldest row is nowhere near `start`.
        let naive = end_anchored_page(end);
        assert_eq!(naive.len(), MAX_LIMIT, "the venue truncates to its cap");
        assert_eq!(
            naive[0].ts,
            T0 + (BARS - MAX_LIMIT as i64) * MIN,
            "the newest page only — this is the truncation signature"
        );
        assert!(naive[0].ts > start, "the requested start is ignored; the older tail is dropped");

        let mut pages = 0usize;
        let bars = walk_backward_pages(start, end, |page_end| {
            pages += 1;
            assert!(pages < 100, "runaway walk");
            Ok(end_anchored_page(page_end))
        })
        .expect("walk");

        assert_eq!(pages, 3, "2500 klines at a 1000-row cap is three pages");
        assert!(bars.len() > MAX_LIMIT, "strictly more than one end-anchored page");
        assert_eq!(bars.len(), BARS as usize, "the window must come back WHOLE");
        assert_eq!(bars.first().expect("non-empty").ts, start, "reaches the requested start");
        assert_eq!(bars.last().expect("non-empty").ts, end, "reaches the requested end");
        assert!(bars.windows(2).all(|w| w[0].ts < w[1].ts), "ascending and de-duplicated");
        for (i, b) in bars.iter().enumerate() {
            assert_eq!(b.ts, T0 + i as i64 * MIN, "contiguous — no gap at the page seams (i={i})");
        }
    }

    /// The double-reverse guard: [`next_page_step`] keys off the page MINIMUM, not its first or last
    /// element, so the same window comes back identically whether pages arrive ascending (what
    /// [`bars_from_envelope`] produces) or newest-first (Bybit's raw `result.list` order). An
    /// implementation that read `page[0].ts` or `page.last()` would walk the wrong direction on one
    /// of the two and fail here.
    #[test]
    fn the_walk_is_independent_of_wire_order_within_a_page() {
        let start = T0;
        let end = T0 + (BARS - 1) * MIN;
        let ascending =
            walk_backward_pages(start, end, |pe| Ok(end_anchored_page(pe))).expect("asc walk");
        let newest_first = walk_backward_pages(start, end, |pe| {
            let mut p = end_anchored_page(pe);
            p.reverse(); // Bybit's RAW result.list order, before `bars_from_envelope` reverses it
            Ok(p)
        })
        .expect("desc walk");

        assert_eq!(ascending.len(), BARS as usize);
        assert_eq!(ascending.len(), newest_first.len(), "same window either way");
        assert!(
            ascending.iter().zip(&newest_first).all(|(a, b)| a.ts == b.ts),
            "identical ts sequence regardless of intra-page wire order"
        );
        assert_eq!(newest_first.first().expect("non-empty").ts, start);
    }

    /// A venue that keeps answering the SAME newest block regardless of `end` must stop, not spin.
    /// This is the guard the task calls for: termination cannot depend on the cursor advancing.
    #[test]
    fn a_non_advancing_cursor_stops_instead_of_looping_forever() {
        let mut pages = 0usize;
        let bars = walk_backward_pages(T0, T0 + 10 * MIN, |_page_end| {
            pages += 1;
            assert!(pages < 50, "the walk must not loop forever on a stuck cursor");
            // Always the same block, ignoring `page_end` entirely.
            Ok(vec![synthetic_bar(T0 + 9 * MIN), synthetic_bar(T0 + 10 * MIN)])
        })
        .expect("walk");
        assert_eq!(pages, 2, "one page, then one whose oldest failed to move — then stop");
        assert_eq!(bars.len(), 2, "deduped to the two distinct klines");
    }

    /// Cross-page overlap is deduped, out-of-window klines are clipped, and the result is sorted.
    #[test]
    fn pages_are_deduped_sorted_and_clipped_to_the_window() {
        let start = T0 + 2 * MIN;
        let end = T0 + 4 * MIN;
        let mut call = 0usize;
        let bars = walk_backward_pages(start, end, |_page_end| {
            call += 1;
            Ok(match call {
                // newest page: carries one kline ABOVE the window end
                1 => vec![
                    synthetic_bar(T0 + 3 * MIN),
                    synthetic_bar(T0 + 4 * MIN),
                    synthetic_bar(T0 + 5 * MIN),
                ],
                // older page: overlaps T0+3MIN and reaches BELOW the window start
                2 => vec![
                    synthetic_bar(T0 + MIN),
                    synthetic_bar(T0 + 2 * MIN),
                    synthetic_bar(T0 + 3 * MIN),
                ],
                _ => Vec::new(),
            })
        })
        .expect("walk");
        let ts: Vec<i64> = bars.iter().map(|b| b.ts).collect();
        assert_eq!(ts, vec![T0 + 2 * MIN, T0 + 3 * MIN, T0 + 4 * MIN]);
        assert_eq!(call, 2, "the second page reached the start and ended the walk");
    }

    #[test]
    fn an_empty_page_terminates_the_walk_and_an_empty_window_never_asks() {
        let mut pages = 0usize;
        let bars = walk_backward_pages(T0, T0 + 10 * MIN, |_| {
            pages += 1;
            Ok(Vec::new())
        })
        .expect("walk");
        assert_eq!(pages, 1, "an empty page stops the walk immediately");
        assert!(bars.is_empty());

        let mut asked = false;
        let bars = walk_backward_pages(T0 + MIN, T0, |_| {
            asked = true;
            Ok(vec![synthetic_bar(T0)])
        })
        .expect("walk");
        assert!(!asked, "end < start must never touch the venue");
        assert!(bars.is_empty());
    }

    #[test]
    fn a_page_error_aborts_the_walk() {
        let err =
            walk_backward_pages(T0, T0 + 10 * MIN, |_| Err("bybit klines HTTP 500".to_string()))
                .expect_err("the error must propagate, not silently truncate");
        assert!(err.contains("HTTP 500"), "{err}");
    }

    #[test]
    fn next_page_step_decisions() {
        let end = T0 + 10 * MIN;
        // Normal: step to one ms below the page's OLDEST kline.
        assert_eq!(
            next_page_step(&[T0 + 5 * MIN, T0 + 6 * MIN], T0, end),
            PageStep::Next(T0 + 5 * MIN - 1)
        );
        // Wire order is irrelevant — the MINIMUM decides, not the first element.
        assert_eq!(
            next_page_step(&[T0 + 6 * MIN, T0 + 5 * MIN], T0, end),
            PageStep::Next(T0 + 5 * MIN - 1)
        );
        // The window start is covered (exactly, then overshot).
        assert_eq!(next_page_step(&[T0, T0 + MIN], T0, end), PageStep::Done);
        assert_eq!(next_page_step(&[T0 - MIN], T0, end), PageStep::Done);
        // Empty page.
        assert_eq!(next_page_step(&[], T0, end), PageStep::Done);
        // Klines NEWER than the `end` asked for cannot move the cursor backward.
        assert_eq!(next_page_step(&[end + MIN], T0, end), PageStep::Done);
        // A SHORT page is NOT a stop signal: termination is by observed ticks, never a page count.
        assert_eq!(next_page_step(&[T0 + 9 * MIN], T0, end), PageStep::Next(T0 + 9 * MIN - 1));
    }

    #[test]
    fn latest_url_spells_every_category_the_live_feed_can_seed_from() {
        let perp = latest_url(Category::Linear.wire(), "BTCUSDT", "1", 1000);
        assert_eq!(
            perp,
            "https://api.bybit.com/v5/market/kline?category=linear&symbol=BTCUSDT&interval=1&limit=1000"
        );
        let spot = latest_url(Category::Spot.wire(), "BTCUSDT", "1", 1000);
        assert_eq!(
            spot,
            "https://api.bybit.com/v5/market/kline?category=spot&symbol=BTCUSDT&interval=1&limit=1000"
        );
        // ⚠ The seed the INVERSE live chart warms up from. It matters that this is the same
        // `Category` value `market_feed::ws_host` picks the socket with: a seed and a stream that
        // resolved the book independently could disagree, which is what the `bool` this parameter
        // used to be actually did.
        let inverse = latest_url(Category::Inverse.wire(), "BTCUSD", "1", 1000);
        assert_eq!(
            inverse,
            "https://api.bybit.com/v5/market/kline?category=inverse&symbol=BTCUSD&interval=1&limit=1000"
        );
    }
}
