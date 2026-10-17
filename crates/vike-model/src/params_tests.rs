use std::collections::HashSet;

use super::*;

fn table(pairs: &[(&str, ParamValue)]) -> ParamValue {
    ParamValue::Table(pairs.iter().map(|(k, v)| (k.to_string(), v.clone())).collect())
}

#[test]
fn the_accessors_keep_int_and_float_apart() {
    let int = ParamValue::Int(3);
    let float = ParamValue::Float(3.0);
    assert_eq!(int.as_integer(), Some(3));
    assert_eq!(int.as_float(), None, "an integer is not a strict float");
    assert_eq!(float.as_float(), Some(3.0));
    assert_eq!(float.as_integer(), None, "a float is not a strict integer");
    // the lenient read widens an integer, never the reverse
    assert_eq!(int.as_f64(), Some(3.0));
    assert_eq!(float.as_f64(), Some(3.0));
    assert_eq!(ParamValue::Str("3".into()).as_f64(), None);
}

#[test]
fn type_str_spells_the_words_the_refusals_quote() {
    let words: Vec<_> = [
        ParamValue::Str(String::new()),
        ParamValue::Int(0),
        ParamValue::Float(0.0),
        ParamValue::Bool(false),
        ParamValue::List(vec![]),
        ParamValue::empty_table(),
    ]
    .iter()
    .map(ParamValue::type_str)
    .collect();
    assert_eq!(words, ["string", "integer", "float", "boolean", "array", "table"]);
}

#[test]
fn get_reads_a_table_and_nothing_else() {
    let t = table(&[("size", ParamValue::Float(2.5))]);
    assert_eq!(t.get("size"), Some(&ParamValue::Float(2.5)));
    assert_eq!(t.get("absent"), None);
    assert_eq!(ParamValue::Int(1).get("size"), None);
}

#[test]
fn keys_iterate_sorted() {
    let p: Params = [("b", 1_i64), ("a", 2), ("c", 3)].into_iter().collect();
    assert_eq!(p.keys().cloned().collect::<Vec<_>>(), ["a", "b", "c"]);
}

#[test]
fn floats_compare_and_hash_by_bits() {
    assert_ne!(ParamValue::Float(0.0), ParamValue::Float(-0.0));
    assert_eq!(ParamValue::Float(f64::NAN), ParamValue::Float(f64::NAN));
    assert_ne!(ParamValue::Int(2), ParamValue::Float(2.0));
    let mut set = HashSet::new();
    set.insert(ParamValue::Float(1.5));
    assert!(set.contains(&ParamValue::Float(1.5)));
    assert!(!set.contains(&ParamValue::Float(1.25)));
}

#[test]
fn display_is_toml_text_and_a_float_keeps_its_kind() {
    assert_eq!(ParamValue::Int(5).to_string(), "5");
    assert_eq!(ParamValue::Float(2.0).to_string(), "2.0");
    assert_eq!(ParamValue::Float(2.5).to_string(), "2.5");
    assert_eq!(ParamValue::Float(1e21).to_string(), "1000000000000000000000.0");
    assert_eq!(ParamValue::Float(f64::INFINITY).to_string(), "inf");
    assert_eq!(ParamValue::Float(f64::NEG_INFINITY).to_string(), "-inf");
    assert_eq!(ParamValue::Float(f64::NAN).to_string(), "nan");
    assert_eq!(ParamValue::Bool(true).to_string(), "true");
    assert_eq!(ParamValue::Str("a\"b\\c\n".into()).to_string(), r#""a\"b\\c\n""#);
    assert_eq!(ParamValue::List(vec![1_i64.into(), "x".into()]).to_string(), r#"[1, "x"]"#);
    assert_eq!(ParamValue::empty_table().to_string(), "{}");
}

#[test]
fn a_table_renders_inline_and_quotes_a_key_that_needs_it() {
    let t = table(&[
        ("size", 2.0_f64.into()),
        ("odd key", true.into()),
        ("venues", table(&[("BTC", "okx".into())])),
    ]);
    assert_eq!(t.to_string(), r#"{ "odd key" = true, size = 2.0, venues = { BTC = "okx" } }"#);
}

#[test]
fn json_keeps_int_and_float_distinct_both_ways() {
    let t = table(&[
        ("rungs", 4_i64.into()),
        ("size", 2.0_f64.into()),
        ("on", true.into()),
        ("name", "x".into()),
    ]);
    let json = serde_json::to_string(&t).unwrap();
    assert_eq!(json, r#"{"name":"x","on":true,"rungs":4,"size":2.0}"#);
    let back: ParamValue = serde_json::from_str(&json).unwrap();
    assert_eq!(back, t, "2 reads back as an integer and 2.0 as a float");
}

#[test]
fn json_nested_values_round_trip() {
    let v = table(&[
        ("wallets", ParamValue::List(vec!["0xa".into(), "0xb".into()])),
        ("venues", table(&[("BTC", "okx".into())])),
    ]);
    let back: ParamValue = serde_json::from_value(serde_json::to_value(&v).unwrap()).unwrap();
    assert_eq!(back, v);
}

#[test]
fn null_is_refused() {
    assert!(serde_json::from_str::<ParamValue>("null").is_err());
    assert!(serde_json::from_str::<ParamValue>(r#"{"a":null}"#).is_err());
}

#[test]
fn an_integer_above_i64_max_is_refused() {
    assert!(serde_json::from_str::<ParamValue>("9223372036854775808").is_err());
    assert_eq!(
        serde_json::from_str::<ParamValue>("9223372036854775807").unwrap(),
        ParamValue::Int(i64::MAX)
    );
}

#[test]
fn params_deserializes_only_a_table() {
    assert!(serde_json::from_str::<Params>("5").is_err());
    assert_eq!(serde_json::from_str::<Params>("{}").unwrap(), Params::new());
}

#[test]
fn the_reserved_key_is_found_at_any_depth() {
    let v = table(&[("a", table(&[("b", ParamValue::List(vec![table(&[("src", "x".into())])]))]))]);
    assert!(v.contains_key_at_any_depth("src"));
    assert!(!v.contains_key_at_any_depth("absent"));
    assert!(!ParamValue::Int(1).contains_key_at_any_depth("src"));
}

#[test]
fn numeric_narrows_to_f64_and_reports_what_it_drops() {
    let p: Params = [
        ("fast", ParamValue::Int(5)),
        ("size", ParamValue::Float(2.5)),
        ("mode", ParamValue::Str("x".into())),
        ("on", ParamValue::Bool(true)),
    ]
    .into_iter()
    .collect();
    let (numbers, dropped) = p.numeric();
    assert_eq!(numbers.get("fast"), Some(&5.0));
    assert_eq!(numbers.get("size"), Some(&2.5));
    assert_eq!(dropped, ["mode", "on"]);
}

#[test]
fn a_duplicate_key_is_refused_as_the_old_type_refused_it() {
    assert!(serde_json::from_str::<Params>(r#"{"a":1,"a":2}"#).is_err());
    assert!(serde_json::from_str::<ParamValue>(r#"{"t":{"a":1,"a":2}}"#).is_err());
}
