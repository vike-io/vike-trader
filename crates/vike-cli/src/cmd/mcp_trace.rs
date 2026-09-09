//! **THE AGENT TRANSCRIPT** — an append-only record of what the AGENT ATTEMPTED through
//! `vike-cli mcp`: one line per `tools/call`, carrying the tool, whether it is a write, the
//! arguments with every secret-shaped value redacted, the verdict, and — for a gated write — the
//! preview token's IDENTITY.
//!
//! # The gap it closes
//!
//! `vike_tradehub::audit` writes one line per ACCEPTED control command, pinned above the file level
//! so an `Environment=` line cannot take it off disk. That is the NODE's record, and it is the
//! record of what SUCCEEDED. Nothing anywhere recorded what the agent TRIED: the tools it called,
//! the arguments it called them with, and — the half an operator actually wants after a bad night —
//! what was REFUSED and why. A refusal reaches the node's audit trail never, by construction: it
//! was refused before a byte left this process.
//!
//! # Why this is a SIBLING of `vike_model::change_journal` rather than a fifth record kind in it
//!
//! That module's own "A future sibling" section states the rule this file follows: a per-EVENT
//! stream does not belong in a per-CHANGE ledger, and the cure is a separate subdirectory with a
//! separate writer of the same SHAPE — "no shared file and therefore nothing to migrate". Three
//! concrete reasons it applies here rather than being a matter of taste:
//!
//!   * **Rate class.** That ledger records settings, credential and arming changes — tens of records
//!     a day. An agent session makes a tool call every few seconds. Mixing them buries the ledger,
//!     which is the exact argument that module makes for connectivity events.
//!   * **Size.** `vike_model::change_journal::MAX_RECORD_BYTES` is one page and every free-text cell
//!     is capped far below it. A `run_backtest` call carries a whole profile TOML AND a whole Rhai
//!     script as ARGUMENTS; recording those at that cap would refuse the record outright. This
//!     writer caps and truncates instead (see [`MAX_RECORD_BYTES`] and [`redact`]).
//!   * **What a torn record costs.** The change journal REFUSES an oversized record, because a
//!     truncated ledger line cannot be told from a forged one. Here the opposite holds: losing the
//!     fact that a write was ATTEMPTED is the worst outcome this file has, so an oversized record
//!     DEGRADES — the arguments are dropped and replaced by a marker naming what happened — and the
//!     row survives. [`McpTrace::line_for`] is that fallback, and
//!     `an_oversized_record_keeps_the_call_and_drops_the_arguments` pins it.
//!
//! What it does NOT re-decide is the storage medium:
//! `docs/decisions/0028-settings-stay-files-change-journal-is-jsonl.md` settled JSONL over an
//! embedded database for this workspace's ledgers, and the deciding property — multi-process
//! append, where an embedded store's connection-lifetime write lock turns a second writer into a
//! failure rather than a wait — applies here harder, not less: two agent sessions on one project
//! are the ordinary case. So the write is the same one: one buffer, one [`std::io::Write::write`]
//! on an append-opened descriptor, `sync_data`, all of it under an exclusive advisory lock on
//! [`TRACE_LOCK_FILE`]. The measurement behind that lock (a host bind mount silently losing most of
//! 100 concurrent appends) is in `vike_model::change_journal`'s module doc and is not re-derived
//! here.
//!
//! # ⚠ Redaction: one table, applied in TWO positions
//!
//! `vike_config::is_secret_key` is the workspace's ONE authority on whether a NAME is
//! credential-shaped, and this module spells no second table. It is applied to:
//!
//!   1. every object KEY in the arguments — a `secret`/`_api_key`/`_token`-shaped key never has its
//!      value recorded. That is also why `preview_token`'s value is redacted IN THE ARGUMENTS: the
//!      token identity the record carries is the one this SERVER minted or the call presented, in
//!      the record's own `token` field, never a string an agent handed us under that name;
//!   2. every SHOUTING_CASE identifier appearing inside a string VALUE — so
//!      `reason: "rotating BINANCE_LIVE_API_SECRET=…"` redacts the whole value. The shouting-case
//!      precondition is deliberate: `is_secret_key` matches the bare word `TOKEN`, and without it
//!      an ordinary English `reason` mentioning a token would be erased.
//!
//! ⚠ **The residual, stated rather than implied: a secret with no NAME anywhere near it is not
//! detectable.** A bare high-entropy string an agent chose to send is recorded (capped). What this
//! module guarantees is that no value the workspace's own naming conventions identify as a
//! credential reaches the file — and, structurally, that nothing here ever READS the credential
//! store or the environment: the arguments are the only input.
//!
//! # Retention
//!
//! Monthly files, pruned to [`DEFAULT_MAX_TRACE_FILES`] at startup by the one caller that arms this
//! writer. A count rather than an age, for the same reason `vike_model::change_journal::prune` uses
//! one: the file NAME sorts in calendar order, so the bound needs no clock and no `stat`.
//!
//! ⚠ **The bound is on the FILE POPULATION, not on a single month** — an agent hammering one
//! session grows the current month's file with nothing to stop it. That is the honest limit of a
//! monthly-file scheme, and it is one of the reasons this writer is OFF unless asked for
//! (`docs/decisions/0039-the-agent-transcript-is-opt-in-and-argument-redacted.md` carries the whole
//! disposition).

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::{Value, json};
use vike_model::change_journal::Proc;

/// The transcript sub-directory inside `vike_model::state_path::STATE_SUBDIR`:
/// `<project>/settings/state/agent`. A SIBLING of `vike_model::change_journal::CHANGES_SUBDIR`,
/// never a child of it — see the module doc.
pub(crate) const TRACE_SUBDIR: &str = "agent";
/// Every transcript file name starts with this: `mcp-2026-09.jsonl`.
pub(crate) const TRACE_FILE_PREFIX: &str = "mcp-";
/// …and ends with this. JSON Lines: one record per line, `jq`-able, appendable.
pub(crate) const TRACE_FILE_SUFFIX: &str = ".jsonl";
/// The sentinel every appender locks before it writes. Its BYTES are meaningless; its LIFETIME is
/// the lock, and the kernel releases it if the holder dies — the `journal_lock`/`live_lock` idiom,
/// verbatim. Its name carries no [`TRACE_FILE_PREFIX`], so [`McpTrace::prune`] can never delete the
/// file every writer is coordinating on.
pub(crate) const TRACE_LOCK_FILE: &str = "mcp.lock";
/// How many monthly transcript files [`McpTrace::prune`] keeps: two years.
///
/// Shorter than the change journal's ten on purpose. That ledger answers "when did this ceiling
/// change", a question asked years later; this one answers "what did the agent do last night", and
/// a transcript from two years ago describes a tool surface that no longer exists.
pub(crate) const DEFAULT_MAX_TRACE_FILES: usize = 24;
/// The hard ceiling on ONE serialized record, newline included.
///
/// Larger than the change journal's single page because the arguments here are agent-authored text
/// (a profile TOML, a Rhai script), and a cap that refused those would refuse exactly the calls a
/// reader wants. Over-cap DEGRADES rather than refusing — see [`McpTrace::line_for`].
pub(crate) const MAX_RECORD_BYTES: usize = 8192;
/// The cap on ONE recorded string, in BYTES. Bytes rather than chars: one char is up to four bytes
/// and up to six more after JSON escaping, so a char cap bounds nothing about the line.
pub(crate) const MAX_STRING_BYTES: usize = 512;
/// How many keys of one argument object are recorded; the rest are COUNTED, never dropped silently.
pub(crate) const MAX_OBJECT_KEYS: usize = 32;
/// How many elements of one argument array are recorded; the rest are counted.
pub(crate) const MAX_ARRAY_ITEMS: usize = 16;
/// How deep [`redact`] walks before it elides. An MCP client can send arbitrarily nested JSON, and
/// a recursive walk over client-shaped input needs a floor that is not the stack.
pub(crate) const MAX_DEPTH: usize = 6;
/// What a redacted value is replaced by.
pub(crate) const REDACTED: &str = "<redacted>";
/// The key an elision is reported under, so a reader never has to wonder whether a short record
/// means "the agent sent little" or "we recorded little".
pub(crate) const ELIDED: &str = "<elided>";
/// The `kind` discriminator every record carries, so a reader who one day finds this file beside
/// another JSONL stream can tell them apart on a prefix match.
pub(crate) const RECORD_KIND: &str = "mcp_tool_call";

/// Orders two records written in the same millisecond by one process. A TIEBREAKER, not a ledger
/// index: it starts at zero in every process, so a gap means "a record was refused or the process
/// restarted", never "a line was deleted". Same contract as `vike_model::change_journal`'s own
/// sequence.
static SEQ: AtomicU64 = AtomicU64::new(0);

/// What happened to the call.
///
/// [`Verdict::Refused`] is a first-class outcome rather than a flavour of error, and it is the whole
/// reason this file exists: "the agent tried to send an order and was refused" is exactly as much of
/// an audit fact as one that went through, and it is the fact NO other record in this workspace
/// holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Verdict {
    /// The tool ran and answered.
    Ok,
    /// A write tool returned its mandatory PREVIEW — nothing was sent. Distinct from [`Verdict::Ok`]
    /// because "the agent called submit_order" and "an order left this process" are different facts,
    /// and a transcript that spelled them the same would be unreadable exactly when it matters.
    Preview,
    /// A GATE said no — the profile, or the preview-token binding. Nothing was attempted.
    Refused,
    /// The tool was admitted and failed: a bad argument, an unreachable node, a panic.
    Error,
}

impl Verdict {
    /// The wire word. A `jq` filter's vocabulary, so it is spelled once, here.
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Verdict::Ok => "ok",
            Verdict::Preview => "preview",
            Verdict::Refused => "refused",
            Verdict::Error => "error",
        }
    }
}

/// The token identity was MINTED by this call's preview.
pub(crate) const TOKEN_MINTED: &str = "minted";
/// The token identity was PRESENTED by this call (a confirming write).
///
/// ⚠ Deliberately not "spent". Whether the store actually held it is the VERDICT's business beside
/// this field — a `refused` whose detail says "unknown or already used" is a token that was
/// presented and consumed nothing. Recording "spent" would claim something this record cannot see.
pub(crate) const TOKEN_PRESENTED: &str = "presented";

/// One `tools/call`, as the transcript sees it. Everything here is already known to the caller;
/// nothing in this module reads the environment, the clock or the credential store.
pub(crate) struct Call<'a> {
    /// The tool name as the client spelled it.
    pub(crate) tool: &'a str,
    /// Whether it goes through the mandatory-preview write gate (`super::mcp::is_write_tool`).
    pub(crate) write: bool,
    /// The scoping profile this server is running under — so a refusal in the file can be read
    /// without knowing how the process was launched.
    pub(crate) profile: &'a str,
    /// The arguments AS SENT. Redacted and capped on the way in; never stored raw.
    pub(crate) args: &'a Value,
    /// Ok / preview / refused / error.
    pub(crate) verdict: Verdict,
    /// The refusal's reason or the error's message. `None` for a clean call.
    pub(crate) detail: Option<String>,
    /// The preview token's IDENTITY — `pv-3`, a per-session counter. ⚠ Not a secret and not treated
    /// as one: `super::mcp::PendingPreviews`' own doc argues why a token encodes a SEQUENCE rather
    /// than an authorization (this is a stdio server; the only party that can send it a request
    /// already holds both ends of the pipe). It is single-use, expiring and command-bound, so the
    /// identity is the whole of it and there are no bytes to withhold.
    pub(crate) token: Option<String>,
    /// [`TOKEN_MINTED`] or [`TOKEN_PRESENTED`].
    pub(crate) token_role: Option<&'static str>,
}

/// Why an append did not happen. Returned rather than logged: the caller owns the `eprintln!`,
/// because `vike-cli mcp`'s STDOUT is a protocol and only the caller knows that.
#[derive(Debug)]
pub(crate) enum TraceError {
    /// The directory could not be created, or the file could not be opened, written or synced.
    Io(std::io::Error),
    /// The record did not fit even after its arguments were dropped. Unreachable through
    /// [`McpTrace::append`] (the degraded record is bounded by construction) and reported rather
    /// than unwrapped anyway.
    TooLarge {
        /// The serialized size, newline included.
        bytes: usize,
    },
    /// The single `write` did not deliver the whole buffer. Reported, never retried: a retry is a
    /// second chance to interleave with a concurrent appender.
    ShortWrite {
        /// Bytes the kernel accepted.
        wrote: usize,
        /// Bytes the record needed.
        expected: usize,
    },
    /// The record could not be serialized.
    Serialize(String),
    /// The append lock could not be TAKEN — not contention (a contended acquire WAITS), but a
    /// sentinel that could not be created or a platform that refused to lock at all.
    Lock(std::io::Error),
}

impl std::fmt::Display for TraceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TraceError::Io(e) => write!(f, "agent transcript I/O: {e}"),
            TraceError::TooLarge { bytes } => write!(
                f,
                "agent transcript record is {bytes} bytes, over the {MAX_RECORD_BYTES}-byte cap"
            ),
            TraceError::ShortWrite { wrote, expected } => {
                write!(f, "agent transcript short write: {wrote} of {expected} bytes")
            }
            TraceError::Serialize(e) => write!(f, "agent transcript serialize: {e}"),
            TraceError::Lock(e) => {
                write!(
                    f,
                    "agent transcript append lock ({TRACE_LOCK_FILE}) could not be taken: {e}"
                )
            }
        }
    }
}

impl std::error::Error for TraceError {}

/// The append-only agent transcript for one project.
///
/// Cheap to construct and cheap to hold: a directory and a process identity. It opens no file until
/// something is appended and holds no descriptor between appends, which is what lets two agent
/// sessions write the same month with nothing to coordinate.
#[derive(Debug, Clone)]
pub(crate) struct McpTrace {
    dir: PathBuf,
    process: Proc,
}

impl McpTrace {
    /// A transcript writing into `dir` directly — the form `--trace-dir` produces.
    pub(crate) fn new(dir: PathBuf) -> Self {
        Self { dir, process: Proc::current(env!("CARGO_PKG_VERSION")) }
    }

    /// A transcript at `<state_dir>/agent`, over the state directory the composition root ALREADY
    /// resolved. Taking the resolved directory rather than walking for one is the rule
    /// `vike_model::state_path::user_data_dir_beside` exists to enforce: a second walk is
    /// `$VIKE_SETTINGS_DIR`-blind and would answer with a different project than the settings came
    /// from.
    pub(crate) fn in_state_dir(state_dir: &Path) -> Self {
        Self::new(state_dir.join(TRACE_SUBDIR))
    }

    /// The directory this transcript writes into — printed to STDERR at startup, because an operator
    /// who asked for a record must be told where it went.
    pub(crate) fn dir(&self) -> &Path {
        &self.dir
    }

    /// The file `ts_ms` belongs in: `<dir>/mcp-YYYY-MM.jsonl`.
    pub(crate) fn file_for(&self, ts_ms: i64) -> PathBuf {
        self.dir.join(month_file_name(ts_ms))
    }

    /// Stamp `call` with `ts_ms` and the next [`SEQ`] value, producing the record that would be
    /// written. Consumes a sequence number, exactly as an append does.
    ///
    /// Nulls are written rather than omitted (`"detail":null`, `"token":null`): a `jq` filter over a
    /// year of records should not have to branch on a field's PRESENCE as well as on its value.
    pub(crate) fn record(&self, ts_ms: i64, call: &Call<'_>) -> Value {
        json!({
            "ts_ms": ts_ms,
            "seq": SEQ.fetch_add(1, Ordering::Relaxed),
            "kind": RECORD_KIND,
            "tool": call.tool,
            "write": call.write,
            "profile": call.profile,
            "verdict": call.verdict.as_str(),
            "detail": call.detail.as_deref().map(cap_string),
            "token": call.token,
            "token_role": call.token_role,
            "args": redact(call.args),
            "proc": serde_json::to_value(&self.process).unwrap_or(Value::Null),
        })
    }

    /// The exact line an append would write, newline included — and the DEGRADE step.
    ///
    /// ⚠ An over-cap record drops its ARGUMENTS and keeps the call. That is the deliberate inversion
    /// of `vike_model::change_journal`'s refusal, argued in the module doc: for a ledger of changes
    /// a truncated line is worse than a missing one, while for a record of ATTEMPTS the missing line
    /// IS the failure — an operator asking "did the agent try to send this" must never be answered
    /// by an absence that only means "the arguments were long".
    pub(crate) fn line_for(&self, ts_ms: i64, call: &Call<'_>) -> Result<String, TraceError> {
        let mut record = self.record(ts_ms, call);
        let line = line_of(&record)?;
        if line.len() <= MAX_RECORD_BYTES {
            return Ok(line);
        }
        record["args"] = json!({
            ELIDED: format!("arguments dropped: the record was {} bytes, over the cap", line.len())
        });
        let line = line_of(&record)?;
        if line.len() > MAX_RECORD_BYTES {
            return Err(TraceError::TooLarge { bytes: line.len() });
        }
        Ok(line)
    }

    /// Append ONE call, durably, and return the file it landed in.
    ///
    /// ⚠ The lock spans the open, the write and the sync, and nothing else — no caller code runs
    /// inside it, which is what makes a blocking acquire safe to wait on. `write` rather than
    /// `write_all`: the latter LOOPS on a short write, and a second write is a second chance to
    /// interleave.
    pub(crate) fn append(&self, ts_ms: i64, call: &Call<'_>) -> Result<PathBuf, TraceError> {
        let line = self.line_for(ts_ms, call)?;
        std::fs::create_dir_all(&self.dir).map_err(TraceError::Io)?;
        let path = self.file_for(ts_ms);
        let _lock = AppendLock::acquire(&self.dir)?;
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .map_err(TraceError::Io)?;
        let wrote = file.write(line.as_bytes()).map_err(TraceError::Io)?;
        if wrote != line.len() {
            return Err(TraceError::ShortWrite { wrote, expected: line.len() });
        }
        file.sync_data().map_err(TraceError::Io)?;
        Ok(path)
    }

    /// Prune to the newest `max_files` monthly files, oldest first.
    ///
    /// ⚠ Ordered by NAME, not by mtime — a `mcp-YYYY-MM.jsonl` name sorts in calendar order, while
    /// an mtime says only when a file was last touched. Anything in the directory that is not a
    /// well-formed monthly file is left ALONE, so a reader's own export cannot be deleted by
    /// housekeeping and cannot make the bound bite early. An absent or unreadable directory is not
    /// an error: a session that recorded nothing has no directory, and a startup must never fail
    /// over housekeeping.
    pub(crate) fn prune(&self, max_files: usize) {
        let Ok(entries) = std::fs::read_dir(&self.dir) else { return };
        let mut found: Vec<PathBuf> = entries
            .flatten()
            .filter(|e| {
                e.file_name().to_str().is_some_and(is_month_file_name) && e.path().is_file()
            })
            .map(|e| e.path())
            .collect();
        found.sort();
        let excess = found.len().saturating_sub(max_files);
        for path in found.into_iter().take(excess) {
            let _ = std::fs::remove_file(&path);
        }
    }
}

/// The held append lock: one exclusive advisory lock on `<dir>/`[`TRACE_LOCK_FILE`], released when
/// this value drops.
///
/// Private on purpose — it is not a capability a caller can hold across several appends, which is
/// the whole argument for a BLOCKING acquire.
#[derive(Debug)]
struct AppendLock(std::fs::File);

impl AppendLock {
    /// Take the exclusive lock on `dir`, creating the sentinel if absent. `dir` must already exist
    /// (the caller's `create_dir_all` runs first). BLOCKS while another writer holds it.
    fn acquire(dir: &Path) -> Result<Self, TraceError> {
        let path = dir.join(TRACE_LOCK_FILE);
        // `read(true).write(true)`, NOT `append(true)`: Windows refuses to lock an append-opened
        // handle. `truncate(false)`: the bytes are meaningless, and a file another process is
        // holding must never be rewritten. (The `journal_lock`/`live_lock` idiom, verbatim.)
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .map_err(TraceError::Lock)?;
        file.lock().map_err(TraceError::Lock)?;
        Ok(AppendLock(file))
    }
}

impl Drop for AppendLock {
    fn drop(&mut self) {
        // Closing the descriptor releases the lock on its own; unlocking first makes the release
        // explicit and ordered. The FILE stays on disk — see [`TRACE_LOCK_FILE`].
        let _ = self.0.unlock();
    }
}

/// Serialize ONE record to its wire line, the trailing `\n` included.
fn line_of(record: &Value) -> Result<String, TraceError> {
    let mut line =
        serde_json::to_string(record).map_err(|e| TraceError::Serialize(e.to_string()))?;
    line.push('\n');
    Ok(line)
}

/// `mcp-YYYY-MM.jsonl` for an epoch-ms instant, UTC.
///
/// Built on `vike_model::time::civil_from_days` — this workspace's one home for calendar math —
/// exactly as `vike_model::change_journal::month_file_name` is. `div_euclid` floors toward -inf, so
/// a pre-1970 instant lands in its own month rather than the next one.
fn month_file_name(ts_ms: i64) -> String {
    let (y, m, _) = vike_model::time::civil_from_days(ts_ms.div_euclid(86_400_000));
    format!("{TRACE_FILE_PREFIX}{y:04}-{m:02}{TRACE_FILE_SUFFIX}")
}

/// Is `name` a well-formed monthly transcript file — `mcp-YYYY-MM.jsonl`?
///
/// Strict on purpose: [`McpTrace::prune`] DELETES what this accepts, so anything it is unsure about
/// (a hand-made `mcp-old.jsonl`, an editor's `.bak`, the lock sentinel) falls outside and is left
/// alone.
fn is_month_file_name(name: &str) -> bool {
    let Some(rest) = name.strip_prefix(TRACE_FILE_PREFIX) else { return false };
    let Some(stamp) = rest.strip_suffix(TRACE_FILE_SUFFIX) else { return false };
    let bytes = stamp.as_bytes();
    bytes.len() == 7
        && bytes[..4].iter().all(u8::is_ascii_digit)
        && bytes[4] == b'-'
        && bytes[5..].iter().all(u8::is_ascii_digit)
}

/// Redact and cap one argument payload. PURE — the whole of this module's exposure to
/// agent-authored text goes through here.
pub(crate) fn redact(value: &Value) -> Value {
    redact_at(value, 0)
}

fn redact_at(value: &Value, depth: usize) -> Value {
    match value {
        Value::String(s) => Value::String(redact_string(s)),
        Value::Array(items) => {
            if depth >= MAX_DEPTH {
                return json!(format!("{ELIDED} nested deeper than {MAX_DEPTH}"));
            }
            let mut out: Vec<Value> =
                items.iter().take(MAX_ARRAY_ITEMS).map(|v| redact_at(v, depth + 1)).collect();
            if items.len() > MAX_ARRAY_ITEMS {
                out.push(json!(format!("{ELIDED} {} more items", items.len() - MAX_ARRAY_ITEMS)));
            }
            Value::Array(out)
        }
        Value::Object(map) => {
            if depth >= MAX_DEPTH {
                return json!(format!("{ELIDED} nested deeper than {MAX_DEPTH}"));
            }
            let mut out = serde_json::Map::new();
            for (key, v) in map.iter().take(MAX_OBJECT_KEYS) {
                // ⚠ THE KEY RULE. `vike_config::is_secret_key` is the workspace's one authority on a
                // credential-shaped NAME, and this module spells no second table — a venue or a knob
                // added there is redacted here by construction, and `preview_token` is caught by the
                // same `_TOKEN` shape that catches a node key.
                if vike_config::is_secret_key(key) {
                    out.insert(key.clone(), json!(REDACTED));
                } else {
                    out.insert(key.clone(), redact_at(v, depth + 1));
                }
            }
            if map.len() > MAX_OBJECT_KEYS {
                out.insert(
                    ELIDED.to_string(),
                    json!(format!("{} more keys", map.len() - MAX_OBJECT_KEYS)),
                );
            }
            Value::Object(out)
        }
        other => other.clone(),
    }
}

/// One string value: redacted whole if it NAMES a credential, capped otherwise.
fn redact_string(s: &str) -> String {
    if names_a_secret(s) { REDACTED.to_string() } else { cap_string(s) }
}

/// Does this string contain a SHOUTING_CASE identifier the workspace's own shapes call
/// credential-shaped?
///
/// ⚠ The shouting-case precondition is the whole difference between a rule and a nuisance.
/// `vike_config::is_secret_key` matches the bare word `TOKEN` (correctly — a setting spelled exactly
/// that must be redacted), so applied to prose it would erase every `reason` that mentions a token.
/// A credential leaking into an argument does not look like prose: it looks like
/// `BINANCE_LIVE_API_SECRET=…`, which is what this catches.
fn names_a_secret(s: &str) -> bool {
    s.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_')).any(|token| {
        token.contains('_')
            && token.chars().any(|c| c.is_ascii_uppercase())
            && token.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
            && vike_config::is_secret_key(token)
    })
}

/// Truncate to [`MAX_STRING_BYTES`] on a char boundary, saying how much was dropped.
///
/// The count is stated rather than a bare ellipsis: a reader deciding whether a recorded Rhai script
/// is the WHOLE script needs to know, and "the record shows what it shows" is the claim this file
/// has to be able to make.
fn cap_string(s: &str) -> String {
    if s.len() <= MAX_STRING_BYTES {
        return s.to_string();
    }
    let mut end = MAX_STRING_BYTES;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…(+{} bytes)", &s[..end], s.len() - end)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> tempfile::TempDir {
        tempfile::Builder::new().prefix(tag).tempdir().expect("a scratch directory")
    }

    fn call<'a>(tool: &'a str, args: &'a Value) -> Call<'a> {
        Call {
            tool,
            write: false,
            profile: "full",
            args,
            verdict: Verdict::Ok,
            detail: None,
            token: None,
            token_role: None,
        }
    }

    /// The record of a real appended call, parsed back — the shape every assertion below reads.
    fn appended(trace: &McpTrace, ts_ms: i64, c: &Call<'_>) -> Value {
        let path = trace.append(ts_ms, c).expect("the append succeeds");
        let text = std::fs::read_to_string(path).expect("the file is readable");
        let last = text.lines().next_back().expect("at least one line").to_string();
        serde_json::from_str(&last).expect("every line is one JSON object")
    }

    #[test]
    fn a_secret_shaped_key_never_has_its_value_recorded() {
        let dir = scratch("vike-mcptrace-key");
        let trace = McpTrace::new(dir.path().to_path_buf());
        let args = json!({
            "venue": "binance",
            "api_key": "PLANTED-KEY-VALUE",
            "nested": { "client_secret": "PLANTED-SECRET-VALUE" },
            "preview_token": "pv-9"
        });
        let record = appended(&trace, 1_767_225_600_000, &call("submit_order", &args));
        let line = record.to_string();
        assert!(
            !line.contains("PLANTED-KEY-VALUE"),
            "a planted key value reached the file: {line}"
        );
        assert!(!line.contains("PLANTED-SECRET-VALUE"), "a planted nested secret reached: {line}");
        assert_eq!(record["args"]["api_key"], json!(REDACTED));
        assert_eq!(record["args"]["nested"]["client_secret"], json!(REDACTED));
        // …and the same rule catches `preview_token`, whose identity the record carries in its own
        // field rather than copying a string the agent chose to send under that name.
        assert_eq!(record["args"]["preview_token"], json!(REDACTED));
        // Non-vacuity: an ordinary argument IS recorded, so the assertions above are about the
        // redaction rather than about an empty payload.
        assert_eq!(record["args"]["venue"], json!("binance"));
    }

    #[test]
    fn a_credential_named_inside_a_free_text_value_is_redacted_whole() {
        let dir = scratch("vike-mcptrace-value");
        let trace = McpTrace::new(dir.path().to_path_buf());
        let args = json!({ "reason": "rotating BINANCE_LIVE_API_SECRET=PLANTED-INLINE-VALUE" });
        let record = appended(&trace, 1_767_225_600_000, &call("cancel_order", &args));
        assert_eq!(record["args"]["reason"], json!(REDACTED));
        assert!(!record.to_string().contains("PLANTED-INLINE-VALUE"));
    }

    #[test]
    fn ordinary_prose_that_merely_says_token_survives() {
        // The nuisance half of the value rule: `is_secret_key` matches the bare word TOKEN, so a
        // rule without the shouting-case precondition would erase this reason — and the transcript
        // would be worthless for the thing it is FOR.
        assert!(!names_a_secret("the preview token expired, taking a fresh one"));
        assert!(names_a_secret("export OKX_DEMO_API_PASSPHRASE=hunter2"));
        assert!(names_a_secret("VIKE_TRADEHUB_CONTROL_KEY was wrong"));
        assert!(!names_a_secret("BTC_USDT on binance"));
    }

    #[test]
    fn a_long_argument_is_capped_and_says_how_much_it_dropped() {
        let script = "x".repeat(MAX_STRING_BYTES * 3);
        let capped = cap_string(&script);
        assert!(capped.len() < script.len());
        assert!(capped.contains("bytes)"), "the cap must state what it dropped: {capped}");
        assert_eq!(cap_string("short"), "short");
    }

    #[test]
    fn an_oversized_record_keeps_the_call_and_drops_the_arguments() {
        let dir = scratch("vike-mcptrace-huge");
        let trace = McpTrace::new(dir.path().to_path_buf());
        // Many keys, each under the string cap — so the DEGRADE path is reached through the RECORD
        // cap rather than through any single value's cap.
        let mut map = serde_json::Map::new();
        for i in 0..MAX_OBJECT_KEYS {
            map.insert(format!("k{i}"), json!("y".repeat(MAX_STRING_BYTES)));
        }
        let args = Value::Object(map);
        let record = appended(&trace, 1_767_225_600_000, &call("run_backtest", &args));
        assert_eq!(record["tool"], json!("run_backtest"), "the CALL survives");
        assert!(
            record["args"][ELIDED].is_string(),
            "the arguments are dropped with a marker, not silently: {record}"
        );
    }

    #[test]
    fn the_record_survives_a_restart_because_it_appends() {
        let dir = scratch("vike-mcptrace-restart");
        let ts = 1_767_225_600_000;
        // Two SEPARATE writers over one directory — the process-restart shape, and also the
        // two-concurrent-sessions shape.
        let first = McpTrace::new(dir.path().to_path_buf());
        let path = first.append(ts, &call("list_templates", &json!({}))).unwrap();
        drop(first);
        let second = McpTrace::new(dir.path().to_path_buf());
        second.append(ts, &call("node_snapshot", &json!({}))).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 2, "the second writer APPENDED rather than rewriting: {text}");
        assert!(lines[0].contains("list_templates"), "the first record is still there: {text}");
        assert!(lines[1].contains("node_snapshot"));
    }

    #[test]
    fn the_month_file_name_is_the_instant_it_belongs_to() {
        assert_eq!(month_file_name(1_767_225_600_000), "mcp-2026-01.jsonl");
        assert!(is_month_file_name("mcp-2026-01.jsonl"));
        assert!(!is_month_file_name(TRACE_LOCK_FILE), "the lock sentinel is never prunable");
        assert!(!is_month_file_name("mcp-old.jsonl"));
    }

    #[test]
    fn prune_keeps_the_newest_months_and_leaves_strangers_alone() {
        let dir = scratch("vike-mcptrace-prune");
        let trace = McpTrace::new(dir.path().to_path_buf());
        for month in 1..=4 {
            std::fs::write(dir.path().join(format!("mcp-2026-{month:02}.jsonl")), "{}\n").unwrap();
        }
        let stranger = dir.path().join("mcp-export.jsonl");
        std::fs::write(&stranger, "mine").unwrap();
        trace.prune(2);
        assert!(!dir.path().join("mcp-2026-01.jsonl").exists(), "the oldest goes first");
        assert!(!dir.path().join("mcp-2026-02.jsonl").exists());
        assert!(dir.path().join("mcp-2026-03.jsonl").exists());
        assert!(dir.path().join("mcp-2026-04.jsonl").exists());
        assert!(stranger.exists(), "housekeeping never deletes what it does not recognise");
    }

    #[test]
    fn a_verdict_and_a_token_role_reach_the_record() {
        let dir = scratch("vike-mcptrace-verdict");
        let trace = McpTrace::new(dir.path().to_path_buf());
        let args = json!({ "venue": "sim" });
        let c = Call {
            tool: "submit_order",
            write: true,
            profile: "read-only",
            args: &args,
            verdict: Verdict::Refused,
            detail: Some("not available under the `read-only` profile".to_string()),
            token: Some("pv-1".to_string()),
            token_role: Some(TOKEN_PRESENTED),
        };
        let record = appended(&trace, 1_767_225_600_000, &c);
        assert_eq!(record["verdict"], json!("refused"));
        assert_eq!(record["write"], json!(true));
        assert_eq!(record["profile"], json!("read-only"));
        assert_eq!(record["token"], json!("pv-1"));
        assert_eq!(record["token_role"], json!(TOKEN_PRESENTED));
        assert_eq!(record["kind"], json!(RECORD_KIND));
        assert!(record["detail"].as_str().unwrap().contains("read-only"));
        assert!(record["proc"]["pid"].is_number(), "the writing process is named: {record}");
        // The minted half of the pair is exercised end to end by `cmd/mcp.rs`'s transcript tests,
        // over the real preview gate — the only place that knows a token was MINTED.
        assert_eq!(TOKEN_MINTED, "minted");
    }
}
