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
//! Gnosis ConditionalTokens (the contract at [`crate::exec_plane::settlement::redeem::CTF_ADDRESS`],
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
//! ⚠ **…but ONLY on [`crate::exec_plane::settlement::redeem::Era::V1`].** Everything above describes the RETIRED
//! `NegRiskAdapter`, which is what the 2026-07-22 bytecode probe below was run against. The CURRENT
//! [`crate::exec_plane::settlement::redeem::Era::V2`] target is a DIFFERENT contract — `NegRiskCtfCollateralAdapter`
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
//! is a lost transaction: the deployed [`crate::exec_plane::settlement::redeem::NEG_RISK_ADAPTER`] bytecode was fetched from a
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
use crate::exec_plane::settlement::redeem::{Era, bytes32_from_hex};
use crate::exec_plane::settlement::redeem_relayer::{
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

/// Which split/merge call to make — the mint/burn analog of [`crate::exec_plane::settlement::redeem_relayer::RedeemKind`].
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
    /// fixed on the redeem path (see [`crate::exec_plane::settlement::redeem_relayer::RedeemKind::call`] and `redeem.rs`'s
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
/// [`crate::exec_plane::settlement::redeem_relayer::build_redeem_request`], reusing its `Batch` signing and `WALLET`
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
/// The convert twin of [`build_split_merge_request`], reusing [`crate::exec_plane::settlement::redeem_relayer`]'s
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

#[path = "split_merge_tests.rs"]
#[cfg(test)]
mod split_merge_tests;
