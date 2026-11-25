//! **Every adapter of this boundary builds its SDK at the SDK's REAL tag, and the artifact loads here**
//! (decision 0296's verdict 8, "an ABI change (to check the adapters)", the owner's yes of 2026-11-20).
//!
//! For each adapter this binary carries (`vike_rust_strategy::sdk::built_in_adapters`, compiled in from this
//! checkout's `crates/vike-rust-strategy/adapters/sdk-N/`, as a deployed vike carries its own): the tag
//! `sdk-N` is read out of git (fetched from `origin` when the checkout lacks it), its strategy closure packed
//! into a snapshot as `scripts/package_strategy_source.sh` packs one (the root manifest and lock, and the
//! crates the strategy template reaches, at that tag) with its marker, and the adapter's marker held to that
//! tree's boundary as this vike hashes it (`vike_rust_strategy::sdk::usable`'s comparison). Beside it, a
//! snapshot that holds THIS checkout's boundary, the glue's source. Then one fixture file pinned to
//! SDK N is resolved as the daemon resolves it (`vike_strategy_builder::render::InstalledSdks`), built through
//! the adapter (`vike_strategy_builder::render`'s `Adapted`), and loaded by this host's loader: its
//! compatibility record must state this boundary. A missing tag, an adapter recording another boundary, a
//! refusal, a failed build or a refused load is red, in words.
//!
//! **When it runs.** It is a test of this crate, the strategy lab, which a pull request plans only when a
//! changed file is strategy-visible: the lab, the strategy boundary or an adapter
//! (`xtask/src/ci/tables/roster.rs`'s `STRATEGY_SURFACE_TREES`, `xtask/src/ci/selection.rs`'s
//! `strategy_lab_touched`); and on every `push` run.
//!
//! **What it costs.** One small fixture per adapter. The snapshots live under this crate's
//! `CARGO_TARGET_TMPDIR` and the builds under the builder's scratch root, both at FIXED paths and written only
//! where their bytes change, so a runner that kept them builds warm (an SDK's closure compiles once). A runner
//! keeps them between jobs, so a snapshot folder another commit left is emptied and every marker written
//! afresh; each SDK's locked crates are fetched before its offline build, as the daemon fetches them; and the
//! artifacts are deleted once loaded (their work directory stays).

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use vike_rust_strategy::sdk::{self, SdkAdapter, SdkMarker, TemplateKind};
use vike_strategy_builder::render::{self, InstalledSdks, Profile};

/// What of `vike-rust-strategy` a snapshot holding this boundary needs: the glue the builder copies, and its
/// manifest, which the glue's is read off.
const HOST_PARTS: [&str; 7] =
    ["build.rs", "build", "src", "template", "tests/boundary", "adapters", "Cargo.toml"];

fn checkout() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
}

/// `git` at this checkout, its stdout; a failure is `Err` with git's words.
fn git(args: &[&str]) -> Result<String, String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(checkout())
        .args(args)
        .output()
        .map_err(|e| format!("git {args:?} could not run: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        Err(format!("git {args:?}: {}", String::from_utf8_lossy(&out.stderr).trim()))
    }
}

/// The commit tagged `tag`, fetched from `origin` when this checkout lacks it.
fn tagged(tag: &str) -> String {
    let commit = format!("refs/tags/{tag}^{{commit}}");
    if let Ok(sha) = git(&["rev-parse", "--verify", "--quiet", &commit]) {
        return sha.trim().to_string();
    }
    let refspec = format!("refs/tags/{tag}:refs/tags/{tag}");
    let fetched = git(&["fetch", "--quiet", "origin", &refspec]);
    git(&["rev-parse", "--verify", "--quiet", &commit]).map(|sha| sha.trim().to_string()).unwrap_or_else(
        |_| {
            panic!(
                "this checkout holds an adapter for {tag} (crates/vike-rust-strategy/adapters/{tag}/), and the \
                 tag {tag} is neither here nor fetchable ({}): fetch the tags, or retire the adapter",
                fetched.err().unwrap_or_default()
            )
        },
    )
}

/// Extract `paths` of `tag`'s tree into `into` (emptied first).
fn extract(tag: &str, paths: &[&str], into: &Path) {
    let _ = fs::remove_dir_all(into);
    fs::create_dir_all(into).expect("the extraction folder");
    let tar = into.with_extension("tar");
    let mut args = vec!["archive".to_string(), "--format=tar".to_string()];
    args.push(format!("--output={}", tar.display()));
    args.push(format!("refs/tags/{tag}"));
    args.push("--".to_string());
    args.extend(paths.iter().map(|p| (*p).to_string()));
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    git(&args).unwrap_or_else(|e| panic!("packing {tag}'s tree: {e}"));
    let untar = Command::new("tar").arg("-xf").arg(&tar).arg("-C").arg(into).status();
    assert!(untar.is_ok_and(|s| s.success()), "unpacking {tag}'s tree from {}", tar.display());
    let _ = fs::remove_file(&tar);
}

/// Copy the file or folder `from` to `to`, writing a file only where its bytes differ (cargo's mtimes stay).
fn mirror(from: &Path, to: &Path) {
    if from.is_dir() {
        for entry in fs::read_dir(from).expect("a folder to copy").flatten() {
            mirror(&entry.path(), &to.join(entry.file_name()));
        }
        return;
    }
    let bytes = fs::read(from).expect("a file to copy");
    if fs::read(to).is_ok_and(|held| held == bytes) {
        return;
    }
    fs::create_dir_all(to.parent().expect("a parent")).expect("the folder");
    fs::write(to, bytes).expect("the copy");
}

/// Install SDK `number` from its tag under `sdk_root`, as the packer packs it, and answer its marker.
fn install_tagged(sdk_root: &Path, number: u32, work: &Path) -> SdkMarker {
    let tag = format!("sdk-{number}");
    let commit = tagged(&tag);
    // The manifests first, to walk the tag's own closure; then that closure whole.
    let manifests = work.join(format!("{tag}-manifests"));
    extract(
        &tag,
        &["Cargo.toml", ":(glob)crates/**/Cargo.toml", "crates/vike-rust-strategy/template"],
        &manifests,
    );
    // Both strategy kinds' crates, as the packer packs them: the fixture names no network and compiles the plain
    // kind's alone, and the prefetch below fetches each kind an adapted SDK serves (the ONNX one's too).
    let crates = sdk::closure(&manifests, TemplateKind::StrategyOnnx);
    let tree = work.join(&tag);
    let mut paths = vec!["Cargo.toml", "Cargo.lock"];
    paths.extend(crates.iter().map(String::as_str));
    extract(&tag, &paths, &tree);
    let snapshot = sdk::snapshot_dir(sdk_root, number);
    // The folder survives between runs (a runner keeps `target/`): one another commit left, an earlier
    // host snapshot of the same number say, or one with no marker, is emptied, never written over.
    if SdkMarker::read(&snapshot).map_or(snapshot.exists(), |held| held.commit != commit) {
        fs::remove_dir_all(&snapshot).expect("a stale snapshot folder");
    }
    mirror(&tree, &snapshot);
    // Written afresh every run: the marker states this binary's interface version too, which a bump moves.
    let _ = fs::remove_file(snapshot.join(sdk::SDK_MARKER_FILE));
    let marker =
        SdkMarker::of_snapshot(&snapshot, number, &commit, vike_rust_strategy::abi::ABI_VERSION);
    marker.write(&snapshot).unwrap_or_else(|e| panic!("{tag}'s marker: {e}"));
    let _ = fs::remove_dir_all(&manifests);
    let _ = fs::remove_dir_all(&tree);
    marker
}

/// Install SDK `number` as the one holding THIS checkout's boundary (the glue's source).
fn install_host(sdk_root: &Path, number: u32) {
    let dir = sdk::snapshot_dir(sdk_root, number);
    let _ = fs::remove_dir_all(&dir);
    let crate_dir = Path::new(sdk::RUST_STRATEGY_CRATE_DIR);
    for part in HOST_PARTS {
        mirror(&checkout().join(crate_dir).join(part), &dir.join(crate_dir).join(part));
    }
    // The root manifest: the glue reads there the versions its crate's `workspace = true` lines pin.
    mirror(&checkout().join("Cargo.toml"), &dir.join("Cargo.toml"));
    let marker =
        SdkMarker::of_snapshot(&dir, number, "this checkout", vike_rust_strategy::abi::ABI_VERSION);
    assert_eq!(marker.boundary_hash, sdk::BOUNDARY_HASH, "the host's snapshot holds this boundary");
    marker.write(&dir).expect("the host's marker");
}

/// The fixture: a strategy on the API every SDK has, pinned to SDK `number`.
fn fixture(number: u32) -> String {
    format!(
        "//! The adapter check's fixture (`crates/vike-strategy-lab/tests/sdk_adapters.rs`).\n\
         //! ```toml\n//! [strategy]\n//! sdk = {number}\n//! ```\n\
         use vike_marketdata::Bar;\nuse vike_model::{{OrderBroker, Strategy}};\n\n\
         pub struct Hold;\n\n\
         impl<B: OrderBroker> Strategy<B> for Hold {{\n    fn on_bar(&mut self, _broker: &mut B, _bar: &Bar) {{}}\n}}\n\n\
         pub fn build<B: OrderBroker + 'static>(\n    _params: &vike_model::params::Params,\n\
         ) -> Box<dyn Strategy<B> + Send> {{\n    Box::new(Hold)\n}}\n"
    )
}

#[test]
fn every_adapter_builds_its_sdk_at_its_tag_and_loads_here() {
    let set = sdk::built_in_adapters();
    assert!(set.broken.is_empty(), "this binary's adapters that do not read: {:?}", set.broken);
    let adapters = set.adapters;
    if adapters.is_empty() {
        eprintln!("no adapter in crates/vike-rust-strategy/adapters/: nothing to check");
        return;
    }
    let base = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("sdk-adapters");
    let (sdk_root, work) = (base.join("vike-strategy-source"), base.join("work"));
    // The builds OUTSIDE this checkout: cargo reads every `.cargo/config.toml` above its work directory,
    // which the build jail does not grant inside a checkout.
    let builds = render::scratch_root().join("sdk-adapters-check");
    for adapter in &adapters {
        install_tagged(&sdk_root, adapter.number, &work);
        let tree = sdk::boundary_hash(&sdk::snapshot_dir(&sdk_root, adapter.number));
        assert_eq!(
            adapter.boundary_hash,
            tree,
            "crates/vike-rust-strategy/adapters/sdk-{n}/ADAPTER records the boundary {}, and this vike hashes \
             the tag sdk-{n}'s tree {tree}: correct the adapter's marker",
            adapter.boundary_hash,
            n = adapter.number
        );
    }
    let host = adapters.iter().map(|a| a.number).max().unwrap_or_default() + 1;
    install_host(&sdk_root, host);

    let sdks = InstalledSdks::open(&sdk_root).unwrap_or_else(|e| panic!("the snapshots: {e}"));
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
    let child_env = render::child_env(&std::env::vars().collect());
    for SdkAdapter { number, .. } in &adapters {
        let text = fixture(*number);
        // The build runs offline, as the builder's does: the crates this SDK locks are fetched first, as the
        // daemon fetches every SDK it serves at its start, never left to what the runner's cargo home holds.
        let snapshot = sdks.resolve(None, render::pin_of(&text)).unwrap_or_else(|e| {
            panic!(
                "a file pinned to SDK {number} is refused on this boundary: {e}\n{}",
                sdks.report().join("\n")
            )
        });
        if let Err(why) = render::prefetch(&snapshot, &cargo, &child_env) {
            eprintln!(
                "SDK {number}: fetching its crates failed, the build reads the cargo home: {why}"
            );
        }
        let artifact = render::build_strategy_file(
            &text,
            &format!("sdk_adapter_check_{number}.rs"),
            &builds,
            &snapshot,
            &cargo,
            &child_env,
            Profile::of_this_binary(),
            &render::CargoHomePin::disabled(),
        )
        .unwrap_or_else(|e| {
            panic!("SDK {number} does not build through its adapter at its tag:\n{e}")
        });
        vike_rust_strategy::loader::load(&artifact).unwrap_or_else(|e| {
            panic!(
                "this host refuses the artifact SDK {number} built through its adapter ({}): {e}",
                artifact.display()
            )
        });
        eprintln!(
            "SDK {number}: built at its tag through its adapter and loaded: {}",
            artifact.display()
        );
        // The artifact's folder goes (the builder's retention never sweeps this root); its work directory
        // beside it stays, so the next run builds warm.
        if let Some(folder) = artifact.parent() {
            fs::remove_dir_all(folder).expect("the check's artifacts");
        }
    }
    let _ = fs::remove_dir_all(&work);
}

/// A snapshot folder an earlier run left is replaced, never refused: one holding another commit's snapshot
/// (here a marker naming another commit, and a file the tag does not have) is emptied first, and one of
/// this tag whose marker another interface version wrote is re-marked.
#[test]
fn an_install_over_a_stale_folder_takes_the_tag() {
    let Some(adapter) = sdk::built_in_adapters().adapters.into_iter().next() else {
        eprintln!("no adapter in crates/vike-rust-strategy/adapters/: nothing to check");
        return;
    };
    let number = adapter.number;
    // A root of its own: nextest runs each test in a process of its own, beside the check above.
    let base = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("sdk-adapters-stale");
    let (sdk_root, work) = (base.join("vike-strategy-source"), base.join("work"));
    let snapshot = sdk::snapshot_dir(&sdk_root, number);
    let _ = fs::remove_dir_all(&base);
    let stray = snapshot.join("crates/vike-model/src/stray.rs");
    fs::create_dir_all(stray.parent().expect("a parent")).expect("the stale folder");
    fs::write(&stray, "// left by another commit\n").expect("a stray file");
    let mut stale = SdkMarker::of_snapshot(&snapshot, number, "another commit", 1);
    stale.write(&snapshot).expect("a stale marker");

    let marker = install_tagged(&sdk_root, number, &work);
    assert_eq!(marker.commit, tagged(&format!("sdk-{number}")), "the tag's snapshot");
    assert!(!stray.exists(), "another commit's file is gone");
    assert_eq!(SdkMarker::read(&snapshot).as_ref(), Ok(&marker));

    // The same tag, marked by another interface version (an `ABI_VERSION` bump): re-marked in place.
    let _ = fs::remove_file(snapshot.join(sdk::SDK_MARKER_FILE));
    stale = marker.clone();
    stale.abi_version += 1;
    stale.write(&snapshot).expect("an older binary's marker");
    assert_eq!(install_tagged(&sdk_root, number, &work), marker);
    let _ = fs::remove_dir_all(&base);
}
