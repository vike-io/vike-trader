//! `crates/bridges/fxcm/src/shim/fcshim.cpp` as the shim's SOURCE gates read it (no CI runner compiles
//! the shim, so what it sends and reads is pinned from its text): the file, its code with comments
//! removed, one export's body, and the JSON keys a C++ string literal writes. The `fxcm_shim` binary
//! (`tests/fxcm_shim.rs`) declares it ONCE, `mod shim_source;`, and each gate reads it as
//! `crate::shim_source`.

// A reader no gate of the group calls stays compiled for the next one.
#![allow(dead_code)]

use std::collections::BTreeSet;
use std::path::PathBuf;

/// `fcshim.cpp`, verbatim.
pub fn shim_source() -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/shim/fcshim.cpp");
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("cannot read the shim source {}: {e}", path.display()))
}

/// `fcshim.cpp` with `//` comments removed (its prose names calls its code must not make). Only a `//`
/// outside a string literal starts a comment: the file has none inside one.
pub fn shim_code() -> String {
    shim_source()
        .lines()
        .map(|l| match l.find("//") {
            Some(at) if !l[..at].contains('"') => &l[..at],
            _ => l,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// `fcshim.cpp` with every `//` and `/* */` comment removed ([`strip_comments`]).
pub fn shim_code_without_comments() -> String {
    strip_comments(&shim_source())
}

/// C++ `src` with its `//` and `/* */` comments removed; string and char literals pass through whole.
pub fn strip_comments(src: &str) -> String {
    let c: Vec<char> = src.chars().collect();
    let mut out = String::with_capacity(src.len());
    let mut i = 0;
    while i < c.len() {
        match (c[i], c.get(i + 1).copied()) {
            ('/', Some('/')) => {
                while i < c.len() && c[i] != '\n' {
                    i += 1;
                }
            }
            ('/', Some('*')) => {
                i += 2;
                while i + 1 < c.len() && !(c[i] == '*' && c[i + 1] == '/') {
                    i += 1;
                }
                i += 2;
            }
            ('"', _) => {
                out.push('"');
                i += 1;
                while i < c.len() && c[i] != '"' {
                    if c[i] == '\\' && i + 1 < c.len() {
                        out.push(c[i]);
                        i += 1;
                    }
                    out.push(c[i]);
                    i += 1;
                }
                if i < c.len() {
                    out.push('"');
                    i += 1;
                }
            }
            // A CHAR literal (`'"'` in `jesc`) must not open a string.
            ('\'', _) => {
                out.push('\'');
                i += 1;
                if i < c.len() && c[i] == '\\' {
                    out.push(c[i]);
                    i += 1;
                }
                if i < c.len() {
                    out.push(c[i]);
                    i += 1;
                }
                if i < c.len() && c[i] == '\'' {
                    out.push('\'');
                    i += 1;
                }
            }
            (ch, _) => {
                out.push(ch);
                i += 1;
            }
        }
    }
    out
}

/// The text of the export `name`, from its signature to the next export, whitespace squeezed.
pub fn body_of(name: &str) -> String {
    let code = shim_code();
    let start = code
        .find(&format!("FC_EXPORT int {name}("))
        .unwrap_or_else(|| panic!("fcshim.cpp does not export {name}"));
    let rest = &code[start..];
    let end = rest[10..].find("FC_EXPORT").map_or(rest.len(), |at| at + 10);
    rest[..end].split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Pull every `\"IDENT\":` key out of a C++ string-literal slice.
pub fn harvest_keys(literal: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let needle = "\\\"";
    let mut i = 0;
    while let Some(off) = literal[i..].find(needle) {
        let key_start = i + off + needle.len();
        let Some(close) = literal[key_start..].find(needle) else { break };
        let key = &literal[key_start..key_start + close];
        let after = key_start + close + needle.len();
        // A KEY is followed by `:`; a VALUE (`\"%s\"`, `\"fill\"`) is followed by `,` or `}`.
        if literal[after..].starts_with(':')
            && !key.is_empty()
            && key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        {
            out.insert(key.to_string());
        }
        i = key_start;
    }
    out
}
