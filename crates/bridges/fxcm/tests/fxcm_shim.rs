//! The shim SOURCE gates as ONE test binary (`cargo test -p vike-fxcm --test fxcm_shim`).
//!
//! Each gate is still its own file, `tests/fxcm_shim/<gate>.rs`, and here a MODULE named after it, so a
//! test's id is `<gate>::<test>` and `cargo test -p vike-fxcm --test fxcm_shim -- fxcm_shim_exit::` runs
//! one gate. Every test and every assertion is the one the gate's own binary ran.
//!
//! Why one binary: every gate reads `crates/bridges/fxcm/src/shim/fcshim.cpp` through
//! `tests/shim_source/mod.rs`, and as seven binaries each compiled its own copy; declared ONCE below,
//! it compiles once and each gate reaches it as `crate::shim_source`. The grouping rule is
//! `crates/vike-backtest/CLAUDE.md`'s "Test-binary consolidation": every member is plain (no feature
//! gate, no proptest sidecar, no process-global mutation). `fxcm_shim_order_permission.rs` reads the
//! C++ without this helper and stays its own binary.

mod shim_source;

#[path = "fxcm_shim/fxcm_shim_close.rs"]
mod fxcm_shim_close;
#[path = "fxcm_shim/fxcm_shim_conversion_reads.rs"]
mod fxcm_shim_conversion_reads;
#[path = "fxcm_shim/fxcm_shim_envelope_grammar.rs"]
mod fxcm_shim_envelope_grammar;
#[path = "fxcm_shim/fxcm_shim_exit.rs"]
mod fxcm_shim_exit;
#[path = "fxcm_shim/fxcm_shim_return_codes.rs"]
mod fxcm_shim_return_codes;
#[path = "fxcm_shim/fxcm_shim_subscription.rs"]
mod fxcm_shim_subscription;
#[path = "fxcm_shim/fxcm_shim_table_reads.rs"]
mod fxcm_shim_table_reads;
