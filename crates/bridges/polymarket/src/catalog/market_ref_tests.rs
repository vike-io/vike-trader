use super::*;
use crate::catalog::gamma::parse_gamma_markets;
use crate::refusals::PolymarketMarketRefRefusalId;
use std::collections::HashMap;

/// The two token ids of the live market below, verbatim from Gamma (2026-10-26, read from a the latency box
/// lane): the 77-digit ids the operator used to paste by hand.
const XI_YES: &str =
    "32338220190071351435772801779725302244575775216413325951443816017994629993401";
const XI_NO: &str = "25659310674993675562345759665114759892400026242514633218387667107987341231962";

/// A real Gamma `/markets?slug=` row (2026-10-26), trimmed to the fields the parser reads plus the
/// ones that decide tradeability. `clobTokenIds` / `outcomes` are the JSON-ENCODED STRINGS the wire
/// really carries.
fn xi() -> serde_json::Value {
    serde_json::json!({
        "id": "559651",
        "question": "Xi Jinping out before 2027?",
        "conditionId": "0xa467b14d51f01b957109d9cbb1d6c124fab2a089d52ed8f471d23c2812e743b7",
        "slug": "xi-jinping-out-before-2027",
        "endDate": "2027-01-01T04:59:00Z",
        "outcomes": "[\"Yes\", \"No\"]",
        "outcomePrices": "[\"0.0435\", \"0.9565\"]",
        "active": true,
        "closed": false,
        "enableOrderBook": true,
        "acceptingOrders": true,
        "orderPriceMinTickSize": 0.001,
        "orderMinSize": 5,
        "negRisk": false,
        "clobTokenIds": format!("[\"{XI_YES}\", \"{XI_NO}\"]"),
        "events": [{ "id": "30828", "slug": "xi-jinping-out-before-2027",
                     "title": "Xi Jinping out before 2027?" }]
    })
}

fn market(slug: &str, question: &str, active: bool, closed: bool, toks: [&str; 2]) -> GammaMarket {
    let mut m = parse_gamma_markets(&serde_json::json!([{
        "id": slug, "question": question, "conditionId": format!("0xc-{slug}"), "slug": slug,
        "active": active, "closed": closed, "outcomes": "[\"Yes\", \"No\"]",
        "clobTokenIds": format!("[\"{}\", \"{}\"]", toks[0], toks[1]),
        "orderPriceMinTickSize": 0.01, "negRisk": true,
    }]))
    .remove(0);
    m.event_slug = "next-pm".to_string();
    m
}

/// An in-memory Gamma: markets by slug, events by slug. A slug listed in `fail` answers a transport
/// error, which is how a Gamma outage reaches the code.
#[derive(Default)]
struct FakeGamma {
    markets: HashMap<String, GammaMarket>,
    events: HashMap<String, Vec<GammaMarket>>,
    fail: bool,
}

impl FakeGamma {
    fn with(markets: Vec<GammaMarket>) -> Self {
        let mut g = FakeGamma::default();
        for m in markets {
            g.markets.insert(m.slug.clone(), m);
        }
        g
    }
}

impl MarketDirectory for FakeGamma {
    fn market(&self, slug: &str) -> Result<Option<GammaMarket>, String> {
        if self.fail {
            return Err("network: connection refused".to_string());
        }
        Ok(self.markets.get(slug).cloned())
    }
    fn event_markets(&self, slug: &str) -> Result<Vec<GammaMarket>, String> {
        if self.fail {
            return Err("network: connection refused".to_string());
        }
        Ok(self.events.get(slug).cloned().unwrap_or_default())
    }
}

/// A directory that must never be asked: a token id answers itself.
struct NoRead;
impl MarketDirectory for NoRead {
    fn market(&self, slug: &str) -> Result<Option<GammaMarket>, String> {
        panic!("a pasted token id must cost no read, but market({slug}) was asked")
    }
    fn event_markets(&self, slug: &str) -> Result<Vec<GammaMarket>, String> {
        panic!("a pasted token id must cost no read, but event_markets({slug}) was asked")
    }
}

fn xi_directory() -> FakeGamma {
    FakeGamma::with(parse_gamma_markets(&serde_json::json!([xi()])))
}

/// The refusal that asks which outcome of the Xi market is meant, both spelled with their token ids.
fn choose_xi() -> PolymarketMarketRefRefusal {
    PolymarketMarketRefRefusal::ChooseOutcome {
        slug: "xi-jinping-out-before-2027".to_string(),
        question: "Xi Jinping out before 2027?".to_string(),
        count: 2,
        listed: format!(
            "`xi-jinping-out-before-2027:Yes` (token {XI_YES}), `xi-jinping-out-before-2027:No` \
             (token {XI_NO})"
        ),
        more: String::new(),
    }
}

// --- parse ----------------------------------------------------------------------------------

#[test]
fn a_pasted_token_id_parses_as_itself() {
    assert_eq!(parse_market_ref(XI_YES), Ok(MarketRef::Token(XI_YES.to_string())));
    assert_eq!(parse_market_ref(&format!("  {XI_NO}\n")), Ok(MarketRef::Token(XI_NO.to_string())));
}

#[test]
fn a_slug_with_or_without_an_outcome_parses() {
    assert_eq!(
        parse_market_ref("xi-jinping-out-before-2027:Yes"),
        Ok(MarketRef::Market {
            slug: "xi-jinping-out-before-2027".to_string(),
            outcome: Some("Yes".to_string())
        })
    );
    assert_eq!(
        parse_market_ref("Xi-Jinping-Out-Before-2027"),
        Ok(MarketRef::Market { slug: "xi-jinping-out-before-2027".to_string(), outcome: None }),
        "a slug is lower-case on the venue; the operator's capitals are forgiven"
    );
}

#[test]
fn the_exact_market_address_reads_as_its_last_segment() {
    let want = MarketRef::Market { slug: "xi-jinping-out-before-2027".to_string(), outcome: None };
    for url in [
        "https://polymarket.com/event/xi-jinping-out-before-2027/xi-jinping-out-before-2027",
        "https://www.polymarket.com/event/some-event/xi-jinping-out-before-2027?tid=1730000000",
        "https://polymarket.com/event/some-event/xi-jinping-out-before-2027#comments",
        "https://polymarket.com/market/xi-jinping-out-before-2027/",
    ] {
        assert_eq!(parse_market_ref(url), Ok(want.clone()), "{url}");
    }
    assert_eq!(
        parse_market_ref("https://polymarket.com/event/next-prime-minister-of-ethiopia?x=1"),
        Ok(MarketRef::Event { slug: "next-prime-minister-of-ethiopia".to_string() })
    );
}

/// Every refusal says what WOULD work, and none of these may cost a network read.
#[test]
fn what_no_spelling_reads_is_refused_with_the_spellings_that_work() {
    let condition = format!("0x{}", "a".repeat(64));
    for (typed, id) in [
        ("", PolymarketMarketRefRefusalId::Empty),
        ("999", PolymarketMarketRefRefusalId::NotAName),
        ("BTC/USD", PolymarketMarketRefRefusalId::NotAName),
        ("will it happen", PolymarketMarketRefRefusalId::NotAName),
        ("xi-jinping-out-before-2027:", PolymarketMarketRefRefusalId::NotAName),
        ("https://example.com/event/x/y", PolymarketMarketRefRefusalId::NotPolymarketAddress),
        ("https://polymarket.com/profile/0xabc", PolymarketMarketRefRefusalId::NotAMarketPage),
        (condition.as_str(), PolymarketMarketRefRefusalId::ConditionId),
    ] {
        let why = parse_market_ref(typed).expect_err(typed);
        assert_eq!(why.id(), id, "{typed:?}: {why}");
        assert!(
            why.template().contains("token id")
                || why.template().contains("polymarket.com/event/<event>/<market>"),
            "{typed:?}: a refusal must name a spelling that works: {why}"
        );
    }
}

// --- resolve --------------------------------------------------------------------------------

#[test]
fn a_slug_and_an_outcome_resolve_to_the_token_id_and_the_market_facts() {
    let g = xi_directory();
    let yes = resolve_market_ref("xi-jinping-out-before-2027:Yes", &g).expect("resolves");
    assert_eq!(yes.token_id, XI_YES);
    assert_eq!(yes.outcome, "Yes");
    assert_eq!(yes.question, "Xi Jinping out before 2027?");
    assert_eq!(yes.neg_risk, Some(false));
    assert_eq!(yes.tick_size, Some(0.001));
    assert_eq!(yes.min_size, Some(5.0));
    assert_eq!(
        yes.condition_id,
        "0xa467b14d51f01b957109d9cbb1d6c124fab2a089d52ed8f471d23c2812e743b7"
    );
    // the outcome label is case-insensitive, and the second token is the second outcome
    assert_eq!(canonical_symbol("xi-jinping-out-before-2027:no", &g), Ok(XI_NO.to_string()));
    let why = canonical_symbol(
        "https://polymarket.com/event/xi-jinping-out-before-2027/xi-jinping-out-before-2027",
        &g,
    )
    .expect_err("a market address names no outcome");
    let choose = resolve_market_ref(
        "https://polymarket.com/event/xi-jinping-out-before-2027/xi-jinping-out-before-2027",
        &g,
    )
    .unwrap_err();
    assert_eq!(choose.id(), PolymarketMarketRefRefusalId::ChooseOutcome);
    assert_eq!(why, MarketNameRefusal::Refused(choose.to_string()));
}

/// What the gatewayhub decides "ask again" versus "remember the verdict" by: a failed Gamma READ is
/// [`MarketNameRefusal::Unread`] whatever its words, and an answer that names no outcome is a verdict.
#[test]
fn a_failed_read_is_unread_and_an_answered_name_is_a_verdict() {
    let down = FakeGamma { fail: true, ..FakeGamma::default() };
    let why = canonical_symbol("xi-jinping-out-before-2027:Yes", &down).unwrap_err();
    assert_eq!(why, MarketNameRefusal::Unread("network: connection refused".to_string()));
    let why = canonical_symbol("no-such-market:Yes", &xi_directory()).unwrap_err();
    assert!(matches!(why, MarketNameRefusal::Refused(_)), "{why}");
    let why = canonical_symbol("", &NoRead).unwrap_err();
    assert!(matches!(why, MarketNameRefusal::Refused(_)), "nothing typed is no read: {why}");
}

/// The pasted id keeps working unchanged — and costs nothing: the directory is never asked.
#[test]
fn a_pasted_token_id_is_canonical_as_it_is() {
    assert_eq!(canonical_symbol(XI_YES, &NoRead), Ok(XI_YES.to_string()));
    let r = resolve_market_ref(XI_NO, &NoRead).expect("a token resolves to itself");
    assert_eq!(r.token_id, XI_NO);
}

#[test]
fn a_bare_slug_is_refused_with_both_outcomes_spelled_out() {
    let why = resolve_market_ref("xi-jinping-out-before-2027", &xi_directory()).unwrap_err();
    assert_eq!(why, choose_xi());
}

#[test]
fn an_outcome_the_market_does_not_have_is_refused_naming_the_ones_it_has() {
    let why = resolve_market_ref("xi-jinping-out-before-2027:Maybe", &xi_directory()).unwrap_err();
    assert_eq!(
        why,
        PolymarketMarketRefRefusal::NoSuchOutcome {
            slug: "xi-jinping-out-before-2027".to_string(),
            want: "Maybe".to_string(),
            choose: Box::new(choose_xi()),
        }
    );
}

#[test]
fn an_unlisted_slug_is_refused_in_words_and_a_gamma_outage_is_not_mistaken_for_it() {
    let g = xi_directory();
    let why = resolve_market_ref("no-such-market:Yes", &g).unwrap_err();
    assert_eq!(
        why,
        PolymarketMarketRefRefusal::MarketNotListed { slug: "no-such-market".to_string() }
    );

    let down = FakeGamma { fail: true, ..FakeGamma::default() };
    let why = resolve_market_ref("xi-jinping-out-before-2027:Yes", &down).unwrap_err();
    assert_eq!(
        why,
        PolymarketMarketRefRefusal::GammaUnread { why: "network: connection refused".to_string() },
        "an outage is not 'no such market'"
    );
}

#[test]
fn a_closed_inactive_or_unmatched_market_has_no_tradeable_outcome() {
    let closed = FakeGamma::with(vec![market("done", "Q?", true, true, ["1", "2"])]);
    let why = resolve_market_ref("done:Yes", &closed).unwrap_err();
    assert_eq!(why, PolymarketMarketRefRefusal::MarketClosed { slug: "done".to_string() });

    let idle = FakeGamma::with(vec![market("idle", "Q?", false, false, ["1", "2"])]);
    let why = resolve_market_ref("idle:Yes", &idle).unwrap_err();
    assert_eq!(why, PolymarketMarketRefRefusal::MarketInactive { slug: "idle".to_string() });

    let mut odd = market("odd", "Q?", true, false, ["1", "2"]);
    odd.token_ids.pop();
    let why = resolve_market_ref("odd:Yes", &FakeGamma::with(vec![odd])).unwrap_err();
    assert_eq!(
        why,
        PolymarketMarketRefRefusal::OutcomesUnmatched {
            outcomes: 2,
            tokens: 1,
            slug: "odd".to_string()
        }
    );
}

/// A real RESOLVED market, as Gamma sends it when asked with `closed=true` (2026-10-26, a the latency box
/// lane; trimmed). Note what the live read taught: it still says `active: true` — only `closed` and
/// `acceptingOrders` tell it is over — so `closed` is judged first.
#[test]
fn a_real_resolved_market_is_refused_as_closed_though_gamma_still_calls_it_active() {
    let row = serde_json::json!([{
        "id": "253591",
        "question": "Will Donald Trump win the 2024 US Presidential Election?",
        "conditionId": "0xdd22472e552920b8438158ea7238bfadfa4f736aa4cee91a6b86c39ead110917",
        "slug": "will-donald-trump-win-the-2024-us-presidential-election",
        "outcomes": "[\"Yes\", \"No\"]",
        "outcomePrices": "[\"1\", \"0\"]",
        "active": true,
        "closed": true,
        "enableOrderBook": true,
        "acceptingOrders": false,
        "orderPriceMinTickSize": 0.001,
        "negRisk": true,
        "clobTokenIds": "[\"21742633143463906290569050155826241533067272736897614950488156847949938836455\", \"48331043336612883890938759509493159234755048973500640148014422747788308965732\"]"
    }]);
    let m = parse_gamma_markets(&row).remove(0);
    assert!(m.active && m.closed && m.accepting_orders == Some(false));
    let why = resolve_market_ref(
        "will-donald-trump-win-the-2024-us-presidential-election:Yes",
        &FakeGamma::with(vec![m]),
    )
    .unwrap_err();
    assert_eq!(why.id(), PolymarketMarketRefRefusalId::MarketClosed, "{why}");
}

/// An OPEN market that takes no orders right now (paused) is refused in words, not signed against.
#[test]
fn a_market_that_is_open_but_not_accepting_orders_is_refused() {
    let mut m = market("paused", "Q?", true, false, ["1", "2"]);
    m.accepting_orders = Some(false);
    let why = resolve_market_ref("paused:Yes", &FakeGamma::with(vec![m.clone()])).unwrap_err();
    assert_eq!(why.id(), PolymarketMarketRefRefusalId::MarketNotAcceptingOrders, "{why}");
    // `None` (Gamma said nothing) and `Some(true)` do not block
    for ok in [None, Some(true)] {
        m.accepting_orders = ok;
        assert!(resolve_market_ref("paused:Yes", &FakeGamma::with(vec![m.clone()])).is_ok());
    }
}

#[test]
fn an_event_page_resolves_only_when_it_holds_one_open_market() {
    let mut g = FakeGamma::default();
    // one open market beside a closed one: the open one is THE market, and still needs an outcome
    g.events.insert(
        "one-open".to_string(),
        vec![
            market("old-one", "Old?", true, true, ["1", "2"]),
            market("new-one", "New?", true, false, ["3", "4"]),
        ],
    );
    let why = resolve_market_ref("https://polymarket.com/event/one-open", &g).unwrap_err();
    assert_eq!(why.id(), PolymarketMarketRefRefusalId::ChooseOutcome);
    assert_eq!(
        why.to_string(),
        "polymarket: the market `new-one` («New?») has 2 outcomes and an order buys or sells ONE \
         of them, so name which: `new-one:Yes` (token 3), `new-one:No` (token 4)"
    );

    // several open markets: refuse, listing the slugs that would work
    g.events.insert(
        "next-pm".to_string(),
        (0..10)
            .map(|i| market(&format!("pm-{i}"), &format!("Will {i} win?"), true, false, ["5", "6"]))
            .collect(),
    );
    let why = resolve_market_ref("https://polymarket.com/event/next-pm", &g).unwrap_err();
    assert_eq!(why.id(), PolymarketMarketRefRefusalId::EventHoldsMany);
    assert_eq!(
        why.to_string(),
        "polymarket: the event `next-pm` holds 10 open markets, so name one market (then its \
         outcome, `<market>:Yes`): `pm-0` («Will 0 win?»), `pm-1` («Will 1 win?»), `pm-2` («Will \
         2 win?»), `pm-3` («Will 3 win?»), `pm-4` («Will 4 win?»), `pm-5` («Will 5 win?»), `pm-6` \
         («Will 6 win?»), `pm-7` («Will 7 win?»), and 2 more",
        "the list is bounded"
    );

    let why = resolve_market_ref("https://polymarket.com/event/nothing", &g).unwrap_err();
    assert_eq!(why, PolymarketMarketRefRefusal::EventNotListed { slug: "nothing".to_string() });

    g.events.insert("all-done".to_string(), vec![market("d", "D?", true, true, ["7", "8"])]);
    let why = resolve_market_ref("https://polymarket.com/event/all-done", &g).unwrap_err();
    assert_eq!(why, PolymarketMarketRefRefusal::EventAllClosed { slug: "all-done".to_string() });
}

// --- the exec thread's explanation ----------------------------------------------------------

#[test]
fn explain_symbol_hands_the_operator_the_token_id_of_the_outcome_they_typed() {
    let words = explain_symbol("xi-jinping-out-before-2027:Yes", &xi_directory()).expect("words");
    assert!(words.contains("is a market name") && words.contains(XI_YES), "{words}");
    assert!(words.contains("Yes of «Xi Jinping out before 2027?»"), "{words}");
    assert!(words.ends_with("The order was NOT sent"), "{words}");
    assert!(!words.contains(XI_NO), "only the outcome that was named: {words}");

    let words = explain_symbol("no-such-market:Yes", &xi_directory()).expect("words");
    let not_listed =
        PolymarketMarketRefRefusal::MarketNotListed { slug: "no-such-market".to_string() };
    assert_eq!(words, format!("{not_listed}. The order was NOT sent"));
}

/// Only a NAME-shaped symbol is explained: a token id and a symbol no spelling reads keep the
/// caller's own refusal (the NegRisk one) and cost no read.
#[test]
fn explain_symbol_leaves_everything_but_a_name_to_the_callers_own_refusal() {
    assert_eq!(explain_symbol(XI_YES, &NoRead), None);
    assert_eq!(explain_symbol("999", &NoRead), None);
    assert_eq!(explain_symbol("", &NoRead), None);
    assert_eq!(explain_symbol("BTC/USD", &NoRead), None);
}

/// The one predicate (`vike_catalog::is_outcome_token`) decides what parses as a token: 20 to 78 digits, the
/// most a uint256 holds; 79 digits is no token id and no slug either.
#[test]
fn the_token_shape_is_one_predicate() {
    for (text, token) in [
        ("7".repeat(20), true),
        ("7".repeat(78), true),
        ("7".repeat(19), false),
        ("7".repeat(79), false),
        ("BTCUSDT".to_string(), false),
        (format!("0x{}", "a".repeat(64)), false),
    ] {
        assert_eq!(vike_catalog::is_outcome_token(&text), token, "{text}");
        assert_eq!(matches!(parse_market_ref(&text), Ok(MarketRef::Token(_))), token, "{text}");
    }
    assert!(parse_market_ref(&"7".repeat(79)).is_err(), "79 digits is no name at all");
}
