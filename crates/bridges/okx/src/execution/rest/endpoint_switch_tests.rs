use super::*;
use vike_model::AccountTier;
use vike_venues::venue_urls::VENUE_URL_SEED;

/// [`OkxNet::resolve`] over the release's seed rows: what a fresh box dials.
fn seeded(tier: AccountTier) -> OkxNet {
    OkxNet::resolve(&VenueUrls::seed("okx"), tier).expect("the seed holds okx's rows")
}

/// The DIALLED URLs are byte-for-byte the ones the code held before the addresses became rows: the
/// row is the base, the code appends the path and, on the demo type ONLY, `?brokerId=9999`. The
/// `x-simulated-trading` header follows the TYPE (present on demo and paper, absent on live), and the
/// REST host is one for both.
#[test]
fn the_seed_dials_exactly_the_urls_the_code_held() {
    for demo in [seeded(AccountTier::Demo), seeded(AccountTier::Paper)] {
        assert!(!demo.mainnet && simulated(demo.mainnet), "demo ⇒ the sim header");
        assert_eq!(demo.rest, "https://www.okx.com");
        assert_eq!(demo.private_websocket, "wss://wspap.okx.com:8443/ws/v5/private?brokerId=9999");
        assert_eq!(demo.business_websocket, "wss://wspap.okx.com/ws/v5/business?brokerId=9999");
    }
    let live = seeded(AccountTier::Live);
    assert!(live.mainnet && !simulated(live.mainnet), "live ⇒ NO sim header");
    assert_eq!(live.rest, "https://www.okx.com");
    assert_eq!(live.private_websocket, "wss://ws.okx.com:8443/ws/v5/private");
    assert_eq!(live.business_websocket, "wss://ws.okx.com/ws/v5/business");
    // The tier-less candle lane dials the live business socket under its own lookup (`data`).
    let feed = crate::feed::tick::feed_websocket(
        &VenueUrls::seed("okx"),
        PRODUCT_BUSINESS,
        PATH_WEBSOCKET_BUSINESS,
    );
    assert_eq!(feed.as_deref(), Ok(live.business_websocket.as_str()));
}

/// **A demo account reaches no live address but the one REST host the venue shares by design**: no
/// demo socket is a live socket or a live row, and the shared REST host is the one key
/// `vike_venues::venue_urls::DEMO_SHARES_LIVE` names for okx.
#[test]
fn the_demo_lookups_never_yield_a_live_address() {
    let (demo, live) = (seeded(AccountTier::Demo), seeded(AccountTier::Live));
    let live_rows: Vec<&str> = VENUE_URL_SEED
        .iter()
        .filter(|r| r.venue == "okx" && r.ty == UrlType::Live)
        .map(|r| r.url)
        .collect();
    for socket in [&demo.private_websocket, &demo.business_websocket] {
        assert!(![&live.private_websocket, &live.business_websocket].contains(&socket), "{socket}");
        assert!(live_rows.iter().all(|row| !socket.starts_with(&format!("{row}/"))), "{socket}");
    }
    assert_eq!(demo.rest, live.rest, "one REST host; the header is the network");
    assert!(
        vike_venues::venue_urls::DEMO_SHARES_LIVE.contains(&(
            "okx",
            UrlChannel::Trade,
            UrlProtocol::Rest,
            ""
        )),
        "the shared REST host is allow-listed, not an accident"
    );
}

/// No row, nothing dialled: a box whose table holds no okx address refuses at every type, naming the
/// row, and a demo `user` row that a hand edit pointed at the live socket is refused by the lookup's
/// guard (trade and user never fall back from demo to live).
#[test]
fn a_missing_or_live_demo_row_is_refused() {
    for tier in [AccountTier::Demo, AccountTier::Live] {
        let why = OkxNet::resolve(&VenueUrls::default(), tier).unwrap_err().to_string();
        assert!(why.contains("no address row"), "{tier}: {why}");
    }
    let rows = VENUE_URL_SEED.iter().filter(|r| r.venue == "okx").map(|r| {
        let url = if r.ty == UrlType::Demo && r.channel == UrlChannel::User && r.product.is_empty()
        {
            "wss://ws.okx.com:8443"
        } else {
            r.url
        };
        (r.venue, r.ty, r.channel, r.protocol, r.product, url)
    });
    let pointed_live = VenueUrls::from_rows("okx", rows);
    let why = OkxNet::resolve(&pointed_live, AccountTier::Demo).unwrap_err();
    assert!(matches!(why, UrlRefusal::DemoIsLive { .. }), "{why}");
    assert!(
        OkxNet::resolve(&pointed_live, AccountTier::Live).is_ok(),
        "the live account is unaffected"
    );
}
