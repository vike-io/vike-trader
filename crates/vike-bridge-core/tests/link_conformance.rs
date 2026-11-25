//! **The LINK conformance harness**: every venue connection on the shared private-stream pump is driven through the
//! connection contract's fault scenarios with its venue's REAL decoder, history replay, keepalive payload and open,
//! and its row's REAL durations on a virtual clock (`vike_bridge_core::pump_clock`'s `VirtualClock`, so a 60 s
//! silence costs microseconds and every run is deterministic). The third harness, beside `bridge_conformance.rs` (the
//! exec contract) and `market_data_conformance.rs` (the L2 book).
//!
//! ## The roster IS the table
//! Every row comes from `vike_venues::venue_connection`'s `connection_for` over `vike_model::VENUES`; no venue list is
//! kept here. A venue whose spot and perp exec answers differ is two rows (binance), every other venue one exec row
//! and one market row. A row's SHAPE decides what happens to it:
//! - an exec lane on the shared pump (`VenueConnectionDriver::UserPump`) must be a covered `LinkVenue`
//!   (`link_conformance/venues.rs`), and an impl on any other row is stale;
//! - a market lane on the shared market pump (`VenueConnectionDriver::MarketPump`) is covered where it has a
//!   `MarketLinkVenue` (`link_conformance/market.rs`: S5 and S11 through the data-client engine, or the venue's own
//!   session body, where the engine's dial is the lane's socket);
//! - an exec lane that is its own loop or a native session is PROVEN ON ITS SEAM where the venue's own suite drives its
//!   real code over a seam of its own (`link_conformance/sessions.rs`: each cell names the tests that pin it, which CI
//!   runs in their own crate and this harness finds at run time), and an entry on any other row is stale;
//! - every other row (an own loop or a native session with no such seam, no lane, a market lane with no market impl)
//!   is DEFERRED with the reason its shape gives: the row's driver, site and `why`;
//! - a scaffolded row (`VenueConnection::NOT_DECLARED`, what `just new-venue` writes) fails
//!   `link_conformance_roster_is_exhaustive` until the bridge's connection shape is declared.
//!
//! What this harness proves is ARMED on: an exec row's `conformance` (`VenueConnectionConformance`) is `Proven` exactly
//! when it is covered or proven on its seam (`the_tables_conformance_is_the_harness_answer`, both ways), and a `live`
//! account of an `Unproven` venue does not run (decision 0156, amended 2026-11-22).
//!
//! Within a covered row the row decides which scenarios apply (its recovery kind, its timings, its path report: S2 and
//! S5 read the pump's stream link) and the impl answers the few its venue code decides (what expires, its exec loop,
//! its keepalive, its dial, and any cell its lane's shape answers itself, `LinkVenue::cell`): each cell runs, does not
//! apply (with why), is proven on its seam by the venue's own suite where no scripted stream reaches it (the tests it
//! names, `Cell::Proven`), or is DEFERRED (with the shape that does not fit, or the defect it found).
//!
//! ## Scenarios (the connection plan's §5; S13 scheduled closures and S14 mass reconnect are not this harness' yet)
//! - **S1 drop mid-session**: the re-open comes the row's first reconnect wait after the drop, `on_reconnect` fires
//!   once, the replay schedule runs (one replay `RESYNC_SETTLE` after the re-open and one more `history_lag` later where
//!   the row declares one; a subscribe snapshot per open on a snapshot venue; on a cursor venue a read past the cursor
//!   per open, or a re-send from the cursor its REAL open asked from), reconcile is nudged as the row says, and
//!   the fill folds once in a REAL `vike_execution::Engine`.
//! - **S2 server close with a code** (hyperliquid's testnet `1000 Expired`): the order path's typed state goes
//!   `Connected -> Connecting -> Connected`, never `Error`; the close code is in the words; the pump says it at `info`.
//! - **S3 announced close** (binance spot's `serverShutdown`, okx's `64008`, deribit's `platform_state` maintenance), on a
//!   row that says `Handled`: the venue's REAL notice hook reads the venue-native frame on a session past the ladder's
//!   reset, and the pump reconnects at once: `Connecting` naming the notice, never `Error`, `on_reconnect` once at the
//!   notice, the replay schedule, `info` lines only, the fill folded once. A row that says `Ignored` is a declared gap,
//!   printed by `coverage_matrix`; every scenario runs the impl's hook, as production does.
//! - **S4 expiry**: a key the venue refuses at the (re)login gives up at once (one open, state `Error`), never a
//!   reconnect loop; a listenKey the venue calls expired is minted again by the REAL re-open after a reopen wait; a
//!   venue with no login answer (polymarket) reopens on the doubling ladder from the row's first wait.
//! - **S5 silent stall**: the idle watchdog trips the row's `idle_threshold` after the last frame (never under twice
//!   the keepalive), state `Error`, then `Connected` on the reopened session: never a latched `Error`. On a venue whose
//!   ladder spares a silence (`LinkVenue::ladder`, alpaca's: no documented heartbeat cadence) the trip is a reopen,
//!   `Connecting` at `info`, never `Error`.
//! - **S6 a fill on neither socket during the gap**: recovered exactly once by the row's recovery kind.
//! - **S7 lagging history**: a fill the history lists `history_lag` late comes back by the second replay; one listed
//!   later, or on a row with no `history_lag`, is left to reconcile (a declared gap, pinned here).
//! - **S8 busy commands**: a command a second for three gap-sentinel windows; the sentinel replays once per window.
//! - **S9 reconcile race** (a snapshot venue): the venue's own reconcile nudge comes once, after the re-open's subscribe
//!   snapshot is decoded, never at the re-open. Its persistence half is vike-core's, venue-neutral.
//! - **S10 double delivery**: the stream and the replay (or the subscribe snapshot) name one fill with the same trade
//!   id and symbol, and it folds once in a REAL `Engine` (the reconcile read's fill join is `bridge_conformance.rs`'s
//!   `FillJoin` row).
//! - **S11 keepalive cadence**: the venue's frame at the row's `keepalive_every`, or a pong to every server ping.
//! - **S12 dial bound**: the venue's REAL open is cut at its window (the row's `dial_timeout`, or `CONNECT_10S`) by
//!   `vike_bridge_core::silent_peer`'s peer. Wall clock: a socket.
//!
//! `coverage_matrix` (`--nocapture`) prints every row, every cell and the declared gaps. Its own binary: it holds
//! bybit's kept list and declares polymarket's process-wide egress, which `tests/integration.rs`'s cells read without.

use std::collections::BTreeSet;

use vike_venues::venue_connection::{
    IBKR_CPAPI, VenueConnection, VenueConnectionAnnouncedClose, VenueConnectionConformance,
    VenueConnectionDriver, VenueConnectionRecovery, connection_for,
};
use vike_venues::venue_rate_limits::VenueRateLimitsMarket;
use vike_venues::venue_timings::{VenueTimingsLane, timings_for};

#[path = "bridge_conformance/fake_private_websocket.rs"]
#[cfg(test)]
mod fake_private_websocket;
#[path = "link_conformance/market.rs"]
#[cfg(test)]
mod market;
#[path = "link_conformance/scenarios.rs"]
#[cfg(test)]
mod scenarios;
#[path = "link_conformance/sessions.rs"]
#[cfg(test)]
mod sessions;
#[path = "link_conformance/venues.rs"]
#[cfg(test)]
mod venues;

use scenarios::Cell;
use sessions::{SessionCell, SessionRow};
use venues::LinkVenue;

/// Which half of a venue's connection a row is.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Lane {
    /// The order and fill stream.
    Exec,
    /// The live market feed.
    Market,
}

/// One row of the harness: a venue's lane as the table answers it. `market` is `Some` only where the table splits
/// the venue's exec lane by market.
#[derive(Clone, Copy, Debug)]
struct Row {
    venue: &'static str,
    market: Option<VenueRateLimitsMarket>,
    lane: Lane,
}

impl Row {
    /// The table's answer for the row.
    fn connection(&self) -> VenueConnection {
        connection_for(self.venue, self.market.unwrap_or(VenueRateLimitsMarket::Spot))
    }

    /// The row's exec timings (`vike_venues::venue_timings`), the source of every duration a scenario waits.
    fn timings(&self) -> Option<VenueTimingsLane> {
        timings_for(self.venue, self.market.unwrap_or(VenueRateLimitsMarket::Spot)).exec
    }

    fn label(&self) -> String {
        let lane = match self.lane {
            Lane::Exec => "exec",
            Lane::Market => "market",
        };
        match self.market {
            Some(VenueRateLimitsMarket::Spot) => format!("{}/spot {lane}", self.venue),
            Some(VenueRateLimitsMarket::Perp) => format!("{}/perp {lane}", self.venue),
            None => format!("{} {lane}", self.venue),
        }
    }

    /// What the harness does with the row, by its shape.
    fn shape(&self) -> Shape {
        let connection = self.connection();
        let (driver, why) = match self.lane {
            Lane::Exec => (connection.exec.driver, connection.exec.why),
            Lane::Market => (connection.market.driver, connection.market.why),
        };
        let on_its_seam = self.session().is_some();
        match (self.lane, driver) {
            (Lane::Exec, VenueConnectionDriver::UserPump) => Shape::Covered,
            (Lane::Market, VenueConnectionDriver::MarketPump) if market::is_covered(self.venue) => {
                Shape::Covered
            }
            (
                Lane::Exec,
                VenueConnectionDriver::OwnLoop { .. } | VenueConnectionDriver::NativeSession { .. },
            ) if on_its_seam => Shape::OnItsSeam,
            (Lane::Market, VenueConnectionDriver::MarketPump) => Shape::Deferred(format!(
                "a market lane on the shared market pump (`run_market`) with no `MarketLinkVenue` \
                 (`link_conformance/market.rs`) to hand the engine its real plan; {why}"
            )),
            (_, VenueConnectionDriver::OwnLoop { site }) => Shape::Deferred(format!(
                "its own loop (`{site}`), not the shared pump: no `run_user` stream seam to script; {why}"
            )),
            (_, VenueConnectionDriver::NativeSession { site }) => Shape::Deferred(format!(
                "a native session (`{site}`): an SDK, process or actor owns the socket and the bridge sees only its \
                 status, so there is no frame seam; {why}"
            )),
            (_, VenueConnectionDriver::No { why }) => {
                Shape::Deferred(format!("no such lane: {why}"))
            }
            (lane, driver) => {
                Shape::Misdeclared(format!("a {lane:?} lane cannot run on {driver:?}"))
            }
        }
    }

    /// Whether `venue` is this row's impl.
    fn is(&self, venue: &dyn LinkVenue) -> bool {
        self.lane == Lane::Exec && self.venue == venue.venue() && self.market == venue.market()
    }

    /// Whether `session` is this row's entry proven on its own seam.
    fn is_on_its_seam(&self, session: &SessionRow) -> bool {
        self.lane == Lane::Exec && self.market.is_none() && self.venue == session.venue
    }

    /// The entry proven on its own seam that names this row, if one does.
    fn session(&self) -> Option<&'static SessionRow> {
        sessions::ROWS.iter().find(|session| self.is_on_its_seam(session))
    }
}

/// What a row's shape asks of the harness.
enum Shape {
    /// A covered impl: an exec lane on the shared pump, or a market lane with a market impl.
    Covered,
    /// An own loop or a native session whose cells the venue's own suites prove over its seam
    /// (`link_conformance/sessions.rs`).
    OnItsSeam,
    /// Not on the pump these scenarios script, with the reason.
    Deferred(String),
    /// A driver the lane cannot have: the table is wrong.
    Misdeclared(String),
}

/// Every row, from the table: per roster venue its exec lane (split by market where the table answers the two
/// markets apart) and its market lane.
fn rows() -> Vec<Row> {
    let mut rows = Vec::new();
    for &venue in vike_model::VENUES {
        let spot = connection_for(venue, VenueRateLimitsMarket::Spot);
        let perp = connection_for(venue, VenueRateLimitsMarket::Perp);
        if spot.exec == perp.exec {
            rows.push(Row { venue, market: None, lane: Lane::Exec });
        } else {
            for market in [VenueRateLimitsMarket::Spot, VenueRateLimitsMarket::Perp] {
                rows.push(Row { venue, market: Some(market), lane: Lane::Exec });
            }
        }
        rows.push(Row { venue, market: None, lane: Lane::Market });
    }
    rows
}

/// Each covered row with its impl.
fn covered_rows() -> Vec<(Row, Box<dyn LinkVenue>)> {
    let rows = rows();
    venues::covered()
        .into_iter()
        .filter_map(|venue| rows.iter().find(|row| row.is(&*venue)).map(|row| (*row, venue)))
        .collect()
}

/// The scenarios of this harness (the connection plan's §5 numbering).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Scenario {
    Drop,
    ServerClose,
    AnnouncedClose,
    Expiry,
    Stall,
    GapFill,
    LaggingHistory,
    BusyCommands,
    ReconcileRace,
    DoubleDelivery,
    Keepalive,
    DialBound,
}

const SCENARIOS: [Scenario; 12] = [
    Scenario::Drop,
    Scenario::ServerClose,
    Scenario::AnnouncedClose,
    Scenario::Expiry,
    Scenario::Stall,
    Scenario::GapFill,
    Scenario::LaggingHistory,
    Scenario::BusyCommands,
    Scenario::ReconcileRace,
    Scenario::DoubleDelivery,
    Scenario::Keepalive,
    Scenario::DialBound,
];

impl Scenario {
    fn label(self) -> &'static str {
        match self {
            Self::Drop => "S1 drop mid-session",
            Self::ServerClose => "S2 server close with a code",
            Self::AnnouncedClose => "S3 announced close",
            Self::Expiry => "S4 expiry",
            Self::Stall => "S5 silent stall",
            Self::GapFill => "S6 fill on neither socket",
            Self::LaggingHistory => "S7 lagging history",
            Self::BusyCommands => "S8 busy commands",
            Self::ReconcileRace => "S9 reconcile race",
            Self::DoubleDelivery => "S10 double delivery",
            Self::Keepalive => "S11 keepalive cadence",
            Self::DialBound => "S12 dial bound",
        }
    }

    fn short(self) -> &'static str {
        self.label().split(' ').next().expect("a numbered label")
    }
}

/// Polymarket's process-wide egress, declared direct (the body of the crate's own test helper; a dev edge declares
/// through its tests, `crates/vike-ops/tests/venues/polymarket_egress_declared_gate.rs`'s rule): its user channel's open
/// would otherwise dial the default SOCKS tunnel, not the loopback peer.
fn declare_polymarket_direct_egress() {
    let _ =
        vike_polymarket::declare_egress(vike_polymarket::EgressSettings::from_lookup(|field| {
            (*field == vike_venues::venue_fields::SOCKS_PROXY).then(|| "none".to_string())
        }));
    assert_eq!(
        vike_polymarket::proxy_url(),
        None,
        "polymarket's egress must be direct to dial a loopback peer: another declaration won in this process"
    );
}

/// Run `scenario` on every covered row where it runs (the market rows' through `market::failures`), one thread a row,
/// and fail naming every row it failed on.
fn run_all(scenario: Scenario) {
    let rows = covered_rows();
    let mut failures: Vec<String> = std::thread::scope(|scope| {
        let runs: Vec<_> = rows
            .iter()
            .filter(|(row, venue)| matches!(scenarios::cell(&**venue, row, scenario), Cell::Runs))
            .map(|(row, venue)| {
                let thread = std::thread::Builder::new()
                    .name(format!("{} × {}", row.label(), scenario.short()))
                    .spawn_scoped(scope, move || scenarios::run(&**venue, row, scenario))
                    .expect("spawn a cell");
                (row, thread)
            })
            .collect();
        runs.into_iter()
            .filter_map(|(row, thread)| match thread.join() {
                Ok(Ok(())) => None,
                Ok(Err(why)) => Some(format!("[{} × {}] {why}", row.label(), scenario.label())),
                Err(_) => Some(format!(
                    "[{} × {}] panicked (its message is above)",
                    row.label(),
                    scenario.label()
                )),
            })
            .collect()
    });
    failures.extend(market::failures(scenario));
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn s1_a_drop_reopens_after_the_rows_wait_and_replays_once_on_every_covered_row() {
    run_all(Scenario::Drop);
}

#[test]
fn s2_a_server_close_is_a_reopen_never_an_error_on_every_covered_row() {
    run_all(Scenario::ServerClose);
}

#[test]
fn s3_an_announced_close_reconnects_at_once_on_every_handled_row() {
    run_all(Scenario::AnnouncedClose);
}

#[test]
fn s4_an_expired_or_refused_key_never_loops_on_every_covered_row() {
    run_all(Scenario::Expiry);
}

#[test]
fn s5_a_silent_stall_trips_at_the_rows_idle_limit_and_heals_on_every_covered_row() {
    run_all(Scenario::Stall);
}

#[test]
fn s6_a_fill_the_gap_held_is_recovered_once_on_every_covered_row() {
    run_all(Scenario::GapFill);
}

#[test]
fn s7_a_lagging_history_is_read_on_the_rows_schedule_on_every_covered_row() {
    run_all(Scenario::LaggingHistory);
}

#[test]
fn s8_busy_commands_never_postpone_the_gap_sentinel_on_every_covered_row() {
    run_all(Scenario::BusyCommands);
}

#[test]
fn s9_a_snapshot_venues_reconcile_nudge_follows_the_reopens_snapshot() {
    run_all(Scenario::ReconcileRace);
}

#[test]
fn s10_one_fill_delivered_twice_folds_once_on_every_covered_row() {
    run_all(Scenario::DoubleDelivery);
}

#[test]
fn s11_the_keepalive_runs_at_the_rows_cadence_on_every_covered_row() {
    run_all(Scenario::Keepalive);
}

#[test]
fn s12_a_silent_peer_is_cut_at_the_dial_window_on_every_covered_row() {
    run_all(Scenario::DialBound);
}

/// Every row is covered, proven on its own seam, DEFERRED with a reason, or a failure: a scaffolded or unknown row, an
/// exec lane on the shared pump with no impl, an impl on a row that is not one, an entry proven on its seam that names
/// no own loop or native session (or two entries for one row), a lane with a driver it cannot have, and a one-row venue
/// whose two markets' exec timings differ (it would need a row each).
#[test]
fn link_conformance_roster_is_exhaustive() {
    let rows = rows();
    let venues = venues::covered();
    let mut problems = Vec::new();
    for row in &rows {
        let connection = row.connection();
        if connection == VenueConnection::NOT_DECLARED || connection == VenueConnection::UNKNOWN {
            problems.push(format!(
                "{}: the connection table has no declared row (`vike_venues::venue_connection`): read the bridge's \
                 connection shape and declare it",
                row.label()
            ));
            continue;
        }
        match row.shape() {
            Shape::Covered if row.lane == Lane::Market => {
                let impls =
                    market::covered().iter().filter(|venue| venue.venue() == row.venue).count();
                if impls != 1 {
                    problems.push(format!(
                        "{}: {impls} `MarketLinkVenue` impls; it needs exactly one (`link_conformance/market.rs`)",
                        row.label()
                    ));
                }
            }
            Shape::Covered => {
                let impls = venues.iter().filter(|venue| row.is(&***venue)).count();
                if impls != 1 {
                    problems.push(format!(
                        "{}: an exec lane on the shared pump has {impls} `LinkVenue` impl(s); it needs exactly one \
                         (`link_conformance/venues.rs`'s `covered`)",
                        row.label()
                    ));
                }
            }
            Shape::OnItsSeam => {
                let entries = sessions::ROWS.iter().filter(|s| row.is_on_its_seam(s)).count();
                if entries != 1 {
                    problems.push(format!(
                        "{}: {entries} entries proven on its seam (`link_conformance/sessions.rs`'s `ROWS`); it \
                         needs exactly one",
                        row.label()
                    ));
                }
            }
            Shape::Deferred(why) if why.trim().is_empty() => {
                problems.push(format!("{}: DEFERRED with no reason", row.label()));
            }
            Shape::Deferred(_) => {}
            Shape::Misdeclared(why) => problems.push(format!("{}: {why}", row.label())),
        }
        if row.lane == Lane::Exec && row.market.is_none() {
            let spot = timings_for(row.venue, VenueRateLimitsMarket::Spot).exec;
            let perp = timings_for(row.venue, VenueRateLimitsMarket::Perp).exec;
            if spot != perp {
                problems.push(format!(
                    "{}: one connection row, but its spot and perp exec timings differ; split the row",
                    row.label()
                ));
            }
        }
    }
    for venue in &venues {
        if !rows.iter().any(|row| row.is(&**venue) && matches!(row.shape(), Shape::Covered)) {
            problems.push(format!(
                "a `LinkVenue` impl for {} {:?} matches no exec lane on the shared pump: a stale impl",
                venue.venue(),
                venue.market()
            ));
        }
    }
    for venue in market::covered() {
        let row = Row { venue: venue.venue(), market: None, lane: Lane::Market };
        if !rows.iter().any(|known| known.venue == row.venue)
            || !matches!(row.shape(), Shape::Covered)
        {
            problems.push(format!(
                "a `MarketLinkVenue` impl for {} matches no market lane on the shared market pump: a stale impl",
                venue.venue()
            ));
        }
    }
    for session in sessions::ROWS {
        if !rows
            .iter()
            .any(|row| row.is_on_its_seam(session) && matches!(row.shape(), Shape::OnItsSeam))
        {
            problems.push(format!(
                "the entry proven on its seam for {} names no exec lane that is its own loop or a native session: a \
                 stale entry",
                session.venue
            ));
        }
    }
    assert!(!venues.is_empty(), "no covered row: the harness checks nothing");
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}

/// **The table's `conformance` is this harness' answer, in both directions** (decision 0156, amended 2026-11-22: a
/// `live` account of an `Unproven` venue does not run, `crates/vike-config/src/account_arming.rs`'s `account_tier`).
/// An exec row is `Proven` exactly when it is a covered `LinkVenue` or proven on its own seam, and an `Unproven` one
/// says why. The cpapi backend is no row here (the roster is `connection_for`), so it is `Unproven`.
#[test]
fn the_tables_conformance_is_the_harness_answer() {
    let covered = covered_rows();
    let mut problems = Vec::new();
    let mut proven_rows = 0;
    for row in rows().iter().filter(|row| row.lane == Lane::Exec) {
        let proven = match row.shape() {
            Shape::Covered => {
                covered.iter().any(|(r, _)| r.venue == row.venue && r.market == row.market)
            }
            Shape::OnItsSeam => true,
            Shape::Deferred(_) | Shape::Misdeclared(_) => false,
        };
        match (row.connection().exec.conformance, proven) {
            (VenueConnectionConformance::Proven, true) => proven_rows += 1,
            (VenueConnectionConformance::Unproven { why }, false) if !why.trim().is_empty() => {}
            (VenueConnectionConformance::Unproven { .. }, false) => {
                problems.push(format!("{}: `Unproven` with no reason", row.label()));
            }
            (VenueConnectionConformance::Proven, false) => problems.push(format!(
                "{}: the table says `Proven` and this harness proves no cell of it, so it would arm `live`: \
                 declare it `Unproven` with why, or cover it",
                row.label()
            )),
            (VenueConnectionConformance::Unproven { why }, true) => problems.push(format!(
                "{}: this harness covers it and the table says `Unproven` ({why}): declare it `Proven`",
                row.label()
            )),
        }
    }
    if IBKR_CPAPI.exec.conformance == VenueConnectionConformance::Proven {
        problems.push(
            "ibkr cpapi: no row of this harness drives it, and the table says `Proven`".into(),
        );
    }
    assert!(proven_rows > 0, "no proven row: the check has gone inert");
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}

/// Every cell proven on its own seam names tests that exist: each file (from the repository root) holds a `fn` of each
/// name, read at run time, so a proof that is renamed or deleted reddens here and never leaves its cell claiming it. That
/// holds for a row proven on its seam and for a covered row's cell its venue's suite proves (`Cell::Proven`). A row
/// lists a scenario at most once, and every cell says what it pins or why nothing does.
#[test]
fn every_cell_proven_on_its_seam_names_a_test_that_exists() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut problems = Vec::new();
    // (row, scenario, the tests, what they pin)
    let mut proven: Vec<(String, Scenario, &'static [sessions::Proof], &'static str)> = Vec::new();
    for session in sessions::ROWS {
        for scenario in SCENARIOS {
            let listed = session.cells.iter().filter(|(listed, _)| *listed == scenario).count();
            if listed > 1 {
                problems.push(format!(
                    "{}: {} is listed {listed} times",
                    session.venue,
                    scenario.short()
                ));
            }
        }
        for (scenario, cell) in session.cells {
            match cell {
                SessionCell::Proven { by, pins } => {
                    proven.push((session.venue.to_string(), *scenario, by, pins));
                }
                SessionCell::NotApplicable(why) | SessionCell::Deferred(why) => {
                    if why.trim().is_empty() {
                        problems.push(format!(
                            "{} × {}: no words",
                            session.venue,
                            scenario.short()
                        ));
                    }
                }
            }
        }
    }
    let (covered, markets) = (covered_rows(), market::covered());
    for scenario in SCENARIOS {
        for (row, venue) in &covered {
            if let Cell::Proven { by, pins } = scenarios::cell(&**venue, row, scenario) {
                proven.push((row.label(), scenario, by, pins));
            }
        }
        for venue in &markets {
            if let Cell::Proven { by, pins } = market::cell(&**venue, scenario) {
                proven.push((format!("{} market", venue.venue()), scenario, by, pins));
            }
        }
    }
    for (row, scenario, by, pins) in &proven {
        if by.is_empty() {
            problems.push(format!("{row} × {}: proven by no test", scenario.short()));
        }
        for proof in *by {
            let path = root.join(proof.file);
            let holds = std::fs::read_to_string(&path)
                .is_ok_and(|source| source.contains(&format!("fn {}(", proof.test)));
            if !holds {
                problems.push(format!(
                    "{row} × {}: `{}` holds no `fn {}`",
                    scenario.short(),
                    proof.file,
                    proof.test
                ));
            }
        }
        if pins.trim().is_empty() {
            problems.push(format!("{row} × {}: no words", scenario.short()));
        }
    }
    assert!(!sessions::ROWS.is_empty(), "no row proven on its seam: the test checks nothing");
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}

/// `by`'s tests, as `coverage_matrix` names them.
fn named(by: &[sessions::Proof]) -> String {
    by.iter()
        .map(|proof| format!("`{}`'s `{}`", proof.file, proof.test))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Prints every row and cell (`--nocapture`) and the declared gaps the rows carry. Asserts nothing the scenario tests
/// and the roster test do not.
#[test]
fn coverage_matrix() {
    let covered = covered_rows();
    let mut lines = vec![format!(
        "LINK conformance — rows of `vike_venues::venue_connection` x scenarios\n{:<24}{}",
        "row",
        SCENARIOS.iter().map(|s| format!("{:<5}", s.short())).collect::<String>()
    )];
    let mut notes = Vec::new();
    let mut mark = |row: &Row, scenario: Scenario, cell: Cell| match cell {
        Cell::Runs => "ok",
        Cell::NotApplicable(why) => {
            notes.push(format!("  {} × {}: n/a — {why}", row.label(), scenario.short()));
            "n/a"
        }
        Cell::Deferred(why) => {
            notes.push(format!("  {} × {}: DEFERRED — {why}", row.label(), scenario.short()));
            "DEF"
        }
        Cell::Proven { by, pins } => {
            notes.push(format!(
                "  {} × {}: proven on its seam — {pins} ({})",
                row.label(),
                scenario.short(),
                named(by)
            ));
            "ok"
        }
    };
    for (row, venue) in &covered {
        let mut line = format!("{:<24}", row.label());
        for scenario in SCENARIOS {
            line.push_str(&format!(
                "{:<5}",
                mark(row, scenario, scenarios::cell(&**venue, row, scenario))
            ));
        }
        lines.push(line);
    }
    for venue in market::covered() {
        let row = Row { venue: venue.venue(), market: None, lane: Lane::Market };
        let mut line = format!("{:<24}", row.label());
        for scenario in SCENARIOS {
            line.push_str(&format!("{:<5}", mark(&row, scenario, market::cell(&*venue, scenario))));
        }
        lines.push(line);
    }
    let mut seams = Vec::new();
    for row in rows().into_iter().filter(|row| matches!(row.shape(), Shape::OnItsSeam)) {
        let Some(session) = row.session() else { continue };
        let recovery = row.connection().exec.recovery;
        seams.push(format!("  {}: {}", row.label(), session.seam));
        let label = format!("{} (seam)", row.label());
        let mut line = format!("{label:<24}");
        for scenario in SCENARIOS {
            let (mark, note) = match session.cell(scenario, recovery) {
                SessionCell::Proven { by, pins } => {
                    ("ok", format!("proven on its seam — {pins} ({})", named(by)))
                }
                SessionCell::NotApplicable(why) => ("n/a", format!("n/a — {why}")),
                SessionCell::Deferred(why) => ("DEF", format!("DEFERRED — {why}")),
            };
            notes.push(format!("  {} × {}: {note}", row.label(), scenario.short()));
            line.push_str(&format!("{mark:<5}"));
        }
        lines.push(line);
    }
    lines.push("\nthe seams the rows proven on their own seam are proven over:".to_string());
    lines.extend(seams);
    lines.push(
        "\ncells that do not run (and the proofs of the rows on their own seam):".to_string(),
    );
    lines.extend(notes);
    lines.push("\nDEFERRED rows (recorded, not faked):".to_string());
    for row in rows() {
        if let Shape::Deferred(why) = row.shape() {
            lines.push(format!("  {}: {why}", row.label()));
        }
    }
    lines.push("\ndeclared gaps of the covered rows:".to_string());
    let mut gaps = BTreeSet::new();
    for (row, _) in &covered {
        let exec = row.connection().exec;
        if let VenueConnectionAnnouncedClose::Ignored { notice } = exec.announced_close {
            gaps.insert(format!(
                "  {}: announces `{notice}` and nothing parses it (S3 does not run)",
                row.label()
            ));
        }
        let lag = row.timings().and_then(|timings| timings.history_lag);
        if exec.recovery == VenueConnectionRecovery::HistoryWindow && lag.is_none() {
            gaps.insert(format!(
                "  {}: one replay after a reopen (no `history_lag`): a fill the history lists later is reconcile's",
                row.label()
            ));
        }
    }
    lines.extend(gaps);
    println!("{}", lines.join("\n"));
    assert_eq!(
        covered.len(),
        venues::covered().len(),
        "an impl matches no row: see the roster test"
    );
}
