//! Safe `KEY=value` UPSERT into the credential store — the one sanctioned way to write it.
//!
//! [`upsert_env`] is a pure string transform: replace matching `KEY=value` lines in place, append
//! new keys at the end, and preserve every other line (comments, blank lines, ordering) verbatim —
//! it never rewrites the file from scratch. [`save_credentials`] is the thin I/O wrapper that reads
//! the current store, upserts, and writes back atomically (temp file + rename in the same
//! directory, so a crash mid-write cannot truncate the store) — and re-establishes the three
//! properties a rename does not carry: the store's MODE, its SYMLINK if it is one, and the removal
//! of the temp file on a failure. That function's doc is where each is argued.
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
    if needs_quotes { format!("\"{value}\"") } else { value.to_string() }
}

/// Upsert `updates` (key, value pairs) into `existing` store text: for each key, replace **every**
/// existing `KEY=...` line's value in place (matched by trimmed key, ignoring surrounding
/// whitespace/comment lines), else append a new `KEY=value` line at the end. Every other line —
/// comments, blank lines, unrelated keys, and their relative order — is preserved verbatim.
/// Output always ends with a trailing newline.
///
/// # ⚠ EVERY matching line, not the first — and the asymmetry with the reader is why
///
/// This used to `remove` an update from the working set as soon as one line matched it, so a store
/// carrying the same key TWICE had its FIRST occurrence rewritten and every later one preserved
/// verbatim. [`crate::parse_dotenv`] is LAST-wins (it `insert`s per line), so the value every loader
/// in the workspace then read was the stale one further down the file: a rotation that reported
/// success, wrote a real change to disk, and changed nothing that is read. The daemon kept signing
/// with the old key.
///
/// Duplicates are not exotic — the store is a flat `KEY=VALUE` file people hand-edit, and
/// `vike-cli secrets template >> settings/secrets.env` (the append typo of the documented `>` form)
/// duplicates the entire grid in one keystroke.
///
/// Replacing all of them is the only outcome that makes the transform's own claim true, and it does
/// not widen the byte-preservation rule: the lines rewritten are exactly the lines naming a key the
/// caller ASKED to write, and a key nobody named is still untouched wherever and however often it
/// appears.
pub fn upsert_env(existing: &str, updates: &[(String, String)]) -> String {
    if updates.is_empty() {
        return existing.to_string();
    }

    // The updates are NOT consumed as they match — see the doc above. `matched` records which ones
    // found a home, and whatever never did gets appended at the end.
    let mut matched: Vec<bool> = vec![false; updates.len()];
    let mut out_lines: Vec<String> = Vec::new();

    for line in existing.lines() {
        let trimmed = line.trim();
        let existing_key = if trimmed.is_empty() || trimmed.starts_with('#') {
            None
        } else {
            trimmed.split_once('=').map(|(k, _)| k.trim())
        };

        let replaced = existing_key.and_then(|key| {
            let pos = updates.iter().position(|(uk, _)| uk == key)?;
            matched[pos] = true;
            Some(format!("{key}={}", format_value(&updates[pos].1)))
        });

        out_lines.push(replaced.unwrap_or_else(|| line.to_string()));
    }

    for (i, (key, value)) in updates.iter().enumerate() {
        if !matched[i] {
            out_lines.push(format!("{key}={}", format_value(value)));
        }
    }

    let mut out = out_lines.join("\n");
    out.push('\n');
    out
}

/// Refuse an update whose KEY or VALUE spans more than one line, before a byte is written.
///
/// ⚠ **This is a CORRECTNESS bound on the transform, not a tidiness rule.** The store's grammar is
/// one credential per LINE, so a value carrying a `\n` does not round-trip: [`format_value`] sees
/// the newline as whitespace and quotes the value, [`upsert_env`] joins the result with `\n`, and
/// the value's own newline becomes a physical line break in the file. [`crate::parse_dotenv`] then
/// reads the first half as a SILENTLY TRUNCATED credential and the second half as a whole new
/// `KEY=VALUE` line — so a multi-line value can inject a credential for a venue the operator never
/// configured, past a key name the caller validated. That was reachable from
/// `vike-cli secrets set KEY --from-env NAME` with a Vault- or Actions-injected variable, which is
/// the ordinary shape of a multi-line secret.
///
/// So the guarantee this module makes — "every other line preserved verbatim" — cannot hold over a
/// value that ADDS lines, and the honest answer is to refuse rather than to produce a store that
/// parses as something else. `\r` goes with it: a CRLF-exported variable would otherwise leave a
/// stray carriage return inside the value.
///
/// The error names the KEY and never the value: the value is the secret.
fn refuse_multiline(updates: &[(String, String)]) -> io::Result<()> {
    for (key, value) in updates {
        if key.contains(['\n', '\r']) || value.contains(['\n', '\r']) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "{key}: a credential is ONE line, and this value spans more than one — the \
                     store's grammar cannot represent it, and writing it would both truncate the \
                     credential and add a line that parses as a different key"
                ),
            ));
        }
    }
    Ok(())
}

/// The temp file [`save_credentials`] writes before the rename, removed on EVERY exit path until
/// [`TempFile::keep`] disarms it.
///
/// ⚠ Without this, a failed write or a failed rename left `.secrets.env.tmp-<pid>` beside the store
/// holding the ENTIRE store in plaintext, and nothing ever removed it. Both failures are real: a
/// full or read-only filesystem fails the write, and on Windows the rename fails with access-denied
/// while another process holds the destination open. The caller is told the write failed — and a
/// dotfile nobody lists keeps every credential next to the store for as long as the box lives.
struct TempFile<'a> {
    path: &'a Path,
    armed: bool,
}

impl<'a> TempFile<'a> {
    fn new(path: &'a Path) -> Self {
        Self { path, armed: true }
    }

    /// The rename succeeded, so the path now names the STORE — leave it alone.
    fn keep(mut self) {
        self.armed = false;
    }
}

impl Drop for TempFile<'_> {
    fn drop(&mut self) {
        if self.armed {
            // Best effort: this runs on a path that is already failing, and there is nothing
            // useful to do with a second error (and no logging dependency to report it through).
            let _ = std::fs::remove_file(self.path);
        }
    }
}

/// Read `path` (treating "file absent" as empty content), upsert `updates`, and write the result
/// back atomically: write to a temp file in the same directory, then rename over the store. The
/// rename is the atomic step — a crash before it leaves the original store untouched, a crash
/// after it lands the fully-written new content, and no partial/truncated store is ever
/// observable.
///
/// # ⚠ A temp+rename replaces the DESTINATION INODE, and three properties do not survive that
///
/// The rename is what makes the write atomic, and it is also what makes it a REPLACEMENT: the new
/// file's identity, mode and link status are the temp file's, not the store's. Each of the three is
/// re-established here, because the failure of each is silent and lands on the one file in this
/// workspace that must not degrade quietly.
///
/// 1. **The MODE.** `fs::write` creates a fresh file at `0o666 & !umask`, i.e. 0644 under the
///    standard 022 — so an operator who did the documented `chmod 600 settings/secrets.env` had
///    their store widened to world-readable plaintext by their next rotation, with the command
///    printing success and no finding. (`crate::permission_warning` reports an over-permissive
///    mode, but the CLI asks it BEFORE the write, so the exposure it exists to report is the one it
///    could not see.) The destination's mode is carried onto the temp file before the rename; a
///    store being CREATED gets 0600 explicitly rather than inheriting the umask.
/// 2. **The SYMLINK.** `read_to_string` follows a link; `rename` does not follow the destination —
///    it replaces the LINK. An operator keeping `settings/secrets.env` as a link into an encrypted
///    or root-owned volume (which `crate::permission_warning`'s own doc calls a legitimate setup,
///    and which the CLI prints a notice about) would find the link gone after one write, the whole
///    store materialised as plaintext inside the project directory, and the real file on the
///    protected volume still holding the PRE-rotation contents for anything reading it by its own
///    path. So the link is resolved FIRST and the write lands on its target.
/// 3. **The TEMP FILE.** See [`TempFile`] — a failed write or rename used to leave the entire store
///    in plaintext in a dotfile beside it, forever.
///
/// Both 1 and 2 are Unix-shaped. The mode carry is `#[cfg(unix)]` (the Windows equivalent is an ACL
/// question this crate has no business answering); the symlink resolution is not gated, because
/// Windows has symlinks too and following one there is just as correct.
pub fn save_credentials(path: &Path, updates: &[(String, String)]) -> io::Result<()> {
    refuse_multiline(updates)?;

    // The file the write must LAND on. A symlinked store is an indirection the operator built on
    // purpose, so it is followed rather than replaced; a link whose target cannot be resolved is an
    // error, because the alternative — renaming over the link — is exactly the destruction above.
    let target = match std::fs::symlink_metadata(path) {
        Ok(m) if m.file_type().is_symlink() => std::fs::canonicalize(path).map_err(|e| {
            io::Error::new(
                e.kind(),
                format!(
                    "{} is a symlink whose target could not be resolved ({e}) — refusing to \
                     replace the link with a regular file holding the credentials",
                    path.display()
                ),
            )
        })?,
        _ => path.to_path_buf(),
    };

    let existing = match std::fs::read_to_string(&target) {
        Ok(text) => text,
        Err(e) if e.kind() == io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e),
    };
    let updated = upsert_env(&existing, updates);

    let dir =
        target.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or_else(|| Path::new("."));
    let file_name = target.file_name().and_then(|n| n.to_str()).unwrap_or(".env");
    let tmp_path = dir.join(format!(".{file_name}.tmp-{}", std::process::id()));

    let tmp = TempFile::new(&tmp_path);
    std::fs::write(&tmp_path, updated.as_bytes())?;
    carry_mode(&target, &tmp_path)?;
    std::fs::rename(&tmp_path, &target)?;
    tmp.keep();
    Ok(())
}

/// Give `tmp` the mode `store` already has, or 0600 when the store is being created. Reason 1 on
/// [`save_credentials`].
#[cfg(unix)]
fn carry_mode(store: &Path, tmp: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mode = match std::fs::metadata(store) {
        Ok(m) => m.permissions().mode(),
        // Creating the store: 0600, never the umask's answer. A credential file this workspace
        // brought into existence must not start out readable by every user on the box.
        Err(e) if e.kind() == io::ErrorKind::NotFound => 0o600,
        Err(e) => return Err(e),
    };
    std::fs::set_permissions(tmp, std::fs::Permissions::from_mode(mode))
}

/// Windows has no mode to carry — the equivalent question is an ACL one, and this zero-dependency
/// crate has no business answering it. `crate::permission_warning` is `#[cfg(unix)]` for the same
/// reason.
#[cfg(not(unix))]
fn carry_mode(_store: &Path, _tmp: &Path) -> io::Result<()> {
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

    /// A per-test temp directory, named for the case so two running at once cannot collide (the
    /// pid alone does not separate them — these tests share a process).
    fn case_dir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir()
            .join(format!("vike_secrets_env_write_{tag}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    /// **A DUPLICATED key is replaced on EVERY line, because the reader is LAST-wins.**
    ///
    /// The transform used to rewrite the first occurrence and preserve the rest verbatim, so
    /// [`parse_dotenv`] — which `insert`s per line — kept returning the stale value further down the
    /// file. A rotation reported success, wrote a real change to disk, and every loader in the
    /// workspace went on reading the old credential. The round-trip through the REAL reader is the
    /// assertion that matters here; the line-level one is what says WHY it passes.
    #[test]
    fn a_duplicated_key_is_replaced_on_every_line() {
        let existing = "\
# two lines for one key — a hand-edit, or `secrets template >>` run twice
TESTVENUE_DEMO_API_KEY=first-old
UNRELATED_KEY=keep-me
TESTVENUE_DEMO_API_KEY=second-old
";
        let updates = vec![("TESTVENUE_DEMO_API_KEY".to_string(), "rotated-new".to_string())];
        let out = upsert_env(existing, &updates);

        assert!(!out.contains("first-old"), "the first occurrence must be rewritten: {out}");
        assert!(
            !out.contains("second-old"),
            "the LAST occurrence is the one the reader returns, so it must be rewritten too: {out}"
        );
        assert_eq!(
            out.matches("rotated-new").count(),
            2,
            "both lines carry the new value, and no line was added: {out}"
        );
        assert!(out.contains("UNRELATED_KEY=keep-me"), "an unnamed key is still untouched");
        assert_eq!(existing.lines().count(), out.lines().count(), "no line added or dropped");

        // …and the property that makes it matter: what the workspace READS is the new value.
        let parsed = parse_dotenv(&out);
        assert_eq!(parsed.get("TESTVENUE_DEMO_API_KEY").map(String::as_str), Some("rotated-new"));
    }

    /// **A MULTI-LINE value is refused, and nothing is written.**
    ///
    /// Not a tidiness rule: quoted and joined, the value's own newline becomes a physical line
    /// break, so the reader truncates the credential at it and reads the remainder as a SECOND
    /// `KEY=VALUE` — a credential for a venue nobody configured, injected past a validated key name.
    /// Reachable from any Vault- or Actions-injected variable, which is the ordinary shape of a
    /// multi-line secret.
    #[test]
    fn a_multiline_value_is_refused_and_writes_nothing() {
        let dir = case_dir("multiline");
        let path = dir.join("secrets.env");
        std::fs::write(&path, "TESTVENUE_DEMO_API_KEY=original\n").unwrap();

        for bad in ["abc\nTESTVENUE_DEMO_API_SECRET=injected", "trailing\n", "cr\r"] {
            let err =
                save_credentials(&path, &[("TESTVENUE_DEMO_API_KEY".to_string(), bad.to_string())])
                    .expect_err("a value spanning lines must be refused");
            assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
            assert!(err.to_string().contains("ONE line"), "{err}");
            // The refusal names the KEY and never the value — the value is the secret.
            assert!(!err.to_string().contains("injected"), "the error ECHOED the value: {err}");
        }
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "TESTVENUE_DEMO_API_KEY=original\n",
            "a refused write must leave the store byte-identical"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// **The store's MODE survives the write.** The rename replaces the inode, so without the mode
    /// carry a `chmod 600` store came back at the umask default (0644) and every user on the box
    /// could read the live signing keys — silently, at the moment of a rotation that printed
    /// success. A store this function CREATES starts at 0600 rather than inheriting the umask.
    #[cfg(unix)]
    #[test]
    fn an_upsert_preserves_the_stores_mode_and_creates_at_0600() {
        use std::os::unix::fs::PermissionsExt;

        let dir = case_dir("mode");
        let path = dir.join("secrets.env");
        std::fs::write(&path, "TESTVENUE_DEMO_API_KEY=old\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();

        save_credentials(&path, &[("TESTVENUE_DEMO_API_KEY".to_string(), "new".to_string())])
            .unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "the write widened a 0600 store to {mode:o}");

        // A mode the operator chose is CARRIED, not corrected: this function preserves, it does not
        // police. `crate::permission_warning` is what reports an over-permissive store.
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        save_credentials(&path, &[("TESTVENUE_DEMO_API_KEY".to_string(), "newer".to_string())])
            .unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o644, "the write changed a 0644 store's mode to {mode:o}");

        // …and a store brought into existence here does not inherit the umask.
        let fresh = dir.join("fresh.env");
        save_credentials(&fresh, &[("TESTVENUE_DEMO_API_KEY".to_string(), "v".to_string())])
            .unwrap();
        let mode = std::fs::metadata(&fresh).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "a created store must be 0600, not the umask's {mode:o}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// **A SYMLINKED store is still a symlink afterwards, and the TARGET is what changed.**
    ///
    /// `rename` does not follow the destination, so the write used to replace the link with a
    /// regular file holding the whole store in plaintext — inside the project directory the link
    /// existed to keep credentials out of — while the real file kept its pre-rotation contents for
    /// anything reading it by its own path.
    #[cfg(unix)]
    #[test]
    fn an_upsert_writes_through_a_symlinked_store_and_keeps_the_link() {
        let dir = case_dir("symlink");
        let real = dir.join("vault-secrets.env");
        let link = dir.join("secrets.env");
        std::fs::write(&real, "TESTVENUE_DEMO_API_KEY=old\n").unwrap();
        std::os::unix::fs::symlink(&real, &link).unwrap();

        save_credentials(&link, &[("TESTVENUE_DEMO_API_KEY".to_string(), "rotated".to_string())])
            .unwrap();

        assert!(
            std::fs::symlink_metadata(&link).unwrap().file_type().is_symlink(),
            "the write replaced the link with a regular file"
        );
        assert!(
            std::fs::read_to_string(&real).unwrap().contains("TESTVENUE_DEMO_API_KEY=rotated"),
            "the rotation must land on the link's TARGET"
        );
        // No stray regular file left in the directory the link points OUT of.
        let names: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names.len(), 2, "unexpected files beside the link: {names:?}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// **The temp file is removed on a failure and KEPT on a success** — [`TempFile`], asserted at
    /// its own seam.
    ///
    /// ⚠ It is tested HERE rather than through a failing [`save_credentials`], and that is a
    /// measured choice rather than a shortcut. The failure that leaves a temp file behind is a
    /// successful write followed by a FAILED RENAME, and no portable way to provoke exactly that
    /// exists: every candidate destination that makes `rename` fail (a directory, a path through a
    /// regular file) makes the earlier `read_to_string` or `fs::write` fail FIRST, so the temp file
    /// is never created and the assertion passes without the guard existing at all. A test that
    /// green-lights a deleted guard is worse than one that names its seam.
    ///
    /// The first version of this test did exactly that — it pointed the store under a regular file
    /// and then read the parent directory for `.tmp-` leftovers, where the temp path could not have
    /// appeared in the first place. It is recorded because the shape is the trap, not the typo.
    #[test]
    fn the_temp_guard_removes_on_failure_and_keeps_on_success() {
        let dir = case_dir("tmp_guard");

        // Dropped without `keep` — the failure path. The file must be gone.
        let doomed = dir.join(".secrets.env.tmp-doomed");
        std::fs::write(&doomed, "the whole store in plaintext\n").unwrap();
        {
            let _guard = TempFile::new(&doomed);
        }
        assert!(!doomed.exists(), "a failed write left the store in a temp file");

        // Disarmed by `keep` — the success path, where the path now names the STORE itself and
        // removing it would delete the credentials this function had just written.
        let kept = dir.join(".secrets.env.tmp-kept");
        std::fs::write(&kept, "renamed into place\n").unwrap();
        {
            let guard = TempFile::new(&kept);
            guard.keep();
        }
        assert!(kept.exists(), "`keep` must disarm the guard — this is the store after the rename");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
