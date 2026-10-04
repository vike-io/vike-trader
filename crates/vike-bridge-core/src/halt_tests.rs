use super::*;

/// The precedence, both rungs, with nothing touching the filesystem.
///
/// ⚠ The first assertion is the REGRESSION GUARD for the defect this module was rewritten for:
/// with a project state directory in hand, the default must NOT be `<exe_dir>/HALT`. Restore
/// `exe_dir.join(HALT_FILE)` as the unconditional default and this line goes red.
#[test]
fn path_precedence_state_dir_then_exe_dir() {
    let exe = Path::new("/srv/vike-<unit>/bin");
    let state = Path::new("/srv/vike-<unit>/settings/state");

    // 2. The DEFAULT is the project state dir — the directory the shipped unit grants.
    assert_eq!(
        resolve_halt_path(Some(state), exe),
        PathBuf::from("/srv/vike-<unit>/settings/state/HALT")
    );
    assert_ne!(
        resolve_halt_path(Some(state), exe),
        PathBuf::from("/srv/vike-<unit>/bin/HALT"),
        "the exe directory is read-only under ProtectSystem=strict — defaulting there is the \
             bug this test exists for"
    );

    // 3. …and the exe directory only when no project resolves at all.
    assert_eq!(resolve_halt_path(None, exe), PathBuf::from("/srv/vike-<unit>/bin/HALT"));
}

/// **A DECLARED project is used verbatim — rung 2 does not walk behind the root's back.**
///
/// The end-to-end proof (a `$VIKE_SETTINGS_DIR` that DISAGREES with the working directory, run
/// through the real `vike_boot::boot`) is
/// `crates/vike-bridge-core/tests/halt_default_path.rs`'s
/// `the_settings_dir_override_moves_the_sentinel_off_the_working_directorys_project`. This is
/// the pure half: the declaration reaches `resolve_halt_path` untouched.
#[test]
fn a_declared_project_supplies_rung_two() {
    let exe = Path::new("/srv/vike-<unit>/bin");
    let named = Path::new("/var/lib/vike/settings/state");

    assert_eq!(
        halt_path_for(HaltProject::Declared(Some(named)), exe),
        named.join(HALT_FILE),
        "the root's own boot walk decides rung 2"
    );
    // …and `Declared(None)` is a real answer: the root resolved NO project, so rung 3 applies.
    // Collapsing it into "nobody told me" would silently walk from the working directory here,
    // which is the exact blindness this arm exists to remove.
    assert_eq!(
        halt_path_for(HaltProject::Declared(None), exe),
        exe.join(HALT_FILE),
        "a root that found no project falls to the exe directory, it does not re-walk"
    );
}

/// ⚠ **THE RUNG GATE.** Each rung of the precedence REPORTS ITSELF, and the reported rung is
/// the one the resolved path actually came from.
///
/// Asserted as an agreement between the two, never as a table of expected rungs on its own: the
/// defect being designed out is a report that describes a rung the resolver did not take, and a
/// test that spelled the classification beside the classifier would be satisfied by any two
/// matching copies of the same mistake. `resolve_halt_path` is a projection of
/// `resolve_halt_path_rung`, so this also pins that the projection did not drift.
#[test]
fn every_rung_reports_itself_and_agrees_with_the_path_it_resolved() {
    let exe = Path::new("/srv/vike-<unit>/bin");
    let state = Path::new("/srv/vike-<unit>/settings/state");

    for (dir, want_rung, want_path) in [
        (Some(state), HaltRung::ProjectState, state.join(HALT_FILE)),
        (None, HaltRung::ExeDir, exe.join(HALT_FILE)),
    ] {
        let (path, rung) = resolve_halt_path_rung(dir, exe);
        assert_eq!(rung, want_rung, "wrong rung reported for state={dir:?}");
        assert_eq!(path, want_path, "wrong path for state={dir:?}");
        assert_eq!(
            path,
            resolve_halt_path(dir, exe),
            "`resolve_halt_path` must be a projection of `resolve_halt_path_rung`, not a \
                 second copy of the precedence"
        );
    }
}

/// The rung tokens are a LOG CONTRACT — the `rung` field of the mount-time report is what an
/// operator greps, and `docs/ops/kill-switches.md` names rung 2 and rung 3 — so retiring rung 1
/// (the `VIKE_HALT_FILE` override, decision 0099) must not renumber the survivors.
#[test]
fn the_surviving_rungs_keep_their_numbers() {
    assert!(
        HaltRung::ProjectState.as_str().starts_with("2: "),
        "{}",
        HaltRung::ProjectState.as_str()
    );
    assert!(HaltRung::ExeDir.as_str().starts_with("3: "), "{}", HaltRung::ExeDir.as_str());
}

/// ⚠ **THE REPORT GATE, and the whole point of the change.** A rung-3 sentinel that the process
/// CAN create is its own report — not the cheerful one a correct rung 2 gets.
///
/// This is the container shape. On a systemd box rung 3 announced itself through
/// `halt_path_arming_error` (`ProtectSystem=strict` makes the exe directory a read-only MOUNT,
/// so `touch` returns EROFS), and that alarm is a property of the SANDBOX rather than of the
/// rung: in an image the exe directory is writable, the probe says `None`, and the resolution
/// used to be reported exactly like a correct one. Collapse `ExeDirFallback` into `Resolved`
/// and this goes red — which is the mutation that reproduces the finding.
#[test]
fn a_writable_exe_dir_fallback_is_reported_apart_from_a_correct_resolution() {
    assert_eq!(
        halt_report(None, HaltRung::ExeDir),
        HaltReport::ExeDirFallback,
        "an ARMABLE last-resort sentinel is the container shape: every other check in this \
             module says yes, so this classification is the only thing that can say otherwise"
    );
    assert_eq!(halt_report(None, HaltRung::ProjectState), HaltReport::Resolved);
    // …and an unarmable path outranks the rung, on BOTH, so the reading that already
    // existed on the shipped units cannot have changed.
    for rung in [HaltRung::ProjectState, HaltRung::ExeDir] {
        assert_eq!(halt_report(Some("EROFS"), rung), HaltReport::NotArmable);
    }
}

/// The advisory has to tell an operator the thing they are about to get wrong, so its
/// load-bearing claims are pinned: that the sentinel is beside the BINARY, that the
/// project-relative path they were about to `touch` is not the one being watched, and the fix —
/// which, now that the sentinel has no override, is the settings directory the daemon declares its
/// state directory from.
///
/// Keyed on the substrings and not on the whole message, deliberately — the wording will be
/// improved, and a gate over the full prose is one that gets loosened until deleting the line
/// stops reddening it.
#[test]
fn the_exe_dir_advisory_names_both_the_wrong_path_and_the_right_one() {
    assert!(
        EXE_DIR_FALLBACK_ADVISORY.contains("BINARY"),
        "the advisory must say WHERE the sentinel actually is"
    );
    assert!(
        EXE_DIR_FALLBACK_ADVISORY.contains("<project>/settings/state/HALT"),
        "the advisory must name the path the operator was about to `touch` in vain — that \
             `touch` is the action this line exists to stop"
    );
    assert!(
        EXE_DIR_FALLBACK_ADVISORY.contains("VIKE_SETTINGS_DIR"),
        "…and the fix, which needs no rebuild"
    );
    assert!(
        !EXE_DIR_FALLBACK_ADVISORY.contains("VIKE_HALT_FILE"),
        "the advisory may not send an operator to a variable the daemon refuses to start with"
    );
}

/// A sentinel file that exists for the length of a test and is removed on the unwinding path too
/// (a trailing `remove_file` cleans up only when the test PASSES). This crate has no `tempfile`
/// dev-dependency, so the guard is hand-rolled — `tests/halt_declaration.rs`'s `Scratch`, smaller.
struct Sentinel(PathBuf);

impl Sentinel {
    fn new(tag: &str) -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        Self(std::env::temp_dir().join(format!("vike-sentinel-engaged-{tag}-{nanos}")))
    }
}

impl Drop for Sentinel {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// **The one `exists()` the enforcing clients share**: a handed path is engaged exactly while its
/// file exists, re-read each call (so `rm` resumes trading and nothing latches).
#[test]
fn a_handed_sentinel_is_engaged_exactly_while_its_file_exists() {
    let sentinel = Sentinel::new("exists");
    assert!(!sentinel_engaged(Some(sentinel.0.as_path())), "no file, not engaged");
    std::fs::write(&sentinel.0, b"").expect("touch the sentinel");
    assert!(sentinel_engaged(Some(sentinel.0.as_path())), "the file exists, engaged");
    std::fs::remove_file(&sentinel.0).expect("rm the sentinel");
    assert!(!sentinel_engaged(Some(sentinel.0.as_path())), "removed again: trading resumes");
}

/// **A client handed no path — or the empty one `ProcessFacts::default()` carries — watches
/// nothing**, and in particular does not fall back to a process-wide sentinel (decision 0099). The
/// empty path is called out because `Path::new("").exists()` is false for a reason that has nothing
/// to do with the switch, and this arm is what makes that visible instead of looking armed.
#[test]
fn a_client_handed_no_sentinel_watches_nothing() {
    assert!(!sentinel_engaged(None));
    assert!(!sentinel_engaged(Some(Path::new(""))));
}
