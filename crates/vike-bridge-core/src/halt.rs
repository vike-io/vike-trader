//! HALT file sentinel — an operator kill-switch that stops NEW order placement (no Python twin; a
//! Rust-native operational safeguard).
//!
//! Existence of a sentinel file on disk halts OPENING/ADDING orders at the [`ExecActor`] submit
//! boundary (see `exec_actor.rs`). It is designed to work when the GUI/runtime is otherwise wedged:
//! an operator does `touch <HALT file>` over ssh on the headless box and the very NEXT submit is
//! refused — no IPC, no running event loop, no responsive UI required; just a filesystem `exists()`
//! check at the submit seam.
//!
//! ## Path precedence (the shape of [`vike_log::resolve_log_dir`]: an env override, a default, a
//! ## last resort)
//!
//! 1. `VIKE_HALT_FILE`, when set to a non-blank value. Names the sentinel outright, so the answer
//!    never depends on a working directory — `deploy/vike-tradehub.service` sets it for exactly
//!    that reason. The other shipped units deliberately do NOT: `deploy/vike-recorder.service` and
//!    `deploy/vike-datahub.service` construct no `ExecutionClient` and so have no submit boundary
//!    to read it at, and `deploy/vike-tradehub-project.service` omits it to exercise rung 2.
//! 2. `<project>/settings/state/HALT` — the ONE directory this workspace writes program-owned state
//!    into. **Which project that is comes from the composition ROOT** ([`HaltProject::Declared`],
//!    fed by [`declare_project_state_dir`] from `vike_boot::Booted::state_dir`), so a deployment
//!    that names its settings directory with `$VIKE_SETTINGS_DIR` moves the sentinel with it. A
//!    process whose root declares nothing falls back to [`HaltProject::WalkFrom`] —
//!    [`vike_model::state_path::project_state_dir`] walking up from the working directory, which
//!    does NOT honour that override.
//! 3. `<exe_dir>/HALT` — the LAST RESORT, reached only when no project resolves at all. It is
//!    REPORTED as such ([`HaltRung::ExeDir`], [`EXE_DIR_FALLBACK_ADVISORY`]), for the reason in the
//!    next section.
//!
//! ⚠ **(2) and (3) used to be one rung, and it was the wrong one.** `<exe_dir>/HALT` was the
//! built-in default, and under `deploy/vike-tradehub.service`'s `ProtectSystem=strict` the exe
//! directory is mounted READ-ONLY — so `touch <project>/bin/HALT` returns `EROFS` and the only kill
//! switch that survives a wedged runtime cannot be armed at all. #1143 moved the DEPLOYED path onto
//! the units' `Environment=VIKE_HALT_FILE=` line, which fixed the boxes it was installed on and left
//! the DEFAULT still pointing at the read-only directory. A unit that forgets that line therefore
//! had no kill switch and no error anywhere — and a kill switch that is not armed looks exactly like
//! one that is, right up until it is needed.
//!
//! The state directory is the right anchor for the same reason vike-log chose it for the rolling
//! trace file: it is the one directory a deployment must already have, that the sandbox already
//! grants (`ReadWritePaths=<project>/settings/state`), and that `cargo clean` does not delete out
//! from under a dev box. It is deliberately NOT the settings directory itself — a daemon that can
//! rewrite its own `policy.toml` has no ceiling — and `state/` is already carved out for exactly
//! this class of file.
//!
//! ## ⚠ …and WHICH project rung 2 means is the ROOT's answer, not a second walk of our own
//!
//! Rung 2 used to walk for itself, unconditionally, through the `_from`-less
//! [`vike_model::state_path::project_state_dir`] — which is `$VIKE_SETTINGS_DIR`-BLIND. So an
//! operator who named the settings directory outright still had the KILL SWITCH's default decided
//! by the working directory, in the one configuration that variable exists to make irrelevant
//! (`deploy/vike-tradehub.service`'s own comment: it "makes the answer EXPLICIT and independent of
//! `WorkingDirectory=`").
//!
//! **Severity, measured rather than asserted: this was LATENT, not live.** On the CI box the tradehub
//! sets `$VIKE_SETTINGS_DIR` to `<project>/settings` and `WorkingDirectory=` to `<project>`, so the
//! blind walk landed on the same directory the override named and the sentinel resolved correctly —
//! by the two agreeing, not by the override being honoured. It bites the first time they disagree,
//! which is exactly the case somebody sets the variable for.
//!
//! The cure is NOT a second environment read here. `crates/vike-ops/tests/settings_registry.rs`'s
//! `LIBRARY_PIN` is a ratchet that may shrink and never grow, and a library resolving global state
//! its caller can neither see nor override is the defect that ratchet exists to stop — this module
//! would be committing it twice over. So the answer arrives as a PARAMETER:
//! [`declare_project_state_dir`] takes `vike_boot::Booted::state_dir`, the ONE walk the composition
//! root already performed (`crates/vike-boot/src/lib.rs` is why there is only one), and
//! [`halt_path_from_env`] prefers it over walking. A root that declares nothing is unchanged —
//! [`HaltProject::WalkFrom`] names the two shipped bins that are in exactly that state today.
//!
//! ## ⚠ RUNG 3's only alarm used to be an ACCIDENT OF THE SANDBOX, which a container does not have
//!
//! Rung 3 has always been reported — [`halt_path_from_env`] logs the resolved path — but the report
//! did not say it WAS rung 3, and the thing that made rung 3 loud anyway was
//! `ProtectSystem=strict`: under the shipped units the exe directory is a READ-ONLY MOUNT, so
//! [`halt_path_arming_error`] returns `EROFS` and the resolution comes out as `tracing::error!`.
//! The alarm was a property of the SANDBOX, not of the rung.
//!
//! Take the sandbox away and the alarm goes with it. In an OCI container the image filesystem is
//! writable by default and the binary lives in the IMAGE while `<project>` is a BIND MOUNT, so
//! `<exe_dir>/HALT` is perfectly creatable: the probe answers "armable", the report is a cheerful
//! `info`, and it reads exactly like a correct rung-2 resolution unless somebody notices that the
//! path is beside the binary. Docker's default working directory is `/`, which is precisely the "no
//! project resolves" case — so an operator doing `touch <project>/settings/state/HALT` on the HOST
//! arms a file this process never looks at, and every check in this module says yes.
//!
//! So the RUNG is now part of the report, and rung 3 is a `warn!` carrying
//! [`EXE_DIR_FALLBACK_ADVISORY`] rather than an `info!` — the one line that distinguishes "your kill
//! switch is in your project" from "your kill switch is inside the image". [`HaltRung`] is decided
//! in the SAME branch that decides the path ([`resolve_halt_path_rung`], which
//! [`resolve_halt_path`] is a projection of), so the reported rung cannot drift from the resolved
//! file — a second `match` restating the precedence beside the first is how that report would come
//! to describe a rung the resolver did not take.
//!
//! ⚠ **Severity, measured rather than asserted: this cannot leak a LIVE order, and the bound is
//! structural.** `vike_boot::boot` derives `Booted::state_dir` as `settings_dir.join("state")`, so
//! rung 3 is reached exactly when NO settings directory resolved — and the credential store is
//! `<project>/settings/secrets.env`, reached by the same pure resolver over the same override and
//! the same working directory (`vike_secrets::resolve_project`, pinned equal to
//! `vike_model::state_path::project_settings_dir_from` by `tests/settings_dir_spellings.rs`).
//! No settings directory therefore means no store, an EMPTY credential map, and every venue on
//! paper by the live gate. The two bins that never boot are bound the same way and by symmetry
//! rather than by luck: `crates/vike-run/src/bin/ibkr_mount.rs` takes its credentials from
//! `vike_secrets::load_workspace_dotenv`, which is `$VIKE_SETTINGS_DIR`-blind and walks from the
//! working directory — the SAME blindness and the SAME start as [`HaltProject::WalkFrom`], so the
//! two cannot disagree about whether a project exists.
//!
//! What rung 3 costs is therefore the REHEARSAL, which is not nothing: `docs/ops/kill-switches.md`
//! gap 4 is the argument that a paper node is where an operator learns what `touch` does, and
//! rehearsing against a sentinel nobody is watching teaches that the switch works when it does not.
//!
//! ## …and the resolution is REPORTED, because an unarmable switch is otherwise invisible
//!
//! Nothing here writes the sentinel: an operator does, with `touch`. So "the switch is broken" is a
//! fact about a `touch` that has not happened yet, discovered mid-incident. [`halt_path_from_env`]
//! therefore probes the resolved path ONCE per process and logs it —
//! `tracing::error!` when an operator could not create it, `info` when they could — so the failure
//! lands in the trace file at MOUNT time (`ExecActor::spawn` asks for the path) rather than at the
//! worst possible moment. [`halt_path_arming_error`] is that probe, pure enough to unit-test.
//!
//! ## What HALT blocks (and what it deliberately does not)
//! - **Submit that OPENS or ADDS: BLOCKED.** The order is refused and a terminal `OrderRejected`
//!   (`reason = `[`HALT_REJECT_REASON`]`)` is synthesized through the normal event path, so the
//!   intent never silently vanishes (the CLAUDE.md venue-adapter contract: "a dead venue path must
//!   synthesize a terminal `OrderRejected`").
//! - **Submit that REDUCES: ALLOWED** — see [`halt_admits_submit`], which is the whole of that rule,
//!   and is one shared function precisely so the clients that enforce this sentinel cannot drift
//!   into disagreeing about what a halt lets out: [`crate::ExecActor`] (every venue on the shared
//!   actor), hyperliquid's bespoke client, and `vike_paper::PaperExecutionClient` (the paper MOUNT —
//!   see the note on that re-export below for why the predicate lives in vike-exec now).
//!
//!   ⚠ **cTrader EXTENDS this predicate rather than replacing it, and the word carries the fix.**
//!   `crates/bridges/ctrader/src/exec.rs`'s `halt_admits_this_submit` is `closes || <this>`: it
//!   admits what that venue's own POSITION BOOK proves closes, because that client routes reduces by
//!   inspecting it — so on a hedging account a plain opposite-side order flattens with no flag, and
//!   the flag-only rule here alone would have refused that exit. It is `||` because a book is
//!   evidence FOR a reduce and never against one: cTrader's book holds nothing until the venue
//!   ANSWERS a reconcile for it, and that answer is best-effort — at connect and again at every
//!   reconnect — so a restarted daemon can easily have heard nothing about positions the account
//!   already holds. The verified-ONLY version that shipped first refused precisely those exits — the
//!   trap this whole arm exists to prevent, wearing the costume of a stronger check. The rule for a
//!   future client: call [`halt_admits_submit`]; if you also hold a position book, admit what it
//!   PROVES on top, never instead, and argue it in your own module doc as that one does.
//! - **Cancel: ALWAYS ALLOWED.** Cancels pass straight through, halted or not.
//! - **Modify: BLOCKED (conservative), on every client including the position-verified one.** A
//!   modify can add size or chase price, and "reduce-only" cannot be reliably inferred from the
//!   request alone (`order.qty` may be original or remaining) — nor from a position book, which
//!   answers a question about ORDERS and not about amendments to resting ones. The exit path under
//!   halt is a cancel or a reducing submit, never a modify, so blocking modify never traps a
//!   position. A blocked modify leaves the resting order at its current terms — identical to the
//!   modify dead-channel contract — so nothing vanishes. ⚠ Most clients do it SILENTLY
//!   (`docs/ops/kill-switches.md` section 5); cTrader emits a non-terminal `OrderModifyRejected`.
//!   The divergence is in the NOTIFICATION only, never in the verdict.
//!
//! ## ⚠ A CANCEL IS NOT AN EXIT — why the reducing-submit arm had to exist
//!
//! "Cancel is always allowed" was stated here as though it discharged the never-trap-you rule. It
//! does not, and the gap was real: a cancel removes a resting ORDER, whereas getting out of a
//! POSITION requires SENDING one. `OrderIntent::Flatten` mints a `reduce_only` MARKET order for
//! `|position|`, and until this arm existed the sentinel refused it like any other submit — so
//! `market-exit` under a HALT file ran its mass-cancel and then had every flatten leg come back
//! `OrderRejected`, leaving the operator halted WITH the position still open. The only escape was to
//! `rm` the sentinel, flatten, and re-`touch` it — which un-halts every OTHER order in that window,
//! from a phone, mid-incident.
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
//! ⚠ **`policy.toml`'s `halt_admit` knob does NOT change that here, and cannot.** `verify` asks a
//! client to weigh its own position book, and this boundary has none — so `ExecActor` calls
//! [`halt_admits_submit`] (the `Admit`/no-book case) unconditionally, `vike_model::halt_verify_support`
//! carries the written per-venue reason, and `vike_mount::make_engine` REPORTS the degrade at mount
//! so an operator who set `verify` learns it there rather than mid-incident. The full rule, the
//! three-state [`PositionEvidence`] it weighs, and the single authority for "what `verify` refuses"
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

/// The DECISION — `halt_admits_submit`, its policy-taking full form `halt_admits_submit_under` with
/// the `PositionEvidence` that form weighs, and the `HALT_REJECT_REASON` it rejects with —
/// re-exported at its historical path so every adapter, test and doc citation that names
/// `vike_bridge_core::halt::…` is unchanged.
///
/// ⚠ **The rule itself now lives in `crates/vike-exec/src/halt.rs`, and that is load-bearing rather
/// than tidiness.** The paper exchange (`vike_paper::PaperExecutionClient`) is an `ExecutionClient`
/// like any venue adapter and must refuse an opening order under a HALT file for the same reason a
/// live one does — but it may not depend on THIS crate, which owns the ureq/tungstenite/rustls
/// transport stack, so the predicate had to move down to the crate all three enforcing clients
/// share. The FILE half — the path precedence, the [`HALT_FILE_ENV`] override, the armability probe
/// and the once-per-process report below — deliberately stayed here: it is an environment read and a
/// `tracing` report, which belong at this layer and not in vike-exec.
///
/// ⚠ GATED ON `full`, because `vike-exec` is an OPTIONAL dependency of this crate and `full` is
/// what turns it on. Without the gate this line is a hard reference to a crate that is not linked,
/// so `cargo build -p vike-cli` — the only consumer taking `vike-bridge-core` with
/// `default-features = false` — failed with `E0433: cannot find module or crate vike_exec`. The
/// FILE half below stays ungated: it names nothing from vike-exec.
#[cfg(feature = "full")]
pub use vike_exec::halt::{
    halt_admits_submit, halt_admits_submit_under, PositionEvidence, HALT_REJECT_REASON,
};

/// The sentinel's FILE NAME, spelled once. Every rung of the precedence below joins the same name
/// onto a different directory, so an operator's `touch` target differs only in where it lives.
pub const HALT_FILE: &str = "HALT";

/// The variable that names the sentinel outright, skipping the walk: `VIKE_HALT_FILE`.
///
/// A `const` rather than a bare literal at the call site, per the repo convention
/// `crates/vike-ops/tests/settings_registry.rs` resolves (`Naming::Konst`) — and because it is the
/// doc anchor the units and `docs/ops/kill-switches.md` both point at.
pub const HALT_FILE_ENV: &str = "VIKE_HALT_FILE";

/// What [`halt_path_from_env`] says when the sentinel fell all the way to [`HaltRung::ExeDir`] AND
/// that path is creatable — the container shape, where no other check in this module objects.
///
/// A NAMED const rather than an inline string for the reason `vike-tradehub`'s
/// `PAPER_MOUNT_HALT_ADVISORY` is one: it is the anchor a gate keys on, so deleting the line is what
/// must go red, and the wording can be improved without breaking anything that points here.
///
/// It names the rung-2 path the operator was ABOUT to `touch`, because that is the action this line
/// exists to stop — an advisory that said only "the sentinel is beside the binary" would leave them
/// to work out that their `touch` on the mount does nothing.
pub const EXE_DIR_FALLBACK_ADVISORY: &str =
    "HALT sentinel fell back to the EXE DIRECTORY: no project resolved above this process's working \
     directory, so the kill switch is the path above — beside the BINARY. `touch \
     <project>/settings/state/HALT` will NOT be seen by this process. In a container the binary is \
     in the IMAGE and <project> is a bind MOUNT, and the working directory defaults to `/`, so this \
     is the expected shape there and not a rare one; the exe directory is also writable there, \
     which is why nothing else in this report objects. Set VIKE_HALT_FILE to the sentinel you \
     intend to `touch`, or give the process a working directory inside the project \
     (docs/ops/kill-switches.md).";

/// Resolve the HALT sentinel path from already-gathered inputs: [`HALT_FILE_ENV`] (`env_path`) when
/// non-blank, else `<state_dir>/HALT`, else `<exe_dir>/HALT`.
///
/// PURE — no environment read, no filesystem probe — so the precedence is unit-testable and so the
/// answer cannot silently change with a directory appearing or disappearing. Whether the resolved
/// path is USABLE is a separate question, deliberately: [`halt_path_arming_error`] answers it and
/// the caller reports it, rather than this quietly picking a different rung and leaving an operator
/// touching the path they were told about last week.
///
/// ⚠ **A blank `env_path` falls through instead of resolving to `""`.** An empty
/// `Environment=VIKE_HALT_FILE=` line in a unit (or an `EnvironmentFile` that sets it to nothing) is
/// a configuration mistake, and honouring it would resolve the sentinel to a path that can never
/// exist and can never be created — the exact silent-inoperable failure this whole module was
/// rewritten for, wearing a different hat. Same guard, same reason, as
/// `vike_model::state_path::project_settings_dir_from`.
pub fn resolve_halt_path(
    env_path: Option<&str>,
    state_dir: Option<&Path>,
    exe_dir: &Path,
) -> PathBuf {
    resolve_halt_path_rung(env_path, state_dir, exe_dir).0
}

/// WHICH RUNG of the precedence answered — the fact the mount-time report could not previously
/// state, and the one that separates "the sentinel is in your project" from "the sentinel is inside
/// the image" (see the module doc's rung-3 section).
///
/// It is not cosmetic. A rung-3 resolution is INDISTINGUISHABLE from a correct rung-2 one in a log
/// line that carries only the path, unless the reader already knows where this binary's exe
/// directory is — which is exactly what an operator reading a container's journal does not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HaltRung {
    /// Rung 1 — [`HALT_FILE_ENV`] named the sentinel outright.
    Env,
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
            Self::Env => "1: VIKE_HALT_FILE",
            Self::ProjectState => "2: <project>/settings/state",
            Self::ExeDir => "3: <exe_dir> (LAST RESORT — no project resolved)",
        }
    }
}

/// [`resolve_halt_path`] and the [`HaltRung`] that produced it, from ONE branch.
///
/// **This is the primitive and `resolve_halt_path` is the projection**, deliberately in that
/// direction: the report has to name the rung the resolver actually took, and a second `match`
/// restating this precedence beside the first is how a report comes to describe a rung that was not
/// taken. There is one `if`/`match` in this module and both callers read it.
///
/// PURE, for the same reasons [`resolve_halt_path`] is — no environment read, no filesystem probe.
pub fn resolve_halt_path_rung(
    env_path: Option<&str>,
    state_dir: Option<&Path>,
    exe_dir: &Path,
) -> (PathBuf, HaltRung) {
    if let Some(p) = env_path.map(str::trim).filter(|s| !s.is_empty()) {
        return (PathBuf::from(p), HaltRung::Env);
    }
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
    /// ⚠ `$VIKE_SETTINGS_DIR`-BLIND: [`vike_model::state_path::project_state_dir`] is the
    /// `_from`-less resolver, so under that override this answers with whatever the WORKING
    /// DIRECTORY happens to sit above. It is the historical behaviour and the right answer for a
    /// process with no composition root to ask (this crate's own tests) — it is not a shape a
    /// booting root should be in.
    ///
    /// ⚠ **Two SHIPPED bins are in it today, and both arm the sentinel** — present tense, not a
    /// hazard reserved for some future root: `crates/vike-run/src/bin/ibkr_mount.rs` builds a real
    /// `IbkrExecutionClient` (hence a [`crate::ExecActor`], which resolves this path at spawn) and
    /// `crates/vike-run/src/bin/polymarket_maker_paper.rs` reaches
    /// `crates/vike-run/src/lib.rs`'s `build_paper_maker_core`, whose paper book is armed from the
    /// same process-wide resolution. Neither calls `vike_boot::boot` at all, so neither HAS a
    /// resolved project to declare — the fix for either is `$VIKE_HALT_FILE`, which outranks this
    /// rung entirely, or a boot. `docs/ops/kill-switches.md` names both for the operator.
    WalkFrom(&'a Path),
}

/// [`resolve_halt_path`] with rung 2's project resolution done for you.
///
/// Split out of [`halt_path_from_env`] so a test can drive the WHOLE resolution — the walk
/// included — over a synthetic deployment tree without touching the process environment or the
/// process's own working directory (this workspace does not `set_var` under threads; see
/// `credentials.rs`). `crates/vike-bridge-core/tests/halt_default_path.rs` is that test.
///
/// PURE under [`HaltProject::Declared`]; under [`HaltProject::WalkFrom`] it probes the filesystem
/// for a project marker, which is what makes the arm worth naming at every call site.
pub fn halt_path_for(env_path: Option<&str>, project: HaltProject<'_>, exe_dir: &Path) -> PathBuf {
    halt_path_for_rung(env_path, project, exe_dir).0
}

/// [`halt_path_for`] and the [`HaltRung`] that produced it — the projection rule of
/// [`resolve_halt_path_rung`], one layer up, and for the same reason: [`halt_path_from_env`] reports
/// the rung and may not decide it a second time.
pub fn halt_path_for_rung(
    env_path: Option<&str>,
    project: HaltProject<'_>,
    exe_dir: &Path,
) -> (PathBuf, HaltRung) {
    let state_dir = match project {
        HaltProject::Declared(dir) => dir.map(Path::to_path_buf),
        HaltProject::WalkFrom(cwd) => vike_model::state_path::project_state_dir(cwd),
    };
    resolve_halt_path_rung(env_path, state_dir.as_deref(), exe_dir)
}

/// WHICH REPORT [`halt_path_from_env`] emits — the severity decision, lifted out so it can be
/// unit-tested without a `tracing` subscriber (this crate has no `tracing-subscriber` dev-dep, and
/// adding one to gate a three-arm match would be the wrong trade).
///
/// ⚠ It is the seam the REAL code calls, not a description of it beside it. A pure classifier that
/// nothing consulted would pin only itself — the failure mode this repo names
/// "declaration-pinning tests don't gate" — and the branch it described could then be reworded into
/// disagreeing with it while every test stayed green.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HaltReport {
    /// The sentinel is where a deployment means it to be and an operator can create it.
    Resolved,
    /// [`HaltRung::ExeDir`] AND creatable — the container shape, where no other check objects. This
    /// is the arm that exists because it used to be indistinguishable from [`Self::Resolved`].
    ExeDirFallback,
    /// The resolved path cannot be created, whatever rung produced it. The strongest fact, so it
    /// outranks the rung.
    NotArmable,
}

/// The report severity for a resolved sentinel: its [`halt_path_arming_error`] and its [`HaltRung`].
///
/// The ORDER is the content. An unarmable path outranks the rung because it is the stronger
/// statement and because it is how rung 3 announced itself on every systemd box (a read-only exe
/// mount); the rung-3 arm then catches the case that reading misses entirely, which is a rung 3 the
/// process CAN write — see the module doc.
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
/// This is how `$VIKE_SETTINGS_DIR` reaches rung 2 (see the module doc). It is a PARAMETER and not
/// an `env::var` here on purpose: `crates/vike-ops/tests/settings_registry.rs`'s `LIBRARY_PIN`
/// ratchets the library-read work-list DOWN, and this module is a library.
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
/// already decided, instead of walking again — the root `CLAUDE.md`'s rule, and the reason it
/// matters is that the `_from`-less resolvers are `$VIKE_SETTINGS_DIR`-blind while all three
/// shipped units set that variable. A second walk would answer for whatever directory the process
/// happens to sit above.
///
/// The cell lives in this module for historical reasons (the HALT sentinel was its first consumer),
/// but the VALUE is not halt-specific — it is the project. `vike_mount`'s cTrader arm derives the
/// credential store from it (`vike_ctrader::token_store::store_path_beside_state_dir`) so a rotated
/// OAuth grant is written back to the store the ROOT loaded, never to one a bridge crate guessed at.
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
            let _ = std::fs::remove_file(&probe);
            None
        }
        Err(e) => Some(format!("{} is not writable: {e}", parent.display())),
    }
}

/// The process's ONE resolved HALT sentinel path — [`HALT_FILE_ENV`], else the project state
/// directory, else the exe directory (see the module doc).
///
/// **Resolved ONCE and memoized**, for two reasons. It is consulted at every submit and the walk
/// underneath it does filesystem probes, which do not belong on a repeated boundary check; and a
/// kill switch whose path could differ between two submits is not a kill switch — an operator
/// resolves it once, over ssh, and must be able to rely on that answer for the life of the process.
///
/// The first call also REPORTS the resolution (see the module doc): `info` with the path, and
/// `error` naming the reason when [`halt_path_arming_error`] says an operator could not create it.
/// `ExecActor::spawn` asks for the path so that report lands when a real venue MOUNTS — nothing is
/// gained by discovering an unarmable switch on the first order.
///
/// Both reports carry a `project` field naming WHERE rung 2's project came from, because the two
/// sources answer differently under `$VIKE_SETTINGS_DIR` and "which one am I looking at" is
/// otherwise unanswerable from the log: a root that forgot [`declare_project_state_dir`] resolves a
/// perfectly valid-looking path off the working directory.
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
            let (path, rung) =
                halt_path_for_rung(std::env::var(HALT_FILE_ENV).ok().as_deref(), project, &exe_dir);
            let arming = halt_path_arming_error(&path);
            match halt_report(arming.as_deref(), rung) {
                // An UNARMABLE path outranks the rung: it is the stronger fact, and on the shipped
                // units it is how rung 3 announced itself (a read-only exe mount). Kept first so
                // that reading does not change.
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
                     out-of-band kill switch cannot be engaged. Create the directory, or set \
                     VIKE_HALT_FILE to a writable path (docs/ops/kill-switches.md)"
                ),
                // ARMABLE, and the last resort — the container shape. Everything else in this
                // module says yes here: the file can be created, so the probe above is silent.
                // `warn!` because the path is inside the IMAGE while the operator's `touch` lands on
                // the MOUNT, and nothing else in the log distinguishes that from a correct rung 2.
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The precedence, all three rungs, with nothing touching the filesystem.
    ///
    /// ⚠ The middle assertion is the REGRESSION GUARD for the defect this module was rewritten for:
    /// with a project state directory in hand, the default must NOT be `<exe_dir>/HALT`. Restore
    /// `exe_dir.join(HALT_FILE)` as the unconditional default and this line goes red.
    #[test]
    fn path_precedence_env_then_state_dir_then_exe_dir() {
        let exe = Path::new("/srv/vike-<unit>/bin");
        let state = Path::new("/srv/vike-<unit>/settings/state");

        // 1. VIKE_HALT_FILE wins outright.
        assert_eq!(
            resolve_halt_path(Some("/run/vike/HALT"), Some(state), exe),
            PathBuf::from("/run/vike/HALT")
        );

        // 2. The DEFAULT is the project state dir — the directory the shipped unit grants.
        assert_eq!(
            resolve_halt_path(None, Some(state), exe),
            PathBuf::from("/srv/vike-<unit>/settings/state/HALT")
        );
        assert_ne!(
            resolve_halt_path(None, Some(state), exe),
            PathBuf::from("/srv/vike-<unit>/bin/HALT"),
            "the exe directory is read-only under ProtectSystem=strict — defaulting there is the \
             bug this test exists for"
        );

        // 3. …and the exe directory only when no project resolves at all.
        assert_eq!(
            resolve_halt_path(None, None, exe),
            PathBuf::from("/srv/vike-<unit>/bin/HALT")
        );
    }

    /// **A DECLARED project is used verbatim — rung 2 does not walk behind the root's back.**
    ///
    /// The end-to-end proof (a `$VIKE_SETTINGS_DIR` that DISAGREES with the working directory, run
    /// through the real `vike_boot::boot`) is
    /// `crates/vike-bridge-core/tests/halt_default_path.rs`'s
    /// `the_settings_dir_override_moves_the_sentinel_off_the_working_directorys_project`. This is
    /// the pure half: the declaration reaches `resolve_halt_path` untouched, and the override
    /// outranks it.
    #[test]
    fn a_declared_project_supplies_rung_two_and_the_env_override_still_outranks_it() {
        let exe = Path::new("/srv/vike-<unit>/bin");
        let named = Path::new("/var/lib/vike/settings/state");

        assert_eq!(
            halt_path_for(None, HaltProject::Declared(Some(named)), exe),
            named.join(HALT_FILE),
            "the root's own boot walk decides rung 2"
        );
        assert_eq!(
            halt_path_for(Some("/run/vike/HALT"), HaltProject::Declared(Some(named)), exe),
            PathBuf::from("/run/vike/HALT"),
            "VIKE_HALT_FILE still wins outright — the three-rung precedence is unchanged"
        );
        // …and `Declared(None)` is a real answer: the root resolved NO project, so rung 3 applies.
        // Collapsing it into "nobody told me" would silently walk from the working directory here,
        // which is the exact blindness this arm exists to remove.
        assert_eq!(
            halt_path_for(None, HaltProject::Declared(None), exe),
            exe.join(HALT_FILE),
            "a root that found no project falls to the exe directory, it does not re-walk"
        );
    }

    /// ⚠ **THE RUNG GATE.** Each rung of the precedence REPORTS ITSELF, and the reported rung is
    /// the one the resolved path actually came from.
    ///
    /// Asserted as an agreement between the two, never as a table of expected rungs on its own: the
    /// defect being designed out is a report that describes a rung the resolver did not take, and a
    /// test that spelled the classification beside the classifier would be satisfied by any two
    /// matching copies of the same mistake. `resolve_halt_path` is a projection of
    /// `resolve_halt_path_rung`, so this also pins that the projection did not drift.
    #[test]
    fn every_rung_reports_itself_and_agrees_with_the_path_it_resolved() {
        let exe = Path::new("/srv/vike-<unit>/bin");
        let state = Path::new("/srv/vike-<unit>/settings/state");

        for (env, dir, want_rung, want_path) in [
            (Some("/run/vike/HALT"), Some(state), HaltRung::Env, PathBuf::from("/run/vike/HALT")),
            (None, Some(state), HaltRung::ProjectState, state.join(HALT_FILE)),
            (None, None, HaltRung::ExeDir, exe.join(HALT_FILE)),
            // A blank override is rung 1's ABSENCE, not rung 1 — it must report the rung that
            // actually answered, or an operator debugging an empty `Environment=` line is told the
            // variable took effect.
            (Some("   "), Some(state), HaltRung::ProjectState, state.join(HALT_FILE)),
            (Some(""), None, HaltRung::ExeDir, exe.join(HALT_FILE)),
        ] {
            let (path, rung) = resolve_halt_path_rung(env, dir, exe);
            assert_eq!(rung, want_rung, "wrong rung reported for env={env:?} state={dir:?}");
            assert_eq!(path, want_path, "wrong path for env={env:?} state={dir:?}");
            assert_eq!(
                path,
                resolve_halt_path(env, dir, exe),
                "`resolve_halt_path` must be a projection of `resolve_halt_path_rung`, not a \
                 second copy of the precedence"
            );
        }
    }

    /// ⚠ **THE REPORT GATE, and the whole point of the change.** A rung-3 sentinel that the process
    /// CAN create is its own report — not the cheerful one a correct rung 2 gets.
    ///
    /// This is the container shape. On a systemd box rung 3 announced itself through
    /// `halt_path_arming_error` (`ProtectSystem=strict` makes the exe directory a read-only MOUNT,
    /// so `touch` returns EROFS), and that alarm is a property of the SANDBOX rather than of the
    /// rung: in an image the exe directory is writable, the probe says `None`, and the resolution
    /// used to be reported exactly like a correct one. Collapse `ExeDirFallback` into `Resolved`
    /// and this goes red — which is the mutation that reproduces the finding.
    #[test]
    fn a_writable_exe_dir_fallback_is_reported_apart_from_a_correct_resolution() {
        assert_eq!(
            halt_report(None, HaltRung::ExeDir),
            HaltReport::ExeDirFallback,
            "an ARMABLE last-resort sentinel is the container shape: every other check in this \
             module says yes, so this classification is the only thing that can say otherwise"
        );
        for ok in [HaltRung::Env, HaltRung::ProjectState] {
            assert_eq!(halt_report(None, ok), HaltReport::Resolved);
        }
        // …and an unarmable path outranks the rung, on ALL THREE, so the reading that already
        // existed on the shipped units cannot have changed.
        for rung in [HaltRung::Env, HaltRung::ProjectState, HaltRung::ExeDir] {
            assert_eq!(halt_report(Some("EROFS"), rung), HaltReport::NotArmable);
        }
    }

    /// The advisory has to tell an operator the thing they are about to get wrong, so its two
    /// load-bearing claims are pinned: that the sentinel is beside the BINARY, and that the
    /// project-relative path they were about to `touch` is not the one being watched.
    ///
    /// Keyed on the two substrings and not on the whole message, deliberately — the wording will be
    /// improved, and a gate over the full prose is one that gets loosened until deleting the line
    /// stops reddening it.
    #[test]
    fn the_exe_dir_advisory_names_both_the_wrong_path_and_the_right_one() {
        assert!(
            EXE_DIR_FALLBACK_ADVISORY.contains("BINARY"),
            "the advisory must say WHERE the sentinel actually is"
        );
        assert!(
            EXE_DIR_FALLBACK_ADVISORY.contains("<project>/settings/state/HALT"),
            "the advisory must name the path the operator was about to `touch` in vain — that \
             `touch` is the action this line exists to stop"
        );
        assert!(
            EXE_DIR_FALLBACK_ADVISORY.contains(HALT_FILE_ENV),
            "…and the fix, which needs no rebuild"
        );
    }

    /// A blank override falls THROUGH rather than resolving the sentinel to `""` — a path that can
    /// neither exist nor be created, i.e. the same silent-inoperable switch by another route.
    #[test]
    fn a_blank_env_override_falls_through_to_the_default() {
        let exe = Path::new("/srv/vike-<unit>/bin");
        let state = Path::new("/srv/vike-<unit>/settings/state");
        for blank in [Some(""), Some("   "), Some("\t")] {
            assert_eq!(
                resolve_halt_path(blank, Some(state), exe),
                PathBuf::from("/srv/vike-<unit>/settings/state/HALT"),
                "an empty Environment=VIKE_HALT_FILE= line must not disarm the switch"
            );
        }
    }
}
