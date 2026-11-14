//! [`DataClientBusSpecAlpaca`]: alpaca's market data as the shared engine's source
//! (`vike_bridge_core::data_client::bus::DataClientBus`), one shared connection per [`StreamRoute`], opened by the
//! route's first key and torn down with its last.
//!
//! The engine counts the holders of each key, mints the ids and refuses a closed client; this file decides what a
//! key costs on the wire. A key reaches [`DataClientBusSpecHoldAlpaca::ask`] on every subscribe and
//! [`DataClientBusSpecHoldAlpaca::unwant`] once, from its last holder, so the hold keeps only which keys each
//! connection carries: the first of a route spawns its connection (which subscribes it itself), a later one writes
//! one `subscribe` frame, a repeat writes nothing, a release writes one `unsubscribe` frame, and the route's last
//! release stops and joins its connection.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use vike_bridge_core::data_client::bus::{
    DataClientBusSpec, DataClientBusSpecDeliver, DataClientBusSpecHold, DataClientBusSpecKey,
};
use vike_data::{DataClientError, require_live_verb};
use vike_venues::DataClientVerb;

use super::{StreamRoute, VENUE, Verb, class_of};
use crate::config::AlpacaConfig;
use crate::feed::connection::{
    RouteConnection, RouteConnectionCommand, RouteConnectionContext, connection_main,
};
use crate::transport::sign::TokenSource;

/// Alpaca's market-data source: the OAuth token this seam mints for itself (its own lifecycle, the crate's
/// `CLAUDE.md`), the data WS host, and the GUI repaint nudge every connection fires after a frame (`|| {}`
/// headless). The client is `DataClientBus::on(&Arc::new(DataClientBusSpecAlpaca::new(config, wake)), sink)`.
pub struct DataClientBusSpecAlpaca {
    token: Arc<TokenSource>,
    data_websocket: String,
    wake: Arc<dyn Fn() + Send + Sync>,
}

impl DataClientBusSpecAlpaca {
    pub fn new(config: AlpacaConfig, wake: impl Fn() + Send + Sync + 'static) -> Self {
        let token = Arc::new(TokenSource::new(
            config.client_id.expose().clone(),
            config.client_secret.expose().clone(),
            config.hosts.authx.to_string(),
        ));
        Self {
            token,
            data_websocket: config.hosts.data_websocket.to_string(),
            wake: Arc::new(wake),
        }
    }
}

impl DataClientBusSpec for DataClientBusSpecAlpaca {
    type Hold = DataClientBusSpecHoldAlpaca;
    const VENUE: &'static str = VENUE;
    const CLOSED: &'static str = "alpaca: the market-data client is closed";
    /// The WS serves 1-minute bars only; any other interval is refused in [`DataClientBusSpecHoldAlpaca::ask`].
    const BARS: Result<(), &'static str> = Ok(());
    const TRADES: Result<(), &'static str> = Ok(());
    /// Refused in [`DataClientBusSpecHoldAlpaca::ask`] through the caps row (`live_data.book = false`), so the
    /// refusal stays in lockstep with the table.
    const BOOK: Result<(), &'static str> = Ok(());
    /// Alpaca states no cap a client could count.
    const MAX_SYMBOLS: Option<(usize, &'static str)> = None;
    /// Owner ruling B-11: the engine's one price check drops a one-sided, crossed or non-finite quote and a print
    /// with no positive, finite price and size (the decode passes whatever the frame says).
    const PRICE_CHECK: bool = true;
    /// [`class_of`] routes on the symbol as the caller spelled it (`BTC/USD` is crypto, `BTCUSD` an equity): never
    /// re-spelled.
    const SYMBOL_VERBATIM: bool = true;
    /// alpaca streams no mark, so a bar's close is this venue's only conflated price lane.
    const BAR_CLOSE_TICK: bool = true;
    /// A shutdown stops every connection at once in the hold's drop (raise both flags, then join), not key by key.
    const UNWANT_AT_SHUTDOWN: bool = false;

    fn consume(self: &Arc<Self>, deliver: DataClientBusSpecDeliver) -> DataClientBusSpecHoldAlpaca {
        DataClientBusSpecHoldAlpaca {
            source: Arc::clone(self),
            deliver: Arc::new(deliver),
            connections: Mutex::default(),
        }
    }
}

/// The engine's hold on [`DataClientBusSpecAlpaca`]: at most one connection per [`StreamRoute`] (Alpaca allows one
/// per key per stream), each with the keys it carries.
pub struct DataClientBusSpecHoldAlpaca {
    source: Arc<DataClientBusSpecAlpaca>,
    deliver: Arc<DataClientBusSpecDeliver>,
    connections: Mutex<HashMap<StreamRoute, RouteConnection>>,
}

/// The verb and symbol `key` names, or the refusal of a key this venue never serves.
fn wire_of(key: &DataClientBusSpecKey) -> Result<(Verb, &str), DataClientError> {
    match key {
        DataClientBusSpecKey::Quotes(symbol) => Ok((Verb::Quotes, symbol)),
        DataClientBusSpecKey::Trades(symbol) => Ok((Verb::Trades, symbol)),
        // Alpaca's market-data WS serves only fixed 1-minute bars.
        DataClientBusSpecKey::Bars { interval, .. } if interval != "1m" => {
            Err(DataClientError::Unsupported("alpaca market-data WS serves 1m bars only"))
        }
        DataClientBusSpecKey::Bars { symbol, .. } => Ok((Verb::Bars, symbol)),
        DataClientBusSpecKey::Book(_) => {
            require_live_verb(VENUE, DataClientVerb::Book)?;
            unreachable!("alpaca declares no lossless book lane")
        }
    }
}

/// The stream that serves `symbol`, decided by its ASSET CLASS ([`StreamRoute::for_class`]); only the class itself
/// is still read off the symbol string, because the `DataClient` seam carries no class ([`class_of`] argues that).
fn route_of(symbol: &str) -> Result<StreamRoute, DataClientError> {
    // Unreachable while [`class_of`] is the only source of the class: it answers one of the two classes this venue
    // lists, and both route. The arm stops being dead the day a real class is threaded in, which is the point: an
    // unstreamable class must REFUSE rather than fall into whichever endpoint a string test happened to pick.
    StreamRoute::for_class(class_of(symbol))
        .ok_or(DataClientError::Unsupported("alpaca streams no market data for this asset class"))
}

impl DataClientBusSpecHoldAlpaca {
    fn connections(&self) -> MutexGuard<'_, HashMap<StreamRoute, RouteConnection>> {
        self.connections.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// A connection for `route` that dials nothing, so a test reads the frames this hold writes.
    #[cfg(test)]
    pub(super) fn plant(&self, route: StreamRoute, conn: RouteConnection) {
        self.connections().insert(route, conn);
    }

    /// Whether `route` has a connection.
    #[cfg(test)]
    pub(super) fn has_connection(&self, route: StreamRoute) -> bool {
        self.connections().contains_key(&route)
    }
}

impl DataClientBusSpecHold for DataClientBusSpecHoldAlpaca {
    /// Route the key to its stream's connection: a key the connection does not carry yet is one incremental
    /// `subscribe` frame, or, with no connection for the route, a new one seeded with it. A key it carries already
    /// writes nothing. A connection whose thread is gone is dropped, so the next key respawns it cleanly, and the
    /// ask fails.
    fn ask(&self, key: &DataClientBusSpecKey, _streaming: bool) -> Result<(), DataClientError> {
        let (verb, symbol) = wire_of(key)?;
        let route = route_of(symbol)?;
        let owned_symbol = symbol.to_string();
        let mut connections = self.connections();
        match connections.get_mut(&route) {
            Some(conn) => {
                if conn.keys.insert((verb, owned_symbol.clone()))
                    && let Err(e) = conn
                        .command_tx
                        .send(RouteConnectionCommand::Sub(verb, owned_symbol.clone()))
                {
                    connections.remove(&route);
                    return Err(DataClientError::Subscribe(format!(
                        "alpaca {owned_symbol} {verb:?}: {e}"
                    )));
                }
            }
            None => {
                let (command_tx, command_rx) = std::sync::mpsc::channel();
                let stop = Arc::new(AtomicBool::new(false));
                let ctx = RouteConnectionContext {
                    route,
                    deliver: Arc::clone(&self.deliver),
                    wake: Arc::clone(&self.source.wake),
                    stop: Arc::clone(&stop),
                    token: Arc::clone(&self.source.token),
                    data_websocket: self.source.data_websocket.clone(),
                    command_rx,
                };
                let initial = (verb, owned_symbol.clone());
                let join = std::thread::Builder::new()
                    .name(format!("alpaca-data-{route:?}"))
                    .spawn(move || {
                        vike_execution::affinity::pin_current_thread(
                            vike_execution::affinity::Role::MarketData,
                            VENUE,
                        );
                        connection_main(ctx, initial)
                    })
                    .map_err(|e| {
                        DataClientError::Subscribe(format!("alpaca {owned_symbol} {verb:?}: {e}"))
                    })?;
                // The connection thread subscribes `initial` itself (via its `active` set): no wire command for the seed.
                let keys = HashSet::from([(verb, owned_symbol)]);
                connections.insert(route, RouteConnection { command_tx, stop, join, keys });
            }
        }
        Ok(())
    }

    /// The key's last holder left: one `unsubscribe` frame, or, for the connection's last key, its stop and join
    /// (outside the lock). A key the connection does not carry (its connection died and was replaced) writes nothing.
    fn unwant(&self, key: &DataClientBusSpecKey) {
        let Ok((verb, symbol)) = wire_of(key) else { return };
        let Ok(route) = route_of(symbol) else { return };
        let mut connections = self.connections();
        let Some(conn) = connections.get_mut(&route) else { return };
        let wire = (verb, symbol.to_string());
        if !conn.keys.remove(&wire) {
            return;
        }
        if !conn.keys.is_empty() {
            let _ = conn.command_tx.send(RouteConnectionCommand::Unsubscribe(wire.0, wire.1));
            return;
        }
        let Some(conn) = connections.remove(&route) else { return };
        drop(connections);
        conn.stop.store(true, Ordering::Relaxed);
        let _ = conn.join.join();
    }
}

impl Drop for DataClientBusSpecHoldAlpaca {
    /// The client closed: raise every connection's stop flag, then join each, so the routes wind down together.
    fn drop(&mut self) {
        let connections: Vec<RouteConnection> =
            self.connections().drain().map(|(_, conn)| conn).collect();
        for conn in &connections {
            conn.stop.store(true, Ordering::Relaxed);
        }
        for conn in connections {
            let _ = conn.join.join();
        }
    }
}
