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

use std::collections::HashMap;
use std::net::{SocketAddr, TcpListener};
use std::thread;

use vike_run::{MakerMount, MakerMountConfig, build_paper_maker_core};
use vike_tradehub::{publish, server};
use vike_tradehub_client::{NodeKeys, set_setting};

use audit_capture::{settings_audit_entry_for, test_init};

const TOKEN: &str = "SETTINGS_AUDIT_TOKEN";
/// Far-future resolution so the A-S horizon is positive (the settings-write mount shape).
const RESOLUTION_TS: i64 = 3_000_000_000;
const OBSERVE_KEY: &[u8] = b"settings-audit-observe-key";
const CONTROL_KEY: &[u8] = b"settings-audit-control-key";

/// Observing the settings-write audit trail — the `control_roundtrip.rs` capture reshaped for
/// [`vike_tradehub::audit::record_settings_write`]'s field set.
mod audit_capture {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Mutex, Once, OnceLock};

    use tracing::field::{Field, Visit};
    use tracing::{Event, Metadata, Subscriber, span};

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

    fn captured() -> &'static Mutex<Vec<SettingsAuditEntry>> {
        static LOG: OnceLock<Mutex<Vec<SettingsAuditEntry>>> = OnceLock::new();
        LOG.get_or_init(|| Mutex::new(Vec::new()))
    }

    /// Install the capture subscriber exactly once for this test binary. Best-effort by design: a
    /// failed install makes the capture EMPTY, which the tests then fail on loudly rather than
    /// passing vacuously.
    pub fn test_init() {
        static ONCE: Once = Once::new();
        ONCE.call_once(|| {
            let _ = tracing::subscriber::set_global_default(AuditCapture {
                next_span: AtomicU64::new(1), // span::Id::from_u64 panics on 0
            });
        });
    }

    /// The audit entry recorded for `key`, waited on briefly (the record lands before the server
    /// writes its reply, so the wait is belt-and-braces, not a race the assertion depends on).
    pub fn settings_audit_entry_for(key: &str) -> Option<SettingsAuditEntry> {
        for _ in 0..200 {
            if let Some(e) =
                captured().lock().expect("audit capture poisoned").iter().find(|e| e.key == key)
            {
                return Some(e.clone());
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        None
    }

    struct AuditCapture {
        next_span: AtomicU64,
    }

    impl Subscriber for AuditCapture {
        /// ONLY the audit target — everything else in the process is dropped at the callsite.
        fn enabled(&self, metadata: &Metadata<'_>) -> bool {
            metadata.target() == AUDIT_TARGET
        }
        fn new_span(&self, _: &span::Attributes<'_>) -> span::Id {
            span::Id::from_u64(self.next_span.fetch_add(1, Ordering::Relaxed))
        }
        fn record(&self, _: &span::Id, _: &span::Record<'_>) {}
        fn record_follows_from(&self, _: &span::Id, _: &span::Id) {}
        fn event(&self, event: &Event<'_>) {
            let mut visitor = FieldVisitor::default();
            event.record(&mut visitor);
            // Only settings-write records carry a `file` field; `record`'s order records land
            // here too (same target) but are not what this binary asserts on, so they are kept
            // out of the buffer by their missing field set.
            if !visitor.file.is_empty() {
                captured().lock().expect("audit capture poisoned").push(SettingsAuditEntry {
                    kind: visitor.kind,
                    file: visitor.file,
                    key: visitor.key,
                    old: visitor.old,
                    new: visitor.new,
                    reason: visitor.reason,
                });
            }
        }
        fn enter(&self, _: &span::Id) {}
        fn exit(&self, _: &span::Id) {}
    }

    /// Pull the settings-write fields off the audit event. `old`/`reason` are recorded as
    /// `Option<&str>`, which tracing renders as the inner `&str` when `Some` and as NO field at
    /// all when `None` — so an absent field here means the record genuinely carried none.
    #[derive(Default)]
    struct FieldVisitor {
        kind: String,
        file: String,
        key: String,
        old: Option<String>,
        new: String,
        reason: Option<String>,
    }

    impl Visit for FieldVisitor {
        fn record_str(&mut self, field: &Field, value: &str) {
            match field.name() {
                "kind" => self.kind = value.to_string(),
                "file" => self.file = value.to_string(),
                "key" => self.key = value.to_string(),
                "old" => self.old = Some(value.to_string()),
                "new" => self.new = value.to_string(),
                "reason" => self.reason = Some(value.to_string()),
                _ => {}
            }
        }
        // The `message` / `?peer` fields arrive here; nothing to capture from them.
        fn record_debug(&mut self, _: &Field, _: &dyn std::fmt::Debug) {}
    }
}

/// A control-enabled PAPER node over a real settings directory (the settings_write.rs shape).
fn spawn_node(dir: &std::path::Path) -> (MakerMount, SocketAddr) {
    let cfg = MakerMountConfig::polymarket(TOKEN, Some(RESOLUTION_TS));
    let mount = build_paper_maker_core(&cfg);
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
    let addr = listener.local_addr().expect("resolve assigned port");
    let publisher = publish::spawn(mount.handle.snapshot_cell(), None);
    let keys = NodeKeys::new(OBSERVE_KEY.to_vec(), CONTROL_KEY.to_vec());
    let commands = Some(mount.handle.command_sink());
    let settings = server::SettingsShowSource {
        settings_dir: Some(dir.to_path_buf()),
        env: HashMap::new(),
        hot: None,
    };
    thread::spawn(move || {
        let _ = server::serve(
            listener,
            publisher,
            keys,
            commands,
            server::ControlLimitsConfig::default(),
            Some(settings),
            // No REQ-2 datahub advertisement — this suite exercises the settings verbs only.
            None,
        );
    });
    (mount, addr)
}

/// ⚠ THE POLICY AUDIT: an accepted policy write (exact typed confirm) leaves ONE record with
/// `kind = "set_setting"`, the file, the key, the OLD value the file held and the NEW value
/// written — plus the operator's sanitized rationale beside them.
#[test]
fn an_accepted_policy_write_is_audited_with_old_and_new_values() {
    test_init();
    let dir = tempfile::tempdir().expect("temp settings dir");
    std::fs::write(dir.path().join("policy.toml"), "max_notional_per_order = 500\n")
        .expect("seed policy.toml");
    let (_mount, addr) = spawn_node(dir.path());

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
    .expect("the confirmed policy write lands");

    let entry = settings_audit_entry_for(key).expect("the write was audited");
    assert_eq!(entry.kind, "set_setting");
    assert_eq!(entry.file, "policy.toml");
    assert_eq!(entry.old.as_deref(), Some("500"), "the value the file held before");
    assert_eq!(entry.new, "250", "the value written");
    assert_eq!(
        entry.reason.as_deref(),
        Some("tighter capfor the weekend"),
        "the rationale rides beside the old→new pair, sanitized (the newline is stripped)"
    );
}

/// A key the file did NOT hold audits with NO `old` field at all — "unset before" and "set to
/// empty" must never look alike in an incident review.
#[test]
fn a_previously_unset_key_audits_with_no_old_field() {
    test_init();
    let dir = tempfile::tempdir().expect("temp settings dir");
    let (_mount, addr) = spawn_node(dir.path());

    let key = "config.tradehub_addr";
    set_setting(addr, CONTROL_KEY, "config.toml", key, "127.0.0.1:9100", None, None)
        .expect("the config write lands");

    let entry = settings_audit_entry_for(key).expect("the write was audited");
    assert_eq!(entry.kind, "set_setting");
    assert_eq!(entry.file, "config.toml");
    assert_eq!(entry.old, None, "an absent old field, not an empty string");
    assert_eq!(entry.new, "\"127.0.0.1:9100\"", "the TOML rendering of the written value");
    assert_eq!(entry.reason, None);
}
