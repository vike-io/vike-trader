use super::*;

/// The precedence, all three rungs, with nothing touching the filesystem.
///
/// ⚠ The middle assertion is the REGRESSION GUARD for the defect this module was rewritten for:
/// with a project state directory in hand, the default must NOT be `<exe_dir>/HALT`. Restore
/// `exe_dir.join(HALT_FILE)` as the unconditional default and this line goes red.
#[test]
fn path_precedence_env_then_state_dir_then_exe_dir() {
    let exe = Path::new("/srv/vike-<unit>/bin");
    let state = Path::new("/srv/vike-<unit>/settings/state");

    // 1. VIKE_HALT_FILE wins outright.
    assert_eq!(
        resolve_halt_path(Some("/run/vike/HALT"), Some(state), exe),
        PathBuf::from("/run/vike/HALT")
    );

    // 2. The DEFAULT is the project state dir — the directory the shipped unit grants.
    assert_eq!(
        resolve_halt_path(None, Some(state), exe),
        PathBuf::from("/srv/vike-<unit>/settings/state/HALT")
    );
    assert_ne!(
        resolve_halt_path(None, Some(state), exe),
        PathBuf::from("/srv/vike-<unit>/bin/HALT"),
        "the exe directory is read-only under ProtectSystem=strict — defaulting there is the \
             bug this test exists for"
    );

    // 3. …and the exe directory only when no project resolves at all.
    assert_eq!(resolve_halt_path(None, None, exe), PathBuf::from("/srv/vike-<unit>/bin/HALT"));
}

/// **A DECLARED project is used verbatim — rung 2 does not walk behind the root's back.**
///
/// The end-to-end proof (a `$VIKE_SETTINGS_DIR` that DISAGREES with the working directory, run
/// through the real `vike_boot::boot`) is
/// `crates/vike-bridge-core/tests/halt_default_path.rs`'s
/// `the_settings_dir_override_moves_the_sentinel_off_the_working_directorys_project`. This is
/// the pure half: the declaration reaches `resolve_halt_path` untouched, and the override
/// outranks it.
#[test]
fn a_declared_project_supplies_rung_two_and_the_env_override_still_outranks_it() {
    let exe = Path::new("/srv/vike-<unit>/bin");
    let named = Path::new("/var/lib/vike/settings/state");

    assert_eq!(
        halt_path_for(None, HaltProject::Declared(Some(named)), exe),
        named.join(HALT_FILE),
        "the root's own boot walk decides rung 2"
    );
    assert_eq!(
        halt_path_for(Some("/run/vike/HALT"), HaltProject::Declared(Some(named)), exe),
        PathBuf::from("/run/vike/HALT"),
        "VIKE_HALT_FILE still wins outright — the three-rung precedence is unchanged"
    );
    // …and `Declared(None)` is a real answer: the root resolved NO project, so rung 3 applies.
    // Collapsing it into "nobody told me" would silently walk from the working directory here,
    // which is the exact blindness this arm exists to remove.
    assert_eq!(
        halt_path_for(None, HaltProject::Declared(None), exe),
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

    for (env, dir, want_rung, want_path) in [
        (Some("/run/vike/HALT"), Some(state), HaltRung::Env, PathBuf::from("/run/vike/HALT")),
        (None, Some(state), HaltRung::ProjectState, state.join(HALT_FILE)),
        (None, None, HaltRung::ExeDir, exe.join(HALT_FILE)),
        // A blank override is rung 1's ABSENCE, not rung 1 — it must report the rung that
        // actually answered, or an operator debugging an empty `Environment=` line is told the
        // variable took effect.
        (Some("   "), Some(state), HaltRung::ProjectState, state.join(HALT_FILE)),
        (Some(""), None, HaltRung::ExeDir, exe.join(HALT_FILE)),
    ] {
        let (path, rung) = resolve_halt_path_rung(env, dir, exe);
        assert_eq!(rung, want_rung, "wrong rung reported for env={env:?} state={dir:?}");
        assert_eq!(path, want_path, "wrong path for env={env:?} state={dir:?}");
        assert_eq!(
            path,
            resolve_halt_path(env, dir, exe),
            "`resolve_halt_path` must be a projection of `resolve_halt_path_rung`, not a \
                 second copy of the precedence"
        );
    }
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
    for ok in [HaltRung::Env, HaltRung::ProjectState] {
        assert_eq!(halt_report(None, ok), HaltReport::Resolved);
    }
    // …and an unarmable path outranks the rung, on ALL THREE, so the reading that already
    // existed on the shipped units cannot have changed.
    for rung in [HaltRung::Env, HaltRung::ProjectState, HaltRung::ExeDir] {
        assert_eq!(halt_report(Some("EROFS"), rung), HaltReport::NotArmable);
    }
}

/// The advisory has to tell an operator the thing they are about to get wrong, so its two
/// load-bearing claims are pinned: that the sentinel is beside the BINARY, and that the
/// project-relative path they were about to `touch` is not the one being watched.
///
/// Keyed on the two substrings and not on the whole message, deliberately — the wording will be
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
        EXE_DIR_FALLBACK_ADVISORY.contains(HALT_FILE_ENV),
        "…and the fix, which needs no rebuild"
    );
}

/// A blank override falls THROUGH rather than resolving the sentinel to `""` — a path that can
/// neither exist nor be created, i.e. the same silent-inoperable switch by another route.
#[test]
fn a_blank_env_override_falls_through_to_the_default() {
    let exe = Path::new("/srv/vike-<unit>/bin");
    let state = Path::new("/srv/vike-<unit>/settings/state");
    for blank in [Some(""), Some("   "), Some("\t")] {
        assert_eq!(
            resolve_halt_path(blank, Some(state), exe),
            PathBuf::from("/srv/vike-<unit>/settings/state/HALT"),
            "an empty Environment=VIKE_HALT_FILE= line must not disarm the switch"
        );
    }
}
