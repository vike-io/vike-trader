//! `frame` — the length-prefixed JSON frame codec both node protocols write and read.
//!
//! A frame is a big-endian `u32` body length followed by that many bytes of JSON. Nothing here
//! knows what the JSON MEANS: [`write_frame`] takes anything `Serialize` and [`read_frame`] returns
//! anything `DeserializeOwned`, which is what lets one codec serve two schemas that share no type.
//!
//! ⚠ **The decode-vs-drop split is the contract worth carrying.** [`read_frame`] fuses transport
//! and decode into one error channel, which is what a CLIENT wants; a server that must stay up
//! across a bad request reads with [`read_frame_raw`] and decodes separately, so an undecodable
//! body is a bad REQUEST rather than a bad CONNECTION. [`read_frame_raw_capped`] is that same
//! primitive under a caller-chosen ceiling, which is how a node server reads its PRE-AUTH frames
//! without letting an unauthenticated peer allocate [`MAX_FRAME_LEN`] off a four-byte prefix.
//!
//! ⚠ **This module's round-trip tests did NOT move with it**, and that is deliberate rather than an
//! omission: they exercise the codec over `vike-datahub-client`'s own `Request`/`Response` pair,
//! which is exactly the coupling this crate exists to not have. They stay in
//! `crates/vike-datahub-client/src/proto.rs`, where they now drive the codec through that module's
//! re-export — so the thing under test is this code, reached the way a real caller reaches it.

use std::io::{self, Read, Write};

use serde::Serialize;
use serde::de::DeserializeOwned;

/// Upper bound on a single frame's body length (64 MiB). A declared length above this is rejected
/// by [`read_frame`] before any allocation, so a bad peer cannot drive us to OOM on a bogus prefix.
/// Comfortably larger than any real profile (in) or report (out), which are kilobytes; a bars/tick
/// answer for a chart's visible range is likewise bounded well under this.
pub const MAX_FRAME_LEN: u32 = 64 * 1024 * 1024;

/// Serialize `msg` to JSON, write a big-endian `u32` length prefix, write the body, and flush.
///
/// Errors: a serialization failure or an over-[`MAX_FRAME_LEN`] body both surface as
/// [`io::ErrorKind::InvalidData`]; the underlying `write`/`flush` I/O errors pass through.
pub fn write_frame<W: Write>(w: &mut W, msg: &impl Serialize) -> io::Result<()> {
    let bytes =
        serde_json::to_vec(msg).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    let len = u32::try_from(bytes.len())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "frame body exceeds u32 length"))?;
    if len > MAX_FRAME_LEN {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("frame body of {len} bytes exceeds MAX_FRAME_LEN {MAX_FRAME_LEN}"),
        ));
    }
    w.write_all(&len.to_be_bytes())?;
    w.write_all(&bytes)?;
    w.flush()
}

/// Read one length-prefixed frame and return its BODY BYTES — WITHOUT decoding.
///
/// Reads the 4-byte big-endian length, rejects a length above [`MAX_FRAME_LEN`] BEFORE allocating
/// (the OOM guard), then reads exactly that many bytes and returns them. A clean end-of-stream
/// surfaces as [`io::ErrorKind::UnexpectedEof`] (from `read_exact`), which callers treat as a closed
/// connection.
///
/// This is the lower half of [`read_frame`], split out (PR-2) so a server can separate FRAMING from
/// DECODING: a well-framed body that then fails to decode into a known [`Request`] is a bad
/// *request* — answer it with [`Response::Error`] and keep the connection — not a bad *connection*.
/// If decode were fused into the read (as in [`read_frame`]), that decode error would be
/// indistinguishable from a transport fault and would drop the connection, the exact footgun PR-2
/// removes.
pub fn read_frame_raw<R: Read>(r: &mut R) -> io::Result<Vec<u8>> {
    read_frame_raw_capped(r, MAX_FRAME_LEN)
}

/// [`read_frame_raw`] with a CALLER-CHOSEN ceiling instead of [`MAX_FRAME_LEN`].
///
/// The guard is the same one and it still fires BEFORE the allocation — this only lets a server
/// spend less trust on a peer it has not authenticated yet. [`MAX_FRAME_LEN`] is 64 MiB because a
/// legitimate *answer* (a chart's worth of bars) can be large; a legitimate *handshake* frame is a
/// few hundred bytes, so accepting 64 MiB of it means an unauthenticated peer can make the server
/// allocate 64 MiB per connection by sending four bytes. A caller that knows the phase can say so.
///
/// `max_len` is clamped to [`MAX_FRAME_LEN`]: this is a way to ask for LESS trust, never more, so a
/// larger value cannot widen the global OOM guard.
pub fn read_frame_raw_capped<R: Read>(r: &mut R, max_len: u32) -> io::Result<Vec<u8>> {
    let cap = max_len.min(MAX_FRAME_LEN);
    let mut len_buf = [0u8; 4];
    r.read_exact(&mut len_buf)?;
    let len = u32::from_be_bytes(len_buf);
    if len > cap {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("declared frame length {len} exceeds the {cap}-byte cap for this phase"),
        ));
    }
    let mut body = vec![0u8; len as usize];
    r.read_exact(&mut body)?;
    Ok(body)
}

/// Read one length-prefixed frame and decode it as `T` — [`read_frame_raw`] plus
/// `serde_json::from_slice`.
///
/// A clean end-of-stream surfaces as [`io::ErrorKind::UnexpectedEof`]; an over-[`MAX_FRAME_LEN`]
/// length or a decode failure both surface as [`io::ErrorKind::InvalidData`], so a caller has ONE
/// error channel. NOTE the fusion: a server that must keep the connection alive across an
/// undecodable body should read with [`read_frame_raw`] and decode separately, so a decode error is
/// not mistaken for a transport fault (see that function's docs and the module-level decode-vs-drop
/// contract).
pub fn read_frame<R: Read, T: DeserializeOwned>(r: &mut R) -> io::Result<T> {
    let body = read_frame_raw(r)?;
    serde_json::from_slice(&body).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}
