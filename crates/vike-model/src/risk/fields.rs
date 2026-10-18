//! The field NAMES serde's own derive reads for a struct, asked of the derive itself — so a list
//! of a struct's keys is never written by hand beside the struct it must agree with.
//!
//! A `#[derive(Deserialize)]` struct hands its whole field list (serialized names, declaration
//! order) to `Deserializer::deserialize_struct`. [`struct_fields`] runs the derive against a
//! deserializer that keeps that argument and refuses everything else, so the answer is the derive's
//! own `FIELDS` constant: a renamed, added or removed field changes it with no second edit, and a
//! `#[serde(rename)]` is reported under the name a TOML table actually uses.

use serde::de::{self, DeserializeOwned, Visitor};

/// The field names `T`'s derived `Deserialize` reads, in declaration order.
///
/// # Panics
///
/// When `T` does not deserialize as a STRUCT (a map, an enum, a newtype): the question has no
/// answer for it, and every caller asks it of a derived struct.
#[must_use]
pub(crate) fn struct_fields<T: DeserializeOwned>() -> &'static [&'static str] {
    let mut fields = None;
    // The probe always errors (it builds no value); what it leaves behind is the answer.
    let _ = T::deserialize(FieldsProbe(&mut fields));
    fields.expect("struct_fields asks a derived STRUCT, which calls deserialize_struct")
}

/// A deserializer that records the `fields` of `deserialize_struct` and refuses every request.
struct FieldsProbe<'a>(&'a mut Option<&'static [&'static str]>);

impl<'de> de::Deserializer<'de> for FieldsProbe<'_> {
    type Error = de::value::Error;

    fn deserialize_any<V: Visitor<'de>>(self, _visitor: V) -> Result<V::Value, Self::Error> {
        Err(de::Error::custom("FieldsProbe answers deserialize_struct only"))
    }

    fn deserialize_struct<V: Visitor<'de>>(
        self,
        _name: &'static str,
        fields: &'static [&'static str],
        _visitor: V,
    ) -> Result<V::Value, Self::Error> {
        *self.0 = Some(fields);
        Err(de::Error::custom("FieldsProbe records the field list and builds nothing"))
    }

    serde::forward_to_deserialize_any! {
        bool i8 i16 i32 i64 i128 u8 u16 u32 u64 u128 f32 f64 char str string bytes byte_buf option
        unit unit_struct newtype_struct seq tuple tuple_struct map enum identifier ignored_any
    }
}
