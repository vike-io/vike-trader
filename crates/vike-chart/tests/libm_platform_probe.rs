//! The cross-platform PIN for this crate's transcendental sites.
//!
//! ⚠ **Read the ranking honestly before reading the rest: this crate is PIXELS, and it is last.**
//! `crates/vike-backtest`'s `impact_frac` is a fill price, `queue_model::ProbFunc::f` decides
//! whether a resting order fills, `crates/vike-model`'s `p_up` reaches a posted quote through four
//! quantisers, and `crates/vike-app-core`'s `nice_orderflow_tick` is a ten-fold divergence in a
//! bucket width. What moves here is a gridline position and a zoom factor. The scale module's own
//! doc says the same thing in its own words — "hygiene rather than a live defect". This file exists
//! because a conversion with no pin is an argument rather than a guarantee, not because anybody
//! believes a chart axis is going to cost money.
//!
//! IEEE 754 requires `+ - * /` and `sqrt` to be correctly rounded and requires NOTHING of
//! `log10`/`pow`/`exp`/`sin`/`cos`, so an `f64` METHOD call reaches whichever libm the PLATFORM
//! ships — MSVC's CRT on the Windows dev box, glibc on the CI runners — and the two disagree in the
//! last bit. `docs/decisions/0032-transcendentals-come-from-the-libm-crate-not-the-platform.md` is
//! the accepted verdict.
//!
//! # Where a last bit is NOT merely a pixel, which is the one part worth arguing about
//!
//! `scale::nice_ticks` in `Log` mode runs the same DECADE-CLIFF shape
//! `crates/vike-app-core/src/orderflow.rs` writes up at length: `log_nice_values` takes
//! `log10(lo).floor()` and `log10(hi).ceil()`, so a last-bit disagreement arbitrarily close to an
//! exact power of ten shifts the whole candidate set by a DECADE — a different gridline set on each
//! box, silently, not a sub-pixel nudge. That is why the `Log` tick row's corpus below is built
//! around decade boundaries rather than around plausible price extents.
//!
//! # Declared coverage gap — the site the SOURCE gate holds and the table does not
//!
//! `crates/vike-chart/src/render.rs`'s `draw_pnf` traces its O-column ellipse ring with
//! `libm::cos`/`libm::sin`. It takes an `egui_plot::PlotUi` and emits shapes, so pinning it means
//! standing up the draw harness rather than calling a function with numbers — a different test with
//! different failure modes. ⚠ That site's own doc already records the sharper problem: it is **the
//! one libm consumer in this crate that NO golden covers**, because
//! `crates/vike-chart/tests/tessellation_goldens.rs` renders sixteen scenarios and not one of them
//! is PointFigure. So it is uncovered here AND uncovered there, and the fix named at the site —
//! adding a PnF scenario to that suite — is the right one rather than a row in this file.
//! [`production_code_calls_libm_not_the_platform`] still stops it being respelled as `a.cos()`.
//!
//! # ⚠ The corpus is PURE ARITHMETIC, and that is load-bearing
//!
//! Every input is an LCG draw or a decade built by repeated `* 10.0` / `/ 10.0`, never a
//! `.sin()`-driven series. Those are themselves platform-dependent (MEASURED —
//! `crates/vike-analytics/tests/libm_platform_probe.rs`'s Part A sweeps them as its own control),
//! so generating inputs with them would make every row diverge because of the INPUTS rather than
//! because of the function under test.
//!
//! ```text
//! <cargo> test -p vike-chart --test libm_platform_probe
//! ```

use vike_chart::interact::{x_drag_zoom, y_drag_zoom};
use vike_chart::scale::{ScaleMode, nice_ticks};
use vike_model::libm_walk::{banned_needles, cfg_test_ranges};

// =================================================================================================
// The hashing/corpus machinery. Deliberately a per-crate COPY: the corpus IS this crate's, and a
// shared generator would have to be told which functions to drive. Only the TEXT machinery of the
// source half below is shared (`crates/vike-model/src/libm_walk.rs`, decision 0074).
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

/// Samples per row.
const SAMPLES: usize = 4096;

/// One row's verdict over its own corpus.
struct Row {
    label: &'static str,
    hash: String,
    n: usize,
    /// ⚠ NON-VACUITY GUARD — how many DISTINCT FINITE values the samples took. A row that collapses
    /// to one value hashes identically on every platform and reads as a PASS while pinning nothing.
    /// The analytics twin caught a real instance of exactly that (`overfit::pbo_cscv`). The shape
    /// to watch HERE is different: `nice_ticks` returns an EMPTY vector for non-finite or inverted
    /// extents, so a mis-centred corpus produces no samples at all rather than constant ones — the
    /// per-row emission counts below are what catch that, and this field catches the rest.
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
const MIN_DISTINCT: usize = 64;

/// Decade span for the log-scale rows. Wider than any real price axis, deliberately: the failure
/// these rows exist for lives AT a decade boundary, so the corpus is built to cross many.
const DECADE_LO: i32 = -9;
const DECADE_HI: i32 = 12;

// =================================================================================================
// The corpus.
// =================================================================================================

/// Every covered function, folded over its own corpus.
///
/// Returns rows in a FIXED order; [`PLATFORM_INVARIANT`] is compared positionally, so a reordering
/// is a re-record rather than a silent re-pairing.
fn production_rows() -> Vec<Row> {
    let mut out = Vec::new();

    // --- scale::ScaleMode::Log map/unmap — `libm::log10(y)` and `libm::pow(10.0, v)`.
    //
    // Both directions in ONE row because they are inverses and are always used as a pair: a plot
    // maps a price down and unmaps a pixel row back up. `unmap`'s own doc is worth heeding when
    // reading this row — the base being the literal 10 does NOT make it exact, because `v` is an
    // arbitrary runtime plot-space y and `10^2.8017…` is not representable.
    //
    // The price sweep spans the decade range rather than a plausible axis, for the reason
    // `DECADE_LO`/`DECADE_HI` give, and every sample is strictly positive: `Log` maps a
    // non-positive price to `-inf`/NaN, and a corpus of those hashes identically everywhere.
    {
        let mut s = 0x1234_5678_9abc_def0u64;
        let mut vals = Vec::with_capacity(SAMPLES);
        for i in 0..SAMPLES {
            let k = DECADE_LO + (i as i32 % (DECADE_HI - DECADE_LO + 1));
            let price = decade(k) * (1.0 + lcg(&mut s) * 9.0);
            let mapped = ScaleMode::Log.map(price, 0.0);
            vals.push(mapped);
            vals.push(ScaleMode::Log.unmap(mapped, 0.0));
        }
        out.push(finish("scale::ScaleMode::Log map+unmap (log10 + pow)", &vals));
    }

    // --- scale::nice_ticks, LINEAR — reaches `nice_step_ceil`, whose
    // `libm::log10(raw_step).floor()` / `libm::pow(10.0, exponent)` pair is the same
    // quantise-a-logarithm shape as `nice_orderflow_tick`.
    //
    // ⚠ `nice_step_ceil` is `pub(crate)` and is deliberately NOT made reachable to pin it: driving
    // it through the public tick generator is the stronger pin anyway, because it covers the
    // candidate loop and the `max_ticks` budget that consume the step.
    //
    // Extents are built around decade boundaries so `raw_step` lands near one — the only place the
    // `floor` can flip. Every field of every tick is hashed: `mapped` and `step_mapped` are what
    // egui_plot consumes, `raw` is what the label prints.
    {
        let mut s = 0x0fed_cba9_8765_4321u64;
        let mut vals = Vec::with_capacity(SAMPLES);
        let mut emitted = 0usize;
        for k in DECADE_LO..=DECADE_HI {
            let d = decade(k);
            for _ in 0..24 {
                let lo = d * (lcg(&mut s) - 0.5) * 2.0;
                let hi = lo + d * (1.0 + lcg(&mut s) * 9.0);
                let max_ticks = 2 + (lcg(&mut s) * 18.0) as usize;
                for t in nice_ticks(ScaleMode::Linear, lo, hi, 0.0, max_ticks) {
                    vals.push(t.mapped);
                    vals.push(t.step_mapped);
                    vals.push(t.raw);
                    emitted += 1;
                }
            }
        }
        assert!(
            emitted > 500,
            "the Linear tick row emitted only {emitted} ticks — `nice_ticks` returns an EMPTY \
             vector for non-finite or inverted extents, so a corpus that produces none hashes the \
             FNV seed and pins nothing. Check the extent generator before re-recording."
        );
        out.push(finish("scale::nice_ticks Linear (log10 + pow)", &vals));
    }

    // --- scale::nice_ticks, LOG — reaches `log_nice_values`, whose `libm::log10(lo).floor()` and
    // `libm::log10(hi).ceil()` bound the decade loop.
    //
    // ⚠ This is the row that is NOT merely a pixel. A last-bit disagreement at an exact power of
    // ten moves `k_min`/`k_max` by a WHOLE decade, which changes the candidate set rather than
    // nudging a position — the same cliff `crates/vike-app-core/src/orderflow.rs` writes up. So the
    // extents here are deliberately planted ON decade boundaries and on their ulp neighbours, which
    // is the only neighbourhood where the failure can occur; a corpus of ordinary extents would sit
    // in the smooth interior and could never report it.
    {
        let mut vals = Vec::with_capacity(SAMPLES);
        let mut emitted = 0usize;
        let mut boundary_extents = 0usize;
        for k in DECADE_LO..DECADE_HI {
            let lo_exact = decade(k);
            let hi_exact = decade(k + 2);
            for delta in [-1i64, 0, 1] {
                // Exact bit arithmetic, so both boxes probe the same two neighbours.
                let lo = f64::from_bits(lo_exact.to_bits().wrapping_add(delta as u64));
                let hi = f64::from_bits(hi_exact.to_bits().wrapping_add(delta as u64));
                if !(lo.is_finite() && lo > 0.0 && hi.is_finite() && hi > lo) {
                    continue;
                }
                boundary_extents += 1;
                for max_ticks in [3usize, 6, 12, 24] {
                    for t in nice_ticks(ScaleMode::Log, lo, hi, 0.0, max_ticks) {
                        vals.push(t.mapped);
                        vals.push(t.step_mapped);
                        vals.push(t.raw);
                        emitted += 1;
                    }
                }
            }
        }
        assert!(
            boundary_extents >= (DECADE_HI - DECADE_LO) as usize,
            "only {boundary_extents} decade-boundary extents survived the finiteness guard — those \
             are the ONLY inputs that can catch the decade cliff this row exists for."
        );
        assert!(
            emitted > 200,
            "the Log tick row emitted only {emitted} ticks — an empty result hashes the FNV seed \
             and pins nothing. Check the extent generator before re-recording."
        );
        out.push(finish("scale::nice_ticks Log (log10, decade cliff)", &vals));
    }

    // --- interact::y_drag_zoom / x_drag_zoom — `libm::exp(±K_DRAG · dpx)`.
    //
    // Both in one row: they are the same call with opposite signs, and a maker of this file who
    // converted only one would want a single red line rather than two. The drag deltas span a
    // frame's worth of mouse movement in both directions, and `0.0` is included explicitly because
    // both functions document it as the EXACT identity — a property `exp(0) == 1` gives for free on
    // any conforming libm, and one worth keeping in the hash so a future rewrite cannot lose it.
    {
        let mut s = 0x6666_7777_8888_9999u64;
        let mut vals = Vec::with_capacity(SAMPLES);
        for i in 0..SAMPLES {
            let d = if i % 64 == 0 { 0.0f32 } else { ((lcg(&mut s) - 0.5) * 4_000.0) as f32 };
            let lo = -1_000.0 + lcg(&mut s) * 2_000.0;
            let hi = lo + 0.001 + lcg(&mut s) * 5_000.0;
            let (a, b) = y_drag_zoom(lo, hi, d);
            let (c, e) = x_drag_zoom(lo, hi, d);
            vals.push(a);
            vals.push(b);
            vals.push(c);
            vals.push(e);
        }
        out.push(finish("interact::{x,y}_drag_zoom (exp)", &vals));
    }

    out
}

/// ⚠ **ONE set of constants for EVERY platform — that is the entire claim.**
///
/// ⚠ **Every hash below is the literal `"RECORD"`, a PLACEHOLDER that fails by construction, and
/// that is deliberate.** The convention is the analytics twin's
/// (`crates/vike-analytics/tests/libm_platform_probe.rs`'s `PLATFORM_INVARIANT`): rows are
/// sometimes written on a box that cannot run cargo, and a plausible-looking hex literal invented
/// there is INDISTINGUISHABLE from a recorded one while pinning nothing. `"RECORD"` cannot match
/// any FNV output, so [`converted_functions_are_platform_invariant`] stays red until a human RUNS
/// this file on Linux/glibc AND on Windows/MSVC, confirms the two runs agree bit-for-bit, and
/// pastes the agreed hash in. **A red row spelled `"RECORD"` is an UNFINISHED RECORDING, not a
/// regression.**
///
/// ⚠ **Re-record by RUNNING, never by editing a digit**, and never from ONE box.
///
/// ⚠ **A per-platform table (`#[cfg(windows)]` / `#[cfg(unix)]`) is NOT a repair and must never be
/// added.**
///
/// ⚠ These rows are CPU-side arithmetic and have nothing to do with rasterization, so they run on
/// the GPU-less CI runners exactly as `crates/vike-chart/tests/draw_characterization.rs` and its
/// siblings do. `CLAUDE.md`'s split is the authority: layout and geometry are arithmetic; only
/// shapes-to-pixels needs the dev box.
const PLATFORM_INVARIANT: [(&str, &str); 4] = [
    ("scale::ScaleMode::Log map+unmap (log10 + pow)", "4d19d9b0959587c7"),
    ("scale::nice_ticks Linear (log10 + pow)", "606cb7d8181acd38"),
    ("scale::nice_ticks Log (log10, decade cliff)", "c8b036a8c4f43f0d"),
    ("interact::{x,y}_drag_zoom (exp)", "ed6c1b1f5a41d0b5"),
];

/// The cross-platform proof, as a GATE rather than a diff somebody has to remember to run.
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
        "a pinned row's corpus is too thin to detect a divergence: it would hash identically on \
         every platform and read as a PASS. WIDEN the inputs — never lower MIN_DISTINCT \
         ({MIN_DISTINCT}):\n{}",
        thin.join("\n")
    );

    if !bad.is_empty() {
        let paste: String =
            got.iter().map(|r| format!("    (\"{}\", \"{}\"),\n", r.label, r.hash)).collect();
        panic!(
            "\nvike-chart output moved away from the platform pin:\n{}\n\n\
             There is no tolerance here to widen, and hand-editing a digit defeats the gate.\n\
             If the want= column reads RECORD this is an UNFINISHED RECORDING rather than a\n\
             regression: run this file on BOTH Windows and Linux, confirm they agree, paste.\n\
             ⚠ For an already-recorded row, the `nice_ticks Log` row is the one to take seriously:\n\
             `log_nice_values` floors and ceils a logarithm, so a move there is a whole DECADE of\n\
             gridlines rather than a sub-pixel nudge. Establish whether\n\
               (a) the arithmetic changed intentionally — re-record on BOTH boxes, or\n\
               (b) a call site went back to `x.log10()` / `10f64.powf(..)`, in which case the two\n\
                   platforms will NOT agree and the fix is in the source, not here.\n\n\
             const PLATFORM_INVARIANT: [(&str, &str); {}] = [\n{paste}];\n",
            bad.join("\n"),
            got.len(),
        );
    }
}

/// The control arm for the non-vacuity threshold itself, plus the decade generator every row rests
/// on.
///
/// ⚠ Without this, `MIN_DISTINCT` is an assertion nobody has checked DISCRIMINATES. A `distinct`
/// computation that counted samples rather than distinct values would pass every row above while
/// the guard did nothing.
#[test]
fn the_non_vacuity_check_discriminates() {
    let constant: Vec<f64> = (0..SAMPLES).map(|_| 0.5f64).collect();
    assert!(
        finish("control::constant", &constant).distinct < MIN_DISTINCT,
        "the non-vacuity check does not discriminate: a constant series passed it"
    );
    let nans: Vec<f64> = (0..SAMPLES).map(|_| f64::NAN).collect();
    assert_eq!(
        finish("control::nan", &nans).distinct,
        0,
        "an all-NaN row must report ZERO distinct FINITE values"
    );
    // An EMPTY corpus hashes the FNV seed — a perfectly stable value on every platform, and the
    // shape a `nice_ticks` row takes when its extents are rejected rather than the shape a broken
    // scalar row takes. The per-row `emitted` assertions above are what guard against it; this
    // pins that an empty corpus really would look like a pass, so those assertions are not
    // decoration.
    assert_eq!(
        finish("control::empty", &[]).hash,
        Fnv::new().hex(),
        "an empty corpus must hash the bare FNV seed — if it does not, the emission guards above \
         are protecting against the wrong failure"
    );
    let ds: Vec<f64> = (DECADE_LO..=DECADE_HI).map(decade).collect();
    let mut bits: Vec<u64> = ds.iter().map(|v| v.to_bits()).collect();
    bits.sort_unstable();
    bits.dedup();
    assert_eq!(
        bits.len(),
        (DECADE_HI - DECADE_LO) as usize + 1,
        "the decade generator produced repeats — the sweep is narrower than it claims to be"
    );
}

// =================================================================================================
// The SOURCE half. The table above proves today's outputs agree; this stops the NEXT edit from
// reintroducing a platform call in a spot the corpus never reaches — `render::draw_pnf`'s ellipse
// ring above all, which the module doc declares as an uncovered gap.
// =================================================================================================

/// The ONE production line under `crates/vike-chart/src` that keeps a `powi`, as
/// `(crate-relative path, the substring that must appear on that line)`.
///
/// ⚠ An exemption keyed on a PATH plus the exempted TEXT, never on a path alone: a whole-file skip
/// stops covering every OTHER line in that file, and `scale.rs` is 1226 lines long. Both halves
/// must match, so moving the call keeps it exempt while writing a NEW `.log10()` three lines below
/// it does not.
///
/// The argument is a MEASUREMENT, and it is written at the site itself:
/// `crates/vike-chart/src/scale.rs`'s `log_nice_values` records that `10f64.powi(n)` came back
/// BIT-IDENTICAL on the Windows dev box (MSVC) and the Linux CI runners (glibc) for every `n` in
/// `-22..=22` — the whole sweep hashing `a87aa0b0905b4eac` on each. Powers of ten in that range are
/// exactly representable in `f64`, so there is no rounding for two libms to disagree about, and
/// rewriting the line as `libm::pow(10.0, k as f64)` would swap an exact integer power for the
/// general transcendental path.
///
/// ⚠ **The residual, stated because it is real and the site's own comment does not name it:** that
/// loop's `k` runs `k_min..=k_max`, derived from `log10(lo).floor()` and `log10(hi).ceil()` on
/// caller-supplied extents. Nothing bounds it to `-22..=22`. The finiteness guard above it rejects
/// infinities, but an extent of `1e-40` or `1e300` is finite and reaches this line with `|k|` far
/// outside the MEASURED interval, where `10^k` is no longer exactly representable and the MSVC
/// `pow()` libcall could in principle disagree with glibc's. This exemption is therefore
/// MEASUREMENT inside `-22..=22` and ARGUMENT outside it. It is left as an exemption rather than
/// converted because the affected extents are not axis ranges any chart shows — and because
/// converting would move every gridline on both platforms to fix a case nobody has produced. If a
/// caller ever passes such extents, the honest fix is to bound `k` at the source, not to widen
/// anything here.
const POWI_BASE_TEN_EXEMPT: [(&str, &str); 1] = [("src/scale.rs", "let base = 10f64.powi(k);")];

/// Production code in this crate must reach a transcendental through the `libm` CRATE.
///
/// ⚠ Not redundant with the hash table, and in this crate the gap it fills is NAMED:
/// `render::draw_pnf`'s ellipse ring has no row here AND no golden in
/// `crates/vike-chart/tests/tessellation_goldens.rs`, so this gate is the ONLY thing standing
/// between that ring and a per-platform pixel difference. That is the site's own assessment, quoted
/// in the module doc above.
///
/// Scope: every `.rs` under `src/`, recursively, MINUS each file's test-item ranges, MINUS the
/// [`POWI_BASE_TEN_EXEMPT`] lines. The TEXT machinery it runs on — the banned-name list, the two
/// spellings each name renders into, and the test-item RANGE walk — is shared:
/// `crates/vike-model/src/libm_walk.rs`, one parser for the eleven crates that carry a probe
/// (decision 0074), with its own planted-fixture self-test at
/// `crates/vike-model/tests/libm_walk_selftest.rs`. The general argument for each mechanism lives
/// there. What could NOT move is this crate's own evidence for trusting it, which is below.
///
/// # What was MEASURED about that machinery HERE — the per-crate half of the argument
///
/// ⚠ **`powi` is banned and this crate keeps exactly one.** MEASURED on MSVC, a `dev` build lowers
/// `llvm.powi` to the CRT's `pow()`, so it is a libcall rather than the free multiply chain its
/// name suggests. The one production site in this crate that keeps a `powi` is
/// [`POWI_BASE_TEN_EXEMPT`], with the measurement behind it and the residual that measurement does
/// NOT cover written on that entry.
///
/// ⚠ **The needle set's three blind spots do not occur here — TODAY.** `banned_needles` cannot see
/// `<f64>::log10(x)` (the angle-bracket spelling), a call reached through a generic bound
/// (`T: Float`), or a call split across two lines by a `max_width` wrap. None of the three occurs
/// under this crate's `src/`. A list of spellings is an enumeration, and an enumeration is never a
/// proof, so this is a fact about this crate rather than a property anything holds.
///
/// ⚠ **The RANGE walk buys THIS crate exactly ONE line, and it is kept anyway.** A `break` at a
/// file's first `#[cfg(test)]` marker says "the shipped half ends here", which holds only when
/// that module is last. MEASURED 2026-08-29, after an earlier claim that this crate had production
/// code between its test modules turned out to be false: `crates/vike-chart/src/indicators.rs` is
/// 1413 lines, its three markers at 760, 1217 and 1331 open `mod tests`, `mod forming_memo_tests`
/// and `mod build_cost_tests`, its last production line is 758, and the only lines between the
/// modules are the blank separators at 1216 and 1330 — so a `break` classifies all 654 lines from
/// 760 to EOF as test code where the walk classifies 653 of them. Sweeping the whole of
/// `crates/vike-chart/src`: **no file here has a production item below its first marker.** That is
/// why the witness the body collects can only prove RESUMPTION, never that production code below a
/// marker is covered — the payoff is proven on the shared planted fixture instead.
///
/// ⚠ **The comment-filter-first rule is LATENT here rather than live.** A whole-file `find` on the
/// marker string truncates at the first occurrence of it ANYWHERE, a doc comment included. MEASURED
/// 2026-08-29: `crates/vike-chart/src` carries no such prose mention, so that trap cannot spring
/// here today — "latent" being a fact about this crate this month and not a guarantee.
///
/// ⚠ **The indentation rule is exact here by inspection.** The walk closes an item on a lone `}` at
/// the item's OWN indentation, which needs every marker to be where rustfmt puts it: VERIFIED over
/// this crate 2026-08-29, every marker under `crates/vike-chart/src` sits at column 0.
#[test]
fn production_code_calls_libm_not_the_platform() {
    let banned = banned_needles();
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let src = root.join("src");
    let mut found = Vec::new();
    let mut scanned: Vec<String> = Vec::new();
    /// What the RANGE walk produced for ONE file — the facts a `break` at the first marker cannot
    /// exhibit, recorded so the assertion below reads the walk's real output rather than a claim
    /// about it. Collected for every file carrying at least one test item.
    struct WalkWitness {
        file: String,
        /// Test-only ranges located. A `break` at the first marker yields at most ONE.
        ranges: usize,
        /// The first range's EXCLUSIVE end: where the walk resumed. A `break` ends it at EOF.
        first_end: usize,
        /// The NEXT range's start, i.e. a further marker found below the first module. A `break`
        /// can never report one.
        next_start: Option<usize>,
        /// The file's line count, so `next_start < lines` states "resumed INSIDE the file".
        lines: usize,
        /// Lines past the first marker that the scan loop below actually reached rather than
        /// skipping as test code. ⚠ In THIS crate every one of them is BLANK — see the assertion —
        /// so it witnesses that the loop RESUMED, never that production code below a marker is
        /// covered. Counted before the comment filter, because a `//` line is examined-then-skipped.
        visited_past_first_marker: usize,
    }
    let mut walk: Vec<WalkWitness> = Vec::new();
    // A test item whose end could not be located swallows the rest of its file exactly the way a
    // `break` would. That is the one QUIET failure of the range walk, so it is collected and
    // asserted rather than absorbed.
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
            scanned.push(name.clone());
            let lines: Vec<&str> = text.lines().collect();
            let test_items = cfg_test_ranges(&lines);
            for &(start, _, closed) in &test_items {
                if !closed {
                    unclosed.push(format!("  {rel}:{}", start + 1));
                }
            }
            let first_marker = test_items.first().map(|&(start, _, _)| start);
            let mut visited_past_first_marker = 0usize;
            for (i, line) in lines.iter().enumerate() {
                if test_items.iter().any(|&(start, end, _)| i >= start && i < end) {
                    continue; // inside a test item — not shipped
                }
                if first_marker.is_some_and(|m| i > m) {
                    // The scan proper reached a line BELOW the first marker: the region a `break`
                    // at that marker discards wholesale.
                    visited_past_first_marker += 1;
                }
                let code = line.trim_start();
                if code.starts_with("//") {
                    continue; // a doc comment naming `log10` is prose, not a call
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
            if let Some(&(_, first_end, _)) = test_items.first() {
                walk.push(WalkWitness {
                    file: name,
                    ranges: test_items.len(),
                    first_end,
                    next_start: test_items.get(1).map(|&(start, _, _)| start),
                    lines: lines.len(),
                    visited_past_first_marker,
                });
            }
        }
    }
    // Non-vacuity, aimed at the failure this scan is most likely to have: `src/` NESTS here
    // (`chart/`), so a walk that stopped at the top level would still open a dozen files and report
    // a confident zero while never reading a subdirectory.
    for must in ["scale.rs", "interact.rs", "render.rs", "price_render.rs"] {
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
         the exact hole a `break` leaves, wearing a different cause. See \
         `vike_model::libm_walk::cfg_test_ranges`: the end \
         is the item's own indentation followed by a lone `}}`, or a `;` for a `mod NAME;` \
         declaration.\n{}",
        unclosed.join("\n")
    );
    // The non-vacuity assertion for the RANGE walk, and the one a `break` at the first marker
    // cannot pass.
    //
    // ⚠ **CORRECTED 2026-08-29 — this used to assert a witness that does not exist.** The old form
    // demanded that a NON-BLANK PRODUCTION line below `indicators.rs`'s first marker had been
    // scanned ("no production line below indicators.rs's first test marker was scanned, so this
    // gate has gone blind to the 653 lines below it"), over a comment claiming that file "carries
    // three test modules with production code between them — measured 2026-08-29". That claim is
    // FALSE and the assertion was unsatisfiable, i.e. permanently red. RE-MEASURED the same day:
    // `indicators.rs` is 1413 lines; its three `#[cfg(test)]` markers at 760, 1217 and 1331 open
    // `mod tests`, `mod forming_memo_tests` and `mod build_cost_tests`; its last production line is
    // 758; and the only lines between the modules are the blank separators at 1216 and 1330.
    // Sweeping the whole of `crates/vike-chart/src` the same way, NO file here carries a production
    // item below its first marker — so a witness of that shape cannot be produced from this crate,
    // and one may not be invented.
    //
    // What IS true is what the walk actually produces, and it is read off `walk` rather than
    // asserted about: `cfg_test_ranges` returned THREE ranges for that file; the first CLOSED at
    // its own `}` instead of running to EOF; a further marker was found below that close and inside
    // the file; and the scan loop then reached lines past the first marker. A `break` at the first
    // marker exhibits NONE of them — it returns at most one range per file, ending at EOF, so
    // `ranges` would be 1, `next_start` would be `None`, and `visited_past_first_marker` 0.
    //
    // ⚠ What this deliberately does NOT claim: the lines visited past the marker are the two BLANK
    // separators. This witnesses RESUMPTION and the multi-marker shape of the walk's real input; it
    // is the shared planted-fixture self-test — `crates/vike-model/tests/libm_walk_selftest.rs`'s
    // `the_test_module_cut_excludes_only_the_test_module` — that proves resumption keeps SCANNING
    // production code. Neither subsumes the other: the fixture cannot prove that the walk's real
    // input in THIS crate still has the multi-marker shape it models (merge `indicators.rs`'s three
    // modules into one and the assertion below goes red while the fixture keeps passing), and this
    // witness cannot prove what resuming BUYS, because the lines it resumes over here are blank.
    let witness = walk.iter().find(|w| w.file == "indicators.rs");
    assert!(
        witness.is_some_and(|w| w.ranges > 1
            && w.next_start.is_some_and(|s| s >= w.first_end && s < w.lines)
            && w.visited_past_first_marker > 0),
        "the walk over indicators.rs no longer shows it RESUMING below that file's first \
         `#[cfg(test)]` marker — the property a `break` at the first marker makes impossible. \
         Either (a) `cfg_test_ranges` has been reverted to stopping at the first marker, and the \
         fix is in the walk; or (b) that file's three test modules were merged into one, in which \
         case this crate has lost its only multi-range file and this witness must be re-pointed at \
         whichever file now carries two (the candidates are listed below, and an empty list means \
         there are none) or deleted in favour of the shared planted-fixture self-test \
         (`crates/vike-model/tests/libm_walk_selftest.rs`), which holds the mechanism without \
         needing a witness in the tree. Multi-range files seen: [{}]",
        walk.iter()
            .filter(|w| w.ranges > 1)
            .map(|w| format!(
                "{} ranges={} first_end={} next_start={:?} lines={} visited_past_marker={}",
                w.file, w.ranges, w.first_end, w.next_start, w.lines, w.visited_past_first_marker
            ))
            .collect::<Vec<_>>()
            .join("; ")
    );
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
        "production code must call the `libm` CRATE rather than `f64`'s platform-libm methods, so \
         two boxes draw the same axis. Most of this crate's exposure really is a pixel — but \
         `scale.rs`'s `log_nice_values` floors and ceils a logarithm, which turns a last bit into a \
         whole DECADE of gridlines. BOTH spellings are banned, `x.log10()` and `f64::log10(x)`: \
         same inherent method, same libcall (see `banned_needles`). `powi` is banned too; the one \
         base-ten production site that keeps it is named in `POWI_BASE_TEN_EXEMPT`, with both the \
         measurement behind it and the residual that measurement does not cover:\n{}",
        found.join("\n")
    );
}
