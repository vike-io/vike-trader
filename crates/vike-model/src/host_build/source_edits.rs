//! The exact text EDITS that carry a user strategy file across a rename table: the file's own text
//! in, `(byte range, new text)` pairs out, plus a one-line hint for each thing no edit can carry.
//!
//! A rename table (one per SDK step, `N` to `N + 1`, decision 0296) is rows of [`SdkRename`]: the
//! item a file reached as `old` (a whole path, `a_crate::module::Name`) is `new` now, or it is gone
//! and the row's hint says what replaces it. [`builtin_renames`] is the same table for the names
//! that moved or were renamed BEFORE the first SDK: `super::moved_name`'s tables, as data.
//!
//! What is EDITED, read off `super::moved_name`'s lexer (comments and literals blanked, so neither
//! supplies a path):
//! - a path to `old`, inline or in a `use`, becomes `new` whole (an `as` rename after it is kept);
//! - an item of a `use old_root::{…}` group becomes the new name, when the row keeps the root;
//! - every bare use of the old name, in a file that imported it UNDER that name by one of the two.
//!
//! What is left as a HINT: a group item whose root changes (the group's other names stay where
//! they are), a row with no `new`, and whatever a glob import reaches (`use a_crate::*;` names
//! nothing, so nothing is known to come from it).
//!
//! ⚠ A text walk is not a compiler. A bare name the file also gave to a local item of its own is
//! renamed with the import, and a field read (`x.name`) is not. Every caller BUILDS the result
//! before it keeps it (`vike-cli strategy migrate`'s step gate, Studio's Build), so a wrong edit is
//! a build error on its line, never a silent change.
//!
//! # A `.rhai` file ([`rhai_edits`])
//!
//! A Rhai script pins no SDK and runs on this vike's own code, so nothing records which tables it
//! has had. Its edits are therefore IDEMPOTENT by rule instead of by bookkeeping: an old name is
//! rewritten only while it RESOLVES TO NOTHING, neither bound by this vike's engine (the caller's
//! `binds`) nor defined by the script itself (`fn old(…)`). Every table may be applied to every
//! script, as often as it is run: a rewritten name is gone the second time, a chain (`a` to `b`, then
//! `b` to `c`) lands on `c`, and a word a later SDK gave back to a new item is left alone. Which
//! tokens a row reaches follows its kind ([`RhaiRename::kind`]). ⚠ The rule holds only where
//! `binds` can answer, for a function and an indicator: no list says whether this vike still has a
//! property or a type, so a `get`, `set` or `type` row is NEVER an edit (it would rewrite the
//! script's own map field or variable of that name too), only a hint per place, with its line.
//!
//! Pure: no file, no environment and no clock.

use std::ops::Range;

use super::moved_name::{
    self, LANGUAGE_ROOT, MARKETDATA_ROOT, MOVED_NAMES, OLD_COMMISSIONS, OLD_ROOT, OLD_SLIPPAGES,
    RENAMED_FIELDS, RENAMED_MARKETDATA_NAMES, RENAMED_NAMES, Tok,
};

/// One `rhai` row of a rename table: a bare name a `.rhai` file writes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RhaiRename {
    /// What the name is, and so which tokens are it: `get`/`set` a property read after a `.`;
    /// `type` the name anywhere but after a `.`; `indicator` a call of it or of one of its per-line
    /// accessors (`<name>_<line>(…)`, which move with it); `fn` and any other kind a call (`old(…)`
    /// or `x.old(…)`).
    pub kind: String,
    /// The name as the older vike bound it.
    pub old: String,
    /// The name now, or `None` when it is gone.
    pub new: Option<String>,
}

/// The edits and hints the `rhai` rows `renames` draw on a script `source`, in table order (the
/// module doc's idempotence rule, `binds` saying whether this vike's engine binds a name).
pub fn rhai_edits(
    source: &str,
    renames: &[RhaiRename],
    binds: &dyn Fn(&str) -> bool,
) -> SourceEdits {
    let (toks, spans) = moved_name::lex(source);
    let defined: Vec<&str> = toks
        .windows(2)
        .filter_map(|w| match w {
            [Tok::Ident("fn"), Tok::Ident(name)] => Some(*name),
            _ => None,
        })
        .collect();
    let resolves = |name: &str| binds(name) || defined.contains(&name);
    let mut found = SourceEdits::default();
    let mut edits = Vec::new();
    for row in renames {
        let mut reached = false;
        for (k, tok) in toks.iter().enumerate() {
            let Tok::Ident(name) = *tok else { continue };
            let after_dot = k > 0 && toks[k - 1] == Tok::Dot;
            let called = toks.get(k + 1) == Some(&Tok::Paren);
            // The name this token is, and the part of it the row renames (an accessor keeps its line).
            let tail = match row.kind.as_str() {
                "get" | "set" if after_dot && !called && name == row.old => "",
                "type" if !after_dot && name == row.old => "",
                "indicator" if called && !after_dot => match name.strip_prefix(row.old.as_str()) {
                    Some(tail) if tail.is_empty() || tail.starts_with('_') => tail,
                    _ => continue,
                },
                "get" | "set" | "type" | "indicator" => continue,
                _ if called && name == row.old => "",
                _ => continue,
            };
            // A property or a type is never edited: no list says whether this vike still has one,
            // so `resolves` cannot judge it (a `strategy` verb may share its word), and the script's
            // own map field or variable of that name would be rewritten with it. Each place is a
            // hint, on its line, for the user to judge.
            if matches!(row.kind.as_str(), "get" | "set" | "type") {
                let line = source[..spans[k].start].matches('\n').count() + 1;
                let now = row
                    .new
                    .as_deref()
                    .map_or_else(|| "is gone".to_string(), |new| format!("is now `{new}`"));
                let hint = format!(
                    "line {line}: `{}` (a {}) {now} in this vike: rewrite it by hand where it \
                     is vike's, not where it is the script's own",
                    row.old, row.kind
                );
                if !found.hints.contains(&hint) {
                    found.hints.push(hint);
                }
                continue;
            }
            if resolves(name) {
                continue;
            }
            reached = true;
            if let Some(new) = &row.new {
                let hint = format!("`{}` is now `{new}`", row.old);
                edits.push(SourceEdit { at: spans[k].clone(), new: format!("{new}{tail}"), hint });
            }
        }
        if reached && row.new.is_none() {
            let hint = format!("`{}` is gone in this vike: rewrite what calls it by hand", row.old);
            if !found.hints.contains(&hint) {
                found.hints.push(hint);
            }
        }
    }
    edits.sort_by_key(|e| (e.at.start, e.at.end));
    for e in edits {
        // The first row to reach a token wins; a token is never edited twice in one pass.
        if found.edits.last().is_none_or(|last| last.at.end <= e.at.start) {
            found.edits.push(e);
        }
    }
    found
}

/// The `rhai` rows of the names renamed before the first SDK: the FIELDS phase 4 spelled out on
/// vike's types (`super::moved_name`'s `RENAMED_FIELDS`), which a script reads as properties, so
/// each draws hints, never an edit ([`rhai_edits`]).
pub fn builtin_rhai_renames() -> Vec<RhaiRename> {
    RENAMED_FIELDS
        .iter()
        .map(|(old, new)| RhaiRename {
            kind: "get".to_string(),
            old: (*old).to_string(),
            new: Some((*new).to_string()),
        })
        .collect()
}

/// One row of a rename table: what a file reached as `old` in the older SDK.
///
/// ⚠ This is the reader's OWN shape. The release side (`just sdk-cut` and the CI report) writes the
/// table each SDK step ships; whichever format it writes is read INTO this, so the walk below has one
/// input whatever the file looks like.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SdkRename {
    /// The whole path the item was reached through (`a_crate::module::Name`).
    pub old: String,
    /// Its whole path now, or `None` when it is gone.
    pub new: Option<String>,
    /// What to tell the user, one line. `None`: "`old` is now `new`" (or "is gone").
    pub hint: Option<String>,
}

impl SdkRename {
    /// The row's one line for the user.
    fn words(&self) -> String {
        match (&self.hint, &self.new) {
            (Some(hint), _) => hint.clone(),
            (None, Some(new)) => format!("`{}` is now `{new}`", self.old),
            (None, None) => format!("`{}` is gone in this SDK", self.old),
        }
    }
}

/// One exact edit: the bytes `at` of the source become `new`; `hint` is the row's line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceEdit {
    /// The byte range of the source this edit replaces (empty: an insertion).
    pub at: Range<usize>,
    /// The text that replaces it.
    pub new: String,
    /// Why, in one line (the rename row's words).
    pub hint: String,
}

/// What [`source_edits`] found: the edits, sorted by position and never overlapping, and the
/// hints for what the file reaches that no edit carries.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SourceEdits {
    /// The edits [`apply_edits`] applies, in source order.
    pub edits: Vec<SourceEdit>,
    /// One line each, in table order, each once.
    pub hints: Vec<String>,
}

/// The edits and hints `renames` draws on `source` (the module doc says which are which).
pub fn source_edits(source: &str, renames: &[SdkRename]) -> SourceEdits {
    let (toks, spans) = moved_name::lex(source);
    let mut found = SourceEdits::default();
    let mut edits: Vec<SourceEdit> = Vec::new();
    for row in renames {
        let Some((old_root, old_name)) = row.old.rsplit_once("::") else { continue };
        let old_segs: Vec<&str> = old_root.split("::").collect();
        let new_path = row.new.as_deref().filter(|new| *new != row.old);
        let new_name = new_path.map(|new| new.rsplit_once("::").map_or(new, |(_, name)| name));
        let keeps_root =
            new_path.and_then(|new| new.rsplit_once("::")).map(|(root, _)| root) == Some(old_root);
        let (mut reached, mut grouped_away, mut imported) = (false, false, false);
        let mut edit = |at: Range<usize>, new: &str| {
            edits.push(SourceEdit { at, new: new.to_string(), hint: row.words() })
        };
        for k in 0..toks.len() {
            let Some(after) = moved_name::path_after(&toks, k, &old_segs) else { continue };
            match toks.get(after) {
                Some(Tok::Ident(name)) if *name == old_name => {
                    reached = true;
                    if let Some(new) = new_path {
                        edit(spans[k].start..spans[after].end, new);
                        imported |= in_use(&toks, k) && !aliased(&toks, after);
                    }
                }
                Some(Tok::Open) => {
                    let mut items = Vec::new();
                    group_names(&toks, after + 1, &mut items);
                    for item in items.into_iter().filter(|&i| toks[i] == Tok::Ident(old_name)) {
                        reached = true;
                        match new_name {
                            Some(name) if keeps_root => {
                                edit(spans[item].clone(), name);
                                imported |= !aliased(&toks, item);
                            }
                            Some(_) => grouped_away = true,
                            None => {}
                        }
                    }
                }
                _ => {}
            }
        }
        if let Some(name) = new_name.filter(|name| imported && *name != old_name) {
            for (j, tok) in toks.iter().enumerate() {
                let follows_a_path_or_a_dot =
                    j > 0 && matches!(toks[j - 1], Tok::PathSep | Tok::Dot);
                if *tok == Tok::Ident(old_name) && !follows_a_path_or_a_dot {
                    edit(spans[j].clone(), name);
                }
            }
        }
        let hint = match (reached, new_path) {
            (true, None) => Some(row.words()),
            (true, Some(new)) if grouped_away => Some(format!(
                "{}: move `{old_name}` out of its `use {old_root}::{{…}}` group into `use {new};`",
                row.words()
            )),
            _ => None,
        };
        if let Some(hint) = hint.filter(|hint| !found.hints.contains(hint)) {
            found.hints.push(hint);
        }
    }
    edits.sort_by_key(|e| (e.at.start, e.at.end));
    for e in edits {
        // The first edit of a span wins: a bare-use pass meets a group item it already renamed.
        if found.edits.last().is_none_or(|last| last.at.end <= e.at.start) {
            found.edits.push(e);
        }
    }
    found
}

/// `source` with `edits` applied: sorted and non-overlapping, as [`source_edits`] returns them.
pub fn apply_edits(source: &str, edits: &[SourceEdit]) -> String {
    let mut out = String::with_capacity(source.len());
    let mut at = 0;
    for e in edits {
        out.push_str(&source[at..e.at.start]);
        out.push_str(&e.new);
        at = e.at.end;
    }
    out.push_str(&source[at..]);
    out
}

/// The names that moved or were renamed before the first SDK (`super::moved_name`'s
/// `RENAMED_NAMES`, `MOVED_NAMES`, `RENAMED_MARKETDATA_NAMES` and the order language's old cost
/// names) as one rename table. The cost names are hints only: their variants became constructors,
/// which no path rename carries. The renamed FIELDS are not rows (no path reaches a field), so
/// they stay `super::moved_name::renamed_field_hint`'s.
///
/// ⚠ For a file that does NOT build: one that builds on an SDK reaches none of these names, and
/// fed one, a word a later SDK gave back to a NEW item would be renamed away.
pub fn builtin_renames() -> Vec<SdkRename> {
    let row = |old: String, new: Option<String>, hint: Option<String>| SdkRename { old, new, hint };
    let mut rows: Vec<SdkRename> = RENAMED_NAMES
        .iter()
        .map(|(root, old, new)| {
            let new =
                if new.contains("::") { (*new).to_string() } else { format!("{root}::{new}") };
            row(format!("{root}::{old}"), Some(new), None)
        })
        .collect();
    rows.extend(MOVED_NAMES.into_iter().map(|name| {
        let now = moved_name::current_marketdata_name(name);
        row(format!("{OLD_ROOT}::{name}"), Some(format!("{MARKETDATA_ROOT}::{now}")), None)
    }));
    rows.extend(RENAMED_MARKETDATA_NAMES.iter().map(|(old, new)| {
        row(format!("{MARKETDATA_ROOT}::{old}"), Some(format!("{MARKETDATA_ROOT}::{new}")), None)
    }));
    rows.extend(OLD_COMMISSIONS.iter().map(|old| {
        row(format!("{LANGUAGE_ROOT}::{old}"), None, Some(moved_name::commission_words(old)))
    }));
    rows.extend(OLD_SLIPPAGES.iter().map(|old| {
        row(format!("{LANGUAGE_ROOT}::{old}"), None, Some(moved_name::slippage_words(old)))
    }));
    rows
}

/// Whether the path starting at token `k` is a `use` item's (`use a::b`, `use ::a::b`).
fn in_use(toks: &[Tok<'_>], k: usize) -> bool {
    let at = |back: usize| k.checked_sub(back).and_then(|i| toks.get(i));
    at(1) == Some(&Tok::Ident("use"))
        || (at(1) == Some(&Tok::PathSep) && at(2) == Some(&Tok::Ident("use")))
}

/// Whether the name at token `i` is imported under another (`Name as Other`).
fn aliased(toks: &[Tok<'_>], i: usize) -> bool {
    toks.get(i + 1) == Some(&Tok::Ident("as"))
}

/// The token indices of the items of a `{ .. }` group whose `{` is just before `i` that ARE names,
/// nested groups walked as the same root (`super::moved_name`'s `group`, keeping indices). Returns
/// the index after the group's `}`.
fn group_names(toks: &[Tok<'_>], mut i: usize, out: &mut Vec<usize>) -> usize {
    loop {
        match toks.get(i) {
            None => return i,
            Some(Tok::Close) => return i + 1,
            Some(Tok::Comma) => {
                i += 1;
                continue;
            }
            Some(Tok::Open) => i = group_names(toks, i + 1, out),
            Some(Tok::Ident(_)) if toks.get(i + 1) != Some(&Tok::PathSep) => {
                out.push(i);
                i += 1;
            }
            Some(_) => {}
        }
        i = moved_name::item_end(toks, i);
    }
}
