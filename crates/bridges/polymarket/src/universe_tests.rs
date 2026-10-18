use super::*;

/// Build a GammaMarket with the fields select_universe reads (others defaulted).
fn mk(cid: &str, liq: f64, vol: f64, toks: &[&str]) -> GammaMarket {
    GammaMarket {
        id: cid.into(),
        question: format!("q-{cid}"),
        condition_id: cid.into(),
        slug: cid.into(),
        end_date: "".into(),
        volume: vol,
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
fn ranks_by_liquidity_desc_and_takes_top_n() {
    let markets = vec![
        mk("a", 100.0, 0.0, &["a1"]),
        mk("b", 300.0, 0.0, &["b1"]),
        mk("c", 200.0, 0.0, &["c1"]),
    ];
    let policy = UniversePolicy { max_markets: 2, min_liquidity: 0.0, ..Default::default() };
    let sel = select_universe(&markets, &policy);
    assert_eq!(sel.len(), 2);
    assert_eq!(sel[0].condition_id, "b"); // 300
    assert_eq!(sel[1].condition_id, "c"); // 200
}

#[test]
fn rank_by_volume_switches_the_order() {
    let markets = vec![mk("a", 10.0, 900.0, &["a1"]), mk("b", 999.0, 5.0, &["b1"])];
    let policy = UniversePolicy {
        max_markets: 1,
        min_liquidity: 0.0,
        rank_by: RankBy::Volume,
        ..Default::default()
    };
    let sel = select_universe(&markets, &policy);
    assert_eq!(sel[0].condition_id, "a"); // higher volume wins under RankBy::Volume
}

#[test]
fn applies_liquidity_and_volume_floors() {
    let markets = vec![mk("keep", 5_000.0, 5_000.0, &["k"]), mk("thin", 10.0, 5_000.0, &["t"])];
    let policy = UniversePolicy {
        max_markets: 10,
        min_liquidity: 1_000.0,
        min_volume: 1_000.0,
        ..Default::default()
    };
    let sel = select_universe(&markets, &policy);
    assert_eq!(sel.len(), 1);
    assert_eq!(sel[0].condition_id, "keep");
}

#[test]
fn require_open_drops_closed_and_inactive() {
    let mut closed = mk("closed", 9_999.0, 0.0, &["c"]);
    closed.closed = true;
    let mut inactive = mk("inactive", 9_999.0, 0.0, &["i"]);
    inactive.active = false;
    let markets = vec![closed, inactive, mk("open", 2_000.0, 0.0, &["o"])];
    let policy = UniversePolicy { max_markets: 10, min_liquidity: 0.0, ..Default::default() };
    let sel = select_universe(&markets, &policy);
    assert_eq!(sel.len(), 1);
    assert_eq!(sel[0].condition_id, "open");
}

#[test]
fn markets_without_token_ids_are_dropped() {
    let markets = vec![mk("has", 5_000.0, 0.0, &["t"]), mk("none", 5_000.0, 0.0, &[])];
    let policy = UniversePolicy { max_markets: 10, min_liquidity: 0.0, ..Default::default() };
    let sel = select_universe(&markets, &policy);
    assert_eq!(sel.len(), 1);
    assert_eq!(sel[0].condition_id, "has");
}

#[test]
fn equal_rank_breaks_by_condition_id_for_stability() {
    // identical liquidity -> deterministic order by condition_id, so the diff never jitters.
    let markets = vec![
        mk("zebra", 100.0, 0.0, &["z"]),
        mk("alpha", 100.0, 0.0, &["a"]),
        mk("mid", 100.0, 0.0, &["m"]),
    ];
    let policy = UniversePolicy { max_markets: 3, min_liquidity: 0.0, ..Default::default() };
    let sel = select_universe(&markets, &policy);
    assert_eq!(
        sel.iter().map(|m| m.condition_id.as_str()).collect::<Vec<_>>(),
        vec!["alpha", "mid", "zebra"]
    );
}

#[test]
fn manager_first_refresh_adds_all_selected_tokens() {
    let mut mgr = UniverseManager::new(UniversePolicy {
        max_markets: 10,
        min_liquidity: 0.0,
        ..Default::default()
    });
    let markets = vec![mk("a", 2.0, 0.0, &["a1", "a2"]), mk("b", 1.0, 0.0, &["b1"])];
    let (sel, diff) = mgr.refresh(&markets);
    assert_eq!(sel.len(), 2);
    assert_eq!(diff.to_add, vec!["a1", "a2", "b1"]); // sorted set order
    assert!(diff.to_remove.is_empty());
    assert_eq!(mgr.subscribed().collect::<Vec<_>>(), vec!["a1", "a2", "b1"]);
}

#[test]
fn manager_rotation_adds_new_and_removes_dropped() {
    let mut mgr = UniverseManager::new(UniversePolicy {
        max_markets: 1,
        min_liquidity: 0.0,
        ..Default::default()
    });
    // Round 1: "a" is most liquid -> subscribe a1.
    let r1 = vec![mk("a", 100.0, 0.0, &["a1"]), mk("b", 10.0, 0.0, &["b1"])];
    let (_, d1) = mgr.refresh(&r1);
    assert_eq!(d1.to_add, vec!["a1"]);
    // Round 2: "b" overtakes -> unsubscribe a1, subscribe b1.
    let r2 = vec![mk("a", 10.0, 0.0, &["a1"]), mk("b", 100.0, 0.0, &["b1"])];
    let (_, d2) = mgr.refresh(&r2);
    assert_eq!(d2.to_add, vec!["b1"]);
    assert_eq!(d2.to_remove, vec!["a1"]);
    assert_eq!(mgr.subscribed().collect::<Vec<_>>(), vec!["b1"]);
}

#[test]
fn plan_union_merges_targets_and_does_not_churn() {
    let mut mgr = UniverseManager::default();
    let a: BTreeSet<String> = ["a1".to_string(), "shared".to_string()].into();
    let b: BTreeSet<String> = ["b1".to_string(), "shared".to_string()].into();

    let d = mgr.plan_union([&a, &b]);
    assert_eq!(d.to_add, vec!["a1", "b1", "shared"], "de-duplicated union, sorted");
    assert!(d.to_remove.is_empty());
    mgr.commit(&d);
    assert!(mgr.plan_union([&a, &b]).is_empty(), "stable union is a no-op");

    // dropping one source removes only that source's exclusive tokens; "shared" survives.
    let d2 = mgr.plan_union([&b]);
    assert_eq!(d2.to_remove, vec!["a1"]);
    assert!(d2.to_add.is_empty());

    // an empty target list unsubscribes everything (union of nothing = nothing).
    let empty: [&BTreeSet<String>; 0] = [];
    assert_eq!(mgr.plan_union(empty).to_remove, vec!["a1", "b1", "shared"]);
}

#[test]
fn manager_stable_universe_is_a_noop_diff() {
    let mut mgr = UniverseManager::new(UniversePolicy {
        max_markets: 5,
        min_liquidity: 0.0,
        ..Default::default()
    });
    let markets = vec![mk("a", 100.0, 0.0, &["a1"]), mk("b", 50.0, 0.0, &["b1"])];
    mgr.refresh(&markets);
    let (_, d2) = mgr.refresh(&markets); // same input again
    assert!(d2.is_empty(), "no churn when the selection is unchanged");
}
