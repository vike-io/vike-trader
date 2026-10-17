//! The API-reference content rule's pure parts: the line classifier, the lexer's per-line classes and
//! the verdict over a one-file patch. The planted-commit half (the wiring through `compute`) is
//! `xtask/tests/ci_plan_gate/docs_content_edits.rs`.

use super::docs_content::{
    ATTR, BODY, DECLARATION_TOKENS, DOC, ITEM, line_classes, line_is_doc_shaped,
    rust_patch_moves_docs,
};

/// Every declaration token fires at the start of a line, indented or not, and in the shapes it is
/// really written in; the same word NOT first, or as the prefix of a longer identifier, does not.
#[test]
fn every_declaration_token_fires_first_and_only_first() {
    for tok in DECLARATION_TOKENS {
        for line in [format!("{tok} x"), format!("    {tok} x"), format!("\t{tok}")] {
            assert!(line_is_doc_shaped(&line), "{line:?} starts with `{tok}`");
        }
        let later = format!("let y = {tok};");
        assert!(!line_is_doc_shaped(&later), "{later:?}: `{tok}` is not the first token");
        let longer = format!("{}_suffix = 1;", tok.trim_end_matches('!'));
        assert!(!line_is_doc_shaped(&longer), "{longer:?} is an identifier, not `{tok}`");
    }
    for line in [
        "pub(crate) fn f() {",
        "pub(super) struct S;",
        "pub(in crate::a) mod m;",
        "const fn f() {}",
        "async fn f() {}",
        "unsafe impl Send for S {}",
        "extern crate core;",
        "impl<T> Trait for T {",
        "macro_rules! m {",
        "use std::fmt;",
        "type Alias = u8;",
        "static X: u8 = 0;",
        "union U { a: u8 }",
        "trait T {}",
        "enum E {}",
    ] {
        assert!(line_is_doc_shaped(line), "{line:?} is a declaration");
    }
}

/// Doc comments and attributes fire; plain comments, statements, blank lines and the spellings that
/// only LOOK like a doc comment (`////`, `/***`, `/**/`) do not.
#[test]
fn doc_comments_and_attributes_fire_and_their_lookalikes_do_not() {
    for line in [
        "/// a doc line",
        "    /// indented",
        "///",
        "//! an inner doc line",
        "/** a block doc */",
        "/*! an inner block doc */",
        "#[derive(Debug)]",
        "    #[cfg(feature = \"x\")]",
        "#![allow(dead_code)]",
        "#[doc = include_str!(\"../README.md\")]",
        "#[cfg_attr(doc, doc(cfg(feature = \"x\")))]",
    ] {
        assert!(line_is_doc_shaped(line), "{line:?} must fire");
    }
    for line in [
        "",
        "    ",
        "// a plain comment",
        "    // pub fn in a comment",
        "//// four slashes are a plain comment",
        "/*** three stars are a plain comment */",
        "/**/",
        "/* a block comment */",
        "let x = 1;",
        "x.push(y);",
        "}",
        "{",
        "return Ok(());",
        "match v {",
        "Some(x) => x,",
        "\"a string with pub fn inside\"",
        "# not an attribute",
        "self.public = true;",
        "letfn();",
    ] {
        assert!(!line_is_doc_shaped(line), "{line:?} must not fire on its own text");
    }
}

/// The classes of a small file, line by line, so a regression names the line it broke.
fn classes(src: &str) -> Vec<u8> {
    line_classes(src).unwrap_or_else(|| panic!("the lexer refused a balanced file:\n{src}"))
}

#[test]
fn a_function_body_is_body_and_everything_around_it_is_item() {
    let src = "\
/// Docs.
#[inline]
pub fn f(
    a: u32,
) -> u32
where
    u32: Copy,
{
    let x = a + 1;
    // a comment
    x
}
pub enum E {
    A,
    B(u8),
}
";
    let c = classes(src);
    assert_eq!(c[0], DOC, "line 1: a doc comment");
    assert_eq!(c[1], ATTR, "line 2: an attribute");
    for (i, what) in [(2, "the signature"), (3, "a wrapped parameter"), (4, "the return type")] {
        assert_eq!(c[i], ITEM, "line {}: {what}", i + 1);
    }
    assert_eq!(c[5] & ITEM, ITEM, "line 6: `where`");
    assert_eq!(c[6], ITEM, "line 7: a where-clause bound");
    assert_eq!(c[7], BODY, "line 8: the body's opening brace");
    assert_eq!(c[8], BODY, "line 9: a statement");
    assert_eq!(c[9], 0, "line 10: a plain comment in a body is nothing");
    assert_eq!(c[10], BODY, "line 11: the tail expression");
    assert_eq!(c[11], BODY, "line 12: the body's closing brace");
    assert_eq!(c[12], ITEM, "line 13: an enum");
    assert_eq!(c[13], ITEM, "line 14: a variant, which starts with no keyword");
    assert_eq!(c[14], ITEM, "line 15: a tuple variant");
}

#[test]
fn methods_have_bodies_and_trait_declarations_and_fn_pointers_do_not() {
    let src = "\
impl S {
    pub fn m(&self) -> u8 {
        self.0
    }
}
trait T {
    fn decl(&self);
    const C: u8;
}
struct P {
    cb: fn(u8) -> u8,
    next: u8,
}
";
    let c = classes(src);
    assert_eq!(c[0], ITEM, "the impl header");
    assert_eq!(c[1] & ITEM, ITEM, "a method signature");
    assert_eq!(c[1] & BODY, BODY, "...whose `{{` opens its body on the same line");
    assert_eq!(c[2], BODY, "a method body");
    assert_eq!(c[3], BODY, "the method's closing brace");
    assert_eq!(c[4], ITEM, "the impl's closing brace");
    assert_eq!(c[6], ITEM, "a body-less trait method: its `;` ends the wait for a body");
    assert_eq!(c[7], ITEM, "the next trait item is not a body");
    assert_eq!(c[10], ITEM, "a fn-pointer field opens no body");
    assert_eq!(c[11], ITEM, "...and the field after it is still item-level");
}

#[test]
fn strings_chars_lifetimes_and_raw_strings_hide_their_braces() {
    let src = r####"fn f<'a>(s: &'a str) -> char {
    let a = "{ not a brace";
    let b = r#"} nor this, "quoted" inside"#;
    let c = '{';
    let d = '\'';
    let e = b"}";
    'outer: loop { break 'outer; }
    '}'
}
pub struct After;
"####;
    let c = classes(src);
    for (i, line) in src.lines().enumerate().skip(1).take(7) {
        assert_eq!(c[i], BODY, "line {}: {line:?} is inside the body", i + 1);
    }
    assert_eq!(c[8], BODY, "the closing brace is the body's own");
    assert_eq!(c[9], ITEM, "the body closed where it should, so the next item is item-level");
}

#[test]
fn a_raw_identifier_is_a_function_name_and_macro_rules_is_rendered_whole() {
    let src = "\
fn r#type() {
    body();
}
macro_rules! m {
    () => { fn inner() { x } };
}
";
    let c = classes(src);
    assert_eq!(c[1], BODY, "`fn r#type` has a body like any other function");
    assert_eq!(c[4], ITEM, "a macro_rules! arm is rendered, so nothing inside it is a body");
}

#[test]
fn multi_line_strings_comments_and_attributes_mark_every_line() {
    let src = "\
/**
 * a block doc
 */
#[derive(
    Debug,
)]
const S: &str = \"one
two\";
/* plain
 block */
";
    let c = classes(src);
    assert_eq!(&c[0..3], &[DOC, DOC, DOC], "every line of a block doc comment");
    assert_eq!(&c[3..6], &[ATTR, ATTR, ATTR], "every line of a wrapped attribute");
    assert_eq!(&c[6..8], &[ITEM, ITEM], "both lines of an item-level string");
    assert_eq!(&c[8..10], &[0, 0], "a plain block comment is nothing");
}

#[test]
fn an_unbalanced_or_unterminated_file_is_refused() {
    for src in ["fn f() {", "}", "let s = \"open", "/* open", "fn f() { ) }", "let c = '\\"] {
        assert!(line_classes(src).is_none(), "{src:?} must be refused, and the caller then fires");
    }
}

/// A one-file `-U0` patch: header, then hunks. `old`/`new` are the whole file before and after.
fn patch(hunks: &str) -> String {
    format!(
        "diff --git a/crates/x/src/lib.rs b/crates/x/src/lib.rs\nindex 1111111..2222222 100644\n\
         --- a/crates/x/src/lib.rs\n+++ b/crates/x/src/lib.rs\n{hunks}"
    )
}

const OLD: &str = "\
/// Docs.
pub fn f(a: u32) -> u32 {
    let x = a + 1;
    x
}
pub enum E {
    A,
}
fn helper() {}
";

#[test]
fn a_body_only_edit_does_not_fire() {
    let new = OLD.replace("a + 1", "a + 2");
    let p =
        patch("@@ -3 +3 @@ pub fn f(a: u32) -> u32 {\n-    let x = a + 1;\n+    let x = a + 2;\n");
    assert!(
        !rust_patch_moves_docs(&p, Some(OLD), Some(new.as_str())),
        "a statement changed, nothing else"
    );
    let new = OLD.replace("    let x = a + 1;\n", "    let x = a + 1;\n    // why\n");
    let p = patch("@@ -3,0 +4 @@ pub fn f(a: u32) -> u32 {\n+    // why\n");
    assert!(
        !rust_patch_moves_docs(&p, Some(OLD), Some(new.as_str())),
        "a plain comment added in a body"
    );
}

#[test]
fn doc_signature_variant_and_private_rename_edits_fire() {
    let cases: [(&str, String, &str); 5] = [
        (
            "a doc comment",
            OLD.replace("/// Docs.", "/// Better docs."),
            "@@ -1 +1 @@\n-/// Docs.\n+/// Better docs.\n",
        ),
        (
            "a pub signature",
            OLD.replace("f(a: u32)", "f(a: u64)"),
            "@@ -2 +2 @@\n-pub fn f(a: u32) -> u32 {\n+pub fn f(a: u64) -> u32 {\n",
        ),
        (
            "an enum variant",
            OLD.replace("    A,\n", "    A,\n    B,\n"),
            "@@ -7,0 +8 @@ pub enum E {\n+    B,\n",
        ),
        (
            "a private fn rename (a doc link may name it)",
            OLD.replace("fn helper()", "fn helper2()"),
            "@@ -9 +9 @@\n-fn helper() {}\n+fn helper2() {}\n",
        ),
        (
            "an attribute",
            OLD.replace("/// Docs.\n", "/// Docs.\n#[must_use]\n"),
            "@@ -1,0 +2 @@\n+#[must_use]\n",
        ),
    ];
    for (what, new, hunks) in cases {
        assert!(
            rust_patch_moves_docs(&patch(hunks), Some(OLD), Some(new.as_str())),
            "{what} must fire"
        );
    }
}

#[test]
fn every_doubt_fires() {
    let body = "@@ -3 +3 @@\n-    let x = a + 1;\n+    let x = a + 2;\n";
    let new = OLD.replace("a + 1", "a + 2");
    assert!(
        rust_patch_moves_docs(&patch(body), None, Some(new.as_str())),
        "an unreadable base text"
    );
    assert!(rust_patch_moves_docs(&patch(body), Some(OLD), None), "an unreadable head text");
    assert!(
        rust_patch_moves_docs(&patch(body), Some("fn f() {"), Some(new.as_str())),
        "an unlexable file"
    );
    let added = "diff --git a/x.rs b/x.rs\nnew file mode 100644\nindex 0000000..2222222\n--- /dev/null\n\
                 +++ b/x.rs\n@@ -0,0 +1 @@\n+fn f() {}\n";
    assert!(rust_patch_moves_docs(added, Some(""), Some("fn f() {}\n")), "a new file");
    let binary =
        "diff --git a/x.rs b/x.rs\nindex 1..2 100644\nBinary files a/x.rs and b/x.rs differ\n";
    assert!(rust_patch_moves_docs(binary, Some(OLD), Some(OLD)), "a binary diff");
    assert!(rust_patch_moves_docs(&patch(""), Some(OLD), Some(OLD)), "a diff with no changed line");
    assert!(
        rust_patch_moves_docs(&patch("@@ garbage @@\n+x\n"), Some(OLD), Some(OLD)),
        "an unreadable hunk header"
    );
    assert!(
        rust_patch_moves_docs(&patch("@@ -40 +40 @@\n-    x\n+    y\n"), Some(OLD), Some(OLD)),
        "a line number past the end of the file"
    );
}
