//! The three rules over their inputs. The venue layer's wording of them (`code: 0` or the ambiguous sentinel, end to
//! end through a real agent) is `vike_bridge_core::transport`'s own suite.

use super::*;
use std::io::ErrorKind as K;

fn io(kind: K) -> ureq::Error {
    ureq::Error::Io(std::io::Error::new(kind, "x"))
}

fn tls(e: rustls::Error) -> ureq::Error {
    ureq::Error::Io(std::io::Error::new(K::InvalidData, e))
}

/// Every kind that can only happen before the request was written is definite (a submit may reject it: nothing
/// reached the venue); everything that can follow the write is not.
#[test]
fn only_a_failure_provably_before_the_write_is_definite() {
    let mut definite = vec![
        ureq::Error::HostNotFound,
        ureq::Error::ConnectionFailed,
        ureq::Error::InvalidProxyUrl,
        ureq::Error::ConnectProxyFailed("407".into()),
        ureq::Error::BadUri("x".into()),
        ureq::Error::TlsRequired,
        ureq::Error::RequireHttpsOnly("http://x".into()),
        ureq::Error::Http(ureq::http::Request::builder().uri("not a uri").body(()).unwrap_err()),
    ];
    for kind in
        [K::ConnectionRefused, K::AddrNotAvailable, K::NetworkUnreachable, K::HostUnreachable]
    {
        definite.push(io(kind));
    }
    for e in definite {
        assert!(send_error_is_definite(&e), "{e} never left this host");
    }
    let mut ambiguous = vec![
        ureq::Error::Timeout(ureq::Timeout::Global),
        ureq::Error::RedirectFailed,
        ureq::Error::LargeResponseHeader(9, 1),
        ureq::Error::TooManyRedirects,
        ureq::Error::BodyStalled,
        ureq::Error::BodyExceedsLimit(1),
    ];
    for kind in [
        K::UnexpectedEof,
        K::ConnectionReset,
        K::ConnectionAborted,
        K::BrokenPipe,
        K::TimedOut,
        K::Other,
        K::NotFound,
    ] {
        ambiguous.push(io(kind));
    }
    for e in ambiguous {
        assert!(!send_error_is_definite(&e), "{e} may have followed the write");
    }
}

/// The lazy rustls handshake surfaces as `Io(InvalidData)` wrapping a `rustls::Error`: only the
/// certificate / negotiation errors (nothing in plaintext was sent) are definite.
#[test]
fn a_tls_handshake_io_error_is_definite_only_for_certificate_and_negotiation() {
    use rustls::{CertificateError, Error as R, PeerIncompatible};
    for e in [
        tls(R::InvalidCertificate(CertificateError::UnknownIssuer)),
        tls(R::InvalidCertificate(CertificateError::Expired)),
        tls(R::PeerIncompatible(PeerIncompatible::Tls12NotOffered)),
        tls(R::NoCertificatesPresented),
    ] {
        assert!(send_error_is_definite(&e), "{e}");
    }
    for e in [
        tls(R::General("x".into())),
        tls(R::DecryptError),
        tls(R::AlertReceived(rustls::AlertDescription::InternalError)),
        io(K::InvalidData),
    ] {
        assert!(!send_error_is_definite(&e), "{e}");
    }
}

/// Gateway-style 5xx (Cloudflare's whole 520 to 530 too) say nothing about the order behind them; a 500 is not in
/// that set, it needs the body read, and any 5xx with no venue code leaves a write unknown.
#[test]
fn the_gateway_set_and_the_unknown_write() {
    for s in [502, 503, 504, 520, 521, 522, 523, 524, 525, 526, 527, 528, 529, 530] {
        assert!(status_is_gateway_ambiguous(s), "{s}");
    }
    for s in [200, 400, 401, 403, 429, 500, 501, 505, 531] {
        assert!(!status_is_gateway_ambiguous(s), "{s}");
    }
    for s in [500, 501, 505, 511, 520, 599] {
        assert!(server_error_leaves_write_unknown(s, None), "{s}");
        assert!(server_error_leaves_write_unknown(s, Some(0)), "{s}: code 0 is no verdict");
        assert!(!server_error_leaves_write_unknown(s, Some(10001)), "{s}");
    }
    for s in [200, 400, 429, 499, 600] {
        assert!(!server_error_leaves_write_unknown(s, None), "{s}");
    }
}
