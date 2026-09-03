//! Alpaca host table — sandbox vs live endpoints for the four planes (authx / broker / data
//! REST / data WS). Verified live 2026-07-14: the OAuth Bearer authorizes both the broker host
//! and the data host; data is NOT reachable via the broker host.

use vike_bridge_core::credentials::Environment;

/// The four Alpaca base URLs for an environment.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AlpacaEnv {
    pub authx: &'static str,
    pub broker: &'static str,
    pub data_rest: &'static str,
    pub data_ws: &'static str,
}

/// Sandbox for `Sim`/`Demo`, live for `Live`.
pub fn hosts_for(env: Environment) -> AlpacaEnv {
    match env {
        Environment::Live => AlpacaEnv {
            authx: "https://authx.alpaca.markets",
            broker: "https://broker-api.alpaca.markets",
            data_rest: "https://data.alpaca.markets",
            data_ws: "wss://stream.data.alpaca.markets",
        },
        _ => AlpacaEnv {
            authx: "https://authx.sandbox.alpaca.markets",
            broker: "https://broker-api.sandbox.alpaca.markets",
            data_rest: "https://data.sandbox.alpaca.markets",
            data_ws: "wss://stream.data.sandbox.alpaca.markets",
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn sandbox_vs_live() {
        assert!(hosts_for(Environment::Demo).broker.contains("sandbox"));
        assert!(hosts_for(Environment::Sim).authx.contains("sandbox"));
        let live = hosts_for(Environment::Live);
        assert_eq!(live.broker, "https://broker-api.alpaca.markets");
        assert!(!live.data_ws.contains("sandbox"));
    }
}
