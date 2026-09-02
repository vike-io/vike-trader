//! 4-byte big-endian length-prefixed ProtoMessage framing over any Read/Write.
//! Ports nothing — the cTrader Open API TCP wire format (port 5035).
use std::io::{self, Read};

use prost::Message;

use crate::proto::ProtoMessage;

/// Encode one length-prefixed frame: [u32 BE len][ProtoMessage].
pub fn encode(payload_type: u32, body: &[u8], client_msg_id: &str) -> Vec<u8> {
    let msg = ProtoMessage {
        payload_type,
        payload: Some(body.to_vec()),
        client_msg_id: Some(client_msg_id.to_string()),
    };
    let encoded = msg.encode_to_vec();
    let mut out = Vec::with_capacity(4 + encoded.len());
    out.extend_from_slice(&(encoded.len() as u32).to_be_bytes());
    out.extend_from_slice(&encoded);
    out
}

/// Buffered frame reader. `next_frame` returns Ok(None) when the underlying reader
/// yields no/partial data (e.g. a socket read timeout) — caller loops.
pub struct FrameReader<S: Read> {
    inner: S,
    buf: Vec<u8>,
}

impl<S: Read> FrameReader<S> {
    pub fn new(inner: S) -> Self {
        Self { inner, buf: Vec::new() }
    }

    /// Mutable access to the underlying stream — the connection actor reads frames via
    /// `next_frame` and writes command/heartbeat frames back through this same handle (one
    /// thread owns the stream, so no split/clone is needed).
    pub fn get_mut(&mut self) -> &mut S {
        &mut self.inner
    }

    pub fn next_frame(&mut self) -> io::Result<Option<ProtoMessage>> {
        // Already have a full frame buffered from a previous read — no syscall needed.
        if let Some(msg) = self.try_take_frame()? {
            return Ok(Some(msg));
        }

        // Attempt exactly one read. A socket with a short read timeout may deliver a
        // partial frame (or nothing) per call; the caller is expected to loop. Looping
        // internally here (rather than doing one read then returning) would defeat that
        // contract — a `Read` that hands back chunks one call at a time (e.g. a real
        // socket under load, or `ChunkReader` in tests) must be allowed to report "not
        // enough yet" after a single short read.
        let mut tmp = [0u8; 8192];
        match self.inner.read(&mut tmp) {
            Ok(0) => return Ok(None), // EOF/no more (cursor) — caller decides
            Ok(n) => self.buf.extend_from_slice(&tmp[..n]),
            Err(ref e)
                if e.kind() == io::ErrorKind::WouldBlock || e.kind() == io::ErrorKind::TimedOut =>
            {
                return Ok(None)
            }
            Err(e) => return Err(e),
        }

        self.try_take_frame()
    }

    /// Returns `Some(msg)` and drains the frame from `buf` if a full length-prefixed
    /// frame is already buffered; `None` if more bytes are needed. No I/O.
    fn try_take_frame(&mut self) -> io::Result<Option<ProtoMessage>> {
        if self.buf.len() < 4 {
            return Ok(None);
        }
        let len = u32::from_be_bytes(self.buf[..4].try_into().unwrap()) as usize;
        if self.buf.len() < 4 + len {
            return Ok(None);
        }
        let body = self.buf[4..4 + len].to_vec();
        self.buf.drain(..4 + len);
        let msg = ProtoMessage::decode(&body[..])
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        Ok(Some(msg))
    }
}
