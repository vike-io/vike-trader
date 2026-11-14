//! **The SOCKS dial of every tunnelled Polymarket request, inside the agent's own time limit.**
//! `crates/bridges/polymarket/src/transport/mod.rs`'s `agent_through` installs [`SocksDial`].
//!
//! ⚠ **ureq's own `SocksConnector` cannot be bounded from outside.** It runs the dial on a scoped
//! thread and waits with `recv_timeout`, but leaving the scope JOINS that thread, which sits in the
//! `socks` crate's `TcpStream::connect` and blocking reads, and those take no timeout. So
//! `timeout_global` reached the caller only when the dial ended by itself: a test call took about
//! 130 s on a black-holed proxy (the kernel's SYN retries) where the limit was 5 s, and a proxy that
//! accepts and then says nothing, which is what a hung `ssh -D` does, held the caller for ever. Order
//! submits ride this agent.
//!
//! So this dials the proxy with ureq's `TcpConnector` and speaks SOCKS over that socket's own read and
//! write timeouts, every step spending ONE deadline. There is no helper thread, so nothing outlives
//! the deadline and no connection can land after it. The wire is what the `socks` crate sent, byte for
//! byte (`crates/bridges/polymarket/src/transport/transport_tests.rs`'s
//! `a_tunnel_speaks_each_scheme_and_carries_the_request`, written against that crate first).
//!
//! ⚠ **What a failure here means: the request was never sent.** A connector runs before the first byte
//! of the request exists, and TLS starts after it, so every way this can fail, running out of time
//! included, is `ureq::Error::ConnectProxyFailed`, which
//! `vike_bridge_core::transport::send_error_is_definite` calls unsent, and a submit ends as a terminal
//! `OrderRejected`. A `Timeout` would read as "may have landed", which is false here and would leave
//! the order to the stuck-order watchdog. A failure AFTER the tunnel is up keeps the ordinary rule:
//! past the write it is an UNKNOWN submit, never a reject
//! (`crates/bridges/polymarket/src/execution/rest.rs`'s `send_error`).
//!
//! ⚠ **It never goes direct**: the chain `agent_through` builds is this connector and TLS, with no
//! direct dial in it to fall through to. It answers a transport or an error, never `None`.
//!
//! ⚠ **A dial spends at most [`SOCKS_DIAL_LIMIT`], however long the agent may wait.** The exec thread
//! serves orders and cancels one at a time, and a dead proxy cannot answer any of them, so it must
//! not hold that thread for the order path's whole limit. The other half is
//! `crates/bridges/polymarket/src/execution/mod.rs`'s `MAX_SUBMIT_AGE_MS`: what still waits behind
//! it past that age is refused, never sent late.
//!
//! **The user channel's WS dial takes the same handshake** ([`websocket_tunnel`], for `crate::transport`'s
//! `user_websocket_dial`): the shared WS dial spends no window through a tunnel, and that channel alone
//! carries a resting order's end. Same rule: one deadline over the socket to the proxy and the whole
//! SOCKS handshake, through the socket's own timeouts, and no helper thread.

use std::io::{self, Read, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::{Duration, Instant};

use ureq::ProxyProtocol;
use ureq::unversioned::transport::{
    ConnectionDetails, Connector, NextTimeout, TcpConnector, Transport, TransportAdapter,
};

/// The most ONE dial may take: the lookup of the proxy's name, the socket to it and the whole SOCKS
/// handshake, the proxy's own connection to the venue included. TLS and the request come after the
/// dial and keep the agent's limit, so a slow tunnel that does answer spends that one, not this.
///
/// Why 5 s: 20 dials through the Dublin tunnel measured on the CI box on 2026-11-07 took 27 ms to 37 ms,
/// so this is over a hundred times the slowest, and it outlasts TCP's first two retries of a lost
/// packet (about 1 s and 3 s). A working tunnel does not reach it; a dead one fails here at once
/// rather than at the order path's 30 s.
pub(super) const SOCKS_DIAL_LIMIT: Duration = Duration::from_secs(5);

/// What [`SocksDial`] hands the next connector: the socket `TcpConnector` opened to the proxy.
type Tunnel = <TcpConnector as Connector<()>>::Out;

/// The connector: a tunnel through the agent's SOCKS proxy, or `ConnectProxyFailed` (module doc).
#[derive(Debug)]
pub(super) struct SocksDial;

impl Connector<()> for SocksDial {
    type Out = Tunnel;

    fn connect(
        &self,
        details: &ConnectionDetails<'_>,
        _chained: Option<()>,
    ) -> Result<Option<Tunnel>, ureq::Error> {
        // `not_zero` is `None` only for an agent with no limit, and `agent_through` always sets one.
        let limit = details
            .timeout
            .not_zero()
            .map_or(super::REST_TIMEOUT, |left| *left)
            .min(SOCKS_DIAL_LIMIT);
        tunnel(details, Instant::now() + limit).map(Some).map_err(|why| {
            // What was left of the agent's limit, a moment under it: said in the whole seconds it was.
            let seconds = limit.as_secs_f64().ceil();
            ureq::Error::ConnectProxyFailed(format!(
                "no tunnel through the SOCKS proxy (limit {seconds} s): {why}"
            ))
        })
    }
}

/// What is left of `deadline`, or the failure that says none is: a socket timeout refuses a zero.
fn left(deadline: Instant) -> io::Result<Duration> {
    let left = deadline.saturating_duration_since(Instant::now());
    if left.is_zero() { Err(io::ErrorKind::TimedOut.into()) } else { Ok(left) }
}

/// The socket's reads and writes, all spending ONE deadline, so a proxy answering a byte at a time
/// cannot stretch it.
struct Within<T: Transport> {
    socket: TransportAdapter<T>,
    deadline: Instant,
    reason: ureq::Timeout,
}

impl<T: Transport> Within<T> {
    fn spend(&mut self) -> io::Result<()> {
        let after = left(self.deadline)?.into();
        self.socket.set_timeout(NextTimeout { after, reason: self.reason });
        Ok(())
    }
}

impl<T: Transport> Read for Within<T> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        self.spend()?;
        self.socket.read(buffer)
    }
}

impl<T: Transport> Write for Within<T> {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.spend()?;
        self.socket.write(buffer)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Where the tunnel must lead.
enum Target<'a> {
    /// A name for the proxy to resolve (`socks5h`, `socks4a`).
    Name(&'a str, u16),
    /// An address: one this box resolved (`socks5`, `socks4`), or one the URL spelled.
    Address(SocketAddr),
}

fn target_of<'a>(
    details: &'a ConnectionDetails<'_>,
    proxy: &ureq::Proxy,
) -> Result<Target<'a>, ureq::Error> {
    if proxy.resolve_target() {
        return details.addrs.first().map(|a| Target::Address(*a)).ok_or(ureq::Error::HostNotFound);
    }
    let host = details.uri.host().ok_or(ureq::Error::HostNotFound)?;
    let port = details.uri.port_u16().unwrap_or(if details.needs_tls() { 443 } else { 80 });
    Ok(match host.trim_matches(['[', ']']).parse::<IpAddr>() {
        Ok(ip) => Target::Address(SocketAddr::new(ip, port)),
        Err(_) => Target::Name(host, port),
    })
}

fn tunnel(details: &ConnectionDetails<'_>, deadline: Instant) -> Result<Tunnel, ureq::Error> {
    let proxy = details.config.proxy().ok_or(ureq::Error::ConnectionFailed)?;
    let target = target_of(details, proxy)?;
    let reason = details.timeout.reason;
    // Under the dial's limit, not the agent's: a proxy declared by name may have nobody to answer it.
    let lookup = NextTimeout { after: left(deadline)?.into(), reason };
    let addrs = details.resolver.resolve(proxy.uri(), details.config, lookup)?;
    // The lookup spent some of the limit: the socket gets what is left of it.
    let timeout = NextTimeout { after: left(deadline)?.into(), reason };
    let to_proxy = ConnectionDetails {
        uri: proxy.uri(),
        addrs,
        config: details.config,
        request_level: details.request_level,
        resolver: details.resolver,
        now: (details.current_time)(),
        timeout,
        current_time: details.current_time.clone(),
        run_connector: details.run_connector.clone(),
    };
    let socket = Connector::<()>::connect(&TcpConnector::default(), &to_proxy, None)?
        .ok_or(ureq::Error::ConnectionFailed)?;
    let mut within = Within { socket: TransportAdapter::new(socket), deadline, reason };
    match proxy.protocol() {
        ProxyProtocol::Socks4 | ProxyProtocol::Socks4A => socks4(&mut within, &target)?,
        _ => {
            let auth = proxy.username().map(|user| (user, proxy.password().unwrap_or("")));
            socks5(&mut within, auth, &target)?;
        }
    }
    Ok(within.socket.into_inner())
}

/// The user channel's tunnel to `host:port` through `proxy`: the socket to the proxy and the SOCKS5
/// handshake, all spending `deadline` (the module doc). The socket comes back with none of it left
/// on it; the TLS and WebSocket handshake after it is the caller's, under its own window.
///
/// ⚠ Two names are looked up with no bound, as the shared dial looked them up: the proxy's own (the
/// declared default is the literal `127.0.0.1`, which looks up nothing) and, under plain `socks5`,
/// the target's (the declared default is `socks5h`, which sends the name).
#[cfg(feature = "polymarket")]
pub(super) fn websocket_tunnel(
    proxy: &vike_bridge_core::websocket_proxy::WebsocketProxy,
    host: &str,
    port: u16,
    deadline: Instant,
) -> io::Result<std::net::TcpStream> {
    use std::net::{TcpStream, ToSocketAddrs};
    let mut last = io::Error::other("the proxy's name has no address");
    let mut dialled = None;
    for addr in (proxy.host.as_str(), proxy.port).to_socket_addrs()? {
        match TcpStream::connect_timeout(&addr, left(deadline)?) {
            Ok(tcp) => {
                dialled = Some(tcp);
                break;
            }
            Err(e) => last = e,
        }
    }
    let tcp = dialled.ok_or(last)?;
    let target = match host.trim_matches(['[', ']']).parse::<IpAddr>() {
        Ok(ip) => Target::Address(SocketAddr::new(ip, port)),
        Err(_) if proxy.remote_dns => Target::Name(host, port),
        Err(_) => Target::Address(
            (host, port)
                .to_socket_addrs()?
                .next()
                .ok_or_else(|| io::Error::other("the target's name has no address"))?,
        ),
    };
    let auth = proxy.auth.as_ref().map(|(user, password)| (user.as_str(), password.as_str()));
    socks5(&mut TcpWithin { tcp: &tcp, deadline }, auth, &target)?;
    let _ = tcp.set_read_timeout(None);
    let _ = tcp.set_write_timeout(None);
    Ok(tcp)
}

/// A plain socket's reads and writes, all spending ONE deadline: [`Within`]'s twin for [`websocket_tunnel`].
#[cfg(feature = "polymarket")]
struct TcpWithin<'a> {
    tcp: &'a std::net::TcpStream,
    deadline: Instant,
}

#[cfg(feature = "polymarket")]
impl Read for TcpWithin<'_> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        self.tcp.set_read_timeout(Some(left(self.deadline)?))?;
        let mut tcp = self.tcp;
        tcp.read(buffer)
    }
}

#[cfg(feature = "polymarket")]
impl Write for TcpWithin<'_> {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.tcp.set_write_timeout(Some(left(self.deadline)?))?;
        let mut tcp = self.tcp;
        tcp.write(buffer)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// The handshake of RFC 1928, with RFC 1929's password when the proxy asks for it. Nothing here echoes
/// the password: it is a declared secret, and these words end up in an order's reject reason.
fn socks5(
    socket: &mut (impl Read + Write),
    auth: Option<(&str, &str)>,
    target: &Target<'_>,
) -> io::Result<()> {
    // "No password" is offered beside a password, so a proxy may waive it.
    socket.write_all(if auth.is_some() { &[5, 2, 2, 0] } else { &[5, 1, 0] })?;
    let mut chosen = [0u8; 2];
    socket.read_exact(&mut chosen)?;
    match (chosen, auth) {
        ([5, 0], _) => {}
        ([5, 2], Some((user, password))) => password_handshake(socket, user, password)?,
        ([5, _], _) => return Err(io::Error::other("the proxy takes no method it was offered")),
        _ => return Err(io::Error::other("not a SOCKS5 proxy")),
    }

    let mut connect = vec![5, 1, 0];
    match target {
        Target::Name(host, port) => {
            let len =
                u8::try_from(host.len()).map_err(|_| io::Error::other("host name too long"))?;
            connect.extend([3, len]);
            connect.extend(host.as_bytes());
            connect.extend(port.to_be_bytes());
        }
        Target::Address(SocketAddr::V4(a)) => {
            connect.push(1);
            connect.extend(a.ip().octets());
            connect.extend(a.port().to_be_bytes());
        }
        Target::Address(SocketAddr::V6(a)) => {
            connect.push(4);
            connect.extend(a.ip().octets());
            connect.extend(a.port().to_be_bytes());
        }
    }
    socket.write_all(&connect)?;

    // Two bytes first: a proxy that refuses may send no more than that.
    let mut reply = [0u8; 2];
    socket.read_exact(&mut reply)?;
    if reply[0] != 5 {
        return Err(io::Error::other("not a SOCKS5 proxy"));
    }
    if reply[1] != 0 {
        return Err(io::Error::other(socks5_refusal(reply[1])));
    }
    // The reserved byte and the kind of the bound address. The address itself is read and dropped:
    // left on the socket, it would be taken for the start of the HTTP response.
    let mut kind = [0u8; 2];
    socket.read_exact(&mut kind)?;
    let address = match kind[1] {
        1 => 4,
        4 => 16,
        3 => {
            let mut len = [0u8; 1];
            socket.read_exact(&mut len)?;
            usize::from(len[0])
        }
        _ => return Err(io::Error::other("a reply with no address kind SOCKS5 has")),
    };
    let mut bound = [0u8; 255 + 2];
    socket.read_exact(&mut bound[..address + 2])
}

fn password_handshake(
    socket: &mut (impl Read + Write),
    user: &str,
    password: &str,
) -> io::Result<()> {
    let too_long = |_| io::Error::other("a proxy user or password over 255 bytes");
    let mut message = vec![1, u8::try_from(user.len()).map_err(too_long)?];
    message.extend(user.as_bytes());
    message.push(u8::try_from(password.len()).map_err(too_long)?);
    message.extend(password.as_bytes());
    socket.write_all(&message)?;
    let mut reply = [0u8; 2];
    socket.read_exact(&mut reply)?;
    if reply != [1, 0] {
        return Err(io::Error::other("the proxy refused the password"));
    }
    Ok(())
}

/// A SOCKS5 refusal code in words, RFC 1928's.
fn socks5_refusal(code: u8) -> &'static str {
    match code {
        1 => "the proxy failed",
        2 => "the proxy's rules do not allow the connection",
        3 => "the proxy says the network is unreachable",
        4 => "the proxy says the host is unreachable",
        5 => "the proxy says the connection was refused",
        6 => "the proxy says the TTL expired",
        7 => "the proxy does not support the command",
        8 => "the proxy does not support the address kind",
        _ => "the proxy refused, with a code SOCKS5 does not have",
    }
}

/// The handshake of SOCKS4, and of SOCKS4a when the target is a name. The user id is empty.
fn socks4(socket: &mut (impl Read + Write), target: &Target<'_>) -> io::Result<()> {
    let (port, address, name) = match target {
        Target::Name(host, port) => (*port, Ipv4Addr::new(0, 0, 0, 1), Some(*host)),
        Target::Address(SocketAddr::V4(a)) => (a.port(), *a.ip(), None),
        Target::Address(SocketAddr::V6(_)) => return Err(io::Error::other("SOCKS4 has no IPv6")),
    };
    let mut request = vec![4, 1];
    request.extend(port.to_be_bytes());
    request.extend(address.octets());
    request.push(0);
    if let Some(name) = name {
        request.extend(name.as_bytes());
        request.push(0);
    }
    socket.write_all(&request)?;
    let mut reply = [0u8; 8];
    socket.read_exact(&mut reply)?;
    if reply[..2] != [0, 90] {
        return Err(io::Error::other("the proxy refused the connection"));
    }
    Ok(())
}
