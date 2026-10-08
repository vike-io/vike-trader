//! Unit tests for the name-keyed scanners (`calls.rs`).

use super::*;

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

/// The raw-text guard in front of `find_calls`, `defines_fn` and `find_path_reads` never hides a
/// match: every spelling those matchers accept is still found, and the only files the guard
/// answers without stripping are files that cannot hold one. The names are the invented ones the
/// tests above use, for the reason `finds_calls_through_a_path_qualifier_and_skips_comments` gives.
///
/// ⚠ The spacings are the point. The definition check skips any run of spaces and tabs between
/// `fn` and the name, and a call is matched through any path qualifier or opening bracket — so a
/// guard that looked for `fn NAME(` or ` NAME(` would read plausibly and drop exactly these. That
/// mutation (the `defines_fn` guard spelled `fn {name}(`) was RUN: the tab and double-space
/// definitions below go red. So was the several-name `(` scan reading its identifier one byte
/// short: the two-name call cases go red.
#[test]
fn the_name_guard_never_hides_a_match() {
    for src in [
        "pub fn load_store(x: u8) {}\n",
        "fn  load_store() {}\n",
        "fn\tload_store() {}\n",
        "// fn other()\nfn load_store() {} // trailing note\n",
        "pub(crate) fn load_store(\n    home: Option<&Path>,\n) -> Map {\n",
    ] {
        assert!(defines_fn(src, "load_store"), "a definition the matcher accepts: {src:?}");
    }
    for src in [
        "fn f() { crate::store::load_store(); }\n",
        "fn f() {\n    // a note\n    let m = load_store(x);\n}\n",
        "fn f() { g(load_store(a)); }\n",
        "fn f() { [load_store(a)]; }\n",
    ] {
        assert_eq!(find_calls(src, &["read_creds", "load_store"]).len(), 1, "{src:?}");
    }
    assert_eq!(find_path_reads("fn f() { std::fs::slurp(p); }\n", &["peek", "slurp"]).len(), 1);
    // A QUALIFIED name is matched as a substring, so the one-pass `(` scan (bare names only) must
    // not be what answers for it.
    assert_eq!(
        find_calls("fn f() { crate::store::load_store(); }\n", &["store::load_store", "x"]).len(),
        1
    );
    // …and what the guard skips is only what the matchers would have rejected anyway — through
    // the one-name substring search and the several-name `(` scan alike.
    for src in [
        "// load_store(x)\nfn f() {}\n",
        "fn f() { other(); }\n",
        "use a::load_store;\n",
        "fn f() { my_load_store(); load_store2(); }\n",
    ] {
        assert!(!defines_fn(src, "load_store"), "{src:?}");
        for names in [&["load_store"][..], &["read_creds", "load_store"][..]] {
            assert!(find_calls(src, names).is_empty(), "{src:?} {names:?}");
            assert!(find_path_reads(src, names).is_empty(), "{src:?} {names:?}");
        }
    }
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
