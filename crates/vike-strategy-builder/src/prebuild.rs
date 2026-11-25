//! The deploy's PREBUILD of the strategy files the tradehub mounts (the owner, 2026-11-19: "for live - do in
//! advance"): `vike-backend strategy-builder --sdk-root R --out-dir D --prebuild-mounts STRATEGIES`, run by
//! `deploy/docker/vikectl`'s pre-flight on the NEW build while the old tradehub still runs (`crate::builder::run`
//! takes the flag). After the restart a mounted file then finds its artifact and starts at once, instead of
//! waiting deactivated while it builds (about 0.7 s a file, and 10 to 19 s for the first under a new SDK).
//!
//! **What is built.** Every `.rs` file a mount of the ACTIVE daemon profile names (`vike_secrets::profile_store`'s
//! `read_profiles`, the rows the tradehub mounts from), each once, read from `STRATEGIES` (the project's
//! `strategies/`) as the mount names it — paper, demo and live mounts alike — unless the mount's account is stored
//! INACTIVE (`vike_secrets::AccountRows::active_tier`: an account that does not run mounts nothing). A `.rhai`
//! mount needs no build, and a backtest stays lazy.
//!
//! **What it never does.** It never writes over an artifact the box holds: a build lands at
//! `<builds>/sdk-N/<folder>/<sha256>.so`, named by the SDK, the toolchain, the template and the text, so a file
//! already built there is a cache hit (`render::build_strategy_file`'s cache check stamps its use and writes no
//! byte), and a new SDK or a new folder is a path the old daemons never mapped. The one rewrite the builder knows
//! is of an artifact its own record check refuses (a stale `.so` under that same name), published by a rename, so a
//! process holding the old one keeps its inode. It prunes nothing (the new builder prunes at its own start, after
//! the restart), needs no key, binds no port, and it never fails a deploy: whatever each file did is one line, and
//! the process exits 0. The deploy bounds the whole step (`vikectl`'s `PREBUILD_TIMEOUT`).
//!
//! **Where it reads.** The settings database and the strategy files are read BEFORE the build jail
//! (`crate::builder`'s `enter_jail`), which hides both; the builds run inside it, as every daemon build does.

use std::path::Path;

use vike_secrets::profile_store::{MountRow, ProfileKind, read_profiles};
use vike_secrets::{AccountRowActiveTier, AccountRows, CredentialStore};
use vike_strategy::file::scan::{StrategyFileLanguage, language_of, resolve};

use crate::builder::BuildSources;
use crate::render;

/// One mounted `.rs` file: its path as the mount names it (under `strategies/`), and its text, or why it
/// could not be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MountedFile {
    pub file: String,
    pub text: Result<String, String>,
}

/// Every `.rs` file the active daemon profile of `store` mounts on an account that is not stored inactive,
/// each once, in mount order, read from `strategies`. `Err` in words when the store cannot be read; an absent
/// store, no active daemon profile or no `.rs` mount is an empty list.
pub fn mounted_files(
    store: &CredentialStore,
    strategies: &Path,
) -> Result<Vec<MountedFile>, String> {
    let Some(location) = store.location() else { return Ok(Vec::new()) };
    let db = vike_secrets::db_path_in(location.settings_dir());
    let profiles = read_profiles(&db).map_err(|e| format!("the settings database: {e}"))?;
    let Some(daemon) = profiles.active(ProfileKind::Daemon) else { return Ok(Vec::new()) };
    // A store that cannot say which accounts are active builds every mount: a file that will not run costs
    // one build, never a refusal.
    let accounts = store.accounts().ok();
    Ok(rust_files(&daemon.mounts, accounts.as_ref())
        .into_iter()
        .map(|file| MountedFile {
            file: file.to_string(),
            text: resolve(strategies, file).and_then(|path| {
                std::fs::read_to_string(&path)
                    .map_err(|e| format!("cannot read {}: {e}", path.display()))
            }),
        })
        .collect())
}

/// The `.rs` files `mounts` name, each once in mount order, but a mount's whose account `accounts` holds
/// INACTIVE; a `.rhai` file and a mount naming no file are skipped.
fn rust_files<'a>(mounts: &'a [MountRow], accounts: Option<&AccountRows>) -> Vec<&'a str> {
    let mut files: Vec<&str> = Vec::new();
    for mount in mounts {
        let Some(file) = mount.strategy_file.as_deref() else { continue };
        let inactive = accounts.and_then(|a| a.active_tier(&mount.venue, mount.account.as_deref()))
            == Some(AccountRowActiveTier::Inactive);
        if language_of(Path::new(file)) == Some(StrategyFileLanguage::Rust)
            && !inactive
            && !files.contains(&file)
        {
            files.push(file);
        }
    }
    files
}

/// Build each of `files` as the daemon builds a mount's file (the SDK its text resolves to, a release build
/// into `out_dir`), saying one line a file on stderr. Returns how many were built.
pub fn build_all(
    files: &[MountedFile],
    sources: &BuildSources,
    out_dir: &Path,
    cargo_bin: &str,
    child_env: &[(String, String)],
    cargo_home_pin: &render::CargoHomePin,
) -> usize {
    let mut built = 0;
    for MountedFile { file, text } in files {
        let answer = text.as_ref().map_err(String::clone).and_then(|text| {
            sources
                .tree_for(Some(text.as_str()), None)
                .and_then(|tree| {
                    render::build_strategy_file(
                        text,
                        file,
                        out_dir,
                        &tree,
                        cargo_bin,
                        child_env,
                        render::Profile::Release,
                        cargo_home_pin,
                    )
                })
                .map_err(|e| match e {
                    // rustc's own words are the mount's to show after the restart; one line here.
                    render::BuildError::Compile { .. } => "it does not compile".to_string(),
                    other => other.to_string(),
                })
        });
        match answer {
            Ok(artifact) => {
                built += 1;
                let shown = artifact.strip_prefix(out_dir).unwrap_or(&artifact);
                eprintln!("vike-strategy-builder: prebuild {file}: built {}", shown.display());
            }
            Err(why) => eprintln!(
                "vike-strategy-builder: prebuild {file}: NOT built, it builds when the tradehub mounts it: {}",
                why.replace(['\r', '\n'], " ")
            ),
        }
    }
    eprintln!(
        "vike-strategy-builder: prebuild: {built} of {} mounted strategy files built",
        files.len()
    );
    built
}

#[path = "prebuild_tests.rs"]
#[cfg(test)]
mod prebuild_tests;
