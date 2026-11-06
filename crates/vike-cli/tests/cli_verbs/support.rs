//! What more than one `cli_verbs` member shares: the throwaway settings project
//! `config_set_cli` and `config_unset_cli` both build, and the two ways they read back what the
//! verb did (one settings row, the change journal). Plain functions over the `settings/`
//! directory, so each member keeps its own `Case` and its own method names.

use std::path::{Path, PathBuf};

/// A throwaway PROJECT and its `settings/` child: a BOUND `tempfile::TempDir` (named `<prefix>…`,
/// so a leftover from a kill signal is attributable) whose `Drop` removes the tree even on a panic
/// path, and the created `settings/` directory `$VIKE_SETTINGS_DIR` names, so `<project>` is the
/// bound root rather than the shared system temp directory.
pub(crate) fn settings_project(prefix: &str) -> (tempfile::TempDir, PathBuf) {
    let root = tempfile::Builder::new().prefix(prefix).tempdir().expect("tempdir");
    let dir = root.path().join("settings");
    std::fs::create_dir_all(&dir).unwrap();
    (root, dir)
}

/// One settings row's value in the settings database under `dir`, dotted-key spelled — the
/// row-native replacement for reading a settings FILE back.
pub(crate) fn read_setting(dir: &Path, key: &str) -> Option<String> {
    let (section, leaf) = key.split_once('.')?;
    let rows = vike_secrets::read_settings_in(dir).ok()?;
    rows.rows()?
        .settings
        .iter()
        .find(|r| r.section == section && r.key == leaf)
        .map(|r| r.value.clone())
}

/// Everything the change journal wrote under `<dir>/state`, concatenated. The ledger's layout is
/// its own business, so this reads what it wrote rather than reconstructing a path.
pub(crate) fn journal(dir: &Path) -> String {
    let mut out = String::new();
    let mut stack = vec![dir.join("state")];
    while let Some(d) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&d) else { continue };
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if let Ok(t) = std::fs::read_to_string(&p) {
                out.push_str(&t);
            }
        }
    }
    out
}
