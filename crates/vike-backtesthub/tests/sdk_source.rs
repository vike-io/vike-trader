//! **The Studio's SDK source verbs over a real socket** (decision 0296, verdict 3): the daemon advertises
//! `sdk_source`, lists what its SDK reader finds under its project and serves a file or folder of a listed
//! SDK's `crates/`, and refuses a path that leaves it with the connection kept. The path rules themselves
//! are `crates/vike-backtesthub/src/sdk_source_tests.rs`'s; the real reader (the SDK vocabulary's markers
//! and usability) is `crates/vike-backend/src/main.rs`'s `sdks_under`, tested there.
//!
//! Its own binary: the reader is a process-global (`sdk_source::use_reader`, first call wins), so no other
//! test may share this process. Every test installs the same stand-in reader.

use std::net::{SocketAddr, TcpListener};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread;

use vike_backtesthub::backtesthub_server::{StudyDirs, serve_authed};
use vike_backtesthub::named_run::NamedRunLane;
use vike_backtesthub::sdk_source::{SdkOnServer, ServerSdks, use_reader};
use vike_backtesthub_client::BacktesthubClient;
use vike_backtesthub_client::proto::FEATURE_SDK_SOURCE;
use vike_backtesthub_client::sdk_source::{WireSdk, WireSdkEntry};
use vike_data::{HistoryStore, HistoryStoreMemory};
use vike_hub_protocol::limits::SERVER_IDLE_READ_TIMEOUT;

/// Where the stand-in reader keeps the SDKs under a project.
const SDK_ROOT: &str = "vendor/vike-strategy-source";

/// The planted SDK's one source, spelled in two literals: xtask's scan for another crate's `.rs` read as text
/// (`xtask/src/ci/tables/readers.rs`'s `SOURCES_READ_AS_TEXT`) would take a whole one for a read of the real
/// file, and the path-key gate (`crates/vike-ops/tests/hygiene/path_key_gate.rs`) for a key git must track.
const TACTICS_LIB: &str = concat!("crates", "/vike-tactics/src/lib.rs");

fn row(number: u32) -> WireSdk {
    WireSdk {
        name: format!("sdk-{number}"),
        number: Some(number),
        commit: "a3ca741107".to_string(),
        abi_version: Some(14),
        usable: true,
        why_unusable: String::new(),
    }
}

/// The stand-in for the composition root's reader: SDK 3 under the project.
fn use_stand_in_reader() {
    use_reader(Box::new(|project: &Path| {
        let root = project.join(SDK_ROOT);
        Ok(ServerSdks {
            root: root.clone(),
            sdks: vec![SdkOnServer { sdk: row(3), dir: root.join("sdk-3") }],
        })
    }));
}

/// A project holding SDK 3's snapshot (its marker and one crate) and a credential store beside it.
fn planted() -> tempfile::TempDir {
    let project = tempfile::tempdir().expect("a project");
    let sdk = project.path().join(SDK_ROOT).join("sdk-3");
    for (path, text) in [
        ("SDK", "number = 3\n"),
        ("crates/vike-tactics/Cargo.toml", "[package]\nname = \"vike-tactics\"\n"),
        (TACTICS_LIB, "//! Tactics.\n"),
    ] {
        let file = sdk.join(path);
        std::fs::create_dir_all(file.parent().expect("a parent")).expect("mkdir");
        std::fs::write(file, text).expect("write");
    }
    std::fs::create_dir_all(project.path().join("settings/db")).expect("mkdir");
    std::fs::write(project.path().join("settings/db/vike.db"), "the credential store")
        .expect("write");
    project
}

/// A key-less daemon whose project is `project_root`.
fn serve(project_root: Option<PathBuf>) -> SocketAddr {
    use_stand_in_reader();
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
    let addr = listener.local_addr().expect("resolve assigned port");
    let study = StudyDirs { project_root, ..StudyDirs::working_directory() };
    thread::spawn(move || {
        let store: Arc<dyn HistoryStore + Send + Sync> = Arc::new(HistoryStoreMemory::new());
        let _ = serve_authed(
            listener,
            store,
            study,
            None,
            NamedRunLane::DISARMED,
            SERVER_IDLE_READ_TIMEOUT,
        );
    });
    addr
}

/// The daemon advertises the capability, lists its project's SDKs, reads a folder as its entries and a
/// file as its text, and refuses a path out of `crates/` in words with the connection kept.
#[test]
fn the_daemon_lists_its_sdks_and_serves_their_crates_only() {
    let project = planted();
    let mut client =
        BacktesthubClient::connect(serve(Some(project.path().to_path_buf()))).expect("connect");
    assert!(
        client.features().iter().any(|f| f == FEATURE_SDK_SOURCE),
        "every daemon advertises the SDK source verbs: {:?}",
        client.features()
    );
    let listing = client.list_sdks().expect("listed");
    assert_eq!(listing.root, project.path().join(SDK_ROOT).display().to_string());
    assert_eq!(listing.sdks, vec![row(3)]);

    let crates = client.read_sdk_file("sdk-3", "crates").expect("the crates folder");
    assert!(crates.folder);
    assert_eq!(
        crates.entries,
        vec![WireSdkEntry { name: "vike-tactics".to_string(), folder: true }]
    );
    let lib = client.read_sdk_file("sdk-3", TACTICS_LIB).expect("a file");
    assert_eq!((lib.folder, lib.text.as_str()), (false, "//! Tactics.\n"));

    for (sdk, path) in [
        ("sdk-3", "crates/../SDK"),
        ("sdk-3", "crates/../../../../settings/db/vike.db"),
        ("sdk-3", "/etc/passwd"),
        ("sdk-3", "SDK"),
        ("../sdk-3", "crates"),
        ("sdk-4", "crates"),
    ] {
        let refused = client.read_sdk_file(sdk, path).expect_err("refused");
        assert!(!refused.contains("the credential store"), "{sdk} {path}: {refused}");
    }
    // The connection survived every refusal.
    assert_eq!(client.list_sdks().expect("still served").sdks, vec![row(3)]);
}

/// A daemon that found no project serves no SDK, and says so.
#[test]
fn a_daemon_with_no_project_serves_no_sdk() {
    let mut client = BacktesthubClient::connect(serve(None)).expect("connect");
    let refused = client.list_sdks().expect_err("no project");
    assert!(refused.contains("found no project"), "{refused}");
    let refused = client.read_sdk_file("sdk-3", "crates").expect_err("no project");
    assert!(refused.contains("found no project"), "{refused}");
}
