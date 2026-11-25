//! `VenueConnection` — the STATIC per-venue CONNECTION SHAPE table (step 1 of the playbook: today's reality).
//!
//! The sibling of [`crate::venue_timings`]: that table says how LONG each wait of a connection is, this one says
//! what is not a duration. Per lane (`exec`, the order and fill stream; `market`, the live market feed): which loop
//! runs it, how what a gap could have lost is read back, whether a reopen pokes reconcile, what the bridge reports
//! about its path, who rebuilds a session that ends, what the venue announces before it cuts the connection, and
//! which calendar holds a login back. Until this table these facts were hardcoded venue lists in the tradehub, one
//! adapter flag per bridge and a market-pump class in `vike-bridge-core`, each true and none side by side.
//!
//! **Step 1 declares, it changes nothing** (`crates/vike-venues/CLAUDE.md`'s "Per-venue capability maps (the
//! playbook)"): every roster venue is a NAMED row whose `why` cites the code each answer was read from, a verbatim
//! matrix pin and a completeness test over `vike_model::VENUES` hold it.
//!
//! **Four answers are READ; for the first three the copies they replaced are gone** (connection-sync B2, the answers
//! unchanged; the fourth is D3's):
//! - `nudges_reconcile` is who gets the reconnect trigger: the gatewayhub hands it to a start only on such a row
//!   (`crates/vike-gatewayhub/src/trade/adapters/contract.rs`'s `contract_parts`) and states it on the arming table
//!   (`VenueAdapterDeclarationView::takes_reconcile_trigger`, the wire field, unchanged), where each bridge's own
//!   declaration flag used to say it;
//! - `path_report` is which venue states a path to the tradehub's path watch and in which words its return is said
//!   (`crates/vike-tradehub/src/venue_backend/gateway/venue_link.rs`'s `reports_path` and `back_words`), where two
//!   venue lists there used to, and which venue's `trade health` `gateway-link` row can only be UNKNOWN for want of a
//!   report (`crates/vike-client/src/health/links.rs`'s `venue_session_checks`), where the wire crate's list used to;
//! - `session_heal` is which session the gatewayhub supervises: only a `Supervised` row is asked for the bridge's
//!   probe (`crates/vike-gatewayhub/src/trade/adapters/bridge_trade.rs`'s `TradeLaneBridge`, its `link_watch`);
//! - `conformance` is whether the link harness proves the venue: a `live` account of an `Unproven` one does not run
//!   (`crates/vike-config/src/account_arming.rs`'s `account_tier`, decision 0156 amended 2026-11-22), and the harness
//!   holds the row to its own answer (`crates/vike-bridge-core/tests/link_conformance.rs`'s
//!   `the_tables_conformance_is_the_harness_answer`).
//!
//! Each is a per-VENUE question, answered by [`venue_wide_exec_for`]. The market `driver` and `subscribe_frame` are
//! read too (B3, 2026-11-22), by the shared market pump: `vike_bridge_core::websocket_session::spec::row` serves a
//! `MarketPump` lane only, with this row's frame and its timing row's waits, where a per-venue class table of the
//! pump's own used to say it. The rest is decided elsewhere and cross-pinned to the row, so it cannot drift before
//! its consumer is moved onto it:
//! - `scheduled_closure` against [`crate::session::session_for`], and a declared `history_lag` against the
//!   recovery that replays history (both here).
//!
//! A contradiction is pinned, not fixed: aster, deribit and polymarket run the same pump as binance and poke no
//! reconcile. Changing an answer is step 2, one venue and one row per PR.
//!
//! **Keyed like [`crate::venue_timings::timings_for`]**: `(venue, market)`. binance's spot and perp exec lanes are
//! different sockets with different notices, so it is the one market-SPLIT venue; aster has a row per market that is
//! one row, and every other venue answers both markets alike. IBKR's row is its default product
//! (`crate::IbkrBackend::Socket`); the Client Portal backend is [`IBKR_CPAPI`], declared beside it and read by name.
//! Each row const describes ONE product, and so does the timings row it pairs with: `venue_connection_tests.rs`'s
//! `PRODUCTS` is the one pairing, and a const of either table that is no pair there fails its test, so a venue that
//! grows a second product declares a row in each table.
//!
//! An unknown venue string answers [`VenueConnection::UNKNOWN`], a scaffolded one [`VenueConnection::NOT_DECLARED`];
//! no roster venue may ride either (completeness-tested). Like every table here it is compiled: no setting, no
//! environment variable (decisions 0095 and 0111).

use crate::venue_rate_limits::VenueRateLimitsMarket;

/// Which loop runs a lane.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VenueConnectionDriver {
    /// The shared private-stream pump, `vike_bridge_core::websocket_session::user`'s `run_user` (spawned through
    /// `spawn_user_with_resync`, or `spawn_user` for a stream with no REST replay; run directly where its frames feed
    /// the bridge's own exec loop, [`IBKR_CPAPI`]). Exec lanes only.
    UserPump,
    /// The shared market pump, `vike_bridge_core::websocket_session::market`'s `run_market`: the lane's knobs are
    /// `vike_bridge_core::websocket_session::spec::row`'s, read from this row and its timing row ([`IBKR_CPAPI`]'s are
    /// that module's `IBKR_CPAPI`). Market lanes only.
    MarketPump,
    /// A loop of the bridge's own; `site` is the file (or folder) it lives in, from the repository root.
    OwnLoop { site: &'static str },
    /// A session an SDK, a process or an actor owns, of which the bridge sees only the status; `site` as above.
    NativeSession { site: &'static str },
    /// No such lane: `why` states the venue's shape.
    No { why: &'static str },
}

/// How an exec lane reads back what a gap could have lost (fills and venue-side order ends).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VenueConnectionRecovery {
    /// The reopen resumes from the last id seen: the venue re-sends exactly the gap.
    Cursor,
    /// Every subscribe re-delivers the current state as a snapshot.
    SnapshotOnSubscribe,
    /// A REST read of the venue's history after a reopen: a window of rows, which can lag the fill.
    HistoryWindow,
    /// The venue or its SDK replays to the new session, or the bridge re-asks it on the new session.
    SessionReplay,
    /// Nothing reads the gap back: only the next reconcile pass sees it.
    ReconcileOnly,
}

/// What a bridge reports about its venue path while the lease stays up (the tradehub's path watch asks it).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VenueConnectionPathReport {
    /// The shared pump's stream link, `vike_bridge_core::websocket_session::user::stream_link`.
    SharedPump,
    /// The bridge's order session with the venue, from its own session state.
    OwnSession,
    /// The venue's account session, which it can end on a socket that stays up (cTrader).
    AccountSession,
    /// An owned child process the bridge restarts (`crate::venue_wiring::VenueWiring::SidecarExecutionReconcile`).
    Sidecar,
    /// Nothing: the bridge states no path.
    NotReported,
}

/// Who opens a session again once it has ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VenueConnectionSessionHeal {
    /// The lane's own loop, actor, SDK or sidecar supervisor.
    OwnReconnect,
    /// The gatewayhub rebuilds the whole adapter: the bridge's `VenueAdapter::link_probe` answers a probe.
    Supervised,
}

/// A frame by which the venue says it is ending the session, other than the close itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VenueConnectionAnnouncedClose {
    /// None is known on this lane, and the bridge parses none.
    NoneKnown,
    /// The venue sends `notice` and the bridge does nothing with it: the close is met after it happens.
    Ignored { notice: &'static str },
    /// The venue sends `notice` and the bridge ends or renews the session on it.
    Handled { notice: &'static str },
}

/// The calendar that holds a login back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VenueConnectionScheduledClosure {
    /// None: the venue's session (`crate::session::session_for`) is not the FX week.
    NoClosure,
    /// The FX week (`crate::session::fx_login_closed_until`): no start and no reopen while it is closed.
    FxWeek,
    /// The FX week holds the account's start, but the lane's own reopen loop does not consult it.
    FxWeekStartOnly,
}

/// Whether the LINK conformance harness proves an exec lane's connection contract. Declared here because arming
/// cannot read a test binary, and held to the harness' own answer in both directions by
/// `crates/vike-bridge-core/tests/link_conformance.rs`'s `the_tables_conformance_is_the_harness_answer`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VenueConnectionConformance {
    /// A covered `LinkVenue` on the shared pump, or a row proven on its own seam (`link_conformance/sessions.rs`).
    Proven,
    /// The harness proves no cell of it; `why` names the shape it lacks.
    Unproven { why: &'static str },
}

/// The order and fill stream of a venue.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VenueConnectionExec {
    /// Which loop runs it.
    pub driver: VenueConnectionDriver,
    /// How a gap is read back.
    pub recovery: VenueConnectionRecovery,
    /// Whether a reopen pokes the reconnect-triggered reconcile pass: the gatewayhub hands the trigger only to a start
    /// whose row says so (a per-venue answer, [`venue_wide_exec_for`]).
    pub nudges_reconcile: bool,
    /// What the bridge reports about its path: the tradehub's path watch reads it (a per-venue answer).
    pub path_report: VenueConnectionPathReport,
    /// Who opens an ended session again: the gatewayhub supervises only a `Supervised` row (a per-venue answer).
    pub session_heal: VenueConnectionSessionHeal,
    /// What the venue announces before it cuts the connection, and whether the bridge acts on it.
    pub announced_close: VenueConnectionAnnouncedClose,
    /// The calendar that holds a login back.
    pub scheduled_closure: VenueConnectionScheduledClosure,
    /// Whether the link harness proves the lane: an account of an `Unproven` venue arms `demo`, never `live`
    /// (`crates/vike-config/src/account_arming.rs`'s `account_tier`; a per-venue answer).
    pub conformance: VenueConnectionConformance,
    /// Where each answer was read (`path`'s `symbol`), and the lane's known gaps.
    pub why: &'static str,
}

/// The live market feed of a venue.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VenueConnectionMarket {
    /// Which loop runs it.
    pub driver: VenueConnectionDriver,
    /// Whether the shared market pump replays a per-session SUBSCRIBE FRAME on each open (`true`), or the
    /// subscription rides the URL or the lane's own connect hook or link (`false`; also every lane off the pump). A
    /// shape, not a duration: the timing row's `subscribe_acknowledgement_timeout` is the wait for its
    /// acknowledgement. The market pump reads it here (`crates/vike-bridge-core/src/websocket_session/spec.rs`'s
    /// `row`).
    pub subscribe_frame: bool,
    /// What the venue announces before it cuts a market socket, and whether the bridge acts on it (on the shared
    /// market pump, through the lane's notice hook, `vike_bridge_core::websocket_session::market`'s
    /// `WebsocketSessionMarketNotice`).
    pub announced_close: VenueConnectionAnnouncedClose,
    /// Where it was read, and the lane's known gaps.
    pub why: &'static str,
}

/// The connection shape of ONE `(venue, market)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VenueConnection {
    /// The order and fill stream.
    pub exec: VenueConnectionExec,
    /// The live market feed.
    pub market: VenueConnectionMarket,
}

impl VenueConnection {
    /// A scaffolded venue's row (`just new-venue`): nothing read from a bridge yet.
    pub const NOT_DECLARED: VenueConnection =
        nothing("not declared yet: the connection shape waits to be read from the bridge");

    /// An unknown venue string's answer; no roster venue rides it.
    pub const UNKNOWN: VenueConnection = nothing("not a roster venue");
}

/// The row of a venue with no declared lane: no loop, nothing read back, nothing reported, no closure.
const fn nothing(why: &'static str) -> VenueConnection {
    VenueConnection {
        exec: VenueConnectionExec {
            driver: VenueConnectionDriver::No { why },
            recovery: VenueConnectionRecovery::ReconcileOnly,
            nudges_reconcile: false,
            path_report: VenueConnectionPathReport::NotReported,
            session_heal: VenueConnectionSessionHeal::OwnReconnect,
            announced_close: VenueConnectionAnnouncedClose::NoneKnown,
            scheduled_closure: VenueConnectionScheduledClosure::NoClosure,
            conformance: VenueConnectionConformance::Unproven { why },
            why,
        },
        market: VenueConnectionMarket {
            driver: VenueConnectionDriver::No { why },
            subscribe_frame: false,
            announced_close: VenueConnectionAnnouncedClose::NoneKnown,
            why,
        },
    }
}

// ---------------------------------------------------------------------------------------------
// The per-venue rows. Each `why` cites the code its answers were read from.
// ---------------------------------------------------------------------------------------------

/// The exec lane every venue on the shared private-stream pump starts from: a history replay after a reopen, the
/// pump's stream link as its path, its own reconnect, no notice, no closure. Each row states its own `why` and every
/// answer it does not share.
const SHARED_PUMP: VenueConnectionExec = VenueConnectionExec {
    driver: VenueConnectionDriver::UserPump,
    recovery: VenueConnectionRecovery::HistoryWindow,
    nudges_reconcile: false,
    path_report: VenueConnectionPathReport::SharedPump,
    session_heal: VenueConnectionSessionHeal::OwnReconnect,
    announced_close: VenueConnectionAnnouncedClose::NoneKnown,
    scheduled_closure: VenueConnectionScheduledClosure::NoClosure,
    conformance: VenueConnectionConformance::Proven,
    why: "",
};

/// A market lane on the shared market pump that replays no subscribe frame (the URL, the connect hook or the lane's
/// link subscribes), with what its venue announces before a cut.
const fn market_pump(
    announced_close: VenueConnectionAnnouncedClose,
    why: &'static str,
) -> VenueConnectionMarket {
    VenueConnectionMarket {
        driver: VenueConnectionDriver::MarketPump,
        subscribe_frame: false,
        announced_close,
        why,
    }
}

/// A market lane on the shared market pump that subscribes by a frame replayed on each open.
const fn market_pump_with_frame(
    announced_close: VenueConnectionAnnouncedClose,
    why: &'static str,
) -> VenueConnectionMarket {
    VenueConnectionMarket { subscribe_frame: true, ..market_pump(announced_close, why) }
}

/// Binance **spot**: the WS-API private stream.
pub const BINANCE_SPOT: VenueConnection = VenueConnection {
    exec: VenueConnectionExec {
        nudges_reconcile: true,
        announced_close: VenueConnectionAnnouncedClose::Handled { notice: "serverShutdown" },
        why: "the WS-API stream runs on the shared pump \
              (`crates/bridges/binance/src/execution/user_data/spot.rs`'s `open_binance_user_data_websocket`); a \
              reopen replays the order and trade history \
              (`crates/bridges/binance/src/family/execution/resync.rs`'s `map_history`) and pokes reconcile \
              (`crates/vike-bridge-core/src/websocket_session/user.rs`'s `run_user_resync`); the venue's \
              `{\"event\":{\"e\":\"serverShutdown\",\"E\":…,\"T\":…}}`, sent when the server is about to shut \
              down (the Binance spot WebSocket API documentation's Server Shutdown section: establish a new \
              connection as soon as possible), is read by `crates/bridges/binance/src/execution/user_data/spot.rs`'s \
              `server_shutdown_notice` and the pump reconnects at once \
              (`crates/vike-bridge-core/src/websocket_session/user.rs`'s `run_user_with_notice`); the 24-hour cut is met \
              after it happens",
        ..SHARED_PUMP
    },
    market: market_pump(
        VenueConnectionAnnouncedClose::Handled { notice: "serverShutdown" },
        "the kline and aggTrade lanes on the shared market pump, URL-subscribed, the server's 20 s ping answered \
         and binance's one 540 s idle limit, sized for its futures books \
         (`crates/vike-bridge-core/src/websocket_session/spec.rs`'s `row`, the idle-limit audit of 2026-11-22: no \
         quiet spot socket was silent past 20.1 s in 900 s); the spot streams' \
         `{\"e\":\"serverShutdown\",\"E\":…,\"T\":…}` (`!serverShutdown` on a combined stream), sent when the server \
         is about to shut down (the Binance spot WebSocket Streams documentation's Server Shutdown section: establish \
         a new connection as soon as possible), is read on both lanes by \
         `crates/bridges/binance/src/feed/mod.rs`'s `market_server_shutdown_notice` and the pump reconnects at once \
         (`crates/vike-bridge-core/src/websocket_session/market.rs`'s `run_market_on`); depth rides its own driver, \
         which reads no notice, and the 24-hour cut is met after it happens",
    ),
};

/// Binance **USDⓈ-M futures**: the listenKey stream.
pub const BINANCE_PERP: VenueConnection = VenueConnection {
    exec: VenueConnectionExec {
        nudges_reconcile: true,
        why: "the listenKey stream runs on the shared pump through the family's \
              (`crates/bridges/binance/src/family/execution/user_data.rs`'s `spawn_user_data_with_resync`), which \
              also ends a session on `listenKeyExpired`; a reopen replays the perp history \
              (`crates/bridges/binance/src/family/execution/resync.rs`'s `map_perp_history`) and pokes reconcile; \
              no notice is parsed, and the 24-hour cut is met after it happens",
        ..SHARED_PUMP
    },
    market: market_pump(
        VenueConnectionAnnouncedClose::NoneKnown,
        "the kline, mark and trade lanes on the shared market pump, URL-subscribed, the server's 180 s ping answered \
         and a 540 s idle limit (`crates/vike-bridge-core/src/websocket_session/spec.rs`'s `row`, the idle-limit \
         audit of 2026-11-22: a quiet USDⓈ-M `@trade` socket was silent at most 135.3 s and a quiet COIN-M \
         `@aggTrade` one 180.1 s, a ping each, in 900 s); the USDⓈ-M and COIN-M streams document \
         no announced close, so their lanes arm no hook (`crates/bridges/binance/src/feed/mod.rs`'s \
         `market_notice`); depth rides its own driver",
    ),
};

/// Aster, both markets: one listenKey pump, the binance family's.
pub const ASTER: VenueConnection = VenueConnection {
    exec: VenueConnectionExec {
        why: "both markets run the binance family's listenKey pump \
              (`crates/bridges/binance/src/family/execution/user_data.rs`'s `spawn_user_data_with_resync`); a \
              reopen replays the history (`crates/bridges/aster/src/execution/resync.rs`'s `map_aster_history` \
              and `map_aster_perp_history`) but pokes no reconcile (binance's pump, another answer); no close is \
              announced: its futures user-data stream documents no notice before its 24-hour cut, and \
              `serverShutdown` is binance spot's WS-API event, not a listenKey stream's",
        ..SHARED_PUMP
    },
    market: market_pump(
        VenueConnectionAnnouncedClose::NoneKnown,
        "binance's kline, mark and aggTrade lanes, run by aster on the shared market pump with a \
         `LIST_SUBSCRIPTIONS` keepalive every 60 s and a 180 s idle limit \
         (`crates/vike-bridge-core/src/websocket_session/spec.rs`'s `row`, the idle-limit audit \
         of 2026-11-22): its perp host cut a quiet `@aggTrade` socket 240 s after the client's last frame, seven \
         times over three quiet sockets, and the answered keepalive kept one up 600 s with no inbound gap over \
         60.8 s; its streams \
         document only the 24-hour cut, no notice (`crates/bridges/aster/src/feed/mod.rs`'s `DataClientSpecAster` \
         arms none)",
    ),
};

/// Bybit V5 (spot and linear perp).
pub const BYBIT: VenueConnection = VenueConnection {
    exec: VenueConnectionExec {
        nudges_reconcile: true,
        why: "the private stream runs on the shared pump (`crates/bridges/bybit/src/execution/user_data.rs`'s \
              `spawn_perp`), which replays `execution/list` and the closed orders after a reopen and a second \
              time the timing row's `history_lag` later (`crates/bridges/bybit/src/execution/resync.rs`'s \
              `map_bybit_history`), then pokes reconcile \
              (`crates/vike-bridge-core/src/websocket_session/user.rs`'s `run_user_resync`)",
        ..SHARED_PUMP
    },
    market: market_pump_with_frame(
        VenueConnectionAnnouncedClose::NoneKnown,
        "the kline, mark and trades lanes on the shared market pump with a subscribe-ack watchdog, the \
         `{\"op\":\"ping\"}` every 20 s and a 60 s idle limit (`crates/vike-bridge-core/src/websocket_session/spec.rs`'s \
         `row`, connection-sync C6): measured 2026-11-22, a quiet `publicTrade` socket with no ping was cut 59.7 s \
         after its last frame and a quiet `kline.1` gapped 56.0 s, while a pinged one ran 600 s with no inbound gap \
         over 20.7 s; tick and depth ping on their own; bybit documents no close notice (V5 WebSocket \"Connect\": a \
         socket may be disconnected at any time), so no lane arms a hook (`crates/bridges/bybit/src/feed/bar.rs`'s \
         `bars_plan`)",
    ),
};

/// OKX v5 (spot and SWAP).
pub const OKX: VenueConnection = VenueConnection {
    exec: VenueConnectionExec {
        nudges_reconcile: true,
        announced_close: VenueConnectionAnnouncedClose::Handled { notice: "64008" },
        why: "two private sockets on the shared pump, the `orders` channel and the business socket's \
              `orders-algo` (`crates/bridges/okx/src/execution/user_data.rs`'s `spawn_okx_stream`), each \
              replaying its history after a reopen (`crates/bridges/okx/src/execution/resync.rs`'s \
              `map_okx_history` and `map_okx_algo_history`) and poking reconcile; the venue's \
              `{\"event\":\"notice\",\"code\":\"64008\",\"msg\":\"The connection will soon be closed for a service \
              upgrade. Please reconnect.\"}`, sent 60 s before the upgrade (OKX API v5, WebSocket \"Notification\"), is \
              read on both sockets by `crates/bridges/okx/src/execution/user_data.rs`'s `service_upgrade_notice` and \
              the pump reconnects at once (`crates/vike-bridge-core/src/websocket_session/user.rs`'s \
              `run_user_with_notice`)",
        ..SHARED_PUMP
    },
    market: market_pump_with_frame(
        VenueConnectionAnnouncedClose::Handled { notice: "64008" },
        "the kline, mark and trades lanes on the shared market pump, as bybit's: a subscribe-ack watchdog, the raw \
         `ping` every 15 s and a 60 s idle limit (`crates/vike-bridge-core/src/websocket_session/spec.rs`'s `row`, \
         connection-sync C6): okx cuts a socket that pushed nothing for 30 s, measured 2026-11-22 on a quiet \
         `trades` channel (close 4004 \"No data received in 30s.\" 30.0 s after its last frame), while a pinged one \
         ran 600 s with no inbound gap over 15.5 s; the exec lane's `64008` notice, sent on every endpoint, the \
         public and business sockets too (OKX API v5, WebSocket \"Notification\"), is read on all three lanes by \
         `crates/bridges/okx/src/feed/mod.rs`'s `market_service_upgrade_notice` and the pump reconnects at once \
         (`crates/vike-bridge-core/src/websocket_session/market.rs`'s `run_market_on`); depth rides its own driver, \
         which reads no notice",
    ),
};

/// Deribit (options and perp).
pub const DERIBIT: VenueConnection = VenueConnection {
    exec: VenueConnectionExec {
        announced_close: VenueConnectionAnnouncedClose::Handled { notice: "platform_state" },
        why: "the private stream runs on the shared pump \
              (`crates/bridges/deribit/src/execution/user_data.rs`'s `spawn_deribit_user_data_with_resync`) and \
              replays the trade and order history after a reopen \
              (`crates/bridges/deribit/src/execution/resync.rs`'s `map_deribit_history`), poking no reconcile; it \
              subscribes `platform_state` in its optional call, and the venue's notification \
              `{\"method\":\"subscription\",\"params\":{\"channel\":\"platform_state\",\"data\":{\"maintenance\":true}}}`, \
              set when the maintenance break begins (Deribit API, subscription `platform_state`), is read by \
              `crates/bridges/deribit/src/execution/user_data.rs`'s `maintenance_notice` and the pump reconnects at \
              once (`crates/vike-bridge-core/src/websocket_session/user.rs`'s `run_user_with_notice`)",
        ..SHARED_PUMP
    },
    market: market_pump_with_frame(
        VenueConnectionAnnouncedClose::NoneKnown,
        "one keyless JSON-RPC socket per subscription on the shared market pump, with the private side's \
         `public/test` ping and idle limit (`crates/vike-bridge-core/src/websocket_session/spec.rs`'s `row`); none \
         subscribes `platform_state`, so its maintenance notice reaches the exec lane only and no market lane arms a \
         hook (`crates/bridges/deribit/src/feed/mod.rs`'s `live_plan`)",
    ),
};

/// Polymarket CLOB.
pub const POLYMARKET: VenueConnection = VenueConnection {
    exec: VenueConnectionExec {
        why: "the user channel runs on the shared pump \
              (`crates/bridges/polymarket/src/execution/user_data/mod.rs`'s \
              `spawn_polymarket_user_data_with_resync_tracked`) and replays the REST trades after a reopen \
              (`crates/bridges/polymarket/src/execution/resync.rs`'s `map_polymarket_history`), rows that carry \
              no venue stamp, so the history floor lets each through; no reconcile poke",
        ..SHARED_PUMP
    },
    market: market_pump_with_frame(
        VenueConnectionAnnouncedClose::NoneKnown,
        "book, quotes and trades on the shared market pump with a PING keepalive and a 30 s idle limit \
         (`crates/vike-bridge-core/src/websocket_session/spec.rs`'s `row`), one shard loop per socket whose dial is \
         handed in (`crates/bridges/polymarket/src/feed/mod.rs`'s `run_shard_on`); the RTDS socket keeps its own \
         loop; the CLOB and RTDS sockets document no close notice (that shard loop arms none)",
    ),
};

/// Hyperliquid.
pub const HYPERLIQUID: VenueConnection = VenueConnection {
    exec: VenueConnectionExec {
        recovery: VenueConnectionRecovery::SnapshotOnSubscribe,
        nudges_reconcile: true,
        why: "`userFills` and `orderUpdates` on the shared pump with no REST replay \
              (`crates/bridges/hyperliquid/src/execution/user_data.rs`'s `spawn_hyperliquid_user_data`, over \
              `spawn_user`): every subscribe re-sends the `userFills` snapshot, and the reopen pokes reconcile \
              once the new session's snapshot is decoded, never at the re-open \
              (`crates/bridges/hyperliquid/src/execution/user_data.rs`'s `ReconnectNudge`); the testnet's \
              `1000 Expired` close every ~10 minutes is an ordinary close",
        ..SHARED_PUMP
    },
    market: market_pump_with_frame(
        VenueConnectionAnnouncedClose::NoneKnown,
        "candle, bbo, trades and l2Book sessions on the shared market pump with the 30 s ping and a 60 s \
         idle limit (`crates/vike-bridge-core/src/websocket_session/spec.rs`'s `row`); no close is announced: \
         disconnects \"may happen periodically and without announcement\" (Hyperliquid API, WebSocket)",
    ),
};

/// Alpaca (US equities and crypto).
pub const ALPACA: VenueConnection = VenueConnection {
    exec: VenueConnectionExec {
        driver: VenueConnectionDriver::UserPump,
        recovery: VenueConnectionRecovery::Cursor,
        nudges_reconcile: false,
        path_report: VenueConnectionPathReport::SharedPump,
        session_heal: VenueConnectionSessionHeal::OwnReconnect,
        announced_close: VenueConnectionAnnouncedClose::NoneKnown,
        scheduled_closure: VenueConnectionScheduledClosure::NoClosure,
        conformance: VenueConnectionConformance::Proven,
        why: "the SSE body over `ureq` on the shared pump (`spawn_user`, read as server-sent-event lines; \
              `crates/bridges/alpaca/src/execution/user_data.rs`'s `spawn_trade_events`), whose open asks \
              `since_id` the last event id (`crates/bridges/alpaca/src/execution/user_data.rs`'s \
              `TradeEventsCursor`), so the venue re-sends exactly the gap; until the first event an open is bare, \
              so a gap before this process saw any event is reconcile's; it reports the pump's stream \
              (`crates/vike-gatewayhub/src/trade/adapters/bridge_trade.rs`'s `venue_link`) and pokes no reconcile",
    },
    market: VenueConnectionMarket {
        driver: VenueConnectionDriver::MarketPump,
        subscribe_frame: false,
        announced_close: VenueConnectionAnnouncedClose::NoneKnown,
        why: "a `SessionOn` lane per subscription on the shared market pump \
              (`crates/bridges/alpaca/src/feed/client.rs`'s `DataClientSpecAlpaca`), every lane of an asset class \
              on that stream's one socket (`crates/bridges/alpaca/src/feed/connection.rs`'s `RouteLink`): the \
              bounded dial and the idle watchdog of the pump row",
    },
};

/// OANDA v20 (FX/CFD).
pub const OANDA: VenueConnection = VenueConnection {
    exec: VenueConnectionExec {
        driver: VenueConnectionDriver::UserPump,
        recovery: VenueConnectionRecovery::Cursor,
        nudges_reconcile: false,
        path_report: VenueConnectionPathReport::SharedPump,
        session_heal: VenueConnectionSessionHeal::OwnReconnect,
        announced_close: VenueConnectionAnnouncedClose::NoneKnown,
        scheduled_closure: VenueConnectionScheduledClosure::FxWeekStartOnly,
        conformance: VenueConnectionConformance::Proven,
        why: "the chunked-HTTP transactions stream runs on the shared pump as newline-delimited lines \
              (`crates/bridges/oanda/src/execution/user_data.rs`'s `stream_transactions`, over `run_user`); its \
              open reads `transactions/sinceid` from the lane's floor before the first line \
              (`crates/bridges/oanda/src/execution/resync.rs`'s `map_transactions_since`) and it reports the pump's \
              stream link (`crates/vike-gatewayhub/src/trade/adapters/bridge_trade.rs`'s `venue_link`), where a 401 \
              inside the ladder's refusal wait reads as a reopen; the FX \
              week holds the account's start (`crates/vike-gatewayhub/src/trade/adapters/bridge_trade.rs`'s \
              `fx_closed_words`), not the pump's reopen",
    },
    market: market_pump(
        VenueConnectionAnnouncedClose::NoneKnown,
        "quotes on the shared market pump, the chunked-HTTP pricing stream read as newline-delimited lines \
         (`crates/bridges/oanda/src/feed/tick.rs`'s `quotes_main`, over `run_market_on`; its knobs the timing row's \
         market lane, read by `crates/vike-bridge-core/src/websocket_session/spec.rs`'s `row`), and bars by a \
         candles REST poll beside it: the venue has no market-data socket",
    ),
};

/// IG (FX/CFD).
pub const IG: VenueConnection = VenueConnection {
    exec: VenueConnectionExec {
        driver: VenueConnectionDriver::OwnLoop {
            site: "crates/bridges/ig/src/execution/user_data.rs",
        },
        recovery: VenueConnectionRecovery::SnapshotOnSubscribe,
        nudges_reconcile: false,
        path_report: VenueConnectionPathReport::OwnSession,
        session_heal: VenueConnectionSessionHeal::OwnReconnect,
        announced_close: VenueConnectionAnnouncedClose::Handled { notice: "LOOP / END" },
        scheduled_closure: VenueConnectionScheduledClosure::FxWeek,
        conformance: VenueConnectionConformance::Proven,
        why: "its own Lightstreamer loop (`crates/bridges/ig/src/execution/user_data.rs`'s `run_once`) \
              subscribes with `LS_snapshot=true`, so a reopen re-delivers the trade state; it ends the session on \
              the server's `LOOP` or `END` line and dials again, each retry waited out to the FX open \
              (`crates/bridges/ig/src/config.rs`'s `fx_held_pause`), and it reports its order session \
              (`crates/vike-gatewayhub/src/trade/adapters/bridge_trade.rs`'s `venue_link`)",
    },
    market: market_pump(
        VenueConnectionAnnouncedClose::Handled { notice: "LOOP / END" },
        "the TLCP market feed on the shared market pump, its `create_session` handshake in the connect hook \
         (`crates/vike-bridge-core/src/websocket_session/spec.rs`'s `row`); the shared link's \
         reader ends the connection on the server's `LOOP` or `END` line and tells every lane, which dials again on \
         its own wait, not the pump's notice hook (`crates/bridges/ig/src/feed/link.rs`'s `route_line`)",
    ),
};

/// cTrader (FX/CFD): one protobuf session actor for orders and data.
pub const CTRADER: VenueConnection = VenueConnection {
    exec: VenueConnectionExec {
        driver: VenueConnectionDriver::NativeSession {
            site: "crates/bridges/ctrader/src/transport/connection",
        },
        recovery: VenueConnectionRecovery::SessionReplay,
        nudges_reconcile: false,
        path_report: VenueConnectionPathReport::AccountSession,
        session_heal: VenueConnectionSessionHeal::OwnReconnect,
        announced_close: VenueConnectionAnnouncedClose::Handled {
            notice: "ProtoOAAccountDisconnectEvent",
        },
        scheduled_closure: VenueConnectionScheduledClosure::FxWeek,
        conformance: VenueConnectionConformance::Proven,
        why: "one session actor redials by itself on the shared reconnect ladder \
              (`crates/bridges/ctrader/src/transport/connection/session.rs`'s `reconnect_with_backoff`, which \
              holds an attempt that would land in the closed FX week through `fx_login_held_pause`); a \
              reopened session re-reads the \
              open positions and orders and asks about what the dead socket left unanswered \
              (`crates/bridges/ctrader/src/transport/connection/reconcile.rs`'s `reconcile_after_reconnect`), \
              while a resting order's gap fill is only warned about; the account-disconnect event opens a new \
              session (`crates/bridges/ctrader/src/transport/connection/session.rs`'s `session_word`); its path \
              is the account session (`crates/vike-gatewayhub/src/trade/adapters/bridge_trade.rs`'s `venue_link`)",
    },
    market: VenueConnectionMarket {
        driver: VenueConnectionDriver::NativeSession {
            site: "crates/bridges/ctrader/src/transport/connection",
        },
        subscribe_frame: false,
        announced_close: VenueConnectionAnnouncedClose::Handled {
            notice: "ProtoOAAccountDisconnectEvent",
        },
        why: "trendbars and spots ride the same session actor as orders, resubscribed after a reconnect \
              (`crates/bridges/ctrader/src/transport/connection/reconcile.rs`'s `replay_after_reconnect`), so the \
              account-disconnect event that opens a new session \
              (`crates/bridges/ctrader/src/transport/connection/session.rs`'s `session_word`) renews them too; \
              the shared driver's success disclosure cannot reach it, and no reconcile health gate reads its \
              status (`crates/vike-tradehub/src/feeds/live.rs`'s `reconcile_feed_statuses`)",
    },
};

/// FXCM ForexConnect (FX/CFD).
pub const FXCM: VenueConnection = VenueConnection {
    exec: VenueConnectionExec {
        driver: VenueConnectionDriver::NativeSession {
            site: "crates/bridges/fxcm/src/execution/mod.rs",
        },
        recovery: VenueConnectionRecovery::SessionReplay,
        nudges_reconcile: false,
        path_report: VenueConnectionPathReport::OwnSession,
        session_heal: VenueConnectionSessionHeal::Supervised,
        announced_close: VenueConnectionAnnouncedClose::NoneKnown,
        scheduled_closure: VenueConnectionScheduledClosure::FxWeek,
        conformance: VenueConnectionConformance::Proven,
        why: "the ForexConnect SDK owns the session and reconnects it itself, its listener re-surfacing the \
              trades through the same lane (`crates/bridges/fxcm/src/execution/event_mapper.rs`'s \
              `closes_routing`); after a final state the gatewayhub rebuilds the adapter \
              (`crates/bridges/fxcm/src/account/venue_link.rs`'s `FxcmSessionProbe`, handed over by \
              `link_probe`), at the FX open while the market is closed; the shim is SDK-gated, proven by the demo \
              smoke and not CI",
    },
    market: VenueConnectionMarket {
        driver: VenueConnectionDriver::NativeSession { site: "crates/bridges/fxcm/src/feed.rs" },
        subscribe_frame: false,
        announced_close: VenueConnectionAnnouncedClose::NoneKnown,
        why: "the price lane polls the Offers table on the account's running ForexConnect reconcile session, \
              opening nothing of its own: the gatewayhub's credentialed price stream and the poller meet on \
              `crates/bridges/fxcm/src/feed.rs`'s `PriceDesk`, its pairs capped by \
              `crates/vike-gatewayhub-client/src/credentialed_limits.rs`'s `FXCM`; the SDK owns the session and its \
              reconnects, so the shared driver's success disclosure cannot reach it",
    },
};

/// Dukascopy (FX): the JForex Java sidecar.
pub const DUKASCOPY: VenueConnection = VenueConnection {
    exec: VenueConnectionExec {
        driver: VenueConnectionDriver::NativeSession {
            site: "crates/bridges/dukascopy/src/execution/mod.rs",
        },
        recovery: VenueConnectionRecovery::SessionReplay,
        nudges_reconcile: false,
        path_report: VenueConnectionPathReport::Sidecar,
        session_heal: VenueConnectionSessionHeal::OwnReconnect,
        announced_close: VenueConnectionAnnouncedClose::NoneKnown,
        scheduled_closure: VenueConnectionScheduledClosure::FxWeek,
        conformance: VenueConnectionConformance::Proven,
        why: "the JForex sidecar owns the session: it reconnects and re-scans the gap's fills itself \
              (`crates/bridges/dukascopy/jforex-bridge/src/main/java/vike/jforex/Bridge.java`'s \
              `replayAfterReconnect`), and a dead sidecar is started again on a doubling wait held to the FX \
              open (`crates/bridges/dukascopy/src/execution/mod.rs`'s `supervise`); a `SERVER_SHUTDOWN` is no \
              advance notice, the sidecar dies on it and is started again",
    },
    market: VenueConnectionMarket {
        driver: VenueConnectionDriver::NativeSession {
            site: "crates/bridges/dukascopy/src/feed.rs",
        },
        subscribe_frame: false,
        announced_close: VenueConnectionAnnouncedClose::NoneKnown,
        why: "the JForex sidecar the trade lane started streams the live ticks over its stdio, and the gatewayhub's \
              credentialed price stream meets it on `crates/bridges/dukascopy/src/feed.rs`'s `QuoteBus`, its \
              instruments capped by `crates/vike-gatewayhub-client/src/credentialed_limits.rs`'s `DUKASCOPY`; the \
              sidecar owns the session and its reconnects, so the shared driver's success disclosure cannot reach \
              it (history stays the keyless `.bi5` files, T+1)",
    },
};

/// Interactive Brokers on its default product, the socket backend (IB Gateway over `ibapi`).
pub const IBKR: VenueConnection = VenueConnection {
    exec: VenueConnectionExec {
        driver: VenueConnectionDriver::NativeSession {
            site: "crates/bridges/vike-ibkr/src/transport/socket.rs",
        },
        recovery: VenueConnectionRecovery::SessionReplay,
        nudges_reconcile: false,
        path_report: VenueConnectionPathReport::OwnSession,
        session_heal: VenueConnectionSessionHeal::Supervised,
        announced_close: VenueConnectionAnnouncedClose::NoneKnown,
        scheduled_closure: VenueConnectionScheduledClosure::NoClosure,
        conformance: VenueConnectionConformance::Proven,
        why: "the `ibapi` client owns the IB Gateway session and the bridge never dials it again; the \
              gatewayhub rebuilds the adapter (`crates/bridges/vike-ibkr/src/adapter.rs`'s `link_probe`), and \
              the new session re-requests the open orders and the day's executions \
              (`crates/bridges/vike-ibkr/src/execution/mod.rs`'s `StreamResync`); the gateway's daily logoff is \
              met after the fact, with no calendar of its own",
    },
    market: VenueConnectionMarket {
        driver: VenueConnectionDriver::NativeSession {
            site: "crates/bridges/vike-ibkr/src/feed/socket",
        },
        subscribe_frame: false,
        announced_close: VenueConnectionAnnouncedClose::NoneKnown,
        why: "the socket backend's quotes and bars ride the `ibapi` client's callbacks over the same gateway \
              session (`crates/bridges/vike-ibkr/src/feed/socket`, on no shared pump); the \
              shared driver's success disclosure cannot reach it, and no reconcile health gate reads its status \
              (`crates/vike-tradehub/src/feeds/live.rs`'s `reconcile_feed_statuses`)",
    },
};

/// Interactive Brokers on the Client Portal Gateway backend (`crate::IbkrBackend::Cpapi`), not deployed. Read by
/// name: [`connection_for`] answers the default product, [`IBKR`].
pub const IBKR_CPAPI: VenueConnection = VenueConnection {
    exec: VenueConnectionExec {
        driver: VenueConnectionDriver::UserPump,
        recovery: VenueConnectionRecovery::HistoryWindow,
        nudges_reconcile: false,
        path_report: VenueConnectionPathReport::SharedPump,
        session_heal: VenueConnectionSessionHeal::OwnReconnect,
        announced_close: VenueConnectionAnnouncedClose::NoneKnown,
        scheduled_closure: VenueConnectionScheduledClosure::NoClosure,
        // shortcut: arming reads `connection_for("ibkr")`, the socket row, whatever the backend; this answer is the
        // harness', read by nothing until arming knows the backend.
        conformance: VenueConnectionConformance::Unproven {
            why: "the link harness' roster is `connection_for`, which answers IBKR's socket product, so no row \
                  drives the Client Portal backend",
        },
        why: "the shared pump run straight into the bridge's exec loop \
              (`crates/bridges/vike-ibkr/src/transport/cpapi/mod.rs`'s `pump_loop`, with a `tic` and the REST \
              tickle every 30 s and an idle limit) sends `StreamResync` on every reopen, answered by a REST read of \
              the open orders and the trades, the only path a cpapi fill has (the socket carries no fill row); it \
              writes the pump's stream link, which the gatewayhub does not read for ibkr (it reads the socket \
              backend's cell: `crates/vike-gatewayhub/src/trade/adapters/bridge_trade.rs`'s `venue_link`); no \
              re-authentication, and `link_probe` answers `None` for this backend",
    },
    market: market_pump(
        VenueConnectionAnnouncedClose::NoneKnown,
        "the `smd` topic on one Client Portal socket per data client, multiplexed by conId, on the shared market pump \
         (`crates/bridges/vike-ibkr/src/feed/cpapi/lane.rs`'s `run_on`, over `run_market_on`; its knobs \
         `crates/vike-bridge-core/src/websocket_session/spec.rs`'s `IBKR_CPAPI`, which reads this product's timing \
         row): the lane's stream subscribes every held conId at each open and applies the subscribe and unsubscribe \
         commands between reads, the REST tickle renews the session every 30 s, a 60 s idle limit over the gateway's \
         10 s `system` heartbeat ends a silent socket, a fixed 2 s redial; the gateway documents no close notice",
    ),
};

// vike:new-venue:row /// TODO(new-venue: {venue}): read the venue's connection shape from its bridge; `NOT_DECLARED` is
// vike:new-venue:row /// the honest scaffold, and a real classification only because it is NAMED.
// vike:new-venue:row pub const {VENUE}: VenueConnection = VenueConnection::NOT_DECLARED;
// vike:new-venue:row

/// The registry: the declared [`VenueConnection`] of a canonical `(venue, market)` pair, one arm per
/// `vike_model::VENUES` entry, mirroring [`crate::venue_timings::timings_for`]. Unknown venue strings answer
/// [`VenueConnection::UNKNOWN`].
#[must_use]
pub fn connection_for(venue: &str, market: VenueRateLimitsMarket) -> VenueConnection {
    match (venue, market) {
        ("binance", VenueRateLimitsMarket::Spot) => BINANCE_SPOT,
        ("binance", VenueRateLimitsMarket::Perp) => BINANCE_PERP,
        ("aster", _) => ASTER,
        ("bybit", _) => BYBIT,
        ("okx", _) => OKX,
        ("deribit", _) => DERIBIT,
        ("polymarket", _) => POLYMARKET,
        ("hyperliquid", _) => HYPERLIQUID,
        ("oanda", _) => OANDA,
        ("ig", _) => IG,
        ("fxcm", _) => FXCM,
        ("dukascopy", _) => DUKASCOPY,
        ("ibkr", _) => IBKR,
        ("ctrader", _) => CTRADER,
        ("alpaca", _) => ALPACA,
        // vike:new-venue:row ("{venue}", _) => {VENUE}, // TODO(new-venue: {venue}): split into (Spot, Perp) arms if its markets run different exec sockets
        _ => VenueConnection::UNKNOWN,
    }
}

/// The exec lane of `venue` for a consumer that asks per VENUE, not per market: its `nudges_reconcile`,
/// `path_report`, `session_heal`, `scheduled_closure` and `conformance` are one answer on both markets
/// (`venue_connection_tests.rs`'s `the_venue_wide_answers_agree_across_markets`), so the spot row gives it. Its
/// driver, recovery and notice are per market: ask [`connection_for`] for those.
#[must_use]
pub fn venue_wide_exec_for(venue: &str) -> VenueConnectionExec {
    connection_for(venue, VenueRateLimitsMarket::Spot).exec
}

#[path = "venue_connection_tests.rs"]
#[cfg(test)]
mod venue_connection_tests;
