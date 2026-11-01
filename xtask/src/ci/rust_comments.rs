//! Is an edit of a `.rs` file COMMENTS ONLY?
//! (`docs/decisions/0139-a-comment-only-edit-of-a-crate-source-plans-its-crate-alone-and-no-latency-gate.md`,
//! amending decision 0130.) The planner asks it of every crate source in a change, once
//! ([`super::git::rust_comment_only_files`]), and such a file then plans its own crate into the lane and
//! nothing else: no reverse-dependency walk, no feature suite, no job, and no latency run (the latency
//! gate's third drop, beside the test-only file and the crate document;
//! [`super::selection::latency_compiled`] is that decision's one site).
//!
//! # The method: one lexer over the WHOLE file, at the base and at HEAD
//!
//! [`strip`] lexes a Rust source once and returns two things: the token text with every comment and
//! every whitespace run collapsed to ONE space ([`Stripped::tokens`]), and which lines carry a code
//! token ([`Stripped::code_lines`]). [`rust_edit_is_comment_only`] then says yes only when BOTH hold:
//!
//!   1. the stripped token text of the base file and of the HEAD file are EQUAL, and
//!   2. every line the `git diff -U0` hunks add or remove carries no code token on its side.
//!
//! Rule 1 is the proof: equal stripped texts are the same token sequence, so the compiler sees the same
//! program modulo comments and line numbers. Rule 2 is the owner's stricter reading on top of it: a
//! changed line that carries code AND a comment (a trailing comment added to `let x = 1;`) is a code
//! edit, and so is a whitespace change inside a code line (`a+b` to `a + b`, re-indented code).
//!
//! WHY not the per-line heuristic `super::git::patch_is_comment_only` uses for `#` files. A `-U0` hunk
//! shows no context: a changed line inside a multi-line `/* */` block carries no `//` and reads as code,
//! and a `// …` line inside a multi-line raw string reads as a comment though it is data. Lexing both
//! whole files sees every such context, and comparing the two token texts is robust to all of them.
//!
//! # What is a comment here
//!
//! `//`, `///`, `//!` line comments and `/* */`, `/** */`, `/*! */` block comments (nested), blank lines
//! and whitespace outside code lines. A doc comment IS a comment (the owner's ruling), though rustc lowers
//! it to `#[doc]`: its text can reach a proc macro's output (clap's help strings), never the speed of a
//! measured instruction.
//!
//! NOT a comment, so a change fires: an attribute (`#[inline]`, `#[cfg(…)]`, `#[doc = "…"]`,
//! `#![doc = include_str!(…)]`); anything inside a string, byte string, C string, raw string or char
//! literal that merely looks like a comment; a changed line that carries code beside a comment; and ANY
//! comment inside a `macro_rules!` definition (from the `macro_rules` word to the close of its body): a
//! matcher may capture a doc comment as `$(#[$m:meta])*`, so the lexer keeps those comments as tokens.
//!
//! # Doubt is "not comment-only" (the gate fires)
//!
//! No text on either side (a new or deleted file, a rename, which `--no-renames` splits into the two, a
//! file that is not UTF-8), a diff header other than a plain edit (a mode change, a binary diff), a hunk
//! header that does not parse or names a line the file does not have, a source the lexer cannot read to
//! a balanced end (an unterminated string, char literal or block comment, an unbalanced delimiter, an
//! unknown `r#` form), and a shebang line.
//!
//! The accepted residual: a comment edit can change the number of lines of a file, which moves the
//! `line!()` and panic-location constants compiled into the binary. That cannot change its speed.

/// One Rust source after [`strip`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stripped {
    /// The source's tokens in order, each copied verbatim (a literal keeps its inner whitespace and its
    /// `//`), with every run of whitespace and comments between two tokens written as ONE space. No
    /// leading or trailing space.
    pub tokens: String,
    /// Per line of the source (index 0 = line 1; one entry per `\n`-separated line, so a file ending in
    /// `\n` has a last, empty entry): true iff some code token, or a comment kept as a token inside a
    /// `macro_rules!` definition, has a character on that line.
    pub code_lines: Vec<bool>,
}

/// Rust's own whitespace (`Pattern_White_Space`), which is narrower than [`char::is_whitespace`].
fn is_ws(c: char) -> bool {
    matches!(
        c,
        '\t' | '\n'
            | '\u{0B}'
            | '\u{0C}'
            | '\r'
            | ' '
            | '\u{85}'
            | '\u{200E}'
            | '\u{200F}'
            | '\u{2028}'
            | '\u{2029}'
    )
}

fn id_start(c: char) -> bool {
    c == '_' || c.is_alphabetic()
}

fn id_continue(c: char) -> bool {
    c == '_' || c.is_alphanumeric()
}

/// The output half of the lexer: the token text and the per-line marks, advanced over `[s, e)` spans.
struct Out<'a> {
    c: &'a [char],
    tokens: String,
    code_lines: Vec<bool>,
    line: usize,
    /// A whitespace or comment run was skipped since the last token.
    sep: bool,
}

impl Out<'_> {
    /// Copy `[s, e)` as one token, marking every line it touches.
    fn keep(&mut self, s: usize, e: usize) {
        if self.sep && !self.tokens.is_empty() {
            self.tokens.push(' ');
        }
        self.sep = false;
        for &ch in &self.c[s..e] {
            self.tokens.push(ch);
            if let Some(slot) = self.code_lines.get_mut(self.line) {
                *slot = true;
            }
            if ch == '\n' {
                self.line += 1;
            }
        }
    }

    /// Skip `[s, e)` (whitespace or a comment): one separator, and the line count follows it.
    fn skip(&mut self, s: usize, e: usize) {
        self.sep = true;
        self.line += self.c[s..e].iter().filter(|&&ch| ch == '\n').count();
    }
}

/// Lex `src` once ([`Stripped`] says what comes back). `None` when it does not lex to a balanced end:
/// an unterminated literal or block comment, an unbalanced `)`/`]`/`}` or one left open, an `r#` that is
/// neither a raw string nor a raw identifier, a char literal of no known shape, or a shebang line.
pub fn strip(src: &str) -> Option<Stripped> {
    // A shebang is skipped by rustc as a line of its own grammar; nothing here reads it.
    if src.starts_with("#!") && !src.starts_with("#![") {
        return None;
    }
    let c: Vec<char> = src.chars().collect();
    let n = c.len();
    let at = |i: usize| c.get(i).copied();
    let mut out = Out {
        c: &c,
        tokens: String::new(),
        code_lines: vec![false; src.split('\n').count()],
        line: 0,
        sep: false,
    };

    // The end (exclusive) of a `"…"` literal whose opening quote is at `q`, escapes honoured.
    let quoted = |q: usize| -> Option<usize> {
        let mut k = q + 1;
        loop {
            match at(k)? {
                '\\' => k += 2,
                '"' => return Some(k + 1),
                _ => k += 1,
            }
        }
    };
    // The end of what a `'` at `q` opens: a char literal, or (when `lifetime`) a lifetime or label. The
    // shapes are rustc's lexer's: `'\…'`, a one-character `'x'` (`'/'`, `'"'` and `' '` included), then an
    // identifier after `'` is a lifetime unless a `'` closes it; anything else is no known shape.
    let quote = |q: usize, lifetime: bool| -> Option<usize> {
        let a = at(q + 1)?;
        if a == '\\' {
            let mut k = q + 3;
            while at(k)? != '\'' {
                if at(k)? == '\n' {
                    return None;
                }
                k += 1;
            }
            return Some(k + 1);
        }
        if a != '\n' && at(q + 2) == Some('\'') {
            return Some(q + 3);
        }
        if lifetime && (id_start(a) || a.is_ascii_digit()) {
            let mut k = q + 1;
            if a == 'r' && at(q + 2) == Some('#') && at(q + 3).is_some_and(id_start) {
                k = q + 3;
            }
            while at(k).is_some_and(id_continue) {
                k += 1;
            }
            return Some(if at(k) == Some('\'') { k + 1 } else { k });
        }
        None
    };

    let mut depth = 0usize;
    // `macro_rules ! NAME {…}`: 1 after `macro_rules`, 2 after `!`, 3 after the name; then the body.
    let mut macro_step = 0u8;
    // `Some(d)`: inside a `macro_rules!` body whose delimiter opened at depth `d`.
    let mut macro_body: Option<usize> = None;
    let mut i = 0usize;
    while i < n {
        let ch = c[i];
        let keep_comments = macro_step != 0 || macro_body.is_some();
        if is_ws(ch) {
            let s = i;
            while i < n && is_ws(c[i]) {
                i += 1;
            }
            out.skip(s, i);
            continue;
        }
        if ch == '/' && at(i + 1) == Some('/') {
            let s = i;
            while i < n && c[i] != '\n' {
                i += 1;
            }
            if keep_comments {
                out.keep(s, i)
            } else {
                out.skip(s, i)
            }
            continue;
        }
        if ch == '/' && at(i + 1) == Some('*') {
            let s = i;
            i += 2;
            let mut nest = 1usize;
            while nest > 0 {
                match (at(i)?, at(i + 1)) {
                    ('/', Some('*')) => {
                        nest += 1;
                        i += 2;
                    }
                    ('*', Some('/')) => {
                        nest -= 1;
                        i += 2;
                    }
                    _ => i += 1,
                }
            }
            if keep_comments {
                out.keep(s, i)
            } else {
                out.skip(s, i)
            }
            continue;
        }

        let mut ident: Option<String> = None;
        let mut punct: Option<char> = None;
        let end = if id_start(ch) {
            let mut j = i;
            while at(j).is_some_and(id_continue) {
                j += 1;
            }
            let word: String = c[i..j].iter().collect();
            match (word.as_str(), at(j)) {
                ("r" | "br" | "cr", Some('"' | '#')) => {
                    let mut hashes = 0usize;
                    while at(j + hashes) == Some('#') {
                        hashes += 1;
                    }
                    if at(j + hashes) == Some('"') {
                        // A raw string: up to `"` followed by the same number of `#`.
                        let mut k = j + hashes + 1;
                        loop {
                            if at(k)? == '"' && (1..=hashes).all(|h| at(k + h) == Some('#')) {
                                break k + 1 + hashes;
                            }
                            k += 1;
                        }
                    } else if word == "r" && hashes == 1 && at(j + 1).is_some_and(id_start) {
                        // A raw identifier (`r#type`): a name.
                        let mut k = j + 1;
                        while at(k).is_some_and(id_continue) {
                            k += 1;
                        }
                        ident = Some(c[j + 1..k].iter().collect());
                        k
                    } else {
                        return None;
                    }
                }
                ("b" | "c", Some('"')) => quoted(j)?,
                ("b", Some('\'')) => quote(j, false)?,
                _ => {
                    ident = Some(word);
                    j
                }
            }
        } else if ch.is_ascii_digit() {
            let mut j = i;
            while at(j).is_some_and(id_continue) {
                j += 1;
            }
            j
        } else if ch == '"' {
            quoted(i)?
        } else if ch == '\'' {
            quote(i, true)?
        } else {
            punct = Some(ch);
            i + 1
        };
        out.keep(i, end);
        i = end;

        match punct {
            Some('{' | '(' | '[') => {
                if macro_step == 3 && macro_body.is_none() {
                    macro_body = Some(depth);
                }
                depth += 1;
            }
            Some('}' | ')' | ']') => {
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
            match (macro_step, ident.as_deref(), punct) {
                (_, Some("macro_rules"), _) => 1,
                (1, _, Some('!')) => 2,
                (2, Some(_), _) => 3,
                _ => 0,
            }
        };
    }
    if depth != 0 {
        return None;
    }
    let Out { tokens, code_lines, .. } = out;
    Some(Stripped { tokens, code_lines })
}

/// `(old start, old count, new start, new count)` of a hunk header's tail (` -a[,b] +c[,d] @@ …`).
fn hunk_ranges(tail: &str) -> Option<(usize, usize, usize, usize)> {
    let mut parts = tail.split_whitespace();
    let range = |s: Option<&str>, sign: char| -> Option<(usize, usize)> {
        let s = s?.strip_prefix(sign)?;
        match s.split_once(',') {
            Some((a, b)) => Some((a.parse().ok()?, b.parse().ok()?)),
            None => Some((s.parse().ok()?, 1)),
        }
    };
    let (a, b) = range(parts.next(), '-')?;
    let (c, d) = range(parts.next(), '+')?;
    Some((a, b, c, d))
}

/// True iff `patch` (a `git diff -U0` of ONE `.rs` file), the file's text at the base (`old`) and at
/// HEAD (`new`) prove the edit changes comments only: the two [`strip`] token texts are equal and no
/// added or removed line carries a code token. Every doubt is `false` (the module doc lists them).
pub fn rust_edit_is_comment_only(patch: &str, old: Option<&str>, new: Option<&str>) -> bool {
    let (Some(old), Some(new)) = (old, new) else { return false };
    let (Some(old), Some(new)) = (strip(old), strip(new)) else { return false };
    if old.tokens != new.tokens {
        return false;
    }
    // A line with no code on its side; a line number the file does not have is doubt.
    let quiet = |s: &Stripped, ln: usize| {
        ln.checked_sub(1).and_then(|k| s.code_lines.get(k)) == Some(&false)
    };
    let mut in_hunk = false;
    let mut changed = false;
    // The `-` and `+` lines still owed by the current hunk, as its header counts them.
    let (mut minus, mut plus) = (0usize, 0usize);
    for line in patch.lines() {
        if let Some(tail) = line.strip_prefix("@@") {
            if minus != 0 || plus != 0 {
                return false;
            }
            let Some((a, b, c, d)) = hunk_ranges(tail) else { return false };
            if !(a..a + b).all(|ln| quiet(&old, ln)) || !(c..c + d).all(|ln| quiet(&new, ln)) {
                return false;
            }
            (minus, plus, in_hunk) = (b, d, true);
            changed |= b + d > 0;
            continue;
        }
        if !in_hunk {
            // A plain edit's header is exactly these four; `new file mode`, `deleted file mode`,
            // `old mode`, `Binary files` and the rest are not an edit a lexer can judge.
            let plain = ["diff --git ", "index ", "--- ", "+++ "];
            if plain.iter().any(|p| line.starts_with(p)) {
                continue;
            }
            return false;
        }
        match line.as_bytes().first() {
            Some(b'-') if minus > 0 => minus -= 1,
            Some(b'+') if plus > 0 => plus -= 1,
            // "\ No newline at end of file" belongs to the line above it, which was judged.
            Some(b'\\') => {}
            _ => return false,
        }
    }
    changed && minus == 0 && plus == 0
}
