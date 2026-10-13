//! One route's connection thread: auth, (re)subscribe, read, fan out; reconnect until stopped.

use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, TryRecvError};
use std::time::Duration;

use tungstenite::Message;
use vike_bridge_core::ws::{configure_ws_stream, is_timeout};
use vike_data::LiveDataSink;

use super::decode::{DataMsg, decode_ws_message};
use super::registry::{ConnCmd, SubRegistry};
use super::{StreamRoute, VENUE, Verb, auth_frame, sub_action, ws_url};
use crate::auth::TokenSource;

/// Stop-flag poll cadence (matches the binance/OKX/Bybit feed convention).
const READ_TIMEOUT: Duration = Duration::from_secs(2);
/// Reconnect backoff after a socket error, in stop-aware 100ms slices (3s total).
const RECONNECT_BACKOFF_SLICES: u32 = 30;

/// The controller-side handle to one route's connection thread.
pub(super) struct Conn {
    pub(super) cmd_tx: Sender<ConnCmd>,
    pub(super) stop: Arc<AtomicBool>,
    pub(super) join: std::thread::JoinHandle<()>,
    /// per-`(verb, symbol)` + connection-wide refcounts; the connection is torn down at total 0.
    pub(super) reg: SubRegistry,
}

/// The thread-side context for one route's connection.
pub(super) struct ConnCtx {
    pub(super) route: StreamRoute,
    pub(super) sink: Arc<dyn LiveDataSink>,
    pub(super) wake: Arc<dyn Fn() + Send + Sync>,
    pub(super) stop: Arc<AtomicBool>,
    pub(super) token: Arc<TokenSource>,
    pub(super) data_ws: String,
    pub(super) cmd_rx: Receiver<ConnCmd>,
}

/// Fan one decoded WS text frame out to the sink, then nudge the GUI.
fn dispatch(ctx: &ConnCtx, prefix: char, txt: &str) {
    for m in decode_ws_message(prefix, txt) {
        match m {
            DataMsg::Bar { symbol, interval, bar, closed } => {
                // `bar_close_tick`, NOT `mark_tick`: a candle close is not a venue mark
                // (mark-slot semantics); alpaca streams no mark price, so the bar-close slot is
                // this venue's only conflated price lane.
                ctx.sink.bar_close_tick(VENUE, &symbol, bar.close, bar.ts);
                if closed {
                    ctx.sink.close_bar(VENUE, &symbol, &interval, bar);
                } else {
                    ctx.sink.forming_bar(VENUE, &symbol, &interval, bar);
                }
            }
            DataMsg::Quote { symbol, q } => ctx.sink.quote(VENUE, &symbol, q),
            DataMsg::Trade { symbol, t } => ctx.sink.trade(VENUE, &symbol, t),
        }
    }
    (ctx.wake)();
}

/// One connection session: mint/refresh the Bearer, connect, auth, (re)subscribe everything in
/// `active`, then interleave draining the command channel (subscribe/unsubscribe frames) with
/// reading data frames until stopped or the socket errors. `active` persists across reconnects (the
/// caller in [`conn_main`] owns it) so every live subscription is re-sent on re-open — no feed is
/// silently lost to a reconnect.
fn run_conn(
    ctx: &ConnCtx,
    active: &mut HashSet<(Verb, String)>,
) -> Result<(), Box<dyn std::error::Error>> {
    let bearer = ctx.token.bearer()?;
    let url = ws_url(&ctx.data_ws, ctx.route);
    let (mut socket, _resp) = tungstenite::connect(url.as_str())?;
    configure_ws_stream(&socket, READ_TIMEOUT);
    socket.send(Message::Text(auth_frame(&bearer).to_string().into()))?;
    for (verb, symbol) in active.iter() {
        socket.send(Message::Text(sub_action("subscribe", *verb, symbol).to_string().into()))?;
    }
    let prefix = match ctx.route {
        StreamRoute::Crypto => 'c',
        StreamRoute::Equity => 's',
    };
    loop {
        if ctx.stop.load(Ordering::Relaxed) {
            return Ok(());
        }
        // Drain pending subscribe/unsubscribe commands (non-blocking).
        loop {
            match ctx.cmd_rx.try_recv() {
                Ok(ConnCmd::Sub(v, s)) => {
                    active.insert((v, s.clone()));
                    socket
                        .send(Message::Text(sub_action("subscribe", v, &s).to_string().into()))?;
                }
                Ok(ConnCmd::Unsub(v, s)) => {
                    active.remove(&(v, s.clone()));
                    socket
                        .send(Message::Text(sub_action("unsubscribe", v, &s).to_string().into()))?;
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => return Ok(()), // client dropped
            }
        }
        let msg = match socket.read() {
            Ok(m) => m,
            Err(e) if is_timeout(&e) => continue, // stop-flag / command poll tick
            Err(e) => return Err(e.into()),
        };
        match msg {
            Message::Text(txt) => dispatch(ctx, prefix, txt.as_str()),
            Message::Ping(p) => socket.send(Message::Pong(p))?,
            Message::Close(_) => return Err("server closed".into()),
            _ => {}
        }
    }
}

/// Reconnect loop for one route's connection: retry [`run_conn`] (stop-aware backoff) until `stop`
/// is raised or a requested stop returns `Ok(())`. `active` (the set of live subscriptions) lives
/// here so it survives reconnects, seeded with the subscription that first opened the connection.
pub(super) fn conn_main(ctx: ConnCtx, initial: (Verb, String)) {
    let mut active: HashSet<(Verb, String)> = HashSet::new();
    active.insert(initial);
    while !ctx.stop.load(Ordering::Relaxed) {
        match run_conn(&ctx, &mut active) {
            Ok(()) => break, // requested stop / client dropped
            Err(_e) => {
                // `_e` is intentionally not interpolated: it may wrap the connect URL/response,
                // never a secret. A generic boundary log matches the OANDA sibling's density.
                tracing::warn!(
                    venue = "alpaca",
                    route = ?ctx.route,
                    "market-data WS reconnect: connection lost"
                );
            }
        }
        for _ in 0..RECONNECT_BACKOFF_SLICES {
            if ctx.stop.load(Ordering::Relaxed) {
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}
