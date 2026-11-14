//! The fixture `.so` cache: built on first use, keyed by content, published atomically.

use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use vike_rust_strategy::abi;
use vike_rust_strategy::fingerprint;

/// A process-uniquely-named temp path beside `dest`, in the SAME directory (so the `rename` below
/// is guaranteed same-filesystem, hence atomic) — `<dest's file name>.tmp-<pid>`, never colliding
/// across concurrent `nextest` processes since each has its own pid. A leftover temp file from a
/// process that crashed before its `rename` is harmless: the next run never reads it by name, and
/// a stable `<name>.tmp-<pid>` cannot collide with a FUTURE process's own distinct pid.
fn temp_sibling(dest: &Path) -> PathBuf {
    let mut tmp_name = dest.file_name().expect("dest must have a file name").to_os_string();
    tmp_name.push(format!(".tmp-{}", std::process::id()));
    dest.with_file_name(tmp_name)
}

/// Publish `built` to `dest` by copying to a process-uniquely-named temp file beside it and
/// `rename`-ing into place, so a concurrent reader of `dest` (another `nextest` process's
/// `dlopen`) never observes a partially-written file — see this module's header.
fn publish_atomically(built: &Path, dest: &Path, label: &str) {
    let tmp = temp_sibling(dest);
    std::fs::copy(built, &tmp)
        .unwrap_or_else(|e| panic!("failed to stage {label} at {tmp:?}: {e}"));
    std::fs::rename(&tmp, dest).unwrap_or_else(|e| {
        let _ = std::fs::remove_file(&tmp); // best-effort; a leaked per-pid temp file is harmless
        panic!("failed to publish {label} from {tmp:?} to {dest:?}: {e}")
    });
}

/// Publish `content` to `dest` the same atomic way as [`publish_atomically`], for the small text
/// hash marker rather than a copied file.
fn publish_text_atomically(content: &str, dest: &Path, label: &str) {
    let tmp = temp_sibling(dest);
    std::fs::write(&tmp, content)
        .unwrap_or_else(|e| panic!("failed to stage {label} at {tmp:?}: {e}"));
    std::fs::rename(&tmp, dest).unwrap_or_else(|e| {
        let _ = std::fs::remove_file(&tmp);
        panic!("failed to publish {label} from {tmp:?} to {dest:?}: {e}")
    });
}

pub(super) fn dylib_file_name(name: &str) -> String {
    if cfg!(target_os = "macos") {
        format!("lib{name}.dylib")
    } else if cfg!(windows) {
        format!("{name}.dll")
    } else {
        format!("lib{name}.so")
    }
}

/// The `good` fixture built at `ABI_VERSION` 8 instead of the host's: this host's toolchain and
/// the previous version — an artifact a user built before `create` took the mount facts. Its
/// exports are this host's, which a real v8 artifact's are not; the version check refuses it before
/// any dispatch symbol is bound, so the difference is never read.
pub(super) const GOOD_AT_ABI_8: &str = "good_at_abi_8";

/// The directory holding every fixture's built `.so`, building (or rebuilding, on a content
/// change) whichever ones are stale. Resolved once per test-binary run via `OnceLock` so N tests
/// share one build pass rather than racing N of them.
pub(super) fn fixture_dir() -> &'static Path {
    static DIR: OnceLock<PathBuf> = OnceLock::new();
    DIR.get_or_init(build_stale_fixtures).as_path()
}

fn build_stale_fixtures() -> PathBuf {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let fixtures_dir = manifest_dir.join("tests").join("fixtures");
    let cache_dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR"));
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
    let abi_version = abi::ABI_VERSION.to_string();

    let entries = std::fs::read_dir(&fixtures_dir)
        .unwrap_or_else(|e| panic!("cannot read {fixtures_dir:?}: {e}"));
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        if !path.join("Cargo.toml").is_file() {
            continue;
        }
        build_if_stale(&cargo, &cache_dir, &path, &name, &name, &abi_version);
    }
    build_if_stale(&cargo, &cache_dir, &fixtures_dir.join("good"), "good", GOOD_AT_ABI_8, "8");
    cache_dir
}

/// Build the fixture crate `crate_name` at `path`, baking `abi_version` in, and publish its cdylib
/// as `published` — unless the published copy is already that content.
fn build_if_stale(
    cargo: &str,
    cache_dir: &Path,
    path: &Path,
    crate_name: &str,
    published: &str,
    abi_version: &str,
) {
    let content_hash = hash_fixture_sources(path, abi_version);
    let so_path = cache_dir.join(dylib_file_name(published));
    let hash_marker = cache_dir.join(format!("{published}.hash"));
    let cached_hash = std::fs::read_to_string(&hash_marker).ok();
    if so_path.is_file() && cached_hash.as_deref() == Some(content_hash.as_str()) {
        return; // unchanged since the last build — an unchanged source makes this instant.
    }

    let fixture_target = cache_dir.join(format!("fixture-target-{published}"));
    let status = std::process::Command::new(cargo)
        .arg("build")
        .arg("--quiet")
        .arg("--manifest-path")
        .arg(path.join("Cargo.toml"))
        .arg("--target-dir")
        .arg(&fixture_target)
        .env("VIKE_TEST_ABI_VERSION", abi_version)
        .env("VIKE_TEST_FINGERPRINT", fingerprint::FINGERPRINT)
        .status()
        .unwrap_or_else(|e| panic!("failed to spawn cargo to build fixture `{published}`: {e}"));
    assert!(
        status.success(),
        "fixture `{published}` failed to build — see cargo's own output above"
    );

    let built_dir = fixture_target.join("debug");
    let built = find_cdylib(&built_dir, crate_name).unwrap_or_else(|| {
        panic!("fixture `{published}` built successfully but produced no cdylib in {built_dir:?}")
    });
    // Both publishes are atomic (temp file + rename) — see this module's header for why a
    // plain `fs::copy`/`fs::write` onto the stable name raced concurrent `nextest` processes.
    publish_atomically(&built, &so_path, &format!("fixture `{published}`"));
    publish_text_atomically(&content_hash, &hash_marker, &format!("hash marker for `{published}`"));
}

/// A content hash over every fixture crate's own files (`Cargo.toml`, `build.rs` if present, and
/// every `src/*.rs`) — deliberately NOT a mtime check, which a fresh `git checkout` or a worktree
/// copy can hand back in either order and which a content hash cannot be fooled by.
fn hash_fixture_sources(fixture_dir: &Path, abi_version: &str) -> String {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    // The two host-crate values a fixture's own tiny build.rs bakes in — a change to either must
    // invalidate every cached fixture that consumes them, even if the fixture's own files did not
    // change.
    //
    // ⚠ **This layer is necessary and was NOT sufficient, and the gap was live.** Invalidating
    // here correctly triggers a rebuild — but cargo's OWN freshness for the fixture's build
    // script does not track the environment that script reads, so the nested `cargo build` below
    // would decide the script was fresh, keep its previous `cargo:rustc-env` values, and hand
    // back a strategy file still carrying the PREVIOUS run's fingerprint. Two tests in this file failed
    // that way the first time this crate's suite ran under two profiles against one cache. The
    // fixtures' build scripts now emit `rerun-if-env-changed` for both values; see
    // `tests/fixtures/good/build.rs`'s header. Both layers are required: this one decides
    // whether to invoke cargo at all, that one decides whether cargo believes anything changed.
    abi_version.hash(&mut hasher);
    fingerprint::FINGERPRINT.hash(&mut hasher);
    let mut files: Vec<PathBuf> = Vec::new();
    for name in ["Cargo.toml", "build.rs"] {
        let p = fixture_dir.join(name);
        if p.is_file() {
            files.push(p);
        }
    }
    // A fixture that runs ANOTHER fixture's build script (`panicking`'s `build = "../good/build.rs"`)
    // must be rebuilt when that script changes, so the script its manifest names is hashed too.
    let manifest = std::fs::read_to_string(fixture_dir.join("Cargo.toml")).unwrap_or_default();
    for line in manifest.lines() {
        if let Some(rest) = line.trim().strip_prefix("build = \"")
            && let Some(rel) = rest.strip_suffix('"')
        {
            files.push(fixture_dir.join(rel));
        }
    }
    if let Ok(entries) = std::fs::read_dir(fixture_dir.join("src")) {
        for e in entries.flatten() {
            if e.path().extension().is_some_and(|e| e == "rs") {
                files.push(e.path());
            }
        }
    }
    files.sort();
    for f in files {
        if let Ok(bytes) = std::fs::read(&f) {
            bytes.hash(&mut hasher);
        }
    }
    format!("{:x}", hasher.finish())
}

fn find_cdylib(dir: &Path, crate_name: &str) -> Option<PathBuf> {
    let want = dylib_file_name(crate_name);
    std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .find(|p| p.file_name().is_some_and(|f| f.to_string_lossy() == want))
}
