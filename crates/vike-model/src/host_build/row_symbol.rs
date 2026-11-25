//! The bars' and ticks' `symbol` TYPE change of the SDK cut on 2026-11-20 (decision 0296's amendment of that
//! day), as text EDITS on a user strategy file, and the builder's one-line hint for a file that still reads
//! it as a `String`.
//!
//! Until that cut `vike_marketdata::Bar::symbol` was an `Option<String>` and `TickTrade::symbol` /
//! `TickQuote::symbol` a `String`. Since it they are an `Option<vike_marketdata::Symbol>` and a `Symbol`, an
//! interned id that reads as a `&str`, so most reads build unchanged (`.as_deref()`, `==`, `&t.symbol` as a
//! `&str`, `format!`). What no longer builds is a read that needs the `String` back and a `String` written
//! into a row. No path moved, so no SDK step's rename table can carry this (`super::source_edits`); it is a
//! rule of its own, which `vike-cli strategy migrate` applies to a file pinned to an SDK whose rows held
//! `String`s (or to one that pins none).
//!
//! What is EDITED, each restoring the old `String` type exactly, so it builds wherever the old code built:
//! - **M1**: `p.symbol.clone()`, where `p` is a parameter the same `fn` declares `&Bar`, `&TickTrade` or
//!   `&TickQuote`, becomes `p.symbol.map(String::from)` for a bar and `p.symbol.to_string()` for a tick;
//! - **M2**: `&p.symbol` for such a bar parameter, not followed by `.`, becomes `&p.symbol.map(String::from)`;
//! - **M3**: in a `TickTrade { … }` or `TickQuote { … }` literal, `symbol: e` becomes `symbol: e.into()`
//!   (`(e).into()` when `e` is no plain postfix chain), unless `e` already ends in `.into()` or is
//!   `Default::default()`; in a `Bar { … }` literal it becomes `symbol: e.map(Into::into)`, unless `e` is
//!   `None` or already ends in `.into())` or `.map(Into::into)`. A shorthand `symbol` field is spelled out.
//!
//! Any other `x.symbol.clone()` or `&x.symbol` (a receiver this rule cannot type; the strategy's own
//! `self.…` fields aside) is left as it is and draws a HINT naming its line: it may be another type's
//! `symbol` (a closed trade's is still a `String`), and the build says whether it moved.
//!
//! ⚠ A text walk is not a compiler (`super::source_edits`'s module doc): every caller builds the result
//! before it keeps it, so a wrong edit is a build error, never a silent change. Each edit's result is a shape
//! the rule leaves alone, so a second pass finds nothing.
//!
//! Pure: no file, no environment and no clock.

use std::ops::Range;

use super::moved_name;
use super::source_edits::{SourceEdit, SourceEdits};

/// Every edit's one line.
const EDIT_WORDS: &str =
    "a bar's and a tick's `symbol` is a `vike_marketdata::Symbol` since the SDK of 2026-11-20";

/// Which row a type name is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Row {
    Bar,
    Tick,
}

fn row_of(name: &str) -> Option<Row> {
    match name {
        "Bar" => Some(Row::Bar),
        "TickTrade" | "TickQuote" => Some(Row::Tick),
        _ => None,
    }
}

/// The edits that carry `source`'s reads and writes of a row's symbol onto `Symbol` (M1 to M3, the module
/// doc), sorted, and a hint per read this rule cannot type.
pub fn symbol_edits(source: &str) -> SourceEdits {
    let (_, spans) = moved_name::lex(source);
    let w: Vec<&str> = spans.iter().map(|s| &source[s.clone()]).collect();
    let line = |k: usize| source[..spans[k].start].matches('\n').count() + 1;
    let mut edits: Vec<(Range<usize>, String)> = Vec::new();
    // The `.` before `symbol` of every read M1 or M2 carried, so no hint names it.
    let mut carried: Vec<usize> = Vec::new();

    // M1 and M2: the reads of each `fn`'s row parameters, inside that `fn`'s body.
    for f in (0..w.len()).filter(|&f| w[f] == "fn") {
        let Some(open) = (f..w.len()).find(|&k| w[k] == "(") else { continue };
        let Some(close) = matching(&w, open) else { continue };
        let Some(body) = (close..w.len()).find(|&k| w[k] == "{" || w[k] == ";") else { continue };
        let Some(end) = matching(&w, body) else { continue };
        let params = row_parameters(&w, open + 1..close);
        for k in body + 1..end {
            let Some(&(_, row)) = params.iter().find(|(p, _)| *p == w[k]) else { continue };
            if matches!(w[k - 1], "." | "::") || !at(&w, k + 1, &[".", "symbol"]) {
                continue;
            }
            if at(&w, k + 3, &[".", "clone", "(", ")"]) {
                let new = match row {
                    Row::Bar => "map(String::from)",
                    Row::Tick => "to_string()",
                };
                edits.push((spans[k + 4].start..spans[k + 6].end, new.to_string()));
                carried.push(k + 1);
            } else if row == Row::Bar && w[k - 1] == "&" && w.get(k + 3) != Some(&".") {
                edits.push((spans[k + 2].end..spans[k + 2].end, ".map(String::from)".to_string()));
                carried.push(k + 1);
            }
        }
    }

    // M3: the `symbol` field of every row literal.
    for k in 0..w.len() {
        let Some(row) = row_of(w[k]) else { continue };
        if w.get(k + 1) != Some(&"{") || !a_literal(&w, k) {
            continue;
        }
        let Some(close) = matching(&w, k + 1) else { continue };
        if a_pattern(&w, k, close) {
            continue;
        }
        let mut field = k + 2;
        while field < close {
            let next = field_end(&w, field, close);
            if w[field] == "symbol" {
                if w.get(field + 1) == Some(&":") && field + 2 < next {
                    let value = &source[spans[field + 2].start..spans[next - 1].end];
                    let new = match row {
                        Row::Tick
                            if value.ends_with(".into()") || value == "Default::default()" =>
                        {
                            None
                        }
                        Row::Bar
                            if value == "None"
                                || value.ends_with(".into())")
                                || value.ends_with(".map(Into::into)") =>
                        {
                            None
                        }
                        Row::Tick => {
                            Some(format!("{}.into()", receiver(&w, field + 2..next, value)))
                        }
                        Row::Bar => Some(format!(
                            "{}.map(Into::into)",
                            receiver(&w, field + 2..next, value)
                        )),
                    };
                    if let Some(new) = new {
                        edits.push((spans[field + 2].start..spans[next - 1].end, new));
                    }
                } else if field + 1 == next {
                    let new = match row {
                        Row::Tick => ": symbol.into()",
                        Row::Bar => ": symbol.map(Into::into)",
                    };
                    edits.push((spans[field].end..spans[field].end, new.to_string()));
                }
            }
            field = next + 1;
        }
    }

    // The hints: every other `x.symbol.clone()` and `&x.symbol`, `self`'s own field aside.
    let mut found = SourceEdits::default();
    for k in 1..w.len() {
        if w[k] != "." || w.get(k + 1) != Some(&"symbol") || carried.contains(&k) {
            continue;
        }
        let read = receiver_chain(&w, k);
        // The strategy's own fields (`self.symbol`, `self.config.symbol`) are its own types, not rows.
        if read.is_empty() || read == "self" || read.starts_with("self.") {
            continue;
        }
        let first = k - read.matches(['.']).count() * 2 - 1;
        let what = if at(&w, k + 2, &[".", "clone", "(", ")"]) {
            format!("`{read}.symbol.clone()`")
        } else if first > 0 && w[first - 1] == "&" && w.get(k + 2) != Some(&".") {
            format!("`&{read}.symbol`")
        } else {
            continue;
        };
        let hint = format!(
            "line {}: {what} is left as it is: if `{read}` is a `Bar`, its `symbol` is an \
             `Option<vike_marketdata::Symbol>` now (write `{read}.symbol.map(String::from)` where an \
             `Option<String>` is needed), and if a `TickTrade` or `TickQuote` a `Symbol` (write \
             `{read}.symbol.to_string()` where a `String` is needed); another type's `symbol` may still \
             be a `String`",
            line(k)
        );
        if !found.hints.contains(&hint) {
            found.hints.push(hint);
        }
    }

    edits.sort_by_key(|(at, _)| (at.start, at.end));
    for (at, new) in edits {
        if found.edits.last().is_none_or(|last| last.at.end <= at.start) {
            found.edits.push(SourceEdit { at, new, hint: EDIT_WORDS.to_string() });
        }
    }
    found
}

/// The builder's line for a source that reads or writes a row's symbol as a `String`: what changed and the
/// command that rewrites it (`super::moved_name`'s `source_hints` prints it after rustc's text).
pub fn symbol_hint(source: &str) -> Option<String> {
    let found = symbol_edits(source);
    (!found.edits.is_empty() || !found.hints.is_empty()).then(|| {
        "a bar's `symbol` is an `Option<vike_marketdata::Symbol>` and a tick's a `Symbol` since the \
         SDK of 2026-11-20, no longer `Option<String>` and `String`: it reads as a `&str` \
         (`.as_deref()`, `==`, `&t.symbol`), and where a `String` is needed write \
         `bar.symbol.map(String::from)` or `t.symbol.to_string()`; `vike-cli strategy migrate <file>` \
         rewrites the common shapes and names the line of any other"
            .to_string()
    })
}

/// The parameters of signature `sig` declared `&Bar`, `&TickTrade` or `&TickQuote` (a path to it, a
/// lifetime and `mut` allowed), with their row.
fn row_parameters<'a>(w: &[&'a str], sig: Range<usize>) -> Vec<(&'a str, Row)> {
    let mut found = Vec::new();
    for k in sig.clone().skip(1) {
        if w[k] != ":" || !is_ident(w[k - 1]) {
            continue;
        }
        let end = (k + 1..sig.end).find(|&j| w[j] == ",").unwrap_or(sig.end);
        let ty: Vec<&str> =
            w[k + 1..end].iter().copied().filter(|t| !t.starts_with('\'') && *t != "mut").collect();
        let Some(path) = ty.strip_prefix(&["&"]) else { continue };
        let a_path =
            path.iter().enumerate().all(|(i, t)| if i % 2 == 0 { is_ident(t) } else { *t == "::" });
        if let (true, Some(row)) = (a_path, path.last().copied().and_then(row_of)) {
            found.push((w[k - 1], row));
        }
    }
    found
}

/// Whether the type name at `k` (followed by `{`) starts a struct literal or pattern rather than a
/// declaration (`struct Bar {`, `impl … for Bar {`, `-> Bar {`) or another type's variant (`Message::Bar {`).
fn a_literal(w: &[&str], k: usize) -> bool {
    let before = k.checked_sub(1).map(|b| w[b]);
    if matches!(before, Some("struct" | "enum" | "union" | "trait" | "impl" | "for" | ">" | "mod"))
    {
        return false;
    }
    before != Some("::") || (k >= 2 && w[k - 2] == "vike_marketdata")
}

/// Whether the braces `k + 1 ..= close` after the row name at `k` are a PATTERN (`let Bar { symbol, .. } =`,
/// a match arm), which no edit may touch.
fn a_pattern(w: &[&str], k: usize, close: usize) -> bool {
    let mut start = k;
    while start >= 2 && w[start - 1] == "::" {
        start -= 2;
    }
    let after = (w.get(close + 1).copied(), w.get(close + 2).copied());
    (start > 0 && w[start - 1] == "let")
        || matches!(after, (Some("="), Some(">")))
        || (after.0 == Some("=") && after.1 != Some("="))
        || (close >= 2 && w[close - 2] == "." && w[close - 1] == ".")
}

/// The index of the `,` or closing `}` that ends the field starting at `field`, inside braces closing at
/// `close`.
fn field_end(w: &[&str], field: usize, close: usize) -> usize {
    let mut k = field;
    while k < close && w[k] != "," {
        k = match w[k] {
            "(" | "{" | "[" => matching(w, k).map_or(close, |m| m + 1),
            _ => k + 1,
        };
    }
    k.min(close)
}

/// `value` (the tokens `range`) as a method call's receiver: itself when it is a plain postfix chain
/// (names, paths, literals, calls, indexing, `?`, a macro's `!`), else in parentheses.
fn receiver(w: &[&str], range: Range<usize>, value: &str) -> String {
    let mut k = range.start;
    let mut plain = is_ident(w[k]) || w[k].starts_with('"');
    while plain && k < range.end {
        let t = w[k];
        k = match t {
            "(" | "[" | "{" => matching(w, k).map_or(range.end, |m| m + 1),
            "." | "::" | "?" | "!" => k + 1,
            _ if matches!(t, "if" | "match" | "else" | "as" | "move" | "loop" | "unsafe") => {
                plain = false;
                k
            }
            _ if is_ident(t) || t.starts_with('"') => k + 1,
            _ => {
                plain = false;
                k
            }
        };
    }
    if plain { value.to_string() } else { format!("({value})") }
}

/// The receiver of the `.` at `dot` as text, `a.b.c` from the identifiers and dots before it; empty when
/// what precedes is no such chain (a call's result, an index).
fn receiver_chain(w: &[&str], dot: usize) -> String {
    let mut k = dot;
    let mut parts = Vec::new();
    loop {
        if k == 0 || !is_ident(w[k - 1]) {
            return String::new();
        }
        parts.push(w[k - 1]);
        if k >= 2 && w[k - 2] == "." {
            k -= 2;
        } else {
            break;
        }
    }
    // A path's tail (`a::b.symbol`) is no receiver this hint can name.
    if k >= 2 && w[k - 2] == "::" {
        return String::new();
    }
    parts.reverse();
    parts.join(".")
}

/// The index of the bracket closing the one at `open` (`(`, `{` and `[` alike), or `None`.
fn matching(w: &[&str], open: usize) -> Option<usize> {
    if !matches!(w.get(open), Some(&("(" | "{" | "["))) {
        return None;
    }
    let mut depth = 0usize;
    for (k, t) in w.iter().enumerate().skip(open) {
        match *t {
            "(" | "{" | "[" => depth += 1,
            ")" | "}" | "]" => {
                depth -= 1;
                if depth == 0 {
                    return Some(k);
                }
            }
            _ => {}
        }
    }
    None
}

/// Whether the tokens from `k` on are `want`.
fn at(w: &[&str], k: usize, want: &[&str]) -> bool {
    w.get(k..k + want.len()) == Some(want)
}

/// Whether a token is an identifier.
fn is_ident(t: &str) -> bool {
    t.starts_with(|c: char| c.is_alphabetic() || c == '_')
}
