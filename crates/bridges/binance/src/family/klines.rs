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
//! [`vike_model::rate_limits::PaceSample`] and receives this run's, and keeping it between runs is
//! the caller's job, because nothing in a bridge crate reads or writes operator state. (The one
//! caller that kept it — `vike-backfill`'s pace file, behind its one-shot kline programs — was
//! deleted by docs/decisions/0094.)
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
use vike_bridge_core::retry::{BackoffPolicy, Verdict, retry_rate_limited};
use vike_model::Bar;
use vike_model::rate_limits::PaceSample;

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
    /// **Which column of this host's kline row carries the BASE-ASSET amount** — see
    /// [`VolumeColumn`]. Every venue face but binance's COIN-M one names
    /// [`VolumeColumn::Index5`], which is what every kline this rung has ever parsed used, so a
    /// spec that does not think about it is unchanged.
    ///
    /// ⚠ It rides on the SPEC rather than on the fetch call because it is a property of the HOST's
    /// row grammar, exactly as [`Self::exchange_info_url`] is a property of the host's budget —
    /// and for the same reason: pairing a host with another host's answer is the defect both
    /// fields exist to make unsayable.
    pub volume_column: VolumeColumn,
}

/// Which column of a binance-grammar kline row holds the amount `vike_model::Bar::volume` means —
/// the BASE ASSET traded in the bar.
///
/// ⚠ **This is not a preference. The two hosts disagree, and it is MEASURED.** From the live
/// keyless endpoints, 2026-09-16, the same hour on two books:
///
/// | book | index 5 | index 7 |
/// |---|---|---|
/// | USDⓈ-M `BTCUSDT` | `"14205.232"` — **BTC** | `"1090447633.34890"` — USDT (quote) |
/// | COIN-M `BTCUSD_PERP` | `"595752"` — **CONTRACTS**, $100 each | `"776.29174326"` — **BTC** |
///
/// The two columns SWAP meaning between binance's two futures books, and the arithmetic confirms
/// which is which: 595,752 contracts x $100 is ~$59.5M, and 776.29 BTC at the bar's ~$76.9k close
/// is the same ~$59.5M. So a COIN-M bar parsed at index 5 lands a contract COUNT where every other
/// binance series holds a base-asset amount — a roughly 760x unit error inside ONE `kind=bar`
/// schema, invisible to `crates/vike-data/src/store/store_kind.rs` (which declares the column, not its
/// unit) and to every consumer of it.
///
/// ⚠ **The divergence follows the SYMBOL, not the host.** MEASURED: `fapi` serves the COIN-M tape
/// byte-identically to `dapi`, so this is neither fixed by adding the COIN-M host nor avoided by
/// staying on fapi. The only thing that decides it is which book the symbol names, which is what
/// `crates/bridges/binance/src/instruments.rs` is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VolumeColumn {
    /// Index 5 — the base-asset amount on spot, on USDⓈ-M futures, and on every aster book. The
    /// default, and what this rung has always parsed.
    Index5,
    /// Index 7 — the base-asset amount on a COIN-M (coin-margined) book, where index 5 is a
    /// contract COUNT instead.
    Index7,
}

impl VolumeColumn {
    /// The row index this column reads.
    #[must_use]
    pub const fn index(self) -> usize {
        match self {
            Self::Index5 => 5,
            Self::Index7 => 7,
        }
    }
}

/// Map one raw kline row (the 12-element JSON array) → a [`Bar`]. o/h/l/c are required decimal
/// strings; volume defaults to 0.0 on a parse miss (the legacy feed's tolerance). Preserves the
/// exact f64 bit pattern of each string via `<f64 as FromStr>` — no rounding, no arithmetic.
///
/// `volume` names WHICH column carries the base-asset amount on the host this body came from; see
/// [`VolumeColumn`] for the measurement that makes it a parameter rather than the constant `5` it
/// was.
fn row_to_bar(r: &[serde_json::Value], volume: VolumeColumn) -> Result<Bar, String> {
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
        .get(volume.index())
        .and_then(serde_json::Value::as_str)
        .and_then(|s| s.parse::<f64>().ok())
        .unwrap_or(0.0);
    Ok(kline_to_bar(t, o, h, l, c, v))
}

/// Pure map: a Binance/Aster klines JSON response body → `Vec<Bar>` (ascending by openTime, as the
/// venue serves it). No network, no venue delta — this is the fixture-tested seam.
///
/// Reads the base-asset amount from [`VolumeColumn::Index5`], which is every book this function has
/// ever been pointed at. A host whose row grammar puts it elsewhere calls [`parse_klines_with`];
/// this signature is unchanged and so is every byte it produces.
pub fn parse_klines(body: &str) -> Result<Vec<Bar>, String> {
    parse_klines_with(body, VolumeColumn::Index5)
}

/// [`parse_klines`] naming the column the base-asset amount lives in on THIS host — the seam
/// binance's COIN-M route needs and the one every other caller takes the default of. See
/// [`VolumeColumn`] for why it is not a constant.
pub fn parse_klines_with(body: &str, volume: VolumeColumn) -> Result<Vec<Bar>, String> {
    let rows: Vec<Vec<serde_json::Value>> =
        serde_json::from_str(body).map_err(|e| format!("kline json: {e}"))?;
    rows.iter().map(|r| row_to_bar(r, volume)).collect()
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
            200..=299 => {
                // The SPEC names the column, because the spec is what names the host — see
                // `KlineSpec::volume_column`.
                Ok(Verdict::Done((
                    parse_klines_with(&raw.body, spec.volume_column)?,
                    raw.used_weight,
                )))
            }
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
/// The twin of `vike_bridge_core::klines::walk_backward_pages` (this family pages forward with
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
// `const _: ()` discipline `vike_model::venues::venue_rate_limits` applies to its per-row invariants. One
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
    // `vike_bridge_core::klines::walk_backward_pages` applies, and it is what makes "the concurrent result IS
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

#[path = "klines_tests.rs"]
#[cfg(test)]
mod klines_tests;
