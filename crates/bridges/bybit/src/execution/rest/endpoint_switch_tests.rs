use super::*;

/// The seed's rows dial byte-for-byte what the constants this replaced dialled: the demo pair for every
/// tier below `live`, the mainnet pair for `live`.
#[test]
fn the_seed_dials_the_hosts_the_constants_dialled() {
    for tier in [AccountTier::Demo, AccountTier::Paper] {
        let demo = seed_endpoints(tier);
        assert!(!demo.mainnet, "{tier}");
        assert_eq!(demo.rest, "https://api-demo.bybit.com");
        assert_eq!(demo.ws, "wss://stream-demo.bybit.com/v5/private");
    }
    let live = seed_endpoints(AccountTier::Live);
    assert!(live.mainnet);
    assert_eq!(live.rest, "https://api.bybit.com");
    assert_eq!(live.ws, "wss://stream.bybit.com/v5/private");
}

/// **A demo account never dials a live address on its signed path**: the demo rows are separate hosts, a
/// demo `trade`/`user` row that equals the live one is refused by the lookup, and a demo account with no
/// demo `trade` row does NOT RUN (it never falls back to the live row).
#[test]
fn the_demo_lookups_never_yield_a_live_address() {
    let live = seed_endpoints(AccountTier::Live);
    let demo = seed_endpoints(AccountTier::Demo);
    assert_ne!(demo.rest, live.rest);
    assert_ne!(demo.ws, live.ws);

    let without = |channel: UrlChannel| {
        let rows: Vec<_> = vike_venues::venue_urls::VENUE_URL_SEED
            .iter()
            .filter(|s| s.venue == VENUE && !(s.ty == UrlType::Demo && s.channel == channel))
            .map(|s| (s.venue, s.ty, s.channel, s.protocol, s.product, s.url))
            .collect();
        VenueUrls::from_rows(VENUE, rows)
    };
    for channel in [UrlChannel::Trade, UrlChannel::User] {
        let refused = endpoints(&without(channel), AccountTier::Demo).expect_err("no demo row");
        assert!(matches!(refused, UrlRefusal::Missing { .. }), "{channel}: {refused}");
        assert!(endpoints(&without(channel), AccountTier::Live).is_ok(), "live keeps its rows");
    }

    let pointed_live = VenueUrls::from_rows(
        VENUE,
        [
            (VENUE, UrlType::Live, UrlChannel::All, UrlProtocol::Rest, "", live.rest.as_str()),
            (
                VENUE,
                UrlType::Live,
                UrlChannel::All,
                UrlProtocol::Websocket,
                "",
                "wss://stream.bybit.com",
            ),
            (VENUE, UrlType::Demo, UrlChannel::Trade, UrlProtocol::Rest, "", live.rest.as_str()),
            (
                VENUE,
                UrlType::Demo,
                UrlChannel::User,
                UrlProtocol::Websocket,
                "",
                "wss://stream-demo.bybit.com",
            ),
        ],
    );
    let refused =
        endpoints(&pointed_live, AccountTier::Demo).expect_err("a live host on a demo row");
    assert!(matches!(refused, UrlRefusal::DemoIsLive { .. }), "{refused}");
}
