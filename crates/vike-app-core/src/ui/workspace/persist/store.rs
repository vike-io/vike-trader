//! The workspace family's state directory and `workspace.json` I/O: declaration, resolution, save, load.

use super::layouts::{LAST_LAYOUT_FILE, LAYOUTS_SUBDIR};
use super::{Workspace, capture};
use crate::ui::workspace::state::WinState;
use std::path::PathBuf;
use vike_chart::DisplayTz;

/// The workspace file's basename, stated once (the `layouts/` siblings use their own names).
pub(super) const WORKSPACE_FILE: &str = "workspace.json";

/// The `$VIKE_WORKSPACE` override, when set: it names the workspace FILE outright, and its
/// directory is the base for the named-layout family, so one variable moves the whole family
/// together.
///
/// A desktop developer and QA switch — a capture or a test run pointing the GUI at a throwaway
/// layout — and not a setting, which is why decision 0111 leaves it in the environment
/// (`crates/vike-ops/tests/settings_secrets/settings_registry.rs`'s `ENV_ALLOWLIST`).
pub(super) fn workspace_override() -> Option<PathBuf> {
    std::env::var("VIKE_WORKSPACE").ok().map(PathBuf::from)
}

/// `<project>/settings/state` exactly as the COMPOSITION ROOT's boot resolved it — the middle rung
/// of [`state_dir`], set once by [`declare_project_state_dir`].
///
/// `None` inside the cell is a legitimate answer (the root booted and found no project); an UNSET
/// cell means no root ever declared, which is a test, a tool, or any binary that does not boot.
static DECLARED_STATE_DIR: std::sync::OnceLock<Option<PathBuf>> = std::sync::OnceLock::new();

/// Raised the first time [`state_dir`] answers WITHOUT a declaration in force, so a declaration
/// arriving afterwards can say it changed nothing instead of silently relocating the family
/// mid-run. The same guard `vike_bridge_core::halt`'s `RESOLVED` provides for the sentinel.
static RESOLVED_UNDECLARED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// **Declare the project state directory the composition root already resolved** — call it with
/// `vike_boot::Booted::state_dir`, once, before anything reads the workspace family.
///
/// # ⚠ Why this exists: `$VIKE_SETTINGS_DIR` did not reach this family, and the mismatch was silent
///
/// MEASURED 2026-09-15. Launching `vike-desktop` with `VIKE_SETTINGS_DIR=<project>/settings` from a
/// DIFFERENT working directory read the CREDENTIALS out of that project while resolving the backend
/// registry somewhere else entirely: the client reported *"no --observe and no active backend"*
/// while a perfectly valid `backends.json` sat in the named settings directory. Passing
/// `$VIKE_STATE_ROOT` as well made it work, which is the shape of a defect rather than a
/// configuration.
///
/// The cause is the rule the root `CLAUDE.md` states as **ONE walk DECIDES**: every project-relative
/// path a root uses must be derived from `Booted`, "because the `_from`-less resolvers are
/// `$VIKE_SETTINGS_DIR`-BLIND and a second call answers with whatever the working directory sits
/// above". [`state_dir`]'s `current_dir()` walk was exactly that second call.
///
/// # Why a declaration rather than a parameter on every function
///
/// The cure may NOT be a second environment read here — `crates/vike-ops/tests/settings_secrets/settings_registry.rs`'s
/// `LIBRARY_PIN` is a ratchet that may shrink and never grow, and a library resolving global state
/// its caller can neither see nor override is the defect that ratchet exists to stop. So the answer
/// arrives as a PARAMETER, exactly as `vike_bridge_core::halt::declare_project_state_dir` takes it
/// for the kill switch's rung 2 and for the same reason. Threading a directory through the whole
/// `path`/`base_dir`/`save_path`/`layouts`/`backends` surface and its GUI call sites would be a far
/// larger change than the defect, in the one crate a size ratchet polices.
///
/// # What it returns
///
/// `Err` when the declaration is refused or arrived too late to matter. `Ok(Some(notice))` when it
/// MOVED the family — the answer changed and the old directory still holds files — which the
/// binary logs once it has a subscriber. `Ok(None)` when nothing moved. This module logs nothing
/// itself: a root declares before `vike_log::init` in every binary that has one.
pub fn declare_project_state_dir(state_dir: Option<PathBuf>) -> Result<Option<String>, String> {
    let before = resolve_state_dir();
    let mut first = false;
    let declared = DECLARED_STATE_DIR.get_or_init(|| {
        first = true;
        state_dir.clone()
    });
    if declared.as_deref() != state_dir.as_deref() {
        return Err(format!(
            "the workspace family's project was already declared as {}; the second declaration \
             ({}) is IGNORED — one process has one workspace directory",
            state_dir_label(declared.as_deref()),
            state_dir_label(state_dir.as_deref())
        ));
    }
    // Only a declaration that WOULD have changed something can be late. Repeating the one already
    // in force is not a fault and must not be reported as one.
    if first && RESOLVED_UNDECLARED.load(std::sync::atomic::Ordering::Relaxed) {
        return Err(format!(
            "the workspace directory was already resolved as {} before this declaration, so the \
             declaration changed nothing — it has to run before the first workspace, layout or \
             backend-registry read",
            state_dir_label(before.as_deref())
        ));
    }
    Ok(relocation_notice(before.as_deref(), resolve_state_dir().as_deref()))
}

/// A state directory, for an operator-facing message. `None` is a real answer and has to read as
/// one rather than as an empty string.
fn state_dir_label(dir: Option<&std::path::Path>) -> String {
    match dir {
        Some(p) => p.display().to_string(),
        None => "<no project above the working directory>".to_string(),
    }
}

/// The family's members, named once — what a relocation actually moves. `layouts/` is a directory;
/// the rest are files.
pub(super) const FAMILY_MEMBERS: [&str; 4] = [
    WORKSPACE_FILE,
    LAYOUTS_SUBDIR,
    LAST_LAYOUT_FILE,
    crate::backend::backend_registry::BACKENDS_FILE,
];

/// ⚠ **The migration question, answered out loud rather than silently.** When a declaration moves
/// the family, an operator who was relying on the working-directory walk now reads a DIFFERENT
/// `workspace.json`, a different `layouts/`, a different `last_layout.txt` and a different
/// `backends.json` — a saved layout and a whole backend registry appearing to vanish.
///
/// Nothing is copied, moved or deleted: this workspace never relocates a file the operator wrote.
/// The notice names both directories and the members that exist at the old one, so the fix is a
/// copy the operator can make with their eyes open. Silence here would look exactly like data loss.
///
/// `None` when the answer did not change, or when the old location holds nothing to lose.
pub(super) fn relocation_notice(
    before: Option<&std::path::Path>,
    after: Option<&std::path::Path>,
) -> Option<String> {
    if before == after {
        return None;
    }
    let old = before?;
    let found: Vec<&str> = FAMILY_MEMBERS.into_iter().filter(|m| old.join(m).exists()).collect();
    if found.is_empty() {
        return None;
    }
    Some(format!(
        "the workspace family now resolves to {} (the settings directory this process booted \
         with), not {} (what the working directory walks up to). {} still sit(s) at the old \
         location and will NOT be read — nothing here moves or deletes them; copy them across if \
         you want them.",
        state_dir_label(after),
        state_dir_label(Some(old)),
        found.join(", ")
    ))
}

/// The project's STATE directory — `<project>/settings/state` — or `None` when neither a root's
/// declaration nor a walk above the working directory answers.
///
/// Two rungs, in order:
///
/// 1. **The COMPOSITION ROOT's declaration** ([`declare_project_state_dir`]) — the one walk the
///    boot already performed, which is the only rung that honours `$VIKE_SETTINGS_DIR`.
/// 2. **The walk**, from the working directory, for a process whose root declares nothing (a test,
///    a tool). `$VIKE_SETTINGS_DIR`-BLIND, which is why rung 1 exists above it.
///
/// There is no third location and no environment rung: `VIKE_STATE_ROOT` refuses startup (decision
/// 0111), and program-written JSON lands in the same folder as every other setting.
pub(super) fn state_dir() -> Option<PathBuf> {
    if DECLARED_STATE_DIR.get().is_none() {
        // Rung 2 is about to answer, so a later declaration is too late to have decided this call.
        RESOLVED_UNDECLARED.store(true, std::sync::atomic::Ordering::Relaxed);
    }
    resolve_state_dir()
}

/// [`state_dir`] WITHOUT the lateness bookkeeping, so [`declare_project_state_dir`] can ask what
/// the answer is (before and after) without making itself late.
fn resolve_state_dir() -> Option<PathBuf> {
    if let Some(declared) = DECLARED_STATE_DIR.get() {
        return declared.clone();
    }
    std::env::current_dir()
        .ok()
        .and_then(|cwd| vike_model::paths::state_path::project_state_dir(&cwd))
}

/// Workspace file path for READING: `$VIKE_WORKSPACE`, else `<state_dir>/workspace.json`.
///
/// `None` when neither resolves — no override, no declaration and no project above the working
/// directory. There is no file to read then, and [`load`] reports that as "no saved
/// workspace" rather than this resolver inventing a second location.
pub fn path() -> Option<PathBuf> {
    match workspace_override() {
        Some(p) => Some(p),
        None => Some(state_dir()?.join(WORKSPACE_FILE)),
    }
}

/// Workspace file path for WRITING: `$VIKE_WORKSPACE`, else `<state_dir>/workspace.json`, whose
/// directory is created lazily here.
///
/// `Err` — never a panic, and never a different location — when there is no state directory to
/// resolve or it cannot be created (a scrubbed environment, a read-only project). The caller
/// surfaces that to the user, who can then say where the file should go; a save that silently
/// landed somewhere else would be found by nothing afterwards.
pub(super) fn save_path() -> std::io::Result<PathBuf> {
    match workspace_override() {
        Some(p) => Ok(p),
        None => vike_model::paths::state_path::write_path(state_dir().as_deref(), WORKSPACE_FILE),
    }
}

/// Serialize + write the current layout; returns the path written.
pub fn save(
    wins: &[WinState],
    display_tz: DisplayTz,
    gpu_render: bool,
    indicator_favs: &[String],
) -> std::io::Result<PathBuf> {
    let p = save_path()?;
    save_to(wins, display_tz, gpu_render, indicator_favs, &p)?;
    Ok(p)
}

pub(super) fn save_to(
    wins: &[WinState],
    display_tz: DisplayTz,
    gpu_render: bool,
    indicator_favs: &[String],
    p: &std::path::Path,
) -> std::io::Result<()> {
    // Favourites (part b) are injected here rather than inside `capture` (which stays
    // window-layout-only — see `Workspace::indicator_favs`'s doc).
    let mut ws = capture(wins, display_tz, gpu_render);
    ws.indicator_favs = indicator_favs.to_vec();
    let json = serde_json::to_string_pretty(&ws).map_err(std::io::Error::other)?;
    std::fs::write(p, json)
}

/// Read + parse the workspace file; `None` if absent or unparseable (a corrupt
/// file must never brick startup — fall back to the default layout). A v1 file
/// parses successfully into the v2 structs via serde defaults (module docs).
pub fn load() -> Option<Workspace> {
    load_from(&path()?)
}

pub(super) fn load_from(p: &std::path::Path) -> Option<Workspace> {
    let raw = std::fs::read_to_string(p).ok()?;
    match serde_json::from_str::<Workspace>(&raw) {
        Ok(ws) => Some(ws),
        Err(e) => {
            tracing::warn!("workspace file unparseable, using default layout: {e}");
            None
        }
    }
}
