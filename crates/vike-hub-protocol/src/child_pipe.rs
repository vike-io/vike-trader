//! **The pipe between a hub and a child it starts from its OWN executable**: the sandboxed live mount
//! (`vike-tradehub-core`'s `remote_strategy::protocol`) and the sandboxed backtest (`vike-backtesthub`'s
//! `strategy_sandbox::wire`). One codec and one same-build handshake for both, so the two pipes cannot
//! disagree about either.
//!
//! - **The frame** is the node wire's: a big-endian `u32` length, then a body of at most
//!   [`frame::MAX_FRAME_LEN`], written in ONE write ([`frame::write_frame_reserved`], whose doc says why). An
//!   end of stream reads as [`io::ErrorKind::UnexpectedEof`], a body that is not a message (or a length
//!   over the cap) as [`io::ErrorKind::InvalidData`].
//! - **The body is msgpack, not JSON**: a pipe carries NaN by design, and `serde_json` writes a non-finite
//!   `f64` as `null`, which reads back as an error or as `None`. Msgpack keeps every bit.
//! - **The body is POSITIONAL** ([`write_message`]: a struct is an array of its fields, no names), which only
//!   two copies of ONE build read alike. So each side's FIRST frame is its build ([`send_build`]) and each
//!   refuses another build in words ([`check_build`]) before it reads a message. This crate names no `vike-*`
//!   crate, so the build is the caller's: both ends pass `vike_buildinfo::summary()`.
//! - ⚠ **A type that leaves a field out while it is at its default** (`skip_serializing_if`) anywhere but as
//!   its LAST field shifts every later field into the wrong slot positional: such a payload crosses NAMED,
//!   either the whole message ([`write_message_named`]) or as an island inside a positional one ([`named`]).
//!   A field left out at the end reads back as its `#[serde(default)]` and needs neither. [`read_message`]
//!   reads both forms: rmp-serde decodes a struct from an array or from a map alike, and an enum variant
//!   crosses by its NAME in both.

use std::io::{self, Read, Write};

use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::frame;

/// Write `msg` as ONE frame, its body POSITIONAL msgpack (the module doc).
///
/// # Errors
/// An encoding failure or a body over [`frame::MAX_FRAME_LEN`] ([`io::ErrorKind::InvalidData`], nothing written);
/// the write's own.
pub fn write_message<W: Write>(w: &mut W, msg: &impl Serialize) -> io::Result<()> {
    let mut frame = vec![0u8; 4];
    rmp_serde::encode::write(&mut frame, msg).map_err(invalid)?;
    frame::write_frame_reserved(w, frame)
}

/// Write `msg` as ONE frame, its body msgpack WITH field names: a message whose type leaves a field out
/// before its last one (the module doc), which [`read_message`] reads as it reads a positional one.
///
/// # Errors
/// [`write_message`]'s.
pub fn write_message_named<W: Write>(w: &mut W, msg: &impl Serialize) -> io::Result<()> {
    let mut frame = vec![0u8; 4];
    rmp_serde::encode::write_named(&mut frame, msg).map_err(invalid)?;
    frame::write_frame_reserved(w, frame)
}

/// Read ONE frame and decode it, positional or named.
///
/// # Errors
/// The read's own (an end of stream is [`io::ErrorKind::UnexpectedEof`]); [`io::ErrorKind::InvalidData`]
/// for a length over [`frame::MAX_FRAME_LEN`] or a body that is not a `T`.
pub fn read_message<R: Read, T: DeserializeOwned>(r: &mut R) -> io::Result<T> {
    let body = frame::read_frame_raw(r)?;
    rmp_serde::from_slice(&body).map_err(invalid)
}

/// Write this side's build (`vike_buildinfo::summary()`, the caller's) as its FIRST frame: a plain string, the
/// one body whose layout never changes. The other side reads it with [`check_build`].
///
/// # Errors
/// [`write_message`]'s.
pub fn send_build<W: Write>(w: &mut W, build: &str) -> io::Result<()> {
    write_message(w, &build)
}

/// Read the other side's first frame and refuse, in words, a build that is not `ours`: the messages after it
/// are positional, and another build's could read into the wrong fields. `other` names that side in the
/// words ("the tradehub", "the strategy sandbox").
///
/// # Errors
/// Another build, or a first frame that is no build (a build from before the check): [`io::ErrorKind::Unsupported`],
/// naming both builds. The read's own (an end of stream is [`io::ErrorKind::UnexpectedEof`]).
pub fn check_build<R: Read>(r: &mut R, ours: &str, other: &str) -> io::Result<()> {
    let refused = |why: String| io::Error::new(io::ErrorKind::Unsupported, why);
    let theirs: String = match read_message(r) {
        Ok(theirs) => theirs,
        Err(e) if e.kind() == io::ErrorKind::InvalidData => {
            return Err(refused(format!(
                "refused: {other} opened with no build ({e}), an older build; this process is build `{ours}`"
            )));
        }
        Err(e) => return Err(e),
    };
    if theirs == ours {
        return Ok(());
    }
    Err(refused(format!(
        "refused: {other} is build `{theirs}`, this process is build `{ours}`: the pipe's messages are \
         positional, so both ends must be one build (a deploy replaced the binary under a running hub: \
         restart it)"
    )))
}

/// A codec's error as the frame's refusal.
fn invalid(e: impl Into<Box<dyn std::error::Error + Send + Sync>>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, e)
}

/// **A NAMED island inside a positional message** (`#[serde(with = "vike_hub_protocol::child_pipe::named")]` on
/// the field): the payload crosses as the bytes of a msgpack body of its own, with field names, so a type
/// that leaves a field out before its last one (the module doc) keeps every later field in its slot.
pub mod named {
    use std::marker::PhantomData;

    use serde::de::{DeserializeOwned, Deserializer, Visitor};
    use serde::ser::{Serialize, Serializer};

    /// `value` as the bytes of its named msgpack body.
    ///
    /// # Errors
    /// The encoding's, as the serializer's own error.
    pub fn serialize<T: Serialize, S: Serializer>(value: &T, s: S) -> Result<S::Ok, S::Error> {
        let body =
            rmp_serde::to_vec_named(value).map_err(<S::Error as serde::ser::Error>::custom)?;
        s.serialize_bytes(&body)
    }

    /// The value the bytes of a named msgpack body hold.
    ///
    /// # Errors
    /// Bytes that are not a `T`'s body, as the deserializer's own error.
    pub fn deserialize<'de, T: DeserializeOwned, D: Deserializer<'de>>(
        d: D,
    ) -> Result<T, D::Error> {
        struct Body<T>(PhantomData<T>);
        impl<T: DeserializeOwned> Visitor<'_> for Body<T> {
            type Value = T;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("the bytes of a named msgpack body")
            }
            fn visit_bytes<E: serde::de::Error>(self, body: &[u8]) -> Result<T, E> {
                rmp_serde::from_slice(body).map_err(E::custom)
            }
        }
        d.deserialize_bytes(Body(PhantomData))
    }
}

#[cfg(test)]
#[path = "child_pipe_tests.rs"]
mod child_pipe_tests;
