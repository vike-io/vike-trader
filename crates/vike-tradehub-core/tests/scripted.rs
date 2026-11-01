//! `scripted` — the paper-mount suites driven by scripted ticks (the A-S maker at a 0-1 and a BTC-scale
//! mid, the cross-venue maker): ONE test binary, so `tests/common/mod.rs` (the halt-indifference proof,
//! `no_halt`, `wait_until`) compiles once instead of once per binary that declared `mod common;`
//! (owner, 2026-10-31). It cannot compile into this crate's lib: it uses `tempfile`, which no member may
//! take as a normal dependency (`crates/vike-ops/tests/hygiene/system_temp_gate/ratchet.rs`'s
//! `tempfile_is_never_a_normal_dependency`).
//!
//! Each former `tests/<name>.rs` is now `tests/scripted/<name>.rs`, included below as a plain module, its
//! tests and their bodies unchanged but one argument: each halt-indifference test now names its own
//! module (`common::assert_indifferent_to_an_engaged_halt_sentinel("mount_scripted::")`), so its child
//! run re-runs exactly the tests it re-ran when its file was its own binary. `--test mount_scripted` is
//! now `--test scripted`, a test's nextest name gains its module, and `-- <name>` filtering is untouched.
//!
//! Under nextest every test is its own process, as before. Under a plain `cargo test` the three files'
//! tests share one process: each calls the same `vike_log::test_init()` (one subscriber, whichever test
//! is first), none sets an environment variable or changes the working directory, and each pins its
//! own HALT sentinel or takes the default one as it always did, so none of them sees another's state.
//! The four HALT binaries stay standalone (`halt_path_from_env` memoizes in a process-global
//! `OnceLock`, see this crate's CLAUDE.md).
//!
//! Since 2026-10-31 the crate's other plain suites are members too, for the LINK alone (owner: fewer
//! test binaries): `account_table_is_reachable`, `halt_paper_live_agree` (it names its own unique
//! sentinel and never asks `halt_path_from_env`), `risk_budget_diagnostic`, and the property suite
//! `account_leverage_props`, whose failure seed stays BESIDE its file
//! (`tests/scripted/account_leverage_props.proptest-regressions` when one is committed): this root is
//! `tests/scripted.rs`, not `tests/scripted/main.rs`, so proptest's walk up from the member finds no
//! `lib.rs`/`main.rs` and falls back to the file beside the source.

#[cfg(test)]
#[path = "common/mod.rs"]
mod common;

#[cfg(test)]
#[path = "scripted/account_leverage_props.rs"]
mod account_leverage_props;
#[cfg(test)]
#[path = "scripted/account_table_is_reachable.rs"]
mod account_table_is_reachable;
#[cfg(test)]
#[path = "scripted/halt_paper_live_agree.rs"]
mod halt_paper_live_agree;
#[cfg(test)]
#[path = "scripted/maker_price_scale.rs"]
mod maker_price_scale;
#[cfg(test)]
#[path = "scripted/mount_scripted.rs"]
mod mount_scripted;
#[cfg(test)]
#[path = "scripted/risk_budget_diagnostic.rs"]
mod risk_budget_diagnostic;
#[cfg(test)]
#[path = "scripted/xemm_scripted.rs"]
mod xemm_scripted;
