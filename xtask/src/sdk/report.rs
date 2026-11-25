//! The SDK REPORT: what a strategy file can see change between two trees, the newest `sdk-*` tag and
//! HEAD (`docs/ops/sdk-release.md`). It decides nothing; CI prints it and a cut's notes quote it.
//!
//! **Rust**, over the public items ([`super::items`]) of the strategy template's crates
//! ([`sdk_crates`]: its `path` closure, without [`super::BOUNDARY_CRATE_DIR`], the host's boundary):
//! - BREAKS THE BUILD: every public item REMOVED, and every one RENAMED, read from the pair
//!   ([`rust_renames`]): the same kind and name in another module or crate (a move), the same text under
//!   another name (a rename), or a member of a renamed type;
//! - ADDED: every public item the older tree lacks;
//! - MAY CHANGE RESULTS SILENTLY: every item, at any visibility, of [`SILENT_CHANGE_CRATES`] present in
//!   both trees whose text moved (comments and layout aside): a changed body, default or field.
//!
//! **Rhai**, which has no SDK: a `.rhai` file runs on the host's own code, so it meets every change at
//! the next deploy. The names the engine registers ([`rhai_names`]: `vike-rhai`'s `register_*` calls,
//! the macros wrapping one, and the indicator registry's rows) REMOVED or RENAMED fail at run time, and
//! the [`SILENT_CHANGE_CRATES`] items whose text moved and that a registration reaches ([`rhai_reach`], by
//! name: an over-approximation, the safe side) may change its results silently.
//!
//! Every rename and removal of both goes into ONE [`RenameTable`], a row per name.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

use super::items::{Item, crate_items, tokens};
use super::rename_table::{RenameRow, RenameTable};

/// The crates whose changed code may move a strategy's results with no compile error: the indicators,
/// the order language and the maths under both.
pub const SILENT_CHANGE_CRATES: [&str; 3] = ["vike-indicators", "vike-tactics", "vike-math"];

/// The crate that registers what a `.rhai` file can call.
pub const RHAI_CRATE_DIR: &str = "crates/vike-rhai";

/// The vike-indicators module whose `IndicatorMeta` rows name every indicator a `.rhai` file calls.
pub const INDICATOR_REGISTRY_MODULE: &str = "registry";

/// The `rhai::Engine` calls whose first argument is a name a script writes, with the kind of that name.
const REGISTRATIONS: [(&str, &str); 6] = [
    ("register_fn", "fn"),
    ("register_type_with_name", "type"),
    ("register_get", "get"),
    ("register_set", "set"),
    ("register_get_set", "get"),
    ("register_raw_fn", "fn"),
];

/// How long a list in the markdown gets before it is cut short (the rename table has every row).
const SHOWN: usize = 100;

/// One tree's side of the report.
pub struct Tree {
    /// What the report calls it: a tag, or a commit.
    pub label: String,
    /// [`sdk_crates`], root-relative folders.
    pub crates: Vec<String>,
    /// Every item of those crates.
    pub items: Vec<Item>,
    /// Every name the Rhai engine registers ([`rhai_names`]).
    pub rhai: Vec<RhaiName>,
    /// The items of [`SILENT_CHANGE_CRATES`] a registration reaches ([`rhai_reach`]), by path.
    pub rhai_reach: BTreeSet<String>,
}

/// A name a `.rhai` file can write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RhaiName {
    /// `fn`, `type`, `get`, `set` or `indicator`.
    pub kind: &'static str,
    pub name: String,
    /// What stays when the name is renamed: the registration's call with the name written `$` (every
    /// overload's, joined), or an indicator row's implementing type.
    pub identity: String,
}

/// The report of one pair of trees ([`compare`]).
pub struct SdkReport {
    pub from: String,
    pub to: String,
    pub crates: Vec<String>,
    pub added: Vec<Item>,
    pub removed: Vec<Item>,
    pub renamed: Vec<(Item, Item)>,
    pub silent: Vec<Item>,
    pub rhai_added: Vec<RhaiName>,
    pub rhai_removed: Vec<RhaiName>,
    pub rhai_renamed: Vec<(RhaiName, RhaiName)>,
    pub rhai_silent: Vec<Item>,
}

/// The strategy template's crates in the tree at `root`: every crate its manifest's `path` closure
/// reaches (`crates/vike-rust-strategy/build/source_hash.rs`'s `closure`, the build folder's own walk),
/// without the boundary crate. Empty when the template cannot be read.
pub fn sdk_crates(root: &Path) -> Vec<String> {
    let template = root
        .join(super::BOUNDARY_CRATE_DIR)
        .join(crate::ci::source_hash::STRATEGY_TEMPLATE_MANIFEST);
    let Ok(template) = fs::read_to_string(template) else { return Vec::new() };
    crate::ci::source_hash::closure(root, &[template.as_str()])
        .into_iter()
        .filter(|krate| krate != super::BOUNDARY_CRATE_DIR)
        .collect()
}

fn crate_name(dir: &str) -> &str {
    dir.rsplit('/').next().unwrap_or(dir)
}

/// Read the tree at `root`, called `label`.
pub fn read_tree(root: &Path, label: &str) -> Tree {
    let crates = sdk_crates(root);
    let items: Vec<Item> =
        crates.iter().flat_map(|dir| crate_items(&root.join(dir), crate_name(dir))).collect();
    let rhai_code = crate_items(&root.join(RHAI_CRATE_DIR), crate_name(RHAI_CRATE_DIR));
    let registry: Vec<&Item> = items
        .iter()
        .filter(|i| i.krate == "vike_indicators" && in_module(&i.module, INDICATOR_REGISTRY_MODULE))
        .collect();
    let rhai = rhai_names(&rhai_code, &registry);
    let rhai_reach = rhai_reach(&rhai_code, &items);
    Tree { label: label.to_string(), crates, items, rhai, rhai_reach }
}

fn in_module(module: &str, top: &str) -> bool {
    module == top || module.starts_with(&format!("{top}::"))
}

fn key(item: &Item) -> (String, &'static str) {
    (item.path(), item.kind)
}

/// An item of [`SILENT_CHANGE_CRATES`] that holds code: an import (`use`) runs nothing itself.
fn silent_crate(item: &Item) -> bool {
    item.kind != "use" && SILENT_CHANGE_CRATES.iter().any(|c| c.replace('-', "_") == item.krate)
}

/// The report from `old` to `new`.
pub fn compare(old: &Tree, new: &Tree) -> SdkReport {
    let api = |tree: &Tree| -> BTreeMap<(String, &'static str), Item> {
        tree.items
            .iter()
            .filter(|i| i.public && i.kind != "impl")
            .map(|i| (key(i), i.clone()))
            .collect()
    };
    let (old_api, new_api) = (api(old), api(new));
    let removed: Vec<&Item> =
        old_api.iter().filter(|(k, _)| !new_api.contains_key(*k)).map(|(_, i)| i).collect();
    let added: Vec<&Item> =
        new_api.iter().filter(|(k, _)| !old_api.contains_key(*k)).map(|(_, i)| i).collect();
    let renamed = rust_renames(&removed, &added);
    let matched_old: BTreeSet<_> = renamed.iter().map(|(o, _)| key(o)).collect();
    let matched_new: BTreeSet<_> = renamed.iter().map(|(_, n)| key(n)).collect();

    let old_all: BTreeMap<_, &Item> =
        old.items.iter().filter(|i| silent_crate(i)).map(|i| (key(i), i)).collect();
    let silent: Vec<Item> = new
        .items
        .iter()
        .filter(|i| silent_crate(i))
        .filter(|i| old_all.get(&key(i)).is_some_and(|o| o.text != i.text))
        .cloned()
        .collect();

    let rhai_map = |tree: &Tree| -> BTreeMap<(&'static str, String), RhaiName> {
        tree.rhai.iter().map(|r| ((r.kind, r.name.clone()), r.clone())).collect()
    };
    let (old_rhai, new_rhai) = (rhai_map(old), rhai_map(new));
    let rhai_removed: Vec<&RhaiName> =
        old_rhai.iter().filter(|(k, _)| !new_rhai.contains_key(*k)).map(|(_, r)| r).collect();
    let rhai_added: Vec<&RhaiName> =
        new_rhai.iter().filter(|(k, _)| !old_rhai.contains_key(*k)).map(|(_, r)| r).collect();
    let rhai_renamed: Vec<(RhaiName, RhaiName)> =
        unique_pairs(&rhai_removed, &rhai_added, rhai_identity, rhai_identity)
            .into_iter()
            .map(|(o, n)| (o.clone(), n.clone()))
            .collect();
    let rhai_old: BTreeSet<_> =
        rhai_renamed.iter().map(|(o, _)| (o.kind, o.name.clone())).collect();
    let rhai_new: BTreeSet<_> =
        rhai_renamed.iter().map(|(_, n)| (n.kind, n.name.clone())).collect();
    let rhai_silent =
        silent.iter().filter(|i| new.rhai_reach.contains(&i.path())).cloned().collect();

    SdkReport {
        from: old.label.clone(),
        to: new.label.clone(),
        crates: new.crates.iter().map(|d| crate_name(d).to_string()).collect(),
        added: added.into_iter().filter(|i| !matched_new.contains(&key(i))).cloned().collect(),
        removed: removed.into_iter().filter(|i| !matched_old.contains(&key(i))).cloned().collect(),
        renamed,
        silent,
        rhai_added: rhai_added
            .into_iter()
            .filter(|r| !rhai_new.contains(&(r.kind, r.name.clone())))
            .cloned()
            .collect(),
        rhai_removed: rhai_removed
            .into_iter()
            .filter(|r| !rhai_old.contains(&(r.kind, r.name.clone())))
            .cloned()
            .collect(),
        rhai_renamed,
        rhai_silent,
    }
}

fn rhai_identity(r: &RhaiName) -> String {
    format!("{} {}", r.kind, r.identity)
}

/// The pairs whose keys occur exactly once on each side.
fn unique_pairs<'a, T>(
    removed: &[&'a T],
    added: &[&'a T],
    old_key: impl Fn(&T) -> String,
    new_key: impl Fn(&T) -> String,
) -> Vec<(&'a T, &'a T)> {
    let count = |side: &[&'a T], f: &dyn Fn(&T) -> String| {
        let mut by: BTreeMap<String, Vec<&'a T>> = BTreeMap::new();
        for item in side {
            by.entry(f(item)).or_default().push(item);
        }
        by
    };
    let (old, new) = (count(removed, &old_key), count(added, &new_key));
    old.iter()
        .filter_map(|(k, o)| match (o.as_slice(), new.get(k).map(Vec::as_slice)) {
            ([o], Some([n])) => Some((*o, *n)),
            _ => None,
        })
        .collect()
}

/// The renames among the REMOVED and ADDED public items, in three passes, each matching a pair only
/// when its key is unique on both sides: (1) a MOVE, the same kind, owner and name in another module or
/// crate; (2) a RENAME, the same kind and text but for the name; (3) a member of a type (1) or (2)
/// renamed, the same kind and name under the new owner.
pub fn rust_renames(removed: &[&Item], added: &[&Item]) -> Vec<(Item, Item)> {
    let mut pairs: Vec<(Item, Item)> = Vec::new();
    let left = |pairs: &Vec<(Item, Item)>| -> (Vec<&Item>, Vec<&Item>) {
        let o: BTreeSet<_> = pairs.iter().map(|(o, _)| key(o)).collect();
        let n: BTreeSet<_> = pairs.iter().map(|(_, n)| key(n)).collect();
        (
            removed.iter().copied().filter(|i| !o.contains(&key(i))).collect(),
            added.iter().copied().filter(|i| !n.contains(&key(i))).collect(),
        )
    };
    let by_name = |i: &Item| format!("{} {} {}", i.kind, i.owner, i.name);
    let (r, a) = left(&pairs);
    pairs.extend(
        unique_pairs(&r, &a, by_name, by_name).into_iter().map(|(o, n)| (o.clone(), n.clone())),
    );
    let by_text = |i: &Item| format!("{} {}", i.kind, i.normalized());
    let (r, a) = left(&pairs);
    pairs.extend(
        unique_pairs(&r, &a, by_text, by_text).into_iter().map(|(o, n)| (o.clone(), n.clone())),
    );
    let owners: BTreeMap<String, String> = pairs
        .iter()
        .filter(|(o, n)| {
            o.owner.is_empty()
                && matches!(o.kind, "struct" | "enum" | "union" | "trait" | "type")
                && o.name != n.name
        })
        .map(|(o, n)| (o.name.clone(), n.name.clone()))
        .collect();
    let (r, a) = left(&pairs);
    let renamed_owner = |i: &Item| {
        let owner =
            owners.get(&i.owner).cloned().unwrap_or_else(|| format!("{} (unrenamed)", i.owner));
        format!("{} {} {}", i.kind, owner, i.name)
    };
    let r: Vec<&Item> = r.into_iter().filter(|i| !i.owner.is_empty()).collect();
    pairs.extend(
        unique_pairs(&r, &a, renamed_owner, |i| format!("{} {} {}", i.kind, i.owner, i.name))
            .into_iter()
            .map(|(o, n)| (o.clone(), n.clone())),
    );
    pairs.sort_by_key(|(o, _)| o.path());
    pairs
}

/// The identifiers a text names: its tokens shaped like one.
fn identifiers(text: &str) -> impl Iterator<Item = &str> {
    text.split(' ').filter(|t| {
        t.chars().next().is_some_and(|c| c == '_' || c.is_alphabetic())
            && t.chars().all(|c| c == '_' || c.is_alphanumeric())
    })
}

/// The type a trait impl's header (`Trait for Type<T>`) is for: its last identifier outside generics.
fn impl_self_type(header: &str) -> &str {
    let ty = header.split_once(" for ").map_or(header, |(_, ty)| ty);
    let ty = ty.split('<').next().unwrap_or(ty);
    ty.rsplit("::").next().unwrap_or(ty).trim()
}

/// The paths of the [`SILENT_CHANGE_CRATES`] items a `.rhai` file can reach, by NAME: start from every
/// identifier `vike-rhai`'s library names, take each item whose name, owner or (for a trait impl) self
/// type is among them, add the identifiers that item names, and repeat until nothing is added. Two
/// items sharing a name are both reached: too much is the safe side for "may change results".
pub fn rhai_reach(rhai_code: &[Item], items: &[Item]) -> BTreeSet<String> {
    let mut names: BTreeSet<&str> = rhai_code.iter().flat_map(|i| identifiers(&i.text)).collect();
    let candidates: Vec<&Item> = items.iter().filter(|i| silent_crate(i)).collect();
    let mut reached: BTreeSet<String> = BTreeSet::new();
    loop {
        let mut grew = false;
        for item in &candidates {
            let path = item.path();
            if reached.contains(&path) {
                continue;
            }
            let hit = names.contains(item.name.as_str())
                || (!item.owner.is_empty() && names.contains(item.owner.as_str()))
                || (item.kind == "impl" && names.contains(impl_self_type(&item.name)));
            if hit {
                reached.insert(path);
                names.extend(identifiers(&item.text));
                grew = true;
            }
        }
        if !grew {
            return reached;
        }
    }
}

/// The literal's text without its quotes, when `token` is a plain `"…"` string.
fn unquote(token: &str) -> Option<&str> {
    token.strip_prefix('"')?.strip_suffix('"')
}

/// Every name the Rhai engine registers: in `rhai_code` (`vike-rhai`'s items), each [`REGISTRATIONS`]
/// call whose first argument is a string literal, and each call of a `macro_rules!` that wraps one with
/// its first argument as the name (`read_f64!("close", …)`); in `registry` (vike-indicators'
/// [`INDICATOR_REGISTRY_MODULE`] items), each `IndicatorMeta { name: "…", … }` row and each call of a
/// macro that writes one from its first argument (`indicator!("rsi", …, Rsi)`). A name registered
/// from a value (an indicator's per-line accessor) is not read here: its row is.
pub fn rhai_names(rhai_code: &[Item], registry: &[&Item]) -> Vec<RhaiName> {
    let mut found: BTreeMap<(&'static str, String), Vec<String>> = BTreeMap::new();
    for item in rhai_code {
        let Some(t) = tokens(&item.text) else { continue };
        let at = |i: usize| t.get(i).map_or("", String::as_str);
        let mut wrappers: BTreeMap<&str, &'static str> = BTreeMap::new();
        for j in 0..t.len() {
            if at(j) == "macro_rules" && at(j + 1) == "!" {
                let close = group_end(&t, j + 3);
                if let Some(kind) = (j + 3..close).find_map(|k| {
                    registration(&t, k)
                        .filter(|(_, arg, _)| arg.starts_with('$'))
                        .map(|(kind, _, _)| kind)
                }) {
                    wrappers.insert(at(j + 2), kind);
                }
            }
        }
        for j in 0..t.len() {
            let call = match registration(&t, j) {
                Some((kind, name, end)) => unquote(name).map(|n| (kind, n, end)),
                None => wrappers
                    .get(at(j))
                    .filter(|_| at(j + 1) == "!" && at(j + 2) == "(")
                    .and_then(|kind| unquote(at(j + 3)).map(|n| (*kind, n, group_end(&t, j + 2)))),
            };
            if let Some((kind, name, end)) = call {
                let text: Vec<&str> = t[j..end.min(t.len())]
                    .iter()
                    .map(|token| if unquote(token) == Some(name) { "$" } else { token.as_str() })
                    .collect();
                found.entry((kind, name.to_string())).or_default().push(text.join(" "));
            }
        }
    }
    let registry: Vec<Vec<String>> =
        registry.iter().filter_map(|item| tokens(&item.text)).collect();
    // The macros that write a row from their first argument (`indicator!`, and `pattern!`, which calls
    // it): a `macro_rules!` whose body opens an `IndicatorMeta` named by a `$` argument, or invokes one
    // of these with a `$` argument first.
    let mut row_macros: BTreeSet<String> = BTreeSet::new();
    loop {
        let before = row_macros.len();
        for t in &registry {
            let at = |i: usize| t.get(i).map_or("", String::as_str);
            for j in (0..t.len()).filter(|&j| at(j) == "macro_rules" && at(j + 1) == "!") {
                let body = j + 3..group_end(t, j + 3);
                let opens_a_row =
                    body.clone().any(|k| at(k) == "IndicatorMeta" && at(k + 1) == "{")
                        && body
                            .clone()
                            .any(|k| at(k) == "name" && at(k + 1) == ":" && at(k + 2) == "$");
                let calls_a_row_macro = body
                    .clone()
                    .any(|k| row_macros.contains(at(k)) && at(k + 1) == "!" && at(k + 3) == "$");
                if opens_a_row || calls_a_row_macro {
                    row_macros.insert(at(j + 2).to_string());
                }
            }
        }
        if row_macros.len() == before {
            break;
        }
    }
    for t in &registry {
        let at = |i: usize| t.get(i).map_or("", String::as_str);
        for j in 0..t.len() {
            // `indicator!("rsi", …, Rsi)`: the name first, the implementing type last.
            if row_macros.contains(at(j)) && at(j + 1) == "!" && at(j + 2) == "(" {
                if let Some(name) = unquote(at(j + 3)) {
                    let close = group_end(t, j + 2);
                    let identity = at(close.saturating_sub(2)).to_string();
                    found.entry(("indicator", name.to_string())).or_default().push(identity);
                }
                continue;
            }
            if at(j) != "IndicatorMeta" || at(j + 1) != "{" {
                continue;
            }
            let close = group_end(t, j + 1);
            let field = |name: &str| (j + 2..close).find(|&k| at(k) == name && at(k + 1) == ":");
            let Some(name) = field("name").and_then(|k| unquote(at(k + 2))) else { continue };
            let made = (j + 2..close)
                .find(|&k| at(k) == "mk" && at(k + 1) == "::" && at(k + 2) == "<")
                .map(|k| at(k + 3).to_string());
            let identity = made.unwrap_or_else(|| {
                let text: Vec<&str> = t[j..close]
                    .iter()
                    .map(|tk| if unquote(tk) == Some(name) { "$" } else { tk.as_str() })
                    .collect();
                text.join(" ")
            });
            found.entry(("indicator", name.to_string())).or_default().push(identity);
        }
    }
    found
        .into_iter()
        .map(|((kind, name), mut texts)| {
            texts.sort();
            RhaiName { kind, name, identity: texts.join(" | ") }
        })
        .collect()
}

/// A [`REGISTRATIONS`] call at `t[j]`: its kind, its first argument's token and the index past its
/// closing parenthesis; `.register_fn::<…>(` is read past its turbofish.
fn registration(t: &[String], j: usize) -> Option<(&'static str, &str, usize)> {
    let kind = REGISTRATIONS.iter().find(|(call, _)| t.get(j).is_some_and(|tk| tk == call))?.1;
    let mut k = j + 1;
    if t.get(k).map(String::as_str) == Some("::") && t.get(k + 1).map(String::as_str) == Some("<") {
        let mut depth = 0i32;
        k += 1;
        while let Some(token) = t.get(k) {
            depth += match token.as_str() {
                "<" => 1,
                ">" => -1,
                _ => 0,
            };
            k += 1;
            if depth == 0 {
                break;
            }
        }
    }
    if t.get(k).map(String::as_str) != Some("(") {
        return None;
    }
    Some((kind, t.get(k + 1)?.as_str(), group_end(t, k)))
}

/// The index past the bracket group opening at `t[open]`.
fn group_end(t: &[String], open: usize) -> usize {
    let mut depth = 0usize;
    for (j, token) in t.iter().enumerate().skip(open) {
        match token.as_str() {
            "(" | "[" | "{" => depth += 1,
            ")" | "]" | "}" => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return j + 1;
                }
            }
            _ => {}
        }
    }
    t.len()
}

impl SdkReport {
    /// Every rename and removal, Rust and Rhai, as one [`RenameTable`].
    pub fn rename_table(&self) -> RenameTable {
        let rust = |kind: &str, old: String, new: Option<String>| RenameRow {
            language: "rust".into(),
            kind: kind.into(),
            old,
            new,
        };
        let rhai = |kind: &str, old: &str, new: Option<&str>| RenameRow {
            language: "rhai".into(),
            kind: kind.into(),
            old: old.into(),
            new: new.map(str::to_string),
        };
        let mut rows: Vec<RenameRow> = Vec::new();
        rows.extend(self.renamed.iter().map(|(o, n)| rust(o.kind, o.path(), Some(n.path()))));
        rows.extend(
            self.removed.iter().filter(|i| i.kind != "use").map(|i| rust(i.kind, i.path(), None)),
        );
        rows.extend(self.rhai_renamed.iter().map(|(o, n)| rhai(o.kind, &o.name, Some(&n.name))));
        rows.extend(self.rhai_removed.iter().map(|r| rhai(r.kind, &r.name, None)));
        rows.sort();
        RenameTable { from: self.from.clone(), to: self.to.clone(), rows }
    }

    /// The report as markdown, for a run's summary page; `boundary` is the line
    /// [`super::Boundary::sentence`] wrote.
    pub fn markdown(&self, boundary: &str) -> String {
        let mut out = format!(
            "## Strategy SDK: what changed since `{}`\n\nFrom `{}` to `{}`, over the strategy template's crates ({}; \
             `vike-rust-strategy`, the boundary, is the host's).\n\n**Boundary:** {boundary}\n",
            self.from,
            self.from,
            self.to,
            self.crates.join(", ")
        );
        let item = |i: &Item| format!("`{} {}`", i.kind, i.path());
        let rhai = |r: &RhaiName| format!("`{} {}`", r.kind, r.name);
        let breaks: Vec<String> = self
            .renamed
            .iter()
            .map(|(o, n)| format!("renamed {} → `{}`", item(o), n.path()))
            .chain(self.removed.iter().map(|i| format!("removed {}", item(i))))
            .collect();
        section(&mut out, "Rust: breaks the build", &breaks);
        section(&mut out, "Rust: added", &self.added.iter().map(item).collect::<Vec<_>>());
        let silent_title = format!(
            "Rust: may change results silently (code changed in {}, comments and layout aside)",
            SILENT_CHANGE_CRATES.join(", ")
        );
        section(&mut out, &silent_title, &self.silent.iter().map(item).collect::<Vec<_>>());
        let rhai_breaks: Vec<String> = self
            .rhai_renamed
            .iter()
            .map(|(o, n)| format!("renamed {} → `{}`", rhai(o), n.name))
            .chain(self.rhai_removed.iter().map(|r| format!("removed {}", rhai(r))))
            .collect();
        out.push_str(
            "\n### Rhai\n\nA `.rhai` file runs on the host's own code and pins no SDK: it meets all of this at the \
             next deploy.\n",
        );
        section(&mut out, "Rhai: fails at run time", &rhai_breaks);
        section(&mut out, "Rhai: added", &self.rhai_added.iter().map(rhai).collect::<Vec<_>>());
        section(
            &mut out,
            "Rhai: may change results silently (the code above a registration reaches)",
            &self.rhai_silent.iter().map(item).collect::<Vec<_>>(),
        );
        out
    }
}

/// `title` with its count and its first [`SHOWN`] lines.
fn section(out: &mut String, title: &str, lines: &[String]) {
    out.push_str(&format!("\n#### {title}: {}\n", lines.len()));
    for line in lines.iter().take(SHOWN) {
        out.push_str(&format!("- {line}\n"));
    }
    if lines.len() > SHOWN {
        out.push_str(&format!(
            "- … and {} more (the rename table has every rename and removal)\n",
            lines.len() - SHOWN
        ));
    }
}
