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
//! `VIKE_HIST_STORE` had FIVE rows in `vike_ops::settings::SETTINGS` — vike-app, vike-backtest,
//! vike-backfill, vike-datahub and vike-studio each read it with a *different* fallback chain. One
//! field with one default is the fix; the divergent fallbacks became the callers' business.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use crate::error::ConfigError;
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

/// The COMPUTE daemon's default address — where `vike-backend backtest --addr` binds, and where a
/// client of the compute verbs (`vike-cli research study` today) dials when nothing else answers.
///
/// ⚠ **7880, and the two digits are the whole point: 7879 is TAKEN, by the live order-signing
/// daemon.** [`Config::node_addr`]'s own doc names it ("the daemon binds `127.0.0.1:7879` on its
/// own box") — the process that signs orders. A compute
/// client defaulting there would open a connection to the TRADING socket and speak a protocol it
/// does not serve. The data server has 7878, the trading daemon 7879, so the compute daemon takes
/// 7880. §0 of `docs/superpowers/specs/2026-09-09-datahub-market-data-wire-design.md` (ruling 7,
/// as corrected by the owner) is the authority.
///
/// ⚠ `crates/vike-ops/src/settings/rows/config.rs`'s `VIKE_BACKTEST_ADDR` row and
/// `deploy/vike-backtest.service`'s `Environment=` line both RESTATE the number as text rather
/// than reading this constant, and a restated default that contradicts the constant is the
/// settings-registry failure the workspace already gates for elsewhere. The unit's line is the
/// sharper of the two — it is what a deployed box actually binds, and pointing the compute daemon
/// at 7879 on a box that also runs the trading node is the collision this constant exists to
/// avoid.
///
/// It is the LAST rung of the ladder, never the only one: `--addr <v>` →
/// [`Config::backtest_addr`] → this. On a deployed box the unit's `Environment=` line (the
/// daemon's own half of ruling 7) sets the address once and nobody types it again.
pub const DEFAULT_BACKTEST_ADDR: &str = "127.0.0.1:7880";

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

    /// Directory for the live core's mmap write-ahead command journal.
    ///
    /// Was `VIKE_JOURNAL_DIR`.
    pub journal_dir: Option<PathBuf>,

    // ⚠ `state_dir` (the desktop's strategy-state SIDECAR directory) is DELETED, and
    // `VIKE_STATE_DIR` is a `crate::REMOVED_ENV` tombstone that refuses startup. ⚠ It is NOT the
    // state ROOT (`<settings>/state`) that vike-tradehub, vike-app-core and vike-studio resolve —
    // see `vike_model::paths::state_path`, which records the collision.
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

    /// The COMPUTE daemon's address (`host:port`) — where `vike-backend backtest --addr` BINDS on
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
    /// compute verbs off `VIKE_DATAHUB_ADDR` is one key added, not a second convention."* The cost
    /// it accepts is the one [`Config::node_addr`] argues about its own sibling: behind a tunnel the
    /// daemon's bind and the client's dial are different facts that happen to agree, and a
    /// deployment where they do not needs the client's box to set this key to the tunnel mouth.
    /// Nothing tunnels between a compute daemon and the store beside it, which is why that cost is
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
    /// | `loopback` | *this listener is on loopback and reached through a tunnel* | **CHECKS it** against `vike_tradehub::server::bind_exposure`; a wide bind REFUSES the capability at boot, naming this key and the address |
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
    /// - `node_addr` (this key) — a CLIENT's box, DIAL. Where this machine reaches a tradehub node.
    /// - [`Config::backtest_addr`] — BOTH boxes, and the only one of the five that is read from
    ///   each end: the COMPUTE daemon binds it and a compute client dials it.
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

    /// The delay before the first reconcile pass after a start, in milliseconds. `None` — the
    /// default — is 2 000. Was `VIKE_RECONCILE_STARTUP_DELAY_MS`.
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
    /// `flags.datahub_allow_public_bind` consents and node keys are set. Was
    /// `VIKE_DATAHUB_ADDR` on the data server.
    pub datahub_bind_addr: Option<String>,

    /// The live market-data plane's RESIDENT set, `venue:symbol:lane,…`, non-blank — the keys the
    /// datahub holds subscribed whether or not a client asks. Read only while the plane is armed
    /// (`flags.datahub_live`); a row inside it that does not parse is a startup WARNING naming the
    /// row, never a refusal (`docs/decisions/0013-degrade-vs-refuse.md`). `None` — the default —
    /// pins nothing. Was `VIKE_DATAHUB_LIVE_RESIDENT`.
    pub datahub_live_resident: Option<String>,

    /// The write-ahead journal's snapshot cadence, in commands, at least 1, used when the journal is
    /// enabled by a directory ([`Config::journal_dir`]) and no run profile
    /// names one. `None` — the default — is 1 024. Was `VIKE_JOURNAL_SNAPSHOT_EVERY`.
    pub journal_snapshot_every: Option<u32>,
}

impl Default for Config {
    /// Every field's absence means "the caller's own fallback applies" — including
    /// `datahub_addr`, whose absence is itself the answer ("no datahub — local store") now that
    /// vike-desktop's Studio branches on its presence.
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
            datahub_live_resident: None,
            journal_snapshot_every: None,
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
    /// See [`Config::datahub_live_resident`].
    pub datahub_live_resident: Option<String>,
    /// See [`Config::journal_snapshot_every`].
    pub journal_snapshot_every: Option<u32>,
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
                     directory). Delete the key",
                    v.display()
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
        if let Some(v) = patch.instance_origin {
            self.instance_origin = Some(parse_origin(file, &v)?);
        }
        // The decision-0111 P3 rows (see the comment above `Config::reconcile_policy`), each checked
        // against its bound. A value the reader would have ignored or misread is REFUSED here
        // instead, at the write and at the load: a row the daemon silently replaces with its
        // default is a setting the operator believes they filed and has not.
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
        if let Some(v) = patch.datahub_live_resident {
            check_non_blank(file, "datahub_live_resident", &v)?;
            self.datahub_live_resident = Some(v);
        }
        if let Some(v) = patch.journal_snapshot_every {
            check_at_least_one(file, "journal_snapshot_every", v)?;
            self.journal_snapshot_every = Some(v);
        }
        Ok(())
    }
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
