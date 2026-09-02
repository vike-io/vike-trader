//! Shared Binance-grammar kline REST history — the ONE kline fetch + JSON→[`Bar`] mapper reused by
//! vike-aster. This is rung 6 of the family (see [`crate::family`] mod doc): the pure map
//! ([`parse_klines`]/[`klines_url`]) AND the paged, rate-limited fetchers, with every per-venue
//! delta passed in as a parameter.
//!
//! Aster's kline wire format is a byte-identical Binance fork — the 12-element array
//! (`[openTime(ms i64), open, high, low, close, volume, closeTime, quoteVol, trades, takerBuyBase,
//! takerBuyQuote, ignore]`, o/h/l/c/v decimal strings), the 1000-row single-response cap, and the
//! `X-MBX-USED-WEIGHT-1M`-header-aware paged backfill all match Binance's. The ONLY per-venue
//! deltas are:
//!   1. **Host resolution** — Binance uses `&'static str` consts (`api.binance.com`/
//!      `fapi.binance.com`, never env-resolved); Aster resolves testnet-vs-mainnet + spot-vs-perp
//!      from `urls::urls_for(env)`. Both are resolved BY THE CALLER and handed in as the fully-built
//!      `base` (host+path) — nothing here decides which network it is on. Same discipline as the
//!      market-data ([`crate::family::UrlTable`]) and recon ([`crate::family::recon::ReconSpec`])
//!      rungs. The venue's DISCOVERY endpoint ([`KlineSpec::exchange_info_url`]) is resolved the
//!      same way and at the same moment, which is why it is a `Cow` rather than a `&'static str`.
//!   2. **The venue label + rate-limit budget** — passed in as a [`KlineSpec`]. The budget is a
//!      per-venue VALUE precisely because Binance's is live-verified while Aster's is an unverified
//!      carry-over: keeping it a parameter lets Aster's diverge without touching Binance's.
//!
//! ## Rate-limit strategy (the paged [`fetch_klines_range`] only)
//! Binance's spot REST is request-*weight* limited (6000 weight/min per IP; `/klines` is weight 2);
//! Aster's v3 API is a Binance fork and ships the same weight-header convention. A burst returns
//! HTTP 429 (temporary block) and ignoring a 429 escalates to 418 (IP ban). So the backfill pager:
//! 1. Uses `limit=1000` (the max) to minimise request count, advancing the cursor by the last
//!    kline's `openTime + 1ms` to page forward with no overlap.
//! 2. Paces each page against the venue's OWN published budget where the venue publishes one —
//!    see "Discovered pacing" below — and against the fixed [`KlineRateLimit::page_delay`] where it
//!    does not. Independently of either, it reads the `X-MBX-USED-WEIGHT-1M` response header and
//!    pauses [`KlineRateLimit::weight_cooldown`] whenever used weight crosses
//!    [`KlineRateLimit::weight_soft_limit`] (staying clear of the hard cap).
//! 3. On 429/418, backs off exponentially but HONORS the `Retry-After` header (sleeps exactly that
//!    long) and retries the *same* page, bounded by [`KlineRateLimit::max_rate_limit_retries`].
//!
//! ## Discovered pacing ([`KlineSpec::exchange_info_url`])
//! A venue that publishes its own `REQUEST_WEIGHT` budget in `exchangeInfo` should be paced against
//! THAT number rather than a hand-measured `page_delay` — the delay only ever encoded
//! `budget / per-request-weight` as a constant someone divided out once, and it is wrong the moment
//! the venue re-prices the endpoint or changes the limit. So when a spec names an
//! [`KlineSpec::exchange_info_url`], [`fetch_klines_range`] makes **ONE** lightweight GET of it
//! before the paging loop (never per page), parses the budget with
//! [`vike_bridge_core::rate_discovery::parse_weight_budget`], and drives a
//! [`vike_bridge_core::pacer::Pacer`] that targets [`KlineSpec::utilization`] of it and infers the
//! per-request weight from successive `X-MBX-USED-WEIGHT-1M` deltas.
//!
//! The URL is checked against the `base` being paged before any request is made
//! ([`discovery_url`]): a published budget belongs to a HOST, so a spec may only be paced against a
//! budget read from the host it is paging. Binance's spot host publishes 6000/min and its fapi host
//! 2400; aster's mainnet fapi publishes 2400 while its TESTNET fapi publishes a nonsense `-2`
//! (MEASURED 2026-08-05, and rejected by [`vike_bridge_core::rate_discovery`]'s positivity check) —
//! so both the market and the ENVIRONMENT halves of that pairing are live hazards, not hypotheses.
//!
//! **Discovery failure is never a backfill failure.** No URL, a URL on some other host, a network
//! error, a non-2xx, a body that does not parse, or a venue that publishes no `REQUEST_WEIGHT` row
//! all collapse to [`vike_bridge_core::pacer::Pacer::fallback`] — a fixed `page_delay`, i.e. the
//! exact pre-discovery behaviour. A spec with `exchange_info_url: None` additionally makes **no
//! request at all**, so an undiscoverable venue issues byte-identical requests on a byte-identical
//! sleep sequence.
//!
//! It is no longer SILENT, though. A fallback pager still times every page and still reports the
//! ETA line below — `discovered = false` is a field on that line, not a reason to withhold it. The
//! old gate rested on "there is nothing measured to report", which was only ever half true: the
//! budget was unknown, the request time never was.
//!
//! ## The gap is `sleep + request` (why each page is TIMED)
//! The pager's real inter-request spacing is `next_delay() + the request's own round trip`, and on
//! this venue the round trip DOMINATES: ~280 ms through the pooled agent against a 312 ms target
//! gap. So each page is wall-clocked and the measurement is fed back through
//! [`vike_bridge_core::pacer::Pacer::observe_request`], which subtracts it from the next sleep.
//! Without that, the 40 % utilization target metered as 23 % (MEASURED on the CI box, 2026-08-04: a
//! 24-month `BTCUSDT.P` backfill finished in 8m00s and left `x-mbx-used-weight-1m` at 556 of 2400).
//! The first page additionally emits ONE `info` line — measured request time, inferred weight, and
//! the ETA for the remaining pages — because a multi-minute backfill that prints nothing is
//! indistinguishable from a hung one. ONE line, never per page: this loop runs hundreds of times.
//!
//! ⚠ Two consequences of actually HITTING the target, both fine and both making the ETA optimistic.
//! First, spending the targeted share of a window now means the counter REACHES that share, so
//! `Pacer::should_cool_down` — dead code while the pager under-spent — fires near the end of each
//! window and adds a [`KlineRateLimit::weight_cooldown`] pause the ETA does not model. Second, the
//! counter is per-IP and shared with anything else on this egress, so it can cross the share sooner
//! than our own requests explain. Both slow the backfill down, never up; the ETA is a floor.
//!
//! ## Carrying the measurement ACROSS runs ([`fetch_klines_range_paced`])
//! Everything above is re-measured from scratch every process: the pager opens on the pessimistic
//! [`vike_bridge_core::pacer`] seed constant, spends its first page discovering the real weight and
//! round trip, and then exits and forgets both. [`fetch_klines_range_paced`] is the same pager with
//! that measurement plumbed in and out — a caller supplies the PREVIOUS run's
//! [`vike_model::rate_limits::PaceSample`] and receives this run's, and a binary
//! (`vike_backfill::pace_book`) owns the file in between, because nothing in a bridge crate reads
//! or writes operator state.
//!
//! [`fetch_klines_range`] is now that function with `None` in and the report dropped, so **every
//! existing caller is byte-identical** — same requests, same sleeps, same output. The pacer refuses
//! a seed measured against a different budget, so a persisted number can only ever move the SLEEP
//! inside the venue's discovered budget, never the budget (see `Pacer::seed`'s rule 3).
//!
//! ## Several windows IN FLIGHT ([`fetch_klines_range_lanes`])
//! Everything above paces ONE request at a time, and a sequential pager cannot space requests closer
//! together than one request TAKES. That is invisible on **fapi**, whose 312 ms target gap is wider
//! than the ~280 ms round trip — but it is the whole story on **spot**, where 6000 weight/min at the
//! measured weight-2 page cost is a **50 ms** target gap, `Pacer::next_delay` is already floored, and
//! the backfill therefore delivers ~18 % of the budget the operator asked for with nothing left to
//! tune. [`fetch_klines_range_lanes`] runs several disjoint sub-WINDOWS concurrently
//! ([`walk_forward_spans`]) so the existing target becomes reachable.
//!
//! Three properties keep that from being a rate increase in disguise, and each is enforced somewhere
//! a test can see it:
//! 1. **ONE pacer, shared.** Every lane takes its dispatch slot from a single
//!    [`vike_bridge_core::concurrent::LaneGate`], which admits one request per
//!    [`vike_bridge_core::pacer::Pacer::target_gap`] however many lanes are queued. Both proactive
//!    guards (the hand-set `weight_soft_limit` and `Pacer::should_cool_down`) pause every lane at
//!    once. N per-lane pacers would each target the venue's whole budget and spend it N times.
//! 2. **The lane COUNT is derived, not chosen** —
//!    [`vike_bridge_core::pacer::Pacer::suggested_lanes`] = `ceil(request_time / target_gap)`, from
//!    this run's own measurement (or last run's, via the [`PaceSample`] seed). Where the budget is
//!    the binding constraint it answers `1` and nothing changes.
//! 3. **No discovered budget ⇒ one lane, structurally.** A `Fixed` pacer never returns more, so a
//!    venue that publishes no ceiling — bybit/okx/deribit — is excluded by the rule rather than by
//!    name. Concurrency needs a published ceiling for the aggregate to be a fraction OF; without one
//!    it is an unmeasured rate increase. The rule is a RUNTIME one, so it also covers a venue that
//!    normally discovers and did not this run (bad body, dead endpoint, mispaired host).
//!
//! Two consequences of "derived from a measurement" that are easy to get wrong, and both cost the
//! whole speedup if you do:
//! * the derivation needs [`PROBE_PAGES`] = **two** sequential pages, because a per-request weight is
//!   a DELTA and one reading is not one. With a single probe, spot derives 3 lanes on a 125 ms gap
//!   instead of 6 on 50 ms — the pessimistic seed weight, not the venue's;
//! * once the lanes are running, a counter reading can no longer be attributed to a request, so the
//!   gate records it through [`vike_bridge_core::pacer::Pacer::observe_absolute`] and infers nothing.
//!   Feeding those readings to `Pacer::observe` inflates the per-request weight by about the lane
//!   count, which widens the shared gap by the same factor and cancels the lanes exactly.
//!
//! The window is split, never the PAGE GRID. Precomputing page boundaries would assume the venue
//! serves rows on a fixed grid at a fixed cap — false for the end-anchored venues that page backward
//! from what the last page returned, where a wrong window silently TRUNCATES (#1030/#1039). A span
//! split assumes nothing: this pager is already correct for any caller-supplied `[start, end]`, so a
//! lane is the same walk answering a narrower question.
//!
//! The warmup ([`fetch_klines_latest`]) is a single request and is deliberately left un-throttled —
//! its behaviour matches the legacy feed and needs only the venue label, not the budget.
//!
//! Each venue keeps its OWN thin `data.rs` wrappers (its host resolution, its public signatures, its
//! rate-limit `const`, its URL-resolution tests), so the shared pure map here is proven once at the
//! rung and each venue's host resolution is proven per-venue.

use std::borrow::Cow;
use std::time::Duration;

use vike_bridge_core::http::{body_head, get_raw};
use vike_bridge_core::klines::kline_to_bar;
use vike_bridge_core::pacer::Pacer;
use vike_bridge_core::rate_discovery::parse_weight_budget;
use vike_bridge_core::retry::{retry_rate_limited, BackoffPolicy, Verdict};
use vike_model::rate_limits::PaceSample;
use vike_model::Bar;

/// Binance/Aster hard cap on klines returned by a single request. Shared Binance grammar (both
/// venues 1000, `Binance-verbatim`) — this is the API's fixed page size, NOT a per-venue rate knob,
/// so it stays a shared const rather than a [`KlineSpec`] field.
pub const MAX_LIMIT: usize = 1000;

/// The paged-backfill rate-limit budget for one venue — passed in as a VALUE so the two venues stay
/// **independently tunable**. Binance's numbers are live-verified (of the 6000-weight/min spot
/// budget, `weight_soft_limit` sits at 5000); Aster's are an unverified carry-over pending a real
/// testnet `exchangeInfo`/rate-limit response. They currently coincide, but are kept as SEPARATE
/// per-venue `const`s on purpose — do NOT converge Aster's guess onto Binance's verified value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KlineRateLimit {
    /// Fixed throttle between successive page requests in a range backfill.
    pub page_delay: Duration,
    /// `X-MBX-USED-WEIGHT-1M` value at/above which we proactively cool down.
    pub weight_soft_limit: u64,
    /// How long to pause once used weight crosses [`Self::weight_soft_limit`] (lets the 1-min
    /// window decay).
    pub weight_cooldown: Duration,
    /// Bounded retries for a single page that keeps getting 429/418'd.
    pub max_rate_limit_retries: u32,
    /// First 429/418 backoff when the server sends no `Retry-After` (doubles each retry, capped).
    pub initial_backoff: Duration,
    /// Ceiling on any single backoff/`Retry-After` sleep.
    pub max_backoff: Duration,
}

/// Everything the shared kline fetch needs to know about the venue it is serving this call: the
/// label stamped into error/log strings, the [`KlineRateLimit`] fallback budget, and where (if
/// anywhere) the venue publishes its OWN budget.
///
/// `Clone` but **not `Copy`**, since [`Self::exchange_info_url`] is a [`Cow`] — see that field for
/// why the discovery URL has to be able to be OWNED. A spec whose URL is a `&'static str` const is
/// still a `const` and still free to build (`Cow::Borrowed` is const-constructible), so binance's
/// two specs are unchanged in every way a caller or the venue can observe.
///
/// `Eq` is deliberately NOT derived (only `PartialEq`): [`Self::utilization`] is an `f64`, which has
/// no total equality. Nothing keys a map or a set on a spec — the only equality uses are the venues'
/// `assert_eq!(spec, &BINANCE_KLINE_PERP, …)` routing tests, which need `PartialEq` alone.
#[derive(Debug, Clone, PartialEq)]
pub struct KlineSpec {
    /// The canonical lowercase venue key used in error/log strings (`"binance"` / `"aster"`, so the
    /// messages read `"binance klines HTTP …"` / `"aster klines rate-limited: …"`).
    pub venue: &'static str,
    /// The paged-backfill rate-limit budget (see [`KlineRateLimit`]). Its `page_delay` is the
    /// FALLBACK pace — what the pager uses when discovery does not answer.
    pub rate_limit: KlineRateLimit,
    /// Where this venue publishes its own `REQUEST_WEIGHT` budget, if it does — **resolved by the
    /// venue face, for the host it is about to page**.
    ///
    /// A [`Cow`], not a `&'static str`, because a discovery endpoint is exactly as
    /// caller-resolved as [`fetch_klines_range`]'s `base` is, and for the same reasons. Both are
    /// the venue's own host decision, and this rung has always refused to make it (see the module
    /// doc's delta 1). The two shapes a venue face can hand in:
    ///
    /// * [`Cow::Borrowed`] — the host is a compile-time constant. Binance's two hosts
    ///   (`api.binance.com` 6000/min, `fapi.binance.com` 2400/min) are `&'static str` consts, so
    ///   both specs stay `const` and cost nothing.
    /// * [`Cow::Owned`] — the host is COMPOSED per [`vike_bridge_core::Environment`] (or per
    ///   market, or per region) and is not nameable at compile time. Aster resolves testnet vs
    ///   mainnet and sapi vs fapi through its own `urls::urls_for(env)` table, so its discovery URL
    ///   is a `String` built beside the `base` it pairs with (see `vike_aster::data`).
    ///
    /// This is a general capability, not an aster affordance: any venue whose endpoints are
    /// env-resolved can now be discovered, and a venue that publishes nothing still names `None`.
    ///
    /// `None` = **not discoverable**: no request is made and the pager behaves exactly as it did
    /// before discovery existed (the fixed [`KlineRateLimit::page_delay`]).
    ///
    /// ⚠ A URL here must be the `exchangeInfo` of the SAME host the pager is paging, because a
    /// published budget belongs to a HOST. Crossing them pairs the 2400-weight fapi host with
    /// spot's 6000, or a testnet backfill with mainnet's budget. That is no longer only a
    /// convention: [`discovery_url`] enforces it at the ONE point that holds both the spec and the
    /// `base`, and a mismatch degrades to the fallback pace instead of pacing against a number that
    /// belongs to a different machine.
    pub exchange_info_url: Option<Cow<'static, str>>,
    /// Target FRACTION of the discovered budget to spend (see [`vike_model::rate_limits`], which
    /// owns the number, its default and its bounds). Every const site takes
    /// [`vike_model::rate_limits::DEFAULT_UTILIZATION`]; the pacer clamps whatever arrives, so no
    /// value here can produce a hang or an over-budget hammer. Inert when
    /// [`Self::exchange_info_url`] is `None` — there is no budget to take a fraction of.
    pub utilization: f64,
}

/// Map one raw kline row (the 12-element JSON array) → a [`Bar`]. o/h/l/c are required decimal
/// strings; volume defaults to 0.0 on a parse miss (the legacy feed's tolerance). Preserves the
/// exact f64 bit pattern of each string via `<f64 as FromStr>` — no rounding, no arithmetic.
fn row_to_bar(r: &[serde_json::Value]) -> Result<Bar, String> {
    let t = r.first().and_then(serde_json::Value::as_i64).ok_or("kline openTime not i64")?;
    let f = |i: usize, name: &str| -> Result<f64, String> {
        r.get(i)
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| format!("kline {name} not str"))?
            .parse::<f64>()
            .map_err(|e| format!("kline {name} parse: {e}"))
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

/// Pure map: a Binance/Aster klines JSON response body → `Vec<Bar>` (ascending by openTime, as the
/// venue serves it). No network, no venue delta — this is the fixture-tested seam.
pub fn parse_klines(body: &str) -> Result<Vec<Bar>, String> {
    let rows: Vec<Vec<serde_json::Value>> =
        serde_json::from_str(body).map_err(|e| format!("kline json: {e}"))?;
    rows.iter().map(|r| row_to_bar(r)).collect()
}

/// Build the `GET /klines` URL against `base` (a fully-resolved host+path from the caller's own host
/// resolution) for the given bounds and limit.
pub fn klines_url(
    base: &str,
    symbol: &str,
    interval: &str,
    start_ms: Option<i64>,
    end_ms: Option<i64>,
    limit: usize,
) -> String {
    let mut url = format!("{base}?symbol={symbol}&interval={interval}&limit={limit}");
    if let Some(s) = start_ms {
        url.push_str(&format!("&startTime={s}"));
    }
    if let Some(e) = end_ms {
        url.push_str(&format!("&endTime={e}"));
    }
    url
}

/// The live feed's warmup seed: the newest `limit` klines (no time bound), the last of which is the
/// still-forming bar. A single un-throttled request — preserves the legacy `fetch_seed` semantics.
/// `base` is the caller-resolved host+path (spot or perp, testnet or mainnet — that decision stays
/// with the venue); `venue` labels errors. The response shape is identical across venues, so
/// `parse_klines` is unchanged.
pub fn fetch_klines_latest(
    base: &str,
    symbol: &str,
    interval: &str,
    limit: usize,
    venue: &str,
) -> Result<Vec<Bar>, String> {
    let agent = vike_bridge_core::http::blocking_agent();
    let raw = get_raw(
        &agent,
        &klines_url(base, symbol, interval, None, None, limit),
        &format!("{venue} klines"),
        &[],
    )?;
    if (200..300).contains(&raw.status) {
        parse_klines(&raw.body)
    } else {
        Err(format!("{venue} klines HTTP {}: {}", raw.status, body_head(&raw.body)))
    }
}

/// Fetch ONE page with the rate-limit policy: on 429/418 honor `Retry-After` (else exponential
/// backoff), retrying the same page up to [`KlineRateLimit::max_rate_limit_retries`]. Returns the
/// page's bars and the server's reported used weight (for the caller's proactive cooldown). `base`
/// is the caller-resolved spot klines host+path (the paged range backfill stays spot-only).
fn fetch_page_rate_limited(
    agent: &ureq::Agent,
    base: &str,
    symbol: &str,
    interval: &str,
    start_ms: i64,
    end_ms: i64,
    spec: &KlineSpec,
) -> Result<(Vec<Bar>, Option<u64>), String> {
    let rl = &spec.rate_limit;
    let venue = spec.venue;
    let url = klines_url(base, symbol, interval, Some(start_ms), Some(end_ms), MAX_LIMIT);
    let policy = BackoffPolicy {
        max_retries: rl.max_rate_limit_retries,
        initial: rl.initial_backoff,
        max: rl.max_backoff,
    };
    // The backoff cadence is the shared `retry_rate_limited` driver (this loop was its
    // statement-for-statement twin — same sleeps, same messages); the classification (429/418 =
    // rate limited) stays Binance-grammar-specific here, and the page's `used_weight` rides out
    // through the `Verdict::Done` payload for the caller's proactive cooldown.
    retry_rate_limited(policy, &format!("{venue} klines"), || {
        let raw = get_raw(agent, &url, &format!("{venue} klines"), &[])?;
        match raw.status {
            200..=299 => Ok(Verdict::Done((parse_klines(&raw.body)?, raw.used_weight))),
            429 | 418 => Ok(Verdict::RateLimited {
                retry_after: raw.retry_after,
                note: format!("HTTP {} (rate limited)", raw.status),
            }),
            other => Err(format!("{venue} klines HTTP {other}: {}", body_head(&raw.body))),
        }
    })
}

/// The HOST of a `scheme://host/path` URL — the unit a published `REQUEST_WEIGHT` budget belongs
/// to, and therefore the only part of a discovery URL that has to match anything.
///
/// `None` for a string with no `://` or an empty authority: a URL this function cannot read is one
/// whose host cannot be compared, and [`discovery_url`] treats that as a mismatch (refuse, don't
/// assume). Port and userinfo are deliberately kept in the returned slice — they are part of the
/// endpoint's identity, and two URLs that differ in either are not obviously the same machine.
pub fn url_host(url: &str) -> Option<&str> {
    url.split_once("://")?.1.split('/').next().filter(|h| !h.is_empty())
}

/// The `exchangeInfo` URL a spec will ACTUALLY be asked for while paging `klines_base` — i.e.
/// [`KlineSpec::exchange_info_url`] after the one structural check a discovery URL can fail.
///
/// A published budget is a property of a HOST, so a spec may only be paced against a budget read
/// from the host it is paging. This function is where that is enforced, because it is the one place
/// that holds BOTH halves: the venue face resolves `base` and the spec independently, and nothing
/// downstream of here can tell that a 2400-weight fapi host was paired with spot's 6000, or that a
/// testnet backfill was paced against mainnet's budget.
///
/// A mismatch is a `warn` + `None`, never an error: the caller then paces on the fixed
/// [`KlineRateLimit::page_delay`], which is the same degrade path a network error takes. Refusing a
/// mispaired budget can only ever make a backfill SLOWER, which is the correct direction for a
/// pairing nobody can vouch for at runtime.
///
/// The venues' own tests drive this function per host — binance over its two static hosts, aster
/// over `Environment` x market — so "each spec discovers the budget of the host it pages" is one
/// property proven at the rung rather than a convention each venue restates.
pub fn discovery_url<'a>(spec: &'a KlineSpec, klines_base: &str) -> Option<&'a str> {
    let url = spec.exchange_info_url.as_deref()?;
    let info_host = url_host(url);
    let base_host = url_host(klines_base);
    if info_host.is_some() && info_host == base_host {
        return Some(url);
    }
    tracing::warn!(
        target: "vike_binance::family::klines",
        venue = spec.venue,
        exchange_info_url = url,
        klines_base,
        "kline rate discovery: the spec's exchangeInfo host is not the host being paged; \
         pacing on the fixed page_delay rather than another host's budget"
    );
    None
}

/// The ONE discovery request: `GET {exchange_info_url}` → its body, or `None`.
///
/// `None` on every failure mode, and it is never an error — a discovery miss must not fail a
/// backfill (the caller then paces on the fixed `page_delay`, which is what it did before discovery
/// existed). A spec with no `exchange_info_url` — or one whose URL fails [`discovery_url`]'s
/// same-host pairing against `klines_base` — returns `None` WITHOUT making a request, which is what
/// keeps an undiscoverable venue byte-identical.
///
/// Reuses the caller's existing paging agent rather than building a second one: same host, same
/// timeouts, one connection pool.
fn fetch_exchange_info(agent: &ureq::Agent, spec: &KlineSpec, klines_base: &str) -> Option<String> {
    let url = discovery_url(spec, klines_base)?;
    match get_raw(agent, url, &format!("{} exchangeInfo", spec.venue), &[]) {
        Ok(raw) if (200..300).contains(&raw.status) => Some(raw.body),
        Ok(raw) => {
            tracing::debug!(
                target: "vike_binance::family::klines",
                venue = spec.venue,
                url,
                status = raw.status,
                body = %body_head(&raw.body),
                "kline rate discovery: non-2xx exchangeInfo; pacing on the fixed page_delay"
            );
            None
        }
        Err(e) => {
            tracing::debug!(
                target: "vike_binance::family::klines",
                venue = spec.venue,
                url,
                error = %e,
                "kline rate discovery: exchangeInfo fetch failed; pacing on the fixed page_delay"
            );
            None
        }
    }
}

/// The PURE half of discovery: an `exchangeInfo` body (or `None`, for "we have none") → the pacer
/// the paging loop runs on. Split from the fetch so the whole degrade-to-fallback policy is testable
/// without a socket.
///
/// A body that parses to a `REQUEST_WEIGHT` budget gives a discovered pacer targeting
/// [`KlineSpec::utilization`] of it; anything else — no body, no `rateLimits`, no `REQUEST_WEIGHT`
/// row, malformed JSON — gives [`Pacer::fallback`], whose `next_delay` IS `page_delay` and whose
/// `should_cool_down` is always `false`.
fn pacer_for(spec: &KlineSpec, exchange_info_body: Option<&str>) -> Pacer {
    match exchange_info_body.and_then(parse_weight_budget) {
        Some(budget) => {
            tracing::debug!(
                target: "vike_binance::family::klines",
                venue = spec.venue,
                limit = budget.limit,
                interval_secs = budget.interval_secs,
                utilization = spec.utilization,
                "kline rate discovery: pacing against the venue's published REQUEST_WEIGHT budget"
            );
            Pacer::discovered(budget, spec.utilization)
        }
        None => Pacer::fallback(spec.rate_limit.page_delay),
    }
}

/// Estimated pages still to fetch from `cursor` to `end_ms` at the venue's [`MAX_LIMIT`] rows per
/// response — the ETA's multiplicand, and the only unknown in it.
///
/// The arithmetic (and its `None`/`0`/overflow contract) lives once in
/// [`vike_bridge_core::pacer::remaining_pages`], which the no-budget venues' pagers share; this is
/// the forward-walking caller's window→span binding, and nothing more. The tests below still pin the
/// numbers from THIS side, so a change to the shared helper cannot silently re-shape binance's ETA.
fn remaining_pages(cursor: i64, end_ms: i64, interval: &str) -> Option<u64> {
    vike_bridge_core::pacer::remaining_pages(end_ms.saturating_sub(cursor), interval, MAX_LIMIT)
}

/// The FORWARD page walk itself, over an INJECTED page fetcher — pure, and the seam that makes the
/// sequential and the concurrent pagers provably the same walk.
///
/// The twin of `vike_bybit::data::walk_backward_pages` (this family pages forward with
/// `startTime`/`endTime`; bybit/deribit walk `end` backward). `fetch_page(cursor, end)` returns one
/// page's bars ascending; the walk clips to the inclusive `[start_ms, end_ms]` window, advances the
/// cursor one ms past each page's last `openTime`, and stops on any of three conditions — an EMPTY
/// page, a SHORT page (fewer than `rows_per_page` ⇒ the window is exhausted, since `startTime`/
/// `endTime` is a filter rather than a grid, so a gap in the venue's own history still yields a full
/// page while any rows remain), or a cursor that fails to move forward.
///
/// It never sleeps and never touches the network: the throttle, the timing and the pace observation
/// all belong to the caller's `fetch_page` closure, which is what lets a test walk many pages
/// instantly AND lets the concurrent pager swap the closure's throttle for a shared gate without
/// touching a line of the walk.
pub fn walk_forward_pages<F>(
    start_ms: i64,
    end_ms: i64,
    rows_per_page: usize,
    mut fetch_page: F,
) -> Result<Vec<Bar>, String>
where
    F: FnMut(i64, i64) -> Result<Vec<Bar>, String>,
{
    let mut out: Vec<Bar> = Vec::new();
    let mut cursor = start_ms;
    while cursor <= end_ms {
        let page = fetch_page(cursor, end_ms)?;
        if page.is_empty() {
            break;
        }
        let page_len = page.len();
        let last_ts = page.last().map(|b| b.ts).unwrap_or(cursor);
        for b in page {
            if start_ms <= b.ts && b.ts <= end_ms {
                out.push(b);
            }
        }
        // Advance strictly past the last openTime; bail if the window can't move forward or the page
        // was short (fewer than the cap ⇒ the window is exhausted).
        let next = last_ts.saturating_add(1);
        if next <= cursor || page_len < rows_per_page {
            break;
        }
        cursor = next;
    }
    Ok(out)
}

/// How many pages [`fetch_klines_range_lanes`] walks SEQUENTIALLY before splitting the remainder.
///
/// **TWO, and the second is not padding.** [`vike_bridge_core::pacer::Pacer::observe`] can only infer
/// a per-request weight from a DELTA, so the first reading establishes a baseline and infers nothing:
/// after ONE probe page `per_request` is still the pacer's pessimistic seed of 5. On binance spot
/// that reads a weight-2 endpoint as weight-5 and costs twice over — it derives **3** lanes where the
/// venue's own budget affords 6, AND it paces the whole lane phase at a 125 ms gap instead of 50 ms,
/// i.e. at 40 % of the 40 % the operator asked for. The second page is the one that measures the
/// venue rather than the constant.
///
/// It costs NO extra request: page two is a page this backfill was going to fetch regardless,
/// fetched sequentially instead of inside a lane. And the probe phase is the ONLY window in which a
/// counter delta means anything at all, because it is the only one with a single request outstanding
/// — see [`vike_bridge_core::pacer::Pacer::observe_absolute`]. Lanes therefore engage only on a
/// window of three pages or more, which is the point at which they could matter.
pub const PROBE_PAGES: usize = 2;

// The rule that const encodes, enforced by the BUILD rather than by a comment — the same
// `const _: ()` discipline `vike_model::venue_rate_limits` applies to its per-row invariants. One
// page cannot produce a delta, so one page cannot measure a per-request weight, so a lane count
// derived from one page is the pacer's seed constant wearing a measurement's clothes.
const _: () = assert!(PROBE_PAGES >= 2, "a per-request weight is a delta; one reading is not one");

/// Walk at most `max_pages` sequentially from `start_ms`, returning the bars and WHERE THE WALK GOT
/// TO — `Some(cursor)` when the page budget ran out with the window still open, `None` when the
/// window is exhausted and there is nothing left for a caller to split.
///
/// The stop conditions are [`walk_forward_pages`]'s three, verbatim (empty page / short page /
/// non-advancing cursor), and `max_pages = usize::MAX` makes this that function with a cursor
/// returned alongside — pinned by `an_unbounded_probe_is_the_sequential_walk`. Splitting it out is
/// what lets the probe phase's page budget, its termination and its hand-off be tested without a
/// socket.
fn probe_pages<F>(
    start_ms: i64,
    end_ms: i64,
    rows_per_page: usize,
    max_pages: usize,
    mut fetch_page: F,
) -> Result<(Vec<Bar>, Option<i64>), String>
where
    F: FnMut(i64, i64) -> Result<Vec<Bar>, String>,
{
    let mut out: Vec<Bar> = Vec::new();
    let mut cursor = start_ms;
    for _ in 0..max_pages {
        if cursor > end_ms {
            return Ok((out, None));
        }
        let page = fetch_page(cursor, end_ms)?;
        if page.is_empty() {
            return Ok((out, None));
        }
        let page_len = page.len();
        let last_ts = page.last().map(|b| b.ts).unwrap_or(cursor);
        for b in page {
            if start_ms <= b.ts && b.ts <= end_ms {
                out.push(b);
            }
        }
        let next = last_ts.saturating_add(1);
        if next <= cursor || page_len < rows_per_page {
            return Ok((out, None));
        }
        cursor = next;
    }
    // The page budget ran out with the window still open: tell the caller where to resume.
    Ok((out, (cursor <= end_ms).then_some(cursor)))
}

/// Fetch closed-kline history for the inclusive `[start_ms, end_ms]` window, paging through the
/// 1000-rows/response cap under the module's rate-limit policy (paced throttle + weight-aware
/// cooldown + 429/418 `Retry-After` backoff). Returns bars ascending by openTime with no cross-page
/// duplicates (each page starts one ms after the previous page's last `openTime`). `base` is the
/// caller-resolved spot klines host+path; `spec` carries the venue label, the fallback rate-limit
/// budget and (optionally) where the venue publishes its own. Network I/O.
///
/// Pacing: ONE `exchangeInfo` GET before the loop (see the module doc's "Discovered pacing"), then
/// every page is WALL-CLOCKED and its `X-MBX-USED-WEIGHT-1M` fed back into the pacer, which infers
/// the per-request weight from the deltas, subtracts the measured request time from the next sleep,
/// and widens the gap by itself if the venue re-prices the endpoint. With no discovered budget the
/// pacer answers a constant `page_delay` and never cools down, so this loop's REQUESTS and SLEEPS
/// are exactly what they were pre-discovery — but the durations are still measured, still reported
/// on the ETA line, and still returned by [`fetch_klines_range_paced`]; a `Fixed` pacer withholds
/// only the steering (see [`vike_bridge_core::pacer`]'s module doc).
pub fn fetch_klines_range(
    base: &str,
    symbol: &str,
    interval: &str,
    start_ms: i64,
    end_ms: i64,
    spec: &KlineSpec,
) -> Result<Vec<Bar>, String> {
    fetch_klines_range_paced(base, symbol, interval, start_ms, end_ms, spec, None)
        .map(|(bars, _pace)| bars)
}

/// [`fetch_klines_range`] with the pace measurement carried ACROSS runs: `seed` is the PREVIOUS
/// run's observation (or `None`, which is byte-identical to [`fetch_klines_range`]), and the second
/// element of the return is THIS run's — `None` unless a page was actually timed, so a run that
/// fetched nothing cannot report a fabricated pace.
///
/// The seed is offered to the pacer AFTER discovery has run, never before, because the pacer refuses
/// a sample whose `budget_per_min` is not the one it just discovered (`Pacer::seed`'s rule 3) — that
/// ordering is what makes a stored record unable to move the venue's budget, only the sleep inside
/// it. A refused seed is logged at `debug` and changes nothing.
///
/// The caller owns the persistence AND the key it stores under: this function knows the venue label
/// but not whether `base` is the spot or the perp host, and those two have different budgets and
/// different per-request weights (see [`KlineSpec::exchange_info_url`]).
pub fn fetch_klines_range_paced(
    base: &str,
    symbol: &str,
    interval: &str,
    start_ms: i64,
    end_ms: i64,
    spec: &KlineSpec,
    seed: Option<&PaceSample>,
) -> Result<(Vec<Bar>, Option<PaceSample>), String> {
    let agent = vike_bridge_core::http::blocking_agent();
    // ONCE per call, never per page: the budget is a property of the host, not of the window.
    let mut pacer = pacer_for(spec, fetch_exchange_info(&agent, spec, base).as_deref());
    seed_pacer(&mut pacer, spec, seed);
    paged_walk(&agent, pacer, base, symbol, interval, start_ms, end_ms, spec)
}

/// Offer a persisted [`PaceSample`] to a freshly-built pacer, and log which of the two answers it
/// gave. Hoisted out of [`fetch_klines_range_paced`] only so its lane-capable sibling can do the
/// identical thing at the identical point in the sequence (AFTER discovery — see that function's doc
/// for why the ordering is load-bearing).
fn seed_pacer(pacer: &mut Pacer, spec: &KlineSpec, seed: Option<&PaceSample>) {
    let Some(sample) = seed else { return };
    let applied = pacer.seed(sample);
    tracing::debug!(
        target: "vike_binance::family::klines",
        venue = spec.venue,
        applied,
        seed_request_ms = sample.request_ms,
        seed_weight = sample.per_request_weight,
        seed_budget = ?sample.budget_per_min,
        discovered_budget = ?pacer.budget_per_minute(),
        "kline pacing: persisted pace offered to the pacer"
    );
}

/// The SEQUENTIAL paged walk over an already-built agent and an already-seeded pacer — the whole
/// body [`fetch_klines_range_paced`] used to be, unchanged in requests and sleeps.
///
/// It is a separate function for exactly one reason: [`fetch_klines_range_lanes`] must be able to
/// fall back to it AFTER it has already paid for discovery, when discovery did not answer. Delegating
/// to `fetch_klines_range_paced` instead would re-issue the `exchangeInfo` GET that just failed and
/// build a second agent, for a path that is degraded already.
#[allow(clippy::too_many_arguments)] // the agent + the pacer on top of the existing seven
fn paged_walk(
    agent: &ureq::Agent,
    mut pacer: Pacer,
    base: &str,
    symbol: &str,
    interval: &str,
    start_ms: i64,
    end_ms: i64,
    spec: &KlineSpec,
) -> Result<(Vec<Bar>, Option<PaceSample>), String> {
    let mut first = true;
    // The pace report is emitted ONCE, after the first page that actually returned rows.
    let mut report_pace = true;
    // The walk (cursor advance / clipping / the three stop conditions) is `walk_forward_pages`; this
    // closure is the THROTTLE + MEASURE + REPORT half, statement for statement what the loop body
    // used to be. Same requests, same sleeps: the only reordering is that the proactive cooldown now
    // runs before the page's bars are pushed into the output rather than after, which is two
    // in-memory `Vec::push`es apart and issues nothing.
    let bars = walk_forward_pages(start_ms, end_ms, MAX_LIMIT, |cursor, window_end| {
        if !first {
            std::thread::sleep(pacer.next_delay()); // paced throttle between page requests
        }
        first = false;

        // Wall-clock the request: `next_delay()` is only half the gap the venue sees, and this is
        // the half the pacer cannot measure for itself. It spans `fetch_page_rate_limited`, so a
        // page that was 429'd and retried reports its retry sleeps as "request time" too — bounded
        // and self-correcting (see `Pacer::observe_request`), and un-separable without reaching
        // inside the shared retry driver.
        let started = std::time::Instant::now();
        let (page, used_weight) =
            fetch_page_rate_limited(agent, base, symbol, interval, cursor, window_end, spec)?;
        let elapsed = started.elapsed();
        // Observe BEFORE the empty-page break: the weight (and the time) was spent whether or not
        // rows came back.
        pacer.observe_request(used_weight, elapsed);
        if page.is_empty() {
            return Ok(page); // the walk stops here — no report, no cooldown, exactly as before
        }
        // ONE line per backfill, not per page — this loop runs hundreds of times.
        //
        // Emitted in BOTH modes. It used to be gated on `pacer.is_discovered()`, on the reasoning
        // that a fallback pager's pace is the caller's own `page_delay` const and there was "nothing
        // measured to report". Half of that was always false: the request time was measured, and it
        // is the half that dominates (measured 476–486 ms per okx page against a 200 ms delay). A
        // fallback backfill printing NOTHING is exactly the multi-minute silence this line exists to
        // break, so `discovered` becomes a FIELD rather than a gate. The sleeps are untouched — only
        // the log is.
        if report_pace {
            report_pace = false;
            // Both halves are `Option` on purpose: an unparseable interval has no page count, and a
            // pacer with nothing measured refuses to guess an ETA. Either ⇒ no line, not a fake one.
            let pages = remaining_pages(cursor, window_end, interval);
            if let Some((pages, eta)) = pages.zip(pages.and_then(|n| pacer.eta(n))) {
                tracing::info!(
                    target: "vike_binance::family::klines",
                    venue = spec.venue,
                    symbol,
                    interval,
                    // `false` ⇒ `next_delay_ms` IS `rate_limit.page_delay` and `page_weight` is the
                    // pacer's un-steering seed, not a budget fraction. Read the line accordingly.
                    discovered = pacer.is_discovered(),
                    request_ms = elapsed.as_millis() as u64,
                    page_weight = pacer.per_request_weight(),
                    used_weight,
                    next_delay_ms = pacer.next_delay().as_millis() as u64,
                    remaining_pages = pages,
                    eta_secs = eta.as_secs(),
                    "kline backfill pace: measured request time + ETA"
                );
            }
        }

        // Proactive cooldown to stay clear of the 429 threshold. TWO independent guards, either of
        // which is enough: the venue-agnostic hand-set `weight_soft_limit` (unchanged, and the ONLY
        // one that can fire without discovery), and — when a budget WAS discovered — the pacer's own
        // "this window's targeted share is spent" verdict. In fallback mode `should_cool_down()` is
        // always `false`, so this condition is byte-identical to the pre-discovery one there.
        if used_weight.is_some_and(|w| w >= spec.rate_limit.weight_soft_limit)
            || pacer.should_cool_down()
        {
            std::thread::sleep(spec.rate_limit.weight_cooldown);
        }
        Ok(page)
    })?;
    Ok((bars, pacer.measured()))
}

/// The CONCURRENT half of the walk, pure and network-free: split `[start_ms, end_ms]` into `lanes`
/// disjoint ascending spans ([`vike_bridge_core::concurrent::split_range`]) and run
/// [`walk_forward_pages`] over each, merged ascending and deduped by `ts`.
///
/// `fetch_page(cursor, window_end)` is the SAME closure shape the sequential walk takes, so the two
/// paths are provably the same walk over the same page source — which is exactly what the
/// `lanes_return_the_same_bars_as_the_sequential_walk` test below asserts, over dense history and
/// over every gap shape (leading, interior, trailing, sparse) that could make them disagree.
///
/// `lanes <= 1` is one span, i.e. one inline [`walk_forward_pages`] on the caller's own thread over
/// the caller's own window — not merely equivalent to the sequential walk but literally it.
///
/// The abort check between pages is what bounds the cost of a failure to one round trip per lane. A
/// lane that stops on it returns a PARTIAL page set, which is sound only because
/// [`vike_bridge_core::concurrent::run_lanes`] sets that flag exclusively when some lane already
/// returned `Err` — so the whole call fails and every lane's rows are discarded.
pub fn walk_forward_spans<F>(
    start_ms: i64,
    end_ms: i64,
    rows_per_page: usize,
    lanes: usize,
    fetch_page: F,
) -> Result<Vec<Bar>, String>
where
    F: Fn(i64, i64) -> Result<Vec<Bar>, String> + Sync,
{
    let spans = vike_bridge_core::concurrent::split_range(start_ms, end_ms, lanes);
    let mut out = vike_bridge_core::concurrent::run_lanes(&spans, |flag, span_start, span_end| {
        walk_forward_pages(span_start, span_end, rows_per_page, |cursor, window_end| {
            if flag.aborted() {
                return Ok(Vec::new());
            }
            fetch_page(cursor, window_end)
        })
    })?;
    // Insurance, not a correction: spans are disjoint and ascending and each walk returns its own
    // window ascending, so this is already sorted and unique. It is the same discipline
    // `vike_bybit::data::walk_backward_pages` applies, and it is what makes "the concurrent result IS
    // the sequential result" a property of this function rather than of the caller.
    out.sort_by_key(|b| b.ts);
    out.dedup_by_key(|b| b.ts);
    Ok(out)
}

/// [`fetch_klines_range_paced`] with BOUNDED CONCURRENCY: the same walk, the same pages, the same
/// aggregate rate — several windows in flight instead of one.
///
/// ## Why this exists (MEASURED, the CI box 2026-08-04)
/// A sequential pager cannot space requests closer together than one request TAKES (~280 ms through
/// the pooled agent). On binance **fapi** that is irrelevant: 2400 weight/min at the 0.40 default and
/// weight-5 pages is a 312 ms target gap, wider than the round trip, so one request at a time already
/// hits the target and this function derives **one lane** and changes nothing. On binance **spot** it
/// is the whole story: 6000 weight/min at weight 2 is a **50 ms** target gap,
/// [`vike_bridge_core::pacer::Pacer::next_delay`] is already floored at 1 ms, and the backfill
/// delivers ~18 % of the budget the operator asked for with no knob left to turn.
///
/// ## What is and is not raised
/// **The target rate is unchanged.** Every lane takes its dispatch slot from ONE shared
/// [`vike_bridge_core::concurrent::LaneGate`], which admits one request per
/// [`vike_bridge_core::pacer::Pacer::target_gap`] however many lanes are waiting — so the venue sees
/// the same weight/min at six lanes as at one, and the pacer's two proactive guards (the hand-set
/// `weight_soft_limit` and `should_cool_down`) pause EVERY lane at once when the counter crosses.
/// What concurrency changes is only that the target becomes *reachable*.
///
/// The lane COUNT is derived from the pacer's own measurement
/// ([`vike_bridge_core::pacer::Pacer::suggested_lanes`] = `ceil(request_time / target_gap)`), capped
/// at `max_lanes` — and the measurement it reads is the one [`PROBE_PAGES`] pages produced, which is
/// why there are TWO of them rather than one. On a cold run with a single probe the estimate is
/// still the pacer's pessimistic seed and spot derives **3** lanes on a **125 ms** gap; measured, it
/// derives 6 on 50 ms.
///
/// ## What this is actually worth (ARITHMETIC, not a measurement)
/// Sequentially, spot manages `60 / (0.280 + 0.001)` = **213 requests/min** — the round trip plus the
/// floored sleep. Concurrently the gate admits one per 50 ms target gap = **1200/min**, so a
/// 24-month 1m spot backfill (1052 pages) goes from **~4.9 min to ~55 s**, about **5.4x**. A longer
/// run settles nearer **4.8x**: once consumption actually reaches the 2400 weight/min target,
/// `should_cool_down` fires near the end of each venue window and adds a `weight_cooldown` pause the
/// ETA does not model, costing roughly 10 s in every 70. Perp is **unchanged** — it derives one lane.
/// ⚠ Both figures are arithmetic from the measured round trip and the published budget. Nothing here
/// has been timed end to end.
///
/// ⚠ **No discovered budget ⇒ the SEQUENTIAL pager, not a one-lane concurrent one.** Discovery is a
/// runtime question: a spec naming an `exchange_info_url` still degrades to a `Fixed` pacer on a
/// network error or an unparseable body, and a `Fixed` pacer's `target_gap` is the caller's hand-set
/// `page_delay` — which a shared gate would space requests by, where the sequential pager spaces them
/// `page_delay + request` apart. Driving the gate off it would be a ~1.5x rate increase against a
/// venue whose ceiling we had just failed to read. So this function checks `is_discovered()` before
/// any lane exists and hands the whole fetch back to `paged_walk`. Aster — whose
/// `exchange_info_url` is `None` because its host is env-resolved — takes the same exit, by the same
/// rule rather than by name.
///
/// ## The probe page
/// The first page is fetched SEQUENTIALLY, exactly as `fetch_klines_range_paced`'s is, for three
/// reasons that all matter: it is what MEASURES the round trip the lane count is a ratio of; it emits
/// the one ETA line; and its last `openTime` is where real history begins, so the split covers only
/// the window that still has data instead of spending lanes on a symbol's pre-listing void. A probe
/// that comes back empty or short ends the fetch with no lane ever spawned.
///
/// ## Ordering, dedup and failure
/// Spans are disjoint and ascending and each lane's walk returns its own window ascending, so the
/// concatenation is already ordered; it is nonetheless sorted and deduped by `ts` (the same
/// insurance `walk_backward_pages` applies), so the result is the identical `Vec<Bar>` the sequential
/// path produces — ascending, no duplicates, clipped to `[start_ms, end_ms]`. **Any lane failure
/// fails the whole call** and discards every row: this window is written under ONE commit key, so a
/// partial result reported as success would mark it permanently ingested.
///
/// `max_lanes <= 1` delegates to [`fetch_klines_range_paced`] before anything else happens — same
/// function, same agent, same requests, same sleeps.
// `max_lanes` is the 8th parameter and pushed this one past clippy's default threshold. Bundling it
// with `seed` into a params struct would hide the two things a caller must think about (which pace
// record, and how many lanes) behind a type that exists only to satisfy an arity count.
#[allow(clippy::too_many_arguments)]
pub fn fetch_klines_range_lanes(
    base: &str,
    symbol: &str,
    interval: &str,
    start_ms: i64,
    end_ms: i64,
    spec: &KlineSpec,
    seed: Option<&PaceSample>,
    max_lanes: usize,
) -> Result<(Vec<Bar>, Option<PaceSample>), String> {
    if max_lanes <= 1 {
        // Not "equivalent to" the sequential path — it IS the sequential path.
        return fetch_klines_range_paced(base, symbol, interval, start_ms, end_ms, spec, seed);
    }
    if end_ms < start_ms {
        return Ok((Vec::new(), None)); // an empty window never touches the venue
    }

    // Sized for the lanes up front: ureq pools only 3 idle connections per host by default, and a
    // surplus lane would pay a fresh TLS handshake per page — which would also inflate the very
    // round-trip measurement the lane count is derived from.
    let agent = vike_bridge_core::http::blocking_agent_for_lanes(max_lanes);
    // ONCE per call, never per page: the budget is a property of the host, not of the window.
    let mut pacer = pacer_for(spec, fetch_exchange_info(&agent, spec, base).as_deref());
    seed_pacer(&mut pacer, spec, seed);

    // ⚠ Discovery is a RUNTIME question, not a static one: a spec that names an `exchange_info_url`
    // still falls back to a `Fixed` pacer on a network error, a non-2xx, or an unparseable body. A
    // `Fixed` pacer's `target_gap` is the caller's hand-set `page_delay`, and a shared gate paced on
    // THAT would space requests `page_delay` apart where the sequential pager spaces them
    // `page_delay + request` apart — a ~1.5x rate increase against a venue whose ceiling we just
    // failed to read. So a discovery miss is not a slower concurrent path; it is the SEQUENTIAL path.
    if !pacer.is_discovered() {
        tracing::debug!(
            target: "vike_binance::family::klines",
            venue = spec.venue,
            symbol,
            "kline lanes: no discovered budget this run ⇒ falling back to the sequential pager"
        );
        return paged_walk(&agent, pacer, base, symbol, interval, start_ms, end_ms, spec);
    }

    // ---- the probe phase: SEQUENTIAL, and every lane decision rests on what it measures ----------
    // Two pages, not one — see `PROBE_PAGES`. This is also the only window in which a used-weight
    // delta is attributable to a request, because it is the only one with a single request
    // outstanding (see `vike_bridge_core::pacer::Pacer::observe_absolute`).
    let mut first = true;
    let mut probe_elapsed = Duration::ZERO;
    let mut probe_weight = None;
    let (mut out, resume) =
        probe_pages(start_ms, end_ms, MAX_LIMIT, PROBE_PAGES, |cursor, window_end| {
            if !first {
                std::thread::sleep(pacer.next_delay()); // the sequential throttle, unchanged
            }
            first = false;
            let started = std::time::Instant::now();
            let (page, used_weight) =
                fetch_page_rate_limited(&agent, base, symbol, interval, cursor, window_end, spec)?;
            let elapsed = started.elapsed();
            // `observe_request`, not `observe_absolute`: one request was outstanding, so the delta
            // between this reading and the previous one IS this request's cost. That is the whole
            // reason the probe is sequential and the whole reason there are two of it.
            pacer.observe_request(used_weight, elapsed);
            probe_elapsed = elapsed;
            probe_weight = used_weight;
            if !page.is_empty()
                && (used_weight.is_some_and(|w| w >= spec.rate_limit.weight_soft_limit)
                    || pacer.should_cool_down())
            {
                std::thread::sleep(spec.rate_limit.weight_cooldown);
            }
            Ok(page)
        })?;
    // `None` = the probe exhausted the window (empty page, short page, or a cursor that could not
    // move forward) — there is nothing left to split and no lane is ever spawned.
    let Some(cursor) = resume else {
        return Ok((out, pacer.measured()));
    };

    let lanes = pacer.suggested_lanes(max_lanes);
    // The ONE line per backfill, now also carrying what the measurement bought.
    let pages = remaining_pages(cursor, end_ms, interval);
    if let Some((pages, eta)) = pages.zip(pages.and_then(|n| pacer.eta(n))) {
        tracing::info!(
            target: "vike_binance::family::klines",
            venue = spec.venue,
            symbol,
            interval,
            discovered = pacer.is_discovered(),
            request_ms = probe_elapsed.as_millis() as u64,
            // MEASURED by the probe's SECOND page, not the pacer's seed constant — that difference
            // is the lane count (3 vs 6 on spot) and the gate's own gap (125 ms vs 50 ms).
            page_weight = pacer.per_request_weight(),
            used_weight = probe_weight,
            target_gap_ms = pacer.target_gap().as_millis() as u64,
            lanes,
            remaining_pages = pages,
            // The SEQUENTIAL ETA — what this backfill would have cost one window at a time. The
            // concurrent one is roughly this over `lanes`, but only roughly (the venue's own history
            // gaps decide how much work each span actually holds), so the honest thing to print is
            // the number that is not a guess.
            sequential_eta_secs = eta.as_secs(),
            "kline backfill pace: measured request time + derived lane count"
        );
    }

    // ---- the lanes -----------------------------------------------------------------------------
    let gate = vike_bridge_core::concurrent::LaneGate::new(pacer);
    let rest = walk_forward_spans(cursor, end_ms, MAX_LIMIT, lanes, |cursor, window_end| {
        gate.admit(); // the shared slot — this is where the aggregate budget is enforced
        let started = std::time::Instant::now();
        let (page, used_weight) =
            fetch_page_rate_limited(&agent, base, symbol, interval, cursor, window_end, spec)?;
        let elapsed = started.elapsed();
        // Observe + BOTH proactive guards, applied to the SHARED schedule so a cooldown pauses every
        // lane once rather than each lane separately.
        gate.complete(
            used_weight,
            elapsed,
            spec.rate_limit.weight_soft_limit,
            spec.rate_limit.weight_cooldown,
        );
        Ok(page)
    })?;

    out.extend(rest);
    // The probe's bars all precede `cursor`, and `walk_forward_spans` already returned its half
    // sorted and deduped — so this is insurance over an already-ordered concatenation, at the one
    // function that PROMISES an ascending, duplicate-free `Vec<Bar>`.
    out.sort_by_key(|b| b.ts);
    out.dedup_by_key(|b| b.ts);
    Ok((out, gate.pacer().measured()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use vike_model::rate_limits::DEFAULT_UTILIZATION;

    // A generic base for the pure-URL/parse tests. Venue-specific host-resolution tests live in
    // each venue's own `data.rs` (proven per-venue), since the resolved host IS the per-venue delta.
    const BASE: &str = "https://host/api/v3/klines";

    /// A trimmed `https://api.binance.com/api/v3/exchangeInfo` body — the `rateLimits` array
    /// verbatim, plus enough envelope to prove the parse reads a real response and not a hand-shaped
    /// fragment. Same capture (2026-08-04, from the CI box) as
    /// `vike_bridge_core::rate_discovery`'s fixtures; duplicated as a literal here because those are
    /// `#[cfg(test)]` consts private to that crate, and this rung must be provable on its own.
    const SPOT_EXCHANGE_INFO: &str = r#"{
      "timezone": "UTC",
      "rateLimits": [
        {"rateLimitType":"REQUEST_WEIGHT","interval":"MINUTE","intervalNum":1,"limit":6000},
        {"rateLimitType":"ORDERS","interval":"SECOND","intervalNum":10,"limit":100},
        {"rateLimitType":"RAW_REQUESTS","interval":"MINUTE","intervalNum":5,"limit":300000}
      ],
      "symbols": [{"symbol":"BTCUSDT","status":"TRADING"}]
    }"#;

    /// The `https://fapi.binance.com/fapi/v1/exchangeInfo` twin — `REQUEST_WEIGHT` 2400, NOT spot's
    /// 6000. Aster's `fapi` serves a byte-identical array.
    const FAPI_EXCHANGE_INFO: &str = r#"{
      "timezone": "UTC",
      "rateLimits": [
        {"rateLimitType":"REQUEST_WEIGHT","interval":"MINUTE","intervalNum":1,"limit":2400},
        {"rateLimitType":"ORDERS","interval":"MINUTE","intervalNum":1,"limit":1200}
      ],
      "symbols": [{"symbol":"BTCUSDT","status":"TRADING","contractType":"PERPETUAL"}]
    }"#;

    fn spec(page_delay_ms: u64, exchange_info_url: Option<&'static str>) -> KlineSpec {
        KlineSpec {
            venue: "testvenue",
            rate_limit: KlineRateLimit {
                page_delay: Duration::from_millis(page_delay_ms),
                weight_soft_limit: 2000,
                weight_cooldown: Duration::from_secs(10),
                max_rate_limit_retries: 6,
                initial_backoff: Duration::from_secs(1),
                max_backoff: Duration::from_secs(60),
            },
            exchange_info_url: exchange_info_url.map(Cow::Borrowed),
            utilization: DEFAULT_UTILIZATION,
        }
    }

    /// Weight actually spent per minute at a given inter-request `delay` and per-request weight —
    /// the quantity the pacer steers, and the only honest thing to assert against a venue ceiling.
    fn weight_per_minute(delay: Duration, per_request: f64) -> f64 {
        (60.0 / delay.as_secs_f64()) * per_request
    }

    /// The byte-identical-behaviour contract for every venue that publishes nothing
    /// (bybit/okx/deribit, and aster while its host stays env-resolved): the pager sleeps exactly
    /// `page_delay`, forever, and never cools down on the pacer's account.
    #[test]
    fn a_spec_without_discovery_paces_on_the_fixed_page_delay() {
        let s = spec(150, None);
        let mut p = pacer_for(&s, None);
        assert_eq!(p.next_delay(), s.rate_limit.page_delay);
        assert!(!p.should_cool_down(), "no budget ⇒ no target ⇒ the second guard stays silent");
        // Observations are still fed in by the loop; in fallback mode they must change nothing.
        p.observe(100);
        p.observe(2_500);
        assert_eq!(p.next_delay(), s.rate_limit.page_delay);
        assert!(!p.should_cool_down());
    }

    /// A `None` URL makes NO request — the early `?` in `fetch_exchange_info` is the whole reason an
    /// undiscoverable venue is byte-identical rather than merely equivalent. (A live agent is built
    /// here precisely to show it is never used.)
    ///
    /// The MISPAIRED case takes the identical exit and is asserted the same way: a spec naming a
    /// budget on some other host issues nothing either, so the refusal costs a backfill one fixed
    /// `page_delay` and never a request against a host it was not paging.
    #[test]
    fn a_none_url_never_reaches_the_network() {
        let agent = vike_bridge_core::http::blocking_agent();
        assert_eq!(fetch_exchange_info(&agent, &spec(150, None), BASE), None);
        assert_eq!(
            fetch_exchange_info(
                &agent,
                &spec(150, Some("https://elsewhere.example/api/v3/exchangeInfo")),
                BASE
            ),
            None
        );
    }

    /// The pairing gate itself, over the shapes a venue face can hand in. It is `pub` and both
    /// venues' `data.rs` tests drive it against their OWN hosts (binance: spot vs fapi; aster:
    /// `Environment` x market), so this proves the RULE and they prove their hosts obey it.
    #[test]
    fn discovery_is_refused_unless_it_names_the_host_being_paged() {
        // Same host, different path ⇒ accepted, and the URL comes back verbatim.
        let same = spec(150, Some("https://host/api/v3/exchangeInfo"));
        assert_eq!(discovery_url(&same, BASE), Some("https://host/api/v3/exchangeInfo"));

        // Another host ⇒ refused. This is the binance spot-vs-fapi and the aster
        // testnet-vs-mainnet defect, in the one place that can see both halves.
        for other in [
            "https://otherhost/api/v3/exchangeInfo",
            "https://host.evil/api/v3/exchangeInfo",
            "https://host:8443/api/v3/exchangeInfo", // a port IS part of the endpoint's identity
            // ...and the scheme is NOT: this gate answers "same host", not "same scheme". No venue
            // face composes an http:// discovery URL, and inventing a scheme rule here would be an
            // untested policy on a shape nothing serves.
            "http://host/api/v3/exchangeInfo",
        ] {
            let s = spec(150, Some(other));
            let got = discovery_url(&s, BASE);
            let expected = if url_host(other) == url_host(BASE) { Some(other) } else { None };
            assert_eq!(got, expected, "{other} against {BASE}");
        }

        // An unreadable URL on either side is a REFUSAL, never an assumption.
        assert_eq!(discovery_url(&spec(150, Some("no-scheme/exchangeInfo")), BASE), None);
        assert_eq!(discovery_url(&same, "no-scheme/klines"), None);
        // ...and `None` in stays `None` out, with nothing to mispair.
        assert_eq!(discovery_url(&spec(150, None), BASE), None);
    }

    /// `url_host` reads the authority and nothing else — the unit a published budget belongs to.
    #[test]
    fn url_host_reads_the_authority_only() {
        assert_eq!(url_host("https://api.binance.com/api/v3/klines"), Some("api.binance.com"));
        assert_eq!(
            url_host("https://fapi.asterdex-testnet.com"),
            Some("fapi.asterdex-testnet.com")
        );
        assert_eq!(url_host("wss://host:9443/ws"), Some("host:9443"));
        assert_eq!(url_host("https:///path"), None); // empty authority
        assert_eq!(url_host("api.binance.com/klines"), None); // no scheme ⇒ unreadable
        assert_eq!(url_host(""), None);
    }

    /// Discovery on the real SPOT body: 6000/min at the 40 % default is 2400 weight/min, and once
    /// the measured weight-2 cost of `/api/v3/klines?limit=1000` is observed that lands on a 50 ms
    /// gap — INDEPENDENTLY reproducing the number `BINANCE_KLINE_SPOT.page_delay` was hand-measured
    /// to. The point is not the coincidence: it is that the venue now supplies it.
    #[test]
    fn the_spot_budget_reproduces_the_hand_measured_pace() {
        let s = spec(50, Some("https://api.binance.com/api/v3/exchangeInfo"));
        let mut p = pacer_for(&s, Some(SPOT_EXCHANGE_INFO));
        p.observe(10);
        p.observe(12); // `/klines?limit=1000` costs weight 2 on spot
        let d = p.next_delay();
        assert!((d.as_secs_f64() - 0.05).abs() < 1e-6, "expected ~50ms, got {d:?}");
        assert!(
            weight_per_minute(d, 2.0) <= 6000.0,
            "must pace UNDER the venue's published ceiling"
        );
    }

    /// The same on FAPI, where the ceiling is 2400 and a page costs weight 5.
    ///
    /// ⚠ This asserts the UNMEASURED pace — the whole target gap slept, which is what the pacer
    /// answers until a caller reports a request duration. Read end-to-end the two differ a lot: the
    /// hand-set 150 ms delay looks like 2000 weight/min (83 % of the ceiling) by this maths, but a
    /// MEASURED 24-month backfill at that delay left `x-mbx-used-weight-1m` at **556** (~23 %),
    /// because each request's ~280 ms round trip dominates the sleep. So "discovery derives
    /// ~312 ms, therefore 150 ms is 2x too fast" is a false conclusion — both are safe, and a
    /// sleep-only pacer systematically UNDER-shoots its utilization target. That gap is what
    /// [`Pacer::observe_request`] closes; see the sibling test below for the pace this loop
    /// actually runs at once the first page has been timed.
    #[test]
    fn a_discovered_budget_paces_below_the_venue_ceiling() {
        let s = spec(150, Some("https://fapi.binance.com/fapi/v1/exchangeInfo"));
        let mut p = pacer_for(&s, Some(FAPI_EXCHANGE_INFO));
        p.observe(100);
        p.observe(105); // weight 5
        let d = p.next_delay();
        let spent = weight_per_minute(d, 5.0);
        assert!(spent <= 2400.0, "paced {spent} weight/min against a 2400 ceiling");
        assert!((spent - 960.0).abs() < 1.0, "40% of 2400 is 960 weight/min, got {spent}");
        assert!(
            d > s.rate_limit.page_delay,
            "the venue's own budget is STRICTER than the hand-set 150ms: {d:?}"
        );
    }

    /// Every unusable body degrades to the fallback — a discovery miss must never fail, or slow, a
    /// backfill in some third way.
    #[test]
    fn an_unusable_exchange_info_body_falls_back_to_page_delay() {
        let s = spec(150, Some("https://host/exchangeInfo"));
        for body in [
            "",                                   // empty
            "not json",                           // garbage
            "<html>502 Bad Gateway</html>",       // a proxy error page
            r#"{"timezone":"UTC","symbols":[]}"#, // a venue that publishes nothing
            r#"{"rateLimits":[]}"#,               // an empty array
            // rows that exist but meter something else — ORDERS is a different gate entirely
            r#"{"rateLimits":[{"rateLimitType":"ORDERS","interval":"SECOND","limit":100}]}"#,
        ] {
            let p = pacer_for(&s, Some(body));
            assert_eq!(
                p.next_delay(),
                s.rate_limit.page_delay,
                "body {body:?} must fall back, not invent a pace"
            );
            assert!(!p.should_cool_down());
        }
    }

    /// The pace this loop ACTUALLY runs at, on the shape that was measured: fapi's 2400/min at the
    /// 40 % default, weight-5 pages, and the ~280 ms round trip a pooled-agent request costs from
    /// the CI box. The gap the venue sees is `sleep + request`, so the sleep must shrink to 32 ms for the
    /// pair to land on the 312 ms that spends 960 weight/min.
    ///
    /// (⚠ the 280 ms is the POOLED-agent number. A hand probe with ten separate `curl` calls read
    /// 417 ms; that figure includes per-process DNS + TLS setup this agent pays once, and using it
    /// here would over-subtract and pace ABOVE target.)
    #[test]
    fn a_timed_page_paces_to_the_target_end_to_end() {
        let s = spec(150, Some("https://fapi.binance.com/fapi/v1/exchangeInfo"));
        let mut p = pacer_for(&s, Some(FAPI_EXCHANGE_INFO));
        let rtt = Duration::from_millis(280);
        p.observe_request(Some(100), rtt);
        p.observe_request(Some(105), rtt); // weight 5

        let d = p.next_delay();
        let cycle = d + rtt;
        let spent = weight_per_minute(cycle, 5.0);
        assert!(
            (spent - 960.0).abs() < 1.0,
            "40% of 2400 is 960 weight/min end-to-end, got {spent}"
        );
        assert!(spent <= 2400.0, "and still under the venue's published ceiling");
        // The sleep-only pace would have spent ~506 — the ~23% the live run actually metered.
        assert!(
            weight_per_minute(Duration::from_millis(312) + rtt, 5.0) < 550.0,
            "the un-corrected pace must be the slow one, or this test proves nothing"
        );
    }

    /// The ETA the first page reports, on the run that motivated all of this: 24 months of 1m bars
    /// is ~1051 pages of 1000, and at the corrected pace that is ~5.5 minutes — against the 8m00s
    /// the same backfill MEASURED at the hand-set 150 ms delay (1051 * (0.150 + 0.280) = 452s, which
    /// is the wall clock that run actually took, to within its 429-free noise).
    #[test]
    fn the_reported_eta_is_the_full_cycle_not_the_sleep() {
        let s = spec(150, Some("https://fapi.binance.com/fapi/v1/exchangeInfo"));
        let mut p = pacer_for(&s, Some(FAPI_EXCHANGE_INFO));
        assert_eq!(p.eta(1_051), None, "nothing timed yet ⇒ no ETA rather than a wrong one");

        p.observe_request(Some(100), Duration::from_millis(280));
        p.observe_request(Some(105), Duration::from_millis(280));
        let eta = p.eta(1_051).expect("a timed page yields an ETA");
        assert!(
            eta > Duration::from_secs(300) && eta < Duration::from_secs(360),
            "expected ~5m28s for 1051 pages at 312ms, got {eta:?}"
        );
    }

    /// The ETA's page count: window width over one page's span, `None` when the interval is not a
    /// vike interval string at all (an ETA is a courtesy — a wrong one is worse than none).
    #[test]
    fn remaining_pages_divides_the_window_by_one_pages_span() {
        // 24 months of 1m bars — the measured shape. 730d = 63_072_000_000ms, a page = 60_000_000ms.
        let two_years = 730 * 86_400_000i64;
        assert_eq!(remaining_pages(0, two_years, "1m"), Some(1_051));
        // Coarser bars ⇒ far fewer pages for the same window.
        assert_eq!(remaining_pages(0, two_years, "1h"), Some(17));
        assert_eq!(
            remaining_pages(0, two_years, "1d"),
            Some(0),
            "730 days fits in one 1000-row page"
        );
        // A window narrower than one page has nothing left after the page just fetched.
        assert_eq!(remaining_pages(0, 1_000, "1m"), Some(0));
        // A cursor already past the end never goes negative.
        assert_eq!(remaining_pages(two_years, 0, "1m"), Some(0));
        // Unparseable / absurd intervals decline to guess instead of dividing by zero.
        assert_eq!(remaining_pages(0, two_years, ""), None);
        assert_eq!(remaining_pages(0, two_years, "1y"), None);
        assert_eq!(remaining_pages(0, two_years, "0m"), None);
        // A page span that overflows i64 (`200000000d` x 1000 rows) declines rather than wrapping
        // into a tiny span and reporting a preposterous page count.
        assert_eq!(remaining_pages(0, i64::MAX, "200000000d"), None);
    }

    /// `is_discovered` is what the pace line stamps as its `discovered` field (it used to be the
    /// GATE on that line — see the loop's comment). Nothing here asserts on a log line; it asserts
    /// on the flag the log site reads, which is the testable half.
    #[test]
    fn the_fallback_pacer_reports_no_discovered_pace() {
        assert!(!pacer_for(&spec(150, None), None).is_discovered());
        assert!(!pacer_for(&spec(150, Some("https://host/exchangeInfo")), Some("not json"))
            .is_discovered());
        assert!(pacer_for(&spec(150, Some("https://host/exchangeInfo")), Some(FAPI_EXCHANGE_INFO))
            .is_discovered());
    }

    /// The fallback pacer now ANSWERS an ETA once a page has been timed — which is what makes the
    /// un-gated report emit rather than fall through its `Option` zip. The sleep is untouched.
    #[test]
    fn a_fallback_pacer_still_reports_a_measured_eta_and_the_unchanged_delay() {
        let s = spec(150, None);
        let mut p = pacer_for(&s, None);
        assert_eq!(p.eta(1_051), None, "nothing timed yet ⇒ no line, not a fabricated one");
        p.observe_request(None, Duration::from_millis(280));
        assert_eq!(p.next_delay(), s.rate_limit.page_delay, "measuring must not move the sleep");
        let eta = p.eta(1_051).expect("a timed page yields an ETA even with no budget");
        // 1051 pages x (280ms request + the unchanged 150ms sleep) = ~452s — which is the wall
        // clock the hand-set delay MEASURED on the CI box, now reportable before the run finishes.
        assert!(eta > Duration::from_secs(440) && eta < Duration::from_secs(465), "{eta:?}");
        assert!(p.measured().is_some(), "and the measurement is persistable");
    }

    /// A venue that RE-PRICES the endpoint self-corrects without anyone editing a const — the whole
    /// reason the delay is inferred from the counter rather than measured once by hand.
    #[test]
    fn a_repriced_endpoint_widens_the_gap_by_itself() {
        let s = spec(50, Some("https://api.binance.com/api/v3/exchangeInfo"));
        let mut p = pacer_for(&s, Some(SPOT_EXCHANGE_INFO));
        p.observe(10);
        p.observe(12); // weight 2
        let cheap = p.next_delay();
        p.observe(17); // the same endpoint now costs 5
        let dear = p.next_delay();
        assert!(dear > cheap, "a heavier page must slow the pager, not keep the old pace");
        assert!(weight_per_minute(dear, 5.0) <= 6000.0);
    }

    #[test]
    fn parse_klines_maps_one_row_exactly() {
        // one 12-element kline row; o/h/l/c/v are decimal strings, openTime is ms.
        let body = r#"[[1700000000000,"27000.10","27050.50","26980.00","27010.25","12.34567800",1700000059999,"0",3,"0","0","0"]]"#;
        let bars = parse_klines(body).unwrap();
        assert_eq!(bars.len(), 1);
        let b = &bars[0];
        assert_eq!(b.ts, 1_700_000_000_000);
        assert_eq!(b.open.to_bits(), 27000.10_f64.to_bits());
        assert_eq!(b.high.to_bits(), 27050.50_f64.to_bits());
        assert_eq!(b.low.to_bits(), 26980.00_f64.to_bits());
        assert_eq!(b.close.to_bits(), 27010.25_f64.to_bits());
        assert_eq!(b.volume.to_bits(), 12.345678_f64.to_bits());
        assert!(b.funding.is_none() && b.bid.is_none() && b.ask.is_none() && b.symbol.is_none());
    }

    #[test]
    fn parse_klines_empty_array_is_empty() {
        assert!(parse_klines("[]").unwrap().is_empty());
    }

    #[test]
    fn parse_klines_rejects_short_row() {
        // a row missing the close field must error, not panic.
        let body = r#"[[1700000000000,"1","2","3"]]"#;
        assert!(parse_klines(body).is_err());
    }

    /// The cross-run payoff, on the two real binance shapes: a stored sample seeds the pacer that
    /// discovery just built, so the FIRST page is paced at last run's measurement instead of the
    /// pessimistic constant — and a sample from the OTHER host is refused, which is what stops a
    /// weight-2 spot record pacing the weight-5 fapi endpoint at 2.5x its target.
    #[test]
    fn a_persisted_sample_seeds_the_pacer_only_for_its_own_host() {
        let spot_spec = spec(50, Some("https://api.binance.com/api/v3/exchangeInfo"));
        let perp_spec = spec(150, Some("https://fapi.binance.com/fapi/v1/exchangeInfo"));
        let spot_record = PaceSample {
            request_ms: 120,
            per_request_weight: 2.0,
            budget_per_min: Some(6000),
            samples: 40,
        };
        let perp_record = PaceSample {
            request_ms: 280,
            per_request_weight: 5.0,
            budget_per_min: Some(2400),
            samples: 40,
        };

        // Matching host: applied, and the pace is immediately the corrected one.
        let mut perp = pacer_for(&perp_spec, Some(FAPI_EXCHANGE_INFO));
        assert!(perp.seed(&perp_record));
        assert!(
            (perp.next_delay().as_secs_f64() - 0.0325).abs() < 1e-6,
            "312.5ms target gap minus the 280ms the request is known to cost, on page ONE"
        );
        assert!(weight_per_minute(perp.next_delay() + Duration::from_millis(280), 5.0) <= 2400.0);

        // Cross-host: refused whole, and the pacer is left exactly as discovery built it.
        let mut cross = pacer_for(&perp_spec, Some(FAPI_EXCHANGE_INFO));
        assert!(!cross.seed(&spot_record), "the spot budget is not the fapi budget");
        assert_eq!(
            cross.next_delay(),
            pacer_for(&perp_spec, Some(FAPI_EXCHANGE_INFO)).next_delay()
        );

        // The spot record does apply to the spot pacer — 6000/min at 40% is a 30ms gap at weight 2,
        // which the known 120ms request time floors, and the END-TO-END pace stays under 6000.
        let mut spot = pacer_for(&spot_spec, Some(SPOT_EXCHANGE_INFO));
        assert!(spot.seed(&spot_record));
        assert_eq!(
            spot.per_request_weight(),
            2.0,
            "the measured cost, not the SEED_WEIGHT constant"
        );
        assert!(
            weight_per_minute(spot.next_delay() + Duration::from_millis(120), 2.0) <= 6000.0,
            "a seeded pace must stay under the venue's published ceiling"
        );

        // And a DISCOVERY-LESS spec is unreachable by any discovered record, so the byte-identical
        // fallback path cannot be sped up by a stored file either.
        let mut none = pacer_for(&spec(150, None), None);
        assert!(!none.seed(&perp_record));
        assert_eq!(none.next_delay(), Duration::from_millis(150));
    }

    // ----- the walk, sequential vs concurrent ---------------------------------------------------

    /// A synthetic binance klines endpoint over a KNOWN set of available bar timestamps.
    ///
    /// It reproduces the wire semantics the walk depends on, and nothing else:
    /// `startTime`/`endTime` are a FILTER (not a grid), the response is the first `rows_per_page`
    /// available rows inside `[cursor, end]`, ascending. That filter semantics is why a short page
    /// genuinely means "the window is exhausted" even across a gap in the venue's own history — the
    /// property the whole sequential-vs-concurrent equivalence rests on.
    struct SyntheticVenue {
        /// Every timestamp this "venue" has history for, ascending.
        ticks: Vec<i64>,
        rows_per_page: usize,
        /// Every `(cursor, end)` this venue was asked for — the request-sequence evidence.
        asked: std::sync::Mutex<Vec<(i64, i64)>>,
        /// Fail any page whose `[cursor, end]` CONTAINS this tick. Keyed on the tick rather than on
        /// the cursor so the same fixture fails deterministically in both paths (the sequential walk
        /// and whichever lane owns that tick), and in exactly ONE lane.
        fail_tick: Option<i64>,
        /// Per-page wall-clock cost, so an abort has something to land in. `ZERO` everywhere except
        /// the abort test, which is the only one that cares about wall-clock interleaving.
        page_cost: Duration,
    }

    impl SyntheticVenue {
        fn new(ticks: Vec<i64>, rows_per_page: usize) -> Self {
            SyntheticVenue {
                ticks,
                rows_per_page,
                asked: std::sync::Mutex::new(Vec::new()),
                fail_tick: None,
                page_cost: Duration::ZERO,
            }
        }

        fn page(&self, cursor: i64, end: i64) -> Result<Vec<Bar>, String> {
            self.asked.lock().unwrap().push((cursor, end));
            if self.page_cost > Duration::ZERO {
                std::thread::sleep(self.page_cost);
            }
            if self.fail_tick.is_some_and(|t| cursor <= t && t <= end) {
                return Err(format!("synthetic venue failed over [{cursor}, {end}]"));
            }
            Ok(self
                .ticks
                .iter()
                .copied()
                .filter(|&t| cursor <= t && t <= end)
                .take(self.rows_per_page)
                .map(|t| kline_to_bar(t, 1.0, 2.0, 0.5, 1.5, 10.0))
                .collect())
        }

        fn requests(&self) -> usize {
            self.asked.lock().unwrap().len()
        }
    }

    /// The four history SHAPES that could make a span split disagree with cursor chaining, plus the
    /// dense one. Each is `(name, ticks)` over a 1m grid.
    fn history_shapes() -> Vec<(&'static str, Vec<i64>)> {
        let step = 60_000i64;
        let dense: Vec<i64> = (0..250).map(|i| i * step).collect();
        // Listed late: nothing before minute 180 (a symbol whose history starts inside the window).
        let leading_gap: Vec<i64> = (180..250).map(|i| i * step).collect();
        // A maintenance/delisting hole in the middle, wider than several pages.
        let interior_gap: Vec<i64> = (0..60).chain(140..250).map(|i: i64| i * step).collect();
        // History runs out well before the requested end.
        let trailing_gap: Vec<i64> = (0..70).map(|i| i * step).collect();
        // Every third bar only — a venue that serves no rows for illiquid minutes.
        let sparse: Vec<i64> = (0..250).filter(|i| i % 3 == 0).map(|i| i * step).collect();
        vec![
            ("dense", dense),
            ("leading gap", leading_gap),
            ("interior gap", interior_gap),
            ("trailing gap", trailing_gap),
            ("sparse", sparse),
            ("empty", Vec::new()),
        ]
    }

    /// THE load-bearing test: a synthetic multi-window fetch returns the SAME `Vec<Bar>` as the
    /// sequential path — same order, same dedup, same clipping — for every lane count and every
    /// history shape that could make the two disagree.
    ///
    /// It is what makes the concurrent pager a refactor rather than a second implementation, and it
    /// is deliberately asserted on the BARS rather than on request counts: a lane split legitimately
    /// costs a few extra boundary pages (each span's last page is short), so equality of requests
    /// would be false while equality of RESULT is the contract.
    #[test]
    fn lanes_return_the_same_bars_as_the_sequential_walk() {
        let rows_per_page = 10usize;
        let (start, end) = (0i64, 250 * 60_000i64);
        for (name, ticks) in history_shapes() {
            let seq_venue = SyntheticVenue::new(ticks.clone(), rows_per_page);
            let sequential =
                walk_forward_pages(start, end, rows_per_page, |c, e| seq_venue.page(c, e)).unwrap();

            for lanes in 1..=8usize {
                let venue = SyntheticVenue::new(ticks.clone(), rows_per_page);
                let concurrent =
                    walk_forward_spans(start, end, rows_per_page, lanes, |c, e| venue.page(c, e))
                        .unwrap();
                assert_eq!(
                    concurrent.iter().map(|b| b.ts).collect::<Vec<_>>(),
                    sequential.iter().map(|b| b.ts).collect::<Vec<_>>(),
                    "{name} @ {lanes} lanes: the concurrent walk must return the sequential result"
                );
                // ...and every field, bit for bit — not just the timestamps.
                for (c, s) in concurrent.iter().zip(sequential.iter()) {
                    assert_eq!(c.open.to_bits(), s.open.to_bits());
                    assert_eq!(c.high.to_bits(), s.high.to_bits());
                    assert_eq!(c.low.to_bits(), s.low.to_bits());
                    assert_eq!(c.close.to_bits(), s.close.to_bits());
                    assert_eq!(c.volume.to_bits(), s.volume.to_bits());
                }
                // The contract the store depends on: ascending, unique, inside the window.
                assert!(
                    concurrent.windows(2).all(|w| w[0].ts < w[1].ts),
                    "{name} @ {lanes}: ascending and deduped"
                );
                assert!(concurrent.iter().all(|b| start <= b.ts && b.ts <= end));
            }
        }
    }

    /// ONE lane is the sequential walk — not "equivalent to" it: `split_range(_, _, 1)` is one span,
    /// which `run_lanes` runs INLINE, so the request sequence is identical too, not merely the rows.
    #[test]
    fn one_lane_issues_exactly_the_sequential_requests() {
        let rows_per_page = 10usize;
        let (start, end) = (0i64, 250 * 60_000i64);
        for (name, ticks) in history_shapes() {
            let seq = SyntheticVenue::new(ticks.clone(), rows_per_page);
            let sequential =
                walk_forward_pages(start, end, rows_per_page, |c, e| seq.page(c, e)).unwrap();
            let one = SyntheticVenue::new(ticks, rows_per_page);
            let lane =
                walk_forward_spans(start, end, rows_per_page, 1, |c, e| one.page(c, e)).unwrap();

            assert_eq!(
                lane.iter().map(|b| b.ts).collect::<Vec<_>>(),
                sequential.iter().map(|b| b.ts).collect::<Vec<_>>(),
                "{name}: one lane must be byte-identical in RESULT"
            );
            assert_eq!(
                *one.asked.lock().unwrap(),
                *seq.asked.lock().unwrap(),
                "{name}: ...and in the exact sequence of (cursor, end) pairs it asked the venue for"
            );
        }
    }

    /// A failed window must NOT silently shrink the result. The whole fetch fails, because these
    /// bars are written under ONE commit key naming the whole window — a partial reported as success
    /// marks that window permanently ingested and the missing rows are never fetched again.
    #[test]
    fn one_failed_window_fails_the_whole_fetch_rather_than_shrinking_it() {
        let rows_per_page = 10usize;
        let (start, end) = (0i64, 250 * 60_000i64);
        let ticks: Vec<i64> = (0..250).map(|i| i * 60_000).collect();

        // A tick in the middle of the window — inside the span lane 2 of 4 owns, and reached by the
        // sequential walk too, so both paths fail on the SAME fixture for the same reason.
        let broken = 130 * 60_000i64;

        let mut venue = SyntheticVenue::new(ticks.clone(), rows_per_page);
        venue.fail_tick = Some(broken);
        let err = walk_forward_spans(start, end, rows_per_page, 4, |c, e| venue.page(c, e))
            .expect_err("a failed window must fail the fetch, never return the other lanes' rows");
        assert!(err.contains("synthetic venue failed"), "{err}");

        // The failure policy is not concurrency-specific — it is the sequential loop's `?`.
        let mut seq = SyntheticVenue::new(ticks, rows_per_page);
        seq.fail_tick = Some(broken);
        assert!(walk_forward_pages(start, end, rows_per_page, |c, e| seq.page(c, e)).is_err());
    }

    /// A failure STOPS the sibling lanes instead of letting them page the window out. Bounded, not
    /// instant: a blocking request cannot be cancelled, so each lane pays at most one more page.
    #[test]
    fn a_failed_window_stops_the_sibling_lanes() {
        let rows_per_page = 10usize;
        let (start, end) = (0i64, 5_000 * 60_000i64);
        let ticks: Vec<i64> = (0..5_000).map(|i| i * 60_000).collect();
        let mut venue = SyntheticVenue::new(ticks, rows_per_page);
        // Tick 0 lives only in lane 0's span, and only in its FIRST page — so exactly one lane
        // fails, immediately, and the other three are still paging when it does.
        venue.fail_tick = Some(0);
        venue.page_cost = Duration::from_micros(300);
        assert!(walk_forward_spans(start, end, rows_per_page, 4, |c, e| venue.page(c, e)).is_err());
        // Run to completion the four lanes would need ~500 pages between them. The abort check caps
        // it far below that; the exact count is scheduler-dependent, the order of magnitude is not.
        assert!(
            venue.requests() < 200,
            "the siblings must stop early, not page the window out: {} requests",
            venue.requests()
        );
    }

    /// The walk's three stop conditions, pinned directly — an EMPTY page, a SHORT page, and a cursor
    /// that cannot move forward. The third is the infinite-loop guard: a venue answering with a bar
    /// at or before the cursor must terminate the walk, not spin.
    #[test]
    fn the_forward_walk_stops_on_empty_short_and_non_advancing_pages() {
        // Empty: one request, no rows.
        let empty = SyntheticVenue::new(Vec::new(), 10);
        assert!(walk_forward_pages(0, 1_000_000, 10, |c, e| empty.page(c, e)).unwrap().is_empty());
        assert_eq!(empty.requests(), 1, "an empty page must not be retried");

        // Short: the page is smaller than the cap ⇒ the window is exhausted, one request.
        let short = SyntheticVenue::new(vec![0, 60_000, 120_000], 10);
        assert_eq!(walk_forward_pages(0, 1_000_000, 10, |c, e| short.page(c, e)).unwrap().len(), 3);
        assert_eq!(short.requests(), 1);

        // Non-advancing: a venue that keeps answering with the SAME full page must TERMINATE rather
        // than loop forever. The cap is 2, so every page is full and the short-page rule cannot
        // help — only the cursor guard can.
        let calls = std::sync::atomic::AtomicUsize::new(0);
        let out = walk_forward_pages(0, 1_000_000, 2, |_, _| {
            calls.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            Ok(vec![
                kline_to_bar(0, 1.0, 1.0, 1.0, 1.0, 0.0),
                kline_to_bar(1, 1.0, 1.0, 1.0, 1.0, 0.0),
            ])
        })
        .unwrap();
        // Page 1 advances the cursor 0 -> 2; page 2 answers the same rows, so `next` (2) is not
        // past the cursor (2) and the walk stops. TWO requests, and the repeated rows come back
        // duplicated — which the sequential path has always done and `walk_forward_spans` dedups.
        assert_eq!(calls.load(std::sync::atomic::Ordering::Relaxed), 2, "must not spin");
        assert_eq!(out.len(), 4);
    }

    /// Bars OUTSIDE the requested window are clipped, in both paths — a venue that over-serves must
    /// not widen the result, and the span split must not let a boundary bar in twice.
    #[test]
    fn both_paths_clip_to_the_requested_window_and_never_double_count_a_boundary() {
        let step = 60_000i64;
        let ticks: Vec<i64> = (0..250).map(|i| i * step).collect();
        // A window that starts and ends INSIDE the available history, on non-page boundaries.
        let (start, end) = (37 * step, 191 * step);
        let expected: Vec<i64> =
            ticks.iter().copied().filter(|&t| start <= t && t <= end).collect();

        let seq = SyntheticVenue::new(ticks.clone(), 10);
        let sequential = walk_forward_pages(start, end, 10, |c, e| seq.page(c, e)).unwrap();
        assert_eq!(sequential.iter().map(|b| b.ts).collect::<Vec<_>>(), expected);

        for lanes in 1..=8usize {
            let v = SyntheticVenue::new(ticks.clone(), 10);
            let got = walk_forward_spans(start, end, 10, lanes, |c, e| v.page(c, e)).unwrap();
            assert_eq!(
                got.iter().map(|b| b.ts).collect::<Vec<_>>(),
                expected,
                "{lanes} lanes must clip to [{start}, {end}] exactly"
            );
        }
    }

    /// The lane count this rung will actually derive on the two REAL binance shapes — the finding
    /// the whole change rests on, asserted here (against the same fixtures the pacing tests use)
    /// rather than left to the pacer's own unit tests.
    #[test]
    fn the_derived_lane_count_splits_binances_two_hosts() {
        let rtt = Duration::from_millis(280); // MEASURED, pooled agent, the CI box

        // fapi: a 312ms target gap is WIDER than the round trip ⇒ one lane, nothing changes.
        let perp = spec(150, Some("https://fapi.binance.com/fapi/v1/exchangeInfo"));
        let mut p = pacer_for(&perp, Some(FAPI_EXCHANGE_INFO));
        p.observe_request(Some(100), rtt);
        p.observe_request(Some(105), rtt); // weight 5
        assert_eq!(p.suggested_lanes(8), 1, "perp is budget-bound, not latency-bound");

        // spot: a 50ms target gap against the same round trip ⇒ 6 lanes, and the sequential sleep is
        // already floored, which is why no delay tuning could have closed this.
        let spot = spec(50, Some("https://api.binance.com/api/v3/exchangeInfo"));
        let mut s = pacer_for(&spot, Some(SPOT_EXCHANGE_INFO));
        s.observe_request(Some(10), rtt);
        s.observe_request(Some(12), rtt); // weight 2
        assert_eq!(s.suggested_lanes(8), 6);
        assert!(s.next_delay() <= Duration::from_millis(1), "the sleep had nothing left to give");
        // And the aggregate the gate will pace at is still 40% of the published 6000, not 6x it.
        assert!(
            weight_per_minute(s.target_gap(), 2.0) <= 6000.0 * 0.4 + 1.0,
            "the target rate is unchanged by concurrency: {:?}",
            s.target_gap()
        );

        // A venue that publishes NO budget gets one lane whatever is measured — aster's shape.
        let none = spec(150, None);
        let mut n = pacer_for(&none, None);
        n.observe_request(None, Duration::from_millis(480));
        assert_eq!(n.suggested_lanes(8), 1, "no discovered ceiling ⇒ no aggregate ⇒ no lanes");
    }

    /// THE cold-run gate for [`PROBE_PAGES`]. One probe page records the counter but yields no
    /// DELTA — there is nothing to subtract from — so `per_request` is still the pacer's pessimistic
    /// seed of 5, and spot's own weight-2 endpoint is paced as though every page cost 5.
    ///
    /// FAILS at `PROBE_PAGES = 1`: the derived count is 3 on a 125 ms gap, not 6 on 50 ms. That is
    /// not merely three fewer lanes — the gap is twice too wide, so the run also spends 40 % of the
    /// 40 % of the venue's budget that was actually asked for.
    #[test]
    fn a_cold_run_needs_two_probe_pages_to_derive_the_venues_own_weight() {
        let rtt = Duration::from_millis(280); // MEASURED, pooled agent, the CI box
        let s = spec(50, Some("https://api.binance.com/api/v3/exchangeInfo"));

        // ONE probe page — what a single-probe implementation would hand `suggested_lanes`.
        let mut one = pacer_for(&s, Some(SPOT_EXCHANGE_INFO));
        one.observe_request(Some(10), rtt);
        assert_eq!(one.per_request_weight(), 5.0, "one reading is a baseline, not a delta");
        assert!((one.target_gap().as_secs_f64() - 0.125).abs() < 1e-6, "{:?}", one.target_gap());
        assert_eq!(
            one.suggested_lanes(8),
            3,
            "ceil(280/125) — the seed's lane count, not the venue's"
        );

        // TWO — what `PROBE_PAGES` actually walks. The second reading is the measurement.
        let mut two = pacer_for(&s, Some(SPOT_EXCHANGE_INFO));
        two.observe_request(Some(10), rtt);
        two.observe_request(Some(12), rtt); // `/api/v3/klines?limit=1000` costs weight 2
        assert_eq!(two.per_request_weight(), 2.0, "the venue's own cost");
        assert!((two.target_gap().as_secs_f64() - 0.050).abs() < 1e-6, "{:?}", two.target_gap());
        assert_eq!(two.suggested_lanes(8), 6, "ceil(280/50)");
        // ...and the aggregate the gate will run at is still 40% of the published 6000, not 6x it.
        assert!(weight_per_minute(two.target_gap(), 2.0) <= 6000.0 * DEFAULT_UTILIZATION + 1.0);

        // (`PROBE_PAGES >= 2` is enforced beside the const itself, as a `const _: ()` — a compile
        // error rather than a test failure, because a build that got it wrong should not link.)

        // fapi is 1 either way, so the second page costs the perp path nothing but a page it was
        // going to fetch regardless.
        let f = spec(150, Some("https://fapi.binance.com/fapi/v1/exchangeInfo"));
        let mut one_perp = pacer_for(&f, Some(FAPI_EXCHANGE_INFO));
        one_perp.observe_request(Some(100), rtt);
        assert_eq!(one_perp.suggested_lanes(8), 1);
        let mut two_perp = pacer_for(&f, Some(FAPI_EXCHANGE_INFO));
        two_perp.observe_request(Some(100), rtt);
        two_perp.observe_request(Some(105), rtt);
        assert_eq!(two_perp.suggested_lanes(8), 1);
    }

    /// The probe phase's PURPOSE, driven end to end without a socket: after [`PROBE_PAGES`] pages
    /// fed through a pacer exactly the way `fetch_klines_range_lanes` feeds them, the pacer knows the
    /// VENUE's per-request weight and therefore derives the venue's lane count.
    ///
    /// This is the behavioural half of the `PROBE_PAGES >= 2` gate (the arithmetic half is the
    /// `const _: ()` beside the const). FAILS at `PROBE_PAGES = 1`: one page leaves `per_request` at
    /// the pacer's seed of 5, the gap at 125 ms and the derivation at 3 lanes.
    #[test]
    fn the_probe_phase_teaches_the_pacer_the_venues_own_weight() {
        let s = spec(50, Some("https://api.binance.com/api/v3/exchangeInfo"));
        let mut pacer = pacer_for(&s, Some(SPOT_EXCHANGE_INFO));
        let ticks: Vec<i64> = (0..250).map(|i| i * 60_000).collect();
        let venue = SyntheticVenue::new(ticks, 10);
        // The venue's counter advancing by the REAL weight-2 page cost, with exactly one request
        // outstanding — the only condition under which a delta is a per-request cost at all.
        let mut counter = 100u64;
        let (_bars, resume) = probe_pages(0, 250 * 60_000, 10, PROBE_PAGES, |c, e| {
            let page = venue.page(c, e)?;
            counter += 2;
            pacer.observe_request(Some(counter), Duration::from_millis(280));
            Ok(page)
        })
        .unwrap();

        assert!(resume.is_some(), "the window is longer than the probe budget");
        assert_eq!(venue.requests(), PROBE_PAGES, "the probe stops at its budget");
        assert_eq!(pacer.per_request_weight(), 2.0, "the VENUE's cost, not the pacer's seed of 5");
        assert!((pacer.target_gap().as_secs_f64() - 0.050).abs() < 1e-6);
        assert_eq!(pacer.suggested_lanes(8), 6, "which is what makes six lanes the derived answer");
    }

    /// `probe_pages` is [`walk_forward_pages`] with a page budget and a resume cursor — pinned
    /// against it directly, so the probe phase cannot drift from the walk it hands off to.
    #[test]
    fn an_unbounded_probe_is_the_sequential_walk() {
        let rows = 10usize;
        let (start, end) = (0i64, 250 * 60_000i64);
        for (name, ticks) in history_shapes() {
            let seq = SyntheticVenue::new(ticks.clone(), rows);
            let walked = walk_forward_pages(start, end, rows, |c, e| seq.page(c, e)).unwrap();
            let probed_venue = SyntheticVenue::new(ticks, rows);
            let (probed, resume) =
                probe_pages(start, end, rows, usize::MAX, |c, e| probed_venue.page(c, e)).unwrap();
            assert_eq!(
                probed.iter().map(|b| b.ts).collect::<Vec<_>>(),
                walked.iter().map(|b| b.ts).collect::<Vec<_>>(),
                "{name}: an unbounded probe must be the sequential walk"
            );
            assert_eq!(resume, None, "{name}: ...which always exhausts the window");
            assert_eq!(*probed_venue.asked.lock().unwrap(), *seq.asked.lock().unwrap());
        }
    }

    /// The hand-off: after `PROBE_PAGES` the probe stops and names the cursor the LANES resume from,
    /// and probe+lanes together are the sequential walk — the same equality the one-lane test makes,
    /// now across the real two-phase shape the network function runs.
    #[test]
    fn the_probe_hands_the_lanes_a_cursor_that_reproduces_the_sequential_walk() {
        let rows = 10usize;
        let (start, end) = (0i64, 250 * 60_000i64);
        for (name, ticks) in history_shapes() {
            let seq = SyntheticVenue::new(ticks.clone(), rows);
            let sequential = walk_forward_pages(start, end, rows, |c, e| seq.page(c, e)).unwrap();

            for lanes in 1..=8usize {
                let venue = SyntheticVenue::new(ticks.clone(), rows);
                let (mut out, resume) =
                    probe_pages(start, end, rows, PROBE_PAGES, |c, e| venue.page(c, e)).unwrap();
                assert!(
                    venue.requests() <= PROBE_PAGES,
                    "{name}: the probe must stop at its budget"
                );
                if let Some(cursor) = resume {
                    assert!(cursor > start && cursor <= end, "{name}: a usable resume cursor");
                    let rest =
                        walk_forward_spans(cursor, end, rows, lanes, |c, e| venue.page(c, e))
                            .unwrap();
                    out.extend(rest);
                    out.sort_by_key(|b| b.ts);
                    out.dedup_by_key(|b| b.ts);
                }
                assert_eq!(
                    out.iter().map(|b| b.ts).collect::<Vec<_>>(),
                    sequential.iter().map(|b| b.ts).collect::<Vec<_>>(),
                    "{name} @ {lanes} lanes: probe + lanes must equal the sequential walk"
                );
            }
        }
    }

    /// The probe's own stop conditions are the walk's three, and it declines to hand over a cursor
    /// for a window it has already exhausted — the guard that stops a lane being spawned for nothing.
    #[test]
    fn the_probe_reports_no_resume_cursor_for_an_exhausted_window() {
        let rows = 10usize;
        // Empty page.
        let empty = SyntheticVenue::new(Vec::new(), rows);
        assert_eq!(probe_pages(0, 1_000_000, rows, 2, |c, e| empty.page(c, e)).unwrap().1, None);
        assert_eq!(empty.requests(), 1, "and it stops asking");

        // Short page — the window is exhausted inside the budget.
        let short = SyntheticVenue::new(vec![0, 60_000], rows);
        let (bars, resume) = probe_pages(0, 1_000_000, rows, 2, |c, e| short.page(c, e)).unwrap();
        assert_eq!(bars.len(), 2);
        assert_eq!(resume, None);
        assert_eq!(short.requests(), 1);

        // Exactly PROBE_PAGES full pages with more to come ⇒ a resume cursor for the lanes.
        let ticks: Vec<i64> = (0..250).map(|i| i * 60_000).collect();
        let more = SyntheticVenue::new(ticks, rows);
        let (bars, resume) = probe_pages(0, 250 * 60_000, rows, 2, |c, e| more.page(c, e)).unwrap();
        assert_eq!(bars.len(), 20, "two full pages");
        assert_eq!(resume, Some(19 * 60_000 + 1), "one ms past the second page's last openTime");
        assert_eq!(more.requests(), 2);

        // An empty window is never even asked about.
        let never = SyntheticVenue::new(vec![0], rows);
        assert_eq!(probe_pages(100, 0, rows, 2, |c, e| never.page(c, e)).unwrap(), (vec![], None));
        assert_eq!(never.requests(), 0);
    }

    /// The RUNTIME discovery miss — a spec that names an `exchangeInfo` URL but does not get a usable
    /// body this run. `fetch_klines_range_lanes` tests `is_discovered()` and hands the fetch to the
    /// sequential `paged_walk` before a gate exists, because a `Fixed` pacer's `target_gap` IS the
    /// hand-set `page_delay` and pacing a shared gate by it would space requests ~1.5x closer than
    /// the sequential pager does — against a venue whose ceiling we just failed to read.
    #[test]
    fn a_runtime_discovery_miss_is_not_a_one_lane_gate() {
        let s = spec(150, Some("https://fapi.binance.com/fapi/v1/exchangeInfo"));
        for body in [None, Some(""), Some("not json"), Some(r#"{"rateLimits":[]}"#)] {
            let mut p = pacer_for(&s, body);
            assert!(!p.is_discovered(), "body {body:?} must not look discovered");
            p.observe_request(None, Duration::from_millis(280));
            // The gate WOULD have paced at the bare page_delay (150ms) where the sequential pager
            // paces at page_delay + request (430ms) — which is why the branch is on `is_discovered`
            // and not on the lane count.
            assert_eq!(p.target_gap(), s.rate_limit.page_delay);
            assert_eq!(p.next_delay(), s.rate_limit.page_delay);
            assert_eq!(p.suggested_lanes(8), 1);
        }
    }

    #[test]
    fn klines_url_includes_optional_bounds() {
        let latest = klines_url(BASE, "BTCUSDT", "1m", None, None, 1000);
        assert_eq!(latest, "https://host/api/v3/klines?symbol=BTCUSDT&interval=1m&limit=1000");
        let ranged = klines_url(BASE, "BTCUSDT", "1m", Some(10), Some(20), 1000);
        assert!(ranged.ends_with("&startTime=10&endTime=20"));
    }
}
