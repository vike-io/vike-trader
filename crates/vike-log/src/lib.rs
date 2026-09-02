//! Workspace logging init. Builds a layered `tracing` subscriber: a console layer (stderr;
//! formatter selected by `ConsoleFormat` — Auto/Pretty/Json, ANSI on a TTY for Auto/Pretty) plus
//! a non-blocking JSON daily-rolling file layer. Depends on NO domain crate — only binaries call
//! `init`. See docs/superpowers/specs/2026-07-06-tracing-logging-design.md.
//!
//! **Where the file lands** is [`resolve_log_dir`]'s four-layer answer: `$VIKE_LOG_DIR` >
//! [`LogConfig::dir`] (a config file's `log_dir` / a `--log-dir` flag) > [`LogConfig::project_dir`]
//! (`<project>/settings/state/logs`, resolved by the BINARY) > `<exe_dir>/logs`. Every layer above
//! the last arrives as a PARAMETER: this crate is the bottom of the graph and cannot walk for a
//! project root, and per the settings rule it should not — the binary owns that read.
//!
//! **The file layer is BEST-EFFORT; the console layer is not.** A destination that cannot be opened
//! drops the file layer with one line on stderr and leaves logging otherwise working. It used to
//! PANIC the process — `tracing_appender::rolling::daily` ends in an `.expect` — which meant a tool
//! run anywhere its `<exe_dir>/logs` last resort is read-only (`ProtectSystem=strict`, a read-only
//! container layer, `/usr/local/bin`) died at exit 101 because it could not open a LOG file.
//! Logging is instrumentation: it may degrade, it may not be what stops a trading tool from running.
//! `tests/unwritable_log_dir.rs` is the gate.

// Build-graph edge only (see the root Cargo.toml `idna_adapter` rationale): holding this dependency
// is what binds the workspace `idna_adapter` requirement into resolution at all. ⚠ That requirement
// is `~1.2` today — the ICU4X stream, NOT the `~1.0` stub back end this edge was added to select.
// NOT an API this crate uses.
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
/// Pure, and the whole precedence lives here rather than being folded together by each caller, so
/// there is exactly one answer to "why did the log land there".
///
/// **`project_dir` is the layer this gained**, and it is what makes the default sane: without it
/// the answer for every binary was the `<exe_dir>/logs` last resort — `target/debug/logs/…` in a
/// checkout, which `cargo clean` deletes, and beside the binary on a deployment. It sits BELOW
/// `cfg_dir` deliberately: a value a human wrote in a config file (or typed as a flag) must still
/// beat a location the program derived for itself.
///
/// It arrives as a PARAMETER because this crate is deliberately the bottom layer and depends on no
/// other crate — it cannot walk for a project root, and per the settings rule it should not: the
/// BINARY resolves it (`vike_model::state_path::project_log_dir`) and hands it over. `None` — no
/// project above the working directory — keeps the `exe_dir` last resort exactly as before.
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

/// Pick the FILE layer's `EnvFilter` directive string: `VIKE_LOG_FILE_LEVEL` env > `cfg.file_level`.
/// Pure for testability.
///
/// The file layer defaults to `trace` and captures the `log`→`tracing` bridge, so a batch tool that
/// drives a chatty dependency (DataFusion/arrow/hyper) writes an enormous JSON file that **no
/// console-level setting can turn down** — `RUST_LOG`/`VIKE_LOG` only filter the console. Measured:
/// one hour of `pmxt_backfill` wrote **6.4 GB**, and a long range once wrote 341 GB and nearly
/// filled the disk hosting a live trading node. This knob is the off switch that was missing;
/// `VIKE_LOG_FILE_LEVEL=warn` (or `off`) keeps the file useful without the per-row firehose.
pub fn file_level_directive(env_level: Option<&str>, cfg_level: &str) -> String {
    let base = match env_level.map(str::trim).filter(|s| !s.is_empty()) {
        Some(l) => l.to_string(),
        None => cfg_level.to_string(),
    };
    with_credential_target_pins(&base)
}

/// Third-party log targets that print CREDENTIALS, pinned OUT of the trace firehose.
///
/// These are not chosen for volume. Each one writes a live secret into the log at a level this
/// workspace's file layer enables BY DEFAULT, below the layer where every app-level redaction
/// lives — `vike-tradehub`'s `telegram/deps.rs` and `vike-alerting`'s `delivery.rs` both redact
/// carefully, and are bypassed entirely by their own HTTP client logging the URL they were handed:
///
/// - **`ureq`** — `run.rs` logs `"{method} {DebugUri}"`. `DebugUri` is deliberately careful: it
///   prints the path+query ONLY `if log_enabled!(log::Level::Trace)` and writes `/******`
///   otherwise. Our `trace` file default is exactly what defeats that redaction. Headers ARE
///   redacted by an allowlist (`NON_SENSITIVE_HEADERS`), so the URL is the whole exposure — and it
///   is enough: the **Telegram bot token is in the URL PATH**, and Finnhub/FMP keys are query
///   parameters.
/// - **`tungstenite`** — worse, because it has no redaction at all. `client.rs`'s
///   `connect_to_some` logs the FULL uri at `debug!` (so `debug` is NOT a safe floor for it), and
///   `handshake/client.rs` logs the ENTIRE raw HTTP upgrade request — path plus every header — at
///   `trace!`. The binance **listenKey** is a path segment of the user-data WS URL.
///
/// Pinned at `info` rather than `debug` precisely because of `connect_to_some`: `debug` still
/// leaks. This is a DENYLIST and therefore not a complete defence — a newly added dependency that
/// logs URLs would leak again — which is why it is kept next to the reasoning rather than buried
/// in a config file. It is the cheap half; the durable half is not logging secrets into URLs.
pub const CREDENTIAL_BEARING_TARGETS: &[&str] = &["ureq", "tungstenite"];

/// Append a `target=info` pin for every [`CREDENTIAL_BEARING_TARGETS`] entry the caller's directive
/// does not ALREADY name, and return the composed `EnvFilter` directive string.
///
/// Two properties, both deliberate:
///
/// - **The pins survive an operator-set `trace`.** They are appended to whatever directive is in
///   effect, so `VIKE_LOG_FILE_LEVEL=trace` — the obvious thing to type when debugging — does not
///   silently re-open the leak. `EnvFilter` resolves the most SPECIFIC matching directive, and a
///   directive with a target is more specific than a bare level, so `trace,ureq=info` filters
///   `ureq::run` at `info` regardless of the order the two appear in.
/// - **An explicit mention wins.** If the directive already names the target
///   (`VIKE_LOG_FILE_LEVEL=ureq=trace`) no pin is added for it, so transport debugging stays
///   possible — but only as a deliberate, typed-out opt-in, never as a side effect of raising the
///   global level.
///
/// Note this only stops the record from reaching the WRITER. `ureq` still FORMATS the full query
/// when the process-wide `log` max level is `Trace` (`log_enabled!` reads the global max, which the
/// bare `trace` directive keeps at Trace); the event is then dropped by this per-layer filter
/// before it is written. Nothing reaches the file — the string is merely built and discarded.
///
/// ⚠ **A pin may only ever NARROW.** It is added only when the directive's bare global level is
/// `debug` or `trace` — the two levels at which these targets leak. At `off`/`error`/`warn`/`info`
/// the global level ALREADY excludes everything the pin would exclude, and appending `ureq=info`
/// there would RAISE those targets instead: `VIKE_LOG_FILE_LEVEL=off,ureq=info` writes `ureq` info
/// lines into a file the operator explicitly turned off. A redaction that turns logging back on is
/// not a redaction.
fn with_credential_target_pins(base: &str) -> String {
    // The bare global level = a directive with no `target=` part. `EnvFilter` allows several
    // comma-separated directives; only the bare one sets the level for unnamed targets.
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

/// Third-party targets whose WARN-level chatter fires on every healthy start, with the level each
/// is pinned to on the CONSOLE. A warning that fires forever on a healthy box is a warning nobody
/// reads — the same argument `crate::server_time`'s clock table makes in `vike-mount`.
///
/// The Windows Vulkan LOADER emits "windows_read_data_files_in_registry: Registry lookup failed to
/// get layer manifest files" (plus an `objects:` continuation line) at WARN on every `vike-app`
/// start on a box with no validation layers registered. It is reporting that optional debug layers
/// are absent; the app then enumerates its adapters and renders normally. Pinned to `error` rather
/// than `off` so a genuine Vulkan instance failure still reaches the console.
pub const NOISY_TARGETS: &[(&str, &str)] = &[("wgpu_hal::vulkan::instance", "error")];

/// Append a pin for every [`NOISY_TARGETS`] entry the caller's directive does not already name.
/// CONSOLE only — the file layer is the forensic record and keeps everything it is set to.
///
/// Same two guards as [`with_credential_target_pins`]: an EXPLICIT mention wins, and a pin may only
/// ever NARROW — it is added only when the bare global level would otherwise ADMIT the noise, so
/// `off`/`error` never gain a directive that would turn these targets back ON.
fn with_noise_target_pins(base: &str) -> String {
    // The bare global level = a directive with no `target=` part, same as the credential pins read
    // it. A directive whose level this function does not understand is left ALONE, never guessed.
    let global = base.split(',').map(str::trim).find(|d| !d.contains('='));
    let Some(global_rank) = global.and_then(level_rank) else {
        return base.to_string();
    };
    let mut out = base.to_string();
    for (target, level) in NOISY_TARGETS {
        // ⚠ NARROW ONLY: pin only where the global level would otherwise ADMIT this target's
        // chatter. At `off`/`error` the global already excludes it, and appending `=error` there
        // would turn the target back ON in a console the operator quieted.
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

/// Pick the console `EnvFilter` directive string: `RUST_LOG` > `VIKE_LOG` > `default_level`. Pure.
///
/// Carries the same [`CREDENTIAL_BEARING_TARGETS`] pins as the file layer. The console is `info` by
/// default and therefore already safe, but stderr is routinely captured — a systemd unit's output
/// lands in the journal — so `RUST_LOG=trace`, which an operator types precisely when something is
/// wrong, must not be the thing that writes a bot token to disk. `RUST_LOG=trace,ureq=trace` is
/// still available when the transport itself is what is being debugged.
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
use tracing_subscriber::{fmt, reload, EnvFilter, Layer, Registry};

/// Console rendering choice. `Auto` = default (Full) formatter, ANSI when stderr is a TTY;
/// `Pretty`/`Json` force that formatter.
#[derive(Clone, Copy, Debug, Default)]
pub enum ConsoleFormat {
    #[default]
    Auto,
    Pretty,
    Json,
}

/// Logging configuration. `Default` is the locked workspace policy: console `info`, file `trace`.
#[derive(Clone, Debug)]
pub struct LogConfig {
    pub console_level: String,
    pub file_level: String,
    /// The CONFIGURED log directory — a config file's `log_dir` or a `--log-dir` flag. Beaten only
    /// by `$VIKE_LOG_DIR`; beats [`Self::project_dir`]. See [`resolve_log_dir`].
    pub dir: Option<PathBuf>,
    /// The PROJECT's own log directory (`<project>/settings/state/logs`), resolved by the BINARY
    /// via `vike_model::state_path::project_log_dir` — the default when nothing above it is set,
    /// and the reason the rolling file no longer lands in `<exe_dir>/logs`. `None` (no project
    /// above the working directory) keeps that last resort. See [`resolve_log_dir`].
    pub project_dir: Option<PathBuf>,
    pub console: ConsoleFormat,
    pub file_enabled: bool,
    /// The rolled file's name prefix. **EMPTY (the default) means "derive it from this executable"**
    /// — see [`effective_file_prefix`].
    ///
    /// ⚠ It is not cosmetic, because [`Self::file_max_files`] prunes by
    /// `filename.starts_with(prefix)`. Two binaries sharing one log directory AND one prefix prune
    /// each other's files; worse, a prefix that is a PREFIX OF ANOTHER prunes that one's too. The
    /// old default, `"vike"`, was exactly that — twenty binaries left it set, `<project>/settings/
    /// state/logs` is shared, and `"vike"` starts `vike-app.2026-08-06` and
    /// `vike-tradehub.2026-08-06`, so a one-shot backfill would have deleted the GUI's and the live
    /// daemon's logs. Retention and a shared prefix cannot both be defaults.
    pub file_prefix: String,
    /// How many rolled log files to KEEP. `Some(n)` prunes to the newest `n`; `None` keeps every
    /// file forever, which is what this crate did before [`DEFAULT_MAX_LOG_FILES`] existed.
    ///
    /// Rotation is DAILY, so the count is a retention in days.
    ///
    /// ⚠ This is a `LogConfig` field and deliberately NOT an environment variable. A retention that
    /// only an operator can arm is a retention nobody has armed — the `VIKE_LOG_FILE_LEVEL` knob was
    /// exactly that, and a 341 GB file is what it cost. It is also a parameter because this crate is
    /// the bottom of the graph and reads no configuration of its own; adding an env read here would
    /// grow `LIBRARY_PIN`, the ratchet that exists to stop precisely that.
    pub file_max_files: Option<usize>,
}

/// Keep three days of rolled log files.
///
/// The disk bound this crate had none of. `VIKE_LOG_FILE_LEVEL` turns the WRITE RATE down but
/// nothing ever deleted a file, so a long-running daemon's log directory grew without limit and a
/// single bulk backfill once wrote **341 GB**, nearly filling the disk hosting a live trading node.
/// Turning the level down is a thing an operator must remember; a retention is a thing that holds
/// while nobody is watching, which is the only kind of protection worth having on an unattended box.
///
/// Three is chosen against the failure it is for: a fault is diagnosed from the run that failed and
/// the one before it, and any investigation reaching further back is reading an ARCHIVE, not a log
/// directory. It is a `LogConfig` field, so a deployment that genuinely wants more sets it.
pub const DEFAULT_MAX_LOG_FILES: usize = 3;

/// The prefix rolled files actually get: the configured one, or the EXECUTABLE's own name when that
/// is empty (the default), or `"vike"` when even that cannot be read.
///
/// Pure, and separate from [`init`] so the rule is testable without spawning processes. Deriving
/// from the executable rather than defaulting to one shared literal is what makes
/// [`LogConfig::file_max_files`] safe to turn on by default: pruning matches on
/// `filename.starts_with(prefix)`, so distinct binaries must have distinct — and non-prefixing —
/// names, and their own executable names already are.
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
            // Deliberately `None` here rather than a resolution: a `Default` that walked the
            // filesystem would make every `..Default::default()` do I/O, and this crate takes its
            // configuration as parameters. Each binary sets it.
            project_dir: None,
            console: ConsoleFormat::Auto,
            file_enabled: true,
            // Empty = derive from the executable name at `init`. Was `"vike"`; see the field doc
            // for why a SHARED default prefix and a retention default cannot coexist.
            file_prefix: String::new(),
            file_max_files: Some(DEFAULT_MAX_LOG_FILES),
        }
    }
}

/// Build the console layer for a given `ConsoleFormat`, filter, and writer. Formatter
/// (Full/Pretty/Json) is chosen per `format`; `.pretty()`/`.json()` each change the layer's
/// concrete type, so all three arms are type-erased via `.boxed()` to a common
/// `Box<dyn Layer<Registry> + Send + Sync>` before composing with the registry. Split out of
/// `init` (rather than inlined) so the formatter-selection logic is unit-testable over a
/// capturing writer without threading a writer type parameter through the public `init` API.
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

/// A per-layer reloadable `EnvFilter` handle, as [`init_with_reload`] installs it: the filter sits
/// inside a [`reload::Layer`] whose `S` is the bare [`Registry`] (both layers are boxed
/// `Layer<Registry>`s composed as one `Vec`, so both handles share this one nameable type).
type ReloadableFilterHandle = reload::Handle<EnvFilter, Registry>;

/// **The live-reload handles over the two installed `EnvFilter`s** — what [`init_with_reload`]
/// returns beside the guards, so a long-running daemon can apply a changed log level WITHOUT a
/// restart (`vike-tradehub`'s REQ-7 hot-apply seam is the consumer).
///
/// Both methods RECOMPOSE the directive through the same pure functions `init` composes with —
/// [`filter_directive`]'s `RUST_LOG` > `VIKE_LOG` > default precedence for the console,
/// [`file_level_directive`]'s `VIKE_LOG_FILE_LEVEL` > config precedence for the file — so a
/// reload can never produce a filter a restart would not, and the [`CREDENTIAL_BEARING_TARGETS`]
/// pins survive a hot level change exactly as they survive a boot-time `trace`. That is why the
/// API takes LEVELS and composes internally rather than accepting a raw directive: a caller
/// handed the raw `reload` handle could swap in a pin-less filter and silently re-open the
/// URL-credential leak those pins exist to close.
///
/// Env values are PARAMETERS (the caller's own sweep), not reads — this crate stays at the bottom
/// of the graph and reads no environment outside [`init`] itself.
#[derive(Clone)]
pub struct LogReloadHandles {
    console: ReloadableFilterHandle,
    file: Option<ReloadableFilterHandle>,
}

impl LogReloadHandles {
    /// Recompose and swap the CONSOLE filter: `rust_log` > `vike_log` > `default_level`, the same
    /// precedence [`init`] resolves (pass the values from the sweep the process booted with — a
    /// daemon's environment is fixed at spawn, so the boot sweep IS the current one).
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

    /// Recompose and swap the FILE filter: `env_level` (`VIKE_LOG_FILE_LEVEL`) > `cfg_level`, the
    /// same precedence [`init`] resolves. `Err` when no reloadable file layer is installed —
    /// file logging disabled, or its destination could not be opened at init — because a level
    /// "applied" to a layer that does not exist has not been applied to anything, and the caller's
    /// honest answer to its own operator is restart-required.
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
        let directive = file_level_directive(env_level, cfg_level);
        handle
            .reload(EnvFilter::new(&directive))
            .map_err(|e| format!("file filter reload failed: {e}"))
    }
}

/// Install the global subscriber. Returns appender guards the binary MUST hold for the process
/// lifetime (dropping flushes the non-blocking writer). Safe to call once; a second call is a
/// no-op warning (the global default can only be set once).
///
/// ⚠ **The returned `Vec` may be EMPTY, and that is a normal outcome, not a failure to handle.** The
/// FILE layer is best-effort: a destination that cannot be opened drops it, says so once on stderr,
/// and leaves the console layer installed — see the `match` on the appender below for why a panic
/// there was the wrong answer. Callers already hold the vec and drop it at exit, so an empty one
/// needs nothing from them; nobody should start branching on its length.
pub fn init(cfg: LogConfig) -> Vec<WorkerGuard> {
    init_with_reload(cfg).0
}

/// [`init`] plus the [`LogReloadHandles`] over the two installed filters. The one entry point —
/// `init` is this function with the handles dropped, which is harmless (a dropped `reload::Handle`
/// leaves the filter permanently at its boot value, exactly the pre-reload behaviour).
pub fn init_with_reload(cfg: LogConfig) -> (Vec<WorkerGuard>, LogReloadHandles) {
    let mut guards = Vec::new();

    // console layer (stderr) — filter: RUST_LOG > VIKE_LOG > cfg.console_level
    let directive = filter_directive(
        std::env::var("RUST_LOG").ok().as_deref(),
        std::env::var("VIKE_LOG").ok().as_deref(),
        &cfg.console_level,
    );
    // Reload seam: the filter goes into the layer THROUGH a `reload::Layer`, whose handle can
    // swap it live ([`LogReloadHandles`]). Steady-state cost is a read-lock per interest check,
    // off every hot fold (which logs nothing per-message by policy anyway).
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
        // ⚠ `Builder::build`, NOT `rolling::daily`. The two are the same call — `daily` is
        // `RollingFileAppender::new`, which is literally
        // `builder().rotation(..).filename_prefix(..).build(dir).expect("initializing rolling file
        // appender failed")` — so the file NAMES and rotation are byte-identical. The difference is
        // the `.expect`: an unwritable log destination PANICKED the whole process, out of
        // `tracing-appender`'s internals, with a backtrace naming neither this crate nor
        // `VIKE_LOG_DIR`.
        //
        // That is not hypothetical and it is not only about deployments. `resolve_log_dir`'s last
        // resort is `<exe_dir>/logs`, so ANY binary run with no project above its working directory
        // and no `VIKE_LOG_DIR` writes beside its own executable — measured: `backtest --help` from
        // a neutral directory creates `target/debug/logs/backtest.<date>` before it has decided it
        // has any work to do. Put that same binary anywhere the exe directory is read-only (a
        // systemd unit with `ProtectSystem=strict`, a read-only container layer, an install under
        // `/usr/local/bin`) and it died with exit 101 — measured on the CI box at both `ENOENT` and
        // `EACCES` — for the sole reason that it could not open a LOG file.
        //
        // Logging is instrumentation. It may degrade; it may not be the thing that stops a trading
        // tool from running. So a failure here drops the FILE layer, says so once on stderr naming
        // the directory and the variable that moves it, and the console layer carries on.
        let file_directive = file_level_directive(
            std::env::var("VIKE_LOG_FILE_LEVEL").ok().as_deref(),
            &cfg.file_level,
        );
        // `max_log_files` is the DISK bound, and it is separate from the write-rate bound above:
        // `VIKE_LOG_FILE_LEVEL` decides how much gets written, this decides how much is KEPT. Only
        // the second one holds while nobody is watching. Absent (`file_max_files: None`) keeps
        // every file forever, which is what every build did before this existed.
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
                // Direct to stderr, not `tracing`: the subscriber is not installed yet, so a
                // `tracing::warn!` here would be swallowed by the `NoSubscriber` default and the
                // operator would get silence where they used to get a panic — a worse trade than the
                // panic itself. The console layer below still installs.
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

    // Both layers are boxed `Layer<Registry>`s composed as ONE `Vec` (rather than the old
    // `.with(console).with(file)` stack) so both reload handles share the same nameable
    // `Handle<EnvFilter, Registry>` type — per-layer filter semantics are unchanged.
    let mut layers: Vec<Box<dyn Layer<Registry> + Send + Sync>> = vec![console];
    if let Some(f) = file_layer {
        layers.push(f);
    }
    let already_set = tracing_subscriber::registry().with(layers).try_init().is_err();
    if already_set {
        tracing::warn!("vike_log::init called twice — keeping the first subscriber");
    }
    (guards, LogReloadHandles { console: console_handle, file: file_handle })
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};

    /// The whole ladder: `$VIKE_LOG_DIR` > a config file's `log_dir` > the project's own
    /// `settings/state/logs` > `<exe_dir>/logs`.
    ///
    /// Every ADJACENT pair is asserted, not just the extremes — a precedence test that only checks
    /// "the top wins" and "the bottom is the fallback" passes with the middle two swapped, and the
    /// middle two are exactly the pair with a real argument behind their order (a value a human
    /// wrote must beat one the program derived).
    #[test]
    fn dir_precedence_env_over_cfg_over_project_over_exe() {
        let exe = Path::new("/opt/vike");
        let cfg = PathBuf::from("/var/cfg");
        let project = PathBuf::from("/proj/settings/state/logs");

        // env wins over everything
        assert_eq!(
            resolve_log_dir(Some("/env/logs"), Some(&cfg), Some(&project), exe),
            PathBuf::from("/env/logs")
        );
        // cfg (a config file's log_dir / a --log-dir flag) wins over the project default
        assert_eq!(resolve_log_dir(None, Some(&cfg), Some(&project), exe), cfg);
        // the project default wins over the exe fallback — the layer this gained, and the reason a
        // checkout's trace log stopped landing in `target/debug/logs`.
        assert_eq!(resolve_log_dir(None, None, Some(&project), exe), project);
        // last resort, unchanged: <exe_dir>/logs, for a binary with no project above it
        assert_eq!(resolve_log_dir(None, None, None, exe), PathBuf::from("/opt/vike/logs"));
        // …and the env var still wins with nothing else set at all.
        assert_eq!(resolve_log_dir(Some("/env/logs"), None, None, exe), PathBuf::from("/env/logs"));
    }

    /// `LogConfig::default()` names no directory itself: both layers it owns are `None`, so a
    /// binary that sets neither still gets the historical `<exe_dir>/logs` and a binary that sets
    /// `project_dir` gets the project one. A `Default` that resolved a path would make every
    /// `..Default::default()` in the workspace touch the filesystem.
    #[test]
    fn the_default_config_resolves_no_directory_of_its_own() {
        let cfg = LogConfig::default();
        assert!(cfg.dir.is_none());
        assert!(cfg.project_dir.is_none());
        assert_eq!(
            resolve_log_dir(None, cfg.dir.as_deref(), cfg.project_dir.as_deref(), Path::new("/x")),
            PathBuf::from("/x/logs")
        );
    }

    /// The BASE of a composed directive — everything before the credential-target pins this
    /// workspace appends to every filter (see `with_credential_target_pins`). Precedence is a
    /// property of the base, so the precedence tests assert on it and the pins get their own tests.
    fn base_of(directive: &str) -> String {
        directive
            .split(',')
            .filter(|d| {
                let d = d.trim();
                !CREDENTIAL_BEARING_TARGETS.iter().any(|t| d.starts_with(t))
                    && !NOISY_TARGETS.iter().any(|(t, _)| d.starts_with(t))
            })
            .collect::<Vec<_>>()
            .join(",")
    }

    #[test]
    fn filter_precedence_rust_log_over_vike_log_over_default() {
        assert_eq!(base_of(&filter_directive(Some("debug"), Some("warn"), "info")), "debug");
        assert_eq!(base_of(&filter_directive(None, Some("warn"), "info")), "warn");
        assert_eq!(base_of(&filter_directive(None, None, "info")), "info");
    }

    /// The file layer's own precedence — the console knobs never reach it, so this is the ONLY
    /// way to turn the `trace` firehose down (see `file_level_directive`'s doc for what it cost).
    #[test]
    fn file_level_precedence_env_over_cfg() {
        assert_eq!(base_of(&file_level_directive(Some("warn"), "trace")), "warn");
        assert_eq!(base_of(&file_level_directive(Some("off"), "trace")), "off");
        assert_eq!(base_of(&file_level_directive(None, "trace")), "trace");
        // An empty or whitespace-only env value is NOT a directive — it would widen the filter to
        // the `EnvFilter` default rather than narrow it, so it falls through to the config.
        assert_eq!(base_of(&file_level_directive(Some(""), "trace")), "trace");
        assert_eq!(base_of(&file_level_directive(Some("  "), "trace")), "trace");
        // Surrounding whitespace is trimmed rather than passed into `EnvFilter`.
        assert_eq!(base_of(&file_level_directive(Some(" warn "), "trace")), "warn");
    }

    /// Every filter this crate builds pins the credential-bearing targets at the two levels where
    /// they LEAK — `debug` and `trace`. `trace` is the workspace's compiled-in file default AND the
    /// thing an operator types when debugging, which is exactly the setting that wrote secrets to
    /// disk.
    #[test]
    fn credential_bearing_targets_are_pinned_at_the_levels_that_leak() {
        for base in ["trace", "debug"] {
            let file = file_level_directive(None, base);
            let console = filter_directive(None, None, base);
            for t in CREDENTIAL_BEARING_TARGETS {
                assert!(
                    file.contains(&format!("{t}=info")),
                    "file directive for base {base:?} must pin {t}, got {file:?}"
                );
                assert!(
                    console.contains(&format!("{t}=info")),
                    "console directive for base {base:?} must pin {t}, got {console:?}"
                );
            }
        }
        // ...and the operator's own env value is pinned just the same — THE regression that
        // matters, because `VIKE_LOG_FILE_LEVEL=trace` is the obvious thing to set when debugging.
        let d = file_level_directive(Some("trace"), "warn");
        assert!(d.contains("ureq=info") && d.contains("tungstenite=info"), "got {d:?}");
    }

    /// The Windows Vulkan loader's registry chatter fires at WARN on EVERY `vike-app` start on this
    /// box — twice, before an adapter is even chosen, and the app then renders fine. Pinned to
    /// `error` on the console so a genuine Vulkan instance failure still gets through.
    #[test]
    fn noisy_third_party_targets_are_pinned_on_the_console() {
        for base in ["warn", "info", "debug", "trace"] {
            let console = filter_directive(None, None, base);
            for (target, level) in NOISY_TARGETS {
                assert!(
                    console.contains(&format!("{target}={level}")),
                    "console directive for base {base:?} must pin {target}, got {console:?}"
                );
            }
        }
    }

    /// The FILE layer is the forensic record and is NOT quieted — the chatter still lands there.
    #[test]
    fn the_noise_pins_do_not_touch_the_file_layer() {
        for base in ["warn", "info", "debug", "trace"] {
            let file = file_level_directive(None, base);
            for (target, _) in NOISY_TARGETS {
                assert!(!file.contains(target), "file directive must not pin {target}: {file:?}");
            }
        }
    }

    /// An EXPLICIT mention wins, exactly as it does for the credential pins — someone debugging the
    /// Vulkan backend can still ask for it.
    #[test]
    fn an_explicitly_named_noisy_target_is_not_pinned_over() {
        let d = filter_directive(Some("info,wgpu_hal::vulkan::instance=trace"), None, "info");
        assert!(d.contains("wgpu_hal::vulkan::instance=trace"), "{d:?}");
        assert!(!d.contains("wgpu_hal::vulkan::instance=error"), "no second directive: {d:?}");
    }

    /// ⚠ A pin may only ever NARROW. At a base that already excludes `debug`/`trace`, appending
    /// `ureq=info` would RAISE those targets — and `off` is the case that makes it obvious:
    /// `off,ureq=info` writes `ureq` info lines into a file the operator explicitly turned OFF. A
    /// redaction that turns logging back on is not a redaction.
    #[test]
    fn a_pin_never_raises_a_target_above_the_base_level() {
        for base in ["off", "error", "warn", "info"] {
            let file = file_level_directive(None, base);
            let console = filter_directive(None, None, base);
            assert_eq!(file, base, "base {base:?} already excludes the leak — no pin may be added");
            // The console carries the NOISE pins too, and they obey the same rule from the other
            // side: one appears exactly when the base would otherwise ADMIT that target's chatter,
            // and never at a base that already excludes it. Compared EXACTLY, so an unexpected
            // directive of any kind still fails here.
            assert_eq!(
                console,
                expected_console(base),
                "same for the console filter at base {base:?}"
            );
        }
        // the same via the env knob, which is how an operator actually silences the file
        assert_eq!(file_level_directive(Some("off"), "trace"), "off");
        assert_eq!(file_level_directive(Some("warn"), "trace"), "warn");
    }

    /// `base` plus exactly those [`NOISY_TARGETS`] pins that NARROW it — derived from the rule
    /// rather than from the implementation, so the two have to agree.
    fn expected_console(base: &str) -> String {
        let mut out = base.to_string();
        for (target, level) in NOISY_TARGETS {
            if level_rank(base) > level_rank(level) {
                out.push_str(&format!(",{target}={level}"));
            }
        }
        out
    }

    /// An EXPLICIT mention of a pinned target wins — transport debugging stays possible, but only
    /// as a deliberate opt-in, never as a side effect of raising the global level.
    #[test]
    fn an_explicit_target_directive_is_not_overridden_by_the_pin() {
        let d = file_level_directive(Some("trace,ureq=trace"), "warn");
        assert!(d.contains("ureq=trace"), "the operator's own ureq directive must survive: {d:?}");
        assert!(
            !d.contains("ureq=info"),
            "and must not be shadowed by a second, conflicting ureq directive: {d:?}"
        );
        // the target they did NOT name is still pinned
        assert!(d.contains("tungstenite=info"), "got {d:?}");
    }

    /// In-memory `MakeWriter` that captures everything written to it, so a test can inspect the
    /// bytes a layer produced without touching the real stderr/global subscriber.
    #[derive(Clone, Default)]
    struct SharedBuf(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

    impl std::io::Write for SharedBuf {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().write(buf)
        }
        fn flush(&mut self) -> std::io::Result<()> {
            self.0.lock().unwrap().flush()
        }
    }

    /// **A live reload actually changes what the layer emits** — the property `vike-tradehub`'s
    /// hot-apply seam rests on. Built locally (`with_default`, never the global subscriber): a
    /// `debug!` is filtered at the boot level `info`, [`LogReloadHandles::reload_console_level`]
    /// swaps the filter to `debug`, and the same `debug!` is then captured.
    ///
    /// ...and the [`CREDENTIAL_BEARING_TARGETS`] pins SURVIVE the reload: after hot-raising to
    /// `debug`, a `ureq`-target `debug!` (the level at which `ureq::run` logs full URLs) must
    /// still be filtered, because the handle recomposes through [`filter_directive`] rather than
    /// swapping in a caller-built filter. A reload API that took a raw directive would fail this.
    #[test]
    fn a_console_reload_applies_live_and_keeps_the_credential_pins() {
        let buf = SharedBuf::default();
        let make_writer = {
            let buf = buf.clone();
            move || buf.clone()
        };
        let (filter, handle): (reload::Layer<EnvFilter, Registry>, _) =
            reload::Layer::new(EnvFilter::new("info"));
        let layer = console_layer(ConsoleFormat::Json, false, filter, make_writer);
        let layers: Vec<Box<dyn Layer<Registry> + Send + Sync>> = vec![layer];
        let subscriber = tracing_subscriber::registry().with(layers);
        let handles = LogReloadHandles { console: handle, file: None };
        tracing::subscriber::with_default(subscriber, || {
            tracing::debug!(marker = "before_reload", "filtered at the boot level");
            handles
                .reload_console_level(None, None, "debug")
                .expect("the console filter must reload");
            tracing::debug!(marker = "after_reload", "captured at the reloaded level");
            tracing::debug!(target: "ureq::run", marker = "pinned_target", "full-URL leak shape");
        });

        let out = String::from_utf8(buf.0.lock().unwrap().clone()).expect("utf8 output");
        assert!(!out.contains("before_reload"), "the boot-level filter must hold first: {out}");
        assert!(out.contains("after_reload"), "the reloaded level must apply live: {out}");
        assert!(
            !out.contains("pinned_target"),
            "the ureq=info credential pin must survive a hot reload to debug: {out}"
        );
    }

    /// The file half of the reload contract: with NO reloadable file layer installed,
    /// [`LogReloadHandles::reload_file_level`] answers `Err` — the honest signal the caller turns
    /// into restart-required — and the env>config precedence composes through
    /// [`file_level_directive`] when one IS installed (proven by swapping a `trace` layer down to
    /// `off` and seeing nothing further emitted).
    #[test]
    fn a_file_reload_is_refused_without_a_file_layer_and_applies_with_one() {
        let none = LogReloadHandles { console: reload_probe_handle(), file: None };
        let err = none.reload_file_level(None, "warn").expect_err("no file layer ⇒ Err");
        assert!(err.contains("no reloadable FILE layer"), "{err}");

        let buf = SharedBuf::default();
        let make_writer = {
            let buf = buf.clone();
            move || buf.clone()
        };
        let (filter, handle): (reload::Layer<EnvFilter, Registry>, _) =
            reload::Layer::new(EnvFilter::new(file_level_directive(None, "trace")));
        let layer: Box<dyn Layer<Registry> + Send + Sync> =
            fmt::layer().json().with_writer(make_writer).with_filter(filter).boxed();
        let layers: Vec<Box<dyn Layer<Registry> + Send + Sync>> = vec![layer];
        let subscriber = tracing_subscriber::registry().with(layers);
        let handles = LogReloadHandles { console: reload_probe_handle(), file: Some(handle) };
        tracing::subscriber::with_default(subscriber, || {
            tracing::info!(marker = "file_before_reload", "captured at trace");
            handles.reload_file_level(Some("off"), "trace").expect("the file filter must reload");
            tracing::info!(marker = "file_after_off", "must NOT be captured");
        });

        let out = String::from_utf8(buf.0.lock().unwrap().clone()).expect("utf8 output");
        assert!(out.contains("file_before_reload"), "{out}");
        assert!(!out.contains("file_after_off"), "`off` must apply live to the file layer: {out}");
    }

    /// A detached console handle for tests that only exercise the FILE half.
    fn reload_probe_handle() -> ReloadableFilterHandle {
        let (_layer, handle): (reload::Layer<EnvFilter, Registry>, _) =
            reload::Layer::new(EnvFilter::new("info"));
        handle
    }

    #[test]
    fn console_json_format_emits_json() {
        let buf = SharedBuf::default();
        let make_writer = {
            let buf = buf.clone();
            move || buf.clone()
        };
        let layer = console_layer(ConsoleFormat::Json, false, EnvFilter::new("trace"), make_writer);
        let subscriber = tracing_subscriber::registry().with(layer);
        tracing::subscriber::with_default(subscriber, || {
            tracing::info!(marker = "console_json_test", "hello from the console json test");
        });

        let out = String::from_utf8(buf.0.lock().unwrap().clone()).expect("utf8 output");
        assert!(out.trim_start().starts_with('{'), "expected JSON console output, got: {out}");
        assert!(
            out.contains("console_json_test"),
            "expected the event field in the output, got: {out}"
        );
    }

    #[test]
    fn console_pretty_format_does_not_emit_json() {
        let buf = SharedBuf::default();
        let make_writer = {
            let buf = buf.clone();
            move || buf.clone()
        };
        let layer =
            console_layer(ConsoleFormat::Pretty, true, EnvFilter::new("trace"), make_writer);
        let subscriber = tracing_subscriber::registry().with(layer);
        tracing::subscriber::with_default(subscriber, || {
            tracing::info!(marker = "console_pretty_test", "hello from the console pretty test");
        });

        let out = String::from_utf8(buf.0.lock().unwrap().clone()).expect("utf8 output");
        assert!(!out.trim_start().starts_with('{'), "expected non-JSON pretty output, got: {out}");
        assert!(
            out.contains("console_pretty_test"),
            "expected the event field in the output, got: {out}"
        );
    }

    /// END-TO-END: a credential-shaped URL logged by the transport stack cannot reach the FILE
    /// layer at the DEFAULT level.
    ///
    /// The directive tests above assert the filter STRING; this one builds the real JSON file layer
    /// over the real composed directive and emits the exact records `ureq` and `tungstenite` emit —
    /// same targets, same levels, same shape — then asserts the secrets are absent from the bytes
    /// that were written. It fails if the pins are dropped, if they are pinned at `debug` (which
    /// `tungstenite::client`'s ungated `debug!` still leaks through), or if a future refactor stops
    /// composing them into the file filter.
    ///
    /// The three payloads are the real leak shapes, not invented ones: the Telegram bot token is a
    /// PATH segment, the binance listenKey is a PATH segment of the user-data WS URL, and vendor
    /// API keys are QUERY parameters.
    #[test]
    fn a_credential_shaped_url_never_reaches_the_file_layer_at_the_default_level() {
        const BOT_TOKEN: &str = "1234567890:AAHsupersecrettelegramtokenvalue";
        const LISTEN_KEY: &str = "pqia91ma19a5supersecretlistenkeyvalue";
        const VENDOR_KEY: &str = "supersecretvendorapikeyvalue";

        let buf = SharedBuf::default();
        let make_writer = {
            let buf = buf.clone();
            move || buf.clone()
        };
        // EXACTLY what `init` builds for the file layer on a default `LogConfig` (file_level
        // "trace", no `VIKE_LOG_FILE_LEVEL` set).
        let directive = file_level_directive(None, &LogConfig::default().file_level);
        let layer =
            fmt::layer().json().with_writer(make_writer).with_filter(EnvFilter::new(directive));
        let subscriber = tracing_subscriber::registry().with(layer);

        tracing::subscriber::with_default(subscriber, || {
            // `ureq::run`'s `debug!("{method} {DebugUri}")`, with the query it prints at trace.
            tracing::debug!(
                target: "ureq::run",
                "POST https://api.telegram.org/bot{BOT_TOKEN}/sendMessage",
            );
            tracing::debug!(
                target: "ureq::run",
                "GET https://finnhub.io/api/v1/quote?symbol=AAPL&token={VENDOR_KEY}",
            );
            // `tungstenite::client::connect_to_some`'s ungated `debug!("Trying to contact {uri} …")`
            // — the reason the pin is `info` and not `debug`.
            tracing::debug!(
                target: "tungstenite::client",
                "Trying to contact wss://stream.binance.com:9443/ws/{LISTEN_KEY} at 1.2.3.4:9443...",
            );
            // `tungstenite::handshake::client`'s `trace!("Request: {:?}")` — the whole raw upgrade
            // request, path and every header, with no redaction of any kind.
            tracing::trace!(
                target: "tungstenite::handshake::client",
                "Request: \"GET /ws/{LISTEN_KEY} HTTP/1.1\\r\\nAuthorization: Bearer {VENDOR_KEY}\\r\\n\"",
            );
            // ...and OUR OWN crates must still get full trace into the file — losing that is the
            // real cost the per-target pin exists to avoid, so it is asserted, not assumed.
            tracing::trace!(target: "vike_exec::oms", marker = "our_own_trace_survives", "hop");
        });

        let out = String::from_utf8(buf.0.lock().unwrap().clone()).expect("utf8 output");
        for (what, secret) in [
            ("telegram bot token", BOT_TOKEN),
            ("binance listenKey", LISTEN_KEY),
            ("vendor api key", VENDOR_KEY),
        ] {
            assert!(
                !out.contains(secret),
                "the {what} reached the trace FILE layer at the default level — \
                 every app-level redaction is bypassed below this point. Output: {out}"
            );
        }
        assert!(
            out.contains("our_own_trace_survives"),
            "our own crates must keep trace-level file logging — a blanket level drop would have \
             been the cheaper fix and this is what it would have cost. Output: {out}"
        );
    }
}
