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

/// An IMPORTED bare read is the same read, and used to match nothing at all.
///
/// ⚠ The pattern list was exactly the two path-qualified spellings, so `use std::env::var;`
/// followed by `var("VIKE_X")` — one line of import away from the form every gate here keys —
/// was invisible: no row demanded, no allowlist entry, no sighting. Every import shape is
/// covered because they are all one keystroke apart.
#[test]
fn an_imported_bare_read_is_still_a_read() {
    let bare = "use std::env::var;\nfn a() { var(\"VIKE_BARE\"); }\n";
    assert_eq!(
        find_env_reads(bare).iter().map(|r| r.arg.as_str()).collect::<Vec<_>>(),
        vec!["\"VIKE_BARE\""]
    );
    let grouped = "use std::env::{var, var_os};\nfn a() { var_os(\"VIKE_GROUPED\"); }\n";
    assert_eq!(
        find_env_reads(grouped).iter().map(|r| r.arg.as_str()).collect::<Vec<_>>(),
        vec!["\"VIKE_GROUPED\""]
    );
    let renamed = "use std::env::var as getenv;\nfn a() { getenv(\"VIKE_ALIAS\"); }\n";
    assert_eq!(
        find_env_reads(renamed).iter().map(|r| r.arg.as_str()).collect::<Vec<_>>(),
        vec!["\"VIKE_ALIAS\""]
    );
    let mod_alias = "use std::env as e;\nfn a() { e::var(\"VIKE_MODALIAS\"); }\n";
    assert_eq!(
        find_env_reads(mod_alias).iter().map(|r| r.arg.as_str()).collect::<Vec<_>>(),
        vec!["\"VIKE_MODALIAS\""]
    );

    // …and ONE row per call site: with the bare name imported, `var(` also matches the tail of
    // `env::var(`, so the qualified read below must not be reported twice.
    let both = "use std::env::var;\nfn a() { std::env::var(\"VIKE_ONCE\"); }\n";
    assert_eq!(find_env_reads(both).len(), 1, "{:?}", find_env_reads(both));
}

/// The general resolver, driven at every `use` shape a caller can meet.
///
/// The names here are invented (`Widget`, `w`) for the reason [`find_calls`]' doc gives: this
/// file is walked by the gate that calls it, and a real type spelled here as fixture DATA would
/// come back as a `vike-ops` sighting.
#[test]
fn an_imported_type_is_found_under_whatever_name_the_file_gave_it() {
    let of = |src: &str| imported_spellings(src, "Widget");

    assert_eq!(of("use krate::Widget;\n"), vec!["Widget".to_string()]);
    assert_eq!(of("use krate::Widget as W;\n"), vec!["W".to_string()]);
    assert_eq!(of("use krate::{Widget, Other};\n"), vec!["Widget".to_string()]);
    assert_eq!(of("use krate::{Other, Widget as W};\n"), vec!["W".to_string()]);
    assert_eq!(of("pub use krate::Widget as W;\n"), vec!["W".to_string()]);
    // A MODULE alias renames the path, so the item keeps its name behind the new prefix.
    assert_eq!(of("use krate as k;\n"), vec!["k::Widget".to_string()]);
    // rustfmt wraps a long group across lines; the statement is read whole, not per line.
    assert_eq!(of("use krate::{\n    Other,\n    Widget as W,\n};\n"), vec!["W".to_string()]);

    // Nothing is invented from a file that imports nothing of the sort…
    assert!(of("use std::collections::BTreeMap;\nfn f() { let _ = Widget::new(); }\n").is_empty());
    // …and `misuse ` is not a declaration.
    assert!(of("fn misuse Widget;\n").is_empty());

    // The DECLARED residual: a glob brings the name in without spelling it, so there is nothing
    // to harvest — the same refusal `imported_env_read_patterns` makes, and for the same
    // reason. Invert this assertion rather than deleting it if globs are ever resolved.
    assert!(of("use krate::*;\n").is_empty());
}

/// The false positives the import requirement exists to avoid, and the residual it accepts.
///
/// ⚠ The first fixture is `crates/bridges/vike-ibkr/src/config.rs`'s
/// `load_ibkr_config_for_account` in miniature: a local closure NAMED `var`, reading a
/// caller-supplied map — the shape this registry calls correct. Searching for a bare `var(`
/// unconditionally reports eight of these in that one file and, because the inner call's
/// argument is a parameter, would demand a `DYNAMIC_ALLOWLIST` row for a file that reads no
/// environment at all.
#[test]
fn an_unimported_bare_name_is_somebody_elses_function() {
    let local = "fn f(vars: &Map) {\n    let var = |s: &str| vars.get(s);\n    \
                     var(\"ACCOUNT\");\n    var(\"BACKEND\");\n}\n";
    assert!(find_env_reads(local).is_empty(), "{:?}", find_env_reads(local));

    // A definition is not a call, and a method is not a free function — both matter only once
    // the bare name is in play, because a `::` path can be neither.
    let shadowed = "use std::env::var;\nfn var(s: &str) -> Option<String> { None }\n\
                        fn f(c: &C) { c.var(\"VIKE_METHOD\"); }\n";
    assert!(find_env_reads(shadowed).is_empty(), "{:?}", find_env_reads(shadowed));

    // The DECLARED residual: a glob import brings the name in without naming it, so there is
    // nothing to harvest. Handled by NOT falling back to an unconditional bare search — that
    // trades this miss for the eight false demands above. If somebody teaches the scanner to
    // resolve globs, invert this assertion rather than deleting it.
    let globbed = "use std::env::*;\nfn a() { var(\"VIKE_GLOB\"); }\n";
    assert!(find_env_reads(globbed).is_empty(), "{:?}", find_env_reads(globbed));

    // …and the positive control, so a scanner that had stopped matching entirely could not
    // pass every assertion above by answering "nothing" to everything.
    let real = "fn a() { std::env::var(\"VIKE_CONTROL\"); }\n";
    assert_eq!(find_env_reads(real).len(), 1);
}

/// Nested parens inside the argument must not truncate it — a `format!(..)` arg is the
/// exact shape that a naive "scan to the first `)`" would mangle into a false Literal.
#[test]
fn balances_nested_parens_in_the_argument() {
    let src = r#"std::env::var(format!("A_{}", f(1, g(2))));"#;
    let reads = find_env_reads(src);
    assert_eq!(reads.len(), 1);
    assert_eq!(reads[0].arg, r#"format!("A_{}", f(1, g(2)))"#);
}

/// A `)` inside a string literal is not a closing paren.
#[test]
fn ignores_parens_inside_string_literals() {
    let src = r#"std::env::var("VIKE_A)B");"#;
    let reads = find_env_reads(src);
    assert_eq!(reads.len(), 1);
    assert_eq!(reads[0].arg, r#""VIKE_A)B""#);
}

/// `env::var` appearing inside a comment or a doc block is not a call site — the repo's
/// `//!` module docs mention these variables constantly.
#[test]
fn skips_comments_and_doc_blocks() {
    let src = "//! set env::var(\"VIKE_DOC\") to enable\n// std::env::var(\"VIKE_LINE\")\nfn a() { std::env::var(\"VIKE_REAL\"); }\n";
    let reads = find_env_reads(src);
    assert_eq!(reads.len(), 1);
    assert_eq!(reads[0].arg, "\"VIKE_REAL\"");
}

/// A raw string with an ODD number of embedded quotes flips naive quote-parity, so the
/// pre-fix scanner believed it was still inside a string when it reached the `//` and
/// therefore did NOT strip the comment — surfacing a call that only exists in a comment.
/// Correct behaviour strips it and finds nothing.
#[test]
fn raw_string_does_not_leak_quote_parity_into_the_next_comment() {
    let src = "let q = r#\"a \"b\"#; // std::env::var(\"VIKE_FAKE\")\n";
    assert_eq!(find_env_reads(src), Vec::new(), "a call inside a comment must not be reported");
}

/// A normal string literal spanning two lines: the `//` on its second line is INSIDE the
/// string, so it is not a comment and the text after it survives. The pre-fix scanner reset
/// string state at the newline, treated that `//` as a real comment, and stripped the rest
/// of the line — losing the call entirely.
#[test]
fn string_state_survives_a_newline() {
    let src = "let s = \"first\n// std::env::var(\"VIKE_IN_STRING\");\n";
    let reads = find_env_reads(src);
    assert_eq!(reads.len(), 1, "text inside a multi-line string must not be comment-stripped");
    assert_eq!(reads[0].line, 2);
}

/// A raw-string ARGUMENT containing both a quote and a close-paren. The pre-fix
/// `take_balanced` let the embedded quote end string-tracking, so the very next `)` looked
/// like the closing paren and the argument came back truncated to `r#"A"`.
#[test]
fn raw_string_argument_survives_embedded_quote_and_paren() {
    let src = "std::env::var(r#\"A\")B\"#);";
    let reads = find_env_reads(src);
    assert_eq!(reads.len(), 1);
    assert_eq!(reads[0].arg, "r#\"A\")B\"#", "argument must not truncate at the in-string paren");
}

/// Only a real `env::var` call matches — not an identifier that merely ends in it.
#[test]
fn requires_a_word_boundary_before_env() {
    let src = "my_env::var(\"VIKE_NOPE\");\nstd::env::var(\"VIKE_YES\");\n";
    let reads = find_env_reads(src);
    assert_eq!(reads.len(), 1);
    assert_eq!(reads[0].arg, "\"VIKE_YES\"");
}

/// An unterminated raw string at EOF must not be treated as closed — the closer check must
/// not succeed vacuously when fewer hashes remain than the opener demanded. `r##"` opens
/// with two hashes; only one is present before EOF, so there is no legal close at all and
/// no call to find.
#[test]
fn truncated_raw_string_at_eof_is_not_closed() {
    let src = "let q = r##\"abc\"#";
    assert_eq!(find_env_reads(src), Vec::new());
}

/// The same input, but asserting directly on `strip_comments`'s output rather than through
/// `find_env_reads`: this input has no `env::var` call at all, so the assertion above passes
/// whether or not the closer is (wrongly) accepted — it cannot, by itself, prove the guard
/// is doing anything. Before the length guard, the vacuous `.take(n).all(..)` accepted the
/// truncated one-hash closer as if it were the demanded two, and `strip_comments`
/// reconstructed a closer with the WRONG hash count — silently fabricating a `#` that was
/// never in the source. The guard forces the malformed literal to pass through byte-for-byte
/// unmodified instead.
#[test]
fn truncated_raw_string_at_eof_is_not_closed_strip_comments_direct() {
    let src = "let q = r##\"abc\"#";
    assert_eq!(
        strip_comments(src),
        src,
        "an unterminated raw string must pass through byte-for-byte, unmodified"
    );
}

/// The raw-string CLOSER's cursor advance was pinned by nothing: a mutation sweep changed
/// `i += 1 + n` to `i -= 1 + n` and all thirty unit tests in this module still passed, because
/// every one of them closes with zero or one hash and none re-enters the walk afterwards.
///
/// Two things rest on that arithmetic, and this test pins both:
///
/// 1. QUOTE PARITY. Mis-walking the closer leaves the scanner believing it is still inside a
///    string, which inverts in-comment state for everything after it — and then a
///    COMMENTED-OUT `env::var` is reported as a real read. That is a false POSITIVE in a merge
///    gate: `settings_registry.rs` would demand a `SETTINGS` row for a name nothing reads.
/// 2. THE NEWLINE COUNT. `strip_comments` must preserve line structure because
///    `direct_test_override` maps an offset back to a line; drift there scores a `Library`
///    read as `TestOnly`, which is a false NEGATIVE — the quieter and worse direction.
///
/// (The names below are INVENTED, for the reason the sibling test above spells out.)
#[test]
fn a_multi_hash_raw_string_closer_is_walked_exactly_once() {
    // No comments anywhere, so `strip_comments` must be the IDENTITY over this input — which
    // holds only if each closer advances the cursor by exactly its own width. `r##"y"#z"##`
    // is the case that matters: the inner `"#` is NOT a closer at two hashes.
    let src = "let a = r\"x\";\nlet b = r#\"abc\"#;\nlet c = r##\"y\"#z\"##;\n";
    assert_eq!(
        strip_comments(src),
        src,
        "raw strings carry no comments — every closer must be walked exactly once"
    );
    assert_eq!(
        strip_comments(src).matches('\n').count(),
        src.matches('\n').count(),
        "line structure must survive, or offset-to-line mapping silently misattributes reads"
    );

    // The parity half: a hashed raw string, then a commented-out read on the NEXT line.
    let commented = "let s = r#\"abc\"#;\n// std::env::var(\"VIKE_NOT_A_REAL_READ\")\n";
    assert!(
        find_env_reads(commented).is_empty(),
        "a commented-out read after a hashed raw string must stay invisible — reporting it \
             would demand a SETTINGS row for a name nothing reads"
    );
}

/// The names below (`load_store`, `read_creds`) are INVENTED. The real credential-store entry
/// points are deliberately not spelled in this file: the gate that calls `find_calls` walks
/// `crates/` including this one, and a real name written here as fixture DATA would be reported
/// as a `vike-ops` library read — the same self-scanning trap that forced
/// `LITERAL_HARVEST_EXCLUDED` on the env side. `names` being a parameter is what makes the
/// avoidance possible.
#[test]
fn finds_calls_through_a_path_qualifier_and_skips_comments() {
    let src = "//! call load_store() to get creds\n\
// let x = load_store();\n\
fn a() { let v = some::path::load_store(); }\n\
fn b() { read_creds(1); }\n";
    let calls = find_calls(src, &["load_store", "read_creds"]);
    assert_eq!(calls.len(), 2, "only the two real call sites, got {calls:?}");
    assert_eq!((calls[0].name.as_str(), calls[0].line), ("load_store", 3));
    assert_eq!((calls[1].name.as_str(), calls[1].line), ("read_creds", 4));
}

/// The DEFINITION of a reader is not a call to one. Without this, the file that defines
/// `load_workspace_dotenv` would be reported as the library that reads the `.env`.
#[test]
fn a_function_definition_is_not_a_call_site() {
    let src = "pub fn load_store() -> Map { inner() }\n";
    assert_eq!(find_calls(src, &["load_store"]), Vec::new());
    assert!(defines_fn(src, "load_store"));
    assert!(!defines_fn(src, "read_creds"));
    // A multi-line signature opens its parameter list on the signature line and closes it
    // several lines down — the definition check must not depend on the arguments fitting on
    // one line.
    assert!(defines_fn("pub fn load_store(\n    home: Option<&Path>,\n) -> Map {\n", "load_store"));
}

/// A `use` / `pub use` re-export names the function with no `(`, so it is never a call site —
/// `vike_bridge_core`'s `pub use vike_secrets::{load_workspace_dotenv, ..}` is exactly this.
#[test]
fn a_re_export_is_not_a_call_site() {
    let src = "pub use vike_secrets::{load_store, parse_thing};\nuse a::load_store;\n";
    assert_eq!(find_calls(src, &["load_store"]), Vec::new());
}

/// Identifier boundary on BOTH sides of the match.
///
/// Before the name: a longer identifier merely ENDING in the searched one (`my_load_store`) is
/// not a match. After it: the searched name is only a match when `(` follows immediately, so a
/// longer identifier merely STARTING with it (`load_store2`) is not one either.
///
/// The third case is the `fn`-keyword check's own boundary: an identifier ending in the two
/// bytes `fn` sits in exactly the byte positions the keyword would, and must not be mistaken
/// for one — otherwise a real call could be written off as a definition and silently escape the
/// gate. Contrived as Rust, but it is the byte pattern the guard exists for.
#[test]
fn call_matching_respects_identifier_boundaries() {
    let src = "fn a() { my_load_store(); load_store2(); }\nfn b() { load_store(); }\n";
    let calls = find_calls(src, &["load_store"]);
    assert_eq!(calls.len(), 1, "only the bare, paren-terminated call matches, got {calls:?}");
    assert_eq!(calls[0].line, 2);

    let looks_like_fn = "let x = elfn load_store();\n";
    assert_eq!(
        find_calls(looks_like_fn, &["load_store"]).len(),
        1,
        "`elfn` is an identifier, not the `fn` keyword — the call must still be seen"
    );
    assert!(!defines_fn(looks_like_fn, "load_store"));
}

/// A call inside a string literal is code-shaped text, not code — and the stripper's raw-string
/// handling must not let it leak either. Guards the same parity class the env scanner's
/// `raw_string_does_not_leak_quote_parity_into_the_next_comment` covers.
#[test]
fn a_call_named_inside_a_comment_after_a_raw_string_is_not_found() {
    let src = "let q = r#\"a \"b\"#; // load_store()\n";
    assert_eq!(find_calls(src, &["load_store"]), Vec::new());
}

/// `slurp` / `boot.cfg` are INVENTED, for the reason
/// `finds_calls_through_a_path_qualifier_and_skips_comments` states: the settings-registry gate
/// walks this file, and a real opener name written here immediately in front of an open paren —
/// with a real store file name inside it — would be reported as a `vike-ops` credential-store
/// read. Keeping BOTH halves of the needle at the caller is what makes this file inert.
#[test]
fn path_reads_carry_their_argument_text() {
    let src = "//! slurp(\"boot.cfg\") in a doc block\n\
fn a() { let t = std::fs::slurp(\"boot.cfg\"); }\n\
fn b() { let t = slurp(dir.join(\"boot.cfg\")); }\n\
fn slurp(p: &Path) -> String { inner(p) }\n";
    let reads = find_path_reads(src, &["slurp"]);
    assert_eq!(reads.len(), 2, "the doc block and the definition are not call sites: {reads:?}");
    assert_eq!((reads[0].line, reads[0].arg.as_str()), (2, "\"boot.cfg\""));
    assert_eq!((reads[1].line, reads[1].arg.as_str()), (3, "dir.join(\"boot.cfg\")"));
    assert_eq!(reads[0].func, "slurp");
}

/// A nested call, a `)` inside a literal, and a multi-line argument all have to come back
/// whole — the argument is the ONLY thing the caller gets to judge, so a truncated one is a
/// silently missed read rather than a visible error.
#[test]
fn path_read_arguments_survive_nesting_quotes_and_newlines() {
    let src = "fn a() { slurp(base.join(\"a)b\").join(NAME)); }\n\
fn b() {\n    slurp(\n        base.join(\"c\"),\n    );\n}\n";
    let reads = find_path_reads(src, &["slurp"]);
    assert_eq!(reads.len(), 2, "{reads:?}");
    assert_eq!(reads[0].arg, "base.join(\"a)b\").join(NAME)");
    assert!(reads[1].arg.contains("base.join(\"c\")"), "{:?}", reads[1].arg);
}

#[test]
fn string_literals_skips_comments_and_char_literals() {
    let src = "// \"in a comment\"\n\
fn a(s: &str) -> bool { s.starts_with('\"') && s == \"real\" }\n\
fn b() -> &'static str { \"second\" }\n";
    assert_eq!(string_literals(src), vec!["real".to_string(), "second".to_string()]);
}

/// The property `find_map_lookups` used to get from its own inline sweep and now inherits: the
/// extraction must not have changed what it observes.
#[test]
fn string_literals_still_feeds_the_env_sweep() {
    let src = "fn a(v: &Map) { v.get(\"VIKE_KEPT\"); let _ = \"PARTIALLY_FILLED\"; }\n";
    assert_eq!(find_map_lookups(src, &BTreeMap::new()), vec!["VIKE_KEPT".to_string()]);
}

#[test]
fn mentions_ident_requires_both_boundaries() {
    assert!(mentions_ident("a.join(STORE_NAME)", "STORE_NAME"));
    assert!(mentions_ident("STORE_NAME", "STORE_NAME"));
    assert!(!mentions_ident("a.join(STORE_NAMES)", "STORE_NAME"));
    assert!(!mentions_ident("a.join(MY_STORE_NAME)", "STORE_NAME"));
    assert!(!mentions_ident("anything", ""));
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

/// A char literal containing exactly a double quote (`'"'`) must not be mistaken for a
/// string opener — the exact live bug found in `crates/vike-bridge-core/src/credentials.rs`
/// (the `.trim_matches('"')` that has since moved to `crates/vike-secrets/src/dotenv.rs`'s
/// `parse_dotenv`): before the fix, that single byte flipped `strip_comments`'/
/// `find_map_lookups`' quote-parity tracking to "inside a string" for the rest of the file,
/// so every env-shaped literal after it (there, `OKX_BROKER_CODE`/`POLYMARKET_BUILDER_CODE`/
/// `DERIBIT_BROKER_CODE`) silently vanished from the exhaustiveness gate. This test fails
/// without the `strip_comments`/`find_map_lookups` char-literal guard: `VIKE_AFTER_CHAR_LIT`
/// would be swallowed into the (wrongly) still-open "string" that starts at the `'"'`'s
/// middle byte.
#[test]
fn char_literal_double_quote_does_not_invert_string_parity() {
    let src = r#"
fn a(s: &str) -> &str { s.trim_matches('"') }
fn b(vars: &std::collections::HashMap<String, String>) {
    vars.get("VIKE_AFTER_CHAR_LIT");
}
"#;
    let consts = const_table(src);
    assert_eq!(find_map_lookups(src, &consts), vec!["VIKE_AFTER_CHAR_LIT"]);
}

/// Same shape, but the byte-literal spelling `b'"'` — the `b` prefix must not change
/// anything (the check anchors on the `'`, and `b` is just an ordinary byte pushed through
/// beforehand).
#[test]
fn byte_literal_double_quote_does_not_invert_string_parity() {
    let src = r#"
fn a(s: &[u8]) -> bool { s[0] == b'"' }
fn b(vars: &std::collections::HashMap<String, String>) {
    vars.get("VIKE_AFTER_BYTE_LIT");
}
"#;
    let consts = const_table(src);
    assert_eq!(find_map_lookups(src, &consts), vec!["VIKE_AFTER_BYTE_LIT"]);
}
