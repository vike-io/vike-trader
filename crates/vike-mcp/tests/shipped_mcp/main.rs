//! `shipped_mcp`: every integration suite of this crate, one test binary. The suites drive the SHIPPED
//! `vike-mcp` over its real stdio, or `serve` over a pipe. The grouping rule is
//! `crates/vike-backtest/CLAUDE.md`'s "Test-binary consolidation".

mod argv_accepted;
mod common;
mod mcp_transcript;
mod mcp_unattended_cli;
mod mcp_wire_snapshot;
#[path = "../../src/tests/write_args.rs"]
mod write_args;
