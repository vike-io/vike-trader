use super::*;
use crate::time::MS_PER_DAY;
use std::assert_matches;

#[test]
fn tif_defaults_gtc_and_serializes_screaming() {
    assert_eq!(TimeInForce::default(), TimeInForce::Gtc);
    assert_eq!(serde_json::to_string(&TimeInForce::Gtc).unwrap(), "\"GTC\"");
    assert_eq!(serde_json::to_string(&TimeInForce::Ioc).unwrap(), "\"IOC\"");
    assert_eq!(serde_json::to_string(&TimeInForce::Day).unwrap(), "\"DAY\"");
}

/// `TimeInForce::ALL` is a hand list, and this is what keeps it whole: `idx` has one arm per
/// variant and no wildcard, so a new variant stops this file compiling until it gets an arm, and
/// then `ARMS` (the arm count) and `ALL` must grow with it.
#[test]
fn time_in_force_all_is_every_variant_in_declaration_order() {
    const ARMS: usize = 6;
    fn idx(k: TimeInForce) -> usize {
        match k {
            TimeInForce::Gtc => 0,
            TimeInForce::Ioc => 1,
            TimeInForce::Fok => 2,
            TimeInForce::Gtd => 3,
            TimeInForce::Day => 4,
            TimeInForce::PostOnly => 5,
        }
    }
    assert_eq!(TimeInForce::ALL.len(), ARMS, "ALL and the arms of `idx` disagree");
    for (i, &k) in TimeInForce::ALL.iter().enumerate() {
        // `idx(ALL[i]) == i` gives `ALL[idx(k)] == k` AND refuses a duplicate standing in for
        // a missing variant; the discriminant pins declaration order without trusting `idx`.
        assert_eq!(idx(k), i, "{k:?} is not at its own index of ALL");
        assert_eq!(TimeInForce::ALL[idx(k)], k);
        assert_eq!(k as usize, i, "{k:?}: ALL is not in declaration order");
    }
}

/// Post-only is a VARIANT, not a field: the request keeps its size, a default request still
/// serializes `GTC`, and the new lifetime is the one journal/wire spelling `POST_ONLY` (the
/// journal reads an older record, which never names it, as `GTC`).
#[test]
fn post_only_is_a_variant_that_adds_no_bytes_to_the_request() {
    assert_eq!(std::mem::size_of::<TimeInForce>(), 1, "a fieldless enum is one byte");
    assert_eq!(serde_json::to_string(&TimeInForce::PostOnly).unwrap(), "\"POST_ONLY\"");
    assert_eq!(
        serde_json::from_str::<TimeInForce>("\"POST_ONLY\"").unwrap(),
        TimeInForce::PostOnly
    );
    let plain = serde_json::to_string(&OrderRequest::default()).unwrap();
    assert!(plain.contains("\"time_in_force\":\"GTC\""), "the default is unchanged: {plain}");
    let post_only = OrderRequest { time_in_force: TimeInForce::PostOnly, ..Default::default() };
    let back: OrderRequest =
        serde_json::from_str(&serde_json::to_string(&post_only).unwrap()).unwrap();
    assert_eq!(back.time_in_force, TimeInForce::PostOnly);
    // an older record, written before the variant existed, still reads back as GTC
    let old = plain.replace("\"GTC\"", "\"IOC\"");
    let old: OrderRequest = serde_json::from_str(&old).unwrap();
    assert_eq!(old.time_in_force, TimeInForce::Ioc);
}

/// The operator door: the five lifetimes plus `post-only`, any case, `post_only` too; anything
/// else is refused naming the accepted words (never the default `Gtc`).
#[test]
fn the_time_in_force_door_reads_the_six_words_and_refuses_the_rest() {
    for &t in TimeInForce::ALL {
        assert_eq!(TimeInForce::parse(t.as_word()), Ok(t), "{t:?}");
        assert_eq!(TimeInForce::parse(&t.as_word().to_uppercase()), Ok(t), "{t:?} upper");
    }
    assert_eq!(TimeInForce::parse("  Post_Only "), Ok(TimeInForce::PostOnly));
    assert_eq!(TimeInForce::parse("gtc"), Ok(TimeInForce::Gtc));
    assert_eq!(TimeInForce::parse("day "), Ok(TimeInForce::Day), "trimmed");
    for bad in ["", "good-till-cancel", "postonly", "gtx"] {
        let e = TimeInForce::parse(bad).expect_err(bad);
        let text = e.to_string();
        assert!(text.contains("gtc, ioc, fok, gtd, day, post-only"), "{text}");
    }
}

/// The lifetime rule both doors apply: a non-default lifetime is for a limit order, `gtd` needs a
/// future expiry, and nothing else takes one. The default `gtc` is never refused, whatever the kind.
#[test]
fn the_lifetime_rule_refuses_what_an_adapter_would_drop() {
    let now = 1_000_000;
    let limit = OrderType::Limit;
    // the default is everywhere legal
    for ot in [OrderType::Market, OrderType::Limit, OrderType::Stop] {
        assert_eq!(lifetime_refusal(&ot, TimeInForce::Gtc, None, now), None, "{ot}");
    }
    // every lifetime is legal on a limit order (gtd with a future expiry)
    for tif in [TimeInForce::Ioc, TimeInForce::Fok, TimeInForce::Day, TimeInForce::PostOnly] {
        assert_eq!(lifetime_refusal(&limit, tif, None, now), None, "{tif:?}");
    }
    assert_eq!(lifetime_refusal(&limit, TimeInForce::Gtd, Some(now + 1), now), None);
    // ...and on nothing else: the word would be dropped and the order would run without it
    for ot in [OrderType::Market, OrderType::Stop, OrderType::StopLimit] {
        let why = lifetime_refusal(&ot, TimeInForce::PostOnly, None, now).expect("refused");
        assert!(why.contains("post-only") && why.contains("limit order"), "{ot}: {why}");
    }
    // gtd needs an expiry, in the future; nothing else takes one
    assert!(
        lifetime_refusal(&limit, TimeInForce::Gtd, None, now).unwrap().contains("needs an expiry")
    );
    assert!(
        lifetime_refusal(&limit, TimeInForce::Gtd, Some(now), now)
            .unwrap()
            .contains("not in the future")
    );
    assert!(
        lifetime_refusal(&limit, TimeInForce::Day, Some(now + 5), now)
            .unwrap()
            .contains("belongs to time in force `gtd`")
    );
}

#[test]
fn gtc_ioc_fok_never_expire() {
    // deadline/anchor deliberately in the deep past — none of these ever expires. A post-only
    // order rests like GTC: it is judged once, at arrival.
    for tif in [TimeInForce::Gtc, TimeInForce::Ioc, TimeInForce::Fok, TimeInForce::PostOnly] {
        assert!(
            !tif_expired(tif, Some(1), 0, 10 * MS_PER_DAY),
            "{tif:?} must never expire by this predicate"
        );
    }
}

#[test]
fn gtd_expires_inclusively_at_the_deadline() {
    // before, at, after — inclusive boundary at `now == deadline`.
    assert!(!tif_expired(TimeInForce::Gtd, Some(2_000), 0, 1_999), "before the deadline");
    assert!(tif_expired(TimeInForce::Gtd, Some(2_000), 0, 2_000), "AT the deadline (inclusive)");
    assert!(tif_expired(TimeInForce::Gtd, Some(2_000), 0, 2_001), "after the deadline");
}

#[test]
fn gtd_without_a_deadline_never_expires() {
    assert!(!tif_expired(TimeInForce::Gtd, None, 0, i64::MAX / 2));
}

#[test]
fn day_expires_when_now_crosses_the_utc_day_boundary_from_its_anchor() {
    let anchor = 12 * 3_600_000; // mid-day on UTC day 0
    assert!(!tif_expired(TimeInForce::Day, None, anchor, anchor), "the anchor moment is alive");
    assert!(
        !tif_expired(TimeInForce::Day, None, anchor, MS_PER_DAY - 1),
        "still alive through the end of the anchor's UTC day"
    );
    assert!(
        tif_expired(TimeInForce::Day, None, anchor, MS_PER_DAY),
        "expired on the first ms of the next UTC day"
    );
}

#[test]
fn day_survives_its_whole_anchor_day_at_a_modern_clock() {
    // guards the seconds-vs-ms / born-expired class: a modern anchor lives out its own day.
    let today = 20_650 * MS_PER_DAY;
    assert!(!tif_expired(TimeInForce::Day, None, today, today), "born on its day, not expired");
    assert!(
        !tif_expired(TimeInForce::Day, None, today, today + MS_PER_DAY - 1),
        "alive to the last ms of the anchor day"
    );
    assert!(
        tif_expired(TimeInForce::Day, None, today, today + MS_PER_DAY),
        "expired the next UTC day"
    );
}

#[test]
fn day_boundary_holds_across_a_leap_day() {
    // 2024-02-29 is UTC day 19782; 2024-03-01 is 19783 — a Day order anchored on the leap day
    // survives it and expires the next calendar day, exactly like any other pair of days.
    let leap = 19_782 * MS_PER_DAY;
    assert_eq!(utc_day(leap + 23 * 3_600_000), 19_782, "23:00 on the leap day is the same day");
    assert!(!tif_expired(TimeInForce::Day, None, leap, leap + 23 * 3_600_000));
    assert!(tif_expired(TimeInForce::Day, None, leap, leap + MS_PER_DAY), "the day after");
}

#[test]
fn order_request_tif_is_additive() {
    // a serialized request WITHOUT time_in_force deserializes as GTC (back-compat)
    let r: OrderRequest = serde_json::from_str(
            r#"{"client_order_id":"c","venue":"v","symbol":"s","side":1,"qty":1.0,"order_type":"market"}"#,
        )
        .unwrap();
    assert_eq!(r.time_in_force, TimeInForce::Gtc);
    assert_eq!(r.gtd_expiry, None);
}

#[test]
fn default_literal_matches_serde() {
    // Piece-1 refactor guard: the live runtime builds OrderRequest via `..Default::default()`
    // instead of a `serde_json` round-trip. This pins that the two produce byte-identical
    // values — i.e. every field's Rust `Default` equals its `#[serde(default)]`. If a future
    // field's two defaults diverge, this fails before the hot-path construction can drift.
    let via_literal = OrderRequest {
        client_order_id: "c".into(),
        venue: "v".into(),
        symbol: "s".into(),
        side: 1,
        qty: 2.0,
        order_type: OrderType::Limit,
        price: Some(3.0),
        reduce_only: true,
        ts: 42,
        ..Default::default()
    };
    let via_serde: OrderRequest = serde_json::from_value(serde_json::json!({
        "client_order_id": "c", "venue": "v", "symbol": "s",
        "side": 1, "qty": 2.0, "order_type": "limit", "price": 3.0,
        "reduce_only": true, "ts": 42,
    }))
    .unwrap();
    assert_eq!(via_literal, via_serde);
    // `order_type`'s Rust default is `Market` (it was the empty string while the field was a
    // `String`); serde has NO default for it — a payload without the field is refused, as before.
    assert_eq!(OrderRequest::default().order_type, OrderType::Market);
    let missing = serde_json::from_value::<OrderRequest>(serde_json::json!({
        "client_order_id": "c", "venue": "v", "symbol": "s", "side": 1, "qty": 2.0,
    }));
    assert!(missing.is_err(), "order_type stays a required field");
}

#[test]
fn build_bracket_wires_oto_oco_linkage() {
    let spec = BracketSpec {
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        side: 1, // long entry
        qty: 3.0,
        entry_price: Some(100.0), // limit entry
        stop_loss: 95.0,
        take_profit: 110.0,
        account: None,
    };
    let [entry, sl, tp] = build_bracket(&spec, "E", "S", "T");

    // shared order-list id = the entry coid; the three orders all carry it
    assert_eq!(entry.order_list_id.as_deref(), Some("E"));
    assert_eq!(sl.order_list_id.as_deref(), Some("E"));
    assert_eq!(tp.order_list_id.as_deref(), Some("E"));

    // entry: OTO parent, limit, long, links to both exits
    assert_eq!(entry.contingency_type.as_deref(), Some("OTO"));
    assert_eq!(entry.order_type, OrderType::Limit);
    assert_eq!(entry.side, 1);
    assert_eq!(entry.price, Some(100.0));
    assert_eq!(entry.parent_order_id, None);
    assert_eq!(entry.linked_order_ids, vec!["S".to_string(), "T".to_string()]);

    // stop-loss: OCO child of entry, opposite side, reduce-only, stop @ trigger, links to TP
    assert_eq!(sl.contingency_type.as_deref(), Some("OCO"));
    assert_eq!(sl.parent_order_id.as_deref(), Some("E"));
    assert_eq!(sl.side, -1);
    assert!(sl.reduce_only);
    assert_eq!(sl.order_type, OrderType::Stop);
    assert_eq!(sl.trigger_price, Some(95.0));
    assert_eq!(sl.linked_order_ids, vec!["T".to_string()]);

    // take-profit: OCO child of entry, opposite side, reduce-only, limit @ tp, links to SL
    assert_eq!(tp.contingency_type.as_deref(), Some("OCO"));
    assert_eq!(tp.parent_order_id.as_deref(), Some("E"));
    assert_eq!(tp.side, -1);
    assert!(tp.reduce_only);
    assert_eq!(tp.order_type, OrderType::Limit);
    assert_eq!(tp.price, Some(110.0));
    assert_eq!(tp.linked_order_ids, vec!["S".to_string()]);
    // a spec that names no account builds three legs that name none
    assert!([&entry, &sl, &tp].iter().all(|o| o.account.is_none()));
}

/// A bracket NAMES its account, and all three legs carry it: the entry is routed by it and the
/// held exits are released onto the engine it resolved to. `Some(Default)` is carried too — it is a
/// NAME, and routes differently from `None` on a venue that runs two engines.
#[test]
fn build_bracket_carries_the_account_onto_all_three_legs() {
    use crate::accounts::account_keys::AccountLabel;
    for account in [AccountLabel::parse("PERP").unwrap(), AccountLabel::Default] {
        let spec = BracketSpec {
            venue: "binance".into(),
            symbol: "BTCUSDT.P".into(),
            side: 1,
            qty: 1.0,
            entry_price: None,
            stop_loss: 95.0,
            take_profit: 110.0,
            account: Some(account.clone()),
        };
        for leg in build_bracket(&spec, "E", "S", "T") {
            assert_eq!(leg.account, Some(account.clone()), "{}", leg.client_order_id);
        }
    }
}

/// The spec is journaled and sent between crates as JSON: an account-less spec is BYTE-IDENTICAL to the
/// pre-field one (no `account` key), a record written before the field reads back as `None`, and a
/// named spec round-trips, the unlabelled account included (its own `AccountLabel` serde refuses it,
/// which is why the field goes through `wire_account_option`).
#[test]
fn a_bracket_spec_serialises_its_account_only_when_named_and_reads_an_old_record() {
    use crate::accounts::account_keys::AccountLabel;
    let spec = |account| BracketSpec {
        venue: "binance".into(),
        symbol: "BTCUSDT.P".into(),
        side: 1,
        qty: 1.0,
        entry_price: Some(100.0),
        stop_loss: 95.0,
        take_profit: 110.0,
        account,
    };
    let bare = serde_json::to_string(&spec(None)).unwrap();
    assert!(!bare.contains("account"), "{bare}");
    let old = r#"{"venue":"binance","symbol":"BTCUSDT.P","side":1,"qty":1.0,
                  "entry_price":100.0,"stop_loss":95.0,"take_profit":110.0}"#;
    assert_eq!(serde_json::from_str::<BracketSpec>(old).unwrap(), spec(None));
    for account in [AccountLabel::parse("PERP").unwrap(), AccountLabel::Default] {
        let named = spec(Some(account));
        let json = serde_json::to_string(&named).expect("a named spec serialises");
        assert_eq!(serde_json::from_str::<BracketSpec>(&json).unwrap(), named, "{json}");
    }
}

// ---- combo orders (PR-1: vocabulary + invariants) ----

fn call_spread() -> ComboSpec {
    ComboSpec {
        venue: "deribit".into(),
        side: 1,
        qty: 2.0,
        legs: vec![
            ComboLeg { symbol: "BTC-27MAR26-100000-C".into(), ratio: 1 },
            ComboLeg { symbol: "BTC-27MAR26-120000-C".into(), ratio: -1 },
        ],
        net_limit: Some(0.015),
        time_in_force: TimeInForce::Gtc,
    }
}

#[test]
fn order_request_without_combo_legs_is_byte_identical() {
    // THE compatibility pin: an ordinary (non-combo) request must serialize EXACTLY as it did
    // before `combo_legs` existed — `skip_serializing_if = "Vec::is_empty"` means the key is
    // absent from the JSON entirely, so every existing fixture / journal record / wire body
    // round-trips unchanged. If this string ever needs editing, a persisted schema broke.
    let req = OrderRequest {
        client_order_id: "c".into(),
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        side: 1,
        qty: 2.0,
        order_type: OrderType::Limit,
        price: Some(3.0),
        ts: 42,
        ..Default::default()
    };
    let js = serde_json::to_string(&req).unwrap();
    assert_eq!(
        js,
        r#"{"client_order_id":"c","venue":"sim","symbol":"BTCUSDT","side":1,"qty":2.0,"order_type":"limit","price":3.0,"trigger_price":null,"reduce_only":false,"time_in_force":"GTC","gtd_expiry":null,"ts":42,"parent_order_id":null,"linked_order_ids":[],"order_list_id":null,"contingency_type":null,"weight":0.0,"stop":null,"trail":null,"extreme":null,"on_close":false}"#
    );
    assert!(!js.contains("combo_legs"));
    // and an OLD payload (no combo_legs key at all) still deserializes
    let back: OrderRequest = serde_json::from_str(&js).unwrap();
    assert_eq!(back, req);
    assert!(back.combo_legs.is_empty());
}

#[test]
fn order_request_without_margin_mode_is_byte_identical() {
    // The margin-mode compatibility pin (twin of the combo_legs pin above): an ordinary
    // request with no requested mode serializes EXACTLY as before the field existed — the
    // `margin_mode` key is absent entirely, so every fixture / journal record / wire body
    // (and the journal `state_hash` fence) round-trips unchanged.
    let req = OrderRequest {
        client_order_id: "c".into(),
        venue: "okx".into(),
        symbol: "BTC-USDT-SWAP".into(),
        side: 1,
        qty: 2.0,
        order_type: OrderType::Limit,
        price: Some(3.0),
        ts: 42,
        ..Default::default()
    };
    let js = serde_json::to_string(&req).unwrap();
    assert!(!js.contains("margin_mode"), "{js}");
    // an OLD payload (no margin_mode key at all) still deserializes as None
    let back: OrderRequest = serde_json::from_str(&js).unwrap();
    assert_eq!(back, req);
    assert_eq!(back.margin_mode, None);
    // and a requested mode round-trips
    let mut iso = req;
    iso.margin_mode = Some(crate::MarginMode::Isolated);
    let ijs = serde_json::to_string(&iso).unwrap();
    assert!(ijs.contains(r#""margin_mode":"Isolated""#), "{ijs}");
    assert_eq!(serde_json::from_str::<OrderRequest>(&ijs).unwrap(), iso);
}

#[test]
fn order_request_without_trigger_by_is_byte_identical() {
    // The trigger-source compatibility pin (twin of the margin_mode pin above): a request
    // with no requested source serializes EXACTLY as before the field existed — the
    // `trigger_by` key is absent entirely, so every fixture / journal record / wire body
    // (and the journal `state_hash` fence) round-trips unchanged.
    let req = OrderRequest {
        client_order_id: "c".into(),
        venue: "bybit".into(),
        symbol: "BTCUSDT".into(),
        side: -1,
        qty: 2.0,
        order_type: OrderType::Stop,
        trigger_price: Some(95.0),
        ts: 42,
        ..Default::default()
    };
    let js = serde_json::to_string(&req).unwrap();
    assert!(!js.contains("trigger_by"), "{js}");
    // an OLD payload (no trigger_by key at all) still deserializes as None
    let back: OrderRequest = serde_json::from_str(&js).unwrap();
    assert_eq!(back, req);
    assert_eq!(back.trigger_by, None);
    // and a requested source round-trips
    let mut mark = req;
    mark.trigger_by = Some(TriggerBy::Mark);
    let mjs = serde_json::to_string(&mark).unwrap();
    assert!(mjs.contains(r#""trigger_by":"Mark""#), "{mjs}");
    assert_eq!(serde_json::from_str::<OrderRequest>(&mjs).unwrap(), mark);
}

#[test]
fn order_request_without_data_delay_is_byte_identical() {
    // The delay-label pin (decision 0140; twin of the trigger_by pin above): an unlabelled
    // request serializes to EXACTLY the bytes it had before the field existed — pinned against a
    // literal, not only "the key is absent" — so every journal record and the `state_hash` fence
    // are unchanged for an order no labelling feed priced.
    let req = OrderRequest {
        client_order_id: "c".into(),
        venue: "ibkr".into(),
        symbol: "AAPL.SMART.USD".into(),
        side: 1,
        qty: 1.0,
        order_type: OrderType::Limit,
        price: Some(190.0),
        ts: 42,
        ..Default::default()
    };
    let js = serde_json::to_string(&req).unwrap();
    assert_eq!(
        js,
        r#"{"client_order_id":"c","venue":"ibkr","symbol":"AAPL.SMART.USD","side":1,"qty":1.0,"order_type":"limit","price":190.0,"trigger_price":null,"reduce_only":false,"time_in_force":"GTC","gtd_expiry":null,"ts":42,"parent_order_id":null,"linked_order_ids":[],"order_list_id":null,"contingency_type":null,"weight":0.0,"stop":null,"trail":null,"extreme":null,"on_close":false}"#
    );
    let back: OrderRequest = serde_json::from_str(&js).unwrap();
    assert_eq!(back.data_delay, None, "an old payload reads back unlabelled");
    // a labelled request carries the label and round-trips it
    let mut delayed = req;
    delayed.data_delay = Some(crate::DataDelay::DELAYED);
    let djs = serde_json::to_string(&delayed).unwrap();
    assert!(djs.ends_with(r#","data_delay":{"Delayed":{"lag_ms":null}}}"#), "{djs}");
    assert_eq!(serde_json::from_str::<OrderRequest>(&djs).unwrap(), delayed);
}

/// ⚠ An absent account must serialize to NOTHING. A single-account deployment's journal is
/// byte-identical before and after this field exists, which is what lets every stage of this
/// change stop safely. Asserted rather than reasoned about.
#[test]
fn an_account_less_order_serializes_exactly_as_it_did_before() {
    let req =
        OrderRequest { venue: "binance".into(), symbol: "BTCUSDT".into(), ..Default::default() };
    let json = serde_json::to_string(&req).expect("serialize");
    assert!(!json.contains("account"), "an absent account must not appear on the wire: {json}");
}

/// The other direction: a journal written before this field existed still deserializes.
#[test]
fn an_old_payload_with_no_account_still_deserializes() {
    let old = r#"{"client_order_id":"c1","venue":"binance","symbol":"BTCUSDT","side":1,"qty":1.0,"order_type":"market"}"#;
    let req: OrderRequest = serde_json::from_str(old).expect("an old payload must still parse");
    assert!(req.account.is_none());
}

#[test]
fn combo_request_serde_roundtrips() {
    let req = build_combo(&call_spread(), "K").unwrap();
    let js = serde_json::to_string(&req).unwrap();
    assert!(js.contains("combo_legs"), "{js}");
    let back: OrderRequest = serde_json::from_str(&js).unwrap();
    assert_eq!(back, req);

    let spec = call_spread();
    let sjs = serde_json::to_string(&spec).unwrap();
    assert_eq!(serde_json::from_str::<ComboSpec>(&sjs).unwrap(), spec);
}

#[test]
fn build_combo_carries_signed_net_limit_and_legs() {
    let req = build_combo(&call_spread(), "K").unwrap();
    assert_eq!(req.client_order_id, "K");
    assert_eq!(req.venue, "deribit");
    assert_eq!(req.symbol, "", "combo instrument is resolved by the adapter at submit");
    assert_eq!(req.side, 1);
    assert_eq!(req.qty, 2.0);
    assert_eq!(req.order_type, OrderType::Limit);
    assert_eq!(req.price, Some(0.015));
    assert_eq!(req.combo_legs.len(), 2);
    assert_eq!(req.combo_legs[1].ratio, -1);

    // market combo: no net limit
    let mut spec = call_spread();
    spec.net_limit = None;
    let m = build_combo(&spec, "K2").unwrap();
    assert_eq!(m.order_type, OrderType::Market);
    assert_eq!(m.price, None);

    // CREDIT combo: the negative net limit passes through verbatim — no clamp, no abs()
    let mut credit = call_spread();
    credit.side = -1;
    credit.net_limit = Some(-0.0125);
    let c = build_combo(&credit, "K3").unwrap();
    assert_eq!(c.price, Some(-0.0125));
    assert_eq!(c.side, -1);
}

#[test]
fn combo_net_is_signed_sum_of_ratio_times_price() {
    // debit call spread: +1 @ 50, -1 @ 30  =>  net +20 (a DEBIT, positive)
    assert_eq!(combo_net(&[(1, 50.0), (-1, 30.0)]), 20.0);
    // credit put spread: -1 @ 50, +1 @ 30  =>  net -20 (a CREDIT, negative)
    assert_eq!(combo_net(&[(-1, 50.0), (1, 30.0)]), -20.0);
    // ratios scale
    assert_eq!(combo_net(&[(2, 10.0), (-1, 5.0)]), 15.0);
    // a balanced spread nets exactly zero — legal
    assert_eq!(combo_net(&[(1, 5.0), (-1, 5.0)]), 0.0);
    assert_eq!(combo_net(&[]), 0.0);
}

#[test]
fn combo_cross_buy_fills_at_or_below_limit() {
    let legs = [(1, 50.0), (-1, 30.0)]; // net = +20
    assert_eq!(combo_net_cross(1, 20.0, &legs), Some(20.0)); // exactly at the limit
    assert_eq!(combo_net_cross(1, 25.0, &legs), Some(20.0)); // better than the limit
    assert_eq!(combo_net_cross(1, 19.0, &legs), None); // too expensive
}

#[test]
fn combo_cross_sell_fills_at_or_above_limit() {
    let legs = [(1, 50.0), (-1, 30.0)]; // net = +20
    assert_eq!(combo_net_cross(-1, 20.0, &legs), Some(20.0)); // exactly at the limit
    assert_eq!(combo_net_cross(-1, 15.0, &legs), Some(20.0)); // better than the limit
    assert_eq!(combo_net_cross(-1, 25.0, &legs), None); // not enough credit
}

#[test]
fn combo_cross_handles_credit_negative_nets() {
    // A CREDIT structure: net is NEGATIVE on both sides of the law — nothing clamps to >= 0.
    let legs = [(-1, 50.0), (1, 30.0)]; // net = -20
    assert!(combo_net(&legs) < 0.0);
    // buying a credit combo at a -10 limit: -20 <= -10 => fills
    assert_eq!(combo_net_cross(1, -10.0, &legs), Some(-20.0));
    // ...but not at -30 (we'd need net <= -30)
    assert_eq!(combo_net_cross(1, -30.0, &legs), None);
    // selling it: net >= limit
    assert_eq!(combo_net_cross(-1, -30.0, &legs), Some(-20.0));
    assert_eq!(combo_net_cross(-1, -10.0, &legs), None);
    // a zero net crosses a zero limit from BOTH sides
    assert_eq!(combo_net_cross(1, 0.0, &[(1, 5.0), (-1, 5.0)]), Some(0.0));
    assert_eq!(combo_net_cross(-1, 0.0, &[(1, 5.0), (-1, 5.0)]), Some(0.0));
}

#[test]
fn combo_cross_never_fills_on_a_zero_side_or_nan() {
    // REGRESSION: `side >= 0` treated 0 (the i32 default, e.g. a garbage/partial spec) as a
    // BUY and could report a crossing. ±1 is the workspace-wide law; anything else = no fill.
    let legs = [(1, 50.0), (-1, 30.0)]; // net = +20, crosses a buy limit of 25
    assert_eq!(combo_net_cross(1, 25.0, &legs), Some(20.0), "sanity: +1 does cross");
    assert_eq!(combo_net_cross(0, 25.0, &legs), None, "side 0 is NOT a buy");
    assert_eq!(combo_net_cross(0, -25.0, &legs), None);
    // NaN anywhere => both comparisons false => no fill (the safe verdict)
    assert_eq!(combo_net_cross(1, f64::NAN, &legs), None);
    assert_eq!(combo_net_cross(-1, f64::NAN, &legs), None);
    assert_eq!(combo_net_cross(1, 25.0, &[(1, f64::NAN), (-1, 30.0)]), None);
}

#[test]
fn combo_net_fold_is_left_to_right_in_leg_order() {
    // The doc says the naive left-to-right fold IN LEG ORDER is part of the contract. Pin it
    // with values whose sum is association-sensitive: (a+b)+c != a+(b+c) in f64.
    let a = 1e16_f64;
    let legs = [(1, a), (1, 1.0), (-1, a)];
    // left-to-right: ((0 + 1e16) + 1) - 1e16 == 0.0 (the 1 is lost to rounding)
    assert_eq!(combo_net(&legs), 0.0);
    // a reordered fold would give 1.0 — proving the order is observable, not cosmetic
    let reordered = [(1, a), (-1, a), (1, 1.0)];
    assert_eq!(combo_net(&reordered), 1.0);
    assert_ne!(
        combo_net(&legs).to_bits(),
        combo_net(&reordered).to_bits(),
        "leg order must NOT be normalized away"
    );
}

#[test]
fn combo_net_from_legs_picks_ask_for_bought_legs_and_bid_for_sold() {
    // Bridges ComboSpec.legs -> combo_net, deciding the book side ONCE.
    let legs = call_spread().legs; // +1 100k call, -1 120k call
    let quote = |_sym: &str, want_ask: bool| if want_ask { 0.030 } else { 0.010 };
    // BUYING the spread: pay the ASK on the +1 leg, receive the BID on the -1 leg.
    assert_eq!(combo_net_from_legs(1, &legs, quote), 0.030 - 0.010);
    // SELLING it flips both sides: receive the BID on the +1 leg, pay the ASK on the -1.
    assert_eq!(combo_net_from_legs(-1, &legs, quote), 0.010 - 0.030);
    // and it agrees with combo_net fed the same per-leg prices, in the same leg order
    assert_eq!(combo_net_from_legs(1, &legs, quote), combo_net(&[(1, 0.030), (-1, 0.010)]));
}

#[test]
fn combo_spec_rejects_degenerate_specs() {
    // REGRESSION (the leg-count invariant): a 0-leg spec used to lower to an ordinary-looking
    // NON-combo OrderRequest (empty `combo_legs` IS the "not a combo" sentinel) on an EMPTY
    // symbol at a NEGATIVE price. Both build_combo and Deserialize must refuse it.
    let mut zero = call_spread();
    zero.legs.clear();
    zero.net_limit = Some(-5.0);
    assert_eq!(zero.validate(), Err(ComboError::TooFewLegs(0)));
    assert_eq!(build_combo(&zero, "K"), Err(ComboError::TooFewLegs(0)));

    let mut one = call_spread();
    one.legs.truncate(1);
    assert_eq!(build_combo(&one, "K"), Err(ComboError::TooFewLegs(1)));

    let mut zero_ratio = call_spread();
    zero_ratio.legs[1].ratio = 0;
    assert_eq!(build_combo(&zero_ratio, "K"), Err(ComboError::ZeroRatio(1)));

    let mut empty_sym = call_spread();
    empty_sym.legs[0].symbol.clear();
    assert_eq!(build_combo(&empty_sym, "K"), Err(ComboError::EmptyLegSymbol(0)));

    for bad in [0, 2, -3] {
        let mut s = call_spread();
        s.side = bad;
        assert_eq!(build_combo(&s, "K"), Err(ComboError::InvalidSide(bad)));
    }

    for bad in [0.0, -1.0, f64::NAN, f64::INFINITY] {
        let mut s = call_spread();
        s.qty = bad;
        assert_matches!(build_combo(&s, "K"), Err(ComboError::InvalidQty(_)));
    }

    let mut nan_limit = call_spread();
    nan_limit.net_limit = Some(f64::NAN);
    assert_matches!(build_combo(&nan_limit, "K"), Err(ComboError::NonFiniteNetLimit(_)));

    // ...while the legal shapes still build, including a CREDIT and a MARKET combo
    assert!(call_spread().validate().is_ok());
    let mut credit = call_spread();
    credit.side = -1;
    credit.net_limit = Some(-0.0125);
    assert_eq!(build_combo(&credit, "K").unwrap().price, Some(-0.0125));
    let mut mkt = call_spread();
    mkt.net_limit = None;
    assert!(build_combo(&mkt, "K").is_ok());
}

#[test]
fn combo_spec_deserialize_enforces_the_invariants() {
    // A REPLAYED journal record / wire body cannot smuggle an invalid spec past the gate.
    let js = r#"{"venue":"deribit","side":1,"qty":2.0,"legs":[],"net_limit":-5.0}"#;
    let err = serde_json::from_str::<ComboSpec>(js).unwrap_err().to_string();
    assert!(err.contains("2..=N legs"), "{err}");

    let one = r#"{"venue":"deribit","side":1,"qty":2.0,
            "legs":[{"symbol":"BTC-PERPETUAL","ratio":1}],"net_limit":1.0}"#;
    assert!(serde_json::from_str::<ComboSpec>(one).is_err());

    let side0 = r#"{"venue":"deribit","side":0,"qty":2.0,
            "legs":[{"symbol":"A","ratio":1},{"symbol":"B","ratio":-1}],"net_limit":1.0}"#;
    assert!(serde_json::from_str::<ComboSpec>(side0).is_err());

    // ...and a VALID one still decodes, with `time_in_force` ABSENT defaulting to Gtc
    // (the additive-field claim for ComboSpec itself).
    let ok = r#"{"venue":"deribit","side":-1,"qty":2.0,
            "legs":[{"symbol":"A","ratio":1},{"symbol":"B","ratio":-1}],"net_limit":-5.0}"#;
    let spec: ComboSpec = serde_json::from_str(ok).unwrap();
    assert_eq!(spec.time_in_force, TimeInForce::Gtc);
    assert_eq!(spec.net_limit, Some(-5.0), "credit net survives deserialize unclamped");
    assert_eq!(spec.legs.len(), 2);
}

#[test]
fn bracket_leg_shaped_request_is_byte_identical_too() {
    // Second compatibility pin: the byte-identity claim must hold for the CONTINGENCY-carrying
    // payloads that actually dominate the journal, not just a plain limit order.
    let req = OrderRequest {
        client_order_id: "S".into(),
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        side: -1,
        qty: 3.0,
        order_type: OrderType::Stop,
        trigger_price: Some(95.0),
        reduce_only: true,
        ts: 7,
        parent_order_id: Some("E".into()),
        linked_order_ids: vec!["T".into()],
        order_list_id: Some("E".into()),
        contingency_type: Some("OCO".into()),
        on_close: true,
        ..Default::default()
    };
    let js = serde_json::to_string(&req).unwrap();
    assert_eq!(
        js,
        r#"{"client_order_id":"S","venue":"binance","symbol":"BTCUSDT","side":-1,"qty":3.0,"order_type":"stop","price":null,"trigger_price":95.0,"reduce_only":true,"time_in_force":"GTC","gtd_expiry":null,"ts":7,"parent_order_id":"E","linked_order_ids":["T"],"order_list_id":"E","contingency_type":"OCO","weight":0.0,"stop":null,"trail":null,"extreme":null,"on_close":true}"#
    );
    assert!(!js.contains("combo_legs"));
    assert_eq!(serde_json::from_str::<OrderRequest>(&js).unwrap(), req);
}

#[test]
fn build_bracket_market_entry_short() {
    let spec = BracketSpec {
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        side: -1, // short entry
        qty: 1.0,
        entry_price: None, // market entry
        stop_loss: 105.0,
        take_profit: 90.0,
        account: None,
    };
    let [entry, sl, tp] = build_bracket(&spec, "E", "S", "T");
    assert_eq!(entry.order_type, OrderType::Market);
    assert_eq!(entry.price, None);
    // exits are the opposite (long) side, sized to the entry
    assert_eq!(sl.side, 1);
    assert_eq!(tp.side, 1);
    assert_eq!(sl.qty, 1.0);
    assert_eq!(tp.qty, 1.0);
}

/// `OrderType::KNOWN` is a hand list, kept whole the way `TimeInForce::ALL` is: `idx` has one arm
/// per recognised variant and no wildcard, so a new variant stops this file compiling until it
/// gets an arm, and then `ARMS` and `KNOWN` must grow with it.
#[test]
fn order_type_known_is_every_recognised_variant_in_declaration_order() {
    const ARMS: usize = 5;
    fn idx(k: &OrderType) -> Option<usize> {
        match k {
            OrderType::Market => Some(0),
            OrderType::Limit => Some(1),
            OrderType::Stop => Some(2),
            OrderType::StopLimit => Some(3),
            OrderType::TakeProfit => Some(4),
            OrderType::Unrecognized(_) => None,
        }
    }
    assert_eq!(OrderType::KNOWN.len(), ARMS, "KNOWN and the arms of `idx` disagree");
    for (i, k) in OrderType::KNOWN.iter().enumerate() {
        assert_eq!(idx(k), Some(i), "{k:?} is not at its own index of KNOWN");
        assert!(k.is_recognized(), "{k:?}");
    }
    assert!(!OrderType::Unrecognized("x".into()).is_recognized());
    assert_eq!(OrderType::default(), OrderType::Market);
}

/// Every recognised variant serializes as its lowercase word and reads back as itself; the
/// trigger set is exactly stop / stop_limit / take_profit.
#[test]
fn order_type_round_trips_every_variant() {
    let words = ["market", "limit", "stop", "stop_limit", "take_profit"];
    for (k, word) in OrderType::KNOWN.iter().zip(words) {
        assert_eq!(k.as_str(), word);
        assert_eq!(k.to_string(), word);
        let json = serde_json::to_string(k).unwrap();
        assert_eq!(json, format!("\"{word}\""));
        assert_eq!(&serde_json::from_str::<OrderType>(&json).unwrap(), k);
        assert_eq!(&OrderType::from_stored(word), k);
    }
    let triggers: Vec<&OrderType> = OrderType::KNOWN.iter().filter(|k| k.is_trigger()).collect();
    assert_eq!(triggers, [&OrderType::Stop, &OrderType::StopLimit, &OrderType::TakeProfit]);
    assert!(!OrderType::Unrecognized("stop".into()).is_trigger());
}

/// The STORED door: a word that is not EXACTLY ours (`"stop_market"`, `"LIMIT"`, `""`) reads as
/// `Unrecognized` with the text verbatim and writes back the very same bytes, both alone and
/// inside a whole `OrderRequest` — so a journal record or snapshot written before the type
/// existed re-serializes byte for byte. `from_stored` gives the same verdict as serde.
#[test]
fn order_type_stored_door_keeps_unknown_text_byte_identical() {
    for word in ["stop_market", "LIMIT", "Market", " limit", "", "trailing"] {
        let json = serde_json::to_string(word).unwrap();
        let read: OrderType = serde_json::from_str(&json).unwrap();
        assert_eq!(read, OrderType::Unrecognized(word.to_owned()), "{word:?}");
        assert_eq!(read, OrderType::from_stored(word), "{word:?}: from_stored agrees with serde");
        assert_eq!(serde_json::to_string(&read).unwrap(), json, "{word:?}: bytes unchanged");
    }
    for word in ["stop_market", "LIMIT"] {
        let doc = format!(
            r#"{{"client_order_id":"c","venue":"binance","symbol":"BTCUSDT","side":1,"qty":1.0,"order_type":"{word}","price":2.0,"trigger_price":null,"reduce_only":false,"time_in_force":"GTC","gtd_expiry":null,"ts":0,"parent_order_id":null,"linked_order_ids":[],"order_list_id":null,"contingency_type":null,"weight":0.0,"stop":null,"trail":null,"extreme":null,"on_close":false}}"#
        );
        let req: OrderRequest = serde_json::from_str(&doc).unwrap();
        assert_eq!(req.order_type.as_str(), word);
        assert_eq!(serde_json::to_string(&req).unwrap(), doc, "{word}: request bytes unchanged");
    }
    // a non-string is refused exactly as the `String` field refused it
    assert!(serde_json::from_str::<OrderType>("3").is_err());
    assert!(serde_json::from_str::<OrderType>("null").is_err());
}

/// The INPUT door: trimmed and ASCII case-insensitive over the five words, nothing else — no
/// synonyms, never `Unrecognized` — and the refusal names every accepted word.
#[test]
fn order_type_parse_is_case_insensitive_trims_and_refuses_the_rest() {
    assert_eq!(OrderType::parse("limit"), Ok(OrderType::Limit));
    assert_eq!(OrderType::parse("LIMIT"), Ok(OrderType::Limit));
    assert_eq!(OrderType::parse(" Limit "), Ok(OrderType::Limit));
    assert_eq!(OrderType::parse("Stop_Limit"), Ok(OrderType::StopLimit));
    assert_eq!(OrderType::parse("TAKE_PROFIT\n"), Ok(OrderType::TakeProfit));
    for bad in ["", "  ", "garbage", "stop_market", "mkt", "trailing", "stop-limit"] {
        let err = OrderType::parse(bad).unwrap_err();
        assert_eq!(err.input, bad);
        assert_eq!(
            err.to_string(),
            format!(
                "unknown order type {bad:?}: expected one of market, limit, stop, stop_limit, \
                 take_profit"
            )
        );
    }
}

/// The side vocabulary every order-placing text surface reads (the CLI, Telegram): six words, any
/// ASCII case, and nothing else — a word outside it is `None`, never a guessed side.
#[test]
fn side_from_word_reads_six_words_in_any_case_and_nothing_else() {
    for w in ["buy", "b", "long", "BUY", "Long", "B"] {
        assert_eq!(side_from_word(w), Some(1), "{w}");
    }
    for w in ["sell", "s", "short", "SELL", "Short", "S"] {
        assert_eq!(side_from_word(w), Some(-1), "{w}");
    }
    for w in ["", "bid", "ask", "buy ", "1", "-1", "longs", "sel"] {
        assert_eq!(side_from_word(w), None, "{w:?}");
    }
}
