//! The crate's own test helpers (`test-support`, never in a normal build; the `vike-ctrader` idiom): the
//! SDK's canonical test order, the signer a user-signed signature recovers to, and the raw testnet
//! `/exchange` POST. Unit tests reach them as `crate::test_support`, the `tests/` binaries through the
//! self dev-dependency. None reads the environment or the credential store.

use vike_bridge_core::http::blocking_agent;
use vike_bridge_core::transport::read_body_ambiguous;

use crate::base::consts::TESTNET_EXCHANGE;
use crate::signing::action::{
    Action, HlBuilderFee, LimitParams, OrderAction, OrderKind, OrderWire,
};
use crate::signing::{Signature, eip712};

/// `activeAssetData`'s answer for ETH holding a `kind` (`cross`/`isolated`) leverage of `value`.
pub fn held(kind: &str, value: u64) -> serde_json::Value {
    serde_json::json!({"user":"0xmaster","coin":"ETH","leverage":{"type":kind,"value":value},
           "maxTradeSzs":["1.0","1.0"],"availableToTrade":["8.8","8.8"],"markPx":"3000.0"})
}

/// The Ethereum address `signature` recovers to over `digest`: reassemble `r‖s`, recover the verifying
/// key, derive `keccak256(pubkey[1..])[12..]`. A LOCAL round-trip proof that a user-signed action is
/// signed by the key it claims; it does not prove the VENUE computes the same digest.
pub fn recovered_address(signature: &Signature, digest: &[u8; 32]) -> String {
    use k256::ecdsa::{RecoveryId, Signature as EcdsaSig, VerifyingKey};
    let r = hex::decode(signature.r.strip_prefix("0x").unwrap()).unwrap();
    let s = hex::decode(signature.s.strip_prefix("0x").unwrap()).unwrap();
    let mut rs = [0u8; 64];
    rs[..32].copy_from_slice(&r);
    rs[32..].copy_from_slice(&s);
    let ecdsa_sig = EcdsaSig::from_slice(&rs).expect("valid r,s");
    let recid = RecoveryId::from_byte(signature.v - 27).expect("v ∈ {27,28}");
    let vk = VerifyingKey::recover_from_prehash(digest, &ecdsa_sig, recid)
        .expect("recovery succeeds for a signature we just produced");
    let point = vk.to_sec1_point(false); // 0x04 ‖ X ‖ Y
    format!("0x{}", hex::encode(&eip712::keccak256(&point.as_bytes()[1..])[12..]))
}

/// The SDK's canonical test order: asset 1, BUY 3.5 at `limit_px` as a `tif` limit, no cloid, `na`
/// grouping, `builder` attached. The price and TIF spellings are each caller's (they are signed bytes).
pub fn sdk_order(limit_px: &str, tif: &str, builder: Option<HlBuilderFee>) -> Action {
    Action::Order(OrderAction {
        orders: vec![OrderWire {
            asset: 1,
            is_buy: true,
            limit_px: limit_px.to_string(),
            sz: "3.5".to_string(),
            reduce_only: false,
            order_type: OrderKind::Limit(LimitParams { tif: tif.to_string() }),
            cloid: None,
        }],
        grouping: "na".to_string(),
        builder,
    })
}

/// POST raw `/exchange` bytes to TESTNET and read the verdict back as (status, VERBATIM body).
///
/// A bare agent rather than `HyperliquidTransport::post`, which is `pub(crate)` and parses the body
/// into a `Value`: a certification records the venue's reply exactly as sent, and a probe or a negative
/// control posts bytes the production API deliberately offers no way to build. Same rustls agent and
/// same body reader the real transport uses, so only the bytes differ.
pub fn post_testnet_exchange(payload: &[u8]) -> (u16, String) {
    let agent = blocking_agent();
    let mut resp = agent
        .post(TESTNET_EXCHANGE)
        .header("Content-Type", "application/json")
        .send(payload)
        .expect("testnet /exchange is reachable");
    let status = resp.status().as_u16();
    let text = read_body_ambiguous(&mut resp).expect("response body reads");
    (status, text)
}
