//! In-app AI copilot (Studio SP3 Part B): the `LlmClient` seam + Anthropic/Cerebras clients over
//! blocking `ureq` + the `develop_strategy` agentic loop (generate → compile-validate → OOS
//! backtest → repair) and its N-candidate batch twin `develop_strategies`, which deflates each
//! accepted candidate's OOS Sharpe against the trial set (Bailey & López de Prado's deflated
//! Sharpe ratio, `vike_backtest::overfit::deflated_sharpe_ratio` — see `agent`'s module doc).
//! Headless; the ChatPane in `vike-studio` drives it.
//!
//! The loop also has a MEMORY: [`ledger`] persists every trial it ran (and every note the model
//! chose to record) as two best-effort JSON files under a CALLER-SUPPLIED directory. That buys
//! prompt grounding (stop rediscovering the same duds) and — the statistically real win —
//! cross-session deflation: the multiple-testing correction finally counts every trial run against
//! a slice, not just the ones that shared a process. `develop_strategy` stays ledger-less and
//! byte-identical; [`develop_strategy_with_ledger`] is the twin that remembers.

pub mod agent;
pub mod anthropic;
pub mod cerebras;
pub mod client;
pub mod ledger;
pub mod provider;

pub use agent::{
    develop_strategies, develop_strategies_with_ledger, develop_strategy,
    develop_strategy_with_ledger, AgentResult,
};
pub use client::{LlmClient, LlmError, ToolCall, ToolSpec};
pub use ledger::{
    Learning, LearningScope, LearningStore, LedgerPaths, Trial, TrialLedger, LEARNINGS_FILE,
    TRIALS_FILE,
};
pub use provider::{make_client, Provider};
