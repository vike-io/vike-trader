//! Polymarket L2 (per-request) authentication — HMAC-SHA256, exactly the shape of the crypto
//! HMAC venues, with two Polymarket-specific gotchas: base64**url** (not std base64) and the
//! timestamp is Unix **seconds** (not ms). The L1 EIP-712 step that DERIVES these L2 creds is a
//! separate slice (needs the eth-crypto deps).
//!
//! Signed string = `timestamp + METHOD + path + body` (body "" for GETs). Key = base64url-decoded
//! L2 secret; digest = base64url-encoded HMAC. Mirrors py-clob-client-v2's `build_hmac_signature`.

use base64::Engine;
use vike_bridge_core::signer::hmac_sha256;

use super::config::PolymarketCreds;

/// L2 auth error (bad secret encoding / key length).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolyAuthError(pub String);

impl std::fmt::Display for PolyAuthError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "polymarket auth: {}", self.0)
    }
}
impl std::error::Error for PolyAuthError {}

/// The L2 HMAC signature (base64url) for one request.
pub fn l2_signature(
    secret_b64url: &str,
    timestamp_secs: i64,
    method: &str,
    path: &str,
    body: &str,
) -> Result<String, PolyAuthError> {
    let key = base64::engine::general_purpose::URL_SAFE
        .decode(secret_b64url.as_bytes())
        .map_err(|e| PolyAuthError(format!("bad L2 secret base64url: {e}")))?;
    // The shared HMAC core is infallible (HMAC accepts any key length — the former
    // `.map_err(… "hmac key")` arm here was unreachable); only the encoding is venue-local.
    let tag = hmac_sha256(&key, format!("{timestamp_secs}{method}{path}{body}").as_bytes());
    Ok(base64::engine::general_purpose::URL_SAFE.encode(tag))
}

/// The full L2 header set for an authenticated CLOB request.
pub fn l2_auth_headers(
    creds: &PolymarketCreds,
    timestamp_secs: i64,
    method: &str,
    path: &str,
    body: &str,
) -> Result<Vec<(String, String)>, PolyAuthError> {
    let sig = l2_signature(&creds.secret, timestamp_secs, method, path, body)?;
    Ok(vec![
        ("POLY_ADDRESS".to_string(), creds.address.clone()),
        ("POLY_SIGNATURE".to_string(), sig),
        ("POLY_TIMESTAMP".to_string(), timestamp_secs.to_string()),
        ("POLY_API_KEY".to_string(), creds.api_key.clone()),
        ("POLY_PASSPHRASE".to_string(), creds.passphrase.clone()),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    // base64url of a fixed 32-byte key ("polymarket-l2-secret-key-1234567" = 31 bytes → pad to 32).
    const SECRET: &str = "cG9seW1hcmtldC1sMi1zZWNyZXQta2V5LTEyMzQ1Njc4"; // 33 raw bytes b64url

    #[test]
    fn l2_signature_deterministic_and_32_bytes() {
        let a = l2_signature(SECRET, 1_700_000_000, "GET", "/order", "").unwrap();
        let b = l2_signature(SECRET, 1_700_000_000, "GET", "/order", "").unwrap();
        assert_eq!(a, b, "same inputs → same signature");
        // a different request → different signature
        let c = l2_signature(SECRET, 1_700_000_000, "POST", "/order", "{}").unwrap();
        assert_ne!(a, c);
        // decodes to a 32-byte HMAC-SHA256 digest
        let raw = base64::engine::general_purpose::URL_SAFE.decode(a.as_bytes()).unwrap();
        assert_eq!(raw.len(), 32);
    }

    #[test]
    fn header_set_is_complete() {
        let creds = PolymarketCreds {
            secret: SECRET.to_string(),
            address: "0xabc".to_string(),
            api_key: "key-1".to_string(),
            passphrase: "pass-1".to_string(),
            ..Default::default()
        };
        let h = l2_auth_headers(&creds, 1_700_000_000, "GET", "/order", "").unwrap();
        let names: Vec<&str> = h.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(
            names,
            ["POLY_ADDRESS", "POLY_SIGNATURE", "POLY_TIMESTAMP", "POLY_API_KEY", "POLY_PASSPHRASE"]
        );
        assert_eq!(h[2].1, "1700000000"); // seconds, not ms
    }

    #[test]
    fn bad_secret_errors() {
        assert!(l2_signature("!!!not-base64!!!", 1, "GET", "/x", "").is_err());
    }
}
