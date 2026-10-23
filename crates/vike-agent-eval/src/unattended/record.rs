//! The per-run record on disk: its writer, its file name, redaction, the text cap and the UTC stamps.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::Value;

use super::{MAX_TEXT_BYTES, REDACTED, RUN_FILE_PREFIX, Summary};

/// Write the run record, and return the file it landed in.
///
/// ⚠ `secrets` is the VALUES this process holds, and every string in the document is walked for
/// them. It is a parameter for the same reason the scrub list is: only the binary knows what it
/// read, and a library that fetched a credential in order to redact it would be a library that holds
/// one.
///
/// ⚠ `create_new`: the file is never opened if it exists. Two runs starting in the same second under
/// the same pid is not a shape that occurs, but "never overwrite a record" is not a property to rest
/// on arithmetic, so a collision takes the next free suffix rather than replacing anything.
pub fn write_summary(dir: &Path, summary: &Summary, secrets: &[&str]) -> Result<PathBuf, String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    let mut record = summary.to_json();
    redact_in_place(&mut record, secrets);
    let body =
        serde_json::to_string_pretty(&record).map_err(|e| format!("encode the run record: {e}"))?;

    let stem = run_stem(summary.started_ms);
    for attempt in 0..64u32 {
        let name =
            if attempt == 0 { format!("{stem}.json") } else { format!("{stem}-{attempt}.json") };
        let path = dir.join(&name);
        match std::fs::File::create_new(&path) {
            Ok(mut f) => {
                use std::io::Write;
                f.write_all(body.as_bytes())
                    .map_err(|e| format!("write {}: {e}", path.display()))?;
                f.sync_all().map_err(|e| format!("sync {}: {e}", path.display()))?;
                return Ok(path);
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(format!("create {}: {e}", path.display())),
        }
    }
    Err(format!("64 run-record names under {} were already taken", dir.display()))
}

/// The name every artefact of one run shares: `run-20260906T031700Z-1234`.
///
/// ONE function, because the record and the server's stderr file must be recognisable as a pair in a
/// directory listing, and two independent renderings of "which run is this" would drift the first
/// time either changed.
pub fn run_stem(started_ms: i64) -> String {
    format!("{RUN_FILE_PREFIX}{}-{}", compact_stamp(started_ms), std::process::id())
}

/// Replace every occurrence of every secret, in every string of the document.
///
/// A whole-document walk rather than a per-field one: the fields are added to over time and a
/// per-field list is a second roster that rots. Empty and whitespace-only entries are skipped —
/// replacing the empty string would rewrite the document into nothing.
pub fn redact_in_place(value: &mut Value, secrets: &[&str]) {
    match value {
        Value::String(s) => {
            for secret in secrets {
                if secret.trim().is_empty() {
                    continue;
                }
                if s.contains(secret) {
                    *s = s.replace(secret, REDACTED);
                }
            }
        }
        Value::Array(items) => {
            for item in items {
                redact_in_place(item, secrets);
            }
        }
        Value::Object(map) => {
            for v in map.values_mut() {
                redact_in_place(v, secrets);
            }
        }
        _ => {}
    }
}

/// One recorded free-text field, capped at [`MAX_TEXT_BYTES`] on a character boundary and SAYING it
/// was capped.
pub fn cap_text(text: &str) -> String {
    if text.len() <= MAX_TEXT_BYTES {
        return text.to_string();
    }
    let mut end = MAX_TEXT_BYTES;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}… <truncated at {MAX_TEXT_BYTES} bytes>", &text[..end])
}

/// Milliseconds since the Unix epoch. A clock read, in a crate the determinism ratchet
/// (`crates/vike-ops/tests/architecture/clock_pin.rs`) deliberately does not scope: a record of WHEN a run
/// happened is the wall clock by definition.
pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|d| i64::try_from(d.as_millis()).ok())
        .unwrap_or(0)
}

/// `1970-01-01T00:00:00Z`, for the record's human-readable fields.
pub fn utc_stamp(ms: i64) -> String {
    let (y, mo, d, h, mi, s) = utc_parts(ms);
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{s:02}Z")
}

/// The same instant with no separators, for a FILE NAME: `19700101T000000Z`. Windows cannot hold a
/// `:` in a path, and the compact form still sorts in calendar order, which is the whole reason the
/// stamp rather than the epoch millisecond is what a reader sees in a directory listing.
pub fn compact_stamp(ms: i64) -> String {
    let (y, mo, d, h, mi, s) = utc_parts(ms);
    format!("{y:04}{mo:02}{d:02}T{h:02}{mi:02}{s:02}Z")
}

/// Split an epoch-millisecond instant into UTC calendar parts.
///
/// ⚠ Hand-rolled rather than reached for: this crate carries NO `vike-*` dependency by design (see
/// the module doc), so `vike_model`'s calendar helpers are unavailable to it, and adding a date
/// crate for six fields would be a new dependency in a tree whose manifest argues every one. The
/// algorithm is Howard Hinnant's `civil_from_days`, which is exact for the whole proleptic Gregorian
/// range; `utc_stamp_matches_known_instants` pins it against four independently known instants
/// rather than against itself.
fn utc_parts(ms: i64) -> (i64, i64, i64, i64, i64, i64) {
    let days = ms.div_euclid(86_400_000);
    let rem_ms = ms.rem_euclid(86_400_000);
    let secs = rem_ms / 1_000;
    let (h, mi, s) = (secs / 3_600, (secs % 3_600) / 60, secs % 60);

    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let mo = if mp < 10 { mp + 3 } else { mp - 9 };
    (if mo <= 2 { y + 1 } else { y }, mo, d, h, mi, s)
}
