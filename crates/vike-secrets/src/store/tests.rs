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

/// The store is read, byte for byte, and never rewritten.
#[test]
fn an_existing_store_is_parsed_and_left_alone() {
    let d = tmpdir("read");
    let store = d.join("settings").join("secrets.env");
    put(&store, "from-the-project");

    let r = resolve(&store).unwrap();
    assert_eq!(r.source, Source::File(store.clone()));
    assert_eq!(value(&r), "from-the-project");
    assert!(std::fs::read_to_string(&store).unwrap().contains("=from-the-project"));
    let _ = std::fs::remove_dir_all(&d);
}

/// An ABSENT store is an empty map, not an error — the live gate.
#[test]
fn no_store_at_all_is_an_empty_map_not_an_error() {
    let d = tmpdir("absent");
    let r = resolve(&d.join("settings").join("secrets.env")).unwrap();
    assert!(r.secrets.is_empty());
    assert_eq!(r.source, Source::None);
    assert_eq!(r.warning, None);
    assert_eq!(r.legacy, None, "no leftover beside it either");
    let _ = std::fs::remove_dir_all(&d);
}

/// **A leftover `<project>/.env` with NO store is a finding — the silent case.**
///
/// The empty map is unchanged and [`Source::None`] is unchanged: this adds a diagnostic beside
/// the live gate, it does not alter it. What it separates is the two boxes that produce the
/// identical empty map — one with no credentials (correct) and one whose credentials never moved
/// (every venue silently on paper).
#[test]
fn a_leftover_dotenv_beside_the_project_is_a_finding_when_the_store_is_absent() {
    let d = tmpdir("legacy-absent");
    let store = d.join("settings").join("secrets.env");
    std::fs::create_dir_all(store.parent().unwrap()).unwrap();
    std::fs::write(d.join(LEGACY_STORE_FILE), "BINANCE_LIVE_API_KEY=never-printed\n").unwrap();

    let r = resolve(&store).unwrap();
    // The live gate is untouched.
    assert!(r.secrets.is_empty());
    assert_eq!(r.source, Source::None);
    // …and the finding names both files.
    let w = r.legacy.expect("a leftover store must not be silent");
    assert_eq!(w.legacy, d.join(LEGACY_STORE_FILE));
    assert_eq!(w.store, store);
    let msg = w.to_string();
    assert!(msg.contains("stays paper"), "{msg}");
    assert!(msg.contains("chmod 600"), "the finding must say how to fix it: {msg}");
    assert!(msg.contains("EnvironmentFile"), "…and when it is NOT a problem: {msg}");
    assert!(!msg.contains("never-printed"), "the probe must never read the CONTENTS: {msg}");
    let _ = std::fs::remove_dir_all(&d);
}

/// **A `.env` beside a store that LOADED is silent.** It is a systemd `EnvironmentFile` — the CI box's
/// live recorder ships one — and nothing was silent about credentials, because the store answered.
#[test]
fn a_dotenv_beside_a_present_store_is_not_a_finding() {
    let d = tmpdir("legacy-present");
    let store = d.join("settings").join("secrets.env");
    put(&store, "from-the-store");
    std::fs::write(d.join(LEGACY_STORE_FILE), "POLY_PROXY_ENABLED=false\n").unwrap();

    let r = resolve(&store).unwrap();
    assert_eq!(value(&r), "from-the-store");
    assert_eq!(r.legacy, None, "a store that loaded is not a silent transition");
    let _ = std::fs::remove_dir_all(&d);
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
    // ENOTDIR. Without this line, replacing the guard with `true` (absence always established)
    // passes, and a store nobody can stat reports as cleanly absent — the exact conflation the
    // doc above forbids.
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
/// The two must not look the same: an empty map means "this box is not configured", and a
/// permissions bug quietly wearing that answer would look exactly like a correct fresh install
/// while every venue dropped to paper for a completely different reason. A DIRECTORY where the
/// file should be is the portable stand-in for an unreadable file (a `chmod 000` proves nothing
/// when the test runs as root, which CI does).
#[test]
fn an_unreadable_store_errors_instead_of_reporting_no_credentials() {
    let d = tmpdir("unreadable");
    let store = d.join("secrets.env");
    std::fs::create_dir_all(&store).unwrap();

    let e = resolve(&store).expect_err("an unopenable store must not read as `not configured`");
    assert_eq!(e.path, store);
    assert!(e.to_string().contains("could not be read"), "{e}");
    let _ = std::fs::remove_dir_all(&d);
}

/// The project entry point resolves through the settings-directory override, with no
/// environment read anywhere in this crate.
#[test]
fn the_project_entry_point_honours_an_explicit_settings_dir() {
    let d = tmpdir("project");
    let settings = d.join("settings");
    put(&settings.join("secrets.env"), "deployed");

    let r = resolve_project(settings.to_str()).unwrap();
    assert_eq!(r.source, Source::File(settings.join("secrets.env")));
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
    put(&a.join("secrets.env"), "first");
    put(&b.join("secrets.env"), "second");

    assert_eq!(value(&resolve_project(a.to_str()).unwrap()), "first");
    assert_eq!(value(&resolve_project(b.to_str()).unwrap()), "second");
    let _ = std::fs::remove_dir_all(&a);
    let _ = std::fs::remove_dir_all(&b);
}

/// **The permission warning fires on a too-open store and not on 0600.**
///
/// `0o640` (group-readable) and `0o604` (other-readable) are the two shapes a `cp` or a shared
/// deploy actually produces; `0o620` proves the check is not read-only — a group-WRITABLE
/// credential file lets somebody substitute the keys an order is signed with.
#[cfg(unix)]
#[test]
fn a_group_or_world_accessible_store_warns_but_still_loads() {
    use std::os::unix::fs::PermissionsExt;
    let d = tmpdir("perm-warn");
    let store = d.join("secrets.env");
    // A value nothing else in this test spells, so the "never prints a value" assert is real.
    put(&store, "s3cr3t-key-material");

    for mode in [0o644u32, 0o640, 0o604, 0o620, 0o666] {
        std::fs::set_permissions(&store, std::fs::Permissions::from_mode(mode)).unwrap();
        let r = resolve(&store).unwrap();
        // It LOADS — a finding is never a refusal.
        assert_eq!(value(&r), "s3cr3t-key-material", "mode {mode:04o} must still load");
        let Some(w) = r.warning else { panic!("mode {mode:04o} must warn") };
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
    let r = resolve(&store).unwrap();
    assert_eq!(r.warning, None, "an owner-only store must not warn");
    assert_eq!(value(&r), "s3cr3t-key-material");
    let _ = std::fs::remove_dir_all(&d);
}

/// **A SYMLINKED store is reported — and the check is the one that can see it.**
///
/// The mode question was asked through `std::fs::metadata`, which FOLLOWS the link, so the
/// answer described a file at a path the operator was never shown: an owner-only target
/// reported clean while the path itself said nothing about where the credentials actually
/// live, who owns that directory, or who can replace the file in it. `symlink_metadata` is
/// what makes the indirection visible.
///
/// The target here is 0600 on purpose — that is exactly the case the follow-the-link check
/// calls clean and returns `None` for.
///
/// ⚠ Still a FINDING, never a refusal: one shared credential file symlinked into several
/// project checkouts is a legitimate setup (it is what the retired `~/.vike/secrets.env` slot
/// existed for), so the store must keep loading. Refusing it would strand that operator with
/// every venue on paper.
#[cfg(unix)]
#[test]
fn a_symlinked_store_is_reported_even_when_the_file_it_points_at_is_owner_only() {
    use std::os::unix::fs::PermissionsExt;
    let d = tmpdir("perm-symlink");
    let real = d.join("shared-secrets.env");
    put(&real, "s3cr3t-key-material");
    std::fs::set_permissions(&real, std::fs::Permissions::from_mode(0o600)).unwrap();
    let link = d.join("secrets.env");
    std::os::unix::fs::symlink(&real, &link).unwrap();

    let w = permission_warning(&link)
        .expect("a symlinked credential store must be reported, whatever its target's mode");
    assert_eq!(w.path, link);
    // The LINK-vs-TARGET contract, which nothing asserted: the reported mode is the FOLLOWED
    // target's 0o600 — not the link's own kernel-ignored 0o777, and not a raw `st_mode` with
    // its file-type bits still on (0o100600). A sweep replaced the `& 0o777` mask with `|` and
    // with `^` and the whole vike-secrets suite stayed green, because every assertion here was
    // about the MESSAGE and none about the number in it. (This is not an exposure inversion:
    // the `Symlink` finding fires whatever the mode, so nothing was ever silenced.)
    assert_eq!(
        w.finding,
        Finding::Symlink { target: Some(real.clone()), target_mode: Some(0o600) },
        "the reported mode is the target's, masked to the permission bits"
    );
    let msg = w.to_string();
    assert!(
        msg.to_lowercase().contains("symlink"),
        "the finding must say the path is a symlink: {msg}"
    );
    assert!(
        msg.contains(&real.display().to_string()),
        "the finding must name where the credentials actually are: {msg}"
    );
    assert!(
        !msg.contains("s3cr3t-key-material"),
        "the finding must never print a credential VALUE: {msg}"
    );

    // A finding is never a refusal — the store still loads through the link.
    let r = resolve(&link).unwrap();
    assert_eq!(value(&r), "s3cr3t-key-material", "a symlinked store must still load");
    assert!(r.warning.is_some(), "…and `resolve` must surface the same finding");
    let _ = std::fs::remove_dir_all(&d);
}
