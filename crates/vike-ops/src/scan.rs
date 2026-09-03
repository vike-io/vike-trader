//! `scan` — the pure source scanner behind the settings-registry gate.
//!
//! String in, data out: no filesystem, no globals, so every edge case (nested parens, `)`
//! inside a literal, `env::var` mentioned in a `//!` doc block) is unit-testable directly.
//! The filesystem walk lives in `tests/settings_registry.rs`.
//!
//! Deliberately hand-rolled rather than `regex`/`syn`: this workspace links every crate into
//! one order-signing binary and audits the whole tree with `cargo deny`, so a scanner used
//! only by a test does not justify a new dependency.
//!
//! # Three read shapes, one comment stripper
//!
//! The registry's rule is "libraries take configuration as PARAMETERS; only binaries read global
//! configuration state", and a library can break it in three ways that look nothing alike in
//! source:
//!
//! - [`find_env_reads`] / [`find_map_lookups`] — the PROCESS environment, keyed on an `env::var`
//!   call or an env-shaped map key. ⚠ The call half keyed the PATH-QUALIFIED spelling only, so an
//!   imported bare `var("VIKE_X")` matched nothing at all; it now also searches whatever bare or
//!   aliased name the file's own `use std::env…` line brings into scope, and that function's doc
//!   lists what the widening still cannot reach.
//! - [`find_calls`] — the credential STORE (`<project>/settings/secrets.env`), which
//!   is a plain `std::fs` read with no `env::var` anywhere near it and therefore structurally
//!   invisible to the two above. It is keyed on the NAME of the function that performs the read.
//! - [`find_path_reads`] — the SAME store, read WITHOUT calling any of those names: a
//!   `std::fs::read_to_string(".env")` written by hand. Keyed on neither a variable name nor a
//!   reader name (there is no name at all), but on the SHAPE — a filesystem opener whose path
//!   argument names the store. See its doc for what that costs and what it cannot see.
//!
//! All three run over the same `strip_comments` pass, so a `//!` doc block naming any of
//! them is not a call site in any scanner — the single most common false positive, and the reason
//! there is one stripper rather than three.

use std::collections::BTreeMap;

/// One `env::var` / `env::var_os` call site.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvRead {
    /// The RAW argument text, verbatim, parens balanced. Resolution is Task 3's job.
    pub arg: String,
    /// 1-indexed line of the call site.
    pub line: usize,
}

/// A byte that can appear inside a Rust identifier — used for the word-boundary checks below
/// (an `r` preceded by one of these is part of an identifier like `for`/`var`, not a raw-string
/// opener; likewise for the `env` in a pattern match).
fn is_word_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// Strip line comments (`//`, `///`, `//!`) outside string literals (both quoted `"..."` and
/// raw `r#"..."#`), preserving line count exactly.
///
/// This is a SINGLE pass over the whole source — not per-line — because string state (are we
/// inside a `"`/raw-string literal right now?) must survive a `\n` unchanged: a legal multi-line
/// string literal's second line is still inside the string, so a `//` there is not a comment
/// start. Newlines are always copied through byte-for-byte (whether skipped-as-comment,
/// inside-a-string, or plain code), so `clean` has exactly as many `\n` bytes as `source` —
/// callers may keep counting them for 1-indexed line numbers.
///
/// `pub` because a gate that DERIVES its subject set from source (rather than listing it) needs the
/// same "is this text code" answer every scanner here already relies on:
/// `crates/vike-ops/tests/paper_mount_arming_gate.rs` reads the paper client's real constructors out
/// of its own `impl` block, and a doc comment showing one would otherwise be read as a definition.
/// A second stripper written beside it would be the duplication this module's header refuses.
pub fn strip_comments(source: &str) -> String {
    let bytes = source.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0usize;
    let mut in_str = false;
    let mut esc = false;
    let mut raw_hashes: Option<usize> = None;
    let mut in_comment = false;

    while i < bytes.len() {
        let c = bytes[i];

        if in_comment {
            if c == b'\n' {
                in_comment = false;
                out.push(b'\n');
            }
            i += 1;
            continue;
        }

        if let Some(n) = raw_hashes {
            // The length guard matters: `.take(n).all(..)` is vacuously true when FEWER than
            // `n` bytes remain (an empty/short iterator has nothing to fail the predicate on),
            // so without `bytes.len() >= i + 1 + n` a truncated closer at EOF (e.g. one `#` when
            // the opener demanded two) would be wrongly accepted as closed.
            if c == b'"'
                && bytes.len() >= i + 1 + n
                && bytes[i + 1..].iter().take(n).all(|&b| b == b'#')
            {
                out.push(b'"');
                out.extend(std::iter::repeat_n(b'#', n));
                i += 1 + n;
                raw_hashes = None;
            } else {
                out.push(c);
                i += 1;
            }
            continue;
        }

        if in_str {
            out.push(c);
            if esc {
                esc = false;
            } else if c == b'\\' {
                esc = true;
            } else if c == b'"' {
                in_str = false;
            }
            i += 1;
            continue;
        }

        // Not inside any string/comment right now: does a raw string open here? `r`, then N
        // `#`s (N may be 0), then `"` — guarded by a word boundary so the `r` in `for`/`var`
        // cannot start one.
        if c == b'r' && (i == 0 || !is_word_byte(bytes[i - 1])) {
            let mut j = i + 1;
            while j < bytes.len() && bytes[j] == b'#' {
                j += 1;
            }
            if j < bytes.len() && bytes[j] == b'"' {
                let hashes = j - (i + 1);
                out.push(b'r');
                out.extend(std::iter::repeat_n(b'#', hashes));
                out.push(b'"');
                raw_hashes = Some(hashes);
                i = j + 1;
                continue;
            }
        }

        // A char literal containing exactly a double quote — `'"'`, or its byte-literal twin
        // `b'"'` (the `b` is just an ordinary byte pushed through above, so the check only needs
        // to anchor on the `'`) — would otherwise have its middle byte mistaken for a string
        // opener below, inverting string-vs-code parity for the rest of the file (real
        // instance: the `.trim_matches('"')` found in `crates/vike-bridge-core/src/credentials.rs`
        // and since moved to `crates/vike-secrets/src/dotenv.rs`'s `parse_dotenv`, which silently
        // swallowed every literal after it). A lifetime (`'static`, `'a`) is `'`
        // followed by an IDENTIFIER, never by `"`, so this cannot mis-fire on one.
        if c == b'\'' && i + 2 < bytes.len() && bytes[i + 1] == b'"' && bytes[i + 2] == b'\'' {
            out.push(b'\'');
            out.push(b'"');
            out.push(b'\'');
            i += 3;
            continue;
        }

        if c == b'"' {
            in_str = true;
            out.push(b'"');
            i += 1;
            continue;
        }

        if c == b'/' && i + 1 < bytes.len() && bytes[i + 1] == b'/' {
            in_comment = true;
            i += 1;
            continue;
        }

        out.push(c);
        i += 1;
    }

    // Every byte we ever drop is an ASCII comment byte between a `//` and the following `\n`
    // (never part of a multi-byte UTF-8 sequence), and every other byte is copied through
    // unchanged — so this can never observe invalid UTF-8.
    String::from_utf8(out).expect("strip_comments only ever removes ASCII comment bytes")
}

/// Take the balanced-paren argument starting just after an opening `(`, aware of both quoted
/// and raw string literals so a `)` (or `"`) inside either is not mistaken for real syntax.
/// Returns the argument text, or `None` if the parens never balance.
fn take_balanced(rest: &str) -> Option<String> {
    let bytes = rest.as_bytes();
    let mut depth = 1i32;
    let mut i = 0usize;
    let mut in_str = false;
    let mut esc = false;
    let mut raw_hashes: Option<usize> = None;

    while i < bytes.len() {
        let c = bytes[i];

        if let Some(n) = raw_hashes {
            // Same length guard as `strip_comments` — see its comment for why the plain
            // `.take(n).all(..)` check is vacuously true (and wrong) near EOF.
            if c == b'"'
                && bytes.len() >= i + 1 + n
                && bytes[i + 1..].iter().take(n).all(|&b| b == b'#')
            {
                i += 1 + n;
                raw_hashes = None;
            } else {
                i += 1;
            }
            continue;
        }

        if in_str {
            if esc {
                esc = false;
            } else if c == b'\\' {
                esc = true;
            } else if c == b'"' {
                in_str = false;
            }
            i += 1;
            continue;
        }

        if c == b'r' && (i == 0 || !is_word_byte(bytes[i - 1])) {
            let mut j = i + 1;
            while j < bytes.len() && bytes[j] == b'#' {
                j += 1;
            }
            if j < bytes.len() && bytes[j] == b'"' {
                raw_hashes = Some(j - (i + 1));
                i = j + 1;
                continue;
            }
        }

        match c {
            b'"' => in_str = true,
            b'(' => depth += 1,
            b')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(rest[..i].trim().to_string());
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

/// The PATH-QUALIFIED spellings of a process-environment read. `env::var(` matches
/// `std::env::var(`, a bare `env::var(` after `use std::env`, and any other path ending in that
/// module, because the boundary check looks only at the byte before `env`.
const QUALIFIED_ENV_READS: [&str; 2] = ["env::var(", "env::var_os("];

/// The last non-whitespace byte before `at`, if any — the method-position probe below.
fn prev_significant(bytes: &[u8], at: usize) -> Option<u8> {
    bytes[..at].iter().rev().find(|b| !b.is_ascii_whitespace()).copied()
}

/// The BARE or ALIASED spellings of `std::env::var` / `var_os` that a file's own `use` declarations
/// bring into scope, as complete `NAME(` search patterns, plus the module-alias form
/// (`use std::env as e;` -> `e::var(`).
///
/// ⚠ **Why an import is required before a bare name is searched for at all.** `var` is an ordinary
/// identifier and this workspace binds it as one: `crates/bridges/vike-ibkr/src/config.rs`'s
/// `load_ibkr_config_for_account` opens with `let var = |suffix: &str| var(vars, env, label,
/// suffix);` and then calls `var("ACCOUNT")`, `var("BACKEND")`, `var("PORT")` — eight sites, none
/// of them the process environment, all of them reading a caller-supplied MAP, which is the shape
/// this registry holds up as CORRECT. Searching for a bare `var(` unconditionally reports every one
/// of them, and the inner `var(vars, env, label, suffix)` resolves to no literal, so
/// `crates/vike-ops/tests/settings_registry.rs`'s `dynamic_sites_are_allowlisted` would demand a
/// `DYNAMIC_ALLOWLIST` row for a file that reads no environment at all. An import is evidence the
/// scanner can check, it costs no false positive, and it closes the alias case for free.
///
/// Measured 2026-08-28 across `crates/` and `xtask/`: NO file imports either function or aliases
/// the module, so this widening finds nothing today and moves no row. It is here because
/// `use std::env::var;` is one line, entirely ordinary, and would have made every read in that file
/// invisible to a gate whose whole subject is reads that hide.
fn imported_env_read_patterns(clean: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in clean.lines() {
        let Some(rest) = line.trim().strip_prefix("use std::env") else { continue };
        let rest = rest.trim();
        if let Some(alias) = rest.strip_prefix("as ").and_then(|r| r.strip_suffix(';')) {
            let alias = alias.trim();
            out.push(format!("{alias}::var("));
            out.push(format!("{alias}::var_os("));
            continue;
        }
        let Some(items) = rest.strip_prefix("::") else { continue };
        let items = items.trim_end_matches(';').trim();
        for item in items.trim_start_matches('{').trim_end_matches('}').split(',') {
            let item = item.trim();
            let (name, alias) = match item.split_once(" as ") {
                Some((n, a)) => (n.trim(), a.trim()),
                None => (item, item),
            };
            if name == "var" || name == "var_os" {
                out.push(format!("{alias}("));
            }
        }
    }
    out.sort();
    out.dedup();
    out
}

/// Every LOCAL spelling a file's own `use` declarations bind `item` to — the general form of
/// [`imported_env_read_patterns`], for a gate keyed on a TYPE rather than on `std::env`.
///
/// ⚠ **The class this exists for has now bitten twice.** `find_env_reads` keyed the path-qualified
/// spelling only, so an imported bare `var("VIKE_X")` matched nothing; the widening above is that
/// repair, and it was written for `std::env` alone. `crates/vike-ops/tests/paper_mount_arming_gate.rs`
/// then keyed `PaperExecutionClient::new` as a literal, and
/// `use vike_paper::PaperExecutionClient as Paper; Paper::new(…)` is the identical hole one type
/// over: a mount seam constructing an unarmed paper book, invisible to the gate whose whole subject
/// is construction sites nobody is looking at. One resolver, two callers.
///
/// Three shapes are resolved, and each is a spelling this workspace actually writes:
///   * `use krate::Item;` / `use krate::{Item, Other};` → `Item`;
///   * `use krate::Item as Alias;` / `use krate::{Item as Alias};` → `Alias`;
///   * `use krate as k;` → `k::Item`, because a MODULE alias renames the path rather than the name.
///
/// The third is emitted for EVERY module alias, so `use std::io::Result as R;` yields the candidate
/// `R::Item` too. That is a candidate rather than a claim — a prefix naming something else simply
/// never matches at a call site — and refusing to guess which aliases are modules is what keeps this
/// free of a heuristic nobody could test.
///
/// ⚠ Two shapes are NOT resolved, declared rather than implied: a GLOB (`use krate::*;`, which
/// brings the name in without spelling it — the same refusal [`imported_env_read_patterns`] makes,
/// and for the same reason: an unconditional bare search trades one blind spot for a pile of false
/// demands), and a NESTED group (`use krate::{a::{B}};`), whose inner braces this splitter reads as
/// one element. Neither occurs for the types this is used with today.
pub fn imported_spellings(source: &str, item: &str) -> Vec<String> {
    let clean = strip_comments(source);
    let bytes = clean.as_bytes();
    let mut out = Vec::new();
    let mut from = 0usize;
    while let Some(rel) = clean[from..].find("use ") {
        let at = from + rel;
        from = at + 4;
        // A word byte in front makes this `misuse `/`reuse `, not a declaration.
        if at > 0 && is_word_byte(bytes[at - 1]) {
            continue;
        }
        let Some(end) = clean[at..].find(';') else { break };
        // Collapsed to one line so a rustfmt-wrapped group reads the same as an inline one.
        let stmt = clean[at + 4..at + end].split_whitespace().collect::<Vec<_>>().join(" ");
        collect_import_spellings(&stmt, item, &mut out);
    }
    out.sort();
    out.dedup();
    out
}

/// One `use` statement's payload (no `use`, no `;`, whitespace collapsed) → the spellings it binds.
fn collect_import_spellings(stmt: &str, item: &str, out: &mut Vec<String>) {
    if let Some(open) = stmt.find('{') {
        let inner = stmt[open + 1..].trim_end().trim_end_matches('}');
        for element in inner.split(',') {
            let element = element.trim();
            if !element.is_empty() {
                push_import_spelling(element, item, out);
            }
        }
        return;
    }
    push_import_spelling(stmt, item, out);
}

/// One element of a `use` list → the spelling it binds, if any.
fn push_import_spelling(element: &str, item: &str, out: &mut Vec<String>) {
    let last = |p: &str| p.trim().rsplit("::").next().unwrap_or("").trim().to_string();
    match element.split_once(" as ") {
        Some((path, alias)) => {
            let alias = alias.trim();
            if alias.is_empty() || alias == "_" {
                return;
            }
            if last(path) == item {
                out.push(alias.to_string());
            } else {
                out.push(format!("{alias}::{item}"));
            }
        }
        None => {
            if last(element) == item {
                out.push(item.to_string());
            }
        }
    }
}

/// Every `env::var(..)` / `env::var_os(..)` call site in `source`, comments excluded. A match is
/// only accepted at a word boundary — the byte immediately before `env` must be absent (start of
/// file) or not `[A-Za-z0-9_]` — so an unrelated `my_env::var(..)` is not mistaken for the real
/// call. A bare or aliased spelling counts too, but only where the file's own `use` declarations
/// bring it into scope: see [`imported_env_read_patterns`] for what that costs and why the
/// unconditional version was rejected.
///
/// # What the widening still cannot resolve
///
/// Declared rather than implied, and `crates/vike-ops/tests/settings_registry.rs`'s
/// `the_shapes_the_store_scanner_cannot_see` is the precedent for writing a blind spot down as
/// something executable rather than as a sentence that rots:
///
///   * **A glob import.** `use std::env::*;` then a bare `var("VIKE_X")` brings the name in without
///     naming it, so there is nothing to harvest. Deliberately NOT handled by falling back to an
///     unconditional bare search — that trades one blind spot for eight false demands in
///     `crates/bridges/vike-ibkr/src/config.rs` alone. Nothing in this workspace glob-imports
///     `std::env`.
///   * **A re-export.** `some_crate::var("VIKE_X")`, where the wrapper crate re-exports the std
///     function, reaches the same read through a path this file has no `use std::env` line to key
///     on. [`find_calls`] is the tool for that shape — it keys on a READER'S NAME — and it is how
///     the credential store's wrapper family is already covered.
///   * **The function as a VALUE.** `let read = std::env::var; read("VIKE_X")` names the function
///     without calling it and calls it through a local. Closing that needs dataflow, which needs a
///     parser, which this scanner deliberately is not.
///   * **A COMPUTED name**, which is unchanged by any of this: the argument, not the callee, is
///     what `resolve_arg` cannot resolve, and `DYNAMIC_ALLOWLIST` is where those live.
pub fn find_env_reads(source: &str) -> Vec<EnvRead> {
    let clean = strip_comments(source);
    let bytes = clean.as_bytes();
    let mut out: Vec<(usize, EnvRead)> = Vec::new();
    let imported = imported_env_read_patterns(&clean);
    let patterns = QUALIFIED_ENV_READS.iter().map(|p| (*p).to_string()).chain(imported);
    for pat in patterns {
        let mut from = 0usize;
        while let Some(hit) = clean[from..].find(&pat) {
            let at = from + hit;
            let open = at + pat.len();
            // `starts_identifier` alone is right for the qualified spellings — a `::` path cannot
            // be a definition or a method call. The bare ones need both extra checks: `fn var(` in
            // a file that also imports the std one is a DEFINITION, and `self.var(` is somebody's
            // method however the file spells its imports.
            let accepted = starts_identifier(bytes, at)
                && !preceded_by_fn_keyword(bytes, at)
                && prev_significant(bytes, at) != Some(b'.');
            if accepted {
                if let Some(arg) = take_balanced(&clean[open..]) {
                    let line = clean[..at].bytes().filter(|b| *b == b'\n').count() + 1;
                    out.push((open, EnvRead { arg, line }));
                }
            }
            from = open;
        }
    }
    // ONE call site, ONE row. The patterns genuinely overlap once a file imports the bare name:
    // `var(` also matches the TAIL of `env::var(`, and `starts_identifier` accepts it there because
    // the byte before it is a `:`. The key is the ARGUMENT-LIST offset rather than the name's,
    // because that is what the two spellings share — the name offsets differ by the qualifier's
    // length, so keying on them deduplicates nothing. Keying on `(line, arg)` instead would have
    // been wrong in the other direction: two REAL reads of the same variable on one line are two
    // reads.
    out.sort_by_key(|(open, _)| *open);
    out.dedup_by_key(|(open, _)| *open);
    let mut out: Vec<EnvRead> = out.into_iter().map(|(_, r)| r).collect();
    out.sort_by_key(|r| r.line);
    out
}

/// One call site of a named free function — the output of [`find_calls`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Call {
    /// Which of the searched names this site calls.
    pub name: String,
    /// 1-indexed line of the call site.
    pub line: usize,
}

/// Is the identifier starting at `at` a fresh one, or the tail of a longer identifier?
///
/// `credentials::load_workspace_dotenv(` matches (the preceding byte is `:`), while
/// `my_load_workspace_dotenv(` does not — exactly the boundary rule [`find_env_reads`] applies to
/// `env::var`.
fn starts_identifier(bytes: &[u8], at: usize) -> bool {
    at == 0 || !is_word_byte(bytes[at - 1])
}

/// Is the name at `at` preceded by the `fn` keyword — i.e. is this a DEFINITION, not a call?
///
/// Load-bearing, and not a nicety: `pub fn load_workspace_dotenv() -> HashMap<..>` in
/// `crates/vike-secrets/src/dotenv.rs` is spelled `NAME(` exactly like every call site, so without
/// this check the function that DEFINES a credential-store read would be reported as a library that
/// PERFORMS one. Checked as a whole token (the bytes before `fn` must not be word bytes) so an
/// identifier merely ending in `fn` cannot mask a real call.
fn preceded_by_fn_keyword(bytes: &[u8], at: usize) -> bool {
    let mut i = at;
    while i > 0 && (bytes[i - 1] == b' ' || bytes[i - 1] == b'\t') {
        i -= 1;
    }
    i >= 2 && &bytes[i - 2..i] == b"fn" && (i == 2 || !is_word_byte(bytes[i - 3]))
}

/// Every call site of any function in `names`, comments excluded and DEFINITIONS excluded.
///
/// The credential-store scanner. `find_env_reads` keys on a call to `env::var`, which is the wrong
/// question for the credential store: `load_workspace_dotenv()` is a `std::fs::read_to_string` of a
/// path a runtime walk produces, with no `env::var` anywhere in it, so a library reading global
/// credential state its caller can neither see nor override is invisible to that scanner. This one
/// keys on the READER'S NAME instead.
///
/// A match needs the name at an identifier boundary immediately followed by `(`. Three shapes fall
/// out of that, all wanted:
///   - a `use` / `pub use` re-export lists the bare name with no `(` — never a call site;
///   - a doc comment or `//` line naming the function is stripped before the search;
///   - `credentials::load_workspace_dotenv()` matches through any path qualifier, because the
///     preceding `:` is not an identifier byte.
///
/// `names` is a PARAMETER rather than a `const` table inside this module for the same reason the
/// unit tests below use invented names: this file is itself walked by the gate that calls this
/// function, and a real entry-point name spelled here as test DATA would be reported as a call site
/// in `vike-ops` — the self-scanning trap `LITERAL_HARVEST_EXCLUDED` exists to paper over for the
/// env scanner. Keeping the names at the caller avoids needing the exclusion at all.
pub fn find_calls(source: &str, names: &[&str]) -> Vec<Call> {
    let clean = strip_comments(source);
    let bytes = clean.as_bytes();
    let mut out = Vec::new();
    for name in names {
        let pat = format!("{name}(");
        let mut from = 0usize;
        while let Some(hit) = clean[from..].find(&pat) {
            let at = from + hit;
            if starts_identifier(bytes, at) && !preceded_by_fn_keyword(bytes, at) {
                let line = clean[..at].bytes().filter(|b| *b == b'\n').count() + 1;
                out.push(Call { name: (*name).to_string(), line });
            }
            from = at + pat.len();
        }
    }
    out.sort_by_key(|c| c.line);
    out
}

/// Does `source` DEFINE `name` as a function (`fn NAME(`), comments excluded?
///
/// Two jobs, both about keeping the name-keyed scanner above honest:
///   - the file that defines a credential-store reader is that reader's IMPLEMENTATION, so its own
///     internal calls to the family are not consumer reads;
///   - a gate keyed on hardcoded names goes silently green if the function is RENAMED. Asserting
///     each keyed name is still defined somewhere in the tree is what turns that from an invisible
///     decay into a red test.
pub fn defines_fn(source: &str, name: &str) -> bool {
    let clean = strip_comments(source);
    let bytes = clean.as_bytes();
    let pat = format!("{name}(");
    let mut from = 0usize;
    while let Some(hit) = clean[from..].find(&pat) {
        let at = from + hit;
        if starts_identifier(bytes, at) && preceded_by_fn_keyword(bytes, at) {
            return true;
        }
        from = at + pat.len();
    }
    false
}

/// One call to a filesystem opener, together with the path expression it was handed — the output
/// of [`find_path_reads`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathRead {
    /// Which of the searched opener names this site calls.
    pub func: String,
    /// The RAW argument text, verbatim, parens balanced. Deciding whether it names anything in
    /// particular is the CALLER's job — see [`find_path_reads`] for why.
    pub arg: String,
    /// 1-indexed line of the call site.
    pub line: usize,
}

/// Every call to one of `names` (a filesystem opener) together with its balanced ARGUMENT text.
///
/// The third read shape, and the one neither sibling scanner can see. [`find_calls`] keys on the
/// NAME of a credential-store reader, so it goes quiet the moment somebody writes the read out by
/// hand; [`find_env_reads`] keys on `env::var`, which such a read never touches. That is not a
/// hypothetical gap — #1122's `env_key`, then living in `crates/vike-app-core/src/tools.rs`, did
/// `std::fs::read_to_string` of a CWD-relative `.env`, a second credential store outside
/// `<project>/settings/`, and was invisible to BOTH gates until a human read the file. (It is
/// DELETED, which is why it is cited by PR and not in the `` `path`'s `SYMBOL` `` form: there is no
/// symbol left to find, and the citation gate is right to insist on the difference.)
///
/// # The precision lives in the ARGUMENT, not in the name
///
/// `read`/`open`/`read_to_string` are among the most common identifiers in any Rust tree, and this
/// function deliberately matches all of them without discrimination: `resp.body_mut()`'s
/// `read_to_string`, an egui window's `.open(&mut shown)`, `io::Read::read(&mut buf)` all come back
/// as hits. That is safe because a hit is not a verdict — the caller decides, from the ARG text,
/// whether this particular open names the thing it cares about. Anchoring on the name alone would
/// be useless; anchoring on a fixed path list would fossilize one spelling of it.
///
/// `names` is a PARAMETER for the same reason [`find_calls`]' is (see its doc): this file is walked
/// by the gate that calls it, and a needle spelled here as fixture DATA immediately in front of an
/// open paren would be reported as a `vike-ops` hit.
///
/// # What it cannot see, by construction
///
/// The argument is TEXT. A path bound to a local first (`let p = ".env"; read_to_string(&p)`), or
/// assembled from configuration, or produced by a helper, yields an argument that names nothing —
/// no hit. Closing that needs dataflow, which needs a parser; the gate that calls this measures the
/// residual instead of pretending it away.
pub fn find_path_reads(source: &str, names: &[&str]) -> Vec<PathRead> {
    let clean = strip_comments(source);
    let bytes = clean.as_bytes();
    let mut out = Vec::new();
    for name in names {
        let pat = format!("{name}(");
        let mut from = 0usize;
        while let Some(hit) = clean[from..].find(&pat) {
            let at = from + hit;
            let open = at + pat.len();
            if starts_identifier(bytes, at) && !preceded_by_fn_keyword(bytes, at) {
                // Unbalanced parens mean the argument cannot be read out at all, so there is
                // nothing for the caller to judge — dropping the site is the honest answer, and
                // matches `find_env_reads`, which does the same.
                if let Some(arg) = take_balanced(&clean[open..]) {
                    let line = clean[..at].bytes().filter(|b| *b == b'\n').count() + 1;
                    out.push(PathRead { func: (*name).to_string(), arg, line });
                }
            }
            from = open;
        }
    }
    out.sort_by_key(|r| r.line);
    out
}

/// Every string literal in `source`, contents only, comments excluded.
///
/// The quote-parity sweep [`find_map_lookups`] used to carry inline, extracted so [`find_path_reads`]'
/// callers can pull the literals out of ONE call argument without growing a second tracker with its
/// own escaping bugs. Carries the same `'"'` / `b'"'` char-literal guard `strip_comments` does:
/// without it, one such literal inverts string-vs-code parity for everything after it.
///
/// Raw strings are not decoded specially — `r"x"` yields `x`, and an escape sequence is returned as
/// written. Every caller compares against plain ASCII names, where neither matters.
pub fn string_literals(source: &str) -> Vec<String> {
    let clean = strip_comments(source);
    let bytes = clean.as_bytes();
    let mut out = Vec::new();
    let (mut i, mut start, mut in_str, mut esc) = (0usize, 0usize, false, false);
    while i < bytes.len() {
        let c = bytes[i];
        if in_str {
            if esc {
                esc = false;
            } else if c == b'\\' {
                esc = true;
            } else if c == b'"' {
                out.push(clean[start..i].to_string());
                in_str = false;
            }
        } else if c == b'\'' && i + 2 < bytes.len() && bytes[i + 1] == b'"' && bytes[i + 2] == b'\''
        {
            // `'"'` / `b'"'`: skip all three bytes so the middle one cannot open a string.
            i += 3;
            continue;
        } else if c == b'"' {
            in_str = true;
            start = i + 1;
        }
        i += 1;
    }
    out
}

/// Does `text` mention `ident` with a non-identifier byte on BOTH sides?
///
/// Substring containment is not enough for the credential-store scanner's second needle: a
/// `SECRETS_FILE` needle would otherwise match `SECRETS_FILENAME`, and a `workspace_dotenv_path`
/// one would match `workspace_dotenv_path_from` — which is the opposite of what a caller listing
/// both spellings separately intends.
pub fn mentions_ident(text: &str, ident: &str) -> bool {
    if ident.is_empty() {
        return false;
    }
    let bytes = text.as_bytes();
    let mut from = 0usize;
    while let Some(hit) = text[from..].find(ident) {
        let at = from + hit;
        let end = at + ident.len();
        let left = at == 0 || !is_word_byte(bytes[at - 1]);
        let right = end >= bytes.len() || !is_word_byte(bytes[end]);
        if left && right {
            return true;
        }
        from = end;
    }
    false
}

/// How a call-site argument resolved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolved {
    /// A concrete variable name. `konst` is `Some(ident)` when the name reached `env::var`
    /// through a `const IDENT: &str`, `None` when the call site used a literal.
    Name { name: String, konst: Option<String> },
    /// Computed or parameterised — the gate requires an explicit allowlist entry.
    Dynamic,
}

/// Every `const IDENT: &str = "VALUE";` (also `&'static str`) declared in `source`.
pub fn const_table(source: &str) -> BTreeMap<String, String> {
    let clean = strip_comments(source);
    let mut out = BTreeMap::new();
    for line in clean.lines() {
        let line = line.trim();
        let Some(rest) = line.strip_prefix("pub const ").or_else(|| line.strip_prefix("const "))
        else {
            continue;
        };
        let Some((ident, tail)) = rest.split_once(':') else { continue };
        let ident = ident.trim();
        if !ident.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_') {
            continue;
        }
        let Some((ty, value)) = tail.split_once('=') else { continue };
        let ty = ty.trim();
        if ty != "&str" && ty != "&'static str" {
            continue;
        }
        let value = value.trim().trim_end_matches(';').trim();
        if let Some(inner) = value.strip_prefix('"').and_then(|v| v.strip_suffix('"')) {
            out.insert(ident.to_string(), inner.to_string());
        }
    }
    out
}

/// Resolve one raw call-site argument against the file's constants.
///
/// Returns an OWNED `konst` identifier rather than `settings::Naming`: `Naming` is a `Copy`
/// registry type holding `&'static str`, and scanned text is not `'static`. Keeping the scan
/// output owned avoids leaking a `Box` per constant just to satisfy that lifetime; the gate
/// compares this `Option<String>` against the registry's `Naming::Konst(ident)` itself.
pub fn resolve_arg(arg: &str, consts: &BTreeMap<String, String>) -> Resolved {
    let arg = arg.trim();
    if let Some(inner) = arg.strip_prefix('"').and_then(|v| v.strip_suffix('"')) {
        if !inner.contains('"') {
            return Resolved::Name { name: inner.to_string(), konst: None };
        }
    }
    let ident = arg.rsplit("::").next().unwrap_or(arg).trim();
    if let Some(value) = consts.get(ident) {
        return Resolved::Name { name: value.clone(), konst: Some(ident.to_string()) };
    }
    Resolved::Dynamic
}

/// Prefixes that mark a SCREAMING_SNAKE literal as one of OUR environment variables.
///
/// Load-bearing: because `find_map_lookups` cannot anchor on a call syntax (keys are passed to
/// helpers), shape alone would swallow venue wire constants — `"PARTIALLY_FILLED"`,
/// `"GOOD_TIL_CANCELLED"`, `"POST_ONLY"` are all SCREAMING_SNAKE strings in the bridge crates.
/// The prefix gate is what separates a knob from a protocol token. Adding a variable under a
/// NEW prefix means adding it here — and the exhaustiveness gate failing is how you find out.
const ENV_PREFIXES: &[&str] = &[
    "VIKE_",
    "POLY_",
    "POLYMARKET_",
    "BINANCE_",
    "BYBIT_",
    "OKX_",
    "DERIBIT_",
    "ASTER_",
    "HYPERLIQUID_",
    "ALPACA_",
    "IG_",
    "OANDA_",
    "FXCM_",
    "DUKASCOPY_",
    // The Dukascopy sidecar's own namespace, distinct from `DUKASCOPY_` (which is the venue's
    // login credentials) — `JFOREX_BRIDGE_JAR` names the JForex jar. It arrived the day that read
    // stopped being a direct `env::var("JFOREX_BRIDGE_JAR")`, which `find_env_reads` resolved
    // without needing any prefix, and became a map lookup, which only this sweep can see: exactly
    // the "adding a variable under a NEW prefix means adding it here — and the exhaustiveness gate
    // failing is how you find out" case above, observed rather than imagined.
    "JFOREX_",
    "CTRADER_",
    "IBKR_",
    "IBAPI_",
    "DATABENTO_",
    "TARDIS_",
    "PMXT_",
    "EOD_",
    "GAMMA_",
    "RUST_LOG",
    "JAVA_HOME",
    "FCSDK_DIR",
];

/// Whole names, not prefixes: the PLATFORM's own directory variables.
///
/// These are the OS's names, not ours, so by construction no [`ENV_PREFIXES`] entry can ever match
/// one — which made every INJECTED read of them invisible to [`find_map_lookups`]. That was a real
/// blind spot, not a theoretical one: a resolver reading `HOME` then `USERPROFILE` out of a
/// caller-supplied map had NO registry row at all, because the only pattern that could see it was
/// the prefix sweep. Direct `env::var("HOME")` reads were always observed (`find_env_reads`
/// resolves the argument and needs no prefix), so the registry looked complete while the injected
/// half of the same variable was unobservable.
///
/// Whole-name matching rather than a `"HOME"` PREFIX entry is deliberate: a prefix would also
/// swallow `HOME_DIR`, `HOMEPATH`, `USERPROFILE_OVERRIDE` and any future look-alike, and the
/// separation between a knob and a protocol token is exactly what [`ENV_PREFIXES`] exists to keep.
const ENV_EXACT_NAMES: &[&str] = &["HOME", "USERPROFILE", "XDG_DATA_HOME", "LOCALAPPDATA"];

/// Env-var name shape AND a known prefix (or one of the [`ENV_EXACT_NAMES`]). Narrow on purpose —
/// see [`ENV_PREFIXES`].
fn is_env_name(s: &str) -> bool {
    s.len() >= 3
        && s.starts_with(|c: char| c.is_ascii_uppercase())
        && s.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
        && (ENV_PREFIXES.iter().any(|p| s.starts_with(p)) || ENV_EXACT_NAMES.contains(&s))
}

/// Injected-map key names in this file, sorted and deduped.
///
/// Catches the consumers (`vike_ops::reconcile_config`, the venue `config.rs` loaders,
/// `vike_bridge_core::key_permissions`) that read a caller-supplied map instead of process
/// env — the pattern `find_env_reads` cannot see.
///
/// THREE shapes, all found in the tree and all required (a `.get("LITERAL")`-only scanner was
/// tried first and silently missed the majority):
///   - `vars.get("VIKE_THING")`                    — literal key
///   - `vars.get(THING_ENV)`                       — key through a `const`, e.g.
///     `crates/bridges/polymarket/src/recon_client.rs`'s `poly_reconcile_enabled`
///   - `parse_i64(vars, "VIKE_THING", 3_600_000)`  — key passed to a helper, e.g.
///     `crates/vike-ops/src/reconcile_config.rs`'s `build_recon_config`; in that ONE file only
///     4 of 11 keys sit at a `.get(` site, so anchoring on `.get(` would have dropped 7.
///
/// The third shape means we cannot anchor on a call syntax at all, so we take EVERY string
/// literal (and every resolvable `const`) whose value has env-var shape AND a known prefix.
/// The prefix gate is what keeps venue wire constants like `"PARTIALLY_FILLED"` out.
pub fn find_map_lookups(source: &str, consts: &BTreeMap<String, String>) -> Vec<String> {
    let mut out = std::collections::BTreeSet::new();

    // Shape 1 & 3: bare string literals anywhere in the file. The sweep itself lives in
    // `string_literals` so the credential-store scanner reuses this one tracker rather than
    // growing a second with its own quote-parity bugs.
    out.extend(string_literals(source).into_iter().filter(|lit| is_env_name(lit)));

    // Shape 2: `something.get(CONST)` where CONST resolves to an env-shaped name. Delegated to
    // `find_lookup_sites` so the two cannot drift: that function is BY CONSTRUCTION a subset of
    // this one, which is the property `find_map_lookups_contains_every_lookup_site` pins.
    out.extend(find_lookup_sites(source, consts));

    out.into_iter().collect()
}

/// Injected-map keys resolved at a REAL `.get(..)` call site — the PRECISE subset of
/// [`find_map_lookups`], and the only positive evidence of a map read this scanner can produce.
///
/// [`find_map_lookups`] deliberately answers the looser question "does this file MENTION an
/// env-shaped name", because the third read shape passes the key to a helper
/// (`parse_i64(vars, "VIKE_X", 3_600_000)`) and leaves no call syntax to anchor on. That
/// imprecision is load-bearing THERE — it is what keeps the exhaustiveness gate from missing the
/// majority of injected reads — but it makes a sighting worthless as proof that a lookup happens:
/// a file whose only mention is an `env::var("VIKE_X")` ARGUMENT reports the very same name, and
/// so does a `const X_ENV: &str = "VIKE_X";` declaration next to no read at all.
///
/// This function answers the narrower question the naming gate needs: is there a `.get(KEY)` whose
/// KEY resolves to an env-shaped name? A `.get(` is a real call site, so a hit is positive
/// evidence. MISSES ARE EXPECTED and are the blind spot the gate MEASURES rather than assumes —
/// see `crates/vike-ops/tests/settings_registry.rs`'s `map_lookup_proof_is_pinned`.
///
/// No false positive is possible despite `.get(` being ubiquitous (`slice.get(0)`, `map.get(&id)`,
/// `headers.get("content-type")`): the RESOLVED value must still pass [`is_env_name`], which
/// demands SCREAMING_SNAKE shape AND one of our known prefixes.
pub fn find_lookup_sites(source: &str, consts: &BTreeMap<String, String>) -> Vec<String> {
    let clean = strip_comments(source);
    let mut out = std::collections::BTreeSet::new();
    let mut from = 0usize;
    while let Some(hit) = clean[from..].find(".get(") {
        let open = from + hit + ".get(".len();
        if let Some(arg) = take_balanced(&clean[open..]) {
            // `&"X"` / `&KEY` are both common at a map lookup; `resolve_arg` handles the literal
            // and the `const` (including a `path::QUALIFIED` one) once the borrow is off.
            let arg = arg.trim().trim_start_matches('&').trim().to_string();
            if let Resolved::Name { name, .. } = resolve_arg(&arg, consts) {
                if is_env_name(&name) {
                    out.insert(name);
                }
            }
        }
        from = open;
    }
    out.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_literal_and_const_and_dynamic_args() {
        let src = r#"
fn a() { std::env::var("VIKE_ONE"); }
fn b() { env::var(TWO_ENV).ok(); }
fn c() { std::env::var_os("VIKE_THREE"); }
fn d() { std::env::var(format!("VIKE_MARK_STREAMS_{}", v)); }
"#;
        let reads = find_env_reads(src);
        let args: Vec<&str> = reads.iter().map(|r| r.arg.as_str()).collect();
        assert_eq!(
            args,
            vec![
                "\"VIKE_ONE\"",
                "TWO_ENV",
                "\"VIKE_THREE\"",
                "format!(\"VIKE_MARK_STREAMS_{}\", v)"
            ]
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
        assert!(
            of("use std::collections::BTreeMap;\nfn f() { let _ = Widget::new(); }\n").is_empty()
        );
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
        assert_eq!(
            reads[0].arg, "r#\"A\")B\"#",
            "argument must not truncate at the in-string paren"
        );
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
        assert!(defines_fn(
            "pub fn load_store(\n    home: Option<&Path>,\n) -> Map {\n",
            "load_store"
        ));
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
        assert_eq!(
            reads.len(),
            2,
            "the doc block and the definition are not call sites: {reads:?}"
        );
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
            Resolved::Name {
                name: "POLY_RECONCILE".into(),
                konst: Some("POLY_RECONCILE_ENV".into()),
            }
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
}
