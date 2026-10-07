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
//! ⚠ **The socket half is here too, and it is the ONE spelling of it.** Every node-protocol
//! `TcpStream` — every server's accepted socket and every client's dialled one, on both node
//! protocols — goes through [`configure_node_stream`] (`TCP_NODELAY`), the node wire's twin of
//! `crates/vike-bridge-core/src/ws.rs`'s `configure_ws_stream` for venue sockets. It lives beside
//! [`write_frame`] because the two are halves of one property: a frame goes to the wire when it is
//! written, not when the peer's delayed-ACK timer fires. `git grep configure_node_stream` lists the
//! call sites; no roster is written down here.
//!
//! ⚠ **This module's round-trip tests did NOT move with it**, and that is deliberate rather than an
//! omission: they exercise the codec over `vike-datahub-client`'s own `Request`/`Response` pair,
//! which is exactly the coupling this crate exists to not have. They stay in
//! `crates/vike-datahub-client/src/proto.rs`, where they now drive the codec through that module's
//! re-export — so the thing under test is this code, reached the way a real caller reaches it.

use std::io::{self, Read, Write};
use std::net::TcpStream;

use serde::Serialize;
use serde::de::DeserializeOwned;

/// Upper bound on a single frame's body length (64 MiB). A declared length above this is rejected
/// by [`read_frame`] before any allocation, so a bad peer cannot drive us to OOM on a bogus prefix.
/// Comfortably larger than any real profile (in) or report (out), which are kilobytes; a bars/tick
/// answer for a chart's visible range is likewise bounded well under this.
pub const MAX_FRAME_LEN: u32 = 64 * 1024 * 1024;

/// Serialize `msg` to JSON and write it as ONE frame — a big-endian `u32` length prefix and the body
/// in a single `write_all` — then flush.
///
/// ⚠ **One write, and the reason is a 40 ms stall per frame, not tidiness.** Writing the prefix and
/// the body as two `write_all` calls on a `TcpStream` with Nagle on sends the four bytes alone and
/// holds the body until the peer ACKs them — and the peer, blocked reading the body, ACKs only when
/// its delayed-ACK timer fires (40 ms on Linux). A request and its answer each paid it: MEASURED
/// 2026-10-03 on a the latency box lane, 82 ms per loopback round trip with two writes, 0.028 ms with one.
/// Every node socket now ALSO carries `TCP_NODELAY` ([`configure_node_stream`]), and the two are
/// not redundant: a socket that never went through that seam (a test's raw stream, the next client
/// somebody writes) still has Nagle on, and a frame split in two would stall on it again.
/// `crates/vike-node-proto/tests/frame_write.rs` pins the single write, and the bytes against the
/// two-write encoder this replaced; `crates/vike-datahub/tests/wire_latency.rs` pins the round trip.
///
/// The bytes are the ones the two-write form produced, and so is every failure: the body is
/// serialized straight into the frame buffer behind four reserved bytes (no extra copy of it), the
/// ceiling is checked before anything is written, and `write_all` still loops over short writes,
/// retries an interrupted one and surfaces a timed-out or `WouldBlock` one — leaving a prefix of
/// the frame on the wire, as the two-write form could, and nothing else. That is why a caller's
/// rule for a write fault (close the connection: the stream is desynced) is unchanged.
///
/// Errors: a serialization failure or an over-[`MAX_FRAME_LEN`] body both surface as
/// [`io::ErrorKind::InvalidData`]; the underlying `write`/`flush` I/O errors pass through.
pub fn write_frame<W: Write>(w: &mut W, msg: &impl Serialize) -> io::Result<()> {
    // `serde_json::to_vec`'s own first allocation (128 bytes), with the four prefix bytes reserved
    // at its head: the body lands behind them and is never copied again.
    let mut frame = Vec::with_capacity(128);
    frame.extend_from_slice(&[0u8; 4]);
    serde_json::to_writer(&mut frame, msg)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    let len = u32::try_from(frame.len() - 4)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "frame body exceeds u32 length"))?;
    if len > MAX_FRAME_LEN {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("frame body of {len} bytes exceeds MAX_FRAME_LEN {MAX_FRAME_LEN}"),
        ));
    }
    frame[..4].copy_from_slice(&len.to_be_bytes());
    w.write_all(&frame)?;
    w.flush()
}

/// Arm a node-protocol socket for its frames: `TCP_NODELAY`, so the bytes [`write_frame`] writes go
/// to the wire at once instead of waiting for the peer to acknowledge what is already in flight.
///
/// One write per frame already removes the stall on a frame written ALONE. What it cannot cover is
/// a frame written while an earlier one is still unacknowledged: the second answer to two pipelined
/// requests, the next frame of a push stream (the tradehub's observe snapshots, the datahub's
/// market-data frames), and the last segment of a frame larger than one segment (64 KiB on
/// loopback, ~1.4 KiB across a LAN). Nagle holds that small segment until the ACK, and a peer that
/// is only reading ACKs on its delayed-ACK timer — MEASURED 2026-10-03 on a the latency box lane, ~40 ms per
/// pipelined pair at the datahub without this, `crates/vike-datahub/tests/wire_latency.rs`'s
/// `back_to_back_answers_cost_no_delayed_ack`. It costs nothing Nagle was buying: every frame is
/// one write, so there is no run of tiny writes for it to have been coalescing.
///
/// Call it on every node socket, on both ends — a server on the socket it accepted, a client on the
/// one it dialled — because each end's option governs only that end's sends.
///
/// ⚠ **A failure is never fatal to the connection**: Nagle costs latency, not correctness, so a
/// caller serves on rather than refusing a connection over a socket option. The `Err` is a failed
/// `setsockopt` on a socket that is otherwise fine; a caller with a logger logs it, and a client
/// crate that carries none drops it, as `configure_ws_stream` does.
pub fn configure_node_stream(stream: &TcpStream) -> io::Result<()> {
    stream.set_nodelay(true)
}

/// Read one length-prefixed frame and return its BODY BYTES — WITHOUT decoding.
///
/// Reads the 4-byte big-endian length, rejects a length above [`MAX_FRAME_LEN`] BEFORE allocating
/// (the OOM guard), then reads exactly that many bytes and returns them. A clean end-of-stream
/// surfaces as [`io::ErrorKind::UnexpectedEof`] (from `read_exact`), which callers treat as a closed
/// connection.
///
/// This is the lower half of [`read_frame`], split out (PR-2) so a server can separate FRAMING from
/// DECODING: a well-framed body that then fails to decode into a known `Request` is a bad
/// *request* — answer it with `Response::Error` and keep the connection — not a bad *connection*.
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
