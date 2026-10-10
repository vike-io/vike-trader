//! **The platform-invariance probe for this crate — the SOURCE half only, and the omission of the
//! other half is argued rather than silent.**
//!
//! `docs/decisions/0032-transcendentals-come-from-the-libm-crate-not-the-platform.md`: IEEE 754
//! requires `+ - * /` and `sqrt` to be correctly rounded and NOTHING of `ln`, `exp`, `pow` or their
//! relatives, so `f64::ln` resolves to the PLATFORM's libm — MSVC's CRT on the Windows dev box,
//! glibc on the Linux CI boxes — and the two disagree in the last bit. The cure is the `libm`
//! CRATE, compiled into the binary so both boxes run one implementation.
//!
//! # No RUNTIME table here, on purpose
//!
//! The sibling probes hash each converted function over a sampled corpus. This crate has exactly
//! one converted transcendental, `trailing_sigma`, already pinned BIT-FOR-BIT across platforms by
//! `crates/vike-strategy/tests/cheap_np_parity.rs`'s
//! `trailing_sigma_matches_the_python_oracle_bit_for_bit` (six committed `sigma_bits`, neither
//! `#[ignore]`d nor cfg-gated), so a second corpus would be maintenance for nothing. What a bit pin
//! CANNOT give is the SOURCE SCAN below: it stops the NEXT edit from reintroducing a platform call
//! in a spot no corpus reaches.
//!
//! # The two structural assumptions, MEASURED on this crate
//!
//! The scan cuts each file at its `#[cfg(test)]` marker and treats everything below as test code.
//! Both properties that makes safe were measured over `crates/vike-strategy/src` after the registry and executor splits:
//!
//! * all **18** `#[cfg(test)]` markers (13 files) sit at column 0 (zero are indented), so a column-0 match is
//!   not missing an inner module;
//! * in the **3** files with an INLINE test module (`cheap_np_ask.rs`, `funding_capture.rs`, `grid_dca.rs`) its
//!   closing brace is the LAST non-empty line, so the coarse cut hides no production code; the other **10**
//!   carry only the `#[cfg(test)] mod NAME;` header form, whose file the shared walk skips whole.
//!
//! ⚠ Re-measure both if a file here ever gets a test module mid-file.

use vike_model::libm_walk::{banned_needles, cfg_test_module_files_under, cfg_test_ranges};

/// The ONE production line under `crates/vike-strategy/src` that keeps a `powi`, as
/// `(crate-relative path, the substring that must appear on that line)`.
///
/// ⚠ Keyed on a PATH plus the exempted TEXT, never a path alone: a whole-file skip would stop
/// covering every OTHER line of `sport_taker.rs`. Moving the call keeps it exempt; a NEW `.exp()`
/// three lines below it is still caught. Reformat the line and the exemption stops matching.
///
/// A MEASUREMENT, not a preference: decision 0032's 2026-08-26 amendment classifies a power by its
/// BASE. `10f64.powi(n)` with a runtime integer exponent, swept on the Windows dev box (MSVC) and
/// the Linux CI runners (glibc), came back BIT-IDENTICAL for every `n` in `-22..=22` — hash
/// `a87aa0b0905b4eac` on both — because powers of ten in that range are exact in `f64`.
/// `round_dp`'s callers pass `dp` of 4 and 6; `libm::pow(10.0, dp as f64)` would buy nothing.
const POWI_BASE_TEN_EXEMPT: [(&str, &str); 1] =
    [("src/strategies/sport_taker.rs", "let f = 10f64.powi(dp as i32);")];

/// Production code in this crate must reach a transcendental through the `libm` CRATE: a new call
/// site in any file would be invisible to every corpus while making a fill price depend on which
/// box ran the replay. This gate reads text, not the feature-resolved crate graph.
///
/// # The shared parser's three per-crate facts (decision 0074), measured here on 2026-09-20
///
/// ⚠ **The needle set's blind spots, none occupied here.** `x.exp()` and `f64::exp(x)` are both
/// covered; three spellings reach the platform unseen: the angle-bracket form (`f64>::exp(`), a
/// call through a generic bound (`T: Float`, `num_traits::Float::exp`), and a call split across two
/// lines by a `max_width` wrap. **None occurs under this crate's `src/` today** — an enumeration,
/// never a proof.
///
/// ⚠ **The second marker spelling has no instance here, and is carried anyway.** The walk
/// recognises `#[cfg(test)]` and `#[cfg(all(test, …))]`, NOT `#[cfg(any(test, feature = "…"))]`
/// (which SHIPS when its feature is on). Keeping it means the first feature-gated test module here
/// is not read as production.
///
/// ⚠ **The indentation walk's precondition holds here.** An item's end is its own indentation
/// followed by a lone `}`, exact only because every file is rustfmt output. Every test marker
/// under `src/` sat at column 0 followed by a line ending in `{` (no `#[cfg(test)] mod NAME;`
/// header form), so the `closed` assertion below is all that stands between a hand-formatted item
/// and a silent skip.
///
/// Scope: every `.rs` under `src/`, recursively, MINUS each file's `#[cfg(test)]` item ranges,
/// MINUS the [`POWI_BASE_TEN_EXEMPT`] lines.
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
    // Which exemptions actually matched. A stale exemption is an un-gated line waiting to happen.
    let mut used_exempt: Vec<&str> = Vec::new();
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
            // `\` -> `/` so an exemption can be written one way and still match on the Windows box.
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
                if let Some((_, needle)) =
                    POWI_BASE_TEN_EXEMPT.iter().find(|(p, n)| *p == rel && code.contains(n))
                {
                    used_exempt.push(needle);
                    continue;
                }
                for pat in &banned {
                    if line.contains(pat.as_str()) {
                        found.push(format!("  {rel}:{} {code}", i + 1));
                    }
                }
            }
        }
    }
    // Non-vacuity: `src/` NESTS, so a walk that stopped at the top level would report a confident
    // zero while never reading the files this gate exists for.
    for must in ["fair_value.rs", "sport_taker.rs", "cheap_np.rs", "registry.rs"] {
        assert!(
            scanned.iter().any(|s| s == must),
            "the scan never opened {must}, so an empty result proves nothing about the files this \
             gate exists for. Scanned {} files.",
            scanned.len()
        );
    }
    assert!(
        unclosed.is_empty(),
        "a `#[cfg(test)]` item's closing line could not be located, so everything below it went \
         UNSCANNED — the exact hole a `break` leaves, wearing a different cause. See \
         `cfg_test_ranges`: the end is the item's own indentation followed by a lone `}}`, or a \
         `;` for a `mod NAME;` declaration. An empty `#[cfg(test)] mod x {{}}` on one line, or a \
         hand-formatted item rustfmt did not touch, lands here.\n{}",
        unclosed.join("\n")
    );
    // ⚠ Deliberately NO "a production line below a test module was scanned" assertion: measured
    // 2026-09-20, every file's `#[cfg(test)]` module is its LAST item, so it would be FALSE here.
    // The mechanism is held by the shared walk's planted fixture,
    // `crates/vike-model/tests/libm_walk_selftest.rs`'s
    // `the_test_module_cut_excludes_only_the_test_module` (one copy since decision 0074). The range
    // walk is used anyway, so a future mid-file test module does not blind the gate below it.
    //
    // A stale exemption is worse than none: it reads as a considered carve-out while covering a
    // line that no longer exists, and the next author copies it.
    for (p, n) in POWI_BASE_TEN_EXEMPT.iter() {
        assert!(
            used_exempt.contains(n),
            "POWI_BASE_TEN_EXEMPT names `{n}` in {p}, and the scan matched no such production \
             line. Either the call moved (update the entry) or it is gone (delete the entry) — a \
             carve-out nothing uses is an un-gated line waiting to happen."
        );
    }
    assert!(
        found.is_empty(),
        "production code must call the `libm` CRATE rather than `f64`'s platform-libm methods, so a \
         FILL PRICE does not depend on which box ran the replay — `crates/vike-strategy/Cargo.toml`'s \
         `libm` rationale block names the four sites and what each one's last bit decides. BOTH \
         spellings are banned, `x.exp()` and `f64::exp(x)`: same inherent method, same libcall (see \
         `banned_needles`). `powi` is banned too — on MSVC a `dev` build makes it a `pow()` libcall \
         — and its cure is a MULTIPLICATION rather than `libm`; the one base-ten production site \
         that keeps it is named in `POWI_BASE_TEN_EXEMPT` with the measurement behind it:\n{}",
        found.join("\n")
    );
}
