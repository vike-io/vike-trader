//! Request signing seam: the venue-neutral half every venue signer is built from. [`Signer`] signs string pairs (the
//! binance-shaped `RestTransport`); [`SignerJson`] signs typed JSON pairs (bybit, okx: the JSON transport). The
//! shared HMAC-SHA256 helpers, the pair encoders ([`urlencode`], [`py_str`], [`compact_json`]), one client's venue
//! clock ([`VenueClock`]) and the key, secret and clock a signer holds ([`KeyClock`]) live here.
//!
//! Each VENUE's signer is its bridge's (a one-venue fact never sits in a shared module):
//! `crates/bridges/binance/src/family/signer.rs`'s `BinanceHmacSigner` (binance and aster),
//! `crates/bridges/bybit/src/signer.rs`'s `BybitV5Signer` and `crates/bridges/okx/src/signer.rs`'s `OkxV5Signer`.
//!
//! A signer's `prepare(params)` stamps timestamp + recvWindow, signs the query string, returns the
//! [`PreparedRequest`]. Params are ORDERED pairs: the signature covers them in insertion order.
//! HARD: the secret/signature never reach Debug/Display/any log line.

use hmac::{Hmac, KeyInit, Mac};
use sha2::Sha256;

use vike_secrets::credentials::Credentials;

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

/// The signer of a venue whose params are TYPED JSON pairs (bybit, okx), what
/// [`crate::transport::json::RestTransportJson`] signs through: a GET signs its urlencoded query, a POST the EXACT
/// compact-JSON bytes that are then sent (sign-then-send; re-serializing breaks the signature).
pub trait SignerJson: Send {
    /// Sign `params` for `method` on `path`.
    fn prepare(
        &self,
        params: &[(&str, serde_json::Value)],
        method: &str,
        path: &str,
    ) -> PreparedRequest;

    /// Sign an already-serialized JSON body (a JSON ARRAY, which no flat pair list expresses), sent verbatim.
    fn prepare_body(&self, body: &str, method: &str, path: &str) -> PreparedRequest;
}

/// Form-urlencode ordered pairs (quote_plus): alphanumerics and `_.-~` unescaped, space → `+`,
/// everything else %XX (uppercase hex).
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
/// construct/update/finalize site for every REST/WS signer (the venue signers, binance/bybit/okx
/// `websocket_auth`, polymarket `auth`).
///
/// Construction is INFALLIBLE: HMAC (RFC 2104) accepts a key of ANY length (shorter keys are
/// zero-padded, longer keys are pre-hashed), so `Hmac::<Sha256>::new_from_slice` cannot fail.
/// Callers pick an encoding via [`hmac_sha256_hex`] / [`hmac_sha256_base64`], or encode this raw
/// tag themselves (Polymarket's signature is base64**url**). NOT for verification — a verifier
/// must compare in constant time (`Mac::verify_slice`, as `crates/vike-hub-protocol/src/auth.rs`'s
/// `verify` does), never `==` on an encoded tag.
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

/// The ceiling on one read of a venue's clock ([`VenueClock`]): the read can sit in front of an order, and a host that
/// does not answer must not hold the exec thread for the shared agent's 30 s. The preflight's clock read has the same
/// figure.
pub const VENUE_CLOCK_READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);

/// How long after a failed read of a venue's clock a signed request asks again.
pub const VENUE_CLOCK_READ_RETRY_MS: i64 = 60_000;

/// **One client's offset from its venue's clock**, shared by clone. A venue refuses a request stamped outside its window
/// (binance's `timestamp` against a 5 s `recvWindow`, `-1021`; bybit's `10002`; aster's µs `nonce`, about 10 s), so every
/// signer and auth frame of one exec client and its reconcile client takes its stamp from one (the adapter builds it and
/// hands it to both); a signer built for one call holds its own. The transport those clients sign through reads it before
/// the first signed request, so a client whose build must stay pure still signs on the venue's clock.
///
/// **Nothing moves at zero skew:** a new clock is the local clock plus zero, the same bytes. A read that fails leaves the
/// offset as it is, the local clock until one answers. It is read once and then kept, so a host clock stepped later is
/// not followed: the preflight's clock leg is what judges the host.
///
/// The offset rides the `now_ms` a signer is already built with ([`Self::ms`]), so the signers' own `set_offset_ms`
/// (smokes and parity tests) is untouched.
#[derive(Clone, Default)]
pub struct VenueClock(std::sync::Arc<VenueClockOffset>);

#[derive(Default)]
struct VenueClockOffset {
    ms: std::sync::atomic::AtomicI64,
    /// The local time from which a read is due: 0 before the first, `i64::MAX` once one answered.
    next_read_ms: std::sync::atomic::AtomicI64,
}

impl VenueClock {
    /// The venue's time in ms: what a signer and a subscribe frame are built with.
    pub fn ms(&self) -> impl Fn() -> i64 + Send + Sync + 'static + use<> {
        self.ms_from(vike_model::now_ms)
    }

    /// [`Self::ms`] over a given local clock.
    pub fn ms_from<F>(&self, local: F) -> impl Fn() -> i64 + Send + Sync + 'static + use<F>
    where
        F: Fn() -> i64 + Send + Sync + 'static,
    {
        let offset = std::sync::Arc::clone(&self.0);
        move || local() + offset.ms.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// The venue's time in µs: what a µs-nonce signer (aster) is built with.
    pub fn us(&self) -> impl Fn() -> i64 + Send + Sync + 'static + use<> {
        self.us_from(vike_model::now_us)
    }

    /// [`Self::us`] over a given local clock.
    pub fn us_from<F>(&self, local: F) -> impl Fn() -> i64 + Send + Sync + 'static + use<F>
    where
        F: Fn() -> i64 + Send + Sync + 'static,
    {
        let offset = std::sync::Arc::clone(&self.0);
        move || local() + offset.ms.load(std::sync::atomic::Ordering::Relaxed) * 1000
    }

    /// Run `read` (handed `now_ms`, answering the offset) if one is due. Nothing is due once one has answered, or within
    /// [`VENUE_CLOCK_READ_RETRY_MS`] of one that failed, and only one caller asks at a time: another client signing
    /// meanwhile carries the offset it finds, as it did before any read.
    ///
    /// # Errors
    /// What `read` answered: the offset stays as it was.
    pub fn read_if_due<E>(
        &self,
        now_ms: i64,
        read: impl FnOnce(i64) -> Result<i64, E>,
    ) -> Result<(), E> {
        use std::sync::atomic::Ordering::Relaxed;
        let due = self.0.next_read_ms.load(Relaxed);
        let retry = now_ms + VENUE_CLOCK_READ_RETRY_MS;
        if now_ms < due
            || self.0.next_read_ms.compare_exchange(due, retry, Relaxed, Relaxed).is_err()
        {
            return Ok(());
        }
        self.0.ms.store(read(now_ms)?, Relaxed);
        self.0.next_read_ms.store(i64::MAX, Relaxed);
        Ok(())
    }
}

/// The key, the secret and the skew-corrected clock every venue signer holds (binance, bybit, okx: each in its
/// bridge). No `Debug`: a signer's own prints [`Self::key_tail`] behind `***` and nothing else.
pub struct KeyClock {
    key: String,
    secret: Vec<u8>,
    now_ms: Box<dyn Fn() -> i64 + Send + Sync>,
    offset_ms: std::sync::atomic::AtomicI64,
}

impl KeyClock {
    pub fn new(
        credentials: &Credentials,
        now_ms: impl Fn() -> i64 + Send + Sync + 'static,
    ) -> Self {
        KeyClock {
            key: credentials.api_key.expose().clone(),
            secret: credentials.api_secret.expose().as_bytes().to_vec(),
            now_ms: Box::new(now_ms),
            offset_ms: std::sync::atomic::AtomicI64::new(0),
        }
    }

    /// The API key: what a venue's auth header carries.
    pub fn key(&self) -> &str {
        &self.key
    }

    /// The API secret's bytes: the HMAC key, never logged.
    pub fn secret(&self) -> &[u8] {
        &self.secret
    }

    /// The local clock plus the server-time skew correction, in ms.
    pub fn now_ms(&self) -> i64 {
        (self.now_ms)() + self.offset_ms.load(std::sync::atomic::Ordering::Relaxed)
    }

    pub fn set_offset_ms(&self, offset_ms: i64) {
        self.offset_ms.store(offset_ms, std::sync::atomic::Ordering::Relaxed);
    }

    /// The key's last four characters (empty when it is shorter): all a `Debug` ever shows.
    pub fn key_tail(&self) -> &str {
        if self.key.len() >= 4 { &self.key[self.key.len() - 4..] } else { "" }
    }
}

/// Stringify a JSON param value the way a signed GET query spells it: a string verbatim,
/// `true` → `"True"`, `0` → `"0"`, `null` → `"None"`.
pub fn py_str(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Bool(b) => if *b { "True" } else { "False" }.to_string(),
        serde_json::Value::Number(n) => n.to_string(),
        serde_json::Value::Null => "None".to_string(),
        other => other.to_string(),
    }
}

/// Compact JSON (`,` / `:` separators, no whitespace) over ORDERED pairs — the EXACT bytes that
/// get signed AND sent (sign-then-send; re-serializing breaks X-BAPI-SIGN).
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

#[cfg(test)]
mod hmac_tests {
    //! Pin the shared HMAC-SHA256 helper against RFC 4231 test case 2 (the any-key-length case:
    //! a 4-byte key, shorter than the SHA-256 block), in BOTH venue encodings. The venue signers
    //! and websocket_auth modules all fold through these exact functions, so this vector pins them all.
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
mod key_clock_tests {
    //! `KeyClock`, the core the three venue signers share: the key and secret as stored, the clock plus its
    //! correction, and the tail a `Debug` shows. Each venue's golden signed bytes are its bridge's
    //! (`binance_signs_with_the_offset_clock`, `bybit_…`, `okx_…`), over this same core.
    use super::KeyClock;
    use vike_secrets::credentials::Credentials;

    #[test]
    fn the_clock_carries_its_correction_and_the_debug_tail_is_the_last_four() {
        let k = KeyClock::new(&Credentials::for_test("kkkk1234", "s3cr3t", None), || 1_000);
        assert_eq!(
            (k.key(), k.secret(), k.now_ms(), k.key_tail()),
            ("kkkk1234", &b"s3cr3t"[..], 1_000, "1234")
        );
        k.set_offset_ms(7);
        assert_eq!(k.now_ms(), 1_007);
        assert_eq!(KeyClock::new(&Credentials::for_test("abc", "s", None), || 0).key_tail(), "");
    }
}

#[cfg(test)]
mod venue_clock_tests {
    //! `VenueClock`: nothing moves at zero skew, an offset moves both units by it, and a read is due first after a
    //! failure's window and never after an answer. The signed bytes on such a clock are each venue signer's own
    //! goldens, in its bridge (`an_offset_signs_what_the_signers_own_correction_signs`).
    use super::{VENUE_CLOCK_READ_RETRY_MS, VenueClock};

    const LOCAL_MS: i64 = 1_700_000_000_000;

    #[test]
    fn at_zero_skew_the_stamp_is_the_local_clock() {
        let read_as_zero = VenueClock::default();
        read_as_zero.read_if_due(0, |_| Ok::<_, ()>(0)).unwrap();
        for clock in [VenueClock::default(), read_as_zero] {
            assert_eq!(clock.ms_from(|| LOCAL_MS)(), LOCAL_MS);
            assert_eq!(clock.us_from(|| LOCAL_MS * 1000)(), LOCAL_MS * 1000);
        }
    }

    #[test]
    fn an_offset_moves_the_ms_and_the_us_stamp_by_it() {
        let clock = VenueClock::default();
        clock.read_if_due(0, |_| Ok::<_, ()>(7)).unwrap();
        assert_eq!(clock.ms_from(|| LOCAL_MS)(), LOCAL_MS + 7);
        assert_eq!(clock.us_from(|| LOCAL_MS * 1000)(), LOCAL_MS * 1000 + 7000);
    }

    #[test]
    fn a_read_is_due_first_after_a_failures_window_and_never_after_an_answer() {
        let clock = VenueClock::default();
        assert!(clock.read_if_due(1_000, |_| Err("no")).is_err());
        assert_eq!(clock.ms_from(|| 0)(), 0, "a failure leaves the offset");
        let not_due = |_: i64| -> Result<i64, &str> { panic!("not due") };
        clock.read_if_due(1_000 + VENUE_CLOCK_READ_RETRY_MS - 1, not_due).unwrap();
        clock.read_if_due(1_000 + VENUE_CLOCK_READ_RETRY_MS, |_| Ok::<_, &str>(5)).unwrap();
        assert_eq!(clock.ms_from(|| 0)(), 5);
        clock.read_if_due(i64::MAX - VENUE_CLOCK_READ_RETRY_MS - 1, not_due).unwrap();
    }
}
