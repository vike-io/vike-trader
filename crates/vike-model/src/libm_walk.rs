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
//! [`cfg_test_ranges`]'s own `TEST_ITEM_ATTRS` does not recognise the
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
