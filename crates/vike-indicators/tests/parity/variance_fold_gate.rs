//! The source-text gate: every squared-deviation fold under `src/` is declared and classified.

// =============================================================================================
// The constant-window law: one spelling, and a declared disposition for every site of the fold
// =============================================================================================

/// The squared-deviation SHAPE, and the two spellings it wears in this crate: a call to
/// `crates/vike-indicators/src/math.rs`'s `sq`, or an explicit self-multiplication `X * X` whose
/// two operands are the SAME source text. [`is_variance_fold_line`] is the one place that decides;
/// this constant only names the shape for the failure messages below.
///
/// ⚠ **WIDENED 2026-08-25, because the previous marker was self-defeating.** It read
/// `const VARIANCE_FOLD_MARKER: &str = "| sq(";` — the CLOSURE form alone — and before that
/// `".powi(2)).sum::<f64>()"`, and its doc claimed "every rolling variance in this crate is
/// spelled exactly this way, which is what makes the set of sites enumerable by reading the
/// source". That claim was false. Both spellings were derived by reading the folds that existed on
/// the day they were written, `"| sq("` matched exactly the eight sites [`VARIANCE_FOLD_SITES`]
/// then declared, and the table therefore passed while SEVEN live squared-deviation folds sat
/// outside it — `pairs.rs`'s `batch_spread_zscore`, `batch_correl`, `batch_correl_log` and
/// `batch_half_life`, and `statistics.rs`'s `std_error_series`, `batch_skew` and `batch_kurtosis`.
/// Every one of them carries the `== 0.0` / `!= 0.0` output guard the `Residual` rows warn about,
/// which is to say the gate was blind to precisely the population it was built to enumerate.
///
/// ⚠ **And it was getting BLINDER, not stabler.** The sibling source gate
/// `crates/vike-indicators/tests/libm_platform_probe.rs`'s
/// `production_code_calls_libm_not_the_platform` now bans `powi` outright, so the spelling an
/// author reaches for when writing a new squared deviation by hand is exactly `(x - m) * (x - m)`
/// — the one shape the closure marker could not see. A gate that goes blind in the direction its
/// sibling gate pushes authors is worse than no gate, because it certifies.
///
/// ⚠ **A THIRD spelling joined the shape on 2026-08-26: `powi(`, at every arity and in every
/// qualification.** [`is_variance_fold_line`] carries the argument. In one line: the enumeration
/// was resting on the sibling gate for the `powi` case, that gate had a hole (it banned the method
/// spelling only, so `f64::powi(d, 2)` walked past both), and an enumeration that depends on
/// another file's configuration is not an enumeration. It adds no row today.
const VARIANCE_FOLD_SHAPE: &str = "a `sq(` call, a `powi(` call, or a self-multiplied `X * X`";

/// Is this line a `fn` DECLARATION rather than a body line?
///
/// Declaration lines are excluded from the scan for two independent reasons, and both are load
/// bearing. `crates/vike-indicators/src/math.rs`'s `pub(crate) fn sq(x: f64) -> f64` contains the
/// literal `sq(` and would otherwise match the first half of the shape; and [`enclosing_fn`]
/// searches STRICTLY ABOVE the hit line, so a hit landing on a declaration would resolve to the
/// PREVIOUS function and file its row under the wrong name.
///
/// The prefix list is the same one [`enclosing_fn`] recognises, deliberately: a visibility this
/// list misses is a function this gate cannot name either way, so the two must agree.
fn is_fn_decl_line(trimmed: &str) -> bool {
    ["fn ", "pub fn ", "pub(crate) fn ", "pub(super) fn "].iter().any(|p| trimmed.starts_with(p))
}

/// Does this line multiply some expression by ITSELF — `d * d`, `sq(x - m) * sq(x - m)`,
/// `(lag[j] - mean_l) * (lag[j] - mean_l)`?
///
/// The rule is deliberately TEXTUAL and deliberately narrow: both operands must be the same source
/// text, character for character. `a * b` with different operands is ordinary arithmetic and must
/// not redden this gate — `crates/vike-indicators/src/pairs.rs`'s `batch_half_life` folds its
/// covariance as `(lag[j] - mean_l) * (dif[j] - mean_d)` on the line directly above the variance
/// this gate wants, and a matcher that could not tell those two apart would have made the whole
/// table noise.
///
/// An operand is scanned outward from the `*`: a run of identifier characters (`[A-Za-z0-9_.[]]`),
/// or a BALANCED parenthesised group optionally preceded by a callee name. The parenthesis walk is
/// what buys the widening its most important hit — `batch_half_life` spells its `var_l` as
/// `(lag[j] - mean_l) * (lag[j] - mean_l)`, which an identifier-only scan stops at the closing
/// paren and misses entirely.
///
/// ⚠ **What it CANNOT see, stated rather than implied:**
/// - **A fold wrapped across two lines.** The scan is line-by-line, so `(x - mean)` on one line and
///   `* (x - mean)` on the next is invisible. Measured on the tree this widening was written
///   against: no production line under `src/` ends in a bare `*` or begins with a binary `*`, so
///   nothing is hidden by it today — but a `max_width` change or a longer identifier could wrap one
///   tomorrow, and nothing here would notice.
/// - **The same VALUE under two names.** `d * d_copy`, `x * y` where `y = x`, or `a[i] * a[j]` with
///   `i == j` at runtime are all squared deviations this returns `false` for. Textual equality is
///   the whole rule.
/// - **A square reached through anything but infix `*`, `sq(` or `powi(`.** `x.mul(x)`,
///   `f64::mul(x, x)`, `iter.product()` over a doubled iterator, a `dot(v, v)` helper.
/// - ⚠ **A square through a general POWER — `libm::pow(d, 2.0)`.** This is the blind spot worth
///   knowing about, because it is the only one that is simultaneously invisible HERE and PERMITTED
///   by the sibling source gate: `libm::pow` is the sanctioned spelling of a power in this
///   workspace, so `crates/vike-indicators/tests/libm_platform_probe.rs`'s
///   `production_code_calls_libm_not_the_platform` will never object to it, and it contains
///   neither `sq(` nor `powi(` nor a repeated operand. Every OTHER power spelling of a square —
///   `.powf(2.0)`, `f64::powf(d, 2.0)` — is banned outright by that gate, so it cannot come back
///   silently. Measured 2026-08-26: `crates/vike-indicators/src` contains no `libm::pow` call at
///   all, so this is a gap rather than an omission from the table.
/// - **Higher moments.** `crates/vike-indicators/src/math.rs`'s `cube` and `quart` CALL sites are
///   not matched; only those two functions' own bodies are, through their `x * x`. Today every
///   third- and fourth-moment fold in the crate sits inside a function that ALSO folds an `m2`
///   (`crates/vike-indicators/src/indicators/statistics.rs`'s `batch_skew` and `batch_kurtosis`),
///   so each already has a row — but a future kernel folding only a cube would be invisible here.
///   ⚠ `powi(` is the ONE exception now that [`is_variance_fold_line`] carries it: a `powi(3)` or
///   `powi(4)` matches at every arity and demands a row, exactly as the sibling gate bans `powi`
///   at every arity. That asymmetry against `cube(`/`quart(` is deliberate — those two are the
///   crate's own reviewed helpers, and `powi` is the spelling that must not reappear.
///
/// ⚠ **CORRECTED 2026-08-26 — the `.powi(2)` bullet above used to read "`.powi(2)` is the one such
/// spelling that CANNOT come back silently, and only because a different gate bans it".** Both
/// halves have moved. The claim named only the METHOD spelling, and until the same day nothing
/// banned the fully-qualified `f64::powi(x, 2)` at all — so the one spelling this list called safe
/// had a twin that was neither banned there nor visible here. And the dependency the sentence
/// admitted to ("only because a different gate bans it") is now gone in the direction that matters:
/// [`is_variance_fold_line`] matches `powi(` itself, so this table's enumeration no longer rests on
/// a gate in another file staying configured a particular way.
fn self_multiplies(line: &str) -> bool {
    let c: Vec<char> = line.chars().collect();
    if c.len() < 3 {
        return false;
    }
    let is_operand = |ch: char| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '.' | '[' | ']');
    for i in 1..c.len() - 1 {
        if c[i] != '*' {
            continue;
        }
        // `*/` and `/*` are comment delimiters and `**` is not Rust at all; a leading `*` deref is
        // excluded by starting the sweep at 1. None of the four is a product.
        if c[i - 1] == '*' || c[i + 1] == '*' || c[i - 1] == '/' || c[i + 1] == '/' {
            continue;
        }

        // ---- left operand, scanned right-to-left from the `*`.
        let mut j = i;
        while j > 0 && c[j - 1] == ' ' {
            j -= 1;
        }
        let end = j;
        if j > 0 && c[j - 1] == ')' {
            let mut depth = 0usize;
            while j > 0 {
                match c[j - 1] {
                    ')' => depth += 1,
                    '(' => {
                        depth -= 1;
                        if depth == 0 {
                            j -= 1;
                            break;
                        }
                    }
                    _ => {}
                }
                j -= 1;
            }
            // ...and the callee name in front of it, so `sq(x - m)` is one operand, not two.
            while j > 0 && is_operand(c[j - 1]) {
                j -= 1;
            }
        } else {
            while j > 0 && is_operand(c[j - 1]) {
                j -= 1;
            }
        }
        let left: String = c[j..end].iter().collect();
        if left.is_empty() {
            continue;
        }

        // ---- right operand, scanned left-to-right from the `*`.
        let mut k = i + 1;
        while k < c.len() && c[k] == ' ' {
            k += 1;
        }
        let start = k;
        while k < c.len() && is_operand(c[k]) {
            k += 1;
        }
        if k < c.len() && c[k] == '(' {
            let mut depth = 0usize;
            while k < c.len() {
                match c[k] {
                    '(' => depth += 1,
                    ')' => {
                        depth -= 1;
                        if depth == 0 {
                            k += 1;
                            break;
                        }
                    }
                    _ => {}
                }
                k += 1;
            }
        }
        let right: String = c[start..k].iter().collect();
        if right.is_empty() {
            continue;
        }

        if left == right {
            return true;
        }
    }
    false
}

/// The whole predicate: a production line carrying a squared deviation in any of its spellings.
///
/// The `sq(` half is a bare `contains`, on purpose. It matches the closure form the old marker
/// keyed on, the statement form `residuals_sq += sq(...)` it missed, and any path-qualified
/// spelling (`math::sq(`, `crate::math::sq(`) — which is why [`self_multiplies`]'s operand
/// alphabet does not need to carry `:`. `.sqrt(` does not contain `sq(`, so the obvious
/// false-positive does not arise.
///
/// ⚠ **The `powi(` half is new as of 2026-08-26, and it is a bare `contains` for the SAME reason
/// the `sq(` half is** — it has to see `x.powi(2)`, `f64::powi(x, 2)` and any path-qualified
/// variant with one needle, and anchoring it on a leading `.` would see only the first of those.
/// That matters because the fully-qualified spelling is exactly the one that was, until the same
/// day, banned by nothing: the sibling probe's `BANNED_FNS` covered `.powi(` alone, so
/// `f64::powi(d, 2)` passed that gate AND was invisible here. (That list was this crate's own
/// copy when the hole was found; since #2031 it is shared, at
/// `crates/vike-model/src/libm_walk.rs`'s `BANNED_FNS`.)
/// Both holes are closed, and closing them in two places rather than one is deliberate — this
/// table's job is to ENUMERATE every squared-deviation fold, and an enumeration that is only
/// complete while a gate in another crate's test directory stays configured a particular way is
/// not an enumeration.
///
/// It matches at EVERY arity, mirroring the sibling gate's decision to ban `powi` at every arity;
/// [`self_multiplies`]'s blind-spot list states what that does to the higher-moment case.
/// Measured 2026-08-26: every `powi(` under `crates/vike-indicators/src` sits in a `//` comment
/// (`math.rs`'s three "never `x.powi(k)`" doc lines and `statistics.rs`'s module header), and the
/// scan drops comment lines before consulting this predicate — so the widening adds no row to
/// [`VARIANCE_FOLD_SITES`] and changes no verdict. It removes a gap, as the `sq(`-only marker's
/// own history says a matcher in this file eventually needs.
fn is_variance_fold_line(line: &str) -> bool {
    line.contains("sq(") || line.contains("powi(") || self_multiplies(line)
}

/// The index of the first line that begins a file's `#[cfg(test)]` section, or `lines.len()`.
///
/// ⚠ **Excluding test code is a HARD requirement of the widened matcher, not tidiness.** Test
/// fixtures multiply values constantly, and one of them is an exact copy of a production kernel:
/// `crates/vike-indicators/src/pairs_tests.rs`'s `spread_zscore_beta_one_is_bit_identical_to_unhedged`
/// keeps a `legacy` reimplementation of `batch_spread_zscore` — `run_sum2 += s[i] * s[i]` and
/// `var = run_sum2 / period as f64 - mean * mean` — inside its `#[cfg(test)] mod tests`. Without
/// this cut that pin would demand two more rows describing a function that ships nowhere.
///
/// Comment lines are skipped BEFORE the cut is looked for, which is the difference between this
/// and the byte-offset `text.find("#[cfg(test)]")` its sibling
/// `crates/vike-indicators/tests/libm_platform_probe.rs`'s
/// `production_code_calls_libm_not_the_platform` uses: a doc comment that merely NAMES the
/// attribute — this very paragraph would be one, were it under `src/` — would otherwise truncate
/// the scan at the mention and let everything below it pass unread.
///
/// The cut is per FILE and takes everything below the first marker, so a production function
/// placed after a test module is invisible. Measured on the tree this was written against: of the
/// ten files under `src/` carrying a `#[cfg(test)]`, none declares a function below it.
fn production_cut(lines: &[&str]) -> usize {
    for (i, line) in lines.iter().enumerate() {
        let t = line.trim_start();
        if t.starts_with("//") {
            continue;
        }
        if t.starts_with("#[cfg(test)]") {
            return i;
        }
    }
    lines.len()
}

/// What a given fold site does about a window whose values are all bit-equal.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum FoldGuard {
    /// Calls `window::is_constant_window` before folding — the one spelling of the law.
    Guarded,
    /// Exactly zero on a constant window WITHOUT the predicate, for a STRUCTURAL reason stated in
    /// the row. Not "measured zero once": a reason that cannot stop being true.
    ExactByConstruction,
    /// Carries the defect today. Declared with the reasoning that makes it a defect, and with its
    /// measured magnitude WHERE ONE HAS BEEN TAKEN. Deliberately not fixed here.
    ///
    /// ⚠ This doc used to read "declared with its measured value", full stop. That was accurate
    /// while the table held three residuals, all of them measured by hand before it was written;
    /// it stopped being accurate when the widened matcher above surfaced seven more in one sweep.
    /// Requiring a measurement per row would have meant either withholding the finding until seven
    /// numbers existed, or — far worse — declaring the sites as something they are not so the
    /// table could go green. A row that states the structural argument and admits the magnitude is
    /// unmeasured is worth strictly more than a row that does not exist.
    Residual,
    /// The SHAPE matched, but the site is not a fold over a window's values at all, so the
    /// constant-window question does not arise for it.
    ///
    /// Two families land here. The squaring PRIMITIVES themselves —
    /// `crates/vike-indicators/src/math.rs`'s `sq` / `cube` / `quart` — are the definitions every
    /// other row folds THROUGH; the matcher cannot tell a definition from a use, and they are
    /// declared here rather than special-cased in the scanner so the exclusion is visible in the
    /// table instead of buried in a predicate. And a squared quantity computed from the window's
    /// SHAPE rather than its VALUES — an integer index grid, a period, a fixed parameter — cannot
    /// be made rounding-sized by a constant window, because the window's values never enter it.
    NotAWindowFold,
}

/// ⚠ **One row per `(file, enclosing fn)` carrying an [`is_variance_fold_line`] hit under `src/`,
/// the NUMBER of hit lines in that function, and what it does about a constant window.**
///
/// This table exists because the defect it guards lived in the gap between two files that agreed in
/// prose. `crates/vike-indicators/src/window.rs` gained `is_constant_window` and documented the law
/// at length; `crates/vike-indicators/src/indicators/volatility.rs` carried three hand-rolled
/// copies of the same fold that never called it, and nothing anywhere compared those two facts.
///
/// ⚠ **The claim this doc used to make here was "a new fold — or a new hand-rolled copy of an
/// existing one — now reddens this test until it is classified, which is the only mechanism here
/// that scales past the sites known today." It did not scale, and it was not true.** It held only
/// for a fold spelled through a `|x| sq(` closure; a fold spelled as an explicit multiplication was
/// invisible, and seven of them already existed when that sentence was written. The claim is true
/// again as of the widening — for the three spellings [`self_multiplies`] and
/// [`is_variance_fold_line`] enumerate (`sq(`, `powi(`, and a self-multiplied `X * X`), and no
/// further; read their blind-spot lists before trusting it a third time. The one to read first is
/// `libm::pow(d, 2.0)`: it is the only square spelling this file cannot see that the sibling
/// source gate also permits.
///
/// The hit COUNT is the third field for the same reason the table exists at all. Keyed on
/// `(file, fn)` alone, a second hand-rolled fold added inside an ALREADY-declared function would
/// change nothing the gate compares — which is the failure mode this whole file is about, one level
/// down. `crates/vike-indicators/src/pairs.rs`'s `batch_correl` folds five such lines and
/// `batch_correl_log` three, so the count is doing real work rather than pinning a formatting
/// detail. A count that moves is a real change to how many squared deviations that function
/// computes: open the line and classify it before editing the number.
///
/// ⚠ Rows are keyed on the function's NAME within its file, so two same-named functions in one file
/// that BOTH fold — several `on_bar` impls live in
/// `crates/vike-indicators/src/indicators/base.rs` — would collapse into one row with a summed
/// count. Exactly one of them folds today. The narrow-marker table had the same property and never
/// stated it.
///
/// The `Residual` rows are the honest part: they are live instances of the same defect, left
/// because each needs a change this one does not carry.
const VARIANCE_FOLD_SITES: &[(&str, &str, usize, FoldGuard, &str)] = &[
    // ---- src/indicators/base.rs -------------------------------------------------------------
    (
        "src/indicators/base/windowed.rs",
        "batch_bollinger",
        1,
        FoldGuard::Residual,
        "`bollinger`'s three band LEVELS. At a 20-bar window flat at 0.1, `upper - lower` is \
         5.551115123125783e-17 rather than 0. Nothing divides by that gap and no NaN-vs-number \
         guard keys on it, so the consequence is a band narrower than the display precision rather \
         than an invented reading — unlike `bbands_pctb`, which is why this one waited. Fixing it \
         also has to move the streaming twin below in the same commit, so it is two coordinated \
         edits rather than one.",
    ),
    (
        "src/indicators/base/windowed.rs",
        "on_bar",
        1,
        FoldGuard::Residual,
        "The streaming twin of `batch_bollinger` above, folding the same two-pass variance over a \
         `VecDeque` window. It has to gain the predicate in the same commit as the batch kernel: \
         `on_bar_equals_vectorize_for_every_indicator` compares the two bit-for-bit, so fixing \
         either one alone turns this declared residual into a red test.",
    ),
    // ---- src/indicators/overlap.rs ----------------------------------------------------------
    (
        "src/indicators/overlap.rs",
        "batch_alma",
        1,
        FoldGuard::NotAWindowFold,
        "ALMA's Gaussian weight kernel, `exp(-(k - m)² / 2s²)`, which matches BOTH halves of the \
         shape on one line — `sq(k as f64 - m)` and `s * s`. Neither square reads a bar: `k` is a \
         loop index, `m` the offset centre and `s` the sigma, all three functions of `period` / \
         `offset` / `sigma` alone. The weights are identical for every window the indicator will \
         ever see, constant or not, so there is no rounding-sized non-zero for a predicate to \
         catch. Newly visible under the widened matcher; the closure marker never saw it because \
         the `sq` call sits inside an `exp`, not directly behind the `|k|`.",
    ),
    // ---- src/indicators/statistics.rs -------------------------------------------------------
    (
        "src/indicators/statistics.rs",
        "ols",
        1,
        FoldGuard::NotAWindowFold,
        "The OLS normal-equation denominator `p·Σx² − (Σx)²`, spelled `p as f64 * sx2 - sx * sx`. \
         It is a variance in form, but of the integer index grid `x = 0..p-1` rather than of the \
         data: `sx` and `sx2` are exact integer closed forms cast to f64 and depend only on \
         `period`. The window's values never enter it, so its `if denom == 0.0` branch is a real \
         degeneracy test on the grid rather than an epsilon a constant window can fool.",
    ),
    (
        "src/indicators/statistics.rs",
        "std_error_series",
        1,
        FoldGuard::Residual,
        "`std_error`, and `std_error_bands` through it: `Σ(window[j] − (a + b·j))²` around the OLS \
         fit, spelled `residuals_sq += sq(...)` — a STATEMENT rather than a closure, which is the \
         entire reason the old `| sq(` marker walked past it. On a constant window the fitted \
         slope is only APPROXIMATELY zero (`p·Σxy` and `Σx·Σy` are different fold orders of the \
         same quantity and cancel to rounding, not exactly), so every residual is rounding-sized \
         and `se` is a small positive number where zero is correct. Nothing divides by it, so the \
         consequence is `batch_bollinger`'s rather than `bbands_pctb`'s: bands narrower than the \
         display precision, not an invented reading. Magnitude not yet measured.",
    ),
    (
        "src/indicators/statistics.rs",
        "batch_skew",
        1,
        FoldGuard::Residual,
        "`skew`'s second moment, `m2 = Σd²/p` over `d = x − mean`, guarded by `if m2 == 0.0` — the \
         output epsilon the law exists to replace, in its purest form. Sharper than it looks: the \
         guard being fooled does not merely perturb the answer, it DIVIDES rounding by rounding. \
         `m3 / cube(sd)` has a numerator of order ε³ and a denominator of order ε³, so a window \
         that never moved reports an order-1 skew indistinguishable from a real one. Newly \
         visible: `|d| d * d` is the explicit multiplication the narrow marker could not see. \
         Magnitude not yet measured.",
    ),
    (
        "src/indicators/statistics.rs",
        "batch_kurtosis",
        2,
        FoldGuard::Residual,
        "`kurtosis`'s twin of `batch_skew` above, same `|d| d * d` fold and same `if m2 == 0.0` \
         guard, dividing `m4 / quart(sd)` — order ε⁴ over order ε⁴. The second hit line is the \
         bias-correction term `((p - 1) * (p - 1))`, integer period arithmetic that matches the \
         shape and folds nothing; it is counted rather than filtered because a matcher narrow \
         enough to drop it would be narrow enough to drop a real fold. Magnitude not yet measured.",
    ),
    (
        "src/indicators/statistics.rs",
        "batch_rank_correlation",
        2,
        FoldGuard::ExactByConstruction,
        "Spearman's `sum_d2 += d * d` over INTEGER rank differences, plus `p2 = p * p`. On a \
         constant window every comparison is a tie, the stable sort therefore leaves the original \
         index order, `price_rank[j]` is exactly `j + 1`, every `d` is exactly `0i64` and the sum \
         is exactly zero — in `i64`, with no float anywhere in the fold to round. That is a \
         property of the reduction and of the sort's documented stability, not a measurement that \
         could quietly stop holding.",
    ),
    // ---- src/indicators/volatility.rs -------------------------------------------------------
    (
        "src/indicators/volatility.rs",
        "stddev_series",
        1,
        FoldGuard::Guarded,
        "`stddev`, and `relative_volatility` through it.",
    ),
    (
        "src/indicators/volatility.rs",
        "bollinger_vals",
        1,
        FoldGuard::Guarded,
        "`bbands_width` and `bbands_pctb`.",
    ),
    (
        "src/indicators/volatility.rs",
        "batch_hvol",
        1,
        FoldGuard::Guarded,
        "`hvol`, over the LOG RETURNS rather than the closes.",
    ),
    (
        "src/indicators/volatility.rs",
        "batch_ulcer",
        1,
        FoldGuard::ExactByConstruction,
        "`ulcer` centres its deviations on `peak`, the window MAXIMUM. A maximum is a SELECTION \
         from the window, not an average of it, so on a constant window `peak` IS the repeated \
         value bit-for-bit and every `c - peak` is exactly 0 with no rounding to cancel. That is a \
         property of the reduction, not a measurement that could quietly stop holding.",
    ),
    // ---- src/math.rs ------------------------------------------------------------------------
    (
        "src/math.rs",
        "sq",
        1,
        FoldGuard::NotAWindowFold,
        "The crate's squaring PRIMITIVE itself — `x * x` is the definition every `sq(` row in this \
         table folds THROUGH, not a fold over any window. It appears here because the matcher \
         cannot tell a definition from a use, and it is declared rather than filtered out in the \
         scanner so that the exclusion is a row a reader can see instead of a predicate they have \
         to go find.",
    ),
    (
        "src/math.rs",
        "cube",
        1,
        FoldGuard::NotAWindowFold,
        "`x³` spelled from the square outward, `let s = x * x; s * x` — matched on the first of \
         those two lines. Same disposition and same reason as `sq` above.",
    ),
    (
        "src/math.rs",
        "quart",
        2,
        FoldGuard::NotAWindowFold,
        "`x⁴` as `(x²)²`, `let s = x * x; s * s` — BOTH lines match, which is why the count is 2. \
         Same disposition and same reason as `sq` above. That the second line is itself a \
         self-multiplication is the clearest small proof that the widened matcher keys on the \
         SHAPE rather than on a variance idiom.",
    ),
    // ---- src/pairs.rs -----------------------------------------------------------------------
    (
        "src/pairs.rs",
        "batch_spread_zscore",
        2,
        FoldGuard::Residual,
        "`spread_zscore`, and the worst-behaved fold in this table: an ACCUMULATED variance, \
         `run_sum2 / period − mean * mean`, guarded by `if sd != 0.0` and then DIVIDED by \
         (`(s[i] − mean) / sd`). The accumulated form does not merely fail to reach zero on a \
         constant spread, it subtracts two nearly-equal large numbers, which is why the code \
         already carries `.max(0.0)` — the difference can come out NEGATIVE. So the guard is \
         fooled and a spread that never moved reports an arbitrary z-score. Newly visible: both \
         hit lines (`s[i] * s[i]` and `mean * mean`) are explicit multiplications. Magnitude not \
         yet measured, and it is price-level-dependent by construction.",
    ),
    (
        "src/pairs.rs",
        "batch_beta",
        1,
        FoldGuard::Residual,
        "`beta`'s denominator variance, guarded by `if vb != 0.0` — the SAME NaN-vs-number branch \
         `bbands_pctb` has. A benchmark whose returns are bit-identical across the window — a \
         stalled or halted feed, the case a beta is least meaningful in — gives `vb` a \
         rounding-sized value instead of zero, so the guard is fooled and `cov / vb` returns an \
         arbitrary large number where NaN is correct. ⚠ This row used to call itself \"the \
         sharpest of the three residuals\"; that was true only of the three the narrow marker \
         could SEE. The widened scan puts four more of the same `!= 0.0` shape beside it, three of \
         them in this very file, and `batch_spread_zscore` above is sharper still.",
    ),
    (
        "src/pairs.rs",
        "batch_correl",
        5,
        FoldGuard::Residual,
        "`correl`'s accumulated closed form, `p·Σa² − (Σa)²` per leg, guarded by \
         `if denom != 0.0`. Five hit lines: the two running `+=` squares, the two `-=` squares \
         that evict the leaving bar, and the denominator that closes both. Same catastrophic \
         cancellation as `batch_spread_zscore`, with one mitigation the others lack — the result \
         is `.clamp(-1.0, 1.0)`, so a fooled guard yields a bounded nonsense correlation rather \
         than an unbounded one. Magnitude not yet measured.",
    ),
    (
        "src/pairs.rs",
        "batch_correl_log",
        3,
        FoldGuard::Residual,
        "`correl_log`, the log-return twin of `batch_correl` above: the same `p·Σa² − (Σa)²` \
         closed form and the same `if denom != 0.0` guard, folded over a `Vec` buffer per window \
         instead of a running accumulator, so it is three hit lines rather than five. Also \
         `.clamp(-1.0, 1.0)`-bounded. Magnitude not yet measured.",
    ),
    (
        "src/pairs.rs",
        "batch_half_life",
        1,
        FoldGuard::Residual,
        "`half_life`'s regressor variance `var_l`, and the single most valuable thing the widening \
         found. It is spelled `(lag[j] - mean_l) * (lag[j] - mean_l)` — the parenthesised \
         self-multiplication, the exact spelling the narrow marker could not see and the one \
         `crates/vike-indicators/tests/libm_platform_probe.rs`'s \
         `production_code_calls_libm_not_the_platform` ban on `.powi(` now steers authors \
         toward. It is guarded by `if var_l != 0.0` and then divided by, twice over: \
         `lambda = cov / var_l`, reported as \
         `−ln 2 / lambda`. ⚠ And this function's own doc comment PROMISES the opposite behaviour \
         — \"a degenerate window (`var(s_lag) == 0`) has no slope at all\", both it and a \
         non-reverting window yielding \"NaN — the signal to stand down rather than a number to \
         trade on\". A rounding-sized `var_l` turns that documented refusal into a finite \
         half-life. Magnitude not yet measured.",
    ),
    // ---- src/window.rs ----------------------------------------------------------------------
    (
        "src/window.rs",
        "rolling_var",
        1,
        FoldGuard::Guarded,
        "`var` and `zscore`, plus the research feature presets. The original home of the law.",
    ),
];

/// Recursively collect `.rs` files under `dir`.
fn rs_files(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap_or_else(|e| panic!("read_dir {dir:?}: {e}")) {
        let path = entry.expect("dir entry").path();
        if path.is_dir() {
            rs_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

/// The index of the `fn` item enclosing `line_idx`, and its name.
fn enclosing_fn(lines: &[&str], line_idx: usize) -> (usize, String) {
    for j in (0..line_idx).rev() {
        let t = lines[j].trim_start();
        let after = t
            .strip_prefix("fn ")
            .or_else(|| t.strip_prefix("pub fn "))
            .or_else(|| t.strip_prefix("pub(crate) fn "))
            .or_else(|| t.strip_prefix("pub(super) fn "));
        if let Some(rest) = after {
            let name = rest.split(['(', '<']).next().unwrap_or(rest).trim().to_string();
            return (j, name);
        }
    }
    (0, "<no enclosing fn>".to_string())
}

/// ⚠ **The constant-window law must have exactly ONE spelling, and every fold must be classified.**
///
/// Scans the production half of every file under `src/` for [`is_variance_fold_line`], resolves
/// each hit to its enclosing `fn`, counts the hits per function, and decides `Guarded` by looking
/// for an `is_constant_window` call between that `fn` and the FIRST hit inside it. The set of
/// `(file, fn, hit count)` triples found must equal [`VARIANCE_FOLD_SITES`] exactly — a new fold, a
/// moved fold, a deleted fold, an extra fold inside an already-declared function, or a changed
/// disposition all redden it.
///
/// Taking the guard from the FIRST hit is the conservative direction rather than a shortcut: the
/// call has to precede every hit in the function in order to precede the first one, so a function
/// that guards one fold and forgets another scans as `Residual` and has to be classified by hand.
///
/// ⚠ **Non-vacuity, and exactly how much of it has been re-verified.** The narrow-marker version of
/// this gate was mutation-tested in both directions: deleting the `is_constant_window` call from
/// any `Guarded` row flipped it to `Residual` and failed, and adding a hand-rolled variance fold
/// anywhere under `src/` failed until its row existed. The mechanism is unchanged by the widening
/// and those two arms still hold. The widened matcher's OWN arms — a self-multiplied fold added
/// under `src/`, and a second hit added inside an already-declared function so the count moves —
/// have NOT been re-run through mutation, because no `cargo` invocation was available where the
/// widening was written. Re-run them; do not assume them from this paragraph.
///
/// The two emptiness assertions guard the degenerate case where the matcher is refactored away and
/// the scan silently checks nothing while still passing — a shape this repo has shipped before, and
/// the shape the narrow marker was one refactor from re-entering.
#[test]
fn is_constant_window_is_the_only_spelling_of_the_constant_window_law() {
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mut files = Vec::new();
    rs_files(&root.join("src"), &mut files);
    files.sort();
    // A `#[cfg(test)] mod NAME;` module that lives in its own file is test code whole — the section
    // `production_cut` drops when the module sits inline — so the file is not scanned.
    let test_files = vike_model::libm_walk::cfg_test_module_files_under(&root.join("src"));
    files.retain(|p| !test_files.contains(p));

    let mut found: Vec<(String, String, usize, FoldGuard)> = Vec::new();
    let mut self_mul_hits = 0usize;
    for path in &files {
        let text = std::fs::read_to_string(path).expect("read source");
        let lines: Vec<&str> = text.lines().collect();
        let cut = production_cut(&lines);
        let rel = path
            .strip_prefix(&root)
            .expect("under manifest dir")
            .to_string_lossy()
            .replace('\\', "/");

        // `(fn name, hit count, disposition)`, in first-hit order within the file.
        let mut per_fn: Vec<(String, usize, FoldGuard)> = Vec::new();
        for (i, line) in lines.iter().enumerate().take(cut) {
            let t = line.trim_start();
            // A comment naming the shape is prose; an import naming `sq` is not a fold site; a
            // `fn` declaration line is skipped for the two reasons `is_fn_decl_line`'s doc gives.
            if t.starts_with("//") || t.starts_with("use ") || is_fn_decl_line(t) {
                continue;
            }
            if !is_variance_fold_line(line) {
                continue;
            }
            if self_multiplies(line) {
                self_mul_hits += 1;
            }
            let (start, name) = enclosing_fn(&lines, i);
            match per_fn.iter_mut().find(|(n, _, _)| *n == name) {
                Some(entry) => entry.1 += 1,
                None => {
                    // Everything between the enclosing `fn` and the fold is where the guard sits.
                    let guarded = lines[start..i]
                        .iter()
                        .any(|l| l.contains("is_constant_window(") && !l.contains(" fn "));
                    let disposition =
                        if guarded { FoldGuard::Guarded } else { FoldGuard::Residual };
                    per_fn.push((name, 1, disposition));
                }
            }
        }
        for (name, count, guard) in per_fn {
            found.push((rel.clone(), name, count, guard));
        }
    }

    assert!(
        !found.is_empty(),
        "no site matching {VARIANCE_FOLD_SHAPE} was found under src/ — the shape this gate keys \
         on was refactored away, so the gate is now checking nothing while still passing. \
         Re-derive it from the current spelling of the two-pass variance fold; do not delete this \
         test."
    );
    assert!(
        self_mul_hits > 0,
        "the `sq(` half of the shape matched but `self_multiplies` matched NOTHING under src/. \
         That half is the whole point of the 2026-08-25 widening — `pairs.rs`'s `batch_half_life` \
         spells its variance `(lag[j] - mean_l) * (lag[j] - mean_l)` and is seen by no other rule \
         here — so zero hits means the operand scanner is broken and the gate has silently \
         reverted to the blind spot it was written to close."
    );

    let mut declared: Vec<(String, String, usize, FoldGuard)> = VARIANCE_FOLD_SITES
        .iter()
        .map(|(f, n, c, g, _)| (f.to_string(), n.to_string(), *c, *g))
        .collect();
    declared.sort();
    let mut seen = found.clone();
    seen.sort();

    let declared_keys: Vec<(&String, &String, usize)> =
        declared.iter().map(|(f, n, c, _)| (f, n, *c)).collect();
    let seen_keys: Vec<(&String, &String, usize)> =
        seen.iter().map(|(f, n, c, _)| (f, n, *c)).collect();
    assert_eq!(
        seen_keys, declared_keys,
        "the set of squared-deviation folds under src/ changed. Every site needs a row in \
         VARIANCE_FOLD_SITES — `(file, enclosing fn, hit lines, disposition, why)` — saying what \
         it does about a constant window: `Guarded` (calls `window::is_constant_window`), \
         `ExactByConstruction` (with a structural reason), `NotAWindowFold` (the shape matched but \
         nothing folds a window's VALUES) or `Residual` (with the reasoning, and the magnitude if \
         one was measured). A new fold is not a formatting detail: it is a new copy of a law that \
         has already shipped this bug three times, and a count that moved means a function grew \
         one."
    );

    // `ExactByConstruction`, `NotAWindowFold` and `Residual` are indistinguishable to a scanner —
    // all three simply lack the call — so the disposition is only asserted where the scan can
    // actually tell: either side claiming `Guarded`.
    for ((f, n, _, want), (_, _, _, got)) in declared.iter().zip(seen.iter()) {
        if *want == FoldGuard::Guarded || *got == FoldGuard::Guarded {
            assert_eq!(
                want, got,
                "{f}'s `{n}` is declared {want:?} but scans as {got:?}. A `Guarded` row that lost \
                 its `is_constant_window` call is the exact regression this gate exists for; a row \
                 that gained one should be promoted to `Guarded` in the table."
            );
        }
    }
}
