//! The per-type override POWER, expressed as traits — the load contract's teeth.
//!
//! The precedence order is one line and applies to everything:
//!
//! ```text
//! code defaults -> <project>/settings/*.toml -> env -> CLI
//! ```
//!
//! What differs per type is not the ORDER, it is WHICH LAYERS EXIST:
//!
//! | type              | defaults | file | env | CLI |
//! |-------------------|----------|------|-----|-----|
//! | [`crate::Policy`] | yes      | yes  | NO  | NO  |
//! | [`crate::Config`] | yes      | yes  | yes | yes |
//! | [`crate::Preferences`] | yes | yes  | yes | yes |
//! | [`crate::Flags`]  | yes      | yes  | yes | yes |
//!
//! **That table is enforced HERE, by the type system, not by a comment.** `Config`,
//! `Preferences` and `Flags` implement [`EnvOverride`] and [`CliOverride`]; `Policy` implements
//! NEITHER, and has no `from_env`/`apply_env`/`apply_cli` inherent method either. So
//! `settings.policy.apply_env(&env)` is not a discouraged call — it is a **compile error**, because
//! there is no such method to resolve, on the type or on any trait the type implements.
//!
//! Why that matters enough to spend a trait on: **a ceiling you can override with an environment
//! variable is not a ceiling.** Every other guard in this workspace can be argued about; this one
//! cannot, because the failure mode is silent. An operator who exports `VIKE_MAX_LEVERAGE=50` in a
//! systemd unit, a CI job or a stale shell would raise the risk ceiling with no file changed, no
//! review, and no diff — and the run would look exactly like a compliant one. Making the override
//! unrepresentable removes the whole class instead of documenting it.
//!
//! Both traits are **sealed** (by a crate-private `sealed::Sealed` supertrait): a downstream crate cannot bolt an
//! `impl EnvOverride for Policy` on from outside. Inside this crate it would take deliberately
//! adding `Policy` to the `Sealed` impl list below — one line, in a file whose entire purpose is
//! this rule, visible in any diff. That is the strongest form available in Rust short of a
//! newtype-per-layer, and it is the reason the seal exists rather than a bare public trait.

use std::collections::HashMap;

use crate::error::ConfigError;

/// Seals [`EnvOverride`] and [`CliOverride`] so the per-type table above cannot be extended from
/// outside this crate.
///
/// ⚠ [`crate::Policy`] is deliberately ABSENT from the impl list. Adding it here is the single
/// edit that would let a ceiling become env-overridable — do not make it without re-reading this
/// module's doc.
pub(crate) mod sealed {
    /// Implemented by exactly the three types that may be overridden past the file layer.
    pub trait Sealed {}
    impl Sealed for crate::config::Config {}
    impl Sealed for crate::preferences::Preferences {}
    impl Sealed for crate::flags::Flags {}
}

/// A settings type the ENVIRONMENT may override. Sealed — see the module doc.
///
/// The env map is a PARAMETER, never read from the process here: libraries take configuration as
/// arguments and only binaries touch `std::env` or the workspace `.env` (CLAUDE.md's settings
/// rule, and the `vike_tradehub_client::auth::from_vars` precedent). It is also what makes every test below a pure
/// function of its inputs.
//
// `private_bounds`: a public trait with a crate-private supertrait IS the sealed-trait pattern,
// and the seal is the whole point here (see the module doc) — the lint is describing the intended
// design, not a mistake, so it is allowed at the two sites that implement the seal rather than
// worked around by making `Sealed` reachable, which would unseal it.
#[allow(private_bounds)]
pub trait EnvOverride: sealed::Sealed {
    /// Apply every variable this type recognizes, in place. An unset variable leaves its field
    /// alone; a SET-but-unparseable variable is an error naming the variable and its value.
    fn apply_env(&mut self, env: &HashMap<String, String>) -> Result<(), ConfigError>;
}

/// A settings type a COMMAND-LINE flag may override — the last and highest layer. Sealed.
#[allow(private_bounds)] // see `EnvOverride` above
pub trait CliOverride: sealed::Sealed {
    /// Apply the subset of `cli` this type owns. Every field of [`CliOverrides`] is optional;
    /// `None` leaves the underlying value untouched.
    fn apply_cli(&mut self, cli: &CliOverrides) -> Result<(), ConfigError>;
}

/// The CLI layer, as data.
///
/// A struct of `Option`s rather than a parsed-argv type on purpose: this crate does not own the
/// argument grammar (each binary spells its own flags — `vike-cli` hand-rolls, `vike-recorder`
/// takes `--profile`), it owns only what a flag is allowed to REACH. A binary parses its own argv
/// and fills this in.
///
/// ⚠ There is deliberately no policy field of any kind, and no way to add one from outside: CLI
/// arguments leak into `ps`, shell history and CI logs, and a risk ceiling must not be settable
/// from a place that is both unaudited and world-readable.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CliOverrides {
    /// `--store` — overrides [`crate::Config::store_root`].
    pub store_root: Option<String>,
    /// `--log-dir` — overrides [`crate::Config::log_dir`].
    pub log_dir: Option<String>,
    /// `--addr` — overrides [`crate::Config::datahub_addr`].
    pub datahub_addr: Option<String>,
    /// `--log-level` — overrides [`crate::Preferences::log_level`].
    pub log_level: Option<String>,
    // ⚠ `--rate-utilization` stood here and is GONE with the preference it overrode. It was this
    // struct's only `f64` flag, and no binary ever parsed it — a CLI override, on a value nothing
    // read, from a surface nothing offered. See `crate::preferences`' module doc.
    /// `--reconcile` — overrides [`crate::Flags::reconcile`].
    pub reconcile: Option<bool>,
}

/// Parse a toggle from an environment variable.
///
/// The workspace idiom is the EXACT string `"1"` (never a fuzzy truthy parse — see CLAUDE.md's
/// `VIKE_RECONCILE` paragraph), and this keeps it, with ONE deliberate change: today an
/// unrecognized value such as `VIKE_RECONCILE=true` reads as FALSE, silently, so an operator who
/// meant to enable reconciliation gets a run that looks normal and reconciles nothing. Here it is
/// an error naming the variable and the value. A typo'd setting that silently does nothing is
/// worse than an error — that is the rule this whole program exists to apply.
pub(crate) fn parse_flag(var: &str, raw: &str) -> Result<bool, ConfigError> {
    match raw {
        "1" => Ok(true),
        "0" => Ok(false),
        other => Err(ConfigError::Env {
            var: var.to_string(),
            value: other.to_string(),
            message: "expected exactly \"1\" (on) or \"0\" (off)".to_string(),
        }),
    }
}

// NOTE there is deliberately no `parse_f64` helper yet. `Policy::market_slippage` is the model's
// only remaining `f64`, and `Policy` has no env layer AT ALL (that is the whole point of the type),
// so a numeric env parser would have no caller — and an unused `pub(crate)` helper is a `dead_code`
// failure under the workspace's `-D warnings` clippy gate. It comes back with the first `f64` knob
// that genuinely reads one. (`Preferences::rate_utilization` was the previous holder of this note
// and has been removed outright — see `preferences`' module doc.)

/// Parse a `usize` from an environment variable.
pub(crate) fn parse_usize(var: &str, raw: &str) -> Result<usize, ConfigError> {
    raw.trim().parse().map_err(|_| ConfigError::Env {
        var: var.to_string(),
        value: raw.to_string(),
        message: "expected a non-negative whole number".to_string(),
    })
}

/// A SET, non-empty variable's value. An empty string is treated as unset, matching every
/// existing reader in the workspace (an exported-but-empty var is how a shell spells "unset").
pub(crate) fn get<'a>(env: &'a HashMap<String, String>, var: &str) -> Option<&'a str> {
    env.get(var).map(|s| s.as_str()).filter(|s| !s.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_flag_accepts_only_the_exact_strings() {
        assert!(parse_flag("VIKE_RECONCILE", "1").unwrap());
        assert!(!parse_flag("VIKE_RECONCILE", "0").unwrap());
        for bad in ["true", "yes", "on", "1 ", "TRUE"] {
            let err = parse_flag("VIKE_RECONCILE", bad).unwrap_err();
            assert!(err.to_string().contains("VIKE_RECONCILE"), "{err}");
            assert!(err.to_string().contains(bad), "{err}");
        }
    }

    // NB every env-shaped string literal anywhere under `crates/` is harvested by `vike-ops`'
    // settings-registry gate and must already carry a `SETTINGS` row — including the ones in
    // tests. Reuse declared names here rather than inventing a placeholder.
    #[test]
    fn a_non_numeric_worker_count_names_the_variable_and_the_value() {
        let err = parse_usize("VIKE_SWEEP_THREADS", "lots").unwrap_err();
        assert_eq!(
            err.to_string(),
            "VIKE_SWEEP_THREADS=lots: expected a non-negative whole number"
        );
    }

    #[test]
    fn an_empty_variable_reads_as_unset() {
        let env = HashMap::from([("A".to_string(), String::new()), ("B".to_string(), "x".into())]);
        assert_eq!(get(&env, "A"), None);
        assert_eq!(get(&env, "B"), Some("x"));
        assert_eq!(get(&env, "C"), None);
    }
}
