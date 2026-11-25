//! The fixture `.so` cache: built on first use, keyed by content, published atomically.

use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use vike_rust_strategy::abi;
use vike_rust_strategy::fingerprint;
use vike_rust_strategy::identity;
use vike_rust_strategy::sdk::BOUNDARY_HASH;

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

/// The `good` fixture built at `ABI_VERSION` 14 instead of the host's: this host's toolchain and
/// the previous version — an artifact a user built before its compatibility record named the host it
/// may load in. Its record states 14; the loader refuses it from the file, before `dlopen`.
pub(super) const GOOD_AT_ABI_14: &str = "good_at_abi_14";

/// The `good` fixture built at this host's version and toolchain but ANOTHER layout (`abi::layout` with
/// its lowest bit flipped): a file built against another `Bar` or `TradeClosedRow`, which would misread
/// the host's rows; the layout check refuses it.
pub(super) const GOOD_AT_OTHER_LAYOUT: &str = "good_at_other_layout";

/// The `good` fixture with every value right but its record naming ANOTHER strategy boundary
/// (`sdk::BOUNDARY_HASH`): C-ABI glue that is not this host's; the record check refuses it.
pub(super) const GOOD_AT_OTHER_BOUNDARY: &str = "good_at_other_boundary";

/// The `good` fixture with NO compatibility record at all (an artifact of `ABI_VERSION` 14 or older, or
/// one vike's builder never made): refused unread, its exports never reached.
pub(super) const GOOD_WITHOUT_RECORD: &str = "good_without_record";

/// The fingerprint the `bad_fingerprint` fixture exports, and its record states.
pub(super) const DOCTORED_FINGERPRINT: &str = "doctored-wrong-fingerprint-for-testing";

/// The record a fixture built at `abi_version` and `layout` carries, with `fingerprint` and `boundary`.
fn record_of(abi_version: u32, layout: u64, fingerprint: &str, boundary: &str) -> Vec<u8> {
    identity::record(abi_version, layout, fingerprint, boundary).to_vec()
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
    let layout = abi::layout();
    let (fingerprint, boundary) = (fingerprint::FINGERPRINT, BOUNDARY_HASH);
    let ours = record_of(abi::ABI_VERSION, layout, fingerprint, boundary);

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
        // Each fixture's record states what its exports state.
        let record = match name.as_str() {
            "bad_abi_version" => record_of(999, layout, fingerprint, boundary),
            "bad_fingerprint" => {
                record_of(abi::ABI_VERSION, layout, DOCTORED_FINGERPRINT, boundary)
            }
            _ => ours.clone(),
        };
        build_if_stale(&cargo, &cache_dir, &path, &name, &name, &abi_version, layout, &record);
    }
    let good = fixtures_dir.join("good");
    let variants = [
        (GOOD_AT_ABI_14, "14", layout, record_of(14, layout, fingerprint, boundary)),
        (
            GOOD_AT_OTHER_LAYOUT,
            abi_version.as_str(),
            layout ^ 1,
            record_of(abi::ABI_VERSION, layout ^ 1, fingerprint, boundary),
        ),
        (
            GOOD_AT_OTHER_BOUNDARY,
            abi_version.as_str(),
            layout,
            record_of(abi::ABI_VERSION, layout, fingerprint, "0000000000000000"),
        ),
        (GOOD_WITHOUT_RECORD, abi_version.as_str(), layout, Vec::new()),
    ];
    for (published, abi_version, layout, record) in &variants {
        build_if_stale(&cargo, &cache_dir, &good, "good", published, abi_version, *layout, record);
    }
    cache_dir
}

/// Build the fixture crate `crate_name` at `path`, baking `abi_version`, `layout` and the compatibility
/// `record` in (none when it is empty), and publish its cdylib as `published` — unless the published copy
/// is already that content.
fn build_if_stale(
    cargo: &str,
    cache_dir: &Path,
    path: &Path,
    crate_name: &str,
    published: &str,
    abi_version: &str,
    layout: u64,
    record: &[u8],
) {
    let content_hash = hash_fixture_sources(path, abi_version, layout, record);
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
        .env("VIKE_TEST_LAYOUT", layout.to_string())
        .env("VIKE_TEST_IDENTITY", record.iter().map(|b| format!("{b:02x}")).collect::<String>())
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
fn hash_fixture_sources(
    fixture_dir: &Path,
    abi_version: &str,
    layout: u64,
    record: &[u8],
) -> String {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    // The host-crate values a fixture's own tiny build.rs bakes in — a change to any must
    // invalidate every cached fixture that consumes them, even if the fixture's own files did not
    // change.
    //
    // ⚠ **This layer is necessary and was NOT sufficient, and the gap was live.** Invalidating
    // here correctly triggers a rebuild — but cargo's OWN freshness for the fixture's build
    // script does not track the environment that script reads, so the nested `cargo build` below
    // would decide the script was fresh, keep its previous `cargo:rustc-env` values, and hand
    // back a strategy file still carrying the PREVIOUS run's fingerprint. Two tests in this file failed
    // that way the first time this crate's suite ran under two profiles against one cache. The
    // fixtures' build scripts now emit `rerun-if-env-changed` for every value; see
    // `tests/fixtures/good/build.rs`'s header. Both layers are required: this one decides
    // whether to invoke cargo at all, that one decides whether cargo believes anything changed.
    abi_version.hash(&mut hasher);
    fingerprint::FINGERPRINT.hash(&mut hasher);
    layout.hash(&mut hasher);
    record.hash(&mut hasher);
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
