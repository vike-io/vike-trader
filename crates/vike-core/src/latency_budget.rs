//! The core-hop tail budget the latency gate enforces — a NUMBER, declared in the library so the
//! two readers that need it name one symbol: `crates/vike-core/tests/runtime_latency.rs` (which
//! asserts the baseline variant against it) and `vike-docs` (which publishes it as
//! `stats.json`'s `latency_p99_budget_ns`). It was a test-file constant that `vike-docs` copied and
//! pinned with a text-parsing gate; a library constant needs neither.
//!
//! ⚠ Nothing on the fold reads this: it is a `const` no production path uses, so it adds no
//! instruction to the measured hop. What the gate measures is `crates/vike-core/CLAUDE.md`'s
//! "The latency gate"; the ceilings of the other variants (`JOURNAL_P99_NS`, the p99.9 and max
//! ratchets) stay in the harness beside their calibration notes.

/// The plan gate itself, unchanged since R5: p99 core-hop < 10 µs, in nanoseconds — the CEILING,
/// never a measurement. **For the BASELINE variant only** since the 2026-08-05 calibration, which
/// gave the journal variants their own `JOURNAL_P99_NS` in `runtime_latency.rs`. An R5 plan
/// literal rather than a derived ceiling, kept because on the journal-free path it sits 8-30x over
/// what the baseline measures in CI; tightening it is a separate question. Re-derive from a fresh
/// harvest rather than nudging the literal:
/// `docs/ops/latency-gate-calibration.md`'s "`P99_BUDGET_NS`".
pub const P99_BUDGET_NS: u64 = 10_000;
