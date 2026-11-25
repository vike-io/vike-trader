use super::*;
use crate::session::{FX_WEEK, session_for};
use crate::venue_timings::{VenueTimings, VenueTimingsLane, timings_for};
use std::collections::BTreeSet;
use vike_model::VENUES;

use VenueConnectionPathReport::{AccountSession, NotReported, OwnSession, SharedPump, Sidecar};
use VenueConnectionRecovery::{
    Cursor, HistoryWindow, ReconcileOnly, SessionReplay, SnapshotOnSubscribe,
};
use VenueConnectionScheduledClosure::{FxWeek, FxWeekStartOnly, NoClosure};
use VenueConnectionSessionHeal::{OwnReconnect, Supervised};
use VenueRateLimitsMarket::{Perp, Spot};

/// `(venue, market, exec driver, recovery, nudges reconcile, path report, session heal, announced close, scheduled
/// closure, market driver, market announced close)`. A driver and a notice are pinned as their kind's word
/// ([`driver_word`], [`close_word`]); the sites and the notices themselves are pinned by
/// `every_driver_site_is_in_the_tree` and `the_announced_closes_are_pinned`.
type Pin = (
    &'static str,
    VenueRateLimitsMarket,
    &'static str,
    VenueConnectionRecovery,
    bool,
    VenueConnectionPathReport,
    VenueConnectionSessionHeal,
    &'static str,
    VenueConnectionScheduledClosure,
    &'static str,
    &'static str,
);

// ---------------------------------------------------------------------------------------------
// THE MATRIX PIN, read from the bridges, the adapter flags and the tradehub's lists as they stand, before any
// consumer reads the table. A venue that answers both markets alike is pinned once, on `Spot`
// (`market_split_venues_are_exactly_binance` holds the other market equal).
//
// `#[rustfmt::skip]`: the table is read across, and a `just new-venue` marker at the tail of a bracketed literal must
// not be re-indented (`crates/vike-ops/tests/venues/new_venue_gate/rustfmt_rule.rs`).
// ---------------------------------------------------------------------------------------------
#[rustfmt::skip]
const PIN: &[Pin] = &[
    ("binance",     Spot, "UserPump",      HistoryWindow,       true,  SharedPump,     OwnReconnect, "Handled",   NoClosure,       "MarketPump",    "Handled"),
    ("binance",     Perp, "UserPump",      HistoryWindow,       true,  SharedPump,     OwnReconnect, "NoneKnown", NoClosure,       "MarketPump",    "NoneKnown"),
    ("aster",       Spot, "UserPump",      HistoryWindow,       false, SharedPump,     OwnReconnect, "NoneKnown", NoClosure,       "MarketPump",    "NoneKnown"),
    ("bybit",       Spot, "UserPump",      HistoryWindow,       true,  SharedPump,     OwnReconnect, "NoneKnown", NoClosure,       "MarketPump",    "NoneKnown"),
    ("okx",         Spot, "UserPump",      HistoryWindow,       true,  SharedPump,     OwnReconnect, "Handled",   NoClosure,       "MarketPump",    "Handled"),
    ("deribit",     Spot, "UserPump",      HistoryWindow,       false, SharedPump,     OwnReconnect, "Handled",   NoClosure,       "MarketPump",    "NoneKnown"),
    ("polymarket",  Spot, "UserPump",      HistoryWindow,       false, SharedPump,     OwnReconnect, "NoneKnown", NoClosure,       "MarketPump",    "NoneKnown"),
    ("hyperliquid", Spot, "UserPump",      SnapshotOnSubscribe, true,  SharedPump,     OwnReconnect, "NoneKnown", NoClosure,       "MarketPump",    "NoneKnown"),
    ("alpaca",      Spot, "UserPump",      Cursor,              false, SharedPump,     OwnReconnect, "NoneKnown", NoClosure,       "MarketPump",    "NoneKnown"),
    ("oanda",       Spot, "UserPump",      Cursor,              false, SharedPump,     OwnReconnect, "NoneKnown", FxWeekStartOnly, "MarketPump",    "NoneKnown"),
    ("ig",          Spot, "OwnLoop",       SnapshotOnSubscribe, false, OwnSession,     OwnReconnect, "Handled",   FxWeek,          "MarketPump",    "Handled"),
    ("ctrader",     Spot, "NativeSession", SessionReplay,       false, AccountSession, OwnReconnect, "Handled",   FxWeek,          "NativeSession", "Handled"),
    ("fxcm",        Spot, "NativeSession", SessionReplay,       false, OwnSession,     Supervised,   "NoneKnown", FxWeek,          "NativeSession", "NoneKnown"),
    ("dukascopy",   Spot, "NativeSession", SessionReplay,       false, Sidecar,        OwnReconnect, "NoneKnown", FxWeek,          "NativeSession", "NoneKnown"),
    ("ibkr",        Spot, "NativeSession", SessionReplay,       false, OwnSession,     Supervised,   "NoneKnown", NoClosure,       "NativeSession", "NoneKnown"),
    // vike:new-venue:row ("{venue}", Spot, "No", ReconcileOnly, false, NotReported, OwnReconnect, "NoneKnown", NoClosure, "No", "NoneKnown"), // TODO(new-venue: {venue}): move with the row; a market-SPLIT venue pins both markets
];

/// IBKR's Client Portal backend, read by name ([`IBKR_CPAPI`]).
#[rustfmt::skip]
const IBKR_CPAPI_PIN: Pin =
    ("ibkr", Spot, "UserPump", HistoryWindow, false, SharedPump, OwnReconnect, "NoneKnown", NoClosure, "MarketPump", "NoneKnown");

fn driver_word(driver: VenueConnectionDriver) -> &'static str {
    match driver {
        VenueConnectionDriver::UserPump => "UserPump",
        VenueConnectionDriver::MarketPump => "MarketPump",
        VenueConnectionDriver::OwnLoop { .. } => "OwnLoop",
        VenueConnectionDriver::NativeSession { .. } => "NativeSession",
        VenueConnectionDriver::No { .. } => "No",
    }
}

fn close_word(close: VenueConnectionAnnouncedClose) -> &'static str {
    match close {
        VenueConnectionAnnouncedClose::NoneKnown => "NoneKnown",
        VenueConnectionAnnouncedClose::Ignored { .. } => "Ignored",
        VenueConnectionAnnouncedClose::Handled { .. } => "Handled",
    }
}

fn site_of(driver: VenueConnectionDriver) -> Option<&'static str> {
    match driver {
        VenueConnectionDriver::OwnLoop { site } | VenueConnectionDriver::NativeSession { site } => {
            Some(site)
        }
        _ => None,
    }
}

fn assert_pinned(who: &str, row: VenueConnection, pin: &Pin) {
    let (
        _,
        _,
        exec_driver,
        recovery,
        nudges,
        path,
        heal,
        close,
        closure,
        market_driver,
        market_close,
    ) = *pin;
    let exec = row.exec;
    assert_eq!(driver_word(exec.driver), exec_driver, "{who} exec driver");
    assert_eq!(exec.recovery, recovery, "{who} recovery");
    assert_eq!(exec.nudges_reconcile, nudges, "{who} nudges_reconcile");
    assert_eq!(exec.path_report, path, "{who} path_report");
    assert_eq!(exec.session_heal, heal, "{who} session_heal");
    assert_eq!(close_word(exec.announced_close), close, "{who} announced_close");
    assert_eq!(exec.scheduled_closure, closure, "{who} scheduled_closure");
    assert_eq!(driver_word(row.market.driver), market_driver, "{who} market driver");
    assert_eq!(
        close_word(row.market.announced_close),
        market_close,
        "{who} market announced_close"
    );
}

/// Every declared row with a name for the messages: the pinned `(venue, market)` rows and the cpapi backend.
fn every_row() -> Vec<(String, VenueConnection)> {
    let mut rows: Vec<(String, VenueConnection)> = PIN
        .iter()
        .map(|&(venue, market, ..)| (format!("{venue} {market:?}"), connection_for(venue, market)))
        .collect();
    rows.push(("ibkr cpapi".to_string(), IBKR_CPAPI));
    rows
}

/// The whole table, verbatim.
#[test]
fn the_connection_matrix_is_pinned_verbatim() {
    for pin in PIN {
        let (venue, market, ..) = *pin;
        assert_pinned(&format!("{venue} {market:?}"), connection_for(venue, market), pin);
    }
    assert_pinned("ibkr cpapi", IBKR_CPAPI, &IBKR_CPAPI_PIN);
}

/// The market lane's `subscribe_frame`, verbatim: the value the shared market pump reads for each roster lane and
/// the Client Portal's (`crates/vike-bridge-core/src/websocket_session/spec.rs`'s `row`), `false` on
/// every lane off the pump. A frame-subscribing lane is on the pump, and of them only bybit, okx and deribit watch an
/// acknowledgement of it (ig's `SUBOK` answers its connect hook's subscribe).
#[test]
fn the_market_subscribe_frame_is_pinned_verbatim() {
    #[rustfmt::skip]
    const FRAME: &[(&str, bool)] = &[
        ("binance",     false),
        ("aster",       false),
        ("bybit",       true),
        ("okx",         true),
        ("deribit",     true),
        ("polymarket",  true),
        ("hyperliquid", true),
        ("alpaca",      false),
        ("oanda",       false),
        ("ig",          false),
        ("ctrader",     false),
        ("fxcm",        false),
        ("dukascopy",   false),
        ("ibkr",        false),
        // vike:new-venue:row ("{venue}", false), // TODO(new-venue: {venue}): does its market lane replay a subscribe frame?
    ];
    assert_eq!(FRAME.len(), VENUES.len(), "the pin names every roster venue once");
    for &(venue, frame) in FRAME {
        assert!(VENUES.contains(&venue), "{venue} is not a roster venue");
        for market in [Spot, Perp] {
            let lane = connection_for(venue, market).market;
            assert_eq!(lane.subscribe_frame, frame, "{venue} {market:?}");
            if frame {
                assert_eq!(
                    lane.driver,
                    VenueConnectionDriver::MarketPump,
                    "{venue}: a frame rides the pump"
                );
            }
        }
    }
    const {
        assert!(
            !IBKR_CPAPI.market.subscribe_frame,
            "ibkr cpapi: its lane's stream subscribes, no frame"
        );
        assert!(!VenueConnection::UNKNOWN.market.subscribe_frame);
    }
}

/// Every roster venue is a NAMED row: the pin covers `vike_model::VENUES` exactly and no roster venue rides the
/// unknown fallback, on either market. An unknown string answers that fallback.
#[test]
fn every_roster_venue_has_a_named_row() {
    for venue in VENUES {
        assert!(PIN.iter().any(|(v, ..)| v == venue), "{venue} has no pinned row");
        for market in [Spot, Perp] {
            assert_ne!(
                connection_for(venue, market),
                VenueConnection::UNKNOWN,
                "{venue} {market:?} rides the fallback"
            );
        }
    }
    for (venue, ..) in PIN {
        assert!(VENUES.contains(venue), "{venue} is not a roster venue");
    }
    assert_eq!(connection_for("nasdaq", Spot), VenueConnection::UNKNOWN);
}

/// binance is the one market-SPLIT venue (its spot WS-API stream and its perp listenKey stream are two sockets):
/// every other venue answers its perp market with its spot row, which is why the pin names it once.
#[test]
fn market_split_venues_are_exactly_binance() {
    let split: Vec<&str> = VENUES
        .iter()
        .copied()
        .filter(|venue| connection_for(venue, Spot) != connection_for(venue, Perp))
        .collect();
    assert_eq!(split, ["binance"]);
}

/// What a consumer asks per VENUE (one adapter flag, one path watch, one supervisor, one calendar) has one answer
/// for both markets.
#[test]
fn the_venue_wide_answers_agree_across_markets() {
    for venue in VENUES {
        let (spot, perp) = (connection_for(venue, Spot).exec, connection_for(venue, Perp).exec);
        assert_eq!(spot.nudges_reconcile, perp.nudges_reconcile, "{venue}: one adapter flag");
        assert_eq!(spot.path_report, perp.path_report, "{venue}: one path watch");
        assert_eq!(spot.session_heal, perp.session_heal, "{venue}: one supervisor");
        assert_eq!(spot.scheduled_closure, perp.scheduled_closure, "{venue}: one calendar");
        assert_eq!(spot.conformance, perp.conformance, "{venue}: one live gate");
    }
}

/// The closure is the venue's session: a venue whose `session_for` is the FX week is held by it, and no other is.
#[test]
fn the_scheduled_closure_is_the_fx_week_of_session_for() {
    for venue in VENUES {
        let closure = connection_for(venue, Spot).exec.scheduled_closure;
        assert_eq!(closure != NoClosure, session_for(venue) == FX_WEEK, "{venue}");
    }
}

/// A timing row's `history_lag` schedules a second REST replay, so it belongs to a lane that replays history.
#[test]
fn a_history_lag_rides_a_history_replay() {
    let mut lagged = 0;
    for venue in VENUES {
        for market in [Spot, Perp] {
            if timings_for(venue, market).exec.and_then(|lane| lane.history_lag).is_some() {
                assert_eq!(
                    connection_for(venue, market).exec.recovery,
                    HistoryWindow,
                    "{venue} {market:?}"
                );
                lagged += 1;
            }
        }
    }
    assert!(lagged > 0, "no row declares a history lag: the check has gone inert");
}

/// What the loop decides: only the shared pump reports the shared pump's stream link, and no lane runs the other lane's
/// pump.
#[test]
fn the_answers_follow_from_the_loop() {
    for (who, row) in every_row() {
        let exec = row.exec;
        assert!(
            exec.path_report != SharedPump || exec.driver == VenueConnectionDriver::UserPump,
            "{who}: the stream link is the shared pump's"
        );
        assert_ne!(exec.driver, VenueConnectionDriver::MarketPump, "{who}: an exec lane");
        assert_ne!(row.market.driver, VenueConnectionDriver::UserPump, "{who}: a market lane");
        for (driver, close) in
            [(exec.driver, exec.announced_close), (row.market.driver, row.market.announced_close)]
        {
            assert!(
                driver_word(driver) != "No" || close == VenueConnectionAnnouncedClose::NoneKnown,
                "{who}: a lane that does not run hears no notice"
            );
        }
    }
}

/// The announced closes of both lanes of every row, pinned exhaustively and verbatim: a notice the bridge ignores is a
/// declared gap, and one it acts on is a deliberate edit here.
#[test]
fn the_announced_closes_are_pinned() {
    let mut said = Vec::new();
    for (who, row) in every_row() {
        for (lane, close) in
            [("exec", row.exec.announced_close), ("market", row.market.announced_close)]
        {
            match close {
                VenueConnectionAnnouncedClose::NoneKnown => {}
                VenueConnectionAnnouncedClose::Ignored { notice } => {
                    said.push(format!("{who} {lane} ignores {notice}"));
                }
                VenueConnectionAnnouncedClose::Handled { notice } => {
                    said.push(format!("{who} {lane} handles {notice}"));
                }
            }
        }
    }
    assert_eq!(
        said,
        [
            "binance Spot exec handles serverShutdown",
            "binance Spot market handles serverShutdown",
            "okx Spot exec handles 64008",
            "okx Spot market handles 64008",
            "deribit Spot exec handles platform_state",
            "ig Spot exec handles LOOP / END",
            "ig Spot market handles LOOP / END",
            "ctrader Spot exec handles ProtoOAAccountDisconnectEvent",
            "ctrader Spot market handles ProtoOAAccountDisconnectEvent",
        ]
    );
}

/// Every own loop and native session names where it lives, and that place is in the tree: a move of the loop must
/// move its row.
#[test]
fn every_driver_site_is_in_the_tree() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut sites = 0;
    for (who, row) in every_row() {
        for driver in [row.exec.driver, row.market.driver] {
            if let Some(site) = site_of(driver) {
                assert!(root.join(site).exists(), "{who}: {site} is not in the tree");
                sites += 1;
            }
        }
    }
    // The table's count once Wave C moved alpaca, oanda's exec and ibkr cpapi's exec onto the shared pump, and oanda's
    // and ibkr cpapi's market lanes followed. A floor guards against an inert walk, not a fixed number: a lane that
    // leaves its own loop lowers it.
    assert!(sites >= 7, "only {sites} sites were examined: the walk has gone inert");
}

/// Every lane cites the code it was read from (a scaffold's `NOT_DECLARED` row has read nothing yet).
#[test]
fn every_row_cites_its_code() {
    for (who, row) in every_row() {
        if row == VenueConnection::NOT_DECLARED {
            continue;
        }
        for why in [row.exec.why, row.market.why] {
            assert!(why.contains("`crates/"), "{who}: a lane without a citation: {why:?}");
        }
    }
}

/// The scaffold's row and the fallback declare nothing: no loop, nothing read back, no poke, no notice on either lane,
/// no path, no calendar, no proof.
#[test]
fn the_undeclared_rows_declare_nothing() {
    for row in [VenueConnection::NOT_DECLARED, VenueConnection::UNKNOWN] {
        assert_eq!(driver_word(row.exec.driver), "No");
        assert_eq!(driver_word(row.market.driver), "No");
        assert_eq!(row.exec.recovery, ReconcileOnly);
        assert!(!row.exec.nudges_reconcile);
        assert_eq!(row.exec.announced_close, VenueConnectionAnnouncedClose::NoneKnown);
        assert_eq!(row.market.announced_close, VenueConnectionAnnouncedClose::NoneKnown);
        assert_eq!(row.exec.path_report, NotReported);
        assert_eq!(row.exec.scheduled_closure, NoClosure);
        assert!(
            matches!(row.exec.conformance, VenueConnectionConformance::Unproven { .. }),
            "an undeclared row is never proven, so it never arms live"
        );
    }
    assert_ne!(VenueConnection::NOT_DECLARED, VenueConnection::UNKNOWN);
}

// ---------------------------------------------------------------------------------------------
// ONE PRODUCT, TWO ROWS: the connection table and the timings table each name a row per product, and a pair that
// describes two products is wrong in both (IBKR's timings row once held the Client Portal's waits while its connection
// row and every reader of it meant the socket).
// ---------------------------------------------------------------------------------------------

/// A row const of one table, with the name it has there (spelled once).
macro_rules! named {
    ($table:ident :: $name:ident) => {
        (stringify!($name), crate::$table::$name)
    };
}

/// `(venue, the markets both registries answer it for, its connection row, its timings row)`; no market: a product read
/// by name, which no registry answers.
type Product = (
    &'static str,
    &'static [VenueRateLimitsMarket],
    (&'static str, VenueConnection),
    (&'static str, VenueTimings),
);

/// **The one pairing of the two tables**: every product with the row each table declares for it.
#[rustfmt::skip]
const PRODUCTS: &[Product] = &[
    ("binance",     &[Spot],       named!(venue_connection::BINANCE_SPOT), named!(venue_timings::BINANCE_SPOT)),
    ("binance",     &[Perp],       named!(venue_connection::BINANCE_PERP), named!(venue_timings::BINANCE_PERP)),
    ("aster",       &[Spot],       named!(venue_connection::ASTER),        named!(venue_timings::ASTER_SPOT)),
    ("aster",       &[Perp],       named!(venue_connection::ASTER),        named!(venue_timings::ASTER_PERP)),
    ("bybit",       &[Spot, Perp], named!(venue_connection::BYBIT),        named!(venue_timings::BYBIT)),
    ("okx",         &[Spot, Perp], named!(venue_connection::OKX),          named!(venue_timings::OKX)),
    ("deribit",     &[Spot, Perp], named!(venue_connection::DERIBIT),      named!(venue_timings::DERIBIT)),
    ("polymarket",  &[Spot, Perp], named!(venue_connection::POLYMARKET),   named!(venue_timings::POLYMARKET)),
    ("hyperliquid", &[Spot, Perp], named!(venue_connection::HYPERLIQUID),  named!(venue_timings::HYPERLIQUID)),
    ("alpaca",      &[Spot, Perp], named!(venue_connection::ALPACA),       named!(venue_timings::ALPACA)),
    ("oanda",       &[Spot, Perp], named!(venue_connection::OANDA),        named!(venue_timings::OANDA)),
    ("ig",          &[Spot, Perp], named!(venue_connection::IG),           named!(venue_timings::IG)),
    ("ctrader",     &[Spot, Perp], named!(venue_connection::CTRADER),      named!(venue_timings::CTRADER)),
    ("fxcm",        &[Spot, Perp], named!(venue_connection::FXCM),         named!(venue_timings::FXCM)),
    ("dukascopy",   &[Spot, Perp], named!(venue_connection::DUKASCOPY),    named!(venue_timings::DUKASCOPY)),
    ("ibkr",        &[Spot, Perp], named!(venue_connection::IBKR),         named!(venue_timings::IBKR)),
    ("ibkr",        &[],           named!(venue_connection::IBKR_CPAPI),   named!(venue_timings::IBKR_CPAPI)),
    // vike:new-venue:row ("{venue}", &[Spot, Perp], named!(venue_connection::{VENUE}), named!(venue_timings::{VENUE})), // TODO(new-venue: {venue}): a pair per product, and per market where its markets run different sockets
];

/// The `pub const` names of type `ty` in a table's source: a row const that no pair names is what this finds.
fn row_consts<'a>(source: &'a str, ty: &str) -> BTreeSet<&'a str> {
    source
        .lines()
        .filter_map(|line| line.strip_prefix("pub const ")?.split_once(':'))
        .filter(|(_, rest)| rest.split_whitespace().next() == Some(ty))
        .map(|(name, _)| name)
        .collect()
}

/// **Each registry answer is one product's two rows**: for every roster venue and market, `connection_for` and
/// `timings_for` answer the two rows of the ONE product [`PRODUCTS`] pairs for it, so binance's spot and perp rows stay
/// apart in both tables and neither table can answer one product while the other answers another.
#[test]
fn each_registry_answer_is_one_products_two_rows() {
    for venue in VENUES {
        for market in [Spot, Perp] {
            let answering: Vec<&Product> = PRODUCTS
                .iter()
                .filter(|(v, markets, ..)| v == venue && markets.contains(&market))
                .collect();
            let [(_, _, (connection_name, connection), (timings_name, timings))] =
                answering.as_slice()
            else {
                panic!("{venue} {market:?}: {} products answer it, not one", answering.len());
            };
            assert_eq!(
                connection_for(venue, market),
                *connection,
                "{venue} {market:?}: {connection_name}"
            );
            assert_eq!(timings_for(venue, market), *timings, "{venue} {market:?}: {timings_name}");
        }
    }
    for (venue, ..) in PRODUCTS {
        assert!(VENUES.contains(venue), "{venue} is not a roster venue");
    }
}

/// **Every row const of either table is one product's**: the names [`PRODUCTS`] pairs are exactly the
/// `VenueConnection` consts of `venue_connection.rs` and the `VenueTimings` consts of `venue_timings.rs`, read off their
/// source. A venue that grows a second product (a backend, a market on a socket of its own) declares a row in EACH table
/// and pairs them here, or this fails: IBKR's Client Portal backend is `IBKR_CPAPI` in both.
#[test]
fn every_row_const_of_either_table_is_paired() {
    let connections: BTreeSet<&str> = PRODUCTS.iter().map(|(_, _, (name, _), _)| *name).collect();
    let timings: BTreeSet<&str> = PRODUCTS.iter().map(|(.., (name, _))| *name).collect();
    assert_eq!(connections, row_consts(include_str!("venue_connection.rs"), "VenueConnection"));
    assert_eq!(timings, row_consts(include_str!("venue_timings.rs"), "VenueTimings"));
}

/// **A product's two rows describe one loop**: what the connection row says runs a lane is what its timings row
/// declares waits for. A lane on the shared pump declares the idle watchdog and the ladder the pump runs; a session the
/// gatewayhub rebuilds (`Supervised`) declares no keepalive and no idle limit, because the SDK or the native client keeps
/// it and the supervisor's probe watches it; and a market lane's own loop declares a redial, and no other lane does but a
/// pump lane, whose redial the pump reads from here (`crates/vike-bridge-core/src/websocket_session/spec.rs`'s `row`),
/// a native session's being its exec session's. IBKR's socket row paired with the Client Portal's waits broke the last
/// two.
#[test]
fn a_products_two_rows_describe_one_loop() {
    for (venue, _, (connection_name, connection), (timings_name, timings)) in PRODUCTS {
        let who = format!("{venue} {connection_name}/{timings_name}");
        let exec = timings.exec.unwrap_or(VenueTimingsLane::NONE);
        if connection.exec.driver == VenueConnectionDriver::UserPump {
            assert!(
                exec.idle_threshold.is_some() && exec.reconnect.is_some(),
                "{who}: the shared pump runs the row's idle watchdog and ladder"
            );
        }
        if connection.exec.session_heal == Supervised {
            assert!(
                exec.keepalive_every.is_none() && exec.idle_threshold.is_none(),
                "{who}: a supervised session's liveness is not the bridge's to keep"
            );
        }
        let own_loop = matches!(connection.market.driver, VenueConnectionDriver::OwnLoop { .. });
        let pump = connection.market.driver == VenueConnectionDriver::MarketPump;
        let redials = timings.market.and_then(|lane| lane.reconnect).is_some();
        assert!(
            !own_loop || redials,
            "{who}: a loop of the bridge's own declares its market redial"
        );
        assert!(
            !redials || own_loop || pump,
            "{who}: a market redial is an own loop's, or read by the lane's pump row"
        );
    }
}
