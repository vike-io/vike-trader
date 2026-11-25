//! Golden signed output of [`BybitV5Signer`], computed independently (Python `hmac`) over a fixed clock plus an
//! offset, its `Debug` line, its `SignerJson` face and its `X-Referer` header: the bytes `vike_bridge_core::signer`
//! pinned before the signer moved here, unchanged.

use serde_json::json;
use vike_bridge_core::signer::{PreparedRequest, SignerJson, VenueClock};
use vike_secrets::credentials::Credentials;

use super::{BybitV5Signer, bybit_default_headers};

const LOCAL_MS: i64 = 1_700_000_000_000;

/// The `X-BAPI-SIGN` of [`pairs`] as a POST stamped 7 ms after [`LOCAL_MS`].
const SIGN_AT_PLUS_7: &str = "bc29b9c5277de8f854299cf4872d2b2ed476b588601f2925055d48f1d5bbadeb";

fn creds() -> Credentials {
    Credentials::for_test("kkkk1234", "s3cr3t", Some("pp"))
}

fn pairs() -> [(&'static str, serde_json::Value); 3] {
    [("category", json!("linear")), ("qty", json!("0.01")), ("reduceOnly", json!(true))]
}

fn parts(p: PreparedRequest) -> (String, Option<Vec<u8>>, Vec<(String, String)>) {
    (p.query, p.body, p.headers)
}

#[test]
fn bybit_signs_with_the_offset_clock() {
    let s = BybitV5Signer::new(&creds(), || LOCAL_MS);
    s.set_offset_ms(7);
    let body = r#"{"category":"linear","qty":"0.01","reduceOnly":true}"#;
    let p = s.prepare(&pairs(), "POST");
    assert_eq!(p.body.as_deref(), Some(body.as_bytes()));
    assert_eq!(
        p.headers,
        vec![
            ("X-BAPI-API-KEY".to_string(), "kkkk1234".to_string()),
            ("X-BAPI-TIMESTAMP".to_string(), "1700000000007".to_string()),
            ("X-BAPI-RECV-WINDOW".to_string(), "5000".to_string()),
            ("X-BAPI-SIGN".to_string(), SIGN_AT_PLUS_7.to_string()),
        ]
    );
    assert_eq!(format!("{s:?}"), "BybitV5Signer(key=***1234, recv_window=5000)");
}

/// The golden of `bybit_signs_with_the_offset_clock`, which corrects by `set_offset_ms(7)`: a clock read as 7 ms
/// ahead signs it.
#[test]
fn an_offset_signs_what_the_signers_own_correction_signs() {
    let clock = VenueClock::default();
    clock.read_if_due(0, |_| Ok::<_, ()>(7)).unwrap();
    let p = BybitV5Signer::new(&creds(), clock.ms_from(|| LOCAL_MS)).prepare(&pairs(), "POST");
    assert!(p.headers.contains(&("X-BAPI-TIMESTAMP".to_string(), "1700000000007".to_string())));
    assert!(p.headers.contains(&("X-BAPI-SIGN".to_string(), SIGN_AT_PLUS_7.to_string())));
}

/// `SignerJson` is the signer's own signing, unchanged: a pre-serialized body signs what the pairs it spells sign,
/// and the trait's `prepare` is the signer's.
#[test]
fn a_body_signs_what_the_pairs_that_spell_it_sign() {
    let pairs = [("category", json!("linear")), ("reduceOnly", json!(true))];
    let body = r#"{"category":"linear","reduceOnly":true}"#;
    let bybit = BybitV5Signer::new(&creds(), || LOCAL_MS);
    assert_eq!(parts(bybit.prepare_body(body)), parts(bybit.prepare(&pairs, "POST")));
    for (method, path) in [("GET", "/p"), ("POST", "/p")] {
        let signer: &dyn SignerJson = &bybit;
        assert_eq!(
            parts(signer.prepare(&pairs, method, path)),
            parts(bybit.prepare(&pairs, method))
        );
    }
}

#[test]
fn referer_header_present_only_when_broker_id_configured() {
    let with = bybit_default_headers("key", "1000", "5000", "sig", Some("api.Abc"));
    assert_eq!(
        with.iter().find(|(k, _)| k.eq_ignore_ascii_case("X-Referer")).map(|(_, v)| v.as_str()),
        Some("api.Abc")
    );
    let without = bybit_default_headers("key", "1000", "5000", "sig", None);
    assert!(without.iter().all(|(k, _)| !k.eq_ignore_ascii_case("X-Referer")));
}
