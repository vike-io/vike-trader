//! The `vike-cli` subcommand modules. Each exposes a `run(args) -> ExitCode` entry the top-level
//! [`crate::dispatch`] routes to: [`backtest`] (compute-to-data offload) and [`mcp`] (the stdio MCP
//! server exposing the create+backtest tools to an agent), [`sweep`] (a remote parameter-grid
//! search), [`walkforward`] (a remote anchored out-of-sample validation), [`config`] (settings
//! provenance: every setting, its effective value, and WHERE that value came from —
//! settings-unification Phase 0), whose `check` verb lives one file over in [`config_check`]
//! because it answers a different question with a different product — an EXIT CODE a systemd
//! `ExecStartPre=` can refuse a unit with, rather than a disclosure — [`secrets`] (inspect the
//! credential store,
//! `<project>/settings/secrets.env`), [`init`] (scaffold `<project>/user_data` — the
//! user-content directory — with runnable examples in every folder) and [`indicators`] (print the
//! indicator functions a Rhai strategy can call, off the same const the host binds from — the
//! human twin of the MCP `list_indicators` tool). An interactive `repl` is the planned sibling
//! (add a module here + one arm in `dispatch`).
//! [`strategy_status`] is the read-only node question — ask a running vike-tradehub node what it
//! is running (the split-plane B4 wire verb's CLI surface, observe scope only).
//! [`args`] is the shared hand-rolled flag-parsing glue every command's own parser drains;
//! [`verbs`] is the ONE order-write verb vocabulary (args→`WireCommand` construction + the coid
//! mint + the client-side guardrail) shared by the `trade` REPL and the `mcp` write tools, and
//! [`nodekeys`] is the ONE resolver for the vike-tradehub node keys both of those surfaces
//! authenticate with (process env, then the credential store the daemon itself reads).

pub(crate) mod args;
pub mod backtest;
pub mod config;
pub mod config_check;
pub mod indicators;
pub mod init;
pub mod mcp;
pub mod nodekeys;
pub mod secrets;
pub mod strategy_status;
pub mod sweep;
pub mod trade;
pub mod verbs;
pub mod walkforward;
