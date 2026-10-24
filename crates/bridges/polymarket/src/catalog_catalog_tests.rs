use super::*;
use crate::gamma::GammaMarket;
use vike_model::AssetClass;

fn mk_with_outcomes(q: &str, cid: &str, outcomes: &[&str], toks: &[&str]) -> GammaMarket {
    GammaMarket {
        id: "1".into(),
        question: q.into(),
        condition_id: cid.into(),
        slug: "s".into(),
        end_date: "".into(),
        volume: 0.0,
        liquidity: 0.0,
        active: true,
        closed: false,
        neg_risk: false,
        tick_size: 0.01,
        outcomes: outcomes.iter().map(|s| s.to_string()).collect(),
        token_ids: toks.iter().map(|s| s.to_string()).collect(),
        ..Default::default()
    }
}

#[test]
fn one_instrument_per_outcome_token_tagged_prediction_market() {
    let markets = vec![
        mk_with_outcomes("Will BTC be above $100k?", "0x1", &["Yes", "No"], &["t1", "t2"]),
        mk_with_outcomes("ETH flippening?", "0x2", &["Yes", "No"], &["t3", "t4"]),
    ];
    let out = markets_to_instruments(&markets);
    assert_eq!(out.len(), 4);
    for inst in &out {
        assert_eq!(inst.venue, "polymarket");
        assert_eq!(inst.asset_class(), AssetClass::PredictionMarket);
        assert_eq!(inst.quote, "USDC");
    }
    assert_eq!(out[0].raw_symbol, "t1");
    assert_eq!(out[0].base, "Yes");
    assert_eq!(out[0].description, "Will BTC be above $100k? — Yes");
    assert_eq!(out[1].raw_symbol, "t2");
    assert_eq!(out[1].base, "No");
    assert_eq!(out[1].description, "Will BTC be above $100k? — No");
    assert_eq!(out[2].raw_symbol, "t3");
    assert_eq!(out[2].description, "ETH flippening? — Yes");
    assert_eq!(out[3].raw_symbol, "t4");
    assert_eq!(out[3].description, "ETH flippening? — No");
}

/// The live Gamma `/markets` shapes: the non-neg-risk Rihanna market (no `negRisk` key) and one
/// member of the 7-way "Next Prime Minister of Ethiopia" neg-risk set (field values copied from
/// `gamma`'s live-verified fixtures). `orderMinSize` is a live Gamma field
/// (`docs/superpowers/specs/2026-07-11-gamma-catalog-design.md`); the third market declares
/// `negRisk: false` explicitly.
fn live_gamma_page() -> serde_json::Value {
    serde_json::json!([
        {
            "id": "540817",
            "question": "New Rihanna Album before GTA VI?",
            "conditionId": "0x1fad72fae204143ff1c3035e99e7c0f65ea8d5cd9bd1070987bd1a3316f772be",
            "slug": "new-rhianna-album-before-gta-vi-926",
            "endDate": "2026-07-31T12:00:00Z",
            "active": true, "closed": false,
            "orderPriceMinTickSize": 0.01,
            "orderMinSize": 5,
            "outcomes": "[\"Yes\", \"No\"]",
            "clobTokenIds": "[\"111222\", \"333444\"]"
        },
        {
            "id": "2063135",
            "question": "Will Gedion Timothewos be the next Prime Minister of Ethiopia?",
            "conditionId": "0xb6d6f15a1b5d08753653f1867ccd6126badfbe182a75159a330dc7b15336b309",
            "slug": "will-gedion-timothewos-be-the-next-prime-minister-of-ethiopia",
            "active": true, "closed": false,
            "negRisk": true, "negRiskOther": false,
            "negRiskMarketID": "0x55ab76d092f682bf5cbb7e14f13ee12f8410ce7cc1b7906f23b8fb56c11f6500",
            "groupItemTitle": "Gedion Timothewos",
            "groupItemThreshold": "7",
            "orderPriceMinTickSize": 0.01,
            "outcomes": "[\"Yes\", \"No\"]",
            "clobTokenIds": "[\"111\", \"222\"]"
        },
        {
            "id": "3",
            "question": "Explicitly not neg-risk?",
            "conditionId": "0x3",
            "negRisk": false,
            "outcomes": "[\"Yes\"]",
            "clobTokenIds": "[\"999\"]"
        }
    ])
}

#[test]
fn gamma_payload_fields_build_the_exact_prediction_contract() {
    let markets = crate::gamma::parse_gamma_markets(&live_gamma_page());
    let out = markets_to_instruments(&markets);
    assert_eq!(out.len(), 5);

    // Rihanna, NO leg: no `negRisk` key -> unknown, not false; endDate -> resolution_ms.
    let no = &out[1];
    assert_eq!(no.raw_symbol, "333444");
    assert_eq!(no.base, "No");
    assert_eq!(no.quote, "USDC");
    assert_eq!(no.description, "New Rihanna Album before GTA VI? — No");
    assert_eq!(
        no.contract,
        Contract::PredictionOutcome(PredictionSpec {
            condition_id: "0x1fad72fae204143ff1c3035e99e7c0f65ea8d5cd9bd1070987bd1a3316f772be"
                .into(),
            outcome: "No".into(),
            token_id: "333444".into(),
            resolution_ms: Some(1_785_499_200_000), // 2026-07-31T12:00:00Z
            neg_risk: None,
        })
    );
    assert_eq!(no.properties.tick_size, 0.01);
    assert_eq!(no.properties.min_qty, 5.0);
    assert_eq!(no.wire, WireIds { asset_id: None, coin: None, group: None });
    assert_eq!(no.contract_type, None, "Polymarket has no venue contract word");

    // Ethiopia member, YES leg: neg-risk as sent, the set key is the wire group, no endDate.
    let yes = &out[2];
    assert_eq!(yes.raw_symbol, "111");
    assert_eq!(
        yes.contract,
        Contract::PredictionOutcome(PredictionSpec {
            condition_id: "0xb6d6f15a1b5d08753653f1867ccd6126badfbe182a75159a330dc7b15336b309"
                .into(),
            outcome: "Yes".into(),
            token_id: "111".into(),
            resolution_ms: None,
            neg_risk: Some(true),
        })
    );
    assert_eq!(yes.properties.tick_size, 0.01);
    assert_eq!(yes.properties.min_qty, 0.0, "absent orderMinSize is the 0.0 absent convention");
    assert_eq!(
        yes.wire.group.as_deref(),
        Some("0x55ab76d092f682bf5cbb7e14f13ee12f8410ce7cc1b7906f23b8fb56c11f6500")
    );
    assert_eq!(yes.wire.coin, None);

    // An explicit `negRisk: false` is Some(false), distinct from the absent key above.
    match &out[4].contract {
        Contract::PredictionOutcome(spec) => assert_eq!(spec.neg_risk, Some(false)),
        other => panic!("expected PredictionOutcome, got {other:?}"),
    }
    assert_eq!(out[4].properties.tick_size, 0.0);
}

#[test]
fn missing_outcomes_falls_back_to_token_id_never_panics() {
    // outcomes shorter than token_ids (or absent entirely) — description must never go blank.
    let markets = vec![mk_with_outcomes("no outcome labels", "0x3", &[], &["tokA", "tokB"])];
    let out = markets_to_instruments(&markets);
    assert_eq!(out.len(), 2);
    assert_eq!(out[0].raw_symbol, "tokA");
    assert_eq!(out[0].base, "tokA");
    assert_eq!(out[0].description, "no outcome labels — tokA");
    assert_eq!(out[1].raw_symbol, "tokB");
    assert_eq!(out[1].description, "no outcome labels — tokB");
}

#[test]
fn no_token_ids_contributes_nothing() {
    let markets = vec![mk_with_outcomes("no tokens", "0x4", &["Yes", "No"], &[])];
    assert!(markets_to_instruments(&markets).is_empty());
}

#[test]
fn provider_declares_venue_asset_class_and_enumerable_mode() {
    let p = PolymarketCatalog;
    assert_eq!(p.venue(), "polymarket");
    assert_eq!(p.asset_classes(), &[AssetClass::PredictionMarket]);
    assert_eq!(p.mode(), CatalogMode::Enumerable);
}

/// A page of `n` distinct markets whose ids encode `(offset, i)` so the tests can prove which
/// offsets were fetched and that ordering/accumulation is intact.
fn page(offset: usize, n: usize) -> Vec<GammaMarket> {
    (0..n)
        .map(|i| GammaMarket {
            id: format!("{}", offset + i),
            question: "q".into(),
            condition_id: format!("0x{:x}", offset + i),
            active: true,
            token_ids: vec![format!("t{}", offset + i)],
            ..Default::default()
        })
        .collect()
}

#[test]
fn pages_until_a_short_page_advancing_the_offset() {
    use std::cell::RefCell;
    let calls: RefCell<Vec<(usize, usize)>> = RefCell::new(Vec::new());
    let out = paged_markets(|limit, offset| {
        calls.borrow_mut().push((limit, offset));
        // two full pages then a short one
        Ok(if offset < 2 * GAMMA_PAGE_LIMIT { page(offset, limit) } else { page(offset, 3) })
    })
    .unwrap();
    assert_eq!(out.len(), 2 * GAMMA_PAGE_LIMIT + 3, "all pages accumulated");
    assert_eq!(
        *calls.borrow(),
        vec![
            (GAMMA_PAGE_LIMIT, 0),
            (GAMMA_PAGE_LIMIT, GAMMA_PAGE_LIMIT),
            (GAMMA_PAGE_LIMIT, 2 * GAMMA_PAGE_LIMIT)
        ],
        "offset advances by the page limit; the short page ends the crawl"
    );
    assert_eq!(out[0].id, "0");
    assert_eq!(out.last().unwrap().id, format!("{}", 2 * GAMMA_PAGE_LIMIT + 2));
}

#[test]
fn a_short_first_page_makes_exactly_one_request() {
    use std::cell::Cell;
    let calls = Cell::new(0usize);
    let out = paged_markets(|_, offset| {
        calls.set(calls.get() + 1);
        Ok(page(offset, 7))
    })
    .unwrap();
    assert_eq!(out.len(), 7);
    assert_eq!(calls.get(), 1, "a short first page must not trigger a second fetch");
}

#[test]
fn an_empty_venue_yields_an_empty_catalog() {
    let out = paged_markets(|_, _| Ok(Vec::new())).unwrap();
    assert!(out.is_empty());
}

#[test]
fn the_page_ceiling_bounds_a_never_short_listing() {
    use std::cell::Cell;
    let calls = Cell::new(0usize);
    let out = paged_markets(|limit, offset| {
        calls.set(calls.get() + 1);
        Ok(page(offset, limit)) // every page full — Gamma "never ends"
    })
    .unwrap();
    assert_eq!(calls.get(), GAMMA_MAX_PAGES, "the defensive ceiling stops the crawl");
    assert_eq!(out.len(), GAMMA_MAX_PAGES * GAMMA_PAGE_LIMIT);
}

#[test]
fn first_page_failure_is_the_venue_being_down_but_a_later_failure_degrades() {
    // first page failing → Err (the previous single-page behavior)
    assert!(paged_markets(|_, _| Err("network: simulated".into())).is_err());
    // a later page failing → the earlier pages are KEPT (warned), never an error
    let out = paged_markets(|limit, offset| {
        if offset == 0 { Ok(page(0, limit)) } else { Err("network: simulated".into()) }
    })
    .unwrap();
    assert_eq!(out.len(), GAMMA_PAGE_LIMIT, "page 0 survives the page-1 failure");
}
