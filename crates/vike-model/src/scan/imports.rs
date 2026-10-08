//! What a file's own `use` declarations bring into scope: the env-read and type spellings.

use super::lexer::is_word_byte;
use super::*;

/// The PATH-QUALIFIED spellings of a process-environment read. `env::var(` matches
/// `std::env::var(`, a bare `env::var(` after `use std::env`, and any other path ending in that
/// module, because the boundary check looks only at the byte before `env`.
pub(super) const QUALIFIED_ENV_READS: [&str; 2] = ["env::var(", "env::var_os("];

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
/// `crates/vike-ops/tests/settings_secrets/settings_registry.rs`'s `dynamic_sites_are_allowlisted` would demand a
/// `DYNAMIC_ALLOWLIST` row for a file that reads no environment at all. An import is evidence the
/// scanner can check, it costs no false positive, and it closes the alias case for free.
///
/// Measured 2026-08-28 across `crates/` and `xtask/`: NO file imports either function or aliases
/// the module, so this widening finds nothing today and moves no row. It is here because
/// `use std::env::var;` is one line, entirely ordinary, and would have made every read in that file
/// invisible to a gate whose whole subject is reads that hide.
pub(super) fn imported_env_read_patterns(clean: &str) -> Vec<String> {
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
/// repair, and it was written for `std::env` alone. `crates/vike-ops/tests/architecture/paper_mount_arming_gate.rs`
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
