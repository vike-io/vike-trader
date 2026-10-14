//! `vike-cli gatewayhub setup`, asserted over the shipped binary: it MINTS the gatewayhub's pair
//! into the hub-key table and nothing else, refuses to replace a pair without `--rotate`, refuses
//! an absent store without creating it, and no key VALUE reaches either stream or the ledger.
//!
//! ⚠ Hub key NAMES are taken from `vike_venues::credential_keys::PLATFORM_KEYS` rather than
//! spelled: an env-shaped literal in this tree is read by the settings registry's harvest as a read.
//! The group lives in `secrets_cli` because its fixtures (`SetCase`: a project directory, the store
//! read back through the one handle, the ledger's lines) are exactly what a hub-key writer is
//! asserted against, and `copy_hub_keys.rs` already drives the sibling `datahub setup` the same way.

use std::process::{Command, Output, Stdio};

use vike_venues::credential_keys::{PLATFORM_KEYS, is_gatewayhub_daemon_key};

use super::support::{SetCase, exit_code, rung};
use super::{BIN, stderr, stdout};

const GW_OBSERVE: usize = 5;
const GW_CONTROL: usize = 6;

/// `vike-cli <args…>` against `c`'s settings directory — any verb, because the fixtures create the
/// store with `secrets init` before minting into it.
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

/// **The mint writes the gatewayhub's TWO names, as 256-bit-shaped values, and nothing else** — no
/// other hub key, no venue credential — prints names and ids and never a value, and records ONE
/// ledger line of names.
#[test]
fn setup_mints_the_gatewayhub_pair_and_only_that() {
    let c = project("gw-mint");
    let records_before = c.journal_lines().len();

    let out = cli(&c, &["gatewayhub", "setup"]);
    ok(&out);

    let rows = node_rows(&c);
    let mut names: Vec<&str> = rows.keys().map(String::as_str).collect();
    names.sort_unstable();
    let mut want = vec![PLATFORM_KEYS[GW_OBSERVE], PLATFORM_KEYS[GW_CONTROL]];
    want.sort_unstable();
    assert_eq!(names, want, "exactly the gatewayhub's two names");
    for name in &names {
        assert!(is_gatewayhub_daemon_key(name), "{name} is not a gatewayhub name");
    }
    for (name, value) in &rows {
        assert_eq!(value.trim().len(), 64, "{name}: a 256-bit key is 64 hex characters");
    }
    assert_ne!(
        rows[PLATFORM_KEYS[GW_OBSERVE]], rows[PLATFORM_KEYS[GW_CONTROL]],
        "observe and control are two keys, never one value written twice"
    );
    // No venue credential and no account came with it.
    let venue_names: Vec<String> = c
        .credential_store()
        .read_all()
        .map(|r| r.secrets.keys().map(str::to_string).collect())
        .unwrap_or_default();
    assert!(venue_names.is_empty(), "the mint wrote a venue credential: {venue_names:?}");

    let (so, se) = (stdout(&out), stderr(&out));
    for name in &names {
        assert!(so.contains(name), "stdout must name {name}: {so}");
    }
    assert!(so.contains("id "), "stdout carries each key's id: {so}");
    assert!(so.contains("copy-hub-keys") && so.contains("--only gatewayhub"), "{so}");
    carries_no_value(&so, &rows, "stdout");
    carries_no_value(&se, &rows, "stderr");

    let lines = c.journal_lines();
    let writes: Vec<&String> = lines.iter().filter(|l| l.contains("credential_write")).collect();
    assert_eq!(writes.len(), 1, "ONE mint ⇒ ONE record: {} lines", lines.len() - records_before);
    for name in &names {
        assert!(writes[0].contains(name), "the record must name {name}");
    }
    carries_no_value(writes[0], &rows, "the ledger");
}

/// **A pair already held is a decision, not a re-run**: refused without `--rotate`, naming the keys
/// and saying what a restart does to every client, and the store is byte-identical afterwards.
/// `--rotate` then replaces BOTH, journalled once.
#[test]
fn a_held_pair_is_refused_without_rotate_and_rotate_replaces_both() {
    let c = project("gw-rotate");
    ok(&cli(&c, &["gatewayhub", "setup"]));
    let first = node_rows(&c);
    let records = c.journal_lines().len();
    let bytes = std::fs::read(c.store()).unwrap();

    let refused = cli(&c, &["gatewayhub", "setup"]);
    failed(&refused);
    let err = stderr(&refused);
    assert!(err.contains(PLATFORM_KEYS[GW_OBSERVE]) && err.contains(PLATFORM_KEYS[GW_CONTROL]));
    assert!(err.contains("--rotate"), "{err}");
    assert!(err.contains("stops working"), "what a restart does to a client: {err}");
    carries_no_value(&err, &first, "the refusal");
    carries_no_value(&stdout(&refused), &first, "stdout");
    assert!(node_rows(&c) == first, "a refused mint changed a key");
    assert!(std::fs::read(c.store()).unwrap() == bytes, "a refused mint wrote the store");
    assert_eq!(c.journal_lines().len(), records, "a refusal records nothing");

    let rotated = cli(&c, &["gatewayhub", "setup", "--rotate"]);
    ok(&rotated);
    let second = node_rows(&c);
    for name in [PLATFORM_KEYS[GW_OBSERVE], PLATFORM_KEYS[GW_CONTROL]] {
        assert_ne!(first[name], second[name], "{name}: --rotate left the old key in place");
    }
    carries_no_value(&stdout(&rotated), &first, "stdout (old)");
    carries_no_value(&stdout(&rotated), &second, "stdout (new)");
    carries_no_value(&stderr(&rotated), &second, "stderr (new)");
    assert_eq!(c.journal_lines().len(), records + 1, "--rotate is ONE journalled write");
}

/// **The mint is the gatewayhub's alone**: another service's pair held in the same store is neither
/// read as "already minted" nor touched, so `gatewayhub setup` cannot rotate a pair a different
/// service is signing with.
#[test]
fn another_services_pair_is_neither_blocking_nor_touched() {
    let c = project("gw-others");
    ok(&cli(&c, &["datahub", "setup"]));
    let datahub_only = node_rows(&c);
    assert_eq!(datahub_only.len(), 2, "the fixture holds the datahub's pair");

    ok(&cli(&c, &["gatewayhub", "setup"]));
    let both = node_rows(&c);
    assert_eq!(both.len(), 4);
    for (name, value) in &datahub_only {
        assert!(both.get(name) == Some(value), "{name}: the datahub's key changed");
    }
}

/// **An ABSENT store is refused, naming the one creator, and none is created.**
#[test]
fn an_absent_store_is_refused_and_not_created() {
    let c = SetCase::new("gw-nostore");
    let out = cli(&c, &["gatewayhub", "setup"]);
    failed(&out);
    assert!(stderr(&out).contains("secrets init"), "{}", stderr(&out));
    assert!(!c.store().exists(), "the mint CREATED the store");
    assert!(c.journal_lines().is_empty());
}

/// **The grammar has no way to hand the verb a key**: a positional, a value flag and the sibling
/// verb's `control-key` are all usage errors, and none writes anything.
#[test]
fn the_verb_takes_no_key_in_any_form() {
    let c = project("gw-grammar");
    let bytes = std::fs::read(c.store()).unwrap();
    for argv in [
        &["gatewayhub"][..],
        &["gatewayhub", "setup", "deadbeef"][..],
        &["gatewayhub", "setup", "--key", "deadbeef"][..],
        &["gatewayhub", "setup", "--rotate=maybe"][..],
        &["gatewayhub", "control-key"][..],
    ] {
        let out = cli(&c, argv);
        assert_eq!(
            exit_code(&out),
            rung(vike_cli::exit::Exit::Usage),
            "{argv:?}: {}",
            stderr(&out)
        );
    }
    assert!(std::fs::read(c.store()).unwrap() == bytes, "a usage error wrote the store");
    assert!(node_rows(&c).is_empty());
}
