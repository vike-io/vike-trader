//! A project with a store: the settings DATABASE answers, and every verb names it.

use std::assert_matches;
use std::process::{Command, Stdio};

use super::support::Project;
use super::{BIN, Case, SAMPLE, stderr, stdout};

// ---- the settings DATABASE answers, and the SHIPPED binary says so ------------------------------
//
// `docs/decisions/0054`'s credential half made `<project>/settings/db/vike.db` the credential store,
// and since 2026-10-07 the only one. Everything below drives the REAL `vike-cli` binary against a
// REAL project whose store is that database, because the defect these close was a CLI that reported
// on a file: a path from a dead store beside a key count read out of it.

/// **`secrets list` reads the DATABASE and names it.**
#[test]
fn list_reads_the_database() {
    let p = Project::new("list");
    p.write_store(SAMPLE);

    let out = p.secrets(&["list"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let (text, err) = (stdout(&out), stderr(&out));

    assert!(text.contains("DATABASE"), "the source line must name the store that answered: {text}");
    assert!(text.contains("vike.db"), "…and its path: {text}");
    assert!(text.contains("BINANCE_LIVE_API_KEY"), "…while the stored keys are there: {text}");

    // The rule that outranks the rest: nothing here read a value out loud.
    for value in ["sup3r-s3cr3t-value", "key-abcd1234"] {
        assert!(
            !text.contains(value) && !err.contains(value),
            "a VALUE reached a stream: {text}{err}"
        );
    }
}

/// **`secrets path` names the database when it answers.**
///
/// `path` is the command an operator runs FIRST, before they know what is wrong, and it is the one
/// that answers *which store are my keys actually coming from*.
///
/// ⚠ It still OPENS NOTHING: the backend choice is one `is_file` on one path, the same probe every
/// reader makes.
#[test]
fn path_names_the_database_when_it_answers() {
    let p = Project::new("path");
    p.write_store(SAMPLE);

    let out = p.secrets(&["path"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let text = stdout(&out);
    assert!(text.contains("vike.db"), "the database must be named: {text}");
    assert!(text.contains("present"), "{text}");
    assert!(
        !text.contains("secrets init"),
        "the create-a-store hint is a lie on a box with a store: {text}"
    );
}

/// **A project with NO store: `secrets path` names the database that is missing** and how to make
/// it.
#[test]
fn path_on_a_project_with_no_store_names_the_missing_database() {
    let p = Project::new("path-plain");

    let out = p.secrets(&["path"]);
    let (text, err) = (stdout(&out), stderr(&out));
    assert!(out.status.success(), "{err}");
    assert!(text.contains("vike.db") && text.contains("absent"), "{text}");
    assert!(text.contains("secrets init"), "the creator must be named: {text}");
    assert!(!p.db().exists(), "a READ verb created a store");
}

/// **`secrets set` writes the DATABASE.**
///
/// The proof is read back through the library's resolver — the thing a daemon calls — rather than
/// by inspecting the row store.
#[test]
fn set_writes_the_store_that_answers() {
    let p = Project::new("set");
    p.write_store(SAMPLE);

    let mut cmd = Command::new(BIN);
    cmd.arg("secrets")
        .args(["set", "BINANCE_LIVE_API_KEY"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env("VIKE_SETTINGS_DIR", p.settings());
    let mut child = cmd.spawn().expect("spawn");
    {
        use std::io::Write;
        child
            .stdin
            .as_mut()
            .expect("stdin")
            .write_all(b"rotated-through-the-cli\n")
            .expect("write stdin");
    }
    let out = child.wait_with_output().expect("wait");
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(stdout(&out).contains("replaced"), "{}", stdout(&out));
    assert!(stdout(&out).contains("vike.db"), "it must say WHERE it landed: {}", stdout(&out));

    // …and the new value is what the reader sees. `list` prints NAMES only, so the round trip is
    // asserted through the library's resolver, which is the thing a daemon calls.
    let resolved =
        vike_secrets::resolve_project(Some(p.settings().to_str().expect("utf-8"))).expect("read");
    assert_matches!(resolved.source, vike_secrets::Source::Database(_), "{:?}", resolved.source);
    assert_eq!(
        resolved.secrets.clone().into_map().remove("BINANCE_LIVE_API_KEY").as_deref(),
        Some("rotated-through-the-cli"),
        "the write did not reach the store that answers"
    );
}

/// **`config check`'s store row reports the store that answers**, and COUNTS it.
#[test]
fn config_check_reports_the_database() {
    let p = Project::new("check");
    p.write_store(SAMPLE);

    let out = p.config_check();
    let text = format!("{}{}", stdout(&out), stderr(&out));
    assert!(text.contains("vike.db"), "the row must name the store that answered: {text}");
    assert!(text.contains("3 key(s)"), "…and COUNT it: {text}");
    assert!(!text.contains("key-abcd1234"), "a VALUE reached a stream: {text}");
}

/// **`config show`'s PROVENANCE names the store that answered.**
///
/// `config show` exists to answer *where did this value come from*, and it once answered with a
/// file: the header's store row labelled with the file's name beside a key count read out of the
/// database, and a precedence line advertising a file nothing consulted. That is the failure
/// `docs/decisions/0054-settings-move-into-one-database.md`'s constraint 2 names — positive
/// confirmation of something false — in the one command whose entire product is provenance.
#[test]
fn config_show_names_the_database() {
    let p = Project::new("show");
    p.write_store(SAMPLE);

    let out = p.config_show(&["--filter", "BINANCE"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let text = format!("{}{}", stdout(&out), stderr(&out));

    assert!(text.contains("vike.db"), "the header must name the store that answered: {text}");
    assert!(
        text.contains("precedence: env > vike.db > default"),
        "the precedence line advertises a store; it must advertise the live one: {text}"
    );
    assert!(
        text.contains("the settings DATABASE, not a text file"),
        "…and say what the operator cannot `cat`: {text}"
    );
    assert!(text.contains("3 key(s)"), "the count must be the DATABASE's: {text}");
    assert!(!text.contains("sup3r-s3cr3t-value"), "a VALUE reached a stream: {text}");
}

/// …and the SOURCE column, which is the cell an operator actually reads.
#[test]
fn config_show_attributes_a_value_to_the_database_that_holds_it() {
    let p = Project::new("show-json");
    p.write_store(SAMPLE);

    let out = p.config_show(&["--json", "--filter", "BINANCE_LIVE_API_KEY"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let doc: serde_json::Value = serde_json::from_str(&stdout(&out)).expect("valid JSON");

    assert_eq!(doc["secrets"]["kind"], "database", "{}", doc["secrets"]);
    assert!(
        doc["secrets"]["path"].as_str().expect("a path").ends_with("vike.db"),
        "{}",
        doc["secrets"]
    );

    let rows = doc["env"].as_array().expect("the env half");
    let row = rows
        .iter()
        .find(|r| r["name"] == "BINANCE_LIVE_API_KEY")
        .unwrap_or_else(|| panic!("no row for the key the database holds: {rows:#?}"));
    assert_eq!(row["source"], "database", "the row must name the store that answered: {row}");
    // …and the rule that outranks every other one here.
    assert_eq!(row["value"], "<set>", "a credential row discloses presence, never a value: {row}");
    assert!(!stdout(&out).contains("key-abcd1234"), "a VALUE reached the document");
}

/// **A project with NO store: `config show` names the database as the store, absent.**
#[test]
fn config_show_on_a_project_with_no_store_names_the_missing_database() {
    let p = Project::new("show-plain");

    let out = p.config_show(&["--filter", "BINANCE"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let text = format!("{}{}", stdout(&out), stderr(&out));
    assert!(text.contains("vike.db"), "{text}");
    assert!(text.contains("absent"), "{text}");
}

/// **`secrets set` records the store the key LANDED in.**
///
/// ⚠ The command once knew the answer and did not use it: `run_set` computed `where_it_landed` from
/// the `Backend` the writer returned, printed it in the success sentence, and handed the change
/// journal a FILE path from three lines earlier — an append-only ledger asserting a store the write
/// never touched, which is worse than one asserting nothing, because it is the record an incident
/// is reconstructed from.
///
/// The three node verbs (`node/setup.rs`, `node/connect.rs`, `datahub.rs`) pass `node::landed` and
/// have been right since they were written; one of two sibling paths had the rule, which is the
/// shape this repo has learned to look for.
#[test]
fn set_journals_the_database_it_wrote() {
    let p = Project::new("set-journal");
    p.write_store(SAMPLE);

    let out = p.secrets_with_stdin(&["set", "BYBIT_DEMO_API_KEY"], "not-a-real-credential\n");
    assert!(out.status.success(), "{}{}", stdout(&out), stderr(&out));
    assert!(
        stdout(&out).contains("vike.db"),
        "the success sentence must name the store written: {}",
        stdout(&out)
    );

    let lines = p.journal_lines();
    assert_eq!(lines.len(), 1, "ONE write ⇒ ONE record: {lines:?}");
    let line = &lines[0];
    assert!(
        line.contains("vike.db"),
        "the ledger's store cell must name the DATABASE the key landed in: {line}"
    );
    assert!(line.contains("BYBIT_DEMO_API_KEY"), "…while still carrying the key NAME: {line}");
    assert!(!line.contains("not-a-real-credential"), "a VALUE reached the ledger: {line}");
}

/// **A shell consumer can match on these bytes with globs, and this pins what it may rely on.**
///
/// ⚠ The consumer this was written for is GONE. `scripts/refuse_live_credentials.sh` — the guard
/// both live-smoke lanes call — used to inspect a migrated store by running `vike-cli secrets list
/// --json` and testing its stdout for `"kind":"database"` and `"keys":[`. Since 2026-10-04 it reads
/// no key name at all: the lane points at the daemon's own settings database, which holds live keys
/// as its normal state, so the guard judges what the lane's tests ASK for and whether the runner
/// could WRITE the store instead (`crates/vike-ops/tests/settings_secrets/smoke_guard_gate.rs`).
///
/// The property is kept because it is cheap and the trap it records is real: this renderer
/// PRETTY-PRINTS, so a glob that matches the raw document misses `"kind": "database"` spread over a
/// dozen lines. That guard's first version did exactly that while every fixture emitted compact
/// bytes and its whole suite was green. With whitespace removed, the document carries both
/// spellings; either rendering satisfies it, and a renamed or dropped field does not.
#[test]
fn the_json_listing_survives_the_flattening_the_smoke_guard_performs() {
    let c = Case::new("list-json-flat");
    c.write_store(SAMPLE);
    let out = c.run_raw(&["list", "--json"]);
    let doc = stdout(&out);

    // A shell consumer's `tr -d '[:space:]'`, spelled in Rust over the same bytes.
    let flat: String = doc.chars().filter(|ch| !ch.is_whitespace()).collect();
    assert!(
        flat.contains("\"kind\":\"database\""),
        "the flattened document must carry the `kind` spelling a glob consumer matches: {doc}"
    );
    assert!(
        flat.contains("\"keys\":["),
        "…and the `keys` array, the field a glob consumer reads names out of: {doc}"
    );
}
