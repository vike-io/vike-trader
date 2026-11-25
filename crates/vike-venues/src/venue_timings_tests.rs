use super::*;
use std::collections::BTreeMap;

/// Every roster venue paired with the row(s) it declares. The completeness test below asserts this
/// covers [`vike_model::VENUES`] exactly.
///
/// `#[rustfmt::skip]`: a `just new-venue` marker at the TAIL of a bracketed literal is re-indented
/// by rustfmt once a row ending in a trailing `//` comment is generated above it, which defeats
/// `--remove`. Gated by `crates/vike-ops/tests/venues/new_venue_gate/rustfmt_rule.rs`'s
/// `a_trailing_comment_marker_is_rustfmt_skipped_unless_a_recognised_sibling_follows`.
#[rustfmt::skip]
const MATRIX: &[(&str, VenueRateLimitsMarket, VenueTimings)] = &[
    ("binance", VenueRateLimitsMarket::Spot, BINANCE_SPOT),
    ("binance", VenueRateLimitsMarket::Perp, BINANCE_PERP),
    ("aster", VenueRateLimitsMarket::Spot, ASTER_SPOT),
    ("aster", VenueRateLimitsMarket::Perp, ASTER_PERP),
    ("bybit", VenueRateLimitsMarket::Spot, BYBIT),
    ("okx", VenueRateLimitsMarket::Spot, OKX),
    ("deribit", VenueRateLimitsMarket::Spot, DERIBIT),
    ("polymarket", VenueRateLimitsMarket::Spot, POLYMARKET),
    ("hyperliquid", VenueRateLimitsMarket::Spot, HYPERLIQUID),
    ("oanda", VenueRateLimitsMarket::Spot, OANDA),
    ("ig", VenueRateLimitsMarket::Spot, IG),
    ("fxcm", VenueRateLimitsMarket::Spot, FXCM),
    ("dukascopy", VenueRateLimitsMarket::Spot, DUKASCOPY),
    ("ibkr", VenueRateLimitsMarket::Spot, IBKR),
    ("ctrader", VenueRateLimitsMarket::Spot, CTRADER),
    ("alpaca", VenueRateLimitsMarket::Spot, ALPACA),
    // vike:new-venue:row ("{venue}", VenueRateLimitsMarket::Spot, {VENUE}), // TODO(new-venue: {venue}): a market-SPLIT venue declares two rows
];

/// The rows no registry answers, read by name (IBKR's Client Portal product): every walk over the declared rows reads
/// them beside [`MATRIX`], and a pin names them by this name.
const BY_NAME: &[(&str, VenueRateLimitsMarket, VenueTimings)] =
    &[("ibkr cpapi", VenueRateLimitsMarket::Spot, IBKR_CPAPI)];

/// Every declared row: the registry's and the ones read by name.
fn every_row() -> impl Iterator<Item = &'static (&'static str, VenueRateLimitsMarket, VenueTimings)>
{
    MATRIX.iter().chain(BY_NAME)
}

/// The row a pin names: a name of [`BY_NAME`], else the registry's answer.
fn row_named(name: &str, market: VenueRateLimitsMarket) -> VenueTimings {
    BY_NAME.iter().find(|(n, ..)| *n == name).map_or_else(|| timings_for(name, market), |r| r.2)
}

// ---------------------------------------------------------------------------------------------
// THE MATRIX PIN. Written from the bridges' constants as they stood BEFORE this table existed, and
// before any row did: this is how "no value changed" is checked rather than asserted. Every number
// below is in MILLISECONDS and is a literal from the adapter it is cited against; none is computed
// from a row. A row that drifts from its bridge fails here, and a retune is a deliberate edit to
// BOTH this pin and the row, in a diff that says so.
//
// The citations name the FILE (it outlives the swap of the bridge onto this table) and, in
// parentheses, the constant the number was read from at the time.
// ---------------------------------------------------------------------------------------------

/// `(venue, market, dial, handshake, response, keepalive, session_renew, idle, reconnect)`, all in
/// milliseconds, `reconnect` as `(initial, max)`.
type LanePin = (
    &'static str,
    VenueRateLimitsMarket,
    Option<u64>,
    Option<u64>,
    Option<u64>,
    Option<u64>,
    Option<u64>,
    Option<u64>,
    Option<(u64, u64)>,
);

fn millis_of(d: Duration) -> u64 {
    u64::try_from(d.as_millis()).expect("a timing fits in u64 milliseconds")
}

fn ms(t: Option<VenueTimingsValue>) -> Option<u64> {
    t.map(|t| millis_of(t.value))
}

fn assert_lane_is_pinned(who: &str, lane: VenueTimingsLane, pin: &LanePin) {
    let (_, _, dial, handshake, response, keepalive, renew, idle, reconnect) = *pin;
    assert_eq!(ms(lane.dial_timeout), dial, "{who} dial_timeout");
    assert_eq!(ms(lane.handshake_timeout), handshake, "{who} handshake_timeout");
    assert_eq!(ms(lane.response_timeout), response, "{who} response_timeout");
    assert_eq!(ms(lane.keepalive_every), keepalive, "{who} keepalive_every");
    assert_eq!(ms(lane.session_renew_every), renew, "{who} session_renew_every");
    assert_eq!(ms(lane.idle_threshold), idle, "{who} idle_threshold");
    assert_eq!(
        lane.reconnect.map(|r| (millis_of(r.initial), millis_of(r.max))),
        reconnect,
        "{who} reconnect"
    );
}

/// The EXEC lane of every venue that has one, pinned verbatim.
#[test]
fn execution_lanes_are_pinned() {
    #[rustfmt::skip]
    let rows: &[LanePin] = &[
        // venue          market        dial        handshake     response    keepalive     session_renew  idle           reconnect (initial, max)
        // `crates/bridges/binance/src/execution/user_data/spot.rs`: HANDSHAKE_TIMEOUT 10 s, IDLE_THRESHOLD 60 s (3 x the 20 s server ping), no app
        // ping, no dial bound (`tungstenite::connect`), the pump's 30 s max_backoff literal over its 1 s first backoff.
        ("binance",     VenueRateLimitsMarket::Spot, None,       Some(10_000), None,       None,         None,          Some(60_000),  Some((1_000, 30_000))),
        // `crates/bridges/binance/src/family/execution/user_data.rs`: KEEPALIVE_EVERY 1800 s (a REST PUT), MAX_BACKOFF 30 s; the
        // `perp_user_data.rs` IDLE_THRESHOLD 720 s; no ack, so no handshake.
        ("binance",     VenueRateLimitsMarket::Perp, None,       None,         None,       None,         Some(1_800_000), Some(720_000), Some((1_000, 30_000))),
        // `crates/bridges/aster/src/execution/user_data/spot.rs` and `perp_user_data.rs`: the same listenKey pump, IDLE_THRESHOLD 1200 s in both, a frame on the socket every 60 s.
        ("aster",       VenueRateLimitsMarket::Spot, None,       None,         None,       Some(60_000), Some(1_800_000), Some(1_200_000), Some((1_000, 30_000))),
        ("aster",       VenueRateLimitsMarket::Perp, None,       None,         None,       Some(60_000), Some(1_800_000), Some(1_200_000), Some((1_000, 30_000))),
        // `crates/bridges/bybit/src/execution/user_data.rs`: HANDSHAKE_TIMEOUT 10 s, PING_EVERY 20 s, IDLE_THRESHOLD 60 s.
        ("bybit",       VenueRateLimitsMarket::Spot, None,       Some(10_000), None,       Some(20_000), None,          Some(60_000),  Some((1_000, 30_000))),
        // `crates/bridges/okx/src/execution/user_data.rs`: HANDSHAKE_TIMEOUT 10 s, PING_EVERY 15 s, IDLE_THRESHOLD 60 s.
        ("okx",         VenueRateLimitsMarket::Spot, None,       Some(10_000), None,       Some(15_000), None,          Some(60_000),  Some((1_000, 30_000))),
        // `crates/bridges/deribit/src/execution/user_data.rs`: HANDSHAKE_TIMEOUT 10 s, PING_EVERY 20 s, IDLE_THRESHOLD 60 s; and its ORDER socket,
        // `crates/bridges/deribit/src/transport/client.rs`: `request_timeout` 10 s, the wait for each JSON-RPC reply (found late, pinned with its row), and the dial bound 10 s of both sockets (`user_data.rs`'s `DIAL_TIMEOUT`, `transport.rs`'s `dial_timeout`).
        ("deribit",     VenueRateLimitsMarket::Spot, Some(10_000), Some(10_000), Some(10_000), Some(20_000), None,      Some(60_000),  Some((1_000, 30_000))),
        // `crates/bridges/polymarket/src/execution/user_data/mod.rs`: PING_EVERY 10 s, IDLE_THRESHOLD 60 s, no ack, `connect_websocket(.., Some(CONNECT_10S))`.
        ("polymarket",  VenueRateLimitsMarket::Spot, None,       None,         None,       Some(10_000), None,          Some(60_000),  Some((1_000, 30_000))),
        // `crates/bridges/hyperliquid/src/execution/user_data.rs`: WEBSOCKET_PING_SECS 30, IDLE_THRESHOLD 4 x that, MAX_BACKOFF 30 s, no ack.
        ("hyperliquid", VenueRateLimitsMarket::Spot, None,       None,         None,       Some(30_000), None,          Some(120_000), Some((1_000, 30_000))),
        // `crates/bridges/oanda/src/execution/user_data.rs`: CONNECT_TIMEOUT 10 s (dial AND `timeout_recv_response`), RECV_IDLE_TIMEOUT 20 s,
        // RECONNECT_BACKOFF a fixed 1 s.
        ("oanda",       VenueRateLimitsMarket::Spot, Some(10_000), None,       Some(10_000), None,       None,          Some(20_000),  Some((1_000, 1_000))),
        // `crates/bridges/ig/src/execution/user_data.rs`: `timeout_connect` 10 s (B-13), `timeout_per_read` 30 s; `stream/retry.rs`:
        // RECONNECT_DELAY 1 s doubling to MAX_RECONNECT_BACKOFF 60 s.
        ("ig",          VenueRateLimitsMarket::Spot, Some(10_000), None,       None,       None,         None,          Some(30_000),  Some((1_000, 60_000))),
        // `crates/bridges/ctrader/src/transport/connection/mod.rs`: HANDSHAKE_TIMEOUT 15 s is a wait for EACH response, HEARTBEAT_INTERVAL 10 s,
        // READ_IDLE_AFTER 30 s (3 x HEARTBEAT_INTERVAL, the actor's stall watchdog, a local constant before the row);
        // `conn/handshake.rs`: DIAL_WINDOW 10 s (B-13); `conn/session.rs`: 1 s (B-13) doubling to 5 s.
        ("ctrader",     VenueRateLimitsMarket::Spot, Some(10_000), None,       Some(15_000), Some(10_000), None,        Some(30_000),  Some((1_000, 5_000))),
        // `crates/bridges/alpaca/src/execution/user_data.rs`: CONNECT_TIMEOUT 5 s, `timeout_per_read` 30 s, the failed-open retry a fixed 2 s.
        ("alpaca",      VenueRateLimitsMarket::Spot, Some(5_000), None,        None,       None,         None,          Some(30_000),  Some((2_000, 2_000))),
        // `crates/bridges/vike-ibkr/src/gateway_watch.rs`: the gatewayhub's rebuild of the socket adapter, its first wait 2 s (read
        // from the Client Portal row's redial until the products split) doubling to MAX_WAIT 60 s; the `ibapi` client keeps the rest.
        ("ibkr",        VenueRateLimitsMarket::Spot, None,       None,         None,       None,         None,          None,          Some((2_000, 60_000))),
        // `crates/bridges/vike-ibkr/src/transport/cpapi/mod.rs`: the 30 s REST tickle, the failed-dial wait a fixed 2 s; the 30 s `tic`
        // (TIC_EVERY) and the 60 s idle limit (IDLE_LIMIT) are NEW, from IBKR's documentation (connection plan Q3d), not read from the bridge.
        ("ibkr cpapi",  VenueRateLimitsMarket::Spot, None,       None,         None,       Some(30_000), Some(30_000),  Some(60_000),  Some((2_000, 2_000))),
        // `crates/bridges/dukascopy/src/execution/mod.rs`: READY_TIMEOUT 300 s; RESTART 5 s doubling to 60 s (a sidecar that died
        // after `ready` is restarted by the client's supervisor); LIVENESS, NEW from the FX open of 2026-11-22 (not read from the
        // bridge): the 60 s `recon` probe and the 210 s idle limit (3 x the 69.15 s longest healthy silence).
        ("dukascopy",   VenueRateLimitsMarket::Spot, None,       Some(300_000), None,      Some(60_000), None,          Some(210_000), Some((5_000, 60_000))),
        // fxcm's SDK owns its transport: its exec lane holds only the gatewayhub rebuild's probation (`session_probes_are_pinned`).
        ("fxcm",        VenueRateLimitsMarket::Spot, None,       None,         None,       None,         None,          None,          None),
    ];
    for row in rows {
        let (venue, market) = (row.0, row.1);
        let lane = row_named(venue, market)
            .exec
            .unwrap_or_else(|| panic!("{venue} {market:?}: no exec lane declared"));
        assert_lane_is_pinned(&format!("{venue} {market:?} exec"), lane, row);
    }
    // ...and no OTHER roster venue declares one.
    let pinned: Vec<&str> = rows.iter().map(|r| r.0).collect();
    for &v in vike_model::VENUES {
        assert_eq!(
            timings_for(v, VenueRateLimitsMarket::Spot).exec.is_some(),
            pinned.contains(&v),
            "{v}: exec lane declared iff pinned"
        );
    }
}

/// The MARKET lane of every venue that declares one, pinned verbatim: oanda's, IBKR's Client Portal's and the nine
/// other pump venues' (binance, aster, bybit, okx, deribit, polymarket, hyperliquid, ig, alpaca), every one read by
/// the shared market pump (`crates/vike-bridge-core/src/websocket_session/spec.rs`'s `row`). The nine's numbers are
/// the pump's old literals, unchanged by the move. The subscribe-acknowledgement watchdog is pinned beside them
/// (`subscribe_acknowledgement_watchdogs_are_pinned`).
#[test]
fn market_lanes_are_pinned() {
    #[rustfmt::skip]
    let rows: &[LanePin] = &[
        // venue     market        dial          handshake     response      keepalive  session_renew  idle          reconnect (initial, max)
        // `crates/bridges/oanda/src/feed/mod.rs`: CONNECT_TIMEOUT 10 s (dial AND `timeout_recv_response`), RECV_IDLE_TIMEOUT 20 s,
        // RECONNECT_BACKOFF a fixed 3 s (the shared pumps' classic).
        ("oanda",  VenueRateLimitsMarket::Spot, Some(10_000), None,         Some(10_000), None,      None,          Some(20_000), Some((3_000, 3_000))),
        // `crates/bridges/vike-ibkr/src/feed/cpapi/lane.rs`: TICKLE_EVERY 30 s, and its pump row's fixed 2 s (the old loop's
        // REDIAL) and 60 s idle limit (new with the pump, IBKR's documented 10 s heartbeat, the exec lane's value).
        ("ibkr cpapi", VenueRateLimitsMarket::Spot, None,     None,         None,         None,      Some(30_000),  Some(60_000), Some((2_000, 2_000))),
        // `crates/bridges/ig/src/feed/mod.rs`: HANDSHAKE_BUDGET 15 s for create_session -> CONOK; the pump row's 60 s idle
        // watchdog and its FIXED_3S redial (its 10 s ack watchdog is the SUBOK wait, pinned apart).
        ("ig",     VenueRateLimitsMarket::Spot, None,         Some(15_000), None,         None,      None,          Some(60_000), Some((3_000, 3_000))),
        // The pump lanes (read by `spec.rs`'s `row`): no dial (CONNECT_10S, the mechanism's), no read poll (READ_POLL), a FIXED_3S redial
        // but polymarket's exponential 500 ms -> 30 s; binance's 540 s and aster's 60 s / 180 s from the idle-limit audit of
        // 2026-11-22, bybit's 20 s and okx's 15 s pings (C6), deribit's 20 s / 60 s pair, hyperliquid's 30 s / 60 s, polymarket's
        // 10 s / 30 s, alpaca's 180 s.
        ("binance",     VenueRateLimitsMarket::Spot, None, None, None, None,         None, Some(540_000), Some((3_000, 3_000))),
        ("binance",     VenueRateLimitsMarket::Perp, None, None, None, None,         None, Some(540_000), Some((3_000, 3_000))),
        ("aster",       VenueRateLimitsMarket::Spot, None, None, None, Some(60_000), None, Some(180_000), Some((3_000, 3_000))),
        ("aster",       VenueRateLimitsMarket::Perp, None, None, None, Some(60_000), None, Some(180_000), Some((3_000, 3_000))),
        ("bybit",       VenueRateLimitsMarket::Spot, None, None, None, Some(20_000), None, Some(60_000),  Some((3_000, 3_000))),
        ("okx",         VenueRateLimitsMarket::Spot, None, None, None, Some(15_000), None, Some(60_000),  Some((3_000, 3_000))),
        ("deribit",     VenueRateLimitsMarket::Spot, None, None, None, Some(20_000), None, Some(60_000),  Some((3_000, 3_000))),
        ("polymarket",  VenueRateLimitsMarket::Spot, None, None, None, Some(10_000), None, Some(30_000),  Some((500, 30_000))),
        ("hyperliquid", VenueRateLimitsMarket::Spot, None, None, None, Some(30_000), None, Some(60_000),  Some((3_000, 3_000))),
        ("alpaca",      VenueRateLimitsMarket::Spot, None, None, None, None,         None, Some(180_000), Some((3_000, 3_000))),
    ];
    for row in rows {
        let (venue, market) = (row.0, row.1);
        let lane = row_named(venue, market)
            .market
            .unwrap_or_else(|| panic!("{venue} {market:?}: no market lane declared"));
        assert_lane_is_pinned(&format!("{venue} {market:?} market"), lane, row);
    }
    // ...and every OTHER roster venue declares no market lane here: ctrader's and ibkr's socket ride their exec
    // session, fxcm and dukascopy have none.
    let pinned: Vec<&str> = rows.iter().map(|r| r.0).collect();
    for &v in vike_model::VENUES {
        assert_eq!(
            timings_for(v, VenueRateLimitsMarket::Spot).market.is_some(),
            pinned.contains(&v),
            "{v}: market lane declared iff pinned"
        );
    }
}

/// The market pump's subscribe-acknowledgement watchdogs, pinned exhaustively and verbatim: the pump rows' 10 s
/// `ack_timeout` of the four venues that acknowledge a subscribe (bybit, okx, deribit, ig's `SUBOK`), and no exec lane
/// declares one (an exec lane's acknowledgements are its handshake).
#[test]
fn subscribe_acknowledgement_watchdogs_are_pinned() {
    let mut declared = Vec::new();
    for (venue, market, row) in every_row() {
        for (lane_name, lane) in [("exec", row.exec), ("market", row.market)] {
            if let Some(t) = lane.and_then(|l| l.subscribe_acknowledgement_timeout) {
                declared.push((format!("{venue} {market:?} {lane_name}"), millis_of(t.value)));
            }
        }
    }
    assert_eq!(
        declared,
        [
            ("bybit Spot market".to_string(), 10_000),
            ("okx Spot market".to_string(), 10_000),
            ("deribit Spot market".to_string(), 10_000),
            ("ig Spot market".to_string(), 10_000),
        ],
        "a subscribe-acknowledgement watchdog appeared, vanished or changed"
    );
}

/// One depth-pin row: `(venue, market, idle, freshness, reseed, backoff)`, milliseconds.
type DepthPinRow = (&'static str, VenueRateLimitsMarket, u64, u64, Option<u64>, u64);

/// The DEPTH driver's four knobs, pinned verbatim: `(venue, market, idle, freshness, reseed,
/// backoff)` in milliseconds.
#[test]
fn depth_blocks_are_pinned() {
    #[rustfmt::skip]
    let rows: &[DepthPinRow] = &[
        // `crates/bridges/binance/src/family/feed/mod.rs`: DEPTH_IDLE_THRESHOLD 30 s, DEPTH_FRESHNESS_THRESHOLD 120 s,
        // DEPTH_RESEED_INTERVAL 300 s, DEPTH_BACKOFF 3 s. Aster runs the SAME `depth_main`.
        ("binance",  VenueRateLimitsMarket::Spot, 30_000, 120_000, Some(300_000), 3_000),
        ("binance",  VenueRateLimitsMarket::Perp, 30_000, 120_000, Some(300_000), 3_000),
        ("aster",    VenueRateLimitsMarket::Spot, 30_000, 120_000, Some(300_000), 3_000),
        ("aster",    VenueRateLimitsMarket::Perp, 30_000, 120_000, Some(300_000), 3_000),
        // `crates/bridges/bybit/src/feed/depth.rs`: the same four constants.
        ("bybit",    VenueRateLimitsMarket::Spot, 30_000, 120_000, Some(300_000), 3_000),
        // `crates/bridges/okx/src/feed/depth.rs`: the same four constants.
        ("okx",      VenueRateLimitsMarket::Spot, 30_000, 120_000, Some(300_000), 3_000),
        // `crates/bridges/deribit/src/feed/mod.rs`: DEPTH_FRESHNESS_THRESHOLD 120 s, DEPTH_RESEED_INTERVAL 300 s, DEPTH_BACKOFF 3 s;
        // its idle threshold is the 60 s its market lane declares (the pump's `knobs.idle_threshold`).
        ("deribit",  VenueRateLimitsMarket::Spot, 60_000, 120_000, Some(300_000), 3_000),
    ];
    for (venue, market, idle, freshness, reseed, backoff) in rows {
        let depth = timings_for(venue, *market)
            .depth
            .unwrap_or_else(|| panic!("{venue} {market:?}: no depth block declared"));
        assert_eq!(millis_of(depth.idle_threshold()), *idle, "{venue} {market:?} idle");
        assert_eq!(millis_of(depth.freshness_threshold()), *freshness, "{venue} {market:?} fresh");
        assert_eq!(ms(depth.reseed_interval), *reseed, "{venue} {market:?} reseed");
        assert_eq!(millis_of(depth.backoff()), *backoff, "{venue} {market:?} backoff");
    }
    // ...and no OTHER roster venue runs the depth driver.
    let pinned: Vec<&str> = rows.iter().map(|r| r.0).collect();
    for &v in vike_model::VENUES {
        assert_eq!(
            timings_for(v, VenueRateLimitsMarket::Spot).depth.is_some(),
            pinned.contains(&v),
            "{v}: depth block declared iff pinned"
        );
    }
}

// ---------------------------------------------------------------------------------------------
// The playbook's structural tests.
// ---------------------------------------------------------------------------------------------

/// Completeness: every registry venue resolves to its declared const, so the pinned tables above
/// are authoritative.
#[test]
fn registry_maps_every_known_venue() {
    for (venue, market, want) in MATRIX {
        assert_eq!(timings_for(venue, *market), *want, "{venue} {market:?}");
    }
}

/// Completeness vs the canonical roster: every [`vike_model::VENUES`] entry has a NAMED declared
/// row, and the registry serves exactly it for BOTH markets. Adding a venue to the roster fails
/// here until its rows exist — the whole point of the playbook.
#[test]
fn every_roster_venue_has_a_declared_row() {
    for &v in vike_model::VENUES {
        let rows: Vec<(&str, VenueRateLimitsMarket, VenueTimings)> =
            MATRIX.iter().copied().filter(|(rv, _, _)| *rv == v).collect();
        assert!(!rows.is_empty(), "no VenueTimings row declared for roster venue {v}");
        for (_, market, want) in rows.iter().copied() {
            assert_eq!(timings_for(v, market), want, "{v}: registry must serve its row");
        }
        // A market-UNIFORM venue's single row answers both markets — a venue is never
        // half-declared, whatever `MATRIX` happens to list.
        if rows.len() == 1 {
            let want = rows[0].2;
            for market in [VenueRateLimitsMarket::Spot, VenueRateLimitsMarket::Perp] {
                assert_eq!(timings_for(v, market), want, "{v} {market:?}");
            }
        } else {
            assert_eq!(rows.len(), 2, "{v}: a split venue declares exactly spot + perp");
        }
    }
    assert_eq!(MATRIX.len(), vike_model::VENUES.len() + 2, "14 venues, 2 of them split");
}

/// The registry's `_` fallback arm is unreachable for a roster venue: every `NOT_DECLARED` row a
/// roster venue gets is a NAMED const. Without this, deleting a venue's arm would look identical to
/// declaring it empty.
#[test]
fn undeclared_roster_venues_are_named_rows_not_fall_through() {
    #[rustfmt::skip]
    let named: &[(&str, VenueTimings)] = &[
        // vike:new-venue:row ("{venue}", {VENUE}), // TODO(new-venue: {venue}): DELETE this row once the venue declares real timings
    ];
    for (venue, row) in named {
        assert_eq!(*row, VenueTimings::NOT_DECLARED, "{venue} declares nothing today");
        for market in [VenueRateLimitsMarket::Spot, VenueRateLimitsMarket::Perp] {
            assert_eq!(timings_for(venue, market), *row, "{venue} {market:?}");
        }
    }
    // Every roster venue is either in `named` or carries real timings — no third state.
    for &v in vike_model::VENUES {
        let declared = timings_for(v, VenueRateLimitsMarket::Spot) != VenueTimings::NOT_DECLARED;
        let is_named = named.iter().any(|(nv, _)| *nv == v);
        assert!(declared ^ is_named, "{v} must be exactly one of declared / explicitly empty");
    }
}

/// Only binance is market-split in substance: its spot socket is a WS-API subscribe/ack session and
/// its perp socket a listenKey stream. Aster has a row per market (its rate limits split) but both
/// run ONE listenKey pump, so its rows are equal; every other venue serves ONE row for both
/// markets. Left implicit this silently rots when a venue gains a second host.
#[test]
fn market_split_venues_are_exactly_binance() {
    for &v in vike_model::VENUES {
        let split = timings_for(v, VenueRateLimitsMarket::Spot)
            != timings_for(v, VenueRateLimitsMarket::Perp);
        assert_eq!(split, v == "binance", "{v}: market-split is {split}, expected only binance");
    }
    assert_ne!(BINANCE_SPOT.exec, BINANCE_PERP.exec);
    assert_eq!(ASTER_SPOT, ASTER_PERP, "aster's two markets run ONE listenKey pump");
}

/// Every declared timing of a row as `(label, value, why)`, a reconnect policy counting once
/// (by its ceiling). Each struct is destructured EXHAUSTIVELY, so a field added to a lane or the
/// depth block stops compiling here until the checks below have learned it.
fn declared(row: &VenueTimings) -> Vec<(String, Duration, &'static str)> {
    let VenueTimings { exec, market, depth } = *row;
    let mut out = Vec::new();
    for (lane_name, lane) in [("exec", exec), ("market", market)] {
        let Some(VenueTimingsLane {
            dial_timeout,
            handshake_timeout,
            subscribe_acknowledgement_timeout,
            response_timeout,
            keepalive_every,
            session_renew_every,
            idle_threshold,
            auth_probe_every,
            lost_after,
            probation,
            reconnect,
            history_lag,
            session_lifetime,
            submit,
        }) = lane
        else {
            continue;
        };
        if let Some(s) = submit {
            out.push((format!("{lane_name}.submit"), s.answered_by(), s.why));
        }
        for (field, timing) in [
            ("dial_timeout", dial_timeout),
            ("handshake_timeout", handshake_timeout),
            ("subscribe_acknowledgement_timeout", subscribe_acknowledgement_timeout),
            ("response_timeout", response_timeout),
            ("keepalive_every", keepalive_every),
            ("session_renew_every", session_renew_every),
            ("idle_threshold", idle_threshold),
            ("auth_probe_every", auth_probe_every),
            ("lost_after", lost_after),
            ("probation", probation),
            ("history_lag", history_lag),
            ("session_lifetime", session_lifetime),
        ] {
            if let Some(t) = timing {
                out.push((format!("{lane_name}.{field}"), t.value, t.why));
            }
        }
        if let Some(r) = reconnect {
            out.push((format!("{lane_name}.reconnect"), r.max, r.why));
        }
    }
    if let Some(VenueTimingsDepth {
        idle_threshold,
        freshness_threshold,
        reseed_interval,
        backoff,
    }) = depth
    {
        for (field, timing) in [
            ("depth.idle_threshold", Some(idle_threshold)),
            ("depth.freshness_threshold", Some(freshness_threshold)),
            ("depth.reseed_interval", reseed_interval),
            ("depth.backoff", Some(backoff)),
        ] {
            if let Some(t) = timing {
                out.push((field.to_string(), t.value, t.why));
            }
        }
    }
    out
}

/// A timing is a duration PLUS the reason for it, and a bare number is how two equal values come to
/// be read as one fact. Every declared value must carry a `why` that is more than a token.
#[test]
fn every_timing_cites_its_reason() {
    let mut checked = 0;
    for (venue, market, row) in every_row() {
        for (label, _value, why) in declared(row) {
            assert!(
                why.len() > 30,
                "{venue} {market:?} {label}: a `why` of {why:?} is not a reason (30 chars at least)"
            );
            assert_eq!(why.trim(), why, "{venue} {market:?} {label}: stray whitespace in `why`");
            checked += 1;
        }
    }
    assert!(checked >= 60, "only {checked} timings were examined: the walk has gone inert");
}

/// Equal numbers do NOT mean equal reasons: no two DIFFERENT venues share a `why`. A reason copied
/// from a sibling is not this venue's reason, and the only way a copy stays honest is by being
/// re-read against the sibling's source, which this test forces a human to do.
#[test]
fn distinct_venues_do_not_share_a_reason() {
    let mut owner: BTreeMap<&'static str, &str> = BTreeMap::new();
    for (venue, _market, row) in every_row() {
        for (label, _value, why) in declared(row) {
            if let Some(first) = owner.insert(why, *venue) {
                assert_eq!(
                    first, *venue,
                    "{venue} {label} reuses {first}'s reason ({why:?}); write this venue's own"
                );
            }
        }
    }
}

/// An idle threshold under two keepalive periods trips on one late pong and tears down a healthy
/// socket forever. Enforced at COMPILE time by the `const _: ()` block; asserted here over the
/// whole roster so a NEW row cannot land without it.
#[test]
fn idle_is_at_least_twice_the_keepalive() {
    let mut both = 0;
    for (venue, market, row) in every_row() {
        for lane in [row.exec, row.market].into_iter().flatten() {
            if let (Some(idle), Some(keepalive)) = (lane.idle_threshold, lane.keepalive_every) {
                assert!(
                    idle.value >= keepalive.value * 2,
                    "{venue} {market:?}: idle {:?} is under two keepalive periods ({:?})",
                    idle.value,
                    keepalive.value
                );
                both += 1;
            }
        }
    }
    assert!(both >= 5, "only {both} lanes set both: bybit, okx, deribit, polymarket, hyperliquid");
}

/// No duration is zero, no reconnect starts at zero or runs backwards, and the whole roster passes
/// the same check the compile-time block asserts.
#[test]
fn no_row_has_a_zero_or_an_inverted_timing() {
    for (venue, market, row) in every_row() {
        assert!(row.is_sane(), "{venue} {market:?} is not sane");
        for (label, value, _why) in declared(row) {
            assert!(!value.is_zero(), "{venue} {market:?} {label} is zero");
        }
    }
    assert!(VenueTimings::NOT_DECLARED.is_sane());
}

/// The fixed-wait reconnects, pinned exhaustively: `initial == max` is a claim ("this lane waits the
/// same every time"), so a NEW fixed lane has to be added here deliberately.
#[test]
fn fixed_reconnects_are_pinned() {
    let mut fixed = Vec::new();
    for (venue, _market, row) in every_row() {
        for (lane_name, lane) in [("exec", row.exec), ("market", row.market)] {
            if lane.and_then(|l| l.reconnect).is_some_and(|r| r.is_fixed()) {
                fixed.push(format!("{venue}/{lane_name}"));
            }
        }
    }
    fixed.sort();
    assert_eq!(
        fixed,
        [
            "alpaca/exec",
            "alpaca/market",
            "aster/market",
            "aster/market",
            "binance/market",
            "binance/market",
            "bybit/market",
            "deribit/market",
            "hyperliquid/market",
            "ibkr cpapi/exec",
            "ibkr cpapi/market",
            "ig/market",
            "oanda/exec",
            "oanda/market",
            "okx/market",
        ],
        "a fixed-wait reconnect appeared or vanished"
    );
}

/// The session probes, pinned exhaustively and verbatim (the owner put them in the table on 2026-11-11), so a
/// venue declaring one is a deliberate edit here. The numbers are the bridges' literals from before the rows:
/// `crates/bridges/ctrader/src/transport/connection/mod.rs` (`AUTH_PROBE_EVERY` 60 s) and
/// `crates/bridges/vike-ibkr/src/transport/link_watch.rs` (`LOST_AFTER` 300 s, `PROBATION` 600 s); fxcm's 30 s
/// probation is the owner's ruling of 2026-11-22 (`crates/bridges/fxcm/src/account/venue_link.rs`'s `PROBATION`).
#[test]
fn session_probes_are_pinned() {
    let mut declared = Vec::new();
    for (venue, market, row) in every_row() {
        for (lane_name, lane) in [("exec", row.exec), ("market", row.market)] {
            let Some(lane) = lane else { continue };
            for (field, timing) in [
                ("auth_probe_every", lane.auth_probe_every),
                ("lost_after", lane.lost_after),
                ("probation", lane.probation),
            ] {
                if let Some(t) = timing {
                    declared.push((
                        format!("{venue} {market:?} {lane_name}.{field}"),
                        millis_of(t.value),
                    ));
                }
            }
        }
    }
    assert_eq!(
        declared,
        [
            ("fxcm Spot exec.probation".to_string(), 30_000),
            ("ibkr Spot exec.lost_after".to_string(), 300_000),
            ("ibkr Spot exec.probation".to_string(), 600_000),
            ("ctrader Spot exec.auth_probe_every".to_string(), 60_000),
        ],
        "a session probe appeared, vanished or changed"
    );
}

/// The history lags, pinned exhaustively and verbatim (decision 0160's amendment of 2026-11-21): a lag
/// is a measured venue fact that makes the reconnect resync replay a SECOND time, so a venue declaring
/// one is a deliberate edit here, and its bridge's spawn site must read the row
/// (`vike_bridge_core::websocket_session::user::WebsocketSessionUserResyncSpec`'s `history_lag`).
#[test]
fn history_lags_are_pinned() {
    let mut declared = Vec::new();
    for (venue, market, row) in every_row() {
        for (lane_name, lane) in [("exec", row.exec), ("market", row.market)] {
            if let Some(lag) = lane.and_then(|l| l.history_lag) {
                declared.push((format!("{venue} {market:?} {lane_name}"), millis_of(lag.value)));
            }
        }
    }
    assert_eq!(
        declared,
        [("bybit Spot exec".to_string(), 30_000)],
        "a history lag appeared, vanished or changed"
    );
}

/// The session lifetimes, pinned exhaustively and verbatim. Each is a limit the venue DOCUMENTS (the connection
/// survey of 2026-11-21), declared only: nothing reads it and nothing rotates a session ahead of it, so a venue
/// declaring one is a deliberate edit here.
#[test]
fn session_lifetimes_are_pinned() {
    let mut declared = Vec::new();
    for (venue, market, row) in every_row() {
        for (lane_name, lane) in [("exec", row.exec), ("market", row.market)] {
            if let Some(life) = lane.and_then(|l| l.session_lifetime) {
                declared.push((format!("{venue} {market:?} {lane_name}"), millis_of(life.value)));
            }
        }
    }
    assert_eq!(
        declared,
        [
            ("binance Spot exec".to_string(), 86_400_000),
            ("binance Perp exec".to_string(), 86_400_000),
            ("aster Spot exec".to_string(), 86_400_000),
            ("aster Perp exec".to_string(), 86_400_000),
            ("ibkr cpapi Spot exec".to_string(), 86_400_000),
        ],
        "a session lifetime appeared, vanished or changed"
    );
}

/// An unknown venue is fail-closed and equals `Default`.
#[test]
fn an_unknown_venue_is_not_declared() {
    assert_eq!(timings_for("nasdaq", VenueRateLimitsMarket::Spot), VenueTimings::NOT_DECLARED);
    assert_eq!(VenueTimings::default(), VenueTimings::NOT_DECLARED);
}

/// The sanity check is not vacuous: each way a lane can be wrong is refused.
#[test]
fn the_sanity_check_refuses_a_zero_an_inverted_reconnect_and_a_tight_idle() {
    let why = "a reason that is long enough to count";
    assert!(VenueTimingsLane::NONE.is_sane());

    let zero = VenueTimingsLane {
        dial_timeout: Some(VenueTimingsValue::secs(0, why)),
        ..VenueTimingsLane::NONE
    };
    assert!(!zero.is_sane(), "a zero dial bound");

    let inverted = VenueTimingsLane {
        reconnect: Some(VenueTimingsReconnect::new(
            Duration::from_secs(5),
            Duration::from_secs(1),
            why,
        )),
        ..VenueTimingsLane::NONE
    };
    assert!(!inverted.is_sane(), "a reconnect that starts above its ceiling");

    let hot = VenueTimingsLane {
        reconnect: Some(VenueTimingsReconnect::fixed(Duration::ZERO, why)),
        ..VenueTimingsLane::NONE
    };
    assert!(!hot.is_sane(), "a zero reconnect wait is a hot loop");

    let tight = VenueTimingsLane {
        keepalive_every: Some(VenueTimingsValue::secs(20, why)),
        idle_threshold: Some(VenueTimingsValue::secs(39, why)),
        ..VenueTimingsLane::NONE
    };
    assert!(!tight.is_sane(), "an idle threshold under two keepalive periods");

    let exact = VenueTimingsLane {
        keepalive_every: Some(VenueTimingsValue::secs(20, why)),
        idle_threshold: Some(VenueTimingsValue::secs(40, why)),
        ..VenueTimingsLane::NONE
    };
    assert!(exact.is_sane(), "exactly two keepalive periods is allowed");

    let forgiven_early = VenueTimingsLane {
        lost_after: Some(VenueTimingsValue::secs(300, why)),
        probation: Some(VenueTimingsValue::secs(300, why)),
        ..VenueTimingsLane::NONE
    };
    assert!(!forgiven_early.is_sane(), "a probation that does not outlast lost_after");

    let zero_probe = VenueTimingsLane {
        auth_probe_every: Some(VenueTimingsValue::secs(0, why)),
        ..VenueTimingsLane::NONE
    };
    assert!(!zero_probe.is_sane(), "a zero auth-probe cadence");

    let zero_lag = VenueTimingsLane {
        history_lag: Some(VenueTimingsValue::secs(0, why)),
        ..VenueTimingsLane::NONE
    };
    assert!(!zero_lag.is_sane(), "a zero history lag replays twice at once");

    let depth = VenueTimingsDepth {
        idle_threshold: VenueTimingsValue::secs(30, why),
        freshness_threshold: VenueTimingsValue::secs(120, why),
        reseed_interval: None,
        backoff: VenueTimingsValue::secs(0, why),
    };
    assert!(!depth.is_sane(), "a zero depth backoff");
    assert!(!VenueTimings { depth: Some(depth), ..VenueTimings::NOT_DECLARED }.is_sane());
}

/// The constructors build what they say, and a fixed wait is exactly `initial == max`.
#[test]
fn the_constructors_build_what_they_say() {
    let t = VenueTimingsValue::millis(1500, "milliseconds are kept to the millisecond");
    assert_eq!(t.value, Duration::from_millis(1500));
    assert_eq!(
        VenueTimingsValue::secs(2, "whole seconds are kept whole, too").value,
        Duration::from_secs(2)
    );

    let growing =
        VenueTimingsReconnect::new(Duration::from_millis(200), Duration::from_secs(5), "growing");
    assert!(!growing.is_fixed());
    assert!(growing.is_sane());
    let fixed = VenueTimingsReconnect::fixed(Duration::from_secs(3), "fixed");
    assert!(fixed.is_fixed());
    assert_eq!((fixed.initial, fixed.max), (Duration::from_secs(3), Duration::from_secs(3)));
}

// ---------------------------------------------------------------------------------------------
// An absent facility REFUSES to answer rather than returning a number a caller could mistake for a
// venue fact. In a `const` initializer — how every bridge consumes this — each of these is a
// COMPILE error; here they are reached through ordinary runtime calls, so a test can observe them.
// ---------------------------------------------------------------------------------------------

#[test]
#[should_panic(expected = "declares no exec lane timings")]
fn a_venue_without_an_execution_lane_refuses_one() {
    let _ = VenueTimings::NOT_DECLARED.exec();
}

#[test]
#[should_panic(expected = "declares no market lane timings here")]
fn a_venue_without_a_market_lane_here_refuses_one() {
    let _ = FXCM.market();
}

#[test]
#[should_panic(expected = "declares no subscribe_acknowledgement_timeout")]
fn a_lane_without_a_subscribe_acknowledgement_watchdog_refuses_one() {
    let _ = HYPERLIQUID.market().subscribe_acknowledgement_timeout();
}

#[test]
#[should_panic(expected = "declares no depth driver timings")]
fn a_venue_without_a_depth_driver_refuses_one() {
    let _ = POLYMARKET.depth();
}

#[test]
#[should_panic(expected = "declares no dial_timeout")]
fn a_lane_without_a_dial_bound_refuses_one() {
    let _ = BYBIT.exec().dial_timeout();
}

#[test]
#[should_panic(expected = "declares no handshake_timeout")]
fn a_lane_without_a_handshake_refuses_one() {
    let _ = HYPERLIQUID.exec().handshake_timeout();
}

#[test]
#[should_panic(expected = "declares no response_timeout")]
fn a_lane_without_a_reply_bound_refuses_one() {
    let _ = BYBIT.exec().response_timeout();
}

#[test]
#[should_panic(expected = "declares no keepalive_every")]
fn a_lane_without_a_client_keepalive_refuses_one() {
    let _ = BINANCE_SPOT.exec().keepalive_every();
}

#[test]
#[should_panic(expected = "declares no session_renew_every")]
fn a_lane_without_a_session_to_renew_refuses_one() {
    let _ = BYBIT.exec().session_renew_every();
}

#[test]
#[should_panic(expected = "declares no idle_threshold")]
fn a_lane_without_a_stall_watchdog_refuses_one() {
    // fxcm's SDK-owned session declares none, on purpose (measured at the FX open of 2026-11-22).
    let _ = FXCM.exec().idle_threshold();
}

#[test]
#[should_panic(expected = "declares no reconnect policy")]
fn a_lane_that_does_not_redial_refuses_a_reconnect() {
    // fxcm's native SDK owns its reconnects and its lane declares none; a lane built bare says the same.
    let _ = VenueTimingsLane::NONE.reconnect();
}

#[test]
#[should_panic(expected = "declares no reseed_interval")]
fn a_depth_block_without_a_timed_reseed_refuses_one() {
    let _ = VenueTimingsDepth { reseed_interval: None, ..BYBIT.depth() }.reseed_interval();
}

// ---------------------------------------------------------------------------------------------
// The way a bridge consumes the table: a `const` read of the row. If any of these stops compiling,
// the accessors are no longer `const fn` and every bridge swap breaks with them. (A `const` that
// reads an undeclared facility would fail the BUILD of this test, which is the point.)
// ---------------------------------------------------------------------------------------------

#[test]
fn the_accessors_are_usable_in_a_const_initializer() {
    const HANDSHAKE: Duration = BYBIT.exec().handshake_timeout();
    const IDLE: Duration = BINANCE_PERP.exec().idle_threshold();
    const RENEW: Duration = BINANCE_PERP.exec().session_renew_every();
    const REDIAL: VenueTimingsReconnect = OANDA.market().reconnect();
    const DEPTH: VenueTimingsDepth = DERIBIT.depth();
    const RESEED: Duration = DEPTH.reseed_interval();

    assert_eq!(HANDSHAKE, Duration::from_secs(10));
    assert_eq!(IDLE, Duration::from_secs(720));
    assert_eq!(RENEW, Duration::from_secs(1800));
    assert!(REDIAL.is_fixed());
    assert_eq!(REDIAL.max, Duration::from_secs(3));
    assert_eq!(DEPTH.idle_threshold(), Duration::from_secs(60));
    assert_eq!(DEPTH.freshness_threshold(), Duration::from_secs(120));
    assert_eq!(RESEED, Duration::from_secs(300));
    assert_eq!(DEPTH.backoff(), Duration::from_secs(3));
}

// ---------------------------------------------------------------------------------------------
// The submit chains (the module doc's `submit`): each declared chain answers before the core's
// last-resort reject, so a slow but legitimate submit is never rejected by the backstop.
// ---------------------------------------------------------------------------------------------

/// **Every declared submit chain answers before the core's last resort**: the order's age at its write plus its calls
/// at their bounds (or the bridge's hold, where shorter) end by `vike_model::submit_ack_last_resort` at the shipped ack
/// timeout and grace, the reject `crates/vike-core/src/runtime/watchdog/orders.rs`'s `sweep_stuck_orders` issues
/// (decision 0171). And the four chains longer than the shared exec loop's are declared, pinned as
/// `(venue, write_by, calls at their bounds, hold, answered by)` in milliseconds: IG's confirm ladder and IBKR's reply
/// chain are cut by their holds, Alpaca's mint is counted before its write, Polymarket's queue ends 25 s after arrival.
#[test]
fn every_declared_submit_chain_answers_before_the_cores_last_resort() {
    let last_resort = vike_model::submit_ack_last_resort(
        vike_model::DEFAULT_SUBMIT_ACK_TIMEOUT,
        vike_model::DEFAULT_SUBMIT_ACK_CONFIRM_GRACE,
    );
    let mut declared = Vec::new();
    for (venue, market, row) in every_row() {
        assert!(
            row.market.and_then(|lane| lane.submit).is_none(),
            "{venue}: a market lane submits nothing"
        );
        let Some(chain) = row.exec.and_then(|lane| lane.submit) else { continue };
        assert!(
            chain.answered_by() <= last_resort,
            "{venue} {market:?}: its submit is answered by {:?}, after the core's last-resort reject at {last_resort:?}",
            chain.answered_by()
        );
        declared.push((
            *venue,
            millis_of(chain.write_by),
            millis_of(chain.calls_at_their_bounds()),
            chain.hold.map(millis_of),
            millis_of(chain.answered_by()),
        ));
    }
    assert_eq!(
        declared,
        [
            ("polymarket", 25_000, 34_000, None, 59_000),
            ("ig", 20_000, 67_400, Some(35_000), 55_000),
            ("alpaca", 20_000, 35_000, None, 55_000),
            ("ibkr cpapi", 20_000, 300_000, Some(35_000), 55_000),
        ]
    );
}

/// The chain's arithmetic: the calls end to end, cut by a shorter hold and never by a longer one, after the write's age;
/// and a chain with a call made no times, a zero bound or a zero hold is not sane.
#[test]
fn a_chain_answers_by_its_write_age_and_its_calls_or_its_hold() {
    const CALLS: [VenueTimingsSubmitCall; 2] = [
        VenueTimingsSubmitCall::new("a", 1, Duration::from_secs(30)),
        VenueTimingsSubmitCall::new("b", 2, Duration::from_secs(5)),
    ];
    const NEVER: [VenueTimingsSubmitCall; 1] =
        [VenueTimingsSubmitCall::new("a", 0, Duration::from_secs(30))];
    const UNBOUNDED: [VenueTimingsSubmitCall; 1] =
        [VenueTimingsSubmitCall::new("a", 1, Duration::ZERO)];
    let s = Duration::from_secs;
    let chain =
        |hold| VenueTimingsSubmit { write_by: s(20), calls: &CALLS, hold, why: "a test's chain" };
    assert_eq!(chain(None).calls_at_their_bounds(), s(40));
    assert_eq!(chain(None).answered_by(), s(60));
    assert_eq!(chain(Some(s(35))).answered_by(), s(55), "a hold shorter than the calls cuts them");
    assert_eq!(chain(Some(s(50))).answered_by(), s(60), "a longer hold cuts nothing");
    assert!(chain(None).is_sane() && chain(Some(s(1))).is_sane());
    assert!(!chain(Some(Duration::ZERO)).is_sane(), "a zero hold");
    assert!(!VenueTimingsSubmit { calls: &NEVER, ..chain(None) }.is_sane(), "a call made no times");
    assert!(
        !VenueTimingsSubmit { calls: &UNBOUNDED, ..chain(None) }.is_sane(),
        "a call with no bound"
    );
    assert!(!VenueTimingsSubmit { calls: &[], ..chain(None) }.is_sane(), "no call at all");
}

/// A bridge reads its chain in a `const` (its pin beside the code), and a lane that declares none reads `None`.
#[test]
fn a_bridge_reads_its_chain_in_a_const() {
    const IG_HOLD: Option<Duration> = match IG.exec().submit {
        Some(chain) => chain.hold,
        None => None,
    };
    assert_eq!(IG_HOLD, Some(Duration::from_secs(35)));
    assert!(BYBIT.exec().submit.is_none(), "the shared exec loop's chain is no row");
}
