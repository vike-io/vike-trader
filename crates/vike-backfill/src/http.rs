//! Shared blocking HTTP GET helpers for the collectors (finding F21). Each vendor module had
//! rebuilt the same ureq skeleton — build agent, optional auth/UA header, manual `(200..300)`
//! status check, `CollectError::Fetch` wrapping — in three subtly different shapes, plus a drift
//! bug: eod used a plain `ureq::agent()` (which errors on 4xx/5xx inside `call()`), so its manual
//! status check + body-head branch was dead code. This module gives all four a single
//! `http_status_as_error(false)` agent and three body sinks: [`get_to_string`], [`get_to_bytes`]
//! (404 → `Ok(None)`), and [`get_to_file`] (streamed, 404 → `Ok(None)` when asked).
//!
//! Auth genuinely differs per vendor (Basic / optional-Bearer / none), so the `Authorization`
//! header stays a parameter rather than being abstracted away. No global timeout is set on purpose
//! — the archive downloads run 130-400 MB and `vike_bridge_core::http::blocking_agent`'s 30s
//! `timeout_global` would abort them mid-stream, so that shared agent is deliberately NOT reused.
//!
//! # ⚠ What bounds these GETs, what deliberately does not, and why the knob names lie
//!
//! `crates/vike-backfill/src/bin/pmxt_backfill.rs` streams 130-400 MB archive parts an hour through
//! [`get_to_file`], so the only bound this module may carry is one that tells "slow but still
//! arriving" from "dead" — never one that tells "big" from "small". A whole-request or whole-body
//! ceiling cannot make the first distinction: large enough for the slowest healthy part on the
//! slowest healthy link it is far too large to notice a dead peer, and small enough to notice a dead
//! peer it aborts a download that was doing nothing wrong. Exactly two knobs are armed:
//!
//! - [`CONNECT_TIMEOUT`] — opening the socket. This is the bound a routable-but-black-holed host
//!   actually stalls on, where the OS SYN-retry default is minutes.
//! - [`BODY_IDLE_TIMEOUT`] — the per-READ idle bound on the body: how long a single `read` may wait
//!   for the NEXT byte. A body that dribbles for an hour is untouched by it; a peer that sent
//!   headers, streamed a few megabytes and then went silent without closing the socket fails after
//!   this long instead of parking the collector until somebody notices the hourly job never ended.
//!
//! **Which ureq knob is which is NOT what the builder names suggest, and the first version of this
//! section got it exactly backwards** — it armed `timeout_recv_response` (documented upstream as
//! "the response headers, but not the body") believing it stopped at the headers, and refused
//! `timeout_recv_body` believing it a cumulative whole-body ceiling. Both beliefs were false, and
//! the truth is read from the pinned `ureq` 3.4's `src/timings.rs` (the root `Cargo.toml` names the
//! pin), not from the field names:
//!
//! - `CallTimings::next_timeout(phase)` takes the MINIMUM over the phase's own configured timeout
//!   measured from NOW, plus — through `Timeout::preceeding` — every PRECEDING phase's configured
//!   timeout measured from the instant that phase ENDED, plus `global`/`per_call`. A phase's
//!   timeout therefore does not stop when the phase does: it becomes an absolute wall the next phase
//!   inherits.
//! - `Timeout::RecvBody`'s preceding set is `[RecvResponse]`. So `timeout_recv_response = X` makes
//!   every body read deadline `headers_time + X` — a whole-body ceiling wearing a headers-only
//!   name — and once that wall is past, `NextTimeout::not_zero` turns the zero remainder into a
//!   ONE-SECOND socket read timeout for the rest of the download. Measured (a loopback probe):
//!   a body still streaming at a 100 ms cadence died on a single 1.5 s gap with
//!   `timeout: receive response`. At a real value that is every archive part which streams longer
//!   than the bound, then dies on the first CDN hiccup or TCP retransmit backoff over a second.
//! - `timeout_recv_body`, by contrast, is the CURRENT phase during the body, and the current phase
//!   is always measured from `now` — recomputed before every `await_input` in `ureq`'s body loop
//!   (`run.rs`'s `BodyHandler::do_read`), with `time_of(RecvBody)` not even recorded until
//!   `ended()`. It is therefore precisely the per-read idle bound this module wants, and the
//!   behavioural tests below prove it on a real socket: a body that streams for LONGER than the
//!   bound completes as long as no single gap exceeds it, and one gap that does fails with
//!   `timeout: receive body`.
//! - `Timeout::Connect` appears in `SendRequest`'s preceding set and in no later one, so the
//!   connect bound leaks into exactly one phase — putting the (bodyless, kilobyte) GET on the wire
//!   within [`CONNECT_TIMEOUT`] of the socket opening, which is harmless — and can never reach the
//!   headers or the body.
//!
//! **Left UNSET on purpose, every one for the same reason:** `timeout_global` and `timeout_per_call`
//! (whole-call ceilings by definition), `timeout_recv_response` (the whole-body ceiling above),
//! and `timeout_send_request`. That last one is the tempting route to a time-to-first-byte bound —
//! `RecvResponse`'s preceding set is `[SendRequest, SendBody]` and `RecvBody`'s is not, so
//! `send_request = X` happens to bound the headers at `send_end + X` and then stop — but it rides
//! an undocumented property of `preceeding`, and a future `ureq` that revises that table would
//! silently move the bound onto the body, i.e. reproduce the regression this section exists to
//! refuse. Not worth it for a batch collector whose job is restartable and idempotent.
//!
//! Which leaves two honest residuals, stated rather than papered over. **Time-to-first-byte is
//! unbounded**: a vendor that accepts the request and never sends headers parks the collector until
//! the TCP stack itself gives up. **Name resolution is unbounded** (`timeout_resolve` unset, so
//! `ureq` calls `to_socket_addrs` inline and spawns no resolver thread): a hostname against a
//! black-holed resolver parks the caller before the connect bound is ever consulted. Both are
//! declared here, not fixed here.
//!
//! [`collector_agent`] is the ONE builder, and the tests below pin all of the above — the absences
//! as field assertions (an absence cannot be read from the code), the two armed bounds as
//! behaviour on a loopback server. ⚠ **The FIELD PIN is the gate against the regression above;
//! the behavioural test is its illustration, not its guard.** This paragraph first claimed the
//! opposite — that the streaming test excluded the `timeout_recv_response` wall "by BEHAVIOUR, not
//! by a field pin" — and a mutation refuted it: with a 500 ms `timeout_recv_response` planted in
//! [`build_agent`], a wall SIX times shorter than that test's ~3 s body, the streaming test still
//! PASSED and only `no_ceiling_reaches_the_headers_or_the_body` failed. The mechanism is
//! `NextTimeout::not_zero` above: past an inherited wall the zero remainder becomes a 1 s socket
//! read timeout, and a body dribbling at 50 ms outruns a 1 s timeout indefinitely, so the wall never
//! fires on a steadily-arriving body — it fires on the first gap over a second, which is exactly the
//! CDN-hiccup case the collector meets in production and a smooth loopback dribble never shows.
//! (The streaming test now plants one such gap late in the body, so it CAN see that class; it is
//! still the illustration, because a wall of any value the gap outruns passes it.) Do not drop the
//! field assertions as redundant with the behaviour: they are the only thing that fails the moment a
//! ceiling is armed, whatever its value.

use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::error::CollectError;

/// Max time to open the socket to a vendor host.
///
/// This is the bound a routable-but-black-holed host actually stalls on (a firewall that DROPs
/// rather than REJECTs, a vendor edge that is up with the origin gone), where the OS SYN-retry
/// default is minutes. Generous by connect standards because a collector reaches vendors across the
/// public internet from whichever box is running it, and a cold connect to a far region is
/// legitimately a couple of seconds. `ureq` shares this budget across a multi-address host
/// (`unversioned/transport/tcp.rs`'s `try_connect`: a geometric split weighted toward the first
/// candidate), so it is the bound on the whole WALK, not on one candidate.
///
/// ⚠ The TLS handshake is bounded only INDIRECTLY: `ureq`'s rustls connector (`tls/rustls.rs`)
/// builds the `StreamOwned` without driving it, so rustls completes the handshake on the first
/// write — under the `SendRequest` phase, which this value reaches only through the inheritance the
/// module doc describes, as an absolute wall [`CONNECT_TIMEOUT`] after the socket opened. So the
/// handshake and the request write together must finish inside it, which a live peer does in well
/// under a second.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

/// Max time one body `read` may wait for the NEXT byte — the per-read idle bound, never a ceiling
/// on the body (the module doc carries the `ureq` mechanism and the measurement).
///
/// Sized as the "not slow, dead" line for a streaming download over the public internet, by the
/// argument `vike_datahub::server`'s `IDLE_READ_TIMEOUT` makes for its own 300 s: a value that
/// would clip a slow but live peer is worse than a stalled collector. Linux caps a single TCP
/// retransmit interval at 120 s (`TCP_RTO_MAX`), so a gap this long means the far end produced
/// no byte across more than two of the kernel's LONGEST retransmit cycles — dead by any standard a
/// batch collector needs, and an idempotent, restartable job re-fetches the part next hour.
/// A CDN hiccup or a moderate retransmit backoff is seconds, two orders of magnitude inside it.
const BODY_IDLE_TIMEOUT: Duration = Duration::from_secs(300);

// Compile-time bounds on the two, the `vike_datahub::server` idiom: a RANGE, so a deliberate tweak
// stays free while "the bound was effectively removed" and "the bound refuses a healthy vendor" both
// fail to compile instead of shipping.
const _: () = assert!(
    CONNECT_TIMEOUT.as_secs() > 0 && CONNECT_TIMEOUT.as_secs() <= 120,
    "CONNECT_TIMEOUT must stay a POSITIVE, short bound — it exists to replace the OS SYN-retry \
     default, which a 0 (refuse everything) and a multi-minute value both fail to improve on"
);
const _: () = assert!(
    BODY_IDLE_TIMEOUT.as_secs() >= 60 && BODY_IDLE_TIMEOUT.as_secs() <= 1800,
    "BODY_IDLE_TIMEOUT must stay a PER-READ bound sized for a dead peer, not a hiccup — under a \
     minute clips a healthy stream on one TCP retransmit backoff, and over half an hour is the \
     stalled-collector failure it exists to end"
);

/// Per-request options common to the collector GETs.
#[derive(Clone, Copy, Default)]
pub struct GetOptions<'a> {
    /// Full `Authorization` header value, e.g. `"Basic <b64>"` or `"Bearer <key>"`. `None` = no auth.
    pub authorization: Option<&'a str>,
    /// `User-Agent` header value. Some sources 403 without one (Yahoo); pmxt sends an identifying UA.
    pub user_agent: Option<&'a str>,
    /// Extra request headers, applied verbatim after the two named above.
    ///
    /// The escape hatch for a vendor whose auth is neither Basic nor Bearer, and it exists for
    /// exactly one: `data.vike.io` authenticates with an `X-API-KEY` header, which has no
    /// `Authorization` spelling at all (`crates/vike-backfill/src/vikedata/client.rs`'s
    /// `API_KEY_HEADER`). Empty for every other caller — the module doc's point stands, that auth
    /// genuinely differs per vendor and stays a parameter rather than being abstracted away.
    ///
    /// ⚠ A header VALUE here can be a credential, so nothing in this module logs `opts`: the error
    /// messages below carry the URL and the status, never the request headers.
    pub headers: &'a [(&'a str, &'a str)],
}

/// The collectors' shared ureq agent: 4xx/5xx returned as responses rather than errors, and the
/// two bounds of the module doc — a connect timeout and a per-read body idle timeout — with NO
/// whole-request or whole-body ceiling of any kind.
///
/// Split out of [`send`] as a named seam so the rule can be a TEST rather than a comment: the
/// module doc's refusal of `timeout_global` (and of `timeout_recv_response`, which is the same
/// ceiling in disguise) is the kind of absence a later reader reads as an omission, and the tests
/// below fail the moment one appears.
fn collector_agent() -> ureq::Agent {
    build_agent(CONNECT_TIMEOUT, BODY_IDLE_TIMEOUT)
}

/// [`collector_agent`] with the two bounds as parameters — the ONE place the builder is spelled,
/// so the behavioural tests can scale the bounds down to sub-second values and prove their
/// semantics on a loopback server without waiting the production five minutes. Nothing else may
/// call this with anything but the module constants.
fn build_agent(connect: Duration, body_idle: Duration) -> ureq::Agent {
    ureq::Agent::config_builder()
        // callers read the status themselves (a 404 is often expected, not a transport error)
        .http_status_as_error(false)
        .timeout_connect(Some(connect))
        .timeout_recv_body(Some(body_idle))
        .build()
        .new_agent()
}

/// Build the request, apply the optional headers, and send it. `ctx` is a short vendor prefix used
/// in error messages (e.g. `"tardis"`).
fn send(
    url: &str,
    opts: &GetOptions,
    ctx: &str,
) -> Result<ureq::http::Response<ureq::Body>, CollectError> {
    let agent = collector_agent();
    let mut req = agent.get(url);
    if let Some(auth) = opts.authorization {
        req = req.header("Authorization", auth);
    }
    if let Some(ua) = opts.user_agent {
        req = req.header("User-Agent", ua);
    }
    for (name, value) in opts.headers {
        req = req.header(*name, *value);
    }
    req.call().map_err(|e| CollectError::Fetch(format!("{ctx} GET {url}: {e}")))
}

/// First 200 chars of a body — the error-message head shared by the status-failure branches.
fn head(s: &str) -> String {
    s.chars().take(200).collect()
}

/// Blocking GET → the response body as a `String`. Non-2xx → `CollectError::Fetch` with the body
/// head (the body is read first so it can be surfaced in the error).
pub fn get_to_string(url: &str, opts: &GetOptions, ctx: &str) -> Result<String, CollectError> {
    let mut resp = send(url, opts, ctx)?;
    let status = resp.status().as_u16();
    let body = resp
        .body_mut()
        .read_to_string()
        .map_err(|e| CollectError::Fetch(format!("read {ctx} {url}: {e}")))?;
    if !(200..300).contains(&status) {
        return Err(CollectError::Fetch(format!(
            "{ctx} HTTP {status} from {url}: {}",
            head(&body)
        )));
    }
    Ok(body)
}

/// Blocking GET → the response body as raw bytes, or `Ok(None)` on HTTP 404 (the "not published"
/// skip). Other non-2xx → `Err`. For callers that post-process the bytes (e.g. gzip-decompress).
pub fn get_to_bytes(
    url: &str,
    opts: &GetOptions,
    ctx: &str,
) -> Result<Option<Vec<u8>>, CollectError> {
    let mut resp = send(url, opts, ctx)?;
    let status = resp.status().as_u16();
    if status == 404 {
        return Ok(None);
    }
    if !(200..300).contains(&status) {
        return Err(CollectError::Fetch(format!("{ctx} HTTP {status} from {url}")));
    }
    let mut buf = Vec::new();
    resp.body_mut()
        .as_reader()
        .read_to_end(&mut buf)
        .map_err(|e| CollectError::Fetch(format!("read {ctx} body {url}: {e}")))?;
    Ok(Some(buf))
}

/// Blocking GET streamed straight to a fresh file at `dest` (never buffered — archive parts run
/// 130-400 MB). `Ok(Some(dest))` on success; `Ok(None)` on HTTP 404 when `treat_404_as_none`
/// (otherwise a 404, like any non-2xx, is an `Err` carrying the body head). Creates `dest`'s
/// parent directory.
pub fn get_to_file(
    url: &str,
    dest: &Path,
    opts: &GetOptions,
    ctx: &str,
    treat_404_as_none: bool,
) -> Result<Option<PathBuf>, CollectError> {
    let mut resp = send(url, opts, ctx)?;
    let status = resp.status().as_u16();
    if treat_404_as_none && status == 404 {
        return Ok(None);
    }
    if !(200..300).contains(&status) {
        let body = resp.body_mut().read_to_string().unwrap_or_default();
        return Err(CollectError::Fetch(format!(
            "{ctx} HTTP {status} from {url}: {}",
            head(&body)
        )));
    }
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| CollectError::Fetch(format!("create {}: {e}", parent.display())))?;
    }
    let mut out = File::create(dest)
        .map_err(|e| CollectError::Fetch(format!("create {}: {e}", dest.display())))?;
    let mut reader = resp.body_mut().as_reader();
    std::io::copy(&mut reader, &mut out).map_err(|e| {
        CollectError::Fetch(format!("stream {ctx} body to {}: {e}", dest.display()))
    })?;
    out.flush().map_err(|e| CollectError::Fetch(format!("flush {}: {e}", dest.display())))?;
    Ok(Some(dest.to_path_buf()))
}

#[cfg(test)]
mod tests {
    use std::net::TcpListener;
    use std::thread::{self, JoinHandle};
    use std::time::Instant;

    use super::*;

    /// A one-shot loopback HTTP/1.1 server that answers the first request with a `200` carrying a
    /// `Content-Length` of `bytes`, then DRIBBLES the body one byte at a time with `gap` between
    /// bytes — and, when `stall` is given, one gap of that length before byte `stall.0` instead.
    ///
    /// The request head is drained byte-wise and never parsed: the test is about the BODY clock,
    /// and `ureq` puts the whole GET on the wire in one write. A write failure ends the thread
    /// quietly — in the stall test the client has already given up on the socket, which is the
    /// point. Returns the URL to fetch and the server thread's handle.
    fn dribbling_server(
        bytes: usize,
        gap: Duration,
        stall: Option<(usize, Duration)>,
    ) -> (String, JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let url = format!("http://{}/part", listener.local_addr().expect("local_addr"));
        let handle = thread::spawn(move || {
            let (mut sock, _) = listener.accept().expect("accept");
            // Bounded so a test that never sends a request cannot park this thread forever.
            sock.set_read_timeout(Some(Duration::from_secs(10))).expect("read timeout");
            let mut head = Vec::new();
            let mut byte = [0u8; 1];
            while !head.ends_with(b"\r\n\r\n") {
                match sock.read(&mut byte) {
                    Ok(1) => head.push(byte[0]),
                    _ => return,
                }
            }
            let headers =
                format!("HTTP/1.1 200 OK\r\nContent-Length: {bytes}\r\nConnection: close\r\n\r\n");
            if sock.write_all(headers.as_bytes()).is_err() {
                return;
            }
            for i in 0..bytes {
                let pause = match stall {
                    Some((at, long)) if at == i => long,
                    _ => gap,
                };
                thread::sleep(pause);
                if sock.write_all(b"x").is_err() {
                    return;
                }
            }
        });
        (url, handle)
    }

    /// The ILLUSTRATION of the idle semantics, as behaviour: a body that streams for longer than
    /// BOTH armed bounds completes, because neither is a ceiling on the body — [`BODY_IDLE_TIMEOUT`]
    /// is per read and [`CONNECT_TIMEOUT`] never reaches the body phase (module doc). Scaled down
    /// through [`build_agent`]: a 300 ms connect bound and a 3 s idle bound against a body that
    /// takes ~5 s to arrive — a 50 ms cadence with ONE 2 s gap late in the body.
    ///
    /// ⚠ This test is NOT the gate against the `timeout_recv_response` regression the module doc
    /// describes; [`no_ceiling_reaches_the_headers_or_the_body`]'s field pin is. Measured by
    /// mutation: with a 500 ms `timeout_recv_response` planted in [`build_agent`] the first version
    /// of this test — a smooth 50 ms dribble — still PASSED, because past an inherited wall `ureq`
    /// degrades the remainder to a 1 s socket read timeout (`NextTimeout::not_zero`) and a steady
    /// dribble never leaves a 1 s hole for it to fire in. The 2 s gap is what lets this test SEE
    /// that class at all (a wall past by then trips on it; the 3 s idle bound does not), but it
    /// catches a wall only when the gap outruns the degraded timeout — a field pin catches a wall
    /// of ANY value. The elapsed-time assertion is what makes the pass MEAN something — a server
    /// that finished inside the bounds would pass the read for the wrong reason.
    #[test]
    fn a_body_streaming_longer_than_both_bounds_completes_when_no_gap_exceeds_the_idle_bound() {
        let connect = Duration::from_millis(300);
        let idle = Duration::from_secs(3);
        let gap = Duration::from_millis(50);
        let bytes = 60;
        // Over `ureq`'s 1 s degraded read timeout (so an inherited wall would trip on it), a full
        // second under `idle` (so a scheduler stall on a loaded runner does not fire the bound
        // early), and late enough that a short wall is already past when it lands.
        let late_gap = Duration::from_secs(2);
        let (url, server) = dribbling_server(bytes, gap, Some((bytes - 10, late_gap)));

        let started = Instant::now();
        let mut resp = build_agent(connect, idle).get(&url).call().expect("headers arrive");
        let mut body = Vec::new();
        resp.body_mut()
            .as_reader()
            .read_to_end(&mut body)
            .expect("a slow but PROGRESSING body must complete — neither bound is a body ceiling");
        let elapsed = started.elapsed();

        assert_eq!(body.len(), bytes, "the whole body arrived");
        assert!(
            elapsed > idle && elapsed > connect,
            "the body must have streamed for LONGER than both bounds for this pass to prove \
             anything: took {elapsed:?} against idle {idle:?} / connect {connect:?}"
        );
        server.join().expect("server thread");
    }

    /// ...and the other half of the idle semantics: ONE gap past [`BODY_IDLE_TIMEOUT`] fails the
    /// read with `ureq`'s `timeout: receive body` — promptly, from the bound, not from the server
    /// eventually resuming — rather than parking the collector on a peer that went silent with the
    /// socket open. The bytes delivered before the stall are already in the caller's buffer, which
    /// is what lets [`get_to_file`] leave a legibly truncated file rather than nothing.
    #[test]
    fn one_gap_past_the_idle_bound_fails_with_receive_body_rather_than_hanging() {
        // A full second, not the few hundred milliseconds the assertions strictly need: the server
        // dribbles at 10 ms, and a scheduler stall longer than `idle` between two dribbled bytes on
        // a loaded runner would fire the bound EARLY and fail `body.len() == before_stall`. Every
        // assertion below holds unchanged at this value (`elapsed < stall` has 4 s of room).
        let idle = Duration::from_secs(1);
        let before_stall = 10;
        let stall = Duration::from_secs(5);
        // The server thread is deliberately not joined: it sleeps out the stall, finds the socket
        // gone and returns on its own, and nextest runs each test in its own process anyway.
        let (url, _server) =
            dribbling_server(20, Duration::from_millis(10), Some((before_stall, stall)));

        let mut resp =
            build_agent(Duration::from_secs(5), idle).get(&url).call().expect("headers arrive");
        let mut body = Vec::new();
        let started = Instant::now();
        let err = resp
            .body_mut()
            .as_reader()
            .read_to_end(&mut body)
            .expect_err("a body that stalls past the idle bound must FAIL, not hang");
        let elapsed = started.elapsed();

        assert!(err.to_string().contains("timeout: receive body"), "the idle bound fired: {err}");
        assert!(
            elapsed < stall,
            "failed by the bound ({idle:?}), not by the server resuming after {stall:?}: {elapsed:?}"
        );
        assert_eq!(body.len(), before_stall, "the bytes before the stall were delivered");
    }

    /// The absences, as field pins — an absence cannot be read from the code, so each ceiling that
    /// would reach the headers or the body is asserted unarmed here and fails the moment somebody
    /// "fixes" the missing timeout. The module doc carries the argument per knob; the messages
    /// carry the one-line version.
    ///
    /// `await_100` and `send_body` are deliberately NOT asserted: `ureq` defaults `await_100` to
    /// one second, both belong to request-BODY phases these bodyless GETs never enter, and neither
    /// sits in `RecvResponse`'s or `RecvBody`'s preceding set, so neither can reach a response.
    #[test]
    fn no_ceiling_reaches_the_headers_or_the_body() {
        let timeouts = collector_agent().config().timeouts();
        assert_eq!(timeouts.global, None, "a global ceiling would abort a large healthy download");
        assert_eq!(timeouts.per_call, None, "per_call is a whole-call ceiling by another name");
        assert_eq!(
            timeouts.recv_response, None,
            "recv_response is inherited by EVERY body read as an absolute wall headers_time + X \
             (module doc) — a whole-body ceiling wearing a headers-only name"
        );
        assert_eq!(
            timeouts.send_request, None,
            "send_request would bound time-to-first-byte only through an undocumented `preceeding` \
             quirk that a future ureq could move onto the body — declared unarmed, see module doc"
        );
        assert_eq!(timeouts.resolve, None, "name resolution is a DECLARED residual, not a bound");
    }

    /// ...and the two that ARE armed, by value: the production constants reach the builder
    /// unchanged. The behavioural tests above prove what the two knobs MEAN; this proves which
    /// numbers the collectors actually run with.
    #[test]
    fn the_connect_and_the_body_idle_bounds_are_armed() {
        let timeouts = collector_agent().config().timeouts();
        assert_eq!(timeouts.connect, Some(CONNECT_TIMEOUT));
        assert_eq!(timeouts.recv_body, Some(BODY_IDLE_TIMEOUT));
    }

    /// The status policy the whole module depends on: 4xx/5xx must come back as RESPONSES, because
    /// every body sink here reads the status itself (a 404 is a "not published" skip, not an error).
    /// Bundled with the timeout pins because they share one builder — a rewrite that arms a timeout
    /// while dropping this would turn every 404 skip into a hard `CollectError::Fetch`.
    #[test]
    fn statuses_are_returned_as_responses_not_errors() {
        assert!(!collector_agent().config().http_status_as_error());
    }
}
