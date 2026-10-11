//! The fixture `.so` cache: built on first use, keyed by content, published atomically.

use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use vike_strategy_builder::render;
use vike_strategy_plugin::abi;
use vike_strategy_plugin::fingerprint;

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

/// A `build_plugin` output directory tagged with everything the LOADER will judge the artifact on
/// and the artifact's own NAME does not carry.
///
/// ⚠ **This is a real defect, found by a real run, and the tag is the test-side half of it.**
/// `build_plugin`'s cache is `Path::exists` on `<name>-<sha>.so`, where the sha covers the SOURCE
/// and nothing else — so an artifact built before an `ABI_VERSION` bump, or under another
/// toolchain, profile or target, is returned unrebuilt, and then `loader::load` refuses it on
/// exactly the guard that changed. That is what happened when `ABI_VERSION` went 2 -> 3: a warm
/// `CARGO_TARGET_TMPDIR` handed both `build_plugin` tests an ABI-2 artifact and they failed with
/// `AbiMismatch` on code that was correct. A CLEAN checkout never sees it, which is exactly what
/// makes it worth a named helper rather than a one-line `rm`.
///
/// The tag is `render::artifact_dir_tag()` — the ABI version and a hash of the whole toolchain
/// fingerprint, not the profile alone (this helper's first cut, covering one of the three causes
/// its own paragraph above names).
///
/// ⚠ **It is a LIBRARY function rather than a local one, and that is the second correction.** The
/// same rule was hand-written here and again in `tests/equivalence.rs`, and a THIRD site —
/// `crates/vike-studio-core/tests/plugin_run.rs`, in another crate entirely — never got it and
/// went red in CI on a warm runner cache after the `ABI_VERSION` bump. Three copies of one rule
/// is what this repo gates against everywhere else, so the rule moved to the crate all three
/// already depend on and they all call it.
///
/// ⚠ **The PRODUCTION half IS fixed now, on the branch that closed this residual — this doc
/// carried two earlier drafts of that story and both are superseded.** The first draft said "the
/// deployed builder has the same cache"; the correction said there was no deployed builder at all
/// (`deploy/sbin/vike-trader-ci-deploy`'s `DEPLOY_UNITS` named neither this daemon nor a real
/// `unit_shape` probe for it) and called the exposure LATENT, becoming live only once the builder
/// unit was installed. That trigger was met (the builder joined `DEPLOY_UNITS`), and rather than
/// stay latent-until-installed, `render::build_plugin`'s cache check now opens a cache hit and
/// asks `vike_strategy_plugin::loader::verify_handshake` before serving it, rebuilding on a
/// mismatch instead of handing back something `loader::load` would refuse. `<name>-<sha>.so` is
/// still the design's WIRE contract (Studio sends that sha back with its Run) and the name still
/// cannot grow a tag — the fix is in what `build_plugin` DOES on a hit, not in the name. This
/// TEST-SIDE tag remains necessary regardless: it defends a DIFFERENT, narrower residual
/// `verify_handshake` cannot see by construction (a template-content edit that changes neither the
/// ABI version nor the fingerprint) — `render::artifact_dir_tag`'s own doc carries that argument.
pub(super) fn artifact_dir_name(stem: &str) -> String {
    format!("{stem}-{}", render::artifact_dir_tag())
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
        let manifest = path.join("Cargo.toml");
        if !manifest.is_file() {
            continue;
        }

        let content_hash = hash_fixture_sources(&path);
        let so_path = cache_dir.join(dylib_file_name(&name));
        let hash_marker = cache_dir.join(format!("{name}.hash"));
        let cached_hash = std::fs::read_to_string(&hash_marker).ok();
        if so_path.is_file() && cached_hash.as_deref() == Some(content_hash.as_str()) {
            continue; // unchanged since the last build — an unchanged source makes this instant.
        }

        let fixture_target = cache_dir.join(format!("fixture-target-{name}"));
        let status = std::process::Command::new(&cargo)
            .arg("build")
            .arg("--quiet")
            .arg("--manifest-path")
            .arg(&manifest)
            .arg("--target-dir")
            .arg(&fixture_target)
            .env("VIKE_TEST_ABI_VERSION", &abi_version)
            .env("VIKE_TEST_FINGERPRINT", fingerprint::FINGERPRINT)
            .status()
            .unwrap_or_else(|e| panic!("failed to spawn cargo to build fixture `{name}`: {e}"));
        assert!(
            status.success(),
            "fixture `{name}` failed to build — see cargo's own output above"
        );

        let built_dir = fixture_target.join("debug");
        let built = find_cdylib(&built_dir, &name).unwrap_or_else(|| {
            panic!("fixture `{name}` built successfully but produced no cdylib in {built_dir:?}")
        });
        // Both publishes are atomic (temp file + rename) — see this module's header for why a
        // plain `fs::copy`/`fs::write` onto the stable name raced concurrent `nextest` processes.
        publish_atomically(&built, &so_path, &format!("fixture `{name}`"));
        publish_text_atomically(&content_hash, &hash_marker, &format!("hash marker for `{name}`"));
    }
    cache_dir
}

/// A content hash over every fixture crate's own files (`Cargo.toml`, `build.rs` if present, and
/// every `src/*.rs`) — deliberately NOT a mtime check, which a fresh `git checkout` or a worktree
/// copy can hand back in either order and which a content hash cannot be fooled by.
fn hash_fixture_sources(fixture_dir: &Path) -> String {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    // The two host-crate values a fixture's own tiny build.rs bakes in — a change to either must
    // invalidate every cached fixture that consumes them, even if the fixture's own files did not
    // change.
    //
    // ⚠ **This layer is necessary and was NOT sufficient, and the gap was live.** Invalidating
    // here correctly triggers a rebuild — but cargo's OWN freshness for the fixture's build
    // script does not track the environment that script reads, so the nested `cargo build` below
    // would decide the script was fresh, keep its previous `cargo:rustc-env` values, and hand
    // back a plugin still carrying the PREVIOUS run's fingerprint. Two tests in this file failed
    // that way the first time this crate's suite ran under two profiles against one cache. The
    // fixtures' build scripts now emit `rerun-if-env-changed` for both values; see
    // `tests/fixtures/good/build.rs`'s header. Both layers are required: this one decides
    // whether to invoke cargo at all, that one decides whether cargo believes anything changed.
    abi::ABI_VERSION.hash(&mut hasher);
    fingerprint::FINGERPRINT.hash(&mut hasher);
    let mut files: Vec<PathBuf> = Vec::new();
    for name in ["Cargo.toml", "build.rs"] {
        let p = fixture_dir.join(name);
        if p.is_file() {
            files.push(p);
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
