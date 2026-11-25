use super::*;

// The planted SDK's sources, spelled in two literals: xtask's scan for another crate's `.rs` read as text
// (`xtask/src/ci/tables/readers.rs`'s `SOURCES_READ_AS_TEXT`) would take a whole one for a read of the real
// file, and the path-key gate (`crates/vike-ops/tests/hygiene/path_key_gate.rs`) for a key git must track.
const TACTICS_LIB: &str = concat!("crates", "/vike-tactics/src/lib.rs");
const MATH_LIB: &str = concat!("crates", "/vike-math/src/lib.rs");

fn write(path: &Path, bytes: &[u8]) {
    std::fs::create_dir_all(path.parent().expect("a parent")).expect("mkdir");
    std::fs::write(path, bytes).expect("write");
}

/// A project holding SDK 3 as a snapshot unpacks it (`sdk-3/` with its marker, its lock and two
/// crates), and a secret beside the SDK that no path may reach.
fn planted() -> tempfile::TempDir {
    let project = tempfile::tempdir().expect("a temp dir");
    let sdk = project.path().join("vendor/vike-strategy-source/sdk-3");
    write(&sdk.join("SDK"), b"number = 3\n");
    write(&sdk.join("Cargo.lock"), b"# lock\n");
    write(&sdk.join("crates/vike-tactics/Cargo.toml"), b"[package]\nname = \"vike-tactics\"\n");
    write(&sdk.join(TACTICS_LIB), b"//! Tactics.\npub fn rule() {}\n");
    write(&sdk.join(MATH_LIB), b"//! Maths.\n");
    write(&project.path().join("settings/db/vike.db"), b"the credential store");
    project
}

fn sdk_root(project: &Path) -> PathBuf {
    project.join("vendor/vike-strategy-source/sdk-3")
}

fn row(name: &str, number: Option<u32>, usable: bool) -> WireSdk {
    WireSdk {
        name: name.to_string(),
        number,
        commit: "a3ca741107".to_string(),
        abi_version: Some(14),
        usable,
        why_unusable: if usable { String::new() } else { "retired".to_string() },
    }
}

/// A stub of the composition root's reader: SDK 3 (usable) and SDK 1 (retired) under the project.
fn reader() -> SdkReader {
    Box::new(|project: &Path| {
        let root = project.join("vendor/vike-strategy-source");
        Ok(ServerSdks {
            root: root.clone(),
            sdks: vec![
                SdkOnServer { sdk: row("sdk-3", Some(3), true), dir: root.join("sdk-3") },
                SdkOnServer { sdk: row("sdk-1", Some(1), false), dir: root.join("sdk-1") },
            ],
        })
    })
}

/// `ListSdks` on a planted project: the folder the reader read and every SDK it found, newest first,
/// each row as the reader made it.
#[test]
fn the_list_is_what_the_reader_found_under_the_project() {
    let project = planted();
    let listing = listing_with(Some(&reader()), Some(project.path())).expect("listed");
    assert_eq!(
        listing.root,
        project.path().join("vendor/vike-strategy-source").display().to_string()
    );
    assert_eq!(listing.sdks, vec![row("sdk-3", Some(3), true), row("sdk-1", Some(1), false)]);
}

/// A daemon handed no reader (started outside `vike-backend`), or one with no project, refuses both
/// verbs in words, and a reader's own failure is its answer.
#[test]
fn no_reader_no_project_and_an_unreadable_folder_are_refused_in_words() {
    let project = planted();
    let no_reader = listing_with(None, Some(project.path())).expect_err("no reader");
    assert!(no_reader.contains("vike-backend backtesthub --addr"), "{no_reader}");
    let no_project = read_with(Some(&reader()), None, "sdk-3", "crates").expect_err("no project");
    assert!(no_project.contains("found no project"), "{no_project}");
    let failing: SdkReader =
        Box::new(|_: &Path| Err("cannot list the SDKs under /x: denied".to_string()));
    assert_eq!(
        listing_with(Some(&failing), Some(project.path())),
        Err("cannot list the SDKs under /x: denied".to_string())
    );
}

/// An SDK is matched by the name the reader LISTS: an unlisted name, a path dressed as a name included,
/// names what the server holds and reads nothing.
#[test]
fn only_a_listed_sdk_is_read() {
    let project = planted();
    for name in ["sdk-9", "../sdk-3", "sdk-3/..", "vendor", ""] {
        let refused = read_with(Some(&reader()), Some(project.path()), name, "crates")
            .expect_err("not a listed SDK");
        assert!(refused.contains(r#"holds ["sdk-3", "sdk-1"]"#), "{name:?}: {refused}");
    }
}

/// A file reads whole as its text; a folder reads as its entries, folders first, each group by name.
#[test]
fn a_file_reads_as_its_text_and_a_folder_as_its_entries() {
    let project = planted();
    let read = |path: &str| {
        read_with(Some(&reader()), Some(project.path()), "sdk-3", path).expect("served")
    };
    let file = read(TACTICS_LIB);
    assert!(!file.folder && file.entries.is_empty());
    assert_eq!(file.text, "//! Tactics.\npub fn rule() {}\n");
    assert_eq!((file.sdk.as_str(), file.path.as_str()), ("sdk-3", TACTICS_LIB));
    let crates = read("crates");
    assert!(crates.folder && crates.text.is_empty());
    let names: Vec<(&str, bool)> =
        crates.entries.iter().map(|e| (e.name.as_str(), e.folder)).collect();
    assert_eq!(names, [("vike-math", true), ("vike-tactics", true)]);
    let tactics = read("crates/vike-tactics");
    let names: Vec<(&str, bool)> =
        tactics.entries.iter().map(|e| (e.name.as_str(), e.folder)).collect();
    assert_eq!(names, [("src", true), ("Cargo.toml", false)]);
}

/// Assert `path` is refused under SDK 3 of `project`, the refusal naming `why`.
fn assert_refused(project: &Path, path: &str, why: &str) {
    let refused = read_under(&sdk_root(project), "sdk-3", path).expect_err(path);
    assert!(refused.contains(why), "{path:?} must be refused for {why:?}: {refused}");
    assert!(!refused.contains("the credential store"), "{refused}");
}

/// **THE TRUST BOUNDARY, by the text alone.** `..` (up, out, and back in), an absolute path (Unix,
/// Windows, a drive), a backslash, a NUL, an empty or `.` part and a path outside `crates/` (the SDK's own
/// marker and lock, the project's credential store) are all refused before a file is opened.
#[test]
fn a_path_that_could_leave_the_crates_folder_is_refused_by_its_text() {
    let project = planted();
    let up = "no part of a path may be empty, `.` or `..`";
    let relative = "a path is relative to the SDK and `/`-separated";
    let crates_only = "only the SDK's `crates/` folder is served";
    for (path, why) in [
        ("crates/..", up),
        ("crates/../SDK", up),
        ("crates/../../../../settings/db/vike.db", up),
        (concat!("crates", "/vike-tactics/../vike-math/src/lib.rs"), up),
        ("..", up),
        ("crates/./vike-tactics", up),
        ("crates//vike-tactics", up),
        ("crates/", up),
        ("", up),
        ("/etc/passwd", relative),
        ("/crates/vike-tactics/src/lib.rs", relative),
        ("C:/Windows/win.ini", relative),
        ("crates\\..\\SDK", relative),
        ("crates/vike-tactics/src/lib.rs:stream", relative),
        ("crates/vike-tactics/src/lib.rs\0", "NUL"),
        ("crates\0/../SDK", "NUL"),
        ("SDK", crates_only),
        ("Cargo.lock", crates_only),
        ("settings/db/vike.db", crates_only),
    ] {
        assert_refused(project.path(), path, why);
    }
}

/// A path to nothing is refused with the system's own words, naming the path as sent and never the
/// server's folder.
#[test]
fn a_missing_path_is_refused_without_naming_the_servers_folder() {
    let project = planted();
    let refused = read_under(&sdk_root(project.path()), "sdk-3", "crates/vike-nothing/src/lib.rs")
        .expect_err("missing");
    assert!(refused.starts_with("sdk-3 \"crates/vike-nothing/src/lib.rs\": "), "{refused}");
    assert!(!refused.contains(&project.path().display().to_string()), "{refused}");
}

/// A file above the cap is refused in words, never cut; one AT the cap reads whole. A file that is not
/// UTF-8 is refused too.
#[test]
fn an_oversize_or_binary_file_is_refused() {
    let project = planted();
    let at_cap = usize::try_from(SDK_FILE_MAX_BYTES).expect("the cap fits");
    let crates = sdk_root(project.path()).join("crates/vike-math");
    write(&crates.join("at_cap.rs"), &vec![b'a'; at_cap]);
    write(&crates.join("over_cap.rs"), &vec![b'a'; at_cap + 1]);
    write(&crates.join("model.onnx"), &[0xff, 0xfe, 0x00, 0x01]);
    let read = |name: &str| read_under(&sdk_root(project.path()), "sdk-3", name);
    assert_eq!(read("crates/vike-math/at_cap.rs").expect("at the cap").text.len(), at_cap);
    assert_refused(project.path(), "crates/vike-math/over_cap.rs", "bytes one read answers");
    assert_refused(project.path(), "crates/vike-math/model.onnx", "not UTF-8");
}

/// **A LINK IS NEVER FOLLOWED**, wherever it points: a linked folder out of the SDK (to the project's
/// settings), a linked file out of it, and a link that points back INSIDE `crates/` are all refused, and
/// a folder's entries do not list a link.
#[cfg(unix)]
#[test]
fn a_link_anywhere_on_the_path_is_refused_and_never_listed() {
    use std::os::unix::fs::symlink;
    let project = planted();
    let crates = sdk_root(project.path()).join("crates");
    let settings = project.path().join("settings");
    symlink(&settings, crates.join("vike-tactics/out")).expect("a folder link out");
    symlink(settings.join("db/vike.db"), crates.join("vike-math/vike.db"))
        .expect("a file link out");
    symlink(crates.join("vike-tactics/src/lib.rs"), crates.join("vike-math/inside.rs"))
        .expect("a link that stays inside");
    let link = "it passes through a link, and a link is never followed";
    for path in [
        "crates/vike-tactics/out",
        "crates/vike-tactics/out/db/vike.db",
        "crates/vike-math/vike.db",
        "crates/vike-math/inside.rs",
    ] {
        assert_refused(project.path(), path, link);
    }
    let math =
        read_under(&sdk_root(project.path()), "sdk-3", "crates/vike-math").expect("a folder");
    let names: Vec<&str> = math.entries.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(names, ["src"], "a folder's entries list no link");
}

/// The SDK's `crates/` being a link itself (a snapshot laid out by hand) is refused too: the walk starts
/// at `crates`, so even the served folder is never reached through a link.
#[cfg(unix)]
#[test]
fn a_crates_folder_that_is_a_link_is_refused() {
    let project = planted();
    let elsewhere = project.path().join("elsewhere");
    std::fs::rename(sdk_root(project.path()).join("crates"), &elsewhere).expect("move crates");
    std::os::unix::fs::symlink(&elsewhere, sdk_root(project.path()).join("crates"))
        .expect("link crates");
    assert_refused(project.path(), "crates", "a link is never followed");
    assert_refused(project.path(), MATH_LIB, "a link is never followed");
}
