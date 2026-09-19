//! The cross-platform PIN for `orderflow::nice_orderflow_tick` — the one site in this crate whose
//! last bit is the PLATFORM's business, and, by the libm survey's own ranking, the site in this
//! workspace where a last bit would cost the MOST.
//!
//! IEEE 754 requires `+ - * /` and `sqrt` to be correctly rounded and requires NOTHING of `log10`
//! or `pow`, so an `f64` METHOD call reaches whichever libm the PLATFORM ships — MSVC's CRT on the
//! Windows dev box, glibc on the the CI box Linux boxes and every CI runner.
//! `crates/vike-app-core/src/orderflow.rs`'s `nice_orderflow_tick` was converted to the `libm`
//! CRATE for that reason and `docs/decisions/0032-transcendentals-come-from-the-libm-crate-not-the-platform.md`
//! is the accepted verdict — but the conversion landed with NO pin, so nothing in the tree would
//! have noticed the next edit spelling `raw.log10()` again.
//!
//! # ⚠ The DECADE CLIFF is why this row exists, and why its corpus looks the way it does
//!
//! The function's own doc carries the argument in full and this file does not restate it; the part
//! that shapes the corpus below is that `.floor()` applied to a logarithm is a QUANTIZER. Two libms
//! agree everywhere EXCEPT arbitrarily close to an exact power of ten, and exactly there a last-bit
//! disagreement flips the floor by a whole one — `mag` comes back as `1.0` on one box and `10.0` on
//! the other, the `1.5 / 3.5 / 7.5` ladder picks a different rung, and the returned bucket width is
//! **ten times finer or coarser from identical inputs**, with nothing reporting an error.
//!
//! ⚠ Read that at its true size, exactly as the function's doc insists: the survey ranked the
//! POTENTIAL magnitude of a divergence and did NOT observe one here. This row is a tripwire on the
//! shape, not a record of a fault.
//!
//! So the corpus is not a plausible price list. It is
//!
//! * a sweep across ~30 DECADES, because every decade boundary crossed is one more chance for the
//!   two libms to disagree, and
//! * a set of probes deliberately landing within an ulp or two of a decade boundary, because that
//!   is the only neighbourhood where the cliff can fire at all.
//!
//! A corpus confined to real instrument prices ($0.0001 to $100,000) spans nine decades and would
//! report roughly 28 distinct outputs — below [`MIN_DISTINCT`], and far below the density this row
//! needs to be worth committing.
//!
//! # ⚠ The corpus is PURE ARITHMETIC, and that is load-bearing
//!
//! Decade values are built by repeated `* 10.0` / `/ 10.0` from `1.0`, never by `powi`/`powf` and
//! never by a `.sin()`-driven generator. Multiplication and division are correctly rounded by
//! IEEE 754, so every input below is bit-identical on both platforms BY CONSTRUCTION — which is
//! what makes a divergence in the OUTPUT attributable to the function rather than to the inputs.
//! (`10f64.powi(n)` is separately MEASURED bit-identical for `n` in `-22..=22`, but the sweep here
//! deliberately goes wider than that range, so the repeated-multiply construction is used instead
//! of relying on an exemption outside the interval it was measured on.)
//!
//! ```text
//! <cargo> test -p vike-app-core --test libm_platform_probe
//! ```

use vike_app_core::orderflow::nice_orderflow_tick;

// =================================================================================================
// The shared hashing/corpus machinery. Deliberately a per-crate COPY — see `cfg_test_ranges`'s doc.
// =================================================================================================

/// FNV-1a over `f64::to_bits`, NaN canonicalised so a NaN's sign and payload — neither of which the
/// architecture pins — cannot report a divergence that is not one. The same hasher the sibling
/// probes use, so a row moved between crates hashes identically.
struct Fnv(u64);

impl Fnv {
    fn new() -> Self {
        Fnv(0xcbf2_9ce4_8422_2325)
    }
    fn push(&mut self, v: f64) {
        let bits = if v.is_nan() { 0x7ff8_0000_0000_0000u64 } else { v.to_bits() };
        for b in bits.to_le_bytes() {
            self.0 ^= u64::from(b);
            self.0 = self.0.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    fn hex(&self) -> String {
        format!("{:016x}", self.0)
    }
}

/// Deterministic pseudo-random `f64` in `(0, 1)` from a pure-integer LCG. No transcendental
/// anywhere in here, for the reason the module doc gives.
fn lcg(seed: &mut u64) -> f64 {
    *seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
    ((*seed >> 11) as f64) / ((1u64 << 53) as f64)
}

/// `10^k` built by repeated multiplication or division from `1.0` — pure IEEE ops only.
///
/// ⚠ For `k < 0` this is NOT the correctly-rounded `10^k`: repeated division accumulates rounding.
/// That is fine and is the point — what the corpus needs is a value that is DETERMINISTIC and
/// bit-identical on both boxes, which every `/` is by IEEE 754, not one that is mathematically
/// exact. Using `powi`/`powf` to get exactness would put a platform-dependent call INSIDE the
/// generator, which is the failure this whole file exists to detect.
fn decade(k: i32) -> f64 {
    let mut v = 1.0f64;
    for _ in 0..k.abs() {
        if k > 0 {
            v *= 10.0;
        } else {
            v /= 10.0;
        }
    }
    v
}

/// One row's verdict over its own corpus.
struct Row {
    label: &'static str,
    hash: String,
    n: usize,
    /// ⚠ NON-VACUITY GUARD — how many DISTINCT FINITE values the samples took. A row that collapses
    /// to one value hashes identically on every platform and reads as a PASS while pinning nothing.
    distinct: usize,
}

fn finish(label: &'static str, vals: &[f64]) -> Row {
    let mut h = Fnv::new();
    for &v in vals {
        h.push(v);
    }
    let mut bits: Vec<u64> = vals.iter().filter(|v| v.is_finite()).map(|v| v.to_bits()).collect();
    bits.sort_unstable();
    bits.dedup();
    Row { label, hash: h.hex(), n: vals.len(), distinct: bits.len() }
}

/// Smallest number of DISTINCT finite values a row may carry before its pin is treated as vacuous.
///
/// ⚠ This row's output is a DISCRETE ladder — `nice ∈ {1, 2, 5, 10}` times a power of ten — so its
/// distinct count is bounded by roughly three per decade the corpus spans, FOREVER. That makes this
/// threshold a constraint on the CORPUS's decade span rather than on the function, and it is the
/// reason the sweep below is thirty decades wide rather than a realistic price list. If this row
/// ever reports thin, widen the decade span; never lower the floor.
const MIN_DISTINCT: usize = 64;

/// The lowest and highest decade of `price` swept. `raw = price * 0.0002` shifts these down by
/// between three and four decades, so the corpus crosses ~30 decade boundaries of the logarithm.
const DECADE_LO: i32 = -14;
const DECADE_HI: i32 = 15;

/// The number of samples that must land in a decade-boundary NEIGHBOURHOOD for the row to be worth
/// committing.
///
/// ⚠ This is the non-vacuity guard that actually matters here, and the generic `distinct` floor is
/// not a substitute for it. A corpus of thirty decades' worth of ordinary prices would pass
/// [`MIN_DISTINCT`] comfortably while landing nowhere near a cliff — every sample in the smooth
/// interior, where the two libms agree and this row could never report the failure it exists for.
/// The probes counted here are constructed to sit within a couple of ulps of an exact power of ten
/// in `raw`, which is the only neighbourhood where a `floor` can flip.
const MIN_BOUNDARY_PROBES: usize = 3 * ((DECADE_HI - DECADE_LO) as usize + 1);

// =================================================================================================
// The corpus.
// =================================================================================================

/// The single covered function, folded over its own corpus.
fn production_rows() -> Vec<Row> {
    let mut vals = Vec::new();
    let mut boundary_probes = 0usize;

    // --- Part 1: the decade sweep. Ordinary prices spread across thirty decades, so the row covers
    // many distinct `mag` values and its hash is sensitive to any of them moving.
    let mut s = 0x1234_5678_9abc_def0u64;
    for k in DECADE_LO..=DECADE_HI {
        let d = decade(k);
        for _ in 0..64 {
            // A multiplier in (1, 10) walks the whole mantissa range of this decade, so the
            // `1.5 / 3.5 / 7.5` ladder's every arm is exercised at every scale.
            vals.push(nice_orderflow_tick(d * (1.0 + lcg(&mut s) * 9.0)));
        }
    }

    // --- Part 2: the CLIFF probes — the samples this row exists for.
    //
    // A price is constructed so that `price * 0.0002` lands as close to an exact power of ten as
    // f64 allows, then its ulp neighbours on both sides are probed too. Constructing the price by
    // INVERTING the multiply (`d / 0.0002`) rather than asserting that some named price happens to
    // produce an exact decade is deliberate: the inversion needs no claim about the representation
    // of `0.0002` and cannot be invalidated by one. `from_bits(±1)` is exact bit arithmetic, so the
    // neighbours are the same two values on both platforms.
    for k in DECADE_LO..=DECADE_HI {
        let at_boundary = decade(k) / 0.0002;
        if !at_boundary.is_finite() || at_boundary <= 0.0 {
            continue;
        }
        for delta in [-2i64, -1, 0, 1, 2] {
            let bits = at_boundary.to_bits().wrapping_add(delta as u64);
            let p = f64::from_bits(bits);
            if !p.is_finite() || p <= 0.0 {
                continue;
            }
            vals.push(nice_orderflow_tick(p));
            boundary_probes += 1;
        }
    }

    // --- Part 3: the two prices the function's own doc NAMES as landing on a decade boundary —
    // 5_000.0 giving `raw == 1.0` and 50_000.0 giving `raw == 10.0`. They are folded in without any
    // assertion about the representation of `0.0002`: the doc's claim is worth exercising, and this
    // file is not the right place to re-litigate it.
    for p in [5_000.0f64, 50_000.0] {
        vals.push(nice_orderflow_tick(p));
        vals.push(nice_orderflow_tick(f64::from_bits(p.to_bits() - 1)));
        vals.push(nice_orderflow_tick(f64::from_bits(p.to_bits() + 1)));
    }

    // --- Part 4: the GUARD arms, so the row also pins that a degenerate input still returns the
    // neutral tick rather than falling through the log/pow path. These contribute one distinct
    // value between them and are the reason the boundary-probe count, not the distinct count, is
    // this row's real non-vacuity evidence.
    for p in [0.0f64, -1.0, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        vals.push(nice_orderflow_tick(p));
    }

    assert!(
        boundary_probes >= MIN_BOUNDARY_PROBES,
        "only {boundary_probes} of the corpus landed in a decade-boundary neighbourhood (need \
         {MIN_BOUNDARY_PROBES}). Those are the ONLY samples that can catch the decade cliff this \
         row exists for — without them the hash pins the smooth interior, where the two libms agree \
         anyway, and would read as a confident pass. Check `decade`/`DECADE_LO`/`DECADE_HI` before \
         re-recording."
    );

    vec![finish("orderflow::nice_orderflow_tick (log10 + pow)", &vals)]
}

/// ⚠ **ONE set of constants for EVERY platform — that is the entire claim.**
///
/// ⚠ **The hash below is the literal `"RECORD"`, a PLACEHOLDER that fails by construction, and that
/// is deliberate.** The convention is the analytics twin's
/// (`crates/vike-analytics/tests/libm_platform_probe.rs`'s `PLATFORM_INVARIANT`): rows are
/// sometimes written on a box that cannot run cargo, and a plausible-looking hex literal invented
/// there is INDISTINGUISHABLE from a recorded one while pinning nothing. `"RECORD"` cannot match
/// any FNV output, so [`converted_functions_are_platform_invariant`] stays red until a human RUNS
/// this file on Linux/glibc AND on Windows/MSVC, confirms the two runs agree bit-for-bit, and
/// pastes the agreed hash in. **A red row spelled `"RECORD"` is an UNFINISHED RECORDING, not a
/// regression.**
///
/// ⚠ **Re-record by RUNNING, never by editing a digit**, and never from ONE box. A hash edited to
/// match one platform silently un-does the fix on the other, and on THIS row the two would not be
/// one ulp apart — they would be a factor of ten apart.
///
/// ⚠ **A per-platform table (`#[cfg(windows)]` / `#[cfg(unix)]`) is NOT a repair and must never be
/// added.** It would make the gate green by asserting that a ten-fold difference in a bucket width
/// was intended.
const PLATFORM_INVARIANT: [(&str, &str); 1] =
    [("orderflow::nice_orderflow_tick (log10 + pow)", "dd6435aed2a39d90")];

/// The cross-platform proof, as a GATE rather than a diff somebody has to remember to run.
///
/// NOT `#[ignore]`d: CI runs it on Linux every PR and `just t vike-app-core` runs it on the Windows
/// dev box, both against the same committed table.
#[test]
fn converted_functions_are_platform_invariant() {
    let got = production_rows();
    assert_eq!(
        got.len(),
        PLATFORM_INVARIANT.len(),
        "row count drifted from PLATFORM_INVARIANT — it must be re-recorded"
    );

    let mut bad = Vec::new();
    for (r, (want_label, want_hash)) in got.iter().zip(PLATFORM_INVARIANT.iter()) {
        assert_eq!(&r.label, want_label, "row ORDER drifted — the compare is positional");
        if r.hash != *want_hash {
            bad.push(format!("  {:<48} want={want_hash} got={}", r.label, r.hash));
        }
    }

    // Non-vacuity BEFORE the hashes: a thin row is a defect whether or not its hash matched, and an
    // author re-recording a `"RECORD"` row must be told about it before pasting.
    let thin: Vec<String> = got
        .iter()
        .filter(|r| r.distinct < MIN_DISTINCT)
        .map(|r| format!("  {:<48} {} distinct finite of {} samples", r.label, r.distinct, r.n))
        .collect();
    assert!(
        thin.is_empty(),
        "this row's corpus is too thin to detect a divergence. Its output is a DISCRETE ladder, so \
         the fix is a WIDER DECADE SPAN (`DECADE_LO`/`DECADE_HI`) — never a lower MIN_DISTINCT \
         ({MIN_DISTINCT}):\n{}",
        thin.join("\n")
    );

    if !bad.is_empty() {
        let paste: String =
            got.iter().map(|r| format!("    (\"{}\", \"{}\"),\n", r.label, r.hash)).collect();
        panic!(
            "\nvike-app-core output moved away from the platform pin:\n{}\n\n\
             There is no tolerance here to widen, and hand-editing a digit defeats the gate.\n\
             If the want= column reads RECORD this is an UNFINISHED RECORDING rather than a\n\
             regression: run this file on BOTH Windows and Linux, confirm they agree, paste.\n\
             For an already-recorded row: a moved hash on THIS function is not a last bit. The\n\
             decade cliff turns a one-ulp libm disagreement into a footprint/volume-profile grid\n\
             ten times finer or coarser — see `nice_orderflow_tick`'s own doc. Establish whether\n\
               (a) the arithmetic changed intentionally — re-record on BOTH boxes, or\n\
               (b) a call site went back to `raw.log10()` / `10f64.powf(..)`, in which case the two\n\
                   platforms will NOT agree and the fix is in the source, not here.\n\n\
             const PLATFORM_INVARIANT: [(&str, &str); {}] = [\n{paste}];\n",
            bad.join("\n"),
            got.len(),
        );
    }
}

/// The control arm for the non-vacuity threshold itself.
///
/// ⚠ Without this, `MIN_DISTINCT` is an assertion nobody has checked DISCRIMINATES. A `distinct`
/// computation that counted samples rather than distinct values would pass the row above while the
/// guard did nothing.
#[test]
fn the_non_vacuity_check_discriminates() {
    assert!(
        finish("control::constant", &(0..4096).map(|_| 0.5f64).collect::<Vec<f64>>()).distinct
            < MIN_DISTINCT,
        "the non-vacuity check does not discriminate: a constant series passed it"
    );
    assert_eq!(
        finish("control::nan", &(0..4096).map(|_| f64::NAN).collect::<Vec<f64>>()).distinct,
        0,
        "an all-NaN row must report ZERO distinct FINITE values"
    );
    // ...and the decade generator, which every part of the corpus is built on. A `decade` that
    // returned the same value for every `k` would collapse the sweep onto one bucket width and the
    // row would pin nothing while still looking like thirty decades of work.
    let ds: Vec<f64> = (DECADE_LO..=DECADE_HI).map(decade).collect();
    let mut bits: Vec<u64> = ds.iter().map(|v| v.to_bits()).collect();
    bits.sort_unstable();
    bits.dedup();
    assert_eq!(
        bits.len(),
        (DECADE_HI - DECADE_LO) as usize + 1,
        "the decade generator produced repeats — the sweep is narrower than it claims to be"
    );
    assert!(ds.iter().all(|v| v.is_finite() && *v > 0.0), "a swept decade is not a usable price");
}

// =================================================================================================
// The SOURCE half. The table above proves today's output agrees; this stops the NEXT edit from
// reintroducing a platform call in a spot the corpus never reaches.
// =================================================================================================

/// The `f64` methods whose last bit is the PLATFORM's business rather than IEEE 754's, as bare
/// names. [`banned_needles`] renders each into the two spellings that reach it.
///
/// `sqrt` is deliberately absent and must stay absent: IEEE 754 requires it correctly rounded.
/// `to_degrees`/`to_radians` are absent for the same reason wearing a different disguise — std
/// lowers each to one multiplication by a constant.
///
/// `powi` IS here: MEASURED on MSVC, a `dev` build lowers `llvm.powi` to the CRT's `pow()`, so it
/// is a libcall rather than the free multiply chain its name suggests. Note what that means for
/// THIS crate's one site — `nice_orderflow_tick`'s own comment explains why it spells `libm::pow`
/// rather than a `powi`: the exponent is an `f64` that merely happens to hold an integral value, so
/// the measured "powers of ten are exactly representable" exemption (which covers the INTEGER-
/// exponent `powi` path for `n` in `-22..=22`) does not apply to it.
const BANNED_FNS: [&str; 26] = [
    "powf", "ln", "log", "log2", "log10", "exp", "exp2", "exp_m1", "ln_1p", "sin", "cos", "tan",
    "powi", "atan", "atan2", "asin", "acos", "sinh", "cosh", "tanh", "cbrt", "hypot", "asinh",
    "acosh", "atanh", "sin_cos",
];

/// Every [`BANNED_FNS`] name in BOTH spellings that reach the platform's libm.
///
/// ⚠ The UFCS half is not decoration. A gate that bans `.log10(` and nothing else walks straight
/// past `f64::log10(x)` — the same inherent method, called through its fully-qualified path,
/// compiling to the same libcall. Neither spelling is more correct Rust and rustfmt rewrites
/// neither into the other. The `f64::NAME(` needle also covers `std::primitive::f64::NAME(` and
/// `core::primitive::f64::NAME(` for free, because both END with exactly that pattern.
///
/// ⚠ **What it still cannot see, stated rather than implied:** `<f64>::log10(x)` (the angle-bracket
/// spelling), a call reached through a generic bound (`T: Float`), and a call split across two
/// lines by a `max_width` wrap. None of the three occurs under this crate's `src/` today. A list of
/// spellings is an enumeration, and an enumeration is never a proof.
fn banned_needles() -> Vec<String> {
    BANNED_FNS.iter().flat_map(|f| [format!(".{f}("), format!("f64::{f}(")]).collect()
}

/// Every TEST-ONLY item's line range in one file, as `(start, end, closed)` — `start` inclusive,
/// `end` EXCLUSIVE, both zero-based; `closed` says whether the item's end was actually LOCATED
/// rather than assumed, so the one silent failure mode can be asserted on.
///
/// ⚠ **RANGES, never a `break` at the first marker, and in THIS crate that is a live difference
/// rather than insurance.** A `break` says "the shipped half of a file ends at its first test
/// module", which holds only when that module is LAST. Measured 2026-08-29 over
/// `crates/vike-app-core/src`, this crate is the workspace's densest counter-example: `state.rs`
/// carries NINE test modules, `tools.rs` seven, `feed_lifecycle.rs` six, `core_sync.rs` five, and
/// `tool_views/stored.rs` puts `pub fn seed_polymarket_proxy_box` at line 298 between markers at
/// 261 and 316. A `break` would hand this gate a confident zero over most of five files.
/// `crates/vike-mm/src/platform_probe.rs` is where that failure was first measured, and where two
/// `libm` sites were found hiding in the discarded region.
///
/// ⚠ **TWO marker spellings are recognised.** `#[cfg(all(test, …))]` is unambiguously test-only and
/// is a range. `#[cfg(any(test, feature = "…"))]` is deliberately NOT — that attribute marks code
/// that SHIPS whenever the feature is on (`crates/vike-model/src/strategy/mod.rs`'s `MockBroker`,
/// `crates/vike-data/src/exec_index.rs`), so it is production for this gate's purposes.
///
/// ⚠ **And the cut is LINE BY LINE with the comment filter FIRST.** A whole-file
/// `text.find("#[cfg(test)]")` truncates at the first occurrence of that string ANYWHERE, a doc
/// comment included, and a `//`-skipping filter applied afterwards cannot rescue it because it runs
/// on already-truncated text. This crate has three such prose mentions today — including one in
/// `orderflow.rs` itself, the file this gate exists for — so a whole-file cut would blank the
/// bottom of it and report a confident zero.
///
/// # How an item's end is found, and why by INDENTATION rather than by counting braces
///
/// A brace count needs to know which braces are code, which puts a Rust string/char/comment lexer
/// inside a gate — and this workspace's test modules are full of `format!("{}")` and of assert
/// messages continued with a trailing `\`, so a naive count is wrong on exactly the files that
/// matter. Indentation needs no lexer and is exact here for a STRUCTURAL reason: `cargo fmt
/// --check` is CI's first gate, so every file in the tree is rustfmt output, and rustfmt closes a
/// block with a `}` alone on a line at the block's OWN indentation. Verified over this crate
/// 2026-08-29: every marker under `crates/vike-app-core/src` sits at column 0.
///
/// Two ways it can be wrong, and only one is quiet. A line INSIDE the item that IS the closing
/// pattern ends the range early — the scan then resumes over test code, which is full of banned
/// spellings, so that failure is a LOUD false positive naming the line. No closing pattern at ALL
/// runs the range to EOF and is SILENT, which is the `break`'s behaviour; that is what `closed` is
/// for and why the caller asserts on it.
///
/// ⚠ **This is a per-crate COPY, deliberately rather than unnoticed.** Sharing it needs a crate
/// every one of these test targets can depend on; none has one, and adding the edge costs a
/// manifest change in every pinned crate to save eleven lines of string handling. A gate that lives
/// in another crate is also a gate the owning crate can be moved out from under.
fn cfg_test_ranges(lines: &[&str]) -> Vec<(usize, usize, bool)> {
    /// The attribute spellings that open a TEST-ONLY item. See the doc above for why
    /// `#[cfg(any(test, …))]` is absent: that code SHIPS when its feature is on.
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
                // ends either by opening a block or, for `#[cfg(test)] mod tests;`, at the `;`.
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

/// The RANGE walk, proved on a synthetic file as well as on the tree.
///
/// ⚠ **The tree-shaped half cannot replace this, which is why both exist.** The
/// `scanned_past_a_test_module` evidence the scan below collects is real, but it is evidence about
/// `stored.rs` AS IT IS TODAY — move its `seed_polymarket_proxy_box` above the first test module
/// and that assertion passes vacuously while the mechanism goes unproven. This fixture holds the
/// mechanism itself, and every property the walk needs.
#[test]
fn the_test_module_cut_excludes_only_the_test_module() {
    // Every fixture row is a string literal, so the needles inside them are data rather than calls;
    // this file lives under `tests/` and the scan below reads `src/` only, so it never sees them.
    let file = [
        "//! House style: naïve f64 folds, pure, in-file `#[cfg(test)]`.",
        "fn shipped_before() -> f64 {",
        "    x.exp()",
        "}",
        "",
        "#[cfg(test)]",
        "mod first_tests {",
        "    fn fixture() -> f64 {",
        "        y.ln()",
        "    }",
        "}",
        "",
        "fn shipped_after() -> f64 {",
        "    f64::powf(z, 2.0)",
        "}",
        "",
        "#[cfg(all(test, feature = \"whatever\"))]",
        "mod gated_tests {",
        "    fn fixture() -> f64 {",
        "        y.log10()",
        "    }",
        "}",
        "",
        "#[cfg(any(test, feature = \"test-support\"))]",
        "pub fn a_shipped_double() -> f64 {",
        "    q.tanh()",
        "}",
        "",
        "#[cfg(test)]",
        "mod tests;",
        "",
        "fn shipped_last() -> f64 {",
        "    w.sin_cos().0",
        "}",
    ];
    let items = cfg_test_ranges(&file);
    assert_eq!(items.len(), 3, "all three TEST-ONLY items must be found: {items:?}");
    assert_eq!(items[0], (5, 11, true), "the braced module spans its own lines only");
    assert_eq!(items[1], (16, 22, true), "the `all(test, …)` module is test-only and is a range");
    assert_eq!(items[2], (28, 30, true), "the `mod tests;` declaration ends at its semicolon");
    // The control arm: the module doc on line 1 NAMES the attribute in backticks. If prose could
    // open a range, `items[0]` would start at 0 and every assertion here would still pass while the
    // gate scanned nothing at all.
    assert_eq!(items[0].0, 5, "a `#[cfg(test)]` inside a comment must not open a range");

    let banned = banned_needles();
    let hits: Vec<usize> = file
        .iter()
        .enumerate()
        .filter(|(i, line)| {
            !items.iter().any(|&(s, e, _)| *i >= s && *i < e)
                && !line.trim_start().starts_with("//")
                && banned.iter().any(|p| line.contains(p.as_str()))
        })
        .map(|(i, _)| i)
        .collect();
    assert_eq!(
        hits,
        [2, 13, 25, 32],
        "the scan must see the production call BEFORE the first test module (line 3), the one AFTER \
         it (line 14), the one inside the `any(test, …)` item that SHIPS (line 26), and the one \
         after the `mod tests;` declaration (line 33); and must NOT see either fixture call \
         (lines 9 and 20). A `break` at the first marker sees only line 3. \
         Saw (zero-based): {hits:?}"
    );
}

/// Production code in this crate must reach a transcendental through the `libm` CRATE.
///
/// ⚠ Not redundant with the hash table. That table pins ONE function; this crate is the GUI's whole
/// non-rendering half — feeds, tools, workspace, order-flow aggregation — and a new transcendental
/// anywhere in it would be invisible to the corpus above.
///
/// Scope: every `.rs` under `src/`, recursively, MINUS each file's test-item ranges. There are NO
/// exemptions and there should not need to be: measured 2026-08-29, `crates/vike-app-core/src`
/// contains not one call to any [`BANNED_FNS`] name outside a test item, so this gate starts at a
/// clean zero rather than at a grandfathered list.
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
    for must in ["orderflow.rs", "stored.rs", "state.rs"] {
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
    // `tool_views/stored.rs` carries `pub fn seed_polymarket_proxy_box` between two test modules —
    // measured 2026-08-29 at 34 non-blank production lines below the first marker. If this goes
    // red, the walk has reverted to treating the first marker as the end of the shipped file.
    assert!(
        scanned_past_a_test_module.iter().any(|s| s == "stored.rs"),
        "no production line below stored.rs's first test marker was scanned, so this gate has gone \
         blind to the region below every mid-file test module — and this crate has five files with \
         several. Scanned past a test module: {scanned_past_a_test_module:?}"
    );
    assert!(
        found.is_empty(),
        "production code must call the `libm` CRATE rather than `f64`'s platform-libm methods. In \
         this crate that is not a last-bit concern: `nice_orderflow_tick` floors a logarithm, and a \
         one-ulp disagreement at a decade boundary returns a bucket width TEN TIMES finer or \
         coarser from identical inputs — see that function's own doc. BOTH spellings are banned, \
         `x.log10()` and `f64::log10(x)`: same inherent method, same libcall (see \
         `banned_needles`). `powi` is banned too — on MSVC a `dev` build makes it a `pow()` \
         libcall:\n{}",
        found.join("\n")
    );
}
