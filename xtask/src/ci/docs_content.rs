//! The CONTENT half of the plan's `docs` output: does an edit of a documented crate's file change what
//! the `api-docs` job renders or checks?
//!
//! [`super::roster::docs_affected`] already asks it only of the crates that OWN a changed, linkable file
//! (a crate document, a test-target source and a crate the roster excludes never fire). This module
//! narrows the remaining case, an edit of a documented crate's own sources, by READING THE DIFF: the
//! owner's rule (decision 0133) is "run it only when documentation or public items change".
//!
//! # What fires
//!
//! A file-level rule, no content read:
//!
//!   * the crate's `Cargo.toml`, `build.rs` or a `build/` module (features, `description`, `readme`, the
//!     generated code a build script emits);
//!   * any file that is not a `.rs` source (a `.json`, `.tsv` or `.sql` beside the sources is
//!     `include_str!`ed, and this module cannot tell into what);
//!   * a new or deleted `.rs` file, a mode change, a binary diff and a diff git could not produce
//!     (`--no-renames`, so a rename is a deletion plus an addition and fires too).
//!
//! A `.rs` edit fires when ANY added or removed line is:
//!
//!   * doc-shaped or declaration-shaped ([`line_is_doc_shaped`]): a doc comment (`///`, `//!`, `/**`,
//!     `/*!`), an attribute (`#[`, `#![`: covers `#[doc]`, `#[cfg]`, `cfg_attr(doc …)`), or a line whose
//!     first token is `pub`, `fn`, `struct`, `enum`, `trait`, `type`, `const`, `static`, `mod`, `use`,
//!     `impl`, `macro_rules!`, `union`, `extern`, `unsafe` or `async`; OR
//!   * CODE OUTSIDE A FUNCTION BODY ([`line_classes`]): an enum variant, a field, a line of a signature
//!     or `where` clause that wraps, a `macro_rules!` arm, an item-level macro invocation, a `const`
//!     initialiser. None of those lines starts with a keyword, and each of them is rendered.
//!
//! What does NOT fire: a `.rs` edit whose every changed line is a statement inside a function body, a
//! plain `//` or `/* */` comment, or blank.
//!
//! # Why that is the whole of what the job can see
//!
//! rustdoc renders doc comments and item signatures, and does not type-check function bodies. The job
//! fails on three things: the `cargo doc` build (with `-D rustdoc::broken_intra_doc_links`), the mirror
//! tree's forbidden-token scan of the SOURCE, and the same scan of the built HTML.
//!
//!   * A broken intra-doc link needs a changed doc comment or a renamed or removed link target, even a
//!     private one; a target is an item, so its declaration line changes and fires.
//!   * A function body IS rendered, on the crate's `src/` page, so a body edit changes the HTML. What it
//!     can put there is the text of the source, which the mirror's scan judges; and `vike-ops`'s
//!     `publish_mirror_gate` runs that same scan (`scripts/publish_mirror.sh --dry-run`) on every pull
//!     request whose added or removed lines carry one of the token shapes the redaction does not remove
//!     (its `tokens` row in `super::tables::gate_triggers::GATE_TRIGGERS`).
//!   * A body that stops compiling fails the `test` job first, on the same pull request.
//!
//! # What this cannot see (decision 0133 states it too)
//!
//!   * A rustdoc behaviour that depends on a body: the evaluated value rustdoc prints beside an integer
//!     `const` whose initialiser calls a `const fn` (the `fn`'s body edit does not fire), and an `impl`
//!     written inside a function body is rendered but fires only through its `impl` line.
//!   * A doc page that `include_str!`s a file outside every crate fires only through
//!     [`super::tables::readers::DOCS_INCLUDED_FILES`] (empty today; a test holds it to the tree).
//!   * The lexer is a lexer, not a parser: an unbalanced brace inside a const generic argument of a `fn`
//!     signature reads as that function's body opening. A file it cannot lex to the end (an unterminated
//!     string or comment, unbalanced delimiters) FIRES.

use std::path::Path;

use super::git;

/// A line holds a doc comment (`///`, `//!`, `/** */`, `/*! */`, or a line of a doc block comment).
pub const DOC: u8 = 1;
/// A line holds part of an attribute (`#[…]`, `#![…]`, including the lines of a wrapped one).
pub const ATTR: u8 = 2;
/// A line holds code OUTSIDE every function body: an item, a signature, a field, a variant.
pub const ITEM: u8 = 4;
/// A line holds code inside a function body (its braces included).
pub const BODY: u8 = 8;

/// The first tokens that make a line declaration-shaped ([`line_is_doc_shaped`]).
pub const DECLARATION_TOKENS: &[&str] = &[
    "pub",
    "fn",
    "struct",
    "enum",
    "trait",
    "type",
    "const",
    "static",
    "mod",
    "use",
    "impl",
    "macro_rules!",
    "union",
    "extern",
    "unsafe",
    "async",
];

/// Does changing `path` (a file of a documented crate, `rel` relative to that crate) between `base` and
/// `HEAD` move the API reference? DOUBT IS `true`: every unreadable input fires.
pub fn edit_moves_docs(base: &str, path: &str, rel: &str, cwd: &Path) -> bool {
    if rel == "Cargo.toml"
        || rel == "build.rs"
        || rel.starts_with("build/")
        || !rel.ends_with(".rs")
    {
        return true;
    }
    let Some(patch) = git::file_patch(base, path, cwd) else { return true };
    let old = git::blob_at(base, path, cwd);
    let new = git::blob_at("HEAD", path, cwd);
    rust_patch_moves_docs(&patch, old.as_deref(), new.as_deref())
}

/// [`edit_moves_docs`]'s verdict over a `git diff -U0` of ONE `.rs` file and the file's text before
/// (`old`) and after (`new`). `None` for either text is doubt, and fires.
pub fn rust_patch_moves_docs(patch: &str, old: Option<&str>, new: Option<&str>) -> bool {
    let (Some(old), Some(new)) = (old, new) else { return true };
    let (Some(old_cls), Some(new_cls)) = (line_classes(old), line_classes(new)) else {
        return true;
    };
    // A changed line fires on its text or on where it sits; a line number the classes do not cover
    // is doubt.
    let fires = |cls: &[u8], ln: usize, text: &str| {
        line_is_doc_shaped(text)
            || ln == 0
            || cls.get(ln - 1).is_none_or(|c| c & (DOC | ATTR | ITEM) != 0)
    };
    let (mut old_ln, mut new_ln) = (0usize, 0usize);
    let mut in_hunk = false;
    let mut changed = false;
    for line in patch.lines() {
        if let Some(header) = line.strip_prefix("@@") {
            let Some((o, n)) = hunk_starts(header) else { return true };
            (old_ln, new_ln, in_hunk) = (o, n, true);
            continue;
        }
        if !in_hunk {
            // A plain edit's header is exactly these four; `new file mode`, `deleted file mode`,
            // `old mode`, `Binary files` and the rest are not an edit of text a lexer can judge.
            let plain = ["diff --git ", "index ", "--- ", "+++ "];
            if plain.iter().any(|p| line.starts_with(p)) {
                continue;
            }
            return true;
        }
        match line.as_bytes().first() {
            Some(b'-') => {
                changed = true;
                if fires(&old_cls, old_ln, &line[1..]) {
                    return true;
                }
                old_ln += 1;
            }
            Some(b'+') => {
                changed = true;
                if fires(&new_cls, new_ln, &line[1..]) {
                    return true;
                }
                new_ln += 1;
            }
            Some(b' ') => {
                old_ln += 1;
                new_ln += 1;
            }
            // "\ No newline at end of file" belongs to the line above it, which was judged.
            Some(b'\\') => {}
            _ => return true,
        }
    }
    // A diff with no changed line is not one this module understands.
    !changed
}

/// The first old and new line numbers of a hunk header's tail (` -a[,b] +c[,d] @@ …`), 1-based; a
/// count of 0 names the line BEFORE the empty range, which no changed line is read against.
fn hunk_starts(header: &str) -> Option<(usize, usize)> {
    let mut parts = header.split_whitespace();
    let num = |s: Option<&str>, sign: char| -> Option<usize> {
        let s = s?.strip_prefix(sign)?;
        s.split(',').next()?.parse().ok()
    };
    let old = num(parts.next(), '-')?;
    let new = num(parts.next(), '+')?;
    Some((old, new))
}

/// The PURE line classifier: is this line (the text of one added or removed line) doc-shaped or
/// declaration-shaped? A doc comment, an attribute, or a first token in [`DECLARATION_TOKENS`] (after
/// leading whitespace; `pub(crate)` is the token `pub`).
pub fn line_is_doc_shaped(line: &str) -> bool {
    let t = line.trim_start();
    let b = t.as_bytes();
    let at = |i: usize| b.get(i).copied();
    // `///` but not `////`, `//!`; `/**` but not `/***` or the empty `/**/`, `/*!`.
    let doc_line = t.starts_with("//!") || (t.starts_with("///") && at(3) != Some(b'/'));
    let doc_block = t.starts_with("/*!")
        || (t.starts_with("/**") && at(3) != Some(b'*') && at(3) != Some(b'/'));
    if doc_line || doc_block || t.starts_with("#[") || t.starts_with("#![") {
        return true;
    }
    let first: String =
        t.chars().take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '!').collect();
    DECLARATION_TOKENS.contains(&first.as_str())
}

/// Per line of `src` (index 0 = line 1), the OR of [`DOC`], [`ATTR`], [`ITEM`] and [`BODY`] for what the
/// line holds; 0 for a line of whitespace and plain comments only. `None` when the text does not lex to
/// a balanced end (an unterminated string or comment, a stray closing delimiter): the caller fires.
///
/// A function BODY is the `{ … }` group after `fn NAME` (at the brace and paren depth the name was read
/// at), unless a `;` there ends a body-less declaration first. `fn` followed by `(` is a function
/// POINTER type and opens nothing. Inside a body every token is [`BODY`] (nested items included: rustdoc
/// documents none of them); inside a `macro_rules!` definition nothing opens a body, because the
/// definition is rendered whole.
pub fn line_classes(src: &str) -> Option<Vec<u8>> {
    let c: Vec<char> = src.chars().collect();
    let n = c.len();
    let at = |i: usize| c.get(i).copied().unwrap_or('\0');
    let mut cls = vec![0u8; src.split('\n').count()];
    let mut line = 0usize;
    let mut depth = 0usize;
    let mut paren = 0usize;
    // `Some(d)`: inside the outermost function body, opened at brace depth `d`.
    let mut body: Option<usize> = None;
    // `Some((brace, paren))`: `fn NAME` was read there and its body brace is awaited.
    let mut pending: Option<(usize, usize)> = None;
    let mut after_fn = false;
    // `Some(p)`: inside an attribute whose `[` opened at paren depth `p`.
    let mut attr: Option<usize> = None;
    // `macro_rules ! NAME {…}`: 1 after `macro_rules`, 2 after `!`, 3 after the name; then the group.
    let mut macro_step = 0u8;
    // `Some((open, depth))`: inside a `macro_rules!` definition whose delimiter `open` opened at `depth`
    // (brace depth for `{`, paren depth for `(`/`[`).
    let mut macro_def: Option<(char, usize)> = None;
    let mut i = 0usize;

    macro_rules! ctx {
        () => {
            if attr.is_some() {
                ATTR
            } else if body.is_some() {
                BODY
            } else {
                ITEM
            }
        };
    }
    macro_rules! mark {
        ($flag:expr) => {
            if let Some(slot) = cls.get_mut(line) {
                *slot |= $flag;
            }
        };
    }

    while i < n {
        let ch = c[i];
        // Every arm below sets this for the token it consumed; whitespace and comments keep the last.
        let mut ident: Option<String> = None;
        let mut significant = true;
        match ch {
            '\n' => {
                line += 1;
                i += 1;
                significant = false;
            }
            w if w.is_whitespace() => {
                i += 1;
                significant = false;
            }
            '/' if at(i + 1) == '/' => {
                let doc = (at(i + 2) == '/' && at(i + 3) != '/') || at(i + 2) == '!';
                while i < n && c[i] != '\n' {
                    i += 1;
                }
                if doc {
                    mark!(DOC);
                }
                significant = false;
            }
            '/' if at(i + 1) == '*' => {
                let doc =
                    at(i + 2) == '!' || (at(i + 2) == '*' && at(i + 3) != '*' && at(i + 3) != '/');
                if doc {
                    mark!(DOC);
                }
                i += 2;
                let mut nest = 1usize;
                while i < n && nest > 0 {
                    if c[i] == '\n' {
                        line += 1;
                        if doc {
                            mark!(DOC);
                        }
                        i += 1;
                    } else if c[i] == '/' && at(i + 1) == '*' {
                        nest += 1;
                        i += 2;
                    } else if c[i] == '*' && at(i + 1) == '/' {
                        nest -= 1;
                        i += 2;
                    } else {
                        i += 1;
                    }
                }
                if nest > 0 {
                    return None;
                }
                significant = false;
            }
            '"' => {
                let flag = ctx!();
                mark!(flag);
                i += 1;
                loop {
                    match c.get(i) {
                        None => return None,
                        Some('\\') => {
                            if at(i + 1) == '\n' {
                                line += 1;
                                mark!(flag);
                            }
                            i += 2;
                        }
                        Some('"') => {
                            i += 1;
                            break;
                        }
                        Some('\n') => {
                            line += 1;
                            mark!(flag);
                            i += 1;
                        }
                        Some(_) => i += 1,
                    }
                }
            }
            '\'' => {
                mark!(ctx!());
                if at(i + 1) == '\\' {
                    // An escaped char literal: skip the escaped char, then up to the closing quote.
                    i += 3;
                    while i < n && c[i] != '\'' && c[i] != '\n' {
                        i += 1;
                    }
                    if at(i) != '\'' {
                        return None;
                    }
                    i += 1;
                } else if at(i + 2) == '\'' && at(i + 1) != '\n' {
                    i += 3;
                } else {
                    // A lifetime or a loop label.
                    i += 1;
                    while i < n && (c[i].is_alphanumeric() || c[i] == '_') {
                        i += 1;
                    }
                }
            }
            a if a.is_alphabetic() || a == '_' => {
                let start = i;
                while i < n && (c[i].is_alphanumeric() || c[i] == '_') {
                    i += 1;
                }
                let word: String = c[start..i].iter().collect();
                let flag = ctx!();
                mark!(flag);
                if matches!(word.as_str(), "r" | "br" | "cr") && matches!(at(i), '#' | '"') {
                    let mut hashes = 0usize;
                    while at(i + hashes) == '#' {
                        hashes += 1;
                    }
                    if at(i + hashes) == '"' {
                        // A raw string: up to `"` followed by the same number of `#`.
                        i += hashes + 1;
                        loop {
                            match c.get(i) {
                                None => return None,
                                Some('\n') => {
                                    line += 1;
                                    mark!(flag);
                                    i += 1;
                                }
                                Some('"') if (1..=hashes).all(|k| at(i + k) == '#') => {
                                    i += 1 + hashes;
                                    break;
                                }
                                Some(_) => i += 1,
                            }
                        }
                        // A literal, not an identifier.
                        after_fn = false;
                        macro_step = 0;
                        continue;
                    }
                    if word == "r" && hashes == 1 {
                        // A raw identifier (`r#type`): a name, never a keyword.
                        let name_start = i + 1;
                        i += 1;
                        while i < n && (c[i].is_alphanumeric() || c[i] == '_') {
                            i += 1;
                        }
                        let name: String = c[name_start..i].iter().collect();
                        ident = Some(format!("r#{name}"));
                    }
                }
                if ident.is_none() {
                    ident = Some(word);
                }
            }
            d if d.is_ascii_digit() => {
                mark!(ctx!());
                while i < n && (c[i].is_alphanumeric() || c[i] == '_') {
                    i += 1;
                }
            }
            '#' => {
                let mut j = i + 1;
                while at(j) == ' ' || at(j) == '\t' {
                    j += 1;
                }
                if at(j) == '!' {
                    j += 1;
                    while at(j) == ' ' || at(j) == '\t' {
                        j += 1;
                    }
                }
                if at(j) == '[' && attr.is_none() {
                    attr = Some(paren);
                }
                mark!(ctx!());
                i += 1;
            }
            '{' => {
                let opens_body = body.is_none()
                    && attr.is_none()
                    && macro_def.is_none()
                    && pending == Some((depth, paren));
                if opens_body {
                    body = Some(depth);
                    pending = None;
                    mark!(BODY);
                } else {
                    if macro_step == 3 && macro_def.is_none() {
                        macro_def = Some(('{', depth));
                    }
                    mark!(ctx!());
                }
                depth += 1;
                i += 1;
            }
            '}' => {
                if depth == 0 {
                    return None;
                }
                depth -= 1;
                if body == Some(depth) {
                    mark!(BODY);
                    body = None;
                } else {
                    mark!(ctx!());
                }
                if pending.is_some_and(|(d, _)| d > depth) {
                    pending = None;
                }
                if macro_def == Some(('{', depth)) {
                    macro_def = None;
                }
                i += 1;
            }
            '(' | '[' => {
                if macro_step == 3 && macro_def.is_none() {
                    macro_def = Some((ch, paren));
                }
                mark!(ctx!());
                paren += 1;
                i += 1;
            }
            ')' | ']' => {
                if paren == 0 {
                    return None;
                }
                paren -= 1;
                mark!(ctx!());
                if ch == ']' && attr == Some(paren) {
                    attr = None;
                }
                if matches!(macro_def, Some((open, p)) if open != '{' && p == paren) {
                    macro_def = None;
                }
                i += 1;
            }
            ';' => {
                if pending == Some((depth, paren)) {
                    pending = None;
                }
                mark!(ctx!());
                i += 1;
            }
            _ => {
                mark!(ctx!());
                i += 1;
            }
        }
        if !significant {
            continue;
        }
        // The `fn NAME` and `macro_rules ! NAME` recognisers, over significant tokens only.
        let in_item_scope = body.is_none() && attr.is_none() && macro_def.is_none();
        match ident.as_deref() {
            Some(word) => {
                // `fn` then a NAME: a function item, whose body brace is awaited. `fn` then `(` is a
                // function pointer type, and any other token ends the recogniser.
                if after_fn && in_item_scope {
                    pending = Some((depth, paren));
                }
                after_fn = word == "fn" && in_item_scope;
                macro_step = match (macro_step, word) {
                    (_, "macro_rules") => 1,
                    (2, _) => 3,
                    _ => 0,
                };
            }
            None => {
                after_fn = false;
                macro_step = if macro_step == 1 && ch == '!' { 2 } else { 0 };
            }
        }
    }
    if depth != 0 || paren != 0 || body.is_some() {
        return None;
    }
    Some(cls)
}
