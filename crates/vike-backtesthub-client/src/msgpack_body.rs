//! **What a msgpack body carries that a JSON one does not** ("JSON only at the edges", 2026-11-19, negotiated by
//! [`vike_hub_protocol::features::FEATURE_WIRE_MSGPACK`]): the whole REPORTS without JSON inside JSON, and the long lists as COLUMNS. A
//! JSON body is today's bytes: everything here asks `is_human_readable()` first and is the derive's own form there.
//! [`write_reply`] is the backtesthub's one reply writer.
//!
//! # Reports
//!
//! The six text reports (`Response::Report`, `ParamscanReport`, `WalkforwardReport`,
//! `CombinatorialWalkforwardReport`, `PermutationReport`, `StudyReport`) are JSON TEXT the server made, and a JSON
//! frame carries each as one escaped string. A msgpack reply carries it one of two ways, by the link ([`write_reply`]):
//!
//! - **plain msgpack** (a client beside the daemon): the text in one msgpack string, no escaping; a copy each end.
//! - **LZ4** (a client on a thin link): the TREE ([`StructuredReports`]): every value in msgpack's own form, every key
//!   in the text's order, every long list as columns, which LZ4 compresses far better than the text. It costs a JSON
//!   parse at the daemon and a JSON write at the client, which the link repays many times over.
//!
//! MEASURED (`msgpack_body_tests.rs`'s `bytes_and_time_before_and_after`, release, a the CI box lane, 2026-11-19): with
//! every refusal sentence in it (before the denials were grouped), a 198 kB report took 177 µs to encode and decode
//! as JSON in JSON, 20 µs as text, 572 µs as the tree, and a 7.9 MB 40-point scan 27 ms, 2 ms and 64 ms; under LZ4
//! the scan was 349 kB as text and 48 kB as the tree. With the denials grouped the same scan is 74 kB as JSON, and
//! under LZ4 2,137 bytes as text (17 µs) against 1,996 as the tree (603 µs).
//!
//! The client reads either back into the VERY TEXT ([`report_text`]), so `vike-cli --json` prints, and vike-mcp hands
//! on, the bytes the server made: the text is `serde_json`'s own compact output, and a value `serde_json` reads
//! (`float_roundtrip`) and writes again is the same characters. `msgpack_body_tests.rs` holds it over every report
//! kind and random documents.
//!
//! ⚠ The CONTENT does not change here, only how it crosses (the owner has not ruled on the duplicate denials,
//! decision 0078). A report that does not parse as JSON, or is not an object or a list, crosses as its text.
//!
//! # Columns
//!
//! A list of two or more values, in a msgpack body:
//!
//! | list | crosses as |
//! |---|---|
//! | every value a float | `bin`: [`F64_COLUMN`], then each value's eight little-endian bytes |
//! | every value an integer in `i64` | `bin`: [`I64_COLUMN`], then each value's eight bytes, when that is shorter than msgpack's own |
//! | every value an object with the same keys in the same order | `[bin [TABLE], [keys], one column per key]` |
//! | every value a list of one length | `[bin [ROWS], one column per position]` |
//!
//! No JSON value is a `bin`, so a reader tells a column from a value by its type, and each column is a list that
//! columns again (a scan's rows of results of numbers). The typed lists that carry the most cross the same way: a
//! curve's `Vec<f64>` and `Vec<i64>` ([`f64_column`], [`i64_column`]), the trade lists and a scan's points
//! ([`table`], two msgpack passes, so only where a record's keys outweigh them: not a thinned curve's 2,000 pairs).
//! The sandboxed child's pipe writes these types in msgpack too, so its curve and points cross as columns there
//! as well (one binary on both ends).

use std::fmt;

use serde::de::{self, Deserializer, MapAccess, SeqAccess, Visitor};
use serde::ser::{self, SerializeMap, SerializeSeq, Serializer};
use serde::{Deserialize, Serialize};
use vike_hub_protocol::frame::{BodyCodec, write_reply_as};

use crate::proto::Response;

/// A `bin` column of floats: then each value's eight little-endian bytes.
const F64_COLUMN: u8 = 1;
/// A `bin` column of `i64`s: then each value's eight little-endian bytes.
const I64_COLUMN: u8 = 2;
/// The one-byte `bin` that opens a list of objects with the same keys: then the keys, then a column per key.
const TABLE: u8 = 3;
/// The one-byte `bin` that opens a list of lists of one length: then a column per position.
const ROWS: u8 = 4;

/// One JSON value, its object keys in the order the text wrote them (`serde_json::Value` sorts them).
#[derive(Debug)]
enum Tree {
    Null,
    Bool(bool),
    Unsigned(u64),
    Negative(i64),
    Float(f64),
    Text(String),
    List(Vec<Tree>),
    Object(Vec<(String, Tree)>),
    /// Read only, never a value: the head of a [`TABLE`] or a [`ROWS`] list.
    Head(u8),
}

impl Serialize for Tree {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            Tree::Null => s.serialize_unit(),
            Tree::Bool(v) => s.serialize_bool(*v),
            Tree::Unsigned(v) => s.serialize_u64(*v),
            Tree::Negative(v) => s.serialize_i64(*v),
            Tree::Float(v) => s.serialize_f64(*v),
            Tree::Text(v) => s.serialize_str(v),
            Tree::List(items) if s.is_human_readable() => s.collect_seq(items),
            Tree::List(items) => List(&items.iter().collect::<Vec<_>>()).serialize(s),
            Tree::Object(entries) => {
                let mut map = s.serialize_map(Some(entries.len()))?;
                for (key, value) in entries {
                    map.serialize_entry(key, value)?;
                }
                map.end()
            }
            Tree::Head(_) => Err(ser::Error::custom("a column head is not a value")),
        }
    }
}

/// A list in a msgpack body: as columns when the module doc's table says so, else value by value.
struct List<'a>(&'a [&'a Tree]);

impl Serialize for List<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let rows = self.0;
        if rows.len() >= 2 {
            if let Some(column) = number_column(rows) {
                return s.serialize_bytes(&column);
            }
            if let Some(keys) = same_keys(rows) {
                let mut seq = s.serialize_seq(Some(2 + keys.len()))?;
                seq.serialize_element(&Head(TABLE))?;
                seq.serialize_element(&keys)?;
                for at in 0..keys.len() {
                    seq.serialize_element(&List(&column_at(rows, at)))?;
                }
                return seq.end();
            }
            if let Some(width) = same_width(rows) {
                let mut seq = s.serialize_seq(Some(1 + width))?;
                seq.serialize_element(&Head(ROWS))?;
                for at in 0..width {
                    seq.serialize_element(&List(&column_at(rows, at)))?;
                }
                return seq.end();
            }
        }
        s.collect_seq(rows)
    }
}

/// A [`TABLE`] or [`ROWS`] head as it is written: a one-byte `bin`.
struct Head(u8);

impl Serialize for Head {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_bytes(&[self.0])
    }
}

/// `rows` as one [`F64_COLUMN`] or [`I64_COLUMN`] `bin`, or `None` when they are not all floats or all integers,
/// or an integer column would be longer than msgpack writes them.
fn number_column(rows: &[&Tree]) -> Option<Vec<u8>> {
    if rows.iter().all(|v| matches!(v, Tree::Float(_))) {
        let floats = rows.iter().map(|v| match v {
            Tree::Float(x) => x.to_le_bytes(),
            _ => unreachable!("every row is a float"),
        });
        return Some(words(F64_COLUMN, floats));
    }
    let integers: Vec<i64> = rows
        .iter()
        .map(|v| match v {
            Tree::Unsigned(u) => i64::try_from(*u).ok(),
            Tree::Negative(i) => Some(*i),
            _ => None,
        })
        .collect::<Option<_>>()?;
    let msgpack: usize = integers.iter().map(|v| msgpack_integer_len(*v)).sum();
    (8 * integers.len() < msgpack)
        .then(|| words(I64_COLUMN, integers.iter().map(|v| v.to_le_bytes())))
}

/// `kind`, then every eight-byte word: a column's `bin`.
fn words(kind: u8, values: impl ExactSizeIterator<Item = [u8; 8]>) -> Vec<u8> {
    let mut out = Vec::with_capacity(1 + 8 * values.len());
    out.push(kind);
    values.for_each(|word| out.extend_from_slice(&word));
    out
}

/// The bytes msgpack writes `v` in (`rmp::encode::write_sint`): what an integer column is weighed against.
fn msgpack_integer_len(v: i64) -> usize {
    let widths: [(std::ops::RangeInclusive<i64>, usize); 4] = [
        (-32..=127, 1),
        (-128..=255, 2),
        (-32_768..=65_535, 3),
        (-2_147_483_648..=4_294_967_295, 5),
    ];
    widths.into_iter().find_map(|(range, len)| range.contains(&v).then_some(len)).unwrap_or(9)
}

/// The keys every row shares in one order, when every row is an object with at least one key.
fn same_keys<'a>(rows: &[&'a Tree]) -> Option<Vec<&'a str>> {
    let Tree::Object(first) = rows[0] else { return None };
    let keys: Vec<&str> = first.iter().map(|(key, _)| key.as_str()).collect();
    let same = |row: &&Tree| match row {
        Tree::Object(entries) => {
            entries.len() == keys.len() && entries.iter().zip(&keys).all(|((key, _), k)| key == k)
        }
        _ => false,
    };
    (!keys.is_empty() && rows.iter().all(same)).then_some(keys)
}

/// The length every row shares, when every row is a non-empty list.
fn same_width(rows: &[&Tree]) -> Option<usize> {
    let Tree::List(first) = rows[0] else { return None };
    let width = first.len();
    let same = |row: &&Tree| matches!(row, Tree::List(items) if items.len() == width);
    (width > 0 && rows.iter().all(same)).then_some(width)
}

/// The `at`-th value of every row of a table or a list of rows.
fn column_at<'a>(rows: &[&'a Tree], at: usize) -> Vec<&'a Tree> {
    rows.iter()
        .map(|row| match row {
            Tree::Object(entries) => &entries[at].1,
            Tree::List(items) => &items[at],
            _ => unreachable!("a table's rows are objects, a ROWS list's are lists"),
        })
        .collect()
}

impl<'de> Deserialize<'de> for Tree {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        d.deserialize_any(TreeVisitor)
    }
}

/// Reads a [`Tree`] from JSON text or from a msgpack body, columns included.
struct TreeVisitor;

impl<'de> Visitor<'de> for TreeVisitor {
    type Value = Tree;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a JSON value, or a column of them")
    }

    fn visit_bool<E: de::Error>(self, v: bool) -> Result<Tree, E> {
        Ok(Tree::Bool(v))
    }

    fn visit_i64<E: de::Error>(self, v: i64) -> Result<Tree, E> {
        Ok(integer(v))
    }

    fn visit_u64<E: de::Error>(self, v: u64) -> Result<Tree, E> {
        Ok(Tree::Unsigned(v))
    }

    fn visit_f64<E: de::Error>(self, v: f64) -> Result<Tree, E> {
        Ok(Tree::Float(v))
    }

    fn visit_str<E: de::Error>(self, v: &str) -> Result<Tree, E> {
        Ok(Tree::Text(v.to_owned()))
    }

    fn visit_string<E: de::Error>(self, v: String) -> Result<Tree, E> {
        Ok(Tree::Text(v))
    }

    fn visit_unit<E: de::Error>(self) -> Result<Tree, E> {
        Ok(Tree::Null)
    }

    fn visit_none<E: de::Error>(self) -> Result<Tree, E> {
        Ok(Tree::Null)
    }

    fn visit_some<D: Deserializer<'de>>(self, d: D) -> Result<Tree, D::Error> {
        Tree::deserialize(d)
    }

    fn visit_bytes<E: de::Error>(self, bytes: &[u8]) -> Result<Tree, E> {
        let Some((&kind, rest)) = bytes.split_first() else {
            return Err(E::custom("an empty column"));
        };
        match kind {
            TABLE | ROWS if rest.is_empty() => Ok(Tree::Head(kind)),
            F64_COLUMN | I64_COLUMN if rest.len().is_multiple_of(8) => Ok(Tree::List(
                rest.as_chunks::<8>()
                    .0
                    .iter()
                    .map(|&word| match kind {
                        F64_COLUMN => Tree::Float(f64::from_le_bytes(word)),
                        _ => integer(i64::from_le_bytes(word)),
                    })
                    .collect(),
            )),
            _ => Err(E::custom(format!("a column of kind {kind} and {} bytes", rest.len()))),
        }
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Tree, A::Error> {
        let Some(first) = seq.next_element::<Tree>()? else {
            return Ok(Tree::List(Vec::new()));
        };
        let keys = match first {
            Tree::Head(TABLE) => Some(
                seq.next_element::<Vec<String>>()?
                    .ok_or_else(|| de::Error::custom("a table without its keys"))?,
            ),
            Tree::Head(_) => None,
            first => {
                let mut items = Vec::with_capacity(seq.size_hint().unwrap_or(0).min(4096) + 1);
                items.push(first);
                while let Some(item) = seq.next_element::<Tree>()? {
                    if let Tree::Head(_) = item {
                        return Err(de::Error::custom("a column head inside a list"));
                    }
                    items.push(item);
                }
                return Ok(Tree::List(items));
            }
        };
        let mut columns = Vec::new();
        while let Some(column) = seq.next_element::<Tree>()? {
            let Tree::List(values) = column else {
                return Err(de::Error::custom("a table's column that is not a list"));
            };
            columns.push(values.into_iter());
        }
        let rows = columns.first().map_or(0, ExactSizeIterator::len);
        if columns.is_empty()
            || columns.iter().any(|c| c.len() != rows)
            || keys.as_ref().is_some_and(|k| k.len() != columns.len())
        {
            return Err(de::Error::custom("a table whose columns do not line up"));
        }
        let mut built = Vec::with_capacity(rows);
        for _ in 0..rows {
            let values: Vec<Tree> =
                columns.iter_mut().map(|c| c.next().expect("lined up")).collect();
            built.push(match &keys {
                Some(keys) => Tree::Object(keys.iter().cloned().zip(values).collect()),
                None => Tree::List(values),
            });
        }
        Ok(Tree::List(built))
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Tree, A::Error> {
        let mut entries = Vec::with_capacity(map.size_hint().unwrap_or(0).min(4096));
        while let Some(entry) = map.next_entry::<String, Tree>()? {
            entries.push(entry);
        }
        Ok(Tree::Object(entries))
    }
}

/// An integer as JSON reads it: [`Tree::Unsigned`] from zero up, [`Tree::Negative`] below.
fn integer(v: i64) -> Tree {
    u64::try_from(v).map_or(Tree::Negative(v), Tree::Unsigned)
}

/// **The backtesthub's one reply writer**: `response` in `codec`, the request's own (`BodyCodec::of` its body), a whole
/// report as its tree to an LZ4 client and as its text to every other (the module doc's "Reports"). The daemon's
/// connection loop and this crate's fake peer both answer through it.
pub fn write_reply<W: std::io::Write>(
    w: &mut W,
    codec: BodyCodec,
    response: &Response,
) -> std::io::Result<()> {
    match codec {
        BodyCodec::MsgpackLz4 => write_reply_as(w, codec, &StructuredReports(response)),
        BodyCodec::Json | BodyCodec::Msgpack => write_reply_as(w, codec, response),
    }
}

/// `response` as its derive writes it, but a whole REPORT as its tree (the module doc's "Reports"), under the same
/// variant tag: what [`write_reply`] sends a thin-link client. A JSON writer gets the derive's bytes.
pub struct StructuredReports<'a>(pub &'a Response);

impl Serialize for StructuredReports<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let report = match self.0 {
            Response::Report(text) => Some(("Report", text)),
            Response::ParamscanReport(text) => Some(("ParamscanReport", text)),
            Response::WalkforwardReport(text) => Some(("WalkforwardReport", text)),
            Response::CombinatorialWalkforwardReport(text) => {
                Some(("CombinatorialWalkforwardReport", text))
            }
            Response::PermutationReport(text) => Some(("PermutationReport", text)),
            Response::StudyReport(text) => Some(("StudyReport", text)),
            _ => None,
        };
        let tree = report.filter(|_| !s.is_human_readable()).and_then(|(variant, text)| {
            match serde_json::from_str::<Tree>(text) {
                Ok(tree @ (Tree::Object(_) | Tree::List(_))) => Some((variant, tree)),
                _ => None,
            }
        });
        match tree {
            // The externally tagged form the derive writes, `{variant: value}`, with the tree as the value.
            Some((variant, tree)) => {
                let mut map = s.serialize_map(Some(1))?;
                map.serialize_entry(variant, &tree)?;
                map.end()
            }
            None => self.0.serialize(s),
        }
    }
}

/// The six text reports' `deserialize_with`: the text itself (a JSON frame, the sandbox pipe), or the tree a
/// msgpack reply carries ([`StructuredReports`]) written back as the very text.
pub(crate) fn report_text<'de, D: Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    struct ReportText;

    impl<'de> Visitor<'de> for ReportText {
        type Value = String;

        fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("a report's JSON text, or its tree")
        }

        fn visit_str<E: de::Error>(self, v: &str) -> Result<String, E> {
            Ok(v.to_owned())
        }

        fn visit_string<E: de::Error>(self, v: String) -> Result<String, E> {
            Ok(v)
        }

        fn visit_seq<A: SeqAccess<'de>>(self, seq: A) -> Result<String, A::Error> {
            json_text(TreeVisitor.visit_seq(seq)?)
        }

        fn visit_map<A: MapAccess<'de>>(self, map: A) -> Result<String, A::Error> {
            json_text(TreeVisitor.visit_map(map)?)
        }
    }

    d.deserialize_any(ReportText)
}

/// `tree` as `serde_json`'s compact text.
fn json_text<E: de::Error>(tree: Tree) -> Result<String, E> {
    serde_json::to_string(&tree).map_err(E::custom)
}

/// A column field's binary read: the `bin` a msgpack writer wrote ([`F64_COLUMN`] or [`I64_COLUMN`]), or a plain list.
fn read_words<'de, D: Deserializer<'de>, T: Deserialize<'de>>(
    d: D,
    kind: u8,
    word: fn([u8; 8]) -> T,
) -> Result<Vec<T>, D::Error> {
    struct Words<T> {
        kind: u8,
        word: fn([u8; 8]) -> T,
    }

    impl<'de, T: Deserialize<'de>> Visitor<'de> for Words<T> {
        type Value = Vec<T>;

        fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("a column")
        }

        fn visit_bytes<E: de::Error>(self, bytes: &[u8]) -> Result<Vec<T>, E> {
            match bytes.split_first() {
                Some((&kind, rest)) if kind == self.kind && rest.len().is_multiple_of(8) => {
                    Ok(rest.as_chunks::<8>().0.iter().map(|w| (self.word)(*w)).collect())
                }
                _ => Err(E::custom(format!(
                    "not a column of kind {} ({} bytes)",
                    self.kind,
                    bytes.len()
                ))),
            }
        }

        fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Vec<T>, A::Error> {
            let mut out = Vec::with_capacity(seq.size_hint().unwrap_or(0).min(1 << 20));
            while let Some(value) = seq.next_element()? {
                out.push(value);
            }
            Ok(out)
        }
    }

    d.deserialize_any(Words { kind, word })
}

/// `#[serde(with)]` for a long `Vec<f64>`: an [`F64_COLUMN`] in a msgpack body, the derive's list in JSON.
pub(crate) mod f64_column {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub(crate) fn serialize<S: Serializer>(values: &[f64], s: S) -> Result<S::Ok, S::Error> {
        if s.is_human_readable() {
            return values.serialize(s);
        }
        s.serialize_bytes(&super::words(super::F64_COLUMN, values.iter().map(|v| v.to_le_bytes())))
    }

    pub(crate) fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<f64>, D::Error> {
        if d.is_human_readable() {
            return Vec::deserialize(d);
        }
        super::read_words(d, super::F64_COLUMN, f64::from_le_bytes)
    }
}

/// `#[serde(with)]` for a long `Vec<i64>` (timestamps): an [`I64_COLUMN`] in a msgpack body, the derive's list in JSON.
pub(crate) mod i64_column {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub(crate) fn serialize<S: Serializer>(values: &[i64], s: S) -> Result<S::Ok, S::Error> {
        if s.is_human_readable() {
            return values.serialize(s);
        }
        s.serialize_bytes(&super::words(super::I64_COLUMN, values.iter().map(|v| v.to_le_bytes())))
    }

    pub(crate) fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<i64>, D::Error> {
        if d.is_human_readable() {
            return Vec::deserialize(d);
        }
        super::read_words(d, super::I64_COLUMN, i64::from_le_bytes)
    }
}

/// `#[serde(with)]` for a long list of records (trades, a scan's points, a thinned curve's pairs): in a msgpack body
/// its msgpack read as a tree and written with the module doc's columns, so a record's keys cross once; the derive's
/// list in JSON. Read back through the same tree into plain msgpack, then into the records.
pub(crate) mod table {
    use serde::de::{self, DeserializeOwned};
    use serde::{Deserialize, Deserializer, Serialize, Serializer, ser};

    use super::Tree;

    pub(crate) fn serialize<S: Serializer, T: Serialize>(
        values: &[T],
        s: S,
    ) -> Result<S::Ok, S::Error> {
        if s.is_human_readable() {
            return values.serialize(s);
        }
        let msgpack = rmp_serde::to_vec_named(values).map_err(ser::Error::custom)?;
        let tree: Tree = rmp_serde::from_slice(&msgpack).map_err(ser::Error::custom)?;
        tree.serialize(s)
    }

    pub(crate) fn deserialize<'de, D: Deserializer<'de>, T: DeserializeOwned>(
        d: D,
    ) -> Result<Vec<T>, D::Error> {
        if d.is_human_readable() {
            return Vec::deserialize(d);
        }
        let tree = Tree::deserialize(d)?;
        let mut plain = Vec::new();
        // Human-readable, so the tree writes its lists value by value; the records' own column fields read both.
        let mut writer =
            rmp_serde::Serializer::new(&mut plain).with_struct_map().with_human_readable();
        tree.serialize(&mut writer).map_err(de::Error::custom)?;
        rmp_serde::from_slice(&plain).map_err(de::Error::custom)
    }
}

#[cfg(test)]
#[path = "msgpack_body_tests.rs"]
mod msgpack_body_tests;
