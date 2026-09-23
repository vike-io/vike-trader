//! LIVE Polymarket wiring for the paper maker mount — behind the `polymarket` feature (the only
//! network-touching surface). It resolves a market via the Gamma catalog and turns it into a
//! [`crate::MakerMountConfig`]; the bin wires the real [`vike_polymarket::Feeds`] into a
//! [`crate::MakerSink`]. Polymarket is US-geo-blocked, so the Gamma GET + the market WS must be
//! reached from the user's Dublin EU AWS host (`vike_polymarket::exec::agent`'s proxy handles the
//! catalog read; the WS feed inherits the same egress). None of this runs in the offline test.

use vike_model::RewardParams;
use vike_polymarket::{
    GammaClient, GammaMarket, MarketCatalog, RewardsConfig, UniversePolicy, select_universe,
};

use crate::MakerMountConfig;

/// Translate a market's parsed Polymarket liquidity-rewards config (vike-polymarket #674) into the
/// `vike-mm` maker's [`RewardParams`] (vike-mm #675), gated by the operator's opt-in `weight`.
///
/// Returns `None` — leaving the maker UNREWARDED, byte-identical to a default mount — when EITHER the
/// operator has not opted in (`weight <= 0`) OR the market runs no reward program
/// ([`RewardsConfig::earns_rewards`] is `false`). Otherwise it carries the venue's OWN reward band
/// into the params: `max_spread` (already in CENTS on the wire) → `max_spread_cents`, `min_size`
/// (shares) → `min_size`, and the compact `moas` (SECONDS) → `min_order_age_ms` (× 1000). `weight`
/// (clamped to `[0, 1]`) is the reward-vs-adverse-selection dial the maker folds with.
///
/// PURE: the opt-in `weight` is a PARAMETER (env reads stay in the bin), so this maps config→params
/// deterministically and unit-tests without touching the environment. Note a Gamma-sourced
/// [`RewardsConfig`] carries `min_order_age_secs == 0` (Gamma has no `moas`), so the moas gate is
/// inert on the `--query`/`--auto` path — an owed follow-up is to enrich it from the CLOB compact
/// `r` shape ([`RewardsConfig::from_compact`]) so a rewards maker honours the 30 s min-order-age.
pub fn reward_params_from(rewards: &RewardsConfig, weight: f64) -> Option<RewardParams> {
    // `weight <= 0.0` (not `!(.. > 0.0)`) is the crate's positive-or-off f64 guard — it also keeps
    // clippy's `neg_cmp_op_on_partial_ord` happy. Not opted in, or no reward program ⇒ leave OFF.
    if weight <= 0.0 || !rewards.earns_rewards() {
        return None;
    }
    Some(RewardParams {
        weight: weight.clamp(0.0, 1.0),
        max_spread_cents: rewards.max_spread,
        min_size: rewards.min_size,
        min_order_age_ms: (rewards.min_order_age_secs as i64).saturating_mul(1_000),
    })
}

/// Resolve one live market by a case-insensitive name/slug `query` over the top-`page_limit`
/// active markets by volume (Gamma `active=true&closed=false&order=volumeNum`). Returns the
/// highest-volume match that carries at least one outcome token. Geo-blocked → routes through the
/// Polymarket proxy like every other read in this crate.
pub fn select_market(query: &str, page_limit: usize) -> Result<GammaMarket, String> {
    let markets = GammaClient::list(true, page_limit, 0)?;
    let catalog = MarketCatalog::from_markets(markets);
    catalog
        .search(query)
        .into_iter()
        .find(|m| !m.token_ids.is_empty())
        .cloned()
        .ok_or_else(|| format!("no active market with a token matched query {query:?}"))
}

/// Build a paper-maker config for `market`'s FIRST outcome token (conventionally "Yes"), with the
/// A-S `resolution_ts` wired from the market's `end_date` and the tick grid from its
/// `orderPriceMinTickSize` (falling back to `0.01`). `Err` if the market carries no token.
///
/// `reward_weight` is the operator's liquidity-rewards opt-in (the bin reads it from
/// `--reward-weight` / `POLY_REWARD_WEIGHT`): `<= 0` leaves `cfg.reward` at `None` (rewards OFF,
/// byte-identical to a plain mount), and a positive value folds the market's parsed
/// [`RewardsConfig`] into the maker's reward params via [`reward_params_from`] (still `None` for a
/// non-rewarded market). The `--token-id` path ([`config_for_token`]) has no market metadata, so it
/// stays unrewarded regardless.
pub fn config_for_market(
    market: &GammaMarket,
    reward_weight: f64,
) -> Result<MakerMountConfig, String> {
    let token = market
        .token_ids
        .first()
        .cloned()
        .ok_or_else(|| format!("market {:?} has no outcome token", market.slug))?;
    let mut cfg = MakerMountConfig::polymarket(token, market.resolution_ts_ms());
    if market.tick_size > 0.0 {
        cfg.tick_size = market.tick_size;
    }
    cfg.reward = reward_params_from(&market.rewards, reward_weight);
    Ok(cfg)
}

/// Build a paper-maker config for a known `token_id` WITHOUT catalog metadata — the no-catalog
/// FALLBACK for the `--token-id` path (used only when [`market_for_token`] finds nothing).
/// `resolution_ts_ms` is the market's `end_date` in epoch-ms (or `None`). Takes the
/// `MakerMountConfig::polymarket` default tick (`0.01`) and no rewards; the live maker still
/// self-corrects its tick from the first L2 book grid. The catalog-found path reuses
/// [`config_for_market`] instead — the SINGLE real builder (real tick + rewards + resolution).
pub fn config_for_token(token_id: &str, resolution_ts_ms: Option<i64>) -> MakerMountConfig {
    MakerMountConfig::polymarket(token_id, resolution_ts_ms)
}

/// Look up the full [`GammaMarket`] that lists a raw `token_id` via the proxy-routed Gamma catalog,
/// so the `--token-id` path can reuse [`config_for_market`] — the SAME single builder the
/// `--query`/`--auto` paths use (real tick + rewards + resolution) — instead of the bare
/// [`config_for_token`] fallback. A ONE-TIME mount lookup, never per-order (the tick is a static
/// market property, cached in the config). `None` if the fetch fails or no active market lists the
/// token (the caller falls back to `config_for_token`; the maker still self-corrects its tick from
/// the first L2 book grid).
pub fn market_for_token(token_id: &str, page_limit: usize) -> Option<GammaMarket> {
    let markets = GammaClient::list(true, page_limit, 0).ok()?;
    markets.into_iter().find(|m| m.token_ids.iter().any(|t| t == token_id))
}

/// Pure: pick the single most-liquid tradeable market from an already-fetched list under `policy`
/// — universe selection ([`vike_polymarket::select_universe`]) driving the mount, no name query.
/// Returns the top-ranked market mapped back to its [`GammaMarket`] (so the caller reuses
/// [`config_for_market`]); `None` when nothing clears the policy's floors. Factored out of
/// [`select_top_market`] so the ranking→mapping is unit-tested without a network fetch.
pub fn top_market_from<'a>(
    markets: &'a [GammaMarket],
    policy: &UniversePolicy,
) -> Option<&'a GammaMarket> {
    let selected = select_universe(markets, policy);
    let top = selected.first()?;
    markets.iter().find(|m| m.condition_id == top.condition_id)
}

/// Auto-select the most-liquid tradeable market under `policy` — the `--auto` mount path: universe
/// selection instead of a hand-named `--query`/`--token-id`. Fetches the top-`page_limit` active
/// markets by volume and returns the highest-ranked one. Geo-blocked → routes through the Polymarket
/// proxy like every other read here. `Err` if nothing clears the policy.
pub fn select_top_market(
    policy: &UniversePolicy,
    page_limit: usize,
) -> Result<GammaMarket, String> {
    let markets = GammaClient::list(true, page_limit, 0)?;
    top_market_from(&markets, policy)
        .cloned()
        .ok_or_else(|| "no active market cleared the universe policy".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mk(cid: &str, liq: f64, toks: &[&str]) -> GammaMarket {
        GammaMarket {
            id: cid.into(),
            question: format!("q-{cid}"),
            condition_id: cid.into(),
            slug: cid.into(),
            end_date: "".into(),
            volume: 0.0,
            liquidity: liq,
            active: true,
            closed: false,
            neg_risk: false,
            tick_size: 0.01,
            outcomes: vec![],
            token_ids: toks.iter().map(|s| s.to_string()).collect(),
            ..Default::default()
        }
    }

    #[test]
    fn top_market_picks_the_most_liquid_and_maps_back_to_gamma() {
        let markets =
            vec![mk("a", 100.0, &["a1"]), mk("b", 900.0, &["b1"]), mk("c", 500.0, &["c1"])];
        let policy = UniversePolicy { min_liquidity: 0.0, ..Default::default() };
        let top = top_market_from(&markets, &policy).expect("a top market");
        assert_eq!(top.condition_id, "b"); // 900 is most-liquid
        assert_eq!(top.token_ids, vec!["b1".to_string()]);
    }

    #[test]
    fn top_market_respects_the_liquidity_floor() {
        // everything below the floor ⇒ nothing selected.
        let markets = vec![mk("a", 10.0, &["a1"]), mk("b", 50.0, &["b1"])];
        let policy = UniversePolicy { min_liquidity: 1_000.0, ..Default::default() };
        assert!(top_market_from(&markets, &policy).is_none());
    }

    /// A rewarded market's parsed config (mirrors the CLOB compact shape that carries `moas`).
    fn rewarded() -> RewardsConfig {
        RewardsConfig {
            min_size: 100.0,
            max_spread: 3.0, // cents, verbatim
            min_order_age_secs: 30,
            enabled: true,
            ..Default::default()
        }
    }

    // Opted in on a rewarded market: the venue's own band flows into the maker's reward params, with
    // the compact `moas` seconds converted to ms and the opt-in `weight` carried (and clamped).
    #[test]
    fn reward_params_from_opt_in_carries_the_venue_band() {
        let p = reward_params_from(&rewarded(), 0.5).expect("opted-in rewarded market ⇒ params");
        assert_eq!(p.weight, 0.5);
        assert_eq!(p.max_spread_cents, 3.0, "max_spread (cents) ← venue max_spread");
        assert_eq!(p.min_size, 100.0, "min_size ← venue min_size");
        assert_eq!(p.min_order_age_ms, 30_000, "moas seconds → ms");
        // weight is clamped into [0, 1].
        let clamped = reward_params_from(&rewarded(), 2.0).expect("still rewarded");
        assert_eq!(clamped.weight, 1.0, "weight clamps to 1");
    }

    // Opt-in UNSET (weight 0) on a rewarded market ⇒ no reward params ⇒ rewards OFF, byte-identical.
    #[test]
    fn reward_params_from_off_when_not_opted_in() {
        assert!(
            reward_params_from(&rewarded(), 0.0).is_none(),
            "unopted ⇒ None ⇒ reward weight stays 0 (rewards OFF)"
        );
        assert!(reward_params_from(&rewarded(), -1.0).is_none(), "negative weight ⇒ OFF");
    }

    // A NON-rewarded market (default all-zero config) leaves the maker unrewarded even when opted in.
    #[test]
    fn reward_params_from_none_for_non_rewarded_market() {
        assert!(!RewardsConfig::default().earns_rewards(), "precondition: default runs no program");
        assert!(
            reward_params_from(&RewardsConfig::default(), 0.5).is_none(),
            "non-rewarded market ⇒ unrewarded maker"
        );
    }

    // End-to-end config wiring: `config_for_market` folds the market's parsed rewards into the mount
    // config under the opt-in — Some(matching) when opted-in-and-rewarded, None otherwise.
    #[test]
    fn config_for_market_wires_rewards_under_opt_in() {
        let mut rewarded_market = mk("r", 100.0, &["t1"]);
        rewarded_market.rewards = rewarded();

        // opted in + rewarded ⇒ the mount config carries the matching reward params.
        let cfg = config_for_market(&rewarded_market, 0.4).expect("token present");
        assert_eq!(
            cfg.reward,
            Some(RewardParams {
                weight: 0.4,
                max_spread_cents: 3.0,
                min_size: 100.0,
                min_order_age_ms: 30_000,
            })
        );

        // same market, opt-in UNSET ⇒ rewards OFF (byte-identical to before this wiring).
        let off = config_for_market(&rewarded_market, 0.0).expect("token present");
        assert!(off.reward.is_none(), "opt-in unset ⇒ no reward params on the mount config");

        // a non-rewarded market (default rewards via `mk`'s `..Default::default()`) ⇒ unrewarded even
        // when opted in.
        let plain = mk("p", 100.0, &["t1"]);
        assert!(!plain.rewards.earns_rewards(), "precondition: mk builds a non-rewarded market");
        let plain_cfg = config_for_market(&plain, 0.4).expect("token present");
        assert!(plain_cfg.reward.is_none(), "non-rewarded market ⇒ unrewarded mount");
    }
}
