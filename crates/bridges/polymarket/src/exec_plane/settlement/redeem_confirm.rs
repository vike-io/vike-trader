//! `redeem_confirm` — the ON-CHAIN proof that a submitted CTF redemption actually happened.
//!
//! ## Why this module exists
//!
//! [`crate::exec_plane::settlement::redeem_relayer::submit_redeem`] returns `Ok` when the gasless relayer answers **HTTP
//! 2xx**. That is an acknowledgement that the relayer ACCEPTED a signed batch — nothing more. The
//! relayer's own success body says so out loud: the shape pinned from the reference SDK is
//! `{"transactionID":"…","state":"NEW"}`, i.e. *queued*, frequently with no `transactionHash` at
//! all yet. Between that 2xx and the money arriving, a batch can be dropped from the mempool, or
//! mined and REVERTED (a stale nonce, an expired deadline, a wrong neg-risk amount — the three
//! encodings `redeem_relayer`/`redeem` still mark ASSUMED pending the arbdub live-verify).
//!
//! [`crate::exec_plane::settlement::redeem_ledger::RedeemLedger`] is a PERSISTED at-most-once guard, so treating a 2xx as
//! "done" writes a permanent tombstone on evidence that does not prove the money moved: a dropped
//! or reverted redemption would forfeit those winnings forever, with nothing left to retry them.
//! This module supplies the missing evidence.
//!
//! ## What counts as confirmation (and why)
//!
//! Three facts, ALL required, read straight off the transaction receipt via the existing read-only
//! [`crate::exec_plane::settlement::chain::PolygonRpc`] (`eth_getTransactionReceipt` + `eth_blockNumber` — both already
//! live-verified against `polygon.drpc.org`, no new endpoint, no new dependency):
//!
//! 1. **A receipt exists.** No receipt ⇒ [`RedeemConfirmation::Unmined`] — still queued, or
//!    dropped. Never a verdict on its own; the caller's timeout decides which.
//! 2. **`status == 1`.** A `0x0` status is a definitive on-chain failure ⇒
//!    [`RedeemConfirmation::Reverted`], which must RETRY.
//! 3. **A `PayoutRedemption` log for OUR `conditionId` is in that receipt's logs.** This is the
//!    load-bearing check and the reason a bare `status == 1` is not enough: the relayer mines a
//!    BATCH, and "the batch succeeded" is not "my redeem call settled my condition". A reverted
//!    call emits no logs, so the log's presence is strictly stronger evidence than the status word
//!    — which is why an ABSENT `status` field is not itself treated as failure here.
//!    Matching is on `conditionId`, NEVER on `redeemer`: the on-chain `msg.sender` is a shared
//!    relayer proxy, not the funder (the TRAP documented in [`crate::exec_plane::settlement::chain`]), and under
//!    [`crate::exec_plane::settlement::redeem::Era::V2`] the call is routed through a collateral adapter, so the redeemer
//!    is an adapter address either way. Both event shapes are accepted via
//!    [`crate::exec_plane::settlement::chain::decode_redemption`], so the CTF and NegRiskAdapter paths need no special
//!    casing here.
//!
//! Plus a fourth, which gates only WHEN the verdict becomes permanent:
//!
//! 4. **[`MIN_CONFIRMATIONS`] blocks of depth.** A receipt can be un-mined by a reorg, and the
//!    tombstone this feeds is permanent, so a mined-but-shallow receipt reports
//!    [`RedeemConfirmation::Maturing`] rather than settling. `Maturing` is deliberately a SEPARATE
//!    variant from `Unmined`: the caller's dropped-transaction timeout must never fire on a
//!    transaction it can see mined.
//!
//! ## What is deliberately NOT used as confirmation
//!
//! **The data-api `redeemable` flag going false.** It is not evidence: it is a *lagging* signal
//! (a position stays `redeemable` between submit and settlement — the very lag the ledger exists
//! to bridge), and a partial page or a de-listed market flips it for reasons that have nothing to
//! do with our redemption. It narrows the discovery set — the job it has in
//! [`crate::exec_plane::settlement::positions::resolved_candidates`], where it is explicitly NOT trusted to identify a
//! winner either (see that module's doc) — and it is a poor *proof*.
//!
//! ## Cost
//!
//! One `eth_getTransactionReceipt` per in-flight redemption per tick, plus one `eth_blockNumber`
//! only when a receipt came back. The in-flight set is normally empty and never more than a
//! handful, and the default cadence is minutes — so this is a read-only trickle against a keyless
//! public RPC, not a new load source.
//!
//! ⚠ **Operational consequence:** starting the auto-redeem poller also requires reachable Polygon
//! RPC (the keyless [`crate::exec_plane::settlement::chain::DEFAULT_RPC_URL`] by default). It does NOT
//! require the chain watcher — this confirmer owns its own `PolygonRpc` and is independent of the
//! watcher's `enabled`, because confirmation is not optional once real money is moving.

use serde_json::Value;

use crate::exec_plane::settlement::chain::{ChainRpcSettings, PolygonRpc, decode_redemption};

/// Block depth a redemption receipt must reach before its ledger tombstone becomes permanent.
///
/// Polygon PoS reaches deterministic finality through Heimdall milestones covering ~12–16 blocks,
/// so 32 blocks (~64 s at 2 s blocks) is comfortably past milestone finality. The cost of waiting
/// is one extra poll tick; the cost of settling a reorged-away receipt is a permanent forfeit —
/// which is the whole asymmetry this module is built around.
pub const MIN_CONFIRMATIONS: u64 = 32;

/// The chain's verdict on one submitted redemption.
#[derive(Debug, Clone, PartialEq)]
pub enum RedeemConfirmation {
    /// PROVEN: receipt succeeded, a `PayoutRedemption` for this conditionId is in it, and it is
    /// buried at least `min_confirmations` deep. The ONLY verdict that may settle the ledger.
    Settled { block: u64, payout_usdc: f64 },
    /// DEFINITIVE FAILURE: mined, but this conditionId was not redeemed (status `0x0`, or no
    /// matching `PayoutRedemption`). Retry.
    Reverted { reason: String },
    /// Mined and proven, but not yet `min_confirmations` deep. Wait — never a dropped transaction.
    Maturing { block: u64, confirmations: u64 },
    /// No receipt at all: still queued at the relayer / in the mempool, or dropped. Indistinguishable
    /// from here — the caller's age-based timeout is what separates the two.
    Unmined,
}

impl RedeemConfirmation {
    /// A one-line reason string for logs (`Settled` has no failure reason and yields `""`).
    pub fn reason(&self) -> String {
        match self {
            RedeemConfirmation::Settled { .. } => String::new(),
            RedeemConfirmation::Reverted { reason } => reason.clone(),
            RedeemConfirmation::Maturing { block, confirmations } => {
                format!("mined in block {block}, only {confirmations} confirmations")
            }
            RedeemConfirmation::Unmined => "no receipt yet (queued or dropped)".to_string(),
        }
    }
}

/// Strip a `0x`/`0X` prefix and lowercase — the local twin of `chain.rs`'s private `clean`, so both
/// sides of a conditionId comparison are normalised regardless of which wire spelled it.
fn norm_hex(s: &str) -> String {
    s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")).unwrap_or(s).to_ascii_lowercase()
}

/// A receipt field that may be a hex string (`"0x1"`, the normal JSON-RPC spelling) or a bare JSON
/// number (some proxies re-encode it).
fn hex_u64(v: Option<&Value>) -> Option<u64> {
    let v = v?;
    if let Some(n) = v.as_u64() {
        return Some(n);
    }
    u64::from_str_radix(&norm_hex(v.as_str()?), 16).ok()
}

/// **The pure core**: a transaction receipt (or its absence) + the chain head ⇒ a verdict. No I/O,
/// so the whole state machine is testable offline against real captured receipts.
///
/// See the module doc for why all three of receipt / `status` / matching `PayoutRedemption` are
/// required, and why an absent `status` field alone is not treated as a failure.
pub fn classify_receipt(
    receipt: Option<&Value>,
    head_block: u64,
    condition_id: &str,
    min_confirmations: u64,
) -> RedeemConfirmation {
    let Some(r) = receipt else {
        return RedeemConfirmation::Unmined;
    };

    // (2) An explicit non-1 status is a definitive on-chain failure. An ABSENT status is NOT — the
    // log check below is stronger evidence, and a receipt shape without `status` must not be read
    // as a revert.
    if let Some(status) = hex_u64(r.get("status"))
        && status != 1
    {
        return RedeemConfirmation::Reverted { reason: format!("receipt status 0x{status:x}") };
    }

    // (3) A PayoutRedemption for OUR conditionId — matched on the condition, never the redeemer.
    let empty: Vec<Value> = Vec::new();
    let logs = r.get("logs").and_then(|l| l.as_array()).unwrap_or(&empty);
    let want = norm_hex(condition_id);
    let Some(redemption) =
        logs.iter().filter_map(decode_redemption).find(|d| norm_hex(&d.condition_id) == want)
    else {
        return RedeemConfirmation::Reverted {
            reason: "mined, but no PayoutRedemption for this conditionId in the receipt"
                .to_string(),
        };
    };

    // (4) Depth. Prefer the receipt's own blockNumber; fall back to the log's.
    let block = hex_u64(r.get("blockNumber")).filter(|b| *b > 0).unwrap_or(redemption.block);
    if block == 0 {
        return RedeemConfirmation::Unmined; // a receipt with no usable block is not proof of depth
    }
    let confirmations = if head_block >= block { head_block - block + 1 } else { 0 };
    if confirmations < min_confirmations {
        return RedeemConfirmation::Maturing { block, confirmations };
    }
    RedeemConfirmation::Settled { block, payout_usdc: redemption.payout_usdc }
}

/// The live confirmer: [`classify_receipt`] wired to a read-only [`PolygonRpc`].
///
/// Never signs and never sends a transaction — it is `eth_getTransactionReceipt` +
/// `eth_blockNumber` and nothing else.
pub struct ChainRedeemConfirmer {
    rpc: PolygonRpc,
    min_confirmations: u64,
}

impl ChainRedeemConfirmer {
    pub fn new(rpc: PolygonRpc) -> Self {
        ChainRedeemConfirmer { rpc, min_confirmations: MIN_CONFIRMATIONS }
    }

    /// From the caller's [`ChainRpcSettings`] ([`PolygonRpc::new`] — the same settings
    /// [`crate::exec_plane::settlement::chain`]'s readers take; this adds no new setting).
    pub fn from_settings(settings: &ChainRpcSettings) -> Self {
        Self::new(PolygonRpc::new(settings))
    }

    /// Override the confirmation depth (tests, and an operator who wants a deeper bury).
    pub fn with_min_confirmations(mut self, n: u64) -> Self {
        self.min_confirmations = n;
        self
    }

    pub fn min_confirmations(&self) -> u64 {
        self.min_confirmations
    }

    pub fn rpc(&self) -> &PolygonRpc {
        &self.rpc
    }

    /// One confirmation read. `Err` is a TRANSPORT verdict, never a settlement one — the caller
    /// must leave the ledger untouched on an `Err` rather than inferring anything from an RPC blip.
    pub fn confirm(&self, condition_id: &str, tx_hash: &str) -> Result<RedeemConfirmation, String> {
        let Some(receipt) = self.rpc.transaction_receipt(tx_hash)? else {
            // No receipt ⇒ no need to spend a second call on the head block.
            return Ok(RedeemConfirmation::Unmined);
        };
        let head = self.rpc.block_number()?;
        Ok(classify_receipt(Some(&receipt), head, condition_id, self.min_confirmations))
    }
}

#[path = "redeem_confirm_tests.rs"]
#[cfg(test)]
mod redeem_confirm_tests;
