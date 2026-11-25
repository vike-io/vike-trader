//! **What the hashes read of a source** (`source_hash.rs`'s `HashForm::Tokens`, the owner's ruling of
//! 2026-11-20, "I don't want to recompile all because of a comment"): a Rust source as its TOKENS and a
//! manifest without its comments and layout, so an edit of comments, doc comments or whitespace alone moves
//! neither the source hash nor the boundary hash and rebuilds nothing. Std only, like the walk: `build.rs`,
//! xtask and `src/sdk.rs` compile it in through `source_hash.rs`.
//!
//! ⚠ **The OUTPUT of [`rust_tokens`] and [`toml_tokens`] is FROZEN once an SDK is cut with it**: every later
//! SDK marker (its tokens form, `SdkMarker::verify`), every SDK's baked boundary and every adapter's recorded
//! boundary is a hash of it, so a change of a single output byte refuses every installed SDK on every box.
//! Never change it in place: a change is a NEW hash form (`HashForm`), the old one kept for the markers
//! written with it. `tests/source_hash.rs`'s `the_token_forms_are_pinned_byte_for_byte` holds it.
//!
//! **Why a lexer of its own.** `vike_model`'s `moved_name::lex` blanks every literal (a changed string would
//! not move the hash) and lives in a crate this std-only walk cannot link; xtask's `ci::rust_comments::strip`
//! is not in the SDK tree a strategy file's own build compiles, and keeps a space wherever one stood.
//!
//! **A Rust source** ([`rust_tokens`]): every comment (`//`, `/* */` nested, and the doc comments `///`,
//! `//!`, `/** */`, `/*! */`) and every whitespace run is dropped; each token is copied as written (a string,
//! byte, C or raw string and a char literal byte for byte, a number with its suffix and exponent); tokens are
//! joined by ONE space, except two punctuation characters rustc may glue into one token ([`glues`]) that
//! stood side by side, which stay joined (`&&` is not `& &`, `<<` not `< <`). Re-lexing that text gives
//! back the same tokens, so two sources with one text differ only in comments and layout (and a source
//! read so is never split where rustc reads one token: a number keeps its fraction and exponent, a literal
//! its prefix). `None` (the caller hashes the bytes) for a source that does not
//! lex to a balanced end: an unterminated literal or comment, a quote of no known shape, a delimiter left
//! open or closed twice.
//!
//! KEPT as tokens, so an edit of them still moves the hash: every comment inside a `macro_rules!` definition
//! (a matcher may hold a doc comment as a literal or capture one, the planner's rule, decision 0139), and
//! every comment holding `{{`, with a `{{NAME}}` placeholder as one token: the builder fills a template's
//! placeholders as text (`crates/vike-strategy-builder/src/render.rs`), so one in a comment is not inert.
//!
//! **A manifest** ([`toml_tokens`]): every `#` comment and every space, tab and carriage return outside a
//! string dropped, and every blank line; strings (basic, literal, multi-line) as written, and every other
//! newline, which TOML reads. A comment holding `{{` is kept, as in a Rust template.
//!
//! **The accepted residual**, what such an edit can still change in a built file without moving the hash:
//! the line numbers compiled in (`line!()`, a panic's location) and the spacing `stringify!` and `assert!`
//! print, never a value a strategy computes; and the text of a doc comment, which reaches code only through
//! a macro that reads `#[doc]` (`clap`'s and `schemars`' derives: optional and off on every edge the hashed
//! closure compiles). A file the hashed sources READ as text (`include_str!`, `include_bytes!`) is never
//! read this way: the walk hashes its bytes.

/// Rust's own whitespace (`Pattern_White_Space`) among the ASCII bytes; the others are kept as token bytes,
/// the safe direction.
fn is_space(b: u8) -> bool {
    matches!(b, b'\t' | b'\n' | 0x0B | 0x0C | b'\r' | b' ')
}

/// A byte an identifier (or a number, a suffix) may hold: ASCII alphanumerics, `_` and every non-ASCII
/// byte (a Unicode identifier's).
fn is_word(b: u8) -> bool {
    b == b'_' || b.is_ascii_alphanumeric() || b >= 0x80
}

/// A punctuation character rustc may glue to the one right after it into one token (`&&`, `<<=`, `::`,
/// `->`, `..=`, `=>`): side by side, two of them stay joined. A delimiter, `,`, `;`, `#`, `$`, `?`, `@` and
/// `~` never glue, so the layout around one is no token's.
fn glues(b: u8) -> bool {
    matches!(
        b,
        b'!' | b'%'
            | b'&'
            | b'*'
            | b'+'
            | b'-'
            | b'.'
            | b'/'
            | b':'
            | b'<'
            | b'='
            | b'>'
            | b'^'
            | b'|'
    )
}

/// The byte length of the UTF-8 character whose first byte is `b`.
fn char_len(b: u8) -> usize {
    match b {
        0xF0.. => 4,
        0xE0.. => 3,
        0xC0.. => 2,
        _ => 1,
    }
}

/// `text` as the hashes read a Rust source (the module doc says what is dropped and what is kept); `None`
/// when it does not lex to a balanced end.
pub(crate) fn rust_tokens(text: &str) -> Option<String> {
    let b = text.as_bytes();
    let n = b.len();
    let at = |i: usize| b.get(i).copied();
    let mut out: Vec<u8> = Vec::with_capacity(n / 2);
    // The last token is a punctuation character that glues ([`glues`]) and nothing has stood since: one
    // next joins it.
    let mut joins = false;
    let mut depth = 0usize;
    // `macro_rules ! NAME {…}`: 1 after `macro_rules`, 2 after `!`, 3 after the name; then the body.
    let mut macro_step = 0u8;
    let mut macro_body: Option<usize> = None;
    let mut i = 0usize;
    while i < n {
        let c = b[i];
        if is_space(c) {
            i += 1;
            joins = false;
            continue;
        }
        let comment_end = match (c, at(i + 1)) {
            (b'/', Some(b'/')) => {
                Some(b[i..].iter().position(|&x| x == b'\n').map_or(n, |p| i + p))
            }
            (b'/', Some(b'*')) => {
                let mut k = i + 2;
                let mut nest = 1usize;
                while nest > 0 {
                    match (at(k)?, at(k + 1)) {
                        (b'/', Some(b'*')) => (nest, k) = (nest + 1, k + 2),
                        (b'*', Some(b'/')) => (nest, k) = (nest - 1, k + 2),
                        _ => k += 1,
                    }
                }
                Some(k)
            }
            _ => None,
        };
        if let Some(end) = comment_end {
            let comment = &b[i..end];
            let kept =
                macro_step != 0 || macro_body.is_some() || comment.windows(2).any(|w| w == b"{{");
            if kept {
                push_token(&mut out, comment);
                // A kept line comment ends at its newline: a token after it is never read as its text.
                if comment.starts_with(b"//") {
                    out.push(b'\n');
                }
            }
            i = end;
            joins = false;
            continue;
        }
        let (end, word, glued) = if let Some(end) = placeholder_end(b, i) {
            (end, false, false)
        } else if c.is_ascii_digit() {
            (number_end(b, i), false, false)
        } else if is_word(c) {
            let mut j = i;
            while at(j).is_some_and(is_word) {
                j += 1;
            }
            match (&b[i..j], at(j)) {
                (b"r" | b"br" | b"cr", Some(b'"' | b'#')) => {
                    let hashes = b[j..].iter().take_while(|&&x| x == b'#').count();
                    if at(j + hashes) == Some(b'"') {
                        let mut k = j + hashes + 1;
                        loop {
                            if at(k)? == b'"' && (1..=hashes).all(|h| at(k + h) == Some(b'#')) {
                                break (k + 1 + hashes, false, false);
                            }
                            k += 1;
                        }
                    } else if &b[i..j] == b"r" && hashes == 1 && at(j + 1).is_some_and(is_word) {
                        let mut k = j + 1;
                        while at(k).is_some_and(is_word) {
                            k += 1;
                        }
                        (k, true, false)
                    } else {
                        return None;
                    }
                }
                (b"b" | b"c", Some(b'"')) => (quoted_end(b, j)?, false, false),
                (b"b", Some(b'\'')) => (quote_end(b, j, false)?, false, false),
                _ => (j, true, false),
            }
        } else if c == b'"' {
            (quoted_end(b, i)?, false, false)
        } else if c == b'\'' {
            (quote_end(b, i, true)?, false, false)
        } else {
            (i + char_len(c), false, glues(c))
        };
        let token = &b[i..end];
        if glued && joins {
            out.extend_from_slice(token);
        } else {
            push_token(&mut out, token);
        }
        i = end;
        joins = glued;

        match token {
            b"{" | b"(" | b"[" => {
                if macro_step == 3 && macro_body.is_none() {
                    macro_body = Some(depth);
                }
                depth += 1;
            }
            b"}" | b")" | b"]" => {
                depth = depth.checked_sub(1)?;
                if macro_body == Some(depth) {
                    macro_body = None;
                }
            }
            _ => {}
        }
        macro_step = if macro_body.is_some() {
            0
        } else {
            match (macro_step, token) {
                (_, b"macro_rules") if word => 1,
                (1, b"!") => 2,
                (2, _) if word => 3,
                _ => 0,
            }
        };
    }
    if depth != 0 {
        return None;
    }
    String::from_utf8(out).ok()
}

/// Separate a new token from the one before it (one space; nothing before the first).
fn push_token(out: &mut Vec<u8>, token: &[u8]) {
    if !out.is_empty() && out.last() != Some(&b'\n') {
        out.push(b' ');
    }
    out.extend_from_slice(token);
}

/// The end of a template placeholder `{{NAME}}` (`NAME` of `A-Z`, `0-9` and `_`) opening at `i`, if one
/// does.
fn placeholder_end(b: &[u8], i: usize) -> Option<usize> {
    let rest = b[i..].strip_prefix(b"{{")?;
    let name = rest
        .iter()
        .take_while(|x| x.is_ascii_uppercase() || x.is_ascii_digit() || **x == b'_')
        .count();
    (name > 0 && rest[name..].starts_with(b"}}")).then_some(i + 4 + name)
}

/// The end of the number starting at `i`, as rustc's lexer reads it: its digits and suffix, a fraction
/// (`1.5`, `1.` when no `.` or identifier follows the dot) and an exponent's sign (`1e-3`, not in a
/// hexadecimal `0x1e`).
fn number_end(b: &[u8], i: usize) -> usize {
    let at = |k: usize| b.get(k).copied();
    let word = |mut k: usize| {
        while at(k).is_some_and(is_word) {
            k += 1;
        }
        k
    };
    let mut j = word(i);
    if at(j) == Some(b'.')
        && at(j + 1).is_none_or(|x| x != b'.' && !(is_word(x) && !x.is_ascii_digit()))
    {
        j = word(j + 1);
    }
    let hexadecimal = b[i..].starts_with(b"0x") || b[i..].starts_with(b"0X");
    if !hexadecimal
        && matches!(b[j - 1], b'e' | b'E')
        && matches!(at(j), Some(b'+' | b'-'))
        && at(j + 1).is_some_and(|x| x.is_ascii_digit())
    {
        j = word(j + 1);
    }
    j
}

/// The end of the `"…"` literal whose opening quote is at `q`, escapes honoured.
fn quoted_end(b: &[u8], q: usize) -> Option<usize> {
    let mut k = q + 1;
    loop {
        match *b.get(k)? {
            b'\\' => k += 2,
            b'"' => return Some(k + 1),
            _ => k += 1,
        }
    }
}

/// The end of what a `'` at `q` opens: a char literal (`'\…'`, one character), or (when `lifetime`) a
/// lifetime or label (`'a`, `'r#a`); `None` for any other shape.
fn quote_end(b: &[u8], q: usize, lifetime: bool) -> Option<usize> {
    let at = |k: usize| b.get(k).copied();
    let a = at(q + 1)?;
    if a == b'\\' {
        let mut k = q + 3;
        while at(k)? != b'\'' {
            if at(k)? == b'\n' {
                return None;
            }
            k += 1;
        }
        return Some(k + 1);
    }
    let width = char_len(a);
    if a != b'\n' && at(q + 1 + width) == Some(b'\'') {
        return Some(q + 2 + width);
    }
    if lifetime && is_word(a) {
        let mut k = q + 1;
        if a == b'r' && at(q + 2) == Some(b'#') && at(q + 3).is_some_and(is_word) {
            k = q + 3;
        }
        while at(k).is_some_and(is_word) {
            k += 1;
        }
        return Some(if at(k) == Some(b'\'') { k + 1 } else { k });
    }
    None
}

/// `text` as the hashes read a TOML manifest (the module doc says what is dropped).
pub(crate) fn toml_tokens(text: &str) -> String {
    let b = text.as_bytes();
    let n = b.len();
    let mut out: Vec<u8> = Vec::with_capacity(n);
    let mut i = 0usize;
    while i < n {
        match b[i] {
            b' ' | b'\t' | b'\r' => i += 1,
            b'\n' => {
                if !out.is_empty() && out.last() != Some(&b'\n') {
                    out.push(b'\n');
                }
                i += 1;
            }
            b'#' => {
                let end = b[i..].iter().position(|&x| x == b'\n').map_or(n, |p| i + p);
                if b[i..end].windows(2).any(|w| w == b"{{") {
                    out.extend_from_slice(b[i..end].trim_ascii_end());
                }
                i = end;
            }
            quote @ (b'"' | b'\'') => {
                let end = toml_string_end(b, i, quote);
                out.extend_from_slice(&b[i..end]);
                i = end;
            }
            other => {
                out.push(other);
                i += 1;
            }
        }
    }
    String::from_utf8(out).unwrap_or_else(|_| text.to_string())
}

/// The end of the TOML string opening at `i` with `quote`: a multi-line one (three quotes, closed by three to
/// five, the extra ones its own) or a one-line one, a basic string's escapes honoured; the text's end when
/// it never closes.
fn toml_string_end(b: &[u8], i: usize, quote: u8) -> usize {
    let n = b.len();
    let run = |k: usize| b[k..].iter().take_while(|&&x| x == quote).count();
    if run(i) >= 3 {
        let mut k = i + 3;
        while k < n {
            if quote == b'"' && b[k] == b'\\' {
                k += 2;
            } else if b[k] == quote && run(k) >= 3 {
                return (k + run(k).min(5)).min(n);
            } else {
                k += 1;
            }
        }
        return n;
    }
    let mut k = i + 1;
    while k < n && b[k] != b'\n' {
        if quote == b'"' && b[k] == b'\\' {
            k += 2;
        } else if b[k] == quote {
            return k + 1;
        } else {
            k += 1;
        }
    }
    k.min(n)
}
