//! `vike-node-proto` — the substrate BOTH node protocols share, and nothing either of them owns.
//!
//! Two localhost services speak a node protocol: `vike-datahub` (through `vike-datahub-client`) and
//! the `vike-tradehub` node (through `vike-tradehub-client`). They carry different verbs and
//! schemas, and must not disagree about exactly two things — how a frame is framed, and how a peer
//! proves it holds a key. Those two live here:
//!
//! * [`auth`] — the HMAC-SHA256 nonce-challenge handshake, generalized over its DOMAIN SEPARATOR so
//!   one implementation serves both services with two disjoint preimages.
//! * [`frame`] — the length-prefixed JSON frame codec, including the raw read a server needs to
//!   survive an undecodable body without dropping the connection, and the one socket option every
//!   node socket carries (`frame::configure_node_stream`, `TCP_NODELAY`).
//!
//! With the substrate below both, the two clients sit at the same rank and neither names the
//! other: *when two sides must not disagree, the cure is a shared crate BELOW both*. Why this is a
//! crate of its own:
//! `docs/decisions/0107-the-node-protocol-substrate-is-a-crate-below-both-clients.md`.
//!
//! # ⚠ It names NO `vike-*` crate, and that is load-bearing rather than incidental
//!
//! A shared floor that names a domain crate is a floor with an opinion, and the next protocol that
//! wants it would inherit that opinion. Nothing here knows what a venue, a bar or an order is: the
//! codec serializes whatever it is handed, and the handshake signs over bytes. Naming no `vike-*`
//! crate is held by `crates/vike-ops/tests/architecture/layer_gate/vike_free.rs`'s
//! `every_vike_free_crate_names_no_vike_crate`, through this crate's row in that file's
//! `VIKE_FREE_CRATES` — the tier-15 rule alone is weaker, since a `leaf` may still name the floor.

pub mod auth;
pub mod frame;
