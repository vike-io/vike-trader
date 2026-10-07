use super::*;

fn canned() -> serde_json::Value {
    serde_json::json!([
        {
            "id": "540817",
            "question": "New Rihanna Album before GTA VI?",
            "conditionId": "0x1fad72fae204143ff1c3035e99e7c0f65ea8d5cd9bd1070987bd1a3316f772be",
            "slug": "new-rhianna-album-before-gta-vi-926",
            "endDate": "2026-07-31T12:00:00Z",
            "volumeNum": 854399.71,
            "liquidityNum": 27192.11,
            "active": true,
            "closed": false,
            "orderPriceMinTickSize": 0.01,
            "outcomes": "[\"Yes\", \"No\"]",
            "outcomePrices": "[\"0.505\", \"0.495\"]",
            "clobTokenIds": "[\"111222\", \"333444\"]"
        },
        {
            "id": "999", "question": "grouped neg-risk market", "conditionId": "0xabc",
            "slug": "grp", "endDate": "2026-08-01T00:00:00Z", "volumeNum": 1.0, "liquidityNum": 1.0,
            "active": true, "closed": false, "orderPriceMinTickSize": 0.01,
            "groupItemThreshold": "2", "negRisk": true,
            "outcomes": "[\"A\", \"B\"]", "clobTokenIds": "[\"555\", \"666\"]"
        },
        { "id": "bad", "slug": "no-question-or-condition" },
        {
            "id": "empty-toks", "question": "no tokens", "conditionId": "0xdef", "slug": "e",
            "endDate": "x", "volumeNum": 0.0, "liquidityNum": 0.0, "active": true, "closed": false,
            "orderPriceMinTickSize": 0.01, "outcomes": "[]", "clobTokenIds": ""
        }
    ])
}

#[test]
fn gamma_slug_query_is_an_exact_lookup_not_a_volume_browse() {
    // no order/limit/offset: the by-slug route must not inherit the volume-DESC browse, which
    // is what buries a fresh ~$0-volume window below any crawl ceiling.
    let q = gamma_slug_query("bitcoin-up-or-down-2026-07-18-1205", true);
    assert_eq!(q, "active=true&closed=false&slug=bitcoin-up-or-down-2026-07-18-1205");
    assert!(!q.contains("order=") && !q.contains("limit="));
    assert_eq!(gamma_slug_query("abc", false), "slug=abc");
    // anything outside the unreserved set is percent-encoded rather than injected raw.
    assert_eq!(gamma_slug_query("a b&c=d", false), "slug=a%20b%26c%3Dd");
}

#[test]
fn gamma_query_filters_active_open_by_volume() {
    assert_eq!(
        gamma_query(true, 50, 0),
        "active=true&closed=false&limit=50&offset=0&order=volumeNum&ascending=false"
    );
    // active_only=false drops the active/closed filters (browse everything)
    assert_eq!(gamma_query(false, 20, 40), "limit=20&offset=40&order=volumeNum&ascending=false");
}

#[test]
fn decode_json_string_array_double_decodes() {
    // the load-bearing gotcha: a STRING containing a JSON array
    let v = serde_json::json!("[\"Yes\", \"No\"]");
    assert_eq!(decode_json_string_array(&v), vec!["Yes".to_string(), "No".to_string()]);
    // garbage / non-string → empty, no panic
    assert!(decode_json_string_array(&serde_json::json!("not json")).is_empty());
    assert!(decode_json_string_array(&serde_json::json!(42)).is_empty());
    assert!(decode_json_string_array(&serde_json::json!("")).is_empty());
}

#[test]
fn parse_reads_markets_and_decodes_token_ids() {
    let ms = parse_gamma_markets(&canned());
    // "bad" (no question/conditionId) is skipped → 3 markets
    assert_eq!(ms.len(), 3);
    let m = &ms[0];
    assert_eq!(m.question, "New Rihanna Album before GTA VI?");
    assert_eq!(
        m.condition_id,
        "0x1fad72fae204143ff1c3035e99e7c0f65ea8d5cd9bd1070987bd1a3316f772be"
    );
    assert_eq!(m.slug, "new-rhianna-album-before-gta-vi-926");
    assert_eq!(m.volume.to_bits(), 854399.71f64.to_bits());
    assert_eq!(m.tick_size.to_bits(), 0.01f64.to_bits());
    assert_eq!(m.outcomes, vec!["Yes".to_string(), "No".to_string()]);
    assert_eq!(m.token_ids, vec!["111222".to_string(), "333444".to_string()]); // decoded from clobTokenIds
    assert!(!m.neg_risk);
    // grouped neg-risk market
    assert!(ms[1].neg_risk);
    // empty clobTokenIds → empty token_ids, no panic
    assert!(ms[2].token_ids.is_empty());
}

/// VERBATIM live Gamma shape (2026-07-22, via arbdub) for two members of the 7-way
/// "Next Prime Minister of Ethiopia" neg-risk set — the fixture the group-identity parse pins
/// against. Field values are copied from the wire, not invented.
fn live_neg_risk_pair() -> serde_json::Value {
    serde_json::json!([
        {
            "id": "2063135",
            "question": "Will Gedion Timothewos be the next Prime Minister of Ethiopia?",
            "conditionId": "0xb6d6f15a1b5d08753653f1867ccd6126badfbe182a75159a330dc7b15336b309",
            "questionID": "0x55ab76d092f682bf5cbb7e14f13ee12f8410ce7cc1b7906f23b8fb56c11f6507",
            "slug": "will-gedion-timothewos-be-the-next-prime-minister-of-ethiopia",
            "endDate": "2026-12-31T12:00:00Z",
            "active": true, "closed": false,
            "negRisk": true, "negRiskOther": false,
            "negRiskMarketID": "0x55ab76d092f682bf5cbb7e14f13ee12f8410ce7cc1b7906f23b8fb56c11f6500",
            "negRiskRequestID": "0x1291bf079fc5e94bc2e72e790495aedb8e755c0b2a8fce3b60f01284c3f5ae67",
            "groupItemTitle": "Gedion Timothewos",
            "groupItemThreshold": "7",
            "orderPriceMinTickSize": 0.01,
            "outcomes": "[\"Yes\", \"No\"]",
            "outcomePrices": "[\"0.0185\", \"0.9815\"]",
            "clobTokenIds": "[\"111\", \"222\"]",
            "events": [{
                "id": "411239", "slug": "next-prime-minister-of-ethiopia",
                "title": "Next Prime Minister of Ethiopia", "ticker": "next-pm-ethiopia",
                "negRisk": true,
                "negRiskMarketID": "0x55ab76d092f682bf5cbb7e14f13ee12f8410ce7cc1b7906f23b8fb56c11f6500"
            }]
        },
        {
            "id": "2063136",
            "question": "Will Belete Molla be the next Prime Minister of Ethiopia?",
            "conditionId": "0x7c97f7315a000000000000000000000000000000000000000000000000000000",
            "questionID": "0x55ab76d092f682bf5cbb7e14f13ee12f8410ce7cc1b7906f23b8fb56c11f6501",
            "slug": "will-belete-molla-be-the-next-prime-minister-of-ethiopia",
            "endDate": "2026-12-31T12:00:00Z",
            "active": true, "closed": false,
            "negRisk": true,
            "negRiskMarketID": "0x55ab76d092f682bf5cbb7e14f13ee12f8410ce7cc1b7906f23b8fb56c11f6500",
            "negRiskRequestID": "0x8f41e93fb2ebd2b33e72d54940bee621cf7eac4ce795a2403256e4263cba0bf1",
            "groupItemTitle": "Belete Molla",
            "groupItemThreshold": "1",
            "orderPriceMinTickSize": 0.01,
            "outcomes": "[\"Yes\", \"No\"]",
            "outcomePrices": "[\"0.008\", \"0.992\"]",
            "clobTokenIds": "[\"333\", \"444\"]",
            "events": [{
                "id": "411239", "slug": "next-prime-minister-of-ethiopia",
                "title": "Next Prime Minister of Ethiopia", "negRisk": true,
                "negRiskMarketID": "0x55ab76d092f682bf5cbb7e14f13ee12f8410ce7cc1b7906f23b8fb56c11f6500"
            }]
        }
    ])
}

#[test]
fn parses_neg_risk_group_identity_from_the_live_shape() {
    let ms = parse_gamma_markets(&live_neg_risk_pair());
    assert_eq!(ms.len(), 2);
    let a = &ms[0];
    // THE group key — shared by both members
    assert_eq!(
        a.neg_risk_market_id,
        "0x55ab76d092f682bf5cbb7e14f13ee12f8410ce7cc1b7906f23b8fb56c11f6500"
    );
    assert_eq!(a.neg_risk_market_id, ms[1].neg_risk_market_id, "one group key");
    assert!(a.is_neg_risk_member());
    // ...while negRiskRequestID is PER-MARKET and would NOT group them
    assert_ne!(a.neg_risk_request_id, ms[1].neg_risk_request_id);
    assert!(a.neg_risk_request_id.starts_with("0x1291bf07"));
    // groupItemThreshold is the 0-based INDEX (a decimal STRING on the wire), and it is
    // exactly questionID's last byte
    assert_eq!(a.group_item_index, Some(7));
    assert_eq!(ms[1].group_item_index, Some(1));
    assert_eq!(a.question_id, "0x55ab76d092f682bf5cbb7e14f13ee12f8410ce7cc1b7906f23b8fb56c11f6507");
    assert_eq!(neg_risk_question_id(&a.neg_risk_market_id, 7).unwrap(), a.question_id);
    assert_eq!(neg_risk_question_id(&a.neg_risk_market_id, 1).unwrap(), ms[1].question_id);
    assert_eq!(a.group_item_title, "Gedion Timothewos");
    // parent event lifted off events[0]
    assert_eq!(a.event_id, "411239");
    assert_eq!(a.event_slug, "next-prime-minister-of-ethiopia");
    assert_eq!(a.event_title, "Next Prime Minister of Ethiopia");
    // outcomePrices double-decoded from decimal strings, positionally aligned
    assert_eq!(a.outcome_prices, vec![0.0185, 0.9815]);
    assert_eq!(a.yes_price(), Some(0.0185));
    assert_eq!(a.yes_token_id(), Some("111"));
    assert_eq!(a.no_token_id(), Some("222"));
    // and the pre-existing fields are unchanged
    assert_eq!(a.token_ids, vec!["111".to_string(), "222".to_string()]);
    assert_eq!(a.outcomes, vec!["Yes".to_string(), "No".to_string()]);
    assert!(a.neg_risk);
}

#[test]
fn a_non_neg_risk_market_parses_exactly_as_before() {
    // the FIRST canned entry has no negRiskMarketID / questionID / groupItem* / events —
    // every new field must come back empty, and nothing pre-existing may change.
    let m = &parse_gamma_markets(&canned())[0];
    assert!(m.neg_risk_market_id.is_empty());
    assert!(m.neg_risk_request_id.is_empty());
    assert!(m.question_id.is_empty());
    assert!(m.group_item_title.is_empty());
    assert_eq!(m.group_item_index, None);
    assert!(m.event_id.is_empty() && m.event_slug.is_empty() && m.event_title.is_empty());
    assert!(!m.is_neg_risk_member(), "no group key => not a set member");
    // outcomePrices IS present on that entry and is now decoded
    assert_eq!(m.outcome_prices, vec![0.505, 0.495]);
    // the legacy groupItemThreshold-presence heuristic still raises `neg_risk` (entry 2),
    // even though that entry has no group key to group on — which is exactly why
    // `is_neg_risk_member` exists as the stricter test.
    let g = &parse_gamma_markets(&canned())[1];
    assert!(g.neg_risk);
    assert!(!g.is_neg_risk_member());
    assert_eq!(g.group_item_index, Some(2), "\"2\" parsed as the index");
}

#[test]
fn parse_gamma_events_flattens_nested_markets_and_injects_event_identity() {
    // the /events shape: the event wraps markets[], and those nested markets carry NO `events`
    // key (live-verified) — so the parent's identity must be injected.
    let ev = serde_json::json!([{
        "id": "30829",
        "slug": "democratic-presidential-nominee-2028",
        "title": "Democratic Presidential Nominee 2028",
        "negRisk": true,
        "negRiskMarketID": "0x2c3d7e0eee6f058be3006baabf0d54a07da254ba47fe6e3e095e7990c7814700",
        "markets": [
            { "id": "1", "question": "Oprah Winfrey?", "conditionId": "0xe06a7e94cf",
              "questionID": "0x2c3d7e0eee6f058be3006baabf0d54a07da254ba47fe6e3e095e7990c7814700",
              "negRiskMarketID": "0x2c3d7e0eee6f058be3006baabf0d54a07da254ba47fe6e3e095e7990c7814700",
              "groupItemTitle": "Oprah Winfrey", "groupItemThreshold": "0", "active": true,
              "outcomes": "[\"Yes\",\"No\"]", "outcomePrices": "[\"0.0045\",\"0.9955\"]",
              "clobTokenIds": "[\"a\",\"b\"]" },
            // a nested market that omitted its own group key: the event's fills in
            { "id": "2", "question": "Bernie Sanders?", "conditionId": "0x30cfb88755",
              "groupItemTitle": "Bernie Sanders", "groupItemThreshold": "1", "active": true,
              "outcomes": "[\"Yes\",\"No\"]", "outcomePrices": "[\"0.0065\",\"0.9935\"]",
              "clobTokenIds": "[\"c\",\"d\"]" }
        ]
    }]);
    let ms = parse_gamma_events(&ev);
    assert_eq!(ms.len(), 2, "flattened out of the event");
    for m in &ms {
        assert_eq!(m.event_id, "30829");
        assert_eq!(m.event_slug, "democratic-presidential-nominee-2028");
        assert_eq!(m.event_title, "Democratic Presidential Nominee 2028");
        assert_eq!(
            m.neg_risk_market_id,
            "0x2c3d7e0eee6f058be3006baabf0d54a07da254ba47fe6e3e095e7990c7814700",
            "own key kept, missing key filled from the event"
        );
    }
    assert_eq!(ms[0].group_item_index, Some(0));
    assert_eq!(ms[1].group_item_index, Some(1));
    // an event with no markets contributes nothing; a non-array is empty, no panic
    assert!(parse_gamma_events(&serde_json::json!([{ "id": "x", "slug": "y" }])).is_empty());
    assert!(parse_gamma_events(&serde_json::json!({})).is_empty());
}

#[test]
fn neg_risk_question_id_replaces_the_last_byte_only() {
    const MID: &str = "0x55ab76d092f682bf5cbb7e14f13ee12f8410ce7cc1b7906f23b8fb56c11f6500";
    assert_eq!(
        neg_risk_question_id(MID, 0).unwrap(),
        "0x55ab76d092f682bf5cbb7e14f13ee12f8410ce7cc1b7906f23b8fb56c11f6500"
    );
    assert_eq!(
        neg_risk_question_id(MID, 127).unwrap(),
        "0x55ab76d092f682bf5cbb7e14f13ee12f8410ce7cc1b7906f23b8fb56c11f657f"
    );
    assert_eq!(
        neg_risk_question_id(MID, 255).unwrap(),
        "0x55ab76d092f682bf5cbb7e14f13ee12f8410ce7cc1b7906f23b8fb56c11f65ff"
    );
    // works without the 0x prefix, and always emits one
    assert_eq!(neg_risk_question_id(&MID[2..], 1).unwrap(), neg_risk_question_id(MID, 1).unwrap());
    // refusals: an index that does not fit one byte, and a malformed key
    assert_eq!(neg_risk_question_id(MID, 256), None);
    assert_eq!(neg_risk_question_id("0xdead", 1), None);
    assert_eq!(neg_risk_question_id("", 1), None);
    assert_eq!(neg_risk_question_id(&"z".repeat(64), 1), None);
}

#[test]
fn decode_json_string_f64_array_double_decodes_decimal_strings() {
    let v = serde_json::json!("[\"0.0185\", \"0.9815\"]");
    assert_eq!(decode_json_string_f64_array(&v), vec![0.0185, 0.9815]);
    // an unparseable element becomes 0.0 rather than dropping the array (alignment matters)
    assert_eq!(decode_json_string_f64_array(&serde_json::json!("[\"x\",\"1\"]")), vec![0.0, 1.0]);
    assert!(decode_json_string_f64_array(&serde_json::json!("")).is_empty());
    assert!(decode_json_string_f64_array(&serde_json::json!(7)).is_empty());
}

#[test]
fn gamma_event_slug_query_is_an_exact_event_lookup() {
    assert_eq!(
        gamma_event_slug_query("next-prime-minister-of-ethiopia", true),
        "active=true&closed=false&slug=next-prime-minister-of-ethiopia"
    );
    assert_eq!(gamma_event_slug_query("abc", false), "slug=abc");
    assert!(!gamma_event_slug_query("abc", true).contains("order="));
}

#[test]
fn resolution_ts_ms_parses_rfc3339_utc() {
    let mk = |end: &str| GammaMarket {
        id: "1".into(),
        question: "q".into(),
        condition_id: "0x1".into(),
        slug: "s".into(),
        end_date: end.into(),
        volume: 0.0,
        liquidity: 0.0,
        active: true,
        closed: false,
        neg_risk: false,
        tick_size: 0.01,
        outcomes: vec![],
        token_ids: vec![],
        ..Default::default()
    };
    // epoch anchors (cross-checked against `date -u -d ... +%s`)
    assert_eq!(mk("1970-01-01T00:00:00Z").resolution_ts_ms(), Some(0));
    assert_eq!(mk("2000-01-01T00:00:00Z").resolution_ts_ms(), Some(946_684_800_000));
    // the canned catalog value in this file
    assert_eq!(mk("2026-07-31T12:00:00Z").resolution_ts_ms(), Some(1_785_499_200_000));
    // tolerant of a fractional-second suffix + a space separator (both treated as UTC)
    assert_eq!(mk("2026-07-31T12:00:00.500Z").resolution_ts_ms(), Some(1_785_499_200_000));
    assert_eq!(mk("2026-07-31 12:00:00Z").resolution_ts_ms(), Some(1_785_499_200_000));
    // absent / malformed → None (never panics)
    assert_eq!(mk("").resolution_ts_ms(), None);
    assert_eq!(mk("x").resolution_ts_ms(), None);
    assert_eq!(mk("2026-13-01T00:00:00Z").resolution_ts_ms(), None); // month 13
}

#[test]
fn gamma_markets_without_reward_fields_have_default_rewards() {
    // the canned catalog markets carry no reward fields → the additive field is inert/default,
    // so nothing that parsed before this field existed changes.
    for m in parse_gamma_markets(&canned()) {
        assert_eq!(m.rewards, crate::rewards::RewardsConfig::default());
        assert!(!m.rewards.earns_rewards());
    }
}

#[test]
fn gamma_market_with_reward_fields_parses_min_size_and_max_spread() {
    let json = serde_json::json!([{
        "id": "1", "question": "rewarded?", "conditionId": "0xr", "slug": "r",
        "endDate": "2026-08-01T00:00:00Z", "active": true, "closed": false,
        "orderPriceMinTickSize": 0.01,
        "outcomes": "[\"Yes\",\"No\"]", "clobTokenIds": "[\"1\",\"2\"]",
        "rewardsMinSize": 50.0, "rewardsMaxSpread": 3.5
    }]);
    let m = &parse_gamma_markets(&json)[0];
    assert_eq!(m.rewards.min_size, 50.0);
    assert_eq!(m.rewards.max_spread, 3.5); // cents, verbatim
    assert!(m.rewards.earns_rewards());
    // Gamma carries neither of these — they stay at the zero default until enriched off the CLOB.
    assert_eq!(m.rewards.daily_rate, 0.0);
    assert_eq!(m.rewards.min_order_age_secs, 0);
}
