//! `migrate`: the dry run, the apply, its refusals, and `--file` beside a migrated project.

use std::process::{Command, Stdio};

use super::support::{MIGRATE_NODE_STORE, MIGRATE_STORE, SetCase, exit_code, rung};
use super::{BIN, stderr, stdout};

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
    let before = (
        std::fs::read_to_string(c.file()).unwrap(),
        std::fs::read_to_string(c.node_store()).unwrap(),
    );

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
        (
            std::fs::read_to_string(c.file()).unwrap(),
            std::fs::read_to_string(c.node_store()).unwrap()
        ),
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

    assert_eq!(
        std::fs::read_to_string(c.file()).unwrap(),
        MIGRATE_STORE,
        "the credential file was rewritten"
    );
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
    c.store_rows(MIGRATE_STORE);
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

// ── `--file` ────────────────────────────────────────────────────────────────────────────────────
//
// The `--file PATH` flag, and the four cases that pinned what it printed beside a migrated box (a
// shadowed file's listing, `path` naming the database, a `--file` naming the database refused, an
// ordinary file untouched), went with the credential FILE store on 2026-10-07: there is no file
// store left to inspect, and the flag is refused by name on every subcommand
// (`crates/vike-cli/src/cmd/secrets/grammar.rs`'s `FILE_FLAG_REMOVED`).

// ── migrate --init ──────────────────────────────────────────────────────────────────────────────
//
// The explicit fresh start. Plain `migrate` on an empty box creates NOTHING (the test above), and
// that stays; `--init` is the operator saying the box starts fresh, and each case below holds one
// of its invariants over the SHIPPED binary.

/// `vike-cli config …` against this case's settings directory, environment cleared — the same
/// isolation `crates/vike-cli/tests/bootstrap_daemon_cli.rs`'s `run` takes.
fn config(c: &SetCase, args: &[&str]) -> std::process::Output {
    Command::new(BIN)
        .arg("config")
        .args(args)
        .env_clear()
        .env("VIKE_SETTINGS_DIR", c.settings())
        .stdin(Stdio::null())
        .output()
        .expect("the vike-cli binary must run")
}

/// **`--init --dry-run` creates NOTHING; `--init` then creates the EMPTY store, says what that
/// costs, and the store it made is one a credential can be SET into.**
///
/// The consequence line is asserted word by word because it is the whole of what this run did:
/// the database answers for every credential from now on, the two files are not read, and the way
/// in is `secrets set` with the value off argv.
#[test]
fn migrate_init_rehearses_then_creates_the_empty_store_that_answers() {
    let c = SetCase::new("migrate-init");

    let out = c.run(&["migrate", "--init", "--dry-run"], None, &[]);
    assert_eq!(exit_code(&out), rung(vike_cli::exit::Exit::Ok), "{}", stderr(&out));
    let plan = stdout(&out);
    assert!(plan.contains("would be CREATED EMPTY"), "{plan}");
    assert!(plan.contains("DRY RUN") && plan.contains("NOTHING WAS WRITTEN"), "{plan}");
    assert!(plan.contains("vike-cli secrets migrate --init"), "it must name the apply: {plan}");
    assert!(!c.db().exists(), "A DRY RUN OF --init CREATED THE STORE");
    assert!(!c.db().parent().unwrap().exists(), "…nor even the `db/` directory");

    let out = c.run(&["migrate", "--init"], None, &[]);
    assert_eq!(exit_code(&out), rung(vike_cli::exit::Exit::Ok), "{}", stderr(&out));
    let text = stdout(&out);
    assert!(c.db().is_file(), "--init must create the store: {text}");
    assert!(text.contains("created EMPTY"), "{text}");
    for needle in ["EVERY credential", "secrets.env", "node.env", "NOT read", "secrets set"] {
        assert!(text.contains(needle), "the consequence must say `{needle}`: {text}");
    }
    assert!(text.contains("never on the command line"), "…and that a value stays off argv: {text}");
    assert!(c.journal_lines().is_empty(), "no key was written, so no credential_write is owed");

    // The store answers — and a credential set into it lands, value on stdin, never echoed.
    let set = c.run(&["set", "BINANCE_DEMO_API_KEY"], Some("fake-init-value"), &[]);
    assert_eq!(exit_code(&set), rung(vike_cli::exit::Exit::Ok), "{}", stderr(&set));
    let listed = c.run(&["list"], None, &[]);
    assert_eq!(exit_code(&listed), rung(vike_cli::exit::Exit::Ok), "{}", stderr(&listed));
    let names = stdout(&listed);
    assert!(names.contains("BINANCE_DEMO_API_KEY"), "the set key must answer: {names}");
    for text in [stdout(&set), stderr(&set), names] {
        assert!(!text.contains("fake-init-value"), "a VALUE reached a stream: {text}");
    }
}

/// **The order the fix exists for: `config bootstrap-daemon` REFUSES on a box with no store —
/// naming `--init` — and SUCCEEDS once `secrets migrate --init` has created it.** A paper profile,
/// the first run's own arguments.
#[test]
fn bootstrap_daemon_succeeds_after_migrate_init_and_not_before() {
    let c = SetCase::new("migrate-init-bootstrap");
    let argv = [
        "bootstrap-daemon",
        "default",
        "--venue",
        "binance",
        "--asset-class",
        "CryptoSpot",
        "--symbol",
        "BTCUSDT",
    ];

    let refused = config(&c, &argv);
    assert!(!refused.status.success(), "no store, so no profile row: {refused:?}");
    assert!(stderr(&refused).contains("secrets migrate --init"), "{}", stderr(&refused));
    assert!(!c.db().exists(), "a refused profile write created a store");

    assert_eq!(
        exit_code(&c.run(&["migrate", "--init"], None, &[])),
        rung(vike_cli::exit::Exit::Ok)
    );
    let built = config(&c, &argv);
    assert!(built.status.success(), "{}{}", stdout(&built), stderr(&built));
}

/// **THE INCIDENT GUARD, over the binary: `--init` beside a credential file CARRIES it** — the
/// ordinary creating run, with the ordinary report — and never leaves an empty store answering for
/// a box whose keys are on disk.
#[test]
fn migrate_init_beside_a_credential_file_carries_it() {
    let c = SetCase::new("migrate-init-carries");
    c.seed_for_migration();

    let out = c.run(&["migrate", "--init"], None, &[]);
    assert_eq!(exit_code(&out), rung(vike_cli::exit::Exit::Ok), "{}", stderr(&out));
    let text = stdout(&out);
    assert!(!text.contains("EMPTY"), "a box with keys is MIGRATED, not initialised: {text}");
    assert!(text.contains("NO LONGER READ"), "the ordinary creating run's report: {text}");
    let listed = stdout(&c.run(&["list"], None, &[]));
    for key in ["BINANCE_LIVE_API_KEY", "DUKASCOPY_DEMO1_LOGIN", "HYPERLIQUID_LIVE_PRIVATE_KEY"] {
        assert!(listed.contains(key), "{key} was shadowed by --init: {listed}");
    }
    assert_eq!(
        std::fs::read_to_string(c.file()).unwrap(),
        MIGRATE_STORE,
        "the credential file was rewritten"
    );
    assert_eq!(c.journal_lines().len(), 1, "a creating run with keys journals itself once");
}

/// **`--init` on a box that already has a store is the ordinary no-op**, byte for byte.
#[test]
fn migrate_init_on_a_migrated_box_moves_no_byte() {
    let c = SetCase::new("migrate-init-existing");
    c.seed_for_migration();
    assert_eq!(exit_code(&c.run(&["migrate"], None, &[])), rung(vike_cli::exit::Exit::Ok));
    let before = std::fs::read(c.db()).unwrap();

    for argv in [&["migrate", "--init", "--dry-run"][..], &["migrate", "--init"][..]] {
        let out = c.run(argv, None, &[]);
        assert_eq!(exit_code(&out), rung(vike_cli::exit::Exit::Ok), "{argv:?}: {}", stderr(&out));
        assert!(!stdout(&out).contains("EMPTY"), "{argv:?}: {}", stdout(&out));
        assert_eq!(std::fs::read(c.db()).unwrap(), before, "{argv:?} moved the store's bytes");
    }
    assert!(stdout(&c.run(&["migrate", "--init"], None, &[])).contains("already complete"));
}
