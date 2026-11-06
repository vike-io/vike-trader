//! Named layouts: `layouts/<name>.json` beside the workspace file, and the `last_layout.txt` pointer.

use super::Workspace;
#[cfg(doc)]
use super::store::load;
use super::store::{load_from, save_to, state_dir, workspace_override};
use crate::ui::workspace::state::WinState;
use std::path::PathBuf;
use vike_chart::DisplayTz;

// ---------------------------------------------------------------------------
// Named / multiple saved layouts (TradingView-style named layouts)
//
// The single `workspace.json` above stays the DEFAULT / auto-saved layout —
// still loaded at startup, still what File → Save/Open Workspace writes/reads.
// Named layouts live ALONGSIDE it as individual files under a sibling
// `layouts/` directory (`<name>.json`, the SAME `Workspace` serde schema as the
// single file, so all the v1/v2 forward-compat above applies to them verbatim).
// A tiny pointer file (`last_layout.txt`) records the last layout explicitly
// loaded/saved-as so a restart can reopen it. Names are sanitized into safe
// filename stems; the stem IS the displayed layout name (a lossless,
// path-injection-proof mapping).
// ---------------------------------------------------------------------------

/// The `layouts/` sub-directory name and the pointer file's basename, stated once.
pub(super) const LAYOUTS_SUBDIR: &str = "layouts";
pub(super) const LAST_LAYOUT_FILE: &str = "last_layout.txt";

/// The named-layout family's base directory: `$VIKE_WORKSPACE`'s directory — the override moves
/// the whole family together, which is why it short-circuits — else [`state_dir`].
///
/// ONE base for `layouts/`, `last_layout.txt` AND the backend registry's `backends.json`
/// (`crate::backend::backend_registry`, which is why this is `pub(crate)`), so the family can never
/// half-move. `None` when neither resolves, which every member below reports as "no layouts"
/// rather than reaching for a directory of its own.
pub(crate) fn base_dir() -> Option<PathBuf> {
    match workspace_override() {
        Some(p) => Some(p.parent().map(|d| d.to_path_buf()).unwrap_or_default()),
        None => state_dir(),
    }
}

/// Directory named-layout files live in: `<base_dir>/layouts/`.
pub fn layouts_dir() -> Option<PathBuf> {
    Some(base_dir()?.join(LAYOUTS_SUBDIR))
}

/// Pointer file recording the last layout name explicitly loaded or saved-as,
/// so a restart can reopen it: `<base_dir>/last_layout.txt`.
pub(super) fn last_layout_path() -> Option<PathBuf> {
    Some(base_dir()?.join(LAST_LAYOUT_FILE))
}

/// Sanitize a user-entered layout name into a safe filename STEM: keep
/// alphanumerics, dash, underscore and space; every other character (path
/// separators, dots, control chars, …) becomes `_`; collapse internal
/// whitespace runs to a single space; trim leading/trailing space/underscore/
/// dot; cap the length. Returns `""` for a name that sanitizes to nothing (the
/// empty/whitespace-only case) — callers treat that as "no valid name".
pub fn sanitize_layout_name(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut prev_space = false;
    for ch in name.trim().chars() {
        let keep = if ch.is_alphanumeric() || ch == '-' || ch == '_' {
            ch
        } else if ch.is_whitespace() {
            ' '
        } else {
            '_'
        };
        // collapse runs of spaces (only spaces — not underscores/dashes)
        if keep == ' ' {
            if prev_space {
                continue;
            }
            prev_space = true;
        } else {
            prev_space = false;
        }
        out.push(keep);
        if out.chars().count() >= 64 {
            break;
        }
    }
    out.trim_matches(|c: char| c == ' ' || c == '_' || c == '.').to_string()
}

/// Absolute path of the named layout `name`, or `None` if the name sanitizes to nothing
/// (empty/whitespace/all-punctuation) or no [`layouts_dir`] resolves.
///
/// The same path for reads and writes — this is where a layout of that name lives.
pub fn layout_path(name: &str) -> Option<PathBuf> {
    let stem = sanitize_layout_name(name);
    if stem.is_empty() {
        return None;
    }
    Some(layouts_dir()?.join(format!("{stem}.json")))
}

/// The saved named layouts (sanitized stems), sorted case-insensitively.
/// A missing directory ⇒ empty list (never an error — the feature is simply unused yet).
/// Non-`.json` entries and unreadable names are skipped.
pub fn list_layouts() -> Vec<String> {
    layouts_dir().map(|d| list_layouts_in(&d)).unwrap_or_default()
}

pub(super) fn list_layouts_in(dir: &std::path::Path) -> Vec<String> {
    let mut names: Vec<String> = match std::fs::read_dir(dir) {
        Ok(rd) => rd
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some("json"))
            .filter_map(|e| e.path().file_stem().and_then(|s| s.to_str()).map(String::from))
            .collect(),
        Err(_) => Vec::new(),
    };
    names.sort_by_key(|n| n.to_lowercase());
    names.dedup();
    names
}

/// Save the current layout as the named layout `name`, creating `layouts/` if
/// needed. Returns the path written, or an error (invalid name, or IO). The
/// name is also recorded as the last-active layout (best-effort).
pub fn save_layout(
    name: &str,
    wins: &[WinState],
    display_tz: DisplayTz,
    gpu_render: bool,
    indicator_favs: &[String],
) -> std::io::Result<PathBuf> {
    let p = layout_path(name).ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "empty/invalid layout name")
    })?;
    if let Some(dir) = p.parent() {
        std::fs::create_dir_all(dir)?;
    }
    save_to(wins, display_tz, gpu_render, indicator_favs, &p)?;
    set_last_layout(name);
    Ok(p)
}

/// Load the named layout `name`; `None` if the name is invalid, the file is
/// absent, or it is unparseable (same never-brick contract as [`load`]). Records
/// the name as last-active on a successful load (best-effort).
pub fn load_layout(name: &str) -> Option<Workspace> {
    let p = layout_path(name)?;
    let ws = load_from(&p);
    if ws.is_some() {
        set_last_layout(name);
    } else if !p.exists() {
        tracing::warn!("layout '{}' not found at {}", name, p.display());
    }
    ws
}

/// Delete the named layout `name`. `Ok(())` if the file was removed OR was
/// already absent (idempotent); an invalid name is an error. Clears the
/// last-active pointer if it named the deleted layout.
pub fn delete_layout(name: &str) -> std::io::Result<()> {
    let stem = sanitize_layout_name(name);
    if stem.is_empty() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "empty/invalid layout name",
        ));
    }
    if let Some(p) = layout_path(&stem) {
        match std::fs::remove_file(&p) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
    }
    if last_layout().as_deref() == Some(stem.as_str())
        && let Some(p) = last_layout_path()
    {
        let _ = std::fs::remove_file(p);
    }
    Ok(())
}

/// Record `name` (sanitized) as the last explicitly-used layout. Best-effort:
/// a write failure is logged, never propagated (it must not fail a save/load).
fn set_last_layout(name: &str) {
    let stem = sanitize_layout_name(name);
    if stem.is_empty() {
        return;
    }
    let Some(p) = last_layout_path() else { return };
    if let Err(e) = std::fs::write(p, &stem) {
        tracing::warn!("could not record last layout: {e}");
    }
}

/// The last explicitly-used layout name, if the pointer file exists AND that
/// layout still exists on disk (a stale pointer to a deleted layout is ignored).
pub fn last_layout() -> Option<String> {
    let raw = std::fs::read_to_string(last_layout_path()?).ok()?;
    let name = raw.trim();
    let stem = sanitize_layout_name(name);
    if stem.is_empty() {
        return None;
    }
    match layout_path(&stem) {
        Some(p) if p.exists() => Some(stem),
        _ => None,
    }
}
