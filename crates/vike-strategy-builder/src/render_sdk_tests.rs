//! The SDK a build compiles against (decision 0296): what a source tree may be, the order a build
//! resolves its SDK in among the installed ones (an override, then the file's pin, then the newest
//! the usability rule accepts), the refusals' words, and where a build works.

use super::tests::test_workspace_root;
use super::*;

/// Install a miniature SDK `number` under `sdk_root`: this checkout's boundary crate as the glue copies it
/// (`GLUE_PARTS`: its boundary files among them, so its boundary is this host's `BOUNDARY_HASH`, unless
/// `moved` edits one, and its templates), then its marker.
fn install(sdk_root: &Path, number: u32, moved: bool) -> PathBuf {
    let dir = vike_rust_strategy::sdk::snapshot_dir(sdk_root, number);
    let crate_dir = vike_rust_strategy::sdk::RUST_STRATEGY_CRATE_DIR;
    let (from, to) = (test_workspace_root().join(crate_dir), dir.join(crate_dir));
    // The boundary (`crates/vike-rust-strategy/build/source_hash.rs`'s `BOUNDARY_FILES` and `BOUNDARY_FOLDERS`)
    // lies inside these.
    // And the crate's manifest, the glue's (`glue_manifest`), as a packed snapshot holds it.
    for part in GLUE_PARTS.iter().chain(&["Cargo.toml"]) {
        let (source, target) = (from.join(part), to.join(part));
        if source.is_dir() {
            copy_tree(&source, &target).expect("copy a boundary folder");
        } else if source.is_file() {
            fs::create_dir_all(target.parent().expect("a parent")).expect("the folder");
            fs::copy(&source, &target).expect("copy a boundary file");
        }
    }
    if moved {
        // Code, not a comment: the boundary is hashed as its tokens.
        let abi = to.join("src/abi.rs");
        let text =
            fs::read_to_string(&abi).expect("abi.rs") + "pub const ANOTHER_BOUNDARY: u8 = 0;\n";
        fs::write(&abi, text).expect("move the boundary");
    }
    // The root manifest, where the glue reads the versions its crate's `workspace = true` lines pin.
    fs::copy(test_workspace_root().join("Cargo.toml"), dir.join("Cargo.toml"))
        .expect("the root manifest");
    let marker = SdkMarker::of_snapshot(&dir, number, "0123456789", 14);
    // The list above is `source_hash.rs`'s, which this crate cannot name (a private module there): a boundary part
    // it leaves out moves the copy's hash, so it fails HERE, in these words, not as an unusable SDK later.
    assert!(
        moved || marker.boundary_hash == vike_rust_strategy::sdk::BOUNDARY_HASH,
        "install copies the whole SDK boundary (`crates/vike-rust-strategy/build/source_hash.rs`'s \
         `BOUNDARY_FILES` and `BOUNDARY_FOLDERS`)"
    );
    marker.write(&dir).expect("the marker");
    dir
}

#[test]
fn a_source_tree_is_an_installed_sdk_or_a_checkout_and_nothing_else() {
    let bare = tempfile::tempdir().expect("tempdir");
    let why = source_tree_refusal(bare.path()).expect("a tree that is neither is refused");
    assert!(why.contains(vike_rust_strategy::sdk::SDK_MARKER_FILE) && why.contains("--sdk-root"));
    assert!(
        matches!(SourceTree::open(bare.path()), Err(BuildError::Sdk { .. })),
        "a build refuses it"
    );

    let checkout = tempfile::tempdir().expect("tempdir");
    fs::create_dir(checkout.path().join(".git")).expect("plant .git");
    assert_eq!(source_tree_refusal(checkout.path()), None);
    assert_eq!(source_tree_refusal(&test_workspace_root()), None, "the real checkout");
    assert_eq!(SourceTree::open(&test_workspace_root()).map(|t| t.sdk).ok(), Some(Sdk::Dev));

    let sdks = tempfile::tempdir().expect("tempdir");
    let three = install(sdks.path(), 3, false);
    assert_eq!(source_tree_refusal(&three), None, "a snapshot its marker describes");
    assert_eq!(SourceTree::open(&three).map(|t| t.sdk).ok(), Some(Sdk::Number(3)));
    let abi = three.join(vike_rust_strategy::sdk::RUST_STRATEGY_CRATE_DIR).join("src/abi.rs");
    fs::write(&abi, "// edited by hand\n").expect("edit the snapshot");
    let why = source_tree_refusal(&three).expect("a snapshot whose bytes moved is refused");
    assert!(why.contains("SDK 3") && why.contains("--sdk 3"), "{why}");
}

/// The order: an explicit override, then the file's pin, then the newest SDK the usability rule accepts
/// (here SDK 3: SDK 4 is newer, and cut with another boundary). A named SDK that cannot be built is
/// refused with the fix, never swapped for another.
#[test]
fn a_build_resolves_the_override_then_the_pin_then_the_newest_usable_sdk() {
    let root = tempfile::tempdir().expect("tempdir");
    let nothing = InstalledSdks::open(root.path()).expect("an empty root");
    let why = nothing.resolve(None, None).expect_err("no SDK").to_string();
    assert!(why.contains("no SDK is installed"), "{why}");

    for number in [1, 2, 3] {
        install(root.path(), number, false);
    }
    install(root.path(), 4, true);
    let sdks = InstalledSdks::open(root.path()).expect("the SDKs");
    let dir = |number| vike_rust_strategy::sdk::snapshot_dir(root.path(), number);
    assert_eq!(sdks.resolve(None, None).ok(), Some(dir(3)), "the newest the rule accepts");
    assert_eq!(sdks.resolve(None, Some(2)).ok(), Some(dir(2)), "the file's pin");
    assert_eq!(sdks.resolve(Some(3), Some(2)).ok(), Some(dir(3)), "the override, over the pin");
    let numbers: Vec<u32> = sdks.usable_snapshots().into_iter().map(|(n, _)| n).collect();
    assert_eq!(numbers, [3, 2], "SDK 1 is outside the window, SDK 4 another boundary");

    let refused = |chosen, pin| match sdks.resolve(chosen, pin) {
        Err(BuildError::Sdk { why }) => why,
        other => panic!("expected an SDK refusal, got {other:?}"),
    };
    let retired = refused(None, Some(1));
    assert!(retired.contains("this file pins"), "{retired}");
    assert!(
        retired.contains("SDK 1 is retired") && retired.contains("migrate <FILE> --to 4"),
        "{retired}"
    );
    let moved = refused(None, Some(4));
    assert!(moved.contains("another strategy boundary"), "{moved}");
    let absent = refused(Some(9), None);
    assert!(absent.contains("this run asks for"), "{absent}");
    assert!(absent.contains("SDK 9 is not installed") && absent.contains("--sdk 9"), "{absent}");
    let report = sdks.report().join("\n");
    assert!(report.contains("SDK 3 at") && report.contains("SDK 1: NOT usable"), "{report}");
}

/// A snapshot whose bytes moved after its marker was written is refused at the builder's start, and so
/// never built against, whatever asks for it.
#[test]
fn a_snapshot_that_fails_its_marker_is_never_built_against() {
    let root = tempfile::tempdir().expect("tempdir");
    let one = install(root.path(), 1, false);
    let abi = one.join(vike_rust_strategy::sdk::RUST_STRATEGY_CRATE_DIR).join("src/abi.rs");
    fs::write(&abi, "// edited by hand\n").expect("edit the snapshot");
    let sdks = InstalledSdks::open(root.path()).expect("the SDKs");
    assert!(sdks.report()[0].contains("NOT usable"), "{:?}", sdks.report());
    let why = sdks.resolve(None, Some(1)).expect_err("the pin").to_string();
    assert!(why.contains("not the snapshot its marker describes"), "{why}");
    let why = sdks.resolve(None, None).expect_err("the newest").to_string();
    assert!(why.contains("is usable on this vike"), "{why}");
}

/// A numbered SDK's work directories are persistent, beside its build folders under the builds
/// directory; SDK dev's stay under the scratch root. One folder per tree and kind.
#[test]
fn a_numbered_sdk_works_beside_its_folders_and_sdk_dev_in_the_scratch_root() {
    let builds = Path::new("/srv/p/user_data/strategy-builds");
    let at = |sdk, kind| work_directory(builds, sdk, "0123abcd", kind);
    assert_eq!(at(Sdk::Number(3), TemplateKind::Strategy), builds.join("sdk-3/work-0123abcd"));
    assert_eq!(at(Sdk::Number(3), TemplateKind::Study), builds.join("sdk-3/work-0123abcd-study"));
    assert_eq!(
        at(Sdk::Dev, TemplateKind::StrategyOnnx),
        scratch_root().join("sdk-dev").join("work-0123abcd-onnx")
    );
    assert!(is_work_of("work-0123abcd", TemplateKind::Strategy));
    assert!(!is_work_of("work-0123abcd-study", TemplateKind::Strategy));
    assert!(is_work_of("work-0123abcd-study", TemplateKind::Study));
    assert!(is_work_directory("work-0123abcd") && !is_work_directory("0123abcd"));
    assert!(is_pre_sdk_scratch("0123abcd-5f2e9a1b3c4d-onnx"));
    assert!(!is_pre_sdk_scratch("prefetch-5f2e9a1b3c4d") && !is_pre_sdk_scratch("sdk-dev"));

    let checkout = test_workspace_root();
    let folder_of = |root: &Path, kind, profile| build_folder(root, kind, profile).expect("folder");
    let (sdk, folder) = folder_of(&checkout, TemplateKind::Strategy, Profile::Debug);
    assert_eq!((sdk, folder.len()), (Sdk::Dev, 8));
    assert_ne!(folder, folder_of(&checkout, TemplateKind::Study, Profile::Debug).1, "per kind");
    assert_ne!(folder, folder_of(&checkout, TemplateKind::Strategy, Profile::Release).1);
    let root = tempfile::tempdir().expect("tempdir");
    let two = install(root.path(), 2, false);
    let (sdk, snapshot) = folder_of(&two, TemplateKind::Strategy, Profile::Debug);
    assert_eq!(sdk, Sdk::Number(2));
    assert_ne!(snapshot, folder, "a snapshot's folder is its own sources'");
}

/// The sweep of one SDK's builds touches its work directories alone: a build folder of artifacts
/// beside them is never swept, however old.
#[test]
fn the_sweep_of_an_sdk_touches_its_work_directories_alone() {
    let sdk = tempfile::tempdir().expect("tempdir");
    let now = SystemTime::now();
    let old = now - SCRATCH_MAX_AGE - Duration::from_secs(60);
    let make = |name: &str| {
        let dir = sdk.path().join(name);
        fs::create_dir_all(&dir).expect("dir");
        fs::write(dir.join("Cargo.toml"), "").expect("stamp");
        let stamp = fs::File::options().write(true).open(dir.join("Cargo.toml"));
        stamp.and_then(|f| f.set_modified(old)).expect("age it");
        dir
    };
    let (work, folder, mine) = (make("work-0123abcd"), make("0123abcd"), make("work-89abcdef"));
    let swept =
        prune_stale_scratch(sdk.path(), &mine, is_work_directory, SCRATCH_MAX_AGE, None, now);
    assert_eq!(swept, 1);
    assert!(!work.exists() && folder.exists() && mine.exists());
}

/// The pin a file's header states, read for the resolution; none for a header that names none or a
/// text with no header.
#[test]
fn the_pin_is_the_headers_sdk() {
    let pinned = "//! ```toml\n//! [strategy]\n//! sdk = 3\n//! ```\npub fn build() {}\n";
    assert_eq!(pin_of(pinned), Some(3));
    assert_eq!(pin_of("//! ```toml\n//! [strategy]\n//! ```\npub fn build() {}\n"), None);
    assert_eq!(pin_of("pub fn build() {}\n"), None);
}

/// Run `f` with `adapters` as this host's, each `(number, boundary, face)`: an adapter for SDK `number`, cut
/// with `boundary`, its face `face` (`vike_rust_strategy::sdk::with_adapters`, in place of the ones this binary
/// was built with).
fn serving<T>(adapters: &[(u32, &str, &str)], f: impl FnOnce() -> T) -> T {
    use vike_rust_strategy::sdk::{AdapterSet, SdkAdapter, with_adapters};
    let adapters = adapters
        .iter()
        .map(|&(number, boundary, face)| SdkAdapter {
            number,
            boundary_hash: boundary.to_string(),
            face: face.to_string(),
        })
        .collect();
    with_adapters(AdapterSet { adapters, broken: Vec::new() }, f)
}

/// **An SDK cut with another boundary builds through THIS host's adapter, inside the window, or is refused**
/// (decision 0296's step 6). SDK 4 holds this host's boundary; 1 and 3 another. Without an adapter 3 is
/// refused in the rule's words; with one it resolves and opens adapted: this host's templates, its
/// `vike-rust-strategy` the glue laid out beside the crate, whose boundary hash IS this host's (the
/// copy is exact) and whose face is the adapter's, against SDK 3's own crates. The folder moves with the
/// adapter's code, never with a comment in it. SDK 1, outside the window, and SDK 2, whose lent rows are laid
/// out otherwise, are retired whatever adapter exists. No cargo runs.
#[test]
fn an_sdk_of_another_boundary_builds_through_this_hosts_adapter_inside_the_window() {
    use vike_rust_strategy::sdk::{BOUNDARY_HASH, boundary_hash};
    let root = tempfile::tempdir().expect("tempdir");
    let (one, three, four) = (
        install(root.path(), 1, true),
        install(root.path(), 3, true),
        install(root.path(), 4, false),
    );
    let moved = SdkMarker::read(&three).expect("SDK 3's marker").boundary_hash;
    assert_ne!(moved, BOUNDARY_HASH);

    let sdks = serving(&[], || InstalledSdks::open(root.path())).expect("the SDKs");
    let why = sdks.resolve(None, Some(3)).expect_err("no adapter yet").to_string();
    assert!(why.contains("another strategy boundary") && why.contains("no adapter"), "{why}");
    let refused =
        serving(&[], || SourceTree::open(&three).map(drop)).expect_err("a build refuses it");
    assert!(
        matches!(&refused, BuildError::Sdk { why } if why.contains("no adapter")),
        "{refused:?}"
    );

    let face = fs::read_to_string(
        test_workspace_root()
            .join(vike_rust_strategy::sdk::RUST_STRATEGY_CRATE_DIR)
            .join("src/guest/face.rs"),
    )
    .expect("this host's own face");
    let held = [(3, moved.as_str(), face.as_str()), (1, moved.as_str(), face.as_str())];
    let sdks = serving(&held, || InstalledSdks::open(root.path())).expect("the SDKs, adapted");
    assert_eq!(
        sdks.resolve(None, Some(3)).ok(),
        Some(three.clone()),
        "the pin, through its adapter"
    );
    let report = sdks.report().join("\n");
    assert!(report.contains("SDK 3 at") && report.contains("through its adapter"), "{report}");
    let retired = sdks.resolve(None, Some(1)).expect_err("outside the window").to_string();
    assert!(retired.contains("SDK 1 is retired"), "{retired}");
    let one = serving(&held, || SourceTree::open(&one).map(drop));
    assert!(matches!(one, Err(BuildError::Sdk { why }) if why.contains("retired")));

    let tree = serving(&held, || SourceTree::open(&three)).expect("SDK 3, adapted");
    let adapted = tree.adapted.as_ref().expect("adapted");
    assert_eq!(
        adapted.host, four,
        "the glue and the templates come from the tree of this boundary"
    );
    let manifest = tree.cargo_toml(TemplateKind::Strategy);
    assert!(manifest.contains(&format!("path = \"{GLUE_DIR}\"")), "{manifest}");
    let three_model = three.join("crates").join("vike-model").display().to_string();
    assert!(manifest.contains(&three_model), "SDK 3's own crates: {manifest}");

    let work = tempfile::tempdir().expect("a work directory");
    tree.lay_out(work.path()).expect("the glue");
    let glue = work.path().join(GLUE_DIR);
    assert_eq!(
        boundary_hash(&work.path().join("boundary")),
        BOUNDARY_HASH,
        "the glue's boundary is this host's, so the artifact's record states it"
    );
    assert_eq!(fs::read_to_string(glue.join("adapter/face.rs")).ok(), Some(face.clone()));
    let glue_manifest = fs::read_to_string(glue.join("Cargo.toml")).expect("the glue's manifest");
    assert!(
        glue_manifest.contains(&three_model) && !glue_manifest.contains("{{"),
        "{glue_manifest}"
    );
    let stamp = fs::metadata(glue.join("src/abi.rs")).and_then(|m| m.modified()).expect("mtime");
    tree.lay_out(work.path()).expect("the glue again");
    let again = fs::metadata(glue.join("src/abi.rs")).and_then(|m| m.modified()).expect("mtime");
    assert_eq!(stamp, again, "an unchanged file is not written again: cargo's mtimes stay");

    let folder = tree.folder(TemplateKind::Strategy, Profile::Debug);
    let folder_with = |face: &str| {
        let tree =
            serving(&[(3, moved.as_str(), face)], || SourceTree::open(&three)).expect("SDK 3");
        tree.folder(TemplateKind::Strategy, Profile::Debug)
    };
    assert_eq!(folder_with(&(face.clone() + "\n// a comment, re-wrapped\n")), folder, "a comment");
    assert_ne!(folder_with(&(face.clone() + "const _: () = ();\n")), folder, "another adapter");

    // SDK 2 lays out a lent row otherwise (its golden says `Bar`'s symbol comes first): retired, though an
    // adapter for it is held (decision 0296's verdict 7), and never built.
    let two = install(root.path(), 2, true);
    let golden = two
        .join(vike_rust_strategy::sdk::RUST_STRATEGY_CRATE_DIR)
        .join(vike_rust_strategy::sdk::LENT_FIELDS_FILE);
    let text = fs::read_to_string(&golden).expect("its lent fields");
    let symbol = text.lines().find(|l| l.starts_with("Bar.symbol:")).expect("Bar's symbol");
    let reordered: String = std::iter::once(symbol)
        .chain(text.lines().filter(|l| *l != symbol))
        .map(|l| format!("{l}\n"))
        .collect();
    fs::write(&golden, reordered).expect("move its row");
    fs::remove_file(two.join(vike_rust_strategy::sdk::SDK_MARKER_FILE)).expect("its old marker");
    let marker = SdkMarker::of_snapshot(&two, 2, "0123456789", 14);
    marker.write(&two).expect("its marker");
    let held = [(2, marker.boundary_hash.as_str(), face.as_str())];
    let sdks = serving(&held, || InstalledSdks::open(root.path())).expect("the SDKs");
    let why = sdks.resolve(None, Some(2)).expect_err("rows moved").to_string();
    assert!(why.contains("SDK 2 is retired") && why.contains("migrate <FILE> --to 4"), "{why}");
    let refused =
        serving(&held, || SourceTree::open(&two).map(drop)).expect_err("a build refuses it");
    assert!(
        matches!(&refused, BuildError::Sdk { why } if why.contains("is retired")),
        "{refused:?}"
    );
}

/// The glue's manifest is the host tree's own `vike-rust-strategy` manifest (one commit's sources and
/// dependencies): its dependencies by name, a `path` one at the SDK's crate and a `workspace = true` one at the
/// root's pin, `adapted` the default feature; a dependency line of another shape is refused in words (a line
/// scan: this crate parses no TOML).
#[test]
fn the_glue_manifest_is_the_host_crates_own_against_the_sdks_crates() {
    let names = |text: &str| -> BTreeSet<String> {
        let mut in_dependencies = false;
        let mut found = BTreeSet::new();
        for line in text.lines().map(str::trim) {
            if line.starts_with('[') {
                in_dependencies = line == "[dependencies]";
            } else if in_dependencies
                && !line.starts_with('#')
                && let Some((name, _)) = line.split_once(" = ")
            {
                found.insert(name.to_string());
            }
        }
        found
    };
    let host = test_workspace_root();
    let crate_manifest =
        host.join(vike_rust_strategy::sdk::RUST_STRATEGY_CRATE_DIR).join("Cargo.toml");
    let own = fs::read_to_string(&crate_manifest).expect("the boundary crate's manifest");
    let sdk = Path::new("/srv/p/vendor/vike-strategy-source/sdk-3");
    let manifest = glue_manifest(&host, sdk).expect("this checkout's manifest renders");
    assert!(!names(&own).is_empty());
    assert_eq!(names(&manifest), names(&own));
    assert!(!manifest.contains("workspace = true"), "{manifest}");
    let model = sdk.join("crates").join("vike-model").display().to_string();
    assert!(manifest.contains(&format!("vike-model = {{ path = \"{model}\" }}")), "{manifest}");
    assert!(manifest.contains("default = [\"adapted\"]") && manifest.contains("adapted = []"));

    let odd = tempfile::tempdir().expect("tempdir");
    let crate_dir = odd.path().join(vike_rust_strategy::sdk::RUST_STRATEGY_CRATE_DIR);
    fs::create_dir_all(&crate_dir).expect("the crate");
    fs::write(
        odd.path().join("Cargo.toml"),
        "[workspace.dependencies]
",
    )
    .expect("a root");
    fs::write(
        crate_dir.join("Cargo.toml"),
        "[dependencies]
odd = { git = \"x\" }
",
    )
    .expect("a crate");
    let why = glue_manifest(odd.path(), sdk).expect_err("a line of another shape");
    assert!(why.contains("odd = { git"), "{why}");
}
