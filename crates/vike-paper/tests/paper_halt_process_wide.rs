//! The `backtest == paper` half of the HALT arming, proven WITHOUT depending on what is lying
//! around on the box that runs it.
//!
//! # Why this is its own file, and its own test binary
//!
//! `tests/paper_halt.rs`'s `an_unarmed_book_ignores_a_halt_file_that_exists` states the property
//! this crate's opt-in arming exists for: a book no mount armed reads NO sentinel, so a stray `HALT`
//! file cannot silently change `crates/vike-backtest/tests/r7_gate.rs`'s bit-for-bit fills. The
//! property is right. **The proof was not** — the mutation it named (make the `None` arm fall back
//! to the process-wide path) only reddens on a box where that resolved path happens to EXIST, and it
//! resolves to `<project>/settings/state/HALT` or `<exe_dir>/HALT`, neither of which exists on a CI
//! runner or on a clean checkout. So the named mutation shipped GREEN everywhere the gate actually
//! runs: a mutation proof that holds only on the author's machine is not a proof.
//!
//! This file removes the dependency on the box by supplying the environment itself. It sets
//! `VIKE_HALT_FILE` — the FIRST and highest-precedence rung of
//! `crates/vike-bridge-core/src/halt.rs`'s `resolve_halt_path`, so ANY fallback that consults the
//! process-wide sentinel lands on it whatever else the machine has — and points it at a file this
//! test creates. An unarmed book must still accept. The mutation now reddens on every box, because
//! the file it would find is one this test put there.
//!
//! ⚠ **`set_var` is why this is a separate binary containing exactly ONE test.** Cargo runs a test
//! binary's tests as threads in one process, and mutating the environment while another thread reads
//! it is a data race. One test per process removes the question rather than reasoning about it — and
//! it is also why the sibling file must NOT grow this test: `paper_halt.rs` has nine.
//!
//! What it deliberately does not cover: a hypothetical fallback that skipped the `VIKE_HALT_FILE`
//! rung and went straight to `<project>/settings/state/HALT`. Planting THAT file is not available to
//! a test — it is the same path every other test binary in this workspace resolves, and creating it
//! mid-run would engage a real halt inside `vike-bridge-core`'s and `vike-ctrader`'s suites running
//! in parallel. The env rung is the one a resolver reads first, so it is the one worth pinning.

use std::path::PathBuf;

use vike_exec::ExecutionClient;
use vike_model::events::Event;
use vike_model::{FeeSchedule, OrderRequest};
use vike_paper::PaperExecutionClient;

/// The variable the process-wide resolver reads FIRST. Named here rather than reached through
/// `vike_bridge_core::halt` on purpose: this crate must not depend on that crate in any capacity
/// (see `crates/vike-exec/src/halt.rs`'s module doc on why the predicate moved down), and a test
/// that pulled the transport stack in to name one string would undo the split it is protecting.
const HALT_FILE_ENV: &str = "VIKE_HALT_FILE";

/// A book that no mount armed ignores the PROCESS-WIDE sentinel, even when that sentinel exists —
/// and this test is what makes it exist, on any box.
///
/// ⚠ **MEASURED on the CI box (2026-08-08), which is not the author's box — and the measurement is also
/// the proof that the OLD one was worthless.** Giving `PaperExecutionClient`'s `halt_engaged` a
/// `None`-arm fallback that resolves `VIKE_HALT_FILE` produced:
///
/// ```console
/// PASS vike-paper::paper_halt an_unarmed_book_ignores_a_halt_file_that_exists
/// PASS vike-paper::paper_halt an_unarmed_book_modifies_under_a_halt_file_that_exists
/// FAIL vike-paper::paper_halt_process_wide an_unarmed_book_ignores_the_process_wide_sentinel_even_when_it_exists
/// ```
///
/// The two tests that NAMED that mutation shipped GREEN through it, because on that runner
/// `VIKE_HALT_FILE` is unset and `settings/state/HALT` does not exist — exactly the environment CI
/// has. This one reddens, and it reddens anywhere, because the file the fallback finds is the one
/// written three lines up rather than one the machine happened to have.
#[test]
fn an_unarmed_book_ignores_the_process_wide_sentinel_even_when_it_exists() {
    // ⚠ `dir` is BOUND for the whole test: the `TempDir` guard deletes the directory when it drops,
    // and the sentinel this test points `VIKE_HALT_FILE` at lives inside it.
    //
    // This used to be `temp_dir().join(format!("vike-paper-halt-procwide-{pid}"))` followed by a
    // `create_dir_all`, which was wrong twice over (measured on the CI box, 2026-08-25). Nothing ever
    // deleted the directory (the test's own cleanup removed the sentinel FILE and left its parent),
    // so every CI run and every verification-lane run left one behind forever. And a REUSED pid
    // colliding with a directory the box's OTHER test user made (`the CI user` for CI, `the operator` for
    // the verification lanes) makes `create_dir_all` SUCCEED while the `fs::write` below fails
    // PermissionDenied: a live, intermittent flake. `tempfile::TempDir` is unique by construction
    // AND self-deleting, including on the panic path. The measured counts for the same idiom's two
    // siblings are on `crates/vike-paper/tests/paper_halt.rs`'s `sentinel_for`.
    let dir = tempfile::Builder::new()
        .prefix("vike-paper-halt-procwide-")
        .tempdir()
        .expect("temp sentinel dir");
    let path: PathBuf = dir.path().join("HALT");
    std::fs::write(&path, b"").expect("engage the PROCESS-WIDE sentinel");

    // SAFE in edition 2021, and race-free here: this binary contains exactly one test, so no other
    // thread exists to read the environment while it is written. That is the whole reason the file
    // exists separately from `tests/paper_halt.rs`.
    std::env::set_var(HALT_FILE_ENV, &path);
    assert!(
        std::env::var(HALT_FILE_ENV).map(PathBuf::from).as_deref() == Ok(path.as_path()),
        "the test did not actually engage the process-wide sentinel — the assertion below would \
         then pass for the wrong reason, which is exactly the defect this file exists to fix"
    );
    assert!(path.exists(), "and the file it names must be on disk");

    // No `with_halt_path`: the shape every backtest, every unit test and the r7 gate construct.
    let mut client =
        PaperExecutionClient::with_fee_schedule("binance", "BTCUSDT", 0.0, FeeSchedule::Free);
    assert!(
        client.halt_path().is_none(),
        "a constructor armed the book. The r7 gate builds it exactly like this, so its fills would \
         start depending on a file on disk — arming belongs to a MOUNT and to nothing else \
         (`crates/vike-ops/tests/paper_mount_arming_gate.rs` gates the seams)"
    );

    client.submit(&OrderRequest {
        client_order_id: "sim-procwide".into(),
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        side: 1,
        qty: 1.0,
        order_type: "limit".into(),
        price: Some(100.0),
        ..Default::default()
    });

    let mut events = Vec::new();
    while let Some(e) = client.poll_events() {
        events.push(e);
    }
    assert!(
        matches!(events.as_slice(), [Event::OrderSubmitted(_), Event::OrderAccepted(_)]),
        "an UNARMED book must be byte-identical to its pre-sentinel behaviour with the PROCESS-WIDE \
         sentinel engaged — a backtest's fills may never depend on a file on disk. Got: {events:?}"
    );
    // `dir` drops here, taking the sentinel and its directory with it.
}
