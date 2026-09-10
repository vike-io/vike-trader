//! Where the LIVE-TICK store lives when nobody said — the tape twin of [`crate::store_path`].
//!
//! Ported from nothing: net-new Rust surface, extracted from TWO byte-identical copies of one
//! six-line ladder — `crates/vike-desktop/src/main.rs`'s `tick_store_root` and
//! `crates/vike-tradehub/src/tradehub_cli.rs`'s `tick_store_root`. Two sides that must not disagree is the
//! shape this workspace's layering rule already answers: the cure is a shared home BELOW both, not
//! a copy in each.
//!
//! # What both copies resolved, and why it was wrong
//!
//! `$VIKE_TICK_STORE`, else **`<exe_dir>/market_data/ticks`** — no project rung, and no log line naming
//! either the path or the reason. That default is the one every other program-written path in this
//! workspace has already moved off: the rolling log file, the HALT sentinel
//! (`crates/vike-bridge-core/src/halt.rs`'s `resolve_halt_path`), the workspace/layout family
//! (`crates/vike-app-core/src/workspace/persist.rs`), the Telegram at-most-once ledger and the
//! strategy-state sidecars all resolve `<project>/settings/state` first and keep `<exe_dir>` only
//! as the answer for a binary with no project above it. This was the straggler.
//!
//! `<exe_dir>` is not a neutral place to put bytes:
//!
//! * in a checkout it is `target/debug`, which `cargo clean` deletes;
//! * in a systemd install it is the `bin/` directory the unit's `ReadWritePaths=` deliberately does
//!   NOT grant — `deploy/vike-tradehub.service` argues that refusal twice, because a daemon that can
//!   write its own executable's directory is a worse problem than any it would solve;
//! * in a container it is the image layer (`/usr/local/bin`), which `docker run` discards, while the
//!   thing the operator bind-mounted is `<project>`.
//!
//! And what lands there is not a cache. `vike_data::RecorderSink` writes the live quote/trade/book
//! TAPE, which is market data that cannot be re-fetched at any price, and the journal materializer
//! writes the `kind=exec_fill` / `kind=exec_order` series that `vike-report` reads as the live
//! tearsheet's durable sink.
//!
//! # The precedence
//!
//! 1. `$VIKE_TICK_STORE` — stated for this box, so it outranks every default. A blank or
//!    whitespace-only value is IGNORED rather than honoured (it would resolve the store to `""` and
//!    create one at the working directory), the same rule [`crate::store_path::resolve_store_root`]
//!    applies to `$VIKE_HIST_STORE` and the halt sentinel applies to `$VIKE_HALT_FILE`.
//! 2. **An `<exe_dir>/market_data/ticks` that ALREADY EXISTS** — the compatibility hinge, and it is the
//!    same argument [`crate::store_path`]'s dev-checkout rung makes: *a store that moves does not
//!    merge*. The old one simply stops being read and the run reports zero rows, which is
//!    indistinguishable from "the recorder found nothing". Every box that has ever recorded a tape
//!    under the old default keeps reading and writing it, so this change relocates nothing that
//!    holds data. Nothing ever CREATES that directory again once a project answers, so a fresh
//!    install never reaches this rung.
//! 3. `<project>/market_data/ticks` — the project the settings walk already resolved, so the tape lands in
//!    the same folder as the settings, the credentials and the state that were loaded for this run.
//!    A sibling of `market_data/hist` inside [`crate::state_path::PROJECT_DATA_DIR`], which is exactly the
//!    growth that constant's own doc anticipates ("a second store later becomes a sibling INSIDE
//!    this folder rather than a second top-level name").
//! 4. `<exe_dir>/market_data/ticks` — no project above the working directory. **Byte-identical to what both
//!    copies resolved**, so a deployment that had no project never changes its answer.
//! 5. `market_data/ticks`, relative — no project AND no executable path (`current_exe()` failed). Total
//!    rather than panicking, and identical to what the shipped `unwrap_or_default().join(..)`
//!    produced in the same case.
//!
//! ⚠ **The project arrives as the SETTINGS directory, not as the project root**, and the strip to
//! its parent happens HERE rather than at each binary. That is deliberate: it is the one line both
//! callers would otherwise spell for themselves, and a `VIKE_SETTINGS_DIR=settings` with no parent
//! must be refused rather than resolved against the working directory — the refusal
//! [`crate::store_path`]'s module doc already states for the hist store's project rung.
//!
//! # The resolution is OBSERVABLE, and that is not decoration
//!
//! Every answer comes back as a [`TickStoreRoot`] — the path AND the [`TickStoreRootRung`] that
//! chose it — and the binaries log both once at startup. This module logs NOTHING itself:
//! `vike-model` carries no logging dependency and is not getting one, so the rung travels as DATA
//! and the CALLER emits it, the same shape as [`crate::store_path::StoreRootRung::why`],
//! `vike_secrets`' permission finding and `vike-cli`'s `settings_warning_lines`.
//!
//! # Every function here is PURE with respect to the ENVIRONMENT
//!
//! The variable, the settings directory and the executable's directory all arrive as parameters and
//! none is read here, per the rule `crates/vike-ops/tests/settings_registry.rs` enforces (libraries
//! take configuration as parameters; only binaries read the process environment). The one thing it
//! DOES touch is the filesystem, for rung 2's `is_dir` probe — the same probe
//! [`crate::store_path::resolve_store_root`] performs for its own compatibility hinge, and for the
//! same reason: whether a store is already there is the only evidence that answers the question.

use std::path::{Path, PathBuf};

use crate::state_path::PROJECT_DATA_DIR;

/// `ticks` — the live-tape store inside [`crate::state_path::PROJECT_DATA_DIR`], the sibling of
/// [`crate::state_path::HIST_SUBDIR`].
///
/// The two are separate stores rather than one because they are written by different things at
/// different rates: `hist` is what the backfill collectors and the harness populate, `ticks` is what
/// a live recorder appends to while a daemon trades. They share the `market_data/` parent so an operator
/// still has ONE folder to move, back up or delete.
pub const TICKS_SUBDIR: &str = "ticks";

/// WHICH rung of [`resolve_tick_store_root`] answered — returned as DATA so the CALLER can log it
/// (`vike-model` carries no logging dependency; see the module doc).
///
/// The variants are in precedence order, and that order is asserted by `the_rungs_step_down_in_order`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum TickStoreRootRung {
    /// Rung 1 — `$VIKE_TICK_STORE`.
    EnvVar,
    /// Rung 2 — a store that ALREADY EXISTS beside the executable, under the pre-project default.
    ExeDirInPlace,
    /// Rung 3 — `<project>/market_data/ticks`, the project the settings walk resolved.
    Project,
    /// Rung 4 — `<exe_dir>/market_data/ticks`: no project above the working directory.
    ExeDir,
    /// Rung 5 — no project and no executable path either (a scrubbed environment).
    LastResort,
}

impl TickStoreRootRung {
    /// A stable machine-readable tag for a structured log field (`rung = "project"`).
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::EnvVar => "env",
            Self::ExeDirInPlace => "exe-dir-in-place",
            Self::Project => "project",
            Self::ExeDir => "exe-dir",
            Self::LastResort => "last-resort",
        }
    }

    /// The operator-facing sentence: WHY this rung answered, and — for the rungs that surprise
    /// people — what to state instead. This is the half of the log line that turns "why is my tape
    /// empty" into a one-line diagnosis.
    pub const fn why(self) -> &'static str {
        match self {
            Self::EnvVar => "stated for this box ($VIKE_TICK_STORE)",
            Self::ExeDirInPlace => {
                "a tick store ALREADY EXISTS beside the executable, and a store that moves does not \
                 merge — the old one would simply stop being read. Move it into <project>/market_data/ticks \
                 (or name it with VIKE_TICK_STORE) to take the project default"
            }
            Self::Project => {
                "this project's own data folder, found by the same walk as <project>/settings — set \
                 VIKE_SETTINGS_DIR to pin the project, or VIKE_TICK_STORE to pin the store outright"
            }
            Self::ExeDir => {
                "beside the executable: NO project was found above the working directory (create \
                 <project>/settings/, or set VIKE_SETTINGS_DIR / VIKE_TICK_STORE). ⚠ that directory \
                 is the image layer in a container and target/debug in a checkout — both discarded"
            }
            Self::LastResort => {
                "last resort: no project, and this process could not read its own executable path — \
                 set VIKE_TICK_STORE"
            }
        }
    }
}

/// A resolved tick-store root together with the [`TickStoreRootRung`] that chose it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TickStoreRoot {
    /// The directory a store will be opened (and CREATED) at.
    pub root: PathBuf,
    /// Which rung answered — the field that makes a silent relocation visible.
    pub rung: TickStoreRootRung,
}

impl TickStoreRoot {
    /// The path alone, for the call sites that only need somewhere to open a store.
    pub fn into_path(self) -> PathBuf {
        self.root
    }
}

impl std::fmt::Display for TickStoreRoot {
    /// ONE operator-facing line: the path, then why it is that path.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} ({})", self.root.display(), self.rung.why())
    }
}

/// Resolve the live-tick store root. See the module doc for the full precedence and the WHY of each
/// rung.
///
/// `env_tick_store` is `$VIKE_TICK_STORE` as the BINARY read it; `settings_dir` is
/// `<project>/settings` as that binary's ONE boot walk resolved it (`vike_boot::Booted::settings_dir`
/// — never a second walk, which would be `$VIKE_SETTINGS_DIR`-blind); `exe_dir` is the directory of
/// the running executable.
pub fn resolve_tick_store_root(
    env_tick_store: Option<&str>,
    settings_dir: Option<&Path>,
    exe_dir: Option<&Path>,
) -> TickStoreRoot {
    if let Some(p) = env_tick_store.filter(|s| !s.trim().is_empty()) {
        return TickStoreRoot { root: PathBuf::from(p), rung: TickStoreRootRung::EnvVar };
    }
    let beside_exe = exe_dir.map(|d| d.join(PROJECT_DATA_DIR).join(TICKS_SUBDIR));
    // THE COMPATIBILITY HINGE, and it probes for the same reason `store_path`'s dev-checkout rung
    // does: whether a store is ALREADY there is the only evidence that answers "would this change
    // relocate somebody's tape". A store that moves does not merge — the old one just stops being
    // read and the run reports zero rows, which reads as "the recorder found nothing". Nothing
    // creates this directory once a project answers, so a fresh install never reaches the rung.
    if let Some(p) = beside_exe.as_ref().filter(|p| p.is_dir()) {
        return TickStoreRoot { root: p.clone(), rung: TickStoreRootRung::ExeDirInPlace };
    }
    // THE PROJECT RUNG. No existence probe, deliberately, and the asymmetry with the hinge above is
    // the point: this path was resolved at RUNTIME by the walk that already found `settings/`, so
    // the project is proven to exist and `market_data/` merely has not been written to yet. Probing it
    // would send every fresh install beside the executable on its first run and to the project on
    // its second. The `parent()` strip is what turns `<project>/settings` back into `<project>`; a
    // parentless value names no project and falls through rather than resolving against the CWD.
    if let Some(project) = settings_dir.and_then(Path::parent).filter(|p| !p.as_os_str().is_empty())
    {
        return TickStoreRoot {
            root: project.join(PROJECT_DATA_DIR).join(TICKS_SUBDIR),
            rung: TickStoreRootRung::Project,
        };
    }
    match beside_exe {
        Some(p) => TickStoreRoot { root: p, rung: TickStoreRootRung::ExeDir },
        None => TickStoreRoot {
            root: Path::new(PROJECT_DATA_DIR).join(TICKS_SUBDIR),
            rung: TickStoreRootRung::LastResort,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A private scratch directory under the system temp dir — this crate has no `tempfile`
    /// dev-dependency, so these tests make (and remove) their own. Same shape as
    /// [`crate::store_path`]'s.
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
        let settings = project.join(crate::state_path::PROJECT_SETTINGS_DIR);
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
        let settings = scratch.path().join("proj").join(crate::state_path::PROJECT_SETTINGS_DIR);
        std::fs::create_dir_all(&settings).unwrap();
        let exe = empty_exe_dir(&scratch);

        let got = resolve_tick_store_root(Some("/mnt/tape"), Some(&settings), Some(&exe));

        assert_eq!(got.root, PathBuf::from("/mnt/tape"));
        assert_eq!(got.rung, TickStoreRootRung::EnvVar);
    }

    /// A blank override must NOT win: it would resolve the store to `""` and create one at the
    /// working directory — the exact class of bug `crate::store_path` exists to remove.
    #[test]
    fn a_blank_env_value_is_ignored() {
        let scratch = Scratch::new("blank");
        let project = scratch.path().join("proj");
        let settings = project.join(crate::state_path::PROJECT_SETTINGS_DIR);
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
        let settings = scratch.path().join("proj").join(crate::state_path::PROJECT_SETTINGS_DIR);
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
    /// than resolved against the working directory — the same refusal `crate::store_path`'s module
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
        let settings = project.join(crate::state_path::PROJECT_SETTINGS_DIR);
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
            (
                exe_legacy.join(PROJECT_DATA_DIR).join(TICKS_SUBDIR),
                TickStoreRootRung::ExeDirInPlace
            )
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
        let settings = scratch.path().join("proj").join(crate::state_path::PROJECT_SETTINGS_DIR);
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
    /// top-level name — the layout `crate::state_path::PROJECT_DATA_DIR`'s doc commits to.
    #[test]
    fn the_tape_is_a_sibling_of_the_hist_store_under_one_data_folder() {
        let hist = crate::state_path::project_hist_store_dir(Path::new("/never/mind"));
        // The walk may answer `None` on a machine with no marker above `/`; the shape assertion
        // below is what matters and does not depend on it.
        let _ = hist;
        let ticks = Path::new("/p").join(PROJECT_DATA_DIR).join(TICKS_SUBDIR);
        let hist = Path::new("/p").join(PROJECT_DATA_DIR).join(crate::state_path::HIST_SUBDIR);
        assert_eq!(ticks.parent(), hist.parent(), "one data/ folder, two stores");
        assert_ne!(ticks, hist);
    }
}
