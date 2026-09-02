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
                ))
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
/// `crates/vike-recorder/src/recorder_cli.rs`'s `FEED_STOP_BUDGET_SECS`) would not be a ceiling
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_socks5h_with_remote_dns() {
        let p = WsProxy::parse("socks5h://127.0.0.1:1080").unwrap();
        assert_eq!(p.host, "127.0.0.1");
        assert_eq!(p.port, 1080);
        assert!(p.remote_dns, "socks5h MUST resolve remotely — that is what clears the geo-block");
        assert!(p.auth.is_none());
    }

    #[test]
    fn parses_plain_socks5_as_local_dns() {
        let p = WsProxy::parse("socks5://<host>:9050").unwrap();
        assert!(!p.remote_dns);
        assert_eq!((p.host.as_str(), p.port), ("<host>", 9050));
    }

    #[test]
    fn defaults_the_port_to_1080() {
        assert_eq!(WsProxy::parse("socks5h://tunnel.local").unwrap().port, 1080);
    }

    #[test]
    fn tolerates_a_trailing_slash() {
        let p = WsProxy::parse("socks5h://127.0.0.1:1080/").unwrap();
        assert_eq!((p.host.as_str(), p.port), ("127.0.0.1", 1080));
    }

    #[test]
    fn parses_userinfo() {
        let p = WsProxy::parse("socks5h://alice:s3cr3t@127.0.0.1:1080").unwrap();
        assert_eq!(p.auth, Some(("alice".to_string(), "s3cr3t".to_string())));
        assert_eq!(p.host, "127.0.0.1");
    }

    #[test]
    fn parses_ipv6_literal() {
        let p = WsProxy::parse("socks5h://[::1]:1080").unwrap();
        assert_eq!((p.host.as_str(), p.port), ("::1", 1080));
        assert_eq!(WsProxy::parse("socks5h://[::1]").unwrap().port, 1080);
    }

    #[test]
    fn rejects_a_missing_or_unsupported_scheme() {
        assert!(WsProxy::parse("127.0.0.1:1080").is_err());
        assert!(WsProxy::parse("http://127.0.0.1:8080").is_err());
        assert!(WsProxy::parse("socks4://127.0.0.1:1080").is_err());
        assert!(WsProxy::parse("").is_err());
    }

    #[test]
    fn rejects_a_bad_port() {
        assert!(WsProxy::parse("socks5h://127.0.0.1:notaport").is_err());
    }

    /// **A parse ERROR must not carry the URL's userinfo either.**
    ///
    /// [`WsProxy`]'s `Debug`/`Display` redact, but a REJECTED url never becomes a `WsProxy` — it
    /// comes back as a `String`, and `crates/bridges/polymarket/src/egress.rs`'s `ws_proxy_with`
    /// logs that string at `error!` under a comment asserting "the message carries the parse
    /// error, never the URL's userinfo". Two arms interpolate the RAW input, and both are
    /// reachable with credentials in it: a url with no scheme (`user:pass@host:port` — the shape
    /// somebody writes when they copy an authority out of another config) and one whose authority
    /// has an empty host.
    ///
    /// The assertion is on the credential strings, not on the message format: it fails if the
    /// username or the password appears in ANY spelling. The last two rows are already safe and
    /// are pinned here so a future rewrite of the error strings cannot open them.
    #[test]
    fn a_parse_error_never_carries_the_proxy_credentials() {
        for url in [
            "alice:s3cr3t@127.0.0.1:1080",         // no scheme  → the raw-url arm
            "socks5h://alice:s3cr3t@:1080",        // no host    → the other raw-url arm
            "socks4://alice:s3cr3t@<host>:1080", // unsupported scheme (already safe)
            "socks5h://alice:s3cr3t@h:notaport",   // bad port (already safe)
        ] {
            let err = WsProxy::parse(url).unwrap_err();
            assert!(
                !err.contains("s3cr3t"),
                "proxy password leaked into the parse error for {url:?}: {err}"
            );
            assert!(
                !err.contains("alice"),
                "proxy username leaked into the parse error for {url:?}: {err}"
            );
        }
    }

    /// …and what the redaction KEEPS, which is what makes those errors still actionable: the
    /// scheme, host, port and path. A url with no userinfo passes through untouched, so the
    /// messages for the overwhelmingly common case are byte-identical to before.
    #[test]
    fn redact_userinfo_keeps_everything_that_is_not_a_credential() {
        assert_eq!(redact_userinfo("socks5h://127.0.0.1:1080"), "socks5h://127.0.0.1:1080");
        assert_eq!(redact_userinfo("socks5h://tunnel.local/x?y"), "socks5h://tunnel.local/x?y");
        assert_eq!(
            redact_userinfo("socks5h://alice:s3cr3t@127.0.0.1:1080"),
            "socks5h://<redacted>@127.0.0.1:1080"
        );
        // No scheme at all — the arm that cannot lean on `parse` having split anything.
        assert_eq!(redact_userinfo("alice:s3cr3t@127.0.0.1:1080"), "<redacted>@127.0.0.1:1080");
        // A '@' inside the password must not shift the cut (the LAST '@' separates userinfo).
        assert_eq!(
            redact_userinfo("socks5h://alice:p@ss@<host>:1080/x"),
            "socks5h://<redacted>@<host>:1080/x"
        );
        // A '@' in the PATH is not userinfo — the authority ends at the first '/'.
        assert_eq!(redact_userinfo("socks5h://h:1080/a@b"), "socks5h://h:1080/a@b");
    }

    #[test]
    fn debug_redacts_the_password_and_display_omits_credentials() {
        let p = WsProxy::parse("socks5h://alice:s3cr3t@127.0.0.1:1080").unwrap();
        let dbg = format!("{p:?}");
        assert!(!dbg.contains("s3cr3t"), "password leaked into Debug: {dbg}");
        assert!(!dbg.contains("alice"), "username leaked into Debug: {dbg}");
        assert!(dbg.contains("auth=set"));
        assert_eq!(p.to_string(), "socks5h://127.0.0.1:1080");
    }

    #[test]
    fn ws_target_defaults_ports_by_scheme() {
        assert_eq!(
            ws_target("wss://ws.example.com/ws/market").unwrap(),
            ("ws.example.com".into(), 443)
        );
        assert_eq!(ws_target("ws://ws.example.com/x").unwrap(), ("ws.example.com".into(), 80));
        assert_eq!(
            ws_target("wss://ws.example.com:9443/x").unwrap(),
            ("ws.example.com".into(), 9443)
        );
        assert!(ws_target("not a url").is_err());
    }

    /// A listening loopback socket, and a loopback port with certainly nothing on it (bound, its
    /// port read, then dropped) — the two ends every dial test below needs, with no network.
    ///
    /// A closed loopback port answers RST immediately on every OS, which is what makes
    /// "unreachable address" deterministic here rather than a timing gamble.
    fn live_and_dead() -> (std::net::TcpListener, SocketAddr, SocketAddr) {
        let live = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let live_addr = live.local_addr().unwrap();
        let dead_addr = {
            let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            l.local_addr().unwrap()
        };
        (live, live_addr, dead_addr)
    }

    /// **F1 — the resolver cannot outlive the bound.** The bounded arm used to call
    /// `to_socket_addrs()` in front of `TcpStream::connect_timeout`: a blocking `getaddrinfo` with
    /// no timeout in its API, so a dead resolver hung the "bounded" dial for as long as libc liked
    /// and every teardown budget derived from `connect_timeout` was derived from half the operation.
    ///
    /// The mechanism, not the DNS, is what is testable offline — so this drives
    /// [`call_with_deadline`] with a call that would block for a minute and asserts the wait ends at
    /// the deadline. Restore a `join()` (or any wait on the call itself) and this test hangs until
    /// the closure finishes, which is the defect reproduced.
    #[test]
    fn a_blocking_call_cannot_outlive_its_deadline() {
        let started = Instant::now();
        let out = call_with_deadline(Duration::from_millis(120), || {
            std::thread::sleep(Duration::from_secs(60));
            "the resolver eventually answered"
        });
        let elapsed = started.elapsed();
        assert!(out.is_none(), "the deadline must win, got {out:?}");
        assert!(
            elapsed < Duration::from_secs(5),
            "the bound did not bound: waited {elapsed:?} for a 120 ms deadline"
        );
    }

    /// …and the deadline does not COST anything when the call answers: a prompt call still returns
    /// its value. Without this, "return `None` immediately" would pass the test above.
    #[test]
    fn a_prompt_call_still_returns_its_value() {
        assert_eq!(call_with_deadline(Duration::from_secs(30), || 7 + 1), Some(8));
    }

    /// **F2 — every resolved address is tried, not just the first.** The bounded arm did
    /// `to_socket_addrs()?.next()`, while the `tungstenite::connect` arm beside it walks them all.
    /// MEASURED from the CI box on 2026-08-08, `stream.binance.com` resolves to 6 addresses and
    /// `fstream.binance.com` to 8 — so on the venue this bound was introduced for, one unreachable
    /// edge failed a connect the unbounded arm would have completed. That is an availability
    /// regression traded for boundedness, and it was neither necessary nor disclosed.
    ///
    /// Deterministic and offline: the first address is a closed loopback port (instant RST), the
    /// second a live listener. Reinstate first-only and the dial fails.
    #[test]
    fn every_resolved_address_is_tried_not_just_the_first() {
        let (_live, live_addr, dead_addr) = live_and_dead();
        let tcp = dial_bounded_with(
            "venue.example",
            443,
            Duration::from_secs(10),
            &|_: &str, _: u16, _: Duration| Ok(vec![dead_addr, live_addr]),
            &|addr, timeout| TcpStream::connect_timeout(addr, timeout),
        )
        .expect("the second resolved address is reachable, so the dial must succeed");
        assert_eq!(
            tcp.peer_addr().unwrap(),
            live_addr,
            "the dial must have fallen through to the reachable address"
        );
    }

    /// **F1 — resolution is DEBITED from the window, not added to it.** The bound is a ceiling on
    /// the whole TCP phase, which only holds if the time resolution spent is gone from what the
    /// connects may spend. Asserted on the timeout the connect step is actually HANDED, so it
    /// cannot be satisfied by luck.
    ///
    /// Hand the attempts the full `bound` again and the recorded value jumps back to it — a phase
    /// ceiling of `resolve + n × bound`, which is what the callers' budget arithmetic denies.
    #[test]
    fn resolution_time_is_spent_from_the_same_window_as_the_connects() {
        let bound = Duration::from_millis(600);
        let resolve_cost = Duration::from_millis(250);
        let handed = std::sync::Mutex::new(Vec::<Duration>::new());
        let (_live, _live_addr, dead_addr) = live_and_dead();

        let err = dial_bounded_with(
            "venue.example",
            443,
            bound,
            &|_: &str, _: u16, _: Duration| {
                std::thread::sleep(resolve_cost);
                Ok(vec![dead_addr])
            },
            &|addr, timeout| {
                handed.lock().unwrap().push(timeout);
                TcpStream::connect_timeout(addr, timeout)
            },
        )
        .expect_err("a closed port cannot connect");

        let handed = handed.lock().unwrap().clone();
        assert_eq!(handed.len(), 1, "one address, one attempt");
        assert!(
            handed[0] < bound - resolve_cost + Duration::from_millis(150),
            "the connect was handed {:?} of a {bound:?} window after resolution had already spent \
             {resolve_cost:?} — resolution is not being debited, so `bound` bounds no phase",
            handed[0]
        );
        assert!(err.contains("venue.example:443"), "unexpected: {err}");
    }

    /// **F1 + F2 together — the whole phase is bounded HOWEVER MANY addresses resolve.** Trying
    /// every address (F2) is only safe because they share one deadline (F1); per-attempt bounds
    /// would make the real ceiling `n × bound` for an `n` the venue's DNS picks, and binance's is 6
    /// and 8. Five black-holing addresses under a 300 ms window must still cost ~300 ms, not 1.5 s,
    /// and the error must say the WINDOW ran out rather than blaming the last address.
    #[test]
    fn the_whole_tcp_phase_is_bounded_however_many_addresses_resolve() {
        let bound = Duration::from_millis(300);
        let addrs: Vec<SocketAddr> =
            (1..=5u8).map(|i| SocketAddr::from(([192, 0, 2, i], 443))).collect();
        let attempts = std::sync::atomic::AtomicUsize::new(0);

        let started = Instant::now();
        let err = dial_bounded_with(
            "venue.example",
            443,
            bound,
            &|_: &str, _: u16, _: Duration| Ok(addrs.clone()),
            // A black hole: consume exactly the window handed over, then fail like a real timeout.
            &|_, timeout| {
                attempts.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                std::thread::sleep(timeout);
                Err(std::io::Error::new(std::io::ErrorKind::TimedOut, "connection timed out"))
            },
        )
        .expect_err("every address black-holes");
        let elapsed = started.elapsed();

        assert!(
            elapsed < bound * 2,
            "5 black-holed addresses under a {bound:?} phase bound took {elapsed:?} — the bound is \
             per-attempt, not per-phase"
        );
        assert!(
            err.contains("dial bound"),
            "an exhausted window must report itself, not the last address's error: {err}"
        );
        assert!(
            attempts.load(std::sync::atomic::Ordering::Relaxed) >= 1,
            "the loop must actually attempt something"
        );
    }

    /// A resolver that answers with NOTHING is its own outcome, not a silent success — the arm this
    /// replaced spelled it `no address for host:port` and callers' logs still say so.
    #[test]
    fn an_empty_resolution_is_reported_as_such() {
        let err = dial_bounded_with(
            "venue.example",
            443,
            Duration::from_secs(1),
            &|_: &str, _: u16, _: Duration| Ok(vec![]),
            &|addr, timeout| TcpStream::connect_timeout(addr, timeout),
        )
        .expect_err("no addresses, no dial");
        assert_eq!(err, "no address for venue.example:443");
    }

    /// INERTNESS: `connect_ws(url, None, _)` must never touch the proxy machinery — a malformed
    /// URL fails in the SAME pre-dial parse the pre-existing bounded arm used, with no socket
    /// opened and no proxy consulted.
    #[test]
    fn no_proxy_bounded_arm_fails_in_the_url_parse_exactly_as_before() {
        let err = connect_ws("not a url", None, Some(Duration::from_millis(50))).unwrap_err();
        assert!(err.starts_with("bad ws url"), "unexpected: {err}");
    }

    /// The `Some(proxy)` arm reaches the proxy path (and, without the feature, says so by name)
    /// — never a silent fall-through to a direct dial.
    #[test]
    fn proxy_arm_never_silently_falls_back_to_direct() {
        let p = WsProxy::parse("socks5h://127.0.0.1:1").unwrap();
        // Port 1 is reserved; nothing listens, so the SOCKS connect fails fast on every OS.
        let err = connect_ws("wss://example.invalid/ws", Some(&p), None).unwrap_err();
        #[cfg(feature = "socks-proxy")]
        assert!(err.contains("socks5 connect to example.invalid:443"), "unexpected: {err}");
        #[cfg(not(feature = "socks-proxy"))]
        assert!(err.contains("`socks-proxy` feature"), "unexpected: {err}");
        assert!(!err.contains("s3cr3t"));
    }

    /// End-to-end proof of the SOCKS5 wire exchange against a hand-rolled one-shot proxy: the
    /// greeting is answered no-auth, the CONNECT request is asserted to carry the **domain**
    /// (ATYP=3 — i.e. `socks5h` remote DNS, NOT a locally-resolved IP), and a success reply is
    /// returned. The subsequent TLS handshake then fails against the dummy server, which is the
    /// point: reaching a TLS error proves the tunnel was established.
    #[cfg(feature = "socks-proxy")]
    #[test]
    fn socks5h_sends_the_domain_to_the_proxy() {
        use std::io::{Read, Write};
        use std::net::TcpListener;
        use std::sync::mpsc;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let (tx, rx) = mpsc::channel::<(u8, String, u16)>();
        let server = std::thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            let mut greet = [0u8; 3];
            s.read_exact(&mut greet).unwrap();
            assert_eq!(greet[0], 5, "socks version");
            s.write_all(&[5, 0]).unwrap(); // version 5, no-auth selected
            let mut head = [0u8; 4];
            s.read_exact(&mut head).unwrap();
            assert_eq!(&head[..2], &[5, 1], "CONNECT command");
            let atyp = head[3];
            let mut len = [0u8; 1];
            s.read_exact(&mut len).unwrap();
            let mut dom = vec![0u8; len[0] as usize];
            s.read_exact(&mut dom).unwrap();
            let mut pbuf = [0u8; 2];
            s.read_exact(&mut pbuf).unwrap();
            let tport = u16::from_be_bytes(pbuf);
            // success, bound to 0.0.0.0:0
            s.write_all(&[5, 0, 0, 1, 0, 0, 0, 0, 0, 0]).unwrap();
            tx.send((atyp, String::from_utf8_lossy(&dom).to_string(), tport)).unwrap();
            // Let the client's TLS ClientHello land, then drop — the client sees a TLS failure.
            let mut sink = [0u8; 512];
            let _ = s.read(&mut sink);
        });

        let p = WsProxy::parse(&format!("socks5h://127.0.0.1:{port}")).unwrap();
        let err =
            connect_ws("wss://ws-subscriptions-clob.polymarket.com/ws/market", Some(&p), None)
                .unwrap_err();
        let (atyp, domain, tport) = rx.recv_timeout(Duration::from_secs(10)).unwrap();
        server.join().unwrap();

        assert_eq!(atyp, 3, "ATYP must be DOMAINNAME (3) — socks5h resolves at the proxy");
        assert_eq!(domain, "ws-subscriptions-clob.polymarket.com");
        assert_eq!(tport, 443);
        // The tunnel was established; only the TLS/WS handshake against the dummy failed.
        assert!(err.starts_with("ws handshake via socks5h://127.0.0.1:"), "unexpected: {err}");
    }
}
