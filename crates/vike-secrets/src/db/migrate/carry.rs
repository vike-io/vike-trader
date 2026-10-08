//! **The ONE reader of a credential FILE left in this workspace** — `vike-cli secrets migrate`'s
//! read-only CARRY of a `secrets.env` / `node.env` into the settings database.
//!
//! # Why a file reader survives the file plane
//!
//! The credential FILE store — `secrets.env` and `node.env` answering a box that had no settings
//! database, parsed on every read and rewritten by every write — was removed on the owner's order of
//! 2026-10-07 (*"we don't use any files anymore, we use sqlite"*). Nothing reads either file AS A
//! STORE now: a box with no database has no credentials, and every venue mounts paper.
//!
//! What is kept is the ONE-TIME CARRY. A box that still holds its keys in a file has exactly one
//! way into the database, and it is `vike-cli secrets migrate`, which reads both files and writes
//! the rows. Removing this reader would strand every such box, and nobody can prove none exists —
//! so it stays, named for what it is and living beside the only thing that calls it. The other caller
//! is the finding that SAYS such a file is unread (`crate::store`'s `unread_credential_file`), which
//! counts the keyed names so the report can tell an empty template from a box's live keys; it drops
//! every value before it returns.
//!
//! **Read-only, always.** Nothing in this workspace deletes, moves, truncates or rewrites a
//! credential file: it may be the user's only copy of live venue keys. The carry reads it and writes
//! ELSEWHERE.

use std::collections::HashMap;
use std::path::Path;

use crate::store::SecretsError;

/// Minimal `KEY=VALUE` parser (`#` comments; optional surrounding quotes) — the grammar every
/// `secrets.env` / `node.env` on a real box was written in, read without a dotenv dependency.
/// Returns the map instead of mutating the process env (`set_var` is unsafe under threads).
///
/// ⚠ Its expressive limits are real: the format cannot represent a value containing a newline, and
/// it strips surrounding quotes unconditionally. LAST-wins: a name repeated further down the file
/// takes the later value, which is what every reader of the file store saw.
pub(crate) fn parse_credential_file(text: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((k, v)) = line.split_once('=') else { continue };
        let v = v.trim().trim_matches('"').trim_matches('\'');
        out.insert(k.trim().to_string(), v.to_string());
    }
    out
}

/// **Read one credential file for the carry.**
///
/// | on disk | result |
/// |---|---|
/// | present and readable | the parsed map |
/// | absent | an EMPTY map — nothing to carry |
/// | present and unreadable | [`SecretsError`] — loud: a file nobody can read must not migrate as "empty" |
///
/// Nothing here writes, moves or deletes the file.
pub(crate) fn read_credential_file(path: &Path) -> Result<HashMap<String, String>, SecretsError> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(parse_credential_file(&text)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(HashMap::new()),
        Err(source) => Err(SecretsError { path: path.to_path_buf(), source }),
    }
}

#[path = "carry_tests.rs"]
#[cfg(test)]
mod carry_tests;
