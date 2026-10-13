//! Shared backfill-CLI harness for the `*_backfill` bins — the small store/log/credential/symbol
//! utilities every bin re-implemented (finding F19). Folding them here removes the duplication AND
//! the shipped store drift, where half the collectors defaulted CWD-relative. [`store_root`] is the
//! single resolution law and every bin goes through it.
//!
//! **Reading the command line is NOT done here.** Each bin declares its arguments as a
//! `#[derive(clap::Parser)]` struct beside its `main` and reads them through [`parse`] — the shared
//! rules (decision 0155) are `vike_util::cli`'s: `--flag value` and `--flag=value`, an unknown flag
//! refused with a suggestion, a repeated flag refused, a blank value refused, `-h/--help` and
//! `-V/--version` on stdout with exit 0, any other usage error on stderr with exit 2. What stays in a
//! bin AFTER the parse is what is not argv: cross-flag rules, [`RETIRED_VARIABLES`], credentials.
//!
//! NOTE the one intentional sibling kept OUT of scope (it cannot depend on vike-backfill —
//! nothing may, it pulls every bridge crate): `vike-backtest/src/bin/backtest.rs` keeps its own
//! `store_root` copy. And `ingest_bench_bars` resolves a *different* root
//! (`market_data/bench_hist`, no env override) on purpose, so it is not a caller here.

use std::path::{Path, PathBuf};

use vike_util::cli::{CliExit, Program};

/// Read the process's own command line as a `T` for the bin called `bin`.
///
/// ⚠ **Call this FIRST in `main` — before `vike_log::init`, before opening a store, before any
/// network call.** Answering a question about the command line (`--help`, `--version`, a usage
/// error) must not create a log file, create a store directory, spawn a subprocess or ingest
/// anything. `crates/vike-backfill/tests/help_cli.rs` asserts `--help` does no work.
///
/// `-V` prints `<bin> <crate version>`, the shape every `--version` on the box prints
/// (`git version 2.x`): the NAME is the bin's, the VERSION is the crate's, which is what these bins
/// ship as. (Not `CARGO_PKG_NAME`, which is `vike-backfill` for every bin here and answers the wrong
/// question.)
pub fn parse<T: clap::Parser>(bin: &str) -> T {
    let version_line = version_line(bin);
    Program::new(bin, &version_line).parse()
}

/// [`parse`] over an explicit argument list (WITHOUT the program name), returning the outcome as a
/// value instead of printing and exiting: the seam a bin's own unit tests drive.
///
/// # Errors
/// The [`CliExit`] a process would have printed: `--help`/`--version` (code 0) or a usage error
/// (code 2).
pub fn try_parse_from<T, I, A>(bin: &str, args: I) -> Result<T, CliExit>
where
    T: clap::Parser,
    I: IntoIterator<Item = A>,
    A: Into<std::ffi::OsString> + Clone,
{
    let version_line = version_line(bin);
    Program::new(bin, &version_line).try_parse_from(args)
}

fn version_line(bin: &str) -> String {
    format!("{bin} {}", env!("CARGO_PKG_VERSION"))
}

/// The FILE level a bin in this crate compiles in: `warn`, not vike-log's own `trace`. These bins
/// run long and unattended, driving chatty deps (DataFusion/arrow/hyper) whose trace-level rows no
/// console-level knob can silence (`RUST_LOG` filters the console layer only — see
/// `vike_log::file_level_directive`'s doc). Measured cost of a `trace` file: one hour of
/// `pmxt_backfill` wrote 6.4 GB, and a long range once wrote 341 GB and nearly filled the disk
/// hosting a live trading node.
pub const BATCH_FILE_LEVEL: &str = "warn";

/// Build the `vike_log::LogConfig` every batch/backfill bin in this crate passes to
/// `vike_log::init`. Call it right after [`parse`] and before anything else; `vars` is
/// the bin's one `std::env::vars()` sweep.
///
/// ⚠ **It EXITS the process (status 2) when a [`RETIRED_VARIABLES`] name is set**, before anything
/// is read or opened — the refusal [`store_root`] applies too, brought forward so a retired
/// variable stops the bin before its log file exists.
///
/// **The file level is the `preferences.log_file_level` row** (decision 0111, verdict 3), read
/// through `vike_config::log_file_level` before the subscriber exists: `$VIKE_SETTINGS_DIR`, else the
/// project walk from the working directory, best-effort. No project, no database or no such row
/// keeps [`BATCH_FILE_LEVEL`]; a store that cannot answer keeps it too and says why on stderr. So
/// a one-off debug run asks for `debug` by writing the row of the project it runs in.
///
/// It also sets the DEFAULT log DIRECTORY — `<project>/settings/state/logs` off that same
/// resolution, beside every other file the program writes — rather than leaving it to vike-log's
/// `<exe_dir>/logs` last resort, which in a checkout is `target/debug/logs/…` and vanishes with
/// `cargo clean`. `$VIKE_LOG_DIR` still wins over it (`vike_log::resolve_log_dir`). A bin run with
/// no project keeps `<exe_dir>/logs`.
pub fn log_config(
    file_prefix: impl Into<String>,
    vars: &std::collections::HashMap<String, String>,
) -> vike_log::LogConfig {
    let file_prefix = file_prefix.into();
    if let Err(refusal) = refuse_retired_variables(vars) {
        eprintln!("{file_prefix}: {refusal}");
        std::process::exit(2);
    }
    let cwd = std::env::current_dir().ok();
    let read = vike_config::log_file_level(vars, cwd.as_deref());
    // Straight to stderr: no subscriber exists yet, and this is the line that explains the level.
    if let Some(line) = read.unread_line(BATCH_FILE_LEVEL) {
        eprintln!("{file_prefix}: {line}");
    }
    batch_log_config(file_prefix, &read)
}

/// [`log_config`]'s pure half: the `LogConfig` for what `vike_config::log_file_level` read.
fn batch_log_config(file_prefix: String, read: &vike_config::LogFileLevel) -> vike_log::LogConfig {
    vike_log::LogConfig {
        file_prefix,
        file_level: read.level_or(BATCH_FILE_LEVEL),
        project_dir: read.log_home.clone(),
        ..Default::default()
    }
}

/// The process variables these bins read until decision 0111, each with what replaced it. The
/// `vike-config` table (`vike_config::REMOVED_ENV`) carries the same names for every booting root.
pub const RETIRED_VARIABLES: [(&str, &str); 3] = [
    ("VIKE_HIST_STORE", "pass --store DIR instead"),
    ("VIKE_API_BASE", "pass --api-base URL instead (vikedata_backfill)"),
    (
        "VIKE_LOG_FILE_LEVEL",
        "the rolling log's level is the preferences.log_file_level row of the project the bin runs \
         in (`vike-cli config set preferences.log_file_level <level>`)",
    ),
];

/// Refuse while a [`RETIRED_VARIABLES`] name is set — by NAME, never by value. A blank value is
/// unset, the rule every booting root applies to the same names.
///
/// # Errors
/// The refusal text, naming every set variable and its replacement.
pub fn refuse_retired_variables(
    vars: &std::collections::HashMap<String, String>,
) -> Result<(), String> {
    let set: Vec<String> = RETIRED_VARIABLES
        .iter()
        .filter(|(var, _)| vars.get(*var).is_some_and(|v| !v.trim().is_empty()))
        .map(|(var, instead)| format!("  {var} is set, but no backfill reads it: {instead}"))
        .collect();
    if set.is_empty() {
        return Ok(());
    }
    Err(format!(
        "refusing to start — decision 0111 retired these process variables, and a set one would be \
         ignored in silence:\n{}\nUnset them and run again. Nothing was read and nothing was written.",
        set.join("\n")
    ))
}

/// Resolve the hist-store root with the canonical precedence: explicit `--store` → the repo-root
/// default `<repo>/market_data/hist` when this box still has that checkout → the PROJECT's own
/// `<project>/market_data/hist` → the per-user `…/vike-data`. Every backfill bin honours the same
/// default. No variable is a rung: decision 0111 retired `VIKE_HIST_STORE`, and the one row these
/// bins read from the settings database is the log level ([`log_file_level`]'s), so `--store` is how
/// one names a store outright.
///
/// [`log_file_level`]: vike_config::log_file_level
///
/// ⚠ **It EXITS the process (status 2) when a [`RETIRED_VARIABLES`] name is set**, before anything
/// is opened: these bins boot nothing, so `vike_config::REMOVED_ENV`'s startup refusal never reaches
/// them, and a leftover `VIKE_HIST_STORE` would otherwise send a backfill into the DEFAULT store in
/// silence. Every caller is a bin's `main`, and this is the one step all of them take.
///
/// PURE: `vars` is the process environment as the CALLING BIN collected it
/// (`std::env::vars().collect()`), per the settings-registry rule that libraries take configuration
/// as parameters and only binaries read the process environment. This function used to read the
/// `XDG_DATA_HOME`/`HOME`/`LOCALAPPDATA` platform trio itself from a LIBRARY file, so all four carried `Layer::Library` rows on the registry's STEP-2
/// work-list even though all sixteen call sites are bins in this same crate (fifteen files).
///
/// ⚠ The `std::env::current_dir()` below is a DIFFERENT kind of read: it names no variable, so no
/// operator can export it, no registry row can describe it and the settings gate — which keys on
/// `env::var`/`env::var_os` calls and on env-shaped map keys — has nothing to see either way, so
/// injecting it would retire no row and buy no override. `$VIKE_SETTINGS_DIR` is the override that
/// DOES pin the project, and [`vike_paths::store_path::resolve_store_root_from`] honours it out of
/// the map this function is already handed — so no bin gains an argument and no rung is lost.
///
/// **The resolution is LOGGED**, root and rung both. A store does not merge: if the default moves,
/// the old store is simply no longer read and a backfill reports zero rows into a fresh empty one —
/// indistinguishable from "the venue returned nothing" until somebody goes looking.
pub fn store_root(
    explicit: Option<&str>,
    vars: &std::collections::HashMap<String, String>,
) -> PathBuf {
    if let Err(refusal) = refuse_retired_variables(vars) {
        eprintln!("{refusal}");
        std::process::exit(2);
    }
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

/// The WIRING used by [`store_root`], with `repo_default` and `cwd` injected so a test can reach the
/// rungs BELOW the
/// dev-checkout hinge, which a `cargo test` run can never otherwise see (the compile-time repo path
/// always exists while testing, so the hinge always fires).
///
/// Delegates to [`vike_paths::store_path::resolve_store_root_from`] — ONE precedence for every
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
) -> vike_paths::store_path::StoreRoot {
    // Always `Some`: this crate is NOT a release asset (`release.yml` builds no `vike-backfill`
    // binary, and `xtask::ci::tables` excludes it from CI), so the compile-time checkout path
    // never reaches a public download and the dev-checkout rung stays in every profile. The
    // shipped call sites — `vike-desktop`'s `studio_store_root`, `vike-backtest`'s `binutil`,
    // `vike-datahub`'s `run` — pass it under `cfg(debug_assertions)` only, and
    // `vike_paths::store_path`'s module doc (rung 4) says why the two differ.
    vike_paths::store_path::resolve_store_root_from(
        explicit.map(PathBuf::from),
        None,
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
///   mounted beside the project. See [`vike_paths::state_path::PROJECT_TMP_DIR`] for the full
///   argument; `crates/vike-ops/tests/hygiene/system_temp_gate.rs` is the ratchet.
/// * **The retention.** Nothing removed a staged export, ever. Measured on the CI box on 2026-08-23:
///   26,851 leaked scratch directories totalling 215 GB, about 70% of that filesystem. A fixed name
///   made it worse rather than better on a shared box — whichever user created
///   `/tmp/pmxt_backfill` first OWNED it, and every later run as the other user failed
///   `PermissionDenied` forever.
///
/// So this returns a root, and the caller allocates inside it with
/// [`vike_paths::scratch::ScratchDir`], which removes what it created — including on the panic
/// path.
///
/// # What this call also DOES, deliberately
///
/// It [`sweep`](vike_paths::scratch::sweep)s. A guard cleans up only what the running process
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
/// ⚠ The `$VIKE_SETTINGS_DIR` lookup goes through `vike_paths::state_path::SETTINGS_DIR_ENV` rather
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
/// precedent, filed in `crates/vike-ops/tests/docs/citation_gate/dead_paths.rs`'s `DEAD_PATH_EXCEPTIONS`) — and it
/// is still inside a directory the operator chose, which the system temp directory
/// is not.
pub fn scratch_root(vars: &std::collections::HashMap<String, String>) -> PathBuf {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let root = vike_paths::state_path::project_tmp_dir_from(
        vars.get(vike_paths::state_path::SETTINGS_DIR_ENV).map(String::as_str),
        &cwd,
    )
    .unwrap_or_else(|| cwd.join(vike_paths::state_path::PROJECT_TMP_DIR));

    let swept =
        vike_paths::scratch::sweep(&root, Some(vike_paths::scratch::DEFAULT_MAX_SCRATCH_ENTRIES));
    tracing::info!(
        scratch = %root.display(),
        found = swept.found,
        removed = swept.removed,
        failed = swept.failed,
        "scratch root resolved and swept"
    );
    root
}

/// **The DECLARED credentials of one vendor bin** — the credential store opened out of the bin's
/// one environment sweep, read under `keys`, its findings LOGGED. Every vendor bin in this crate
/// that needs an API key calls this, once, after `vike_log::init`.
///
/// The store is opened by [`vike_secrets::open_credential_store`], the one door for a root that
/// runs no boot — `$VIKE_SETTINGS_DIR` (else the workspace walk, with the relative `settings/` last
/// resort) and `$VIKE_NO_LIVE_KEYS` parsed strictly, both taken out of `vars`.
///
/// A store that EXISTS and would not open degrades to an EMPTY scope (every declared name reads
/// as absent, an undeclared one still `NotDeclared`) and is logged at `error` by
/// [`vike_secrets::ScopedSecrets::report`], which also logs the permission finding and, under
/// `VIKE_NO_LIVE_KEYS=true`, how many declared names the scope withheld.
///
/// # Errors
/// [`vike_secrets::ScopeError`] when `$VIKE_NO_LIVE_KEYS` is set to anything but `true` or
/// `false`: the bin refuses to start on it, printing the message as is.
pub fn scoped_credentials(
    vars: &std::collections::HashMap<String, String>,
    keys: &vike_secrets::KeyScope,
) -> Result<vike_secrets::ScopedSecrets, vike_secrets::ScopeError> {
    let store = vike_secrets::open_credential_store(vars)?;
    let scoped = match store.read(keys) {
        Ok(scoped) => scoped,
        Err(e) => vike_secrets::ScopedSecrets::unreadable(keys, store.scope(), &e),
    };
    scoped.report();
    Ok(scoped)
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

/// `YYYY-MM-DD` labels for every UTC day in `[from, to]` inclusive; empty on an unparsable bound or
/// `from` after `to` (mirrors `vike_model::time::hours_in_range`'s empty-on-bad-input contract).
/// Both Polymarket-archive bins (`vike_archive_backfill` and `events_api_backfill`) walk their
/// `--from/--to` range through it — they each carried a byte-identical copy.
pub fn day_labels(from: &str, to: &str) -> Vec<String> {
    let (Ok((fy, fm, fd)), Ok((ty, tm, td))) =
        (vike_model::parse_ymd(from), vike_model::parse_ymd(to))
    else {
        return Vec::new();
    };
    vike_model::time::days_in_range((fy, fm, fd), (ty, tm, td))
        .into_iter()
        .map(|(y, m, d)| format!("{y:04}-{m:02}-{d:02}"))
        .collect()
}

#[path = "cli_tests.rs"]
#[cfg(test)]
mod cli_tests;
