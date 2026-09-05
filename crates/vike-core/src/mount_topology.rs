//! RUNTIME mount TOPOLOGY as daemon-level STATE (split-plane B5, residual closed): the sidecar
//! that lets a runtime-mounted strategy survive a restart — crash or clean — without the journal
//! ever carrying a topology frame.
//!
//! # Why this is a SIDECAR and not a journal record (the design seam)
//!
//! B5 deliberately excluded `Command::MountStrategy`/`Command::UnmountStrategy` from the journaled
//! set (the `journaled` match in `crate::runtime`'s `dispatch`), and that exclusion STANDS. The
//! journal is the replay determinism fence: every journaled record must re-fold bit-identically
//! through the mount-less replay core (`crate::replay`'s `replay_from` spawns a core with NO
//! `CoreConfig::strategy_factory` — vike-core sits below the strategy crates and can resolve
//! nothing), so a journaled mount could only ever replay as a REFUSAL note — a divergence, not a
//! restore, and a fence over it would gate nothing. Topology is SESSION state, not order state:
//! it belongs beside the strategy-state sidecars ([`crate::strategy_state`]), written through the
//! same atomic-rename JSON shape, gated behind the same opt-in `CoreConfig::state_dir`, invisible
//! to `state_hash` and to the replay core. What closes the crash-restore residual is therefore
//! NOT a journal write but a startup replay at the COMPOSITION ROOT — the one place a
//! `strategy_factory` exists: the daemon reads this file after its core spawns and BEFORE feeds
//! arm, and re-sends each record as an ordinary `Command::MountStrategy` through the same command
//! lane a wire mount takes (vike-tradehub's `mount_factory`'s `resurrect_runtime_mounts`),
//! meeting the same refusals.
//!
//! # Lifecycle (who writes, who forgets)
//!
//! - **Mount** (`CoreThread::mount_strategy_runtime`, on SUCCESS only): [`upsert`] the record —
//!   the spec that mounted, keyed by its derived mount id. Written at command cadence in the arm,
//!   off the per-event fold, best-effort (a write failure warns and the mount stays live).
//! - **Unmount** (`CoreThread::unmount_strategy_runtime`): [`remove`] the record — an explicit
//!   unmount is the ONE thing that forgets a runtime mount. A clean shutdown does NOT remove
//!   records, deliberately: a runtime mount survives `systemctl restart` exactly as it survives a
//!   crash, and "the operator said stop the daemon" is not "the operator said unmount".
//! - **Spawn-time (profile) mounts never enter this file**: the profile re-creates them at every
//!   boot; the file records only what the profile cannot re-derive.
//!
//! Fail-open like the strategy-state sidecars: an absent file is the ordinary empty state; a file
//! that exists but does not parse is LOUDLY warned about and read as empty — a corrupt topology
//! must never refuse a boot (the daemon still starts, mount-less, and says why). The next
//! successful mount rewrites the file whole (every write is a full-list atomic rewrite through
//! [`crate::strategy_state::write_json_atomic`]), so corruption never propagates.
//!
//! Shared-`state_dir` caveat (inherited from the strategy-state sidecars, not new here): nothing
//! locks this file, because the only writer is the single-writer fold thread of ONE core. Two
//! cores pointed at one `state_dir` already share/clobber `<mount_id>.json` sidecars; they would
//! share this file the same way. The journal's `LOCK` interlock is the mechanism that already
//! makes that deployment shape loud.

use std::path::{Path, PathBuf};

use vike_exec::MountSpec;

/// The topology sidecar's filename under `CoreConfig::state_dir` — a sibling of the per-mount
/// `<mount_id>.json` strategy-state sidecars (mount ids are sanitized to alphanumerics-and-`_`,
/// so no mount id's sidecar can collide with this name: `mount_id_with` can never produce a `.`).
pub const TOPOLOGY_FILE: &str = "runtime_mounts.json";

/// One resurrectable runtime mount: the SPEC-vocabulary payload exactly as the mount command
/// carried it (venue/symbol/interval + `controller_id` + `name`/`rhai`/`params` — resolution is
/// deliberately NOT captured: a record holds what to ASK the factory for, never what the factory
/// answered, so a re-resolve at boot meets today's registry/script and today's refusals), plus
/// the wall-clock time the mount last LANDED on a core (diagnostics only — a resurrected mount
/// re-stamps it).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct MountRecord {
    /// The `Command::MountStrategy` payload verbatim.
    pub spec: MountSpec,
    /// Wall-clock ms when this mount last landed (mount time, not resurrect-read time).
    #[serde(default)]
    pub mounted_at_ms: u64,
}

impl MountRecord {
    /// A record for `spec`, stamped with the caller-supplied wall clock in ms.
    ///
    /// ⚠ TIME IS AN INPUT HERE, and this file may not read one: vike-core is a
    /// determinism-critical crate (`backtest == paper == live`), and
    /// `crates/vike-ops/tests/clock_pin.rs`'s `clock_readers_do_not_grow` refuses a library-layer
    /// ambient read in it — a fold that invents a clock cannot be replayed, and three engines
    /// cannot be proven equal on an input one of them made up. The runtime already carries the
    /// seam (`CoreConfig::clock`, a `vike_model::clock::Clock`), so the mount arm passes
    /// `self.config.clock.now_ms()` and a test passes whatever it wants to assert about.
    /// A negative or pre-epoch reading clamps to 0 — the field is human-facing provenance
    /// ("when did this mount land"), never an ordering key.
    pub fn stamped(spec: MountSpec, now_ms: i64) -> Self {
        Self { spec, mounted_at_ms: now_ms.max(0) as u64 }
    }

    /// The record's mount identity — [`crate::strategy_state::mount_id_with`] over the spec, i.e.
    /// the SAME id the runtime keys the mount, its state sidecar and its journal provenance by.
    pub fn mount_id(&self) -> String {
        crate::strategy_state::mount_id_with(
            self.spec.controller_id.as_deref(),
            &self.spec.venue,
            &self.spec.symbol,
            &self.spec.interval,
        )
    }
}

/// The topology file path under `state_dir`.
pub fn topology_path(state_dir: &Path) -> PathBuf {
    state_dir.join(TOPOLOGY_FILE)
}

/// Read every recorded runtime mount, in mount order. Fail-open BOTH ways, like
/// [`crate::strategy_state::read_json`]: an absent file is the ordinary empty state (silent); a
/// present file that does not parse is a LOUD warn and an empty answer — never an `Err`, so a
/// corrupt topology can never refuse a boot. The warn names the path so the operator can inspect
/// or delete the file; the next successful mount rewrites it whole.
pub fn read(state_dir: &Path) -> Vec<MountRecord> {
    let path = topology_path(state_dir);
    let bytes = match std::fs::read(&path) {
        Ok(b) => b,
        Err(_) => return Vec::new(),
    };
    match serde_json::from_slice::<Vec<MountRecord>>(&bytes) {
        Ok(records) => records,
        Err(e) => {
            tracing::warn!(
                target: "vike_core::mount_topology",
                path = %path.display(),
                error = %e,
                "runtime-mount topology sidecar is unreadable — resurrecting NOTHING (the daemon \
                 still boots; re-mount by hand, or delete the file — the next runtime mount \
                 rewrites it whole)"
            );
            Vec::new()
        }
    }
}

/// Insert-or-replace `record` (keyed by [`MountRecord::mount_id`]) and atomically rewrite the
/// whole file. A replace keeps the record's position (a re-mount of the same id stays where it
/// was); an insert appends (mount order is resurrect order).
pub fn upsert(state_dir: &Path, record: MountRecord) -> std::io::Result<()> {
    let mut records = read(state_dir);
    let id = record.mount_id();
    match records.iter_mut().find(|r| r.mount_id() == id) {
        Some(slot) => *slot = record,
        None => records.push(record),
    }
    write_all(state_dir, &records)
}

/// Remove the record whose [`MountRecord::mount_id`] equals `mount_id` (already-sanitized — the
/// unmount arm passes the id it resolved the slot by) and atomically rewrite. A no-op when no
/// record matches: unmounting an unrecorded mount (a spawn-time profile mount, or one mounted
/// before `state_dir` was armed) must not churn the file.
pub fn remove(state_dir: &Path, mount_id: &str) -> std::io::Result<()> {
    let records = read(state_dir);
    let kept: Vec<MountRecord> = records.into_iter().filter(|r| r.mount_id() != mount_id).collect();
    if kept.len() == read(state_dir).len() {
        return Ok(());
    }
    write_all(state_dir, &kept)
}

/// Whole-list atomic rewrite through the strategy-state sidecar's own fsync+rename shape.
fn write_all(state_dir: &Path, records: &[MountRecord]) -> std::io::Result<()> {
    let value = serde_json::to_value(records).map_err(std::io::Error::other)?;
    crate::strategy_state::write_json_atomic(&topology_path(state_dir), &value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn unique_temp_dir(tag: &str) -> PathBuf {
        static N: AtomicU64 = AtomicU64::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!(
            "vike-core-mount-topology-{tag}-{}-{}-{}",
            std::process::id(),
            nanos,
            n
        ));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    fn spec(cid: &str) -> MountSpec {
        MountSpec {
            venue: "sim".into(),
            symbol: "BTCUSDT".into(),
            interval: "1m".into(),
            account: None,
            controller_id: Some(cid.to_string()),
            name: Some("test_quoter".into()),
            rhai: None,
            params: serde_json::json!({}),
        }
    }

    /// Upsert appends in mount order, replaces in place on a same-id re-mount, and `remove`
    /// forgets exactly the named id.
    #[test]
    fn upsert_orders_replaces_and_remove_forgets() {
        let dir = unique_temp_dir("crud");
        assert!(read(&dir).is_empty(), "absent file reads empty, silently");

        upsert(&dir, MountRecord::stamped(spec("rt-a"), 1_700_000_000_000)).unwrap();
        upsert(&dir, MountRecord::stamped(spec("rt-b"), 1_700_000_000_000)).unwrap();
        let ids: Vec<String> = read(&dir).iter().map(MountRecord::mount_id).collect();
        assert_eq!(ids, vec!["rt_a", "rt_b"], "mount order is resurrect order, ids sanitized");

        // Same-id upsert replaces IN PLACE (position kept), never duplicates.
        let mut re = MountRecord::stamped(spec("rt-a"), 1_700_000_000_000);
        re.spec.params = serde_json::json!({"n": 2});
        upsert(&dir, re).unwrap();
        let records = read(&dir);
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].spec.params, serde_json::json!({"n": 2}));

        remove(&dir, "rt_a").unwrap();
        let ids: Vec<String> = read(&dir).iter().map(MountRecord::mount_id).collect();
        assert_eq!(ids, vec!["rt_b"]);
        // Removing an unrecorded id is a quiet no-op (spawn-time mounts are never recorded).
        remove(&dir, "never_recorded").unwrap();
        assert_eq!(read(&dir).len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A corrupt file reads EMPTY (loudly, but without an `Err` — a boot must never fail on it),
    /// and the next upsert rewrites the file whole, so corruption never propagates.
    #[test]
    fn a_corrupt_file_reads_empty_and_the_next_upsert_rewrites_it() {
        let dir = unique_temp_dir("corrupt");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(topology_path(&dir), b"{ not json [").unwrap();
        assert!(read(&dir).is_empty(), "corrupt reads empty, never panics or errors");

        upsert(&dir, MountRecord::stamped(spec("rt-a"), 1_700_000_000_000)).unwrap();
        let records = read(&dir);
        assert_eq!(records.len(), 1, "the rewrite replaced the corrupt bytes wholesale");
        assert_eq!(records[0].mount_id(), "rt_a");
        // ...and atomically: no `.tmp` sibling left behind.
        let mut tmp = topology_path(&dir).into_os_string();
        tmp.push(".tmp");
        assert!(!PathBuf::from(tmp).exists(), "tmp renamed away, not left behind");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The record round-trips through serde with the spec verbatim, and `mounted_at_ms` is
    /// `#[serde(default)]` so a hand-written or older record without it still reads.
    #[test]
    fn records_roundtrip_and_timestamp_is_defaulted() {
        let dir = unique_temp_dir("roundtrip");
        upsert(&dir, MountRecord::stamped(spec("rt-a"), 1_700_000_000_000)).unwrap();
        let records = read(&dir);
        assert_eq!(records[0].spec.name.as_deref(), Some("test_quoter"));
        assert!(records[0].mounted_at_ms > 0, "stamped with wall clock");

        // A record written without the timestamp field still reads (defaulted to 0).
        std::fs::write(
            topology_path(&dir),
            serde_json::to_vec(&serde_json::json!([{ "spec": spec("rt-b") }])).unwrap(),
        )
        .unwrap();
        let records = read(&dir);
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].mount_id(), "rt_b");
        assert_eq!(records[0].mounted_at_ms, 0);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
