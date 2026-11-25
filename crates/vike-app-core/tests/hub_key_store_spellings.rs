//! **The pin that the GUI signs with its DEVICE key and reads no shared hub key** (decision 0288 item 8, the
//! cutover).
//!
//! A hub admits a person by its access list alone, so no shipped binary reads a shared person key any more; a row
//! left in a `hub_key` table is dead. The desktop's two resolutions — the tradehub half
//! ([`vike_app_core::backend::registry::fill_hub_keys`], which `vike-desktop`'s `refresh_device_key` calls;
//! `crates/vike-ops/tests/settings_secrets/hub_key_store_gate/shell_rule.rs`'s `SHELL_SITES` holds the
//! shell's call) and the datahub half ([`vike_app_core::backend::registry::datahub_dial_keys`], which
//! the store, the chart seed, the venue catalog and the market-data session dial through) — are pinned here over
//! a scratch store: both carry this desktop's device key when the store holds one, and nothing else.
//!
//! Every path below is inside a private scratch directory. No real settings directory is read,
//! written or named, and no test here touches the process environment.

use std::fs;
use std::path::Path;

use vike_app_core::backend::registry;
use vike_app_core::data::datahub::dial::resolve_datahub_dial;
use vike_app_core::data::stored::chart_seed::ChartSeedDial;
use vike_hub_protocol::access::DeviceKey;
use vike_hub_protocol::auth::Scope;
use vike_utility::mutex::lock;

/// A PC's settings database, created the way a fresh box gets one, with no device key yet.
fn pc(dir: &Path) {
    vike_secrets::create_store(dir.to_str()).expect("create the empty store");
}

/// An UPGRADED PC's database: [`pc`] plus the retired shared pairs as rows of its `hub_key` table, each
/// with a value a dial would sign with if anything read it (decision 0288: the rows stay, dead). The
/// names come from `vike_venues::credential_keys::RETIRED_HUB_KEYS`, never spelled here.
fn upgraded_pc(dir: &Path) {
    pc(dir);
    let rows: Vec<(String, String)> = vike_venues::credential_keys::RETIRED_HUB_KEYS
        .iter()
        .map(|name| ((*name).to_string(), format!("shared-{name}")))
        .collect();
    vike_secrets::save_credentials_to_store_unjournalled(
        dir,
        vike_secrets::StoreTable::HubKey,
        &rows,
        vike_secrets::CredentialUpsertIntent::Add,
    )
    .expect("plant the retired pairs");
}

/// A device key stored in `dir`'s `device_key` table, as a `vike-cli backend connect` child stores it.
fn store_device(dir: &Path) -> DeviceKey {
    let (device, pkcs8) = DeviceKey::generate().expect("a key");
    vike_secrets::access::device_key_insert(dir, &device.key_id(), "owner", "pc", &pkcs8)
        .expect("store the device key");
    device
}

/// [`registry::fill_hub_keys`] refreshes a PROCESS cell (the device key the tradehub dials read), so the
/// tests that call it run one at a time.
static DEVICE_CELL: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// The scratch settings directory for one shape.
fn scratch() -> tempfile::TempDir {
    tempfile::Builder::new().prefix("vike-hub-key-spellings-").tempdir().expect("scratch dir")
}

/// **THE GATE, tradehub half.** An upgraded PC still holds the retired shared pairs in `hub_key`; with no
/// device key in the store a tradehub dial has nothing to sign with all the same ([`registry::device_key`]
/// is `None`, and the dials send nothing); once the store holds one, it is exactly what they sign with,
/// refreshed by the same call.
#[test]
fn the_gui_signs_with_its_device_key() {
    let _one_at_a_time = lock(&DEVICE_CELL);
    let stored = scratch();
    upgraded_pc(stored.path());
    let r = registry::fill_hub_keys(stored.path().to_str());
    assert!(r.notices.is_empty(), "{:?}", r.notices);
    assert!(registry::device_key().is_none(), "no device key stored yet");

    let device = store_device(stored.path());
    let r = registry::fill_hub_keys(stored.path().to_str());
    assert!(r.notices.is_empty(), "{:?}", r.notices);
    assert_eq!(registry::device_key().map(|d| d.key_id()), Some(device.key_id()));
}

/// **A key made OUTSIDE the desktop is signed with at the next dial.** [`registry::no_device_key`] sends the
/// operator to `vike-cli backend mint-missing` in a terminal; the cached key ([`registry::device_key`]) knows
/// nothing of a row another process wrote, and [`registry::fresh_device_key`] — what every dial about to be
/// made asks, the observe bridge's re-dial and the signers included — re-reads the store the shell last read.
#[test]
fn a_key_made_outside_the_desktop_is_read_at_the_next_dial() {
    let _one_at_a_time = lock(&DEVICE_CELL);
    let dir = scratch();
    pc(dir.path());
    registry::fill_hub_keys(dir.path().to_str());
    assert!(registry::fresh_device_key().is_none(), "no device key stored yet");

    let device = store_device(dir.path());
    assert!(registry::device_key().is_none(), "the cache has not read the new row yet");
    assert_eq!(registry::fresh_device_key().map(|d| d.key_id()), Some(device.key_id()));
    let signer = registry::observe_signer("127.0.0.1:9301").ok().map(|d| d.key_id());
    assert_eq!(signer, Some(device.key_id()));
    assert_eq!(
        registry::device_key().map(|d| d.key_id()),
        Some(device.key_id()),
        "and the cache follows"
    );
}

/// **Every gate up, this PC's device key held, and NO daemon at the address: read-only** (`connect_control`'s
/// `Err` arm, reached through the registry's device key). A dead daemon degrades the observer; it never errors
/// and never mounts control. Here rather than beside `connect_backend`'s other gates because it fills the
/// process's device-key cell, which the crate's unit tests never do.
#[test]
fn connect_backend_with_a_device_key_and_a_dead_daemon_stays_read_only() {
    use vike_app_core::backend::connection::connect_backend;
    let _one_at_a_time = lock(&DEVICE_CELL);
    let dir = scratch();
    pc(dir.path());
    store_device(dir.path());
    registry::fill_hub_keys(dir.path().to_str());
    assert!(registry::device_key().is_some(), "the key is held");
    let record = registry::BackendRecord {
        name: "armed".into(),
        address: vike_app_core::test_support::dead_address(),
        observe_key: String::new(),
        control_key: None,
        control: true,
    };
    let conn = connect_backend(
        &record,
        true,
        std::time::Duration::from_millis(50),
        vike_tradehub_client::liveness::LinkPatience::default(),
        std::sync::Arc::default(),
        |_| {},
        || {},
    );
    assert!(conn.control.is_none(), "a dead daemon degrades to read-only, never Some");
}

/// **Arriving at the Data Manager's Backend page re-reads the device key**, so its Providers and Server rows
/// stop saying [`registry::no_device_key`] once a terminal made the key; staying on the page reads nothing more.
#[test]
fn arriving_at_the_backend_page_reads_a_key_made_outside_the_desktop() {
    use vike_app_core::ui::windows::{DataManagerCredentialReads, DataManagerDestination};
    let _one_at_a_time = lock(&DEVICE_CELL);
    let dir = scratch();
    pc(dir.path());
    registry::fill_hub_keys(dir.path().to_str());
    let mut reads = DataManagerCredentialReads::default();
    let unread = || panic!("the Backend page reads no credential");
    let _ = reads.read(DataManagerDestination::Backend, 0, unread);
    assert!(registry::device_key().is_none(), "no device key stored yet");

    let device = store_device(dir.path());
    let _ = reads.read(DataManagerDestination::Backend, 0, unread);
    assert!(registry::device_key().is_none(), "still on the page: no second read");
    let _ = reads.read(DataManagerDestination::default(), 0, unread);
    let _ = reads.read(DataManagerDestination::Backend, 0, unread);
    let read = registry::device_key().map(|d| d.key_id());
    assert_eq!(read, Some(device.key_id()), "arriving again read it");
}

/// **THE GATE, datahub half.** Every datahub dial this crate resolves — the store's own resolution, the chart seed
/// and the venue catalog — carries the device key and NO shared key, whatever an upgraded PC still holds under
/// the retired pair names; with no device key there is nothing to sign with.
#[test]
fn the_datahub_dials_here_carry_the_device_key_and_never_the_pair() {
    let dir = scratch();
    upgraded_pc(dir.path());
    let settings = dir.path().to_str();

    let (keys, notices) = registry::datahub_dial_keys(settings);
    assert!(keys.is_none(), "no device key, nothing to sign with");
    assert!(notices.is_empty(), "{notices:?}");

    let device = store_device(dir.path());
    let keys = registry::datahub_dial_keys(settings).0.expect("the device key");
    assert_eq!(keys.device().map(DeviceKey::key_id), Some(device.key_id()));
    assert!(!keys.has(Scope::Read) && !keys.has(Scope::Write), "a shared key rode beside it");

    let seed_dial = ChartSeedDial::resolve(
        "127.0.0.1:1".to_string(),
        settings,
        std::sync::Arc::new(std::sync::Mutex::new(None)),
    );
    let catalog_dial = resolve_datahub_dial("127.0.0.1:1".to_string(), settings);
    for (dial, keys) in
        [("the chart seed", seed_dial.keys), ("the venue catalog", catalog_dial.keys)]
    {
        let keys = keys.unwrap_or_else(|| panic!("{dial}: the device key resolves"));
        assert_eq!(keys.device().map(DeviceKey::key_id), Some(device.key_id()), "{dial}");
        assert!(!keys.has(Scope::Read) && !keys.has(Scope::Write), "{dial} read a shared key");
    }
}

/// ⚠ **An unreadable store names the store that failed** rather than reading as "no key". Bytes that are
/// not a database where the database belongs: present, and never openable, on every platform.
#[test]
fn the_unreadable_store_notice_names_the_store() {
    let _one_at_a_time = lock(&DEVICE_CELL);
    let broken = scratch();
    fs::create_dir_all(broken.path().join("db")).expect("db dir");
    fs::write(broken.path().join("db").join("vike.db"), b"not a sqlite database, and not empty")
        .expect("plant");

    let r = registry::fill_hub_keys(broken.path().to_str());
    let said = r.notices.join("\n");
    assert!(said.contains("vike.db"), "the notice must name the store that failed: {said}");
    assert!(registry::device_key().is_none(), "an unreadable store holds no key to sign with");
}

/// **A read while this process MAKES its store reports nothing** (the owner's PC, 2026-11-16): the Server
/// group's first connect creates the store on its own thread, the frame thread reads the device key every
/// frame, and a read between the file's creation and its stamp found "schema version 0 … never
/// finished" about a store a millisecond from done. While the store is being made a read answers "no
/// key yet"; outside that window an unreadable store is reported exactly as before.
#[test]
fn a_read_while_the_store_is_being_made_reports_nothing() {
    let _one_at_a_time = lock(&DEVICE_CELL);
    let half_made = scratch();
    fs::create_dir_all(half_made.path().join("db")).expect("db dir");
    fs::write(half_made.path().join("db").join("vike.db"), b"not finished yet").expect("plant");
    let settings = half_made.path().to_str();

    let r = registry::while_making_the_store(|| registry::fill_hub_keys(settings));
    assert!(r.notices.is_empty(), "nothing to report while the store is made: {:?}", r.notices);
    assert!(registry::device_key().is_none(), "no key yet");
    let (keys, notices) =
        registry::while_making_the_store(|| registry::datahub_dial_keys(settings));
    assert!(keys.is_none() && notices.is_empty(), "the datahub dials read nothing either");

    let r = registry::fill_hub_keys(settings);
    assert!(!r.notices.is_empty(), "outside the making, an unreadable store is still reported");
}
