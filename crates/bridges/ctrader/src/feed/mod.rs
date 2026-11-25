//! cTrader's market data as the shared engine's source (`vike_bridge_core::data_client::bus::DataClientBus` over
//! [`DataClientBusSpecCtrader`]): the source sends `ConnectionCommand`s over the actor's channel (`transport::connection::ActorHandle`) to
//! (un)subscribe spot and trend-bar streams. The actor decodes the inbound `SPOT_EVENT` and `GET_TRENDBARS_RES`
//! frames on its own thread into the sink it took at its handshake, and that sink is the engine's fixed sink
//! (`vike_bridge_core::data_client::bus::DataClientBusSink`), so every price meets the engine's rules on the way to
//! the client's sink. The engine counts the holders of each key and mints the ids; this file decides what a key costs
//! on the wire. Ports nothing — cTrader Open API (https://help.ctrader.com/open-api/).
//!
//! ## Ownership: single-client vs shared (F4)
//! Two construction paths, distinguished by who owns the actor thread:
//! - [`connect`] runs the handshake over a fixed sink of its own and OWNS the actor thread: the client's shutdown
//!   (or drop) sends `Shutdown` and joins it. This is the data-only path.
//! - [`DataClientBusSpecCtrader::view`] takes a cloned [`ConnectionShared`] (command channel + handshake state) and the
//!   fixed sink the actor was built with, and does NOT own the actor thread. It is a VIEW over an actor owned
//!   elsewhere — by a [`crate::transport::CtraderClient`] sharing the socket with a `ExecutionClientCtrader`. Closing it
//!   unsubscribes its own streams but leaves the socket up for the co-mounted exec client.
//!
//! ## One wire stream, however many keys hold it (decision 0193)
//! A quote key and every trend-bar key of a symbol all stand on that symbol's ONE spot subscription
//! (`ProtoOASubscribeLiveTrendbarReq` "requires subscription on the spot events"), and a second subscribe
//! of a live stream is the venue's `ALREADY_SUBSCRIBED` (`OpenApiModelMessages.proto`). The engine counts a key's
//! holders, so a key reaches the hold's `ask` on every subscribe and its `unwant` once, from its last holder; the hold
//! keeps the streams each held key stands on, so no second number can drift from it: the first key of a stream
//! writes its subscribe, the last one its unsubscribe (trend bars before the spots they need), and a release in
//! between writes nothing. This is the reading `crates/vike-datahub/src/feeds/venues.rs`'s `EXPLICIT_SHARING` row
//! stands on.
//!
//! shortcut: the count is per client, so two [`DataClientBusSpecCtrader::view`]s of one socket do not see each
//! other; production builds one client per socket (the gatewayhub's [`connect`]), move the count into the actor if
//! that ever changes.

pub mod bar;
pub mod decode;

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use vike_bridge_core::data_client::bus::{
    DataClientBus, DataClientBusSink, DataClientBusSpec, DataClientBusSpecDeliver,
    DataClientBusSpecHold, DataClientBusSpecKey,
};
use vike_data::{DataClientError, DataClientSink, require_live_verb};
use vike_model::now_ms;
use vike_venues::DataClientVerb;

use crate::transport::connection::{
    ActorHandle, ConnectionCommand, ConnectionConfig, ConnectionError, ConnectionShared, VENUE,
    connect_and_auth,
};
use crate::transport::proto::ProtoOaTrendbarPeriod;

/// How many historical bars a bars subscribe seeds via `GetTrendbars` before starting the live
/// trendbar stream (F2). Bounded, not paged — 300 bars is enough context for a chart/consumer to
/// render immediately without a full backfill; `ConnectionCommand::GetTrendbars`'s own `count` field caps
/// the venue response at this number too, so the `from_timestamp` window computed below can be generous
/// without risking an oversized reply. A reconnect's trendbar fill asks this window too
/// (`crate::feed::bar`'s module doc says why).
pub(crate) const SEED_BAR_COUNT: u32 = 300;

/// The data-only start: connect, run the handshake over a fixed sink, and return the client that OWNS the actor
/// (module doc). `sink` receives every price the engine passes.
///
/// # Errors
/// The handshake's ([`connect_and_auth`]).
pub fn connect(
    config: ConnectionConfig,
    sink: Arc<dyn DataClientSink>,
) -> Result<DataClientBus<DataClientBusSpecCtrader>, ConnectionError> {
    let relay = Arc::new(DataClientBusSink::default());
    let handle = connect_and_auth(config, Arc::clone(&relay) as Arc<dyn DataClientSink>)?;
    let source = DataClientBusSpecCtrader {
        shared: handle.shared(),
        relay,
        owner: Mutex::new(Some(handle)),
    };
    Ok(DataClientBus::on(&Arc::new(source), sink))
}

/// The streams one held key stands on: a quote key its symbol's spots, a bar key the spots and its period's trend
/// bars.
#[derive(Clone, Copy)]
enum Stream {
    Quotes { symbol_id: i64 },
    Bars { symbol_id: i64, period: ProtoOaTrendbarPeriod },
}

impl Stream {
    fn symbol_id(self) -> i64 {
        match self {
            Stream::Quotes { symbol_id } | Stream::Bars { symbol_id, .. } => symbol_id,
        }
    }
}

/// cTrader's market-data source: the actor's command channel and handshake state, the fixed sink the actor delivers
/// into, and, on the data-only path, the actor itself (module doc).
pub struct DataClientBusSpecCtrader {
    shared: ConnectionShared,
    relay: Arc<DataClientBusSink>,
    /// `Some` iff this source owns the actor thread ([`connect`]); the hold's drop sends `Shutdown` and joins it.
    /// `None` for a [`DataClientBusSpecCtrader::view`]: closing it must not close the co-mounted socket.
    owner: Mutex<Option<ActorHandle>>,
}

impl DataClientBusSpecCtrader {
    /// A VIEW over an actor owned elsewhere ([`crate::transport::CtraderClient`]), built with `relay` as its sink: the
    /// view never closes the socket.
    pub fn view(shared: ConnectionShared, relay: Arc<DataClientBusSink>) -> Self {
        Self { shared, relay, owner: Mutex::new(None) }
    }

    fn send(&self, cmd: ConnectionCommand) -> Result<(), DataClientError> {
        self.shared
            .tx
            .send(cmd)
            .map_err(|_| DataClientError::Subscribe("actor thread gone".to_string()))
    }

    /// The handshake's `SymbolsList` id of the USER name `symbol` (decision 0290): its kept row's broker name
    /// (`crate::instrument::names::wire_name`), then that name's id. No kept list, no row or two rows is refused in
    /// the translator's words, nothing subscribed; a row whose broker name this account does not list is
    /// `Unsupported`, as before.
    fn resolve_symbol(&self, symbol: &str) -> Result<i64, DataClientError> {
        let wire = crate::instrument::names::wire_name(symbol)
            .map_err(|why| DataClientError::Subscribe(why.to_string()))?;
        self.shared
            .symbols
            .id_of(&wire)
            .ok_or(DataClientError::Unsupported("unknown cTrader symbol (not in SymbolsList)"))
    }
}

impl DataClientBusSpec for DataClientBusSpecCtrader {
    type Hold = DataClientBusSpecHoldCtrader;
    const VENUE: &'static str = VENUE;
    const CLOSED: &'static str = "ctrader: the market-data client is closed";
    /// An interval with no trend-bar period is refused in [`DataClientBusSpecHoldCtrader::ask`].
    const BARS: Result<(), &'static str> = Ok(());
    /// Refused in [`DataClientBusSpecHoldCtrader::ask`] through the caps row (`live_data.trades = false`): no
    /// last-trade print stream on the Open API (capabilities-not-obligations, spec D7).
    const TRADES: Result<(), &'static str> = Ok(());
    /// Refused in [`DataClientBusSpecHoldCtrader::ask`] through the caps row (`live_data.book = false`): depth quotes
    /// are a separate, unimplemented venue verb (`PROTO_OA_SUBSCRIBE_DEPTH_QUOTES_REQ`).
    const BOOK: Result<(), &'static str> = Ok(());
    /// No stated limit on subscribed symbols (the crate's `CLAUDE.md`); the gatewayhub counts its own.
    const MAX_SYMBOLS: Option<(usize, &'static str)> = None;
    /// Owner ruling B-11: the engine's one price check drops a crossed, non-positive or non-finite spot (the actor
    /// already drops a one-sided one).
    const PRICE_CHECK: bool = true;
    // A key is a USER name (decision 0290: upper case, the engine's spelling); the broker's case-significant name is
    // its kept row's, read in `resolve_symbol`, so the default spelling stands (no `SYMBOL_VERBATIM`).
    /// `begin_shutdown` stops every stream and keeps the socket: the session guard's `wind_down` relies on it for a
    /// replaced session's bar keys, and the client may subscribe again after it.
    const RELEASE_AT_BEGIN_SHUTDOWN: bool = true;

    fn consume(
        self: &Arc<Self>,
        deliver: DataClientBusSpecDeliver,
    ) -> DataClientBusSpecHoldCtrader {
        self.relay.attach(deliver);
        DataClientBusSpecHoldCtrader { source: Arc::clone(self), held: Mutex::default() }
    }
}

/// The engine's hold on [`DataClientBusSpecCtrader`]: the streams each held key stands on.
pub struct DataClientBusSpecHoldCtrader {
    source: Arc<DataClientBusSpecCtrader>,
    held: Mutex<HashMap<DataClientBusSpecKey, Stream>>,
}

impl DataClientBusSpecHoldCtrader {
    fn held(&self) -> MutexGuard<'_, HashMap<DataClientBusSpecKey, Stream>> {
        self.held.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Does a held key stand on `symbol_id`'s spot stream? Every key does.
fn holds_spots(held: &HashMap<DataClientBusSpecKey, Stream>, symbol_id: i64) -> bool {
    held.values().any(|s| s.symbol_id() == symbol_id)
}

/// Does a held key hold `symbol_id`'s trend bars at `period`?
fn holds_trendbar(
    held: &HashMap<DataClientBusSpecKey, Stream>,
    symbol_id: i64,
    period: ProtoOaTrendbarPeriod,
) -> bool {
    held.values().any(|s| match s {
        Stream::Bars { symbol_id: id, period: at } => *id == symbol_id && *at == period,
        Stream::Quotes { .. } => false,
    })
}

impl DataClientBusSpecHold for DataClientBusSpecHoldCtrader {
    /// A quote key: the symbol's spots, unless a held key already stands on them (then its next price is the
    /// stream's next tick, not the venue's first-event snapshot). A bar key: the spots the same way, then a bounded
    /// one-shot `GetTrendbars` seed on EVERY subscribe (F2: `SEED_BAR_COUNT` bars ending now, fire-and-forget; the actor
    /// pushes the seeded bars, the still-forming period held back, as `seed_bars`, best-effort), then the live trend
    /// bars unless held already. Each snapshot of the forming period goes out as `forming_bar`, and a period closes
    /// when the next one's first snapshot arrives (`crate::feed::bar::TrendbarCloses`). A symbol not in the
    /// handshake's `SymbolsList`, an interval with no trend-bar period, trades and book are `Unsupported`.
    fn ask(&self, key: &DataClientBusSpecKey, _streaming: bool) -> Result<(), DataClientError> {
        let source = &self.source;
        let stream = match key {
            DataClientBusSpecKey::Trades(_) => {
                require_live_verb(VENUE, DataClientVerb::Trades)?;
                unreachable!("ctrader declares no trade-print feed")
            }
            DataClientBusSpecKey::Book(_) => {
                require_live_verb(VENUE, DataClientVerb::Book)?;
                unreachable!("ctrader declares no lossless book lane")
            }
            DataClientBusSpecKey::Quotes(symbol) => {
                Stream::Quotes { symbol_id: source.resolve_symbol(symbol)? }
            }
            DataClientBusSpecKey::Bars { symbol, interval } => {
                let symbol_id = source.resolve_symbol(symbol)?;
                let period = decode::trendbar_period_for_interval(interval).ok_or(
                    DataClientError::Unsupported("interval has no cTrader trendbar period"),
                )?;
                Stream::Bars { symbol_id, period }
            }
        };
        let mut held = self.held();
        let symbol_id = stream.symbol_id();
        if !holds_spots(&held, symbol_id) {
            source.send(ConnectionCommand::SubscribeSpots { symbol_id })?;
        }
        if let Stream::Bars { symbol_id, period } = stream {
            let to_timestamp = now_ms();
            let from_timestamp =
                to_timestamp - decode::trendbar_period_ms(period) * SEED_BAR_COUNT as i64;
            source.send(ConnectionCommand::GetTrendbars {
                symbol_id,
                period,
                from_timestamp,
                to_timestamp,
                count: SEED_BAR_COUNT,
            })?;
            if !holds_trendbar(&held, symbol_id, period) {
                source.send(ConnectionCommand::SubscribeTrendbar { symbol_id, period })?;
            }
        }
        held.insert(key.clone(), stream);
        Ok(())
    }

    /// The key's last holder left: an unsubscribe only for a wire stream no other held key stands on, trend bars
    /// before the spots they need; a dead actor thread is swallowed (a release never fails).
    fn unwant(&self, key: &DataClientBusSpecKey) {
        let mut held = self.held();
        let Some(stream) = held.remove(key) else { return };
        if let Stream::Bars { symbol_id, period } = stream
            && !holds_trendbar(&held, symbol_id, period)
        {
            let _ = self.source.send(ConnectionCommand::UnsubscribeTrendbar { symbol_id, period });
        }
        let symbol_id = stream.symbol_id();
        if !holds_spots(&held, symbol_id) {
            let _ = self.source.send(ConnectionCommand::UnsubscribeSpots { symbol_id });
        }
    }
}

impl Drop for DataClientBusSpecHoldCtrader {
    /// The client closed: nothing more reaches its sink, and on the data-only path the actor's handle sends
    /// `Shutdown` and joins it (a view leaves the socket to its owner).
    fn drop(&mut self) {
        self.source.relay.detach();
        drop(self.source.owner.lock().unwrap_or_else(PoisonError::into_inner).take());
    }
}
