//! **The platform-invariance probe for this crate — the SOURCE half only, and the omission of the
//! other half is argued rather than silent.**
//!
//! `docs/decisions/0032-transcendentals-come-from-the-libm-crate-not-the-platform.md`: IEEE 754
//! requires `+ - * /` and `sqrt` to be correctly rounded and NOTHING of `ln`, `exp`, `pow` or their
//! relatives, so `f64::ln` resolves to the PLATFORM's libm — MSVC's CRT on the Windows dev box,
//! glibc on the Linux CI boxes — and the two disagree in the last bit. The cure is the `libm`
//! CRATE, compiled into the binary so both boxes run one implementation.
//!
//! ⚠ **Why this file exists: the coverage MOVED with the code.** `math.rs` was the `math` child of
//! vike-model's `util` module until 2026-10-12, inside the `src/` tree that
//! `crates/vike-model/tests/libm_platform_probe.rs`'s `production_code_calls_libm_not_the_platform`
//! walks. When the helper floor became this crate, no probe walked these files any more, so a
//! `.exp()` written here next week would have shipped into the options pricing, the Bachelier
//! `p_up` and BVC VPIN unseen. This is that walk, over this crate's `src/`.
//!
//! # No RUNTIME table here, on purpose
//!
//! The sibling probes hash each converted function over a sampled corpus. This crate has exactly
//! one transcendental, `math.rs`'s `norm_cdf` (`libm::erf`), and its output BITS are already
//! pinned: `crates/vike-util/src/math_tests.rs`'s `norm_cdf_bits_are_pinned` holds twelve recorded
//! bit patterns (the tails, the saturations and ½ included), and the corpus rows that hash it from
//! its callers stay where they were: the `p_up` row in
//! `crates/vike-model/tests/libm_platform_probe.rs` and the Black–Scholes rows in
//! `crates/vike-options/tests/libm_platform_probe/rows.rs`. A third corpus over the same `erf`
//! would be maintenance for nothing. What a bit pin CANNOT give is the SOURCE SCAN below: it stops
//! the NEXT edit from reintroducing a platform call in a spot no corpus reaches.
//!
//! # The structural facts the scan rests on, MEASURED on this crate (2026-10-12)
//!
//! * `src/` is FLAT (no sub-directory) and holds 13 files: `lib.rs`, six modules and their six
//!   `*_tests.rs` children.
//! * All **6** `#[cfg(test)]` markers sit at column 0, each in the header form
//!   `#[cfg(test)] #[path = "x_tests.rs"] mod tests;` as its file's LAST item. There is no inline
//!   test module, so no production line sits below a marker, and the six `*_tests.rs` files are
//!   skipped whole by the shared walk.
//! * No `#[cfg(any(test, feature = "…"))]` gate exists here (that spelling SHIPS with its feature
//!   on, and the walk deliberately reads it as production).
//!
//! ⚠ Re-measure all three if a file here ever gets a sub-directory or a mid-file test module.
//!
//! ```text
//! <cargo> test -p vike-util --test libm_platform_probe
//! ```

use vike_model::libm_walk::{banned_needles, cfg_test_module_files_under, cfg_test_ranges};

/// Production code in this crate must reach a transcendental through the `libm` CRATE: these
/// helpers are called from the options pricing, the posted-quote probability and the order-flow
/// studies, and a platform call here would make every one of them depend on which box ran it. This
/// gate reads text, not the feature-resolved crate graph.
///
/// # The shared parser's three per-crate facts (decision 0074), measured here on 2026-10-12
///
/// ⚠ **The needle set's blind spots, none occupied here.** `x.exp()` and `f64::exp(x)` are both
/// covered; three spellings reach the platform unseen: the angle-bracket form (`f64>::exp(`), a
/// call through a generic bound (`T: Float`, `num_traits::Float::exp`), and a call split across two
/// lines by a `max_width` wrap. **None occurs under this crate's `src/` today** — an enumeration,
/// never a proof.
///
/// ⚠ **The second marker spelling has no instance here, and is carried anyway.** The walk
/// recognises `#[cfg(test)]` and `#[cfg(all(test, …))]`, NOT `#[cfg(any(test, feature = "…"))]`
/// (which SHIPS when its feature is on). Keeping the shared walk means the first feature-gated test
/// module here is not read as production.
///
/// ⚠ **The indentation walk's precondition holds here.** An item's end is its own indentation
/// followed by a lone `}`, or the `;` of a `mod NAME;` header — exact only because every file is
/// rustfmt output. Every marker under `src/` is the header form, so the `closed` assertion below is
/// all that stands between a hand-formatted item and a silent skip.
///
/// Scope: every `.rs` under `src/`, recursively, MINUS each file's `#[cfg(test)]` item ranges and
/// the files a `#[cfg(test)] mod NAME;` declaration pulls in. No exemption table: there is no
/// `powi` or other banned call in production here to exempt.
#[test]
fn production_code_calls_libm_not_the_platform() {
    let banned = banned_needles();
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let src = root.join("src");
    let mut found = Vec::new();
    let mut scanned: Vec<String> = Vec::new();
    // A `#[cfg(test)]` item whose end cannot be located swallows the rest of its file: the range
    // walk's one QUIET failure, so it is collected and asserted.
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
                    continue; // inside a `#[cfg(test)]` item — not shipped
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
    // Non-vacuity, first half: the walk opened the file this gate exists for (`math.rs`, the one
    // `libm` site) and its neighbours, so an empty result is a statement about them.
    for must in ["math.rs", "calendar.rs", "fnv.rs", "atomic_file.rs", "lib.rs"] {
        assert!(
            scanned.iter().any(|s| s == must),
            "the scan never opened {must}, so an empty result proves nothing about the files this \
             gate exists for. Scanned {} files: {scanned:?}",
            scanned.len()
        );
    }
    // Non-vacuity, second half: the six `*_tests.rs` children were SKIPPED, which is what proves
    // the shared walk still derives `#[path]` test files here. Read as production, a test that
    // spells a banned call as a reference value (none does today) would fail this gate for the
    // wrong reason — and a derivation that stopped finding them would go unnoticed until then.
    let read_as_production: Vec<&String> =
        scanned.iter().filter(|s| s.ends_with("_tests.rs")).collect();
    assert!(
        read_as_production.is_empty() && test_files.len() >= 6,
        "the `#[cfg(test)] #[path = \"x_tests.rs\"] mod tests;` children must be skipped whole: \
         derived {} test file(s), and read {read_as_production:?} as production",
        test_files.len()
    );
    assert!(
        unclosed.is_empty(),
        "a `#[cfg(test)]` item's closing line could not be located, so everything below it went \
         UNSCANNED — the exact hole a `break` leaves, wearing a different cause. See \
         `cfg_test_ranges`: the end is the item's own indentation followed by a lone `}}`, or a \
         `;` for a `mod NAME;` declaration.\n{}",
        unclosed.join("\n")
    );
    // ⚠ Deliberately NO "a production line below a test module was scanned" assertion: measured
    // 2026-10-12, every file's `#[cfg(test)]` module is its LAST item, so it would be FALSE here.
    // The mechanism is held by the shared walk's planted fixture,
    // `crates/vike-model/tests/libm_walk_selftest.rs`'s
    // `the_test_module_cut_excludes_only_the_test_module` (one copy since decision 0074).
    assert!(
        found.is_empty(),
        "production code must call the `libm` CRATE rather than `f64`'s platform-libm methods: \
         these helpers feed the options pricing (`norm_cdf`), the posted-quote probability \
         (`vike_model::fair::p_up`) and BVC VPIN, whose results are compared BIT FOR BIT. BOTH \
         spellings are banned, `x.exp()` and `f64::exp(x)`: same inherent method, same libcall \
         (see `banned_needles`). `powi` is banned too — on MSVC a `dev` build makes it a `pow()` \
         libcall — and its cure is `libm::pow` for a runtime base or a MULTIPLICATION for a \
         literal one:\n{}",
        found.join("\n")
    );
}
