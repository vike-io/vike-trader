//! The TRADE rig: a real gatewayhub server on loopback over a `ScriptedTrade` lane, the client crate's REAL
//! `TradeLink`/`TradeSession` as the peer, and the helpers every trade suite shares.
//!
//! Compiled once, in the testkit (`crate::testkit`'s module doc), for every trade suite.

use std::time::Duration;

use crate::server::limits::ServerConfig;
use crate::testkit::data::ScriptedData;
use crate::testkit::gateway::{LoopbackGateway, TradeKit};
use crate::testkit::rest::ScriptedRest;
use crate::testkit::trade::ScriptedTrade;
use crate::trade::TradeConfig;
use vike_exec::Ingest;
use vike_gatewayhub_client::proto::{AdapterStart, WireCursor};
use vike_gatewayhub_client::test_support::fake_trade_peer::{Lane, account, lane_with_capacity};
use vike_gatewayhub_client::trade::{TradeLink, TradeSession, TradeTimings};
use vike_model::OrderRequest;
use vike_model::events::{Event, OrderAccepted};

/// A started gateway and the lane behind it.
pub struct Rig {
    /// The server. Declared first so it drops AFTER the clients a test declares later.
    pub gw: LoopbackGateway,
    /// The scripted lane (a handle on the one the server dispatches to).
    pub lane: ScriptedTrade,
}

/// A rig serving `binance` with a default lane, the production server configuration and no state
/// directory.
pub fn rig() -> Rig {
    rig_with(ScriptedTrade::new(&["binance"]), ServerConfig::PRODUCTION, TradeKit::default())
}

/// A rig over `lane` with `config` and `kit`.
pub fn rig_with(lane: ScriptedTrade, config: ServerConfig, kit: TradeKit) -> Rig {
    let gw = LoopbackGateway::start_trade_with(
        ScriptedData::new(&[]),
        ScriptedRest::new(),
        lane.clone(),
        config,
        kit,
    );
    Rig { gw, lane }
}

/// The production configuration with a short heartbeat and fence, so a test never waits on production's
/// numbers. The heartbeat is the SMALLEST the real client honours (it clamps what it is told into
/// `GW_TRADE_HEARTBEAT_MIN..`, 500 ms, and pings at that period), and the read floor is above three of
/// them: a faster daemon deadline would drop a healthy real client. A test with a RAW peer that wants a
/// ghost released sooner sets its own.
pub fn quick_config() -> ServerConfig {
    let mut config = ServerConfig::PRODUCTION;
    config.timings.trade_heartbeat = Duration::from_millis(500);
    config.timings.trade_read_floor = Duration::from_secs(2);
    config.timings.trade_fence_drain = Duration::from_millis(1_500);
    config
}

/// Trade timings short enough that a dead link is called dead in a fraction of a second.
pub fn fast_trade_timings() -> TradeTimings {
    TradeTimings {
        read_floor: Duration::from_millis(600),
        request_reply: Duration::from_secs(10),
        recon_stall_floor: Duration::from_millis(900),
        detach_flush: Duration::from_secs(3),
    }
}

/// The order sessions' configuration a socketless `TradeHub` test builds over: small queues and rings,
/// a 100 ms heartbeat, no state directory and no HALT file.
pub fn hub_config() -> TradeConfig {
    TradeConfig {
        heartbeat: Duration::from_millis(100),
        read_floor: Duration::from_millis(300),
        ring_events: 64,
        dedup_coids: 64,
        command_queue: 64,
        control_queue: 64,
        event_channel: 64,
        max_leases: 8,
        orphan_ttl: Duration::from_secs(600),
        recon_progress: Duration::from_millis(100),
        recon_page_bytes: 4096,
        fence_drain: Duration::from_millis(500),
        account_ccy_retry_after: crate::trade::ACCOUNT_CCY_RETRY_AFTER,
        state_dir: None,
        halt_path: None,
    }
}

/// Open a lease for `client_id` with the fast timings and a fresh core lane.
pub fn open(rig: &Rig, client_id: &str) -> (TradeSession, Lane) {
    open_with(rig, client_id, |link| link)
}

/// As [`open`], letting the test adjust the builder.
pub fn open_with(
    rig: &Rig,
    client_id: &str,
    adjust: impl FnOnce(TradeLink) -> TradeLink,
) -> (TradeSession, Lane) {
    // A deeper lane than the client suites' (4096, not 1024): the hop harnesses burst into it.
    let (events, lane) = lane_with_capacity(4096);
    let link = TradeLink::new(rig.gw.trade_link(), account(client_id), events)
        .with_timings(fast_trade_timings());
    let session = adjust(link).open().expect("the lease opens");
    (session, lane)
}

/// The adapter start every test sends (the session names the route key itself).
pub fn start_args() -> AdapterStart {
    AdapterStart {
        rid: 0,
        symbol: "BTCUSDT".into(),
        declared_legs: Vec::new(),
        recon_enabled: true,
        takes_trigger: false,
        risk: None,
        market_slippage: None,
        halt_admit: vike_model::HaltAdmit::Admit,
        withhold: None,
        account_leverage: None,
        read_account_ccy: false,
        route_key: "ignored: the session names the account's own".into(),
    }
}

/// An order the core would submit.
pub fn order(coid: &str) -> OrderRequest {
    OrderRequest {
        client_order_id: coid.to_string(),
        venue: "binance".to_string(),
        symbol: "BTCUSDT".to_string(),
        side: 1,
        qty: 1.0,
        price: Some(100.0),
        ts: 1_700_000_000_000,
        ..Default::default()
    }
}

/// An order that REDUCES: what a HALT file admits.
pub fn reducing_order(coid: &str) -> OrderRequest {
    OrderRequest { reduce_only: true, ..order(coid) }
}

/// An `Event::OrderAccepted` for `coid`: a plain venue event.
pub fn accepted(coid: &str) -> Event {
    Event::OrderAccepted(OrderAccepted {
        client_order_id: coid.to_string(),
        venue_order_id: None,
        ts: 5,
    })
}

/// The cursor an event at `seq` of `gw_epoch` leaves.
pub fn cursor(gw_epoch: u64, seq: u64) -> WireCursor {
    WireCursor { gw_epoch, seq, ts_ms: 0 }
}

/// The coids of the `Ingest::GatewayEvent`s in `ingest`, in order, as `(seq, replayed-agnostic coid)`.
pub fn gateway_coids(ingest: &[Ingest]) -> Vec<(u64, String)> {
    ingest
        .iter()
        .filter_map(|i| match i {
            Ingest::GatewayEvent(g) => match &g.event {
                Event::OrderAccepted(a) => Some((g.cursor.seq, a.client_order_id.clone())),
                _ => None,
            },
            _ => None,
        })
        .collect()
}

/// The terminal rejects in `ingest` (plain or over the gateway), as `(coid, reason)`.
pub fn rejects(ingest: &[Ingest]) -> Vec<(String, String)> {
    ingest
        .iter()
        .filter_map(|i| match i.venue_event() {
            Some(Event::OrderRejected(r)) => {
                Some((r.client_order_id.clone(), r.reason.to_string()))
            }
            _ => None,
        })
        .collect()
}

/// The non-terminal modify advisories in `ingest`, as `(coid, reason)`.
pub fn modify_advisories(ingest: &[Ingest]) -> Vec<(String, String)> {
    ingest
        .iter()
        .filter_map(|i| match i.venue_event() {
            Some(Event::OrderModifyRejected(r)) => {
                Some((r.client_order_id.clone(), r.reason.to_string()))
            }
            _ => None,
        })
        .collect()
}

/// The non-terminal cancel advisories in `ingest`, as `(coid, reason)`.
pub fn cancel_advisories(ingest: &[Ingest]) -> Vec<(String, String)> {
    ingest
        .iter()
        .filter_map(|i| match i.venue_event() {
            Some(Event::OrderCancelRejected(r)) => {
                Some((r.client_order_id.clone(), r.reason.to_string()))
            }
            _ => None,
        })
        .collect()
}
