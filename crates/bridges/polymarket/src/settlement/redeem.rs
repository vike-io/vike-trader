//! `redeem` — CTF + NegRiskAdapter `redeemPositions` calldata + gasless-relayer redeem (CTF
//! auto-redeem, docs/superpowers/specs/2026-07-11-ctf-redeem-design.md). Binary markets only
//! for the relayer wiring this PR; `redeem_neg_risk_calldata` is the pure ABI-encoder foundation
//! for neg-risk (multi-outcome) redemption, added as a fast-follow (Task 1 of the neg-risk plan).
//!
//! ⚠ **[`Era::V2`] NEG-RISK DOES NOT USE [`redeem_neg_risk_calldata`] — it uses the 4-arg CTF
//! encoder [`redeem_positions_calldata`], and differs from a binary redeem ONLY by target address.**
//! Read from the VERIFIED `NegRiskCtfCollateralAdapter` ABI + source on 2026-08-02 (blockscout,
//! `0xadA2005600Dec949baf300f4C6120000bDB6eAab`), replacing the earlier ASSUMPTION that V2 kept V1's
//! signature. That adapter's ABI does **not contain `redeemPositions(bytes32,uint256[])` at all**;
//! it exposes `redeemPositions(address,bytes32,bytes32,uint256[])` (`0x01b7037c`) because it EXTENDS
//! `CtfCollateralAdapter` — the very base the BINARY V2 adapter is. Submitting `0xdbeccb23` there
//! would have reverted on every neg-risk redeem. (The earlier `eth_getCode` probe that reported
//! "both selectors present" was misleading: `0xdbeccb23` is in the bytecode because the V2 adapter
//! FORWARDS to the retired V1 [`NEG_RISK_ADAPTER`] internally.)
//!
//! ⚠ And on V2 the `uint256[]` argument is **IGNORED**. Verified source:
//! `redeemPositions(address, bytes32, bytes32 _conditionId, uint256[] calldata)` — three params
//! UNNAMED ("Unnamed params retained for IConditionalTokens interface compatibility"); the adapter
//! reads `CONDITIONAL_TOKENS.balanceOf(msg.sender, positionIds[i])` for BOTH slots itself, pulls
//! them, redeems, wraps to pUSD and transfers back. So the per-slot-AMOUNTS pin below is **moot on
//! V2**, binding only for the retired [`Era::V1`] adapter — live V2 traffic carries `[1,2]`, `[1]`
//! and `[53000000,0]` interchangeably, all mined `ok`. [`crate::redeem_relayer::RedeemKind`]'s
//! `NegRisk` still carries the amounts because the V1 encoder genuinely needs them.
//!
//! PINNED ([`Era::V1`] ONLY): NegRiskAdapter `redeemPositions(bytes32,uint256[])`'s `uint256[]` = PER-SLOT AMOUNTS
//! (the redeemer's actual YES/NO conditional-token balances, `_amounts[0]`/`_amounts[1]`), NOT
//! index-sets. Confirmed against the on-chain contract source
//! (<https://github.com/Polymarket/neg-risk-ctf-adapter/blob/main/src/NegRiskAdapter.sol>,
//! `redeemPositions`): `_amounts` is passed directly as the `values` array of
//! `ctf.safeBatchTransferFrom(msg.sender, address(this), positionIds, _amounts, "")` — an ERC1155
//! batch-transfer value array, i.e. real token-unit amounts, not a `[1,2]`-style index-set
//! selector. NOTE: `OrderBookTrade/rs-builder-relayer-client`'s
//! (<https://github.com/OrderBookTrade/rs-builder-relayer-client/blob/main/src/operations/redeem.rs>)
//! `redeem_neg_risk_positions` names this parameter `index_sets` with a `[1, 2]` doc example —
//! that naming is MISLEADING/WRONG against the actual contract semantics and must NOT be
//! followed; the on-chain source is the ground truth this crate pins to. This governs what the
//! poller (Task 4) must supply: each Position's redeemable token balance per outcome slot, not a
//! constant `[1,2]`.

use vike_bridge_core::eip712::{enc_address, enc_uint, keccak256};

/// ConditionalTokens (CTF) on Polygon (chainId 137) — the Gnosis contract that ultimately mints /
/// burns / redeems outcome tokens. In the legacy [`Era::V1`] (USDC.e) era a binary op is submitted
/// DIRECTLY here; in the current [`Era::V2`] (pUSD) era it is submitted to [`CTF_COLLATERAL_ADAPTER`]
/// which wraps pUSD around this contract. This address is ALSO the settlement/read authority
/// `chain.rs` calls (`payoutNumerators` etc.) — that read use is era-independent and unchanged.
pub const CTF_ADDRESS: &str = "0x4D97DCd97eC945f40cF65F87097ACe5EA0476045";
/// USDC.e collateral on Polygon — the `collateralToken` arg for a LEGACY [`Era::V1`] binary redeem /
/// split / merge. Superseded by [`PUSD_COLLATERAL`] for all current activity (see [`Era`]).
pub const USDC_E_ADDRESS: &str = "0x2791Bca1f2de4661ED88A30C99A7a9449Aa84174";
/// ⚠ RETIRED — the LEGACY V1 NegRiskAdapter on Polygon. **Polymarket fully retired relayer calls to
/// this contract on 2026-07-17 (changelog 2026-07-14).** A relayer transaction targeting it now
/// fails on-chain, so it is kept ONLY as the [`Era::V1`] neg-risk target for a caller that positively
/// knows a position predates the migration. All current (V2) neg-risk ops route to
/// [`NEG_RISK_CTF_COLLATERAL_ADAPTER`] instead. Source: docs.polymarket.com/resources/contracts.
pub const NEG_RISK_ADAPTER: &str = "0xd91E80cF2E7be2e162c6513ceD06f1dD0dA35296";

// ── V2 (pUSD-era) contract addresses ─────────────────────────────────────────────────────────
// Polymarket migrated collateral from USDC.e to pUSD and retired the V1 relayer path on 2026-07-17
// (changelog 2026-07-14). New CTF operations route through dedicated collateral adapters. All three
// addresses are from docs.polymarket.com/resources/contracts, and [`CTF_COLLATERAL_ADAPTER`] is
// independently corroborated in-repo: `chain.rs` decoded this account's OWN live binary redemption
// with `redeemer == 0xada100db00ca00073811820692005400218fce1f` == this adapter.
//
// ⚠ ABI NOT re-verified: the V2 adapters are ASSUMED to expose the SAME function signatures (hence
// the SAME 4-byte selectors) as the V1 CTF / NegRiskAdapter — only the target address and (for
// binary ops) the collateral word change. The V1 selectors were bytecode-probed (see
// `split_merge.rs`'s module doc); the V2 adapters' selectors are NOT probed here. Confirming them via
// an arbdub `eth_getCode` probe + a live round-trip is part of the OWED wire-verify, still pending
// before any real-money enablement — this change makes the TARGET correct, it does not authorize it.

/// pUSD CollateralToken (Polygon) — the [`Era::V2`] `collateralToken` arg for a binary-CTF redeem /
/// split / merge. (Neg-risk calldata carries no collateral word.) Source: contracts page above.
pub const PUSD_COLLATERAL: &str = "0xC011a7E12a19f7B1f670d46F03B03f3342E82DFB";
/// CtfCollateralAdapter (Polygon) — the [`Era::V2`] target for pUSD **binary-CTF** ops. Corroborated
/// in-repo as this account's live-redemption `redeemer` (see `chain.rs`).
pub const CTF_COLLATERAL_ADAPTER: &str = "0xAdA100Db00Ca00073811820692005400218FcE1f";
/// NegRiskCtfCollateralAdapter (Polygon) — the [`Era::V2`] target for pUSD **neg-risk** ops
/// (redeem / split / merge / convert), replacing the retired V1 [`NEG_RISK_ADAPTER`].
pub const NEG_RISK_CTF_COLLATERAL_ADAPTER: &str = "0xadA2005600Dec949baf300f4C6120000bDB6eAab";

/// The collateral era of a Polymarket condition — which collateral token backs it, and therefore
/// which relayer target its CTF operations route through and which `collateralToken` a binary op
/// encodes.
///
/// **⚠ There is NO era field to read.** Neither the data-api `/positions` row ([`crate::positions`])
/// nor the Gamma market ([`crate::gamma`]) carries a collateral/era discriminator today — the only
/// per-position routing signal that IS parsed is `negativeRisk` (binary vs neg-risk), which is
/// ORTHOGONAL to the era. So a caller cannot determine the era from available data.
///
/// The safe default follows from the retirement: Polymarket is **V2-only for every new condition**,
/// and a **V1 relayer call now fails on-chain** ([`NEG_RISK_ADAPTER`] retired 2026-07-17). Therefore
/// [`Era::V2`] is the DEFAULT ([`Default`]) and the correct choice for all current activity;
/// [`Era::V1`] is retained ONLY for a caller that positively knows a position predates the migration.
/// A genuinely-legacy USDC.e position cannot be auto-detected here — see [`crate::auto_redeem`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Era {
    /// Legacy USDC.e-collateralized conditions: binary ops go DIRECT to [`CTF_ADDRESS`], neg-risk ops
    /// to the retired [`NEG_RISK_ADAPTER`]. Select only for a known-legacy position.
    V1,
    /// Current pUSD-collateralized conditions: binary ops route through [`CTF_COLLATERAL_ADAPTER`],
    /// neg-risk ops through [`NEG_RISK_CTF_COLLATERAL_ADAPTER`]. The default.
    #[default]
    V2,
}

impl Era {
    /// The `collateralToken` address a BINARY-CTF redeem/split/merge encodes for this era (USDC.e for
    /// V1, pUSD for V2). Neg-risk calldata has no collateral word, so this is unused on that path.
    pub fn collateral(self) -> &'static str {
        match self {
            Era::V1 => USDC_E_ADDRESS,
            Era::V2 => PUSD_COLLATERAL,
        }
    }

    /// The relayer target contract for a BINARY-CTF op (redeem/split/merge) in this era.
    pub fn binary_target(self) -> &'static str {
        match self {
            Era::V1 => CTF_ADDRESS,
            Era::V2 => CTF_COLLATERAL_ADAPTER,
        }
    }

    /// The relayer target contract for a NEG-RISK op (redeem/split/merge/convert) in this era.
    /// ⚠ V1 resolves to the RETIRED [`NEG_RISK_ADAPTER`] — see [`Era`].
    ///
    /// ⚠ The target follows the era, but so does the CALLDATA SHAPE, and not in the same way:
    /// the V2 adapter is a `CtfCollateralAdapter` subclass, so its redeem/split/merge take the
    /// 4-/5-arg CTF signatures (the BINARY encoders), while V1's take the 2-arg neg-risk overloads.
    /// Only `convertPositions(bytes32,uint256,uint256)` is neg-risk-native in BOTH eras. See the
    /// module doc; the per-era encoder choice lives in [`crate::redeem_relayer::RedeemKind::call`]
    /// and [`crate::split_merge::SplitMergeKind::call`].
    pub fn neg_risk_target(self) -> &'static str {
        match self {
            Era::V1 => NEG_RISK_ADAPTER,
            Era::V2 => NEG_RISK_CTF_COLLATERAL_ADAPTER,
        }
    }
}

/// ABI-encode `redeemPositions(address,bytes32,bytes32,uint256[])` calldata for a BINARY market:
/// `collateralToken = era.collateral()` (USDC.e for [`Era::V1`], pUSD for [`Era::V2`]),
/// `parentCollectionId = bytes32(0)`, `conditionId`, `indexSets = [1,2]`. Layout: selector(4) |
/// collateral(32) | parent(32) | condition(32) | array-offset(32=0x80) | array-len(32=2) | 1(32) |
/// 2(32). Uses the shared `eip712.rs` word encoders (bit-parity with the order-signing path). The
/// SELECTOR is era-independent — only the collateral word changes; the relayer TARGET
/// ([`Era::binary_target`]) changes with the era at the submit site. `condition_id_hex` is a
/// 0x-prefixed 32-byte hex string; a malformed or wrong-length conditionId is a hard `Err` (this is
/// on-chain calldata — a silently truncated or zero-padded id would redeem the wrong/no condition).
pub fn redeem_positions_calldata(condition_id_hex: &str, era: Era) -> Result<Vec<u8>, String> {
    let mut out = Vec::with_capacity(4 + 7 * 32);
    out.extend_from_slice(&keccak256(b"redeemPositions(address,bytes32,bytes32,uint256[])")[0..4]);
    out.extend_from_slice(&enc_address(era.collateral())); // collateralToken
    out.extend_from_slice(&[0u8; 32]); // parentCollectionId = bytes32(0)
    out.extend_from_slice(&bytes32_from_hex(condition_id_hex)?); // conditionId
    out.extend_from_slice(&enc_uint(0x80)); // offset to the dynamic uint256[] (4 head words * 32)
    out.extend_from_slice(&enc_uint(2)); // indexSets.length
    out.extend_from_slice(&enc_uint(1)); // indexSets[0]
    out.extend_from_slice(&enc_uint(2)); // indexSets[1]
    Ok(out)
}

/// ABI-encode `redeemPositions(bytes32,uint256[])` calldata for a NEG-RISK market — ⚠ **[`Era::V1`]
/// ONLY**, i.e. the RETIRED [`NEG_RISK_ADAPTER`], which is the only contract that exposes this
/// 2-arg overload. A V2 neg-risk redeem uses [`redeem_positions_calldata`] against
/// [`NEG_RISK_CTF_COLLATERAL_ADAPTER`] instead (module doc has the verified ABI); the routing is
/// [`crate::redeem_relayer::RedeemKind::call`]'s job, so no caller picks this by hand.
/// Layout: selector(4) | conditionId(32) | array-offset=0x40(32) |
/// len(32) | elems(32 each). `uint_array` is the `uint256[]` argument — per the module doc's
/// pinned NEG-RISK semantics, these are per-slot AMOUNTS (the redeemer's YES/NO token balances),
/// not index-sets — that is what the poller (Task 4) must supply. `condition_id_hex` is validated
/// the same way as [`redeem_positions_calldata`]: a malformed or wrong-length id is a hard `Err`.
pub fn redeem_neg_risk_calldata(
    condition_id_hex: &str,
    uint_array: &[u128],
) -> Result<Vec<u8>, String> {
    let cid = bytes32_from_hex(condition_id_hex)?;
    let mut out = Vec::with_capacity(4 + (3 + uint_array.len()) * 32);
    out.extend_from_slice(&keccak256(b"redeemPositions(bytes32,uint256[])")[0..4]);
    out.extend_from_slice(&cid); // conditionId
    out.extend_from_slice(&enc_uint(0x40)); // offset to the dynamic uint256[] (2 head words * 32)
    out.extend_from_slice(&enc_uint(uint_array.len() as u128)); // length
    for &v in uint_array {
        out.extend_from_slice(&enc_uint(v)); // each element
    }
    Ok(out)
}

/// A 0x-prefixed 32-byte hex string → a 32-byte word. `enc_uint256_dec` is for DECIMAL; conditionId
/// is HEX, so decode it directly. A valid bytes32 is EXACTLY 32 bytes (64 hex chars) — anything
/// else is rejected rather than silently truncated or zero-padded (real-money safety: this word is
/// the on-chain conditionId).
pub(crate) fn bytes32_from_hex(s: &str) -> Result<[u8; 32], String> {
    let clean = s.strip_prefix("0x").unwrap_or(s);
    let bytes = hex::decode(clean).map_err(|e| format!("conditionId not valid hex: {e}"))?;
    if bytes.len() != 32 {
        return Err(format!("conditionId must be 32 bytes, got {}", bytes.len()));
    }
    let mut out = [0u8; 32];
    out.copy_from_slice(&bytes);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The V2 (pUSD) redeem — the default, and what all current activity uses. The GOLDEN
    /// collateral word CHANGED from USDC.e to pUSD (the whole point of this fix); the selector is
    /// era-independent and unchanged.
    #[test]
    fn binary_redeem_calldata_v2_is_byte_exact() {
        // conditionId = 0x0000...0001 (32-byte hex) for a determinate vector.
        let cid = "0x0000000000000000000000000000000000000000000000000000000000000001";
        let cd = redeem_positions_calldata(cid, Era::V2).unwrap();
        // selector = keccak256("redeemPositions(address,bytes32,bytes32,uint256[])")[0..4]
        let sel = &vike_bridge_core::eip712::keccak256(
            b"redeemPositions(address,bytes32,bytes32,uint256[])",
        )[0..4];
        assert_eq!(&cd[0..4], sel, "selector");
        // Independent check against the KNOWN on-chain selector for the Gnosis CTF V1
        // `redeemPositions(address,bytes32,bytes32,uint256[])` = 0x01b7037c — verified against two
        // independent keccak implementations (tiny-keccak + pycryptodome). A typo in the signature
        // literal above would change the keccak output away from this, so BOTH must agree. The V2
        // routing changes ONLY the collateral word + the target address, NOT this selector.
        assert_eq!(
            &cd[0..4],
            &[0x01, 0xb7, 0x03, 0x7c],
            "redeemPositions selector (independent literal, era-independent)"
        );
        // layout: selector(4) | collateral(32) | parentCollectionId(32) | conditionId(32)
        //         | offset-to-array(32) | array-len(32) | elem0(32) | elem1(32)  = 4 + 7*32 = 228
        assert_eq!(cd.len(), 4 + 7 * 32, "total calldata length");
        // collateral = pUSD right-aligned (GOLDEN CHANGE: was USDC.e in the V1/pre-fix encoding)
        let coll = &cd[4 + 12..4 + 32];
        assert_eq!(hex::encode(coll), PUSD_COLLATERAL.trim_start_matches("0x").to_lowercase());
        // parentCollectionId = 0
        assert_eq!(&cd[4 + 32..4 + 64], &[0u8; 32]);
        // conditionId last byte = 1
        assert_eq!(cd[4 + 64 + 31], 1);
        // dynamic-array offset = 0x80 (128 = 4 words after the 4 head words)
        assert_eq!(cd[4 + 96 + 31], 0x80);
        // array length = 2
        assert_eq!(cd[4 + 128 + 31], 2);
        // indexSets [1, 2]
        assert_eq!(cd[4 + 160 + 31], 1);
        assert_eq!(cd[4 + 192 + 31], 2);
    }

    /// GOLDEN — REAL ON-CHAIN CALLDATA, not a self-derived vector. Polygon tx
    /// `0x1aef81b295f6603441c992e28e92bfe119c674061e2b6a66317c08d5001efe41`, block 91303901,
    /// 2026-08-02T08:54:32Z, `status=ok`: an **EOA calling
    /// `NegRiskCtfCollateralAdapter.redeemPositions` DIRECTLY** — so `tx.input` IS the raw calldata,
    /// with no proxy/relayer wrapper to strip. Captured keylessly via
    /// `polygon.blockscout.com/api/v2/addresses/<adapter>/transactions`.
    ///
    /// This pins the two things the neg-risk path had only ASSUMED (see the module doc): that a V2
    /// NEG-RISK redeem is the **4-arg CTF encoding** (`0x01b7037c`, pUSD collateral word, zero
    /// parent, `[1,2]` tail) and NOT `redeem_neg_risk_calldata`'s `0xdbeccb23`; and that our binary
    /// encoder reproduces real-world bytes exactly. Because the V2 adapter IGNORES all three of
    /// collateral / parentCollectionId / `uint256[]`, this same vector is simultaneously the
    /// golden for a V2 BINARY redeem — on V2 the two differ only by relayer target.
    #[test]
    fn v2_redeem_calldata_matches_real_onchain_tx() {
        // The conditionId of that live redemption.
        let cid = "0x381aaac1a78fa86befe8336d4f7d37bf3bd80feb450cf0b0122929f4d0c7dac6";
        let onchain = concat!(
            "01b7037c",
            "000000000000000000000000c011a7e12a19f7b1f670d46f03b03f3342e82dfb",
            "0000000000000000000000000000000000000000000000000000000000000000",
            "381aaac1a78fa86befe8336d4f7d37bf3bd80feb450cf0b0122929f4d0c7dac6",
            "0000000000000000000000000000000000000000000000000000000000000080",
            "0000000000000000000000000000000000000000000000000000000000000002",
            "0000000000000000000000000000000000000000000000000000000000000001",
            "0000000000000000000000000000000000000000000000000000000000000002",
        );
        let ours = hex::encode(redeem_positions_calldata(cid, Era::V2).unwrap());
        assert_eq!(ours, onchain, "V2 redeem calldata must equal the real on-chain tx input");
        // And the retired 2-arg neg-risk encoding is NOT what that adapter takes — the bug this
        // vector caught. Byte-different from the first byte (selector) on.
        let v1_shape = hex::encode(redeem_neg_risk_calldata(cid, &[1, 2]).unwrap());
        assert_ne!(v1_shape, onchain, "0xdbeccb23 is not the V2 neg-risk encoding");
        assert!(v1_shape.starts_with("dbeccb23"), "V1 neg-risk selector unchanged");
    }

    /// The legacy V1 (USDC.e) redeem — retained for known-legacy positions. Byte-identical to the V2
    /// encoding EXCEPT the collateral word, which is USDC.e here.
    #[test]
    fn binary_redeem_calldata_v1_keeps_usdc_e() {
        let cid = "0x0000000000000000000000000000000000000000000000000000000000000001";
        let v1 = redeem_positions_calldata(cid, Era::V1).unwrap();
        let v2 = redeem_positions_calldata(cid, Era::V2).unwrap();
        // selector unchanged across eras
        assert_eq!(&v1[0..4], &[0x01, 0xb7, 0x03, 0x7c], "selector era-independent");
        // V1 collateral = USDC.e
        let coll = &v1[4 + 12..4 + 32];
        assert_eq!(hex::encode(coll), USDC_E_ADDRESS.trim_start_matches("0x").to_lowercase());
        // ONLY the collateral word (bytes 4..36) differs between the two eras.
        assert_ne!(&v1[4..36], &v2[4..36], "collateral word differs by era");
        assert_eq!(&v1[0..4], &v2[0..4], "selector identical");
        assert_eq!(&v1[36..], &v2[36..], "everything after the collateral word is identical");
    }

    #[test]
    fn rejects_malformed_condition_id() {
        // Too short (2 bytes) — must NOT be zero-padded up to a plausible-but-wrong word.
        assert!(redeem_positions_calldata("0xdead", Era::V2).is_err(), "too-short conditionId");
        // Too long (33 bytes) — must NOT be truncated down to 32.
        let too_long = format!("0x{}", "aa".repeat(33));
        assert!(redeem_positions_calldata(&too_long, Era::V2).is_err(), "too-long conditionId");
        // Non-hex garbage.
        assert!(redeem_positions_calldata("0xzzzz", Era::V2).is_err(), "non-hex conditionId");
    }

    /// The era selectors resolve to the exact new/old addresses this fix pins.
    #[test]
    fn era_routing_addresses() {
        assert_eq!(Era::default(), Era::V2, "V2 is the default era");
        assert_eq!(Era::V2.collateral(), PUSD_COLLATERAL);
        assert_eq!(Era::V2.binary_target(), CTF_COLLATERAL_ADAPTER);
        assert_eq!(Era::V2.neg_risk_target(), NEG_RISK_CTF_COLLATERAL_ADAPTER);
        assert_eq!(Era::V1.collateral(), USDC_E_ADDRESS);
        assert_eq!(Era::V1.binary_target(), CTF_ADDRESS);
        assert_eq!(Era::V1.neg_risk_target(), NEG_RISK_ADAPTER);
        // the retired V1 neg-risk target must NOT be the current one (the bug this fixes).
        assert_ne!(Era::V1.neg_risk_target(), Era::V2.neg_risk_target());
    }

    #[test]
    fn neg_risk_redeem_calldata_is_byte_exact() {
        let cid = "0x0000000000000000000000000000000000000000000000000000000000000001";
        let cd = redeem_neg_risk_calldata(cid, &[1, 2]).unwrap();
        // selector = keccak256("redeemPositions(bytes32,uint256[])")[0..4]
        let sel = &vike_bridge_core::eip712::keccak256(b"redeemPositions(bytes32,uint256[])")[0..4];
        assert_eq!(&cd[0..4], sel, "derived selector");
        // Independent literal, verified via two independent keccak implementations
        // (pycryptodome + eth_utils, both agreeing, and both correctly reproducing the KNOWN
        // binary-redeem selector 0x01b7037c as a methodology sanity check). The plan's
        // placeholder literal 0x9c9a2553 was WRONG (unverified guess) — this is the real value,
        // exactly the #125 discipline that caught the wrong binary selector 0x9e7212ad.
        assert_eq!(
            &cd[0..4],
            &[0xdb, 0xec, 0xcb, 0x23],
            "neg-risk redeemPositions selector literal"
        );
        // layout: selector(4) | conditionId(32) | array-offset=0x40(32) | len=2(32) | 1(32) | 2(32) = 4 + 5*32 = 164
        assert_eq!(cd.len(), 4 + 5 * 32, "total length");
        assert_eq!(cd[4 + 31], 1, "conditionId low byte");
        assert_eq!(cd[4 + 32 + 31], 0x40, "uint256[] offset = 2 head words * 32");
        assert_eq!(cd[4 + 64 + 31], 2, "array length");
        assert_eq!(cd[4 + 96 + 31], 1);
        assert_eq!(cd[4 + 128 + 31], 2);
    }

    #[test]
    fn neg_risk_rejects_malformed_condition_id() {
        assert!(redeem_neg_risk_calldata("0xdead", &[1, 2]).is_err());
    }
}
