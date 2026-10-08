use super::*;

#[test]
fn parses_pairs_comments_and_quotes() {
    let m = parse_credential_file(
        "# a comment\n\nA=1\n B = two \nC=\"quoted\"\nD='single'\nnot a pair\nE=has=equals\n",
    );
    assert_eq!(m.get("A").map(String::as_str), Some("1"));
    assert_eq!(m.get("B").map(String::as_str), Some("two"));
    assert_eq!(m.get("C").map(String::as_str), Some("quoted"));
    assert_eq!(m.get("D").map(String::as_str), Some("single"));
    assert_eq!(m.get("E").map(String::as_str), Some("has=equals"));
    assert!(!m.contains_key("not a pair"));
}

/// A COMMENTED-OUT credential is the case the `||` in the skip guard exists for: under `&&` a
/// rotated-out key comes back as an entry named `"# BINANCE_LIVE_API_KEY"`, and the carry would
/// file a retired credential as a row.
#[test]
fn a_commented_out_credential_stays_out_of_the_map() {
    let m = parse_credential_file("LIVE=in-force\n# RETIRED=old-key-material\n#ALSO_RETIRED=x\n");
    assert_eq!(m.get("LIVE").map(String::as_str), Some("in-force"));
    assert!(!m.contains_key("# RETIRED"), "a comment is not a key: {:?}", m.keys());
    assert!(!m.contains_key("RETIRED"), "and it is certainly not the key it names");
    assert!(!m.contains_key("#ALSO_RETIRED"), "with or without the space after the #");
    assert_eq!(m.len(), 1, "{:?}", m.keys());
}

/// The carry's three arms: present → parsed, absent → empty, unreadable → LOUD. A DIRECTORY where
/// the file should be is the portable stand-in for an unreadable file (`chmod 000` proves nothing
/// when the test runs as root, which CI does).
#[test]
fn the_carry_reads_a_present_file_skips_an_absent_one_and_refuses_an_unreadable_one() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("secrets.env");
    assert!(read_credential_file(&file).unwrap().is_empty(), "absent is nothing to carry");

    std::fs::write(&file, "# kept\nA=1\n").unwrap();
    let before = std::fs::read(&file).unwrap();
    assert_eq!(read_credential_file(&file).unwrap().get("A").map(String::as_str), Some("1"));
    assert_eq!(std::fs::read(&file).unwrap(), before, "the carry must never rewrite the file");

    let unreadable = dir.path().join("node.env");
    std::fs::create_dir_all(&unreadable).unwrap();
    let e = read_credential_file(&unreadable).expect_err("unreadable must not carry as empty");
    assert_eq!(e.path, unreadable);
}
