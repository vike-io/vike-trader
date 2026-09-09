//! Workspace persistence: a small `StudioWorkspace` snapshot of restorable UI state (which
//! right-hand tab is open, the editor/tools collapsed flags, the template dropdown selection,
//! and the editor's last source).
//!
//! **Where it lives: `<project>/settings/state/studio_workspace.json`.** It used to be
//! `store.root().join("studio_workspace.json")`, colocated with the hist store like `saved.rs`'s
//! strategy list still is; that placement was wrong for the same reason `pace.json`'s was — which
//! tab is open describes THIS USER's screen, not the data in that store, so pointing the Studio at
//! a second store silently forgot the layout.
//!
//! [`state_dir`] resolves it: `$VIKE_STATE_ROOT` names the directory outright and wins, otherwise it
//! is `<project>/settings/state` ([`project_state_dir`]). There is no third location.
//!
//! Adoption stays a DUAL READ, never a move: [`workspace_read_path`] falls through to the legacy
//! `<store_root>` file while nothing has been written yet, [`workspace_write_path`] always writes the
//! state directory, and no old file is ever deleted (see `vike_model::state_path`'s module doc).
//!
//! `load_workspace`/`save_workspace` are pure and best-effort, mirroring
//! `saved::{load_strategies, save_strategies}`: a missing or corrupt file yields
//! `StudioWorkspace::default()` rather than panicking — the file is UI convenience state, not
//! load-bearing data. The path helpers keep that posture: an unresolvable or uncreatable state
//! directory silently yields the legacy store-root path rather than failing, which is the same
//! swallow `StudioState::persist_saved` already applies to a failed write — this crate links no
//! logger, so there is nowhere to warn.
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::studio::RightTab;

/// The workspace snapshot's basename, stated once for both halves of the dual read.
pub const WORKSPACE_FILE: &str = "studio_workspace.json";

/// The explicit state-root override (`$VIKE_STATE_ROOT`), when set and non-empty.
///
/// An exported-but-blank variable is an unset variable in every shell that produced it, so it falls
/// THROUGH rather than resolving to `""` (which would point the writer at the process CWD) — the
/// same filter `vike_app_core::workspace::persist`'s twin applies.
fn state_root_override() -> Option<PathBuf> {
    std::env::var("VIKE_STATE_ROOT").ok().filter(|s| !s.trim().is_empty()).map(PathBuf::from)
}

/// `<project>/settings/state` — found by walking UP from the working directory for the project
/// marker. `None` when no project sits above it.
///
/// The env-reading half of `vike_model::state_path` (which is pure) — the same split
/// `vike_backfill::cli::resolve` uses for the store root. This crate is a LIBRARY, so this read
/// carries a `Layer::Library` row in `vike_ops::settings::SETTINGS`.
fn project_state_dir() -> Option<PathBuf> {
    std::env::current_dir().ok().and_then(|cwd| vike_model::state_path::project_state_dir(&cwd))
}

/// The state directory a WRITE lands in: the override, else the project's. There is no third.
fn state_dir() -> Option<PathBuf> {
    state_root_override().or_else(project_state_dir)
}

/// Where to READ the workspace snapshot from: `<state_dir>/studio_workspace.json` when it exists,
/// else the legacy `<store_root>/studio_workspace.json`.
///
/// The shared `state_path::read_path` helper rather than a bespoke path chain of its own.
pub fn workspace_read_path(store_root: &Path) -> PathBuf {
    vike_model::state_path::read_path(
        state_dir().as_deref(),
        WORKSPACE_FILE,
        store_root.join(WORKSPACE_FILE),
    )
}

/// Where to WRITE it: always `<state_dir>/studio_workspace.json` (which is what migrates an
/// existing install on its first save), falling back to the legacy store-root path when the state
/// directory cannot be resolved or created — or when the state path is a SYMLINK, which
/// `vike_model::state_path::write_path` refuses rather than truncating the link's target.
///
/// ⚠ That last case degrades SILENTLY here, unlike `vike_app_core::workspace::persist`'s
/// `save_path`, which propagates the `Err` to the user. The save still lands somewhere real and
/// never writes through the link, but nothing says the state directory was skipped.
pub fn workspace_write_path(store_root: &Path) -> PathBuf {
    vike_model::state_path::write_path(state_dir().as_deref(), WORKSPACE_FILE)
        .unwrap_or_else(|_| store_root.join(WORKSPACE_FILE))
}

/// A snapshot of the Studio's restorable UI state — everything `StudioState::new` needs to
/// reopen where the user left off.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct StudioWorkspace {
    pub right_tab: RightTab,
    pub editor_collapsed: bool,
    pub tools_collapsed: bool,
    pub template_idx: usize,
    /// The editor buffer's source as of the last persist. Empty (the `Default` value) is
    /// treated by the caller as "nothing to restore" — keep the default starter script rather
    /// than blanking the editor.
    pub editor_source: String,
}

/// Load the workspace snapshot from `path`. A missing file, or one that fails to parse, yields
/// `StudioWorkspace::default()` rather than panicking.
pub fn load_workspace(path: &Path) -> StudioWorkspace {
    let Ok(text) = std::fs::read_to_string(path) else { return StudioWorkspace::default() };
    serde_json::from_str(&text).unwrap_or_default()
}

/// Write the workspace snapshot to `path` as pretty JSON, overwriting any existing file.
pub fn save_workspace(path: &Path, ws: &StudioWorkspace) -> std::io::Result<()> {
    let text = serde_json::to_string_pretty(ws).unwrap_or_else(|_| "{}".to_string());
    std::fs::write(path, text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_workspace_has_sane_defaults() {
        let ws = StudioWorkspace::default();
        assert!(ws.editor_source.is_empty());
        assert_eq!(ws.right_tab, RightTab::Sweep);
        assert!(!ws.editor_collapsed);
        assert!(!ws.tools_collapsed);
        assert_eq!(ws.template_idx, 0);
    }

    #[test]
    fn save_then_load_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("studio_workspace.json");
        let ws = StudioWorkspace {
            right_tab: RightTab::Saved,
            editor_collapsed: true,
            tools_collapsed: false,
            template_idx: 3,
            editor_source: "fn on_bar() { market(1, 1.0); }".to_string(),
        };

        save_workspace(&path, &ws).unwrap();
        let loaded = load_workspace(&path);

        assert_eq!(loaded, ws);
    }

    #[test]
    fn load_of_a_missing_path_is_default_not_a_panic() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("does_not_exist.json");
        assert_eq!(load_workspace(&path), StudioWorkspace::default());
    }

    #[test]
    fn load_of_a_corrupt_file_is_default_not_a_panic() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("garbage.json");
        std::fs::write(&path, "{ not: valid json ]]]").unwrap();
        assert_eq!(load_workspace(&path), StudioWorkspace::default());
    }

    /// Settings-unification Phase 2 — the dual read, driven through THIS module's basename.
    ///
    /// The precedence itself is `vike_model::state_path`'s own test; what this pins is that both
    /// halves use the SAME name (a typo on either side would silently write a second file the
    /// loader never looks at) and that they compose the intended way: the legacy store-root file
    /// while nothing has migrated, the new one the moment a save has landed.
    ///
    /// Env-free by construction — the resolver is pure and every directory here is a temp one, so
    /// no real `$HOME` is touched and no ambient `$VIKE_STATE_ROOT` can move the answer.
    #[test]
    fn the_dual_read_uses_one_basename_and_a_write_always_lands_on_the_new_path() {
        assert_eq!(WORKSPACE_FILE, "studio_workspace.json");
        let root = tempfile::tempdir().unwrap();
        let state = root.path().join("state");
        let store_root = root.path().join("store");
        std::fs::create_dir_all(&store_root).unwrap();
        let legacy = store_root.join(WORKSPACE_FILE);
        save_workspace(&legacy, &StudioWorkspace { template_idx: 7, ..Default::default() })
            .unwrap();

        // Nothing migrated yet ⇒ a read still resolves (and loads) the store-root file.
        let read = vike_model::state_path::read_path(
            Some(state.as_path()),
            WORKSPACE_FILE,
            store_root.join(WORKSPACE_FILE),
        );
        assert_eq!(read, legacy);
        assert_eq!(load_workspace(&read).template_idx, 7);

        // A write always lands on the new path, creating the directory lazily.
        let written =
            vike_model::state_path::write_path(Some(state.as_path()), WORKSPACE_FILE).unwrap();
        assert_eq!(written, state.join(WORKSPACE_FILE));
        save_workspace(&written, &StudioWorkspace { template_idx: 9, ..Default::default() })
            .unwrap();

        // …and from then on the read follows it there, with the old file left in place.
        let read = vike_model::state_path::read_path(
            Some(state.as_path()),
            WORKSPACE_FILE,
            store_root.join(WORKSPACE_FILE),
        );
        assert_eq!(read, written);
        assert_eq!(load_workspace(&read).template_idx, 9);
        assert!(legacy.exists(), "Phase 2 never deletes the old file");
    }

    /// The project's `settings/state` is what a WRITE lands in, and the READ falls back to the
    /// legacy store-root file until one has.
    ///
    /// Pure by construction: `project_state_dir` walks up for the project marker, so a throwaway
    /// project laid out under a temp dir answers without touching the real repo, the process CWD,
    /// or any environment variable.
    #[test]
    fn the_project_settings_state_dir_wins_and_the_read_falls_back_to_the_store_root() {
        let root = tempfile::tempdir().unwrap();
        let project = root.path().join("proj");
        std::fs::create_dir_all(project.join("crates/vike-studio")).unwrap();
        std::fs::write(project.join("Cargo.toml"), "[workspace]\n").unwrap();

        // The resolver this module consults, and where it points.
        let proj_state = vike_model::state_path::project_state_dir(&project).unwrap();
        assert_eq!(proj_state, project.join("settings").join("state"));
        // …and it is found from a nested working directory, not just the project root.
        assert_eq!(
            vike_model::state_path::project_state_dir(&project.join("crates/vike-studio")),
            Some(proj_state.clone())
        );

        let store_root = root.path().join("store");
        std::fs::create_dir_all(&store_root).unwrap();

        // The read, spelled exactly as `workspace_read_path` composes it.
        let read = |proj: Option<&Path>| {
            vike_model::state_path::read_path(proj, WORKSPACE_FILE, store_root.join(WORKSPACE_FILE))
        };

        // Nothing migrated yet ⇒ the legacy store-root file.
        assert_eq!(read(Some(&proj_state)), store_root.join(WORKSPACE_FILE));

        // Once a save has landed in the project, that is what answers.
        let written =
            vike_model::state_path::write_path(Some(&proj_state), WORKSPACE_FILE).unwrap();
        assert_eq!(written, proj_state.join(WORKSPACE_FILE));
        save_workspace(&written, &StudioWorkspace { template_idx: 8, ..Default::default() })
            .unwrap();
        assert_eq!(read(Some(&proj_state)), written);
        assert_eq!(load_workspace(&read(Some(&proj_state))).template_idx, 8);

        // No project above the working directory ⇒ the legacy path, and nothing is invented.
        assert_eq!(read(None), store_root.join(WORKSPACE_FILE));
    }

    /// A state directory that cannot be created (here: a plain FILE sits where it should be)
    /// degrades to the legacy store-root path. Never a panic — the Studio must still start and
    /// still persist somewhere.
    #[test]
    fn an_unusable_state_dir_degrades_to_the_store_root_path() {
        let root = tempfile::tempdir().unwrap();
        let blocker = root.path().join("blocker");
        std::fs::write(&blocker, "not a directory").unwrap();
        let store_root = root.path().join("store");

        let blocked = blocker.join("state");
        let got = vike_model::state_path::write_path(Some(blocked.as_path()), WORKSPACE_FILE)
            .unwrap_or_else(|_| store_root.join(WORKSPACE_FILE));
        assert_eq!(got, store_root.join(WORKSPACE_FILE));
    }

    #[test]
    fn save_overwrites_an_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("studio_workspace.json");
        save_workspace(&path, &StudioWorkspace { template_idx: 1, ..Default::default() }).unwrap();
        save_workspace(&path, &StudioWorkspace { template_idx: 2, ..Default::default() }).unwrap();
        assert_eq!(
            load_workspace(&path),
            StudioWorkspace { template_idx: 2, ..Default::default() }
        );
    }
}
