//! `control` — THE external control boundary of the live core, named in ONE place.
//!
//! # The contract
//! External control = [`vike_exec::Command`]/[`vike_exec::OrderIntent`] IN (via
//! [`CoreHandle::try_command`](crate::CoreHandle::try_command) — non-blocking, the GUI's path; or
//! [`CoreHandle::send_command`](crate::CoreHandle::send_command) —
//! lossless, session/tests),
//! [`vike_exec::CoreSnapshot`] queries OUT (its accessor methods: `orders_for`/`order`/
//! `open_order`/`position`/`last_mark`/`equity`/`trading_state`; the type is vike-exec's read
//! model, this crate builds it). GUI, CLI, and MCP are TRANSLATORS over this seam; nothing
//! bypasses it. Every order-scoped verb is a [`vike_exec::OrderIntent`]; every intent from
//! every origin lowers through the core's single `apply_intent` site — mint → RiskGate → client —
//! so the risk gate is STRUCTURAL on all paths, not a convention.
//!
//! # Fire-and-forget
//! Commands have no synchronous reply (single-writer fold; a synchronous order handle was
//! rejected). A caller that must know its client-order-id PRE-MINTS it (a non-empty
//! `client_order_id` is respected; empty ⇒ the core mints); outcomes are observed via the snapshot
//! ([`vike_exec::CoreSnapshot`]'s `rejected_commands` counts queue rejections; `OrderDenied`
//! events surface RiskGate vetoes in `recent_events`).
//!
//! # Building adapter #2 (CLI / MCP)
//! In-process (a `--headless` mode / REPL inside the running core binary) holds the
//! [`CoreHandle`](crate::CoreHandle) and snapshot cell directly — an afternoon, zero transport. Recommended first cut: newline-JSON
//! over stdio (the dukascopy sidecar shape) — `Command` is already `serde`, so a command ships as
//! `to_string`→write→read→`from_str`→`send_command`. Out-of-process (a detached `vike` binary vs a
//! running daemon) additionally needs a transport (socket/pipe) + `Serialize` on the snapshot view
//! types — the parked RPC layer, added when a real consumer exists, built ON this seam.
//!
//! The out-of-process WRITE half now exists: [`CommandSink`](crate::CommandSink) (from
//! [`CoreHandle::command_sink`](crate::CoreHandle::command_sink)) is the narrow, cloneable, command-only capability the headless-node control server (`vike-tradehub`)
//! holds. It lowers a remote `Scope::Write` peer's order command into the SAME `apply_intent` seam,
//! carrying ONLY the ingest lane (no snapshot read, no shutdown reach), so mint → RiskGate → client
//! stays structural on the network write path too.

// ⚠ `pub use vike_exec::{Command, ConditionalIntent, OrderIntent};` stood here until 2026-10-10: a
// second name for vike-exec's command-lane types (the lane is vike-exec's so a venue bridge never
// links this crate). Every caller names `vike_exec::X`; this module names the boundary in prose.
// ⚠ So did `pub use crate::runtime::{CommandRejected, CommandSink, CoreHandle};`, a THIRD path to
// this crate's own types beside the crate root's `vike_core::{…}` (which every caller names) and
// `vike_core::runtime::…`; no caller named it. `crates/vike-core/tests/wiring/control_boundary.rs`'s
// `no_own_symbol_gets_a_second_public_path_below_the_root` refuses its return.
