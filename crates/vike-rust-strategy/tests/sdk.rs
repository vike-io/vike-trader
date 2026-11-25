//! The versioned strategy SDK's vocabulary (`src/sdk.rs`): one source hash per template kind, the
//! marker's round trip and its verifier refusing a changed snapshot, the installed listing, and the one
//! usability rule (inside the window, outside it, a boundary mismatch, an adapter) and the adapters a tree holds. The miniature snapshots live under
//! `CARGO_TARGET_TMPDIR`, one per test process.

use std::fs;
use std::path::{Path, PathBuf};

use vike_rust_strategy::sdk::{
    ADAPTER_FACE_FILE, ADAPTER_FILE, AdapterSet, BOUNDARY_HASH, LENT_FIELDS, LENT_FIELDS_FILE,
    SDK_MARKER_FILE, Sdk, SdkAdapter, SdkMarker, SdkSourceHashes, SdkUnusable, SdkUse,
    TemplateKind, adapters, boundary_hash, built_in_adapters, host_adapters, host_tree, installed,
    snapshot_dir, source_hash, usable,
};

/// The miniature snapshot's ONNX crate source. Spelled in two literals so xtask's scan for another crate's
/// `.rs` read as text (`xtask/src/ci/tables/readers.rs`'s `SOURCES_READ_AS_TEXT`) does not take this planted
/// path for a read of the real file, nor the path-key gate (`crates/vike-ops/tests/hygiene/path_key_gate.rs`) for a
/// key git must track.
const ONNX_LIB: &str = concat!("crates", "/vike-ml-onnx-prediction/src/lib.rs");

/// A miniature snapshot: the strategy template names `vike-a` and the opt-in ONNX crate, the study
/// template `vike-s`; the boundary is one `abi.rs`.
fn snapshot(tag: &str) -> PathBuf {
    let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("sdk-{tag}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let files = [
        ("Cargo.toml", "[workspace]\nmembers = []\n\n[workspace.dependencies]\nserde = \"1\"\n"),
        (
            "crates/vike-rust-strategy/template/Cargo.toml.in",
            "[dependencies]\nvike-a = { path = \"{{A}}\" }\n\
             vike-ml-onnx-prediction = { path = \"{{O}}\" }\n",
        ),
        (
            "crates/vike-rust-strategy/template/study_Cargo.toml.in",
            "[dependencies]\nvike-s = { path = \"{{S}}\" }\n",
        ),
        ("crates/vike-rust-strategy/src/abi.rs", "pub const ABI_VERSION: u32 = 14;\n"),
        ("crates/vike-a/Cargo.toml", "[package]\nname = \"vike-a\"\n"),
        ("crates/vike-a/src/lib.rs", "pub fn a() {}\n"),
        ("crates/vike-ml-onnx-prediction/Cargo.toml", "[package]\nname = \"o\"\n"),
        (ONNX_LIB, "pub fn o() {}\n"),
        ("crates/vike-s/Cargo.toml", "[package]\nname = \"vike-s\"\n"),
        ("crates/vike-s/src/lib.rs", "pub fn s() {}\n"),
    ];
    for (file, text) in files {
        write(&root, file, text);
    }
    root
}

fn write(root: &Path, file: &str, text: &str) {
    let path = root.join(file);
    fs::create_dir_all(path.parent().expect("a parent")).expect("the folder");
    fs::write(path, text).expect("the file");
}

fn hashes(root: &Path) -> [String; 3] {
    TemplateKind::ALL.map(|kind| source_hash(root, kind))
}

/// One hash per kind: the ONNX crate moves the onnx kind's alone, a study-only crate the study kind's
/// alone, a crate every strategy links both strategy kinds'.
#[test]
fn each_template_kind_has_its_own_source_hash() {
    let root = snapshot("kinds");
    let [strategy, onnx, study] = hashes(&root);
    assert!(strategy != onnx && onnx != study && strategy != study, "{strategy} {onnx} {study}");
    assert_eq!(hashes(&root), [strategy.clone(), onnx.clone(), study.clone()], "stable");

    write(&root, ONNX_LIB, "pub fn o() -> u8 { 1 }\n");
    let [strategy_2, onnx_2, study_2] = hashes(&root);
    assert_eq!(
        (&strategy_2, &study_2),
        (&strategy, &study),
        "the ONNX crate is no plain strategy's"
    );
    assert_ne!(onnx_2, onnx);

    write(&root, "crates/vike-s/src/lib.rs", "pub fn s() -> u8 { 1 }\n");
    let [strategy_3, onnx_3, study_3] = hashes(&root);
    assert_eq!(
        (&strategy_3, &onnx_3),
        (&strategy_2, &onnx_2),
        "a study-only crate is no strategy's"
    );
    assert_ne!(study_3, study_2);

    write(&root, "crates/vike-a/src/lib.rs", "pub fn a() -> u8 { 1 }\n");
    let [strategy_4, onnx_4, study_4] = hashes(&root);
    assert!(strategy_4 != strategy_3 && onnx_4 != onnx_3 && study_4 == study_3);
    let _ = fs::remove_dir_all(&root);
}

/// The marker is written once, read back equal, left untouched by an identical write, refuses a
/// different one, and refuses an unknown key.
#[test]
fn the_marker_round_trips_and_an_installed_one_never_changes() {
    let root = snapshot("round-trip");
    let marker = SdkMarker::of_snapshot(&root, 3, "a3ca741107", 14);
    assert_eq!(marker.source_hash, SdkSourceHashes::of_tree(&root));
    assert_eq!(marker.boundary_hash, boundary_hash(&root));
    marker.write(&root).expect("the first write");
    assert_eq!(SdkMarker::read(&root).expect("read back"), marker);
    let text = fs::read_to_string(root.join(SDK_MARKER_FILE)).expect("the text");
    assert!(text.contains("number = 3") && text.contains("[source_hash]"), "{text}");

    let written =
        fs::metadata(root.join(SDK_MARKER_FILE)).and_then(|m| m.modified()).expect("mtime");
    marker.write(&root).expect("the same marker again touches nothing");
    let after = fs::metadata(root.join(SDK_MARKER_FILE)).and_then(|m| m.modified()).expect("mtime");
    assert_eq!(written, after);

    let other = SdkMarker { commit: "0000000000".to_string(), ..marker.clone() };
    let refused = other.write(&root).expect_err("another marker over an installed SDK");
    assert!(refused.contains("an installed SDK never changes"), "{refused}");
    assert_eq!(SdkMarker::read(&root).expect("unchanged"), marker);

    fs::write(root.join(SDK_MARKER_FILE), format!("{text}extra = 1\n")).expect("an unknown key");
    let refused = SdkMarker::read(&root).expect_err("an unknown key");
    assert!(refused.contains("does not parse"), "{refused}");
    let _ = fs::remove_dir_all(&root);
}

/// The verifier recomputes the snapshot's hashes and refuses, in words, one whose bytes moved: a source
/// of the closure, or the boundary.
#[test]
fn the_verifier_refuses_a_changed_snapshot() {
    let root = snapshot("verify");
    let marker = SdkMarker::of_snapshot(&root, 4, "a3ca741107", 14);
    marker.verify(&root).expect("the snapshot it was written from");

    write(&root, "crates/vike-a/src/lib.rs", "pub fn a() -> u8 { 1 }\n");
    let refused = marker.verify(&root).expect_err("a changed source");
    assert!(
        refused.contains("SDK 4") && refused.contains("is not the snapshot its marker describes"),
        "{refused}"
    );
    assert!(
        refused.contains("the strategy source hash") && refused.contains("the onnx source hash")
    );
    assert!(
        !refused.contains("the study source hash") && !refused.contains("boundary"),
        "{refused}"
    );
    assert!(refused.contains("--sdk 4"), "the fix: {refused}");

    write(&root, "crates/vike-a/src/lib.rs", "pub fn a() {}\n");
    marker.verify(&root).expect("restored");
    // A comment or a layout edit is no change of what the snapshot builds: its sources are hashed as tokens.
    write(&root, "crates/vike-a/src/lib.rs", "/// A doc.\npub fn a()\n{} // a comment\n");
    marker.verify(&root).expect("comments and layout alone");
    write(&root, "crates/vike-rust-strategy/src/abi.rs", "pub const ABI_VERSION: u32 = 15;\n");
    let refused = marker.verify(&root).expect_err("a changed boundary");
    assert!(refused.contains("the boundary hash"), "{refused}");
    let _ = fs::remove_dir_all(&root);
}

/// An SDK folder is `sdk-N` (one spelling per number) or `sdk-dev`; the listing takes every numbered
/// folder holding a marker, ascending.
#[test]
fn the_installed_sdks_are_the_numbered_folders_holding_a_marker() {
    for (name, sdk) in [("sdk-3", Some(Sdk::Number(3))), ("sdk-dev", Some(Sdk::Dev))] {
        assert_eq!(Sdk::of_directory_name(name), sdk);
        assert_eq!(sdk.map(Sdk::directory_name).as_deref(), Some(name));
    }
    for name in ["sdk-03", "sdk-", "sdk-x", "3", "sdk--1", "SDK-3"] {
        assert_eq!(Sdk::of_directory_name(name), None, "{name}");
    }
    let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("sdk-installed-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    assert_eq!(installed(&root), Ok(Vec::new()), "a missing root is none");
    for name in ["sdk-10", "sdk-9", "sdk-03", "sdk-dev"] {
        write(&root, &format!("{name}/{SDK_MARKER_FILE}"), "");
    }
    fs::create_dir_all(root.join("sdk-11")).expect("an SDK folder with no marker");
    assert_eq!(installed(&root), Ok(vec![9, 10]));
    assert_eq!(snapshot_dir(&root, 9), root.join("sdk-9"));
    let _ = fs::remove_dir_all(&root);
}

fn marker(number: u32, boundary: &str) -> SdkMarker {
    let hash = "0123456789abcdef".to_string();
    SdkMarker {
        number,
        commit: "a3ca741107".to_string(),
        abi_version: 14,
        boundary_hash: boundary.to_string(),
        source_hash: SdkSourceHashes { strategy: hash.clone(), onnx: hash.clone(), study: hash },
    }
}

/// A snapshot holding only `golden` as its lent fields (`None`: none), for the rule's read of them.
fn rows_snapshot(tag: &str, golden: Option<&str>) -> PathBuf {
    let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("sdk-rows-{tag}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).expect("the snapshot");
    if let Some(golden) = golden {
        write(&root, &format!("crates/vike-rust-strategy/{LENT_FIELDS_FILE}"), golden);
    }
    root
}

/// The usability rule: the newest SDK and the two before it build, an older one is retired whatever
/// adapter exists; an SDK cut with another boundary builds only through an adapter of THIS host naming its
/// number and its snapshot's boundary as this vike hashes it (decision 0296's step 6: whatever walk wrote
/// its marker, SDK 1's of the bytes too), and never when it lays out a lent row otherwise: then it is
/// retired, adapter or not (verdict 7, the owner's "Б" of 2026-11-20). Each refusal names the fix.
#[test]
fn an_sdk_is_usable_inside_the_window_with_the_host_boundary_or_an_adapter() {
    let same_rows = rows_snapshot("same", Some(LENT_FIELDS));
    for number in [7, 8, 9, 10] {
        let same = usable(&marker(number, BOUNDARY_HASH), &same_rows, 9, &[]);
        assert_eq!(same, Ok(SdkUse::SameBoundary), "SDK {number}");
    }
    let retired = usable(&marker(6, BOUNDARY_HASH), &same_rows, 9, &[])
        .expect_err("SDK 6 retires when 9 ships");
    assert_eq!(retired, SdkUnusable::Retired { number: 6, newest: 9 });
    let words = retired.to_string();
    assert!(words.contains("SDK 7 to 9") && words.contains("migrate <FILE> --to 9"), "{words}");

    let other = "ffffffffffffffff";
    let moved = usable(&marker(9, other), &same_rows, 9, &[]).expect_err("another boundary");
    assert!(matches!(moved, SdkUnusable::BoundaryMoved { number: 9, .. }), "{moved:?}");
    let words = moved.to_string();
    assert!(words.contains(other) && words.contains(BOUNDARY_HASH), "{words}");
    assert!(words.contains("no adapter") && words.contains("migrate <FILE> --to 9"), "{words}");

    // An adapter names the boundary it adapts from as this vike hashes the SDK's tree: here each miniature
    // snapshot's own.
    let adapter = |number: u32, snapshot: &Path| SdkAdapter {
        number,
        boundary_hash: boundary_hash(snapshot),
        face: String::new(),
    };
    let held = |snapshot: &Path| [adapter(7, snapshot), adapter(8, snapshot)];
    assert_eq!(
        usable(&marker(8, other), &same_rows, 9, &held(&same_rows)),
        Ok(SdkUse::Adapted(adapter(8, &same_rows))),
        "inside the window, another boundary, the same rows, and this host's adapter from it"
    );
    let elsewhere =
        SdkAdapter { boundary_hash: "eeeeeeeeeeeeeeee".to_string(), ..adapter(8, &same_rows) };
    let elsewhere = usable(&marker(8, other), &same_rows, 9, &[elsewhere]);
    assert!(
        matches!(elsewhere, Err(SdkUnusable::BoundaryMoved { number: 8, .. })),
        "an adapter from another boundary is no adapter from this one: {elsewhere:?}"
    );
    let unheld = usable(&marker(9, other), &same_rows, 9, &held(&same_rows));
    assert!(matches!(unheld, Err(SdkUnusable::BoundaryMoved { .. })), "{unheld:?}");
    let old = usable(&marker(6, other), &same_rows, 9, &[adapter(6, &same_rows)]);
    assert_eq!(old, Err(SdkUnusable::Retired { number: 6, newest: 9 }), "outside the window");
    assert_eq!(
        usable(&marker(9, BOUNDARY_HASH), &same_rows, 9, &held(&same_rows)),
        Ok(SdkUse::SameBoundary)
    );

    // A lent row laid out otherwise (`Bar`'s symbol first): retired, though an adapter is held.
    let symbol = LENT_FIELDS.lines().find(|l| l.starts_with("Bar.symbol:")).expect("Bar's symbol");
    let reordered: String = std::iter::once(symbol)
        .chain(LENT_FIELDS.lines().filter(|l| *l != symbol))
        .map(|l| format!("{l}\n"))
        .collect();
    let moved_rows = rows_snapshot("moved", Some(&reordered));
    let rows =
        usable(&marker(8, other), &moved_rows, 9, &held(&moved_rows)).expect_err("rows moved");
    assert_eq!(rows, SdkUnusable::RowsMoved { number: 8, newest: 9 });
    let words = rows.to_string();
    assert!(
        words.contains("SDK 8 is retired") && words.contains("migrate <FILE> --to 9"),
        "{words}"
    );
    let none = rows_snapshot("none", None);
    let unread = usable(&marker(8, other), &none, 9, &held(&none));
    assert_eq!(unread, Err(SdkUnusable::RowsMoved { number: 8, newest: 9 }), "no golden: refused");
    // The boundary's own rows (`OrderListLegAbi`) are the glue's, whatever the SDK: they move nothing.
    let own: String =
        LENT_FIELDS
            .lines()
            .map(|l| {
                if l.starts_with("OrderListLegAbi.") { format!("{l} \n") } else { format!("{l}\n") }
            })
            .collect();
    let own_rows = rows_snapshot("own", Some(&own));
    assert_eq!(
        usable(&marker(8, other), &own_rows, 9, &held(&own_rows)),
        Ok(SdkUse::Adapted(adapter(8, &own_rows)))
    );
    // A golden cut before sizes and offsets were recorded (SDK 1's: the field types alone) compares on the
    // types: the same ones adapt, another one retires.
    let types_only: String = LENT_FIELDS
        .lines()
        .filter_map(|l| l.rsplit_once(" @ ").map(|(field, _)| format!("{field}\n")))
        .collect();
    assert!(types_only.starts_with("Bar.timestamp: i64\n"), "{types_only}");
    let typed_rows = rows_snapshot("typed", Some(&types_only));
    assert_eq!(
        usable(&marker(8, other), &typed_rows, 9, &held(&typed_rows)),
        Ok(SdkUse::Adapted(adapter(8, &typed_rows)))
    );
    let retyped_rows = rows_snapshot(
        "retyped",
        Some(&types_only.replacen("Bar.timestamp: i64\n", "Bar.timestamp: u64\n", 1)),
    );
    assert_eq!(
        usable(&marker(8, other), &retyped_rows, 9, &held(&retyped_rows)),
        Err(SdkUnusable::RowsMoved { number: 8, newest: 9 })
    );
    for dir in [same_rows, moved_rows, none, own_rows, typed_rows, retyped_rows] {
        let _ = fs::remove_dir_all(dir);
    }
}

/// **An SDK cut with `String` symbols in its rows (`sdk-1`) is retired by the `Symbol` rows** (decision 0296's
/// amendment of 2026-11-20, policy "Б"): its golden says `String` where this host's says `Symbol`, so it is
/// refused in words naming `migrate`, even inside the window and with an adapter from it held (an artifact
/// built against those rows states another `abi::layout`, which the loader refuses on its own).
#[test]
fn an_sdk_with_string_symbols_is_retired_naming_migrate() {
    let symbol = "vike_marketdata::symbol::Symbol";
    assert_eq!(LENT_FIELDS.matches(symbol).count(), 3, "the bar's and both ticks' symbols");
    // `sdk-1`'s own golden (its tag, `203e18f9e4`): each field's type alone, the symbols `String`s.
    let types_only = |text: &str| -> String {
        text.lines()
            .filter_map(|line| line.rsplit_once(" @ ").map(|(field, _)| format!("{field}\n")))
            .collect()
    };
    let string_rows = types_only(LENT_FIELDS)
        .replace(
            &format!("core::option::Option<{symbol}>"),
            "core::option::Option<alloc::string::String>",
        )
        .replace(symbol, "alloc::string::String");
    assert!(string_rows.contains("\nTickTrade.symbol: alloc::string::String\n"), "{string_rows}");
    let other = "3498e4cf9f88148c";
    // An adapter from SDK 1 as this vike hashes its snapshot's boundary (the shape `usable` matches).
    let adapter = |snapshot: &Path| SdkAdapter {
        number: 1,
        boundary_hash: boundary_hash(snapshot),
        face: String::new(),
    };
    // The same golden with this host's symbol type is adapted: only the symbol retires SDK 1.
    let same_types = rows_snapshot("symbol-types", Some(&types_only(LENT_FIELDS)));
    assert_eq!(
        usable(&marker(1, other), &same_types, 2, &[adapter(&same_types)]),
        Ok(SdkUse::Adapted(adapter(&same_types)))
    );
    let sdk_one = rows_snapshot("string-symbols", Some(&string_rows));
    let refused = usable(&marker(1, other), &sdk_one, 2, &[adapter(&sdk_one)])
        .expect_err("String rows retire");
    assert_eq!(refused, SdkUnusable::RowsMoved { number: 1, newest: 2 });
    let words = refused.to_string();
    assert!(
        words.contains("SDK 1 is retired") && words.contains("migrate <FILE> --to 2"),
        "{words}"
    );
    for dir in [same_types, sdk_one] {
        let _ = fs::remove_dir_all(dir);
    }
}

/// The adapters a tree holds (`adapters/sdk-N/ADAPTER` beside its `face.rs`), ascending; a broken one is
/// said in words, never skipped. This host's are the ones it was built with, served where an installed SDK
/// holds its boundary (the glue's source): this checkout's, every one of which reads.
#[test]
fn this_hosts_adapters_are_its_own_where_an_sdk_holds_its_boundary() {
    let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("sdk-adapters-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let host = root.join("sdk-4");
    assert_eq!(adapters(&host), AdapterSet::default(), "a tree with no adapters folder");
    let adapter_of = |number: u32| SdkAdapter::dir(&host, number);
    let marker_text =
        |number: u32| format!("number = {number}\nboundary_hash = \"ffffffffffffffff\"\n");
    for number in [3, 2] {
        write(&adapter_of(number), ADAPTER_FILE, &marker_text(number));
        write(&adapter_of(number), ADAPTER_FACE_FILE, "// a face\n");
    }
    fs::create_dir_all(SdkAdapter::dir(&host, 1)).expect("a folder with no marker is no adapter");
    let read = adapters(&host).adapters;
    assert_eq!(read.iter().map(|a| a.number).collect::<Vec<_>>(), [2, 3]);
    assert_eq!(
        (read[0].boundary_hash.as_str(), read[0].face.as_str()),
        ("ffffffffffffffff", "// a face\n")
    );

    // This host's adapters are its own, never the installed SDK's: none until an installed SDK holds this
    // boundary, and only one whose sources its marker describes (a half-unpacked tree is passed over).
    write(&root.join("sdk-3"), SDK_MARKER_FILE, &marker(3, "ffffffffffffffff").to_toml());
    write(&host, SDK_MARKER_FILE, &marker(4, BOUNDARY_HASH).to_toml());
    assert_eq!(host_tree(&root), None, "a marker its tree's bytes do not match");
    let checkout = Path::new(env!("CARGO_MANIFEST_DIR"));
    for part in ["src/abi.rs", "src/guest.rs", "src/host.rs", "src/study.rs"] {
        let text = fs::read_to_string(checkout.join(part)).expect("a boundary file");
        write(&host, &format!("crates/vike-rust-strategy/{part}"), &text);
    }
    for folder in ["src/guest", "src/host", "template", "tests/boundary"] {
        for entry in fs::read_dir(checkout.join(folder)).expect("a boundary folder").flatten() {
            let text = fs::read_to_string(entry.path()).expect("a boundary file");
            let name = entry.file_name().to_string_lossy().into_owned();
            write(&host, &format!("crates/vike-rust-strategy/{folder}/{name}"), &text);
        }
    }
    fs::remove_file(host.join(SDK_MARKER_FILE)).expect("the hand-written marker");
    let this_host = SdkMarker::of_snapshot(&host, 4, "a3ca741107", 14);
    assert_eq!(this_host.boundary_hash, BOUNDARY_HASH, "the host's tree holds this boundary");
    this_host.write(&host).expect("its marker");
    assert_eq!(host_tree(&root), Some(host.clone()));
    assert_eq!(host_adapters(&root), built_in_adapters(), "its own, not the tree's two");
    write(
        &host,
        "crates/vike-rust-strategy/src/abi.rs",
        "// half unpacked
",
    );
    assert_eq!(host_tree(&root), None, "no verified installed SDK holds this boundary");
    assert_eq!(host_adapters(&root), AdapterSet::default());

    write(&adapter_of(3), ADAPTER_FILE, &marker_text(5));
    // One broken adapter costs its SDK alone: the other stays, and the broken one is said.
    let set = adapters(&host);
    assert_eq!(
        set.adapters.iter().map(|a| a.number).collect::<Vec<_>>(),
        [2],
        "SDK 2's still reads"
    );
    let why = set.broken.join("; ");
    assert!(why.contains("names SDK 5") && why.contains("SDK 3"), "{why}");
    write(&adapter_of(3), ADAPTER_FILE, &marker_text(3));
    fs::remove_file(adapter_of(2).join(ADAPTER_FACE_FILE)).expect("drop a face");
    let set = adapters(&host);
    assert_eq!(
        set.adapters.iter().map(|a| a.number).collect::<Vec<_>>(),
        [3],
        "SDK 3's reads again"
    );
    let why = set.broken.join("; ");
    assert!(why.contains(ADAPTER_FACE_FILE) && why.contains("SDK 2"), "{why}");
    let _ = fs::remove_dir_all(&root);

    let shipped = adapters(&checkout.join("../.."));
    assert!(shipped.broken.is_empty(), "this checkout's adapters all read: {:?}", shipped.broken);
    for adapter in &shipped.adapters {
        assert_ne!(adapter.boundary_hash, BOUNDARY_HASH, "an adapter adapts ANOTHER boundary");
    }
    // What this binary serves is what its crate's folder held when it was built (`build.rs`).
    let built_in = built_in_adapters();
    assert!(built_in.broken.is_empty(), "{:?}", built_in.broken);
    assert_eq!(built_in.adapters, shipped.adapters);
}
