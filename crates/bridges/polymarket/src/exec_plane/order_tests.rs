use super::*;

fn sample(neg_risk: bool) -> Order {
    Order {
        salt: 123_456,
        maker: "0x8f0a3e01d916486735a8f6a2ffc0685a3fa57bf5".into(),
        signer: "0x8f0a3e01d916486735a8f6a2ffc0685a3fa57bf5".into(),
        token_id: "71321045679252212594626385532706912750332728571942532289631379312455583992563"
            .into(),
        maker_amount: 52_000_000,
        taker_amount: 100_000_000,
        side: Side::Buy,
        signature_type: SignatureType::PolyProxy,
        timestamp_ms: 1_700_000_000_000,
        metadata: [0u8; 32],
        builder: [0u8; 32],
        neg_risk,
    }
}

const PK: &str = "0xc85ef7d79691fe79573b1a7064c19c1a9819ebdbd1faaab1a8ec92344438aaf4";

#[test]
fn order_signing_is_deterministic() {
    let o = sample(false);
    assert_eq!(sign_order(&o, PK).unwrap(), sign_order(&o, PK).unwrap());
    assert_eq!(sign_order(&o, PK).unwrap().len(), 132); // 0x + 65 bytes
}

// Golden pins for the derived CLOB order id: the EIP-712 order hash of `sample`. Produced with a
// pure-keccak reference that reproduces eip712.rs's own published goldens (keccak("abc") + the
// Ether-Mail domain separator), so a mismatch here means the order domain/type-string/field-order
// changed, not that the golden is unverified.
const ORDER_ID_NON_NEG: &str = "0xc88896a9767976508cb9418fef30b08199be74dbff950d61c4af06618ef3370d";
const ORDER_ID_NEG_RISK: &str =
    "0x430689c15c9081d463d9766484edabd61df3511ba158a95409816f7516db896e";
const ORDER_STRUCT_HASH_HEX: &str =
    "0xbd21ad9dc00175b7c3bf83214a7ee34e2c1baad647f967d8130c3fa79a588c35";

#[test]
fn derive_order_id_is_the_signed_eip712_hash() {
    let o = sample(false);
    let id = derive_order_id(&o);
    // golden pin: the exact derived id + the underlying struct hash.
    assert_eq!(id, ORDER_ID_NON_NEG);
    assert_eq!(format!("0x{}", hex::encode(order_struct_hash(&o))), ORDER_STRUCT_HASH_HEX);
    // shape + determinism
    assert!(id.starts_with("0x"));
    assert_eq!(id.len(), 66);
    assert_eq!(id, derive_order_id(&sample(false)));
    // it IS the prehash `sign_order` signs, for ANY key — so the id is fixed by the order alone,
    // knowable before the signature exists (the whole point of pre-derivation).
    assert_eq!(sign_digest_hex(&order_id_hash(&o), PK).unwrap(), sign_order(&o, PK).unwrap());
    const PK2: &str = "0x1111111111111111111111111111111111111111111111111111111111111111";
    assert_eq!(sign_digest_hex(&order_id_hash(&o), PK2).unwrap(), sign_order(&o, PK2).unwrap());
    // neg-risk flips the verifyingContract → a distinct id (mirrors sign_order's domain split).
    assert_eq!(derive_order_id(&sample(true)), ORDER_ID_NEG_RISK);
    assert_ne!(id, derive_order_id(&sample(true)));
}

#[test]
fn neg_risk_uses_a_distinct_domain() {
    // same order, different market kind → the domain (name+contract) differs → different sig
    assert_ne!(sign_order(&sample(false), PK).unwrap(), sign_order(&sample(true), PK).unwrap());
    assert_ne!(order_struct_hash(&sample(false)), [0u8; 32]);
}

#[test]
fn enum_codes() {
    assert_eq!((Side::Buy.code(), Side::Sell.code()), (0, 1));
    assert_eq!(
        (
            SignatureType::Eoa.code(),
            SignatureType::PolyProxy.code(),
            SignatureType::PolyGnosisSafe.code()
        ),
        (0, 1, 2)
    );
}

#[test]
fn amounts_buy_and_sell() {
    // buy 100 shares @ 0.52 → give 52 USDC, get 100 shares
    let b = build_order(
        0.52,
        100.0,
        Side::Buy,
        "123",
        "0xm",
        "0xs",
        SignatureType::PolyProxy,
        1,
        1,
        false,
        [0u8; 32],
    );
    assert_eq!(b.maker_amount, 52_000_000);
    assert_eq!(b.taker_amount, 100_000_000);
    // sell mirrors it
    let s = build_order(
        0.52,
        100.0,
        Side::Sell,
        "123",
        "0xm",
        "0xs",
        SignatureType::PolyProxy,
        1,
        1,
        false,
        [0u8; 32],
    );
    assert_eq!(s.maker_amount, 100_000_000);
    assert_eq!(s.taker_amount, 52_000_000);
}

#[test]
fn poly1271_signature_is_wrapped_and_deterministic() {
    let mut o = sample(false);
    o.signature_type = SignatureType::Poly1271;
    o.maker = "0x107c01d04fd68557acd52e89dd01972b22803ad5".into();
    o.signer = o.maker.clone(); // 1271: signer == maker == the deposit wallet
    let a = sign_order_1271(&o, PK).unwrap();
    assert_eq!(a, sign_order_1271(&o, PK).unwrap()); // deterministic
    assert!(a.starts_with("0x"));
    // wrapper = 65-byte sig ‖ 32 domainSep ‖ 32 contentsHash ‖ typestring ‖ 2-byte len
    assert_eq!(a.len(), 2 + 2 * (65 + 32 + 32 + ORDER_TYPE.len() + 2));
    assert!(a.ends_with(&format!("{:04x}", ORDER_TYPE.len()))); // trailing type length
    assert_ne!(a, sign_order(&o, PK).unwrap()); // ≠ the plain V2 signature
}

#[test]
fn order_json_shape() {
    let o = build_order(
        0.52,
        100.0,
        Side::Buy,
        "123",
        "0xmaker",
        "0xsigner",
        SignatureType::PolyProxy,
        1_700_000_000_000,
        42,
        false,
        [0u8; 32],
    );
    let j = order_to_json(&o, "0xsig", 0);
    assert_eq!(j["side"], "BUY");
    assert_eq!(j["makerAmount"], "52000000");
    assert_eq!(j["takerAmount"], "100000000");
    assert_eq!(j["signatureType"], 1);
    assert!(j.get("taker").is_none()); // V2 removed taker from the wire
    assert_eq!(j["signature"], "0xsig");
    assert_eq!(j["timestamp"], "1700000000000");
}

#[test]
fn build_order_carries_a_nonzero_builder_code() {
    let mut code = [0u8; 32];
    code[31] = 0xAB;
    let o = build_order(
        0.52,
        10.0,
        Side::Buy,
        "123",
        "0xmaker",
        "0xsigner",
        SignatureType::PolyProxy,
        1_700_000_000_000,
        42,
        false,
        code,
    );
    assert_eq!(o.builder, code, "builder code must reach the order struct");
    // and it must surface on the wire object
    let j = order_to_json(&o, "0xsig", 0);
    assert_eq!(j["builder"], format!("0x{}", hex::encode(code)));
}

#[test]
fn zero_builder_is_byte_identical_to_the_old_default() {
    let o = build_order(
        0.52,
        10.0,
        Side::Buy,
        "123",
        "0xmaker",
        "0xsigner",
        SignatureType::PolyProxy,
        1_700_000_000_000,
        42,
        false,
        [0u8; 32],
    );
    assert_eq!(o.builder, [0u8; 32]);
}
