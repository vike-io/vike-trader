//! [`AlpacaDataClient`]: one shared connection per [`StreamRoute`], opened lazily, torn down empty.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use vike_data::{DataClient, LiveDataError, LiveDataSink, SubscriptionId, require_live_verb};
use vike_venues::LiveVerb;

use super::conn::{Conn, ConnCtx, conn_main};
use super::registry::{ConnCmd, SubRegistry, WireAction};
use super::{StreamRoute, VENUE, Verb, class_of};
use crate::auth::TokenSource;
use crate::config::AlpacaConfig;

/// Live Alpaca market-data client. Keeps at most one shared, multiplexed WS connection per
/// [`StreamRoute`] (Alpaca allows one connection per key per stream — see the module doc). `sink`
/// receives every bar/quote/trade; `wake` is the GUI repaint nudge (pass `|| {}` headless).
pub struct AlpacaDataClient {
    sink: Arc<dyn LiveDataSink>,
    wake: Arc<dyn Fn() + Send + Sync>,
    token: Arc<TokenSource>,
    data_ws: String,
    next_id: u64,
    conns: HashMap<StreamRoute, Conn>,
    subs: HashMap<SubscriptionId, (StreamRoute, Verb, String)>,
}

impl AlpacaDataClient {
    pub fn new(
        config: AlpacaConfig,
        sink: Arc<dyn LiveDataSink>,
        wake: impl Fn() + Send + Sync + 'static,
    ) -> Self {
        let token = Arc::new(TokenSource::new(
            config.client_id.clone(),
            config.client_secret.clone(),
            config.hosts.authx.to_string(),
        ));
        Self {
            sink,
            wake: Arc::new(wake),
            token,
            data_ws: config.hosts.data_ws.to_string(),
            next_id: 0,
            conns: HashMap::new(),
            subs: HashMap::new(),
        }
    }

    /// Route `(symbol, verb)` to its stream's connection — lazily opening the connection (seeding
    /// it with this subscription) or sending an incremental subscribe frame to an existing one.
    ///
    /// The endpoint is decided by the ASSET CLASS ([`StreamRoute::for_class`]); only the class
    /// itself is still read off the symbol string, because the `DataClient` seam carries no class
    /// — [`class_of`] argues that and names what would have to be threaded.
    fn subscribe(&mut self, symbol: &str, verb: Verb) -> Result<SubscriptionId, LiveDataError> {
        let class = class_of(symbol);
        // Unreachable while [`class_of`] is the only source of `class` — it answers with one of
        // the two classes this venue lists, and both route. The arm stops being dead the day a
        // real class is threaded in, which is the point: an unstreamable class must REFUSE rather
        // than fall into whichever endpoint a string test happened to pick.
        let Some(route) = StreamRoute::for_class(class) else {
            return Err(LiveDataError::Unsupported(
                "alpaca streams no market data for this asset class",
            ));
        };
        let sym = symbol.to_string();
        match self.conns.get_mut(&route) {
            Some(conn) => {
                // Only send a wire subscribe when this (verb, symbol) first goes live on the
                // connection; a duplicate subscribe just bumps the refcount.
                if conn.reg.add(verb, sym.clone()) == WireAction::Subscribe
                    && let Err(e) = conn.cmd_tx.send(ConnCmd::Sub(verb, sym.clone()))
                {
                    // the connection thread is gone — drop the dead handle so a later
                    // subscribe respawns it cleanly, and don't record this subscription.
                    self.conns.remove(&route);
                    return Err(LiveDataError::Subscribe(format!("alpaca {sym} {verb:?}: {e}")));
                }
            }
            None => {
                let (cmd_tx, cmd_rx) = std::sync::mpsc::channel();
                let stop = Arc::new(AtomicBool::new(false));
                let ctx = ConnCtx {
                    route,
                    sink: Arc::clone(&self.sink),
                    wake: Arc::clone(&self.wake),
                    stop: Arc::clone(&stop),
                    token: Arc::clone(&self.token),
                    data_ws: self.data_ws.clone(),
                    cmd_rx,
                };
                let initial = (verb, sym.clone());
                let join = std::thread::Builder::new()
                    .name(format!("alpaca-data-{route:?}"))
                    .spawn(move || conn_main(ctx, initial))
                    .map_err(|e| LiveDataError::Subscribe(format!("alpaca {sym} {verb:?}: {e}")))?;
                // The connection thread subscribes `initial` itself (via its `active` set), so the
                // registry just records it — no wire command is sent for the seed.
                let mut reg = SubRegistry::default();
                reg.add(verb, sym.clone());
                self.conns.insert(route, Conn { cmd_tx, stop, join, reg });
            }
        }
        let id = SubscriptionId(self.next_id);
        self.next_id += 1;
        self.subs.insert(id, (route, verb, sym));
        Ok(id)
    }
}

impl DataClient for AlpacaDataClient {
    fn subscribe_bars(
        &mut self,
        symbol: &str,
        interval: &str,
    ) -> Result<SubscriptionId, LiveDataError> {
        // Alpaca's market-data WS serves only fixed 1-minute bars.
        if interval != "1m" {
            return Err(LiveDataError::Unsupported("alpaca market-data WS serves 1m bars only"));
        }
        self.subscribe(symbol, Verb::Bars)
    }

    fn subscribe_quotes(&mut self, symbol: &str) -> Result<SubscriptionId, LiveDataError> {
        self.subscribe(symbol, Verb::Quotes)
    }

    fn subscribe_trades(&mut self, symbol: &str) -> Result<SubscriptionId, LiveDataError> {
        self.subscribe(symbol, Verb::Trades)
    }

    fn subscribe_book(&mut self, _symbol: &str) -> Result<SubscriptionId, LiveDataError> {
        // Caps-driven refusal (alpaca live_data.book = false): stays in lockstep with the table.
        require_live_verb(VENUE, LiveVerb::Book)?;
        unreachable!("alpaca declares no lossless book lane")
    }

    fn unsubscribe(&mut self, id: SubscriptionId) {
        let Some((route, verb, sym)) = self.subs.remove(&id) else { return };
        // Send a wire unsubscribe only when this (verb, symbol)'s LAST subscriber drops (so a
        // sibling SubscriptionId on the same pair keeps its feed); tear the whole connection down
        // once its last subscription of any kind drops.
        let teardown = match self.conns.get_mut(&route) {
            Some(conn) => {
                let (action, teardown) = conn.reg.remove(verb, &sym);
                if action == WireAction::Unsubscribe {
                    let _ = conn.cmd_tx.send(ConnCmd::Unsub(verb, sym));
                }
                teardown
            }
            None => false,
        };
        if teardown && let Some(conn) = self.conns.remove(&route) {
            conn.stop.store(true, Ordering::Relaxed);
            let _ = conn.join.join();
        }
    }

    fn shutdown(&mut self) {
        for conn in self.conns.values() {
            conn.stop.store(true, Ordering::Relaxed);
        }
        for (_, conn) in self.conns.drain() {
            let _ = conn.join.join();
        }
        self.subs.clear();
    }
}
