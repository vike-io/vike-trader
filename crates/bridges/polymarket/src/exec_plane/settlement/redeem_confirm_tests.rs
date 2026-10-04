use super::*;

/// A REAL Polygon mainnet CTF `PayoutRedemption` log — the same fixture `chain.rs`'s decoder
/// tests are pinned against (tx `0x3f12d5d3…`, captured 2026-07-25). Reused verbatim so this
/// module's state machine is exercised against genuine wire bytes, not a hand-written mock.
const CTF_REDEMPTION_LOG: &str = r#"{"address":"0x4d97dcd97ec945f40cf65f87097ace5ea0476045","topics":["0x2682012a4a4f1973119f1c9b90745d1bd91fa2bab387344f044cb3586864d18d","0x0000000000000000000000005d4aba8ad45bb5eab3499a0294b42da5d1e455d3","0x0000000000000000000000002791bca1f2de4661ed88a30c99a7a9449aa84174","0x0000000000000000000000000000000000000000000000000000000000000000"],"data":"0x62530e00e2f67d9757e0b06e168e9929e0661daff1276354a3018f1568120c2f0000000000000000000000000000000000000000000000000000000000000060000000000000000000000000000000000000000000000000000000000385715b000000000000000000000000000000000000000000000000000000000000000200000000000000000000000000000000000000000000000000000000000000010000000000000000000000000000000000000000000000000000000000000002","blockNumber":"0x5683395","transactionHash":"0x3f12d5d3285700b3b818784725b19c5ac4fcdfc987eff668175aa82aaf41c3cc","blockTimestamp":"0x6a618b45","logIndex":"0x440","removed":false}"#;

/// The conditionId inside [`CTF_REDEMPTION_LOG`] (first data word).
const CTF_CID: &str = "0x62530e00e2f67d9757e0b06e168e9929e0661daff1276354a3018f1568120c2f";
/// Its block (`"blockNumber":"0x5683395"`).
const CTF_BLOCK: u64 = 0x5683395;

/// A REAL Polygon mainnet NegRiskAdapter `PayoutRedemption` log (tx `0x92ebcfb0…`), same
/// provenance — the neg-risk half of the routing must confirm through the identical path.
const NEG_RISK_REDEMPTION_LOG: &str = r#"{"address":"0xd91e80cf2e7be2e162c6513ced06f1dd0da35296","topics":["0x9140a6a270ef945260c03894b3c6b3b2695e9d5101feef0ff24fec960cfd3224","0x00000000000000000000000041792a63ad17e7a210c808de4177e64a561eccee","0x95dbea2403eefccc30a0b4f276e0dd94d8030ac0f826aa2702ebd835cd75985c"],"data":"0x00000000000000000000000000000000000000000000000000000000000000400000000000000000000000000000000000000000000000000000000000a7d8c0000000000000000000000000000000000000000000000000000000000000000200000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000a7d8c0","blockNumber":"0x568339f","transactionHash":"0x92ebcfb0cb9cd1c984d747eeaa6b693103b51f67a822628464a6d9b89b658d0f","blockTimestamp":"0x6a618b54","logIndex":"0x3c2","removed":false}"#;
const NEG_RISK_CID: &str = "0x95dbea2403eefccc30a0b4f276e0dd94d8030ac0f826aa2702ebd835cd75985c";
const NEG_RISK_BLOCK: u64 = 0x568339f;

/// An unrelated ERC-1155 `TransferSingle` — a log that is real, and is NOT a redemption.
const TRANSFER_SINGLE_LOG: &str = r#"{"address":"0x4d97dcd97ec945f40cf65f87097ace5ea0476045","topics":["0xc3d58168c5ae7397731d063d5bbf3d657854427343f4c083240f7aacaa2d0f62","0x000000000000000000000000d91e80cf2e7be2e162c6513ced06f1dd0da35296","0x000000000000000000000000d91e80cf2e7be2e162c6513ced06f1dd0da35296","0x0000000000000000000000000000000000000000000000000000000000000000"],"data":"0x47212f7902c30bd802ea461cc340b9fdb9ad0b35c1ce8aa75807ee264579f66400000000000000000000000000000000000000000000000000000000004c4b40","blockNumber":"0x568339e","transactionHash":"0xc6c3f62a83b1dc2518a78f64878dfe479d4b5ffc98cf0a40f12ae63affdf60e4","removed":false}"#;

/// Build a receipt envelope around a set of logs.
fn receipt(status: &str, block: u64, logs: &[&str]) -> Value {
    let logs: Vec<Value> = logs.iter().map(|l| serde_json::from_str(l).unwrap()).collect();
    serde_json::json!({
        "status": status,
        "blockNumber": format!("0x{block:x}"),
        "transactionHash": "0xfeed",
        "logs": logs,
    })
}

#[test]
fn no_receipt_is_unmined_not_a_verdict() {
    assert_eq!(
        classify_receipt(None, 999_999_999, CTF_CID, MIN_CONFIRMATIONS),
        RedeemConfirmation::Unmined
    );
}

#[test]
fn deep_successful_receipt_with_our_redemption_settles() {
    let r = receipt("0x1", CTF_BLOCK, &[CTF_REDEMPTION_LOG]);
    let head = CTF_BLOCK + MIN_CONFIRMATIONS; // comfortably buried
    match classify_receipt(Some(&r), head, CTF_CID, MIN_CONFIRMATIONS) {
        RedeemConfirmation::Settled { block, payout_usdc } => {
            assert_eq!(block, CTF_BLOCK);
            assert!(payout_usdc > 0.0, "the realised payout is carried through");
        }
        other => panic!("expected Settled, got {other:?}"),
    }
}

#[test]
fn neg_risk_redemption_settles_through_the_same_path() {
    let r = receipt("0x1", NEG_RISK_BLOCK, &[NEG_RISK_REDEMPTION_LOG]);
    let head = NEG_RISK_BLOCK + MIN_CONFIRMATIONS;
    assert!(matches!(
        classify_receipt(Some(&r), head, NEG_RISK_CID, MIN_CONFIRMATIONS),
        RedeemConfirmation::Settled { .. }
    ));
}

/// Exactly `MIN_CONFIRMATIONS` deep settles; one block shallower does not. The boundary is
/// load-bearing: it decides when a PERMANENT tombstone is written.
#[test]
fn confirmation_depth_boundary_is_inclusive() {
    let r = receipt("0x1", CTF_BLOCK, &[CTF_REDEMPTION_LOG]);
    // head == block + N - 1  ⇒  exactly N confirmations (the tx's own block counts as one).
    let exactly = CTF_BLOCK + MIN_CONFIRMATIONS - 1;
    assert!(matches!(
        classify_receipt(Some(&r), exactly, CTF_CID, MIN_CONFIRMATIONS),
        RedeemConfirmation::Settled { .. }
    ));
    match classify_receipt(Some(&r), exactly - 1, CTF_CID, MIN_CONFIRMATIONS) {
        RedeemConfirmation::Maturing { block, confirmations } => {
            assert_eq!(block, CTF_BLOCK);
            assert_eq!(confirmations, MIN_CONFIRMATIONS - 1);
        }
        other => panic!("expected Maturing one block short, got {other:?}"),
    }
}

/// A head BEHIND the receipt's block (a lagging RPC replica) must never settle.
#[test]
fn head_behind_the_receipt_block_does_not_settle() {
    let r = receipt("0x1", CTF_BLOCK, &[CTF_REDEMPTION_LOG]);
    assert!(matches!(
        classify_receipt(Some(&r), CTF_BLOCK - 10, CTF_CID, MIN_CONFIRMATIONS),
        RedeemConfirmation::Maturing { confirmations: 0, .. }
    ));
}

#[test]
fn reverted_status_is_a_definitive_failure() {
    // status 0x0 — even if (impossibly) a redemption log were present, the status wins.
    let r = receipt("0x0", CTF_BLOCK, &[CTF_REDEMPTION_LOG]);
    match classify_receipt(Some(&r), CTF_BLOCK + 1000, CTF_CID, MIN_CONFIRMATIONS) {
        RedeemConfirmation::Reverted { reason } => assert!(reason.contains("status")),
        other => panic!("expected Reverted, got {other:?}"),
    }
}

/// THE case a bare `status == 1` check would get wrong: the relayer's batch mined fine, but it
/// did not redeem OUR condition. That is a failure, and it must retry.
#[test]
fn success_status_without_our_redemption_log_is_reverted() {
    let r = receipt("0x1", CTF_BLOCK, &[TRANSFER_SINGLE_LOG]);
    match classify_receipt(Some(&r), CTF_BLOCK + 1000, CTF_CID, MIN_CONFIRMATIONS) {
        RedeemConfirmation::Reverted { reason } => assert!(reason.contains("PayoutRedemption")),
        other => panic!("expected Reverted, got {other:?}"),
    }
}

/// A redemption for a DIFFERENT condition in the same batch must not confirm ours.
#[test]
fn a_redemption_for_another_condition_does_not_confirm_ours() {
    let r = receipt("0x1", CTF_BLOCK, &[CTF_REDEMPTION_LOG]);
    assert!(matches!(
        classify_receipt(Some(&r), CTF_BLOCK + 1000, NEG_RISK_CID, MIN_CONFIRMATIONS),
        RedeemConfirmation::Reverted { .. }
    ));
}

/// A batch receipt carrying several redemptions confirms the one that is ours.
#[test]
fn picks_our_condition_out_of_a_multi_redemption_batch() {
    let r = receipt(
        "0x1",
        CTF_BLOCK,
        &[TRANSFER_SINGLE_LOG, NEG_RISK_REDEMPTION_LOG, CTF_REDEMPTION_LOG],
    );
    let head = CTF_BLOCK + 1000;
    assert!(matches!(
        classify_receipt(Some(&r), head, CTF_CID, MIN_CONFIRMATIONS),
        RedeemConfirmation::Settled { .. }
    ));
    assert!(matches!(
        classify_receipt(Some(&r), head, NEG_RISK_CID, MIN_CONFIRMATIONS),
        RedeemConfirmation::Settled { .. }
    ));
}

/// An absent `status` field is not read as a revert — the redemption log is the stronger proof.
#[test]
fn absent_status_falls_through_to_the_log_evidence() {
    let log: Value = serde_json::from_str(CTF_REDEMPTION_LOG).unwrap();
    let r = serde_json::json!({ "blockNumber": format!("0x{CTF_BLOCK:x}"), "logs": [log] });
    assert!(matches!(
        classify_receipt(Some(&r), CTF_BLOCK + 1000, CTF_CID, MIN_CONFIRMATIONS),
        RedeemConfirmation::Settled { .. }
    ));
}

/// conditionId comparison is case- and `0x`-insensitive on BOTH sides.
#[test]
fn condition_id_match_is_normalised() {
    let r = receipt("0x1", CTF_BLOCK, &[CTF_REDEMPTION_LOG]);
    let head = CTF_BLOCK + 1000;
    let upper = CTF_CID.to_ascii_uppercase().replace("0X", "0x");
    assert!(matches!(
        classify_receipt(Some(&r), head, &upper, MIN_CONFIRMATIONS),
        RedeemConfirmation::Settled { .. }
    ));
    let bare = CTF_CID.trim_start_matches("0x");
    assert!(matches!(
        classify_receipt(Some(&r), head, bare, MIN_CONFIRMATIONS),
        RedeemConfirmation::Settled { .. }
    ));
}

/// `status` re-encoded as a bare JSON number (some RPC proxies do this) still parses.
#[test]
fn numeric_status_is_accepted() {
    let log: Value = serde_json::from_str(CTF_REDEMPTION_LOG).unwrap();
    let ok = serde_json::json!({ "status": 1, "blockNumber": format!("0x{CTF_BLOCK:x}"), "logs": [log.clone()] });
    assert!(matches!(
        classify_receipt(Some(&ok), CTF_BLOCK + 1000, CTF_CID, MIN_CONFIRMATIONS),
        RedeemConfirmation::Settled { .. }
    ));
    let bad = serde_json::json!({ "status": 0, "blockNumber": format!("0x{CTF_BLOCK:x}"), "logs": [log] });
    assert!(matches!(
        classify_receipt(Some(&bad), CTF_BLOCK + 1000, CTF_CID, MIN_CONFIRMATIONS),
        RedeemConfirmation::Reverted { .. }
    ));
}

/// A receipt with no usable block number is not proof of depth — treat it as unmined rather
/// than settling something whose finality cannot be measured.
#[test]
fn receipt_without_a_block_number_is_unmined() {
    let log: Value = serde_json::from_str(CTF_REDEMPTION_LOG).unwrap();
    // strip blockNumber from BOTH the receipt and the log so no fallback can supply it.
    let mut bare_log = log.clone();
    bare_log.as_object_mut().unwrap().remove("blockNumber");
    let r = serde_json::json!({ "status": "0x1", "logs": [bare_log] });
    assert_eq!(
        classify_receipt(Some(&r), 999_999_999, CTF_CID, MIN_CONFIRMATIONS),
        RedeemConfirmation::Unmined
    );
}

/// The confirmer is constructible without opening a socket, and its depth is overridable.
#[test]
fn confirmer_construction_is_io_free() {
    let c = ChainRedeemConfirmer::new(PolygonRpc::with_url("http://127.0.0.1:1/never-dialled"))
        .with_min_confirmations(3);
    assert_eq!(c.min_confirmations(), 3);
    assert_eq!(c.rpc().url(), "http://127.0.0.1:1/never-dialled");
}
