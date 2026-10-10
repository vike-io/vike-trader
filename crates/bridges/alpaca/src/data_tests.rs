use super::*;
use std::assert_matches;

/// The shape guess answers in the SHARED vocabulary — the property that stopped this file
/// carrying a second, venue-local `AssetClass`.
#[test]
fn class_guess_answers_in_the_shared_taxonomy() {
    assert_eq!(class_of("BTC/USD"), AssetClass::CryptoSpot);
    assert_eq!(class_of("ETH/USDT"), AssetClass::CryptoSpot);
    assert_eq!(class_of("AAPL"), AssetClass::Equity);
    assert_eq!(class_of("SPY"), AssetClass::Equity);
}

/// The route is a function of the CLASS, and the classes Alpaca does not stream route NOWHERE
/// rather than to whichever endpoint a string test would have picked (`"BTC-PERP"` carries no
/// slash, so the old `contains('/')` test would have opened the EQUITY socket for a perp).
#[test]
fn route_is_taken_from_the_class() {
    assert_eq!(StreamRoute::for_class(AssetClass::CryptoSpot), Some(StreamRoute::Crypto));
    assert_eq!(StreamRoute::for_class(AssetClass::Equity), Some(StreamRoute::Equity));
    // an ETF is `us_equity` on this venue's own `/v1/assets`, and IEX carries its tape
    assert_eq!(StreamRoute::for_class(AssetClass::Etf), Some(StreamRoute::Equity));
    for unlisted in [
        AssetClass::CryptoPerp,
        AssetClass::CryptoFuture,
        AssetClass::Option,
        AssetClass::Fx,
        AssetClass::Future,
        AssetClass::Index,
        AssetClass::PredictionMarket,
        AssetClass::Cfd,
    ] {
        assert_eq!(StreamRoute::for_class(unlisted), None, "{unlisted:?} is not listed here");
    }
}

/// ...and the two that DO route are exactly the two this venue's catalog declares, so the
/// routing table and `catalog.rs`'s `asset_classes` cannot drift apart silently.
#[test]
fn every_class_the_catalog_declares_has_a_stream() {
    use vike_catalog::CatalogProvider;

    // Credential-less: `new` does no I/O, and `asset_classes` is the venue's static claim.
    let catalog = crate::catalog::AlpacaCatalog::new(&HashMap::new());
    for class in catalog.asset_classes().iter().copied() {
        assert!(
            StreamRoute::for_class(class).is_some(),
            "{class:?} is listed by the catalog but routes to no stream"
        );
    }
}

#[test]
fn ws_urls_by_route() {
    let h = "wss://stream.data.sandbox.alpaca.markets";
    assert_eq!(ws_url(h, StreamRoute::Crypto), format!("{h}/v1beta3/crypto/us"));
    // equity must be the free IEX feed, NOT sip (sip → 409 insufficient subscription)
    assert_eq!(ws_url(h, StreamRoute::Equity), format!("{h}/v2/iex"));
}

#[test]
fn sub_action_frames() {
    assert_eq!(
        sub_action("subscribe", Verb::Quotes, "BTC/USD"),
        serde_json::json!({"action": "subscribe", "quotes": ["BTC/USD"]})
    );
    assert_eq!(
        sub_action("unsubscribe", Verb::Trades, "AAPL"),
        serde_json::json!({"action": "unsubscribe", "trades": ["AAPL"]})
    );
    assert_eq!(
        sub_action("subscribe", Verb::Bars, "ETH/USD"),
        serde_json::json!({"action": "subscribe", "bars": ["ETH/USD"]})
    );
}

#[test]
fn auth_frame_is_oauth_key_bearer_secret() {
    assert_eq!(
        auth_frame("tok-123"),
        serde_json::json!({"action": "auth", "key": "oauth", "secret": "tok-123"})
    );
}

#[test]
fn registry_refcounts_pairs_and_connection() {
    let mut r = SubRegistry::default();
    // first subscribe to a pair → wire subscribe
    assert_eq!(r.add(Verb::Quotes, "BTC/USD".into()), WireAction::Subscribe);
    // duplicate subscribe to the SAME (verb, symbol) → no wire frame (already live)
    assert_eq!(r.add(Verb::Quotes, "BTC/USD".into()), WireAction::None);
    // a different verb on the same symbol → its own wire subscribe
    assert_eq!(r.add(Verb::Trades, "BTC/USD".into()), WireAction::Subscribe);
    // remove ONE of the two duplicate quote subs → not the last for the pair → no unsub/teardown
    assert_eq!(r.remove(Verb::Quotes, "BTC/USD"), (WireAction::None, false));
    // remove the LAST quote sub → wire unsubscribe; trades still live so no teardown
    assert_eq!(r.remove(Verb::Quotes, "BTC/USD"), (WireAction::Unsubscribe, false));
    // remove the trades sub → last pair AND last on the connection → teardown, no unsub frame
    assert_eq!(r.remove(Verb::Trades, "BTC/USD"), (WireAction::None, true));
}

/// `subscribe_book` refuses THROUGH `require_live_verb` (driven off
/// `VenueCaps.live_data.book` = false) rather than a hand-rolled message, so its refusal stays
/// pinned to the declared matrix. Same observable outcome (an `Unsupported` variant), asserted
/// here on a client built with a stub config + no-op sink (no network — the refusal short-
/// circuits before any connection).
#[test]
fn subscribe_book_is_caps_refused() {
    use crate::hosts::hosts_for;
    use vike_bridge_core::credentials::Environment;

    struct NoopSink;
    impl LiveDataSink for NoopSink {
        fn seed_bars(&self, _v: &str, _s: &str, _i: &str, _b: Vec<Bar>) {}
        fn close_bar(&self, _v: &str, _s: &str, _i: &str, _b: Bar) {}
        fn forming_bar(&self, _v: &str, _s: &str, _i: &str, _b: Bar) {}
        fn mark_tick(&self, _v: &str, _s: &str, _px: f64, _ts: i64) {}
        fn quote(&self, _v: &str, _s: &str, _q: QuoteTick) {}
        fn trade(&self, _v: &str, _s: &str, _t: TradeTick) {}
        fn book(&self, _v: &str, _s: &str, _b: std::sync::Arc<vike_model::L2Book>) {}
    }

    let env = Environment::Sim;
    let config = AlpacaConfig {
        client_id: String::new(),
        client_secret: String::new(),
        account_id: String::new(),
        env,
        hosts: hosts_for(env),
    };
    let mut client = AlpacaDataClient::new(config, Arc::new(NoopSink), || {});
    assert_matches!(client.subscribe_book("BTC/USD"), Err(LiveDataError::Unsupported(_)));
}

#[test]
fn registry_distinct_symbols_are_independent() {
    let mut r = SubRegistry::default();
    assert_eq!(r.add(Verb::Quotes, "BTC/USD".into()), WireAction::Subscribe);
    assert_eq!(r.add(Verb::Quotes, "ETH/USD".into()), WireAction::Subscribe);
    // dropping one symbol unsubscribes only it; the other keeps the connection alive
    assert_eq!(r.remove(Verb::Quotes, "BTC/USD"), (WireAction::Unsubscribe, false));
    assert_eq!(r.remove(Verb::Quotes, "ETH/USD"), (WireAction::None, true));
}

#[test]
fn registry_remove_unknown_is_safe() {
    let mut r = SubRegistry::default();
    assert_eq!(r.remove(Verb::Bars, "AAPL"), (WireAction::None, true));
}
