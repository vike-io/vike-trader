//! What the two command-line programs share over the hub clients, one family per module.
//! `crates/vike-client/CLAUDE.md` says what stays out.

pub mod account_report;
pub mod addressed;
pub mod attach;
pub mod first_frame;
pub mod health;
pub mod hubkeys;
// The Rhai host's indicator rows, behind `indicators`: only `vike-cli` and `vike-mcp` list them.
#[cfg(feature = "indicators")]
pub mod indicators;
pub mod links;
pub mod order_gate;
pub mod profile;
pub mod refusals;
pub mod resolved;
pub mod settings_row;
pub mod verbs;
