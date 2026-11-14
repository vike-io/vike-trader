//! The version-compat contract per record: pre-vN frames read back, absent keys stay off the wire.

use super::*;
use crate::format::HEADER;
use crate::frame::fnv1a32;
use crate::testutil::*;
use crate::{Journal, JournalFileConfig};

/// A pre-v14 `MarginCallLiquidate` frame — one with NO `mount_id` key at all — still reads back,
/// as `None`. That is the additive-step contract `MIN_READABLE_VERSION` rests on, and `None` is
/// the correct reading: every such frame predates the per-mount budget latch's ownership stamp.
#[test]
fn a_pre_v14_margin_call_liquidate_frame_reads_back_unowned() {
    let json = serde_json::json!({
        "MarginCallLiquidate": {
            "sequence": 7,
            "now_ms": 1_000,
            "request": { "order_client_id": "", "venue": "sim", "symbol": "BTCUSDT",
                     "side": -1, "qty": 3.0, "order_type": "market" }
        }
    });
    match serde_json::from_value::<JournalRecord>(json).expect("a v13 frame is a valid v14 one") {
        JournalRecord::MarginCallLiquidate { sequence: seq, mount_id, .. } => {
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
    let mut j = Journal::open(&dir, JournalFileConfig::default()).unwrap();
    let req = vike_model::OrderRequest {
        order_client_id: "c-1".into(),
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        side: 1,
        qty: 1.0,
        order_type: vike_model::OrderType::Limit,
        timestamp: 1_000,
        ..Default::default()
    };
    j.append_minted_submit(1_000, &req, Some("sim#ALT")).unwrap();
    j.append_margin_call_liquidate(1_100, &req, Some("maker_a"), Some("sim#ALT")).unwrap();
    drop(j);

    let recs = Journal::read_all(&dir).unwrap();
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
/// written as `null` — `vike_execution::ReconcileReports::route_key`'s own argument: *"keeping the
/// absent field OFF the wire is what makes a single-account box's journal bytes identical to
/// the ones written before this field existed."*
///
/// Read out of the raw segment rather than through serde, because serde would answer the same
/// for a `null` and for an absent key — which is exactly the difference under test.
#[test]
fn a_default_account_box_writes_no_route_key_key_at_all() {
    let dir = tmp_dir("write-ahead-route-key-absent");
    let config = small_config();
    let mut j = Journal::open(&dir, config).unwrap();
    let req = vike_model::OrderRequest {
        order_client_id: "c-1".into(),
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        side: 1,
        qty: 1.0,
        order_type: vike_model::OrderType::Limit,
        timestamp: 1_000,
        ..Default::default()
    };
    j.append_minted_submit(1_000, &req, None).unwrap();
    j.append_margin_call_liquidate(1_100, &req, None, None).unwrap();
    drop(j);

    let bytes = std::fs::read(segment_files(&dir).remove(0)).unwrap();
    let text = String::from_utf8_lossy(&bytes);
    assert!(text.contains("MintedSubmit"), "the frames really were written: {text:?}");
    assert!(text.contains("MarginCallLiquidate"));
    assert!(
        !text.contains("route_key"),
        "a single-account box's journal BYTES must not move: {text:?}"
    );
}

/// A cancel-then-new amend's mint names the order it `replaces` and round-trips; every other mint writes
/// no such key, so its frame keeps its bytes (and a frame without the key reads back as no replacement).
#[test]
fn a_minted_replacement_names_its_predecessor_and_every_other_mint_writes_no_key() {
    let dir = tmp_dir("minted-replaces");
    let mut j = Journal::open(&dir, small_config()).unwrap();
    let req = vike_model::OrderRequest {
        order_client_id: "c-2".into(),
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        side: 1,
        qty: 1.0,
        order_type: vike_model::OrderType::Limit,
        timestamp: 1_000,
        ..Default::default()
    };
    j.append_minted_submit(1_000, &req, None).unwrap();
    j.append_minted_submit_replacing(1_100, &req, None, Some("c-1")).unwrap();
    drop(j);

    let bytes = std::fs::read(segment_files(&dir).remove(0)).unwrap();
    let text = String::from_utf8_lossy(&bytes);
    assert_eq!(text.matches("replaces").count(), 1, "only the amend's mint carries it: {text:?}");
    let replaces: Vec<Option<String>> = Journal::read_all(&dir)
        .unwrap()
        .into_iter()
        .map(|r| match r {
            JournalRecord::MintedSubmit { replaces, .. } => replaces,
            other => panic!("expected MintedSubmit, got {other:?}"),
        })
        .collect();
    assert_eq!(replaces, vec![None, Some("c-1".to_string())]);
}

/// …and a pre-v16 frame — one with no `route_key` key at all — still reads back, as the
/// venue's sole account. The other direction of the same additive claim.
#[test]
fn a_pre_v16_write_ahead_frame_reads_back_as_the_sole_account() {
    let minted = serde_json::json!({
        "MintedSubmit": {
            "sequence": 3,
            "now_ms": 1_000,
            "request": { "order_client_id": "c-1", "venue": "sim", "symbol": "BTCUSDT",
                     "side": 1, "qty": 1.0, "order_type": "limit" }
        }
    });
    match serde_json::from_value::<JournalRecord>(minted).expect("a v15 frame is a valid v16 one") {
        JournalRecord::MintedSubmit { sequence: seq, route_key, .. } => {
            assert_eq!(seq, 3);
            assert_eq!(route_key, None, "an absent key means the venue's sole account");
        }
        other => panic!("expected MintedSubmit, got {other:?}"),
    }
    let liquidation = serde_json::json!({
        "MarginCallLiquidate": {
            "sequence": 4,
            "now_ms": 1_000,
            "request": { "order_client_id": "", "venue": "sim", "symbol": "BTCUSDT",
                     "side": -1, "qty": 3.0, "order_type": "market" },
            "mount_id": null
        }
    });
    match serde_json::from_value::<JournalRecord>(liquidation)
        .expect("a v15 frame is a valid v16 one")
    {
        JournalRecord::MarginCallLiquidate { sequence: seq, route_key, .. } => {
            assert_eq!(seq, 4);
            assert_eq!(route_key, None);
        }
        other => panic!("expected MarginCallLiquidate, got {other:?}"),
    }
}

/// The v6->v7 `Snapshot` compat seam: a pre-v7 `Snapshot` frame carries NO `arm_sequence` field, and this
/// build must read it back as `None` (ABSENT — never conflated with a genuine `Some(0)`), so
/// the restore path knows to fall back to its prune-safe bound. Hand-writes the v6 frame
/// (strip the field from a v7 payload, re-frame, restamp the header to 6) since this build's
/// writer only ever emits v7.
#[test]
fn a_pre_v7_snap_without_arm_sequence_reads_back_as_absent() {
    let dir = tmp_dir("snap-arm-seq-compat");
    let config = small_config();
    let mut j = Journal::open(&dir, config).unwrap();
    j.append_snap(1_000, &snap_engines(), "sess", 7, 9, JournalSnapshotParts::default(), 0xABCD)
        .unwrap();
    drop(j);

    // Rewrite the one frame with `arm_sequence` REMOVED from its JSON payload — byte-for-byte what
    // a v6 writer produced — and restamp the header version to 6.
    let seg = segment_files(&dir).remove(0);
    let bytes = std::fs::read(&seg).unwrap();
    let len = u32::from_le_bytes(bytes[HEADER..HEADER + 4].try_into().unwrap()) as usize;
    let payload = &bytes[HEADER + 8..HEADER + 8 + len];
    let mut v: serde_json::Value = serde_json::from_slice(payload).unwrap();
    assert!(
        v["Snapshot"].as_object_mut().unwrap().remove("arm_sequence").is_some(),
        "the v7 writer stamps arm_sequence"
    );
    // a v6 frame predates `conditionals` too — strip it so the payload is byte-faithful
    assert!(
        v["Snapshot"].as_object_mut().unwrap().remove("order_emulation_arms").is_some(),
        "the v8 writer stamps conditionals"
    );
    let stripped = serde_json::to_vec(&v).unwrap();
    let mut out = bytes[..HEADER].to_vec();
    out[4..8].copy_from_slice(&crate::format::MIN_READABLE_VERSION.to_le_bytes());
    out.extend_from_slice(&(stripped.len() as u32).to_le_bytes());
    out.extend_from_slice(&fnv1a32(&stripped).to_le_bytes());
    out.extend_from_slice(&stripped);
    out.resize(bytes.len(), 0);
    std::fs::write(&seg, out).unwrap();

    let recs = Journal::read_all(&dir).unwrap();
    assert_eq!(recs.len(), 1);
    match &recs[0] {
        JournalRecord::Snapshot {
            arm_sequence: arm_sequence_value,
            order_client_id_sequence,
            order_emulation_arms: conditionals,
            ..
        } => {
            assert_eq!(*arm_sequence_value, None, "absent field reads back as None, never Some(0)");
            assert!(conditionals.is_empty(), "absent books read back EMPTY, never an error");
            assert_eq!(*order_client_id_sequence, 7, "the rest of the record is untouched");
        }
        other => panic!("expected Snapshot, got {other:?}"),
    }
}

/// The v7->v8 `Snapshot` compat seam (emulator PR-3): a pre-v8 `Snapshot` frame carries NO
/// `conditionals` field, and this build must read it back as EMPTY books — a pre-existing
/// journal restores with today's (pre-PR-3) behavior, never an error. Hand-writes the v7
/// frame (strip the field from a v8 payload, re-frame, restamp the header to 7) since this
/// build's writer only ever emits v8.
#[test]
fn a_pre_v8_snap_without_conditionals_reads_back_as_empty_books() {
    let dir = tmp_dir("snap-conditionals-compat");
    let config = small_config();
    let mut j = Journal::open(&dir, config).unwrap();
    j.append_snap(1_000, &snap_engines(), "sess", 7, 9, JournalSnapshotParts::default(), 0xABCD)
        .unwrap();
    drop(j);

    let seg = segment_files(&dir).remove(0);
    let bytes = std::fs::read(&seg).unwrap();
    let len = u32::from_le_bytes(bytes[HEADER..HEADER + 4].try_into().unwrap()) as usize;
    let payload = &bytes[HEADER + 8..HEADER + 8 + len];
    let mut v: serde_json::Value = serde_json::from_slice(payload).unwrap();
    assert!(
        v["Snapshot"].as_object_mut().unwrap().remove("order_emulation_arms").is_some(),
        "the v8 writer stamps conditionals"
    );
    let stripped = serde_json::to_vec(&v).unwrap();
    let mut out = bytes[..HEADER].to_vec();
    out[4..8].copy_from_slice(&crate::format::MIN_READABLE_VERSION.to_le_bytes());
    out.extend_from_slice(&(stripped.len() as u32).to_le_bytes());
    out.extend_from_slice(&fnv1a32(&stripped).to_le_bytes());
    out.extend_from_slice(&stripped);
    out.resize(bytes.len(), 0);
    std::fs::write(&seg, out).unwrap();

    let recs = Journal::read_all(&dir).unwrap();
    assert_eq!(recs.len(), 1);
    match &recs[0] {
        JournalRecord::Snapshot {
            order_emulation_arms: conditionals,
            arm_sequence: arm_sequence_value,
            order_client_id_sequence,
            ..
        } => {
            assert!(conditionals.is_empty(), "absent field reads back as EMPTY books");
            assert_eq!(*arm_sequence_value, Some(9), "the v7-era field is untouched");
            assert_eq!(*order_client_id_sequence, 7, "the rest of the record is untouched");
        }
        other => panic!("expected Snapshot, got {other:?}"),
    }
}

/// The v10->v11 `Snapshot` compat seam (live-runtime OCO/OTO): a pre-v11 `Snapshot` frame carries NO
/// `contingencies` field, and this build must read it back as an EMPTY book — a pre-OCO/OTO
/// journal restores with today's behavior (no contingency state), never an error. Hand-writes
/// the v10 frame (strip the field from a v11 payload, re-frame, restamp the header to 10).
#[test]
fn a_pre_v11_snap_without_contingencies_reads_back_as_empty() {
    let dir = tmp_dir("snap-contingencies-compat");
    let config = small_config();
    let mut j = Journal::open(&dir, config).unwrap();
    j.append_snap(1_000, &snap_engines(), "sess", 7, 9, JournalSnapshotParts::default(), 0xABCD)
        .unwrap();
    drop(j);

    let seg = segment_files(&dir).remove(0);
    let bytes = std::fs::read(&seg).unwrap();
    let len = u32::from_le_bytes(bytes[HEADER..HEADER + 4].try_into().unwrap()) as usize;
    let payload = &bytes[HEADER + 8..HEADER + 8 + len];
    let mut v: serde_json::Value = serde_json::from_slice(payload).unwrap();
    assert!(
        v["Snapshot"].as_object_mut().unwrap().remove("contingencies").is_some(),
        "the v11 writer stamps contingencies"
    );
    let stripped = serde_json::to_vec(&v).unwrap();
    let mut out = bytes[..HEADER].to_vec();
    out[4..8].copy_from_slice(&crate::format::MIN_READABLE_VERSION.to_le_bytes());
    out.extend_from_slice(&(stripped.len() as u32).to_le_bytes());
    out.extend_from_slice(&fnv1a32(&stripped).to_le_bytes());
    out.extend_from_slice(&stripped);
    out.resize(bytes.len(), 0);
    std::fs::write(&seg, out).unwrap();

    let recs = Journal::read_all(&dir).unwrap();
    assert_eq!(recs.len(), 1);
    match &recs[0] {
        JournalRecord::Snapshot {
            contingencies,
            order_emulation_arms: conditionals,
            order_client_id_sequence,
            ..
        } => {
            assert!(contingencies.is_empty(), "absent field reads back as an EMPTY book");
            assert!(conditionals.is_empty(), "the v8-era field is still empty/intact");
            assert_eq!(*order_client_id_sequence, 7, "the rest of the record is untouched");
        }
        other => panic!("expected Snapshot, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------------------
// v15: `AccountState.route_key` — the journal half of the multi-account balance routing fix
// ---------------------------------------------------------------------------------------

/// One `Ingest::Event` carrying a balance snapshot, stamped or not.
fn account_state_ingest(route_key: Option<&str>) -> vike_execution::Ingest {
    vike_execution::Ingest::Event(vike_model::events::Event::AccountState(
        vike_model::events::AccountState {
            venue: "binance".into(),
            balances: vec![("USDT".to_string(), 1234.5)],
            timestamp: 7,
            route_key: route_key.map(Into::into),
        },
    ))
}

/// The framed payload bytes of every record in `dir`'s single segment — what actually reaches
/// the disk, read back through the frame codec rather than re-derived from the types.
fn framed_payloads(dir: &std::path::Path) -> Vec<String> {
    let bytes = std::fs::read(segment_files(dir).remove(0)).unwrap();
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
    let config = small_config();
    let mut j = Journal::open(&dir, config).unwrap();
    j.append_command(1_000, &account_state_ingest(None)).unwrap();
    drop(j);

    assert_eq!(
        framed_payloads(&dir),
        vec![
            concat!(
                r#"{"Command":{"sequence":0,"now_ms":1000,"message":{"Event":{"type":"AccountState","#,
                r#""venue":"binance","balances":[["USDT",1234.5]],"timestamp":7}}}}"#
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
    let config = small_config();
    let mut j = Journal::open(&dir, config).unwrap();
    j.append_command(1_000, &account_state_ingest(Some("binance#ALT"))).unwrap();
    drop(j);

    assert!(
        framed_payloads(&dir)[0].contains(r#""route_key":"binance#ALT""#),
        "got {:?}",
        framed_payloads(&dir)
    );
    match &Journal::read_all(&dir).unwrap()[0] {
        JournalRecord::Command {
            message: vike_execution::Ingest::Event(vike_model::events::Event::AccountState(a)),
            ..
        } => assert_eq!(a.route_key.as_ref().map(|k| k.as_str()), Some("binance#ALT")),
        other => panic!("expected a Command carrying an AccountState, got {other:?}"),
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
    let config = small_config();
    let mut j = Journal::open(&dir, config).unwrap();
    for i in 0..3 {
        j.append_command(1_000 + i as i64, &account_state_ingest(None)).unwrap();
    }
    drop(j);
    let before = framed_payloads(&dir);

    let seg = segment_files(&dir).remove(0);
    let mut bytes = std::fs::read(&seg).unwrap();
    bytes[4..8].copy_from_slice(&crate::format::MIN_READABLE_VERSION.to_le_bytes());
    std::fs::write(&seg, bytes).unwrap();

    assert_eq!(framed_payloads(&dir), before, "restamping the header touches no frame");
    let back = Journal::read_all(&dir).unwrap();
    assert_eq!(back.len(), 3, "a v14 segment of balance snapshots is fully readable");
    for r in &back {
        match r {
            JournalRecord::Command {
                message: vike_execution::Ingest::Event(vike_model::events::Event::AccountState(a)),
                ..
            } => assert_eq!(
                a.route_key, None,
                "a pre-v15 frame means THE venue's sole account, and must read back as that"
            ),
            other => panic!("expected a Command carrying an AccountState, got {other:?}"),
        }
    }
}

// ---------------------------------------------------------------------------------------
// `OrderRequest.order_type` became `vike_model::OrderType`: every word ever written stays
// readable and re-writes to the very same bytes (no migration, decision 0117)
// ---------------------------------------------------------------------------------------

/// A `Snapshot` whose registry holds an order ADOPTED from a venue under the venue's own word
/// (`"stop_market"`, not one of ours): it reads back as `OrderType::Unrecognized` with the text
/// verbatim, the engines' `state_hash` is unchanged by the round trip, and writing the read-back
/// snapshot again yields the identical frame.
#[test]
fn a_snap_holding_an_adopted_venue_order_type_re_serializes_byte_identically() {
    let report = vike_model::OrderStatusReport {
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        venue_order_id: "v-9".into(),
        order_client_id: Some("adopted-1".into()),
        side: -1,
        order_type: vike_model::OrderType::Unrecognized("stop_market".into()),
        qty: 2.0,
        filled_qty: 0.0,
        average_price: 0.0,
        status: vike_model::OrderStatus::Accepted,
        timestamp: 5,
        price: None,
        trigger_price: None,
        trail: None,
        trail_percent: None,
    };
    let adopted = vike_model::OrderRecord::from_status_report(&report).unwrap();
    let mut engines = snap_engines();
    engines[0].registry.push(("adopted-1".into(), adopted));
    let hash = vike_execution::state_hash(&engines);

    let dir = tmp_dir("snap-unrecognized-order-type");
    let mut j = Journal::open(&dir, small_config()).unwrap();
    j.append_snap(1_000, &engines, "sess", 7, 9, JournalSnapshotParts::default(), hash).unwrap();
    drop(j);
    let written = framed_payloads(&dir);
    assert!(written[0].contains(r#""order_type":"stop_market""#), "{written:?}");

    let back = match Journal::read_all(&dir).unwrap().remove(0) {
        JournalRecord::Snapshot { engines, .. } => engines,
        other => panic!("expected Snapshot, got {other:?}"),
    };
    let order = &back[0].registry.last().unwrap().1.request.order_type;
    assert_eq!(order, &vike_model::OrderType::Unrecognized("stop_market".into()));
    assert_eq!(vike_execution::state_hash(&back), hash, "the determinism fence does not move");

    let again = tmp_dir("snap-unrecognized-order-type-again");
    let mut j = Journal::open(&again, small_config()).unwrap();
    j.append_snap(1_000, &back, "sess", 7, 9, JournalSnapshotParts::default(), hash).unwrap();
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
            "sequence": 0,
            "now_ms": 1_000,
            "request": { "order_client_id": "c-1", "venue": "sim", "symbol": "BTCUSDT",
                     "side": 1, "qty": 1.0, "order_type": "LIMIT", "price": 2.0 }
        }
    });
    let req = match serde_json::from_value::<JournalRecord>(minted).unwrap() {
        JournalRecord::MintedSubmit { request: req, .. } => req,
        other => panic!("expected MintedSubmit, got {other:?}"),
    };
    assert_eq!(req.order_type, vike_model::OrderType::Unrecognized("LIMIT".into()));

    let dir = tmp_dir("minted-unrecognized-order-type");
    let mut j = Journal::open(&dir, small_config()).unwrap();
    j.append_minted_submit(1_000, &req, None).unwrap();
    drop(j);
    let written = framed_payloads(&dir);
    assert!(written[0].contains(r#""order_type":"LIMIT""#), "{written:?}");

    let back = match Journal::read_all(&dir).unwrap().remove(0) {
        JournalRecord::MintedSubmit { request: req, .. } => req,
        other => panic!("expected MintedSubmit, got {other:?}"),
    };
    assert_eq!(back, req);
    let again = tmp_dir("minted-unrecognized-order-type-again");
    let mut j = Journal::open(&again, small_config()).unwrap();
    j.append_minted_submit(1_000, &back, None).unwrap();
    drop(j);
    assert_eq!(framed_payloads(&again), written, "the re-written frame is byte-identical");
}

// ---------------------------------------------------------------------------------------
// v17: `EngineSnapshot::route_key` and `JournalSnapshotOrderEmulationArm::route_key` — WHICH ACCOUNT a Snapshot's
// engine and armed conditional belong to, so a boot restore can put them back on the right book
// ---------------------------------------------------------------------------------------

/// A v16 directory (no `route_key` on any engine or arm) still READS under this v17 build, and
/// every absent key reads back `None` — the additive-step contract `MIN_READABLE_VERSION` rests
/// on (decision 0117: no migration; the old journal is simply read).
#[test]
fn a_v16_snap_without_route_keys_reads_back_with_none() {
    let dir = tmp_dir("snap-v16-route-keys");
    let books = vec![JournalSnapshotOrderEmulationArm {
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
    }];
    let mut j = Journal::open(&dir, small_config()).unwrap();
    j.append_snap(
        1_000,
        &snap_engines(),
        "sess",
        7,
        9,
        JournalSnapshotParts { order_emulation_arms: &books, ..JournalSnapshotParts::default() },
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
    let seg = segment_files(&dir).remove(0);
    let mut bytes = std::fs::read(&seg).unwrap();
    bytes[4..8].copy_from_slice(&crate::format::MIN_READABLE_VERSION.to_le_bytes());
    std::fs::write(&seg, bytes).unwrap();

    let recs = Journal::read_all(&dir).unwrap();
    assert_eq!(recs.len(), 1);
    match &recs[0] {
        JournalRecord::Snapshot {
            engines,
            order_emulation_arms: conditionals,
            order_client_id_sequence,
            ..
        } => {
            assert_eq!(engines[0].route_key, None, "an absent engine key reads back None");
            assert_eq!(conditionals[0].route_key, None, "an absent arm key reads back None");
            assert_eq!(*order_client_id_sequence, 7, "the rest of the record is untouched");
        }
        other => panic!("expected Snapshot, got {other:?}"),
    }
}

/// A LABELLED engine (route key `sim#ALT`, not its venue) writes its route key into the `Snapshot`
/// and reads it back: the fact a two-account boot restore matches a snapshot to its engine by.
#[test]
fn a_v17_snap_carries_a_labelled_engines_route_key() {
    use vike_execution::testing::RecordingClient;
    use vike_execution::{Account, BalanceMode, Engine, RiskGate};
    let mut labelled = Engine::new(
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
    let mut j = Journal::open(&dir, small_config()).unwrap();
    j.append_snap(1_000, &engines, "sess", 7, 9, JournalSnapshotParts::default(), 0xABCD).unwrap();
    drop(j);
    match Journal::read_all(&dir).unwrap().remove(0) {
        JournalRecord::Snapshot { engines, .. } => {
            assert_eq!(engines[0].route_key, None, "the default account writes none");
            assert_eq!(engines[1].route_key.as_deref(), Some("sim#ALT"), "the labelled one does");
        }
        other => panic!("expected Snapshot, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------------------
// v18: `Ingest::GatewayEvent` and `Snapshot::gateway_cursors` — a venue event with the ring position
// it held at a gateway daemon, and where each ring stood at checkpoint time
// ---------------------------------------------------------------------------------------

fn cursor(gateway_epoch: u64, seq: u64) -> vike_model::EventRingCursor {
    vike_model::EventRingCursor { gateway_epoch, sequence: seq, timestamp_ms: seq as i64 * 10 }
}

/// A v17 `Snapshot` (no `gateway_cursors` key) still READS under this v18 build, as EMPTY: the
/// additive-step contract `MIN_READABLE_VERSION` rests on. And a `Snapshot` written with NO cursors
/// carries no key either, so a run that never receives a gateway event writes the `Snapshot` bytes it
/// wrote before v18 (only the header version moves).
#[test]
fn a_v17_snap_without_gateway_cursors_reads_back_empty_and_an_empty_one_writes_no_key() {
    let dir = tmp_dir("snap-v17-gateway-cursors");
    let mut j = Journal::open(&dir, small_config()).unwrap();
    j.append_snap(1_000, &snap_engines(), "sess", 7, 9, JournalSnapshotParts::default(), 0xABCD)
        .unwrap();
    drop(j);
    let written = framed_payloads(&dir);
    assert!(
        !written[0].contains("gateway_cursors"),
        "no cursors, no key: the Snapshot bytes are the pre-v18 bytes: {written:?}"
    );
    // Restamp the header to 17: byte-for-byte the frame a v17 writer produced.
    let seg = segment_files(&dir).remove(0);
    let mut bytes = std::fs::read(&seg).unwrap();
    bytes[4..8].copy_from_slice(&crate::format::MIN_READABLE_VERSION.to_le_bytes());
    std::fs::write(&seg, bytes).unwrap();
    match Journal::read_all(&dir).unwrap().remove(0) {
        JournalRecord::Snapshot { gateway_cursors, order_client_id_sequence, .. } => {
            assert!(gateway_cursors.is_empty(), "an absent field reads back EMPTY, never an error");
            assert_eq!(order_client_id_sequence, 7, "the rest of the record is untouched");
        }
        other => panic!("expected Snapshot, got {other:?}"),
    }
}

/// The cursors a `Snapshot` is given come back verbatim and in order, one pair per account.
#[test]
fn a_v18_snap_carries_the_gateway_cursors_it_was_given() {
    let dir = tmp_dir("snap-v18-gateway-cursors");
    let held =
        vec![("binance".to_string(), cursor(7, 12)), ("binance#alt".to_string(), cursor(7, 3))];
    let mut j = Journal::open(&dir, small_config()).unwrap();
    let books = JournalSnapshotParts { gateway_cursors: &held, ..JournalSnapshotParts::default() };
    j.append_snap(1_000, &snap_engines(), "sess", 7, 9, books, 0xABCD).unwrap();
    drop(j);
    match Journal::read_all(&dir).unwrap().remove(0) {
        JournalRecord::Snapshot { gateway_cursors, .. } => assert_eq!(gateway_cursors, held),
        other => panic!("expected Snapshot, got {other:?}"),
    }
}

/// `Snapshot.held_releases` (the orders the live core holds until their release) is ADDITIVE and NOT a
/// format step: a `Snapshot` holding none writes no key (the bytes it always wrote) and one written before
/// the key reads back empty; a held order comes back whole (its route key, its order client id, its release with
/// the price condition); and a build from before the key reads this build's `Snapshot`, the key ignored.
/// What that build then does not do (send the held orders) is the rollback runbook's line: cancel them
/// first.
#[test]
fn a_snap_carries_its_held_releases_and_a_build_from_before_the_key_reads_it_without_them() {
    let dir = tmp_dir("snap-held-releases-none");
    let mut j = Journal::open(&dir, small_config()).unwrap();
    j.append_snap(1_000, &snap_engines(), "sess", 7, 9, JournalSnapshotParts::default(), 0xABCD)
        .unwrap();
    drop(j);
    let written = framed_payloads(&dir);
    assert!(!written[0].contains("held_releases"), "none held, no key: {written:?}");
    match Journal::read_all(&dir).unwrap().remove(0) {
        JournalRecord::Snapshot { held_releases, .. } => assert!(held_releases.is_empty()),
        other => panic!("expected Snapshot, got {other:?}"),
    }

    let held = vec![OrderHeldRelease {
        route_key: "binance#ALT".into(),
        request: vike_model::OrderRequest {
            order_client_id: "s-7".into(),
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            side: 1,
            qty: 0.5,
            order_type: vike_model::OrderType::Limit,
            price: Some(99.0),
            release: Some(vike_model::OrderRelease {
                at: 5_000,
                price: Some(vike_model::OrderReleasePrice {
                    symbol: "BTCUSDT".into(),
                    level: 100.0,
                    side: vike_model::OrderReleasePriceSide::Below,
                    venue: None,
                }),
            }),
            ..Default::default()
        },
    }];
    let dir = tmp_dir("snap-held-releases-one");
    let mut j = Journal::open(&dir, small_config()).unwrap();
    let books = JournalSnapshotParts { held_releases: &held, ..JournalSnapshotParts::default() };
    j.append_snap(1_000, &snap_engines(), "sess", 7, 9, books, 0xABCD).unwrap();
    drop(j);
    match Journal::read_all(&dir).unwrap().remove(0) {
        JournalRecord::Snapshot { held_releases, .. } => assert_eq!(held_releases, held),
        other => panic!("expected Snapshot, got {other:?}"),
    }

    /// `JournalRecord` as the build before the key declared its `Snapshot` (the fields it needs to restore;
    /// serde ignores the rest, `held_releases` with them, exactly as that build's own struct variant did).
    #[derive(Debug, serde::Deserialize)]
    enum BeforeHeldReleases {
        Snapshot { order_client_id_sequence: u64, hash: u64 },
    }
    let payload = framed_payloads(&dir).remove(0);
    assert!(payload.contains("\"held_releases\":[{\"route_key\":\"binance#ALT\""), "{payload}");
    match serde_json::from_str::<BeforeHeldReleases>(&payload).unwrap() {
        BeforeHeldReleases::Snapshot { order_client_id_sequence, hash } => {
            assert_eq!((order_client_id_sequence, hash), (7, 0xABCD))
        }
    }
}

/// A mount row's `latched` flag (the per-mount budget latch) is ADDITIVE and NOT a format step: an
/// unlatched row writes no key (the bytes a build from before the flag wrote, which also read back
/// unlatched), and a latched one comes back latched, beside the unlatched row it was written with.
#[test]
fn a_snap_mount_row_carries_its_latch_and_an_unlatched_one_writes_no_key() {
    let dir = tmp_dir("snap-mount-latched");
    let rows = vec![
        JournalSnapshotMountAttribution {
            mount_id: "free".into(),
            size: 1.0,
            ..JournalSnapshotMountAttribution::default()
        },
        JournalSnapshotMountAttribution {
            mount_id: "spent".into(),
            size: -2.0,
            latched: true,
            ..JournalSnapshotMountAttribution::default()
        },
    ];
    let mut j = Journal::open(&dir, small_config()).unwrap();
    let books =
        JournalSnapshotParts { mount_attribution: &rows, ..JournalSnapshotParts::default() };
    j.append_snap(1_000, &snap_engines(), "sess", 7, 9, books, 0xABCD).unwrap();
    drop(j);
    let written = framed_payloads(&dir);
    assert_eq!(written[0].matches("\"latched\":true").count(), 1, "one latched row: {written:?}");
    assert_eq!(written[0].matches("latched").count(), 1, "the free row writes no key: {written:?}");
    match Journal::read_all(&dir).unwrap().remove(0) {
        JournalRecord::Snapshot { mount_attribution: mount_attribute, .. } => {
            assert_eq!(mount_attribute, rows)
        }
        other => panic!("expected Snapshot, got {other:?}"),
    }
}

/// A mount row's `loss_base` (the loss budget's base after an operator's un-latch, decision 0239) is ADDITIVE and
/// NOT a format step, the `latched` flag's twin: a row that was never un-latched writes no key (the bytes a build
/// from before the key wrote), one that was comes back with its base beside the row it was written with, a row
/// written BEFORE the key reads back `0.0`, and a build from before the key (the v0.1.97 row, copied) ignores it.
#[test]
fn a_snap_mount_row_carries_its_loss_base_and_a_never_unlatched_one_writes_no_key() {
    let dir = tmp_dir("snap-mount-loss-base");
    let rows = vec![
        JournalSnapshotMountAttribution {
            mount_id: "free".into(),
            size: 1.0,
            ..JournalSnapshotMountAttribution::default()
        },
        JournalSnapshotMountAttribution {
            mount_id: "unlatched".into(),
            realized_pnl: -62.5,
            fees_paid: 1.5,
            loss_base: -64.0,
            ..JournalSnapshotMountAttribution::default()
        },
    ];
    let mut j = Journal::open(&dir, small_config()).unwrap();
    let books =
        JournalSnapshotParts { mount_attribution: &rows, ..JournalSnapshotParts::default() };
    j.append_snap(1_000, &snap_engines(), "sess", 7, 9, books, 0xABCD).unwrap();
    drop(j);
    let written = framed_payloads(&dir);
    assert_eq!(written[0].matches("loss_base").count(), 1, "only the un-latched row: {written:?}");
    assert!(written[0].contains("\"loss_base\":-64.0"), "{written:?}");
    match Journal::read_all(&dir).unwrap().remove(0) {
        JournalRecord::Snapshot { mount_attribution: mount_attribute, .. } => {
            assert_eq!(mount_attribute, rows)
        }
        other => panic!("expected Snapshot, got {other:?}"),
    }

    // A row written before the key has none: it reads back as the base every mount always had.
    let old: JournalSnapshotMountAttribution = serde_json::from_str(
        r#"{"mount_id":"m","size":1.0,"average_price":100.0,"realized_pnl":-3.0,"fees_paid":0.5}"#,
    )
    .unwrap();
    assert_eq!(old.loss_base, 0.0);
    assert!(!old.latched);

    // …and a build from before the key reads what this one writes, with the key ignored.
    /// `JournalSnapshotMountAttribution` exactly as v0.1.97 declared it (the row had `latched` and not `loss_base`).
    #[derive(Debug, serde::Deserialize)]
    struct V0197SnapMountAttribute {
        mount_id: String,
        #[serde(default)]
        realized_pnl: f64,
        #[serde(default)]
        fees_paid: f64,
        #[serde(default)]
        latched: bool,
    }
    let bytes = serde_json::to_string(&rows[1]).unwrap();
    let back: V0197SnapMountAttribute = serde_json::from_str(&bytes).unwrap();
    assert_eq!(
        (back.mount_id.as_str(), back.realized_pnl, back.fees_paid),
        ("unlatched", -62.5, 1.5)
    );
    assert!(
        !back.latched,
        "the older build judges the cumulative loss and latches it again if it is past"
    );
}

/// A mount row's `venue` and `symbol` (the row of a leg, not of the mount's own symbol) are ADDITIVE and NOT a format
/// step, `loss_base`'s twins: the own row writes no key, so a mount with no legs writes the bytes a build from before
/// the key wrote; a row written before the key reads back as the own row; and that build reads what this one writes,
/// with the keys ignored (the v0.1.97 row, copied).
#[test]
fn a_leg_row_carries_its_key_and_a_mounts_own_row_writes_none() {
    let dir = tmp_dir("snap-mount-leg");
    let own = JournalSnapshotMountAttribution {
        mount_id: "m".into(),
        size: 1.0,
        ..JournalSnapshotMountAttribution::default()
    };
    let leg = JournalSnapshotMountAttribution {
        mount_id: "m".into(),
        size: -15.0,
        average_price: 3_000.0,
        venue: Some("okx".into()),
        symbol: Some("ETHUSDT".into()),
        ..JournalSnapshotMountAttribution::default()
    };
    let rows = vec![leg.clone(), own.clone()];
    let mut j = Journal::open(&dir, small_config()).unwrap();
    let books =
        JournalSnapshotParts { mount_attribution: &rows, ..JournalSnapshotParts::default() };
    j.append_snap(1_000, &snap_engines(), "sess", 7, 9, books, 0xABCD).unwrap();
    drop(j);
    match Journal::read_all(&dir).unwrap().remove(0) {
        JournalRecord::Snapshot { mount_attribution: mount_attribute, .. } => {
            assert_eq!(mount_attribute, rows)
        }
        other => panic!("expected Snapshot, got {other:?}"),
    }

    let old =
        r#"{"mount_id":"m","size":1.0,"average_price":0.0,"realized_pnl":0.0,"fees_paid":0.0}"#;
    assert_eq!(serde_json::to_string(&own).unwrap(), old, "the own row's bytes do not move");
    assert_eq!(
        serde_json::from_str::<JournalSnapshotMountAttribution>(old).unwrap(),
        own,
        "an old row is the own"
    );

    /// `JournalSnapshotMountAttribution` exactly as v0.1.97 declared it.
    #[derive(Debug, serde::Deserialize)]
    struct V0197SnapMountAttribute {
        mount_id: String,
        #[serde(default)]
        size: f64,
    }
    let back: V0197SnapMountAttribute =
        serde_json::from_str(&serde_json::to_string(&leg).unwrap()).unwrap();
    assert_eq!((back.mount_id.as_str(), back.size), ("m", -15.0));
}

/// A `GatewayEvent` is ONE record: the event and its cursor read back together, and the record is
/// a `Command` like every other ingest (so `read_since`, the prune and the materializer see it).
#[test]
fn a_gateway_event_round_trips_as_one_command_record() {
    let dir = tmp_dir("gateway-event-cmd");
    let mut j = Journal::open(&dir, small_config()).unwrap();
    let msg = Ingest::GatewayEvent(Box::new(vike_execution::GatewayEvent {
        event: vike_model::events::Event::OrderSubmitted(vike_model::events::OrderSubmitted {
            order_client_id: "c1".into(),
            timestamp: 5,
        }),
        route_key: "binance".into(),
        cursor: cursor(9, 41),
    }));
    j.append_command(1_000, &msg).unwrap();
    drop(j);
    match Journal::read_all(&dir).unwrap().remove(0) {
        JournalRecord::Command { message: Ingest::GatewayEvent(g), now_ms: 1_000, .. } => {
            assert_eq!(g.cursor, cursor(9, 41));
            assert_eq!(g.route_key.as_str(), "binance");
            assert!(matches!(g.event, vike_model::events::Event::OrderSubmitted(_)));
        }
        other => panic!("expected a GatewayEvent Command, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------------------
// v19: `Command::AdmitSymbol` — an account's engine takes a second symbol on demand, with the
// symbol's own grid in the command, so a replay needs no venue (decision 0173)
// ---------------------------------------------------------------------------------------

/// An `AdmitSymbol` is a `Command` like every other ingest: it round-trips as ONE record with its grid
/// and contract value intact, and `check_version` accepts the v19 header it is written under.
/// Purely additive (a new variant of the externally-tagged `Command`), so the floor does not move.
#[test]
fn an_admit_symbol_round_trips_as_one_command_record() {
    let dir = tmp_dir("admit-symbol-cmd");
    let mut j = Journal::open(&dir, small_config()).unwrap();
    let admission = vike_execution::SymbolAdmission {
        route_key: "binance#alt".into(),
        symbol: "ETHUSDT".into(),
        grid: vike_model::SymbolGrid {
            tick_size: Some(0.01),
            lot_size: Some(0.0001),
            ..vike_model::SymbolGrid::default()
        },
        contract_value: Some(vike_model::ContractValue::linear(10.0)),
        initial_mark: None,
        initial_mark_ms: None,
        quote_currency: None,
        quote_rate: None,
        rate_as_of_ms: None,
        initial_margin_requirement: None,
        release: false,
        margin_mode: None,
    };
    let msg = Ingest::Command(vike_execution::Command::AdmitSymbol(Box::new(admission.clone())));
    j.append_command(1_000, &msg).unwrap();
    drop(j);
    match Journal::read_all(&dir).unwrap().remove(0) {
        JournalRecord::Command {
            message: Ingest::Command(vike_execution::Command::AdmitSymbol(a)),
            ..
        } => {
            assert_eq!(*a, admission, "the facts the replay needs came back verbatim");
        }
        other => panic!("expected an AdmitSymbol Command, got {other:?}"),
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
    let mut j = Journal::open(&dir, small_config()).unwrap();
    let grid = vike_model::SymbolGrid {
        tick_size: Some(0.001),
        lot_size: Some(1.0),
        ..vike_model::SymbolGrid::default()
    };
    let plain = vike_execution::SymbolAdmission {
        route_key: "oanda".into(),
        symbol: "EUR_USD".into(),
        grid: grid.clone(),
        contract_value: None,
        initial_mark: None,
        initial_mark_ms: None,
        quote_currency: None,
        quote_rate: None,
        rate_as_of_ms: None,
        initial_margin_requirement: None,
        release: false,
        margin_mode: None,
    };
    let quoted = vike_execution::SymbolAdmission {
        symbol: "USD_JPY".into(),
        quote_currency: Some("JPY".into()),
        quote_rate: Some(1.0 / 150.0),
        rate_as_of_ms: Some(1_000),
        ..plain.clone()
    };
    for a in [&plain, &quoted] {
        let msg = Ingest::Command(vike_execution::Command::AdmitSymbol(Box::new(a.clone())));
        j.append_command(1_000, &msg).unwrap();
    }
    drop(j);
    let text = serde_json::to_string(&plain).unwrap();
    assert!(
        !text.contains("quote_") && !text.contains("rate_as_of"),
        "an unquoted admission keeps its pre-change bytes: {text}"
    );
    let back = Journal::read_all(&dir).unwrap();
    for (rec, want) in back.into_iter().zip([plain, quoted]) {
        match rec {
            JournalRecord::Command {
                message: Ingest::Command(vike_execution::Command::AdmitSymbol(a)),
                ..
            } => assert_eq!(*a, want, "the rate the replay needs came back verbatim"),
            other => panic!("expected an AdmitSymbol Command, got {other:?}"),
        }
    }
}

// ---------------------------------------------------------------------------------------
// v21: `Command::SetAccountTradingState` — ONE account's engine halted, reduced or resumed on its own
// ---------------------------------------------------------------------------------------

/// The record is a `Command` like every other command: it round-trips as ONE record, route key and state
/// verbatim, and a node-wide `SetTradingState` keeps its bytes (so a journal that never uses the new
/// variant is unchanged, which is what keeps its header below 21).
#[test]
fn a_set_account_trading_state_round_trips_as_one_command_record() {
    let dir = tmp_dir("set-account-trading-state-cmd");
    let mut j = Journal::open(&dir, small_config()).unwrap();
    let want = ("binance#alt".to_string(), vike_model::TradingState::Halted);
    let msg = Ingest::Command(vike_execution::Command::SetAccountTradingState {
        route_key: want.0.clone(),
        state: want.1,
    });
    j.append_command(1_000, &msg).unwrap();
    drop(j);
    let mut back = Journal::read_all(&dir).unwrap();
    assert_eq!(back.len(), 1);
    match back.remove(0) {
        JournalRecord::Command {
            message:
                Ingest::Command(vike_execution::Command::SetAccountTradingState { route_key, state }),
            ..
        } => assert_eq!((route_key, state), want, "the record came back verbatim"),
        other => panic!("expected a SetAccountTradingState Command, got {other:?}"),
    }
    // The bytes of the records that existed before v21 are EXACTLY what they were (an exact compare: the
    // old `!contains("Account")` could not fail for a changed representation), and the two new variants
    // have the shape a v21 reader parses.
    let bytes = |c: vike_execution::Command| serde_json::to_string(&Ingest::Command(c)).unwrap();
    assert_eq!(
        bytes(vike_execution::Command::SetTradingState(vike_model::TradingState::Active)),
        r#"{"Command":{"SetTradingState":"Active"}}"#,
        "a node-wide state keeps its bytes"
    );
    assert_eq!(
        bytes(vike_execution::Command::SetAccountTradingState {
            route_key: "binance#alt".into(),
            state: vike_model::TradingState::Halted,
        }),
        r#"{"Command":{"SetAccountTradingState":{"route_key":"binance#alt","state":"Halted"}}}"#
    );
    assert_eq!(
        bytes(vike_execution::Command::HoldAccount {
            route_key: "binance#PERP".into(),
            hold: true
        }),
        r#"{"Command":{"HoldAccount":{"route_key":"binance#PERP","hold":true}}}"#
    );
}

// ---------------------------------------------------------------------------------------
// `OrderEmulationRecord::reduce_only` — an operator's CORE-HELD reduce-only stop. NOT a format step
// (`VERSION` stays 21, the rollback door stays open): the proof is below, not a promise.
// ---------------------------------------------------------------------------------------

/// A stop arm with the given flag, on a venue with no native stop.
fn core_held_stop(reduce_only: bool) -> OrderEmulationRecord {
    OrderEmulationRecord {
        venue: "fxcm".into(),
        symbol: "EUR/USD".into(),
        side: -1,
        qty: 2.0,
        price: Some(1.05),
        trail: None,
        extreme: None,
        trigger_by: None,
        reduce_only,
        kind_terms: None,
    }
}

/// **BYTE IDENTITY, and the flag really travels.** A plain arm writes the bytes it always wrote (a
/// LITERAL, not derived from the type, so the type and the expectation cannot move together), a
/// reduce-only arm writes the one new key, and both read back equal to what was written.
#[test]
fn a_plain_arm_writes_no_reduce_only_key_and_a_reduce_only_arm_round_trips_it() {
    let dir = tmp_dir("conditional-reduce-only-bytes");
    let mut j = Journal::open(&dir, small_config()).unwrap();
    j.append_conditional_armed(1_000, "s0a0", &core_held_stop(false)).unwrap();
    j.append_conditional_armed(1_001, "s0a1", &core_held_stop(true)).unwrap();
    drop(j);

    let written = framed_payloads(&dir);
    assert_eq!(
        written[0],
        concat!(
            r#"{"OrderEmulationArmed":{"sequence":0,"now_ms":1000,"arm_id":"s0a0","resolved":{"#,
            r#""venue":"fxcm","symbol":"EUR/USD","side":-1,"qty":2.0,"price":1.05,"#,
            r#""trail":null,"extreme":null}}}"#
        ),
        "a plain arm keeps its pre-field bytes"
    );
    assert!(written[1].ends_with(r#""extreme":null,"reduce_only":true}}}"#), "{}", written[1]);

    let back = Journal::read_all(&dir).unwrap();
    match (&back[0], &back[1]) {
        (
            JournalRecord::OrderEmulationArmed { resolved: plain, .. },
            JournalRecord::OrderEmulationArmed { resolved: flagged, .. },
        ) => {
            assert_eq!(plain, &core_held_stop(false));
            assert_eq!(flagged, &core_held_stop(true), "the flag survives the frame");
        }
        other => panic!("expected two OrderEmulationArmed records, got {other:?}"),
    }
}

/// A frame written BEFORE the field (no key at all) reads back as a plain arm.
#[test]
fn a_pre_field_conditional_frame_reads_back_as_a_plain_arm() {
    let frame = serde_json::json!({
        "OrderEmulationArmed": {
            "sequence": 4, "now_ms": 1_000, "arm_id": "s0a0",
            "resolved": { "venue": "fxcm", "symbol": "EUR/USD", "side": -1, "qty": 2.0,
                          "price": 1.05, "trail": null, "extreme": null }
        }
    });
    match serde_json::from_value::<JournalRecord>(frame).expect("an old frame is a valid new one") {
        JournalRecord::OrderEmulationArmed { resolved, .. } => {
            assert!(!resolved.reduce_only, "an absent key is a plain arm");
        }
        other => panic!("expected OrderEmulationArmed, got {other:?}"),
    }
}

/// **THE OLD READER IGNORES THE KEY: how this was proved.** v0.1.91 reads a frame with
/// `serde_json::from_slice::<JournalRecord>` (`crates/vike-journal/src/frame.rs`'s walk) and neither
/// `OrderEmulationRecord` nor `JournalSnapshotOrderEmulationArm` nor `JournalRecord` takes `deny_unknown_fields`, so
/// serde skips a key it does not know. These are the v0.1.91 shapes COPIED VERBATIM (the struct
/// before the field existed), and they are asked to read what THIS build writes for a reduce-only
/// arm, in both homes: the `OrderEmulationArmed` record and a `JournalSnapshotOrderEmulationArm` of a `Snapshot`.
/// The cost of a rollback is what the field's doc says: the old build restores a plain stop.
#[test]
fn a_build_from_before_the_field_reads_a_reduce_only_arm_as_a_plain_stop() {
    /// `OrderEmulationRecord` exactly as v0.1.91 declared it.
    #[derive(Debug, PartialEq, serde::Deserialize)]
    struct V0191ConditionalRecord {
        venue: String,
        symbol: String,
        side: i32,
        qty: f64,
        price: Option<f64>,
        trail: Option<f64>,
        extreme: Option<f64>,
        #[serde(default)]
        trigger_by: Option<vike_model::TriggerBy>,
    }
    /// `JournalSnapshotOrderEmulationArm` exactly as v0.1.91 declared it.
    #[derive(Debug, serde::Deserialize)]
    struct V0191SnapConditional {
        arm_id: String,
        terms: V0191ConditionalRecord,
        #[serde(default)]
        route_key: Option<String>,
    }
    /// The one `JournalRecord` variant this test reads, as v0.1.91 declared it.
    #[derive(Debug, serde::Deserialize)]
    enum V0191Record {
        OrderEmulationArmed {
            sequence: u64,
            now_ms: i64,
            arm_id: String,
            resolved: V0191ConditionalRecord,
        },
    }

    let armed = serde_json::to_vec(&JournalRecord::OrderEmulationArmed {
        sequence: 7,
        now_ms: 1_000,
        arm_id: "s0a0".into(),
        resolved: core_held_stop(true),
    })
    .unwrap();
    assert!(String::from_utf8_lossy(&armed).contains(r#""reduce_only":true"#), "the key is there");
    let want = V0191ConditionalRecord {
        venue: "fxcm".into(),
        symbol: "EUR/USD".into(),
        side: -1,
        qty: 2.0,
        price: Some(1.05),
        trail: None,
        extreme: None,
        trigger_by: None,
    };
    match serde_json::from_slice::<V0191Record>(&armed).expect("the old reader must not refuse") {
        V0191Record::OrderEmulationArmed { sequence: seq, now_ms, arm_id, resolved } => {
            assert_eq!((seq, now_ms, arm_id.as_str()), (7, 1_000, "s0a0"));
            assert_eq!(resolved, want, "…and reads the terms of a plain stop");
        }
    }

    let snap = serde_json::to_vec(&JournalSnapshotOrderEmulationArm {
        arm_id: "s0a0".into(),
        terms: core_held_stop(true),
        route_key: None,
    })
    .unwrap();
    assert!(String::from_utf8_lossy(&snap).contains(r#""reduce_only":true"#));
    let old: V0191SnapConditional = serde_json::from_slice(&snap)
        .expect("the old reader must not refuse a Snapshot entry either");
    assert_eq!((old.arm_id.as_str(), old.terms, old.route_key), ("s0a0", want, None));
}

// ---------------------------------------------------------------------------------------
// `OrderEmulationRecord::kind_terms` / `ConditionalIntent::kind_terms` (wave 3 of the order kinds): an
// arm the legacy stop/trailing shape cannot state. NOT a format step (`VERSION` unchanged); an older
// build must SKIP such an arm, never arm it as a plain stop. The proof is below.
// ---------------------------------------------------------------------------------------

/// The arms the emulator rests, legacy and new: `(arm_id, order)`. A sell, at a market near 100.
fn kind_arms() -> Vec<(&'static str, vike_model::OrderResting)> {
    use vike_model::{OrderKind, OrderResting};
    fn arm(kind: OrderKind, f: impl Fn(&mut OrderResting)) -> OrderResting {
        let mut o = OrderResting::new(kind, -1, 2.0);
        f(&mut o);
        o
    }
    vec![
        ("s0a0", arm(OrderKind::Stop, |o| o.price = Some(95.0))),
        ("s0a1", arm(OrderKind::StopLimit, |o| (o.price, o.limit) = (Some(95.0), Some(94.5)))),
        // A take-profit sell ABOVE the market: as a plain stop it would fire at the first check.
        ("s0a2", arm(OrderKind::TakeProfit, |o| o.price = Some(110.0))),
        // Armed before its activation: no extreme yet.
        (
            "s0a3",
            arm(OrderKind::TrailingLimit, |o| {
                (o.trail, o.activation, o.limit_offset) = (Some(2.0), Some(105.0), Some(0.5));
            }),
        ),
        // A trailing stop that has activated past its activation level.
        (
            "s0a4",
            arm(OrderKind::Trailing, |o| {
                (o.trail, o.activation, o.extreme) = (Some(2.0), Some(105.0), Some(107.0));
            }),
        ),
        ("s0a5", arm(OrderKind::Trailing, |o| (o.trail, o.extreme) = (Some(2.0), Some(101.0)))),
    ]
}

fn kind_record(order: &vike_model::OrderResting) -> OrderEmulationRecord {
    OrderEmulationRecord::of_arm("sim".into(), "BTCUSDT".into(), order, None, false)
}

/// **BYTE IDENTITY for the legacy arms, and the new ones round-trip whole.** A plain stop and a plain
/// trailing stop write the bytes they always did (LITERALS) and no `kind_terms`; every other arm
/// writes its terms ONLY under `kind_terms`, and both homes (the `OrderEmulationArmed` record, a `Snapshot`'s
/// `JournalSnapshotOrderEmulationArm`) read back what was written. `working_order` gives back the very order, so an arm
/// armed before its activation restores with its activation and no extreme.
#[test]
fn a_legacy_arm_keeps_its_bytes_and_a_new_kind_arm_round_trips_under_kind_terms() {
    let arms = kind_arms();
    let dir = tmp_dir("conditional-kind-terms");
    let mut j = Journal::open(&dir, small_config()).unwrap();
    for (i, (arm_id, o)) in arms.iter().enumerate() {
        j.append_conditional_armed(1_000 + i as i64, arm_id, &kind_record(o)).unwrap();
    }
    let books: Vec<JournalSnapshotOrderEmulationArm> = arms
        .iter()
        .map(|(arm_id, o)| JournalSnapshotOrderEmulationArm {
            arm_id: (*arm_id).into(),
            terms: kind_record(o),
            route_key: None,
        })
        .collect();
    j.append_snap(
        2_000,
        &snap_engines(),
        "s0",
        0,
        6,
        JournalSnapshotParts { order_emulation_arms: &books, ..JournalSnapshotParts::default() },
        0xABCD,
    )
    .unwrap();
    drop(j);

    let written = framed_payloads(&dir);
    assert_eq!(
        written[0],
        concat!(
            r#"{"OrderEmulationArmed":{"sequence":0,"now_ms":1000,"arm_id":"s0a0","resolved":{"#,
            r#""venue":"sim","symbol":"BTCUSDT","side":-1,"qty":2.0,"price":95.0,"#,
            r#""trail":null,"extreme":null}}}"#
        ),
        "a plain stop keeps its pre-field bytes"
    );
    assert_eq!(
        written[5],
        concat!(
            r#"{"OrderEmulationArmed":{"sequence":5,"now_ms":1005,"arm_id":"s0a5","resolved":{"#,
            r#""venue":"sim","symbol":"BTCUSDT","side":-1,"qty":2.0,"price":null,"#,
            r#""trail":2.0,"extreme":101.0}}}"#
        ),
        "a plain trailing stop keeps its pre-field bytes"
    );
    assert!(
        written[1].contains(concat!(
            r#""price":null,"trail":null,"extreme":null,"#,
            r#""kind_terms":{"kind":"stop_limit","price":95.0,"limit":94.5}"#
        )),
        "a stop-limit's terms live ONLY under kind_terms: {}",
        written[1]
    );
    assert!(
        written[3].contains(concat!(
            r#""kind_terms":{"kind":"trailing_limit","trail":2.0,"#,
            r#""limit_offset":0.5,"activation":105.0}"#
        )),
        "an unactivated trailing-limit writes its activation and no extreme: {}",
        written[3]
    );

    let back = Journal::read_all(&dir).unwrap();
    assert_eq!(back.len(), arms.len() + 1);
    for (rec, (arm_id, o)) in back.iter().zip(&arms) {
        match rec {
            JournalRecord::OrderEmulationArmed { arm_id: id, resolved, .. } => {
                assert_eq!((id.as_str(), resolved), (*arm_id, &kind_record(o)));
            }
            other => panic!("expected OrderEmulationArmed, got {other:?}"),
        }
    }
    match back.last().unwrap() {
        JournalRecord::Snapshot { order_emulation_arms: conditionals, .. } => {
            assert_eq!(conditionals, &books)
        }
        other => panic!("expected Snapshot, got {other:?}"),
    }
    for (arm_id, o) in &arms {
        let r = kind_record(o);
        match &r.kind_terms {
            None => assert!(matches!(*arm_id, "s0a0" | "s0a5"), "{arm_id} is a legacy arm"),
            Some(t) => assert_eq!(t.working_order(o.side, o.size).as_ref(), Some(o), "{arm_id}"),
        }
    }
    let unactivated = kind_record(&arms[3].1).kind_terms.unwrap().working_order(-1, 2.0).unwrap();
    assert_eq!((unactivated.activation, unactivated.extreme), (Some(105.0), None));
}

/// A frame written BEFORE the key (none at all) reads back with no `kind_terms`, in all three homes:
/// the `OrderEmulationArmed` record, a `Snapshot` entry and the write-ahead `ArmConditional` intent.
#[test]
fn a_frame_from_before_kind_terms_reads_back_as_a_legacy_arm() {
    let terms = serde_json::json!({ "venue": "sim", "symbol": "BTCUSDT", "side": -1, "qty": 2.0,
                                    "price": 95.0, "trail": null, "extreme": null });
    let armed = serde_json::json!({ "OrderEmulationArmed": {
        "sequence": 4, "now_ms": 1_000, "arm_id": "s0a0", "resolved": terms.clone() } });
    match serde_json::from_value::<JournalRecord>(armed).expect("an old frame is a valid new one") {
        JournalRecord::OrderEmulationArmed { resolved, .. } => {
            assert_eq!(resolved.kind_terms, None)
        }
        other => panic!("expected OrderEmulationArmed, got {other:?}"),
    }
    let snap: JournalSnapshotOrderEmulationArm =
        serde_json::from_value(serde_json::json!({ "arm_id": "s0a0", "terms": terms })).unwrap();
    assert_eq!(snap.terms.kind_terms, None);
    let cmd = serde_json::json!({ "Command": { "sequence": 5, "now_ms": 1_000, "message": { "Command": { "Order": {
        "ArmConditional": { "venue": "sim", "symbol": "BTCUSDT", "side": -1, "qty": 2.0,
                            "price": 95.0, "trail": null } } } } } });
    match serde_json::from_value::<JournalRecord>(cmd).expect("an old intent is a valid new one") {
        JournalRecord::Command {
            message:
                Ingest::Command(vike_execution::Command::Order(
                    vike_execution::OrderIntent::ArmConditional(c),
                )),
            ..
        } => assert!(c.kind_terms.is_none() && c.price == Some(95.0)),
        other => panic!("expected an ArmConditional Command, got {other:?}"),
    }
}

/// **AN OLDER BUILD RESTORES NO NEW-KIND ARM, and still every legacy one: how this was proved.** The
/// shapes below are the build before the key, COPIED VERBATIM (no struct takes `deny_unknown_fields`,
/// so serde skips `kind_terms`), and they read what THIS build wrote. `older_restore` is that build's
/// `restore_conditional_books` decision (`crates/vike-core/src/runtime/assemble.rs`, frozen in the
/// build it describes): a trailing stop from `trail` + `extreme`, else a stop from `price`, else the arm
/// is SKIPPED with a warning. `older_arm` is its `arm_conditional_as` for the write-ahead intent: a
/// trailing stop from `trail`, else a stop from `price`, else "neither price nor trail set — ignored".
/// So a take-profit or a stop-limit is never armed as a plain stop by a rollback: it is dropped, loudly,
/// and the rollback line (mass-cancel first, 0199's) covers it.
#[test]
fn a_build_from_before_kind_terms_restores_no_new_kind_arm_and_every_legacy_one() {
    /// `OrderEmulationRecord` exactly as the build before the key declared it.
    #[derive(Debug, PartialEq, serde::Deserialize)]
    struct OlderConditionalRecord {
        venue: String,
        symbol: String,
        side: i32,
        qty: f64,
        price: Option<f64>,
        trail: Option<f64>,
        extreme: Option<f64>,
        #[serde(default)]
        trigger_by: Option<vike_model::TriggerBy>,
        #[serde(default)]
        reduce_only: bool,
    }
    /// `JournalSnapshotOrderEmulationArm` exactly as that build declared it.
    #[derive(Debug, PartialEq, serde::Deserialize)]
    struct OlderSnapConditional {
        arm_id: String,
        terms: OlderConditionalRecord,
        #[serde(default)]
        route_key: Option<String>,
    }
    /// `ConditionalIntent` exactly as that build declared it.
    #[derive(Debug, PartialEq, serde::Deserialize)]
    struct OlderConditionalIntent {
        venue: String,
        symbol: String,
        side: i32,
        qty: f64,
        price: Option<f64>,
        trail: Option<f64>,
        #[serde(default)]
        trigger_by: Option<vike_model::TriggerBy>,
    }
    /// The one `JournalRecord` variant this test reads as a record, as that build declared it.
    #[derive(Debug, PartialEq, serde::Deserialize)]
    enum OlderRecord {
        OrderEmulationArmed {
            sequence: u64,
            now_ms: i64,
            arm_id: String,
            resolved: OlderConditionalRecord,
        },
    }
    fn older_restore(t: &OlderConditionalRecord) -> Option<&'static str> {
        match (t.trail, t.extreme, t.price) {
            (Some(_), Some(_), _) => Some("trailing"),
            (_, _, Some(_)) => Some("stop"),
            _ => None,
        }
    }
    fn older_arm(c: &OlderConditionalIntent) -> Option<&'static str> {
        match (c.trail, c.price) {
            (Some(_), _) => Some("trailing"),
            (None, Some(_)) => Some("stop"),
            (None, None) => None,
        }
    }
    // stop, stop_limit, take_profit, trailing_limit (unactivated), trailing (activation), trailing.
    let want = [Some("stop"), None, None, None, None, Some("trailing")];

    let arms = kind_arms();
    let dir = tmp_dir("conditional-kind-terms-older");
    let mut j = Journal::open(&dir, small_config()).unwrap();
    for (arm_id, o) in &arms {
        j.append_conditional_armed(1_000, arm_id, &kind_record(o)).unwrap();
    }
    let books: Vec<JournalSnapshotOrderEmulationArm> = arms
        .iter()
        .map(|(arm_id, o)| JournalSnapshotOrderEmulationArm {
            arm_id: (*arm_id).into(),
            terms: kind_record(o),
            route_key: None,
        })
        .collect();
    j.append_snap(
        2_000,
        &snap_engines(),
        "s0",
        0,
        6,
        JournalSnapshotParts { order_emulation_arms: &books, ..JournalSnapshotParts::default() },
        0xABCD,
    )
    .unwrap();
    for (_, o) in &arms {
        let r = kind_record(o);
        let intent = vike_execution::ConditionalIntent {
            venue: r.venue,
            symbol: r.symbol,
            side: r.side,
            qty: r.qty,
            price: r.price,
            trail: r.trail,
            trigger_by: None,
            kind_terms: r.kind_terms.map(Box::new),
        };
        let msg = Ingest::Command(vike_execution::Command::Order(
            vike_execution::OrderIntent::ArmConditional(intent),
        ));
        j.append_command(3_000, &msg).unwrap();
    }
    drop(j);

    let written = framed_payloads(&dir);
    let n = arms.len();
    for (i, frame) in written[..n].iter().enumerate() {
        let OlderRecord::OrderEmulationArmed { arm_id, resolved, .. } =
            serde_json::from_str(frame).expect("the older reader must not refuse the record");
        assert_eq!((arm_id.as_str(), older_restore(&resolved)), (arms[i].0, want[i]), "{frame}");
    }
    let snap: serde_json::Value = serde_json::from_str(&written[n]).unwrap();
    let older: Vec<OlderSnapConditional> =
        serde_json::from_value(snap["Snapshot"]["order_emulation_arms"].clone())
            .expect("the older reader must not refuse a Snapshot entry either");
    let restored: Vec<_> =
        older.iter().map(|c| (c.arm_id.as_str(), older_restore(&c.terms))).collect();
    let expected: Vec<_> = arms.iter().map(|(id, _)| *id).zip(want).collect();
    assert_eq!(restored, expected, "the Snapshot restores the legacy arms and skips the rest");
    for (i, frame) in written[n + 1..].iter().enumerate() {
        let v: serde_json::Value = serde_json::from_str(frame).unwrap();
        let c: OlderConditionalIntent = serde_json::from_value(
            v["Command"]["message"]["Command"]["Order"]["ArmConditional"].clone(),
        )
        .expect("the older reader must not refuse the intent");
        assert_eq!(older_arm(&c), want[i], "{frame}");
    }
}

/// The attributes wave's arms (decision 0267, question 3): a trailing take-profit, a percent trail and
/// three adjustable stops, the auto trailing stop among them. A sell, at a market near 100.
fn attribute_arms() -> Vec<(&'static str, vike_model::OrderResting)> {
    use vike_model::{OrderKind, OrderResting, OrderStopAdjust, OrderStopAdjustTo};
    fn arm(kind: OrderKind, f: impl Fn(&mut OrderResting)) -> OrderResting {
        let mut o = OrderResting::new(kind, -1, 2.0);
        f(&mut o);
        o
    }
    let adjust = |at, to| Some(Box::new(OrderStopAdjust { at, to }));
    vec![
        // Trails the LOWEST price; a limit offset makes it IB's `TRAIL LIT`.
        (
            "s0a6",
            arm(OrderKind::TrailingTakeProfit, |o| {
                (o.trail, o.extreme, o.limit_offset) = (Some(2.0), Some(99.0), Some(0.5));
            }),
        ),
        // A percent trail with no activation: the shape the legacy keys took for a price trail.
        (
            "s0a7",
            arm(OrderKind::Trailing, |o| (o.trail_percent, o.extreme) = (Some(1.5), Some(101.0))),
        ),
        (
            "s0a8",
            arm(OrderKind::TrailingLimit, |o| {
                (o.trail_percent, o.activation, o.limit_offset) =
                    (Some(1.5), Some(105.0), Some(0.5));
            }),
        ),
        (
            "s0a9",
            arm(OrderKind::Stop, |o| {
                (o.price, o.adjust) =
                    (Some(95.0), adjust(105.0, OrderStopAdjustTo::Stop { stop: 100.0 }));
            }),
        ),
        (
            "s0a10",
            arm(OrderKind::StopLimit, |o| {
                let to = OrderStopAdjustTo::StopLimit { stop: 100.0, limit: 99.5 };
                (o.price, o.limit, o.adjust) = (Some(95.0), Some(94.5), adjust(105.0, to));
            }),
        ),
        // The auto trailing stop: it turns into a percent trailing stop at the profit level.
        (
            "s0a11",
            arm(OrderKind::Stop, |o| {
                let to = OrderStopAdjustTo::Trailing {
                    stop: 100.0,
                    trail: None,
                    trail_percent: Some(2.0),
                };
                (o.price, o.adjust) = (Some(95.0), adjust(108.0, to));
            }),
        ),
    ]
}

/// **Every attributes-wave arm states its terms ONLY under `kind_terms`, and round-trips whole.** The
/// legacy `price`/`trail`/`extreme` are `None` on each; a percent trail writes no `trail`; an adjustable
/// stop's own trigger and limit live inside `adjust`, never in `kind_terms`' `price`/`limit`. Both
/// homes read back what was written, and `working_order` gives back the very order.
#[test]
fn an_attributes_arm_writes_its_terms_only_under_kind_terms_and_round_trips() {
    let arms = attribute_arms();
    let dir = tmp_dir("conditional-attr-terms");
    let mut j = Journal::open(&dir, small_config()).unwrap();
    for (arm_id, o) in &arms {
        j.append_conditional_armed(1_000, arm_id, &kind_record(o)).unwrap();
    }
    let books: Vec<JournalSnapshotOrderEmulationArm> = arms
        .iter()
        .map(|(arm_id, o)| JournalSnapshotOrderEmulationArm {
            arm_id: (*arm_id).into(),
            terms: kind_record(o),
            route_key: None,
        })
        .collect();
    let books_ref =
        JournalSnapshotParts { order_emulation_arms: &books, ..JournalSnapshotParts::default() };
    j.append_snap(2_000, &snap_engines(), "s0", 0, 12, books_ref, 0xABCD).unwrap();
    drop(j);

    let written = framed_payloads(&dir);
    let terms = [
        concat!(
            r#"{"kind":"trailing_take_profit","trail":2.0,"extreme":99.0,"#,
            r#""limit_offset":0.5}"#
        ),
        r#"{"kind":"trailing","extreme":101.0,"trail_percent":1.5}"#,
        r#"{"kind":"trailing_limit","limit_offset":0.5,"activation":105.0,"trail_percent":1.5}"#,
        r#"{"kind":"stop","adjust":{"price":95.0,"at":105.0,"to":{"stop":{"stop":100.0}}}}"#,
        concat!(
            r#"{"kind":"stop_limit","adjust":{"price":95.0,"limit":94.5,"at":105.0,"#,
            r#""to":{"stop_limit":{"stop":100.0,"limit":99.5}}}}"#
        ),
        concat!(
            r#"{"kind":"stop","adjust":{"price":95.0,"at":108.0,"#,
            r#""to":{"trailing":{"stop":100.0,"trail_percent":2.0}}}}"#
        ),
    ];
    for (frame, want) in written.iter().zip(terms) {
        let legacy = r#""price":null,"trail":null,"extreme":null,"kind_terms":"#;
        assert!(frame.contains(&format!("{legacy}{want}")), "{frame}");
    }

    let back = Journal::read_all(&dir).unwrap();
    assert_eq!(back.len(), arms.len() + 1);
    for (rec, (arm_id, o)) in back.iter().zip(&arms) {
        match rec {
            JournalRecord::OrderEmulationArmed { arm_id: id, resolved, .. } => {
                assert_eq!((id.as_str(), resolved), (*arm_id, &kind_record(o)));
            }
            other => panic!("expected OrderEmulationArmed, got {other:?}"),
        }
    }
    match back.last().unwrap() {
        JournalRecord::Snapshot { order_emulation_arms: conditionals, .. } => {
            assert_eq!(conditionals, &books)
        }
        other => panic!("expected Snapshot, got {other:?}"),
    }
    for (arm_id, o) in &arms {
        let t = kind_record(o).kind_terms.unwrap_or_else(|| panic!("{arm_id} has kind terms"));
        assert_eq!(t.working_order(o.side, o.size).as_ref(), Some(o), "{arm_id}");
    }
}

/// **A BUILD FROM BEFORE THE ATTRIBUTES WAVE RESTORES NONE OF ITS ARMS, and still every older one.**
/// The shapes below are that build's (`ConditionalKindTerms` with its seven terms), COPIED VERBATIM;
/// `kind` is read as the word, since that build's `OrderType` reads a word it does not know as
/// `Unrecognized`, which its `working_order` arms nothing of. `holds` is that build's `working_order`
/// and its emulator's `is_holdable` as one verdict (`crates/vike-core/src/emulator.rs`, frozen in the
/// build it describes); `pre_wave_restore` its `from_record` (the `Snapshot` and the `OrderEmulationArmed`);
/// `pre_wave_arm` its `arm_conditional_as` for the write-ahead intent, which seeds a trailing arm's
/// extreme from the mark when it has a trail and neither an extreme nor an activation. So a trailing
/// take-profit, a percent trail and an adjustable stop are never armed as a plain stop or as a trail of
/// no distance by a rollback: each is dropped with that build's warning, and the rollback line
/// (mass-cancel first, 0199's and 0261's) covers it. The build before `kind_terms` reads none of the
/// three legacy keys on any of them, so it skips them too.
#[test]
fn a_build_from_before_the_attributes_wave_restores_none_of_its_arms_and_every_older_one() {
    /// `ConditionalKindTerms` exactly as that build declared it.
    #[derive(Debug, Clone, PartialEq, serde::Deserialize)]
    struct PreWaveKindTerms {
        kind: String,
        #[serde(default)]
        price: Option<f64>,
        #[serde(default)]
        trail: Option<f64>,
        #[serde(default)]
        extreme: Option<f64>,
        #[serde(default)]
        limit: Option<f64>,
        #[serde(default)]
        limit_offset: Option<f64>,
        #[serde(default)]
        activation: Option<f64>,
    }
    /// `OrderEmulationRecord` exactly as that build declared it.
    #[derive(Debug, PartialEq, serde::Deserialize)]
    struct PreWaveConditionalRecord {
        venue: String,
        symbol: String,
        side: i32,
        qty: f64,
        price: Option<f64>,
        trail: Option<f64>,
        extreme: Option<f64>,
        #[serde(default)]
        trigger_by: Option<vike_model::TriggerBy>,
        #[serde(default)]
        reduce_only: bool,
        #[serde(default)]
        kind_terms: Option<PreWaveKindTerms>,
    }
    /// `JournalSnapshotOrderEmulationArm` exactly as that build declared it.
    #[derive(Debug, PartialEq, serde::Deserialize)]
    struct PreWaveSnapConditional {
        arm_id: String,
        terms: PreWaveConditionalRecord,
        #[serde(default)]
        route_key: Option<String>,
    }
    /// `ConditionalIntent` exactly as that build declared it.
    #[derive(Debug, PartialEq, serde::Deserialize)]
    struct PreWaveConditionalIntent {
        venue: String,
        symbol: String,
        side: i32,
        qty: f64,
        price: Option<f64>,
        trail: Option<f64>,
        #[serde(default)]
        trigger_by: Option<vike_model::TriggerBy>,
        #[serde(default)]
        kind_terms: Option<Box<PreWaveKindTerms>>,
    }
    /// The one `JournalRecord` variant this test reads as a record, as that build declared it.
    #[derive(Debug, PartialEq, serde::Deserialize)]
    enum PreWaveRecord {
        OrderEmulationArmed {
            sequence: u64,
            now_ms: i64,
            arm_id: String,
            resolved: PreWaveConditionalRecord,
        },
    }
    fn holds(t: &PreWaveKindTerms) -> bool {
        let trails = t.trail.is_some() && (t.extreme.is_some() || t.activation.is_some());
        match t.kind.as_str() {
            "stop" | "take_profit" => t.price.is_some(),
            "stop_limit" => t.price.is_some() && t.limit.is_some(),
            "trailing" => trails,
            "trailing_limit" => trails && t.limit_offset.is_some(),
            _ => false,
        }
    }
    fn pre_wave_restore(r: &PreWaveConditionalRecord) -> Option<String> {
        match (&r.kind_terms, r.trail, r.extreme, r.price) {
            (Some(t), ..) => holds(t).then(|| t.kind.clone()),
            (None, Some(_), Some(_), _) => Some("trailing".into()),
            (None, _, _, Some(_)) => Some("stop".into()),
            (None, ..) => None,
        }
    }
    fn pre_wave_arm(c: &PreWaveConditionalIntent) -> Option<String> {
        match (&c.kind_terms, c.trail, c.price) {
            (Some(t), ..) => {
                let seeds = t.trail.is_some() && t.extreme.is_none() && t.activation.is_none();
                let t = PreWaveKindTerms {
                    extreme: t.extreme.or(seeds.then_some(100.0)),
                    ..(**t).clone()
                };
                holds(&t).then(|| t.kind.clone())
            }
            (None, Some(_), _) => Some("trailing".into()),
            (None, None, Some(_)) => Some("stop".into()),
            (None, None, None) => None,
        }
    }
    let mut arms = kind_arms();
    let older = arms.len();
    arms.extend(attribute_arms());
    // Every older arm restores as its own kind; every attributes-wave arm is skipped.
    let want: Vec<Option<String>> = ["stop", "stop_limit", "take_profit", "trailing_limit"]
        .into_iter()
        .chain(["trailing", "trailing"])
        .map(|k| Some(k.to_string()))
        .chain(std::iter::repeat_n(None, arms.len() - older))
        .collect();

    let dir = tmp_dir("conditional-attr-terms-pre-wave");
    let mut j = Journal::open(&dir, small_config()).unwrap();
    for (arm_id, o) in &arms {
        j.append_conditional_armed(1_000, arm_id, &kind_record(o)).unwrap();
    }
    let books: Vec<JournalSnapshotOrderEmulationArm> = arms
        .iter()
        .map(|(arm_id, o)| JournalSnapshotOrderEmulationArm {
            arm_id: (*arm_id).into(),
            terms: kind_record(o),
            route_key: None,
        })
        .collect();
    let books_ref =
        JournalSnapshotParts { order_emulation_arms: &books, ..JournalSnapshotParts::default() };
    j.append_snap(2_000, &snap_engines(), "s0", 0, 12, books_ref, 0xABCD).unwrap();
    for (_, o) in &arms {
        let r = kind_record(o);
        let intent = vike_execution::ConditionalIntent {
            venue: r.venue,
            symbol: r.symbol,
            side: r.side,
            qty: r.qty,
            price: r.price,
            trail: r.trail,
            trigger_by: None,
            kind_terms: r.kind_terms.map(Box::new),
        };
        let msg = Ingest::Command(vike_execution::Command::Order(
            vike_execution::OrderIntent::ArmConditional(intent),
        ));
        j.append_command(3_000, &msg).unwrap();
    }
    drop(j);

    let written = framed_payloads(&dir);
    let n = arms.len();
    for (i, frame) in written[..n].iter().enumerate() {
        let PreWaveRecord::OrderEmulationArmed { arm_id, resolved, .. } =
            serde_json::from_str(frame).expect("the older reader must not refuse the record");
        assert_eq!((arm_id.as_str(), pre_wave_restore(&resolved)), (arms[i].0, want[i].clone()));
        if i >= older {
            let legacy = (resolved.price, resolved.trail, resolved.extreme);
            assert_eq!(legacy, (None, None, None), "the build before kind_terms skips it: {frame}");
        }
    }
    let snap: serde_json::Value = serde_json::from_str(&written[n]).unwrap();
    let entries: Vec<PreWaveSnapConditional> =
        serde_json::from_value(snap["Snapshot"]["order_emulation_arms"].clone())
            .expect("the older reader must not refuse a Snapshot entry either");
    let restored: Vec<_> =
        entries.iter().map(|c| (c.arm_id.as_str(), pre_wave_restore(&c.terms))).collect();
    let expected: Vec<_> = arms.iter().map(|(id, _)| *id).zip(want.clone()).collect();
    assert_eq!(restored, expected, "the Snapshot restores the older arms and skips the wave's");
    for (i, frame) in written[n + 1..].iter().enumerate() {
        let v: serde_json::Value = serde_json::from_str(frame).unwrap();
        let c: PreWaveConditionalIntent = serde_json::from_value(
            v["Command"]["message"]["Command"]["Order"]["ArmConditional"].clone(),
        )
        .expect("the older reader must not refuse the intent");
        assert_eq!(pre_wave_arm(&c), want[i], "{frame}");
    }
}

// ---------------------------------------------------------------------------------------
// `SymbolAdmission::release` — an engine GIVES an admitted symbol back (decision 0205). NOT a format step
// (`VERSION` stays 21): the statement is an `AdmitSymbol` record with one more key, and the proof that a
// build from before the key restores the journal without an error is below, not a promise.
// ---------------------------------------------------------------------------------------

/// **BYTE IDENTITY, and the key really travels.** An admission writes the bytes it always wrote (a LITERAL),
/// a release writes the one new key, both round-trip verbatim through a `Command` record.
#[test]
fn a_release_statement_round_trips_and_an_admission_keeps_its_bytes() {
    let dir = tmp_dir("release-statement-cmd");
    let mut j = Journal::open(&dir, small_config()).unwrap();
    let release = vike_execution::SymbolAdmission::releasing("binance#alt", "ETHUSDT");
    j.append_command(
        1_000,
        &Ingest::Command(vike_execution::Command::AdmitSymbol(Box::new(release.clone()))),
    )
    .unwrap();
    drop(j);
    match Journal::read_all(&dir).unwrap().remove(0) {
        JournalRecord::Command {
            message: Ingest::Command(vike_execution::Command::AdmitSymbol(a)),
            ..
        } => {
            assert_eq!(*a, release, "the statement the replay needs came back verbatim");
            assert!(a.release);
        }
        other => panic!("expected an AdmitSymbol Command, got {other:?}"),
    }
    let bytes = |a: &vike_execution::SymbolAdmission| {
        serde_json::to_string(&Ingest::Command(vike_execution::Command::AdmitSymbol(Box::new(
            a.clone(),
        ))))
        .unwrap()
    };
    assert_eq!(
        bytes(&release),
        r#"{"Command":{"AdmitSymbol":{"route_key":"binance#alt","symbol":"ETHUSDT","grid":{},"release":true}}}"#
    );
    let admission = vike_execution::SymbolAdmission {
        grid: vike_model::SymbolGrid {
            tick_size: Some(0.01),
            lot_size: Some(0.0001),
            ..vike_model::SymbolGrid::default()
        },
        release: false,
        ..release
    };
    assert_eq!(
        bytes(&admission),
        r#"{"Command":{"AdmitSymbol":{"route_key":"binance#alt","symbol":"ETHUSDT","grid":{"tick_size":0.01,"lot_size":0.0001}}}}"#,
        "an admission has no `release` key"
    );
}

/// **THE OLD READER IGNORES THE KEY: how this was proved.** v0.1.97 reads a frame with
/// `serde_json::from_slice::<JournalRecord>` (`crates/vike-journal/src/frame.rs`'s walk) and neither
/// `SymbolAdmission` nor `JournalRecord` takes `deny_unknown_fields`, so serde skips the key it does not
/// know. These are the v0.1.97 shapes COPIED VERBATIM (the struct before the field existed), asked to read
/// what THIS build writes for a release. They parse it, find an EMPTY grid, and the engine of that build
/// refuses the record (`grid-incomplete`): the symbol stays admitted there, which is all that build ever did.
/// A new `Command` variant would instead be `JournalCorruptCause::ParseFailed`, and the boot restore then restores
/// NOTHING from the directory (`crates/vike-core/src/replay/boot.rs`): the reason there is no such variant.
#[test]
fn a_build_from_before_the_key_reads_a_release_as_an_admission_with_no_grid() {
    /// `SymbolAdmission` exactly as v0.1.97 declared it.
    #[derive(Debug, serde::Deserialize)]
    struct V0197SymbolAdmission {
        route_key: String,
        symbol: String,
        grid: vike_model::SymbolGrid,
        #[serde(default)]
        contract_value: Option<vike_model::ContractValue>,
        #[serde(default)]
        initial_mark: Option<f64>,
        #[serde(default)]
        quote_currency: Option<String>,
        #[serde(default)]
        quote_rate: Option<f64>,
        #[serde(default)]
        rate_as_of_ms: Option<i64>,
        #[serde(default)]
        initial_margin_requirement: Option<f64>,
    }
    /// The one `Command` variant this test reads, as v0.1.97 declared it.
    #[derive(Debug, serde::Deserialize)]
    enum V0197Command {
        AdmitSymbol(Box<V0197SymbolAdmission>),
    }
    #[derive(Debug, serde::Deserialize)]
    enum V0197Ingest {
        Command(V0197Command),
    }
    #[derive(Debug, serde::Deserialize)]
    enum V0197Record {
        Command { sequence: u64, now_ms: i64, message: V0197Ingest },
    }

    let record = serde_json::to_vec(&JournalRecord::Command {
        sequence: 7,
        now_ms: 1_000,
        message: Ingest::Command(vike_execution::Command::AdmitSymbol(Box::new(
            vike_execution::SymbolAdmission::releasing("binance#alt", "ETHUSDT"),
        ))),
    })
    .unwrap();
    assert!(String::from_utf8_lossy(&record).contains(r#""release":true"#), "the key is there");
    let V0197Record::Command { sequence: seq, now_ms, message: msg } =
        serde_json::from_slice(&record).expect("the old reader must not refuse a release");
    assert_eq!((seq, now_ms), (7, 1_000));
    let V0197Ingest::Command(V0197Command::AdmitSymbol(a)) = msg;
    assert_eq!((a.route_key.as_str(), a.symbol.as_str()), ("binance#alt", "ETHUSDT"));
    assert!(
        a.grid.tick_size.is_none() && a.grid.lot_size.is_none(),
        "an incomplete grid: refused there"
    );
    assert!(
        a.contract_value.is_none()
            && a.initial_mark.is_none()
            && a.quote_currency.is_none()
            && a.quote_rate.is_none()
            && a.rate_as_of_ms.is_none()
            && a.initial_margin_requirement.is_none(),
        "and nothing else that build could fold"
    );
}

// ---------------------------------------------------------------------------------------
// `Event::FeeAdjustment` — a later fee correction of a folded fill (decision 0238): a new EVENT
// tag, carried by the existing `Command` record, so no new record kind and no format step
// ---------------------------------------------------------------------------------------

fn fee_adjustment_ingest() -> Ingest {
    Ingest::Event(vike_model::events::Event::FeeAdjustment(
        vike_model::events::FeeAdjustmentEvent {
            trade_id: "0001f4e8.6540b5a1.01.01".into(),
            order_client_id: "c1".into(),
            venue: "ibkr".into(),
            symbol: "AAPL.SMART.USD".into(),
            last_price: 190.0,
            extra_commission: 1.25,
            commission_asset: "USD".into(),
            timestamp: 9,
        },
    ))
}

/// **An adjustment is ONE `Command` record and reads back verbatim**, so the boot replay re-folds it: the same ingest
/// the live core journaled. The header it is written under is NOT raised: the format version stays what the segment
/// was stamped (no lazy step is owed, see the next test for what an older build does with it).
#[test]
fn a_fee_adjustment_round_trips_as_one_command_record_and_stamps_no_step() {
    let dir = tmp_dir("fee-adjustment-cmd");
    let mut j = Journal::open(&dir, small_config()).unwrap();
    j.append_command(1_000, &fee_adjustment_ingest()).unwrap();
    drop(j);
    assert_eq!(
        Journal::latest_segment_version(&dir).unwrap(),
        Some(crate::format::BASE_VERSION),
        "no step stamped: the segment keeps the version it was opened at"
    );
    match Journal::read_all(&dir).unwrap().remove(0) {
        JournalRecord::Command { message: msg, now_ms: 1_000, .. } => assert_eq!(
            serde_json::to_string(&msg).unwrap(),
            serde_json::to_string(&fee_adjustment_ingest()).unwrap(),
            "the same ingest the core journaled"
        ),
        other => panic!("expected a Command, got {other:?}"),
    }
    let written = framed_payloads(&dir);
    assert!(written[0].contains(r#""type":"FeeAdjustmentEvent""#), "{written:?}");
}

/// **A journal that never carried an adjustment is read and written exactly as before**: a fill, the event beside it,
/// keeps its bytes (no key of the new event appears in it), and a pre-event segment replays unchanged.
#[test]
fn a_journal_without_an_adjustment_writes_and_reads_what_it_always_did() {
    let dir = tmp_dir("fee-adjustment-absent");
    let mut j = Journal::open(&dir, small_config()).unwrap();
    j.append_command(1_000, &account_state_ingest(None)).unwrap();
    drop(j);
    let written = framed_payloads(&dir);
    assert!(
        !written[0].contains("FeeAdjustment") && !written[0].contains("extra_commission"),
        "{written:?}"
    );
    assert_eq!(Journal::read_all(&dir).unwrap().len(), 1);
}

/// **A build from before the event refuses a record that carries one, the way it refuses every unknown tag.** The old
/// build's `Event` has no `FeeAdjustmentEvent` tag, so serde reports an unknown variant for the frame, `walk_frames` ends the
/// segment as `JournalCorruptCause::ParseFailed`, and the boot restore sets the directory aside (a rollback past the first
/// adjustment moves the journal aside first, `crates/vike-journal/CLAUDE.md`). Modelled by the tag any build with a
/// shorter list would not know: the SAME refusal, read through the current `Event`.
#[test]
fn a_build_that_does_not_know_the_tag_refuses_the_frame_as_an_unknown_variant() {
    let record = serde_json::to_string(&JournalRecord::Command {
        sequence: 3,
        now_ms: 1_000,
        message: fee_adjustment_ingest(),
    })
    .unwrap();
    assert!(record.contains(r#""type":"FeeAdjustmentEvent""#));
    serde_json::from_str::<JournalRecord>(&record).expect("this build reads its own tag");
    let from_the_future = record
        .replace(r#""type":"FeeAdjustmentEvent""#, r#""type":"FeeAdjustmentFromAnOlderBuild""#);
    let err = serde_json::from_str::<JournalRecord>(&from_the_future)
        .expect_err("an unknown event tag must not parse");
    assert!(err.to_string().contains("unknown variant"), "{err}");
}

// ---------------------------------------------------------------------------------------
// `JournalSnapshotContingency::contingency_type` (wave 4 of the order kinds): a FORMAT STEP, stamped lazily. A
// bracket leg writes no key, so its `Snapshot` keeps its bytes and its stamp; an OUO leg writes one, which
// an older build would ignore (below), so a `Snapshot` holding one is stamped `ORDER_LIST_VERSION`
// (`format_tests.rs`'s `the_header_rises_to_22_only_for_an_order_list_or_an_ouo_leg`).
// ---------------------------------------------------------------------------------------

/// A leg whose shape gives its type writes the pre-key bytes, and those read back `None`; an OUO leg
/// writes its type, and a build from before the key (its `JournalSnapshotContingency`, copied verbatim) reads
/// that leg back WITHOUT it: as OCO, the misfold the format step's stamp exists to refuse.
#[test]
fn a_build_from_before_the_key_reads_an_ouo_leg_as_oco() {
    /// `JournalSnapshotContingency` exactly as it was declared before the key.
    #[derive(Debug, PartialEq, serde::Deserialize)]
    struct PreKeySnapContingency {
        order_client_id: String,
        parent: Option<String>,
        linked: Vec<String>,
        active: bool,
        #[serde(default)]
        held_request: Option<vike_model::OrderRequest>,
    }
    let leg = |contingency_type| JournalSnapshotContingency {
        order_client_id: "stop".into(),
        parent: Some("entry".into()),
        linked: vec!["t1".into()],
        active: true,
        held_request: None,
        contingency_type,
    };
    let bracket = serde_json::to_string(&leg(None)).unwrap();
    assert_eq!(
        bracket,
        r#"{"order_client_id":"stop","parent":"entry","linked":["t1"],"active":true}"#
    );
    assert_eq!(serde_json::from_str::<JournalSnapshotContingency>(&bracket).unwrap(), leg(None));

    let ouo = serde_json::to_string(&leg(Some(vike_model::ContingencyType::Ouo))).unwrap();
    assert!(ouo.ends_with(r#","contingency_type":"OUO"}"#), "{ouo}");
    let old: PreKeySnapContingency =
        serde_json::from_str(&ouo).expect("the old reader must not refuse");
    assert_eq!(
        old,
        PreKeySnapContingency {
            order_client_id: "stop".into(),
            parent: Some("entry".into()),
            linked: vec!["t1".into()],
            active: true,
            held_request: None,
        }
    );
}

// ---------------------------------------------------------------------------------------
// `OrderReleasePrice::venue` (the attributes wave, decision 0267's 2.13): a FORMAT STEP, stamped lazily.
// A release read on the order's own venue writes no key, so its records keep their bytes and their
// stamp; one naming a venue writes it, which a v23 build cannot PARSE (below), so a journal carrying
// one is stamped `RELEASE_VENUE_VERSION` (`format_tests.rs`'s
// `the_header_rises_to_24_only_for_a_release_read_on_another_venue`).
// ---------------------------------------------------------------------------------------

/// **A v23 build REFUSES a journal holding a release read on another venue, and reads one without it
/// as it always did.** The shapes below are the v23 build's `OrderRelease` and `OrderReleasePrice`, COPIED
/// VERBATIM (both `deny_unknown_fields`), and its `check_version` (`MIN_READABLE_VERSION..=23`). A
/// journal whose held release reads the order's own venue (a write-ahead `Submit` and a `Snapshot`'s held
/// order) keeps `BASE_VERSION` and the v23 bytes, which that build reads. One naming a venue is stamped
/// 24, which that build's header check refuses first; past it, the frame itself would not parse (an
/// unknown field of a `deny_unknown_fields` struct fails the whole record: `JournalCorruptCause::ParseFailed`,
/// a boot restore that restores NOTHING), which is why the step is a stamp and not a silent key.
#[test]
fn a_v23_build_refuses_a_release_read_on_another_venue_and_reads_one_on_its_own_venue() {
    /// `OrderRelease` exactly as the v23 build declared it.
    #[derive(Debug, PartialEq, serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct V23Release {
        at: i64,
        #[serde(default)]
        price: Option<V23PriceCondition>,
    }
    /// `OrderReleasePrice` exactly as the v23 build declared it.
    #[derive(Debug, PartialEq, serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct V23PriceCondition {
        symbol: String,
        level: f64,
        side: vike_model::OrderReleasePriceSide,
    }
    // The v23 build's own range (its floor was 4); since format 30 it refuses every journal this build writes.
    let v23_build_reads = |v: u32| (4..=crate::format::LEG_LIMIT_EXERCISE_VERSION).contains(&v);
    let req = |venue: Option<&str>| vike_model::OrderRequest {
        order_client_id: "c1".into(),
        venue: "ibkr".into(),
        symbol: "AAPL.SMART.USD.STK".into(),
        side: 1,
        qty: 1.0,
        order_type: vike_model::OrderType::Limit,
        price: Some(1.0),
        release: Some(vike_model::OrderRelease {
            at: 5_000,
            price: Some(vike_model::OrderReleasePrice {
                symbol: "BTCUSDT".into(),
                level: 100.0,
                side: vike_model::OrderReleasePriceSide::Below,
                venue: venue.map(Into::into),
            }),
        }),
        ..Default::default()
    };
    // A journal of one write-ahead submit and one `Snapshot` holding it: its stamp, its two frames and the
    // `release` of each.
    let journal = |tag: &str, venue: Option<&str>| {
        let dir = tmp_dir(tag);
        let mut j = Journal::open(&dir, small_config()).unwrap();
        let submit = vike_execution::OrderIntent::Submit(Box::new(req(venue)));
        j.append_command(1_000, &Ingest::Command(vike_execution::Command::Order(submit))).unwrap();
        let held = [OrderHeldRelease { route_key: "ibkr".into(), request: req(venue) }];
        let books =
            JournalSnapshotParts { held_releases: &held, ..JournalSnapshotParts::default() };
        j.append_snap(2_000, &snap_engines(), "sess", 0, 0, books, 1).unwrap();
        drop(j);
        assert_eq!(Journal::read_all(&dir).unwrap().len(), 2, "this build reads it back");
        let seg = std::fs::read(segment_files(&dir).remove(0)).unwrap();
        let stamp = u32::from_le_bytes(seg[4..8].try_into().unwrap());
        let frames = framed_payloads(&dir);
        let cmd: serde_json::Value = serde_json::from_str(&frames[0]).unwrap();
        let snap: serde_json::Value = serde_json::from_str(&frames[1]).unwrap();
        let releases = [
            cmd["Command"]["message"]["Command"]["Order"]["Submit"]["release"].clone(),
            snap["Snapshot"]["held_releases"][0]["request"]["release"].clone(),
        ];
        (stamp, frames, releases)
    };

    let (stamp, frames, releases) = journal("release-venue-own", None);
    assert_eq!(stamp, crate::format::BASE_VERSION, "no venue, no step past the floor");
    assert!(!v23_build_reads(stamp), "format 30: a v23 build refuses the header of every journal");
    let v23_bytes = concat!(
        r#""release":{"at":5000,"#,
        r#""price":{"symbol":"BTCUSDT","level":100.0,"side":"below"}}"#
    );
    for (frame, release) in frames.iter().zip(releases) {
        assert!(frame.contains(v23_bytes), "the v23 bytes: {frame}");
        let old: V23Release = serde_json::from_value(release).expect("a v23 build reads it");
        let condition = V23PriceCondition {
            symbol: "BTCUSDT".into(),
            level: 100.0,
            side: vike_model::OrderReleasePriceSide::Below,
        };
        assert_eq!(old, V23Release { at: 5_000, price: Some(condition) });
    }

    let (stamp, frames, releases) = journal("release-venue-binance", Some("binance"));
    assert_eq!(stamp, crate::format::BASE_VERSION, "the release door (24) is below the floor");
    assert!(!v23_build_reads(stamp), "a v23 build refuses the header");
    for (frame, release) in frames.iter().zip(releases) {
        assert!(frame.contains(r#""side":"below","venue":"binance"}"#), "{frame}");
        let err = serde_json::from_value::<V23Release>(release)
            .expect_err("past the header, a v23 build could not parse the frame either");
        assert!(err.to_string().contains("unknown field `venue`"), "{err}");
    }
}

/// A combo leg priced by the net writes the pre-key bytes; a leg with its own limit writes it, and a
/// build from before the key (its `OrderComboLeg`, copied verbatim) reads that leg back WITHOUT it, so its
/// `build_combo` lowers the spec as a combo MARKET order: the misfold format 23's stamp exists to refuse.
#[test]
fn a_build_from_before_the_key_reads_a_leg_limit_as_no_limit() {
    /// `OrderComboLeg` exactly as it was declared before the key.
    #[derive(Debug, PartialEq, serde::Deserialize)]
    struct PreKeyComboLeg {
        symbol: String,
        ratio: i32,
    }
    let leg = |limit| vike_model::OrderComboLeg { symbol: "A".into(), ratio: 1, limit };
    let by_net = serde_json::to_string(&leg(None)).unwrap();
    assert_eq!(by_net, r#"{"symbol":"A","ratio":1}"#);
    assert_eq!(serde_json::from_str::<vike_model::OrderComboLeg>(&by_net).unwrap(), leg(None));

    let by_leg = serde_json::to_string(&leg(Some(0.03))).unwrap();
    assert_eq!(by_leg, r#"{"symbol":"A","ratio":1,"limit":0.03}"#);
    let old: PreKeyComboLeg =
        serde_json::from_str(&by_leg).expect("the old reader must not refuse");
    assert_eq!(old, PreKeyComboLeg { symbol: "A".into(), ratio: 1 });
}

/// **A combo leg is STRICT (the owner's ruling 14, 2026-11-11):** a record carrying a leg key this build does not know
/// fails to parse, so `walk_frames` ends the segment as `JournalCorruptCause::ParseFailed` and the boot restore sets the directory
/// aside, where the combo would otherwise replay without the key. Every key a build ever wrote into a leg reads.
#[test]
fn a_combo_leg_key_this_build_does_not_know_refuses_the_record() {
    let combo = vike_execution::OrderIntent::Combo(Box::new(vike_model::OrderCombo {
        venue: "deribit".into(),
        side: 1,
        qty: 1.0,
        legs: vec![
            vike_model::OrderComboLeg { symbol: "A".into(), ratio: 1, limit: Some(0.03) },
            vike_model::OrderComboLeg { symbol: "B".into(), ratio: -1, limit: Some(0.01) },
        ],
        net_limit: None,
        time_in_force: vike_model::TimeInForce::Gtc,
        account: None,
        order_client_id: Some("c1".into()),
    }));
    let record = serde_json::to_string(&JournalRecord::Command {
        sequence: 3,
        now_ms: 1_000,
        message: Ingest::Command(vike_execution::Command::Order(combo)),
    })
    .unwrap();
    serde_json::from_str::<JournalRecord>(&record).expect("every leg key a build ever wrote reads");
    let leg = r#"{"symbol":"A","ratio":1,"limit":0.03}"#;
    assert!(record.contains(leg), "{record}");
    let from_the_future =
        record.replace(leg, r#"{"symbol":"A","ratio":1,"limit":0.03,"weight":2}"#);
    let err = serde_json::from_str::<JournalRecord>(&from_the_future)
        .expect_err("an unknown leg key must stop the read, never be dropped");
    assert!(err.to_string().contains("unknown field `weight`"), "{err}");
}

// ---------------------------------------------------------------------------------------
// The order client id v2 (decision 0270): the bracket's group stem is a FORMAT STEP, stamped lazily
// (`format_tests.rs`'s `the_header_rises_to_25_only_for_a_bracket_that_names_its_stem`); the late-retry
// memory and a mount row's strategy code are additive keys and NOT steps.
// ---------------------------------------------------------------------------------------

/// A bracket naming no stem writes the pre-key bytes, and those read back `None`; one naming its stem
/// writes it, and a build from before the key (its `OrderBracket`, copied verbatim: no
/// `deny_unknown_fields`) reads that bracket back WITHOUT it, so its refold mints three leg ids of its
/// own: the misfold format 25's stamp exists to refuse.
#[test]
fn a_build_from_before_the_key_reads_a_bracket_stem_as_no_stem() {
    /// `OrderBracket` exactly as the v24 build declared it.
    #[derive(Debug, PartialEq, serde::Deserialize)]
    struct V24BracketSpec {
        venue: String,
        symbol: String,
        side: i32,
        qty: f64,
        entry_price: Option<f64>,
        stop_loss: f64,
        take_profit: f64,
        #[serde(default)]
        account: Option<String>,
    }
    let spec = |stem: Option<&str>| vike_model::OrderBracket {
        venue: "bybit".into(),
        symbol: "BTCUSDT".into(),
        side: 1,
        qty: 1.0,
        entry_price: None,
        stop_loss: 90.0,
        take_profit: 110.0,
        account: None,
        order_client_id: stem.map(Into::into),
    };
    let v24_bytes = concat!(
        r#"{"venue":"bybit","symbol":"BTCUSDT","side":1,"qty":1.0,"entry_price":null,"#,
        r#""stop_loss":90.0,"take_profit":110.0}"#
    );
    assert_eq!(serde_json::to_string(&spec(None)).unwrap(), v24_bytes, "no stem, no key");
    assert_eq!(serde_json::from_str::<vike_model::OrderBracket>(v24_bytes).unwrap(), spec(None));

    let stem = "C000007k3x9q2m7d1p4";
    let with = serde_json::to_string(&spec(Some(stem))).unwrap();
    assert!(with.ends_with(r#","order_client_id":"C000007k3x9q2m7d1p4"}"#), "{with}");
    assert_eq!(serde_json::from_str::<vike_model::OrderBracket>(&with).unwrap(), spec(Some(stem)));
    let old: V24BracketSpec = serde_json::from_str(&with).expect("the old reader must not refuse");
    assert_eq!(
        old,
        V24BracketSpec {
            venue: "bybit".into(),
            symbol: "BTCUSDT".into(),
            side: 1,
            qty: 1.0,
            entry_price: None,
            stop_loss: 90.0,
            take_profit: 110.0,
            account: None,
        }
    );
}

/// `Snapshot.recent_order_client_ids` (the late-retry memory) is ADDITIVE and NOT a format step: a `Snapshot` holding none
/// writes no key (the bytes it always wrote) and one written before the key reads back empty; the memory
/// comes back whole and in order; and a build from before the key reads this build's `Snapshot`, the key
/// ignored (it restores with no memory, as every restart did before).
#[test]
fn a_snap_carries_its_recent_order_client_ids_and_a_build_from_before_the_key_reads_it_without_them()
 {
    let dir = tmp_dir("snap-recent-order-client-ids-none");
    let mut j = Journal::open(&dir, small_config()).unwrap();
    j.append_snap(1_000, &snap_engines(), "sess", 7, 9, JournalSnapshotParts::default(), 0xABCD)
        .unwrap();
    drop(j);
    let written = framed_payloads(&dir);
    assert!(!written[0].contains("recent_order_client_ids"), "no memory, no key: {written:?}");
    match Journal::read_all(&dir).unwrap().remove(0) {
        JournalRecord::Snapshot { recent_order_client_ids, .. } => {
            assert!(recent_order_client_ids.is_empty())
        }
        other => panic!("expected Snapshot, got {other:?}"),
    }

    let recent = vec![("sess1".to_string(), 900_i64), ("C00000Bq7w2e9r4t1y6FLSL".to_string(), 950)];
    let dir = tmp_dir("snap-recent-order-client-ids-two");
    let mut j = Journal::open(&dir, small_config()).unwrap();
    let books = JournalSnapshotParts {
        recent_order_client_ids: &recent,
        ..JournalSnapshotParts::default()
    };
    j.append_snap(1_000, &snap_engines(), "sess", 7, 9, books, 0xABCD).unwrap();
    drop(j);
    let seg = std::fs::read(segment_files(&dir).remove(0)).unwrap();
    let stamp = u32::from_le_bytes(seg[4..8].try_into().unwrap());
    assert_eq!(stamp, crate::format::BASE_VERSION, "not a step");
    match Journal::read_all(&dir).unwrap().remove(0) {
        JournalRecord::Snapshot { recent_order_client_ids, .. } => {
            assert_eq!(recent_order_client_ids, recent)
        }
        other => panic!("expected Snapshot, got {other:?}"),
    }

    /// `JournalRecord` as the build before the key declared its `Snapshot` (the fields it needs to restore;
    /// serde ignores the rest, `recent_order_client_ids` with them).
    #[derive(Debug, serde::Deserialize)]
    enum BeforeRecentOrderClientIds {
        Snapshot { order_client_id_sequence: u64, hash: u64 },
    }
    let payload = framed_payloads(&dir).remove(0);
    assert!(payload.contains(r#""recent_order_client_ids":[["sess1",900],["#), "{payload}");
    match serde_json::from_str::<BeforeRecentOrderClientIds>(&payload).unwrap() {
        BeforeRecentOrderClientIds::Snapshot { order_client_id_sequence, hash } => {
            assert_eq!((order_client_id_sequence, hash), (7, 0xABCD))
        }
    }
}

/// A mount row's `order_client_id_code` (its strategy code) is ADDITIVE and NOT a format step, `loss_base`'s twin: a
/// row with no code writes no key (the bytes a build from before the key wrote), one with a code comes
/// back with it, and a build from before the key (the v0.1.97 row, copied) reads it with the key ignored.
#[test]
fn a_snap_mount_row_carries_its_order_client_id_code_and_a_codeless_one_writes_no_key() {
    let code = vike_model::orders::client_id::OrderClientIdStrategyCode::new(363).unwrap();
    assert_eq!(code.as_str(), "00A3");
    let own = JournalSnapshotMountAttribution {
        mount_id: "m".into(),
        size: 1.0,
        ..JournalSnapshotMountAttribution::default()
    };
    let coded = JournalSnapshotMountAttribution { order_client_id_code: Some(code), ..own.clone() };
    let old =
        r#"{"mount_id":"m","size":1.0,"average_price":0.0,"realized_pnl":0.0,"fees_paid":0.0}"#;
    assert_eq!(serde_json::to_string(&own).unwrap(), old, "a codeless row's bytes do not move");
    assert_eq!(
        serde_json::from_str::<JournalSnapshotMountAttribution>(old).unwrap(),
        own,
        "an old row has no code"
    );
    let bytes = serde_json::to_string(&coded).unwrap();
    assert!(bytes.ends_with(r#","order_client_id_code":"00A3"}"#), "{bytes}");
    assert_eq!(serde_json::from_str::<JournalSnapshotMountAttribution>(&bytes).unwrap(), coded);

    let dir = tmp_dir("snap-mount-order-client-id-code");
    let rows = vec![coded.clone()];
    let mut j = Journal::open(&dir, small_config()).unwrap();
    let books =
        JournalSnapshotParts { mount_attribution: &rows, ..JournalSnapshotParts::default() };
    j.append_snap(1_000, &snap_engines(), "sess", 7, 9, books, 0xABCD).unwrap();
    drop(j);
    match Journal::read_all(&dir).unwrap().remove(0) {
        JournalRecord::Snapshot { mount_attribution: mount_attribute, .. } => {
            assert_eq!(mount_attribute, rows)
        }
        other => panic!("expected Snapshot, got {other:?}"),
    }

    /// `JournalSnapshotMountAttribution` exactly as v0.1.97 declared it.
    #[derive(Debug, serde::Deserialize)]
    struct V0197SnapMountAttribute {
        mount_id: String,
        #[serde(default)]
        size: f64,
    }
    let back: V0197SnapMountAttribute = serde_json::from_str(&bytes).unwrap();
    assert_eq!((back.mount_id.as_str(), back.size), ("m", 1.0));
}

/// A mount row's `strategy_file` (2b's J30: the file the mount runs and the sha256 of its bytes) rides the
/// `Snapshot` and comes back whole, and a row naming no file writes no key: its bytes are the ones it always had.
#[test]
fn a_snap_mount_row_carries_its_strategy_file_and_a_fileless_one_writes_no_key() {
    let own = JournalSnapshotMountAttribution {
        mount_id: "m".into(),
        size: 1.0,
        ..JournalSnapshotMountAttribution::default()
    };
    let file = JournalStrategyFile { path: "momentum/ema.rhai".into(), sha256: "0f".repeat(32) };
    let filed = JournalSnapshotMountAttribution { strategy_file: Some(file), ..own.clone() };
    let fileless = serde_json::to_string(&own).unwrap();
    assert!(!fileless.contains("strategy_file"), "a fileless row writes no key: {fileless}");
    let bytes = serde_json::to_string(&filed).unwrap();
    let key = format!(
        r#","strategy_file":{{"path":"momentum/ema.rhai","sha256":"{}"}}}}"#,
        "0f".repeat(32)
    );
    assert!(bytes.ends_with(&key), "{bytes}");

    let dir = tmp_dir("snap-mount-strategy-file");
    let rows = vec![filed, own];
    let mut j = Journal::open(&dir, small_config()).unwrap();
    let books =
        JournalSnapshotParts { mount_attribution: &rows, ..JournalSnapshotParts::default() };
    j.append_snap(1_000, &snap_engines(), "sess", 7, 9, books, 0xABCD).unwrap();
    drop(j);
    match Journal::read_all(&dir).unwrap().remove(0) {
        JournalRecord::Snapshot { mount_attribution: mount_attribute, .. } => {
            assert_eq!(mount_attribute, rows)
        }
        other => panic!("expected Snapshot, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------------------
// An order list's trailing leg (decision 0269's W3c): a FORMAT STEP, stamped lazily
// (`format_tests.rs`'s `the_header_rises_to_26_only_for_an_order_list_with_a_trailing_leg`).
// ---------------------------------------------------------------------------------------

/// A leg with no trail writes the pre-key bytes, and those read back `None`; a trailing leg writes its
/// trail, and a build from before the key (its `OrderListLeg`, copied verbatim: no
/// `deny_unknown_fields`) reads that leg back WITHOUT it, so its refold lowers a trailing leg with no
/// distance: the misfold format 26's stamp exists to refuse.
#[test]
fn a_build_from_before_the_key_reads_a_trailing_leg_as_no_trail() {
    /// `OrderListLeg` exactly as the v25 build declared it.
    #[derive(Debug, PartialEq, serde::Deserialize)]
    struct V25OrderListLeg {
        side: i32,
        qty: f64,
        order_type: vike_model::OrderType,
        #[serde(default)]
        price: Option<f64>,
        #[serde(default)]
        trigger_price: Option<f64>,
        #[serde(default)]
        reduce_only: bool,
        #[serde(default)]
        time_in_force: vike_model::TimeInForce,
        #[serde(default)]
        parent: Option<usize>,
        #[serde(default)]
        contingency: Option<vike_model::ContingencyType>,
        #[serde(default)]
        linked: Vec<usize>,
        #[serde(default)]
        order_client_id: Option<String>,
    }
    let leg = |trail| vike_model::OrderListLeg {
        side: -1,
        qty: 1.0,
        order_type: vike_model::OrderType::Trailing,
        price: None,
        trigger_price: Some(105.0),
        trail,
        reduce_only: true,
        time_in_force: vike_model::TimeInForce::Gtc,
        parent: Some(0),
        contingency: None,
        linked: Vec::new(),
        order_client_id: None,
    };
    let bare = serde_json::to_string(&leg(None)).unwrap();
    assert!(!bare.contains(r#""trail":"#), "no trail, no key: {bare}");
    assert_eq!(serde_json::from_str::<vike_model::OrderListLeg>(&bare).unwrap(), leg(None));

    let with = serde_json::to_string(&leg(Some(2.0))).unwrap();
    assert!(with.contains(r#""trigger_price":105.0,"trail":2.0,"#), "{with}");
    assert_eq!(serde_json::from_str::<vike_model::OrderListLeg>(&with).unwrap(), leg(Some(2.0)));
    let old: V25OrderListLeg = serde_json::from_str(&with).expect("the old reader must not refuse");
    assert_eq!(
        old,
        V25OrderListLeg {
            side: -1,
            qty: 1.0,
            order_type: vike_model::OrderType::Trailing,
            price: None,
            trigger_price: Some(105.0),
            reduce_only: true,
            time_in_force: vike_model::TimeInForce::Gtc,
            parent: Some(0),
            contingency: None,
            linked: Vec::new(),
            order_client_id: None,
        },
        "the leg, its trail gone"
    );
}
