use super::*;

fn tmpdir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!(
        "vike-secrets-{tag}-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn sample() -> SecretMap {
    let mut m = BTreeMap::new();
    m.insert("BINANCE_LIVE_API_KEY".to_string(), "key-abcd".to_string());
    m.insert("BINANCE_LIVE_API_SECRET".to_string(), "sup3r-s3cr3t".to_string());
    SecretMap::new(m)
}

fn put(path: &Path, value: &str) {
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d).unwrap();
    }
    std::fs::write(path, format!("# a comment\nBINANCE_LIVE_API_KEY={value}\n")).unwrap();
}

fn value(r: &Resolved) -> String {
    r.secrets.clone().into_map().get("BINANCE_LIVE_API_KEY").cloned().unwrap_or_default()
}

#[test]
fn debug_shows_key_names_but_never_a_value() {
    let shown = format!("{:?}", sample());
    assert!(shown.contains("BINANCE_LIVE_API_KEY=***"));
    assert!(shown.contains("2 entries"));
    assert!(!shown.contains("sup3r-s3cr3t"));
    assert!(!shown.contains("key-abcd"));
}

#[test]
fn debug_of_an_empty_map_is_harmless() {
    assert_eq!(format!("{:?}", SecretMap::default()), "SecretMap(0 entries: [])");
}

/// Bring a settings database into existence the one way that is allowed — `migrate --init`'s
/// parameter — so a test can exercise the arm that answers.
fn plant_store(settings: &Path) {
    crate::migrate(
        Some(settings.to_str().expect("utf-8 tempdir")),
        |_| false,
        &crate::schema::Classification::unrecognised,
        crate::WhenNothingToCarry::CreateEmptyStore,
    )
    .expect("migrate --init creates the empty store");
}

fn write_one(settings: &Path, value: &str) -> std::io::Result<Backend> {
    save_credentials_to_store(
        settings,
        crate::db::Table::Credential,
        &[("BINANCE_LIVE_API_KEY".to_string(), value.to_string())],
        Some(&crate::schema::Classification::unrecognised),
    )
}

/// **A box with NO database: an empty map, [`Source::None`] — the live gate — and nothing else.**
#[test]
fn no_store_at_all_is_an_empty_map_not_an_error() {
    let d = tmpdir("absent");
    let settings = d.join("settings");
    std::fs::create_dir_all(&settings).unwrap();
    let r = resolve_store_in(&settings, crate::db::Table::Credential).unwrap();
    assert!(r.secrets.is_empty());
    assert_eq!(r.source, Source::None);
    assert_eq!(r.warning, None);
    assert_eq!(r.legacy, None, "no leftover beside it either");
    assert_eq!(r.unread, None, "and no credential file to report");
    let _ = std::fs::remove_dir_all(&d);
}

/// **RULE 3: a credential file with keys and NO database is SAID OUT LOUD — and still not read.**
///
/// The map stays EMPTY (the live gate: every venue paper), the finding names the file, how many
/// keyed names it holds, that it is NOT READ, and `vike-cli secrets migrate`. It prints a path and a
/// number and never a value, and the file is byte-identical afterwards.
#[test]
fn a_keyed_credential_file_with_no_database_is_reported_unread() {
    let d = tmpdir("unread");
    let settings = d.join("settings");
    let file = settings.join(crate::SECRETS_FILE);
    put(&file, "never-printed");
    let before = std::fs::read(&file).unwrap();

    let r = resolve_store_in(&settings, crate::db::Table::Credential).unwrap();
    assert!(r.secrets.is_empty(), "the file must NOT be read as a store");
    assert_eq!(r.source, Source::None);
    let u = r.unread.expect("a keyed credential file with no database must not be silent");
    assert_eq!(u.file, file);
    assert_eq!(u.keyed, Ok(1));
    assert!(u.holds_keys());
    let msg = u.to_string();
    assert!(msg.contains("NOT READ"), "{msg}");
    assert!(msg.contains("vike-cli secrets migrate"), "it must name the way in: {msg}");
    assert!(msg.contains("PAPER"), "and say what it costs: {msg}");
    assert!(!msg.contains("never-printed"), "the finding must never print a value: {msg}");
    assert_eq!(std::fs::read(&file).unwrap(), before, "nothing may touch the file");
    assert_eq!(r.legacy, None, "the louder finding already covers this box");

    // The NODE-key file gets the same finding through the node-key front door.
    let node = settings.join(crate::NODE_FILE);
    std::fs::write(&node, "VIKE_TRADEHUB_OBSERVE_KEY=never-printed\n").unwrap();
    let n = resolve_node_keys(settings.to_str(), |_| true).unwrap();
    assert!(n.secrets.is_empty(), "node.env must NOT be read as a store");
    assert_eq!(n.unread.expect("node.env must be reported").file, node);

    // A file that holds no value is still reported, and says so — `warn`, not `error`.
    std::fs::write(&file, "# template\nBINANCE_LIVE_API_KEY=\n").unwrap();
    let blank = resolve_store_in(&settings, crate::db::Table::Credential).unwrap();
    let u = blank.unread.expect("an unread file is reported whatever it holds");
    assert_eq!(u.keyed, Ok(0));
    assert!(!u.holds_keys());
    assert!(u.to_string().contains("--init"), "an empty file's remedy is --init: {u}");

    // …and one that cannot be read is reported with the OS reason instead of a count.
    std::fs::remove_file(&file).unwrap();
    std::fs::create_dir_all(&file).unwrap();
    let odd = resolve_store_in(&settings, crate::db::Table::Credential).unwrap();
    let u = odd.unread.expect("an unreadable leftover is reported, not skipped");
    assert!(u.keyed.is_err() && u.holds_keys(), "{u:?}");
    let _ = std::fs::remove_dir_all(&d);
}

/// **A leftover `<project>/.env` with NO store is a finding — the silent case.**
///
/// The empty map is unchanged and [`Source::None`] is unchanged: this adds a diagnostic beside
/// the live gate, it does not alter it.
#[test]
fn a_leftover_dotenv_beside_the_project_is_a_finding_when_the_store_is_absent() {
    let d = tmpdir("legacy-absent");
    let settings = d.join("settings");
    std::fs::create_dir_all(&settings).unwrap();
    std::fs::write(d.join(LEGACY_STORE_FILE), "BINANCE_LIVE_API_KEY=never-printed\n").unwrap();

    let r = resolve_store_in(&settings, crate::db::Table::Credential).unwrap();
    // The live gate is untouched.
    assert!(r.secrets.is_empty());
    assert_eq!(r.source, Source::None);
    // …and the finding names both files.
    let w = r.legacy.expect("a leftover store must not be silent");
    assert_eq!(w.legacy, d.join(LEGACY_STORE_FILE));
    assert_eq!(w.store, settings.join(crate::SECRETS_FILE));
    let msg = w.to_string();
    assert!(msg.contains("stays") && msg.contains("paper"), "{msg}");
    assert!(msg.contains("vike-cli secrets migrate"), "the finding must say how to fix it: {msg}");
    assert!(msg.contains("EnvironmentFile"), "…and when it is NOT a problem: {msg}");
    assert!(!msg.contains("never-printed"), "the probe must never read the CONTENTS: {msg}");
    let _ = std::fs::remove_dir_all(&d);
}

/// **A `.env` beside a store that LOADED is silent.** It is a systemd `EnvironmentFile` — the CI box's
/// live recorder ships one — and nothing was silent about credentials, because the store answered.
#[test]
fn a_dotenv_beside_a_present_store_is_not_a_finding() {
    let d = tmpdir("legacy-present");
    let settings = d.join("settings");
    std::fs::create_dir_all(&settings).unwrap();
    plant_store(&settings);
    write_one(&settings, "from-the-store").unwrap();
    std::fs::write(d.join(LEGACY_STORE_FILE), "POLY_PROXY_ENABLED=false\n").unwrap();

    let r = resolve_store_in(&settings, crate::db::Table::Credential).unwrap();
    assert_eq!(value(&r), "from-the-store");
    assert_eq!(r.legacy, None, "a store that loaded is not a silent transition");
    let _ = std::fs::remove_dir_all(&d);
}

/// **Beside a DATABASE the same file is SHADOWED, not unread** — a different sentence, and the
/// database's value wins with no per-key fallback to the file.
#[test]
fn a_credential_file_beside_the_database_is_shadowed_and_never_read() {
    let d = tmpdir("shadowed");
    let settings = d.join("settings");
    plant_store(&settings);
    write_one(&settings, "from-the-database").unwrap();
    put(&settings.join(crate::SECRETS_FILE), "from-the-file");

    let r = resolve_store_in(&settings, crate::db::Table::Credential).unwrap();
    assert_eq!(value(&r), "from-the-database");
    assert!(matches!(r.source, Source::Database(_)));
    assert!(r.shadowed.is_some(), "the file beside the database must be reported");
    assert_eq!(r.unread, None, "UNREAD is the no-database finding; this one is SHADOWED");
    let _ = std::fs::remove_dir_all(&d);
}

/// **A WRITE with no database is REFUSED and lands nowhere** — no database is created and no file
/// is written, so a key cannot vanish into a file nothing reads.
#[test]
fn a_write_with_no_database_is_refused_and_creates_nothing() {
    let d = tmpdir("write-absent");
    let settings = d.join("settings");
    std::fs::create_dir_all(&settings).unwrap();

    let e = write_one(&settings, "nowhere-at-all").expect_err("no store, no write");
    assert_eq!(e.kind(), std::io::ErrorKind::NotFound);
    let msg = e.to_string();
    assert!(msg.contains("migrate --init"), "the refusal must name the way in: {msg}");
    assert!(!msg.contains("nowhere-at-all"), "a refusal must never echo the value: {msg}");
    assert!(!crate::db_path_in(&settings).exists(), "no database may be created");
    assert!(!settings.join(crate::SECRETS_FILE).exists(), "and no credential FILE written");
    let _ = std::fs::remove_dir_all(&d);
}

/// A credential is ONE line — refused before any store is touched.
#[test]
fn a_multiline_value_is_refused_first() {
    let e = journal::refuse_multiline(&[("K".to_string(), "a\nb".to_string())]).unwrap_err();
    assert_eq!(e.kind(), std::io::ErrorKind::InvalidInput);
    assert!(journal::refuse_multiline(&[("K".to_string(), "ab".to_string())]).is_ok());
}

/// The probe is `metadata`, so only `NotFound` is absence — a DIRECTORY named `.env` still
/// reports, and a path with no grandparent has no project to look beside.
#[test]
fn only_not_found_counts_as_absent_and_a_rootless_path_is_skipped() {
    let d = tmpdir("legacy-dir");
    let store = d.join("settings").join("secrets.env");
    std::fs::create_dir_all(store.parent().unwrap()).unwrap();
    std::fs::create_dir_all(d.join(LEGACY_STORE_FILE)).unwrap();
    assert!(legacy_store_warning(&store).is_some(), "a directory is not established absence");

    // ...but a DIRECTORY makes `metadata` return `Ok`, so the case above exercises the `_` arm
    // and never the guard itself. The guard — "only NotFound counts as absent" — needs an
    // error that is NOT NotFound, and this is it: probing THROUGH a regular file yields
    // ENOTDIR.
    #[cfg(unix)]
    {
        let file = d.join("proj");
        put(&file, "not a directory");
        let under_a_file = file.join("settings").join(crate::SECRETS_FILE);
        assert!(
            legacy_store_warning(&under_a_file).is_some(),
            "ENOTDIR is not established absence — only NotFound is"
        );
    }

    // `secrets.env` alone: parent is "", and "" has no parent — no project, no probe.
    assert_eq!(legacy_store_warning(Path::new(crate::SECRETS_FILE)), None);
    let _ = std::fs::remove_dir_all(&d);
}

/// **A store that EXISTS and cannot be opened is an ERROR, never a silent empty map.**
///
/// A file of non-database bytes where the database should be: `database_present` is an `is_file`,
/// so the backend says there IS a store, and the open must then fail out loud rather than read as
/// "not configured" — the conflation the live gate must never wear.
#[test]
fn an_unreadable_store_errors_instead_of_reporting_no_credentials() {
    let d = tmpdir("unreadable");
    let settings = d.join("settings");
    let db = crate::db_path_in(&settings);
    std::fs::create_dir_all(db.parent().unwrap()).unwrap();
    std::fs::write(&db, b"this is not a sqlite database, and it is not empty either").unwrap();

    resolve_store_in(&settings, crate::db::Table::Credential)
        .expect_err("an unopenable store must not read as `not configured`");
    let _ = std::fs::remove_dir_all(&d);
}

/// The project entry point resolves through the settings-directory override, with no
/// environment read anywhere in this crate.
#[test]
fn the_project_entry_point_honours_an_explicit_settings_dir() {
    let d = tmpdir("project");
    let settings = d.join("settings");
    plant_store(&settings);
    write_one(&settings, "deployed").unwrap();

    let r = resolve_project(settings.to_str()).unwrap();
    assert_eq!(r.source, Source::Database(crate::db_path_in(&settings)));
    assert_eq!(value(&r), "deployed");

    // …and a blank override falls through to the walk rather than resolving to `""`.
    assert_eq!(resolve_project(Some("  ")).unwrap(), resolve_project(None).unwrap());
    let _ = std::fs::remove_dir_all(&d);
}

/// Two projects, two stores, one process — no global state is involved in pointing at either.
#[test]
fn two_stores_coexist_in_one_process() {
    let a = tmpdir("coexist-a");
    let b = tmpdir("coexist-b");
    plant_store(&a);
    plant_store(&b);
    write_one(&a, "first").unwrap();
    write_one(&b, "second").unwrap();

    assert_eq!(value(&resolve_project(a.to_str()).unwrap()), "first");
    assert_eq!(value(&resolve_project(b.to_str()).unwrap()), "second");
    let _ = std::fs::remove_dir_all(&a);
    let _ = std::fs::remove_dir_all(&b);
}

/// **The permission probe fires on a too-open path and not on 0600** — asked directly, because it
/// is a stat that never opens the file (what `vike-cli secrets path` and `config check` rely on).
///
/// `0o640` (group-readable) and `0o604` (other-readable) are the two shapes a `cp` or a shared
/// deploy actually produces; `0o620` proves the check is not read-only — a group-WRITABLE store
/// lets somebody substitute the keys an order is signed with.
#[cfg(unix)]
#[test]
fn a_group_or_world_accessible_store_warns() {
    use std::os::unix::fs::PermissionsExt;
    let d = tmpdir("perm-warn");
    let store = d.join("vike.db");
    put(&store, "s3cr3t-key-material");

    for mode in [0o644u32, 0o640, 0o604, 0o620, 0o666] {
        std::fs::set_permissions(&store, std::fs::Permissions::from_mode(mode)).unwrap();
        let Some(w) = permission_warning(&store) else { panic!("mode {mode:04o} must warn") };
        assert_eq!(w.path, store);
        assert_eq!(w.finding, Finding::ExposedMode(mode));
        let msg = w.to_string();
        assert!(msg.contains("chmod 600"), "the warning must say how to fix it: {msg}");
        assert!(
            !msg.contains("s3cr3t-key-material"),
            "the warning must never print a credential VALUE"
        );
    }

    // …and 0600 is silent.
    std::fs::set_permissions(&store, std::fs::Permissions::from_mode(0o600)).unwrap();
    assert_eq!(permission_warning(&store), None, "an owner-only store must not warn");
    let _ = std::fs::remove_dir_all(&d);
}

/// **A SYMLINKED store is reported — and the check is the one that can see it.**
///
/// The stat is `symlink_metadata`, so the indirection is visible; the reported mode is the
/// FOLLOWED target's, masked to the permission bits (a sweep that replaced the `& 0o777` mask with
/// `|` or `^` once left every message-only assertion green).
#[cfg(unix)]
#[test]
fn a_symlinked_store_is_reported_even_when_the_file_it_points_at_is_owner_only() {
    use std::os::unix::fs::PermissionsExt;
    let d = tmpdir("perm-symlink");
    let real = d.join("shared-vike.db");
    put(&real, "s3cr3t-key-material");
    std::fs::set_permissions(&real, std::fs::Permissions::from_mode(0o600)).unwrap();
    let link = d.join("vike.db");
    std::os::unix::fs::symlink(&real, &link).unwrap();

    let w = permission_warning(&link)
        .expect("a symlinked credential store must be reported, whatever its target's mode");
    assert_eq!(w.path, link);
    assert_eq!(
        w.finding,
        Finding::Symlink { target: Some(real.clone()), target_mode: Some(0o600) },
        "the reported mode is the target's, masked to the permission bits"
    );
    let msg = w.to_string();
    assert!(msg.to_lowercase().contains("symlink"), "{msg}");
    assert!(msg.contains(&real.display().to_string()), "{msg}");
    assert!(!msg.contains("s3cr3t-key-material"), "{msg}");
    let _ = std::fs::remove_dir_all(&d);
}
