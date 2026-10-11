//! **The COMPILER-FREE resolver a NAMED RUN goes through — the FENCE, not a filter.**
//!
//! `docs/decisions/0064-a-named-run-carries-no-source.md`'s decision 2 is the whole of this module,
//! and its first paragraph says why a membership test would not have done:
//!
//! > `vike_backtest::harness::registry::strategy_by_name` is a `&str` lookup with no allowlist of
//! > its own. […] A verb that carries a NAME and forwards it to that function compiles Rhai the
//! > moment the name is `"rhai"` and a `src` param rides along. A membership test in front of the
//! > call would prevent that — and it is a check a refactor can move, a future arm can sidestep and
//! > a reviewer can pass over.
//!
//! > **A named run resolves through a function whose crate cannot name `vike-script`.** Not "does
//! > not today" — cannot.
//!
//! # Why this crate, and what makes the claim machine-checked
//!
//! [`resolve`](crate::named_run::resolve) calls exactly two resolvers, and neither can reach a compiler:
//!
//! * `vike_strategy::strategy_by_name` — that crate's closure holds no Rhai compiler. ⚠ WHAT
//!   ENFORCES THAT CHANGED on 2026-09-23 and this bullet used to name the old enforcement:
//!   `vike-strategy` and `vike-script` declared the SAME layer rank, so
//!   `crates/vike-ops/tests/architecture/layer_gate.rs` — which fails a PR whose normal `vike-*` dependency
//!   does not declare a STRICTLY LOWER one — refused the edge. `vike-script` moved to `domain`
//!   (20), its own dependency ceiling, so that edge is PERMITTED by rank today. It is refused
//!   instead by the transitive closure check in
//!   `crates/vike-ops/tests/architecture/named_run_closure_gate.rs`, which covers `vike-strategy` because that
//!   crate is inside the closure it walks. Same force, and a gate written for the purpose rather
//!   than a rank coincidence.
//! * `crate::user_strategy_by_name` — the BUILD-TIME generated registry over the operator's own
//!   `user_data/strategies/rust/`. This crate's whole dependency set is vike-model, vike-strategy,
//!   vike-indicators and toml, which is also the user-strategy API surface (see the crate doc).
//!
//! Under that closure a params key called `src` is **UNREAD, not refused** — the same way any
//! unrecognised key in a params table is unread — because nothing in the closure holds a reader for
//! it. 0064 puts it plainly: *refusing a key is a check, having no reader is a fact.*
//!
//! ⚠ **One of those two legs was NOT gated when 0064 was written, and the record says so**: this
//! crate sits ABOVE `vike-script`'s layer rank, so the layer gate would PERMIT the edge, and its
//! compiler-freedom was a manifest fact rather than a machine-checked one. That is the gate the
//! record declares it owes, and it is
//! `crates/vike-ops/tests/architecture/named_run_closure_gate.rs` — a transitive walk of this crate's NORMAL
//! dependency closure that fails if `vike-script` (or any crate that names it) appears in it.
//!
//! # What this fence COSTS, declared rather than buried
//!
//! The seven arms `vike_backtest::harness::registry::strategy_by_name`'s own `match` holds — the
//! five simulator-bound reference strategies plus `cheap_catch_updown_fair_value` and
//! `sport_copy_follower` — sit in `vike-backtest`, which CAN name `vike-script`; they are one
//! `match` line away from the `"rhai"` arm. **They are NOT on the named-run roster**, and that is a
//! real loss accepted for the reason 0062's decision 3 refused a credentialed venue BY
//! CONSTRUCTION: the hazard is remote code execution, which no rate limit and no ceiling bounds.
//! `vike_strategy::SIMULATOR_ONLY` is the table naming each of them and why it is where it is, so
//! the deferral is a NAMED ROW rather than a gap — the shape the bridge conformance harness uses
//! for a deferred venue.
//!
//! **Admitting one of them is a REOPENER of 0064, not a configuration change**, and the honest
//! route is to move the arm BELOW `vike-script`'s layer rank, which is mechanical and carries none
//! of this record's hazards.

use toml::Value;
use vike_model::{HftBroker, Strategy};

/// Why a named run could not resolve a strategy — the two facts a caller renders differently.
///
/// Deliberately NOT a string: the server turns [`Self::Unknown`] into a
/// `vike_datahub_client::named_run::NamedRunRefusal::UnknownStrategy` carrying the roster, which a
/// picker corrects itself from, while [`Self::BadParams`] is a message about the request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NamedRunError {
    /// The name is on neither roster [`resolve`] consults.
    ///
    /// ⚠ **`"rhai"` lands here, and NOT because anything checked for it.** Neither resolver in the
    /// closure has an arm for that name — the arm that does lives in `vike-backtest` beside the
    /// compiler — so it is unknown in the ordinary way a typo is unknown.
    Unknown(String),
    /// The name resolved and its params are unusable — `vike_strategy::RegistryError::BadParams`,
    /// raised today by the maker arms when a `[strategy.params]` enum key names no variant. The
    /// message is that reader's own, which already names the key and the accepted spellings.
    BadParams(String),
}

impl std::fmt::Display for NamedRunError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            NamedRunError::Unknown(name) => {
                write!(f, "unknown strategy {name:?} on the named-run roster")
            }
            NamedRunError::BadParams(msg) => write!(f, "{msg}"),
        }
    }
}

impl std::error::Error for NamedRunError {}

/// **The roster a named run serves** — `vike_strategy::PORTABLE_STRATEGIES` then this build's own
/// [`USER_STRATEGIES`](crate::USER_STRATEGIES), with every `vike_strategy::SCRIPT_ONLY` name
/// subtracted.
///
/// DERIVED, never written down: 0064's decision 2 says the roster is those two tables, and every
/// prose copy of a roster in this workspace has rotted. The subtraction is not decoration — a user
/// strategy folder may legally be called `rhai` (`vike_model::host_build::scan::valid_name` admits
/// it), and such a folder would be a compiled-in Rust strategy rather than the script arm, but
/// publishing that NAME on a roster a client picks from would make one word mean two things on one
/// wire.
///
/// ⚠ The order is PORTABLE first, then user — the same "built-in arms are tried first, so a user
/// folder can never shadow a built-in name" ordering [`resolve`] applies, made visible. A duplicate
/// is dropped rather than listed twice.
pub fn roster() -> Vec<&'static str> {
    let script_only = |n: &str| vike_strategy::SCRIPT_ONLY.iter().any(|(s, _)| *s == n);
    let mut out: Vec<&'static str> =
        vike_strategy::PORTABLE_STRATEGIES.iter().copied().filter(|n| !script_only(n)).collect();
    for name in crate::USER_STRATEGIES {
        if !script_only(name) && !out.contains(name) {
            out.push(name);
        }
    }
    out
}

/// **Resolve a named run's strategy — the one call a named run makes, and the fence itself.**
///
/// Built-ins first, then this build's user registry, exactly as
/// `vike_backtest::harness::registry::strategy_by_name`'s fall-through does, so a user folder can
/// never shadow a built-in name. What is DIFFERENT — and is the whole point — is that this function
/// has no `match` of its own above those two calls, so there is no arm here for a future author to
/// add a compiler to without first adding a dependency that
/// `crates/vike-ops/tests/architecture/named_run_closure_gate.rs` refuses.
///
/// `params` is a TOML table the caller built from
/// `vike_datahub_client::named_run::NamedParam` values, which carry no strings — see that enum. A
/// `src` key would be unread here even if one arrived.
pub fn resolve<B: HftBroker + 'static>(
    name: &str,
    params: &Value,
) -> Result<Box<dyn Strategy<B> + Send>, NamedRunError> {
    if vike_strategy::SCRIPT_ONLY.iter().any(|(s, _)| *s == name) {
        // A BELT, and labelled one: `"rhai"` resolves in NEITHER call below, so this arm changes no
        // outcome — it only makes the refusal say the true thing instead of "typo". Without it an
        // operator who asked for the script path is told the name does not exist, which is the
        // rejection-class confusion `vike_strategy::SCRIPT_ONLY`'s own doc exists to fix.
        return Err(NamedRunError::Unknown(name.to_string()));
    }
    match vike_strategy::strategy_by_name::<B>(name, params) {
        Ok(boxed) => Ok(boxed),
        Err(vike_strategy::RegistryError::Unknown(n)) => {
            crate::user_strategy_by_name::<B>(&n, params).ok_or(NamedRunError::Unknown(n))
        }
        Err(vike_strategy::RegistryError::BadParams(msg)) => Err(NamedRunError::BadParams(msg)),
    }
}

#[cfg(test)]
mod tests;
