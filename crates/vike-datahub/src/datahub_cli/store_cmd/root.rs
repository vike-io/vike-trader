//! The hist-store root the local `store` verbs act on: `--store`, else the `config.store_root` row,
//! else the project's own folder — the daemon's ladder (`super::super::boot`'s `resolve_hist_store`).
//!
//! EVERY writer (`fetch-starter`, `rm`, `repair`, `rename`) takes the row at the rung the daemon gives
//! it. Without it a verb resolved off the working directory and acted on a DIFFERENT tree than the one
//! the running datahub serves: `rename --dry-run` answered "nothing to rename" beside the CI box's real
//! tape (2026-11-17). `--store` still names a store outright and reads no setting, so a broken settings
//! tree stops no repair that names its store.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// The hist-store root: `explicit` (`--store`) > `configured` (the `config.store_root` row, for the
/// verbs the module doc names) > `<repo>/market_data/hist` when this box still has that checkout > the
/// project's own `<project>/market_data/hist` > the per-user `…/vike-data`. No variable is a rung
/// (decision 0111 retired `VIKE_HIST_STORE`).
///
/// `vars` is the process environment as the CALLER collected it, and `cwd` its working directory, per
/// the settings-registry rule that libraries take configuration as parameters.
///
/// The repo fallback is resolved from `CARGO_MANIFEST_DIR` ancestors — NEVER the current
/// working directory. A CWD-relative default is exactly the bug this helper replaces: run a
/// bin from outside the repo and `HistoryStoreDataFusion::open("market_data/hist")` silently CREATES an
/// empty store wherever you happen to stand, so the run reports zero data instead of
/// failing.
///
/// ⚠ The project rung IS resolved from the working directory, which looks like that same bug and
/// is its opposite: the walk requires a project MARKER (`crates/vike-paths/src/state_path.rs`'s
/// `nearest_project_marker`), so standing somewhere that is not a project yields `None` and falls
/// through. A CWD-relative LITERAL creates a store wherever you stand; a walk for a marker either
/// finds the project or admits there is none. `$VIKE_SETTINGS_DIR` pins that project outright, and
/// `resolve_store_root_from` honours it here without this crate naming it.
///
/// **The resolution is LOGGED**, root and rung both. A store does not merge: if the default moves,
/// the old store is simply no longer read and the run reports zero rows — which reads exactly like
/// "there is no data for that range". One line at startup is the difference between an operator
/// seeing that and hunting it.
pub(super) fn store_root(
    explicit: Option<PathBuf>,
    configured: Option<String>,
    vars: &HashMap<String, String>,
    cwd: Option<&Path>,
) -> PathBuf {
    store_root_resolved(explicit, configured, vars, cwd).into_path()
}

/// [`store_root`] keeping the [`StoreRoot`](vike_paths::store_path::StoreRoot) — the path AND the
/// rung that chose it.
///
/// ⚠ It exists because a DESTRUCTIVE verb has to answer "which store" on STDOUT before it answers
/// "which series", and the `tracing::info!` inside [`store_root`] is silenced by `RUST_LOG`. A
/// store does not merge: a resolution that moved is invisible until it has destroyed the wrong
/// tree. `store rm` leads its plan with `StoreRoot`'s `Display`, which is exactly this value.
///
/// [`store_root`] is the same call with the rung dropped, so the two cannot resolve differently —
/// which is the whole reason this is an accessor rather than a second resolution at the call site.
pub(super) fn store_root_resolved(
    explicit: Option<PathBuf>,
    configured: Option<String>,
    vars: &HashMap<String, String>,
    cwd: Option<&Path>,
) -> vike_paths::store_path::StoreRoot {
    // ⚠ A BLANK `--store` falls through instead of resolving the store to `""`.
    //
    // `vike_paths::store_path::resolve_store_root` honours an EXPLICIT path unfiltered (the `Explicit`
    // rung returns before the `.filter(|s| !s.trim().is_empty())` that guards the configured rung),
    // so `""` would reach `HistoryStoreDataFusion::open`, which `create_dir_all`s whatever it is given — a
    // store minted in the working directory, and a run reporting zero rows. `Program` refuses a blank
    // value at the parse, so this is the guard for a caller that did not read its argv through it.
    let explicit =
        explicit.filter(|p| !p.as_os_str().is_empty() && !p.to_string_lossy().trim().is_empty());
    let resolved = resolve_with(explicit, configured, repo_default().as_deref(), cwd, vars);
    tracing::info!(
        store = %resolved.root.display(),
        rung = resolved.rung.as_str(),
        "hist store root resolved: {}",
        resolved.rung.why()
    );
    if let Some(note) = row_outranks_a_second_store(&resolved, vars, cwd) {
        tracing::warn!("{note}");
        eprintln!("{note}");
    }
    resolved
}

/// When the row answered and the folder the ladder takes WITHOUT it is a store on disk too, the
/// sentence naming both. The row's store wins: it is the tree the datahub serves.
fn row_outranks_a_second_store(
    resolved: &vike_paths::store_path::StoreRoot,
    vars: &HashMap<String, String>,
    cwd: Option<&Path>,
) -> Option<String> {
    if resolved.rung != vike_paths::store_path::StoreRootRung::Configured {
        return None;
    }
    let other = resolve_with(None, None, repo_default().as_deref(), cwd, vars);
    let same =
        std::fs::canonicalize(&resolved.root).ok() == std::fs::canonicalize(&other.root).ok();
    (resolved.root.is_dir() && other.root.is_dir() && !same).then(|| {
        format!(
            "note: two stores exist. The config.store_root row names {}, and {} is {}. Acting on \
             the row's store, the one the datahub serves; pass --store to name the other.",
            resolved.root.display(),
            other.root.display(),
            other.rung.why()
        )
    })
}

/// `<repo>/market_data/hist` for THIS crate — `CARGO_MANIFEST_DIR` ancestors, never the CWD —
/// **in a debug build; `None` in a release build.**
///
/// ⚠ The macro is a string literal in the binary, and `release.yml` builds the multicall that links
/// it `--release` on a runner whose checkout path names the runner account. The literal survives
/// `--remap-path-prefix` (that flag rewrites what the COMPILER emits, never what a crate embeds
/// itself), so the release's box-path guard (`scripts/refuse_box_paths.sh`) would refuse the asset by
/// name. A release build therefore carries no rung at all and resolves from the project walk down —
/// the same directory, by a different rung name, for anyone standing in a checkout;
/// `vike_paths::store_path`'s module doc (rung 4) argues it. The attribute rather than `cfg!()`,
/// because a runtime `if` still compiles the literal in.
fn repo_default() -> Option<PathBuf> {
    #[cfg(debug_assertions)]
    {
        Some(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .ancestors()
                .nth(2)
                .unwrap()
                .join("market_data")
                .join("hist"),
        )
    }
    #[cfg(not(debug_assertions))]
    {
        None
    }
}

/// The WIRING, with `repo_default` and `cwd` injected so a test can reach the rungs a `cargo test`
/// run can never otherwise see.
///
/// The compile-time repo path always EXISTS during `cargo test`, so the dev-checkout hinge always
/// fires and the project/user rungs are unreachable from a test of the public function. Taking them
/// as parameters is what lets `the_call_site_reaches_the_project_rung_when_this_box_has_no_checkout`
/// prove that this crate's call site is wired to the shared precedence and not to a local restatement
/// of it.
///
/// ⚠ `resolve_store_root_from`, never the bare ladder: the ladder's project/user defaults are two
/// adjacent `Option<PathBuf>`s that a transposition would swap SILENTLY. Passing the working
/// directory and the environment map instead leaves no two arguments here of the same type.
fn resolve_with(
    explicit: Option<PathBuf>,
    configured: Option<String>,
    repo_default: Option<&Path>,
    cwd: Option<&Path>,
    vars: &HashMap<String, String>,
) -> vike_paths::store_path::StoreRoot {
    vike_paths::store_path::resolve_store_root_from(explicit, configured, repo_default, cwd, vars)
}

#[path = "root_tests.rs"]
#[cfg(test)]
mod root_tests;
