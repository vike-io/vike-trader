//! The blocking-HTTP agents every crate of the workspace builds its calls on, and the finishers a hand-built agent
//! ends in.
//!
//! The ONE ureq [`Agent`](ureq::Agent) shape the venues use: 4xx/5xx returned as responses (not errors) so callers
//! parse each venue's own error body, plus a global timeout and the shared user-agent, a resolver that reports a failed
//! DNS lookup as `HostNotFound` (so `crate::send_outcome::send_error_is_definite` can call it definite) and NO redirect
//! following (a signed query is never replayed to a `Location`).
//!
//! **A builder with knobs of its own ends in a finisher** instead of `.build().new_agent()`: [`agent_no_redirect`] for a
//! call that carries a credential, a signature or a body, [`agent_public_get`] for a keyless public GET, and
//! [`agent_following`] for a caller that states another limit. Each installs [`DnsResolver`] and states the redirect
//! limit; the caller keeps its own timeouts and status policy. (`proxied_agent` is the one variant that cannot: its
//! SOCKS arm hands ureq a connector of its own, and it installs [`DnsResolver`] itself.)
//!
//! **Every agent names this client, [`USER_AGENT`]**: the finisher sets it (over whatever its builder said), and a
//! request that sets its own `User-Agent` header still sends that one (ureq 3.5.2 sends the agent's only when the
//! request has none), so SEC's declared contact string reaches SEC.
//!
//! **The builders whose knobs are parameters**, so a caller passes the values it reads and copies no recipe:
//! [`base_config`] (4xx/5xx as responses plus a total timeout, the start of every venue agent here),
//! [`stream_agent`] (a long-lived streamed response: a connect bound and a per-read idle bound, never a total),
//! [`public_get_agent_bounded`] (a keyless GET: an optional connect bound, a [`CallBound`], a [`StatusPolicy`]) and,
//! with `socks-proxy`, `proxied_agent` (a caller's own proxy: polymarket's tunnel). A failed call reads in one
//! fixed vocabulary through [`describe_error`].
//!
//! **The one exception to "follows none" is on purpose and named: [`public_get_agent`].** An UNSIGNED public GET (a
//! catalog, an instrument list) carries no credential and no body, so there is nothing for a `Location` to replay, and
//! a host that moves its endpoint behind a 301/308 should not turn a catalog read into an error. It follows at most
//! [`PUBLIC_GET_MAX_REDIRECTS`] hops and then fails (`TooManyRedirects`), the figure [`agent_public_get`] and
//! [`public_get_agent_bounded`] give every other keyless GET too. Every other builder here, and so every SIGNED call,
//! follows none.
//!
//! **A request that carries a credential ureq does not strip is sent through [`request_no_redirect`]** whatever its
//! agent follows: a custom key header or a key in the query (the data vendors' `X-API-Key`, Finnhub's token, FMP's
//! `apikey=`), so a 3xx is the answer and the key never reaches the `Location`.
//!
//! **No agent takes its proxy from the process environment.** ureq's default config reads `ALL_PROXY` /
//! `HTTPS_PROXY` / `HTTP_PROXY` (a setting no row holds, decision 0111); every finisher replaces whatever proxy its
//! builder carried with none, so every agent here dials direct. There is no box-wide proxy: the one proxied agent is
//! `proxied_agent` (feature `socks-proxy`), polymarket's own tunnel (its `venue.polymarket.*` rows).
//!
//! On top of the builders sits the keyless public read the crypto **catalogs** use, [`get_json`] (agent → status →
//! body → 200-range gate → parse), returning `Err(String)` so this crate need not name vike-catalog's `CatalogError`
//! (each catalog wraps it with a one-line `map_err`). It rides [`public_get_agent`], so it follows redirects.
//! [`get_json_with_headers`] is the same read carrying a header that is never a credential. The rate-limit-aware raw
//! GET of the kline backfills (`vike_bridge_core::http::get_raw`) stays with the venue layer.

use std::time::Duration;

/// Default global request timeout for venue REST calls.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

/// **The ONE `User-Agent` this client sends**: its own name and the workspace version, never a browser's. The
/// owner's standing rule is that vike is an HONEST client; okx, the Dukascopy `.bi5` feed, Yahoo's chart,
/// ForexFactory's calendar and Nasdaq's IPO calendar each used to send a `Mozilla/5.0 (…)` copy of their own, and
/// each answered this name with its normal reply when measured from a datacenter box on 2026-11-16. A provider that
/// refuses it is reported to the owner, never answered with a browser string. Every workspace crate shares the one
/// version, so this crate's is it. The finisher puts it on every agent ([`agent_following`]) and the WebSocket dial
/// on every upgrade that names none.
pub const USER_AGENT: &str = concat!("vike-trader-rust/", env!("CARGO_PKG_VERSION"));

/// **The base every venue agent here starts from**: 4xx/5xx returned as responses (callers parse each venue's
/// `{code,msg}` / `errorMessage` / `errorCode` out of the body) and `total` as the global timeout. The finisher it
/// ends in adds the resolver, the redirect limit, the proxy rule and [`USER_AGENT`].
pub fn base_config(total: Duration) -> ureq::config::ConfigBuilder<ureq::typestate::AgentScope> {
    ureq::Agent::config_builder().http_status_as_error(false).timeout_global(Some(total))
}

/// What a 4xx/5xx is to the caller of an agent built with a parameter for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusPolicy {
    /// A response, its status and body for the caller to read (a venue's own error body, a 404 that is expected).
    Response,
    /// `ureq::Error::StatusCode`: the caller's error path, which reads no body.
    Error,
}

impl StatusPolicy {
    fn status_as_error(self) -> bool {
        self == StatusPolicy::Error
    }
}

/// **The agent of a long-lived streamed response** (a server-sent event stream, a chunked transaction or price
/// stream): `connect` bounds the dial, `read_idle` EACH read, `response` (when given) the wait for the response
/// head, and `status` says what a 4xx/5xx is. It follows no redirect (the open carries a credential) and has no
/// total timeout.
///
/// ⚠ **`read_idle` is a PER-READ bound (`timeout_per_read`), never `timeout_recv_body`**: in ureq 3.5 that one is a
/// total budget on the whole body, so it would end a healthy stream that many seconds after it opened. The per-read
/// bound caps the read that waits for the head too, so a host that takes the request and never answers fails after
/// `read_idle` even with no `response`.
///
/// ⚠ **The connect bound is a SHUTDOWN fix, not a networking nicety.** ureq's `Timeouts::default()` leaves
/// `connect: None`, so without it the dial parks in the kernel for the OS connect timeout (~130 s on Linux) on an
/// unreachable host. A stream thread reads its `stop` flag only BETWEEN reads, so for that whole window it cannot
/// observe a stop, and the thread that joins it (`ExecutionClientActor::stop` in
/// `crates/vike-bridge-core/src/exec_actor.rs`, with no timeout, inside `vike_boot::shutdown::run_with_deadline`'s
/// sequential tail, which the task counter cannot see) blows the shutdown deadline while the daemon reports
/// `0 task(s) still in flight`. Measured, not hypothetical: every Alpaca host was TCP-unreachable from the box in
/// the alpaca+ctrader live rehearsal (PR #1407, Evidence 4: DNS resolves, TCP 443 times out on both tiers).
///
/// ⚠ **Deliberately NOT `timeout_global`**: the request IS the stream, and a global timeout would cap the stream
/// itself and force a reconnect every N seconds. The connect bound covers only the dial (TCP and TLS, since ureq
/// 3.4.1); `read_idle` is the idle bound.
///
/// The bounds are parameters so a test can build the same agent with short ones and prove each one gives up.
pub fn stream_agent(
    connect: Duration,
    response: Option<Duration>,
    read_idle: Duration,
    status: StatusPolicy,
) -> ureq::Agent {
    agent_no_redirect(
        ureq::Agent::config_builder()
            .http_status_as_error(status.status_as_error())
            .timeout_connect(Some(connect))
            .timeout_recv_response(response)
            .timeout_per_read(Some(read_idle)),
    )
}

/// What bounds a call after its connect.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallBound {
    /// The whole call, head and body (`timeout_global`): a page, a feed, a file small enough to have a ceiling.
    Total(Duration),
    /// Each read, the head's included (`timeout_per_read`), and no ceiling: a large download on a slow line is slow,
    /// a line silent for this long is dead.
    Idle(Duration),
}

/// **The agent of a keyless public GET whose bounds are the caller's**: `connect` (when given) bounds the dial,
/// `bound` the rest of the call, `status` says what a 4xx/5xx is, ended in [`agent_public_get`] (the resolver, at
/// most [`PUBLIC_GET_MAX_REDIRECTS`] redirects). ⚠ Never for a call that carries a credential or a body
/// ([`public_get_agent`]'s doc says why).
pub fn public_get_agent_bounded(
    connect: Option<Duration>,
    bound: CallBound,
    status: StatusPolicy,
) -> ureq::Agent {
    let builder = ureq::Agent::config_builder()
        .http_status_as_error(status.status_as_error())
        .timeout_connect(connect);
    agent_public_get(match bound {
        CallBound::Total(total) => builder.timeout_global(Some(total)),
        CallBound::Idle(idle) => builder.timeout_per_read(Some(idle)),
    })
}

/// **The agent through a proxy of the caller's own** (polymarket's tunnel; there is no box-wide one): [`base_config`]
/// (4xx/5xx as responses, `total`), no redirect (the calls carry credentials), the resolver and [`USER_AGENT`].
///
/// - A SOCKS proxy is dialled by `crate::socks::SocksDial`, never ureq's stock SOCKS connector (its timeout reads as
///   may-have-landed; that module's doc), and so within `crate::socks::SOCKS_DIAL_LIMIT`.
/// - An `http(s)://` proxy keeps ureq's stock CONNECT chain, whose dial and reads keep `total`.
/// - ⚠ **`Err`: a proxy was DECLARED and cannot be used, and that never goes direct** (a direct dial leaves by this
///   box's own exit, around the tunnel the declaration asked for, with the account's signed headers on it). The
///   agent still builds, and refuses each request before a socket exists with `InvalidProxyUrl`, which
///   `crate::send_outcome::send_error_is_definite` calls unsent. The caller words the error (it may carry a secret).
#[cfg(feature = "socks-proxy")]
pub fn proxied_agent(total: Duration, proxy: Result<ureq::Proxy, ureq::Error>) -> ureq::Agent {
    use ureq::unversioned::transport::{Connector, DefaultConnector, RustlsConnector};
    let config = |proxy: Option<ureq::Proxy>| {
        base_config(total).max_redirects(0).proxy(proxy).user_agent(USER_AGENT).build()
    };
    match proxy {
        Ok(proxy) if is_socks(&proxy) => ureq::Agent::with_parts(
            config(Some(proxy)),
            crate::socks::SocksDial.chain(RustlsConnector::default()),
            DnsResolver::default(),
        ),
        Ok(proxy) => ureq::Agent::with_parts(
            config(Some(proxy)),
            DefaultConnector::default(),
            DnsResolver::default(),
        ),
        // `proxy(None)`: a request through a proxy never asks the resolver that refuses.
        Err(_) => {
            ureq::Agent::with_parts(config(None), DefaultConnector::default(), RefuseUnusableProxy)
        }
    }
}

#[cfg(feature = "socks-proxy")]
fn is_socks(proxy: &ureq::Proxy) -> bool {
    use ureq::ProxyProtocol as P;
    matches!(proxy.protocol(), P::Socks4 | P::Socks4A | P::Socks5 | P::Socks5h)
}

/// The resolver of a [`proxied_agent`] whose declared proxy cannot be used: every request fails here, before a
/// socket exists.
#[cfg(feature = "socks-proxy")]
#[derive(Debug)]
struct RefuseUnusableProxy;

#[cfg(feature = "socks-proxy")]
impl ureq::unversioned::resolver::Resolver for RefuseUnusableProxy {
    fn resolve(
        &self,
        _uri: &ureq::http::Uri,
        _config: &ureq::config::Config,
        _timeout: ureq::unversioned::transport::NextTimeout,
    ) -> Result<ureq::unversioned::resolver::ResolvedSocketAddrs, ureq::Error> {
        Err(ureq::Error::InvalidProxyUrl)
    }
}

/// **A failed call in one short, fixed vocabulary** (`HTTP 429`, `timed out`, `I/O error (ConnectionRefused)`,
/// `host not found`, `connection failed`, `redirect failed`, `the body was too large`, `request failed`) that never
/// formats the error itself: a keyed provider's URL carries its key (FMP's `apikey=`), and ureq's own text is not
/// promised to omit it, so a line logged from here names no URL and no credential.
pub fn describe_error(error: &ureq::Error) -> String {
    match error {
        ureq::Error::StatusCode(code) => format!("HTTP {code}"),
        ureq::Error::Timeout(_) => "timed out".to_string(),
        ureq::Error::Io(io) => format!("I/O error ({:?})", io.kind()),
        ureq::Error::HostNotFound => "host not found".to_string(),
        ureq::Error::ConnectionFailed => "connection failed".to_string(),
        ureq::Error::TooManyRedirects | ureq::Error::RedirectFailed => {
            "redirect failed".to_string()
        }
        ureq::Error::BodyExceedsLimit(_) => "the body was too large".to_string(),
        _ => "request failed".to_string(),
    }
}

/// The most a venue reply body may hold once DECODED: 64 MiB, read by [`read_body_text`].
///
/// Since ureq 3.5.2 (algesten/ureq#1231) a body limit counts the bytes AFTER gzip decoding; up to 3.5.1
/// it counted the bytes on the wire, so ureq's 10 MiB default for `read_to_string` let a large list
/// through as long as its gzip was small. Measured 2026-11-21, decoded (gzipped): Binance spot
/// `/api/v3/exchangeInfo` 18,152,669 (339,205), refused by that default under 3.5.2 and read whole
/// under 3.4.0; Deribit `get_instruments?currency=any` 4,929,121 (113,991); OKX SPOT instruments
/// 1,350,546 (75,810); Binance USDⓈ-M `exchangeInfo` 1,189,717 (53,433); Bybit linear
/// instruments-info 880,851; Aster USDⓈ-M `exchangeInfo` 890,709; a Polymarket gamma keyset page of
/// 500 632,080; Hyperliquid `spotMetaAndAssetCtxs` 366,814. 64 MiB is ~3.7 times the largest, room
/// for Binance's list to keep growing, and still a bound: a decompression bomb stops here, at 64 MiB
/// of memory, not at whatever it inflates to.
pub const RESPONSE_BODY_LIMIT: u64 = 64 * 1024 * 1024;

/// A reply body read whole as text under [`RESPONSE_BODY_LIMIT`]: the ONE way a reader of a list that may outgrow
/// ureq's 10 MiB default reads a body. Past the limit it is ureq's `BodyExceedsLimit`.
pub fn read_body_text(body: &mut ureq::Body) -> Result<String, ureq::Error> {
    body.with_config().limit(RESPONSE_BODY_LIMIT).read_to_string()
}

/// A blocking ureq agent with the vike venue defaults (30s global timeout).
pub fn blocking_agent() -> ureq::Agent {
    blocking_agent_with_timeout(DEFAULT_TIMEOUT)
}

/// A blocking ureq agent with an explicit global timeout (e.g. a short listen-key refresh).
pub fn blocking_agent_with_timeout(timeout: Duration) -> ureq::Agent {
    agent_no_redirect(base_config(timeout))
}

/// [`blocking_agent_with_timeout`] with the TCP connect bounded apart: `connect` for the connect,
/// `total` for the whole call. A host that drops the SYN (an IP the server fenced off) is told apart
/// by [`is_connect_failure`] from a slow answer, which still has `total` to arrive.
pub fn blocking_agent_with_connect_timeout(connect: Duration, total: Duration) -> ureq::Agent {
    agent_no_redirect(base_config(total).timeout_connect(Some(connect)))
}

/// Whether `error` says the connection never completed: its bound ran out (`Timeout::Connect`, which
/// since ureq 3.4.1 covers the TLS handshake too), or every resolved address refused. ureq 3.5 splits
/// the connect bound across the addresses and, when all of them fail, reports the last one's own
/// failure (a refusal, or the timeout), so both mean "no connection was made".
pub fn is_connect_failure(error: &ureq::Error) -> bool {
    match error {
        ureq::Error::Timeout(ureq::Timeout::Connect) => true,
        ureq::Error::Io(e) => e.kind() == std::io::ErrorKind::ConnectionRefused,
        _ => false,
    }
}

/// Whether `error` is one of ureq's own time bounds running out (`Error::Timeout`, whichever phase:
/// the lookup, the connect, the send or the answer).
pub fn is_timeout(error: &ureq::Error) -> bool {
    matches!(error, ureq::Error::Timeout(_))
}

/// ureq 3.5 reports a real DNS failure as `Error::Io(Uncategorized)` (`HostNotFound` only for an
/// EMPTY answer), which `crate::send_outcome::send_error_is_definite` must treat as ambiguous. A
/// lookup happens before any byte is written, so this resolver says what it is: `HostNotFound`.
/// Every agent here and every finisher installs it; `proxied_agent`, which cannot end in a finisher, installs it itself.
#[derive(Debug, Default)]
pub struct DnsResolver(ureq::unversioned::resolver::DefaultResolver);

impl ureq::unversioned::resolver::Resolver for DnsResolver {
    fn resolve(
        &self,
        uri: &ureq::http::Uri,
        config: &ureq::config::Config,
        timeout: ureq::unversioned::transport::NextTimeout,
    ) -> Result<ureq::unversioned::resolver::ResolvedSocketAddrs, ureq::Error> {
        match self.0.resolve(uri, config, timeout) {
            Err(ureq::Error::Io(_)) => Err(ureq::Error::HostNotFound),
            other => other,
        }
    }
}

/// **The finisher of a call that carries a credential, a signature or a body**: `builder` (the caller's own
/// timeouts, user agent and status policy), [`DnsResolver`], and NO redirect following (a signed query must never be
/// replayed to a `Location`, and a hop failing after the first request was delivered must not read as a pre-write
/// failure). ureq replays every header but `Authorization` and `Cookie` to a `Location`, so a key header would follow.
pub fn agent_no_redirect(
    builder: ureq::config::ConfigBuilder<ureq::typestate::AgentScope>,
) -> ureq::Agent {
    agent_following(builder, 0)
}

/// **The finisher of a keyless public GET**: `builder`, [`DnsResolver`], and at most [`PUBLIC_GET_MAX_REDIRECTS`]
/// followed redirects. ⚠ Never for a call that carries a credential or a body ([`public_get_agent`]'s doc says why).
pub fn agent_public_get(
    builder: ureq::config::ConfigBuilder<ureq::typestate::AgentScope>,
) -> ureq::Agent {
    agent_following(builder, PUBLIC_GET_MAX_REDIRECTS)
}

/// The agent behind both finishers: `builder`, the [`DnsResolver`], a redirect limit that is always STATED by the
/// caller (never ureq's default of ten left unsaid), NO proxy and [`USER_AGENT`] (over any the builder set; a
/// request's own header still wins). A finisher's agent never keeps the proxy its builder carried, the one ureq's
/// default config read from the environment included: it dials direct.
pub fn agent_following(
    builder: ureq::config::ConfigBuilder<ureq::typestate::AgentScope>,
    max_redirects: u32,
) -> ureq::Agent {
    ureq::Agent::with_parts(
        builder.max_redirects(max_redirects).proxy(None).user_agent(USER_AGENT).build(),
        ureq::unversioned::transport::DefaultConnector::default(),
        DnsResolver::default(),
    )
}

/// `request`, sent following NO redirect whatever its agent follows: for a request carrying a credential ureq does
/// not strip on a redirect (it removes only `Authorization` and `Cookie`), a custom key header or a key in the query.
/// The 3xx comes back as the response, for the caller's non-2xx path to report. Nothing else of the agent's config
/// changes (a request's config is a copy of its agent's), so a pooled agent keeps its pool.
/// shortcut: no own-origin follow (no keyed vendor redirects, probed 2026-11-22); add one here when one does.
pub fn request_no_redirect<B>(request: ureq::RequestBuilder<B>) -> ureq::RequestBuilder<B> {
    request.config().max_redirects(0).build()
}

/// How many redirects an UNSIGNED public GET follows before it fails (`TooManyRedirects`): five,
/// which is more than any host designs on purpose (a scheme hop, a canonical-host hop, a CDN hop)
/// and few enough that a redirect loop fails fast. A decision, not ureq's default of ten.
/// [`agent_public_get`] gives the same figure to the agents outside the venue stack.
pub const PUBLIC_GET_MAX_REDIRECTS: u32 = 5;

/// A blocking ureq agent for an UNSIGNED public GET (a catalog, an instrument list): exactly
/// [`blocking_agent`] (4xx/5xx as responses, the 30 s global timeout, the user agent, the resolver)
/// except that it FOLLOWS up to [`PUBLIC_GET_MAX_REDIRECTS`] redirects, on purpose.
///
/// ⚠ **Never for a call that carries a credential or a body.** ureq drops `Authorization` and
/// `Cookie` on a redirect and nothing else, so a custom key header would be replayed to whatever host
/// the `Location` names; a signed query likewise. Those calls build [`blocking_agent`] and its
/// siblings, which follow none.
pub fn public_get_agent() -> ureq::Agent {
    agent_public_get(base_config(DEFAULT_TIMEOUT))
}

/// A blocking ureq agent sized for CONCURRENT use: [`blocking_agent`] plus room in the connection
/// pool for `lanes` simultaneous requests to the same host.
///
/// ureq 3.3 defaults to **3** idle connections per host (and 10 overall). A pager running more lanes
/// than that against one host pays a fresh DNS+TLS handshake on every surplus request. That is not
/// merely slower: it distorts the pacer's own round-trip measurement, which the lane count is
/// DERIVED from (`vike_bridge_core::pacer::Pacer::suggested_lanes`), so the concurrency would end up partly
/// measuring its own pool starvation.
///
/// `lanes` only ever WIDENS the pool (it is clamped up to the ureq defaults, never down). Everything
/// else — the 4xx/5xx-as-response policy, the 30 s global timeout, the user agent — is
/// [`blocking_agent`] verbatim, so a request issued through this agent is identical on the wire.
pub fn blocking_agent_for_lanes(lanes: usize) -> ureq::Agent {
    // ureq 3.3's own defaults, restated so "widen only" is provable rather than assumed.
    const DEFAULT_IDLE_PER_HOST: usize = 3;
    const DEFAULT_IDLE_TOTAL: usize = 10;
    let per_host = lanes.max(DEFAULT_IDLE_PER_HOST);
    agent_no_redirect(
        base_config(DEFAULT_TIMEOUT)
            .max_idle_connections_per_host(per_host)
            .max_idle_connections(per_host.max(DEFAULT_IDLE_TOTAL)),
    )
}

/// A blocking ureq agent that does NOT verify TLS certificates — for the IBKR Client Portal
/// Gateway's self-signed cert on `https://localhost:5000` ONLY.
///
/// ureq 3.3 exposes cert-verification bypass directly (`TlsConfig::disable_verification`), so no
/// custom rustls verifier is needed. The bypass is agent-wide, so the caller MUST only ever point
/// this agent at a loopback host — gate its use with `vike_utility::url::is_loopback_url`. NEVER use it for a
/// non-loopback host. Built through [`agent_no_redirect`] like the others, so it has the DNS resolver and follows no
/// redirect.
pub fn blocking_agent_loopback_insecure() -> ureq::Agent {
    let tls = ureq::tls::TlsConfig::builder().disable_verification(true).build();
    agent_no_redirect(base_config(DEFAULT_TIMEOUT).tls_config(tls))
}

/// Blocking keyless `GET url` → parsed JSON, over a fresh [`public_get_agent`] (it follows up to
/// [`PUBLIC_GET_MAX_REDIRECTS`] redirects: a catalog host that moves its endpoint is still read).
///
/// Call, read the status, drain the body, gate on the 200-range (error carries a [`body_head`] of
/// the body), then parse. Errors are per-stage strings — `"{url} GET: …"`, `"{url} read: …"`,
/// `"{url} HTTP {status}: {head}"`, `"{url} JSON: …"` — as a plain `String`, since this crate
/// cannot name vike-catalog's `CatalogError`; each catalog keeps the one-line `.map_err(CatalogError)`.
///
/// For an authed/signed read use the venue's own `rest`/transport client, not this: it follows
/// redirects, which a call that carries a credential must never do.
pub fn get_json(url: &str) -> Result<serde_json::Value, String> {
    get_json_with_headers(url, &[])
}

/// [`get_json`] with `headers` set on the request (okx's demo-network `x-simulated-trading` on a keyless
/// instrument read); the same agent, gate, parse and error words.
///
/// ⚠ **A header here is never a credential.** The agent is [`public_get_agent`], which follows
/// redirects, and ureq replays every header but `Authorization` and `Cookie` to the `Location`.
pub fn get_json_with_headers(
    url: &str,
    headers: &[(&str, &str)],
) -> Result<serde_json::Value, String> {
    let agent = public_get_agent();
    let mut req = agent.get(url);
    for (name, value) in headers {
        req = req.header(*name, *value);
    }
    let mut response = req.call().map_err(|e| format!("{url} GET: {e}"))?;
    let status = response.status().as_u16();
    let body = read_body_text(response.body_mut()).map_err(|e| format!("{url} read: {e}"))?;
    if !(200..300).contains(&status) {
        return Err(format!("{url} HTTP {status}: {}", body_head(&body)));
    }
    serde_json::from_str(&body).map_err(|e| format!("{url} JSON: {e}"))
}

/// A response body parsed as JSON; the error reads `"{what} json: {e}"`, `what` being the venue's own
/// words (`"bybit kline"`, `"okx candle"`).
pub fn json_body(body: &str, what: &str) -> Result<serde_json::Value, String> {
    serde_json::from_str(body).map_err(|e| format!("{what} json: {e}"))
}

/// Truncate a (possibly large) error body to its first 200 chars for a diagnostic message.
pub fn body_head(body: &str) -> String {
    body.chars().take(200).collect()
}

#[cfg(test)]
#[path = "http_tests.rs"]
mod tests;
