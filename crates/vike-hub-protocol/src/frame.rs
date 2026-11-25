//! `frame` — the length-prefixed JSON frame codec both hub protocols write and read.
//!
//! A frame is a big-endian `u32` body length followed by that many bytes of JSON; the two bodies that are
//! not JSON (the datahub's columns, decision 0107's amendment of 2026-11-18, and the strategy lane's
//! binary tick frames, decision 0167's amendment of 2026-11-19) are written by [`write_frame_reserved`]
//! (or [`write_frame_raw`]) and read by [`read_frame_raw`], under the same prefix and
//! ceiling, and their little-endian fields are read back through ONE cursor, [`BodyReader`]. Nothing here
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
//! ⚠ **The socket half is here too, and it is the ONE spelling of it.** Every hub-protocol
//! `TcpStream` — every server's accepted socket and every client's dialled one, on both node
//! protocols — goes through [`configure_hub_stream`] (`TCP_NODELAY`), the node wire's twin of
//! `crates/vike-transport/src/websocket.rs`'s `configure_websocket_stream` for venue sockets. It lives beside
//! [`write_frame`] because the two are halves of one property: a frame goes to the wire when it is
//! written, not when the peer's delayed-ACK timer fires. `git grep configure_hub_stream` lists the
//! call sites.
//!
//! The codec's round-trip tests live in `crates/vike-datahub-client/src/proto_tests.rs`, over that
//! crate's own `Request`/`Response` — a schema this crate must not know.
//!
//! # The msgpack body (JSON only at the edges, 2026-11-19)
//!
//! A frame body may be MSGPACK instead of JSON, chosen per connection ([`BodyCodec`]): a hub advertises a
//! feature word, a client that sees it writes its requests as [`MSGPACK_BODY_MAGIC`] bodies (msgpack WITH
//! field names, so an older reader skips a field it does not know), and the hub answers each request in
//! the codec it came in ([`encode_reply`]). A body tells its codec by its first bytes, a NUL that no JSON
//! body opens with, so [`decode_body`] reads any of the three and a client that never asked is answered
//! JSON. [`BodyCodec::MsgpackLz4`] is for a client on a THIN link (the desktop over its ssh tunnel): its
//! requests are LZ4 ([`MSGPACK_LZ4_BODY_MAGIC`]) and say so, and a reply is LZ4 when that shrinks it.
//! Never between daemons on one box: LZ4 costs CPU at both ends and buys nothing on loopback.

use std::io::{self, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};

use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::limits::{CLIENT_CONNECT_TIMEOUT, CLIENT_HANDSHAKE_DEADLINE};

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
/// its delayed-ACK timer fires (40 ms on Linux): MEASURED 82 ms per loopback round trip with two
/// writes, 0.028 ms with one. `TCP_NODELAY` ([`configure_hub_stream`]) does not make this
/// redundant: a socket that never went through that seam (a test's raw stream, the next client
/// somebody writes) still has Nagle on. `crates/vike-hub-protocol/tests/frame_write.rs` pins the
/// single write and the bytes; `crates/vike-datahub/tests/default_build/wire_latency.rs` pins the round trip.
///
/// The bytes and every failure are the two-write form's: the body is serialized straight into the
/// frame buffer behind four reserved bytes, the ceiling is checked before anything is written, and
/// `write_all` loops over short writes, retries an interrupted one and surfaces a timed-out or
/// `WouldBlock` one — leaving a prefix of the frame on the wire and nothing else. A caller's rule
/// for a write fault is therefore unchanged: close the connection, the stream is desynced.
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
    write_frame_reserved(w, frame)
}

/// [`write_frame`] for a body that is BYTES already, not a value to write as JSON: the one non-JSON
/// reply, the datahub's bar columns (`vike_datahub_client::proto`'s `BAR_COLUMNS_MAGIC`, decision
/// 0107's amendment of 2026-11-18). The same prefix, the same ceiling, the same single write; the body
/// is copied once behind the prefix to keep that write single.
pub fn write_frame_raw<W: Write>(w: &mut W, body: &[u8]) -> io::Result<()> {
    let mut frame = Vec::with_capacity(4 + body.len());
    frame.extend_from_slice(&[0u8; 4]);
    frame.extend_from_slice(body);
    write_frame_reserved(w, frame)
}

/// A frame the caller BUILT in place: four reserved bytes at its head (any value; they are overwritten), then
/// the body. Fills the prefix with the body's length, refuses a body over [`MAX_FRAME_LEN`] before writing
/// anything, and writes the frame in ONE `write_all`. For a body built straight into its frame (the datahub's
/// bar columns, `vike_datahub_client::proto::encode_bar_columns_into`, decision 0107's amendment of
/// 2026-11-18), so it is never copied behind a prefix; [`write_frame`] and [`write_frame_raw`] end here too.
pub fn write_frame_reserved<W: Write>(w: &mut W, mut frame: Vec<u8>) -> io::Result<()> {
    let body = frame.len().checked_sub(4).ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidData, "a frame shorter than its four reserved bytes")
    })?;
    let len = u32::try_from(body)
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

/// The first four bytes of a MSGPACK frame body: a NUL first, which no JSON body has (the line the datahub's
/// `BAR_COLUMNS_MAGIC` draws too), then the value as msgpack with field names (`rmp_serde::encode::write_named`).
pub const MSGPACK_BODY_MAGIC: [u8; 4] = *b"\0MPK";

/// The first four bytes of an LZ4-compressed msgpack body: then the msgpack's length (`u32`, little-endian) and
/// one LZ4 block of it (`lz4_flex::block`, the bar columns' codec).
pub const MSGPACK_LZ4_BODY_MAGIC: [u8; 4] = *b"\0MLZ";

/// How a frame body is written: the module doc's "The msgpack body".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BodyCodec {
    /// JSON, what every body was before 2026-11-19 and what a peer that never negotiated gets.
    #[default]
    Json,
    /// Msgpack with field names, behind [`MSGPACK_BODY_MAGIC`].
    Msgpack,
    /// Msgpack in one LZ4 block, behind [`MSGPACK_LZ4_BODY_MAGIC`]: a request is always compressed (that is
    /// how it asks for LZ4), a reply only when LZ4 shrinks it ([`encode_reply`]).
    MsgpackLz4,
}

impl BodyCodec {
    /// The codec `body` is written in, by its first bytes: what a hub answers that request in.
    pub fn of(body: &[u8]) -> Self {
        if body.starts_with(&MSGPACK_BODY_MAGIC) {
            BodyCodec::Msgpack
        } else if body.starts_with(&MSGPACK_LZ4_BODY_MAGIC) {
            BodyCodec::MsgpackLz4
        } else {
            BodyCodec::Json
        }
    }
}

/// `msg` as one whole frame in `codec` (four reserved bytes, then the body), for [`write_frame_reserved`].
/// [`BodyCodec::MsgpackLz4`] is always the LZ4 form here: a client's request carries its ask that way.
/// Errors are [`write_frame`]'s: an unwritable value or an over-[`MAX_FRAME_LEN`] body is
/// [`io::ErrorKind::InvalidData`], before anything is written.
pub fn encode_frame(codec: BodyCodec, msg: &impl Serialize) -> io::Result<Vec<u8>> {
    encode(codec, msg, true)
}

/// [`encode_frame`] for a hub's REPLY to a request in `codec`: an LZ4 request is answered LZ4 only when that
/// shrinks the body, plain msgpack otherwise (a client in [`BodyCodec::MsgpackLz4`] reads both).
pub fn encode_reply(codec: BodyCodec, msg: &impl Serialize) -> io::Result<Vec<u8>> {
    encode(codec, msg, false)
}

/// A REPLY body in `codec` from a value's msgpack written already (`rmp_serde`'s named encoding, no magic): byte for
/// byte the body [`encode_reply`] makes of that value, for a hub that HOLDS a reply's msgpack (the datahub's listings)
/// and so encodes nothing again. `None` for [`BodyCodec::Json`], which these bytes cannot answer.
pub fn reply_body_of_msgpack(codec: BodyCodec, msgpack: &[u8]) -> Option<Vec<u8>> {
    let plain = || [&MSGPACK_BODY_MAGIC[..], msgpack].concat();
    match codec {
        BodyCodec::Json => None,
        BodyCodec::Msgpack => Some(plain()),
        BodyCodec::MsgpackLz4 => {
            // `encode_reply`'s rule: LZ4 only where it shrinks the body, and never past the frame cap.
            let len = u32::try_from(msgpack.len()).ok().filter(|len| *len <= MAX_FRAME_LEN);
            let block = lz4_flex::block::compress(msgpack);
            match len {
                Some(len) if block.len() + 4 < msgpack.len() => {
                    Some([&MSGPACK_LZ4_BODY_MAGIC[..], &len.to_le_bytes(), &block].concat())
                }
                _ => Some(plain()),
            }
        }
    }
}

/// [`encode_frame`] written as ONE frame ([`write_frame_reserved`]): a client's request.
pub fn write_frame_as<W: Write>(
    w: &mut W,
    codec: BodyCodec,
    msg: &impl Serialize,
) -> io::Result<()> {
    write_frame_reserved(w, encode_frame(codec, msg)?)
}

/// [`encode_reply`] written as ONE frame: a hub's answer to a request whose body was in `codec`.
pub fn write_reply_as<W: Write>(
    w: &mut W,
    codec: BodyCodec,
    msg: &impl Serialize,
) -> io::Result<()> {
    write_frame_reserved(w, encode_reply(codec, msg)?)
}

/// The body of [`encode_frame`] and [`encode_reply`]; `always_lz4` is the request's rule.
fn encode(codec: BodyCodec, msg: &impl Serialize, always_lz4: bool) -> io::Result<Vec<u8>> {
    let invalid =
        |e: &dyn std::fmt::Display| io::Error::new(io::ErrorKind::InvalidData, e.to_string());
    let mut frame = Vec::with_capacity(128);
    frame.extend_from_slice(&[0u8; 4]);
    if codec == BodyCodec::Json {
        serde_json::to_writer(&mut frame, msg).map_err(|e| invalid(&e))?;
        return Ok(frame);
    }
    frame.extend_from_slice(&MSGPACK_BODY_MAGIC);
    rmp_serde::encode::write_named(&mut frame, msg).map_err(|e| invalid(&e))?;
    if codec == BodyCodec::Msgpack {
        return Ok(frame);
    }
    let plain = &frame[8..];
    // The decoder refuses a stated length over the cap, so LZ4 carries no body a plain frame could not.
    let len =
        u32::try_from(plain.len()).ok().filter(|len| *len <= MAX_FRAME_LEN).ok_or_else(|| {
            invalid(&format!("a msgpack body of {} bytes exceeds MAX_FRAME_LEN", plain.len()))
        })?;
    let block = lz4_flex::block::compress(plain);
    if !always_lz4 && block.len() + 4 >= plain.len() {
        return Ok(frame);
    }
    let mut packed = Vec::with_capacity(12 + block.len());
    packed.extend_from_slice(&[0u8; 4]);
    packed.extend_from_slice(&MSGPACK_LZ4_BODY_MAGIC);
    packed.extend_from_slice(&len.to_le_bytes());
    packed.extend_from_slice(&block);
    Ok(packed)
}

/// A frame `body` decoded as `T` in whichever codec it is written ([`BodyCodec::of`]): JSON, msgpack, or LZ4
/// msgpack. A body that does not decode is [`io::ErrorKind::InvalidData`], as [`read_frame`]'s is.
///
/// ⚠ An LZ4 body's stated length is checked against [`MAX_FRAME_LEN`] BEFORE the buffer is made, so a garbled
/// or hostile length cannot make the reader allocate what no frame could carry.
pub fn decode_body<T: DeserializeOwned>(body: &[u8]) -> io::Result<T> {
    let invalid =
        |e: &dyn std::fmt::Display| io::Error::new(io::ErrorKind::InvalidData, e.to_string());
    if let Some(msgpack) = body.strip_prefix(&MSGPACK_BODY_MAGIC[..]) {
        return rmp_serde::from_slice(msgpack).map_err(|e| invalid(&e));
    }
    let Some(packed) = body.strip_prefix(&MSGPACK_LZ4_BODY_MAGIC[..]) else {
        return serde_json::from_slice(body).map_err(|e| invalid(&e));
    };
    let (len, block) = packed
        .split_at_checked(4)
        .ok_or_else(|| invalid(&"an LZ4 msgpack body that ends inside its length"))?;
    let len = u32::from_le_bytes(len.try_into().expect("four bytes"));
    if len > MAX_FRAME_LEN {
        return Err(invalid(&format!(
            "an LZ4 msgpack body states {len} bytes, over MAX_FRAME_LEN"
        )));
    }
    let mut msgpack = vec![0u8; len as usize];
    match lz4_flex::block::decompress_into(block, &mut msgpack) {
        Ok(n) if n == msgpack.len() => rmp_serde::from_slice(&msgpack).map_err(|e| invalid(&e)),
        Ok(_) => Err(invalid(&"an LZ4 msgpack body shorter than the length it states")),
        Err(e) => Err(invalid(&format!("an LZ4 msgpack body that does not decompress ({e})"))),
    }
}

/// Arm a hub-protocol socket for its frames: `TCP_NODELAY`, so the bytes [`write_frame`] writes go
/// to the wire at once instead of waiting for the peer to acknowledge what is already in flight.
///
/// One write per frame removes the stall on a frame written ALONE, not on a frame written while an
/// earlier one is still unacknowledged: the second answer to two pipelined requests, the next frame
/// of a push stream, and the last segment of a frame larger than one segment. Nagle holds that
/// segment until the peer's delayed ACK — MEASURED ~40 ms per pipelined pair at the datahub without
/// this, `crates/vike-datahub/tests/default_build/wire_latency.rs`'s `back_to_back_answers_cost_no_delayed_ack`.
/// It costs nothing Nagle was buying: every frame is one write, so there is nothing to coalesce.
///
/// Call it on every node socket, on both ends — a server on the socket it accepted, a client on the
/// one it dialled — because each end's option governs only that end's sends.
///
/// ⚠ **A failure is never fatal to the connection**: Nagle costs latency, not correctness, so a
/// caller serves on rather than refusing a connection over a socket option. The `Err` is a failed
/// `setsockopt` on a socket that is otherwise fine; a caller with a logger logs it, and a client
/// crate that carries none drops it, as `configure_websocket_stream` does.
pub fn configure_hub_stream(stream: &TcpStream) -> io::Result<()> {
    stream.set_nodelay(true)
}

/// Resolve `addr` and open a TCP connection to the FIRST address that answers within
/// [`CLIENT_CONNECT_TIMEOUT`], with [`CLIENT_HANDSHAKE_DEADLINE`] armed as the new stream's read and
/// write timeout for the protocol handshake that follows, and `TCP_NODELAY` on
/// ([`configure_hub_stream`]).
///
/// The ONE way a node CLIENT dials: [`crate::conn::HubConnection`] (the datahub's and the backtesthub's clients),
/// `vike-strategy-builder`'s client and `vike-tradehub-client` all call it, so a dead or half-open host costs a bounded time on every one
/// of them (a plain `TcpStream::connect` waits out the OS SYN-retry ladder, MINUTES on Linux).
///
/// `connect_timeout` takes ONE `SocketAddr`, so the walk `TcpStream::connect` does itself is written
/// out here: a name routinely resolves to both an A and an AAAA record, and trying only the first
/// would fail against a dual-stack host whose IPv6 route is dark. The budget is therefore PER
/// ADDRESS: N candidates can cost N × [`CLIENT_CONNECT_TIMEOUT`].
///
/// The LAST error is returned when every candidate fails (the first is usually the dead IPv6 leg),
/// and an address that resolves to nothing is its own [`io::ErrorKind::InvalidInput`]. The
/// handshake bounds are a HANDSHAKE-phase policy: a caller replaces them (clears the read bound,
/// sets its own write bound) once the handshake is done. A refused `TCP_NODELAY` costs latency,
/// never correctness, so it does not fail the connect.
pub fn connect_bounded<A: ToSocketAddrs>(addr: A) -> io::Result<TcpStream> {
    let mut last_err: Option<io::Error> = None;
    for candidate in addr.to_socket_addrs()? {
        match TcpStream::connect_timeout(&candidate, CLIENT_CONNECT_TIMEOUT) {
            Ok(stream) => {
                stream.set_read_timeout(Some(CLIENT_HANDSHAKE_DEADLINE))?;
                stream.set_write_timeout(Some(CLIENT_HANDSHAKE_DEADLINE))?;
                let _ = configure_hub_stream(&stream);
                return Ok(stream);
            }
            Err(e) => last_err = Some(e),
        }
    }
    Err(last_err.unwrap_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "the address resolved to no socket addresses")
    }))
}

/// Read one length-prefixed frame and return its BODY BYTES — WITHOUT decoding.
///
/// Reads the 4-byte big-endian length, rejects a length above [`MAX_FRAME_LEN`] BEFORE allocating
/// (the OOM guard), then reads exactly that many bytes and returns them. A clean end-of-stream
/// surfaces as [`io::ErrorKind::UnexpectedEof`] (from `read_exact`), which callers treat as a closed
/// connection.
///
/// The lower half of [`read_frame`], split out so a server can separate FRAMING from DECODING: a
/// well-framed body that then fails to decode into a known `Request` is a bad *request* — answer it
/// with `Response::Error` and keep the connection — not a bad *connection*, which is what a decode
/// error fused into the read would look like.
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
    let mut body = Vec::new();
    read_frame_raw_capped_into(r, max_len, &mut body)?;
    Ok(body)
}

/// [`read_frame_raw`] INTO `body`, whose old bytes it replaces: a reader of many frames that keeps none of
/// them hands the same buffer back each time and reads them all into ONE allocation, grown only for a
/// frame longer than its capacity. The ceiling, and its check before any allocation, are [`read_frame_raw`]'s.
pub fn read_frame_raw_into<R: Read>(r: &mut R, body: &mut Vec<u8>) -> io::Result<()> {
    read_frame_raw_capped_into(r, MAX_FRAME_LEN, body)
}

/// [`read_frame_raw_capped`] into `body` ([`read_frame_raw_into`]). An EMPTY `body` gets a fresh zeroed allocation
/// (`vec!`, whose pages the read itself touches first): what [`read_frame_raw_capped`] has always returned.
fn read_frame_raw_capped_into<R: Read>(
    r: &mut R,
    max_len: u32,
    body: &mut Vec<u8>,
) -> io::Result<()> {
    let cap = max_len.min(MAX_FRAME_LEN);
    let mut len_buffer = [0u8; 4];
    r.read_exact(&mut len_buffer)?;
    let len = u32::from_be_bytes(len_buffer);
    if len > cap {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("declared frame length {len} exceeds the {cap}-byte cap for this phase"),
        ));
    }
    if body.capacity() == 0 {
        *body = vec![0u8; len as usize];
    } else {
        body.clear();
        body.resize(len as usize, 0);
    }
    r.read_exact(body)
}

/// Read one length-prefixed frame and decode it as `T` — [`read_frame_raw`] plus
/// [`decode_body`]: a JSON, msgpack or LZ4 msgpack body alike.
///
/// A clean end-of-stream surfaces as [`io::ErrorKind::UnexpectedEof`]; an over-[`MAX_FRAME_LEN`]
/// length or a decode failure both surface as [`io::ErrorKind::InvalidData`], so a caller has ONE
/// error channel — a server that must survive an undecodable body reads with [`read_frame_raw`]
/// instead (the module doc's decode-vs-drop split).
pub fn read_frame<R: Read, T: DeserializeOwned>(r: &mut R) -> io::Result<T> {
    // In whichever codec it came ([`decode_body`]): a pushed frame after a mode switch is in the codec of the
    // request that opened the stream, and its reader asked for none.
    decode_body(&read_frame_raw(r)?)
}

/// The exact length of `value`'s compact JSON — what [`write_frame`] puts in a frame body —
/// measured without building it, or `None` when serde cannot write the value. Each caller decides
/// what an unwritable value counts as: a frame budget refuses it as larger than any frame, a pager
/// counts it as nothing and lets the frame write that follows fail loudly.
pub fn json_body_len<T: Serialize + ?Sized>(value: &T) -> Option<usize> {
    let mut count = ByteCount(0);
    serde_json::to_writer(&mut count, value).ok().map(|()| count.0)
}

/// A read position in a BINARY frame body: the one cursor every non-JSON body is decoded through (the
/// datahub's bar columns, the strategy lane's tick frames). Each read is bounds-checked and answers `None`
/// past the end, so a truncated body is a refusal and never a short read or a panic; numbers are
/// little-endian.
#[derive(Debug)]
pub struct BodyReader<'a> {
    body: &'a [u8],
    at: usize,
}

impl<'a> BodyReader<'a> {
    /// A cursor at the first byte of `body`.
    pub fn new(body: &'a [u8]) -> Self {
        Self { body, at: 0 }
    }

    /// The next `len` bytes, or `None` (the position unmoved) when fewer are left.
    pub fn take(&mut self, len: usize) -> Option<&'a [u8]> {
        let end = self.at.checked_add(len).filter(|end| *end <= self.body.len())?;
        let taken = &self.body[self.at..end];
        self.at = end;
        Some(taken)
    }

    /// The next `N` bytes as an array.
    pub fn array<const N: usize>(&mut self) -> Option<[u8; N]> {
        self.take(N).map(|b| b.try_into().expect("N bytes"))
    }

    /// The next byte.
    pub fn u8(&mut self) -> Option<u8> {
        self.array::<1>().map(|[b]| b)
    }

    /// The next little-endian `u16`.
    pub fn u16(&mut self) -> Option<u16> {
        self.array().map(u16::from_le_bytes)
    }

    /// The next little-endian `u32`.
    pub fn u32(&mut self) -> Option<u32> {
        self.array().map(u32::from_le_bytes)
    }

    /// The next little-endian `u64`.
    pub fn u64(&mut self) -> Option<u64> {
        self.array().map(u64::from_le_bytes)
    }

    /// The next little-endian `i64`.
    pub fn i64(&mut self) -> Option<i64> {
        self.array().map(i64::from_le_bytes)
    }

    /// The next eight bytes as `f64` BITS, verbatim (a NaN keeps its payload).
    pub fn f64(&mut self) -> Option<f64> {
        self.u64().map(f64::from_bits)
    }

    /// Whether every byte of the body has been read: a decoder refuses a body with bytes left over.
    pub fn is_done(&self) -> bool {
        self.at == self.body.len()
    }
}

/// An `io::Write` that keeps only a COUNT of what is written to it.
struct ByteCount(usize);

impl Write for ByteCount {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.0 += buffer.len();
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
#[path = "frame_tests.rs"]
mod frame_tests;
