//! Connections to backend daemons: the registry, the live connection and its identity, the observe
//! bridge, the control-wire send, the split-plane routing. The wire's decode and the command lift are
//! `vike_tradehub_client`'s (`observe_decode`, `command_lift`, `node_words`).

pub mod connection;
pub mod editor;
// How a backend is IDENTIFIED on screen: one authority for the strip, picker, status line and title.
pub mod identity;
// `vike-desktop --mcp`: the MCP server over stdio, no window, the desktop's settings and tunnel.
pub mod mcp_mode;
pub mod observe_bridge;
pub mod registry;
// The Data providers group's client of the node's credential verbs (Finnhub and FMP keys, on the SERVER).
pub mod provider_keys;
// The desktop connects itself to its own server, in its own process (`vike_client::attach`): the Server group.
pub mod server_link;
// The pure ARM decisions behind `App::new` (split-plane B2); the shell keeps only the wiring.
pub mod split_plane;
pub mod tradehub_control;
