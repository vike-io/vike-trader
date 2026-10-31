//! `offline` -- vike-aster's plain offline suite: ONE test binary over what used to be six.
//!
//! Cargo links a separate test BINARY per top-level `tests/*.rs` file. Each former
//! `tests/<name>.rs` member here is now `tests/offline/<name>.rs`, included below as a plain
//! module: the tests, their names, their bodies and their fixtures are unchanged -- only the binary
//! they link into moved. Run one member's tests with `offline` in the `--test` slot:
//!
//! ```sh
//! cargo test -p vike-aster --test offline                 # the whole group
//! cargo test -p vike-aster --test offline -- trigger_gate # one member, filtered by module name
//! ```
//!
//! The rule for what may join: OFFLINE -- no network, no credentials, nothing `#[ignore]`d. Every
//! other `tests/*.rs` is its own binary on purpose: the live smokes and the soak (`aster_smoke`,
//! `aster_market_data_smoke`, `aster_reconcile_smoke`, `aster_userdata_soak`) are `#[ignore]`d and
//! hit the network -- all but `aster_market_data_smoke` can reach MAINNET with LIVE keys -- and run
//! by hand with the command in their own `//!`; `decoder_never_panics` is the proptest harness;
//! `testnet_claim_gate` is named in `scripts/publish_mirror.sh`. Every member imports an
//! `exec`-only module, hence the `required-features = ["exec"]` entry in this crate's `Cargo.toml`.

// `#[path]` because this file is a test-target CRATE ROOT: a bare `mod trigger_gate;` would
// resolve against `tests/` (the root's own directory), not `tests/offline/`.
#[path = "offline/aster_mapper.rs"]
mod aster_mapper;
#[path = "offline/aster_userdata.rs"]
mod aster_userdata;
#[path = "offline/confirm_by_coid.rs"]
mod confirm_by_coid;
#[path = "offline/per_symbol.rs"]
mod per_symbol;
#[path = "offline/perp_contract.rs"]
mod perp_contract;
#[path = "offline/recon_admitted.rs"]
mod recon_admitted;
#[path = "offline/recon_client_parse.rs"]
mod recon_client_parse;
#[path = "offline/recon_fee_lane_routing.rs"]
mod recon_fee_lane_routing;
#[path = "offline/resync_admitted.rs"]
mod resync_admitted;
#[path = "offline/submit_status_unknown.rs"]
mod submit_status_unknown;
#[path = "offline/trigger_gate.rs"]
mod trigger_gate;
