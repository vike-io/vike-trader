//! The `backtest == paper` half of the HALT arming, proven WITHOUT depending on what is lying
//! around on the box that runs it.
//!
//! # Why this is its own file, and its own test binary
//!
//! `tests/paper_halt.rs`'s `an_unarmed_book_ignores_a_halt_file_that_exists` states the property
//! this crate's opt-in arming exists for: a book no mount armed reads NO sentinel, so a stray `HALT`
//! file cannot silently change `crates/vike-sim/tests/r7_gate.rs`'s bit-for-bit fills. The
//! property is right. **The proof was not** — the mutation it named (make the `None` arm fall back
//! to the process-wide path) only reddens on a box where that resolved path happens to EXIST, and it
//! resolves to `<project>/settings/state/HALT` or `<exe_dir>/HALT`, neither of which exists on a CI
//! runner or on a clean checkout. So the named mutation shipped GREEN everywhere the gate actually
//! runs: a mutation proof that holds only on the author's machine is not a proof.
//!
//! This file removes the dependency on the box by supplying the project itself. It builds a
//! synthetic project (`<tmp>/settings/state/HALT`, the file already on disk) and runs this very test
//! in a CHILD whose WORKING DIRECTORY is that project. A test binary declares no composition root's
//! project, so the process-wide sentinel's default — `crates/vike-bridge-core/src/halt.rs`'s
//! `halt_path_from_env`, which walks up from the working directory — lands on exactly that file
//! whatever else the machine has. An unarmed book must still accept. The mutation now reddens on
//! every box, because the file it would find is one this test put there.
//!
//! ⚠ **This used to supply `VIKE_HALT_FILE` instead**, the FIRST and highest-precedence rung of that
//! resolver, which is why a fallback landed on it "whatever else the machine had". Decision 0099
//! retired that variable, so the proof moved to the rung that remains: the project the working
//! directory sits in. It is the weaker of the two in one respect and the same in the one that
//! matters — a fallback that consults the process-wide sentinel at all lands on the engaged file.
//!
//! ⚠ **The project is supplied to a CHILD process, never made the working directory of this one.**
//! `std::env::set_current_dir` is process-global and cargo runs a binary's tests as threads in ONE
//! process, and the sibling `std::env::set_var` is an `unsafe fn` since edition 2024 that this
//! workspace forbids outright. The test re-executes ITS OWN binary with `Command::current_dir`
//! filtered to itself (the idiom `crates/vike-mount/tests/common/mod.rs` uses for the same reason)
//! and asserts the child's exit status; the child branch — recognised by finding a sentinel under
//! ITS working directory, which the parent's (a crate directory) never has — runs the real
//! assertions. The file stays separate for its documentary value and so the re-exec filter is
//! unambiguous.

use std::path::PathBuf;

use vike_exec::ExecutionClient;
use vike_model::events::Event;
use vike_model::{FeeSchedule, OrderRequest};
use vike_paper::PaperExecutionClient;

/// Where the synthetic project's sentinel is, relative to the project root — the process-wide
/// default's own spelling (`<project>/settings/state/HALT`).
const SENTINEL_REL: [&str; 3] = ["settings", "state", "HALT"];

fn sentinel_under(root: &std::path::Path) -> PathBuf {
    SENTINEL_REL.iter().fold(root.to_path_buf(), |p, part| p.join(part))
}

/// A book that no mount armed ignores the PROCESS-WIDE sentinel, even when that sentinel exists —
/// and this test is what makes it exist, on any box.
///
/// ⚠ **MEASURED on the CI box (2026-08-08), which is not the author's box — and the measurement is also
/// the proof that the OLD one was worthless.** Giving `PaperExecutionClient`'s `halt_engaged` a
/// `None`-arm fallback that resolves the process-wide sentinel (then, in that run, through
/// `VIKE_HALT_FILE`) produced:
///
/// ```console
/// PASS vike-paper::paper_halt an_unarmed_book_ignores_a_halt_file_that_exists
/// PASS vike-paper::paper_halt an_unarmed_book_modifies_under_a_halt_file_that_exists
/// FAIL vike-paper::paper_halt_process_wide an_unarmed_book_ignores_the_process_wide_sentinel_even_when_it_exists
/// ```
///
/// The two tests that NAMED that mutation shipped GREEN through it, because on that runner
/// the variable was unset and `settings/state/HALT` did not exist — exactly the environment CI
/// has. This one reddens, and it reddens anywhere, because the file the fallback finds is the one
/// written three lines up rather than one the machine happened to have.
#[test]
fn an_unarmed_book_ignores_the_process_wide_sentinel_even_when_it_exists() {
    let here = std::env::current_dir().expect("the working directory");
    let path = sentinel_under(&here);
    if !path.is_file() {
        // PARENT: no synthetic project around us. Create one with the sentinel ENGAGED and re-run
        // this exact test in a CHILD whose working directory it is — `Command::current_dir` on the
        // child, never `std::env::set_current_dir` here.
        //
        // ⚠ `dir` is BOUND for the whole of this arm: the `TempDir` guard deletes the project when
        // it drops, and the sentinel the child is looking at lives inside it. `tempfile` rather than
        // a pid-named `temp_dir()` child, measured on the CI box 2026-08-25: nothing ever deleted the
        // pid-named parent, and a REUSED pid colliding with a directory the box's OTHER test user
        // made (`the CI user` for CI, `the operator` for the verification lanes) let `create_dir_all`
        // succeed while the write failed PermissionDenied — a live flake.
        let dir = tempfile::Builder::new()
            .prefix("vike-paper-halt-procwide-")
            .tempdir()
            .expect("temp project dir");
        let path = sentinel_under(dir.path());
        std::fs::create_dir_all(path.parent().expect("the sentinel's directory"))
            .expect("the project's state directory");
        std::fs::write(&path, b"").expect("engage the PROCESS-WIDE sentinel");

        let exe = std::env::current_exe().expect("this test binary's own path");
        let status = std::process::Command::new(exe)
            .current_dir(dir.path())
            .args([
                "--exact",
                "an_unarmed_book_ignores_the_process_wide_sentinel_even_when_it_exists",
                "--nocapture",
            ])
            .status()
            .expect("re-exec this test binary");
        assert!(
            status.success(),
            "the CHILD run — the one with the process-wide sentinel engaged — failed ({status}); \
             its own assertion message is above"
        );
        return;
        // `dir` drops here, taking the sentinel and its project with it.
    }

    // CHILD: the parent made this directory a project and engaged its sentinel. Prove the wiring
    // before proving the property, or the assertion below passes for the wrong reason — which is
    // exactly the defect this file exists to fix.
    assert!(path.exists(), "the parent's sentinel must be on disk: {}", path.display());

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
}
