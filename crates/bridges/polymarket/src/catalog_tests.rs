use super::*;
use crate::gamma::GammaMarket;

fn mk(q: &str, slug: &str, cid: &str, toks: &[&str]) -> GammaMarket {
    GammaMarket {
        id: "1".into(),
        question: q.into(),
        condition_id: cid.into(),
        slug: slug.into(),
        end_date: "".into(),
        volume: 0.0,
        liquidity: 0.0,
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
fn search_is_case_insensitive_over_question_and_slug() {
    let cat = MarketCatalog::from_markets(vec![
        mk("Will BTC be above $100k?", "btc-100k", "0x1", &["a", "b"]),
        mk("ETH flippening", "eth-flip", "0x2", &["c"]),
    ]);
    let r = cat.search("btc");
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].condition_id, "0x1");
    // matches slug too:
    assert_eq!(cat.search("FLIP").len(), 1);
    // no match:
    assert!(cat.search("dogecoin").is_empty());
}

#[test]
fn by_condition_and_token_ids() {
    let cat = MarketCatalog::from_markets(vec![mk("q", "s", "0xabc", &["t1", "t2"])]);
    let m = cat.by_condition("0xabc").expect("found");
    assert_eq!(m.token_ids, vec!["t1".to_string(), "t2".to_string()]);
    assert!(cat.by_condition("0xnope").is_none());
}

const MID: &str = "0x55ab76d092f682bf5cbb7e14f13ee12f8410ce7cc1b7906f23b8fb56c11f6500";

fn nr(idx: u32, title: &str, cid: &str, yes: f64) -> GammaMarket {
    GammaMarket {
        question: format!("Will {title} win?"),
        condition_id: cid.into(),
        active: true,
        neg_risk: true,
        outcomes: vec!["Yes".into(), "No".into()],
        token_ids: vec![format!("y{idx}"), format!("n{idx}")],
        outcome_prices: vec![yes, 1.0 - yes],
        neg_risk_market_id: MID.into(),
        group_item_title: title.into(),
        group_item_index: Some(idx),
        event_slug: "next-prime-minister-of-ethiopia".into(),
        ..Default::default()
    }
}

#[test]
fn catalog_surfaces_neg_risk_sets_and_resolves_one_by_id() {
    let cat = MarketCatalog::from_markets(vec![
        nr(0, "A", "0xa", 0.50),
        mk("plain binary", "bin", "0xbin", &["t"]), // non-neg-risk: contributes no set
        nr(1, "B", "0xb", 0.30),
        nr(2, "C", "0xc", 0.15),
    ]);
    let sets = cat.neg_risk_sets();
    assert_eq!(sets.len(), 1, "the binary market forms no set");
    assert_eq!(sets[0].len(), 3);
    assert_eq!(sets[0].event_slug, "next-prime-minister-of-ethiopia");
    assert_eq!(sets[0].yes_token_ids(), vec!["y0", "y1", "y2"]);
    // Σ = 0.95 over a complete set → a 5c gross screen edge
    assert!((sets[0].yes_price_sum().unwrap() - 0.95).abs() < 1e-12);

    // resolve by the group key, case-insensitively
    let s = cat.neg_risk_set(MID).expect("resolved");
    assert_eq!(s.market_id, MID);
    assert_eq!(cat.neg_risk_set(&MID.to_uppercase()).unwrap().len(), 3);
    assert!(cat.neg_risk_set("0xnope").is_none());
}

#[test]
fn neg_risk_set_for_condition_finds_the_siblings_of_a_held_market() {
    let cat = MarketCatalog::from_markets(vec![
        nr(0, "A", "0xa", 0.5),
        nr(1, "B", "0xb", 0.5),
        mk("plain binary", "bin", "0xbin", &["t"]),
    ]);
    let s = cat.neg_risk_set_for_condition("0xb").expect("member of a set");
    assert_eq!(s.len(), 2);
    assert!(s.members.iter().any(|m| m.condition_id == "0xa"), "sees its sibling");
    // a non-neg-risk market has no set — NOT a degenerate one-member one
    assert!(cat.neg_risk_set_for_condition("0xbin").is_none());
    assert!(cat.neg_risk_set_for_condition("0xunknown").is_none());
}

fn open_market() -> GammaMarket {
    GammaMarket {
        question: "Will it rain?".into(),
        condition_id: "0xrain".into(),
        active: true,
        accepting_orders: Some(true),
        neg_risk_declared: Some(true),
        tick_size: 0.001,
        order_min_size: Some(5.0),
        outcomes: vec!["Yes".into(), "No".into()],
        token_ids: vec!["111".into(), "222".into()],
        ..Default::default()
    }
}

#[test]
fn an_outcome_row_carries_the_markets_grid_neg_risk_and_the_share_lot() {
    let rows = outcome_rows_of(&open_market(), "222");
    assert_eq!(rows.len(), 1, "the ONE token asked about, not its sibling outcome");
    let row = &rows[0];
    assert_eq!(row.raw_symbol, "222");
    assert_eq!(row.properties.tick_size, 0.001, "the market's own tick");
    assert_eq!(row.properties.min_qty, 5.0, "orderMinSize, in shares");
    assert_eq!(row.properties.step_size, SHARE_STEP, "the share lot the row itself lacks");
    assert_eq!(row.contract_type, None, "an open market carries no refusal word");
    assert!(
        matches!(&row.contract, Contract::PredictionOutcome(spec)
            if spec.neg_risk == Some(true) && spec.outcome == "No" && spec.token_id == "222"),
        "{:?}",
        row.contract
    );
    assert!(outcome_rows_of(&open_market(), "333").is_empty(), "a token the market does not list");
}

#[test]
fn a_market_that_takes_no_orders_is_marked_for_the_facts_to_refuse() {
    for (what, market) in [
        ("closed", GammaMarket { closed: true, ..open_market() }),
        ("inactive", GammaMarket { active: false, ..open_market() }),
        ("paused", GammaMarket { accepting_orders: Some(false), ..open_market() }),
    ] {
        let rows = outcome_rows_of(&market, "111");
        assert_eq!(
            rows[0].contract_type.as_deref(),
            Some(VENUE_WORD_NOT_ACCEPTING_ORDERS),
            "{what}"
        );
    }
    // Gamma that says nothing about acceptingOrders is not a pause.
    let silent = GammaMarket { accepting_orders: None, ..open_market() };
    assert_eq!(outcome_rows_of(&silent, "111")[0].contract_type, None);
}
