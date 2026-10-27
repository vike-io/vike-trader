//! `onboarding` — the one-time approvals a deposit wallet owes the exchange before it can trade,
//! sent through the gasless relayer client that already exists (`settlement::redeem_relayer`).
//!
//! ## What "Enable trading" is, on chain
//! Orders settle through an EXCHANGE contract that pulls the maker's assets: pUSD (the collateral
//! token, `crate::exec_plane::settlement::redeem::PUSD_COLLATERAL`) for a BUY, outcome tokens (the
//! CTF at `crate::exec_plane::settlement::redeem::CTF_ADDRESS`) for a SELL. A wallet that has not
//! approved the exchange has every order refused by the venue ("not enough balance / allowance"),
//! however full it is. There are TWO exchanges, one per market kind
//! (`crate::exec_plane::order::exchange_contract`: the standard CTF exchange, and the neg-risk one),
//! and one wallet needs both to trade every market:
//!
//! | call (from the deposit wallet) | on |
//! |---|---|
//! | `approve(exchange, 2^256-1)` | pUSD, for each of the two exchanges |
//! | `setApprovalForAll(exchange, true)` | the CTF, for each of the two exchanges |
//!
//! [`trading_calls`] is those four calls, in that order, and [`send_trading_approvals`] sends them as
//! ONE relayer `Batch` (one nonce, one signature) with `redeem_relayer::submit_wallet_calls_at`.
//!
//! ## What this does NOT do, and what is unverified
//! * It approves the two EXCHANGES only — what an order needs. The collateral adapters that redeem /
//!   split / merge / convert go through (`CTF_COLLATERAL_ADAPTER` and its neg-risk twin) need their
//!   own CTF approval; the settlement cluster is unmounted and this does not touch it.
//! * ⚠ **Nothing here has ever been sent.** The calldata is pinned by the same two-way selector
//!   check the redeem / split / merge encoders use and the batch envelope is the one
//!   `redeem_relayer` pinned from the reference SDK — but that envelope's nonce / deadline semantics
//!   and the relayer's reply shape are the two items `redeem_relayer.rs` lists as STILL OWED a live
//!   round trip, and this inherits both. The first live send is the proof.
//! * A wallet that is not a deposit wallet (a bare EOA, a proxy, a Safe) cannot use the relayer
//!   batch: its approvals are Polygon transactions it pays gas for, which this bridge does not send
//!   ([`crate::exec_plane::preflight`] says so in words).
//! * The relayer key itself, and deploying the deposit wallet, are not created here: there is no
//!   repo-verified API for either.
//!
//! Secrets: the batch is signed by the account's key and the relayer headers join at submit time; no
//! return value or log line carries either.

use vike_bridge_core::eip712::{enc_address, enc_uint, keccak256};

use crate::config::PolymarketCreds;
use crate::exec_plane::order::exchange_contract;
use crate::exec_plane::settlement::redeem::{CTF_ADDRESS, Era};
use crate::exec_plane::settlement::redeem_relayer::{RedeemResult, submit_wallet_calls_at};

/// ERC-20 `approve(address,uint256)`.
const APPROVE_SIG: &[u8] = b"approve(address,uint256)";
/// ERC-1155 `setApprovalForAll(address,bool)`.
const SET_APPROVAL_FOR_ALL_SIG: &[u8] = b"setApprovalForAll(address,bool)";

/// ABI calldata for `approve(spender, 2^256-1)` — the unlimited allowance the venue's own app grants.
pub(crate) fn approve_max_calldata(spender: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(4 + 2 * 32);
    out.extend_from_slice(&keccak256(APPROVE_SIG)[0..4]);
    out.extend_from_slice(&enc_address(spender));
    out.extend_from_slice(&[0xff; 32]);
    out
}

/// ABI calldata for `setApprovalForAll(operator, true)`.
pub(crate) fn set_approval_for_all_calldata(operator: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(4 + 2 * 32);
    out.extend_from_slice(&keccak256(SET_APPROVAL_FOR_ALL_SIG)[0..4]);
    out.extend_from_slice(&enc_address(operator));
    out.extend_from_slice(&enc_uint(1));
    out
}

/// One call of the approval batch: the contract it is sent to, and its calldata.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TradingCall {
    pub target: &'static str,
    pub calldata: Vec<u8>,
}

/// The four approvals a wallet needs to trade every market, in the order they are sent.
pub(crate) fn trading_calls() -> Vec<TradingCall> {
    let pusd = Era::V2.collateral();
    let mut calls = Vec::with_capacity(4);
    for neg_risk in [false, true] {
        calls.push(TradingCall {
            target: pusd,
            calldata: approve_max_calldata(exchange_contract(neg_risk)),
        });
    }
    for neg_risk in [false, true] {
        calls.push(TradingCall {
            target: CTF_ADDRESS,
            calldata: set_approval_for_all_calldata(exchange_contract(neg_risk)),
        });
    }
    calls
}

/// Send the four trading approvals from `deposit_wallet` as ONE relayer batch to `relayer_base`.
///
/// **DANGER — a real, signed, on-chain action.** It gives the two exchange contracts an unlimited
/// pUSD allowance and operator rights over the wallet's outcome tokens, which is exactly what the
/// venue's own "Enable trading" grants and nothing wider. It has no gate of its own: the only
/// production caller is [`crate::exec_plane::preflight::Preflight`], inside the live exec thread
/// (so behind `flags.poly_exec`, a live-tier account and its credentials), and only after a READ of
/// the wallet's allowance proved it short.
pub(crate) fn send_trading_approvals(
    relayer_base: &str,
    creds: &PolymarketCreds,
    deposit_wallet: &str,
) -> Result<RedeemResult, String> {
    let calls = trading_calls();
    let batch: Vec<(&str, &[u8])> =
        calls.iter().map(|c| (c.target, c.calldata.as_slice())).collect();
    submit_wallet_calls_at(relayer_base, creds, deposit_wallet, &batch)
}

#[path = "onboarding_tests.rs"]
#[cfg(test)]
mod onboarding_tests;
