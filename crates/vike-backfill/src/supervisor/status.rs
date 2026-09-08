//! The supervisor's STATUS SURFACE: one JSON document, rewritten after every pass, carrying
//! last-run / next-run / heal-queue / last-error per configured source. Deliberately a plain file
//! and not a socket or an HTTP endpoint — no new transport, no port to secure, and `cat`/`jq` (or a
//! future Data-Manager panel) can read it while the loop runs.
//!
//! WRITTEN ATOMICALLY: serialize → write `<path>.tmp` → rename over `<path>`. A reader therefore
//! always sees a complete document, never a half-written one, on every platform the workspace
//! targets (`std::fs::rename` replaces an existing destination on Windows as well as unix).
//!
//! A status write FAILURE is never fatal — the caller logs it and keeps collecting. Collecting data
//! is the job; publishing telemetry about it is not worth killing the loop over.
//!
//! FIELD POLICY: every optional field carries `#[serde(default, skip_serializing_if = ...)]`, so an
//! absent error / never-run source simply omits the key, and an older document still deserializes
//! after a future field is added.

use std::path::Path;

use serde::{Deserialize, Serialize};

use super::SupervisorError;

/// `skip_serializing_if` helper — serde hands these a `&T`, so `std::ops::Not::not` (which takes
/// `bool` by value) cannot be named directly.
fn is_false(b: &bool) -> bool {
    !*b
}

/// `skip_serializing_if` helper for the zero-valued counters (a quiet source stays terse).
fn is_zero_u64(n: &u64) -> bool {
    *n == 0
}

/// `skip_serializing_if` helper for the zero-valued counters.
fn is_zero_usize(n: &usize) -> bool {
    *n == 0
}

/// `skip_serializing_if` helper for the zero-valued counters.
fn is_zero_u32(n: &u32) -> bool {
    *n == 0
}

/// The whole status document: when it was generated, how many loop passes have completed, and one
/// row per configured source (in config declaration order).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SupervisorStatus {
    /// Epoch-ms at which this document was built.
    pub generated_ms: i64,
    /// Completed supervisor loop passes (a pass may skip every source that is not yet due).
    #[serde(default)]
    pub passes: u64,
    /// One row per `[[source]]`, in declaration order.
    #[serde(default)]
    pub sources: Vec<SourceStatus>,
}

/// One source's row.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceStatus {
    /// The config `name`.
    pub name: String,
    /// The registry row driving it.
    pub collector: String,
    /// The venue partition it writes under (from the registry, never operator-set).
    pub venue: String,
    /// The series kind / interval it maintains.
    pub kind: String,
    pub interval: String,
    pub symbols: Vec<String>,
    /// Epoch-ms of the last pass over this source; absent until it has run once.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_run_ms: Option<i64>,
    /// Epoch-ms this source is next expected to run (already reflects any active backoff).
    pub next_run_ms: i64,
    /// Passes whose FRESHNESS lane ended with an error, back to back. Reset to 0 by a clean
    /// freshness lane — it is what drives the exponential backoff and parking. Heal-lane failures
    /// are counted separately (`heal_failures`) precisely so healing history can never slow "now"
    /// down.
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub consecutive_failures: u32,
    /// Passes whose HEAL lane (a `series_gaps` lookup or a gap-fill fetch) ended with an error, back
    /// to back. Purely informational: it changes no schedule.
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub heal_failures: u32,
    /// True once `consecutive_failures` reached the source's `max_consecutive_failures` — the
    /// source is skipped until the supervisor restarts.
    #[serde(default, skip_serializing_if = "is_false")]
    pub parked: bool,
    /// Gap ranges the LAST pass planned to heal, summed across the source's symbols. A steadily
    /// shrinking number is the supervisor catching up; a stuck one means the venue has no data for
    /// those days.
    #[serde(default, skip_serializing_if = "is_zero_usize")]
    pub heal_queue: usize,
    /// Rows this supervisor process has appended for this source since it started.
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub rows_ingested: u64,
    /// Passes made over this source (not loop ticks).
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub passes: u64,
    /// The last error text seen for this source, cleared by a clean pass. Never carries credentials
    /// — the collectors it quotes are keyless public REST.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
}

/// Serialize `status` and publish it at `path` atomically (temp file + rename), creating the parent
/// directory if needed.
pub fn write_status(path: &Path, status: &SupervisorStatus) -> Result<(), SupervisorError> {
    let json = serde_json::to_string_pretty(status)
        .map_err(|e| SupervisorError::Io(format!("serialize status: {e}")))?;
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)
            .map_err(|e| SupervisorError::Io(format!("create {}: {e}", parent.display())))?;
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, json.as_bytes())
        .map_err(|e| SupervisorError::Io(format!("write {}: {e}", tmp.display())))?;
    std::fs::rename(&tmp, path).map_err(|e| {
        SupervisorError::Io(format!("rename {} -> {}: {e}", tmp.display(), path.display()))
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row() -> SourceStatus {
        SourceStatus {
            name: "binance-majors-1m".to_string(),
            collector: "binance_klines".to_string(),
            venue: "binance".to_string(),
            kind: "bar".to_string(),
            interval: "1m".to_string(),
            symbols: vec!["BTCUSDT".to_string()],
            last_run_ms: Some(1_700_000_000_000),
            next_run_ms: 1_700_000_060_000,
            consecutive_failures: 2,
            heal_failures: 1,
            parked: false,
            heal_queue: 3,
            rows_ingested: 4_242,
            passes: 7,
            last_error: Some("venue fetch: 429".to_string()),
        }
    }

    #[test]
    fn a_status_document_round_trips_through_json() {
        let status =
            SupervisorStatus { generated_ms: 1_700_000_000_000, passes: 9, sources: vec![row()] };
        let json = serde_json::to_string_pretty(&status).unwrap();
        let back: SupervisorStatus = serde_json::from_str(&json).unwrap();
        assert_eq!(back, status);
    }

    #[test]
    fn a_quiet_source_omits_every_zero_and_absent_field() {
        let status = SupervisorStatus {
            generated_ms: 1,
            passes: 0,
            sources: vec![SourceStatus {
                name: "quiet".to_string(),
                collector: "okx_klines".to_string(),
                venue: "okx".to_string(),
                kind: "bar".to_string(),
                interval: "1m".to_string(),
                symbols: vec!["BTC-USDT".to_string()],
                next_run_ms: 5,
                ..Default::default()
            }],
        };
        let json = serde_json::to_string(&status).unwrap();
        assert!(!json.contains("last_error"), "{json}");
        assert!(!json.contains("last_run_ms"), "{json}");
        assert!(!json.contains("consecutive_failures"), "{json}");
        assert!(!json.contains("heal_failures"), "{json}");
        assert!(!json.contains("parked"), "{json}");
        assert!(!json.contains("heal_queue"), "{json}");
        assert!(!json.contains("rows_ingested"), "{json}");
        // the always-present identity/schedule fields stay
        assert!(json.contains("\"next_run_ms\":5"), "{json}");
        assert!(json.contains("\"generated_ms\":1"), "{json}");
    }

    #[test]
    fn a_terse_document_still_deserializes() {
        // forward/backward compatibility: every omitted field has a serde default.
        let back: SupervisorStatus = serde_json::from_str(
            r#"{"generated_ms": 7, "sources": [
                 {"name":"a","collector":"okx_klines","venue":"okx","kind":"bar",
                  "interval":"1m","symbols":["BTC-USDT"],"next_run_ms":9}]}"#,
        )
        .unwrap();
        assert_eq!(back.generated_ms, 7);
        assert_eq!(back.passes, 0);
        assert_eq!(back.sources.len(), 1);
        assert_eq!(back.sources[0].last_run_ms, None);
        assert_eq!(back.sources[0].next_run_ms, 9);
        assert!(!back.sources[0].parked);
    }

    #[test]
    fn write_status_publishes_a_readable_document_and_overwrites_in_place() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("supervisor-status.json");

        let first = SupervisorStatus { generated_ms: 1, passes: 1, sources: vec![row()] };
        write_status(&path, &first).unwrap();
        let read: SupervisorStatus =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(read, first, "the parent dir is created and the document is complete");

        // a second write REPLACES the first at the same path (rename-over, not an error)
        let second = SupervisorStatus { generated_ms: 2, passes: 2, sources: vec![row()] };
        write_status(&path, &second).unwrap();
        let read: SupervisorStatus =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(read, second);

        // the temp file is renamed away, never left behind
        assert!(!path.with_extension("json.tmp").exists());
    }

    #[test]
    fn the_default_document_is_empty_not_malformed() {
        let json = serde_json::to_string(&SupervisorStatus::default()).unwrap();
        let back: SupervisorStatus = serde_json::from_str(&json).unwrap();
        assert_eq!(back, SupervisorStatus::default());
        assert!(back.sources.is_empty());
    }
}
