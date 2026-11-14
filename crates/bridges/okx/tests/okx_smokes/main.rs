//! **The okx demo smokes and probes that resolve their account through `demo_credentials`, ONE test binary**
//! (`--test okx_smokes`), so `demo_credentials` (the demo account's credentials) and `demo_swap` (the three
//! swap drills' REST and instrument) compile ONCE instead of once per binary.
//!
//! Every member is `#[ignore]`d and runs only when asked, by module:
//! `cargo test -p vike-okx --test okx_smokes -- --ignored --nocapture okx_restore_smoke::` (each test's
//! name is unchanged after its module prefix), per `crates/vike-backtest/CLAUDE.md`'s "Test-binary
//! consolidation". Each member keeps its own file, module doc and tests.
//!
//! ⚠ **Plain `mod` lines only, never a path attribute or an include macro** (in any member too):
//! `.github/workflows/live-smokes.yml` runs `okx_reconcile_smoke` from this binary, and
//! `scripts/refuse_live_credentials.sh` scans EVERY file the binary it names compiles, following plain `mod`
//! declarations from this file; it refuses one it cannot follow, and refuses the whole lane if any member
//! asks for a live tier. A new member is scanned by the lane too.

mod demo_credentials;
mod demo_swap;

mod okx_algo_amend_smoke;
mod okx_algo_socket_smoke;
mod okx_algo_stop_smoke;
mod okx_fired_stop_smoke;
mod okx_funding_capture;
mod okx_identity_probe;
mod okx_inverse_smoke;
mod okx_margin_mode_smoke;
mod okx_per_instrument_smoke;
mod okx_post_only_smoke;
mod okx_reconcile_smoke;
mod okx_restore_smoke;
mod okx_spot_buy_stop_smoke;
mod okx_stop_limit_smoke;
mod okx_take_profit_smoke;
mod okx_time_in_force_smoke;
mod okx_trailing_smoke;
