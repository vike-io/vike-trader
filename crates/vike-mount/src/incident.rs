//! `incident` — freeze the perishable evidence of a live run into ONE timestamped bundle.
//!
//! The forensic trail is perishable: the command journal ([`vike_journal`], segments PRUNE), the
//! trace log ([`vike_log`], ROLLS, may be size-capped), the [`vike_core::RunProfile`] (editable
//! between runs) and the process environment (gone at exit). Given a window (`--since <dur>`),
//! [`run_incident`](crate::incident::run_incident) copies into one timestamped directory:
//!   - journal segments and log-dir trace-log files whose last-write overlaps the window
//!     (gzipped, per file),
//!   - the resolved `RunProfile` TOML + its FNV-1a64 hash (a later edit is detectable),
//!   - the latest [`vike_exec::EngineSnapshot`] + `state_hash` from the journal's last `Snap`,
//!   - git SHA / build info, and
//!   - a REDACTED environment dump — secret-shaped values masked EXACTLY as the signers' manual
//!     `Debug` impls do (`crates/vike-bridge-core/src/signer.rs`: `***` + the last four chars), so
//!     a shared bundle never carries an API secret / private key.
//!
//! ## Read-only, additive
//! Nothing in the mount path calls it. It opens the journal only through the lock-free
//! [`vike_journal::CommandJournal::read_all`] (the "offline inspection tooling" entry point),
//! never the writer, so it is safe against a live node.
//!
//! ## Archive shape (per-file gz in a directory, NOT a single `tar.gz`)
//! Each file is gzipped individually ([`flate2`] `GzEncoder`) into `incident-<UTC>/…` beside a
//! plain `manifest.json`: a `.tar.gz` would need a `tar` crate this lane cannot compile-check.
//!
//! ## Purity for testability
//! Pure and unit-tested: [`select_overlapping`](crate::incident::select_overlapping) (window→file),
//! [`redact_env`](crate::incident::redact_env) /
//! [`redact_secret_value`](crate::incident::redact_secret_value) (masking), and
//! [`build_manifest`](crate::incident::build_manifest) (redacts internally: safe by construction).
//! [`run_incident_at`](crate::incident::run_incident_at) is a thin `std::fs` + `flate2` shell.

use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use flate2::Compression;
use flate2::write::GzEncoder;
use serde::{Deserialize, Serialize};

use vike_exec::state_hash;
use vike_journal::{CommandJournal, JournalRecord};
use vike_model::time::civil_from_days;

// ---- pure helpers (unit-tested) ----

/// Window-overlap selection: of `(label, mtime epoch-ms)` pairs, the labels whose mtime is AT OR
/// AFTER the cutoff `now_ms - since_ms` (inclusive; saturating, so a huge window cannot overflow).
///
/// Sound because the stores are append-only and time-ordered: a file whose LAST write precedes the
/// cutoff holds no record in the window, and one straddling it is kept (extra context is fine).
/// Generic: selects journal segments AND trace-log files.
pub fn select_overlapping<T: Clone>(items: &[(T, i64)], now_ms: i64, since_ms: i64) -> Vec<T> {
    let cutoff = now_ms.saturating_sub(since_ms);
    items.iter().filter(|(_, mtime)| *mtime >= cutoff).map(|(t, _)| t.clone()).collect()
}

/// Does this env-var KEY name a secret whose value must be masked? Generous case-insensitive
/// substring match: over-redaction only hides a non-secret flag, a miss leaks a credential. Covers
/// `*_API_KEY` / `*_API_SECRET` / `*_API_PASSPHRASE` (`vike-bridge-core/src/credentials.rs`),
/// `*_PRIVATE_KEY` / `POLY_*_PK` (`bridges/polymarket/src/config.rs`), tokens, mnemonics, seeds,
/// signatures. `VIKE_*` runtime flags match no needle and pass verbatim.
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
/// (`crates/vike-bridge-core/src/signer.rs`): `***` plus the last four characters as a fingerprint;
/// under four chars, a bare `***`. Char-boundary-safe: never panics on a multibyte value (the
/// signers' byte-slice only ever sees ASCII keys).
pub fn redact_secret_value(val: &str) -> String {
    let n = val.chars().count();
    let tail: String = if n >= 4 { val.chars().skip(n - 4).collect() } else { String::new() };
    format!("***{tail}")
}

/// Sort an environment dump by key and mask every secret-shaped value ([`is_secret_key`] →
/// [`redact_secret_value`]). Pure over a slice, so tests never touch the process env.
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

/// FNV-1a **64-bit** of `bytes` as 16 lowercase hex digits (constants as [`vike_exec::state_hash`],
/// inline: no hashing dependency). Pins the `RunProfile` TOML so a later edit is detectable.
pub fn fnv1a64_hex(bytes: &[u8]) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{h:016x}")
}

/// Epoch-ms → UTC `(year, month, day, hour, minute, second, milli)`, chrono-free
/// ([`vike_model::time::civil_from_days`]).
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

/// Epoch-ms → `YYYYMMDDThhmmssZ`, the filesystem-safe bundle directory stamp.
pub fn utc_stamp_compact(ms: i64) -> String {
    let (y, mo, d, h, mi, s, _) = utc_parts(ms);
    format!("{y:04}{mo:02}{d:02}T{h:02}{mi:02}{s:02}Z")
}

/// Epoch-ms → `YYYY-MM-DDThh:mm:ss.sssZ`, for the manifest's human fields.
pub fn utc_stamp_rfc3339(ms: i64) -> String {
    let (y, mo, d, h, mi, s, millis) = utc_parts(ms);
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{s:02}.{millis:03}Z")
}

// ---- manifest DTOs (Deserialize so tests round-trip the artifact) ----

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
    /// `"debug"` / `"release"` (`cfg!(debug_assertions)`).
    pub build_profile: String,
    /// `git rev-parse HEAD`; `None` outside a repo.
    pub git_sha: Option<String>,
    /// `git status --porcelain` non-empty; `None` when git was unavailable.
    pub git_dirty: Option<bool>,
}

/// The resolved `RunProfile` TOML the run loaded (from `--profile` / `VIKE_RUN_PROFILE`), pinned.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProfileInfo {
    pub path: String,
    /// the file was read.
    pub loaded: bool,
    /// it parsed+validated as a [`vike_core::RunProfile`].
    pub valid: bool,
    /// [`fnv1a64_hex`] of the raw bytes; `None` if unreadable.
    pub sha_fnv1a64: Option<String>,
    /// the gzipped copy's bundle name; `None` if the copy failed.
    pub archive_name: Option<String>,
    /// read/parse error, if any.
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

/// One engine's triage counts from the latest `Snap` (full [`vike_exec::EngineSnapshot`]s ride in
/// `engine_snapshot.json`).
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
    /// a `Snap` was found (`false` = no journal / no snapshot yet; still valid).
    pub reachable: bool,
    pub journal_dir: Option<String>,
    /// the `hash` (its `state_hash`) the runtime stamped on the `Snap`.
    pub state_hash_stored: Option<u64>,
    pub state_hash_hex: Option<String>,
    /// [`vike_exec::state_hash`] recomputed over the restored engines; a divergence from the
    /// stored value is itself diagnostic.
    pub state_hash_recomputed: Option<u64>,
    pub engine_count: usize,
    pub engines: Vec<EngineSummary>,
    /// the full-snapshot JSON's bundle name; `None` if unreachable / write failed.
    pub snapshot_archive: Option<String>,
}

impl EngineState {
    /// No journal dir, or no `Snap` in it: still a valid manifest section.
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

/// The bundle's plain-JSON index of everything frozen.
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

/// Assemble the manifest from collected pieces. Pure; REDACTS the environment itself
/// ([`redact_env`]), so it is secret-safe even if a caller forgets.
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

// ---- orchestration (impure: std::fs + flate2 + the read-only journal reader) ----

/// What to collect and where to write it.
#[derive(Debug, Clone)]
pub struct IncidentConfig {
    /// the look-back window.
    pub since: Duration,
    /// parent of the `incident-<UTC>/` bundle.
    pub out_root: PathBuf,
    /// `None` = the loaded `RunProfile`'s journal sink.
    pub journal_dir: Option<PathBuf>,
    /// resolved trace-log directory (the bin: `vike_log::resolve_log_dir`); `None` skips logs.
    pub log_dir: Option<PathBuf>,
    /// resolved `RunProfile` path (`--profile` / `VIKE_RUN_PROFILE`); `None` skips profile capture.
    pub profile_path: Option<PathBuf>,
}

/// [`run_incident_at`] with the wall clock and the process environment; returns the bundle dir.
pub fn run_incident(cfg: &IncidentConfig) -> io::Result<PathBuf> {
    run_incident_at(cfg, vike_model::now_ms(), std::env::vars().collect())
}

/// [`run_incident`] with clock and environment injected, so tests drive the whole pipeline
/// deterministically and never write a real secret to disk.
pub fn run_incident_at(
    cfg: &IncidentConfig,
    now_ms: i64,
    env_vars: Vec<(String, String)>,
) -> io::Result<PathBuf> {
    let since_ms = cfg.since.as_millis().min(i64::MAX as u128) as i64;
    let bundle = cfg.out_root.join(format!("incident-{}", utc_stamp_compact(now_ms)));
    std::fs::create_dir_all(&bundle)?;

    // Profile captured + hashed; a second best-effort parse ONLY derives the journal-dir default.
    let profile = cfg.profile_path.as_deref().map(|pp| capture_profile(pp, &bundle));
    let parsed_profile =
        cfg.profile_path.as_deref().and_then(|pp| vike_core::RunProfile::from_path(pp).ok());

    let journal_dir = cfg.journal_dir.clone().or_else(|| {
        parsed_profile
            .as_ref()
            .and_then(|p| p.sinks.journal.as_ref())
            .map(|j| PathBuf::from(&j.dir))
    });

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

    let build = git_build_info();
    let manifest =
        build_manifest(now_ms, since_ms, build, profile, journal, logs, engine_state, &env_vars);
    let json = serde_json::to_vec_pretty(&manifest).expect("IncidentManifest serializes");
    std::fs::write(bundle.join("manifest.json"), json)?;
    Ok(bundle)
}

#[derive(Clone)]
struct FileMeta {
    path: PathBuf,
    size: u64,
    mtime_ms: i64,
}

/// mtime as epoch-ms; 0 when pre-epoch or unavailable.
fn mtime_ms(meta: &std::fs::Metadata) -> i64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Gzip `dir`'s `keep`-matching, window-overlapping files ([`select_overlapping`]) into `dest_dir`;
/// returns `(total_matched, collected)`. Best-effort: a failure is logged and skipped, never
/// propagated, so one unreadable file cannot abort the bundle.
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
    if !selected.is_empty()
        && let Err(e) = std::fs::create_dir_all(dest_dir)
    {
        tracing::warn!(dir = %dest_dir.display(), %e, "incident: cannot create bundle subdir");
        return (total, Vec::new());
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

/// The journal's LATEST `Snap` (lock-free reader) → engines + `state_hash`; full engines written to
/// `engine_snapshot.json`. A read failure or no snapshot is `unreachable`, not an error (normal
/// before the first snapshot).
fn latest_engine_state(dir: &Path, bundle: &Path) -> EngineState {
    let records = match CommandJournal::read_all(dir) {
        Ok(r) => r,
        Err(_) => return EngineState::unreachable(Some(dir)),
    };
    // read_all is in seq order: the last `Snap` is the newest.
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
    // Trading state only, no credentials: written plain for offline inspection.
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

/// Best-effort provenance; git runs in the CWD, so a deployed binary yields `None`.
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

/// Gzip `src` into `dst` ([`GzEncoder`]); returns the compressed size. Pre-allocated segments are
/// mostly zeros, so a 64 MiB segment freezes to a tiny file.
fn gzip_file(src: &Path, dst: &Path) -> io::Result<u64> {
    let mut input = std::fs::File::open(src)?;
    let out = std::fs::File::create(dst)?;
    let mut enc = GzEncoder::new(out, Compression::default());
    io::copy(&mut input, &mut enc)?;
    enc.finish()?;
    std::fs::metadata(dst).map(|m| m.len())
}

/// Freeze the `RunProfile` TOML at `pp`: hash ([`fnv1a64_hex`]), gzip a copy into `bundle`, note
/// whether it validates as a [`vike_core::RunProfile`]. A read failure is `loaded: false`.
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

/// Gzip `bytes` into `dst`; returns the compressed size.
fn gzip_bytes(bytes: &[u8], dst: &Path) -> io::Result<u64> {
    let out = std::fs::File::create(dst)?;
    let mut enc = GzEncoder::new(out, Compression::default());
    enc.write_all(bytes)?;
    enc.finish()?;
    std::fs::metadata(dst).map(|m| m.len())
}

#[path = "incident_tests.rs"]
#[cfg(test)]
mod incident_tests;
