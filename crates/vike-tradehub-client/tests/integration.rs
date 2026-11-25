//! **vike-tradehub-client's integration tests as ONE test binary** (`cargo test -p vike-tradehub-client --test integration`).
//!
//! Each file keeps its path, `tests/<name>.rs`; here it is a MODULE, so a test's id is `<name>::<test>` and
//! `cargo test -p vike-tradehub-client --test integration -- <name>::` runs one file. One binary is compiled and
//! linked instead of one per file, grouped by the rule of `crates/vike-backtest/CLAUDE.md`'s "Test-binary
//! consolidation": no member mutates process-global state (each scripts its own `fake_node` on its own port).
//! Test discovery is OFF (`autotests = false`), so a new `tests/*.rs` builds only as a `mod` here or a
//! `[[test]]` row, and `crates/vike-ops/tests/hygiene/test_registry_gate.rs`'s `every_root_test_file_is_built_where_discovery_is_off` fails until it is one.

mod account_negotiation;
mod codec_negotiation;
mod credential_negotiation;
mod directory_negotiation;
mod handshake;
mod observe_newest_negotiation;
mod order_tif_negotiation;
mod settings_show_negotiation;
mod settings_write_negotiation;
mod strategy_verbs;
mod venue_read_negotiation;
