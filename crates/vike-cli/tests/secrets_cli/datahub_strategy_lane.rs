//! `vike-cli datahub setup --strategy-lane`, asserted over the shipped binary: it MINTS the datahub's
//! strategy-lane key (`PLATFORM_KEYS[9]`) ALONE, leaves a datahub pair that is already minted
//! byte-identical, refuses to replace the lane key without `--rotate`, refuses an absent store
//! without creating it, and no key VALUE reaches either stream or the ledger. Without the flag the
//! verb mints the pair and never the lane key.
//!
//! ⚠ Hub key NAMES are taken from `vike_venues::credential_keys::PLATFORM_KEYS` rather than
//! spelled: an env-shaped literal in this tree is read by the settings registry's harvest as a read.

use std::process::{Command, Output, Stdio};

use vike_venues::credential_keys::PLATFORM_KEYS;

use super::support::{SetCase, exit_code, rung};
use super::{BIN, stderr, stdout};

const DH_OBSERVE: usize = 2;
const DH_CONTROL: usize = 3;
const DH_LANE: usize = 9;

/// `vike-cli <args…>` against `c`'s settings directory.
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

/// Every hub-key row a store holds, values included — for COMPARING, never for printing.
fn node_rows(c: &SetCase) -> std::collections::BTreeMap<String, String> {
    c.credential_store()
        .hub_keys(|_| true)
        .map(|r| r.secrets.into_map().into_iter().collect())
        .unwrap_or_default()
}

/// A project holding an EMPTY store, made the way an operator makes one.
fn project(tag: &str) -> SetCase {
    let c = SetCase::new(tag);
    ok(&cli(&c, &["secrets", "init"]));
    c
}

/// No value of `rows` appears in `text`, whole or by its first sixteen characters.
fn carries_no_value(text: &str, rows: &std::collections::BTreeMap<String, String>, what: &str) {
    for (name, value) in rows {
        assert!(!text.contains(value.as_str()), "{what} carries the VALUE of {name}");
        if value.len() >= 16 {
            assert!(!text.contains(&value[..16]), "{what} carries a PREFIX of {name}");
        }
    }
}

/// **`--strategy-lane` mints ONE name, as a 256-bit-shaped value, over a pair that is already
/// minted and in use** — the pair is byte-identical afterwards, nothing else is written, only names
/// and ids are printed, and ONE ledger line of names is recorded.
#[test]
fn the_lane_key_is_minted_alone_and_the_pair_is_untouched() {
    let c = project("dh-lane");
    ok(&cli(&c, &["datahub", "setup"]));
    let pair = node_rows(&c);
    assert_eq!(pair.len(), 2, "the fixture holds the datahub pair");
    assert!(!pair.contains_key(PLATFORM_KEYS[DH_LANE]), "the plain mint never writes the lane key");
    let records = c.journal_lines().len();

    let out = cli(&c, &["datahub", "setup", "--strategy-lane"]);
    ok(&out);

    let rows = node_rows(&c);
    assert_eq!(rows.len(), 3, "exactly one name was added");
    for name in [PLATFORM_KEYS[DH_OBSERVE], PLATFORM_KEYS[DH_CONTROL]] {
        assert!(rows.get(name) == pair.get(name), "{name}: the pair changed");
    }
    let lane = &rows[PLATFORM_KEYS[DH_LANE]];
    assert_eq!(lane.trim().len(), 64, "a 256-bit key is 64 hex characters");
    assert!(
        !pair.values().any(|v| v == lane),
        "the lane key is a key of its own, never a value reused from the pair"
    );

    let (so, se) = (stdout(&out), stderr(&out));
    assert!(so.contains(PLATFORM_KEYS[DH_LANE]) && so.contains("id "), "{so}");
    assert!(
        !so.contains(PLATFORM_KEYS[DH_OBSERVE]) && !so.contains(PLATFORM_KEYS[DH_CONTROL]),
        "{so}"
    );
    carries_no_value(&so, &rows, "stdout");
    carries_no_value(&se, &rows, "stderr");

    let lines = c.journal_lines();
    let writes: Vec<&String> = lines.iter().filter(|l| l.contains("credential_write")).collect();
    assert_eq!(writes.len(), 2, "the pair's mint and the lane's mint: two records");
    assert_eq!(lines.len(), records + 1, "the lane mint is ONE record");
    assert!(writes[1].contains(PLATFORM_KEYS[DH_LANE]), "the record names the lane key");
    assert!(
        !writes[1].contains(PLATFORM_KEYS[DH_OBSERVE])
            && !writes[1].contains(PLATFORM_KEYS[DH_CONTROL]),
        "the lane mint records no pair name"
    );
    carries_no_value(writes[1], &rows, "the ledger");
}

/// **It needs no pair**: on a store that holds nothing it mints the lane key and nothing else, and the
/// plain verb afterwards mints the pair without touching the lane key.
#[test]
fn the_lane_key_and_the_pair_are_independent_mints() {
    let c = project("dh-lane-first");
    ok(&cli(&c, &["datahub", "setup", "--strategy-lane"]));
    let first = node_rows(&c);
    assert_eq!(first.len(), 1);
    assert!(first.contains_key(PLATFORM_KEYS[DH_LANE]));

    ok(&cli(&c, &["datahub", "setup"]));
    let both = node_rows(&c);
    assert_eq!(both.len(), 3);
    assert!(
        both.get(PLATFORM_KEYS[DH_LANE]) == first.get(PLATFORM_KEYS[DH_LANE]),
        "the plain mint rotated the lane key"
    );
}

/// **A held lane key is a decision, not a re-run**: refused without `--rotate`, naming the key, the
/// store byte-identical afterwards; `--rotate` replaces ONLY the lane key, journalled once.
#[test]
fn a_held_lane_key_is_refused_without_rotate_and_rotate_replaces_only_it() {
    let c = project("dh-lane-rotate");
    ok(&cli(&c, &["datahub", "setup"]));
    ok(&cli(&c, &["datahub", "setup", "--strategy-lane"]));
    let first = node_rows(&c);
    let records = c.journal_lines().len();
    let bytes = std::fs::read(c.store()).unwrap();

    let refused = cli(&c, &["datahub", "setup", "--strategy-lane"]);
    failed(&refused);
    let err = stderr(&refused);
    assert!(err.contains(PLATFORM_KEYS[DH_LANE]) && err.contains("--rotate"), "{err}");
    carries_no_value(&err, &first, "the refusal");
    assert!(node_rows(&c) == first, "a refused mint changed a key");
    assert!(std::fs::read(c.store()).unwrap() == bytes, "a refused mint wrote the store");
    assert_eq!(c.journal_lines().len(), records, "a refusal records nothing");

    let rotated = cli(&c, &["datahub", "setup", "--strategy-lane", "--rotate"]);
    ok(&rotated);
    let second = node_rows(&c);
    assert_ne!(first[PLATFORM_KEYS[DH_LANE]], second[PLATFORM_KEYS[DH_LANE]], "not rotated");
    for name in [PLATFORM_KEYS[DH_OBSERVE], PLATFORM_KEYS[DH_CONTROL]] {
        assert!(
            first.get(name) == second.get(name),
            "{name}: --rotate on the lane key moved the pair"
        );
    }
    carries_no_value(&stdout(&rotated), &first, "stdout (old)");
    carries_no_value(&stdout(&rotated), &second, "stdout (new)");
    assert_eq!(c.journal_lines().len(), records + 1, "--rotate is ONE journalled write");
}

/// **An ABSENT store is refused, naming the one creator, and none is created.**
#[test]
fn an_absent_store_is_refused_and_not_created() {
    let c = SetCase::new("dh-lane-nostore");
    let out = cli(&c, &["datahub", "setup", "--strategy-lane"]);
    failed(&out);
    assert!(stderr(&out).contains("secrets init"), "{}", stderr(&out));
    assert!(!c.store().exists(), "the mint CREATED the store");
    assert!(c.journal_lines().is_empty());
}

/// **`secrets set` and `secrets delete` route the lane key to the command that owns it.**
#[test]
fn set_and_delete_name_the_lane_keys_own_command() {
    let c = project("dh-lane-route");
    ok(&cli(&c, &["datahub", "setup", "--strategy-lane"]));
    let before = std::fs::read(c.store()).unwrap();
    let key = PLATFORM_KEYS[DH_LANE];

    let set = cli(&c, &["secrets", "set", key]);
    let err = stderr(&set);
    assert_eq!(exit_code(&set), rung(vike_cli::exit::Exit::Usage), "{err}");
    assert!(err.contains("datahub setup --strategy-lane"), "{err}");
    assert!(!err.contains("backend setup") && !err.contains("gatewayhub setup"), "{err}");

    let del = cli(&c, &["secrets", "delete", key, "--confirm", key]);
    let err = stderr(&del);
    assert_eq!(exit_code(&del), rung(vike_cli::exit::Exit::Usage), "{err}");
    assert!(err.contains("datahub setup --strategy-lane --rotate"), "{err}");
    assert!(std::fs::read(c.store()).unwrap() == before, "a refusal wrote the store");
}
