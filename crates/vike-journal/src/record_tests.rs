use super::*;
use crate::testutil::*;
use crate::{Journal, JournalFileConfig};
use std::assert_matches;

/// A mounted strategy's order intent (drain_broker boundary, PR-5) round-trips through the
/// journal exactly like a `Command` does: append (borrowed, no clone) -> flush on drop -> read_all
/// deserializes it back as an owned `JournalRecord::StrategySubmit` with the same mount_id +
/// intent. `mount_id` uses the runtime's real key shape (`venue__symbol__interval`).
#[test]
fn strategy_submit_record_roundtrips() {
    let dir = tmp_dir("strategy-submit");
    let config = JournalFileConfig::default();
    let mut j = Journal::open(&dir, config).unwrap();
    let intent = vike_execution::OrderIntent::Submit(Box::new(vike_model::OrderRequest {
        symbol: "BTC".into(),
        ..Default::default()
    }));
    let seq = j.append_strategy_submit(1_000, "binance__BTC__1m", &intent, None).unwrap();
    drop(j);

    let recs = Journal::read_all(&dir).unwrap();
    assert_eq!(recs.len(), 1);
    match &recs[0] {
        JournalRecord::StrategySubmit {
            sequence: got_seq,
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
                got_intent, vike_execution::OrderIntent::Submit(req) if req.symbol == "BTC",
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
    let mut j = Journal::open(&dir, JournalFileConfig::default()).unwrap();
    let order = |tif, gtd_expiry| {
        vike_execution::OrderIntent::Submit(Box::new(vike_model::OrderRequest {
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

    let got: Vec<(vike_model::TimeInForce, Option<i64>)> = Journal::read_all(&dir)
        .unwrap()
        .into_iter()
        .map(|r| match r {
            JournalRecord::StrategySubmit {
                intent: vike_execution::OrderIntent::Submit(req),
                ..
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
    assert_eq!(
        Journal::latest_segment_version(&dir).unwrap(),
        Some(crate::format::BASE_VERSION),
        "a purely additive word: no record of it raises the journal's stamp"
    );
}

/// **The route key of a write-ahead record is additive**: an absent one writes no key at all (a core
/// that routes by the payload's venue keeps its frame bytes) and reads back `None`; a present one
/// round-trips on both `StrategySubmit` and `OrderEmulationFire`, and a frame written BEFORE the field
/// (no such key) reads back `None`.
#[test]
fn the_route_key_of_a_write_ahead_record_is_additive() {
    let dir = tmp_dir("write-ahead-route-key");
    let mut j = Journal::open(&dir, JournalFileConfig::default()).unwrap();
    let intent = vike_execution::OrderIntent::Cancel("c-1".into());
    let req = vike_model::OrderRequest { venue: "bybit".into(), ..Default::default() };
    j.append_strategy_submit(1, "m", &intent, None).unwrap();
    j.append_strategy_submit(2, "m", &intent, Some("bybit#DEMO")).unwrap();
    j.append_conditional_fire(3, "a0", 9.5, &req, None).unwrap();
    j.append_conditional_fire(4, "a1", 9.5, &req, Some("bybit#DEMO")).unwrap();
    drop(j);

    let recs = Journal::read_all(&dir).unwrap();
    let keys: Vec<Option<&str>> = recs
        .iter()
        .map(|r| match r {
            JournalRecord::StrategySubmit { route_key, .. }
            | JournalRecord::OrderEmulationFire { route_key, .. } => route_key.as_deref(),
            other => panic!("unexpected {other:?}"),
        })
        .collect();
    assert_eq!(keys, [None, Some("bybit#DEMO"), None, Some("bybit#DEMO")]);

    // An absent key is OFF the wire, and a frame from before the field parses to `None`.
    let bytes = serde_json::to_vec(&JournalRecordRef::StrategySubmit {
        sequence: 0,
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

/// `StrategySubmit` is additive to the tagged enum: a journal with ONLY `Command`/`Snapshot` records
/// (no mounted strategy ever ran) still round-trips exactly as before the variant was added.
#[test]
fn command_and_snap_still_roundtrip_alongside_the_new_variant() {
    let dir = tmp_dir("cmd-snap-unaffected");
    let config = JournalFileConfig::default();
    let mut j = Journal::open(&dir, config).unwrap();
    j.append_command(1_000, &ingest(0)).unwrap();
    j.append_snap(1_001, &snap_engines(), "sess", 0, 3, JournalSnapshotParts::default(), 0xBEEF)
        .unwrap();
    drop(j);

    let recs = Journal::read_all(&dir).unwrap();
    assert_eq!(recs.len(), 2);
    assert_matches!(&recs[0], JournalRecord::Command { sequence: 0, .. });
    assert_matches!(
        &recs[1],
        JournalRecord::Snapshot { sequence: 1, hash: 0xBEEF, arm_sequence: Some(3), .. }
    );
}

/// The v7 record round-trips: append (borrowed) -> read_all -> owned
/// `JournalRecord::OrderEmulationDisarmed` with the same arm_id/now_ms/seq.
#[test]
fn conditional_disarmed_record_roundtrips() {
    let dir = tmp_dir("cond-disarmed");
    let mut j = Journal::open(&dir, JournalFileConfig::default()).unwrap();
    let seq = j.append_conditional_disarmed(1_000, "cafef00da3").unwrap();
    drop(j);
    let recs = Journal::read_all(&dir).unwrap();
    assert_eq!(recs.len(), 1);
    match &recs[0] {
        JournalRecord::OrderEmulationDisarmed { sequence: got, now_ms, arm_id } => {
            assert_eq!(*got, seq);
            assert_eq!(*now_ms, 1_000);
            assert_eq!(arm_id, "cafef00da3");
        }
        other => panic!("expected OrderEmulationDisarmed, got {other:?}"),
    }
}

/// The v9 record round-trips: append (borrowed) -> read_all -> owned
/// `JournalRecord::MarginCallLiquidate` with the same released request/now_ms/seq.
#[test]
fn margin_call_liquidate_record_roundtrips() {
    let dir = tmp_dir("margin-liquidate");
    let mut j = Journal::open(&dir, JournalFileConfig::default()).unwrap();
    let req = vike_model::OrderRequest {
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        side: -1,
        qty: 3.0,
        order_type: vike_model::OrderType::Market,
        reduce_only: true,
        timestamp: 1_000,
        ..Default::default()
    };
    let seq = j.append_margin_call_liquidate(1_000, &req, None, None).unwrap();
    // ...and the v14 OWNED variant: the per-mount budget latch's flatten names its mount.
    let owned_sequence =
        j.append_margin_call_liquidate(1_100, &req, Some("maker_a"), None).unwrap();
    drop(j);
    let recs = Journal::read_all(&dir).unwrap();
    assert_eq!(recs.len(), 2);
    match &recs[0] {
        JournalRecord::MarginCallLiquidate {
            sequence: got,
            now_ms,
            request: got_request,
            mount_id,
            ..
        } => {
            assert_eq!(*got, seq);
            assert_eq!(*now_ms, 1_000);
            assert_eq!(got_request.symbol, "BTCUSDT");
            assert_eq!(got_request.side, -1);
            assert_eq!(got_request.qty, 3.0);
            assert!(got_request.reduce_only, "the liquidation is reduce-only");
            assert!(
                got_request.order_client_id.is_empty(),
                "recorded PRE-mint, so replay re-mints the identical order client id"
            );
            assert_eq!(*mount_id, None, "the ACCOUNT-wide sweep is owned by no mount");
        }
        other => panic!("expected MarginCallLiquidate, got {other:?}"),
    }
    match &recs[1] {
        JournalRecord::MarginCallLiquidate { sequence: got, mount_id, .. } => {
            assert_eq!(*got, owned_sequence);
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

/// A `Snapshot` carrying armed books round-trips: append (borrowed) -> read_all -> owned
/// `JournalRecord::Snapshot` with the identical `JournalSnapshotOrderEmulationArm` entries in the identical
/// (fire) order — the trailing arm's ratcheted extreme included.
#[test]
fn snap_conditionals_roundtrip_in_order() {
    let dir = tmp_dir("snap-conditionals");
    let mut j = Journal::open(&dir, JournalFileConfig::default()).unwrap();
    let books = vec![
        JournalSnapshotOrderEmulationArm {
            arm_id: "cafef00da0".into(),
            terms: OrderEmulationRecord {
                venue: "sim".into(),
                symbol: "BTCUSDT".into(),
                side: -1,
                qty: 1.0,
                price: Some(95.0),
                trail: None,
                extreme: None,
                trigger_by: None,
                reduce_only: false,
                kind_terms: None,
            },
            route_key: None,
            mount_id: None,
        },
        JournalSnapshotOrderEmulationArm {
            arm_id: "cafef00da1".into(),
            terms: OrderEmulationRecord {
                venue: "sim".into(),
                symbol: "BTCUSDT".into(),
                side: -1,
                qty: 2.0,
                price: None,
                trail: Some(5.0),
                extreme: Some(110.0), // the CURRENT ratcheted extreme, not the seed
                trigger_by: None,
                reduce_only: false,
                kind_terms: None,
            },
            // v17: the arm's account rides the Snapshot on a multi-account core.
            route_key: Some("sim#ALT".into()),
            mount_id: None,
        },
    ];
    j.append_snap(
        1_000,
        &snap_engines(),
        "sess",
        3,
        2,
        JournalSnapshotParts { order_emulation_arms: &books, ..JournalSnapshotParts::default() },
        0xF00D,
    )
    .unwrap();
    drop(j);

    let recs = Journal::read_all(&dir).unwrap();
    assert_eq!(recs.len(), 1);
    match &recs[0] {
        JournalRecord::Snapshot { order_emulation_arms: conditionals, .. } => {
            assert_eq!(conditionals, &books, "books round-trip identically, order preserved");
        }
        other => panic!("expected Snapshot, got {other:?}"),
    }
}

/// A `Snapshot` carrying a resting OTO/OCO book round-trips: append (borrowed) -> read_all -> owned
/// `JournalRecord::Snapshot` with the identical `JournalSnapshotContingency` entries in the identical insertion
/// order — the held exits' resolved requests included (the durable half). The live-runtime
/// OCO/OTO twin of `snap_conditionals_roundtrip_in_order`.
#[test]
fn snap_contingencies_roundtrip_in_order() {
    let dir = tmp_dir("snap-contingencies");
    let mut j = Journal::open(&dir, JournalFileConfig::default()).unwrap();
    let held = |order_client_id: &str, side: i32, ot: &str| vike_model::OrderRequest {
        order_client_id: order_client_id.into(),
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
        JournalSnapshotContingency {
            order_client_id: "e".into(),
            parent: None,
            linked: vec!["sl".into(), "tp".into()],
            active: true,
            held_request: None,
            contingency_type: None,
        },
        // the HELD exits — each carries its resolved request
        JournalSnapshotContingency {
            order_client_id: "sl".into(),
            parent: Some("e".into()),
            linked: vec!["tp".into()],
            active: false,
            held_request: Some(held("sl", -1, "stop")),
            contingency_type: None,
        },
        JournalSnapshotContingency {
            order_client_id: "tp".into(),
            parent: Some("e".into()),
            linked: vec!["sl".into()],
            active: false,
            held_request: Some(held("tp", -1, "limit")),
            contingency_type: None,
        },
    ];
    j.append_snap(
        1_000,
        &snap_engines(),
        "sess",
        3,
        2,
        JournalSnapshotParts { contingencies: &book, ..JournalSnapshotParts::default() },
        0xF00D,
    )
    .unwrap();
    drop(j);

    let recs = Journal::read_all(&dir).unwrap();
    assert_eq!(recs.len(), 1);
    match &recs[0] {
        JournalRecord::Snapshot { contingencies, .. } => {
            assert_eq!(contingencies, &book, "the contingency book round-trips, order preserved");
        }
        other => panic!("expected Snapshot, got {other:?}"),
    }
}

/// **What one closed row costs** (`vike_model::TradeClosedRow`: a frame of the closed-trade archive, which keeps every row of a
/// mount's life, and a `Snapshot` row not archived yet): a busy BTC row with full-precision money, its bar index in the
/// tens of thousands, is about 250 bytes of JSON, so a mount closing 20,000 trades a year (a 1m strategy trading
/// about every 25 bars) adds about 5 MB to the archive in its year. The figure the PR states is this test's; it
/// fails when the row's shape grows past 300.
#[test]
fn a_closed_row_is_about_250_bytes() {
    let row = vike_model::TradeClosedRow {
        symbol: "BTCUSDT".into(),
        tag: String::new(),
        side: 1,
        entry_timestamp: 1_731_300_000_000,
        entry_bar: 43_210,
        entry_price: 104_235.5,
        exit_timestamp: 1_731_303_600_000,
        exit_bar: 43_211,
        exit_price: 104_300.25,
        qty: 0.015,
        pnl: (104_300.25 - 104_235.5) * 0.015,
        commission: 0.015 * 104_235.5 * 0.0001 + 0.015 * 104_300.25 * 0.0001,
        net: (104_300.25 - 104_235.5) * 0.015
            - (0.015 * 104_235.5 * 0.0001 + 0.015 * 104_300.25 * 0.0001),
    };
    let bytes = serde_json::to_vec(&row).unwrap().len();
    println!("one closed row: {bytes} bytes");
    assert!((200..=300).contains(&bytes), "{bytes}");
    let back: vike_model::TradeClosedRow =
        serde_json::from_slice(&serde_json::to_vec(&row).unwrap()).unwrap();
    assert_eq!(back, row, "a row round-trips bit for bit");
}

/// **A `Snapshot`'s open lots keep their journal bytes** (`JournalSnapshotClosedTrades::lots`, what a restart
/// resumes a mount's closed-trade ledger from): the nine keys in declaration order, an untagged lot writing no
/// `tag`, a linear and an inverse value. Pinned on the journal's own lot struct before B1.25 made the lot
/// `vike_model::TradeClosedOpenLot`, and unchanged after it, so a `Snapshot` either build wrote reads in the
/// other.
#[test]
fn a_snapshots_open_lots_keep_their_journal_bytes() {
    const BYTES: &str = concat!(
        r#"{"mount_id":"binance__BTCUSDT__1m","lots":["#,
        r#"{"symbol":"BTCUSDT","tag":"breakout","side":-1,"entry_timestamp":1700000000000,"entry_bar":7,"#,
        r#""price":100.5,"qty":0.25,"fee_per_unit":0.01,"value":{"kind":"linear","per_unit":0.5}},"#,
        r#"{"symbol":"BTCUSD_PERP","side":1,"entry_timestamp":1700000060000,"entry_bar":8,"#,
        r#""price":60000.0,"qty":3.0,"fee_per_unit":0.0,"value":{"kind":"inverse","per_unit":100.0}}]}"#,
    );
    let snap: JournalSnapshotClosedTrades = serde_json::from_str(BYTES).unwrap();
    let (tagged, untagged) = (&snap.lots[0], &snap.lots[1]);
    assert_eq!((tagged.tag.as_str(), tagged.side, tagged.entry_bar), ("breakout", -1, 7));
    assert_eq!((tagged.price, tagged.qty, tagged.fee_per_unit), (100.5, 0.25, 0.01));
    assert_eq!(tagged.value, vike_model::ContractValue::linear(0.5));
    assert_eq!((untagged.symbol.as_str(), untagged.tag.as_str()), ("BTCUSD_PERP", ""));
    assert_eq!(untagged.entry_timestamp, 1_700_000_060_000);
    assert_eq!(untagged.value, vike_model::ContractValue::inverse(100.0));
    assert_eq!(serde_json::to_string(&snap).unwrap(), BYTES, "the lots' bytes moved");
}
