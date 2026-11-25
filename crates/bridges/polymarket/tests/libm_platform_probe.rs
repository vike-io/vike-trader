//! The SOURCE gate that keeps a platform transcendental out of this bridge.
//!
//! IEEE 754 requires `+ - * /` and `sqrt` to be correctly rounded and requires NOTHING of `exp`, so
//! an `f64` METHOD call reaches whichever libm the PLATFORM ships — MSVC's CRT on the Windows dev
//! box, glibc on the the CI box Linux boxes and every CI runner — and the two disagree in the last bit
//! (`docs/decisions/0032-transcendentals-come-from-the-libm-crate-not-the-platform.md` is the
//! accepted verdict).
//!
//! This bridge holds NO transcendental site today. Its one site was the flow-toxicity aggregator, which
//! moved to `vike-market-making` in B3.22 (2026-11-23) together with its cross-platform hash pin
//! (`crates/vike-market-making/src/platform_probe.rs`'s `toxicity_row`, the same recorded hash). What
//! stays here is the gate that stops the NEXT edit from adding one: order signing, the CLOB REST/WS
//! planes, settlement, rewards and the fee and tick-regime tables all live in this crate, and a
//! transcendental anywhere in them would make a signed order's numbers depend on the box that made
//! them.
//!
//! ⚠ **The MINIMAL feature is `feeds`, not `polymarket`.** A default (no-feature) build of this
//! crate is EMPTY — the crate-level gate is `#![cfg(feature = "feeds")]` — so a run with no
//! `--features` compiles an empty test binary and reports `0 passed` while measuring nothing. The
//! gate reads `src/` off DISK, so it scans every `polymarket`-gated file whether or not the feature
//! compiled it.
//!
//! ⚠ **No `--ignored`.** The test below is ORDINARY, and `--ignored` runs ONLY ignored tests — so adding it
//! filters it out and the command exits 0 having measured nothing.
//!
//! ```text
//! <cargo> test -p vike-polymarket --features feeds      --test integration -- libm_platform_probe::   # minimal
//! <cargo> test -p vike-polymarket --features polymarket --test integration -- libm_platform_probe::   # CI's lane
//! ```

#![cfg(feature = "feeds")]

use vike_utility::libm_walk::{banned_needles, cfg_test_module_files_under, cfg_test_ranges};

/// Production code in this bridge must reach a transcendental through the `libm` CRATE.
///
/// The TEXT machinery this gate walks with — the banned-name list, the two needle spellings it
/// renders, and the test-item range walk — is `vike_utility::libm_walk`, shared by the eleven crates
/// carrying one of these probes (decision 0074). The GATE stays here: its `CARGO_MANIFEST_DIR`, its
/// must-scan list, its (empty) exemption table and its failure message are this crate's own, so
/// this bridge cannot be moved out from under its own determinism check. The general argument for
/// each parser rule lives with the parser; what is recorded below is what was MEASURED under THIS
/// crate's sources, which is the half a reader of this bridge came here for.
///
/// Scope: every `.rs` under `src/`, recursively, MINUS each file's test-item ranges. There are NO
/// exemptions and there should not need to be: measured 2026-08-29, `crates/bridges/polymarket/src`
/// contains not one call to any banned name outside a test item, so this gate starts at a clean
/// zero rather than at a grandfathered list. It has no `powi` to exempt either — the one entry in
/// `vike_utility::libm_walk`'s list that most often needs a per-crate exemption table (on MSVC a
/// `dev` build lowers `llvm.powi` to the CRT's `pow()`) costs this crate nothing.
///
/// # What was measured under this crate's `src/`
///
/// * **The needle set's three blind spots do not occur here.** It cannot see the angle-bracket
///   spelling, a call reached through a generic bound (`T: Float`), or a call split across two
///   lines by a `max_width` wrap. None of the three occurs under this crate's `src/` today. A list
///   of spellings is an enumeration and an enumeration is never a proof, so that is a measurement
///   with a date on it rather than a guarantee.
/// * **The RANGE walk earns its keep here, not merely in principle.** `transport/mod.rs` carries TWO test
///   markers with production code between them, and what sits between them is
///   `API_PERMITTED_WHEN_FRONTEND_BLOCKED` and `api_placement_permitted` — the geoblock policy this
///   bridge REFUSES to place orders on. A walk that stopped at the first marker would put exactly
///   that region outside this gate's view, and the `scanned_past_a_test_module` assertion below is
///   what catches it. ⚠ The offsets this measurement was originally written with (markers at 521
///   and 840 of 878 lines, 2026-08-29) had already ROTTED by the time it was re-homed here on
///   2026-09-20 — the file is 905 lines and the markers are at 542 and 867, the policy unmoved
///   between them — which is why the durable anchors are the two symbol names, not the numbers.
/// * **This crate carries the SECOND marker spelling.**
///   `crates/bridges/polymarket/src/instrument/record.rs` opens its tests with
///   `#[cfg(all(test, feature = "polymarket"))]`. A walk that knew only the bare `#[cfg(test)]`
///   spelling would treat that whole module as production and report every banned needle inside it
///   — a LOUD false positive on a file nobody touched.
/// * **This crate has a PROSE mention of the marker, so the line-by-line cut is load-bearing
///   here.** `crates/bridges/polymarket/src/instrument/tick_regime.rs` names the attribute in a comment; a
///   whole-file truncation at the first occurrence of that string anywhere would blank most of that
///   file and then report a confident zero for it.
/// * **Markers here are NOT all at column 0 — and the indentation search is exact anyway.** ⚠ The
///   harvested form of this claim read "every marker under `crates/bridges/polymarket/src` sits at
///   column 0 (verified 2026-08-29)", and re-measuring on 2026-09-20 while re-homing it found THREE
///   that do not: `feed/tap.rs`, `execution/pending_events.rs` and
///   `execution/registry.rs` each open a test module
///   nested inside another module, indented. The walk is right over them because it builds each
///   item's closing pattern from THAT marker's own indentation rather than assuming column 0, and
///   because `cargo fmt --check` is CI's first gate — every file here is rustfmt output, and
///   rustfmt closes a block with a lone brace at the block's OWN column. The structural argument
///   survives the measurement that was attached to it; only the measurement was stale.
///
/// ⚠ **The `scanned_past_a_test_module` evidence collected below is about `transport/mod.rs` AS IT IS
/// TODAY.** Move its geoblock policy above the first test module and that assertion passes
/// vacuously while the walk itself goes unproven. The walk's MECHANISM is proved separately, on a
/// planted synthetic fixture, in `crates/vike-utility/tests/libm_walk_selftest.rs` — one copy in
/// place of the per-crate fixture this file used to carry.
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
            let text = std::fs::read_to_string(&path).expect("readable source file");
            scanned.push(rel.clone());
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
                    continue; // a doc comment naming `exp` is prose, not a call
                }
                if !code.is_empty()
                    && first_marker.is_some_and(|m| i > m)
                    && !scanned_past_a_test_module.contains(&rel)
                {
                    scanned_past_a_test_module.push(rel.clone());
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
    // (`settlement/`), so a walk that stopped at the top level would still open ~45 files and
    // report a confident zero while never reading a subdirectory.
    for must in ["src/transport/mod.rs", "src/instrument/record.rs"] {
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
    // `transport/mod.rs` carries the geoblock policy between two test modules — measured 2026-08-29 at 11
    // non-blank production lines below the first marker. If this goes red, the walk has reverted to
    // treating the first marker as the end of the shipped file.
    assert!(
        scanned_past_a_test_module.iter().any(|s| s == "src/transport/mod.rs"),
        "no production line below transport/mod.rs's first test marker was scanned, so this gate \
         has gone blind to the region below every mid-file test module — which here holds \
         the geoblock policy that decides whether this bridge places an order at all. Scanned \
         past a test \
         module: {scanned_past_a_test_module:?}"
    );
    assert!(
        found.is_empty(),
        "production code must call the `libm` CRATE rather than `f64`'s platform-libm methods, so a \
         signed order's numbers are a property of its inputs rather than of the box that computed \
         them. BOTH spellings are banned, `x.exp()` and \
         `f64::exp(x)`: same inherent method, same libcall (see `banned_needles`). `powi` is banned \
         too — on MSVC a `dev` build makes it a `pow()` libcall:\n{}",
        found.join("\n")
    );
}
