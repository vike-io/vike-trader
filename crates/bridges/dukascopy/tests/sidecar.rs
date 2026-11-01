//! The fake-sidecar suites as ONE test binary (`cargo test -p vike-dukascopy --test sidecar`).
//!
//! Each suite is still its own file, `tests/sidecar/<suite>.rs`, and here a MODULE named after it, so a
//! test's id is `<suite>::<test>` and `cargo test -p vike-dukascopy --test sidecar -- dukascopy_restart::`
//! runs one suite. Every test and every assertion is the one the suite's own binary ran.
//!
//! Why one binary: every suite drives the scripted fake JForex bridge through `tests/support/mod.rs`
//! (its path, the client configuration, the EURUSD orders, the engine holding one, the quoting
//! spawn), and as seven binaries each compiled its own copy; declared ONCE below, it compiles once and
//! each suite reaches it as `crate::support`. The grouping rule is `crates/vike-backtest/CLAUDE.md`'s
//! "Test-binary consolidation": every member is plain (no feature gate, no proptest sidecar, no
//! process-global mutation); each test spawns its own fake child process, so several in one process
//! share nothing (the reason the venue claim lives outside this crate, `src/account.rs`).

mod support;

#[path = "sidecar/dukascopy_adopt.rs"]
mod dukascopy_adopt;
#[path = "sidecar/dukascopy_currency.rs"]
mod dukascopy_currency;
#[path = "sidecar/dukascopy_exec.rs"]
mod dukascopy_exec;
#[path = "sidecar/dukascopy_netting.rs"]
mod dukascopy_netting;
#[path = "sidecar/dukascopy_quotes.rs"]
mod dukascopy_quotes;
#[path = "sidecar/dukascopy_reconcile.rs"]
mod dukascopy_reconcile;
#[path = "sidecar/dukascopy_restart.rs"]
mod dukascopy_restart;
