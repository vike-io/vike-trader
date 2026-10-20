//! The "this name moved" HINT for user Rust source: the market-data names a file still reaches
//! through `vike_model`, and the one-line fix, for an operator whose strategy or study stopped
//! compiling with rustc's bare "unresolved import".
//!
//! The bar, tick, book and consolidator names live ONLY at `vike_marketdata`'s root
//! ([`MOVED_NAMES`] is the list). Nothing is migrated: a user file under `user_data` is the
//! operator's own and outside the repo, so the owner ruled that the tooling PRINTS A HINT and
//! changes nothing. Three entry points show it, each next to rustc's own error and never instead of
//! it: the two compiled user-code hosts' build scripts (a `cargo:warning=` line per file, through
//! [`folder_hints`] and the driver's `Built::warnings`) and the plugin builder
//! (`crates/vike-strategy-builder/src/render.rs`'s `build_plugin`, appended to a `Compile` failure).
//!
//! [`moved_name_hint`] reads source TEXT, not a syntax tree: comments, string and char literals are
//! blanked first, then every `vike_model` `::` path is followed — one name, a `{ .. }` group,
//! groups nested in groups, any layout across lines. A group item that goes on into a sub-module
//! (`params::Params`, `strategy::{..}`) names nothing at the root and is skipped; so are `self`,
//! `*` and an `as` rename's new name. A glob import names nothing, so it gets no hint.
//!
//! ⚠ **No literal in this module, its tests or its callers spells `vike_model` and a moved name
//! joined by `::`.** The market-data codemod rewrites every such spelling it finds, string literals
//! included, so a needle or a golden source written that way would be rewritten into the NEW
//! spelling and the hint would test nothing. Every one is built from parts
//! (`format!("{}::{}", "vike_model", name)`), and the message itself never needs the old spelling.
//!
//! Pure except [`folder_hints`], which reads files; no environment and no clock (the
//! [`crate::host_build`] module doc says why that matters here).

use std::fs;
use std::path::{Path, PathBuf};

/// The names that left `vike_model`'s root for `vike_marketdata`'s: the market-data codemod's name
/// table, in its order. `crates/vike-model/tests/host_build/moved_name.rs` compiles a `use` of
/// every one from `vike_marketdata` and compares it with this list, so a name missing there or
/// spelled wrong here fails a test.
pub const MOVED_NAMES: [&str; 17] = [
    "Bar",
    "BarConsolidator",
    "BookLevel",
    "BookUpdate",
    "BookUpdateKind",
    "DeltaDecision",
    "FillSim",
    "L2Book",
    "OptionKind",
    "QuoteTick",
    "SeqPolicy",
    "TradeTick",
    "book_taker_price",
    "consolidate_quotes",
    "consolidate_trades",
    "quote_tick_to_bar",
    "trade_tick_to_bar",
];

/// The crate the names left, a constant so no needle in this module is a joined literal.
const OLD_ROOT: &str = "vike_model";

/// ONE operator-facing message for one file of user source, or `None` when the file reaches no
/// moved name through `vike_model`. Names come in the order the file first mentions them, each
/// once. The message is ONE line (a `cargo:warning=` directive cannot carry two):
///
/// ```text
/// `Bar` moved from `vike_model` to `vike_marketdata`: write `use vike_marketdata::Bar;`, keep the rest in `use vike_model::{…}`, and start any inline path to it with `vike_marketdata::`
/// ```
pub fn moved_name_hint(source: &str) -> Option<String> {
    let names = moved_names_in(source);
    if names.is_empty() {
        return None;
    }
    let listed: Vec<String> = names.iter().map(|n| format!("`{n}`")).collect();
    let (import, pronoun) = match names.as_slice() {
        [one] => ((*one).to_string(), "it"),
        many => (format!("{{{}}}", many.join(", ")), "them"),
    };
    Some(format!(
        "{} moved from `{OLD_ROOT}` to `vike_marketdata`: write `use vike_marketdata::{import};`, \
         keep the rest in `use {OLD_ROOT}::{{…}}`, and start any inline path to {pronoun} with \
         `vike_marketdata::`",
        listed.join(", ")
    ))
}

/// Every `.rs` file under `dir` (recursively, in sorted path order) whose source draws a
/// [`moved_name_hint`], as `<file>: <hint>` lines — one per file, each a ready `cargo:warning=`
/// body. A file that cannot be read or is not UTF-8 is skipped: the hint is advisory, and rustc
/// reports such a file itself.
pub fn folder_hints(dir: &Path) -> Vec<String> {
    let mut files = Vec::new();
    rust_files(dir, &mut files);
    files.sort();
    files
        .iter()
        .filter_map(|f| {
            let source = fs::read_to_string(f).ok()?;
            moved_name_hint(&source).map(|hint| format!("{}: {hint}", f.display()))
        })
        .collect()
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        match entry.file_type() {
            Ok(t) if t.is_dir() => rust_files(&path, out),
            Ok(t) if t.is_file() && path.extension().is_some_and(|e| e == "rs") => out.push(path),
            _ => {}
        }
    }
}

/// The moved names `source` reaches through `vike_model`, first mention first, each once.
fn moved_names_in(source: &str) -> Vec<&'static str> {
    let toks = tokens(source);
    let mut found = Vec::new();
    for (k, tok) in toks.iter().enumerate() {
        if *tok != Tok::Ident(OLD_ROOT) || toks.get(k + 1) != Some(&Tok::PathSep) {
            continue;
        }
        match toks.get(k + 2) {
            Some(Tok::Ident(name)) => note(name, &mut found),
            Some(Tok::Open) => {
                group(&toks, k + 3, &mut found);
            }
            _ => {}
        }
    }
    found
}

fn note(name: &str, found: &mut Vec<&'static str>) {
    if let Some(moved) = MOVED_NAMES.iter().find(|m| **m == name)
        && !found.contains(moved)
    {
        found.push(moved);
    }
}

/// Walk the items of a `{ .. }` group whose `{` is just before `i`; returns the index after its
/// `}`. An item that IS a name is noted, a nested group is walked as the same root, and anything
/// else (a sub-module path, `self`, `*`) is skipped to the next `,` or `}` at this depth.
fn group(toks: &[Tok<'_>], mut i: usize, found: &mut Vec<&'static str>) -> usize {
    loop {
        match toks.get(i) {
            None => return i,
            Some(Tok::Close) => return i + 1,
            Some(Tok::Comma) => {
                i += 1;
                continue;
            }
            Some(Tok::Open) => i = group(toks, i + 1, found),
            Some(Tok::Ident(name)) if toks.get(i + 1) != Some(&Tok::PathSep) => {
                note(name, found);
                i += 1;
            }
            Some(_) => {}
        }
        i = item_end(toks, i);
    }
}

/// The index of the `,` or `}` that ends the group item containing `i`, at the item's own depth.
fn item_end(toks: &[Tok<'_>], mut i: usize) -> usize {
    let mut depth = 0usize;
    while let Some(t) = toks.get(i) {
        match t {
            Tok::Open => depth += 1,
            Tok::Close if depth == 0 => return i,
            Tok::Close => depth -= 1,
            Tok::Comma if depth == 0 => return i,
            _ => {}
        }
        i += 1;
    }
    i
}

/// The few token kinds a path walk needs; everything else is `Other`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tok<'a> {
    Ident(&'a str),
    PathSep,
    Open,
    Close,
    Comma,
    Other,
}

/// Lex `src` into [`Tok`]s, dropping comments (line, nested block, doc) and turning every string,
/// raw string, byte string and char literal into one `Other`, so neither can supply a path.
fn tokens(src: &str) -> Vec<Tok<'_>> {
    let b = src.as_bytes();
    let n = b.len();
    let at = |j: usize| b.get(j).copied().unwrap_or(0);
    let mut toks = Vec::new();
    let mut i = 0;
    while i < n {
        let c = b[i];
        if c == b'/' && at(i + 1) == b'/' {
            while i < n && b[i] != b'\n' {
                i += 1;
            }
        } else if c == b'/' && at(i + 1) == b'*' {
            i = block_comment_end(b, i + 2);
        } else if c == b'"' {
            i = quoted_end(b, i + 1, b'"');
            toks.push(Tok::Other);
        } else if c == b'\'' {
            i = char_or_lifetime_end(b, i);
            toks.push(Tok::Other);
        } else if c.is_ascii_digit() {
            while i < n && is_ident_byte(b[i]) {
                i += 1;
            }
            toks.push(Tok::Other);
        } else if is_ident_byte(c) {
            let start = i;
            while i < n && is_ident_byte(b[i]) {
                i += 1;
            }
            let word = &src[start..i];
            let next = at(i);
            if matches!(word, "r" | "br" | "cr")
                && (next == b'"' || next == b'#')
                && let Some(end) = raw_string_end(b, i)
            {
                i = end;
                toks.push(Tok::Other);
            } else if word == "r" && next == b'#' && is_ident_byte(at(i + 1)) {
                // A raw identifier, `r#name`: the name is the identifier.
                let s = i + 1;
                i = s;
                while i < n && is_ident_byte(b[i]) {
                    i += 1;
                }
                toks.push(Tok::Ident(&src[s..i]));
            } else if matches!(word, "b" | "c") && next == b'"' {
                i = quoted_end(b, i + 1, b'"');
                toks.push(Tok::Other);
            } else if word == "b" && next == b'\'' {
                i = quoted_end(b, i + 1, b'\'');
                toks.push(Tok::Other);
            } else {
                toks.push(Tok::Ident(word));
            }
        } else if c == b':' && at(i + 1) == b':' {
            toks.push(Tok::PathSep);
            i += 2;
        } else {
            match c {
                b'{' => toks.push(Tok::Open),
                b'}' => toks.push(Tok::Close),
                b',' => toks.push(Tok::Comma),
                _ if c.is_ascii_whitespace() => {}
                _ => toks.push(Tok::Other),
            }
            i += 1;
        }
    }
    toks
}

/// An identifier byte: ASCII alphanumeric, `_`, or any byte of a non-ASCII character (Rust admits
/// Unicode identifiers, and every byte boundary this leaves is an ASCII one).
fn is_ident_byte(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_' || c >= 0x80
}

/// One past the `*/` closing a block comment whose `/*` ends just before `i` (nesting counted).
fn block_comment_end(b: &[u8], mut i: usize) -> usize {
    let mut depth = 1usize;
    while i < b.len() && depth > 0 {
        if b[i] == b'/' && b.get(i + 1) == Some(&b'*') {
            depth += 1;
            i += 2;
        } else if b[i] == b'*' && b.get(i + 1) == Some(&b'/') {
            depth -= 1;
            i += 2;
        } else {
            i += 1;
        }
    }
    i
}

/// One past the unescaped `quote` closing a literal whose body starts at `i`.
fn quoted_end(b: &[u8], mut i: usize, quote: u8) -> usize {
    while i < b.len() {
        if b[i] == b'\\' {
            i += 2;
        } else if b[i] == quote {
            return i + 1;
        } else {
            i += 1;
        }
    }
    b.len()
}

/// A raw string's end, where `i` is just after its `r`/`br`/`cr` prefix: `None` when what follows
/// is not `#`* then `"` (a raw identifier, say).
fn raw_string_end(b: &[u8], i: usize) -> Option<usize> {
    let mut j = i;
    while b.get(j) == Some(&b'#') {
        j += 1;
    }
    let hashes = j - i;
    if b.get(j) != Some(&b'"') {
        return None;
    }
    j += 1;
    while j < b.len() {
        let closes = b.get(j + 1..j + 1 + hashes).is_some_and(|h| h.iter().all(|c| *c == b'#'));
        if b[j] == b'"' && closes {
            return Some(j + 1 + hashes);
        }
        j += 1;
    }
    Some(b.len())
}

/// Past a char literal (`'x'`, `'\n'`, `'\u{..}'`, a multi-byte `'é'`) starting at the `'` at `i`,
/// or just past the `'` of a lifetime or label (`'a`), whose name is then lexed as an identifier.
fn char_or_lifetime_end(b: &[u8], i: usize) -> usize {
    match b.get(i + 1) {
        Some(b'\\') => quoted_end(b, i + 1, b'\''),
        Some(&c) => {
            let len = utf8_len(c);
            if b.get(i + 1 + len) == Some(&b'\'') { i + 2 + len } else { i + 1 }
        }
        None => i + 1,
    }
}

fn utf8_len(lead: u8) -> usize {
    match lead {
        0xF0..=0xFF => 4,
        0xE0..=0xEF => 3,
        0xC0..=0xDF => 2,
        _ => 1,
    }
}
