//! **The SHADOWED-STORE finding reaches a log line, proved by RUNNING the loader and capturing it.**
//!
//! `docs/decisions/0054`'s credential half lets `<project>/settings/db/vike.db` answer for the
//! credential store, at which point `<project>/settings/secrets.env` is still on disk and is NO
//! LONGER READ. That is invisible from the operator's side — every skill, runbook and CLI sentence
//! in this tree says *edit `<project>/settings/secrets.env`* — so `vike_secrets` returns a
//! [`vike_secrets::ShadowedStore`] finding beside the credentials.
//!
//! # Why this file exists
//!
//! The finding was returned as DATA and **consumed by nothing**: no binary logged it, no CLI verb
//! printed it, and `git grep shadowed -- crates/ ':!crates/vike-secrets'` found zero consumers. The
//! entire operator-facing mitigation for the move was unreachable, so an operator whose keys had
//! quietly stopped being read got no sentence anywhere telling them why.
//!
//! ⚠ **A grep for the call is not the proof, and that is the whole point of this file.** The arm is
//! three lines inside a conditional, in a function that returns before it on two error paths; a test
//! that asserted `resolved.shadowed.is_some()` would prove the FINDING and say nothing about the
//! LINE. So this drives the real
//! [`vike_bridge_core::credentials::try_load_workspace_secrets_at`] — the one site every composition
//! root's credential read converges on — against a real migrated project, with a real
//! `tracing::Subscriber` installed, and reads what came out.
//!
//! # The capture is the workspace's one, deliberately
//!
//! `vike_log::capture::captured` — written over `tracing`'s own `Subscriber` trait, so asserting a
//! log line here adds no `tracing-subscriber`. This file used to hand-roll its own and override
//! `register_callsite` to `Interest::sometimes()` on the theory that the per-callsite Interest
//! CACHE could then not answer for it. The theory was false: while a capture is the only live
//! dispatcher, tracing-core asks only the default of whichever thread first hits a callsite, so a
//! sibling test reaching the loader's line uncaptured cached `never` for every thread whatever the
//! capture answered. The shared capture's module doc carries the mechanism and the anchor that
//! closes it.

// -------------------------------------------------------------------------------------------
// The fixture
// -------------------------------------------------------------------------------------------

/// Two obviously-fake keys with DISTINCTIVE values, so "no value was logged" is an assertion that
/// could actually fail rather than one that passes because there was nothing to leak.
const KEYS: [(&str, &str); 2] = [
    ("BINANCE_DEMO_API_KEY", "UNIQUE-VALUE-ALPHA"),
    ("BINANCE_DEMO_API_SECRET", "UNIQUE-VALUE-BRAVO"),
];

struct Project {
    _dir: tempfile::TempDir,
    settings: std::path::PathBuf,
}

impl Project {
    fn new() -> Project {
        let dir = tempfile::tempdir().expect("tempdir");
        let settings = dir.path().join("settings");
        std::fs::create_dir_all(&settings).expect("settings dir");
        let body: String =
            KEYS.iter().map(|(k, v)| format!("{k}={v}\n")).collect::<Vec<_>>().concat();
        std::fs::write(settings.join("secrets.env"), body).expect("write store");
        Project { _dir: dir, settings }
    }

    fn arg(&self) -> Option<&str> {
        Some(self.settings.to_str().expect("utf-8 temp path"))
    }

    /// Fill the database from the file. Nothing here removes the file — that is an OPERATOR act and
    /// this workspace performs none of it — which is exactly the state the finding is about.
    fn migrate(&self) {
        // The account classification is the PRODUCTION one — this fixture is about the shadowed
        // FILE, not about the schema, so it has no business carrying a second opinion about which
        // account a key belongs to.
        vike_secrets::migrate(
            self.arg(),
            |_| false,
            &vike_bridge_core::credentials::classify_credential_name,
            vike_secrets::WhenNothingToCarry::CreateNothing,
        )
        .expect("migrate");
        assert!(
            self.settings.join("db").join("vike.db").is_file(),
            "the fixture must actually be migrated, or this file proves nothing"
        );
        assert!(
            self.settings.join("secrets.env").is_file(),
            "…and the shadowed file must still be there, which is the whole finding"
        );
    }
}

/// Every event's LEVEL and message text, in order — `"{LEVEL} {message}"`, the shape the proofs
/// below read — projected from the workspace's one scoped capture.
fn under_capture<T>(f: impl FnOnce() -> T) -> (T, Vec<String>) {
    let (out, events) = vike_log::capture::captured(f);
    (out, events.iter().map(|e| format!("{} {}", e.level, e.message)).collect())
}

// -------------------------------------------------------------------------------------------
// The proofs
// -------------------------------------------------------------------------------------------

/// **A migrated project logs the shadowed file, at WARN, naming both artifacts and no value.**
///
/// Deleting the arm in `try_load_workspace_secrets_at` makes this fail on the first assertion — the
/// captured log is empty and there is nothing to match. That is what makes it the proof rather than
/// a restatement of the finding.
#[test]
fn the_loader_logs_the_credential_file_the_database_shadows() {
    let project = Project::new();
    project.migrate();

    let (loaded, lines) = under_capture(|| {
        vike_bridge_core::credentials::try_load_workspace_secrets_at(project.arg())
    });

    let (map, source) = loaded.expect("a migrated project reads");
    assert!(
        matches!(source, vike_secrets::Source::Database(_)),
        "the fixture must be answering from the database: {source:?}"
    );
    assert_eq!(map.len(), KEYS.len(), "…with the keys in it");

    let shadowed: Vec<&String> = lines.iter().filter(|l| l.contains("NO LONGER READ")).collect();
    assert_eq!(
        shadowed.len(),
        1,
        "the loader must log the shadowed store EXACTLY ONCE. Captured: {lines:?}"
    );
    let line = shadowed[0];
    assert!(line.starts_with("WARN"), "a finding is a WARN, never an error: {line}");
    assert!(line.contains("secrets.env"), "it must name the file that stopped being read: {line}");
    assert!(line.contains("vike.db"), "…and the store that answers instead: {line}");
    assert!(
        line.contains("migrated in"),
        "…and what to do about it, since an edit to that file now changes nothing: {line}"
    );

    // The contract every credential-adjacent log line in this workspace holds.
    let all = lines.join("\n");
    for (key, secret) in KEYS {
        assert!(!all.contains(secret), "a credential VALUE reached the log: {all}");
        let _ = key;
    }
}

/// **RULE 3, daemon side: an UNMIGRATED project — a keyed `secrets.env` and NO database — has NO
/// credentials, and the loader every composition root converges on says so at ERROR.**
///
/// The credential FILE store was removed on 2026-10-07, so this box mounts every venue on paper (the
/// live gate). Without the line asserted here it would do so in SILENCE while the operator's keys
/// sat in the file — the one way a configured box goes paper with nothing in any log. The line
/// names the file, says it is NOT READ, names `vike-cli secrets migrate`, and carries no value. It
/// is not the shadow finding (there is no database to shadow anything), which keeps the test above
/// honest too.
#[test]
fn a_project_with_no_database_says_its_credential_file_is_not_read() {
    let project = Project::new();
    let file = project.settings.join("secrets.env");
    let before = std::fs::read(&file).expect("the fixture's file");

    let (loaded, lines) = under_capture(|| {
        vike_bridge_core::credentials::try_load_workspace_secrets_at(project.arg())
    });

    let (map, source) = loaded.expect("a box with no database is an ANSWER, not an error");
    assert_eq!(source, vike_secrets::Source::None, "a credential FILE must never answer");
    assert!(map.is_empty(), "no credentials, so every venue mounts paper: {map:?}");
    let unread: Vec<&String> = lines.iter().filter(|l| l.contains("NOT READ")).collect();
    assert_eq!(unread.len(), 1, "the unread file must be said EXACTLY ONCE. Captured: {lines:?}");
    let line = unread[0];
    assert!(line.starts_with("ERROR"), "a keyed file nothing reads is an ERROR: {line}");
    assert!(line.contains("secrets.env"), "it must name the file: {line}");
    assert!(line.contains("vike-cli secrets migrate"), "…and the way in: {line}");
    assert!(line.contains("PAPER"), "…and what it costs: {line}");
    assert!(
        !lines.iter().any(|l| l.contains("NO LONGER READ")),
        "nothing is shadowed here, so nothing may say so: {lines:?}"
    );
    let all = lines.join("\n");
    for (_, secret) in KEYS {
        assert!(!all.contains(secret), "a credential VALUE reached the log: {all}");
    }
    assert_eq!(std::fs::read(&file).expect("still there"), before, "the file was touched");
    assert!(!project.settings.join("db").exists(), "a READ created a database");
}

/// **The capture is not blind** — the floor under both tests above.
///
/// If the capture silently saw no events (an `Interest` cache answering `never`, a `Visit` that
/// never matched the `message` field), `a_project_with_no_database_says_its_credential_file_is_not_read` would pass
/// vacuously and `the_loader_logs_…` would fail with a message pointing at the wrong thing. This
/// proves the machinery carries a line through.
#[test]
fn the_capture_sees_a_warn_that_is_really_emitted() {
    let (_, lines) = under_capture(|| tracing::warn!("a_marker_this_file_emitted_itself"));
    assert!(
        lines.iter().any(|l| l.contains("a_marker_this_file_emitted_itself")),
        "the capture is blind, so every assertion in this file is worthless: {lines:?}"
    );
}

/// A loader call that returns an ERROR must not be silent about it either — and must not log a
/// shadow finding for a store it could not open.
///
/// The arm order in `try_load_workspace_secrets_at` is: error out, or log the findings. A database
/// that is present and unreadable has no findings to log, and the caller gets the LOUD error the
/// root `CLAUDE.md` demands rather than an empty map.
#[test]
fn an_unreadable_database_errors_and_logs_no_shadow() {
    let project = Project::new();
    let db = project.settings.join("db").join("vike.db");
    std::fs::create_dir_all(db.parent().expect("parent")).expect("db dir");
    std::fs::write(&db, b"this is not a database").expect("plant");

    let (loaded, lines) = under_capture(|| {
        vike_bridge_core::credentials::try_load_workspace_secrets_at(project.arg())
    });

    let err = loaded.expect_err("a present-and-unreadable store is an ERROR, never an empty map");
    assert!(err.to_string().contains("vike.db"), "{err}");
    assert!(
        !lines.iter().any(|l| l.contains("NO LONGER READ")),
        "a store that did not answer cannot be shadowing anything: {lines:?}"
    );
}
