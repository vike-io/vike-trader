//! **The HOT-APPLY half of the settings write** (split-plane REQ-7 v2), end-to-end over the REAL
//! wire: a paper node + the real [`vike_tradehub::server`] with a CONTROL key and a
//! [`vike_tradehub::server::SettingsShowSource`] whose `hot` seam is wired to a drain loop
//! standing in for the daemon's summary tick, driven through the REAL client verb
//! ([`vike_tradehub_client::set_setting`]).
//!
//! What is proven:
//!
//! - **A hot key applies live and the wire says so:** a `preferences.log_level` write answers
//!   `restart_required: FALSE`, and the tick-side applier really ran for that key.
//! - **`restart_required: false` is EARNED, not assumed:** the same write on a node whose seam
//!   is present but whose apply FAILS answers `true`; so does a node with NO seam wired at all;
//!   so does a node whose tick never drains (the deadline).
//! - **A policy write is ALWAYS restart-required, confirm or no confirm** — the sealed-policy
//!   doctrine (`docs/decisions/0005-settings-split-by-authority.md`): the typed confirm gates
//!   whether the write happens; it never buys a hot apply.
//! - **A restart-class non-policy key** (`config.tradehub_addr`) answers `true` on a node with a
//!   fully working seam — the classification, not the seam's presence, is what decides.
//!
//! No env mutation, no CWD change, no global subscriber: the settings directory is a
//! `tempfile::TempDir`, the "environment" a local `HashMap`, and the applier a closure. (The
//! REAL `vike_log` applier is unit-tested in `vike-log` itself, which owns the subscriber; this
//! file owns the WIRE contract, which is the half that can lie to an operator.)

use std::collections::HashMap;
use std::net::{SocketAddr, TcpListener};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use vike_run::{build_paper_maker_core, MakerMount, MakerMountConfig};
use vike_tradehub::hot_reload::{self, HotApplyHandle, HotApplyTicker};
use vike_tradehub::{publish, server};
use vike_tradehub_client::{set_setting, NodeKeys};

const TOKEN: &str = "SETTINGS_HOT_TOKEN";
const RESOLUTION_TS: i64 = 3_000_000_000;
const OBSERVE_KEY: &[u8] = b"settings-hot-observe-key";
const CONTROL_KEY: &[u8] = b"settings-hot-control-key";

/// A `preferences.toml` the loader accepts, carrying the keys the hot set names.
const PREFERENCES: &str = "\
# operator taste — hand-edited
log_level = \"info\"
log_file_level = \"trace\"
";

/// The stand-in for `main.rs`'s summary tick: a thread that drains the ticker on a tight cadence
/// and runs an apply for each key, recording what it was asked to apply. Stops on drop.
struct TickThread {
    stop: Arc<AtomicBool>,
    applied: Arc<Mutex<Vec<String>>>,
    handle: Option<thread::JoinHandle<()>>,
}

impl TickThread {
    fn spawn(ticker: HotApplyTicker, verdict: bool) -> TickThread {
        let stop = Arc::new(AtomicBool::new(false));
        let applied = Arc::new(Mutex::new(Vec::new()));
        let handle = {
            let stop = Arc::clone(&stop);
            let applied = Arc::clone(&applied);
            thread::spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    ticker.drain(|key| {
                        applied.lock().expect("applied lock").push(key.to_string());
                        verdict
                    });
                    thread::sleep(Duration::from_millis(5));
                }
            })
        };
        TickThread { stop, applied, handle: Some(handle) }
    }

    fn applied_keys(&self) -> Vec<String> {
        self.applied.lock().expect("applied lock").clone()
    }
}

impl Drop for TickThread {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

/// Build a PAPER node whose settings source carries `hot` and a real settings directory.
fn spawn_node(hot: Option<HotApplyHandle>) -> (MakerMount, SocketAddr, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("temp settings dir");
    std::fs::write(dir.path().join("preferences.toml"), PREFERENCES).expect("write fixture");
    let cfg = MakerMountConfig::polymarket(TOKEN, Some(RESOLUTION_TS));
    let mount = build_paper_maker_core(&cfg);
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
    let addr = listener.local_addr().expect("resolve assigned port");
    let publisher = publish::spawn(mount.handle.snapshot_cell(), None);
    let keys = NodeKeys::new(OBSERVE_KEY.to_vec(), CONTROL_KEY.to_vec());
    let commands = Some(mount.handle.command_sink());
    let settings = server::SettingsShowSource {
        settings_dir: Some(dir.path().to_path_buf()),
        env: HashMap::new(),
        hot,
    };
    thread::spawn(move || {
        let _ = server::serve(
            listener,
            publisher,
            keys,
            commands,
            server::ControlLimitsConfig::default(),
            Some(settings),
            // No REQ-2 datahub advertisement — this suite exercises the settings verbs only.
            None,
        );
    });
    (mount, addr, dir)
}

/// ⚠ **THE v2 PROPERTY**: a hot-safe key applies to the RUNNING node, so the wire answers
/// `restart_required: false` — and the tick really executed the apply for that exact key (a
/// `false` that no applier produced would be the lie this whole seam exists to prevent). The
/// write also lands on disk, so the next boot loads the same value.
#[test]
fn a_hot_log_level_write_applies_live_and_answers_restart_required_false() {
    let (handle, ticker) = hot_reload::hot_apply_channel();
    let tick = TickThread::spawn(ticker, true);
    let (_mount, addr, dir) = spawn_node(Some(handle));

    let restart = set_setting(
        addr,
        CONTROL_KEY,
        "preferences.toml",
        "preferences.log_level",
        "debug",
        None,
        Some("turning the console up to debug"),
    )
    .expect("a valid preferences write lands");

    assert!(
        !restart,
        "`preferences.log_level` is HOT (vike_tradehub::hot_reload::CLASSIFICATION) and the tick \
         applied it — the node must NOT ask for a restart"
    );
    assert_eq!(
        tick.applied_keys(),
        ["preferences.log_level"],
        "the apply ran on the TICK side, for the key that was written — `restart_required: false` \
         is only ever sent because this happened"
    );
    let after = std::fs::read_to_string(dir.path().join("preferences.toml")).expect("read back");
    assert!(after.contains("log_level = \"debug\""), "the write still lands on disk: {after}");
    assert!(
        after.contains("# operator taste — hand-edited"),
        "and it is still the comment-preserving write: {after}"
    );
}

/// The FILE level is the other hot key — and the one whose restart-to-apply gap has a measured
/// cost (`vike_log::file_level_directive`'s 341 GB). Same contract: applied live, no restart.
#[test]
fn a_hot_file_level_write_also_applies_live() {
    let (handle, ticker) = hot_reload::hot_apply_channel();
    let tick = TickThread::spawn(ticker, true);
    let (_mount, addr, _dir) = spawn_node(Some(handle));

    let restart = set_setting(
        addr,
        CONTROL_KEY,
        "preferences.toml",
        "preferences.log_file_level",
        "warn",
        None,
        None,
    )
    .expect("a valid preferences write lands");
    assert!(!restart);
    assert_eq!(tick.applied_keys(), ["preferences.log_file_level"]);
}

/// ⚠ **`restart_required: false` is EARNED.** Same hot key, same seam — but the tick's apply
/// FAILS (the shape a daemon with no reloadable file layer really produces). The write still
/// lands on disk, and the wire tells the truth: restart to apply.
#[test]
fn a_hot_key_whose_apply_fails_still_answers_restart_required() {
    let (handle, ticker) = hot_reload::hot_apply_channel();
    let tick = TickThread::spawn(ticker, false);
    let (_mount, addr, dir) = spawn_node(Some(handle));

    let restart = set_setting(
        addr,
        CONTROL_KEY,
        "preferences.toml",
        "preferences.log_file_level",
        "warn",
        None,
        None,
    )
    .expect("the write itself is valid and lands");
    assert!(restart, "an apply that did not execute must never be reported as applied");
    assert_eq!(tick.applied_keys(), ["preferences.log_file_level"], "the tick was asked");
    let after = std::fs::read_to_string(dir.path().join("preferences.toml")).expect("read back");
    assert!(after.contains("log_file_level = \"warn\""), "the disk write is unaffected: {after}");
}

/// A node with NO hot seam wired (`hot: None` — every pre-v2 daemon, and every fixture that
/// predates the seam) keeps the v1 answer for a hot key: the write lands, restart to apply.
#[test]
fn a_node_without_the_seam_keeps_the_restart_to_apply_answer() {
    let (_mount, addr, _dir) = spawn_node(None);
    let restart = set_setting(
        addr,
        CONTROL_KEY,
        "preferences.toml",
        "preferences.log_level",
        "warn",
        None,
        None,
    )
    .expect("the write lands");
    assert!(restart, "no seam ⇒ nothing applied it ⇒ restart-required, honestly");
}

/// …and a seam NOBODY drains (a stalled or not-yet-started tick) answers restart-required at the
/// deadline rather than hanging the peer or claiming an apply that never ran.
#[test]
fn an_undrained_seam_times_out_to_restart_required() {
    let (handle, ticker) = hot_reload::hot_apply_channel();
    let (_mount, addr, _dir) = spawn_node(Some(handle));
    let started = std::time::Instant::now();
    let restart = set_setting(
        addr,
        CONTROL_KEY,
        "preferences.toml",
        "preferences.log_level",
        "warn",
        None,
        None,
    )
    .expect("the write lands");
    assert!(restart, "an apply nobody performed is restart-required");
    assert!(
        started.elapsed() < hot_reload::HOT_APPLY_DEADLINE + Duration::from_secs(5),
        "the wait is BOUNDED by HOT_APPLY_DEADLINE — a stalled tick must not hang the peer"
    );
    drop(ticker);
}

/// ⚠ **THE SEALED-POLICY DOCTRINE, on the write path**
/// (`docs/decisions/0005-settings-split-by-authority.md`): a `policy.toml` write is
/// restart-required even with a fully working hot seam and the correct typed confirm. The
/// confirm gates whether the write HAPPENS; it never buys a hot apply — a ceiling a peer could
/// lower live is a ceiling the same peer could RAISE live.
#[test]
fn a_policy_write_is_always_restart_required_even_with_the_confirm_and_a_live_seam() {
    let (handle, ticker) = hot_reload::hot_apply_channel();
    let tick = TickThread::spawn(ticker, true);
    let (_mount, addr, dir) = spawn_node(Some(handle));
    let key = "policy.max_notional_per_order";

    // No confirm: refused outright (the v1 contract, unchanged) — and nothing is applied.
    let err = set_setting(addr, CONTROL_KEY, "policy.toml", key, "250", None, None)
        .expect_err("a policy write without confirm is refused");
    assert!(err.to_string().contains("typed confirm"), "{err}");

    // The exact confirm: the write lands, and the answer is STILL restart-required.
    let restart = set_setting(addr, CONTROL_KEY, "policy.toml", key, "250", Some(key), None)
        .expect("the exact confirm lands");
    assert!(restart, "policy is NEVER hot — the sealed-policy doctrine, decision 0005");
    assert!(
        tick.applied_keys().is_empty(),
        "and no apply was even ATTEMPTED for a policy key: the file is refused into the restart \
         class before the classification table is consulted, so no future table row can undo it"
    );
    let written = std::fs::read_to_string(dir.path().join("policy.toml")).expect("read back");
    assert_eq!(written, "max_notional_per_order = 250\n", "the disk write still happened");
}

/// A RESTART-class non-policy key on a node with a fully working seam: the CLASSIFICATION
/// decides, not the seam's presence. `config.tradehub_addr` names an address a listener is
/// already bound to — re-pointing it mid-flight would move nothing but the claim.
#[test]
fn a_restart_class_config_key_is_not_applied_even_with_a_live_seam() {
    let (handle, ticker) = hot_reload::hot_apply_channel();
    let tick = TickThread::spawn(ticker, true);
    let (_mount, addr, _dir) = spawn_node(Some(handle));

    let restart = set_setting(
        addr,
        CONTROL_KEY,
        "config.toml",
        "config.tradehub_addr",
        "127.0.0.1:9100",
        None,
        None,
    )
    .expect("the write lands");
    assert!(restart, "config keys are restart-class");
    assert!(
        tick.applied_keys().is_empty(),
        "a restart-class key must not even reach the tick — the seam is consulted only for a key \
         the classification calls hot-safe"
    );
}
