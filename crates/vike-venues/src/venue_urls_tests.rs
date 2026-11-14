//! The seed's shape, the four-step lookup and the demo-is-live guard.

use std::collections::BTreeSet;

use super::*;

fn rows_of(
    rows: &[(&'static str, UrlType, UrlChannel, UrlProtocol, &'static str, &'static str)],
) -> VenueUrls {
    VenueUrls::from_rows("v", rows.iter().copied())
}

#[test]
fn the_seed_is_the_eighty_rows_the_plan_approved() {
    // The plan's prose said 81; its table, which this seed is row for row, holds 80. Plus ONE since:
    // dukascopy's S3 archive (`history`, product `archive`), the owner's ruling of 2026-11-13.
    assert_eq!(VENUE_URL_SEED.len(), 81);
}

#[test]
fn every_seed_key_is_unique() {
    let mut seen = BTreeSet::new();
    for s in VENUE_URL_SEED {
        assert!(
            seen.insert((s.venue, s.ty, s.channel, s.protocol, s.product)),
            "{} {} {} {} {:?} twice",
            s.venue,
            s.ty,
            s.channel,
            s.protocol,
            s.product
        );
    }
}

#[test]
fn every_seed_url_is_a_legal_base_for_its_protocol() {
    for s in VENUE_URL_SEED {
        assert_eq!(check_url(s.protocol, s.url), Ok(()), "{} {} {}", s.venue, s.ty, s.url);
    }
}

#[test]
fn every_roster_venue_has_a_live_row_and_polymarket_has_no_demo_row() {
    for venue in vike_model::VENUES {
        assert!(
            VENUE_URL_SEED.iter().any(|s| s.venue == *venue && s.ty == UrlType::Live),
            "{venue} has no live address"
        );
    }
    assert!(!VENUE_URL_SEED.iter().any(|s| s.venue == "polymarket" && s.ty == UrlType::Demo));
}

#[test]
fn the_words_round_trip_and_nothing_else_parses() {
    for t in [UrlType::Live, UrlType::Demo] {
        assert_eq!(t.as_str().parse::<UrlType>(), Ok(t));
    }
    for c in UrlChannel::ALL {
        assert_eq!(c.as_str().parse::<UrlChannel>(), Ok(c));
    }
    for p in UrlProtocol::ALL {
        assert_eq!(p.as_str().parse::<UrlProtocol>(), Ok(p));
    }
    assert!("testnet".parse::<UrlType>().is_err(), "a testnet IS demo: no third type");
    assert!("paper".parse::<UrlType>().is_err());
    assert!("Live".parse::<UrlType>().is_err(), "exact and lower-case");
    assert_eq!(UrlType::of_tier(AccountTier::Paper), UrlType::Demo);
    assert_eq!(UrlType::of_tier(AccountTier::Live), UrlType::Live);
}

#[test]
fn the_shape_check_refuses_what_it_says() {
    assert!(check_url(Rest, "https://api.x.com").is_ok());
    assert!(check_url(Stream, "http://x.com/base").is_ok());
    assert!(check_url(Websocket, "wss://x.com:8443").is_ok());
    assert!(check_url(Socket, "127.0.0.1:4102").is_ok());
    for (p, bad) in [
        (Rest, "wss://x.com"),
        (Websocket, "https://x.com"),
        (Rest, "https://"),
        (Rest, "https://x.com/"),
        (Websocket, "wss://x.com?brokerId=9999"),
        (Rest, "https://x.com#a"),
        (Rest, "https://x .com"),
        (Socket, "tcp://h:1"),
        (Socket, "host"),
        (Socket, "host:0"),
        (Socket, ":5035"),
        (Rest, ""),
    ] {
        assert!(check_url(p, bad).is_err(), "{p} {bad:?} must be refused");
    }
}

/// The two seed lookups a test compares against: one type's rows alone, and one exact row's value.
#[test]
fn the_seed_lookups_answer_one_type_and_one_exact_row() {
    let live = VenueUrls::seed_typed("bybit", Live);
    assert_eq!(live.url(Live, Trade, Rest, ""), Ok("https://api.bybit.com"));
    assert!(live.url(Demo, Trade, Rest, "").is_err(), "no demo row, and trade never falls back");
    assert_eq!(
        seed_url("ibkr", Demo, Trade, Socket, "gateway"),
        Some("127.0.0.1:4102"),
        "the exact key"
    );
    assert_eq!(seed_url("ibkr", Demo, Trade, Socket, ""), None, "no product, no row");
    assert_eq!(seed_url("bybit", Live, Data, Rest, ""), None, "no `all` step");
}

/// A socket row splits at its LAST `:`, and what the write refuses the split refuses: one rule.
#[test]
fn a_socket_row_is_read_by_the_rule_its_write_is_judged_by() {
    let at = |host: &str, port| Ok(UrlSocket { host: host.to_string(), port });
    assert_eq!(UrlSocket::parse("127.0.0.1:4102"), at("127.0.0.1", 4102));
    assert_eq!(UrlSocket::parse("demo.ctraderapi.com:5035"), at("demo.ctraderapi.com", 5035));
    assert_eq!(UrlSocket::parse("[::1]:5000"), at("[::1]", 5000));
    for bad in ["tcp://h:1", "host", "host:0", ":5035", "h:65536", "h:x", ""] {
        assert!(UrlSocket::parse(bad).is_err(), "{bad:?} must be refused");
        assert!(check_url(Socket, bad).is_err(), "{bad:?} must be refused at the write");
    }
    for s in VENUE_URL_SEED.iter().filter(|s| s.protocol == Socket) {
        assert!(UrlSocket::parse(s.url).is_ok(), "{} {}", s.venue, s.url);
    }
}

#[test]
fn step_one_takes_the_exact_row_and_step_two_the_all_row() {
    let urls = rows_of(&[
        ("v", Live, All, Rest, "", "https://all"),
        ("v", Live, Trade, Rest, "", "https://trade"),
        ("other", Live, Data, Rest, "", "https://other"),
    ]);
    assert_eq!(urls.url(Live, Trade, Rest, ""), Ok("https://trade"));
    assert_eq!(urls.url(Live, Data, Rest, ""), Ok("https://all"));
    assert!(urls.url(Live, Data, Rest, "spot").is_err(), "the product is part of the key");
    assert!(urls.url(Live, Data, Websocket, "").is_err(), "so is the protocol");
}

#[test]
fn only_demo_data_and_history_fall_back_to_live() {
    let urls = rows_of(&[("v", Live, All, Rest, "", "https://live")]);
    assert_eq!(urls.url(Demo, Data, Rest, ""), Ok("https://live"));
    assert_eq!(urls.url(Demo, History, Rest, ""), Ok("https://live"));
    for channel in [Trade, User, Login, All] {
        assert_eq!(
            urls.url(Demo, channel, Rest, ""),
            Err(UrlRefusal::Missing {
                venue: "v".into(),
                ty: Demo,
                channel,
                protocol: Rest,
                product: String::new(),
            }),
            "{channel} never falls back from demo to live"
        );
    }
}

#[test]
fn a_demo_data_row_wins_over_the_live_fallback() {
    let seed = VenueUrls::seed("hyperliquid");
    assert_eq!(seed.url(Demo, Data, Rest, ""), Ok("https://api.hyperliquid-testnet.xyz"));
    assert_eq!(seed.url(Demo, History, Rest, ""), Ok("https://api.hyperliquid.xyz"));
    assert_eq!(seed.url(Demo, Trade, Rest, ""), Ok("https://api.hyperliquid-testnet.xyz"));
}

#[test]
fn a_missing_row_is_refused_in_words_that_name_it() {
    let words = VenueUrls::seed("polymarket").url(Demo, Trade, Rest, "").unwrap_err().to_string();
    assert!(words.contains("polymarket") && words.contains("demo trade rest"), "{words}");
    assert!(words.contains("config url"), "the cure is named: {words}");
}

#[test]
fn the_guard_refuses_a_demo_trade_or_user_address_equal_to_live() {
    let urls = rows_of(&[
        ("v", Live, All, Rest, "", "https://live"),
        ("v", Live, User, Websocket, "", "wss://live"),
        ("v", Demo, Trade, Rest, "", "https://live"),
        ("v", Demo, User, Websocket, "", "wss://live"),
        ("v", Demo, Data, Rest, "", "https://live"),
    ]);
    assert!(matches!(urls.url(Demo, Trade, Rest, ""), Err(UrlRefusal::DemoIsLive { .. })));
    assert!(matches!(urls.url(Demo, User, Websocket, ""), Err(UrlRefusal::DemoIsLive { .. })));
    assert_eq!(urls.url(Demo, Data, Rest, ""), Ok("https://live"), "public data may be live");
    assert_eq!(urls.url(Live, Trade, Rest, ""), Ok("https://live"));
}

#[test]
fn the_seed_passes_the_guard_at_every_demo_key() {
    for s in VENUE_URL_SEED.iter().filter(|s| s.ty == Demo) {
        let urls = VenueUrls::seed(s.venue);
        assert_eq!(
            urls.url(Demo, s.channel, s.protocol, s.product),
            Ok(s.url),
            "{} demo {} {}",
            s.venue,
            s.channel,
            s.protocol
        );
    }
}

/// A demo row that shares a live row's address BY DESIGN (okx's REST host, ibkr's Client Portal
/// gateway, ctrader's login host, fxcm's Hosts.jsp) says so in its note, so `config url ls` shows why.
#[test]
fn every_demo_row_sharing_a_live_address_says_why_in_its_note() {
    for d in VENUE_URL_SEED.iter().filter(|s| s.ty == Demo) {
        let shared = VENUE_URL_SEED.iter().any(|l| {
            l.ty == Live && l.venue == d.venue && l.protocol == d.protocol && l.url == d.url
        });
        assert!(
            !shared || !d.note.is_empty(),
            "{} demo {} {} {:?}: shares live's address and has no note",
            d.venue,
            d.channel,
            d.protocol,
            d.product
        );
    }
}

/// Each allow-list row is LIVE: without it the seed's own demo row would be refused, so no row
/// of [`DEMO_SHARES_LIVE`] is dead and none is missing.
#[test]
fn every_allow_list_row_is_a_seed_demo_address_equal_to_live() {
    for (venue, channel, protocol, product) in DEMO_SHARES_LIVE {
        let urls = VenueUrls::seed(venue);
        let demo = urls.url(Demo, *channel, *protocol, product).expect("the seed holds it");
        let live = urls.url(Live, *channel, *protocol, product).expect("the seed holds it");
        assert_eq!(demo, live, "{venue} {channel} {protocol} {product:?}");
    }
}

/// Every [`KEYLESS_FEED_DIALS`] key is a seed row's, and a `trade`/`user`/`login` one: a stale key
/// would keep a row in use that nothing dials.
#[test]
fn every_keyless_dial_is_a_private_seed_row() {
    for &(venue, channel, protocol, product) in KEYLESS_FEED_DIALS {
        let seeded = VENUE_URL_SEED.iter().any(|s| {
            (s.venue, s.channel, s.protocol, s.product) == (venue, channel, protocol, product)
        });
        assert!(seeded, "{venue} {channel} {protocol} {product:?} is no seed row");
        assert!(matches!(channel, Trade | User | Login), "{venue} {channel} needs no list");
    }
}

/// the CI box on 2026-11-09 (decision 0265's verdict 8): aster LIVE active, ibkr DEMO active on the socket
/// backend. The rows it warned on that no account dials are unused; the rest are judged.
#[test]
fn a_row_no_active_account_dials_is_unused() {
    use vike_model::AccountTier::{Demo as DemoTier, Live as LiveTier, Paper};
    let active = |rows: &[(&str, AccountTier)]| -> Vec<(String, AccountTier)> {
        rows.iter().map(|&(venue, tier)| (venue.to_string(), tier)).collect()
    };
    let socket = |_: AccountTier, _: &str| None;
    let the CI box = active(&[("aster", LiveTier), ("ibkr", DemoTier)]);
    let unused = |venue, ty, channel, protocol, product| {
        unused_row(venue, ty, channel, protocol, product, &the CI box, socket)
    };
    let none_at_demo = Some("no active aster demo account".to_string());
    assert_eq!(unused("aster", Demo, User, Websocket, "spot"), none_at_demo);
    assert!(unused("aster", Demo, Trade, Rest, "usdm").is_some());
    assert_eq!(
        unused("aster", Live, All, Websocket, "spot"),
        None,
        "an `all` row feeds the data lane"
    );
    assert_eq!(unused("dukascopy", Live, History, Rest, ""), None, "history needs no account");
    assert_eq!(unused("ibkr", Demo, Trade, Socket, "gateway"), None);
    let on_socket = Some("the ibkr demo backend is socket".to_string());
    assert_eq!(unused("ibkr", Demo, Trade, Rest, "cpapi"), on_socket);
    let at_demo = Some("the ibkr session is demo".to_string());
    assert_eq!(unused("ibkr", Live, Trade, Socket, "gateway"), at_demo);
    assert_eq!(unused("ibkr", Live, Trade, Rest, "cpapi"), at_demo);
    assert_eq!(unused("polymarket", Live, Trade, Rest, ""), None, "the keyless tape dials it");
    assert_eq!(unused("polymarket", Live, User, Websocket, ""), None, "the keyless tape dials it");
    assert!(unused("polymarket", Live, Trade, Rest, "relayer").is_some());

    // A paper row may hold the demo login a credentialed feed signs in with.
    let paper = active(&[("ctrader", Paper)]);
    assert_eq!(unused_row("ctrader", Demo, Trade, Socket, "", &paper, socket), None);
    // The backend is the tier's `backend` row: a cpapi session dials cpapi and not the socket.
    let live = active(&[("ibkr", LiveTier)]);
    let cpapi = |tier: AccountTier, field: &str| {
        (tier == LiveTier && field == "backend").then_some("cpapi")
    };
    assert_eq!(unused_row("ibkr", Live, Trade, Rest, "cpapi", &live, cpapi), None);
    assert!(unused_row("ibkr", Live, Trade, Socket, "gateway", &live, cpapi).is_some());
    // No active non-paper ibkr row starts no gateway; a backend word no parser takes is judged.
    assert!(unused_row("ibkr", Demo, Trade, Socket, "gateway", &paper, socket).is_some());
    let grpc = |_: AccountTier, _: &str| Some("grpc");
    assert_eq!(unused_row("ibkr", Live, Trade, Socket, "gateway", &live, grpc), None, "judged");
}
