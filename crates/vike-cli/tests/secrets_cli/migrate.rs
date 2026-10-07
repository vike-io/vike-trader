//! `migrate`: the dry run, the apply, its refusals, and `--file` beside a migrated project.

use std::process::{Command, Stdio};

use super::support::{MIGRATE_NODE_STORE, MIGRATE_STORE, Project, SetCase, exit_code, rung};
use super::{BIN, Case, stderr, stdout};

// ── migrate ─────────────────────────────────────────────────────────────────────────────────────
//
// ⚠ Every case drives its own temp settings directory through `$VIKE_SETTINGS_DIR` — the same
// isolation `SetCase` takes, and it is doubly load-bearing here: this verb CREATES a credential
// database inside whatever directory it resolves, so a case that fell through to the walk would
// build one inside the developer's checkout.

/// **`--dry-run` creates NOTHING and says so, and the apply then does what it said.**
///
/// The first successful migration is irreversible in practice — from then on the database answers
/// for every process on the box and `secrets.env` is no longer read — so the assertion that matters
/// most here is the negative one: after a dry run there is no database and not even a `db/`
/// directory. The second half is what makes the dry run worth running: the numbers it printed are
/// the numbers the apply reports.
#[test]
fn migrate_dry_run_writes_nothing_and_then_the_apply_matches_it() {
    let c = SetCase::new("migrate-dry");
    c.seed_for_migration();
    let before = (c.read_store(), std::fs::read_to_string(c.node_store()).unwrap());

    let out = c.run(&["migrate", "--dry-run"], None, &[]);
    assert_eq!(exit_code(&out), rung(vike_cli::exit::Exit::Ok), "{}", stderr(&out));
    let plan = stdout(&out);
    assert!(plan.contains("would be CREATED"), "{plan}");
    assert!(plan.contains("NOTHING WAS WRITTEN"), "{plan}");
    assert!(plan.contains("DRY RUN"), "{plan}");
    assert!(plan.contains("vike-cli secrets migrate"), "it must name the applying command: {plan}");

    assert!(
        !c.db().exists(),
        "A DRY RUN CREATED THE DATABASE — from here the credential file on this box is never read \
         again, which is the exact act the rehearsal exists to let somebody decide about first"
    );
    assert!(!c.db().parent().unwrap().exists(), "…nor even the `db/` directory");
    assert!(c.journal_lines().is_empty(), "a rehearsal records nothing");
    assert_eq!(
        (c.read_store(), std::fs::read_to_string(c.node_store()).unwrap()),
        before,
        "a dry run touched a source file"
    );
    // A VALUE cannot reach either stream, on this verb as on `set`.
    let dry_err = stderr(&out);
    for text in [plan.as_str(), dry_err.as_str()] {
        assert!(!text.contains("0xdeadbeef"), "a VALUE reached a stream: {text}");
        assert!(!text.contains("observe-one"), "a VALUE reached a stream: {text}");
    }

    // …and now the real thing, which must report the same counts.
    let out = c.run(&["migrate"], None, &[]);
    assert_eq!(exit_code(&out), rung(vike_cli::exit::Exit::Ok), "{}", stderr(&out));
    let done = stdout(&out);
    assert!(c.db().is_file(), "the apply must create the database: {done}");
    for row in plan.lines().filter(|l| l.contains("key(s) read")) {
        let read_count = row.split("key(s) read").next().expect("a prefix");
        assert!(
            done.lines().any(|l| l.starts_with(read_count)),
            "the apply reported a different per-file row than the plan promised:\n{plan}\n{done}"
        );
    }
}

/// **The migration lands every name — including the ones no grid can enumerate — and the FILES are
/// byte-identical afterwards.**
///
/// The second half is the rule that outranks everything else here: the credential file is the
/// operator's only copy of their live venue keys, and a migration is the exact place somebody
/// reaches for "…and then tidy it up". Retiring the file stays an operator act.
#[test]
fn migrate_creates_the_database_and_leaves_both_files_byte_identical() {
    let c = SetCase::new("migrate-create");
    c.seed_for_migration();

    let out = c.run(&["migrate"], None, &[]);
    assert_eq!(exit_code(&out), rung(vike_cli::exit::Exit::Ok), "{}", stderr(&out));
    let text = stdout(&out);
    assert!(text.contains("created"), "{text}");
    assert!(text.contains("READ ONLY"), "the report must say the files were only read: {text}");
    assert!(c.db().is_file(), "no database at {}", c.db().display());

    // ⚠ **The one fact an operator has to carry away**: the files have stopped being READ. The
    // report's "READ ONLY" line says they were not written, which is true and is a different claim
    // — and every runbook in this tree says *edit secrets.env*, which from this moment changes
    // nothing while looking exactly like it worked.
    assert!(text.contains("NO LONGER READ"), "{text}");
    assert!(text.contains("secrets.env") && text.contains("node.env"), "both files: {text}");

    assert_eq!(c.read_store(), MIGRATE_STORE, "the credential file was rewritten");
    assert_eq!(
        std::fs::read_to_string(c.node_store()).unwrap(),
        MIGRATE_NODE_STORE,
        "the node file was rewritten"
    );

    // The store that ANSWERS is now the database, and it holds the venue names — proven through
    // the shipped binary's own reader rather than by opening the file here.
    let listed = c.run(&["list"], None, &[]);
    assert_eq!(exit_code(&listed), rung(vike_cli::exit::Exit::Ok), "{}", stderr(&listed));
    let out = stdout(&listed);
    for key in ["BINANCE_LIVE_API_KEY", "DUKASCOPY_DEMO1_LOGIN", "HYPERLIQUID_LIVE_PRIVATE_KEY"] {
        assert!(out.contains(key), "{key} did not survive the migration: {out}");
    }
    assert!(!out.contains("0xdeadbeef"), "a VALUE reached the listing: {out}");
    // …and the node keys are in the OTHER namespace, so `secrets list` does not print them.
    assert!(!out.contains("VIKE_TRADEHUB_OBSERVE_KEY"), "a node key joined the venue grid: {out}");
}

/// **Twice is the same as once, over the shipped binary**, and the second run says so rather than
/// looking like a fresh success.
#[test]
fn migrate_is_idempotent_and_the_second_run_says_so() {
    let c = SetCase::new("migrate-twice");
    c.seed_for_migration();

    assert_eq!(exit_code(&c.run(&["migrate"], None, &[])), rung(vike_cli::exit::Exit::Ok));
    let first_bytes = std::fs::read(c.db()).unwrap();
    let first_records = c.journal_lines().len();

    let out = c.run(&["migrate"], None, &[]);
    assert_eq!(exit_code(&out), rung(vike_cli::exit::Exit::Ok), "{}", stderr(&out));
    assert!(
        stdout(&out).contains("already complete"),
        "the second run must say it had nothing to do: {}",
        stdout(&out)
    );
    assert_eq!(std::fs::read(c.db()).unwrap(), first_bytes, "the database bytes moved on a no-op");
    assert_eq!(
        c.journal_lines().len(),
        first_records,
        "a run that wrote nothing appended a ledger record claiming it did"
    );
}

/// **A box with no credentials at all: `nothing to migrate`, exit 0, and NO database.**
///
/// Creating one is the harmful act — from that moment every process on the box reads the database,
/// so the credential file the operator writes afterwards is never read and every venue silently
/// stays on paper. A non-zero rung would be wrong for the opposite reason: this is the correct
/// outcome on a fresh install, not a failure.
#[test]
fn migrate_on_an_unconfigured_box_creates_no_database_and_exits_zero() {
    let c = SetCase::new("migrate-empty");

    let out = c.run(&["migrate"], None, &[]);
    assert_eq!(exit_code(&out), rung(vike_cli::exit::Exit::Ok), "{}", stderr(&out));
    let text = stdout(&out);
    assert!(text.contains("nothing to migrate"), "{text}");
    assert!(text.contains("NOT created"), "{text}");
    assert!(!c.db().exists(), "an empty database was created and now shadows a file nobody wrote");
    assert!(!c.db().parent().unwrap().exists(), "…nor even the `db/` directory");
    assert!(c.journal_lines().is_empty(), "nothing was written, so nothing may be recorded");

    // …and the box is still ordinary afterwards: a store written NOW is the one that answers.
    c.write_store(MIGRATE_STORE);
    let listed = c.run(&["list"], None, &[]);
    assert!(stdout(&listed).contains("BINANCE_LIVE_API_KEY"), "{}", stdout(&listed));
}

/// **An AMBIGUOUS box is refused, nothing is written, and the rung is FAILED rather than USAGE.**
///
/// The command line was fine — no change to it fixes this — so it is the ordinary run failure, which
/// is the same split `set` takes between a bad key (USAGE) and an absent store (FAILED). The refusal
/// names the key, because a refusal an operator cannot act on is just a stop.
#[test]
fn migrate_refuses_an_ambiguous_box_and_writes_nothing() {
    let c = SetCase::new("migrate-ambiguous");
    // The same name in both files with DIFFERENT values — a half-migrated box. Picking a side would
    // produce a mismatched pair, whose symptom at the node is an opaque `bad mac`.
    c.write_store("VIKE_TRADEHUB_OBSERVE_KEY=old\nBINANCE_LIVE_API_KEY=k\n");
    std::fs::write(c.node_store(), "VIKE_TRADEHUB_OBSERVE_KEY=new\n").unwrap();

    for argv in [&["migrate"][..], &["migrate", "--dry-run"][..]] {
        let out = c.run(argv, None, &[]);
        assert_eq!(
            exit_code(&out),
            rung(vike_cli::exit::Exit::Failed),
            "{argv:?}: {}",
            stderr(&out)
        );
        let err = stderr(&out);
        assert!(err.contains("VIKE_TRADEHUB_OBSERVE_KEY"), "{argv:?}: {err}");
        assert!(err.contains("nothing written"), "{argv:?}: {err}");
        assert!(!err.contains("=old"), "a VALUE reached stderr: {err}");
        assert!(!c.db().exists(), "{argv:?}: a refusal left a database behind");
        assert!(!c.db().parent().unwrap().exists(), "{argv:?}: …nor may it leave the directory");
    }
}

/// **A per-KEY refusal is LOUD on stderr and the run still succeeds** — the one judgement on this
/// verb that could reasonably have gone the other way, so it is pinned.
///
/// Every unambiguous key lands, the disagreeing one is never overwritten and is NAMED, and the rung
/// stays 0 because a non-zero one would make a box somebody has deliberately left in this state fail
/// this verb forever. The stderr copy exists because the report on stdout is a document operators
/// redirect.
#[test]
fn migrate_reports_a_refused_key_on_stderr_and_still_lands_the_rest() {
    let c = SetCase::new("migrate-refused");
    c.write_store("BINANCE_LIVE_API_KEY=first\n");
    assert_eq!(exit_code(&c.run(&["migrate"], None, &[])), rung(vike_cli::exit::Exit::Ok));

    // The operator edits the migrated key AND adds a new one in the same edit.
    c.write_store("BINANCE_LIVE_API_KEY=second\nOKX_DEMO_API_KEY=fresh\n");

    let out = c.run(&["migrate"], None, &[]);
    assert_eq!(exit_code(&out), rung(vike_cli::exit::Exit::Ok), "{}", stderr(&out));
    let err = stderr(&out);
    assert!(err.contains("REFUSED"), "the refusal must be loud on stderr: {err}");
    assert!(err.contains("BINANCE_LIVE_API_KEY"), "…and must name the key: {err}");
    assert!(err.contains("did not carry everything"), "{err}");
    assert!(!err.contains("second"), "a VALUE reached stderr: {err}");

    // The new key landed; the refused one kept the value already stored.
    let listed = stdout(&c.run(&["list"], None, &[]));
    assert!(listed.contains("OKX_DEMO_API_KEY"), "the unambiguous key must land: {listed}");
}

/// **ONE `credential_write` record for the whole act**, carrying key NAMES, the DATABASE as the
/// store, and no value.
///
/// The shape is a judgement — `crates/vike-cli/src/cmd/secrets/migrate.rs`'s `record_migration` argues one
/// record for the act against one per (venue, tier) — so it is pinned rather than left to drift. The
/// `multi` venue and the untiered tier are `vike_model::change_journal`'s own documented vocabulary
/// for a write that spans several.
#[test]
fn the_migration_is_journalled_once_for_the_act_with_names_and_no_value() {
    let c = SetCase::new("migrate-journal");
    c.seed_for_migration();

    let out = c.run(&["migrate"], None, &[]);
    assert_eq!(exit_code(&out), rung(vike_cli::exit::Exit::Ok), "{}", stderr(&out));

    let lines = c.journal_lines();
    assert_eq!(lines.len(), 1, "ONE act ⇒ ONE record, not one per venue: {lines:?}");
    let line = &lines[0];
    assert!(line.contains("credential_write"), "{line}");
    assert!(line.contains("vike.db"), "the store cell must name where the rows LANDED: {line}");
    assert!(!line.contains("secrets.env"), "…and not the file they were read from: {line}");
    assert!(line.contains("\"multi\""), "a write spanning venues is `multi`: {line}");
    assert!(line.contains("vike-cli"), "the CLI is the actor: {line}");
    for key in [
        "BINANCE_LIVE_API_KEY",
        "DUKASCOPY_DEMO1_LOGIN",
        "HYPERLIQUID_LIVE_PRIVATE_KEY",
        "VIKE_TRADEHUB_OBSERVE_KEY",
    ] {
        assert!(line.contains(key), "the record must carry the key NAME {key}: {line}");
    }
    for value in ["key-one", "login-one", "0xdeadbeef", "observe-one", "control-one"] {
        assert!(!line.contains(value), "a VALUE reached the ledger: {line}");
    }
}

/// **`--file` is refused on `migrate`**, on the usage rung, and the refusal names the flag.
///
/// Sharper than the same refusal on `set`: `set` aimed at a path appends a line to it, while
/// `migrate` aimed at one would CREATE a credential database beside an arbitrary file.
#[test]
fn migrate_refuses_a_file_flag() {
    let c = SetCase::new("migrate-file");
    c.seed_for_migration();

    let elsewhere = c.dir().join("elsewhere.env");
    std::fs::write(&elsewhere, "BINANCE_LIVE_API_KEY=k\n").unwrap();
    let out = c.run(&["migrate", "--file", &elsewhere.display().to_string()], None, &[]);
    assert_eq!(exit_code(&out), rung(vike_cli::exit::Exit::Usage), "{}", stderr(&out));
    assert!(stderr(&out).contains("--file"), "{}", stderr(&out));
    assert!(!c.db().exists(), "a refused command created a database");
    assert!(!c.dir().join("db").exists(), "…and none beside the named file's directory either");
}

/// **`template` is BACKEND-AWARE: byte-identical on an unmigrated box, and a finding on a migrated
/// one.**
///
/// Its whole documented use is `vike-cli secrets template > settings/secrets.env`, and every doc and
/// skill in this tree names it as HOW TO CREATE THE STORE. After a migration that redirect writes a
/// file nothing reads and exits 0 — the live gate wearing the fresh-install answer. So the grid is
/// still printed (it is a SHAPE, and reading it is not a write) and stderr says the file is no longer
/// read.
///
/// ⚠ The stdout half is asserted byte-for-byte across the two boxes, because this output is what
/// gets redirected into real stores.
#[test]
fn template_warns_only_once_the_database_answers() {
    let c = SetCase::new("migrate-template");
    c.seed_for_migration();

    let before = c.run(&["template"], None, &[]);
    assert_eq!(exit_code(&before), rung(vike_cli::exit::Exit::Ok), "{}", stderr(&before));
    assert!(
        !stderr(&before).contains("MIGRATED"),
        "an unmigrated box must produce the output it always did: {}",
        stderr(&before)
    );

    assert_eq!(exit_code(&c.run(&["migrate"], None, &[])), rung(vike_cli::exit::Exit::Ok));

    let after = c.run(&["template"], None, &[]);
    assert_eq!(exit_code(&after), rung(vike_cli::exit::Exit::Ok), "{}", stderr(&after));
    assert_eq!(
        stdout(&after),
        stdout(&before),
        "the GRID itself must not change — it is redirected into real stores"
    );
    let err = stderr(&after);
    assert!(err.contains("MIGRATED"), "{err}");
    assert!(err.contains("vike.db"), "it must name the store that answers: {err}");
    assert!(err.contains("secrets set"), "…and the verb that writes it: {err}");
    assert!(err.contains("secrets migrate"), "…and the verb that created it: {err}");
}

// ── `--file` on a migrated box ───────────────────────────────────────────────────────────────────
//
// `--file` is the flag an operator reaches for when they already suspect the store, and it is the
// one path on this command that `docs/decisions/0054`'s credential half left FILE-SHAPED: it goes
// to `vike_secrets::resolve`, the text arm by definition, which reports `shadowed: None` by
// construction. So on a migrated box it printed a roster of a file no process on the machine loads
// and said nothing about it — a stale answer with no qualifier, reached by the flag most likely to
// be typed by somebody who needed the qualifier most.
//
// These drive the REAL binary against a REAL migrated project, with `--file` aimed at that
// project's own retired credential file and at the database beside it.

/// A `--file` aimed at a store the database SHADOWS still lists its key NAMES, and now carries the
/// finding that the file is no longer read.
///
/// The names still appear: the operator asked to see that file, and the file is still their own
/// copy. What must not happen is the listing standing alone.
///
/// ⚠ The sentence above deliberately avoids naming a command and then saying what it writes to
/// stdout. `crates/vike-ops/tests/docs/unrun_command_gate.rs` harvests a backticked command carrying a
/// `CLAIM_MARKERS` phrase within 80 bytes and demands a CHECKED or UNVERIFIABLE row for the pair —
/// and it is right to: a doc comment stating a command's output is a claim, and this one's proof is
/// the test body rather than a row in that table. Measured on the CI box TWICE — the first correction
/// tripped the same gate from inside its own explanation of it.
#[test]
fn list_with_a_file_that_a_database_shadows_says_the_file_is_no_longer_read() {
    let p = Project::new("file-shadowed-list");
    p.write_store("BINANCE_LIVE_API_KEY=abc\n");
    p.migrate();

    let out = Command::new(BIN)
        .args(["secrets", "list", "--file"])
        .arg(p.store())
        .stdin(Stdio::null())
        .env("VIKE_SETTINGS_DIR", p.settings())
        .output()
        .expect("the vike-cli binary must run");
    assert_eq!(exit_code(&out), rung(vike_cli::exit::Exit::Ok), "{}", stderr(&out));

    let o = stdout(&out);
    assert!(o.contains("BINANCE_LIVE_API_KEY"), "the named file's keys still print: {o}");
    assert!(!o.contains("abc"), "a VALUE may never appear: {o}");

    let e = stderr(&out);
    assert!(
        e.contains("NO LONGER READ"),
        "a listing of a shadowed file must carry the finding: {e}"
    );
    assert!(e.contains("vike.db"), "…and name the store that answers instead: {e}");
}

/// `path --file <a shadowed store>` names the database beside it.
///
/// The guard used to be `args.file.is_none()`, so this invocation printed the file as `store:` and
/// suppressed the `db:`/`answers:` block entirely — this verb doing the one thing it exists to
/// prevent, on the flag an operator uses when something is already wrong.
#[test]
fn path_with_a_file_that_a_database_shadows_names_the_database() {
    let p = Project::new("file-shadowed-path");
    p.write_store("BINANCE_LIVE_API_KEY=abc\n");
    p.migrate();

    let out = Command::new(BIN)
        .args(["secrets", "path", "--file"])
        .arg(p.store())
        .stdin(Stdio::null())
        .env("VIKE_SETTINGS_DIR", p.settings())
        .output()
        .expect("the vike-cli binary must run");
    assert_eq!(exit_code(&out), rung(vike_cli::exit::Exit::Ok), "{}", stderr(&out));

    let o = stdout(&out);
    assert!(o.contains("store:"), "{o}");
    assert!(o.contains("db:"), "the database beside the named file must be printed: {o}");
    assert!(o.contains("NO LONGER READ"), "…and said to be what answers: {o}");
}

/// **`--file` pointed at the DATABASE is refused, and the refusal is what a silent wrong answer is
/// replaced by.**
///
/// The assertion that carries the weight is the NEGATIVE one: the old behaviour did not error, it
/// printed an empty listing. A database's pages are largely NUL bytes, which are valid UTF-8, so
/// the text read succeeded and the `KEY=VALUE` parser found nothing in the binary — `0 secret(s)`,
/// about the one artifact holding every venue key.
#[test]
fn a_file_naming_the_database_is_refused_rather_than_parsed_as_an_empty_store() {
    let p = Project::new("file-is-the-db");
    p.write_store("BINANCE_LIVE_API_KEY=abc\n");
    p.migrate();

    for verb in ["list", "path"] {
        let out = Command::new(BIN)
            .args(["secrets", verb, "--file"])
            .arg(p.db())
            .stdin(Stdio::null())
            .env("VIKE_SETTINGS_DIR", p.settings())
            .output()
            .expect("the vike-cli binary must run");
        assert_ne!(
            exit_code(&out),
            rung(vike_cli::exit::Exit::Ok),
            "`{verb} --file <db>` must not succeed: {}{}",
            stdout(&out),
            stderr(&out)
        );
        let o = stdout(&out);
        assert!(
            !o.contains("secret(s)"),
            "the failure this refusal replaces is a LISTING, so none may print: {o}"
        );
        let e = stderr(&out);
        assert!(e.contains("DATABASE"), "{e}");
        assert!(e.contains("VIKE_SETTINGS_DIR"), "a refusal must name the way through: {e}");
    }
}

/// **An ordinary `--file` is untouched**, which is the property every assertion above is paid for
/// with: a path with no `db/vike.db` beside it prints exactly what it printed before any of this.
#[test]
fn an_ordinary_file_outside_a_migrated_project_prints_no_database_line() {
    // Two separate throwaway roots: the file being inspected, and an EMPTY settings directory for
    // `$VIKE_SETTINGS_DIR`, so nothing here can reach the developer box's own store.
    let c = Case::new("file-unmigrated");
    c.write_store("BINANCE_LIVE_API_KEY=abc\n");
    let elsewhere = tempfile::Builder::new().prefix("vike-cli-empty-").tempdir().expect("tempdir");

    for verb in ["list", "path"] {
        let out = Command::new(BIN)
            .args(["secrets", verb, "--file"])
            .arg(c.store())
            .stdin(Stdio::null())
            .env("VIKE_SETTINGS_DIR", elsewhere.path())
            .output()
            .expect("the vike-cli binary must run");
        assert_eq!(exit_code(&out), rung(vike_cli::exit::Exit::Ok), "{}", stderr(&out));
        let both = format!("{}{}", stdout(&out), stderr(&out));
        assert!(
            !both.contains("vike.db") && !both.contains("NO LONGER READ"),
            "`{verb}` over an unmigrated path must be byte-identical to before: {both}"
        );
    }
}
