//! Backend registry — which remote tradehub daemons the GUI knows how to reach (split-plane
//! B7/B8/B9): the record model, the per-backend control-arming gate, and `backends.json`
//! persistence beside the workspace file.
//!
//! One [`BackendRecord`] per backend: a display name, the daemon's dial address and its arming —
//! **never key material, and no key name either**: every dial of this desktop signs with its own
//! device key ([`device_key`], decision 0288) and the server's access list decides what that device
//! may do; a desktop that holds none sends nothing ([`no_device_key`]). This module obeys the
//! settings-registry rule (libraries take configuration as parameters): it reads NO environment
//! variable itself — directory resolution delegates to
//! `crates/vike-app-core/src/ui/window/workspace/mod.rs` — its `base_dir`, the workspace family's one
//! resolver, whose env reads already carry their `vike_config::settings::SETTINGS` rows — and the
//! only file it touches is `backends.json` itself. **Key material must never land in this GUI-owned
//! JSON file** (spec B7): the credential store (the settings database) is the ONE home for secrets,
//! and a value stored here would be a second, ungated copy of a live key that no redaction
//! discipline covers.
//!
//! **The arming gate (B9):** [`BackendRecord::control`] is the per-backend write-channel gate.
//! A record with `control = false` never arms a control (order-writing) channel and
//! [`control_signer`] refuses it, so disarming a backend in the file disarms it everywhere
//! downstream.
//!
//! **Why this is deliberately NOT a settings row (B8):** settings rows are OPERATOR-owned —
//! validated on write, unknown keys refused by name, every key gated by `vike_config::CONSUMPTION`.
//! This file is GUI-owned: the app WRITES it (add / remove / select a backend from the UI),
//! exactly like `workspace.json`, so it takes the same route — the workspace-persistence idiom
//! (one `base_dir`, sanitized names, never-brick loads) and NOT the settings pipeline. It also must
//! not reuse `config.tradehub_addr`: that key is the DAEMON's bind address, not a client-side dial
//! list.
//!
//! **An old file is REFUSED, never read with empty fields** (the owner, 2026-11-17; decision 0117's
//! amendment of 2026-11-18). Both types deny unknown fields, so a file an older build wrote (it says
//! `address`) is not read: [`load`] answers an empty registry whose [`BackendRegistry::refused`] holds a
//! one-line hint naming the file, the Backend screen shows it, and `save_to` writes nothing over
//! it until it is moved away. An absent field still defaults (an unarmed, empty record).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use vike_client::resolved::DeviceKeyFault;
use vike_hub_protocol::access::DeviceKey;
use vike_hub_protocol::call::HubCallError;

use crate::ui::window::workspace::sanitize_layout_name;
use vike_utility::mutex::lock;

/// One remote backend daemon the GUI can dial.
///
/// Never key material — the module doc carries the argument. Every field is `#[serde(default)]`;
/// an unknown key (an older build's `address`) refuses the whole file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BackendRecord {
    /// Display name — user-entered, sanitized on save by `sanitize_layout_name` (the workspace
    /// layouts' sanitizer: one authority, not a second copy) so it can never
    /// smuggle path separators into anything derived from it.
    #[serde(default)]
    pub name: String,
    /// The daemon's dial address (`host:port`). NOT `config.tradehub_addr` — that settings key
    /// is the daemon's own BIND address; this is the client-side inverse.
    #[serde(default)]
    pub address: String,
    /// RETIRED: the shared observe key's name, read by nothing since decision 0288 (this desktop
    /// signs with its device key). Kept only so an existing `backends.json` that names one still
    /// loads (the file denies unknown fields).
    #[serde(default)]
    pub observe_key: String,
    /// RETIRED: the shared control key's name, read by nothing since decision 0288; kept for the
    /// same reason as [`Self::observe_key`].
    #[serde(default)]
    pub control_key: Option<String>,
    /// The per-backend arming gate (B9): `false` ⇒ no write channel to this backend ever arms
    /// ([`control_signer`] refuses it).
    #[serde(default)]
    pub control: bool,
}

/// The whole `backends.json`: the records plus which one is active.
///
/// Struct-level [`Default`] + per-field `#[serde(default)]` make an EMPTY or PARTIAL file load
/// as "no backends yet" rather than an error; an unknown key refuses the file (`load_from`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BackendRegistry {
    /// The known backends, in display order.
    #[serde(default)]
    pub backends: Vec<BackendRecord>,
    /// [`BackendRecord::name`] of the currently selected backend, if any. Sanitized on save
    /// with the same function as the record names, so the pointer follows them.
    #[serde(default)]
    pub active: Option<String>,
    /// Why the file on disk was NOT read (`load_from`'s hint), in which case this registry is
    /// empty. Never written: the editor refuses a Save while it is set and `save_to` writes
    /// nothing over a refused file.
    #[serde(skip)]
    pub refused: Option<String>,
}

/// The registry file's basename, stated once.
///
/// `pub(crate)` so `crate::ui::window::workspace`'s `FAMILY_MEMBERS` — the list of what a state-directory
/// relocation moves — names this file through the constant rather than through a second literal.
pub(crate) const BACKENDS_FILE: &str = "backends.json";

/// The ACTIVE backend's whole RECORD, if the registry names one and it is usable — the one a
/// STARTUP connect must use.
///
/// The "configure once" half of the resolution order. Reads the file every call rather than caching
/// it: the Connections editor writes it while the app runs, and a viewer that kept a stale address
/// after the user changed it would be the same class of confusion this replaced.
///
/// ⚠ An address alone loses the record's NAME and its `control` arming, which is why the startup
/// path takes the whole record rather than rebuilding a synthetic one
/// ([`crate::backend::connection::cli_observe_record`]) around the address. (Measured 2026-09-03,
/// when records still named a shared key: the rebuilt record signed with the wrong one and never
/// authenticated once.)
///
/// The rules: the pointer must name a record the registry holds, and a record with a blank address
/// is treated as absent (a half-filled editor row is unfinished, not an instruction to dial nothing).
#[must_use]
pub fn active_record() -> Option<BackendRecord> {
    pick_active(&load()).cloned()
}

/// [`active_record`]'s PURE half — the resolution with the file supplied, so the rules can be
/// tested without one on disk and the tests drive the real code rather than a copy of it.
fn pick_active(file: &BackendRegistry) -> Option<&BackendRecord> {
    let active = file.active.as_deref()?;
    file.backends.iter().find(|b| b.name == active).filter(|b| !b.address.trim().is_empty())
}

/// Registry file path for READING: `<base_dir>/backends.json` — beside the workspace file, so
/// `$VIKE_WORKSPACE` (whose directory is the family's base) moves this file together with the
/// layouts family. `None` when no base resolves (no override, no project above the working
/// directory): there is no file to read then, and [`load`] reports that as "no backends yet".
pub fn path() -> Option<PathBuf> {
    Some(crate::ui::window::workspace::base_dir()?.join(BACKENDS_FILE))
}

/// Registry file path for WRITING — same location as [`path`], with the directory created
/// lazily and a symlinked target refused (`vike_paths::state_path::write_path`, the same
/// resolver the workspace file's save uses).
fn save_path() -> std::io::Result<PathBuf> {
    vike_paths::state_path::write_path(
        crate::ui::window::workspace::base_dir().as_deref(),
        BACKENDS_FILE,
    )
}

/// Read the registry; an ABSENT file (or no resolvable location) is the ordinary "no backends
/// yet" state and answers the empty default. A file that exists and is not read (an older build's,
/// or a corrupt one) answers an EMPTY registry carrying the hint in [`BackendRegistry::refused`],
/// after a `tracing::error!` of it: startup never bricks, and nothing is read silently.
pub fn load() -> BackendRegistry {
    path().map(|p| load_or_refused_from(&p)).unwrap_or_default()
}

/// [`load`]'s pure half: absent → default, refused → an empty registry holding the hint.
fn load_or_refused_from(p: &Path) -> BackendRegistry {
    load_from(p).unwrap_or_else(|hint| {
        tracing::error!("{hint}");
        BackendRegistry { refused: Some(hint), ..BackendRegistry::default() }
    })
}

/// The one word only an older build's `backends.json` holds, spelled with `concat!` so a sweep
/// that renames the word cannot rewrite the one place that must still recognise it.
const OLD_ADDRESS_WORD: &str = concat!("ad", "dr");

/// Read + parse an explicit registry path: `Ok(default)` when absent, `Err(hint)` naming the file
/// and what to do when it exists and is not read. Crate-visible for the same round-trip test as
/// [`save_to`].
pub(crate) fn load_from(p: &Path) -> Result<BackendRegistry, String> {
    let raw = match std::fs::read_to_string(p) {
        Ok(raw) => raw,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(BackendRegistry::default()),
        Err(e) => return Err(not_read_hint(p, &e.to_string())),
    };
    serde_json::from_str::<BackendRegistry>(&raw).map_err(|e| {
        if e.to_string().contains(&format!("unknown field `{OLD_ADDRESS_WORD}`")) {
            format!(
                "{} was written by an older Vike Trader (it says `{OLD_ADDRESS_WORD}`; this build \
                 reads `address`) and is not read: move it away and add your backends again in Data \
                 Manager → Backend. Nothing is saved over it until then.",
                p.display()
            )
        } else {
            not_read_hint(p, &e.to_string())
        }
    })
}

/// The hint for a registry file that exists and is not read for any other reason.
fn not_read_hint(p: &Path, e: &str) -> String {
    format!(
        "{} could not be read ({e}): nothing is saved over it until it is fixed or moved away.",
        p.display()
    )
}

/// Serialize + write the registry; returns the path written. Record names and the `active`
/// pointer are passed through `sanitize_layout_name` on the way out.
pub fn save(file: &BackendRegistry) -> std::io::Result<PathBuf> {
    let p = save_path()?;
    save_to(file, &p)?;
    Ok(p)
}

/// [`save`]'s pure half (crate-visible so `backend::editor`'s round-trip test drives the REAL
/// saver over a temp path, no env juggling): sanitize names, then an atomic write to an explicit
/// path through `vike_utility::atomic_file`'s `write_atomic` (temp file beside the target,
/// fsync, rename, directory fsync), so a crash mid-write leaves the old file intact rather than a
/// truncated one. A rename Windows refuses (the destination held open without delete sharing) is
/// returned as the error with the old file untouched; there is no non-atomic fallback. A file at
/// `p` that [`load_from`] refuses is never written over: its hint is the error.
pub(crate) fn save_to(file: &BackendRegistry, p: &Path) -> std::io::Result<()> {
    load_from(p).map_err(std::io::Error::other)?;
    let clean = sanitized(file);
    let json = serde_json::to_string_pretty(&clean).map_err(std::io::Error::other)?;
    vike_utility::atomic_file::write_atomic(p, json.as_bytes())
}

/// The copy [`save_to`] actually writes: every record name and the `active` pointer sanitized,
/// so what lands on disk can never carry a path-hostile name (and the pointer still matches the
/// record it named, because both go through the same function).
fn sanitized(file: &BackendRegistry) -> BackendRegistry {
    BackendRegistry {
        backends: file
            .backends
            .iter()
            .map(|r| BackendRecord { name: sanitize_layout_name(&r.name), ..r.clone() })
            .collect(),
        active: file.active.as_deref().map(sanitize_layout_name),
        refused: None,
    }
}

/// What a READ of the backend at `address` signs with (its Backend settings section, the node's
/// directory): this desktop's device key read afresh ([`fresh_device_key`], decision 0288), so a key made
/// in a terminal since the last read is used. `Err` is [`no_device_key`]: a desktop that holds none sends
/// nothing (decision 0288 §3). One spelling for what the shell's reads each restated.
pub fn observe_signer(address: &str) -> Result<DeviceKey, HubCallError> {
    fresh_device_key().ok_or_else(|| not_sent(no_device_key(address)))
}

/// What a WRITE to `rec` signs with: the device key, on an ARMED record only (the B9 gate is the
/// record's `control` switch; the server's access list decides the rest). `Err` says why nothing is
/// sent.
pub fn control_signer(rec: &BackendRecord) -> Result<DeviceKey, HubCallError> {
    if !rec.control {
        return Err(not_sent(
            "this backend's write channel is not armed: tick `control armed` in its record (Data \
             Manager → Backend → Edit)",
        ));
    }
    observe_signer(&rec.address)
}

/// A signer's refusal: NOT SENT, nothing was dialled (the same type the hub call it stands in for
/// returns, so a caller chains the two).
fn not_sent(why: impl Into<String>) -> HubCallError {
    HubCallError::NotSent(std::io::Error::other(why.into()))
}

// ⚠ THE DATAHUB NODE-KEY BLOCK SITS ABOVE `mod tests`, AND THE POSITION USED TO BE LOAD-BEARING.
// `crates/vike-ops/tests/settings_secrets/hub_key_store_gate/source_scan.rs`'s `code_lines` finds the end of a `#[cfg(test)]`
// item by COUNTING BRACES, and it counted them inside string literals too. This file's
// `missing_file_is_an_empty_default` writes the corrupt payload `"{ not json"` — one unmatched
// brace — so from that line to EOF the gate saw nothing at all, and a hub-key constructor call
// placed below it was invisible: the gate reported the DECLARED_SITES row as stale rather than
// reporting the call. That was a blind spot in the SCANNER, not in this file.
//
// It is CLOSED: that gate's `without_literals` now drops string, raw-string and char-literal spans
// before counting, and its `an_unmatched_brace_in_a_string_literal_does_not_hide_the_code_below_it`
// plants this exact payload as the kill proof. The block stays here because moving it back would be
// churn, not because it has to be — and this note stays because the workaround it describes is the
// reason the code is where it is.

/// **What a datahub dial of this desktop signs with: its own device key alone** (decision 0288 item 8, the
/// cutover). A datahub admits a person by its access list, so no shared datahub key is read, and a `hub_key` row
/// left behind is dead. Nothing names a key here: when the dial must re-resolve (the Server group made the key
/// mid-session) is [`device_key_epoch`]'s to say.
///
/// Returns the keys (`None` = this desktop holds no device key, which is what makes a key-less loopback dev
/// server work untouched) plus every NOTICE the resolution produced — an unreadable store, or a row that is not a
/// key — as data, for the caller to log once. Never panics, never creates a file, never writes one.
/// `crates/vike-app-core/tests/hub_key_store_spellings.rs`'s `the_datahub_dials_here_carry_the_device_key_and_never_the_pair`
/// is the gate.
#[must_use]
pub fn datahub_dial_keys(
    settings_dir: Option<&str>,
) -> (Option<vike_hub_protocol::auth::HubKeys>, Vec<String>) {
    let (device, notice) = load_device_key(settings_dir);
    let keys = device.map(|(device, _)| vike_hub_protocol::auth::HubKeys::of_device(device));
    (keys, notice.into_iter().collect())
}

/// THIS desktop's own device key (decision 0288), as the last [`fill_hub_keys`] read it: what every
/// tradehub dial signs with, and without which it sends nothing. A process cell rather than a
/// parameter because the shell's credential read ([`fill_hub_keys`]) is what refreshes it (the Server
/// group's connect, or `vike-cli backend connect`, may have just made the key).
static DEVICE_KEY: std::sync::Mutex<Option<DeviceKey>> = std::sync::Mutex::new(None);

/// This desktop's device key, if it holds one (`DEVICE_KEY`): the cached answer, for what is asked
/// every frame. A dial about to be made asks [`fresh_device_key`].
pub fn device_key() -> Option<DeviceKey> {
    lock(&DEVICE_KEY).clone()
}

/// The settings directory the last [`fill_hub_keys`] read, so [`fresh_device_key`] re-reads the SAME
/// store from any thread; `None` until the shell's first read.
static SETTINGS_DIR_READ: std::sync::Mutex<Option<Option<String>>> = std::sync::Mutex::new(None);

/// **This desktop's device key read afresh** (one row) from the store the last [`fill_hub_keys`] read, so a
/// key made OUTSIDE this app — `vike-cli backend mint-missing` in a terminal, as [`no_device_key`] says — is
/// signed with at the next dial, never only after a restart. Before the shell's first read it is
/// [`device_key`]'s cached answer. For a dial about to be made (a click, a re-dial, a page's arrival), never
/// for a per-frame question. `crates/vike-app-core/tests/hub_key_store_spellings.rs`'s
/// `a_key_made_outside_the_desktop_is_read_at_the_next_dial` is the gate.
pub fn fresh_device_key() -> Option<DeviceKey> {
    let read = lock(&SETTINGS_DIR_READ).clone();
    if let Some(settings_dir) = read {
        fill_hub_keys(settings_dir.as_deref());
    }
    device_key()
}

/// How many times [`fill_hub_keys`] found this desktop's device key CHANGED (made, replaced or gone).
static DEVICE_KEY_EPOCH: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// **The device key's epoch: what every datahub dial that KEEPS its resolved keys compares.** A dial
/// resolved before the key existed holds `None` and dials key-less, which a keyed datahub refuses at
/// the handshake; the Server group makes the key mid-session, so a holder that re-resolved only when
/// its address moved kept dialling key-less forever (the CI box, 2026-11-16: the datahub
/// refused the owner's desktop every 1-60 s while the tradehub, which reads [`device_key`] at every
/// dial, admitted it). The market-data session, the venue catalog and the DataSets re-resolve when
/// this moves; a read resolved per dial (the stored load, the chart seed) never held a stale one.
pub fn device_key_epoch() -> u64 {
    DEVICE_KEY_EPOCH.load(std::sync::atomic::Ordering::Acquire)
}

/// Hold `device` in [`DEVICE_KEY`], moving [`device_key_epoch`] when it is not the key held before.
fn remember_device_key(device: Option<DeviceKey>) {
    let mut held = lock(&DEVICE_KEY);
    let id = |key: &Option<DeviceKey>| key.as_ref().map(|k| k.key_id());
    if id(&held) != id(&device) {
        DEVICE_KEY_EPOCH.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
    }
    *held = device;
}

/// **The ONE line a desktop says about what it signs with, and only when it has nothing to sign
/// with.** Since the ACCESS cutover (decision 0288 item 8) a person signs with this PC's device key and
/// nothing else: no hub loads a shared key any more, so no message may name one as what is missing.
/// With the device key held there is nothing to say; without it, nothing is sent and this is the fix:
/// the hub protocol's ONE wording of the cure (`vike_hub_protocol::conn::no_device_key_cure`), by
/// whether `address` names this box (`vike_hub_protocol::conn::names_this_box`), and the desktop's own
/// button for the remote route.
pub fn no_device_key(address: &str) -> String {
    let cure = vike_hub_protocol::conn::no_device_key_cure(
        vike_hub_protocol::conn::names_this_box(address),
    );
    format!(
        "this PC has no device key, so nothing was sent. {cure} (in this app, Connect in Data \
         Manager → Backend → Server registers it with a server)"
    )
}

/// Set while this process creates its settings database ([`while_making_the_store`]).
static STORE_BEING_MADE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// **Run `make` (the creation of this PC's settings database) with every device-key read held off.**
/// The Server group's first connect creates the store on its own thread while other threads read the
/// device key from it (the bridge's re-dial and every dial about to be made, [`fresh_device_key`]; it
/// was the frame thread, every frame, when this was measured); `vike_secrets::create_store` writes the file before
/// its stamp commits, so a read in between found "schema version 0, not 2 — it was never finished" and
/// warned once (latched, so it also hid any later notice) about a store that was a millisecond from
/// done (the owner's PC, 2026-11-16). While `make` runs a read answers "no key yet", which is the
/// truth; the next read after it finds the finished store.
pub fn while_making_the_store<T>(make: impl FnOnce() -> T) -> T {
    struct Done;
    impl Drop for Done {
        fn drop(&mut self) {
            STORE_BEING_MADE.store(false, std::sync::atomic::Ordering::Release);
        }
    }
    STORE_BEING_MADE.store(true, std::sync::atomic::Ordering::Release);
    let _done = Done; // cleared on a panic too, so a failed create never silences the reads for good
    make()
}

/// What a connect to `address` says about its signer: [`no_device_key`] when this PC holds no device
/// key, nothing when it does. PURE.
pub fn signer_notice(device_held: bool, address: &str) -> Option<String> {
    (!device_held).then(|| no_device_key(address))
}

/// The name [`DEVICE_KEY`] was registered under, `person/device` (its `device_key` row): what a
/// connect's log line says this desktop connected as. Not a secret.
static DEVICE_NAME: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

/// [`DEVICE_NAME`], when this desktop holds a device key.
pub(crate) fn device_name() -> Option<String> {
    lock(&DEVICE_NAME).clone()
}

/// This desktop's device key from its own store's `device_key` table (the PKCS#8 document a
/// connect made) with the `person/device` it was registered as, plus a notice when a store that
/// exists could not answer. It never creates one and never shows a byte of it.
fn load_device_key(
    settings_dir: Option<&str>,
) -> (Option<(vike_hub_protocol::access::DeviceKey, String)>, Option<String>) {
    if STORE_BEING_MADE.load(std::sync::atomic::Ordering::Acquire) {
        return (None, None); // see `while_making_the_store`: there is no key yet, and nothing to report
    }
    let dir = vike_secrets::workspace_settings_dir_from(settings_dir);
    match vike_client::resolved::device_key_of(vike_secrets::access::device_key_load(&dir)) {
        Ok(held) => (held, None),
        Err(DeviceKeyFault::NotAKey { key_id, error }) => {
            (None, Some(format!("this desktop's device key {key_id} cannot be used: {error}")))
        }
        Err(DeviceKeyFault::Unreadable(e)) => {
            (None, Some(format!("cannot read this desktop's device key: {e}")))
        }
    }
}

/// **The datahub STORE handle every hist-plane read dials through** — authenticated when this
/// desktop holds its device key, and the ONE spelling of that dial.
///
/// It lives beside [`datahub_dial_keys`] because the key resolution IS the dial's hard part:
/// every consumer (the Studio's one-shot open, the Data Manager's inventory walk, the chart's
/// store read in [`crate::data::stored`]) needs the identical pairing of "resolve this desktop's
/// device key, then hand it to `with_keys`", and the binary's own wrapper is now
/// just the one composition-root fact — the settings directory — fed in as a parameter.
///
/// ⚠ **There is no configuration in which the key-less constructor is the better choice**, which is
/// why this has no "plain" arm to pick: `HistoryStoreDatahub::with_keys`' own contract is that against
/// a KEY-LESS server it degrades to an ordinary unauthenticated connect, "so one configured GUI
/// works against both a keyed production datahub and a local dev one". The `None` arm below is
/// reached only when this desktop holds NO key to offer, never as a preference. What the key-less
/// path cost when both call sites were on it — `No stored data · 0 rows · 0 B` from a datahub that
/// answered the same address with 7.49 billion rows on the CLI — is recorded at
/// [`datahub_dial_keys`].
///
/// Notices from the key resolution are LOGGED here rather than returned: every caller did the same
/// thing with them, and a store dial is not a place an operator is watching for a return value.
pub fn open_history_store_datahub(
    address: String,
    settings_dir: Option<&str>,
) -> vike_datahub_client::HistoryStoreDatahub {
    let (keys, notices) = datahub_dial_keys(settings_dir);
    for n in notices {
        tracing::warn!("datahub store: {n}");
    }
    let store = match keys {
        Some(k) => vike_datahub_client::HistoryStoreDatahub::with_keys(address, k),
        None => vike_datahub_client::HistoryStoreDatahub::new(address),
    };
    // The desktop reads its datahub across a link (an ssh tunnel to the server box, 13-40 KB/s
    // measured), so its bar pages cross LZ4-compressed; the rows are the same.
    store.with_compressed_bars()
}

#[path = "registry_tests.rs"]
#[cfg(test)]
pub(crate) mod registry_tests;

// ─── The device key: where it comes from ─────────────────────────────────────────────────────

/// What ONE [`fill_hub_keys`] call did — returned as DATA rather than only logged, so a test can
/// assert the resolution and a caller that has a subscriber can render the notices itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackendKeyResolution {
    /// An unreadable-store or unusable-key finding, already rendered. Empty on the overwhelmingly
    /// common path.
    pub notices: Vec<String>,
}

/// **What the tradehub dials of this desktop sign with: refresh this desktop's DEVICE key** (decision 0288 item 8,
/// the cutover). A hub admits a person by its access list alone, so no shared key is read from any store or map.
/// The device key is what every tradehub dial signs with ([`device_key`], [`observe_signer`], [`control_signer`]),
/// read afresh here because a connect may have just made it.
///
/// # ⚠ `settings_dir` is a PARAMETER and this function reads NO environment
///
/// The composition root owns the boot walk, so taking it here is what keeps this out of
/// `crates/vike-ops/tests/settings_secrets/settings_registry.rs`'s `LIBRARY_PIN` (a ratchet that may shrink, never
/// grow).
///
/// `crates/vike-app-core/tests/hub_key_store_spellings.rs`'s `the_gui_signs_with_its_device_key`
/// is the gate; `the_unreadable_store_notice_names_the_store` holds the unreadable store.
pub fn fill_hub_keys(settings_dir: Option<&str>) -> BackendKeyResolution {
    *lock(&SETTINGS_DIR_READ) = Some(settings_dir.map(str::to_string));
    let (device, notice) = load_device_key(settings_dir);
    let (device, name) = device.unzip();
    remember_device_key(device);
    *lock(&DEVICE_NAME) = name;
    let notices: Vec<String> = notice.into_iter().collect();
    report_hub_key_store_once(&notices);
    BackendKeyResolution { notices }
}

// Log the node-store notices at most ONCE per process, and only once a SUBSCRIBER exists.
///
/// ⚠ **Both halves of that are load-bearing.** `vike-desktop`'s `workspace_credentials` is called
/// on every connect, settings fetch and Data Manager credential read, so an unlatched notice would
/// be a wall of identical warnings;
/// and that same function is handed to `vike_boot::boot`, which runs BEFORE `vike_log::init` (the
/// log directory is itself a setting), so a latch armed on that first call would swallow the one
/// line the operator needs — a store notice emitted into a subscriber-less void and never
/// repeated. The notices are returned as data regardless, so a test asserts them and never this.
fn report_hub_key_store_once(notices: &[String]) {
    static SAID: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if notices.is_empty() || !tracing::dispatcher::has_been_set() {
        return;
    }
    if SAID.swap(true, std::sync::atomic::Ordering::Relaxed) {
        return;
    }
    for n in notices {
        tracing::warn!("hub keys: {n}");
    }
}

#[path = "active_record_tests.rs"]
#[cfg(test)]
pub(crate) mod active_record_tests;
