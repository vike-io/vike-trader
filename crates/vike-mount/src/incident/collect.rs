//! The incident orchestration: collect the window's files and state into one bundle on disk.

use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use flate2::Compression;
use flate2::write::GzEncoder;

use vike_exec::state_hash;
use vike_journal::{CommandJournal, JournalRecord};

use super::{
    CollectedFile, EngineState, EngineSummary, GitBuildInfo, JournalCollection, LogCollection,
    ProfileInfo, RunProfileRow, build_manifest, fnv1a64_hex, select_overlapping, utc_stamp_compact,
    utc_stamp_rfc3339,
};

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
    /// the settings database's ACTIVE `run` row: `None` = no store or no active row (profile
    /// capture skipped), `Some(Err(why))` = the store exists and could not be read (recorded in the
    /// manifest, never silently skipped).
    pub run_profile: Option<Result<RunProfileRow, String>>,
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
    let profile = cfg.run_profile.as_ref().map(|src| capture_profile(src, &bundle));
    let parsed_profile = cfg
        .run_profile
        .as_ref()
        .and_then(|src| src.as_ref().ok())
        .and_then(|row| vike_core::RunProfile::from_toml_str(&row.toml).ok());

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

/// Freeze the active `run` row's rendered body: hash ([`fnv1a64_hex`]), gzip a copy into `bundle`,
/// note whether it validates as a [`vike_core::RunProfile`]. An unreadable store is
/// `loaded: false`, with the reason.
fn capture_profile(src: &Result<RunProfileRow, String>, bundle: &Path) -> ProfileInfo {
    match src {
        Ok(row) => {
            let sha = fnv1a64_hex(row.toml.as_bytes());
            let archive = "profile.toml.gz";
            let wrote = gzip_bytes(row.toml.as_bytes(), &bundle.join(archive)).is_ok();
            let parsed = vike_core::RunProfile::from_toml_str(&row.toml);
            ProfileInfo {
                row: row.name.clone(),
                loaded: true,
                valid: parsed.is_ok(),
                sha_fnv1a64: Some(sha),
                archive_name: wrote.then(|| archive.to_string()),
                error: parsed.err().map(|e| e.to_string()),
            }
        }
        Err(e) => ProfileInfo {
            row: String::new(),
            loaded: false,
            valid: false,
            sha_fnv1a64: None,
            archive_name: None,
            error: Some(e.clone()),
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
