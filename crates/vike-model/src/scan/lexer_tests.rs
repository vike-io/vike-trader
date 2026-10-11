//! Unit tests for the lexical layer (`lexer.rs`), driven through the public scanners.

use std::collections::BTreeMap;

use super::*;

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

/// A char literal containing exactly a double quote (`'"'`) must not be mistaken for a
/// string opener — the exact live bug found in `crates/vike-bridge-core/src/credentials.rs`
/// (the `.trim_matches('"')` in its credential-file parser, since deleted): before the fix, that single byte flipped `strip_comments`'/
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
