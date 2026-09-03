//! `incident` — freeze the perishable evidence of a live run into ONE timestamped bundle.
//!
//! A live trading node's forensic trail is scattered across short-lived, overwrite-prone stores:
//! the write-ahead command journal ([`vike_core::journal`], mmap segments that PRUNE), the
//! daily-rolling JSON trace log ([`vike_log`], which ROLLS + can be size-capped), the resolved
//! [`vike_core::RunProfile`] (a file that can be edited between runs), and the process environment
//! (gone the instant the process exits). When something goes wrong at 03:00, the operator needs a
//! SINGLE artifact that pins all of it to the moment of the incident before any of it rots.
//!
//! This module is that artifact's collector. Given a window (`--since <dur>`), [`run_incident`]
//! copies into one timestamped directory:
//!   - the journal segment files whose last-write overlaps the window (gzipped, per file),
//!   - the trace-log files from `$VIKE_LOG_DIR` whose last-write overlaps the window (gzipped),
//!   - the resolved `RunProfile` TOML + its FNV-1a64 hash (so a later edit is detectable),
//!   - the latest [`vike_exec::EngineSnapshot`] + `state_hash` from the journal's last `Snap`,
//!   - git SHA / build info, and
//!   - a REDACTED dump of the process environment — secret-shaped values masked EXACTLY the way
//!     the signers' manual `Debug` impls do (`crates/vike-bridge-core/src/signer.rs`: `***` + the
//!     last four chars), so a shared bundle never carries an API secret / private key.
//!
//! ## Off / additive
//! This is a brand-new subcommand (the `incident` bin). Nothing in the existing mount path
//! calls it; the live core is untouched and no existing struct gains a field. It is a READ-ONLY
//! collector — it opens the journal through the lock-free [`vike_core::journal::CommandJournal::
//! read_all`] entry point the journal module blesses for "offline inspection tooling", never the
//! writer — so it is safe to run against a live node.
//!
//! ## Archive shape (per-file gz in a directory, NOT a single `tar.gz`)
//! Each frozen file is gzipped individually (the [`flate2`] `GzEncoder` pattern already used by the
//! Polymarket raw-frame tap) into `incident-<UTC>/…`, alongside a plain `manifest.json`. A single
//! `.tar.gz` would be tidier but needs a `tar` crate whose API this crate cannot compile-check in
//! the current build lane, so the per-file-gz fallback is used deliberately (bundling the directory
//! into one `.tar.gz` afterward is a trivial, dependency-free follow-up).
//!
//! ## Purity for testability
//! The load-bearing logic is pure and unit-tested: [`select_overlapping`] (the window→file
//! selection), [`redact_env`] / [`redact_secret_value`] (the secret masking), and [`build_manifest`]
//! (the manifest assembly, which redacts internally so the artifact is safe by construction). The
//! impure orchestration ([`run_incident_at`]) is a thin shell over those plus `std::fs` + `flate2`.

use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use flate2::write::GzEncoder;
use flate2::Compression;
use serde::{Deserialize, Serialize};

use vike_core::journal::{CommandJournal, JournalRecord};
use vike_exec::state_hash;
use vike_model::time::civil_from_days;

// ─────────────────────────────────────────────────────────────────────────────────────────────
// Pure helpers (unit-tested below)
// ─────────────────────────────────────────────────────────────────────────────────────────────

/// Window-overlap file selection — the pure heart of the collector. Given each candidate file's
/// last-modified time in epoch-**ms** paired with the file (any `Clone` label) and the incident
/// window `[now_ms - since_ms, now_ms]`, return the labels whose mtime is AT OR AFTER the cutoff.
///
/// Sound because the stores this selects over are append-only and time-ordered: a journal segment
/// (or a daily log file) whose LAST write is before the cutoff can hold no record inside the
/// window, and a segment that straddles the cutoff (started earlier, still being written) has a
/// last-write at/after the cutoff and so is included — carrying the window's records plus some
/// older context, which is fine for evidence. The bound is INCLUSIVE (`>=`) so a file written
/// exactly at the cutoff is kept. Generic so the same tested function selects journal segments AND
/// trace-log files. `since_ms` is subtracted saturating, so a huge window can never overflow.
pub fn select_overlapping<T: Clone>(items: &[(T, i64)], now_ms: i64, since_ms: i64) -> Vec<T> {
    let cutoff = now_ms.saturating_sub(since_ms);
    items.iter().filter(|(_, mtime)| *mtime >= cutoff).map(|(t, _)| t.clone()).collect()
}

/// Does this env-var KEY name a secret whose value must be masked? Generous, case-insensitive
/// substring match — over-redaction is SAFE (it only costs an operator a non-secret flag), while a
/// miss leaks a credential. Covers every credential shape the workspace uses: `*_API_KEY` /
/// `*_API_SECRET` / `*_API_PASSPHRASE` (`vike-bridge-core/src/credentials.rs`) and `*_PRIVATE_KEY`
/// / `POLY_*_PK` (`bridges/polymarket/src/config.rs`), plus tokens / mnemonics / seeds / signatures.
/// The `VIKE_*` runtime flags carry none of these needles, so they pass through verbatim.
pub fn is_secret_key(key: &str) -> bool {
    const NEEDLES: &[&str] = &[
        "SECRET",
        "PASSWORD",
        "PASSWD",
        "PASSPHRASE",
        "TOKEN",
        "PRIVATE",
        "PRIVKEY",
        "MNEMONIC",
        "SEED",
        "SIGNATURE",
        "CREDENTIAL",
        "APIKEY",
        "API_KEY",
        "_KEY",
        "_PK",
        "WALLET",
    ];
    let up = key.to_ascii_uppercase();
    NEEDLES.iter().any(|&n| up.contains(n))
}

/// Mask a secret value EXACTLY as the signers' manual `Debug` impls do
/// (`crates/vike-bridge-core/src/signer.rs`): `***` plus the last four characters as a coarse
/// fingerprint, never the whole secret. Char-boundary-safe — env values are usually ASCII, but this
/// must never panic on a multibyte value (unlike the signers' byte-slice, which is only ever fed an
/// ASCII api-key). A value shorter than four chars masks to a bare `***`.
pub fn redact_secret_value(val: &str) -> String {
    let n = val.chars().count();
    let tail: String = if n >= 4 { val.chars().skip(n - 4).collect() } else { String::new() };
    format!("***{tail}")
}

/// Redact + normalize an environment dump: sort by key (deterministic output) and mask the value
/// of every secret-shaped key ([`is_secret_key`] → [`redact_secret_value`]). Pure over a provided
/// slice so it is tested without touching the process env; the binary feeds it `std::env::vars()`.
pub fn redact_env(vars: &[(String, String)]) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = vars
        .iter()
        .map(|(k, v)| {
            if is_secret_key(k) {
                (k.clone(), redact_secret_value(v))
            } else {
                (k.clone(), v.clone())
            }
        })
        .collect();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

/// FNV-1a **64-bit** of `bytes`, rendered as 16 lowercase hex digits — the same algorithm/constants
/// as [`vike_exec::state_hash`], inline so this module needs no hashing dependency. Used to pin the
/// resolved `RunProfile` TOML so a later edit to the profile file is detectable from the bundle.
pub fn fnv1a64_hex(bytes: &[u8]) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{h:016x}")
}

/// Split epoch-ms into UTC `(year, month, day, hour, minute, second, milli)` via the workspace's
/// chrono-free civil-calendar math ([`vike_model::time::civil_from_days`]).
fn utc_parts(ms: i64) -> (i64, u32, u32, u32, u32, u32, u32) {
    let (y, mo, d) = civil_from_days(ms.div_euclid(86_400_000));
    let ms_of_day = ms.rem_euclid(86_400_000);
    let secs = ms_of_day / 1000;
    let millis = (ms_of_day % 1000) as u32;
    let h = (secs / 3600) as u32;
    let mi = ((secs % 3600) / 60) as u32;
    let s = (secs % 60) as u32;
    (y, mo, d, h, mi, s, millis)
}

/// Epoch-ms → `YYYYMMDDThhmmssZ` — a filesystem-safe compact UTC stamp for the bundle directory name.
pub fn utc_stamp_compact(ms: i64) -> String {
    let (y, mo, d, h, mi, s, _) = utc_parts(ms);
    format!("{y:04}{mo:02}{d:02}T{h:02}{mi:02}{s:02}Z")
}

/// Epoch-ms → `YYYY-MM-DDThh:mm:ss.sssZ` — RFC3339-ish UTC stamp for the manifest's human fields.
pub fn utc_stamp_rfc3339(ms: i64) -> String {
    let (y, mo, d, h, mi, s, millis) = utc_parts(ms);
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{s:02}.{millis:03}Z")
}

// ─────────────────────────────────────────────────────────────────────────────────────────────
// Manifest DTOs (Serialize + Deserialize so tests can round-trip the written artifact)
// ─────────────────────────────────────────────────────────────────────────────────────────────

/// The incident window, echoed into the manifest.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WindowInfo {
    pub since_ms: i64,
    pub cutoff_ms: i64,
    pub cutoff_utc: String,
    pub now_ms: i64,
    pub now_utc: String,
}

/// Package / git / build provenance of the binary that produced the bundle.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitBuildInfo {
    pub pkg_name: String,
    pub pkg_version: String,
    /// `"debug"` / `"release"` (from `cfg!(debug_assertions)`).
    pub build_profile: String,
    /// `git rev-parse HEAD`, best-effort — `None` when not run inside a repo.
    pub git_sha: Option<String>,
    /// whether `git status --porcelain` reported a dirty tree — `None` when git was unavailable.
    pub git_dirty: Option<bool>,
}

/// The resolved `RunProfile` TOML the run loaded (from `--profile` / `VIKE_RUN_PROFILE`), pinned.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProfileInfo {
    pub path: String,
    /// whether the file was read at all.
    pub loaded: bool,
    /// whether it parsed+validated as a real [`vike_core::RunProfile`].
    pub valid: bool,
    /// FNV-1a64 hex of the raw TOML bytes ([`fnv1a64_hex`]); `None` if the file could not be read.
    pub sha_fnv1a64: Option<String>,
    /// the gzipped copy's name inside the bundle; `None` if the copy failed.
    pub archive_name: Option<String>,
    /// a read/parse error message, if any.
    pub error: Option<String>,
}

/// One frozen evidence file: what it is called in the bundle, where it came from, and its sizes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CollectedFile {
    pub archive_name: String,
    pub source_path: String,
    pub source_bytes: u64,
    pub gz_bytes: u64,
    pub mtime_ms: i64,
    pub mtime_utc: String,
}

/// The journal segments frozen for the window.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JournalCollection {
    pub dir: String,
    pub segments_total: usize,
    pub segments_selected: usize,
    pub files: Vec<CollectedFile>,
}

/// The trace-log files frozen for the window.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogCollection {
    pub dir: String,
    pub files_total: usize,
    pub files_selected: usize,
    pub files: Vec<CollectedFile>,
}

/// One engine's headline in the latest journal `Snap` — no order/fill bodies, just the counts an
/// operator triages by (the full [`vike_exec::EngineSnapshot`]s ride in `engine_snapshot.json`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EngineSummary {
    pub venue: String,
    pub symbol: String,
    pub orders: usize,
    pub positions: usize,
}

/// The latest reachable OMS state, extracted from the journal's most recent `Snap` record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EngineState {
    /// `true` iff a `Snap` was found; `false` means no journal / no snapshot yet (still valid).
    pub reachable: bool,
    pub journal_dir: Option<String>,
    /// the `hash` the runtime stamped on the `Snap` (its `state_hash` at snapshot time).
    pub state_hash_stored: Option<u64>,
    pub state_hash_hex: Option<String>,
    /// [`vike_exec::state_hash`] recomputed over the restored engines — a cross-check of the stored
    /// value (they match on a clean journal; a divergence is itself diagnostic).
    pub state_hash_recomputed: Option<u64>,
    pub engine_count: usize,
    pub engines: Vec<EngineSummary>,
    /// the full-snapshot JSON file's name inside the bundle; `None` if unreachable / write failed.
    pub snapshot_archive: Option<String>,
}

impl EngineState {
    /// The "no reachable snapshot" state (no journal dir, or a dir with no `Snap`) — still a valid,
    /// serializable manifest section.
    fn unreachable(dir: Option<&Path>) -> Self {
        EngineState {
            reachable: false,
            journal_dir: dir.map(|d| d.display().to_string()),
            state_hash_stored: None,
            state_hash_hex: None,
            state_hash_recomputed: None,
            engine_count: 0,
            engines: Vec::new(),
            snapshot_archive: None,
        }
    }
}

/// The bundle manifest — the one plain-JSON index of everything frozen.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IncidentManifest {
    /// fixed tag identifying the artifact kind.
    pub kind: String,
    /// manifest schema version.
    pub schema: u32,
    pub created_ms: i64,
    pub created_utc: String,
    pub window: WindowInfo,
    pub build: GitBuildInfo,
    pub profile: Option<ProfileInfo>,
    pub journal: Option<JournalCollection>,
    pub logs: Option<LogCollection>,
    pub engine_state: EngineState,
    /// process environment, sorted, secret values masked ([`redact_env`]).
    pub env: Vec<(String, String)>,
}

/// Assemble the manifest from already-collected pieces. Pure — and it REDACTS the environment
/// internally ([`redact_env`]) so the manifest is secret-safe by construction, even if a caller
/// forgets. The window fields are derived from `now_ms`/`since_ms`.
#[allow(clippy::too_many_arguments)] // mirrors journal.rs::append_snap — one struct field per arg
pub fn build_manifest(
    now_ms: i64,
    since_ms: i64,
    build: GitBuildInfo,
    profile: Option<ProfileInfo>,
    journal: Option<JournalCollection>,
    logs: Option<LogCollection>,
    engine_state: EngineState,
    env_raw: &[(String, String)],
) -> IncidentManifest {
    let cutoff_ms = now_ms.saturating_sub(since_ms);
    IncidentManifest {
        kind: "vike-incident-bundle".to_string(),
        schema: 1,
        created_ms: now_ms,
        created_utc: utc_stamp_rfc3339(now_ms),
        window: WindowInfo {
            since_ms,
            cutoff_ms,
            cutoff_utc: utc_stamp_rfc3339(cutoff_ms),
            now_ms,
            now_utc: utc_stamp_rfc3339(now_ms),
        },
        build,
        profile,
        journal,
        logs,
        engine_state,
        env: redact_env(env_raw),
    }
}

// ─────────────────────────────────────────────────────────────────────────────────────────────
// Orchestration (impure — std::fs + flate2 + the read-only journal reader)
// ─────────────────────────────────────────────────────────────────────────────────────────────

/// What to collect and where to write it.
#[derive(Debug, Clone)]
pub struct IncidentConfig {
    /// the look-back window.
    pub since: Duration,
    /// parent directory the timestamped `incident-<UTC>/` bundle is created under.
    pub out_root: PathBuf,
    /// explicit journal directory; when `None`, derived from a loaded `RunProfile`'s journal sink.
    pub journal_dir: Option<PathBuf>,
    /// resolved trace-log directory (`$VIKE_LOG_DIR` / `<exe>/logs`); `None` skips log collection.
    pub log_dir: Option<PathBuf>,
    /// resolved `RunProfile` path (`--profile` / `VIKE_RUN_PROFILE`); `None` skips profile capture.
    pub profile_path: Option<PathBuf>,
}

/// Collect the incident bundle using the wall clock + the live process environment. Returns the
/// created bundle directory. See [`run_incident_at`] for the pure-inputs variant tests drive.
pub fn run_incident(cfg: &IncidentConfig) -> io::Result<PathBuf> {
    run_incident_at(cfg, vike_model::now_ms(), std::env::vars().collect())
}

/// [`run_incident`] with the clock (`now_ms`) and environment (`env_vars`) injected, so the whole
/// pipeline — file selection, gzip, manifest, redaction — is exercised deterministically without
/// touching the real clock/env (and without ever writing a real secret to disk in a test).
pub fn run_incident_at(
    cfg: &IncidentConfig,
    now_ms: i64,
    env_vars: Vec<(String, String)>,
) -> io::Result<PathBuf> {
    let since_ms = cfg.since.as_millis().min(i64::MAX as u128) as i64;
    let bundle = cfg.out_root.join(format!("incident-{}", utc_stamp_compact(now_ms)));
    std::fs::create_dir_all(&bundle)?;

    // ── resolved RunProfile TOML + its hash (captured), plus a best-effort parse used ONLY to derive
    // the journal-dir default. The tiny TOML is read twice (hash/copy vs. parse) — negligible for a
    // one-shot tool, and it keeps each step a plain, side-effect-free call.
    let profile = cfg.profile_path.as_deref().map(|pp| capture_profile(pp, &bundle));
    let parsed_profile =
        cfg.profile_path.as_deref().and_then(|pp| vike_core::RunProfile::from_path(pp).ok());

    // ── journal directory: explicit override, else the loaded profile's journal sink
    let journal_dir = cfg.journal_dir.clone().or_else(|| {
        parsed_profile
            .as_ref()
            .and_then(|p| p.sinks.journal.as_ref())
            .map(|j| PathBuf::from(&j.dir))
    });

    // ── journal segments overlapping the window + the latest EngineSnapshot / state_hash
    let mut journal = None;
    let mut engine_state = EngineState::unreachable(None);
    if let Some(jd) = &journal_dir {
        if jd.is_dir() {
            let (total, files) = collect_files(
                jd,
                |name| name.starts_with("journal-") && name.ends_with(".vjl"),
                now_ms,
                since_ms,
                &bundle.join("journal"),
            );
            journal = Some(JournalCollection {
                dir: jd.display().to_string(),
                segments_total: total,
                segments_selected: files.len(),
                files,
            });
            engine_state = latest_engine_state(jd, &bundle);
        } else {
            engine_state = EngineState::unreachable(Some(jd.as_path()));
        }
    }

    // ── trace-log slice overlapping the window
    let logs = match &cfg.log_dir {
        Some(ld) if ld.is_dir() => {
            let (total, files) =
                collect_files(ld, |_| true, now_ms, since_ms, &bundle.join("logs"));
            Some(LogCollection {
                dir: ld.display().to_string(),
                files_total: total,
                files_selected: files.len(),
                files,
            })
        }
        _ => None,
    };

    // ── manifest (redacts env internally) → plain JSON at the bundle root
    let build = git_build_info();
    let manifest =
        build_manifest(now_ms, since_ms, build, profile, journal, logs, engine_state, &env_vars);
    let json = serde_json::to_vec_pretty(&manifest).expect("IncidentManifest serializes");
    std::fs::write(bundle.join("manifest.json"), json)?;
    Ok(bundle)
}

/// A candidate file with the two facts selection + the manifest need.
#[derive(Clone)]
struct FileMeta {
    path: PathBuf,
    size: u64,
    mtime_ms: i64,
}

/// Metadata last-modified time as epoch-ms, saturating to 0 on the pre-epoch / unavailable path.
fn mtime_ms(meta: &std::fs::Metadata) -> i64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// List `dir`'s files matching `keep`, select the window-overlapping ones ([`select_overlapping`]),
/// gzip each into `dest_dir`, and return `(total_matched, collected)`. Best-effort throughout — a
/// dir/file/gzip failure is logged and skipped (freeze what you can), never propagated — so a single
/// unreadable file can't abort the whole bundle.
fn collect_files(
    dir: &Path,
    keep: impl Fn(&str) -> bool,
    now_ms: i64,
    since_ms: i64,
    dest_dir: &Path,
) -> (usize, Vec<CollectedFile>) {
    let read = match std::fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(e) => {
            tracing::warn!(dir = %dir.display(), %e, "incident: cannot read dir — skipping");
            return (0, Vec::new());
        }
    };
    let mut metas: Vec<FileMeta> = Vec::new();
    for entry in read {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };
        let meta = match entry.metadata() {
            Ok(m) => m,
            Err(_) => continue,
        };
        if !meta.is_file() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        if !keep(&name) {
            continue;
        }
        metas.push(FileMeta { path: entry.path(), size: meta.len(), mtime_ms: mtime_ms(&meta) });
    }
    let total = metas.len();
    let items: Vec<(FileMeta, i64)> = metas
        .into_iter()
        .map(|f| {
            let mt = f.mtime_ms;
            (f, mt)
        })
        .collect();
    let selected = select_overlapping(&items, now_ms, since_ms);

    let mut out = Vec::new();
    if !selected.is_empty() {
        if let Err(e) = std::fs::create_dir_all(dest_dir) {
            tracing::warn!(dir = %dest_dir.display(), %e, "incident: cannot create bundle subdir");
            return (total, Vec::new());
        }
    }
    for f in selected {
        let name = f.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let archive_name = format!("{name}.gz");
        let dst = dest_dir.join(&archive_name);
        match gzip_file(&f.path, &dst) {
            Ok(gz_bytes) => out.push(CollectedFile {
                archive_name,
                source_path: f.path.display().to_string(),
                source_bytes: f.size,
                gz_bytes,
                mtime_ms: f.mtime_ms,
                mtime_utc: utc_stamp_rfc3339(f.mtime_ms),
            }),
            Err(e) => {
                tracing::warn!(path = %f.path.display(), %e, "incident: gzip failed — skipping file")
            }
        }
    }
    (total, out)
}

/// Read the journal's LATEST `Snap` (via the lock-free reader) → its engines + `state_hash`, and
/// write the full engines out as `engine_snapshot.json`. Any read failure / absent snapshot yields
/// an `unreachable` state rather than an error — a node that has not snapshotted yet is normal.
fn latest_engine_state(dir: &Path, bundle: &Path) -> EngineState {
    let records = match CommandJournal::read_all(dir) {
        Ok(r) => r,
        Err(_) => return EngineState::unreachable(Some(dir)),
    };
    // read_all returns records in seq order; the last `Snap` is the most recent checkpoint.
    let latest = records.into_iter().rev().find_map(|r| match r {
        JournalRecord::Snap { engines, hash, .. } => Some((engines, hash)),
        _ => None,
    });
    let Some((engines, hash)) = latest else {
        return EngineState::unreachable(Some(dir));
    };
    let recomputed = state_hash(&engines);
    let summaries: Vec<EngineSummary> = engines
        .iter()
        .map(|e| EngineSummary {
            venue: e.venue.clone(),
            symbol: e.symbol.clone(),
            orders: e.registry.len(),
            positions: e.account.positions.len(),
        })
        .collect();
    // Full snapshots (trading state only — no credentials) written plain for offline replay/inspect.
    let archive = "engine_snapshot.json";
    let wrote = serde_json::to_vec_pretty(&engines)
        .ok()
        .and_then(|b| std::fs::write(bundle.join(archive), b).ok())
        .is_some();
    EngineState {
        reachable: true,
        journal_dir: Some(dir.display().to_string()),
        state_hash_stored: Some(hash),
        state_hash_hex: Some(format!("{hash:016x}")),
        state_hash_recomputed: Some(recomputed),
        engine_count: summaries.len(),
        engines: summaries,
        snapshot_archive: wrote.then(|| archive.to_string()),
    }
}

/// Best-effort git + compile-time build provenance. git runs as a subprocess in the CWD; on a
/// deployed binary far from the repo it simply yields `None`.
fn git_build_info() -> GitBuildInfo {
    let git = |args: &[&str]| -> Option<std::process::Output> {
        std::process::Command::new("git").args(args).output().ok().filter(|o| o.status.success())
    };
    let git_sha = git(&["rev-parse", "HEAD"])
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty());
    let git_dirty = git(&["status", "--porcelain"]).map(|o| !o.stdout.is_empty());
    let build_profile = if cfg!(debug_assertions) { "debug" } else { "release" };
    GitBuildInfo {
        pkg_name: env!("CARGO_PKG_NAME").to_string(),
        pkg_version: env!("CARGO_PKG_VERSION").to_string(),
        build_profile: build_profile.to_string(),
        git_sha,
        git_dirty,
    }
}

/// Stream `src` through gzip into `dst`; return the compressed byte count. The [`GzEncoder`] pattern
/// from the Polymarket raw-frame tap. Segments are pre-allocated (mostly zeros past the write
/// cursor), which gzip collapses to almost nothing — so freezing a 64 MiB segment costs a tiny file.
fn gzip_file(src: &Path, dst: &Path) -> io::Result<u64> {
    let mut input = std::fs::File::open(src)?;
    let out = std::fs::File::create(dst)?;
    let mut enc = GzEncoder::new(out, Compression::default());
    io::copy(&mut input, &mut enc)?;
    enc.finish()?;
    std::fs::metadata(dst).map(|m| m.len())
}

/// Freeze the resolved `RunProfile` TOML at `pp`: hash the raw bytes ([`fnv1a64_hex`], so a later
/// edit is detectable), gzip a copy into `bundle`, and note whether it parses+validates as a real
/// [`vike_core::RunProfile`]. A read failure yields a `loaded: false` record rather than aborting.
fn capture_profile(pp: &Path, bundle: &Path) -> ProfileInfo {
    match std::fs::read_to_string(pp) {
        Ok(text) => {
            let sha = fnv1a64_hex(text.as_bytes());
            let archive = "profile.toml.gz";
            let wrote = gzip_bytes(text.as_bytes(), &bundle.join(archive)).is_ok();
            ProfileInfo {
                path: pp.display().to_string(),
                loaded: true,
                valid: vike_core::RunProfile::from_path(pp).is_ok(),
                sha_fnv1a64: Some(sha),
                archive_name: wrote.then(|| archive.to_string()),
                error: None,
            }
        }
        Err(e) => ProfileInfo {
            path: pp.display().to_string(),
            loaded: false,
            valid: false,
            sha_fnv1a64: None,
            archive_name: None,
            error: Some(e.to_string()),
        },
    }
}

/// Gzip in-memory `bytes` into `dst`; return the compressed byte count.
fn gzip_bytes(bytes: &[u8], dst: &Path) -> io::Result<u64> {
    let out = std::fs::File::create(dst)?;
    let mut enc = GzEncoder::new(out, Compression::default());
    enc.write_all(bytes)?;
    enc.finish()?;
    std::fs::metadata(dst).map(|m| m.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── window-overlap selection ─────────────────────────────────────────────────────────────

    #[test]
    fn select_overlapping_keeps_at_or_after_cutoff() {
        let items = [("a", 100_i64), ("b", 200), ("c", 300)];
        // now=300, since=100 → cutoff=200 → keep b (boundary, inclusive) and c; drop a.
        assert_eq!(select_overlapping(&items, 300, 100), vec!["b", "c"]);
        // since=0 → cutoff=now=300 → only the file written exactly at now.
        assert_eq!(select_overlapping(&items, 300, 0), vec!["c"]);
        // a huge window (saturating) → everything.
        assert_eq!(select_overlapping(&items, 300, i64::MAX), vec!["a", "b", "c"]);
    }

    #[test]
    fn select_overlapping_excludes_everything_before_the_window() {
        // every file's last write predates the cutoff → nothing overlaps (the degenerate/off case).
        let items = [("old1", 10_i64), ("old2", 50)];
        assert!(select_overlapping(&items, 1_000, 100).is_empty()); // cutoff=900 > 50
                                                                    // and an empty input is empty out.
        let empty: Vec<((), i64)> = Vec::new();
        assert!(select_overlapping(&empty, 1_000, 100).is_empty());
    }

    // ── redaction ────────────────────────────────────────────────────────────────────────────

    #[test]
    fn is_secret_key_matches_every_credential_shape_but_not_vike_flags() {
        for k in [
            "BINANCE_LIVE_API_KEY",
            "BINANCE_LIVE_API_SECRET",
            "OKX_DEMO_API_PASSPHRASE",
            "POLY_MAINNET_PRIVATE_KEY",
            "POLY_LIVE_PK",
            "GITHUB_TOKEN",
            "some_wallet_mnemonic",
        ] {
            assert!(is_secret_key(k), "{k} should be treated as secret");
        }
        for k in
            ["VIKE_RECONCILE", "VIKE_JOURNAL_DIR", "VIKE_LOG_DIR", "RUST_LOG", "VIKE_PIN_CORES"]
        {
            assert!(!is_secret_key(k), "{k} is a runtime flag, not a secret");
        }
    }

    #[test]
    fn redact_secret_value_masks_all_but_the_last_four() {
        assert_eq!(redact_secret_value("supersecretvalue123"), "***e123");
        assert_eq!(redact_secret_value("abcd"), "***abcd"); // exactly four → all four are the tail
        assert_eq!(redact_secret_value("xy"), "***"); // too short → bare mask, no tail
        assert_eq!(redact_secret_value(""), "***");
    }

    #[test]
    fn redact_env_sorts_and_masks_secret_values_only() {
        let raw = [
            ("VIKE_RECONCILE".to_string(), "1".to_string()),
            ("BINANCE_LIVE_API_SECRET".to_string(), "supersecretvalue123".to_string()),
            ("ABC".to_string(), "plain".to_string()),
        ];
        let out = redact_env(&raw);
        // sorted by key
        assert_eq!(
            out.iter().map(|(k, _)| k.as_str()).collect::<Vec<_>>(),
            ["ABC", "BINANCE_LIVE_API_SECRET", "VIKE_RECONCILE"]
        );
        // the full secret never appears; only the masked fingerprint does; flags pass verbatim.
        let flat = format!("{out:?}");
        assert!(!flat.contains("supersecretvalue123"), "full secret leaked: {flat}");
        assert!(flat.contains("***e123"), "masked fingerprint missing: {flat}");
        assert!(flat.contains("\"1\""), "the plain flag value must pass through: {flat}");
    }

    // ── stamps + hash ────────────────────────────────────────────────────────────────────────

    #[test]
    fn utc_stamps_match_known_instants() {
        assert_eq!(utc_stamp_compact(0), "19700101T000000Z");
        assert_eq!(utc_stamp_rfc3339(0), "1970-01-01T00:00:00.000Z");
        // 2021-01-01T00:00:00Z + 1h1m1s
        assert_eq!(utc_stamp_compact(1_609_459_200_000), "20210101T000000Z");
        assert_eq!(utc_stamp_compact(1_609_459_200_000 + 3_661_000), "20210101T010101Z");
        assert_eq!(utc_stamp_rfc3339(1_609_459_200_000 + 3_661_123), "2021-01-01T01:01:01.123Z");
    }

    #[test]
    fn fnv1a64_hex_is_stable_and_distinguishing() {
        // empty input → the FNV-1a64 offset basis, 16 hex digits.
        assert_eq!(fnv1a64_hex(b""), "cbf29ce484222325");
        assert_eq!(fnv1a64_hex(b"profile-a"), fnv1a64_hex(b"profile-a")); // deterministic
        assert_ne!(fnv1a64_hex(b"profile-a"), fnv1a64_hex(b"profile-b")); // differs on change
        assert_eq!(fnv1a64_hex(b"x").len(), 16); // always 16 hex digits
    }

    // ── manifest assembly ────────────────────────────────────────────────────────────────────

    fn build_info() -> GitBuildInfo {
        GitBuildInfo {
            pkg_name: "vike-run".to_string(),
            pkg_version: "0.1.0".to_string(),
            build_profile: "release".to_string(),
            git_sha: Some("deadbeef".to_string()),
            git_dirty: Some(false),
        }
    }

    #[test]
    fn build_manifest_assembles_window_and_redacts_env_by_construction() {
        let env = [
            ("VIKE_RECONCILE".to_string(), "1".to_string()),
            ("BINANCE_LIVE_API_SECRET".to_string(), "supersecretvalue123".to_string()),
        ];
        let m = build_manifest(
            1_609_459_200_000,
            3_600_000,
            build_info(),
            None,
            None,
            None,
            EngineState::unreachable(None),
            &env, // RAW env with a secret — build_manifest must redact it
        );
        assert_eq!(m.kind, "vike-incident-bundle");
        assert_eq!(m.schema, 1);
        assert_eq!(m.window.now_ms, 1_609_459_200_000);
        assert_eq!(m.window.cutoff_ms, 1_609_459_200_000 - 3_600_000);
        assert_eq!(m.window.cutoff_utc, "2020-12-31T23:00:00.000Z");
        assert!(!m.engine_state.reachable);

        // Serialized artifact must never carry the full secret, must carry the mask + the flag.
        let json = serde_json::to_string(&m).expect("manifest serializes");
        assert!(!json.contains("supersecretvalue123"), "full secret reached the manifest: {json}");
        assert!(json.contains("***e123"), "masked fingerprint missing: {json}");
        assert!(json.contains("VIKE_RECONCILE"), "plain flag missing: {json}");

        // env sorted: the credential key precedes the VIKE flag.
        let ib = m.env.iter().position(|(k, _)| k == "BINANCE_LIVE_API_SECRET").unwrap();
        let iv = m.env.iter().position(|(k, _)| k == "VIKE_RECONCILE").unwrap();
        assert!(ib < iv, "env must be sorted by key");

        // round-trips back through serde (the artifact is machine-readable).
        let back: IncidentManifest = serde_json::from_str(&json).expect("manifest round-trips");
        assert_eq!(back.window.cutoff_ms, m.window.cutoff_ms);
    }

    // ── end-to-end orchestration (temp dir, injected clock + env; no real secret on disk) ─────

    fn temp_root(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("vike_incident_{}_{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn run_incident_at_freezes_the_window_and_never_writes_a_secret() {
        let root = temp_root("wide");
        let jdir = root.join("journal");
        let ldir = root.join("logs");
        std::fs::create_dir_all(&jdir).unwrap();
        std::fs::create_dir_all(&ldir).unwrap();
        // A fake segment (no journal MAGIC → engine_state unreachable, but still frozen) and a log.
        std::fs::write(jdir.join("journal-00000001.vjl"), b"not-a-real-journal-segment").unwrap();
        std::fs::write(ldir.join("vike.2026-07-24"), b"log line, no secrets here\n").unwrap();

        let cfg = IncidentConfig {
            since: Duration::from_secs(10 * 365 * 86_400), // wide → include everything
            out_root: root.join("out"),
            journal_dir: Some(jdir.clone()),
            log_dir: Some(ldir.clone()),
            profile_path: None,
        };
        // Injected env carries a secret; injected clock is ahead of the just-written files.
        let env = vec![
            ("VIKE_JOURNAL_DIR".to_string(), jdir.display().to_string()),
            ("BINANCE_LIVE_API_SECRET".to_string(), "supersecretvalue123".to_string()),
        ];
        let now = vike_model::now_ms() + 60_000;
        let bundle = run_incident_at(&cfg, now, env).expect("bundle written");

        // manifest exists, parses, carries NO secret.
        let text = std::fs::read_to_string(bundle.join("manifest.json")).expect("manifest present");
        assert!(!text.contains("supersecretvalue123"), "the on-disk bundle leaked a secret!");
        let m: IncidentManifest = serde_json::from_str(&text).expect("manifest parses");

        // both evidence files frozen as gz.
        assert!(bundle.join("journal").join("journal-00000001.vjl.gz").is_file());
        assert!(bundle.join("logs").join("vike.2026-07-24.gz").is_file());
        assert_eq!(m.journal.as_ref().unwrap().segments_selected, 1);
        assert_eq!(m.logs.as_ref().unwrap().files_selected, 1);
        // a non-journal file yields an unreachable (but valid) engine-state section.
        assert!(!m.engine_state.reachable);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn run_incident_at_is_inert_when_the_window_selects_nothing() {
        // The OFF/degenerate path: a zero-length window selects no evidence, so the collector
        // freezes nothing and requires nothing from the live system — yet still emits a valid,
        // secret-free manifest. Nothing about the trading node is read for state or altered.
        let root = temp_root("empty");
        let jdir = root.join("journal");
        let ldir = root.join("logs");
        std::fs::create_dir_all(&jdir).unwrap();
        std::fs::create_dir_all(&ldir).unwrap();
        std::fs::write(jdir.join("journal-00000001.vjl"), b"not-a-real-journal-segment").unwrap();
        std::fs::write(ldir.join("vike.2026-07-24"), b"older log line\n").unwrap();

        let cfg = IncidentConfig {
            since: Duration::from_millis(0), // cutoff == now → the just-written files are excluded
            out_root: root.join("out"),
            journal_dir: Some(jdir.clone()),
            log_dir: Some(ldir.clone()),
            profile_path: None,
        };
        let now = vike_model::now_ms() + 60_000; // strictly after the files' mtimes
        let bundle =
            run_incident_at(&cfg, now, vec![("VIKE_RECONCILE".to_string(), "1".to_string())])
                .expect("bundle written");

        let text = std::fs::read_to_string(bundle.join("manifest.json")).expect("manifest present");
        let m: IncidentManifest = serde_json::from_str(&text).expect("manifest parses");
        assert_eq!(m.journal.as_ref().unwrap().segments_selected, 0, "nothing in the window");
        assert_eq!(m.logs.as_ref().unwrap().files_selected, 0, "nothing in the window");
        // segments were still ENUMERATED (total), just none selected — the tool is inert, not blind.
        assert_eq!(m.journal.as_ref().unwrap().segments_total, 1);
        // no gz files written for an empty selection.
        assert!(!bundle.join("journal").join("journal-00000001.vjl.gz").exists());

        let _ = std::fs::remove_dir_all(&root);
    }
}
