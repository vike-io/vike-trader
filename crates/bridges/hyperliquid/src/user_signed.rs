//! Standalone USER-signed `/exchange` actions (`HyperliquidSignTransaction` scheme), off the exec hot
//! path: `transfer` (`usdClassTransfer`) and `builder_fee` (`approveBuilderFee`). The venue resolves
//! such an action to the RECOVERED signer's own account (see each module's doc for the measurement).

use serde_json::Value;
use vike_bridge_core::transport::VenueApiError;

use crate::base::config::Network;
use crate::base::consts::VENUE;
use crate::base::transport::HyperliquidTransport;

pub mod builder_fee;
pub mod transfer;

/// POST one user-signed `action` (its wire name, the rate gate's label): a raw wall-clock ms nonce
/// (a one-shot operator action has no same-ms burst to dedup, unlike the exec order path's
/// `crate::signing::NonceManager`), the body `payload` builds over that nonce, then IP weight 1
/// (`transport::exchange_weight(0)`) on the same per-IP budget window as orders and reads, so a
/// manual action cannot overrun it.
fn post_action(
    transport: &HyperliquidTransport,
    network: Network,
    action: &str,
    payload: impl FnOnce(u64) -> Result<Vec<u8>, VenueApiError>,
) -> Result<Value, VenueApiError> {
    let nonce = vike_model::time::clock::now_ms_u64();
    let payload = payload(nonce)?;
    transport.rate_gate().proceed_cost_logged(VENUE, action, 1);
    let (_, exchange_url, _) = network.urls();
    transport.post(exchange_url, &payload)
}

#[path = "user_signed_tests.rs"]
#[cfg(test)]
mod user_signed_tests;
