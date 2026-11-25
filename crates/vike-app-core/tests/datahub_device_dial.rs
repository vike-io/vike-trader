//! **Every desktop datahub dial signed by this PC's device key is admitted — also a dial set up BEFORE
//! the key existed** (the CI box, 2026-11-16: the Server group made the key mid-session, the tradehub admitted
//! the desktop, and the datahub refused its market-data dial every 1-60 s because that dial had resolved
//! `None` at start and never again).
//!
//! The door is the datahub's own handshake: `vike_hub_protocol::handshake::run_keyed_handshake` under
//! `DATAHUB_DOMAIN` and `Plane::Data`, no shared key and an access list holding this PC's device as
//! `admin` — what `vike-datahub serve` builds (`crates/vike-datahub/src/datahub_cli.rs`). It counts the
//! connections it admitted and the ones it refused.

use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use vike_app_core::backend::registry;
use vike_app_core::data::catalog_refresh::DatahubCatalogRefresh;
use vike_app_core::data::data_sink::{L2BookStore, TickStore};
use vike_app_core::data::datahub::dial::resolve_datahub_dial;
use vike_app_core::data::market_data_session;
use vike_app_core::data::stored::chart_seed::ChartSeedDial;
use vike_data::HistoryStore as _;
use vike_datahub_client::proto::{Request, Response, request_kind};
use vike_datahub_client::{DatahubClient, PROTO_VERSION};
use vike_hub_protocol::access::{AccessEntry, AccessList, DeviceKey};
use vike_hub_protocol::auth::{DATAHUB_DOMAIN, FEATURE_AUTH, HubKeys, Scope};
use vike_hub_protocol::frame::{MAX_FRAME_LEN, read_frame, write_frame};
use vike_hub_protocol::handshake::{HandshakeOutcome, HandshakeSpec, run_keyed_handshake};
use vike_hub_protocol::plane::{DATA_PLANE_SENTINEL, Plane};
use vike_utility::mutex::lock;

/// The device key's epoch and cell are process-wide, so these tests run one at a time.
static ONE_AT_A_TIME: Mutex<()> = Mutex::new(());

/// A keyed datahub door admitting `device` and nobody else.
struct Door {
    address: String,
    admitted: Arc<AtomicUsize>,
    refused: Arc<AtomicUsize>,
}

impl Door {
    fn admitting(device: &DeviceKey) -> Self {
        let entry = AccessEntry {
            person: "owner".into(),
            device: "pc".into(),
            role: "admin".into(),
            public_key: device.public_key().to_vec(),
            revoked: false,
        };
        let id = device.key_id();
        let list = AccessList::new(move |key_id| (key_id == id).then(|| entry.clone()));
        let keys = HubKeys::new(Vec::new(), Vec::new()).with_access(list);
        let listener = TcpListener::bind("127.0.0.1:0").expect("a port");
        let address = listener.local_addr().expect("its address").to_string();
        let (admitted, refused) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
        let (a, r) = (admitted.clone(), refused.clone());
        std::thread::Builder::new()
            .name("datahub-door".into())
            .spawn(move || {
                for stream in listener.incoming().map_while(Result::ok) {
                    let (keys, a, r) = (keys.clone(), a.clone(), r.clone());
                    let _ = std::thread::Builder::new()
                        .name("datahub-door-conn".into())
                        .spawn(move || serve(stream, &keys, &a, &r));
                }
            })
            .expect("the door");
        Self { address, admitted, refused }
    }

    fn admitted(&self) -> usize {
        self.admitted.load(Ordering::SeqCst)
    }

    fn refused(&self) -> usize {
        self.refused.load(Ordering::SeqCst)
    }
}

/// One connection: the datahub's handshake, then a `Pong` for a `Ping` and an error for anything else.
fn serve(mut stream: TcpStream, keys: &HubKeys, admitted: &AtomicUsize, refused: &AtomicUsize) {
    let features = vec![FEATURE_AUTH.to_string(), DATA_PLANE_SENTINEL.to_string()];
    let spec = HandshakeSpec {
        domain: DATAHUB_DOMAIN,
        proto_version: PROTO_VERSION,
        plane: Some(Plane::Data),
        features: &features,
        deadline: Duration::from_secs(5),
        reply_ceiling: MAX_FRAME_LEN,
    };
    let outcome = run_keyed_handshake::<Request>(
        &mut stream,
        keys,
        spec,
        |_| Ok(()),
        None,
        request_kind,
        |_| {},
    );
    let HandshakeOutcome::Authed { person: Some(_), .. } = outcome else {
        refused.fetch_add(1, Ordering::SeqCst);
        return;
    };
    admitted.fetch_add(1, Ordering::SeqCst);
    while let Ok(request) = read_frame::<_, Request>(&mut stream) {
        let answer = match request {
            Request::Ping => Response::Pong,
            _ => Response::Error("this door serves no verb".to_string()),
        };
        if write_frame(&mut stream, &answer).is_err() {
            return;
        }
    }
}

/// A PC's settings database, with no device key yet.
fn pc() -> tempfile::TempDir {
    let dir = tempfile::Builder::new().prefix("vike-datahub-dial-").tempdir().expect("scratch");
    vike_secrets::create_store(dir.path().to_str()).expect("the PC's store");
    dir
}

/// This PC's device key, made in its store as the Server group's connect makes it.
fn make_device_key(dir: &Path) -> DeviceKey {
    let (device, pkcs8) = DeviceKey::generate().expect("a key");
    vike_secrets::access::device_key_insert(dir, &device.key_id(), "owner", "pc", &pkcs8)
        .expect("store the device key");
    device
}

/// What the desktop does before a tradehub dial: refresh the device key cell.
fn refresh_device_key(dir: &Path) {
    registry::fill_hub_keys(dir.to_str());
}

fn until(what: &str, done: impl Fn() -> bool) {
    let by = Instant::now() + Duration::from_secs(30);
    while !done() {
        assert!(Instant::now() < by, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Dial `keys` the way every desktop datahub client dials, and say whether the door let it in.
fn admits(door: &Door, keys: Option<&HubKeys>) -> bool {
    DatahubClient::connect_with_keys(&door.address, keys, Scope::Read)
        .is_ok_and(|c| c.authenticated_scope() == Some(Scope::Read))
}

/// With the key held, every desktop datahub path dials signed and is admitted: the venue catalog (and the
/// DataSets, the options and the tool feeds, which dial its `keys`), the chart seed, the store (the Stored
/// tab's load and the chart's bars), and the market-data session (prices and depth).
#[test]
fn every_desktop_datahub_path_is_admitted_with_the_device_key() {
    let _one = lock(&ONE_AT_A_TIME);
    let dir = pc();
    let device = make_device_key(dir.path());
    refresh_device_key(dir.path());
    let door = Door::admitting(&device);
    let settings = dir.path().to_str();

    let catalog = resolve_datahub_dial(door.address.clone(), settings);
    assert!(admits(&door, catalog.keys.as_ref()), "the venue catalog");
    let note = Arc::new(Mutex::new(None));
    let seed = ChartSeedDial::resolve(door.address.clone(), settings, note);
    assert!(admits(&door, seed.keys.as_ref()), "the chart seed");

    let before = door.admitted();
    let store = registry::open_history_store_datahub(door.address.clone(), settings);
    let _ = store.list_series();
    assert!(door.admitted() > before, "the store (the Stored tab, the chart's bars)");

    let before = door.admitted();
    let mount = market_data_session::build(
        settings,
        Arc::new(L2BookStore::default()),
        Arc::new(TickStore::default()),
        || {},
    );
    mount.session.set_address(Some(&door.address));
    let session = mount.session.clone();
    until("the market-data session's signed probe", || session.capabilities().is_some());
    assert!(door.admitted() > before, "the market-data session");
    assert_eq!(door.refused(), 0, "nothing was refused");
}

/// **The the CI box bug.** The market-data session and the venue catalog are set up while this PC holds NO
/// device key (the desktop started before its first connect): they dial key-less and the door refuses
/// them. The connect then makes the key; once the desktop has read it, both dial with it and are admitted,
/// with no restart and no change of address.
#[test]
fn a_dial_set_up_before_the_device_key_existed_signs_with_it_once_it_does() {
    let _one = lock(&ONE_AT_A_TIME);
    let dir = pc();
    refresh_device_key(dir.path());
    let (device, pkcs8) = DeviceKey::generate().expect("the key the connect will store");
    let door = Door::admitting(&device);
    let settings = dir.path().to_str();

    let mount = market_data_session::build(
        settings,
        Arc::new(L2BookStore::default()),
        Arc::new(TickStore::default()),
        || {},
    );
    mount.session.set_address(Some(&door.address));
    let (tx, _rx) = std::sync::mpsc::channel();
    let catalog = DatahubCatalogRefresh::new(Vec::new(), tx);
    catalog.set_dial(Some(resolve_datahub_dial(door.address.clone(), settings)));
    until("the key-less dials to be refused", || door.refused() >= 1);
    assert_eq!(door.admitted(), 0, "nothing signed yet: there is no key");
    assert!(!catalog.dial_is_stale(Some(&door.address)), "nothing has moved yet");

    // The Server group's connect makes the key; the desktop's next tradehub dial reads it.
    vike_secrets::access::device_key_insert(dir.path(), &device.key_id(), "owner", "pc", &pkcs8)
        .expect("store the device key");
    refresh_device_key(dir.path());

    // The shell's per-frame call wakes the session; it re-resolves and dials signed.
    mount.session.follow_device_key();
    let session = mount.session.clone();
    until("the market-data session to dial with the key", || session.capabilities().is_some());
    assert!(door.admitted() >= 1, "the market-data session is admitted");

    assert!(catalog.dial_is_stale(Some(&door.address)), "the catalog's key-less dial is stale");
    let fresh = resolve_datahub_dial(door.address.clone(), settings);
    assert!(admits(&door, fresh.keys.as_ref()), "the venue catalog is admitted");
    catalog.set_dial(Some(fresh));
    assert!(!catalog.dial_is_stale(Some(&door.address)), "and the fresh dial holds");
}
