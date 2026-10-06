//! A migrated project: the settings DATABASE answers, and every verb says the file is shadowed.

use std::process::{Command, Stdio};

use super::support::Project;
use super::{BIN, Case, SAMPLE, stderr, stdout};

// ---- the settings DATABASE answers, and the SHIPPED binary says so ------------------------------
//
// `docs/decisions/0054`'s credential half lets `<project>/settings/db/vike.db` answer for the
// credential store. Everything below drives the REAL `vike-cli` binary against a REAL migrated
// project — a `settings/` holding a credential file AND the database that shadows it — because the
// defect these close was that the CLI reported on the FILE: a path from the dead store beside a key
// count read out of it, and not one sentence anywhere saying the file had stopped being read.

/// **`secrets list` reads the DATABASE on a migrated project, and says the file is shadowed.**
///
/// Two defects in one run. `list` handed `vike_secrets::resolve` a path it had built itself, which
/// is the FILE arm by definition — so on a migrated box it printed the dead store's path and the
/// dead store's key count, confidently. And `ShadowedStore`, the finding that exists to tell an
/// operator their hand-edit stopped mattering, was returned as data and printed by NOTHING: a grep
/// for it across `crates/` outside `vike-secrets` found zero consumers.
#[test]
fn list_reads_the_database_and_reports_the_shadowed_file() {
    let p = Project::new("list");
    p.write_store(SAMPLE);
    p.migrate();

    // A key added to the FILE after the migration. It is in the file and not in the database, so a
    // listing that shows it is a listing of the dead store.
    std::fs::write(
        p.store(),
        format!("{SAMPLE}BYBIT_DEMO_API_KEY=added-to-the-file-after-the-migration\n"),
    )
    .expect("append");

    let out = p.secrets(&["list"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let (text, err) = (stdout(&out), stderr(&out));

    assert!(text.contains("DATABASE"), "the source line must name the store that answered: {text}");
    assert!(text.contains("vike.db"), "…and its path: {text}");
    assert!(
        !text.contains("BYBIT_DEMO_API_KEY"),
        "the listing showed a key that exists only in the SHADOWED file: {text}"
    );
    assert!(text.contains("BINANCE_LIVE_API_KEY"), "…while the migrated keys are there: {text}");

    assert!(
        err.contains("NO LONGER READ"),
        "the shadowed file must be REPORTED — this was returned as data and printed by nothing: \
         {err}"
    );
    assert!(err.contains("secrets.env"), "{err}");

    // The rule that outranks the rest: nothing here read a value out loud.
    for value in ["sup3r-s3cr3t-value", "key-abcd1234", "added-to-the-file-after-the-migration"] {
        assert!(
            !text.contains(value) && !err.contains(value),
            "a VALUE reached a stream: {text}{err}"
        );
    }
}

/// **`secrets path` names the database and says the store line is a file nobody reads.**
///
/// `path` is the command an operator runs FIRST, before they know what is wrong, and it is the one
/// that answers *which store are my keys actually coming from*. Printing the file alone on a
/// migrated box made it the thing it exists to prevent.
///
/// ⚠ It still OPENS NOTHING: the backend choice is one `is_file` on one path, the same probe every
/// reader makes.
#[test]
fn path_names_the_database_when_it_answers() {
    let p = Project::new("path");
    p.write_store(SAMPLE);
    p.migrate();

    let out = p.secrets(&["path"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let text = stdout(&out);
    assert!(text.contains("vike.db"), "the database must be named: {text}");
    assert!(text.contains("NO LONGER READ"), "…and the file's status said plainly: {text}");
    assert!(
        !text.contains("chmod 600"),
        "the create-a-file hint is a lie on a migrated box: {text}"
    );
}

/// **An UNMIGRATED project's `secrets path` output is byte-identical to before any of this landed.**
///
/// The property that makes the whole change mergeable, asserted on a PUBLISHED surface rather than
/// only on library behaviour: this verb's output is what operators paste into issues and what
/// runbooks quote. A permanent `db: … (absent)` row on every box would be a change to that surface
/// in exchange for saying nothing.
#[test]
fn path_on_an_unmigrated_project_says_nothing_about_a_database() {
    let p = Project::new("path-plain");
    p.write_store(SAMPLE);

    let out = p.secrets(&["path"]);
    let text = stdout(&out);
    assert!(text.contains("secrets.env"), "{text}");
    assert!(text.contains("present"), "{text}");
    assert!(!text.contains("vike.db"), "no database exists, so none may be mentioned: {text}");
    assert!(!text.contains("db:"), "{text}");
    assert!(!text.contains("NO LONGER READ"), "{text}");
    assert!(!stderr(&out).contains("NO LONGER READ"), "{}", stderr(&out));
}

/// **`secrets set` writes the DATABASE on a migrated project, and the shadowed file does not move.**
///
/// The write half. Before this, `set` read the file to decide replaced-vs-appended, refused an
/// absent FILE, and wrote the FILE — three answers about a store nothing reads. The proof is read
/// back through `list`, i.e. through the resolver a daemon would use, rather than by inspecting the
/// row store.
#[test]
fn set_writes_the_store_that_answers_and_leaves_the_shadowed_file_alone() {
    let p = Project::new("set");
    p.write_store(SAMPLE);
    p.migrate();
    let file_before = std::fs::read(p.store()).expect("read");

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

    // ⚠ THE RULE THAT OUTRANKS THE REST. A write routed past the file must not have touched it.
    assert_eq!(
        std::fs::read(p.store()).expect("read"),
        file_before,
        "the shadowed credential file was modified by a write that did not go to it"
    );

    // …and the new value is what the reader sees. `list` prints NAMES only, so the round trip is
    // asserted through the library's resolver, which is the thing a daemon calls.
    let resolved =
        vike_secrets::resolve_project(Some(p.settings().to_str().expect("utf-8"))).expect("read");
    assert!(matches!(resolved.source, vike_secrets::Source::Database(_)), "{:?}", resolved.source);
    assert_eq!(
        resolved.secrets.clone().into_map().remove("BINANCE_LIVE_API_KEY").as_deref(),
        Some("rotated-through-the-cli"),
        "the write did not reach the store that answers"
    );
}

/// **`config check`'s store row reports the store that answers, and flags the shadowed file.**
///
/// `Source::Database(_)` was folded into the `Source::File(_)` arm, so a migrated box printed the
/// FILE path beside a key count read from the dead store: two wrong halves reading as one confident
/// answer.
#[test]
fn config_check_reports_the_database_and_the_shadowed_file() {
    let p = Project::new("check");
    p.write_store(SAMPLE);
    p.migrate();
    // A key only the FILE has, so a row counting the file is distinguishable from one counting the
    // database.
    std::fs::write(p.store(), format!("{SAMPLE}BYBIT_DEMO_API_KEY=only-in-the-file\n"))
        .expect("append");

    let out = p.config_check();
    let text = format!("{}{}", stdout(&out), stderr(&out));
    assert!(text.contains("vike.db"), "the row must name the store that answered: {text}");
    assert!(text.contains("3 key(s)"), "…and COUNT it, not the shadowed file's 4: {text}");
    assert!(text.contains("NO LONGER READ"), "…and flag the file: {text}");
    assert!(!text.contains("only-in-the-file"), "a VALUE reached a stream: {text}");
}

/// **`config show`'s PROVENANCE names the store that answered.**
///
/// `config show` exists to answer *where did this value come from*, and on a migrated box it
/// answered with the file: the header's store row was labelled `secrets.env` beside a key count
/// read out of the database, the precedence line advertised `env > secrets.env > default` for a
/// file nothing consults, and every store-sourced row printed a flat, confident `dotenv`. That is
/// the failure `docs/decisions/0054-settings-move-into-one-database.md`'s constraint 2 names —
/// positive confirmation of something false — landing in the one command whose entire product is
/// provenance. Its sibling `config check` was repaired first; this is the other half.
#[test]
fn config_show_names_the_database_and_the_file_it_shadows() {
    let p = Project::new("show");
    p.write_store(SAMPLE);
    p.migrate();
    // A key only the FILE has, so a count of the file is distinguishable from a count of the
    // database, and its VALUE is a tripwire for any path that reads the shadowed store.
    std::fs::write(p.store(), format!("{SAMPLE}BYBIT_DEMO_API_KEY=only-in-the-file\n"))
        .expect("append");

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
    assert!(
        text.contains("3 key(s)"),
        "the count must be the DATABASE's, not the shadowed file's 4: {text}"
    );
    assert!(text.contains("NO LONGER READ"), "…and the shadowed file must be flagged: {text}");
    assert!(
        !text.contains("only-in-the-file") && !text.contains("sup3r-s3cr3t-value"),
        "a VALUE reached a stream: {text}"
    );
}

/// …and the SOURCE column, which is the cell an operator actually reads.
#[test]
fn config_show_attributes_a_value_to_the_database_that_holds_it() {
    let p = Project::new("show-json");
    p.write_store(SAMPLE);
    p.migrate();

    let out = p.config_show(&["--json", "--filter", "BINANCE_LIVE_API_KEY"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let doc: serde_json::Value = serde_json::from_str(&stdout(&out)).expect("valid JSON");

    assert_eq!(doc["secrets"]["kind"], "database", "{}", doc["secrets"]);
    assert!(
        doc["secrets"]["path"].as_str().expect("a path").ends_with("vike.db"),
        "{}",
        doc["secrets"]
    );
    assert!(
        doc["secrets"]["shadowed"]["file"]
            .as_str()
            .expect("the shadowed file")
            .ends_with("secrets.env"),
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

/// **An UNMIGRATED project's `config show` is what it always was.** The property that makes this
/// mergeable: the fix must be invisible on every box that has not migrated, because that output is
/// what operators paste into issues and what this command's own regression baseline is built on.
#[test]
fn config_show_on_an_unmigrated_project_says_nothing_about_a_database() {
    let p = Project::new("show-plain");
    p.write_store(SAMPLE);

    let out = p.config_show(&["--filter", "BINANCE"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let text = format!("{}{}", stdout(&out), stderr(&out));
    assert!(text.contains("secrets.env"), "{text}");
    assert!(
        text.contains("precedence: env > secrets.env > default"),
        "the precedence line is unchanged on an unmigrated box: {text}"
    );
    assert!(!text.contains("vike.db"), "no database exists, so none may be mentioned: {text}");
    assert!(!text.contains("NO LONGER READ"), "nothing shadows anything here: {text}");
    assert!(!text.contains("DATABASE"), "{text}");
}

/// **`secrets set` records the store the key LANDED in — not the file path it computed first.**
///
/// ⚠ The command already knew the answer and did not use it: `run_set` computed `where_it_landed`
/// from the `Backend` the writer returned, printed it in the success sentence, and handed the
/// change journal the FILE path from three lines earlier. So on a migrated box every
/// `credential_write` record named `secrets.env` for a key that went into the database — an
/// append-only ledger asserting a store the write never touched, which is worse than one asserting
/// nothing, because it is the record an incident is reconstructed from.
///
/// The three node verbs (`node/setup.rs`, `node/connect.rs`, `datahub.rs`) pass `node::landed` and
/// have been right since they were written; one of two sibling paths had the rule, which is the
/// shape this repo has learned to look for.
#[test]
fn set_journals_the_database_it_wrote_and_not_the_shadowed_file() {
    let p = Project::new("set-journal");
    p.write_store(SAMPLE);
    p.migrate();
    let before = std::fs::read(p.store()).expect("read the store");

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
    assert!(
        !line.contains("secrets.env"),
        "…and must NOT name the shadowed file, which this write never opened: {line}"
    );
    assert!(line.contains("BYBIT_DEMO_API_KEY"), "…while still carrying the key NAME: {line}");
    assert!(!line.contains("not-a-real-credential"), "a VALUE reached the ledger: {line}");

    assert_eq!(
        std::fs::read(p.store()).expect("read the store"),
        before,
        "and the operator's only copy of their keys is untouched by a write that went elsewhere"
    );
}

/// **`secrets move-venue-config` is journalled: ONE `credential_write` record per move that WROTE,
/// carrying the moved credential NAMES and no value.**
///
/// Since decision 0095's Task 7 this verb is the mandatory remedy for a startup refusal, and it
/// DELETES credential rows from the only copy of a box's venue keys — so it records the act the
/// way its sibling writers do (`secrets set`, `secrets migrate`), and a rehearsal, a store with
/// nothing to move and a refusal record nothing, because none of them changed anything.
///
/// ⚠ Names are composed rather than spelled, for the reason `crates/vike-cli/src/cmd/secrets/move_venue.rs`'s
/// fixture gives; the values are markers this test then hunts for in the ledger.
#[test]
fn the_move_is_journalled_once_with_the_names_and_no_value() {
    let host = format!("IBKR_{}", "DEMO_HOST");
    let socks = format!("POLY_{}", "SOCKS_PROXY");
    let p = Project::new("move-journal");
    p.write_store(&format!(
        "IBKR_DEMO_ACCOUNT=DU0000000\n{host}=move-host-marker\n\
         {socks}=socks5h://move:secret-marker@127.0.0.1:1080\n"
    ));
    p.migrate();
    assert!(p.journal_lines().is_empty(), "the fixture's own migration journals nothing");

    let rehearsal = p.secrets(&["move-venue-config", "--dry-run"]);
    assert!(rehearsal.status.success(), "{}{}", stdout(&rehearsal), stderr(&rehearsal));
    assert!(p.journal_lines().is_empty(), "a rehearsal changes nothing and records nothing");

    let out = p.secrets(&["move-venue-config"]);
    assert!(out.status.success(), "{}{}", stdout(&out), stderr(&out));
    assert!(stdout(&out).contains("MOVED"), "{}", stdout(&out));
    let lines = p.journal_lines();
    assert_eq!(lines.len(), 1, "ONE move ⇒ ONE record: {lines:?}");
    let line = &lines[0];
    assert!(line.contains("\"kind\":\"credential_write\""), "{line}");
    assert!(line.contains("vike.db"), "the store cell names the database the rows left: {line}");
    assert!(line.contains(&host) && line.contains(&socks), "…and carries the moved NAMES: {line}");
    assert!(!line.contains("move-host-marker"), "a VALUE reached the ledger: {line}");
    assert!(!line.contains("secret-marker"), "a SECRET value reached the ledger: {line}");

    let again = p.secrets(&["move-venue-config"]);
    assert!(again.status.success(), "{}{}", stdout(&again), stderr(&again));
    assert!(stdout(&again).contains("nothing to move"), "{}", stdout(&again));
    assert_eq!(p.journal_lines().len(), 1, "a store with nothing to move records nothing");

    // A REFUSAL writes nothing and records nothing: two names collapsing onto one key with
    // DIFFERENT values is the divergence the move refuses.
    let r = Project::new("move-journal-refused");
    r.write_store(&format!(
        "DUKASCOPY_{}=https://one.example/a.jnlp\nDUKASCOPY_{}=https://two.example/b.jnlp\n",
        "DEMO1_SERVER", "DEMO2_SERVER"
    ));
    r.migrate();
    let refused = r.secrets(&["move-venue-config"]);
    assert!(!refused.status.success(), "a divergence must refuse: {}", stdout(&refused));
    assert!(r.journal_lines().is_empty(), "a refusal records nothing: {:?}", r.journal_lines());
}

/// **A shell consumer can match on these bytes with globs, and this pins what it may rely on.**
///
/// ⚠ The consumer this was written for is GONE. `scripts/refuse_live_credentials.sh` — the guard
/// both live-smoke lanes call — used to inspect a migrated store by running `vike-cli secrets list
/// --json` and testing its stdout for `"kind":"database"` and `"keys":[`. Since 2026-10-04 it reads
/// no key name at all: the lane points at the daemon's own settings database, which holds live keys
/// as its normal state, so the guard judges what the lane's tests ASK for and whether the runner
/// could WRITE the store instead (`crates/vike-ops/tests/credentials/smoke_guard_gate.rs`).
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
    let out = c.run_raw(&["list", "--file", &c.store().display().to_string(), "--json"]);
    let doc = stdout(&out);

    // A shell consumer's `tr -d '[:space:]'`, spelled in Rust over the same bytes.
    let flat: String = doc.chars().filter(|ch| !ch.is_whitespace()).collect();
    assert!(
        flat.contains("\"kind\":\"file\""),
        "the flattened document must carry the `kind` spelling a glob consumer matches: {doc}"
    );
    assert!(
        flat.contains("\"keys\":["),
        "…and the `keys` array, the field a glob consumer reads names out of: {doc}"
    );
}
