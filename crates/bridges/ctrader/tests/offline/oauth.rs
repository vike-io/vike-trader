//! Pure-parser / URL-builder tests for `oauth.rs` — no network. Verbatim from
//! `.superpowers/sdd/task-2-brief.md` Step 1.

use vike_ctrader::oauth::{build_authorize_url, parse_token_response};

#[test]
fn parses_camelcase_token_json() {
    let j = r#"{"accessToken":"AT","refreshToken":"RT","tokenType":"bearer","expiresIn":2628000}"#;
    let t = parse_token_response(j).unwrap();
    assert_eq!(t.access_token, "AT");
    assert_eq!(t.refresh_token, "RT");
    assert_eq!(t.expires_in, 2628000);
}

#[test]
fn parse_surfaces_access_denied() {
    let j = r#"{"errorCode":"ACCESS_DENIED","description":"Access denied."}"#;
    assert!(parse_token_response(j).is_err());
}

#[test]
fn authorize_url_encodes_redirect() {
    let u = build_authorize_url("cid", "http://localhost:5033/", "trading");
    assert!(u.starts_with("https://openapi.ctrader.com/apps/auth?"));
    assert!(u.contains("redirect_uri=http%3A%2F%2Flocalhost%3A5033%2F"));
    assert!(u.contains("scope=trading"));
}

#[test]
fn token_debug_redacts() {
    let t = vike_ctrader::oauth::Token {
        access_token: "SECRET".into(),
        refresh_token: "SEC2".into(),
        expires_in: 1,
    };
    let s = format!("{t:?}");
    assert!(!s.contains("SECRET") && !s.contains("SEC2"));
}
