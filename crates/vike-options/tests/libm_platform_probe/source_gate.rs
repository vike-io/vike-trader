//! The SOURCE half: production code reaches every transcendental through the `libm` crate.

use vike_model::libm_walk::{banned_needles, cfg_test_module_files_under, cfg_test_ranges};

// =================================================================================================
// The SOURCE half. The table above proves today's outputs agree; this stops the NEXT edit from
// reintroducing a platform call in a spot the corpus never reaches — `vol::atm_iv` above all, which
// the table declares as an uncovered gap.
// =================================================================================================

/// Production code in this crate must reach a transcendental through the `libm` CRATE.
///
/// ⚠ Not redundant with the hash table, and in this crate the gap it fills is NAMED: `vol::atm_iv`
/// has no row (see the module doc's declared gaps), so this gate is the only thing standing between
/// that forward and a respelling as `(r * t).exp()`.
///
/// The text machinery this gate walks with — `banned_needles` and `cfg_test_ranges` — is
/// `vike_model::libm_walk`, shared by all eleven probes under decision 0074, and that module's own
/// docs carry the GENERAL argument for each: why BOTH the method and the UFCS spelling are banned,
/// why the test-item cut is RANGES rather than a `break` at the first marker, why the comment
/// filter runs FIRST, and why an item's end is found by INDENTATION rather than by counting
/// braces. The walk's proof is the planted fixture at
/// `crates/vike-model/tests/libm_walk_selftest.rs`. What stays below is the per-crate EVIDENCE
/// that machinery was measured AGAINST — facts about `crates/vike-options/src` and about nothing
/// else, which is why they could not move with the parser.
///
/// Scope: every `.rs` under `src/`, recursively, MINUS each file's test-item ranges. There are NO
/// exemptions and there should not need to be: measured 2026-08-29, `crates/vike-options/src`
/// contains not one call to any `vike_model::libm_walk::BANNED_FNS` name — inside a test module or
/// outside one — so this gate starts at a clean zero rather than at a grandfathered list. The same
/// sweep found no `powi`, which is why this crate carries no `powi` exemption table even though
/// `powi` is on the banned list (MEASURED on MSVC, a `dev` build lowers `llvm.powi` to the CRT's
/// `pow()`, so it is a libcall rather than the free multiply chain its name suggests).
///
/// # ⚠ Measured on THIS crate 2026-08-29 — what the shared walk buys here, and what it does not
///
/// * **Every test item under `crates/vike-options/src` was the last thing in its file** when this
///   was measured, so a `break` at the first marker would then have changed no verdict. That
///   stopped being true on 2026-10-11: `src/chain/index.rs` keeps its two `#[cfg(test)]` instrument
///   builders (`test_option`, `test_delta_one`) ABOVE its `mod tests;` declaration, and the ranges
///   are what keep a scan from going blind below them. The property they buy is that a mid-file
///   test item never makes this gate silently stop reading everything below it while still
///   reporting a confident zero. It is also why this crate can state no tree-shaped
///   "some production line below a marker was scanned" assertion — that sentence is FALSE here and
///   could only be written by weakening it into something vacuous, so the shared fixture is the
///   only proof of the walk there is.
/// * **Every marker under `crates/vike-options/src` sits at column 0**, which is what makes the
///   indentation-based end-finding exact against THESE files rather than merely plausible.
/// * **None of the three spellings the needle list cannot see occurs under this crate's `src/`
///   today** — `<f64>::exp(x)` (the angle-bracket form), a call reached through a generic bound
///   (`T: Float`), and a call split across two lines by a `max_width` wrap. A list of spellings is
///   an enumeration and an enumeration is never a proof, which is why the shared module leaves
///   this sentence to each crate instead of claiming it for all of them.
/// * **`sqrt` is deliberately NOT banned** — IEEE 754 requires it correctly rounded, so it is
///   identical on every box and there is nothing to convert it TO. This crate leans on that
///   exemption at three named sites and says so at each: `d1_d2`'s `t.sqrt()`, `norm_pdf`'s
///   `(2·π).sqrt()`, and `TerminalDist::log_params`' `tau.sqrt()`.
#[test]
fn production_code_calls_libm_not_the_platform() {
    let banned = banned_needles();
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let src = root.join("src");
    let mut found = Vec::new();
    let mut scanned: Vec<String> = Vec::new();
    // A test item whose end could not be located swallows the rest of its file exactly the way a
    // `break` would. That is the one QUIET failure of the range walk, so it is collected and
    // asserted rather than absorbed.
    let mut unclosed: Vec<String> = Vec::new();
    // A file a `#[cfg(test)] mod NAME;` declaration pulls in is test code WHOLE, with no marker
    // inside it for `cfg_test_ranges` to find — skipped before it is read, like any test module.
    let test_files = cfg_test_module_files_under(&src);
    let mut stack = vec![src];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).expect("crate has a src/ directory") {
            let path = entry.expect("readable dir entry").path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            if path.extension().and_then(|e| e.to_str()) != Some("rs") {
                continue;
            }
            if test_files.contains(&path) {
                continue;
            }
            // `\` -> `/` so a reported path reads the same on the Windows dev box and on Linux.
            let rel = path.strip_prefix(root).unwrap_or(&path).to_string_lossy().replace('\\', "/");
            let name = path.file_name().unwrap().to_string_lossy().to_string();
            let text = std::fs::read_to_string(&path).expect("readable source file");
            scanned.push(name);
            let lines: Vec<&str> = text.lines().collect();
            let test_items = cfg_test_ranges(&lines);
            for &(start, _, closed) in &test_items {
                if !closed {
                    unclosed.push(format!("  {rel}:{}", start + 1));
                }
            }
            for (i, line) in lines.iter().enumerate() {
                if test_items.iter().any(|&(start, end, _)| i >= start && i < end) {
                    continue; // inside a test item — not shipped
                }
                let code = line.trim_start();
                if code.starts_with("//") {
                    continue; // a doc comment naming `exp` is prose, not a call
                }
                for pat in &banned {
                    if line.contains(pat.as_str()) {
                        found.push(format!("  {rel}:{} {code}", i + 1));
                    }
                }
            }
        }
    }
    // Non-vacuity: an empty `found` is indistinguishable from a scan that opened nothing. The five
    // named files are the ones carrying a `libm::` call today — `vol.rs` above all, because it is
    // the file with the row-less site.
    for must in ["greeks.rs", "vol.rs", "basket.rs", "prob.rs", "second_order.rs"] {
        assert!(
            scanned.iter().any(|s| s == must),
            "the scan never opened {must}, so an empty result proves nothing about the files this \
             gate exists for. Scanned {} files.",
            scanned.len()
        );
    }
    assert!(
        unclosed.is_empty(),
        "a test item's closing line could not be located, so everything below it went UNSCANNED — \
         the exact hole a `break` leaves, wearing a different cause. See `cfg_test_ranges`: the end \
         is the item's own indentation followed by a lone `}}`, or a `;` for a `mod NAME;` \
         declaration.\n{}",
        unclosed.join("\n")
    );
    assert!(
        found.is_empty(),
        "production code must call the `libm` CRATE rather than `f64`'s platform-libm methods. This \
         crate's own greeks module doc states the trap: a `libm::erf` two lines above an `x.ln()` \
         makes a HALF-converted file read as done. BOTH spellings are banned, `x.exp()` and \
         `f64::exp(x)`: same inherent method, same libcall (see `banned_needles`). `sqrt` is \
         deliberately NOT banned — `d1_d2`, `norm_pdf` and `TerminalDist::log_params` each rely on \
         that exemption and each says so at the line:\n{}",
        found.join("\n")
    );
}
