//! `hop` — the hop twin, the kill proofs and the property test over ONE shared rig: ONE test binary,
//! so `tests/support/hop_rig.rs` (a REAL gatewayhub server, a link proxy, a scripted venue, a real
//! core) compiles once instead of once per binary that `#[path]`-included it (owner, 2026-10-31). The
//! rig cannot compile into this crate's lib with the other shared helpers (`src/test_support.rs`): it
//! names `vike-gatewayhub`, a dev-dependency at this crate's own layer.
//!
//! Each former `tests/<name>.rs` is now `tests/hop/<name>.rs`, included below as a plain module, its
//! tests and their bodies unchanged: only the binary they link into moved (`--test hop_kill_proofs` is
//! now `--test hop`, a test's nextest name gains its module, `hop_kill_proofs::<test>`, and
//! `-- <name>` substring filtering is untouched). Each member reaches the rig as `crate::hop_rig`.
//!
//! Since 2026-10-31 the ten WIRE suites (the node's half of the hop against a scripted gatewayhub
//! peer or the real server's scripted lane: `gateway_refusals`, `gateway_restart_wire`, `late_arm_wire`,
//! `late_currency_wire`, `oneshots_wire`, `preflight_parity`, `refusal_chain`, `remote_backend_wire`,
//! `symbol_money_wire`, `venue_link_wire`) are members too, for the link alone: one binary links once.
//!
//! What changes with the merge: under nextest (CI, `just test`) every test was and stays its own
//! process, so nothing serialises and no state is shared. Under a plain `cargo test -p vike-tradehub`
//! the members' tests now run as threads of ONE process instead of one process per file, one after
//! another; none of them installs a global `tracing` subscriber, sets an environment variable or
//! changes the working directory (the two that read logs, `gateway_refusals` and `remote_backend_wire`,
//! use `vike_log::capture::captured`, the thread-scoped capture built for exactly this), and every
//! socket binds an ephemeral loopback port, so there is no process-global state for them to race on.
//!
//! `no_order_vanishes` is a proptest with a committed seed. The repo's grouping rule keeps a file
//! with a seed in its own binary; it is grouped here by the owner's order, and the seed moved BESIDE
//! it (`tests/hop/no_order_vanishes.proptest-regressions`), where proptest still reads it: this root
//! is `tests/hop.rs`, not `tests/hop/main.rs`, so proptest's walk up from the member finds no
//! `lib.rs`/`main.rs` and falls back to the file beside the source (that member's own doc says so).

#[cfg(test)]
#[allow(dead_code, clippy::allow_attributes)]
#[path = "support/hop_rig.rs"]
mod hop_rig;

#[cfg(test)]
#[path = "hop/any_symbol_twin.rs"]
mod any_symbol_twin;
#[cfg(test)]
#[path = "hop/gateway_twin.rs"]
mod gateway_twin;
#[cfg(test)]
#[path = "hop/hop_kill_proofs.rs"]
mod hop_kill_proofs;
#[cfg(test)]
#[path = "hop/no_order_vanishes.rs"]
mod no_order_vanishes;
#[cfg(test)]
#[path = "hop/venue_backend_parity.rs"]
mod venue_backend_parity;

// The wire suites: the node's half of the gatewayhub hop against a scripted peer
// (`vike_gatewayhub_client::test_support`) or the real server's scripted lane. They share no helper
// the lib cannot hold; they are here for the LINK (owner, 2026-10-31: fewer test binaries).
#[cfg(test)]
#[path = "hop/gateway_refusals.rs"]
mod gateway_refusals;
#[cfg(test)]
#[path = "hop/gateway_restart_wire.rs"]
mod gateway_restart_wire;
#[cfg(test)]
#[path = "hop/late_arm_wire.rs"]
mod late_arm_wire;
#[cfg(test)]
#[path = "hop/late_currency_wire.rs"]
mod late_currency_wire;
#[cfg(test)]
#[path = "hop/oneshots_wire.rs"]
mod oneshots_wire;
#[cfg(test)]
#[path = "hop/preflight_parity.rs"]
mod preflight_parity;
#[cfg(test)]
#[path = "hop/refusal_chain.rs"]
mod refusal_chain;
#[cfg(test)]
#[path = "hop/remote_backend_wire.rs"]
mod remote_backend_wire;
#[cfg(test)]
#[path = "hop/symbol_money_wire.rs"]
mod symbol_money_wire;
#[cfg(test)]
#[path = "hop/venue_link_wire.rs"]
mod venue_link_wire;
