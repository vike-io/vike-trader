//! The BODY of a host's build script — impure in effect (it writes OUT_DIR and prints cargo
//! directives), parameters in input: the manifest dir, OUT_DIR and the build-time override all
//! arrive as arguments, so no environment variable is read here (the [`crate::host_build`] module
//! doc says why that matters).
//!
//! It resolves the scan root, prints the rerun watch over the directories the host's scan reads,
//! and writes two registries into OUT_DIR — the REAL user_data's (`user_registry.rs`) and the
//! committed fixture tree's (`fixture_registry.rs`). Any scan error FAILS THE BUILD naming the
//! offending path: an operator who dropped a malformed folder must hear about it at compile time,
//! not discover an absent strategy or study at run time.
//!
//! ## Scan-root resolution (build time, deliberately simpler than the runtime walk)
//!
//! A non-blank override wins outright; otherwise `<workspace-root>/user_data`, where the workspace
//! root is `<manifest dir>/../..` — a fixed hop, NOT the runtime marker walk in
//! `crates/vike-model/src/paths/state_path.rs`. A build script runs in a source checkout by
//! construction, so the checkout layout is known and none of the deployment ambiguities that walk
//! arbitrates can occur. [`user_data_root`] is the rule.
//!
//! ## Re-run triggers — and the ones that must NOT fire
//!
//! The script re-runs when a directory the scan READS changes ([`Host::scanned_dirs`], through
//! [`crate::host_build::watch`]) and when the fixture tree changes; the host's own `build.rs`
//! prints the override's `rerun-if-env-changed`. The whole user_data is never watched: cargo
//! watches a directory recursively, so a write to `runs/` or `logs/` would re-run the script and
//! recompile every crate above it.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use super::watch;

/// One registry's worth of a host's scan: the rendered source plus every objection and every
/// non-fatal observation, each naming its path.
#[derive(Debug, Default)]
pub struct Built {
    /// The generated registry source, written to OUT_DIR when `errors` is empty.
    pub rendered: String,
    /// Malformed folders: any one fails the build.
    pub errors: Vec<String>,
    /// Ambiguous rather than broken state, printed as `cargo:warning=` lines.
    pub warnings: Vec<String>,
}

/// What a host hands the driver: its name, its folder noun and its two scan functions.
pub struct Host<'a> {
    /// The package name, prefixed to the failure and to every warning (`vike-user-strategies`).
    pub label: &'a str,
    /// The folder kind in the failure (`strategy`, `study`).
    pub noun: &'a str,
    /// Every directory the scan reads under a user_data root — exactly what is watched.
    pub scanned_dirs: fn(&Path) -> Vec<PathBuf>,
    /// Scan ONE user_data root and render its registry. Called with the root as spelled (the
    /// override or the default hop), so the generated `#[path]`s keep that spelling.
    pub build: fn(&Path) -> Built,
}

/// The build-time scan root: `override_dir` when it is set and not blank, otherwise
/// `<manifest_dir>/../../user_data`.
pub fn user_data_root(manifest_dir: &Path, override_dir: Option<&str>) -> PathBuf {
    match override_dir {
        Some(v) if !v.trim().is_empty() => PathBuf::from(v),
        _ => manifest_dir.join("..").join("..").join("user_data"),
    }
}

/// The whole build script after its env reads: [`run_with`], timed by the script's own build
/// instant, printing every directive to stdout for cargo.
pub fn run(host: &Host<'_>, manifest_dir: &Path, out_dir: &Path, override_dir: Option<&str>) {
    run_with(host, manifest_dir, out_dir, override_dir, script_built_at(), &mut |line: &str| {
        println!("{line}");
    });
}

/// [`run`] with the watch's `not_after` and the directive sink as parameters, so a test holds the
/// exact lines. Each line is handed to `emit` the moment it is decided, before the next step can
/// fail. Panics on a malformed folder and on an OUT_DIR write failure.
pub fn run_with(
    host: &Host<'_>,
    manifest_dir: &Path,
    out_dir: &Path,
    override_dir: Option<&str>,
    not_after: SystemTime,
    emit: &mut dyn FnMut(&str),
) {
    let user_data = user_data_root(manifest_dir, override_dir);
    // ONLY the directories the scan reads — never the whole user_data — each as an ABSOLUTE path
    // (a relative override resolves against the package, which is the script's working
    // directory): the watch may plant a symlink in OUT_DIR, and a relative link target would
    // resolve against OUT_DIR instead. The scan keeps the spelling it always had, so the generated
    // registry is unchanged.
    let root = manifest_dir.join(&user_data);
    for line in watch::rerun_directives(&root, &(host.scanned_dirs)(&root), out_dir, not_after) {
        emit(&line);
    }
    write_registry(host, &user_data, out_dir, "user_registry.rs", emit);

    // The committed fixture tree: scanned UNCONDITIONALLY so the host's own tests prove the full
    // scan->generate->compile pipeline on every CI run, where the real registry above is empty
    // (CI checkouts carry no user_data).
    let fixtures = manifest_dir.join("tests").join("fixture_user_data");
    emit(&format!("cargo:rerun-if-changed={}", fixtures.display()));
    write_registry(host, &fixtures, out_dir, "fixture_registry.rs", emit);
}

/// When this script was BUILT — necessarily before cargo stamped this run's start, which is what
/// makes it a safe `not_after` for the watch ([`crate::host_build::watch`]'s module doc says why
/// one is needed at all).
fn script_built_at() -> SystemTime {
    std::env::current_exe()
        .and_then(|exe| exe.metadata())
        .and_then(|m| m.modified())
        .unwrap_or(SystemTime::UNIX_EPOCH)
}

fn write_registry(
    host: &Host<'_>,
    root: &Path,
    out_dir: &Path,
    file: &str,
    emit: &mut dyn FnMut(&str),
) {
    let built = (host.build)(root);
    if !built.errors.is_empty() {
        panic!(
            "{}: refusing to build with malformed {} folders:\n  {}",
            host.label,
            host.noun,
            built.errors.join("\n  ")
        );
    }
    for w in &built.warnings {
        emit(&format!("cargo:warning={}: {w}", host.label));
    }
    let path = out_dir.join(file);
    std::fs::write(&path, built.rendered)
        .unwrap_or_else(|e| panic!("write {}: {e}", path.display()));
}
