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
const MATRIX: &[(&str, Market, VenueTimings)] = &[
    ("binance", Market::Spot, BINANCE_SPOT),
    ("binance", Market::Perp, BINANCE_PERP),
    ("aster", Market::Spot, ASTER_SPOT),
    ("aster", Market::Perp, ASTER_PERP),
    ("bybit", Market::Spot, BYBIT),
    ("okx", Market::Spot, OKX),
    ("deribit", Market::Spot, DERIBIT),
    ("polymarket", Market::Spot, POLYMARKET),
    ("hyperliquid", Market::Spot, HYPERLIQUID),
    ("oanda", Market::Spot, OANDA),
    ("ig", Market::Spot, IG),
    ("fxcm", Market::Spot, FXCM),
    ("dukascopy", Market::Spot, DUKASCOPY),
    ("ibkr", Market::Spot, IBKR),
    ("ctrader", Market::Spot, CTRADER),
    ("alpaca", Market::Spot, ALPACA),
    // vike:new-venue:row ("{venue}", Market::Spot, {VENUE}), // TODO(new-venue: {venue}): a market-SPLIT venue declares two rows
];

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
    Market,
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

fn ms(t: Option<Timing>) -> Option<u64> {
    t.map(|t| millis_of(t.value))
}

fn assert_lane_is_pinned(who: &str, lane: Lane, pin: &LanePin) {
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
fn exec_lanes_are_pinned() {
    #[rustfmt::skip]
    let rows: &[LanePin] = &[
        // venue          market        dial        handshake     response    keepalive     session_renew  idle           reconnect (initial, max)
        // `crates/bridges/binance/src/user_data.rs`: HANDSHAKE_TIMEOUT 10 s, IDLE_THRESHOLD 60 s (3 x the 20 s server ping), no app
        // ping, no dial bound (`tungstenite::connect`), the pump's 30 s max_backoff literal over its 1 s first backoff.
        ("binance",     Market::Spot, None,       Some(10_000), None,       None,         None,          Some(60_000),  Some((1_000, 30_000))),
        // `crates/bridges/binance/src/family/listenkey.rs`: KEEPALIVE_EVERY 1800 s (a REST PUT), MAX_BACKOFF 30 s; the
        // `perp_user_data.rs` IDLE_THRESHOLD 720 s; no ack, so no handshake.
        ("binance",     Market::Perp, None,       None,         None,       None,         Some(1_800_000), Some(720_000), Some((1_000, 30_000))),
        // `crates/bridges/aster/src/user_data.rs` and `perp_user_data.rs`: the same listenKey pump, IDLE_THRESHOLD 1200 s in both.
        ("aster",       Market::Spot, None,       None,         None,       None,         Some(1_800_000), Some(1_200_000), Some((1_000, 30_000))),
        ("aster",       Market::Perp, None,       None,         None,       None,         Some(1_800_000), Some(1_200_000), Some((1_000, 30_000))),
        // `crates/bridges/bybit/src/user_data.rs`: HANDSHAKE_TIMEOUT 10 s, PING_EVERY 20 s, IDLE_THRESHOLD 60 s.
        ("bybit",       Market::Spot, None,       Some(10_000), None,       Some(20_000), None,          Some(60_000),  Some((1_000, 30_000))),
        // `crates/bridges/okx/src/user_data.rs`: HANDSHAKE_TIMEOUT 10 s, PING_EVERY 15 s, IDLE_THRESHOLD 60 s.
        ("okx",         Market::Spot, None,       Some(10_000), None,       Some(15_000), None,          Some(60_000),  Some((1_000, 30_000))),
        // `crates/bridges/deribit/src/user_data.rs`: HANDSHAKE_TIMEOUT 10 s, PING_EVERY 20 s, IDLE_THRESHOLD 60 s; and its ORDER socket,
        // `crates/bridges/deribit/src/transport.rs`: `request_timeout` 10 s, the wait for each JSON-RPC reply (found late, pinned with its row), and the ORDER socket's dial bound 10 s (the user-data dial stays bare).
        ("deribit",     Market::Spot, Some(10_000), Some(10_000), Some(10_000), Some(20_000), None,      Some(60_000),  Some((1_000, 30_000))),
        // `crates/bridges/polymarket/src/exec_plane/user_data.rs`: PING_EVERY 10 s, IDLE_THRESHOLD 60 s, no ack, `connect_ws(.., None)`.
        ("polymarket",  Market::Spot, None,       None,         None,       Some(10_000), None,          Some(60_000),  Some((1_000, 30_000))),
        // `crates/bridges/hyperliquid/src/exec/user_data.rs`: WS_PING_SECS 30, IDLE_THRESHOLD 4 x that, MAX_BACKOFF 30 s, no ack.
        ("hyperliquid", Market::Spot, None,       None,         None,       Some(30_000), None,          Some(120_000), Some((1_000, 30_000))),
        // `crates/bridges/oanda/src/stream.rs`: CONNECT_TIMEOUT 10 s (dial AND `timeout_recv_response`), RECV_IDLE_TIMEOUT 20 s,
        // RECONNECT_BACKOFF a fixed 1 s.
        ("oanda",       Market::Spot, Some(10_000), None,       Some(10_000), None,       None,          Some(20_000),  Some((1_000, 1_000))),
        // `crates/bridges/ig/src/exec/stream.rs`: `timeout_recv_body` 30 s; `stream/retry.rs`: RECONNECT_DELAY 1 s doubling to
        // MAX_RECONNECT_BACKOFF 60 s.
        ("ig",          Market::Spot, None,       None,         None,       None,         None,          Some(30_000),  Some((1_000, 60_000))),
        // `crates/bridges/ctrader/src/conn.rs`: HANDSHAKE_TIMEOUT 15 s is a wait for EACH response, HEARTBEAT_INTERVAL 10 s;
        // `conn/session.rs`: RECONNECT_INITIAL_BACKOFF 200 ms doubling to RECONNECT_MAX_BACKOFF 5 s.
        ("ctrader",     Market::Spot, None,       None,         Some(15_000), Some(10_000), None,        None,          Some((200, 5_000))),
        // `crates/bridges/alpaca/src/stream.rs`: CONNECT_TIMEOUT 5 s, `timeout_recv_body` 30 s, the failed-open retry a fixed 2 s.
        ("alpaca",      Market::Spot, Some(5_000), None,        None,       None,         None,          Some(30_000),  Some((2_000, 2_000))),
        // `crates/bridges/vike-ibkr/src/transport/cpapi/mod.rs`: the 30 s REST tickle, the failed-dial wait a fixed 2 s.
        ("ibkr",        Market::Spot, None,       None,         None,       None,         Some(30_000),  None,          Some((2_000, 2_000))),
        // `crates/bridges/dukascopy/src/exec.rs`: READY_TIMEOUT 300 s; RESTART 5 s doubling to 60 s (a sidecar that died
        // after `ready` is restarted by the client's supervisor).
        ("dukascopy",   Market::Spot, None,       Some(300_000), None,      None,         None,          None,          Some((5_000, 60_000))),
    ];
    for row in rows {
        let (venue, market) = (row.0, row.1);
        let lane = timings_for(venue, market)
            .exec
            .unwrap_or_else(|| panic!("{venue} {market:?}: no exec lane declared"));
        assert_lane_is_pinned(&format!("{venue} {market:?} exec"), lane, row);
    }
    // ...and no OTHER roster venue declares one: fxcm's SDK owns its transport.
    let pinned: Vec<&str> = rows.iter().map(|r| r.0).collect();
    for &v in vike_model::VENUES {
        assert_eq!(
            timings_for(v, Market::Spot).exec.is_some(),
            pinned.contains(&v),
            "{v}: exec lane declared iff pinned"
        );
    }
}

/// The MARKET lane of the venues whose live market feed is NOT a `pump_spec` row, pinned verbatim —
/// plus ig's handshake, the one fact a pump row cannot carry.
#[test]
fn market_lanes_are_pinned() {
    #[rustfmt::skip]
    let rows: &[LanePin] = &[
        // venue     market        dial          handshake     response      keepalive  session_renew  idle          reconnect (initial, max)
        // `crates/bridges/oanda/src/market_feed.rs`: CONNECT_TIMEOUT 10 s (dial AND `timeout_recv_response`), RECV_IDLE_TIMEOUT 20 s,
        // RECONNECT_BACKOFF a fixed 3 s (the shared pumps' classic).
        ("oanda",  Market::Spot, Some(10_000), None,         Some(10_000), None,      None,          Some(20_000), Some((3_000, 3_000))),
        // `crates/bridges/alpaca/src/data/conn.rs`: RECONNECT_BACKOFF_SLICES 30 x 100 ms = a fixed 3 s; the dial is `tungstenite::connect`.
        ("alpaca", Market::Spot, None,         None,         None,         None,      None,          None,         Some((3_000, 3_000))),
        // `crates/bridges/vike-ibkr/src/cpapi_feed/pump.rs`: TICKLE_EVERY 30 s, REDIAL a fixed 2 s.
        ("ibkr",   Market::Spot, None,         None,         None,         None,      Some(30_000),  None,         Some((2_000, 2_000))),
        // `crates/bridges/ig/src/market_feed.rs`: HANDSHAKE_BUDGET 15 s for create_session -> CONOK; every other ig market knob is a
        // `pump_spec` row.
        ("ig",     Market::Spot, None,         Some(15_000), None,         None,      None,          None,         None),
    ];
    for row in rows {
        let (venue, market) = (row.0, row.1);
        let lane = timings_for(venue, market)
            .market
            .unwrap_or_else(|| panic!("{venue} {market:?}: no market lane declared"));
        assert_lane_is_pinned(&format!("{venue} {market:?} market"), lane, row);
    }
    // ...and every OTHER roster venue declares no market lane here: the seven other pump venues
    // keep theirs in `pump_spec`, ctrader's rides its exec session, fxcm and dukascopy have none.
    let pinned: Vec<&str> = rows.iter().map(|r| r.0).collect();
    for &v in vike_model::VENUES {
        assert_eq!(
            timings_for(v, Market::Spot).market.is_some(),
            pinned.contains(&v),
            "{v}: market lane declared iff pinned"
        );
    }
}

/// One depth-pin row: `(venue, market, idle, freshness, reseed, backoff)`, milliseconds.
type DepthPinRow = (&'static str, Market, u64, u64, Option<u64>, u64);

/// The DEPTH driver's four knobs, pinned verbatim: `(venue, market, idle, freshness, reseed,
/// backoff)` in milliseconds.
#[test]
fn depth_blocks_are_pinned() {
    #[rustfmt::skip]
    let rows: &[DepthPinRow] = &[
        // `crates/bridges/binance/src/family/market_feed.rs`: DEPTH_IDLE_THRESHOLD 30 s, DEPTH_FRESHNESS_THRESHOLD 120 s,
        // DEPTH_RESEED_INTERVAL 300 s, DEPTH_BACKOFF 3 s. Aster runs the SAME `depth_main`.
        ("binance",  Market::Spot, 30_000, 120_000, Some(300_000), 3_000),
        ("binance",  Market::Perp, 30_000, 120_000, Some(300_000), 3_000),
        ("aster",    Market::Spot, 30_000, 120_000, Some(300_000), 3_000),
        ("aster",    Market::Perp, 30_000, 120_000, Some(300_000), 3_000),
        // `crates/bridges/bybit/src/market_feed/depth.rs`: the same four constants.
        ("bybit",    Market::Spot, 30_000, 120_000, Some(300_000), 3_000),
        // `crates/bridges/okx/src/market_feed/depth.rs`: the same four constants.
        ("okx",      Market::Spot, 30_000, 120_000, Some(300_000), 3_000),
        // `crates/bridges/deribit/src/market_feed.rs`: DEPTH_FRESHNESS_THRESHOLD 120 s, DEPTH_RESEED_INTERVAL 300 s, DEPTH_BACKOFF 3 s;
        // its idle threshold is the 60 s its `pump_spec` row declares (`knobs.idle_threshold`).
        ("deribit",  Market::Spot, 60_000, 120_000, Some(300_000), 3_000),
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
            timings_for(v, Market::Spot).depth.is_some(),
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
        let rows: Vec<(&str, Market, VenueTimings)> =
            MATRIX.iter().copied().filter(|(rv, _, _)| *rv == v).collect();
        assert!(!rows.is_empty(), "no VenueTimings row declared for roster venue {v}");
        for (_, market, want) in rows.iter().copied() {
            assert_eq!(timings_for(v, market), want, "{v}: registry must serve its row");
        }
        // A market-UNIFORM venue's single row answers both markets — a venue is never
        // half-declared, whatever `MATRIX` happens to list.
        if rows.len() == 1 {
            let want = rows[0].2;
            for market in [Market::Spot, Market::Perp] {
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
        ("fxcm", FXCM),
        // vike:new-venue:row ("{venue}", {VENUE}), // TODO(new-venue: {venue}): DELETE this row once the venue declares real timings
    ];
    for (venue, row) in named {
        assert_eq!(*row, VenueTimings::NOT_DECLARED, "{venue} declares nothing today");
        for market in [Market::Spot, Market::Perp] {
            assert_eq!(timings_for(venue, market), *row, "{venue} {market:?}");
        }
    }
    // Every roster venue is either in `named` or carries real timings — no third state.
    for &v in vike_model::VENUES {
        let declared = timings_for(v, Market::Spot) != VenueTimings::NOT_DECLARED;
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
        let split = timings_for(v, Market::Spot) != timings_for(v, Market::Perp);
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
        let Some(Lane {
            dial_timeout,
            handshake_timeout,
            response_timeout,
            keepalive_every,
            session_renew_every,
            idle_threshold,
            reconnect,
        }) = lane
        else {
            continue;
        };
        for (field, timing) in [
            ("dial_timeout", dial_timeout),
            ("handshake_timeout", handshake_timeout),
            ("response_timeout", response_timeout),
            ("keepalive_every", keepalive_every),
            ("session_renew_every", session_renew_every),
            ("idle_threshold", idle_threshold),
        ] {
            if let Some(t) = timing {
                out.push((format!("{lane_name}.{field}"), t.value, t.why));
            }
        }
        if let Some(r) = reconnect {
            out.push((format!("{lane_name}.reconnect"), r.max, r.why));
        }
    }
    if let Some(DepthTimings { idle_threshold, freshness_threshold, reseed_interval, backoff }) =
        depth
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
    for (venue, market, row) in MATRIX {
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
    for (venue, _market, row) in MATRIX {
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
    for (venue, market, row) in MATRIX {
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
    for (venue, market, row) in MATRIX {
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
    for (venue, _market, row) in MATRIX {
        for (lane_name, lane) in [("exec", row.exec), ("market", row.market)] {
            if lane.and_then(|l| l.reconnect).is_some_and(|r| r.is_fixed()) {
                fixed.push(format!("{venue}/{lane_name}"));
            }
        }
    }
    fixed.sort();
    assert_eq!(
        fixed,
        ["alpaca/exec", "alpaca/market", "ibkr/exec", "ibkr/market", "oanda/exec", "oanda/market"],
        "a fixed-wait reconnect appeared or vanished"
    );
}

/// An unknown venue is fail-closed and equals `Default`.
#[test]
fn an_unknown_venue_is_not_declared() {
    assert_eq!(timings_for("nasdaq", Market::Spot), VenueTimings::NOT_DECLARED);
    assert_eq!(VenueTimings::default(), VenueTimings::NOT_DECLARED);
}

/// The sanity check is not vacuous: each way a lane can be wrong is refused.
#[test]
fn the_sanity_check_refuses_a_zero_an_inverted_reconnect_and_a_tight_idle() {
    let why = "a reason that is long enough to count";
    assert!(Lane::NONE.is_sane());

    let zero = Lane { dial_timeout: Some(Timing::secs(0, why)), ..Lane::NONE };
    assert!(!zero.is_sane(), "a zero dial bound");

    let inverted = Lane {
        reconnect: Some(Reconnect::new(Duration::from_secs(5), Duration::from_secs(1), why)),
        ..Lane::NONE
    };
    assert!(!inverted.is_sane(), "a reconnect that starts above its ceiling");

    let hot = Lane { reconnect: Some(Reconnect::fixed(Duration::ZERO, why)), ..Lane::NONE };
    assert!(!hot.is_sane(), "a zero reconnect wait is a hot loop");

    let tight = Lane {
        keepalive_every: Some(Timing::secs(20, why)),
        idle_threshold: Some(Timing::secs(39, why)),
        ..Lane::NONE
    };
    assert!(!tight.is_sane(), "an idle threshold under two keepalive periods");

    let exact = Lane {
        keepalive_every: Some(Timing::secs(20, why)),
        idle_threshold: Some(Timing::secs(40, why)),
        ..Lane::NONE
    };
    assert!(exact.is_sane(), "exactly two keepalive periods is allowed");

    let depth = DepthTimings {
        idle_threshold: Timing::secs(30, why),
        freshness_threshold: Timing::secs(120, why),
        reseed_interval: None,
        backoff: Timing::secs(0, why),
    };
    assert!(!depth.is_sane(), "a zero depth backoff");
    assert!(!VenueTimings { depth: Some(depth), ..VenueTimings::NOT_DECLARED }.is_sane());
}

/// The constructors build what they say, and a fixed wait is exactly `initial == max`.
#[test]
fn the_constructors_build_what_they_say() {
    let t = Timing::millis(1500, "milliseconds are kept to the millisecond");
    assert_eq!(t.value, Duration::from_millis(1500));
    assert_eq!(Timing::secs(2, "whole seconds are kept whole, too").value, Duration::from_secs(2));

    let growing = Reconnect::new(Duration::from_millis(200), Duration::from_secs(5), "growing");
    assert!(!growing.is_fixed());
    assert!(growing.is_sane());
    let fixed = Reconnect::fixed(Duration::from_secs(3), "fixed");
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
fn a_venue_without_an_exec_lane_refuses_one() {
    let _ = FXCM.exec();
}

#[test]
#[should_panic(expected = "declares no market lane timings here")]
fn a_venue_without_a_market_lane_here_refuses_one() {
    let _ = BYBIT.market();
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
    let _ = CTRADER.exec().idle_threshold();
}

#[test]
#[should_panic(expected = "declares no reconnect policy")]
fn a_lane_that_does_not_redial_refuses_a_reconnect() {
    // fxcm's native SDK owns its reconnects and the row declares nothing; a lane built bare says the same.
    let _ = Lane::NONE.reconnect();
}

#[test]
#[should_panic(expected = "declares no reseed_interval")]
fn a_depth_block_without_a_timed_reseed_refuses_one() {
    let _ = DepthTimings { reseed_interval: None, ..BYBIT.depth() }.reseed_interval();
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
    const REDIAL: Reconnect = OANDA.market().reconnect();
    const DEPTH: DepthTimings = DERIBIT.depth();
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
