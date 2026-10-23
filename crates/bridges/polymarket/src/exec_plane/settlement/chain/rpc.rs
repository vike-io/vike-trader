//! `chain`'s settlement join and the minimal Polygon JSON-RPC reader (`PolygonRpc`).

use std::time::Duration;

use serde_json::Value;

use super::abi::clean;
use super::decode::decode_uint_result;
use super::{
    ChainResolution, ChainRpcSettings, ChainSettlement, DEFAULT_MAX_SPAN, PAYOUT_EPSILON,
    Redemption, SEL_OUTCOME_SLOT_COUNT, SEL_PAYOUT_DENOMINATOR, SEL_PAYOUT_NUMERATORS,
    TokenTransfer, USDC_DECIMALS,
};
use crate::exec_plane::settlement::redeem::CTF_ADDRESS;

// ---------------------------------------------------------------------------------------------
// Joining a redemption to the funder's own tokens
// ---------------------------------------------------------------------------------------------

/// PURE join: one transaction's funder token-outflows + its redemption + (optionally) the chain's
/// payout numerators → the per-token [`ChainSettlement`] rows.
///
/// Pricing, in strict precedence — and **never a guess**:
/// 1. `resolution` known ⇒ each leg is priced at `payout_for_index(i)`, where `i` is the token's
///    position in the transfer's id vector, **and the mapping is then PROVEN, not assumed**: the
///    identity `Σ(qtyᵢ · priceᵢ) == payout` must hold to [`PAYOUT_EPSILON`]. That check is what
///    makes this safe — the CTF builds its ERC-1155 batch in `indexSets` order, so vector position
///    equals outcome slot for a `[1, 2]` redeem but NOT for a single-leg `[2]` redeem (observed
///    live). A mismatch falls through to rule 2 instead of booking a wrong price.
/// 2. If exactly ONE leg has a non-zero amount, that leg absorbs the whole payout (`payout / qty`)
///    and every other leg prices at 0.0 — arithmetically forced, so it is correct whatever the slot
///    ordering turns out to be.
/// 3. Otherwise the transaction is SKIPPED with a warning. Two moved legs and one total is
///    under-determined, and a fabricated realised PnL is worse than a missing one — the same
///    refuse-to-guess rule [`crate::exec_plane::settlement::resolve::ambiguous_conditions`] applies.
pub fn join_settlement(
    redemption: &Redemption,
    transfers: &[TokenTransfer],
    resolution: Option<&ChainResolution>,
) -> Vec<ChainSettlement> {
    let mut legs: Vec<(String, u128)> = Vec::new();
    for t in transfers {
        for (id, v) in t.ids.iter().zip(t.values.iter()) {
            legs.push((id.clone(), *v));
        }
    }
    if legs.is_empty() {
        return Vec::new();
    }
    let non_zero: Vec<usize> =
        legs.iter().enumerate().filter(|(_, (_, v))| *v > 0).map(|(i, _)| i).collect();
    // Rule 1, with its proof obligation.
    let by_resolution = resolution.filter(|r| r.is_resolved()).and_then(|r| {
        let prices: Vec<f64> =
            (0..legs.len()).map(|i| r.payout_for_index(i as u32).unwrap_or(0.0)).collect();
        let implied: f64 =
            legs.iter().zip(&prices).map(|((_, v), px)| (*v as f64 / USDC_DECIMALS) * px).sum();
        if (implied - redemption.payout_usdc).abs() <= PAYOUT_EPSILON {
            Some(prices)
        } else {
            tracing::warn!(
                target: "vike_polymarket::chain",
                condition_id = %redemption.condition_id,
                tx = %redemption.tx_hash,
                implied, payout = redemption.payout_usdc,
                "chain watcher: slot mapping failed its payout identity — falling back"
            );
            None
        }
    });
    let prices: Vec<f64> = match by_resolution {
        Some(p) => p,
        None if non_zero.len() == 1 => {
            let qty = legs[non_zero[0]].1 as f64 / USDC_DECIMALS;
            let px = if qty > 0.0 { redemption.payout_usdc / qty } else { 0.0 };
            (0..legs.len()).map(|i| if i == non_zero[0] { px } else { 0.0 }).collect()
        }
        None => {
            tracing::warn!(
                target: "vike_polymarket::chain",
                condition_id = %redemption.condition_id,
                tx = %redemption.tx_hash,
                legs = legs.len(),
                "chain watcher: payout split across legs is under-determined without a chain \
                 resolution — settlement not synthesised (refusing to guess)"
            );
            return Vec::new();
        }
    };
    legs.into_iter()
        .zip(prices)
        .filter(|((_, v), _)| *v > 0)
        .map(|((token_id, v), price)| ChainSettlement {
            condition_id: redemption.condition_id.clone(),
            token_id,
            qty: v as f64 / USDC_DECIMALS,
            price,
            payout_usdc: redemption.payout_usdc,
            venue: redemption.venue,
            tx_hash: redemption.tx_hash.clone(),
            block: redemption.block,
            ts_ms: redemption.ts_ms,
        })
        .collect()
}

// ---------------------------------------------------------------------------------------------
// The JSON-RPC client
// ---------------------------------------------------------------------------------------------

/// A MINIMAL Polygon JSON-RPC reader over the crate's existing blocking `ureq` stack. Four read
/// methods, no signing, no transaction submission, no new dependency.
pub struct PolygonRpc {
    url: String,
    agent: ureq::Agent,
    max_span: u64,
}

impl Default for PolygonRpc {
    fn default() -> Self {
        Self::new(&ChainRpcSettings::default())
    }
}

impl PolygonRpc {
    /// Build from the caller's [`ChainRpcSettings`].
    pub fn new(settings: &ChainRpcSettings) -> Self {
        let max_span = if settings.max_span == 0 { DEFAULT_MAX_SPAN } else { settings.max_span };
        PolygonRpc {
            url: settings.rpc_url.clone(),
            agent: Self::build_agent(settings.via_proxy),
            max_span,
        }
    }

    /// An explicit endpoint, direct (the smoke and the offline tests use this).
    pub fn with_url(url: impl Into<String>) -> Self {
        PolygonRpc { url: url.into(), agent: Self::build_agent(false), max_span: DEFAULT_MAX_SPAN }
    }

    pub fn url(&self) -> &str {
        &self.url
    }

    pub fn max_span(&self) -> u64 {
        self.max_span
    }

    /// A fresh agent, tunnelled only when `via_proxy` ([`ChainRpcSettings::via_proxy`]). Deliberately
    /// NOT [`crate::exec_plane::exec`]'s `agent()`: that one tunnels unconditionally, and a Polygon RPC
    /// is not geo-blocked, so paying the tunnel by default would add a failure mode for no benefit.
    fn build_agent(via_proxy: bool) -> ureq::Agent {
        let mut b = ureq::Agent::config_builder()
            .http_status_as_error(false)
            // JSON-RPC needs no redirect (ureq turns a followed POST into a bodyless GET or fails
            // it), and the endpoint URL may carry an API key.
            .max_redirects(0)
            .timeout_global(Some(Duration::from_secs(30)))
            .user_agent("vike-trader-rust");
        if via_proxy
            && let Some(url) = crate::egress::proxy_url()
            && let Ok(p) = ureq::Proxy::new(&url)
        {
            b = b.proxy(Some(p));
        }
        b.build().new_agent()
    }

    /// One JSON-RPC round trip. A JSON-RPC `error` member is a hard `Err` (a silently-`None`
    /// result would read as "no events", the exact failure this module exists to avoid).
    pub fn call(&self, method: &str, params: Value) -> Result<Value, String> {
        let body =
            serde_json::json!({ "jsonrpc": "2.0", "id": 1, "method": method, "params": params });
        let mut resp = self
            .agent
            .post(&self.url)
            .content_type("application/json")
            .send(body.to_string())
            .map_err(|e| format!("rpc {method}: network: {e}"))?;
        let text =
            resp.body_mut().read_to_string().map_err(|e| format!("rpc {method}: body: {e}"))?;
        let v: Value =
            serde_json::from_str(&text).map_err(|e| format!("rpc {method}: bad json: {e}"))?;
        if let Some(err) = v.get("error") {
            return Err(format!("rpc {method}: {err}"));
        }
        v.get("result").cloned().ok_or_else(|| format!("rpc {method}: no result member"))
    }

    pub fn chain_id(&self) -> Result<u64, String> {
        let r = self.call("eth_chainId", serde_json::json!([]))?;
        let s = r.as_str().ok_or("eth_chainId: not a string")?;
        u64::from_str_radix(clean(s).as_str(), 16).map_err(|e| format!("eth_chainId: {e}"))
    }

    pub fn block_number(&self) -> Result<u64, String> {
        let r = self.call("eth_blockNumber", serde_json::json!([]))?;
        let s = r.as_str().ok_or("eth_blockNumber: not a string")?;
        u64::from_str_radix(clean(s).as_str(), 16).map_err(|e| format!("eth_blockNumber: {e}"))
    }

    pub fn eth_call(&self, to: &str, data: &str) -> Result<String, String> {
        let r = self.call("eth_call", serde_json::json!([{ "to": to, "data": data }, "latest"]))?;
        r.as_str().map(|s| s.to_string()).ok_or_else(|| "eth_call: not a string".to_string())
    }

    /// `eth_getLogs`, CHUNKED at [`max_span`](Self::max_span) blocks so a free endpoint's window
    /// cap is a cadence detail rather than a hard failure. `topics` is passed through verbatim, so
    /// a caller can use the `[[a,b], null, x]` OR/positional form.
    pub fn get_logs(
        &self,
        address: &str,
        topics: Value,
        from_block: u64,
        to_block: u64,
    ) -> Result<Vec<Value>, String> {
        let mut out = Vec::new();
        let mut lo = from_block;
        while lo <= to_block {
            let hi = to_block.min(lo + self.max_span.saturating_sub(1));
            let filter = serde_json::json!({
                "address": address,
                "topics": topics,
                "fromBlock": format!("0x{lo:x}"),
                "toBlock": format!("0x{hi:x}"),
            });
            let r = self.call("eth_getLogs", serde_json::json!([filter]))?;
            let arr = r.as_array().ok_or("eth_getLogs: result is not an array")?;
            out.extend(arr.iter().cloned());
            lo = hi + 1;
        }
        Ok(out)
    }

    pub fn transaction_receipt(&self, tx_hash: &str) -> Result<Option<Value>, String> {
        let r = self.call("eth_getTransactionReceipt", serde_json::json!([tx_hash]))?;
        Ok(if r.is_null() { None } else { Some(r) })
    }

    /// **The authoritative winner read**: the CTF's own payout mappings for one condition, via
    /// `eth_call` — no log window, no archive node, and correct for a SPLIT resolution that no
    /// flag-based heuristic can express. `denominator == 0` ⇒ not resolved (and the numerators are
    /// then not fetched at all).
    pub fn condition_resolution(&self, condition_id: &str) -> Result<ChainResolution, String> {
        let cid = clean(condition_id);
        if cid.len() != 64 {
            return Err(format!("conditionId must be 32 bytes, got {} hex chars", cid.len()));
        }
        let denominator = decode_uint_result(
            &self.eth_call(CTF_ADDRESS, &format!("{SEL_PAYOUT_DENOMINATOR}{cid}"))?,
        )?;
        if denominator == 0 {
            return Ok(ChainResolution {
                condition_id: condition_id.to_string(),
                denominator: 0,
                numerators: Vec::new(),
            });
        }
        let slots = decode_uint_result(
            &self.eth_call(CTF_ADDRESS, &format!("{SEL_OUTCOME_SLOT_COUNT}{cid}"))?,
        )?;
        let mut numerators = Vec::with_capacity(slots as usize);
        for i in 0..slots {
            let arg = format!("{i:064x}");
            numerators.push(decode_uint_result(
                &self.eth_call(CTF_ADDRESS, &format!("{SEL_PAYOUT_NUMERATORS}{cid}{arg}"))?,
            )?);
        }
        Ok(ChainResolution { condition_id: condition_id.to_string(), denominator, numerators })
    }
}
