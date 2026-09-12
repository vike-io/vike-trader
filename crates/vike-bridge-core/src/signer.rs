//! Request signing seam. Exact port of `exec/signer.py` (Binance HMAC for R6 slice 1;
//! Bybit V5 / OKX V5 signers land with their venue slices).
//!
//! `prepare(params)` stamps timestamp + recvWindow, signs the query string, returns the
//! [`PreparedRequest`]. Params are ORDERED pairs (Python signs dict insertion order).
//! HARD: the secret/signature never reach Debug/Display/any log line.

use hmac::{Hmac, KeyInit, Mac};
use sha2::Sha256;

use crate::credentials::Credentials;

/// A signed request the transport can send: GET appends `query` to the URL; POST sends the
/// exact signed `body` bytes (Bybit). `headers` carries the venue auth headers.
#[derive(Debug, Clone, Default)]
pub struct PreparedRequest {
    pub query: String,
    pub body: Option<Vec<u8>>,
    pub headers: Vec<(String, String)>,
}

pub trait Signer: Send {
    fn prepare(&self, params: &[(&str, String)], method: &str, path: &str) -> PreparedRequest;
}

/// Python `urllib.parse.urlencode` twin (quote_plus): alphanumerics and `_.-~` unescaped,
/// space → `+`, everything else %XX (uppercase hex).
pub fn urlencode(pairs: &[(&str, String)]) -> String {
    fn quote_plus(s: &str, out: &mut String) {
        for b in s.bytes() {
            match b {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'_' | b'.' | b'-' | b'~' => {
                    out.push(b as char)
                }
                b' ' => out.push('+'),
                _ => out.push_str(&format!("%{b:02X}")),
            }
        }
    }
    let mut out = String::new();
    for (i, (k, v)) in pairs.iter().enumerate() {
        if i > 0 {
            out.push('&');
        }
        quote_plus(k, &mut out);
        out.push('=');
        quote_plus(v, &mut out);
    }
    out
}

/// HMAC-SHA256 of `payload` keyed by `secret`, as the raw 32-byte tag — the ONE shared
/// construct/update/finalize site (the same three lines were previously copy-pasted at every
/// REST/WS signing site: this module ×4, binance/bybit/okx `ws_auth`, polymarket `auth`).
///
/// Construction is INFALLIBLE: HMAC (RFC 2104) accepts a key of ANY length (shorter keys are
/// zero-padded, longer keys are pre-hashed), so `Hmac::<Sha256>::new_from_slice` cannot fail.
/// Callers pick an encoding via [`hmac_sha256_hex`] / [`hmac_sha256_base64`], or encode this raw
/// tag themselves (Polymarket's signature is base64**url**). NOT for verification — a verifier
/// must compare in constant time (`Mac::verify_slice`, as `vike-tradehub-client::auth` does),
/// never `==` on an encoded tag.
pub fn hmac_sha256(secret: &[u8], payload: &[u8]) -> [u8; 32] {
    let mut mac = Hmac::<Sha256>::new_from_slice(secret).expect("hmac accepts any key length");
    mac.update(payload);
    mac.finalize().into_bytes().into()
}

/// Lowercase-hex [`hmac_sha256`] — the Binance-grammar + Bybit V5 signature encoding (REST and
/// private-WS alike).
pub fn hmac_sha256_hex(secret: &[u8], payload: &[u8]) -> String {
    hex::encode(hmac_sha256(secret, payload))
}

/// Standard-base64 [`hmac_sha256`] — the OKX V5 signature encoding (REST and private-WS alike).
pub fn hmac_sha256_base64(secret: &[u8], payload: &[u8]) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(hmac_sha256(secret, payload))
}

/// HMAC-SHA256 hex over the query string, X-MBX-APIKEY header, ms timestamp + recvWindow.
pub struct BinanceHmacSigner {
    key: String,
    secret: Vec<u8>,
    now_ms: Box<dyn Fn() -> i64 + Send + Sync>,
    recv_window: i64,
    offset_ms: std::sync::atomic::AtomicI64,
}

impl BinanceHmacSigner {
    pub fn new(
        credentials: &Credentials,
        now_ms: impl Fn() -> i64 + Send + Sync + 'static,
    ) -> Self {
        BinanceHmacSigner {
            key: credentials.api_key.clone(),
            secret: credentials.api_secret.as_bytes().to_vec(),
            now_ms: Box::new(now_ms),
            recv_window: 5000,
            offset_ms: std::sync::atomic::AtomicI64::new(0),
        }
    }

    /// Apply a server-time skew correction (from GET /api/v3/time).
    pub fn set_offset_ms(&self, offset_ms: i64) {
        self.offset_ms.store(offset_ms, std::sync::atomic::Ordering::Relaxed);
    }
}

impl Signer for BinanceHmacSigner {
    fn prepare(&self, params: &[(&str, String)], _method: &str, _path: &str) -> PreparedRequest {
        let ts = (self.now_ms)() + self.offset_ms.load(std::sync::atomic::Ordering::Relaxed);
        let mut signed: Vec<(&str, String)> = params.to_vec();
        signed.push(("timestamp", ts.to_string()));
        signed.push(("recvWindow", self.recv_window.to_string()));
        let body = urlencode(&signed);
        let signature = hmac_sha256_hex(&self.secret, body.as_bytes());
        PreparedRequest {
            query: format!("{body}&signature={signature}"),
            body: None,
            headers: vec![("X-MBX-APIKEY".to_string(), self.key.clone())],
        }
    }
}

impl std::fmt::Debug for BinanceHmacSigner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let tail = if self.key.len() >= 4 { &self.key[self.key.len() - 4..] } else { "" };
        write!(f, "BinanceHmacSigner(key=***{tail}, recv_window={})", self.recv_window)
    }
}

/// Python `str()` over the JSON param types Bybit dicts carry (urlencode stringifies
/// values with str(): True -> "True", 0 -> "0").
pub fn py_str(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Bool(b) => if *b { "True" } else { "False" }.to_string(),
        serde_json::Value::Number(n) => n.to_string(),
        serde_json::Value::Null => "None".to_string(),
        other => other.to_string(),
    }
}

/// `json.dumps(params, separators=(",", ":"))` twin over ORDERED pairs — the EXACT bytes
/// that get signed AND sent (sign-then-send; re-serializing breaks X-BAPI-SIGN).
pub fn compact_json(pairs: &[(&str, serde_json::Value)]) -> String {
    let mut out = String::from("{");
    for (i, (k, v)) in pairs.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str(&serde_json::to_string(k).expect("string key"));
        out.push(':');
        out.push_str(&serde_json::to_string(v).expect("json value"));
    }
    out.push('}');
    out
}

/// Bybit V5: HMAC-SHA256 hex over `ts + api_key + recv_window + (queryString | rawJsonBody)`,
/// carried in X-BAPI-* headers. GET signs the urlencoded query; POST signs the EXACT json
/// body bytes that are then sent verbatim. Exact port of `exec/signer.py::BybitV5Signer`.
/// Params are TYPED ordered pairs (Bybit dicts carry ints/bools — `positionIdx: 0`,
/// `reduceOnly: true` — and the JSON body must keep them typed).
pub struct BybitV5Signer {
    key: String,
    secret: Vec<u8>,
    now_ms: Box<dyn Fn() -> i64 + Send + Sync>,
    recv_window: i64,
    offset_ms: std::sync::atomic::AtomicI64,
    /// Fee-attribution broker code (unified cross-venue attribution, task 5), resolved ONCE at
    /// mount from `BYBIT_BROKER_CODE`/`BYBIT_BUILDER_CODE` via `attribution_code_from`. `None`
    /// (the `new()` default, and every non-order-submit signer this crate constructs) means the
    /// `X-Referer` header is never added — byte-identical to before this field existed.
    broker_id: Option<String>,
}

impl BybitV5Signer {
    pub fn new(
        credentials: &Credentials,
        now_ms: impl Fn() -> i64 + Send + Sync + 'static,
    ) -> Self {
        BybitV5Signer {
            key: credentials.api_key.clone(),
            secret: credentials.api_secret.as_bytes().to_vec(),
            now_ms: Box::new(now_ms),
            recv_window: 5000,
            offset_ms: std::sync::atomic::AtomicI64::new(0),
            broker_id: None,
        }
    }

    /// Attach the resolved FD-broker attribution code (task 5): every `prepare()` call after this
    /// then carries an `X-Referer: <id>` header. `None` is a no-op (matches the `new()` default).
    pub fn with_broker_id(mut self, broker_id: Option<String>) -> Self {
        self.broker_id = broker_id;
        self
    }

    /// Apply a server-time skew correction (from GET /v5/market/time).
    pub fn set_offset_ms(&self, offset_ms: i64) {
        self.offset_ms.store(offset_ms, std::sync::atomic::Ordering::Relaxed);
    }

    pub fn prepare(&self, params: &[(&str, serde_json::Value)], method: &str) -> PreparedRequest {
        let ts = ((self.now_ms)() + self.offset_ms.load(std::sync::atomic::Ordering::Relaxed))
            .to_string();
        let recv = self.recv_window.to_string();
        let (payload, query, body) = if method.eq_ignore_ascii_case("GET") {
            let owned: Vec<(&str, String)> = params.iter().map(|(k, v)| (*k, py_str(v))).collect();
            let q = urlencode(&owned);
            (q.clone(), q, None)
        } else {
            let b = compact_json(params);
            (b.clone(), String::new(), Some(b.into_bytes()))
        };
        let sign =
            hmac_sha256_hex(&self.secret, format!("{ts}{}{recv}{payload}", self.key).as_bytes());
        PreparedRequest {
            query,
            body,
            headers: bybit_default_headers(&self.key, &ts, &recv, &sign, self.broker_id.as_deref()),
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

impl std::fmt::Debug for BybitV5Signer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let tail = if self.key.len() >= 4 { &self.key[self.key.len() - 4..] } else { "" };
        write!(f, "BybitV5Signer(key=***{tail}, recv_window={})", self.recv_window)
    }
}

/// Python `datetime.fromtimestamp(ms/1000, tz=utc).strftime("%Y-%m-%dT%H:%M:%S.%f")[:-3]+"Z"`
/// — the OKX REST timestamp format (millisecond precision, trailing Z).
pub fn iso8601_ms(epoch_ms: i64) -> String {
    let secs = epoch_ms.div_euclid(1000);
    let ms = epoch_ms.rem_euclid(1000);
    let days = secs.div_euclid(86_400);
    let sod = secs.rem_euclid(86_400);
    let (h, m, s) = (sod / 3600, (sod % 3600) / 60, sod % 60);
    // civil-from-days (Howard Hinnant's algorithm)
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if month <= 2 { y + 1 } else { y };
    format!("{year:04}-{month:02}-{d:02}T{h:02}:{m:02}:{s:02}.{ms:03}Z")
}

/// OKX V5: HMAC-SHA256 **base64** over `ts + METHOD + requestPath + body`, carried in
/// OK-ACCESS-* headers with the passphrase. GET signs `path?query`; POST signs the EXACT
/// json body bytes (sign-then-send). Exact port of `exec/signer.py::OKXV5Signer`.
pub struct OkxV5Signer {
    key: String,
    secret: Vec<u8>,
    passphrase: String,
    now_ms: Box<dyn Fn() -> i64 + Send + Sync>,
    offset_ms: std::sync::atomic::AtomicI64,
}

impl OkxV5Signer {
    pub fn new(
        credentials: &Credentials,
        now_ms: impl Fn() -> i64 + Send + Sync + 'static,
    ) -> Self {
        OkxV5Signer {
            key: credentials.api_key.clone(),
            secret: credentials.api_secret.as_bytes().to_vec(),
            passphrase: credentials.passphrase.clone().unwrap_or_default(),
            now_ms: Box::new(now_ms),
            offset_ms: std::sync::atomic::AtomicI64::new(0),
        }
    }

    pub fn set_offset_ms(&self, offset_ms: i64) {
        self.offset_ms.store(offset_ms, std::sync::atomic::Ordering::Relaxed);
    }

    pub fn prepare(
        &self,
        params: &[(&str, serde_json::Value)],
        method: &str,
        path: &str,
    ) -> PreparedRequest {
        let ms = (self.now_ms)() + self.offset_ms.load(std::sync::atomic::Ordering::Relaxed);
        let ts = iso8601_ms(ms);
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
        let sign = hmac_sha256_base64(&self.secret, prehash.as_bytes());
        PreparedRequest {
            query,
            body,
            headers: vec![
                ("OK-ACCESS-KEY".to_string(), self.key.clone()),
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
        let ms = (self.now_ms)() + self.offset_ms.load(std::sync::atomic::Ordering::Relaxed);
        let ts = iso8601_ms(ms);
        let prehash = format!("{ts}{}{path}{body}", method.to_uppercase());
        let sign = hmac_sha256_base64(&self.secret, prehash.as_bytes());
        PreparedRequest {
            query: String::new(),
            body: Some(body.as_bytes().to_vec()),
            headers: vec![
                ("OK-ACCESS-KEY".to_string(), self.key.clone()),
                ("OK-ACCESS-SIGN".to_string(), sign),
                ("OK-ACCESS-TIMESTAMP".to_string(), ts),
                ("OK-ACCESS-PASSPHRASE".to_string(), self.passphrase.clone()),
            ],
        }
    }
}

impl std::fmt::Debug for OkxV5Signer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let tail = if self.key.len() >= 4 { &self.key[self.key.len() - 4..] } else { "" };
        write!(f, "OKXV5Signer(key=***{tail})")
    }
}

#[cfg(test)]
mod hmac_tests {
    //! Pin the shared HMAC-SHA256 helper against RFC 4231 test case 2 (the any-key-length case:
    //! a 4-byte key, shorter than the SHA-256 block), in BOTH venue encodings. The venue signers
    //! and ws_auth modules all fold through these exact functions, so this vector pins them all.
    use super::{hmac_sha256, hmac_sha256_base64, hmac_sha256_hex};

    const KEY: &[u8] = b"Jefe";
    const DATA: &[u8] = b"what do ya want for nothing?";
    const TAG_HEX: &str = "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843";

    #[test]
    fn rfc4231_case2_pins_raw_hex_and_base64() {
        assert_eq!(hmac_sha256(KEY, DATA).as_slice(), hex::decode(TAG_HEX).unwrap().as_slice());
        assert_eq!(hmac_sha256_hex(KEY, DATA), TAG_HEX);
        assert_eq!(hmac_sha256_base64(KEY, DATA), "W9zBRr9gdU5qBCQmCJV1x1oAPwidJzmDnexYuWTsOEM=");
    }
}

#[cfg(test)]
mod bybit_attribution_tests {
    //! Unified cross-venue attribution, task 5: Bybit's `X-Referer` header.
    use super::bybit_default_headers;

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
}
