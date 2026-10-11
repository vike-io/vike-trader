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
