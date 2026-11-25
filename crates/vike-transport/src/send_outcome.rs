//! **What a failed or 5xx-answered call says about whether the request took effect**: the three pure rules over a
//! ureq error and an HTTP status that every write path reads. A wrong answer either strands a phantom position (a
//! write called refused that landed) or double-submits an order (one called safe to retry that landed).
//!
//! These are the facts; the venue layer words them: `vike_bridge_core::transport::classify_send_error` turns
//! [`send_error_is_definite`] into the venue error's `code: 0` or its ambiguous sentinel, and
//! `vike_bridge_core::transport::reply`'s engine and `vike_bridge_core::transport::http_error` read the two status rules.

/// Did this `ureq` send failure PROVABLY happen before the request left this host? Only an
/// explicit allowlist of kinds is definite; everything else (a timeout, `Io` after the TCP
/// connect, `Protocol`, `LargeResponseHeader`, `RedirectFailed`, any future variant) may have
/// happened AFTER the body was written and is ambiguous.
///
/// Two ureq 3.5 facts shape the `Io` arm. (1) A DNS failure arrives as `Io(Uncategorized)`, not
/// `HostNotFound`; `crate::http`'s agents and finishers install a resolver that maps it to `HostNotFound`, so it
/// is definite HERE. (2) The rustls handshake completes inside the connect (since 3.4.1, before any
/// request byte), and a handshake failure arrives as `Io(InvalidData)` wrapping a `rustls::Error`:
/// only the certificate / negotiation errors are definite, by that downcast; every other `Io` (a
/// reset or `UnexpectedEof`, which may follow the POST) is ambiguous — the safe direction, the order
/// stays UNKNOWN and the confirm / watchdog resolve it, never a stranded real order. `PeerMisbehaved` is deliberately NOT listed: it
/// can also fire after the handshake. The ONE site for the rule.
pub fn send_error_is_definite(e: &ureq::Error) -> bool {
    use std::io::ErrorKind as K;
    match e {
        ureq::Error::Http(_)
        | ureq::Error::BadUri(_)
        | ureq::Error::HostNotFound
        | ureq::Error::InvalidProxyUrl
        | ureq::Error::ConnectionFailed
        | ureq::Error::ConnectProxyFailed(_)
        | ureq::Error::TlsRequired
        | ureq::Error::RequireHttpsOnly(_)
        | ureq::Error::Tls(_)
        | ureq::Error::Rustls(_) => true,
        ureq::Error::Io(io) => match io.kind() {
            // Connect-phase kinds only: the TCP/SOCKS connect never completed.
            K::ConnectionRefused
            | K::AddrNotAvailable
            | K::NetworkUnreachable
            | K::HostUnreachable => true,
            K::InvalidData => matches!(
                io.get_ref().and_then(|s| s.downcast_ref::<rustls::Error>()),
                Some(
                    rustls::Error::InvalidCertificate(_)
                        | rustls::Error::PeerIncompatible(_)
                        | rustls::Error::NoCertificatesPresented
                )
            ),
            _ => false,
        },
        _ => false,
    }
}

/// Gateway-style 5xx statuses (a proxy/CDN in front of the venue timing out or failing: 502, 503, 504
/// and Cloudflare's own 520 to 530) say nothing about whether the venue processed the request behind
/// them: the outcome is UNKNOWN. A transport that reads a response status maps these to the venue error's ambiguous
/// sentinel (`vike_bridge_core::transport::gateway_ambiguous`), never to the status-as-code a `ServerError`
/// retry/reject would follow. A transport that can read a WRITE's body as well asks
/// [`server_error_leaves_write_unknown`].
pub fn status_is_gateway_ambiguous(status: u16) -> bool {
    matches!(status, 502 | 503 | 504 | 520..=530)
}

/// Does this 5xx answer leave a WRITE's outcome unknown? Yes, unless the body carries the venue's own
/// non-zero code (`venue_code`, read by the transport from the venue's own field). A bare 500 is no
/// verdict: nothing says the venue did not act before it failed, so calling it a refusal rejects an
/// order that may be live and words an amend that may have landed as refused. The caller re-queries
/// once and never rejects. A read has no outcome to be unsure of, so it keeps
/// its status-as-code and does not ask.
pub fn server_error_leaves_write_unknown(status: u16, venue_code: Option<i64>) -> bool {
    (500..600).contains(&status) && venue_code.is_none_or(|code| code == 0)
}

#[cfg(test)]
#[path = "send_outcome_tests.rs"]
mod tests;
