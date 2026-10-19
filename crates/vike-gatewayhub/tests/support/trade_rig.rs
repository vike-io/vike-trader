//! The TRADE rig: a real gatewayhub server on loopback over a `ScriptedTrade` lane, the client crate's REAL
//! `TradeLink`/`TradeSession` as the peer, and the helpers every trade suite shares.
//!
//! Each caller `#[path]`-includes it (a bare `mod` would resolve beside the binary root); every includer
//! uses a different part, so the includer's `mod` declaration carries the one
//! `#[allow(dead_code, clippy::allow_attributes)]`.

use std::time::{Duration, Instant};

use vike_exec::{EventSender, Ingest};
use vike_gatewayhub::server::limits::ServerConfig;
use vike_gatewayhub::testkit::data::ScriptedData;
use vike_gatewayhub::testkit::gateway::{LoopbackGateway, TradeKit};
use vike_gatewayhub::testkit::rest::ScriptedRest;
use vike_gatewayhub::testkit::trade::ScriptedTrade;
use vike_gatewayhub_client::proto::{AdapterStart, WireCursor};
use vike_gatewayhub_client::trade::{TradeAccount, TradeLink, TradeSession, TradeTimings};
use vike_model::AccountTier;
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

/// The account every test leases unless it says otherwise: the default `binance` account at `demo`.
pub fn account(client_id: &str) -> TradeAccount {
    TradeAccount {
        client_id: client_id.to_string(),
        venue: "binance".to_string(),
        label: None,
        tier: AccountTier::Demo,
        route_key: "binance".to_string(),
    }
}

/// What the core's lane received: a polling reader over the `event_channel` receiver.
pub struct Lane {
    poll: Box<dyn FnMut() -> Option<Ingest> + Send>,
}

/// A fresh core lane: the sender the client delivers to (already `routed("binance")`, as a mount makes
/// it) and the reader a test drains.
pub fn lane() -> (EventSender, Lane) {
    let (tx, mut rx) = vike_exec::event_channel(4096);
    (tx.routed("binance"), Lane { poll: Box::new(move || rx.try_recv().ok()) })
}

impl Lane {
    /// Everything that arrived so far.
    pub fn drain(&mut self) -> Vec<Ingest> {
        let mut out = Vec::new();
        while let Some(i) = (self.poll)() {
            out.push(i);
        }
        out
    }

    /// Wait until `n` messages matching `keep` arrived (collected in order), or `within` passes.
    pub fn collect_until(
        &mut self,
        within: Duration,
        n: usize,
        keep: impl Fn(&Ingest) -> bool,
    ) -> Vec<Ingest> {
        let deadline = Instant::now() + within;
        let mut out = Vec::new();
        loop {
            while let Some(i) = (self.poll)() {
                if keep(&i) {
                    out.push(i);
                }
            }
            if out.len() >= n || Instant::now() >= deadline {
                return out;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
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
    let (events, lane) = lane();
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

/// Poll `cond` until it holds or `within` passes; whether it held.
pub fn wait_until(within: Duration, mut cond: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + within;
    loop {
        if cond() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
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
