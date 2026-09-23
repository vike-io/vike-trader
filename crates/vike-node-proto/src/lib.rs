//! `vike-node-proto` — the substrate BOTH node protocols share, and nothing either of them owns.
//!
//! Two localhost services speak a node protocol: `vike-datahub` (through `vike-datahub-client`) and
//! the `vike-tradehub` node (through `vike-tradehub-client`). They carry different verbs and
//! different schemas, and they must not disagree about exactly two things — how a frame is framed,
//! and how a peer proves it holds a key. Those two live here.
//!
//! * [`auth`] — the HMAC-SHA256 nonce-challenge handshake, generalized over its DOMAIN SEPARATOR so
//!   one implementation serves both services with two disjoint preimages
//!   (`docs/decisions/0025-datahub-remote-posture.md`: *"One scheme, not two … the borrow is the
//!   module generalized over its domain constant, not a second implementation"*).
//! * [`frame`] — the length-prefixed JSON frame codec, including the raw read a server needs to
//!   survive an undecodable body without dropping the connection.
//!
//! # ⚠ Why this crate exists, when the tree had already decided it should not
//!
//! Both modules used to live in `vike-datahub-client`, and that home carried an explicit argument
//! against exactly this crate: *"a NEW crate for ~150 lines would buy a workspace member, a CI
//! roster row and a layer negotiation to hold a module that already has a home its sibling depends
//! on."* It was right when it was written. Measured on 2026-09-23, both of its premises had failed:
//!
//! 1. **The module is 937 lines, not ~150** — six times the size the trade-off was priced at.
//! 2. **The home argument was CIRCULAR.** It rested on `vike-datahub-client` being the light crate
//!    *below* `vike-tradehub-client` (layer 30 against 50). But `vike-tradehub-client`'s ONLY
//!    `vike-*` dependency was `vike-datahub-client` — so it sat at 50 *because* the shared code
//!    lived in its sibling, and the shared code lived in its sibling *because* the sibling sat
//!    below. Neither fact held the other up; they held each other up.
//!
//! `CLAUDE.md` states the rule this instantiates, and states it as one the workspace keeps
//! re-learning: *when two sides must not disagree, the cure is a shared crate BELOW both, not a
//! shared crate containing both.* The old home was the second shape wearing the first one's name.
//!
//! # What it buys, measured rather than asserted
//!
//! With the substrate below both, neither client names the other. `vike-tradehub-client` had
//! fifteen rungs of declared slack over an arithmetic floor its one edge set; it now sits at the
//! same rank as its peer, which is what it always was. The knock-on is `vike-cli`, whose ceiling
//! was pinned at 50 by that crate alone as much as by `vike-ops`.
//!
//! # ⚠ It names NO `vike-*` crate, and that is load-bearing rather than incidental
//!
//! A shared floor that names a domain crate is a floor with an opinion, and the next protocol that
//! wants it would inherit that opinion. Nothing here knows what a venue, a bar or an order is: the
//! codec serializes whatever it is handed, and the handshake signs over bytes. That is the property
//! `crates/vike-ops/tests/layer_gate.rs`'s tier-15 rule checks — a `leaf` names at most the floor,
//! and this one names less than that.

pub mod auth;
pub mod frame;
