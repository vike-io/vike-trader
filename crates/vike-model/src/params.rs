//! Strategy construction parameters — the value a strategy's `from_params` reads, as DOMAIN vocabulary.
//!
//! A strategy is built from a free-form bag of named knobs (`[strategy.params]` of a profile, a
//! mount row, a sweep point, a preset). Until this module existed that bag was `toml::Value`, so a
//! FORMAT LIBRARY's type crossed every crate boundary. This module is the one vocabulary: TOML and
//! JSON are TEXT EDGES that serde-read into it and serde-write out of it, and nothing past an edge
//! names `toml`.
//!
//! ## Shape
//!
//! [`ParamValue`] is `Int(i64)`, `Float(f64)`, `Bool`, `Str`, `List` or `Table(`[`Params`]`)`.
//! **`Int` and `Float` are distinct kinds and stay so**: a strategy knob read with
//! [`ParamValue::as_integer`] (`rungs`, `exit_delay_ms`, `cooldown_ms`) takes `Int` only, so a value
//! stored as `Float` is ABSENT to it. There is no datetime kind: no reader reads one, and an edge
//! that meets one REFUSES it. [`Params`] is a SORTED map; every consumer already assumed sorted
//! iteration, and a sorted map reproduces the bytes the old `toml::map::Map` produced.
//!
//! ## Readers
//!
//! The accessors carry `toml::Value`'s names (`get`, `as_str`, `as_integer`, `as_float`, `as_bool`,
//! `as_array`, `as_table`) so a reader body keeps compiling and `param_keys_gate`, which matches
//! accessor names as TEXT, keeps reading the same idioms. [`ParamValue::as_f64`] is the ONE lenient
//! number read: a float, or an integer widened to one — never the reverse.
//!
//! ## Text forms
//!
//! * serde is hand-written and untagged, so the JSON and the TOML form of a value are byte-identical
//!   to what `toml::Value` produced (`2` is an integer, `2.0` a float, a table an object). A stored
//!   ledger, a sweep report and a mount sidecar therefore read back with no migration. `null`,
//!   an integer above `i64::MAX` and a TOML datetime are REFUSED.
//! * [`std::fmt::Display`] renders a value as TOML text (strings quoted, a float always with a
//!   `.` or exponent, a table as an inline table), the form a `mount_param` row stores.
//!
//! ## Equality
//!
//! `PartialEq`, `Eq` and `Hash` compare a float by its BIT PATTERN (`0.0 != -0.0`, `NaN == NaN`), so
//! a value can key a map directly. This is a deliberate departure from `f64`'s `==`.

use std::collections::BTreeMap;
use std::collections::btree_map;
use std::fmt;
use std::hash::{Hash, Hasher};

use indexmap::IndexMap;
use serde::de::{self, Deserialize, Deserializer, MapAccess, SeqAccess, Visitor};
use serde::ser::{Serialize, SerializeMap, SerializeSeq, Serializer};

/// One parameter value.
#[derive(Debug, Clone)]
pub enum ParamValue {
    Int(i64),
    Float(f64),
    Bool(bool),
    Str(String),
    List(Vec<ParamValue>),
    Table(Params),
}

/// A sorted table of named parameters.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct Params(BTreeMap<String, ParamValue>);

impl ParamValue {
    /// An empty table — the value of a strategy that sets no knob.
    pub fn empty_table() -> Self {
        Self::Table(Params::new())
    }

    /// The kind's name, spelled as `toml::Value::type_str` spells it: `string`, `integer`, `float`,
    /// `boolean`, `array`, `table`. `ParamType`'s refusals and their tests quote these words.
    pub fn type_str(&self) -> &'static str {
        match self {
            Self::Int(_) => "integer",
            Self::Float(_) => "float",
            Self::Bool(_) => "boolean",
            Self::Str(_) => "string",
            Self::List(_) => "array",
            Self::Table(_) => "table",
        }
    }

    /// The entry under `key` when this value is a table; `None` for any other kind.
    pub fn get(&self, key: &str) -> Option<&ParamValue> {
        match self {
            Self::Table(t) => t.get(key),
            _ => None,
        }
    }

    /// Mutable form of [`Self::get`].
    pub fn get_mut(&mut self, key: &str) -> Option<&mut ParamValue> {
        match self {
            Self::Table(t) => t.get_mut(key),
            _ => None,
        }
    }

    /// The text, for a `Str`.
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::Str(s) => Some(s),
            _ => None,
        }
    }

    /// The integer, for an `Int` ONLY — a `Float` is `None`.
    pub fn as_integer(&self) -> Option<i64> {
        match self {
            Self::Int(i) => Some(*i),
            _ => None,
        }
    }

    /// The float, for a `Float` ONLY — an `Int` is `None`.
    pub fn as_float(&self) -> Option<f64> {
        match self {
            Self::Float(f) => Some(*f),
            _ => None,
        }
    }

    /// THE lenient number read: a `Float`, or an `Int` widened to `f64`. Never the reverse.
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Self::Float(f) => Some(*f),
            Self::Int(i) => Some(*i as f64),
            _ => None,
        }
    }

    /// The boolean, for a `Bool`.
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Self::Bool(b) => Some(*b),
            _ => None,
        }
    }

    /// The items, for a `List`.
    pub fn as_array(&self) -> Option<&Vec<ParamValue>> {
        match self {
            Self::List(v) => Some(v),
            _ => None,
        }
    }

    /// Mutable form of [`Self::as_array`].
    pub fn as_array_mut(&mut self) -> Option<&mut Vec<ParamValue>> {
        match self {
            Self::List(v) => Some(v),
            _ => None,
        }
    }

    /// The table, for a `Table`.
    pub fn as_table(&self) -> Option<&Params> {
        match self {
            Self::Table(t) => Some(t),
            _ => None,
        }
    }

    /// Mutable form of [`Self::as_table`].
    pub fn as_table_mut(&mut self) -> Option<&mut Params> {
        match self {
            Self::Table(t) => Some(t),
            _ => None,
        }
    }

    /// `true` for a `Table`.
    pub fn is_table(&self) -> bool {
        matches!(self, Self::Table(_))
    }

    /// `true` for a `Str`.
    pub fn is_str(&self) -> bool {
        matches!(self, Self::Str(_))
    }

    /// `true` when this value, or any value nested in it, is a table holding `key`. The "reserved
    /// key at any depth" belt (decision 0064) walks with this.
    pub fn contains_key_at_any_depth(&self, key: &str) -> bool {
        match self {
            Self::Table(t) => t.contains_key_at_any_depth(key),
            Self::List(items) => items.iter().any(|v| v.contains_key_at_any_depth(key)),
            _ => false,
        }
    }
}

impl Params {
    /// An empty table.
    pub fn new() -> Self {
        Self(BTreeMap::new())
    }

    /// The entry under `key`.
    pub fn get(&self, key: &str) -> Option<&ParamValue> {
        self.0.get(key)
    }

    /// Mutable form of [`Self::get`].
    pub fn get_mut(&mut self, key: &str) -> Option<&mut ParamValue> {
        self.0.get_mut(key)
    }

    /// Insert `value` under `key`; returns what it replaced.
    pub fn insert(
        &mut self,
        key: impl Into<String>,
        value: impl Into<ParamValue>,
    ) -> Option<ParamValue> {
        self.0.insert(key.into(), value.into())
    }

    /// Remove `key`; returns what it held.
    pub fn remove(&mut self, key: &str) -> Option<ParamValue> {
        self.0.remove(key)
    }

    /// `true` when `key` is set at THIS level.
    pub fn contains_key(&self, key: &str) -> bool {
        self.0.contains_key(key)
    }

    /// The entry API, for an update-or-insert.
    pub fn entry(&mut self, key: impl Into<String>) -> btree_map::Entry<'_, String, ParamValue> {
        self.0.entry(key.into())
    }

    /// Number of entries at this level.
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// `true` when no knob is set.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Entries in key order.
    pub fn iter(&self) -> btree_map::Iter<'_, String, ParamValue> {
        self.0.iter()
    }

    /// Mutable entries in key order.
    pub fn iter_mut(&mut self) -> btree_map::IterMut<'_, String, ParamValue> {
        self.0.iter_mut()
    }

    /// Keys in order.
    pub fn keys(&self) -> btree_map::Keys<'_, String, ParamValue> {
        self.0.keys()
    }

    /// Values in key order.
    pub fn values(&self) -> btree_map::Values<'_, String, ParamValue> {
        self.0.values()
    }

    /// `true` when `key` is set at this level or in any table nested below (through lists too).
    pub fn contains_key_at_any_depth(&self, key: &str) -> bool {
        self.0.contains_key(key) || self.0.values().any(|v| v.contains_key_at_any_depth(key))
    }

    /// The numeric narrowing a Rhai script receives: every `Float` and `Int` entry as an `f64`, in
    /// key order, and the keys that are NOT numbers (a string, a bool, a list, a table) so the
    /// caller can REPORT what it dropped instead of dropping it silently.
    pub fn numeric(&self) -> (IndexMap<String, f64>, Vec<String>) {
        let mut numbers = IndexMap::new();
        let mut dropped = Vec::new();
        for (k, v) in &self.0 {
            match v.as_f64() {
                Some(n) => {
                    numbers.insert(k.clone(), n);
                }
                None => dropped.push(k.clone()),
            }
        }
        (numbers, dropped)
    }
}

impl IntoIterator for Params {
    type Item = (String, ParamValue);
    type IntoIter = btree_map::IntoIter<String, ParamValue>;
    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
    }
}

impl<'a> IntoIterator for &'a Params {
    type Item = (&'a String, &'a ParamValue);
    type IntoIter = btree_map::Iter<'a, String, ParamValue>;
    fn into_iter(self) -> Self::IntoIter {
        self.0.iter()
    }
}

impl<'a> IntoIterator for &'a mut Params {
    type Item = (&'a String, &'a mut ParamValue);
    type IntoIter = btree_map::IterMut<'a, String, ParamValue>;
    fn into_iter(self) -> Self::IntoIter {
        self.0.iter_mut()
    }
}

impl<K: Into<String>, V: Into<ParamValue>> FromIterator<(K, V)> for Params {
    fn from_iter<I: IntoIterator<Item = (K, V)>>(iter: I) -> Self {
        Self(iter.into_iter().map(|(k, v)| (k.into(), v.into())).collect())
    }
}

impl<K: Into<String>, V: Into<ParamValue>> Extend<(K, V)> for Params {
    fn extend<I: IntoIterator<Item = (K, V)>>(&mut self, iter: I) {
        self.0.extend(iter.into_iter().map(|(k, v)| (k.into(), v.into())));
    }
}

impl From<i64> for ParamValue {
    fn from(v: i64) -> Self {
        Self::Int(v)
    }
}
impl From<f64> for ParamValue {
    fn from(v: f64) -> Self {
        Self::Float(v)
    }
}
impl From<bool> for ParamValue {
    fn from(v: bool) -> Self {
        Self::Bool(v)
    }
}
impl From<&str> for ParamValue {
    fn from(v: &str) -> Self {
        Self::Str(v.to_string())
    }
}
impl From<String> for ParamValue {
    fn from(v: String) -> Self {
        Self::Str(v)
    }
}
impl From<Vec<ParamValue>> for ParamValue {
    fn from(v: Vec<ParamValue>) -> Self {
        Self::List(v)
    }
}
impl From<Params> for ParamValue {
    fn from(v: Params) -> Self {
        Self::Table(v)
    }
}

// ---- equality: floats by bit pattern, so a value can key a map ----------------------------------

impl PartialEq for ParamValue {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Int(a), Self::Int(b)) => a == b,
            (Self::Float(a), Self::Float(b)) => a.to_bits() == b.to_bits(),
            (Self::Bool(a), Self::Bool(b)) => a == b,
            (Self::Str(a), Self::Str(b)) => a == b,
            (Self::List(a), Self::List(b)) => a == b,
            (Self::Table(a), Self::Table(b)) => a == b,
            _ => false,
        }
    }
}

impl Eq for ParamValue {}

impl Hash for ParamValue {
    fn hash<H: Hasher>(&self, state: &mut H) {
        std::mem::discriminant(self).hash(state);
        match self {
            Self::Int(i) => i.hash(state),
            Self::Float(f) => f.to_bits().hash(state),
            Self::Bool(b) => b.hash(state),
            Self::Str(s) => s.hash(state),
            Self::List(v) => v.hash(state),
            Self::Table(t) => t.hash(state),
        }
    }
}

// ---- TOML text of one value ---------------------------------------------------------------------

impl fmt::Display for ParamValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Int(i) => write!(f, "{i}"),
            Self::Float(x) => write_float(f, *x),
            Self::Bool(b) => write!(f, "{b}"),
            Self::Str(s) => write_basic_string(f, s),
            Self::List(items) => {
                f.write_str("[")?;
                for (i, v) in items.iter().enumerate() {
                    if i > 0 {
                        f.write_str(", ")?;
                    }
                    write!(f, "{v}")?;
                }
                f.write_str("]")
            }
            Self::Table(t) => write!(f, "{t}"),
        }
    }
}

/// An INLINE table: `{ a = 1, b = "x" }`, or `{}` when empty.
impl fmt::Display for Params {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.0.is_empty() {
            return f.write_str("{}");
        }
        f.write_str("{ ")?;
        for (i, (k, v)) in self.0.iter().enumerate() {
            if i > 0 {
                f.write_str(", ")?;
            }
            write_key(f, k)?;
            write!(f, " = {v}")?;
        }
        f.write_str(" }")
    }
}

fn write_float(f: &mut fmt::Formatter<'_>, x: f64) -> fmt::Result {
    if x.is_nan() {
        return f.write_str("nan");
    }
    if x.is_infinite() {
        return f.write_str(if x > 0.0 { "inf" } else { "-inf" });
    }
    let s = format!("{x}");
    f.write_str(&s)?;
    // `format!("{}", 2.0)` is `2`, which TOML reads as an INTEGER: a float always keeps its kind.
    if !s.contains(['.', 'e', 'E']) {
        f.write_str(".0")?;
    }
    Ok(())
}

fn write_basic_string(f: &mut fmt::Formatter<'_>, s: &str) -> fmt::Result {
    f.write_str("\"")?;
    for c in s.chars() {
        match c {
            '"' => f.write_str("\\\"")?,
            '\\' => f.write_str("\\\\")?,
            '\n' => f.write_str("\\n")?,
            '\r' => f.write_str("\\r")?,
            '\t' => f.write_str("\\t")?,
            '\u{8}' => f.write_str("\\b")?,
            '\u{c}' => f.write_str("\\f")?,
            c if (c as u32) < 0x20 || c as u32 == 0x7f => write!(f, "\\u{:04X}", c as u32)?,
            c => write!(f, "{c}")?,
        }
    }
    f.write_str("\"")
}

/// A bare key when it is `[A-Za-z0-9_-]+`, else a quoted one — a key that needs quoting must not be
/// rendered bare.
fn write_key(f: &mut fmt::Formatter<'_>, k: &str) -> fmt::Result {
    let bare =
        !k.is_empty() && k.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-');
    if bare { f.write_str(k) } else { write_basic_string(f, k) }
}

// ---- serde: untagged, byte-identical to `toml::Value` -------------------------------------------

impl Serialize for ParamValue {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Int(i) => s.serialize_i64(*i),
            Self::Float(x) => s.serialize_f64(*x),
            Self::Bool(b) => s.serialize_bool(*b),
            Self::Str(v) => s.serialize_str(v),
            Self::List(items) => {
                let mut seq = s.serialize_seq(Some(items.len()))?;
                for item in items {
                    seq.serialize_element(item)?;
                }
                seq.end()
            }
            Self::Table(t) => t.serialize(s),
        }
    }
}

impl Serialize for Params {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut map = s.serialize_map(Some(self.0.len()))?;
        for (k, v) in &self.0 {
            map.serialize_entry(k, v)?;
        }
        map.end()
    }
}

/// `toml`'s deserializer hands a datetime over as a one-key map under this name.
const TOML_DATETIME_KEY: &str = "$__toml_private_datetime";

struct ParamValueVisitor;

impl<'de> Visitor<'de> for ParamValueVisitor {
    type Value = ParamValue;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a parameter value (integer, float, bool, string, array or table)")
    }

    fn visit_bool<E: de::Error>(self, v: bool) -> Result<ParamValue, E> {
        Ok(ParamValue::Bool(v))
    }

    fn visit_i64<E: de::Error>(self, v: i64) -> Result<ParamValue, E> {
        Ok(ParamValue::Int(v))
    }

    fn visit_u64<E: de::Error>(self, v: u64) -> Result<ParamValue, E> {
        i64::try_from(v)
            .map(ParamValue::Int)
            .map_err(|_| E::custom("integer above i64::MAX is not a parameter value"))
    }

    fn visit_f64<E: de::Error>(self, v: f64) -> Result<ParamValue, E> {
        Ok(ParamValue::Float(v))
    }

    fn visit_str<E: de::Error>(self, v: &str) -> Result<ParamValue, E> {
        Ok(ParamValue::Str(v.to_string()))
    }

    fn visit_string<E: de::Error>(self, v: String) -> Result<ParamValue, E> {
        Ok(ParamValue::Str(v))
    }

    fn visit_unit<E: de::Error>(self) -> Result<ParamValue, E> {
        Err(E::custom("null is not a parameter value"))
    }

    fn visit_none<E: de::Error>(self) -> Result<ParamValue, E> {
        Err(E::custom("null is not a parameter value"))
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<ParamValue, A::Error> {
        let mut items = Vec::with_capacity(seq.size_hint().unwrap_or(0).min(1024));
        while let Some(item) = seq.next_element()? {
            items.push(item);
        }
        Ok(ParamValue::List(items))
    }

    fn visit_map<A: MapAccess<'de>>(self, map: A) -> Result<ParamValue, A::Error> {
        visit_table(map).map(ParamValue::Table)
    }
}

fn visit_table<'de, A: MapAccess<'de>>(mut map: A) -> Result<Params, A::Error> {
    let mut table = Params::new();
    while let Some(key) = map.next_key::<String>()? {
        if key == TOML_DATETIME_KEY {
            return Err(de::Error::custom("a TOML datetime is not a parameter value"));
        }
        let value: ParamValue = map.next_value()?;
        if table.0.contains_key(&key) {
            return Err(de::Error::custom(format!("duplicate key `{key}`")));
        }
        table.0.insert(key, value);
    }
    Ok(table)
}

impl<'de> Deserialize<'de> for ParamValue {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        d.deserialize_any(ParamValueVisitor)
    }
}

impl<'de> Deserialize<'de> for Params {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct TableVisitor;
        impl<'de> Visitor<'de> for TableVisitor {
            type Value = Params;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a table of parameters")
            }
            fn visit_map<A: MapAccess<'de>>(self, map: A) -> Result<Params, A::Error> {
                visit_table(map)
            }
        }
        d.deserialize_map(TableVisitor)
    }
}

#[cfg(test)]
#[path = "params_tests.rs"]
mod tests;
