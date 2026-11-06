//! **The live IG smokes and probes, ONE test binary** (`--test ig_smokes`): the seven files that share the
//! demo-account setup in `common` (the credential store's default DEMO ig account and the default epic),
//! so `common` compiles ONCE instead of once per binary.
//!
//! Every member is `#[ignore]`d and runs only when asked, by module:
//! `cargo test -p vike-ig --test ig_smokes -- --ignored --nocapture ig_reconcile_smoke::` (each test's name is
//! unchanged after its module prefix), per `crates/vike-backtest/CLAUDE.md`'s "Test-binary consolidation".
//! Each member keeps its own file, module doc and tests.
//!
//! ⚠ **Plain `mod` lines only, never a path attribute or an include macro** (in any member too):
//! `.github/workflows/live-smokes.yml` runs `ig_reconcile_smoke` from this binary, and
//! `scripts/refuse_live_credentials.sh` scans EVERY file the binary it names compiles, following plain `mod`
//! declarations from this file; it refuses one it cannot follow, and refuses the whole lane if any member
//! asks for a live tier. A new member is scanned by the lane too.

mod common;

mod ig_accounts_smoke;
mod ig_close_position_smoke;
mod ig_history_probe;
mod ig_limit_close_probe;
mod ig_market_feed_smoke;
mod ig_market_group_probe;
mod ig_reconcile_smoke;
