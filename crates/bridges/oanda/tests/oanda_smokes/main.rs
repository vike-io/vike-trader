//! **The two OANDA demo smokes that wait on an ingest lane, ONE test binary** (`--test oanda_smokes`), so
//! `ingest_wait` compiles ONCE instead of once per binary.
//!
//! Every member is `#[ignore]`d and runs only when asked, by module:
//! `cargo test -p vike-oanda --test oanda_smokes -- --ignored --nocapture oanda_demo_smoke::` (each test's
//! name is unchanged after its module prefix), per `crates/vike-backtest/CLAUDE.md`'s "Test-binary
//! consolidation". Each member keeps its own file, module doc and tests.
//!
//! ⚠ **Plain `mod` lines only, never a path attribute or an include macro** (in any member too):
//! `.github/workflows/live-smokes.yml` runs `oanda_demo_smoke` from this binary, and
//! `scripts/refuse_live_credentials.sh` scans EVERY file the binary it names compiles, following plain `mod`
//! declarations from this file; it refuses one it cannot follow, and refuses the whole lane if any member
//! asks for a live tier. A new member is scanned by the lane too.

mod ingest_wait;

mod oanda_demo_smoke;
mod oanda_unit_semantics_smoke;
