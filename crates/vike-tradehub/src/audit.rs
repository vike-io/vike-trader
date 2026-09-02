//! `audit` — the control-command audit trail (headless two-layer plan, Layer 2, PR-12).
//!
//! This is a SECURITY-SENSITIVE network write path: a remote `Scope::Control` peer can place and
//! cancel REAL orders on a live daemon, so every ACCEPTED control command (one the core's
//! [`vike_core::CommandSink`] took) must leave a durable record. v1 keeps that deliberately minimal:
//! ONE structured `tracing::info!` event per accepted command. A DENIED or malformed command is
//! already surfaced by the server's `Response::Error` / `Response::AuthDenied` reply and the
//! connection log; this records only what the core actually accepted, keyed by peer + verb +
//! client-order-id.
//!
//! # ⚠ A rolling log line is NOT a durable record — and the SETTINGS half no longer relies on one
//!
//! This module's original doc claimed the `tracing::info!` line "IS the durable audit record"
//! because [`vike_log`] writes a daily-rolling JSON `trace` file. That was measured and it is
//! false, twice over:
//!
//! * `vike_log::DEFAULT_MAX_LOG_FILES` is the retention, `vike_log::LogConfig`'s `Default` uses it,
//!   and no binary overrides it. Rotation is daily, so a change from a handful of days ago has
//!   already been deleted.
//! * `deploy/vike-tradehub.service` sets `Environment=VIKE_LOG_FILE_LEVEL=warn`, and the
//!   environment beats `LogConfig`'s `file_level`. These records are emitted at `info`. Measured on
//!   the live the CI box box: that daemon's 23 MB log file held **53,160 ERROR lines, 1,928 WARN lines
//!   and ZERO INFO lines.** The record never reached disk there at all.
//!
//! So [`record_settings_write`] now ALSO appends to the append-only CHANGE JOURNAL
//! ([`vike_model::change_journal`], `<project>/settings/state/changes/changes-YYYY-MM.jsonl`),
//! which nothing rotates away and no log level can silence. **The `tracing` line stays** — it is
//! the console/journald copy, and it is the right thing for an operator tailing a unit. The two are
//! built from the SAME sanitized, redacted cells, so they can never disagree about what changed.
//!
//! ⚠ [`record`] — the ORDER-command trail — is deliberately NOT journalled here. Its rate class is
//! different (a command per order, not a change per operator action), and the core already carries
//! its own durable order journal. Mixing the two would bury the change ledger the moment a maker
//! strategy started quoting.
//!
//! ## The rationale (proto v4) and why it is SANITIZED
//!
//! The record answers *that* a command was accepted; `reason` answers *why*. It is an optional free
//! text an operator (or an agent) supplies alongside the command
//! ([`vike_tradehub_client::proto::Request::Command`]'s `reason` field), so an incident review reads
//! "peer 127.0.0.1 submitted coid a3f10007 — *flattening ahead of the CPI print*" instead of the
//! coid alone.
//!
//! That text is **remote input landing verbatim in a structured JSON log line**, which makes it a
//! LOG-INJECTION vector: a newline followed by a forged `{"kind":...}` object would append entries
//! that read exactly like genuine audit records, and an unbounded string would let one command
//! write an arbitrarily large line into the trace file. [`sanitize_reason`] is therefore mandatory
//! at the call site, not advisory — it strips every Unicode control character (so nothing can end
//! the line or embed a NUL) and caps the length ([`MAX_REASON_CHARS`], counted in CHARS so a
//! multi-byte sequence is never split). It is a pure function with its own unit tests below.
//!
//! The rationale is audit-only. It never reaches `OrderRequest`, the core fold, the journal, or any
//! venue — `server.rs` lowers the command and passes the reason here, nowhere else.

use std::net::SocketAddr;

use vike_model::change_journal::{Actor, Change, ChangeJournal, Outcome};

/// The recorded-rationale cap, in CHARS (not bytes): a bound on how much remote free text one
/// accepted command can write into the audit line. 512 is far more than a real "why" needs and far
/// less than a log-flooding payload wants.
pub const MAX_REASON_CHARS: usize = 512;

/// Record ONE accepted control command to the audit trail (the vike-log JSON trace file). `kind` is
/// the command verb (`"submit"` / `"cancel"` / `"modify"` / `"mass_cancel"` / `"flatten"` /
/// `"market_exit"` / `"set_trading_state"` / `"update_params"` — `server::command_kind` is the
/// authority); `coid` is the client-order-id it targeted (empty for the
/// account-wide verbs, `update_params` included). `reason` is the caller-supplied rationale — it MUST already have been run
/// through [`sanitize_reason`] (the server's Command arm does exactly that); `None` records no
/// `reason` field at all, so a command with no rationale logs byte-identically to pre-v4. Emitted at
/// `info` so it always reaches the file layer. `peer` is optional because `TcpStream::peer_addr` can
/// fail — the whole server module carries it as `Option`.
pub fn record(peer: Option<SocketAddr>, kind: &str, coid: &str, reason: Option<&str>) {
    tracing::info!(?peer, kind, coid, reason, "vike-tradehub control: command accepted");
}

/// Record ONE accepted SETTINGS WRITE (`WireCommand::SetSetting`, split-plane REQ-7) — the
/// [`record`] twin for the one control command that lands on DISK rather than in the core (no
/// coid to key by). The record carries the file, the full dotted key, and the **old → new
/// values** — a `None` `old` (recorded as an absent field, the `reason` idiom) means the key was
/// not set in the file before — because a POLICY edit's whole audit story is what the ceiling
/// changed FROM and TO, and a record without both would leave an incident review diffing file
/// history to learn it.
///
/// `old`/`new` are REMOTE-INFLUENCED text (the new value is the peer's own string; the old value
/// came off a disk file a peer may have written earlier) landing in the structured JSON line, so
/// both pass the same control-strip + length-cap discipline as the rationale
/// ([`sanitize_value`]). A credential-shaped KEY's values are additionally redacted outright
/// (`vike_config::is_secret_key`) — pure insurance today, since no settings key is
/// credential-shaped (`vike_config::show`'s construction-time pin), but this line must not be
/// the one surface that leaks if that ever changes. `reason` follows [`record`]'s contract
/// exactly (already sanitized by the caller).
///
/// ⚠ **TWO destinations, one set of cells.** The `tracing::info!` line is the console/journald
/// copy; the append-only CHANGE JOURNAL ([`SettingsWriteAudit::journal`]) is the durable one, and
/// the module doc carries the measurement that made the second necessary. Both are built from the
/// same sanitized-and-redacted `old`/`new`, so no redaction can apply to one and not the other.
pub fn record_settings_write(audit: SettingsWriteAudit<'_>) {
    let SettingsWriteAudit { peer, key_id, journal, now_ms, write, reason, outcome } = audit;
    let (file, key) = (write.file, write.key.as_str());
    let (old, new) = if vike_config::is_secret_key(key) {
        (
            write.old_value.as_ref().map(|_| vike_config::redact::REDACTED.to_string()),
            vike_config::redact::REDACTED.to_string(),
        )
    } else {
        (write.old_value.as_deref().map(sanitize_value), sanitize_value(&write.new_value))
    };
    tracing::info!(
        ?peer,
        kind = "set_setting",
        file,
        key,
        old = old.as_deref(),
        new = new.as_str(),
        reason,
        "vike-tradehub control: settings write accepted"
    );

    // …and the DURABLE half. Built from the SAME `old`/`new` locals the line above used, so the
    // console copy and the ledger can never disagree about what changed — including about a
    // redaction. A journal-less surface (a server booted with no settings directory: the walk found
    // no project) keeps the pre-journal behaviour exactly.
    let Some(journal) = journal else { return };
    let change = Change::set_setting(
        outcome,
        // `scope` is `"control"` unconditionally and that is a fact rather than a default: the
        // server's `Request::Command` arm refuses anything but `Scope::Control` before
        // `crate::server::accept_command` is reached, and the Telegram surface — the only other
        // caller — serves no settings source and refuses the verb inside. `key_id` is the
        // fingerprint of the key that actually authenticated
        // (`vike_tradehub_client::auth::NodeKeys`' `key_id`, resolved once per connection): the
        // daemon authenticates a KEY, so that is the truest answer available to "who", and it is
        // still `None` — an ABSENT field, never an invented id — for a surface that authenticated
        // no key at all.
        Actor::wire(peer.map(|p| p.to_string()).as_deref(), Some("control"), key_id),
        file,
        key,
        old.as_deref(),
        &new,
    )
    .with_reason(reason);
    if let Err(e) = journal.append(now_ms, &change) {
        // A journal failure must not fail the write that already landed on disk — the setting IS
        // changed, and refusing to acknowledge it would be a worse lie than a missing record. It is
        // logged at `error` deliberately: that is the ONE level `deploy/vike-tradehub.service`'s
        // `VIKE_LOG_FILE_LEVEL=warn` still lets through, so the failure of the durable channel is
        // itself durable.
        tracing::error!(
            error = %e,
            dir = %journal.dir().display(),
            file,
            key,
            "vike-tradehub control: settings write NOT recorded to the change journal"
        );
    }
}

/// ONE accepted settings write and everywhere it is recorded — [`record_settings_write`]'s whole
/// argument list.
///
/// A struct rather than seven positional parameters, for two reasons that both bite: clippy's
/// `too_many_arguments` fires at eight and this path is under `-D warnings`, and — the one that
/// matters — `file`/`key`/`old`/`new` as four adjacent `&str`s is a call site where a transposition
/// compiles. `write` is taken WHOLE, as [`vike_config::write::SettingsWrite`], whose own doc calls
/// itself "the audit record's raw material": the four cells travel together, in the order the type
/// declares, and cannot be re-ordered at a call site at all.
pub struct SettingsWriteAudit<'a> {
    /// The TCP peer, or `None` for a surface that has none (`TcpStream::peer_addr` can also fail —
    /// the whole server module carries it as an `Option` for that reason).
    pub peer: Option<SocketAddr>,
    /// WHO — the stable, non-secret fingerprint of the key whose mac authenticated this connection
    /// (`vike_tradehub_client::auth::NodeKeys`' `key_id`), resolved once per connection by
    /// `crate::server::handle_connection` and threaded through
    /// [`crate::server::accept_command`].
    ///
    /// ⚠ **A key, not a person — and that is the record, not a shortfall.** There are no human
    /// accounts in this system, and `vike_model::change_journal::Actor`'s doc is explicit that
    /// inventing one would be worse than admitting the channel cannot name one. The peer ADDRESS
    /// is not an identity (it changes with the tunnel and is trivially shared); the credential is.
    ///
    /// `None` for a surface that authenticates no key — the Telegram channel, and any caller
    /// predating this field. It records an ABSENT `key_id`, never an empty or placeholder one:
    /// the fingerprint of a key that does not exist would be a lie in an append-only ledger.
    pub key_id: Option<&'a str>,
    /// Where the DURABLE record goes. `None` keeps the pre-journal behaviour (the `tracing` line
    /// alone) — the honest state for a server whose boot walk found no project directory.
    pub journal: Option<&'a ChangeJournal>,
    /// The instant to stamp, supplied by the caller: [`vike_model::change_journal`] reads no clock
    /// (it is inside `crates/vike-ops/tests/clock_pin.rs`'s scope), so the timestamp is a
    /// parameter all the way down.
    pub now_ms: i64,
    /// What the write did, verbatim from the writer.
    pub write: &'a vike_config::write::SettingsWrite,
    /// The operator's rationale — already through [`sanitize_reason`] at the call site.
    pub reason: Option<&'a str>,
    /// Whether the running node picked the change up, or keeps its boot-time value until restarted
    /// — the same bit `crate::server::Accepted`'s `SettingsWritten { restart_required }` carries to
    /// the peer. Recording it is what makes the journal answer "was that ceiling ACTUALLY armed"
    /// rather than only "was it written".
    pub outcome: Outcome,
}

/// [`sanitize_reason`]'s discipline for a VALUE cell: strip every Unicode control character and
/// cap at [`MAX_REASON_CHARS`] chars — but, unlike a rationale, an EMPTY value stays an empty
/// string rather than becoming `None`: "the key was set to nothing" is a real old/new value, not
/// an absent one (absence is [`record_settings_write`]'s `old: None`, meaning "not set at all").
fn sanitize_value(raw: &str) -> String {
    raw.chars().filter(|c| !c.is_control()).take(MAX_REASON_CHARS).collect()
}

/// Make a remote-supplied rationale SAFE to write into the structured audit line — the pure gate
/// [`record`]'s callers must pass every `reason` through. In order:
///
/// 1. **Strip every Unicode control character** (`char::is_control` — C0 `\n` `\r` `\t` `\0` … and
///    C1). This is the load-bearing step: without it a newline plus a forged JSON object could
///    fabricate audit entries in the trace file.
/// 2. **Cap at [`MAX_REASON_CHARS`] chars** of what survives. Counted in CHARS, so a multi-byte
///    UTF-8 sequence is never split (a byte-wise truncation could emit invalid UTF-8).
/// 3. **Trim surrounding whitespace**, then map an EMPTY result to `None` rather than `Some("")` —
///    a rationale of nothing is no rationale, and an empty field in the audit line is noise that
///    reads like a supplied-but-blank explanation.
///
/// A `None` input is `None` out (no rationale given). Note the ONE residual, documented rather than
/// silently carried: non-control text is preserved verbatim, so display-layer trickery that needs no
/// control characters (e.g. bidirectional-override formatting characters) survives. That cannot
/// forge a record — the line, its field set, and its framing are intact — it can only render oddly
/// in a terminal.
pub fn sanitize_reason(raw: Option<&str>) -> Option<String> {
    let cleaned: String = raw?.chars().filter(|c| !c.is_control()).take(MAX_REASON_CHARS).collect();
    let trimmed = cleaned.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_reason_passes_ordinary_text_through() {
        assert_eq!(
            sanitize_reason(Some("flattening ahead of the CPI print")).as_deref(),
            Some("flattening ahead of the CPI print")
        );
        // Non-ASCII, non-control text is NOT mangled — only control characters are removed.
        assert_eq!(
            sanitize_reason(Some("écart trop large — 幅が広い")).as_deref(),
            Some("écart trop large — 幅が広い")
        );
        assert_eq!(sanitize_reason(None), None);
    }

    #[test]
    fn sanitize_reason_strips_every_control_char() {
        // THE reason this function exists: a newline + a forged audit-shaped JSON object must not
        // survive into the structured trace line.
        let injected = "flat\n{\"kind\":\"forged\",\"coid\":\"evil\"}\r\nmore";
        let out = sanitize_reason(Some(injected)).unwrap();
        assert_eq!(out, "flat{\"kind\":\"forged\",\"coid\":\"evil\"}more");
        assert!(!out.contains('\n') && !out.contains('\r'), "no line terminator survives: {out:?}");
        // Tabs, NUL and the C1 block go the same way.
        assert_eq!(sanitize_reason(Some("a\tb\u{0}c\u{7f}d\u{85}e")).as_deref(), Some("abcde"));
        // …and nothing but control characters is nothing at all.
        assert_eq!(sanitize_reason(Some("\n\r\t\u{0}")), None);
    }

    #[test]
    fn sanitize_reason_truncates_at_the_cap() {
        let long = "x".repeat(MAX_REASON_CHARS + 88);
        let out = sanitize_reason(Some(&long)).unwrap();
        assert_eq!(out.chars().count(), MAX_REASON_CHARS);
        assert_eq!(out, "x".repeat(MAX_REASON_CHARS));
        // Exactly at the cap is NOT truncated.
        let exact = "y".repeat(MAX_REASON_CHARS);
        assert_eq!(sanitize_reason(Some(&exact)).as_deref(), Some(exact.as_str()));
        // The cap counts what SURVIVES the strip, not the raw input: 600 payload chars interleaved
        // with newlines still yields 512 payload chars, not 512 minus the newlines.
        let noisy = "z\n".repeat(600);
        assert_eq!(sanitize_reason(Some(&noisy)).unwrap().chars().count(), MAX_REASON_CHARS);
    }

    #[test]
    fn sanitize_reason_never_splits_a_multibyte_sequence() {
        // '☃' is 3 bytes: a BYTE-wise truncation at 512 would land mid-sequence and produce invalid
        // UTF-8. Char-wise truncation keeps exactly 512 whole chars = 1536 bytes.
        let snow = "☃";
        assert_eq!(snow.len(), 3, "the fixture must actually be multi-byte");

        let exactly_cap = snow.repeat(MAX_REASON_CHARS);
        let out = sanitize_reason(Some(&exactly_cap)).unwrap();
        assert_eq!(out, exactly_cap, "a 512-CHAR multibyte string is untouched");
        assert_eq!(out.chars().count(), MAX_REASON_CHARS);
        assert_eq!(out.len(), MAX_REASON_CHARS * 3, "…and every char survived whole");

        let over = snow.repeat(MAX_REASON_CHARS + 40);
        let out = sanitize_reason(Some(&over)).unwrap();
        assert_eq!(out.chars().count(), MAX_REASON_CHARS);
        assert_eq!(out.len(), MAX_REASON_CHARS * 3);
        assert!(out.chars().all(|c| c == '☃'), "no replacement char / partial sequence: {out:?}");
    }

    #[test]
    fn sanitize_reason_empty_result_is_none_not_some_empty() {
        // An empty or whitespace-only rationale is NO rationale — never `Some("")`, which would log
        // an empty `reason` field that reads like a supplied-but-blank explanation.
        for raw in ["", "   ", "\t\t", " \n ", "\u{0}"] {
            assert_eq!(sanitize_reason(Some(raw)), None, "{raw:?} must sanitize to None");
        }
        // Surrounding whitespace is trimmed off a real rationale.
        assert_eq!(sanitize_reason(Some("  keep me  ")).as_deref(), Some("keep me"));
    }
}
