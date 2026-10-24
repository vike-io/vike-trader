//! `vike-hub-proto` — the substrate BOTH hub protocols share, and nothing either of them owns.
//!
//! Two localhost services speak a hub protocol: `vike-datahub` (through `vike-datahub-client`) and
//! the `vike-tradehub` node (through `vike-tradehub-client`). They carry different verbs and
//! schemas, and must not disagree about how a frame is framed, how a peer proves it holds a key,
//! and whether a listen address is reachable off-box. Those live here:
//!
//! * [`auth`] — the HMAC-SHA256 nonce-challenge handshake, generalized over its DOMAIN SEPARATOR so
//!   one implementation serves both services with two disjoint preimages.
//! * [`frame`] — the length-prefixed JSON frame codec, including the raw read a server needs to
//!   survive an undecodable body without dropping the connection, and the one socket option every
//!   node socket carries (`frame::configure_hub_stream`, `TCP_NODELAY`).
//! * [`bind`] — the bind ADDRESS classification (loopback, public, unresolvable) each server's
//!   non-loopback refusal reads; each server keeps its own decision over it.
//! * [`hash`] — SHA-256 of bytes as lowercase hex, the one home for that digest spelling; it is
//!   here because this crate already links `sha2` and names no `vike-*` crate.
//! * [`limits`] — the pre-auth frame cap, the handshake deadline, the idle read timeout and the two
//!   client dial bounds every node daemon and client reads, one definition each.
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
pub mod bind;
pub mod frame;
pub mod hash;
pub mod limits;
