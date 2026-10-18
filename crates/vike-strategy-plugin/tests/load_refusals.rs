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

use std::path::{Path, PathBuf};

use vike_strategy_builder::render;
use vike_strategy_plugin::abi::{BrokerRef, CBar, PluginStatus};
use vike_strategy_plugin::loader::{self, LoadError};

use cache::{artifact_dir_name, dylib_file_name, fixture_dir};
use common::RecordingBroker;

#[path = "load_refusals/cache.rs"]
mod cache;
#[path = "common/mod.rs"]
mod common;
#[path = "load_refusals/sources.rs"]
mod sources;

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

    let src = sources::PARAMS_PROBE_SOURCE;

    let profile = render::Profile::of_this_binary();
    // Its OWN output directory and its own source text, so neither the artifact name nor the
    // content-addressed scratch directory can collide with `equivalence.rs`'s build.
    let out_dir =
        PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(artifact_dir_name("params-probe-plugin"));
    let workspace_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..").join("..");
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
    let child_env = render::child_env(&std::env::vars().collect());
    let so = render::build_plugin(
        src,
        "params_probe",
        &out_dir,
        &workspace_root,
        &cargo,
        &child_env,
        profile,
        &render::CargoHomePin::disabled(),
    )
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
    let src = sources::PANICS_ON_THE_SECOND_BAR_SOURCE;

    let profile = render::Profile::of_this_binary();
    let out_dir =
        PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(artifact_dir_name("panic-probe-plugin"));
    let workspace_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..").join("..");
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
    let child_env = render::child_env(&std::env::vars().collect());
    let so = render::build_plugin(
        src,
        "panic_probe",
        &out_dir,
        &workspace_root,
        &cargo,
        &child_env,
        profile,
        &render::CargoHomePin::disabled(),
    )
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
    let vtable = vike_strategy_plugin::host::broker_vtable::<RecordingBroker>();
    let mut broker = RecordingBroker::default();
    let mut cbar = CBar::empty();
    cbar.symbol_ptr = SYMBOL.as_ptr();
    cbar.symbol_len = SYMBOL.len();
    cbar.close = 100.0;

    let dispatch = |b: &mut RecordingBroker| {
        let r = BrokerRef { ctx: (b as *mut RecordingBroker).cast(), vtable };
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
