//! **A Studio's Build goes THROUGH the backtesthub** (decision 0288's amendment of 2026-11-16, the owner's
//! option A): `Request::BuildStrategyFile` is forwarded to the strategy builder this daemon alone dials, so no
//! person holds the builder's key. What is proved here, over a real socket:
//!
//! - a Control connection's Build answers the builder's sha, and a compile failure the builder's diagnostics
//!   VERBATIM (the result Studio showed when it dialled the builder itself);
//! - an Observe connection, a person whose ROLE is `observe` and a daemon with no hub keys cannot build, and
//!   the builder is never asked.
//!
//! Its own binary: the builder is a process-global (`strategy_sandbox::use_builder`, first call wins), so no
//! other test may share this process. The stand-in builder below is installed by every test, identically.

use std::net::{SocketAddr, TcpListener};
use std::sync::{Arc, Mutex};
use std::thread;

use vike_backtesthub::backtesthub_server::{StudyDirs, serve_authed};
use vike_backtesthub::named_run::NamedRunLane;
use vike_backtesthub_client::BacktesthubClient;
use vike_backtesthub_client::proto::{FEATURE_BUILD_STRATEGY_FILE, FEATURE_STRATEGY_SDK};
use vike_data::{HistoryStore, HistoryStoreMemory};
use vike_hub_protocol::access::{AccessEntry, AccessList, DeviceKey};
use vike_hub_protocol::auth::{HubKeys, Scope};
use vike_hub_protocol::hash::sha256_hex;
use vike_hub_protocol::limits::SERVER_IDLE_READ_TIMEOUT;

/// rustc's words for a source that does not compile, as the stand-in builder answers them.
const DIAGNOSTICS: &str = "error[E0425]: cannot find value `x` in this scope";

/// Every `(file, text)` the stand-in builder was asked to build.
static ASKED: Mutex<Vec<(String, String)>> = Mutex::new(Vec::new());

/// Every `(file, sdk)` the stand-in builder was asked to build: the SDK a Build named.
static ASKED_SDK: Mutex<Vec<(String, Option<u32>)>> = Mutex::new(Vec::new());

/// Install the stand-in builder (the same one from every test): a text holding `COMPILE_ERROR` is refused with
/// [`DIAGNOSTICS`], any other answers its sha256 and where its artifact would lie (what the real builder answers).
fn use_stand_in_builder() {
    vike_backtesthub::strategy_sandbox::use_builder(Box::new(|file, text, sdk| {
        ASKED.lock().unwrap().push((file.to_string(), text.to_string()));
        ASKED_SDK.lock().unwrap().push((file.to_string(), sdk));
        if text.contains("COMPILE_ERROR") {
            // As the builder's client hands it on: the builder's id beside the words.
            let id = Some("strategy.build.compile_failed".to_string());
            Err(vike_utility::errors::CarriedRefusal::new(id, DIAGNOSTICS.to_string()))
        } else {
            let sha256 = sha256_hex(text.as_bytes());
            // Where the real builder would answer it lies (`sdk-N/<folder>/<sha256>.so`).
            let artifact = format!("sdk-dev/00000000/{sha256}.so");
            Ok(vike_backtesthub::strategy_sandbox::BuiltFile { sha256, artifact })
        }
    }));
}

/// Whether the stand-in builder was ever asked to build `file`.
fn asked_for(file: &str) -> bool {
    ASKED.lock().unwrap().iter().any(|(f, _)| f == file)
}

/// A backtesthub over an empty store, keyed by `keys` (`None`: it authenticates nothing).
fn spawn_hub(keys: Option<HubKeys>) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
    let addr = listener.local_addr().expect("resolve assigned port");
    thread::spawn(move || {
        let store: Arc<dyn HistoryStore + Send + Sync> = Arc::new(HistoryStoreMemory::new());
        let _ = serve_authed(
            listener,
            store,
            StudyDirs::working_directory(),
            keys,
            NamedRunLane::DISARMED,
            SERVER_IDLE_READ_TIMEOUT,
        );
    });
    addr
}

/// A shared key pair built by hand (the tests' seam; no person holds one since decision 0288).
fn hub_keys() -> HubKeys {
    HubKeys::new(b"observe-build".to_vec(), b"control-build".to_vec())
}

#[test]
fn a_control_build_answers_the_builders_sha_and_its_diagnostics_verbatim() {
    use_stand_in_builder();
    let addr = spawn_hub(Some(hub_keys()));
    let mut client =
        BacktesthubClient::connect_authed(addr, &hub_keys(), Scope::Write).expect("Control");
    assert!(
        client.features().iter().any(|f| f == FEATURE_BUILD_STRATEGY_FILE),
        "{:?}",
        client.features()
    );

    let text = "pub fn build() {}\n";
    let sha = client.build_strategy_file("trend/control_build.rs", text, None).expect("built");
    assert_eq!(sha, sha256_hex(text.as_bytes()));
    assert!(
        ASKED.lock().unwrap().contains(&("trend/control_build.rs".to_string(), text.to_string())),
        "the builder is handed the file's path and the text as sent"
    );

    let err = client
        .build_strategy_file("trend/control_broken.rs", "COMPILE_ERROR", None)
        .expect_err("the compiler refused it");
    // What Studio and the CLI show as they get it: the builder's one id, then its diagnostics verbatim.
    let shown = format!("error [strategy.build.compile_failed]: {DIAGNOSTICS}");
    assert_eq!(err, shown, "the diagnostics come back verbatim, under the builder's one id");
    client.ping().expect("the connection survives a refused build");
}

/// **The SDK the Studio's picker names reaches the builder** (decision 0296, the owner's yes of 2026-11-19):
/// the daemon advertises `FEATURE_STRATEGY_SDK`, a Build naming SDK 5 is built against 5, and one naming
/// none against what the file says.
#[test]
fn a_builds_sdk_reaches_the_builder() {
    use_stand_in_builder();
    let addr = spawn_hub(Some(hub_keys()));
    let mut client =
        BacktesthubClient::connect_authed(addr, &hub_keys(), Scope::Write).expect("Control");
    assert!(client.features().iter().any(|f| f == FEATURE_STRATEGY_SDK), "{:?}", client.features());
    let text = "pub fn build() {}\n";
    client.build_strategy_file("trend/pinned_build.rs", text, Some(5)).expect("built");
    client.build_strategy_file("trend/unpinned_build.rs", text, None).expect("built");
    let asked = ASKED_SDK.lock().unwrap().clone();
    assert!(asked.contains(&("trend/pinned_build.rs".to_string(), Some(5))), "{asked:?}");
    assert!(asked.contains(&("trend/unpinned_build.rs".to_string(), None)), "{asked:?}");
}

#[test]
fn an_observe_connection_and_a_keyless_daemon_cannot_build() {
    use_stand_in_builder();
    let mut observe =
        BacktesthubClient::connect_authed(spawn_hub(Some(hub_keys())), &hub_keys(), Scope::Read)
            .expect("Observe");
    let err = observe
        .build_strategy_file("trend/observe.rs", "pub fn build() {}\n", None)
        .expect_err("Observe");
    assert!(err.contains("BuildStrategyFile requires the Control scope"), "{err}");

    let mut anyone = BacktesthubClient::connect(spawn_hub(None)).expect("connect");
    let err = anyone
        .build_strategy_file("trend/keyless.rs", "pub fn build() {}\n", None)
        .expect_err("keyless");
    assert!(err.contains("has no hub keys"), "{err}");

    assert!(
        !asked_for("trend/observe.rs") && !asked_for("trend/keyless.rs"),
        "the builder was asked"
    );
}

/// A person signs with their own device key (decision 0288): `observe` cannot even authenticate the Control
/// scope, and on its Observe connection the Build is refused; `control` builds.
#[test]
fn an_observe_role_person_cannot_build_and_a_control_role_person_can() {
    use_stand_in_builder();
    for (role, file) in
        [("observe", "trend/observe_person.rs"), ("control", "trend/control_person.rs")]
    {
        let (device, _) = DeviceKey::generate().expect("a device key");
        let row = AccessEntry {
            person: "alice".into(),
            device: "macbook".into(),
            role: role.into(),
            public_key: device.public_key().to_vec(),
            revoked: false,
        };
        let key_id = device.key_id();
        let list = AccessList::new(move |asked| (asked == key_id).then(|| row.clone()));
        let addr = spawn_hub(Some(hub_keys().with_access(list)));
        let person = HubKeys::of_device(device);

        let control = BacktesthubClient::connect_authed(addr, &person, Scope::Write);
        if role == "observe" {
            let err = control.expect_err("an observe role is not admitted at Control");
            assert!(err.to_string().contains("access denied"), "{err}");
            let mut client =
                BacktesthubClient::connect_authed(addr, &person, Scope::Read).expect("Observe");
            let err =
                client.build_strategy_file(file, "pub fn build() {}\n", None).expect_err("observe");
            assert!(err.contains("BuildStrategyFile requires the Control scope"), "{err}");
            assert!(!asked_for(file), "the builder was asked for an observe person's build");
        } else {
            let sha = control
                .expect("a control role is admitted")
                .build_strategy_file(file, "pub fn build() {}\n", None)
                .expect("built");
            assert_eq!(sha, sha256_hex(b"pub fn build() {}\n"));
            assert!(asked_for(file));
        }
    }
}
