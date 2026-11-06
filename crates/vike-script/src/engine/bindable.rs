//! Which registry indicators a script may call by their bare name, and why the rest may not.

use crate::engine::UNBOUND;
use crate::engine::builtin::MAX_INDICATOR_ARITY;
use crate::engine::host::HOST_FN_NAMES;
use std::sync::LazyLock;

/// Why a `vike_indicators::registry()` entry is deliberately NOT bound as a Rhai host function —
/// `None` when it IS bound.
///
/// Pure over the four facts that decide it (rather than taking an `IndicatorMeta`) so that every
/// rule is unit-testable, INCLUDING the two that exclude nothing in today's registry. A rule that
/// can only be exercised by the data it happens to receive is a rule nobody can prove works — this
/// tree has already watched declaration-pinning tests stay green through real regressions, and a
/// dormant rule is the easiest place for that to happen again.
///
/// The rules, each a silent-WRONG-ANSWER hazard rather than a matter of taste:
///
/// 1. **Not callable from Rhai.** `var` (Variance) is a Rhai RESERVED WORD
///    (rhai-1.25.1 `src/tokenizer.rs`'s `RESERVED_LIST` holds `("var", true, false, false)`, whose
///    second flag — "may be called normally as a function" — is `false`), so `var(20)` fails at
///    PARSE time with a message about rhai's grammar, naming nothing an author can act on.
///    `Engine::register_fn` performs no name validation, so binding it would SUCCEED silently and
///    fail only when somebody wrote the call. Determined by asking rhai itself rather than by a
///    hand list, so a future rhai release that reserves another word cannot leave an
///    advertised-but-uncallable name behind.
/// 2. **Collides with a host function.** See [`HOST_FN_NAMES`].
/// 3. **`batch_only`.** `ichimoku`/`zigzag`/`williams_fractal` read FUTURE bars, so their streamed
///    value is a causal best-effort that the batch path later revises — a backtest-vs-chart
///    divergence that shows up as a wrong number, never as an error. (`ichimoku`'s line 0 is the
///    strictly-causal tenkan and would in fact be safe, but it is multi-output and excluded by
///    rule 4 anyway; excluding all three on the flag keeps ONE rule instead of a per-indicator
///    judgement.)
/// 4. **Multi-output UNDER ITS BARE NAME.** The bare-name form returns one scalar — `on_bar`'s line
///    0 — and for the BAND indicators that is the wrong line outright: `bollinger`/`donchian`/
///    `keltner`/`envelopes`/`std_error_bands` all declare `out!{"upper", "mid", "lower"}`, so
///    `bollinger(20)` would return the UPPER band to an author who read it as the middle.
///
///    ⚠ This rule NARROWED. It used to reject every multi-output indicator outright, and that was
///    too broad in one direction and not a solution in the other. Too broad, because on six of them
///    — `macd`, `adx`, `fisher`, `kst`, `kvo`, `supertrend` — line 0's own `OutSpec::name` IS the
///    indicator's name, so `macd()` was always the macd line and was refused for a hazard that does
///    not apply to it. Not a solution, because refusing `bollinger` left its middle band
///    unreachable by any spelling at all.
///
///    So the test is now NAMESAKE, not arity: a bare name binds when line 0 is named after the
///    indicator, and every line of every multi-output indicator is separately reachable through the
///    per-line accessors [`line_fn_name`] generates. `bollinger` is still refused under its bare
///    name — `bollinger_mid()` is the spelling — and that refusal is now a signpost rather than a
///    dead end.
pub(super) fn exclusion(
    name: &str,
    batch_only: bool,
    line0: Option<&str>,
    outputs: usize,
    params: usize,
    callable_in_rhai: bool,
) -> Option<&'static str> {
    if !callable_in_rhai {
        return Some(
            "the name is a Rhai reserved word or symbol, so a script calling it fails at PARSE \
             time (today: `var` -> Variance; call it from Rust, or use `stddev`)",
        );
    }
    if HOST_FN_NAMES.contains(&name) {
        return Some(
            "the name collides with a host read/verb, and binding it would SHADOW that function \
             for every script",
        );
    }
    if batch_only {
        return Some(
            "batch_only: it reads FUTURE bars, so the streamed value is retroactively revised and \
             disagrees with the chart",
        );
    }
    if params > MAX_INDICATOR_ARITY {
        return Some(
            "more parameters than the bridge can express: `register_indicators` writes forms up to              MAX_INDICATOR_ARITY, so binding this would pin every parameter past that to its              registry default — knobs that look present and are not (today: `kst`, 9 parameters).              Construct it from Rust, or add the arity arms and raise the constant",
        );
    }
    if !bare_name_binds(name, outputs, line0) {
        return Some(
            "multi-output whose line 0 is NOT the namesake line, so a bare call would return a \
             line the caller did not ask for (`bollinger` returns `upper` first). Every line has \
             its own accessor — call `<name>_<line>()`, e.g. `bollinger_mid()`",
        );
    }
    None
}

/// Whether an indicator's BARE name binds, and therefore IS line 0 — [`exclusion`]'s rule 4, and
/// the one function both sides of that rule ask.
///
/// A single-output indicator's bare name is its one line, so it always binds. A multi-output one's
/// binds only when line 0 is the NAMESAKE line, because otherwise the bare call returns a line the
/// caller did not ask for (`bollinger()` handing back `upper`), and every line is separately
/// reachable through [`line_fn_name`] either way.
///
/// ⚠ It sanitises the INDICATOR name as well as the line, which the registry side never needed:
/// every registry name is already a bare lowercase identifier, while a USER file's stem is whatever
/// the filesystem allowed (`Shouty.RHAI` loads today — `crates/vike-script/src/load_tests.rs`'s
/// `the_extension_is_matched_case_insensitively`). Sharing ONE function is therefore only
/// behaviour-preserving for the built-ins while `sanitize_line` is the IDENTITY on a registry name
/// — a claim about DATA, so it is a test, `one_namesake_rule_serves_the_registry_and_the_user_files`,
/// which fails the day an entry takes a capital, a dash or a `%` rather than silently flipping
/// which bare names bind.
pub(crate) fn bare_name_binds(name: &str, outputs: usize, line0: Option<&str>) -> bool {
    outputs == 1 || line0.map(sanitize_line) == Some(sanitize_line(name))
}

/// One output line's name, as the suffix of a Rhai host-function name.
///
/// Lowercases and replaces every character a Rhai identifier cannot carry with `_`. This exists for
/// exactly one family and it is not hypothetical: `stochastic`, `stochf` and `stochrsi` declare
/// their lines as **`%K`** and **`%D`** (`crates/vike-indicators/src/registry.rs`'s `out` macro),
/// and `stochastic_%K` is not a name a script can call — it is a PARSE error about rhai's grammar,
/// which is the same dead end [`exclusion`]'s first rule refuses `var` for.
///
/// ⚠ Sanitising is lossy in principle: two distinct line names can map onto one suffix (`%K` and
/// `K`), which would silently give one accessor two meanings. That is a silent-wrong-answer hazard,
/// so it is GATED rather than assumed away — `generated_line_names_are_unambiguous` proves the whole
/// generated set is collision-free, and it is written to fail on the future indicator that breaks
/// it rather than on today's registry, which is clean.
pub(crate) fn sanitize_line(line: &str) -> String {
    line.chars()
        .map(|c| match c.to_ascii_lowercase() {
            c @ ('a'..='z' | '0'..='9') => c,
            // Everything else — `%`, a space, a dash — becomes `_`, then the trim below drops any
            // that ended up leading or trailing. `%K` therefore reads as `k`, not `_k`.
            _ => '_',
        })
        .collect::<String>()
        .trim_matches('_')
        .to_string()
}

/// The Rhai host-function name for one output line: `"<indicator>_<line>"`.
///
/// `bollinger_mid`, `macd_signal`, `adx_plus_di`, `stochastic_k`. The separator is `_` because that
/// is what the registry's own multi-word line names already use (`plus_di`, `aroon_up`,
/// `bull_power`), so the composite reads as one identifier rather than as two conventions joined.
pub fn line_fn_name(indicator: &str, line: &str) -> String {
    format!("{indicator}_{}", sanitize_line(line))
}

/// The EXACT indicator names [`register_indicators`](crate::engine::builtin::register_indicators) binds as Rhai host functions — the
/// HOST-BOUND callable set. Exported (re-exported at the crate root) so any surface that advertises
/// "the indicators a script can call" (vike-cli's MCP `list_indicators` tool) lists THIS set and
/// never `vike_indicators::registry()` wholesale: a script calling an unbound registry name hits a
/// Rhai function-not-found error every bar and self-disables after the strategy's
/// consecutive-error cap (see `strategy.rs`).
///
/// DERIVED, not written down: it is `registry()` in registry order, minus every entry
/// [`unbound_reason`] rejects. `register_indicators` filters on the same predicate, so the binding
/// cannot drift from the advertisement in either direction, and a new registry indicator becomes
/// callable the moment it lands — no list to remember. That is why this is a `LazyLock<Vec<..>>`
/// rather than the `&'static [&'static str]` const it used to be (`registry()` is built at run
/// time, so no `const` can name its contents); ⚠ a consumer iterating it directly needs
/// `RHAI_INDICATORS.iter()` — `LazyLock` derefs for method calls but does not implement
/// `IntoIterator`.
///
/// The count is deliberately not stated here. `RHAI_INDICATORS.len()` is the answer, and every
/// prose copy of a number in this workspace has rotted.
pub static RHAI_INDICATORS: LazyLock<Vec<&'static str>> = LazyLock::new(|| {
    vike_indicators::registry()
        .iter()
        .map(|m| m.name)
        .filter(|n| !UNBOUND.contains_key(n))
        .collect()
});

/// Why `name` — a `vike_indicators::registry()` indicator — is NOT callable from a Rhai script, or
/// `None` when it is bound (and also `None` for a name that is in no registry at all: an unknown
/// name is a typo, not an exclusion).
///
/// Exported so an advertising surface can say WHY a real indicator is missing instead of silently
/// omitting it, and so a downstream test needing a guaranteed-unbound name can ask rather than
/// hard-code one that later becomes bound.
pub fn unbound_reason(name: &str) -> Option<&'static str> {
    UNBOUND.get(name).copied()
}
