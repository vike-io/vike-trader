//! Where a strategy file's built ARTIFACT lives: `<builds>/sdk-N/<folder>/<sha256>.so`, the one writer
//! and the one reader (plan 2b: every strategy is a file; decision 0296: every build names its SDK).
//!
//! `<builds>` is `<project>/user_data/strategy-builds` (`vike_paths::state_path::STRATEGY_BUILDS_SUBDIR`
//! under the user data folder). `sdk-N` is the SDK the file built against (`crate::sdk::Sdk`'s
//! `directory_name`, `sdk-dev` for a git checkout). `<sha256>` is the hash of the file's TEXT, so two
//! identical files share one artifact and an edit is a new one. `<folder>` ([`build_folder`]) names
//! everything else an artifact was built under: this crate's `ABI_VERSION`, the toolchain fingerprint, the
//! strategy boundary (`crate::sdk::BOUNDARY_HASH`), the profile, the SDK's source hash for the template
//! kind, and the recipe (what the builder writes around the file and how it runs cargo). A deploy that
//! cut no SDK changes none of those and serves every artifact already built; a change to any of them
//! starts a new folder and never serves an artifact built before it. Each kind's persistent cargo target
//! sits beside the folders, `<builds>/sdk-N/work-<folder>{,-onnx,-study}/target` ([`work_directory`]),
//! off the tmpfs and never touched by a deploy. The builder writes the path
//! (`crates/vike-strategy-builder/src/render.rs`'s `build_strategy_file`) and a host that loads an
//! artifact by its sha checks its shape (the sandboxed child, `crates/vike-strategy-sandbox/src/lib.rs`'s
//! `artifact_of`: no hub links this crate); this crate sits below both.
//!
//! ⚠ The suffix is `.so` on EVERY host, not the platform's `DLL_SUFFIX`: the design's own naming.

use std::path::{Path, PathBuf};

use vike_utility::fnv::Fnv1a64;

use crate::sdk::{Sdk, TemplateKind};

/// The hash of the SOURCES the cdylib templates compile against, as this binary was built from
/// them: every crate the two template manifests reach by `path` (dev-dependencies aside), the root
/// manifest's dependency table and the builder's renderer, baked by `build.rs` (its
/// `build/source_hash.rs` says exactly what is read). Every binary built from one tree carries the
/// same value.
///
/// ⚠ It names the tree this BINARY was compiled from, not an SDK's: a build folder is named by the
/// SDK's own per-kind hash (`crate::sdk::SdkMarker`'s `source_hash`, or `crate::sdk::source_hash` of a
/// dev checkout). It stays for the "changes pending since the newest SDK" answer.
pub const SOURCE_HASH: &str = env!("VIKE_STRATEGY_SOURCE_HASH");

/// The prefix of a kind's work directory beside the build folders ([`work_directory`]).
pub const WORK_DIRECTORY_PREFIX: &str = "work-";

/// The folder an artifact built under `profile` (`"release"` or `"debug"`) lands in, inside its SDK's
/// builds: [`folder_name`] over this crate's ABI version, the toolchain fingerprint, this host's
/// boundary hash, the profile, `sdk_source_hash` (the SDK's source hash for the artifact's template kind,
/// `crate::sdk::SdkSourceHashes::of`) and `recipe_hash` ([`recipe_hash`] of what the builder writes around
/// the file and how it runs cargo). One hash per kind, so a study-only crate never moves a strategy's folder.
pub fn build_folder(profile: &str, sdk_source_hash: &str, recipe_hash: &str) -> String {
    folder_name(
        crate::abi::ABI_VERSION,
        crate::fingerprint::FINGERPRINT,
        crate::sdk::BOUNDARY_HASH,
        profile,
        sdk_source_hash,
        recipe_hash,
    )
}

/// Eight lowercase hex digits of FNV-1a 64 (`vike_utility::fnv`, a frozen algorithm, so the name is
/// the same in every process and release that computes it) over the ABI version and the other parts,
/// each fed with a terminator so no two inputs run together.
fn folder_name(
    abi_version: u32,
    fingerprint: &str,
    boundary_hash: &str,
    profile: &str,
    sdk_source_hash: &str,
    recipe_hash: &str,
) -> String {
    let abi_version = abi_version.to_string();
    let parts =
        [abi_version.as_str(), fingerprint, boundary_hash, profile, sdk_source_hash, recipe_hash];
    format!("{:016x}", fold(&parts))[..8].to_string()
}

/// Sixteen lowercase hex digits of FNV-1a 64 over `parts`, each fed with a terminator: the recipe a build
/// ran (the builder's rendered `Cargo.toml`, its `lib.rs` around the file, the cargo argv), one input of
/// [`build_folder`]. Which parts make the recipe is the builder's to say.
pub fn recipe_hash(parts: &[&str]) -> String {
    format!("{:016x}", fold(parts))
}

fn fold(parts: &[&str]) -> u64 {
    let mut h = Fnv1a64::new();
    for part in parts {
        h.write(part.as_bytes());
        h.write(&[0]);
    }
    h.finish()
}

/// The profile THIS binary was compiled under, in [`build_folder`]'s words (`"release"` or
/// `"debug"`), read off the toolchain fingerprint the loader compares: the profile a host that loads
/// an artifact by its sha expects.
pub fn host_profile() -> &'static str {
    if crate::fingerprint::FINGERPRINT.contains("profile=release") { "release" } else { "debug" }
}

/// The artifact built against `sdk` from text hashing to `sha` (sha256, lowercase hex) in `folder` under
/// `builds`: `<builds>/sdk-N/<folder>/<sha>.so`.
pub fn artifact_path(builds: &Path, sdk: Sdk, folder: &str, sha: &str) -> PathBuf {
    builds.join(sdk.directory_name()).join(folder).join(format!("{sha}.so"))
}

/// The persistent work directory the `kind` builds of `folder` against `sdk` share:
/// `<builds>/sdk-N/work-<folder>{,-onnx,-study}`. Its `target/` keeps the SDK's compiled closure between
/// builds; a target only ever sees ONE immutable source tree, so cargo's mtime check is sound there.
pub fn work_directory(builds: &Path, sdk: Sdk, folder: &str, kind: TemplateKind) -> PathBuf {
    builds
        .join(sdk.directory_name())
        .join(format!("{WORK_DIRECTORY_PREFIX}{folder}{}", kind.work_suffix()))
}

/// The bytes a research STUDY file's artifact is named by (its sha256 is the `<sha>` of
/// [`artifact_path`]): the study's name, then each of its folder's `.rs` files as `(file name,
/// text)` in the order given (the caller sorts them by name), each length-prefixed, after a tag no
/// strategy file's text can start with. So a study and a strategy never name one artifact, an edit to
/// ANY file of the study is a new artifact, and the same folder is the same artifact.
pub fn study_identity(name: &str, files: &[(String, String)]) -> Vec<u8> {
    let mut bytes = b"\0vike research study\0".to_vec();
    let mut part = |b: &[u8]| {
        bytes.extend_from_slice(&(b.len() as u64).to_le_bytes());
        bytes.extend_from_slice(b);
    };
    part(name.as_bytes());
    for (file, text) in files {
        part(file.as_bytes());
        part(text.as_bytes());
    }
    bytes
}

#[path = "artifact_tests.rs"]
#[cfg(test)]
mod artifact_tests;
