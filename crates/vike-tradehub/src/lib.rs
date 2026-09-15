//! `vike-tradehub` — the headless trading DAEMON's library surface (headless two-layer plan, Layer 2).
//!
//! The daemon's testable internals live here so integration tests (`tests/`) — and a future
//! in-process embedder — can reach them; the thin [`main`](../main.rs) bin composes them into the
//! process lifecycle (log init, node build, stdio control, bounded teardown). Splitting a lib out of
//! the bin is what lets `tests/observe_roundtrip.rs` drive the real [`server`] + [`publish`] fan-out
//! (an integration test cannot see a bin crate's private modules).
//!
//! ## What ships here
//! - [`alerts`] — the HEADLESS alerting mount: `vike_ops::alerting`'s `AlertEngine` driven from the
//!   daemon's existing periodic snapshot tick, so rules keep evaluating (and delivering to
//!   log/Telegram/webhook) on an unattended production node. A strict OFF-FOLD consumer, and OFF by
//!   default — the rules file is the gate.
//! - [`config`] (PR-9) — the reviewable TOML daemon PROFILE lowered into a `vike_run::MakerMountConfig`.
//! - [`feeds`] — the feed-wiring surface moved out of `main.rs` alongside `venue_plan`/`venue_arming`
//!   for the same reason: `venue_feed_plan`/`wire_venue_feeds`/`recon_feed_statuses_of` needed to
//!   name [`types::ResolvedMount`] and the feed-object types ([`feeds::LiveFeeds`]/
//!   [`feeds::CexBars`]/[`feeds::CexTicks`]) and the [`feeds::FeedCtors`] construction seam, all of
//!   which moved here with them. `main.rs`'s `live_mount`/`live_mount_with` stay put and call in.
//! - [`hot_reload`] (REQ-7 v2) — the settings hot-apply seam: the per-key HOT-vs-RESTART
//!   classification table (policy is NEVER hot — the sealed-policy doctrine, decision 0005) and
//!   the queue the node server hands an accepted hot write to, drained and executed on the
//!   daemon's existing periodic summary tick — never the fold, never the wire thread.
//! - [`publish`] (PR-11) — the `CoreSnapshot` -> `WireSnapshot` projection and the ONE publisher
//!   thread that reads the arc-swap snapshot cell and fans coalesced frames out to subscribers over
//!   bounded, drop-oldest mailboxes. It NEVER touches the vike-core hot fold.
//! - [`server`] (PR-11/12) — the authenticated node server: the PR-10 node handshake + `NodeKeys`
//!   auth, thread-per-connection, `Subscribe` -> push for `Observe`, and — when the daemon threads a
//!   `vike_core::CommandSink` in (gated by `VIKE_TRADEHUB_CONTROL=1`) — a `Scope::Control` peer's
//!   `Request::Command` lowered into the core's real `Command`/`OrderIntent`.
//! - [`summary`] (I10 rehearsal follow-up) — the stdio summary line's MOUNTED-SET scoping: the
//!   pure, integration-testable half of `main.rs`'s `summary_line`, which is why it lives in the
//!   lib rather than beside the line it feeds.
//! - [`audit`] (PR-12) — the one-line structured audit record every ACCEPTED control command emits to
//!   the vike-log trace file (the durable record for the network order-write path).
//! - [`types`] — the per-venue types [`CexVenue`]/[`VenuePlan`] `main.rs`'s feed dispatch and this
//!   crate's [`venue_plan`] module both name. Moved out of `main.rs` alongside `venue_plan` because
//!   a library crate cannot name a type the binary depending on it defines.
//! - [`venue_plan`] — the per-venue feed-plan CONSTRUCTION functions (`cex_plan`/`alpaca_plan`/…):
//!   prove a mount's `(symbol, mainnet)` pair is wired before yielding a [`VenuePlan`]. Moved out of
//!   `main.rs` so this crate's integration tests can drive it directly (an integration test cannot
//!   see a bin crate's private modules).
//! - `telegram` (behind the OFF-BY-DEFAULT `telegram` Cargo feature) — the opt-in Telegram CONTROL
//!   channel: a SECOND remote write surface (`getUpdates` long-poll, no inbound listener) behind a
//!   mandatory preview + `/confirm` contract, a chat-id allowlist, and four simultaneous gates. It
//!   shares [`server::accept_command`] with the TCP surface, so both are gated identically by
//!   construction. Absent the FEATURE the module is not compiled at all — a remote
//!   order-origination path reachable from a third-party chat service is not something a default
//!   daemon build should merely have dormant, and a runtime env gate can be misconfigured or
//!   inherited from a stale systemd unit while un-compiled code cannot. With the feature on it is
//!   still OFF until all four runtime gates open — absent any one NOTHING is constructed: no
//!   thread, no bot token in memory, no network call.
//!
//! ## Deferred: `vike-app --observe`
//! Driving the GUI panels from a `vike_tradehub_client::RemoteCoreHandle` (a `vike-app --observe`
//! mode) is a separate, LOCAL-VERIFY follow-up — vike-app is compile-checked but never TESTED in CI.
//! PR-11's CI-testable core is the [`server`] + [`publish`] here plus the light `RemoteCoreHandle`
//! client, all exercised by `tests/observe_roundtrip.rs`.

pub mod alerts;
pub mod audit;
pub mod config;
pub mod feeds;
pub mod hot_reload;
pub mod mount_factory;
pub mod publish;
pub mod server;
pub mod summary;
pub mod types;
pub use types::{CexVenue, ResolvedMount, VenuePlan};
pub mod venue_arming;
pub mod venue_plan;
// The daemon CLI as a library function, so the bin and the `vike` multicall dispatcher reach one
// copy. ⚠ Its opening statement order is checked POSITIONALLY by
// `crates/vike-ops/tests/graceful_stop_pin.rs` — see the module doc.
pub mod tradehub_cli;
// ⚠ A REMOTE ORDER-ORIGINATION path reachable from a third-party chat service — compiled ONLY
// under the crate's own off-by-default `telegram` feature (see the module list above). A default
// daemon build does not contain this code, so no runtime misconfiguration can reach it.
#[cfg(feature = "telegram")]
pub mod telegram;
