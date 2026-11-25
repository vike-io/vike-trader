//! [`DataClientSpecAlpaca`]: alpaca's market data as a venue of the shared data engine
//! (`vike_bridge_core::data_client::ws::DataClientWebsocket`): every subscription is a `SessionOn` lane on the market
//! pump with this venue's `websocket_session::spec` row (the bounded dial, the idle watchdog, the fixed redial), and
//! every lane of a [`StreamRoute`] rides that route's ONE socket (`feed/connection.rs`'s `RouteLink`, dialled by the
//! first lane that needs it). The engine spawns, names, pins, refuses and stops the lanes; this file decides what a
//! lane asks of the socket and what it hands the sink.
//!
//! A lane keeps the messages of its own key from every frame of its route, under the USER name it was subscribed by,
//! and judges them by the one price check (owner ruling B-11: a one-sided, crossed or non-finite quote and a print with
//! no positive, finite price and size are dropped). A bar's close is this venue's last-price snapshot
//! (`bar_close_tick`, ahead of the bar: alpaca streams no mark). Its status is the engine's words through
//! `DataClientContext::set_status`: `Connected` on its session's first frame, `Error` on a session the socket ended.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use vike_bridge_core::data_client::bus::quote_prices_hold;
use vike_bridge_core::data_client::context::DataClientContext;
use vike_bridge_core::data_client::ws::{
    DataClientLane, DataClientLanePlan, DataClientSpec, live_status,
};
use vike_bridge_core::websocket_session::market::{
    WebsocketSessionFrameOutcome, WebsocketSessionMarketStream,
};
use vike_bridge_core::websocket_session::spec;
use vike_model::feed_status::DataClientConnectionState;
use vike_utility::mutex::lock;

use super::connection::RouteLink;
use super::decode::{FeedMessage, decode_websocket_message};
use super::{StreamRoute, VENUE, Verb, class_of, websocket_url};
use crate::config::AlpacaConfig;
use crate::refusals::AlpacaFeedRefusal;
use crate::transport::sign::TokenSource;

/// The name the status line speaks: `connecting to Alpaca…`, `LIVE · Alpaca`.
const DISPLAY: &str = "Alpaca";

/// Alpaca's market-data source: the OAuth token this seam mints for itself (its own lifecycle, the crate's
/// `CLAUDE.md`), the data WS host, and the one socket of each route, made when its first lane asks. The client is
/// `DataClientWebsocket::new(DataClientSpecAlpaca::new(config), sink, wake)`.
pub struct DataClientSpecAlpaca {
    token: Arc<TokenSource>,
    data_websocket: String,
    links: Mutex<HashMap<StreamRoute, Arc<RouteLink>>>,
}

impl DataClientSpecAlpaca {
    pub fn new(config: AlpacaConfig) -> Self {
        let token = Arc::new(TokenSource::new(
            config.client_id.expose().clone(),
            config.client_secret.expose().clone(),
            config.hosts.authx.to_string(),
        ));
        Self {
            token,
            data_websocket: config.hosts.data_websocket.to_string(),
            links: Mutex::default(),
        }
    }

    /// The one socket of `route`.
    fn link(&self, route: StreamRoute) -> Arc<RouteLink> {
        let mut links = lock(&self.links);
        let link = links.entry(route).or_insert_with(|| {
            Arc::new(RouteLink::new(
                route,
                websocket_url(&self.data_websocket, route),
                Arc::clone(&self.token),
                spec::row(VENUE),
            ))
        });
        Arc::clone(link)
    }
}

impl DataClientSpec for DataClientSpecAlpaca {
    const VENUE: &'static str = VENUE;
    const DISPLAY: &'static str = DISPLAY;
    /// alpaca streams no mark: a bar's close is its last-price snapshot.
    const MARK_STREAMS_DEFAULT_ON: bool = false;

    /// The WS serves fixed 1-minute bars only.
    fn refuse_bars(&self, interval: &str) -> Result<(), String> {
        if interval == "1m" {
            Ok(())
        } else {
            Err(AlpacaFeedRefusal::BarsOnlyOneMinute.to_string())
        }
    }

    /// The lane's key on its route's socket: the verb, and the WIRE name of the USER name `symbol` off its kept row,
    /// on the stream of that row's class. A name with no row, or a class alpaca streams nothing for, is refused in
    /// words (the lane's status says why, and it ends without a dial).
    fn plan(
        &self,
        lane: DataClientLane,
        symbol: &str,
        interval: &str,
    ) -> Result<DataClientLanePlan, String> {
        let verb = match lane {
            DataClientLane::Bars => Verb::Bars,
            DataClientLane::Quotes => Verb::Quotes,
            DataClientLane::Trades => Verb::Trades,
            // Never asked: the caps row refuses book and depth first, and no bar pairs a mark.
            other => {
                let lane = other.word(interval).to_string();
                return Err(AlpacaFeedRefusal::NoLane { lane }.to_string());
            }
        };
        let (class, wire) = class_of(symbol).map_err(|refusal| refusal.to_string())?;
        let route = StreamRoute::for_class(class)
            .ok_or_else(|| AlpacaFeedRefusal::ClassNotStreamed.to_string())?;
        let key = lane.key(symbol, interval);
        Ok(lane_plan(self.link(route), route, verb, symbol.to_string(), wire, key))
    }

    /// Every socket's stop, after every lane's: each reader closes within its poll.
    fn begin_shutdown(&self) {
        for link in lock(&self.links).values() {
            link.raise_stop();
        }
    }

    /// Every socket's reader joined, after every lane is.
    fn shutdown(&self) {
        let links: Vec<Arc<RouteLink>> = lock(&self.links).values().cloned().collect();
        for link in links {
            link.shutdown();
        }
    }
}

/// One subscription's lane, as the engine's `SessionOn` plan: its key on the route's socket in `connect`, its own
/// messages to the sink in `on_text`, the engine's healthy status in `on_live` and its error words in `on_error`.
/// `symbol` is the USER name every sink call is keyed by; `wire` (its kept row's) is the name the socket speaks.
pub(super) fn lane_plan(
    link: Arc<RouteLink>,
    route: StreamRoute,
    verb: Verb,
    symbol: String,
    wire: String,
    key: String,
) -> DataClientLanePlan {
    let asked = wire.clone();
    DataClientLanePlan::SessionOn {
        connect: Box::new(move |ctx: &DataClientContext| {
            link.attach(&ctx.stop, verb, &asked)
                .map(|stream| Box::new(stream) as Box<dyn WebsocketSessionMarketStream>)
        }),
        on_text: Box::new(move |ctx: &DataClientContext, text: &str| {
            deliver(ctx, route.prefix(), verb, &symbol, &wire, text);
            // Any frame on the route's socket proves it works (one status line for every lane of the client).
            WebsocketSessionFrameOutcome::Confirm
        }),
        on_tick: Box::new(|_: &DataClientContext| {}),
        on_live: Box::new(|ctx: &DataClientContext| {
            ctx.set_status(DataClientConnectionState::Connected, live_status(DISPLAY));
        }),
        on_error: Box::new(move |ctx: &DataClientContext, e: &str| {
            ctx.set_status(
                DataClientConnectionState::Error,
                format!("{key} ws error (reconnecting): {e}"),
            );
        }),
        backoff: None,
    }
}

/// The messages of `verb` on `wire` in one frame of the route (its `prefix`), to the sink under `symbol`, then one wake
/// if any went.
pub(super) fn deliver(
    ctx: &DataClientContext,
    prefix: char,
    verb: Verb,
    symbol: &str,
    wire: &str,
    text: &str,
) {
    let sink = &ctx.sink;
    let mut delivered = false;
    for message in decode_websocket_message(prefix, text) {
        match (verb, message) {
            (Verb::Quotes, FeedMessage::Quote { symbol: on, mut q }) if on == wire => {
                if quote_prices_hold(&q) {
                    q.local_timestamp = vike_model::now_ms();
                    q.symbol = symbol.into();
                    sink.quote(VENUE, symbol, q);
                    delivered = true;
                }
            }
            (Verb::Trades, FeedMessage::Trade { symbol: on, mut t }) if on == wire => {
                if vike_bridge_core::is_valid_trade(t.price, t.size) {
                    t.local_timestamp = vike_model::now_ms();
                    t.symbol = symbol.into();
                    sink.trade(VENUE, symbol, t);
                    delivered = true;
                }
            }
            (Verb::Bars, FeedMessage::Bar { symbol: on, interval, bar, closed }) if on == wire => {
                sink.bar_close_tick(VENUE, symbol, bar.close, bar.timestamp);
                if closed {
                    sink.close_bar(VENUE, symbol, &interval, bar);
                } else {
                    sink.forming_bar(VENUE, symbol, &interval, bar);
                }
                delivered = true;
            }
            _ => {}
        }
    }
    if delivered {
        (ctx.wake)();
    }
}
