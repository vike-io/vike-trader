//! The BINARY tick frames: the per-tick [`StreamDown`] frames as one fixed little-endian body instead of
//! JSON text (`docs/decisions/0167-one-live-data-tube-vike-market-stream-on-a-data-plane-tier.md`'s
//! amendment of 2026-11-19, the owner's yes). The prices still flow gateway -> datahub -> tradehub; only the
//! bytes of one hop change.
//!
//! **Who sends it.** A sender writes these bodies only on a session that ASKED ([`StreamUp::BinaryTicks`]),
//! which the engine sends once at the start of a session and only to a peer whose hosting protocol advertised
//! it ([`crate::opener::StreamConnection::binary_ticks`]: the datahub's strategy lane, `stream_binary_ticks`).
//! So an older subscriber, which never asks, is sent JSON, and a newer one facing an older sender never asks
//! and reads JSON. Only the kinds below are binary; every other frame (control, gaps, bars, the folded book,
//! funding, the seed) stays JSON on every session.
//!
//! **How a receiver tells them apart: PER FRAME, by the first byte** ([`BINARY_TICKS_MARKER`], a NUL, which no
//! JSON body opens with: the bar-columns rule). The switch needs no ordering: a frame queued before the ask
//! is JSON and decodes as it always did ([`decode_stream_down`]).
//!
//! # The body (version 1)
//!
//! Every number little-endian; every `f64` is its eight bits, copied; a string is a `u16` byte length and
//! its UTF-8; a level is `price` then `qty`, bids first, then asks.
//!
//! | bytes | field |
//! |---|---|
//! | 1 | [`BINARY_TICKS_MARKER`] |
//! | 1 | [`BINARY_TICKS_VERSION`] |
//! | 1 | kind: 1 `Quote`, 2 `Trade`, 3 `BookDelta`, 4 `Mark`, 5 `BarCloseTick`, 6 `Depth` |
//! | 4 | `k` (`u32`) |
//! | 8 | `sequence` (`u64`) |
//!
//! then, by kind:
//!
//! | kind | fields |
//! |---|---|
//! | `Quote` | `timestamp`, `local_timestamp` (`i64`); `bid`, `ask`, `bid_size`, `ask_size` (`f64`); `symbol` |
//! | `Trade` | `timestamp`, `local_timestamp` (`i64`); `price`, `size` (`f64`); `is_buyer_maker` (one byte, 0 or 1); `symbol` |
//! | `BookDelta` | `timestamp`, `local_timestamp` (`i64`); the update's own `sequence` (`u64`); `kind` (one byte: 0 `Delta`, 1 `Snapshot`, 2 `GapStart`, 3 `Stale`, 4 `LiveResume`); `tick_size` (`f64`); bid and ask counts (`u32` each); the levels; `symbol` |
//! | `Mark`, `BarCloseTick` | `price` (`f64`); `timestamp` (`i64`) |
//! | `Depth` | `tick_size` (`f64`); `timestamp` (`i64`); bid and ask counts (`u32` each); the levels |
//!
//! ⚠ **The same values as the JSON frame, bit for bit, which is the contract** (`binary_ticks_tests.rs`
//! decodes both and compares). JSON carries a finite `f64` exactly (`float_roundtrip`) and cannot carry a
//! non-finite one at all, so the QUEUE refuses a non-finite row before any encoder sees it
//! (`crate::queue`'s module doc): a binary session carries exactly the frames a JSON session does. The codec
//! itself keeps any bits it is given.
//!
//! ⚠ **A frame this codec cannot carry goes as JSON, never as a hole**: a symbol over `u16::MAX` bytes, or a
//! body over the session's frame ceiling (a level is 16 bytes here and as few as 5 in JSON).
//! [`encode_binary_tick_into`] answers `false` and leaves the buffer as it was; the writer then frames JSON,
//! under its own ceiling rule.
//!
//! ⚠ **A malformed body is refused, never read short**: an unknown version or kind, a body that ends early
//! or runs on, a flag byte that is not 0 or 1, an unknown book kind, a symbol that is not UTF-8, a quote's or
//! trade's symbol longer than a serde read admits ([`Symbol::MAX_READ_LEN`], refused before it is interned). The reader
//! treats it as it treats undecodable JSON: the session is rebuilt. A new kind or a new field is a new
//! VERSION, and a receiver of this build refuses it by name, so a sender may write it only to a session
//! that asked for it under a new feature word (decision 0112).

use std::io;

use vike_hub_protocol::frame::BodyReader;
use vike_marketdata::{L2BookLevel, L2BookUpdate, L2BookUpdateKind, Symbol, TickQuote, TickTrade};

use crate::wire::StreamDown;
#[cfg(doc)]
use crate::wire::StreamUp;

/// The first byte of a binary tick body. A JSON body opens with `{`, `[`, `"`, a letter, a digit or
/// whitespace, never a NUL.
pub const BINARY_TICKS_MARKER: u8 = 0x00;

/// The layout version this build writes and reads (the module doc's table).
pub const BINARY_TICKS_VERSION: u8 = 1;

const QUOTE: u8 = 1;
const TRADE: u8 = 2;
const BOOK_DELTA: u8 = 3;
const MARK: u8 = 4;
const BAR_CLOSE_TICK: u8 = 5;
const DEPTH: u8 = 6;

/// Append `frame` to `buffer` as ONE length-prefixed binary frame (the `vike_hub_protocol::frame` prefix, a
/// big-endian `u32`, then the module doc's body) and answer `true`; or answer `false` with `buffer` exactly as
/// it was, for a frame of any other kind or one this codec cannot carry (the module doc's second ⚠), which
/// the caller then frames as JSON.
pub fn encode_binary_tick_into(buffer: &mut Vec<u8>, frame: &StreamDown, ceiling: u32) -> bool {
    let start = buffer.len();
    buffer.extend_from_slice(&[0u8; 4]);
    let encoded = match frame {
        StreamDown::Quote { k, sequence, quote } => {
            head(buffer, QUOTE, *k, *sequence);
            words(
                buffer,
                &[
                    quote.timestamp as u64,
                    quote.local_timestamp as u64,
                    quote.bid.to_bits(),
                    quote.ask.to_bits(),
                    quote.bid_size.to_bits(),
                    quote.ask_size.to_bits(),
                ],
            );
            string(buffer, &quote.symbol)
        }
        StreamDown::Trade { k, sequence, trade } => {
            head(buffer, TRADE, *k, *sequence);
            words(
                buffer,
                &[
                    trade.timestamp as u64,
                    trade.local_timestamp as u64,
                    trade.price.to_bits(),
                    trade.size.to_bits(),
                ],
            );
            buffer.push(u8::from(trade.is_buyer_maker));
            string(buffer, &trade.symbol)
        }
        StreamDown::BookDelta { k, sequence, update } => {
            head(buffer, BOOK_DELTA, *k, *sequence);
            words(
                buffer,
                &[update.timestamp as u64, update.local_timestamp as u64, update.sequence],
            );
            buffer.push(book_kind_byte(update.kind));
            words(buffer, &[update.tick_size.to_bits()]);
            levels(buffer, &update.bids, &update.asks) && string(buffer, &update.symbol)
        }
        StreamDown::Mark { k, sequence, price, timestamp } => {
            head(buffer, MARK, *k, *sequence);
            words(buffer, &[price.to_bits(), *timestamp as u64]);
            true
        }
        StreamDown::BarCloseTick { k, sequence, price, timestamp } => {
            head(buffer, BAR_CLOSE_TICK, *k, *sequence);
            words(buffer, &[price.to_bits(), *timestamp as u64]);
            true
        }
        StreamDown::Depth { k, sequence, tick_size, bids, asks, timestamp } => {
            head(buffer, DEPTH, *k, *sequence);
            words(buffer, &[tick_size.to_bits(), *timestamp as u64]);
            levels(buffer, bids, asks)
        }
        _ => false,
    };
    let len = buffer.len() - start - 4;
    match u32::try_from(len) {
        Ok(prefix) if encoded && prefix <= ceiling => {
            buffer[start..start + 4].copy_from_slice(&prefix.to_be_bytes());
            true
        }
        _ => {
            buffer.truncate(start);
            false
        }
    }
}

/// One SERVER frame body decoded: a binary tick body when it opens with [`BINARY_TICKS_MARKER`], else a frame in
/// the hub codec's forms (`vike_hub_protocol::frame::decode_body`: JSON, or msgpack behind its own NUL-led magic,
/// which is told from a tick body first). Every failure is `InvalidData`, as an undecodable JSON body always was.
pub fn decode_stream_down(body: &[u8]) -> io::Result<StreamDown> {
    use vike_hub_protocol::frame::{BodyCodec, decode_body};
    if body.first() != Some(&BINARY_TICKS_MARKER) || BodyCodec::of(body) != BodyCodec::Json {
        return decode_body(body);
    }
    decode_binary(body).map_err(|why| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("a malformed binary tick frame ({} bytes): {why}", body.len()),
        )
    })
}

fn head(buffer: &mut Vec<u8>, kind: u8, k: u32, sequence: u64) {
    buffer.extend_from_slice(&[BINARY_TICKS_MARKER, BINARY_TICKS_VERSION, kind]);
    buffer.extend_from_slice(&k.to_le_bytes());
    buffer.extend_from_slice(&sequence.to_le_bytes());
}

fn words(buffer: &mut Vec<u8>, words: &[u64]) {
    for word in words {
        buffer.extend_from_slice(&word.to_le_bytes());
    }
}

/// `false` (and nothing written) for a string longer than a `u16` can count.
fn string(buffer: &mut Vec<u8>, s: &str) -> bool {
    let Ok(len) = u16::try_from(s.len()) else { return false };
    buffer.extend_from_slice(&len.to_le_bytes());
    buffer.extend_from_slice(s.as_bytes());
    true
}

/// The two counts, then every level. `false` for a side longer than a `u32` can count.
fn levels(buffer: &mut Vec<u8>, bids: &[L2BookLevel], asks: &[L2BookLevel]) -> bool {
    let (Ok(b), Ok(a)) = (u32::try_from(bids.len()), u32::try_from(asks.len())) else {
        return false;
    };
    buffer.extend_from_slice(&b.to_le_bytes());
    buffer.extend_from_slice(&a.to_le_bytes());
    for level in bids.iter().chain(asks) {
        words(buffer, &[level.price.to_bits(), level.qty.to_bits()]);
    }
    true
}

/// Exhaustive: a new kind fails to compile here until it has a byte.
fn book_kind_byte(kind: L2BookUpdateKind) -> u8 {
    match kind {
        L2BookUpdateKind::Delta => 0,
        L2BookUpdateKind::Snapshot => 1,
        L2BookUpdateKind::GapStart => 2,
        L2BookUpdateKind::Stale => 3,
        L2BookUpdateKind::LiveResume => 4,
    }
}

fn book_kind(byte: u8) -> Option<L2BookUpdateKind> {
    Some(match byte {
        0 => L2BookUpdateKind::Delta,
        1 => L2BookUpdateKind::Snapshot,
        2 => L2BookUpdateKind::GapStart,
        3 => L2BookUpdateKind::Stale,
        4 => L2BookUpdateKind::LiveResume,
        _ => return None,
    })
}

fn decode_binary(body: &[u8]) -> Result<StreamDown, String> {
    let mut r = BodyReader::new(body);
    let (Some(_marker), Some(version), Some(kind), Some(k), Some(sequence)) =
        (r.u8(), r.u8(), r.u8(), r.u32(), r.u64())
    else {
        return Err("it ends inside its head".to_string());
    };
    if version != BINARY_TICKS_VERSION {
        return Err(format!(
            "version {version}, and this build reads version {BINARY_TICKS_VERSION} only"
        ));
    }
    let frame = fields(&mut r, kind, k, sequence)
        .ok_or_else(|| format!("its kind-{kind} fields do not decode"))?;
    if !r.is_done() {
        return Err(format!("bytes follow its kind-{kind} fields"));
    }
    Ok(frame)
}

/// The fields after the head; `None` for an unknown kind, a short body, a bad flag, book kind or symbol (not
/// UTF-8, or a tick's over [`Symbol::MAX_READ_LEN`]).
fn fields(r: &mut BodyReader<'_>, kind: u8, k: u32, sequence: u64) -> Option<StreamDown> {
    Some(match kind {
        QUOTE => StreamDown::Quote {
            k,
            sequence,
            quote: TickQuote {
                timestamp: r.i64()?,
                local_timestamp: r.i64()?,
                bid: r.f64()?,
                ask: r.f64()?,
                bid_size: r.f64()?,
                ask_size: r.f64()?,
                symbol: read_symbol(r)?,
            },
        },
        TRADE => StreamDown::Trade {
            k,
            sequence,
            trade: TickTrade {
                timestamp: r.i64()?,
                local_timestamp: r.i64()?,
                price: r.f64()?,
                size: r.f64()?,
                is_buyer_maker: match r.u8()? {
                    0 => false,
                    1 => true,
                    _ => return None,
                },
                symbol: read_symbol(r)?,
            },
        },
        BOOK_DELTA => {
            let (timestamp, local_timestamp, update_sequence) = (r.i64()?, r.i64()?, r.u64()?);
            let (update_kind, tick_size) = (book_kind(r.u8()?)?, r.f64()?);
            let (bids, asks) = read_levels(r)?;
            StreamDown::BookDelta {
                k,
                sequence,
                update: Box::new(L2BookUpdate {
                    timestamp,
                    local_timestamp,
                    sequence: update_sequence,
                    kind: update_kind,
                    tick_size,
                    bids,
                    asks,
                    symbol: read_string(r)?,
                }),
            }
        }
        MARK => StreamDown::Mark { k, sequence, price: r.f64()?, timestamp: r.i64()? },
        BAR_CLOSE_TICK => {
            StreamDown::BarCloseTick { k, sequence, price: r.f64()?, timestamp: r.i64()? }
        }
        DEPTH => {
            let (tick_size, timestamp) = (r.f64()?, r.i64()?);
            let (bids, asks) = read_levels(r)?;
            StreamDown::Depth { k, sequence, tick_size, bids, asks, timestamp }
        }
        _ => return None,
    })
}

fn read_string(r: &mut BodyReader<'_>) -> Option<String> {
    let len = r.u16()?;
    String::from_utf8(r.take(usize::from(len))?.to_vec()).ok()
}

/// [`read_string`] as a tick's interned symbol: the same bytes and the same UTF-8 check, no allocation, and the
/// cap a serde read applies: a text over [`Symbol::MAX_READ_LEN`] refuses the body and is never interned (an
/// interned text is kept for the process's life).
fn read_symbol(r: &mut BodyReader<'_>) -> Option<Symbol> {
    let len = usize::from(r.u16()?);
    if len > Symbol::MAX_READ_LEN {
        return None;
    }
    std::str::from_utf8(r.take(len)?).ok().map(Symbol::new)
}

/// The two counts and the levels; the counts are checked against the body BEFORE anything is allocated.
fn read_levels(r: &mut BodyReader<'_>) -> Option<(Vec<L2BookLevel>, Vec<L2BookLevel>)> {
    let (bids, asks) = (r.u32()? as usize, r.u32()? as usize);
    let bytes = r.take(bids.checked_add(asks)?.checked_mul(16)?)?;
    let (levels, _) = bytes.as_chunks::<16>();
    let mut all = levels.iter().map(|level| {
        let (price, qty) = level.split_at(8);
        L2BookLevel::new(
            f64::from_le_bytes(price.try_into().expect("eight bytes")),
            f64::from_le_bytes(qty.try_into().expect("eight bytes")),
        )
    });
    let bid_levels = all.by_ref().take(bids).collect();
    Some((bid_levels, all.collect()))
}

#[cfg(test)]
#[path = "binary_ticks_tests.rs"]
mod binary_ticks_tests;
