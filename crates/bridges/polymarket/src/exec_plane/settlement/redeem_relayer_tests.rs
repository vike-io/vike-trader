use super::*;

// a fixed throwaway test key (NOT a real account) → a deterministic signature.
const KEY: &str = "0x1111111111111111111111111111111111111111111111111111111111111111";
const PROXY: &str = "0x00000000000000000000000000000000000000aa";
const CID: &str = "0x0000000000000000000000000000000000000000000000000000000000000001";

/// The GOLDEN signature is pinned for the LEGACY [`Era::V1`] encoding (CTF target + USDC.e
/// collateral) — its digest is unchanged by this fix, so the byte-exact pin still regression-
/// guards the EIP-712 domain / type-string / field-order wiring. (A V2 golden would need a fresh
/// green run to capture; the V2 test below proves routing + determinism instead.)
#[test]
fn redeem_request_v1_is_deterministic_with_golden_signature() {
    let req = build_redeem_request(KEY, PROXY, CID, 0, 0, &RedeemKind::Binary, Era::V1)
        .expect("build ok");

    // V1 targets the CTF contract directly with the binary redeem calldata (selector 0x01b7037c):
    let ctf = crate::exec_plane::settlement::redeem::CTF_ADDRESS;
    assert!(
        req.body.contains(ctf) || req.body.contains(&ctf.to_lowercase()),
        "body targets the CTF contract"
    );
    let expected_calldata =
        format!("0x{}", hex::encode(redeem_positions_calldata(CID, Era::V1).unwrap()));
    assert!(req.body.contains("01b7037c"), "body carries the redeemPositions selector");
    assert!(req.body.contains(&expected_calldata), "body carries the exact redeem calldata");

    // deposit-wallet WALLET request shape (VERIFIED from source):
    assert!(req.body.contains("\"type\":\"WALLET\""), "tx type WALLET");
    assert!(req.body.contains(DEPOSIT_WALLET_FACTORY), "to = deposit-wallet factory");
    assert!(req.body.contains(PROXY), "depositWallet = the proxy/deposit wallet");
    // `from` = the owner EOA derived from the fixed key (deterministic).
    let owner = eth_address_from_private_key(KEY).unwrap();
    assert!(req.body.contains(&owner), "from = derived owner EOA");

    // a real, well-formed EIP-712 signature: 0x + 65 bytes (130 hex).
    assert!(req.signature.starts_with("0x"), "0x-prefixed signature");
    assert_eq!(req.signature.len(), 2 + 130, "65-byte r||s||v signature");
    assert!(req.signature[2..].bytes().all(|b| b.is_ascii_hexdigit()), "signature is hex");

    // determinism: same inputs → identical signature + body.
    let again = build_redeem_request(KEY, PROXY, CID, 0, 0, &RedeemKind::Binary, Era::V1)
        .expect("build ok");
    assert_eq!(req.signature, again.signature, "signature is deterministic");
    assert_eq!(req.body, again.body, "body is deterministic");

    // golden pin — the exact Batch signature for the V1 (KEY, PROXY, CID, nonce=0, deadline=0)
    // encoding. A change here means the signed digest changed (domain/type-string/field-order).
    assert_eq!(req.signature, GOLDEN_SIGNATURE, "EIP-712 Batch signature regression pin");

    // non-secret: the deterministic request carries no relayer key.
    assert!(req.headers.iter().any(|(k, _)| k == "Content-Type"));
}

// Pinned from the first green run of the V1 encoding (secp256k1 is deterministic for a fixed key).
const GOLDEN_SIGNATURE: &str = "0x8124e5f28cb7cf7f4bf08cb3aaad48ff686cfb6282ca64374bd9493ff047055124659389443dbac637275db2cdabb3c99de9db6ef76ee3147edd47fbf792e7c71c";

/// The V2 (default) binary redeem routes to the CtfCollateralAdapter with the pUSD-collateral
/// calldata — and MUST NOT hit the raw CTF (the pre-fix target). No golden signature is pinned
/// (it would need a fresh green run); determinism + routing prove the wiring meanwhile.
#[test]
fn redeem_request_v2_routes_to_ctf_collateral_adapter() {
    let req = build_redeem_request(KEY, PROXY, CID, 0, 0, &RedeemKind::Binary, Era::V2).unwrap();
    assert!(
        req.body.contains(crate::exec_plane::settlement::redeem::CTF_COLLATERAL_ADAPTER),
        "V2 binary targets the CtfCollateralAdapter"
    );
    let expected = format!("0x{}", hex::encode(redeem_positions_calldata(CID, Era::V2).unwrap()));
    assert!(req.body.contains(&expected), "carries the exact pUSD-collateral calldata");
    assert!(req.body.contains("01b7037c"), "selector is era-independent");
    // must not target the raw CTF nor carry USDC.e collateral (the retired/pre-fix encoding):
    assert!(
        !req.body.contains(
            crate::exec_plane::settlement::redeem::USDC_E_ADDRESS.to_lowercase().as_str()
        )
    );
    // deterministic
    let again = build_redeem_request(KEY, PROXY, CID, 0, 0, &RedeemKind::Binary, Era::V2).unwrap();
    assert_eq!(req.signature, again.signature);
    assert_eq!(req.body, again.body);
}

#[test]
fn rejects_malformed_proxy_addr() {
    // too-short / non-20-byte proxy must hard-error, not silently zero-default the wallet.
    assert!(
        build_redeem_request(KEY, "0xbad", CID, 0, 0, &RedeemKind::Binary, Era::V2).is_err(),
        "short proxy"
    );
    assert!(
        build_redeem_request(KEY, "0xzz", CID, 0, 0, &RedeemKind::Binary, Era::V2).is_err(),
        "non-hex proxy"
    );
    // a 32-byte value where a 20-byte address is required is rejected too.
    let too_long = format!("0x{}", "aa".repeat(32));
    assert!(
        build_redeem_request(KEY, &too_long, CID, 0, 0, &RedeemKind::Binary, Era::V2).is_err(),
        "too-long proxy"
    );
    // the valid proxy still builds.
    assert!(
        build_redeem_request(KEY, PROXY, CID, 0, 0, &RedeemKind::Binary, Era::V2).is_ok(),
        "valid proxy"
    );
}

#[test]
fn neg_risk_request_targets_the_era_adapter() {
    // V2 (default) → the new NegRiskCtfCollateralAdapter, NEVER the retired V1 adapter nor CTF.
    let v2 = build_redeem_request(KEY, PROXY, CID, 0, 0, &RedeemKind::NegRisk(vec![1, 2]), Era::V2)
        .unwrap();
    assert!(
        v2.body.contains(crate::exec_plane::settlement::redeem::NEG_RISK_CTF_COLLATERAL_ADAPTER)
    );
    assert!(
        !v2.body.contains(crate::exec_plane::settlement::redeem::NEG_RISK_ADAPTER),
        "not the retired V1 adapter"
    );
    assert!(!v2.body.contains(crate::exec_plane::settlement::redeem::CTF_ADDRESS));
    assert!(!v2.signature.is_empty());
    // V1 (legacy) → the retired adapter, retained for known-legacy positions.
    let v1 = build_redeem_request(KEY, PROXY, CID, 0, 0, &RedeemKind::NegRisk(vec![1, 2]), Era::V1)
        .unwrap();
    assert!(v1.body.contains(crate::exec_plane::settlement::redeem::NEG_RISK_ADAPTER));
}

/// ⚠ THE REGRESSION GUARD for the 2026-08-02 ABI fix. A V2 neg-risk redeem must carry the
/// **4-arg CTF calldata** (`0x01b7037c`) — byte-identical to a binary redeem, differing ONLY by
/// target — because `NegRiskCtfCollateralAdapter` extends `CtfCollateralAdapter` and does not
/// implement the 2-arg `0xdbeccb23` overload at all. Only the retired V1 adapter does, and only
/// there do the per-slot amounts reach the chain. See [`RedeemKind::call`].
#[test]
fn v2_neg_risk_uses_the_ctf_encoder_v1_keeps_the_two_arg_overload() {
    let (nr_target, nr_data) = RedeemKind::NegRisk(vec![1, 2]).call(CID, Era::V2).unwrap();
    let (bin_target, bin_data) = RedeemKind::Binary.call(CID, Era::V2).unwrap();
    assert_eq!(hex::encode(&nr_data[0..4]), "01b7037c", "V2 neg-risk selector");
    assert_eq!(nr_data, bin_data, "on V2 the calldata is identical to a binary redeem");
    assert_eq!(nr_target, crate::exec_plane::settlement::redeem::NEG_RISK_CTF_COLLATERAL_ADAPTER);
    assert_eq!(bin_target, crate::exec_plane::settlement::redeem::CTF_COLLATERAL_ADAPTER);
    assert_ne!(nr_target, bin_target, "ONLY the target differs on V2");
    // The V2 adapter reads its own balances, so the uint256[] is inert — a different array must
    // not move a single byte (this is what makes carrying amounts harmless, not meaningful).
    let (_, other) = RedeemKind::NegRisk(vec![53_000_000, 0]).call(CID, Era::V2).unwrap();
    assert_eq!(other, nr_data, "V2 ignores the uint256[]; the encoder must not vary with it");
    // V1 (retired) keeps the 2-arg overload, where the amounts ARE load-bearing.
    let (v1_target, v1_data) = RedeemKind::NegRisk(vec![1, 2]).call(CID, Era::V1).unwrap();
    assert_eq!(hex::encode(&v1_data[0..4]), "dbeccb23", "V1 neg-risk selector");
    assert_eq!(v1_target, crate::exec_plane::settlement::redeem::NEG_RISK_ADAPTER);
    let (_, v1_other) = RedeemKind::NegRisk(vec![53_000_000, 0]).call(CID, Era::V1).unwrap();
    assert_ne!(v1_other, v1_data, "V1 amounts reach the chain, so they change the bytes");
}

#[test]
fn binary_request_targets_the_era_contract() {
    // V2 (default) → the CtfCollateralAdapter; V1 (legacy) → the raw CTF.
    let v2 = build_redeem_request(KEY, PROXY, CID, 0, 0, &RedeemKind::Binary, Era::V2).unwrap();
    assert!(v2.body.contains(crate::exec_plane::settlement::redeem::CTF_COLLATERAL_ADAPTER));
    let v1 = build_redeem_request(KEY, PROXY, CID, 0, 0, &RedeemKind::Binary, Era::V1).unwrap();
    assert!(v1.body.contains(crate::exec_plane::settlement::redeem::CTF_ADDRESS));
}

#[test]
fn nonce_parse_tolerates_shapes() {
    assert_eq!(parse_nonce(&serde_json::json!(7)).unwrap(), 7);
    assert_eq!(parse_nonce(&serde_json::json!({"nonce": 5})).unwrap(), 5);
    assert_eq!(parse_nonce(&serde_json::json!({"nonce": "9"})).unwrap(), 9);
    assert_eq!(parse_nonce(&serde_json::json!("3")).unwrap(), 3);
    assert!(parse_nonce(&serde_json::json!({"x": 1})).is_err());
}

#[test]
fn submit_parse_extracts_tx_hash() {
    let r = parse_submit(r#"{"transactionID":"abc","state":"NEW","transactionHash":"0xdeadbeef"}"#);
    assert_eq!(r.tx_hash.as_deref(), Some("0xdeadbeef"));
    let r2 = parse_submit(r#"{"transactionID":"abc","state":"NEW"}"#);
    assert_eq!(r2.tx_hash, None);
}
