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

use vike_core::{CommandSink, CoreHandle};
use vike_execution::BarUpdate;
use vike_hub_protocol::auth::{HubKeys, Scope};
use vike_hub_protocol::frame::{read_frame, write_frame};
use vike_hub_protocol::handshake::{HandshakeFrame, HandshakeReply};
use vike_marketdata::test_support::flat_bar_zero_volume;
use vike_tradehub::config::ProfileDaemon;
use vike_tradehub::publish::{self, PublisherHandle};
use vike_tradehub::server;
use vike_tradehub::server::settings::SettingsShowSource;
use vike_tradehub_client::auth;
use vike_tradehub_client::proto::NODE_PROTO_VERSION;
use vike_tradehub_core::{
    MountBuild, StrategyCorePaper, StrategyCorePaperConfig, StrategyCorePaperHalt,
};

/// **The account rows of a box whose every roster venue's DEFAULT account is an active `demo` row stating its fixture
/// symbol** (`vike_secrets::fixture_account_symbol`: the symbol each venue was mounted on before the 2026-11-13
/// ruling; polymarket's states none, its engine being account-wide).
static FIXTURE_ACCOUNTS: std::sync::LazyLock<vike_secrets::account_directory::AccountDirectory> =
    std::sync::LazyLock::new(|| {
        let rows = vike_model::VENUES
            .iter()
            .zip(1..)
            .map(|(venue, id)| {
                vike_secrets::account_row(id, venue, vike_model::AccountTier::Demo, None, true)
            })
            .collect();
        vike_secrets::account_directory::AccountDirectory::from_rows(
            vike_secrets::AccountRows::Known(rows),
            None,
        )
    });

/// A kept-list reader whose venues each list exactly their fixture symbol.
fn fixture_kept(venue: &str, symbol: &str) -> Result<(), String> {
    if vike_secrets::fixture_account_symbol(venue) == Some(symbol) {
        Ok(())
    } else {
        Err(format!("it is not on {venue}'s kept instrument list"))
    }
}

/// **The live box these suites judge a profile against** (`ProfileDaemon::validate_for_live`): [`FIXTURE_ACCOUNTS`]
/// and [`fixture_kept`].
pub(crate) fn live_box() -> vike_tradehub::config::LiveBox<'static> {
    vike_tradehub::config::LiveBox { accounts: &FIXTURE_ACCOUNTS, kept: &fixture_kept }
}

/// The account table a paper box has: ONE active `paper` row for the DEFAULT account of every wired
/// market's venue (since decision 0156 an account no row names does not run).
pub(crate) fn paper_policy() -> vike_tradehub_core::AdapterPolicy {
    crate::local_backend::wired_markets().iter().fold(
        vike_tradehub_core::AdapterPolicy::default(),
        |policy, market| {
            policy.with_account(
                market.venue,
                &vike_model::accounts::account_keys::AccountLabel::Default,
                vike_model::AccountTier::Paper,
            )
        },
    )
}

/// A HALT sentinel path the calling test OWNS and (except in the one test that engages it
/// deliberately, `any_strategy_mount.rs`'s `the_operator_halt_sentinel_stops_a_strategy_file_mount`)
/// never creates. `file` names the member in the temp root's prefix
/// (`vike-tradehub-<file>-<name>-…`), so a leftover root on a box says whose it was; `name` keys
/// the path per test, so a test that DOES create a sentinel cannot disturb a sibling running in the
/// same process.
///
/// ⚠ **A paper MOUNT is HALT-armed by design** (`vike_tradehub_core::StrategyCorePaperHalt`), so a test that stands one
/// up and expects fills inherits the OPERATOR's kill switch off whatever box runs it — the mount
/// would refuse the strategy's opening order and the fill assertions would fail as a
/// strategy-resolution mystery that never mentions halt. MEASURED on the CI box when this seam was first
/// armed (the sentinel still had a `VIKE_HALT_FILE` override then; decision 0099 retired it):
/// `VIKE_HALT_FILE=<an existing file> cargo nextest run` over the CI roster turned 22 tests red
/// while the same command without it ran 7147/7147 green. `crates/vike-tradehub-core/tests/common/mod.rs`
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
/// `crates/vike-tradehub/src/test_strategy_files.rs`'s `folder`.
pub(crate) fn own_sentinel(file: &str, name: &str) -> (tempfile::TempDir, PathBuf) {
    let root = tempfile::Builder::new()
        .prefix(&format!("vike-tradehub-{file}-{name}-"))
        .tempdir()
        .expect("temp sentinel root");
    let path = root.path().join("HALT");
    (root, path)
}

/// [`StrategyCorePaperConfig::default`] with the sentinel pinned to a path the test owns — see
/// [`own_sentinel`]. Everything else is the SHIPPED default, so these tests still exercise the
/// daemon's own mount options.
pub(crate) fn opts_pinned_to(sentinel: &Path) -> StrategyCorePaperConfig {
    assert!(
        !sentinel.exists(),
        "the pinned sentinel must NOT exist, or the mount refuses opening orders for the reason \
         this pinning exists to rule out: {}",
        sentinel.display()
    );
    StrategyCorePaperConfig {
        halt: StrategyCorePaperHalt::Pinned(sentinel.to_path_buf()),
        ..Default::default()
    }
}

/// Resolve a validated profile's rows into the `MountBuild`s `main.rs`'s multi paper arm
/// builds — the same three library calls, in `main`'s order (rows → lowerings → derived
/// controller ids → resolve), each row's file read from a strategies folder holding
/// `crate::strategy_files::standard`'s files, on the paper arm (the live gate off). The folder may go
/// once the rows resolve: a `.rhai` file is compiled at resolve and read no more.
pub(crate) fn resolve_mounts(profile: &ProfileDaemon) -> Vec<MountBuild> {
    let strategies = crate::strategy_files::standard();
    profile
        .mount_rows()
        .into_iter()
        .map(|row| {
            let mut spec = row.to_mount_spec();
            spec.controller_id = Some(row.derived_controller_id());
            let strategy =
                row.resolve_strategy(strategies.path(), false).expect("a validated row resolves");
            MountBuild { strategy, spec }
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
            sides: None,
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
    keys: HubKeys,
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

/// Build a PAPER maker node on `token` ([`vike_tradehub_core::test_support::paper_maker`], a far-future resolution so the
/// A-S horizon is positive) and serve it on loopback with `settings` as its settings source.
///
/// `control_key: None` is an OBSERVE-ONLY node — no control key and no `CommandSink`, for a suite
/// about a read verb. `Some(key)` arms BOTH the control key and the core's `CommandSink`, for a
/// suite that writes.
pub(crate) fn spawn_maker_hub(
    token: &str,
    observe_key: &[u8],
    control_key: Option<&[u8]>,
    settings: Option<SettingsShowSource>,
) -> (StrategyCorePaper, SocketAddr) {
    let keys = HubKeys::new(observe_key.to_vec(), control_key.unwrap_or_default().to_vec());
    spawn_maker_hub_with_keys(token, keys, control_key.is_some(), settings)
}

/// [`spawn_maker_hub`] with the node's keys WHOLE, for a suite whose keys carry more than the pair (the access
/// list of decision 0288). `control` arms the core's `CommandSink`.
pub(crate) fn spawn_maker_hub_with_keys(
    token: &str,
    keys: HubKeys,
    control: bool,
    settings: Option<SettingsShowSource>,
) -> (StrategyCorePaper, SocketAddr) {
    let mount = vike_tradehub_core::test_support::paper_maker::paper_maker_core(
        vike_strategy_lab::makers::spread_maker::SpreadMaker::from_params,
        "polymarket",
        token,
        Some(3_000_000_000),
    );
    let publisher = publish::spawn(mount.handle.snapshot_cell(), None);
    let commands = control.then(|| mount.handle.command_sink());
    let addr = serve_on_loopback(publisher, keys, commands, settings, None);
    (mount, addr)
}

/// Complete the Hello -> Welcome -> Auth(Observe) handshake over a raw stream and return it authed
/// (NOT yet subscribed) — the low-level twin of `TradehubCoreHandle::connect` for the tests that need
/// to control the wire directly.
pub(crate) fn authed_observe_stream(addr: SocketAddr, key: &[u8]) -> TcpStream {
    authed_stream(addr, key, Scope::Read)
}

/// [`authed_observe_stream`] under any `scope`: the stream, authed (NOT subscribed).
pub(crate) fn authed_stream(addr: SocketAddr, key: &[u8], scope: Scope) -> TcpStream {
    match handshake_reply(addr, key, scope) {
        (stream, HandshakeReply::AuthOk { scope: granted, .. }) if granted == scope => stream,
        (_, other) => panic!("expected AuthOk({scope:?}), got {other:?}"),
    }
}

/// The ONE hub handshake by hand: `Hello`, the `Welcome`'s nonce, `Auth` under `scope` signed with `key`;
/// the stream and the handshake's answer to the `Auth` (an `AuthOk`, or the refusal a test reads).
pub(crate) fn handshake_reply(
    addr: SocketAddr,
    key: &[u8],
    scope: Scope,
) -> (TcpStream, HandshakeReply) {
    let mut stream = TcpStream::connect(addr).expect("connect");
    write_frame(&mut stream, &HandshakeFrame::Hello { proto_version: NODE_PROTO_VERSION })
        .expect("hello");
    let nonce = match read_frame::<_, HandshakeReply>(&mut stream).expect("welcome") {
        HandshakeReply::Welcome { nonce: Some(nonce), .. } => nonce,
        other => panic!("expected Welcome, got {other:?}"),
    };
    let mac = auth::sign(key, &nonce, NODE_PROTO_VERSION, scope);
    write_frame(&mut stream, &HandshakeFrame::Auth { scope, mac }).expect("auth");
    let reply = read_frame::<_, HandshakeReply>(&mut stream).expect("auth reply");
    (stream, reply)
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

// ---- The execution stream's FAKE DATAHUB (the `execution_stream_*` members) ---------------------------------------
//
// A key-less datahub that speaks the stream's grammar and keeps what it is sent in memory: per journal id, every
// record and gap, a cursor that rises with each frame (it acknowledges every frame at once), and the order the opens
// came in. It checks the chain as the datahub does: a record that does not chain from the last one asks for a resend
// from the cursor, and what follows is dropped until a record chains from it.

/// What the fake holds of one journal.
#[derive(Debug, Clone, Default)]
pub(crate) struct FakeStream {
    /// `None` while nothing is stored: the one state a record chained from `None` (the start) is taken in.
    pub(crate) stored_through: Option<u64>,
    pub(crate) recorded_ms: Option<i64>,
    pub(crate) records: Vec<vike_datahub_client::execution_stream::ExecutionRecordWire>,
    /// `(after_sequence, resumes_at_sequence)` of every gap.
    pub(crate) gaps: Vec<(u64, u64)>,
}

/// How the fake meets a connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FakeDatahubMode {
    /// Accept it and never read or write a byte: a datahub that is up and stuck.
    Hold,
    /// Serve the stream.
    Serve,
}

/// Each open, in order: the journal id, and where every OTHER journal's cursor stood when it came.
pub(crate) type FakeOpens = Vec<(String, std::collections::HashMap<String, Option<u64>>)>;

#[derive(Default)]
struct FakeDatahubState {
    streams: std::collections::HashMap<String, FakeStream>,
    opens: FakeOpens,
    held: Vec<TcpStream>,
}

pub(crate) struct FakeDatahub {
    pub(crate) addr: SocketAddr,
    mode: std::sync::Arc<std::sync::Mutex<FakeDatahubMode>>,
    state: std::sync::Arc<std::sync::Mutex<FakeDatahubState>>,
}

/// **A datahub that is DOWN on a port this test HOLDS**: every connection is closed unanswered, so a client fails
/// and redials as against a datahub that is not there, until [`Self::up`] hands the very listener to the datahub
/// that comes up. A port bound and released instead could be taken by another process on a busy box before the
/// datahub re-bound it (the 2026-11-19 lane flake of `execution_stream_real_sink`).
pub(crate) struct DatahubDown {
    pub(crate) addr: SocketAddr,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    held: thread::JoinHandle<TcpListener>,
}

impl DatahubDown {
    pub(crate) fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind the down datahub's port");
        let addr = listener.local_addr().expect("addr");
        listener.set_nonblocking(true).expect("a non-blocking accept");
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stopping = std::sync::Arc::clone(&stop);
        let held = thread::spawn(move || {
            while !stopping.load(std::sync::atomic::Ordering::Acquire) {
                match listener.accept() {
                    Ok((socket, _)) => drop(socket),
                    Err(_) => thread::sleep(std::time::Duration::from_millis(5)),
                }
            }
            listener.set_nonblocking(false).expect("a blocking accept again");
            listener
        });
        DatahubDown { addr, stop, held }
    }

    /// The datahub comes up: the listener on [`Self::addr`], for its server to accept on.
    pub(crate) fn up(self) -> TcpListener {
        self.stop.store(true, std::sync::atomic::Ordering::Release);
        self.held.join().expect("the down datahub's thread")
    }
}

impl FakeDatahub {
    /// Listen on `listener` (an ephemeral port for `None`) and meet every connection in `mode`.
    pub(crate) fn start(listener: Option<TcpListener>, mode: FakeDatahubMode) -> Self {
        let listener = listener
            .unwrap_or_else(|| TcpListener::bind("127.0.0.1:0").expect("bind the fake datahub"));
        let fake = FakeDatahub {
            addr: listener.local_addr().expect("addr"),
            mode: std::sync::Arc::new(std::sync::Mutex::new(mode)),
            state: std::sync::Arc::default(),
        };
        let (mode, state) = (fake.mode.clone(), fake.state.clone());
        thread::Builder::new()
            .name("fake-datahub".into())
            .spawn(move || {
                for socket in listener.incoming().flatten() {
                    if *mode.lock().unwrap() == FakeDatahubMode::Hold {
                        state.lock().unwrap().held.push(socket);
                        continue;
                    }
                    let state = state.clone();
                    let _ = thread::Builder::new()
                        .name("fake-datahub-stream".into())
                        .spawn(move || fake_datahub_session(socket, &state));
                }
            })
            .expect("spawn the fake datahub");
        fake
    }

    /// Serve from now on; the connections it held are closed.
    pub(crate) fn serve(&self) {
        *self.mode.lock().unwrap() = FakeDatahubMode::Serve;
        self.state.lock().unwrap().held.clear();
    }

    /// The cursor this datahub answers `journal_id`'s next open with, as if it had stored that much.
    pub(crate) fn set_cursor(
        &self,
        journal_id: &str,
        stored_through: u64,
        recorded_ms: Option<i64>,
    ) {
        let mut state = self.state.lock().unwrap();
        let stream = state.streams.entry(journal_id.to_string()).or_default();
        stream.stored_through = Some(stored_through);
        stream.recorded_ms = recorded_ms;
    }

    pub(crate) fn stream(&self, journal_id: &str) -> FakeStream {
        self.state.lock().unwrap().streams.get(journal_id).cloned().unwrap_or_default()
    }

    pub(crate) fn opens(&self) -> FakeOpens {
        self.state.lock().unwrap().opens.clone()
    }

    /// Wait (20 s at most) until `done` holds of what the fake was sent.
    pub(crate) fn wait_until(&self, what: &str, done: impl Fn(&FakeDatahub) -> bool) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        while !done(self) {
            assert!(std::time::Instant::now() < deadline, "{what}: opens {:?}", self.opens());
            thread::sleep(std::time::Duration::from_millis(10));
        }
    }
}

fn fake_datahub_session(mut socket: TcpStream, state: &std::sync::Mutex<FakeDatahubState>) {
    use vike_datahub_client::execution_stream::{ExecutionStreamDown, ExecutionStreamUp};
    use vike_datahub_client::proto::{FEATURE_EXECUTION_STREAM, Request, Response};
    if !matches!(read_frame::<_, HandshakeFrame>(&mut socket), Ok(HandshakeFrame::Hello { .. })) {
        return;
    }
    let welcome = HandshakeReply::Welcome {
        proto_version: vike_datahub_client::PROTO_VERSION,
        features: vec![FEATURE_EXECUTION_STREAM.to_string()],
        nonce: None,
    };
    if write_frame(&mut socket, &welcome).is_err() {
        return;
    }
    let Ok(Request::ExecutionStreamOpen { journal_id, .. }) = read_frame::<_, Request>(&mut socket)
    else {
        return;
    };
    let (mut stored, recorded_ms) = {
        let mut state = state.lock().unwrap();
        let others = state
            .streams
            .iter()
            .filter(|(id, _)| **id != journal_id)
            .map(|(id, s)| (id.clone(), s.stored_through))
            .collect();
        state.opens.push((journal_id.clone(), others));
        let stream = state.streams.entry(journal_id.clone()).or_default();
        (stream.stored_through, stream.recorded_ms)
    };
    let opened = Response::ExecutionStreamOpened {
        stored_through: stored,
        stored_through_recorded_ms: recorded_ms,
        heartbeat_ms: 30_000,
    };
    if write_frame(&mut socket, &opened).is_err() {
        return;
    }
    let mut expected = stored;
    let mut awaiting_resend = false;
    while let Ok(up) = read_frame::<_, ExecutionStreamUp>(&mut socket) {
        let mut down = None;
        {
            let mut state = state.lock().unwrap();
            let stream = state.streams.get_mut(&journal_id).expect("opened");
            match up {
                ExecutionStreamUp::Records(records) => {
                    for record in records {
                        if record.previous_sequence != expected {
                            if !awaiting_resend {
                                awaiting_resend = true;
                                expected = stored;
                                down = Some(ExecutionStreamDown::Resend { after_sequence: stored });
                            }
                            continue;
                        }
                        awaiting_resend = false;
                        expected = Some(record.sequence);
                        stored = Some(record.sequence);
                        stream.stored_through = stored;
                        stream.recorded_ms = Some(record.recorded_ms);
                        stream.records.push(record);
                    }
                    if down.is_none() {
                        down = stored.map(|through_sequence| ExecutionStreamDown::Stored {
                            through_sequence,
                        });
                    }
                }
                ExecutionStreamUp::Gap { after_sequence, resumes_at_sequence, .. } => {
                    stream.gaps.push((after_sequence, resumes_at_sequence));
                    stored = Some(resumes_at_sequence - 1);
                    stream.stored_through = stored;
                    down = stored
                        .map(|through_sequence| ExecutionStreamDown::Stored { through_sequence });
                }
                ExecutionStreamUp::Heartbeat { .. } => {}
            }
        }
        if let Some(down) = down
            && write_frame(&mut socket, &down).is_err()
        {
            return;
        }
    }
}

/// **The execution stream's publisher over `dir`'s journal**, to the datahub at `addr`, with no key and short waits;
/// `settings` names removed mounts and keeps the order client id sessions.
pub(crate) fn start_execution_stream(
    dir: &Path,
    watch: vike_journal::JournalDurableWatch,
    addr: SocketAddr,
    settings: Option<PathBuf>,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
) -> vike_tradehub::execution_stream::ExecutionStreamPublisher {
    use std::time::Duration;
    let source = vike_tradehub::execution_stream::ExecutionStreamSource {
        journal_dir: dir.to_path_buf(),
        watch,
        datahub_addr: addr.to_string(),
        tradehub: "test".into(),
        keys: std::sync::Arc::new(|| None),
        settings_dir: settings,
        timings: vike_tradehub::execution_stream::ExecutionStreamTimings {
            retry_first: Duration::from_millis(20),
            retry_max: Duration::from_millis(200),
            unsupported_wait: Duration::from_millis(200),
            ack_poll: Duration::from_millis(20),
            write_timeout: Duration::from_secs(5),
            final_send: Duration::from_millis(500),
        },
    };
    vike_tradehub::execution_stream::ExecutionStreamPublisher::spawn(source, stop)
        .expect("spawn the publisher")
}

/// The id the publisher gave the journal in `dir` (its stream file), once it has.
pub(crate) fn journal_id_of(dir: &Path) -> String {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    loop {
        if let Some(file) =
            vike_journal::JournalExecutionStream::load(dir).expect("the stream file reads")
        {
            return file.journal_id;
        }
        assert!(std::time::Instant::now() < deadline, "the publisher never wrote the stream file");
        thread::sleep(std::time::Duration::from_millis(10));
    }
}

/// A fill on binance/BTCUSDT of `qty` at 100, venue time `timestamp`.
pub(crate) fn binance_fill(
    order_client_id: &str,
    trade_id: &str,
    qty: f64,
    timestamp: i64,
) -> vike_model::events::FillEvent {
    vike_model::events::FillEvent {
        trade_id: vike_model::events::TradeId::new(trade_id).expect("non-empty"),
        order_client_id: order_client_id.into(),
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        side: 1,
        last_qty: qty,
        last_price: 100.0,
        commission: 0.01,
        commission_asset: "USDT".into(),
        liquidity_side: "taker".into(),
        timestamp,
        mark_price: None,
        position_side: vike_model::events::PositionSide::Both,
    }
}

/// A self-contained execution report of OURS: a fill of `order_client_id` on binance/BTCUSDT's default account.
pub(crate) fn fill_report(
    order_client_id: &str,
    trade_id: &str,
) -> vike_model::orders::ExecutionReport {
    let fill = binance_fill(order_client_id, trade_id, 1.0, 7);
    vike_model::orders::ExecutionReport {
        caused_by_sequence: 0,
        at_ms: 7,
        ownership: vike_model::orders::ExecutionOwnership::Ours,
        account: Some("DEFAULT".into()),
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        order_client_id: order_client_id.into(),
        venue_order_id: None,
        mount_id: None,
        strategy_code: None,
        order: None,
        fill: Some(vike_model::orders::ExecutionReportFill::of_fill(&fill)),
        fee_correction: None,
        reason: None,
    }
}

/// A journal at `dir` with short segments, open for writing (the test is its core).
pub(crate) fn open_journal(dir: &Path) -> vike_journal::Journal {
    vike_journal::Journal::open(
        dir,
        vike_journal::JournalFileConfig { segment_bytes: 4096, flush_every: 1 },
    )
    .expect("open the journal")
}
