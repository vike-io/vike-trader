//! **The bybit demo smokes that resolve their account by `demo_label`, ONE test binary**
//! (`--test bybit_smokes`): the classes probe, the funding capture, the inverse-mount smoke, the modes smoke, the restore smoke, the
//! stop-limit smoke, the take-profit smoke, the spot conditional smoke and the trigger-amend smoke, so
//! `demo_label` compiles ONCE instead of once per binary. The venue-read smoke asks for the `INV` row by
//! name instead, and sits here because it reads the store the same way.
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
mod bybit_funding_capture;
mod bybit_inverse_smoke;
mod bybit_modes_smoke;
mod bybit_restore_smoke;
mod bybit_spot_conditional_smoke;
mod bybit_stop_limit_smoke;
mod bybit_take_profit_smoke;
mod bybit_trigger_amend_smoke;
mod bybit_venue_read_smoke;
