//! Strategy-state persistence sidecar (portfolio-observer PR-4, task 2): a deterministic mount
//! identity plus an atomic sidecar write/read pair. The runtime (`runtime::assemble_core`) uses
//! [`mount_id_with`]/[`read_state`] to LOAD a strategy's durable state (`Strategy::load_state`) when it
//! is mounted, and the state timer and `runtime::CoreThread::run`'s teardown use [`state_path`] and
//! [`write_state_atomic`] to SAVE it (`Strategy::save_state`) — all gated behind the opt-in
//! `CoreConfig::state_dir` (`None` by default: nothing in this module is ever called). Since decision
//! 0296's ABI 15 the state sidecar is msgpack with field names (`<mount_id>.msgpack`); an OLD JSON one
//! (`<mount_id>.json`, [`sidecar_path`]) is still read, never written. [`write_json_atomic`] and
//! [`read_json`] stay for the JSON sidecars of the mount topology.
//!
//! [`write_json_atomic`] serializes and creates the parent directory; the write itself is
//! `vike_utility::atomic_file`'s `write_atomic`, the workspace's one atomic-replace policy
//! (temp file beside the target, fsync, rename, directory fsync; the target is never removed first).
//!
//! # A sidecar that exists and cannot be read
//!
//! Only "not found" is no saved state (`vike_utility::atomic_file::read_if_present`). A sidecar that
//! exists and cannot be read, or holds no state ([`vike_model::StrategyState::check`]), is an ERROR
//! naming it, and it is SET ASIDE beside itself (`vike_utility::unreadable_file::set_aside`): the
//! strategy starts with NO saved state and the line says so, and its next save starts a new file
//! instead of replacing the only copy of what it knew. One that cannot be set aside is held against
//! every write until the process restarts (`UNREAD_THIS_RUN`): its saves are refused, which the
//! save sites already log.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// **The state files this PROCESS could not read and must not write again before it restarts**:
/// [`write_json_atomic`] and [`write_state_atomic`] refuse each. A strategy-state sidecar enters when
/// it could not be set aside (it still holds the bytes nobody could read), the topology file after
/// any failed read (`crate::mount_topology`'s module doc says why). One process runs one core, so
/// this is "the run".
static UNREAD_THIS_RUN: Mutex<BTreeSet<PathBuf>> = Mutex::new(BTreeSet::new());

/// Hold `path` against every write until the process restarts ([`UNREAD_THIS_RUN`]).
pub(crate) fn hold_unread(path: &Path) {
    vike_utility::mutex::lock(&UNREAD_THIS_RUN).insert(path.to_path_buf());
}

/// `Err` naming `path` when this run could not read it ([`hold_unread`]): nothing writes it.
pub(crate) fn refuse_unread(path: &Path) -> std::io::Result<()> {
    if vike_utility::mutex::lock(&UNREAD_THIS_RUN).contains(path) {
        return Err(std::io::Error::other(format!(
            "{} is not written: it could not be read in this run, and a write would replace what it \
             holds (restart once it reads, or move it away)",
            path.display()
        )));
    }
    Ok(())
}

/// Deterministic mount identity for a (venue, symbol, interval) strategy mount — doubles as the
/// sidecar's filename stem. Each part is sanitized independently (every non-alphanumeric char
/// replaced with `_`), then the three sanitized parts are joined with `__`, e.g.
/// `mount_id_of("binance", "BTC/USDT", "1m") == "binance__BTC_USDT__1m"`. Pure and total: any
/// input produces a valid single-path-segment filename stem, and identical inputs always produce
/// the identical id (so a restart derives the same sidecar path a prior run wrote).
///
/// Private: every caller, tests included, names [`mount_id_with`] (`None` for no controller id),
/// so a key built without the controller id cannot silently miss a mount that carries one.
fn mount_id_of(venue: &str, symbol: &str, interval: &str) -> String {
    format!("{}__{}__{}", sanitize(venue), sanitize(symbol), sanitize(interval))
}

/// The mount-identity law once an explicit CONTROLLER ID may name a mount (multi-mount
/// correctness, gap C): a `controller_id` — when the mount carries one
/// ([`crate::Mount::controller_id`]) — IS the mount id, sanitized exactly like the derived
/// parts so it stays a safe single filename segment; otherwise the legacy `mount_id_of`
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

/// The OLD (JSON) state sidecar of `mount_id` under `state_dir`: `<state_dir>/<mount_id>.json`. Read only, a
/// state saved before states were msgpack ([`read_state`]); a save writes [`state_path`].
pub fn sidecar_path(state_dir: &Path, mount_id: &str) -> PathBuf {
    state_dir.join(format!("{mount_id}.json"))
}

/// Durably write `value` to `path` as pretty JSON, creating `path`'s parent directory first if it
/// does not exist yet. The replace is `vike_utility::atomic_file::write_atomic` (module doc); a path
/// this run could not read is refused (`refuse_unread`).
pub fn write_json_atomic(path: &Path, value: &serde_json::Value) -> std::io::Result<()> {
    refuse_unread(path)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let bytes = serde_json::to_vec_pretty(value).map_err(std::io::Error::other)?;
    vike_utility::atomic_file::write_atomic(path, &bytes)
}

/// Read + parse the JSON sidecar at `path`: `None` when there is none. One that exists and cannot be
/// read or parsed is `None` too, never a load-time crash, and never a quiet one: ERROR, set aside
/// (`set_aside_unread`).
pub fn read_json(path: &Path) -> Option<serde_json::Value> {
    match vike_utility::atomic_file::read_if_present(path) {
        Ok(None) => None,
        Ok(Some(bytes)) => match serde_json::from_slice(&bytes) {
            Ok(value) => Some(value),
            Err(e) => set_aside_unread(path, &e),
        },
        Err(e) => set_aside_unread(path, &e),
    }
}

/// The sidecar at `path` exists and could not be read (`why`): an ERROR naming it, then set aside so
/// a save never replaces it, or, when it cannot be, held against every write of this run
/// ([`hold_unread`]). Always `None`: the strategy starts with no saved state, as the line says.
fn set_aside_unread<T>(path: &Path, why: &dyn std::fmt::Display) -> Option<T> {
    match vike_utility::unreadable_file::set_aside(path) {
        Ok(aside) => tracing::error!(
            target: "vike_core::strategy_state",
            path = %path.display(),
            set_aside = %aside.display(),
            error = %why,
            "a saved strategy state exists and cannot be read: it is set aside, and the strategy \
             starts with NO saved state"
        ),
        Err(e) => {
            hold_unread(path);
            tracing::error!(
                target: "vike_core::strategy_state",
                path = %path.display(),
                error = %why,
                set_aside_error = %e,
                "a saved strategy state exists and cannot be read, nor set aside: the strategy \
                 starts with NO saved state, and nothing writes the file until a restart"
            );
        }
    }
    None
}

/// The STATE sidecar of `mount_id` under `state_dir`: `<state_dir>/<mount_id>.msgpack`, the strategy's
/// state as its own bytes (`vike_model::StrategyState`, msgpack with field names: decision 0296's ABI 15,
/// the owner's "2 (б)" of 2026-11-19). Its JSON predecessor ([`sidecar_path`]) is still READ
/// ([`read_state`]), never written: a new format reads the old one, and nothing is migrated.
pub fn state_path(state_dir: &Path, mount_id: &str) -> PathBuf {
    state_dir.join(format!("{mount_id}.msgpack"))
}

/// Durably write `state`'s bytes to `path`, creating its parent directory first, through the same
/// atomic replace (and the same refusal) as [`write_json_atomic`].
pub fn write_state_atomic(path: &Path, state: &vike_model::StrategyState) -> std::io::Result<()> {
    refuse_unread(path)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    vike_utility::atomic_file::write_atomic(path, state.bytes())
}

/// The state saved for `mount_id` under `state_dir`: its msgpack sidecar ([`state_path`]), else, only when
/// that file does not exist, the OLD JSON one ([`sidecar_path`], a state saved before msgpack), read into its
/// msgpack encoding. `None` when neither exists. A sidecar that exists and cannot be read, or holds no
/// state, is `None` as well, and never a quiet one: it is set aside with an ERROR (the module doc), and
/// the OLD JSON one beside it with it.
pub fn read_state(state_dir: &Path, mount_id: &str) -> Option<vike_model::StrategyState> {
    let path = state_path(state_dir, mount_id);
    let why = match vike_utility::atomic_file::read_if_present(&path) {
        Ok(Some(bytes)) => match vike_model::StrategyState::check(&bytes) {
            Ok(()) => return Some(vike_model::StrategyState::from_bytes(bytes)),
            Err(e) => e,
        },
        // Only a MISSING msgpack sidecar falls back to the older JSON.
        Ok(None) => {
            return read_json(&sidecar_path(state_dir, mount_id))
                .map(|old| vike_model::StrategyState::from_json_value(&old));
        }
        Err(e) => e.to_string(),
    };
    // The older JSON is what this file superseded: aside with it, so no later read loads that older
    // state in place of the one nobody could read.
    let older = sidecar_path(state_dir, mount_id);
    if let Err(e) = vike_utility::unreadable_file::set_aside(&older)
        && e.kind() != std::io::ErrorKind::NotFound
    {
        tracing::error!(
            target: "vike_core::strategy_state",
            path = %older.display(),
            error = %e,
            "the older JSON state beside a saved state that cannot be read could not be set aside: \
             a later start may load that older state"
        );
    }
    set_aside_unread(&path, &why)
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
        // no temporary file of any name left behind after a successful publish
        let left: Vec<_> =
            std::fs::read_dir(&*dir).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert_eq!(
            left,
            vec![std::ffi::OsString::from("m1.json")],
            "tmp renamed away, not left behind"
        );
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

    /// The state sidecar (decision 0296's ABI 15): a save writes msgpack, which a read prefers; an OLD
    /// JSON sidecar alone still reads, as its msgpack encoding; a msgpack file that is not one value is
    /// "no saved state", now and at the next read, and so is no file at all.
    #[test]
    fn the_state_sidecar_is_msgpack_and_an_old_json_one_still_reads() {
        use vike_model::StrategyState;
        let dir = unique_temp_dir("state");
        assert_eq!(read_state(&dir, "m4"), None, "nothing saved");
        let old = serde_json::json!({"n": 1, "kappa": 0.5});
        write_json_atomic(&sidecar_path(&dir, "m4"), &old).unwrap();
        assert_eq!(
            read_state(&dir, "m4"),
            Some(StrategyState::from_json_value(&old)),
            "the old JSON"
        );
        let new = StrategyState::of(&serde_json::json!({"n": 2})).unwrap();
        write_state_atomic(&state_path(&dir, "m4"), &new).unwrap();
        assert_eq!(read_state(&dir, "m4"), Some(new), "the msgpack sidecar wins");
        std::fs::write(state_path(&dir, "m4"), [0xc1]).unwrap();
        assert_eq!(read_state(&dir, "m4"), None, "a corrupt msgpack sidecar is no state");
        assert_eq!(read_state(&dir, "m4"), None, "nor is the stale JSON it superseded, later");
    }

    /// Every entry of `dir` whose name starts with `prefix`.
    fn entries_starting(dir: &Path, prefix: &str) -> Vec<PathBuf> {
        std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().path())
            .filter(|p| p.file_name().unwrap().to_string_lossy().starts_with(prefix))
            .collect()
    }

    /// **A sidecar that exists and cannot be read is set aside with an ERROR naming where, and never
    /// written over** (a directory stands in for an unreadable file: a lane may run as root, which
    /// reads a mode-000 file). The strategy starts with no state; the older JSON beside it goes aside
    /// too, so a second read loads nothing in its place; the next save starts a new file and the set-aside
    /// copy keeps what it held.
    #[tracing_test::traced_test]
    #[test]
    fn an_unreadable_sidecar_is_set_aside_with_an_error_and_a_save_never_writes_over_it() {
        use vike_model::StrategyState;
        let dir = unique_temp_dir("unreadable");
        write_json_atomic(&sidecar_path(&dir, "m5"), &serde_json::json!({"n": 1})).unwrap();
        std::fs::create_dir(state_path(&dir, "m5")).unwrap();
        std::fs::write(state_path(&dir, "m5").join("held"), b"what it knew").unwrap();

        assert_eq!(read_state(&dir, "m5"), None, "the strategy starts with no saved state");
        let aside = entries_starting(&dir, "m5.msgpack.unreadable-");
        assert_eq!(aside.len(), 1, "set aside beside itself: {aside:?}");
        assert_eq!(std::fs::read(aside[0].join("held")).unwrap(), b"what it knew");
        assert_eq!(entries_starting(&dir, "m5.json.unreadable-").len(), 1, "the older JSON too");
        assert_eq!(read_state(&dir, "m5"), None, "no older state is loaded in its place");

        let new = StrategyState::of(&serde_json::json!({"n": 2})).unwrap();
        write_state_atomic(&state_path(&dir, "m5"), &new).unwrap();
        assert_eq!(read_state(&dir, "m5"), Some(new), "the next save starts a new file");
        assert_eq!(std::fs::read(aside[0].join("held")).unwrap(), b"what it knew");

        let named = aside[0].display().to_string();
        logs_assert(|lines: &[&str]| {
            lines
                .iter()
                .any(|l| l.contains("ERROR") && l.contains(&named))
                .then_some(())
                .ok_or_else(|| format!("no ERROR line names {named}"))
        });
    }

    /// A path this run could not read and could not set aside is held: no write reaches it.
    #[test]
    fn a_path_held_this_run_is_never_written() {
        let dir = unique_temp_dir("held");
        let path = state_path(&dir, "m6");
        hold_unread(&path);
        let state = vike_model::StrategyState::of(&serde_json::json!({"n": 1})).unwrap();
        assert!(write_state_atomic(&path, &state).is_err());
        assert!(write_json_atomic(&path, &serde_json::json!({"n": 1})).is_err());
        assert!(!dir.exists(), "nothing was created on the way to a refused write");
    }
}
