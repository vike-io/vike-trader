//! MEASUREMENT probe **and** the committed cross-platform GATE for the maker's quote math.
//!
//! IEEE 754 requires `+ - * /` and `sqrt` to be correctly rounded; it does NOT require `exp`,
//! `ln` or `pow`, so two C runtimes may disagree in the last bit. `crates/vike-indicators` and
//! `crates/vike-analytics` each measured that disagreement, found it real, and converted their
//! production paths to the `libm` CRATE — the verdict is
//! `docs/decisions/0032-transcendentals-come-from-the-libm-crate-not-the-platform.md`.
//!
//! ⚠ **`vike-mm` was never measured, and it is the crate where a divergence would cost money.**
//! A sweep on 2026-08-25 declared it as its own blind spot: this crate had no probe, no pin and no
//! source gate, while carrying **fifteen** production `f64` transcendental METHOD calls
//! across twelve functions — and the functions are not incidental. `avellaneda`'s reservation
//! price and optimal half-spread ARE the quotes this maker posts; `spread_source`'s LS-LMSR prices and
//! `fairvalue`'s LMSR reservation are the same for the prediction-market side. Two boxes running
//! the same strategy on the same tape could therefore quote a different price, and nothing would
//! have said so.
//!
//! # Measured first, converted second — in that order, and the order is the point
//!
//! `CLAUDE.md`'s playbook: **step 1 declares today's reality byte-identically; step 2 flips
//! behaviour behind evidence.** This file was written as step 1 alone and landed the measurement
//! before a single production line moved. The measurement then said the seven-of-nine divergence
//! was real, and step 2 followed: all FIFTEEN call sites now route through the `libm` crate.
//!
//! ⚠ **CORRECTED 2026-08-26: this file said THIRTEEN throughout, and the number was wrong by two
//! — the two the gate below could not see.** It had already stopped agreeing with itself: one
//! bullet said the source gate "stops the FOURTEENTH site being written as `x.exp()`" while that
//! gate's own doc said THIRTEENTH, which is the shape a count takes when it is being maintained
//! by hand in several places at once. It also said those thirteen sat "across nine functions",
//! wrong by one on its own arithmetic (the nine [`PINNED`] names carry
//! twelve sites; the thirteenth is a tenth function). The real roster is FIFTEEN sites across
//! TWELVE functions, and the missing pair is `crates/vike-mm/src/avellaneda.rs`'s
//! `effective_blackout_ms` (an `exp`) and its `moneyness_z` (a `log`), both on `AsState`. Both sit
//! BELOW that file's first `#[cfg(test)] mod gueant_tests` and above five more test modules, and
//! [`production_code_calls_libm_not_the_platform`] used to `break` its per-file scan at the first
//! marker — so 633 of `avellaneda.rs`'s 1921 lines, and 972 of `lib.rs`'s 1104, were outside the
//! gate's view while it reported a confident zero. The count was not sloppy bookkeeping; it was
//! the gate's blind spot being copied into prose. The scan now records each `#[cfg(test)]` item's
//! RANGE and excludes only those ranges, and the correction is kept here rather than tidied away
//! because "all THIRTEEN call sites" read as a complete inventory and was cited as one.
//!
//! [`PINNED`] carries both halves — the before-and-after table — because a set of agreeing numbers
//! looks the same whether it was always true or was made true, and only one of those is a claim
//! worth trusting.
//!
//! What this gives the crate, stated exactly so nobody reads more into it:
//!
//! * [`platform_pin`] holds ONE table on both boxes, so a future edit that moves a quote goes red.
//! * [`platform_pin_stateful`] does the same for the three sites [`PINNED`] cannot reach, which
//!   until 2026-08-29 were declared gaps rather than rows.
//! * [`production_code_calls_libm_not_the_platform`] stops the SIXTEENTH site being written as
//!   `x.exp()` — or as `f64::exp(x)`, the spelling it did not see until 2026-08-26. The two are
//!   not redundant: the pin's corpus reaches nine functions, and a call in a tenth is invisible to
//!   it while quoting a platform-dependent price. That was not hypothetical — until 2026-08-29
//!   three such calls existed, and Declared coverage below named each one. All three now have rows
//!   in [`PINNED_STATEFUL`], so the source gate is back to being a tripwire on the NEXT one rather
//!   than the only thing holding three live sites.
//! * It does **not** make these values match what the platform libm used to compute. The `libm`
//!   crate agrees with neither MSVC nor glibc, so this moved numbers on both boxes — the cost
//!   `docs/decisions/0032-transcendentals-come-from-the-libm-crate-not-the-platform.md` states as
//!   the price of reproducibility.
//!
//! ⚠ **The thirteenth site was found by the source gate, not by the survey**, and it is worth
//! knowing why. `crates/vike-mm/src/xemm/basis.rs` opens with a house-style doc line that spells
//! `#[cfg(test)]` inside backticks, forty lines above any real marker. A survey that cuts a file
//! at its first `#[cfg(test)]` SUBSTRING therefore stopped at that comment and never reached the
//! `powf` on line 89 — which is exactly the defect the gate below is written against, caught in
//! this crate on its first run.
//!
//! # Why the probe lives INSIDE `src/`
//!
//! Every one of these functions is `pub(crate)` inside a private module (`mod alpha;`, not
//! `pub mod alpha;`), so an integration test under `tests/` cannot reach a single one. The
//! alternative — widening the API, or adding a `test-support` feature — would change the crate's
//! public surface to measure it, which is exactly the kind of change step 1 forbids. A
//! `#[cfg(test)]` module compiles into no shipped build.
//!
//! # Running it, and the `--ignored` trap in the first spelling
//!
//! ⚠ **`--ignored` runs ONLY the ignored tests**, and [`platform_probe`] is the one ignored test
//! here — every GATE in this file ([`platform_pin`], [`platform_pin_stateful`],
//! [`every_pinned_row_is_non_vacuous`], [`production_code_calls_libm_not_the_platform`] and the two
//! walk controls) is an ORDINARY test that `--ignored` filters OUT. A recording session driven by
//! the second line alone therefore exits 0 having asserted nothing, which is how a run can look
//! green while the outstanding rows go unmeasured. Use the FIRST line to record and gate; the
//! second only to eyeball two boxes side by side.
//!
//! ```text
//! <cargo> test -p vike-mm --lib platform_probe                       # the GATES (ordinary tests)
//! <cargo> test -p vike-mm --lib platform_probe::platform_pin_stateful -- --nocapture
//! <cargo> test -p vike-mm --lib platform_probe -- --ignored --nocapture   # the printed table only
//! ```
//!
//! An unrecorded row is recorded by RUNNING the second line on BOTH boxes: it fails on a `"RECORD"`
//! row and its panic carries the paste-ready `PINNED_STATEFUL` block. Paste only the hash the two
//! boxes AGREE on.
//!
//! # ⚠ The corpus is PURE ARITHMETIC, and that is load-bearing
//!
//! Inputs are generated by an LCG and plain `+ - * /`, never by `.sin()`/`.cos()`. Those are
//! themselves platform-dependent (measured, in the indicators twin), so generating inputs with
//! them would make every column below diverge because of the INPUTS rather than because of the
//! function under test — a false positive that reads exactly like a real one.
//!
//! # Declared coverage
//!
//! [`PINNED`] covers NINE functions carrying TWELVE of the FIFTEEN converted sites. Each row
//! names the site it exists for. [`PINNED_STATEFUL`] covers the remaining THREE.
//!
//! ⚠ **CORRECTED 2026-08-29. This section said "THREE sites are covered by the SOURCE GATE and not
//! by the pin", and named each as a declared gap. That is no longer true — all three now have
//! rows** — but the reason it WAS true is worth keeping, because it is the reason the gaps went
//! unnoticed for so long. Two of the three (`effective_blackout_ms`, `moneyness_z`) were invisible
//! to the source gate as well, so they were invisible to the person writing the gap list too; the
//! list could only name the one gap the gate could see. What the three genuinely share is that none
//! is a `pub(crate)` scalar function this file can call with plain numbers: pinning any of them
//! means driving a STATEFUL object, which is a different test with different failure modes — hence
//! a second table rather than three more [`PINNED`] rows.
//!
//! * `crates/vike-mm/src/xemm/basis.rs`'s `observe` (on `BasisEwma`) — the EWMA weight inside the
//!   cross-exchange basis tracker, reached through `XemmMaker`. Reverting it to `0.5_f64.powf(..)`
//!   reddens [`production_code_calls_libm_not_the_platform`]; now it reddens the pin too.
//! * `crates/vike-mm/src/avellaneda.rs`'s `effective_blackout_ms` — a private method on `AsState`,
//!   reachable only through `in_blackout` on a state that already holds an underlying.
//!   ⚠ Its divergence would be DISCRETE rather than last-bit: the `exp` feeds
//!   `(base as f64 * factor).round() as i64`, so a one-ulp disagreement either side of a `.5`
//!   boundary moves the blackout window by a whole millisecond and flips `in_blackout` on one box
//!   and not the other. That site's own comment carries the argument, and
//!   [`PINNED_STATEFUL`]'s row is built to catch exactly that flip rather than a last bit.
//! * `crates/vike-mm/src/avellaneda.rs`'s `moneyness_z` — likewise private, and the input to the
//!   one above; the same row covers it, because there is no way to reach one without the other.
//!
//! `.sqrt()` calls are NOT covered and need no coverage: IEEE 754 requires `sqrt` to be correctly
//! rounded, so it is already identical everywhere — `crates/vike-analytics/src/lib.rs` carries the
//! same exemption and the same reason.

use crate::{alpha, avellaneda, fairvalue, fits, spread_source, underlying};

/// FNV-1a over `f64::to_bits`, NaN canonicalised so a NaN's payload cannot make two boxes differ
/// for a reason that is not the arithmetic. The same hasher the two sibling probes use.
struct Fnv(u64);

impl Fnv {
    fn new() -> Self {
        Self(0xcbf2_9ce4_8422_2325)
    }
    fn push(&mut self, v: f64) {
        let bits = if v.is_nan() { 0x7ff8_0000_0000_0000 } else { v.to_bits() };
        for b in bits.to_le_bytes() {
            self.0 ^= u64::from(b);
            self.0 = self.0.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    fn hex(&self) -> String {
        format!("{:016x}", self.0)
    }
}

/// Deterministic pseudo-random f64 in `(0, 1)`, from a pure-integer LCG.
///
/// ⚠ No transcendental anywhere in here, for the reason the module doc gives. The `as f64` and the
/// single division are exact-or-correctly-rounded operations, so the corpus is bit-identical on
/// every platform by construction — which is what makes a divergence in the OUTPUT attributable to
/// the function under test.
fn lcg(seed: &mut u64) -> f64 {
    *seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
    // Top 53 bits over 2^53: an exact ratio of integers, then one correctly-rounded divide.
    ((*seed >> 11) as f64) / ((1u64 << 53) as f64)
}

/// Number of samples per row. Large enough that a last-bit disagreement on a rare input survives
/// into the hash rather than being rounded away — the trap the indicators twin fell into twice
/// with a single series, where two genuinely divergent indicators both reported "identical".
const SAMPLES: usize = 4096;

/// Every covered function, hashed over the corpus.
///
/// Returns `(label, hash)` rows in a FIXED order; [`PINNED`] is compared positionally, so a
/// reordering is a re-record rather than a silent re-pairing.
fn rows() -> Vec<(String, String)> {
    let mut out = Vec::new();

    // --- alpha::ofi_toxicity — `1.0 - (-ofi.abs() / scale).exp()`
    {
        let mut h = Fnv::new();
        let mut s = 0x1234_5678_9abc_def0u64;
        for _ in 0..SAMPLES {
            let ofi = (lcg(&mut s) - 0.5) * 200.0;
            let scale = 0.5 + lcg(&mut s) * 50.0;
            h.push(alpha::ofi_toxicity(ofi, scale));
        }
        out.push(("alpha::ofi_toxicity (exp)".to_string(), h.hex()));
    }

    // --- avellaneda::ewma_alpha — `1.0 - 0.5_f64.powf(1.0 / half_life)`
    {
        let mut h = Fnv::new();
        let mut s = 0x0fed_cba9_8765_4321u64;
        for _ in 0..SAMPLES {
            let half_life = 0.5 + lcg(&mut s) * 500.0;
            h.push(avellaneda::ewma_alpha(half_life));
        }
        out.push(("avellaneda::ewma_alpha (powf)".to_string(), h.hex()));
    }

    // --- avellaneda::as_intensity_halfspread — `(1.0 / gamma) * (1.0 + gamma / kappa).ln()`
    {
        let mut h = Fnv::new();
        let mut s = 0x2222_3333_4444_5555u64;
        for _ in 0..SAMPLES {
            let gamma = 1e-4 + lcg(&mut s) * 5.0;
            let kappa = 1e-3 + lcg(&mut s) * 200.0;
            h.push(avellaneda::as_intensity_halfspread(gamma, kappa));
        }
        out.push(("avellaneda::as_intensity_halfspread (ln)".to_string(), h.hex()));
    }

    // --- avellaneda::gueant_skew_coeff — `(… * (1.0 + ratio).powf(1.0 + kappa / gamma)).sqrt()`
    {
        let mut h = Fnv::new();
        let mut s = 0x6666_7777_8888_9999u64;
        for _ in 0..SAMPLES {
            let sigma2 = 1e-8 + lcg(&mut s) * 1e-2;
            let gamma = 1e-4 + lcg(&mut s) * 5.0;
            let kappa = 1e-3 + lcg(&mut s) * 200.0;
            let a = 1e-3 + lcg(&mut s) * 10.0;
            h.push(avellaneda::gueant_skew_coeff(sigma2, gamma, kappa, a));
        }
        out.push(("avellaneda::gueant_skew_coeff (powf)".to_string(), h.hex()));
    }

    // --- fairvalue::lmsr_reservation — `(mid / (1.0 - mid)).ln()` and `(-shifted).exp()`
    {
        let mut h = Fnv::new();
        let mut s = 0xaaaa_bbbb_cccc_ddddu64;
        for _ in 0..SAMPLES {
            // Kept strictly inside (0, 1): a probability of exactly 0 or 1 makes the logit
            // infinite, and an infinity hashes the same on every platform, so those samples would
            // dilute the measurement rather than sharpen it.
            let mid = 0.001 + lcg(&mut s) * 0.998;
            let net_inventory = (lcg(&mut s) - 0.5) * 2_000.0;
            let b = 1.0 + lcg(&mut s) * 500.0;
            h.push(fairvalue::lmsr_reservation(mid, net_inventory, b));
        }
        out.push(("fairvalue::lmsr_reservation (ln + exp)".to_string(), h.hex()));
    }

    // --- spread_source::ls_lmsr_prices — two `.exp()` and one `.ln()`
    {
        let mut h = Fnv::new();
        let mut s = 0x1111_2222_3333_4444u64;
        for _ in 0..SAMPLES {
            let q_yes = lcg(&mut s) * 10_000.0;
            let q_no = lcg(&mut s) * 10_000.0;
            let a = 0.001 + lcg(&mut s) * 0.5;
            let (y, n) = spread_source::ls_lmsr_prices(q_yes, q_no, a);
            h.push(y);
            h.push(n);
        }
        out.push(("spread_source::ls_lmsr_prices (exp x2 + ln)".to_string(), h.hex()));
    }

    // --- fits::fit_kappa_censored and fits::fit_base_intensity — `(-k * delta).exp()`
    //
    // Both take the same censored-observation slice, so one corpus drives both. `Option::None` is
    // hashed as a canonical NaN rather than skipped: whether a fit CONVERGES is itself part of the
    // behaviour being pinned, and skipping the misses would hide a platform that stops converging.
    {
        let mut hk = Fnv::new();
        let mut hb = Fnv::new();
        let mut s = 0xdead_beef_cafe_babeu64;
        for _ in 0..64 {
            let obs: Vec<(f64, f64, bool)> = (0..64)
                .map(|_| {
                    let delta = lcg(&mut s) * 0.05;
                    let t = 0.01 + lcg(&mut s) * 10.0;
                    (delta, t, lcg(&mut s) < 0.7)
                })
                .collect();
            let k = fits::fit_kappa_censored(&obs, 1.0, 500.0);
            hk.push(k.unwrap_or(f64::NAN));
            hb.push(fits::fit_base_intensity(&obs, k.unwrap_or(50.0)).unwrap_or(f64::NAN));
        }
        out.push(("fits::fit_kappa_censored (exp)".to_string(), hk.hex()));
        out.push(("fits::fit_base_intensity (exp)".to_string(), hb.hex()));
    }

    // --- underlying::UnderlyingTracker::observe — `price.ln()`
    //
    // The only STATEFUL site: the log-return variance folds across calls, so it is driven as a
    // sequence rather than as independent samples, and every returned triple is hashed.
    {
        let mut h = Fnv::new();
        let mut s = 0x5555_6666_7777_8888u64;
        let mut tracker = underlying::UnderlyingTracker::new();
        let mut price = 100.0f64;
        let mut ts = 1_700_000_000_000i64;
        // ⚠ `resolution_ts` MUST be `Some`. `observe` opens with `let t_res = resolution_ts?`, so
        // passing `None` returns on the second line and the tracker never folds anything — the
        // first draft of this row did exactly that and hashed the FNV seed itself,
        // `cbf29ce484222325`, which is a perfectly stable value on every platform and would have
        // pinned NOTHING while looking like coverage. `every_pinned_row_is_non_vacuous` does not
        // cover this row (it is stateful, not a sample generator), so the guard here is the
        // assertion below.
        // `window_open_ms = resolution_ts - window_secs * 1000`, and `s_open` is captured only once
        // `ts >= window_open_ms`. Anchoring the resolution one window AHEAD of the first tick puts
        // the window open from the very first observation; a resolution further out leaves it shut
        // for the whole run, which is the second way this row folded nothing.
        let resolution_ts = ts + 60_000;
        let mut folded = 0usize;
        for _ in 0..SAMPLES {
            // A multiplicative walk with a purely arithmetic step — no transcendental in the input.
            price *= 1.0 + (lcg(&mut s) - 0.5) * 0.002;
            ts += 250;
            if let Some((a, b, c)) = tracker.observe(price, ts, 60.0, Some(resolution_ts)) {
                h.push(a);
                h.push(b);
                h.push(c);
                folded += 1;
            }
        }
        assert!(
            folded > SAMPLES / 2,
            "the underlying row folded only {folded}/{SAMPLES} observations — a row that folds \
             nothing hashes the FNV seed and pins nothing. Check `observe`'s early returns before \
             re-recording."
        );
        out.push(("underlying::observe (ln)".to_string(), h.hex()));
    }

    out
}

/// Smallest number of DISTINCT finite values a row may carry before its pin is treated as vacuous.
///
/// ⚠ The failure this guards is not hypothetical: the analytics twin caught a real instance
/// (`overfit::pbo_cscv`) where an all-NaN or one-constant row hashed identically on every platform
/// and read as a pass while pinning nothing.
const MIN_DISTINCT: usize = 64;

/// The measurement harness. `#[ignore]`d: it asserts nothing and prints a table to diff.
///
/// Run it on Windows and on Linux and compare the two outputs column by column. A row that differs
/// is a value this maker computes differently depending on which box ran it.
///
/// ⚠ **It prints [`stateful_rows`] too, and its not doing so was a live trap rather than an
/// omission.** This is the only `#[ignore]`d test in the file, so a recording session driven as
/// `--lib platform_probe -- --ignored` — the spelling the module doc offers — runs THIS and nothing
/// else. While it printed [`rows`] alone it reported the nine rows that are already RECORDED and
/// not one of the two still spelled `"RECORD"`: the exact shape of a measurement command that exits
/// 0 having measured nothing outstanding. Recording is still done by RUNNING
/// [`platform_pin_stateful`], whose panic carries the paste-ready block; this table is the
/// side-by-side view for the human diffing two boxes.
#[test]
#[ignore = "measurement probe — run explicitly on each platform and diff the output"]
fn platform_probe() {
    println!("{:<46}  {:>16}  {:>10}", "function (transcendental)", "fnv1a", "witness");
    for (label, hash) in rows() {
        println!("{label:<46}  {hash:>16}");
    }
    for (label, hash, witness) in stateful_rows() {
        println!("{label:<46}  {hash:>16}  {witness:>10}");
    }
}

/// ⚠ **ONE set of constants for EVERY platform — that is the entire claim**, and in this crate it
/// is a claim that was FALSE until 2026-08-26.
///
/// The measurement that forced the conversion, kept because a table of agreeing numbers looks the
/// same whether it was always true or was made true, and the difference is the whole point:
///
/// ```text
///   BEFORE — production called f64::exp / ln / powf, i.e. the PLATFORM's libm
///   function (transcendental)                     Windows/MSVC dev   Linux/glibc dev
///   alpha::ofi_toxicity (exp)                     a5be703612e3b996   94f05a12c3426df5   DIFFER
///   avellaneda::ewma_alpha (powf)                 c1596b755b3856d9   210b93414ad020b7   DIFFER
///   avellaneda::as_intensity_halfspread (ln)      446cb5cee4a09636   bb521d3e0b5774eb   DIFFER
///   avellaneda::gueant_skew_coeff (powf)          67647621c724e77d   67647621c724e77d   agree
///   fairvalue::lmsr_reservation (ln + exp)        b6253f2c796321bb   e610a5c3ccf505fd   DIFFER
///   spread_source::ls_lmsr_prices (exp x2 + ln)   2d1fd91e43a2fd36   6ad692d604904cc2   DIFFER
///   fits::fit_kappa_censored (exp)                c2bebd042f84f781   7498a33a30f682ef   DIFFER
///   fits::fit_base_intensity (exp)                5120db4e7ca9067a   c0fc1bd3eec71d23   DIFFER
///   underlying::observe (ln)                      61677db0ec21d09b   61677db0ec21d09b   agree
///
///   AFTER — every site routes through the `libm` CRATE
///   every row above collapses to the single value pinned below, on BOTH boxes.
/// ```
///
/// ⚠ **That AFTER line read "all thirteen sites" and the count was wrong** — the conversion it
/// records reached thirteen because the source gate could see thirteen; two more sat below a
/// mid-file test module the scan `break`ed at. Neither is in the table above, so no hash here
/// moves; what changes is the claim, from a complete inventory to what it actually was. The
/// module doc's CORRECTION carries the roster and the measurement.
///
/// ⚠ **The two AGREEING rows were the control, and they are what made the other seven a finding
/// rather than a broken harness.** Every row draws from the same LCG corpus — pure integer
/// arithmetic plus one correctly-rounded divide, bit-identical on both boxes by construction. Had
/// the inputs differed, `gueant_skew_coeff` and `underlying::observe` would have differed too.
/// They did not, so the divergence was in the FUNCTIONS.
///
/// What that meant, stated plainly because it is the reason this crate was converted ahead of the
/// rest of the tree: `as_intensity_halfspread` is the Avellaneda-Stoikov optimal half-spread,
/// `lmsr_reservation` is a reservation price, `ls_lmsr_prices` are the prices themselves. **Two
/// boxes running the same strategy over the same tape posted different quotes.** Not a reporting
/// column — the number that goes on the wire.
///
/// # What the conversion COST, stated because 0032 requires it stated
///
/// All NINE rows moved, including the two that already agreed. The `libm` crate does not match
/// glibc either, so this changed numbers on Linux as well — the platform where this workspace's CI
/// and its deployments run. `docs/decisions/0032-transcendentals-come-from-the-libm-crate-not-the-platform.md`
/// is `accepted` and names that cost as the price of reproducibility rather than denying it. The
/// maker was on PAPER when this landed, which is why the change was taken here rather than staged.
///
/// ⚠ **A per-platform table (`#[cfg(windows)]` / `#[cfg(unix)]`) is NOT a repair and must never be
/// added.** It was the tempting answer while the seven rows disagreed, and it would have made the
/// gate green by asserting the divergence was intended — the one conclusion the measurement
/// refutes. If a row goes red, the question is which platform moved and why.
///
/// Recorded by RUNNING, never by hand: a mismatch panics with a paste-ready replacement block.
const PINNED: [(&str, &str); 9] = [
    ("alpha::ofi_toxicity (exp)", "fb2e25235eff77c2"),
    ("avellaneda::ewma_alpha (powf)", "d0ee1b3beac5871e"),
    ("avellaneda::as_intensity_halfspread (ln)", "d6dc7bf4f7935eb3"),
    ("avellaneda::gueant_skew_coeff (powf)", "e8d98c683bcc2c03"),
    ("fairvalue::lmsr_reservation (ln + exp)", "cd586c1bf5f68de5"),
    ("spread_source::ls_lmsr_prices (exp x2 + ln)", "8f17d385e06fe894"),
    ("fits::fit_kappa_censored (exp)", "eeba560f8d7115df"),
    ("fits::fit_base_intensity (exp)", "30d806096289a9ed"),
    ("underlying::observe (ln)", "154bca2245971d3c"),
];

/// An ORDINARY test, so every `just t vike-mm` and every CI roster run executes it.
///
/// ⚠ It spent one commit `#[ignore]`d, as the acceptance test for the conversion that made it
/// passable. That is worth knowing rather than tidying away: before the conversion this test was a
/// TRUE report of a divergence, and ignoring a true report is only defensible while the fix is on
/// its way. It is not a pattern to reach for — the alternative it rejected, a `#[cfg(windows)]`
/// table, would have made the gate green forever by asserting the bug was intended.
#[test]
fn platform_pin() {
    let actual = rows();
    assert_eq!(actual.len(), PINNED.len(), "row count changed — PINNED must be re-recorded");

    let mut bad = Vec::new();
    for ((label, got), (want_label, want)) in actual.iter().zip(PINNED.iter()) {
        assert_eq!(label, want_label, "row order changed — PINNED must be re-recorded");
        if got != want {
            bad.push(format!("{label:<46} want={want} got={got}"));
        }
    }

    if !bad.is_empty() {
        let paste: String =
            actual.iter().map(|(l, h)| format!("    (\"{l}\", \"{h}\"),\n")).collect();
        panic!(
            "\nvike-mm quote math moved away from the platform pin:\n  {}\n\n\
             There is no tolerance here to widen, and hand-editing a digit defeats the gate.\n\
             A moved hash means a QUOTE this maker would post moved. Establish WHY first:\n\
               (a) an intended numeric change — re-record by RUNNING this test on BOTH Windows and\n\
                   Linux and pinning the hash they AGREE on; or\n\
               (b) a call site changed which libm it reaches, in which case the two platforms will\n\
                   NOT agree and the fix is in the source, not here.\n\n\
             const PINNED: [(&str, &str); {}] = [\n{paste}];\n",
            bad.join("\n  "),
            actual.len(),
        );
    }
}

// =================================================================================================
// The STATEFUL half — the three sites `rows()` cannot reach, added 2026-08-29.
// =================================================================================================

/// The three converted sites that need a driven OBJECT rather than a scalar call, hashed.
///
/// ⚠ **Why a second `rows()` rather than three more rows on the first.** Every one of these is
/// reached through a `pub(crate)` method on a struct that has to be built and fed first, so the
/// generators look nothing like the one-line samplers above; folding them in would have made
/// [`rows`] a function with two personalities. The decisive reason is the TABLE, though — see
/// [`PINNED_STATEFUL`].
///
/// Returns `(label, hash, distinct-or-witness)` rows in a FIXED order, compared positionally.
fn stateful_rows() -> Vec<(String, String, usize)> {
    let mut out = Vec::new();

    // --- xemm::basis::BasisEwma::observe — `libm::pow(0.5, dt / halflife_ms)`.
    //
    // STATEFUL by construction: the weight multiplies the PREVIOUS estimate, so a last bit
    // compounds across the fold rather than washing out. Every post-fold `value()` is hashed.
    //
    // ⚠ `dt` must VARY. `observe` folds at full weight when `dt <= 0` and skips the `pow` entirely,
    // and a fixed-cadence tape evaluates `pow(0.5, c)` at ONE argument for the whole run — either
    // way the row would pin one value of the function under test. The gaps below span sub-half-life
    // to several half-lives, and a deliberate minority are non-positive so the full-weight branch is
    // pinned too (it has no `pow` in it, which is exactly why a future edit putting one there should
    // go red).
    //
    // ⚠ **This row is guarded by a DISTINCT-VALUE floor as well as by `folded`, and the second
    // guard was added because the first cannot do this job.** `folded` counts how many readings
    // were hashed; it says nothing about whether they DIFFER. An estimator that returned one
    // constant — a clamp collapsed onto its bound, a `value()` that stopped folding the previous
    // estimate in — folds all 4,096 readings, passes `folded > 2_000` comfortably, and hashes
    // identically on Windows and on Linux while pinning nothing. That is the same vacuity
    // `MIN_DISTINCT` exists for on every other float row in this file, and this row is a float row;
    // only `avellaneda::in_blackout` below is genuinely discrete and needs a different floor.
    {
        let mut h = Fnv::new();
        let mut vals: Vec<f64> = Vec::new();
        let mut s = 0x3141_5926_5358_9793u64;
        let mut folded = 0usize;
        for _ in 0..64 {
            let halflife_ms = 50 + (lcg(&mut s) * 20_000.0) as i64;
            let clamp = if lcg(&mut s) < 0.5 { 0.0 } else { 0.001 + lcg(&mut s) * 0.05 };
            let mut ewma = crate::xemm::basis::BasisEwma::default();
            let mut ts = 1_700_000_000_000i64;
            let mut mid_b = 100.0f64;
            for _ in 0..64 {
                // A purely arithmetic walk on both mids — no transcendental in the input, for the
                // reason the module doc gives.
                mid_b *= 1.0 + (lcg(&mut s) - 0.5) * 0.004;
                let mid_a = mid_b * (1.0 + (lcg(&mut s) - 0.5) * 0.02);
                // One in eight steps goes BACKWARDS in event time, which is the documented
                // full-weight arm rather than an error.
                let step = if lcg(&mut s) < 0.125 {
                    -(lcg(&mut s) * 500.0) as i64
                } else {
                    1 + (lcg(&mut s) * (halflife_ms as f64) * 4.0) as i64
                };
                ts += step;
                ewma.observe(mid_a, mid_b, ts, halflife_ms, clamp);
                if let Some(v) = ewma.value() {
                    // Collected rather than hashed in place ONLY so the distinct count below can be
                    // taken over the same values in the same order — the hash is folded from this
                    // vector immediately afterwards, so it is byte-identical to hashing here.
                    vals.push(v);
                    folded += 1;
                }
            }
        }
        for &v in &vals {
            h.push(v);
        }
        // A cold estimator returns `None` forever when `halflife_ms <= 0`; if that ever became the
        // whole corpus this row would hash the FNV seed and pin nothing. The `underlying::observe`
        // row above records that exact failure happening in a first draft.
        assert!(
            folded > 2_000,
            "the BasisEwma row folded only {folded} readings — a row that folds nothing hashes the \
             FNV seed and pins nothing. Check `observe`'s early returns before re-recording."
        );
        // ...and the guard `folded` cannot be: 4,096 copies of one number is a full fold and a
        // vacuous pin at the same time. Same threshold and same reason as `MIN_DISTINCT`'s other
        // uses; widen the corpus rather than lowering it.
        let d = distinct(&vals);
        assert!(
            d >= MIN_DISTINCT,
            "the BasisEwma row folded {folded} readings but only {d} DISTINCT finite values — under \
             MIN_DISTINCT ({MIN_DISTINCT}), so this corpus would hash identically on every platform \
             and read as a PASS while pinning nothing. `folded` cannot see this: a constant series \
             folds every reading. Check that `dt`, `halflife_ms` and `clamp` still vary — a clamp \
             collapsed onto its bound is the likeliest cause — and WIDEN the corpus rather than \
             lowering MIN_DISTINCT."
        );
        out.push(("xemm::basis::BasisEwma::observe (pow)".to_string(), h.hex(), d));
    }

    // --- avellaneda::AsState::in_blackout — `effective_blackout_ms` (`libm::exp`) and
    // `moneyness_z` (`libm::log`), the FOURTEENTH and FIFTEENTH sites.
    //
    // ⚠ This row hashes BOOLEANS, and that is the point rather than a compromise. Both private
    // methods feed `(base as f64 * factor).round() as i64`, so their divergence is DISCRETE: a
    // one-ulp disagreement either side of a `.5` boundary moves the blackout window by a whole
    // millisecond and flips `in_blackout` on one box and not the other. Hashing the boolean
    // SEQUENCE across a ts sweep is therefore a sharper instrument than hashing the ms value would
    // be — it is the actual observable the maker's `requote` branches on.
    //
    // ⚠ The generic `MIN_DISTINCT` guard cannot apply to a boolean corpus (its distinct count is 2
    // by construction), so this row carries its own: the number of TRANSITIONS observed. A sweep
    // that never crosses the boundary is all-`false` or all-`true`, hashes identically on any two
    // platforms, and pins nothing — and a transition count is the only evidence that the corpus
    // actually straddles the window edge where the `.round()` can flip.
    //
    // ⚠ Every knob below must be armed or the site is not reached at all:
    //   * `resolution_blackout_ms > 0`, else `in_blackout` returns `false` on line one;
    //   * `HorizonMode::TimeToResolution` WITH `resolution_ts: Some(..)`, else the match falls
    //     through to `_ => false`;
    //   * `atm_blackout_scale > 0.0`, else `effective_blackout_ms` returns the base BEFORE calling
    //     `moneyness_z` — the default is `0.0`, so a `..Default::default()` state would exercise
    //     neither `exp` nor `log`;
    //   * `set_underlying` with `sigma > 0` and `s_open > 0`, else `moneyness_z` returns `None` and
    //     `effective_blackout_ms` takes its own base arm.
    {
        /// Widest `atm_blackout_scale` drawn below. The window can therefore open at most
        /// `base · (1 + ATM_SCALE_MIN + ATM_SCALE_SPAN)` before resolution.
        const ATM_SCALE_MIN: f64 = 0.05;
        const ATM_SCALE_SPAN: f64 = 4.0;
        /// Samples per sweep, and INTERVALS rather than a step size: `lo + (hi - lo)·i/N` hits both
        /// endpoints exactly. A truncating `step` would leave the last sample up to `N - 1` ms
        /// short of `hi`, which for a small `base` can fall back OUTSIDE the window — the sweep
        /// would then be all-`false`, the transition guard below would fire, and the cause would
        /// look like a bug in the function rather than in the sweep.
        const SWEEP: i64 = 256;
        /// How far BEYOND the widest possible window edge the sweep starts, as a multiple of it.
        /// At `1.0` the first sample sits exactly on the edge for a maximum-scale, exactly-ATM
        /// draw, which is a coin flip rather than a guarantee; the margin makes "the sweep starts
        /// outside the window" true for EVERY draw instead of for almost every one.
        const START_MARGIN: f64 = 1.25;

        let mut h = Fnv::new();
        let mut s = 0x2718_2818_2845_9045u64;
        let mut transitions = 0usize;
        let mut sweeps = 0usize;
        let t_res = 1_700_000_000_000i64;
        for _ in 0..96 {
            let base = 1_000 + (lcg(&mut s) * 600_000.0) as i64;
            let mut st = avellaneda::AsState::new(vike_model::AsParams {
                horizon_mode: vike_model::HorizonMode::TimeToResolution,
                resolution_ts: Some(t_res),
                resolution_blackout_ms: base,
                atm_blackout_scale: ATM_SCALE_MIN + lcg(&mut s) * ATM_SCALE_SPAN,
                ..Default::default()
            });
            let s_open = 100.0 + lcg(&mut s) * 50_000.0;
            // Close to the open, so `z` stays small and the ATM widening is actually in play; far
            // out of the money the factor collapses toward 1 and the window stops moving.
            let s_now = s_open * (0.995 + lcg(&mut s) * 0.01);
            st.set_underlying(s_now, s_open, 1e-5 + lcg(&mut s) * 2e-3);

            // Sweep ts ACROSS the widened window's opening edge, which sits somewhere in
            // `[t_res - base·(1 + scale), t_res - base]`. `hi` is one ms INSIDE the narrowest
            // possible window (the factor is never below 1), and `lo` is a margin beyond the widest
            // possible one — so the sweep straddles the edge wherever the `exp` puts it, by
            // construction rather than by luck.
            let widest = (base as f64) * (1.0 + ATM_SCALE_MIN + ATM_SCALE_SPAN);
            let lo = t_res - (widest * START_MARGIN) as i64;
            let hi = t_res - base + 1;
            let mut prev: Option<bool> = None;
            for i in 0..=SWEEP {
                let ts = lo + (hi - lo) * i / SWEEP;
                let b = st.in_blackout(ts);
                h.push(if b { 1.0 } else { 0.0 });
                if prev.is_some_and(|p| p != b) {
                    transitions += 1;
                }
                prev = Some(b);
            }
            sweeps += 1;
        }
        assert!(
            transitions >= sweeps,
            "only {transitions} blackout transitions across {sweeps} sweeps — a sweep that never \
             crosses the window edge is a constant boolean series, which hashes identically on \
             every platform and pins nothing. That edge is the ONLY place the `.round()` in \
             `effective_blackout_ms` can flip. The endpoints are chosen so each sweep MUST cross \
             it once (see SWEEP and START_MARGIN), so a failure here means an early return in \
             `in_blackout` is firing — check the four armed knobs above before re-recording."
        );
        out.push((
            "avellaneda::in_blackout (exp + log, DISCRETE)".to_string(),
            h.hex(),
            transitions,
        ));
    }

    out
}

/// The stateful table. ⚠ **Every hash is the literal `"RECORD"` and both rows are therefore RED
/// until a human runs this file on both boxes and pastes.**
///
/// ⚠ **This is a SECOND table rather than two more [`PINNED`] rows, and the reason is that
/// [`PINNED`]'s nine hashes are RECORDED — measured on Windows/MSVC and on Linux/glibc, agreeing.**
/// Appending unrecorded rows to it would turn a recorded gate red for an unrecorded reason, and a
/// gate that is red for a known-boring reason is a gate people stop reading: the next REAL quote
/// regression would land in a test that was already failing. Keeping the unfinished recording in
/// its own table means [`platform_pin`] still means what it meant yesterday, and
/// [`platform_pin_stateful`] carries exactly the work that is outstanding. Once both rows are
/// recorded, MERGING the tables is a reasonable tidy-up — but only then, and merging must keep the
/// nine existing rows in their existing order, because both compares are positional.
///
/// The re-record rule is [`PINNED`]'s, unchanged: run on BOTH boxes, confirm they AGREE, paste the
/// agreed hash. Never edit a digit, never add a `#[cfg(windows)]` arm.
const PINNED_STATEFUL: [(&str, &str); 2] = [
    ("xemm::basis::BasisEwma::observe (pow)", "7269b33a23946646"),
    ("avellaneda::in_blackout (exp + log, DISCRETE)", "43170bd5c4e0da65"),
];

/// The gate for the three sites [`PINNED`] cannot reach. An ORDINARY test, like its sibling.
///
/// ⚠ A red row here whose `want` reads `RECORD` is an UNFINISHED RECORDING, not a regression —
/// exactly the distinction [`PINNED`]'s doc draws, and the reason the placeholder is a word rather
/// than a plausible-looking hex literal that would be indistinguishable from a measured one.
#[test]
fn platform_pin_stateful() {
    let actual = stateful_rows();
    assert_eq!(
        actual.len(),
        PINNED_STATEFUL.len(),
        "row count changed — PINNED_STATEFUL must be re-recorded"
    );

    let mut bad = Vec::new();
    for ((label, got, witness), (want_label, want)) in actual.iter().zip(PINNED_STATEFUL.iter()) {
        assert_eq!(label, want_label, "row order changed — PINNED_STATEFUL must be re-recorded");
        // The per-row non-vacuity WITNESS (folded readings / observed transitions) is asserted
        // inside `stateful_rows` itself, where the generator that produced it is in scope. It is
        // carried out here as well so a failure message can report it — a hash mismatch on a row
        // whose witness is near its floor is a different problem from one on a healthy row.
        if got != want {
            bad.push(format!("{label:<46} want={want} got={got} (witness={witness})"));
        }
    }

    if !bad.is_empty() {
        let paste: String =
            actual.iter().map(|(l, h, _)| format!("    (\"{l}\", \"{h}\"),\n")).collect();
        panic!(
            "\nvike-mm's stateful quote math moved away from the platform pin:\n  {}\n\n\
             If want= reads RECORD this is an UNFINISHED RECORDING rather than a regression: run\n\
             this file on BOTH Windows and Linux, confirm the two agree bit-for-bit, and paste.\n\
             Otherwise: there is no tolerance here to widen, and hand-editing a digit defeats the\n\
             gate. `in_blackout` is a BOOLEAN sequence, so a moved hash there is not a last bit —\n\
             it is the blackout window opening a whole millisecond earlier or later, which flips a\n\
             safety gate on one box and not the other. Establish WHY first:\n\
               (a) an intended numeric change — re-record by RUNNING on BOTH boxes and pinning the\n\
                   hash they AGREE on; or\n\
               (b) a call site changed which libm it reaches, in which case the two platforms will\n\
                   NOT agree and the fix is in the source, not here.\n\n\
             const PINNED_STATEFUL: [(&str, &str); {}] = [\n{paste}];\n",
            bad.join("\n  "),
            actual.len(),
        );
    }
}

/// No pinned row may be vacuous.
///
/// A row whose corpus collapses to one value — or to all-NaN — hashes identically everywhere and
/// would read as a pass while proving nothing. This drives each row's own generator and counts
/// distinct finite outputs.
///
/// ⚠ It covers TWO of [`PINNED`]'s nine rows and none of [`PINNED_STATEFUL`]'s, and that is a
/// declared limit rather than an oversight. The seven it skips are stateful or sequence-shaped, so
/// re-deriving them here would mean duplicating their drivers; each carries its own guard at its
/// generator instead — `underlying::observe`'s `folded` assertion, [`stateful_rows`]'s BasisEwma
/// row's `MIN_DISTINCT` floor (a real distinct count, taken over the same values in the same order
/// the hash folds, so it is the identical check this test performs, just performed at the
/// generator), and its `in_blackout` row's TRANSITION count, which is the discrete twin: a boolean
/// corpus has a distinct count of 2 by construction, so the floor that means anything there is
/// "both outcomes present, and the sweep actually crossed the edge". The sibling probes written
/// since —
/// `crates/vike-backtest/tests/libm_platform_probe.rs` and its four peers — compute `distinct` as
/// a FIELD on every row instead, which needs no duplication and covers everything; that is the
/// better shape and this file has not been converted to it.
#[test]
fn every_pinned_row_is_non_vacuous() {
    // Re-derive per row rather than trusting `rows()`: the hash alone cannot tell a rich row from
    // a constant one, which is the whole point.
    let mut thin = Vec::new();

    let mut s = 0x1234_5678_9abc_def0u64;
    let vals: Vec<f64> = (0..SAMPLES)
        .map(|_| {
            let ofi = (lcg(&mut s) - 0.5) * 200.0;
            let scale = 0.5 + lcg(&mut s) * 50.0;
            alpha::ofi_toxicity(ofi, scale)
        })
        .collect();
    let d = distinct(&vals);
    if d < MIN_DISTINCT {
        thin.push(format!("alpha::ofi_toxicity: {d} distinct finite values"));
    }

    let mut s = 0xaaaa_bbbb_cccc_ddddu64;
    let vals: Vec<f64> = (0..SAMPLES)
        .map(|_| {
            let mid = 0.001 + lcg(&mut s) * 0.998;
            let net_inventory = (lcg(&mut s) - 0.5) * 2_000.0;
            let b = 1.0 + lcg(&mut s) * 500.0;
            fairvalue::lmsr_reservation(mid, net_inventory, b)
        })
        .collect();
    let d = distinct(&vals);
    if d < MIN_DISTINCT {
        thin.push(format!("fairvalue::lmsr_reservation: {d} distinct finite values"));
    }

    assert!(
        thin.is_empty(),
        "a pinned row's corpus is too thin to detect a last-bit divergence — widen its input \
         ranges rather than lowering MIN_DISTINCT ({MIN_DISTINCT}):\n  {}",
        thin.join("\n  ")
    );

    // Control: a deliberately constant series must FAIL the same check, or the threshold is not
    // actually being applied and every assertion above is decoration.
    let constant = vec![0.5f64; SAMPLES];
    assert!(
        distinct(&constant) < MIN_DISTINCT,
        "the non-vacuity check does not discriminate: a constant series passed it"
    );
}

fn distinct(vals: &[f64]) -> usize {
    let mut bits: Vec<u64> = vals.iter().filter(|v| v.is_finite()).map(|v| v.to_bits()).collect();
    bits.sort_unstable();
    bits.dedup();
    bits.len()
}

/// The `f64` methods whose last bit is the PLATFORM's business rather than IEEE 754's, as bare
/// names. [`banned_needles`] renders each into the two spellings that reach it.
///
/// `sqrt` is deliberately absent and must stay absent: IEEE 754 requires it correctly rounded, so
/// it is already identical on every box and there is nothing to convert it TO. `powi` IS here —
/// measured on MSVC, a `dev` build lowers `llvm.powi` to the CRT's `pow()`, so it is a libcall
/// rather than the free multiply its name suggests, and its cure is a MULTIPLICATION rather than
/// `libm` (`crates/vike-indicators/src/math.rs`'s `sq` / `cube` / `quart` carry that argument).
///
/// ⚠ **WIDENED 2026-08-26 from 22 names to 26**, and the four that joined — `asinh`, `acosh`,
/// `atanh`, `sin_cos` — were not reachable from any of the original 22 by substring, which is the
/// only way this list has ever gained coverage. Verified needle by needle rather than assumed:
/// every pattern [`banned_needles`] builds is anchored on a `(` immediately after the name, so
/// `.asin(` cannot match `.asinh(` (an `h` sits where the paren must be) and `.acos(`/`.atan(`
/// cannot match `.acosh(`/`.atanh(` for the same reason; and every pattern is anchored on a `.` or
/// a `::` immediately before the name, so `.sinh(`/`.cosh(`/`.tanh(` cannot match `.asinh(`/
/// `.acosh(`/`.atanh(` (the character before `sinh` there is an `a`, not a dot). `.sin_cos(` is
/// missed by `.sin(` for the paren reason and by `.cos(` for the dot reason at once. None of the
/// four appears anywhere under this crate's `src/` today, so the widening changed no verdict — it
/// removed a gap the next edit could have walked into, which is the only moment a list like this
/// can be widened for free.
///
/// ⚠ `.to_degrees(` / `.to_radians(` are deliberately ABSENT and should not be added: std lowers
/// each to a single multiply by a constant, so they are ordinary arithmetic rather than a libcall,
/// platform-invariant for exactly the reason `sqrt` is, and there is no `libm` equivalent to
/// convert them to.
const BANNED_FNS: [&str; 26] = [
    "powf", "ln", "log", "log2", "log10", "exp", "exp2", "exp_m1", "ln_1p", "sin", "cos", "tan",
    "powi", "atan", "atan2", "asin", "acos", "sinh", "cosh", "tanh", "cbrt", "hypot", "asinh",
    "acosh", "atanh", "sin_cos",
];

/// Every [`BANNED_FNS`] name in BOTH spellings that reach the platform's libm.
///
/// ⚠ **The UFCS half is new as of 2026-08-26, and its absence was a real hole rather than a
/// tidiness point.** This gate banned `.exp(` and nothing else, so `f64::exp(x)` — the same
/// inherent method, called through its fully-qualified path, compiling to the same libcall —
/// matched NOTHING and would have walked straight past a gate whose whole subject it is. Neither
/// spelling is more correct Rust than the other and rustfmt does not rewrite one into the other,
/// so which one an author reaches for is a coin flip.
///
/// The `f64::NAME(` needle also covers `std::primitive::f64::NAME(` and
/// `core::primitive::f64::NAME(` for free, because both END with exactly that pattern.
///
/// ⚠ **What it still cannot see, stated rather than implied:** `<f64>::exp(x)` (the angle-bracket
/// qualified spelling — `f64>::exp(` is what appears, and no needle here ends in `>`), a call
/// reached through a generic bound (`T: Float`, `num_traits::Float::exp`), and a call split across
/// two lines by a `max_width` wrap. Not one of the three occurs under this crate's `src/` today,
/// and the first would be the cheapest to add the day it does — but a list of spellings is an
/// enumeration, and an enumeration is never a proof.
fn banned_needles() -> Vec<String> {
    BANNED_FNS.iter().flat_map(|f| [format!(".{f}("), format!("f64::{f}(")]).collect()
}

/// Every `#[cfg(test)]` item's line range in one file, as `(start, end, closed)` — `start`
/// inclusive, `end` EXCLUSIVE, both zero-based; `closed` says whether the item's end was actually
/// located rather than assumed.
///
/// ⚠ **This replaced a `break` at the first `#[cfg(test)]` line on 2026-08-26, and the `break` was
/// not a simplification — it was a hole with a measured size.** A `break` treats the first marker
/// as the end of the shipped half of the file, which is correct only for a file whose test module
/// is last. It is wrong for a file with a test module in the MIDDLE, and this crate has five of
/// them: measured over `crates/vike-mm/src`, the range walk scans 1,612 production lines the
/// `break` discarded — `lib.rs` +972 (its `#[cfg(test)] mod platform_probe;` sits on line 129 of
/// 1104, so 88% of the file was outside the gate), `avellaneda.rs` +633 (six test modules, the
/// first at line 394 of 1921), `own_book.rs` +5, `fits.rs` +1, `strategy_impl.rs` +1. Two of the
/// crate's fifteen `libm` sites live in that discarded region; see the module doc's CORRECTION.
///
/// # How an item's end is found, and why by INDENTATION rather than by counting braces
///
/// A brace count has to know which braces are code, which puts a Rust string/char/comment lexer in
/// the middle of a gate — and this workspace's test modules are full of `format!("{}")` and of
/// assert messages continued across lines with a trailing `\`, so a naive count is not merely
/// imprecise, it is wrong on the very files that matter. Indentation needs no lexer and is exact
/// here for a structural reason: `cargo fmt --check` is CI's FIRST gate, so every file in the tree
/// is rustfmt output, and rustfmt closes a block with a `}` alone on a line at the block's OWN
/// indentation. Measured 2026-08-26: all 52 `#[cfg(test)]` markers under the three gated `src/`
/// trees sit at column 0 — not one is indented — and every one of them is followed either by a
/// line ending in `{` (a `mod`/`impl` item) or by a `;` (`lib.rs`'s two `mod NAME;` declarations,
/// so the `;` arm is exercised on the real tree rather than written for a hypothetical).
///
/// # The two ways it can be wrong, and why only one of them is quiet
///
/// * A line INSIDE the item that is exactly the closing pattern — a `}` at the item's indentation
///   sitting inside a multi-line string literal, or inside a `#[rustfmt::skip]` block — ends the
///   range early. The scan then resumes over test code, and test code is full of banned spellings,
///   so the failure is a LOUD false positive naming the line. There is exactly one raw string
///   under this crate's `src/` (`src/tests/xemm_mount.rs`) and that whole directory is excluded.
/// * No closing pattern is found at all, so the range runs to EOF and the rest of the file goes
///   unscanned — which is the `break`'s behaviour, i.e. SILENT. That is why `closed` is returned
///   rather than swallowed: the caller asserts on it, so this failure is loud too.
fn cfg_test_ranges(lines: &[&str]) -> Vec<(usize, usize, bool)> {
    let mut items = Vec::new();
    let mut i = 0usize;
    while i < lines.len() {
        let trimmed = lines[i].trim_start();
        // Prose FIRST, exactly as in the scan loop: a doc comment spelling the attribute inside
        // backticks is a house-style header in this crate (`src/xemm/pricing.rs` and three
        // siblings open with one), and reading it as a marker would blank the file.
        if trimmed.starts_with("//") || !trimmed.starts_with("#[cfg(test)]") {
            i += 1;
            continue;
        }
        let mut close = " ".repeat(lines[i].len() - trimmed.len());
        close.push('}');
        let mut opened = false;
        let mut end = lines.len();
        let mut closed = false;
        for (j, line) in lines.iter().enumerate().skip(i) {
            let tail = line.trim_end();
            if !opened {
                // The item's header may span lines (attributes, a `mod` on the next line). It
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
            if tail == close {
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

/// Production code must reach a transcendental through the `libm` CRATE, never through `f64`'s
/// platform-libm methods.
///
/// ⚠ [`platform_pin`] proves today's pinned nine functions agree across platforms; this stops the
/// SIXTEENTH site from being written as `x.exp()`. The two are not redundant: the pin's corpus
/// reaches nine functions, and a new call site in a tenth would be invisible to it while quoting a
/// platform-dependent price. That gap is exactly how the seven diverging rows survived — this
/// crate had neither gate.
///
/// The banned set is [`BANNED_FNS`] rendered through [`banned_needles`], so the method spelling
/// and the UFCS spelling of the same call can never fall out of step; both constants carry the
/// argument for what is on the list and what is deliberately off it.
///
/// Scope: every `.rs` under `src/`, recursively, MINUS each file's `#[cfg(test)]` item RANGES —
/// and `src/tests/` is excluded WHOLESALE, because `lib.rs` declares it `#[cfg(test)] mod tests;`,
/// so the entire directory is test-only and its files carry no marker of their own. Without that
/// exclusion this gate reports `tests/as_pricing.rs`'s exponential-sample fixture, which is
/// generated test data rather than a quote.
///
/// ⚠ **RANGES, not a `break` — see [`cfg_test_ranges`] for the measurement.** The scan used to
/// stop at the first marker, which is right only for a file whose test module is last, and five
/// files in this crate put one in the middle. That `break` is why the module doc said THIRTEEN
/// sites when there are fifteen: the two it could not see were the two nobody counted.
#[test]
fn production_code_calls_libm_not_the_platform() {
    let banned = banned_needles();
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let src = root.join("src");
    let mut found = Vec::new();
    let mut scanned: Vec<String> = Vec::new();
    // Files where at least one PRODUCTION line below the first `#[cfg(test)]` marker was scanned —
    // the property the `break` made impossible, asserted below so the fix cannot silently revert.
    let mut scanned_past_a_test_module: Vec<String> = Vec::new();
    // A `#[cfg(test)]` item whose end could not be located swallows the rest of its file exactly
    // the way the old `break` did. That is the one failure of the range walk that would be QUIET,
    // so it is collected and asserted rather than absorbed.
    let mut unclosed: Vec<String> = Vec::new();
    let mut stack = vec![src];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).expect("crate has a src/ directory") {
            let path = entry.expect("readable dir entry").path();
            if path.is_dir() {
                // The `#[cfg(test)] mod tests;` tree — test-only in its entirety.
                if path.file_name().is_some_and(|n| n == "tests") {
                    continue;
                }
                stack.push(path);
                continue;
            }
            if path.extension().and_then(|e| e.to_str()) != Some("rs") {
                continue;
            }
            let name = path.file_name().unwrap().to_string_lossy().to_string();
            // ⚠ THIS FILE EXCLUDES ITSELF, and the reason is structural rather than convenient.
            // The sibling gates in `vike-indicators` and `vike-analytics` live under `tests/`, so
            // their scan of `src/` can never reach them. This one lives INSIDE `src/` — it has to,
            // because every function it measures is `pub(crate)` in a private module — so it scans
            // its own needle list and reports every one of them as a call site. That is
            // `SELF_EXCLUDE` in miniature, the same shape
            // `crates/vike-ops/tests/unrun_command_gate.rs` records as its incident 2.
            //
            // ⚠ The exemption is keyed on the CRATE-RELATIVE PATH, not on the bare file name, and
            // that changed on 2026-08-26. A `name == "platform_probe.rs"` test exempts ANY file
            // anywhere under this crate's `src/` that happens to carry the name —
            // `src/xemm/platform_probe.rs`, `src/venue/platform_probe.rs`. Neither exists today,
            // and either would have been silently un-gated the day it was created, under a name an
            // author would reach for precisely BECAUSE this file exists. `src/` already nests
            // (`xemm/`) and the crate's own scaffolding pushes toward per-area probes, so this is
            // the ordinary next file rather than an exotic one. An exclusion is meant to name ONE
            // file; now it does. The `\` -> `/` normalisation is what lets the needle be written
            // one way and still match on the Windows dev box.
            //
            // The exclusion is safe TODAY because this file calls no transcendental of its own: it
            // drives the crate's functions and a pure-integer LCG. `the_self_exclusion_is_narrow`
            // below re-measures that rather than trusting this comment, so the day somebody writes
            // a platform call in here the exemption stops being free.
            let rel = path.strip_prefix(root).unwrap_or(&path).to_string_lossy().replace('\\', "/");
            if rel == "src/platform_probe.rs" {
                continue;
            }
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
                    continue; // inside a `#[cfg(test)]` item — not shipped
                }
                let code = line.trim_start();
                if code.starts_with("//") {
                    continue; // a doc comment naming `exp` is prose, not a call
                }
                let past_marker = first_marker.is_some_and(|m| i > m);
                if past_marker && !scanned_past_a_test_module.contains(&name) {
                    scanned_past_a_test_module.push(name.clone());
                }
                for pat in &banned {
                    if line.contains(pat.as_str()) {
                        found.push(format!("  {name}:{} {}", i + 1, line.trim()));
                    }
                }
            }
        }
    }
    // Non-vacuity, aimed at the failure this scan is most likely to have: `src/` has nested
    // module directories, so a walk that stopped at the top level would still scan a dozen files
    // and report a confident zero while never opening the ones that carry the quote math.
    for must in ["alpha.rs", "avellaneda.rs", "fairvalue.rs", "spread_source.rs", "underlying.rs"] {
        assert!(
            scanned.iter().any(|s| s == must),
            "the scan never opened {must}, so an empty result proves nothing about the files this \
             gate exists for. Scanned: {scanned:?}"
        );
    }
    assert!(
        unclosed.is_empty(),
        "a `#[cfg(test)]` item's closing line could not be located, so everything below it went \
         UNSCANNED — the exact hole the old `break` left, wearing a different cause. See \
         `cfg_test_ranges`: the end is the item's own indentation followed by a lone `}}`, or a \
         `;` for a `mod NAME;` declaration. An empty `#[cfg(test)] mod x {{}}` on one line, or a \
         hand-formatted item rustfmt did not touch, will land here.\n{}",
        unclosed.join("\n")
    );
    // ⚠ **The non-vacuity assertion for the RANGE walk, and the one the `break` could not pass.**
    // The two files named carry production code BELOW a test module — `avellaneda.rs` has six test
    // modules with the crate's fourteenth and fifteenth `libm` sites among them, and `lib.rs`
    // declares `#[cfg(test)] mod platform_probe;` on line 129 of 1104. Under the `break` this list
    // was EMPTY by construction and nothing said so. If it goes red, the walk has reverted to
    // treating the first marker as the end of the shipped file.
    for must in ["avellaneda.rs", "lib.rs"] {
        assert!(
            scanned_past_a_test_module.iter().any(|s| s == must),
            "no production line below {must}'s first `#[cfg(test)]` marker was scanned, so this \
             gate has gone blind to the region where two of this crate's fifteen `libm` sites \
             live. Scanned past a test module: {scanned_past_a_test_module:?}"
        );
    }
    assert!(
        found.is_empty(),
        "production code must call the `libm` CRATE rather than `f64`'s platform-libm methods — \
         this crate was MEASURED quoting differently on Windows and Linux before its fifteen sites \
         were converted, and `PINNED`'s doc carries that table. BOTH spellings are banned, the \
         method one and the fully-qualified one, because they are the same libcall — see \
         `banned_needles`. A `powi` is banned too: on MSVC a `dev` build makes it a `pow()` \
         libcall, and its cure is a multiply rather than `libm`:\n{}",
        found.join("\n")
    );
}

/// The RANGE walk, proved on a synthetic file rather than on the tree it happens to run against.
///
/// ⚠ **A tree-shaped assertion cannot cover this on its own**, which is why both exist. The two
/// `scanned_past_a_test_module` rows above are real evidence, but they are evidence about
/// `avellaneda.rs` and `lib.rs` as they are TODAY — reorganise either file so its test module goes
/// last and the assertion passes vacuously while the mechanism is unproven. This fixture holds the
/// mechanism itself: production before a test module, a banned call INSIDE it, production after
/// it, and a banned call in each of the two production halves.
///
/// It is also the control arm for the `;` header form, which no brace-matching walk would handle
/// by accident and which `lib.rs` uses twice.
#[test]
fn the_test_module_cut_excludes_only_the_test_module() {
    // ⚠ Each fixture line is a STRING, and several of them carry a banned needle — which is
    // exactly the spelling that defeats a "the line does not start with a quote" filter, since the
    // `let` line below starts with `l`. `the_self_exclusion_is_narrow` reads this file through a
    // literal-stripper for that reason; see its doc.
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
        "#[cfg(test)]",
        "mod tests;",
        "",
        "fn shipped_last() -> f64 {",
        "    w.sin_cos().0",
        "}",
    ];
    let items = cfg_test_ranges(&file);
    assert_eq!(items.len(), 2, "both `#[cfg(test)]` items must be found: {items:?}");
    assert_eq!(items[0], (5, 11, true), "the braced module spans its own lines only");
    assert_eq!(items[1], (16, 18, true), "the `mod tests;` declaration ends at its semicolon");

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
    // ⚠ The message below names the fixture's inner call by LINE, never by spelling it: this
    // file is scanned needle-by-needle by `the_self_exclusion_is_narrow`, and a continuation line
    // of a multi-line string literal is code as far as any line-local stripper can tell.
    assert_eq!(
        hits,
        [2, 13, 20],
        "the scan must see the production call BEFORE the test module (line 3), the one AFTER it \
         (line 14), and the one after the `mod tests;` declaration (line 21) — and must NOT see \
         the fixture logarithm on line 9. A `break` at the first marker sees only line 3. \
         Saw (zero-based): {hits:?}"
    );

    // The control arm: the module doc line NAMES the attribute in backticks, forty lines above any
    // real marker in this crate's `xemm/` files. If prose could open a range, `items[0]` would
    // start at 0 and every assertion above would still pass while the gate scanned nothing.
    assert_eq!(items[0].0, 5, "a `#[cfg(test)]` inside a comment must not open a range");
}

/// The code half of one source line: string literals emptied, char literals dropped whole, and a
/// `//` comment cut from where it starts.
///
/// ⚠ **This is the string-stripper [`the_self_exclusion_is_narrow`]'s doc used to say the file
/// would need "if that approximation ever needs to be smarter". It needed to be smarter.** The
/// approximation was "the line does not begin with a quote after trimming", and it fails in BOTH
/// directions:
///
/// * **False positive** — any needle row rustfmt puts on a line that starts with something else.
///   That is what the now-deleted `#[rustfmt::skip]` on the old `NEEDLES` array was fighting:
///   collapse the array and every needle lands after `const NEEDLES: … = [`, which does not start
///   with a quote, so the test reported itself. Its comment called the attribute "load-bearing,
///   not a style preference", and it was — for the wrong rule. With literals stripped there is
///   nothing to lay out defensively, so both the attribute and the array are gone: the needles are
///   built at runtime from a name list, the same shape [`banned_needles`] uses.
/// * **False negative, which is the direction that matters** — a REAL call on a line that begins
///   with a quote is skipped in silence. A `format!` whose arguments rustfmt wraps onto their own
///   line is the whole exploit: that line opens with the format string's `"`, and the one thing
///   this test exists to catch walks straight past it. The mirror-image spelling is now live in
///   this very file — the fixture in [`the_test_module_cut_excludes_only_the_test_module`] opens
///   with `let file = [` and carries a banned call inside a string two lines down — so the crude
///   filter is no longer merely fragile, it is being leaned on from both sides at once.
///
/// The rule is now the honest one: search the line's CODE, with literals removed. A needle row
/// strips to nothing; a real call survives stripping.
///
/// ⚠ **What it does not handle, stated because a stripper that overstates itself is worse than a
/// crude filter that admits what it is:** raw strings (`r"…"`, `r#"…"#`), block comments
/// (`/* … */`), and a string literal continued across lines. Measured 2026-08-26 over the three
/// gated `src/` trees: exactly one raw string exists (`crates/vike-mm/src/tests/xemm_mount.rs`,
/// inside the directory this gate excludes wholesale) and there is not one block comment. The
/// multi-line case is why this is applied ONLY to this file, whose lines it was measured against,
/// rather than being pushed into the tree-wide scan above.
fn code_without_literals(line: &str) -> String {
    let chars: Vec<char> = line.chars().collect();
    let mut out = String::with_capacity(line.len());
    let mut i = 0usize;
    while i < chars.len() {
        match chars[i] {
            // A `//` outside a literal ends the code half of the line.
            '/' if chars.get(i + 1) == Some(&'/') => break,
            // A string literal contributes nothing; `\"` does not close it.
            '"' => {
                i += 1;
                while i < chars.len() {
                    match chars[i] {
                        '\\' => i += 2,
                        '"' => {
                            i += 1;
                            break;
                        }
                        _ => i += 1,
                    }
                }
            }
            // `'x'` / `'\n'` is a char literal and is skipped WHOLE, so a quote inside one cannot
            // open a phantom string. Anything else starting with `'` is a lifetime, i.e. code.
            '\'' => {
                let width = if chars.get(i + 1) == Some(&'\\') { 4 } else { 3 };
                if chars.get(i + width - 1) == Some(&'\'') {
                    i += width;
                } else {
                    out.push(chars[i]);
                    i += 1;
                }
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    out
}

/// The one file [`production_code_calls_libm_not_the_platform`] exempts must stay worth exempting.
///
/// ⚠ An exclusion added to silence a false positive is how a gate quietly stops covering a real
/// one. This file is skipped because it spells out the gate's own needles — [`BANNED_FNS`] and
/// everything [`banned_needles`] renders from it — so the exemption is only honest while the file
/// contains no actual transcendental call of its own.
///
/// The check has to distinguish a needle from a call, which a plain `contains` cannot: both spell
/// `.exp(`. [`code_without_literals`] is how, and its doc carries the crude filter this replaced
/// and the two ways that filter was wrong.
///
/// The needle list here is a SUBSET of [`BANNED_FNS`] on purpose — it is a tripwire on one file,
/// not the tree-wide ban — but it tracks the widenings, so the 2026-08-26 four (`asinh`, `acosh`,
/// `atanh`, `sin_cos`) and both UFCS spellings are here.
#[test]
fn the_self_exclusion_is_narrow() {
    // ⚠ Both spellings of each, for the reason `banned_needles` gives: `f64::exp(x)` is the same
    // libcall as `x.exp()`, and a list that names only one of them is a list with a hole in it.
    let needles: Vec<String> = ["exp", "ln", "powf", "powi", "log", "sin", "cos", "atan", "asinh"]
        .iter()
        .flat_map(|f| [format!(".{f}("), format!("f64::{f}(")])
        .collect();

    // The control arm, run BEFORE the scan: a stripper that returns the line unchanged, or that
    // eats everything, would make the scan below pass while checking nothing. Both directions are
    // asserted, because only the first is the failure anyone would predict.
    let planted = "    let y = x.exp(); // and a comment naming .ln(";
    let stripped = code_without_literals(planted);
    assert!(stripped.contains(".exp("), "the stripper ate a real call: {stripped:?}");
    assert!(!stripped.contains(".ln("), "the stripper kept a trailing comment: {stripped:?}");
    let literal_row = "        \"    let y = x.exp();\",";
    assert!(
        !code_without_literals(literal_row).contains(".exp("),
        "the stripper kept a needle that was inside a string literal — this test would then \
         report the fixture in `the_test_module_cut_excludes_only_the_test_module` as a call"
    );

    let me = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/platform_probe.rs");
    let text = std::fs::read_to_string(&me).expect("this file is readable");
    let mut suspicious = Vec::new();
    for (i, line) in text.lines().enumerate() {
        let code = code_without_literals(line);
        for pat in &needles {
            if code.contains(pat.as_str()) {
                suspicious.push(format!("  platform_probe.rs:{} {}", i + 1, line.trim()));
            }
        }
    }
    assert!(
        suspicious.is_empty(),
        "`production_code_calls_libm_not_the_platform` skips this file because it spells out the \
         gate's own needles rather than calling them. That exemption is no longer free — this \
         file now appears to call a platform transcendental itself, on a line whose CODE (not \
         whose string literals) carries the call:\n{}\n\nEither route it through `libm`, or drop \
         the whole-file skip and give the tree-wide scan the same stripper.",
        suspicious.join("\n")
    );
}
