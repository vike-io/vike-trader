//! The presets beside a strategy: the two positions they are found in, read in name order.
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use super::{LoadDiagnostic, Preset, TOML_EXT, check_preset_shape, has_ext};

/// The optional preset sub-directory — the filed position, honoured beside the flat one.
const PRESETS_SUBDIR: &str = "presets";

/// Every preset STEM in `dir` and its `presets/` sub-directory, sorted — the "what was found"
/// half of the two diagnostics that report a folder whose presets reach nothing.
pub(super) fn preset_names(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = toml_files(dir)
        .into_iter()
        .chain(toml_files(&dir.join(PRESETS_SUBDIR)))
        .filter_map(|p| p.file_stem().map(|s| s.to_string_lossy().into_owned()))
        .collect();
    names.sort();
    names.dedup();
    names
}

/// The presets for one strategy: the flat position first, then `presets/`.
///
/// The flat position WINS a name clash, because it is the documented primary one — and the loser
/// is reported rather than dropped, since two files claiming one preset name is a mistake in the
/// tree, not a preference to be silently resolved.
pub fn collect_presets(dir: &Path, strategy: &str, diags: &mut Vec<LoadDiagnostic>) -> Vec<Preset> {
    let mut presets: Vec<Preset> = Vec::new();
    let mut claimed: BTreeMap<String, PathBuf> = BTreeMap::new();

    for path in toml_files(dir).into_iter().chain(toml_files(&dir.join(PRESETS_SUBDIR))) {
        let Some(name) = path.file_stem().map(|s| s.to_string_lossy().into_owned()) else {
            continue;
        };
        let key = name.to_lowercase();
        if let Some(kept) = claimed.get(&key) {
            diags.push(LoadDiagnostic::ShadowedPreset {
                name: strategy.to_string(),
                preset: name,
                kept: kept.clone(),
                ignored: path,
            });
            continue;
        }
        match read_preset(&path) {
            Ok(params) => {
                claimed.insert(key, path.clone());
                presets.push(Preset { name, path, params });
            }
            Err(error) => {
                diags.push(LoadDiagnostic::BadPreset { name: strategy.to_string(), path, error })
            }
        }
    }
    presets
}

/// Read + parse + shape-check one preset. All three failure modes collapse into one message on
/// purpose: to the user they are the same event — "this preset is not usable, here is why" — and
/// splitting them would buy diagnostic variants that read identically.
fn read_preset(path: &Path) -> Result<toml::Value, String> {
    let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    let value = toml::from_str::<toml::Value>(&text).map_err(|e| e.to_string())?;
    check_preset_shape(&value)?;
    Ok(value)
}

/// Every `.toml` file directly in `dir`, sorted by name. A missing directory is an empty list —
/// `presets/` is optional by design.
fn toml_files(dir: &Path) -> Vec<PathBuf> {
    let Ok(listing) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut found: Vec<PathBuf> = listing
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_file() && has_ext(p, TOML_EXT))
        .filter(|p| !p.file_name().is_some_and(|n| n.to_string_lossy().starts_with('.')))
        .collect();
    found.sort();
    found
}
