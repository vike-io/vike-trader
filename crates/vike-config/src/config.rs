//! [`Config`] — DEPLOYMENT settings: where things live on this box. Full override chain.
//!
//! The distinguishing question is "would a second machine running the same strategy need a
//! different value?". Store roots, log directories and listen addresses all answer yes, which is
//! why they carry the full `defaults -> file -> env -> CLI` chain: a container overrides a path
//! with an env var, an operator overrides a port with a flag for one run, and neither is a risk
//! decision.
//!
//! Contrast [`crate::Preferences`], whose values would be the SAME on the second machine (they
//! express taste), and [`crate::Policy`], which stops at the file layer on purpose.
//!
//! ## Field provenance
//!
//! Each field replaces a variable that exists today, named in its doc comment. Two of them are
//! the reason the settings program was written at all:
//!
//! - **`VIKE_HIST_STORE` had FIVE rows** in `vike_ops::settings::SETTINGS` — vike-app,
//!   vike-backtest, vike-backfill, vike-datahub and vike-studio each read it with a *different*
//!   fallback chain. One field with one default is the fix; the divergent fallbacks become the
//!   callers' business, not the variable's. vike-app's row is the first to have MOVED here: its
//!   `studio_store_root` takes [`Config::store_root`] now, and passes it to the same
//!   `vike_model::store_path::resolve_store_root` precedence it always used.
//! - **`VIKE_LOG_DIR` and `VIKE_JOURNAL_DIR`** are each read from several crates, some as a
//!   direct `env::var` deep inside a library (`Layer::Library`, the registry's STEP-2 work list).
//!   Reading them HERE, from an injected map, is that work list's target state.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::error::ConfigError;
use crate::layers::{CliOverride, CliOverrides, EnvOverride, get};

/// `VIKE_HIST_STORE` — the historical/tick store root.
pub const STORE_ROOT_ENV: &str = "VIKE_HIST_STORE";
/// `VIKE_LOG_DIR` — where the JSON daily-rolling log file is written.
pub const LOG_DIR_ENV: &str = "VIKE_LOG_DIR";
/// `VIKE_JOURNAL_DIR` — the live core's write-ahead command journal directory.
pub const JOURNAL_DIR_ENV: &str = "VIKE_JOURNAL_DIR";
/// `VIKE_DATAHUB_ADDR` — the data-service listen/connect address.
pub const DATAHUB_ADDR_ENV: &str = "VIKE_DATAHUB_ADDR";
/// `VIKE_BACKTEST_ADDR` — the COMPUTE daemon's listen/connect address (`vike-backend backtest
/// --addr`). One digit from its datahub sibling above, and it is a SEPARATE key rather than a
/// second meaning for that one: ruling 7 of
/// `docs/superpowers/specs/2026-09-09-datahub-market-data-wire-design.md` split one served surface
/// into two daemons, and two daemons on one box cannot share one bind address.
pub const BACKTEST_ADDR_ENV: &str = "VIKE_BACKTEST_ADDR";
/// `VIKE_TRADEHUB_ADDR` — the headless trading daemon's control listen address.
pub const TRADEHUB_ADDR_ENV: &str = "VIKE_TRADEHUB_ADDR";
/// `VIKE_DATAHUB_ADVERTISE_ADDR` — the datahub dial address the trading daemon ADVERTISES.
pub const DATAHUB_ADVERTISE_ADDR_ENV: &str = "VIKE_DATAHUB_ADVERTISE_ADDR";
/// `VIKE_TRADEHUB_ACCOUNT_ADMIN` — the THREE-VALUED barrier declaration for the node's account
/// verbs. See [`Config::tradehub_account_admin`].
pub const TRADEHUB_ACCOUNT_ADMIN_ENV: &str = "VIKE_TRADEHUB_ACCOUNT_ADMIN";
/// `VIKE_TRADEHUB_ADVERTISE_ADDR` — the address the trading daemon reports as ITS OWN, overriding
/// the one it discovers from the routing table. ⚠ Not a dial address and not a sibling of the two
/// above: it answers "which box is this", not "where do I connect".
pub const TRADEHUB_ADVERTISE_ADDR_ENV: &str = "VIKE_TRADEHUB_ADVERTISE_ADDR";

/// `VIKE_INSTANCE_ORIGIN` — this deployment's origin tag, stamped into every client order id.
pub const INSTANCE_ORIGIN_ENV: &str = "VIKE_INSTANCE_ORIGIN";

/// The `vike-datahub` default listen address, as its `main.rs` reads it today.
pub const DEFAULT_DATAHUB_ADDR: &str = "127.0.0.1:7878";

/// The COMPUTE daemon's default address — where `vike-backend backtest --addr` binds, and where a
/// client of the compute verbs (`vike-cli research study` today) dials when nothing else answers.
///
/// ⚠ **7880, and the two digits are the whole point: 7879 is TAKEN, by the live order-signing
/// daemon.** [`Config::node_addr`]'s own doc names it ("the daemon binds `127.0.0.1:7879` on its
/// own box"), and `ss -ltnp` on the production box on 2026-09-10 confirmed it —
/// `127.0.0.1:7879 users:(("vike-backend",pid=…))`, the process that signs orders. A compute
/// client defaulting there would open a connection to the TRADING socket and speak a protocol it
/// does not serve. The data server has 7878, the trading daemon 7879, so the compute daemon takes
/// 7880. §0 of `docs/superpowers/specs/2026-09-09-datahub-market-data-wire-design.md` (ruling 7,
/// as corrected by the owner) is the authority.
///
/// ⚠ **This constant was `127.0.0.1:7879` for exactly one commit range, and everything derived
/// from it in that window states the old number.** Ruling 7 landed the daemon side spelling the
/// ruling's original digit; the correction (#1744) fixed the SPEC and deliberately left the code
/// to the client PR that had copied the same number. So the sweep that comes with this change is
/// part of it, not tidying: `crates/vike-ops/src/settings.rs`'s `VIKE_BACKTEST_ADDR` row and
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

    // ⚠ `state_dir` stood here — the desktop's strategy-state SIDECAR directory. It is DELETED,
    // and `VIKE_STATE_DIR` is a `crate::REMOVED_ENV` tombstone that refuses startup. The reader was
    // `crates/vike-desktop/src/main.rs`'s `state_dir_path`, which went with the desktop cut's local
    // core, and this crate's own `crate::consumed` doc is the argument for why an unread key is
    // worse than an absent one rather than merely untidy. ⚠ It is NOT the `VIKE_STATE_ROOT` state
    // ROOT that vike-tradehub, vike-app-core and vike-studio resolve, and it never was — see
    // `vike_model::state_path`, which records the collision.
    /// CLIENT-side DIAL address of a `vike-datahub` data server (`host:port`). **Set → vike-app's
    /// Studio takes its history from that server over RPC** (a DataFusion-free
    /// `RemoteHistStore`; the Studio's run Backend also defaults to Remote at the same address);
    /// **unset — the default — → the Studio opens the LOCAL store** at `config.store_root`'s
    /// resolution, exactly as before the split-plane program (2026-08-18). Presence IS the
    /// branch, which is why this field lost its old always-set default.
    ///
    /// ⚠ Deliberately DISTINCT from [`Config::tradehub_addr`]: that one is the DAEMON'S BIND
    /// address (where `vike-tradehub` LISTENS on this machine); this one is where a CLIENT
    /// DIALS a datahub server, usually on another machine. The `vike-datahub` server bin does
    /// not read settings at all — its own listen address is its `VIKE_DATAHUB_ADDR` environment
    /// read, defaulting to [`DEFAULT_DATAHUB_ADDR`].
    ///
    /// Was `VIKE_DATAHUB_ADDR` (the variable still overrides the file layer here).
    pub datahub_addr: Option<String>,

    /// The COMPUTE daemon's address (`host:port`) — where `vike-backend backtest --addr` BINDS on
    /// this box, and where a client DIALS it. `None` = [`DEFAULT_BACKTEST_ADDR`].
    ///
    /// ⚠ **This line NAMED THE CLIENTS and two of the names were retired verbs**
    /// (`vike-cli sweep`, ruling 13; `vike-cli walkforward`, decision 3 of the backtest-CLI-surface
    /// design — both fold into `vike-cli backtest run`, which routes on the PROFILE). A list of
    /// callers in a setting's doc is a second roster, and this one rotted the ordinary way. The
    /// answer a reader needs is a COMMAND rather than a list, and
    /// `crates/vike-cli/src/cmd/backtest.rs` carries the one this repository actually RUNS —
    /// `crates/vike-ops/tests/unrun_command_gate.rs` has a row for it. Read it there; a second copy
    /// here would be the very thing this paragraph is correcting.
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
    /// ⚠ That function MOVED there from `crates/vike-cli/src/cmd/study.rs`, which is now a CALLER:
    /// every verb that dials this daemon does so on this key, so a second copy of the fold could
    /// disagree about a blank rung and aim one of them at `7878` or `7879`. ⚠ The `mcp` server's
    /// compute tools joined that set in stage 7 and read the key from NOWHERE before it — the one
    /// residual §18 row 9 of the backtest-CLI-surface design recorded against this key.
    ///
    /// ⚠ **This field was DECLARED TWICE for the length of one merge, and the reason is worth
    /// keeping.** Rulings 7 and 16 are the two halves of one wire — the daemon that binds and the
    /// client that dials — and each half added this field to `Config`, to `ConfigPatch`, to
    /// `Default`, to `apply`, to `setting_keys` and to `CONSUMPTION` on its own branch, correctly,
    /// knowing nothing of the other. git saw no textual conflict in any of the six: the two
    /// insertions landed at different offsets. Four of them are hard compile errors (a duplicate
    /// struct field, a duplicate initializer field, and a second `if let Some(v) = patch.…` that
    /// moves an already-moved value) and two are silent — `setting_keys` would have printed this
    /// key twice in `vike-cli config show`, once claiming an environment layer and once denying
    /// it, and `consumer_of` is a `find`, so the second `CONSUMPTION` row was unreachable. Only
    /// the merge could see any of it.
    ///
    /// Ruling 7 of `docs/superpowers/specs/2026-09-09-datahub-market-data-wire-design.md`; the
    /// client half is ruling 16.
    ///
    /// Env layer: [`BACKTEST_ADDR_ENV`].
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
    /// Never read by the datahub server itself (its listen address stays its own
    /// `VIKE_DATAHUB_ADDR` read); consumed by `vike-tradehub`'s `start_observe_server`.
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
    /// calls `local_addr`; `deploy/docker/Dockerfile` sets `VIKE_TRADEHUB_ALLOW_PUBLIC_BIND=1`
    /// unconditionally (Docker publishes to the container's `eth0`), so a bind-keyed rule would
    /// refuse every containerised install; and the per-frame PEER answers backwards inside a
    /// container correctly published to `127.0.0.1`. `docs/decisions/0026` forbids the inference
    /// cure outright. So the sound predicate is an operator DECLARATION — the same shape
    /// `flags.tradehub_allow_public_bind` already is, under its own rule that *an address cannot
    /// be its own consent, because typing the address is the mistake*. That rule cuts both ways:
    /// an address cannot be its own refusal either.
    ///
    /// ⚠ **A CONFIG key rather than a flag, and 0065 §3c says `flags.toml`.** `Flags` is a
    /// BOOLEAN plane by construction — `FLAG_META`, `FlagsPatch` and `parse_flag` all are — and a
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
    /// ⚠ **No environment layer, and that is a decision rather than an omission.** Every other key
    /// in this struct has one because it configures a DEPLOYMENT, which is what an `Environment=`
    /// line and a container `-e` flag exist to move. This one configures a per-invocation default
    /// that already has a per-invocation override: `--node` wins over it, is typed by the person
    /// running the command, and appears in the command's own usage. A third spelling would buy no
    /// capability and add one more place "which node am I talking to?" can be answered from.
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
    /// misconfiguration the tag exists to reveal. And it is not a ceiling: it can arm nothing,
    /// widen nothing and place no order, so the environment layer `policy` refuses is safe here,
    /// and is in fact the primary way it will be set (two containers from one image, differing by
    /// one `VIKE_INSTANCE_ORIGIN=`).
    ///
    /// `None` — the default — mints exactly the ids this workspace has always minted and leaves
    /// every reconcile fold decision byte-identical. `vike_model::instance_origin`'s module doc
    /// argues the wire format and why a DECLARED tag beat every derived candidate;
    /// `docs/ops/double-live-instances.md` is the operator page.
    pub instance_origin: Option<vike_model::InstanceOrigin>,
}

impl Default for Config {
    /// Every field's absence means "the caller's own fallback applies" — including
    /// `datahub_addr`, whose absence is itself the answer ("no datahub — local store") now that
    /// vike-app's Studio branches on its presence.
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
                     core. It is NOT the state ROOT (that is VIKE_STATE_ROOT, a different \
                     variable and a different directory). Delete the key",
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
        Ok(())
    }
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
/// This is the ONE place the rule is spelled. `check_addr` (file layer), `check_env_addr` (env
/// layer) and `apply_cli`'s CLI-layer check (below) each wrap this in their own [`ConfigError`]
/// shape — a different variant with different fields per layer, which is why they cannot simply
/// call one another — but the boolean itself lives here exactly once, so a validation gap (a
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

/// The env-layer twin of [`check_addr`] — same rule (see [`is_host_port`]), but the message
/// points at the shell.
fn check_env_addr(var: &str, v: &str) -> Result<(), ConfigError> {
    if is_host_port(v) {
        return Ok(());
    }
    Err(ConfigError::Env {
        var: var.to_string(),
        value: v.to_string(),
        message: "expected a host:port address".to_string(),
    })
}

impl EnvOverride for Config {
    fn apply_env(&mut self, env: &HashMap<String, String>) -> Result<(), ConfigError> {
        if let Some(v) = get(env, STORE_ROOT_ENV) {
            self.store_root = Some(PathBuf::from(v));
        }
        if let Some(v) = get(env, LOG_DIR_ENV) {
            self.log_dir = Some(PathBuf::from(v));
        }
        if let Some(v) = get(env, JOURNAL_DIR_ENV) {
            self.journal_dir = Some(PathBuf::from(v));
        }
        if let Some(v) = get(env, BACKTEST_ADDR_ENV) {
            check_env_addr(BACKTEST_ADDR_ENV, v)?;
            self.backtest_addr = Some(v.to_string());
        }
        if let Some(v) = get(env, DATAHUB_ADDR_ENV) {
            check_env_addr(DATAHUB_ADDR_ENV, v)?;
            self.datahub_addr = Some(v.to_string());
        }
        if let Some(v) = get(env, TRADEHUB_ADDR_ENV) {
            check_env_addr(TRADEHUB_ADDR_ENV, v)?;
            self.tradehub_addr = Some(v.to_string());
        }
        if let Some(v) = get(env, DATAHUB_ADVERTISE_ADDR_ENV) {
            check_env_addr(DATAHUB_ADVERTISE_ADDR_ENV, v)?;
            self.datahub_advertise_addr = Some(v.to_string());
        }
        if let Some(v) = get(env, TRADEHUB_ACCOUNT_ADMIN_ENV) {
            self.tradehub_account_admin = Some(v.to_string());
        }
        if let Some(v) = get(env, TRADEHUB_ADVERTISE_ADDR_ENV) {
            check_env_addr(TRADEHUB_ADVERTISE_ADDR_ENV, v)?;
            self.tradehub_advertise_addr = Some(v.to_string());
        }
        if let Some(v) = get(env, INSTANCE_ORIGIN_ENV) {
            // The env layer is the PRIMARY way this key gets set — two containers from one image
            // differ by exactly this variable — so a malformed value has to fail here, loudly,
            // rather than fall back to "no origin". An instance silently running untagged is
            // indistinguishable from one that was never configured, which is the whole failure the
            // tag exists to end.
            self.instance_origin =
                Some(vike_model::InstanceOrigin::parse(v).map_err(|e| ConfigError::Env {
                    var: INSTANCE_ORIGIN_ENV.to_string(),
                    value: v.to_string(),
                    message: e.to_string(),
                })?);
        }
        Ok(())
    }
}

impl CliOverride for Config {
    fn apply_cli(&mut self, cli: &CliOverrides) -> Result<(), ConfigError> {
        if let Some(v) = &cli.store_root {
            self.store_root = Some(PathBuf::from(v));
        }
        if let Some(v) = &cli.log_dir {
            self.log_dir = Some(PathBuf::from(v));
        }
        if let Some(v) = &cli.datahub_addr {
            // Same rule as `check_addr`/`check_env_addr` — see `is_host_port` — wrapped in the
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

#[cfg(test)]
mod tests {
    use super::*;

    fn file() -> &'static Path {
        Path::new("config.toml")
    }

    #[test]
    fn env_overrides_the_file_layer() {
        let mut c = Config::default();
        c.apply(
            ConfigPatch { store_root: Some("/from/file".into()), ..Default::default() },
            file(),
        )
        .unwrap();
        assert_eq!(c.store_root, Some(PathBuf::from("/from/file")));

        let env = HashMap::from([(STORE_ROOT_ENV.to_string(), "/from/env".to_string())]);
        c.apply_env(&env).unwrap();
        assert_eq!(c.store_root, Some(PathBuf::from("/from/env")));
    }

    /// The FILE half of the `state_dir` deletion. The ENV half is `crate::removed`'s
    /// `the_unread_state_dir_is_refused_and_offers_no_replacement_line`; both must bite, because a
    /// deployment can carry either spelling and neither configured anything.
    #[test]
    fn the_removed_state_dir_key_is_refused_and_does_not_point_at_the_state_root() {
        let err = Config::default()
            .apply(
                ConfigPatch { state_dir: Some("/srv/vike/state".into()), ..Default::default() },
                file(),
            )
            .unwrap_err()
            .to_string();
        assert!(err.starts_with("config.toml: state_dir = "), "{err}");
        assert!(err.contains("NOTHING read it"), "{err}");
        assert!(err.contains("VIKE_STATE_ROOT"), "{err} must warn off the look-alike variable");
    }

    #[test]
    fn cli_outranks_env() {
        let mut c = Config::default();
        c.apply_env(&HashMap::from([(STORE_ROOT_ENV.to_string(), "/from/env".to_string())]))
            .unwrap();
        c.apply_cli(&CliOverrides { store_root: Some("/from/cli".into()), ..Default::default() })
            .unwrap();
        assert_eq!(c.store_root, Some(PathBuf::from("/from/cli")));
    }

    #[test]
    fn a_malformed_address_is_rejected_naming_the_variable() {
        let env = HashMap::from([(DATAHUB_ADDR_ENV.to_string(), "localhost".to_string())]);
        let err = Config::default().apply_env(&env).unwrap_err();
        assert!(err.to_string().starts_with("VIKE_DATAHUB_ADDR=localhost: "), "{err}");
    }

    #[test]
    fn an_unset_variable_leaves_the_file_value_alone() {
        let mut c = Config::default();
        c.apply(ConfigPatch { log_dir: Some("/logs".into()), ..Default::default() }, file())
            .unwrap();
        c.apply_env(&HashMap::new()).unwrap();
        assert_eq!(c.log_dir, Some(PathBuf::from("/logs")));
        assert_eq!(c.datahub_addr, None, "unset stays unset — presence is the Studio's branch");
    }

    /// `check_addr` is the FILE-layer twin of `check_env_addr`
    /// (`a_malformed_address_is_rejected_naming_the_variable` above only exercises the ENV
    /// layer) — nothing else calls it, so it needs its own direct coverage through `Config::apply`.
    /// A value with no `:` must be rejected, naming the key.
    #[test]
    fn a_malformed_file_address_is_rejected() {
        let err = Config::default()
            .apply(
                ConfigPatch { datahub_addr: Some("localhost".to_string()), ..Default::default() },
                file(),
            )
            .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("datahub_addr"), "{msg}");
        assert!(msg.contains("localhost"), "{msg}");
    }

    /// The REQ-2 advertisement key rides `tradehub_addr`'s exact wiring: file layer with the
    /// shared `check_addr` rule, env layer (`VIKE_DATAHUB_ADVERTISE_ADDR`) overriding it, and a
    /// malformed value rejected at either layer naming the key/variable.
    #[test]
    fn datahub_advertise_addr_takes_the_file_then_env_chain() {
        let mut c = Config::default();
        c.apply(
            ConfigPatch {
                datahub_advertise_addr: Some("127.0.0.1:7878".to_string()),
                ..Default::default()
            },
            file(),
        )
        .unwrap();
        assert_eq!(c.datahub_advertise_addr, Some("127.0.0.1:7878".to_string()));

        let env =
            HashMap::from([(DATAHUB_ADVERTISE_ADDR_ENV.to_string(), "tunnel:9999".to_string())]);
        c.apply_env(&env).unwrap();
        assert_eq!(c.datahub_advertise_addr, Some("tunnel:9999".to_string()));

        let bad = HashMap::from([(DATAHUB_ADVERTISE_ADDR_ENV.to_string(), "noport".to_string())]);
        let err = Config::default().apply_env(&bad).unwrap_err();
        assert!(err.to_string().starts_with("VIKE_DATAHUB_ADVERTISE_ADDR=noport: "), "{err}");

        let err = Config::default()
            .apply(
                ConfigPatch {
                    datahub_advertise_addr: Some("noport".to_string()),
                    ..Default::default()
                },
                file(),
            )
            .unwrap_err();
        assert!(err.to_string().contains("datahub_advertise_addr"), "{err}");
        assert_eq!(
            Config::default().datahub_advertise_addr,
            None,
            "unset advertises nothing — the pre-REQ-2 Welcome"
        );
    }

    /// The daemon's SELF-REPORT override rides the same wiring one key over.
    ///
    /// ⚠ **`None` here does NOT mean "report nothing"**, which is the one way this key differs
    /// from every other address in this struct and the reason the last assertion is spelled out:
    /// unset is the ORDINARY state, in which the daemon discovers its own address from the routing
    /// table (`crates/vike-tradehub/src/self_address.rs`). An operator has to configure nothing to
    /// see a real address; this key is the override for the cases a route lookup cannot answer —
    /// NAT above all.
    #[test]
    fn tradehub_advertise_addr_takes_the_file_then_env_chain() {
        // RFC 5737 documentation addresses: a real box's address must never reach a tracked file.
        let mut c = Config::default();
        c.apply(
            ConfigPatch {
                tradehub_advertise_addr: Some("203.0.113.7:7879".to_string()),
                ..Default::default()
            },
            file(),
        )
        .unwrap();
        assert_eq!(c.tradehub_advertise_addr, Some("203.0.113.7:7879".to_string()));

        let env =
            HashMap::from([(TRADEHUB_ADVERTISE_ADDR_ENV.to_string(), "198.51.100.4:7879".into())]);
        c.apply_env(&env).unwrap();
        assert_eq!(c.tradehub_advertise_addr, Some("198.51.100.4:7879".to_string()));

        let bad = HashMap::from([(TRADEHUB_ADVERTISE_ADDR_ENV.to_string(), "noport".to_string())]);
        let err = Config::default().apply_env(&bad).unwrap_err();
        assert!(err.to_string().starts_with("VIKE_TRADEHUB_ADVERTISE_ADDR=noport: "), "{err}");

        let err = Config::default()
            .apply(
                ConfigPatch {
                    tradehub_advertise_addr: Some("noport".to_string()),
                    ..Default::default()
                },
                file(),
            )
            .unwrap_err();
        assert!(err.to_string().contains("tradehub_advertise_addr"), "{err}");
        assert_eq!(
            Config::default().tradehub_advertise_addr,
            None,
            "unset is the ORDINARY state: the daemon discovers its own address and reports that"
        );
    }

    /// `instance_origin` rides the same file-then-env chain as its neighbours, and a malformed
    /// value must be REFUSED at whichever layer supplied it — naming the key on the file layer and
    /// the variable on the env layer.
    ///
    /// The refusal is the half that matters. The env layer is the primary one here (two containers
    /// from one image differ by exactly this variable), and an instance silently running UNTAGGED
    /// is indistinguishable from one that was never configured — which is the whole failure the
    /// tag exists to end. So a typo must stop the process, never degrade to "no origin".
    #[test]
    fn instance_origin_takes_the_file_then_env_chain_and_refuses_a_bad_tag() {
        let mut c = Config::default();
        assert_eq!(c.instance_origin, None, "unset is the default — today's untagged ids");
        c.apply(
            ConfigPatch { instance_origin: Some("West".to_string()), ..Default::default() },
            file(),
        )
        .unwrap();
        // Normalised by `InstanceOrigin::parse`, which is the ONE authority for the rule.
        assert_eq!(c.instance_origin.as_ref().map(|o| o.as_str()), Some("west"));

        let env = HashMap::from([(INSTANCE_ORIGIN_ENV.to_string(), "east".to_string())]);
        c.apply_env(&env).unwrap();
        assert_eq!(c.instance_origin.as_ref().map(|o| o.as_str()), Some("east"));

        // ENV layer: refused, naming the variable AND the value the operator wrote.
        let bad = HashMap::from([(INSTANCE_ORIGIN_ENV.to_string(), "toolong".to_string())]);
        let err = Config::default().apply_env(&bad).unwrap_err();
        assert!(err.to_string().starts_with("VIKE_INSTANCE_ORIGIN=toolong: "), "{err}");

        // FILE layer: refused, naming the key.
        let err = Config::default()
            .apply(
                ConfigPatch { instance_origin: Some("a-b".to_string()), ..Default::default() },
                file(),
            )
            .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("instance_origin"), "{msg}");
        assert!(msg.contains("letters and digits"), "the reason must reach the operator: {msg}");

        // An EMPTY variable configures nothing rather than failing — the same `get` filter every
        // other key here takes, so an `Environment=VIKE_INSTANCE_ORIGIN=` line in a unit leaves
        // the file layer's answer standing.
        let mut kept = Config::default();
        kept.apply(
            ConfigPatch { instance_origin: Some("p2".to_string()), ..Default::default() },
            file(),
        )
        .unwrap();
        kept.apply_env(&HashMap::from([(INSTANCE_ORIGIN_ENV.to_string(), String::new())])).unwrap();
        assert_eq!(kept.instance_origin.as_ref().map(|o| o.as_str()), Some("p2"));
    }

    /// A blank value is rejected too — "must at least be non-blank" is a separate half of the
    /// rule from "must contain a `:`", and both halves need to actually fire.
    #[test]
    fn a_blank_file_address_is_rejected() {
        let err = Config::default()
            .apply(
                ConfigPatch { tradehub_addr: Some("   ".to_string()), ..Default::default() },
                file(),
            )
            .unwrap_err();
        assert!(err.to_string().contains("tradehub_addr"), "{err}");
    }

    /// A well-formed `host:port` value passes through unchanged — `check_addr` must not reject
    /// what it is supposed to accept.
    #[test]
    fn a_well_formed_file_address_is_accepted() {
        let mut c = Config::default();
        c.apply(
            ConfigPatch {
                datahub_addr: Some("127.0.0.1:9000".to_string()),
                tradehub_addr: Some("0.0.0.0:9100".to_string()),
                ..Default::default()
            },
            file(),
        )
        .unwrap();
        assert_eq!(c.datahub_addr, Some("127.0.0.1:9000".to_string()));
        assert_eq!(c.tradehub_addr, Some("0.0.0.0:9100".to_string()));
    }

    /// The predicate every layer's check wraps — pinned directly so a mutant flipping `!`,
    /// dropping the blank half, or dropping the colon half is caught here even before it reaches
    /// any one layer's error shape.
    #[test]
    fn is_host_port_accepts_only_non_blank_colon_bearing_values() {
        assert!(is_host_port("127.0.0.1:9000"));
        assert!(is_host_port("localhost:80"));
        // A colon is non-whitespace, so padding around it still counts as non-blank content.
        assert!(is_host_port("   :   "));
        assert!(!is_host_port("localhost"));
        assert!(!is_host_port(""));
        assert!(!is_host_port("   "));
    }

    /// `apply_cli`'s inline `datahub_addr` check used to duplicate `check_addr`'s rule rather
    /// than share it, and carried no direct test — two mutants survived there (the whole check
    /// replaced with `Ok(())`, and the `!` deleted) because nothing ever drove a bad CLI address
    /// through `apply_cli`. Now it shares `is_host_port` with the file/env layers, and this test
    /// exercises it through the real entry point: a malformed value is rejected, naming the flag.
    #[test]
    fn a_malformed_cli_address_is_rejected() {
        let err = Config::default()
            .apply_cli(&CliOverrides {
                datahub_addr: Some("localhost".to_string()),
                ..Default::default()
            })
            .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("addr"), "{msg}");
        assert!(msg.contains("localhost"), "{msg}");
    }

    /// The blank half of the rule, through the CLI layer.
    #[test]
    fn a_blank_cli_address_is_rejected() {
        let err = Config::default()
            .apply_cli(&CliOverrides {
                datahub_addr: Some("   ".to_string()),
                ..Default::default()
            })
            .unwrap_err();
        assert!(err.to_string().contains("addr"), "{err}");
    }

    /// The COMPUTE daemon's key goes through the SAME file-layer check as its siblings — it is a
    /// `host:port` like the rest, and a key that skipped `check_addr` would accept a value the
    /// dialler then fails on with a worse message.
    #[test]
    fn the_backtest_address_is_checked_like_every_other_address() {
        let mut c = Config::default();
        assert_eq!(c.backtest_addr, None, "unset stays unset — the default is the constant");
        c.apply(
            ConfigPatch { backtest_addr: Some("<host>:7880".to_string()), ..Default::default() },
            file(),
        )
        .unwrap();
        assert_eq!(c.backtest_addr, Some("<host>:7880".to_string()));

        let err = Config::default()
            .apply(
                ConfigPatch { backtest_addr: Some("localhost".to_string()), ..Default::default() },
                file(),
            )
            .unwrap_err();
        assert!(err.to_string().contains("backtest_addr"), "the key is named: {err}");
    }

    /// ⚠ The DEFAULT, pinned as a SEPARATION rather than as a number on its own. `7879` is the
    /// live order-signing daemon's port and `7878` is the data server's; a compute default equal
    /// to either would aim every client of that plane at a process that does not serve it, and one
    /// of those two signs orders. The first writing of ruling 7 picked `7879`, which is exactly the
    /// mistake this test exists to make unrepeatable.
    #[test]
    fn the_compute_default_is_neither_of_its_neighbours() {
        assert_eq!(DEFAULT_BACKTEST_ADDR, "127.0.0.1:7880");
        assert_ne!(DEFAULT_BACKTEST_ADDR, "127.0.0.1:7879", "that is the LIVE trading daemon");
        assert_ne!(DEFAULT_BACKTEST_ADDR, DEFAULT_DATAHUB_ADDR, "that is the DATA server");
        // …and it is a value the file layer would accept, so the three rungs cannot disagree about
        // what an address is.
        assert!(is_host_port(DEFAULT_BACKTEST_ADDR));
    }

    /// A well-formed value passes through `apply_cli` unchanged.
    #[test]
    fn a_well_formed_cli_address_is_accepted() {
        let mut c = Config::default();
        c.apply_cli(&CliOverrides {
            datahub_addr: Some("127.0.0.1:9000".to_string()),
            ..Default::default()
        })
        .unwrap();
        assert_eq!(c.datahub_addr, Some("127.0.0.1:9000".to_string()));
    }
}
