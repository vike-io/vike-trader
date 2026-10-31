use super::*;

/// Flag unset ⇒ demo WS + the `x-simulated-trading` header PRESENT (byte-identical to the
/// historical `UreqOkxTransport::new(true)`); flag set ⇒ mainnet WS + NO sim header. The REST
/// host is shared and never changes.
#[test]
fn switch_selects_ws_and_sim_header() {
    // demo (default)
    assert_eq!(ws_url(false), DEMO_WS);
    assert!(simulated(false), "demo ⇒ x-simulated-trading header present");
    assert_eq!(ws_url(false), "wss://wspap.okx.com:8443/ws/v5/private?brokerId=9999");
    // mainnet
    assert_eq!(ws_url(true), MAINNET_WS);
    assert!(!simulated(true), "mainnet ⇒ NO sim header");
    assert_eq!(ws_url(true), "wss://ws.okx.com:8443/ws/v5/private");
    // REST host is shared across both.
    assert_eq!(REST, "https://www.okx.com");
}

/// The algo-order socket follows the SAME verdict as the private one: demo business socket for a
/// demo start, mainnet business socket only for the live tier.
#[test]
fn the_business_socket_follows_the_same_switch() {
    assert_eq!(business_ws_url(false), DEMO_BUSINESS_WS);
    assert_eq!(business_ws_url(false), "wss://wspap.okx.com/ws/v5/business?brokerId=9999");
    assert_eq!(business_ws_url(true), MAINNET_BUSINESS_WS);
    assert_eq!(business_ws_url(true), "wss://ws.okx.com/ws/v5/business");
    // The kline lane (not `exec`-gated) dials the same host under its own name.
    assert_eq!(business_ws_url(true), crate::market_data::BUSINESS_WS);
}
