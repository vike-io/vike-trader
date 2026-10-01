//! Where the LIVE-TICK store lives when nobody said — the tape twin of [`crate::store_path`].
//!
//! Ported from nothing: net-new Rust surface, extracted from TWO byte-identical copies of one
//! six-line ladder, one in the desktop and one in the daemon. Two sides that must not disagree is
//! the shape this workspace's layering rule already answers: the cure is a shared home BELOW both,
//! not a copy in each.
//!
//! ⚠ **BOTH COPIES ARE GONE, AND SO IS EVERY PRODUCTION CALLER OF WHAT REPLACED THEM.** The
//! desktop's went with the GUI's local tick plane; the daemon's went with `record-feeds` under
//! `docs/decisions/0084-only-the-datahub-touches-the-store.md`. This module doc used to CITE the
//! two by path and symbol, and by the day the second one died the citation was resting on a stale
//! comment that happened to still spell the token — `crates/vike-ops/tests/citation_gate.rs` is
//! text-only, so it read green while pointing at nothing. Both citations are withdrawn rather
//! than repaired: there is nowhere to repoint them.
//!
//! What that leaves is a pure, tested resolver that nothing calls. It is kept rather than deleted
//! because the question above it is unanswered — whether `VIKE_TICK_STORE` should REFUSE a startup
//! the way its sibling `VIKE_TRADEHUB_RECORD` now does (`vike_config::REMOVED_ENV`) — and that
//! answer decides whether this file is dead code or the thing a refusal is written against.
//!
//! # What both copies resolved, and why it was wrong
//!
//! `$VIKE_TICK_STORE`, else **`<exe_dir>/market_data/ticks`** — no project rung, and no log line naming
//! either the path or the reason. That default is the one every other program-written path in this
//! workspace has already moved off: the rolling log file, the HALT sentinel
//! (`crates/vike-bridge-core/src/halt.rs`'s `resolve_halt_path`), the workspace/layout family
//! (`crates/vike-app-core/src/ui/workspace/persist.rs`), the Telegram at-most-once ledger and the
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

#[path = "tick_store_path_tests.rs"]
#[cfg(test)]
mod tick_store_path_tests;
