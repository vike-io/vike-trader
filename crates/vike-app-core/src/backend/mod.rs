//! Connections to backend daemons: the registry, the live connection and its identity, the observe
//! bridge, the control-wire lowering, the split-plane routing.

pub mod backend_conn;
pub mod backend_editor;
// How a backend is IDENTIFIED on screen — the NAME leads, the address is secondary detail. One
// authority, so the strip, the picker rows, the status line and the window title cannot disagree.
pub mod backend_identity;
pub mod backend_registry;
pub mod observe_bridge;
// The pure ARM decisions behind `App::new` (split-plane B2): the three-mode table, the ONE copy
// of the mounted feed-venue set, and the double-fold guard deciding each series' single render
// source. The shell's `main.rs` (`vike-desktop`; `vike-app` when this was written) keeps the
// wiring; the decisions are CI-tested here. Only the observe arm is reachable since the `fat`
// build went (2026-09-09) — `split_plane`'s own doc says so.
pub mod split_plane;
pub mod tradehub_control;
// The pure venue/instrument resolvers (`venue_of_key`/`venue_bar_instrument`) moved down from
// `vike-app`'s CI-excluded `main.rs` so the merge gate tests them.
pub mod venue_routing;
