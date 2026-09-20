//! `split_merge` — CTF + NegRiskAdapter `splitPosition` / `mergePositions` calldata + gasless-relayer
//! submit. The mint/burn twin of `redeem.rs`: redeem converts a RESOLVED position back to collateral,
//! split mints a complete outcome set FROM collateral, and merge burns a complete set BACK to
//! collateral — both available at any time, resolved or not.
//!
//! Structure deliberately mirrors `redeem.rs` + `redeem_relayer.rs` exactly (pure verified-encoding
//! calldata here, EIP-712 `Batch` signing + `/submit` reused from `redeem_relayer`), including their
//! test discipline: golden calldata hex, derived-vs-literal selector cross-checks, and NO live
//! submission from any test.
//!
//! ## VERIFIED FROM SOURCE — function signatures
//! Gnosis ConditionalTokens (the contract at [`crate::redeem::CTF_ADDRESS`],
//! <https://github.com/gnosis/conditional-tokens-contracts/blob/master/contracts/ConditionalTokens.sol>):
//! ```solidity
//! function splitPosition (IERC20 collateralToken, bytes32 parentCollectionId, bytes32 conditionId, uint[] partition, uint amount) external
//! function mergePositions(IERC20 collateralToken, bytes32 parentCollectionId, bytes32 conditionId, uint[] partition, uint amount) external
//! ```
//! `IERC20` ABI-encodes as `address`, and bare `uint`/`uint[]` are `uint256`/`uint256[]`, so the
//! canonical signature strings this module keccaks are:
//! - `splitPosition(address,bytes32,bytes32,uint256[],uint256)`  → selector `0x72ce4275`
//! - `mergePositions(address,bytes32,bytes32,uint256[],uint256)` → selector `0x9e7212ad`
//!
//! Both selectors are CROSS-CHECKED two ways (the #125 discipline that caught a wrong redeem
//! selector): derived here from the exact signature literal via this crate's `keccak256`, AND pinned
//! as an independent byte literal in the tests, sourced from the public 4byte.directory registry
//! (`/api/v1/signatures/?text_signature=…`). A typo in either the signature string or the literal
//! makes the two disagree and the test fails. Corroborating datapoint: `redeem.rs`'s module history
//! records `0x9e7212ad` having once been mistaken for a *redeem* selector — it is in fact
//! `mergePositions`, exactly as pinned here.
//!
//! ## NegRiskAdapter DOES expose split/merge (so this is NOT binary-only)
//! <https://github.com/Polymarket/neg-risk-ctf-adapter/blob/main/src/NegRiskAdapter.sol> declares
//! TWO overloads of each. The 5-arg one is a CTF-compatible shim whose `parentCollectionId` and
//! `partition` params are UNNAMED and IGNORED; the short one is the real entry point:
//! ```solidity
//! function splitPosition (bytes32 _conditionId, uint256 _amount) public   // selector 0xa3d7da1d
//! function mergePositions(bytes32 _conditionId, uint256 _amount) public   // selector 0xb10c5c17
//! ```
//! This module encodes the SHORT overloads for neg-risk (fewer words on the wire, and the form the
//! adapter's own callers use), likewise derived-and-literal cross-checked. Note these are STATIC —
//! two head words, no dynamic-array offset — unlike every other encoder in this pair of modules.
//!
//! ⚠ **…but ONLY on [`crate::redeem::Era::V1`].** Everything above describes the RETIRED
//! `NegRiskAdapter`, which is what the 2026-07-22 bytecode probe below was run against. The CURRENT
//! [`crate::redeem::Era::V2`] target is a DIFFERENT contract — `NegRiskCtfCollateralAdapter`
//! (`0xadA2005600Dec949baf300f4C6120000bDB6eAab`), which extends `CtfCollateralAdapter` and whose
//! VERIFIED ABI has NO short overload at all: its `splitPosition`/`mergePositions`/`redeemPositions`
//! are the 5-/5-/4-arg CTF signatures, with collateral / parentCollectionId / partition unnamed and
//! ignored and only `_conditionId` + `_amount` read. So on V2 a neg-risk split/merge is BYTE-
//! IDENTICAL to its binary twin and differs only by target; sending `0xa3d7da1d`/`0xb10c5c17`
//! there would revert. [`SplitMergeKind::call`] is where the era picks the encoder, and
//! `redeem.rs`'s module doc carries the full finding (read from the verified source, 2026-08-02).
//! `convertPositions` below is the ONE neg-risk-native call present in both eras, unaffected.
//!
//! ## NegRiskAdapter `convertPositions` — signature VERIFIED AGAINST DEPLOYED BYTECODE
//! ```solidity
//! function convertPositions(bytes32 _marketId, uint256 _indexSet, uint256 _amount) external
//! ```
//! → selector `0xc64748c4`, derived here from the signature literal and pinned as an independent
//! byte literal in the tests, exactly like the four encoders above.
//!
//! This one got a THIRD, stronger check, because a wrong selector on a position-transforming call
//! is a lost transaction: the deployed [`crate::redeem::NEG_RISK_ADAPTER`] bytecode was fetched from a
//! public Polygon RPC (`eth_getCode`, 17 KB) and the candidate selectors probed against its
//! dispatch table (2026-07-22, via arbdub):
//!
//! | candidate signature | selector | in dispatch table |
//! |---|---|---|
//! | `convertPositions(bytes32,uint256,uint256)`         | `c64748c4` | **YES** (`PUSH4 c64748c4`) |
//! | `convertPositions(bytes32,uint256,uint256,address)` | `0ddf2fce` | no |
//! | `convertPositions(bytes32,uint256[],uint256)`       | `b5b50c23` | no |
//! | `convertPositions(bytes32,uint256)`                 | `da17ef56` | no |
//!
//! The same probe reproduced this module's and `redeem.rs`'s already-pinned selectors from the same
//! bytecode (`dbeccb23` redeem, `a3d7da1d` split, `b10c5c17` merge), which is what validates the
//! probe itself. Corroborating neighbours also present: `getConditionId(bytes32)` `04329c03`,
//! `getPositionId(bytes32,bool)` `752b5ba5`, `getMarketData(bytes32)` `30f4f4bb`.
//!
//! And the contract SOURCE agrees with the bytecode, declaration and semantics both
//! (<https://github.com/Polymarket/neg-risk-ctf-adapter/blob/main/src/NegRiskAdapter.sol>):
//! ```solidity
//! /// @notice Convert a set of no positions to the complementary set of yes positions plus collateral proportional to
//! /// (# of no positions - 1)
//! /// @notice If the market has a fee, the fee is taken from both collateral and the yes positions
//! /// @param _marketId - the marketId
//! /// @param _indexSet - the set of positions to convert, expressed as an index set where the least significant bit is
//! /// the first question (index zero)
//! /// @param _amount   - the amount of tokens to convert
//! function convertPositions(bytes32 _marketId, uint256 _indexSet, uint256 _amount) external
//! ```
//! — which pins BOTH footguns below straight from the source: the first word is a `_marketId`, and
//! `_indexSet` is LSB-is-index-0 bitmap.
//!
//! ### What it does, and why `_marketId` is NOT a conditionId
//! `convertPositions` is the neg-risk-only operation with no CTF analogue: it burns NO positions
//! across a chosen subset of a set's outcomes and returns the complementary YES positions plus
//! collateral. Its first word is the **`negRiskMarketID`** — the SET key
//! ([`crate::neg_risk_set::NegRiskSet::market_id`]) — **not** a per-market `conditionId`, which is
//! what every other encoder in this pair of modules takes. Passing a conditionId here would address
//! a nonexistent market. The distinction is enforced by naming
//! ([`convert_positions_calldata`]'s parameter is `neg_risk_market_id_hex`) and stated in its doc.
//!
//! `_indexSet` is a **BITMAP over 0-based outcome indices** — bit `i` set means outcome `i` is in
//! the converted subset — the same index space as `groupItemThreshold` / `questionID`'s last byte
//! (see `gamma.rs`'s module doc). [`index_set_from_indices`] builds it; there is no per-slot-amounts
//! subtlety here, unlike `redeemPositions`'s `uint256[]`.
//!
//! ### PLUMBING-ONLY submit path (dedicated, NOT via `SplitMergeKind`)
//! `convertPositions` takes THREE args (`marketId`, `indexSet`, `amount`), not the
//! `(conditionId, amount)` pair [`SplitMergeKind::call`] dispatches, so it gets its OWN
//! [`build_convert_request`] / [`submit_convert`] pair rather than an enum variant — reusing the
//! SAME `build_wallet_call_request` / `submit_wallet_call` `Batch`-signing + `/submit` path the
//! split/merge and redeem sides use. It stays PLUMBING ONLY: no auto-executor, no poller, no
//! strategy wiring, and no caller anywhere in the crate — nothing runs unless a human explicitly
//! calls it. (The `convert_arb` detector SCREENS for the NO-leg opportunity this call would realize,
//! but it never sends — it emits sized opportunities and stops.) The submit path carries the same
//! `DANGER — real funds, NOT live-verified` warning as [`submit_split_merge`]; the live round-trip is
//! still OWED before any real-money call (see the DEFERRED section).
//!
//! ## `amount` units
//! `amount` is in collateral base units: USDC.e has 6 decimals, so `1_000_000` == $1.00, and the
//! complete outcome set minted/burned is 1e6 units of EACH outcome token. Callers pass integer base
//! units (`u128`) — there is no float anywhere on this path.
//!
//! ## DEFERRED — live wire verification (mirrors how redeem was built and shipped)
//! The calldata encoders are verified against contract source + an independent selector registry, and
//! the relayer envelope is the SAME EIP-712 `Batch` path the redeem work pinned from
//! `rs-builder-relayer-client`. But NO split, merge, or convert has been submitted to the live
//! relayer from this code. An arbdub demo round-trip (headers accepted → batch mined → outcome-token
//! balances actually move) is OWED before anything calls [`submit_split_merge`] / [`submit_convert`]
//! with real funds — exactly the posture `redeem_relayer.rs` documents for `POLY_AUTO_REDEEM`. This
//! module deliberately ships
//! PLUMBING ONLY: no auto-executor, no poller, no strategy wiring, and no caller anywhere in the
//! crate. Nothing runs unless a human explicitly calls it.
//!
//! Secrets: the deterministic request carries no relayer key (headers are joined at submit time) and
//! the private key never appears in any returned value or log.

use vike_bridge_core::eip712::{enc_address, enc_uint, keccak256};

use crate::config::PolymarketCreds;
use crate::redeem::{Era, bytes32_from_hex};
use crate::redeem_relayer::{
    RedeemRequest, RedeemResult, build_wallet_call_request, submit_wallet_call,
};

/// The CTF `splitPosition` signature — see the module doc for the source-verified declaration.
const CTF_SPLIT_SIG: &[u8] = b"splitPosition(address,bytes32,bytes32,uint256[],uint256)";
/// The CTF `mergePositions` signature — see the module doc for the source-verified declaration.
const CTF_MERGE_SIG: &[u8] = b"mergePositions(address,bytes32,bytes32,uint256[],uint256)";
/// The NegRiskAdapter short `splitPosition` overload.
const NR_SPLIT_SIG: &[u8] = b"splitPosition(bytes32,uint256)";
/// The NegRiskAdapter short `mergePositions` overload.
const NR_MERGE_SIG: &[u8] = b"mergePositions(bytes32,uint256)";
/// The NegRiskAdapter `convertPositions` entry point — bytecode-verified, see the module doc.
const NR_CONVERT_SIG: &[u8] = b"convertPositions(bytes32,uint256,uint256)";

/// ABI-encode the CTF 5-arg `splitPosition` / `mergePositions` calldata for a BINARY market:
/// `collateralToken = era.collateral()` (USDC.e for [`Era::V1`], pUSD for [`Era::V2`]),
/// `parentCollectionId = bytes32(0)`, `conditionId`, `partition = [1,2]`, `amount`. Layout:
/// selector(4) | collateral(32) | parent(32) | condition(32) | partition-offset(32 = 0xa0) |
/// amount(32) | partition-len(32 = 2) | 1(32) | 2(32) = 4 + 8*32 = 260 bytes. The SELECTOR is
/// era-independent — only the collateral word changes; the relayer TARGET
/// ([`Era::binary_target`]) changes with the era at the submit site.
///
/// NOTE the head/tail split that is easy to get wrong: `amount` is a STATIC 5th head word, so it sits
/// BEFORE the dynamic `partition` tail even though it is the LAST declared parameter, and the offset
/// is 5 head words (0xa0), not 4 (0x80) as in `redeem_positions_calldata`.
///
/// `condition_id_hex` is validated exactly as in `redeem.rs` — a malformed or wrong-length id is a
/// hard `Err`, never silently truncated or zero-padded (this is on-chain calldata for real funds).
fn ctf_split_merge_calldata(
    selector_sig: &[u8],
    condition_id_hex: &str,
    amount: u128,
    era: Era,
) -> Result<Vec<u8>, String> {
    let cid = bytes32_from_hex(condition_id_hex)?;
    let mut out = Vec::with_capacity(4 + 8 * 32);
    out.extend_from_slice(&keccak256(selector_sig)[0..4]);
    out.extend_from_slice(&enc_address(era.collateral())); // collateralToken
    out.extend_from_slice(&[0u8; 32]); // parentCollectionId = bytes32(0)
    out.extend_from_slice(&cid); // conditionId
    out.extend_from_slice(&enc_uint(0xa0)); // offset to partition (5 head words * 32)
    out.extend_from_slice(&enc_uint(amount)); // amount (static head word)
    out.extend_from_slice(&enc_uint(2)); // partition.length
    out.extend_from_slice(&enc_uint(1)); // partition[0] — the YES index-set
    out.extend_from_slice(&enc_uint(2)); // partition[1] — the NO index-set
    Ok(out)
}

/// ABI-encode the NegRiskAdapter SHORT overload `(bytes32 conditionId, uint256 amount)`. Fully
/// STATIC: selector(4) | condition(32) | amount(32) = 68 bytes, no dynamic-array offset.
fn neg_risk_split_merge_calldata(
    selector_sig: &[u8],
    condition_id_hex: &str,
    amount: u128,
) -> Result<Vec<u8>, String> {
    let cid = bytes32_from_hex(condition_id_hex)?;
    let mut out = Vec::with_capacity(4 + 2 * 32);
    out.extend_from_slice(&keccak256(selector_sig)[0..4]);
    out.extend_from_slice(&cid); // conditionId
    out.extend_from_slice(&enc_uint(amount)); // amount
    Ok(out)
}

/// CTF `splitPosition(address,bytes32,bytes32,uint256[],uint256)` calldata for a binary market —
/// mints `amount` of BOTH outcome tokens from `amount` of the era's collateral (pUSD for
/// [`Era::V2`], USDC.e for [`Era::V1`]). See [`ctf_split_merge_calldata`] for the layout and the
/// validation contract.
pub fn split_position_calldata(
    condition_id_hex: &str,
    amount: u128,
    era: Era,
) -> Result<Vec<u8>, String> {
    ctf_split_merge_calldata(CTF_SPLIT_SIG, condition_id_hex, amount, era)
}

/// CTF `mergePositions(address,bytes32,bytes32,uint256[],uint256)` calldata for a binary market —
/// burns `amount` of BOTH outcome tokens back into `amount` of the era's collateral.
pub fn merge_positions_calldata(
    condition_id_hex: &str,
    amount: u128,
    era: Era,
) -> Result<Vec<u8>, String> {
    ctf_split_merge_calldata(CTF_MERGE_SIG, condition_id_hex, amount, era)
}

/// NegRisk-adapter `splitPosition(bytes32,uint256)` calldata (the short overload; caller targets the
/// era's neg-risk adapter via [`Era::neg_risk_target`], not the CTF). Calldata is era-independent —
/// the neg-risk short overload carries no collateral word.
pub fn split_position_neg_risk_calldata(
    condition_id_hex: &str,
    amount: u128,
) -> Result<Vec<u8>, String> {
    neg_risk_split_merge_calldata(NR_SPLIT_SIG, condition_id_hex, amount)
}

/// NegRisk-adapter `mergePositions(bytes32,uint256)` calldata (the short overload; caller targets the
/// era's neg-risk adapter via [`Era::neg_risk_target`], not the CTF).
pub fn merge_positions_neg_risk_calldata(
    condition_id_hex: &str,
    amount: u128,
) -> Result<Vec<u8>, String> {
    neg_risk_split_merge_calldata(NR_MERGE_SIG, condition_id_hex, amount)
}

/// Build a `convertPositions` `_indexSet` BITMAP from 0-based outcome indices: bit `i` set means
/// outcome `i` is part of the converted subset. `[0, 2]` → `0b101` = `5`.
///
/// A duplicate index is idempotent (setting a set bit again is a no-op). An index `>= 128` is a
/// hard `Err` rather than a silent wrap: `u128` is this crate's calldata word type, so a larger
/// index cannot be represented and MUST NOT round-trip as some other outcome — on-chain that would
/// convert the wrong legs. An EMPTY index set is also an `Err`: converting nothing is never the
/// intent and the adapter would revert or no-op depending on version.
///
/// (The on-chain word is a full `uint256`, so sets in bits 128..255 are expressible on chain but
/// not by this helper. No observed neg-risk set is anywhere near that wide — the largest live event
/// seen was 128 outcomes, indices `0x00..0x7f` — and the ceiling is enforced, not assumed.)
pub fn index_set_from_indices(indices: &[u32]) -> Result<u128, String> {
    if indices.is_empty() {
        return Err("convertPositions index set must not be empty".to_string());
    }
    let mut bits: u128 = 0;
    for &i in indices {
        if i >= 128 {
            return Err(format!("outcome index {i} exceeds this encoder's 128-bit index set"));
        }
        bits |= 1u128 << i;
    }
    Ok(bits)
}

/// ABI-encode NegRiskAdapter `convertPositions(bytes32,uint256,uint256)` calldata — converts NO
/// positions across the outcomes selected by `index_set` into the complementary YES positions plus
/// collateral. Selector `0xc64748c4`, verified against the deployed contract's dispatch table (see
/// the module doc). Fully STATIC: selector(4) | marketId(32) | indexSet(32) | amount(32) = 100
/// bytes, no dynamic-array offset.
///
/// ⚠ `neg_risk_market_id_hex` is the SET key `negRiskMarketID`
/// ([`crate::neg_risk_set::NegRiskSet::market_id`]) — **NOT** a per-market `conditionId` like every
/// other encoder in this module takes. A malformed or wrong-length value is a hard `Err`, never
/// truncated or zero-padded.
///
/// `index_set` is the bitmap from [`index_set_from_indices`]; `amount` is in collateral base units
/// (USDC.e, 6 decimals — `1_000_000` == $1.00), same as split/merge.
///
/// PLUMBING ONLY: its only in-crate callers are [`build_convert_request`] / [`submit_convert`] (the
/// dedicated relayer path — a `convertPositions` has no [`SplitMergeKind`] variant), and THEY have
/// no caller either. See the module doc.
pub fn convert_positions_calldata(
    neg_risk_market_id_hex: &str,
    index_set: u128,
    amount: u128,
) -> Result<Vec<u8>, String> {
    // `bytes32_from_hex`'s message names a conditionId (its original caller); re-label so a bad
    // marketId doesn't send the operator hunting the wrong field.
    let mid = bytes32_from_hex(neg_risk_market_id_hex)
        .map_err(|e| e.replace("conditionId", "negRiskMarketID"))?;
    let mut out = Vec::with_capacity(4 + 3 * 32);
    out.extend_from_slice(&keccak256(NR_CONVERT_SIG)[0..4]);
    out.extend_from_slice(&mid); // _marketId (the SET key, not a conditionId)
    out.extend_from_slice(&enc_uint(index_set)); // _indexSet (bitmap over outcome indices)
    out.extend_from_slice(&enc_uint(amount)); // _amount (collateral base units)
    Ok(out)
}

/// Which split/merge call to make — the mint/burn analog of [`crate::redeem_relayer::RedeemKind`].
/// Selects the target contract + calldata encoder; the EIP-712 `Batch` signing and the `/submit`
/// POST are identical for all four.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SplitMergeKind {
    /// CTF `splitPosition`, binary market (partition `[1,2]`): collateral → complete outcome set.
    SplitBinary,
    /// CTF `mergePositions`, binary market: complete outcome set → collateral.
    MergeBinary,
    /// Neg-risk split. ⚠ The ENCODING is era-dependent — the 2-arg
    /// `splitPosition(bytes32,uint256)` only on the retired [`Era::V1`] adapter, the 5-arg CTF
    /// `splitPosition` on [`Era::V2`]. See [`SplitMergeKind::call`].
    SplitNegRisk,
    /// Neg-risk merge. ⚠ Era-dependent encoding, same as [`SplitMergeKind::SplitNegRisk`].
    MergeNegRisk,
}

impl SplitMergeKind {
    /// The `(target contract, calldata)` pair for this kind in the given collateral `era` — the
    /// target ([`Era::binary_target`]/[`Era::neg_risk_target`]) and, for a binary op, the calldata's
    /// collateral word both follow the era. Pass [`Era::V2`] for current activity (see [`Era`]).
    ///
    /// ⚠ **The era also changes the NEG-RISK ENCODER, not just the target** — the identical defect
    /// fixed on the redeem path (see [`crate::redeem_relayer::RedeemKind::call`] and `redeem.rs`'s
    /// module doc). [`Era::V2`]'s `NegRiskCtfCollateralAdapter` extends `CtfCollateralAdapter`, so
    /// its VERIFIED ABI exposes `splitPosition(address,bytes32,bytes32,uint256[],uint256)` and
    /// `mergePositions(address,bytes32,bytes32,uint256[],uint256)` — the BINARY (CTF) encoders,
    /// whose collateral / parentCollectionId / partition words are unnamed and ignored, only
    /// `_conditionId` + `_amount` being read. The 2-arg neg-risk overloads exist ONLY on the
    /// retired V1 adapter, so sending them to V2 would revert. (`convertPositions(bytes32,uint256,
    /// uint256)` is the one neg-risk-native call present in BOTH eras and is unaffected — it has no
    /// [`SplitMergeKind`] variant.)
    pub fn call(
        self,
        condition_id_hex: &str,
        amount: u128,
        era: Era,
    ) -> Result<(&'static str, Vec<u8>), String> {
        Ok(match (self, era) {
            (Self::SplitBinary, _) => {
                (era.binary_target(), split_position_calldata(condition_id_hex, amount, era)?)
            }
            (Self::MergeBinary, _) => {
                (era.binary_target(), merge_positions_calldata(condition_id_hex, amount, era)?)
            }
            // V2: the CTF-shaped encoder aimed at the NEG-RISK target — same calldata as the binary
            // op, different contract.
            (Self::SplitNegRisk, Era::V2) => {
                (era.neg_risk_target(), split_position_calldata(condition_id_hex, amount, era)?)
            }
            (Self::MergeNegRisk, Era::V2) => {
                (era.neg_risk_target(), merge_positions_calldata(condition_id_hex, amount, era)?)
            }
            // V1 (retired): the 2-arg neg-risk overloads, which only that adapter implements.
            (Self::SplitNegRisk, Era::V1) => {
                (era.neg_risk_target(), split_position_neg_risk_calldata(condition_id_hex, amount)?)
            }
            (Self::MergeNegRisk, Era::V1) => (
                era.neg_risk_target(),
                merge_positions_neg_risk_calldata(condition_id_hex, amount)?,
            ),
        })
    }
}

/// Build + EIP-712-sign the gasless-relayer split/merge request — PURE and DETERMINISTIC given
/// `(private_key, proxy_addr, condition_id, amount, nonce, deadline, kind)`. The exact twin of
/// [`crate::redeem_relayer::build_redeem_request`], reusing its `Batch` signing and `WALLET`
/// envelope verbatim; only the `Call{target, data}` differs.
///
/// `nonce` and `deadline` are explicit args for the same reason as in the redeem path: both are
/// LOAD-BEARING inputs to the signed digest, so a defaulted placeholder would produce a signature
/// that reverts on-chain. [`submit_split_merge`] fetches the live nonce; tests pass fixed values for
/// a stable signature.
///
/// `proxy_addr` is the account's deposit wallet (== EIP-712 `verifyingContract` == `Batch.wallet`).
// A signed calldata builder: signer/proxy/condition/index-set/amount/kind/era/nonce are all
// intrinsic wire inputs — bundling them into a struct would just move the arg list, not shrink it.
#[allow(clippy::too_many_arguments)]
pub fn build_split_merge_request(
    private_key: &str,
    proxy_addr: &str,
    condition_id: &str,
    amount: u128,
    nonce: u64,
    deadline: u64,
    kind: SplitMergeKind,
    era: Era,
) -> Result<RedeemRequest, String> {
    let (target, calldata) = kind.call(condition_id, amount, era)?;
    build_wallet_call_request(private_key, proxy_addr, target, &calldata, nonce, deadline)
}

/// Submit a gasless split/merge for `proxy_addr`'s deposit wallet: fetch the live batch nonce,
/// build and sign the batch, POST it to the relayer, parse the result. Thin network wrapper over
/// [`build_split_merge_request`] (shares [`submit_wallet_call`] with the redeem path).
///
/// **DANGER — real funds, and NOT live-verified.** This POSTs a REAL EIP-712-signed batch to the
/// LIVE relayer and moves real money on-chain. Unlike redeem it has no proven round-trip at all yet
/// (see the module doc's DEFERRED section — an arbdub demo split+merge is OWED first). There is NO
/// internal opt-in guard and, deliberately, NO caller in this crate: the only gate is whoever calls
/// it. Do NOT call this from any always-on path, startup, resync, poller, or test that isn't an
/// explicitly `#[ignore]`d live smoke.
pub fn submit_split_merge(
    creds: &PolymarketCreds,
    proxy_addr: &str,
    condition_id: &str,
    amount: u128,
    kind: SplitMergeKind,
    era: Era,
) -> Result<RedeemResult, String> {
    let (target, calldata) = kind.call(condition_id, amount, era)?;
    submit_wallet_call(creds, proxy_addr, target, &calldata)
}

/// Build + EIP-712-sign the gasless-relayer `convertPositions` request — PURE and DETERMINISTIC
/// given `(private_key, proxy_addr, neg_risk_market_id, index_set, amount, nonce, deadline, era)`.
/// The convert twin of [`build_split_merge_request`], reusing [`crate::redeem_relayer`]'s
/// `build_wallet_call_request` `Batch` signing + `WALLET` envelope verbatim; only the
/// `Call{target: era.neg_risk_target(), data: convertPositions calldata}` differs. `era` routes the
/// neg-risk adapter (V2 default; V1 = the retired adapter — see [`Era`]).
///
/// It gets its OWN entry point rather than a [`SplitMergeKind`] variant because `convertPositions`
/// takes THREE args (`marketId`, `indexSet`, `amount`) — not the `(conditionId, amount)` pair
/// [`SplitMergeKind::call`] dispatches. `neg_risk_market_id` is the SET key `negRiskMarketID` (NOT a
/// per-market `conditionId` — see [`convert_positions_calldata`]); `index_set` is the
/// [`index_set_from_indices`] bitmap; `amount` is collateral base units.
///
/// `nonce`/`deadline` are explicit for the same reason as in the redeem/split-merge paths: both are
/// LOAD-BEARING inputs to the signed digest, so a defaulted placeholder would produce a signature
/// that reverts on-chain. [`submit_convert`] fetches the live nonce; tests pass fixed values.
// Signed calldata builder — all args are intrinsic wire inputs to the digest (see the sibling
// build_split_merge_request); a struct would move the arg list, not shrink it.
#[allow(clippy::too_many_arguments)]
pub fn build_convert_request(
    private_key: &str,
    proxy_addr: &str,
    neg_risk_market_id: &str,
    index_set: u128,
    amount: u128,
    nonce: u64,
    deadline: u64,
    era: Era,
) -> Result<RedeemRequest, String> {
    let calldata = convert_positions_calldata(neg_risk_market_id, index_set, amount)?;
    build_wallet_call_request(
        private_key,
        proxy_addr,
        era.neg_risk_target(),
        &calldata,
        nonce,
        deadline,
    )
}

/// Submit a gasless `convertPositions` for `proxy_addr`'s deposit wallet: fetch the live batch nonce,
/// build and sign the batch, POST it to the relayer, parse the result. Thin network wrapper over
/// [`build_convert_request`] (shares `submit_wallet_call` with the redeem/split-merge paths).
///
/// **DANGER — real funds, and NOT live-verified.** This POSTs a REAL EIP-712-signed batch to the
/// LIVE relayer and moves real money on-chain. Like [`submit_split_merge`] it has no proven
/// round-trip yet (see the module doc's DEFERRED section — an arbdub demo convert is OWED first).
/// There is NO internal opt-in guard and, deliberately, NO caller in this crate — not even the
/// `convert_arb` detector, which only SCREENS. The only gate is whoever calls it. Do NOT call this
/// from any always-on path, startup, resync, poller, or test that isn't an explicitly `#[ignore]`d
/// live smoke.
pub fn submit_convert(
    creds: &PolymarketCreds,
    proxy_addr: &str,
    neg_risk_market_id: &str,
    index_set: u128,
    amount: u128,
    era: Era,
) -> Result<RedeemResult, String> {
    let calldata = convert_positions_calldata(neg_risk_market_id, index_set, amount)?;
    submit_wallet_call(creds, proxy_addr, era.neg_risk_target(), &calldata)
}

#[cfg(test)]
mod tests {
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
        assert_eq!(
            &keccak256(CTF_MERGE_SIG)[0..4],
            &[0x9e, 0x72, 0x12, 0xad],
            "CTF mergePositions"
        );
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
            crate::redeem::USDC_E_ADDRESS.trim_start_matches("0x").to_lowercase()
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
        let req =
            build_convert_request(KEY, PROXY, CID, index_set, ONE_USDC, 0, 0, Era::V2).unwrap();
        assert!(
            req.body.contains(crate::redeem::NEG_RISK_CTF_COLLATERAL_ADAPTER),
            "targets the V2 NegRiskCtfCollateralAdapter"
        );
        assert!(!req.body.contains(crate::redeem::NEG_RISK_ADAPTER), "not the retired V1 adapter");
        assert!(!req.body.contains(crate::redeem::CTF_ADDRESS), "convert does not hit the CTF");
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
        let v1 =
            build_convert_request(KEY, PROXY, CID, index_set, ONE_USDC, 0, 0, Era::V1).unwrap();
        assert!(v1.body.contains(crate::redeem::NEG_RISK_ADAPTER), "V1 → retired adapter");

        // deterministic, and every load-bearing input is bound into the digest.
        let again =
            build_convert_request(KEY, PROXY, CID, index_set, ONE_USDC, 0, 0, Era::V2).unwrap();
        assert_eq!(req.signature, again.signature, "signature is deterministic");
        assert_eq!(req.body, again.body, "body is deterministic");
        let nonce1 =
            build_convert_request(KEY, PROXY, CID, index_set, ONE_USDC, 1, 0, Era::V2).unwrap();
        assert_ne!(req.signature, nonce1.signature, "nonce is bound in");
        let dl1 =
            build_convert_request(KEY, PROXY, CID, index_set, ONE_USDC, 0, 1, Era::V2).unwrap();
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
            build_convert_request(KEY, PROXY, "0xdead", index_set, ONE_USDC, 0, 0, Era::V2)
                .is_err()
        );
        // and a malformed deposit wallet is rejected too.
        assert!(
            build_convert_request(KEY, "0xbad", CID, index_set, ONE_USDC, 0, 0, Era::V2).is_err()
        );
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
        let bin_adapter = crate::redeem::CTF_COLLATERAL_ADAPTER;
        let nr_adapter = crate::redeem::NEG_RISK_CTF_COLLATERAL_ADAPTER;
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
            !b.body.contains(crate::redeem::NEG_RISK_CTF_COLLATERAL_ADAPTER),
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
            !n.body.contains(crate::redeem::CTF_COLLATERAL_ADAPTER),
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
            assert_eq!(target, crate::redeem::NEG_RISK_ADAPTER, "{kind:?} V1 target");
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
            assert_eq!(nr_target, crate::redeem::NEG_RISK_CTF_COLLATERAL_ADAPTER);
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
        let a2 = build_split_merge_request(
            KEY,
            PROXY,
            CID,
            2,
            0,
            0,
            SplitMergeKind::SplitBinary,
            Era::V1,
        )
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
}
