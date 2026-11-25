//! The socket under the connection: the EOF-flagging stream, the TLS wrap, what a failed frame write left, the
//! ERROR_RES decode.

use std::io::{self, Read, Write};
use std::net::{Shutdown, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use prost::Message;
use vike_utility::errors::FailureOutcome;

use super::ConnectionError;
use crate::transport::proto::{ProtoMessage, ProtoOaErrorRes};

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
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buffer)?;
        if n == 0 {
            self.eof.store(true, Ordering::SeqCst);
        }
        Ok(n)
    }
}

impl<S: Write> Write for EofStream<S> {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.inner.write(buffer)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

/// The underlying socket: plaintext (tests) or rustls-wrapped (production), or none once [`EofStream::abort`] let it go.
pub(crate) enum ConnectionSocket {
    Plain(TcpStream),
    Tls(Box<vike_transport::tls::TlsStream>),
    Aborted,
}

fn aborted() -> io::Error {
    io::Error::new(io::ErrorKind::NotConnected, "the socket was aborted")
}

impl Read for ConnectionSocket {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        match self {
            ConnectionSocket::Plain(s) => s.read(buffer),
            ConnectionSocket::Tls(s) => s.read(buffer),
            ConnectionSocket::Aborted => Err(aborted()),
        }
    }
}

impl Write for ConnectionSocket {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        match self {
            ConnectionSocket::Plain(s) => s.write(buffer),
            ConnectionSocket::Tls(s) => s.write(buffer),
            ConnectionSocket::Aborted => Err(aborted()),
        }
    }
    fn flush(&mut self) -> io::Result<()> {
        match self {
            ConnectionSocket::Plain(s) => s.flush(),
            ConnectionSocket::Tls(s) => s.flush(),
            ConnectionSocket::Aborted => Err(aborted()),
        }
    }
}

impl EofStream<ConnectionSocket> {
    /// Close the connection under the stream, gracefully: what was written still goes out, then the FIN. What the actor
    /// does to a socket it leaves while the way to the venue works (an account session the venue ended leaves the socket
    /// up), so that it does not sit idle beside the new session, or for as long as a new one is refused.
    pub(super) fn close(&self) {
        if let Some(tcp) = self.tcp() {
            let _ = tcp.shutdown(Shutdown::Both);
        }
    }

    /// **Let go of a socket the actor called DEAD, and of every byte it still holds unsent.** A graceful close leaves the
    /// unsent bytes with the kernel, which sends them once the path heals, minutes later perhaps: an order written in the
    /// last seconds before the death would then reach the venue after the reconnect's reconcile, or the engine's
    /// stuck-order ladder, ended it by name. `SO_LINGER` 0 makes the close an abort (an RST, the send queue dropped), and
    /// the socket is closed HERE, not when the reconnect has found a new one: the old one would go on retransmitting
    /// through the whole outage. A frame the venue's side already received is the venue's; no abort recalls it.
    ///
    /// `std`'s `TcpStream::set_linger` is unstable (rust-lang/rust#88494), hence `socket2`. A socket whose linger cannot
    /// be set is said once and closed all the same, gracefully.
    pub(super) fn abort(&mut self) {
        if let Some(tcp) = self.tcp()
            && let Err(e) = socket2::SockRef::from(tcp).set_linger(Some(Duration::ZERO))
        {
            tracing::warn!(
                target: "ctrader",
                error = %e,
                "could not set SO_LINGER 0 on a dead socket: what it holds unsent may still go out when the path heals"
            );
        }
        self.inner = ConnectionSocket::Aborted;
    }

    /// How long a read of this socket blocks from now on.
    pub(super) fn set_read_timeout(&self, timeout: Duration) -> io::Result<()> {
        self.tcp().ok_or_else(aborted)?.set_read_timeout(Some(timeout))
    }

    fn tcp(&self) -> Option<&TcpStream> {
        match &self.inner {
            ConnectionSocket::Plain(tcp) => Some(tcp),
            ConnectionSocket::Tls(tls) => Some(tls.get_ref()),
            ConnectionSocket::Aborted => None,
        }
    }
}

/// Build a rustls client stream (ring provider, webpki-roots) — see resolution 3: the default
/// aws-lc provider is disabled, so an explicit provider is required. The config is the network layer's one
/// verified config (`vike_transport::tls::verified_tls_stream`); a failed step is a [`ConnectionError::Tls`].
pub(super) fn wrap_tls(host: &str, tcp: TcpStream) -> Result<ConnectionSocket, ConnectionError> {
    let tls = vike_transport::tls::verified_tls_stream(host, tcp).map_err(ConnectionError::Tls)?;
    Ok(ConnectionSocket::Tls(Box::new(tls)))
}

/// **A frame whose write failed, and what the venue may hold of it** ([`write_frame`]): one of the two outcomes a failed
/// write has (decision 0303), [`FailureOutcome::NotSent`] or [`FailureOutcome::OutcomeUnknown`].
#[derive(Debug)]
pub(super) struct FrameWriteFailure {
    pub(super) outcome: FailureOutcome,
    pub(super) error: io::Error,
}

impl FrameWriteFailure {
    fn of(error: io::Error, handed_over: bool) -> Self {
        // The socket blocks and has no send timeout, so these are what a dead connection answers: it carries no byte more.
        let gone = matches!(
            error.kind(),
            io::ErrorKind::ConnectionReset
                | io::ErrorKind::ConnectionAborted
                | io::ErrorKind::BrokenPipe
                | io::ErrorKind::NotConnected
        );
        let outcome = if handed_over && !gone {
            FailureOutcome::OutcomeUnknown
        } else {
            FailureOutcome::NotSent
        };
        Self { outcome, error }
    }
}

/// **Write `frame` whole and flush it; on a failure, say what the venue may hold of it**, by the step that failed and
/// the error's kind, never its text (decision 0301):
/// - **NOT SENT** when no byte of the frame was handed to the stream (rustls refuses a write before it takes a byte of
///   it when what it already holds cannot go out), or when the connection is gone (reset, aborted, broken pipe, not
///   connected): the venue then holds a cut frame or a TLS record cut short at most, and reads neither.
/// - **OUTCOME UNKNOWN** otherwise (an interrupted or timed-out send, any other kind). rustls's `write` takes the whole
///   frame and drops the socket's error, its `flush` reports it, and the rest of the record stays queued: the next read
///   or write of this stream sends it first (`rustls::Stream::read`), and the actor reads what is left before it drops
///   a socket. The venue may read the frame whole.
pub(super) fn write_frame(stream: &mut impl Write, frame: &[u8]) -> Result<(), FrameWriteFailure> {
    let mut handed = 0;
    while handed < frame.len() {
        match stream.write(&frame[handed..]) {
            Ok(0) => {
                return Err(FrameWriteFailure::of(io::ErrorKind::WriteZero.into(), handed > 0));
            }
            Ok(n) => handed += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(FrameWriteFailure::of(e, handed > 0)),
        }
    }
    stream.flush().map_err(|e| FrameWriteFailure::of(e, true))
}

/// Decode an `ERROR_RES` frame into its code/description.
pub(super) fn decode_error(msg: &ProtoMessage) -> ConnectionError {
    match ProtoOaErrorRes::decode(msg.payload.as_deref().unwrap_or(&[])) {
        Ok(e) => ConnectionError::Venue {
            error_code: e.error_code,
            description: e.description.unwrap_or_default(),
        },
        Err(e) => ConnectionError::Decode(format!("error_res: {e}")),
    }
}
