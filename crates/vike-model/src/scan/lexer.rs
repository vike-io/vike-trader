//! The shared lexical layer: comment stripping, balanced arguments, literals, identifier bounds.

#[cfg(doc)]
use super::{find_map_lookups, find_path_reads};

/// A byte that can appear inside a Rust identifier — used for the word-boundary checks below
/// (an `r` preceded by one of these is part of an identifier like `for`/`var`, not a raw-string
/// opener; likewise for the `env` in a pattern match).
pub(super) fn is_word_byte(b: u8) -> bool {
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
/// `crates/vike-ops/tests/architecture/paper_mount_arming_gate.rs` reads the paper client's real constructors out
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
        // and since moved to `crates/vike-secrets/src/db/migrate/carry.rs`'s
        // `parse_credential_file` (`parse_dotenv` in `dotenv.rs` until 2026-10-07), which silently
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
pub(super) fn take_balanced(rest: &str) -> Option<String> {
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

/// The last non-whitespace byte before `at`, if any — the method-position probe below.
pub(super) fn prev_significant(bytes: &[u8], at: usize) -> Option<u8> {
    bytes[..at].iter().rev().find(|b| !b.is_ascii_whitespace()).copied()
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
