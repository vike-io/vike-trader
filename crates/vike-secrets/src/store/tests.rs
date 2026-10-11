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

/// A file holding `value` — the bytes the two permission probes below stat and must never print.
#[cfg(unix)]
fn put(path: &Path, value: &str) {
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d).unwrap();
    }
    std::fs::write(path, value).unwrap();
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

/// Bring a settings database into existence the one way that is allowed — `secrets init`'s
/// parameter — so a test can exercise the arm that answers.
fn plant_store(settings: &Path) {
    crate::create_store(Some(settings.to_str().expect("utf-8 tempdir")))
        .expect("secrets init creates the empty store");
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
    let _ = std::fs::remove_dir_all(&d);
}

/// **A WRITE with no database is REFUSED and lands nowhere** — no database is created, so a key
/// cannot vanish into a store nobody asked for.
#[test]
fn a_write_with_no_database_is_refused_and_creates_nothing() {
    let d = tmpdir("write-absent");
    let settings = d.join("settings");
    std::fs::create_dir_all(&settings).unwrap();

    let e = write_one(&settings, "nowhere-at-all").expect_err("no store, no write");
    assert_eq!(e.kind(), std::io::ErrorKind::NotFound);
    let msg = e.to_string();
    assert!(msg.contains("secrets init"), "the refusal must name the way in: {msg}");
    assert!(!msg.contains("nowhere-at-all"), "a refusal must never echo the value: {msg}");
    assert!(!crate::db_path_in(&settings).exists(), "no database may be created");
    let _ = std::fs::remove_dir_all(&d);
}

/// A credential is ONE line — refused before any store is touched.
#[test]
fn a_multiline_value_is_refused_first() {
    let e = journal::refuse_multiline(&[("K".to_string(), "a\nb".to_string())]).unwrap_err();
    assert_eq!(e.kind(), std::io::ErrorKind::InvalidInput);
    assert!(journal::refuse_multiline(&[("K".to_string(), "ab".to_string())]).is_ok());
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
