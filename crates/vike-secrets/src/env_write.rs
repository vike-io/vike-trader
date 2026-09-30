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
//! It lives in `vike-secrets` — the crate that already OWNS the store and NAMES NOTHING ABOVE THE
//! VOCABULARY FLOOR (rank 10), which is tier 15's machine-checked rule — so every
//! layer can reach it: the GUI editor (`vike-connections`, which calls these two functions by this
//! crate's name rather than keeping a second copy) and a venue bridge persisting a refreshed
//! OAuth grant (`vike_ctrader::token_store`) are on opposite sides of the layer graph and must not
//! drag each other in.
//!
//! ⚠ **That clause has expired TWICE and is corrected rather than deleted both times.** It read
//! "and has ZERO dependencies" until 2026-09-13, when
//! `docs/decisions/0054-settings-move-into-one-database.md` put `rusqlite` in the manifest; it then
//! read "and declares no `vike-*` dependency" until
//! `docs/decisions/0072-vike-secrets-takes-one-vike-edge-and-is-not-split.md` (accepted
//! 2026-09-20) admitted `vike-model`. What this argument actually needs is the ALTITUDE — that
//! nothing reaching this writer has to climb — and both amendments left that intact, which is why
//! the corrected sentence states the RANK rather than the edge: `crates/vike-ops/tests/layer_gate.rs`'s
//! `every_tier_15_crate_names_nothing_above_the_vocabulary` is the authority, and the tier's own
//! description was repaired to that wording on 2026-09-23 with this crate as the occupant that
//! forced it. A verbatim second implementation would be two things to keep in step about
//! one file's byte-level preservation.
//!
//! Matches [`crate::parse_dotenv`]'s tolerant parsing (trimmed line, first `=` splits key/value,
//! one layer of surrounding `"`/`'` stripped) so anything written here round-trips through the
//! reader.
//!
//! SECURITY: this module never logs anything at all — it has no logging dependency and cannot
//! acquire one without adding a SECOND external crate to a closure crate that carries exactly one
//! (⚠ this read "breaking the crate's zero-dependency property" until 2026-09-13; that property is
//! gone, the no-logging one is not, and it rests on the dependency FLOOR rather than on emptiness).
//! Callers must log only
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
pub(crate) fn refuse_multiline(updates: &[(String, String)]) -> io::Result<()> {
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

/// Windows has no mode to carry — the equivalent question is an ACL one, and a crate whose whole
/// job is *which store answers and what its bytes say* has no business answering it.
/// `crate::permission_warning` is `#[cfg(unix)]` for the same reason. (⚠ This said
/// "this no-`vike-*`-dependency crate", which
/// `docs/decisions/0072-vike-secrets-takes-one-vike-edge-and-is-not-split.md` made false and which
/// was never the reason anyway: an ACL query is out of scope here at any dependency count.)
#[cfg(not(unix))]
fn carry_mode(_store: &Path, _tmp: &Path) -> io::Result<()> {
    Ok(())
}

#[path = "env_write_tests.rs"]
#[cfg(test)]
mod env_write_tests;
