//! Aster REST kline history — the venue face of the shared [`vike_binance::family::history::klines`] rung
//! (F, dedup rung 6). Aster's kline wire format (12-element array, decimal-string OHLCV, 1000-row
//! cap, `X-MBX-USED-WEIGHT-1M`-header-aware paged backfill) is byte-identical to Binance's, so both
//! the live feed's warmup seed (`fetch_klines_latest`) and a paged historical backfill
//! (`fetch_klines_range`) delegate to the family core.
//!
//! This module supplies only Aster's OWN two deltas: host resolution — the venue's `venue_url` rows
//! (spot vs perp), resolved ONCE by whoever builds the fetch: [`KlineSourceAster::from_urls`] for the
//! backfill (`live` `history`), the live feed's [`crate::transport::urls::AsterUrls`] for its warmup seed — and
//! the WIRING of Aster's rate-limit budget, whose FALLBACK numbers live in
//! `vike_venues::venue_rate_limits`'s `ASTER_PERP` row (a venue's published ceiling is a fact about
//! the venue, not a constant of this crate). The public surface
//! is `fetch_klines_latest`/`fetch_klines_range`; the pure `parse_klines` is the family's. See
//! [`vike_binance::family::history::klines`] for the endpoint/response/rate-limit contract.
//!
//! Endpoint: `GET {sapi_rest}/api/v3/klines?…` (spot) or `GET {fapi_rest}/fapi/v3/klines?…` (perp).
//!
//! **Aster DISCOVERS its budget** like Binance does (one keyless `exchangeInfo` GET per paged
//! backfill, then the pager targets a fraction of what the venue published). Its discovery endpoint
//! is composed on the SAME host as its `/klines` endpoint — both fall out of the one host
//! [`range_target`] picks — which is why the spec is BUILT per call ([`aster_kline_spec`]) rather
//! than being a `const`, and why the shared rung's URL field is a `Cow`. See [`rest_exchange_info`]
//! for what each of the four hosts actually publishes.

use std::borrow::Cow;
use std::time::Duration;

use vike_binance::family::history::klines::{self, KlineRateLimit, KlineSpec, VolumeColumn};
use vike_marketdata::Bar;
use vike_venues::venue_rate_limits::{ASTER_PERP, ASTER_SPOT, VenueRateLimitsHistory};
use vike_venues::venue_urls::{UrlChannel, UrlProtocol, UrlRefusal, UrlType, VenueUrls};

use crate::transport::urls::{AsterUrls, PERP_PRODUCT, SPOT_PRODUCT, VENUE};

/// Aster's paged-backfill rate-limit budget. **MEASURED 2026-08-04 — the TODO this comment used to
/// carry is now done, and the value it guessed was WRONG.**
///
/// A clean probe from the CI box (`x-mbx-used-weight-1m` read on a counter with no concurrent traffic,
/// unlike Binance's, which a running backfill was polluting) reports weight **5** for
/// `fapi/v3/klines?limit=1000`, and live `exchangeInfo` reports `REQUEST_WEIGHT` `MINUTE` = **2400**
/// — NOT the 6000 this const inherited from Binance's SPOT template. The old `weight_soft_limit:
/// 5000` therefore sat ABOVE Aster's hard ceiling and could never fire: the venue's 418/ban would
/// arrive first, and the guard was decoration. 480 req/min is the ceiling, so a `page_delay` of
/// 150ms (~400 req/min ⇒ ~2000 weight/min) leaves genuine headroom instead of accidental safety.
///
/// ⚠ That paragraph used to end "ONE fallback budget is right here — unlike Binance, whose spot and
/// perp hosts have DIFFERENT published budgets". **Aster's differ too**, measured 2026-08-05: sapi
/// publishes 6000 where fapi publishes 2400 — the same 6000-vs-2400 split, on the same two host
/// roles. The claim was inherited from a time when only fapi had been probed.
///
/// **The two rate numbers are not this crate's to choose** — `page_delay` and `weight_soft_limit`
/// come from the workspace's per-venue table ([`vike_venues::venue_rate_limits::ASTER_SPOT`] /
/// [`vike_venues::venue_rate_limits::ASTER_PERP`]), which also carries the ceiling they are sized
/// against. The retry/backoff knobs are this pager's own POLICY, identical on both hosts, so they
/// stay here.
///
/// ⚠ **Per HOST, not one shared const — and that is the whole point.** These were built from
/// `ASTER_PERP` alone and used for whichever host `range_target` resolved, so a bare (spot) symbol
/// braked on the FAPI soft limit of 2000. Since the discovery split, `vike_aster` already learns
/// sapi's own 6000 ceiling and paces against it — but `should_cool_down` fires on this hand-set
/// number, which applies ON TOP of whatever was discovered. A spot backfill therefore braked at
/// 33 % of the budget it had just proven it was entitled to.
///
/// The corollary matters more than the fix: raising `ASTER_SPOT` in the venue table **alone** would
/// have changed nothing, because that row had no reader. A declared value with nothing consuming it
/// is not a setting, it is a comment.
const fn aster_rate_limit(h: VenueRateLimitsHistory) -> KlineRateLimit {
    KlineRateLimit {
        page_delay: h.page_delay(),
        weight_soft_limit: h.soft_limit(),
        weight_cooldown: Duration::from_secs(10),
        max_rate_limit_retries: 6,
        initial_backoff: Duration::from_secs(1),
        max_backoff: Duration::from_secs(60),
    }
}

const ASTER_RATE_LIMIT_PERP: KlineRateLimit = aster_rate_limit(ASTER_PERP.history);
const ASTER_RATE_LIMIT_SPOT: KlineRateLimit = aster_rate_limit(ASTER_SPOT.history);

/// Where Aster publishes the `REQUEST_WEIGHT` budget **for the host [`rest_klines_base`] is about to
/// page** — the SAME `rest` host, so the pager and its budget can never name different machines.
/// Paths are the venue's own canonical consts ([`crate::transport::urls::PERP_PATH_EXCHANGE_INFO`] /
/// [`crate::transport::urls::SPOT_PATH_EXCHANGE_INFO`]) rather than fresh literals.
///
/// This is what [`KlineSpec::exchange_info_url`] becoming a `Cow` bought. It used to be a
/// `&'static str`, which Aster could not fill: naming one would have meant either hardcoding a
/// network (which the `venue_url` rows exist to forbid) or pacing one host's backfill against
/// another host's budget. Both halves of that hazard are REAL, not theoretical — MEASURED
/// 2026-08-05 over the four hosts:
///
/// | host | `REQUEST_WEIGHT`/MINUTE |
/// |---|---|
/// | `fapi.asterdex.com` (mainnet perp) | **2400** — matches [`ASTER_PERP`] exactly |
/// | `sapi.asterdex.com` (mainnet spot) | **6000** — NOT 2400; see the note below |
/// | `fapi.asterdex-testnet.com` | **-2** — a nonsense value; rejected, so testnet keeps the fallback |
/// | `sapi.asterdex-testnet.com` | 6000 |
///
/// ⚠ Two consequences worth reading before trusting a number here. The mainnet SPOT host publishes
/// **6000**, and [`vike_venues::venue_rate_limits::ASTER_SPOT`] now declares that — reconciled in
/// the retune that also split this module's `KlineRateLimit` per host, because until then the
/// discovered 6000 was capped by a fapi-shaped `weight_soft_limit` of 2000 applied on top of it.
/// A page on sapi costs weight **5**, not binance's 2 (measured the same day, three requests, a
/// clean +5 each) — the analogy that had been assumed was wrong. And testnet fapi's `-2` is
/// refused by [`vike_binance::family::rate_discovery`]'s positivity check, so a backfill on that host
/// paces on `page_delay` — which is precisely why the URL is composed on the host being paged
/// instead of pinned to whichever network someone typed.
fn rest_exchange_info(rest: &str, perp: bool) -> String {
    let path = if perp {
        crate::transport::urls::PERP_PATH_EXCHANGE_INFO
    } else {
        crate::transport::urls::SPOT_PATH_EXCHANGE_INFO
    };
    format!("{rest}{path}")
}

/// Aster's [`KlineSpec`] for one `(rest host, perp)` target: that book's own fallback budget plus
/// the discovery URL of the host this fetch will page.
///
/// A function rather than a `const` because the URL is composed at call time — which is the ONE
/// thing that changed here. Everything a request or a sleep can depend on is unchanged: the venue
/// label, every rate-limit knob, and the shared `DEFAULT_UTILIZATION`.
fn aster_kline_spec(rest: &str, perp: bool) -> KlineSpec {
    KlineSpec {
        venue: VENUE,
        rate_limit: if perp { ASTER_RATE_LIMIT_PERP } else { ASTER_RATE_LIMIT_SPOT },
        // OWNED: the host is a resolved row, so there is no `&'static str` to borrow.
        exchange_info_url: Some(Cow::Owned(rest_exchange_info(rest, perp))),
        utilization: vike_model::rate_limits::DEFAULT_UTILIZATION,
        // Aster lists ONE settlement plane per book and no coin-margined one, so both its specs
        // read the base asset where every binance-grammar row but a COIN-M one carries it. This
        // field exists for binance's third host; naming the default here is what makes aster's
        // bytes unchanged by that (`VolumeColumn`'s own doc carries the measurement).
        volume_column: VolumeColumn::Index5,
    }
}

// Aster's own published `REQUEST_WEIGHT` `MINUTE` ceiling (2400, MEASURED 2026-08-04 from live
// `exchangeInfo`) and the COMPILE-TIME invariant keeping `weight_soft_limit` under it now live in
// `vike_venues::venue_rate_limits::ASTER_PERP`, beside every other venue's. A ceiling is a FACT ABOUT
// THE VENUE, not a tuning value, so it belongs in the per-venue capability table rather than in the
// one consumer that read it.
//
// The invariant is unchanged in strength: a soft limit at or above the hard ceiling can never fire.
// This value inherited 5000 from Binance's SPOT template and sat above Aster's real ceiling for its
// entire life — its own doc comment said "UNVERIFIED", and no test ever read the number.

/// Spot vs USDⓈ-M-futures host+path for the `/klines` endpoint on the `rest` host already picked for
/// `perp` — the ONE place the venue path is chosen, so the URL-building tests below and the real
/// fetch never drift. Unlike Binance's old `&'static str` consts (which bundled host+path
/// together), this returns an owned `String`: the rows hold bare hosts with no path, so the venue
/// path segment (`/fapi/v3/klines` or `/api/v3/klines`) is appended here.
fn rest_klines_base(rest: &str, perp: bool) -> String {
    let path = if perp { "/fapi/v3/klines" } else { "/api/v3/klines" };
    format!("{rest}{path}")
}

/// The live feed's warmup seed: the newest `limit` klines (no time bound), the last of which is the
/// still-forming bar, on the host of `urls` that `perp` names ([`AsterUrls::rest`]: the USDⓈ-M
/// futures host instead of spot). Delegates to [`klines::fetch_klines_latest`].
pub fn fetch_klines_latest(
    urls: AsterUrls,
    symbol: &str,
    interval: &str,
    limit: usize,
    perp: bool,
) -> Result<Vec<Bar>, String> {
    klines::fetch_klines_latest(
        &rest_klines_base(urls.rest(perp), perp),
        symbol,
        interval,
        limit,
        VENUE,
    )
}

/// The whole routing decision [`fetch_klines_range`] makes — `(base URL, wire symbol, spec)` —
/// hoisted out as a PURE fn so a test can gate it without network I/O. [`rest_klines_base`] has
/// understood `perp` since the live warmup seed was written; the bug this repairs was the paged
/// backfill never ASKING it, and a test that shapes a URL from `rest_klines_base` directly cannot
/// see that gap.
///
/// The spec rides along for the reason binance's does: the host and the budget are ONE decision,
/// not two. Here they are one decision per HOST — mainnet fapi's 2400 has nothing to do with
/// mainnet sapi's 6000 or with testnet fapi's nonsense `-2`. Composing both on the one host this
/// picks is what makes the wrong-host pairing unrepresentable here rather than merely untested.
fn range_target<'a>(source: &KlineSourceAster, symbol: &'a str) -> (String, &'a str, KlineSpec) {
    let (wire, perp) = vike_catalog::split_perp_at(VENUE, symbol);
    let rest = source.rest(perp);
    (rest_klines_base(rest, perp), wire, aster_kline_spec(rest, perp))
}

/// Fetch closed-kline history for the inclusive `[start_ms, end_ms]` window, paging through Aster's
/// 1000-rows/response cap under the family rate-limit policy, on `source`'s resolved hosts. A
/// `symbol` carrying [`vike_venues::PERP_SUFFIX`] routes to the USDⓈ-M futures host and is stripped
/// before it reaches the wire; the CALLER's suffixed symbol remains the store/series key, so a
/// perp's series can never collide with its spot twin's. Delegates to [`klines::fetch_klines_range`]
/// with the spec [`range_target`] built for that host.
pub fn fetch_klines_range(
    source: &KlineSourceAster,
    symbol: &str,
    interval: &str,
    start_ms: i64,
    end_ms: i64,
) -> Result<Vec<Bar>, String> {
    let (base, wire, spec) = range_target(source, symbol);
    klines::fetch_klines_range(&base, wire, interval, start_ms, end_ms, &spec)
}

/// This venue's row in the kline registry (`KLINE_SOURCES`): the store-free fetch half of a
/// backfill. The ingest it feeds is `crates/vike-backfill/src/kline_source.rs`'s
/// `backfill_kline_source`.
///
/// It holds its two REST hosts, resolved ONCE by [`KlineSourceAster::from_urls`] at the `live`
/// `history` rows. Mainnet is not a risk decision here: klines are a KEYLESS public read, so the
/// standing "exclude aster from demo/validation runs" rule — which is about authenticated mainnet
/// account calls — does not reach this path. Testnet would be the wrong default anyway: it carries
/// no real price history to backfill. Perp vs spot is carried by the SYMBOL, not by a flag:
/// `BTCUSDT.P` routes to the USDⓈ-M futures host and has its `.P` stripped before the wire, INSIDE
/// the bridge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KlineSourceAster {
    fapi_rest: String,
    sapi_rest: String,
}

impl KlineSourceAster {
    /// The backfill's two hosts: `live` `history` `rest`, products `usdm` and `spot`.
    ///
    /// # Errors
    /// The row the table cannot answer.
    pub fn from_urls(urls: &VenueUrls) -> Result<KlineSourceAster, UrlRefusal> {
        let at = |product| {
            urls.url(UrlType::Live, UrlChannel::History, UrlProtocol::Rest, product)
                .map(str::to_string)
        };
        Ok(KlineSourceAster { fapi_rest: at(PERP_PRODUCT)?, sapi_rest: at(SPOT_PRODUCT)? })
    }

    /// The REST host of the book `perp` names.
    fn rest(&self, perp: bool) -> &str {
        if perp { &self.fapi_rest } else { &self.sapi_rest }
    }
}

impl vike_data::source::KlineSource for KlineSourceAster {
    fn venue(&self) -> &str {
        crate::transport::urls::VENUE
    }

    /// The bridge's own paged `fetch_klines_range` verbatim, on the held `live` hosts — the `.P`
    /// perp routing happens inside the bridge.
    fn fetch(
        &self,
        symbol: &str,
        interval: &str,
        start_ms: i64,
        end_ms: i64,
    ) -> Result<Vec<Bar>, vike_data::source::SourceError> {
        fetch_klines_range(self, symbol, interval, start_ms, end_ms)
            .map_err(|why| vike_data::source::SourceError::Fetch(why.into()))
    }
}

// --- funding-rate source -------------------------------------------------------------------------

/// The funding-rate history path on the USDⓈ-M futures host: binance's, on aster's host.
const PERP_PATH_FUNDING_RATE: &str = "/fapi/v1/fundingRate";
/// Rows per funding page. MEASURED 2026-11-08: `limit=1000` from a `startTime` 13 months back answered
/// exactly 1000 rows starting AT `startTime`, so the endpoint is start-anchored like binance's and pages
/// FORWARD (`vike_data::source::page_walk_forward`).
const FUNDING_PAGE_CAP: usize = 1000;

/// The funding-rate URL of one forward page on the futures host `fapi_rest` (the `live` `data` row
/// at the one caller: history is a keyless public read, and the testnet carries no real history).
fn funding_url(fapi_rest: &str, wire: &str, cursor: i64, end_ms: i64) -> String {
    format!(
        "{fapi_rest}{PERP_PATH_FUNDING_RATE}?symbol={wire}&startTime={cursor}&endTime={end_ms}\
         &limit={FUNDING_PAGE_CAP}"
    )
}

/// This venue's market funding-rate source, a row of the gatewayhub's `FUNDING_SOURCES`: binance's
/// funding-rate wire on aster's PUBLIC mainnet futures host, decoded by binance's own parser
/// (`vike_binance::history::parse_funding_rates`; aster's rows are `{symbol, fundingTime, fundingRate}`,
/// measured 2026-11-08, so no mark and no premium). Keyless: no account, no signature, which is why the
/// keep-aster-out-of-demo-runs rule (about authenticated mainnet reads) does not reach it, exactly as it
/// does not reach [`KlineSourceAster`]. Cadence 8h on `BTCUSDT`; the venue stamps a few ms past the hour
/// (`…001`, `…005`), which the ingest floors onto the hour.
///
/// The store spells a perpetual with [`vike_venues::PERP_SUFFIX`] (`BTCUSDT.P`, the key the kline
/// lane files perp bars under; a bare name is the spot pair), so a bare symbol is REFUSED before
/// anything is asked: spot has no funding.
///
/// It holds its futures host, resolved ONCE by [`FundingRateSourceAster::from_urls`] at the `live` `data` row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FundingRateSourceAster {
    fapi_rest: String,
}

impl FundingRateSourceAster {
    /// The funding-rate history's host: `live` `data` `rest`, product `usdm` (the public rate is
    /// market data, never the account's own funding).
    ///
    /// # Errors
    /// The row the table cannot answer.
    pub fn from_urls(urls: &VenueUrls) -> Result<FundingRateSourceAster, UrlRefusal> {
        let fapi_rest =
            urls.url(UrlType::Live, UrlChannel::Data, UrlProtocol::Rest, PERP_PRODUCT)?;
        Ok(FundingRateSourceAster { fapi_rest: fapi_rest.to_string() })
    }
}

impl vike_data::source::FundingRateSource for FundingRateSourceAster {
    fn venue(&self) -> &str {
        crate::transport::urls::VENUE
    }

    fn fetch(
        &self,
        symbol: &str,
        start_ms: i64,
        end_ms: i64,
    ) -> Result<Vec<vike_marketdata::FundingRatePoint>, vike_data::source::SourceError> {
        use vike_data::source::SourceError;
        // A bare (spot) symbol is refused before anything is asked, in aster's words: the family's rule.
        let wire = vike_binance::history::funding_wire(VENUE, symbol, "")?;
        vike_binance::history::funding_walk_forward(
            start_ms,
            end_ms,
            FUNDING_PAGE_CAP,
            ASTER_PERP.history.page_delay(),
            "aster-funding",
            |at| {
                SourceError::Fetch(
                    at.text(VENUE, symbol, vike_data::source::FUNDING_INTERVAL).into(),
                )
            },
            |cursor| funding_url(&self.fapi_rest, wire, cursor, end_ms),
        )
    }
}

#[path = "history_tests.rs"]
#[cfg(test)]
mod history_tests;
