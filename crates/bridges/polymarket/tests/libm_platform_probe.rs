//! The cross-platform PIN for `toxicity_agg` — this bridge's only transcendental site, and the
//! only one in any venue crate.
//!
//! IEEE 754 requires `+ - * /` and `sqrt` to be correctly rounded and requires NOTHING of `exp`, so
//! an `f64` METHOD call reaches whichever libm the PLATFORM ships — MSVC's CRT on the Windows dev
//! box, glibc on the the CI box Linux boxes and every CI runner — and the two disagree in the last bit.
//! `crates/bridges/polymarket/src/toxicity_agg.rs`'s `SideAccum` — its `decay_to` and its `reading`
//! were converted to the `libm` CRATE for that reason, and
//! `docs/decisions/0032-transcendentals-come-from-the-libm-crate-not-the-platform.md` is the
//! accepted verdict. The conversion landed with NO pin, so nothing in the tree would have noticed
//! the next edit spelling `(…).exp()` again.
//!
//! # What the pinned value reaches
//!
//! A `vike_model::FlowToxicity` reading is consumed by a mounted maker on its `on_flow` hook, where it
//! is a GUARD: a reading is compared against a threshold, and the maker widens, skews or stops
//! quoting on the far side of it. So the exposure is not "a diagnostic column is one ulp out" — it
//! is that two boxes running the same maker over the same wallet-classified tape can make different
//! quoting decisions at a threshold crossing. Stated at its true size, though: this is a threshold
//! comparison on a value that moves continuously with every trade, so a last-bit disagreement
//! matters only for the tick where the reading sits exactly on the boundary. That is rarer than the
//! decade-cliff shape in `crates/vike-app-core/src/orderflow.rs` and commoner than "never".
//!
//! # ⚠ This crate cannot be verified locally, and the pin is written accordingly
//!
//! Everything below is pure arithmetic over an in-process struct: no CLOB call, no WS, no
//! credentials, no geo-restricted host. That is deliberate — a pin that needed the venue would be a
//! pin nobody could re-record. CI's `--features polymarket` lane runs it on Linux; `just t
//! vike-polymarket` with a feature runs it on the Windows dev box.
//!
//! ⚠ **The MINIMAL feature is `feeds`, not `polymarket`.** A default (no-feature) build of this
//! crate is EMPTY — the crate-level gate is `#![cfg(feature = "feeds")]` — so a probe run with no
//! `--features` compiles an empty lib and an empty test binary and reports `0 passed` while
//! measuring nothing. Everything this file touches (`toxicity_agg`, `rtds`, `wallet_class`) is
//! declared with no per-module `#[cfg]` of its own, i.e. behind `feeds` alone; the signing/exec
//! plane `polymarket` adds is reached by nothing here. `feeds` is the better recording lane for
//! being the narrower one — it links no k256/keccak stack — and it is not a weaker one for
//! [`production_code_calls_libm_not_the_platform`] either: that gate reads `src/` off DISK, so it
//! scans every `polymarket`-gated file whether or not the feature compiled it.
//!
//! # ⚠ The corpus is PURE ARITHMETIC, and that is load-bearing
//!
//! Every input is an integer timestamp or an LCG-derived size built from `+ - * /`, never from
//! `.sin()`/`.cos()`. Those are themselves platform-dependent (MEASURED —
//! `crates/vike-analytics/tests/libm_platform_probe.rs`'s Part A sweeps them as its own control),
//! so generating inputs with them would make this row diverge because of the INPUTS rather than
//! because of the function under test.
//!
//! ⚠ **No `--ignored`.** Every test in this file is ORDINARY (see
//! [`converted_functions_are_platform_invariant`]), and `--ignored` runs ONLY ignored tests — so
//! adding it filters all four out and the command exits 0 having measured nothing.
//!
//! ```text
//! <cargo> test -p vike-polymarket --features feeds      --test libm_platform_probe   # minimal
//! <cargo> test -p vike-polymarket --features polymarket --test libm_platform_probe   # CI's lane
//! ```

#![cfg(feature = "feeds")]

use vike_polymarket::rtds::TradeSide;
use vike_polymarket::toxicity_agg::ToxicityAggregator;
use vike_polymarket::wallet_class::WalletClass;

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

/// One row's verdict over its own corpus.
struct Row {
    label: &'static str,
    hash: String,
    n: usize,
    /// ⚠ NON-VACUITY GUARD — how many DISTINCT FINITE values the samples took. A row that collapses
    /// to one value hashes identically on every platform and reads as a PASS while pinning nothing.
    /// The analytics twin caught a real instance of exactly that (`overfit::pbo_cscv`), and this
    /// aggregator has TWO ways to become that row: an empty toxic-class set makes every reading
    /// exactly `0.0`, and a `total` below `TOTAL_EPS` short-circuits before the `exp` is ever
    /// called. The corpus below is built to avoid both, and this field is what proves it did.
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

/// Independent tapes folded into the row.
const TAPES: usize = 96;
/// Trades per tape.
const TRADES: usize = 64;

// =================================================================================================
// The corpus.
// =================================================================================================

/// The one covered site, driven through the PUBLIC verbs.
///
/// ⚠ `SideAccum::decay_to` and `SideAccum::reading` are private methods on a private struct, and
/// they are deliberately not made reachable in order to pin them. Driving `observe`/`current` is
/// the stronger pin anyway: `decay_to` runs INSIDE `observe`, so its `exp` compounds into the
/// accumulator state that every later reading is computed from, and `reading`'s own freshness `exp`
/// is applied on top. A row over the two functions in isolation would miss the compounding, which
/// is the part that makes a last bit persist rather than wash out.
///
/// Both mechanisms are exercised deliberately:
///
/// * **decay-at-`observe`** needs a tape whose gaps VARY. A fixed-cadence tape applies the same
///   `exp(-Δt/window)` at every fold, so the whole row would pin one value of the function under
///   test evaluated many times. The gaps below span sub-window to multi-window.
/// * **the freshness factor** needs `current` to be called at times PAST the last trade, not just
///   at it. A reading taken exactly at `last_ts` takes the `now_ms > self.last_ts` false branch and
///   returns the fraction with NO `exp` at all — a sample that cannot report a divergence.
fn production_rows() -> Vec<Row> {
    let mut vals = Vec::new();
    let mut fresh_exp_samples = 0usize;
    let mut s = 0xdead_beef_cafe_babeu64;

    for _ in 0..TAPES {
        // A window from a fraction of a second to a couple of minutes. The bin's own default is
        // 30 s (`--tox-window-secs`), which sits inside this range rather than being the only
        // point in it — an operator sets the flag, and a site portable only at its default is
        // portable by accident.
        let window_ms = 250 + (lcg(&mut s) * 120_000.0) as i64;
        let mut agg = ToxicityAggregator::new(window_ms);
        let mut ts = 1_700_000_000_000i64;
        for _ in 0..TRADES {
            // Gaps from one millisecond to several windows, so the decay factor spans nearly its
            // whole useful range rather than clustering at one value.
            ts += 1 + (lcg(&mut s) * (window_ms as f64) * 3.0) as i64;
            // ⚠ A MIXED class stream. An all-toxic tape drives the fraction to exactly 1.0 and an
            // all-retail one to exactly 0.0 — the aggregator's own tests assert both as bit-exact
            // properties — and either would collapse this row onto a constant. `Sharp` and `Whale`
            // are the default toxic set; `Retail` and `Unknown` feed `total` only.
            let class = match (lcg(&mut s) * 4.0) as usize {
                0 => WalletClass::Sharp,
                1 => WalletClass::Whale,
                2 => WalletClass::Retail,
                _ => WalletClass::Unknown,
            };
            let side = if lcg(&mut s) < 0.5 { TradeSide::Buy } else { TradeSide::Sell };
            let size = 1.0 + lcg(&mut s) * 5_000.0;
            agg.observe(side, class, size, ts);

            // Read at a time strictly AFTER the last trade, so the freshness `exp` actually runs.
            let ahead = 1 + (lcg(&mut s) * (window_ms as f64) * 2.0) as i64;
            let flow = agg.current(ts + ahead);
            vals.push(flow.bid);
            vals.push(flow.ask);
            // ⚠ CONDITIONAL, and the condition is the entire guard. An unconditional `+= 2` makes
            // the assertion below read `2·TAPES·TRADES >= TAPES·TRADES` — true for EVERY possible
            // `ahead`, the `ahead = 0` included, which is precisely the corpus the assertion claims
            // to rule out (every read back ON `last_ts`, `reading`'s `now_ms > self.last_ts` branch
            // never taken, not one freshness `exp` evaluated). A guard that cannot fail is not a
            // guard; counting only the reads that are strictly AFTER the last trade is what makes
            // it able to.
            if ahead > 0 {
                fresh_exp_samples += 2;
            }
        }
        // ...and one read AT the last trade per tape, which takes the `now_ms > last_ts` FALSE
        // branch. That arm has no `exp` in it and is pinned here for the opposite reason to
        // everything above: so a future edit that quietly puts one there goes red.
        let flow = agg.current(ts);
        vals.push(flow.bid);
        vals.push(flow.ask);
    }

    // The non-vacuity guard that matters for THIS function, beyond the generic `distinct` floor: a
    // corpus whose reads all happen at `last_ts` would exercise no freshness `exp` at all while
    // still producing plenty of distinct values from the fraction alone.
    assert!(
        fresh_exp_samples >= TAPES * TRADES,
        "only {fresh_exp_samples} samples were read strictly AFTER the last trade, so most of this \
         row never reached `reading`'s freshness `exp`. Check the `ahead` computation before \
         re-recording."
    );

    vec![finish("toxicity_agg::ToxicityAggregator (exp x2)", &vals)]
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
/// match one platform silently un-does the fix on the other.
///
/// ⚠ **A per-platform table (`#[cfg(windows)]` / `#[cfg(unix)]`) is NOT a repair and must never be
/// added.**
const PLATFORM_INVARIANT: [(&str, &str); 1] =
    [("toxicity_agg::ToxicityAggregator (exp x2)", "271189c6da7a233c")];

/// The cross-platform proof, as a GATE rather than a diff somebody has to remember to run.
///
/// NOT `#[ignore]`d, and not credential-gated the way this crate's smokes are: it touches no
/// network and no venue, so it runs wherever the `feeds` feature is on — which the CI
/// `--features polymarket` lane implies.
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
            bad.push(format!("  {:<44} want={want_hash} got={}", r.label, r.hash));
        }
    }

    // Non-vacuity BEFORE the hashes: a thin row is a defect whether or not its hash matched, and an
    // author re-recording a `"RECORD"` row must be told about it before pasting.
    let thin: Vec<String> = got
        .iter()
        .filter(|r| r.distinct < MIN_DISTINCT)
        .map(|r| format!("  {:<44} {} distinct finite of {} samples", r.label, r.distinct, r.n))
        .collect();
    assert!(
        thin.is_empty(),
        "this row's corpus is too thin to detect a last-bit divergence: it would hash identically \
         on every platform and read as a PASS. The two ways this aggregator degenerates are an \
         all-one-class tape (fraction pinned to 0.0 or 1.0) and a `total` under TOTAL_EPS (the \
         `exp` is never reached). WIDEN the corpus — never lower MIN_DISTINCT ({MIN_DISTINCT}):\n{}",
        thin.join("\n")
    );

    if !bad.is_empty() {
        let paste: String =
            got.iter().map(|r| format!("    (\"{}\", \"{}\"),\n", r.label, r.hash)).collect();
        panic!(
            "\nvike-polymarket toxicity output moved away from the platform pin:\n{}\n\n\
             There is no tolerance here to widen, and hand-editing a digit defeats the gate.\n\
             If the want= column reads RECORD this is an UNFINISHED RECORDING rather than a\n\
             regression: run this file on BOTH Windows and Linux, confirm they agree, paste.\n\
             For an already-recorded row, establish WHY first:\n\
               (a) an intended numeric change — re-record on BOTH boxes and pin the agreed hash; or\n\
               (b) a call site went back to `(…).exp()`, in which case the two platforms will NOT\n\
                   agree and the fix is in the source, not here.\n\n\
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
/// guard did nothing — and this aggregator's degenerate cases return EXACTLY `0.0`, so the
/// constant-series control is the shape it would actually take here.
#[test]
fn the_non_vacuity_check_discriminates() {
    assert!(
        finish("control::constant", &(0..4096).map(|_| 0.0f64).collect::<Vec<f64>>()).distinct
            < MIN_DISTINCT,
        "the non-vacuity check does not discriminate: a constant series passed it"
    );
    assert_eq!(
        finish("control::nan", &(0..4096).map(|_| f64::NAN).collect::<Vec<f64>>()).distinct,
        0,
        "an all-NaN row must report ZERO distinct FINITE values"
    );
    // The degenerate configuration this file's corpus is built to avoid, run as a real control: an
    // EMPTY toxic-class set makes every reading exactly 0.0, which is a perfectly stable value on
    // every platform. If the corpus above ever drifted into this shape it would look like a pass.
    let mut agg = ToxicityAggregator::with_toxic_classes(30_000, Vec::new());
    let mut vals = Vec::new();
    for i in 0..256i64 {
        agg.observe(TradeSide::Buy, WalletClass::Sharp, 10.0, 1_000 + i * 137);
        let flow = agg.current(1_000 + i * 137 + 500);
        vals.push(flow.bid);
        vals.push(flow.ask);
    }
    assert!(
        finish("control::no-toxic-classes", &vals).distinct < MIN_DISTINCT,
        "an empty toxic-class set must produce a degenerate corpus — if it does not, the \
         aggregator's documented `[0, 1]` contract has changed and this control is measuring \
         something else"
    );
}

// =================================================================================================
// The SOURCE half. The table above proves today's output agrees; this stops the NEXT edit from
// reintroducing a platform call in a spot the corpus never reaches.
// =================================================================================================

/// The `f64` methods whose last bit is the PLATFORM's business rather than IEEE 754's, as bare
/// names. [`banned_needles`] renders each into the two spellings that reach it.
///
/// `sqrt` is deliberately absent and must stay absent: IEEE 754 requires it correctly rounded, so
/// it is identical on every box and there is nothing to convert it TO. `to_degrees`/`to_radians`
/// are absent for the same reason wearing a different disguise.
///
/// `powi` IS here: MEASURED on MSVC, a `dev` build lowers `llvm.powi` to the CRT's `pow()`, so it
/// is a libcall rather than the free multiply chain its name suggests. Its cure is a MULTIPLICATION
/// where the base is a literal and `libm::pow` where it is a runtime `f64`;
/// `crates/bridges/polymarket/src` has no `powi` to exempt today.
const BANNED_FNS: [&str; 26] = [
    "powf", "ln", "log", "log2", "log10", "exp", "exp2", "exp_m1", "ln_1p", "sin", "cos", "tan",
    "powi", "atan", "atan2", "asin", "acos", "sinh", "cosh", "tanh", "cbrt", "hypot", "asinh",
    "acosh", "atanh", "sin_cos",
];

/// Every [`BANNED_FNS`] name in BOTH spellings that reach the platform's libm.
///
/// ⚠ The UFCS half is not decoration. A gate that bans `.exp(` and nothing else walks straight past
/// `f64::exp(x)` — the same inherent method, called through its fully-qualified path, compiling to
/// the same libcall. Neither spelling is more correct Rust and rustfmt rewrites neither into the
/// other. The `f64::NAME(` needle also covers `std::primitive::f64::NAME(` and
/// `core::primitive::f64::NAME(` for free, because both END with exactly that pattern.
///
/// ⚠ **What it still cannot see, stated rather than implied:** `<f64>::exp(x)` (the angle-bracket
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
/// `crates/bridges/polymarket/src`: `egress.rs` carries markers at 521 and 840 of 878 lines, with
/// `API_PERMITTED_WHEN_FRONTEND_BLOCKED` and `api_placement_permitted` — the geoblock policy this
/// bridge REFUSES to place orders on — sitting between them. A `break` would put that region
/// outside this gate's view. `crates/vike-mm/src/platform_probe.rs` is where that failure was first
/// measured, and where two `libm` sites were found hiding in the discarded region.
///
/// ⚠ **TWO marker spellings are recognised, and this crate has the second.**
/// `crates/bridges/polymarket/src/filters_rec.rs` opens its tests with
/// `#[cfg(all(test, feature = "polymarket"))]`. A walk that knows only the bare `#[cfg(test)]`
/// spelling treats that whole module as production and reports every banned needle in it — a LOUD
/// false positive on a file nobody touched. `#[cfg(any(test, feature = "…"))]` is deliberately NOT
/// a marker: that attribute marks code that SHIPS whenever the feature is on
/// (`crates/vike-model/src/strategy/mod.rs`'s `MockBroker` is the workspace's example), so it is
/// production for this gate's purposes.
///
/// ⚠ **And the cut is LINE BY LINE with the comment filter FIRST.** A whole-file
/// `text.find("#[cfg(test)]")` truncates at the first occurrence of that string ANYWHERE, a doc
/// comment included, and a `//`-skipping filter applied afterwards cannot rescue it because it runs
/// on already-truncated text. This crate has one such prose mention today
/// (`crates/bridges/polymarket/src/tick_regime.rs`), so a whole-file cut would blank most of that
/// file and report a confident zero for it.
///
/// # How an item's end is found, and why by INDENTATION rather than by counting braces
///
/// A brace count needs to know which braces are code, which puts a Rust string/char/comment lexer
/// inside a gate — and this workspace's test modules are full of `format!("{}")` and of assert
/// messages continued with a trailing `\`, so a naive count is wrong on exactly the files that
/// matter. Indentation needs no lexer and is exact here for a STRUCTURAL reason: `cargo fmt
/// --check` is CI's first gate, so every file in the tree is rustfmt output, and rustfmt closes a
/// block with a `}` alone on a line at the block's OWN indentation. Verified over this crate
/// 2026-08-29: every marker under `crates/bridges/polymarket/src` sits at column 0.
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
/// `egress.rs` AS IT IS TODAY — move its geoblock policy above the first test module and that
/// assertion passes vacuously while the mechanism goes unproven. This fixture holds the mechanism
/// itself, and every property the walk needs.
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
        "#[cfg(all(test, feature = \"polymarket\"))]",
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
    assert_eq!(
        items[1],
        (16, 22, true),
        "the `all(test, …)` module is test-only and is a range — `filters_rec.rs` uses exactly this \
         spelling, so a walk that misses it reports that whole module as production"
    );
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

/// Production code in this bridge must reach a transcendental through the `libm` CRATE.
///
/// ⚠ Not redundant with the hash table. That table pins ONE aggregator; this crate is a whole venue
/// adapter — order signing, the CLOB REST/WS planes, settlement, rewards, the fee and tick-regime
/// tables — and a new transcendental anywhere in it would be invisible to the corpus above while
/// quietly making a signed order's numbers depend on which box produced them.
///
/// Scope: every `.rs` under `src/`, recursively, MINUS each file's test-item ranges. There are NO
/// exemptions and there should not need to be: measured 2026-08-29, `crates/bridges/polymarket/src`
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
                    continue; // a doc comment naming `exp` is prose, not a call
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
    // (`settlement/`), so a walk that stopped at the top level would still open ~45 files and
    // report a confident zero while never reading a subdirectory.
    for must in ["toxicity_agg.rs", "egress.rs", "filters_rec.rs"] {
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
    // `egress.rs` carries the geoblock policy between two test modules — measured 2026-08-29 at 11
    // non-blank production lines below the first marker. If this goes red, the walk has reverted to
    // treating the first marker as the end of the shipped file.
    assert!(
        scanned_past_a_test_module.iter().any(|s| s == "egress.rs"),
        "no production line below egress.rs's first test marker was scanned, so this gate has gone \
         blind to the region below every mid-file test module — which here holds the geoblock \
         policy that decides whether this bridge places an order at all. Scanned past a test \
         module: {scanned_past_a_test_module:?}"
    );
    assert!(
        found.is_empty(),
        "production code must call the `libm` CRATE rather than `f64`'s platform-libm methods, so a \
         flow-toxicity reading — which a mounted maker uses as a QUOTING GUARD — is a property of \
         the tape rather than of the box that replayed it. BOTH spellings are banned, `x.exp()` and \
         `f64::exp(x)`: same inherent method, same libcall (see `banned_needles`). `powi` is banned \
         too — on MSVC a `dev` build makes it a `pow()` libcall:\n{}",
        found.join("\n")
    );
}
