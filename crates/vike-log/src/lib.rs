//! Workspace logging init. Builds a layered `tracing` subscriber: a console layer on stderr
//! (formatter per [`ConsoleFormat`]) plus a non-blocking JSON daily-rolling file layer. Depends on
//! NO domain crate. Only binaries call [`init`]; libraries use the `tracing` facade and tests call
//! [`test_init`]. Design: docs/superpowers/specs/2026-07-06-tracing-logging-design.md.
//!
//! **Levels.** Console: `RUST_LOG` > `VIKE_LOG` > [`LogConfig::console_level`] (default `info`).
//! File: `VIKE_LOG_FILE_LEVEL` > [`LogConfig::file_level`] (default `trace`); the console knobs
//! never reach the file. Each directive then gains this crate's NARROWING target pins:
//! [`CREDENTIAL_BEARING_TARGETS`] on both layers, [`HIGH_VOLUME_TARGETS`] on the file,
//! [`NOISY_TARGETS`] on the console. A binary's [`LogConfig::file_target_pins`] go the other way:
//! they only RAISE ([`file_level_directive_with_pins`]).
//!
//! **Where the file lands** is [`resolve_log_dir`]'s four-layer answer: `$VIKE_LOG_DIR` >
//! [`LogConfig::dir`] > [`LogConfig::project_dir`] > `<exe_dir>/logs`.
//!
//! **Retention and prefix.** Files rotate DAILY and the newest [`DEFAULT_MAX_LOG_FILES`] are kept,
//! pruned by file-name prefix. The prefix defaults to the executable's own name
//! ([`effective_file_prefix`]), so binaries sharing one log directory never prune each other.
//!
//! **Panics.** [`init_with_reload`] also installs, once, a panic hook that logs every panic
//! (`target: "panic"`, thread name, location, payload) before the previous hook runs, so a panic on a
//! worker thread reaches the file layer; [`test_init`] does not.
//!
//! **The file layer is BEST-EFFORT; the console layer is not.** A destination that cannot be opened
//! drops the file layer with one line on stderr instead of panicking, and logging otherwise works:
//! logging may degrade, it may not be what stops a trading tool. `tests/unwritable_log_dir.rs` is
//! the gate.

// Build-graph edge only, not an API: see this crate's Cargo.toml comment on `idna_adapter`.
use idna_adapter as _;

use std::path::{Path, PathBuf};

/// Resolve the log directory. FOUR layers, highest first:
///
/// | layer | comes from | typical value |
/// |---|---|---|
/// | `env_dir` | `$VIKE_LOG_DIR` | whatever the operator exported |
/// | `cfg_dir` | [`LogConfig::dir`] — a CONFIG FILE's `log_dir`, or a `--log-dir` flag | `/var/log/vike` |
/// | `project_dir` | [`LogConfig::project_dir`] — the project's own log directory | `<project>/settings/state/logs` |
/// | (last resort) | `exe_dir` | `<exe_dir>/logs` |
///
/// Pure, and the whole precedence lives here, so there is one answer to "why did the log land
/// there". `project_dir` sits BELOW `cfg_dir` deliberately: a value a human wrote (a config file, a
/// flag) beats a location the program derived. The last resort is `target/debug/logs` in a
/// checkout, which `cargo clean` deletes.
///
/// Every layer but the last arrives as a PARAMETER because this crate is deliberately the bottom
/// layer and depends on no other crate — it cannot walk for a project root, and per the settings
/// rule it should not: the BINARY resolves it (`vike_model::paths::state_path::project_log_dir`)
/// and hands it over; `None` (no project above the working directory) keeps the last resort.
/// That "cannot" is held by `crates/vike-ops/tests/architecture/layer_gate/vike_free.rs`'s
/// `every_vike_free_crate_names_no_vike_crate`.
pub fn resolve_log_dir(
    env_dir: Option<&str>,
    cfg_dir: Option<&Path>,
    project_dir: Option<&Path>,
    exe_dir: &Path,
) -> PathBuf {
    if let Some(d) = env_dir {
        return PathBuf::from(d);
    }
    if let Some(d) = cfg_dir {
        return d.to_path_buf();
    }
    if let Some(d) = project_dir {
        return d.to_path_buf();
    }
    exe_dir.join("logs")
}

/// Pick the FILE layer's `EnvFilter` directive string: `VIKE_LOG_FILE_LEVEL` env > `cfg_level`
/// (an empty or blank env value falls through), plus the narrowing pins. Pure.
///
/// The file layer defaults to `trace` and captures the `log`→`tracing` bridge, so a batch tool that
/// drives a chatty dependency (DataFusion/arrow/hyper) writes an enormous JSON file that **no
/// console-level setting can turn down** — `RUST_LOG`/`VIKE_LOG` only filter the console. Measured:
/// one hour of `pmxt_backfill` wrote **6.4 GB**, and a long range once wrote 341 GB and nearly
/// filled the disk hosting a live trading node. `VIKE_LOG_FILE_LEVEL=warn` (or `off`) is the off
/// switch that keeps the file useful without the per-row firehose.
pub fn file_level_directive(env_level: Option<&str>, cfg_level: &str) -> String {
    let base = match env_level.map(str::trim).filter(|s| !s.is_empty()) {
        Some(l) => l.to_string(),
        None => cfg_level.to_string(),
    };
    with_volume_target_pins(&with_credential_target_pins(&base))
}

/// [`file_level_directive`], plus caller-supplied targets the global level may not SILENCE.
///
/// The inverse of the narrowing pins: each `(target, level)` RAISES one record that must survive
/// the level an operator chose (`vike_tradehub::audit::FILE_PIN`, the control audit trail, under a
/// daemon unit's `VIKE_LOG_FILE_LEVEL=warn`). The BINARY supplies them: the target belongs to a
/// crate this one may not depend on, so the crate that emits the line declares it.
///
/// - **A pin may only ever RAISE.** One whose level is not MORE verbose than the global level is
///   dropped: at `trace`, pinning a target to `info` would quieten it.
/// - **An EXPLICIT mention wins.** A directive that already names the target
///   (`VIKE_LOG_FILE_LEVEL=warn,vike_tradehub::audit=debug`) is left alone. ⚠ That is also the ONE
///   way to turn a pinned target down: changing the GLOBAL level (a `deploy/*.service`
///   `Environment=` line, a hot `SetSetting`) cannot reach it.
/// - **An unreadable global is left ALONE.** A directive with no bare level (`ureq=trace`) states
///   no global to compare against, so no pin is added; such a directive silences a pinned target
///   too, and no shipped unit writes one.
///
/// ⚠ **`off` is not an exemption**: it is an operator-set global like any other, and the failure
/// this closes is an environment value silencing a security record. A binary that wants no file at
/// all sets [`LogConfig::file_enabled`].
pub fn file_level_directive_with_pins(
    env_level: Option<&str>,
    cfg_level: &str,
    pins: &[(String, String)],
) -> String {
    let base = file_level_directive(env_level, cfg_level);
    if pins.is_empty() {
        return base;
    }
    // The bare global level = the directive with no `target=` part.
    let global = base.split(',').map(str::trim).find(|d| !d.contains('='));
    let Some(global_rank) = global.and_then(level_rank) else {
        return base;
    };
    let mut out = base;
    for (target, level) in pins {
        // RAISE ONLY: a pin no more verbose than the global changes nothing it does not narrow.
        if level_rank(level).is_none_or(|pin| pin <= global_rank) {
            continue;
        }
        // "already named" = the directive mentions `<target>=`; a bare level like `warn` does not.
        if !out.split(',').any(|d| d.trim().starts_with(&format!("{target}="))) {
            out.push_str(&format!(",{target}={level}"));
        }
    }
    out
}

/// Per-FRAME log targets, pinned out of the FILE's default `trace`, with the level each keeps.
///
/// Chosen for VOLUME: a render loop emits at frame rate, so `trace` here is a disk filling for as
/// long as a window is open. Measured on a default desktop install: 5.8 GB in 4h10m, nearly all of
/// it these targets, and [`LogConfig::file_max_files`] cannot help because one day IS the 5.8 GB.
///
/// `info`, not `warn`/`off`: wgpu's one-time adapter and device lines are what a GPU bug report
/// needs, and a device-lost or validation error still reaches the file at `warn`/`error`.
///
/// ⚠ `eframe::native` is the whole SUBTREE on purpose: its per-frame lines come from several
/// modules (`run`, `wgpu_integration`), so a one-module pin misses most of them. `eframe`'s other
/// modules keep their levels.
pub const HIGH_VOLUME_TARGETS: &[(&str, &str)] =
    &[("wgpu_core", "info"), ("wgpu_hal", "info"), ("naga", "info"), ("eframe::native", "info")];

/// Append a pin for every [`HIGH_VOLUME_TARGETS`] entry the directive does not already name. FILE
/// layer only. The guards are [`with_credential_target_pins`]'s: an EXPLICIT mention wins
/// (`VIKE_LOG_FILE_LEVEL=wgpu_core=trace` still gets the firehose), and a pin may only NARROW.
fn with_volume_target_pins(base: &str) -> String {
    let global = base.split(',').map(str::trim).find(|d| !d.contains('='));
    let Some(global_rank) = global.and_then(level_rank) else {
        return base.to_string();
    };
    let mut out = base.to_string();
    for (target, level) in HIGH_VOLUME_TARGETS {
        if level_rank(level).is_none_or(|pin| global_rank <= pin) {
            continue;
        }
        if !base.split(',').any(|d| d.trim().starts_with(&format!("{target}="))) {
            out.push_str(&format!(",{target}={level}"));
        }
    }
    out
}

/// Third-party log targets that print CREDENTIALS, pinned to `info` on BOTH layers.
///
/// Not chosen for volume: each writes a live secret at a level the file layer enables BY DEFAULT,
/// below every app-level redaction (the HTTP client logs the URL it was handed):
///
/// - **`ureq`** logs `"{method} {DebugUri}"`, and `DebugUri` prints the path+query only when the
///   `log` level is `Trace`, so our `trace` default defeats its redaction. Headers are
///   allowlist-redacted, but the URL is enough: the **Telegram bot token is in the URL PATH**, and
///   Finnhub/FMP keys are query parameters.
/// - **`tungstenite`** has no redaction: `connect_to_some` logs the FULL uri at `debug!`, and the
///   client handshake logs the ENTIRE raw upgrade request (path plus every header) at `trace!`. The
///   binance **listenKey** is a path segment of the user-data WS URL.
///
/// ⚠ `info`, not `debug`, because of `connect_to_some`: `debug` still leaks. A DENYLIST, so not a
/// complete defence: a new dependency that logs URLs leaks again. The durable half is not putting
/// secrets in URLs.
pub const CREDENTIAL_BEARING_TARGETS: &[&str] = &["ureq", "tungstenite"];

/// Append a `target=info` pin for every [`CREDENTIAL_BEARING_TARGETS`] entry the directive does not
/// ALREADY name. The guards all three narrowing families share are spelled here:
///
/// - **The pins survive an operator-set `trace`.** `EnvFilter` resolves the most SPECIFIC matching
///   directive, so `trace,ureq=info` filters `ureq::run` at `info` in either order:
///   `VIKE_LOG_FILE_LEVEL=trace`, the obvious debugging value, does not re-open the leak.
/// - **An EXPLICIT mention wins.** `VIKE_LOG_FILE_LEVEL=ureq=trace` adds no pin for `ureq`:
///   transport debugging is a typed-out opt-in, never a side effect of raising the global level.
/// - ⚠ **A pin may only ever NARROW.** It is added only when the bare global level admits the
///   target (here `debug`/`trace`, the levels that leak). Below that, `ureq=info` would RAISE the
///   target: `off,ureq=info` writes into a file the operator turned off. An unreadable global is
///   left alone, never guessed at.
///
/// This stops the record at the WRITER only: `ureq` still formats the full query while the
/// process-wide `log` max level is `Trace`, and the event is dropped before it is written.
fn with_credential_target_pins(base: &str) -> String {
    // The bare global level = the directive with no `target=` part; only it sets the level for
    // unnamed targets.
    let global = base.split(',').map(str::trim).find(|d| !d.contains('='));
    if !matches!(global, Some(g) if g.eq_ignore_ascii_case("debug") || g.eq_ignore_ascii_case("trace"))
    {
        return base.to_string();
    }
    let mut out = base.to_string();
    for target in CREDENTIAL_BEARING_TARGETS {
        // "already named" = the directive mentions `<target>=`; a bare level like `trace` does not.
        if !base.split(',').any(|d| d.trim().starts_with(&format!("{target}="))) {
            out.push_str(&format!(",{target}=info"));
        }
    }
    out
}

/// Third-party targets whose WARN chatter fires on every healthy start, with the level each is
/// pinned to on the CONSOLE: a warning that fires forever on a healthy box is one nobody reads.
///
/// The Windows Vulkan LOADER warns "Registry lookup failed to get layer manifest files" on every
/// `vike-desktop` start on a box with no validation layers registered; the app then renders
/// normally. `error`, not `off`, so a genuine Vulkan instance failure still reaches the console.
pub const NOISY_TARGETS: &[(&str, &str)] = &[("wgpu_hal::vulkan::instance", "error")];

/// Append a pin for every [`NOISY_TARGETS`] entry the directive does not already name. CONSOLE
/// only: the file is the forensic record. The guards are [`with_credential_target_pins`]'s.
fn with_noise_target_pins(base: &str) -> String {
    // The bare global level = the directive with no `target=` part; unreadable = left alone.
    let global = base.split(',').map(str::trim).find(|d| !d.contains('='));
    let Some(global_rank) = global.and_then(level_rank) else {
        return base.to_string();
    };
    let mut out = base.to_string();
    for (target, level) in NOISY_TARGETS {
        // ⚠ NARROW ONLY: at `off`/`error` an `=error` pin would turn the target back ON.
        if level_rank(level).is_none_or(|pin| global_rank <= pin) {
            continue;
        }
        // "already named" = the directive mentions `<target>=`; a bare level does not.
        if !base.split(',').any(|d| d.trim().starts_with(&format!("{target}="))) {
            out.push_str(&format!(",{target}={level}"));
        }
    }
    out
}

/// Permissiveness rank of a bare `EnvFilter` level, least permissive first. `None` for anything
/// this crate does not recognise — an unparseable directive is left alone rather than guessed at.
fn level_rank(level: &str) -> Option<u8> {
    match level.trim().to_ascii_lowercase().as_str() {
        "off" => Some(0),
        "error" => Some(1),
        "warn" => Some(2),
        "info" => Some(3),
        "debug" => Some(4),
        "trace" => Some(5),
        _ => None,
    }
}

/// Pick the console `EnvFilter` directive string: `RUST_LOG` > `VIKE_LOG` > `default_level`, plus
/// the credential and noise pins. Pure.
///
/// The credential pins apply here too: stderr is routinely captured (a systemd unit's output lands
/// in the journal), so `RUST_LOG=trace` must not write a bot token to disk.
pub(crate) fn filter_directive(
    rust_log: Option<&str>,
    vike_log: Option<&str>,
    default_level: &str,
) -> String {
    let base =
        rust_log.or(vike_log).map(str::to_string).unwrap_or_else(|| default_level.to_string());
    with_noise_target_pins(&with_credential_target_pins(&base))
}

use std::io::IsTerminal;
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{EnvFilter, Layer, Registry, fmt, reload};

/// Console rendering choice. `Auto` = default (Full) formatter, ANSI when stderr is a TTY;
/// `Pretty`/`Json` force that formatter.
#[derive(Clone, Copy, Debug, Default)]
pub enum ConsoleFormat {
    #[default]
    Auto,
    Pretty,
    Json,
}

/// Logging configuration. `Default` is the locked workspace policy: console `info`, file `trace`,
/// [`DEFAULT_MAX_LOG_FILES`] kept, prefix from the executable, no directory and no pins of its own.
#[derive(Clone, Debug)]
pub struct LogConfig {
    pub console_level: String,
    pub file_level: String,
    /// The CONFIGURED log directory — a config file's `log_dir` or a `--log-dir` flag. Beaten only
    /// by `$VIKE_LOG_DIR`; beats [`Self::project_dir`]. See [`resolve_log_dir`].
    pub dir: Option<PathBuf>,
    /// The PROJECT's own log directory (`<project>/settings/state/logs`), resolved by the BINARY
    /// via `vike_model::paths::state_path::project_log_dir`. `None` (no project above the working
    /// directory) keeps the `<exe_dir>/logs` last resort. See [`resolve_log_dir`].
    pub project_dir: Option<PathBuf>,
    pub console: ConsoleFormat,
    pub file_enabled: bool,
    /// The rolled file's name prefix. **EMPTY (the default) derives it from this executable**
    /// ([`effective_file_prefix`]).
    ///
    /// ⚠ Not cosmetic: [`Self::file_max_files`] prunes by `filename.starts_with(prefix)`, so two
    /// binaries sharing a log directory AND a prefix, or one whose prefix PREFIXES the other's
    /// (`vike` starts `vike-tradehub.<date>`), prune each other's files. A shared default prefix
    /// and a retention default cannot coexist.
    pub file_prefix: String,
    /// How many rolled log files to KEEP: `Some(n)` prunes to the newest `n`, `None` keeps every
    /// file. Rotation is DAILY, so the count is a retention in days.
    ///
    /// ⚠ A field, deliberately NOT an environment variable: a retention only an operator can arm is
    /// one nobody arms, and an env read here would grow `LIBRARY_PIN`.
    pub file_max_files: Option<usize>,

    /// Targets whose file-layer level the GLOBAL file level may not silence, as `(target, level)`
    /// pairs; the rules are [`file_level_directive_with_pins`]'s. Empty by default, which composes
    /// exactly the unpinned directive.
    ///
    /// The BINARY declares them, beside the `tracing` call each protects. ⚠ Not a convenience: a
    /// daemon unit sets `VIKE_LOG_FILE_LEVEL=warn` and the control audit record emits at `info`,
    /// so without its pin the audit trail is absent from the file
    /// (`crates/vike-tradehub/src/audit.rs`'s `FILE_PIN` carries the measurement).
    pub file_target_pins: Vec<(String, String)>,
}

/// Keep three days of rolled log files: the default [`LogConfig::file_max_files`].
///
/// The DISK bound, separate from the write-rate bound `VIKE_LOG_FILE_LEVEL`: a level an operator
/// must remember to lower is not a bound (one bulk backfill once wrote 341 GB, nearly filling the
/// disk hosting a live trading node), while a retention holds when nobody is watching. Three
/// because a fault is diagnosed from the failed run and the one before it; anything older is an
/// archive. A deployment that wants more sets the field.
pub const DEFAULT_MAX_LOG_FILES: usize = 3;

/// The prefix rolled files actually get: the configured one, else the EXECUTABLE's own name, else
/// `"vike"` when even that cannot be read.
///
/// Pure, so the rule is testable without spawning processes. Deriving from the executable is what
/// makes [`LogConfig::file_max_files`] safe to default on: pruning matches by prefix, so distinct
/// binaries need distinct, non-prefixing names, and their executable names already are.
pub fn effective_file_prefix(configured: &str, exe_stem: Option<&str>) -> String {
    let configured = configured.trim();
    if !configured.is_empty() {
        return configured.to_string();
    }
    exe_stem
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| "vike".to_string())
}

impl Default for LogConfig {
    fn default() -> Self {
        Self {
            console_level: "info".to_string(),
            file_level: "trace".to_string(),
            dir: None,
            // `None`, not a resolution: a `Default` that walked the filesystem would make every
            // `..Default::default()` do I/O. Each binary sets it.
            project_dir: None,
            console: ConsoleFormat::Auto,
            file_enabled: true,
            // Empty = derive from the executable name at `init` (see the field doc).
            file_prefix: String::new(),
            file_max_files: Some(DEFAULT_MAX_LOG_FILES),
            // No pins: opt-in, declared by the binary.
            file_target_pins: Vec::new(),
        }
    }
}

/// Build the console layer for `format` over `filter` and `make_writer`, boxed because each
/// formatter is a different concrete type. Separate from `init` so the formatter selection is
/// testable over a capturing writer.
fn console_layer(
    format: ConsoleFormat,
    ansi_tty: bool,
    filter: impl tracing_subscriber::layer::Filter<Registry> + Send + Sync + 'static,
    make_writer: impl for<'w> fmt::MakeWriter<'w> + 'static + Send + Sync,
) -> Box<dyn Layer<Registry> + Send + Sync> {
    match format {
        ConsoleFormat::Auto => {
            fmt::layer().with_writer(make_writer).with_ansi(ansi_tty).with_filter(filter).boxed()
        }
        ConsoleFormat::Pretty => fmt::layer()
            .pretty()
            .with_writer(make_writer)
            .with_ansi(true)
            .with_filter(filter)
            .boxed(),
        ConsoleFormat::Json => fmt::layer()
            .json()
            .with_writer(make_writer)
            .with_ansi(false)
            .with_filter(filter)
            .boxed(),
    }
}

/// One layer's reloadable `EnvFilter` handle, as [`init_with_reload`] installs it. `S` is the bare
/// [`Registry`], so both layers' handles share this one nameable type.
type ReloadableFilterHandle = reload::Handle<EnvFilter, Registry>;

/// **The live-reload handles over the two installed `EnvFilter`s**, returned by
/// [`init_with_reload`] so a long-running daemon can apply a changed log level WITHOUT a restart
/// (`vike-tradehub`'s hot-apply seam is the consumer).
///
/// Both methods take LEVELS and RECOMPOSE the directive through the same pure functions `init`
/// uses, so a reload can never produce a filter a restart would not: the
/// [`CREDENTIAL_BEARING_TARGETS`] pins and the boot's [`LogConfig::file_target_pins`] survive a hot
/// level change. ⚠ That is why no raw directive is accepted: a pin-less filter swapped in would
/// silently re-open the URL-credential leak.
///
/// Env values are PARAMETERS (the caller's own sweep), not reads.
#[derive(Clone)]
pub struct LogReloadHandles {
    console: ReloadableFilterHandle,
    file: Option<ReloadableFilterHandle>,
    /// The boot's [`LogConfig::file_target_pins`], so a hot reload recomposes the SAME directive a
    /// restart would: a `SetSetting` turning `preferences.log_file_level` DOWN must not drop the
    /// audit pin for the rest of the process's life.
    file_pins: Vec<(String, String)>,
}

impl LogReloadHandles {
    /// Recompose and swap the CONSOLE filter: `rust_log` > `vike_log` > `default_level`, the same
    /// precedence [`init`] resolves. Pass the boot sweep's values: a daemon's environment is fixed
    /// at spawn.
    pub fn reload_console_level(
        &self,
        rust_log: Option<&str>,
        vike_log: Option<&str>,
        default_level: &str,
    ) -> Result<(), String> {
        let directive = filter_directive(rust_log, vike_log, default_level);
        self.console
            .reload(EnvFilter::new(&directive))
            .map_err(|e| format!("console filter reload failed: {e}"))
    }

    /// Recompose and swap the FILE filter: `env_level` (`VIKE_LOG_FILE_LEVEL`) > `cfg_level`, plus
    /// the boot's pins, the same precedence [`init`] resolves. `Err` when no reloadable file layer
    /// is installed (file logging disabled, or its destination could not be opened at init): the
    /// caller's honest answer is then restart-required.
    pub fn reload_file_level(
        &self,
        env_level: Option<&str>,
        cfg_level: &str,
    ) -> Result<(), String> {
        let Some(handle) = &self.file else {
            return Err(
                "no reloadable FILE layer is installed (file logging disabled, or its destination \
                 could not be opened at init) — a file-level change cannot apply to this process"
                    .to_string(),
            );
        };
        let directive = file_level_directive_with_pins(env_level, cfg_level, &self.file_pins);
        handle
            .reload(EnvFilter::new(&directive))
            .map_err(|e| format!("file filter reload failed: {e}"))
    }
}

/// Install the global subscriber. Returns appender guards the binary MUST hold for the process
/// lifetime (dropping flushes the non-blocking writer). A second call is a no-op warning: the
/// global default can only be set once.
///
/// ⚠ **The returned `Vec` may be EMPTY, and that is normal, not a failure to handle**: the FILE
/// layer is best-effort (see the crate doc). An empty vec needs nothing from the caller; do not
/// branch on its length.
pub fn init(cfg: LogConfig) -> Vec<WorkerGuard> {
    init_with_reload(cfg).0
}

/// [`init`] plus the [`LogReloadHandles`] over the two installed filters. `init` is this with the
/// handles dropped, which leaves each filter at its boot value.
pub fn init_with_reload(cfg: LogConfig) -> (Vec<WorkerGuard>, LogReloadHandles) {
    let mut guards = Vec::new();

    // console layer (stderr) — filter: RUST_LOG > VIKE_LOG > cfg.console_level
    let directive = filter_directive(
        std::env::var("RUST_LOG").ok().as_deref(),
        std::env::var("VIKE_LOG").ok().as_deref(),
        &cfg.console_level,
    );
    // Through a `reload::Layer` so `LogReloadHandles` can swap the filter live; steady-state cost
    // is a read-lock per interest check.
    let (console_filter, console_handle): (reload::Layer<EnvFilter, Registry>, _) =
        reload::Layer::new(EnvFilter::new(directive));
    let ansi = std::io::stderr().is_terminal();
    let console = console_layer(cfg.console, ansi, console_filter, std::io::stderr);

    // file layer — JSON, daily-rolling, non-blocking; filter: VIKE_LOG_FILE_LEVEL > cfg.file_level
    let mut file_handle: Option<ReloadableFilterHandle> = None;
    let file_layer: Option<Box<dyn Layer<Registry> + Send + Sync>> = if cfg.file_enabled {
        let exe_path = std::env::current_exe().ok();
        let exe_dir = exe_path
            .as_ref()
            .and_then(|p| p.parent().map(Path::to_path_buf))
            .unwrap_or_else(|| PathBuf::from("."));
        let file_prefix = effective_file_prefix(
            &cfg.file_prefix,
            exe_path.as_ref().and_then(|p| p.file_stem()).and_then(|s| s.to_str()),
        );
        let dir = resolve_log_dir(
            std::env::var("VIKE_LOG_DIR").ok().as_deref(),
            cfg.dir.as_deref(),
            cfg.project_dir.as_deref(),
            &exe_dir,
        );
        let _ = std::fs::create_dir_all(&dir);
        // ⚠ `Builder::build`, NOT `rolling::daily`: same file names and rotation, but `daily` ends
        // in an `.expect` that PANICS the process when the destination cannot be opened (a
        // read-only `<exe_dir>/logs` under `ProtectSystem=strict`, a read-only container layer).
        // A failure here drops the FILE layer with one stderr line; the console layer carries on.
        let file_directive = file_level_directive_with_pins(
            std::env::var("VIKE_LOG_FILE_LEVEL").ok().as_deref(),
            &cfg.file_level,
            &cfg.file_target_pins,
        );
        // `max_log_files` is the DISK bound (how much is KEPT), separate from the write-rate bound
        // above; `file_max_files: None` keeps every file.
        let mut builder = tracing_appender::rolling::Builder::new()
            .rotation(tracing_appender::rolling::Rotation::DAILY)
            .filename_prefix(&file_prefix);
        if let Some(keep) = cfg.file_max_files {
            builder = builder.max_log_files(keep);
        }
        let appender = builder.build(&dir);
        match appender {
            Ok(appender) => {
                let (nb, guard) = tracing_appender::non_blocking(appender);
                guards.push(guard);
                let (file_filter, handle): (reload::Layer<EnvFilter, Registry>, _) =
                    reload::Layer::new(EnvFilter::new(file_directive));
                file_handle = Some(handle);
                Some(fmt::layer().json().with_writer(nb).with_filter(file_filter).boxed())
            }
            Err(e) => {
                // Direct to stderr, not `tracing`: no subscriber is installed yet, so a
                // `tracing::warn!` here would be swallowed, and silence is worse than a panic.
                eprintln!(
                    "vike-log: the trace FILE log is DISABLED for this run — {} could not be \
                     opened ({e}). Console logging is unaffected. Set VIKE_LOG_DIR to a writable \
                     directory to restore it.",
                    dir.display()
                );
                None
            }
        }
    } else {
        None
    };

    // ONE `Vec` of boxed `Layer<Registry>`s, so both reload handles share the nameable
    // `Handle<EnvFilter, Registry>` type; per-layer filtering is unchanged.
    let mut layers: Vec<Box<dyn Layer<Registry> + Send + Sync>> = vec![console];
    if let Some(f) = file_layer {
        layers.push(f);
    }
    let already_set = tracing_subscriber::registry().with(layers).try_init().is_err();
    if already_set {
        tracing::warn!("vike_log::init called twice — keeping the first subscriber");
    }
    install_panic_hook();
    (
        guards,
        // The pins travel WITH the handles, so a hot reload recomposes this boot's directive.
        LogReloadHandles {
            console: console_handle,
            file: file_handle,
            file_pins: cfg.file_target_pins,
        },
    )
}

/// Guards [`install_panic_hook`]: a second `init` must not stack a second hook, which would log
/// every panic twice.
static PANIC_HOOK: std::sync::Once = std::sync::Once::new();

/// Install, once per process, a panic hook that logs the panic as a `tracing` error and then calls
/// the hook that was installed before it (the default one prints to stderr, so stderr is unchanged).
///
/// Without it a panic on a worker thread reaches stderr only, never the file layer an operator
/// reads afterwards. Called from [`init_with_reload`] AFTER the subscriber is set, so the hook never
/// logs into a void; deliberately NOT from [`test_init`], so a test that installs its own hook
/// (`crates/vike-core/src/scratch.rs`) keeps it.
///
/// A panic that is CAUGHT (`catch_unwind` in the core's per-dispatch loop) is logged too, on purpose:
/// the operator should see every handler panic, not only the fatal ones.
fn install_panic_hook() {
    PANIC_HOOK.call_once(|| {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            // ⚠ A hook that panics aborts the process (a panic while panicking), so nothing in
            // `log_panic` may unwind: the unwind is caught and dropped, and the previous hook runs
            // regardless.
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| log_panic(info)));
            previous(info);
        }));
    });
}

/// The `tracing` event for one panic: target `panic`, the thread's name (`<unnamed>` when it has
/// none), `file:line:col` and the payload text.
fn log_panic(info: &std::panic::PanicHookInfo<'_>) {
    let thread = std::thread::current();
    let name = thread.name().unwrap_or("<unnamed>");
    let location = match info.location() {
        Some(l) => format!("{}:{}:{}", l.file(), l.line(), l.column()),
        None => "<unknown>".to_string(),
    };
    let payload = panic_payload_text(info.payload());
    tracing::error!(
        target: "panic",
        thread = name,
        location = location.as_str(),
        payload = payload,
        "thread panicked"
    );
}

/// A panic payload as text: `panic!("literal")` carries a `&str`, `panic!("{x}")` a `String`,
/// and `std::panic::panic_any` anything.
fn panic_payload_text(payload: &(dyn std::any::Any + Send)) -> &str {
    if let Some(s) = payload.downcast_ref::<&str>() {
        s
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.as_str()
    } else {
        "<non-string payload>"
    }
}

/// Test-only console/`with_test_writer` init, no file, idempotent. For tests that want log output.
pub fn test_init() {
    // Honor RUST_LOG > VIKE_LOG > "debug", matching init()'s console aliasing.
    let directive = filter_directive(
        std::env::var("RUST_LOG").ok().as_deref(),
        std::env::var("VIKE_LOG").ok().as_deref(),
        "debug",
    );
    let _ = fmt().with_test_writer().with_env_filter(EnvFilter::new(directive)).try_init();
}

// The scoped `tracing` capture; behind `test-support`, so a shipped build never compiles it.
#[cfg(any(test, feature = "test-support"))]
pub mod capture;

#[cfg(test)]
mod lib_tests;
