//! The refusals this daemon raises and records itself, declared once (`vike_utility::refusal_enum!`, decision 0301).
//! The ones that cross its wire are `vike_gatewayhub_client::refusals`'.
//!
//! [`UrlProbeRefusal`]: what the venue-address probe (`url_probe`'s module doc) found at a row that did not answer,
//! recorded beside the row (`vike_secrets::venue_url::UrlProbe::words`) and printed by `vike-cli config url probes`
//! and `trade health`. The column holds the BARE text: those readers tell "refused for now" from "down" by the words'
//! `vike_secrets::venue_url::PROBE_REFUSED_FOR_NOW` start (`crates/vike-cli/src/cmd/config/url.rs`'s
//! `refused_for_now`), so the id is published in `refusals.json` and shown once the row carries it in a column of
//! its own.

vike_utility::refusal_enum! {
    /// A venue address that did not answer the probe. The `rate-limited or temporarily refused` ones are a `429` or
    /// a timeout (`vike_secrets::venue_url::PROBE_REFUSED_FOR_NOW`): a venue pacing this box, not a dead endpoint.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum UrlProbeRefusal with ids UrlProbeRefusalId {
        /// A socket row whose `host:port` does not split.
        NoAddress { error: String } => "gatewayhub.url_probe.no_address": "no address: {error}",
        /// A resource answered a status that is not 2xx or 3xx.
        Status { status: u16 } => "gatewayhub.url_probe.status": "{status}",
        /// A resource answered `429`.
        RateLimited { status: u16 } => "gatewayhub.url_probe.rate_limited":
            "rate-limited or temporarily refused: {status}",
        /// A resource's GET timed out.
        NoAnswerInTime { error: String } => "gatewayhub.url_probe.no_answer_in_time":
            "rate-limited or temporarily refused: no answer: {error}",
        /// A resource's GET failed otherwise.
        NoAnswer { error: String } => "gatewayhub.url_probe.no_answer": "no answer: {error}",
        /// A rest, stream or ws row that is not `scheme://host[:port][/base]` of a known scheme.
        NotABaseAddress { url: String } => "gatewayhub.url_probe.not_a_base_address":
            "not a base address: {url}",
        /// The lookup or the TCP connect ran out of time.
        ConnectTimedOut { error: String } => "gatewayhub.url_probe.connect_timed_out":
            "rate-limited or temporarily refused: connect timed out ({error})",
        /// The TCP connect failed (refused, no route, a name that does not resolve).
        ConnectFailed { error: String } => "gatewayhub.url_probe.connect_failed": "connect failed: {error}",
        /// The verified TLS handshake failed (a bad certificate, a server that stopped answering).
        TlsFailed { error: String } => "gatewayhub.url_probe.tls_failed": "TLS handshake failed: {error}",
        /// The TLS handshake got no answer in time.
        NoTlsAnswer => "gatewayhub.url_probe.no_tls_answer":
            "rate-limited or temporarily refused: no TLS answer: timeout",
    }
}

#[cfg(test)]
#[path = "refusals_tests.rs"]
mod tests;
