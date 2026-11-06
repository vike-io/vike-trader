//! The tree walk: each folder loaded by its tree's rule, every failure pushed as a named row.
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use vike_paths::state_path::RUST_SUBDIR;

use super::preset_files::{collect_presets, preset_names};
use super::{LoadDiagnostic, LoadReport, RHAI_EXT, StrategyBody, Tree, UserStrategy, has_ext};
use crate::{StrategySpec, build_strategy};

/// One tree's worth of scanning, appending to a shared `claimed` namespace and one `report`.
pub(super) fn scan_tree(
    root: &Path,
    tree: Tree,
    claimed: &mut BTreeMap<String, PathBuf>,
    report: &mut LoadReport,
) {
    let listing = match std::fs::read_dir(root) {
        Ok(listing) => listing,
        // The ordinary un-configured state: no `user_data/` yet, or no `strategies/<tree>` in it.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return,
        Err(e) => {
            report.diagnostics.push(LoadDiagnostic::RootUnreadable {
                root: root.to_path_buf(),
                error: e.to_string(),
            });
            return;
        }
    };

    let mut folders: Vec<(String, PathBuf)> = Vec::new();
    let mut strays: Vec<PathBuf> = Vec::new();
    // A per-ENTRY error carries no name to attach a diagnostic to — the `DirEntry` itself failed
    // to materialise, so we do not even know which file it was. It is an OS-level anomaly with no
    // actionable text, unlike the root failure above, which names the directory.
    for entry in listing.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().map(|n| n.to_string_lossy().into_owned()) else {
            continue;
        };
        // Dot-entries are tool droppings (`.git`, `.DS_Store`), not user content — skipping them
        // is a rule, not a silent loss.
        if name.starts_with('.') {
            continue;
        }
        if path.is_dir() {
            folders.push((name, path));
        } else if has_ext(&path, tree.ext()) {
            strays.push(path);
        }
    }
    folders.sort();
    strays.sort();

    for path in strays {
        report.diagnostics.push(LoadDiagnostic::StrayScript { path });
    }

    for (name, dir) in folders {
        let key = name.to_lowercase();
        if let Some(kept) = claimed.get(&key) {
            report.diagnostics.push(LoadDiagnostic::DuplicateName {
                name,
                kept: kept.clone(),
                ignored: dir,
            });
            continue;
        }
        // A folder that loaded NOTHING claims no name: a later folder with the same name is a real
        // strategy, not a duplicate of a failure.
        if let Some(loaded) = load_one(&dir, &name, tree, &mut report.diagnostics) {
            claimed.insert(key, dir);
            report.strategies.push(loaded);
        }
    }
}

/// Load ONE folder, appending a diagnostic for whatever stopped it. `None` means nothing loaded —
/// and in that case at least one diagnostic was pushed, WITH one deliberate exception named in the
/// `Rust` arm below.
fn load_one(
    dir: &Path,
    name: &str,
    tree: Tree,
    diags: &mut Vec<LoadDiagnostic>,
) -> Option<UserStrategy> {
    match tree {
        Tree::Rhai => load_rhai_folder(dir, name, diags),
        Tree::Rust => load_rust_folder(dir, name, diags),
    }
}

/// A `rhai/` folder: the entry file IS the strategy, so its absence is a real loss.
fn load_rhai_folder(
    dir: &Path,
    name: &str,
    diags: &mut Vec<LoadDiagnostic>,
) -> Option<UserStrategy> {
    let entry = dir.join(format!("{name}.{RHAI_EXT}"));
    if !entry.is_file() {
        // A folder named after a BUILT-IN strategy is not a broken script folder — it is a preset
        // folder in the wrong tree, and "rename the script" is advice about a script that should
        // not exist. Say what it actually is. (Only when it holds presets: an empty folder named
        // `grid/` is more likely someone starting a Rhai strategy than misfiling nothing.)
        if let Some(registry) = registry_name(name) {
            let presets = preset_names(dir);
            if !presets.is_empty() {
                diags.push(LoadDiagnostic::NativePresetsMisfiled {
                    name: registry.to_string(),
                    dir: dir.to_path_buf(),
                    expected_dir: rust_twin(dir, registry),
                });
                return None;
            }
        }
        diags.push(LoadDiagnostic::MissingEntry {
            name: name.to_string(),
            dir: dir.to_path_buf(),
            scripts: entries_in(dir, RHAI_EXT),
        });
        return None;
    }

    let source = match std::fs::read_to_string(&entry) {
        Ok(text) => text,
        Err(e) => {
            diags.push(LoadDiagnostic::UnreadableEntry {
                name: name.to_string(),
                path: entry,
                error: e.to_string(),
            });
            return None;
        }
    };

    // The same call a Run makes — see the module doc on why this is not `RhaiStrategy::compile`
    // reached for directly. The built strategy is dropped: what we wanted was the verdict.
    if let Err(e) = build_strategy(&StrategySpec::Rhai(source.clone())) {
        diags.push(LoadDiagnostic::CompileFailed {
            name: name.to_string(),
            path: entry,
            error: e.to_string(),
        });
        return None;
    }

    let presets = collect_presets(dir, name, diags);
    Some(UserStrategy {
        name: name.to_string(),
        dir: dir.to_path_buf(),
        body: StrategyBody::Rhai { entry, source },
        presets,
    })
}

/// A `rust/` folder: the code is in the BINARY, so there is no entry file to miss.
///
/// ⚠ The one place this loader returns `None` WITHOUT a diagnostic, and it is deliberate: a folder
/// naming no built-in strategy and holding no presets is a source-checkout Rust strategy that has
/// not been registered yet — which is the shipped `my_experiment/` template's exact state, and
/// `crates/vike-cli/src/cmd/init/content/readmes.rs`'s `RUST_README` already says in its first line that
/// nothing reads that tree at runtime. Nothing was skipped, because there was nothing here for a
/// runtime scan to load. The moment the folder holds PRESETS that changes — they are addressed by
/// a name that resolves to nothing, and that IS a loss.
fn load_rust_folder(
    dir: &Path,
    name: &str,
    diags: &mut Vec<LoadDiagnostic>,
) -> Option<UserStrategy> {
    let Some(registry) = registry_name(name) else {
        let presets = preset_names(dir);
        if !presets.is_empty() {
            diags.push(LoadDiagnostic::UnregisteredNative {
                name: name.to_string(),
                dir: dir.to_path_buf(),
                presets,
            });
        }
        return None;
    };
    // The REGISTRY's spelling, not the folder's: `strategy_by_name` matches exactly, and a folder
    // created as `Buy_Hold/` on a case-folding filesystem still means `buy_hold`.
    let presets = collect_presets(dir, registry, diags);
    Some(UserStrategy {
        name: registry.to_string(),
        dir: dir.to_path_buf(),
        body: StrategyBody::Native,
        presets,
    })
}

/// The built-in strategy `folder` names, in the REGISTRY's own spelling — or `None` if it names
/// none.
///
/// Case-insensitive for the same reason [`RHAI_EXT`] is: the folder name came off a filesystem, and
/// a Windows checkout that folded `Grid/` to `grid/` (or the other way) has not changed which
/// strategy the user meant. The roster is `crates/vike-studio-core/src/spec.rs`'s
/// `native_strategies`, which is `vike_backtest::harness::registry::STRATEGIES` itself — so a
/// strategy added to the registry becomes a legal preset folder for free.
fn registry_name(folder: &str) -> Option<&'static str> {
    crate::spec::native_strategies().iter().copied().find(|n| n.eq_ignore_ascii_case(folder))
}

/// Where a misfiled `rhai/<name>/` preset folder belongs: the same name under the sibling `rust/`
/// tree. Falls back to a bare `rust/<name>` when `dir` has no grandparent to hang it off, which
/// only happens for a hand-constructed root — the message is still readable.
fn rust_twin(dir: &Path, name: &str) -> PathBuf {
    dir.parent()
        .and_then(Path::parent)
        .map(|strategies| strategies.join(RUST_SUBDIR).join(name))
        .unwrap_or_else(|| Path::new(RUST_SUBDIR).join(name))
}

/// Every file name with extension `ext` in `dir`, sorted — what [`LoadDiagnostic::MissingEntry`]
/// shows so a misnaming explains itself.
fn entries_in(dir: &Path, ext: &str) -> Vec<String> {
    let Ok(listing) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut found: Vec<String> = listing
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_file() && has_ext(p, ext))
        .filter_map(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
        .collect();
    found.sort();
    found
}
