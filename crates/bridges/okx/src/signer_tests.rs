//! Golden signed output of [`OkxV5Signer`], computed independently (Python `hmac`) over a fixed clock plus an
//! offset, its `Debug` line and its `SignerJson` face: the bytes `vike_bridge_core::signer` pinned before the signer
//! moved here, unchanged.

use serde_json::json;
use vike_bridge_core::signer::{PreparedRequest, SignerJson, VenueClock};
use vike_secrets::credentials::Credentials;

use super::OkxV5Signer;

const LOCAL_MS: i64 = 1_700_000_000_000;

/// The `OK-ACCESS-SIGN` of [`order_query`] as a GET stamped 7 ms after [`LOCAL_MS`].
const SIGN_AT_PLUS_7: &str = "jC4QtwO8Z7h1bchIUaIUuGmWSfRiiNr6paLzZMGIplE=";

fn creds() -> Credentials {
    Credentials::for_test("kkkk1234", "s3cr3t", Some("pp"))
}

fn order_query() -> [(&'static str, serde_json::Value); 2] {
    [("instId", json!("BTC-USDT-SWAP")), ("ordId", json!("42"))]
}

fn parts(p: PreparedRequest) -> (String, Option<Vec<u8>>, Vec<(String, String)>) {
    (p.query, p.body, p.headers)
}

#[test]
fn okx_signs_with_the_offset_clock() {
    let s = OkxV5Signer::new(&creds(), || LOCAL_MS);
    s.set_offset_ms(7);
    let p = s.prepare(&order_query(), "GET", "/api/v5/trade/order");
    assert_eq!(p.query, "instId=BTC-USDT-SWAP&ordId=42");
    assert_eq!(
        p.headers,
        vec![
            ("OK-ACCESS-KEY".to_string(), "kkkk1234".to_string()),
            ("OK-ACCESS-SIGN".to_string(), SIGN_AT_PLUS_7.to_string()),
            ("OK-ACCESS-TIMESTAMP".to_string(), "2023-11-14T22:13:20.007Z".to_string()),
            ("OK-ACCESS-PASSPHRASE".to_string(), "pp".to_string()),
        ]
    );
    assert_eq!(format!("{s:?}"), "OKXV5Signer(key=***1234)");
}

/// The golden of `okx_signs_with_the_offset_clock`, which corrects by `set_offset_ms(7)`: a clock read as 7 ms ahead
/// signs it.
#[test]
fn an_offset_signs_what_the_signers_own_correction_signs() {
    let clock = VenueClock::default();
    clock.read_if_due(0, |_| Ok::<_, ()>(7)).unwrap();
    let okx = OkxV5Signer::new(&creds(), clock.ms_from(|| LOCAL_MS));
    let p = okx.prepare(&order_query(), "GET", "/api/v5/trade/order");
    assert!(p.headers.contains(&("OK-ACCESS-SIGN".to_string(), SIGN_AT_PLUS_7.to_string())));
}

/// `SignerJson` is the signer's own signing, unchanged: a pre-serialized body signs what the pairs it spells sign,
/// and the trait's `prepare` is the signer's.
#[test]
fn a_body_signs_what_the_pairs_that_spell_it_sign() {
    let pairs = [("category", json!("linear")), ("reduceOnly", json!(true))];
    let body = r#"{"category":"linear","reduceOnly":true}"#;
    let okx = OkxV5Signer::new(&creds(), || LOCAL_MS);
    assert_eq!(
        parts(SignerJson::prepare_body(&okx, body, "POST", "/p")),
        parts(okx.prepare(&pairs, "POST", "/p"))
    );
    for (method, path) in [("GET", "/p"), ("POST", "/p")] {
        let signer: &dyn SignerJson = &okx;
        assert_eq!(
            parts(signer.prepare(&pairs, method, path)),
            parts(okx.prepare(&pairs, method, path))
        );
    }
}
