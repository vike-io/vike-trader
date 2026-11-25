//! **vike-ml-training's suites as ONE test binary** (`cargo test -p vike-ml-training --test integration`).
//!
//! Each suite keeps its file, `tests/<suite>.rs`, with its `//!` contract; here it is a MODULE, so a test's id is
//! `<suite>::<test>` and `cargo test -p vike-ml-training --test integration -- <suite>::` runs one suite. One binary
//! is compiled and linked instead of one per file, grouped by the rule of `crates/vike-backtest/CLAUDE.md`'s
//! "Test-binary consolidation": no member mutates process-global state. The `#[ignore]`d LightGBM suites
//! (`lightgbm_cli_smoke`, `train_infer_equality`, `gbdt_learner_smoke`) run as
//! `--test integration -- --ignored <suite>::`, which is how `scripts/ci_lightgbm_suite.sh` runs them. The pinned
//! binary's helper, `tests/common/lightgbm.rs`, is declared once here.
//!
//! ⚠ The manifest turns test discovery OFF (`autotests = false`) so the suites keep their paths: a new
//! `tests/*.rs` runs only once it is a `mod` here or a `[[test]]` row of `crates/vike-ml-training/Cargo.toml`.

#[path = "common/lightgbm.rs"]
mod lightgbm;

mod gbdt_learner_smoke;
mod learner_double;
mod libm_platform_probe;
mod lightgbm_cli_smoke;
mod train_infer_equality;
