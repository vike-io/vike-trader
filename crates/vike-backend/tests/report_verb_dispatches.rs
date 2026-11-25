//! End-to-end gate for `vike-backend report`, the RETIRED journal door (owner, 2026-11-18): every
//! invocation — the old `--journal DIR` line, a bare call, `--help`, `--version` — is refused on the
//! usage rung with the ONE line `vike-cli report --node` answers too, and stdout stays empty.
//!
//! ⚠ Whole-file gated on the `tearsheet` feature, and the gate is load-bearing:
//! `crates/vike-backend/src/main.rs`'s `TOOLS` carries the `report` row only under it, so a
//! feature-off binary answers the verb with its tool list and exit 2. The crate's self
//! dev-dependency turns `full`, and so `tearsheet`, on in every test build, which is what runs it.
//!
//! Until the retirement this file journaled two round-trips through a real `spawn_core` session and
//! asserted the tearsheet the verb folded from them; the fold left with the door. It was
//! `tests/tearsheet_cli.rs` until 2026-11-18, when it took the verb's name like its
//! `*_verb_dispatches.rs` siblings; the feature keeps the stored word `tearsheet`.
#![cfg(feature = "tearsheet")]

#[test]
fn every_report_invocation_is_refused_naming_the_account_spelling() {
    let line = format!("vike-backend report: {}", vike_cli::cmd::report::JOURNAL_DOORS_RETIRED);
    for args in [
        vec!["--journal", "/nonexistent/journal", "--json"],
        vec!["--journal", "/nonexistent/journal", "--datahub", "--mtm"],
        vec![],
        vec!["--help"],
        vec!["--version"],
    ] {
        let out = std::process::Command::new(env!("CARGO_BIN_EXE_vike-backend"))
            .arg("report")
            .args(&args)
            .output()
            .expect("run vike-backend report");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_eq!(out.status.code(), Some(2), "{args:?}: the usage rung\n{stderr}");
        assert_eq!(stderr.trim_end(), line, "{args:?}: one line, the shared one");
        assert!(stderr.contains("report account:<venue>/<LABEL>"), "{args:?}: {stderr}");
        assert!(out.stdout.is_empty(), "{args:?}: nothing on stdout");
    }
}
