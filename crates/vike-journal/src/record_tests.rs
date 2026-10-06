use super::*;
use crate::frame::fnv1a32;
use crate::testutil::*;
use crate::{CommandJournal, HEADER, JournalFileConfig};

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
            assert!(
                matches!(got_intent, vike_exec::OrderIntent::Submit(req) if req.symbol == "BTC"),
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
    assert!(matches!(&recs[0], JournalRecord::Cmd { seq: 0, .. }));
    assert!(matches!(&recs[1], JournalRecord::Snap { seq: 1, hash: 0xBEEF, arm_seq: Some(3), .. }));
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

/// A pre-v14 `MarginCallLiquidate` frame — one with NO `mount_id` key at all — still reads back,
/// as `None`. That is the additive-step contract `MIN_READABLE_VERSION` rests on, and `None` is
/// the correct reading: every such frame predates the per-mount budget latch's ownership stamp.
#[test]
fn a_pre_v14_margin_call_liquidate_frame_reads_back_unowned() {
    let json = serde_json::json!({
        "MarginCallLiquidate": {
            "seq": 7,
            "now_ms": 1_000,
            "req": { "client_order_id": "", "venue": "sim", "symbol": "BTCUSDT",
                     "side": -1, "qty": 3.0, "order_type": "market" }
        }
    });
    match serde_json::from_value::<JournalRecord>(json).expect("a v13 frame is a valid v14 one") {
        JournalRecord::MarginCallLiquidate { seq, mount_id, .. } => {
            assert_eq!(seq, 7);
            assert_eq!(mount_id, None, "an absent key defaults to unowned");
        }
        other => panic!("expected MarginCallLiquidate, got {other:?}"),
    }
}

/// **THE v16 ARM** (the account-routing spec's §9 item 12): both write-ahead records carry
/// WHICH ACCOUNT of `req.venue` their order was lowered onto, and both round-trip.
#[test]
fn a_write_ahead_record_carries_its_resolved_route_key() {
    let dir = tmp_dir("write-ahead-route-key");
    let mut j = CommandJournal::open(&dir, JournalFileConfig::default()).unwrap();
    let req = vike_model::OrderRequest {
        client_order_id: "c-1".into(),
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        side: 1,
        qty: 1.0,
        order_type: "limit".into(),
        ts: 1_000,
        ..Default::default()
    };
    j.append_minted_submit(1_000, &req, Some("sim#ALT")).unwrap();
    j.append_margin_call_liquidate(1_100, &req, Some("maker_a"), Some("sim#ALT")).unwrap();
    drop(j);

    let recs = CommandJournal::read_all(&dir).unwrap();
    assert_eq!(recs.len(), 2);
    match &recs[0] {
        JournalRecord::MintedSubmit { route_key, .. } => {
            assert_eq!(route_key.as_deref(), Some("sim#ALT"), "the minted submit's account");
        }
        other => panic!("expected MintedSubmit, got {other:?}"),
    }
    match &recs[1] {
        JournalRecord::MarginCallLiquidate { mount_id, route_key, .. } => {
            assert_eq!(mount_id.as_deref(), Some("maker_a"), "WHOSE loss it is");
            assert_eq!(
                route_key.as_deref(),
                Some("sim#ALT"),
                "…and WHICH of the exchange's accounts holds it — a different question"
            );
        }
        other => panic!("expected MarginCallLiquidate, got {other:?}"),
    }
}

/// ⚠ **THE BYTE-IDENTITY HALF, and it is the one that matters for every box in production.**
/// A default account passes `None`, and the key must be OFF THE WIRE entirely rather than
/// written as `null` — `vike_exec::ReconcileReports::route_key`'s own argument: *"keeping the
/// absent field OFF the wire is what makes a single-account box's journal bytes identical to
/// the ones written before this field existed."*
///
/// Read out of the raw segment rather than through serde, because serde would answer the same
/// for a `null` and for an absent key — which is exactly the difference under test.
#[test]
fn a_default_account_box_writes_no_route_key_key_at_all() {
    let dir = tmp_dir("write-ahead-route-key-absent");
    let cfg = JournalFileConfig { segment_bytes: 64 * 1024, flush_every: 1 };
    let mut j = CommandJournal::open(&dir, cfg).unwrap();
    let req = vike_model::OrderRequest {
        client_order_id: "c-1".into(),
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        side: 1,
        qty: 1.0,
        order_type: "limit".into(),
        ts: 1_000,
        ..Default::default()
    };
    j.append_minted_submit(1_000, &req, None).unwrap();
    j.append_margin_call_liquidate(1_100, &req, None, None).unwrap();
    drop(j);

    let bytes = std::fs::read(seg_files(&dir).remove(0)).unwrap();
    let text = String::from_utf8_lossy(&bytes);
    assert!(text.contains("MintedSubmit"), "the frames really were written: {text:?}");
    assert!(text.contains("MarginCallLiquidate"));
    assert!(
        !text.contains("route_key"),
        "a single-account box's journal BYTES must not move: {text:?}"
    );
}

/// …and a pre-v16 frame — one with no `route_key` key at all — still reads back, as the
/// venue's sole account. The other direction of the same additive claim.
#[test]
fn a_pre_v16_write_ahead_frame_reads_back_as_the_sole_account() {
    let minted = serde_json::json!({
        "MintedSubmit": {
            "seq": 3,
            "now_ms": 1_000,
            "req": { "client_order_id": "c-1", "venue": "sim", "symbol": "BTCUSDT",
                     "side": 1, "qty": 1.0, "order_type": "limit" }
        }
    });
    match serde_json::from_value::<JournalRecord>(minted).expect("a v15 frame is a valid v16 one") {
        JournalRecord::MintedSubmit { seq, route_key, .. } => {
            assert_eq!(seq, 3);
            assert_eq!(route_key, None, "an absent key means the venue's sole account");
        }
        other => panic!("expected MintedSubmit, got {other:?}"),
    }
    let liq = serde_json::json!({
        "MarginCallLiquidate": {
            "seq": 4,
            "now_ms": 1_000,
            "req": { "client_order_id": "", "venue": "sim", "symbol": "BTCUSDT",
                     "side": -1, "qty": 3.0, "order_type": "market" },
            "mount_id": null
        }
    });
    match serde_json::from_value::<JournalRecord>(liq).expect("a v15 frame is a valid v16 one") {
        JournalRecord::MarginCallLiquidate { seq, route_key, .. } => {
            assert_eq!(seq, 4);
            assert_eq!(route_key, None);
        }
        other => panic!("expected MarginCallLiquidate, got {other:?}"),
    }
}

/// The v6->v7 `Snap` compat seam: a pre-v7 `Snap` frame carries NO `arm_seq` field, and this
/// build must read it back as `None` (ABSENT — never conflated with a genuine `Some(0)`), so
/// the restore path knows to fall back to its prune-safe bound. Hand-writes the v6 frame
/// (strip the field from a v7 payload, re-frame, restamp the header to 6) since this build's
/// writer only ever emits v7.
#[test]
fn a_pre_v7_snap_without_arm_seq_reads_back_as_absent() {
    let dir = tmp_dir("snap-arm-seq-compat");
    let cfg = JournalFileConfig { segment_bytes: 64 * 1024, flush_every: 1 };
    let mut j = CommandJournal::open(&dir, cfg).unwrap();
    j.append_snap(1_000, &snap_engines(), "sess", 7, 9, &[], &[], &[], 0xABCD).unwrap();
    drop(j);

    // Rewrite the one frame with `arm_seq` REMOVED from its JSON payload — byte-for-byte what
    // a v6 writer produced — and restamp the header version to 6.
    let seg = seg_files(&dir).remove(0);
    let bytes = std::fs::read(&seg).unwrap();
    let len = u32::from_le_bytes(bytes[HEADER..HEADER + 4].try_into().unwrap()) as usize;
    let payload = &bytes[HEADER + 8..HEADER + 8 + len];
    let mut v: serde_json::Value = serde_json::from_slice(payload).unwrap();
    assert!(
        v["Snap"].as_object_mut().unwrap().remove("arm_seq").is_some(),
        "the v7 writer stamps arm_seq"
    );
    // a v6 frame predates `conditionals` too — strip it so the payload is byte-faithful
    assert!(
        v["Snap"].as_object_mut().unwrap().remove("conditionals").is_some(),
        "the v8 writer stamps conditionals"
    );
    let stripped = serde_json::to_vec(&v).unwrap();
    let mut out = bytes[..HEADER].to_vec();
    out[4..8].copy_from_slice(&6u32.to_le_bytes());
    out.extend_from_slice(&(stripped.len() as u32).to_le_bytes());
    out.extend_from_slice(&fnv1a32(&stripped).to_le_bytes());
    out.extend_from_slice(&stripped);
    out.resize(bytes.len(), 0);
    std::fs::write(&seg, out).unwrap();

    let recs = CommandJournal::read_all(&dir).unwrap();
    assert_eq!(recs.len(), 1);
    match &recs[0] {
        JournalRecord::Snap { arm_seq, coid_seq, conditionals, .. } => {
            assert_eq!(*arm_seq, None, "absent field reads back as None, never Some(0)");
            assert!(conditionals.is_empty(), "absent books read back EMPTY, never an error");
            assert_eq!(*coid_seq, 7, "the rest of the record is untouched");
        }
        other => panic!("expected Snap, got {other:?}"),
    }
}

/// The v7->v8 `Snap` compat seam (emulator PR-3): a pre-v8 `Snap` frame carries NO
/// `conditionals` field, and this build must read it back as EMPTY books — a pre-existing
/// journal restores with today's (pre-PR-3) behavior, never an error. Hand-writes the v7
/// frame (strip the field from a v8 payload, re-frame, restamp the header to 7) since this
/// build's writer only ever emits v8.
#[test]
fn a_pre_v8_snap_without_conditionals_reads_back_as_empty_books() {
    let dir = tmp_dir("snap-conditionals-compat");
    let cfg = JournalFileConfig { segment_bytes: 64 * 1024, flush_every: 1 };
    let mut j = CommandJournal::open(&dir, cfg).unwrap();
    j.append_snap(1_000, &snap_engines(), "sess", 7, 9, &[], &[], &[], 0xABCD).unwrap();
    drop(j);

    let seg = seg_files(&dir).remove(0);
    let bytes = std::fs::read(&seg).unwrap();
    let len = u32::from_le_bytes(bytes[HEADER..HEADER + 4].try_into().unwrap()) as usize;
    let payload = &bytes[HEADER + 8..HEADER + 8 + len];
    let mut v: serde_json::Value = serde_json::from_slice(payload).unwrap();
    assert!(
        v["Snap"].as_object_mut().unwrap().remove("conditionals").is_some(),
        "the v8 writer stamps conditionals"
    );
    let stripped = serde_json::to_vec(&v).unwrap();
    let mut out = bytes[..HEADER].to_vec();
    out[4..8].copy_from_slice(&7u32.to_le_bytes());
    out.extend_from_slice(&(stripped.len() as u32).to_le_bytes());
    out.extend_from_slice(&fnv1a32(&stripped).to_le_bytes());
    out.extend_from_slice(&stripped);
    out.resize(bytes.len(), 0);
    std::fs::write(&seg, out).unwrap();

    let recs = CommandJournal::read_all(&dir).unwrap();
    assert_eq!(recs.len(), 1);
    match &recs[0] {
        JournalRecord::Snap { conditionals, arm_seq, coid_seq, .. } => {
            assert!(conditionals.is_empty(), "absent field reads back as EMPTY books");
            assert_eq!(*arm_seq, Some(9), "the v7-era field is untouched");
            assert_eq!(*coid_seq, 7, "the rest of the record is untouched");
        }
        other => panic!("expected Snap, got {other:?}"),
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

/// The v10->v11 `Snap` compat seam (live-runtime OCO/OTO): a pre-v11 `Snap` frame carries NO
/// `contingencies` field, and this build must read it back as an EMPTY book — a pre-OCO/OTO
/// journal restores with today's behavior (no contingency state), never an error. Hand-writes
/// the v10 frame (strip the field from a v11 payload, re-frame, restamp the header to 10).
#[test]
fn a_pre_v11_snap_without_contingencies_reads_back_as_empty() {
    let dir = tmp_dir("snap-contingencies-compat");
    let cfg = JournalFileConfig { segment_bytes: 64 * 1024, flush_every: 1 };
    let mut j = CommandJournal::open(&dir, cfg).unwrap();
    j.append_snap(1_000, &snap_engines(), "sess", 7, 9, &[], &[], &[], 0xABCD).unwrap();
    drop(j);

    let seg = seg_files(&dir).remove(0);
    let bytes = std::fs::read(&seg).unwrap();
    let len = u32::from_le_bytes(bytes[HEADER..HEADER + 4].try_into().unwrap()) as usize;
    let payload = &bytes[HEADER + 8..HEADER + 8 + len];
    let mut v: serde_json::Value = serde_json::from_slice(payload).unwrap();
    assert!(
        v["Snap"].as_object_mut().unwrap().remove("contingencies").is_some(),
        "the v11 writer stamps contingencies"
    );
    let stripped = serde_json::to_vec(&v).unwrap();
    let mut out = bytes[..HEADER].to_vec();
    out[4..8].copy_from_slice(&10u32.to_le_bytes());
    out.extend_from_slice(&(stripped.len() as u32).to_le_bytes());
    out.extend_from_slice(&fnv1a32(&stripped).to_le_bytes());
    out.extend_from_slice(&stripped);
    out.resize(bytes.len(), 0);
    std::fs::write(&seg, out).unwrap();

    let recs = CommandJournal::read_all(&dir).unwrap();
    assert_eq!(recs.len(), 1);
    match &recs[0] {
        JournalRecord::Snap { contingencies, conditionals, coid_seq, .. } => {
            assert!(contingencies.is_empty(), "absent field reads back as an EMPTY book");
            assert!(conditionals.is_empty(), "the v8-era field is still empty/intact");
            assert_eq!(*coid_seq, 7, "the rest of the record is untouched");
        }
        other => panic!("expected Snap, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------------------
// v15: `AccountState.route_key` — the journal half of the multi-account balance routing fix
// ---------------------------------------------------------------------------------------

/// One `Ingest::Event` carrying a balance snapshot, stamped or not.
fn account_state_ingest(route_key: Option<&str>) -> vike_exec::Ingest {
    vike_exec::Ingest::Event(vike_model::events::Event::AccountState(
        vike_model::events::AccountState {
            venue: "binance".into(),
            balances: vec![("USDT".to_string(), 1234.5)],
            ts: 7,
            route_key: route_key.map(Into::into),
        },
    ))
}

/// The framed payload bytes of every record in `dir`'s single segment — what actually reaches
/// the disk, read back through the frame codec rather than re-derived from the types.
fn framed_payloads(dir: &std::path::Path) -> Vec<String> {
    let bytes = std::fs::read(seg_files(dir).remove(0)).unwrap();
    let mut out = Vec::new();
    let mut at = HEADER;
    loop {
        let len = u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap()) as usize;
        if len == 0 {
            return out;
        }
        let payload = &bytes[at + 8..at + 8 + len];
        assert_eq!(
            fnv1a32(payload),
            u32::from_le_bytes(bytes[at + 4..at + 8].try_into().unwrap()),
            "the frame must checksum, or the bytes below are not the bytes written"
        );
        out.push(String::from_utf8(payload.to_vec()).unwrap());
        at += 8 + len;
    }
}

/// **THE BYTE-IDENTITY GATE: a default-account box writes the frame it always wrote.**
///
/// Spelled as a LITERAL rather than derived from the types, so a change to `AccountState`
/// cannot move both sides of the comparison together — the failure mode that has let four
/// gates in this program pass while measuring nothing. These are the exact bytes a pre-v15
/// build emitted for this record, `route_key` omitted entirely by `skip_serializing_if`.
#[test]
fn an_unstamped_account_state_frame_is_byte_identical_to_its_pre_v15_bytes() {
    let dir = tmp_dir("acct-state-bytes");
    let cfg = JournalFileConfig { segment_bytes: 64 * 1024, flush_every: 1 };
    let mut j = CommandJournal::open(&dir, cfg).unwrap();
    j.append_cmd(1_000, &account_state_ingest(None)).unwrap();
    drop(j);

    assert_eq!(
        framed_payloads(&dir),
        vec![
            concat!(
                r#"{"Cmd":{"seq":0,"now_ms":1000,"msg":{"Event":{"type":"AccountState","#,
                r#""venue":"binance","balances":[["USDT",1234.5]],"ts":7}}}}"#
            )
            .to_string()
        ],
        "no `route_key` key may appear in a default-account box's journal"
    );
}

/// …and a LABELLED account's frame does carry it, and reads back as the key the router folds
/// on. Without this the gate above would pass just as well if the field never serialized.
#[test]
fn a_stamped_account_state_frame_carries_its_route_key_through_the_journal() {
    let dir = tmp_dir("acct-state-stamped");
    let cfg = JournalFileConfig { segment_bytes: 64 * 1024, flush_every: 1 };
    let mut j = CommandJournal::open(&dir, cfg).unwrap();
    j.append_cmd(1_000, &account_state_ingest(Some("binance#ALT"))).unwrap();
    drop(j);

    assert!(
        framed_payloads(&dir)[0].contains(r#""route_key":"binance#ALT""#),
        "got {:?}",
        framed_payloads(&dir)
    );
    match &CommandJournal::read_all(&dir).unwrap()[0] {
        JournalRecord::Cmd {
            msg: vike_exec::Ingest::Event(vike_model::events::Event::AccountState(a)),
            ..
        } => assert_eq!(a.route_key.as_ref().map(|k| k.as_str()), Some("binance#ALT")),
        other => panic!("expected a Cmd carrying an AccountState, got {other:?}"),
    }
}

/// **THE REPLAY GATE: an existing (pre-v15) segment still reads back, unchanged.**
///
/// The setup is only sound because of the test above it: the records this build writes for an
/// unstamped snapshot ARE the bytes a v14 build wrote, so restamping the header to 14 produces
/// a genuine v14 segment rather than an approximation of one.
#[test]
fn a_pre_v15_segment_of_account_states_replays_unchanged() {
    let dir = tmp_dir("acct-state-v14-replay");
    let cfg = JournalFileConfig { segment_bytes: 64 * 1024, flush_every: 1 };
    let mut j = CommandJournal::open(&dir, cfg).unwrap();
    for i in 0..3 {
        j.append_cmd(1_000 + i as i64, &account_state_ingest(None)).unwrap();
    }
    drop(j);
    let before = framed_payloads(&dir);

    let seg = seg_files(&dir).remove(0);
    let mut bytes = std::fs::read(&seg).unwrap();
    bytes[4..8].copy_from_slice(&14u32.to_le_bytes());
    std::fs::write(&seg, bytes).unwrap();

    assert_eq!(framed_payloads(&dir), before, "restamping the header touches no frame");
    let back = CommandJournal::read_all(&dir).unwrap();
    assert_eq!(back.len(), 3, "a v14 segment of balance snapshots is fully readable");
    for r in &back {
        match r {
            JournalRecord::Cmd {
                msg: vike_exec::Ingest::Event(vike_model::events::Event::AccountState(a)),
                ..
            } => assert_eq!(
                a.route_key, None,
                "a pre-v15 frame means THE venue's sole account, and must read back as that"
            ),
            other => panic!("expected a Cmd carrying an AccountState, got {other:?}"),
        }
    }
}
