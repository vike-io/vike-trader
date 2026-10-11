//! Workspace persistence: a small `StudioWorkspace` snapshot of restorable UI state (which
//! right-hand tab is open, the editor/tools collapsed flags, the template dropdown selection,
//! the editor's last source, and the strategy source that says which language that buffer is —
//! with the Native/Plugin pane selection it needs to mean the same thing after a restart).
//!
//! **Where it lives: `<project>/settings/state/studio_workspace.json`**, not beside the hist store
//! like `saved.rs`'s strategy list: which tab is open describes THIS USER's screen, not the data in
//! that store. The old `<store_root>/studio_workspace.json` is read by nothing (decision 0117).
//!
//! [`state_dir`] resolves it: the directory the composition root's boot resolved, declared once
//! through [`declare_workspace_state_dir`] (the desktop hands it `vike_boot::Booted::state_dir`,
//! the only answer that honours `$VIKE_SETTINGS_DIR`), otherwise `<project>/settings/state` found
//! by walking up from the working directory ([`project_state_dir`]) for a process that declares
//! nothing — a test or an example. There is no second location and no environment rung:
//! `VIKE_STATE_ROOT` refuses startup (decision 0111).
//!
//! `load_workspace`/`save_workspace` are pure and best-effort, mirroring
//! `saved::{load_strategies, save_strategies}`: a missing or corrupt file yields
//! `StudioWorkspace::default()` rather than panicking — the file is UI convenience state, not
//! load-bearing data. The path helpers keep that posture: with no resolvable state directory there
//! is nothing to read and the save is skipped, the same swallow `StudioState::persist_saved`
//! already applies to a failed write — this crate links no logger, so there is nowhere to warn.
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::panes::saved::StrategySource;
use crate::studio::RightTab;

/// The workspace snapshot's basename, stated once for the read and the write.
pub(crate) const WORKSPACE_FILE: &str = "studio_workspace.json";

/// `<project>/settings/state` exactly as the composition root's boot resolved it, set once by
/// [`declare_workspace_state_dir`]. `None` inside the cell is a legitimate answer (the root booted
/// and found no project); an UNSET cell means no root declared, which is a test or an example.
static DECLARED_STATE_DIR: std::sync::OnceLock<Option<PathBuf>> = std::sync::OnceLock::new();

/// **Declare the state directory the composition root already resolved** — call it with
/// `vike_boot::Booted::state_dir`, once, before the Studio is constructed.
///
/// A declaration rather than a constructor parameter for the reason
/// `vike_app_core::ui::workspace::persist::declare_project_state_dir` gives for the desktop's own
/// workspace family: the answer is a property of the PROCESS (its one boot walk), and this library
/// may not resolve it from the environment itself.
///
/// # Errors
/// A second declaration naming a DIFFERENT directory is refused and changes nothing — one process
/// has one workspace directory. Repeating the declaration in force is not a fault.
pub fn declare_workspace_state_dir(state_dir: Option<PathBuf>) -> Result<(), String> {
    let declared = DECLARED_STATE_DIR.get_or_init(|| state_dir.clone());
    if declared.as_deref() == state_dir.as_deref() {
        return Ok(());
    }
    let label = |d: Option<&Path>| {
        d.map_or_else(|| "<no project>".to_string(), |p| p.display().to_string())
    };
    Err(format!(
        "the Studio's workspace directory was already declared as {}; the second declaration ({}) \
         is IGNORED — one process has one workspace directory",
        label(declared.as_deref()),
        label(state_dir.as_deref())
    ))
}

/// `<project>/settings/state` — found by walking UP from the working directory for the project
/// marker, for a process whose root declared nothing. `None` when no project sits above it.
fn project_state_dir() -> Option<PathBuf> {
    std::env::current_dir()
        .ok()
        .and_then(|cwd| vike_model::paths::state_path::project_state_dir(&cwd))
}

/// The state directory the snapshot lives in: the root's declaration, else the walk. There is no
/// third.
fn state_dir() -> Option<PathBuf> {
    match DECLARED_STATE_DIR.get() {
        Some(declared) => declared.clone(),
        None => project_state_dir(),
    }
}

/// Where to READ the workspace snapshot from: `<state_dir>/studio_workspace.json`. `None` when no
/// state directory resolves — there is no file to read then, and the caller restores the default.
pub(crate) fn workspace_read_path() -> Option<PathBuf> {
    Some(state_dir()?.join(WORKSPACE_FILE))
}

/// Where to WRITE it: `<state_dir>/studio_workspace.json`, the directory created lazily.
///
/// `Err` when the state directory cannot be resolved or created, or when the state path is a
/// SYMLINK, which `vike_model::paths::state_path::write_path` refuses rather than truncating the
/// link's target. ⚠ The caller swallows it (this crate links no logger), unlike
/// `vike_app_core::ui::workspace::persist`'s `save_path`, which propagates the `Err` to the user:
/// the save is skipped and nothing says so.
pub(crate) fn workspace_write_path() -> std::io::Result<PathBuf> {
    vike_model::paths::state_path::write_path(state_dir().as_deref(), WORKSPACE_FILE)
}

/// A snapshot of the Studio's restorable UI state — everything `StudioState::new` needs to
/// reopen where the user left off.
///
/// ⚠ **Every field added after the original five carries `#[serde(default)]`, and must.** The
/// derive has no container-level default, so a field an existing file lacks is a PARSE ERROR — and
/// [`load_workspace`] answers a parse error with `StudioWorkspace::default()`, silently dropping the
/// whole snapshot (buffer included) rather than the one field. Same purely additive migration as
/// `crate::panes::saved::SavedStrategy`: no version stamp, no rewrite pass. Pinned by
/// `an_old_format_file_loads_every_field_it_has_and_reads_as_rhai`.
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
    /// Which language [`Self::editor_source`] is — the Strategy pane's source selection. A file
    /// written before this field existed has none, and reads as [`StrategySource::Rhai`], which is
    /// what every such session was in (nothing could restore another).
    ///
    /// ⚠ It rides WITH the buffer because the buffer alone is not a restorable fact: persisted
    /// without it, a Plugin author's Rust source came back in Rhai mode.
    #[serde(default)]
    pub strategy_source: StrategySource,
    /// The Native dropdown's selection, by registry NAME (an index would name a different strategy
    /// once the registry changes). Empty in an old file, which restores row 0 as before.
    #[serde(default)]
    pub native_strategy: String,
    /// The Native pane's `(key, value-text)` param rows, verbatim.
    #[serde(default)]
    pub native_params: Vec<(String, String)>,
    /// The Plugin pane's declared name. The plugin's last-built SHA is deliberately NOT here: a sha
    /// with no recorded source is a sha the staleness guard cannot check — `StudioState`'s
    /// constructor says why a restored Plugin session starts unbuilt instead.
    #[serde(default)]
    pub plugin_name: String,
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
            editor_source: "pub fn build() {}".to_string(),
            strategy_source: StrategySource::Plugin,
            native_strategy: "buy_hold".to_string(),
            native_params: vec![("size".to_string(), "2".to_string())],
            plugin_name: "my_strat".to_string(),
        };

        save_workspace(&path, &ws).unwrap();
        let loaded = load_workspace(&path);

        assert_eq!(loaded, ws);
    }

    /// **Backward compatibility, over the exact bytes an existing install holds.** A
    /// `studio_workspace.json` written before the strategy source was persisted has the original
    /// five keys and nothing else. It must load with every one of those five intact and read as
    /// Rhai — the mode every such session was in.
    ///
    /// The five-field assertions are the load-bearing half: without `#[serde(default)]` on a new
    /// field the parse FAILS, and `load_workspace` turns that into `StudioWorkspace::default()` —
    /// which is ALSO Rhai. So "reads as Rhai" alone would pass over a file whose buffer, tab and
    /// template had all been thrown away.
    #[test]
    fn an_old_format_file_loads_every_field_it_has_and_reads_as_rhai() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(WORKSPACE_FILE);
        std::fs::write(
            &path,
            r#"{
  "right_tab": "Saved",
  "editor_collapsed": true,
  "tools_collapsed": false,
  "template_idx": 2,
  "editor_source": "fn on_bar() { market(1, 1.0); }"
}"#,
        )
        .unwrap();

        let loaded = load_workspace(&path);
        assert_eq!(
            loaded,
            StudioWorkspace {
                right_tab: RightTab::Saved,
                editor_collapsed: true,
                tools_collapsed: false,
                template_idx: 2,
                editor_source: "fn on_bar() { market(1, 1.0); }".to_string(),
                strategy_source: StrategySource::Rhai,
                native_strategy: String::new(),
                native_params: Vec::new(),
                plugin_name: String::new(),
            },
            "an old file must keep all five of its fields and read as Rhai"
        );
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

    /// The snapshot lives in the project's `settings/state`, found from a nested working directory,
    /// and a save there is what the next read loads.
    ///
    /// Pure by construction: `project_state_dir` walks up for the project marker, so a throwaway
    /// project laid out under a temp dir answers without touching the real repo, the process CWD,
    /// or any environment variable.
    #[test]
    fn the_snapshot_lives_in_the_project_settings_state_dir() {
        assert_eq!(WORKSPACE_FILE, "studio_workspace.json");
        let root = tempfile::tempdir().unwrap();
        let project = root.path().join("proj");
        std::fs::create_dir_all(project.join("crates/vike-studio")).unwrap();
        std::fs::write(project.join("Cargo.toml"), "[workspace]\n").unwrap();

        // The resolver this module consults, and where it points…
        let proj_state = vike_model::paths::state_path::project_state_dir(&project).unwrap();
        assert_eq!(proj_state, project.join("settings").join("state"));
        // …found from a nested working directory, not just the project root.
        assert_eq!(
            vike_model::paths::state_path::project_state_dir(&project.join("crates/vike-studio")),
            Some(proj_state.clone())
        );

        // The write, spelled exactly as `workspace_write_path` composes it, creates the directory
        // lazily, and the read path `workspace_read_path` composes then loads it.
        let written =
            vike_model::paths::state_path::write_path(Some(&proj_state), WORKSPACE_FILE).unwrap();
        assert_eq!(written, proj_state.join(WORKSPACE_FILE));
        save_workspace(&written, &StudioWorkspace { template_idx: 8, ..Default::default() })
            .unwrap();
        assert_eq!(load_workspace(&proj_state.join(WORKSPACE_FILE)).template_idx, 8);
    }

    /// A state directory that cannot be created (here: a plain FILE sits where it should be) is an
    /// `Err` the caller swallows: the save is skipped, never a panic and never a second location.
    #[test]
    fn an_unusable_state_dir_is_an_error_not_a_second_location() {
        let root = tempfile::tempdir().unwrap();
        let blocker = root.path().join("blocker");
        std::fs::write(&blocker, "not a directory").unwrap();

        let blocked = blocker.join("state");
        assert!(
            vike_model::paths::state_path::write_path(Some(blocked.as_path()), WORKSPACE_FILE)
                .is_err()
        );
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
