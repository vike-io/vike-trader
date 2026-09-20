//! **The platform-invariance probe for this crate — the SOURCE half only, and the omission of the
//! other half is argued rather than silent.**
//!
//! `docs/decisions/0032-transcendentals-come-from-the-libm-crate-not-the-platform.md` is the
//! verdict this holds: IEEE 754 requires `+ - * /` and `sqrt` to be correctly rounded and requires
//! NOTHING of `ln`, `exp`, `pow` or their relatives, so `f64::ln` resolves to the PLATFORM's libm —
//! MSVC's CRT on the Windows dev box, glibc on the Linux CI boxes — and the two disagree in the
//! last bit. The cure is the `libm` CRATE, compiled into the binary so both boxes run one
//! implementation.
//!
//! # Why this crate has a probe at all, and why it did not before
//!
//! It arrived with the Polymarket takers. `fair_value.rs`'s `trailing_sigma` calls `libm::log` and
//! `sport_taker.rs`'s `round_dp` keeps a `10f64.powi`, and both moved here out of `vike-backtest`,
//! whose probe scanned them. Nine crates carry one of these files; the receiving crate was not
//! among them, so for the duration of that move those two sites sat in a crate with no scan. This
//! file closes that.
//!
//! # What this file does NOT carry, and where that coverage actually lives
//!
//! The sibling probes open with a RUNTIME table — drive each converted function over a sampled
//! corpus, hash the outputs, compare against a committed digest. **There is no such table here,
//! and adding an empty one would be worse than saying why.** This crate has exactly one converted
//! transcendental, `trailing_sigma`, and it is already pinned BIT-FOR-BIT across platforms by
//! `crates/vike-strategy/tests/cheap_np_parity.rs`'s
//! `trailing_sigma_matches_the_python_oracle_bit_for_bit`, which compares `to_bits()` against six
//! committed `sigma_bits` constants and is neither `#[ignore]`d nor cfg-gated — it runs on the
//! Windows dev box and on Linux in CI against the same committed bytes. `vike-backtest`'s probe
//! reached the same conclusion about the same function while it lived there, and said so in the
//! same words: a second corpus would be maintenance for nothing.
//!
//! So what remains is the half that a bit pin CANNOT give you: the SOURCE SCAN below stops the
//! NEXT edit from reintroducing a platform call in a spot no corpus reaches.
//!
//! # The two structural assumptions, MEASURED on this crate rather than inherited
//!
//! The scan cuts each file at its `#[cfg(test)]` marker and treats everything below as test code.
//! Both properties that makes safe were re-measured over `crates/vike-strategy/src` on 2026-09-20,
//! after the takers landed — not copied from the crate this file was ported from:
//!
//! * all **12** `#[cfg(test)]` markers sit at column 0 (zero are indented), so a column-0 match is
//!   not missing an inner module;
//! * in all **11** files that carry a test module, its closing brace is the LAST non-empty line, so
//!   the coarse cut hides no production code.
//!
//! ⚠ Re-measure both if a long file here is ever split with a test module left mid-file.
//! `registry.rs` (2516 lines) and `position_executor.rs` (1870) are the two closest to wanting it.

use vike_model::libm_walk::{banned_needles, cfg_test_ranges};

/// The ONE production line under `crates/vike-strategy/src` that keeps a `powi`, as
/// `(crate-relative path, the substring that must appear on that line)`.
///
/// ⚠ An exemption keyed on a PATH plus the exempted TEXT, never on a path alone: a whole-file skip
/// stops covering every OTHER line in that file, and `sport_taker.rs` is 714 lines long. Both
/// halves must match, so moving the call keeps it exempt while writing a NEW `.exp()` three lines
/// below it does not.
///
/// The argument is a MEASUREMENT rather than a preference.
/// `docs/decisions/0032-transcendentals-come-from-the-libm-crate-not-the-platform.md`'s 2026-08-26
/// amendment classifies a power by its BASE: `10f64.powi(n)` with a runtime integer exponent was
/// swept across the Windows dev box (MSVC) and the Linux CI runners (glibc) and came back
/// BIT-IDENTICAL for every `n` in `-22..=22` — hash `a87aa0b0905b4eac` on both. That is not luck;
/// powers of ten in that range are exactly representable in `f64`, so there is no rounding for two
/// libms to disagree about. `round_dp`'s callers pass `dp` of 4 and 6, comfortably inside the
/// measured range, and rewriting the line as `libm::pow(10.0, dp as f64)` would swap an exact
/// integer power for the general transcendental path and buy nothing.
const POWI_BASE_TEN_EXEMPT: [(&str, &str); 1] =
    [("src/sport_taker.rs", "let f = 10f64.powi(dp as i32);")];

/// Production code in this crate must reach a transcendental through the `libm` CRATE.
///
/// ⚠ Not redundant with the hash tables. Those pin FIVE functions; this crate carries dozens of
/// source files, and a new call site in any of the others would be invisible to every corpus above
/// while quietly making a fill price depend on which box ran the replay. It also covers `harness/`,
/// which no DEFAULT build compiles — this gate reads text, not the feature-resolved crate graph.
///
/// # The shared parser's three per-crate facts, MEASURED here rather than inherited
///
/// The text machinery this gate runs on — `banned_needles` and `cfg_test_ranges` — lives in
/// `vike_model::libm_walk` since decision 0074, and it deliberately leaves three questions to the
/// crate that calls it, because each is a fact about THIS crate's sources rather than about the
/// parser. All three were measured over `crates/vike-strategy/src` on 2026-09-20.
///
/// ⚠ **The needle set's blind spots, and that none of them is occupied here.** Both spellings of a
/// banned call are covered, `x.exp()` and `f64::exp(x)` — same inherent method, same libcall.
/// Three spellings reach the platform anyway and no needle can see them: the angle-bracket form
/// (`f64>::exp(` is what appears, and no needle ends in `>`), a call reached through a generic
/// bound (`T: Float`, `num_traits::Float::exp`), and a call split across two lines by a
/// `max_width` wrap. **None of the three occurs under this crate's `src/` today.** A list of
/// spellings is an enumeration, and an enumeration is never a proof.
///
/// ⚠ **The second marker spelling has no instance here, and is carried anyway.** The walk
/// recognises `#[cfg(test)]` and `#[cfg(all(test, …))]`, and deliberately does NOT recognise
/// `#[cfg(any(test, feature = "…"))]`, which SHIPS whenever its feature is on and must therefore
/// be scanned. **This crate carries no `#[cfg(all(test, …))]` module today.** The spelling costs
/// nothing to keep, and what it buys is that this crate does not acquire the blind spot the first
/// time a feature-gated test module lands here — a walk that knew only the bare spelling would
/// read such a module as production and report every banned needle inside it.
///
/// ⚠ **The indentation walk's precondition holds here.** An item's end is its own indentation
/// followed by a lone `}`, which is exact only because every file in the tree is rustfmt output.
/// Alongside the two counts in this file's module doc: every test marker under this crate's `src/`
/// sits at column 0 AND each is followed by a line ending in `{` — so no file here uses the
/// `#[cfg(test)] mod NAME;` header form the walk also handles, and the `closed` assertion below is
/// the whole of what stands between a hand-formatted item and a silent skip.
///
/// Scope: every `.rs` under `src/`, recursively, MINUS each file's `#[cfg(test)]` item ranges, MINUS
/// the [`POWI_BASE_TEN_EXEMPT`] lines.
#[test]
fn production_code_calls_libm_not_the_platform() {
    let banned = banned_needles();
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let src = root.join("src");
    let mut found = Vec::new();
    let mut scanned: Vec<String> = Vec::new();
    // A `#[cfg(test)]` item whose end could not be located swallows the rest of its file exactly
    // the way a `break` would. That is the one QUIET failure of the range walk, so it is collected
    // and asserted rather than absorbed.
    let mut unclosed: Vec<String> = Vec::new();
    // Which exemptions actually matched. A stale exemption is an un-gated line waiting to happen.
    let mut used_exempt: Vec<&str> = Vec::new();
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
    // Non-vacuity, aimed at the failure this scan is most likely to have: `src/` NESTS here
    // (`harness/`, `laws/`), so a walk that stopped at the top level would still open dozens of
    // files and report a confident zero while never reading the sampler.
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
    // ⚠ There is deliberately NO "some production line below a test module was scanned" assertion
    // here, and its absence is a MEASURED fact about this tree rather than an oversight. Measured
    // 2026-09-20 over `crates/vike-strategy/src`: every file's `#[cfg(test)]` module is the LAST
    // item in it — not one of the ~85 files puts one in the middle, and no file carries two — so
    // such an assertion would be FALSE today and could only be written by weakening it into
    // something that proves nothing. The mechanism is held instead by the shared walk's own
    // planted fixture, `crates/vike-model/tests/libm_walk_selftest.rs`'s
    // `the_test_module_cut_excludes_only_the_test_module` — ONE copy since decision 0074, where
    // this file used to carry a private one — which is the same answer
    // `crates/vike-analytics/tests/libm_platform_probe.rs` reached for the same reason.
    // (`crates/vike-mm/src/platform_probe.rs` CAN assert it on the tree, because that crate has
    // five mid-file test modules and two `libm` sites below one of them.) The range walk is used
    // here anyway: the property it buys is that the day somebody DOES add a mid-file test module —
    // `registry.rs` is 2516 lines and already close to wanting one — the gate does not go blind
    // to the third of the file below it.
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
