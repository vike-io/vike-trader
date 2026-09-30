//! Optional **SOCKS5 egress for the blocking WS dial** — the WebSocket twin of the proxy `ureq`
//! already applies to REST (polymarket-workstreams spec §0.1).
//!
//! Context: Polymarket is US-geo-blocked, so `crates/bridges/polymarket/src/egress.rs` routes its
//! HTTP through a `socks5h://` tunnel (the arbdub/Dublin SSH forward). Every WebSocket, however,
//! dialled DIRECT: [`crate::market_pump::connect_market_stream`] does a raw `TcpStream` +
//! `tungstenite::client_tls`, and this crate had no notion of a proxy at all. From a blocked
//! location the market/user feeds therefore could not connect — the single blocker §0 of the spec
//! records.
//!
//! **This is a SHARED HOME** (`market_pump` serves every venue), so per CLAUDE.md's per-venue
//! capability-map playbook the PROXY extension is ADDITIVE and INERT: [`connect_ws`] with
//! `proxy: None` takes the same two arms `connect_market_stream` had before this module existed —
//! unbounded (`tungstenite::connect`) and bounded — so binance/bybit/okx/aster/deribit/hyperliquid,
//! none of which ever pass a proxy, never touch the proxy machinery. Only a caller that explicitly
//! hands a [`WsProxy`] takes that arm. ⚠ The BOUNDED arm is no longer byte-identical to what it was
//! then, and deliberately so: the next section is what changed in it and why.
//!
//! **Dependency:** the SOCKS5 client is the `socks` crate, which was ALREADY in the tree
//! (`ureq`'s `socks-proxy` feature, enabled by `vike-polymarket/polymarket` — see `Cargo.lock`).
//! Reusing it keeps the "ONE transport stack" invariant (`deny.toml [bans].deny`) intact: no
//! second HTTP/WS/TLS/curve crate joins the audit surface. It is behind this crate's OPT-IN
//! `socks-proxy` feature so a default build compiles no proxy code at all.
//!
//! **`socks5h` vs `socks5`** is honoured, and it matters: plain `socks5` resolves the target DNS
//! LOCALLY (which in a geo-blocked/DNS-poisoned network resolves to the wrong host — exactly the
//! failure `crates/bridges/polymarket/src/egress.rs`'s `proxy_url` documents), while `socks5h` hands
//! the DOMAIN to the proxy and lets it resolve remotely. [`WsProxy::remote_dns`] carries that bit
//! and [`connect_ws`] acts on it.
//!
//! **What "bounded" covers, stated as the whole TCP phase.** [`connect_ws`]'s `Some(bound)` arm is
//! ONE wall-clock deadline over BOTH blocking steps in front of the handshake — name resolution and
//! every resolved address in turn ([`dial_bounded`]) — not a timeout on the last of them. Two
//! defects lived in the gap between those readings and both were real:
//!
//! - **Resolution was unbounded.** The arm called `to_socket_addrs()` before
//!   `TcpStream::connect_timeout`, and that is a blocking glibc `getaddrinfo` with no timeout
//!   argument in its API at all. A dead or slow resolver hung the dial for as long as the C library
//!   felt like, so a caller sizing a teardown budget off `connect_timeout` was sizing it off the
//!   second half of a two-half operation. std offers no bounded resolver, so [`resolve_with_deadline`]
//!   runs the call on a throwaway thread and abandons it at the deadline — the usual shape, and the
//!   only one available without adding a resolver crate (which `deny.toml`'s ONE-transport-stack ban
//!   exists to prevent).
//! - **Only the FIRST address was tried.** `to_socket_addrs()?.next()` dropped the rest, while the
//!   `tungstenite::connect` arm beside it tries them all. MEASURED from the CI box on 2026-08-08,
//!   `stream.binance.com` resolves to **6** addresses and `fstream.binance.com` to **8**, so on the
//!   venue this bound was introduced for, first-only turned an unreachable edge into a failed
//!   connect where the unbounded arm would have succeeded — boundedness bought with availability,
//!   and nobody was told. [`dial_bounded`] iterates.
//!
//! **Known residual (documented, not hidden):** the deadline ends where the handshake begins — the
//! TLS + WS handshake `tungstenite::client_tls` performs after it is unbounded. And with a proxy the
//! bound covers nothing at all: neither the SOCKS greet/CONNECT exchange nor the handshake, because
//! the `socks` crate exposes no timeout on its own. The proxy is normally a localhost SSH forward,
//! so that exposure is a hung tunnel rather than a black-holed internet route.

use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::time::{Duration, Instant};

use crate::ws::WsSocket;

/// A parsed SOCKS5 endpoint for the WS lane.
///
/// Built from the same URL string the HTTP lane hands `ureq::Proxy::new` ([`WsProxy::parse`]), so
/// a venue never grows a SECOND place to say where its tunnel is.
#[derive(Clone, PartialEq, Eq)]
pub struct WsProxy {
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
impl std::fmt::Debug for WsProxy {
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

impl std::fmt::Display for WsProxy {
    /// The endpoint WITHOUT credentials — safe for a status line.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let scheme = if self.remote_dns { "socks5h" } else { "socks5" };
        write!(f, "{scheme}://{}:{}", self.host, self.port)
    }
}

impl WsProxy {
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
    /// `crates/bridges/polymarket/src/egress.rs`'s `ws_proxy_with` logs the resulting string at
    /// `error!`. Both go through [`redact_userinfo`], which is the only spelling of the input this
    /// function may produce.
    pub fn parse(url: &str) -> Result<WsProxy, String> {
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
        Ok(WsProxy { host, port, remote_dns, auth })
    }
}

/// `url` with any RFC-1929 userinfo replaced by `<redacted>@` — the ONLY spelling of a raw proxy
/// url that may appear in an error message.
///
/// Deliberately independent of [`WsProxy::parse`]'s own splitting: it must work on inputs the parse
/// REJECTED, including one with no scheme at all, so it cannot be expressed as "the tail after the
/// parse got that far". It re-derives the authority the same way — everything after `://` (or the
/// whole string), up to the first `/` or `?` — and replaces everything up to and including the LAST
/// `@` in it, matching `parse`'s own `rsplit_once('@')` so a `:` or `@` inside a password cannot
/// shift the cut.
///
/// The host, port, scheme and any path are KEPT: they are what makes the message actionable, and
/// none of them is a secret.
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

/// Split `host:port` / `host` / `[v6]:port` / `[v6]`, defaulting the port. Shared by the proxy
/// URL parse and the WS TARGET parse below (a ws:// URL's authority has the identical shape).
fn split_host_port(authority: &str, default_port: u16) -> Result<(String, u16), String> {
    if let Some(rest) = authority.strip_prefix('[') {
        let (v6, tail) = rest
            .split_once(']')
            .ok_or_else(|| format!("unterminated IPv6 literal in {authority:?}"))?;
        let port = match tail.strip_prefix(':') {
            Some(p) => p.parse().map_err(|_| format!("bad port in {authority:?}"))?,
            None => default_port,
        };
        return Ok((v6.to_string(), port));
    }
    match authority.rsplit_once(':') {
        Some((h, p)) => {
            Ok((h.to_string(), p.parse().map_err(|_| format!("bad port in {authority:?}"))?))
        }
        None => Ok((authority.to_string(), default_port)),
    }
}

/// The TCP target a `ws://`/`wss://` URL dials: `(host, port)`, defaulting `443` for TLS schemes
/// and `80` for plain `ws`. Extracted verbatim from `market_pump::connect_market_stream`'s bounded
/// arm so the direct and proxied paths can never drift on how a URL is read.
pub fn ws_target(url: &str) -> Result<(String, u16), String> {
    let uri: tungstenite::http::Uri = url.parse().map_err(|e| format!("bad ws url: {e}"))?;
    let host = uri.host().ok_or("ws url has no host")?.to_string();
    let tls = uri.scheme_str() != Some("ws"); // wss (or unspecified) defaults to TLS/443
    let port = uri.port_u16().unwrap_or(if tls { 443 } else { 80 });
    Ok((host, port))
}

/// Run one blocking call on a throwaway thread and stop waiting for it after `bound`.
///
/// `None` means the deadline won, and the thread is deliberately **abandoned, not joined** — that is
/// the whole point: joining is the blocking wait being escaped. It runs to completion in the
/// background, sends into a receiver that is already gone (a plain `Err` it ignores), and exits. The
/// cost of a timeout is therefore one leaked thread for as long as the blocked call takes, which for
/// `getaddrinfo` is bounded by the C library's own retry policy (glibc's `resolv.conf` defaults —
/// `timeout:5 attempts:2` — per nameserver). That is acceptable HERE and would not be everywhere: a
/// dial happens at most once per reconnect backoff (>=500 ms on the fastest roster row), so the
/// leaked threads cannot accumulate faster than they retire.
///
/// There is no bounded resolver in std, and reaching for one would add a DNS crate to a workspace
/// whose `deny.toml` bans a second transport stack on purpose — so this is the shape, not a
/// preference.
fn call_with_deadline<T: Send + 'static>(
    bound: Duration,
    f: impl FnOnce() -> T + Send + 'static,
) -> Option<T> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(f());
    });
    rx.recv_timeout(bound).ok()
}

/// `host:port` resolved to every address it names, under a wall-clock `bound`.
///
/// MEASURED on the CI box 2026-08-08: a healthy lookup of `stream.binance.com` costs 0–1 ms served from
/// the local `systemd-resolved` stub and 6–10 ms when it has to go upstream, so the bound charges
/// the healthy path nothing worth counting. It exists for the unhealthy one, which has no ceiling of
/// its own that this code can see.
fn resolve_with_deadline(
    host: &str,
    port: u16,
    bound: Duration,
) -> Result<Vec<SocketAddr>, String> {
    let owned = host.to_string();
    match call_with_deadline(bound, move || {
        (owned.as_str(), port)
            .to_socket_addrs()
            .map(|it| it.collect::<Vec<_>>())
            .map_err(|e| e.to_string())
    }) {
        Some(Ok(addrs)) => Ok(addrs),
        Some(Err(e)) => Err(format!("resolve {host}:{port}: {e}")),
        None => Err(format!("resolve {host}:{port}: no answer within {bound:?}")),
    }
}

/// The bounded TCP phase of [`connect_ws`]: resolve `host:port`, then try EVERY address it named, in
/// order, with resolution and all attempts spending ONE shared `bound`.
///
/// **One deadline, not one per step.** A per-step bound would make the real ceiling
/// `resolve + n × bound`, where `n` is a number the venue's DNS chooses — so the constant a caller
/// budgets against (`crate::pump_spec`'s `CONNECT_10S`, spent by
/// `crates/vike-datahub/src/recorder.rs`'s `FEED_STOP_BUDGET_SECS`) would not be a ceiling
/// on anything. Sharing it keeps the published number true whatever DNS returns.
///
/// **The cost of sharing, stated.** A first address that black-holes can consume the window before
/// the second is tried, where per-attempt bounds would have reached it. That is still strictly
/// better than what this replaced on both axes: `tungstenite::connect` also walks the addresses in
/// order, with NO bound at all, so a black-holed first address there costs the OS's full SYN ladder
/// (~127 s on Linux defaults) before the second is attempted. The common multi-address failure —
/// an edge answering RST or ICMP unreachable — costs milliseconds and falls straight through.
fn dial_bounded(host: &str, port: u16, bound: Duration) -> Result<TcpStream, String> {
    dial_bounded_with(host, port, bound, &resolve_with_deadline, &|addr, timeout| {
        TcpStream::connect_timeout(addr, timeout)
    })
}

/// The name-resolution step of [`dial_bounded_with`]: `(host, port, bound) -> addresses`.
/// Production is [`resolve_with_deadline`]; a test hands a canned list, or a slow one.
type ResolveStep<'a> = dyn Fn(&str, u16, Duration) -> Result<Vec<SocketAddr>, String> + 'a;

/// The per-address connect step of [`dial_bounded_with`]: `(addr, timeout) -> stream`. Production is
/// `TcpStream::connect_timeout`; a test records the timeout it was HANDED, which is the only way to
/// observe the deadline arithmetic rather than infer it from wall-clock luck.
type ConnectStep<'a> = dyn Fn(&SocketAddr, Duration) -> std::io::Result<TcpStream> + 'a;

/// [`dial_bounded`] with its two blocking steps injected, so the deadline arithmetic is testable
/// with no network, no DNS and no wall-clock luck. Production passes the real pair.
fn dial_bounded_with(
    host: &str,
    port: u16,
    bound: Duration,
    resolve: &ResolveStep<'_>,
    connect: &ConnectStep<'_>,
) -> Result<TcpStream, String> {
    let deadline = Instant::now() + bound;
    let addrs = resolve(host, port, bound)?;
    if addrs.is_empty() {
        return Err(format!("no address for {host}:{port}"));
    }
    let mut last = "none attempted".to_string();
    for (i, addr) in addrs.iter().enumerate() {
        // Whatever resolution and the earlier attempts already spent is GONE from the window — this
        // is what makes `bound` a ceiling on the phase rather than on its last step.
        let remaining = deadline.saturating_duration_since(Instant::now());
        // `TcpStream::connect_timeout` rejects a zero Duration, and an exhausted window is a real
        // outcome that must be reported as itself: "the bound ran out", not "connection refused".
        if remaining.is_zero() {
            return Err(format!(
                "connect {host}:{port}: dial bound {bound:?} spent after {i} of {} address(es); \
                 last error: {last}",
                addrs.len()
            ));
        }
        match connect(addr, remaining) {
            Ok(tcp) => return Ok(tcp),
            Err(e) => last = format!("{addr}: {e}"),
        }
    }
    Err(format!(
        "connect {host}:{port}: all {} resolved address(es) failed; last error: {last}",
        addrs.len()
    ))
}

/// Dial `url` and complete the TLS + WebSocket handshake, optionally through a SOCKS5 `proxy`.
///
/// The THREE arms, in the order a reader should check them for inertness:
/// 1. `proxy: None, connect_timeout: None` ⇒ `tungstenite::connect(url)` — the pre-existing
///    unbounded dial, verbatim.
/// 2. `proxy: None, connect_timeout: Some(bound)` ⇒ [`dial_bounded`] + `tungstenite::client_tls`:
///    resolution and every resolved address under ONE `bound` (see that function, and the module
///    doc for the two defects the previous spelling of this arm carried).
/// 3. `proxy: Some(p)` ⇒ a SOCKS5 CONNECT to the ws target through `p` (remote DNS when the URL
///    said `socks5h`), then `tungstenite::client_tls` over the tunnelled TCP stream. The resulting
///    socket type is IDENTICAL (`WebSocket<MaybeTlsStream<TcpStream>>`), so everything downstream
///    — `configure_ws_stream`'s read timeout + `TCP_NODELAY`, the pump, the venue closures — is
///    unchanged.
///
/// Requires the crate's `socks-proxy` feature for arm 3; without it a `Some(proxy)` is a clean,
/// named error rather than a silent direct dial (which would defeat the geo-block the proxy exists
/// to clear).
pub fn connect_ws(
    url: &str,
    proxy: Option<&WsProxy>,
    connect_timeout: Option<Duration>,
) -> Result<WsSocket, String> {
    let Some(p) = proxy else {
        return match connect_timeout {
            None => Ok(tungstenite::connect(url).map_err(|e| e.to_string())?.0),
            Some(bound) => {
                let (host, port) = ws_target(url)?;
                let tcp = dial_bounded(&host, port, bound)?;
                Ok(tungstenite::client_tls(url, tcp).map_err(|e| e.to_string())?.0)
            }
        };
    };
    let (host, port) = ws_target(url)?;
    let tcp = socks_connect(p, &host, port)?;
    Ok(tungstenite::client_tls(url, tcp).map_err(|e| format!("ws handshake via {p}: {e}"))?.0)
}

/// SOCKS5 CONNECT to `host:port` through `p`, yielding the tunnelled `TcpStream` the TLS + WS
/// handshake then rides. `remote_dns` decides whether the DOMAIN or a locally-resolved IP goes on
/// the wire — the whole point of `socks5h`.
#[cfg(feature = "socks-proxy")]
fn socks_connect(p: &WsProxy, host: &str, port: u16) -> Result<TcpStream, String> {
    // `(&str, u16)` → `TargetAddr::Domain` (proxy-side DNS) unless the host is already a literal
    // IP; spelled explicitly so the two modes read as the deliberate choice they are.
    let target = if p.remote_dns {
        socks::TargetAddr::Domain(host.to_string(), port)
    } else {
        let addr = (host, port)
            .to_socket_addrs()
            .map_err(|e| format!("resolve {host}:{port}: {e}"))?
            .next()
            .ok_or_else(|| format!("no address for {host}:{port}"))?;
        socks::TargetAddr::Ip(addr)
    };
    let proxy_addr = (p.host.as_str(), p.port);
    let stream = match &p.auth {
        // NOTE the password is passed to the socks client and NEVER into the error string below.
        Some((user, pass)) => {
            socks::Socks5Stream::connect_with_password(proxy_addr, target, user, pass)
        }
        None => socks::Socks5Stream::connect(proxy_addr, target),
    }
    .map_err(|e| format!("socks5 connect to {host}:{port} via {p}: {e}"))?;
    Ok(stream.into_inner())
}

#[cfg(not(feature = "socks-proxy"))]
fn socks_connect(p: &WsProxy, host: &str, port: u16) -> Result<TcpStream, String> {
    Err(format!(
        "ws proxy {p} requested for {host}:{port} but vike-bridge-core was built without the \
         `socks-proxy` feature"
    ))
}

#[path = "ws_proxy_tests.rs"]
#[cfg(test)]
mod ws_proxy_tests;
