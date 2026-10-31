//! Shared blocking-HTTP helpers for the venue REST clients.
//!
//! The ONE ureq [`Agent`](ureq::Agent) shape every venue uses: 4xx/5xx returned as responses (not
//! errors) so callers parse each venue's own error body, plus a global timeout and the shared
//! user-agent, a resolver that reports a failed DNS lookup as `HostNotFound` (so the send-error
//! classifier can call it definite) and NO redirect following (a signed query is never replayed to
//! a `Location`). (Polymarket keeps its own proxy-aware builder — a genuine variant, not a copy.)
//!
//! **The one exception is on purpose and named: [`public_get_agent`].** An UNSIGNED public GET (a
//! catalog, an instrument list) carries no credential and no body, so there is nothing for a
//! `Location` to replay, and a host that moves its endpoint behind a 301/308 should not turn a
//! catalog read into an error. It follows at most [`PUBLIC_GET_MAX_REDIRECTS`] hops and then fails
//! (`TooManyRedirects`), the same shape as the two agents outside the venue stack that cannot link
//! this crate (`crates/vike-datahub/src/tool_feeds/http.rs`'s `UreqHttp` and
//! `crates/vike-backfill/src/http.rs`'s `build_agent`, which state the same figure for themselves).
//! Every other builder here, and so every SIGNED call, still follows none.
//!
//! On top of the builders sit two GET shapes:
//! - [`get_json`] — the keyless public read the crypto **catalogs** use (agent → status → body →
//!   200-range gate → parse), returning `Err(String)` so bridge-core need not name vike-catalog's
//!   `CatalogError` (each catalog wraps it with a one-line `map_err`). It rides
//!   [`public_get_agent`], so it follows redirects.
//! - [`get_raw`] + [`RawResp`] + [`body_head`] — the rate-limit-aware raw GET of the kline backfill
//!   modules (`data.rs`): capture status + `Retry-After` (+ the Binance-family weight header)
//!   BEFORE draining the body, so the per-venue retry loops can classify and back off. The response
//!   *classification* stays per-venue (which statuses/business codes mean rate limited genuinely
//!   differ), and so do the PAGING loops (forward vs backward). The backoff *cadence* is the shared
//!   [`crate::retry::retry_rate_limited`] driver (the family's page `used_weight` rides out via the
//!   `Verdict::Done` payload).

use std::time::Duration;

/// Default global request timeout for venue REST calls.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

/// A blocking ureq agent with the vike venue defaults (30s global timeout).
pub fn blocking_agent() -> ureq::Agent {
    blocking_agent_with_timeout(DEFAULT_TIMEOUT)
}

/// A blocking ureq agent with an explicit global timeout (e.g. a short listen-key refresh).
pub fn blocking_agent_with_timeout(timeout: Duration) -> ureq::Agent {
    venue_agent(
        ureq::Agent::config_builder()
            // callers parse each venue's {code,msg} / errorMessage / errorCode out of 4xx bodies
            .http_status_as_error(false)
            .timeout_global(Some(timeout))
            .user_agent("vike-trader-rust"),
    )
}

/// ureq 3.4 reports a real DNS failure as `Error::Io(Uncategorized)` (`HostNotFound` only for an
/// EMPTY answer), which [`crate::transport::send_error_is_definite`] must treat as ambiguous. A
/// lookup happens before any byte is written, so this resolver says what it is: `HostNotFound`.
#[derive(Debug, Default)]
struct DnsResolver(ureq::unversioned::resolver::DefaultResolver);

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

/// Build the venue agent: the config, [`DnsResolver`], and NO redirect following (a signed query
/// must never be replayed to a `Location`, and a hop failing after the first request was delivered
/// must not read as a pre-write failure).
fn venue_agent(builder: ureq::config::ConfigBuilder<ureq::typestate::AgentScope>) -> ureq::Agent {
    agent_following(builder, 0)
}

/// The agent behind [`venue_agent`] (`max_redirects` 0) and [`public_get_agent`]: the config, the
/// [`DnsResolver`] and a redirect limit that is always STATED, never ureq's default of ten.
fn agent_following(
    builder: ureq::config::ConfigBuilder<ureq::typestate::AgentScope>,
    max_redirects: u32,
) -> ureq::Agent {
    ureq::Agent::with_parts(
        builder.max_redirects(max_redirects).build(),
        ureq::unversioned::transport::DefaultConnector::default(),
        DnsResolver::default(),
    )
}

/// How many redirects an UNSIGNED public GET follows before it fails (`TooManyRedirects`): five,
/// which is more than any host designs on purpose (a scheme hop, a canonical-host hop, a CDN hop)
/// and few enough that a redirect loop fails fast. A decision, not ureq's default of ten.
/// `crates/vike-datahub/src/tool_feeds/http.rs` and `crates/vike-backfill/src/http.rs` state the same
/// figure for the two agents outside the venue stack; a test in each pins it.
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
    agent_following(
        ureq::Agent::config_builder()
            // callers parse each venue's {code,msg} / errorMessage / errorCode out of 4xx bodies
            .http_status_as_error(false)
            .timeout_global(Some(DEFAULT_TIMEOUT))
            .user_agent("vike-trader-rust"),
        PUBLIC_GET_MAX_REDIRECTS,
    )
}

/// A blocking ureq agent sized for CONCURRENT use: [`blocking_agent`] plus room in the connection
/// pool for `lanes` simultaneous requests to the same host.
///
/// ureq 3.3 defaults to **3** idle connections per host (and 10 overall). A pager running more lanes
/// than that against one host pays a fresh DNS+TLS handshake on every surplus request. That is not
/// merely slower: it distorts the pacer's own round-trip measurement, which the lane count is
/// DERIVED from ([`crate::pacer::Pacer::suggested_lanes`]), so the concurrency would end up partly
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
    venue_agent(
        ureq::Agent::config_builder()
            // callers parse each venue's {code,msg} / errorMessage / errorCode out of 4xx bodies
            .http_status_as_error(false)
            .timeout_global(Some(DEFAULT_TIMEOUT))
            .user_agent("vike-trader-rust")
            .max_idle_connections_per_host(per_host)
            .max_idle_connections(per_host.max(DEFAULT_IDLE_TOTAL)),
    )
}

/// A blocking ureq agent that does NOT verify TLS certificates — for the IBKR Client Portal
/// Gateway's self-signed cert on `https://localhost:5000` ONLY.
///
/// ureq 3.3 exposes cert-verification bypass directly (`TlsConfig::disable_verification`), so no
/// custom rustls verifier is needed. The bypass is agent-wide, so the caller MUST only ever point
/// this agent at a loopback host — gate its use with [`is_loopback_url`]. NEVER use it for a
/// non-loopback host.
pub fn blocking_agent_loopback_insecure() -> ureq::Agent {
    let tls = ureq::tls::TlsConfig::builder().disable_verification(true).build();
    venue_agent(
        ureq::Agent::config_builder()
            .http_status_as_error(false)
            .timeout_global(Some(DEFAULT_TIMEOUT))
            .user_agent("vike-trader-rust")
            .tls_config(tls),
    )
}

/// True when `url`'s host is a loopback address (`localhost`, `127.0.0.0/8`, or `::1`). The guard a
/// caller checks before trusting [`blocking_agent_loopback_insecure`] (which disables cert
/// verification) — a non-loopback URL must never be served by the insecure agent. Built through
/// [`venue_agent`] like the others, so it has the DNS resolver and follows no redirect.
pub fn is_loopback_url(url: &str) -> bool {
    let after = url.split("://").nth(1).unwrap_or(url);
    let hostport = after.split('/').next().unwrap_or("");
    // Isolate the host: an IPv6 literal is bracketed (`[::1]:5000`); otherwise strip a `:port` from
    // the RIGHT so an IPv4/hostname keeps its dots.
    let host = if let Some(rest) = hostport.strip_prefix('[') {
        rest.split(']').next().unwrap_or("")
    } else {
        hostport.rsplit_once(':').map_or(hostport, |(h, _)| h)
    };
    // Only the exact literal "localhost", or a host that PARSES as a loopback IP. A prefix check
    // (e.g. `starts_with("127.")`) would wrongly accept `127.0.0.1.evil.com` — and this guard gates
    // the cert-verification-DISABLED agent, so it must not be fooled by a loopback-looking hostname.
    host == "localhost"
        || host.parse::<std::net::IpAddr>().map(|ip| ip.is_loopback()).unwrap_or(false)
}

/// Blocking keyless `GET url` → parsed JSON, over a fresh [`public_get_agent`] (it follows up to
/// [`PUBLIC_GET_MAX_REDIRECTS`] redirects: a catalog host that moves its endpoint is still read).
///
/// Call, read the status, drain the body, gate on the 200-range (error carries a [`body_head`] of
/// the body), then parse. Errors are per-stage strings — `"{url} GET: …"`, `"{url} read: …"`,
/// `"{url} HTTP {status}: {head}"`, `"{url} JSON: …"` — as a plain `String`, since bridge-core
/// cannot name vike-catalog's `CatalogError` without a new dep edge; each catalog keeps the
/// one-line `.map_err(CatalogError)`.
///
/// For an authed/signed read use the venue's own `rest`/transport client, not this: it follows
/// redirects, which a call that carries a credential must never do.
pub fn get_json(url: &str) -> Result<serde_json::Value, String> {
    let agent = public_get_agent();
    let mut resp = agent.get(url).call().map_err(|e| format!("{url} GET: {e}"))?;
    let status = resp.status().as_u16();
    let body = resp.body_mut().read_to_string().map_err(|e| format!("{url} read: {e}"))?;
    if !(200..300).contains(&status) {
        return Err(format!("{url} HTTP {status}: {}", body_head(&body)));
    }
    serde_json::from_str(&body).map_err(|e| format!("{url} JSON: {e}"))
}

/// Truncate a (possibly large) error body to its first 200 chars for a diagnostic message.
pub fn body_head(body: &str) -> String {
    body.chars().take(200).collect()
}

/// A raw HTTP result captured before the body is interpreted: the status, the rate-limit-relevant
/// headers, and the body.
///
/// `used_weight` is the Binance-family `X-MBX-USED-WEIGHT-1M` (binance/aster read it to cool down
/// proactively); it is simply `None` on venues that don't send it, which those callers ignore.
pub struct RawResp {
    pub status: u16,
    pub retry_after: Option<Duration>,
    pub used_weight: Option<u64>,
    pub body: String,
}

/// One blocking GET capturing status + the rate-limit headers before draining the body.
///
/// `label` prefixes the two error messages (`"{label} GET: …"` / `"{label} read: …"`), keeping each
/// venue's wording. `extra_headers` are set on the request (OKX passes its Cloudflare browser
/// User-Agent + Accept). The shared agent has `http_status_as_error(false)`, so 429/418/403 arrive
/// here as a `status`, NOT as a transport error — which is what lets the caller's retry loop honor
/// `Retry-After` instead of failing the page.
pub fn get_raw(
    agent: &ureq::Agent,
    url: &str,
    label: &str,
    extra_headers: &[(&str, &str)],
) -> Result<RawResp, String> {
    let mut req = agent.get(url);
    for (name, value) in extra_headers {
        req = req.header(*name, *value);
    }
    let mut resp = req.call().map_err(|e| format!("{label} GET: {e}"))?;
    let status = resp.status().as_u16();
    let retry_after = resp
        .headers()
        .get("retry-after")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.trim().parse::<u64>().ok())
        .map(Duration::from_secs);
    let used_weight = resp
        .headers()
        .get("x-mbx-used-weight-1m")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.trim().parse::<u64>().ok());
    let body = resp.body_mut().read_to_string().map_err(|e| format!("{label} read: {e}"))?;
    Ok(RawResp { status, retry_after, used_weight, body })
}

#[cfg(test)]
mod body_head_tests {
    use super::body_head;

    #[test]
    fn truncates_to_200_chars_and_passes_short_bodies_through() {
        assert_eq!(body_head("short"), "short");
        assert_eq!(body_head(&"x".repeat(500)).len(), 200);
        // chars(), not bytes — a multi-byte body must not split a char boundary/panic.
        assert_eq!(body_head(&"é".repeat(500)).chars().count(), 200);
    }
}

#[cfg(test)]
mod loopback_tests {
    use super::is_loopback_url;

    #[test]
    fn loopback_hosts_only() {
        assert!(is_loopback_url("https://127.0.0.1:5000"));
        assert!(is_loopback_url("https://localhost:5000/v1/api"));
        assert!(is_loopback_url("https://127.0.0.5:5000"));
        assert!(is_loopback_url("https://[::1]:5000"));
        assert!(!is_loopback_url("https://api.ibkr.com/v1/api"));
        assert!(!is_loopback_url("https://<host>:5000"));
        assert!(!is_loopback_url("https://127evil.com:5000")); // not a parseable IP
        // The security case: a loopback-LOOKING hostname must NOT pass (it would enable MITM under
        // the cert-bypass agent). Only real loopback IPs / literal "localhost" do.
        assert!(!is_loopback_url("https://127.0.0.1.evil.com:5000"));
        assert!(!is_loopback_url("https://localhost.evil.com:5000"));
    }
}

#[cfg(test)]
mod loopback_agent_tests {
    use super::blocking_agent_loopback_insecure;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// A server that answers every request with `reply` and counts the requests it served.
    pub(super) fn serve(reply: String) -> (String, Arc<AtomicUsize>) {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        let hits = Arc::new(AtomicUsize::new(0));
        let n = hits.clone();
        std::thread::spawn(move || {
            for conn in l.incoming() {
                let Ok(mut s) = conn else { continue };
                let mut buf = [0u8; 2048];
                let _ = s.read(&mut buf);
                n.fetch_add(1, Ordering::SeqCst);
                let _ = s.write_all(reply.as_bytes());
            }
        });
        (format!("http://{addr}"), hits)
    }

    /// The loopback agent shares the venue agents' two guarantees: a 3xx is the answer (a request is
    /// never replayed to its `Location`, which for a 301/302/303 would turn a POST or DELETE into a
    /// bodyless GET carrying every header but `Authorization`), and a failed DNS lookup is the
    /// definite `HostNotFound`.
    #[test]
    fn the_loopback_agent_follows_no_redirect() {
        let (target, target_hits) =
            serve("HTTP/1.1 200 OK\r\ncontent-length: 2\r\nconnection: close\r\n\r\n{}".into());
        let (base, _) = serve(format!(
            "HTTP/1.1 302 Found\r\nlocation: {target}/y\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"
        ));
        let agent = blocking_agent_loopback_insecure();
        for resp in [
            agent.post(&format!("{base}/x")).send_empty(),
            agent.delete(&format!("{base}/x")).call(),
        ] {
            assert_eq!(resp.expect("the 302 is a response").status().as_u16(), 302);
        }
        assert_eq!(target_hits.load(Ordering::SeqCst), 0, "the Location was never requested");
    }

    #[test]
    fn a_dns_failure_through_the_loopback_agent_is_host_not_found() {
        let raw =
            blocking_agent_loopback_insecure().post("http://no-such-host.invalid/x").send_empty();
        assert!(matches!(raw, Err(ureq::Error::HostNotFound)), "{raw:?}");
    }
}

/// **Redirects are a decision, per kind of call** (A50): an UNSIGNED public GET follows a few on
/// purpose, every other builder follows none. Each test below is a loopback server, no network.
#[cfg(test)]
mod redirect_tests {
    use super::loopback_agent_tests::serve;
    use super::{
        PUBLIC_GET_MAX_REDIRECTS, blocking_agent, blocking_agent_for_lanes,
        blocking_agent_loopback_insecure, blocking_agent_with_timeout, get_json, public_get_agent,
    };
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    const OK_JSON: &str = "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: 11\r\nconnection: close\r\n\r\n{\"ok\":true}";

    /// A redirect to `location` (a 302), and nothing else.
    fn redirect_to(location: &str) -> String {
        format!(
            "HTTP/1.1 302 Found\r\nlocation: {location}\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"
        )
    }

    /// A server that answers EVERY request with a redirect to itself (a loop), counting the requests.
    fn serve_a_loop() -> (String, Arc<AtomicUsize>) {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", l.local_addr().unwrap());
        let hits = Arc::new(AtomicUsize::new(0));
        let (n, reply) = (hits.clone(), redirect_to(&format!("{base}/again")));
        std::thread::spawn(move || {
            for conn in l.incoming() {
                let Ok(mut s) = conn else { continue };
                let mut buf = [0u8; 2048];
                let _ = s.read(&mut buf);
                n.fetch_add(1, Ordering::SeqCst);
                let _ = s.write_all(reply.as_bytes());
            }
        });
        (base, hits)
    }

    /// THE POINT: a catalog read whose host moved the endpoint behind a redirect is still read.
    #[test]
    fn get_json_follows_a_redirect_to_the_moved_endpoint() {
        let (target, target_hits) = serve(OK_JSON.to_string());
        let (base, _) = serve(redirect_to(&format!("{target}/moved")));
        let json = get_json(&format!("{base}/catalog")).expect("the redirect is followed");
        assert_eq!(json, serde_json::json!({"ok": true}));
        assert_eq!(target_hits.load(Ordering::SeqCst), 1, "the Location was read once");
    }

    /// …and the follow is BOUNDED: a loop fails after exactly the stated number of hops (the first
    /// request plus [`PUBLIC_GET_MAX_REDIRECTS`] followed redirects), in words, instead of ureq's ten.
    #[test]
    fn a_redirect_loop_stops_at_the_stated_limit() {
        let (base, hits) = serve_a_loop();
        let err = get_json(&format!("{base}/loop")).expect_err("a loop cannot be read");
        assert!(err.contains("too many redirects"), "{err}");
        assert_eq!(
            hits.load(Ordering::SeqCst) as u32,
            PUBLIC_GET_MAX_REDIRECTS + 1,
            "one request and {PUBLIC_GET_MAX_REDIRECTS} followed redirects"
        );
        let agent = public_get_agent();
        assert_eq!(agent.config().max_redirects(), PUBLIC_GET_MAX_REDIRECTS);
        assert_eq!(
            PUBLIC_GET_MAX_REDIRECTS, 5,
            "the figure the datahub and backfill agents state too"
        );
    }

    /// THE OTHER SIDE, which must not move: every venue builder follows none, so a signed query (or
    /// any request carrying a credential) is never replayed to a `Location`. The 302 is the answer and
    /// the target is never asked. Covers all four builders `venue_agent` makes.
    #[test]
    fn every_venue_builder_still_follows_no_redirect() {
        let (target, target_hits) = serve(OK_JSON.to_string());
        let (base, _) = serve(redirect_to(&format!("{target}/moved")));
        for (name, agent) in [
            ("blocking_agent", blocking_agent()),
            (
                "blocking_agent_with_timeout",
                blocking_agent_with_timeout(std::time::Duration::from_secs(5)),
            ),
            ("blocking_agent_for_lanes", blocking_agent_for_lanes(8)),
            ("blocking_agent_loopback_insecure", blocking_agent_loopback_insecure()),
        ] {
            assert_eq!(agent.config().max_redirects(), 0, "{name}");
            let resp = agent.get(&format!("{base}/x")).call().expect("the 302 is a response");
            assert_eq!(resp.status().as_u16(), 302, "{name}");
        }
        assert_eq!(target_hits.load(Ordering::SeqCst), 0, "no venue agent asked the Location");
    }
}
