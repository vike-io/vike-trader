//! HALT file sentinel — the operator kill switch that stops NEW order placement.
//!
//! Existence of a sentinel file on disk halts OPENING/ADDING orders at the `ExecActor` submit
//! boundary (see `exec_actor.rs`). It is designed to work when the GUI/runtime is otherwise wedged:
//! an operator does `touch <HALT file>` over ssh on the headless box and the very NEXT submit is
//! refused — no IPC, no running event loop, no responsive UI, no readable settings database; just a
//! filesystem `exists()` check at the submit seam.
//!
//! ## Path precedence (the shape of `vike_log::resolve_log_dir`: a default and a last resort)
//!
//! 2. `<project>/settings/state/HALT` — the ONE directory this workspace writes program-owned state
//!    into. **Which project that is comes from the composition ROOT** ([`HaltProject::Declared`],
//!    fed by [`declare_project_state_dir`] from `vike_boot::Booted::state_dir`), so a deployment
//!    that names its settings directory with `$VIKE_SETTINGS_DIR` moves the sentinel with it. A
//!    process whose root declares nothing falls back to [`HaltProject::WalkFrom`] —
//!    [`vike_model::paths::state_path::project_state_dir`] walking up from the working directory, which
//!    does NOT honour that override.
//! 3. `<exe_dir>/HALT` — the LAST RESORT, reached only when no project resolves at all, and REPORTED
//!    as such ([`HaltRung::ExeDir`], [`EXE_DIR_FALLBACK_ADVISORY`]).
//!
//! No environment variable names the sentinel (decision 0099): its path is a fact about the
//! PROJECT, which the boot has already resolved, and a path in a variable could name a file nothing
//! watches. ⚠ There is no rung 1, and the numbers above are never changed: the `rung` field of the
//! mount-time report is a log contract an operator greps.
//!
//! ## The path reaches a client as DATA
//!
//! A venue bridge reads no process-global state (decision 0096, contract rule 1), and this module's
//! memoized resolver is process-global state. So the resolver is called by the COMPOSITION layer —
//! `vike-mount`, once per mount, into `vike_bridge_core::venue_mount::ProcessFacts::halt_path` — and
//! each bridge's mount hands the path to its client with the client's `with_halt_path` builder
//! (`ExecActor::with_halt_path` for the shared actor; cTrader's, dukascopy's
//! and hyperliquid's own clients have one each). A client's submit seam then asks
//! [`sentinel_engaged`] about the path it was given. `crates/vike-ops/tests/settings_secrets/bridge_inputs_gate.rs`
//! holds both halves: no bridge production source names [`halt_path_from_env`], and every bridge
//! `mount.rs` that builds a live exec client reads `process.halt_path` and calls `.with_halt_path(`.
//!
//! ⚠ **A client that was never handed a path watches NOTHING**, and says so once at `error`
//! ([`sentinel_engaged`]'s unwired arm). A handed path can be forgotten where a process-wide
//! default could not, which is what the gate above and the loud unwired line are for. An unwired
//! client is reachable only from a test or a tool: every mount is covered by the gate.
//!
//! ## Why the project's STATE directory, and never beside the binary
//!
//! Under `deploy/vike-tradehub.service`'s `ProtectSystem=strict` the exe directory is mounted
//! READ-ONLY, so `touch <project>/bin/HALT` returns `EROFS`: a sentinel there is a kill switch that
//! cannot be armed — and a kill switch that is not armed looks exactly like one that is, right up
//! until it is needed. The state directory is the one directory a deployment must already have,
//! that the sandbox already grants (`ReadWritePaths=<project>/settings/state`), and that
//! `cargo clean` does not delete on a dev box. It is deliberately NOT the settings directory itself
//! — a daemon that can rewrite its own `policy` rows (the settings database lives there) has no
//! ceiling.
//!
//! ## ⚠ …and WHICH project rung 2 means is the ROOT's answer, not a second walk of our own
//!
//! The `_from`-less [`vike_model::paths::state_path::project_state_dir`] is `$VIKE_SETTINGS_DIR`-BLIND:
//! walking with it lets the WORKING DIRECTORY decide the kill switch in the one configuration that
//! variable exists to make irrelevant. On the CI box the variable and `WorkingDirectory=` name the same
//! project, so a blind walk lands correctly there by the two agreeing, not by the override being
//! honoured — it bites the first time they disagree.
//!
//! The cure is NOT an environment read here. `crates/vike-ops/tests/settings_secrets/settings_registry.rs`'s
//! `LIBRARY_PIN` is a ratchet that may shrink and never grow, and a library resolving global state
//! its caller can neither see nor override is the defect that ratchet exists to stop. So the answer
//! arrives as a PARAMETER: [`declare_project_state_dir`] takes `vike_boot::Booted::state_dir`, the
//! ONE walk the composition root already performed (`crates/vike-boot/src/lib.rs` is why there is
//! only one), and [`halt_path_from_env`] prefers it over walking. A root that declares nothing gets
//! [`HaltProject::WalkFrom`]; the roots that boot (`vike-tradehub`, `vike-desktop`) both declare, so
//! what lands there is this crate's own tests, or a new root that skips the boot.
//!
//! ## ⚠ Rung 3 is REPORTED as rung 3, because a container has no sandbox to sound the alarm
//!
//! On a systemd box rung 3 is loud by accident of the SANDBOX: the read-only exe mount makes
//! [`halt_path_arming_error`] return `EROFS`, and the resolution comes out as `tracing::error!`.
//! In an OCI container the image filesystem is writable by default and the binary lives in the
//! IMAGE while `<project>` is a BIND MOUNT, so `<exe_dir>/HALT` is creatable: the probe answers
//! "armable" and the report would read exactly like a correct rung-2 resolution. Docker's default
//! working directory is `/`, which is precisely the "no project resolves" case — so an operator
//! doing `touch <project>/settings/state/HALT` on the HOST arms a file this process never looks at.
//!
//! So the RUNG is part of the report, and an armable rung 3 is a `warn!` carrying
//! [`EXE_DIR_FALLBACK_ADVISORY`] rather than an `info!`. [`HaltRung`] is decided in the SAME branch
//! that decides the path ([`resolve_halt_path_rung`], which [`resolve_halt_path`] is a projection
//! of), so the reported rung cannot drift from the resolved file — a second `match` restating the
//! precedence beside the first is how a report comes to describe a rung the resolver did not take.
//!
//! ⚠ **Rung 3 cannot leak a LIVE order, and the bound is structural.** `vike_boot::boot` derives
//! `Booted::state_dir` as `settings_dir.join("state")`, so rung 3 is reached exactly when NO
//! settings directory resolved — and the credential store is the settings database inside that
//! directory, reached by the same pure resolver over the same override and the same working
//! directory (`vike_secrets::resolve_project`, pinned equal to
//! `vike_model::paths::state_path::project_settings_dir_from` by `tests/settings_dir_spellings.rs`).
//! No settings directory therefore means no store, an EMPTY credential map, and every venue on
//! paper by the live gate. What rung 3 costs is the REHEARSAL: `docs/ops/kill-switches.md` gap 4 is
//! the argument that a paper node is where an operator learns what `touch` does, and rehearsing
//! against a sentinel nobody watches teaches that the switch works when it does not.
//!
//! ## …and the resolution is REPORTED, because an unarmable switch is otherwise invisible
//!
//! Nothing here writes the sentinel: an operator does, with `touch`. So "the switch is broken" is a
//! fact about a `touch` that has not happened yet, discovered mid-incident. [`halt_path_from_env`]
//! therefore probes the resolved path ONCE per process and logs it —
//! `tracing::error!` when an operator could not create it, `info` when they could — so the failure
//! lands in the trace file at MOUNT time (`vike-mount` asks for the path when it hands a mount its
//! `ProcessFacts`, before the first venue's client exists) rather than at the worst possible
//! moment. [`halt_path_arming_error`] is that probe, pure enough to unit-test.
//!
//! ## What HALT blocks (and what it deliberately does not)
//! - **Submit that OPENS or ADDS: BLOCKED.** The order is refused and a terminal `OrderRejected`
//!   (`reason = HALT_REJECT_REASON`) is synthesized through the normal event path, so the
//!   intent never silently vanishes (the CLAUDE.md venue-adapter contract: "a dead venue path must
//!   synthesize a terminal `OrderRejected`").
//! - **Submit that REDUCES: ALLOWED** — see `halt_admits_submit`, which is the whole of that rule,
//!   and is one shared function precisely so the clients that enforce this sentinel cannot drift
//!   into disagreeing about what a halt lets out: `ExecActor` (every venue on the shared
//!   actor), hyperliquid's bespoke client, and `vike_paper::PaperExecutionClient` (the paper MOUNT —
//!   see the note on that re-export below for why the predicate lives in vike-exec).
//!
//!   ⚠ **cTrader EXTENDS this predicate rather than replacing it.**
//!   `crates/bridges/ctrader/src/exec.rs`'s `halt_admits_this_submit` is `closes || <this>`: it
//!   admits what that venue's own POSITION BOOK proves closes, because on a hedging account a plain
//!   opposite-side order flattens with no flag, and the flag-only rule here alone would refuse that
//!   exit. It is `||` because a book is evidence FOR a reduce and never against one: cTrader's book
//!   holds nothing until the venue ANSWERS a best-effort reconcile (at connect and at every
//!   reconnect), so a restarted daemon can have heard nothing about positions the account already
//!   holds, and a verified-ONLY rule refuses exactly those exits. The rule for a future client: call
//!   `halt_admits_submit`; if you also hold a position book, admit what it PROVES on top, never
//!   instead, and argue it in your own module doc as that one does.
//! - **Cancel: ALWAYS ALLOWED.** Cancels pass straight through, halted or not.
//! - **Modify: BLOCKED (conservative), on every client including the position-verified one.** A
//!   modify can add size or chase price, and "reduce-only" cannot be reliably inferred from the
//!   request alone (`order.qty` may be original or remaining) — nor from a position book, which
//!   answers a question about ORDERS and not about amendments to resting ones. The exit path under
//!   halt is a cancel or a reducing submit, never a modify, so blocking modify never traps a
//!   position. A blocked modify leaves the resting order at its current terms — identical to the
//!   modify dead-channel contract — so nothing vanishes. Every enforcing client also SAYS SO, with a
//!   non-terminal `OrderModifyRejected` carrying `HALT_REJECT_REASON` — one wording across all of
//!   them, so a single recogniser matches any of them, and because a silent refusal is
//!   byte-indistinguishable from a modify the adapter LOST.
//!
//! ## ⚠ A CANCEL IS NOT AN EXIT — why the reducing-submit arm exists
//!
//! A cancel removes a resting ORDER, whereas getting out of a POSITION requires SENDING one.
//! `OrderIntent::Flatten` mints a `reduce_only` MARKET order for `|position|`; a sentinel that
//! refused it like any other submit would leave `market-exit` under a HALT file halted WITH the
//! position still open, and the only escape — `rm` the sentinel, flatten, re-`touch` it — un-halts
//! every OTHER order in that window, from a phone, mid-incident.
//!
//! ## ⚠ THIS BOUNDARY TRUSTS THE FLAG. THE RISK GATE DOES NOT. Both facts matter
//!
//! `vike_exec::RiskGate` gates the same idea under `TradingState::Halted` and admits only a
//! POSITION-VERIFIED reduce (`vike_model::is_covered_reduce`: the order must OPPOSE the position and
//! be COVERED by it). It can do that because it holds the position book. **`ExecActor` does not** —
//! it owns a command channel, an event lane and this sentinel path, and nothing that could say how
//! big any position is. So at this boundary the caller-asserted `reduce_only` flag is the only
//! evidence in existence, and this arm takes it.
//!
//! ⚠ **The `policy.halt_admit` knob does NOT change that here, and cannot.** `verify` asks a
//! client to weigh its own position book, and this boundary has none — so `ExecActor` calls
//! `halt_admits_submit` (the `Admit`/no-book case) unconditionally, `vike_model::halt_verify_support`
//! carries the written per-venue reason, and `vike_mount::make_engine` REPORTS the degrade at mount
//! so an operator who set `verify` learns it there rather than mid-incident. The full rule, the
//! three-state `PositionEvidence` it weighs, and the single authority for "what `verify` refuses"
//! all live on `crates/vike-exec/src/halt.rs` beside the predicate — deliberately not restated here.
//!
//! The two mechanisms therefore agree on POLICY (a halt stops opening risk and never traps you) and
//! differ in VERIFICATION STRENGTH. That difference is not free: a strategy bug that tags its
//! entries `reduce_only` can open risk through a HALT file where the `RiskGate` would refuse it.
//! Three things bound it, and it matters which are load-bearing:
//!
//! * every submit reaching this boundary was minted by our own core and has ALREADY passed that
//!   `RiskGate` — this is a second gate BEHIND the verifying one, not the only gate;
//! * `RiskGate`'s own `Reducing` state trusts the same bare flag (`RiskGate::reduces`), so this is
//!   an already-accepted residual rather than a new class of one;
//! * real perp venues (binance/bybit) reject a `reduce_only` order that would increase a position —
//!   but ONLY where a position exists to check it against, so a FLAT-BOOK mis-tag is caught by
//!   nothing, anywhere. That is the honest residual, and it is why this admission is LOGGED on
//!   every occurrence instead of passing silently: an operator who sees orders leaving under a HALT
//!   has to be able to find out why.
//!
//! ## Cost / where it runs
//! The check runs ONLY at the submit/modify boundary (submits are infrequent), so a plain
//! `Path::exists` per submit is acceptable. It is NEVER on any per-message hot path (the vike-core
//! fold loop or the WS pump) — those must stay allocation/syscall-free for the `p99 < 10µs` gate.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};

/// The DECISION — `halt_admits_submit`, its policy-taking full form `halt_admits_submit_under` with
/// the `PositionEvidence` that form weighs, and the `HALT_REJECT_REASON` it rejects with —
/// re-exported so `vike_bridge_core::halt::…` names it for every adapter, test and doc citation.
///
/// ⚠ **The rule itself lives in `crates/vike-exec/src/halt.rs`, and that is load-bearing.** The
/// paper exchange (`vike_paper::PaperExecutionClient`) must refuse an opening order under a HALT
/// file for the same reason a live adapter does, but it may not depend on THIS crate (which owns
/// the ureq/tungstenite/rustls transport stack), so the predicate sits in the crate all three
/// enforcing clients share. The FILE half — the path precedence, the armability probe and the
/// once-per-process report below — is a process-state read and a `tracing` report, which belong at
/// this layer and not in vike-exec.
///
/// ⚠ GATED ON `full`, because `vike-exec` is an OPTIONAL dependency that `full` turns on. Without
/// the gate this line names a crate that is not linked, and `cargo build -p vike-cli` (the consumer
/// taking `vike-bridge-core` with `default-features = false`) fails with `E0433`. The FILE half
/// below stays ungated: it names nothing from vike-exec.
#[cfg(feature = "full")]
pub use vike_exec::halt::{
    HALT_REJECT_REASON, PositionEvidence, halt_admits_submit, halt_admits_submit_under,
};

/// The sentinel's FILE NAME, spelled once. Every rung of the precedence below joins the same name
/// onto a different directory, so an operator's `touch` target differs only in where it lives.
pub const HALT_FILE: &str = "HALT";

/// What [`halt_path_from_env`] says when the sentinel fell all the way to [`HaltRung::ExeDir`] AND
/// that path is creatable — the container shape, where no other check in this module objects.
///
/// A NAMED const rather than an inline string because it is the anchor a gate keys on
/// (`docs/ops/kill-switches.md` sends operators to grep it): deleting the line is what must go red,
/// and the wording can be improved without breaking anything that points here.
///
/// It names the rung-2 path the operator was ABOUT to `touch`, because that is the action this line
/// exists to stop.
pub const EXE_DIR_FALLBACK_ADVISORY: &str = "HALT sentinel fell back to the EXE DIRECTORY: no project resolved above this process's working \
     directory, so the kill switch is the path above — beside the BINARY. `touch \
     <project>/settings/state/HALT` will NOT be seen by this process. In a container the binary is \
     in the IMAGE and <project> is a bind MOUNT, and the working directory defaults to `/`, so this \
     is the expected shape there and not a rare one; the exe directory is also writable there, \
     which is why nothing else in this report objects. Point VIKE_SETTINGS_DIR at the project's \
     settings directory (the daemon declares its state directory from it), or give the process a \
     working directory inside the project (docs/ops/kill-switches.md).";

/// Resolve the HALT sentinel path from already-gathered inputs: `<state_dir>/HALT`, else
/// `<exe_dir>/HALT`.
///
/// PURE — no environment read, no filesystem probe — so the precedence is unit-testable and so the
/// answer cannot silently change with a directory appearing or disappearing. Whether the resolved
/// path is USABLE is a separate question, deliberately: [`halt_path_arming_error`] answers it and
/// the caller reports it, rather than this quietly picking a different rung and leaving an operator
/// touching the path they were told about last week.
pub fn resolve_halt_path(state_dir: Option<&Path>, exe_dir: &Path) -> PathBuf {
    resolve_halt_path_rung(state_dir, exe_dir).0
}

/// WHICH RUNG of the precedence answered — what separates "the sentinel is in your project" from
/// "the sentinel is inside the image" (see the module doc's rung-3 section). A rung-3 resolution is
/// INDISTINGUISHABLE from a correct rung-2 one in a log line that carries only the path, unless the
/// reader already knows where this binary's exe directory is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HaltRung {
    // There is no rung 1 (decision 0099), and the numbers are not reused: `as_str` is a log
    // contract.
    /// Rung 2 — `<project>/settings/state/HALT`, the deployed shape and the built-in default.
    ProjectState,
    /// Rung 3 — `<exe_dir>/HALT`, the LAST RESORT: no project resolved at all.
    ExeDir,
}

impl HaltRung {
    /// The `rung` field's value in the mount-time report — a stable token an operator can grep,
    /// which is why it is spelled here rather than through `Debug` (a derive nobody promises to
    /// keep stable is not a log contract).
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ProjectState => "2: <project>/settings/state",
            Self::ExeDir => "3: <exe_dir> (LAST RESORT — no project resolved)",
        }
    }
}

/// [`resolve_halt_path`] and the [`HaltRung`] that produced it, from ONE branch.
///
/// **This is the primitive and `resolve_halt_path` is the projection**, deliberately in that
/// direction: the report has to name the rung the resolver actually took. There is one
/// `if`/`match` in this module and both callers read it.
///
/// PURE, for the same reasons [`resolve_halt_path`] is — no environment read, no filesystem probe.
pub fn resolve_halt_path_rung(state_dir: Option<&Path>, exe_dir: &Path) -> (PathBuf, HaltRung) {
    match state_dir {
        Some(dir) => (dir.join(HALT_FILE), HaltRung::ProjectState),
        None => (exe_dir.join(HALT_FILE), HaltRung::ExeDir),
    }
}

/// Where rung 2's `<project>` comes from — the one input [`halt_path_for`] cannot decide for
/// itself.
///
/// An enum rather than an `Option<&Path>` because "this process has NO project" and "nobody told me
/// which project" are different facts with different answers, and collapsing them is how a root
/// that resolved no project would silently get a fresh walk instead of rung 3.
#[derive(Debug, Clone, Copy)]
pub enum HaltProject<'a> {
    /// **The composition root's ONE boot walk already answered** — `vike_boot::Booted::state_dir`,
    /// handed over by [`declare_project_state_dir`]. That walk honours `$VIKE_SETTINGS_DIR`, which
    /// is the whole reason this arm exists.
    ///
    /// `None` INSIDE the arm is a real answer (no project above the root's working directory) and
    /// resolves rung 3 — never a licence to walk again.
    Declared(Option<&'a Path>),
    /// **Nobody declared**, so walk up from this working directory for a project marker.
    ///
    /// ⚠ `$VIKE_SETTINGS_DIR`-BLIND: [`vike_model::paths::state_path::project_state_dir`] is the
    /// `_from`-less resolver, so under that override this answers with whatever the WORKING
    /// DIRECTORY happens to sit above. It is the right answer for a process with no composition
    /// root to ask (this crate's own tests) — not a shape a booting root should be in.
    ///
    /// ⚠ No gate keeps a root out of it: a new root that builds an `ExecutionClient` without
    /// booting lands here silently, and nothing outranks this rung for it.
    WalkFrom(&'a Path),
}

/// [`resolve_halt_path`] with rung 2's project resolution done for you.
///
/// Split out of [`halt_path_from_env`] so a test can drive the WHOLE resolution — the walk
/// included — over a synthetic deployment tree without touching the process environment or the
/// process's own working directory (this workspace does not `set_var` under threads).
/// `crates/vike-bridge-core/tests/halt_default_path.rs` is that test.
///
/// PURE under [`HaltProject::Declared`]; under [`HaltProject::WalkFrom`] it probes the filesystem
/// for a project marker, which is what makes the arm worth naming at every call site.
pub fn halt_path_for(project: HaltProject<'_>, exe_dir: &Path) -> PathBuf {
    halt_path_for_rung(project, exe_dir).0
}

/// [`halt_path_for`] and the [`HaltRung`] that produced it — the projection rule of
/// [`resolve_halt_path_rung`], one layer up, and for the same reason: [`halt_path_from_env`] reports
/// the rung and may not decide it a second time.
pub fn halt_path_for_rung(project: HaltProject<'_>, exe_dir: &Path) -> (PathBuf, HaltRung) {
    let state_dir = match project {
        HaltProject::Declared(dir) => dir.map(Path::to_path_buf),
        HaltProject::WalkFrom(cwd) => vike_model::paths::state_path::project_state_dir(cwd),
    };
    resolve_halt_path_rung(state_dir.as_deref(), exe_dir)
}

/// WHICH REPORT [`halt_path_from_env`] emits — the severity decision, lifted out so it can be
/// unit-tested without a `tracing` subscriber.
///
/// ⚠ It is the seam the REAL code calls, not a description of it beside it: a pure classifier that
/// nothing consulted would pin only itself, and the branch it described could be reworded into
/// disagreeing with it while every test stayed green.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HaltReport {
    /// The sentinel is where a deployment means it to be and an operator can create it.
    Resolved,
    /// [`HaltRung::ExeDir`] AND creatable — the container shape, where no other check objects, and
    /// which would otherwise read exactly like [`Self::Resolved`].
    ExeDirFallback,
    /// The resolved path cannot be created, whatever rung produced it. The strongest fact, so it
    /// outranks the rung.
    NotArmable,
}

/// The report severity for a resolved sentinel: its [`halt_path_arming_error`] and its [`HaltRung`].
///
/// The ORDER is the content. An unarmable path outranks the rung because it is the stronger
/// statement and because it is how rung 3 announces itself on a systemd box (a read-only exe
/// mount); the rung-3 arm then catches what that reading misses, a rung 3 the process CAN write —
/// see the module doc.
#[must_use]
pub fn halt_report(arming_error: Option<&str>, rung: HaltRung) -> HaltReport {
    match (arming_error, rung) {
        (Some(_), _) => HaltReport::NotArmable,
        (None, HaltRung::ExeDir) => HaltReport::ExeDirFallback,
        (None, _) => HaltReport::Resolved,
    }
}

/// The process's ONE resolved sentinel path — see [`halt_path_from_env`], which owns it.
///
/// At module scope rather than inside that function only so [`declare_project_state_dir`] can tell
/// an operator that their declaration arrived too late to change anything.
static RESOLVED: OnceLock<PathBuf> = OnceLock::new();

/// `<project>/settings/state` as the composition root's boot resolved it, when a root supplied one.
///
/// `Some(None)` — declared, and the root found no project — is a real answer distinct from an unset
/// cell; see [`HaltProject`].
static DECLARED_STATE_DIR: OnceLock<Option<PathBuf>> = OnceLock::new();

/// **Hand the sentinel the project the composition root already resolved.** Call it once, from a
/// `main`, with `vike_boot::Booted::state_dir`.
///
/// This is how `$VIKE_SETTINGS_DIR` reaches rung 2 — as a PARAMETER, never an `env::var` here (see
/// the module doc).
///
/// # It must run before the first venue mounts
///
/// [`halt_path_from_env`] memoizes, so the FIRST caller fixes the answer for the process
/// (deliberately — a kill switch whose path could differ between two submits is not a kill switch).
/// A declaration after that point cannot change it, and returning `Ok` would tell an operator their
/// override took effect when it did not. So the failures are REPORTED rather than swallowed:
///
/// * `Err` naming the already-resolved path — a FIRST declaration that arrived too late, so the
///   sentinel is whatever the earlier resolution decided;
/// * `Err` naming both directories — a second, DIFFERENT declaration, which one process may not
///   have (the first one wins, as the memoized path already would).
///
/// A repeat declaration of the SAME directory is `Ok`, so a root that calls this from two arms of a
/// startup branch is not punished for it. The caller logs the error; this module cannot, usefully —
/// a root calls it around `vike_log::init` and the message has to reach whatever is listening.
///
/// ⚠ The too-late check is an ORDERING diagnostic, not a lock. Roots call this from `main` before
/// any mount thread exists, so the interleaving it would have to lose to cannot occur there; a
/// process that genuinely resolved the sentinel on another thread first gets the `Err` and the
/// earlier path, which is the honest outcome either way.
pub fn declare_project_state_dir(state_dir: Option<PathBuf>) -> Result<(), String> {
    let mut first = false;
    let declared = DECLARED_STATE_DIR.get_or_init(|| {
        first = true;
        state_dir.clone()
    });
    if declared.as_deref() != state_dir.as_deref() {
        return Err(format!(
            "the HALT sentinel's project was already declared as {}; the second declaration ({}) \
             is IGNORED — one process has one sentinel",
            state_dir_label(declared.as_deref()),
            state_dir_label(state_dir.as_deref())
        ));
    }
    // Only a declaration that WOULD have changed something can be late. Repeating the one already
    // in force after the path resolved is not a fault and must not be reported as one.
    let too_late = if first { RESOLVED.get() } else { None };
    if let Some(already) = too_late {
        return Err(format!(
            "the HALT sentinel path was already resolved as {} before this declaration, so the \
             declaration changed nothing — it has to run before the first venue mount \
             (docs/ops/kill-switches.md)",
            already.display()
        ));
    }
    Ok(())
}

/// `<project>/settings/state` exactly as the composition root's boot resolved it, or `None` when no
/// root has declared one (a test, a tool, any binary that never called
/// [`declare_project_state_dir`]).
///
/// # This is a READ of the root's declaration, not a resolution
///
/// It exists so a library that needs a project-relative path can DERIVE it from the one walk that
/// already decided, instead of walking again — the root `CLAUDE.md`'s rule, which matters because
/// the `_from`-less resolvers are `$VIKE_SETTINGS_DIR`-blind while `deploy/vike-tradehub.service`
/// and most sibling units set that variable. A second walk would answer for whatever directory the
/// process happens to sit above.
///
/// The cell lives here because the HALT sentinel was its first consumer, but the VALUE is not
/// halt-specific — it is the project. `vike-mount` reads it for each contract
/// mount (`crates/vike-mount/src/contract.rs`'s `process_facts`) and hands it to the bridge as
/// `MountInputs::process.state_dir`; cTrader's mount derives the settings directory from it
/// (`vike_ctrader::token_store::settings_dir_beside_state_dir`) so a rotated OAuth grant is written
/// back to the store the ROOT loaded, never to one a bridge crate guessed at.
///
/// ⚠ Reading this does NOT resolve the sentinel path, so it cannot make a later
/// [`declare_project_state_dir`] "too late" — only [`halt_path_from_env`] memoizes.
#[must_use]
pub fn declared_project_state_dir() -> Option<PathBuf> {
    DECLARED_STATE_DIR.get().cloned().flatten()
}

/// A declared state directory, for an operator-facing message. `None` is a real answer and has to
/// read as one rather than as an empty string.
fn state_dir_label(dir: Option<&Path>) -> String {
    match dir {
        Some(p) => p.display().to_string(),
        None => "<no project above the working directory>".to_string(),
    }
}

/// Why an operator's `touch` on this path would FAIL, or `None` when it would succeed.
///
/// The check is an actual create-and-remove, not a permission-bit inspection: the failure this
/// exists to catch is `EROFS` from `ProtectSystem=strict`, and a read-only MOUNT leaves the mode
/// bits of the directory completely unchanged — `Permissions::readonly()` answers `false` on the
/// very directory a write cannot reach. Only a write finds out.
///
/// It probes with a **uniquely-named sibling**, never with the sentinel itself. Creating and then
/// removing `HALT` would race an operator's `touch` and could delete a halt that had just been
/// engaged — un-halting a live node to answer a diagnostic question, which is not a trade this
/// module is willing to make. An already-existing sentinel is reported armable without touching it.
pub fn halt_path_arming_error(path: &Path) -> Option<String> {
    if path.as_os_str().is_empty() {
        return Some("the resolved sentinel path is empty".to_string());
    }
    if path.exists() {
        // Already there: the switch is not merely armable, it is ARMED. Nothing to probe, and
        // nothing here may touch it.
        return None;
    }
    let parent = match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
        // A bare relative file name ("HALT"): its directory is the working directory.
        _ => PathBuf::from("."),
    };
    if !parent.is_dir() {
        return Some(format!(
            "{} is not a directory, so `touch {}` would fail with ENOENT — nothing creates it \
             lazily",
            parent.display(),
            path.display()
        ));
    }
    let probe = parent.join(format!(".vike-halt-probe-{}", std::process::id()));
    match std::fs::File::create(&probe) {
        Ok(_) => {
            // Best-effort cleanup of our own probe file; a leftover is harmless (pid-named).
            let _ = std::fs::remove_file(&probe);
            None
        }
        Err(e) => Some(format!("{} is not writable: {e}", parent.display())),
    }
}

/// The process's ONE resolved HALT sentinel path — the project state directory, else the exe
/// directory (see the module doc). No environment variable names it: the "env" in this function's
/// name is the PROCESS's environment in the loose sense — its working directory and its executable.
///
/// **Called by the COMPOSITION layer, never by a venue bridge** (decision 0099):
/// `vike_mount::contract::mount_process_facts` resolves it once into `ProcessFacts::halt_path`, which
/// every bridge's mount receives as data, and the paper books, the daemon's control surface and its
/// startup advisory read the same answer. `crates/vike-ops/tests/settings_secrets/bridge_inputs_gate.rs` fails a
/// bridge that calls it.
///
/// **Resolved ONCE and memoized**: the walk underneath it does filesystem probes, which do not
/// belong on a repeated check, and a kill switch whose path could differ between two submits is not
/// a kill switch — an operator resolves it once, over ssh, and relies on it for the process's life.
///
/// The first call also REPORTS the resolution (see the module doc), as [`halt_report`] classifies
/// it: `error` naming the reason when [`halt_path_arming_error`] says an operator could not create
/// it, `warn` with [`EXE_DIR_FALLBACK_ADVISORY`] for an armable rung 3, else `info` with the path.
/// `vike-mount` asks for the path before it builds any client, so the report lands when the first
/// venue MOUNTS rather than on the first order.
///
/// Every report carries a `project` field naming WHERE rung 2's project came from, because the two
/// sources answer differently under `$VIKE_SETTINGS_DIR`: a root that forgot
/// [`declare_project_state_dir`] resolves a perfectly valid-looking path off the working directory.
pub fn halt_path_from_env() -> PathBuf {
    RESOLVED
        .get_or_init(|| {
            let exe_dir = std::env::current_exe()
                .ok()
                .and_then(|p| p.parent().map(Path::to_path_buf))
                .unwrap_or_else(|| PathBuf::from("."));
            let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
            let declared = DECLARED_STATE_DIR.get();
            let project = match declared {
                Some(dir) => HaltProject::Declared(dir.as_deref()),
                None => HaltProject::WalkFrom(&cwd),
            };
            let source = match declared {
                Some(_) => "declared by the composition root's boot (honours VIKE_SETTINGS_DIR)",
                None => "walked up from the working directory (VIKE_SETTINGS_DIR-blind)",
            };
            let (path, rung) = halt_path_for_rung(project, &exe_dir);
            let arming = halt_path_arming_error(&path);
            match halt_report(arming.as_deref(), rung) {
                // An UNARMABLE path outranks the rung: it is the stronger fact, and on the shipped
                // units it is how rung 3 announces itself (a read-only exe mount).
                HaltReport::NotArmable => tracing::error!(
                    target: "vike_bridge_core::halt",
                    path = %path.display(),
                    project = source,
                    rung = rung.as_str(),
                    // `halt_report` returns this arm only when the probe said `Some`, so the
                    // fallback is unreachable — spelled rather than `unwrap`ed because a kill
                    // switch's own report may not be the thing that panics.
                    reason = arming.as_deref().unwrap_or("unknown"),
                    "HALT KILL SWITCH IS NOT ARMABLE: this path cannot be created, so the \
                     out-of-band kill switch cannot be engaged. Create the directory and make it \
                     writable to the daemon's user — under a systemd unit that means its \
                     ReadWritePaths= must name it (docs/ops/kill-switches.md)"
                ),
                // ARMABLE, and the last resort — the container shape, where the probe above is
                // silent. `warn!` because the path is inside the IMAGE while the operator's `touch`
                // lands on the MOUNT, and nothing else in the log distinguishes that from rung 2.
                HaltReport::ExeDirFallback => tracing::warn!(
                    target: "vike_bridge_core::halt",
                    path = %path.display(),
                    project = source,
                    rung = rung.as_str(),
                    "{EXE_DIR_FALLBACK_ADVISORY}"
                ),
                HaltReport::Resolved => tracing::info!(
                    target: "vike_bridge_core::halt",
                    path = %path.display(),
                    project = source,
                    rung = rung.as_str(),
                    "HALT sentinel path resolved; `touch` it to stop new orders"
                ),
            }
            path
        })
        .clone()
}

/// Whether the sentinel a client was handed is engaged right now — the ONE `exists()` every
/// enforcing client's submit seam asks (the shared `ExecActor`, cTrader's,
/// dukascopy's and hyperliquid's own clients), so the four cannot drift apart about what an
/// absent path means.
///
/// `Some(path)` is the ordinary case: the client's mount handed it `ProcessFacts::halt_path`, and
/// the answer is whether that file exists, re-read on every call so `rm` resumes trading and
/// nothing latches.
///
/// **`None` — a client that was never handed a path — watches NOTHING, and says so.** That is the
/// shape of a test double or a tool, and also of a mount that forgot to wire the sentinel: a dead
/// kill switch reading exactly like an armed one, which is why the arm logs ONCE per process at
/// `error`. The structural guard is the gate
/// (`crates/vike-ops/tests/settings_secrets/bridge_inputs_gate.rs`'s
/// `every_live_mount_hands_its_client_the_halt_path`); this line is the backstop for what a text
/// gate cannot see. An EMPTY path is the same state — `ProcessFacts::default()` carries one —
/// because `Path::new("").exists()` is false for a reason that has nothing to do with the switch.
///
/// Takes everything it uses as an argument, apart from the once-per-process log.
#[must_use]
pub fn sentinel_engaged(path: Option<&Path>) -> bool {
    match path {
        Some(p) if !p.as_os_str().is_empty() => p.exists(),
        _ => {
            static UNWIRED_REPORTED: AtomicBool = AtomicBool::new(false);
            if !UNWIRED_REPORTED.swap(true, Ordering::Relaxed) {
                tracing::error!(
                    target: "vike_bridge_core::halt",
                    "HALT KILL SWITCH IS NOT WIRED for an exec client: it was built without a \
                     sentinel path, so it watches no file and `touch <project>/settings/state/HALT` \
                     will not stop it. Every live mount must hand its client \
                     `ProcessFacts::halt_path` (docs/ops/kill-switches.md)"
                );
            }
            false
        }
    }
}

#[path = "halt_tests.rs"]
#[cfg(test)]
mod halt_tests;
