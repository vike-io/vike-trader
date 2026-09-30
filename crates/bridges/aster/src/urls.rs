//! Aster REST/WS base URLs, selected by `Environment`. Testnet (`Environment::Demo`) vs mainnet
//! (`Environment::Live`). Futures = fapi/fstream; spot = sapi/sstream. NO hardcoded network — the
//! adapter always resolves through here so a credentialed `Live` run hits mainnet and an
//! uncredentialed/`Demo` run hits testnet.

use vike_bridge_core::Environment;

/// The canonical venue id — homed here (the feed-plane URL module) rather than in `spot`, because
/// BOTH planes need it and `spot` is `exec`-gated (split-plane Phase 5): the keyless klines
/// fetcher (`crate::data`) keys its rate buckets by it, and every exec-plane consumer
/// (`spot`/`perp`/`recon_client`) imports it from here — no re-export shim, per the no-move-shims
/// convention (root CLAUDE.md).
pub const VENUE: &str = "aster";

/// Keyless spot `exchangeInfo` path — homed beside [`VENUE`] for the same reason (the feed-plane
/// `crate::data` composes it onto `sapi_rest`; the exec plane's startup fetch in `crate::exec`
/// spells it `urls::SPOT_PATH_EXCHANGE_INFO` directly).
pub const SPOT_PATH_EXCHANGE_INFO: &str = "/api/v3/exchangeInfo";

/// Keyless perp `exchangeInfo` path — the fapi twin of [`SPOT_PATH_EXCHANGE_INFO`].
pub const PERP_PATH_EXCHANGE_INFO: &str = "/fapi/v3/exchangeInfo";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AsterUrls {
    pub fapi_rest: &'static str,
    pub sapi_rest: &'static str,
    pub fapi_ws: &'static str,
    pub sapi_ws: &'static str,
}

const MAINNET: AsterUrls = AsterUrls {
    fapi_rest: "https://fapi.asterdex.com",
    sapi_rest: "https://sapi.asterdex.com",
    fapi_ws: "wss://fstream.asterdex.com",
    sapi_ws: "wss://sstream.asterdex.com",
};

const TESTNET: AsterUrls = AsterUrls {
    fapi_rest: "https://fapi.asterdex-testnet.com",
    sapi_rest: "https://sapi.asterdex-testnet.com",
    fapi_ws: "wss://fstream.asterdex-testnet.com",
    sapi_ws: "wss://sstream.asterdex-testnet.com",
};

/// `Live` → mainnet; everything else (`Demo`/`Sim`) → testnet.
pub fn urls_for(env: Environment) -> AsterUrls {
    match env {
        Environment::Live => MAINNET,
        _ => TESTNET,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn live_is_mainnet_demo_is_testnet() {
        assert_eq!(urls_for(Environment::Live).fapi_rest, "https://fapi.asterdex.com");
        assert_eq!(urls_for(Environment::Demo).fapi_rest, "https://fapi.asterdex-testnet.com");
        assert_eq!(urls_for(Environment::Demo).sapi_ws, "wss://sstream.asterdex-testnet.com");
        assert_ne!(urls_for(Environment::Live), urls_for(Environment::Demo));
    }
}
