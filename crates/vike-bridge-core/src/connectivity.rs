//! `ConnectivityProbe` — an opt-in, independent connectivity probe that classifies a venue outage
//! as [`OutageClass::LocalNetworkDown`] vs [`OutageClass::VenueDown`] (net-hardening, audit br8).
//!
//! The reconnect/health loops in this crate ([`crate::depth`], [`crate::user_data`], the venue
//! feeds) treat every disconnect identically: they back off and reconnect. That hides the ONE
//! distinction an operator most wants when a socket drops — is it OUR side (this box's internet, or
//! the user's Dublin EU tunnel that live Polymarket routes through) or is it the VENUE? It matters
//! most for the all-venues-die-at-once case, where a single local-network / tunnel fault masquerades
//! as N simultaneous venue outages.
//!
//! This module answers that with a cheap, neutral, third-party probe: when a venue connection is
//! judged down, probe one or more NEUTRAL endpoints (deliberately NOT any venue — a target whose
//! reachability is independent of any single venue's health) and fold the result:
//!
//! - EVERY neutral endpoint unreachable → [`OutageClass::LocalNetworkDown`] (our internet/tunnel is
//!   down; the venue itself may be perfectly healthy).
//! - AT LEAST ONE neutral endpoint reachable → [`OutageClass::VenueDown`] (our network is up, so the
//!   venue-specific outage is the venue's — or a venue-specific route/geo/tunnel — not general local
//!   connectivity).
//!
//! **OFF by default.** Nothing here runs unless a caller (a) constructs a [`ConnectivityProbe`] with
//! ≥1 configured endpoint AND (b) explicitly calls [`ConnectivityProbe::classify`] from its
//! reconnect/health thread when a venue is already judged down. There is no timer, no background
//! thread, and no probing on a healthy connection — the probe exists purely to disambiguate a
//! disconnect that ALREADY happened.
//!
//! **Off any hot path.** A probe is a blocking TCP connect (the venue reconnect thread is where it
//! belongs — never the vike-core fold). It is invoked at most once per outage, at the point the
//! reconnect loop opens a transport gap.
//!
//! **No single point of failure.** More than one neutral endpoint is allowed (three independent DNS
//! operators by default, see [`DEFAULT_NEUTRAL_ENDPOINTS`]); "local down" is disclosed ONLY when
//! ALL of them fail, so one neutral host being down (or blocking us) can never on its own be
//! mistaken for our whole network being down.
//!
//! **Dependency-light & `vike-data`-free.** The default real probe is a plain `std::net` TCP connect
//! with a bounded `connect_timeout` — zero new dependencies and no TLS at all (so "rustls only, no
//! OpenSSL" is trivially satisfied: no OpenSSL is reachable from here). A caller that prefers an
//! HTTP HEAD over the crate's existing blocking [`ureq`](crate::http) agent — or any other reachability
//! test — supplies it through the [`ConnectivityProbe::classify_with`] injectable seam, which is
//! also exactly what the unit tests drive with a mock (so classification is proven with zero real
//! network). Like [`crate::stream_health`], this stays `vike-data`-free and emits a neutral type; a
//! producer can fold the verdict into a `tracing` line ([`ConnectivityProbe::classify_and_log`]) or,
//! at its own `LiveDataSink` boundary, into a `vike_data::StreamStatus` reason next to the
//! `HealthEvent::Gap` it already discloses — additively, with no serde/Event wire-schema change.

use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

/// Neutral, venue-independent probe targets (host:port). Three INDEPENDENT operators — Cloudflare,
/// Google, Quad9 — so no one of them is a single point of failure (a "local down" verdict needs all
/// three to fail). Literal IPs on purpose: `to_socket_addrs` on a literal IP does no DNS lookup, so
/// a probe never blocks on (possibly-also-broken) name resolution and its latency is bounded by the
/// caller's `connect_timeout` alone. `443`/`53` are ports these hosts actually answer on, so a
/// successful TCP handshake genuinely proves reachability rather than a black-hole accept.
pub const DEFAULT_NEUTRAL_ENDPOINTS: &[&str] = &[
    "1.1.1.1:443", // Cloudflare DNS-over-HTTPS
    "8.8.8.8:53",  // Google Public DNS
    "9.9.9.9:443", // Quad9 DNS-over-HTTPS
];

/// Default per-endpoint TCP connect timeout. Generous enough to ride out a slow-but-alive link, short
/// enough that classifying an outage over three endpoints stays well under a reconnect backoff.
pub const DEFAULT_PROBE_TIMEOUT: Duration = Duration::from_secs(3);

/// The verdict from probing the neutral endpoints AFTER a venue connection was judged down. The
/// caller supplies the "venue is down" precondition (it only probes because a socket dropped); this
/// enum discloses the CAUSE. Neutral (no `vike-data` dependency) so `vike-bridge-core` keeps its
/// layering; a venue can map it onto a `vike_data::StreamStatus` reason at its own sink boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutageClass {
    /// EVERY neutral endpoint was unreachable → this box's own network / tunnel is down (e.g. the
    /// Dublin EU tunnel live Polymarket routes through). The venue may be fine; the fault is local.
    /// This is the all-venues-die-at-once signature — one local fault, disclosed as one local cause.
    LocalNetworkDown,
    /// At least one neutral endpoint answered → our network is up, so the venue-specific outage is
    /// the venue's own (or a venue-specific route/geo block), NOT general local connectivity.
    VenueDown,
}

impl OutageClass {
    /// Whether this is the local-network/tunnel fault (all neutral probes failed).
    #[must_use]
    pub fn is_local_network_down(self) -> bool {
        matches!(self, OutageClass::LocalNetworkDown)
    }

    /// Whether the outage was classified as venue-side (a neutral endpoint was reachable).
    #[must_use]
    pub fn is_venue_down(self) -> bool {
        matches!(self, OutageClass::VenueDown)
    }

    /// A short, stable human reason — for a log line or a future `StreamStatus` reason string.
    #[must_use]
    pub fn reason(self) -> &'static str {
        match self {
            OutageClass::LocalNetworkDown => {
                "local network/tunnel down — all neutral endpoints unreachable"
            }
            OutageClass::VenueDown => "local network OK — outage is venue-side",
        }
    }
}

/// An opt-in connectivity probe over a fixed set of neutral endpoints. Cheap to hold (a `Vec` of
/// endpoints + a timeout); construct it ONCE per venue feed and call [`Self::classify`] from the
/// reconnect thread when that feed's connection is judged down. Does nothing on its own.
#[derive(Debug, Clone)]
pub struct ConnectivityProbe {
    /// Neutral host:port targets. Guaranteed non-empty (both constructors enforce it), so a
    /// classification always probes at least one endpoint — an empty list would fold to a false
    /// `LocalNetworkDown` (nothing to prove reachability), which is why it is rejected up front.
    endpoints: Vec<String>,
    /// Per-endpoint TCP connect timeout used by the DEFAULT real probe ([`Self::classify`]); the
    /// injected-seam [`Self::classify_with`] ignores it (the mock/HTTP probe carries its own).
    timeout: Duration,
}

impl ConnectivityProbe {
    /// Build a probe over `endpoints` (host:port each) with a per-endpoint connect `timeout`.
    /// Returns `None` if `endpoints` is empty — an empty probe can never distinguish local-vs-venue
    /// (it would always cry `LocalNetworkDown`), so "no endpoints configured" is "no probe", which
    /// is also how the feature stays OFF by default (a caller that configures nothing gets nothing).
    #[must_use]
    pub fn new(endpoints: Vec<String>, timeout: Duration) -> Option<Self> {
        if endpoints.is_empty() {
            return None;
        }
        Some(ConnectivityProbe { endpoints, timeout })
    }

    /// A probe over the three [`DEFAULT_NEUTRAL_ENDPOINTS`] with the [`DEFAULT_PROBE_TIMEOUT`] — the
    /// zero-config opt-in. Always valid (the defaults are non-empty).
    #[must_use]
    pub fn with_default_endpoints() -> Self {
        ConnectivityProbe {
            endpoints: DEFAULT_NEUTRAL_ENDPOINTS.iter().map(|s| (*s).to_string()).collect(),
            timeout: DEFAULT_PROBE_TIMEOUT,
        }
    }

    /// The configured neutral endpoints (host:port).
    #[must_use]
    pub fn endpoints(&self) -> &[String] {
        &self.endpoints
    }

    /// Classify the outage using an INJECTED reachability probe — the test/customization seam.
    /// `probe(endpoint) -> true` means that endpoint is reachable. Folds the endpoints with the one
    /// rule the module exists to enforce: [`OutageClass::LocalNetworkDown`] ONLY when ALL neutral
    /// probes fail; any single reachable endpoint ⇒ [`OutageClass::VenueDown`]. Short-circuits on the
    /// first reachable endpoint (the common "our network is fine" case costs one probe), and probes
    /// EVERY endpoint before disclosing local-down (so one dead neutral host can't trigger it alone).
    ///
    /// Unit tests drive this with a deterministic mock (`|_| false` / `|_| true`); production wires
    /// [`Self::classify`], which passes the real TCP probe. A caller wanting an HTTP HEAD over the
    /// crate's [`ureq`](crate::http) agent instead simply passes that closure here.
    #[must_use]
    pub fn classify_with(&self, mut probe: impl FnMut(&str) -> bool) -> OutageClass {
        debug_assert!(!self.endpoints.is_empty(), "a ConnectivityProbe is always non-empty");
        // `any` short-circuits on the first `true`, and returns `false` for an (impossible) empty
        // list — the non-empty invariant above is what makes the `false` case genuinely "all failed".
        if self.endpoints.iter().any(|ep| probe(ep.as_str())) {
            OutageClass::VenueDown
        } else {
            OutageClass::LocalNetworkDown
        }
    }

    /// Classify using the DEFAULT real probe: a blocking `std::net` TCP connect to each neutral
    /// endpoint, bounded by the configured `timeout`. No TLS, no new dependencies. MUST be called
    /// off the hot path (the reconnect/health thread), never the vike-core fold.
    #[must_use]
    pub fn classify(&self) -> OutageClass {
        let timeout = self.timeout;
        self.classify_with(|ep| tcp_reachable(ep, timeout))
    }

    /// [`Self::classify`], then disclose the verdict on a single `tracing` line stamped with `venue`
    /// (the surfacing a reconnect thread wants in one call). Returns the verdict so the caller can
    /// also act on it (e.g. fold it into a `StreamStatus` reason). Off the hot path, like `classify`.
    pub fn classify_and_log(&self, venue: &str) -> OutageClass {
        let verdict = self.classify();
        let reason = verdict.reason();
        let neutral_endpoints = self.endpoints.len();
        match verdict {
            // A local/tunnel fault is the more actionable, louder case (it can mask N venue outages).
            OutageClass::LocalNetworkDown => {
                tracing::warn!(venue, neutral_endpoints, "connectivity probe: {reason}");
            }
            OutageClass::VenueDown => {
                tracing::info!(venue, neutral_endpoints, "connectivity probe: {reason}");
            }
        }
        verdict
    }
}

/// The default real reachability test: is `endpoint` (host:port) reachable by a bounded blocking TCP
/// connect? `true` iff SOME resolved address accepts a connection within `timeout`. A resolution
/// failure (DNS down / bad host) is treated as unreachable — itself a local-network symptom. Public
/// so a caller can reuse it directly or wrap it; used by [`ConnectivityProbe::classify`].
///
/// Note: `timeout` bounds the CONNECT per address. When `endpoint` is a literal IP (the defaults),
/// `to_socket_addrs` does no DNS lookup, so the whole call is bounded by `timeout`; a hostname adds
/// an unbounded resolution step, which is why the defaults are literal IPs.
#[must_use]
pub fn tcp_reachable(endpoint: &str, timeout: Duration) -> bool {
    match endpoint.to_socket_addrs() {
        Ok(addrs) => {
            for addr in addrs {
                if TcpStream::connect_timeout(&addr, timeout).is_ok() {
                    return true;
                }
            }
            false
        }
        // Couldn't even resolve — treat as unreachable (a broken resolver is a local fault).
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn probe(endpoints: &[&str]) -> ConnectivityProbe {
        ConnectivityProbe::new(
            endpoints.iter().map(|s| (*s).to_string()).collect(),
            DEFAULT_PROBE_TIMEOUT,
        )
        .expect("non-empty endpoint list")
    }

    // ---- the two required classifications, via the injected mock (no real network) --------------

    /// ALL neutral probes fail → our own network/tunnel is down.
    #[test]
    fn all_neutral_probes_failing_means_local_network_down() {
        let p = probe(&["1.1.1.1:443", "8.8.8.8:53", "9.9.9.9:443"]);
        assert_eq!(p.classify_with(|_ep| false), OutageClass::LocalNetworkDown);
    }

    /// A reachable neutral endpoint (our network is up) + a venue that dropped → the venue is down.
    #[test]
    fn a_reachable_neutral_endpoint_means_venue_down() {
        let p = probe(&["1.1.1.1:443", "8.8.8.8:53", "9.9.9.9:443"]);
        assert_eq!(p.classify_with(|_ep| true), OutageClass::VenueDown);
    }

    // ---- the fold rules: "local down" needs ALL to fail; any one reachable ⇒ venue down ----------

    /// Exactly one neutral endpoint reachable (and NOT the first) still means VenueDown — local-down
    /// requires EVERY neutral probe to fail, so a lone survivor is enough to exonerate our network.
    #[test]
    fn a_single_reachable_endpoint_among_failures_means_venue_down() {
        let p = probe(&["a:1", "b:2", "c:3"]);
        assert_eq!(p.classify_with(|ep| ep == "b:2"), OutageClass::VenueDown);
    }

    /// A local-down verdict must probe EVERY endpoint (one dead neutral host can't trigger it alone).
    #[test]
    fn local_down_requires_every_neutral_endpoint_to_be_probed() {
        let p = probe(&["a:1", "b:2", "c:3"]);
        let mut probed = 0usize;
        let verdict = p.classify_with(|_ep| {
            probed += 1;
            false
        });
        assert_eq!(verdict, OutageClass::LocalNetworkDown);
        assert_eq!(
            probed, 3,
            "all three neutral endpoints were probed before disclosing local-down"
        );
    }

    /// The common "our network is fine" path short-circuits on the FIRST reachable endpoint — it does
    /// not needlessly probe the rest.
    #[test]
    fn venue_down_short_circuits_on_the_first_reachable_endpoint() {
        let p = probe(&["a:1", "b:2", "c:3"]);
        let mut probed = 0usize;
        let verdict = p.classify_with(|_ep| {
            probed += 1;
            true // the first endpoint is reachable
        });
        assert_eq!(verdict, OutageClass::VenueDown);
        assert_eq!(probed, 1, "any() stops at the first reachable endpoint");
    }

    // ---- construction: opt-in / non-empty invariant ---------------------------------------------

    /// An empty endpoint list is rejected — "no endpoints configured" is "no probe" (the OFF default),
    /// not a probe that would always falsely cry local-down.
    #[test]
    fn new_rejects_an_empty_endpoint_list() {
        assert!(ConnectivityProbe::new(vec![], DEFAULT_PROBE_TIMEOUT).is_none());
    }

    /// A configured list yields a probe over exactly those endpoints.
    #[test]
    fn new_accepts_a_configured_endpoint_list() {
        let p = ConnectivityProbe::new(vec!["host:9000".to_string()], DEFAULT_PROBE_TIMEOUT)
            .expect("one endpoint is enough");
        assert_eq!(p.endpoints(), ["host:9000".to_string()]);
    }

    /// The zero-config default has MORE THAN ONE neutral endpoint (no single point of failure) and is
    /// always valid.
    #[test]
    fn default_endpoints_have_no_single_point_of_failure() {
        let p = ConnectivityProbe::with_default_endpoints();
        assert!(p.endpoints().len() > 1, "must allow >1 neutral endpoint");
        assert_eq!(p.endpoints().len(), DEFAULT_NEUTRAL_ENDPOINTS.len());
    }

    // ---- verdict predicates / reason ------------------------------------------------------------

    #[test]
    fn outage_class_predicates_and_reason() {
        assert!(OutageClass::LocalNetworkDown.is_local_network_down());
        assert!(!OutageClass::LocalNetworkDown.is_venue_down());
        assert!(OutageClass::VenueDown.is_venue_down());
        assert!(!OutageClass::VenueDown.is_local_network_down());
        assert!(OutageClass::LocalNetworkDown.reason().contains("local network"));
        assert!(OutageClass::VenueDown.reason().contains("venue"));
    }

    // ---- the REAL TCP probe path, still deterministic & offline (closed loopback port) ----------
    //
    // Port 1 on loopback is not listening on a dev box or CI (the same assumption the transport-layer
    // `rate_gate_wiring` tests already lean on with `http://127.0.0.1:1`): connection-refused returns
    // immediately, so these need no external network and never wait out the timeout.

    /// `tcp_reachable` is `false` for a closed loopback port (connection refused), deterministically.
    #[test]
    fn tcp_reachable_is_false_for_a_closed_loopback_port() {
        assert!(!tcp_reachable("127.0.0.1:1", Duration::from_millis(200)));
    }

    /// `classify` runs the REAL probe end-to-end: a probe whose only endpoints are closed loopback
    /// ports folds all-fail → LocalNetworkDown. (Contrived endpoints — this proves `classify` wires
    /// `tcp_reachable` through the same all-fail fold as the mock, offline and deterministically.)
    #[test]
    fn classify_folds_the_real_probe_over_closed_ports_to_local_down() {
        let p = ConnectivityProbe::new(
            vec!["127.0.0.1:1".to_string(), "127.0.0.1:2".to_string()],
            Duration::from_millis(200),
        )
        .expect("two endpoints");
        assert_eq!(p.classify(), OutageClass::LocalNetworkDown);
    }
}
