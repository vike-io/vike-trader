//! The frame discipline the two ends of a `stream` socket share: a frame read and decoded under a
//! ceiling, a frame written under a ceiling (refused BEFORE its first byte when it is over), and the
//! one definition of a socket read timeout.
//!
//! The codec is `vike_hub_proto::frame`'s and is not repeated here. The ceilings are the hosting
//! protocol's to state (the gatewayhub's are `GW_UP_FRAME_CEILING_BYTES` and
//! `GW_DOWN_FRAME_CEILING_BYTES`) and arrive as arguments: a reader reads under the ceiling of the
//! direction it receives, a writer writes under the one its peer reads under, so a frame over it must
//! be an error HERE, before its first byte, and not a framing fault that kills the peer's session.
//! A server that must notice a peer going away waits for a frame to begin with [`wait_readable`].

use std::io::{self, Read, Write};
use std::net::TcpStream;
use std::time::Duration;

use serde::Serialize;
use serde::de::DeserializeOwned;
use vike_hub_proto::frame::{read_frame_raw_capped, write_frame};

/// `Read` one frame under `cap` and decode it, mapping a decode failure to `InvalidData`.
pub fn read_capped<R: Read, T: DeserializeOwned>(r: &mut R, cap: u32) -> io::Result<T> {
    let body = read_frame_raw_capped(r, cap)?;
    serde_json::from_slice(&body).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

/// Whether `e` is a socket read timeout, whatever the platform calls it (unix reports `WouldBlock`).
pub fn is_timeout(e: &io::Error) -> bool {
    matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut)
}

/// What [`wait_readable`] found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ready {
    /// At least one byte of a frame is waiting.
    Data,
    /// The peer closed its half.
    Eof,
    /// Nothing arrived within the wait.
    Timeout,
}

/// Wait up to `wait` (at least 1 ms) for a frame to BEGIN, without consuming a byte: a `peek`, so a
/// timeout cannot leave the stream half-read. An interrupted wait is a [`Ready::Timeout`], and the socket's
/// read timeout is left at the wait. The one wait both daemons' servers (the gatewayhub's connections and
/// stream feeds, the datahub's strategy sessions) watch a peer with: a read timeout that fired mid-frame
/// would lose the bytes already consumed and desync the stream.
pub fn wait_readable(stream: &TcpStream, wait: Duration) -> io::Result<Ready> {
    stream.set_read_timeout(Some(wait.max(Duration::from_millis(1))))?;
    let mut probe = [0u8; 1];
    match stream.peek(&mut probe) {
        Ok(0) => Ok(Ready::Eof),
        Ok(_) => Ok(Ready::Data),
        Err(e) if is_timeout(&e) || e.kind() == io::ErrorKind::Interrupted => Ok(Ready::Timeout),
        Err(e) => Err(e),
    }
}

/// [`write_frame`] for a CLIENT frame, refusing one over `ceiling` BEFORE its first byte: the daemon
/// reads client frames under that ceiling (the gatewayhub's `GW_UP_FRAME_CEILING_BYTES`) and treats a
/// longer one as a framing fault that closes the connection, so an over-long argument must be an error
/// here and not a lost session. `InvalidData`, like `write_frame`'s own pre-write refusals, means
/// nothing reached the wire.
pub fn write_up<W: Write>(w: &mut W, msg: &impl Serialize, ceiling: u32) -> io::Result<()> {
    let len =
        serde_json::to_vec(msg).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?.len();
    if len > ceiling as usize {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "a client frame of {len} bytes exceeds the {ceiling}-byte \
                 ceiling the gatewayhub reads client frames under; nothing was sent"
            ),
        ));
    }
    write_frame(w, msg)
}

/// Serialize `msg` into one length-prefixed frame, refusing a body over `ceiling` (`InvalidData`,
/// nothing written). The prefix and the body are one buffer, so the caller's single `write_all` is the
/// single write `vike_hub_proto::frame::write_frame` pins.
pub fn encode_capped(msg: &impl Serialize, ceiling: u32) -> io::Result<Vec<u8>> {
    let mut frame = Vec::with_capacity(128);
    encode_capped_into(&mut frame, msg, ceiling)?;
    Ok(frame)
}

/// [`encode_capped`] appended to `buf`, which may already hold earlier frames: the writer batches
/// several frames into one buffer and one `write_all`. On an error `buf` is exactly as it was (a
/// refused frame adds no byte), so the bytes of the frames before it are still a valid stream.
pub fn encode_capped_into(buf: &mut Vec<u8>, msg: &impl Serialize, ceiling: u32) -> io::Result<()> {
    let start = buf.len();
    buf.extend_from_slice(&[0u8; 4]);
    if let Err(e) = serde_json::to_writer(&mut *buf, msg) {
        buf.truncate(start);
        return Err(io::Error::new(io::ErrorKind::InvalidData, e));
    }
    let len = buf.len() - start - 4;
    if len > ceiling as usize {
        buf.truncate(start);
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "a frame body of {len} bytes exceeds the {ceiling}-byte ceiling clients read \
                 server frames under"
            ),
        ));
    }
    let Ok(prefix) = u32::try_from(len) else {
        buf.truncate(start);
        return Err(io::Error::new(io::ErrorKind::InvalidData, "frame body exceeds u32 length"));
    };
    buf[start..start + 4].copy_from_slice(&prefix.to_be_bytes());
    Ok(())
}

/// [`encode_capped`] and write it: ONE `write_all`, then a flush.
pub fn write_capped<W: Write>(w: &mut W, msg: &impl Serialize, ceiling: u32) -> io::Result<()> {
    let frame = encode_capped(msg, ceiling)?;
    w.write_all(&frame)?;
    w.flush()
}

#[cfg(test)]
#[path = "frame_tests.rs"]
mod frame_tests;
