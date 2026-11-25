//! The three readings of a URL string the network layers share, std only: its authority, whether its host is a
//! loopback address (the guard in front of every certificate-check bypass) and the WebSocket url of a path on an HTTP
//! base's host. Pure text; nothing here parses a full URI or opens anything.

/// `url`'s authority, `host[:port]`: what follows the scheme's `://` (the whole `url` when it has
/// none) up to the first `/`. `https://127.0.0.1:5000/v1/api` → `127.0.0.1:5000`.
#[must_use]
pub fn url_authority(url: &str) -> &str {
    let after = url.split("://").nth(1).unwrap_or(url);
    after.split('/').next().unwrap_or("")
}

/// True when `url`'s host is a loopback address (`localhost`, `127.0.0.0/8`, or `::1`) and its authority
/// has no `user@` part. The guard in front of plain `http://`/`ws://` venue rows
/// (`vike_venues::venue_urls::check_url`) and the guard a
/// caller checks before trusting `vike_transport::http::blocking_agent_loopback_insecure` or
/// `vike_transport::tls::loopback_insecure_client_config` (which disable cert verification) — a non-loopback URL must
/// never be served by either.
pub fn is_loopback_url(url: &str) -> bool {
    let hostport = url_authority(url);
    // A `user@` part puts the real host after the `@`: `https://127.0.0.1:1@evil.com` dials `evil.com`.
    if hostport.contains('@') {
        return false;
    }
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

/// The WebSocket url of `path` on the host of `base`, an HTTP base: `http://` becomes `ws://`, any
/// other scheme (or none) `wss://`, and a trailing `/` of `base` is dropped before `path` is added.
/// `https://127.0.0.1:5000` and `/v1/api/ws` give `wss://127.0.0.1:5000/v1/api/ws`.
#[must_use]
pub fn websocket_url_from_http(base: &str, path: &str) -> String {
    let base = base.trim_end_matches('/');
    let (scheme, rest) = match base.split_once("://") {
        Some(("http", rest)) => ("ws", rest),
        Some((_, rest)) => ("wss", rest),
        None => ("wss", base),
    };
    format!("{scheme}://{rest}{path}")
}

#[cfg(test)]
#[path = "url_tests.rs"]
mod tests;
