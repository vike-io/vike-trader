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
    let seq = j.append_strategy_submit(1_000, "binance__BTC__1m", &intent).unwrap();
    drop(j);

    let recs = CommandJournal::read_all(&dir).unwrap();
    assert_eq!(recs.len(), 1);
    match &recs[0] {
        JournalRecord::StrategySubmit { seq: got_seq, now_ms, mount_id, intent: got_intent } => {
            assert_eq!(*got_seq, seq);
            assert_eq!(*now_ms, 1_000);
            assert_eq!(mount_id, "binance__BTC__1m");
            assert_matches!(
                got_intent, vike_exec::OrderIntent::Submit(req) if req.symbol == "BTC",
                "intent round-trips byte-identical: {got_intent:?}"
            );
        }
        other => panic!("expected JournalRecord::StrategySubmit, got {other:?}"),
    }
}

/// `StrategySubmit` is additive to the tagged enum: a journal with ONLY `Cmd`/`Snap` records
/// (no mounted strategy ever ran) still round-trips exactly as before the variant was added.
#[test]
fn cmd_and_snap_still_roundtrip_alongside_the_new_variant() {
    let dir = tmp_dir("cmd-snap-unaffected");
    let cfg = JournalFileConfig::default();
    let mut j = CommandJournal::open(&dir, cfg).unwrap();
    j.append_cmd(1_000, &ingest(0)).unwrap();
    j.append_snap(1_001, &snap_engines(), "sess", 0, 3, &[], &[], &[], 0xBEEF).unwrap();
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
        order_type: "market".into(),
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
            },
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
            },
        },
    ];
    j.append_snap(1_000, &snap_engines(), "sess", 3, 2, &books, &[], &[], 0xF00D).unwrap();
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
        order_type: ot.into(),
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
    j.append_snap(1_000, &snap_engines(), "sess", 3, 2, &[], &book, &[], 0xF00D).unwrap();
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
