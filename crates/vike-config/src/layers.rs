//! The per-type override POWER, expressed as a trait — the load contract's teeth.
//!
//! The precedence order is one line and applies to everything:
//!
//! ```text
//! code defaults -> the settings database's rows -> CLI
//! ```
//!
//! What differs per type is not the ORDER, it is WHICH LAYERS EXIST:
//!
//! | type              | defaults | database | CLI |
//! |-------------------|----------|----------|-----|
//! | [`crate::Policy`] | yes      | yes      | NO  |
//! | [`crate::Config`] | yes      | yes      | yes |
//! | [`crate::Preferences`] | yes | yes      | yes |
//! | [`crate::Flags`]  | yes      | yes      | yes |
//!
//! **No type has an environment layer**
//! (`docs/decisions/0111-no-setting-lives-in-the-environment-or-a-toml-file.md`): a setting is a
//! row, and a variable that used to set one is refused at startup by [`crate::refuse_removed_env`],
//! naming the row that replaced it. The `EnvOverride` trait that carried the environment layer is
//! DELETED with it; the name stays in this sentence because records cite it here.
//!
//! **The CLI half of that table is enforced HERE, by the type system, not by a comment.**
//! `Config`, `Preferences` and `Flags` implement [`CliOverride`]; `Policy` does NOT, and has no
//! `apply_cli` inherent method either. So `settings.policy.apply_cli(&cli)` is not a discouraged
//! call — it is a **compile error**, because there is no such method to resolve, on the type or on
//! any trait the type implements.
//!
//! Why that matters enough to spend a trait on: **a ceiling you can override from outside the
//! settings database is not a ceiling.** Every other guard in this workspace can be argued about;
//! this one cannot, because the failure mode is silent. A ceiling raised by a flag in a systemd
//! unit, a CI job or a stale shell would change with no row written, no review, and no diff — and
//! the run would look exactly like a compliant one. Making the override unrepresentable removes the
//! whole class instead of documenting it.
//!
//! The trait is **sealed** (by a crate-private `sealed::Sealed` supertrait): a downstream crate
//! cannot bolt an `impl CliOverride for Policy` on from outside. Inside this crate it would take
//! deliberately adding `Policy` to the `Sealed` impl list below — one line, in a file whose entire
//! purpose is this rule, visible in any diff. That is the strongest form available in Rust short of
//! a newtype-per-layer, and it is the reason the seal exists rather than a bare public trait.

use crate::error::ConfigError;

/// Seals [`CliOverride`] so the per-type table above cannot be extended from outside this crate.
///
/// ⚠ [`crate::Policy`] is deliberately ABSENT from the impl list. Adding it here is the single
/// edit that would let a ceiling become overridable from the command line — do not make it without
/// re-reading this module's doc.
pub(crate) mod sealed {
    /// Implemented by exactly the three types that may be overridden past the database layer.
    pub trait Sealed {}
    impl Sealed for crate::config::Config {}
    impl Sealed for crate::preferences::Preferences {}
    impl Sealed for crate::flags::Flags {}
}

/// A settings type a COMMAND-LINE flag may override — the last and highest layer. Sealed.
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
    // ⚠ `--rate-utilization` is DELETED with the preference it overrode. See
    // `crate::preferences`' module doc.
    /// `--reconcile` — overrides [`crate::Flags::reconcile`].
    pub reconcile: Option<bool>,
}
