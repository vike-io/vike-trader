//! One route's connection thread: auth, (re)subscribe, read, fan out; reconnect until stopped.

use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, TryRecvError};
use std::time::Duration;

use tungstenite::Message;
use vike_bridge_core::pump_spec::READ_POLL;
use vike_bridge_core::user_data::sleep_unless_stopped;
use vike_bridge_core::ws::{configure_ws_stream, is_timeout};
use vike_data::LiveDataSink;
use vike_venues::venue_timings;

use super::decode::{DataMsg, decode_control_errors, decode_ws_message};
use super::registry::{ConnCmd, SubRegistry};
use super::{StreamRoute, VENUE, Verb, auth_frame, sub_action, ws_url};
use crate::auth::TokenSource;

/// Reconnect backoff after a socket error: the market lane's fixed `reconnect` wait in
/// `vike_venues::venue_timings::ALPACA` (the shared market pumps' classic 3 s), walked by
/// `sleep_unless_stopped` in stop-aware 100 ms naps. It is the row's `initial`, which is the whole
/// policy only while the row is fixed (`reconnect_backoff_is_the_market_rows_fixed_three_seconds`).
const RECONNECT_BACKOFF: Duration = venue_timings::ALPACA.market().reconnect().initial;

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
///
/// A venue `error` frame is not data and decodes to nothing there, so it is SAID here first: the
/// session it announces ends in a server close and the generic "connection lost" line of
/// [`conn_main`], which names no cause. The text is the venue's own (no credential travels in it).
fn dispatch(ctx: &ConnCtx, prefix: char, txt: &str) {
    for e in decode_control_errors(txt) {
        tracing::warn!(
            venue = "alpaca",
            route = ?ctx.route,
            code = ?e.code,
            msg = %e.msg,
            "market-data WS: the venue sent an error frame"
        );
    }
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
    // The stop-flag poll cadence, a mechanism default shared with every on-driver feed (matches the
    // binance/OKX/Bybit feed convention), not a fact about this venue.
    configure_ws_stream(&socket, READ_POLL);
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
        // Stop-aware: 100 ms naps with a stop check before each, exactly the 30-slice walk this
        // replaced; the loop head above ends the thread once the stop flag is seen.
        sleep_unless_stopped(&ctx.stop, RECONNECT_BACKOFF);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The wait after a socket error is the market row's fixed 3 s, as the thread reads it.
    #[test]
    fn reconnect_backoff_is_the_market_rows_fixed_three_seconds() {
        assert_eq!(RECONNECT_BACKOFF, Duration::from_secs(3));
        assert!(venue_timings::ALPACA.market().reconnect().is_fixed());
    }
}
