//! The blocking tungstenite WebSocket socket every WS venue lane holds, the stream options a pump sets on it and the
//! one test for a read that timed out (feature `websocket`). The gated send, the handshake-ack loop and the pump's
//! stream adapter are venue-shaped and stay in `vike-bridge-core`'s `websocket` module.

use std::net::TcpStream;
use std::time::Duration;

use tungstenite::WebSocket;
use tungstenite::stream::MaybeTlsStream;

/// The socket a WS dial hands back (`crate::websocket_proxy::connect_websocket`), plain or rustls over TCP.
pub type WsSocket = WebSocket<MaybeTlsStream<TcpStream>>;

/// Configure the raw TCP stream under a freshly-opened WS socket for the pump:
/// 1. a read timeout so `read()` returns periodically for stop-polls, and
/// 2. `TCP_NODELAY`, so small order/subscribe/quote frames go on the wire immediately instead of
///    being Nagle-coalesced (tens of ms on tiny frames).
///
/// Both are best-effort (`let _`): a transient socket-option failure must not abort a connection.
pub fn configure_websocket_stream(socket: &WsSocket, timeout: Duration) {
    match socket.get_ref() {
        MaybeTlsStream::Plain(s) => {
            let _ = s.set_read_timeout(Some(timeout));
            let _ = s.set_nodelay(true);
        }
        MaybeTlsStream::Rustls(t) => {
            let _ = t.get_ref().set_read_timeout(Some(timeout));
            let _ = t.get_ref().set_nodelay(true);
        }
        _ => {}
    }
}

/// Shared WouldBlock/TimedOut classifier for a raw socket read: the pump's stream adapter
/// (`vike_bridge_core::websocket::WebsocketSessionUserStreamTungstenite`) and every venue loop that reads the socket
/// before it is wrapped.
pub fn is_timeout(err: &tungstenite::Error) -> bool {
    matches!(
        err,
        tungstenite::Error::Io(e) if matches!(
            e.kind(),
            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
        )
    )
}
