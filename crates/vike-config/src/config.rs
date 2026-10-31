//! [`Config`] — DEPLOYMENT settings: where things live on this box. Rows, plus a CLI layer.
//!
//! The distinguishing question is "would a second machine running the same strategy need a
//! different value?". Store roots, log directories and listen addresses all answer yes: each box
//! writes its own `config.*` rows into its own settings database, and an operator may override a
//! path or a port with a flag for one run. None of them is a risk decision.
//!
//! Contrast [`crate::Preferences`], whose values would be the SAME on the second machine (they
//! express taste), and [`crate::Policy`], which stops at the database layer on purpose.
//!
//! No key here is read from the process environment
//! (`docs/decisions/0111-no-setting-lives-in-the-environment-or-a-toml-file.md`): each field's doc
//! names the variable it replaced, and [`crate::REMOVED_ENV`] refuses that variable at startup. The
//! one exception is a BOOTSTRAP read outside this crate: the logger reads `VIKE_LOG_DIR` itself,
//! ahead of [`Config::log_dir`], because a log directory has to work when the database is absent.
//!
//! `VIKE_HIST_STORE` had FIVE rows in `vike_config::settings::SETTINGS` — vike-app, vike-backtest,
//! vike-backfill, vike-datahub and vike-studio each read it with a *different* fallback chain. One
//! field with one default is the fix; the divergent fallbacks became the callers' business.

use serde::{Deserialize, Serialize};
use std::ops::RangeInclusive;
use std::path::{Path, PathBuf};
use vike_venues::venue_restore::MAX_RECONCILE_STARTUP_DELAY_MS;

use crate::error::{ConfigError, stale_row_removal};
use crate::layers::{CliOverride, CliOverrides};

/// The `vike-datahub` default listen address.
pub const DEFAULT_DATAHUB_ADDR: &str = "127.0.0.1:7878";

/// [`Config::reconcile_policy`]'s words — the four reconcile policies, ONE spelling each, as
/// `vike_exec::recon::ReconPolicy::from_policy_name` accepts them. This crate cannot name that one
/// (it depends on `vike-model` and `vike-secrets` only), so the words are data here and
/// `crates/vike-tradehub/src/reconcile_config_tests.rs`'s
/// `every_reconcile_policy_word_names_a_real_policy` holds the two spellings equal.
///
/// ⚠ A word outside this list is REFUSED, at the write and at the load, and that is the point of
/// the list: the variable's reader falls back to `hybrid` — which auto-applies `PositionDrift` —
/// for a value it does not recognise (`parse_policy`), so a typo'd row must never reach it.
pub const RECONCILE_POLICIES: [&str; 4] =
    ["hybrid", "synthesize", "quarantine", "external-quarantine"];

/// [`Config::paper_slippage`]'s ceiling, as a FRACTION (`0.05` is 500 basis points). A paper fill
/// slips by this much at most: past it the row is a typo (`2` for `0.0002`), not a market.
pub const MAX_PAPER_SLIPPAGE: f64 = 0.05;

/// [`Config::preflight_disk_warn_mib`]'s default, in MiB (5 GiB). This crate cannot name the
/// reader's own byte constant (`vike_tradehub_core::preflight::DEFAULT_DISK_WARN_BYTES`: it depends on
/// `vike-model` and `vike-secrets` only), so the MiB figure is data here and
/// `crates/vike-tradehub-core/src/preflight_tests/disk_and_network.rs`'s
/// `the_disk_row_defaults_are_the_compiled_floors` holds the two spellings equal. [`Config::apply`]
/// needs it: a row that moves one floor is judged against the OTHER at its default.
pub const DEFAULT_PREFLIGHT_DISK_WARN_MIB: u32 = 5_120;

/// [`Config::preflight_disk_fail_mib`]'s default, in MiB (1 GiB); see
/// [`DEFAULT_PREFLIGHT_DISK_WARN_MIB`] for who holds it equal to the reader's own constant.
pub const DEFAULT_PREFLIGHT_DISK_FAIL_MIB: u32 = 1_024;

/// The ceiling of both preflight disk floors, in MiB (10 TiB): a larger figure is a unit slip
/// (bytes written where MiB are read), not a disk.
pub const MAX_PREFLIGHT_DISK_MIB: u32 = 10_485_760;

/// [`Config::inflight_confirm_ms`]'s floor when it is on: the core re-queries each stuck order at
/// this cadence, one venue round trip per stuck order (two on bybit), so a sub-second cadence is a
/// request storm aimed at a venue that is already slow to answer.
pub const MIN_INFLIGHT_CONFIRM_MS: u32 = 1_000;

/// [`Config::inflight_confirm_ms`]'s ceiling, one minute: the fast rung acts only on orders younger
/// than the effective `submit_ack_timeout` (30 s at the daemon's default, the run profile's to
/// change), so a cadence past a minute is slower than any timeout worth running. A value at or above
/// the effective timeout is still ACCEPTED — the timeout is not this crate's to see — and the daemon
/// warns about it at startup.
pub const MAX_INFLIGHT_CONFIRM_MS: u32 = 60_000;

/// [`Config::strategy_state_save_ms`]'s floor when it is on: every save is an atomic, fsynced file
/// write per stateful mount, run by the core thread at a drain-loop boundary, so a faster cadence
/// spends the single writer's time on disk waits.
pub const MIN_STRATEGY_STATE_SAVE_MS: u32 = 1_000;

/// [`Config::strategy_state_save_ms`]'s ceiling, one hour: past it a crash loses more than a
/// session usually runs between restarts, and the save at a clean shutdown already covers that.
pub const MAX_STRATEGY_STATE_SAVE_MS: u32 = 3_600_000;

// -- The data plane's per-box tuning rows (`vike-datahub`). Each range below is RESTATED here, because
//    this crate cannot name the higher-layer constant it protects, and each is held to that
//    constant by a pin test in the crate that owns it (the `RECONCILE_POLICIES` shape): a number
//    that moves on one side reddens the other. A row outside its range is REFUSED, at the write
//    and at the load, and never clamped.

/// [`Config::datahub_catalog_ttl_secs`]'s range, in seconds. The floor restates the venue-catalog
/// lane's own budget arithmetic (`crates/vike-datahub/src/catalog.rs`'s `CATALOG_TTL_FLOOR`: four
/// minutes of memo per fetch the per-venue burst admits); the ceiling is one week.
pub const DATAHUB_CATALOG_TTL_SECS: RangeInclusive<u32> = 480..=604_800;

/// [`Config::recorder_silent_secs`]'s range when it is not `0`, in seconds. The ceiling restates
/// the rate judgement's window (`crates/vike-recorder/src/liveness/slow.rs`'s `CADENCE_WINDOW_MS`,
/// 900 s), which is sized as three times the default threshold so the recency watchdog catches
/// anything it catches first.
pub const RECORDER_SILENT_SECS: RangeInclusive<u32> = 60..=900;

/// [`Config::recorder_tick_secs`]'s range, in seconds. The floor keeps the resolver off the venue
/// directory APIs (the flag already refuses `0`); what a long tick costs is on the field.
pub const RECORDER_TICK_SECS: RangeInclusive<u32> = 5..=300;

/// [`Config::recorder_flush_max_rows`]'s range, in rows.
pub const RECORDER_FLUSH_MAX_ROWS: RangeInclusive<u32> = 500..=50_000;

/// [`Config::recorder_flush_max_age_secs`]'s range, in seconds.
pub const RECORDER_FLUSH_MAX_AGE_SECS: RangeInclusive<u32> = 5..=300;

/// [`Config::datahub_chain_cadence_secs`]'s range, in seconds. The floor is the options lane's REST
/// poll (`crates/vike-datahub/src/options/deribit.rs`'s `POLL_INTERVAL`): a bucket narrower than a
/// poll records every poll.
pub const DATAHUB_CHAIN_CADENCE_SECS: RangeInclusive<u32> = 12..=3600;

/// [`Config::datahub_options_poll_secs`]'s range, in seconds.
pub const DATAHUB_OPTIONS_POLL_SECS: RangeInclusive<u32> = 5..=300;

/// The shortest [`Config::node_idle_read_timeout_ms`] a row may name, in milliseconds.
///
/// ⚠ A RESTATEMENT, not a source. The bound is `vike_hub_proto::limits::SERVER_IDLE_READ_TIMEOUT_MIN`
/// (the argument for the number is on that constant), which this crate cannot name — it depends on
/// `vike-model` and `vike-secrets` only — so the number is data here and each daemon that reads the
/// row holds the two equal:
/// `crates/vike-datahub/tests/auth_roundtrip/connection_timings.rs`'s
/// `the_idle_read_timeout_range_is_the_one_vike_config_restates` and
/// `crates/vike-backtesthub/tests/idle_read_timeout.rs`'s twin of it. The `RECONCILE_POLICIES`
/// precedent.
pub const NODE_IDLE_READ_TIMEOUT_MIN_MS: u32 = 120_000;

/// The longest [`Config::node_idle_read_timeout_ms`] a row may name, in milliseconds. A restatement
/// of `vike_hub_proto::limits::SERVER_IDLE_READ_TIMEOUT_MAX`, held equal the way
/// [`NODE_IDLE_READ_TIMEOUT_MIN_MS`] is.
pub const NODE_IDLE_READ_TIMEOUT_MAX_MS: u32 = 3_600_000;

/// The shortest [`Config::md_heartbeat_ms`] a row may name, in milliseconds. A restatement of the
/// floor the compile-time assert on `vike_datahub_client::market::MD_HEARTBEAT` states (a positive
/// period: zero would write continuously), held equal by
/// `crates/vike-datahub/tests/auth_roundtrip/connection_timings.rs`'s
/// `the_heartbeat_range_is_the_one_vike_config_restates`.
pub const MD_HEARTBEAT_MIN_MS: u32 = 1_000;

/// The longest [`Config::md_heartbeat_ms`] a row may name, in milliseconds: the same assert's ceiling,
/// above which a heartbeat is too rare to be a liveness signal. See [`MD_HEARTBEAT_MIN_MS`].
pub const MD_HEARTBEAT_MAX_MS: u32 = 60_000;

/// The backtesthub's default address — where `vike-backend backtesthub --addr` binds, and where a
/// client of the backtest verbs (`vike-cli research study` today) dials when nothing else answers.
///
/// ⚠ **7880, and the two digits are the whole point: 7879 is TAKEN, by the live order-signing
/// daemon.** [`Config::node_addr`]'s own doc names it ("the daemon binds `127.0.0.1:7879` on its
/// own box") — the process that signs orders. A compute
/// client defaulting there would open a connection to the TRADING socket and speak a protocol it
/// does not serve. The data server has 7878, the trading daemon 7879, so the backtesthub takes
/// 7880. §0 of `docs/superpowers/specs/2026-09-09-datahub-market-data-wire-design.md` (ruling 7,
/// as corrected by the owner) is the authority.
///
/// ⚠ `crates/vike-config/src/settings/rows/config.rs`'s `VIKE_BACKTEST_ADDR` row and
/// `deploy/vike-backtesthub.service`'s `Environment=` line both RESTATE the number as text rather
/// than reading this constant, and a restated default that contradicts the constant is the
/// settings-registry failure the workspace already gates for elsewhere. The unit's line is the
/// sharper of the two — it is what a deployed box actually binds, and pointing the backtesthub
/// at 7879 on a box that also runs the trading node is the collision this constant exists to
/// avoid.
///
/// It is the LAST rung of the ladder, never the only one: `--addr <v>` →
/// [`Config::backtest_addr`] → this. On a deployed box the unit's `Environment=` line (the
/// daemon's own half of ruling 7) sets the address once and nobody types it again.
pub const DEFAULT_BACKTEST_ADDR: &str = "127.0.0.1:7880";

/// The gatewayhub's default address — where `vike-gatewayhub` binds on this box, and where the
/// datahub (and, in a later wave, the tradehub) dials it when no row says otherwise.
///
/// ⚠ **7883, and it is the next free port, not a pattern.** 7878 is the data server, 7879 the
/// trading daemon, 7880 the backtesthub, 7881 the strategy-builder (`--port 7881` in its unit) and
/// 7882 is the address the docs use as a backtest-address EXAMPLE. A default equal to any of them
/// would aim a client at a process that does not serve this protocol (the 7879 mistake
/// [`DEFAULT_BACKTEST_ADDR`] records, one plane over). `config_tests.rs`'s
/// `the_gatewayhub_default_is_none_of_its_neighbours` pins the separation.
///
/// ⚠ **The daemon refuses a NON-LOOPBACK value outright**, with no opt-in flag and no key that
/// lifts it (`crates/vike-gatewayhub/src/gatewayhub_cli/boot.rs`'s `check_bind`): it holds every
/// venue connection and, in a later wave, the order path, and its handshake is plaintext. This
/// constant is the LAST rung of the ladder `config.gatewayhub_addr` → this, never the only one.
pub const DEFAULT_GATEWAYHUB_ADDR: &str = "127.0.0.1:7883";

/// [`Config::gatewayhub_connect_wait_ms`]'s default, in milliseconds: how long a tradehub that starts
/// while the gatewayhub is down keeps asking before it stops its `demo`/`live` accounts with
/// `ArmingBlock::GatewayUnavailable` (decision 0164, owner ruling D7). A minute covers a gatewayhub
/// that is restarting beside it (`Restart=on-failure` with `RestartSec=5`) without holding a start
/// open for a daemon that is not coming; it is asked only when an account is active at a non-paper
/// tier, so a paper-only box never waits.
pub const DEFAULT_GATEWAYHUB_CONNECT_WAIT_MS: u32 = 60_000;

/// [`Config::gatewayhub_connect_wait_ms`]'s floor, in milliseconds. One second is "ask once and
/// give up almost at once": below it the wait is not a wait, and zero would make the row mean two
/// things (no wait, and an unset default).
pub const GATEWAYHUB_CONNECT_WAIT_MIN_MS: u32 = 1_000;

/// [`Config::gatewayhub_connect_wait_ms`]'s ceiling: ten minutes. A start held open longer than that
/// is a hung start to every supervisor that watches one, and the late-arm restart (which brings a
/// tradehub back armed once the gatewayhub answers, whatever this row says) is the cure for a
/// gatewayhub that is further away than that.
pub const GATEWAYHUB_CONNECT_WAIT_MAX_MS: u32 = 600_000;

/// How long the tradehub's control door holds an OPENING order for the first price of a symbol whose quote
/// lane it just subscribed, in milliseconds. `vike-tradehub`'s `PRICE_WAIT` is built from this number, and
/// it lives here, below the door and below the client, because the client's default wait
/// ([`DEFAULT_NODE_ACK_WAIT_MS`]) is derived from it: a client that hangs up inside the hold never reads the
/// refusal the hold ends in (the v0.1.81 IG drill: a 2 s wait against this 4 s hold left no row, no audit
/// line and no answer). Not a setting: no row changes it.
pub const DOOR_PRICE_WAIT_MS: u32 = 4_000;

/// What [`DEFAULT_NODE_ACK_WAIT_MS`] carries over [`DOOR_PRICE_WAIT_MS`]: the tunnelled round trip, and the
/// facts look-up the door runs before it starts holding.
pub const ACK_WAIT_MARGIN_MS: u32 = 2_000;

/// [`Config::node_ack_wait_ms`]'s default, in milliseconds: how long `vike-cli`'s `trade` and `mcp`
/// wait for the node's verdict on the command they just sent. It is [`DOOR_PRICE_WAIT_MS`] plus
/// [`ACK_WAIT_MARGIN_MS`] (6 s), so a refusal the door gives after holding an order for a price is read
/// instead of hung up on; it used to be 2 s, which ended inside that hold. A box with a row keeps its row.
pub const DEFAULT_NODE_ACK_WAIT_MS: u32 = DOOR_PRICE_WAIT_MS + ACK_WAIT_MARGIN_MS;

/// The loader's warning for a `config.node_ack_wait_ms` row SHORTER than the control door's price hold
/// ([`DOOR_PRICE_WAIT_MS`]); `ms` is the row's value. The row is legal down to [`MIN_NODE_ACK_WAIT_MS`] and stays
/// honoured, so this is a warning and never a refusal (a box that filed such a row before the door held orders
/// for a price must still boot), but a client that hangs up inside the hold prints "sent, but the node has not
/// answered" for an opening order on a new symbol that the door then refuses or places, and never reads the
/// refusal (the v0.1.81 IG drill). Pushed by `load.rs`'s `load_with_source` while the row is below the hold.
#[must_use]
pub fn node_ack_wait_below_hold_warning(ms: u32) -> String {
    format!(
        "config.node_ack_wait_ms = {ms} ms is shorter than the control door's {DOOR_PRICE_WAIT_MS} ms price \
         hold: `vike-cli trade` and `mcp` hang up inside it, print \"sent, but the node has not answered\" for \
         an opening order on a new symbol that the door then refuses or places, and never read its refusal. \
         The default is {DEFAULT_NODE_ACK_WAIT_MS} ms; `vike-cli config unset config.node_ack_wait_ms` \
         restores it"
    )
}

/// [`Config::node_ack_wait_ms`]'s floor. Below half a second a healthy tunnelled node answers
/// "outcome NOT known" for a command it accepted, and an operator who retries on that wording
/// sends the order twice.
pub const MIN_NODE_ACK_WAIT_MS: u32 = 500;

/// [`Config::node_ack_wait_ms`]'s ceiling, in milliseconds: the reply timeout the control client
/// enforces on its own (`vike_tradehub_client::liveness::CONTROL_REPLY_TIMEOUT`, which is 30 s).
/// A longer wait could never be answered, because the client declares the link dead first, and
/// `vike-cli mcp` serves one request at a time, so this is also the longest a write tool can hold
/// it.
///
/// ⚠ This crate cannot import that constant (it sits at a higher layer), so the number is restated
/// here and `crates/vike-cli/src/cmd/trade/tests/ack_wait.rs`'s
/// `the_ack_wait_ceiling_is_the_control_reply_timeout` holds the two equal, the way
/// [`RECONCILE_POLICIES`] is held to its reader's words.
pub const MAX_NODE_ACK_WAIT_MS: u32 = 30_000;

/// Deployment settings — paths and addresses for THIS machine.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Config {
    /// Root of the DataFusion/Parquet history + tick store. `None` = the caller's own fallback
    /// (today: five different ones — see the module doc).
    ///
    /// Was `VIKE_HIST_STORE`.
    pub store_root: Option<PathBuf>,

    /// Directory for the JSON daily-rolling log file. `None` = `<exe_dir>/logs`, `vike-log`'s
    /// own fallback.
    ///
    /// Was `VIKE_LOG_DIR`.
    pub log_dir: Option<PathBuf>,

    /// Directory for the live core's mmap write-ahead command journal — its ONLY source and its
    /// switch: set = journaling on, in this directory; absent = off. A run profile cannot name
    /// another (decision 0111, amended 2026-10-11), so `vike-tradehub`, which writes the journal,
    /// and `vike-datahub`, whose materializer follows it, read one directory.
    ///
    /// Was `VIKE_JOURNAL_DIR`.
    pub journal_dir: Option<PathBuf>,

    // ⚠ `state_dir` (the desktop's strategy-state SIDECAR directory) is DELETED, and
    // `VIKE_STATE_DIR` is a `crate::REMOVED_ENV` tombstone that refuses startup. ⚠ It is NOT the
    // state ROOT (`<settings>/state`) that vike-tradehub, vike-app-core and vike-studio resolve —
    // see `vike_paths::state_path`, which records the collision.
    /// CLIENT-side DIAL address of a `vike-datahub` data server (`host:port`). **Set → vike-desktop's
    /// Studio takes its history from that server over RPC** (a DataFusion-free
    /// `RemoteHistStore`; the Studio's run Backend also defaults to Remote at the same address);
    /// **unset — the default — → the Studio opens the LOCAL store** at `config.store_root`'s
    /// resolution. Presence IS the branch, which is why this field has no always-set default.
    ///
    /// ⚠ Deliberately DISTINCT from [`Config::tradehub_addr`]: that one is the DAEMON'S BIND
    /// address (where `vike-tradehub` LISTENS on this machine); this one is where a CLIENT
    /// DIALS a datahub server, usually on another machine. The `vike-datahub` server's own listen
    /// address is [`Config::datahub_bind_addr`], defaulting to [`DEFAULT_DATAHUB_ADDR`].
    ///
    /// Was `VIKE_DATAHUB_ADDR` on a client.
    pub datahub_addr: Option<String>,

    /// The backtesthub's address (`host:port`) — where `vike-backend backtesthub --addr` BINDS on
    /// this box, and where a client DIALS it. `None` = [`DEFAULT_BACKTEST_ADDR`].
    ///
    /// ⚠ **No list of CLIENTS here**: a list of callers in a setting's doc is a second roster, and
    /// rots. The answer a reader needs is a COMMAND rather than a list, and
    /// `crates/vike-cli/src/cmd/backtest.rs` carries the one this repository actually RUNS —
    /// `crates/vike-ops/tests/docs/unrun_command_gate.rs` has a row for it. Read it there.
    ///
    /// ⚠ **ONE key for both sides, unlike the datahub/tradehub pair above, and that is the ruling's
    /// own choice**: *"The same key is what the CLIENT resolves, exactly as `config.tradehub_addr`
    /// is read by both the daemon that binds and the `vike-cli trade` that connects — so moving the
    /// backtest verbs off `VIKE_DATAHUB_ADDR` is one key added, not a second convention."* The cost
    /// it accepts is the one [`Config::node_addr`] argues about its own sibling: behind a tunnel the
    /// daemon's bind and the client's dial are different facts that happen to agree, and a
    /// deployment where they do not needs the client's box to set this key to the tunnel mouth.
    /// Nothing tunnels between a backtesthub and the store beside it, which is why that cost is
    /// affordable here and is not on the tradehub plane.
    ///
    /// The client ladder is `--addr <v>` → this key → [`DEFAULT_BACKTEST_ADDR`], and
    /// `crates/vike-cli/src/cmd/backtest.rs`'s `resolve_addr` is the one place it is folded.
    /// ⚠ `crates/vike-cli/src/cmd/study.rs` and the `mcp` server's compute tools are CALLERS of
    /// it: every verb that dials this daemon does so on this key, so a second copy of the fold
    /// could disagree about a blank rung and aim one of them at `7878` or `7879`.
    ///
    /// ⚠ Rulings 7 and 16 are the two halves of one wire — the daemon that binds and the client
    /// that dials — and each half once added this field on its own branch with no textual
    /// conflict. Two of the duplicates are silent — `setting_keys` would print this key twice in
    /// `vike-cli config show`, and `consumer_of` is a `find`, so a second `CONSUMPTION` row is
    /// unreachable. Only the merge can see them.
    ///
    /// Ruling 7 of `docs/superpowers/specs/2026-09-09-datahub-market-data-wire-design.md`; the
    /// client half is ruling 16.
    ///
    /// Was `VIKE_BACKTEST_ADDR`.
    pub backtest_addr: Option<String>,

    /// Address the headless `vike-tradehub` control surface listens on — the DAEMON'S BIND
    /// address, deliberately distinct from [`Config::datahub_addr`], which is a CLIENT'S dial
    /// address. `None` = no control server, which is also the safe default — that surface is
    /// opt-in.
    ///
    /// Was `VIKE_TRADEHUB_ADDR`.
    pub tradehub_addr: Option<String>,

    /// The datahub dial address the `vike-tradehub` daemon ADVERTISES to its clients
    /// (split-plane REQ-2): set → every `Welcome.features` the daemon's node server sends
    /// carries `datahub=<this value>`, and a connected client that has no explicit
    /// `datahub_addr` of its own adopts it — one configured address (the daemon's) reaches both
    /// planes. `None` — the default — advertises nothing, byte-identical to the pre-REQ-2
    /// Welcome. Advertisement, NEVER proxying: history/backtest traffic still dials the datahub
    /// directly; nothing routes through the process holding live orders.
    ///
    /// ⚠ THREE addresses, three jobs — this is the third:
    /// - [`Config::tradehub_addr`]: where the DAEMON LISTENS (its bind address, on the daemon's
    ///   box).
    /// - [`Config::datahub_addr`]: where THIS machine's CLIENT DIALS a datahub — an explicit
    ///   client-side setting that always WINS over any advertisement.
    /// - `datahub_advertise_addr` (this key, read on the DAEMON's box): where the daemon TELLS
    ///   clients to dial the datahub it fronts. It must be an address valid FROM THE CLIENT'S
    ///   side of the wire — for the tunnel-only posture both servers ship with, that is the
    ///   client-local tunnel mouth (e.g. `127.0.0.1:7878`, with `ssh -L` forwarding both ports),
    ///   not the daemon's private interface.
    ///
    /// Never read by the datahub server itself (its listen address is
    /// [`Config::datahub_bind_addr`]); consumed by `vike-tradehub`'s `start_observe_server`.
    ///
    /// Was `VIKE_DATAHUB_ADVERTISE_ADDR`.
    pub datahub_advertise_addr: Option<String>,

    /// **The BARRIER an operator DECLARES before the node will administer accounts** — the
    /// credential-carrying wire verbs of
    /// `docs/decisions/0065-accounts-are-managed-and-the-barrier-is-declared.md`.
    ///
    /// THREE values, not a boolean, and the split is the whole design:
    ///
    /// | value | what the operator asserts | what the daemon does |
    /// |---|---|---|
    /// | unset / `off` | nothing | the account capability is NOT built; no admin key is read and no `account-verbs` feature is advertised. Byte-identical to a binary without the verb |
    /// | `loopback` | *this listener is on loopback and reached through a tunnel* | **CHECKS it** against `vike_hub_proto::bind::bind_exposure`; a wide bind REFUSES the capability at boot, naming this key and the address |
    /// | `contained` | *the barrier is outside this process* — a `127.0.0.1`-published container port, a private interface | arms regardless of the bind, does NOT verify it, and logs exactly what has been asserted |
    ///
    /// ⚠ **Why it is a DECLARATION rather than something the process works out.** That wire is
    /// PLAINTEXT and authenticates the CONNECTION rather than each frame, so confidentiality comes
    /// entirely from REACHABILITY — and 0065 §3b measures three independent reasons the process
    /// cannot decide that for itself: the server is handed an ALREADY-BOUND listener and never
    /// calls `local_addr`; `deploy/docker/entrypoint.sh` starts the daemon with
    /// `--allow-public-bind` unconditionally (Docker publishes to the container's `eth0`), so a
    /// bind-keyed rule would
    /// refuse every containerised install; and the per-frame PEER answers backwards inside a
    /// container correctly published to `127.0.0.1`. `docs/decisions/0026` forbids the inference
    /// cure outright. So the sound predicate is an operator DECLARATION — the same shape
    /// `flags.tradehub_allow_public_bind` already is, under its own rule that *an address cannot
    /// be its own consent, because typing the address is the mistake*. That rule cuts both ways:
    /// an address cannot be its own refusal either.
    ///
    /// ⚠ **A CONFIG key rather than a flag, and 0065 §3c says `flags.toml`.** `Flags` is a
    /// BOOLEAN plane by construction — `FLAG_META` and `FlagsPatch` both are — and a
    /// boolean here would collapse the two assertions into one word and make the CHECKABLE case
    /// uncheckable, which is precisely what that record refuses. The disposition is unchanged in
    /// every other respect: it is read at BOOT, so a change is restart-required, and arming the
    /// capability still needs an `VIKE_TRADEHUB_ADMIN_KEY` a settings write cannot mint.
    ///
    /// ⚠ **An UNRECOGNISED value is OFF**, not an error: a typo'd `loopbak` must never arm a
    /// credential surface. The daemon logs the value it did not recognise, so an operator who
    /// typed one is told rather than left believing the barrier is up.
    pub tradehub_account_admin: Option<String>,

    /// **WHICH BOX the `vike-tradehub` daemon tells its clients it is running on** — the OVERRIDE
    /// half of the daemon's self-report, and the half an operator only needs when the kernel's
    /// answer is wrong.
    ///
    /// `None` — the default — is NOT "advertise nothing": the daemon discovers its own source
    /// address from the routing table and reports that
    /// (`crates/vike-tradehub/src/self_address.rs`). **An operator configures nothing to see a real
    /// address**; this key exists for the cases the route lookup cannot answer, and it WINS
    /// outright when set:
    ///
    /// - **behind NAT** — the lookup returns the box's PRIVATE address, because the kernel does not
    ///   know the public one in front of it and asking a third party is refused. Set this to the
    ///   public face.
    /// - **a box whose useful name is not the routed interface** — a management VLAN, a second
    ///   provider, the address a tunnel is keyed on.
    /// - **a loopback-only container**, where the lookup has no route to name a source from and the
    ///   daemon would otherwise report nothing.
    ///
    /// ⚠ **This is an ADVERTISEMENT OF IDENTITY, not a dial address, and it is the one thing that
    /// separates it from [`Config::datahub_advertise_addr`] above.** A daemon that binds loopback is
    /// reachable only through a tunnel, so the address it reports for itself names the BOX and is
    /// not something a client can connect to — every surface renders it as the daemon's claim about
    /// where it is running. It changes no routing, opens no socket and is read by no client as a
    /// destination.
    ///
    /// Validated as `host:port` like its four siblings, so the value reads the same way as every
    /// other address in this file (`203.0.113.7:7879`, `[2001:db8::1]:7879`).
    ///
    /// Read on the DAEMON's box, by `vike-tradehub`'s startup, into the identity block every
    /// published frame carries.
    pub tradehub_advertise_addr: Option<String>,

    /// CLIENT-side DIAL address of a running `vike-tradehub` node — where THIS box's `vike-cli`
    /// looks for a node when `--node` is not given. `None` — the default — means every node-facing
    /// verb still requires `--node` outright, exactly as before this key existed.
    ///
    /// ⚠ **FIVE addresses, five jobs, and this is the fourth of them.** Three are argued on
    /// [`Config::datahub_advertise_addr`]; the whole set, in the one order that makes them
    /// distinguishable — WHO holds the value, and whether they LISTEN on it or DIAL it:
    /// - [`Config::tradehub_addr`] — the DAEMON's box, LISTEN. Where `vike-tradehub` binds.
    /// - [`Config::datahub_addr`] — a CLIENT's box, DIAL. Where this machine reaches a datahub.
    /// - [`Config::datahub_advertise_addr`] — the DAEMON's box, DIAL-FOR-SOMEBODY-ELSE. What the
    ///   node tells its clients to use for the datahub it fronts.
    /// - `node_addr` (this key) — a CLIENT's box, DIAL. Where this machine reaches a tradehub.
    /// - [`Config::backtest_addr`] — BOTH boxes, and the only one of the five that is read from
    ///   each end: the backtesthub binds it and a compute client dials it.
    ///
    /// So it is [`Config::datahub_addr`]'s exact twin one plane over, and it is deliberately NOT
    /// named `tradehub_addr` — that spelling is taken by the daemon's BIND, and the two are set on
    /// different machines to different values whenever a tunnel is in front (the daemon binds
    /// `127.0.0.1:7879` on its own box; the client dials `127.0.0.1:7879` at the tunnel mouth, and
    /// the two agreeing is a coincidence of the tunnel rather than a shared fact). It is named for
    /// the FLAG it defaults — `--node` — because that is the one thing a reader can check without
    /// knowing which box they are on.
    ///
    /// It configures a per-invocation default that already has a per-invocation override: `--node`
    /// wins over it, is typed by the person running the command, and appears in the command's own
    /// usage.
    ///
    /// Written by `vike-cli backend connect`, which is the point of it: that verb raises the tunnel
    /// and then records the address it raised, so the six later commands do not each carry it.
    pub node_addr: Option<String>,

    /// How long `vike-cli`'s `trade` (the REPL and every one-shot write verb) and `mcp` write tools
    /// wait for the node's verdict on the command they just sent, in milliseconds, from
    /// [`MIN_NODE_ACK_WAIT_MS`] to [`MAX_NODE_ACK_WAIT_MS`]. Defaults to
    /// [`DEFAULT_NODE_ACK_WAIT_MS`], which outlasts the control door's price hold ([`DOOR_PRICE_WAIT_MS`]).
    /// A row below that hold hangs up inside it and reads "not known" for an order the door then refuses.
    ///
    /// A CLIENT's value, read on the box that runs `vike-cli` and never by the daemon (like
    /// [`Config::node_addr`], whose pair it is). ⚠ **It only changes WHEN the client gives up
    /// waiting.** A command sent is sent: when the wait runs out the client says the outcome is NOT
    /// known and points at `node_snapshot` / `trade order ls`, and the command may still execute.
    /// Nothing about this row cancels, retries or re-sends an order, so a shorter wait can only
    /// make the wording "not known" appear sooner. A longer one holds a REPL prompt, a one-shot
    /// verb or (for `mcp`, which serves one request at a time) the whole server for that long when
    /// a node goes quiet, which is why the ceiling is the control client's own reply timeout.
    ///
    /// A value outside the range is REFUSED at the write and at the load; nothing clamps it.
    pub node_ack_wait_ms: u32,

    /// This deployment's ORIGIN TAG — 1..=4 alphanumerics, stamped into the client order id of
    /// every order this instance places so a SECOND instance trading the same venue account is
    /// recognisable on reconcile instead of anonymous.
    ///
    /// It is a `config` key rather than a `policy` one because it answers this section's own
    /// question — "would a second machine running the same strategy need a different value?" —
    /// with the loudest possible yes: two machines sharing this value is precisely the
    /// misconfiguration the tag exists to reveal. Each box — and each container, which mounts its
    /// own project — writes its own row into its own settings database; two instances that share
    /// ONE database would share the tag, and that is the one deployment a row cannot tell apart
    /// (decision 0111's first reopen condition).
    ///
    /// Was `VIKE_INSTANCE_ORIGIN`.
    ///
    /// `None` — the default — mints exactly the ids this workspace has always minted and leaves
    /// every reconcile fold decision byte-identical. `vike_model::instance_origin`'s module doc
    /// argues the wire format and why a DECLARED tag beat every derived candidate;
    /// `docs/ops/double-live-instances.md` is the operator page.
    pub instance_origin: Option<vike_model::InstanceOrigin>,

    // -- The rows `docs/decisions/0111-no-setting-lives-in-the-environment-or-a-toml-file.md` added
    //    for settings that had none. Each is read by the daemon named in its doc, after its boot,
    //    and each field names the variable it replaced (a `crate::REMOVED_ENV` refusal).
    /// The reconcile POLICY word, one of [`RECONCILE_POLICIES`]. `None` — the default — is
    /// `quarantine`: the live daemon folds the quarantine-first default in for an unset policy
    /// (`docs/decisions/0043-reconcile-is-on-by-default-under-quarantine.md`).
    ///
    /// Was `VIKE_RECONCILE_POLICY`. ⚠ Read the root `CLAUDE.md`'s rollout rule before writing
    /// `hybrid` or `synthesize`: both auto-apply `PositionDrift`.
    pub reconcile_policy: Option<String>,

    /// The continuous re-reconcile cadence, in milliseconds. `0` turns the repeat off, leaving the
    /// startup pass. `None` — the default — is 60 000. Was `VIKE_RECONCILE_INTERVAL_MS`.
    pub reconcile_interval_ms: Option<u32>,

    /// The stuck-order audit cadence, in milliseconds. `0` turns audits off. `None` — the default —
    /// follows whatever the interval resolved to. Was `VIKE_RECONCILE_AUDIT_MS`.
    pub reconcile_audit_ms: Option<u32>,

    /// How far back each pass asks a venue for order and fill reports, in milliseconds, at least
    /// 1. `None` — the default — is one hour. Was `VIKE_RECONCILE_LOOKBACK_MS`.
    pub reconcile_lookback_ms: Option<u32>,

    /// The delay before the first reconcile pass after a start, in milliseconds, at most
    /// [`vike_venues::venue_restore::MAX_RECONCILE_STARTUP_DELAY_MS`]. `None` — the
    /// default — is 2 000. Was `VIKE_RECONCILE_STARTUP_DELAY_MS`.
    ///
    /// ⚠ The ceiling is not about the reader, which would honour any `u32`: it is half the window
    /// in which a restored order mutes its strategy's tagged resubmit
    /// ([`vike_venues::venue_restore::RESTORED_TAG_DENY_WINDOW_MS`]). That window ends by
    /// itself at the first pass, so a pass that starts after it leaves the order resting
    /// unregistered while the strategy quotes beside it. A larger row is REFUSED, at the write and
    /// at the load (`Config::apply`, which both share); nothing clamps it.
    pub reconcile_startup_delay_ms: Option<u32>,

    /// The absolute floor (quote units) of the cash-reconcile money tolerance, finite and not
    /// negative. Consulted only while `flags.reconcile_balance` is on. `None` — the default — is
    /// 1.0. Was `VIKE_RECONCILE_BALANCE_TOL_ABS`.
    pub reconcile_balance_tol_abs: Option<f64>,

    /// The relative band (a fraction of the wallet) of the same tolerance, finite and not
    /// negative. `None` — the default — is 1e-4. Was `VIKE_RECONCILE_BALANCE_TOL_REL`.
    pub reconcile_balance_tol_rel: Option<f64>,

    /// The node's control-command RATE cap, commands per second, finite and above zero — a
    /// throughput knob, never a risk ceiling (raising it cannot place a larger order), which is why
    /// it is `config` rather than `policy`. `None` — the default — is 20. Was
    /// `VIKE_TRADEHUB_CONTROL_RATE`.
    pub tradehub_control_rate: Option<f64>,

    /// The trading daemon's thread-to-core pinning, in `vike_exec::affinity`'s own `role:core,…`
    /// grammar (`md:28,core:29,exec:30`), non-blank. `None` — the default — pins nothing. Only
    /// `vike-tradehub` installs it: the database is shared by every process on the box, and a
    /// second daemon pinning to the same cores would fight the first for them. Was
    /// `VIKE_PIN_CORES`.
    pub pin_cores: Option<String>,

    /// Where the `vike-datahub` SERVER binds (`host:port`) — its LISTEN address, deliberately a
    /// different key from [`Config::datahub_addr`], which is where a CLIENT dials one. `None` — the
    /// default — is [`DEFAULT_DATAHUB_ADDR`]. A non-loopback address is still refused unless
    /// `flags.datahub_allow_public_bind` consents and hub keys are set. Was
    /// `VIKE_DATAHUB_ADDR` on the data server.
    pub datahub_bind_addr: Option<String>,

    /// The gatewayhub's address (`host:port`) — where `vike-backend gatewayhub` BINDS on this box,
    /// and where the data daemon DIALS it. `None` — the default — is [`DEFAULT_GATEWAYHUB_ADDR`].
    ///
    /// ⚠ **ONE key for both sides, [`Config::backtest_addr`]'s shape and for its reason**: the
    /// daemon that binds and the client that dials sit on the same box (the gatewayhub is a
    /// loopback service of the box that runs it, never reached through a tunnel), so two keys would
    /// be two facts that must agree and nothing holds them equal. The cost that record accepts
    /// applies here too, and is smaller: there is no tunnel in front of this one.
    ///
    /// ⚠ **The daemon refuses a non-loopback value at startup** (`vike-gatewayhub`'s bind guard),
    /// with no `allow_public_bind` flag to lift it. This crate does not repeat the rule at the
    /// write: the classification is `vike_hub_proto::bind::bind_exposure`'s, a crate above this
    /// one, and a second spelling of "is this loopback" here would be the copy that drifts. The
    /// write checks only what it checks for every address row: non-blank and carrying the `host:port`
    /// separator (`check_addr`), so a value the daemon cannot resolve is refused by the bind guard
    /// at startup, naming this key.
    ///
    /// A new `Config` row is an unknown key to an older binary, which refuses the store: unset the
    /// key (`vike-cli config unset config.gatewayhub_addr`) before a downgrade.
    pub gatewayhub_addr: Option<String>,

    /// How long a tradehub that starts while the gatewayhub does not answer keeps asking before it
    /// gives up on its `demo`/`live` accounts, in milliseconds, within
    /// [`GATEWAYHUB_CONNECT_WAIT_MIN_MS`]..=[`GATEWAYHUB_CONNECT_WAIT_MAX_MS`]. `None` — the default —
    /// is [`DEFAULT_GATEWAYHUB_CONNECT_WAIT_MS`] (a minute). Read at start by the tradehub only, and
    /// only when at least one account is active at a non-paper tier: a paper-only box never waits
    /// (decision 0164, owner ruling D7).
    ///
    /// What the wait ends in is NOT a refusal to start: the accounts that need the gatewayhub are
    /// NOT RUNNING with `ArmingBlock::GatewayUnavailable` (never a paper stand-in, decision 0156),
    /// the accounts that need no venue run as usual, and the tradehub arms the others by itself
    /// once the gatewayhub answers (one graceful restart per process start). A row outside the
    /// range is REFUSED at the write and the load, never clamped.
    ///
    /// A new `Config` row is an unknown key to an older binary, which refuses the store: unset the
    /// key (`vike-cli config unset config.gatewayhub_connect_wait_ms`) before a downgrade.
    pub gatewayhub_connect_wait_ms: Option<u32>,

    /// The live market-data plane's RESIDENT set, `venue:symbol:lane,…`, non-blank — the keys the
    /// datahub holds subscribed whether or not a client asks. Read only while the plane is armed
    /// (`flags.datahub_live`); a row inside it that does not parse is a startup WARNING naming the
    /// row, never a refusal (`docs/decisions/0013-degrade-vs-refuse.md`). `None` — the default —
    /// pins nothing. Was `VIKE_DATAHUB_LIVE_RESIDENT`.
    pub datahub_live_resident: Option<String>,

    // -- The data plane's per-box tuning rows, all read by `vike-datahub` once at its start (each is
    //    restart-required). For every one, `None` — the default — is exactly the behaviour of a box
    //    that never heard of the key; a value outside its range (the `pub const`s above) is REFUSED
    //    at the write and at the load, never clamped; and an OLDER binary refuses a store that
    //    carries the row, which no verb can remove (there is no `config unset`), so writing one is a
    //    one-way door for a rollback. They are `config.*` rows and not columns of the `recorder`
    //    profile table: that table cannot grow a column on an existing database, and these are
    //    facts about one box rather than about one profile.
    /// How long the venue-catalog lane answers a venue's instrument list from memory before it
    /// asks the venue again, in seconds, within [`DATAHUB_CATALOG_TTL_SECS`]. `None` — the
    /// default — is six hours (`crates/vike-datahub/src/catalog.rs`'s `CATALOG_TTL`). A shorter TTL
    /// shows a new listing sooner on the venues that list within hours (Polymarket, Hyperliquid's
    /// HIP-3 dexes).
    ///
    /// ⚠ This is the STEADY-STATE knob. How fast a venue can be asked at all stays the per-venue
    /// bucket's (`CATALOG_VENUE_BURST` and `CATALOG_VENUE_REFILL`), so a short TTL cannot make the
    /// lane spend more than one fetch a minute per venue once the burst is gone.
    pub datahub_catalog_ttl_secs: Option<u32>,

    /// The recorder's silence-watchdog threshold, in seconds: `vike-datahub`'s `--silent-secs`,
    /// which beats this row for the run that names it (the shipped unit names none). `0` switches
    /// the whole watchdog off — the recency, rate, family and never-resolved judgements alike: no
    /// verdict, no log line, no alert — and any other value must lie within
    /// [`RECORDER_SILENT_SECS`]. `None` — the default — is 300
    /// (`crates/vike-datahub/src/recorder.rs`'s `DEFAULT_SILENT_SECS`).
    ///
    /// ⚠ Below 300 the Polymarket five-minute rotation cries wolf: a freshly opened window can be
    /// genuinely quiet for a stretch (the doc on `DEFAULT_SILENT_SECS`). The ceiling is the rate
    /// judgement's own window; at it the two judgements race instead of the recency one winning.
    /// `--exit-on-silence` with this row at `0` is refused at start, as it is with `--silent-secs 0`.
    pub recorder_silent_secs: Option<u32>,

    /// How often the recorder re-resolves its desired symbol set, and judges its liveness, in
    /// seconds, within [`RECORDER_TICK_SECS`]: `vike-datahub`'s `--tick-secs`, which beats this
    /// row for the run that names it. `None` — the default — is 30
    /// (`crates/vike-datahub/src/recorder.rs`'s `DEFAULT_TICK_SECS`).
    ///
    /// ⚠ The tick paces every watchdog verdict. The family-collapse window is time-defined and
    /// closes on the first tick that reaches 30 s, so a longer tick makes it a tick long and its
    /// detection latency doubles by 60 s and keeps growing
    /// (`crates/vike-recorder/src/liveness/family.rs`'s `FAMILY_WINDOW_MS` carries the figures, and
    /// measures a five-minute tile MISSING the 2026-08-05 incident at the wrong phase). A
    /// Polymarket window rotates every five minutes with the planner holding the next one, so a
    /// long tick also leaves fewer chances to see a new window listed before the current one
    /// expires.
    pub recorder_tick_secs: Option<u32>,

    /// Flush a recorded `(venue, symbol)` buffer once it holds this many rows, within
    /// [`RECORDER_FLUSH_MAX_ROWS`]. `None` — the default — is 5 000
    /// (`crates/vike-store/src/live_rec.rs`'s `RecorderConfig`). More rows per flush means fewer,
    /// larger store commits.
    ///
    /// ⚠ A bigger buffer is a bigger loss window on SIGKILL, an OOM kill or a power cut (a graceful
    /// stop flushes it, inside the stop budget the shipped unit's `TimeoutStopSec=` is sized
    /// against), and more resident memory under the unit's `MemoryMax=`.
    pub recorder_flush_max_rows: Option<u32>,

    /// Flush a recorded buffer once its oldest row has waited this long, in seconds, within
    /// [`RECORDER_FLUSH_MAX_AGE_SECS`]. `None` — the default — is 30
    /// (`crates/vike-store/src/live_rec.rs`'s `RecorderConfig`). Same trade, same ⚠, as
    /// [`Config::recorder_flush_max_rows`]: whichever of the two trips first flushes.
    pub recorder_flush_max_age_secs: Option<u32>,

    /// The option-chain recorder's snapshot cadence, in seconds, within
    /// [`DATAHUB_CHAIN_CADENCE_SECS`]: at most one `kind=chain` snapshot per (underlying, expiry)
    /// per this many seconds. Read only while `flags.record_chains` arms the recorder. `None` — the
    /// default — is 60 (`crates/vike-data/src/rec/chain_rec.rs`'s `DEFAULT_CHAIN_CADENCE_MS`).
    ///
    /// ⚠ The idempotency key of a snapshot is `chain:{venue}:{underlying}:{expiry}:{ts_ms /
    /// cadence_ms}` and does NOT contain the cadence, so changing this row on a series that already
    /// holds snapshots RE-NUMBERS its buckets: one extra snapshot can land in a period that already
    /// had one, and two cadences that differ by a fraction of a percent can collide on a key written
    /// days earlier for the same expiry, which drops the new snapshot. Pick it once
    /// (`crates/vike-store/src/chain_rec_tests.rs`'s
    /// `a_near_equal_cadence_can_swallow_a_snapshot_written_days_earlier` builds the collision).
    /// The recorder writes on the options lane's poll, so a cadence below
    /// [`Config::datahub_options_poll_secs`] records every poll.
    pub datahub_chain_cadence_secs: Option<u32>,

    /// How often the options lane re-polls Deribit's REST book summaries, in seconds, within
    /// [`DATAHUB_OPTIONS_POLL_SECS`]. `None` — the default — is 12
    /// (`crates/vike-datahub/src/options/deribit.rs`'s `POLL_INTERVAL`). The lane polls nothing
    /// until the first request, or from the start while `flags.record_chains` arms the recorder.
    ///
    /// ⚠ It spends Deribit's public API budget from this box's IP, in proportion: halving it doubles
    /// the requests.
    pub datahub_options_poll_secs: Option<u32>,

    /// The write-ahead journal's snapshot cadence, in commands, at least 1, for the journal
    /// [`Config::journal_dir`] turns on; an active run profile's `sinks.journal.snapshot_every`,
    /// when named, beats it. `None` — the default — is 1 024. Was `VIKE_JOURNAL_SNAPSHOT_EVERY`.
    pub journal_snapshot_every: Option<u32>,

    // -- The rows of the trading-mount batch: each one lifts a number the mount hard-coded, and each
    //    keeps today's behaviour while its row is absent (no variable ever carried any of them, so
    //    none has a `crate::REMOVED_ENV` refusal). Every reader is `vike-tradehub`'s
    //    `MountRows::from_config`, which hands the value to `vike-tradehub-core` and `vike-core` as a
    //    PARAMETER (neither library reads a setting).
    /// The slippage, as a fraction of price (`0.0002` is 2 basis points), a `paper`-tier account's
    /// simulated exchange fills every order at, finite and from `0` to [`MAX_PAPER_SLIPPAGE`]. `None`
    /// — the default — is `vike_tradehub_core::policy::DEFAULT_PAPER_SLIPPAGE`, the figure the mount
    /// hard-coded.
    ///
    /// ⚠ Fees are not touched: a paper book keeps the venue's own `fee_schedule_for`. ⚠ Only the
    /// paper books `vike_tradehub_core::assemble_runtime` makes for a `paper`-tier ACCOUNT read it. The daemon's
    /// paper arm (`flags.tradehub_live` off) and the cross-exchange maker take their slippage from
    /// their own mount config and do not.
    pub paper_slippage: Option<f64>,

    /// The free space, in MiB, below which the startup preflight WARNS about the journal directory,
    /// from 1 to [`MAX_PREFLIGHT_DISK_MIB`] and ABOVE [`Config::preflight_disk_fail_mib`] (a pair
    /// that does not order is refused naming both keys). `None` — the default — is
    /// [`DEFAULT_PREFLIGHT_DISK_WARN_MIB`].
    ///
    /// ⚠ The disk leg watches ONE directory, the one `config.journal_dir` names, so with no journal
    /// row it has nothing to measure and neither floor is read. It never stops a venue: a FAIL is
    /// logged and the mount proceeds. It measures on unix only (elsewhere the leg warns that it
    /// cannot).
    pub preflight_disk_warn_mib: Option<u32>,

    /// The free space, in MiB, below which the startup preflight FAILS the journal directory, from
    /// 1 to [`MAX_PREFLIGHT_DISK_MIB`] and BELOW [`Config::preflight_disk_warn_mib`]. `None` — the
    /// default — is [`DEFAULT_PREFLIGHT_DISK_FAIL_MIB`]. Read with its twin; see there.
    pub preflight_disk_fail_mib: Option<u32>,

    /// The live core's FAST in-flight confirm, in milliseconds: an order stuck submitted-but-unacked
    /// is re-queried at the venue this often, from [`MIN_INFLIGHT_CONFIRM_MS`] to
    /// [`MAX_INFLIGHT_CONFIRM_MS`]. `0` and `None` — the default — leave it OFF
    /// (`vike_core::CoreConfig::inflight_confirm` stays `None`, the core arms no timer for it). It
    /// never synthesizes a terminal: it asks the venue and the venue's own answer folds back, so a
    /// stuck order is found in seconds instead of at `submit_ack_timeout`, at the price of extra
    /// reject-free REST reads (two on bybit) per stuck order.
    ///
    /// ⚠ It acts only on orders younger than the effective `submit_ack_timeout`; a value at or above
    /// it leaves the fast rung nothing to do, which the daemon warns about at startup but does not
    /// refuse (the timeout is the run profile's, unknown here). The LIVE mount only: a `paper` core
    /// has no venue to ask.
    ///
    /// ⚠ It also shortens the core's idle waker to half this cadence (the waker ticks at half the
    /// smallest period any enabled sweep needs, and the stuck-order watchdog's is long), and the
    /// live core journals every waker tick when `config.journal_dir` is set: a faster cadence is a
    /// faster stream of tiny no-op records, and each one counts toward the next journal snapshot.
    pub inflight_confirm_ms: Option<u32>,

    /// How often the core saves every stateful strategy mount's durable state (a breaker's trip
    /// count, an A-S accumulator) to its sidecar file WHILE IT RUNS, in milliseconds, from
    /// [`MIN_STRATEGY_STATE_SAVE_MS`] to [`MAX_STRATEGY_STATE_SAVE_MS`]. `0` and `None` — the
    /// default — leave only the save at a clean shutdown, so a crash loses everything since the
    /// start (`vike_core::CoreConfig::state_save` stays `None`, no timer is armed).
    ///
    /// ⚠ Each save is the atomic, fsynced sidecar write the shutdown save makes, per mount that has
    /// state, run by the core thread at a drain-loop boundary: a cold path, but not free. Needs the
    /// strategy-state directory the project provides (`vike_core::CoreConfig::state_dir`); with no
    /// project there is nowhere to write and the row does nothing. Both mounts read it, live and
    /// paper.
    pub strategy_state_save_ms: Option<u32>,

    /// How long a `vike-datahub` or `vike-backtesthub` connection may go WITHOUT A REQUEST FRAME
    /// before the daemon closes it, in milliseconds, within
    /// [`NODE_IDLE_READ_TIMEOUT_MIN_MS`]..=[`NODE_IDLE_READ_TIMEOUT_MAX_MS`]. `None` — the default —
    /// is `vike_hub_proto::limits::SERVER_IDLE_READ_TIMEOUT` (five minutes), byte for byte what a
    /// daemon without the row has always run. Read at start by both daemons, so a write waits for
    /// the next restart of each; the bound has no environment variable and never had one.
    ///
    /// ⚠ It bounds the gap BETWEEN frames, never the length of a request: a sweep or a backfill
    /// holds its connection thread for minutes inside one request and nothing here can fire while
    /// it does (`vike_hub_proto::limits::SERVER_IDLE_READ_TIMEOUT` carries why).
    ///
    /// ⚠ A SHORTER value reaps a client that holds one connection across requests, and that
    /// client's next request fails once: the desktop's Studio history store and the backtesthub's
    /// own reads hold their connection with no keepalive, so they pay it after any idle gap longer
    /// than this. The floor (`vike_hub_proto::limits::SERVER_IDLE_READ_TIMEOUT_MIN`) is twice the
    /// slowest cadence at which a legitimate client keeps a connection warm, and a row below it is
    /// REFUSED, at the write and at the load (`Config::apply`, which both share), never clamped.
    /// It does not bound a client's wait for a REPLY (`vike-datahub-client`'s own constant does).
    pub node_idle_read_timeout_ms: Option<u32>,

    /// How often the `vike-datahub` market-data stream writes a heartbeat when it has nothing else
    /// to say, in milliseconds, within [`MD_HEARTBEAT_MIN_MS`]..=[`MD_HEARTBEAT_MAX_MS`]. `None` —
    /// the default — is `vike_datahub_client::market::MD_HEARTBEAT` (15 s). Read at start by the
    /// data daemon only.
    ///
    /// The period is NEGOTIATED on the wire, not agreed in advance: the server sends it in
    /// `MdSubscribed.heartbeat_ms` and the client's read deadline is the larger of its own 45 s
    /// floor and three times that (`DatahubClient::md_subscribe`, the one door every client in this
    /// tree opens a stream through), so changing it here cannot desync a client. A longer period
    /// slows how soon a client calls a silent
    /// link dead (three periods, never under 45 s); a shorter one costs a small frame per quiet
    /// subscription per period. A row outside the range is REFUSED at the write and the load.
    pub md_heartbeat_ms: Option<u32>,

    /// **How long the data daemon's maintenance pass keeps each KIND of stored series**, as
    /// `kind:days` entries joined by commas, `book:14,depth:14,trade:90`. `None`, and the empty
    /// string, are today's behaviour: the recorder profile's scalar `[maintenance].retention_days`
    /// (itself absent by default, which keeps everything) decides every kind.
    ///
    /// ⚠ **IRREVERSIBLE DELETION.** A kind named here has every part of every series of that kind
    /// whose newest row is older than its days unlinked from disk by each pass, including history a
    /// backfill filled (`bar:30` deletes backfilled bars older than 30 days too). The only way back
    /// is to fetch the window again from its provider. A listed kind uses its own days INSTEAD of the
    /// scalar, longer or shorter; a kind not listed keeps the scalar.
    ///
    /// The grammar is [`crate::retention::parse_retention_days_by_kind`]'s, and every rule in it is a
    /// REFUSAL at the write and at the load (`Config::apply`): days from 1, an unknown kind, a
    /// duplicate, whitespace and an empty entry are refused, and so are `exec_fill`, `exec_order` and
    /// `exec_funding` by name, because execution history is the operator's own trade record and
    /// nothing on this row may expire it. Nothing clamps a value.
    ///
    /// Read on the DATA daemon's box, by `vike-datahub`, and only by a process that RUNS maintenance
    /// (a recording profile with `interval_secs` above zero); anywhere else the row deletes nothing and
    /// the daemon says so at startup. An older binary refuses a store that carries this row (unknown
    /// key): unset it before downgrading.
    pub recorder_retention_days_by_kind: Option<String>,

    /// How often the data daemon re-polls the NEWS feeds, in seconds, from
    /// [`MIN_FEED_POLL_SECS`] to [`MAX_FEED_POLL_SECS`]. `None` — the default — is 300. Read on the
    /// data daemon's box by its tool-feeds lane; the desktop learns the cadence from the daemon's
    /// reply and flags a feed STALE only past three times it. ⚠ The providers' rate limits are the
    /// operator's: this tree cannot verify any provider's quota, so a shorter cadence than the
    /// default is spent against a quota nobody here has measured.
    pub datahub_news_poll_secs: Option<u32>,

    /// How often the data daemon re-polls the ECONOMIC CALENDAR (ForexFactory's week), in seconds,
    /// from [`MIN_FEED_POLL_SECS`] to [`MAX_FEED_POLL_SECS`]. `None` — the default — is 1 800. See
    /// [`Config::datahub_news_poll_secs`] for who reads it and whose quota it spends.
    pub datahub_calendar_poll_secs: Option<u32>,

    /// How often the data daemon re-polls the three EQUITY calendars (Finnhub earnings, FMP
    /// dividends, Nasdaq IPOs — one cadence for all three), in seconds, from [`MIN_FEED_POLL_SECS`] to
    /// [`MAX_FEED_POLL_SECS`]. `None` — the default — is 1 800. Finnhub and FMP are KEYED: each poll
    /// spends one call of that key's quota per requested week. See [`Config::datahub_news_poll_secs`].
    pub datahub_equity_poll_secs: Option<u32>,
}

/// The shortest a tool feed's poll cadence may be, in seconds. The data daemon's first retry after
/// a failed poll is this long (`crates/vike-datahub/src/tool_feeds/mod.rs`'s `MIN_RETRY`) and doubles
/// up to the cadence, so a cadence below it would be shorter than the retry that follows a failure.
/// `crates/vike-datahub/src/tool_feeds/mod.rs`'s `the_config_range_is_the_lanes_own` holds the two
/// numbers equal.
pub const MIN_FEED_POLL_SECS: u32 = 60;

/// The longest a tool feed's poll cadence may be, in seconds: a day. A feed polled less often than
/// that is stale before it is asked again, and the desktop's staleness limit is three cadences.
pub const MAX_FEED_POLL_SECS: u32 = 86_400;

/// The range is not empty: a floor above the ceiling would refuse every cadence.
const _: () = assert!(MIN_FEED_POLL_SECS < MAX_FEED_POLL_SECS);

impl Default for Config {
    /// Every field's absence means "the caller's own fallback applies" — including
    /// `datahub_addr`, whose absence is itself the answer ("no datahub — local store") now that
    /// vike-desktop's Studio branches on its presence. The one exception is
    /// [`Config::node_ack_wait_ms`], which carries its default as a VALUE so `config show` can
    /// print what a box with no row runs.
    fn default() -> Self {
        Config {
            store_root: None,
            log_dir: None,
            journal_dir: None,
            backtest_addr: None,
            datahub_addr: None,
            tradehub_addr: None,
            datahub_advertise_addr: None,
            tradehub_account_admin: None,
            tradehub_advertise_addr: None,
            node_addr: None,
            node_ack_wait_ms: DEFAULT_NODE_ACK_WAIT_MS,
            instance_origin: None,
            reconcile_policy: None,
            reconcile_interval_ms: None,
            reconcile_audit_ms: None,
            reconcile_lookback_ms: None,
            reconcile_startup_delay_ms: None,
            reconcile_balance_tol_abs: None,
            reconcile_balance_tol_rel: None,
            tradehub_control_rate: None,
            pin_cores: None,
            datahub_bind_addr: None,
            gatewayhub_addr: None,
            gatewayhub_connect_wait_ms: None,
            datahub_live_resident: None,
            datahub_catalog_ttl_secs: None,
            recorder_silent_secs: None,
            recorder_tick_secs: None,
            recorder_flush_max_rows: None,
            recorder_flush_max_age_secs: None,
            datahub_chain_cadence_secs: None,
            datahub_options_poll_secs: None,
            journal_snapshot_every: None,
            paper_slippage: None,
            preflight_disk_warn_mib: None,
            preflight_disk_fail_mib: None,
            inflight_confirm_ms: None,
            strategy_state_save_ms: None,
            node_idle_read_timeout_ms: None,
            md_heartbeat_ms: None,
            recorder_retention_days_by_kind: None,
            datahub_news_poll_secs: None,
            datahub_calendar_poll_secs: None,
            datahub_equity_poll_secs: None,
        }
    }
}

/// The FILE shape of [`Config`] — all-optional, unknown keys rejected by name. See
/// [`crate::policy::PolicyPatch`] for why the loader patches instead of deserializing the
/// effective struct.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfigPatch {
    /// **TOMBSTONE — removed, and REFUSED rather than ignored.** Not a [`Config`] field.
    ///
    /// Parsed only so [`Config::apply`] can refuse the key by name and say what happened to it.
    /// The environment half is refused by [`crate::REMOVED_ENV`]; this is the file half, and both
    /// halves exist for the reason [`crate::consumed`]'s module doc gives — a key that validates,
    /// displays as effective and is read by nothing hands the operator positive confirmation of
    /// something false.
    pub state_dir: Option<PathBuf>,
    /// See [`Config::store_root`].
    pub store_root: Option<PathBuf>,
    /// See [`Config::log_dir`].
    pub log_dir: Option<PathBuf>,
    /// See [`Config::journal_dir`].
    pub journal_dir: Option<PathBuf>,
    /// See [`Config::backtest_addr`].
    pub backtest_addr: Option<String>,
    /// See [`Config::datahub_addr`].
    pub datahub_addr: Option<String>,
    /// See [`Config::tradehub_addr`].
    pub tradehub_addr: Option<String>,
    /// See [`Config::datahub_advertise_addr`].
    pub datahub_advertise_addr: Option<String>,
    /// See [`Config::tradehub_account_admin`].
    pub tradehub_account_admin: Option<String>,
    /// See [`Config::tradehub_advertise_addr`].
    pub tradehub_advertise_addr: Option<String>,
    /// See [`Config::node_addr`].
    pub node_addr: Option<String>,
    /// See [`Config::node_ack_wait_ms`].
    pub node_ack_wait_ms: Option<u32>,
    /// See [`Config::instance_origin`]. A raw string here, validated on [`Config::apply`] so a
    /// malformed tag names the FILE and the KEY rather than surfacing as a serde parse error.
    pub instance_origin: Option<String>,
    /// See [`Config::reconcile_policy`].
    pub reconcile_policy: Option<String>,
    /// See [`Config::reconcile_interval_ms`].
    pub reconcile_interval_ms: Option<u32>,
    /// See [`Config::reconcile_audit_ms`].
    pub reconcile_audit_ms: Option<u32>,
    /// See [`Config::reconcile_lookback_ms`].
    pub reconcile_lookback_ms: Option<u32>,
    /// See [`Config::reconcile_startup_delay_ms`].
    pub reconcile_startup_delay_ms: Option<u32>,
    /// See [`Config::reconcile_balance_tol_abs`].
    pub reconcile_balance_tol_abs: Option<f64>,
    /// See [`Config::reconcile_balance_tol_rel`].
    pub reconcile_balance_tol_rel: Option<f64>,
    /// See [`Config::tradehub_control_rate`].
    pub tradehub_control_rate: Option<f64>,
    /// See [`Config::pin_cores`].
    pub pin_cores: Option<String>,
    /// See [`Config::datahub_bind_addr`].
    pub datahub_bind_addr: Option<String>,
    /// See [`Config::gatewayhub_addr`].
    pub gatewayhub_addr: Option<String>,
    /// See [`Config::gatewayhub_connect_wait_ms`].
    pub gatewayhub_connect_wait_ms: Option<u32>,
    /// See [`Config::datahub_live_resident`].
    pub datahub_live_resident: Option<String>,
    /// See [`Config::datahub_catalog_ttl_secs`].
    pub datahub_catalog_ttl_secs: Option<u32>,
    /// See [`Config::recorder_silent_secs`].
    pub recorder_silent_secs: Option<u32>,
    /// See [`Config::recorder_tick_secs`].
    pub recorder_tick_secs: Option<u32>,
    /// See [`Config::recorder_flush_max_rows`].
    pub recorder_flush_max_rows: Option<u32>,
    /// See [`Config::recorder_flush_max_age_secs`].
    pub recorder_flush_max_age_secs: Option<u32>,
    /// See [`Config::datahub_chain_cadence_secs`].
    pub datahub_chain_cadence_secs: Option<u32>,
    /// See [`Config::datahub_options_poll_secs`].
    pub datahub_options_poll_secs: Option<u32>,
    /// See [`Config::journal_snapshot_every`].
    pub journal_snapshot_every: Option<u32>,
    /// See [`Config::paper_slippage`].
    pub paper_slippage: Option<f64>,
    /// See [`Config::preflight_disk_warn_mib`].
    pub preflight_disk_warn_mib: Option<u32>,
    /// See [`Config::preflight_disk_fail_mib`].
    pub preflight_disk_fail_mib: Option<u32>,
    /// See [`Config::inflight_confirm_ms`].
    pub inflight_confirm_ms: Option<u32>,
    /// See [`Config::strategy_state_save_ms`].
    pub strategy_state_save_ms: Option<u32>,
    /// See [`Config::node_idle_read_timeout_ms`].
    pub node_idle_read_timeout_ms: Option<u32>,
    /// See [`Config::md_heartbeat_ms`].
    pub md_heartbeat_ms: Option<u32>,
    /// See [`Config::recorder_retention_days_by_kind`].
    pub recorder_retention_days_by_kind: Option<String>,
    /// See [`Config::datahub_news_poll_secs`].
    pub datahub_news_poll_secs: Option<u32>,
    /// See [`Config::datahub_calendar_poll_secs`].
    pub datahub_calendar_poll_secs: Option<u32>,
    /// See [`Config::datahub_equity_poll_secs`].
    pub datahub_equity_poll_secs: Option<u32>,
}

impl Config {
    /// Fold one file's patch in. Paths are not checked for existence — a store root may legally
    /// be created on first write, and a loader that stats the filesystem stops being a pure
    /// function of its inputs.
    pub(crate) fn apply(&mut self, patch: ConfigPatch, file: &Path) -> Result<(), ConfigError> {
        // TOMBSTONE — see `ConfigPatch::state_dir`. Refused, never applied.
        if let Some(v) = patch.state_dir {
            return Err(ConfigError::Value {
                file: file.to_path_buf(),
                key: "state_dir".to_string(),
                message: format!(
                    "{} is no longer a setting — NOTHING read it. It named the DESKTOP's \
                     strategy-state sidecar directory, and the reader went with the desktop's local \
                     core. It is NOT the state ROOT (that is <settings>/state, a different \
                     directory). {}",
                    v.display(),
                    stale_row_removal("config.state_dir")
                ),
            });
        }
        if let Some(v) = patch.store_root {
            self.store_root = Some(v);
        }
        if let Some(v) = patch.log_dir {
            self.log_dir = Some(v);
        }
        if let Some(v) = patch.journal_dir {
            self.journal_dir = Some(v);
        }
        if let Some(v) = patch.backtest_addr {
            check_addr(file, "backtest_addr", &v)?;
            self.backtest_addr = Some(v);
        }
        if let Some(v) = patch.datahub_addr {
            check_addr(file, "datahub_addr", &v)?;
            self.datahub_addr = Some(v);
        }
        if let Some(v) = patch.tradehub_addr {
            check_addr(file, "tradehub_addr", &v)?;
            self.tradehub_addr = Some(v);
        }
        if let Some(v) = patch.datahub_advertise_addr {
            check_addr(file, "datahub_advertise_addr", &v)?;
            self.datahub_advertise_addr = Some(v);
        }
        if let Some(v) = patch.tradehub_account_admin {
            // ⚠ NOT validated against the three spellings here, and deliberately: an unrecognised
            // value is OFF rather than an error (see the field's doc), so refusing the FILE would
            // turn a typo into a daemon that will not start — strictly worse than a daemon that
            // keeps trading with one capability unarmed, which is the same trade
            // `BindDecision::Refuse` already makes. The daemon logs what it did not recognise.
            self.tradehub_account_admin = Some(v);
        }
        if let Some(v) = patch.tradehub_advertise_addr {
            check_addr(file, "tradehub_advertise_addr", &v)?;
            self.tradehub_advertise_addr = Some(v);
        }
        if let Some(v) = patch.node_addr {
            check_addr(file, "node_addr", &v)?;
            self.node_addr = Some(v);
        }
        if let Some(v) = patch.node_ack_wait_ms {
            if !(MIN_NODE_ACK_WAIT_MS..=MAX_NODE_ACK_WAIT_MS).contains(&v) {
                return Err(ConfigError::out_of_range(
                    file,
                    "node_ack_wait_ms",
                    v,
                    MIN_NODE_ACK_WAIT_MS,
                    MAX_NODE_ACK_WAIT_MS,
                    "the floor keeps a healthy tunnelled node from reading as \"outcome NOT known\" \
                     for a command it accepted; the ceiling is the control client's own reply \
                     timeout, past which the link is declared dead anyway",
                ));
            }
            self.node_ack_wait_ms = v;
        }
        if let Some(v) = patch.instance_origin {
            self.instance_origin = Some(parse_origin(file, &v)?);
        }
        // The decision-0111 P3 rows (see the comment above `Config::reconcile_policy`), each checked
        // against its bound. A value the reader would have ignored or misread is REFUSED here
        // instead, at the write and at the load: a row the daemon silently replaces with its
        // default is a setting the operator believes they filed and has not. One bound is of the
        // other kind, a value the reader WOULD honour but the safety it feeds could not survive
        // (`reconcile_startup_delay_ms`): refused the same way, never clamped.
        if let Some(v) = patch.reconcile_policy {
            if !RECONCILE_POLICIES.contains(&v.as_str()) {
                // The value is not echoed: a credential pasted into the wrong key must not be
                // printed by the refusal (`crate::preferences`' `one_of` makes the same choice).
                return Err(ConfigError::Value {
                    file: file.to_path_buf(),
                    key: "reconcile_policy".to_string(),
                    message: format!(
                        "… must be one of: {} (lower case, one spelling each)",
                        RECONCILE_POLICIES.join(", ")
                    ),
                });
            }
            self.reconcile_policy = Some(v);
        }
        if let Some(v) = patch.reconcile_interval_ms {
            self.reconcile_interval_ms = Some(v);
        }
        if let Some(v) = patch.reconcile_audit_ms {
            self.reconcile_audit_ms = Some(v);
        }
        if let Some(v) = patch.reconcile_lookback_ms {
            check_at_least_one(file, "reconcile_lookback_ms", v)?;
            self.reconcile_lookback_ms = Some(v);
        }
        if let Some(v) = patch.reconcile_startup_delay_ms {
            check_at_most(
                file,
                "reconcile_startup_delay_ms",
                v,
                MAX_RECONCILE_STARTUP_DELAY_MS,
                "the first reconcile pass must start inside the window in which a restored order \
                 still mutes its strategy's tagged resubmits (the core's refusal window, of which \
                 this maximum is half); write a shorter delay",
            )?;
            self.reconcile_startup_delay_ms = Some(v);
        }
        if let Some(v) = patch.reconcile_balance_tol_abs {
            check_non_negative(file, "reconcile_balance_tol_abs", v)?;
            self.reconcile_balance_tol_abs = Some(v);
        }
        if let Some(v) = patch.reconcile_balance_tol_rel {
            check_non_negative(file, "reconcile_balance_tol_rel", v)?;
            self.reconcile_balance_tol_rel = Some(v);
        }
        if let Some(v) = patch.tradehub_control_rate {
            if !(v.is_finite() && v > 0.0) {
                return Err(ConfigError::value(
                    file,
                    "tradehub_control_rate",
                    v,
                    "is not a rate: it must be a finite number of commands per second above 0 \
                     (the reader treats anything else as unset and runs the default 20)",
                ));
            }
            self.tradehub_control_rate = Some(v);
        }
        if let Some(v) = patch.pin_cores {
            check_non_blank(file, "pin_cores", &v)?;
            self.pin_cores = Some(v);
        }
        if let Some(v) = patch.datahub_bind_addr {
            check_addr(file, "datahub_bind_addr", &v)?;
            self.datahub_bind_addr = Some(v);
        }
        if let Some(v) = patch.gatewayhub_addr {
            check_addr(file, "gatewayhub_addr", &v)?;
            self.gatewayhub_addr = Some(v);
        }
        if let Some(v) = patch.gatewayhub_connect_wait_ms {
            check_min_max(
                file,
                "gatewayhub_connect_wait_ms",
                v,
                (GATEWAYHUB_CONNECT_WAIT_MIN_MS, GATEWAYHUB_CONNECT_WAIT_MAX_MS),
                "the wait is how long a tradehub that starts before the gatewayhub keeps asking for \
                 it: under a second it is not a wait, and over ten minutes it is a hung start to \
                 any supervisor; omit the key for the default",
            )?;
            self.gatewayhub_connect_wait_ms = Some(v);
        }
        if let Some(v) = patch.datahub_live_resident {
            check_non_blank(file, "datahub_live_resident", &v)?;
            self.datahub_live_resident = Some(v);
        }
        // The data plane's tuning rows: each range is a `pub const` above, and an out-of-range
        // value is refused naming the key and the range, never clamped. `0` is the one value the
        // silence threshold takes from outside its range, because it is the flag's own "off".
        if let Some(v) = patch.datahub_catalog_ttl_secs {
            check_between(
                file,
                "datahub_catalog_ttl_secs",
                v,
                &DATAHUB_CATALOG_TTL_SECS,
                "below the floor a full burst of fetches out-costs the lane's own steady state, and \
                 above a week a listing is stale; omit the key for six hours",
            )?;
            self.datahub_catalog_ttl_secs = Some(v);
        }
        if let Some(v) = patch.recorder_silent_secs {
            if v != 0 {
                check_between(
                    file,
                    "recorder_silent_secs",
                    v,
                    &RECORDER_SILENT_SECS,
                    "write 0 to switch the silence watchdog off, or a threshold in this range; \
                     omit the key for 300",
                )?;
            }
            self.recorder_silent_secs = Some(v);
        }
        if let Some(v) = patch.recorder_tick_secs {
            check_between(
                file,
                "recorder_tick_secs",
                v,
                &RECORDER_TICK_SECS,
                "the tick re-resolves the symbol set against the venues' directory APIs and paces \
                 every watchdog verdict; omit the key for 30",
            )?;
            self.recorder_tick_secs = Some(v);
        }
        if let Some(v) = patch.recorder_flush_max_rows {
            check_between(
                file,
                "recorder_flush_max_rows",
                v,
                &RECORDER_FLUSH_MAX_ROWS,
                "every flush is a store commit and every buffered row is lost on a hard kill; omit \
                 the key for 5000",
            )?;
            self.recorder_flush_max_rows = Some(v);
        }
        if let Some(v) = patch.recorder_flush_max_age_secs {
            check_between(
                file,
                "recorder_flush_max_age_secs",
                v,
                &RECORDER_FLUSH_MAX_AGE_SECS,
                "every flush is a store commit and every buffered row is lost on a hard kill; omit \
                 the key for 30",
            )?;
            self.recorder_flush_max_age_secs = Some(v);
        }
        if let Some(v) = patch.datahub_chain_cadence_secs {
            check_between(
                file,
                "datahub_chain_cadence_secs",
                v,
                &DATAHUB_CHAIN_CADENCE_SECS,
                "the options lane polls Deribit every few seconds, so a narrower bucket records \
                 every poll; omit the key for 60",
            )?;
            self.datahub_chain_cadence_secs = Some(v);
        }
        if let Some(v) = patch.datahub_options_poll_secs {
            check_between(
                file,
                "datahub_options_poll_secs",
                v,
                &DATAHUB_OPTIONS_POLL_SECS,
                "every poll spends Deribit's public API budget from this box's IP; omit the key \
                 for 12",
            )?;
            self.datahub_options_poll_secs = Some(v);
        }
        if let Some(v) = patch.journal_snapshot_every {
            check_at_least_one(file, "journal_snapshot_every", v)?;
            self.journal_snapshot_every = Some(v);
        }
        // The trading-mount batch (see the comment above `Config::paper_slippage`). Each bound
        // refuses a value the mount would honour into something no operator meant — a unit slip, a
        // request storm — rather than clamping it.
        if let Some(v) = patch.paper_slippage {
            if !(v.is_finite() && (0.0..=MAX_PAPER_SLIPPAGE).contains(&v)) {
                return Err(ConfigError::value(
                    file,
                    "paper_slippage",
                    v,
                    &format!(
                        "is not a slippage: it must be a finite fraction of price from 0 to \
                         {MAX_PAPER_SLIPPAGE} (0.0002 is 2 basis points); omit the key for the \
                         default"
                    ),
                ));
            }
            self.paper_slippage = Some(v);
        }
        // The two preflight disk floors are judged TOGETHER: each against its own range, then as a
        // pair. A floor the patch does not name counts at its DEFAULT, so one key written alone can
        // still be refused for its twin — and the refusal says which, rather than letting a `warn`
        // below the `fail` floor sit in the store as a warning that can never be reached.
        if patch.preflight_disk_warn_mib.is_some() || patch.preflight_disk_fail_mib.is_some() {
            if let Some(v) = patch.preflight_disk_warn_mib {
                check_disk_mib(file, "preflight_disk_warn_mib", v)?;
            }
            if let Some(v) = patch.preflight_disk_fail_mib {
                check_disk_mib(file, "preflight_disk_fail_mib", v)?;
            }
            let warn = patch
                .preflight_disk_warn_mib
                .or(self.preflight_disk_warn_mib)
                .unwrap_or(DEFAULT_PREFLIGHT_DISK_WARN_MIB);
            let fail = patch
                .preflight_disk_fail_mib
                .or(self.preflight_disk_fail_mib)
                .unwrap_or(DEFAULT_PREFLIGHT_DISK_FAIL_MIB);
            if warn <= fail {
                let (key, message) = if patch.preflight_disk_warn_mib.is_some() {
                    (
                        "preflight_disk_warn_mib",
                        format!("{warn} is not above preflight_disk_fail_mib ({fail})"),
                    )
                } else {
                    (
                        "preflight_disk_fail_mib",
                        format!("{fail} is not below preflight_disk_warn_mib ({warn})"),
                    )
                };
                return Err(ConfigError::Value {
                    file: file.to_path_buf(),
                    key: key.to_string(),
                    message: format!(
                        "{message}: the warn floor must be the higher of the two (a key not written \
                         is at its default, {DEFAULT_PREFLIGHT_DISK_WARN_MIB} and \
                         {DEFAULT_PREFLIGHT_DISK_FAIL_MIB} MiB), so move the pair one key at a \
                         time in the order that keeps warn above fail"
                    ),
                });
            }
            if let Some(v) = patch.preflight_disk_warn_mib {
                self.preflight_disk_warn_mib = Some(v);
            }
            if let Some(v) = patch.preflight_disk_fail_mib {
                self.preflight_disk_fail_mib = Some(v);
            }
        }
        if let Some(v) = patch.inflight_confirm_ms {
            check_cadence_ms(
                file,
                "inflight_confirm_ms",
                v,
                MIN_INFLIGHT_CONFIRM_MS,
                MAX_INFLIGHT_CONFIRM_MS,
                "a faster cadence is a request storm against a venue already slow to answer, a \
                 slower one is behind the stuck-order ladder it exists to beat",
            )?;
            self.inflight_confirm_ms = Some(v);
        }
        if let Some(v) = patch.strategy_state_save_ms {
            check_cadence_ms(
                file,
                "strategy_state_save_ms",
                v,
                MIN_STRATEGY_STATE_SAVE_MS,
                MAX_STRATEGY_STATE_SAVE_MS,
                "every save is an fsynced file write on the core thread, and past an hour a crash \
                 loses more than the clean-shutdown save would have kept",
            )?;
            self.strategy_state_save_ms = Some(v);
        }
        // The two connection-timing rows. Both are BOUNDED on both sides and refused outside the
        // range, never clamped: a value the daemon would honour but that clips a legitimate client
        // (idle) or leaves a dead link undetected for minutes (heartbeat) is a setting the operator
        // believes they filed and whose consequence they did not choose.
        if let Some(v) = patch.node_idle_read_timeout_ms {
            check_min_max(
                file,
                "node_idle_read_timeout_ms",
                v,
                (NODE_IDLE_READ_TIMEOUT_MIN_MS, NODE_IDLE_READ_TIMEOUT_MAX_MS),
                "the idle bound must outlast the slowest client that keeps a connection warm \
                 between requests, and not be so long that a dead peer holds a connection thread \
                 for hours; omit the key for the default",
            )?;
            self.node_idle_read_timeout_ms = Some(v);
        }
        if let Some(v) = patch.md_heartbeat_ms {
            check_min_max(
                file,
                "md_heartbeat_ms",
                v,
                (MD_HEARTBEAT_MIN_MS, MD_HEARTBEAT_MAX_MS),
                "the heartbeat is the proof a quiet stream is alive: zero would write continuously, \
                 and a period beyond the maximum makes the client's three-period read deadline too \
                 slow to be a liveness signal; omit the key for the default",
            )?;
            self.md_heartbeat_ms = Some(v);
        }
        if let Some(v) = patch.recorder_retention_days_by_kind {
            // Parsed here only to REFUSE; the row keeps its text and the reader parses it again
            // with the same function (`crate::retention`), so the two cannot disagree.
            crate::retention::parse_retention_days_by_kind(&v).map_err(|message| {
                ConfigError::Value {
                    file: file.to_path_buf(),
                    key: "recorder_retention_days_by_kind".to_string(),
                    message,
                }
            })?;
            self.recorder_retention_days_by_kind = Some(v);
        }
        if let Some(v) = patch.datahub_news_poll_secs {
            check_feed_poll_secs(file, "datahub_news_poll_secs", v)?;
            self.datahub_news_poll_secs = Some(v);
        }
        if let Some(v) = patch.datahub_calendar_poll_secs {
            check_feed_poll_secs(file, "datahub_calendar_poll_secs", v)?;
            self.datahub_calendar_poll_secs = Some(v);
        }
        if let Some(v) = patch.datahub_equity_poll_secs {
            check_feed_poll_secs(file, "datahub_equity_poll_secs", v)?;
            self.datahub_equity_poll_secs = Some(v);
        }
        Ok(())
    }
}

/// A tool feed's poll cadence, in seconds: [`MIN_FEED_POLL_SECS`] to [`MAX_FEED_POLL_SECS`], both
/// ends taken. Outside it the value is REFUSED, never clamped — a clamp is a cadence the operator
/// believes they filed and has not, and for the keyed providers it is a different spend of a quota
/// only they know.
fn check_feed_poll_secs(file: &Path, key: &str, v: u32) -> Result<(), ConfigError> {
    let why = if v < MIN_FEED_POLL_SECS {
        format!(
            "is below the minimum {MIN_FEED_POLL_SECS} — a failed poll is retried after \
             {MIN_FEED_POLL_SECS} seconds and the retry doubles up to the cadence, so the cadence \
             cannot be shorter than that first retry"
        )
    } else if v > MAX_FEED_POLL_SECS {
        format!(
            "exceeds the allowed maximum {MAX_FEED_POLL_SECS} (one day) — a feed polled less \
             often than that is stale before it is asked again"
        )
    } else {
        return Ok(());
    };
    Err(ConfigError::Value {
        file: file.to_path_buf(),
        key: key.to_string(),
        message: format!("{v} {why}"),
    })
}

/// A count or a window that means nothing at zero — the reader ignores a `0` and runs its default,
/// so a `0` row would be a setting that silently changes nothing.
fn check_at_least_one(file: &Path, key: &str, v: u32) -> Result<(), ConfigError> {
    if v >= 1 {
        return Ok(());
    }
    Err(ConfigError::Value {
        file: file.to_path_buf(),
        key: key.to_string(),
        message: "0 is not a usable value — it must be at least 1; omit the key for the default"
            .to_string(),
    })
}

/// A count or a window with a CEILING: above `max` the value is one the reader would honour and the
/// behaviour it feeds could not survive, so it is refused rather than clamped (a clamp is a setting
/// the operator believes they filed and has not). `why` says what the ceiling protects.
fn check_at_most(file: &Path, key: &str, v: u32, max: u32, why: &str) -> Result<(), ConfigError> {
    if v <= max {
        return Ok(());
    }
    Err(ConfigError::Value {
        file: file.to_path_buf(),
        key: key.to_string(),
        message: format!("{v} exceeds the allowed maximum {max} — {why}"),
    })
}

/// A cadence with an OFF word: `0` switches the feature off (the key's own spelling of "not wired",
/// the same word `config.reconcile_interval_ms` uses) and any other value must sit in
/// `min..=max`. Refused rather than clamped: `why` says what the range protects.
fn check_cadence_ms(
    file: &Path,
    key: &str,
    v: u32,
    min: u32,
    max: u32,
    why: &str,
) -> Result<(), ConfigError> {
    if v == 0 || (min..=max).contains(&v) {
        return Ok(());
    }
    Err(ConfigError::Value {
        file: file.to_path_buf(),
        key: key.to_string(),
        message: format!(
            "{v} is outside the allowed range {min}..={max} ms (or 0 for off) — {why}"
        ),
    })
}

/// A preflight disk floor, in MiB: at least 1 (a `0` floor can never be crossed, so it is a check
/// that was switched off by accident, not a floor) and at most [`MAX_PREFLIGHT_DISK_MIB`].
fn check_disk_mib(file: &Path, key: &str, v: u32) -> Result<(), ConfigError> {
    if (1..=MAX_PREFLIGHT_DISK_MIB).contains(&v) {
        return Ok(());
    }
    Err(ConfigError::Value {
        file: file.to_path_buf(),
        key: key.to_string(),
        message: format!(
            "{v} is outside the allowed range 1..={MAX_PREFLIGHT_DISK_MIB} MiB — the floor is a \
             free-space figure in MiB, and 0 can never be crossed"
        ),
    })
}

/// A count or a window that must lie in `range` (inclusive). Both ends are refused, with the range
/// spelled out: a value the reader would honour but the behaviour it feeds could not survive is a
/// setting the operator must be told about, so it is never clamped. `why` says what the range
/// protects and what omitting the key does.
fn check_between(
    file: &Path,
    key: &str,
    v: u32,
    range: &RangeInclusive<u32>,
    why: &str,
) -> Result<(), ConfigError> {
    if range.contains(&v) {
        return Ok(());
    }
    Err(ConfigError::Value {
        file: file.to_path_buf(),
        key: key.to_string(),
        message: format!(
            "{v} is outside the allowed range {} to {} — {why}",
            range.start(),
            range.end()
        ),
    })
}

/// A window with a FLOOR and a CEILING, `range` being `(min, max)` inclusive: outside it the value is
/// one the reader would honour and the behaviour it feeds could not survive, so it is refused rather
/// than clamped. Below the floor says "minimum", above the ceiling is [`check_at_most`]'s sentence.
fn check_min_max(
    file: &Path,
    key: &str,
    v: u32,
    range: (u32, u32),
    why: &str,
) -> Result<(), ConfigError> {
    let (min, max) = range;
    if v < min {
        return Err(ConfigError::Value {
            file: file.to_path_buf(),
            key: key.to_string(),
            message: format!("{v} is below the allowed minimum {min} — {why}"),
        });
    }
    check_at_most(file, key, v, max, why)
}

/// A money tolerance: finite and not negative. A negative band flags every balance as drifted and a
/// non-finite one flags none (the comparison is `drift.abs() > band`).
fn check_non_negative(file: &Path, key: &str, v: f64) -> Result<(), ConfigError> {
    if v.is_finite() && v >= 0.0 {
        return Ok(());
    }
    Err(ConfigError::value(file, key, v, "must be a finite number at or above 0"))
}

/// A text value in the reader's own grammar: this crate checks only that it says something.
fn check_non_blank(file: &Path, key: &str, v: &str) -> Result<(), ConfigError> {
    if !v.trim().is_empty() {
        return Ok(());
    }
    Err(ConfigError::Value {
        file: file.to_path_buf(),
        key: key.to_string(),
        message: "\"\" is blank — omit the key to keep the default".to_string(),
    })
}

/// The FILE-layer origin parse: `vike_model::InstanceOrigin`'s own rule, wrapped in this layer's
/// error shape so a bad tag names the file and the key. The rule itself is NOT restated here —
/// `crates/vike-model/src/instance_origin.rs`'s `InstanceOrigin::parse` is the single authority
/// for what a tag may be, and its `OriginError` already explains each refusal in terms an operator
/// can act on.
fn parse_origin(file: &Path, v: &str) -> Result<vike_model::InstanceOrigin, ConfigError> {
    vike_model::InstanceOrigin::parse(v).map_err(|e| ConfigError::Value {
        file: file.to_path_buf(),
        key: "instance_origin".to_string(),
        message: e.to_string(),
    })
}

/// The shared `host:port` rule every override layer enforces: non-blank, and carrying a `:`
/// separator. Deliberately NOT a full `SocketAddr` parse: `vike-datahub` is reached through an
/// SSH tunnel and a hostname is a legitimate value, so resolving here would reject a working
/// config.
///
/// This is the ONE place the rule is spelled. `check_addr` (the row layer) and `apply_cli`'s
/// CLI-layer check (below) each wrap this in their own [`ConfigError`] shape — a different variant
/// with different fields per layer, which is why they cannot simply call one another — but the boolean itself lives here exactly once, so a validation gap (a
/// missing blank check, a dropped `!`) can no longer exist in one copy while the others stay
/// fixed.
fn is_host_port(v: &str) -> bool {
    !v.trim().is_empty() && v.contains(':')
}

/// An address must at least be non-blank and carry a `host:port` separator. See [`is_host_port`]
/// for the shared rule.
fn check_addr(file: &Path, key: &str, v: &str) -> Result<(), ConfigError> {
    if is_host_port(v) {
        return Ok(());
    }
    Err(ConfigError::Value {
        file: file.to_path_buf(),
        key: key.to_string(),
        message: format!("{v:?} is not a host:port address"),
    })
}

// ⚠ `Config`'s environment layer — its `apply_env`, and the `check_env_addr` twin of `check_addr` it
// called — is DELETED (decision 0111): every key is its `config.*` row, and each former variable is
// a `crate::REMOVED_ENV` refusal.

impl CliOverride for Config {
    fn apply_cli(&mut self, cli: &CliOverrides) -> Result<(), ConfigError> {
        if let Some(v) = &cli.store_root {
            self.store_root = Some(PathBuf::from(v));
        }
        if let Some(v) = &cli.log_dir {
            self.log_dir = Some(PathBuf::from(v));
        }
        if let Some(v) = &cli.datahub_addr {
            // Same rule as `check_addr` — see `is_host_port` — wrapped in the
            // CLI layer's own error shape (a flag name, not a file/key pair).
            if !is_host_port(v) {
                return Err(ConfigError::Cli {
                    flag: "addr".to_string(),
                    value: v.clone(),
                    message: "expected a host:port address".to_string(),
                });
            }
            self.datahub_addr = Some(v.clone());
        }
        Ok(())
    }
}

#[path = "config_tests.rs"]
#[cfg(test)]
mod config_tests;
