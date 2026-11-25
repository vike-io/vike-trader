//! The frame discipline the two ends of a `stream` socket share: a frame read and decoded under a
//! ceiling, a frame written under a ceiling (refused BEFORE its first byte when it is over), and the
//! one definition of a socket read timeout.
//!
//! The codec is `vike_hub_protocol::frame`'s and is not repeated here: a frame is read in any of the codec's forms
//! (`vike_hub_protocol::frame::BodyCodec`), and written in the form the session negotiated (`encode_capped_in`,
//! `write_up_in`; the `_in`-less spellings are JSON). The ceilings are the hosting
//! protocol's to state (the gatewayhub's are `GATEWAY_UP_FRAME_CEILING_BYTES` and
//! `GATEWAY_DOWN_FRAME_CEILING_BYTES`) and arrive as arguments: a reader reads under the ceiling of the
//! direction it receives, a writer writes under the one its peer reads under, so a frame over it must
//! be an error HERE, before its first byte, and not a framing fault that kills the peer's session.
//! A server that must notice a peer going away waits for a frame to begin with [`wait_readable`].

use std::io::{self, Read, Write};
use std::net::TcpStream;
use std::time::Duration;

use serde::Serialize;
use serde::de::DeserializeOwned;
use vike_hub_protocol::frame::{
    BodyCodec, decode_body, encode_frame, encode_reply, read_frame_raw_capped, write_frame_reserved,
};

use crate::binary_ticks::decode_stream_down;
use crate::wire::StreamDown;

/// `Read` one frame under `cap` and decode it, in whichever form it came, mapping a decode failure to
/// `InvalidData`.
pub fn read_capped<R: Read, T: DeserializeOwned>(r: &mut R, cap: u32) -> io::Result<T> {
    decode_body(&read_frame_raw_capped(r, cap)?)
}

/// `Read` one SERVER frame under `cap` and decode it: a binary tick body (`crate::binary_ticks`), or a frame in
/// any of the hub codec's forms (JSON, msgpack, LZ4), told apart by its first bytes. The receiving engine's one read; a decode failure is `InvalidData`.
pub fn read_down_capped<R: Read>(r: &mut R, cap: u32) -> io::Result<StreamDown> {
    decode_stream_down(&read_frame_raw_capped(r, cap)?)
}

/// Whether `e` is a socket read timeout, whatever the platform calls it (unix reports `WouldBlock`).
pub fn is_timeout(e: &io::Error) -> bool {
    matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut)
}

/// What [`wait_readable`] found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamFrameReady {
    /// At least one byte of a frame is waiting.
    Data,
    /// The peer closed its half.
    Eof,
    /// Nothing arrived within the wait.
    Timeout,
}

/// Wait up to `wait` (at least 1 ms) for a frame to BEGIN, without consuming a byte: a `peek`, so a
/// timeout cannot leave the stream half-read. An interrupted wait is a [`StreamFrameReady::Timeout`], and the socket's
/// read timeout is left at the wait. The one wait both daemons' servers (the gatewayhub's connections and
/// stream feeds, the datahub's strategy sessions) watch a peer with: a read timeout that fired mid-frame
/// would lose the bytes already consumed and desync the stream.
pub fn wait_readable(stream: &TcpStream, wait: Duration) -> io::Result<StreamFrameReady> {
    stream.set_read_timeout(Some(wait.max(Duration::from_millis(1))))?;
    let mut probe = [0u8; 1];
    match stream.peek(&mut probe) {
        Ok(0) => Ok(StreamFrameReady::Eof),
        Ok(_) => Ok(StreamFrameReady::Data),
        Err(e) if is_timeout(&e) || e.kind() == io::ErrorKind::Interrupted => {
            Ok(StreamFrameReady::Timeout)
        }
        Err(e) => Err(e),
    }
}

/// `vike_hub_protocol::frame::write_frame` for a CLIENT frame, refusing one over `ceiling` BEFORE its first byte: the daemon
/// reads client frames under that ceiling (the gatewayhub's `GATEWAY_UP_FRAME_CEILING_BYTES`) and treats a
/// longer one as a framing fault that closes the connection, so an over-long argument must be an error
/// here and not a lost session. `InvalidData`, like `write_frame`'s own pre-write refusals, means
/// nothing reached the wire. In JSON: [`write_up_in`] writes the form a session negotiated.
pub fn write_up<W: Write>(w: &mut W, msg: &impl Serialize, ceiling: u32) -> io::Result<()> {
    write_up_in(w, BodyCodec::Json, msg, ceiling)
}

/// [`write_up`] in `codec`'s form, the ceiling counted in that form.
pub fn write_up_in<W: Write>(
    w: &mut W,
    codec: BodyCodec,
    msg: &impl Serialize,
    ceiling: u32,
) -> io::Result<()> {
    // A client frame: the request form (`encode_frame`), whose LZ4 says the client takes LZ4.
    let frame = encode_frame(codec, msg)?;
    let len = frame.len() - 4;
    if len > ceiling as usize {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "a client frame of {len} bytes exceeds the {ceiling}-byte \
                 ceiling the gatewayhub reads client frames under; nothing was sent"
            ),
        ));
    }
    write_frame_reserved(w, frame)
}

/// Serialize `msg` into one length-prefixed frame, refusing a body over `ceiling` (`InvalidData`,
/// nothing written). The prefix and the body are one buffer, so the caller's single `write_all` is the
/// single write `vike_hub_protocol::frame::write_frame` pins.
pub fn encode_capped(msg: &impl Serialize, ceiling: u32) -> io::Result<Vec<u8>> {
    let mut frame = Vec::with_capacity(128);
    encode_capped_into(&mut frame, msg, ceiling)?;
    Ok(frame)
}

/// [`encode_capped`] appended to `buffer`, which may already hold earlier frames: the writer batches
/// several frames into one buffer and one `write_all`. On an error `buffer` is exactly as it was (a
/// refused frame adds no byte), so the bytes of the frames before it are still a valid stream. In JSON:
/// [`encode_capped_in`] encodes the form a session negotiated.
pub fn encode_capped_into(
    buffer: &mut Vec<u8>,
    msg: &impl Serialize,
    ceiling: u32,
) -> io::Result<()> {
    encode_capped_in(buffer, BodyCodec::Json, msg, ceiling)
}

/// [`encode_capped_into`] in `codec`'s form, the ceiling counted in that form.
pub fn encode_capped_in(
    buffer: &mut Vec<u8>,
    codec: BodyCodec,
    msg: &impl Serialize,
    ceiling: u32,
) -> io::Result<()> {
    let start = buffer.len();
    if codec == BodyCodec::Json {
        // Straight into the batch, as this encoder always wrote JSON: no frame of its own per frame.
        buffer.extend_from_slice(&[0u8; 4]);
        if let Err(e) = serde_json::to_writer(&mut *buffer, msg) {
            buffer.truncate(start);
            return Err(io::Error::new(io::ErrorKind::InvalidData, e));
        }
    } else {
        // A server frame in the reply form (`encode_reply`): LZ4 only where it shrinks the body.
        buffer.extend_from_slice(&encode_reply(codec, msg)?);
    }
    let len = buffer.len() - start - 4;
    if len > ceiling as usize {
        buffer.truncate(start);
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "a frame body of {len} bytes exceeds the {ceiling}-byte ceiling clients read \
                 server frames under"
            ),
        ));
    }
    let Ok(prefix) = u32::try_from(len) else {
        buffer.truncate(start);
        return Err(io::Error::new(io::ErrorKind::InvalidData, "frame body exceeds u32 length"));
    };
    buffer[start..start + 4].copy_from_slice(&prefix.to_be_bytes());
    Ok(())
}

/// [`encode_capped`] and write it: ONE `write_all`, then a flush. In JSON: [`write_capped_in`] writes the form a
/// session negotiated.
pub fn write_capped<W: Write>(w: &mut W, msg: &impl Serialize, ceiling: u32) -> io::Result<()> {
    write_capped_in(w, BodyCodec::Json, msg, ceiling)
}

/// [`write_capped`] in `codec`'s form (a server frame: the reply form), the ceiling counted in that form.
pub fn write_capped_in<W: Write>(
    w: &mut W,
    codec: BodyCodec,
    msg: &impl Serialize,
    ceiling: u32,
) -> io::Result<()> {
    let mut frame = Vec::with_capacity(128);
    encode_capped_in(&mut frame, codec, msg, ceiling)?;
    w.write_all(&frame)?;
    w.flush()
}

#[cfg(test)]
#[path = "frame_tests.rs"]
mod frame_tests;
