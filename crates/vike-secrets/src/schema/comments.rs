//! The comment scan (spec 4.2 and 4.3): `FileComments` and `scan_comments`.

use std::collections::BTreeMap;

// ---------------------------------------------------------------------------------------------
// The comment scan — spec 4.2 and 4.3
// ---------------------------------------------------------------------------------------------

/// **What the credential FILE's comment lines carry** — the one thing `parse_dotenv` throws away.
///
/// ⚠ **This is a SECOND read of the credential file, and the property that makes it safe is that it
/// produces no LIVE value.** [`crate::db::migrate`]'s step 1 reads through `crate::store::resolve`
/// — *"the one parser, so the migration cannot disagree with the reader about what a line means"* —
/// and `crate::dotenv::parse_dotenv` skips every line beginning `#`. §4.2's two superseded
/// `ASTER_*` values and §4.3's provenance comments are therefore unreachable through it. This scan
/// looks ONLY at lines the one parser skipped, so the two can never disagree about anything that
/// reaches the credential map: this one contributes `superseded_at IS NOT NULL` rows and `notes`,
/// and no live row at all.
///
/// The file is opened READ-ONLY and is never written, moved or normalised — the rule that outranks
/// everything in `crate::db`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FileComments {
    /// `#KEY=VALUE` lines — §4.2's rollback copies, as `(name, value)`. The value is a CREDENTIAL
    /// and is never printed by anything in this crate.
    pub superseded: Vec<(String, String)>,
    /// Prose comment blocks, keyed by the `KEY=` line they sit immediately above — §4.3's
    /// provenance. **Nothing ever reads these back**; they exist so that retiring the file does not
    /// destroy what the operator wrote in it.
    pub notes: BTreeMap<String, String>,
    /// Prose comment lines that sit above no key (a file header, a trailing note, anything
    /// separated from the next key by a blank line). COUNTED and never attached, because a note
    /// attached to the wrong row is worse than one nobody kept.
    pub unattached_prose_lines: usize,
}

/// Read [`FileComments`] out of a credential file's TEXT.
///
/// The rules, stated because §11 step 5 says a line is *classified, never guessed at*:
///
/// * a line matching `#[ ]*KEY=VALUE` (optional spaces after the `#`, a legal env-var name, an `=`)
///   is a SUPERSEDED VALUE;
/// * any other `#` line is PROSE;
/// * a run of prose lines **immediately** above a `KEY=` line — no blank line and no superseded
///   line between — is that key's note. Anything else is unattached and COUNTED.
///
/// ⚠ A `#` inside a VALUE is not a comment: this scan only considers lines whose first
/// non-whitespace byte is `#`, which is exactly `crate::dotenv::parse_dotenv`'s own skip condition,
/// so the two agree about which lines are comments by construction.
#[must_use]
pub fn scan_comments(text: &str) -> FileComments {
    let mut out = FileComments::default();
    let mut block: Vec<&str> = Vec::new();
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() {
            out.unattached_prose_lines += block.len();
            block.clear();
            continue;
        }
        if let Some(body) = line.strip_prefix('#') {
            let body = body.trim_start();
            match split_assignment(body) {
                Some((name, value)) => {
                    // ⚠ **A `#KEY=VALUE` line ENDS the prose block**, and it did not until a
                    // reviewer measured §4.2's own file shape. The provenance line an operator
                    // writes above a rollback copy —
                    // `# superseded 2026-07-29 (kept for rollback)` — was left in the block, so it
                    // was carried PAST the rollback line and attached to the NEXT key in the file,
                    // whose note then claimed a rollback that was somebody else's. That is exactly
                    // what [`FileComments::unattached_prose_lines`] exists to prevent, and this
                    // function's own doc says a note is attached only to a key it sits
                    // IMMEDIATELY above.
                    //
                    // The block is COUNTED rather than attached because [`FileComments::superseded`]
                    // has no note column to put it in: a rollback copy carries the same `name` as
                    // the live row, so filing its provenance under that name would overwrite the
                    // live row's own note.
                    out.unattached_prose_lines += block.len();
                    block.clear();
                    out.superseded.push((name, value));
                }
                None => block.push(raw.trim_end()),
            }
            continue;
        }
        match split_assignment(line) {
            Some((name, _)) if !block.is_empty() => {
                out.notes.insert(name, block.join("\n"));
                block.clear();
            }
            _ => {
                out.unattached_prose_lines += block.len();
                block.clear();
            }
        }
    }
    out.unattached_prose_lines += block.len();
    out
}

/// `NAME=VALUE` with a legal credential-key name, or `None`.
///
/// The name grammar is deliberately the narrow one — an ASCII **UPPERCASE** letter or `_`, then
/// uppercase letters, digits and `_` — so a prose comment that merely contains an `=` is prose and
/// not a mangled superseded credential.
///
/// ⚠ **The uppercase requirement is a FENCE, and the example this doc used to give for it was
/// wrong.** It read: §4.3 quotes a real comment from the live store — *"polydata.live: key VALID
/// but FREE tier => data_access_days=0"* — and *"with a case-insensitive name grammar that tail
/// parses as an assignment and the line is read as a superseded value of a key called
/// `data_access_days`"*, described as MEASURED. It is not, and it cannot be: [`str::split_once`]
/// takes the FIRST `=`, which in that line is the one inside `=>`, so the candidate name is
/// `polydata.live: key VALID but FREE tier` — spaces, a dot and a colon — and no grammar that
/// admits an env-var name admits it, in any case. That line was never at risk and a
/// case-insensitive grammar would not have mis-read it.
///
/// What the rule actually fences is a comment whose WHOLE BODY is a lowercase assignment —
/// `# data_access_days=0`, the note left behind when somebody pastes the tail of that same sentence
/// onto its own line. THAT parses cleanly as `data_access_days = 0` under a case-insensitive
/// grammar and is read as a superseded credential. The `SupersededKeyIsNotInTheStore` safety net
/// catches it — there is no live row by that name, so nothing is written — but it catches it as a
/// REFUSAL an operator then has to read and dismiss on every run. Every credential key this store
/// holds is uppercase; a lowercase left-hand side is prose.
fn split_assignment(line: &str) -> Option<(String, String)> {
    let (name, value) = line.split_once('=')?;
    let name = name.trim();
    let mut bytes = name.bytes();
    let first = bytes.next()?;
    if !(first.is_ascii_uppercase() || first == b'_') {
        return None;
    }
    if !bytes.all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_') {
        return None;
    }
    Some((name.to_string(), value.trim().to_string()))
}
