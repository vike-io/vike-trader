//! Safe `KEY=value` UPSERT into the credential store — the one sanctioned way to write it.
//!
//! [`upsert_env`] is a pure string transform: replace matching `KEY=value` lines in place, append
//! new keys at the end, and preserve every other line (comments, blank lines, ordering) verbatim —
//! it never rewrites the file from scratch. [`save_credentials`] is the thin I/O wrapper that reads
//! the current store, upserts, and writes back atomically (temp file + rename in the same
//! directory, so a crash mid-write cannot truncate the store).
//!
//! # ⚠ "Nothing ever rewrites the credential store" means WHOLESALE, not "never writes"
//!
//! The store is the user's only copy of live venue keys, so the rule that matters is that no code
//! path may regenerate it, reorder it, drop a comment, or clobber a key it was not asked to touch.
//! That is a statement about the TRANSFORM, and this module is where it is enforced and tested —
//! which is precisely why a rotating credential may be written HERE and nowhere else. A caller that
//! reaches for `fs::write` on the store is the bug this module exists to prevent.
//!
//! It lives in `vike-secrets` — the crate that already OWNS the store and has ZERO dependencies —
//! so every layer can reach it: the GUI editor (`vike_connections::env_write`, which re-exports
//! these two functions rather than keeping a second copy) and a venue bridge persisting a refreshed
//! OAuth grant (`vike_ctrader::token_store`) are on opposite sides of the layer graph and must not
//! drag each other in. A verbatim second implementation would be two things to keep in step about
//! one file's byte-level preservation.
//!
//! Matches [`crate::parse_dotenv`]'s tolerant parsing (trimmed line, first `=` splits key/value,
//! one layer of surrounding `"`/`'` stripped) so anything written here round-trips through the
//! reader.
//!
//! SECURITY: this module never logs anything at all — it has no logging dependency and cannot
//! acquire one without breaking the crate's zero-dependency property. Callers must log only
//! non-secret facts ("saved credentials for binance/live"), never the key/secret/token text.

use std::io;
use std::path::Path;

/// Quote a value if needed so it round-trips through [`crate::parse_dotenv`] unchanged. That parser
/// trims the raw value before stripping one layer of surrounding quotes, so any value with
/// leading/trailing whitespace or embedded spaces needs quoting to survive that trim; a plain token
/// needs none.
fn format_value(value: &str) -> String {
    let needs_quotes = value.is_empty()
        || value.starts_with(char::is_whitespace)
        || value.ends_with(char::is_whitespace)
        || value.contains(char::is_whitespace);
    if needs_quotes {
        format!("\"{value}\"")
    } else {
        value.to_string()
    }
}

/// Upsert `updates` (key, value pairs) into `existing` store text: for each key, replace an
/// existing `KEY=...` line's value in place (matched by trimmed key, ignoring surrounding
/// whitespace/comment lines), else append a new `KEY=value` line at the end. Every other line —
/// comments, blank lines, unrelated keys, and their relative order — is preserved verbatim.
/// Output always ends with a trailing newline.
pub fn upsert_env(existing: &str, updates: &[(String, String)]) -> String {
    if updates.is_empty() {
        return existing.to_string();
    }

    // Working copy of updates we can remove from as each key is matched against an existing
    // line; whatever's left over at the end gets appended as new lines.
    let mut remaining: Vec<(String, String)> = updates.to_vec();
    let mut out_lines: Vec<String> = Vec::new();

    for line in existing.lines() {
        let trimmed = line.trim();
        let existing_key = if trimmed.is_empty() || trimmed.starts_with('#') {
            None
        } else {
            trimmed.split_once('=').map(|(k, _)| k.trim())
        };

        let replaced = existing_key.and_then(|key| {
            let pos = remaining.iter().position(|(uk, _)| uk == key)?;
            let (_, value) = remaining.remove(pos);
            Some(format!("{key}={}", format_value(&value)))
        });

        out_lines.push(replaced.unwrap_or_else(|| line.to_string()));
    }

    for (key, value) in remaining {
        out_lines.push(format!("{key}={}", format_value(&value)));
    }

    let mut out = out_lines.join("\n");
    out.push('\n');
    out
}

/// Read `path` (treating "file absent" as empty content), upsert `updates`, and write the result
/// back atomically: write to a temp file in the same directory, then rename over `path`. The
/// rename is the atomic step — a crash before it leaves the original store untouched, a crash
/// after it lands the fully-written new content, and no partial/truncated store is ever
/// observable.
pub fn save_credentials(path: &Path, updates: &[(String, String)]) -> io::Result<()> {
    let existing = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e),
    };
    let updated = upsert_env(&existing, updates);

    let dir = path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or_else(|| Path::new("."));
    let file_name = path.file_name().and_then(|n| n.to_str()).unwrap_or(".env");
    let tmp_path = dir.join(format!(".{file_name}.tmp-{}", std::process::id()));

    std::fs::write(&tmp_path, updated.as_bytes())?;
    std::fs::rename(&tmp_path, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse_dotenv;

    // NOTE: every value below is a dummy placeholder ("test-key-123" etc), never a real secret.

    /// THE byte-preservation property, asserted LINE BY LINE rather than by spot `contains` checks:
    /// a store carrying comments, blank lines and several venues' keys comes back with every line
    /// it did not name IDENTICAL, in the same order. This is what makes the store safe to write for
    /// a rotating credential — a transform that reordered or dropped so much as a blank line would
    /// be rewriting the user's only copy of their keys.
    #[test]
    fn every_untouched_line_is_byte_identical_and_in_order() {
        let existing = "\
# vike credential store
# one file, in the project

BINANCE_LIVE_API_KEY=binance-key-old
BINANCE_LIVE_API_SECRET=binance-secret-old

# cTrader — the OAuth pair rotates
CTRADER_CLIENT_ID=app-id-123
CTRADER_DEMO_ACCESS_TOKEN=access-old
CTRADER_DEMO_REFRESH_TOKEN=refresh-old

OKX_DEMO_API_PASSPHRASE=okx-pass
";
        let updates = vec![
            ("CTRADER_DEMO_ACCESS_TOKEN".to_string(), "access-new".to_string()),
            ("CTRADER_DEMO_REFRESH_TOKEN".to_string(), "refresh-new".to_string()),
        ];
        let out = upsert_env(existing, &updates);

        let before: Vec<&str> = existing.lines().collect();
        let after: Vec<&str> = out.lines().collect();
        assert_eq!(before.len(), after.len(), "an upsert of existing keys must add no line");

        for (i, (b, a)) in before.iter().zip(after.iter()).enumerate() {
            let is_target = b.starts_with("CTRADER_DEMO_ACCESS_TOKEN")
                || b.starts_with("CTRADER_DEMO_REFRESH_TOKEN");
            if is_target {
                continue;
            }
            assert_eq!(b, a, "line {i} must be byte-identical: {b:?} -> {a:?}");
        }
        // …and the two targeted lines are replaced IN PLACE, keeping their position.
        assert_eq!(after[8], "CTRADER_DEMO_ACCESS_TOKEN=access-new");
        assert_eq!(after[9], "CTRADER_DEMO_REFRESH_TOKEN=refresh-new");
        assert!(!out.contains("access-old"), "the old grant must not survive");
        assert!(!out.contains("refresh-old"), "the old grant must not survive");
    }

    #[test]
    fn updating_existing_key_preserves_other_lines_and_comments() {
        let existing = "\
# top comment
BINANCE_LIVE_API_KEY=old-key-123
BINANCE_LIVE_API_SECRET=old-secret-456

# a section comment
BYBIT_DEMO_API_KEY=untouched-key
";
        let updates = vec![("BINANCE_LIVE_API_KEY".to_string(), "new-key-789".to_string())];
        let out = upsert_env(existing, &updates);

        assert!(out.contains("# top comment"));
        assert!(out.contains("# a section comment"));
        assert!(out.contains("BINANCE_LIVE_API_KEY=new-key-789"));
        assert!(out.contains("BINANCE_LIVE_API_SECRET=old-secret-456"), "unrelated key untouched");
        assert!(out.contains("BYBIT_DEMO_API_KEY=untouched-key"), "unrelated key untouched");
        assert!(!out.contains("old-key-123"), "old value must be replaced, not left behind");

        // Order preserved: BINANCE_LIVE_API_KEY line still comes before BINANCE_LIVE_API_SECRET.
        let key_pos = out.find("BINANCE_LIVE_API_KEY").unwrap();
        let secret_pos = out.find("BINANCE_LIVE_API_SECRET").unwrap();
        assert!(key_pos < secret_pos);
    }

    #[test]
    fn new_key_appends_at_end() {
        let existing = "EXISTING_KEY=value\n";
        // A PLACEHOLDER venue, deliberately: this test is about append ORDER, and spelling a real
        // credential key here would declare to `settings_registry.rs` that this crate reads that
        // venue's keys — a row nothing would back.
        let updates = vec![("TESTVENUE_DEMO_API_KEY".to_string(), "test-key-123".to_string())];
        let out = upsert_env(existing, &updates);

        assert!(out.contains("EXISTING_KEY=value"));
        assert!(out.contains("TESTVENUE_DEMO_API_KEY=test-key-123"));
        // the new line comes after the existing one
        let existing_pos = out.find("EXISTING_KEY").unwrap();
        let new_pos = out.find("TESTVENUE_DEMO_API_KEY").unwrap();
        assert!(existing_pos < new_pos);
    }

    #[test]
    fn appending_to_empty_file_produces_just_the_new_lines() {
        let out = upsert_env("", &[("A_KEY".to_string(), "test-val".to_string())]);
        assert_eq!(out, "A_KEY=test-val\n");
    }

    #[test]
    fn values_with_spaces_round_trip_through_parse_dotenv() {
        let updates = vec![
            ("SPACE_VAL".to_string(), "hello world value".to_string()),
            ("PLAIN_VAL".to_string(), "plain-token-123".to_string()),
        ];
        let out = upsert_env("", &updates);
        let parsed = parse_dotenv(&out);
        assert_eq!(parsed.get("SPACE_VAL").map(String::as_str), Some("hello world value"));
        assert_eq!(parsed.get("PLAIN_VAL").map(String::as_str), Some("plain-token-123"));
    }

    #[test]
    fn empty_updates_returns_existing_unchanged() {
        let existing = "FOO=bar\n";
        assert_eq!(upsert_env(existing, &[]), existing);
    }

    #[test]
    fn no_op_when_key_matches_but_no_updates_targets_it() {
        let existing = "FOO=bar\n# comment\n";
        let out = upsert_env(existing, &[("BAZ".to_string(), "qux".to_string())]);
        assert!(out.contains("FOO=bar"));
        assert!(out.contains("# comment"));
        assert!(out.contains("BAZ=qux"));
    }

    /// A rotated OAuth grant round-trips through the REAL store reader under the exact key names
    /// `vike_ctrader::config::CtraderConfig::from_vars` reads — the end-to-end property that makes
    /// the store the rotation home rather than a second file.
    #[test]
    fn a_rotated_oauth_pair_round_trips_under_the_venue_key_names() {
        let existing = "CTRADER_CLIENT_ID=app\nCTRADER_DEMO_ACCESS_TOKEN=old\n\
                        CTRADER_DEMO_REFRESH_TOKEN=old-r\n";
        let updates = vec![
            ("CTRADER_DEMO_ACCESS_TOKEN".to_string(), "AT_rotated".to_string()),
            ("CTRADER_DEMO_REFRESH_TOKEN".to_string(), "RT_rotated".to_string()),
        ];
        let parsed = parse_dotenv(&upsert_env(existing, &updates));
        assert_eq!(parsed.get("CTRADER_DEMO_ACCESS_TOKEN").map(String::as_str), Some("AT_rotated"));
        assert_eq!(
            parsed.get("CTRADER_DEMO_REFRESH_TOKEN").map(String::as_str),
            Some("RT_rotated")
        );
        assert_eq!(parsed.get("CTRADER_CLIENT_ID").map(String::as_str), Some("app"));
    }

    #[test]
    fn save_credentials_atomic_write_round_trip() {
        let dir = std::env::temp_dir()
            .join(format!("vike_secrets_env_write_test_{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        let path = dir.join("secrets.env");

        // 1) file absent -> save creates it
        let updates = vec![("TESTVENUE_DEMO_API_KEY".to_string(), "test-key-abc".to_string())];
        save_credentials(&path, &updates).expect("save to absent file");
        let text = std::fs::read_to_string(&path).expect("read back");
        assert!(text.contains("TESTVENUE_DEMO_API_KEY=test-key-abc"));

        // 2) second save merges: existing key updated, a sibling key preserved
        std::fs::write(&path, format!("{text}SIBLING_KEY=keep-me\n")).unwrap();
        let updates2 = vec![("TESTVENUE_DEMO_API_KEY".to_string(), "test-key-xyz".to_string())];
        save_credentials(&path, &updates2).expect("save merge");
        let text2 = std::fs::read_to_string(&path).expect("read back 2");
        assert!(text2.contains("TESTVENUE_DEMO_API_KEY=test-key-xyz"));
        assert!(!text2.contains("test-key-abc"), "old value replaced");
        assert!(text2.contains("SIBLING_KEY=keep-me"), "sibling key preserved");

        // no leftover temp files
        let leftovers: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains(".tmp-"))
            .collect();
        assert!(leftovers.is_empty(), "temp file must be renamed away, not left behind");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
