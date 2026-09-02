//! The HALT sentinel's DEFAULT path must land somewhere an operator can actually create it.
//!
//! # The defect this file exists for
//!
//! The sentinel's built-in default was `<exe_dir>/HALT`. Every shipped unit sets
//! `ProtectSystem=strict`, which mounts the whole filesystem read-only except what
//! `ReadWritePaths=` grants — and no unit grants the exe directory, deliberately (a daemon that can
//! write the binary it is running is a worse problem than any it would solve). So on a deployed box
//! `touch <project>/bin/HALT` returns `EROFS`: the ONE kill switch that survives a wedged runtime
//! could not be armed at all.
//!
//! #1143 fixed the DEPLOYED path — `deploy/vike-tradehub.service` gained
//! `Environment=VIKE_HALT_FILE=<project>/settings/state/HALT` — and left the DEFAULT pointing at the
//! read-only directory. A unit that forgets that line therefore had no working kill switch and no
//! error anywhere, which is the worst shape a safety mechanism can take: **an unarmed kill switch is
//! indistinguishable from an armed one until the moment it is needed.**
//!
//! # What is asserted, and why it is a regression test rather than a description
//!
//! [`the_default_is_the_project_state_dir_not_the_read_only_exe_dir`] drives the WHOLE resolution
//! (`crates/vike-bridge-core/src/halt.rs`'s `halt_path_for`, project walk included) over a synthetic
//! deployment tree and asserts the answer is under `settings/state`. Restore
//! `exe_dir.join(HALT_FILE)` as the unconditional default and it goes red — verified by making
//! exactly that mutation while writing this file.
//!
//! [`the_default_lands_where_the_shipped_unit_grants_write`] reads
//! `deploy/vike-tradehub.service` and asserts the resolved directory is one the unit's
//! `ReadWritePaths=` names. That is the half a pure unit test cannot cover: the code and the sandbox
//! have to agree, and they are edited in different files by different people. A change to either
//! without the other reddens here.
//!
//! [`the_read_only_exe_dir_really_does_refuse_a_touch`] is the negative control, on unix: it makes a
//! directory unwritable, proves creating the sentinel there fails, and proves creating it at the new
//! default succeeds. Without it the two tests above would be asserting a preference rather than a
//! fix. It SKIPS when the negative control does not hold (running as root, where mode bits do not
//! bind) rather than passing vacuously.
//!
//! # …and the rung-3 case the two above cannot reach: a WRITABLE exe directory
//!
//! [`a_writable_exe_dir_with_no_project_is_the_shape_no_other_check_objects_to`] is the third
//! defect's regression test, and the reason it is not covered by either test above is that both of
//! them assert about rung 2. Rung 3 was never silent — the resolution has always been logged — but
//! the thing that made it LOUD was `halt_path_arming_error` returning `EROFS`, which is a property
//! of `ProtectSystem=strict` and not of the rung. Take the sandbox away (an OCI image, where the
//! exe directory is writable, `<project>` is a bind MOUNT and the working directory defaults to
//! `/`) and the probe says "armable", the report reads like a correct rung 2, and an operator's
//! `touch <project>/settings/state/HALT` on the host arms a file the process never opens.
//!
//! That test asserts the SILENCE of the old alarm as a precondition, which is what makes the new
//! classification necessary rather than decorative.
//!
//! # …and WHICH project that default means
//!
//! [`the_settings_dir_override_moves_the_sentinel_off_the_working_directorys_project`] is the second
//! defect's regression test, and it is deliberately end-to-end through the REAL
//! `crates/vike-boot/src/lib.rs`'s `boot`: rung 2 used to walk from the working directory with the
//! `$VIKE_SETTINGS_DIR`-BLIND resolver, so an operator who named the settings directory outright
//! still had the kill switch decided by `WorkingDirectory=`. Two synthetic deployment trees make the
//! two answers DISAGREE, which is the only configuration in which the bug is visible at all — on
//! the CI box the override and the working directory name the same project, so the sentinel resolved
//! correctly there by coincidence rather than by honouring anything.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use vike_bridge_core::halt::{
    halt_path_arming_error, halt_path_for, halt_path_for_rung, halt_report, resolve_halt_path,
    HaltProject, HaltReport, HaltRung, HALT_FILE,
};

/// A private scratch directory. `vike-bridge-core` has no `tempfile` dev-dependency and this change
/// adds no dependency, so — like `vike_model::state_path`'s own tests — these make their own.
struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let p = std::env::temp_dir().join(format!("vike-halt-{tag}-{nanos}"));
        std::fs::create_dir_all(&p).expect("scratch dir");
        Self(p)
    }
    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        // Best effort: a unix test below chmods a directory read-only, so restore it first.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let bin = self.0.join("opt-vike").join("bin");
            if bin.is_dir() {
                let _ = std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755));
            }
        }
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Build the DEPLOYED shape: a binary in `bin/`, a `settings/state/` beside it, and **no
/// `Cargo.toml` anywhere** — which is what makes `settings/` the project marker
/// (`vike_model::state_path`'s `project_settings_dir`). Returns `(working_dir, exe_dir, state_dir)`.
fn deployment_tree(scratch: &Scratch) -> (PathBuf, PathBuf, PathBuf) {
    // The name the unix negative control's `Drop` restores permissions on — see [`Scratch`].
    deployment_tree_named(scratch, "opt-vike")
}

/// [`deployment_tree`] under a chosen sub-directory, so ONE scratch can hold TWO unrelated projects
/// — which is what makes the settings-dir override's answer differ from the working directory's.
fn deployment_tree_named(scratch: &Scratch, name: &str) -> (PathBuf, PathBuf, PathBuf) {
    let root = scratch.path().join(name);
    let bin = root.join("bin");
    let state = root.join("settings").join("state");
    std::fs::create_dir_all(&bin).unwrap();
    std::fs::create_dir_all(&state).unwrap();
    (root, bin, state)
}

/// ⚠ **THE REGRESSION GUARD.** On the deployed shape the default sentinel is
/// `<project>/settings/state/HALT`, and it is NOT beside the binary.
#[test]
fn the_default_is_the_project_state_dir_not_the_read_only_exe_dir() {
    let scratch = Scratch::new("default");
    let (cwd, exe_dir, state) = deployment_tree(&scratch);

    let resolved = halt_path_for(None, HaltProject::WalkFrom(&cwd), &exe_dir);
    assert_eq!(
        resolved,
        state.join(HALT_FILE),
        "the built-in default must resolve into the project's state directory — the one place a \
         deployment already has, already grants write to, and does not rebuild on `cargo clean`"
    );
    assert_ne!(
        resolved,
        exe_dir.join(HALT_FILE),
        "defaulting beside the BINARY is the defect: ProtectSystem=strict makes that directory \
         read-only, so `touch` returns EROFS and the kill switch cannot be armed"
    );

    // …and from anywhere below the working directory, since a daemon may be started deeper.
    let deeper = cwd.join("bin");
    assert_eq!(
        halt_path_for(None, HaltProject::WalkFrom(&deeper), &exe_dir),
        state.join(HALT_FILE)
    );

    // The override still wins outright — the units set it, and this must not have changed.
    let pinned = scratch.path().join("elsewhere").join("HALT");
    assert_eq!(halt_path_for(pinned.to_str(), HaltProject::WalkFrom(&cwd), &exe_dir), pinned);
}

/// ⚠ **THE SECOND REGRESSION GUARD: `$VIKE_SETTINGS_DIR` moves the sentinel.**
///
/// Rung 2's project is the composition root's own boot walk, not a second `$VIKE_SETTINGS_DIR`-blind
/// one of the sentinel's. Driven through the REAL `vike_boot::boot` — the same call
/// `crates/vike-tradehub/src/tradehub_cli.rs`'s `resolve_settings` makes — because the property being
/// asserted spans two crates: the boot has to HONOUR the override and the sentinel has to USE the
/// boot's answer, and a test that resolved the directory for itself would prove only the second.
///
/// The two trees are what makes it mean anything. With one project, the walk and the override give
/// the same answer and the assertion holds under the bug — which is precisely the shape the CI box is in
/// (`WorkingDirectory=` equals the overridden project), and why the defect was LATENT there rather
/// than live.
///
/// ⚠ It reaches no process environment: `BootSpec::env` is a caller-supplied map, so the override is
/// passed as data. This workspace does not `set_var` under threads.
#[test]
fn the_settings_dir_override_moves_the_sentinel_off_the_working_directorys_project() {
    let scratch = Scratch::new("override");
    let (cwd, exe_dir, cwd_state) = deployment_tree_named(&scratch, "cwd-project");
    let (named_root, _named_bin, named_state) = deployment_tree_named(&scratch, "named-project");

    // The PRECONDITION: the blind walk answers with the WORKING DIRECTORY's project, so the two
    // rungs genuinely disagree and nothing below can pass by the two being equal.
    assert_eq!(
        halt_path_for(None, HaltProject::WalkFrom(&cwd), &exe_dir),
        cwd_state.join(HALT_FILE),
        "precondition: without a declaration the sentinel follows the working directory"
    );

    let env = HashMap::from([(
        "VIKE_SETTINGS_DIR".to_string(),
        named_root.join("settings").display().to_string(),
    )]);
    let booted = vike_boot::boot(&vike_boot::BootSpec {
        env: &env,
        cwd: Some(&cwd),
        identity: vike_boot::Identity { name: "vike-halt-test", version: "0.0.0" },
        removed_env: vike_boot::RemovedEnv::Refuse,
        settings: vike_boot::SettingsLoad::Load,
        credentials: vike_boot::Credentials::Deferred("this test opens no credential store"),
        log_home: vike_boot::LogHome::UnderSettings,
        disclosure: vike_boot::Disclosure::Skip("nothing here reads the startup disclosure"),
    })
    .expect("a synthetic deployment tree boots");
    assert_eq!(
        booted.state_dir.as_deref(),
        Some(named_state.as_path()),
        "precondition: the boot honours $VIKE_SETTINGS_DIR (crates/vike-boot/src/lib.rs's `boot`)"
    );

    // THE PROPERTY: the sentinel follows the boot, not the working directory.
    let declared = HaltProject::Declared(booted.state_dir.as_deref());
    let resolved = halt_path_for(None, declared, &exe_dir);
    assert_eq!(
        resolved,
        named_state.join(HALT_FILE),
        "the kill switch's default must land under the settings directory the operator NAMED"
    );
    assert_ne!(
        resolved,
        cwd_state.join(HALT_FILE),
        "resolving off the working directory is the defect: `$VIKE_SETTINGS_DIR` exists to make \
         the answer independent of `WorkingDirectory=`, and the kill switch was the one path still \
         deciding it from there"
    );

    // …and rung 1 still outranks BOTH. The shipped units set it, and this change may not have
    // reordered the precedence — only which project rung 2 means.
    let pinned = scratch.path().join("pinned").join("HALT");
    assert_eq!(halt_path_for(pinned.to_str(), declared, &exe_dir), pinned);
}

/// The exe directory is the LAST RESORT, not the first choice: it answers only when no project
/// marker exists at all. Pinned so "make the default safe" cannot be read as "delete the fallback"
/// — a binary run from a directory with no project above it still needs ONE predictable answer.
#[test]
fn the_exe_dir_answers_only_when_no_project_resolves() {
    let exe = Path::new("/srv/vike-<unit>/bin");
    assert_eq!(resolve_halt_path(None, None, exe), exe.join(HALT_FILE));
    assert_eq!(
        resolve_halt_path(None, Some(Path::new("/srv/vike-<unit>/settings/state")), exe),
        Path::new("/srv/vike-<unit>/settings/state").join(HALT_FILE),
        "a resolvable project state directory outranks it"
    );
}

/// ⚠ **THE CONTAINER SHAPE: rung 3 with a WRITABLE exe directory, where every other check says
/// yes.**
///
/// This is the test that proves the finding rather than the classification. On a systemd box rung 3
/// announced itself through [`halt_path_arming_error`] — `ProtectSystem=strict` makes the exe
/// directory a read-only MOUNT, `touch` returns `EROFS`, and the resolution is reported at `error`.
/// That alarm belongs to the SANDBOX, not to the rung. In an OCI image the exe directory is
/// ordinarily writable and `<project>` is a bind MOUNT, and Docker's default working directory is
/// `/` — which is exactly "no project resolves". So the probe answers `None`, and before
/// `HaltReport::ExeDirFallback` existed the report was indistinguishable from a correct rung 2.
///
/// The three assertions are the three steps of that argument, in order: the rung is the last
/// resort, the OLD alarm is genuinely SILENT here (the half that makes the new one necessary), and
/// the classification says so anyway. Collapse `ExeDirFallback` into `Resolved` and the third goes
/// red while the first two stay green — which is the finding, reproduced.
#[test]
fn a_writable_exe_dir_with_no_project_is_the_shape_no_other_check_objects_to() {
    let scratch = Scratch::new("container");
    // The image: a binary, and NO project marker above it. `deployment_tree` deliberately builds a
    // `settings/`, so this one is built by hand — the absence is the whole precondition.
    let exe_dir = scratch.path().join("usr-local-bin");
    std::fs::create_dir_all(&exe_dir).unwrap();
    // Docker's default working directory. Any directory with no `settings/` or `Cargo.toml` above
    // it would do; what matters is that the walk finds nothing.
    let cwd = scratch.path().join("container-root");
    std::fs::create_dir_all(&cwd).unwrap();

    let (resolved, rung) = halt_path_for_rung(None, HaltProject::WalkFrom(&cwd), &exe_dir);

    // ⚠ The skip guard keys on the resolved PATH, never on the rung, and the difference is a
    // vacuous pass. "Did a project resolve above the scratch directory" is a fact about the WALK
    // and a genuine precondition — a developer whose temp directory sits under a checkout can
    // prove nothing here. The RUNG is the thing under test, so a build that mis-classifies it must
    // FAIL this test rather than skip it. Guarding on `rung != ExeDir` did exactly that: verified
    // by mutation while writing this file (returning `ProjectState` from the last-resort branch of
    // `resolve_halt_path_rung` made this test skip and pass, while the unit test went red).
    if resolved != exe_dir.join(HALT_FILE) {
        eprintln!(
            "SKIPPED: {} has a project marker above it, so the walk resolved {resolved:?} — the \
             precondition (no project resolves) does not hold here",
            cwd.display()
        );
        return;
    }

    // 1. The last resort answered, and it is beside the BINARY — inside the image.
    assert_eq!(
        rung,
        HaltRung::ExeDir,
        "the walk found no project and the path is the exe directory's, so the reported rung must \
         be the last resort — a report naming any other rung is describing a resolution that did \
         not happen"
    );

    // 2. …and the alarm that catches this on a systemd box is SILENT, because the directory is
    //    writable. This is the half that makes the rung report necessary rather than decorative.
    assert_eq!(
        halt_path_arming_error(&resolved),
        None,
        "precondition: an image's exe directory is writable, so the EROFS report that made rung 3 \
         loud under ProtectSystem=strict does not fire here. If this ever asserts otherwise the \
         finding has changed shape."
    );

    // 3. So the CLASSIFICATION is the only thing left that can distinguish this from a correct
    //    rung-2 resolution — and it does.
    assert_eq!(
        halt_report(halt_path_arming_error(&resolved).as_deref(), rung),
        HaltReport::ExeDirFallback,
        "a writable last-resort sentinel must not be reported like a correct one: the operator's \
         `touch` lands on the bind MOUNT while this process watches the IMAGE"
    );

    // …and the CONTRAST, in the same scratch: a real project resolves rung 2 and reports as
    // ordinary. Without this the assertions above would hold for a classifier that answered
    // `ExeDirFallback` unconditionally.
    let (project_cwd, project_exe, project_state) = deployment_tree(&scratch);
    let (deployed, deployed_rung) =
        halt_path_for_rung(None, HaltProject::WalkFrom(&project_cwd), &project_exe);
    assert_eq!(deployed, project_state.join(HALT_FILE));
    assert_eq!(deployed_rung, HaltRung::ProjectState);
    assert_eq!(
        halt_report(halt_path_arming_error(&deployed).as_deref(), deployed_rung),
        HaltReport::Resolved
    );

    // …and `VIKE_HALT_FILE` remains the operator's fix for the container case, needing no rebuild:
    // it outranks the walk entirely and reports as rung 1.
    let named = scratch.path().join("mounted").join(HALT_FILE);
    std::fs::create_dir_all(named.parent().unwrap()).unwrap();
    let (pinned, pinned_rung) =
        halt_path_for_rung(named.to_str(), HaltProject::WalkFrom(&cwd), &exe_dir);
    assert_eq!(pinned, named);
    assert_eq!(pinned_rung, HaltRung::Env);
    assert_eq!(
        halt_report(halt_path_arming_error(&pinned).as_deref(), pinned_rung),
        HaltReport::Resolved
    );
}

/// The code's default and the sandbox's grant must agree, and they live in different files.
///
/// `deploy/vike-tradehub.service` grants write to `<project>/settings/state`; the default resolves
/// to `<project>/settings/state/HALT`. This asserts the RELATIONSHIP rather than either literal: the
/// directory the default lands in, relative to the project root, must appear as a suffix of one of
/// the unit's `ReadWritePaths=` entries. Move the default and this reddens until the unit moves too.
#[test]
fn the_default_lands_where_the_shipped_unit_grants_write() {
    let scratch = Scratch::new("grant");
    let (cwd, exe_dir, _state) = deployment_tree(&scratch);
    let resolved = halt_path_for(None, HaltProject::WalkFrom(&cwd), &exe_dir);

    // The default's directory, expressed relative to the project root: `settings/state`.
    let dir = resolved.parent().expect("the sentinel has a directory");
    let relative = dir.strip_prefix(&cwd).expect("the default resolves inside the project");
    let relative = relative.to_string_lossy().replace('\\', "/");
    assert_eq!(relative, "settings/state", "the shape the unit's grant is written against");

    let unit = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("deploy")
            .join("vike-tradehub.service"),
    )
    .expect("deploy/vike-tradehub.service must exist");

    let granted: Vec<&str> = unit
        .lines()
        .map(str::trim)
        .filter(|l| l.starts_with("ReadWritePaths="))
        .flat_map(|l| l.trim_start_matches("ReadWritePaths=").split_whitespace())
        .collect();
    assert!(!granted.is_empty(), "the unit must grant write somewhere, or nothing here is tested");
    assert!(
        granted.iter().any(|g| g.ends_with(&relative)),
        "the built-in default resolves into `{relative}`, which none of the unit's \
         ReadWritePaths= entries ({granted:?}) grant. Under ProtectSystem=strict that directory is \
         READ-ONLY and the kill switch cannot be armed. Move the grant or move the default — \
         never both apart."
    );
}

/// The negative control, and the reason the two tests above are a fix rather than a preference: a
/// directory the process cannot write really does refuse the `touch`, and the new default does not.
///
/// Unix only — this asserts through mode bits, which is the portable stand-in for the read-only
/// MOUNT `ProtectSystem=strict` actually creates. It SKIPS (loudly, via `eprintln!`) when the
/// control does not hold, rather than passing vacuously: as root, mode bits do not bind.
#[cfg(unix)]
#[test]
fn the_read_only_exe_dir_really_does_refuse_a_touch() {
    use std::os::unix::fs::PermissionsExt;

    let scratch = Scratch::new("erofs");
    let (cwd, exe_dir, state) = deployment_tree(&scratch);
    std::fs::set_permissions(&exe_dir, std::fs::Permissions::from_mode(0o555)).unwrap();

    // The control: creating the OLD default must fail. If it succeeds we are root (or on a
    // filesystem that ignores the mode) and this test can prove nothing — say so and stop.
    let old_default = exe_dir.join(HALT_FILE);
    if std::fs::File::create(&old_default).is_ok() {
        let _ = std::fs::remove_file(&old_default);
        eprintln!(
            "SKIPPED: {} is writable despite mode 0555 (running as root?) — the negative control \
             does not hold, so nothing here would be proven",
            exe_dir.display()
        );
        return;
    }
    assert!(
        halt_path_arming_error(&old_default).is_some(),
        "the arming probe must REPORT an unwritable sentinel — a silent one is the whole defect"
    );

    // …and the new default, in the same tree, is armable.
    let resolved = halt_path_for(None, HaltProject::WalkFrom(&cwd), &exe_dir);
    assert_eq!(resolved, state.join(HALT_FILE));
    assert_eq!(
        halt_path_arming_error(&resolved),
        None,
        "the state directory must be writable — that is the whole point of moving the default here"
    );
    std::fs::File::create(&resolved).expect("an operator's `touch` must succeed at the default");

    std::fs::set_permissions(&exe_dir, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// The arming probe answers the three cases it is asked, and — the part that matters — it can say
/// NO. A probe that answered `None` unconditionally would leave every report above green forever,
/// which is the failure mode this repo has shipped three times.
#[test]
fn the_arming_probe_can_actually_fail() {
    let scratch = Scratch::new("probe");

    // Writable directory, sentinel absent ⇒ armable, and the probe leaves NOTHING behind.
    let ok = scratch.path().join(HALT_FILE);
    assert_eq!(halt_path_arming_error(&ok), None);
    assert!(!ok.exists(), "the probe must not create the sentinel it is asking about");
    let leftovers: Vec<_> = std::fs::read_dir(scratch.path())
        .unwrap()
        .filter_map(Result::ok)
        .map(|e| e.file_name())
        .collect();
    assert!(leftovers.is_empty(), "the probe must clean up after itself, found {leftovers:?}");

    // Parent directory missing ⇒ reported, naming the directory. Nothing creates it lazily, and an
    // operator discovering that with `touch` is exactly the 3am failure being designed out.
    let missing = scratch.path().join("not-created-yet").join(HALT_FILE);
    let err = halt_path_arming_error(&missing).expect("a missing parent must be reported");
    assert!(err.contains("not-created-yet"), "the report must name the directory: {err}");

    // An EXISTING sentinel is armable and is NOT touched — probing must never be able to un-halt a
    // live node by racing an operator's `touch`.
    let armed = scratch.path().join("armed").join(HALT_FILE);
    std::fs::create_dir_all(armed.parent().unwrap()).unwrap();
    std::fs::write(&armed, "engaged").unwrap();
    assert_eq!(halt_path_arming_error(&armed), None);
    assert_eq!(std::fs::read_to_string(&armed).unwrap(), "engaged", "the sentinel is untouched");
}
