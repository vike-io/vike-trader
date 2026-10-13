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

/// The index of the closing quote of the string literal whose opening quote sits at `open`.
///
/// Escapes are honoured (`\"`); a raw string is not a case this tree's printers contain.
fn string_end(bytes: &[u8], open: usize) -> usize {
    let mut i = open + 1;
    while i < bytes.len() && bytes[i] != b'"' {
        if bytes[i] == b'\\' {
            i += 1;
        }
        i += 1;
    }
    i
}

/// The index of the delimiter that closes the `(` or `{` at `open`, with string and char literals
/// skipped — a `{bin}` inside a `format!` string is not a brace of the function around it.
fn matching(code: &str, open: usize) -> Option<usize> {
    let bytes = code.as_bytes();
    let (opener, closer) = match bytes.get(open)? {
        b'(' => (b'(', b')'),
        b'{' => (b'{', b'}'),
        _ => return None,
    };
    let mut depth = 0usize;
    let mut i = open;
    while i < bytes.len() {
        match bytes[i] {
            b'"' => i = string_end(bytes, i),
            // A char literal (`'x'`, `'"'`, `'\n'`); a lifetime has no closing quote that near.
            b'\'' if bytes.get(i + 2) == Some(&b'\'') => i += 2,
            b'\'' if bytes.get(i + 1) == Some(&b'\\') && bytes.get(i + 3) == Some(&b'\'') => {
                i += 3;
            }
            b if b == opener => depth += 1,
            b if b == closer => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

/// The index of the `;` that ends the statement starting at `from` (brackets and strings skipped).
fn statement_end(code: &str, from: usize) -> usize {
    let bytes = code.as_bytes();
    let mut i = from;
    while i < bytes.len() {
        match bytes[i] {
            b';' => return i,
            b'(' | b'{' => match matching(code, i) {
                Some(close) => i = close,
                None => return code.len(),
            },
            b'"' => i = string_end(bytes, i),
            _ => {}
        }
        i += 1;
    }
    code.len()
}

/// Every `Program::new(` call's offset and ARGUMENT text (the outer parentheses excluded).
///
/// `vike_util::cli::Program` is the ONE reader of a program's `-V`/`--version`: its second argument
/// is the whole line `CliExit::emit` prints, verbatim (decision 0155). `MyProgram::new(` is another
/// type and is skipped.
fn program_new_calls(code: &str) -> Vec<(usize, String)> {
    const CALL: &str = "Program::new(";
    let mut out = Vec::new();
    for (at, _) in code.match_indices(CALL) {
        if at > 0 && ident_byte(code.as_bytes()[at - 1]) {
            continue;
        }
        let open = at + CALL.len() - 1;
        if let Some(close) = matching(code, open) {
            out.push((at, code[open + 1..close].to_string()));
        }
    }
    out
}

/// `args` split at its top-level commas, each part trimmed, a trailing comma dropped.
fn split_top_level(args: &str) -> Vec<String> {
    let bytes = args.as_bytes();
    let mut parts = Vec::new();
    let mut depth = 0i32;
    let mut start = 0usize;
    let mut i = 0usize;
    while i < bytes.len() {
        match bytes[i] {
            b'"' => i = string_end(bytes, i),
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => depth -= 1,
            b',' if depth == 0 => {
                parts.push(args[start..i].trim().to_string());
                start = i + 1;
            }
            _ => {}
        }
        i += 1;
    }
    let last = args[start.min(args.len())..].trim();
    if !last.is_empty() {
        parts.push(last.to_string());
    }
    parts
}

/// The right-hand side of the LAST `let [mut] name … = …;` that starts before `before`, and where
/// that `let` starts. The nearest earlier binding is the one a later use sees.
fn let_rhs(code: &str, name: &str, before: usize) -> Option<(String, usize)> {
    let head = &code[..before.min(code.len())];
    let mut found = None;
    for (at, _) in head.match_indices("let ") {
        if at > 0 && ident_byte(head.as_bytes()[at - 1]) {
            continue;
        }
        let rest = code[at + 4..].trim_start();
        let rest = rest.strip_prefix("mut ").map_or(rest, str::trim_start);
        let Some(after_name) = rest.strip_prefix(name) else { continue };
        if after_name.as_bytes().first().is_some_and(|b| ident_byte(*b)) {
            continue;
        }
        // `let name;` has no initializer: the `=` found would belong to another statement.
        let Some(eq) = after_name.find('=') else { continue };
        if after_name[..eq].contains(';') {
            continue;
        }
        let rhs_start = code.len() - after_name.len() + eq + 1;
        let rhs_end = statement_end(code, rhs_start);
        found = Some((code[rhs_start..rhs_end].trim().to_string(), at));
    }
    found
}

/// The body (braces excluded) of every `fn name` in `code`, each with the offset of its closing
/// brace. A declaration with no body (a trait method) is skipped.
fn fn_bodies(code: &str, name: &str) -> Vec<(String, usize)> {
    let needle = format!("fn {name}");
    let bytes = code.as_bytes();
    let mut out = Vec::new();
    for (at, _) in code.match_indices(&needle) {
        if at > 0 && ident_byte(bytes[at - 1]) {
            continue;
        }
        let end = at + needle.len();
        if bytes.get(end).is_some_and(|b| ident_byte(*b)) {
            continue;
        }
        let mut depth = 0i32;
        let mut open = None;
        for (i, b) in bytes.iter().enumerate().skip(end) {
            match *b {
                b'(' | b'[' => depth += 1,
                b')' | b']' => depth -= 1,
                b';' if depth == 0 => break,
                b'{' if depth == 0 => {
                    open = Some(i);
                    break;
                }
                _ => {}
            }
        }
        let Some(open) = open else { continue };
        let Some(close) = matching(code, open) else { continue };
        out.push((code[open + 1..close].to_string(), close));
    }
    out
}

/// The identifier an expression starts with — `version` in `&version`, `bin_version_line` in
/// `vike_compute::binutil::bin_version_line("x")`, `version_line` in `version_line()` — or `None`
/// for a literal or a macro call.
fn leading_name(text: &str) -> Option<&str> {
    let head = text.split(['(', '.']).next()?.trim();
    let last = head.rsplit("::").next()?;
    let pathish = head.bytes().all(|b| ident_byte(b) || b == b':');
    (pathish && !last.is_empty() && !last.as_bytes()[0].is_ascii_digit()).then_some(last)
}

/// How far a version argument is followed: the argument, a `let` binding, a function, and what that
/// function itself binds or calls.
const RESOLVE_DEPTH: u8 = 4;

/// The text the `Program::new(…, <version>)` call at `at` is given as its version line, resolved
/// through `let` bindings and function bodies until it names `CARGO_PKG_VERSION` or
/// `vike_buildinfo::version_line` (or the depth runs out).
///
/// A `let` is read in `code`, the calling file. A function is read in `code` first and in the
/// crate's other files (`crate_files`) when the file has none of that name — a bin's
/// `binutil::bin_version_line` lives in a sibling module. A name that resolves to nothing (a
/// function PARAMETER) is returned as written: the declared blind spot.
fn resolve_version(
    code: &str,
    at: usize,
    expr: &str,
    crate_files: &[String],
    depth: u8,
) -> Vec<String> {
    let text = expr.trim().trim_start_matches('&').trim_start();
    if depth == 0
        || text.contains("CARGO_PKG_VERSION")
        || text.contains("vike_buildinfo::version_line")
    {
        return vec![text.to_string()];
    }
    let Some(name) = leading_name(text) else { return vec![text.to_string()] };
    if let Some((rhs, rhs_at)) = let_rhs(code, name, at) {
        return resolve_version(code, rhs_at, &rhs, crate_files, depth - 1);
    }
    let local = fn_bodies(code, name);
    if !local.is_empty() {
        return local
            .into_iter()
            .flat_map(|(body, end)| resolve_version(code, end, &body, crate_files, depth - 1))
            .collect();
    }
    let elsewhere: Vec<String> =
        crate_files.iter().flat_map(|file| fn_bodies(file, name)).map(|(body, _)| body).collect();
    if elsewhere.is_empty() { vec![text.to_string()] } else { elsewhere }
}

/// The resolved version-line text of every `Program::new(<bin>, <version>)` call in `code`.
///
/// `crate_files` is every (comment-stripped) file of the same crate, `code` included.
pub(super) fn program_version_texts(code: &str, crate_files: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    for (at, args) in program_new_calls(code) {
        let parts = split_top_level(&args);
        let Some(version) = parts.get(1) else { continue };
        out.extend(resolve_version(code, at, version, crate_files, RESOLVE_DEPTH));
    }
    out
}
