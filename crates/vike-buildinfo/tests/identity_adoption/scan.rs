//! The text scanner the gates stand on: the tree walk, the comment stripper and the matchers.

use std::path::{Path, PathBuf};

// Same spelling as `crates/vike-ops/tests/common/repo.rs`'s `workspace_root` (keeps the `..`); the
// `parent()` twins, e.g. `crates/vike-catalog/tests/baseline_artifact.rs`'s `repo_root`, do not.
/// Workspace root, resolved from `CARGO_MANIFEST_DIR` (never CWD) — the same idiom
/// `crates/vike-ops/tests/architecture/layer_gate.rs` and `crates/vike-ops/tests/docs/citation_gate.rs` use.
pub(super) fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
}

/// Every `.rs` file under `crates/`, minus the ctrader protogen recipe the root manifest EXCLUDES
/// from the workspace (`crates/bridges/ctrader/protogen`) and any `vendor` directory — the
/// committed `ibapi` copy that also bore that name is gone.
pub(super) fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if path.is_dir() {
            if name == "vendor" || name == "protogen" || name == "target" {
                continue;
            }
            rust_files(&path, out);
        } else if name.ends_with(".rs") {
            out.push(path);
        }
    }
}

// Argument order is `(path, root)`: `crates/vike-ops/tests/venues/new_venue_gate/derivation.rs`'s
// `rel` has the same body with the arguments SWAPPED, `(root, path)`.
/// Repo-relative, forward-slashed — so a row in [`WITHOUT_IDENTITY`] reads the same on Windows and
/// on the Linux runners.
pub(super) fn rel(path: &Path, root: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
        .trim_start_matches("./")
        .to_string()
}

// Twin of `crates/vike-boot/tests/common/mod.rs`'s; unlike `crates/vike-ops/tests/common/strip.rs`'s
// `strip_line_comments_keeping_urls`, it also cuts at the `//` of a `https://`.
/// Drop `//` line comments so a paragraph ABOUT a `--version` printer is not mistaken for one.
/// Crude on purpose: a `//` inside a string literal is not a case this tree contains, and an
/// approximate matcher with a reasoned exemption table beats a precise one nobody maintains.
pub(super) fn strip_comments(text: &str) -> String {
    text.lines()
        .map(|l| match l.find("//") {
            Some(i) => &l[..i],
            None => l,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Every `println!` invocation's ARGUMENT TEXT, captured by walking the parentheses from the macro
/// name to its match.
///
/// ⚠ It has to be the macro's arguments and NOT the line, and not a character window either.
/// `rustfmt` splits a wrapped call across four lines — the `println!` on one and the
/// `CARGO_PKG_VERSION` three lines down — so a line-based matcher silently sees NOTHING the moment
/// a call grows past 100 columns, which is exactly what happened to the first cut of this gate: it
/// found 3 of 6 printers and the two that mattered were among the missing. A character window would
/// have found them and would also pair a `println!` with an unrelated `CARGO_PKG_VERSION` further
/// down the file — `crates/vike-cli/src/cmd/mcp.rs`'s `SERVER_VERSION` sits in a file whose stdout
/// is a JSON-RPC protocol, i.e. full of `println!`.
///
/// The paren walk is approximate in one declared way: an unbalanced `(` or `)` inside a string
/// literal would mis-terminate the capture. No `--version` arm in this tree has one, and the floor
/// test (`the_gate_actually_sees_the_tree`, in the root file) is what notices if the walk ever stops
/// finding anything.
pub(super) fn println_args(code: &str) -> Vec<String> {
    const MACRO: &str = "println!";
    let mut out = Vec::new();
    let mut cursor = 0usize;
    while let Some(hit) = code[cursor..].find(MACRO) {
        let after = cursor + hit + MACRO.len();
        cursor = after;
        let Some(open) = code[after..].find('(').map(|i| after + i) else { break };
        // Only whitespace may sit between the macro name and its delimiter; anything else means
        // this was not a call.
        //
        // ⚠ Nothing here looks at what PRECEDES the match, so `eprintln!` — whose name ends in
        // `println!` and whose `(` follows just as immediately — IS captured. Measured, not
        // assumed. That over-match is the safe direction and is left deliberately: a version number
        // written to stderr is still an answer somebody reads off a box, so it gets demanded to
        // carry identity or to become a declared row, never silently exempted.
        if !code[after..open].trim().is_empty() {
            continue;
        }
        let mut depth = 0usize;
        for (i, c) in code[open..].char_indices() {
            match c {
                '(' => depth += 1,
                ')' => {
                    depth -= 1;
                    if depth == 0 {
                        out.push(code[open..open + i + 1].to_string());
                        cursor = open + i + 1;
                        break;
                    }
                }
                _ => {}
            }
        }
    }
    out
}

/// The `const`/`static` names this text binds to `env!("CARGO_PKG_VERSION")` — the ONE indirection
/// this gate resolves, so a `--version` arm that prints an alias is not invisible.
///
/// Each declaration is read as `const NAME` … up to the terminating `;`, so a `rustfmt`-wrapped
/// binding is found for the same reason [`println_args`] walks parentheses instead of lines.
/// `crates/vike-cli/src/cmd/mcp.rs`'s `SERVER_VERSION` is the shape in the tree today.
pub(super) fn version_aliases(code: &str) -> Vec<String> {
    let mut out = Vec::new();
    for keyword in ["const ", "static "] {
        let mut cursor = 0usize;
        while let Some(hit) = code[cursor..].find(keyword) {
            let after = cursor + hit + keyword.len();
            cursor = after;
            let Some(end) = code[after..].find(';').map(|i| after + i) else { break };
            let declaration = &code[after..end];
            let Some((name, _)) = declaration.split_once(':') else { continue };
            let name = name.trim();
            // A real identifier, and one bound to the version: `const fn`, a `static mut` and a
            // `const` holding anything else all fall out here.
            if declaration.contains("CARGO_PKG_VERSION")
                && !name.is_empty()
                && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
            {
                out.push(name.to_string());
            }
        }
    }
    out.sort();
    out.dedup();
    out
}

/// Whether one `println!`'s arguments name the package version — literally, or through a crate's
/// own [`version_aliases`].
pub(super) fn names_version(args: &str, aliases: &[String]) -> bool {
    contains_ident(args, "CARGO_PKG_VERSION")
        || aliases.iter().any(|alias| contains_ident(args, alias))
}

// ASCII bytes, like `crates/vike-ops/tests/common/rust_text.rs`'s `find_word`; NOT the Unicode
// `crates/vike-backtest/tests/run_record_completeness.rs`'s `contains_ident` of the same name.
/// `needle` appears in `haystack` as a WHOLE identifier, not as a substring of a longer one.
///
/// ⚠ A plain `contains` is wrong HERE and only here: an alias name comes from the tree, not from
/// this file, and nothing stops a crate from binding the version to `V`. `args.contains("V")` then
/// matches essentially every `println!` in that crate, and a gate that reports forty printers where
/// there is one gets switched off rather than read. The literal `CARGO_PKG_VERSION` goes through
/// the same check for consistency; its own delimiters are quotes, which are not identifier bytes.
fn contains_ident(haystack: &str, needle: &str) -> bool {
    let bytes = haystack.as_bytes();
    haystack.match_indices(needle).any(|(start, _)| {
        let end = start + needle.len();
        (start == 0 || !ident_byte(bytes[start - 1]))
            && (end >= bytes.len() || !ident_byte(bytes[end]))
    })
}

/// A byte that can be part of a Rust identifier: the one definition [`contains_ident`] and
/// `version_line_names` both draw their word boundaries from.
pub(super) fn ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}
