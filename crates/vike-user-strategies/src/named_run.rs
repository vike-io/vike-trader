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
//! [`resolve`] calls exactly two resolvers, and neither can reach a compiler:
//!
//! * `vike_strategy::strategy_by_name` — that crate's closure holds no Rhai compiler. ⚠ WHAT
//!   ENFORCES THAT CHANGED on 2026-09-23 and this bullet used to name the old enforcement:
//!   `vike-strategy` and `vike-script` declared the SAME layer rank, so
//!   `crates/vike-ops/tests/layer_gate.rs` — which fails a PR whose normal `vike-*` dependency
//!   does not declare a STRICTLY LOWER one — refused the edge. `vike-script` moved to `domain`
//!   (20), its own dependency ceiling, so that edge is PERMITTED by rank today. It is refused
//!   instead by the transitive closure check in
//!   `crates/vike-ops/tests/named_run_closure_gate.rs`, which covers `vike-strategy` because that
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
//! `crates/vike-ops/tests/named_run_closure_gate.rs` — a transitive walk of this crate's NORMAL
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
/// strategy folder may legally be called `rhai` (`crate::codegen`'s `valid_name` admits it), and
/// such a folder would be a compiled-in Rust strategy rather than the script arm, but publishing
/// that NAME on a roster a client picks from would make one word mean two things on one wire.
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
/// `crates/vike-ops/tests/named_run_closure_gate.rs` refuses.
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
mod tests {
    use super::*;
    use vike_model::{Bar, Broker};

    /// A ~35-line `HftBroker` double. Copied from `tests/pipeline.rs`'s for the reason that file's
    /// own comment gives: `vike_model::MockBroker` implements only `Broker`, not the tagged-verb
    /// extension the portable registry's bound requires, and this crate declares no dev-dependency
    /// that could carry a shared one (the manifest's dependency set IS the user-strategy API
    /// surface, and widening it for a test double would widen that surface).
    #[derive(Default)]
    struct TestBroker;

    impl Broker for TestBroker {
        fn submit_market(&mut self, _symbol: &str, _side: i32, _qty: f64) {}
        fn submit_limit(&mut self, _symbol: &str, _side: i32, _qty: f64, _price: f64) {}
        fn position(&self, _symbol: &str) -> f64 {
            0.0
        }
        fn price(&self, _symbol: &str) -> f64 {
            0.0
        }
        fn equity(&self) -> f64 {
            0.0
        }
        fn bars(&self, _symbol: &str) -> &[Bar] {
            &[]
        }
        fn index(&self) -> usize {
            0
        }
        fn now(&self) -> i64 {
            0
        }
    }

    impl HftBroker for TestBroker {
        fn position(&self) -> f64 {
            0.0
        }
        fn submit_limit_tagged(&mut self, _tag: &str, _side: i32, _qty: f64, _price: f64) {}
        fn modify_tagged(&mut self, _tag: &str, _new_qty: Option<f64>, _new_price: Option<f64>) {}
        fn cancel_tagged(&mut self, _tag: &str) {}
    }

    /// **THE FENCE, stated as a test.** The script path is not resolvable here, and — the half that
    /// matters — it is not resolvable here WITH a `src` param either, which is exactly the shape
    /// `vike_backtest::harness::registry`'s `"rhai"` arm accepts one line away from this closure.
    ///
    /// ⚠ This test is evidence, not the fence. The fence is the DEPENDENCY CLOSURE
    /// (`crates/vike-ops/tests/named_run_closure_gate.rs`): a test proves what today's code does, a
    /// closure proves what tomorrow's can.
    #[test]
    fn a_param_cannot_reach_a_compiler_through_the_named_run_resolver() {
        let with_src: Value = toml::from_str("src = \"fn on_bar() { buy(1.0); }\"\nqty = 2.0\n")
            .expect("fixture params parse");
        // The SAME (name, params) pair `rhai_resolves_through_the_registry_with_inline_src` proves
        // DOES compile in `vike-backtest`. Here it is unknown, and the params are unread.
        match resolve::<TestBroker>("rhai", &with_src) {
            Err(NamedRunError::Unknown(n)) => assert_eq!(n, "rhai"),
            Err(other) => panic!("the script path must not resolve here, got {other:?}"),
            Ok(_) => panic!("the script path RESOLVED in a closure that holds no compiler"),
        }
        // …and a `src` riding along with a name that DOES resolve changes nothing about the run: it
        // is unread, the way any unrecognised key is unread.
        let mut params = with_src.clone();
        if let Some(t) = params.as_table_mut() {
            t.insert("size".to_string(), Value::Float(1.0));
        }
        assert!(
            resolve::<TestBroker>("buy_hold", &params).is_ok(),
            "a `src` key must be UNREAD rather than fatal — it is not a checked field here"
        );
    }

    /// Every name the roster advertises actually resolves WITH EMPTY PARAMS — the property
    /// `vike_backtest::harness::registry`'s `registry_lists_every_match_arm` holds for its own
    /// roster, and the one that makes a named-only verb possible at all.
    #[test]
    fn every_rostered_name_resolves_with_default_params() {
        let empty = Value::Table(Default::default());
        for name in roster() {
            assert!(
                resolve::<TestBroker>(name, &empty).is_ok(),
                "{name} is on the named-run roster but does not resolve"
            );
        }
    }

    /// …and the other direction: the roster carries no SCRIPT_ONLY name, and no simulator-only one
    /// either. The second half is the DECLARED COST of 0064's decision 2, pinned so that admitting
    /// one of those arms reddens a test that names the record rather than passing silently.
    #[test]
    fn the_roster_excludes_the_script_arm_and_the_deferred_simulator_arms() {
        let names = roster();
        for (script, _) in vike_strategy::SCRIPT_ONLY {
            assert!(!names.contains(script), "{script} must not be on the named-run roster");
        }
        for (sim, why) in vike_strategy::SIMULATOR_ONLY {
            assert!(
                !names.contains(sim),
                "{sim} is DEFERRED from the named-run roster by \
                 docs/decisions/0064-a-named-run-carries-no-source.md's decision 2 — it lives in \
                 vike-backtest, which CAN name vike-script, so it is outside the compiler-free \
                 closure. Its own reason for living there: {why}. Admitting it is a REOPENER of \
                 0064; the route that reopens nothing is moving the arm below vike-script's layer \
                 rank."
            );
        }
    }

    /// The roster is free of duplicates and non-empty — a duplicate would be a picker showing one
    /// strategy twice, and an empty roster would make every named run a refusal nobody could act on.
    #[test]
    fn the_roster_is_non_empty_and_free_of_duplicates() {
        let names = roster();
        assert!(!names.is_empty(), "the named-run roster must never be empty");
        let mut sorted = names.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), names.len(), "duplicate on the named-run roster: {names:?}");
    }

    /// An ordinary typo is `Unknown` and carries the name back, so the server can answer with its
    /// roster rather than with a shrug.
    #[test]
    fn an_unknown_name_names_itself() {
        let empty = Value::Table(Default::default());
        // ⚠ `err()` rather than a `match` over the whole `Result`: the `Ok` arm is a
        // `Box<dyn Strategy<..> + Send>`, which implements no `Debug`, so a panic message naming
        // the whole result does not compile.
        match resolve::<TestBroker>("nope", &empty).err() {
            Some(NamedRunError::Unknown(n)) => assert_eq!(n, "nope"),
            other => panic!("expected Unknown, got {other:?}"),
        }
    }
}
