//! Bybit's request signer, [`BybitV5Signer`], and its auth header set ([`bybit_default_headers`]). It moved here from
//! `vike_bridge_core::signer`, which keeps the venue-neutral half it is built from: the [`SignerJson`] seam, the HMAC
//! helper, the pair encoders and the key/secret/clock core [`KeyClock`]. The signed bytes did not change: the goldens
//! in `signer_tests.rs` are the ones the shared module held.
//!
//! HARD: the secret/signature never reach Debug/Display/any log line.

use vike_bridge_core::signer::{
    KeyClock, PreparedRequest, SignerJson, compact_json, hmac_sha256_hex, py_str, urlencode,
};
use vike_secrets::credentials::Credentials;

/// Bybit V5: HMAC-SHA256 hex over `ts + api_key + recv_window + (queryString | rawJsonBody)`,
/// carried in X-BAPI-* headers. GET signs the urlencoded query; POST signs the EXACT json
/// body bytes that are then sent verbatim. Params are TYPED ordered pairs (Bybit params carry
/// ints/bools — `positionIdx: 0`, `reduceOnly: true` — and the JSON body must keep them typed).
pub struct BybitV5Signer {
    keys: KeyClock,
    recv_window: i64,
    /// Fee-attribution broker code, resolved ONCE at start from `BYBIT_BROKER_CODE` /
    /// `BYBIT_BUILDER_CODE` via `attribution_code_from`. `None` (the `new()` default, and every
    /// non-order-submit signer this crate constructs) means the `X-Referer` header is never added.
    broker_id: Option<String>,
}

impl BybitV5Signer {
    pub fn new(
        credentials: &Credentials,
        now_ms: impl Fn() -> i64 + Send + Sync + 'static,
    ) -> Self {
        BybitV5Signer {
            keys: KeyClock::new(credentials, now_ms),
            recv_window: 5000,
            broker_id: None,
        }
    }

    /// Attach the resolved FD-broker attribution code: every `prepare()` call after this
    /// then carries an `X-Referer: <id>` header. `None` is a no-op (matches the `new()` default).
    pub fn with_broker_id(mut self, broker_id: Option<String>) -> Self {
        self.broker_id = broker_id;
        self
    }

    /// Apply a server-time skew correction (from GET /v5/market/time).
    pub fn set_offset_ms(&self, offset_ms: i64) {
        self.keys.set_offset_ms(offset_ms);
    }

    pub fn prepare(&self, params: &[(&str, serde_json::Value)], method: &str) -> PreparedRequest {
        if method.eq_ignore_ascii_case("GET") {
            let owned: Vec<(&str, String)> = params.iter().map(|(k, v)| (*k, py_str(v))).collect();
            let q = urlencode(&owned);
            self.sign(q.clone(), q, None)
        } else {
            self.prepare_body(&compact_json(params))
        }
    }

    /// Sign an already-serialized JSON body, sent verbatim (a POST): what [`Self::prepare`] does with its compact JSON.
    pub fn prepare_body(&self, body: &str) -> PreparedRequest {
        self.sign(body.to_string(), String::new(), Some(body.as_bytes().to_vec()))
    }

    fn sign(&self, payload: String, query: String, body: Option<Vec<u8>>) -> PreparedRequest {
        let ts = self.keys.now_ms().to_string();
        let recv = self.recv_window.to_string();
        let key = self.keys.key();
        let sign =
            hmac_sha256_hex(self.keys.secret(), format!("{ts}{key}{recv}{payload}").as_bytes());
        PreparedRequest {
            query,
            body,
            headers: bybit_default_headers(key, &ts, &recv, &sign, self.broker_id.as_deref()),
        }
    }
}

/// The Bybit V5 auth header set: the four required `X-BAPI-*` signing headers, plus — only when
/// `broker_id` is `Some` — `X-Referer` carrying the FD-broker attribution code (Bybit's Broker
/// Program mechanic, `AttributionMechanic::Header { name: "X-Referer" }`). Extracted from
/// [`BybitV5Signer::prepare`] as a pure fn so the attribution behavior is unit-testable without
/// signing a real request. Absent `broker_id` ⇒ no `X-Referer` key at all (byte-identical).
pub fn bybit_default_headers(
    api_key: &str,
    ts: &str,
    recv_window: &str,
    sign: &str,
    broker_id: Option<&str>,
) -> Vec<(String, String)> {
    let mut headers = vec![
        ("X-BAPI-API-KEY".to_string(), api_key.to_string()),
        ("X-BAPI-TIMESTAMP".to_string(), ts.to_string()),
        ("X-BAPI-RECV-WINDOW".to_string(), recv_window.to_string()),
        ("X-BAPI-SIGN".to_string(), sign.to_string()),
    ];
    if let Some(id) = broker_id {
        headers.push(("X-Referer".to_string(), id.to_string()));
    }
    headers
}

impl SignerJson for BybitV5Signer {
    fn prepare(
        &self,
        params: &[(&str, serde_json::Value)],
        method: &str,
        _path: &str,
    ) -> PreparedRequest {
        BybitV5Signer::prepare(self, params, method)
    }

    fn prepare_body(&self, body: &str, _method: &str, _path: &str) -> PreparedRequest {
        BybitV5Signer::prepare_body(self, body)
    }
}

impl std::fmt::Debug for BybitV5Signer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let tail = self.keys.key_tail();
        write!(f, "BybitV5Signer(key=***{tail}, recv_window={})", self.recv_window)
    }
}

#[path = "signer_tests.rs"]
#[cfg(test)]
mod signer_tests;
