//! Aster REST kline history — the venue face of the shared [`vike_binance::family::klines`] rung
//! (F, dedup rung 6). Aster's kline wire format (12-element array, decimal-string OHLCV, 1000-row
//! cap, `X-MBX-USED-WEIGHT-1M`-header-aware paged backfill) is byte-identical to Binance's, so both
//! the live feed's warmup seed (`fetch_klines_latest`) and a paged historical backfill
//! (`fetch_klines_range`) delegate to the family core.
//!
//! This module supplies only Aster's OWN two deltas: host resolution — resolved from
//! [`crate::urls::urls_for`] (env-keyed testnet/mainnet, spot vs perp) instead of Binance's
//! hardcoded consts — and the WIRING of Aster's rate-limit budget, whose FALLBACK numbers live in
//! `vike_model::venue_rate_limits`'s `ASTER_PERP` row (a venue's published ceiling is a fact about
//! the venue, not a constant of this crate). The public surface
//! (`parse_klines`/`fetch_klines_latest`/`fetch_klines_range`) is unchanged. See
//! [`vike_binance::family::klines`] for the endpoint/response/rate-limit contract.
//!
//! Endpoint: `GET {sapi_rest}/api/v3/klines?…` (spot) or `GET {fapi_rest}/fapi/v3/klines?…` (perp) —
//! `env` (via [`crate::urls::urls_for`]) picks testnet vs mainnet hosts.
//!
//! **Aster DISCOVERS its budget** like Binance does (one keyless `exchangeInfo` GET per paged
//! backfill, then the pager targets a fraction of what the venue published). Its discovery endpoint
//! is env-resolved exactly as its `/klines` endpoint is — both fall out of the same
//! [`urls::urls_for`] call in [`range_target`] — which is why the spec is BUILT per call
//! ([`aster_kline_spec`]) rather than being a `const`, and why the shared rung's URL field is a
//! `Cow`. See [`rest_exchange_info`] for what each of the four hosts actually publishes.

use std::borrow::Cow;
use std::time::Duration;

use vike_binance::family::klines::{self, KlineRateLimit, KlineSpec};
use vike_bridge_core::Environment;
use vike_model::Bar;
use vike_model::venue_rate_limits::{ASTER_PERP, ASTER_SPOT, History};

use crate::urls;
use crate::urls::VENUE;

/// Re-exported pure map (venue-agnostic) — kept at `vike_aster::data::parse_klines`. See
/// [`klines::parse_klines`].
pub use vike_binance::family::klines::parse_klines;

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
/// come from the workspace's per-venue table ([`vike_model::venue_rate_limits::ASTER_SPOT`] /
/// [`vike_model::venue_rate_limits::ASTER_PERP`]), which also carries the ceiling they are sized
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
const fn aster_rate_limit(h: History) -> KlineRateLimit {
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
/// page** — the same `(env, perp)` decision, resolved through the same [`urls::urls_for`] table, so
/// the pager and its budget can never name different machines. Paths are the venue's own canonical
/// consts ([`crate::urls::PERP_PATH_EXCHANGE_INFO`] / [`crate::urls::SPOT_PATH_EXCHANGE_INFO`]) rather than
/// fresh literals.
///
/// This is what [`KlineSpec::exchange_info_url`] becoming a `Cow` bought. It used to be a
/// `&'static str`, which Aster could not fill: naming one would have meant either hardcoding a
/// network (which [`crate::urls`] exists to forbid) or pacing a testnet backfill against the
/// mainnet host's budget. Both halves of that hazard are REAL, not theoretical — MEASURED
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
/// **6000**, and [`vike_model::venue_rate_limits::ASTER_SPOT`] now declares that — reconciled in
/// the retune that also split this module's `KlineRateLimit` per host, because until then the
/// discovered 6000 was capped by a fapi-shaped `weight_soft_limit` of 2000 applied on top of it.
/// A page on sapi costs weight **5**, not binance's 2 (measured the same day, three requests, a
/// clean +5 each) — the analogy that had been assumed was wrong. And testnet fapi's `-2` is
/// refused by [`vike_bridge_core::rate_discovery`]'s positivity check, so a testnet backfill paces
/// on `page_delay` exactly as it does today — which is precisely why the URL had to be resolved per
/// `Environment` instead of pinned to whichever network someone typed.
fn rest_exchange_info(env: Environment, perp: bool) -> String {
    let u = urls::urls_for(env);
    if perp {
        format!("{}{}", u.fapi_rest, crate::urls::PERP_PATH_EXCHANGE_INFO)
    } else {
        format!("{}{}", u.sapi_rest, crate::urls::SPOT_PATH_EXCHANGE_INFO)
    }
}

/// Aster's [`KlineSpec`] for one `(env, perp)` target: that host's own fallback budget plus
/// the discovery URL of the host this fetch will page.
///
/// A function rather than a `const` because the URL is composed at call time — which is the ONE
/// thing that changed here. Everything a request or a sleep can depend on is unchanged: the venue
/// label, every rate-limit knob, and the shared `DEFAULT_UTILIZATION`.
fn aster_kline_spec(env: Environment, perp: bool) -> KlineSpec {
    KlineSpec {
        venue: VENUE,
        rate_limit: if perp { ASTER_RATE_LIMIT_PERP } else { ASTER_RATE_LIMIT_SPOT },
        // OWNED: the host is `env`-resolved, so there is no `&'static str` to borrow.
        exchange_info_url: Some(Cow::Owned(rest_exchange_info(env, perp))),
        utilization: vike_model::rate_limits::DEFAULT_UTILIZATION,
    }
}

// Aster's own published `REQUEST_WEIGHT` `MINUTE` ceiling (2400, MEASURED 2026-08-04 from live
// `exchangeInfo`) and the COMPILE-TIME invariant keeping `weight_soft_limit` under it now live in
// `vike_model::venue_rate_limits::ASTER_PERP`, beside every other venue's. A ceiling is a FACT ABOUT
// THE VENUE, not a tuning value, so it belongs in the per-venue capability table rather than in the
// one consumer that read it.
//
// The invariant is unchanged in strength: a soft limit at or above the hard ceiling can never fire.
// This value inherited 5000 from Binance's SPOT template and sat above Aster's real ceiling for its
// entire life — its own doc comment said "UNVERIFIED", and no test ever read the number.

/// Spot vs USDⓈ-M-futures host+path for the `/klines` endpoint, resolved from `env` via
/// [`urls::urls_for`] — the ONE place that decides fapi-vs-sapi (+ testnet-vs-mainnet) so the
/// URL-building tests below and the real fetch never drift. Unlike Binance's `&'static str` consts
/// (which bundled host+path together), this returns an owned `String`: `AsterUrls`'s fields are bare
/// hosts with no path, so the venue path segment (`/fapi/v3/klines` or `/api/v3/klines`) is appended
/// here.
fn rest_klines_base(env: Environment, perp: bool) -> String {
    let u = urls::urls_for(env);
    if perp {
        format!("{}/fapi/v3/klines", u.fapi_rest)
    } else {
        format!("{}/api/v3/klines", u.sapi_rest)
    }
}

/// The live feed's warmup seed: the newest `limit` klines (no time bound), the last of which is the
/// still-forming bar. `perp = true` routes to the USDⓈ-M futures host instead of spot; `env` picks
/// testnet vs mainnet. Delegates to [`klines::fetch_klines_latest`].
pub fn fetch_klines_latest(
    symbol: &str,
    interval: &str,
    limit: usize,
    perp: bool,
    env: Environment,
) -> Result<Vec<Bar>, String> {
    klines::fetch_klines_latest(&rest_klines_base(env, perp), symbol, interval, limit, VENUE)
}

/// The whole routing decision [`fetch_klines_range`] makes — `(base URL, wire symbol, spec)` —
/// hoisted out as a PURE fn so a test can gate it without network I/O. [`rest_klines_base`] has
/// understood `perp` since the live warmup seed was written; the bug this repairs was the paged
/// backfill never ASKING it, and a test that shapes a URL from `rest_klines_base` directly cannot
/// see that gap.
///
/// The spec rides along for the reason binance's does: the host and the budget are ONE decision,
/// not two. Here they are one decision TWICE OVER — market (sapi vs fapi) and network (testnet vs
/// mainnet) — and mainnet fapi's 2400 has nothing to do with mainnet sapi's 6000 or with testnet
/// fapi's nonsense `-2`. Resolving both out of the same `(env, perp)` pair is what makes the
/// wrong-host pairing unrepresentable here rather than merely untested.
fn range_target(symbol: &str, env: Environment) -> (String, &str, KlineSpec) {
    let (wire, perp) = vike_catalog::split_perp(symbol);
    (rest_klines_base(env, perp), wire, aster_kline_spec(env, perp))
}

/// Fetch closed-kline history for the inclusive `[start_ms, end_ms]` window, paging through Aster's
/// 1000-rows/response cap under the family rate-limit policy. A `symbol` carrying
/// [`vike_catalog::PERP_SUFFIX`] routes to the USDⓈ-M futures host and is stripped before it reaches
/// the wire; the CALLER's suffixed symbol remains the store/series key, so a perp's series can never
/// collide with its spot twin's. `env` picks testnet vs mainnet. Delegates to
/// [`klines::fetch_klines_range`] with the spec [`range_target`] built for that host.
pub fn fetch_klines_range(
    symbol: &str,
    interval: &str,
    start_ms: i64,
    end_ms: i64,
    env: Environment,
) -> Result<Vec<Bar>, String> {
    let (base, wire, spec) = range_target(symbol, env);
    klines::fetch_klines_range(&base, wire, interval, start_ms, end_ms, &spec)
}

#[cfg(test)]
mod tests {
    use super::*;
    use vike_binance::family::klines::klines_url;

    /// The ceiling is asserted at COMPILE time (now in `vike_model::venue_rate_limits`, one
    /// `const _: ()` per row), which is strictly stronger than this test — a violation fails the
    /// build, not a test run.
    ///
    /// What this gates is the WIRING: a drift in the table's aster rows changes this pager's
    /// FALLBACK pacing and fails here.
    ///
    /// ⚠ It also gates the thing that made the sapi retune worth doing: the two hosts must resolve
    /// to DIFFERENT budgets. They were one shared const built from `ASTER_PERP`, so a bare (spot)
    /// symbol braked on the fapi soft limit of 2000 — 33 % of the 6000 sapi publishes and that
    /// discovery already finds. Asserting `perp != spot` here is what stops that collapsing back.
    #[test]
    fn the_spec_paces_on_the_venue_tables_aster_row() {
        // perp — unchanged, the byte-identical half
        assert_eq!(ASTER_RATE_LIMIT_PERP.weight_soft_limit, 2000);
        assert_eq!(ASTER_RATE_LIMIT_PERP.page_delay, Duration::from_millis(150));
        assert_eq!(ASTER_PERP.history.ceiling_per_min(), 2400);
        // spot — sapi's own published budget, measured 2026-08-05
        assert_eq!(ASTER_RATE_LIMIT_SPOT.weight_soft_limit, 4800);
        assert_eq!(ASTER_RATE_LIMIT_SPOT.page_delay, Duration::from_millis(125));
        assert_eq!(ASTER_SPOT.history.ceiling_per_min(), 6000);
        // a soft limit at or above its own hard ceiling can never fire
        assert!(ASTER_RATE_LIMIT_PERP.weight_soft_limit < ASTER_PERP.history.ceiling_per_min());
        assert!(ASTER_RATE_LIMIT_SPOT.weight_soft_limit < ASTER_SPOT.history.ceiling_per_min());
        // THE property: the hosts are paced apart, not off one shared number.
        assert_ne!(ASTER_RATE_LIMIT_PERP, ASTER_RATE_LIMIT_SPOT);
        // Every spec this module can build carries its OWN host's fallback and the shared default
        // utilization; `(env, perp)` changes the discovery URL and now the budget with it.
        for env in [Environment::Live, Environment::Demo, Environment::Sim] {
            for perp in [true, false] {
                let s = aster_kline_spec(env, perp);
                let want = if perp { ASTER_RATE_LIMIT_PERP } else { ASTER_RATE_LIMIT_SPOT };
                assert_eq!(s.rate_limit, want, "{env:?}/{perp}");
                assert_eq!(s.venue, VENUE);
                assert_eq!(
                    s.utilization.to_bits(),
                    vike_model::rate_limits::DEFAULT_UTILIZATION.to_bits()
                );
            }
        }
    }

    /// **Aster discovers.** The limitation this replaces was a limitation of the spec's TYPE, never
    /// of the venue: all four hosts serve a `REQUEST_WEIGHT` row (MEASURED 2026-08-05, see
    /// [`rest_exchange_info`]). With the field a `Cow`, the URL is composed from the same
    /// `urls::urls_for(env)` table the `/klines` base is, and the rung accepts it.
    #[test]
    fn every_env_and_market_now_names_a_discovery_url() {
        for env in [Environment::Live, Environment::Demo, Environment::Sim] {
            for perp in [true, false] {
                let s = aster_kline_spec(env, perp);
                let url = s
                    .exchange_info_url
                    .as_deref()
                    .unwrap_or_else(|| panic!("{env:?}/{perp} must name a discovery URL"));
                let path = if perp {
                    crate::urls::PERP_PATH_EXCHANGE_INFO
                } else {
                    crate::urls::SPOT_PATH_EXCHANGE_INFO
                };
                assert!(url.ends_with(path), "{url} must end with {path}");
                // OWNED, which is the whole capability: there is no `&'static str` to borrow when
                // the host comes out of an `Environment`.
                assert!(matches!(s.exchange_info_url, Some(Cow::Owned(_))), "{url}");
            }
        }
    }

    /// **The per-environment pairing.** A spec must discover against the host it will ACTUALLY page,
    /// and on this venue "the host" has two independent axes: market (sapi vs fapi) and network
    /// (testnet vs mainnet). Asserted through the SHARED [`klines::discovery_url`] gate — the same
    /// one the runtime consults and binance's twin test drives — against the base
    /// [`range_target`] itself resolves, so the fetcher and its budget cannot drift apart.
    ///
    /// Not a hypothetical pairing: mainnet fapi publishes 2400, mainnet sapi 6000, and testnet fapi
    /// a nonsense `-2`. Every crossing of those axes is a different number.
    #[test]
    fn each_spec_discovers_the_budget_of_the_host_it_pages() {
        for env in [Environment::Live, Environment::Demo, Environment::Sim] {
            for symbol in ["BTCUSDT", "BTCUSDT.P"] {
                let (base, _, spec) = range_target(symbol, env);
                let info = klines::discovery_url(&spec, &base)
                    .unwrap_or_else(|| panic!("{env:?}/{symbol}: must discover against {base}"));
                assert_eq!(
                    klines::url_host(info),
                    klines::url_host(&base),
                    "{info} must be the budget of {base}'s host"
                );
            }
        }
    }

    /// The hazard the `&'static str` could not avoid, stated as a REFUSAL: a mainnet spec against a
    /// testnet base (and every other crossing of the env x market grid) discovers NOTHING, so it
    /// falls back to `page_delay` rather than pacing one network against the other's budget.
    ///
    /// The four hosts are also pairwise distinct, which is what stops this from being vacuous.
    #[test]
    fn a_spec_never_discovers_against_another_network_or_market() {
        let targets: Vec<(String, String, KlineSpec)> = [
            (Environment::Live, true),
            (Environment::Live, false),
            (Environment::Demo, true),
            (Environment::Demo, false),
        ]
        .into_iter()
        .map(|(env, perp)| {
            (
                format!("{env:?}/{}", if perp { "perp" } else { "spot" }),
                rest_klines_base(env, perp),
                aster_kline_spec(env, perp),
            )
        })
        .collect();

        // Four genuinely different hosts — otherwise the crossings below prove nothing.
        let hosts: std::collections::BTreeSet<&str> =
            targets.iter().filter_map(|(_, base, _)| klines::url_host(base)).collect();
        assert_eq!(hosts.len(), 4, "the env x market grid must be four distinct hosts: {hosts:?}");

        for (i_label, _, spec) in &targets {
            for (j_label, base, _) in &targets {
                let got = klines::discovery_url(spec, base);
                if i_label == j_label {
                    assert!(got.is_some(), "{i_label} must discover against its own host");
                } else {
                    assert_eq!(got, None, "{i_label}'s budget must not pace {j_label}");
                }
            }
        }
    }

    // Aster's OWN host-resolution proof (the per-venue delta): the shared pure `parse_klines`/URL
    // shaping is proven once at the rung (`vike_binance::family::klines`); here we prove the
    // testnet/mainnet + sapi/fapi hosts.
    #[test]
    fn klines_url_includes_optional_bounds() {
        let base = rest_klines_base(Environment::Demo, false);
        let latest = klines_url(&base, "BTCUSDT", "1m", None, None, 1000);
        assert_eq!(
            latest,
            "https://sapi.asterdex-testnet.com/api/v3/klines?symbol=BTCUSDT&interval=1m&limit=1000"
        );
        let ranged = klines_url(&base, "BTCUSDT", "1m", Some(10), Some(20), 1000);
        assert!(ranged.ends_with("&startTime=10&endTime=20"));
    }

    #[test]
    fn klines_url_perp_uses_fapi_host_spot_uses_sapi_host() {
        let perp_base = rest_klines_base(Environment::Demo, true);
        let perp = klines_url(&perp_base, "BTCUSDT", "1m", None, None, 1000);
        assert_eq!(
            perp,
            "https://fapi.asterdex-testnet.com/fapi/v3/klines?symbol=BTCUSDT&interval=1m&limit=1000"
        );
        let spot_base = rest_klines_base(Environment::Demo, false);
        let spot = klines_url(&spot_base, "BTCUSDT", "1m", None, None, 1000);
        assert_eq!(
            spot,
            "https://sapi.asterdex-testnet.com/api/v3/klines?symbol=BTCUSDT&interval=1m&limit=1000"
        );
    }

    /// The perp suffix selects the fapi host AND is stripped from the wire symbol.
    #[test]
    fn perp_suffix_selects_fapi_and_strips_the_wire_symbol() {
        let (wire, perp) = vike_catalog::split_perp("BTCUSDT.P");
        assert_eq!(wire, "BTCUSDT");
        assert!(perp);
        let base = rest_klines_base(Environment::Live, perp);
        assert!(base.ends_with("/fapi/v3/klines"), "perp must use fapi: {base}");
        let url = klines_url(&base, wire, "1m", Some(10), Some(20), 1000);
        assert!(url.contains("symbol=BTCUSDT&"), "the .P must not reach the wire: {url}");
        assert!(!url.contains(".P"), "the .P must not reach the wire: {url}");
    }

    /// A bare symbol still resolves the SPOT (sapi) host.
    #[test]
    fn a_bare_symbol_still_resolves_the_spot_host() {
        let (wire, perp) = vike_catalog::split_perp("BTCUSDT");
        assert!(!perp);
        assert_eq!(wire, "BTCUSDT");
        let base = rest_klines_base(Environment::Live, perp);
        assert!(base.ends_with("/api/v3/klines"), "spot must use sapi: {base}");
    }

    // The two tests above shape a URL out of `rest_klines_base` DIRECTLY, which has supported perp
    // since the live warmup seed was written — they would pass even with `fetch_klines_range` still
    // pinned to spot. The bug is the FETCHER not calling the helper, so gate that decision itself:
    // `range_target` is exactly what `fetch_klines_range` routes on, and it is pure (no network).
    #[test]
    fn fetch_klines_range_routes_a_perp_symbol_to_fapi_with_the_suffix_stripped() {
        let (base, wire, spec) = range_target("BTCUSDT.P", Environment::Live);
        assert_eq!(base, "https://fapi.asterdex.com/fapi/v3/klines");
        assert_eq!(wire, "BTCUSDT");
        assert_eq!(
            spec.exchange_info_url.as_deref(),
            Some("https://fapi.asterdex.com/fapi/v3/exchangeInfo"),
            "the fapi host must carry the FAPI budget"
        );
    }

    #[test]
    fn fetch_klines_range_routes_a_bare_symbol_to_sapi_unchanged() {
        let (base, wire, spec) = range_target("BTCUSDT", Environment::Live);
        assert_eq!(base, "https://sapi.asterdex.com/api/v3/klines");
        assert_eq!(wire, "BTCUSDT");
        assert_eq!(
            spec.exchange_info_url.as_deref(),
            Some("https://sapi.asterdex.com/api/v3/exchangeInfo"),
            "the sapi host must carry the SAPI budget"
        );
        // testnet is unchanged too — `env` still picks the tier, orthogonally to the perp split,
        // and it picks it for the discovery URL as well as for the pages.
        let (base, wire, spec) = range_target("BTCUSDT", Environment::Demo);
        assert_eq!(base, "https://sapi.asterdex-testnet.com/api/v3/klines");
        assert_eq!(wire, "BTCUSDT");
        assert_eq!(
            spec.exchange_info_url.as_deref(),
            Some("https://sapi.asterdex-testnet.com/api/v3/exchangeInfo")
        );
    }

    /// The discovery URL is composed from [`urls::urls_for`], never from a second copy of the host
    /// — so a host change in that ONE table moves the pages and the budget together. Asserted as a
    /// prefix relation rather than by re-typing the literals, which is what a duplicate would pass.
    #[test]
    fn the_discovery_host_comes_from_the_urls_table_not_a_second_literal() {
        for env in [Environment::Live, Environment::Demo] {
            let u = urls::urls_for(env);
            assert_eq!(
                rest_exchange_info(env, true),
                format!("{}{}", u.fapi_rest, crate::urls::PERP_PATH_EXCHANGE_INFO)
            );
            assert_eq!(
                rest_exchange_info(env, false),
                format!("{}{}", u.sapi_rest, crate::urls::SPOT_PATH_EXCHANGE_INFO)
            );
            // ...and the klines base it pairs with comes out of the same two fields.
            assert!(rest_klines_base(env, true).starts_with(u.fapi_rest));
            assert!(rest_klines_base(env, false).starts_with(u.sapi_rest));
        }
    }

    #[test]
    fn rest_klines_base_selects_fapi_for_perp_and_testnet_vs_mainnet() {
        // rest_klines_base is the pure host-selection the perp seed relies on — no network.
        assert_eq!(
            rest_klines_base(Environment::Demo, true),
            "https://fapi.asterdex-testnet.com/fapi/v3/klines"
        );
        assert_eq!(
            rest_klines_base(Environment::Demo, false),
            "https://sapi.asterdex-testnet.com/api/v3/klines"
        );
        assert_eq!(
            rest_klines_base(Environment::Live, false),
            "https://sapi.asterdex.com/api/v3/klines"
        );
        assert_eq!(
            rest_klines_base(Environment::Live, true),
            "https://fapi.asterdex.com/fapi/v3/klines"
        );
    }
}
