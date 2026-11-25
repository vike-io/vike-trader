//! The refusals the strategy sandbox itself makes, declared once (`vike_utility::refusal_enum!`, decision 0301): the
//! jail refused, or a loaded live file built no strategy. The loader's own refusals are
//! `vike_rust_strategy::loader::LoadError`'s.
//!
//! Each leaves this process in its BARE words (`Display`, or `carried()` with its id beside them; never `rendered()`):
//! the backtest's and the study's answers carry it as the strategy's refusal (`carried()`, so its id shows), the
//! live verb answers the tradehub's `Open` with it and says it on stderr, the
//! reason tail the tradehub and the deploy's pre-flight re-report. The surface that shows it adds the one id
//! (`vike_utility::errors`'s module doc). Decide on the variant, never on its words.

vike_utility::refusal_enum! {
    /// A refusal of the strategy sandbox: this module's doc.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum SandboxRefusal with ids SandboxRefusalId {
        /// The jail could not be applied (`vike_sandbox::StrategyProcessPolicy::apply`: not Linux, a kernel without
        /// Landlock ABI 4, a unit without `@sandbox`, an artifact it refuses), so nothing loads; `why` is its reason.
        Unjailed { why: String } => "strategy.sandbox.unjailed": "no jail, so no strategy: {why}",
        /// A live mount's file loaded and did not create a strategy from the `Open`'s params and mount facts.
        NotCreated { file: String } => "strategy.sandbox.not_created":
            "`{file}` did not build from its params and mount facts (a params document that does \
             not parse, a build that panicked, or a rule file handed no symbol)",
    }
}
