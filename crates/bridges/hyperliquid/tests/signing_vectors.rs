//! Golden signature vectors — THE correctness gate for the signing core (a wrong byte silently
//! rejects orders). Every expected value below is ported **verbatim** from the official Hyperliquid
//! Rust SDK's hardcoded `#[cfg(test)]` vectors (`src/exchange/exchange_client.rs` +
//! `src/signature/create_signature.rs`, commit `master`), cross-validated against the canonical
//! Python SDK (`hyperliquid/utils/signing.py`). Nothing here is invented: if the fixture is wrong
//! the test is meaningless, so the values are transcribed exactly and asserted byte-exact on BOTH
//! mainnet (`source:"a"`) and testnet (`source:"b"`).
//!
//! Coverage (L1 phantom-agent scheme — all v1 needs):
//! - `low_level_agent_sign_fixed_connection_id` — isolates [`eip712::sign_agent`] with a fixed
//!   `connectionId` (no msgpack), pinning the Exchange domain + `Agent` struct hash + secp256k1
//!   sign independently of the action encoding.
//! - `limit_order_no_cloid` / `_with_cloid` — the `order` action msgpack (`a,b,p,s,r,t,c` field
//!   order, `Ioc` limit, absent-vs-present `c`).
//! - `tpsl_trigger_order` — the `{trigger:{isMarket,triggerPx,tpsl}}` order kind, `tp` and `sl`.
//! - `cancel_by_oid` — the `cancel` action (`{a,o}` items).
//!
//! The shared inputs are the SDK's test key + nonce; prices are the SDK's un-normalized `"2000.0"`
//! /`"3.5"` strings (NOT routed through [`vike_hyperliquid::px`] — the golden test must reproduce the
//! SDK's exact signed bytes; both `"2000.0"` and the canonical `"2000"` are valid on the wire).

use vike_hyperliquid::config::Network;
use vike_hyperliquid::signing::action::{
    Action, BatchModifyAction, CancelAction, CancelByCloidAction, CancelCloidWire, CancelWire,
    LimitParams, ModifyWire, OrderAction, OrderKind, OrderWire, TriggerParams,
};
use vike_hyperliquid::signing::{Signature, Signer, eip712};

/// The official Rust SDK's test wallet (`exchange_client.rs::get_wallet`).
const KEY: &str = "e908f86dbb4d55ac876378565aafeabc187f6690f046459397b17d9b9a19688e";
/// The SDK's fixed test nonce (`action.hash(1583838, None)`).
const NONCE: u64 = 1583838;

/// Reconstruct the SDK's `Signature::to_string()` form = `0x` ‖ r(32B) ‖ s(32B) ‖ v(1B), from our
/// wire `Signature { r:"0x…", s:"0x…", v }`. This is the exact string the SDK asserts against.
fn wire65(sig: &Signature) -> String {
    format!("0x{}{}{:02x}", &sig.r[2..], &sig.s[2..], sig.v)
}

fn signer(net: Network) -> Signer {
    Signer::from_private_key(KEY, net).expect("the SDK test key is a valid secp256k1 key")
}

/// Sign `action` at [`NONCE`] (no vault, no expiresAfter — the SDK vectors' shape) on BOTH networks
/// and assert byte-exact against the SDK's `(mainnet, testnet)` hex. Exercises the full production
/// path: `action_hash` (msgpack) → phantom-agent digest → secp256k1 sign.
fn assert_l1(action: &Action, mainnet_hex: &str, testnet_hex: &str) {
    let m = signer(Network::Mainnet).sign_l1_action(action, NONCE, None, None);
    assert_eq!(wire65(&m), mainnet_hex, "mainnet (source 'a') signature mismatch");
    let t = signer(Network::Testnet).sign_l1_action(action, NONCE, None, None);
    assert_eq!(wire65(&t), testnet_hex, "testnet (source 'b') signature mismatch");
}

/// The SDK's canonical test order (asset 1, buy 3.5 @ 2000.0, not reduce-only) with a given kind
/// and optional cloid, wrapped as a single-order `order` action, `grouping:"na"`, no builder.
fn base_order(kind: OrderKind, cloid: Option<String>) -> Action {
    Action::Order(OrderAction {
        orders: vec![OrderWire {
            asset: 1,
            is_buy: true,
            limit_px: "2000.0".to_string(),
            sz: "3.5".to_string(),
            reduce_only: false,
            order_type: kind,
            cloid,
        }],
        grouping: "na".to_string(),
        builder: None,
    })
}

#[test]
fn low_level_agent_sign_fixed_connection_id() {
    // create_signature.rs::test_sign_l1_action — a FIXED connectionId, so this pins the eip712
    // layer (Exchange domain + Agent struct hash + sign) with zero msgpack involvement.
    let mut cid = [0u8; 32];
    hex::decode_to_slice(
        "de6c4037798a4434ca03cd05f00e3b803126221375cd1e7eaaaf041768be06eb",
        &mut cid,
    )
    .unwrap();

    let mainnet = eip712::sign_agent("a", &cid, 1337, KEY).unwrap();
    assert_eq!(
        format!("0x{}", hex::encode(mainnet)),
        "0xfa8a41f6a3fa728206df80801a83bcbfbab08649cd34d9c0bfba7c7b2f99340f53a00226604567b98a1492803190d65a201d6805e5831b7044f17fd530aec7841c"
    );
    let testnet = eip712::sign_agent("b", &cid, 1337, KEY).unwrap();
    assert_eq!(
        format!("0x{}", hex::encode(testnet)),
        "0x1713c0fc661b792a50e8ffdd59b637b1ed172d9a3aa4d801d9d88646710fb74b33959f4d075a7ccbec9f2374a6da21ffa4448d58d0413a0d335775f680a881431c"
    );
}

#[test]
fn limit_order_no_cloid() {
    // exchange_client.rs::test_limit_order_action_hashing
    assert_l1(
        &base_order(OrderKind::Limit(LimitParams { tif: "Ioc".to_string() }), None),
        "0x77957e58e70f43b6b68581f2dc42011fc384538a2e5b7bf42d5b936f19fbb67360721a8598727230f67080efee48c812a6a4442013fd3b0eed509171bef9f23f1c",
        "0xcd0925372ff1ed499e54883e9a6205ecfadec748f80ec463fe2f84f1209648776377961965cb7b12414186b1ea291e95fd512722427efcbcfb3b0b2bcd4d79d01c",
    );
}

#[test]
fn limit_order_with_cloid() {
    // exchange_client.rs::test_limit_order_action_hashing_with_cloid.
    // cloid = uuid_to_hex_string(1e60610f-0b3d-4205-97c8-8c1fed2ad5ee) = 0x + the 16 bytes in order.
    let cloid = "0x1e60610f0b3d420597c88c1fed2ad5ee".to_string();
    assert_l1(
        &base_order(OrderKind::Limit(LimitParams { tif: "Ioc".to_string() }), Some(cloid)),
        "0xd3e894092eb27098077145714630a77bbe3836120ee29df7d935d8510b03a08f456de5ec1be82aa65fc6ecda9ef928b0445e212517a98858cfaa251c4cd7552b1c",
        "0x3768349dbb22a7fd770fc9fc50c7b5124a7da342ea579b309f58002ceae49b4357badc7909770919c45d850aabb08474ff2b7b3204ae5b66d9f7375582981f111c",
    );
}

#[test]
fn tpsl_trigger_order() {
    // exchange_client.rs::test_tpsl_order_action_hashing — trigger kind, is_market:true, px 2000.0.
    let tp = base_order(
        OrderKind::Trigger(TriggerParams {
            is_market: true,
            trigger_px: "2000.0".to_string(),
            tpsl: "tp".to_string(),
        }),
        None,
    );
    assert_l1(
        &tp,
        "0xb91e5011dff15e4b4a40753730bda44972132e7b75641f3cac58b66159534a170d422ee1ac3c7a7a2e11e298108a2d6b8da8612caceaeeb3e571de3b2dfda9e41b",
        "0x6df38b609904d0d4439884756b8f366f22b3a081801dbdd23f279094a2299fac6424cb0cdc48c3706aeaa368f81959e91059205403d3afd23a55983f710aee871b",
    );

    let sl = base_order(
        OrderKind::Trigger(TriggerParams {
            is_market: true,
            trigger_px: "2000.0".to_string(),
            tpsl: "sl".to_string(),
        }),
        None,
    );
    assert_l1(
        &sl,
        "0x8456d2ace666fce1bee1084b00e9620fb20e810368841e9d4dd80eb29014611a0843416e51b1529c22dd2fc28f7ff8f6443875635c72011f60b62cbb8ce90e2d1c",
        "0xeb5bdb52297c1d19da45458758bd569dcb24c07e5c7bd52cf76600fd92fdd8213e661e21899c985421ec018a9ee7f3790e7b7d723a9932b7b5adcd7def5354601c",
    );
}

#[test]
fn cancel_by_oid() {
    // exchange_client.rs::test_cancel_action_hashing — cancel {a:1, o:82382}.
    let action =
        Action::Cancel(CancelAction { cancels: vec![CancelWire { asset: 1, oid: 82382 }] });
    assert_l1(
        &action,
        "0x02f76cc5b16e0810152fa0e14e7b219f49c361e3325f771544c6f54e157bf9fa17ed0afc11a98596be85d5cd9f86600aad515337318f7ab346e5ccc1b03425d51b",
        "0x6ffebadfd48067663390962539fbde76cfa36f53be65abe2ab72c9db6d0db44457720db9d7c4860f142a484f070c84eb4b9694c3a617c83f0d698a27e55fd5e01c",
    );
}

#[test]
fn non_vector_actions_serialize_sign_and_are_deterministic() {
    // Modify / batchModify / cancelByCloid have no hardcoded SDK signature vector, so this guards
    // their msgpack + sign path structurally: each must serialize (no `action_hash` panic), produce
    // a well-formed `{r,s,v}`, and be DETERMINISTIC (RFC-6979 ECDSA ⇒ identical bytes for identical
    // input). No invented expected values — only shape + reproducibility are asserted.
    let order = OrderWire {
        asset: 1,
        is_buy: true,
        limit_px: "2000.0".to_string(),
        sz: "3.5".to_string(),
        reduce_only: false,
        order_type: OrderKind::Limit(LimitParams { tif: "Gtc".to_string() }),
        cloid: None,
    };
    let actions = [
        Action::Modify(ModifyWire { oid: 82382, order: order.clone() }),
        Action::BatchModify(BatchModifyAction { modifies: vec![ModifyWire { oid: 82382, order }] }),
        Action::CancelByCloid(CancelByCloidAction {
            cancels: vec![CancelCloidWire {
                asset: 1,
                cloid: "0x1e60610f0b3d420597c88c1fed2ad5ee".to_string(),
            }],
        }),
    ];
    let s = signer(Network::Mainnet);
    for action in &actions {
        let a = s.sign_l1_action(action, NONCE, None, None);
        let b = s.sign_l1_action(action, NONCE, None, None);
        assert_eq!(a, b, "signing must be deterministic for {action:?}");
        assert!(a.r.starts_with("0x") && a.r.len() == 66, "malformed r: {}", a.r);
        assert!(a.s.starts_with("0x") && a.s.len() == 66, "malformed s: {}", a.s);
        assert!(a.v == 27 || a.v == 28, "v out of range: {}", a.v);
    }
}

#[test]
fn signer_address_is_wellformed_lowercase() {
    // Structural check only (no invented golden address — the address-derivation oracle is
    // vike-bridge-core eip712.rs::eth_address_from_cow_key). Proves the SDK key parses and an
    // address is derived.
    let addr = signer(Network::Mainnet).address().to_string();
    assert!(addr.starts_with("0x"), "address must be 0x-prefixed: {addr}");
    assert_eq!(addr.len(), 42, "address must be 0x + 40 hex: {addr}");
    assert_eq!(addr, addr.to_lowercase(), "address must be lowercased for signing: {addr}");
    assert!(addr[2..].chars().all(|c| c.is_ascii_hexdigit()));
}
