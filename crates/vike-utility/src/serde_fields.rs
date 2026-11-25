//! The field NAMES a derived `Deserialize` reads, asked of the derive itself, so a list of a struct's
//! keys is never written by hand beside the struct it must agree with.
//!
//! A `#[derive(Deserialize)]` struct hands its whole field list to `Deserializer::deserialize_struct`:
//! fields in declaration order, each field's own name and its aliases (serde sorts one field's names
//! among themselves), a `#[serde(rename)]` under the name the input carries, a `#[serde(skip)]` field
//! left out. [`declared_fields`] runs the derive against a deserializer that keeps that argument and
//! refuses everything else, so the answer is the derive's own: a renamed, added or removed field
//! changes it with no second edit. [`field_groups`] cuts the same list into one group per field, by
//! asking the derive which field each name reads into.
//!
//! Behind the off-by-default `serde` feature, whose one dependency is `serde`.

use serde::de::{self, DeserializeOwned, DeserializeSeed, MapAccess, Visitor};

/// The names `T`'s derived `Deserialize` reads, in declaration order; empty for a `T` that does not
/// deserialize as a struct with named fields (a map, an enum, a newtype), which the question does not fit.
#[must_use]
pub fn declared_fields<T: DeserializeOwned>() -> &'static [&'static str] {
    let mut probe = Probe { fields: &[], key: None, field: None };
    // The probe always errors (it builds no value); what it leaves behind is the answer.
    let _ = T::deserialize(&mut probe);
    probe.fields
}

/// [`declared_fields`], one group per field: the field's own name and its aliases, in the order the
/// derive lists them. The flat list alone cannot say where one field's names end, because serde sorts
/// each field's names: `zeta` with the alias `alpha` lists as `alpha, zeta`.
#[must_use]
pub fn field_groups<T: DeserializeOwned>() -> Vec<Vec<&'static str>> {
    let mut groups: Vec<(Option<String>, Vec<&'static str>)> = Vec::new();
    for &name in declared_fields::<T>() {
        let mut probe = Probe { fields: &[], key: Some(name), field: None };
        let _ = T::deserialize(&mut probe);
        let field = probe.field;
        if let Some((_, names)) =
            groups.last_mut().filter(|(last, _)| last.is_some() && *last == field)
        {
            names.push(name);
        } else {
            groups.push((field, vec![name]));
        }
    }
    groups.into_iter().map(|(_, names)| names).collect()
}

/// A deserializer that records the `fields` of `deserialize_struct` and, holding a `key`, which field
/// the derive reads that key into (its field enum's variant), and refuses every other request.
struct Probe {
    fields: &'static [&'static str],
    key: Option<&'static str>,
    field: Option<String>,
}

impl<'de> de::Deserializer<'de> for &mut Probe {
    type Error = de::value::Error;

    fn deserialize_any<V: Visitor<'de>>(self, _visitor: V) -> Result<V::Value, Self::Error> {
        Err(de::Error::custom("the probe answers deserialize_struct only"))
    }

    fn deserialize_struct<V: Visitor<'de>>(
        self,
        _name: &'static str,
        fields: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, Self::Error> {
        self.fields = fields;
        if self.key.is_some() {
            return visitor.visit_map(self);
        }
        Err(de::Error::custom("the probe records the field list and builds nothing"))
    }

    serde::forward_to_deserialize_any! {
        bool i8 i16 i32 i64 i128 u8 u16 u32 u64 u128 f32 f64 char str string bytes byte_buf option
        unit unit_struct newtype_struct seq tuple tuple_struct map enum identifier ignored_any
    }
}

impl<'de> MapAccess<'de> for &mut Probe {
    type Error = de::value::Error;

    fn next_key_seed<K: DeserializeSeed<'de>>(
        &mut self,
        seed: K,
    ) -> Result<Option<K::Value>, Self::Error> {
        let Some(key) = self.key.take() else {
            return Err(de::Error::custom("the probe offers one key"));
        };
        let field = seed.deserialize(de::value::StrDeserializer::<Self::Error>::new(key))?;
        // The derive's field enum: two names read into one field exactly when the variant is the same.
        self.field = Some(format!("{:?}", std::mem::discriminant(&field)));
        Err(de::Error::custom("the field a name reads into is all the probe asks"))
    }

    fn next_value_seed<V: DeserializeSeed<'de>>(
        &mut self,
        _seed: V,
    ) -> Result<V::Value, Self::Error> {
        Err(de::Error::custom("the probe reads no value"))
    }
}

#[cfg(test)]
#[path = "serde_fields_tests.rs"]
mod tests;
