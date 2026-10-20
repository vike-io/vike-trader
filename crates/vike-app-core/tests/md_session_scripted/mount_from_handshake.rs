//! The feed set is the SERVER's word: the probe that learns it, the mount that follows it, a switch,
//! a server that advertises nothing, and a series that was asked for before any of it was known.
//!
//! `LOCAL_FEED_VENUES` mounted one feed per hand-written slug at launch, so every suite here used to
//! be able to subscribe before the session had an address. The feeds are now mounted from
//! `Welcome.features` by `MdSession::sync_mount`, and the reconciler learns those by a handshake-only
//! PROBE when it has an address and nothing is wanted — the one connection that is not a stream, and
//! the one the fake's `accepted` counter must count exactly once per server.

use std::collections::HashSet;

use vike_app_core::ui::feed_lifecycle::{
    FeedMap, FeedRetries, FeedSlots, TradeDepthSubs, ensure_depth, ensure_trade_feed_on,
    retry_unroutable,
};
use vike_data::SubscriptionId;

use super::*;

/// The probe LEARNS what the server advertises and SUBSCRIBES nothing: it is the one connection, it
/// sends no frame after the handshake, it is not repeated, and it wakes the frame thread (the mount
/// happens there, and egui repaints only when woken).
#[test]
fn a_probe_learns_the_advertisement_and_subscribes_nothing() {
    let fake =
        spawn_fake(md_features(), Box::new(|specs| ScriptedAnswer::accept_all(specs, Vec::new())));
    let mut h = Harness::new();
    assert!(h.mount.session.caps().is_none(), "nothing has been heard from any server yet");
    h.attach(&fake);

    let caps = h.mount.session.caps().expect("the probe cached what the server advertised");
    assert!(caps.market_data, "the plane was advertised");
    assert_eq!(caps.venues, vec!["binance", "bybit", "okx", "aster", "hyperliquid", "polymarket"]);
    // Long enough for a probe that repeats: nothing wanted and the advertisement known means the
    // reconciler parks, it does not ask again.
    thread::sleep(Duration::from_millis(300));
    assert_eq!(
        fake.accepted.load(Ordering::SeqCst),
        1,
        "the probe is the one connection a session with nothing wanted opens, and it is not repeated"
    );
    assert_eq!(
        fake.seen.recv_timeout(SETTLE).expect("the fake reports what the probe sent"),
        vec!["Hello".to_string()],
        "the probe asked the server for something after the handshake"
    );
    assert!(h.wakes.load(Ordering::SeqCst) > 0, "the probe must wake the frame thread to mount");
}

/// Every advertised ROSTER venue gets exactly one feed and one non-empty status line, and nothing
/// else does: a repeated slug is mounted once, a slug the roster does not know (no capability row to
/// route it by) is not mounted, and deribit — which the six-venue fixture never advertised — is
/// mounted because the server said so, not because a list in this crate knew it.
#[test]
fn every_advertised_roster_venue_gets_one_feed_and_one_line() {
    let fake = spawn_fake(
        features_for(&["binance", "deribit", "binance", "no-such-venue"]),
        Box::new(|specs| ScriptedAnswer::accept_all(specs, Vec::new())),
    );
    let mut h = Harness::new();
    h.attach(&fake);
    h.sync(); // a second pass over the same advertisement changes nothing

    assert_eq!(h.mounted(), vec!["binance", "deribit"]);
    let mut lines: Vec<&str> = h.mount.feed_statuses.keys().map(String::as_str).collect();
    lines.sort_unstable();
    assert_eq!(lines, vec!["binance", "deribit"], "one line per mounted venue");
    for (venue, line) in &h.mount.feed_statuses {
        assert!(
            !line.lock().unwrap().is_empty(),
            "{venue}: a venue with no status line renders `Unknown` in the Connections tool"
        );
    }
}

/// A server switch converges on the NEW server's venues: the one it newly advertises is mounted, the
/// idle one it dropped is unmounted with its line, and a venue both advertise keeps its feed and its
/// status handle untouched (the Connections tool holds that `Arc`).
#[test]
fn a_server_switch_mounts_the_new_servers_venues_and_drops_the_old_ones() {
    let a = spawn_fake(
        features_for(&["binance", "okx"]),
        Box::new(|specs| ScriptedAnswer::accept_all(specs, Vec::new())),
    );
    let b = spawn_fake(
        features_for(&["binance", "polymarket"]),
        Box::new(|specs| ScriptedAnswer::accept_all(specs, Vec::new())),
    );
    let mut h = Harness::new();
    h.attach(&a);
    assert_eq!(h.mounted(), vec!["binance", "okx"]);
    let binance_line = h.mount.feed_statuses["binance"].clone();

    h.mount.session.set_addr(Some(&b.addr.to_string()));
    until("B's advertisement to replace A's", || {
        h.sync();
        h.mounted() == vec!["binance", "polymarket"]
    });

    let mut lines: Vec<&str> = h.mount.feed_statuses.keys().map(String::as_str).collect();
    lines.sort_unstable();
    assert_eq!(lines, vec!["binance", "polymarket"], "the dropped venue kept its status line");
    assert!(
        Arc::ptr_eq(&binance_line, &h.mount.feed_statuses["binance"]),
        "a venue both servers advertise was torn down and rebuilt"
    );
}

/// **A feed some key still holds outlives the advertisement that named it** — `App`'s ledgers
/// (`subs`, `trade_depth`, `poly_subs`) unsubscribe through `App::feeds`, so dropping the feed under
/// a live key would strand its release. New keys on it are refused locally in the meantime, and the
/// feed goes with the last holder.
///
/// B's advertisement is learned here by the DIAL the held key forces, not by a probe: a dial refreshes
/// the cached advertisement exactly as a probe does.
#[test]
fn a_feed_with_a_live_key_outlives_the_advertisement_that_named_it() {
    let a = spawn_fake(
        features_for(&["binance", "okx"]),
        Box::new(|specs| ScriptedAnswer::accept_all(specs, Vec::new())),
    );
    let b = spawn_fake(
        features_for(&["binance"]),
        Box::new(|specs| ScriptedAnswer::accept_all(specs, Vec::new())),
    );
    let mut h = Harness::new();
    h.attach(&a);
    let id = h.feed("okx").subscribe_depth("BTC-USDT-SWAP").expect("an advertised venue");

    h.mount.session.set_addr(Some(&b.addr.to_string()));
    until("B's advertisement to land", || {
        h.mount.session.caps().is_some_and(|c| c.venues == ["binance"])
    });
    h.sync();
    assert_eq!(
        h.mounted(),
        vec!["binance", "okx"],
        "the feed holding a live key was dropped with the advertisement that named it"
    );
    assert_matches!(
        h.feed("okx").subscribe_depth("ETH-USDT-SWAP"),
        Err(LiveDataError::Unsupported(_)),
        "a venue the server no longer advertises must refuse NEW keys locally"
    );

    h.feed("okx").unsubscribe(id);
    h.sync();
    assert_eq!(h.mounted(), vec!["binance"], "the last holder released the feed, so it goes");
    assert!(!h.mount.feed_statuses.contains_key("okx"), "...and so does its status line");
}

/// A plane-less server, and a plane with no venue this desktop knows, both mount NOTHING and both
/// say why — in the link line, because no venue line exists to carry it — as an ERROR (never a
/// healthy-looking `idle`), after exactly one connection that sent nothing past the handshake.
#[test]
fn a_server_that_advertises_nothing_mounts_nothing_and_says_so() {
    use vike_model::feed_status::{ConnectionState, parse_feed_status};

    // (what the server advertises, a phrase the link line must carry)
    let cases: [(Vec<String>, &str); 3] = [
        (vec!["load_bars".to_string()], "live-feeds"),
        (vec!["load_bars".to_string(), FEATURE_MARKET_DATA.to_string()], "advertises no venue"),
        (features_for(&["no-such-venue"]), "advertises no venue"),
    ];
    for (features, phrase) in cases {
        let fake =
            spawn_fake(features, Box::new(|specs| ScriptedAnswer::accept_all(specs, Vec::new())));
        let mut h = Harness::new();
        h.attach(&fake);

        assert!(h.mount.feeds.is_empty(), "{phrase}: a feed was mounted for nothing");
        assert!(h.mount.feed_statuses.is_empty(), "{phrase}: a status line was mounted");
        until("the link line to say why nothing was mounted", || {
            h.mount.session.link_status().contains(phrase)
        });
        let link = h.mount.session.link_status();
        assert_eq!(
            parse_feed_status(&link),
            ConnectionState::Error,
            "a server with nothing to mount must not read as healthy: {link:?}"
        );
        thread::sleep(Duration::from_millis(200));
        assert_eq!(
            fake.accepted.load(Ordering::SeqCst),
            1,
            "{phrase}: the probe is not repeated against a server that answered"
        );
    }
}

/// **An unanswered switch HOLDS the mounted feeds.** Right after an address change the cached
/// advertisement is cleared (it described the old server), and `None` means "unknown", not "empty":
/// clearing on it would unmount every idle feed across each backend switch and have every window's
/// `ensure_*` log a missing venue for the gap. The set converges the moment the new server answers.
#[test]
fn feeds_are_held_while_a_new_server_has_not_answered() {
    let a = spawn_fake(
        features_for(&["binance", "okx"]),
        Box::new(|specs| ScriptedAnswer::accept_all(specs, Vec::new())),
    );
    let c = spawn_fake(
        features_for(&["bybit"]),
        Box::new(|specs| ScriptedAnswer::accept_all(specs, Vec::new())),
    );
    // An address that refuses connections: bind to learn a free port, then close it.
    let dead = TcpListener::bind("127.0.0.1:0").expect("bind").local_addr().expect("local addr");
    let mut h = Harness::new();
    h.attach(&a);

    h.mount.session.set_addr(Some(&dead.to_string()));
    until("A's advertisement to be forgotten", || h.mount.session.caps().is_none());
    h.sync();
    assert_eq!(
        h.mounted(),
        vec!["binance", "okx"],
        "an unanswered switch unmounted the old server's idle feeds"
    );

    h.mount.session.set_addr(Some(&c.addr.to_string()));
    until("C's advertisement to replace A's", || {
        h.sync();
        h.mounted() == vec!["bybit"]
    });
}

/// The `FeedSlots` bundle over a `FeedMap` and the shell's four ledgers, owned in one place.
struct Slots {
    subs: HashMap<String, SubscriptionId>,
    spawned: HashSet<String>,
    unroutable: HashSet<String>,
    retries: FeedRetries,
}

impl Slots {
    fn new() -> Slots {
        Slots {
            subs: HashMap::new(),
            spawned: HashSet::new(),
            unroutable: HashSet::new(),
            retries: FeedRetries::default(),
        }
    }

    fn of<'a>(&'a mut self, feeds: &'a mut FeedMap) -> FeedSlots<'a> {
        FeedSlots {
            feeds,
            subs: &mut self.subs,
            spawned: &mut self.spawned,
            unroutable: &mut self.unroutable,
            retries: &mut self.retries,
        }
    }
}

/// **A series ensured BEFORE the handshake is subscribed when the feed arrives** — end to end, over
/// the real mount. A restored workspace asks for a tick/volume chart's trade tape in `App::new`, long
/// before any datahub has answered; its primary series is not re-asked per frame, so
/// `retry_unroutable` is the only thing that ever re-asks. Proven by the tape ARRIVING.
#[test]
fn a_trade_tape_asked_for_before_the_handshake_arrives_once_the_feed_does() {
    let fake = spawn_fake(
        md_features(),
        Box::new(|specs| {
            ScriptedAnswer::accept_all(
                specs,
                vec![MdFrame::Trades {
                    venue: "binance".to_string(),
                    symbol: "BTCUSDT".to_string(),
                    ticks: vec![stripped_tick(1_000, 100.0, 1.0)],
                    seq: 1,
                }],
            )
        }),
    );
    let mut h = Harness::new();
    let mut slots = Slots::new();

    ensure_trade_feed_on(&mut slots.of(&mut h.mount.feeds), "binance", "BTCUSDT");
    assert!(slots.unroutable.contains("BTCUSDT@trades"), "the miss is recorded, not silent");
    assert!(slots.spawned.contains("BTCUSDT@trades"), "...and the series stays wanted");
    retry_unroutable(&mut slots.of(&mut h.mount.feeds));
    assert!(slots.subs.is_empty(), "nothing can be subscribed while no feed exists");
    assert!(slots.unroutable.contains("BTCUSDT@trades"), "...and the record must survive the pass");

    h.attach(&fake);
    retry_unroutable(&mut slots.of(&mut h.mount.feeds));
    assert!(slots.subs.contains_key("BTCUSDT@trades"), "the late feed was never asked");
    assert!(slots.unroutable.is_empty(), "a routable key leaves the unroutable record");
    until("the late feed's tape to arrive", || !h.trades.drain("binance", "BTCUSDT").is_empty());
}

/// The Trade window's depth stream is re-asked every frame, so it needs no pass of its own: the
/// missing-venue record is always due, and the first call after the mount subscribes.
#[test]
fn a_depth_stream_asked_for_before_the_handshake_is_subscribed_by_the_next_frame() {
    let fake = spawn_fake(
        md_features(),
        Box::new(|specs| {
            ScriptedAnswer::accept_all(specs, vec![book_frame("binance", "BTCUSDT", 1)])
        }),
    );
    let mut h = Harness::new();
    let mut retries = FeedRetries::default();
    let mut trade_depth: HashMap<(String, String), TradeDepthSubs> = HashMap::new();

    ensure_depth(&mut h.mount.feeds, &mut trade_depth, &mut retries, "binance", "BTCUSDT");
    assert!(trade_depth.is_empty(), "no feed, so nothing was subscribed");

    h.attach(&fake);
    ensure_depth(&mut h.mount.feeds, &mut trade_depth, &mut retries, "binance", "BTCUSDT");
    assert!(
        trade_depth.contains_key(&("binance".to_string(), "BTCUSDT".to_string())),
        "the next frame's ensure_depth did not pick the late feed up"
    );
    until("the ladder to arrive", || h.books.get("binance", "BTCUSDT").is_some());
}
