use super::*;
use crate::testutil::*;
use crate::{CommandJournal, JournalFileConfig};
use std::assert_matches;

/// A mounted strategy's order intent (drain_broker boundary, PR-5) round-trips through the
/// journal exactly like a `Cmd` does: append (borrowed, no clone) -> flush on drop -> read_all
/// deserializes it back as an owned `JournalRecord::StrategySubmit` with the same mount_id +
/// intent. `mount_id` uses the runtime's real key shape (`venue__symbol__interval`).
#[test]
fn strategy_submit_record_roundtrips() {
    let dir = tmp_dir("strategy-submit");
    let cfg = JournalFileConfig::default();
    let mut j = CommandJournal::open(&dir, cfg).unwrap();
    let intent = vike_exec::OrderIntent::Submit(Box::new(vike_model::OrderRequest {
        symbol: "BTC".into(),
        ..Default::default()
    }));
    let seq = j.append_strategy_submit(1_000, "binance__BTC__1m", &intent, None).unwrap();
    drop(j);

    let recs = CommandJournal::read_all(&dir).unwrap();
    assert_eq!(recs.len(), 1);
    match &recs[0] {
        JournalRecord::StrategySubmit {
            seq: got_seq,
            now_ms,
            mount_id,
            intent: got_intent,
            route_key,
        } => {
            assert_eq!(*got_seq, seq);
            assert_eq!(*now_ms, 1_000);
            assert_eq!(mount_id, "binance__BTC__1m");
            assert_eq!(*route_key, None, "no key was given");
            assert_matches!(
                got_intent, vike_exec::OrderIntent::Submit(req) if req.symbol == "BTC",
                "intent round-trips byte-identical: {got_intent:?}"
            );
        }
        other => panic!("expected JournalRecord::StrategySubmit, got {other:?}"),
    }
}

/// **The order's lifetime is additive in the journal**: a default order is written exactly as before
/// (`"GTC"`, no expiry), a post-only or good-till-date order carries its word (`"POST_ONLY"`, `"GTD"` and the
/// expiry) and reads back the same, and a record written BEFORE the variant existed (no `POST_ONLY` anywhere)
/// still reads: the format version does not move.
#[test]
fn the_orders_lifetime_is_additive_in_the_journal() {
    let dir = tmp_dir("order-lifetime");
    let mut j = CommandJournal::open(&dir, JournalFileConfig::default()).unwrap();
    let order = |tif, gtd_expiry| {
        vike_exec::OrderIntent::Submit(Box::new(vike_model::OrderRequest {
            symbol: "BTC".into(),
            order_type: vike_model::OrderType::Limit,
            price: Some(100.0),
            time_in_force: tif,
            gtd_expiry,
            ..Default::default()
        }))
    };
    j.append_strategy_submit(1, "m", &order(vike_model::TimeInForce::Gtc, None), None).unwrap();
    j.append_strategy_submit(2, "m", &order(vike_model::TimeInForce::PostOnly, None), None)
        .unwrap();
    j.append_strategy_submit(
        3,
        "m",
        &order(vike_model::TimeInForce::Gtd, Some(4_102_444_800_000)),
        None,
    )
    .unwrap();
    drop(j);

    let got: Vec<(vike_model::TimeInForce, Option<i64>)> = CommandJournal::read_all(&dir)
        .unwrap()
        .into_iter()
        .map(|r| match r {
            JournalRecord::StrategySubmit {
                intent: vike_exec::OrderIntent::Submit(req), ..
            } => (req.time_in_force, req.gtd_expiry),
            other => panic!("expected a StrategySubmit, got {other:?}"),
        })
        .collect();
    assert_eq!(
        got,
        [
            (vike_model::TimeInForce::Gtc, None),
            (vike_model::TimeInForce::PostOnly, None),
            (vike_model::TimeInForce::Gtd, Some(4_102_444_800_000)),
        ]
    );
    // the bytes: the default is the word it always was, the new one is the one spelling
    let text = |tif| serde_json::to_string(&order(tif, None)).unwrap();
    assert!(text(vike_model::TimeInForce::Gtc).contains("\"time_in_force\":\"GTC\""));
    assert!(text(vike_model::TimeInForce::PostOnly).contains("\"time_in_force\":\"POST_ONLY\""));
    assert_eq!(crate::format::VERSION, 21, "a purely additive word: the journal version stays");
}

/// **The route key of a write-ahead record is additive**: an absent one writes no key at all (a core
/// that routes by the payload's venue keeps its frame bytes) and reads back `None`; a present one
/// round-trips on both `StrategySubmit` and `ConditionalFire`, and a frame written BEFORE the field
/// (no such key) reads back `None`.
#[test]
fn the_route_key_of_a_write_ahead_record_is_additive() {
    let dir = tmp_dir("write-ahead-route-key");
    let mut j = CommandJournal::open(&dir, JournalFileConfig::default()).unwrap();
    let intent = vike_exec::OrderIntent::Cancel("c-1".into());
    let req = vike_model::OrderRequest { venue: "bybit".into(), ..Default::default() };
    j.append_strategy_submit(1, "m", &intent, None).unwrap();
    j.append_strategy_submit(2, "m", &intent, Some("bybit#DEMO")).unwrap();
    j.append_conditional_fire(3, "a0", 9.5, &req, None).unwrap();
    j.append_conditional_fire(4, "a1", 9.5, &req, Some("bybit#DEMO")).unwrap();
    drop(j);

    let recs = CommandJournal::read_all(&dir).unwrap();
    let keys: Vec<Option<&str>> = recs
        .iter()
        .map(|r| match r {
            JournalRecord::StrategySubmit { route_key, .. }
            | JournalRecord::ConditionalFire { route_key, .. } => route_key.as_deref(),
            other => panic!("unexpected {other:?}"),
        })
        .collect();
    assert_eq!(keys, [None, Some("bybit#DEMO"), None, Some("bybit#DEMO")]);

    // An absent key is OFF the wire, and a frame from before the field parses to `None`.
    let bytes = serde_json::to_vec(&JournalRecordRef::StrategySubmit {
        seq: 0,
        now_ms: 1,
        mount_id: "m",
        intent: &intent,
        route_key: None,
    })
    .unwrap();
    assert!(!String::from_utf8(bytes.clone()).unwrap().contains("route_key"));
    assert_matches!(
        serde_json::from_slice::<JournalRecord>(&bytes).unwrap(),
        JournalRecord::StrategySubmit { route_key: None, .. }
    );
}

/// `StrategySubmit` is additive to the tagged enum: a journal with ONLY `Cmd`/`Snap` records
/// (no mounted strategy ever ran) still round-trips exactly as before the variant was added.
#[test]
fn cmd_and_snap_still_roundtrip_alongside_the_new_variant() {
    let dir = tmp_dir("cmd-snap-unaffected");
    let cfg = JournalFileConfig::default();
    let mut j = CommandJournal::open(&dir, cfg).unwrap();
    j.append_cmd(1_000, &ingest(0)).unwrap();
    j.append_snap(1_001, &snap_engines(), "sess", 0, 3, SnapBooks::default(), 0xBEEF).unwrap();
    drop(j);

    let recs = CommandJournal::read_all(&dir).unwrap();
    assert_eq!(recs.len(), 2);
    assert_matches!(&recs[0], JournalRecord::Cmd { seq: 0, .. });
    assert_matches!(&recs[1], JournalRecord::Snap { seq: 1, hash: 0xBEEF, arm_seq: Some(3), .. });
}

/// The v7 record round-trips: append (borrowed) -> read_all -> owned
/// `JournalRecord::ConditionalDisarmed` with the same arm_id/now_ms/seq.
#[test]
fn conditional_disarmed_record_roundtrips() {
    let dir = tmp_dir("cond-disarmed");
    let mut j = CommandJournal::open(&dir, JournalFileConfig::default()).unwrap();
    let seq = j.append_conditional_disarmed(1_000, "cafef00da3").unwrap();
    drop(j);
    let recs = CommandJournal::read_all(&dir).unwrap();
    assert_eq!(recs.len(), 1);
    match &recs[0] {
        JournalRecord::ConditionalDisarmed { seq: got, now_ms, arm_id } => {
            assert_eq!(*got, seq);
            assert_eq!(*now_ms, 1_000);
            assert_eq!(arm_id, "cafef00da3");
        }
        other => panic!("expected ConditionalDisarmed, got {other:?}"),
    }
}

/// The v9 record round-trips: append (borrowed) -> read_all -> owned
/// `JournalRecord::MarginCallLiquidate` with the same released request/now_ms/seq.
#[test]
fn margin_call_liquidate_record_roundtrips() {
    let dir = tmp_dir("margin-liquidate");
    let mut j = CommandJournal::open(&dir, JournalFileConfig::default()).unwrap();
    let req = vike_model::OrderRequest {
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        side: -1,
        qty: 3.0,
        order_type: vike_model::OrderType::Market,
        reduce_only: true,
        ts: 1_000,
        ..Default::default()
    };
    let seq = j.append_margin_call_liquidate(1_000, &req, None, None).unwrap();
    // ...and the v14 OWNED variant: the per-mount budget latch's flatten names its mount.
    let owned_seq = j.append_margin_call_liquidate(1_100, &req, Some("maker_a"), None).unwrap();
    drop(j);
    let recs = CommandJournal::read_all(&dir).unwrap();
    assert_eq!(recs.len(), 2);
    match &recs[0] {
        JournalRecord::MarginCallLiquidate { seq: got, now_ms, req: got_req, mount_id, .. } => {
            assert_eq!(*got, seq);
            assert_eq!(*now_ms, 1_000);
            assert_eq!(got_req.symbol, "BTCUSDT");
            assert_eq!(got_req.side, -1);
            assert_eq!(got_req.qty, 3.0);
            assert!(got_req.reduce_only, "the liquidation is reduce-only");
            assert!(
                got_req.client_order_id.is_empty(),
                "recorded PRE-mint, so replay re-mints the identical coid"
            );
            assert_eq!(*mount_id, None, "the ACCOUNT-wide sweep is owned by no mount");
        }
        other => panic!("expected MarginCallLiquidate, got {other:?}"),
    }
    match &recs[1] {
        JournalRecord::MarginCallLiquidate { seq: got, mount_id, .. } => {
            assert_eq!(*got, owned_seq);
            assert_eq!(
                mount_id.as_deref(),
                Some("maker_a"),
                "the budget latch's flatten carries its OWNING mount, so a restore books its \
                     fill into that mount's ledger instead of the residual row"
            );
        }
        other => panic!("expected MarginCallLiquidate, got {other:?}"),
    }
}

/// A `Snap` carrying armed books round-trips: append (borrowed) -> read_all -> owned
/// `JournalRecord::Snap` with the identical `SnapConditional` entries in the identical
/// (fire) order — the trailing arm's ratcheted extreme included.
#[test]
fn snap_conditionals_roundtrip_in_order() {
    let dir = tmp_dir("snap-conditionals");
    let mut j = CommandJournal::open(&dir, JournalFileConfig::default()).unwrap();
    let books = vec![
        SnapConditional {
            arm_id: "cafef00da0".into(),
            terms: ConditionalRecord {
                venue: "sim".into(),
                symbol: "BTCUSDT".into(),
                side: -1,
                qty: 1.0,
                price: Some(95.0),
                trail: None,
                extreme: None,
                trigger_by: None,
                reduce_only: false,
            },
            route_key: None,
        },
        SnapConditional {
            arm_id: "cafef00da1".into(),
            terms: ConditionalRecord {
                venue: "sim".into(),
                symbol: "BTCUSDT".into(),
                side: -1,
                qty: 2.0,
                price: None,
                trail: Some(5.0),
                extreme: Some(110.0), // the CURRENT ratcheted extreme, not the seed
                trigger_by: None,
                reduce_only: false,
            },
            // v17: the arm's account rides the Snap on a multi-account core.
            route_key: Some("sim#ALT".into()),
        },
    ];
    j.append_snap(
        1_000,
        &snap_engines(),
        "sess",
        3,
        2,
        SnapBooks { conditionals: &books, ..SnapBooks::default() },
        0xF00D,
    )
    .unwrap();
    drop(j);

    let recs = CommandJournal::read_all(&dir).unwrap();
    assert_eq!(recs.len(), 1);
    match &recs[0] {
        JournalRecord::Snap { conditionals, .. } => {
            assert_eq!(conditionals, &books, "books round-trip identically, order preserved");
        }
        other => panic!("expected Snap, got {other:?}"),
    }
}

/// A `Snap` carrying a resting OTO/OCO book round-trips: append (borrowed) -> read_all -> owned
/// `JournalRecord::Snap` with the identical `SnapContingency` entries in the identical insertion
/// order — the held exits' resolved requests included (the durable half). The live-runtime
/// OCO/OTO twin of `snap_conditionals_roundtrip_in_order`.
#[test]
fn snap_contingencies_roundtrip_in_order() {
    let dir = tmp_dir("snap-contingencies");
    let mut j = CommandJournal::open(&dir, JournalFileConfig::default()).unwrap();
    let held = |coid: &str, side: i32, ot: &str| vike_model::OrderRequest {
        client_order_id: coid.into(),
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        side,
        qty: 2.0,
        order_type: vike_model::OrderType::from_stored(ot),
        reduce_only: true,
        parent_order_id: Some("e".into()),
        ..Default::default()
    };
    let book = vec![
        // the ACTIVE entry (already in the registry — no held request)
        SnapContingency {
            coid: "e".into(),
            parent: None,
            linked: vec!["sl".into(), "tp".into()],
            active: true,
            held_request: None,
        },
        // the HELD exits — each carries its resolved request
        SnapContingency {
            coid: "sl".into(),
            parent: Some("e".into()),
            linked: vec!["tp".into()],
            active: false,
            held_request: Some(held("sl", -1, "stop")),
        },
        SnapContingency {
            coid: "tp".into(),
            parent: Some("e".into()),
            linked: vec!["sl".into()],
            active: false,
            held_request: Some(held("tp", -1, "limit")),
        },
    ];
    j.append_snap(
        1_000,
        &snap_engines(),
        "sess",
        3,
        2,
        SnapBooks { contingencies: &book, ..SnapBooks::default() },
        0xF00D,
    )
    .unwrap();
    drop(j);

    let recs = CommandJournal::read_all(&dir).unwrap();
    assert_eq!(recs.len(), 1);
    match &recs[0] {
        JournalRecord::Snap { contingencies, .. } => {
            assert_eq!(contingencies, &book, "the contingency book round-trips, order preserved");
        }
        other => panic!("expected Snap, got {other:?}"),
    }
}
