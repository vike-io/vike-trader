//! **The bounded TCP dial**, featureless: name resolution and every address it names under ONE wall-clock deadline.
//! The WebSocket dial (`crate::websocket_proxy`'s `connect_websocket`) runs its TCP phase here; a plain TCP or TLS
//! probe outside it takes the same dial instead of a `to_socket_addrs()` (no bound at all) followed by one
//! `connect_timeout` per address (`n` x the bound, for an `n` the host's DNS picks).
//!
//! - **Resolution is bounded.** `to_socket_addrs()` is a blocking `getaddrinfo` with no timeout argument, and std
//!   offers no bounded resolver, so [`resolve_with_deadline`] runs it on a throwaway thread and abandons it at the
//!   deadline: the only shape available without adding a resolver crate (which `deny.toml`'s ONE-transport-stack ban
//!   exists to prevent).
//! - **EVERY resolved address is tried**, as `tungstenite::connect` does. A venue edge name resolves to several
//!   addresses (`stream.binance.com` to 6, `fstream.binance.com` to 8), so first-only would turn one unreachable edge
//!   into a failed connect.
//! - **Each address gets an equal share of what is LEFT** ([`dial_bounded`]), so one black-holed address cannot eat
//!   the window the next one needed.
//! - **The errors are `io::Error`s with the kind kept**: a spent window is `TimedOut`, every address failing is the
//!   last attempt's kind, so a caller tells "no answer yet" from "refused" without reading the words.

use std::io;
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::time::{Duration, Instant};

/// Split `host:port` / `host` / `[v6]:port` / `[v6]`, defaulting the port. A proxy URL's authority and a `ws://`
/// URL's have the identical shape.
pub fn split_host_port(authority: &str, default_port: u16) -> Result<(String, u16), String> {
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

/// Run one blocking call on a throwaway thread and stop waiting for it after `bound`.
///
/// `None` means the deadline won, and the thread is deliberately **abandoned, not joined** (joining
/// is the blocking wait being escaped): it runs to completion in the background, sends into a gone
/// receiver, and exits. A timeout therefore costs one leaked thread for as long as the blocked call
/// takes — for `getaddrinfo`, the C library's own retry policy (glibc defaults `timeout:5
/// attempts:2` per nameserver). Acceptable HERE and not everywhere: a dial happens at most once per
/// reconnect backoff (>=500 ms on the fastest roster row), so leaked threads cannot accumulate
/// faster than they retire.
fn call_with_deadline<T: Send + 'static>(
    bound: Duration,
    f: impl FnOnce() -> T + Send + 'static,
) -> Option<T> {
    // one-shot: at most 1 message (the closure's result).
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .name("ws-deadline".into())
        .spawn(move || {
            // Receiver gone = the caller timed out and moved on; the late result is discarded.
            let _ = tx.send(f());
        })
        .expect("spawn ws deadline thread");
    rx.recv_timeout(bound).ok()
}

/// `host:port` resolved to every address it names, under a wall-clock `bound`.
///
/// A healthy lookup costs about 0–10 ms, so the bound charges the healthy path nothing worth
/// counting; it exists for the unhealthy one, which has no ceiling this code can see.
/// A lookup that fails keeps its `io::ErrorKind`; one that does not answer within `bound` is `TimedOut`.
pub fn resolve_with_deadline(
    host: &str,
    port: u16,
    bound: Duration,
) -> io::Result<Vec<SocketAddr>> {
    let owned = host.to_string();
    match call_with_deadline(bound, move || {
        (owned.as_str(), port).to_socket_addrs().map(|it| it.collect::<Vec<_>>())
    }) {
        Some(Ok(addrs)) => Ok(addrs),
        Some(Err(e)) => Err(io::Error::new(e.kind(), format!("resolve {host}:{port}: {e}"))),
        None => Err(io::Error::new(
            io::ErrorKind::TimedOut,
            format!("resolve {host}:{port}: no answer within {bound:?}"),
        )),
    }
}

/// Resolve `host:port`, then try EVERY address it named, in order, with resolution and all attempts spending ONE
/// shared `bound`; each attempt at most `per_address_cap` when one is given.
///
/// **One deadline, not one per step.** A per-step bound would make the real ceiling
/// `resolve + n × bound`, where `n` is a number the venue's DNS chooses — so the constant a caller
/// budgets against (`vike_bridge_core::websocket_session::spec`'s `CONNECT_10S`, spent by
/// `crates/vike-datahub/src/recorder.rs`'s `FEED_STOP_BUDGET_SECS`) would not be a ceiling
/// on anything.
///
/// **The cost of sharing, and the rule that pays it:** each address in turn is handed an EQUAL SHARE OF WHAT IS
/// LEFT of the deadline (`left / addresses not yet tried`, never more than `per_address_cap`), so the last address
/// gets all that remains and an address that fails fast (RST, ICMP unreachable) passes its unused time on to the
/// rest. A black-holed first address therefore costs at most its share, never the whole window, and the working
/// address behind it still gets a share of its own (a dual-stack host whose first answer this box cannot route;
/// the CI box's 2026-11-09 `datafeed.dukascopy.com` false alarm). `tungstenite::connect` walks the
/// addresses with NO bound, so there a black-holed first address costs the OS's full SYN ladder (~127 s on Linux
/// defaults).
///
/// A window spent before every address was tried is `TimedOut`; every address failing is the last attempt's kind.
/// A zero `bound` or a zero cap dials nothing.
pub fn dial_bounded(
    host: &str,
    port: u16,
    bound: Duration,
    per_address_cap: Option<Duration>,
) -> io::Result<TcpStream> {
    dial_bounded_with(
        host,
        port,
        bound,
        per_address_cap,
        &resolve_with_deadline,
        &|addr, timeout| TcpStream::connect_timeout(addr, timeout),
    )
}

/// What ONE connect attempt may take when `left` of the window is all that remains for `remaining` addresses (this
/// one included): an equal share, never more than `cap`. Zero addresses left reads as one.
fn attempt_share(left: Duration, remaining: usize, cap: Option<Duration>) -> Duration {
    let share = left / u32::try_from(remaining.max(1)).unwrap_or(u32::MAX);
    cap.map_or(share, |cap| share.min(cap))
}

/// The name-resolution step of [`dial_bounded_with`]: `(host, port, bound) -> addresses`.
/// Production is [`resolve_with_deadline`]; a test hands a canned list, or a slow one.
type ResolveStep<'a> = dyn Fn(&str, u16, Duration) -> io::Result<Vec<SocketAddr>> + 'a;

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
    per_address_cap: Option<Duration>,
    resolve: &ResolveStep<'_>,
    connect: &ConnectStep<'_>,
) -> io::Result<TcpStream> {
    let deadline = Instant::now() + bound;
    let addrs = resolve(host, port, bound)?;
    if addrs.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("no address for {host}:{port}"),
        ));
    }
    let mut last = "none attempted".to_string();
    let mut last_kind = io::ErrorKind::Other;
    for (i, addr) in addrs.iter().enumerate() {
        // Whatever resolution and the earlier attempts already spent is GONE from the window — this
        // is what makes `bound` a ceiling on the phase rather than on its last step.
        let left = deadline.saturating_duration_since(Instant::now());
        let share = attempt_share(left, addrs.len() - i, per_address_cap);
        // `TcpStream::connect_timeout` rejects a zero Duration, and an exhausted window is a real
        // outcome that must be reported as itself: "the bound ran out", not "connection refused".
        if share.is_zero() {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                format!(
                    "connect {host}:{port}: dial bound {bound:?} spent after {i} of {} address(es); \
                     last error: {last}",
                    addrs.len()
                ),
            ));
        }
        match connect(addr, share) {
            Ok(tcp) => return Ok(tcp),
            Err(e) => {
                last = format!("{addr}: {e}");
                last_kind = e.kind();
            }
        }
    }
    Err(io::Error::new(
        last_kind,
        format!(
            "connect {host}:{port}: all {} resolved address(es) failed; last error: {last}",
            addrs.len()
        ),
    ))
}

#[cfg(test)]
#[path = "dial_tests.rs"]
mod tests;
