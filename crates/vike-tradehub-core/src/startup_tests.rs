//! The disk leg of the startup orchestrator: a real number for a real directory, the dedup by path
//! and the floors the policy carries. (The venue legs — the probe bounds, the clock read on a
//! start's inputs, the credential retries — are
//! `crates/vike-gatewayhub/src/trade/adapters/startup.rs`'s tests.)

use super::*;
use std::path::PathBuf;

/// THE DISK LEG, armed: a real number for a real directory, an ERROR (rendered WARN, never a
/// fake PASS) for a missing one.
///
/// ⚠ Windows takes the declared-gap path (honest, not skipped): the CONTRACT is asserted on both
/// platforms, the measurement only where it can be taken. Unseen: `f_bavail` vs `f_bfree` (equal
/// on an unreserved filesystem), a documented judgement in [`free_space_bytes`], not pinned.
#[test]
fn the_disk_probe_measures_a_real_directory_and_declares_a_missing_one() {
    let here = std::env::temp_dir();
    let measured = free_space_bytes(&here);
    // ⚠ `#[cfg]`-selected, not `if cfg!(…)`: that fires clippy's `assertions_on_constants`
    // under `-D warnings` and lets the WRONG arm compile-check into nothing.
    #[cfg(unix)]
    {
        let free = measured.expect("unix must MEASURE, not declare");
        assert!(free > 0, "a writable temp dir reporting ZERO free bytes is not a reading");
        // A missing directory is an ERROR, so `check_disk_headroom` WARNs naming it.
        let missing = here.join("vike-preflight-no-such-dir-8f3a1c");
        assert!(free_space_bytes(&missing).is_err(), "an absent directory cannot be measured");
    }
    #[cfg(not(unix))]
    {
        let reason = measured.expect_err("this platform cannot measure, so it must DECLARE");
        assert!(!reason.is_empty(), "a declared gap must carry its reason");
    }
}

/// Deduplicated by PATH, not label: a journal inside the store root is ONE filesystem; two rows
/// would double every finding.
#[test]
fn the_disk_dirs_are_deduplicated_by_path() {
    let p = PathBuf::from("/srv/vike/data");
    let out = disk_dirs(&[
        ("journal".to_string(), p.clone()),
        ("hist-store".to_string(), p.clone()),
        ("other".to_string(), PathBuf::from("/srv/vike/other")),
    ]);
    assert_eq!(out.len(), 2, "{out:?}");
    assert_eq!(out[0].0, "journal", "the FIRST label wins, so the order is deterministic");
}

/// **The disk leg's floors are the policy's rows, else the compiled pair**: a caller that threads no
/// policy, and a policy that carries no rows, run the preflight they always ran; a policy with the
/// rows runs the floors they state. (`run_startup_preflight` builds its `PreflightConfig` from this,
/// pinned by the next test.)
#[test]
fn the_disk_floors_are_the_policy_rows_else_the_compiled_pair() {
    use crate::preflight::{DEFAULT_DISK_FAIL_BYTES, DEFAULT_DISK_WARN_BYTES, DiskThresholds};
    let compiled =
        DiskThresholds { warn_bytes: DEFAULT_DISK_WARN_BYTES, fail_bytes: DEFAULT_DISK_FAIL_BYTES };
    assert_eq!(disk_floors(None), compiled, "no policy");
    assert_eq!(disk_floors(Some(&crate::AdapterPolicy::default())), compiled, "no rows");

    let rows = DiskThresholds::from_rows(Some(8_192), Some(2_048));
    let policy = crate::AdapterPolicy { preflight_disk: rows, ..crate::AdapterPolicy::default() };
    assert_eq!(disk_floors(Some(&policy)), rows, "the rows");
    assert_ne!(rows, compiled, "premise: the rows differ from the compiled pair");
}

/// `run_startup_preflight` fills the disk leg's `PreflightConfig` from [`disk_floors`] (a text pin:
/// the function builds real probes and dials, so no test can read its `PreflightConfig` back).
/// Whitespace-insensitive; it can fail — a literal figure, or the compiled default alone, is refused.
#[test]
fn the_startup_preflight_hands_the_disk_floors_to_the_disk_leg() {
    const RUN: &str = include_str!("startup/run.rs");
    let canon = |s: &str| s.chars().filter(|c| !c.is_whitespace()).collect::<String>();
    let assigns = |src: &str| {
        let src = canon(src);
        src.contains("letdisk=disk_floors(policy);")
            && src.contains("disk_warn_bytes:disk.warn_bytes,")
            && src.contains("disk_fail_bytes:disk.fail_bytes,")
    };
    assert!(assigns(RUN), "`run_startup_preflight` must build its disk floors from `disk_floors`");
    for wrong in [
        "let disk = disk_floors(policy); disk_warn_bytes: DEFAULT_DISK_WARN_BYTES, disk_fail_bytes: disk.fail_bytes,",
        "let disk = DiskThresholds::default(); disk_warn_bytes: disk.warn_bytes, disk_fail_bytes: disk.fail_bytes,",
        "",
    ] {
        assert!(!assigns(wrong), "{wrong:?}");
    }
}
