//! `observe_stream` — what a subscriber of the node's observe stream reads beyond a plain frame: a COMPRESSED body.
//!
//! A frame is the node codec's `[u32 big-endian len][body]`. A plain body is one [`Response`] in the connection's form
//! (`vike_hub_protocol::frame::BodyCodec`: JSON, or msgpack with field names where the node advertises
//! [`vike_hub_protocol::features::FEATURE_WIRE_MSGPACK`]). A subscriber that asked for compression
//! ([`crate::proto::ObserveSubscriptionWire::compressed`], to a node advertising
//! [`crate::proto::FEATURE_OBSERVE_COMPRESSED`]) may instead be sent ONE compression, by its form:
//!
//! - a msgpack subscriber: [`MSGPACK_DEFLATE_BODY_MAGIC`] and its msgpack body deflated ([`deflate_msgpack_body`]).
//!   Deflate, not the codec's LZ4 form, on this stream: measured on the 16-series synthetic node, deflate sends fewer
//!   bytes than LZ4 for the whole frames and the patches alike, and the link is the desktop's ssh tunnel;
//! - a JSON subscriber (a client from before msgpack): [`COMPRESSED_BODY_MARKER`] and the JSON deflated (raw deflate,
//!   `flate2`), as before.
//!
//! The node decides per its `config.observer_frame_compression` row. ⚠ A deflated JSON body, a deflated msgpack one and
//! the codec's magic-led msgpack all open with a NUL, and deflate's own bytes are anything, so the reader is told which
//! connection it reads ([`decode_response`]'s `connection`) and a deflated msgpack body names itself by four bytes no
//! deflate stream is read after: neither form can be mistaken for the other. A subscriber that never asked is never sent
//! a compressed body (`crates/vike-tradehub/src/server/observer_push.rs`), so an older client keeps reading exactly the
//! frames it always read.
//!
//! The decompressed body is bounded by [`MAX_FRAME_LEN`], the codec's own ceiling for a plain body, so a compressed
//! frame cannot make a reader hold more than a plain one could.
//!
//! A subscriber that asked for PATCHES ([`crate::proto::ObserveSubscriptionWire::patches`], to a node advertising
//! [`crate::proto::FEATURE_OBSERVE_PATCHES`]) is sent [`Response::SnapshotPatch`] frames instead: a whole snapshot, then
//! what changed ([`crate::observe_patch`]). [`ObserveReceiver`] holds the state they rebuild and says when a frame is
//! missing, so the reader asks for a whole one again ([`crate::proto::Request::ObserveResync`]).

use std::io::{self, Read, Write};

use serde::Deserialize;
use vike_hub_protocol::frame::{BodyCodec, MAX_FRAME_LEN, decode_body};

use crate::proto::Response;
use crate::wire::CoreSnapshotWire;

/// The first byte of a COMPRESSED frame body; the deflated JSON follows it.
pub const COMPRESSED_BODY_MARKER: u8 = 0;

/// The first four bytes of a DEFLATED MSGPACK frame body (a msgpack subscriber's compression); the deflated msgpack body,
/// its own `vike_hub_protocol::frame::MSGPACK_BODY_MAGIC` included, follows. A NUL first, as the codec's magics.
pub const MSGPACK_DEFLATE_BODY_MAGIC: [u8; 4] = *b"\0MDF";

/// The compressed body of one frame whose plain body is `json`: [`COMPRESSED_BODY_MARKER`] and the deflated JSON. The
/// fast level: the node compresses on a writer thread per subscriber, and the JSON of a snapshot shrinks several times
/// over at it.
pub fn compress_body(json: &[u8]) -> Vec<u8> {
    deflated(&[COMPRESSED_BODY_MARKER], json, flate2::Compression::fast())
}

/// The compressed body of one frame whose plain body is `msgpack` (magic-led): [`MSGPACK_DEFLATE_BODY_MAGIC`] and that
/// body deflated. The default level: the node deflates a whole frame once per publish for every subscriber, and a
/// patch is a few kilobytes, while the bytes cross a thin link.
pub fn deflate_msgpack_body(msgpack: &[u8]) -> Vec<u8> {
    deflated(&MSGPACK_DEFLATE_BODY_MAGIC, msgpack, flate2::Compression::default())
}

/// `head`, then `body` raw-deflated at `level`.
fn deflated(head: &[u8], body: &[u8], level: flate2::Compression) -> Vec<u8> {
    let mut out = Vec::with_capacity(head.len() + body.len() / 4 + 16);
    out.extend_from_slice(head);
    let mut encoder = flate2::write::DeflateEncoder::new(out, level);
    // Writing into a `Vec` cannot fail.
    encoder.write_all(body).expect("deflate into memory");
    encoder.finish().expect("deflate into memory")
}

/// One whole frame around `body`: the codec's length prefix, refused past [`MAX_FRAME_LEN`] as the codec refuses it.
pub fn frame_body(body: &[u8]) -> io::Result<Vec<u8>> {
    let length =
        u32::try_from(body.len()).ok().filter(|l| *l <= MAX_FRAME_LEN).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("frame body of {} bytes exceeds MAX_FRAME_LEN {MAX_FRAME_LEN}", body.len()),
            )
        })?;
    let mut frame = Vec::with_capacity(4 + body.len());
    frame.extend_from_slice(&length.to_be_bytes());
    frame.extend_from_slice(body);
    Ok(frame)
}

/// The JSON of one frame body: `body` itself, or inflated when it opens with [`COMPRESSED_BODY_MARKER`] (refused past
/// [`MAX_FRAME_LEN`] inflated).
pub fn plain_body(body: &[u8]) -> io::Result<std::borrow::Cow<'_, [u8]>> {
    let Some((&COMPRESSED_BODY_MARKER, deflated)) = body.split_first() else {
        return Ok(std::borrow::Cow::Borrowed(body));
    };
    inflate(deflated).map(std::borrow::Cow::Owned)
}

/// A raw-deflated body inflated, refused past [`MAX_FRAME_LEN`] inflated.
fn inflate(deflated: &[u8]) -> io::Result<Vec<u8>> {
    let mut plain = Vec::with_capacity(deflated.len() * 4);
    flate2::read::DeflateDecoder::new(deflated)
        .take(u64::from(MAX_FRAME_LEN) + 1)
        .read_to_end(&mut plain)?;
    if plain.len() > MAX_FRAME_LEN as usize {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("a compressed frame inflates past MAX_FRAME_LEN {MAX_FRAME_LEN}"),
        ));
    }
    Ok(plain)
}

/// Decode one observe-stream frame body ([`vike_hub_protocol::frame::read_frame_raw`]'s) into its [`Response`], compressed or not, on
/// a `connection` that writes in that form ([`crate::proto::node_codec`]): a msgpack connection inflates a body behind
/// [`MSGPACK_DEFLATE_BODY_MAGIC`] and reads every form of the hub codec; a JSON one, to a node before msgpack, inflates
/// a body behind [`COMPRESSED_BODY_MARKER`].
pub fn decode_response(body: &[u8], connection: BodyCodec) -> io::Result<Response> {
    match connection {
        BodyCodec::Json => decode_body(&plain_body(body)?),
        BodyCodec::Msgpack | BodyCodec::MsgpackLz4 => {
            match body.strip_prefix(&MSGPACK_DEFLATE_BODY_MAGIC[..]) {
                Some(deflated) => decode_body(&inflate(deflated)?),
                None => decode_body(body),
            }
        }
    }
}

/// What one frame of the stream gave its reader ([`ObserveReceiver::receive`]).
#[derive(Debug, PartialEq)]
pub enum Received {
    /// The node's state as of this frame.
    Snapshot(Box<CoreSnapshotWire>),
    /// A patch did not fit the state held (a frame went missing, or the state is not one it was made against): the
    /// reader asks for a whole frame ([`crate::proto::Request::ObserveResync`]). Said once per gap; the frames until the
    /// whole one are [`Received::Nothing`].
    Resync,
    /// No state in this frame: a heartbeat, another reply, or a patch while a whole frame is awaited.
    Nothing,
}

/// **The state a patched subscription rebuilds**, frame by frame: a whole frame (`base` absent) replaces it, a patch
/// whose `base` is the frame held applies to it, and anything else is a gap.
#[derive(Debug, Default)]
pub struct ObserveReceiver {
    /// The JSON of the snapshot held and the number of the frame that made it; `None` before the first whole frame and
    /// after a gap.
    state: Option<(serde_json::Value, u64)>,
    /// A resync was asked for and no whole frame has come since.
    awaiting_whole: bool,
}

impl ObserveReceiver {
    /// Read one response of the stream. A plain [`Response::SnapshotFrame`] is the state as it is.
    pub fn receive(&mut self, response: Response) -> Received {
        let patched = match response {
            Response::SnapshotFrame(wire) => return Received::Snapshot(wire),
            Response::SnapshotPatch(patched) => *patched,
            _ => return Received::Nothing,
        };
        let state = match (patched.base, self.state.take()) {
            (None, _) => Some(patched.patch),
            (Some(base), Some((mut state, held))) if base == held => {
                crate::observe_patch::apply(&mut state, patched.patch).ok().map(|()| state)
            }
            _ => None,
        };
        let decoded = state.and_then(|state| {
            let wire = CoreSnapshotWire::deserialize(&state).ok()?;
            Some((wire, state))
        });
        match decoded {
            Some((wire, state)) => {
                self.state = Some((state, patched.frame));
                self.awaiting_whole = false;
                Received::Snapshot(Box::new(wire))
            }
            None if self.awaiting_whole => Received::Nothing,
            None => {
                self.awaiting_whole = true;
                Received::Resync
            }
        }
    }
}

#[path = "observe_stream_tests.rs"]
#[cfg(test)]
mod observe_stream_tests;
