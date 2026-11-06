//! The socket under the connection: the EOF-flagging stream, the TLS wrap, the ERROR_RES decode.

use std::io::{self, Read, Write};
use std::net::TcpStream;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use prost::Message;

use super::ConnError;
use crate::proto::{ProtoMessage, ProtoOaErrorRes};

/// A `Read`/`Write` stream that flags EOF (a `read` returning `Ok(0)`) so an actor loop can
/// break instead of busy-spinning on a closed socket (the framing layer maps both EOF and a
/// read-timeout to `Ok(None)`, which are otherwise indistinguishable). Reused by the fake test
/// server. Read-timeouts surface as `WouldBlock`/`TimedOut` errors (not `Ok(0)`), so they do NOT
/// set the flag.
pub(crate) struct EofStream<S> {
    inner: S,
    pub(super) eof: Arc<AtomicBool>,
}

impl<S> EofStream<S> {
    pub(crate) fn new(inner: S) -> (Self, Arc<AtomicBool>) {
        let eof = Arc::new(AtomicBool::new(false));
        (Self { inner, eof: eof.clone() }, eof)
    }
}

impl<S: Read> Read for EofStream<S> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buf)?;
        if n == 0 {
            self.eof.store(true, Ordering::SeqCst);
        }
        Ok(n)
    }
}

impl<S: Write> Write for EofStream<S> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.inner.write(buf)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

/// The underlying socket: plaintext (tests) or rustls-wrapped (production).
pub(crate) enum Stream {
    Plain(TcpStream),
    Tls(Box<rustls::StreamOwned<rustls::ClientConnection, TcpStream>>),
}

impl Read for Stream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            Stream::Plain(s) => s.read(buf),
            Stream::Tls(s) => s.read(buf),
        }
    }
}

impl Write for Stream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self {
            Stream::Plain(s) => s.write(buf),
            Stream::Tls(s) => s.write(buf),
        }
    }
    fn flush(&mut self) -> io::Result<()> {
        match self {
            Stream::Plain(s) => s.flush(),
            Stream::Tls(s) => s.flush(),
        }
    }
}

/// Build a rustls client stream (ring provider, webpki-roots) — see resolution 3: the default
/// aws-lc provider is disabled, so an explicit provider is required.
pub(super) fn wrap_tls(host: &str, tcp: TcpStream) -> Result<Stream, ConnError> {
    let mut roots = rustls::RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let config = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .map_err(|e| ConnError::Tls(e.to_string()))?
    .with_root_certificates(roots)
    .with_no_client_auth();
    let server_name = rustls::pki_types::ServerName::try_from(host.to_owned())
        .map_err(|e| ConnError::Tls(e.to_string()))?;
    let conn = rustls::ClientConnection::new(Arc::new(config), server_name)
        .map_err(|e| ConnError::Tls(e.to_string()))?;
    Ok(Stream::Tls(Box::new(rustls::StreamOwned::new(conn, tcp))))
}

/// Decode an `ERROR_RES` frame into its code/description.
pub(super) fn decode_error(msg: &ProtoMessage) -> ConnError {
    match ProtoOaErrorRes::decode(msg.payload.as_deref().unwrap_or(&[])) {
        Ok(e) => ConnError::Venue {
            error_code: e.error_code,
            description: e.description.unwrap_or_default(),
        },
        Err(e) => ConnError::Decode(format!("error_res: {e}")),
    }
}
