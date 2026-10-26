//! Dukascopy execution — drives the JForex Java sidecar over JSON-lines stdio.
//!
//! Dukascopy's only trading API is JForex (Java), so unlike every HMAC-REST venue this
//! client owns a CHILD PROCESS (`java -jar jforex-bridge.jar`) instead of a socket or
//! FFI handle: commands go to child stdin, canonical [`Event`] envelopes come back on
//! stdout and are pumped into the core ingest (the same inbound shape as the WS
//! user-data pumps). See docs/superpowers/specs/2026-07-04-dukascopy-jforex-bridge-design.md.
//!
//! LIVE GATE: jar missing, `java` missing, login failure, or handshake timeout →
//! [`DukascopyError::Unavailable`] → the caller stays paper. A child that dies later is
//! RESTARTED by the client itself (the next section); until it is back, every order is
//! rejected ("bridge unavailable: ...") and the reader thread's EOF drain rejects every
//! already-in-flight order ("bridge died: ...") — either way every order still reaches a
//! terminal state.
//!
//! # ⚠ The sidecar is SUPERVISED: a dead sidecar comes back by itself, and says so while it is away
//!
//! A sidecar that dies after `ready` (JForex ends its session at a server shutdown, a network drop it cannot
//! reconnect, an OOM) used to be gone for good: the reader hit EOF, every later submit was rejected, and nothing
//! started another one, while the node still counted the account as running (measured 2026-10-26: the sidecar
//! died at 08:15:03 UTC on `JForex session disconnected` and was not started again for 2.5 hours, until a deploy
//! restarted the daemon). Now a supervisor thread, one per client, starts the next sidecar:
//!
//! * **Backoff**: the first restart waits `RESTART.initial`, each failed one doubles the wait up to `RESTART.max`
//!   (the exec lane's `reconnect` row of `vike_venues::venue_timings::DUKASCOPY`, which carries the reason), and it
//!   never gives up while the client lives: no tight loop, no hand step. A restarted sidecar that dies again before it has
//!   lived `RESTART.max` is a FLAP and counts as one more failed restart, so a venue that keeps cutting the session
//!   off is backed off like a refused login (only a sidecar that lived that long starts the waits over). A start that says nothing is cut at
//!   [`ATTEMPT_READY_TIMEOUT`] and a refused one (a `fatal`, a failed spawn, a silent death) counts as one failed restart;
//!   every one is logged WITH the sidecar's own reason.
//! * **One JForex session per login, always**: the next sidecar starts only after the dead one has been killed
//!   and REAPED, and a client that is detached or dropped (the account deactivated, the tier changed, a clean stop,
//!   an orphan reaped) ends the supervisor and kills a sidecar that was still logging in, so no second session ever
//!   shares the login and the loop outlives nothing. That is also why the loop reads no account row: the account
//!   being deactivated ends the CLIENT (`crates/vike-tradehub-core/src/node/hot.rs`'s `detach_account` closes its
//!   session, and the gatewayhub tears the adapter down), and the client ends the loop.
//! * **While it is away it is not healthy, and it says so by name**: [`DukascopyExecutionClient::sidecar_state`] is
//!   [`SidecarState::Down`], the death is one `error` line (`the JForex sidecar is DOWN`), a submit is rejected
//!   `bridge unavailable: the JForex sidecar is down (...) ... this order was NOT sent`, a cancel is an
//!   `OrderCancelRejected` and an amend an `OrderModifyRejected` with the same words, and the price lane's bus has
//!   no sidecar attached (it replays its wants to the next one). The state itself lives on that bus
//!   (`QuoteBus::sidecar`, written through `Shared::set_state`), the one place the gatewayhub's trade lane can read it
//!   from, so the lease's status says the path is down and `trade doctor` stops reading `RUNNING` as healthy.
//! * **What a new sidecar does NOT know**: it starts with no memory of the orders the dead one placed (its label,
//!   fill-sequence and commission maps are in-process, `StrategyBridge.java`), so it can neither cancel, amend nor
//!   report a fill of an order that was resting across the death. A cancel or amend of such an order is refused by
//!   name here (`stranded`) instead of being skipped in silence by Java, and its fill is NOT reported (a position
//!   built that way shows only at the next fill's `position` line, as a loud `SizeMismatch`). Closing a position
//!   needs none of that memory: a market order nets against what the VENUE holds (`engine.getOrders`), and a closing
//!   fill's id (`<order id>:c:<client order id>`, `OrderMap.closeTradeId`) is restart-safe by construction.
//! * **The netting shadow survives the restart** ([`ShadowBook`] is shared by every reader of the client): a shadow
//!   born empty at each sidecar would adopt the venue's position as its baseline and re-anchor the core's blend
//!   against a basis the core never folded.
//! * A start that FAILS (the first one, before any sidecar lived) is not supervised: it stays the live gate above.
//!
//! # The HALT sentinel, on a client that inherits nothing
//!
//! Every venue whose `submit` delegates to `vike_bridge_core::exec_actor::ExecActor` gets the
//! operator kill switch for free. This one delegates to nothing — it owns a child process — so it
//! reads the sentinel itself, in [`DukascopyExecutionClient::submit`], through the SAME two shared
//! functions every other enforcing client calls (`halt_admits_submit` for the verdict,
//! `HALT_REJECT_REASON` for the wording) so it cannot drift into its own idea of what a halt lets
//! out. `docs/ops/kill-switches.md` carried its absence for months, correctly labelled LATENT:
//! nothing in production started this venue, so the hazard was never a live order leaking past an
//! engaged switch — it was that somebody would wire the venue and inherit the silence with it.
//! Decision 0168 wired it, and `crate::adapter`'s `start` hands every client its halt path
//! (`with_halt_path`), so the switch is in force the day the venue is.
//!
//! # ⚠ The DUAL-PUBLISH fill contract (break this and position/PnL silently desync)
//!
//! A fill arrives on the wire as TWO envelopes carrying the SAME fill, and both must reach the
//! core, in order: the BARE `Event::Fill` first — which the core `Account` folds into position and
//! realized PnL independently of any order — and then the wrapping `Event::OrderFilled` the order
//! FSM applies. Dropping either one leaves half the system right and the other half permanently
//! wrong, and neither half complains.
//!
//! The netting shadow (`ShadowBook::fold_fill`, called from the reader thread) folds the **bare
//! lane ONLY**, for exactly the reason the `Account` does: the wrap carries the same fill, so
//! folding both double-counts. If you add a fill-consuming side effect to the reader loop, decide
//! which lane it belongs on before you write it — "both" is never the answer.
//!
//! # The price lane rides this same child
//!
//! The sidecar is ONE process per daemon, so live quotes for an instrument the engine was not mounted on
//! (any-symbol) cannot come from a second login: `crate::quotes`' bus is attached to this client's stdin and
//! stdout by its reader thread (on `ready`, the `tick` envelopes, detach at EOF), and the data lane's stream asks for
//! symbols through it. This client sends no price command itself and a client nobody listens to ([`DukascopyExecutionClient::spawn`])
//! behaves exactly as before. Writes to the child's stdin from two places go through one mutex, a whole line at a
//! time.
//!
//! # The `ready` balance is the engine's balance, once per start
//!
//! The venue has no reconcile leg, so the sidecar's `ready` line is the only place the engine can learn the
//! account's balance. The reader's first-`ready` arm hands it to `crate::ready_balance`, which builds ONE
//! `AccountState` wallet frame for a USD account on a USD-quoted mount (the start tells the client its mount in
//! `DukascopyConfig::ledger_mount`) and sends it after the handshake reply. ⚠ The figure is `IAccount.getBalance()`,
//! the vendor's PREVIOUS END-OF-DAY balance: fresh only at start, so realized PnL of the engine's own trades stays
//! out of its equity until the next start. That module's doc carries the gate, the residual and what the operator
//! sees when the frame is withheld; the gate `crates/vike-ops/tests/venues/wallet_frame_venues_gate.rs` holds the
//! "one frame per start" claim to this reader's bytes.
//!
//! # ⚠ The server setting is usually NOT a JNLP URL
//!
//! `venue.dukascopy.demo.server` (the `DUKASCOPY_DEMO{1,2}_SERVER` credential rows until decision
//! 0095's Task 7) commonly holds the WEB-PLATFORM login URL. The sidecar would hand it to
//! `IClient.connect` as a JNLP and HANG there — no error, no timeout distinguishable from a slow
//! login. [`DukascopyExecutionClient::spawn_with_program`] self-defends: a `server` value that does
//! not end in `.jnlp` is warned about and replaced by the built-in demo JNLP. So a mis-set value
//! costs a warning line, not a stall — do not "fix" that branch by trusting the value.
//!
//! # ⚠ The sidecar's HOME is the project's, not the operating-system user's
//!
//! The bundled JForex SDK writes its working directory (`.cache`, `Strategies`) under
//! `System.getProperty("user.home")` and `Bridge.java` overrides nothing, so an un-steered sidecar
//! puts the platform cache in `$HOME/JForex` — MEASURED on the production box, outside the project
//! folder entirely. `deploy/vike-tradehub.service` runs with `ProtectHome=yes`, which makes `/home`
//! INACCESSIBLE in that namespace rather than merely read-only, so the first LIVE Dukascopy mount
//! under the daemon could not create it; in a container `$HOME` may be `/` or unset and the
//! writable layer is gone on the next `docker run`, so the >60s first platform fetch this file's
//! [`READY_TIMEOUT`] is sized for would be paid on every start.
//!
//! [`jvm_args`] therefore prepends `-Duser.home=<project>/settings/state/jforex`, which that unit's
//! `ReadWritePaths=` already grants — the fix widens no sandbox and changes no unit. What it is NOT
//! is a per-sidecar cache: one process gets one home, so the one-sidecar-per-process refusal
//! (`crate::adapter`'s declaration, enforced by `vike-tradehub-core`) stands exactly as it did.
//!
//! # The three runtime facts are RESOLVED BY THE CALLER — this module decides nothing
//!
//! [`DukascopyExecutionClient::spawn`] takes a [`DukascopyTools`], which the composition root
//! produces with `vike_dukascopy::resolve_dukascopy_tools` from its own environment sweep,
//! `vike_paths::state_path::project_bin_dir_from` and
//! `vike_paths::state_path::project_state_dir_from`. Nothing here reads the environment, walks for
//! a project, or knows where a jar lives.
//!
//! ⚠ **It used to, and that is worth keeping written down**: the deleted `bridge_jar` and
//! `java_program` built their fallbacks from `concat!(env!("CARGO_MANIFEST_DIR"), …)` — the tree
//! this crate was COMPILED in, not the project the binary is later run from. A binary built
//! elsewhere and deployed into a `<project>` folder found neither jar nor JVM and degraded to
//! paper, with nothing in the log but "bridge jar not found" — the same defect class the
//! credential-store ratchet spent a release retiring for settings. The escape hatches were
//! asymmetric too: `JFOREX_BRIDGE_JAR` and `JAVA_HOME` named the two artifacts outright, but the
//! `vendor/tools/` sweep beneath them had **no override at all**, so a deployment had to set both
//! explicitly or get nothing. Both variables were `Layer::Library` rows in
//! `crates/vike-config/src/settings.rs`; they are `Layer::Injected` rows now, read from the map the
//! caller supplies, and `crates/vike-ops/tests/settings_secrets/settings_registry.rs`'s `LIBRARY_PIN` ratchet shrank
//! by two. The ladders themselves live with the parser, on
//! `crates/bridges/dukascopy/src/config.rs`'s `resolve_dukascopy_tools`.

use std::collections::{HashMap, HashSet};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc as std_mpsc;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use vike_exec::{EventSender, ExecutionClient};
use vike_model::OrderRequest;
use vike_model::events::{
    Event, OrderCancelRejected, OrderModifyRejected, OrderRejected, OrderSubmitted,
};
use vike_venues::venue_timings::{self, Reconnect};

use super::config::{DukascopyConfig, DukascopyTools};
use super::netting::{Reanchor, ShadowBook};
use super::proto::{Command, Envelope, encode_line, parse_envelope};
use super::quotes::{LinePort, QuoteBus};
use super::ready_balance::wallet_frame;
use super::recon_client::{DukascopyReconClient, PositionSnapshot, VenuePosition};

/// Shared tracing target for this module's diagnostics.
const TARGET: &str = "vike_dukascopy::exec";

/// The READY line's feature word for the `modify` command (`Proto.java`'s `ready`). A jar that does not announce it
/// cannot amend.
const FEATURE_MODIFY: &str = "modify";

/// The child's stdin, shared by the exec client (its submits, cancels, amends and the shutdown) and the price lane
/// (`crate::quotes`' subscribe commands). One mutex, one whole line per write, so two writers never interleave a line.
/// `None` is the dead-bridge marker: a failed write or the shutdown leaves it, and every later write is refused.
type Stdin = Mutex<Option<ChildStdin>>;

impl LinePort for Stdin {
    fn send(&self, line: &str) -> bool {
        let mut slot = self.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(stdin) = slot.as_mut() else { return false };
        let ok = writeln!(stdin, "{line}").and_then(|_| stdin.flush()).is_ok();
        if !ok {
            *slot = None;
        }
        ok
    }
}

/// Verified live demo JNLP (spec: Build & gating). `DukascopyConfig.server` overrides.
const DEFAULT_DEMO_JNLP: &str = "https://www.dukascopy.com/client/demo/jclient/jforex.jnlp";
/// JForex JNLP login is slow — first login downloads platform config (observed > 60s);
/// the sidecar polls the session for up to 240s, so bound the handshake just above that. Since the
/// stall cure it is the TOTAL cap over every start of one `spawn` ([`ATTEMPT_READY_TIMEOUT`] bounds
/// each), and with 3 x 25 s the cap is never the binding one.
///
/// The VALUE is the exec lane's `handshake_timeout` in
/// `vike_venues::venue_timings::DUKASCOPY`, which carries the reason; the NAME stays because
/// what this bounds is the sidecar becoming READY (the wait for its `Ready`/`Fatal` envelope
/// before the child is killed), and the Java sidecar's and the start's docs cite it by that name.
const READY_TIMEOUT: Duration = venue_timings::DUKASCOPY.exec().handshake_timeout();
/// How long ONE sidecar start may stay silent before it is killed and started again.
///
/// MEASURED (2026-10-17, the v0.1.71 deploy): 29 logins since 10-16 reached `bridge ready` 2-3 s
/// after the SDK's `Authorize task in queue` line; the one that hung there never completed, held the
/// tradehub's boot for 70+ s, and a fresh sidecar 8 s later was ready in 5 s. 25 s is 8x the normal
/// login. A compiled bound like `SHUTDOWN_GRACE` (decisions 0086, 0111), not a settings row.
///
/// ⚠ A FRESH platform cache's first login is documented to take over 60 s
/// (`crates/bridges/dukascopy/CLAUDE.md`'s "Login-failure triage"); on such a box every attempt is
/// cut at 25 s and the start gives up after ~75 s where it used to wait for the sidecar's own 240 s.
const ATTEMPT_READY_TIMEOUT: Duration = Duration::from_secs(25);
/// Sidecar starts per `spawn`: the first plus two respawns after a silent one.
const MAX_SPAWN_ATTEMPTS: u32 = 3;
/// Grace between `shutdown` and kill.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(5);
/// The supervisor's wait before it starts another sidecar after one ended, and how that wait grows: the exec lane's
/// `reconnect` of the `DUKASCOPY` row (5 s doubling to 60 s, decision 0160: the row carries the reason, never retune
/// this constant). A `const` read, so a row that declares none is a COMPILE error here.
const RESTART: Reconnect = venue_timings::DUKASCOPY.exec().reconnect();
/// The opening words of every refusal while no sidecar answers: a submit's `OrderRejected`, a cancel's
/// `OrderCancelRejected`, an amend's `OrderModifyRejected`. It was the whole reason before the supervisor existed.
const BRIDGE_UNAVAILABLE: &str = "bridge unavailable";
/// The `OrderRejected` reason of an order that was in flight when its sidecar died. It opens with the words it always had.
const BRIDGE_DIED: &str = "bridge died: the JForex sidecar exited while this order was in flight, so the venue's \
     state of it is unknown (look at the JForex platform)";
/// How often a sidecar start that is waiting for `ready` looks up to see whether its client was stopped meanwhile,
/// so that a detach never waits for a login.
const HANDSHAKE_POLL: Duration = Duration::from_millis(100);
/// The most orders one client remembers having stranded across sidecar deaths (see `Shared::stranded`). Past it the
/// extra ones are not remembered, which only costs the named refusal of their cancel (Java then skips it in silence).
/// shortcut: a flat cap, a client lives hours between deploys; upgrade if a venue ever rests thousands of orders.
const MAX_STRANDED: usize = 4096;

/// The JVM system-property flag that relocates the sidecar's home directory: `-Duser.home=`.
///
/// ⚠ **A JVM OPTION, so it must precede `-jar`** — see [`jvm_args`], which is the only place it is
/// ever spelled.
const USER_HOME_FLAG: &str = "-Duser.home=";

/// The sidecar JVM's explicit MAXIMUM HEAP: `-Xmx512m` (512 MiB). Without it the JVM's default is a
/// quarter of the memory it believes it has, so the heap was whatever the host's cgroup happened to
/// say, not a number this crate chose.
///
/// ⚠ **The value is the default the one daemon that can launch the sidecar today already got, so
/// the flag changes nothing there.** That launcher is the trading daemon's execution path (the
/// gateway daemon starts no JVM until the trade lane moves into it, and production wires no
/// Dukascopy engine yet: `crate::adapter`'s module doc). The JRE is Temurin 17 (container-aware:
/// `MaxRAMPercentage` 25), and the trading daemon's unit has `MemoryMax=2G`
/// (`deploy/vike-tradehub.service`), so its default heap is 2 GiB / 4 = 512 MiB exactly. Under
/// `deploy/vike-gatewayhub.service` (`MemoryMax=8G`) the default would be 8 GiB / 4 = 2 GiB, and on
/// a host with no cgroup limit a quarter of the machine; this pins both to the 512 MiB the sidecar
/// is sized for. A reading of the JDK's ergonomics, not a measurement: a real JVM under a unit's
/// cgroup was not run (`crates/bridges/dukascopy/CLAUDE.md`'s "The JVM's heap is capped").
///
/// A COMPILED bound, not a settings row: it is a safety bound sized against a unit's `MemoryMax`
/// (the unit comment carries the sum), the way `SHUTDOWN_GRACE` and the venue-timing table
/// (decision 0160) are compiled; no process environment may carry it (decision 0095).
///
/// ⚠ **A JVM OPTION, so it must precede `-jar`** — see [`jvm_args`], which is the only place it is
/// ever spelled.
const MAX_HEAP_FLAG: &str = "-Xmx512m";

/// Errors from the Dukascopy venue layer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DukascopyError {
    /// No jar / no java / login failed / handshake timed out — stay paper.
    Unavailable,
}

impl std::fmt::Display for DukascopyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DukascopyError::Unavailable => {
                write!(f, "Dukascopy JForex bridge unavailable (jar/java missing or login failed)")
            }
        }
    }
}

impl std::error::Error for DukascopyError {}

/// Why one sidecar start produced no sidecar (see `DukascopyExecutionClient::spawn_with_attempts`).
enum AttemptFailed {
    /// No `ready` and no `fatal` within the wait: the one failure worth a fresh sidecar at once.
    NoReady,
    /// A verdict (spawn error, `fatal`, silent death), in the sidecar's own words: starting again at once repeats it.
    Refused(String),
}

/// Where the client's JForex sidecar is, as `submit`, `cancel` and `modify` read it and as a test or a status
/// reader can ask ([`DukascopyExecutionClient::sidecar_state`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SidecarState {
    /// A sidecar said `ready` and has not ended.
    Up,
    /// No sidecar runs: the last one ended and the client's supervisor is starting another.
    Down {
        /// What the sidecar said when it ended (its `fatal` reason), or the last failed restart's reason.
        why: String,
        /// Restarts that were refused or said nothing in time, plus each restarted sidecar that died again before it
        /// had lived `RESTART.max` (the count only starts over after a sidecar that lived that long).
        failed_restarts: u32,
        /// The wait before the next restart.
        retry_in: Duration,
    },
}

impl SidecarState {
    /// The words a refusal carries while no sidecar runs; `None` while one does.
    fn down_words(&self) -> Option<String> {
        match self {
            SidecarState::Up => None,
            SidecarState::Down { why, failed_restarts, retry_in } => Some(format!(
                "the JForex sidecar is down ({why}); the node restarts it by itself \
                 ({failed_restarts} restart(s) failed so far, next try in {retry_in:?})"
            )),
        }
    }
}

/// `m`, locked, whether or not a panicking holder poisoned it: every guarded value here is a plain value a half
/// finished writer cannot leave torn.
fn locked<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// One sidecar process the client owns: the child and the thread reading its stdout.
struct Session {
    child: Child,
    reader: std::thread::JoinHandle<()>,
}

impl Session {
    /// Kill the child, reap it and join its reader. Idempotent on a child that already exited.
    fn kill_and_reap(self) {
        let Session { mut child, reader } = self;
        let _ = child.kill();
        let _ = child.wait();
        let _ = reader.join();
    }
}

/// A sidecar that reached `ready`, as `start_sidecar` hands it to its caller.
struct Started {
    session: Session,
    /// `IAccount.getAccountId()` from the `ready` line.
    account: String,
    /// Whether the `ready` line announced the `modify` feature.
    amend_supported: bool,
}

/// What the client, the reader of each sidecar and the supervisor share. It OUTLIVES every sidecar: the sidecars come
/// and go under it, and the book-keeping that must not restart with a process lives here.
struct Shared {
    events: EventSender,
    /// The current sidecar's stdin, shared with the price lane (`crate::quotes`' subscribe commands). `None` inside is
    /// the dead-bridge marker: no sidecar runs, a write failed, or the client shut down. Each sidecar's start puts its
    /// pipe here, so the price lane's one `LinePort` keeps working across a restart.
    stdin: Arc<Stdin>,
    inflight: Inflight,
    /// The coids that were in flight (accepted, resting or not yet answered) when a sidecar died and were rejected
    /// with `bridge died`. The sidecar that replaced it has no memory of them (its label and order maps are
    /// in-process, `StrategyBridge.java`), so a cancel or an amend of one is refused by name here.
    stranded: Mutex<HashSet<String>>,
    /// The sidecar's last authoritative `position` line per symbol (`DukascopyReconClient`'s source).
    venue_positions: PositionSnapshot,
    /// The netting-truth shadow of the core's blended fold. It lives as long as the CLIENT, not the sidecar: it mirrors
    /// the core `Account`, which a sidecar restart does not reset.
    shadow: Mutex<ShadowBook>,
    /// The process's price lane. It also holds the client's [`SidecarState`] ([`QuoteBus::sidecar`]), the one place
    /// the gatewayhub's trade lane can read it from: read and write it through [`Self::state`] / [`Self::set_state`].
    quotes: Arc<QuoteBus>,
    /// The last `fatal` reason the reader heard, for the words of the death.
    last_fatal: Mutex<Option<String>>,
    /// The sidecar that runs now (`None` while none does).
    session: Mutex<Option<Session>>,
    /// Whether the CURRENT sidecar's `ready` announced `modify`.
    amend_supported: AtomicBool,
    /// Set by the client's detach or drop, or by a reader whose core is gone: no sidecar is to be started and a
    /// start in progress is to be killed.
    stopping: AtomicBool,
    /// The wait policy between restarts (`RESTART`; a test passes its own).
    restart: Reconnect,
}

impl Shared {
    fn is_stopping(&self) -> bool {
        self.stopping.load(Ordering::SeqCst)
    }

    fn state(&self) -> SidecarState {
        self.quotes.sidecar()
    }

    fn set_state(&self, state: SidecarState) {
        self.quotes.set_sidecar(state);
    }

    /// The reader of a sidecar that had said `ready` reached EOF while the client still runs: say so, ONCE, and
    /// refuse commands by name from here on.
    fn went_down(&self) {
        let why = locked(&self.last_fatal)
            .take()
            .unwrap_or_else(|| "its output ended and the process exited".to_string());
        tracing::error!(
            target: TARGET,
            %why,
            restart_in = ?self.restart.initial,
            "the JForex sidecar is DOWN: every order, cancel and amend is refused by name until the node has \
             restarted it"
        );
        self.set_state(SidecarState::Down {
            why,
            failed_restarts: 0,
            retry_in: self.restart.initial,
        });
    }
}

/// Everything one more sidecar start needs; the supervisor keeps it for the life of the client.
struct Launch {
    program: String,
    args: Vec<String>,
    /// The login the first sidecar used, kept so the next one can log in again. Never logged (`DukascopyConfig`'s
    /// `Debug` omits the password) and never put on argv: it reaches the child through the environment.
    config: DukascopyConfig,
    jnlp: String,
    quotes: Arc<QuoteBus>,
    /// How long ONE start may stay silent before it is killed.
    attempt_timeout: Duration,
}

/// The refusal of a cancel or an amend (`verb`) of an order that was in flight when a sidecar died.
fn stranded_words(verb: &str) -> String {
    format!(
        "dukascopy: the JForex sidecar was restarted after this order was placed and the new one does not \
         track it, so the {verb} was NOT sent; the order may still rest at the venue: {verb} it in the \
         JForex platform"
    )
}

/// The wait before restart number `failed + 1`: `policy.initial` doubled once per failed restart, never above
/// `policy.max`.
fn restart_delay(policy: &Reconnect, failed: u32) -> Duration {
    policy.initial.saturating_mul(1u32 << failed.min(16)).min(policy.max)
}

/// client_order_ids awaiting a terminal event, shared between `submit` (insert before
/// write) and the reader thread (remove on terminal event; drain into synthetic
/// rejects at EOF so a dead bridge never strands an order non-terminal).
type Inflight = Arc<Mutex<HashSet<String>>>;

/// The client_order_id when `event` is TERMINAL for an order (in-flight tracking).
/// Accepted/PartiallyFilled/Triggered are non-terminal — the order is still live.
fn terminal_coid(event: &Event) -> Option<&str> {
    match event {
        Event::OrderRejected(e) => Some(&e.client_order_id),
        Event::OrderCanceled(e) => Some(&e.client_order_id),
        Event::OrderFilled(e) => Some(&e.client_order_id),
        Event::OrderExpired(e) => Some(&e.client_order_id),
        Event::OrderLiquidated(e) => Some(&e.client_order_id),
        // every other event: not this tap's business (audited 2026-10, I-21). ⚠ Not spelled out as
        // a variant list ON PURPOSE: `crates/vike-ops/tests/venues/wallet_frame_venues_gate.rs`
        // derives "which bridge constructs the account-state event" from the TEXT of a bridge's
        // `src/`, and this venue's hand-classified row there allows exactly ONE constructor, in
        // `crate::ready_balance`: spelling that variant out in a pattern here would add a second.
        _ => None,
    }
}

/// Live Dukascopy exec client — owns the sidecar child process, and starts the next one when it dies.
pub struct DukascopyExecutionClient {
    /// The state every sidecar of this client shares (its stdin, the in-flight set, the netting shadow, the
    /// sidecar's [`SidecarState`], the running sidecar's child and reader).
    shared: Arc<Shared>,
    events: EventSender,
    /// The supervisor thread (`supervise`): parked while a sidecar lives, restarting one while none does.
    supervisor: Option<std::thread::JoinHandle<()>>,
    /// Wakes the supervisor out of its park or its backoff wait; `Shared::stopping` is what it then reads.
    wake_supervisor: std_mpsc::Sender<()>,
    /// **The account identifier the FIRST sidecar's READY handshake carried** — `IAccount.getAccountId()`
    /// on the JForex account object this session authenticated as, verbatim.
    ///
    /// ⚠ Kept rather than discarded, which it was until 2026-09-15: `spawn_with_program` matched
    /// `Ok(Ok(_account))` and threw the venue's own answer away at the one moment it is knowable.
    /// It is what `crate::adapter`'s `record_confirmation` records so that `account.venue_account_id`
    /// can say *the venue confirmed it* rather than *somebody told the store* — the
    /// credential-schema spec's §4.5 handshake writer. A restarted sidecar logs in as the same account.
    ///
    /// ⚠ **Not a secret, and not a login either** — it is the id the venue prints on its own pages
    /// and echoes on its own wire, and it is already logged at `info` by the reader thread. The login
    /// and the password this client was spawned with reach the child through ENV; the one copy this
    /// client keeps is the supervisor's, to log in again, and it is never logged.
    handshake_account: String,
    /// The operator HALT sentinel this client watches — handed in by the start from
    /// `AdapterInputs::process.halt_path` ([`Self::with_halt_path`]), never resolved here.
    ///
    /// `None` (a test, a tool) watches NO file: this client used to fall back to the process-wide
    /// resolver (`vike_boot::halt::halt_path_from_env`), which made its kill switch work by
    /// reaching into process state at the moment an order arrived — a bridge reading a
    /// process-global path, which the start contract forbids (decision 0099). A client with no path
    /// says so once at `error` (`vike_exec::halt::sentinel_engaged`), because a start that
    /// forgot to hand it one is a dead kill switch.
    halt_path: Option<PathBuf>,
}

/// The child JVM's argv, built from the CALLER'S already-resolved [`DukascopyTools`].
///
/// A pure function rather than a literal inside [`DukascopyExecutionClient::spawn`] so the argv can
/// be asserted without forking, and so the one ORDERING rule below has a place to be stated.
///
/// ⚠ **`-Duser.home=` goes BEFORE `-jar`, and that is not cosmetic.** The JVM treats everything up
/// to `-jar` as its OWN options and everything after `-jar <path>` as arguments to the application;
/// `java -jar bridge.jar -Duser.home=X` therefore hands the flag to `Bridge.main` — which ignores
/// it — and the JVM keeps `$HOME`. The bug looks exactly like the defect being fixed, so the test
/// that proves the flag lands is one that parses the argv the way a JVM does
/// (`crates/bridges/dukascopy/src/bin/fake_jforex_bridge.rs`'s `jvm_option`), never one that
/// searches the whole vector.
///
/// ⚠ **The flag is ONE argv element and is never quoted by us.** `std::process::Command` passes
/// each element to the OS as a unit — on Unix through `execvp`'s vector, on Windows through the
/// standard library's own command-line escaping — so a `user.home` holding spaces or a drive letter
/// (`C:\Program Files\vike trader\settings\state\jforex`) needs no shell quoting and must not be
/// given any. Joining these into a string and letting a shell split it is the way that breaks.
///
/// `None` emits NO home flag at all, which leaves the JVM's own `user.home` in place —
/// `crates/bridges/dukascopy/src/config.rs`'s `resolve_dukascopy_tools` argues why that rung is a
/// refusal rather than a guess. The heap flag ([`MAX_HEAP_FLAG`]) is unconditional and precedes
/// `-jar` for the same reason the home flag does.
fn jvm_args(tools: &DukascopyTools) -> Vec<String> {
    let mut args = Vec::with_capacity(4);
    if let Some(home) = tools.jforex_home.as_ref() {
        args.push(format!("{USER_HOME_FLAG}{}", home.display()));
    }
    args.push(MAX_HEAP_FLAG.to_string());
    args.push("-jar".to_string());
    args.push(tools.bridge_jar.display().to_string());
    args
}

impl DukascopyExecutionClient {
    /// Spawn the real sidecar with the CALLER'S resolved tool paths, pass creds via child env
    /// (never argv), and wait for the ready/fatal handshake.
    ///
    /// `tools` comes from [`crate::resolve_dukascopy_tools`], called by the composition root that
    /// owns the environment sweep — see this module's doc for why this function resolves nothing
    /// itself. A jar that is not a file is the live gate: warn with the path that was actually
    /// tried (so an operator can see WHICH project was resolved) and stay paper.
    pub fn spawn(
        config: DukascopyConfig,
        tools: &DukascopyTools,
        events: EventSender,
    ) -> Result<Self, DukascopyError> {
        Self::spawn_quoting(config, tools, events, QuoteBus::new())
    }

    /// [`Self::spawn`], with the sidecar's price lane attached to `quotes` (`crate::quotes`): the production start
    /// passes the process's bus (`crate::quotes::process_bus`), so the data lane's stream and this client share one
    /// sidecar. [`Self::spawn`] gives a private bus nobody listens to.
    pub fn spawn_quoting(
        config: DukascopyConfig,
        tools: &DukascopyTools,
        events: EventSender,
        quotes: Arc<QuoteBus>,
    ) -> Result<Self, DukascopyError> {
        if !tools.bridge_jar.is_file() {
            tracing::warn!(
                target: TARGET,
                jar = %tools.bridge_jar.display(),
                java = %tools.java,
                "bridge jar not found — staying paper (install it at <project>/bin/jforex/, or name \
                 it with JFOREX_BRIDGE_JAR)"
            );
            return Err(DukascopyError::Unavailable);
        }
        Self::spawn_with_program_quoting(&tools.java, &jvm_args(tools), &config, events, quotes)
    }

    /// Test seam: spawn an arbitrary program speaking the bridge protocol, with the production
    /// retry policy ([`ATTEMPT_READY_TIMEOUT`] x [`MAX_SPAWN_ATTEMPTS`]).
    pub fn spawn_with_program(
        program: &str,
        args: &[String],
        config: &DukascopyConfig,
        events: EventSender,
    ) -> Result<Self, DukascopyError> {
        Self::spawn_with_program_quoting(program, args, config, events, QuoteBus::new())
    }

    /// [`Self::spawn_with_program`] with the price lane attached to `quotes` ([`Self::spawn_quoting`]).
    pub fn spawn_with_program_quoting(
        program: &str,
        args: &[String],
        config: &DukascopyConfig,
        events: EventSender,
        quotes: Arc<QuoteBus>,
    ) -> Result<Self, DukascopyError> {
        Self::spawn_with_attempts_quoting(
            program,
            args,
            config,
            events,
            quotes,
            ATTEMPT_READY_TIMEOUT,
            MAX_SPAWN_ATTEMPTS,
        )
    }

    /// [`Self::spawn_with_program`] with the retry policy as parameters, so a test can script a hung
    /// first start in milliseconds.
    ///
    /// ⚠ **A sidecar that sends neither `ready` nor `fatal` within `attempt_timeout` is KILLED and
    /// REAPED, then started again, up to `max_attempts` starts in all** (never past
    /// [`READY_TIMEOUT`] in total). Only that silence is retried: a `fatal`, a failed spawn and a
    /// child that dies without a word are verdicts and return at once. The old child is gone
    /// (`Child::wait` returned) before the next one starts, so one login never has two JForex
    /// sessions.
    pub fn spawn_with_attempts(
        program: &str,
        args: &[String],
        config: &DukascopyConfig,
        events: EventSender,
        attempt_timeout: Duration,
        max_attempts: u32,
    ) -> Result<Self, DukascopyError> {
        Self::spawn_with_attempts_quoting(
            program,
            args,
            config,
            events,
            QuoteBus::new(),
            attempt_timeout,
            max_attempts,
        )
    }

    /// [`Self::spawn_with_attempts`] with the price lane attached to `quotes` ([`Self::spawn_quoting`]); every attempt
    /// that reaches `ready` attaches to the same bus, and the reader detaches it at EOF.
    pub fn spawn_with_attempts_quoting(
        program: &str,
        args: &[String],
        config: &DukascopyConfig,
        events: EventSender,
        quotes: Arc<QuoteBus>,
        attempt_timeout: Duration,
        max_attempts: u32,
    ) -> Result<Self, DukascopyError> {
        Self::launch_supervised(
            Launch {
                program: program.to_string(),
                args: args.to_vec(),
                config: config.clone(),
                jnlp: jnlp_of(config),
                quotes,
                attempt_timeout,
            },
            events,
            max_attempts,
            RESTART,
        )
    }

    /// [`Self::spawn_with_program`] with the restart policy as a parameter, so a test can script a sidecar that dies,
    /// is refused a few times and comes back in milliseconds (the production policy is [`RESTART`]).
    pub fn spawn_with_restart(
        program: &str,
        args: &[String],
        config: &DukascopyConfig,
        events: EventSender,
        restart: Reconnect,
    ) -> Result<Self, DukascopyError> {
        Self::launch_supervised(
            Launch {
                program: program.to_string(),
                args: args.to_vec(),
                config: config.clone(),
                jnlp: jnlp_of(config),
                quotes: QuoteBus::new(),
                attempt_timeout: ATTEMPT_READY_TIMEOUT,
            },
            events,
            MAX_SPAWN_ATTEMPTS,
            restart,
        )
    }

    /// The first start (up to `max_attempts` sidecars, a silent one killed and replaced), then the supervisor that
    /// starts every later one. A first start that fails is the live gate: no client, no supervisor.
    fn launch_supervised(
        launch: Launch,
        events: EventSender,
        max_attempts: u32,
        restart: Reconnect,
    ) -> Result<Self, DukascopyError> {
        let shared = Arc::new(Shared {
            events: events.clone(),
            stdin: Arc::new(Mutex::new(None)),
            inflight: Arc::new(Mutex::new(HashSet::new())),
            stranded: Mutex::new(HashSet::new()),
            venue_positions: Arc::new(Mutex::new(HashMap::new())),
            shadow: Mutex::new(ShadowBook::default()),
            // A bus outlives its clients (the process's one does): a new client starts from "up", never from the
            // last one's outage.
            quotes: {
                launch.quotes.set_sidecar(SidecarState::Up);
                Arc::clone(&launch.quotes)
            },
            last_fatal: Mutex::new(None),
            session: Mutex::new(None),
            amend_supported: AtomicBool::new(false),
            stopping: AtomicBool::new(false),
            restart,
        });
        let (wake, parked) = std_mpsc::channel::<()>();
        let began = Instant::now();
        let first = 'start: {
            for attempt in 1..=max_attempts {
                // The whole start never outlives `READY_TIMEOUT`, whatever the caller asked for.
                let Some(wait) = READY_TIMEOUT
                    .checked_sub(began.elapsed())
                    .filter(|left| !left.is_zero())
                    .map(|left| left.min(launch.attempt_timeout))
                else {
                    break;
                };
                match Self::start_sidecar(&launch, &shared, &wake, wait) {
                    Ok(started) => break 'start started,
                    // A verdict (fatal, spawn error, silent death): a respawn would only repeat it.
                    Err(AttemptFailed::Refused(_)) => return Err(DukascopyError::Unavailable),
                    // `start_sidecar` has already killed AND reaped the child: no second JForex session
                    // ever shares the login with a hung first one.
                    Err(AttemptFailed::NoReady) => tracing::warn!(
                        target: TARGET,
                        attempt,
                        max_attempts,
                        waited_secs = wait.as_secs(),
                        "the JForex sidecar sent no ready in time — killed it"
                    ),
                }
            }
            return Err(DukascopyError::Unavailable);
        };
        shared.amend_supported.store(first.amend_supported, Ordering::SeqCst);
        *locked(&shared.session) = Some(first.session);
        let supervisor = {
            let shared = Arc::clone(&shared);
            let wake = wake.clone();
            std::thread::Builder::new()
                .name("dukascopy-sidecar-supervisor".into())
                .spawn(move || supervise(&launch, &shared, &wake, &parked))
                .expect("spawn supervisor thread")
        };
        Ok(Self {
            shared,
            events,
            supervisor: Some(supervisor),
            wake_supervisor: wake,
            handshake_account: first.account,
            halt_path: None,
        })
    }

    /// ONE sidecar start: spawn the child, wait up to `wait` for its handshake, and on anything but
    /// a READY leave NO child behind (killed and reaped, reader joined). On a READY the child's stdin is the
    /// client's (`Shared::stdin`) and its reader runs; the caller installs the [`Session`].
    fn start_sidecar(
        launch: &Launch,
        shared: &Arc<Shared>,
        wake: &std_mpsc::Sender<()>,
        wait: Duration,
    ) -> Result<Started, AttemptFailed> {
        let config = &launch.config;
        let quotes = &launch.quotes;
        let mut child = std::process::Command::new(&launch.program)
            .args(&launch.args)
            .env("DUKASCOPY_LOGIN", &config.login)
            .env("DUKASCOPY_PASSWORD", &config.password)
            .env("DUKASCOPY_JNLP", &launch.jnlp)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|e| {
                tracing::error!(target: TARGET, error = %e, "failed to spawn bridge");
                AttemptFailed::Refused(format!("the sidecar process could not be started: {e}"))
            })?;

        *locked(&shared.stdin) = Some(child.stdin.take().expect("piped stdin"));
        let stdout = child.stdout.take().expect("piped stdout");

        // Handshake channel: the reader thread forwards the first ready/fatal here,
        // then pumps every subsequent `event` envelope into the core ingest.
        let (hs_tx, hs_rx) = std_mpsc::sync_channel::<Result<(String, bool), String>>(1);
        let quotes_reader = Arc::clone(quotes);
        let ledger_mount = config.ledger_mount.clone();
        let shared_reader = Arc::clone(shared);
        let ended = wake.clone();
        let reader = std::thread::Builder::new()
            .name("dukascopy-bridge-reader".into())
            .spawn(move || {
                let pump = shared_reader.events.clone();
                let reader_stdin = Arc::clone(&shared_reader.stdin);
                let inflight_reader = Arc::clone(&shared_reader.inflight);
                let venue_positions_reader = Arc::clone(&shared_reader.venue_positions);
                let mut handshaken = false;
                // The price lane's attach id (`crate::quotes`), set on `ready` and given back at EOF by THIS thread,
                // so a tick is never seen before the attach nor after the detach.
                let mut attached: Option<u64> = None;
                // Set when the core stopped listening (`blocking_send` failed): nobody is left to serve.
                let mut core_gone = false;
                for line in BufReader::new(stdout).lines() {
                    let Ok(line) = line else { break };
                    match parse_envelope(&line) {
                        Some(Envelope::Ready { account, balance, features, currency }) if !handshaken => {
                            handshaken = true;
                            tracing::info!(
                                target: TARGET, %account, balance, ?features, ?currency, "bridge ready"
                            );
                            let can_amend = features.iter().any(|f| f == FEATURE_MODIFY);
                            let id = quotes_reader.attach(
                                Arc::clone(&reader_stdin) as Arc<dyn LinePort>,
                                &features,
                            );
                            attached = Some(id);
                            // The account's currency (decision 0183) is on the bus BEFORE the start is told it
                            // succeeded, so the trade lane's first read of it finds it.
                            quotes_reader.set_account_currency(id, currency.as_deref());
                            let _ = hs_tx.send(Ok((account, can_amend)));
                            // The engine's only way to learn the venue's balance on this unreconciled
                            // venue (`crate::ready_balance`): ONE wallet frame per sidecar start, sent after
                            // the handshake reply so a stalled lane can never cost the start itself, and
                            // only for a start that was told its mount. The currency is the bus's own
                            // reading of the line, so the gate and the node's ledger agree on it.
                            if let Some(mount) = ledger_mount.as_deref() {
                                let stated = quotes_reader.account_currency();
                                let now = vike_model::now_ms();
                                // `wallet_frame` says on its own whether it built one and, if not, why.
                                if let Ok(frame) = wallet_frame(balance, stated.as_deref(), mount, now)
                                    && pump.blocking_send(frame).is_err()
                                {
                                    core_gone = true; // core gone — stop pumping
                                    break;
                                }
                            }
                        }
                        Some(Envelope::Tick { symbol, bid, ask, time }) => {
                            // No tick precedes `ready` on a protocol-honest sidecar (and none is sent before a
                            // `subscribe`), so one that does is dropped like any pre-ready line. `deliver` only
                            // queues for the consumer: a fault there never reaches this thread.
                            if handshaken {
                                quotes_reader.deliver(&symbol, bid, ask, time, vike_model::now_ms());
                            }
                        }
                        Some(Envelope::Subscription { symbol, ok, reason }) => {
                            // The jar's answer to a `subscribe` (only a jar that announced `tick-acks` sends one).
                            if handshaken {
                                quotes_reader.subscription(&symbol, ok, &reason);
                            }
                        }
                        Some(Envelope::Ready { .. }) => {} // duplicate ready: ignore
                        Some(Envelope::Fatal { reason }) => {
                            tracing::error!(target: TARGET, %reason, "bridge fatal");
                            if !handshaken {
                                let _ = hs_tx.send(Err(reason));
                            } else {
                                // the words of the death, for `Shared::went_down`
                                *locked(&shared_reader.last_fatal) = Some(reason);
                            }
                            // post-ready fatal precedes sidecar exit → EOF ends the loop
                        }
                        Some(Envelope::Event { event }) => {
                            if !handshaken {
                                // Protocol: no event may precede the ready handshake —
                                // a ghost event here would reach the core for an order
                                // it never submitted (and pre-ready blocking_send could
                                // wedge the not-yet-live ingest lane).
                                tracing::debug!(target: TARGET, ?event, "dropped pre-ready event");
                                continue;
                            }
                            if let Some(coid) = terminal_coid(&event) {
                                inflight_reader.lock().unwrap().remove(coid);
                            }
                            // Shadow-fold the bare fill lane ONLY (the OrderFilled wrap carries
                            // the same fill and must not double-fold — same law as the Account).
                            if let Event::Fill(fill) = event.as_ref() {
                                locked(&shared_reader.shadow).fold_fill(fill);
                            }
                            if pump.blocking_send(*event).is_err() {
                                core_gone = true; // core gone — stop pumping
                                break;
                            }
                        }
                        Some(Envelope::Position { symbol, size, avg_px, ts }) => {
                            if !handshaken {
                                tracing::debug!(target: TARGET, %symbol, "dropped pre-ready position line");
                                continue;
                            }
                            // Capture the RAW venue truth for the reconcile seam BEFORE the
                            // re-anchor consumes it — so the snapshot reflects the venue even in
                            // the `SizeMismatch` case the re-anchor refuses to auto-heal.
                            venue_positions_reader
                                .lock()
                                .unwrap()
                                .insert(symbol.clone(), VenuePosition { size, avg_px, ts });
                            // The shadow is the CLIENT's, not this sidecar's (`Shared::shadow`): a restart must not
                            // hand the next sidecar an empty one.
                            let resolved = locked(&shared_reader.shadow).reanchor(&symbol, size, avg_px, ts);
                            match resolved {
                                Reanchor::Corrected(legs) => {
                                    // Netted-close attribution drift: fold the venue truth in via
                                    // ordinary synthesized fills (see netting.rs module doc).
                                    tracing::info!(
                                        target: TARGET,
                                        %symbol, venue_size = size, venue_avg = avg_px,
                                        "netting re-anchor: folding venue basis via synthesized close+reopen legs"
                                    );
                                    if legs.into_iter().any(|leg| pump.blocking_send(leg).is_err())
                                    {
                                        core_gone = true; // core gone — stop pumping
                                        break;
                                    }
                                }
                                Reanchor::SizeMismatch { local_size, venue_size } => {
                                    // Never auto-healed here — a missed/spurious fill is real
                                    // recon territory; keep it LOUD until fills explain it.
                                    tracing::warn!(
                                        target: TARGET,
                                        %symbol, local_size, venue_size,
                                        "sidecar position line disagrees with folded fills — NOT auto-healed"
                                    );
                                }
                                Reanchor::Baseline => {
                                    tracing::info!(
                                        target: TARGET,
                                        %symbol, venue_size = size, venue_avg = avg_px,
                                        "adopted venue position line as shadow baseline (no folded fills yet)"
                                    );
                                }
                                Reanchor::InSync => {}
                            }
                        }
                        None => {
                            let snippet: String = line.chars().take(160).collect();
                            tracing::debug!(target: TARGET, %snippet, "skipped unparseable stdout line");
                        }
                    }
                }
                // EOF: the price lane loses its sidecar (the wants stay, for the next one).
                if let Some(id) = attached {
                    quotes_reader.detach(id);
                }
                // EOF: a clean stop, a death, or a core that went away. A sidecar that had said `ready` and
                // ended while the client runs is DOWN: say so first, so a submit that races the drain below is
                // refused by name instead of entering a dead pipe.
                if core_gone {
                    shared_reader.stopping.store(true, Ordering::SeqCst);
                }
                let died = handshaken && !shared_reader.is_stopping();
                if died {
                    shared_reader.went_down();
                }
                // Any order still awaiting its terminal event would otherwise wedge non-terminal (a later
                // failed stdin write only covers NEW submits) — synthesize its rejection now. EXACTLY ONCE: the
                // drain and `submit`'s failed write both take a coid out of the set before rejecting it, so
                // whoever finds it there rejects it and the other finds nothing.
                let orphans: Vec<String> = {
                    let mut set = inflight_reader.lock().unwrap();
                    set.drain().collect()
                };
                for coid in orphans {
                    tracing::warn!(target: TARGET, %coid, "bridge died with in-flight order — synthesizing OrderRejected");
                    if died {
                        let mut stranded = locked(&shared_reader.stranded);
                        if stranded.len() < MAX_STRANDED {
                            stranded.insert(coid.clone());
                        }
                    }
                    let _ = pump.blocking_send(Event::OrderRejected(OrderRejected {
                        client_order_id: coid,
                        reason: BRIDGE_DIED.into(),
                        ts: 0,
                    }));
                }
                // The supervisor starts the next sidecar. A start that never said `ready` is
                // `start_sidecar`'s outcome to report, not a death: it tells nobody here.
                if handshaken {
                    let _ = ended.send(());
                }
            })
            .expect("spawn reader thread");

        // Ok(ready) | Err(fatal) | RecvTimeout (hang) | Disconnected (silent death). Waited in short slices so
        // that a client stopped meanwhile (a detach during a restart) does not wait for a login.
        let waiting = Instant::now();
        let handshake = loop {
            let left = wait.saturating_sub(waiting.elapsed());
            match hs_rx.recv_timeout(left.min(HANDSHAKE_POLL)) {
                Err(std_mpsc::RecvTimeoutError::Timeout)
                    if !left.is_zero() && !shared.is_stopping() => {}
                other => break other,
            }
        };
        match handshake {
            Ok(Ok((account, amend_supported))) => {
                // ⚠ This used to resolve — and, once per process, REPORT — the HALT sentinel here
                // (`vike_boot::halt::halt_path_from_env`), so an unarmable switch landed in
                // the trace file at START time. The resolver is the composition layer's now:
                // `vike-tradehub-core` calls it before the first start and the start hands the answer to
                // `with_halt_path`, which is the same moment and the same report (decision 0099).
                Ok(Started { session: Session { child, reader }, account, amend_supported })
            }
            other => {
                let _ = child.kill();
                let _ = child.wait();
                *locked(&shared.stdin) = None;
                let _ = reader.join();
                Err(match other {
                    Ok(Err(reason)) => {
                        AttemptFailed::Refused(format!("the sidecar said fatal: {reason}"))
                    }
                    Err(std_mpsc::RecvTimeoutError::Timeout) => AttemptFailed::NoReady,
                    _ => AttemptFailed::Refused(
                        "the sidecar exited before it said ready".to_string(),
                    ),
                })
            }
        }
    }

    /// **The account identifier this session's READY handshake carried**, verbatim — the venue's own
    /// answer to *which account did these credentials authenticate as*.
    ///
    /// This is the fact `docs/superpowers/specs/2026-09-14-the-credential-schema.md` §4.5 calls the
    /// handshake writer's input and §9 calls the evidence that dukascopy's book is not unknowable.
    /// A caller records it; NOTHING here writes a store — see [`Self::handshake_account`]'s field
    /// doc for why it is not a secret.
    ///
    /// ⚠ **Its FORM is not settled and this accessor asserts none.** The sidecar sends
    /// `IAccount.getAccountId()`, the one frame this crate pins is login-shaped (`"DEMO2cGyrc"`),
    /// and the numbers an operator reads off Dukascopy's own page are numeric. §9 records that as
    /// an open question to be settled by MEASUREMENT on an SDK-staged box, so a consumer that
    /// compares this against a stored value must treat a difference as a report rather than as
    /// proof — `vike_secrets::account_confirmation::verdict` is where that disposition lives.
    pub fn handshake_account(&self) -> &str {
        &self.handshake_account
    }

    /// Hand this client the sentinel it watches: `crate::adapter`'s `start_with` passes
    /// `AdapterInputs::process.halt_path`, the one file `vike-tradehub-core` resolved for the whole process,
    /// and a test passes a path of its own. Mirrors
    /// `vike_bridge_core::exec_actor::ExecActor::with_halt_path`; without it the client watches
    /// nothing (see the field's doc).
    pub fn with_halt_path(mut self, path: PathBuf) -> Self {
        self.halt_path = Some(path);
        self
    }

    /// True while the sentinel this client watches exists ⇒ refuse a submit that OPENS risk.
    ///
    /// Consulted at the submit boundary only, never on the reader thread's per-message path.
    fn halt_engaged(&self) -> bool {
        vike_exec::halt::sentinel_engaged(self.halt_path.as_deref())
    }

    /// Build a [`DukascopyReconClient`] sharing this client's live venue-position snapshot (the
    /// recon-breadth seam). Unlike the REST venues' `recon_client(config, symbol)` factories, the
    /// Dukascopy reconcile client is DERIVED from the running exec client: there is no REST endpoint
    /// to connect to, so the venue's position truth arrives only through the sidecar stdio this
    /// client owns. See `recon_client.rs` for what it covers (positions) and what it defers
    /// (orders/fills — the Java/jar-gated query slice).
    pub fn recon_client(&self) -> DukascopyReconClient {
        DukascopyReconClient::new(Arc::clone(&self.shared.venue_positions))
    }

    /// Where the sidecar is now: [`SidecarState::Up`], or [`SidecarState::Down`] with the sidecar's own words, how
    /// many restarts have failed and how long until the next. The same answer that every refusal carries.
    pub fn sidecar_state(&self) -> SidecarState {
        self.shared.state()
    }

    /// The words of a refusal while no sidecar runs; `None` while one does.
    fn down_words(&self) -> Option<String> {
        self.shared.state().down_words()
    }

    /// Whether `coid` was in flight when a sidecar died: the sidecar that replaced it has no memory of the order.
    fn stranded(&self, coid: &str) -> bool {
        locked(&self.shared.stranded).contains(coid)
    }

    /// Write one command line; `false` marks the bridge dead (stdin dropped).
    fn write_line(&mut self, line: &str) -> bool {
        self.shared.stdin.send(line)
    }

    /// Tell the sidecar to exit, and WAIT FOR NOTHING — the first half of [`Self::shutdown_child`],
    /// hoisted to back `ExecutionClient::begin_detach`
    /// (`crates/vike-exec/src/execution_engine/client.rs`). On this venue the wind-down being
    /// overlapped is a CHILD PROCESS's exit rather than a socket read timeout, but the shape is the
    /// same: `shutdown_child` then spends up to `SHUTDOWN_GRACE` polling `try_wait`, and a core
    /// tearing down several venues should have started that clock on all of them first.
    ///
    /// ⚠ **It stops the supervisor FIRST**: the sidecar's exit that this very command causes must not be read as a
    /// death and restarted. Idempotent: the write is `let _ =` and clearing an already-cleared `stdin` is a no-op, so
    /// [`Self::shutdown_child`] running after this re-signals harmlessly — its own
    /// `write_line` simply fails into the same ignored result.
    fn signal_child_shutdown(&mut self) {
        self.shared.stopping.store(true, Ordering::SeqCst);
        let _ = self.wake_supervisor.send(());
        let _ = self.write_line(&encode_line(&Command::Shutdown));
        // close the pipe: stdin EOF is the sidecar's fallback exit signal (and the price lane's writes now fail)
        *locked(&self.shared.stdin) = None;
    }

    /// `shutdown` → ≤5s grace → kill → reap child + reader (idempotent). The signalling half is
    /// [`Self::signal_child_shutdown`]; everything below it WAITS.
    fn shutdown_child(&mut self) {
        self.signal_child_shutdown();
        // `stopping` is set, so the supervisor installs no sidecar from here on (it kills one it had in hand): what
        // is in the slot now is the last.
        let session = locked(&self.shared.session).take();
        if let Some(Session { mut child, reader }) = session {
            let deadline = Instant::now() + SHUTDOWN_GRACE;
            loop {
                match child.try_wait() {
                    Ok(Some(_)) => break,
                    Ok(None) if Instant::now() < deadline => {
                        std::thread::sleep(Duration::from_millis(50));
                    }
                    _ => {
                        let _ = child.kill();
                        let _ = child.wait();
                        break;
                    }
                }
            }
            // Bounded: the reader normally exits at pipe EOF, but it can be parked in
            // blocking_send on a stalled ingest channel — killing the child doesn't
            // unblock that. Wait briefly, then detach; the thread dies on its own when
            // the channel unblocks/closes (bounded teardown beats a guaranteed join).
            join_bounded(reader);
        }
        // The supervisor wakes on the stop and ends within one poll of a login in progress.
        if let Some(handle) = self.supervisor.take() {
            join_bounded(handle);
        }
    }
}

/// Join `handle` if it ends within `SHUTDOWN_GRACE`, else leave it to end on its own.
fn join_bounded(handle: std::thread::JoinHandle<()>) {
    let deadline = Instant::now() + SHUTDOWN_GRACE;
    while !handle.is_finished() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    if handle.is_finished() {
        let _ = handle.join();
    }
}

/// The JNLP URL a sidecar logs in through: `config.server` when it is one, else the demo default.
fn jnlp_of(config: &DukascopyConfig) -> String {
    let server = config.server.trim();
    if server.is_empty() {
        DEFAULT_DEMO_JNLP.to_string()
    } else if !server.ends_with(".jnlp") {
        // The server setting commonly holds the web-platform LOGIN URL, which the
        // sidecar would feed to IClient.connect as a JNLP and hang.
        tracing::warn!(
            target: TARGET,
            server = ?server,
            "venue.dukascopy.demo.server is not a JNLP URL; using the default demo JNLP"
        );
        DEFAULT_DEMO_JNLP.to_string()
    } else {
        server.to_string()
    }
}

/// The supervisor thread's body: park while a sidecar lives, and when it ends start another until the client stops.
///
/// Never gives up while the client lives (the owner's order: a dead venue path comes back by itself, no hand step),
/// and never spins: every restart waits `restart_delay`, and a start is bounded by `Launch::attempt_timeout`.
fn supervise(
    launch: &Launch,
    shared: &Arc<Shared>,
    wake: &std_mpsc::Sender<()>,
    parked: &std_mpsc::Receiver<()>,
) {
    // The wait's exponent. It counts the restarts that failed, and ALSO a restarted sidecar that died before it had
    // lived `restart.max` (a flap: `ready`, then cut off again), which a count started over at every death would
    // answer with the first, shortest wait for ever: a login every ~5 s against a venue that keeps ending the session.
    // Only a sidecar that lived that long starts the count over. The FIRST sidecar's death is always a fresh outage.
    let mut failed = 0u32;
    let mut back_at: Option<Instant> = None;
    loop {
        // Parked: a sidecar's reader says it ended (`ended.send`), the client says stop (`wake_supervisor`).
        if parked.recv().is_err() || shared.is_stopping() {
            return;
        }
        failed = match back_at {
            Some(at) if at.elapsed() < shared.restart.max => failed + 1,
            _ => 0,
        };
        // The dead sidecar's reader has finished its drain; reap the child so no second JForex session can ever
        // share the login with it, then start over.
        let dead = locked(&shared.session).take();
        if let Some(dead) = dead {
            dead.kill_and_reap();
        }
        *locked(&shared.stdin) = None;
        let down_since = Instant::now();
        let died_of = match shared.state() {
            SidecarState::Down { why, .. } => why,
            SidecarState::Up => "its output ended".to_string(),
        };
        // The latest answer to "why is it down": the death's, then each failed restart's.
        let mut why_now = died_of.clone();
        loop {
            let delay = restart_delay(&shared.restart, failed);
            shared.set_state(SidecarState::Down {
                why: why_now.clone(),
                failed_restarts: failed,
                retry_in: delay,
            });
            tracing::warn!(
                target: TARGET,
                failed_restarts = failed,
                retry_in = ?delay,
                why = %why_now,
                "the JForex sidecar is still DOWN: starting another"
            );
            if !pause(shared, parked, delay) {
                return;
            }
            match DukascopyExecutionClient::start_sidecar(
                launch,
                shared,
                wake,
                launch.attempt_timeout,
            ) {
                Ok(started) => {
                    let mut slot = locked(&shared.session);
                    // Installed under the slot's lock, and `stopping` is read under it: a detach that ran
                    // first took the slot empty and set the flag, so this sidecar is killed here and a detach
                    // that runs later finds it in the slot. Either way none outlives the client.
                    if shared.is_stopping() {
                        drop(slot);
                        started.session.kill_and_reap();
                        *locked(&shared.stdin) = None;
                        return;
                    }
                    shared.amend_supported.store(started.amend_supported, Ordering::SeqCst);
                    *slot = Some(started.session);
                    drop(slot);
                    back_at = Some(Instant::now());
                    shared.set_state(SidecarState::Up);
                    tracing::warn!(
                        target: TARGET,
                        account = %started.account,
                        failed_restarts = failed,
                        down_secs = down_since.elapsed().as_secs(),
                        "the JForex sidecar is BACK (bridge ready): orders are accepted again. ⚠ This sidecar has \
                         no memory of the orders the dead one placed: a cancel or amend of an order that was in \
                         flight at the death is refused by name, and a position that moved during the outage \
                         shows at the next fill's position line"
                    );
                    break;
                }
                Err(failure) => {
                    failed += 1;
                    let reason = match failure {
                        AttemptFailed::NoReady => {
                            "no ready within the start's time limit (killed it)".to_string()
                        }
                        AttemptFailed::Refused(why) => why,
                    };
                    tracing::warn!(
                        target: TARGET,
                        failed_restarts = failed,
                        %reason,
                        "restarting the JForex sidecar failed; it will be tried again"
                    );
                    why_now = format!("{died_of}; the last restart failed: {reason}");
                    if shared.is_stopping() {
                        return;
                    }
                }
            }
        }
    }
}

/// Wait `delay`, or less if the client stops; `false` when it did.
fn pause(shared: &Shared, parked: &std_mpsc::Receiver<()>, delay: Duration) -> bool {
    let until = Instant::now() + delay;
    loop {
        if shared.is_stopping() {
            return false;
        }
        let left = until.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return true;
        }
        // A stop wakes this at once; any other message is a stale wake that only re-checks the clock.
        let _ = parked.recv_timeout(left);
    }
}

impl ExecutionClient for DukascopyExecutionClient {
    fn submit(&mut self, request: &OrderRequest) {
        // A reduce-only LIMIT is refused by name, terminally, before anything is sent: the sidecar nets MARKET
        // orders only, so a resting reduce-only limit is a plain pending order and OPENS an opposite position when
        // it is hit. It matters since the row declares `stop`: a bracket (entry, stop-loss, take-profit) is no
        // longer refused whole at the core edge, and its take-profit leg is exactly such a limit. A refused leg
        // leaves its OCO sibling, the native stop-loss, in place (`drive_contingency_on_terminal` keeps it).
        if request.reduce_only && request.order_type == vike_model::OrderType::Limit {
            let _ = self.events.blocking_send(Event::OrderRejected(OrderRejected {
                client_order_id: request.client_order_id.clone(),
                reason: "dukascopy: a reduce-only LIMIT order is not wired: the sidecar nets only market orders, \
                         so a resting limit would OPEN an opposite position when hit instead of closing the \
                         open one (a bracket's take-profit leg is such an order; its stop-loss leg is \
                         unaffected); close with a reduce-only market order or protect with a reduce-only \
                         stop; this order was NOT sent"
                    .into(),
                ts: request.ts,
            }));
            return;
        }
        // HALT kill-switch. While the sentinel exists, refuse a submit that OPENS or ADDS risk and
        // synthesize the terminal rejection — the same shape `ExecActor` uses, checked BEFORE the
        // `OrderSubmitted` half so a refused order never enters the sidecar's inflight set.
        //
        // ⚠ A REDUCING submit is ADMITTED (`vike_exec::halt::halt_admits_submit`, the ONE
        // definition of that rule): a halt must never trap the operator in a position, and this
        // venue holds no position book at this boundary, so the caller-asserted flag is the only
        // evidence there is.
        //
        // This client is BESPOKE — it owns a child process rather than an `ExecActor` — so it
        // inherited nothing, and for a long time it consulted the sentinel nowhere.
        // `docs/ops/kill-switches.md` carried that as a LATENT gap rather than a live one, because
        // nothing in production started this venue: the risk was somebody wiring the venue without
        // wiring the switch. Closing it here means the venue can be wired by itself.
        if self.halt_engaged() && !vike_exec::halt::halt_admits_submit(request) {
            let _ = self.events.blocking_send(Event::OrderRejected(OrderRejected {
                client_order_id: request.client_order_id.clone(),
                reason: vike_exec::halt::HALT_REJECT_REASON.into(),
                ts: request.ts,
            }));
            return;
        }
        // No sidecar runs (it died and the supervisor is starting another): refuse by name, terminally, before the
        // order enters the in-flight set. Nothing was sent, so nothing can have reached the venue.
        if let Some(words) = self.down_words() {
            let _ = self.events.blocking_send(Event::OrderRejected(OrderRejected {
                client_order_id: request.client_order_id.clone(),
                reason: format!("{BRIDGE_UNAVAILABLE}: {words}; this order was NOT sent").into(),
                ts: request.ts,
            }));
            return;
        }
        // Rust-side synchronous half of the lifecycle.
        let _ = self.events.blocking_send(Event::OrderSubmitted(OrderSubmitted {
            client_order_id: request.client_order_id.clone(),
            ts: request.ts,
        }));
        // Track BEFORE writing: if the child dies with this order in flight, the
        // reader's EOF drain synthesizes the terminal rejection.
        self.shared.inflight.lock().unwrap().insert(request.client_order_id.clone());
        let line = encode_line(&Command::Submit { order: Box::new(request.clone()) });
        if !self.write_line(&line) {
            // Dead bridge: the order must still reach a terminal state (spec). Only
            // reject if the reader's EOF drain (or a real terminal event) hasn't
            // already taken this coid — belt and braces without double-rejecting.
            let still_inflight =
                self.shared.inflight.lock().unwrap().remove(&request.client_order_id);
            if still_inflight {
                let _ = self.events.blocking_send(Event::OrderRejected(OrderRejected {
                    client_order_id: request.client_order_id.clone(),
                    reason: format!(
                        "{BRIDGE_UNAVAILABLE}: the write to the JForex sidecar failed (it has exited and the \
                         node is restarting it); this order was NOT sent"
                    )
                    .into(),
                    ts: request.ts,
                }));
            }
        }
    }

    /// A cancel is never gated by a halt. While no sidecar runs, or when the order was in flight at a sidecar's
    /// death (the new one does not know it), it is refused by name with the non-terminal `OrderCancelRejected`
    /// instead of being dropped in silence (the sidecar's own `handleCancel` skips an unknown order without a word).
    fn cancel(&mut self, client_order_id: &str) {
        let refuse = |events: &EventSender, reason: String| {
            let _ = events.blocking_send(Event::OrderCancelRejected(OrderCancelRejected {
                client_order_id: client_order_id.to_string(),
                reason: reason.into(),
                ts: vike_model::now_ms(),
            }));
        };
        if let Some(words) = self.down_words() {
            refuse(
                &self.events,
                format!("dukascopy: {BRIDGE_UNAVAILABLE}: {words}; the cancel was NOT sent"),
            );
            return;
        }
        if self.stranded(client_order_id) {
            refuse(&self.events, stranded_words("cancel"));
            return;
        }
        let line = encode_line(&Command::Cancel { client_order_id: client_order_id.into() });
        if !self.write_line(&line) {
            tracing::warn!(target: TARGET, client_order_id, "cancel not sent — the sidecar's pipe is closed");
            refuse(
                &self.events,
                format!(
                    "dukascopy: {BRIDGE_UNAVAILABLE}: the write to the JForex sidecar failed (it has exited \
                     and the node is restarting it); the cancel was NOT sent"
                ),
            );
        }
    }

    /// Amend a resting limit order in place (`IOrder.setOpenPrice` / `setRequestedAmount` in the sidecar). The
    /// sidecar answers with `OrderModified` (the terms JForex now holds) or `OrderModifyRejected`, both through
    /// the reader; this side only refuses what it can know without asking.
    ///
    /// ⚠ **HALT blocks a modify outright**, like every client that has a sentinel (`ExecActor::modify`, ctrader):
    /// an amend can raise the size of a resting order, and the exit path under a halt is a cancel. It says so with
    /// the non-terminal advisory instead of returning in silence.
    fn modify(&mut self, order: &OrderRequest, new_qty: Option<f64>, new_price: Option<f64>) {
        let refuse = |events: &EventSender, reason: &str| {
            let _ = events.blocking_send(Event::OrderModifyRejected(OrderModifyRejected {
                client_order_id: order.client_order_id.clone(),
                reason: reason.into(),
                ts: order.ts,
            }));
        };
        if self.halt_engaged() {
            refuse(&self.events, vike_exec::halt::HALT_REJECT_REASON);
            return;
        }
        if new_qty.is_none() && new_price.is_none() {
            refuse(
                &self.events,
                "dukascopy: an amend names neither a new quantity nor a new price",
            );
            return;
        }
        if let Some(words) = self.down_words() {
            refuse(
                &self.events,
                &format!("dukascopy: {BRIDGE_UNAVAILABLE}: {words}; the amend was NOT sent"),
            );
            return;
        }
        if self.stranded(&order.client_order_id) {
            refuse(&self.events, &stranded_words("amend"));
            return;
        }
        if !self.shared.amend_supported.load(Ordering::SeqCst) {
            refuse(
                &self.events,
                "dukascopy: the installed JForex sidecar jar predates the amend command; install the \
                 jar from this release (`just fetch-tools`) and restart",
            );
            return;
        }
        let line = encode_line(&Command::Modify {
            client_order_id: order.client_order_id.clone(),
            new_qty,
            new_price,
            ts: order.ts,
        });
        if !self.write_line(&line) {
            // The order is still live (or gone with the child): the amend is refused, non-terminal; the
            // reader's EOF drain owns the order's own terminal.
            refuse(
                &self.events,
                &format!(
                    "dukascopy: {BRIDGE_UNAVAILABLE}: the write to the JForex sidecar failed (it has \
                     exited and the node is restarting it); the amend was NOT sent"
                ),
            );
        }
    }

    /// Phase-one teardown seam — see `crates/vike-exec/src/execution_engine/client.rs`'s
    /// `ExecutionClient::begin_detach`. This venue owns a CHILD PROCESS rather than a socket pump,
    /// so what gets overlapped here is the sidecar's own exit.
    fn begin_detach(&mut self) {
        self.signal_child_shutdown();
    }
    fn detach(&mut self) {
        self.shutdown_child();
    }
}

impl Drop for DukascopyExecutionClient {
    fn drop(&mut self) {
        self.shutdown_child(); // idempotent: try_wait returns Ok(Some) once reaped
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    /// The READY handshake's bound is the exec row's 300 s, the figure the sidecar's own login
    /// polling (up to 240 s) is sized under, and the Java sidecar's `JnlpRetry` arithmetic cites it
    /// by name, so it is pinned here as a literal rather than as a restatement of the row.
    #[test]
    fn the_ready_timeout_is_the_exec_rows_three_hundred_seconds() {
        assert_eq!(READY_TIMEOUT, Duration::from_secs(300));
    }

    /// The live gate reports the path it actually TRIED, which is the whole operator-facing point
    /// of resolving in the composition root: `spawn` no longer has an opinion about where a jar
    /// lives, so a wrong answer has to be visible as a wrong PATH rather than as a venue that went
    /// quiet. (`vendored_java_picks_highest_jdk_or_none` stood here until the sweep moved to
    /// `config.rs` beside the ladder it belongs to; `the_jre_sweep_picks_the_highest_image_or_falls_through`
    /// is its successor.)
    #[test]
    fn a_missing_jar_is_unavailable_rather_than_a_spawn() {
        let (tx, _rx) = vike_exec::event_channel(4);
        let tools = DukascopyTools {
            java: "java".into(),
            bridge_jar: PathBuf::from("Z:/no-such-project/bin/jforex/jforex-bridge.jar"),
            jforex_home: None,
        };
        let config = DukascopyConfig {
            login: "l".into(),
            password: "p".into(),
            server: String::new(),
            ledger_mount: None,
        };
        assert_eq!(
            DukascopyExecutionClient::spawn(config, &tools, tx).err(),
            Some(DukascopyError::Unavailable)
        );
    }

    fn tools_with_home(home: Option<&str>) -> DukascopyTools {
        DukascopyTools {
            java: "java".into(),
            bridge_jar: PathBuf::from("/p/bin/jforex/jforex-bridge.jar"),
            jforex_home: home.map(PathBuf::from),
        }
    }

    /// ⚠ **This asserts the ARGUMENT VECTOR, not the BEHAVIOUR.** It proves what `spawn` would hand
    /// `std::process::Command` and nothing about what a JVM does with it — no process is started
    /// here and no `user.home` is observed. The behavioural half, which starts a real child and
    /// reads back the home that child was actually given, is
    /// `crates/bridges/dukascopy/tests/dukascopy_exec.rs`'s
    /// `the_sidecar_is_started_with_the_projects_jforex_home`; even that one models the JVM rather
    /// than being one.
    ///
    /// What it DOES carry on its own is the ordering rule, which is a real correctness property:
    /// the flag is a JVM option, so `-jar` must come after it or the JVM never sees it.
    #[test]
    fn the_home_flag_is_a_jvm_option_and_precedes_the_jar() {
        let args = jvm_args(&tools_with_home(Some("/p/settings/state/jforex")));
        assert_eq!(
            args,
            vec![
                "-Duser.home=/p/settings/state/jforex".to_string(),
                "-Xmx512m".to_string(),
                "-jar".to_string(),
                "/p/bin/jforex/jforex-bridge.jar".to_string(),
            ]
        );
        let flag = args.iter().position(|a| a.starts_with(USER_HOME_FLAG)).expect("the flag");
        let jar = args.iter().position(|a| a == "-jar").expect("-jar");
        assert!(flag < jar, "a -D flag after -jar is an argument to Bridge.main, not a JVM option");
    }

    /// No home resolved → NO `-D` flag, and the rest of the command line is what this venue has
    /// always spawned plus the heap cap. The `None` rung must leave the JVM's own `user.home` alone
    /// rather than passing an empty or guessed one: `-Duser.home=` with nothing after it would point
    /// the SDK at the working directory, which is worse than the default it replaced.
    #[test]
    fn no_resolved_home_emits_no_home_flag_at_all() {
        let args = jvm_args(&tools_with_home(None));
        assert_eq!(
            args,
            vec![
                "-Xmx512m".to_string(),
                "-jar".to_string(),
                "/p/bin/jforex/jforex-bridge.jar".to_string()
            ]
        );
        assert!(args.iter().all(|a| !a.starts_with("-D")), "{args:?}");
    }

    /// **The heap cap is one literal argv element, in the JVM's option section, with or without a
    /// home** — the 512 MiB figure is the arithmetic in [`MAX_HEAP_FLAG`]'s doc (a quarter of the
    /// trading unit's `MemoryMax=2G`), so the literal is pinned here: a drift of the number is a
    /// decision, not an edit. Like the home test above, this asserts the ARGUMENT VECTOR and says
    /// nothing about what a JVM does with it; the vector is split the way a JVM splits it
    /// (everything before `-jar` is the JVM's own).
    #[test]
    fn the_heap_cap_is_one_explicit_option_before_the_jar() {
        for home in [None, Some("/p/settings/state/jforex")] {
            let args = jvm_args(&tools_with_home(home));
            let jar = args.iter().position(|a| a == "-jar").expect("-jar");
            let jvm_section = &args[..jar];
            assert_eq!(
                jvm_section.iter().filter(|a| a.starts_with("-Xmx")).collect::<Vec<_>>(),
                [&"-Xmx512m".to_string()],
                "exactly one maximum-heap option, 512 MiB, before -jar: {args:?}"
            );
            assert!(
                args[jar + 1..].iter().all(|a| !a.starts_with("-X")),
                "a -X option after -jar is an argument to Bridge.main, not the JVM: {args:?}"
            );
        }
    }

    /// **A path with spaces or a drive letter is ONE argv element**, carried verbatim with nothing
    /// quoted, escaped or split by us — `std::process::Command` hands each element to the OS as a
    /// unit. The resolver half of the same Windows concern is
    /// `crates/bridges/dukascopy/src/config_tests.rs`'s
    /// `a_state_path_with_spaces_reaches_the_home_unaltered`.
    #[test]
    fn the_home_flag_is_one_argv_element_whatever_the_path() {
        let home = r"C:\Program Files\vike trader\settings\state\jforex";
        let args = jvm_args(&tools_with_home(Some(home)));
        let flag = &args[0];
        assert_eq!(flag, &format!("{USER_HOME_FLAG}{home}"));
        assert!(!flag.contains('"'), "we must not quote: {flag}");
        assert_eq!(
            flag.strip_prefix(USER_HOME_FLAG),
            Some(home),
            "the path must round-trip through the flag unaltered"
        );
    }

    /// **The restart wait is the `DUKASCOPY` row's: 5 s, doubling, never above a minute** (decision 0160: the numbers
    /// live in the row and `RESTART` reads them, so this pins the READING, and a retune of the row is the one edit
    /// that moves it). Never a tight loop, never a wait that grows without bound.
    #[test]
    fn the_restart_wait_doubles_from_five_seconds_to_a_minute_and_stays_there() {
        assert_eq!(RESTART.initial, Duration::from_secs(5));
        assert_eq!(RESTART.max, Duration::from_secs(60));
        let waits: Vec<u64> = (0..8).map(|n| restart_delay(&RESTART, n).as_secs()).collect();
        assert_eq!(waits, [5, 10, 20, 40, 60, 60, 60, 60]);
        assert_eq!(restart_delay(&RESTART, u32::MAX), RESTART.max, "no overflow, no escape");
    }

    /// Every refusal while no sidecar runs carries the sidecar's reason, the failed restarts and the next try.
    #[test]
    fn a_down_sidecar_gives_its_reason_and_its_restart_in_the_words_of_a_refusal() {
        assert_eq!(SidecarState::Up.down_words(), None);
        let down = SidecarState::Down {
            why: "JForex session disconnected".into(),
            failed_restarts: 2,
            retry_in: Duration::from_secs(20),
        };
        let words = down.down_words().expect("down has words");
        assert!(words.contains("JForex session disconnected"), "{words}");
        assert!(words.contains("2 restart(s) failed"), "{words}");
        assert!(words.contains("20s"), "{words}");
    }
}
