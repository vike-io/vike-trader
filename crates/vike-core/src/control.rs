//! `control` — THE external control boundary of the live core, named in ONE place.
//!
//! # The contract
//! External control = [`Command`]/[`OrderIntent`] IN (via [`CoreHandle::try_command`] —
//! non-blocking, the GUI's path; or [`CoreHandle::send_command`] — lossless, session/tests),
//! [`CoreSnapshot`] queries OUT (its accessor methods: `orders_for`/`order`/`open_order`/
//! `position`/`last_mark`/`equity`/`trading_state`). GUI, CLI, and MCP are TRANSLATORS over this
//! seam; nothing bypasses it. Every order-scoped verb is an [`OrderIntent`]; every intent from
//! every origin lowers through the core's single `apply_intent` site — mint → RiskGate → client —
//! so the risk gate is STRUCTURAL on all paths, not a convention.
//!
//! # Fire-and-forget
//! Commands have no synchronous reply (single-writer fold; a synchronous order handle was
//! rejected). A caller that must know its client-order-id PRE-MINTS it (a non-empty
//! `client_order_id` is respected; empty ⇒ the core mints); outcomes are observed via the snapshot
//! ([`CoreSnapshot`]'s `rejected_commands` counts queue rejections; `OrderDenied` events surface
//! RiskGate vetoes in `recent_events`).
//!
//! # Building adapter #2 (CLI / MCP)
//! In-process (a `--headless` mode / REPL inside the running core binary) holds the [`CoreHandle`]
//! and snapshot cell directly — an afternoon, zero transport. Recommended first cut: newline-JSON
//! over stdio (the dukascopy sidecar shape) — `Command` is already `serde`, so a command ships as
//! `to_string`→write→read→`from_str`→`send_command`. Out-of-process (a detached `vike` binary vs a
//! running daemon) additionally needs a transport (socket/pipe) + `Serialize` on the snapshot view
//! types — the parked RPC layer, added when a real consumer exists, built ON this seam.
//!
//! The out-of-process WRITE half now exists: [`CommandSink`] (from [`CoreHandle::command_sink`]) is
//! the narrow, cloneable, command-only capability the headless-node control server (`vike-tradehub`)
//! holds. It lowers a remote `Scope::Control` peer's order command into the SAME `apply_intent` seam,
//! carrying ONLY the ingest lane (no snapshot read, no shutdown reach), so mint → RiskGate → client
//! stays structural on the network write path too.

pub use crate::runtime::{CommandRejected, CommandSink, CoreHandle};
pub use crate::snapshot::{CoreSnapshot, OrderView, PositionView, VenueBlock};
pub use vike_exec::{Command, ConditionalIntent, OrderIntent};
