//! The version-compat contract per record: pre-vN frames read back, absent keys stay off the wire.

use super::*;
use crate::format::HEADER;
use crate::frame::fnv1a32;
use crate::testutil::*;
use crate::{CommandJournal, JournalFileConfig};

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
        order_type: vike_model::OrderType::Limit,
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
    let cfg = small_cfg();
    let mut j = CommandJournal::open(&dir, cfg).unwrap();
    let req = vike_model::OrderRequest {
        client_order_id: "c-1".into(),
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        side: 1,
        qty: 1.0,
        order_type: vike_model::OrderType::Limit,
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
    let cfg = small_cfg();
    let mut j = CommandJournal::open(&dir, cfg).unwrap();
    j.append_snap(1_000, &snap_engines(), "sess", 7, 9, SnapBooks::default(), 0xABCD).unwrap();
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
    let cfg = small_cfg();
    let mut j = CommandJournal::open(&dir, cfg).unwrap();
    j.append_snap(1_000, &snap_engines(), "sess", 7, 9, SnapBooks::default(), 0xABCD).unwrap();
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

/// The v10->v11 `Snap` compat seam (live-runtime OCO/OTO): a pre-v11 `Snap` frame carries NO
/// `contingencies` field, and this build must read it back as an EMPTY book — a pre-OCO/OTO
/// journal restores with today's behavior (no contingency state), never an error. Hand-writes
/// the v10 frame (strip the field from a v11 payload, re-frame, restamp the header to 10).
#[test]
fn a_pre_v11_snap_without_contingencies_reads_back_as_empty() {
    let dir = tmp_dir("snap-contingencies-compat");
    let cfg = small_cfg();
    let mut j = CommandJournal::open(&dir, cfg).unwrap();
    j.append_snap(1_000, &snap_engines(), "sess", 7, 9, SnapBooks::default(), 0xABCD).unwrap();
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
    let cfg = small_cfg();
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
    let cfg = small_cfg();
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
    let cfg = small_cfg();
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

// ---------------------------------------------------------------------------------------
// `OrderRequest.order_type` became `vike_model::OrderType`: every word ever written stays
// readable and re-writes to the very same bytes (no migration, decision 0117)
// ---------------------------------------------------------------------------------------

/// A `Snap` whose registry holds an order ADOPTED from a venue under the venue's own word
/// (`"stop_market"`, not one of ours): it reads back as `OrderType::Unrecognized` with the text
/// verbatim, the engines' `state_hash` is unchanged by the round trip, and writing the read-back
/// snapshot again yields the identical frame.
#[test]
fn a_snap_holding_an_adopted_venue_order_type_re_serializes_byte_identically() {
    let report = vike_model::OrderStatusReport {
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        venue_order_id: "v-9".into(),
        client_order_id: Some("adopted-1".into()),
        side: -1,
        order_type: vike_model::OrderType::Unrecognized("stop_market".into()),
        qty: 2.0,
        filled_qty: 0.0,
        avg_px: 0.0,
        status: vike_model::OrderStatus::Accepted,
        ts: 5,
    };
    let adopted = vike_model::ManagedOrder::from_status_report(&report).unwrap();
    let mut engines = snap_engines();
    engines[0].registry.push(("adopted-1".into(), adopted));
    let hash = vike_exec::state_hash(&engines);

    let dir = tmp_dir("snap-unrecognized-order-type");
    let mut j = CommandJournal::open(&dir, small_cfg()).unwrap();
    j.append_snap(1_000, &engines, "sess", 7, 9, SnapBooks::default(), hash).unwrap();
    drop(j);
    let written = framed_payloads(&dir);
    assert!(written[0].contains(r#""order_type":"stop_market""#), "{written:?}");

    let back = match CommandJournal::read_all(&dir).unwrap().remove(0) {
        JournalRecord::Snap { engines, .. } => engines,
        other => panic!("expected Snap, got {other:?}"),
    };
    let order = &back[0].registry.last().unwrap().1.request.order_type;
    assert_eq!(order, &vike_model::OrderType::Unrecognized("stop_market".into()));
    assert_eq!(vike_exec::state_hash(&back), hash, "the determinism fence does not move");

    let again = tmp_dir("snap-unrecognized-order-type-again");
    let mut j = CommandJournal::open(&again, small_cfg()).unwrap();
    j.append_snap(1_000, &back, "sess", 7, 9, SnapBooks::default(), hash).unwrap();
    drop(j);
    assert_eq!(framed_payloads(&again), written, "the re-written frame is byte-identical");
}

/// A `MintedSubmit` whose request carries a word that is not EXACTLY ours (`"LIMIT"`, as a
/// stringly writer could have stored it) reads back as `Unrecognized("LIMIT")` and re-writes to
/// the identical frame — the stored door never normalises.
#[test]
fn a_minted_submit_with_an_upper_case_order_type_re_serializes_byte_identically() {
    let minted = serde_json::json!({
        "MintedSubmit": {
            "seq": 0,
            "now_ms": 1_000,
            "req": { "client_order_id": "c-1", "venue": "sim", "symbol": "BTCUSDT",
                     "side": 1, "qty": 1.0, "order_type": "LIMIT", "price": 2.0 }
        }
    });
    let req = match serde_json::from_value::<JournalRecord>(minted).unwrap() {
        JournalRecord::MintedSubmit { req, .. } => req,
        other => panic!("expected MintedSubmit, got {other:?}"),
    };
    assert_eq!(req.order_type, vike_model::OrderType::Unrecognized("LIMIT".into()));

    let dir = tmp_dir("minted-unrecognized-order-type");
    let mut j = CommandJournal::open(&dir, small_cfg()).unwrap();
    j.append_minted_submit(1_000, &req, None).unwrap();
    drop(j);
    let written = framed_payloads(&dir);
    assert!(written[0].contains(r#""order_type":"LIMIT""#), "{written:?}");

    let back = match CommandJournal::read_all(&dir).unwrap().remove(0) {
        JournalRecord::MintedSubmit { req, .. } => req,
        other => panic!("expected MintedSubmit, got {other:?}"),
    };
    assert_eq!(back, req);
    let again = tmp_dir("minted-unrecognized-order-type-again");
    let mut j = CommandJournal::open(&again, small_cfg()).unwrap();
    j.append_minted_submit(1_000, &back, None).unwrap();
    drop(j);
    assert_eq!(framed_payloads(&again), written, "the re-written frame is byte-identical");
}

// ---------------------------------------------------------------------------------------
// v17: `EngineSnapshot::route_key` and `SnapConditional::route_key` — WHICH ACCOUNT a Snap's
// engine and armed conditional belong to, so a boot restore can put them back on the right book
// ---------------------------------------------------------------------------------------

/// A v16 directory (no `route_key` on any engine or arm) still READS under this v17 build, and
/// every absent key reads back `None` — the additive-step contract `MIN_READABLE_VERSION` rests
/// on (decision 0117: no migration; the old journal is simply read).
#[test]
fn a_v16_snap_without_route_keys_reads_back_with_none() {
    let dir = tmp_dir("snap-v16-route-keys");
    let books = vec![SnapConditional {
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
        route_key: None,
    }];
    let mut j = CommandJournal::open(&dir, small_cfg()).unwrap();
    j.append_snap(
        1_000,
        &snap_engines(),
        "sess",
        7,
        9,
        SnapBooks { conditionals: &books, ..SnapBooks::default() },
        0xABCD,
    )
    .unwrap();
    drop(j);
    let written = framed_payloads(&dir);
    assert!(
        !written[0].contains("route_key"),
        "a single-account engine and an arm with no account write NO key: {written:?}"
    );
    // Restamp the header to 16: these are byte-for-byte the frames a v16 writer produced.
    let seg = seg_files(&dir).remove(0);
    let mut bytes = std::fs::read(&seg).unwrap();
    bytes[4..8].copy_from_slice(&16u32.to_le_bytes());
    std::fs::write(&seg, bytes).unwrap();

    let recs = CommandJournal::read_all(&dir).unwrap();
    assert_eq!(recs.len(), 1);
    match &recs[0] {
        JournalRecord::Snap { engines, conditionals, coid_seq, .. } => {
            assert_eq!(engines[0].route_key, None, "an absent engine key reads back None");
            assert_eq!(conditionals[0].route_key, None, "an absent arm key reads back None");
            assert_eq!(*coid_seq, 7, "the rest of the record is untouched");
        }
        other => panic!("expected Snap, got {other:?}"),
    }
}

/// A LABELLED engine (route key `sim#ALT`, not its venue) writes its route key into the `Snap`
/// and reads it back: the fact a two-account boot restore matches a snapshot to its engine by.
#[test]
fn a_v17_snap_carries_a_labelled_engines_route_key() {
    use vike_exec::testing::RecordingClient;
    use vike_exec::{Account, BalanceMode, ExecutionEngine, RiskGate};
    let mut labelled = ExecutionEngine::new(
        Account::new(1.0, "sim", None, BalanceMode::Delta),
        RiskGate::new(vike_model::RiskLimits::new()),
        RecordingClient::default(),
        "sim",
        "BTCUSDT",
    );
    labelled.route_key = "sim#ALT".into();
    let mut engines = snap_engines();
    engines.push(labelled.snapshot_state());

    let dir = tmp_dir("snap-v17-route-keys");
    let mut j = CommandJournal::open(&dir, small_cfg()).unwrap();
    j.append_snap(1_000, &engines, "sess", 7, 9, SnapBooks::default(), 0xABCD).unwrap();
    drop(j);
    match CommandJournal::read_all(&dir).unwrap().remove(0) {
        JournalRecord::Snap { engines, .. } => {
            assert_eq!(engines[0].route_key, None, "the default account writes none");
            assert_eq!(engines[1].route_key.as_deref(), Some("sim#ALT"), "the labelled one does");
        }
        other => panic!("expected Snap, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------------------
// v18: `Ingest::GatewayEvent` and `Snap::gateway_cursors` — a venue event with the ring position
// it held at a gateway daemon, and where each ring stood at checkpoint time
// ---------------------------------------------------------------------------------------

fn cursor(gw_epoch: u64, seq: u64) -> vike_exec::GatewayCursor {
    vike_exec::GatewayCursor { gw_epoch, seq, ts_ms: seq as i64 * 10 }
}

/// A v17 `Snap` (no `gateway_cursors` key) still READS under this v18 build, as EMPTY: the
/// additive-step contract `MIN_READABLE_VERSION` rests on. And a `Snap` written with NO cursors
/// carries no key either, so a run that never receives a gateway event writes the `Snap` bytes it
/// wrote before v18 (only the header version moves).
#[test]
fn a_v17_snap_without_gateway_cursors_reads_back_empty_and_an_empty_one_writes_no_key() {
    let dir = tmp_dir("snap-v17-gateway-cursors");
    let mut j = CommandJournal::open(&dir, small_cfg()).unwrap();
    j.append_snap(1_000, &snap_engines(), "sess", 7, 9, SnapBooks::default(), 0xABCD).unwrap();
    drop(j);
    let written = framed_payloads(&dir);
    assert!(
        !written[0].contains("gateway_cursors"),
        "no cursors, no key: the Snap bytes are the pre-v18 bytes: {written:?}"
    );
    // Restamp the header to 17: byte-for-byte the frame a v17 writer produced.
    let seg = seg_files(&dir).remove(0);
    let mut bytes = std::fs::read(&seg).unwrap();
    bytes[4..8].copy_from_slice(&17u32.to_le_bytes());
    std::fs::write(&seg, bytes).unwrap();
    match CommandJournal::read_all(&dir).unwrap().remove(0) {
        JournalRecord::Snap { gateway_cursors, coid_seq, .. } => {
            assert!(gateway_cursors.is_empty(), "an absent field reads back EMPTY, never an error");
            assert_eq!(coid_seq, 7, "the rest of the record is untouched");
        }
        other => panic!("expected Snap, got {other:?}"),
    }
}

/// The cursors a `Snap` is given come back verbatim and in order, one pair per account.
#[test]
fn a_v18_snap_carries_the_gateway_cursors_it_was_given() {
    let dir = tmp_dir("snap-v18-gateway-cursors");
    let held =
        vec![("binance".to_string(), cursor(7, 12)), ("binance#alt".to_string(), cursor(7, 3))];
    let mut j = CommandJournal::open(&dir, small_cfg()).unwrap();
    let books = SnapBooks { gateway_cursors: &held, ..SnapBooks::default() };
    j.append_snap(1_000, &snap_engines(), "sess", 7, 9, books, 0xABCD).unwrap();
    drop(j);
    match CommandJournal::read_all(&dir).unwrap().remove(0) {
        JournalRecord::Snap { gateway_cursors, .. } => assert_eq!(gateway_cursors, held),
        other => panic!("expected Snap, got {other:?}"),
    }
}

/// A `GatewayEvent` is ONE record: the event and its cursor read back together, and the record is
/// a `Cmd` like every other ingest (so `read_since`, the prune and the materializer see it).
#[test]
fn a_gateway_event_round_trips_as_one_cmd_record() {
    let dir = tmp_dir("gateway-event-cmd");
    let mut j = CommandJournal::open(&dir, small_cfg()).unwrap();
    let msg = Ingest::GatewayEvent(Box::new(vike_exec::GatewayEvent {
        event: vike_model::events::Event::OrderSubmitted(vike_model::events::OrderSubmitted {
            client_order_id: "c1".into(),
            ts: 5,
        }),
        route_key: "binance".into(),
        cursor: cursor(9, 41),
    }));
    j.append_cmd(1_000, &msg).unwrap();
    drop(j);
    match CommandJournal::read_all(&dir).unwrap().remove(0) {
        JournalRecord::Cmd { msg: Ingest::GatewayEvent(g), now_ms: 1_000, .. } => {
            assert_eq!(g.cursor, cursor(9, 41));
            assert_eq!(g.route_key.as_str(), "binance");
            assert!(matches!(g.event, vike_model::events::Event::OrderSubmitted(_)));
        }
        other => panic!("expected a GatewayEvent Cmd, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------------------
// v19: `Command::AdmitSymbol` — an account's engine takes a second symbol on demand, with the
// symbol's own grid in the command, so a replay needs no venue (decision 0173)
// ---------------------------------------------------------------------------------------

/// An `AdmitSymbol` is a `Cmd` like every other ingest: it round-trips as ONE record with its grid
/// and contract value intact, and `check_version` accepts the v19 header it is written under.
/// Purely additive (a new variant of the externally-tagged `Command`), so the floor does not move.
#[test]
fn an_admit_symbol_round_trips_as_one_cmd_record() {
    let dir = tmp_dir("admit-symbol-cmd");
    let mut j = CommandJournal::open(&dir, small_cfg()).unwrap();
    let admission = vike_exec::SymbolAdmission {
        route_key: "binance#alt".into(),
        symbol: "ETHUSDT".into(),
        grid: vike_model::SymbolGrid {
            tick_size: Some(0.01),
            lot_size: Some(0.0001),
            ..vike_model::SymbolGrid::default()
        },
        contract_value: Some(vike_model::ContractValue::linear(10.0)),
        initial_mark: None,
        quote_ccy: None,
        quote_rate: None,
        rate_as_of_ms: None,
        im_requirement: None,
    };
    let msg = Ingest::Command(vike_exec::Command::AdmitSymbol(Box::new(admission.clone())));
    j.append_cmd(1_000, &msg).unwrap();
    drop(j);
    match CommandJournal::read_all(&dir).unwrap().remove(0) {
        JournalRecord::Cmd { msg: Ingest::Command(vike_exec::Command::AdmitSymbol(a)), .. } => {
            assert_eq!(*a, admission, "the facts the replay needs came back verbatim");
        }
        other => panic!("expected an AdmitSymbol Cmd, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------------------
// v20: the quote currency and the rate on `Command::AdmitSymbol` — a foreign-quoted symbol's money
// is the account's through a journaled rate, so a replay needs no venue
// ---------------------------------------------------------------------------------------

/// The three new fields round-trip verbatim, and an admission that names none writes NONE of the
/// keys (so every journal that never trades a foreign-quoted symbol keeps its bytes, which is what
/// lets the header stay at 19 for it).
#[test]
fn a_quoted_admission_round_trips_and_an_unquoted_one_writes_no_new_key() {
    let dir = tmp_dir("admit-symbol-quoted");
    let mut j = CommandJournal::open(&dir, small_cfg()).unwrap();
    let grid = vike_model::SymbolGrid {
        tick_size: Some(0.001),
        lot_size: Some(1.0),
        ..vike_model::SymbolGrid::default()
    };
    let plain = vike_exec::SymbolAdmission {
        route_key: "oanda".into(),
        symbol: "EUR_USD".into(),
        grid: grid.clone(),
        contract_value: None,
        initial_mark: None,
        quote_ccy: None,
        quote_rate: None,
        rate_as_of_ms: None,
        im_requirement: None,
    };
    let quoted = vike_exec::SymbolAdmission {
        symbol: "USD_JPY".into(),
        quote_ccy: Some("JPY".into()),
        quote_rate: Some(1.0 / 150.0),
        rate_as_of_ms: Some(1_000),
        ..plain.clone()
    };
    for a in [&plain, &quoted] {
        let msg = Ingest::Command(vike_exec::Command::AdmitSymbol(Box::new(a.clone())));
        j.append_cmd(1_000, &msg).unwrap();
    }
    drop(j);
    let text = serde_json::to_string(&plain).unwrap();
    assert!(
        !text.contains("quote_") && !text.contains("rate_as_of"),
        "an unquoted admission keeps its pre-change bytes: {text}"
    );
    let back = CommandJournal::read_all(&dir).unwrap();
    for (rec, want) in back.into_iter().zip([plain, quoted]) {
        match rec {
            JournalRecord::Cmd {
                msg: Ingest::Command(vike_exec::Command::AdmitSymbol(a)), ..
            } => assert_eq!(*a, want, "the rate the replay needs came back verbatim"),
            other => panic!("expected an AdmitSymbol Cmd, got {other:?}"),
        }
    }
}
