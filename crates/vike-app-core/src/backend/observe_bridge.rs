//! `observe_bridge` — the `--observe` thin-client's self-healing bridge from a tradehub's observe stream to
//! the GUI's `CoreSnapshot` cell.
//!
//! The read-only observer — the desktop shell, in EVERY launch since it lost its local core
//! (`crates/vike-desktop/src/app_setup.rs`'s `App::new`; it was `vike-app --observe <ADDR>`, one mode of
//! three, when this was written) — has NO local trading core. It connects to a headless
//! `vike-tradehub` daemon's observe server under `Scope::Read` and republishes each pushed
//! [`vike_tradehub_client::wire::CoreSnapshotWire`] into a GUI-owned arc-swap cell as a
//! [`vike_execution::CoreSnapshot`], so every existing Trade/Account/Portfolio panel renders the remote
//! daemon's live trading state unchanged. The decode itself is
//! [`vike_tradehub_client::observe_decode::wire_to_core`], beside the wire it mirrors: it lived in this file
//! until D56 moved it down (the owner's 2026-11-17 ruling), and what stays is the GUI's half — the dial and
//! re-dial thread, the identity and datahub cells, the status-line words and the control gate. The shell
//! reaches this module through [`crate::backend::connection`], which spawns [`spawn_bridge`] (and
//! [`connect_control`]) for the backend it connects to.

use vike_hub_protocol::access::DeviceKey;
use vike_model::feed_status::{DataClientConnectionState, DataClientStatus};
use vike_tradehub_client::node_words::BackendNodeWords;
use vike_tradehub_client::proto::SeriesKeyWire;

/// Render a daemon identity as the short display tag every status surface leads with: `name [demo]`,
/// `name [LIVE + demo]`, where `running` is what it runs now ([`vike_tradehub_client::node_words::running_tiers`];
/// split-plane I3: "which daemon is the armed control channel pointing at" is the spec's named
/// most-dangerous ambiguity). The status bar's one backend item leads with it, through
/// [`observing_status`], so the daemon is named once.
pub(crate) fn identity_label(
    id: &vike_tradehub_client::wire::HubIdentityWire,
    running: &str,
) -> String {
    format!("{} [{running}]", id.name)
}

/// The connected observe status line — `"<address> (connected)"` until a frame carries the
/// daemon's [`HubIdentityWire`](vike_tradehub_client::wire::HubIdentityWire), then led by
/// [`identity_label`] over what that frame says it runs: `"the build runner [demo] — <address> (connected)"`.
/// Pure so it is unit-tested below; [`spawn_bridge`]'s thread is the one caller.
///
/// It says WHICH node, WHERE and that the link is up, and nothing about what this desktop may do
/// there: the status bar draws ONE backend item, this line then the access words
/// (`crate::backend::tradehub_control::access_line`: `OBSERVING` read-only, `CONTROL: …` or `ADMIN: …`),
/// so the daemon is named once and a read-only word never stands beside a writing one.
///
/// These are the WORDS only. [`spawn_bridge`] writes them beside the state it knows
/// (`DataClientConnectionState::Connected`: this is called from the `Ok(remote)` arm of the
/// reconnect loop and from nowhere else), and no reader recovers a state from them. They were once
/// classified by keyword, and a daemon tagged `[LIVE]` then read green while one tagged `[paper]`
/// read grey: the dot tracked ARMING, not the link.
pub(crate) fn observing_status(
    backend: Option<&str>,
    address: &str,
    identity: Option<(&vike_tradehub_client::wire::HubIdentityWire, &str)>,
) -> String {
    // ⚠ WHICH address this line names is `backend::identity`'s decision, not this function's — the
    // status bar and the Connections foot strip must not name two different boxes for one
    // connection. A daemon that reported its own address wins; `address` (the DIAL address, and
    // through a tunnel always the tunnel mouth) is what is left when it reported none.
    let address = crate::backend::identity::shown_address(
        address,
        identity.map(|(id, _)| id.advertise_addr.as_str()),
    );
    let bare = format!("{address} (connected)");
    // The daemon's OWN self-report wins: it carries the tier tag as well as a name, and it is
    // the backend answering for itself rather than the registry answering about it. The registry
    // NAME is the rung below — the only identity this side has before the first frame lands, and
    // the whole of what an operator gets on a link that never carries one.
    let lead = match identity {
        Some((id, running)) => Some(identity_label(id, running)),
        None => backend.map(str::trim).filter(|n| !n.is_empty()).map(str::to_string),
    };
    led_by(lead.as_deref(), &bare)
}

/// The words of a dial this PC cannot sign (no device key; nothing was sent): a failed connect and
/// its address-aware cure. [`spawn_bridge`] writes them as an `Error`; the state is set there, so the
/// cure's own word "Connect" cannot make the dot amber (it did, while lines were read by keyword).
fn no_device_key_line(address: &str) -> String {
    let cure = crate::backend::registry::no_device_key(address);
    format!("observe connect to {address} failed: {cure}")
}

/// Lead `bare` with `label`, when there is one. A name is operator-chosen text, and it is safe in
/// the line because the line's STATE is set beside it, never read out of it: a backend named
/// `error-box` cannot turn a healthy link's dot red (it once did, and the name was dropped to save
/// the dot).
fn led_by(label: Option<&str>, bare: &str) -> String {
    match label.map(str::trim).filter(|l| !l.is_empty()) {
        Some(label) => format!("{label} — {bare}"),
        None => bare.to_string(),
    }
}

/// Stop handle for the [`spawn_bridge`] thread: raise the flag, then JOIN.
///
/// Exists for the multi-backend GUI (split-plane B1): switching backends at runtime replaces the
/// bridge, and the reconnect loop never exits on its own — without a join, every switch would leak
/// a thread that keeps dialling the OLD address forever. [`BackendObserveBridgeHandle::stop`] is idempotent and
/// [`Drop`] calls it, so letting the handle fall out of scope IS a full stop-and-join.
#[must_use = "dropping the handle stops the bridge thread — hold it for the bridge's lifetime"]
pub struct BackendObserveBridgeHandle {
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// `None` once [`BackendObserveBridgeHandle::stop`] has joined (taken on the first call — the idempotence),
    /// or when the spawn itself failed and there is no thread to join.
    join: Option<std::thread::JoinHandle<()>>,
    /// The daemon identity the CURRENT connection last reported (split-plane I3) — written by the
    /// bridge thread on every frame whose `identity` changed, cleared on a link drop (a stale name
    /// must not outlive its connection: the next daemon at the same address may be a different —
    /// or an identity-less — node). Read by the GUI via [`BackendObserveBridgeHandle::identity`] so the control
    /// status line can name the daemon without `CoreSnapshot` (built in the latency-gated vike-core fold) growing a
    /// display-only field.
    identity: std::sync::Arc<std::sync::Mutex<Option<vike_tradehub_client::wire::HubIdentityWire>>>,
    /// The datahub dial address the CURRENT connection's `Welcome.features` advertised
    /// (split-plane REQ-2, `datahub=<address>`) — written at connect (the handshake is where the
    /// advertisement lives, so unlike `identity` it needs no frame), cleared on a link drop under
    /// the SAME discipline: the next daemon at this address may front a different datahub, or
    /// none, and a stale advertisement must not outlive its connection. Read by the GUI via
    /// [`BackendObserveBridgeHandle::advertised_datahub`] into the one resolution point
    /// ([`crate::data::datahub::resolve_datahub_address`]), where an explicit
    /// `config.datahub_addr` still always wins.
    advertised_datahub: std::sync::Arc<std::sync::Mutex<Option<String>>>,
    /// What the current connection's last frame said beyond the snapshot — written on every frame
    /// whose words changed, cleared on a link drop like `identity`. Read by the GUI via
    /// [`BackendObserveBridgeHandle::node_words`].
    words: std::sync::Arc<std::sync::Mutex<std::sync::Arc<BackendNodeWords>>>,
    /// The bar series the GUI's charts show ([`BackendObserveBridgeHandle::show_series`]), stated to the node at every dial and
    /// whenever it changes; `None` until the GUI says (every series).
    series: std::sync::Arc<std::sync::Mutex<Option<Vec<SeriesKeyWire>>>>,
}

impl BackendObserveBridgeHandle {
    /// What the current connection's last frame said beyond the snapshot ([`BackendNodeWords`]); empty before
    /// the first frame and between a link drop and the next connection. A pointer read once per
    /// painted frame.
    pub fn node_words(&self) -> std::sync::Arc<BackendNodeWords> {
        self.words.lock().map(|g| g.clone()).unwrap_or_default()
    }
    /// **The bar series the GUI's charts show now**, sorted: the node then sends the bars of those alone
    /// (`vike_tradehub_client::proto::FEATURE_OBSERVE_SERIES`; orders, positions, accounts and halts always flow). Called
    /// on every painted frame; only a CHANGE (a chart opened or closed) reaches the node, from the bridge thread.
    pub fn show_series(&self, mut series: Vec<SeriesKeyWire>) {
        series.sort_unstable();
        series.dedup();
        if let Ok(mut shown) = self.series.lock()
            && shown.as_ref() != Some(&series)
        {
            *shown = Some(series);
        }
    }
    /// The daemon identity the current connection last reported — `None` before the first
    /// identity-carrying frame, from an older (pre-B3) node, or between a link drop and the next
    /// connection's first frame. A clone (five short strings) read once per painted frame.
    pub fn identity(&self) -> Option<vike_tradehub_client::wire::HubIdentityWire> {
        self.identity.lock().ok().and_then(|g| g.clone())
    }
    /// The datahub dial address the current connection's Welcome advertised — `None` before the
    /// first successful connect, from a daemon with no `datahub_advertise_addr` configured, or
    /// between a link drop and the next successful handshake (the advertisement is only as live
    /// as its connection). Feed it to [`crate::data::datahub::resolve_datahub_address`] beside the
    /// explicit `config.datahub_addr` — never adopt it directly.
    pub fn advertised_datahub(&self) -> Option<String> {
        self.advertised_datahub.lock().ok().and_then(|g| g.clone())
    }
    /// Raise the stop flag and join the bridge thread. Idempotent: the join handle is taken on
    /// the first call, so a second call (or the [`Drop`] after an explicit `stop`) is a no-op.
    /// Returns promptly — the thread re-checks the flag every `STOP_SLICE` both in the connected
    /// snapshot poll and inside the dial backoff, so a stop parks for ~one slice, never the full
    /// `DIAL_BACKOFF` window and never a whole `preferences.observe_poll_ms` however long that is
    /// set.
    pub fn stop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

impl Drop for BackendObserveBridgeHandle {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Backoff between dial attempts — long enough not to hammer a down daemon.
const DIAL_BACKOFF: std::time::Duration = std::time::Duration::from_secs(2);
/// How finely every wait of the bridge thread is sliced so a [`BackendObserveBridgeHandle::stop`] lands in ~one
/// slice: the granularity of the backoff sleep AND of the connected poll's sleep. It is NOT the
/// poll interval any more (that is the `poll` argument of [`spawn_bridge`], the
/// `preferences.observe_poll_ms` row): the two used to be one 50ms constant, and a poll longer
/// than the slice must not make a backend switch — which joins this thread on the GUI's frame
/// thread — wait out a whole poll.
const STOP_SLICE: std::time::Duration = std::time::Duration::from_millis(50);

/// How often the bridge asks the node what it runs (`strategy_status`, one short connection): a mount
/// comes and goes rarely, and no frame carries it. Far slower than the frames, never on this thread.
const MOUNTS_EVERY: std::time::Duration = std::time::Duration::from_secs(30);

/// Ask the node for its mount rows on a throwaway thread, signed with `device` (the key this
/// connection dialled with), landing them in `rows`, a cell of THIS connection: an answer that lands
/// after a link drop lands in a cell nobody reads. An older node refuses the verb before anything is
/// sent, and the rows then stay empty.
fn ask_mounts(
    address: String,
    device: DeviceKey,
    rows: std::sync::Arc<std::sync::Mutex<Vec<vike_tradehub_client::wire::MountViewWire>>>,
) {
    let asked = std::thread::Builder::new().name("ui-mounts".into()).spawn(move || {
        match vike_tradehub_client::strategy_status(address.as_str(), &device) {
            Ok(status) => {
                if let Ok(mut g) = rows.lock() {
                    *g = status.mounts;
                }
            }
            Err(e) => {
                tracing::debug!(addr = %address, "observe: the node's mount rows were not read: {e}")
            }
        }
    });
    if let Err(e) = asked {
        tracing::warn!("observe: could not start the mount rows' read: {e}");
    }
}

/// Sleep up to `total` in naps of at most [`STOP_SLICE`], returning as soon as `stopped` says so.
///
/// The ONE sliced wait the bridge thread uses, for the dial backoff and for the poll between
/// snapshot reads alike, so neither can park a [`BackendObserveBridgeHandle::stop`] for longer than one slice.
/// A `total` of at most one slice is one nap of exactly `total`, which at the default poll is the
/// plain 50ms sleep the loop always took.
fn nap_unless_stopped(total: std::time::Duration, stopped: &impl Fn() -> bool) {
    let mut waited = std::time::Duration::ZERO;
    while waited < total && !stopped() {
        let nap = STOP_SLICE.min(total - waited);
        std::thread::sleep(nap);
        waited += nap;
    }
}

/// Spawn the `--observe` push→`CoreSnapshot` bridge thread; the returned [`BackendObserveBridgeHandle`] is the
/// ONLY way to stop it before process exit — hold it for exactly the bridge's intended lifetime.
///
/// The connect lives INSIDE the thread so it (a) never blocks window startup and (b) RE-DIALS on a
/// dropped link: when the daemon restarts or the tunnel blips, `is_connected()` flips false, we drop
/// the handle, back off, and reconnect from scratch (fresh handshake + subscribe) — the observer
/// heals itself instead of freezing on the last frame. The status line tracks each phase.
///
/// Moved down out of `vike-app`'s `App::new` with the rest of the wiring: this loop is the only
/// consumer of three `vike_tradehub_client::TradehubCoreHandle` APIs (`connect` / `is_connected` /
/// `snapshot`) plus [`vike_tradehub_client::observe_decode::wire_to_core`], and it lived in a file no gate
/// compiles — so a signature
/// change in that CI-tested crate broke the observer silently.
///
/// The GUI-owned arc-swap cell and the egui repaint stay in the binary as two closures: `publish`
/// receives the already-`Arc`ed snapshot exactly as `ArcSwap::store` wants it (so nothing is copied
/// that was not copied before), and `repaint` is the same `context.request_repaint()` wake. Keeping the
/// cell out of the signature is what lets this crate stay free of an `arc_swap` dependency — the
/// same choice [`crate::ui::core_sync`] made.
/// `backend` is the record's NAME (`crate::backend::identity::status_label`) — the one thing this
/// side knows that IDENTIFIES the box, and `None` for a record that carries none. It is an
/// `Option<String>` rather than a second `String` deliberately: two adjacent `String` parameters
/// are a swap that compiles.
///
/// `device_key` answers what each dial signs with, asked afresh at EVERY dial because the Server
/// group, or `vike-cli backend mint-missing` in a terminal, makes the key mid-session (production passes
/// `crate::backend::registry::fresh_device_key`, which re-reads the store). With
/// none, nothing is sent (decision 0288 §3): the status line says [`crate::backend::registry::no_device_key`]
/// and the bridge asks again after the backoff.
///
/// `poll` is the sleep between the bridge's reads of the push-fed cell — the caller's
/// `preferences.observe_poll_ms` row, so it is also the ceiling on how often a chart can refresh
/// from the node. It runs on this thread, never the GUI's frame thread, and is slept in
/// `STOP_SLICE` naps so a stop lands within one slice whatever it is.
///
/// `patience` is the operator's resolved link patience (the desktop reads the two
/// `preferences.*` rows in its `main.rs` and hands them down; this library reads no setting): the
/// bridge dials with `patience.observe_stall` as the observe stream's silence limit, on every
/// re-dial. `LinkPatience::default()` is the window every dial had before the rows existed.
pub fn spawn_bridge<K, P, R>(
    address: String,
    backend: Option<String>,
    device_key: K,
    poll: std::time::Duration,
    patience: vike_tradehub_client::liveness::LinkPatience,
    status: std::sync::Arc<std::sync::Mutex<DataClientStatus>>,
    publish: P,
    repaint: R,
) -> BackendObserveBridgeHandle
where
    K: Fn() -> Option<DeviceKey> + Send + 'static,
    P: Fn(std::sync::Arc<vike_execution::CoreSnapshot>) + Send + 'static,
    R: Fn() + Send + 'static,
{
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let stop_thread = stop.clone();
    let identity = std::sync::Arc::new(std::sync::Mutex::new(None));
    let identity_thread = identity.clone();
    let advertised_datahub = std::sync::Arc::new(std::sync::Mutex::new(None));
    let advertised_thread = advertised_datahub.clone();
    let words: std::sync::Arc<std::sync::Mutex<std::sync::Arc<BackendNodeWords>>> =
        Default::default();
    let words_thread = words.clone();
    let series: std::sync::Arc<std::sync::Mutex<Option<Vec<SeriesKeyWire>>>> = Default::default();
    let series_thread = series.clone();
    let shown_series = move || series_thread.lock().map(|g| g.clone()).unwrap_or_default();
    let address_thread = address;
    let backend_thread = backend;
    let join = std::thread::Builder::new()
        .name("ui-observe".into())
        .spawn(move || {
            let stopped = || stop_thread.load(std::sync::atomic::Ordering::Relaxed);
            let set_status = |state: DataClientConnectionState, text: String| {
                if let Ok(mut g) = status.lock() {
                    *g = DataClientStatus::new(state, text);
                }
            };
            let set_identity = |id: &Option<vike_tradehub_client::wire::HubIdentityWire>| {
                if let Ok(mut g) = identity_thread.lock() {
                    g.clone_from(id);
                }
            };
            let set_advertised = |a: Option<String>| {
                if let Ok(mut g) = advertised_thread.lock() {
                    *g = a;
                }
            };
            let set_words = |w: BackendNodeWords| {
                if let Ok(mut g) = words_thread.lock() {
                    *g = std::sync::Arc::new(w);
                }
            };
            let mut shown_words = BackendNodeWords::default();
            let mut last_sequence = u64::MAX;
            // The identity currently RENDERED in the status line + shared cell — per connection,
            // reset on a drop (a stale daemon name must not outlive its connection).
            let mut shown_identity: Option<vike_tradehub_client::wire::HubIdentityWire> = None;
            while !stopped() {
                // Asked at every dial: a connect may have just made the key. None: nothing is sent.
                let Some(device) = device_key() else {
                    set_status(
                        DataClientConnectionState::Error,
                        led_by(backend_thread.as_deref(), &no_device_key_line(&address_thread)),
                    );
                    nap_unless_stopped(DIAL_BACKOFF, &stopped);
                    continue;
                };
                // The NAME leads here too: "connecting to 127.0.0.1:7879…" is the same string on
                // every box, and this is the line an operator stares at while nothing works.
                set_status(
                    DataClientConnectionState::Connecting,
                    led_by(backend_thread.as_deref(), &format!("connecting to {address_thread}…")),
                );
                // The series the charts show NOW, so the first frame carries no bars nobody shows.
                let mut stated = shown_series();
                match vike_tradehub_client::TradehubCoreHandle::connect_with_series(
                    address_thread.as_str(),
                    &device,
                    patience.observe_stall,
                    stated.clone(),
                ) {
                    Ok(remote) => {
                        set_status(
                            DataClientConnectionState::Connected,
                            observing_status(backend_thread.as_deref(), &address_thread, None),
                        );
                        // The REQ-2 advertisement lives in the handshake this connect just ran —
                        // publish it for the GUI's datahub resolution (explicit key still wins,
                        // see `crate::data::datahub`). Cleared again on a link drop below.
                        set_advertised(remote.advertised_datahub().map(str::to_string));
                        tracing::info!(
                            address = %address_thread,
                            advertised_datahub = remote.advertised_datahub().unwrap_or("(none)"),
                            "observe: connected"
                        );
                        let mount_rows: std::sync::Arc<std::sync::Mutex<Vec<_>>> =
                            std::sync::Arc::default();
                        let mut mounts_asked: Option<std::time::Instant> = None;
                        // Poll the LOCAL push-fed cell until the link drops (or we are told to
                        // stop) — a poll of this size never blocks the node.
                        while remote.is_connected() && !stopped() {
                            // A chart opened or closed: the node sends the bars of the series shown now.
                            let shown = shown_series();
                            if shown != stated {
                                if let Err(e) = remote.show_series(shown.clone()) {
                                    tracing::debug!(addr = %address_thread, "observe: the shown series were not sent: {e}");
                                }
                                stated = shown;
                            }
                            if mounts_asked.is_none_or(|at| at.elapsed() >= MOUNTS_EVERY) {
                                mounts_asked = Some(std::time::Instant::now());
                                ask_mounts(
                                    address_thread.clone(),
                                    device.clone(),
                                    std::sync::Arc::clone(&mount_rows),
                                );
                            }
                            let wire = remote.snapshot();
                            if wire.sequence != last_sequence {
                                last_sequence = wire.sequence;
                                // I3: the frame names its daemon and what it runs — surface them in
                                // the observe status and the shared cell the GUI reads (only on
                                // change; the usual case is a cheap eq check per frame).
                                let mut said = BackendNodeWords::of(&wire);
                                if let Ok(rows) = mount_rows.lock() {
                                    said.mounts.clone_from(&rows);
                                }
                                if wire.identity != shown_identity
                                    || said.running != shown_words.running
                                {
                                    shown_identity.clone_from(&wire.identity);
                                    set_identity(&wire.identity);
                                    set_status(
                                        DataClientConnectionState::Connected,
                                        observing_status(
                                            backend_thread.as_deref(),
                                            &address_thread,
                                            wire.identity
                                                .as_ref()
                                                .map(|id| (id, said.running.as_str())),
                                        ),
                                    );
                                }
                                // Published BEFORE the snapshot, so the frame that repaints sees it.
                                if said != shown_words {
                                    shown_words = said;
                                    set_words(shown_words.clone());
                                }
                                publish(std::sync::Arc::new(
                                    vike_tradehub_client::observe_decode::wire_to_core(&wire),
                                ));
                                repaint();
                            }
                            nap_unless_stopped(poll, &stopped);
                        }
                        if stopped() {
                            // Told to stop, not a dropped link: no "reconnecting…" status/log.
                            break;
                        }
                        // Link dropped. Force a re-store on the NEW connection's first
                        // frame (its `seq` may restart at 0), forget the dropped connection's
                        // identity AND its datahub advertisement (the next daemon may be a
                        // different node fronting a different — or no — datahub), and flag the
                        // outage.
                        last_sequence = u64::MAX;
                        shown_identity = None;
                        set_identity(&None);
                        shown_words = BackendNodeWords::default();
                        set_words(BackendNodeWords::default());
                        set_advertised(None);
                        tracing::warn!(addr = %address_thread, "observe: link dropped; reconnecting");
                        // An UNPLANNED loss (a told-to-stop never reaches here): a fault, red, like
                        // the ws engine's mid-stream drop. `Disconnected` is a stop on purpose.
                        set_status(
                            DataClientConnectionState::Error,
                            format!("disconnected from {address_thread}, reconnecting…"),
                        );
                        repaint();
                    }
                    Err(e) => {
                        tracing::warn!(addr = %address_thread, error = %e, "observe: reconnect failed; retrying");
                        set_status(
                            DataClientConnectionState::Error,
                            format!("observe connect to {address_thread} failed ({e}); retrying…"),
                        );
                    }
                }
                // Backoff before the next dial, SLICED: the flag is re-checked every STOP_SLICE so
                // a `stop()` mid-backoff returns in ~one slice, never the full window (the old
                // unsliced 2s sleep is why this thread could only die with the process — B6).
                nap_unless_stopped(DIAL_BACKOFF, &stopped);
            }
            tracing::info!(addr = %address_thread, "observe: bridge stopped");
        })
        .ok();
    BackendObserveBridgeHandle { stop, join, identity, advertised_datahub, words, series }
}

/// Build the `--observe` **`Scope::Write` WRITE path** — opt-in, default OFF.
///
/// A SECOND, dedicated connection under `Scope::Write`, built ONLY when `enabled` (the
/// `flags.tradehub_control` row of the PC's settings database AND the record's arming — the CALLER
/// reads both and the key, so this stays a pure function of its arguments) AND `device` is this
/// desktop's own device key (decision 0288: the server's access list decides whether it may write;
/// without one nothing is sent). When it connects, the GUI's order buttons drive this observer's REMOTE
/// core (see `App.tradehub_control` + the command-dispatch choke point): ⚠ **this can place/cancel REAL
/// orders on the daemon.** A missing key or a failed connect leaves it `None` and the observer
/// stays read-only.
///
/// `connect` may block briefly at startup (fine — same one-shot handshake cost as
/// [`TradehubCoreHandle::connect`](vike_tradehub_client::TradehubCoreHandle::connect)).
///
/// `patience.control_reply` is the control worker's reply wait (the caller read the
/// `preferences.control_reply_timeout_ms` row; this library reads no setting).
///
/// Moved down out of `App::new` because it is a **safety gate guarding real order placement** whose
/// refuse-paths lived in a file no test could reach; the two that need no network are pinned below.
pub fn connect_control(
    enabled: bool,
    address: &str,
    device: Option<&DeviceKey>,
    patience: vike_tradehub_client::liveness::LinkPatience,
) -> Option<vike_tradehub_client::TradehubControlHandle> {
    if !enabled {
        return None;
    }
    let Some(device) = device else {
        tracing::warn!(
            "flags.tradehub_control is on, but {}; observer stays read-only",
            crate::backend::registry::no_device_key(address)
        );
        return None;
    };
    match vike_tradehub_client::TradehubControlHandle::connect_with_reply_timeout(
        address,
        device,
        patience.control_reply,
    ) {
        Ok(h) => {
            let registered_as = crate::backend::registry::device_name();
            let line = crate::backend::tradehub_control::connected_line(
                h.admitted(),
                registered_as.as_deref(),
            );
            tracing::info!(addr = %address, "{line}");
            Some(h)
        }
        Err(e) => {
            tracing::error!(addr = %address, error = %e, "control connect failed; observer stays read-only");
            None
        }
    }
}

#[path = "observe_bridge_tests.rs"]
#[cfg(test)]
mod observe_bridge_tests;
