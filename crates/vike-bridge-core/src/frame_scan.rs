//! Strict, borrowing JSON scanning for the per-frame market-data hot path: the building blocks of a
//! typed decode that never builds a `serde_json::Value` tree.
//!
//! # Contract: declines instead of deciding
//!
//! Every reader here either returns EXACTLY what the `Value`-based decoder would have read, or
//! returns an `Err`, and the caller (a fast path in front of the unchanged `Value` decoder) then
//! runs that decoder. So a wrong type, a missing or repeated key, an escaped string, an extra
//! array element and an unparseable number are all "decline", never a guess; the behaviour on such
//! a frame is the old decoder's by construction.
//!
//! # Why a hand-written `Visitor` and not `#[derive(Deserialize)]`
//!
//! The derive would break the "identical on every input" promise in two ways, both measured against
//! `serde_json`'s sources rather than assumed:
//!
//! * a derived struct also accepts a JSON ARRAY (positional `visit_seq`), so `["x", {..}]` would
//!   decode as `{stream, data}` where the `Value` decoder sees no `stream` key at all;
//! * a derived struct skips unknown fields with `IgnoredAny`, whose string and number skipper
//!   validates LESS than `Value` does (a lone `\ud800` escape and an out-of-range `1e999` are
//!   accepted when skipped and rejected by `Value`'s parse), so a frame the `Value` decoder drops
//!   as invalid JSON would decode.
//!
//! So an unknown field is consumed by [`Skip`], which goes through `deserialize_any` like `Value`
//! does and therefore validates identically (escapes, number range, recursion limit), and every
//! reader uses `deserialize_any` with a visitor that accepts one shape.
//!
//! # One home for every venue bridge
//!
//! binance (the whole family) and bybit scan their frames with these blocks. A venue bridge may not
//! name another (`layer_gate`), so the blocks live here, in the shared crate below both, and the
//! two cannot drift apart.

use std::borrow::Cow;
use std::fmt;

use serde::de::{self, Deserialize, DeserializeSeed, Deserializer, MapAccess, SeqAccess, Visitor};
use vike_model::BookLevel;

/// Any JSON value, consumed and validated the way `serde_json::Value` parses it.
pub struct Skip;

impl<'de> Deserialize<'de> for Skip {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        d.deserialize_any(SkipVisitor)
    }
}

struct SkipVisitor;

impl<'de> Visitor<'de> for SkipVisitor {
    type Value = Skip;
    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("any JSON value")
    }
    fn visit_bool<E: de::Error>(self, _: bool) -> Result<Skip, E> {
        Ok(Skip)
    }
    fn visit_i64<E: de::Error>(self, _: i64) -> Result<Skip, E> {
        Ok(Skip)
    }
    fn visit_u64<E: de::Error>(self, _: u64) -> Result<Skip, E> {
        Ok(Skip)
    }
    fn visit_f64<E: de::Error>(self, _: f64) -> Result<Skip, E> {
        Ok(Skip)
    }
    fn visit_str<E: de::Error>(self, _: &str) -> Result<Skip, E> {
        Ok(Skip)
    }
    fn visit_unit<E: de::Error>(self) -> Result<Skip, E> {
        Ok(Skip)
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Skip, A::Error> {
        while seq.next_element::<Skip>()?.is_some() {}
        Ok(Skip)
    }
    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Skip, A::Error> {
        while map.next_key::<Skip>()?.is_some() {
            map.next_value::<Skip>()?;
        }
        Ok(Skip)
    }
}

/// An object key, borrowed from the input unless it carried an escape.
pub struct Key<'de>(pub Cow<'de, str>);

impl<'de> Deserialize<'de> for Key<'de> {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = Key<'de>;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("an object key")
            }
            fn visit_borrowed_str<E: de::Error>(self, v: &'de str) -> Result<Key<'de>, E> {
                Ok(Key(Cow::Borrowed(v)))
            }
            fn visit_str<E: de::Error>(self, v: &str) -> Result<Key<'de>, E> {
                Ok(Key(Cow::Owned(v.to_owned())))
            }
        }
        d.deserialize_str(V)
    }
}

/// A JSON string with no escape in it, borrowed from the input. An escaped string DECLINES (the
/// decoded text would need an allocation, and no venue sends one in these fields).
pub struct StrRef<'de>(pub &'de str);

impl<'de> Deserialize<'de> for StrRef<'de> {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = StrRef<'de>;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("an unescaped JSON string")
            }
            fn visit_borrowed_str<E: de::Error>(self, v: &'de str) -> Result<StrRef<'de>, E> {
                Ok(StrRef(v))
            }
        }
        d.deserialize_any(V)
    }
}

/// A non-negative JSON integer, as `Value::as_u64` reads it (a float, a negative number and
/// anything that is not a number decline).
pub struct U64(pub u64);

impl<'de> Deserialize<'de> for U64 {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl Visitor<'_> for V {
            type Value = U64;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a non-negative integer")
            }
            fn visit_u64<E: de::Error>(self, v: u64) -> Result<U64, E> {
                Ok(U64(v))
            }
        }
        d.deserialize_any(V)
    }
}

/// A JSON integer that fits an `i64`, as `Value::as_i64` reads it (a float and anything else
/// decline).
pub struct I64(pub i64);

impl<'de> Deserialize<'de> for I64 {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl Visitor<'_> for V {
            type Value = I64;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("an i64 integer")
            }
            fn visit_i64<E: de::Error>(self, v: i64) -> Result<I64, E> {
                Ok(I64(v))
            }
            fn visit_u64<E: de::Error>(self, v: u64) -> Result<I64, E> {
                i64::try_from(v).map(I64).map_err(E::custom)
            }
        }
        d.deserialize_any(V)
    }
}

/// A string holding a decimal number, parsed with `str::parse::<f64>` exactly as the `Value`
/// decoders' `f()` does (so `"NaN"` and `"inf"` parse to the same non-finite values). An escaped
/// string or an unparseable one declines.
pub struct DecimalStr(pub f64);

impl<'de> Deserialize<'de> for DecimalStr {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = StrRef::deserialize(d)?;
        s.0.parse::<f64>().map(DecimalStr).map_err(de::Error::custom)
    }
}

/// A `[[price, qty], …]` side of a book: every level exactly two unescaped decimal strings.
/// The `Value` reader (`vike_bridge_core::depth::parse_levels`) SKIPS a malformed level; this one
/// declines the whole frame instead, so the skip stays the old reader's behaviour.
pub struct Levels(pub Vec<BookLevel>);

impl<'de> Deserialize<'de> for Levels {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = Levels;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("an array of [price, qty] string pairs")
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Levels, A::Error> {
                let mut out = Vec::with_capacity(32);
                while let Some(Level(l)) = seq.next_element::<Level>()? {
                    out.push(l);
                }
                Ok(Levels(out))
            }
        }
        d.deserialize_any(V)
    }
}

struct Level(BookLevel);

impl<'de> Deserialize<'de> for Level {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = Level;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a [price, qty] pair")
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Level, A::Error> {
                let price = seq
                    .next_element::<DecimalStr>()?
                    .ok_or_else(|| <A::Error as de::Error>::invalid_length(0, &self))?;
                let qty = seq
                    .next_element::<DecimalStr>()?
                    .ok_or_else(|| <A::Error as de::Error>::invalid_length(1, &self))?;
                if seq.next_element::<Skip>()?.is_some() {
                    return Err(<A::Error as de::Error>::invalid_length(3, &self));
                }
                Ok(Level(BookLevel { price: price.0, qty: qty.0 }))
            }
        }
        d.deserialize_any(V)
    }
}

/// Where the fields of one JSON object go. [`Fields::field`] MUST consume the value of every key
/// it is handed (`map.next_value::<Skip>()` for the ones it does not want).
pub trait Fields<'de> {
    fn field<A: MapAccess<'de>>(&mut self, key: &str, map: &mut A) -> Result<(), A::Error>;
}

/// Store a field's value; a REPEATED key declines (the `Value` decoder would keep the last one,
/// and a venue frame never repeats a key).
pub fn set<T, E: de::Error>(slot: &mut Option<T>, v: T) -> Result<(), E> {
    if slot.replace(v).is_some() { Err(E::custom("duplicate key")) } else { Ok(()) }
}

/// A required field that never arrived.
pub fn missing<T, E: de::Error>(slot: Option<T>, name: &'static str) -> Result<T, E> {
    slot.ok_or_else(|| E::missing_field(name))
}

/// Drive `fields` over ONE JSON object. Anything but an object (an array, a scalar, `null`)
/// declines.
pub struct Object<'f, F>(pub &'f mut F);

impl<'de, F: Fields<'de>> DeserializeSeed<'de> for Object<'_, F> {
    type Value = ();
    fn deserialize<D: Deserializer<'de>>(self, d: D) -> Result<(), D::Error> {
        struct V<'f, F>(&'f mut F);
        impl<'de, F: Fields<'de>> Visitor<'de> for V<'_, F> {
            type Value = ();
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a JSON object")
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<(), A::Error> {
                while let Some(Key(k)) = map.next_key::<Key<'de>>()? {
                    self.0.field(&k, &mut map)?;
                }
                Ok(())
            }
        }
        d.deserialize_map(V(self.0))
    }
}

/// Consume (and validate) the value of a key nobody wants.
pub fn skip<'de, A: MapAccess<'de>>(map: &mut A) -> Result<(), A::Error> {
    map.next_value::<Skip>().map(|_skipped| ())
}
