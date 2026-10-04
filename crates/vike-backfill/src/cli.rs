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
    /// `CARGO_PKG_NAME`, which is `vike-backfill` for all 10 bins and answers the wrong question.
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
/// which in a checkout is `target/debug/logs/…` and vanishes with `cargo clean`. The WALK is
/// `vike_model::paths::state_path`'s (pure), the working directory is read here, and `$VIKE_LOG_DIR` still
/// wins over the result (`vike_log::resolve_log_dir`).
/// A bin run with no project above it keeps the historical `<exe_dir>/logs`.
pub fn log_config(file_prefix: impl Into<String>) -> vike_log::LogConfig {
    vike_log::LogConfig {
        file_prefix: file_prefix.into(),
        file_level: "warn".to_string(),
        project_dir: std::env::current_dir()
            .ok()
            .and_then(|cwd| vike_model::paths::state_path::project_log_dir(&cwd)),
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
///
/// # ⚠ It LOOKS like a duplicate of `vike_analytics::binutil::arg`, and unifying them was MEASURED
/// and refused (2026-09-20)
///
/// It is not a duplicate: that one also accepts the INLINE `--flag=value` spelling, this one only
/// `--flag value`. The difference is unreachable, though, and that is the interesting part —
/// [`CliSpec::triage`] answers any unmatched `-`-prefixed token with `unknown argument`, so
/// `--store=/path` never reaches either function on any bin in this crate. [`has_flag`] below IS a
/// true duplicate, body for body.
///
/// So unification is behaviour-neutral and still not worth it: `vike-analytics` brings **chrono,
/// indexmap, libm and rand**, none of which this crate has, to delete two one-line bodies. A
/// collector's dependency surface is part of the audit surface every adapter's is — the
/// root `Cargo.toml`'s rationale rule — and four crates for four lines is the wrong side of it.
///
/// What a NEW caller should do is the opposite of what these two say about themselves: name the
/// LOWER home, as `crates/vike-poly-research` did because it already linked analytics for the
/// sweep — until #2046 deleted that crate on 2026-09-20.
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
/// ⚠ The `std::env::current_dir()` below is a DIFFERENT kind of read: it names no variable, so no
/// operator can export it, no registry row can describe it and the settings gate — which keys on
/// `env::var`/`env::var_os` calls and on env-shaped map keys — has nothing to see either way, so
/// injecting it would retire no row and buy no override. `$VIKE_SETTINGS_DIR` is the override that
/// DOES pin the project, and [`vike_model::paths::store_path::resolve_store_root_from`] honours it out of
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
/// Delegates to [`vike_model::paths::store_path::resolve_store_root_from`] — ONE precedence for every
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
) -> vike_model::paths::store_path::StoreRoot {
    // Always `Some`: this crate is NOT a release asset (`release.yml` builds no `vike-backfill`
    // binary, and `xtask::ci::tables` excludes it from CI), so the compile-time checkout path
    // never reaches a public download and the dev-checkout rung stays in every profile. The
    // shipped call sites — `vike-desktop`'s `studio_store_root`, `vike-backtest`'s `binutil`,
    // `vike-datahub`'s `run` — pass it under `cfg(debug_assertions)` only, and
    // `vike_model::paths::store_path`'s module doc (rung 4) says why the two differ.
    vike_model::paths::store_path::resolve_store_root_from(
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
///   mounted beside the project. See [`vike_model::paths::state_path::PROJECT_TMP_DIR`] for the full
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
/// DIFFERENT kind of read [`store_root`] documents: it names no variable, so no operator can
/// export it and no registry row can describe it.
///
/// ⚠ The `$VIKE_SETTINGS_DIR` lookup goes through `vike_model::paths::state_path::SETTINGS_DIR_ENV` rather
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
    let root = vike_model::paths::state_path::project_tmp_dir_from(
        vars.get(vike_model::paths::state_path::SETTINGS_DIR_ENV).map(String::as_str),
        &cwd,
    )
    .unwrap_or_else(|| cwd.join(vike_model::paths::state_path::PROJECT_TMP_DIR));

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

#[path = "cli_tests.rs"]
#[cfg(test)]
mod cli_tests;
