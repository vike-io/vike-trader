//! The SETTINGS-WRITE audit record (split-plane REQ-7, write half): an accepted
//! `WireCommand::SetSetting` leaves ONE audit record with `kind = "set_setting"` carrying the
//! file, the full dotted key and the **old → new values** — the whole point of auditing a POLICY
//! edit is what the ceiling changed FROM and TO.
//!
//! STANDALONE binary, deliberately NOT in `tests/daemon.rs`'s grouped suite, for exactly
//! `control_roundtrip.rs`'s reason (that file's `audit_capture` doc and the group's own header
//! carry the full argument): observing `vike_tradehub::audit`'s `tracing::info!` events from an
//! integration test requires installing the PROCESS-GLOBAL default subscriber and winning that
//! install race, which is only safe when every other test in the process stays out of it — and
//! the daemon group's members call `vike_log::test_init()`, which installs a different one.
//!
//! The capture module is `control_roundtrip.rs`'s `audit_capture` with the settings-write field
//! set (`file`/`key`/`old`/`new` instead of `coid`) — a sibling, not an import, because a test
//! binary cannot import another test binary's private module.

use std::net::{SocketAddr, TcpListener};
use std::thread;

use vike_hub_proto::auth::HubKeys;
use vike_tradehub::{publish, server};
use vike_tradehub_client::set_setting;
use vike_tradehub_core::{MakerConfig, PaperMakerCore, build_paper_maker_core};

use audit_capture::{settings_audit_entry_for, test_init};

const TOKEN: &str = "SETTINGS_AUDIT_TOKEN";
/// Far-future resolution so the A-S horizon is positive (the settings-write mount shape).
const RESOLUTION_TS: i64 = 3_000_000_000;
const OBSERVE_KEY: &[u8] = b"settings-audit-observe-key";
const CONTROL_KEY: &[u8] = b"settings-audit-control-key";

/// Observing the settings-write audit trail — the `control_roundtrip.rs` capture reshaped for
/// [`vike_tradehub::audit::record_settings_write`]'s field set.
mod audit_capture {
    use vike_log::capture::global;

    /// The tracing target `vike_tradehub::audit`'s events carry (the module path).
    const AUDIT_TARGET: &str = "vike_tradehub::audit";

    /// One captured settings-write audit record. `old: None` means the event carried NO `old`
    /// field at all — the record's honest "the key was not set in the file before".
    #[derive(Debug, Clone, PartialEq)]
    pub struct SettingsAuditEntry {
        pub kind: String,
        pub file: String,
        pub key: String,
        pub old: Option<String>,
        pub new: String,
        pub reason: Option<String>,
    }

    /// Install the process-global capture (`vike_log::capture::global`, the audit target only) exactly
    /// once for this test binary. Best-effort by design: a failed install makes the capture EMPTY,
    /// which the tests then fail on loudly rather than passing vacuously.
    pub fn test_init() {
        global(AUDIT_TARGET);
    }

    /// The audit entry recorded for `key`, waited on briefly (the record lands before the server
    /// writes its reply, so the wait is belt-and-braces, not a race the assertion depends on). Only
    /// settings-write records carry a `file` field; `record`'s order records share the target and are
    /// passed over by their missing field set.
    pub fn settings_audit_entry_for(key: &str) -> Option<SettingsAuditEntry> {
        for _ in 0..200 {
            let found = global(AUDIT_TARGET).into_iter().find(|e| {
                e.field("file").is_some_and(|f| !f.is_empty()) && e.field("key") == Some(key)
            });
            if let Some(e) = found {
                let text = |name: &str| e.field(name).unwrap_or_default().to_string();
                return Some(SettingsAuditEntry {
                    kind: text("kind"),
                    file: text("file"),
                    key: text("key"),
                    old: e.field("old").map(str::to_string),
                    new: text("new"),
                    reason: e.field("reason").map(str::to_string),
                });
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        None
    }
}

/// A control-enabled PAPER node over a real settings directory (the settings_write.rs shape).
fn spawn_hub(dir: &std::path::Path) -> (PaperMakerCore, SocketAddr) {
    let cfg = MakerConfig::outcome_token("polymarket", TOKEN, Some(RESOLUTION_TS));
    let mount = build_paper_maker_core(&cfg);
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
    let addr = listener.local_addr().expect("resolve assigned port");
    let publisher = publish::spawn(mount.handle.snapshot_cell(), None);
    let keys = HubKeys::new(OBSERVE_KEY.to_vec(), CONTROL_KEY.to_vec());
    let commands = Some(mount.handle.command_sink());
    let settings = server::settings::SettingsShowSource {
        settings_dir: Some(dir.to_path_buf()),
        hot: None,
        journal: None,
    };
    thread::spawn(move || {
        let _ = server::serve(
            listener,
            publisher,
            keys,
            commands,
            server::control::ControlLimitsConfig::default(),
            Some(settings),
            // No `AccountAdminSource`: the account capability is an ABSENCE on every box that
            // has not DECLARED a barrier, which is every fixture here and every shipped box today.
            None,
            // No REQ-2 datahub advertisement — this suite exercises the settings verbs only.
            None,
        );
    });
    (mount, addr)
}

/// ⚠ THE POLICY AUDIT: an accepted policy write leaves ONE record with `kind = "set_setting"`, the
/// section, the key, the OLD value the row held and the NEW value written — plus the operator's
/// sanitized rationale beside them.
///
/// The write below still carries a `confirm` and a file NAME, deliberately: that is the shape an
/// OLDER client sends, and the node must accept it while ignoring both (`docs/decisions/0086`
/// points 6 and 7). The modern shape, with neither, is
/// `crates/vike-tradehub/tests/daemon/settings_write.rs`'s
/// `a_settings_write_with_no_file_and_no_confirm_lands`.
#[test]
fn an_accepted_policy_write_is_audited_with_old_and_new_values() {
    test_init();
    let dir = tempfile::tempdir().expect("temp settings dir");
    // ⚠ 0086: there is no settings FILE any more — the daemon resolves from the settings DATABASE,
    // so the fixture seeds a `setting` row directly rather than a `policy.toml` on disk.
    vike_secrets::plant_settings_rows(
        dir.path(),
        &vike_secrets::StoredSettings {
            settings: vec![vike_secrets::SettingRow {
                section: "policy".into(),
                key: "max_notional_per_order".into(),
                value: "500".into(),
            }],
            venue: Vec::new(),
        },
    )
    .expect("seed the settings database");
    let (_mount, addr) = spawn_hub(dir.path());

    let key = "policy.max_notional_per_order";
    set_setting(
        addr,
        CONTROL_KEY,
        "policy.toml",
        key,
        "250",
        Some(key),
        Some("tighter cap\nfor the weekend"),
    )
    .expect("an older client's policy write lands, its ignored confirm and all");

    let entry = settings_audit_entry_for(key).expect("the write was audited");
    assert_eq!(entry.kind, "set_setting");
    // The key's own SECTION WORD, not a file name — there is no settings file any more (0086).
    assert_eq!(entry.file, "policy");
    assert_eq!(entry.old.as_deref(), Some("500"), "the value the row held before");
    assert_eq!(entry.new, "250", "the value written");
    assert_eq!(
        entry.reason.as_deref(),
        Some("tighter capfor the weekend"),
        "the rationale rides beside the old→new pair, sanitized (the newline is stripped)"
    );
}

/// A key the row did NOT hold audits with NO `old` field at all — "unset before" and "set to
/// empty" must never look alike in an incident review.
#[test]
fn a_previously_unset_key_audits_with_no_old_field() {
    test_init();
    let dir = tempfile::tempdir().expect("temp settings dir");
    // A settings DATABASE with no rows at all — still a real store, just one this key has never
    // been written into.
    vike_secrets::plant_settings_rows(dir.path(), &vike_secrets::StoredSettings::default())
        .expect("seed an empty settings database");
    let (_mount, addr) = spawn_hub(dir.path());

    let key = "config.tradehub_addr";
    set_setting(addr, CONTROL_KEY, "config.toml", key, "127.0.0.1:9100", None, None)
        .expect("the config write lands");

    let entry = settings_audit_entry_for(key).expect("the write was audited");
    assert_eq!(entry.kind, "set_setting");
    assert_eq!(entry.file, "config");
    assert_eq!(entry.old, None, "an absent old field, not an empty string");
    assert_eq!(entry.new, "\"127.0.0.1:9100\"", "the JSON-scalar rendering of the written value");
    assert_eq!(entry.reason, None);
}
