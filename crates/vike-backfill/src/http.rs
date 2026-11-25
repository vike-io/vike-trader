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
//! — the archive downloads run 130-400 MB and `vike_transport::http::blocking_agent`'s 30s
//! `timeout_global` would abort them mid-stream, so that shared agent is deliberately NOT reused.
//!
//! # ⚠ What bounds these GETs, what deliberately does not, and why the knob names lie
//!
//! ⚠ **THE EXAMPLE THIS SECTION WAS WRITTEN AROUND IS DEAD, AND THE RULE IS NOT.** It read
//! *"the `pmxt_backfill` bin streams 130-400 MB archive parts an hour"*,
//! and that source stopped publishing on 2026-08-10 — `crates/vike-backfill/src/pmxt/mod.rs`'s
//! module doc carries the measurement and is the authority for it. The live streaming consumer is
//! **`crates/vike-backfill/src/databento/client.rs`**'s `fetch_to_file`, whose ranges are large for
//! the same reason and which has always used the same agent. Read the rule below as its, not
//! pmxt's; nothing about the design changes, only which caller makes the case for it.
//!
//! A collector that streams hundreds of megabytes through
//! [`get_to_file`] needs the only bound this module may carry to be one that tells "slow but still
//! arriving" from "dead" — never one that tells "big" from "small". A whole-request or whole-body
//! ceiling cannot make the first distinction: large enough for the slowest healthy part on the
//! slowest healthy link it is far too large to notice a dead peer, and small enough to notice a dead
//! peer it aborts a download that was doing nothing wrong. Exactly two knobs are armed:
//!
//! - [`CONNECT_TIMEOUT`] — opening the connection: the TCP connect and the TLS handshake, one
//!   budget (`timeout_connect` covers the handshake since ureq 3.4.1). This is the bound a
//!   routable-but-black-holed host actually stalls on, where the OS SYN-retry default is minutes.
//! - [`BODY_IDLE_TIMEOUT`] — ureq's `timeout_per_read`: how long a single transport `read` may wait
//!   for the NEXT byte. A body that dribbles for an hour is untouched by it; a peer that sent
//!   headers, streamed a few megabytes and then went silent without closing the socket fails after
//!   this long instead of parking the collector until somebody notices the hourly job never ended.
//!   It bounds every read, the response head's included (ureq's `run.rs` `recv_response` waits under
//!   `next_timeout(RecvResponse)`, which carries the per-read limit to the socket read), so a vendor
//!   that accepts the request and never answers fails after this long too
//!   (`a_vendor_that_never_answers_fails_per_read_rather_than_hanging`).
//!
//! **Which ureq knob is which is NOT what the builder names suggest, and two earlier versions of
//! this section got it wrong** (read the knobs from ureq's own `src/timings.rs`, not from their
//! names). In ureq 3.5 every phase knob is a TOTAL budget: `CallTimings::next_timeout` measures a
//! phase's deadline from the instant the phase before it ENDED, never from `now`. So
//! `timeout_recv_body`, which reads like an idle bound, is a ceiling on the whole body counted from
//! the response head, exactly what this module refuses. Under ureq 3.4.0 that deadline was
//! re-derived from `now` on every read, which is why this module armed `recv_body` until
//! algesten/ureq#1194 (3.4.1) fixed the restart and made it the ceiling: MEASURED, one test, same
//! lane, 3.4.0 passed at ~5.0 s and 3.4.1 failed at 3.01 s, three for three (the root `Cargo.toml`'s
//! `ureq` note). The per-read meaning is `timeout_per_read` (ureq 3.5.0), which restarts on every
//! transport read and applies beside the phase budgets, never instead of them. The other direction
//! changed too: before 3.4.1 a phase's budget lived on as a wall the next phase inherited (an armed
//! `timeout_recv_response` capped every body read at `headers_time + X`); in 3.5 a phase budget
//! stops when its phase ends.
//!
//! **Left UNSET on purpose:** `timeout_global` and `timeout_per_call` (whole-call ceilings by
//! definition) and `timeout_recv_body` (the whole-body ceiling above). The head-phase budgets
//! (`timeout_send_request`, `timeout_recv_response`) can no longer reach the body, and they are not
//! needed: the per-read bound already ends a silent head. **Name resolution is the one declared
//! residual** (`timeout_resolve` unset, so `ureq` calls `to_socket_addrs` inline and spawns no
//! resolver thread, and `timeout_per_read` covers no lookup): a hostname against a black-holed
//! resolver parks the caller before the connect bound is ever consulted. Declared here, not fixed
//! here.
//!
//! [`collector_agent`] is the ONE builder, and the tests below pin all of the above — the absences
//! as field assertions (an absence cannot be read from the code), the two armed bounds as
//! behaviour on a loopback server. ⚠ **The FIELD PIN is the gate against a ceiling; the behavioural
//! test is its illustration, not its guard**: a ceiling longer than the test's ~5 s body (ten
//! minutes, say, which still truncates a large part) passes any loopback test that does not wait it
//! out, and only `no_ceiling_reaches_the_headers_or_the_body` fails the moment one is armed,
//! whatever its value. Do not drop the field assertions as redundant with the behaviour.

use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::error::CollectError;

/// Max time to open the connection to a vendor host: the TCP connect and the TLS handshake, which
/// `timeout_connect` covers as ONE budget since ureq 3.4.1 (algesten/ureq#1193).
///
/// This is the bound a routable-but-black-holed host actually stalls on (a firewall that DROPs
/// rather than REJECTs, a vendor edge that is up with the origin gone), where the OS SYN-retry
/// default is minutes. Generous by connect standards because a collector reaches vendors across the
/// public internet from whichever box is running it, and a cold connect to a far region is
/// legitimately a couple of seconds. `ureq` shares this budget across a multi-address host
/// (`unversioned/transport/tcp.rs`'s `try_connect`: a geometric split weighted toward the first
/// candidate), so it is the bound on the whole WALK, not on one candidate; the handshake spends what
/// the walk left, which a live peer needs well under a second of.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

/// Max time one transport `read` (the response head's or the body's) may wait for the NEXT byte —
/// ureq's `timeout_per_read`, never a ceiling on the body (the module doc carries the `ureq`
/// mechanism and the measurement).
///
/// Sized as the "not slow, dead" line for a streaming download over the public internet, by the
/// argument `vike_datahub::server`'s `SERVER_IDLE_READ_TIMEOUT` makes for its own 300 s: a value that
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
    /// Extra request headers, applied verbatim after the one named above. No GET names its own
    /// `User-Agent`: every one sends the agent's (`vike_transport::http::USER_AGENT`).
    ///
    /// The escape hatch for a vendor whose auth is neither Basic nor Bearer, and it exists for
    /// exactly one: `data.vike.io` authenticates with an `X-API-KEY` header, which has no
    /// `Authorization` spelling at all (`crates/vike-backfill/src/vikedata/client.rs`'s
    /// `API_KEY_HEADER`). Empty for every other caller — the module doc's point stands, that auth
    /// genuinely differs per vendor and stays a parameter rather than being abstracted away.
    ///
    /// ⚠ A header VALUE here can be a credential, so nothing in this module logs `opts`: the error
    /// messages below carry the URL and the status, never the request headers. And a request carrying
    /// any of these follows NO redirect (`vike_transport::http::request_no_redirect`): ureq would replay
    /// the header to whatever host the `Location` names, so a 3xx is the failure `"{ctx} HTTP 30x from {url}"`.
    pub headers: &'a [(&'a str, &'a str)],
}

/// The collectors' shared ureq agent: 4xx/5xx returned as responses rather than errors, and the
/// two bounds of the module doc — a connect timeout and a per-read body idle timeout — with NO
/// whole-request or whole-body ceiling of any kind.
///
/// Split out of [`send`] as a named seam so the rule can be a TEST rather than a comment: the
/// module doc's refusal of `timeout_global` (and of `timeout_recv_body`, which is the same ceiling
/// under an idle-sounding name) is the kind of absence a later reader reads as an omission, and the
/// tests below fail the moment one appears.
pub(crate) fn collector_agent() -> ureq::Agent {
    build_agent(CONNECT_TIMEOUT, BODY_IDLE_TIMEOUT)
}

/// [`collector_agent`] with the two bounds as parameters — the ONE place they reach the network layer's keyless-GET
/// builder (`vike_transport::http::public_get_agent_bounded`, decision 0299: the connect bound, `CallBound::Idle`, the
/// status as a response), so the behavioural tests can scale the bounds down to sub-second values and prove their
/// semantics on a loopback server without waiting the production five minutes. Nothing else may
/// call this with anything but the module constants.
///
/// That builder ends in the public-GET finisher (`vike_transport::http::agent_public_get`): the DNS resolver,
/// `vike_transport::http::USER_AGENT`, and at most `vike_transport::http::PUBLIC_GET_MAX_REDIRECTS` (five) redirects
/// followed before it fails
/// (`too many redirects`), stated because ureq's own default (ten) was an accident of the library and not a decision.
/// The vendors legitimately redirect (a CDN, a presigned download URL) and none chains more than a hop or two;
/// `http_tests.rs`'s `the_redirect_limit_is_stated_and_a_longer_chain_fails` pins it.
///
/// ⚠ **What a followed redirect carries.** ureq removes `Authorization` and `Cookie` from the request
/// it sends to the `Location` and NOTHING else, so a vendor whose credential is a custom header
/// ([`GetOptions::headers`]; today only `data.vike.io`'s `X-API-KEY`) would send it to whatever host a
/// redirect names. Such a request therefore follows NO redirect (`send_on` sends it through
/// `vike_transport::http::request_no_redirect`; no keyed `data.vike.io` endpoint redirects, probed
/// 2026-11-22). A credential in `Authorization` (Databento's Basic, Tardis' Bearer) keeps the follow: the
/// strip is what lets their authenticated endpoints redirect to a presigned URL.
fn build_agent(connect: Duration, body_idle: Duration) -> ureq::Agent {
    use vike_transport::http::{CallBound, StatusPolicy};
    vike_transport::http::public_get_agent_bounded(
        Some(connect),
        CallBound::Idle(body_idle),
        // callers read the status themselves (a 404 is often expected, not a transport error)
        StatusPolicy::Response,
    )
}

/// Build the request on a fresh [`collector_agent`], apply the optional headers, and send it. `ctx`
/// is a short vendor prefix used in error messages (e.g. `"tardis"`).
fn send(
    url: &str,
    opts: &GetOptions,
    ctx: &str,
) -> Result<ureq::http::Response<ureq::Body>, CollectError> {
    send_on(&collector_agent(), url, opts, ctx)
}

/// [`send`] on the caller's agent, so a client that pools its connections keeps them.
fn send_on(
    agent: &ureq::Agent,
    url: &str,
    opts: &GetOptions,
    ctx: &str,
) -> Result<ureq::http::Response<ureq::Body>, CollectError> {
    let mut req = agent.get(url);
    if let Some(auth) = opts.authorization {
        req = req.header("Authorization", auth);
    }
    for (name, value) in opts.headers {
        req = req.header(*name, *value);
    }
    if !opts.headers.is_empty() {
        // A custom header is a key ureq would replay to any `Location` ([`GetOptions::headers`]).
        req = vike_transport::http::request_no_redirect(req);
    }
    req.call().map_err(|e| CollectError::Fetch(format!("{ctx} GET {url}: {e}").into()))
}

/// First 200 chars of a body — the error-message head shared by the status-failure branches.
fn head(s: &str) -> String {
    s.chars().take(200).collect()
}

/// Blocking GET → the response body as a `String`. Non-2xx → `CollectError::Fetch` with the body
/// head (the body is read first so it can be surfaced in the error).
pub fn get_to_string(url: &str, opts: &GetOptions, ctx: &str) -> Result<String, CollectError> {
    get_to_string_on(&collector_agent(), url, opts, ctx)
}

/// [`get_to_string`] on the caller's [`collector_agent`]: a client paging one host keeps its pool.
pub(crate) fn get_to_string_on(
    agent: &ureq::Agent,
    url: &str,
    opts: &GetOptions,
    ctx: &str,
) -> Result<String, CollectError> {
    get_answered_on(agent, url, opts, ctx).map_err(|failure| failure.error)
}

/// A failed [`get_to_string`] with the status the server answered, for a caller that decides on the status
/// (decision 0301: Yahoo's `404` for a ticker it does not list, `crate::eod::yahoo::fetch_splits`), never on the
/// words.
pub(crate) struct GetFailure {
    /// The non-2xx status; `None` when no answer was read.
    pub(crate) status: Option<u16>,
    /// The error, in the words every [`get_to_string`] caller gets.
    pub(crate) error: CollectError,
}

/// [`get_to_string`], its failure carrying the status ([`GetFailure`]).
pub(crate) fn get_to_string_answered(
    url: &str,
    opts: &GetOptions,
    ctx: &str,
) -> Result<String, GetFailure> {
    get_answered_on(&collector_agent(), url, opts, ctx)
}

fn get_answered_on(
    agent: &ureq::Agent,
    url: &str,
    opts: &GetOptions,
    ctx: &str,
) -> Result<String, GetFailure> {
    let unanswered = |error| GetFailure { status: None, error };
    let mut response = send_on(agent, url, opts, ctx).map_err(unanswered)?;
    let status = response.status().as_u16();
    // Under the network layer's DECODED bound (`vike_transport::http::RESPONSE_BODY_LIMIT`, 64 MiB, where ureq's own
    // default is 10 MiB; ureq 3.5.2 counts after gzip decoding): `data.vike.io`'s hourly asset panel grows with the
    // caller's window (`crate::vikedata::panel`), and its open archive `manifest.json` measured 1,117,823 bytes
    // decoded (142,209 gzipped) on 2026-11-21. A streamed download ([`get_to_file`], [`get_to_bytes`]) reads through
    // `as_reader`, which ureq does not limit.
    let body = vike_transport::http::read_body_text(response.body_mut())
        .map_err(|e| unanswered(CollectError::Fetch(format!("read {ctx} {url}: {e}").into())))?;
    if !(200..300).contains(&status) {
        return Err(GetFailure {
            status: Some(status),
            error: CollectError::Fetch(
                format!("{ctx} HTTP {status} from {url}: {}", head(&body)).into(),
            ),
        });
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
    let mut response = send(url, opts, ctx)?;
    let status = response.status().as_u16();
    if status == 404 {
        return Ok(None);
    }
    if !(200..300).contains(&status) {
        return Err(CollectError::Fetch(format!("{ctx} HTTP {status} from {url}").into()));
    }
    let mut buffer = Vec::new();
    response
        .body_mut()
        .as_reader()
        .read_to_end(&mut buffer)
        .map_err(|e| CollectError::Fetch(format!("read {ctx} body {url}: {e}").into()))?;
    Ok(Some(buffer))
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
    let mut response = send(url, opts, ctx)?;
    let status = response.status().as_u16();
    if treat_404_as_none && status == 404 {
        return Ok(None);
    }
    if !(200..300).contains(&status) {
        let body = response.body_mut().read_to_string().unwrap_or_default();
        return Err(CollectError::Fetch(
            format!("{ctx} HTTP {status} from {url}: {}", head(&body)).into(),
        ));
    }
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| CollectError::Fetch(format!("create {}: {e}", parent.display()).into()))?;
    }
    let mut out = File::create(dest)
        .map_err(|e| CollectError::Fetch(format!("create {}: {e}", dest.display()).into()))?;
    let mut reader = response.body_mut().as_reader();
    std::io::copy(&mut reader, &mut out).map_err(|e| {
        CollectError::Fetch(format!("stream {ctx} body to {}: {e}", dest.display()).into())
    })?;
    out.flush()
        .map_err(|e| CollectError::Fetch(format!("flush {}: {e}", dest.display()).into()))?;
    Ok(Some(dest.to_path_buf()))
}

#[path = "http_tests.rs"]
#[cfg(test)]
mod http_tests;
