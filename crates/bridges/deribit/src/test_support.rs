//! The crate's own test helpers (`test-support` with `exec`, never in a normal build; the `vike-ctrader`
//! idiom): the TESTNET door the order-placing drills share, the endpoint lock, the public read and the
//! authed order socket they read and clean up through
//! (`crates/bridges/deribit/tests/deribit_any_symbol_testnet_smoke.rs`,
//! `crates/bridges/deribit/tests/deribit_classes_testnet_smoke.rs` and
//! `crates/bridges/deribit/tests/deribit_smokes/deribit_native_stop_testnet_smoke.rs`). It refuses any
//! endpoint but the testnet and reads no environment and no store: each drill resolves its own keys and
//! hands them in.

use serde_json::Value;
use vike_bridge_core::transport::{RestTransport, UreqTransport, VenueApiError};

use crate::transport::{DeribitOrderTransport, TESTNET_REST, TESTNET_WS, is_dead_socket_error};

/// Refuses to go anywhere but the testnet: every drill's socket and read goes through a spelled
/// `TESTNET_*` constant, and this is the second lock on the same door.
pub fn only_testnet() {
    assert!(
        TESTNET_WS == "wss://test.deribit.com/ws/api/v2"
            && TESTNET_REST == "https://test.deribit.com",
        "REFUSING TO RUN: the resolved endpoint is not the Deribit testnet ({TESTNET_WS}, {TESTNET_REST})"
    );
}

/// A public testnet read's `result`; panics naming the path and the venue's error.
pub fn public(path: &str, params: &[(&str, String)]) -> Value {
    only_testnet();
    let r = UreqTransport::new("deribit").public(TESTNET_REST, path, params);
    r.unwrap_or_else(|e| panic!("testnet {path}: {} ({})", e.msg, e.code))["result"].clone()
}

/// One authed testnet order socket, connected.
pub fn connect(key: &str, secret: &str) -> Result<DeribitOrderTransport, String> {
    only_testnet();
    let mut t = DeribitOrderTransport::new(TESTNET_WS, key, secret, None);
    t.connect().map_err(|e| e.0)?;
    Ok(t)
}

/// One authed testnet socket for a drill's READS and its cleanup (the clients under test own their own).
pub struct TestnetConn {
    key: String,
    secret: String,
    t: DeribitOrderTransport,
    /// An error's message also carries the venue's `data` (`"{message} {data}"`).
    error_data: bool,
    /// A read's re-dial prints one line.
    say_redial: bool,
}

impl TestnetConn {
    /// Dial and authenticate on the testnet: an error's message is the venue's `message` alone, and a
    /// re-dial prints nothing (the builders below change either).
    pub fn open(key: &str, secret: &str) -> Result<Self, String> {
        let t = connect(key, secret)?;
        let (key, secret) = (key.into(), secret.into());
        Ok(Self { key, secret, t, error_data: false, say_redial: false })
    }

    /// Builder: an error's message also carries the venue's `data` (the classes drill's FINDING lines).
    pub fn with_error_data(mut self) -> Self {
        self.error_data = true;
        self
    }

    /// Builder: a read's re-dial prints `  (socket dead on {method}: {msg}; re-dialling once)`.
    pub fn saying_redials(mut self) -> Self {
        self.say_redial = true;
        self
    }

    /// One request: the envelope's `result`, or its `error` as a [`VenueApiError`] (`code`, else `-1`).
    pub fn send(&mut self, method: &str, params: &Value) -> Result<Value, VenueApiError> {
        let env = self.t.call(method, params)?;
        match env.get("error").filter(|e| !e.is_null()) {
            Some(e) => {
                let message = e["message"].as_str().unwrap_or("");
                let msg = if self.error_data {
                    format!("{message} {}", e["data"])
                } else {
                    message.to_string()
                };
                Err(VenueApiError { code: e["code"].as_i64().unwrap_or(-1), msg })
            }
            None => Ok(env["result"].clone()),
        }
    }

    /// A read is re-dialled once on a dead socket; a write never is. The new socket keeps this one's
    /// knobs.
    pub fn rpc(&mut self, method: &str, params: &Value) -> Result<Value, VenueApiError> {
        match self.send(method, params) {
            Err(e) if method.starts_with("private/get_") && is_dead_socket_error(&e) => {
                if self.say_redial {
                    println!("  (socket dead on {method}: {}; re-dialling once)", e.msg);
                }
                let knobs = (self.error_data, self.say_redial);
                *self = Self::open(&self.key.clone(), &self.secret.clone()).expect("re-dial");
                (self.error_data, self.say_redial) = knobs;
                self.send(method, params)
            }
            r => r,
        }
    }
}
