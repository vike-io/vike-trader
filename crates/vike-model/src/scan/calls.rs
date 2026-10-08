//! The name-keyed scanners: call sites of named functions, definitions, and filesystem opens.

use super::lexer::{is_word_byte, take_balanced};
use super::*;

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
pub(super) fn starts_identifier(bytes: &[u8], at: usize) -> bool {
    at == 0 || !is_word_byte(bytes[at - 1])
}

/// Can `source` hold a `NAME(` match for any of `names` at all? The raw-text guard in front of
/// [`find_calls`], [`defines_fn`] and [`find_path_reads`] — a NECESSARY condition of each, so a
/// file that fails it is answered without the comment strip, which is the whole cost of those
/// three: a strip allocates and rewrites the file, the search after it is a `memchr`-speed scan.
///
/// ⚠ **Sound because [`strip_comments`] only DELETES**: each `//…` run up to, never including, its
/// `\n`. A `NAME(` match in the stripped text holds no `\n`, so it cannot straddle a deletion —
/// every deletion is followed by the `\n` it stopped at, or by the end of the text — and every
/// match in the stripped text is therefore the same bytes in `source`. The converse is false (a
/// `NAME(` inside a comment), which is why this only ever SKIPS a file and never accepts one;
/// `crates/vike-model/src/scan/calls_tests.rs`'s `the_name_guard_never_hides_a_match` drives each
/// matcher's accepted spellings through it.
///
/// MEASURED 2026-10-03 on a the latency box lane: `crates/vike-ops/tests/settings_secrets/settings_registry.rs` asked
/// [`defines_fn`] about each of its sixteen credential-store reader names for every file in the
/// tree, sixteen strips per file, and that alone was 7.3 s of one check and 11 s of one 16 s test.
///
/// ⚠ **Several bare identifiers are checked in ONE pass over the `(`s**, not one substring search
/// each: sixteen searches of the whole tree were still ~3 s per caller at opt-level 0 (MEASURED,
/// same lane), while a `(` is found at `memchr` speed and each one costs a short walk back. It is
/// the same condition: every matcher here demands the byte before `NAME` be a non-word byte
/// (`starts_identifier`), so the maximal word run ending at the `(` IS the name — provided the
/// name is itself all word bytes, which is why a qualified name (`a::b`, as some callers pass)
/// keeps the substring search.
fn spells_a_call_of(source: &str, names: &[&str]) -> bool {
    let bare = |name: &&str| !name.is_empty() && name.bytes().all(is_word_byte);
    if names.len() > 1 && names.iter().all(bare) {
        let bytes = source.as_bytes();
        return source.match_indices('(').any(|(at, _)| {
            let mut start = at;
            while start > 0 && is_word_byte(bytes[start - 1]) {
                start -= 1;
            }
            start < at && names.contains(&&source[start..at])
        });
    }
    names.iter().any(|name| source.contains(&format!("{name}(")))
}

/// Is the name at `at` preceded by the `fn` keyword — i.e. is this a DEFINITION, not a call?
///
/// Load-bearing, and not a nicety: `pub fn load_workspace_dotenv() -> HashMap<..>` in
/// `crates/vike-secrets/src/dotenv.rs` is spelled `NAME(` exactly like every call site, so without
/// this check the function that DEFINES a credential-store read would be reported as a library that
/// PERFORMS one. Checked as a whole token (the bytes before `fn` must not be word bytes) so an
/// identifier merely ending in `fn` cannot mask a real call.
pub(super) fn preceded_by_fn_keyword(bytes: &[u8], at: usize) -> bool {
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
    if !spells_a_call_of(source, names) {
        return Vec::new();
    }
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
    if !spells_a_call_of(source, &[name]) {
        return false;
    }
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
    if !spells_a_call_of(source, names) {
        return Vec::new();
    }
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
