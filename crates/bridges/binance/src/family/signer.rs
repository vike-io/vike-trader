//! The binance family's request signer, [`BinanceHmacSigner`]: binance and aster sign their HMAC requests with it
//! (aster's EIP-712 orders sign with `crates/bridges/aster/src/transport/sign.rs`'s `SignerAster`). It moved here from
//! `vike_bridge_core::signer`, which keeps the venue-neutral half it is built from: the [`Signer`] seam, the HMAC
//! helper, [`urlencode`] and the key/secret/clock core [`KeyClock`]. The signed bytes did not change: the goldens in
//! `signer_tests.rs` are the ones the shared module held.
//!
//! HARD: the secret/signature never reach Debug/Display/any log line.

use vike_bridge_core::signer::{KeyClock, PreparedRequest, Signer, hmac_sha256_hex, urlencode};
use vike_secrets::credentials::Credentials;

/// HMAC-SHA256 hex over the query string, X-MBX-APIKEY header, ms timestamp + recvWindow.
pub struct BinanceHmacSigner {
    keys: KeyClock,
    recv_window: i64,
}

impl BinanceHmacSigner {
    pub fn new(
        credentials: &Credentials,
        now_ms: impl Fn() -> i64 + Send + Sync + 'static,
    ) -> Self {
        BinanceHmacSigner { keys: KeyClock::new(credentials, now_ms), recv_window: 5000 }
    }

    /// Apply a server-time skew correction (from GET /api/v3/time).
    pub fn set_offset_ms(&self, offset_ms: i64) {
        self.keys.set_offset_ms(offset_ms);
    }
}

impl Signer for BinanceHmacSigner {
    fn prepare(&self, params: &[(&str, String)], _method: &str, _path: &str) -> PreparedRequest {
        let ts = self.keys.now_ms();
        let mut signed: Vec<(&str, String)> = params.to_vec();
        signed.push(("timestamp", ts.to_string()));
        signed.push(("recvWindow", self.recv_window.to_string()));
        let body = urlencode(&signed);
        let signature = hmac_sha256_hex(self.keys.secret(), body.as_bytes());
        PreparedRequest {
            query: format!("{body}&signature={signature}"),
            body: None,
            headers: vec![("X-MBX-APIKEY".to_string(), self.keys.key().to_string())],
        }
    }
}

impl std::fmt::Debug for BinanceHmacSigner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let tail = self.keys.key_tail();
        write!(f, "BinanceHmacSigner(key=***{tail}, recv_window={})", self.recv_window)
    }
}

#[path = "signer_tests.rs"]
#[cfg(test)]
mod signer_tests;
