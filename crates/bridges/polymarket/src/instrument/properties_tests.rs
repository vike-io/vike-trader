use super::*;

/// The CLOB base every lookup here is handed; the stubs answer by path and never read it.
const CLOB: &str = "https://clob.invalid";

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
    assert_eq!(m.rewards, crate::instrument::rewards::RewardsConfig::default());
    assert!(!m.rewards.earns_rewards());
}

#[test]
fn cursor_end_detection() {
    assert_eq!(next_cursor(&sample()), None); // "LTE=" == end
    assert_eq!(next_cursor(&serde_json::json!({"next_cursor": "abc"})), Some("abc".to_string()));
}

/// Canned-page transport for [`fetch_token_tick_size`] tests: replays `pages` in order by
/// cursor value (empty cursor = page 0), or errors if asked past the end.
struct PageStub {
    pages: Vec<serde_json::Value>,
}

impl RestTransport for PageStub {
    vike_bridge_core::no_signed_call!("fetch_token_tick_size never signs a request");
    fn public(
        &self,
        _base: &str,
        _path: &str,
        params: &[(&str, String)],
    ) -> Result<serde_json::Value, VenueApiError> {
        let index = match params.iter().find(|(k, _)| *k == "next_cursor") {
            None => 0,
            Some((_, c)) => self
                .pages
                .iter()
                .position(|p| next_cursor(p).as_deref() == Some(c.as_str()))
                .map(|i| i + 1)
                .unwrap_or(usize::MAX),
        };
        self.pages
            .get(index)
            .cloned()
            .ok_or(VenueApiError { code: 0, msg: "past last page".into() })
    }
}

/// A failing transport — every `/markets` page request errors.
struct FailingStub;
impl RestTransport for FailingStub {
    vike_bridge_core::no_signed_call!("fetch_token_tick_size never signs a request");
    fn public(
        &self,
        _base: &str,
        _path: &str,
        _params: &[(&str, String)],
    ) -> Result<serde_json::Value, VenueApiError> {
        Err(VenueApiError { code: 0, msg: "network down".into() })
    }
}

/// The whole-catalog walk as it always read: every page to the end cursor, at most `max_pages`, and ANY
/// page failing (the first or a later one) the walk's error, never a short catalog.
#[test]
fn the_markets_walk_reads_to_the_end_mark_within_its_ceiling_and_fails_whole() {
    let two = PageStub {
        pages: vec![page("c-1", "t-1", "0.01", "n1"), page("c-2", "t-2", "0.01", "LTE=")],
    };
    let ids = |max| -> Vec<String> {
        fetch_all_markets(&two, "", max).unwrap().into_iter().map(|m| m.condition_id).collect()
    };
    assert_eq!(ids(10), ["c-1", "c-2"]);
    assert_eq!(ids(1), ["c-1"], "the ceiling stops the walk (with a warn line)");
    assert!(ids(0).is_empty());
    assert!(fetch_all_markets(&FailingStub, "", 10).is_err());
    let cut = PageStub { pages: vec![page("c-1", "t-1", "0.01", "n1")] };
    assert!(fetch_all_markets(&cut, "", 10).is_err(), "a later page failing fails the walk");
}

/// A page handing back the cursor just spent ends the walk there (owner ruling C-D2), with the pages
/// read once each: it used to ask that cursor again until the ceiling, appending the same page each time.
#[test]
fn the_markets_walk_stops_on_a_repeated_cursor() {
    let stuck = PageStub {
        pages: vec![page("c-1", "t-1", "0.01", "n1"), page("c-2", "t-2", "0.01", "n1")],
    };
    let ids: Vec<String> =
        fetch_all_markets(&stuck, "", 10).unwrap().into_iter().map(|m| m.condition_id).collect();
    assert_eq!(ids, ["c-1", "c-2"]);
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
    assert_eq!(fetch_token_tick_size(&t, CLOB, "111"), Some(0.001));
}

#[test]
fn tick_size_found_after_walking_a_later_page() {
    let t = PageStub {
        pages: vec![
            page("0xcond1", "111", "0.01", "cursor2"),
            page("0xcond2", "222", "0.005", "LTE="),
        ],
    };
    assert_eq!(fetch_token_tick_size(&t, CLOB, "222"), Some(0.005));
}

/// No default grid (owner, 2026-11-13): a token the catalog does not carry has no tick.
#[test]
fn a_token_not_in_the_catalog_has_no_tick() {
    let t = PageStub { pages: vec![page("0xcond1", "111", "0.01", "LTE=")] };
    assert_eq!(fetch_token_tick_size(&t, CLOB, "does-not-exist"), None);
}

/// …and neither has a token whose lookup failed: never a guessed tick.
#[test]
fn a_rest_failure_resolves_no_tick() {
    assert_eq!(fetch_token_tick_size(&FailingStub, CLOB, "111"), None);
}

/// A market row that states no tick resolves none: the walk finds the token's market and answers nothing
/// rather than a guessed grid.
#[test]
fn a_market_row_without_a_tick_resolves_no_tick() {
    let mut row = page("0xcond1", "111", "0.01", "LTE=");
    row["data"][0].as_object_mut().expect("a market row").remove("minimum_tick_size");
    assert_eq!(parse_markets(&row)[0].tick_size, 0.0, "the row states none");
    let t = PageStub { pages: vec![row] };
    assert_eq!(fetch_token_tick_size(&t, CLOB, "111"), None);
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
    vike_bridge_core::no_signed_call!("the tick-size / neg-risk lookups never sign a request");
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
                let index = match params.iter().find(|(k, _)| *k == "next_cursor") {
                    None => 0,
                    Some((_, c)) => self
                        .pages
                        .iter()
                        .position(|p| next_cursor(p).as_deref() == Some(c.as_str()))
                        .map(|i| i + 1)
                        .unwrap_or(usize::MAX),
                };
                self.pages
                    .get(index)
                    .cloned()
                    .ok_or(VenueApiError { code: 0, msg: "past last page".into() })
            }
        }
    }
}

#[test]
fn tick_size_body_parse_accepts_string_and_number_and_rejects_garbage() {
    assert_eq!(parse_tick_size(&serde_json::json!({"minimum_tick_size": "0.001"})), Some(0.001));
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
    assert_eq!(fetch_token_tick_size(&t, CLOB, "111"), Some(0.001), "the direct lookup wins");
    assert_eq!(t.paths(), vec!["/tick-size".to_string()], "no /markets page was walked");
}

#[test]
fn a_deployment_without_the_direct_endpoint_falls_back_to_the_page_walk() {
    // `/tick-size` errors (no canned body) → the walk resolves the SAME value it always did.
    let t = PathStub::new().with_pages(vec![
        page("0xcond1", "111", "0.01", "cursor2"),
        page("0xcond2", "222", "0.005", "LTE="),
    ]);
    assert_eq!(fetch_token_tick_size(&t, CLOB, "222"), Some(0.005));
    assert_eq!(
        t.paths(),
        vec!["/tick-size".to_string(), "/markets".to_string(), "/markets".to_string()],
        "the direct lookup is tried first, then the walk"
    );
}

#[test]
fn neither_lookup_resolving_answers_no_tick() {
    let t = PathStub::new().with_pages(vec![page("0xcond1", "111", "0.01", "LTE=")]);
    assert_eq!(fetch_token_tick_size(&t, CLOB, "does-not-exist"), None);
}

#[test]
fn direct_neg_risk_lookup_round_trip() {
    let t = PathStub::new().with_neg_risk(serde_json::json!({"neg_risk": true}));
    assert_eq!(fetch_token_neg_risk(&t, CLOB, "111"), Some(true));
    assert_eq!(t.paths(), vec!["/neg-risk".to_string()]);
    // an unserved endpoint is `None` — never a defaulted `false`
    assert_eq!(fetch_token_neg_risk(&PathStub::new(), CLOB, "111"), None);
    assert_eq!(fetch_token_neg_risk(&FailingStub, CLOB, "111"), None);
}
