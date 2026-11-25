//! The ITEMS of a crate's library, as the SDK report compares two trees by them
//! ([`super::report`]): every item its module tree declares, from `src/lib.rs` down through each `mod`
//! (a `#[path]` one too), with its kind, its name, its module path, whether it is part of the crate's
//! public face, and its text with comments and layout taken out.
//!
//! **A lexical reading, not rustc's.** The source is lexed by [`crate::ci::rust_comments::strip`] (the
//! planner's own lexer: comments and whitespace runs out) and split into tokens here; the items are
//! found by their keyword at the top of a block. What that reading gives up, on purpose:
//! - `public` is "declared `pub`, in a chain of `pub` modules", never what a `pub use` makes reachable.
//!   A `pub use` is an item of its own (kind `use`, named by its whole tree), so a re-export that moves
//!   still shows.
//! - A trait impl is ONE item (`impl`, named by its header), never public API, its methods inside it.
//! - A `#[cfg(test)]` item or module is skipped; any other `cfg` is read as if it were on (a feature's
//!   API is API). Two items one `cfg` pair declares under one name are folded into one.
//! - A file that does not lex to a balanced end, or that no `mod` reaches, is not read; a macro's
//!   expansion is never seen.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

/// One item of a crate's library ([`crate_items`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    /// The crate's name the way a path names it (`vike_tactics`).
    pub krate: String,
    /// The module path inside the crate, `::`-joined; empty at the crate root.
    pub module: String,
    /// The type an inherent `impl` block's member belongs to; empty for every other item.
    pub owner: String,
    /// `fn`, `struct`, `enum`, `union`, `trait`, `type`, `const`, `static`, `macro`, `use` or `impl`.
    pub kind: &'static str,
    /// The item's name; a `use` item's whole tree, an `impl` item's header.
    pub name: String,
    /// Declared `pub` in a chain of `pub` modules ([`crate_items`] says what that leaves out).
    pub public: bool,
    /// The item's attributes (`#[doc]` aside) and tokens, one space between tokens.
    pub text: String,
}

impl Item {
    /// The item's path, `crate::module::Owner::name`.
    pub fn path(&self) -> String {
        [self.krate.as_str(), self.module.as_str(), self.owner.as_str(), self.name.as_str()]
            .iter()
            .filter(|part| !part.is_empty())
            .copied()
            .collect::<Vec<_>>()
            .join("::")
    }

    /// [`Self::text`] with every token equal to the item's own name written `$`: two items whose
    /// normalized texts are equal differ by their name alone (a rename).
    pub fn normalized(&self) -> String {
        self.text
            .split(' ')
            .map(|token| if token == self.name { "$" } else { token })
            .collect::<Vec<_>>()
            .join(" ")
    }
}

/// Every item of the library of the crate at `crate_dir` (its `src/lib.rs` and the module files it
/// reaches), the crate named `krate` (`vike-tactics` or `vike_tactics`). A crate with no `src/lib.rs`
/// has none.
pub fn crate_items(crate_dir: &Path, krate: &str) -> Vec<Item> {
    let mut walk = Walk { krate: krate.replace('-', "_"), out: Vec::new(), seen: BTreeSet::new() };
    let src = crate_dir.join("src");
    walk.file(&src.join("lib.rs"), "", true, &src);
    walk.out
}

/// The walk's state: the crate's name, what it found, and the files already read (a `#[path]` loop
/// reads nothing twice).
struct Walk {
    krate: String,
    out: Vec<Item>,
    seen: BTreeSet<PathBuf>,
}

/// Where a block sits: its module path, whether every module above it is `pub`, the inherent impl's
/// type its members belong to, and the folder its `mod x;` children are read from.
struct Scope<'a> {
    module: &'a str,
    public: bool,
    owner: &'a str,
    children: &'a Path,
}

impl Walk {
    fn file(&mut self, file: &Path, module: &str, public: bool, children: &Path) {
        if !self.seen.insert(file.to_path_buf()) {
            return;
        }
        let Some(tokens) = fs::read_to_string(file).ok().as_deref().and_then(tokens) else {
            return;
        };
        let scope = Scope { module, public, owner: "", children };
        self.block(&tokens, 0, tokens.len(), &scope);
    }

    /// The items of `t[from..end]`, a file or the inside of a `mod`/`impl` block.
    fn block(&mut self, t: &[String], from: usize, end: usize, scope: &Scope<'_>) {
        let at = |i: usize| t.get(i).map_or("", String::as_str);
        let mut i = from;
        while i < end {
            let mut attributes: Vec<String> = Vec::new();
            while at(i) == "#" {
                let inner = at(i + 1) == "!";
                let open = if inner { i + 2 } else { i + 1 };
                if at(open) != "[" {
                    break;
                }
                let close = matching(t, open, end);
                if !inner {
                    attributes.push(t[open..=close].join(" "));
                }
                i = close + 1;
            }
            if i >= end {
                break;
            }
            let test_only = attributes.iter().any(|a| a.replace(' ', "") == "[cfg(test)]");
            let kept: Vec<String> = attributes
                .iter()
                .filter(|a| !a.starts_with("[ doc"))
                .map(|a| format!("# {a}"))
                .collect();
            let start = i;
            let mut public = false;
            if at(i) == "pub" {
                if at(i + 1) == "(" {
                    i = matching(t, i + 1, end) + 1;
                } else {
                    public = true;
                    i += 1;
                }
            }
            loop {
                match at(i) {
                    "default" | "async" | "unsafe" | "auto" | "safe" => i += 1,
                    "const" if matches!(at(i + 1), "fn" | "unsafe" | "async" | "extern") => i += 1,
                    "extern" if at(i + 1).starts_with('"') => i += 2,
                    "extern" if at(i + 1) == "fn" => i += 1,
                    _ => break,
                }
            }
            let keyword = at(i);
            let name_at = if keyword == "static" && at(i + 1) == "mut" { i + 2 } else { i + 1 };
            let (kind, name, next): (Option<&'static str>, String, usize) = match keyword {
                "fn" | "struct" | "enum" | "union" | "trait" => {
                    (Some(kind_word(keyword)), at(name_at).to_string(), item_end(t, i, end, true))
                }
                "type" | "const" | "static" => {
                    (Some(kind_word(keyword)), at(name_at).to_string(), item_end(t, i, end, false))
                }
                // One item per name the tree brings in, so a list that gains one name moves one item.
                "use" => {
                    let next = item_end(t, i, end, false);
                    if !test_only {
                        let tree = &t[i + 1..next.saturating_sub(1).max(i + 1)];
                        let mut leaves = Vec::new();
                        use_leaves(tree, "", &mut leaves);
                        for (leaf, path) in leaves {
                            let text: Vec<String> =
                                kept.iter().cloned().chain([format!("use {path}")]).collect();
                            self.push(scope, "use", leaf, public && scope.public, text);
                        }
                    }
                    (None, String::new(), next)
                }
                "macro_rules" if at(i + 1) == "!" => {
                    let exported = attributes.iter().any(|a| a.contains("macro_export"));
                    public = exported;
                    (Some("macro"), at(i + 2).to_string(), item_end(t, i, end, true))
                }
                "mod" => {
                    let name = at(i + 1).to_string();
                    if at(i + 2) == "{" {
                        let close = matching(t, i + 2, end);
                        if !test_only {
                            let module = join_module(scope.module, &name);
                            let children = scope.children.join(&name);
                            let inner = Scope {
                                module: &module,
                                public: scope.public && public,
                                owner: "",
                                children: &children,
                            };
                            self.block(t, i + 3, close, &inner);
                        }
                        (None, String::new(), close + 1)
                    } else {
                        if !test_only {
                            let module = join_module(scope.module, &name);
                            self.module_file(&name, &attributes, &module, scope, public);
                        }
                        (None, String::new(), item_end(t, i, end, false))
                    }
                }
                "impl" => {
                    let open = (i..end).find(|&j| at(j) == "{").unwrap_or(end);
                    let close = if open < end { matching(t, open, end) } else { end - 1 };
                    if !test_only {
                        let (trait_part, self_part) = impl_header(&t[i + 1..open.min(end)]);
                        match trait_part {
                            Some(trait_part) => {
                                let header =
                                    format!("{} for {}", pretty(trait_part), pretty(self_part));
                                let text =
                                    kept.iter().cloned().chain(t[start..=close].iter().cloned());
                                self.push(scope, "impl", header, true, text.collect());
                            }
                            None => {
                                let owner = type_name(self_part);
                                let inner = Scope {
                                    module: scope.module,
                                    public: scope.public,
                                    owner: &owner,
                                    children: scope.children,
                                };
                                self.block(t, open + 1, close, &inner);
                            }
                        }
                    }
                    (None, String::new(), close + 1)
                }
                word if at(i + 1) == "!" && !word.is_empty() => {
                    (None, String::new(), item_end(t, i, end, true))
                }
                "extern" | "crate" if at(i + 1) == "{" || at(i + 1) == "crate" => {
                    (None, String::new(), item_end(t, i, end, true))
                }
                _ => (None, String::new(), i + 1),
            };
            if let Some(kind) = kind.filter(|_| !test_only) {
                let text: Vec<String> =
                    kept.into_iter().chain(t[start..next].iter().cloned()).collect();
                self.push(scope, kind, name, public && scope.public, text);
            }
            i = next.max(i + 1);
        }
    }

    /// Read the file module `name` a `mod name;` declares (its `#[path]` when it has one).
    fn module_file(
        &mut self,
        name: &str,
        attributes: &[String],
        module: &str,
        scope: &Scope<'_>,
        public: bool,
    ) {
        let path_attribute = attributes.iter().find_map(|a| {
            let rest = a.strip_prefix("[ path = ")?;
            rest.strip_suffix(" ]")?.strip_prefix('"')?.strip_suffix('"').map(str::to_string)
        });
        let public = scope.public && public;
        match path_attribute {
            // A `#[path]` file's own children sit beside it, as a `mod.rs`'s do.
            Some(relative) => {
                let file = scope.children.join(relative);
                let children = file.parent().map(Path::to_path_buf).unwrap_or_default();
                self.file(&file, module, public, &children);
            }
            None => {
                let flat = scope.children.join(format!("{name}.rs"));
                let file =
                    if flat.is_file() { flat } else { scope.children.join(name).join("mod.rs") };
                self.file(&file, module, public, &scope.children.join(name));
            }
        }
    }

    fn push(
        &mut self,
        scope: &Scope<'_>,
        kind: &'static str,
        name: String,
        public: bool,
        text: Vec<String>,
    ) {
        let item = Item {
            krate: self.krate.clone(),
            module: scope.module.to_string(),
            owner: scope.owner.to_string(),
            kind,
            name,
            public,
            text: text.join(" "),
        };
        // Two items one `cfg` pair declares under one name are one item, their texts joined.
        match self.out.iter_mut().find(|o| {
            o.module == item.module
                && o.owner == item.owner
                && o.kind == item.kind
                && o.name == item.name
        }) {
            Some(same) => {
                same.text = format!("{} | {}", same.text, item.text);
                same.public |= item.public;
            }
            None => self.out.push(item),
        }
    }
}

fn kind_word(keyword: &str) -> &'static str {
    match keyword {
        "fn" => "fn",
        "struct" => "struct",
        "enum" => "enum",
        "union" => "union",
        "trait" => "trait",
        "type" => "type",
        "const" => "const",
        _ => "static",
    }
}

fn join_module(parent: &str, name: &str) -> String {
    match (parent.is_empty(), name.is_empty()) {
        (true, _) => name.to_string(),
        (_, true) => parent.to_string(),
        _ => format!("{parent}::{name}"),
    }
}

/// Every name the use tree `tree` (after `prefix`) brings in, with the path it names, into `out`:
/// `a::{b, c as d, e::{self, f}}` is `b`, `d`, `e` and `f`.
fn use_leaves(tree: &[String], prefix: &str, out: &mut Vec<(String, String)>) {
    if let Some(open) = tree.iter().position(|token| token == "{") {
        let base = join_module(prefix, tree[..open].concat().trim_end_matches("::"));
        let close = matching(tree, open, tree.len());
        let mut depth = 0usize;
        let mut start = open + 1;
        for (j, token) in tree.iter().enumerate().take(close + 1).skip(open + 1) {
            match token.as_str() {
                "{" => depth += 1,
                "}" if depth > 0 => depth -= 1,
                "," | "}" if depth == 0 => {
                    if j > start {
                        use_leaves(&tree[start..j], &base, out);
                    }
                    start = j + 1;
                }
                _ => {}
            }
        }
        return;
    }
    let (path, alias) = match tree.iter().position(|token| token == "as") {
        Some(k) => (&tree[..k], tree.get(k + 1).cloned()),
        None => (tree, None),
    };
    let Some(last) = path.last() else { return };
    let full = join_module(prefix, &path.concat());
    if last == "self" {
        let named = full.trim_end_matches("::self").to_string();
        let leaf = named.rsplit("::").next().unwrap_or_default().to_string();
        out.push((alias.unwrap_or(leaf), named));
    } else {
        out.push((alias.unwrap_or_else(|| last.clone()), full));
    }
}

/// The index of the bracket closing the one opening at `t[open]` (any of `( [ {` counts), or the last
/// index before `end` when it never closes (a source [`tokens`] took is balanced).
fn matching(t: &[String], open: usize, end: usize) -> usize {
    let mut depth = 0usize;
    for (j, token) in t.iter().enumerate().take(end).skip(open) {
        match token.as_str() {
            "(" | "[" | "{" => depth += 1,
            ")" | "]" | "}" => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return j;
                }
            }
            _ => {}
        }
    }
    end.saturating_sub(1)
}

/// The index just past the item starting at `t[from]`: past its first `;` outside brackets, or, when
/// `braced`, past its first `{…}` block if that comes first (a fn's body, a struct's fields).
fn item_end(t: &[String], from: usize, end: usize, braced: bool) -> usize {
    let mut j = from;
    while j < end {
        match t[j].as_str() {
            ";" => return j + 1,
            "{" if braced => return matching(t, j, end) + 1,
            "(" | "[" | "{" => j = matching(t, j, end),
            _ => {}
        }
        j += 1;
    }
    end
}

/// An `impl` header (the tokens between `impl` and its block) split into its trait part, if any, and
/// its self type, its own generics and `where` clause left out.
fn impl_header(header: &[String]) -> (Option<&[String]>, &[String]) {
    let mut start = 0;
    if header.first().map(String::as_str) == Some("<") {
        let mut depth = 0i32;
        for (j, token) in header.iter().enumerate() {
            depth += angle(token);
            if depth == 0 {
                start = j + 1;
                break;
            }
        }
    }
    let header = &header[start..];
    let mut depth = 0i32;
    let mut split = None;
    let mut stop = header.len();
    for (j, token) in header.iter().enumerate() {
        depth += angle(token);
        if depth == 0 && token == "for" && split.is_none() {
            split = Some(j);
        }
        if depth == 0 && token == "where" {
            stop = j;
            break;
        }
    }
    match split {
        Some(j) if j < stop => (Some(&header[..j]), &header[j + 1..stop]),
        _ => (None, &header[..stop]),
    }
}

fn angle(token: &str) -> i32 {
    match token {
        "<" => 1,
        ">" => -1,
        _ => 0,
    }
}

/// A type's name as a member's owner: the last identifier outside its generics (`vike_model::Bar<T>`
/// is `Bar`), or the whole type written out when it has none (`&[T]`).
fn type_name(ty: &[String]) -> String {
    let mut depth = 0i32;
    let mut name = None;
    for token in ty {
        depth += angle(token);
        let ident = token.chars().next().is_some_and(|c| c == '_' || c.is_alphabetic());
        if depth == 0 && ident && !matches!(token.as_str(), "dyn" | "mut" | "const" | "impl") {
            name = Some(token.clone());
        }
    }
    name.unwrap_or_else(|| pretty(ty))
}

/// Tokens written back as a reader would write them: a space between words, none around `<`, `>`,
/// `::`, `&` and before `,`.
fn pretty(tokens: &[String]) -> String {
    let mut out = String::new();
    for token in tokens {
        let tight_before = matches!(token.as_str(), "<" | ">" | "::" | "," | ")" | "]");
        let tight_after = out.ends_with(['<', ':', '&', '(', '[']);
        if !out.is_empty() && !tight_before && !tight_after {
            out.push(' ');
        }
        out.push_str(token);
    }
    out
}

/// The tokens of the Rust source `src`, comments and layout out, each literal one token; `None` when
/// the source does not lex to a balanced end.
pub fn tokens(src: &str) -> Option<Vec<String>> {
    let stripped = crate::ci::rust_comments::strip(src)?.tokens;
    let c: Vec<char> = stripped.chars().collect();
    let at = |i: usize| c.get(i).copied();
    let word = |ch: char| ch == '_' || ch.is_alphanumeric();
    let mut out = Vec::new();
    let mut i = 0;
    while i < c.len() {
        let ch = c[i];
        if ch.is_whitespace() {
            i += 1;
            continue;
        }
        let start = i;
        if ch == '_' || ch.is_alphabetic() {
            while at(i).is_some_and(word) {
                i += 1;
            }
            let prefix: String = c[start..i].iter().collect();
            match (prefix.as_str(), at(i)) {
                ("r", Some('#')) if at(i + 1).is_some_and(|n| n == '_' || n.is_alphabetic()) => {
                    i += 1;
                    while at(i).is_some_and(word) {
                        i += 1;
                    }
                }
                ("b" | "c", Some('"')) => i = string_end(&c, i)?,
                ("b", Some('\'')) => i = char_end(&c, i)?,
                ("r" | "br" | "cr", Some('"' | '#')) => i = raw_string_end(&c, i)?,
                _ => {}
            }
        } else if ch.is_ascii_digit() {
            while at(i).is_some_and(|n| {
                word(n) || (n == '.' && at(i + 1).is_some_and(|d| d.is_ascii_digit()))
            }) {
                i += 1;
            }
        } else {
            i = match (ch, at(i + 1)) {
                ('"', _) => string_end(&c, i)?,
                ('\'', Some('\\')) => char_end(&c, i)?,
                ('\'', _) if at(i + 2) == Some('\'') => char_end(&c, i)?,
                // A lifetime or a label.
                ('\'', _) => {
                    let mut j = i + 1;
                    while at(j).is_some_and(word) {
                        j += 1;
                    }
                    j
                }
                ('-' | '=', Some('>')) | (':', Some(':')) => i + 2,
                _ => i + 1,
            };
        }
        out.push(c[start..i].iter().collect());
    }
    Some(out)
}

/// Past the `"…"` string starting at `c[i]`.
fn string_end(c: &[char], i: usize) -> Option<usize> {
    let mut j = i + 1;
    loop {
        match c.get(j)? {
            '\\' => j += 2,
            '"' => return Some(j + 1),
            _ => j += 1,
        }
    }
}

/// Past the char literal starting at `c[i]` (`'x'`, `'\n'`, `'\u{..}'`, `'\''`).
fn char_end(c: &[char], i: usize) -> Option<usize> {
    if c.get(i + 1)? == &'\\' {
        let mut j = i + 3;
        while c.get(j)? != &'\'' {
            j += 1;
        }
        Some(j + 1)
    } else {
        Some(i + 3)
    }
}

/// Past the raw string whose hashes or quote start at `c[i]` (`r#"…"#`).
fn raw_string_end(c: &[char], i: usize) -> Option<usize> {
    let mut j = i;
    while c.get(j)? == &'#' {
        j += 1;
    }
    let hashes = j - i;
    if c.get(j)? != &'"' {
        return None;
    }
    j += 1;
    loop {
        if c.get(j)? == &'"' && (1..=hashes).all(|k| c.get(j + k) == Some(&'#')) {
            return Some(j + 1 + hashes);
        }
        j += 1;
    }
}
