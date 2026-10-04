use super::*;

/// A member of the LIVE 7-way "Next Prime Minister of Ethiopia" set (see `gamma.rs`'s doc).
const ETH_MARKET_ID: &str = "0x55ab76d092f682bf5cbb7e14f13ee12f8410ce7cc1b7906f23b8fb56c11f6500";

fn mk(idx: u32, title: &str, yes: Option<f64>, market_id: &str) -> GammaMarket {
    GammaMarket {
        id: format!("m{idx}"),
        question: format!("Will {title} …?"),
        condition_id: format!("0xcond{idx}"),
        slug: format!("s{idx}"),
        active: true,
        neg_risk: true,
        tick_size: 0.01,
        outcomes: vec!["Yes".into(), "No".into()],
        token_ids: vec![format!("yes{idx}"), format!("no{idx}")],
        outcome_prices: match yes {
            Some(p) => vec![p, 1.0 - p],
            None => vec![],
        },
        question_id: neg_risk_question_id(market_id, idx).unwrap_or_default(),
        neg_risk_market_id: market_id.into(),
        neg_risk_request_id: format!("0xreq{idx}"),
        group_item_title: title.into(),
        group_item_index: Some(idx),
        event_id: "411239".into(),
        event_slug: "next-prime-minister-of-ethiopia".into(),
        event_title: "Next Prime Minister of Ethiopia".into(),
        ..Default::default()
    }
}

/// A complete 3-way set, the happy path.
fn complete_set() -> Vec<GammaMarket> {
    vec![
        mk(1, "B", Some(0.30), ETH_MARKET_ID),
        mk(0, "A", Some(0.50), ETH_MARKET_ID), // out of order on purpose
        mk(2, "C", Some(0.15), ETH_MARKET_ID),
    ]
}

#[test]
fn groups_by_neg_risk_market_id_not_request_id() {
    // THE correctness point: `negRiskRequestID` is per-market (each mk gives a distinct one),
    // so grouping on it would yield 3 singleton sets instead of 1 set of 3.
    let sets = NegRiskSet::group(&complete_set());
    assert_eq!(sets.len(), 1, "one negRiskMarketID => one set");
    let s = &sets[0];
    assert_eq!(s.market_id, ETH_MARKET_ID);
    assert_eq!(s.len(), 3);
    assert_eq!(s.event_slug, "next-prime-minister-of-ethiopia");
    // members come back sorted by index regardless of input order
    assert_eq!(
        s.members.iter().map(|m| m.index).collect::<Vec<_>>(),
        vec![Some(0), Some(1), Some(2)]
    );
    assert_eq!(s.members[0].title, "A");
    // two distinct market ids => two sets
    let mut two = complete_set();
    two.push(mk(
        0,
        "Z",
        Some(0.9),
        "0xaa11223344556677889900aabbccddeeff00112233445566778899aabbccdd00",
    ));
    assert_eq!(NegRiskSet::group(&two).len(), 2);
}

#[test]
fn non_neg_risk_markets_are_not_degenerate_sets() {
    let plain = GammaMarket {
        question: "binary".into(),
        condition_id: "0x1".into(),
        token_ids: vec!["y".into(), "n".into()],
        outcome_prices: vec![0.5, 0.5],
        ..Default::default()
    };
    assert!(!plain.is_neg_risk_member());
    assert!(NegRiskSet::group(&[plain]).is_empty());
}

#[test]
fn token_id_accessors_are_index_ordered() {
    let s = &NegRiskSet::group(&complete_set())[0];
    assert_eq!(s.yes_token_ids(), vec!["yes0", "yes1", "yes2"]);
    assert_eq!(s.no_token_ids(), vec!["no0", "no1", "no2"]);
    assert_eq!(s.all_token_ids(), vec!["yes0", "no0", "yes1", "no1", "yes2", "no2"]);
    assert_eq!(s.member(1).unwrap().title, "B");
    assert!(s.member(9).is_none());
}

#[test]
fn sigma_invariant_over_a_complete_set() {
    let s = &NegRiskSet::group(&complete_set())[0];
    assert_eq!(s.completeness(), SetCompleteness::Complete);
    // 0.50 + 0.30 + 0.15 = 0.95 → a 0.05 gross buy-side edge
    let sum = s.yes_price_sum().expect("complete");
    assert!((sum - 0.95).abs() < 1e-12, "sum={sum}");
    let gross = s.arb_edge(0.0).expect("complete");
    assert!((gross - 0.05).abs() < 1e-12, "gross={gross}");
    // 2c per leg over 3 legs eats 6c > the 5c gross edge → net NEGATIVE, no arb.
    let net = s.arb_edge(0.02).expect("complete");
    assert!(net < 0.0, "net={net}");
}

#[test]
fn a_fairly_priced_set_sums_to_one_and_shows_no_edge() {
    let ms = vec![mk(0, "A", Some(0.60), ETH_MARKET_ID), mk(1, "B", Some(0.40), ETH_MARKET_ID)];
    let s = &NegRiskSet::group(&ms)[0];
    assert!((s.yes_price_sum().unwrap() - 1.0).abs() < 1e-12);
    assert!(s.arb_edge(0.0).unwrap().abs() < 1e-12);
}

#[test]
fn an_incomplete_set_refuses_to_report_an_edge() {
    // THE trap: dropping index 1 lowers Σ to 0.65, which *looks* like a 35c arb. It must not
    // be reported at all.
    let ms: Vec<GammaMarket> =
        complete_set().into_iter().filter(|m| m.group_item_index != Some(1)).collect();
    let s = &NegRiskSet::group(&ms)[0];
    assert_eq!(s.completeness(), SetCompleteness::IndexGap { held: 2, max_index: 2 });
    assert!(s.yes_price_sum().is_none(), "no Σ over a gapped set");
    assert!(s.arb_edge(0.0).is_none(), "no edge over a gapped set");
    // a TRUNCATED tail (0,1 held, 2 dropped) is contiguous-from-zero and therefore
    // indistinguishable from a genuine 2-member set — documented limitation, see below.
    let ms2: Vec<GammaMarket> =
        complete_set().into_iter().filter(|m| m.group_item_index != Some(2)).collect();
    assert_eq!(NegRiskSet::group(&ms2)[0].completeness(), SetCompleteness::Complete);
}

/// The REAL divergence that motivates the completeness gate, both halves measured live
/// (2026-07-22): the `/markets` volume browse returned indices 1..7 of
/// `next-prime-minister-of-ethiopia` and silently omitted index 0 — `"Abiy Ahmed"` at 0.958,
/// the whole probability mass. Σ over the stragglers is 0.039, i.e. a 96c phantom arb.
#[test]
fn the_live_partial_ethiopia_set_is_refused_not_reported_as_a_96c_arb() {
    // exactly what the volume browse returned: indices 1..7, index 0 absent
    let straggler_prices =
        [(1, 0.008), (2, 0.0025), (3, 0.0025), (4, 0.0025), (5, 0.0025), (6, 0.0025), (7, 0.0185)];
    let partial: Vec<GammaMarket> =
        straggler_prices.iter().map(|&(i, p)| mk(i, "x", Some(p), ETH_MARKET_ID)).collect();
    let s = &NegRiskSet::group(&partial)[0];
    // the naive sum a caller would have computed without this type:
    let naive: f64 = partial.iter().map(|m| m.yes_price().unwrap()).sum();
    assert!(naive < 0.04, "the phantom Σ really is ~0.039 (got {naive})");
    // ...and it is refused, because index 0 is missing.
    assert_eq!(s.completeness(), SetCompleteness::IndexGap { held: 7, max_index: 7 });
    assert!(s.yes_price_sum().is_none());
    assert!(s.arb_edge(0.0).is_none(), "no 96c arb is ever reported");
    // a missing MEMBER cannot be papered over by a price assumption either
    assert!(s.yes_price_sum_with_ceiling(0.01).is_none());

    // the complete /events fetch adds index 0 = 0.958 → Σ ≈ 0.997, no edge.
    let mut full = partial;
    full.push(mk(0, "Abiy Ahmed", Some(0.958), ETH_MARKET_ID));
    let s = &NegRiskSet::group(&full)[0];
    assert_eq!(s.completeness(), SetCompleteness::Complete);
    let sum = s.yes_price_sum().unwrap();
    assert!((sum - 0.997).abs() < 1e-9, "sum={sum}");
    assert!(s.arb_edge(0.0).unwrap() < 0.01, "no meaningful edge once complete");
}

#[test]
fn a_price_ceiling_can_only_understate_the_edge() {
    // 8 priced members summing to 0.997, plus 2 unpriced ones — the live shape.
    let mut ms = vec![mk(0, "Abiy Ahmed", Some(0.958), ETH_MARKET_ID)];
    for (i, p) in
        [(1, 0.008), (2, 0.0025), (3, 0.0025), (4, 0.0025), (5, 0.0025), (6, 0.0025), (7, 0.0185)]
    {
        ms.push(mk(i, "x", Some(p), ETH_MARKET_ID));
    }
    ms.push(mk(8, "Person C", None, ETH_MARKET_ID));
    ms.push(mk(9, "Person D", None, ETH_MARKET_ID));
    let s = &NegRiskSet::group(&ms)[0];
    // strict path refuses: two members carry no price
    assert_eq!(s.completeness(), SetCompleteness::MissingPrices { missing: 2 });
    assert!(s.yes_price_sum().is_none());
    // with a 1-tick ceiling the two unpriced legs are charged 0.01 each → Σ = 1.017
    let sum = s.yes_price_sum_with_ceiling(0.01).unwrap();
    assert!((sum - 1.017).abs() < 1e-9, "sum={sum}");
    // a HIGHER ceiling can only raise Σ, i.e. lower the edge — never invent one
    let looser = s.yes_price_sum_with_ceiling(0.05).unwrap();
    assert!(looser > sum, "a more conservative ceiling never shrinks Σ");
    // ceiling 0.0 is the optimistic reading, and is exactly the trap: it recovers 0.997
    assert!((s.yes_price_sum_with_ceiling(0.0).unwrap() - 0.997).abs() < 1e-9);
}

#[test]
fn a_member_without_a_price_blocks_sigma() {
    let ms = vec![
        mk(0, "A", Some(0.50), ETH_MARKET_ID),
        mk(1, "B", Some(0.30), ETH_MARKET_ID),
        mk(2, "C", None, ETH_MARKET_ID), // inactive member, Gamma quoted nothing
    ];
    let s = &NegRiskSet::group(&ms)[0];
    assert_eq!(s.completeness(), SetCompleteness::MissingPrices { missing: 1 });
    assert!(s.yes_price_sum().is_none());
    assert!(s.arb_edge(0.0).is_none());
}

#[test]
fn an_unindexed_member_is_never_complete() {
    let mut ms = complete_set();
    ms[0].group_item_index = None;
    let s = &NegRiskSet::group(&ms)[0];
    assert_eq!(s.completeness(), SetCompleteness::UnindexedMembers { without_index: 1 });
    assert!(s.arb_edge(0.0).is_none());
    // unindexed members sort last but are still enumerable
    assert_eq!(s.len(), 3);
    assert_eq!(s.members[2].index, None);
}

#[test]
fn empty_set_is_empty_not_complete() {
    let s = NegRiskSet {
        market_id: ETH_MARKET_ID.into(),
        event_id: String::new(),
        event_slug: String::new(),
        event_title: String::new(),
        members: vec![],
    };
    assert_eq!(s.completeness(), SetCompleteness::Empty);
    assert!(s.is_empty());
    assert!(s.arb_edge(0.0).is_none());
}

#[test]
fn question_id_encoding_holds_across_the_set() {
    let s = &NegRiskSet::group(&complete_set())[0];
    assert!(s.question_id_mismatches().is_empty());
    // the live-observed value: marketId's last byte replaced by the index
    assert_eq!(
        s.member(2).unwrap().question_id,
        "0x55ab76d092f682bf5cbb7e14f13ee12f8410ce7cc1b7906f23b8fb56c11f6502"
    );
    // corrupt one and the cross-check catches it
    let mut ms = complete_set();
    ms[0].question_id = "0xdeadbeef".repeat(8);
    let bad = &NegRiskSet::group(&ms)[0];
    assert_eq!(bad.question_id_mismatches().len(), 1);
}
