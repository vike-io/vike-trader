//! The strategy STATE's signature change of `ABI_VERSION` 15, as text EDITS on a user strategy file,
//! and the builder's one-line hint for a file that still has the old signature.
//!
//! Until 15 a `Strategy` impl wrote `fn save_state(&self) -> Option<serde_json::Value>` and
//! `fn load_state(&mut self, state: &serde_json::Value)`. Since 15 the state is
//! `crate::StrategyState` (msgpack with field names), and a file still on the JSON signature fails its
//! Build with rustc's bare "incompatible type for trait". No path moved, so no SDK step's rename table
//! can carry this (`super::source_edits`); it is a rule of its own. `vike-cli strategy migrate` applies
//! it to every `.rs` file, pinned or not.
//!
//! What is EDITED, inside an `impl … Strategy<…> for …` block only (an inherent helper of the same name
//! is the file's own), is the shapes the two shipped makers had and the idiom they moved to:
//! - the signature's `serde_json::Value` (any path ending in `Value`) becomes `vike_model::StrategyState`;
//! - in `save_state`, `serde_json::to_value(X).ok()` becomes `vike_model::StrategyState::of(X).ok()`, and
//!   `Some(serde_json::json!(…))` becomes `Some(vike_model::StrategyState::from_json_value(&…))`;
//! - in `load_state`, `serde_json::from_value(state.clone())` becomes `state.read()` (a turbofish kept).
//!
//! A body is edited only WHOLE: a `save_state` whose every `serde_json`, `json`, `to_value` and `Value`
//! token is inside one of those shapes, and a `load_state` whose every use of its parameter is (or which
//! never names it). Any other body is left as it is and draws a HINT naming its line and the new
//! signature, so the file still fails its build there instead of building on a guess.
//!
//! ⚠ A text walk is not a compiler (`super::source_edits`'s module doc): every caller builds the result
//! before it keeps it, so a wrong edit is a build error, never a silent change. A rewritten signature
//! no longer names `Value`, so a second pass finds nothing: the rule is idempotent by construction.
//!
//! Pure: no file, no environment and no clock.

use std::ops::Range;

use super::moved_name;
use super::source_edits::{SourceEdit, SourceEdits};

/// The state's type since `ABI_VERSION` 15, as a file names it.
const STATE_TYPE: &str = "vike_model::StrategyState";

/// The two signatures since 15, as a file writes them.
const NEW_SAVE: &str = "fn save_state(&self) -> Option<vike_model::StrategyState>";
const NEW_LOAD: &str = "fn load_state(&mut self, state: &vike_model::StrategyState)";

/// Every edit's one line.
const EDIT_WORDS: &str = "the strategy state is `vike_model::StrategyState` since ABI 15";

/// The edits that move `source`'s `Strategy` impls onto the state signatures of 15, sorted, and a hint
/// per hook whose body is no shape the module doc names (that hook is then not edited at all).
pub fn state_edits(source: &str) -> SourceEdits {
    let (_, spans) = moved_name::lex(source);
    let w: Vec<&str> = spans.iter().map(|s| &source[s.clone()]).collect();
    let mut found = SourceEdits::default();
    for (open, close) in strategy_impls(&w) {
        for f in open..close {
            let hook = match (w[f], w.get(f + 1)) {
                ("fn", Some(&name)) if name == "save_state" || name == "load_state" => name,
                _ => continue,
            };
            let Some(body) = (f..close).find(|&k| w[k] == "{") else { continue };
            let Some(end) = matching(&w, body) else { continue };
            // No `Value` in the signature: 15's already (or nothing this rule knows).
            let Some(ty) = old_type(&w, f + 2..body) else { continue };
            let inner = body + 1..end;
            let edits = if hook == "save_state" {
                save_body(&w, &spans, inner)
            } else {
                parameter(&w, f + 2..body).and_then(|p| load_body(source, &w, &spans, inner, p))
            };
            let Some(edits) = edits else {
                let line = source[..spans[f].start].matches('\n').count() + 1;
                found.hints.push(hint(line, hook));
                continue;
            };
            let signature = (spans[ty.start].start..spans[ty.end - 1].end, STATE_TYPE.to_string());
            found.edits.extend(
                std::iter::once(signature).chain(edits).map(|(at, new)| SourceEdit {
                    at,
                    new,
                    hint: EDIT_WORDS.to_string(),
                }),
            );
        }
    }
    found.edits.sort_by_key(|e| (e.at.start, e.at.end));
    found
}

/// The builder's line for a source whose `Strategy` impl still has a JSON state signature: what changed
/// and the command that rewrites it (`super::moved_name`'s `source_hints` prints it after rustc's text).
pub fn state_hint(source: &str) -> Option<String> {
    let found = state_edits(source);
    (!found.edits.is_empty() || !found.hints.is_empty()).then(|| {
        format!(
            "`save_state` and `load_state` carry the strategy state as `{STATE_TYPE}` (msgpack) \
             since ABI 15, no longer `serde_json::Value`: `vike-cli strategy migrate <file>` \
             rewrites the common shapes and names the line of any other, or write `{NEW_SAVE}` and \
             `{NEW_LOAD}` by hand"
        )
    })
}

/// The hint for hook `hook` declared on line `line`, whose body no edit carries.
fn hint(line: usize, hook: &str) -> String {
    if hook == "save_state" {
        format!(
            "line {line}: `save_state` returns `Option<{STATE_TYPE}>` since ABI 15, no longer \
             `Option<serde_json::Value>`, and migrate rewrites no body of this shape: write \
             `{NEW_SAVE}` and build the state with `{STATE_TYPE}::of(&your_state).ok()`"
        )
    } else {
        format!(
            "line {line}: `load_state` takes `&{STATE_TYPE}` since ABI 15, no longer \
             `&serde_json::Value`, and migrate rewrites no body of this shape: write `{NEW_LOAD}` \
             and read it with `state.read::<YourState>()`, or `state.to_json_value()` for the JSON \
             document"
        )
    }
}

/// `(open, close)` token indices of the braces of every `impl … Strategy<…> for … { … }` block.
fn strategy_impls(w: &[&str]) -> Vec<(usize, usize)> {
    let mut found = Vec::new();
    let mut k = 0;
    while k < w.len() {
        if w[k] == "impl"
            && let Some(open) = (k..w.len()).find(|&o| w[o] == "{")
            && w[k..open].windows(2).any(|pair| pair == ["Strategy", "<"])
            && w[k..open].contains(&"for")
            && let Some(close) = matching(w, open)
        {
            found.push((open, close));
            k = close;
        }
        k += 1;
    }
    found
}

/// The index of the bracket closing the one at `open`, counting `(`, `{` and `[` alike; `None` when
/// `open` is no opening bracket or nothing closes it.
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

/// The token range of the path ending in `Value` in signature `sig` (`serde_json::Value`,
/// `::serde_json::Value`, a bare `Value`), or `None` when the signature names none.
fn old_type(w: &[&str], sig: Range<usize>) -> Option<Range<usize>> {
    let v = sig.clone().find(|&k| w[k] == "Value")?;
    let mut start = v;
    while start >= sig.start + 2 && w[start - 1] == "::" && is_ident(w[start - 2]) {
        start -= 2;
    }
    if start > sig.start && w[start - 1] == "::" {
        start -= 1;
    }
    Some(start..v + 1)
}

/// `load_state`'s parameter name, the plain identifier after `self,` in signature `sig`.
fn parameter<'a>(w: &[&'a str], mut sig: Range<usize>) -> Option<&'a str> {
    let s = sig.find(|&k| w[k] == "self")?;
    match *w.get(s + 1..s + 4)? {
        [",", name, ":"] if is_ident(name) && name != "mut" => Some(name),
        _ => None,
    }
}

/// `save_state`'s body `inner` as byte edits, or `None` when it is no shape the module doc names.
fn save_body(
    w: &[&str],
    spans: &[Range<usize>],
    inner: Range<usize>,
) -> Option<Vec<(Range<usize>, String)>> {
    let mut edits = Vec::new();
    let mut i = inner.start;
    while i < inner.end {
        let path = i + usize::from(w[i] == "::");
        // `serde_json::to_value(X).ok()`
        if at(w, path, &["serde_json", "::", "to_value", "("])
            && let Some(e) = matching(w, path + 3)
            && at(w, e + 1, &[".", "ok", "(", ")"])
        {
            edits.push((spans[i].start..spans[path + 2].end, format!("{STATE_TYPE}::of")));
            i = e + 5;
            continue;
        }
        // `Some(serde_json::json!(…))`, the path optional
        if w[i] == "Some" && w.get(i + 1) == Some(&"(") {
            let mut m = i + 2;
            m += usize::from(w.get(m) == Some(&"::"));
            if at(w, m, &["serde_json", "::"]) {
                m += 2;
            }
            if at(w, m, &["json", "!"])
                && let Some(e) = matching(w, m + 2)
                && w.get(e + 1) == Some(&")")
            {
                let from = spans[i + 2].start;
                edits.push((from..from, format!("{STATE_TYPE}::from_json_value(&")));
                edits.push((spans[e].end..spans[e].end, ")".to_string()));
                i = e + 2;
                continue;
            }
        }
        if matches!(w[i], "serde_json" | "json" | "to_value" | "Value") {
            return None;
        }
        i += 1;
    }
    (!edits.is_empty()).then_some(edits)
}

/// `load_state`'s body `inner`, whose parameter is `p`, as byte edits (none when it never names `p`),
/// or `None` when it is no shape the module doc names.
fn load_body(
    source: &str,
    w: &[&str],
    spans: &[Range<usize>],
    inner: Range<usize>,
    p: &str,
) -> Option<Vec<(Range<usize>, String)>> {
    let mut edits = Vec::new();
    if p == "_" {
        return Some(edits);
    }
    let mut i = inner.start;
    while i < inner.end {
        let path = i + usize::from(w[i] == "::");
        if at(w, path, &["serde_json", "::", "from_value"]) {
            let mut k = path + 3;
            // `::<T>`, kept as the file wrote it
            let mut turbofish = "";
            if at(w, k, &["::", "<"]) {
                let mut depth = 0usize;
                let close = (k + 1..inner.end).find(|&g| {
                    match w[g] {
                        "<" => depth += 1,
                        ">" => depth -= 1,
                        _ => {}
                    }
                    depth == 0
                })?;
                turbofish = &source[spans[k].start..spans[close].end];
                k = close + 1;
            }
            if let Some(&["(", name, ".", "clone" | "to_owned", "(", ")", ")"]) = w.get(k..k + 7)
                && name == p
            {
                edits.push((spans[i].start..spans[k + 6].end, format!("{p}.read{turbofish}()")));
                i = k + 7;
                continue;
            }
        }
        let field_or_path = i > 0 && matches!(w[i - 1], "." | "::");
        if w[i] == p && !field_or_path {
            return None;
        }
        i += 1;
    }
    Some(edits)
}

/// Whether the tokens from `k` on are `want`.
fn at(w: &[&str], k: usize, want: &[&str]) -> bool {
    w.get(k..k + want.len()) == Some(want)
}

/// Whether a token is an identifier (a keyword too: the callers only skip over paths).
fn is_ident(t: &str) -> bool {
    t.starts_with(|c: char| c.is_alphabetic() || c == '_')
}
