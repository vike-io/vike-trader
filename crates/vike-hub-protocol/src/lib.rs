//! `vike-hub-protocol` — the substrate BOTH hub protocols share, and nothing either of them owns.
//!
//! Two localhost services speak a hub protocol: `vike-datahub` (through `vike-datahub-client`) and
//! the `vike-tradehub` node (through `vike-tradehub-client`). They carry different verbs and
//! schemas, and must not disagree about how a frame is framed, how a peer proves it holds a key,
//! and whether a listen address is reachable off-box. Those live here:
//!
//! * [`auth`] — the HMAC-SHA256 nonce-challenge handshake, generalized over its DOMAIN SEPARATOR so
//!   one implementation serves both services with two disjoint preimages.
//! * [`access`] — a device's own Ed25519 key, the access list's row and the ONE check every hub runs on a
//!   person (decision 0288), plus an invitation's proof.
//! * [`frame`] — the length-prefixed JSON frame codec, including the raw read a server needs to
//!   survive an undecodable body without dropping the connection, the raw write of the one body that is
//!   not JSON (the datahub's bar columns, decision 0107's amendment), and the one socket option every
//!   node socket carries (`frame::configure_hub_stream`, `TCP_NODELAY`).
//! * [`child_pipe`] — the pipe between a hub and a child it starts from its OWN executable (the sandboxed
//!   backtest and the sandboxed live mount): one msgpack frame codec, positional by default, and the ONE
//!   same-build handshake that makes positional safe.
//! * [`bind`] — the bind ADDRESS classification (loopback, public, unresolvable) each server's
//!   non-loopback refusal reads; each server keeps its own decision over it.
//! * [`hash`] — SHA-256 of bytes as lowercase hex, the one home for that digest spelling; it is
//!   here because this crate already links `sha2`; its hex is `vike_utility::hex::hex_lower`.
//! * [`limits`] — the pre-auth frame cap, the handshake deadline, the idle read timeout and the two
//!   client dial bounds every node daemon and client reads, one definition each.
//! * [`plane`], [`handshake`], [`conn`] — what the two DATA-plane hubs share since each has its own
//!   client (`docs/decisions/0252-the-cli-and-the-mcp-server-are-two-doors-over-one-client-and-every-hub-has-its-own-client.md`):
//!   which hub answers which verb and the refusal a wrong dial gets, the keyed pre-auth handshake
//!   (the ONE every hub daemon runs: the gatewayhub and the tradehub too, each under its own domain and
//!   no data plane, decision 0288), and the blocking connection both data-plane clients are built on.
//! * [`wire_ident`] — the length and charset rules of the two untrusted names both those clients refuse
//!   by, a venue slug and a symbol. Bytes only: it holds no roster.
//! * [`features`] — the `Welcome.features` words more than one hub speaks (`wire_msgpack`, `gateway_status`,
//!   `market_data_venue=`), and the one reader of a `prefix=value` entry. Each hub's own words stay in its client.
//! * [`refusal`] — the hubs' refusal FRAME `{id, text}` (`HubRefusal`), sent only to a client that said it reads it.
//! * [`call`] — `HubCallError`, what a failed hub call DID: the one type a hub client returns where its caller decides.
//!
//! With the substrate below both, the two clients sit at the same rank and neither names the
//! other: *when two sides must not disagree, the cure is a shared crate BELOW both*. Why this is a
//! crate of its own:
//! `docs/decisions/0107-the-node-protocol-substrate-is-a-crate-below-both-clients.md`.
//!
//! # ⚠ It names NO `vike-*` crate but the helper floor `vike-utility`, and that is load-bearing rather than incidental
//!
//! A shared floor that names a domain crate is a floor with an opinion, and the next protocol that
//! wants it would inherit that opinion. Nothing here knows what a venue, a bar or an order is: the
//! codec serializes whatever it is handed, and the handshake signs over bytes. Naming no `vike-*`
//! crate is held by `crates/vike-ops/tests/architecture/layer_gate/vike_free.rs`'s
//! `every_vike_free_crate_names_no_vike_crate`, through this crate's row in that file's
//! `VIKE_FREE_CRATES` and its one `FLOOR_EDGES` row, onto `vike-utility` (the one lowercase hex: a floor that names no
//! `vike-*` crate and knows no venue, bar or order) — the tier-15 rule alone is weaker, since a `leaf` may still name
//! the vocabulary floor.

pub mod access;
pub mod auth;
pub mod bind;
pub mod call;
pub mod child_pipe;
pub mod conn;
pub mod features;
pub mod frame;
pub mod handshake;
pub mod hash;
pub mod limits;
pub mod plane;
pub mod refusal;
/// The committed wire-golden checks the protocol tag suites share (`test-support` only: test code).
#[cfg(feature = "test-support")]
pub mod test_support;
pub mod wire_ident;
