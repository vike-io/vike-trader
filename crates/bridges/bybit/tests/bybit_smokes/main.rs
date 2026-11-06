//! **The bybit demo smokes that resolve their account by `demo_label`, ONE test binary**
//! (`--test bybit_smokes`): the classes probe, the modes smoke and the restore smoke, so `demo_label`
//! compiles ONCE instead of once per binary.
//!
//! Every member is `#[ignore]`d and runs only when asked, by module:
//! `cargo test -p vike-bybit --test bybit_smokes -- --ignored --nocapture bybit_restore_smoke::` (each
//! test's name is unchanged after its module prefix), per `crates/vike-backtest/CLAUDE.md`'s "Test-binary
//! consolidation". Each member keeps its own file, module doc and tests.
//!
//! ⚠ **Not a lane binary, and none may join it**: `demo_label` names `vike_secrets::account_directory`,
//! which `scripts/refuse_live_credentials.sh` refuses in every file of a binary
//! `.github/workflows/live-smokes.yml` runs. Plain `mod` lines only all the same, the lane groups' shape.

mod demo_label;

mod bybit_classes_probe;
mod bybit_modes_smoke;
mod bybit_restore_smoke;
