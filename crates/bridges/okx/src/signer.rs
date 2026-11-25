//! OKX's request signer, [`OkxV5Signer`]. It moved here from `vike_bridge_core::signer`, which keeps the
//! venue-neutral half it is built from: the [`SignerJson`] seam, the HMAC helper, the pair encoders and the
//! key/secret/clock core [`KeyClock`]. The signed bytes did not change: the goldens in `signer_tests.rs` are the ones
//! the shared module held.
//!
//! HARD: the secret/signature never reach Debug/Display/any log line.

use vike_bridge_core::signer::{
    KeyClock, PreparedRequest, SignerJson, compact_json, hmac_sha256_base64, py_str, urlencode,
};
use vike_secrets::credentials::Credentials;

/// OKX V5: HMAC-SHA256 **base64** over `ts + METHOD + requestPath + body`, carried in
/// OK-ACCESS-* headers with the passphrase. GET signs `path?query`; POST signs the EXACT
/// json body bytes (sign-then-send).
pub struct OkxV5Signer {
    keys: KeyClock,
    passphrase: String,
}

impl OkxV5Signer {
    pub fn new(
        credentials: &Credentials,
        now_ms: impl Fn() -> i64 + Send + Sync + 'static,
    ) -> Self {
        OkxV5Signer {
            keys: KeyClock::new(credentials, now_ms),
            passphrase: credentials
                .passphrase
                .as_ref()
                .map(|p| p.expose().clone())
                .unwrap_or_default(),
        }
    }

    pub fn set_offset_ms(&self, offset_ms: i64) {
        self.keys.set_offset_ms(offset_ms);
    }

    pub fn prepare(
        &self,
        params: &[(&str, serde_json::Value)],
        method: &str,
        path: &str,
    ) -> PreparedRequest {
        let ts = vike_model::time::epoch_ms_to_utc_timestamp_ms(self.keys.now_ms());
        let (request_path, query, body_str, body) = if method.eq_ignore_ascii_case("GET") {
            let owned: Vec<(&str, String)> = params.iter().map(|(k, v)| (*k, py_str(v))).collect();
            let q = urlencode(&owned);
            let rp = if q.is_empty() { path.to_string() } else { format!("{path}?{q}") };
            (rp, q, String::new(), None)
        } else {
            let b = compact_json(params);
            (path.to_string(), String::new(), b.clone(), Some(b.into_bytes()))
        };
        let prehash = format!("{ts}{}{request_path}{body_str}", method.to_uppercase());
        let sign = hmac_sha256_base64(self.keys.secret(), prehash.as_bytes());
        PreparedRequest {
            query,
            body,
            headers: vec![
                ("OK-ACCESS-KEY".to_string(), self.keys.key().to_string()),
                ("OK-ACCESS-SIGN".to_string(), sign),
                ("OK-ACCESS-TIMESTAMP".to_string(), ts),
                ("OK-ACCESS-PASSPHRASE".to_string(), self.passphrase.clone()),
            ],
        }
    }

    /// Sign an ALREADY-serialized JSON body (POST-family). For endpoints whose body is a JSON
    /// ARRAY — batch-orders / cancel-batch-orders — which the flat-object `prepare` can't express.
    /// The signature scheme is identical (`{ts}{METHOD}{path}{body}`); only the body shape differs.
    pub fn prepare_json(&self, body: &str, method: &str, path: &str) -> PreparedRequest {
        let ts = vike_model::time::epoch_ms_to_utc_timestamp_ms(self.keys.now_ms());
        let prehash = format!("{ts}{}{path}{body}", method.to_uppercase());
        let sign = hmac_sha256_base64(self.keys.secret(), prehash.as_bytes());
        PreparedRequest {
            query: String::new(),
            body: Some(body.as_bytes().to_vec()),
            headers: vec![
                ("OK-ACCESS-KEY".to_string(), self.keys.key().to_string()),
                ("OK-ACCESS-SIGN".to_string(), sign),
                ("OK-ACCESS-TIMESTAMP".to_string(), ts),
                ("OK-ACCESS-PASSPHRASE".to_string(), self.passphrase.clone()),
            ],
        }
    }
}

impl SignerJson for OkxV5Signer {
    fn prepare(
        &self,
        params: &[(&str, serde_json::Value)],
        method: &str,
        path: &str,
    ) -> PreparedRequest {
        OkxV5Signer::prepare(self, params, method, path)
    }

    fn prepare_body(&self, body: &str, method: &str, path: &str) -> PreparedRequest {
        self.prepare_json(body, method, path)
    }
}

impl std::fmt::Debug for OkxV5Signer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let tail = vike_model::secret::key_tail(self.keys.key());
        write!(f, "OKXV5Signer(key=***{tail})")
    }
}

#[path = "signer_tests.rs"]
#[cfg(test)]
mod signer_tests;
