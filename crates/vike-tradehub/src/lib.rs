//! `vike-tradehub` — the headless trading DAEMON's library surface (headless two-layer plan, Layer 2).
//!
//! The daemon's testable internals live here so integration tests (`tests/`) — and a future
//! in-process embedder — can reach them; the thin [`main`](../main.rs) bin composes them into the
//! process lifecycle (log init, runtime assembly, stdio control, bounded teardown). Splitting a lib out of
//! the bin is what lets `tests/daemon/observe_roundtrip.rs` drive the real [`server`] + [`publish`] fan-out
//! (an integration test cannot see a bin crate's private modules).
//!
//! ## What ships here
//! - [`alerts`] — the HEADLESS alerting mount: `vike_alerting`'s `AlertEngine` driven from the
//!   daemon's existing periodic snapshot tick, so rules keep evaluating (and delivering to
//!   log/Telegram/webhook) on an unattended production node. A strict OFF-FOLD consumer, and OFF by
//!   default — the rules file is the gate.
//! - [`config`] (PR-9) — the reviewable TOML daemon PROFILE lowered into a `vike_tradehub_core::MakerConfig`.
//! - [`feeds`] — the feed-wiring surface moved out of the old single-file daemon alongside `venue_plan`/`venue_arming`
//!   for the same reason: `venue_feed_plan`/`wire_venue_feeds`/`recon_feed_statuses_of` needed to
//!   name [`types::ResolvedAdapter`] and the feed-object types ([`feeds::LiveFeeds`]/
//!   [`feeds::CexFeed`]/[`feeds::Tracked`]) and the [`feeds::FeedCtors`] construction seam, all of
//!   which moved here with them. `tradehub_cli/live_runtime.rs`'s `live_runtime`/`live_runtime_with` call in (the bin `main.rs` is a 19-line shim).
//! - [`hot_reload`] (REQ-7 v2) — the settings hot-apply seam: the per-key HOT-vs-RESTART
//!   classification table (policy is NEVER hot — the sealed-policy doctrine, decision 0005) and
//!   the queue the node server hands an accepted hot write to, drained and executed on the
//!   daemon's existing periodic summary tick — never the fold, never the wire thread.
//! - `node` (private) — the node server's STARTUP, moved out of `tradehub_cli.rs` with the `run`
//!   phase split: the bind classification, the hub-key store read, the control gate, the
//!   account-admin barrier and the publisher plus accept thread. Crate-private because its one
//!   caller is `tradehub_cli::run`.
//! - [`publish`] (PR-11) — the `CoreSnapshot` -> `WireSnapshot` projection and the ONE publisher
//!   thread that reads the arc-swap snapshot cell and fans coalesced frames out to subscribers over
//!   bounded, drop-oldest mailboxes. It NEVER touches the vike-core hot fold.
//! - [`server`] (PR-11/12) — the authenticated node server: the PR-10 node handshake + `HubKeys`
//!   auth, thread-per-connection, `Subscribe` -> push for `Observe`, and — when the daemon threads a
//!   `vike_core::CommandSink` in (gated by the `flags.tradehub_control` row) — a `Scope::Write` peer's
//!   `Request::Command` lowered into the core's real `Command`/`OrderIntent`.
//! - [`summary`] (I10 rehearsal follow-up) — the stdio summary line (`summary_line`) and its
//!   MOUNTED-SET scoping: the pure, integration-testable half, which is why it lives in the lib
//!   rather than in the binary's own file.
//! - [`audit`] (PR-12) — the one-line structured audit record every ACCEPTED control command emits to
//!   the vike-log trace file (the durable record for the network order-write path).
//! - [`types`] — the per-venue types [`CexVenue`]/[`VenuePlan`] `feeds.rs`'s feed dispatch and this
//!   crate's [`venue_plan`] module both name. Moved out of the old single-file daemon alongside `venue_plan` because
//!   a library crate cannot name a type the binary depending on it defines.
//! - [`venue_plan`] — the per-venue feed-plan CONSTRUCTION functions (`cex_plan`/`alpaca_plan`/…):
//!   prove a mount's `(symbol, mainnet)` pair is wired before yielding a [`VenuePlan`]. Moved out of
//!   the old single-file daemon so this crate's integration tests can drive it directly (an integration test cannot
//!   see a bin crate's private modules).
//! - `telegram` (behind the OFF-BY-DEFAULT `telegram` Cargo feature) — the opt-in Telegram CONTROL
//!   channel: a SECOND remote write surface (`getUpdates` long-poll, no inbound listener) behind a
//!   mandatory preview + `/confirm` contract, a chat-id allowlist, and four simultaneous gates. It
//!   shares [`server::control::accept_command`] with the TCP surface, so both are gated identically by
//!   construction. Absent the FEATURE the module is not compiled at all — a remote
//!   order-origination path reachable from a third-party chat service is not something a default
//!   daemon build should merely have dormant, and a runtime env gate can be misconfigured or
//!   inherited from a stale systemd unit while un-compiled code cannot. With the feature on it is
//!   still OFF until all four runtime gates open — absent any one NOTHING is constructed: no
//!   thread, no bot token in memory, no network call.
//!
//! ## The GUI's observe mode (this heading read "Deferred: `vike-app --observe`" until 2026-09-28)
//! Driving the GUI panels from a `vike_tradehub_client::RemoteCoreHandle` was deferred here as a
//! LOCAL-VERIFY follow-up; it landed, and since the desktop lost its local core every
//! `vike-desktop` launch observes (`vike_app_core::backend::observe_bridge`). PR-11's CI-testable
//! core is the [`server`] + [`publish`] here plus the light `RemoteCoreHandle` client, all
//! exercised by `tests/daemon/observe_roundtrip.rs`.

#![warn(unreachable_pub)]

pub mod alerts;
pub mod audit;
pub mod config;
mod feed_gate;
pub mod feeds;
pub mod hot_reload;
mod hub_server;
// The late arm (decision 0164, D7; H1): a tradehub that started while the gatewayhub was down arms
// its demo/live accounts when the gatewayhub comes up, by one bounded graceful restart.
pub mod late_arm;
pub mod mount_factory;
pub mod profile_rows;
// The reconcile ENGINE's env-driven configuration and its arming gate. ⚠ This was
// `vike_tradehub::reconcile_config` until 2026-09-23, on the stated ground that the gate is "called
// once by each live root so the GUI and the daemon cannot answer differently". There is ONE live
// root now — the desktop lost its local core (#1610) and names this module zero times — so the
// module that was shared is shared with nobody, and it lives with its only caller.
pub mod publish;
pub mod reconcile_config;
// THE VENUE PORT OVER THE GATEWAYHUB (wave 2, W11; decision 0164): the `VenueBackend` the live mount
// composes, asking the daemon that owns every venue connection for the arming table, the preflight
// legs and each adapter start. Its module doc is the contract.
pub mod remote_backend;
pub mod self_address;
pub mod server;
pub mod summary;
pub mod types;
pub use types::{CexVenue, HlNetwork, ResolvedAdapter, VenuePlan};
pub mod venue_arming;
pub mod venue_plan;
// The daemon CLI as a library function, so the bin and the `vike-backend` multicall dispatcher reach
// one copy. ⚠ Its opening statement order is checked POSITIONALLY by
// `crates/vike-ops/tests/container_deploy/graceful_stop_pin.rs` — see the module doc.
pub mod tradehub_cli;
// ⚠ A COMMAND, in a daemon's crate, that starts no daemon — `vike-backend catalog`, the instrument
// listing `docs/decisions/0066`'s decision 9 armed. It asks the gatewayhub (the one process that links
// the venue bridges, decision 0164) over its data wire and reads no venue credential; it lives here
// because `vike-cli` is refused by `crates/vike-boot/tests/dependency_floor.rs`, which holds that binary
// transport-free. It mounts nothing, spawns nothing and can place no order — its own module doc states
// that at the top.
pub mod catalog_cli;
// `vike-backend venues` — the read-only report of WHICH venues and accounts this box would mount
// and WHY the rest stay on paper. Its load-bearing column is the gatewayhub's arming table (one
// `ArmingQuery` over the trade wire), rendered through `vike_tradehub_core::venue_arming`: each exchange's
// tier rule is its own bridge's, and the bridges are linked only by the gatewayhub. `vike-cli` cannot host
// it — the same `dependency_floor` gate that refuses `catalog` there holds that binary transport-free.
// Like `catalog` it mounts nothing, places no order and dials only the gatewayhub.
pub mod venues_cli;

// The credential fields that arm each roster venue, shared by the two arming parity goldens
// (the tradehub's `venues` golden, `src/venues_cli_parity_tests.rs`, uses it; the gatewayhub keeps its own twin).
#[cfg(test)]
mod arming_fixtures_tests;
#[cfg(test)]
mod scripted_arming_tests;
// `vike-backend notify <unit>` — the systemd `OnFailure=` pager, reading its two Telegram keys from
// the settings database. Here for the same reason as the two commands above: `vike-cli` is held
// transport-free and an HTTPS POST is a transport, while this crate already links `vike-alerting`,
// `vike-secrets` and `vike-boot`. It mounts nothing, opens no listening socket and can place no
// order — its module doc states that at the top.
pub mod notify_cli;
// ⚠ A REMOTE ORDER-ORIGINATION path reachable from a third-party chat service — compiled ONLY
// under the crate's own off-by-default `telegram` feature (see the module list above). A default
// daemon build does not contain this code, so no runtime misconfiguration can reach it.
#[cfg(feature = "telegram")]
pub mod telegram;
