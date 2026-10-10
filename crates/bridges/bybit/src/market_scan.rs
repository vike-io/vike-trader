//! The typed fast path in front of [`crate::market_data::route_frame`]: reads a Bybit
//! `orderbook.*` or `publicTrade.*` frame straight into the book diff or the trade, WITHOUT the
//! `serde_json::Value` tree the old decode allocated for every frame (and `orderbook.50` is the
//! fastest frame this adapter sees: a delta every 20 ms per symbol).
//!
//! # It only ever answers a frame it fully understood
//!
//! [`scan_frame`] returns `Err` — "decline" — for anything but the shape the venue sends, and the
//! caller then runs the unchanged `Value` decoder on the same text. Accepted: an object whose FIRST
//! key is `topic` (an unescaped string); keys then in any order, none of `topic`/`type`/`ts`/`data`
//! repeated; on an `orderbook.*` topic `type` is `snapshot` or `delta` and `data` is an object with
//! a non-negative integer `u` and `[price, qty]` decimal-string levels in `b`/`a`; on a
//! `publicTrade*` topic `data` is a non-empty array whose FIRST element is an object with decimal
//! strings `p` and `v`. A wrong type, an escape, a missing field, a repeated key, trailing text or
//! invalid JSON anywhere declines, so on those inputs the answer is the old decoder's by
//! construction. (Like the old decoder, only the FIRST trade of a `publicTrade` frame is read; the
//! rest are validated and dropped.)
//!
//! Equality with the old decoder on the frames this accepts is the proptest in
//! `crates/bridges/bybit/tests/typed_frame_props.rs`, which keeps the old `route_frame` verbatim as
//! the oracle. Nothing is applied to the book until the whole frame, trailing text included, has
//! been read. The scanning blocks, and why they are not `#[derive(Deserialize)]`:
//! `crates/bridges/binance/src/family/frame_scan.rs`.

use std::fmt;

use serde::de::{self, DeserializeSeed, Deserializer, MapAccess, SeqAccess, Visitor};
use vike_model::{BookLevel, TradeTick};

use crate::frame_scan::{
    DecimalStr, Fields, I64, Key, Levels, Object, Skip, StrRef, U64, missing, set, skip,
};

/// The snapshot-or-delta word of an `orderbook` frame.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum BookKind {
    Snapshot,
    Delta,
}

/// One `orderbook.*` frame, read but not applied.
pub(crate) struct BookFrame {
    pub kind: BookKind,
    pub u: u64,
    pub ts_ms: i64,
    pub bids: Vec<BookLevel>,
    pub asks: Vec<BookLevel>,
}

/// What a fully-read frame is.
pub(crate) enum Scanned {
    Book(BookFrame),
    Trade(TradeTick),
    /// A well-formed frame on a topic the pump does not consume.
    Ignored,
}

/// Read `text` as one Bybit public frame, or decline (`Err`).
pub(crate) fn scan_frame(text: &str, symbol: &str) -> Result<Scanned, serde_json::Error> {
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

enum Topic {
    Book,
    Trade,
    Other,
}

impl<'de> Visitor<'de> for Envelope<'_> {
    type Value = Scanned;
    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a Bybit public frame")
    }
    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Scanned, A::Error> {
        // `topic` first is how the venue sends it, and it is what lets `data` be read once,
        // straight into the right shape.
        match map.next_key::<Key<'de>>()? {
            Some(Key(k)) if k == "topic" => {}
            _ => return Err(decline("topic is not the first key")),
        }
        let StrRef(topic) = map.next_value()?;
        // The same precedence as the `Value` decoder: `orderbook.`, then `publicTrade`.
        let kind = if topic.starts_with("orderbook.") {
            Topic::Book
        } else if topic.starts_with("publicTrade") {
            Topic::Trade
        } else {
            Topic::Other
        };
        let mut msg_type: Option<StrRef<'de>> = None;
        let mut ts: Option<I64> = None;
        let mut book: Option<BookData> = None;
        let mut trade: Option<TradeTick> = None;
        let mut data_seen = false;
        while let Some(Key(k)) = map.next_key::<Key<'de>>()? {
            match &*k {
                "type" => set::<_, A::Error>(&mut msg_type, map.next_value()?)?,
                "ts" => set::<_, A::Error>(&mut ts, map.next_value()?)?,
                "data" => {
                    if std::mem::replace(&mut data_seen, true) {
                        return Err(decline("data twice"));
                    }
                    match kind {
                        Topic::Book => book = Some(map.next_value()?),
                        Topic::Trade => {
                            trade = Some(map.next_value_seed(FirstTrade { symbol: self.symbol })?);
                        }
                        Topic::Other => skip(&mut map)?,
                    }
                }
                "topic" => return Err(decline("topic twice")),
                _ => skip(&mut map)?,
            }
        }
        match kind {
            Topic::Book => {
                let book = book.ok_or_else(|| decline::<A::Error>("orderbook without data"))?;
                let kind = match msg_type.as_ref().map(|t| t.0) {
                    Some("snapshot") => BookKind::Snapshot,
                    Some("delta") => BookKind::Delta,
                    _ => return Err(decline("orderbook type")),
                };
                Ok(Scanned::Book(BookFrame {
                    kind,
                    u: book.u,
                    ts_ms: ts.map_or(0, |t| t.0),
                    bids: book.bids,
                    asks: book.asks,
                }))
            }
            Topic::Trade => {
                let trade = trade.ok_or_else(|| decline::<A::Error>("publicTrade without data"))?;
                Ok(Scanned::Trade(trade))
            }
            Topic::Other => Ok(Scanned::Ignored),
        }
    }
}

// ---------------------------------------------------------------------------------------------
// `orderbook.*` data
// ---------------------------------------------------------------------------------------------

struct BookData {
    u: u64,
    bids: Vec<BookLevel>,
    asks: Vec<BookLevel>,
}

#[derive(Default)]
struct BookFields {
    u: Option<U64>,
    bids: Option<Levels>,
    asks: Option<Levels>,
}

impl<'de> Fields<'de> for BookFields {
    fn field<A: MapAccess<'de>>(&mut self, key: &str, map: &mut A) -> Result<(), A::Error> {
        match key {
            "u" => set(&mut self.u, map.next_value()?),
            "b" => set(&mut self.bids, map.next_value()?),
            "a" => set(&mut self.asks, map.next_value()?),
            _ => skip(map),
        }
    }
}

impl<'de> de::Deserialize<'de> for BookData {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<BookData, D::Error> {
        let mut f = BookFields::default();
        Object(&mut f).deserialize(d)?;
        Ok(BookData {
            u: missing::<_, D::Error>(f.u, "u")?.0,
            bids: f.bids.map(|l| l.0).unwrap_or_default(),
            asks: f.asks.map(|l| l.0).unwrap_or_default(),
        })
    }
}

// ---------------------------------------------------------------------------------------------
// `publicTrade.*` data
// ---------------------------------------------------------------------------------------------

/// The `data` array: the FIRST element becomes the tick, the rest are validated and dropped.
struct FirstTrade<'s> {
    symbol: &'s str,
}

impl<'de> DeserializeSeed<'de> for FirstTrade<'_> {
    type Value = TradeTick;
    fn deserialize<D: Deserializer<'de>>(self, d: D) -> Result<TradeTick, D::Error> {
        d.deserialize_seq(self)
    }
}

impl<'de> Visitor<'de> for FirstTrade<'_> {
    type Value = TradeTick;
    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a non-empty array of trades")
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<TradeTick, A::Error> {
        let first = seq
            .next_element_seed(TradeSeed { symbol: self.symbol })?
            .ok_or_else(|| decline::<A::Error>("empty trade array"))?;
        while seq.next_element::<Skip>()?.is_some() {}
        Ok(first)
    }
}

struct TradeSeed<'s> {
    symbol: &'s str,
}

#[derive(Default)]
struct TradeFields<'de> {
    ts: Option<I64>,
    price: Option<DecimalStr>,
    size: Option<DecimalStr>,
    side: Option<StrRef<'de>>,
}

impl<'de> Fields<'de> for TradeFields<'de> {
    fn field<A: MapAccess<'de>>(&mut self, key: &str, map: &mut A) -> Result<(), A::Error> {
        match key {
            "T" => set(&mut self.ts, map.next_value()?),
            "p" => set(&mut self.price, map.next_value()?),
            "v" => set(&mut self.size, map.next_value()?),
            "S" => set(&mut self.side, map.next_value()?),
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
            size: missing::<_, D::Error>(f.size, "v")?.0,
            // `S` is the taker side, so a taker "Sell" means the buyer was the maker.
            is_buyer_maker: f.side.is_some_and(|s| s.0 == "Sell"),
            symbol: self.symbol.to_string(),
        })
    }
}

#[path = "market_scan_tests.rs"]
#[cfg(test)]
mod market_scan_tests;
