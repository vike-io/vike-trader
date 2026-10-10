//! The typed fast path in front of [`super::depth::route_frame`]: reads a combined-stream frame
//! (`{"stream":…,"data":…}`) straight into the tick, the trade or the depth diff, WITHOUT building
//! the `serde_json::Value` tree the old decode allocated for every frame (an object per envelope,
//! one more for `data`, an array and two `String`s per book level, a `String` per key).
//!
//! # It only ever answers a frame it fully understood
//!
//! [`scan_frame`] returns `Err` — "decline" — for anything but the shape the venue sends, and the
//! caller then runs the unchanged `Value` decoder on the same text. The accepted shape is exactly:
//! an object whose FIRST key is `stream` (an unescaped string) and whose SECOND is `data`, nothing
//! after; a `data` object whose keys are unique and whose read fields have the one expected type
//! (numbers as unescaped decimal STRINGS, ids as non-negative integers); every book level two
//! decimal strings. A repeated key, a wrong type, an escape, a missing field, an array where an
//! object belongs, trailing text or invalid JSON anywhere all decline, so on those inputs the
//! answer is the old decoder's by construction, not by argument.
//!
//! For the frames it does accept, equality with the old decoder is the proptest in
//! `crates/bridges/binance/tests/typed_frame_props.rs` (the old `route_frame` is kept there,
//! verbatim, as the oracle) and the scan reads each field exactly as the old code did: the same
//! `str::parse::<f64>` (so `"NaN"` and `"inf"` decode to the same values), `Value::as_u64`'s
//! non-negative-integer rule for `U`/`u`/`pu`, `Value::as_i64`'s for `T`. Nothing is APPLIED to the
//! book until the whole frame, trailing text included, has been read, because the old decoder
//! never touched the book for a frame that was not valid JSON end to end.
//!
//! The scanning blocks and why they are not `#[derive(Deserialize)]`: [`super::frame_scan`].

use serde::de::{self, DeserializeSeed, Deserializer, MapAccess, Visitor};
use std::fmt;
use vike_model::{BookLevel, QuoteTick, TradeTick};

use super::frame_scan::{
    DecimalStr, Fields, I64, Key, Levels, Object, StrRef, U64, missing, set, skip,
};

/// One diff read off a `@depth` frame, not yet folded.
pub(super) struct DepthDiff {
    pub first_u: u64,
    pub final_u: u64,
    /// `pu`, present on the futures grammar only.
    pub prev_final_u: Option<u64>,
    pub bids: Vec<BookLevel>,
    pub asks: Vec<BookLevel>,
}

/// What a fully-read frame is.
pub(super) enum Scanned {
    Quote(QuoteTick),
    Trade(TradeTick),
    Depth(DepthDiff),
    /// A well-formed frame on a stream the pump does not consume.
    Ignored,
}

/// Read `text` as one combined-stream frame, or decline (`Err`).
pub(super) fn scan_frame(text: &str, symbol: &str) -> Result<Scanned, serde_json::Error> {
    let mut de = serde_json::Deserializer::from_str(text);
    let scanned = Envelope { symbol }.deserialize(&mut de)?;
    de.end()?;
    Ok(scanned)
}

fn decline<E: de::Error>(why: &'static str) -> E {
    E::custom(why)
}

struct Envelope<'s> {
    symbol: &'s str,
}

impl<'de> DeserializeSeed<'de> for Envelope<'_> {
    type Value = Scanned;
    fn deserialize<D: Deserializer<'de>>(self, d: D) -> Result<Scanned, D::Error> {
        d.deserialize_map(self)
    }
}

impl<'de> Visitor<'de> for Envelope<'_> {
    type Value = Scanned;
    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a combined-stream frame")
    }
    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Scanned, A::Error> {
        // `stream` first and `data` second is how the venue sends it, and it is what lets `data`
        // be read once, straight into the right shape, instead of buffered until the kind is known.
        match map.next_key::<Key<'de>>()? {
            Some(Key(k)) if k == "stream" => {}
            _ => return Err(decline("stream is not the first key")),
        }
        let StrRef(stream) = map.next_value()?;
        match map.next_key::<Key<'de>>()? {
            Some(Key(k)) if k == "data" => {}
            _ => return Err(decline("data is not the second key")),
        }
        // The same precedence as the `Value` decoder: `@bookTicker`, then `@trade`, then `@depth`.
        let scanned = if stream.ends_with("@bookTicker") {
            Scanned::Quote(map.next_value_seed(QuoteSeed { symbol: self.symbol })?)
        } else if stream.ends_with("@trade") {
            Scanned::Trade(map.next_value_seed(TradeSeed { symbol: self.symbol })?)
        } else if stream.contains("@depth") {
            Scanned::Depth(map.next_value_seed(DepthSeed)?)
        } else {
            skip(&mut map)?;
            Scanned::Ignored
        };
        if map.next_key::<Key<'de>>()?.is_some() {
            return Err(decline("a key after data"));
        }
        Ok(scanned)
    }
}

// ---------------------------------------------------------------------------------------------
// `<sym>@bookTicker`
// ---------------------------------------------------------------------------------------------

struct QuoteSeed<'s> {
    symbol: &'s str,
}

#[derive(Default)]
struct QuoteFields {
    bid: Option<DecimalStr>,
    ask: Option<DecimalStr>,
    bid_size: Option<DecimalStr>,
    ask_size: Option<DecimalStr>,
}

impl<'de> Fields<'de> for QuoteFields {
    fn field<A: MapAccess<'de>>(&mut self, key: &str, map: &mut A) -> Result<(), A::Error> {
        match key {
            "b" => set(&mut self.bid, map.next_value()?),
            "a" => set(&mut self.ask, map.next_value()?),
            "B" => set(&mut self.bid_size, map.next_value()?),
            "A" => set(&mut self.ask_size, map.next_value()?),
            _ => skip(map),
        }
    }
}

impl<'de> DeserializeSeed<'de> for QuoteSeed<'_> {
    type Value = QuoteTick;
    fn deserialize<D: Deserializer<'de>>(self, d: D) -> Result<QuoteTick, D::Error> {
        let mut f = QuoteFields::default();
        Object(&mut f).deserialize(d)?;
        Ok(QuoteTick {
            ts: 0,
            local_ts: 0,
            bid: missing::<_, D::Error>(f.bid, "b")?.0,
            ask: missing::<_, D::Error>(f.ask, "a")?.0,
            bid_size: f.bid_size.map_or(0.0, |s| s.0),
            ask_size: f.ask_size.map_or(0.0, |s| s.0),
            symbol: self.symbol.to_string(),
        })
    }
}

// ---------------------------------------------------------------------------------------------
// `<sym>@trade`
// ---------------------------------------------------------------------------------------------

struct TradeSeed<'s> {
    symbol: &'s str,
}

/// A JSON boolean (`m`, "the buyer is the maker"). Not a shared block: only this frame reads one.
struct Bool(bool);

impl<'de> de::Deserialize<'de> for Bool {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl Visitor<'_> for V {
            type Value = Bool;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a boolean")
            }
            fn visit_bool<E: de::Error>(self, v: bool) -> Result<Bool, E> {
                Ok(Bool(v))
            }
        }
        d.deserialize_any(V)
    }
}

#[derive(Default)]
struct TradeFields {
    ts: Option<I64>,
    price: Option<DecimalStr>,
    size: Option<DecimalStr>,
    is_buyer_maker: Option<Bool>,
}

impl<'de> Fields<'de> for TradeFields {
    fn field<A: MapAccess<'de>>(&mut self, key: &str, map: &mut A) -> Result<(), A::Error> {
        match key {
            "T" => set(&mut self.ts, map.next_value()?),
            "p" => set(&mut self.price, map.next_value()?),
            "q" => set(&mut self.size, map.next_value()?),
            "m" => set(&mut self.is_buyer_maker, map.next_value()?),
            _ => skip(map),
        }
    }
}

impl<'de> DeserializeSeed<'de> for TradeSeed<'_> {
    type Value = TradeTick;
    fn deserialize<D: Deserializer<'de>>(self, d: D) -> Result<TradeTick, D::Error> {
        let mut f = TradeFields::default();
        Object(&mut f).deserialize(d)?;
        Ok(TradeTick {
            ts: f.ts.map_or(0, |t| t.0),
            local_ts: 0,
            price: missing::<_, D::Error>(f.price, "p")?.0,
            size: missing::<_, D::Error>(f.size, "q")?.0,
            is_buyer_maker: f.is_buyer_maker.is_some_and(|m| m.0),
            symbol: self.symbol.to_string(),
        })
    }
}

// ---------------------------------------------------------------------------------------------
// `<sym>@depth@100ms`
// ---------------------------------------------------------------------------------------------

struct DepthSeed;

#[derive(Default)]
struct DepthFields {
    first_u: Option<U64>,
    final_u: Option<U64>,
    prev_final_u: Option<U64>,
    bids: Option<Levels>,
    asks: Option<Levels>,
}

impl<'de> Fields<'de> for DepthFields {
    fn field<A: MapAccess<'de>>(&mut self, key: &str, map: &mut A) -> Result<(), A::Error> {
        match key {
            "U" => set(&mut self.first_u, map.next_value()?),
            "u" => set(&mut self.final_u, map.next_value()?),
            "pu" => set(&mut self.prev_final_u, map.next_value()?),
            "b" => set(&mut self.bids, map.next_value()?),
            "a" => set(&mut self.asks, map.next_value()?),
            _ => skip(map),
        }
    }
}

impl<'de> DeserializeSeed<'de> for DepthSeed {
    type Value = DepthDiff;
    fn deserialize<D: Deserializer<'de>>(self, d: D) -> Result<DepthDiff, D::Error> {
        let mut f = DepthFields::default();
        Object(&mut f).deserialize(d)?;
        Ok(DepthDiff {
            first_u: missing::<_, D::Error>(f.first_u, "U")?.0,
            final_u: missing::<_, D::Error>(f.final_u, "u")?.0,
            prev_final_u: f.prev_final_u.map(|p| p.0),
            bids: f.bids.map(|l| l.0).unwrap_or_default(),
            asks: f.asks.map(|l| l.0).unwrap_or_default(),
        })
    }
}

#[path = "depth_scan_tests.rs"]
#[cfg(test)]
mod depth_scan_tests;
