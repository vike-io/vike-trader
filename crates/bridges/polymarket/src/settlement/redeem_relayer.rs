//! `redeem_relayer` — build + submit the gasless-relayer `redeemPositions` request for the account's
//! Poly1271 **deposit wallet** (CTF auto-redeem, docs/superpowers/specs/2026-07-11-ctf-redeem-design.md).
//!
//! Wire format + signing scheme PINNED against the reference SDK
//! `OrderBookTrade/rs-builder-relayer-client` (branch `main`) — the only public Rust client for
//! Polymarket's Builder Relayer V2:
//! - src/contracts.rs — `RELAYER_URL`, `DEPOSIT_WALLET_FACTORY`, `CTF`, `USDC_E`
//! - src/client.rs — `submit()` (POST `/submit`), `get_deposit_wallet_nonce()` (GET
//!   `/nonce?address=<owner>&type=WALLET`), `execute_deposit_wallet_batch()`
//! - src/types.rs — `TransactionRequest` / `DepositWalletParams` / `DepositWalletCall`
//!   (camelCase serde), `RelayerTransactionResponse`
//! - src/builder/deposit_wallet.rs — the EIP-712 `Batch` typed-data + `build_batch_request()`
//! - src/auth/relayer_key.rs — the `/submit` auth is the TWO PLAINTEXT headers only
//! - src/operations/redeem.rs — `redeem_regular` == our Task-1 calldata (identical selector/args)
//!
//! ## VERIFIED FROM SOURCE (rs-builder-relayer-client @ main)
//! - Endpoint: `POST https://relayer-v2.polymarket.com/submit` (body = JSON `TransactionRequest`).
//! - Auth: ONLY `RELAYER_API_KEY` + `RELAYER_API_KEY_ADDRESS` plaintext headers (NO L2 HMAC — the
//!   relayer `/submit` endpoint is not the CLOB, so unlike `exec::submit_order_relayer` there is no
//!   L2 signature). `src/auth/relayer_key.rs::build_headers`.
//! - Deposit-wallet `TransactionRequest`: `type="WALLET"`, `from=<owner EOA>`,
//!   `to=DEPOSIT_WALLET_FACTORY` (NOT the wallet), `signature=<0x r||s||v, 65 bytes>`,
//!   `nonce=<string>`, `depositWalletParams={depositWallet,<deadline string>,calls:[{target,value,data}]}`.
//! - The redeem `Call` is `{target: CTF, value: "0", data: redeemPositions calldata}` — Task-1's
//!   `redeem_positions_calldata` is byte-identical to the SDK's `operations::redeem_regular`.
//! - What is signed (deposit wallet / POLY_1271): an EIP-712 `Batch` digest, NOT the ERC-7739 order
//!   wrapper that `order::sign_order_1271` uses. The signature carried in the body is the RAW 65-byte
//!   `r||s||v` (`eip712::sign_digest_hex`), exactly what `build_batch_request` emits.
//!   - domain = `EIP712Domain(name="DepositWallet", version="1", chainId=137, verifyingContract=<deposit wallet>)`
//!   - message = `Batch(address wallet,uint256 nonce,uint256 deadline,Call[] calls)Call(address target,uint256 value,bytes data)`
//!   - `wallet` = the deposit wallet (== domain verifyingContract == `proxy_addr`)
//!   - `calls[i]` hash = keccak256(callTypeHash ‖ target ‖ value ‖ keccak256(data))
//!   - `calls` field = keccak256(‖ callHash_i)
//!
//! ## ASSUMED — MUST be verified against a live demo redeem via arbdub before `POLY_AUTO_REDEEM` is
//! ## ever enabled (this handles real funds):
//! - The `nonce`/`deadline` semantics (batch-nonce fetched from `/nonce?address=<owner>&type=WALLET`,
//!   deadline = now + 1h). The digest binds both, so a wrong nonce → on-chain signature-verify revert
//!   (the SDK's GS026 class of failure). `submit_redeem` fetches the live nonce; the pure
//!   `build_redeem_request` takes nonce+deadline as explicit args so it stays deterministic/testable.
//! - The exact `RelayerTransactionResponse` field names on a REAL success (`transactionID` /
//!   `transactionHash`); parsing here is tolerant (checks several) but has only been read from the
//!   reference struct, not observed live.
//! - vike's `PolymarketCreds` carries `relayer_key`/`relayer_address` (the two plaintext headers) but
//!   no relayer HMAC secret — consistent with the source's plaintext-only `/submit` auth, but the
//!   full round-trip (headers accepted, wallet deployed, batch mined) is unconfirmed without arbdub.
//!
//! Secrets: the deterministic `RedeemRequest` never contains the relayer key (it is a header joined
//! only at `submit_redeem` time); the private key never appears in any returned value or log.

use vike_bridge_core::eip712::{
    digest, domain_separator, enc_address, enc_uint, eth_address_from_private_key, hash_struct,
    keccak256, sign_digest_hex,
};

use crate::config::PolymarketCreds;
use crate::redeem::{redeem_neg_risk_calldata, redeem_positions_calldata, Era};

/// Polymarket Builder Relayer V2 base (VERIFIED: `contracts::RELAYER_URL`, trailing slash trimmed).
pub const RELAYER_BASE: &str = "https://relayer-v2.polymarket.com";
/// Submit endpoint path (VERIFIED: `client::submit` POSTs `{base}/submit`).
pub const SUBMIT_PATH: &str = "/submit";
/// Batch-nonce read path (VERIFIED: `client::get_deposit_wallet_nonce`).
pub const NONCE_PATH: &str = "/nonce";
/// The V2 deposit-wallet factory — the `to` of a `WALLET` relayer tx (VERIFIED: `contracts::DEPOSIT_WALLET_FACTORY`).
pub const DEPOSIT_WALLET_FACTORY: &str = "0x00000000000Fb5C9ADea0298D729A0CB3823Cc07";

const CHAIN_ID: u128 = 137;
const DW_DOMAIN_NAME: &str = "DepositWallet";
const DW_DOMAIN_VERSION: &str = "1";
const BATCH_TYPE: &str = "Batch(address wallet,uint256 nonce,uint256 deadline,Call[] calls)Call(address target,uint256 value,bytes data)";
const CALL_TYPE: &str = "Call(address target,uint256 value,bytes data)";

/// Which redeem call to make. `Binary` → CTF `redeemPositions(address,bytes32,bytes32,uint256[])`.
/// `NegRisk(uint_array)` → the era's neg-risk adapter; ⚠ the ENCODING is era-dependent, so the
/// `uint_array` is only used on [`Era::V1`] (see [`RedeemKind::call`] and `redeem.rs`'s module doc).
/// The relayer `Call.target` and calldata are the ONLY thing that differs between kinds; the
/// EIP-712 Batch signing + POST are identical.
pub enum RedeemKind {
    Binary,
    NegRisk(Vec<u128>),
}

impl RedeemKind {
    /// The `(target contract, calldata)` pair for this kind in `era` — the single routing authority
    /// shared by [`build_redeem_request`] and [`submit_redeem`], so the pure builder and the live
    /// submit can never disagree about what gets signed.
    ///
    /// ⚠ **The era changes the neg-risk ENCODER, not just the target.** [`Era::V2`]'s
    /// `NegRiskCtfCollateralAdapter` extends `CtfCollateralAdapter`, so its `redeemPositions` is the
    /// 4-arg CTF signature (`0x01b7037c`) — identical calldata to a binary redeem, differing ONLY by
    /// target address — and the `uint256[]` it takes is IGNORED (it reads the caller's balances
    /// itself). Only the RETIRED [`Era::V1`] `NEG_RISK_ADAPTER` exposes the 2-arg
    /// `redeemPositions(bytes32,uint256[])` (`0xdbeccb23`) that consumes the per-slot amounts.
    /// Verified against the deployed adapter's ABI + source, 2026-08-02 — before that this path
    /// sent `0xdbeccb23` to the V2 adapter, which does not implement it, so every neg-risk redeem
    /// would have reverted. `redeem.rs`'s module doc is the authority.
    pub fn call(&self, condition_id: &str, era: Era) -> Result<(&'static str, Vec<u8>), String> {
        Ok(match (self, era) {
            (RedeemKind::Binary, _) => {
                (era.binary_target(), redeem_positions_calldata(condition_id, era)?)
            }
            (RedeemKind::NegRisk(_), Era::V2) => {
                (era.neg_risk_target(), redeem_positions_calldata(condition_id, era)?)
            }
            (RedeemKind::NegRisk(arr), Era::V1) => {
                (era.neg_risk_target(), redeem_neg_risk_calldata(condition_id, arr)?)
            }
        })
    }
}

/// A fully-built, EIP-712-signed relayer request. Deterministic given its inputs — it carries NO
/// secret (the relayer key is a header added only at submit time), so it is safe to log/inspect.
#[derive(Clone, Debug)]
pub struct RedeemRequest {
    /// the `TransactionRequest` JSON body (serialized, ready to POST).
    pub body: String,
    /// the raw EIP-712 `Batch` signature (`0x` + 65-byte `r||s||v`) — also embedded in `body`.
    pub signature: String,
    /// non-secret headers (`Content-Type`); the relayer key/address are joined in [`submit_redeem`].
    pub headers: Vec<(String, String)>,
}

/// The parsed relayer submit result.
#[derive(Clone, Debug)]
pub struct RedeemResult {
    /// on-chain tx hash when the relayer returns one (`transactionHash`/`hash`), else `None`.
    pub tx_hash: Option<String>,
    /// the raw response body (always retained for audit / verify-before-live).
    pub raw: String,
}

/// Validate a `0x…` Ethereum address is EXACTLY 20 bytes (mirrors redeem.rs's bytes32 hardening):
/// a malformed address would be silently zero-defaulted by `enc_address`, corrupting the EIP-712
/// verifyingContract / `Batch.wallet` / body `depositWallet` — a real-money misdirection risk.
fn validate_address(addr: &str) -> Result<(), String> {
    let clean = addr.strip_prefix("0x").unwrap_or(addr);
    let bytes = hex::decode(clean).map_err(|e| format!("not valid hex: {e}"))?;
    if bytes.len() != 20 {
        return Err(format!("must be a 20-byte address, got {} bytes", bytes.len()));
    }
    Ok(())
}

/// EIP-712 hash of one relayer `Call` struct: keccak256(callTypeHash ‖ target ‖ value ‖ keccak256(data)).
pub(crate) fn hash_call(target: &str, value: u128, data: &[u8]) -> [u8; 32] {
    hash_struct(CALL_TYPE, &[enc_address(target), enc_uint(value), keccak256(data)])
}

/// The `Batch` struct hash for a single-`Call` redeem batch.
fn batch_struct_hash(wallet: &str, nonce: u64, deadline: u64, calls: &[[u8; 32]]) -> [u8; 32] {
    // dynamic `Call[]` field = keccak256(‖ callHash_i)
    let mut acc = Vec::with_capacity(calls.len() * 32);
    for c in calls {
        acc.extend_from_slice(c);
    }
    let calls_hash = keccak256(&acc);
    hash_struct(
        BATCH_TYPE,
        &[
            enc_address(wallet),
            enc_uint(u128::from(nonce)),
            enc_uint(u128::from(deadline)),
            calls_hash,
        ],
    )
}

/// Build + EIP-712-sign the gasless-relayer `redeemPositions` request — PURE and DETERMINISTIC given
/// `(private_key, proxy_addr, condition_id, nonce, deadline, kind, era)`. `kind` selects binary vs
/// neg-risk and `era` selects the collateral era; TOGETHER they select the relayer target contract +
/// calldata ([`Era::binary_target`]/[`Era::neg_risk_target`], and the binary calldata's collateral
/// word). The EIP-712 Batch signing + request envelope are identical either way.
///
/// `era` routes the retired-vs-current contract split: pass [`Era::V2`] (the default) for all current
/// activity — a V1 relayer call to the retired NegRiskAdapter fails on-chain. See [`Era`].
///
/// NOTE (deviates from the brief's 3-arg sketch): `nonce` and `deadline` are LOAD-BEARING inputs to
/// the EIP-712 `Batch` digest — a defaulted/placeholder nonce would produce a signature that reverts
/// on-chain (real money), so they are explicit args rather than hidden constants. [`submit_redeem`]
/// fetches the live nonce and derives the deadline before calling this; tests pass fixed values for a
/// stable signature.
///
/// `proxy_addr` is the account's **deposit wallet** (== the EIP-712 `verifyingContract` and the
/// `Batch.wallet`). The `owner` (`from`) is derived from `private_key`.
pub fn build_redeem_request(
    private_key: &str,
    proxy_addr: &str,
    condition_id: &str,
    nonce: u64,
    deadline: u64,
    kind: &RedeemKind,
    era: Era,
) -> Result<RedeemRequest, String> {
    let (target, calldata) = kind.call(condition_id, era)?;
    build_wallet_call_request(private_key, proxy_addr, target, &calldata, nonce, deadline)
}

/// The venue-neutral deposit-wallet single-`Call` builder underneath [`build_redeem_request`] —
/// EIP-712 `Batch`-signs ONE `Call{target, value: 0, data: calldata}` and emits the `WALLET`
/// `TransactionRequest` body. Extracted so `split_merge.rs` reuses the exact same signing +
/// envelope path rather than re-deriving it; the redeem behavior is unchanged and pinned
/// byte-for-byte by this module's `GOLDEN_SIGNATURE` test.
pub(crate) fn build_wallet_call_request(
    private_key: &str,
    proxy_addr: &str,
    target: &str,
    calldata: &[u8],
    nonce: u64,
    deadline: u64,
) -> Result<RedeemRequest, String> {
    // Real-money hardening (mirrors redeem.rs's conditionId check): `enc_address` silently
    // zero-defaults a malformed address, which would corrupt the EIP-712 verifyingContract /
    // Batch.wallet / body depositWallet all to the ZERO address. Hard-error instead.
    validate_address(proxy_addr).map_err(|e| format!("proxy_addr {e}"))?;
    let owner = eth_address_from_private_key(private_key)?;
    let data_hex = format!("0x{}", hex::encode(calldata));

    // EIP-712 Batch over a single Call{ target, value: 0, data: calldata }.
    let call_hash = hash_call(target, 0, calldata);
    let struct_hash = batch_struct_hash(proxy_addr, nonce, deadline, &[call_hash]);
    let domain = domain_separator(DW_DOMAIN_NAME, DW_DOMAIN_VERSION, CHAIN_ID, proxy_addr);
    let signature = sign_digest_hex(&digest(&domain, &struct_hash), private_key)?;

    // TransactionRequest (camelCase) — mirrors rs-builder-relayer-client `build_batch_request`.
    let body = serde_json::json!({
        "type": "WALLET",
        "from": owner,
        "to": DEPOSIT_WALLET_FACTORY,
        "signature": signature,
        "nonce": nonce.to_string(),
        "depositWalletParams": {
            "depositWallet": proxy_addr,
            "deadline": deadline.to_string(),
            "calls": [{
                "target": target,
                "value": "0",
                "data": data_hex,
            }],
        },
    });
    let body = serde_json::to_string(&body).map_err(|e| e.to_string())?;

    Ok(RedeemRequest {
        body,
        signature,
        headers: vec![("Content-Type".to_string(), "application/json".to_string())],
    })
}

/// The batch-nonce query for a deposit wallet (VERIFIED: `/nonce?address=<owner>&type=WALLET`).
fn nonce_query(owner: &str) -> String {
    format!("address={owner}&type=WALLET")
}

/// Parse the relayer nonce response (VERIFIED tolerant parse mirroring `get_deposit_wallet_nonce`:
/// bare number, `nonce` number, or `nonce` string).
fn parse_nonce(v: &serde_json::Value) -> Result<u64, String> {
    if let Some(n) = v.as_u64() {
        return Ok(n);
    }
    if let Some(n) = v.get("nonce").and_then(serde_json::Value::as_u64) {
        return Ok(n);
    }
    if let Some(s) = v.get("nonce").and_then(serde_json::Value::as_str) {
        return s.parse::<u64>().map_err(|e| format!("nonce not a number: {e}"));
    }
    if let Some(s) = v.as_str() {
        return s.parse::<u64>().map_err(|e| format!("nonce not a number: {e}"));
    }
    Err(format!("no nonce in relayer response: {v}"))
}

/// Parse the relayer submit response into a [`RedeemResult`] (tolerant: `transactionHash` / `hash`).
fn parse_submit(text: &str) -> RedeemResult {
    let tx_hash = serde_json::from_str::<serde_json::Value>(text).ok().and_then(|v| {
        v.get("transactionHash")
            .or_else(|| v.get("transaction_hash"))
            .or_else(|| v.get("hash"))
            .and_then(serde_json::Value::as_str)
            .map(str::to_string)
    });
    RedeemResult { tx_hash, raw: text.to_string() }
}

/// Submit a gasless `redeemPositions` for `proxy_addr`'s deposit wallet: fetch the live batch nonce,
/// build + sign the request, POST it to the relayer with the plaintext relayer headers, parse the
/// result. Thin network wrapper over [`build_redeem_request`].
///
/// **DANGER — real funds.** This POSTs a REAL EIP-712-signed redeem to the LIVE relayer and moves
/// real money on-chain. It has NO internal opt-in guard: the ONLY gate is the caller (the Task-5
/// `AutoRedeemPoller`, which is off unless `POLY_AUTO_REDEEM=1`). Do NOT call this from any
/// always-on path, startup, resync, or test that isn't an explicitly `#[ignore]`d live smoke.
///
/// `era` selects the collateral era's target contract + collateral word (see [`Era`]); pass
/// [`Era::V2`] for current activity — a V1 relayer call targets the retired NegRiskAdapter.
pub fn submit_redeem(
    creds: &PolymarketCreds,
    proxy_addr: &str,
    condition_id: &str,
    kind: &RedeemKind,
    era: Era,
) -> Result<RedeemResult, String> {
    let (target, calldata) = kind.call(condition_id, era)?;
    submit_wallet_call(creds, proxy_addr, target, &calldata)
}

/// The venue-neutral deposit-wallet submit underneath [`submit_redeem`]: fetch the live batch
/// nonce, build + sign a single-`Call` batch, POST it with the plaintext relayer headers, parse
/// the result. Extracted so `split_merge.rs` reuses the identical network path.
///
/// **DANGER — real funds.** Same warning as [`submit_redeem`]: this POSTs a REAL signed batch to
/// the LIVE relayer. No internal opt-in guard; gating is the caller's job.
pub(crate) fn submit_wallet_call(
    creds: &PolymarketCreds,
    proxy_addr: &str,
    target: &str,
    calldata: &[u8],
) -> Result<RedeemResult, String> {
    if creds.relayer_key.is_empty() || creds.relayer_address.is_empty() {
        return Err("relayer credentials unset (RELAYER_API_KEY / RELAYER_API_KEY_ADDRESS)".into());
    }
    let owner = eth_address_from_private_key(&creds.private_key)?;
    let ag = crate::egress::agent();

    // 1) live batch nonce for this owner's deposit wallet.
    let nonce_url = format!("{RELAYER_BASE}{NONCE_PATH}?{}", nonce_query(&owner));
    let mut resp = ag
        .get(&nonce_url)
        .header("RELAYER_API_KEY", creds.relayer_key.as_str())
        .header("RELAYER_API_KEY_ADDRESS", creds.relayer_address.as_str())
        .call()
        .map_err(|e| format!("network (nonce): {e}"))?;
    let nonce_text =
        resp.body_mut().read_to_string().map_err(|e| format!("network (nonce): {e}"))?;
    let nonce_json: serde_json::Value = serde_json::from_str(&nonce_text)
        .map_err(|e| format!("bad nonce json: {e}: {nonce_text}"))?;
    let nonce = parse_nonce(&nonce_json)?;

    // 2) deadline = now + 1h (relayer bounds the batch validity window).
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .as_secs();
    let deadline = now + 3600;

    // 3) build + sign.
    let req = build_wallet_call_request(
        &creds.private_key,
        proxy_addr,
        target,
        calldata,
        nonce,
        deadline,
    )?;

    // 4) POST /submit with Content-Type + the two plaintext relayer headers.
    let submit_url = format!("{RELAYER_BASE}{SUBMIT_PATH}");
    let mut post = ag
        .post(&submit_url)
        .header("RELAYER_API_KEY", creds.relayer_key.as_str())
        .header("RELAYER_API_KEY_ADDRESS", creds.relayer_address.as_str());
    for (k, v) in &req.headers {
        post = post.header(k.as_str(), v.as_str());
    }
    let mut resp = post.send(req.body.as_bytes()).map_err(|e| format!("network (submit): {e}"))?;
    let status = resp.status().as_u16();
    let text = resp.body_mut().read_to_string().map_err(|e| format!("network (submit): {e}"))?;
    if !(200..300).contains(&status) {
        return Err(format!("relayer {status}: {text}"));
    }
    Ok(parse_submit(&text))
}

#[cfg(test)]
mod tests {
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
        let ctf = crate::redeem::CTF_ADDRESS;
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
        let req =
            build_redeem_request(KEY, PROXY, CID, 0, 0, &RedeemKind::Binary, Era::V2).unwrap();
        assert!(
            req.body.contains(crate::redeem::CTF_COLLATERAL_ADAPTER),
            "V2 binary targets the CtfCollateralAdapter"
        );
        let expected =
            format!("0x{}", hex::encode(redeem_positions_calldata(CID, Era::V2).unwrap()));
        assert!(req.body.contains(&expected), "carries the exact pUSD-collateral calldata");
        assert!(req.body.contains("01b7037c"), "selector is era-independent");
        // must not target the raw CTF nor carry USDC.e collateral (the retired/pre-fix encoding):
        assert!(!req.body.contains(crate::redeem::USDC_E_ADDRESS.to_lowercase().as_str()));
        // deterministic
        let again =
            build_redeem_request(KEY, PROXY, CID, 0, 0, &RedeemKind::Binary, Era::V2).unwrap();
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
        let v2 =
            build_redeem_request(KEY, PROXY, CID, 0, 0, &RedeemKind::NegRisk(vec![1, 2]), Era::V2)
                .unwrap();
        assert!(v2.body.contains(crate::redeem::NEG_RISK_CTF_COLLATERAL_ADAPTER));
        assert!(!v2.body.contains(crate::redeem::NEG_RISK_ADAPTER), "not the retired V1 adapter");
        assert!(!v2.body.contains(crate::redeem::CTF_ADDRESS));
        assert!(!v2.signature.is_empty());
        // V1 (legacy) → the retired adapter, retained for known-legacy positions.
        let v1 =
            build_redeem_request(KEY, PROXY, CID, 0, 0, &RedeemKind::NegRisk(vec![1, 2]), Era::V1)
                .unwrap();
        assert!(v1.body.contains(crate::redeem::NEG_RISK_ADAPTER));
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
        assert_eq!(nr_target, crate::redeem::NEG_RISK_CTF_COLLATERAL_ADAPTER);
        assert_eq!(bin_target, crate::redeem::CTF_COLLATERAL_ADAPTER);
        assert_ne!(nr_target, bin_target, "ONLY the target differs on V2");
        // The V2 adapter reads its own balances, so the uint256[] is inert — a different array must
        // not move a single byte (this is what makes carrying amounts harmless, not meaningful).
        let (_, other) = RedeemKind::NegRisk(vec![53_000_000, 0]).call(CID, Era::V2).unwrap();
        assert_eq!(other, nr_data, "V2 ignores the uint256[]; the encoder must not vary with it");
        // V1 (retired) keeps the 2-arg overload, where the amounts ARE load-bearing.
        let (v1_target, v1_data) = RedeemKind::NegRisk(vec![1, 2]).call(CID, Era::V1).unwrap();
        assert_eq!(hex::encode(&v1_data[0..4]), "dbeccb23", "V1 neg-risk selector");
        assert_eq!(v1_target, crate::redeem::NEG_RISK_ADAPTER);
        let (_, v1_other) = RedeemKind::NegRisk(vec![53_000_000, 0]).call(CID, Era::V1).unwrap();
        assert_ne!(v1_other, v1_data, "V1 amounts reach the chain, so they change the bytes");
    }

    #[test]
    fn binary_request_targets_the_era_contract() {
        // V2 (default) → the CtfCollateralAdapter; V1 (legacy) → the raw CTF.
        let v2 = build_redeem_request(KEY, PROXY, CID, 0, 0, &RedeemKind::Binary, Era::V2).unwrap();
        assert!(v2.body.contains(crate::redeem::CTF_COLLATERAL_ADAPTER));
        let v1 = build_redeem_request(KEY, PROXY, CID, 0, 0, &RedeemKind::Binary, Era::V1).unwrap();
        assert!(v1.body.contains(crate::redeem::CTF_ADDRESS));
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
        let r =
            parse_submit(r#"{"transactionID":"abc","state":"NEW","transactionHash":"0xdeadbeef"}"#);
        assert_eq!(r.tx_hash.as_deref(), Some("0xdeadbeef"));
        let r2 = parse_submit(r#"{"transactionID":"abc","state":"NEW"}"#);
        assert_eq!(r2.tx_hash, None);
    }
}
