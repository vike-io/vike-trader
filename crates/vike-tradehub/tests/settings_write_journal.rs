//! **An accepted settings write reaches the DURABLE change journal, not only the log line.**
//!
//! `crates/vike-tradehub/tests/settings_write_audit.rs` is the sibling that proves the
//! `tracing::info!` record — the console/journald copy. This file proves the half that survives:
//! the append-only JSONL ledger under `<project>/settings/state/changes`, which nothing rotates
//! away and no `VIKE_LOG_FILE_LEVEL` can silence.
//!
//! That distinction is measured rather than theoretical, and `vike_tradehub::audit`'s module doc
//! carries the numbers: on the live the CI box box that daemon's 23 MB log file held 53,160 ERROR lines,
//! 1,928 WARN lines and **zero** INFO lines, because
//! `deploy/vike-tradehub.service` sets `VIKE_LOG_FILE_LEVEL=warn` and the environment beats
//! `vike_log::LogConfig`'s `file_level`. Every record this module's sibling asserts about was
//! therefore invisible on the one box that matters.
//!
//! ⚠ **The LEVEL half of that is now closed; the RETENTION half is what keeps this file's subject
//! the durable one.** `vike_tradehub::audit::FILE_PIN` raises this crate's audit target above the
//! global file level, so the sibling's line does reach the file again at the shipped `warn`
//! (`crates/vike-tradehub/tests/daemon/audit_reaches_disk.rs` proves it against a real daemon). What the
//! pin cannot do is stop the file being rotated daily and deleted after
//! `vike_log::DEFAULT_MAX_LOG_FILES` days. So the log copy is now a complete record of a short
//! window, and the JSONL ledger this file asserts about is still the one that is there when an
//! incident review arrives weeks later.
//!
//! ⚠ **Nothing here installs a tracing subscriber**, which is why this is a separate binary from
//! neither of the daemon groups: it drives `vike_tradehub::audit::record_settings_write` directly
//! and reads the FILE. The `tracing` line it also emits goes nowhere and is not this file's
//! subject.

use std::path::Path;

use tempfile::TempDir;
use vike_config::write::SettingsWrite;
use vike_model::change_journal::{ChangeJournal, Outcome, Proc};
use vike_model::state_path::STATE_SUBDIR;
use vike_tradehub::audit::{SettingsWriteAudit, record_settings_write};
use vike_tradehub_client::{NodeKeys, Scope};

/// 2026-08-21T00:00:00Z.
const T: i64 = 1_787_356_800_000;

fn journal(dir: &Path) -> ChangeJournal {
    ChangeJournal::new(dir.to_path_buf(), Proc::new("vike-tradehub", 4711, "0.1.0"))
}

/// ⚠ The `key` these tests pass is not free-choice: `crates/vike-config/tests/policy_is_consumed.rs`'s
/// `an_unconsumed_field_is_really_unconsumed` scans the tree for TEXTUAL appearances of every
/// `Policy` field marked `Consumed::No` and fails if one is "read" anywhere. This file first used
/// `policy.max_leverage` as an arbitrary fixture string and turned that gate red in CI — the field
/// is deliberately unconsumed (its `1.0` default would clamp every policy-file-less deployment to
/// 1x, so the mount does not carry it) and a test naming it looks exactly like a wiring that
/// stopped honouring the admission. Use `max_notional_per_order`, which IS consumed and is also the
/// ceiling an operator actually edits, so the fixture reads as the real case rather than as noise.
fn write_of(key: &str, old: Option<&str>, new: &str) -> SettingsWrite {
    SettingsWrite {
        file: "policy.toml",
        key: key.to_string(),
        old_value: old.map(str::to_string),
        new_value: new.to_string(),
    }
}

fn lines(dir: &Path) -> Vec<serde_json::Value> {
    let path = journal(dir).file_for(T);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("no journal at {} ({e})", path.display()))
        .lines()
        .map(|l| serde_json::from_str(l).expect("each line is one whole JSON record"))
        .collect()
}

/// **THE WIRING.** A recorded settings write lands in the journal, carrying the cells an incident
/// review needs: what file, what key, from what, to what, why, and whether it is actually ARMED.
#[test]
fn a_recorded_settings_write_lands_in_the_change_journal() {
    let dir = TempDir::new().expect("temp dir");
    let j = journal(dir.path());
    let peer: std::net::SocketAddr = "127.0.0.1:51234".parse().unwrap();

    record_settings_write(SettingsWriteAudit {
        peer: Some(peer),
        key_id: Some("nk-0011223344556677"),
        journal: Some(&j),
        now_ms: T,
        write: &write_of("policy.max_notional_per_order", Some("100"), "250"),
        reason: Some("raising ahead of the CPI print"),
        outcome: Outcome::AppliedPendingRestart,
    });

    let v = &lines(dir.path())[0];
    assert_eq!(v["kind"], "set_setting");
    assert_eq!(v["target"]["file"], "policy.toml");
    assert_eq!(v["target"]["key"], "policy.max_notional_per_order");
    assert_eq!(v["target"]["old"], "100", "the ceiling it changed FROM");
    assert_eq!(v["target"]["new"], "250", "…and TO");
    assert_eq!(v["reason"], "raising ahead of the CPI print");
    assert_eq!(v["ts_ms"], T);

    // The ACTOR is the truth about the channel, not an invented human.
    assert_eq!(v["actor"]["origin"], "wire");
    assert_eq!(v["actor"]["peer"], "127.0.0.1:51234");
    assert_eq!(v["actor"]["scope"], "control");
    assert_eq!(v["actor"]["key_id"], "nk-0011223344556677", "…and WHICH key authenticated");
    assert!(v["actor"].get("user").is_none(), "there are no human accounts in this system");

    // …and the restart-to-apply bit, which is what makes the journal answer "was that ceiling
    // ACTUALLY armed" rather than only "was it written".
    assert_eq!(v["outcome"], "applied_pending_restart");
    assert_eq!(v["proc"]["bin"], "vike-tradehub");
    assert_eq!(v["proc"]["pid"], 4711);
}

/// The restart bit really is carried through rather than constant — the anti-vacuity twin of the
/// assertion above.
#[test]
fn a_hot_applied_write_records_a_different_outcome() {
    let dir = TempDir::new().expect("temp dir");
    let j = journal(dir.path());
    record_settings_write(SettingsWriteAudit {
        peer: None,
        key_id: None,
        journal: Some(&j),
        now_ms: T,
        write: &write_of("config.tradehub_addr", None, "\"0.0.0.0:9000\""),
        reason: None,
        outcome: Outcome::Applied,
    });
    let v = &lines(dir.path())[0];
    assert_eq!(v["outcome"], "applied", "a hot-applied write is not pending a restart");
    assert!(v.get("reason").is_none(), "no rationale records no field");
    assert!(v["target"].get("old").is_none(), "…and a key absent from the file records no `old`");
    assert!(v["actor"].get("peer").is_none(), "a surface with no peer records none");
    assert!(
        v["actor"].get("key_id").is_none(),
        "…and a surface that authenticated NO key records no id rather than a borrowed one"
    );
}

/// ⚠ **The id in the ledger is the fingerprint of the key that actually authenticated, and the KEY
/// ITSELF reaches no byte of the file.**
///
/// The id is taken from the REAL `NodeKeys::key_id` rather than a literal, so this fails if the
/// audit path ever records something else (a peer address, a scope name, a truncation of the key).
/// The negative half asserts on the key's own BYTES — raw and hex, whole and in four-byte windows —
/// against the raw file, because a ledger is append-only: a credential that lands in one is not
/// rotated away by anything.
#[test]
fn the_ledger_names_the_authenticating_key_and_never_carries_it() {
    let dir = TempDir::new().expect("temp dir");
    let j = journal(dir.path());
    // ⚠ Deliberately not an English phrase and free of digits: the windowed check below would
    // otherwise fire on the record's OWN vocabulary (a key spelled `…-control-…` shares `cont` and
    // `trol` with `"scope":"control"`), and a digit run's hex could collide with `ts_ms`/`pid`.
    let control: &[u8] = b"ZqXvNbKdMsWtYrHjPlGfTcRxSaUeObJw";
    let keys = NodeKeys::new(b"an-observe-key".to_vec(), control.to_vec());
    let id = keys.key_id(Scope::Control).expect("a configured control key has an id");

    record_settings_write(SettingsWriteAudit {
        peer: Some("127.0.0.1:51234".parse().unwrap()),
        key_id: Some(&id),
        journal: Some(&j),
        now_ms: T,
        write: &write_of("policy.max_notional_per_order", Some("2"), "3"),
        reason: None,
        outcome: Outcome::Applied,
    });

    let raw = std::fs::read_to_string(journal(dir.path()).file_for(T)).expect("journal");
    let v: serde_json::Value = serde_json::from_str(raw.trim_end()).expect("one record");
    assert_eq!(v["actor"]["key_id"], id.as_str(), "the ledger names the key that authenticated");
    assert!(id.starts_with("nk-"), "…by its fingerprint, self-describing in a bare grep: {id}");

    // The key material, every way it could have leaked into the line.
    let hex = |b: &[u8]| b.iter().map(|x| format!("{x:02x}")).collect::<String>();
    assert!(!raw.contains(std::str::from_utf8(control).unwrap()), "raw key bytes: {raw}");
    assert!(!raw.contains(&hex(control)), "the key's hex: {raw}");
    for window in control.windows(4) {
        let w = std::str::from_utf8(window).unwrap();
        assert!(!raw.contains(w), "a 4-byte window of the key leaked ({w}): {raw}");
        assert!(!raw.contains(&hex(window)), "a 4-byte window's hex leaked: {raw}");
    }

    // ⚠ The anti-vacuity controls, keyed on things INDEPENDENT of the property under test: the
    // record really was written (so the `contains` checks above are not scanning an empty file),
    // and the leak detector really can fire (so they are not passing by the helper being broken).
    assert!(raw.contains(&id), "the assertions above ran against a real record: {raw}");
    assert!(
        raw.contains("policy.max_notional_per_order"),
        "…a real record with the write in it: {raw}"
    );
    let planted = format!("leak={}", hex(control));
    assert!(planted.contains(&hex(control)), "the detector can find a hex leak");
    assert!(control.windows(4).all(|w| planted.contains(&hex(w))), "…and a windowed one");
}

/// An OBSERVE key and a CONTROL key are distinguishable in the ledger — the id identifies the
/// credential, so a record cannot silently attribute a control write to the read-only key.
#[test]
fn the_two_scopes_keys_do_not_share_an_id() {
    let keys = NodeKeys::new(b"an-observe-key".to_vec(), b"a-control-key".to_vec());
    assert_ne!(keys.key_id(Scope::Observe), keys.key_id(Scope::Control));
    // …and an unconfigured scope is not identified at all.
    let control_only = NodeKeys::new(Vec::new(), b"a-control-key".to_vec());
    assert!(control_only.key_id(Scope::Observe).is_none());
    assert_eq!(control_only.key_id(Scope::Control), keys.key_id(Scope::Control));
}

/// ⚠ **The redaction reaches the DURABLE copy too.**
///
/// `vike_config::is_secret_key` redacts a credential-shaped key's values in the `tracing` line, and
/// the journal is built from the same locals — so a leak cannot exist in one channel and not the
/// other. Insurance today (no settings key is credential-shaped), and this is precisely the file
/// that must not be the one surface that leaks if that changes: unlike a log line, a journal record
/// is not rotated away.
#[test]
fn a_credential_shaped_key_is_redacted_in_the_journal_as_well() {
    let dir = TempDir::new().expect("temp dir");
    let j = journal(dir.path());
    let key = "config.node_api_token";
    assert!(vike_config::is_secret_key(key), "precondition: this key really is credential-shaped");

    record_settings_write(SettingsWriteAudit {
        peer: None,
        key_id: None,
        journal: Some(&j),
        now_ms: T,
        write: &write_of(key, Some("old-secret-value"), "new-secret-value"),
        reason: None,
        outcome: Outcome::Applied,
    });

    let raw = std::fs::read_to_string(journal(dir.path()).file_for(T)).expect("journal");
    assert!(!raw.contains("secret-value"), "no credential value reaches the ledger: {raw}");
    let v: serde_json::Value = serde_json::from_str(raw.trim_end()).unwrap();
    assert_eq!(v["target"]["old"], vike_config::redact::REDACTED);
    assert_eq!(v["target"]["new"], vike_config::redact::REDACTED);
    // …and `old` is still PRESENT, so "there was a previous value, redacted" stays distinct from
    // "the key was not set at all".
    assert!(v["target"].get("old").is_some());
}

/// A remote rationale cannot forge a second record. The `reason` is peer-supplied free text and
/// this is an append-only LEDGER — a newline plus a plausible object would append entries that read
/// exactly like genuine ones.
#[test]
fn a_remote_rationale_cannot_forge_a_journal_entry() {
    let dir = TempDir::new().expect("temp dir");
    let j = journal(dir.path());
    // Pre-sanitized by `accept_command` in production; passed RAW here on purpose, because the
    // journal must not be the one layer relying on somebody else having done it.
    let injected = "flat\n{\"ts_ms\":0,\"seq\":0,\"kind\":\"set_setting\"}\r\nmore";
    record_settings_write(SettingsWriteAudit {
        peer: None,
        key_id: None,
        journal: Some(&j),
        now_ms: T,
        write: &write_of("policy.max_notional_per_order", None, injected),
        reason: Some(injected),
        outcome: Outcome::Applied,
    });

    let raw = std::fs::read_to_string(journal(dir.path()).file_for(T)).expect("journal");
    assert_eq!(raw.lines().count(), 1, "one write is one record: {raw:?}");
    assert!(!raw.trim_end().contains('\n') && !raw.contains('\r'), "no line terminator: {raw:?}");
    let v: serde_json::Value = serde_json::from_str(raw.trim_end()).expect("still one object");
    assert_eq!(v["ts_ms"], T, "the forged `ts_ms` did not become a record");
}

/// `journal: None` keeps the pre-journal behaviour EXACTLY — the honest degradation for a server
/// whose boot walk found no project directory. It must write nothing rather than invent a location.
#[test]
fn a_journal_less_surface_writes_nothing() {
    let dir = TempDir::new().expect("temp dir");
    record_settings_write(SettingsWriteAudit {
        peer: None,
        key_id: None,
        journal: None,
        now_ms: T,
        write: &write_of("policy.max_notional_per_order", None, "1"),
        reason: None,
        outcome: Outcome::Applied,
    });
    assert_eq!(
        std::fs::read_dir(dir.path()).unwrap().count(),
        0,
        "a journal-less record must not invent a location to write to"
    );
}

/// The server-arm plumbing: the journal resolves under the SAME already-resolved settings directory
/// the write itself lands in — never a fresh, `$VIKE_SETTINGS_DIR`-blind walk.
#[test]
fn the_server_resolves_the_journal_beside_the_settings_it_writes() {
    let dir = TempDir::new().expect("temp dir");
    let src = vike_tradehub::server::SettingsShowSource {
        settings_dir: Some(dir.path().to_path_buf()),
        env: std::collections::HashMap::new(),
        hot: None,
    };
    let j = src.change_journal().expect("a settings dir yields a journal");
    assert_eq!(
        j.dir(),
        dir.path().join(STATE_SUBDIR).join("changes"),
        "the ledger sits under the state directory of the very project being written"
    );
    // …and the process identity is this binary's, not a placeholder.
    let file = j.file_for(T);
    assert_eq!(file.file_name().unwrap(), "changes-2026-08.jsonl");

    // No project above the working directory: no journal, and the tracing line alone remains.
    let none = vike_tradehub::server::SettingsShowSource {
        settings_dir: None,
        env: std::collections::HashMap::new(),
        hot: None,
    };
    assert!(none.change_journal().is_none(), "no project means no invented ledger location");
}
