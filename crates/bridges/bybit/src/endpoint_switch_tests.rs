use super::*;

/// Flag unset ⇒ the demo host pair binance/bybit used before this existed (byte-identical);
/// flag set ⇒ the mainnet pair.
#[test]
fn endpoints_default_is_demo_set_is_mainnet() {
    assert_eq!(endpoints(false), (DEMO_REST, DEMO_WS));
    assert_eq!(endpoints(true), (MAINNET_REST, MAINNET_WS));
    // Pin the concrete demo hosts so a URL edit can't silently move the default path.
    assert_eq!(endpoints(false).0, "https://api-demo.bybit.com");
    assert_eq!(endpoints(false).1, "wss://stream-demo.bybit.com/v5/private");
    // Pin the mainnet hosts (from Bybit's v5 REST/WS docs).
    assert_eq!(endpoints(true).0, "https://api.bybit.com");
    assert_eq!(endpoints(true).1, "wss://stream.bybit.com/v5/private");
}
