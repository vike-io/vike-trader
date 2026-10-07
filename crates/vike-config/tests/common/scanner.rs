//! The scanner: ONE notion of production code, shared by every direction of the gate.
#![allow(dead_code)] // each test binary compiles this file and uses a subset of it

use std::path::{Path, PathBuf};

use vike_model::libm_walk::cfg_test_module_files_under;

// ---------------------------------------------------------------------------------------------
// the scanner — ONE notion of production code, shared by every direction above
// ---------------------------------------------------------------------------------------------

/// Which test ITEM a `#[cfg(test)]`-family attribute guards, and where that item sits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum TestItem {
    /// `#[cfg(test)] mod name;` — the body is a sibling FILE and is not in this text at all, so
    /// only the declaration is stepped over.
    OutOfLine {
        /// 0-indexed line of the `mod name;` declaration.
        at: usize,
    },
    /// `#[cfg(test)] mod name { … }` — a module boundary: its whole body is test code.
    Inline {
        /// 0-indexed line the `mod name` opens on.
        at: usize,
    },
}

/// The PRODUCTION lines of a Rust source — what a NON-test build compiles, minus comment and
/// attribute lines — each paired with its 1-indexed number in the ORIGINAL file.
///
/// Every direction in this file goes through here, and that is the point rather than tidiness:
/// [`Reader::Live`] and [`Reader::Uncalled`] are supposed to be each other's opposites, so a
/// caller that counts for one and not the other puts a row in the gap between them.
///
/// ⚠ **This replaced a one-line rule that disabled the gate over most of the tree, and the shape
/// of that failure is why the rule is now spelled out.** Each scan used to stop at the FIRST line
/// whose trimmed text starts with `#[cfg(test)]`, on the stated assumption that a test module sits
/// at the bottom of its file. But `#[cfg(test)]` is an attribute on an ITEM, not a module marker,
/// and `#[cfg(test)] use`, `#[cfg(test)] fn` and `#[cfg(test)] const` all wear the identical
/// spelling while cutting nothing. `crates/vike-tradehub/src/tradehub_cli.rs` carries
/// `#[cfg(test)] use crate::feeds::CexBars;` about 300 lines into a 7,684-line file, so the whole
/// of the daemon's live mount — every `FoldTier` row, `make_engine`, `spawn_recon` — was invisible
/// to the no-caller search. A/B-measured against the real gate binary: one planted
/// `AutoRedeemPoller::spawn(` call read GREEN 1,700 lines below that `use` and RED above it.
/// Tree-wide, 823 `crates/**/src/*.rs` files carry a `#[cfg(test)]` and 231,897 lines sat after
/// the first one — MEASURED, and re-derivable: for each tracked `src/` file (outside the vendored
/// `ibapi` copy that still sat in the tree when this was measured), the first line matching
/// `^\s*#\[cfg(test)\]` subtracted from that file's line count, summed.
/// [`the_scanner_reads_past_a_cfg_test_attribute_in_a_real_file`] is the A/B proof kept as a test,
/// over the real file rather than a synthetic string.
///
/// So the cut is at a test MODULE and at nothing else:
///
/// * `#[cfg(test)] mod name { … }` — skipped, body and all, and the scan RESUMES after it. The end
///   of the body is the first line equal to the module's own indentation plus `}`, which is exact
///   because `cargo fmt --check` is this repo's first CI gate; a module that never closes that way
///   runs to EOF, which is the old behaviour and therefore no worse.
/// * `#[cfg(test)] mod name;` — one declaration line stepped over
///   (`crates/vike-ops/tests/docs/docs_constants_gate/source_reading.rs`'s `code_only` is where this shape was already
///   solved, and this is deliberately the same answer rather than a second one).
/// * anything else the attribute guards — `use`, `fn`, `const`, `impl` — is NOT a boundary and the
///   scan continues straight through it.
///
/// ⚠ **Two residuals, declared.** A `#[cfg(test)] fn` body IS scanned, so a settings key written
/// inside one reads as production — that fails LOUDLY (a red gate naming the line) rather than
/// silently, which is the direction this file has to fail in. And the cfg predicate is read
/// literally: `#[cfg(test)]` and `#[cfg(all(test, …))]` are test-only, while
/// `#[cfg(any(test, feature = "test-support"))]` is NOT, because a `test-support` build ships it.
pub(super) fn production_lines(source: &str) -> Vec<(usize, &str)> {
    let lines: Vec<&str> = source.lines().collect();
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < lines.len() {
        match test_item_at(&lines, i) {
            Some(TestItem::OutOfLine { at }) => {
                i = at + 1;
                continue;
            }
            Some(TestItem::Inline { at }) => {
                i = end_of_block(&lines, at);
                continue;
            }
            None => {}
        }
        let trimmed = lines[i].trim_start();
        // A comment line, a block-comment continuation, or an attribute: none of them is the
        // program reading a setting or calling an entry point.
        if !(trimmed.starts_with("//") || trimmed.starts_with('*') || trimmed.starts_with('#')) {
            out.push((i + 1, trimmed));
        }
        i += 1;
    }
    out
}

/// The test ITEM a test-only `cfg` attribute on line `i` guards, or `None` when line `i` is not
/// such an attribute or the item it guards is not a module.
///
/// The item may sit on the attribute's own line (`#[cfg(test)] mod tests;`) or below it, past
/// further attributes, comments and blank lines — `tradehub_cli.rs` spells a `#[path = "…"]`
/// between the two, which is why the look-ahead skips attributes rather than demanding that the
/// very next line be the item.
pub(super) fn test_item_at(lines: &[&str], i: usize) -> Option<TestItem> {
    let rest = strip_test_only_cfg(lines[i].trim_start())?;
    let (at, item) = if rest.trim().is_empty() {
        let mut j = i + 1;
        loop {
            let t = lines.get(j)?.trim();
            if t.is_empty() || t.starts_with("//") || t.starts_with('*') || t.starts_with('#') {
                j += 1;
                continue;
            }
            break (j, t);
        }
    } else {
        (i, rest.trim())
    };
    // `pub mod` is not how a test module is written, but stripping visibility costs two lines and
    // removes a way for this to answer `None` about something that IS one.
    let item = item.strip_prefix("pub ").unwrap_or(item).trim_start();
    let item = match item.find(')') {
        Some(p) if item.starts_with("pub(") => item[p + 1..].trim_start(),
        _ => item,
    };
    if !item.starts_with("mod ") {
        return None;
    }
    if item.ends_with(';') {
        Some(TestItem::OutOfLine { at })
    } else {
        Some(TestItem::Inline { at })
    }
}

/// The text after a TEST-ONLY `cfg` attribute at the start of `line`, or `None` when `line` does
/// not open with one.
///
/// The predicate is matched with paren counting rather than a `find(")]")`, because a nested
/// predicate's first `)]` is not the attribute's — `#[cfg(all(test, not(fcsdk)))]` was a real
/// spelling in this tree until 2026-09-09 (#1720 deleted the `fcsdk` cfg).
pub(super) fn strip_test_only_cfg(line: &str) -> Option<&str> {
    let rest = line.strip_prefix("#[cfg(")?;
    let mut depth = 1usize;
    let mut end = None;
    for (idx, ch) in rest.char_indices() {
        match ch {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    end = Some(idx);
                    break;
                }
            }
            _ => {}
        }
    }
    let end = end?;
    let after = rest.get(end + 1..)?.strip_prefix(']')?;
    is_test_only(&rest[..end]).then_some(after)
}

/// `true` when a `cfg` predicate can hold ONLY in a test build.
///
/// `all(test, …)` qualifies — every conjunct must hold and `test` is one of them. `any(test, …)`
/// deliberately does not: `#[cfg(any(test, feature = "test-support"))]` guards modules a
/// `test-support` build SHIPS, and treating those as test code would narrow the scan for no
/// reason. Erring towards scanning more is the safe direction here; erring the other way is the
/// defect this whole helper exists to repair.
fn is_test_only(pred: &str) -> bool {
    let p = pred.trim();
    if p == "test" {
        return true;
    }
    p.strip_prefix("all(")
        .and_then(|s| s.strip_suffix(')'))
        .is_some_and(|inner| inner.split(',').any(|t| t.trim() == "test"))
}

/// The 0-indexed line AFTER the block opening on line `at` — the first line equal to that line's
/// own indentation followed by `}`, or EOF when there is none.
fn end_of_block(lines: &[&str], at: usize) -> usize {
    let open = lines[at];
    // A one-line module (`mod tests {}`) closes on its own line.
    if open.contains('{') && open.matches('{').count() == open.matches('}').count() {
        return at + 1;
    }
    let indent: String = open.chars().take_while(|c| c.is_whitespace()).collect();
    let closing = format!("{indent}}}");
    for (j, line) in lines.iter().enumerate().skip(at + 1) {
        if *line == closing {
            return j + 1;
        }
    }
    lines.len()
}

/// A call site for `entry` in `source`, 1-indexed, or `None`.
///
/// Requires the `(` so a doc reference (``[`AutoRedeemPoller::spawn`]``), a re-export or a prose
/// mention is not mistaken for a call; everything else about what counts as code is
/// [`production_lines`]'s job, shared with [`contains_code_in`] so the two [`Reader`] variants
/// cannot disagree about what a caller is.
pub(super) fn call_line(source: &str, entry: &str) -> Option<usize> {
    let needle = format!("{entry}(");
    production_lines(source).into_iter().find(|(_, l)| l.contains(&needle)).map(|(n, _)| n)
}

/// The bare name of a PUBLIC FREE function from a [`Reader`] `needle`, or `None` when the needle is
/// not a `pub fn` signature (a private one, or a parameter's gate such as `if !enabled {`).
///
/// `pub fn kill_switch_tripped(halted: bool, halt_file: &Path) -> bool` -> `kill_switch_tripped`.
/// A needle that is not `pub` yields `None` deliberately — see the call site for why a private
/// read is neither checkable nor in need of checking.
pub(super) fn public_free_fn_name(needle: &str) -> Option<&str> {
    let rest = needle.strip_prefix("pub fn ")?;
    let name = rest.split('(').next()?.trim();
    (!name.is_empty() && name.chars().all(|c| c.is_alphanumeric() || c == '_')).then_some(name)
}

/// A call to the free function `name` in `source`, 1-indexed, or `None`.
///
/// Word-boundary-checked on the LEFT: `journal_env_snapshot(` must not read as a call to
/// `env_snapshot`, and `redeem_kill_switch_tripped(` must not read as one to
/// `kill_switch_tripped`. A `::` before the name is fine and is the normal spelling from another
/// crate (`vike_polymarket::kill_switch_tripped(`).
pub(super) fn free_fn_call_line(source: &str, name: &str) -> Option<usize> {
    let needle = format!("{name}(");
    for (n, line) in production_lines(source) {
        let mut from = 0usize;
        while let Some(off) = line[from..].find(&needle) {
            let at = from + off;
            let prev = line[..at].chars().next_back();
            if !prev.is_some_and(|c| c.is_alphanumeric() || c == '_') {
                return Some(n);
            }
            from = at + 1;
        }
    }
    None
}

/// `true` when `source` holds `needle` in PRODUCTION code — the same rule [`call_line`] applies,
/// and that sharing is the fix for a second hole: this used to skip comment lines only, so a
/// [`Reader::Live`] row could be satisfied by a caller existing solely inside the file's own
/// `#[cfg(test)] mod tests`, making "the environment variable still works" true of test builds
/// alone. Mutation-proved both ways by
/// [`a_reader_live_caller_inside_a_test_module_does_not_count`].
pub(super) fn contains_code_in(source: &str, needle: &str) -> bool {
    production_lines(source).iter().any(|(_, l)| l.contains(needle))
}

/// [`contains_code_in`] over a repo-relative path; a missing file is `false`.
pub(super) fn contains_code(root: &Path, rel: &str, needle: &str) -> bool {
    let path = root.join(rel.replace('/', std::path::MAIN_SEPARATOR_STR));
    std::fs::read_to_string(path).is_ok_and(|s| contains_code_in(&s, needle))
}

/// Repo-relative, forward-slashed — the spelling every table in this file is keyed on.
pub(super) fn rel_path(root: &Path, path: &Path) -> String {
    path.strip_prefix(root).unwrap_or(path).to_string_lossy().replace('\\', "/")
}

/// A repo-relative path that holds TEST code rather than program code: an integration-test
/// directory, a benchmark, or an example. A setting read in one of these is a test exercising the
/// loader, never the program acting on the value.
pub(super) fn is_test_path(rel: &str) -> bool {
    rel.contains("/tests/") || rel.contains("/benches/") || rel.contains("/examples/")
}

/// Every `.rs` file under `dir`, minus build output and any `vendor` directory (the gitignored
/// repo-root one; the committed `ibapi` copy that also bore the name is gone).
pub(super) fn rust_sources(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&d) else { continue };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                let name = path.file_name().unwrap_or_default().to_string_lossy().to_string();
                if name == "target" || name == "vendor" {
                    continue;
                }
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "rs") {
                out.push(path);
            }
        }
    }
    // A `#[cfg(test)] mod NAME;` module that lives in its own file is TEST code, the same as the
    // inline module [`production_lines`] steps over — a caller there is `cargo test`'s, not the
    // program's — so the file is left out whole.
    let test_files = cfg_test_module_files_under(dir);
    out.retain(|p| !test_files.contains(p));
    out
}
