//! Connections to backend daemons: the registry, the live connection and its identity, the observe
//! bridge, the control-wire lowering, the split-plane routing.

pub mod backend_conn;
pub mod backend_editor;
// How a backend is IDENTIFIED on screen: one authority for the strip, picker, status line and title.
pub mod backend_identity;
pub mod backend_registry;
pub mod observe_bridge;
// The pure ARM decisions behind `App::new` (split-plane B2); the shell keeps only the wiring.
pub mod split_plane;
pub mod tradehub_control;
// The pure venue/instrument resolvers (`venue_of_key`/`venue_bar_instrument`).
pub mod venue_routing;
