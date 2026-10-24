//! The world one case runs in: the spawned `vike-cli mcp` server with its recorded transport ([`mcp`]) and the
//! throwaway paper `vike-tradehub` node it talks to ([`hub`]).

pub mod hub;
pub mod mcp;
