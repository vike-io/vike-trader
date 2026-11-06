use super::*;

const CID: &str = "0x0000000000000000000000000000000000000000000000000000000000000001";
// a fixed throwaway test key (NOT a real account) → a deterministic signature.
const KEY: &str = "0x1111111111111111111111111111111111111111111111111111111111111111";
const PROXY: &str = "0x00000000000000000000000000000000000000aa";
/// $1.00 of USDC.e (6 decimals) — the amount used across the golden vectors.
const ONE_USDC: u128 = 1_000_000;

/// Both the derived selector AND an independent literal must agree, for all four signatures.
/// The literals come from 4byte.directory, NOT from running this code — so a typo in a signature
/// string above cannot "confirm itself". This is the #125 discipline that caught a wrong redeem
/// selector.
#[test]
fn selectors_match_independent_literals() {
    assert_eq!(&keccak256(CTF_SPLIT_SIG)[0..4], &[0x72, 0xce, 0x42, 0x75], "CTF splitPosition");
    assert_eq!(&keccak256(CTF_MERGE_SIG)[0..4], &[0x9e, 0x72, 0x12, 0xad], "CTF mergePositions");
    assert_eq!(
        &keccak256(NR_SPLIT_SIG)[0..4],
        &[0xa3, 0xd7, 0xda, 0x1d],
        "NegRisk splitPosition(bytes32,uint256)"
    );
    assert_eq!(
        &keccak256(NR_MERGE_SIG)[0..4],
        &[0xb1, 0x0c, 0x5c, 0x17],
        "NegRisk mergePositions(bytes32,uint256)"
    );
    // Methodology sanity check: the SAME keccak path must reproduce the known-good redeem
    // selector 0x01b7037c that redeem.rs already pins on-chain-verified.
    assert_eq!(
        &keccak256(b"redeemPositions(address,bytes32,bytes32,uint256[])")[0..4],
        &[0x01, 0xb7, 0x03, 0x7c],
        "known redeem selector reproduces — keccak path is sound"
    );
    // `convertPositions` — the literal is from the DEPLOYED NegRiskAdapter's dispatch table
    // (eth_getCode + PUSH4 probe, see the module doc), not from running this code.
    assert_eq!(
        &keccak256(NR_CONVERT_SIG)[0..4],
        &[0xc6, 0x47, 0x48, 0xc4],
        "NegRisk convertPositions(bytes32,uint256,uint256)"
    );
    // ...and the arities that are NOT on the deployed contract must NOT produce it — a guard
    // against someone "fixing" the signature to a plausible-looking variant.
    for wrong in [
        &b"convertPositions(bytes32,uint256,uint256,address)"[..],
        &b"convertPositions(bytes32,uint256[],uint256)"[..],
        &b"convertPositions(bytes32,uint256)"[..],
    ] {
        assert_ne!(
            &keccak256(wrong)[0..4],
            &[0xc6, 0x47, 0x48, 0xc4],
            "a non-deployed arity must not collide with the real selector"
        );
    }
    // The five selectors are mutually distinct (guards a copy-paste of the wrong const).
    let sels = [CTF_SPLIT_SIG, CTF_MERGE_SIG, NR_SPLIT_SIG, NR_MERGE_SIG, NR_CONVERT_SIG]
        .map(|s| keccak256(s)[0..4].to_vec());
    for i in 0..sels.len() {
        for j in (i + 1)..sels.len() {
            assert_ne!(sels[i], sels[j], "selectors {i} and {j} collide");
        }
    }
}

/// Hand-derived golden hex for the whole CTF split calldata in the V2 (pUSD) era — the default.
/// Built by hand from the ABI rules (not captured from this function's output), so it
/// independently pins layout AND ordering. GOLDEN CHANGE vs the pre-fix encoding: the collateral
/// word is pUSD (`c011a7…`), not USDC.e (`2791bc…`); the selector is era-independent.
#[test]
fn ctf_split_calldata_v2_is_byte_exact() {
    let cd = split_position_calldata(CID, ONE_USDC, Era::V2).unwrap();
    let expected = concat!(
        "72ce4275", // selector: splitPosition(address,bytes32,bytes32,uint256[],uint256)
        "000000000000000000000000c011a7e12a19f7b1f670d46f03b03f3342e82dfb", // pUSD collateral
        "0000000000000000000000000000000000000000000000000000000000000000", // parentCollectionId
        "0000000000000000000000000000000000000000000000000000000000000001", // conditionId
        "00000000000000000000000000000000000000000000000000000000000000a0", // partition offset
        "00000000000000000000000000000000000000000000000000000000000f4240", // amount = 1_000_000
        "0000000000000000000000000000000000000000000000000000000000000002", // partition.length
        "0000000000000000000000000000000000000000000000000000000000000001", // partition[0] = 1
        "0000000000000000000000000000000000000000000000000000000000000002", // partition[1] = 2
    );
    assert_eq!(hex::encode(&cd), expected, "full CTF split calldata (V2/pUSD)");
    assert_eq!(cd.len(), 4 + 8 * 32, "260-byte total");
}

/// The legacy V1 (USDC.e) CTF split — retained for known-legacy positions. Byte-identical to V2
/// except the collateral word, and the selector is unchanged.
#[test]
fn ctf_split_calldata_v1_keeps_usdc_e() {
    let v1 = split_position_calldata(CID, ONE_USDC, Era::V1).unwrap();
    let v2 = split_position_calldata(CID, ONE_USDC, Era::V2).unwrap();
    // V1 collateral = USDC.e (bytes 4..36, low 20 of the 32-byte word)
    assert_eq!(
        hex::encode(&v1[4 + 12..4 + 32]),
        crate::exec_plane::settlement::redeem::USDC_E_ADDRESS
            .trim_start_matches("0x")
            .to_lowercase()
    );
    assert_eq!(&v1[0..4], &[0x72, 0xce, 0x42, 0x75], "selector era-independent");
    assert_ne!(&v1[4..36], &v2[4..36], "collateral word differs by era");
    assert_eq!(&v1[36..], &v2[36..], "everything after the collateral word is identical");
}

/// Merge is byte-identical to split except the 4-byte selector — pinning that directly catches a
/// swapped-encoder bug that per-field assertions would miss.
#[test]
fn ctf_merge_calldata_differs_only_in_selector() {
    let split = split_position_calldata(CID, ONE_USDC, Era::V2).unwrap();
    let merge = merge_positions_calldata(CID, ONE_USDC, Era::V2).unwrap();
    assert_eq!(&merge[0..4], &[0x9e, 0x72, 0x12, 0xad], "mergePositions selector");
    assert_eq!(&merge[4..], &split[4..], "args identical to split");
    assert_ne!(&merge[0..4], &split[0..4], "selectors differ");
}

/// The neg-risk short overload is fully static — 68 bytes, no dynamic-array offset word.
#[test]
fn neg_risk_calldata_is_byte_exact() {
    let cd = split_position_neg_risk_calldata(CID, ONE_USDC).unwrap();
    let expected = concat!(
        "a3d7da1d", // selector: splitPosition(bytes32,uint256)
        "0000000000000000000000000000000000000000000000000000000000000001", // conditionId
        "00000000000000000000000000000000000000000000000000000000000f4240", // amount
    );
    assert_eq!(hex::encode(&cd), expected, "full neg-risk split calldata");
    assert_eq!(cd.len(), 4 + 2 * 32, "68-byte total — static, no offset word");

    let m = merge_positions_neg_risk_calldata(CID, ONE_USDC).unwrap();
    assert_eq!(&m[0..4], &[0xb1, 0x0c, 0x5c, 0x17], "neg-risk mergePositions selector");
    assert_eq!(&m[4..], &cd[4..], "args identical to the neg-risk split");
}

/// `amount` must be encoded, not dropped — a zero-amount split would be a silent no-op on-chain.
#[test]
fn amount_is_encoded_and_varies() {
    let a = split_position_calldata(CID, 1, Era::V2).unwrap();
    let b = split_position_calldata(CID, 2, Era::V2).unwrap();
    assert_ne!(a, b, "different amounts → different calldata");
    // amount is head word 5: offset 4 + 4*32, low byte at +31.
    assert_eq!(a[4 + 128 + 31], 1);
    assert_eq!(b[4 + 128 + 31], 2);
    // u128::MAX round-trips into the low 16 bytes of the word.
    let big = split_position_calldata(CID, u128::MAX, Era::V2).unwrap();
    assert_eq!(&big[4 + 128 + 16..4 + 128 + 32], &[0xffu8; 16]);
    assert_eq!(&big[4 + 128..4 + 128 + 16], &[0u8; 16], "high 16 bytes stay zero");
}

/// Same hardening contract as `redeem.rs`: never truncate or zero-pad a conditionId. Covered for
/// BOTH the binary (era-taking) encoders and the neg-risk (era-free) short overloads.
#[test]
fn rejects_malformed_condition_id() {
    let too_long = format!("0x{}", "aa".repeat(33));
    for bad in ["0xdead", too_long.as_str(), "0xzzzz"] {
        // binary (era-taking) encoders
        assert!(split_position_calldata(bad, 1, Era::V2).is_err(), "binary split {bad}");
        assert!(merge_positions_calldata(bad, 1, Era::V2).is_err(), "binary merge {bad}");
        // neg-risk (era-free) short overloads
        assert!(split_position_neg_risk_calldata(bad, 1).is_err(), "neg-risk split {bad}");
        assert!(merge_positions_neg_risk_calldata(bad, 1).is_err(), "neg-risk merge {bad}");
    }
    // a valid conditionId still encodes on every path.
    assert!(split_position_calldata(CID, 1, Era::V2).is_ok());
    assert!(merge_positions_calldata(CID, 1, Era::V2).is_ok());
    assert!(split_position_neg_risk_calldata(CID, 1).is_ok());
    assert!(merge_positions_neg_risk_calldata(CID, 1).is_ok());
}

/// Hand-derived golden hex for `convertPositions`. Fully static — three words, no offset.
#[test]
fn convert_positions_calldata_is_byte_exact() {
    // the LIVE "Next Prime Minister of Ethiopia" negRiskMarketID (see gamma.rs's module doc):
    // note its last byte is 00 — the set's index-0 slot — which is what distinguishes a
    // marketId from a conditionId at a glance.
    const MID: &str = "0x55ab76d092f682bf5cbb7e14f13ee12f8410ce7cc1b7906f23b8fb56c11f6500";
    // convert outcomes 0 and 2 → bitmap 0b101 = 5
    let index_set = index_set_from_indices(&[0, 2]).unwrap();
    assert_eq!(index_set, 5);
    let cd = convert_positions_calldata(MID, index_set, ONE_USDC).unwrap();
    let expected = concat!(
        "c64748c4",                                                         // selector
        "55ab76d092f682bf5cbb7e14f13ee12f8410ce7cc1b7906f23b8fb56c11f6500", // _marketId
        "0000000000000000000000000000000000000000000000000000000000000005", // _indexSet
        "00000000000000000000000000000000000000000000000000000000000f4240", // _amount = 1e6
    );
    assert_eq!(hex::encode(&cd), expected);
    assert_eq!(cd.len(), 4 + 3 * 32, "static 100-byte calldata, no dynamic offset");
    // a marketId is validated exactly like a conditionId — never truncated or zero-padded,
    // and the message names the RIGHT field.
    let err = convert_positions_calldata("0xdead", 1, 1).unwrap_err();
    assert!(err.contains("negRiskMarketID"), "{err}");
    assert!(!err.contains("conditionId"), "{err}");
    assert!(convert_positions_calldata(&"ab".repeat(33), 1, 1).is_err(), "too long");
    assert!(convert_positions_calldata("0xzzzz", 1, 1).is_err(), "non-hex");
}

/// The `_indexSet` BITMAP contract: bit i ↔ outcome index i, with the out-of-range and empty
/// cases refused rather than silently wrapping into the wrong outcomes.
#[test]
fn index_set_bitmap_semantics() {
    assert_eq!(index_set_from_indices(&[0]).unwrap(), 1);
    assert_eq!(index_set_from_indices(&[1]).unwrap(), 2);
    assert_eq!(index_set_from_indices(&[0, 1]).unwrap(), 3);
    assert_eq!(index_set_from_indices(&[7]).unwrap(), 128);
    assert_eq!(index_set_from_indices(&[0, 1, 2, 3, 4, 5, 6]).unwrap(), 127);
    // order-independent and duplicate-idempotent
    assert_eq!(index_set_from_indices(&[2, 0]).unwrap(), 5);
    assert_eq!(index_set_from_indices(&[2, 0, 2, 0]).unwrap(), 5);
    // the widest slot this encoder can express
    assert_eq!(index_set_from_indices(&[127]).unwrap(), 1u128 << 127);
    // refusals — a wrap here would convert the WRONG legs on chain
    assert!(index_set_from_indices(&[]).is_err(), "empty set");
    assert!(index_set_from_indices(&[128]).is_err(), "beyond u128");
    assert!(index_set_from_indices(&[0, 200]).is_err(), "one bad index poisons the set");
}

/// `convertPositions` has a DEDICATED submit path ([`build_convert_request`]/[`submit_convert`]),
/// NOT a [`SplitMergeKind`] variant — the enum stays the `(conditionId, amount)` 2-arg dispatch,
/// so no split/merge kind may ever encode the 3-arg convert. This preserves that invariant even
/// though the convert submit route now exists (its live round-trip is OWED, per the module doc).
#[test]
fn convert_stays_out_of_the_split_merge_kind_enum() {
    for kind in [
        SplitMergeKind::SplitBinary,
        SplitMergeKind::MergeBinary,
        SplitMergeKind::SplitNegRisk,
        SplitMergeKind::MergeNegRisk,
    ] {
        let (_, cd) = kind.call(CID, ONE_USDC, Era::V2).unwrap();
        assert_ne!(
            &cd[0..4],
            &keccak256(NR_CONVERT_SIG)[0..4],
            "{kind:?} must not encode convertPositions"
        );
    }
}

/// The convert relayer request routes to the era's neg-risk adapter, carries the exact
/// convertPositions selector + calldata inside the same `WALLET` deposit-wallet envelope the
/// redeem/split-merge paths use, is deterministic, and binds every load-bearing input into the
/// signed digest. (No golden-signature constant is pinned here — unlike the split path above —
/// because it must be captured from a green run; determinism + per-input binding prove the digest
/// wiring meanwhile.)
#[test]
fn convert_relayer_request_shape_and_routing() {
    let index_set = index_set_from_indices(&[0, 2]).unwrap(); // 0b101
    // V2 (default) → the new NegRiskCtfCollateralAdapter, NEVER the retired V1 adapter nor CTF.
    let req = build_convert_request(KEY, PROXY, CID, index_set, ONE_USDC, 0, 0, Era::V2).unwrap();
    assert!(
        req.body.contains(crate::exec_plane::settlement::redeem::NEG_RISK_CTF_COLLATERAL_ADAPTER),
        "targets the V2 NegRiskCtfCollateralAdapter"
    );
    assert!(
        !req.body.contains(crate::exec_plane::settlement::redeem::NEG_RISK_ADAPTER),
        "not the retired V1 adapter"
    );
    assert!(
        !req.body.contains(crate::exec_plane::settlement::redeem::CTF_ADDRESS),
        "convert does not hit the CTF"
    );
    // carries the convertPositions selector + the exact calldata:
    assert!(req.body.contains("c64748c4"), "convertPositions selector");
    let cd = convert_positions_calldata(CID, index_set, ONE_USDC).unwrap();
    assert!(req.body.contains(&format!("0x{}", hex::encode(&cd))), "exact convert calldata");
    // the deposit-wallet WALLET envelope, same as redeem/split-merge:
    assert!(req.body.contains("\"type\":\"WALLET\""), "tx type WALLET");
    assert!(req.body.contains(PROXY), "depositWallet = proxy");
    assert!(req.body.contains("\"value\":\"0\""), "Call.value = 0");
    // a well-formed EIP-712 signature: 0x + 65 bytes, hex, no secret leaked.
    assert!(req.signature.starts_with("0x"));
    assert_eq!(req.signature.len(), 2 + 130, "65-byte r||s||v");
    assert!(req.signature[2..].bytes().all(|b| b.is_ascii_hexdigit()));
    assert!(!req.body.contains(KEY), "private key never in the body");

    // V1 (legacy) routes to the retired adapter, retained for known-legacy positions.
    let v1 = build_convert_request(KEY, PROXY, CID, index_set, ONE_USDC, 0, 0, Era::V1).unwrap();
    assert!(
        v1.body.contains(crate::exec_plane::settlement::redeem::NEG_RISK_ADAPTER),
        "V1 → retired adapter"
    );

    // deterministic, and every load-bearing input is bound into the digest.
    let again = build_convert_request(KEY, PROXY, CID, index_set, ONE_USDC, 0, 0, Era::V2).unwrap();
    assert_eq!(req.signature, again.signature, "signature is deterministic");
    assert_eq!(req.body, again.body, "body is deterministic");
    let nonce1 =
        build_convert_request(KEY, PROXY, CID, index_set, ONE_USDC, 1, 0, Era::V2).unwrap();
    assert_ne!(req.signature, nonce1.signature, "nonce is bound in");
    let dl1 = build_convert_request(KEY, PROXY, CID, index_set, ONE_USDC, 0, 1, Era::V2).unwrap();
    assert_ne!(req.signature, dl1.signature, "deadline is bound in");
    let amt2 = build_convert_request(KEY, PROXY, CID, index_set, 2, 0, 0, Era::V2).unwrap();
    assert_ne!(req.signature, amt2.signature, "amount is bound in");
    let idx2 =
        build_convert_request(KEY, PROXY, CID, index_set + 1, ONE_USDC, 0, 0, Era::V2).unwrap();
    assert_ne!(req.signature, idx2.signature, "index_set is bound in");
    // the era is bound in too (different target → different Call hash → different signature).
    assert_ne!(req.signature, v1.signature, "era is bound into the digest");
    // a malformed marketId hard-errors (never a zero-defaulted send).
    assert!(
        build_convert_request(KEY, PROXY, "0xdead", index_set, ONE_USDC, 0, 0, Era::V2).is_err()
    );
    // and a malformed deposit wallet is rejected too.
    assert!(build_convert_request(KEY, "0xbad", CID, index_set, ONE_USDC, 0, 0, Era::V2).is_err());
}

/// Relayer payload shape: the four kinds route to the era-correct target contract and carry the
/// exact calldata, inside the same `WALLET` deposit-wallet envelope the redeem path uses. Asserted
/// for the V2 (default) targets — binary → CtfCollateralAdapter, neg-risk →
/// NegRiskCtfCollateralAdapter.
///
/// ⚠ The neg-risk SELECTORS here changed on 2026-08-02: this table used to pin `a3d7da1d` /
/// `b10c5c17` (the 2-arg overloads) for V2, which the V2 adapter does not implement — see
/// [`SplitMergeKind::call`]. On V2 all four kinds now carry the CTF selectors and differ only by
/// target; the 2-arg pins moved to `neg_risk_v1_keeps_the_two_arg_overloads` below.
#[test]
fn relayer_request_shape_and_routing() {
    let bin_adapter = crate::exec_plane::settlement::redeem::CTF_COLLATERAL_ADAPTER;
    let nr_adapter = crate::exec_plane::settlement::redeem::NEG_RISK_CTF_COLLATERAL_ADAPTER;
    for (kind, target, sel) in [
        (SplitMergeKind::SplitBinary, bin_adapter, "72ce4275"),
        (SplitMergeKind::MergeBinary, bin_adapter, "9e7212ad"),
        (SplitMergeKind::SplitNegRisk, nr_adapter, "72ce4275"),
        (SplitMergeKind::MergeNegRisk, nr_adapter, "9e7212ad"),
    ] {
        let req =
            build_split_merge_request(KEY, PROXY, CID, ONE_USDC, 0, 0, kind, Era::V2).unwrap();
        assert!(req.body.contains(target), "{kind:?} targets {target}");
        assert!(req.body.contains(sel), "{kind:?} carries selector {sel}");
        let (_, cd) = kind.call(CID, ONE_USDC, Era::V2).unwrap();
        assert!(
            req.body.contains(&format!("0x{}", hex::encode(&cd))),
            "{kind:?} carries the exact calldata"
        );
        // the deposit-wallet WALLET envelope, same as redeem:
        assert!(req.body.contains("\"type\":\"WALLET\""), "tx type WALLET");
        assert!(req.body.contains(PROXY), "depositWallet = proxy");
        assert!(req.body.contains("\"value\":\"0\""), "Call.value = 0");
        // a well-formed EIP-712 signature: 0x + 65 bytes.
        assert!(req.signature.starts_with("0x"));
        assert_eq!(req.signature.len(), 2 + 130, "65-byte r||s||v");
        assert!(req.signature[2..].bytes().all(|b| b.is_ascii_hexdigit()));
        // non-secret: no relayer key in the deterministic request.
        assert!(req.headers.iter().any(|(k, _)| k == "Content-Type"));
        assert!(!req.body.contains(KEY), "private key never in the body");
    }
    // binary and neg-risk must NOT cross-target (V2 adapters).
    let b = build_split_merge_request(
        KEY,
        PROXY,
        CID,
        ONE_USDC,
        0,
        0,
        SplitMergeKind::SplitBinary,
        Era::V2,
    )
    .unwrap();
    assert!(
        !b.body.contains(crate::exec_plane::settlement::redeem::NEG_RISK_CTF_COLLATERAL_ADAPTER),
        "binary split does not hit the neg-risk adapter"
    );
    let n = build_split_merge_request(
        KEY,
        PROXY,
        CID,
        ONE_USDC,
        0,
        0,
        SplitMergeKind::SplitNegRisk,
        Era::V2,
    )
    .unwrap();
    assert!(
        !n.body.contains(crate::exec_plane::settlement::redeem::CTF_COLLATERAL_ADAPTER),
        "neg-risk split does not hit the binary adapter"
    );
}

/// The mirror of the V2 table above: on the RETIRED [`Era::V1`] adapter the neg-risk kinds keep
/// the 2-arg overloads (`a3d7da1d` / `b10c5c17`) — the only contract that implements them — and
/// on V2 they carry byte-identical calldata to their binary twins, the target alone differing.
/// This is the split/merge half of the 2026-08-02 ABI fix; see [`SplitMergeKind::call`].
#[test]
fn neg_risk_v1_keeps_the_two_arg_overloads() {
    for (kind, sel) in
        [(SplitMergeKind::SplitNegRisk, "a3d7da1d"), (SplitMergeKind::MergeNegRisk, "b10c5c17")]
    {
        let (target, cd) = kind.call(CID, ONE_USDC, Era::V1).unwrap();
        assert_eq!(
            target,
            crate::exec_plane::settlement::redeem::NEG_RISK_ADAPTER,
            "{kind:?} V1 target"
        );
        assert_eq!(hex::encode(&cd[0..4]), sel, "{kind:?} V1 selector");
        assert_eq!(cd.len(), 4 + 2 * 32, "{kind:?} 2-arg static calldata");
    }
    for (nr, bin) in [
        (SplitMergeKind::SplitNegRisk, SplitMergeKind::SplitBinary),
        (SplitMergeKind::MergeNegRisk, SplitMergeKind::MergeBinary),
    ] {
        let (nr_target, nr_cd) = nr.call(CID, ONE_USDC, Era::V2).unwrap();
        let (bin_target, bin_cd) = bin.call(CID, ONE_USDC, Era::V2).unwrap();
        assert_eq!(nr_cd, bin_cd, "{nr:?} V2 calldata must equal {bin:?}");
        assert_ne!(nr_target, bin_target, "only the target differs on V2");
        assert_eq!(
            nr_target,
            crate::exec_plane::settlement::redeem::NEG_RISK_CTF_COLLATERAL_ADAPTER
        );
    }
}

/// Determinism + a golden signature pin, anchored to the LEGACY [`Era::V1`] encoding (CTF target
/// with USDC.e collateral) so its digest is unchanged by this fix and the byte-exact pin still
/// regression-guards the domain / type-string / field-order / calldata wiring. (A V2 golden would
/// need a fresh green run to capture; `relayer_request_shape_and_routing` proves V2 routing +
/// per-field digest binding instead.) Mirrors `redeem_relayer`'s pin.
#[test]
fn request_v1_is_deterministic_with_a_golden_signature() {
    let a = build_split_merge_request(
        KEY,
        PROXY,
        CID,
        ONE_USDC,
        0,
        0,
        SplitMergeKind::SplitBinary,
        Era::V1,
    )
    .unwrap();
    let b = build_split_merge_request(
        KEY,
        PROXY,
        CID,
        ONE_USDC,
        0,
        0,
        SplitMergeKind::SplitBinary,
        Era::V1,
    )
    .unwrap();
    assert_eq!(a.signature, b.signature, "signature is deterministic");
    assert_eq!(a.body, b.body, "body is deterministic");
    assert_eq!(a.signature, GOLDEN_SPLIT_SIGNATURE, "EIP-712 Batch signature regression pin");
    // nonce and deadline are load-bearing: changing either must change the signature.
    let n1 = build_split_merge_request(
        KEY,
        PROXY,
        CID,
        ONE_USDC,
        1,
        0,
        SplitMergeKind::SplitBinary,
        Era::V1,
    )
    .unwrap();
    assert_ne!(a.signature, n1.signature, "nonce is bound into the digest");
    let d1 = build_split_merge_request(
        KEY,
        PROXY,
        CID,
        ONE_USDC,
        0,
        1,
        SplitMergeKind::SplitBinary,
        Era::V1,
    )
    .unwrap();
    assert_ne!(a.signature, d1.signature, "deadline is bound into the digest");
    // amount is bound in too (via the Call data hash).
    let a2 =
        build_split_merge_request(KEY, PROXY, CID, 2, 0, 0, SplitMergeKind::SplitBinary, Era::V1)
            .unwrap();
    assert_ne!(a.signature, a2.signature, "amount is bound into the digest");
    // and the era is bound in: the V2 encoding (new target + pUSD collateral) must differ.
    let v2 = build_split_merge_request(
        KEY,
        PROXY,
        CID,
        ONE_USDC,
        0,
        0,
        SplitMergeKind::SplitBinary,
        Era::V2,
    )
    .unwrap();
    assert_ne!(a.signature, v2.signature, "era is bound into the digest");
}

// Pinned from the first green run of the V1 encoding (secp256k1 is deterministic for a fixed key).
const GOLDEN_SPLIT_SIGNATURE: &str = "0xd0562479650088c3d69f23cb50ae6d2fe1e6a6acda9a33ccd307300e8966e06508bae28a6894113d57284c92ad04a17e6362de582d90a2366ac796993920c6d71b";

/// A malformed deposit wallet must hard-error, not silently zero-default (inherited from
/// `build_wallet_call_request`; asserted here so the split/merge entry point is covered too).
#[test]
fn rejects_malformed_proxy_addr() {
    for bad in ["0xbad", "0xzz", "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"] {
        assert!(
            build_split_merge_request(
                KEY,
                bad,
                CID,
                ONE_USDC,
                0,
                0,
                SplitMergeKind::SplitBinary,
                Era::V2,
            )
            .is_err(),
            "bad proxy {bad}"
        );
    }
    assert!(
        build_split_merge_request(
            KEY,
            PROXY,
            CID,
            ONE_USDC,
            0,
            0,
            SplitMergeKind::SplitBinary,
            Era::V2,
        )
        .is_ok(),
        "valid proxy"
    );
}
