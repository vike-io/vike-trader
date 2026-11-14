//! **The Studio's posed-shell suites, ONE test binary** (`--test headless`): every integration suite
//! that drives the Studio headlessly over the shared fixtures in `common` (the refusing catalog store,
//! the empty and seeded stores, the workspace-scrubbed posed `StudioState`, the loopback backtest
//! server).
//!
//! Grouped (the shape of `crates/vike-chart/tests/headless/main.rs`) so that `common` compiles ONCE:
//! as four binaries each of the four compiled its own copy. The lib route was not open: `common`
//! needs `tempfile`, `vike-store` and `vike-backtesthub`, all dev-dependencies (the Studio's normal
//! tree stays DataFusion-free). Every suite is a module named after the file it was, so a test's
//! name is unchanged after its module prefix and only the `--test` slot moved:
//! `cargo test -p vike-studio --test headless -- tessellation_goldens::`. Every member is plain (no
//! feature gate, no `#[ignore]`, no proptest sidecar, no `env::set_var`/`set_current_dir`).

mod common;

mod picker_disclosure;
mod studio_follows_the_appearance;
mod studio_shell_render;
mod tessellation_goldens;
