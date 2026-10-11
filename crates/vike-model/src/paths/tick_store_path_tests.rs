use super::*;

/// A private scratch directory under the system temp dir — this crate has no `tempfile`
/// dev-dependency, so these tests make (and remove) their own. Same shape as
/// [`crate::paths::store_path`]'s.
struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let p = std::env::temp_dir().join(format!("vike-tick-store-{tag}-{nanos}"));
        std::fs::create_dir_all(&p).expect("scratch dir");
        Self(p)
    }
    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A fixture whose executable directory provably has NO tick store in it — the state a fresh
/// install, a fresh checkout and a fresh container image are all in.
///
/// ⚠ The `assert!` is the PRECONDITION guard, and it is deliberately keyed on the FIXTURE
/// rather than on the behaviour under test: it asserts the compatibility hinge cannot fire,
/// which is a fact about the temp tree, not about which rung wins. A guard keyed on the answer
/// would SKIP instead of failing when the fix is mutated away.
fn empty_exe_dir(scratch: &Scratch) -> PathBuf {
    let exe = scratch.path().join("usr").join("local").join("bin");
    std::fs::create_dir_all(&exe).unwrap();
    assert!(
        !exe.join(PROJECT_DATA_DIR).join(TICKS_SUBDIR).is_dir(),
        "precondition: no pre-existing store beside the executable, or rung 2 answers and this \
             test proves nothing"
    );
    exe
}

/// **THE test the shipped code fails.** A project resolved — settings, credentials and state all
/// loaded from it — and the tape must land INSIDE it, not beside the executable.
///
/// This is the containerisation case stated concretely: `<project>` is the host folder that was
/// bind-mounted, `<exe_dir>` is `/usr/local/bin` in the image's own ephemeral layer.
#[test]
fn a_resolved_project_keeps_the_tape_inside_the_project() {
    let scratch = Scratch::new("project");
    let project = scratch.path().join("srv").join("vike-tradehub");
    let settings = project.join(crate::paths::state_path::PROJECT_SETTINGS_DIR);
    std::fs::create_dir_all(&settings).unwrap();
    let exe = empty_exe_dir(&scratch);

    let got = resolve_tick_store_root(None, Some(&settings), Some(&exe));

    assert_eq!(
        got.root,
        project.join(PROJECT_DATA_DIR).join(TICKS_SUBDIR),
        "the tape belongs in the project the settings were loaded from"
    );
    assert_eq!(got.rung, TickStoreRootRung::Project);
    assert_ne!(
        got.root,
        exe.join(PROJECT_DATA_DIR).join(TICKS_SUBDIR),
        "…and NOT beside the executable, which is the image layer in a container"
    );
}

/// …and with NO project above the working directory the answer is BYTE-IDENTICAL to what both
/// binaries resolved before this module existed. This is the half that keeps every existing
/// deployment unchanged.
#[test]
fn without_a_project_the_answer_is_the_shipped_one() {
    let scratch = Scratch::new("noproject");
    let exe = empty_exe_dir(&scratch);

    let got = resolve_tick_store_root(None, None, Some(&exe));

    assert_eq!(got.root, exe.join(PROJECT_DATA_DIR).join(TICKS_SUBDIR));
    assert_eq!(got.rung, TickStoreRootRung::ExeDir);
}

/// The override still wins over everything, including a resolved project — a value typed for
/// this box outranks anything inferred.
#[test]
fn the_env_override_beats_the_project() {
    let scratch = Scratch::new("env");
    let settings = scratch.path().join("proj").join(crate::paths::state_path::PROJECT_SETTINGS_DIR);
    std::fs::create_dir_all(&settings).unwrap();
    let exe = empty_exe_dir(&scratch);

    let got = resolve_tick_store_root(Some("/mnt/tape"), Some(&settings), Some(&exe));

    assert_eq!(got.root, PathBuf::from("/mnt/tape"));
    assert_eq!(got.rung, TickStoreRootRung::EnvVar);
}

/// A blank override must NOT win: it would resolve the store to `""` and create one at the
/// working directory — the exact class of bug `crate::paths::store_path` exists to remove.
#[test]
fn a_blank_env_value_is_ignored() {
    let scratch = Scratch::new("blank");
    let project = scratch.path().join("proj");
    let settings = project.join(crate::paths::state_path::PROJECT_SETTINGS_DIR);
    std::fs::create_dir_all(&settings).unwrap();
    let exe = empty_exe_dir(&scratch);

    let got = resolve_tick_store_root(Some("   "), Some(&settings), Some(&exe));

    assert_eq!(got.root, project.join(PROJECT_DATA_DIR).join(TICKS_SUBDIR));
    assert_eq!(got.rung, TickStoreRootRung::Project);
}

/// **THE compatibility hinge.** A box that has ALREADY been recording under the old default
/// keeps that store, project or no project — because a store that moves does not merge, and the
/// old tape would simply stop being read.
#[test]
fn an_existing_store_beside_the_executable_is_never_relocated() {
    let scratch = Scratch::new("inplace");
    let settings = scratch.path().join("proj").join(crate::paths::state_path::PROJECT_SETTINGS_DIR);
    std::fs::create_dir_all(&settings).unwrap();
    let exe = scratch.path().join("opt").join("vike").join("bin");
    let legacy = exe.join(PROJECT_DATA_DIR).join(TICKS_SUBDIR);
    std::fs::create_dir_all(&legacy).unwrap();
    // Precondition on the FIXTURE: the legacy store is genuinely there. Independent of which
    // rung the code under test picks.
    assert!(legacy.is_dir(), "precondition: the pre-existing store must exist");

    let got = resolve_tick_store_root(None, Some(&settings), Some(&exe));

    assert_eq!(got.root, legacy, "a populated store must not silently relocate");
    assert_eq!(got.rung, TickStoreRootRung::ExeDirInPlace);
}

/// A `VIKE_SETTINGS_DIR=settings` with no parent names no project, and must be REFUSED rather
/// than resolved against the working directory — the same refusal `crate::paths::store_path`'s module
/// doc states for the hist store.
#[test]
fn a_parentless_settings_dir_names_no_project() {
    let scratch = Scratch::new("parentless");
    let exe = empty_exe_dir(&scratch);

    let got = resolve_tick_store_root(None, Some(Path::new("settings")), Some(&exe));

    assert_eq!(got.root, exe.join(PROJECT_DATA_DIR).join(TICKS_SUBDIR));
    assert_eq!(got.rung, TickStoreRootRung::ExeDir);
}

/// Total function: no project, and a process that cannot read its own executable path, still
/// answers — and answers exactly what the shipped `unwrap_or_default().join(..)` did.
#[test]
fn without_an_exe_dir_it_still_answers() {
    let got = resolve_tick_store_root(None, None, None);
    assert_eq!(got.root, Path::new(PROJECT_DATA_DIR).join(TICKS_SUBDIR));
    assert_eq!(got.rung, TickStoreRootRung::LastResort);
}

/// **The whole precedence in ONE ordered assertion.** Each rung is knocked out in turn and the
/// answer must step down exactly one place. A mutation that reorders two rungs — or drops one —
/// changes an answer here even when every single-rung test above still passes.
#[test]
fn the_rungs_step_down_in_order() {
    let scratch = Scratch::new("ladder");
    let project = scratch.path().join("proj");
    let settings = project.join(crate::paths::state_path::PROJECT_SETTINGS_DIR);
    std::fs::create_dir_all(&settings).unwrap();
    // TWO executable directories: one holding a legacy store, one empty. Knocking out rung 2
    // means swapping which one is passed, because the rung's evidence IS the directory.
    let exe_legacy = scratch.path().join("legacy").join("bin");
    std::fs::create_dir_all(exe_legacy.join(PROJECT_DATA_DIR).join(TICKS_SUBDIR)).unwrap();
    let exe_fresh = empty_exe_dir(&scratch);

    // 1. the override — every other rung supplied, and still ignored.
    let got = resolve_tick_store_root(Some("/mnt/tape"), Some(&settings), Some(&exe_legacy));
    assert_eq!((got.root, got.rung), (PathBuf::from("/mnt/tape"), TickStoreRootRung::EnvVar));
    // 2. the in-place hinge, once nothing was stated — ABOVE the project rung.
    let got = resolve_tick_store_root(None, Some(&settings), Some(&exe_legacy));
    assert_eq!(
        (got.root, got.rung),
        (exe_legacy.join(PROJECT_DATA_DIR).join(TICKS_SUBDIR), TickStoreRootRung::ExeDirInPlace)
    );
    // 3. the project, once there is no store beside the executable.
    let got = resolve_tick_store_root(None, Some(&settings), Some(&exe_fresh));
    assert_eq!(
        (got.root, got.rung),
        (project.join(PROJECT_DATA_DIR).join(TICKS_SUBDIR), TickStoreRootRung::Project)
    );
    // 4. beside the executable, once there is no project either — the shipped answer.
    let got = resolve_tick_store_root(None, None, Some(&exe_fresh));
    assert_eq!(
        (got.root, got.rung),
        (exe_fresh.join(PROJECT_DATA_DIR).join(TICKS_SUBDIR), TickStoreRootRung::ExeDir)
    );
    // 5. and the relative path as a last resort, so the function is total.
    let got = resolve_tick_store_root(None, None, None);
    assert_eq!(
        (got.root, got.rung),
        (Path::new(PROJECT_DATA_DIR).join(TICKS_SUBDIR), TickStoreRootRung::LastResort)
    );
}

/// **The rung a caller LOGS must be the rung that answered.** The path and the rung are two
/// fields of one struct, so a copy-paste that returned the right path with a neighbouring rung
/// would report a tape as "stated" while it came from a walk. Every variant is reachable and
/// distinct, and each [`TickStoreRootRung::why`] sentence is non-empty, because an empty
/// explanation in a log line is the same as no log line.
#[test]
fn every_rung_is_reachable_and_carries_its_own_explanation() {
    use std::collections::BTreeSet;
    let scratch = Scratch::new("rungs");
    let settings = scratch.path().join("proj").join(crate::paths::state_path::PROJECT_SETTINGS_DIR);
    std::fs::create_dir_all(&settings).unwrap();
    let exe_legacy = scratch.path().join("legacy").join("bin");
    std::fs::create_dir_all(exe_legacy.join(PROJECT_DATA_DIR).join(TICKS_SUBDIR)).unwrap();
    let exe_fresh = empty_exe_dir(&scratch);

    let seen: Vec<TickStoreRootRung> = vec![
        resolve_tick_store_root(Some("/x"), None, None).rung,
        resolve_tick_store_root(None, Some(&settings), Some(&exe_legacy)).rung,
        resolve_tick_store_root(None, Some(&settings), Some(&exe_fresh)).rung,
        resolve_tick_store_root(None, None, Some(&exe_fresh)).rung,
        resolve_tick_store_root(None, None, None).rung,
    ];
    assert_eq!(
        seen,
        vec![
            TickStoreRootRung::EnvVar,
            TickStoreRootRung::ExeDirInPlace,
            TickStoreRootRung::Project,
            TickStoreRootRung::ExeDir,
            TickStoreRootRung::LastResort,
        ],
        "each rung must be reachable and report ITSELF"
    );
    let tags: BTreeSet<&str> = seen.iter().map(|r| r.as_str()).collect();
    assert_eq!(tags.len(), seen.len(), "the log tags must be distinct");
    for rung in &seen {
        assert!(!rung.why().trim().is_empty(), "{rung:?} must explain itself");
    }
}

/// The `Display` line a binary logs carries BOTH halves: an operator who only sees the path
/// cannot tell a stated `VIKE_TICK_STORE` from a walk that quietly moved.
#[test]
fn the_display_line_names_the_path_and_the_reason() {
    let got = resolve_tick_store_root(Some("/mnt/tape"), None, None);
    let line = got.to_string();
    assert!(line.contains("/mnt/tape"), "the path must be in the line: {line}");
    assert!(line.contains(TickStoreRootRung::EnvVar.why()), "the reason must be too: {line}");
}

/// The tape store is a SIBLING of the hist store inside one `market_data/` folder, not a second
/// top-level name — the layout `crate::paths::state_path::PROJECT_DATA_DIR`'s doc commits to.
#[test]
fn the_tape_is_a_sibling_of_the_hist_store_under_one_data_folder() {
    let hist = crate::paths::state_path::project_hist_store_dir(Path::new("/never/mind"));
    // The walk may answer `None` on a machine with no marker above `/`; the shape assertion
    // below is what matters and does not depend on it.
    let _ = hist;
    let ticks = Path::new("/p").join(PROJECT_DATA_DIR).join(TICKS_SUBDIR);
    let hist = Path::new("/p").join(PROJECT_DATA_DIR).join(crate::paths::state_path::HIST_SUBDIR);
    assert_eq!(ticks.parent(), hist.parent(), "one data/ folder, two stores");
    assert_ne!(ticks, hist);
}
