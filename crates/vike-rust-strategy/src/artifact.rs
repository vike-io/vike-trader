//! Where a strategy file's built ARTIFACT lives: `<builds>/<folder>/<sha256>.so`, the one writer
//! and the one reader (plan 2b: every strategy is a file).
//!
//! `<sha256>` is the hash of the file's TEXT, so two identical files share one artifact and an edit
//! is a new one. `<folder>` ([`build_folder`]) names everything else an artifact was built under:
//! this crate's `ABI_VERSION`, the toolchain fingerprint, the cdylib template's two files, the
//! profile and the source-tree stamp. A release (a new stamp) therefore starts a new folder and
//! never serves the previous release's artifact. The builder writes the path
//! (`crates/vike-strategy-builder/src/render.rs`'s `build_strategy_file`) and a host that loads an
//! artifact by its sha computes the same one (the sandboxed child, `crates/vike-strategy-sandbox/src/lib.rs`'s
//! `artifact_of`: no hub links this crate); this crate sits below both.
//!
//! ⚠ The suffix is `.so` on EVERY host, not the platform's `DLL_SUFFIX`: the design's own naming.

use std::path::{Path, PathBuf};

use vike_utility::fnv::Fnv1a64;

use crate::{TEMPLATE_CARGO_TOML, TEMPLATE_LIB_RS};

/// The folder an artifact built under `profile` (`"release"` or `"debug"`) from a source tree
/// stamped `source_stamp` lands in: eight lowercase hex digits of FNV-1a 64 (`vike_utility::fnv`, a
/// frozen algorithm, so the name is the same in every process and release that computes it) over
/// the ABI version, the toolchain fingerprint, both template files, the profile and the stamp, each
/// fed with a terminator so no two inputs run together.
///
/// The stamp is the builder's `vike_buildinfo::GIT_SHA`: a build proceeds only against a source
/// tree whose `SOURCE_GIT_SHA` stamp is that commit (or carries none), so the folder names the
/// source commit too.
pub fn build_folder(profile: &str, source_stamp: &str) -> String {
    let mut h = Fnv1a64::new();
    for part in [
        crate::abi::ABI_VERSION.to_string().as_str(),
        crate::fingerprint::FINGERPRINT,
        TEMPLATE_CARGO_TOML,
        TEMPLATE_LIB_RS,
        profile,
        source_stamp,
    ] {
        h.write(part.as_bytes());
        h.write(&[0]);
    }
    format!("{:016x}", h.finish())[..8].to_string()
}

/// The profile THIS binary was compiled under, in [`build_folder`]'s words (`"release"` or
/// `"debug"`), read off the toolchain fingerprint the loader compares: the folder a host that loads
/// an artifact by its sha looks in.
pub fn host_profile() -> &'static str {
    if crate::fingerprint::FINGERPRINT.contains("profile=release") { "release" } else { "debug" }
}

/// The artifact built from text hashing to `sha` (sha256, lowercase hex) in `folder` under
/// `builds`: `<builds>/<folder>/<sha>.so`.
pub fn artifact_path(builds: &Path, folder: &str, sha: &str) -> PathBuf {
    builds.join(folder).join(format!("{sha}.so"))
}

#[path = "artifact_tests.rs"]
#[cfg(test)]
mod artifact_tests;
