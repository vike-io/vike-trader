//! The process-wide INSTALLED user-indicator set, and the accessors that read it.

use crate::engine::user::{user_bare_name_binds, user_line_accessors};
use std::sync::LazyLock;

/// The process-wide user-indicator set, installed once by a BINARY — see
/// [`install_user_indicators`].
pub(super) static INSTALLED: LazyLock<std::sync::OnceLock<Vec<crate::RhaiIndicator>>> =
    LazyLock::new(std::sync::OnceLock::new);

/// Installs the user's indicators (`user_data/indicators/`) process-wide, so EVERY strategy this
/// process compiles can call them — including through the six `harness::run_*` entry points and the
/// Studio runners, none of which take an indicator argument.
///
/// ## Why a process-wide set rather than a parameter on every path
///
/// The alternative is threading `&[RhaiIndicator]` through `run_backtest`, `run_paramscan`,
/// `run_paramscan_with`, `run_walkforward`, `run_paramscan_euler`, the Studio's `build_strategy` funnel and
/// the datahub proto verbs that call them — public signatures, all of which exist to describe a
/// PROFILE, none of which has anything to do with where a user keeps their files.
///
/// The shape is not new here: `vike_indicators::registry()` — the ~140 BUILT-IN indicators every
/// script already calls — is itself a process-wide static that no run path passes around. This makes
/// the user's own indicators the same kind of thing as the built-ins, which is also how an author
/// thinks of them.
///
/// ## The rule that keeps it honest: only a BINARY may call this
///
/// The precedent is `vike_log::init` — binaries call it, libraries use the facade. A library that
/// installed indicators would be reaching for a directory its caller can neither see nor override,
/// which is the exact defect `crates/vike-ops/tests/settings_secrets/settings_registry.rs`'s `CREDENTIAL_STORE_PIN`
/// ratchets down. So this takes ALREADY-COMPILED prototypes: the caller owns the directory
/// resolution and the file I/O (`crates/vike-script/src/load.rs`'s `load_user_indicators` +
/// `vike_model::paths::state_path::user_indicators_dir`), and this function performs none.
///
/// ⚠ That is a property of THIS function, not of the crate: its caller-side pair
/// `crates/vike-script/src/load.rs`'s `load_and_install_user_indicators` performs exactly the I/O
/// described above, so that a root wires the whole thing in one call instead of copying three
/// statements. The rule the pair preserves is that the CALLER names the directory — which it takes
/// as a parameter and never resolves for itself.
///
/// ## Once, and observably
///
/// Returns `Err` with the already-installed count if called twice, rather than silently keeping the
/// first set or replacing it: a second call means two places believe they own this, and both
/// answers would be wrong somewhere. `RhaiStrategy::compile_with_indicators` still takes an explicit
/// set and OVERRIDES the installed one per name, which is what lets a test bind its own without
/// touching process state.
pub fn install_user_indicators(indicators: Vec<crate::RhaiIndicator>) -> Result<(), String> {
    let n = indicators.len();
    INSTALLED.set(indicators).map_err(|_| {
        format!(
            "user indicators are already installed ({} of them); install_user_indicators is a \
             once-per-process call made by a binary at startup, and this second call of {n} would \
             have silently done nothing",
            INSTALLED.get().map(Vec::len).unwrap_or(0)
        )
    })
}

/// The installed user-indicator NAMES — the file stems, which is each indicator's identity. Empty
/// when a binary installed none.
///
/// ⚠ A name here is not necessarily a CALLABLE spelling: a multi-output file whose line 0 is not its
/// namesake has no bare name, exactly as `bollinger` does not. A surface advertising what a script
/// may TYPE must join this with [`installed_user_bare_call`] and [`installed_user_line_accessors`],
/// the way `vike-cli indicators` joins `RHAI_INDICATORS` with [`line_accessors`](crate::engine::builtin::line_accessors) —
/// `crates/vike-cli/src/cmd/indicators.rs`'s `user_call_forms` is that join.
pub fn installed_user_indicators() -> Vec<&'static str> {
    INSTALLED
        .get()
        .map(|v| v.iter().map(vike_indicators::Indicator::name).collect())
        .unwrap_or_default()
}

/// One installed user indicator's `param(name, default)` knobs, in first-seen order — the same
/// `(name, default)` pairs a built-in advertises as `IndicatorMeta::params`, for a surface that
/// prints a CALL FORM. Empty for an unknown name and for a file that declares no knob.
///
/// Exported for the same reason [`line_accessors`](crate::engine::builtin::line_accessors) is: an advertising surface must not REDERIVE the
/// call shape. A user indicator gets one registered form per argument count from zero up to this
/// list's length (`crates/vike-script/src/engine/user.rs`'s `register_user_indicators`), so a listing
/// that printed a bare `my_ema` beside
/// the built-ins' `sma(period=20)` would be telling the author their own indicator takes no
/// arguments — which is the same silent wrongness `vike-cli indicators` was narrowed to prevent,
/// pointed at the one indicator whose parameters the author actually chose.
///
/// ⚠ Deliberately keyed on the NAME rather than returned wholesale beside
/// [`installed_user_indicators`]: the pair is then two lookups against one static, and a caller that
/// wants only the names never pays for the `String` clones the parameters cost.
pub fn installed_user_indicator_params(name: &str) -> Vec<(&'static str, f64)> {
    installed_by_name(name)
        .map(|i| i.params().iter().map(|(n, d)| (n.as_str(), *d)).collect())
        .unwrap_or_default()
}

/// One INSTALLED user indicator, by name — the lookup every `installed_user_*` accessor shares, so
/// the `OnceLock` is read ONE way and no caller is handed a `RhaiIndicator` of its own to stream.
fn installed_by_name(name: &str) -> Option<&'static crate::RhaiIndicator> {
    INSTALLED.get()?.iter().find(|i| vike_indicators::Indicator::name(*i) == name)
}

/// One INSTALLED user indicator's CALLABLE per-line spellings, `(line name, function name)` in
/// declaration order — the [`line_accessors`](crate::engine::builtin::line_accessors) twin for a file the user wrote, keyed on the name for
/// the same reason [`installed_user_indicator_params`] is.
///
/// Empty for a single-output file (its bare name already IS line 0, exactly as on the built-in
/// side) and for a name nothing installed.
///
/// ⚠ Exported because [`installed_user_indicators`] alone stopped being the callable set the moment
/// a user file could declare `fn outputs()`: a band file has NO bare name (see
/// [`installed_user_bare_call`]) and is reachable only through these. A surface that listed the
/// names alone would print a call that raises on every bar and omit every spelling that works —
/// the exact failure `vike-cli indicators` exists to prevent, pointed at the one indicator the
/// author wrote themselves.
pub fn installed_user_line_accessors(name: &str) -> Vec<(String, String)> {
    installed_by_name(name).map(user_line_accessors).unwrap_or_default()
}

/// Whether an INSTALLED user indicator's BARE name resolves — the user-side twin of
/// `RHAI_INDICATORS.contains(..)`, answered by the shared [`bare_name_binds`](crate::engine::bindable::bare_name_binds).
///
/// `false` with a non-empty [`installed_user_line_accessors`] is the band shape: reachable, but
/// only line by line. `false` is also the answer for a name nothing installed, which is the truth
/// about it — no spelling of it resolves.
pub fn installed_user_bare_call(name: &str) -> bool {
    installed_by_name(name).is_some_and(|i| user_bare_name_binds(name, i.outputs()))
}
