//! The SOURCE half of this crate's transcendental-portability probe: production code here must
//! reach a transcendental through the `libm` CRATE, never through `f64`'s platform-libm methods.
//!
//! IEEE 754 requires `+ - * /` and `sqrt` to be correctly rounded and requires NOTHING of `log10`
//! or `pow`, so an `f64` METHOD call reaches whichever libm the PLATFORM ships — MSVC's CRT on the
//! Windows dev box, glibc on the the CI box Linux boxes and every CI runner.
//! `docs/decisions/0032-transcendentals-come-from-the-libm-crate-not-the-platform.md` is the verdict.
//!
//! ⚠ **The corpus pin that used to open this file moved out on 2026-09-27**, together with the one
//! function it pinned: `nice_orderflow_tick` now lives in `crates/vike-orderflow/src/bar_agg.rs`,
//! and `crates/vike-orderflow/tests/libm_platform_probe.rs` carries its hash table. What stays here
//! is the walk, because this crate is still the GUI's whole non-rendering half and a new
//! transcendental anywhere in it should go red.
//!
//! ```text
//! <cargo> test -p vike-app-core --test libm_platform_probe
//! ```

use vike_model::libm_walk::{banned_needles, cfg_test_module_files_under, cfg_test_ranges};

/// Production code in this crate must reach a transcendental through the `libm` CRATE.
///
/// Scope: every `.rs` under `src/`, recursively, MINUS each file's test-item ranges. There are NO
/// exemptions and there should not need to be: measured 2026-08-29 and again 2026-09-27 (after
/// `orderflow.rs` left for `vike-orderflow`), `crates/vike-app-core/src` contains not one call to
/// any `vike_model::libm_walk::BANNED_FNS` name outside a test item, and — since that move — no
/// `libm` CRATE site either.
///
/// # ⚠ The per-crate evidence this gate rests on
///
/// The text machinery is `vike_model::libm_walk` since decision 0074, and the GENERAL argument for
/// each of its rules lives in its doc comments there. What the shared module deliberately does NOT
/// carry is the measurement that makes a rule BITE in a particular crate. This crate's, measured
/// 2026-08-29 over `crates/vike-app-core/src`:
///
/// * **Why RANGES rather than a `break` at the first marker is a LIVE difference here, not
///   insurance.** A `break` says "the shipped half of a file ends at its first test module", which
///   holds only when that module is LAST. This crate is the workspace's densest counter-example:
///   `state.rs` carries NINE test modules, `tools.rs` seven, `feed_lifecycle.rs` six, `core_sync.rs`
///   five, and `tool_views/stored.rs` puts `pub fn seed_polymarket_proxy_box` between two markers.
///   A `break` would hand this gate a confident zero over most of five files.
///   `scanned_past_a_test_module` below is what holds that shut.
/// * **Why the comment filter must run BEFORE the cut.** A whole-file `text.find("#[cfg(test)]")`
///   truncates at the first occurrence of that string ANYWHERE, a doc comment included, and a
///   `//`-skipping filter applied afterwards cannot rescue it because it runs on already-truncated
///   text.
/// * **Why the INDENTATION rule is exact over THESE sources.** An item's end is its own indentation
///   followed by a lone `}`, which needs every file to be rustfmt output. Every marker under
///   `crates/vike-app-core/src` sits at column 0.
/// * **What the needle list still cannot see, over THESE sources.** `<f64>::log10(x)` (the
///   angle-bracket spelling), a call reached through a generic bound (`T: Float`), and a call split
///   across two lines by a `max_width` wrap. None occurs under this crate's `src/` today; a list of
///   spellings is an enumeration, never a proof.
/// * **Where the parser's own mechanism is proved.** The planted fixture lives once, at
///   `crates/vike-model/tests/libm_walk_selftest.rs`. That fixture proves the WALK, while
///   `scanned_past_a_test_module` below is evidence about the TREE AS IT IS TODAY — move
///   `stored.rs`'s `seed_polymarket_proxy_box` above its first test module and this assertion
///   passes vacuously while the mechanism goes unproven here.
#[test]
fn production_code_calls_libm_not_the_platform() {
    let banned = banned_needles();
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let src = root.join("src");
    let mut found = Vec::new();
    let mut scanned: Vec<String> = Vec::new();
    // Files where at least one NON-BLANK production line BELOW the first marker was scanned — the
    // property a `break` makes impossible. ⚠ Non-blank matters: two adjacent test modules are
    // separated by an empty line, and counting that line would make this assertion pass on files
    // that carry no production code below a marker at all.
    let mut scanned_past_a_test_module: Vec<String> = Vec::new();
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
            scanned.push(name.clone());
            let lines: Vec<&str> = text.lines().collect();
            let test_items = cfg_test_ranges(&lines);
            for &(start, _, closed) in &test_items {
                if !closed {
                    unclosed.push(format!("  {rel}:{}", start + 1));
                }
            }
            let first_marker = test_items.first().map(|&(start, _, _)| start);
            for (i, line) in lines.iter().enumerate() {
                if test_items.iter().any(|&(start, end, _)| i >= start && i < end) {
                    continue; // inside a test item — not shipped
                }
                let code = line.trim_start();
                if code.starts_with("//") {
                    continue; // a doc comment naming `log10` is prose, not a call
                }
                if !code.is_empty()
                    && first_marker.is_some_and(|m| i > m)
                    && !scanned_past_a_test_module.contains(&name)
                {
                    scanned_past_a_test_module.push(name.clone());
                }
                for pat in &banned {
                    if line.contains(pat.as_str()) {
                        found.push(format!("  {rel}:{} {code}", i + 1));
                    }
                }
            }
        }
    }
    // Non-vacuity, aimed at the failure this scan is most likely to have: `src/` NESTS here
    // (`tool_views/`, `workspace/`), so a walk that stopped at the top level would still open
    // dozens of files and report a confident zero while never reading a subdirectory.
    for must in ["feed_lifecycle.rs", "stored.rs", "state.rs"] {
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
    // The non-vacuity assertion for the RANGE walk, and the one a `break` could not pass.
    // `tool_views/stored.rs` carries `pub fn seed_polymarket_proxy_box` between two test modules. If
    // this goes red, the walk has reverted to treating the first marker as the end of the file.
    assert!(
        scanned_past_a_test_module.iter().any(|s| s == "stored.rs"),
        "no production line below stored.rs's first test marker was scanned, so this gate has gone \
         blind to the region below every mid-file test module — and this crate has five files with \
         several. Scanned past a test module: {scanned_past_a_test_module:?}"
    );
    assert!(
        found.is_empty(),
        "production code must call the `libm` CRATE rather than `f64`'s platform-libm methods \
         (docs/decisions/0032): a platform libm may disagree in the last bit across the Windows dev \
         box and the Linux boxes. BOTH spellings are banned, `x.log10()` and `f64::log10(x)`: same \
         inherent method, same libcall (see `banned_needles`). `powi` is banned too — on MSVC a \
         `dev` build makes it a `pow()` libcall:\n{}",
        found.join("\n")
    );
}
