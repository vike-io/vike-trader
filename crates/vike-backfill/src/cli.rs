//! Shared backfill-CLI harness for the `*_backfill` bins — the small argv/store/symbol utilities
//! every bin re-implemented (finding F19). Folding them here removes the duplication AND fixes the
//! shipped `$VIKE_HIST_STORE` drift: the four "verbatim" bins honored the env var while
//! pmxt/databento/tardis silently ignored it and defaulted CWD-relative, so one operator env var
//! routed half the collectors to one store and half to another. [`store_root`] is now the single
//! resolution law and every bin goes through it.
//!
//! NOTE the one intentional sibling kept OUT of scope (it cannot depend on vike-backfill —
//! nothing may, it pulls every bridge crate): `vike-backtest/src/bin/backtest.rs` keeps its own
//! `store_root` copy. And `ingest_bench_bars` resolves a *different* root
//! (`market_data/bench_hist`, no env override) on purpose, so it is not a caller here.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

/// What a successful argv triage produced. `--help` and `--version` are neither a run nor an
/// error — the third and fourth outcomes. Named VARIANTS rather than an `Option` (which had room
/// for exactly one of them) so no call site can confuse them; the same shape `vike-tradehub`'s,
/// `vike-recorder`'s and `vike-datahub`'s `Parsed` use. This crate is the fourth adopter, not a
/// fifth pattern.
///
/// `Debug` so a triage that was supposed to FAIL can report what it produced instead
/// (`Result::expect_err` requires it) — the whole value of the unknown-argument tests.
#[derive(Debug, PartialEq, Eq)]
pub enum Parsed {
    Run,
    Help,
    Version,
}

/// The argv contract of one bin in this crate, declared so [`CliSpec::triage`] can answer
/// `-h`/`--help` and `-V`/`--version` **before the bin does anything**, and REJECT an unrecognised
/// flag instead of ignoring it.
///
/// ## Why a declared flag set rather than a bare help check
///
/// Most bins here read their options with [`arg`]/[`has_flag`], which look up ONE flag at a time
/// and are blind to every token they were not asked about — so `--stroe /data` silently resolved
/// to the default store and the backfill wrote to the wrong root with no diagnostic at all. A
/// lookup-shaped parser cannot notice that; only an enumerated set can. Declaring the flags costs
/// one const per bin and is what makes the third clause of the contract (reject the unknown)
/// reachable for these bins at all.
///
/// ## The walk
///
/// `valued` flags consume the token after them (so `--store /x` never mistakes `/x` for a
/// positional), `toggles` consume only themselves, and `positionals` is how many BARE tokens the
/// bin accepts (5 for the `<ROOT_DIR> <SYMBOL> <INTERVAL> <START_MS> <END_MS>` klines bins, 0 for
/// a fully flag-driven one). Anything else beginning with `-` is an error.
///
/// ⚠ **A valued flag's value must EXIST and must not itself be a flag** — the workspace-wide rule
/// [`is_flag_token`] spells. This gate is where it bites hardest, because most bins in this crate
/// read their options with [`arg`], which returns the following token BLINDLY: with argv
/// `--store --dry-run`, `arg` answers `Some("--dry-run")` and the backfill writes into a directory
/// named `--dry-run` while `has_flag` still reports the toggle as set, so nothing anywhere
/// disagrees. `triage` runs FIRST in every bin here (see [`CliSpec::short_circuit`]), so refusing
/// the command line at this one site is what keeps that unreachable for all of them.
///
/// ⚠ `--flag=value` is REJECTED, deliberately and as an improvement: [`arg`] never understood that
/// spelling, so `--store=/x` already fell through to the default store — silently. Now it says so.
/// That is a property of THIS crate's bins only — `vike-tradehub` and `vike-cli` accept the `=`
/// form — and the value rule above is deliberately orthogonal to it: it is about what a value may
/// BE, not about how it is spelled.
pub struct CliSpec<'a> {
    /// The bin's own name, used in the usage-error prefix and the `--version` line. Not
    /// `CARGO_PKG_NAME`, which is `vike-backfill` for all 25 bins and answers the wrong question.
    pub bin: &'a str,
    /// The full usage text, printed on stdout for `--help` and after the message on a usage error.
    pub usage: &'a str,
    /// `--flag VALUE` flags: each consumes the following token.
    pub valued: &'a [&'a str],
    /// Valueless `--flag` toggles.
    pub toggles: &'a [&'a str],
    /// How many BARE (non-`-`-prefixed) arguments this bin accepts.
    pub positionals: usize,
}

impl CliSpec<'_> {
    /// Triage the process argv. `args` is the FULL argv including `argv[0]` — the shape every bin
    /// in this crate already holds (`std::env::args().collect()`) and the shape [`arg`] and
    /// [`has_flag`] take, so no call site has to remember which slice it is passing.
    pub fn triage(&self, args: &[String]) -> Result<Parsed, String> {
        let mut positionals = 0usize;
        let mut it = args.iter().skip(1);
        while let Some(a) = it.next() {
            match a.as_str() {
                "-h" | "--help" => return Ok(Parsed::Help),
                "-V" | "--version" => return Ok(Parsed::Version),
                f if self.valued.contains(&f) => {
                    // A valued flag with nothing after it is a usage error, not a silent `None` —
                    // and neither is another FLAG a value (see the type doc's second ⚠).
                    match it.next() {
                        None => return Err(missing_value(f)),
                        Some(v) if is_flag_token(v) => return Err(swallowed_flag(f, v)),
                        Some(_) => {}
                    }
                }
                f if self.toggles.contains(&f) => {}
                f if f.starts_with('-') => return Err(format!("unknown argument: {f}")),
                _ => {
                    positionals += 1;
                    if positionals > self.positionals {
                        return Err(format!("unexpected argument: {a}"));
                    }
                }
            }
        }
        Ok(Parsed::Run)
    }

    /// The three things every binary owes a caller, resolved to an exit STATUS.
    /// `None` means "keep going, this is a run"; `Some(code)` means the process is done.
    fn outcome(&self, args: &[String]) -> Option<u8> {
        match self.triage(args) {
            Ok(Parsed::Run) => None,
            // Help is normal output a caller pipes into a pager, so STDOUT and exit 0 — a non-zero
            // `--help` breaks `set -e`, packaging smoke tests and any wrapper checking a status.
            Ok(Parsed::Help) => {
                println!("{}", self.usage);
                Some(0)
            }
            // `<name> <version>`, the shape every `--version` on the box prints (`git version 2.x`).
            // The NAME is the bin's; the VERSION is the crate's, which is what these bins ship as.
            Ok(Parsed::Version) => {
                println!("{} {}", self.bin, env!("CARGO_PKG_VERSION"));
                Some(0)
            }
            Err(e) => {
                eprintln!("{}: {e}\n\n{}", self.bin, self.usage);
                Some(2)
            }
        }
    }

    /// For a `fn main() -> ExitCode` bin: `Some(code)` means return it immediately.
    ///
    /// ⚠ **Call this FIRST — before `vike_log::init`, before opening a store, before any network
    /// call.** Answering a question about the command line must not create a log file, create a
    /// store directory, spawn a subprocess or ingest anything. Every bin in this crate used to
    /// initialise logging before it looked at argv, and two of them did considerably more than that
    /// (`ingest_bench_bars` ran the whole ingest; `poly_reparse` panicked).
    pub fn short_circuit(&self, args: &[String]) -> Option<ExitCode> {
        self.outcome(args).map(ExitCode::from)
    }

    /// The `fn main()` (no `ExitCode`) twin of [`CliSpec::short_circuit`], for the bins that report
    /// failure with `std::process::exit` throughout: it EXITS rather than returning a status, so
    /// the call site stays one line. Same ordering rule — call it first.
    pub fn short_circuit_or_exit(&self, args: &[String]) {
        if let Some(code) = self.outcome(args) {
            // Flush what `outcome` just printed: `std::process::exit` runs no destructors, and a
            // `--help` whose text never reached the pipe is the same defect as no `--help` at all.
            use std::io::Write;
            let _ = std::io::stdout().flush();
            std::process::exit(i32::from(code));
        }
    }
}

/// Build the `vike_log::LogConfig` every batch/backfill bin in this crate should pass to
/// `vike_log::init`: file level `warn` instead of vike-log's own `trace` default. These bins run
/// long and unattended, driving chatty deps (DataFusion/arrow/hyper) whose trace-level rows no
/// console-level knob can silence (`RUST_LOG`/`VIKE_LOG` filter the console layer only — see
/// `vike_log::file_level_directive`'s doc). Measured cost of the old default: one hour of
/// `pmxt_backfill` wrote 6.4 GB, and a long range once wrote 341 GB and nearly filled the disk
/// hosting a live trading node. `VIKE_LOG_FILE_LEVEL` still wins over this default — `init`
/// resolves the env var before `cfg.file_level` — so a one-off debug run can still ask for
/// `trace`/`debug` without a code change.
///
/// It also sets the DEFAULT log DIRECTORY — `<project>/settings/state/logs`, beside every other
/// file the program writes — rather than leaving it to vike-log's `<exe_dir>/logs` last resort,
/// which in a checkout is `target/debug/logs/…` and vanishes with `cargo clean`. Same shape as
/// [`pace_book_path`] below: the WALK is `vike_model::state_path`'s (pure), the working directory
/// is read here, and `$VIKE_LOG_DIR` still wins over the result (`vike_log::resolve_log_dir`).
/// A bin run with no project above it keeps the historical `<exe_dir>/logs`.
pub fn log_config(file_prefix: impl Into<String>) -> vike_log::LogConfig {
    vike_log::LogConfig {
        file_prefix: file_prefix.into(),
        file_level: "warn".to_string(),
        project_dir: std::env::current_dir()
            .ok()
            .and_then(|cwd| vike_model::state_path::project_log_dir(&cwd)),
        ..Default::default()
    }
}

/// **THE RULE, spelled once for this crate: a token beginning with `--` is a FLAG, never a value.**
///
/// It is `--`, not a bare `-`, and that is the load-bearing half. A NEGATIVE NUMBER is a real value
/// in this workspace's parsers — `--pacing-ms -1` must reach the integer parse (which is what
/// refuses it, by name), `--min-liquidity -1` is accepted verbatim by a maker knob — so a
/// `starts_with('-')` test would turn a value into a usage error for every signed field. Nothing
/// this crate's bins take legitimately begins with `--`; a path that genuinely does is reachable as
/// `./--weird`.
///
/// The residual, stated rather than implied: a SINGLE-dash token (`-h`, `-V`, `-5`) is still
/// accepted as a value. That is the price of keeping negative numbers, and it is cheap here because
/// no bin in this crate takes a short flag that could be eaten.
pub fn is_flag_token(token: &str) -> bool {
    token.starts_with("--")
}

/// The two halves of the rule, as ONE message shape each, so a bin's error reads the same wherever
/// it came from. Both name the FLAG (never only the offending value) — the whole point is that the
/// operator learns which of their flags went unfed.
pub fn missing_value(flag: &str) -> String {
    format!("{flag} needs a value")
}

/// The swallow half of [`missing_value`]'s pair: the next token exists but is a flag.
pub fn swallowed_flag(flag: &str, value: &str) -> String {
    format!("{flag} needs a value, but the next argument is another flag ({value})")
}

/// Resolve one valued flag's value from the token that follows it, applying the rule above.
///
/// `next` is what the caller's iterator yielded (`it.next()`), so an ABSENT flag never reaches this
/// function at all — an optional flag that was simply not mentioned keeps its default, exactly as
/// before. What changes is the flag that WAS mentioned and was not fed: it is now an error in the
/// parser itself rather than only in [`CliSpec::triage`], so the parser is safe when read alone.
/// That mattered because the two live in different functions, and the tests said so: every bin here
/// pinned "the parser ACCEPTS a valueless `--store`; the SPEC is what refuses it".
pub fn flag_value(flag: &str, next: Option<String>) -> Result<String, String> {
    match next {
        None => Err(missing_value(flag)),
        Some(v) if is_flag_token(&v) => Err(swallowed_flag(flag, &v)),
        Some(v) => Ok(v),
    }
}

/// Positional `--flag <value>` lookup over the process argv (the 5-copy helper the bins pasted).
/// Returns the argument immediately following `flag`, or `None` if the flag is absent / trailing.
///
/// ⚠ This one is deliberately still BLIND to [`is_flag_token`]: it is a raw lookup whose contrast
/// with [`has_flag`] (a valueless flag followed by another flag) is pinned by this module's own
/// tests, and every bin that uses it has already passed [`CliSpec::triage`], which refuses that
/// command line. Fixing it here would change nothing an operator can reach and would erase the
/// distinction the pair exists to draw.
pub fn arg(args: &[String], flag: &str) -> Option<String> {
    args.iter().position(|a| a == flag).and_then(|i| args.get(i + 1).cloned())
}

/// Presence-only (valueless) flag lookup over the process argv — the `--dry-run` shape, which
/// [`arg`] cannot express because it would swallow whatever token happens to follow.
pub fn has_flag(args: &[String], flag: &str) -> bool {
    args.iter().any(|a| a == flag)
}

/// The `VIKE_HIST_STORE` override, spelled once for this crate's map lookup.
const HIST_STORE_VAR: &str = "VIKE_HIST_STORE";

/// Resolve the hist-store root with the canonical precedence: explicit `--store` →
/// `$VIKE_HIST_STORE` → the repo-root default `<repo>/market_data/hist` when this box still has that
/// checkout → the PROJECT's own `<project>/market_data/hist` → the per-user `…/vike-data`. This is THE fix
/// for the shipped drift — every backfill bin now honors the same env var and the same default.
///
/// PURE: `vars` is the process environment as the CALLING BIN collected it
/// (`std::env::vars().collect()`), per the settings-registry rule that libraries take configuration
/// as parameters and only binaries read the process environment. This function used to read four
/// variables itself (`VIKE_HIST_STORE` plus the `XDG_DATA_HOME`/`HOME`/`LOCALAPPDATA` platform
/// trio) from a LIBRARY file, so all four carried `Layer::Library` rows on the registry's STEP-2
/// work-list even though all sixteen call sites are bins in this same crate (fifteen files).
///
/// ⚠ The `std::env::current_dir()` below is the same DIFFERENT kind of read [`pace_book_path`]
/// documents: it names no variable, so no operator can export it, no registry row can describe it
/// and the settings gate has nothing to see either way. `$VIKE_SETTINGS_DIR` is the override that
/// DOES pin the project, and [`vike_model::store_path::resolve_store_root_from`] honours it out of
/// the map this function is already handed — so no bin gains an argument and no rung is lost.
///
/// **The resolution is LOGGED**, root and rung both. A store does not merge: if the default moves,
/// the old store is simply no longer read and a backfill reports zero rows into a fresh empty one —
/// indistinguishable from "the venue returned nothing" until somebody goes looking.
pub fn store_root(
    explicit: Option<&str>,
    vars: &std::collections::HashMap<String, String>,
) -> PathBuf {
    let cwd = std::env::current_dir().ok();
    let resolved = resolve(explicit, &default_store_root(), cwd.as_deref(), vars);
    tracing::info!(
        store = %resolved.root.display(),
        rung = resolved.rung.as_str(),
        "hist store root resolved: {}",
        resolved.rung.why()
    );
    resolved.into_path()
}

/// The WIRING used by [`store_root`], with `repo_default` and `cwd` injected so the env-var-wins
/// rule stays unit-testable with a one-entry map — and so a test can reach the rungs BELOW the
/// dev-checkout hinge, which a `cargo test` run can never otherwise see (the compile-time repo path
/// always exists while testing, so the hinge always fires).
///
/// Delegates to [`vike_model::store_path::resolve_store_root_from`] — ONE precedence for every
/// binary in the workspace, including the project and installed-user fallbacks the repo-relative
/// default cannot serve.
///
/// ⚠ `resolve_store_root_from`, never the bare ladder: the ladder's project and per-user defaults
/// are two adjacent `Option<PathBuf>`s that a transposition would swap SILENTLY, sending gigabytes
/// to the wrong disk with no compile error and no runtime complaint. Passing the working directory
/// and the environment map instead leaves no two arguments here of the same type.
fn resolve(
    explicit: Option<&str>,
    repo_default: &Path,
    cwd: Option<&Path>,
    vars: &std::collections::HashMap<String, String>,
) -> vike_model::store_path::StoreRoot {
    // Always `Some`: this crate is NOT a release asset (`release.yml` builds no `vike-backfill`
    // binary, and `xtask::ci::tables` excludes it from CI), so the compile-time checkout path
    // never reaches a public download and the dev-checkout rung stays in every profile. The
    // shipped call sites — `vike-app`'s `studio_store_root`, `vike-backtest`'s `binutil`,
    // `vike-datahub`'s `run` — pass it under `cfg(debug_assertions)` only, and
    // `vike_model::store_path`'s module doc (rung 4) says why the two differ.
    vike_model::store_path::resolve_store_root_from(
        explicit.map(PathBuf::from),
        vars.get(HIST_STORE_VAR).cloned(),
        Some(repo_default),
        cwd,
        vars,
    )
}

/// `<project>/tmp` — the scratch root every collector in this crate stages into, **swept** on the
/// way out so the directory cannot grow without bound.
///
/// # Why this is not `std::env::temp_dir()` any more
///
/// Every bin here used to stage into a FIXED name under the system temp directory
/// (`clickhouse_poly_backfill/`, `pmxt_backfill/`, `vike_databento/`, …) and two things were wrong
/// with that at once.
///
/// * **The container.** This project is moving to ONE image with the project folder mounted in,
///   where the system temp directory is not the host's, does not survive a restart, and cannot be
///   mounted beside the project. See [`vike_model::state_path::PROJECT_TMP_DIR`] for the full
///   argument; `crates/vike-ops/tests/system_temp_gate.rs` is the ratchet.
/// * **The retention.** Nothing removed a staged export, ever. Measured on the CI box on 2026-08-23:
///   26,851 leaked scratch directories totalling 215 GB, about 70% of that filesystem. A fixed name
///   made it worse rather than better on a shared box — whichever user created
///   `/tmp/pmxt_backfill` first OWNED it, and every later run as the other user failed
///   `PermissionDenied` forever.
///
/// So this returns a root, and the caller allocates inside it with
/// [`vike_model::scratch::ScratchDir`], which removes what it created — including on the panic
/// path.
///
/// # What this call also DOES, deliberately
///
/// It [`sweep`](vike_model::scratch::sweep)s. A guard cleans up only what the running process
/// created, and `Drop` does not run on a `SIGKILL`, an OOM kill or a power loss — so without a
/// sweep the abandoned population is unbounded, which is exactly how the 215 GB happened. Folding
/// it into the resolver means no bin can resolve the root and forget the retention; the alternative
/// was a second call every bin had to remember, and the tree already knows how that ends.
///
/// The sweep result is LOGGED for the reason [`store_root`] logs its rung: housekeeping that
/// removes gigabytes without saying so is indistinguishable from a disk that quietly emptied
/// itself.
///
/// # PURE, in the sense the settings registry means
///
/// `vars` is the process environment as the CALLING BIN collected it — the same parameter
/// [`store_root`] takes, and usually the same map. The `std::env::current_dir()` below is the
/// DIFFERENT kind of read [`pace_book_path`] documents: it names no variable, so no operator can
/// export it and no registry row can describe it.
///
/// ⚠ The `$VIKE_SETTINGS_DIR` lookup goes through `vike_model::state_path::SETTINGS_DIR_ENV` rather
/// than a literal spelled here, and that is an ATTRIBUTION choice rather than an oversight. The
/// variable, its blank-value rule and its whole meaning belong to `vike-model`, which carries the
/// registry row describing it; this function forwards it into that crate's own resolver exactly as
/// [`store_root`] forwards the whole map into `resolve_store_root_from`. A literal here would claim
/// `vike-backfill` has an independent reading of a variable it merely passes along.
///
/// # The fallback, and why it is CWD-relative
///
/// No project above the working directory means no `<project>/tmp` to answer with, and a scratch
/// path is needed regardless. `<cwd>/tmp` is the genuine last resort — the same shape
/// `crates/vike-research/src/bin/research.rs`'s `default_lightgbm` fell back to for the same
/// reason (a DELETED binary, gone with the research crate — the citation is the evidence for the
/// precedent, filed in `crates/vike-ops/tests/citation_gate.rs`'s `DEAD_PATH_EXCEPTIONS`) — and it
/// is still inside a directory the operator chose, which the system temp directory
/// is not.
pub fn scratch_root(vars: &std::collections::HashMap<String, String>) -> PathBuf {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let root = vike_model::state_path::project_tmp_dir_from(
        vars.get(vike_model::state_path::SETTINGS_DIR_ENV).map(String::as_str),
        &cwd,
    )
    .unwrap_or_else(|| cwd.join(vike_model::state_path::PROJECT_TMP_DIR));

    let swept =
        vike_model::scratch::sweep(&root, Some(vike_model::scratch::DEFAULT_MAX_SCRATCH_ENTRIES));
    tracing::info!(
        scratch = %root.display(),
        found = swept.found,
        removed = swept.removed,
        failed = swept.failed,
        "scratch root resolved and swept"
    );
    root
}

/// Basename of the persisted pace file — see [`pace_book_path`].
const PACE_BOOK_FILE: &str = "pace.json";

/// The pace-file override key [`pace_book_path`] looks up in the caller-supplied environment map,
/// spelled once like [`HIST_STORE_VAR`] above. The settings-registry scanner resolves the `const`
/// indirection, and the constant is the natural doc anchor.
const PACE_BOOK_VAR: &str = "VIKE_PACE_BOOK";

/// Resolve the persisted-pace file: an explicit caller value → `$VIKE_PACE_BOOK` →
/// `<project>/settings/state/pace.json` → `<store_root>/pace.json`.
///
/// **Under the project's state directory** (`vike_model::state_path::project_state_dir`, the
/// settings-unification root), because `pace.json` is PROGRAM-WRITTEN STATE by that design's own
/// test — delete it and the program re-derives it — and every such file belongs in the one folder
/// the app owns. It is the third of the three state resolvers to be repointed, after
/// `vike_app_core::workspace::persist`'s `state_dir` and `vike-app`'s `state_dir_path`.
///
/// It used to default beside the STORE, argued as "a pace describes this egress filling THIS
/// store". That reasoning was real but it is the same reasoning `studio_workspace.json` was moved
/// off: colocating with unrelated data scatters the files the program owns across as many roots as
/// there are stores, and the operator gains nothing they cannot get from `$VIKE_PACE_BOOK`. The two
/// escape hatches the old default served are both still served — a read-only store mount, or
/// several jobs deliberately sharing one record — by that override, which still wins.
///
/// ⚠ **A box appears to forget its measured pace ONCE.** This is a silent relocation, not a
/// migration: there is deliberately no dual read (`vike_model::state_path::read_path`) here, because
/// the file is a CACHE and not an input the pipeline depends on — `crate::pace_book::load_path`
/// answers "absent" and "unparseable" identically, with an empty book, and every pacer then starts
/// exactly where it started before this file existed. So the first run after this change re-measures
/// from the pacer's pessimistic constant and writes a fresh record at the new path; the run after
/// that is back to normal. The cost is one backfill's opening pages. The old `<store_root>/pace.json`
/// is left in place, unread — deleting an operator's file to save them one slow run is the worse
/// trade.
///
/// The last row is what a binary run with NO project above its working directory gets: unchanged
/// behaviour, `<store_root>/pace.json`.
///
/// The ENVIRONMENT is INJECTED, exactly like [`store_root`] above and for the same reason: `vars`
/// is the process environment as the CALLING BIN collected it (`std::env::vars().collect()`), so a
/// LIBRARY never reads configuration its caller can neither see nor override. This function used to
/// read `$VIKE_PACE_BOOK` itself and carried this crate's last `Layer::Library` row on the settings
/// registry's STEP-2 work-list. Moving the ENVIRONMENT rather than the lookup keeps the whole
/// precedence — including WHICH key is consulted — right here, so nothing is duplicated across the
/// five `<venue>_backfill` bins; that is the same third shape the `store_root` lift used.
///
/// ⚠ The `std::env::current_dir()` below is a DIFFERENT kind of read and deliberately stays. It
/// names no variable: an operator cannot export it, no registry row can describe it, and the
/// settings gate — which keys on `env::var`/`env::var_os` calls and on env-shaped map keys — has
/// nothing to see either way, so injecting it would retire no row and buy no override, while
/// costing every bin an extra argument. What the STEP-2 rule is actually about is still honoured:
/// the precedence stays testable with no process state at all, because [`resolve_pace_book_path`]
/// takes the resolved directory as a parameter.
pub fn pace_book_path(
    explicit: Option<&str>,
    vars: &std::collections::HashMap<String, String>,
    store_root: &std::path::Path,
) -> PathBuf {
    resolve_pace_book_path(
        explicit,
        vars.get(PACE_BOOK_VAR).cloned(),
        std::env::current_dir()
            .ok()
            .and_then(|cwd| vike_model::state_path::project_state_dir(&cwd)),
        store_root,
    )
}

/// Pure precedence used by [`pace_book_path`], split out so the env-var-wins rule is unit-testable
/// without touching the process environment OR the working directory — the same split
/// [`store_root`]/[`resolve`] uses. `project_state_dir` is `<project>/settings/state` as the CALLER
/// resolved it, `None` when no `Cargo.toml` sits above the working directory.
///
/// An empty/whitespace `$VIKE_PACE_BOOK` falls THROUGH to the default rather than resolving to `""`:
/// an exported-but-blank var is an unset var in every shell that produced it, and honouring it would
/// point the writer at the process CWD.
fn resolve_pace_book_path(
    explicit: Option<&str>,
    env: Option<String>,
    project_state_dir: Option<PathBuf>,
    store_root: &std::path::Path,
) -> PathBuf {
    explicit
        .map(PathBuf::from)
        .or_else(|| env.filter(|s| !s.trim().is_empty()).map(PathBuf::from))
        .or_else(|| project_state_dir.map(|d| d.join(PACE_BOOK_FILE)))
        .unwrap_or_else(|| store_root.join(PACE_BOOK_FILE))
}

/// `<repo>/market_data/hist`, derived at COMPILE time from this crate's manifest dir so it resolves to the
/// checkout that built the binary (works from any worktree, never a CWD-relative or hardcoded
/// path). `crates/vike-backfill` → `.ancestors().nth(2)` → the repo root.
fn default_store_root() -> PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("CARGO_MANIFEST_DIR has a repo-root ancestor")
        .join("market_data")
        .join("hist")
}

/// Parse a `--symbols` spec: a comma list of `vikeSymbol=sourceSymbol` pairs (a bare `SYM` means
/// source == symbol). Whitespace around each field is trimmed; empty entries are dropped. `Err`
/// (with a caller-printable message) when the spec yields no pairs. The exact `vikeSymbol=source`
/// contract the eod / ibkr / hyperliquid bins document.
pub fn parse_symbol_pairs(spec: &str) -> Result<Vec<(String, String)>, String> {
    let pairs: Vec<(String, String)> = spec
        .split(',')
        .filter(|s| !s.trim().is_empty())
        .map(|pair| match pair.split_once('=') {
            Some((sym, src)) => (sym.trim().to_string(), src.trim().to_string()),
            None => (pair.trim().to_string(), pair.trim().to_string()),
        })
        .collect();
    if pairs.is_empty() {
        return Err("--symbols produced no entries".into());
    }
    Ok(pairs)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(v: &[&str]) -> Vec<String> {
        std::iter::once("prog").chain(v.iter().copied()).map(str::to_string).collect()
    }

    /// A representative spec: valued flags, a toggle, and no positionals — the shape 18 of the
    /// 25 bins have.
    const FLAGGY: CliSpec = CliSpec {
        bin: "demo_backfill",
        usage: "usage: demo_backfill --from D --to D [--store DIR] [--dry-run]",
        valued: &["--from", "--to", "--store"],
        toggles: &["--dry-run"],
        positionals: 0,
    };

    /// …and the positional shape the five klines bins plus `dukascopy_backfill` have.
    const POSITIONAL: CliSpec = CliSpec {
        bin: "demo_klines",
        usage: "usage: demo_klines <ROOT> <SYM> <IV> <START> <END>",
        valued: &[],
        toggles: &[],
        positionals: 5,
    };

    /// The two outcomes that are NOT a run, in both spellings each. These were the whole defect:
    /// every bin in this crate answered `--help` with a usage error, a panic, or an ingest.
    #[test]
    fn help_and_version_are_outcomes_not_errors() {
        for flag in ["-h", "--help"] {
            assert_eq!(FLAGGY.triage(&argv(&[flag])), Ok(Parsed::Help), "{flag}");
            assert_eq!(POSITIONAL.triage(&argv(&[flag])), Ok(Parsed::Help), "{flag}");
        }
        for flag in ["-V", "--version"] {
            assert_eq!(FLAGGY.triage(&argv(&[flag])), Ok(Parsed::Version), "{flag}");
            assert_eq!(POSITIONAL.triage(&argv(&[flag])), Ok(Parsed::Version), "{flag}");
        }
        // …and asked for AFTER other arguments, which is how a person actually reaches for it
        // ("what were the other flags again?").
        assert_eq!(FLAGGY.triage(&argv(&["--from", "x", "--help"])), Ok(Parsed::Help));
    }

    /// A valid invocation is still a run — the property that keeps "exit 0 on --help" from being
    /// bought by making everything exit 0, and that keeps this triage out of the way of real work.
    #[test]
    fn a_valid_invocation_triages_as_a_run() {
        assert_eq!(FLAGGY.triage(&argv(&[])), Ok(Parsed::Run));
        assert_eq!(
            FLAGGY.triage(&argv(&["--from", "2026-01-01", "--to", "2026-01-02", "--dry-run"])),
            Ok(Parsed::Run)
        );
        assert_eq!(POSITIONAL.triage(&argv(&["/d", "BTC", "1m", "0", "1"])), Ok(Parsed::Run));
    }

    /// The negative half. `arg`/`has_flag` are one-flag lookups blind to every token they were not
    /// asked about, so a typo used to resolve to the DEFAULT and write to the wrong store in
    /// silence.
    #[test]
    fn an_unknown_flag_is_rejected_rather_than_ignored() {
        let err = FLAGGY.triage(&argv(&["--stroe", "/data"])).expect_err("a typo must not run");
        assert!(err.contains("--stroe"), "the error names the offending argument: {err}");
        // `--flag=value` too: `arg` never understood that spelling and silently ignored it.
        assert!(FLAGGY.triage(&argv(&["--store=/x"])).is_err(), "--flag=value is not supported");
        // A lowercase `-v` is verbosity everywhere else on the box, so it stays unknown here.
        assert!(FLAGGY.triage(&argv(&["-v"])).is_err(), "-v must stay unknown");
    }

    /// A valued flag consumes the token after it, so a store PATH is never counted as a stray
    /// positional — and a trailing valued flag with nothing after it is a usage error, not a
    /// silent `None` (which is what `arg` returned, leaving the bin to use its default).
    #[test]
    fn valued_flags_consume_their_value_and_a_trailing_one_is_an_error() {
        assert_eq!(FLAGGY.triage(&argv(&["--store", "/x/y"])), Ok(Parsed::Run));
        let err = FLAGGY.triage(&argv(&["--store"])).expect_err("a trailing valued flag");
        assert!(err.contains("--store"), "{err}");
    }

    /// **The swallow rule, at the gate every bin in this crate runs first.** A valued flag may not
    /// eat a FLAG: `--store --dry-run` used to triage as a clean run, after which `arg` answered
    /// `Some("--dry-run")` for the store root while `has_flag` still reported the toggle as set —
    /// the backfill wrote into a directory named `--dry-run` and nothing anywhere disagreed.
    ///
    /// Both messages name the FLAG that went unfed, and the swallow message names the eaten token
    /// too, so the diagnostic is about the operator's mistake rather than about whatever landed in
    /// flag position afterwards.
    #[test]
    fn a_valued_flag_may_not_swallow_a_following_flag() {
        for line in [
            &["--store", "--dry-run"][..],
            &["--from", "--to", "2026-01-01"][..],
            &["--store", "--"][..],
        ] {
            let err = FLAGGY.triage(&argv(line)).expect_err("a flag is not a value");
            assert!(err.contains(line[0]), "the message names the unfed flag: {err}");
            assert!(err.contains(line[1]), "…and the token it would have eaten: {err}");
        }
        // The rule is `--`, not `-`: a NEGATIVE NUMBER is a real value and still reaches the bin.
        assert_eq!(FLAGGY.triage(&argv(&["--from", "-1"])), Ok(Parsed::Run));
        assert_eq!(FLAGGY.triage(&argv(&["--store", "-h"])), Ok(Parsed::Run), "the residual");
    }

    /// [`flag_value`] is the same rule as a function, for the five bins in this crate that parse
    /// their own argv rather than reading it back with [`arg`]. An ABSENT optional flag never
    /// reaches it — that is what keeps an unmentioned `--store` on its default instead of becoming
    /// an error.
    #[test]
    fn flag_value_refuses_a_missing_and_a_flag_shaped_value() {
        assert_eq!(flag_value("--store", Some("/x".to_string())).unwrap(), "/x");
        assert_eq!(
            flag_value("--store", Some("-1".to_string())).unwrap(),
            "-1",
            "a negative value"
        );
        assert_eq!(
            flag_value("--store", Some(String::new())).unwrap(),
            "",
            "emptiness is per-flag"
        );
        let missing = flag_value("--store", None).expect_err("no token at all");
        assert!(missing.contains("--store"), "{missing}");
        let swallowed = flag_value("--store", Some("--interval".to_string()))
            .expect_err("a flag is not a value");
        assert!(swallowed.contains("--store") && swallowed.contains("--interval"), "{swallowed}");
        assert!(is_flag_token("--x") && !is_flag_token("-x") && !is_flag_token("x"));
    }

    /// Positional bins accept exactly as many bare tokens as they document, and one more is a
    /// usage error rather than being dropped on the floor.
    #[test]
    fn extra_positionals_are_rejected_and_a_flaggy_bin_takes_none() {
        assert!(POSITIONAL.triage(&argv(&["/d", "BTC", "1m", "0", "1", "extra"])).is_err());
        assert!(
            FLAGGY.triage(&argv(&["stray"])).is_err(),
            "a bin with no positionals must reject a bare token"
        );
    }

    /// The 341-GB-footgun regression guard: every batch bin's `LogConfig` must default its file
    /// level to `warn`, not vike-log's own `trace` default — see [`log_config`]'s doc. This does
    /// NOT assert the env override still wins; that precedence is `vike_log::file_level_directive`'s
    /// own test (`file_level_precedence_env_over_cfg`), unchanged and untouched here.
    #[test]
    fn log_config_defaults_file_level_to_warn_not_trace() {
        let cfg = log_config("test-bin");
        assert_eq!(cfg.file_level, "warn");
        assert_eq!(cfg.file_prefix, "test-bin");
        // Everything else stays vike-log's own default — this helper narrows file_level and the
        // log DIRECTORY (below), nothing more.
        let default = vike_log::LogConfig::default();
        assert_eq!(cfg.console_level, default.console_level);
        assert_eq!(cfg.file_enabled, default.file_enabled);
        assert!(cfg.dir.is_none(), "the CONFIG layer stays unset — this helper names no override");
    }

    /// …and the DEFAULT log DIRECTORY is the project's, not `<exe_dir>/logs`. Asserted by SHAPE,
    /// because the answer depends on where the harness runs: under `cargo test` a project resolves
    /// (the repo above this crate) and it must end `settings/state/logs`; with no project above the
    /// working directory it is `None`, which is vike-log's `<exe_dir>/logs` last resort — the
    /// pre-repoint behaviour, deliberately kept.
    #[test]
    fn log_config_defaults_the_directory_to_the_project_state_dir() {
        let cfg = log_config("test-bin");
        match &cfg.project_dir {
            Some(dir) => {
                let tail = PathBuf::from(vike_model::state_path::PROJECT_SETTINGS_DIR)
                    .join(vike_model::state_path::STATE_SUBDIR)
                    .join(vike_model::state_path::LOGS_SUBDIR);
                assert!(
                    dir.ends_with(&tail),
                    "{} must end with {} — the one directory the program writes into",
                    dir.display(),
                    tail.display()
                );
            }
            None => { /* no project above the CWD: the <exe_dir>/logs last resort, unchanged */ }
        }
    }

    #[test]
    fn arg_finds_value_after_flag() {
        let args: Vec<String> =
            ["prog", "--store", "/x", "--flag"].iter().map(|s| s.to_string()).collect();
        assert_eq!(arg(&args, "--store"), Some("/x".to_string()));
        assert_eq!(arg(&args, "--missing"), None);
        assert_eq!(arg(&args, "--flag"), None, "trailing flag has no value");
    }

    #[test]
    fn has_flag_is_presence_only() {
        let args: Vec<String> =
            ["prog", "--dry-run", "--store", "/x"].iter().map(|s| s.to_string()).collect();
        assert!(has_flag(&args, "--dry-run"));
        assert!(!has_flag(&args, "--nope"));
        // The distinction from `arg`: a valueless flag followed by another flag must NOT be read
        // as carrying that next token as its value.
        assert_eq!(arg(&args, "--dry-run"), Some("--store".to_string()));
    }

    fn env(pairs: &[(&str, &str)]) -> std::collections::HashMap<String, String> {
        pairs.iter().map(|(k, v)| ((*k).to_string(), (*v).to_string())).collect()
    }

    /// A `repo_default` that exists on no machine — the INSTALLED shape, and the only way a test
    /// running inside the checkout can see any rung below the dev-checkout hinge.
    const NO_CHECKOUT: &str = "/definitely/not/a/real/build/machine/path/market_data/hist";

    #[test]
    fn env_var_wins_over_default_but_loses_to_explicit() {
        let repo = default_store_root();
        // explicit --store beats everything
        assert_eq!(
            resolve(Some("/x/explicit"), &repo, None, &env(&[("VIKE_HIST_STORE", "/y/env")])).root,
            PathBuf::from("/x/explicit")
        );
        // $VIKE_HIST_STORE is honored when there is no --store — THE drift fix: pmxt/databento/
        // tardis previously ignored it and fell through to a CWD-relative default.
        assert_eq!(
            resolve(None, &repo, None, &env(&[("VIKE_HIST_STORE", "/y/env")])).root,
            PathBuf::from("/y/env")
        );
        // neither → the repo-root default, because these tests RUN in the checkout and the
        // dev-checkout hinge outranks the project rung there.
        assert_eq!(resolve(None, &repo, None, &env(&[])).root, repo);
    }

    /// **THIS crate's call site, on the rung a `cargo test` run cannot otherwise reach** — and the
    /// transposition guard for it. `resolve` is the one place this crate assembles the shared
    /// precedence, and the project and per-user rungs are made unmistakably different (a scratch
    /// project vs a fake `$HOME`), so a wiring that swapped them reddens on the value rather than
    /// on a path suffix. Verified by mutation: swapping the two inside
    /// `vike_model::store_path::resolve_store_root_from` turns this red with the `$HOME` path.
    #[test]
    fn the_call_site_reaches_the_project_rung_when_this_box_has_no_checkout() {
        use vike_model::store_path::{
            HOME_VAR, LOCALAPPDATA_VAR, StoreRootRung, XDG_DATA_HOME_VAR,
        };

        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let scratch = std::env::temp_dir().join(format!("vike-backfill-store-{nanos}"));
        let project = scratch.join("proj");
        let fake_home = scratch.join("home");
        std::fs::create_dir_all(project.join("settings")).unwrap();

        let vars = env(&[
            (HOME_VAR, fake_home.to_str().unwrap()),
            (XDG_DATA_HOME_VAR, fake_home.to_str().unwrap()),
            (LOCALAPPDATA_VAR, fake_home.to_str().unwrap()),
        ]);
        let got = resolve(None, Path::new(NO_CHECKOUT), Some(&project), &vars);
        let user = vike_model::store_path::user_data_dir_from_vars(&vars).unwrap();
        let _ = std::fs::remove_dir_all(&scratch);

        assert_eq!(
            got.root,
            project.join("market_data").join("hist"),
            "the PROJECT's own data folder"
        );
        assert_eq!(got.rung, StoreRootRung::Project);
        assert_ne!(got.root, user, "the two rungs must be distinguishable in this fixture");
    }

    /// …and with no project above the working directory, the SAME call site falls to the per-user
    /// directory. Together with the test above this pins the ORDER, not just one answer.
    #[test]
    fn the_call_site_falls_to_the_user_dir_without_a_project() {
        use vike_model::store_path::{
            HOME_VAR, LOCALAPPDATA_VAR, StoreRootRung, XDG_DATA_HOME_VAR,
        };

        let vars = env(&[
            (HOME_VAR, "/home/u"),
            (XDG_DATA_HOME_VAR, "/xdg"),
            (LOCALAPPDATA_VAR, "C:\\Users\\u\\AppData\\Local"),
        ]);
        // `cwd = None` — a binary that could not read its own working directory. No project can be
        // resolved from it, so the per-user directory is the only remaining answer.
        let got = resolve(None, Path::new(NO_CHECKOUT), None, &vars);
        assert_eq!(got.root, vike_model::store_path::user_data_dir_from_vars(&vars).unwrap());
        assert_eq!(got.rung, StoreRootRung::UserDir);
    }

    /// **B4 at this call site:** `$VIKE_SETTINGS_DIR` relocates the project, and the store's default
    /// travels with it — out of the SAME map the bins already collect, so no bin can forget it.
    #[test]
    fn the_call_site_honours_the_settings_dir_override() {
        let vars = env(&[("VIKE_SETTINGS_DIR", "/relocated/settings")]);
        let got = resolve(None, Path::new(NO_CHECKOUT), Some(Path::new("/somewhere/else")), &vars);
        assert_eq!(got.root, PathBuf::from("/relocated").join("market_data").join("hist"));
    }

    /// The public entry point reads `VIKE_HIST_STORE` out of the SAME map it forwards, so a bin
    /// that collected the process environment once gets the historical precedence exactly.
    #[test]
    fn store_root_reads_the_override_out_of_the_supplied_map() {
        assert_eq!(
            store_root(None, &env(&[("VIKE_HIST_STORE", "/y/env")])),
            PathBuf::from("/y/env")
        );
        assert_eq!(
            store_root(Some("/x/explicit"), &env(&[("VIKE_HIST_STORE", "/y/env")])),
            PathBuf::from("/x/explicit")
        );
    }

    /// **Behaviour preservation across the map lift.** A dev checkout (this repo, whose root the
    /// compile-time `default_store_root` names) resolves to `<repo>/market_data/hist` regardless of the
    /// platform trio — the compatibility hinge in `vike_model::store_path::resolve_store_root`.
    /// That is the path every the CI box backfill takes today, asserted on a unix-shaped AND a
    /// windows-shaped environment map.
    #[test]
    fn a_dev_checkout_is_unaffected_by_the_platform_trio_on_either_platform_shape() {
        // Spelled through `vike_model::store_path`'s constants, not as bare literals: that crate is
        // the ONE place these three variable names appear, and a fixture that re-spelled them here
        // would put an incidental `vike-backfill` row back on the settings registry for a read this
        // crate no longer performs.
        use vike_model::store_path::{HOME_VAR, LOCALAPPDATA_VAR, XDG_DATA_HOME_VAR};
        let unix = env(&[(XDG_DATA_HOME_VAR, "/xdg"), (HOME_VAR, "/home/u")]);
        let windows =
            env(&[(LOCALAPPDATA_VAR, "C:\\Users\\u\\AppData\\Local"), (HOME_VAR, "C:\\Users\\u")]);
        assert_eq!(store_root(None, &unix), default_store_root());
        assert_eq!(store_root(None, &windows), default_store_root());
    }

    /// The pace file's precedence, and the one non-obvious rule: a blank env var is an UNSET env
    /// var, not a request to write into the CWD.
    ///
    /// The DEFAULT row is the project's `settings/state/pace.json` — the settings-unification
    /// repoint — and every override above it still wins over that, which is the property that keeps
    /// a read-only store mount and a deliberately-shared record working.
    #[test]
    fn pace_book_path_prefers_the_project_state_dir_and_honors_the_override() {
        let store = PathBuf::from("/y/store");
        let state = PathBuf::from("/p/settings/state");
        assert_eq!(
            resolve_pace_book_path(None, None, Some(state.clone()), &store),
            state.join("pace.json"),
            "the default is the project's state directory, not the store root"
        );
        assert_eq!(
            resolve_pace_book_path(
                None,
                Some("/z/shared.json".to_string()),
                Some(state.clone()),
                &store
            ),
            PathBuf::from("/z/shared.json"),
            "$VIKE_PACE_BOOK still beats the project default"
        );
        assert_eq!(
            resolve_pace_book_path(
                Some("/x/explicit.json"),
                Some("/z/env.json".to_string()),
                Some(state.clone()),
                &store
            ),
            PathBuf::from("/x/explicit.json"),
            "an explicit caller value beats the env var, like --store does"
        );
        for blank in ["", "   ", "\t"] {
            assert_eq!(
                resolve_pace_book_path(None, Some(blank.to_string()), Some(state.clone()), &store),
                state.join("pace.json"),
                "a blank override must not resolve to a CWD-relative path"
            );
        }
    }

    /// The public entry point reads `VIKE_PACE_BOOK` out of the SAME map it is handed, the twin of
    /// `store_root_reads_the_override_out_of_the_supplied_map` above and the seam the map lift
    /// created: the pure `resolve_pace_book_path` tests cover the precedence but say nothing about
    /// WHICH key feeds its `env` argument.
    ///
    /// Asserted through the override rows ONLY, deliberately: those short-circuit before
    /// `pace_book_path` consults the working directory, so this test is deterministic no matter
    /// where the harness runs it. The default rows are the pure function's job.
    ///
    /// Spelled through `PACE_BOOK_VAR` rather than as a bare literal so this fixture cannot be
    /// mistaken for a second read site by the settings-registry scanner.
    #[test]
    fn pace_book_path_reads_the_override_out_of_the_supplied_map() {
        let store = PathBuf::from("/y/store");
        assert_eq!(
            pace_book_path(None, &env(&[(PACE_BOOK_VAR, "/z/shared.json")]), &store),
            PathBuf::from("/z/shared.json"),
            "the key the map is asked for must be the one the operator exports"
        );
        assert_eq!(
            pace_book_path(
                Some("/x/explicit.json"),
                &env(&[(PACE_BOOK_VAR, "/z/shared.json")]),
                &store
            ),
            PathBuf::from("/x/explicit.json"),
            "an explicit caller value still beats the map, like --store does"
        );
    }

    /// The last row: a binary with NO project above its working directory keeps the pre-repoint
    /// behaviour exactly — `<store_root>/pace.json`, the path every shipped backfill wrote until
    /// now. Without this the repoint would resolve to nothing for an installed binary.
    #[test]
    fn pace_book_path_falls_back_to_the_store_root_with_no_project() {
        let store = PathBuf::from("/y/store");
        assert_eq!(
            resolve_pace_book_path(None, None, None, &store),
            store.join("pace.json"),
            "no project ⇒ the historical store-root default"
        );
        assert_eq!(
            resolve_pace_book_path(None, Some("/z/shared.json".to_string()), None, &store),
            PathBuf::from("/z/shared.json"),
            "and the override still wins there too"
        );
        for blank in ["", "   ", "\t"] {
            assert_eq!(
                resolve_pace_book_path(None, Some(blank.to_string()), None, &store),
                store.join("pace.json"),
            );
        }
    }

    #[test]
    fn parse_symbol_pairs_maps_and_defaults_bare() {
        assert_eq!(
            parse_symbol_pairs("SPX=^GSPC, VIX=^VIX ,ETH").unwrap(),
            vec![
                ("SPX".to_string(), "^GSPC".to_string()),
                ("VIX".to_string(), "^VIX".to_string()),
                ("ETH".to_string(), "ETH".to_string()),
            ]
        );
        assert!(parse_symbol_pairs("  , ,").is_err(), "no entries → Err");
    }
}
