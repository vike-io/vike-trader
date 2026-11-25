//! **The rustls client configs a raw TLS socket is wrapped with**: the verified one (ring provider, the webpki roots,
//! the safe default protocol versions) and the loopback-only one that verifies nothing.
//!
//! ureq and tungstenite build their own configs from the same pieces; these are for a socket a crate opens itself:
//! cTrader's protobuf-over-TLS connection (`crates/bridges/ctrader/src/transport/connection/stream.rs`'s `wrap_tls`),
//! the gatewayhub's base-host probe (`crates/vike-gatewayhub/src/url_probe.rs`'s `handshake`) and the IBKR Client
//! Portal's loopback WebSocket (`crates/bridges/vike-ibkr/src/transport/cpapi/mod.rs`'s `connect_websocket`). The
//! aws-lc provider rustls defaults to is off workspace-wide (the root manifest's `rustls` note), so the provider is
//! named explicitly.

use std::net::TcpStream;
use std::sync::Arc;

/// A verified TLS client stream over a TCP socket: what [`verified_tls_stream`] wraps a dialed socket in.
/// `conn` drives the handshake (`complete_io` over `sock`), and the stream reads and writes plaintext.
pub type TlsStream = rustls::StreamOwned<rustls::ClientConnection, TcpStream>;

/// The verified client config: ring, the webpki roots, the safe default protocol versions, no client auth.
pub fn verified_client_config() -> Result<rustls::ClientConfig, rustls::Error> {
    let mut roots = rustls::RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    Ok(rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()?
    .with_root_certificates(roots)
    .with_no_client_auth())
}

/// `tcp` wrapped for `host` under [`verified_client_config`]. Nothing is sent yet: the handshake runs inside the
/// first read or write (or an explicit `conn.complete_io(&mut sock)`). The error is the failed step's own words (the
/// config, the server name, the connection).
pub fn verified_tls_stream(host: &str, tcp: TcpStream) -> Result<TlsStream, String> {
    let config = verified_client_config().map_err(|e| e.to_string())?;
    let server_name =
        rustls::pki_types::ServerName::try_from(host.to_owned()).map_err(|e| e.to_string())?;
    let conn =
        rustls::ClientConnection::new(Arc::new(config), server_name).map_err(|e| e.to_string())?;
    Ok(rustls::StreamOwned::new(conn, tcp))
}

/// A client config (ring provider) that accepts ANY server cert. ONLY ever handed to a loopback connection (the
/// caller gates it with `vike_utility::url::is_loopback_url`): the IBKR Client Portal Gateway's self-signed loopback
/// cert, the WebSocket twin of `crate::http::blocking_agent_loopback_insecure`. NEVER use this for a non-loopback host.
pub fn loopback_insecure_client_config() -> rustls::ClientConfig {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    rustls::ClientConfig::builder_with_provider(provider.clone())
        .with_safe_default_protocol_versions()
        .expect("ring provider supports the default protocol versions")
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(NoVerify(provider)))
        .with_no_client_auth()
}

/// Certificate verifier that trusts everything. Sound ONLY behind a caller's loopback gate.
#[derive(Debug)]
struct NoVerify(Arc<rustls::crypto::CryptoProvider>);

impl rustls::client::danger::ServerCertVerifier for NoVerify {
    fn verify_server_cert(
        &self,
        _end_entity: &rustls::pki_types::CertificateDer<'_>,
        _intermediates: &[rustls::pki_types::CertificateDer<'_>],
        _server_name: &rustls::pki_types::ServerName<'_>,
        _ocsp_response: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &rustls::pki_types::CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &rustls::pki_types::CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}
