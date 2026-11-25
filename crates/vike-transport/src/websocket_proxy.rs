//! Optional **SOCKS5 egress for the blocking WS dial** — the WebSocket twin of the proxy `ureq`
//! applies to REST, for a venue whose feeds must leave through a tunnel (polymarket is
//! US-geo-blocked, and `crates/bridges/polymarket/src/transport/mod.rs` routes its HTTP through a
//! `socks5h://` tunnel).
//!
//! **ADDITIVE and INERT in a shared home** (`market_pump` serves every venue): [`connect_websocket`] with
//! `proxy: None` never touches the proxy machinery, so a venue that passes no proxy is unaffected.
//! Only a caller that explicitly hands a [`WebsocketProxy`] takes that arm.
//!
//! **The SOCKS5 client is `crate::socks`**, the one the HTTP lane's `SocksDial` speaks, so the two lanes put the
//! same bytes on the wire. It sits behind this crate's OPT-IN `socks-proxy` feature, so a default
//! build compiles no proxy code at all.
//!
//! **`socks5h` vs `socks5`** is honoured, and it matters: plain `socks5` resolves the target DNS
//! LOCALLY (which in a geo-blocked/DNS-poisoned network resolves to the wrong host —
//! `crates/bridges/polymarket/src/transport/mod.rs`'s `proxy_url`), while `socks5h` hands the DOMAIN to the
//! proxy to resolve remotely. [`WebsocketProxy::remote_dns`] carries that bit and [`connect_websocket`] acts on it.
//!
//! **What "bounded" covers: the whole TCP phase.** [`connect_websocket`]'s `Some(bound)` arm is ONE
//! wall-clock deadline over BOTH blocking steps in front of the handshake ([`crate::dial::dial_bounded`]: the
//! lookup bounded, every resolved address tried), not a timeout on the last of them.
//!
//! **The upgrade names this client**: a request with no `User-Agent` of its own is sent with
//! [`crate::http::USER_AGENT`]; one the caller set is kept.
//!
//! **The handshake after the TCP phase is bounded PER READ, by the same window.** `Some(bound)` also
//! sets `bound` as the read timeout of the dialed stream before `tungstenite::client_tls` runs the TLS
//! and WS handshake on it ([`handshake_bounded`]), so a peer that accepts the TCP connection and then
//! says nothing (a half-open edge, a stalled TLS terminator) fails the connect after one window
//! instead of blocking the caller for ever. It is a READ bound, not a deadline: the handshake is a few
//! reads (the TLS flights and the upgrade reply), each allowed `bound`, so a peer that answers each
//! one just in time still holds the call for several windows. The timeout is CLEARED once the socket is
//! up, so a socket leaves `connect_websocket` exactly as it did before (no read timeout; `configure_websocket_stream`
//! sets the pump's poll), and a caller that never configures one does not suddenly time out on a quiet,
//! healthy feed. A timed-out handshake is the error `ws handshake: no answer within <window>`.
//!
//! **The proxied arm spends the same window.** ONE deadline covers the proxy's name lookup, every address's TCP
//! connect ([`crate::dial::dial_bounded`] on the proxy) and the whole SOCKS5 exchange, the target's local lookup
//! under plain `socks5` included; the TLS + WS handshake through the tunnel then reads under the same window
//! ([`handshake_bounded`]), as the direct arm's does. A proxied dial with no window is REFUSED by name and dials
//! nothing: only the direct arm has an unbounded twin (`connect_timeout: None` stays `tungstenite::connect`).
//!
//! **A venue whose upgrade needs its own header dials [`connect_websocket_request`]**, the same three arms
//! over a request the caller built (ig's TLCP subprotocol), so no bridge
//! re-spells this bound to say one header.
//!
//! **The bounded arm follows no redirect**, where `tungstenite::connect` follows up to three: a 3xx to
//! the upgrade is a failed connect, named by its status. The private sockets moved onto this arm
//! answered `101` to a keyless upgrade on all 17 of their hosts (measured from the CI box, 2026-11-07), and
//! the REST agents of `crate::http` follow no `Location` either.

use std::net::TcpStream;
use std::time::Duration;

use tungstenite::client::IntoClientRequest;
use tungstenite::handshake::client::Request;
use tungstenite::stream::MaybeTlsStream;

use crate::dial::{dial_bounded, split_host_port};
use crate::websocket::{WsSocket, is_timeout};

/// A parsed SOCKS5 endpoint for the WS lane.
///
/// Built from the same URL string the HTTP lane hands `ureq::Proxy::new` ([`WebsocketProxy::parse`]), so
/// a venue never grows a SECOND place to say where its tunnel is.
#[derive(Clone, PartialEq, Eq)]
pub struct WebsocketProxy {
    /// Proxy host (an IP or a name; resolved LOCALLY — the proxy itself is always local-network).
    pub host: String,
    /// Proxy port (`1080` when the URL omits it).
    pub port: u16,
    /// `socks5h` ⇒ `true`: the TARGET domain is sent to the proxy and resolved THERE.
    /// `socks5` ⇒ `false`: the target is resolved locally and an IP is sent. Only `socks5h` clears
    /// a DNS-level geo-block.
    pub remote_dns: bool,
    /// Optional username/password (SOCKS5 RFC-1929). The password is a SECRET — the manual
    /// [`std::fmt::Debug`] impl below redacts it, and it is never logged.
    pub auth: Option<(String, String)>,
}

// Secrets never reach Debug/Display/logs (CLAUDE.md "Credentials & the live gate").
impl std::fmt::Debug for WebsocketProxy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "WsProxy({}:{}, remote_dns={}, auth={})",
            self.host,
            self.port,
            self.remote_dns,
            if self.auth.is_some() { "set" } else { "none" }
        )
    }
}

impl std::fmt::Display for WebsocketProxy {
    /// The endpoint WITHOUT credentials — safe for a status line.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let scheme = if self.remote_dns { "socks5h" } else { "socks5" };
        write!(f, "{scheme}://{}:{}", self.host, self.port)
    }
}

impl WebsocketProxy {
    /// Parse `socks5://[user:pass@]host[:port]` or `socks5h://…` (default port `1080`).
    ///
    /// Deliberately STRICT: an unknown/absent scheme is an error rather than a guess, because a
    /// silently-misparsed proxy would fall back to a direct dial and hit the very geo-block the
    /// proxy exists to clear. `http`/`https` proxies are NOT supported here — a CONNECT-tunnel
    /// client is a different protocol and no venue asks for one.
    ///
    /// ⚠ **A rejected url still has to be REPORTED, and the url may carry RFC-1929 credentials.**
    /// `Debug`/`Display` above only protect a url that PARSED; the two arms below that echo the
    /// input echo it before any userinfo has been split off, and
    /// `crates/bridges/polymarket/src/transport/mod.rs`'s `websocket_proxy_with` logs the resulting string at
    /// `error!`. Both go through [`redact_userinfo`], which is the only spelling of the input this
    /// function may produce.
    pub fn parse(url: &str) -> Result<WebsocketProxy, String> {
        let url = url.trim();
        let (scheme, rest) = url.split_once("://").ok_or_else(|| {
            format!(
                "ws proxy url has no scheme: {:?} (want socks5h://host:port)",
                redact_userinfo(url)
            )
        })?;
        let remote_dns = match scheme.to_ascii_lowercase().as_str() {
            "socks5h" => true,
            "socks5" => false,
            other => {
                return Err(format!(
                    "unsupported ws proxy scheme {other:?} (only socks5:// and socks5h:// are supported)"
                ));
            }
        };
        // strip a trailing path/query — `socks5h://host:1080/` is a legal spelling
        let rest = rest.split(['/', '?']).next().unwrap_or("");
        // optional RFC-1929 userinfo; rsplit so a ':' or '@' inside the password can't confuse the
        // host split (the LAST '@' separates userinfo from the authority).
        let (auth, authority) = match rest.rsplit_once('@') {
            Some((userinfo, authority)) => {
                let (u, p) = userinfo.split_once(':').unwrap_or((userinfo, ""));
                if u.is_empty() {
                    return Err("ws proxy url has an empty username".to_string());
                }
                (Some((u.to_string(), p.to_string())), authority)
            }
            None => (None, rest),
        };
        let (host, port) = split_host_port(authority, 1080)?;
        if host.is_empty() {
            return Err(format!("ws proxy url has no host: {:?}", redact_userinfo(url)));
        }
        Ok(WebsocketProxy { host, port, remote_dns, auth })
    }
}

/// `url` with any RFC-1929 userinfo replaced by `<redacted>@` — the ONLY spelling of a raw proxy
/// url that may appear in an error message.
///
/// Independent of [`WebsocketProxy::parse`]'s own splitting because it must work on inputs the parse
/// REJECTED, including one with no scheme. It re-derives the authority the same way (after `://`,
/// or the whole string, up to the first `/` or `?`) and cuts at the LAST `@`, matching `parse`'s
/// `rsplit_once('@')` so a `:` or `@` inside a password cannot shift the cut. Host, port, scheme
/// and path are KEPT: they make the message actionable and none is a secret.
fn redact_userinfo(url: &str) -> String {
    let (scheme, rest) = match url.split_once("://") {
        Some((scheme, rest)) => (Some(scheme), rest),
        None => (None, url),
    };
    let (authority, tail) = match rest.find(['/', '?']) {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, ""),
    };
    let authority = match authority.rsplit_once('@') {
        Some((_userinfo, hostport)) => format!("<redacted>@{hostport}"),
        None => authority.to_string(),
    };
    match scheme {
        Some(scheme) => format!("{scheme}://{authority}{tail}"),
        None => format!("{authority}{tail}"),
    }
}

/// The TCP target a `ws://`/`wss://` URL dials: `(host, port)`, defaulting `443` for TLS schemes
/// and `80` for plain `ws`. Shared by the bounded direct arm and the proxied arm, so the two cannot
/// drift on how a URL is read.
pub fn websocket_target(url: &str) -> Result<(String, u16), String> {
    let uri: tungstenite::http::Uri = url.parse().map_err(|e| format!("bad ws url: {e}"))?;
    let host = uri.host().ok_or("ws url has no host")?.to_string();
    let tls = uri.scheme_str() != Some("ws"); // wss (or unspecified) defaults to TLS/443
    let port = uri.port_u16().unwrap_or(if tls { 443 } else { 80 });
    Ok((host, port))
}

/// The TLS + WebSocket handshake of `request` over the dialed `tcp`, every blocking READ of it bounded
/// by `window` (the module doc: a read bound, cleared again once the socket is up).
///
/// The option is set on the TCP stream BEFORE `tungstenite::client_tls` takes it, because that call
/// is where the handshake blocks (the TLS handshake itself runs lazily inside it, on the first write).
/// Setting it is best-effort, like [`crate::websocket::configure_websocket_stream`]'s options: a failure leaves the
/// handshake as unbounded as it was, never fails a connect that would have worked.
fn handshake_bounded<R: IntoClientRequest>(
    request: R,
    tcp: TcpStream,
    window: Duration,
) -> Result<WsSocket, String> {
    let request = naming_this_client(request)?;
    let _ = tcp.set_read_timeout(Some(window));
    let (socket, _response) =
        tungstenite::client_tls(request, tcp).map_err(|e| handshake_failed(e, window))?;
    clear_read_timeout(&socket);
    Ok(socket)
}

/// `request` as the upgrade this client sends: [`crate::http::USER_AGENT`] unless the caller set a `User-Agent` of its
/// own, which is kept. A request that does not build is `tungstenite::connect`'s own error, in its words.
fn naming_this_client<R: IntoClientRequest>(request: R) -> Result<Request, String> {
    let mut request = request.into_client_request().map_err(|e| e.to_string())?;
    request
        .headers_mut()
        .entry(tungstenite::http::header::USER_AGENT)
        .or_insert(tungstenite::http::HeaderValue::from_static(crate::http::USER_AGENT));
    Ok(request)
}

/// Undo the handshake window on the socket's TCP stream, so the socket leaves [`connect_websocket`] with no
/// read timeout, as it always has.
fn clear_read_timeout(socket: &WsSocket) {
    match socket.get_ref() {
        MaybeTlsStream::Plain(s) => {
            let _ = s.set_read_timeout(None);
        }
        MaybeTlsStream::Rustls(t) => {
            let _ = t.get_ref().set_read_timeout(None);
        }
        _ => {}
    }
}

/// A failed handshake as a one-line error. A read that timed out is `Interrupted` on a platform whose
/// timeout is `WouldBlock` (Linux) and an `Io(TimedOut)` failure where it is not (Windows); both are
/// the same event and say so, instead of `Interrupted handshake (WouldBlock)`.
fn handshake_failed<R: tungstenite::handshake::HandshakeRole>(
    e: tungstenite::HandshakeError<R>,
    window: Duration,
) -> String {
    match e {
        tungstenite::HandshakeError::Interrupted(_) => no_answer(window),
        tungstenite::HandshakeError::Failure(err) if is_timeout(&err) => no_answer(window),
        tungstenite::HandshakeError::Failure(err) => err.to_string(),
    }
}

fn no_answer(window: Duration) -> String {
    format!("ws handshake: no answer within {window:?}")
}

/// Dial `url` and complete the TLS + WebSocket handshake, optionally through a SOCKS5 `proxy`.
///
/// The THREE arms:
/// 1. `proxy: None, connect_timeout: None` ⇒ `tungstenite::connect(url)`, the unbounded dial.
/// 2. `proxy: None, connect_timeout: Some(bound)` ⇒ [`crate::dial::dial_bounded`] + [`handshake_bounded`]:
///    resolution and every resolved address under ONE `bound`, then the handshake with each read
///    under the same `bound` (see the module doc).
/// 3. `proxy: Some(p), connect_timeout: Some(bound)` ⇒ a SOCKS5 CONNECT to the ws target through `p` (remote
///    DNS when the URL said `socks5h`) under ONE `bound`, then [`handshake_bounded`] over the tunnelled TCP
///    stream (the module doc). The socket type is IDENTICAL (`WebSocket<MaybeTlsStream<TcpStream>>`), so
///    everything downstream — `configure_websocket_stream`, the pump, the venue closures — is unchanged.
///    `Some(p)` with no window is refused by name and dials nothing.
///
/// Requires the crate's `socks-proxy` feature for arm 3; without it a `Some(proxy)` is a clean,
/// named error rather than a silent direct dial (which would defeat the geo-block the proxy exists
/// to clear).
pub fn connect_websocket(
    url: &str,
    proxy: Option<&WebsocketProxy>,
    connect_timeout: Option<Duration>,
) -> Result<WsSocket, String> {
    open(url, url, proxy, connect_timeout)
}

/// [`connect_websocket`] over an upgrade `request` the caller built from `url.into_client_request()`, for a
/// venue whose upgrade carries a header of its own. The arms and the bound are [`connect_websocket`]'s.
pub fn connect_websocket_request(
    request: Request,
    proxy: Option<&WebsocketProxy>,
    connect_timeout: Option<Duration>,
) -> Result<WsSocket, String> {
    let url = request.uri().to_string();
    open(&url, request, proxy, connect_timeout)
}

/// The three arms, over `url` for where to dial and `request` for what to say there.
fn open<R: IntoClientRequest>(
    url: &str,
    request: R,
    proxy: Option<&WebsocketProxy>,
    connect_timeout: Option<Duration>,
) -> Result<WsSocket, String> {
    let Some(p) = proxy else {
        return match connect_timeout {
            None => {
                Ok(tungstenite::connect(naming_this_client(request)?).map_err(|e| e.to_string())?.0)
            }
            Some(bound) => {
                let (host, port) = websocket_target(url)?;
                let tcp = dial_bounded(&host, port, bound, None).map_err(|e| e.to_string())?;
                handshake_bounded(request, tcp, bound)
            }
        };
    };
    let (host, port) = websocket_target(url)?;
    let Some(bound) = connect_timeout else {
        return Err(format!(
            "ws proxy {p} for {host}:{port}: a tunnelled dial takes a window, and none was given"
        ));
    };
    let tcp = socks_connect(p, &host, port, bound)?;
    handshake_bounded(request, tcp, bound).map_err(|e| format!("via {p}: {e}"))
}

/// SOCKS5 CONNECT to `host:port` through `p`, every step inside ONE `bound` (the module doc), yielding the
/// tunnelled `TcpStream` the TLS + WS handshake then rides, with no timeout left on it. An IP-literal target goes as
/// an address; a name goes to the proxy under `socks5h` (`remote_dns`, the whole point of it) and is resolved here,
/// first address, under plain `socks5`.
#[cfg(feature = "socks-proxy")]
fn socks_connect(
    p: &WebsocketProxy,
    host: &str,
    port: u16,
    bound: Duration,
) -> Result<TcpStream, String> {
    use crate::dial::resolve_with_deadline;
    use crate::socks::{Target, TcpWithin, socks5};
    use std::net::SocketAddr;
    use std::time::Instant;
    let deadline = Instant::now() + bound;
    let failed = |why: String| format!("socks5 connect to {host}:{port} via {p}: {why}");
    let tcp = dial_bounded(&p.host, p.port, bound, None).map_err(|e| failed(e.to_string()))?;
    let target = match host.trim_matches(['[', ']']).parse::<std::net::IpAddr>() {
        Ok(ip) => Target::Address(SocketAddr::new(ip, port)),
        Err(_) if p.remote_dns => Target::Name(host, port),
        Err(_) => {
            let left = deadline.saturating_duration_since(Instant::now());
            let addrs =
                resolve_with_deadline(host, port, left).map_err(|e| failed(e.to_string()))?;
            let first =
                addrs.first().ok_or_else(|| failed(format!("no address for {host}:{port}")))?;
            Target::Address(*first)
        }
    };
    let auth = p.auth.as_ref().map(|(user, password)| (user.as_str(), password.as_str()));
    // The password goes to the proxy and NEVER into the words: `socks5`'s own carry none.
    socks5(&mut TcpWithin { tcp: &tcp, deadline }, auth, &target).map_err(|e| {
        failed(match e.kind() {
            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut => {
                format!("no answer within {bound:?}")
            }
            _ => e.to_string(),
        })
    })?;
    let _ = tcp.set_read_timeout(None);
    let _ = tcp.set_write_timeout(None);
    Ok(tcp)
}

#[cfg(not(feature = "socks-proxy"))]
fn socks_connect(
    p: &WebsocketProxy,
    host: &str,
    port: u16,
    _bound: Duration,
) -> Result<TcpStream, String> {
    Err(format!(
        "ws proxy {p} requested for {host}:{port} but vike-transport was built without the \
         `socks-proxy` feature"
    ))
}

#[path = "websocket_proxy_tests.rs"]
#[cfg(test)]
mod websocket_proxy_tests;
