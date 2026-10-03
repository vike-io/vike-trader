use super::*;

/// **The sandbox diagnosis.** `EROFS` from a systemd unit reads as a disk problem; the unit's
/// own `ProtectSystem=strict` is the actual cause, and `ReadWritePaths=` is the actual fix.
/// The message must name BOTH — and the path, so an operator can paste it straight into the
/// unit.
#[test]
fn a_read_only_root_names_readwritepaths_and_the_path() {
    let e = std::io::Error::from(std::io::ErrorKind::ReadOnlyFilesystem);
    let msg = unwritable_store_root(Path::new("/opt/vike/market_data/hist"), &e).to_string();
    assert!(msg.contains("/opt/vike/market_data/hist"), "the path must be quotable: {msg}");
    assert!(msg.contains("ReadWritePaths="), "the fix must be named: {msg}");
    assert!(msg.contains("ProtectSystem=strict"), "the cause must be named: {msg}");
    assert!(msg.contains("VIKE_HIST_STORE"), "the other way out must be named: {msg}");
}

/// The sibling shape — a root owned by another user, or under a `-m700` parent — gets the same
/// treatment: it is the same question ("who is allowed to write here") wearing a different
/// errno.
#[test]
fn a_permission_denied_root_gets_the_same_diagnosis() {
    let e = std::io::Error::from(std::io::ErrorKind::PermissionDenied);
    let msg = unwritable_store_root(Path::new("/srv/tape"), &e).to_string();
    assert!(msg.contains("ReadWritePaths="), "{msg}");
    assert!(msg.contains("/srv/tape"), "{msg}");
}

/// …and every OTHER failure is passed through verbatim. A gate that decorated unrelated errors
/// would send operators to the unit file for a full disk or a bad symlink.
#[test]
fn an_unrelated_io_error_is_not_decorated() {
    let e = std::io::Error::new(std::io::ErrorKind::NotADirectory, "a file is in the way");
    let msg = unwritable_store_root(Path::new("/x"), &e).to_string();
    assert!(!msg.contains("ReadWritePaths="), "unrelated errors must not be decorated: {msg}");
    assert_eq!(msg, io(&e).to_string(), "…and must read exactly as they did before");
}

/// End to end through the real `open`, on a genuinely unwritable parent — proving the
/// decoration is actually WIRED, not merely defined. Skipped when the process can write to a
/// `0o555` directory anyway (running as root, or a filesystem without unix modes).
#[cfg(unix)]
#[test]
fn open_under_an_unwritable_parent_reports_the_sandbox_diagnosis() {
    use std::os::unix::fs::PermissionsExt;
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let parent = std::env::temp_dir().join(format!("vike-ro-store-{nanos}"));
    std::fs::create_dir_all(&parent).unwrap();
    std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o555)).unwrap();

    let root = parent.join("market_data").join("hist");
    let outcome = DataFusionHist::open(&root).err().map(|e| e.to_string());

    // Restore before asserting, so a failure does not leave an unremovable directory behind.
    let _ = std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o755));
    let _ = std::fs::remove_dir_all(&parent);

    match outcome {
        Some(msg) => assert!(
            msg.contains("ReadWritePaths="),
            "an unwritable root must carry the diagnosis: {msg}"
        ),
        // root, or a filesystem ignoring the mode: the fixture proved nothing, so say so.
        None => eprintln!("skipped: this process can write under a 0o555 directory"),
    }
}
