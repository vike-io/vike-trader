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
    // ---- src/indicators/statistics.rs -------------------------------------------------------
    // ---- src/indicators/volatility.rs -------------------------------------------------------
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
    // ---- src/window.rs ----------------------------------------------------------------------
    (
        "src/window.rs",
        "rolling_var",
        1,
        FoldGuard::Guarded,
        "`var` and `zscore`, plus the research feature presets. The original home of the law.",
    ),
    // ---- src/roll.rs (the O(1) rolling primitives) -------------------------------------------
    (
        "src/roll.rs",
        "push",
        3,
        FoldGuard::Residual,
        "`BollingerCore` carries SHIFTED running sums (`x - shift`, the window's oldest value at the \
         last rebuild) and rebuilds them from the window every `ceil(512 / n)` laps, and at once \
         when the leaving term dwarfs the result. A window that is constant FROM the rebuild on is \
         exactly zero (every deviation is 0.0); a window that BECOMES constant keeps the rounding \
         residue of the sums until the next rebuild — competitor-level noise, accepted by the \
         owner (2026-10-14, speed over bit-exactness), and bounded by the rebuild, unlike a plain \
         running `E[x^2] - E[x]^2`. Not measured per indicator; the variance clamps at 0. (The \
         general `Roll*` primitives that shared this row were deleted: nothing called them.)",
    ),
    (
        "src/roll.rs",
        "shifted_sums",
        1,
        FoldGuard::ExactByConstruction,
        "`BollingerCore`'s rebuild folds `d = v - shift` and `d * d` from scratch with `shift` the \
         window's oldest finite value: a window of bit-equal values has every `d == 0.0`, so both \
         sums are exactly zero without a predicate.",
    ),
    // ---- src/fast_b.rs, src/fast_d2.rs (the second wave's cores) ------------------------------
    (
        "src/fast_b.rs",
        "new",
        1,
        FoldGuard::NotAWindowFold,
        "`k2: keep * keep`, a coefficient of the two-step EMA recurrence (`keep = 1 - alpha`), \
         fixed by the period alone. No window's values enter it, so a constant window cannot make it \
         rounding-sized.",
    ),
    (
        "src/fast_d2.rs",
        "push",
        4,
        FoldGuard::ExactByConstruction,
        "`StddevCore`'s shifted running sums (`d * d - o * o` on `x - shift`). A window of bit-equal \
         values is EXACTLY `0.0` because the variance compare `var >= smax * inv * 1e-5` fails on \
         the rounding residue, takes the cold path, re-centres the shift on the repeated value (every \
         deviation is then `0.0`) and sums again; the compare is then `0 >= 0` and stays exact. A \
         structural property of that compare, not a measured value.",
    ),
    (
        "src/fast_d2.rs",
        "shifted_sums",
        1,
        FoldGuard::ExactByConstruction,
        "The rebuild folds `d = v - shift` and `d * d` from scratch with `shift` the window's \
         oldest finite value: a window of bit-equal values has every `d == 0.0`, so both sums are \
         exactly zero without a predicate.",
    ),
    // ---- src/fast_e.rs .. src/fast_j.rs (the third wave's cores) -------------------------------
    (
        "src/fast_e.rs",
        "new",
        1,
        FoldGuard::NotAWindowFold,
        "`AlmaCore`'s Gaussian weight kernel, `exp(-sq(k - m) / (2 s s))`: `k` is a loop index and `m` \
         and `s` come from `period` / `offset` / `sigma` alone, so the weights are the same for every \
         window the indicator will see. No window value enters it (the same argument as the \
         `batch_alma` row this replaces).",
    ),
    (
        "src/fast_h.rs",
        "shifted_sums",
        1,
        FoldGuard::ExactByConstruction,
        "The rebuild folds `d = v - shift` and `d * d` from scratch with `shift` the window's oldest \
         finite value: a window of bit-equal values has every `d == 0.0`, so both sums are exactly \
         zero without a predicate.",
    ),
    (
        "src/fast_h.rs",
        "push",
        8,
        FoldGuard::ExactByConstruction,
        "Four lines are shifted running sums of the standard-deviation and channel cores (`hvol`, \
         `relative_volatility`, `linreg_channel`: `d * d - o * o`, `ms - m * m`, `syy - sy * sy / n`). \
         A variance or residual sum that is not above a fixed fraction of the second moment \
         (`VAR_FLOOR`, `FIT_FLOOR`) is not trusted: it takes the cold path (`var_cold`, `fit_cold`), \
         which returns exactly `0.0` for a window of bit-equal values. A structural property of that \
         compare, not a measured value. The other four lines are McGinley's recurrence coefficients \
         (`c * c`, `prev * prev`): a function of the previous output and the close, not a fold over a \
         window.",
    ),
    (
        "src/fast_h.rs",
        "var_cold",
        1,
        FoldGuard::ExactByConstruction,
        "Rebuilds the shifted sums and, when they cannot resolve the variance, hands the window to \
         `exact_sd`, which answers `0.0` for a window of bit-equal values.",
    ),
    (
        "src/fast_h.rs",
        "exact_sd",
        1,
        FoldGuard::ExactByConstruction,
        "The long way: it returns `0.0` before folding anything when every value of the window is \
         bit-equal, else the mean and then the sum of squared deviations from it.",
    ),
    (
        "src/fast_h.rs",
        "new",
        1,
        FoldGuard::NotAWindowFold,
        "`n2: nf * nf`, a constant of the linear-regression normal equations fixed by the window \
         length alone.",
    ),
    (
        "src/fast_h.rs",
        "of",
        1,
        FoldGuard::ExactByConstruction,
        "`Fit::of` folds `d = v - shift` with `shift` the window's oldest finite value: a window of \
         bit-equal values has every `d == 0.0`, so `syy` is exactly zero without a predicate.",
    ),
    (
        "src/fast_h.rs",
        "fit_cold",
        2,
        FoldGuard::ExactByConstruction,
        "A window of bit-identical values is answered as its own line (`mid = first`, `sd = 0.0`) \
         before any residual is folded; the residual sums are reached only by windows with at least \
         two different values.",
    ),
    (
        "src/fast_i.rs",
        "new",
        2,
        FoldGuard::NotAWindowFold,
        "`k2: keep * keep` (a coefficient of the two-step EMA recurrence) and the regression \
         denominator `n2 * (n2 - 1) / 12` of `ttm_squeeze`'s momentum, both fixed by the period \
         alone.",
    ),
    (
        "src/fast_i.rs",
        "push",
        3,
        FoldGuard::ExactByConstruction,
        "`TtmSqueezeCore`'s shifted running sums (`d * d - o * o`, `s2 / n - m * m`). A variance not \
         above `smax * 1e-5 / n` falls to the cold path, which re-centres the shift on the window's \
         oldest value and sums again, so a window of bit-equal values is exactly `0.0` (the same \
         compare as `StddevCore`).",
    ),
    (
        "src/fast_i.rs",
        "shifted_sums",
        1,
        FoldGuard::ExactByConstruction,
        "The rebuild folds `d = v - shift` and `d * d` from scratch with `shift` the window's oldest \
         finite value: a window of bit-equal values has every `d == 0.0`.",
    ),
    (
        "src/fast_j.rs",
        "shifted_sums",
        1,
        FoldGuard::ExactByConstruction,
        "The rebuild folds `d = v - shift` and `d * d` from scratch with `shift` the window's oldest \
         finite value: a window of bit-equal values has every `d == 0.0`.",
    ),
    (
        "src/fast_j.rs",
        "push",
        8,
        FoldGuard::ExactByConstruction,
        "Four lines are shifted running sums (`BandCore`: `d * d - o * o`, `s2 / n - m * m` twice; the \
         close-to-close `SampleVar`: `d * d - o * o`) and one is `UlcerCore`'s drawdown sum. Each \
         compares its result with `smax * 1e-5` and sends a too-small or `NaN` result to a cold \
         rebuild that re-centres on the window (a peak, for the ulcer) so a flat window is exactly \
         `0.0`. Three lines are per-bar terms (`r * r`, `hl * hl`, `co * co`) of the log-return \
         estimators: they square one bar's own return, not a deviation from a window mean.",
    ),
    (
        "src/fast_j.rs",
        "step",
        1,
        FoldGuard::ExactByConstruction,
        "`Shifted::step` of the Yang-Zhang streams: `d * d - o * o` on values shifted by the window's \
         oldest. Its reader `spread` compares with `smax * 1e-5` and the cold path `triple_sums` \
         re-centres, so a flat window is exactly `0.0`.",
    ),
    (
        "src/fast_j.rs",
        "triple_sums",
        2,
        FoldGuard::ExactByConstruction,
        "The rebuild folds `dx * dx` and `dy * dy` with each shift the oldest finite value of its \
         stream: a window of bit-equal values has every deviation `0.0`.",
    ),
    (
        "src/fast_j.rs",
        "drawdown_sums",
        1,
        FoldGuard::ExactByConstruction,
        "`UlcerCore`'s rebuild folds `d = v - peak` with `peak` the running maximum: a window of \
         bit-equal values has the peak equal to the value, so every drawdown is `0.0`.",
    ),
    // ---- src/fast_l.rs, src/fast_r.rs (the fourth wave's cores) -------------------------------
    (
        "src/fast_l.rs",
        "of",
        7,
        FoldGuard::ExactByConstruction,
        "The from-scratch rebuilds of the statistics cores (moments, regression, pairs). The sums \
         are folded on d = v - shift with shift the window's oldest finite value (or the first \
         value), so a window of bit-equal values has every d == 0.0 and the sums are exactly zero \
         without a predicate.",
    ),
    (
        "src/fast_l.rs",
        "push",
        6,
        FoldGuard::ExactByConstruction,
        "The statistics cores' shifted running sums (d * d - o * o, fourth powers). A result the \
         running sums cannot resolve (a variance or residual not above a fixed fraction of the \
         second moment, a NaN, a stale sum) takes the cold path, which rebuilds the sums on the \
         window's differences from its first value, so a window of bit-equal values is exactly \
         0.0 (a structural property of that compare, as in StddevCore).",
    ),
    (
        "src/fast_l.rs",
        "step",
        3,
        FoldGuard::ExactByConstruction,
        "The regression core's step (syy += d * d - o * o). A result the running sums cannot \
         resolve (a variance or residual not above a fixed fraction of the second moment, a NaN, \
         a stale sum) takes the cold path, which rebuilds the sums on the window's differences \
         from its first value, so a window of bit-equal values is exactly 0.0 (a structural \
         property of that compare, as in StddevCore).",
    ),
    (
        "src/fast_l.rs",
        "moment_eval",
        3,
        FoldGuard::ExactByConstruction,
        "The central moments from the shifted sums, reached only for a resolved variance; an \
         unresolved one never gets here, it takes moments_cold.",
    ),
    (
        "src/fast_l.rs",
        "moments_cold",
        1,
        FoldGuard::ExactByConstruction,
        "The cold rebuild of skew and kurtosis: evaluated on the window's differences from its \
         first value, so a flat window gives m2 = m3 = m4 = 0 and the answer 0.0 (tested on a \
         flat 0.1).",
    ),
    (
        "src/fast_l.rs",
        "new",
        3,
        FoldGuard::NotAWindowFold,
        "Constants of the cores fixed by the period alone (n * n, the bias-correction terms). No \
         window value enters them.",
    ),
    (
        "src/fast_l.rs",
        "ols_ordered",
        1,
        FoldGuard::NotAWindowFold,
        "The OLS normal-equation denominator p * sum(x^2) - sum(x)^2 of the integer index grid; \
         it depends on the period alone, as the ols row it replaces did.",
    ),
    (
        "src/fast_l.rs",
        "std_error_ordered",
        1,
        FoldGuard::Residual,
        "The old std_error fold, sum((window[j] - (a + b j))^2) around the OLS fit, kept as it \
         was: on a constant window the fitted slope cancels to rounding, not exactly, so the \
         residuals are rounding-sized where zero is correct. The same finding as the \
         std_error_series row it replaces; magnitude not measured.",
    ),
    (
        "src/fast_l.rs",
        "r_squared_ordered",
        2,
        FoldGuard::ExactByConstruction,
        "Returns early on `constant(..)`, the ring form of window::is_constant_window, before any \
         centred sum is folded: a flat window never reaches the squared-deviation fold (NaN \
         here).",
    ),
    (
        "src/fast_l.rs",
        "var_ordered",
        1,
        FoldGuard::ExactByConstruction,
        "Returns early on `constant(..)`, the ring form of window::is_constant_window, with 0.0 for \
         a window of identical values, before the squared deviations from the mean are folded.",
    ),
    (
        "src/fast_l.rs",
        "skew_ordered",
        1,
        FoldGuard::ExactByConstruction,
        "The old bias-corrected skewness evaluated on the window's differences from its first \
         value: a window of bit-equal values has every difference 0.0, so the third and second \
         moments are exactly zero and the answer is 0.0.",
    ),
    (
        "src/fast_l.rs",
        "kurtosis_ordered",
        1,
        FoldGuard::ExactByConstruction,
        "The old bias-corrected excess kurtosis evaluated on the window's differences from its \
         first value: a window of bit-equal values has every difference 0.0, so the answer is \
         exactly 0.0.",
    ),
    (
        "src/fast_l.rs",
        "pair_moments",
        2,
        FoldGuard::Guarded,
        "autocorr's centred sums over the overlapping pairs; it is called only after \
         window::is_constant_window has rejected a flat side, which is NaN.",
    ),
    (
        "src/fast_l.rs",
        "rescaled_range",
        1,
        FoldGuard::Guarded,
        "hurst's rescaled range of one block; calls window::is_constant_window on the block first \
         and answers nothing for a flat one.",
    ),
    (
        "src/fast_l.rs",
        "log_log_slope",
        1,
        FoldGuard::NotAWindowFold,
        "hurst's regression of log(R/S) on log(block size): the squared deviations are of the \
         log-size grid, which depends on the block sizes alone, never on the data.",
    ),
    (
        "src/fast_r.rs",
        "shifted_sums",
        1,
        FoldGuard::ExactByConstruction,
        "The pair cores' rebuild. The sums are folded on d = v - shift with shift the window's \
         oldest finite value (or the first value), so a window of bit-equal values has every d == \
         0.0 and the sums are exactly zero without a predicate.",
    ),
    (
        "src/fast_r.rs",
        "co_sums",
        2,
        FoldGuard::ExactByConstruction,
        "The two-stream rebuild folds du * du and dv * dv with each shift the oldest finite value \
         of its stream: a window of bit-equal values has every deviation 0.0.",
    ),
    (
        "src/fast_r.rs",
        "push",
        3,
        FoldGuard::ExactByConstruction,
        "The pair cores' shifted running sums (d * d - o * o, the variance from them). A variance \
         not above a fixed fraction of the second moment takes the cold re-centre, so a bit-equal \
         window is exactly zero variance, and the result is NaN (a flat side has no correlation \
         or z-score).",
    ),
    (
        "src/fast_r.rs",
        "moments",
        2,
        FoldGuard::ExactByConstruction,
        "The variances of the two sides from the shifted sums (pp * suu - su * su): exactly 0.0 \
         for a bit-equal window because the sums of deviations are exactly zero.",
    ),
    // ---- src/indicators/oscillators.rs, overlays.rs (post-release additions) ---------------------
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
