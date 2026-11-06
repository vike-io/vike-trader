//! **`vike_model::params` against `toml::Value`, byte for byte** — the proof the params vocabulary
//! rests on.
//!
//! Strategy construction parameters stopped being `toml::Value` and became
//! `vike_model::params::{ParamValue, Params}`. Nothing was migrated: a stored ledger, a sweep report,
//! a `MountSpec` sidecar and every profile file are read by the new type exactly as they were
//! written by the old one. That holds only if the two types agree on every text form, so this file
//! builds one battery of values TWICE — once as `toml::Value`, once as `ParamValue`, each side from
//! its own constructors — and asserts:
//!
//! * (a) `serde_json::to_string` is the same bytes;
//! * (b) `toml::to_string` of a one-field document `{ params = <table> }` is the same bytes;
//! * (c) `toml::from_str::<Params>(text)` equals the old `toml::Table` read of the same text,
//!   converted by hand here (for every battery document and a hand-written one using every TOML
//!   spelling a profile may contain);
//! * (d) the JSON the OLD type wrote reads back through the NEW type to the battery value, an
//!   integer staying `Int` and a float staying `Float`;
//! * (e) what the new type REFUSES: JSON `null`, an integer above `i64::MAX`, a TOML datetime in
//!   any position, a non-table where a table is due;
//! * and that `ParamValue`'s `Display` (the TOML text a `mount_param` row stores) parses back to the
//!   same value. Where that text differs from `toml::Value`'s own `Display`, the case is listed in
//!   [`DISPLAY_TEXT_DIFFERS`]: a difference in TEXT is allowed, a difference in the value read back
//!   is not.
//!
//! It lives in `vike-config` because this crate is the text edge that already links `vike-model`,
//! `toml` and `serde_json` together; it tests no `vike-config` code.
//!
//! Floats compare by BIT PATTERN on the new side (`ParamValue`'s `PartialEq`), so `-0.0` and `NaN`
//! are asserted exactly, not approximately. A non-finite float has no JSON spelling (serde_json
//! writes `null`, which neither type reads back), so (d) skips the cases that hold one.

use serde::Serialize;
use vike_model::params::{ParamValue, Params};

/// One value built on BOTH sides, each from its own constructors.
type Pair = (toml::Value, ParamValue);

fn int(i: i64) -> Pair {
    (toml::Value::Integer(i), ParamValue::Int(i))
}

fn float(x: f64) -> Pair {
    (toml::Value::Float(x), ParamValue::Float(x))
}

fn boolean(b: bool) -> Pair {
    (toml::Value::Boolean(b), ParamValue::Bool(b))
}

fn string(s: &str) -> Pair {
    (toml::Value::String(s.to_string()), ParamValue::Str(s.to_string()))
}

fn list(items: Vec<Pair>) -> Pair {
    let (old, new): (Vec<toml::Value>, Vec<ParamValue>) = items.into_iter().unzip();
    (toml::Value::Array(old), ParamValue::List(new))
}

fn table(entries: Vec<(&str, Pair)>) -> Pair {
    let (old, new) = table_pair(entries);
    (toml::Value::Table(old), ParamValue::Table(new))
}

fn table_pair(entries: Vec<(&str, Pair)>) -> (toml::Table, Params) {
    let mut old = toml::Table::new();
    let mut new = Params::new();
    for (k, (o, n)) in entries {
        old.insert(k.to_string(), o);
        new.insert(k, n);
    }
    (old, new)
}

/// The battery: every kind, its edges, and the shapes a strategy profile actually holds.
fn battery() -> Vec<(&'static str, Pair)> {
    vec![
        ("int zero", int(0)),
        ("int two", int(2)),
        ("int negative", int(-7)),
        ("int i64::MIN", int(i64::MIN)),
        ("int i64::MAX", int(i64::MAX)),
        ("float 2.0", float(2.0)),
        ("float 2.5", float(2.5)),
        ("float 0.0", float(0.0)),
        ("float -0.0", float(-0.0)),
        ("float 0.1", float(0.1)),
        ("float 1e21", float(1e21)),
        ("float 1e-7", float(1e-7)),
        ("float 1e300", float(1e300)),
        ("float f64::MAX", float(f64::MAX)),
        ("float f64::MIN_POSITIVE", float(f64::MIN_POSITIVE)),
        ("float subnormal 5e-324", float(f64::from_bits(1))),
        ("float inf", float(f64::INFINITY)),
        ("float -inf", float(f64::NEG_INFINITY)),
        ("float nan", float(f64::NAN)),
        ("bool true", boolean(true)),
        ("bool false", boolean(false)),
        ("string empty", string("")),
        ("string plain", string("plain")),
        ("string double quotes", string("say \"hi\"")),
        ("string backslash", string("back\\slash C:\\path")),
        ("string single quote", string("it's")),
        ("string triple double quotes", string("a \"\"\" b")),
        ("string triple single quotes", string("a ''' b")),
        ("string newline", string("line1\nline2")),
        ("string crlf", string("cr\r\nlf")),
        ("string tab", string("tab\there")),
        ("string unicode", string("é 漢字 🚀 ключ")),
        ("string controls", string("\u{0}\u{1}\u{8}\u{b}\u{c}\u{1b}\u{1f}\u{7f}")),
        ("string looks like a float", string("2.5")),
        ("string looks like a datetime", string("1979-05-27T07:32:00Z")),
        ("string looks like a bool", string("true")),
        ("array empty", list(vec![])),
        ("array of ints", list(vec![int(1), int(2), int(3)])),
        ("array mixed kinds", list(vec![int(1), float(2.5), string("x"), boolean(true)])),
        ("array of empty arrays", list(vec![list(vec![]), list(vec![list(vec![])])])),
        ("array nested", list(vec![list(vec![int(1)]), list(vec![float(2.5), string("x")])])),
        (
            "array of tables",
            list(vec![table(vec![("a", int(1))]), table(vec![("b", string("x"))])]),
        ),
        ("array of one empty table", list(vec![table(vec![])])),
        ("table empty", table(vec![])),
        ("table flat", table(vec![("size", float(2.5)), ("rungs", int(4))])),
        (
            "table nested",
            table(vec![
                (
                    "outer",
                    table(vec![
                        ("inner", table(vec![("leaf", float(1.5))])),
                        ("list", list(vec![int(1), int(2)])),
                    ]),
                ),
                ("z", int(0)),
            ]),
        ),
        (
            "table scalar after a subtable",
            table(vec![("a", table(vec![("x", int(1))])), ("b", int(2))]),
        ),
        (
            "table keys needing quotes",
            table(vec![
                ("a b", int(1)),
                ("", int(2)),
                ("a.b", int(3)),
                ("ключ", int(4)),
                ("quote\"key", int(5)),
                ("-dash_ok", int(6)),
                ("new\nline", int(7)),
            ]),
        ),
    ]
}

/// The old document form a profile is written in: `[params]` with the value under `v`.
#[derive(Serialize)]
struct OldDoc {
    params: toml::Table,
}

#[derive(Serialize)]
struct NewDoc {
    params: Params,
}

fn wrap((old, new): Pair) -> (OldDoc, NewDoc) {
    let (old, new) = table_pair(vec![("v", (old, new))]);
    (OldDoc { params: old }, NewDoc { params: new })
}

/// The hand conversion (c) checks against: a datetime has no `ParamValue` and is a test bug here.
fn convert(v: &toml::Value) -> ParamValue {
    match v {
        toml::Value::Integer(i) => ParamValue::Int(*i),
        toml::Value::Float(x) => ParamValue::Float(*x),
        toml::Value::Boolean(b) => ParamValue::Bool(*b),
        toml::Value::String(s) => ParamValue::Str(s.clone()),
        toml::Value::Array(items) => ParamValue::List(items.iter().map(convert).collect()),
        toml::Value::Table(t) => ParamValue::Table(convert_table(t)),
        toml::Value::Datetime(d) => panic!("a battery document holds a datetime ({d})"),
    }
}

fn convert_table(t: &toml::Table) -> Params {
    t.iter().map(|(k, v)| (k.clone(), convert(v))).collect()
}

fn has_non_finite(v: &ParamValue) -> bool {
    match v {
        ParamValue::Float(x) => !x.is_finite(),
        ParamValue::List(items) => items.iter().any(has_non_finite),
        ParamValue::Table(t) => t.values().any(has_non_finite),
        _ => false,
    }
}

/// Every TOML spelling a profile may use, in one document.
const HAND_DOCUMENT: &str = r#"
# a comment
size = 2.5
rungs = 4
hex = 0xff
oct = 0o17
bin = 0b101
under = 1_000_000
exp = 6.02e23
neg = -1.5E-3
pinf = +inf
ninf = -inf
pnan = +nan
nnan = -nan
nzero = -0.0
lit = 'C:\path'
basic = "tab\tquote\"uni\u00e9"
ml = """
two
lines"""
mll = '''raw \n stays'''
dotted.key.leaf = 1
"quoted key" = "q"
inline = { a = 1, b = { c = [1, 2] } }
trailing = [ 1, 2, ]
multiline_array = [
  "a",
  "b",
]

[sub]
x = 1

[sub.deeper]
y = true

[[aot]]
n = 1

[[aot]]
n = 2
"#;

// ---- (a) JSON bytes -----------------------------------------------------------------------------

#[test]
fn a_json_bytes_are_identical() {
    for (name, (old, new)) in battery() {
        let old_json = serde_json::to_string(&old).expect("old serializes");
        let new_json = serde_json::to_string(&new).expect("new serializes");
        assert_eq!(new_json, old_json, "JSON bytes differ for `{name}`");
    }
}

// ---- (b) TOML document bytes --------------------------------------------------------------------

#[test]
fn b_toml_document_bytes_are_identical() {
    for (name, pair) in battery() {
        // The value under a key …
        let (old_doc, new_doc) = wrap(pair.clone());
        let old_text = toml::to_string(&old_doc)
            .unwrap_or_else(|e| panic!("old document fails for `{name}`: {e}"));
        let new_text = toml::to_string(&new_doc)
            .unwrap_or_else(|e| panic!("new document fails for `{name}`: {e}"));
        assert_eq!(new_text, old_text, "TOML document bytes differ for `{name}`");
        // … and a table AS the params table (the shape `[strategy.params]` really has).
        if let (toml::Value::Table(old_t), ParamValue::Table(new_t)) = pair {
            let old_text = toml::to_string(&OldDoc { params: old_t }).expect("old table");
            let new_text = toml::to_string(&NewDoc { params: new_t }).expect("new table");
            assert_eq!(new_text, old_text, "TOML table bytes differ for `{name}` as params");
        }
    }
}

// ---- (c) the TOML read ---------------------------------------------------------------------------

fn assert_same_toml_read(label: &str, text: &str) {
    let old: toml::Table =
        toml::from_str(text).unwrap_or_else(|e| panic!("old read refuses `{label}`: {e}"));
    let new: Params =
        toml::from_str(text).unwrap_or_else(|e| panic!("new read refuses `{label}`: {e}"));
    assert_eq!(new, convert_table(&old), "the TOML read differs for `{label}`:\n{text}");
}

#[test]
fn c_toml_read_equals_the_old_read_converted() {
    for (name, pair) in battery() {
        let (old_doc, _) = wrap(pair.clone());
        let text = toml::to_string(&old_doc).expect("old document");
        assert_same_toml_read(name, &text);
        if let (toml::Value::Table(old_t), _) = pair {
            let text = toml::to_string(&OldDoc { params: old_t }).expect("old table");
            assert_same_toml_read(name, &text);
        }
    }
    assert_same_toml_read("the hand document", HAND_DOCUMENT);
    assert_same_toml_read("an empty document", "");
}

#[test]
fn c_the_hand_document_reads_the_kinds_it_spells() {
    let p: Params = toml::from_str(HAND_DOCUMENT).expect("hand document");
    assert_eq!(p.get("rungs"), Some(&ParamValue::Int(4)));
    assert_eq!(p.get("hex"), Some(&ParamValue::Int(255)));
    assert_eq!(p.get("under"), Some(&ParamValue::Int(1_000_000)));
    assert_eq!(p.get("size"), Some(&ParamValue::Float(2.5)));
    assert_eq!(p.get("nzero"), Some(&ParamValue::Float(-0.0)));
    assert_eq!(p.get("pinf"), Some(&ParamValue::Float(f64::INFINITY)));
    assert!(p.get("nnan").and_then(ParamValue::as_float).is_some_and(f64::is_nan));
    assert_eq!(p.get("lit").and_then(ParamValue::as_str), Some("C:\\path"));
    let leaf = p
        .get("dotted")
        .and_then(|v| v.get("key"))
        .and_then(|v| v.get("leaf"))
        .and_then(ParamValue::as_integer);
    assert_eq!(leaf, Some(1));
    let aot = p.get("aot").and_then(ParamValue::as_array).expect("array of tables");
    assert_eq!(aot.len(), 2);
    assert_eq!(aot[1].get("n"), Some(&ParamValue::Int(2)));
}

// ---- (d) the old type's JSON reads back through the new type -----------------------------------

#[test]
fn d_old_json_reads_back_to_the_same_value() {
    for (name, (old, new)) in battery() {
        if has_non_finite(&new) {
            continue;
        }
        let json = serde_json::to_string(&old).expect("old serializes");
        let back: ParamValue = serde_json::from_str(&json)
            .unwrap_or_else(|e| panic!("new type refuses the old JSON of `{name}` ({json}): {e}"));
        assert_eq!(back, new, "old JSON of `{name}` reads back different: {json}");

        let (old_doc, new_doc) = wrap((old, new));
        let json = serde_json::to_string(&old_doc.params).expect("old table serializes");
        let back: Params = serde_json::from_str(&json).unwrap_or_else(|e| {
            panic!("new Params refuses the old JSON of `{name}` ({json}): {e}")
        });
        assert_eq!(back, new_doc.params, "old table JSON of `{name}` reads back different");
    }
}

#[test]
fn d_an_integer_stays_int_and_a_float_stays_float() {
    let read = |json: &str| serde_json::from_str::<ParamValue>(json).expect(json);
    assert_eq!(read("2"), ParamValue::Int(2));
    assert_eq!(read("2.0"), ParamValue::Float(2.0));
    assert_eq!(read("-0.0"), ParamValue::Float(-0.0));
    assert_ne!(read("-0.0"), ParamValue::Float(0.0), "-0.0 keeps its sign bit");
    assert_eq!(read("1e21"), ParamValue::Float(1e21));
    assert_eq!(read("9223372036854775807"), ParamValue::Int(i64::MAX));
    assert_eq!(read("-9223372036854775808"), ParamValue::Int(i64::MIN));
    let t: Params = serde_json::from_str(r#"{"rungs":4,"size":4.0}"#).expect("table");
    assert_eq!(t.get("rungs"), Some(&ParamValue::Int(4)));
    assert_eq!(t.get("size"), Some(&ParamValue::Float(4.0)));
}

// ---- (e) refusals ------------------------------------------------------------------------------

#[test]
fn e_json_null_is_refused() {
    assert!(serde_json::from_str::<ParamValue>("null").is_err());
    assert!(serde_json::from_str::<Params>("null").is_err());
    assert!(serde_json::from_str::<Params>(r#"{"a":null}"#).is_err());
    assert!(serde_json::from_str::<Params>(r#"{"a":[1,null]}"#).is_err());
    assert!(serde_json::from_str::<Params>(r#"{"a":{"b":null}}"#).is_err());
}

#[test]
fn e_an_integer_above_i64_max_is_refused() {
    assert!(serde_json::from_str::<ParamValue>("9223372036854775808").is_err());
    assert!(serde_json::from_str::<ParamValue>("18446744073709551615").is_err());
    assert!(serde_json::from_str::<Params>(r#"{"a":9223372036854775808}"#).is_err());
    assert!(toml::from_str::<Params>("a = 9223372036854775808").is_err());
}

#[test]
fn e_a_toml_datetime_is_refused_in_every_position() {
    for doc in [
        "d = 1979-05-27T07:32:00Z",
        "d = 1979-05-27T00:32:00-07:00",
        "d = 1979-05-27T07:32:00",
        "d = 1979-05-27",
        "d = 07:32:00",
        "d = [1979-05-27]",
        "d = { e = 1979-05-27 }",
        "[t]\nd = 1979-05-27",
        "[[t]]\nd = 1979-05-27",
    ] {
        // The old type read it (a `Datetime`); the new one refuses it at the edge.
        assert!(toml::from_str::<toml::Table>(doc).is_ok(), "the old type read `{doc}`");
        assert!(toml::from_str::<Params>(doc).is_err(), "the new type must refuse `{doc}`");
    }
    // The JSON the old type wrote for a datetime is refused too.
    let old: toml::Table = toml::from_str("d = 1979-05-27T07:32:00Z").expect("old datetime");
    let json = serde_json::to_string(&old).expect("old datetime JSON");
    assert!(json.contains("$__toml_private_datetime"), "{json}");
    assert!(serde_json::from_str::<Params>(&json).is_err(), "{json}");
}

#[test]
fn e_a_non_table_is_not_params() {
    assert!(serde_json::from_str::<Params>("5").is_err());
    assert!(serde_json::from_str::<Params>("[1]").is_err());
    assert!(serde_json::from_str::<Params>(r#""x""#).is_err());
    #[derive(serde::Deserialize)]
    struct Profile {
        params: Params,
    }
    let read = |doc: &str| toml::from_str::<Profile>(doc).map(|p| p.params);
    assert!(read("params = 5").is_err());
    assert!(read("params = [1]").is_err());
    assert_eq!(read("params = {}").expect("an empty table"), Params::new());
}

// ---- the TOML text of one value (`Display`) ------------------------------------------------------

/// The battery cases whose `ParamValue::to_string()` TEXT differs from `toml::Value::to_string()`.
/// Each still reads back to the same value; the difference is spelling only. `ParamValue` always
/// writes a BASIC string (`"..."`, escapes inside); `toml` picks a LITERAL string (`'...'`) for text
/// holding `"` or `\`, and a multi-line basic string (`"""` + raw newlines) for text holding a
/// newline. Measured on toml 1.1.6; numbers, booleans, arrays and every other key render alike.
const DISPLAY_TEXT_DIFFERS: &[&str] = &[
    "string double quotes",
    "string backslash",
    "string triple double quotes",
    "string newline",
    "string crlf",
    "table keys needing quotes",
];

fn read_back(text: &str) -> ParamValue {
    let doc: Params = toml::from_str(&format!("v = {text}"))
        .unwrap_or_else(|e| panic!("`v = {text}` does not parse: {e}"));
    doc.get("v").cloned().expect("the one key")
}

#[test]
fn display_reads_back_to_the_same_value() {
    let mut differs = Vec::new();
    for (name, (old, new)) in battery() {
        let ours = new.to_string();
        assert_eq!(read_back(&ours), new, "`{name}` rendered as `{ours}` reads back different");
        let theirs = old.to_string();
        assert_eq!(read_back(&theirs), new, "toml's `{name}` text `{theirs}` reads back different");
        if ours != theirs {
            eprintln!("DISPLAY DIFFERS `{name}`: ours `{ours}` | toml `{theirs}`");
            differs.push(name);
        }
    }
    assert_eq!(differs, DISPLAY_TEXT_DIFFERS, "the Display text differences changed");
}

/// A NaN with its sign bit set is the one float whose TEXT does not keep its bits — on BOTH sides:
/// `toml` writes `nan` for it as `ParamValue` does, and `nan` reads back as the positive NaN. Not a
/// difference between the types; pinned so a change on either side is seen.
#[test]
fn a_negative_nan_renders_as_nan_on_both_sides() {
    let neg_nan = -f64::NAN;
    let ours = ParamValue::Float(neg_nan).to_string();
    assert_eq!(ours, toml::Value::Float(neg_nan).to_string());
    assert_eq!(ours, "nan");
    assert!(read_back(&ours).as_float().is_some_and(f64::is_nan));
}
