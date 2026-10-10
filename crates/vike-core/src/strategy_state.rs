//! Strategy-state persistence sidecar (portfolio-observer PR-4, task 2): a deterministic mount
//! identity plus an atomic JSON sidecar write/read pair. The runtime (`runtime::assemble_core`)
//! uses [`mount_id_of`]/[`sidecar_path`]/[`read_json`] to LOAD a strategy's durable state
//! (`Strategy::load_state`) when it is mounted, and `runtime::CoreThread::run`'s teardown uses
//! [`write_json_atomic`] to SAVE it (`Strategy::save_state`) on a clean shutdown — both gated
//! behind the opt-in `CoreConfig::state_dir` (`None` by default: nothing in this module is ever
//! called).
//!
//! [`write_json_atomic`] mirrors `crates/vike-data/src/store/datafusion_hist/manifest.rs`'s
//! `write_manifest` fsync+rename+fallback shape exactly: write the bytes to a `.tmp` sibling,
//! `sync_all` to make them durable, then atomically `rename` over the final path, with a
//! remove-then-retry fallback for platforms where `rename` refuses to replace an existing file.
//! That function is `pub(super)` to vike-data, so this module replicates the shape rather than
//! depending on it.

use std::io::Write;
use std::path::{Path, PathBuf};

/// Deterministic mount identity for a (venue, symbol, interval) strategy mount — doubles as the
/// sidecar's filename stem. Each part is sanitized independently (every non-alphanumeric char
/// replaced with `_`), then the three sanitized parts are joined with `__`, e.g.
/// `mount_id_of("binance", "BTC/USDT", "1m") == "binance__BTC_USDT__1m"`. Pure and total: any
/// input produces a valid single-path-segment filename stem, and identical inputs always produce
/// the identical id (so a restart derives the same sidecar path a prior run wrote).
pub fn mount_id_of(venue: &str, symbol: &str, interval: &str) -> String {
    format!("{}__{}__{}", sanitize(venue), sanitize(symbol), sanitize(interval))
}

/// The mount-identity law once an explicit CONTROLLER ID may name a mount (multi-mount
/// correctness, gap C): a `controller_id` — when the mount carries one
/// ([`crate::StrategyMount::controller_id`]) — IS the mount id, sanitized exactly like the derived
/// parts so it stays a safe single filename segment; otherwise the legacy [`mount_id_of`]
/// derivation applies VERBATIM.
///
/// The fallback is what makes this additive: a mount with no controller id (every mount that
/// exists today) derives the identical `{venue}__{symbol}__{interval}` id it always did, so its
/// state sidecar, its journal `mount_id` provenance and its budget/schedule keys are all
/// byte-identical. An explicit id exists for the ONE case the derivation cannot express — two
/// strategies mounted on the SAME `(venue, symbol, interval)` triple, which would otherwise share
/// one sidecar file and one identity in the journal.
///
/// `None`, `Some("")` and a whitespace-only id all mean ABSENT (fall back to the derivation): an
/// empty id would sanitize to an empty filename stem, which is not a usable identity.
pub fn mount_id_with(
    controller_id: Option<&str>,
    venue: &str,
    symbol: &str,
    interval: &str,
) -> String {
    match controller_id.map(str::trim).filter(|c| !c.is_empty()) {
        Some(cid) => sanitize(cid),
        None => mount_id_of(venue, symbol, interval),
    }
}

/// Replace every non-alphanumeric char with `_` — keeps the result a safe single filename
/// segment regardless of what the venue/symbol/interval strings contain (e.g. `BTC/USDT`'s `/`,
/// which would otherwise be read as a path separator).
fn sanitize(part: &str) -> String {
    part.chars().map(|c| if c.is_alphanumeric() { c } else { '_' }).collect()
}

/// The sidecar file path for `mount_id` under `state_dir`: `<state_dir>/<mount_id>.json`.
pub fn sidecar_path(state_dir: &Path, mount_id: &str) -> PathBuf {
    state_dir.join(format!("{mount_id}.json"))
}

/// Durably write `value` to `path`: serialize to a `<path>.tmp` sibling, `fsync` it, then
/// atomically `rename` over `path` (remove-then-retry fallback), creating `path`'s parent
/// directory first if it does not exist yet. See the module doc for the vike-data function this
/// mirrors.
pub fn write_json_atomic(path: &Path, value: &serde_json::Value) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let bytes = serde_json::to_vec_pretty(value).map_err(std::io::Error::other)?;
    let mut tmp_name = path.as_os_str().to_owned();
    tmp_name.push(".tmp");
    let tmp = PathBuf::from(tmp_name);
    // fsync the new sidecar's bytes BEFORE the rename so the published file is durable — the same
    // ordering rationale as write_manifest: a crash between write and fsync must never leave a
    // torn/truncated file behind the eventual rename.
    {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(&bytes)?;
        f.sync_all()?;
    }
    if std::fs::rename(&tmp, path).is_err() {
        // Windows `rename` fails onto an existing file: clear it and retry (that error propagates).
        let _ = std::fs::remove_file(path);
        std::fs::rename(&tmp, path)?;
    }
    Ok(())
}

/// Read + parse the sidecar at `path`. Fail-open: a missing file or a parse error both return
/// `None` (never propagated as an `Err`) — an absent or corrupt sidecar simply means "no saved
/// state to restore", never a load-time crash.
pub fn read_json(path: &Path) -> Option<serde_json::Value> {
    let bytes = std::fs::read(path).ok()?;
    serde_json::from_slice(&bytes).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scratch::Scratch;

    /// A fresh state directory for one test, removed when the returned guard drops — on a failing
    /// run too, which the hand-rolled `env::temp_dir()` path this replaced never did. `reserved`,
    /// so the directory does not exist yet and `write_json_atomic` is what creates it. Hold the
    /// guard for the whole test: a guard that is only a temporary drops at the end of its
    /// statement, deleting the root the code under test then re-creates and leaks.
    fn unique_temp_dir(tag: &str) -> Scratch {
        Scratch::reserved(&format!("vike-core-strategy-state-{tag}"))
    }

    #[test]
    fn mount_id_is_deterministic_and_sanitized() {
        assert_eq!(mount_id_of("binance", "BTC/USDT", "1m"), "binance__BTC_USDT__1m");
        assert_eq!(
            mount_id_of("binance", "BTC/USDT", "1m"),
            mount_id_of("binance", "BTC/USDT", "1m")
        );
    }

    /// The controller-id law: an explicit id REPLACES the derivation (sanitized the same way), and
    /// every absent-ish spelling (`None`, empty, whitespace) falls back to the legacy id — the
    /// byte-identical guarantee every existing mount rides.
    #[test]
    fn controller_id_replaces_the_derivation_and_absent_falls_back() {
        assert_eq!(mount_id_with(Some("maker-a"), "sim", "BTC/USDT", "1m"), "maker_a");
        assert_eq!(
            mount_id_with(None, "sim", "BTC/USDT", "1m"),
            mount_id_of("sim", "BTC/USDT", "1m")
        );
        assert_eq!(
            mount_id_with(Some(""), "sim", "BTC/USDT", "1m"),
            mount_id_of("sim", "BTC/USDT", "1m")
        );
        assert_eq!(
            mount_id_with(Some("   "), "sim", "BTC/USDT", "1m"),
            mount_id_of("sim", "BTC/USDT", "1m")
        );
    }

    #[test]
    fn write_then_read_json_roundtrips_atomically() {
        let dir = unique_temp_dir("roundtrip");
        let p = sidecar_path(&dir, "m1");
        write_json_atomic(&p, &serde_json::json!({"k": 1})).unwrap();
        assert_eq!(read_json(&p), Some(serde_json::json!({"k": 1})));
        assert_eq!(read_json(&sidecar_path(&dir, "absent")), None);
        // no tmp sibling left behind after a successful publish
        let mut tmp_name = p.as_os_str().to_owned();
        tmp_name.push(".tmp");
        assert!(!PathBuf::from(tmp_name).exists(), "tmp sidecar renamed away, not left behind");
    }

    #[test]
    fn write_json_atomic_creates_missing_parent_dir() {
        // ⚠ Bound, not chained: `unique_temp_dir("mkdir").join(..)` would drop the guard at the
        // end of the statement, and `write_json_atomic` would then re-create and leak the root.
        let scratch = unique_temp_dir("mkdir");
        let dir = scratch.join("nested").join("state");
        assert!(!dir.exists());
        let p = sidecar_path(&dir, "m2");
        write_json_atomic(&p, &serde_json::json!({"ok": true})).unwrap();
        assert_eq!(read_json(&p), Some(serde_json::json!({"ok": true})));
    }

    #[test]
    fn write_json_atomic_overwrites_existing_sidecar() {
        let dir = unique_temp_dir("overwrite");
        let p = sidecar_path(&dir, "m3");
        write_json_atomic(&p, &serde_json::json!({"n": 1})).unwrap();
        write_json_atomic(&p, &serde_json::json!({"n": 2})).unwrap();
        assert_eq!(read_json(&p), Some(serde_json::json!({"n": 2})));
    }
}
