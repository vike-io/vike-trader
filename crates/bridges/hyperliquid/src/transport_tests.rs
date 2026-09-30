use super::*;
// Everything below that names the signing types is `exec`-gated with its half of the
// transport; the `info_weight` schedule test at the bottom is the feeds-plane residue and
// stays feature-less. A default `cargo test` compiles and runs all of it unchanged.
#[cfg(feature = "exec")]
use crate::signing::action::{
    BatchModifyAction, CancelAction, CancelByCloidAction, CancelCloidWire, CancelWire, LimitParams,
    ModifyWire, OrderAction, OrderKind, OrderWire,
};

/// The official Rust SDK's test wallet (shared with `tests/signing_vectors.rs`) — a valid
/// secp256k1 key, so the `Signer` constructs and signing is exercised end-to-end.
#[cfg(feature = "exec")]
const KEY: &str = "e908f86dbb4d55ac876378565aafeabc187f6690f046459397b17d9b9a19688e";
#[cfg(feature = "exec")]
const NONCE: u64 = 1583838;

#[cfg(feature = "exec")]
fn signer() -> Signer {
    Signer::from_private_key(KEY, Network::Mainnet).expect("SDK test key is valid")
}

/// A single-order `order` action (asset 1, buy 3.5 @ 2000, Ioc) — the shape exec submits.
#[cfg(feature = "exec")]
fn order_action() -> Action {
    Action::Order(OrderAction {
        orders: vec![OrderWire {
            asset: 1,
            is_buy: true,
            limit_px: "2000".to_string(),
            sz: "3.5".to_string(),
            reduce_only: false,
            order_type: OrderKind::Limit(LimitParams { tif: "Ioc".to_string() }),
            cloid: None,
        }],
        grouping: "na".to_string(),
        builder: None,
    })
}

#[cfg(feature = "exec")]
#[test]
fn exchange_body_has_action_nonce_signature_and_omits_vault() {
    let body = exchange_body(&order_action(), &signer(), NONCE, None);

    // action serializes inline as the tagged order envelope.
    assert_eq!(body["action"]["type"], "order");
    assert_eq!(body["action"]["orders"][0]["a"], 1);
    assert_eq!(body["action"]["orders"][0]["p"], "2000");

    // nonce is the numeric u64 we passed.
    assert_eq!(body["nonce"].as_u64(), Some(NONCE));

    // signature is the `{r,s,v}` object: r/s are 0x + 64 hex, v ∈ {27,28}.
    let sig = &body["signature"];
    let r = sig["r"].as_str().expect("r present");
    let s = sig["s"].as_str().expect("s present");
    assert!(r.starts_with("0x") && r.len() == 66, "malformed r: {r}");
    assert!(s.starts_with("0x") && s.len() == 66, "malformed s: {s}");
    assert!(matches!(sig["v"].as_u64(), Some(27) | Some(28)), "v: {}", sig["v"]);

    // no vault → the key is ABSENT (not null), so the venue's hash marker matches.
    assert!(body.get("vaultAddress").is_none(), "vaultAddress must be omitted when absent");
}

#[cfg(feature = "exec")]
#[test]
fn exchange_body_includes_lowercased_vault_address_when_present() {
    let vault = [0xABu8; 20];
    let body = exchange_body(&order_action(), &signer(), NONCE, Some(vault));
    assert_eq!(
        body["vaultAddress"].as_str(),
        Some("0xabababababababababababababababababababab"),
        "vaultAddress is 0x + lowercased 20-byte hex"
    );
}

#[cfg(feature = "exec")]
#[test]
fn signature_is_deterministic_for_a_fixed_action_and_nonce() {
    // RFC-6979 ECDSA ⇒ the same body is produced twice (guards against nondeterministic assembly).
    let a = exchange_body(&order_action(), &signer(), NONCE, None);
    let b = exchange_body(&order_action(), &signer(), NONCE, None);
    assert_eq!(a, b);
}

#[test]
fn info_weight_schedule_matches_research_9() {
    for t in ["l2Book", "clearinghouseState", "orderStatus", "spotClearinghouseState", "allMids"] {
        assert_eq!(info_weight(t), 2, "{t} is a light read");
    }
    assert_eq!(info_weight("userRole"), 60);
    assert_eq!(info_weight("meta"), 20);
    assert_eq!(info_weight("spotMeta"), 20);
    assert_eq!(info_weight(""), 20, "an absent type defaults to the heavy bucket");
}

#[cfg(feature = "exec")]
#[test]
fn exchange_weight_is_one_plus_batch_over_forty() {
    assert_eq!(exchange_weight(0), 1);
    assert_eq!(exchange_weight(1), 1);
    assert_eq!(exchange_weight(39), 1);
    assert_eq!(exchange_weight(40), 2);
    assert_eq!(exchange_weight(79), 2);
    assert_eq!(exchange_weight(80), 3);
}

#[cfg(feature = "exec")]
#[test]
fn action_batch_len_reads_each_variant() {
    let order = OrderWire {
        asset: 1,
        is_buy: true,
        limit_px: "2000".to_string(),
        sz: "3.5".to_string(),
        reduce_only: false,
        order_type: OrderKind::Limit(LimitParams { tif: "Gtc".to_string() }),
        cloid: None,
    };
    assert_eq!(action_batch_len(&order_action()), 1);
    assert_eq!(
        action_batch_len(&Action::Cancel(CancelAction {
            cancels: vec![CancelWire { asset: 1, oid: 1 }, CancelWire { asset: 1, oid: 2 }],
        })),
        2
    );
    assert_eq!(
        action_batch_len(&Action::CancelByCloid(CancelByCloidAction {
            cancels: vec![CancelCloidWire { asset: 1, cloid: "0x00".to_string() }],
        })),
        1
    );
    assert_eq!(action_batch_len(&Action::Modify(ModifyWire { oid: 1, order: order.clone() })), 1);
    assert_eq!(
        action_batch_len(&Action::BatchModify(BatchModifyAction {
            modifies: vec![
                ModifyWire { oid: 1, order: order.clone() },
                ModifyWire { oid: 2, order },
            ],
        })),
        2
    );
}
