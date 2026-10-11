//! The `daemon` binary's shared test helpers: ONE copy of each helper its members used to carry
//! verbatim, or differing only by a constant that is now a parameter.
//!
//! Declared once, in `tests/daemon.rs`. A member names what it uses with
//! `use crate::support::{…}`; a CHILD of a member (`venue_routing/`, `multi_mount_profile/`) reaches
//! the same names through its parent's `use super::*`.
//!
//! The binary's rule holds here as everywhere in it: nothing in this file mutates process-global
//! state — no env, no CWD, no `tracing` subscriber — because a plain
//! `cargo test -p vike-tradehub --test daemon` runs every member's tests as THREADS of one process.

use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use vike_core::{CommandSink, CoreHandle};
use vike_exec::BarUpdate;
use vike_marketdata::test_support::flat_bar_zero_volume;
use vike_mount::{
    MakerMount, MakerMountConfig, PaperHalt, PaperMountOpts, StrategyMountSpec,
    build_paper_maker_core,
};
use vike_tradehub::config::DaemonProfile;
use vike_tradehub::publish::{self, PublisherHandle};
use vike_tradehub::server;
use vike_tradehub::server::settings::SettingsShowSource;
use vike_tradehub_client::proto::{
    NODE_PROTO_VERSION, Request, Response, Scope, read_frame, write_frame,
};
use vike_tradehub_client::{NodeKeys, auth};

// Same as `crates/vike-mount/tests/common/mod.rs`'s `wait_until` (`secs`, 10 ms poll); NOT
// `crates/vike-cli/tests/common/mod.rs`'s (20 ms) nor the `Duration`-taking copies.
/// Poll `cond` every 10 ms for up to `secs`, returning whether it became true. The core folds on
/// its own thread and publishes coalesced snapshots, and the publisher and a client fan the push
/// out on more, so nothing in this binary may assume a publish has already landed.
pub(crate) fn wait_until(secs: u64, mut cond: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + Duration::from_secs(secs);
    loop {
        if cond() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        thread::sleep(Duration::from_millis(10));
    }
}

/// A HALT sentinel path the calling test OWNS and (except in the one test that engages it
/// deliberately, `any_strategy_mount.rs`'s `the_operator_halt_sentinel_stops_a_registry_strategy_mount`)
/// never creates. `file` names the member in the temp root's prefix
/// (`vike-tradehub-<file>-<name>-…`), so a leftover root on a box says whose it was; `name` keys
/// the path per test, so a test that DOES create a sentinel cannot disturb a sibling running in the
/// same process.
///
/// ⚠ **A paper MOUNT is HALT-armed by design** (`vike_mount::PaperHalt`), so a test that stands one
/// up and expects fills inherits the OPERATOR's kill switch off whatever box runs it — the mount
/// would refuse the strategy's opening order and the fill assertions would fail as a
/// strategy-resolution mystery that never mentions halt. MEASURED on the CI box when this seam was first
/// armed (the sentinel still had a `VIKE_HALT_FILE` override then; decision 0099 retired it):
/// `VIKE_HALT_FILE=<an existing file> cargo nextest run` over the CI roster turned 22 tests red
/// while the same command without it ran 7147/7147 green. `crates/vike-mount/tests/common/mod.rs`
/// carries the full argument; this is the same cure at the daemon's own mount.
///
/// ⚠ Returns the owning `TempDir` ALONGSIDE the path, and the caller must BIND it for as long as
/// the mount lives — the paper client consults the pinned path on every opening order. This is
/// `crates/vike-core/src/scratch.rs`'s `Scratch::reserved` shape: the ROOT exists and is owned, the
/// `HALT` file inside it does not, and whatever a test writes there is removed when the guard
/// drops. That makes [`opts_pinned_to`]'s "must NOT exist" true BY CONSTRUCTION, not by hope.
///
/// Every copy of this helper used to be `env::temp_dir().join(format!("…-{pid}-{name}"))`, and
/// both halves of the pid defect applied: a test that ENGAGED a sentinel removed it by hand only on
/// its success path, so a failing run left a `HALT` behind under a pid-keyed name that a later run
/// drawing the same pid would find already on disk, halting a mount that expects fills. The
/// measured numbers behind the family (44,840 leaked directories under the CI box's `/tmp` on
/// 2026-08-25, and the two-users/one-pid `PermissionDenied` flake) are in
/// `crates/vike-tradehub/src/config/tests/rhai.rs`'s `own_script`.
pub(crate) fn own_sentinel(file: &str, name: &str) -> (tempfile::TempDir, PathBuf) {
    let root = tempfile::Builder::new()
        .prefix(&format!("vike-tradehub-{file}-{name}-"))
        .tempdir()
        .expect("temp sentinel root");
    let path = root.path().join("HALT");
    (root, path)
}

/// [`PaperMountOpts::default`] with the sentinel pinned to a path the test owns — see
/// [`own_sentinel`]. Everything else is the SHIPPED default, so these tests still exercise the
/// daemon's own mount options.
pub(crate) fn opts_pinned_to(sentinel: &Path) -> PaperMountOpts {
    assert!(
        !sentinel.exists(),
        "the pinned sentinel must NOT exist, or the mount refuses opening orders for the reason \
         this pinning exists to rule out: {}",
        sentinel.display()
    );
    PaperMountOpts { halt: PaperHalt::Pinned(sentinel.to_path_buf()), ..Default::default() }
}

/// Resolve a validated profile's rows into the `StrategyMountSpec`s `main.rs`'s multi paper arm
/// builds — the same three library calls, in `main`'s order (rows → lowerings → derived
/// controller ids → resolve).
pub(crate) fn resolve_mounts(profile: &DaemonProfile) -> Vec<StrategyMountSpec> {
    profile
        .mount_rows()
        .into_iter()
        .map(|row| {
            let cfg = row.to_mount_config();
            let mut spec = row.to_mount_spec();
            spec.controller_id = Some(row.derived_controller_id());
            let strategy = row.resolve_strategy(&cfg).expect("a validated row resolves");
            StrategyMountSpec { strategy, spec }
        })
        .collect()
}

/// Drive two CLOSED `1m` bars (at 0.40 then 0.50) onto `(venue, symbol)`'s own bar lane. The first
/// reaches `on_bar` (the runtime stamps the series symbol onto it, which is why a symbol-inferring
/// strategy routes correctly live) and a `buy_hold`-shaped strategy submits ONE market order; the
/// paper book fills a market order at the NEXT bar's open — hence bar two, stamped `120_000`.
pub(crate) fn drive_two_bars(handle: &CoreHandle, venue: &str, symbol: &str) {
    let bars = handle.bar_sender();
    for (i, px) in [0.40_f64, 0.50].into_iter().enumerate() {
        bars.close(BarUpdate {
            venue: venue.to_string(),
            symbol: symbol.to_string(),
            interval: "1m".to_string(),
            bar: flat_bar_zero_volume(60_000 * (i as i64 + 1), px),
        })
        .expect("the core is alive");
    }
}

/// Bind an ephemeral `127.0.0.1:0` listener and run the REAL [`server::serve`] on it on a detached
/// thread, with the default edge limits (no size cap, default rate — the caller-owned config seam,
/// audit F13). Returns the assigned address.
///
/// The thread OWNS `publisher`, which keeps its poll thread alive for the test's lifetime; a test
/// that also reads the publisher passes a clone. `commands` is the core's `CommandSink` for a
/// control-enabled node and `None` for an observe-only one. No `AccountAdminSource`, ever: the
/// account capability is an ABSENCE on every box that has not DECLARED a barrier, which is every
/// fixture here and every shipped box today.
pub(crate) fn serve_on_loopback(
    publisher: PublisherHandle,
    keys: NodeKeys,
    commands: Option<CommandSink>,
    settings: Option<SettingsShowSource>,
    datahub_advertise: Option<String>,
) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
    let addr = listener.local_addr().expect("resolve assigned port");
    thread::spawn(move || {
        let _ = server::serve(
            listener,
            publisher,
            keys,
            commands,
            server::control::ControlLimitsConfig::default(),
            settings,
            None,
            datahub_advertise,
        );
    });
    addr
}

/// Build a PAPER maker node on `token` ([`build_paper_maker_core`], a far-future resolution so the
/// A-S horizon is positive) and serve it on loopback with `settings` as its settings source.
///
/// `control_key: None` is an OBSERVE-ONLY node — no control key and no `CommandSink`, for a suite
/// about a read verb. `Some(key)` arms BOTH the control key and the core's `CommandSink`, for a
/// suite that writes.
pub(crate) fn spawn_maker_node(
    token: &str,
    observe_key: &[u8],
    control_key: Option<&[u8]>,
    settings: Option<SettingsShowSource>,
) -> (MakerMount, SocketAddr) {
    let cfg = MakerMountConfig::outcome_token("polymarket", token, Some(3_000_000_000));
    let mount = build_paper_maker_core(&cfg);
    let publisher = publish::spawn(mount.handle.snapshot_cell(), None);
    let keys = NodeKeys::new(observe_key.to_vec(), control_key.unwrap_or_default().to_vec());
    let commands = control_key.map(|_| mount.handle.command_sink());
    let addr = serve_on_loopback(publisher, keys, commands, settings, None);
    (mount, addr)
}

/// Complete the Hello -> Welcome -> Auth(Observe) handshake over a raw stream and return it authed
/// (NOT yet subscribed) — the low-level twin of `RemoteCoreHandle::connect` for the tests that need
/// to control the wire directly.
pub(crate) fn authed_observe_stream(addr: SocketAddr, key: &[u8]) -> TcpStream {
    let mut stream = TcpStream::connect(addr).expect("connect");
    write_frame(&mut stream, &Request::Hello { proto_version: NODE_PROTO_VERSION }).expect("hello");
    let nonce = match read_frame::<_, Response>(&mut stream).expect("welcome") {
        Response::Welcome { nonce, .. } => nonce,
        other => panic!("expected Welcome, got {other:?}"),
    };
    let mac = auth::sign(key, &nonce, NODE_PROTO_VERSION, Scope::Read);
    write_frame(&mut stream, &Request::Auth { scope: Scope::Read, mac }).expect("auth");
    match read_frame::<_, Response>(&mut stream).expect("authok") {
        Response::AuthOk { scope: Scope::Read } => {}
        other => panic!("expected AuthOk(Observe), got {other:?}"),
    }
    stream
}

/// Settings rows in ONE `section`, as `(key, raw JSON value)` pairs — the 0086 database-native
/// twin of the settings files these suites used to plant (there is no settings file any more).
pub(crate) fn settings_rows(section: &str, rows: &[(&str, &str)]) -> vike_secrets::StoredSettings {
    vike_secrets::StoredSettings {
        settings: rows
            .iter()
            .map(|(key, value)| vike_secrets::SettingRow {
                section: section.to_string(),
                key: key.to_string(),
                value: value.to_string(),
            })
            .collect(),
        ..Default::default()
    }
}

/// The raw stored value of `section.key`, read back off the settings DATABASE under `dir` (0086:
/// a write lands in a row, so there is no file to read back).
pub(crate) fn stored_value(dir: &Path, section: &str, key: &str) -> String {
    let source = vike_secrets::read_settings_in(dir).expect("read the settings database");
    let rows = &source.rows().expect("rows").settings;
    rows.iter()
        .find(|r| r.section == section && r.key == key)
        .unwrap_or_else(|| panic!("the {section}.{key} row"))
        .value
        .clone()
}
