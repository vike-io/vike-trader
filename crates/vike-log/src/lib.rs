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
//! That "cannot" is HELD, not only stated, since 2026-09-29:
//! `crates/vike-ops/tests/layer_gate.rs`'s `every_vike_free_crate_names_no_vike_crate` fails on any
//! `vike-*` normal dependency here, through this crate's `VIKE_FREE_CRATES` row. ⚠ Until then the
//! only check was that file's tier-15 rule, which is weaker: a `leaf` may still name `vike-model`
//! (10), and `vike_model::state_path`'s walkers would then be one call away.
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
    with_volume_target_pins(&with_credential_target_pins(&base))
}

/// [`file_level_directive`], plus caller-supplied targets the global level may not SILENCE.
///
/// The three families above ([`with_credential_target_pins`], [`with_volume_target_pins`],
/// [`with_noise_target_pins`]) only ever NARROW a target: each exists because something writes too
/// much, or writes a secret, into a file at a level the operator did not think about. This one is
/// their inverse and exists for the opposite failure — a record that MUST survive the level the
/// operator did think about. It is supplied by the BINARY rather than listed here, because this
/// crate is the bottom of the graph and takes no `vike-*` DEPENDENCY — so a pin cannot be a
/// constant in this file at all: the target it names belongs to a crate this one cannot see, and
/// the edge that would let it be named in code is the one `crates/vike-ops/tests/layer_gate.rs`
/// refuses. Prose may cite it; the compiler may not. So the crate that emits the line is the crate
/// that declares it must not be lost (`vike_tradehub::audit::FILE_PIN`).
///
/// Three rules, and each is the same shape as the narrowing families' guards read backwards:
///
/// - **A pin may only ever RAISE.** One whose level is not MORE verbose than the resolved global
///   level is dropped — at `trace`, pinning a target to `info` would quieten it, which is the
///   opposite of the point, and would hand this family the very defect it closes.
/// - **An EXPLICIT mention wins.** A directive that already names the target
///   (`VIKE_LOG_FILE_LEVEL=warn,vike_tradehub::audit=debug`) is left alone, so a deliberate,
///   typed-out level for that target is never fighting a duplicate directive appended behind it.
///   ⚠ That is also the one way to turn a pinned target DOWN, and it is deliberate that it takes
///   naming the target: raising or lowering the GLOBAL level — the thing a `deploy/*.service`
///   `Environment=` line and a hot `SetSetting` both do — cannot reach it.
/// - **An unreadable global is left ALONE**, never guessed at, exactly as the other three families
///   leave it. A directive with no bare level (`ureq=trace`) states no global for a pin to be
///   compared against; `EnvFilter` is the authority on what such a directive admits, and this crate
///   does not second-guess it. The residual is named rather than closed: such a directive silences
///   a pinned target too, and nothing in the shipped units writes one.
///
/// ⚠ **`off` is not an exemption.** It is an operator-set global like any other, and the failure
/// this family closes is precisely an environment value silencing a security record. A binary that
/// genuinely wants no file at all sets [`LogConfig::file_enabled`], which is the binary's own
/// decision rather than the environment's.
pub fn file_level_directive_with_pins(
    env_level: Option<&str>,
    cfg_level: &str,
    pins: &[(String, String)],
) -> String {
    let base = file_level_directive(env_level, cfg_level);
    if pins.is_empty() {
        return base;
    }
    // The bare global level = a directive with no `target=` part, read exactly as the three
    // narrowing families read it.
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

/// Per-FRAME log targets, pinned out of the file's default `trace`, with the level each keeps.
///
/// ⚠ These ARE chosen for volume, which is what makes them a different family from
/// [`CREDENTIAL_BEARING_TARGETS`] (a leak, at any volume) and [`NOISY_TARGETS`] (one line per start,
/// on the console). A render loop emits at frame rate, so `trace` on these targets is not a verbose
/// log — it is a disk filling at a constant rate for as long as the window is open.
///
/// MEASURED 2026-09-03 on a freshly installed Windows desktop client, no `VIKE_LOG_FILE_LEVEL` set
/// — the default an ordinary user gets: **5.8 GB in 4h10m**, about 1.4 GB/hour, so a client left
/// open overnight writes ~11 GB. Reading that file back, `vike`'s OWN lines numbered about
/// twenty-five for the whole session; everything else was `wgpu_core::command`,
/// `wgpu_core::device::{global,queue}`, `wgpu_core::resource` and `eframe::native::run`'s
/// `event_result`, tens of lines per frame. `LogConfig::file_max_files` cannot help: it prunes
/// DAILY files, and one day IS the 5.8 GB.
///
/// `info` rather than `warn` or `off`, deliberately: wgpu's one-time adapter enumeration and device
/// creation lines are exactly what a GPU bug report needs, and they are logged once. What is dropped
/// is the per-frame `debug`/`trace` lifecycle — the volume — while a genuine device-lost or
/// validation error still reaches the file at `warn`/`error`.
///
/// ⚠ `eframe::native` is the SUBTREE, not `eframe` whole and not one module of it. The first
/// attempt pinned `eframe::native::run` alone, because that is the module the 5.8 GB file showed —
/// and RUNNING the fix is what corrected it: over 75 s of a rebuilt client, wgpu and naga fell to
/// TWO lines, and **2167 of the 2561 that remained were `eframe::native::wgpu_integration`**, a
/// sibling module the pin could not match. `eframe::native` is the whole windowing/render/event
/// loop, which is the thing that emits per frame; `eframe`'s other modules (the `epi` glue, startup)
/// keep their levels. A module was the wrong unit here, where it is the right one for
/// [`NOISY_TARGETS`]'s `wgpu_hal::vulkan::instance` — that really is one line from one function.
pub const HIGH_VOLUME_TARGETS: &[(&str, &str)] =
    &[("wgpu_core", "info"), ("wgpu_hal", "info"), ("naga", "info"), ("eframe::native", "info")];

/// Append a pin for every [`HIGH_VOLUME_TARGETS`] entry the caller's directive does not already
/// name. FILE layer — the console has its own [`NOISY_TARGETS`] family and a different problem
/// (a warning nobody reads, rather than a disk nobody watches).
///
/// Same two guards as the other two families, and they carry the same weight here:
///
/// - **An EXPLICIT mention wins**, so `VIKE_LOG_FILE_LEVEL=wgpu_core=trace` still gets the firehose
///   when someone is actually debugging the renderer. That is the whole escape hatch, and it has to
///   be typed rather than arrived at by raising the global level.
/// - **A pin may only ever NARROW.** Appending `wgpu_core=info` to a directive already at
///   `off`/`error`/`warn` would turn those targets back ON in a file the operator quieted — a
///   volume cap that adds volume is not a cap.
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

/// Third-party log targets that print CREDENTIALS, pinned OUT of the trace firehose.
///
/// These are not chosen for volume. Each one writes a live secret into the log at a level this
/// workspace's file layer enables BY DEFAULT, below the layer where every app-level redaction
/// lives — `vike-tradehub`'s `telegram/deps.rs` and `vike-alerting`'s `delivery.rs` both redact
/// carefully, and are bypassed entirely by their own HTTP client logging the URL they were handed:
///
/// - **`ureq`** — `run.rs` logs `"{method} {DebugUri}"`. `DebugUri` is deliberately careful: it
///   prints the path+query ONLY `if log_enabled!(log::BookLevel::Trace)` and writes `/******`
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
/// get layer manifest files" (plus an `objects:` continuation line) at WARN on every `vike-desktop`
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

    /// Targets whose file-layer level the GLOBAL file level may not silence, as
    /// `(target, level)` pairs — see [`file_level_directive_with_pins`] for the three rules and
    /// the one residual.
    ///
    /// Empty by default, so every caller that does not set it composes byte-identically to before
    /// this field existed. It is a `LogConfig` field, and therefore the BINARY's declaration,
    /// because this crate is the bottom of the graph and takes no `vike-*` dependency: a pin belongs
    /// beside the `tracing` call it protects, not in a list down here that would rot the moment
    /// that call moved.
    ///
    /// ⚠ It is not a convenience. `deploy/vike-tradehub.service` sets
    /// `Environment=VIKE_LOG_FILE_LEVEL=warn` to bound the write rate, and
    /// `vike_tradehub::audit::record` — the record of every ACCEPTED control command on a daemon
    /// that signs real orders — emits at `info`. Measured on the live the CI box box: a 23 MB log file
    /// held 53,160 ERROR lines, 1,928 WARN lines and **zero INFO**. The audit trail was not thin
    /// there; it was absent.
    pub file_target_pins: Vec<(String, String)>,
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
            // No pins: the audit-trail family is opt-in, and a caller that declares none composes
            // exactly the directive it composed before `file_target_pins` existed.
            file_target_pins: Vec::new(),
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
    /// The [`LogConfig::file_target_pins`] this process booted with, carried so a hot reload
    /// recomposes the SAME directive a restart would. Without it a `SetSetting` on
    /// `preferences.log_file_level` — a change an operator makes to turn the file DOWN — would drop
    /// the audit pin for the rest of the process's life, which is the M2 defect wearing a different
    /// trigger.
    file_pins: Vec<(String, String)>,
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
        let directive = file_level_directive_with_pins(env_level, cfg_level, &self.file_pins);
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
        let file_directive = file_level_directive_with_pins(
            std::env::var("VIKE_LOG_FILE_LEVEL").ok().as_deref(),
            &cfg.file_level,
            &cfg.file_target_pins,
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
    (
        guards,
        // The pins travel WITH the handles so a hot `SetSetting` on `preferences.log_file_level`
        // recomposes the same directive this boot composed — see the field's own doc for why a
        // reload that dropped them would be the same defect wearing a different trigger.
        LogReloadHandles {
            console: console_handle,
            file: file_handle,
            file_pins: cfg.file_target_pins,
        },
    )
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
mod lib_tests;
