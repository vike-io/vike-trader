//! Binance Spot/perp REST kline history — the venue face of the shared
//! [`crate::family::klines`] rung (F, dedup rung 6).
//!
//! Both the live market feed's REST warmup seed (`fetch_klines_latest`, "the newest N klines, last
//! one still forming") and the vike-backfill historical backfill (`fetch_klines_range`, a paged
//! `[start, end]` window) delegate to the shared family kline core; this module supplies only
//! Binance's OWN host resolution and the WIRING of its rate-limit budget — the budget's NUMBERS
//! live in `vike_model::venue_rate_limits`'s `BINANCE_SPOT`/`BINANCE_PERP` rows, because a venue's
//! published ceiling is a fact about the venue rather than a constant of this crate — and keeps the
//! public surface (`parse_klines`/`fetch_klines_latest`/`fetch_klines_range`) unchanged for
//! vike-backfill and the feed. See [`crate::family::klines`] for the endpoint/response/rate-limit
//! contract.
//!
//! Endpoint: `GET https://api.binance.com/api/v3/klines?symbol=&interval=&limit=&[startTime=]&[endTime=]`
//! (spot), or `GET https://fapi.binance.com/fapi/v1/klines?…` (perp — the `.P` suffix on the
//! caller's symbol selects it, on BOTH the warmup seed and the paged range backfill). Binance's hosts
//! are hardcoded `&'static str` consts — never env-resolved — so they stay exactly what they were
//! before the family module existed.

use std::borrow::Cow;
use std::time::Duration;

use vike_model::Bar;
use vike_model::rate_limits::PaceSample;
use vike_model::venue_rate_limits::{BINANCE_PERP, BINANCE_SPOT};

use crate::family::klines::{self, KlineRateLimit, KlineSpec};
use crate::spot::VENUE;

/// Re-exported pure map (venue-agnostic) — kept at `vike_binance::data::parse_klines` for
/// vike-backfill's backfill + its fixture test. See [`klines::parse_klines`].
pub use crate::family::klines::parse_klines;

const REST_KLINES: &str = "https://api.binance.com/api/v3/klines";
/// USDS-M futures (fapi) twin of [`REST_KLINES`] — same response shape, just a different host/path.
/// Reached by the live feed's perp seed (`fetch_klines_latest(.., perp: true)`) AND by the paged
/// range backfill, which derives the same flag from the caller's [`vike_catalog::PERP_SUFFIX`].
const REST_KLINES_FAPI: &str = "https://fapi.binance.com/fapi/v1/klines";

/// Where SPOT publishes its own `REQUEST_WEIGHT` budget — the same host [`REST_KLINES`] pages, which
/// is the only host whose budget applies to it. Read once per backfill by
/// [`klines::fetch_klines_range`]; see that module's "Discovered pacing".
const REST_EXCHANGE_INFO: &str = "https://api.binance.com/api/v3/exchangeInfo";
/// The fapi twin of [`REST_EXCHANGE_INFO`], paired with [`REST_KLINES_FAPI`]. A SEPARATE const for
/// the same reason the two specs are separate: fapi's published budget is 2400/min, spot's is 6000.
const REST_EXCHANGE_INFO_FAPI: &str = "https://fapi.binance.com/fapi/v1/exchangeInfo";

/// Binance's SPOT paged-backfill budget. **MEASURED 2026-08-04** from the CI box, not assumed:
/// `x-mbx-used-weight` reports **2** for `/api/v3/klines?limit=1000`, against the
/// `REQUEST_WEIGHT` `MINUTE` limit of **6000** in live `exchangeInfo`. That is 3000 req/min of
/// headroom; `page_delay` of 50ms yields ~1200 req/min ⇒ ~2400 weight/min, ~40% of budget.
///
/// Those numbers are now the FALLBACK, not the plan: with [`REST_EXCHANGE_INFO`] wired, the pager
/// reads the 6000 off the venue each run and derives the gap from the observed per-request weight,
/// so `page_delay` applies only when discovery does not answer. It is kept — and kept accurate —
/// precisely because that is a mode the pager must still be correct in.
///
/// **The two rate numbers below are no longer this crate's to choose.** `page_delay` and
/// `weight_soft_limit` come from [`vike_model::venue_rate_limits::BINANCE_SPOT`], the workspace's
/// per-venue rate-limit table, which also carries the 6000 ceiling they are sized against and
/// enforces `soft_limit < ceiling` at compile time for every venue at once. The retry/backoff knobs
/// beside them (`weight_cooldown`, `max_rate_limit_retries`, `initial_backoff`, `max_backoff`) are
/// this pager's own POLICY, not venue facts, so they stay here.
const BINANCE_KLINE_SPOT: KlineSpec = KlineSpec {
    venue: VENUE,
    rate_limit: KlineRateLimit {
        page_delay: BINANCE_SPOT.history.page_delay(),
        weight_soft_limit: BINANCE_SPOT.history.soft_limit(),
        weight_cooldown: Duration::from_secs(10),
        max_rate_limit_retries: 6,
        initial_backoff: Duration::from_secs(1),
        max_backoff: Duration::from_secs(60),
    },
    // `Cow::Borrowed` of the SAME `&'static str` this field held before it became a `Cow`: binance's
    // hosts are compile-time constants, so the spec stays a `const` and the request is unchanged.
    exchange_info_url: Some(Cow::Borrowed(REST_EXCHANGE_INFO)),
    utilization: vike_model::rate_limits::DEFAULT_UTILIZATION,
};

/// Binance's USDⓈ-M FUTURES budget — a SEPARATE const, because fapi's is not spot's.
///
/// **MEASURED 2026-08-04**: fapi `exchangeInfo` reports `REQUEST_WEIGHT` `MINUTE` = **2400**
/// (spot's is 6000), and `/fapi/v1/klines?limit=1000` costs weight **5** (confirmed on a clean
/// probe of Aster's byte-identical fapi twin, whose counter carried no concurrent traffic).
/// 480 req/min ceiling. See the RTT note below before reasoning about what a delay costs.
///
/// ⚠ **`weight_soft_limit` MUST stay below 2400.** Both hosts previously shared ONE spec with
/// `weight_soft_limit: 5000` — the SPOT budget. On this path that guard is unreachable dead code:
/// binance's hard 2400 ceiling (HTTP 418 → IP ban) fires long before a 5000 counter could. It was
/// harmless only while a fixed 300ms delay held usage near 600/min — safety by accident, not by
/// design — and became reachable at all when #1029 first routed the range backfill to fapi.
///
/// ⚠ **Reason about this budget as sleep + RTT, never sleep alone.** The zero-RTT arithmetic says
/// 150 ms ⇒ 400 req/min ⇒ ~2000 weight/min, 83 % of the ceiling — and it is wrong by ~3.6x, because
/// each request's own round trip (~290 ms from the CI box) dominates the sleep. MEASURED end-to-end
/// 2026-08-04: a full 24-month `BTCUSDT.P` backfill at this delay finished in **8m00s** and left
/// `x-mbx-used-weight-1m` at **556**, i.e. **~23 %** of the 2400 ceiling. An earlier draft of this
/// comment quoted the 83 % figure and argued for doubling the delay; it would have made a
/// comfortably-safe path 2x slower for nothing.
///
/// The pacer HAS since folded RTT in: each page is wall-clocked and fed to
/// `Pacer::observe_request`, which subtracts the measured request time from the next sleep, so
/// `utilization` now means what it says END TO END (0.40 of 2400 ⇒ ~192 req/min ⇒ ~960 weight/min,
/// versus the ~506 the sleep-only arithmetic delivered). Two things follow. First, this
/// `page_delay` is now a genuinely SLOWER fallback than the discovered pace, not a coincidentally
/// similar one — which is the right shape for a fallback. Second, `utilization` became a live knob:
/// raising it now raises real consumption roughly proportionally, where before it bought about half
/// of what it asked for. Keep it under `weight_soft_limit`'s share of the ceiling.
///
/// Same split of ownership as the spot spec above: `page_delay`/`weight_soft_limit` come from
/// [`vike_model::venue_rate_limits::BINANCE_PERP`] (which carries the 2400 ceiling and asserts the
/// invariant), the retry/backoff knobs stay this pager's own policy.
const BINANCE_KLINE_PERP: KlineSpec = KlineSpec {
    venue: VENUE,
    rate_limit: KlineRateLimit {
        page_delay: BINANCE_PERP.history.page_delay(),
        weight_soft_limit: BINANCE_PERP.history.soft_limit(),
        weight_cooldown: Duration::from_secs(10),
        max_rate_limit_retries: 6,
        initial_backoff: Duration::from_secs(1),
        max_backoff: Duration::from_secs(60),
    },
    exchange_info_url: Some(Cow::Borrowed(REST_EXCHANGE_INFO_FAPI)),
    utilization: vike_model::rate_limits::DEFAULT_UTILIZATION,
};

// The two published `REQUEST_WEIGHT` `MINUTE` ceilings (SPOT 6000, fapi 2400 — MEASURED 2026-08-04
// from live `exchangeInfo` on both hosts) and the COMPILE-TIME invariant that keeps each spec's
// `weight_soft_limit` under its own host's ceiling now live in
// `vike_model::venue_rate_limits::{BINANCE_SPOT, BINANCE_PERP}`, beside every other venue's. They
// are FACTS ABOUT THE VENUE, not tuning values — not ours to raise — which is why they belong in the
// per-venue capability table rather than in the one consumer that happened to read them.
//
// The invariant itself is unchanged in strength: a soft limit at or above the hard ceiling can never
// fire, so the guard silently becomes dead code and the venue's 418 -> IP ban is what stops you
// instead. Both hosts shared ONE spec at 5000 (the SPOT budget) until 2026-08-04 and nothing caught
// it, because nothing read the number. The build still refuses — one `const _: ()` per table row.

/// Spot vs USDS-M-futures host — the ONE place that decides fapi-vs-api, for BOTH
/// `fetch_klines_latest`'s seed request and `fetch_klines_range`'s paged backfill, so the
/// URL-building tests below and the real fetches never drift.
fn rest_klines_base(perp: bool) -> &'static str {
    if perp { REST_KLINES_FAPI } else { REST_KLINES }
}

/// The live feed's warmup seed: the newest `limit` klines (no time bound), the last of which is the
/// still-forming bar. `perp = true` routes to the USDS-M futures host ([`REST_KLINES_FAPI`]) instead
/// of spot ([`REST_KLINES`]). Delegates to [`klines::fetch_klines_latest`].
pub fn fetch_klines_latest(
    symbol: &str,
    interval: &str,
    limit: usize,
    perp: bool,
) -> Result<Vec<Bar>, String> {
    klines::fetch_klines_latest(rest_klines_base(perp), symbol, interval, limit, VENUE)
}

/// Fetch closed-kline history for the inclusive `[start_ms, end_ms]` window, paging through Binance's
/// 1000-rows/response cap under the family rate-limit policy. A `symbol` carrying
/// [`vike_catalog::PERP_SUFFIX`] routes to the USDⓈ-M futures host and is stripped before it reaches
/// the wire; the CALLER's suffixed symbol remains the store/series key, so a perp's series can never
/// collide with its spot twin's. Delegates to [`klines::fetch_klines_range`] with Binance's
/// [`BINANCE_KLINE`] spec.
pub fn fetch_klines_range(
    symbol: &str,
    interval: &str,
    start_ms: i64,
    end_ms: i64,
) -> Result<Vec<Bar>, String> {
    let (base, wire, spec) = range_target(symbol);
    klines::fetch_klines_range(base, wire, interval, start_ms, end_ms, spec)
}

/// [`fetch_klines_range`] with the pace measurement carried across runs — `seed` is the previous
/// run's observation for THIS symbol's market, and the second element of the return is this run's
/// (`None` unless a page was timed). See [`klines::fetch_klines_range_paced`]; `fetch_klines_range`
/// is this call with `None` in and the report dropped, so it is byte-identical to what it was.
///
/// Pair the seed with [`kline_market`] — a spot record must never seed a perp fetch, and the pacer
/// enforces that independently by refusing a sample whose budget is not the one it discovered.
pub fn fetch_klines_range_paced(
    symbol: &str,
    interval: &str,
    start_ms: i64,
    end_ms: i64,
    seed: Option<&PaceSample>,
) -> Result<(Vec<Bar>, Option<PaceSample>), String> {
    let (base, wire, spec) = range_target(symbol);
    klines::fetch_klines_range_paced(base, wire, interval, start_ms, end_ms, spec, seed)
}

/// [`fetch_klines_range_paced`] with BOUNDED CONCURRENCY — several disjoint sub-windows in flight
/// instead of one, at the SAME aggregate rate. See [`klines::fetch_klines_range_lanes`] for the full
/// contract; the two facts a caller here needs:
///
/// * **The venue-facing rate does not change.** Every lane draws its dispatch slot from one shared
///   pacer, so binance still sees `utilization` x its published `REQUEST_WEIGHT` ceiling. What
///   changes is that the target becomes reachable: a sequential pager cannot space requests closer
///   than one request TAKES, and on the SPOT host the target gap (50 ms at the 0.40 default and the
///   measured weight-2 page cost) is far narrower than the ~280 ms round trip.
/// * **The lane count is derived per host, and is often 1.** `ceil(request_time / target_gap)` from
///   this run's own probe pages — MEASURED, that is **1 lane on fapi** (a 312 ms target gap is wider
///   than the round trip, so perp backfills are already at target and this call is the sequential one
///   with an extra `Option`) and **6 on spot**. ⚠ The spot figure needs the venue's real weight-2
///   page cost, which is a DELTA and therefore takes TWO readings — [`klines::PROBE_PAGES`] is that
///   rule. With one probe page the pacer is still on its pessimistic seed and would derive 3 lanes
///   on a gap twice too wide.
///
/// Worth, on spot: 213 requests/min sequential (the round trip plus a floored sleep) against the
/// gate's 1200/min ⇒ a 24-month 1m backfill goes ~4.9 min -> ~55 s, about **5.4x**, settling nearer
/// 4.8x on a longer run once the end-of-window cooldown starts firing. Arithmetic from the measured
/// round trip and the published budget — **not timed end to end**. Perp is unchanged.
///
/// The cap is [`vike_bridge_core::concurrent::MAX_LANES`] — a bound on THREADS and sockets, not on
/// rate, applied HERE (the venue face) rather than asked of every caller, so a backfill seam never
/// has to hold an opinion about a number that cannot change what the venue is sent. A caller that
/// wants the strictly sequential path calls [`fetch_klines_range_paced`], which is unchanged.
pub fn fetch_klines_range_lanes(
    symbol: &str,
    interval: &str,
    start_ms: i64,
    end_ms: i64,
    seed: Option<&PaceSample>,
) -> Result<(Vec<Bar>, Option<PaceSample>), String> {
    let (base, wire, spec) = range_target(symbol);
    klines::fetch_klines_range_lanes(
        base,
        wire,
        interval,
        start_ms,
        end_ms,
        spec,
        seed,
        vike_bridge_core::concurrent::MAX_LANES,
    )
}

/// Which MARKET a caller's symbol routes to — `"perp"` for a [`vike_catalog::PERP_SUFFIX`] symbol,
/// `"spot"` otherwise. The half of a persisted pace record's key that is not the venue.
///
/// Exposed (rather than left to the caller's own suffix check) because it is the SAME routing
/// decision [`range_target`] makes, and the two must not be able to disagree: binance's hosts
/// publish different budgets (6000 vs 2400/min) and price `/klines` differently (weight 2 vs 5), so
/// a record filed under the wrong market is a record that paces the other host wrong.
pub fn kline_market(symbol: &str) -> &'static str {
    if vike_catalog::split_perp(symbol).1 { "perp" } else { "spot" }
}

/// The `(base, wire_symbol, rate_limit_spec)` triple a range fetch routes to — extracted out of
/// [`fetch_klines_range`] so the routing DECISION is assertable without network I/O.
///
/// Testing [`rest_klines_base`] directly proves nothing about this: that helper has understood
/// `perp` since the live warmup seed was written, and the bug was that `fetch_klines_range` never
/// asked it. This function is what the fetcher actually calls, so a test against it fails if the
/// fetcher is ever re-pinned to spot — and if the fetcher stops calling it, `dead_code` trips the
/// `-D warnings` merge gate.
///
/// The spec rides along because host and budget are ONE decision, not two: fapi's 2400-weight
/// ceiling belongs to the fapi host, and pairing a host with the other host's budget is exactly
/// the defect the two consts above document.
fn range_target(symbol: &str) -> (&'static str, &str, &'static KlineSpec) {
    let (wire, perp) = vike_catalog::split_perp(symbol);
    let spec = if perp { &BINANCE_KLINE_PERP } else { &BINANCE_KLINE_SPOT };
    (rest_klines_base(perp), wire, spec)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::family::klines::klines_url;

    /// The FETCHER's own routing decision — the gate the URL-shaping tests below cannot provide,
    /// because they build a URL from `rest_klines_base` directly and that helper was already
    /// perp-capable before this fix. Re-pinning `fetch_klines_range` to spot fails HERE.
    #[test]
    fn range_target_routes_a_perp_symbol_to_fapi_with_the_suffix_stripped() {
        let (base, wire, spec) = range_target("BTCUSDT.P");
        assert_eq!((base, wire), (REST_KLINES_FAPI, "BTCUSDT"));
        assert_eq!(spec, &BINANCE_KLINE_PERP, "the fapi host must carry the FAPI budget");
    }

    /// A bare symbol still resolves the spot host and is passed through untouched.
    #[test]
    fn range_target_routes_a_bare_symbol_to_spot_unchanged() {
        let (base, wire, spec) = range_target("BTCUSDT");
        assert_eq!((base, wire), (REST_KLINES, "BTCUSDT"));
        assert_eq!(spec, &BINANCE_KLINE_SPOT, "the spot host must carry the SPOT budget");
    }

    /// The persisted-pace key must be the SAME decision the fetcher routes on, or a record files
    /// itself under one market and paces the other. Asserted against `range_target` itself rather
    /// than against a `.P` literal, which is what makes the two unable to drift.
    #[test]
    fn kline_market_agrees_with_the_fetchers_own_routing() {
        for symbol in ["BTCUSDT", "BTCUSDT.P", "ETHUSDT", "ETHUSDT.P", "1000PEPEUSDT.P"] {
            let (base, _, spec) = range_target(symbol);
            let expected = if base == REST_KLINES_FAPI { "perp" } else { "spot" };
            assert_eq!(kline_market(symbol), expected, "{symbol} routes to {base}");
            // ...and the market therefore names the budget that record was measured against.
            let want = if expected == "perp" { BINANCE_KLINE_PERP } else { BINANCE_KLINE_SPOT };
            assert_eq!(spec, &want);
        }
        // The two markets are genuinely distinct keys, so a perp record can never overwrite a spot
        // one on the same venue.
        assert_ne!(kline_market("BTCUSDT"), kline_market("BTCUSDT.P"));
    }

    /// The ceilings are asserted at COMPILE time (now in `vike_model::venue_rate_limits`, one
    /// `const _: ()` per row), which is strictly stronger than this test could be — a violation
    /// fails the build rather than a test run.
    ///
    /// What this test does gate is the WIRING: the literals below are the numbers these two specs
    /// carried as local consts before the per-venue table existed, so if the table's binance rows
    /// ever drift, this pager's pacing changes and this test says so. That is the byte-identical
    /// claim of the move, written down.
    #[test]
    fn the_specs_pace_on_the_venue_tables_binance_rows() {
        assert_eq!(BINANCE_KLINE_SPOT.rate_limit.weight_soft_limit, 4800);
        assert_eq!(BINANCE_KLINE_SPOT.rate_limit.page_delay, Duration::from_millis(50));
        assert_eq!(BINANCE_KLINE_PERP.rate_limit.weight_soft_limit, 2000);
        assert_eq!(BINANCE_KLINE_PERP.rate_limit.page_delay, Duration::from_millis(150));
        // ...and each soft limit is genuinely under ITS OWN host's published ceiling (6000 spot,
        // 2400 fapi) — the pairing whose absence let one shared spec sit above fapi's ban threshold.
        assert_eq!(BINANCE_SPOT.history.ceiling_per_min(), 6000);
        assert_eq!(BINANCE_PERP.history.ceiling_per_min(), 2400);
        assert!(
            BINANCE_KLINE_SPOT.rate_limit.weight_soft_limit
                < BINANCE_SPOT.history.ceiling_per_min()
        );
        assert!(
            BINANCE_KLINE_PERP.rate_limit.weight_soft_limit
                < BINANCE_PERP.history.ceiling_per_min()
        );
    }

    /// A discovered budget belongs to the HOST that published it, so each spec's `exchangeInfo` URL
    /// must live on the same host as the `/klines` URL it paces. Crossing them would pace the
    /// 2400-weight fapi host against spot's 6000 — the same class of defect as the shared
    /// `weight_soft_limit: 5000`, only now derived from the venue rather than hand-typed.
    ///
    /// Two things changed when the spec's URL became env-resolvable. The pairing is now the SHARED
    /// [`klines::discovery_url`] — the same gate the runtime consults and the same one aster's tests
    /// drive over its four (`Environment` x market) hosts — instead of a `host()` helper private to
    /// this file. And it is asserted against the base [`range_target`] ACTUALLY RESOLVES rather than
    /// against a literal paired by hand here, so re-pointing the fetcher fails this test too.
    #[test]
    fn each_spec_discovers_the_budget_of_the_host_it_pages() {
        for symbol in ["BTCUSDT", "BTCUSDT.P"] {
            let (base, _, spec) = range_target(symbol);
            let info = klines::discovery_url(spec, base)
                .unwrap_or_else(|| panic!("{symbol}: binance publishes a budget on both hosts"));
            assert_eq!(
                klines::url_host(info),
                klines::url_host(base),
                "{info} must be the budget of {base}'s host"
            );
        }
        // ...and the two hosts are genuinely different, so the pairing above is not vacuous.
        assert_ne!(BINANCE_KLINE_SPOT.exchange_info_url, BINANCE_KLINE_PERP.exchange_info_url);
        // CROSSED, which is the defect itself: each spec against the OTHER host's base discovers
        // nothing at all, so a mispairing degrades to the fixed `page_delay` instead of pacing fapi
        // against spot's 6000.
        assert_eq!(klines::discovery_url(&BINANCE_KLINE_SPOT, REST_KLINES_FAPI), None);
        assert_eq!(klines::discovery_url(&BINANCE_KLINE_PERP, REST_KLINES), None);
    }

    /// **Byte-identity of the discovery request across the `&'static str` -> `Cow` change.** The
    /// URL each spec is asked for is the SAME literal it named before, so both hosts issue the same
    /// one `exchangeInfo` GET they issued before, read the same budget, and pace the same way. The
    /// literals are spelled out rather than compared to the consts on purpose: a test that compares
    /// a const to itself would pass through a rename of the endpoint.
    #[test]
    fn the_discovery_urls_are_the_literals_they_have_always_been() {
        let (spot_base, _, spot) = range_target("BTCUSDT");
        let (perp_base, _, perp) = range_target("BTCUSDT.P");
        assert_eq!(
            klines::discovery_url(spot, spot_base),
            Some("https://api.binance.com/api/v3/exchangeInfo")
        );
        assert_eq!(
            klines::discovery_url(perp, perp_base),
            Some("https://fapi.binance.com/fapi/v1/exchangeInfo")
        );
        // Both are BORROWED — binance's hosts are compile-time constants, so nothing is allocated
        // and the two specs remain `const`. (Aster's are `Owned`; that is the capability the field's
        // type change exists for.)
        for spec in [&BINANCE_KLINE_SPOT, &BINANCE_KLINE_PERP] {
            assert!(
                matches!(spec.exchange_info_url, Some(Cow::Borrowed(_))),
                "a static host must not allocate"
            );
        }
    }

    /// Utilization is vike-model's operator-facing default at every const site — this crate does not
    /// carry a second copy of the number, and a venue does not get to pick its own risk appetite.
    #[test]
    fn both_specs_target_the_shared_default_utilization() {
        for spec in [BINANCE_KLINE_SPOT, BINANCE_KLINE_PERP] {
            assert_eq!(
                spec.utilization.to_bits(),
                vike_model::rate_limits::DEFAULT_UTILIZATION.to_bits()
            );
        }
    }

    // Binance's OWN host-resolution proof (the per-venue delta): the shared pure `parse_klines`/URL
    // shaping is proven once at the rung (`crate::family::klines`); here we prove the api/fapi hosts.
    #[test]
    fn klines_url_includes_optional_bounds() {
        let latest = klines_url(REST_KLINES, "BTCUSDT", "1m", None, None, 1000);
        assert_eq!(
            latest,
            "https://api.binance.com/api/v3/klines?symbol=BTCUSDT&interval=1m&limit=1000"
        );
        let ranged = klines_url(REST_KLINES, "BTCUSDT", "1m", Some(10), Some(20), 1000);
        assert!(ranged.ends_with("&startTime=10&endTime=20"));
    }

    #[test]
    fn klines_url_perp_uses_fapi_host_spot_uses_api_host() {
        let perp = klines_url(REST_KLINES_FAPI, "BTCUSDT", "1m", None, None, 1000);
        assert_eq!(
            perp,
            "https://fapi.binance.com/fapi/v1/klines?symbol=BTCUSDT&interval=1m&limit=1000"
        );
        let spot = klines_url(REST_KLINES, "BTCUSDT", "1m", None, None, 1000);
        assert_eq!(
            spot,
            "https://api.binance.com/api/v3/klines?symbol=BTCUSDT&interval=1m&limit=1000"
        );
    }

    #[test]
    fn fetch_klines_latest_base_url_selects_fapi_for_perp() {
        // rest_klines_base is the pure host-selection the perp seed relies on — no network.
        assert_eq!(rest_klines_base(true), REST_KLINES_FAPI);
        assert_eq!(rest_klines_base(false), REST_KLINES);
    }

    /// The perp suffix selects the fapi host AND is stripped from the wire symbol. This is the
    /// site the `.P` convention was skipped at: before this, `BTCUSDT.P` silently fetched SPOT
    /// bars and stored them under the perp key.
    #[test]
    fn perp_suffix_selects_fapi_and_strips_the_wire_symbol() {
        let (wire, perp) = vike_catalog::split_perp("BTCUSDT.P");
        assert_eq!(wire, "BTCUSDT");
        assert!(perp);
        let url = klines_url(rest_klines_base(perp), wire, "1m", Some(10), Some(20), 1000);
        assert!(url.starts_with(REST_KLINES_FAPI), "perp must use fapi: {url}");
        assert!(url.contains("symbol=BTCUSDT&"), "the .P must not reach the wire: {url}");
        assert!(!url.contains(".P"), "the .P must not reach the wire: {url}");
    }

    /// A bare symbol is unchanged — spot URLs stay byte-identical.
    #[test]
    fn a_bare_symbol_still_builds_the_spot_url() {
        let (wire, perp) = vike_catalog::split_perp("BTCUSDT");
        assert!(!perp);
        let url = klines_url(rest_klines_base(perp), wire, "1m", None, None, 1000);
        assert_eq!(
            url,
            "https://api.binance.com/api/v3/klines?symbol=BTCUSDT&interval=1m&limit=1000"
        );
    }
}
