//! `copy-node-keys`, asserted over the shipped binary: the copy lands whole, the source is never
//! written (byte for byte), a differing destination is refused by NAME, `--replace` rotates and is
//! journalled by name, `--dry-run` writes nothing, and no key VALUE reaches either stream or the
//! ledger.
//!
//! ⚠ Node key NAMES are taken from `vike_model::credential_keys::PLATFORM_KEYS` rather than spelled:
//! an env-shaped literal in this tree is read by the settings registry's harvest as a read.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use vike_model::credential_keys::PLATFORM_KEYS;

use super::support::{SetCase, exit_code, rung};
use super::{BIN, stderr, stdout};

const TH_OBSERVE: usize = 0;
const TH_CONTROL: usize = 1;
const DH_OBSERVE: usize = 2;
const DH_CONTROL: usize = 3;

/// `vike-cli <args…>` against `c`'s settings directory — any verb, not just `secrets`, because
/// the fixtures MINT with `datahub setup` and create stores with `secrets init`.
fn cli(c: &SetCase, args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .env("VIKE_SETTINGS_DIR", c.settings())
        .env_remove("VIKE_MAX_ORDER_NOTIONAL")
        .env_remove("VIKE_TRADEHUB_MAX_ORDER_NOTIONAL")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("the vike-cli binary must run")
}

fn ok(o: &Output) {
    assert_eq!(exit_code(o), rung(vike_cli::exit::Exit::Ok), "{}\n{}", stdout(o), stderr(o));
}

fn failed(o: &Output) {
    assert_eq!(exit_code(o), rung(vike_cli::exit::Exit::Failed), "{}\n{}", stdout(o), stderr(o));
}

/// Every node-key row a store holds, values included — for COMPARING, never for printing.
fn node_rows(c: &SetCase) -> std::collections::BTreeMap<String, String> {
    vike_secrets::read_table(&c.store(), vike_secrets::Table::NodeKey)
        .map(|m| m.into_map().into_iter().collect())
        .unwrap_or_default()
}

/// Every venue-credential row NAME a store holds.
fn credential_names(c: &SetCase) -> Vec<String> {
    vike_secrets::read_table(&c.store(), vike_secrets::Table::Credential)
        .map(|m| m.keys().map(str::to_string).collect())
        .unwrap_or_default()
}

/// The source project every copy reads: the tradehub pair the fixture writes into its store, the
/// datahub pair MINTED by the real `datahub setup`, and one VENUE credential that must never cross.
fn source_project() -> SetCase {
    let a = SetCase::new("copy-src");
    a.store_rows(&format!(
        "{}=th-observe-from-a\n{}=th-control-from-a\nBINANCE_LIVE_API_KEY=venue-secret-from-a\n",
        PLATFORM_KEYS[TH_OBSERVE], PLATFORM_KEYS[TH_CONTROL]
    ));
    ok(&cli(&a, &["datahub", "setup"]));
    a
}

/// An EMPTY destination store, made the way an operator makes one.
fn empty_destination(tag: &str) -> SetCase {
    let b = SetCase::new(tag);
    ok(&cli(&b, &["secrets", "init"]));
    b
}

fn copy(b: &SetCase, from: &Path, extra: &[&str]) -> Output {
    let from = from.to_str().expect("utf-8");
    let mut args = vec!["secrets", "copy-node-keys", "--from-settings-dir", from];
    args.extend_from_slice(extra);
    cli(b, &args)
}

/// Every file under `dir`, with its bytes — the source's whole on-disk state, so "untouched" is an
/// exact claim (no sidecar, no journal file, no changed byte).
fn snapshot(dir: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&d) else { continue };
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else {
                let bytes = std::fs::read(&p).unwrap_or_default();
                out.push((p, bytes));
            }
        }
    }
    out.sort();
    out
}

/// No value of `rows` appears in `text`. Asserted without ever formatting a value into a message.
fn carries_no_value(text: &str, rows: &std::collections::BTreeMap<String, String>, what: &str) {
    for (name, value) in rows {
        assert!(!text.contains(value.as_str()), "{what} carries the VALUE of {name}");
        // …and no 16-character slice of it either: "never even a partial value".
        if value.len() >= 16 {
            assert!(!text.contains(&value[..16]), "{what} carries a PREFIX of {name}");
        }
    }
}

/// **The copy: B ends up answering with the SAME bytes as A, for every node key, and with nothing
/// else** — no venue credential crosses, the source is untouched byte for byte, the streams carry
/// names and counts only, and the ledger carries one record of names.
#[test]
fn the_copy_lands_identical_keys_and_touches_nothing_else() {
    let a = source_project();
    let b = empty_destination("copy-dst");
    let a_rows = node_rows(&a);
    assert_eq!(a_rows.len(), 4, "the fixture must hold both pairs");
    let before = snapshot(&a.settings());

    let out = copy(&b, &a.settings(), &[]);
    ok(&out);

    let b_rows = node_rows(&b);
    for name in &PLATFORM_KEYS[..4] {
        assert!(a_rows.get(*name) == b_rows.get(*name), "{name}: B does not hold A's value");
    }
    assert!(!b_rows.contains_key(PLATFORM_KEYS[4]), "A has no admin key, so B must not gain one");
    assert!(
        !credential_names(&b).iter().any(|n| n == "BINANCE_LIVE_API_KEY"),
        "a VENUE credential crossed — the verb reads the node_key table only"
    );
    assert_eq!(snapshot(&a.settings()), before, "the SOURCE project changed on disk");

    let (so, se) = (stdout(&out), stderr(&out));
    for name in &PLATFORM_KEYS[..4] {
        assert!(so.contains(name), "stdout must name {name}: {so}");
    }
    assert!(so.contains("4 added"), "{so}");
    carries_no_value(&so, &a_rows, "stdout");
    carries_no_value(&se, &a_rows, "stderr");
    assert!(!so.contains("venue-secret-from-a") && !se.contains("venue-secret-from-a"));

    let lines = b.journal_lines();
    let writes: Vec<&String> = lines.iter().filter(|l| l.contains("credential_write")).collect();
    assert_eq!(writes.len(), 1, "ONE copy ⇒ ONE record: {} lines", lines.len());
    for name in &PLATFORM_KEYS[..4] {
        assert!(writes[0].contains(name), "the record must name {name}");
    }
    carries_no_value(writes[0], &a_rows, "the ledger");
}

/// **An identical destination is a no-op**: exit 0, every name `unchanged`, no new record.
#[test]
fn copying_identical_keys_again_is_a_no_op() {
    let a = source_project();
    let b = empty_destination("copy-same");
    ok(&copy(&b, &a.settings(), &[]));
    let records = b.journal_lines().len();
    let bytes = std::fs::read(b.store()).unwrap();

    let again = copy(&b, &a.settings(), &[]);
    ok(&again);
    assert!(stdout(&again).contains("unchanged"), "{}", stdout(&again));
    assert!(stdout(&again).contains("0 added, 0 replaced, 4 unchanged"), "{}", stdout(&again));
    assert_eq!(b.journal_lines().len(), records, "a no-op must journal nothing");
    assert!(std::fs::read(b.store()).unwrap() == bytes, "a no-op must not write the store");
}

/// **A DIFFERENT value already held is REFUSED, naming it and saying what --replace means — and
/// `--replace` then rotates it, journalled by name.**
#[test]
fn a_differing_destination_is_refused_by_name_and_replace_rotates_it() {
    let a = source_project();
    let b = empty_destination("copy-differs");
    // B serves its OWN datahub pair, minted independently: the same names, different values.
    ok(&cli(&b, &["datahub", "setup"]));
    let b_before = node_rows(&b);
    let a_rows = node_rows(&a);
    let records = b.journal_lines().len();

    let refused = copy(&b, &a.settings(), &[]);
    failed(&refused);
    assert!(
        !stdout(&refused).lines().any(|l| l.trim_end().ends_with("  added")),
        "a REFUSED run must not report a name as added: {}",
        stdout(&refused)
    );
    let err = stderr(&refused);
    assert!(err.contains(PLATFORM_KEYS[DH_OBSERVE]) && err.contains(PLATFORM_KEYS[DH_CONTROL]));
    assert!(err.contains("--replace") && err.contains("ROTATES"), "{err}");
    assert!(err.contains("Nothing was written"), "{err}");
    carries_no_value(&err, &a_rows, "the refusal");
    carries_no_value(&err, &b_before, "the refusal");
    carries_no_value(&stdout(&refused), &a_rows, "stdout");
    carries_no_value(&stdout(&refused), &b_before, "stdout");
    assert!(
        node_rows(&b) == b_before,
        "a refused copy wrote something — the pair may be HALF-copied"
    );
    assert_eq!(b.journal_lines().len(), records, "a refusal records nothing");

    let rotated = copy(&b, &a.settings(), &["--replace"]);
    ok(&rotated);
    let b_after = node_rows(&b);
    for name in &PLATFORM_KEYS[..4] {
        assert!(
            a_rows.get(*name) == b_after.get(*name),
            "{name}: --replace did not land A's value"
        );
    }
    assert!(stdout(&rotated).contains("REPLACED"), "{}", stdout(&rotated));
    carries_no_value(&stdout(&rotated), &a_rows, "stdout");
    carries_no_value(&stderr(&rotated), &a_rows, "stderr");
    let lines = b.journal_lines();
    assert_eq!(lines.len(), records + 1, "--replace is ONE journalled write");
    let last = lines.last().unwrap();
    assert!(last.contains(PLATFORM_KEYS[DH_OBSERVE]) && last.contains(PLATFORM_KEYS[DH_CONTROL]));
    carries_no_value(last, &a_rows, "the ledger");
    carries_no_value(last, &b_before, "the ledger");
}

/// **`--dry-run` names what would happen and writes nothing** — the store's bytes and the ledger.
#[test]
fn dry_run_writes_nothing() {
    let a = source_project();
    let b = empty_destination("copy-dry");
    let bytes = std::fs::read(b.store()).unwrap();
    let before = snapshot(&a.settings());

    let out = copy(&b, &a.settings(), &["--dry-run"]);
    ok(&out);
    assert!(stdout(&out).contains("would be added"), "{}", stdout(&out));
    assert!(stdout(&out).contains("nothing was written"), "{}", stdout(&out));
    assert!(node_rows(&b).is_empty(), "a dry run wrote a node key");
    assert!(std::fs::read(b.store()).unwrap() == bytes, "a dry run changed the store's bytes");
    assert!(b.journal_lines().is_empty(), "a dry run journalled");
    assert_eq!(snapshot(&a.settings()), before, "the SOURCE project changed on disk");
}

/// **`--only` narrows to one service's keys.**
#[test]
fn only_copies_one_services_keys() {
    let a = source_project();
    let b = empty_destination("copy-only");
    ok(&copy(&b, &a.settings(), &["--only", "tradehub"]));
    let rows = node_rows(&b);
    assert!(
        rows.contains_key(PLATFORM_KEYS[TH_OBSERVE])
            && rows.contains_key(PLATFORM_KEYS[TH_CONTROL])
    );
    assert!(!rows.contains_key(PLATFORM_KEYS[DH_OBSERVE]), "--only tradehub copied a datahub key");

    let bad = copy(&b, &a.settings(), &["--only", "everything"]);
    assert_eq!(exit_code(&bad), rung(vike_cli::exit::Exit::Usage), "{}", stderr(&bad));
}

/// ⚠ **A MISSING source is refused loudly — and NOTHING is created there.** This is the test the
/// read-only open is held by: a reader that could open the source for writing would bring a
/// database into existence at the source path, and the second assertion is what sees it.
#[test]
fn a_missing_source_is_refused_loudly_and_nothing_is_created_there() {
    let empty = SetCase::new("copy-nosrc");
    let b = empty_destination("copy-nosrc-dst");

    let out = copy(&b, &empty.settings(), &[]);
    failed(&out);
    assert!(stderr(&out).contains("no settings database"), "{}", stderr(&out));
    assert!(!empty.store().exists(), "the copy CREATED a database at the source");
    assert!(!empty.settings().join("db").exists(), "the copy created the source's db/ directory");
    assert!(node_rows(&b).is_empty());
    assert!(b.journal_lines().is_empty());
}

/// **A source that is not a settings store is an ERROR, never "no keys"** — a file that is not a
/// database, and an empty file (a valid SQLite database carrying no schema version). Neither is
/// written.
#[test]
fn a_foreign_source_is_refused_loudly_and_left_untouched() {
    let b = empty_destination("copy-foreign-dst");
    for (tag, bytes) in [("garbage", &b"this is not a database at all"[..]), ("zero", &b""[..])] {
        let src = SetCase::new(&format!("copy-foreign-{tag}"));
        std::fs::create_dir_all(src.settings().join("db")).unwrap();
        std::fs::write(src.store(), bytes).unwrap();
        let before = snapshot(&src.settings());

        let out = copy(&b, &src.settings(), &[]);
        failed(&out);
        assert!(stderr(&out).contains("could not be read"), "{tag}: {}", stderr(&out));
        assert!(!stderr(&out).contains("none of the node keys"), "{tag}: read as 'no keys'");
        assert_eq!(snapshot(&src.settings()), before, "{tag}: the source changed on disk");
    }
    assert!(node_rows(&b).is_empty());
}

/// **The source cannot be the destination** — by the same path, and by a path that only resolves
/// to it.
#[test]
fn the_source_cannot_be_the_destination() {
    let b = source_project();
    let before = node_rows(&b);
    for from in [b.settings(), b.settings().join("..").join("settings")] {
        let out = copy(&b, &from, &[]);
        failed(&out);
        assert!(stderr(&out).contains("same database"), "{}", stderr(&out));
    }
    assert!(node_rows(&b) == before);
}

/// **The operand is a DIRECTORY**: naming the database FILE (or any file) is refused, so no file
/// is ever an input to this verb.
#[test]
fn a_file_is_not_a_settings_directory() {
    let a = source_project();
    let b = empty_destination("copy-file");
    let out = copy(&b, &a.store(), &[]);
    failed(&out);
    assert!(stderr(&out).contains("is not a directory"), "{}", stderr(&out));
    assert!(node_rows(&b).is_empty());
}

/// **An ABSENT destination store is refused, and none is created** — the shared node-key writer
/// rule, naming the one creator.
#[test]
fn an_absent_destination_is_refused_and_not_created() {
    let a = source_project();
    let b = SetCase::new("copy-nodst");
    let out = copy(&b, &a.settings(), &[]);
    failed(&out);
    assert!(stderr(&out).contains("secrets init"), "{}", stderr(&out));
    assert!(!b.store().exists(), "the copy CREATED the destination store");
}
