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

use crate::transport::client::{DeribitTransport, DeribitUrls, is_dead_socket_error};

/// The testnet order socket the drills dial. TEST-ONLY: production dials the `demo` `trade` `ws` row
/// (`crate::transport::client::DeribitUrls`); [`only_testnet`] holds this equal to the release's seed row.
pub const TESTNET_WEBSOCKET: &str = "wss://test.deribit.com/ws/api/v2";
/// The testnet REST base the drills read. TEST-ONLY, held equal to the seed like [`TESTNET_WEBSOCKET`].
pub const TESTNET_REST: &str = "https://test.deribit.com";

/// Refuses to go anywhere but the testnet: every drill's socket and read goes through a spelled
/// `TESTNET_*` constant, and this is the second lock on the same door: the two must be the release's
/// own testnet rows (`vike_venues::venue_urls::VENUE_URL_SEED`, `demo` `trade`).
pub fn only_testnet() {
    let seed = DeribitUrls::of(&vike_venues::venue_urls::VenueUrls::seed("deribit"))
        .expect("the release's seed holds deribit's testnet rows");
    assert!(
        TESTNET_WEBSOCKET == seed.ws && TESTNET_REST == seed.rest,
        "REFUSING TO RUN: the resolved endpoint is not the Deribit testnet ({TESTNET_WEBSOCKET}, {TESTNET_REST})"
    );
}

/// The testnet addresses a drill hands the production clients ([`TESTNET_WEBSOCKET`], [`TESTNET_REST`]).
pub fn testnet_urls() -> DeribitUrls {
    only_testnet();
    DeribitUrls { ws: TESTNET_WEBSOCKET.to_string(), rest: TESTNET_REST.to_string() }
}

#[cfg(test)]
#[test]
fn the_drill_constants_are_the_seeds_testnet_rows() {
    only_testnet();
}

/// A public testnet read's `result`; panics naming the path and the venue's error.
pub fn public(path: &str, params: &[(&str, String)]) -> Value {
    only_testnet();
    let r = UreqTransport::new("deribit").public(TESTNET_REST, path, params);
    r.unwrap_or_else(|e| panic!("testnet {path}: {} ({})", e.msg, e.code))["result"].clone()
}

/// One authed testnet order socket, connected.
pub fn connect(key: &str, secret: &str) -> Result<DeribitTransport, String> {
    only_testnet();
    let mut t = DeribitTransport::new(TESTNET_WEBSOCKET, key, secret, None);
    t.connect().map_err(|e| e.0)?;
    Ok(t)
}

/// One authed testnet socket for a drill's READS and its cleanup (the clients under test own their own).
pub struct TestnetConnection {
    key: String,
    secret: String,
    t: DeribitTransport,
    /// An error's message also carries the venue's `data` (`"{message} {data}"`).
    error_data: bool,
    /// A read's re-dial prints one line.
    say_redial: bool,
}

impl TestnetConnection {
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
