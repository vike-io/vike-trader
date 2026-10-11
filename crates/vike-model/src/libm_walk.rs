//! The SHARED half of the transcendental-portability probe every compute crate carries.
//!
//! Decision 0032 rules that `ln`/`exp`/`pow` come from the `libm` CRATE and not the platform,
//! because IEEE 754 requires nothing of their last bit and the Windows dev box and the Linux CI
//! boxes are each entitled to a different one. Eleven crates hold a `libm_platform_probe` that
//! enforces it, and until this module each held a private copy of the text machinery below —
//! eleven copies of one parser. Decision 0074 overturned a nine-file refusal to share them, after
//! measuring that three of the eleven had already fallen behind the other eight.
//!
//! ⚠ **What lives here is a PARSER, and the GATE is not here.** Each crate keeps
//! `production_code_calls_libm_not_the_platform` in its own tests, with its own
//! `CARGO_MANIFEST_DIR`, its own must-scan non-vacuity list, its own exemption table and its own
//! failure message — so no crate can be moved out from under its own determinism gate. Only the
//! string handling is shared, and a parser cannot be moved out from under anybody: it takes lines
//! of text and returns ranges. The per-crate EVIDENCE that used to sit in these doc comments
//! stayed with each gate, which is where a reader of that crate will look for it.
//!
//! ⚠ **The self-test is deliberately NOT in this file, and that is not tidiness.** It plants a
//! synthetic source fixture whose rows CONTAIN banned needles as string literals, and the property
//! that makes those rows data rather than findings is that they sit under a tests directory, which
//! no crate's scan reads. This module is under src, which every scan reads — and
//! [`cfg_test_ranges`](crate::libm_walk::cfg_test_ranges)'s own `TEST_ITEM_ATTRS` does not recognise the
//! `any(test, feature = "...")` gate this module itself wears, so the fixture would be scanned as
//! production and this crate's gate would fail on it. The fixture therefore lives in
//! `crates/vike-model/tests/libm_walk_selftest.rs` — one copy in place of the ten it replaces.
//!
//! Gated the same way as this crate's `MockBroker`: a default build compiles none of it, and a
//! consumer enables it as a dev-dependency feature.

/// The `f64` methods whose last bit is the PLATFORM's business rather than IEEE 754's, as bare
/// names. [`banned_needles`] renders each into the two spellings that reach it.
///
/// `sqrt` is deliberately absent and must stay absent: IEEE 754 requires it correctly rounded, so
/// it is identical on every box and there is nothing to convert it TO. `to_degrees`/`to_radians`
/// are absent for the same reason wearing a different disguise: std lowers each to one
/// multiplication by a constant.
///
/// `powi` IS here, and it is the entry that looks like a precaution and is not. MEASURED on MSVC,
/// a `dev` build lowers `llvm.powi` to the CRT's `pow()`, so it is a libcall rather than the free
/// multiply chain its name suggests. Its cure is `libm::pow` where the base is a runtime `f64`,
/// and a MULTIPLICATION where the base is a literal — which is why a crate may carry a `powi`
/// exemption table, and why that table is the crate's own rather than this list's.
pub const BANNED_FNS: [&str; 26] = [
    "powf", "ln", "log", "log2", "log10", "exp", "exp2", "exp_m1", "ln_1p", "sin", "cos", "tan",
    "powi", "atan", "atan2", "asin", "acos", "sinh", "cosh", "tanh", "cbrt", "hypot", "asinh",
    "acosh", "atanh", "sin_cos",
];

/// Every [`BANNED_FNS`] name in BOTH spellings that reach the platform's libm.
///
/// ⚠ The UFCS half is not decoration. A gate that bans the method spelling and nothing else walks
/// straight past the fully-qualified one — the same inherent method, compiling to the same
/// libcall. Neither is more correct Rust and rustfmt rewrites neither into the other, so which one
/// an author reaches for is a coin flip. The `f64::NAME(` needle also covers
/// `std::primitive::f64::NAME(` and `core::primitive::f64::NAME(` for free, because both END with
/// exactly that pattern.
///
/// ⚠ **What it still cannot see, stated rather than implied:** the angle-bracket spelling (what
/// appears is `f64>::NAME(`, and no needle here ends in `>`), a call reached through a generic
/// bound (`T: Float`), and a call split across two lines by a `max_width` wrap. A list of
/// spellings is an enumeration, and an enumeration is never a proof — so each crate's gate states
/// whether any of the three occurs under ITS own sources, which is a per-crate fact and stays
/// with that crate.
pub fn banned_needles() -> Vec<String> {
    BANNED_FNS.iter().flat_map(|f| [format!(".{f}("), format!("f64::{f}(")]).collect()
}

/// Every TEST-ONLY item's line range in one file, as `(start, end, closed)` — `start` inclusive,
/// `end` EXCLUSIVE, both zero-based; `closed` says whether the item's end was actually LOCATED
/// rather than assumed, so the one silent failure mode can be asserted on.
///
/// ⚠ **RANGES, never a `break` at the first marker.** A `break` says "the shipped half of a file
/// ends at its first test module", which holds only when that module is LAST. It does not hold in
/// this workspace: files here carry production code below a first test module, and the crate that
/// first measured the failure found two `libm` sites hiding in the region a `break` discarded.
/// Each crate's gate collects its own `scanned_past_a_test_module` evidence and names its own
/// files.
///
/// ⚠ **TWO marker spellings are recognised.** An `all(test, ...)` attribute is unambiguously
/// test-only and is a range. An `any(test, feature = "...")` one is deliberately NOT — that
/// attribute marks code that SHIPS whenever the feature is on, and treating it as test-only would
/// take a shipped double out of every gate's view. `crates/vike-model/src/strategy/mod.rs`'s
/// `MockBroker` is the workspace's clearest instance, and this module itself is a second.
///
/// ⚠ **And the cut is LINE BY LINE with the comment filter FIRST.** A whole-file `find` on the
/// marker truncates at the first occurrence of that string ANYWHERE, a doc comment included, and a
/// comment-skipping filter applied afterwards cannot rescue it because it runs on already-truncated
/// text. Several crates carry such prose mentions, so a whole-file cut would blank most of the
/// files that carry them and report a confident zero for each.
///
/// # How an item's end is found, and why by INDENTATION rather than by counting braces
///
/// A brace count needs to know which braces are code, which puts a Rust string/char/comment lexer
/// inside a gate — and this workspace's test modules are full of formatting macros and of assert
/// messages continued across lines, so a naive count is wrong on exactly the files that matter.
/// Indentation needs no lexer and is exact here for a STRUCTURAL reason: `cargo fmt --check` is
/// CI's first gate, so every file in the tree is rustfmt output, and rustfmt closes a block with a
/// brace alone on a line at the block's OWN indentation.
///
/// Two ways it can be wrong, and only one is quiet. A line INSIDE the item that IS the closing
/// pattern ends the range early — the scan then resumes over test code, which is full of banned
/// spellings, so that failure is a LOUD false positive naming the line. No closing pattern at ALL
/// runs the range to EOF and is SILENT, which is the `break`'s behaviour; that is what `closed` is
/// for and why every caller asserts on it.
pub fn cfg_test_ranges(lines: &[&str]) -> Vec<(usize, usize, bool)> {
    /// The attribute spellings that open a TEST-ONLY item. See the doc above for why the
    /// `any(test, ...)` spelling is absent: that code SHIPS when its feature is on.
    const TEST_ITEM_ATTRS: [&str; 2] = ["#[cfg(test)]", "#[cfg(all(test,"];

    let mut items = Vec::new();
    let mut i = 0usize;
    while i < lines.len() {
        let trimmed = lines[i].trim_start();
        // Prose FIRST, for the reason the doc gives: a comment naming the attribute in backticks
        // must not be able to open a range.
        if trimmed.starts_with("//") || !TEST_ITEM_ATTRS.iter().any(|a| trimmed.starts_with(a)) {
            i += 1;
            continue;
        }
        let mut closing = " ".repeat(lines[i].len() - trimmed.len());
        closing.push('}');
        let mut opened = false;
        let mut end = lines.len();
        let mut closed = false;
        for (j, line) in lines.iter().enumerate().skip(i) {
            let tail = line.trim_end();
            if !opened {
                // The header may span lines (further attributes, the `mod` on the next line). It
                // ends either by opening a block or, for the `;` form, at the semicolon.
                if tail.ends_with('{') {
                    opened = true;
                } else if tail.ends_with(';') {
                    end = j + 1;
                    closed = true;
                    break;
                }
                continue;
            }
            if tail == closing {
                end = j + 1;
                closed = true;
                break;
            }
        }
        items.push((i, end, closed));
        i = end;
    }
    items
}

/// Every source FILE a `#[cfg(test)] mod NAME;` declaration pulls in — test code by construction,
/// so a production scan skips it WHOLE. `sources` is `(path, text)` for each file under a `src/`.
///
/// [`cfg_test_ranges`] answers "which lines of THIS file are test-only" and stops at the
/// declaration's `;`, which is right: the declaration line itself ships nothing. But the module's
/// BODY then lives in another file that no marker inside it announces, so a walk that reads that
/// file as ordinary source reports every test-only spelling in it as production. The code-layout
/// plan's Task 8 moved hundreds of inline modules into exactly that shape, and every
/// `libm_platform_probe` read the moved files as production on the first run.
///
/// Resolution follows rustc: an explicit `#[path = "…"]` in the declaration's attribute block
/// (above or below the `#[cfg(test)]` line) is relative to the declaring file's directory; without
/// one, a `mod.rs`/`lib.rs`/`main.rs` parent looks beside itself and any other `foo.rs` looks in
/// `foo/`, each as `NAME.rs` or `NAME/mod.rs`. Both candidates are returned, so the caller asks
/// `contains` on the path its own walk produced. The same two attribute spellings as
/// [`cfg_test_ranges`] count, for the same reason: `any(test, …)` ships.
pub fn cfg_test_module_files(
    sources: &[(std::path::PathBuf, String)],
) -> std::collections::BTreeSet<std::path::PathBuf> {
    sources.iter().flat_map(|(path, text)| cfg_test_module_children(path, text)).collect()
}

/// The files ONE source's `#[cfg(test)] mod NAME;` declarations pull in — the per-file core of
/// [`cfg_test_module_files`], borrowing rather than copying, because the path-keyed gates call it
/// over the whole `crates/` tree several times per run.
pub fn cfg_test_module_children(path: &std::path::Path, text: &str) -> Vec<std::path::PathBuf> {
    test_module_decls(path, text).into_iter().flat_map(|d| d.candidates).collect()
}

/// One `#[cfg(test)] mod NAME;` declaration, located.
struct TestModuleDecl {
    /// 0-indexed line of the `mod NAME;` itself (the marker's own line in the one-line form).
    decl_line: usize,
    /// 0-indexed line of its `#[path = "…"]` attribute, when it has one.
    path_line: Option<usize>,
    /// Where the module's body can live, per rustc's rules — one explicit path, or the two
    /// implicit candidates.
    candidates: Vec<std::path::PathBuf>,
}

/// `decl` without a leading visibility qualifier — `pub`, `pub(crate)`, `pub(super)`, `pub(self)`
/// or `pub(in …)` — so every spelling of `mod NAME;` is recognised as a declaration.
fn strip_visibility(decl: &str) -> &str {
    let Some(rest) = decl.strip_prefix("pub") else { return decl };
    if let Some(rest) = rest.strip_prefix(' ') {
        return rest.trim_start();
    }
    if rest.starts_with('(')
        && let Some(close) = rest.find(')')
    {
        return rest[close + 1..].trim_start();
    }
    decl
}

/// Whether `text` spells any of `attrs` anywhere — exactly `attrs.iter().any(|a| text.contains(a))`,
/// for needles that all begin with `#`.
///
/// ⚠ **Not `str::contains`, and the reason is measured.** `contains(&str)` is generic, so its
/// substring search is compiled at the CALLER's opt-level — 0 in the dev and test profiles — and
/// two such passes over the `crates/` tree cost MORE than the line scan the guard exists to skip.
/// This hops from `#` to `#` with `str::find(char)`, whose byte search is core's `memchr`
/// (compiled optimised in `core`, whatever this crate's opt-level), and compares only at a `#` —
/// one byte in ~1,600 of that tree. MEASURED 2026-10-03 on the latency box, user CPU of one whole-tree walk
/// and fold (`settings_registry`'s `the_walk_finds_the_workspace`), 8 alternating rounds: 0.71 s
/// with no guard, 0.92 s with a `contains` guard, 0.50 s with this one.
fn spells_any_hash_attr(text: &str, attrs: &[&str]) -> bool {
    debug_assert!(attrs.iter().all(|a| a.starts_with('#')));
    let mut rest = text;
    while let Some(k) = rest.find('#') {
        rest = &rest[k..];
        if attrs.iter().any(|a| rest.starts_with(a)) {
            return true;
        }
        rest = &rest[1..];
    }
    false
}

fn test_module_decls(path: &std::path::Path, text: &str) -> Vec<TestModuleDecl> {
    const TEST_ITEM_ATTRS: [&str; 2] = ["#[cfg(test)]", "#[cfg(all(test,"];
    let mut out = Vec::new();
    // A declaration needs a LINE that STARTS with one of the attributes, so a text that spells
    // neither anywhere declares nothing — skip the line split and the per-line trim, which are the
    // whole cost of this function. MEASURED 2026-10-03: 1,622 of the 2,715 `.rs` files under
    // `crates/` spell neither attribute.
    if !spells_any_hash_attr(text, &TEST_ITEM_ATTRS) {
        return out;
    }
    let Some(dir) = path.parent() else { return out };
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
    let owns_dir = matches!(stem, "mod" | "lib" | "main");
    let lines: Vec<&str> = text.lines().collect();
    for (i, line) in lines.iter().enumerate() {
        let t = line.trim_start();
        if !TEST_ITEM_ATTRS.iter().any(|a| t.starts_with(*a)) {
            continue;
        }
        // The attribute block around the marker: `#[path]` sits above it in this tree's shape, and
        // further attributes may sit on either side.
        let mut lo = i;
        while lo > 0 && lines[lo - 1].trim_start().starts_with("#[") {
            lo -= 1;
        }
        let mut hi = i;
        while hi + 1 < lines.len() && lines[hi + 1].trim_start().starts_with("#[") {
            hi += 1;
        }
        // The declaration: on the marker's own line, or on the first line after the block. The
        // marker is a PREFIX (`#[cfg(all(test,` carries the rest of its attribute on the same line),
        // so what follows it is read from after the attribute's closing `]`.
        let same_line = t.find(']').map_or("", |close| t[close + 1..].trim());
        let (decl_line, decl) = if !same_line.is_empty() {
            (i, same_line)
        } else {
            match lines.get(hi + 1) {
                Some(l) => (hi + 1, l.trim()),
                None => continue,
            }
        };
        let decl = strip_visibility(decl);
        let Some(name) = decl.strip_prefix("mod ").and_then(|d| d.strip_suffix(';')) else {
            continue; // an inline `mod NAME {` — `cfg_test_ranges`' business, not a file
        };
        let explicit = (lo..=hi).find_map(|k| {
            lines[k]
                .trim()
                .strip_prefix("#[path = \"")
                .and_then(|r| r.strip_suffix("\"]"))
                .map(|p| (k, p))
        });
        let (path_line, candidates) = match explicit {
            Some((k, p)) => (Some(k), vec![dir.join(p)]),
            None => {
                let base = if owns_dir { dir.to_path_buf() } else { dir.join(stem) };
                (None, vec![base.join(format!("{name}.rs")), base.join(name).join("mod.rs")])
            }
        };
        out.push(TestModuleDecl { decl_line, path_line, candidates });
    }
    out
}

/// `text` with each `#[cfg(test)] mod NAME;` whose body `read` can supply put back INLINE, as
/// `mod NAME { … }` at the declaration's own position, and its `#[path]` line dropped — the file
/// as it read before its test modules moved out.
///
/// For a test that uses a REAL file as a realistic scanner input and whose assertion is about the
/// INLINE shape (plant a call inside the module body, prove it is not counted). The code-layout
/// plan's Task 8 moved such modules into sibling files; the scanner's view of a declaration is a
/// different, simpler case, and the inline case it still has to handle for every file below the
/// extraction threshold is what these tests prove.
pub fn reinline_test_modules(
    path: &std::path::Path,
    text: &str,
    read: impl Fn(&std::path::Path) -> Option<String>,
) -> String {
    reinline_decls(text, test_module_decls(path, text), read)
}

/// [`reinline_test_modules`] over declarations the caller already located in `text` — so
/// [`fold_test_module_files`] locates each file's declarations once rather than once per step.
fn reinline_decls(
    text: &str,
    decls: Vec<TestModuleDecl>,
    read: impl Fn(&std::path::Path) -> Option<String>,
) -> String {
    let resolved: Vec<(TestModuleDecl, String)> = decls
        .into_iter()
        .filter_map(|d| {
            let body = d.candidates.iter().find_map(|c| read(c.as_path()))?;
            Some((d, body))
        })
        .collect();
    let mut out = String::with_capacity(text.len());
    for (i, line) in text.lines().enumerate() {
        if resolved.iter().any(|(d, _)| d.path_line == Some(i)) {
            continue;
        }
        if let Some((_, body)) = resolved.iter().find(|(d, _)| d.decl_line == i) {
            let indent = &line[..line.len() - line.trim_start().len()];
            let head = line.trim_end();
            out.push_str(head.strip_suffix(';').unwrap_or(head));
            out.push_str(" {\n");
            for b in body.lines() {
                if !b.is_empty() {
                    out.push_str(indent);
                    out.push_str("    ");
                    out.push_str(b);
                }
                out.push('\n');
            }
            out.push_str(indent);
            out.push_str("}\n");
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}

/// [`cfg_test_module_files`] over every `.rs` file under `root`, read from disk — the form each
/// crate's probe calls with its own `src/`.
pub fn cfg_test_module_files_under(
    root: &std::path::Path,
) -> std::collections::BTreeSet<std::path::PathBuf> {
    let mut out = std::collections::BTreeSet::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().and_then(|e| e.to_str()) == Some("rs")
                && let Ok(text) = std::fs::read_to_string(&path)
            {
                out.extend(cfg_test_module_children(&path, &text));
            }
        }
    }
    out
}

/// A resolved module path in the repo-relative, `/`-separated form the path-keyed gates key on.
fn rel_string(path: &std::path::Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

/// [`cfg_test_module_files`] over repo-relative, `/`-separated path STRINGS — the form the
/// path-keyed gates hold their sources in — returning the same form.
pub fn cfg_test_module_rel_files(
    sources: &[(String, String)],
) -> std::collections::BTreeSet<String> {
    sources
        .iter()
        .flat_map(|(rel, text)| cfg_test_module_children(std::path::Path::new(rel), text))
        .map(|p| rel_string(&p))
        .collect()
}

/// `sources` with every test module that lives in its own file FOLDED BACK into the file that
/// declares it — put back INLINE, as `mod NAME { … }` at the declaration's own position (see
/// [`reinline_test_modules`]) — and the module file no longer an entry of its own.
///
/// For a gate whose rows are keyed on the file that OWNS a test module, or that locates test code
/// only as an inline block. Such a gate saw `foo.rs` + its inline `mod tests { … }` as ONE text;
/// after the module moved to `foo_tests.rs` it would see two files, and every row keyed on
/// `foo.rs` would describe half a file. Folding restores the text it was written against — same
/// position, same `#[cfg(test)]` marker, so a "trailing test block" locator finds the same line —
/// and its rows stay true without a re-key. A gate that EXCLUDES test code wants
/// [`cfg_test_module_rel_files`] instead, to drop the files outright.
pub fn fold_test_module_files(sources: Vec<(String, String)>) -> Vec<(String, String)> {
    // Each file's declarations are located ONCE and carried through all three steps — the child
    // set, the ownership test and the re-inlining — which used to locate them again each, so a
    // whole-tree fold scanned most files two to three times. MEASURED 2026-10-03 on the latency box (the
    // measurement `spells_any_hash_attr` cites, 6 rounds): 0.59 s of user CPU -> 0.33 s.
    let decls: Vec<Vec<TestModuleDecl>> = sources
        .iter()
        .map(|(rel, text)| test_module_decls(std::path::Path::new(rel), text))
        .collect();
    let children: std::collections::BTreeSet<String> =
        decls.iter().flatten().flat_map(|d| &d.candidates).map(|p| rel_string(p)).collect();
    let mut parents = Vec::new();
    let mut bodies = std::collections::BTreeMap::new();
    for ((rel, text), decls) in sources.into_iter().zip(decls) {
        if children.contains(&rel) {
            bodies.insert(rel, text);
        } else {
            parents.push((rel, text, decls));
        }
    }
    parents
        .into_iter()
        .map(|(rel, text, decls)| {
            let owns_a_moved_module = decls
                .iter()
                .flat_map(|d| &d.candidates)
                .any(|p| bodies.contains_key(&rel_string(p)));
            if !owns_a_moved_module {
                return (rel, text); // untouched, byte for byte
            }
            let folded = reinline_decls(&text, decls, |p| bodies.get(&rel_string(p)).cloned());
            (rel, folded)
        })
        .collect()
}
