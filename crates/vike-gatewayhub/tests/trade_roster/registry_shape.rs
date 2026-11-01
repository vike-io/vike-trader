//! **The trade registry's shape**: every roster venue exactly once, every feature-off row named after its
//! feature, every contract row paper and offline over an empty store, and what the build mounts. They
//! lived in the tradehub's `tests/daemon/{registry,wired_markets}.rs` while the registry was that crate's;
//! the registry is the gatewayhub's now (`src/registry/trade.rs`), so the pins that read it moved with it
//! (wave 2, W9b). The features are this crate's `ibkr`, `fxcm` and `polymarket`; the tests hold in every
//! combination, and `tradehub-all` runs them with all three on.

// A member of the `trade_roster` group (`main.rs`): its children name the group root's `crate::local`.
#[path = "registry_shape/addressing.rs"]
mod addressing;
#[path = "registry_shape/registry.rs"]
mod registry;
#[path = "registry_shape/wired_markets.rs"]
mod wired_markets;
