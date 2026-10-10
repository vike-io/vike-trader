//! Unit tests for the process-environment scanner (`env_reads.rs`).

use std::collections::BTreeMap;

use super::*;

#[test]
fn finds_literal_and_const_and_dynamic_args() {
    let src = r#"
fn a() { std::env::var("VIKE_ONE"); }
fn b() { env::var(TWO_ENV).ok(); }
fn c() { std::env::var_os("VIKE_THREE"); }
fn d() { std::env::var(format!("VIKE_FOUR_{}", v)); }
"#;
    let reads = find_env_reads(src);
    let args: Vec<&str> = reads.iter().map(|r| r.arg.as_str()).collect();
    assert_eq!(
        args,
        vec!["\"VIKE_ONE\"", "TWO_ENV", "\"VIKE_THREE\"", "format!(\"VIKE_FOUR_{}\", v)"]
    );
    assert_eq!(reads[0].line, 2);
    assert_eq!(reads[3].line, 5);
}

/// Only a real `env::var` call matches — not an identifier that merely ends in it.
#[test]
fn requires_a_word_boundary_before_env() {
    let src = "my_env::var(\"VIKE_NOPE\");\nstd::env::var(\"VIKE_YES\");\n";
    let reads = find_env_reads(src);
    assert_eq!(reads.len(), 1);
    assert_eq!(reads[0].arg, "\"VIKE_YES\"");
}

#[test]
fn const_table_collects_str_constants() {
    let src = r#"
const POLY_RECONCILE_ENV: &str = "POLY_RECONCILE";
pub const PIN_ENV: &'static str = "VIKE_PIN_CORES";
const NOT_A_STR: usize = 3;
"#;
    let t = const_table(src);
    assert_eq!(t.get("POLY_RECONCILE_ENV").map(String::as_str), Some("POLY_RECONCILE"));
    assert_eq!(t.get("PIN_ENV").map(String::as_str), Some("VIKE_PIN_CORES"));
    assert!(!t.contains_key("NOT_A_STR"));
}

#[test]
fn resolve_arg_handles_literal_const_and_dynamic() {
    let mut consts = BTreeMap::new();
    consts.insert("POLY_RECONCILE_ENV".to_string(), "POLY_RECONCILE".to_string());

    assert_eq!(
        resolve_arg("\"VIKE_ONE\"", &consts),
        Resolved::Name { name: "VIKE_ONE".into(), konst: None }
    );
    assert_eq!(
        resolve_arg("POLY_RECONCILE_ENV", &consts),
        Resolved::Name { name: "POLY_RECONCILE".into(), konst: Some("POLY_RECONCILE_ENV".into()) }
    );
    assert_eq!(resolve_arg("format!(\"VIKE_X_{}\", v)", &consts), Resolved::Dynamic);
    assert_eq!(resolve_arg("key", &consts), Resolved::Dynamic);
}

/// The declared `Naming` in the registry must match how the call site actually reads it —
/// this is the accessor the gate uses to cross-check that column.
#[test]
fn resolve_arg_reports_the_const_identifier_for_cross_checking() {
    let mut consts = BTreeMap::new();
    consts.insert("PIN_ENV".to_string(), "VIKE_PIN_CORES".to_string());
    let Resolved::Name { konst, .. } = resolve_arg("PIN_ENV", &consts) else {
        panic!("expected a resolved name");
    };
    assert_eq!(konst.as_deref(), Some("PIN_ENV"));
}

#[test]
fn finds_map_lookup_names() {
    let src = r#"
fn a(vars: &HashMap<String, String>) -> bool {
    vars.get("VIKE_RECONCILE").map(|v| v == "1").unwrap_or(false)
}
fn b(vars: &HashMap<String, String>) { vars.get("POLY_EXEC"); }
fn c(m: &HashMap<String, String>) { m.get(&key); m.get("lowercase_not_env"); }
"#;
    let consts = const_table(src);
    assert_eq!(find_map_lookups(src, &consts), vec!["POLY_EXEC", "VIKE_RECONCILE"]);
}

/// The whole point of `find_lookup_sites`: it must DISCRIMINATE, where `find_map_lookups`
/// deliberately does not. All four names below are reported by the loose sweep — only the two
/// at a real `.get(` are positive evidence of a caller-supplied-map read.
#[test]
fn lookup_sites_are_only_the_real_get_call_sites() {
    let src = r#"
const KEY_ENV: &str = "VIKE_VIA_CONST";
fn a(vars: &HashMap<String, String>) { vars.get("VIKE_VIA_LITERAL"); }
fn b(vars: &HashMap<String, String>) { vars.get(KEY_ENV); }
fn c() { std::env::var("VIKE_DIRECT_READ"); }
fn d(vars: &HashMap<String, String>) { parse_i64(vars, "VIKE_VIA_HELPER", 0); }
"#;
    let consts = const_table(src);
    // The loose sweep sees all four — that is what makes it useless as PROOF.
    assert_eq!(
        find_map_lookups(src, &consts),
        vec!["VIKE_DIRECT_READ", "VIKE_VIA_CONST", "VIKE_VIA_HELPER", "VIKE_VIA_LITERAL"]
    );
    // The precise one sees only the two `.get(` sites. `VIKE_VIA_HELPER` is the MEASURED
    // blind spot (no call syntax to anchor on); `VIKE_DIRECT_READ` is not a map read at all.
    assert_eq!(find_lookup_sites(src, &consts), vec!["VIKE_VIA_CONST", "VIKE_VIA_LITERAL"]);
}

/// `.get(` is ubiquitous; the `is_env_name` gate on the RESOLVED value is what makes a
/// false positive impossible. None of these is an env name.
#[test]
fn ordinary_get_calls_are_not_lookup_sites() {
    let src = r#"
fn a(v: &[u8], m: &HashMap<String, String>, h: &Headers) {
    v.get(0);
    m.get(&id);
    m.get("lowercase_not_env");
    h.get("content-type");
    m.get("PARTIALLY_FILLED");
}
"#;
    let consts = const_table(src);
    assert!(find_lookup_sites(src, &consts).is_empty());
}

/// `find_map_lookups` delegates shape 2 to `find_lookup_sites`, so the subset relation is
/// structural rather than a coincidence two edits could break independently.
#[test]
fn find_map_lookups_contains_every_lookup_site() {
    let src = r#"
const K: &str = "VIKE_SUBSET_CONST";
fn a(vars: &HashMap<String, String>) { vars.get(K); vars.get("VIKE_SUBSET_LITERAL"); }
"#;
    let consts = const_table(src);
    let loose = find_map_lookups(src, &consts);
    for name in find_lookup_sites(src, &consts) {
        assert!(loose.contains(&name), "{name} is a lookup site but not in the loose sweep");
    }
    assert!(loose.contains(&"VIKE_SUBSET_CONST".to_string()));
}
