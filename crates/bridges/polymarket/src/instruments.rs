//! Polymarket market catalog — a CLOB `/markets` cursor-walk resolving each market's outcome
//! `token_id`s, tick size, and the **NegRisk** flag (which selects the order-signing domain). Reads
//! are unauthenticated (the public [`RestTransport`] seam).
//!
//! **Direct point lookups (CLOB hygiene).** The official clients expose two per-token
//! endpoints the catalog walk does not need to substitute for: `GET /tick-size?token_id=…`
//! ([`fetch_tick_size_direct`]) and `GET /neg-risk?token_id=…` ([`fetch_token_neg_risk`]). Those are
//! ONE request instead of up to `TICK_SIZE_LOOKUP_MAX_PAGES` paged `/markets` requests, so
//! [`fetch_token_tick_size`] now asks the direct endpoint FIRST and keeps the page walk
//! ([`fetch_token_tick_size_paged`]) as its documented fallback on ANY failure — a REST error, an
//! unparseable body, or a deployment that does not serve the point endpoint all fall through to
//! the pre-existing walk, so the resolved VALUE can never regress for a token the walk could
//! already resolve (and a genuinely not-found token still lands on the same `DEFAULT_TICK_SIZE`);
//! only the request shape shortens.
//!
//! OWED before this rides on the LIVE market-feed subscribe path: a keyless verification of
//! `/tick-size` + `/neg-risk` through the arbdub (Dublin) host, since Polymarket is US-geo-blocked
//! from the dev box and the point endpoints have not been probed from a permitted region.
//!
//! **Wire-format oracle.** `py-clob-client` was ARCHIVED by Polymarket on 2026-05-25 and is
//! declared NON-FUNCTIONAL — the current official clients are `rs-clob-client-v2` (Rust) and
//! `clob-client-v2` (TypeScript). They are read as an ORACLE only: `rs-clob-client-v2` is
//! reqwest/tokio-based and adding it as a dependency would trip `deny.toml`'s HTTP-stack bans, so
//! it is never a dep of this crate.

use vike_bridge_core::transport::{RestTransport, VenueApiError};

use super::config::CLOB_BASE;
use super::rewards::RewardsConfig;

/// One outcome token of a market.
#[derive(Debug, Clone, PartialEq)]
pub struct PolyToken {
    pub token_id: String,
    pub outcome: String,
}

/// A CLOB market (binary or multi-outcome).
#[derive(Debug, Clone, PartialEq)]
pub struct PolyMarket {
    pub condition_id: String,
    pub tokens: Vec<PolyToken>,
    /// multi-outcome market → orders sign against the NegRisk exchange domain/contract.
    pub neg_risk: bool,
    pub tick_size: f64,
    /// The market's liquidity-rewards config, parsed from the CLOB `rewards` object
    /// (`rates`/`min_size`/`max_spread`; see [`crate::rewards::RewardsConfig`]). ADDITIVE:
    /// [`RewardsConfig::default`] (all-zero, inert) on a non-rewarded market or a `/markets`
    /// response with no `rewards` key. This shape carries the daily rate; the compact `moas`
    /// (min order age) comes only from `/clob-markets/{condition_id}` — see [`crate::rewards`].
    pub rewards: RewardsConfig,
}

/// Parse a `/markets` response `data` array into markets.
pub fn parse_markets(v: &serde_json::Value) -> Vec<PolyMarket> {
    let Some(arr) = v.get("data").and_then(|d| d.as_array()) else {
        return Vec::new();
    };
    arr.iter()
        .filter_map(|m| {
            let condition_id = m.get("condition_id")?.as_str()?.to_string();
            let tokens = m
                .get("tokens")
                .and_then(|t| t.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|tk| {
                            Some(PolyToken {
                                token_id: tk.get("token_id")?.as_str()?.to_string(),
                                outcome: tk
                                    .get("outcome")
                                    .and_then(|o| o.as_str())
                                    .unwrap_or_default()
                                    .to_string(),
                            })
                        })
                        .collect()
                })
                .unwrap_or_default();
            let neg_risk = m.get("neg_risk").and_then(serde_json::Value::as_bool).unwrap_or(false);
            let tick_size = m
                .get("minimum_tick_size")
                .or_else(|| m.get("tick_size"))
                .and_then(vike_bridge_core::json::json_num)
                .unwrap_or(0.01);
            let rewards =
                m.get("rewards").map(RewardsConfig::from_clob_rewards).unwrap_or_default();
            Some(PolyMarket { condition_id, tokens, neg_risk, tick_size, rewards })
        })
        .collect()
}

/// The pagination cursor to fetch the next page, or `None` at the end (`""`/`"LTE="`).
pub fn next_cursor(v: &serde_json::Value) -> Option<String> {
    v.get("next_cursor")
        .and_then(|c| c.as_str())
        .filter(|c| !c.is_empty() && *c != "LTE=")
        .map(str::to_string)
}

/// Fetch one `/markets` page (empty `cursor` = first page). Returns the raw response so the caller
/// can both [`parse_markets`] and read [`next_cursor`].
pub fn fetch_markets_page(
    t: &dyn RestTransport,
    cursor: &str,
) -> Result<serde_json::Value, VenueApiError> {
    let params =
        if cursor.is_empty() { Vec::new() } else { vec![("next_cursor", cursor.to_string())] };
    t.public(CLOB_BASE, "/markets", &params)
}

/// Walk every `/markets` page into one catalog (bounded by `max_pages`).
pub fn fetch_all_markets(
    t: &dyn RestTransport,
    max_pages: usize,
) -> Result<Vec<PolyMarket>, VenueApiError> {
    let mut out = Vec::new();
    let mut cursor = String::new();
    for _ in 0..max_pages {
        let page = fetch_markets_page(t, &cursor)?;
        out.extend(parse_markets(&page));
        match next_cursor(&page) {
            Some(c) => cursor = c,
            None => break,
        }
    }
    Ok(out)
}

/// Does `token_id` belong to a NegRisk market? (selects the order-signing domain).
pub fn is_neg_risk(token_id: &str, markets: &[PolyMarket]) -> bool {
    markets.iter().any(|m| m.neg_risk && m.tokens.iter().any(|t| t.token_id == token_id))
}

/// Bounded `/markets` cursor-walk pages a single `fetch_token_tick_size` lookup will scan before
/// giving up and defaulting — the CLOB API has no per-token market-by-token endpoint (only
/// `/book?token_id=`/`/midpoint?token_id=`, neither of which carries `minimum_tick_size`), so this
/// mirrors [`fetch_all_markets`]'s walk with an early exit on the first page containing the token.
const TICK_SIZE_LOOKUP_MAX_PAGES: usize = 50;
/// Default tick size on a not-found token or a REST/parse failure (Polymarket's most common
/// value — the same default [`parse_markets`] falls back to per-market).
const DEFAULT_TICK_SIZE: f64 = 0.01;

/// The official clients' per-token tick-size endpoint (`GET /tick-size?token_id=…`),
/// answering `{"minimum_tick_size": 0.01}`.
const TICK_SIZE_PATH: &str = "/tick-size";
/// The official clients' per-token neg-risk endpoint (`GET /neg-risk?token_id=…`),
/// answering `{"neg_risk": true}`.
const NEG_RISK_PATH: &str = "/neg-risk";

/// Read a tick size out of a `/tick-size` (or any tick-carrying) response body: `minimum_tick_size`
/// first, then the plain `tick_size` alias — string OR number wire form (`json_num`), the same
/// tolerance [`parse_markets`] applies. `None` for an absent/unparseable field or a non-finite /
/// non-positive value (a zero tick would silently disable rounding downstream).
pub fn parse_tick_size(v: &serde_json::Value) -> Option<f64> {
    v.get("minimum_tick_size")
        .or_else(|| v.get("tick_size"))
        .and_then(vike_bridge_core::json::json_num)
        .filter(|t| t.is_finite() && *t > 0.0)
}

/// Read the NegRisk flag out of a `/neg-risk` response body. Accepts the JSON `true`/`false` form
/// and (defensively) the stringified `"true"`/`"false"` one; `None` when the field is absent or
/// carries anything else — a caller must NOT read that as `false` (the signing domain would be
/// wrong), which is why this is `Option<bool>` rather than a defaulting read.
pub fn parse_neg_risk(v: &serde_json::Value) -> Option<bool> {
    match v.get("neg_risk")? {
        serde_json::Value::Bool(b) => Some(*b),
        serde_json::Value::String(s) => match s.trim().to_ascii_lowercase().as_str() {
            "true" => Some(true),
            "false" => Some(false),
            _ => None,
        },
        _ => None,
    }
}

/// ONE direct `GET /tick-size?token_id=…` (the official clients' point lookup). `None` on a REST
/// failure, an unparseable body, or a deployment that does not serve the endpoint — the caller then
/// falls back to the `/markets` page walk ([`fetch_token_tick_size_paged`]).
pub fn fetch_tick_size_direct<T: RestTransport>(t: &T, token_id: &str) -> Option<f64> {
    match t.public(CLOB_BASE, TICK_SIZE_PATH, &[("token_id", token_id.to_string())]) {
        Ok(v) => parse_tick_size(&v),
        Err(e) => {
            tracing::debug!(
                token_id,
                error = %e,
                "fetch_tick_size_direct: /tick-size unavailable, falling back to the /markets walk"
            );
            None
        }
    }
}

/// ONE direct `GET /neg-risk?token_id=…` (the official clients' point lookup) — the per-token twin of
/// [`is_neg_risk`], which needs a whole fetched catalog. `None` on a REST failure or an
/// unparseable/absent flag; a caller that cannot resolve it must NOT assume `false` (that picks the
/// wrong EIP-712 signing domain — see [`PolyMarket::neg_risk`]).
pub fn fetch_token_neg_risk<T: RestTransport>(t: &T, token_id: &str) -> Option<bool> {
    match t.public(CLOB_BASE, NEG_RISK_PATH, &[("token_id", token_id.to_string())]) {
        Ok(v) => parse_neg_risk(&v),
        Err(e) => {
            tracing::debug!(token_id, error = %e, "fetch_token_neg_risk: /neg-risk request failed");
            None
        }
    }
}

/// The FALLBACK tick-size resolution (unchanged behavior, extracted): walk `/markets` pages until a
/// market carrying `token_id` is found (early exit — does NOT walk the whole catalog once matched).
/// `None` on a REST failure, a page-budget exhaustion, or the token simply not existing in the
/// catalog; [`fetch_token_tick_size`] turns that into the default.
pub fn fetch_token_tick_size_paged<T: RestTransport>(t: &T, token_id: &str) -> Option<f64> {
    fetch_token_tick_size_paged_while(t, token_id, &|| true)
}

/// [`fetch_token_tick_size_paged`] with a CONTINUE PREDICATE consulted before every page request.
///
/// **Why a bound on the agent is not enough.** The walk is up to [`TICK_SIZE_LOOKUP_MAX_PAGES`]
/// sequential requests, so a per-request ceiling multiplies by fifty; a caller on a thread with a
/// stop budget needs the LOOP to end, not just each request. `keep_going` is that caller's stop
/// flag, checked in front of each request, so the longest a raised flag can be ignored here is the
/// ONE request already in flight — the same "one blocking window, then a stop check" discipline
/// `crates/vike-recorder/src/recorder_cli.rs`'s `FEED_STOP_BUDGET_SECS` is derived from.
///
/// A `false` predicate answers `None`, indistinguishable from "not found" on purpose: the caller
/// that raised the flag is tearing down, and inventing a tick size would be worse than the default.
pub fn fetch_token_tick_size_paged_while<T: RestTransport>(
    t: &T,
    token_id: &str,
    keep_going: &dyn Fn() -> bool,
) -> Option<f64> {
    let mut cursor = String::new();
    for _ in 0..TICK_SIZE_LOOKUP_MAX_PAGES {
        if !keep_going() {
            tracing::debug!(
                token_id,
                "fetch_token_tick_size: stop raised — abandoning the /markets walk"
            );
            return None;
        }
        let page = match fetch_markets_page(t, &cursor) {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!(
                    token_id,
                    error = %e,
                    "fetch_token_tick_size: /markets request failed"
                );
                return None;
            }
        };
        if let Some(m) = parse_markets(&page)
            .into_iter()
            .find(|m| m.tokens.iter().any(|tk| tk.token_id == token_id))
        {
            return Some(m.tick_size);
        }
        match next_cursor(&page) {
            Some(c) => cursor = c,
            None => break,
        }
    }
    None
}

/// Resolve one outcome `token_id`'s tick size: the DIRECT `/tick-size` point lookup first, the
/// `/markets` page walk as the documented fallback, and [`DEFAULT_TICK_SIZE`] with a
/// `tracing::warn!` when neither resolves it. Callers that resolve many tokens should cache the
/// result themselves — this is a plain lookup, not a cache (the caching, re-fetch-on-reject shape
/// is [`crate::tick_regime::TickRegime`]).
pub fn fetch_token_tick_size<T: RestTransport>(t: &T, token_id: &str) -> f64 {
    fetch_token_tick_size_while(t, token_id, &|| true)
}

/// [`fetch_token_tick_size`] with a CONTINUE PREDICATE consulted before EVERY request it makes —
/// the point lookup and each page of the fallback walk.
///
/// This is what a LIVE FEED THREAD calls. `crates/bridges/polymarket/src/market_feed.rs`'s
/// `reconcile_slots` resolves a newly-seated token here, on the feed thread, inside the driver's
/// connect closure — so the requests are spent from the recorder's feed-stop budget, and a stop
/// raised mid-warmup must end the resolution rather than pay for up to
/// `1 + TICK_SIZE_LOOKUP_MAX_PAGES` more round trips.
///
/// An interrupted resolution answers [`DEFAULT_TICK_SIZE`] — the same answer an unresolvable token
/// gets, and a `debug!` rather than the `warn!` below, because "we stopped asking" is not the same
/// finding as "the venue does not know this token".
pub fn fetch_token_tick_size_while<T: RestTransport>(
    t: &T,
    token_id: &str,
    keep_going: &dyn Fn() -> bool,
) -> f64 {
    if !keep_going() {
        tracing::debug!(
            token_id,
            "fetch_token_tick_size: stop raised before the lookup — seating at {DEFAULT_TICK_SIZE}"
        );
        return DEFAULT_TICK_SIZE;
    }
    if let Some(tick) = fetch_tick_size_direct(t, token_id) {
        return tick;
    }
    if let Some(tick) = fetch_token_tick_size_paged_while(t, token_id, keep_going) {
        return tick;
    }
    if !keep_going() {
        return DEFAULT_TICK_SIZE; // already disclosed at `debug!` by the walk
    }
    tracing::warn!(
        token_id,
        "fetch_token_tick_size: neither /tick-size nor the /markets catalog walk resolved this token, defaulting to {DEFAULT_TICK_SIZE}"
    );
    DEFAULT_TICK_SIZE
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> serde_json::Value {
        serde_json::json!({
            "data": [{
                "condition_id": "0xcond1",
                "neg_risk": true,
                "minimum_tick_size": "0.001",
                "tokens": [
                    {"token_id": "111", "outcome": "Yes"},
                    {"token_id": "222", "outcome": "No"}
                ]
            }],
            "next_cursor": "LTE="
        })
    }

    #[test]
    fn parse_and_neg_risk() {
        let markets = parse_markets(&sample());
        assert_eq!(markets.len(), 1);
        let m = &markets[0];
        assert_eq!(m.condition_id, "0xcond1");
        assert_eq!(m.tokens.len(), 2);
        assert_eq!(m.tokens[0].outcome, "Yes");
        assert!(m.neg_risk);
        assert_eq!(m.tick_size, 0.001);
        assert!(is_neg_risk("111", &markets));
        assert!(!is_neg_risk("999", &markets));
    }

    #[test]
    fn clob_market_rewards_parsed_from_the_rewards_object() {
        let v = serde_json::json!({
            "data": [{
                "condition_id": "0xc",
                "neg_risk": false,
                "minimum_tick_size": "0.01",
                "tokens": [{"token_id": "1", "outcome": "Yes"}],
                "rewards": {
                    "rates": [{ "asset_address": "0xUSDC", "rewards_daily_rate": 5.0 }],
                    "min_size": 100.0,
                    "max_spread": 3.0
                }
            }],
            "next_cursor": "LTE="
        });
        let m = &parse_markets(&v)[0];
        assert_eq!(m.rewards.min_size, 100.0);
        assert_eq!(m.rewards.max_spread, 3.0); // cents, verbatim
        assert_eq!(m.rewards.daily_rate, 5.0);
        assert!(m.rewards.enabled);
        assert!(m.rewards.earns_rewards());
    }

    #[test]
    fn clob_market_without_a_rewards_object_is_default() {
        // the pre-existing sample() fixture has no `rewards` key → the additive field is inert,
        // so a market that parsed before this field existed is byte-identical.
        let m = &parse_markets(&sample())[0];
        assert_eq!(m.rewards, crate::rewards::RewardsConfig::default());
        assert!(!m.rewards.earns_rewards());
    }

    #[test]
    fn cursor_end_detection() {
        assert_eq!(next_cursor(&sample()), None); // "LTE=" == end
        assert_eq!(
            next_cursor(&serde_json::json!({"next_cursor": "abc"})),
            Some("abc".to_string())
        );
    }

    /// Canned-page transport for [`fetch_token_tick_size`] tests: replays `pages` in order by
    /// cursor value (empty cursor = page 0), or errors if asked past the end.
    struct PageStub {
        pages: Vec<serde_json::Value>,
    }

    impl RestTransport for PageStub {
        fn signed(
            &self,
            _base: &str,
            _path: &str,
            _method: &str,
            _params: &[(&str, String)],
            _signer: &dyn vike_bridge_core::signer::Signer,
        ) -> Result<serde_json::Value, VenueApiError> {
            panic!("fetch_token_tick_size never signs a request")
        }
        fn public(
            &self,
            _base: &str,
            _path: &str,
            params: &[(&str, String)],
        ) -> Result<serde_json::Value, VenueApiError> {
            let idx = match params.iter().find(|(k, _)| *k == "next_cursor") {
                None => 0,
                Some((_, c)) => self
                    .pages
                    .iter()
                    .position(|p| next_cursor(p).as_deref() == Some(c.as_str()))
                    .map(|i| i + 1)
                    .unwrap_or(usize::MAX),
            };
            self.pages
                .get(idx)
                .cloned()
                .ok_or(VenueApiError { code: 0, msg: "past last page".into() })
        }
    }

    /// A failing transport — every `/markets` page request errors.
    struct FailingStub;
    impl RestTransport for FailingStub {
        fn signed(
            &self,
            _base: &str,
            _path: &str,
            _method: &str,
            _params: &[(&str, String)],
            _signer: &dyn vike_bridge_core::signer::Signer,
        ) -> Result<serde_json::Value, VenueApiError> {
            panic!("fetch_token_tick_size never signs a request")
        }
        fn public(
            &self,
            _base: &str,
            _path: &str,
            _params: &[(&str, String)],
        ) -> Result<serde_json::Value, VenueApiError> {
            Err(VenueApiError { code: 0, msg: "network down".into() })
        }
    }

    fn page(condition_id: &str, token_id: &str, tick_size: &str, next: &str) -> serde_json::Value {
        serde_json::json!({
            "data": [{
                "condition_id": condition_id,
                "neg_risk": false,
                "minimum_tick_size": tick_size,
                "tokens": [{"token_id": token_id, "outcome": "Yes"}]
            }],
            "next_cursor": next
        })
    }

    #[test]
    fn tick_size_found_on_first_page() {
        let t = PageStub { pages: vec![page("0xcond1", "111", "0.001", "LTE=")] };
        assert_eq!(fetch_token_tick_size(&t, "111"), 0.001);
    }

    #[test]
    fn tick_size_found_after_walking_a_later_page() {
        let t = PageStub {
            pages: vec![
                page("0xcond1", "111", "0.01", "cursor2"),
                page("0xcond2", "222", "0.005", "LTE="),
            ],
        };
        assert_eq!(fetch_token_tick_size(&t, "222"), 0.005);
    }

    #[test]
    fn tick_size_defaults_when_token_not_in_the_catalog() {
        let t = PageStub { pages: vec![page("0xcond1", "111", "0.01", "LTE=")] };
        assert_eq!(fetch_token_tick_size(&t, "does-not-exist"), DEFAULT_TICK_SIZE);
    }

    #[test]
    fn tick_size_defaults_on_a_rest_failure() {
        assert_eq!(fetch_token_tick_size(&FailingStub, "111"), DEFAULT_TICK_SIZE);
    }

    // ---- direct point lookups (CLOB hygiene) ----------------------------------------------------

    /// A path-routing transport: answers `/tick-size` and `/neg-risk` from canned bodies (`None` =
    /// that endpoint errors, the "deployment does not serve it" shape) and `/markets` from `pages`
    /// exactly as [`PageStub`] does. Every requested `(path, token_id)` is recorded so a test can
    /// prove the direct endpoint was tried FIRST and the walk was (or was not) reached.
    struct PathStub {
        tick: Option<serde_json::Value>,
        neg_risk: Option<serde_json::Value>,
        pages: Vec<serde_json::Value>,
        seen: std::cell::RefCell<Vec<String>>,
    }

    impl PathStub {
        fn new() -> Self {
            PathStub {
                tick: None,
                neg_risk: None,
                pages: Vec::new(),
                seen: std::cell::RefCell::new(Vec::new()),
            }
        }
        fn with_tick(mut self, v: serde_json::Value) -> Self {
            self.tick = Some(v);
            self
        }
        fn with_neg_risk(mut self, v: serde_json::Value) -> Self {
            self.neg_risk = Some(v);
            self
        }
        fn with_pages(mut self, pages: Vec<serde_json::Value>) -> Self {
            self.pages = pages;
            self
        }
        fn paths(&self) -> Vec<String> {
            self.seen.borrow().clone()
        }
    }

    impl RestTransport for PathStub {
        fn signed(
            &self,
            _base: &str,
            _path: &str,
            _method: &str,
            _params: &[(&str, String)],
            _signer: &dyn vike_bridge_core::signer::Signer,
        ) -> Result<serde_json::Value, VenueApiError> {
            panic!("the tick-size / neg-risk lookups never sign a request")
        }
        fn public(
            &self,
            _base: &str,
            path: &str,
            params: &[(&str, String)],
        ) -> Result<serde_json::Value, VenueApiError> {
            self.seen.borrow_mut().push(path.to_string());
            let canned = |v: &Option<serde_json::Value>| {
                v.clone().ok_or(VenueApiError { code: 404, msg: "not served".into() })
            };
            match path {
                "/tick-size" => canned(&self.tick),
                "/neg-risk" => canned(&self.neg_risk),
                _ => {
                    let idx = match params.iter().find(|(k, _)| *k == "next_cursor") {
                        None => 0,
                        Some((_, c)) => self
                            .pages
                            .iter()
                            .position(|p| next_cursor(p).as_deref() == Some(c.as_str()))
                            .map(|i| i + 1)
                            .unwrap_or(usize::MAX),
                    };
                    self.pages
                        .get(idx)
                        .cloned()
                        .ok_or(VenueApiError { code: 0, msg: "past last page".into() })
                }
            }
        }
    }

    #[test]
    fn tick_size_body_parse_accepts_string_and_number_and_rejects_garbage() {
        assert_eq!(
            parse_tick_size(&serde_json::json!({"minimum_tick_size": "0.001"})),
            Some(0.001)
        );
        assert_eq!(parse_tick_size(&serde_json::json!({"minimum_tick_size": 0.01})), Some(0.01));
        // the plain alias is accepted too (the `book` WS frame's spelling)
        assert_eq!(parse_tick_size(&serde_json::json!({"tick_size": "0.0001"})), Some(0.0001));
        // absent / unparseable / degenerate → None, never a silent 0.0 tick
        assert_eq!(parse_tick_size(&serde_json::json!({})), None);
        assert_eq!(parse_tick_size(&serde_json::json!({"minimum_tick_size": "abc"})), None);
        assert_eq!(parse_tick_size(&serde_json::json!({"minimum_tick_size": 0})), None);
        assert_eq!(parse_tick_size(&serde_json::json!({"minimum_tick_size": -0.01})), None);
    }

    #[test]
    fn neg_risk_body_parse_is_option_not_defaulting() {
        assert_eq!(parse_neg_risk(&serde_json::json!({"neg_risk": true})), Some(true));
        assert_eq!(parse_neg_risk(&serde_json::json!({"neg_risk": false})), Some(false));
        assert_eq!(parse_neg_risk(&serde_json::json!({"neg_risk": "TRUE"})), Some(true));
        assert_eq!(parse_neg_risk(&serde_json::json!({"neg_risk": "false"})), Some(false));
        // an absent or nonsense flag must NOT read as `false` (that picks the wrong signing domain)
        assert_eq!(parse_neg_risk(&serde_json::json!({})), None);
        assert_eq!(parse_neg_risk(&serde_json::json!({"neg_risk": 1})), None);
    }

    #[test]
    fn the_direct_tick_size_endpoint_is_used_and_the_walk_is_never_reached() {
        let t = PathStub::new()
            .with_tick(serde_json::json!({"minimum_tick_size": "0.001"}))
            .with_pages(vec![page("0xcond1", "111", "0.01", "LTE=")]);
        assert_eq!(fetch_token_tick_size(&t, "111"), 0.001, "the direct lookup wins");
        assert_eq!(t.paths(), vec!["/tick-size".to_string()], "no /markets page was walked");
    }

    #[test]
    fn a_deployment_without_the_direct_endpoint_falls_back_to_the_page_walk() {
        // `/tick-size` errors (no canned body) → the walk resolves the SAME value it always did.
        let t = PathStub::new().with_pages(vec![
            page("0xcond1", "111", "0.01", "cursor2"),
            page("0xcond2", "222", "0.005", "LTE="),
        ]);
        assert_eq!(fetch_token_tick_size(&t, "222"), 0.005);
        assert_eq!(
            t.paths(),
            vec!["/tick-size".to_string(), "/markets".to_string(), "/markets".to_string()],
            "the direct lookup is tried first, then the walk"
        );
    }

    #[test]
    fn neither_lookup_resolving_still_defaults() {
        let t = PathStub::new().with_pages(vec![page("0xcond1", "111", "0.01", "LTE=")]);
        assert_eq!(fetch_token_tick_size(&t, "does-not-exist"), DEFAULT_TICK_SIZE);
    }

    #[test]
    fn direct_neg_risk_lookup_round_trip() {
        let t = PathStub::new().with_neg_risk(serde_json::json!({"neg_risk": true}));
        assert_eq!(fetch_token_neg_risk(&t, "111"), Some(true));
        assert_eq!(t.paths(), vec!["/neg-risk".to_string()]);
        // an unserved endpoint is `None` — never a defaulted `false`
        assert_eq!(fetch_token_neg_risk(&PathStub::new(), "111"), None);
        assert_eq!(fetch_token_neg_risk(&FailingStub, "111"), None);
    }
}
