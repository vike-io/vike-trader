//! The BINARY answer of [`Request::KeptRowsOfVenues`](super::Request::KeptRowsOfVenues) (the owner's "yes both" of
//! 2026-11-19; `docs/decisions/0107-the-node-protocol-substrate-is-a-crate-below-both-clients.md`'s amendment of that
//! day): per asked venue its version STAMP and, unless the reader already holds that stamp, its kept rows and share
//! splits as MSGPACK, `rmp_serde`'s named encoding of `Vec<Instrument>` and `Vec<StockSplit>` ([`kept_msgpack`]).
//! Msgpack and not the bar columns' fixed-width layout, because a row is a nested struct of strings, enums and options,
//! and because it is the encoding a sandboxed backtest's child reads its rows in
//! (`crates/vike-backtesthub/src/strategy_sandbox/wire.rs`'s `write_message`): the backtesthub keeps the bytes and hands
//! them on as they came, with no conversion. Every `f64` keeps its bits.
//!
//! | bytes | what |
//! |---|---|
//! | 4 | [`KEPT_ROWS_MAGIC`]: a NUL first, which no JSON body has, so a reader tells this reply from a `Response::Error` |
//! | 4 | how many venues follow, a big-endian `u32`, so a body cut between two venues is refused too |
//! | per asked venue | its name, its stamp, then `1` and its rows and splits, or `0` ("unchanged": the reader's stamp) |
//!
//! A name, a stamp, the rows and the splits are each a big-endian `u32` length, then that many bytes. A stamp is
//! opaque to the reader: equal stamps mean the same bytes would be answered. The datahub's rule
//! (`crates/vike-datahub/src/instrument_lists.rs`'s `kept_rows_verb`) derives it from those bytes, so it moves with
//! every writer of the rows or the splits.

use serde::Serialize;
use vike_data::EncodedKept;
use vike_hub_protocol::frame::BodyReader;

/// What a kept-rows body opens with: a NUL, which no JSON body opens with.
pub const KEPT_ROWS_MAGIC: [u8; 4] = *b"\0KRW";

/// One venue of a kept-rows answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeptRowsOfVenue {
    /// The venue, as asked.
    pub venue: String,
    /// Its version stamp: what a later ask sends back in `known`.
    pub stamp: String,
    /// Its rows and splits, each one msgpack array; `None` is "unchanged": the reader sent this stamp.
    pub changed: Option<EncodedKept>,
}

/// `values` as one msgpack array, `rmp_serde`'s NAMED encoding: what a [`KeptRowsOfVenue`] carries.
///
/// # Errors
/// A value msgpack cannot write, in words.
pub fn kept_msgpack<T: Serialize>(values: &[T]) -> Result<Vec<u8>, String> {
    rmp_serde::to_vec_named(values).map_err(|e| format!("the kept rows could not be encoded: {e}"))
}

/// The body of `venues`, in order ([`KEPT_ROWS_MAGIC`] and the module doc's table).
#[must_use]
pub fn encode_kept_rows(venues: &[KeptRowsOfVenue]) -> Vec<u8> {
    fn put(out: &mut Vec<u8>, bytes: &[u8]) {
        // A part is far under the 64 MiB frame cap, so its length fits a `u32`.
        out.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
        out.extend_from_slice(bytes);
    }
    let mut out = KEPT_ROWS_MAGIC.to_vec();
    out.extend_from_slice(&(venues.len() as u32).to_be_bytes());
    for venue in venues {
        put(&mut out, venue.venue.as_bytes());
        put(&mut out, venue.stamp.as_bytes());
        match &venue.changed {
            None => out.push(0),
            Some(kept) => {
                out.push(1);
                put(&mut out, &kept.rows);
                put(&mut out, &kept.splits);
            }
        }
    }
    out
}

/// A kept-rows `body` back as its venues. REFUSED, never read short: a body without the magic, cut anywhere, with
/// bytes past its last venue, or with a name or stamp that is not UTF-8.
///
/// # Errors
/// What is wrong with the body, in words.
pub fn decode_kept_rows(body: &[u8]) -> Result<Vec<KeptRowsOfVenue>, String> {
    let Some(rest) = body.strip_prefix(&KEPT_ROWS_MAGIC[..]) else {
        return Err("the body is not a kept-rows answer".to_string());
    };
    // The one cursor every binary body is read through (`vike_hub_protocol::frame::BodyReader`); this body's
    // lengths are big-endian, so they are read as bytes and turned here.
    let mut reader = BodyReader::new(rest);
    let cut = || "a kept-rows answer cut short".to_string();
    let text = |bytes: &[u8]| {
        String::from_utf8(bytes.to_vec()).map_err(|_| "a kept-rows name is not UTF-8".to_string())
    };
    let count = u32::from_be_bytes(reader.array().ok_or_else(cut)?) as usize;
    let mut venues = Vec::with_capacity(count.min(rest.len()));
    for _ in 0..count {
        let venue = text(part(&mut reader).ok_or_else(cut)?)?;
        let stamp = text(part(&mut reader).ok_or_else(cut)?)?;
        let changed = match reader.u8().ok_or_else(cut)? {
            0 => None,
            1 => {
                let rows = part(&mut reader).ok_or_else(cut)?.to_vec();
                Some(EncodedKept { rows, splits: part(&mut reader).ok_or_else(cut)?.to_vec() })
            }
            other => {
                return Err(format!("a kept-rows venue is flagged {other}, neither 0 nor 1"));
            }
        };
        venues.push(KeptRowsOfVenue { venue, stamp, changed });
    }
    if !reader.is_done() {
        return Err("a kept-rows answer has bytes past its last venue".to_string());
    }
    Ok(venues)
}

/// The next length-prefixed part (a big-endian `u32` length, then that many bytes), `None` when cut.
fn part<'a>(reader: &mut BodyReader<'a>) -> Option<&'a [u8]> {
    let len = u32::from_be_bytes(reader.array()?);
    reader.take(len as usize)
}

/// `parts`, each ONE msgpack array, as one array of every element in order: what several venues' rows are as one
/// answer. The header is written as `rmp` writes one (the shortest form), so one part comes back byte for byte.
///
/// # Errors
/// A part that does not open with an array header.
pub fn join_msgpack_arrays(parts: &[&[u8]]) -> Result<Vec<u8>, String> {
    let mut count = 0u32;
    let mut items = Vec::with_capacity(parts.len());
    for part in parts {
        let (len, body) = array_items(part)?;
        count += len;
        items.push(body);
    }
    let mut out = Vec::with_capacity(5 + items.iter().map(|b| b.len()).sum::<usize>());
    match count {
        0..=15 => out.push(0x90 | count as u8),
        16..=0xffff => {
            out.push(0xdc);
            out.extend_from_slice(&(count as u16).to_be_bytes());
        }
        _ => {
            out.push(0xdd);
            out.extend_from_slice(&count.to_be_bytes());
        }
    }
    for body in items {
        out.extend_from_slice(body);
    }
    Ok(out)
}

/// A msgpack array's element count and its elements' bytes.
fn array_items(part: &[u8]) -> Result<(u32, &[u8]), String> {
    match *part {
        [marker @ 0x90..=0x9f, ref rest @ ..] => Ok((u32::from(marker & 0x0f), rest)),
        [0xdc, a, b, ref rest @ ..] => Ok((u32::from(u16::from_be_bytes([a, b])), rest)),
        [0xdd, a, b, c, d, ref rest @ ..] => Ok((u32::from_be_bytes([a, b, c, d]), rest)),
        _ => Err("a kept-rows part is not a msgpack array".to_string()),
    }
}

#[cfg(test)]
#[path = "kept_rows_tests.rs"]
mod tests;
