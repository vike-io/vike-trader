//! Shared blocking-HTTP helpers for the venue REST clients.
//!
//! Every venue built the same ureq [`Agent`](ureq::Agent): 4xx/5xx returned as responses (not
//! errors) so callers parse each venue's own error body, plus a global timeout and the shared
//! user-agent. This collapses ~6 identical builders into one seam. (Polymarket keeps its own
//! proxy-aware builder — a genuine variant, not a copy.)
//!
//! On top of the builders sit the two GET shapes the venues had each copied:
//! - [`get_json`] — the keyless public read the five crypto **catalogs** duplicated verbatim
//!   (agent → status → body → 200-range gate → parse), returning `Err(String)` so bridge-core need
//!   not name vike-catalog's `CatalogError` (each catalog wraps it with a one-line `map_err`).
//! - [`get_raw`] + [`RawResp`] + [`body_head`] — the rate-limit-aware raw GET the four kline
//!   backfill modules (`data.rs`) duplicated: capture status + `Retry-After` (+ the Binance-family
//!   weight header) BEFORE draining the body, so the per-venue retry loops can classify and back
//!   off. The response *classification* stays per-venue (which statuses/business codes mean rate
//!   limited genuinely differ), and the PAGING loops stay per-venue too (forward vs backward). The
//!   backoff *cadence*, byte-identical across the venues, is now the shared
//!   [`crate::retry::retry_rate_limited`] driver (bybit/okx and the binance/aster family rung all
//!   route through it; the family's page `used_weight` rides out via the `Verdict::Done` payload).

use std::time::Duration;

/// Default global request timeout for venue REST calls.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

/// A blocking ureq agent with the vike venue defaults (30s global timeout).
pub fn blocking_agent() -> ureq::Agent {
    blocking_agent_with_timeout(DEFAULT_TIMEOUT)
}

/// A blocking ureq agent with an explicit global timeout (e.g. a short listen-key refresh).
pub fn blocking_agent_with_timeout(timeout: Duration) -> ureq::Agent {
    ureq::Agent::config_builder()
        // callers parse each venue's {code,msg} / errorMessage / errorCode out of 4xx bodies
        .http_status_as_error(false)
        .timeout_global(Some(timeout))
        .user_agent("vike-trader-rust")
        .build()
        .new_agent()
}

/// A blocking ureq agent sized for CONCURRENT use: [`blocking_agent`] plus room in the connection
/// pool for `lanes` simultaneous requests to the same host.
///
/// ureq 3.3 defaults to **3** idle connections per host (and 10 overall). A pager running more lanes
/// than that against one host finds no pooled connection for the surplus and pays a fresh DNS+TLS
/// handshake on every such request — the difference MEASURED on binance from the CI box is ~280 ms pooled
/// versus ~417 ms for a cold connection. That is not merely slower: it would distort the pacer's own
/// round-trip measurement, which is the number the lane count is DERIVED from
/// ([`crate::pacer::Pacer::suggested_lanes`]), so the concurrency would end up partly measuring its
/// own pool starvation.
///
/// `lanes` only ever WIDENS the pool (it is clamped up to the ureq defaults, never down). Everything
/// else — the 4xx/5xx-as-response policy, the 30 s global timeout, the user agent — is
/// [`blocking_agent`] verbatim, so a request issued through this agent is identical on the wire.
pub fn blocking_agent_for_lanes(lanes: usize) -> ureq::Agent {
    // ureq 3.3's own defaults, restated so "widen only" is provable rather than assumed.
    const DEFAULT_IDLE_PER_HOST: usize = 3;
    const DEFAULT_IDLE_TOTAL: usize = 10;
    let per_host = lanes.max(DEFAULT_IDLE_PER_HOST);
    ureq::Agent::config_builder()
        // callers parse each venue's {code,msg} / errorMessage / errorCode out of 4xx bodies
        .http_status_as_error(false)
        .timeout_global(Some(DEFAULT_TIMEOUT))
        .user_agent("vike-trader-rust")
        .max_idle_connections_per_host(per_host)
        .max_idle_connections(per_host.max(DEFAULT_IDLE_TOTAL))
        .build()
        .new_agent()
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
    ureq::Agent::config_builder()
        .http_status_as_error(false)
        .timeout_global(Some(DEFAULT_TIMEOUT))
        .user_agent("vike-trader-rust")
        .tls_config(tls)
        .build()
        .new_agent()
}

/// True when `url`'s host is a loopback address (`localhost`, `127.0.0.0/8`, or `::1`). The guard a
/// caller checks before trusting [`blocking_agent_loopback_insecure`] (which disables cert
/// verification) — a non-loopback URL must never be served by the insecure agent.
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

/// Blocking keyless `GET url` → parsed JSON, over a fresh [`blocking_agent`].
///
/// The collapsed body of the five crypto catalogs' byte-identical `fetch_json`: call, read the
/// status, drain the body, gate on the 200-range (error carries a [`body_head`] of the body), then
/// parse. Errors are the venues' existing per-stage strings — `"{url} GET: …"`, `"{url} read: …"`,
/// `"{url} HTTP {status}: {head}"`, `"{url} JSON: …"` — as a plain `String`, since bridge-core
/// cannot name vike-catalog's `CatalogError` without a new dep edge; each catalog keeps the
/// one-line `.map_err(CatalogError)`.
///
/// For an authed/signed read use the venue's own `rest`/transport client, not this.
pub fn get_json(url: &str) -> Result<serde_json::Value, String> {
    let agent = blocking_agent();
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

/// One blocking GET capturing status + the rate-limit headers before draining the body — the
/// `get_raw` the four kline-backfill modules copied.
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
