//! Integration tests over `loader::load`, driven by fixture `.so`s this file builds itself, on
//! first use, cached by a content hash — NOT by `crates/vike-strategy-plugin/build.rs`.
//!
//! ⚠ **Why the build lives here and not in `build.rs`.** `build.rs` runs for EVERY compile of this
//! workspace member — `cargo build --workspace`, every `--workspace` clippy invocation, `just
//! windows-check` — none of which ever runs this test binary. `cargo build`/`cargo check`/`cargo
//! clippy` (even with `--all-targets`, which COMPILES `tests/*.rs` but never RUNS it) do not
//! execute test bodies, so a lazy build triggered from inside a `#[test]`'s own call graph is
//! reached only by an actual `cargo test`. `env!("CARGO_TARGET_TMPDIR")` is a Cargo-set (not
//! build-script-set) directory reserved for exactly this: a persistent-across-runs cache an
//! integration test binary may write into.
//!
//! ⚠ **Round-2 review: `fixture_dir()`'s `OnceLock` only serializes within ONE PROCESS.** `cargo
//! test` runs every test in this file as a thread of one binary, so one `OnceLock` genuinely
//! covers all of them — but `cargo nextest` (what this repo's CI runs, and what `just t` runs
//! locally) gives each test its OWN process, in parallel. Four of the five tests here call
//! [`fixture_path`], so on a cold `CARGO_TARGET_TMPDIR` four SEPARATE processes each run
//! [`build_stale_fixtures`] concurrently. Cargo's own target-dir lock serializes the nested `cargo
//! build`s themselves, but nothing serialized the publish step that copied the built artifact to
//! its STABLE name and wrote its hash marker — one process's `fs::copy` truncating `libgood.so`
//! while a sibling process had it `dlopen`'d was a torn load (SIGBUS on Linux), and it would have
//! hit hardest on exactly the PR that changes a fixture, `ABI_VERSION` or `FINGERPRINT` (the one
//! case where the cache is genuinely cold for everyone at once). The fix: every publish (the `.so`
//! AND the hash marker) now writes to a process-uniquely-named temporary file FIRST and
//! `fs::rename`s it onto the stable name SECOND — `rename` within one directory is atomic on the
//! filesystems this ships on, so a concurrent `dlopen`/read of the stable name always sees either
//! the complete OLD file or the complete NEW one, never a partial write, regardless of how many
//! processes race to publish the same (deterministically-identical) content.

use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use vike_strategy_builder::render;
use vike_strategy_plugin::abi::{self, BrokerRef, CBar, PluginStatus};
use vike_strategy_plugin::fingerprint;
use vike_strategy_plugin::loader::{self, LoadError};

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
fn artifact_dir_name(stem: &str) -> String {
    format!("{stem}-{}", render::artifact_dir_tag())
}

fn dylib_file_name(name: &str) -> String {
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
fn fixture_dir() -> &'static Path {
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

fn fixture_path(name: &str) -> PathBuf {
    fixture_dir().join(dylib_file_name(name))
}

#[test]
fn a_well_formed_plugin_loads_successfully() {
    let plugin = loader::load(&fixture_path("good")).expect("a well-formed fixture must load");
    assert_eq!((plugin.vtable.warmup)(std::ptr::null_mut()), 0);
}

#[test]
fn an_abi_version_mismatch_is_refused() {
    let err = loader::load(&fixture_path("bad_abi_version")).unwrap_err();
    match err {
        LoadError::AbiMismatch { host, plugin } => {
            assert_ne!(host, plugin, "the mismatch must be genuine");
            assert_eq!(plugin, 999, "must report the fixture's actual (wrong) value");
        }
        other => panic!("expected AbiMismatch, got {other:?}"),
    }
}

#[test]
fn a_fingerprint_mismatch_is_refused_naming_both_toolchains() {
    let err = loader::load(&fixture_path("bad_fingerprint")).unwrap_err();
    let (host, plugin) = match &err {
        LoadError::FingerprintMismatch { host, plugin } => (host.clone(), plugin.clone()),
        other => panic!("expected FingerprintMismatch, got {other:?}"),
    };
    assert_ne!(host, plugin, "the mismatch must be genuine");
    assert!(!host.is_empty(), "the host's own fingerprint must be reported, not empty");
    assert_eq!(plugin, "doctored-wrong-fingerprint-for-testing");

    // The refusal must NAME both sides in its message, not merely carry them as unread fields.
    let msg = err.to_string();
    assert!(msg.contains(&host), "message must name the host's fingerprint: {msg}");
    assert!(msg.contains(&plugin), "message must name the plugin's fingerprint: {msg}");
}

/// **The builder's cache check, witnessed on the guards it actually relies on.**
/// `vike-strategy-builder`'s `render::build_plugin` opens every cache hit and asks
/// `loader::verify_handshake` before serving it. That crate's own
/// `a_stale_cached_artifact_is_rebuilt_rather_than_served` can only plant NON-ELF bytes, so it
/// exercises the `DlOpen` arm and says nothing about the two comparisons the residual was
/// actually about — an artifact that opens PERFECTLY WELL and answers the wrong contract.
///
/// ⚠ These fixtures are exactly that artifact, and they exist already: `bad_abi_version` and
/// `bad_fingerprint` are real cdylibs built by this file's own `build_stale_fixtures`, so the
/// case the sibling crate cannot construct is one already sitting on disk here. Without this
/// test, deleting both comparisons from `open_and_handshake` would still leave the builder's
/// cache tests green — they would be resting on `load`'s coverage of the SAME helper, which is a
/// structural argument rather than a witness. This makes it a witness.
#[test]
fn verify_handshake_refuses_exactly_what_load_refuses() {
    // The control: a current artifact must PASS, or every refusal below is vacuous.
    loader::verify_handshake(&fixture_path("good"))
        .expect("a well-formed fixture must pass the handshake the cache check runs");

    match loader::verify_handshake(&fixture_path("bad_abi_version")).unwrap_err() {
        LoadError::AbiMismatch { host, plugin } => {
            assert_ne!(host, plugin, "the mismatch must be genuine");
            assert_eq!(plugin, 999, "must report the fixture's actual (wrong) value");
        }
        other => panic!("expected AbiMismatch, got {other:?}"),
    }

    match loader::verify_handshake(&fixture_path("bad_fingerprint")).unwrap_err() {
        LoadError::FingerprintMismatch { host, plugin } => {
            assert_ne!(host, plugin, "the mismatch must be genuine");
            assert_eq!(plugin, "doctored-wrong-fingerprint-for-testing");
        }
        other => panic!("expected FingerprintMismatch, got {other:?}"),
    }
}

#[test]
fn a_missing_artifact_is_refused_with_an_instruction_to_build() {
    let missing = Path::new("/definitely/not/a/real/plugin/path.so");
    let err = loader::load(missing).unwrap_err();
    match &err {
        LoadError::Missing(p) => assert_eq!(p.as_path(), missing),
        other => panic!("expected Missing, got {other:?}"),
    }
    let msg = err.to_string().to_lowercase();
    assert!(msg.contains("build"), "message must instruct the user to build it: {msg}");
}

/// ⚠ **This proves the LOADER's half only, and the design used to read it as proof of the
/// template's.** Its fixture is HAND-WRITTEN (`tests/fixtures/panicking/src/lib.rs`) and carries
/// its own `catch_unwind`; it never touches `template/lib.rs.in`, so the production panic
/// boundary — the one standing between a user's `panic!` and the backtest server — was covered by
/// nothing. `a_panicking_user_strategy_built_from_the_template_is_reported_not_fatal`, at the
/// bottom of this file, covers that one. Keep both: this one is cheap, needs no builder, and is
/// the only witness that the LOADER hands back a usable vtable for a plugin whose dispatch fails.
#[test]
fn a_panicking_plugin_surfaces_an_error_and_does_not_abort_the_process() {
    let plugin =
        loader::load(&fixture_path("panicking")).expect("ABI-wise well-formed fixture must load");

    let handle = (plugin.vtable.create)(std::ptr::null(), 0);
    // The fixture's own `on_bar` never dereferences either the broker ref or the bar — a null
    // vtable pointer and an empty CBar are harmless placeholders (see the fixture's module doc).
    let broker_ref = BrokerRef { ctx: std::ptr::null_mut(), vtable: std::ptr::null() };
    let bar = CBar::empty();
    let status = (plugin.vtable.on_bar)(handle, broker_ref, &bar as *const CBar);
    (plugin.vtable.destroy)(handle);

    assert_eq!(status, PluginStatus::Panicked);
    // Reaching this assertion at all IS the process-survival proof: a real panic INSIDE the
    // compiled `panicking.so` was caught by that `.so`'s OWN `catch_unwind` before it could unwind
    // across the C-ABI boundary (which Rust turns into a process abort by default) — a test
    // process that had aborted would never have reached the `assert_eq!` above.
}

// ── the TEMPLATE's own create path ───────────────────────────────────────────────────────────

/// A plugin built from the REAL cdylib template must accept a real params document, hand back a
/// non-null handle, and let the value in that document reach the strategy.
///
/// ⚠ **This exists because the defect it guards against was invisible to everything else in the
/// project, including every test in this file.** The template parsed its params with
/// `str::parse::<toml::Value>()`, which under the pinned `toml` 1.1 parses a single VALUE
/// expression rather than a document — so `vike_plugin_create` returned null for every params
/// string including the empty one, every dispatch answered `PluginStatus::BadHandle`, and a
/// plugin-loaded strategy traded NOTHING while its artifact built, loaded and passed both
/// handshakes. The fixtures above could not see it: they are hand-written and ignore their params
/// entirely, so they witness the LOADER and say nothing about the template.
///
/// ⚠ **Deliberately a second witness, not the only one.**
/// `crates/vike-strategy-plugin/tests/equivalence.rs` also probes this, from the artifact it
/// already builds. This one is independent of that test's fate and much cheaper to read when it
/// fails — it names the create path directly instead of reporting that two runs disagreed.
///
/// NOT `#[ignore]`d: `build_plugin` now takes the profile, so it builds under whatever profile
/// this binary itself was compiled with and the fingerprint matches in an ordinary debug run.
/// It is the ONLY `build_plugin` caller in this file, which is what keeps it safe under
/// `nextest`'s process-per-test — see `equivalence.rs`'s note on that race.
#[test]
fn a_template_built_plugin_accepts_a_real_params_document() {
    // Every value differs from the strategy's own fallback, so a params table that silently
    // arrived empty cannot satisfy the assertions below by coincidence.
    const PARAMS: &str = "warmup_bars = 13\n";
    const FALLBACK: usize = 1;
    const DECLARED: usize = 13;

    let src = r#"
use vike_model::{Bar, Broker, Strategy};

pub struct ParamsProbe {
    warmup_bars: usize,
}

impl<B: Broker> Strategy<B> for ParamsProbe {
    fn warmup(&self) -> usize {
        self.warmup_bars
    }
    fn on_bar(&mut self, _broker: &mut B, _bar: &Bar) {}
}

pub fn build<B: vike_model::HftBroker + 'static>(
    params: &toml::Value,
) -> Box<dyn vike_model::Strategy<B> + Send> {
    let warmup_bars = params
        .get("warmup_bars")
        .and_then(toml::Value::as_integer)
        .map(|i| i.max(0) as usize)
        .unwrap_or(1);
    Box::new(ParamsProbe { warmup_bars })
}
"#;

    let profile = render::Profile::of_this_binary();
    // Its OWN output directory and its own source text, so neither the artifact name nor the
    // content-addressed scratch directory can collide with `equivalence.rs`'s build.
    let out_dir =
        PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(artifact_dir_name("params-probe-plugin"));
    let workspace_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..").join("..");
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
    let so = render::build_plugin(src, "params_probe", &out_dir, &workspace_root, &cargo, profile)
        .unwrap_or_else(|e| panic!("the template must build this strategy:\n{e}"));

    let plugin = loader::load(&so)
        .unwrap_or_else(|e| panic!("the artifact the builder just produced must load: {e}"));

    let handle = (plugin.vtable.create)(PARAMS.as_ptr(), PARAMS.len());
    assert!(
        !handle.is_null(),
        "`vike_plugin_create` returned NULL for the well-formed params document {PARAMS:?}. \
         A null handle is the one failure this ABI cannot report through a status code: every \
         dispatch will answer `PluginStatus::BadHandle` and the strategy will trade nothing, \
         while the artifact builds, loads and handshakes cleanly. The known cause is the \
         template parsing params as a TOML VALUE instead of a DOCUMENT — `toml::from_str`, \
         never `str::parse`."
    );

    let warmup = (plugin.vtable.warmup)(handle);
    (plugin.vtable.destroy)(handle);
    assert_ne!(
        warmup, FALLBACK,
        "the strategy answered its own fallback, so the params document reached it empty — \
         a parse that produced an empty table rather than refusing"
    );
    assert_eq!(
        warmup, DECLARED,
        "the value in the params document must reach the strategy unchanged"
    );
}

// ── the TEMPLATE's own panic boundary ────────────────────────────────────────────────────────

/// A broker the plugin's `guest::HostBroker` can really call back into, so the dispatch below is
/// the PRODUCTION shape rather than a null-vtable placeholder.
///
/// Required, not cosmetic: `guest::HostBroker::new` dereferences `BrokerRef::vtable` on entry, so
/// the null-vtable `BrokerRef` the hand-written `panicking` fixture is driven with above would be
/// undefined behaviour against a template-built plugin. The methods mirror `host.rs`'s own
/// `FakeBroker`; `submits` exists so the first dispatch can be PROVEN to have reached user code
/// (see the test).
#[derive(Default)]
struct PanicProbeBroker {
    submits: Vec<(String, i32, f64)>,
}

impl vike_model::Broker for PanicProbeBroker {
    fn submit_market(&mut self, symbol: &str, side: i32, qty: f64) {
        self.submits.push((symbol.to_string(), side, qty));
    }
    fn submit_limit(&mut self, _symbol: &str, _side: i32, _qty: f64, _price: f64) {}
    fn position(&self, _symbol: &str) -> f64 {
        0.0
    }
    fn price(&self, _symbol: &str) -> f64 {
        0.0
    }
    fn equity(&self) -> f64 {
        100_000.0
    }
    fn bars(&self, _symbol: &str) -> &[vike_marketdata::Bar] {
        &[]
    }
    fn index(&self) -> usize {
        0
    }
    fn now(&self) -> i64 {
        0
    }
}

impl vike_model::HftBroker for PanicProbeBroker {
    fn position(&self) -> f64 {
        0.0
    }
    fn submit_limit_tagged(&mut self, _tag: &str, _side: i32, _qty: f64, _price: f64) {}
    fn modify_tagged(&mut self, _tag: &str, _new_qty: Option<f64>, _new_price: Option<f64>) {}
    fn cancel_tagged(&mut self, _tag: &str) {}
}

/// A panic inside a USER STRATEGY's `on_bar`, in a plugin the REAL builder produced from the REAL
/// template, is reported as `PluginStatus::Panicked` and the process keeps running.
///
/// ⚠ **This exists because the design listed this property as proven and nothing proved it.** The
/// only panic test before this one
/// (`a_panicking_plugin_surfaces_an_error_and_does_not_abort_the_process`, above) drives
/// `crates/vike-strategy-plugin/tests/fixtures/panicking/src/lib.rs` — a HAND-WRITTEN
/// `extern "C"` function carrying its own `catch_unwind`, which never touches
/// `template/lib.rs.in`. So it proves that *a* `.so` can catch its own panic and says nothing
/// whatever about the `catch_unwind` in the template — which is the one that actually stands
/// between a user's `panic!` and the backtest server's process. Exactly the shape of the toml
/// defect the equivalence test found: the artifact builds, loads and handshakes, and the
/// behaviour underneath is unproven.
///
/// ⚠ **It discriminates in three directions, because a test that merely observes "not Ok" passes
/// for the wrong reasons.** `PluginStatus::BadHandle` is what a null handle and a null bar both
/// answer, and it is what every dispatch would answer if `create` had failed for the params
/// reason `a_template_built_plugin_accepts_a_real_params_document` guards. So each assertion
/// pins the SPECIFIC status, and:
///
/// 1. the FIRST bar must answer `Ok` AND be observable in the broker, so the dispatch path
///    provably reaches user code rather than short-circuiting before it;
/// 2. the SECOND bar — the one the strategy panics on — must answer exactly `Panicked`, not
///    `BadHandle` and not `Ok`;
/// 3. a THIRD bar must answer `Panicked` again rather than aborting or hanging, so a caught panic
///    leaves the plugin callable instead of poisoning it.
///
/// Reaching any assertion at all is the process-survival proof: an unwind escaping an
/// `extern "C"` frame aborts the process, so a test binary that had lost this would not report a
/// failure — it would report nothing.
///
/// ⚠ **Placed here rather than in `crates/vike-strategy-builder/tests/build_errors.rs`**, whose
/// harness this otherwise reuses (a `workspace_root`, a `CARGO` read, a `Profile`): every
/// cargo-invoking test in that file is `#[ignore]`d as lane-only, and an ignored witness for a
/// production panic boundary is barely better than none. It also has to LOAD what it builds,
/// which needs the host's own profile-matched fingerprint — the thing this file's sibling above
/// already derives from `Profile::of_this_binary()`.
///
/// Its own `out_dir` and its own source text, so neither the artifact name nor the
/// content-addressed scratch directory can collide with the other `build_plugin` callers in this
/// crate's suite under `nextest`'s process-per-test.
#[test]
fn a_panicking_user_strategy_built_from_the_template_is_reported_not_fatal() {
    const PARAMS: &str = "unused = 1\n";
    const SYMBOL: &str = "BTCUSDT";
    const QTY: f64 = 3.5;

    // Panics on the SECOND bar, not the first: the first is what proves this harness reaches user
    // code at all, which is what stops a `Panicked` on the second from passing for another reason.
    let src = r#"
use vike_model::{Bar, Broker, Strategy};

pub struct PanicsOnTheSecondBar {
    seen: usize,
}

impl<B: Broker> Strategy<B> for PanicsOnTheSecondBar {
    fn on_bar(&mut self, broker: &mut B, bar: &Bar) {
        self.seen += 1;
        if self.seen >= 2 {
            panic!("deliberate panic in a user strategy's on_bar");
        }
        let symbol = bar.symbol.clone().unwrap_or_default();
        broker.submit_market(&symbol, 1, 3.5);
    }
}

pub fn build<B: vike_model::HftBroker + 'static>(
    _params: &toml::Value,
) -> Box<dyn vike_model::Strategy<B> + Send> {
    Box::new(PanicsOnTheSecondBar { seen: 0 })
}
"#;

    let profile = render::Profile::of_this_binary();
    let out_dir =
        PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(artifact_dir_name("panic-probe-plugin"));
    let workspace_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..").join("..");
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
    let so = render::build_plugin(src, "panic_probe", &out_dir, &workspace_root, &cargo, profile)
        .unwrap_or_else(|e| panic!("the template must build a panicking strategy:\n{e}"));

    let plugin = loader::load(&so)
        .unwrap_or_else(|e| panic!("the artifact the builder just produced must load: {e}"));

    let handle = (plugin.vtable.create)(PARAMS.as_ptr(), PARAMS.len());
    assert!(
        !handle.is_null(),
        "`vike_plugin_create` returned NULL, so every dispatch below would answer BadHandle and \
         this test would 'pass' for a reason with nothing to do with a panic — see \
         `a_template_built_plugin_accepts_a_real_params_document` for that failure's own witness"
    );

    // A real, host-owned vtable over a real broker: the same pair `host::PluginStrategy::on_bar`
    // hands a plugin in production. `SYMBOL` is `'static`, so the borrowed (ptr, len) the ABI
    // contract requires outlives every call below.
    let vtable = vike_strategy_plugin::host::broker_vtable::<PanicProbeBroker>();
    let mut broker = PanicProbeBroker::default();
    let mut cbar = CBar::empty();
    cbar.symbol_ptr = SYMBOL.as_ptr();
    cbar.symbol_len = SYMBOL.len();
    cbar.close = 100.0;

    let dispatch = |b: &mut PanicProbeBroker| {
        let r = BrokerRef { ctx: (b as *mut PanicProbeBroker).cast(), vtable };
        (plugin.vtable.on_bar)(handle, r, &cbar as *const CBar)
    };

    let first = dispatch(&mut broker);
    assert_eq!(
        first,
        PluginStatus::Ok,
        "the first bar must dispatch cleanly — without that this test cannot tell a caught panic \
         from a dispatch path that never reached the strategy at all"
    );
    assert_eq!(
        broker.submits,
        vec![(SYMBOL.to_string(), 1, QTY)],
        "the first bar must have reached USER CODE and come back out through the host's own \
         BrokerVTable; an empty list means the `Ok` above proves nothing"
    );

    let second = dispatch(&mut broker);
    assert_eq!(
        second,
        PluginStatus::Panicked,
        "a `panic!` inside the user strategy must be caught by the TEMPLATE's own catch_unwind \
         and reported as Panicked. `BadHandle` here would mean the dispatch never reached the \
         strategy; `Ok` would mean the panic never happened and this test proves nothing"
    );

    let third = dispatch(&mut broker);
    assert_eq!(
        third,
        PluginStatus::Panicked,
        "the plugin must still be callable after a caught panic, and must still report it"
    );
    assert_eq!(broker.submits.len(), 1, "the panicking bars must not have submitted anything");

    (plugin.vtable.destroy)(handle);
    // Destroying after a caught panic must not abort either — reaching the end of this test is
    // that proof, the same way reaching any assertion above is the process-survival proof.
}

/// A file that exists but is not a loadable shared object must come back with the DYNAMIC
/// LOADER's own reason, not just its path.
///
/// ⚠ Added with the `dlerror` fix: `plat::open` used to format only the path, so an unresolved
/// symbol, a missing transitive `.so` and a wrong ELF class were one indistinguishable message
/// that told an operator nothing they did not already know. Unix-only, because the
/// `#[cfg(not(unix))]` `plat` stub refuses before any loader is consulted and has no `dlerror` to
/// report; every box this crate runs on is Linux.
#[cfg(unix)]
#[test]
fn a_file_that_is_not_a_shared_object_is_refused_with_the_loaders_own_message() {
    let path = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("not-an-elf-at-all.so");
    std::fs::write(&path, b"this is not an ELF file").expect("plant a non-ELF file");

    let err = loader::load(&path).expect_err("a non-ELF file must not load");
    let msg = match &err {
        LoadError::DlOpen(m) => m.clone(),
        other => panic!("expected DlOpen, got {other:?}"),
    };
    assert!(msg.contains(&path.display().to_string()), "the message must name the path: {msg}");
    assert!(
        !msg.contains("dlerror() reported no message"),
        "the loader offered no reason at all, which is the finding rather than a passing test — \
         `plat::open` reads `dlerror()` inside the same block as the failed `dlopen`, so an empty \
         answer here means that read has stopped working: {msg}"
    );
    // Beyond the path: the whole point is a DETAIL the caller did not already have.
    let prefix = format!("dlopen failed for `{}`: ", path.display());
    assert!(
        msg.starts_with(&prefix) && msg.len() > prefix.len(),
        "the message must carry the loader's own text after the path: {msg}"
    );
}
