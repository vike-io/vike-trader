//! `init`: the empty store, its rehearsal, the no-op on an existing store, and the refused `--file`.

use std::process::{Command, Stdio};

use super::support::{SEEDED_STORE, SetCase, exit_code, rung};
use super::{BIN, stderr, stdout};

// ── init ────────────────────────────────────────────────────────────────────────────────────────
//
// ⚠ Every case drives its own temp settings directory through `$VIKE_SETTINGS_DIR` — the same
// isolation `SetCase` takes, and it is doubly load-bearing here: this verb CREATES a credential
// database inside whatever directory it resolves, so a case that fell through to the walk would
// build one inside the developer's checkout.

/// **Twice is the same as once, over the shipped binary**, and the second run says so rather than
/// looking like a fresh success.
#[test]
fn init_is_idempotent_and_the_second_run_says_so() {
    let c = SetCase::new("init-twice");
    assert_eq!(exit_code(&c.run(&["init"], None, &[])), rung(vike_cli::exit::Exit::Ok));
    let first_bytes = std::fs::read(c.db()).unwrap();
    let first_records = c.journal_lines().len();

    let out = c.run(&["init"], None, &[]);
    assert_eq!(exit_code(&out), rung(vike_cli::exit::Exit::Ok), "{}", stderr(&out));
    assert!(
        stdout(&out).contains("already exists"),
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

/// **`--file` is refused on `init`**, on the usage rung, and the refusal names the flag.
///
/// Sharper than the same refusal on `set`: `init` aimed at a path would CREATE a credential database
/// beside an arbitrary file.
#[test]
fn init_refuses_a_file_flag() {
    let c = SetCase::new("init-file");

    let elsewhere = c.dir().join("elsewhere.env");
    let out = c.run(&["init", "--file", &elsewhere.display().to_string()], None, &[]);
    assert_eq!(exit_code(&out), rung(vike_cli::exit::Exit::Usage), "{}", stderr(&out));
    assert!(stderr(&out).contains("--file"), "{}", stderr(&out));
    assert!(!c.db().exists(), "a refused command created a database");
    assert!(!c.dir().join("db").exists(), "…and none beside the named file's directory either");
}

/// **The retired verb is an unknown subcommand**, on the usage rung, and it creates nothing: a
/// retired name simply stops being read (`docs/decisions/0117-there-are-no-migrations.md`).
#[test]
fn the_old_verb_is_unknown_and_creates_nothing() {
    let c = SetCase::new("init-old-verb");
    let out = c.run(&[concat!("mig", "rate")], None, &[]);
    assert_eq!(exit_code(&out), rung(vike_cli::exit::Exit::Usage), "{}", stderr(&out));
    assert!(stderr(&out).contains("unknown `secrets` subcommand"), "{}", stderr(&out));
    assert!(!c.db().exists(), "an unknown verb created a database");
}

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

/// **`init --dry-run` creates NOTHING; `init` then creates the EMPTY store, says what that costs,
/// and the store it made is one a credential can be SET into.**
///
/// The consequence line is asserted word by word because it is the whole of what this run did:
/// the database answers for every credential from now on, and the way in is `secrets set` with the
/// value off argv.
#[test]
fn init_rehearses_then_creates_the_empty_store_that_answers() {
    let c = SetCase::new("init-fresh");

    let out = c.run(&["init", "--dry-run"], None, &[]);
    assert_eq!(exit_code(&out), rung(vike_cli::exit::Exit::Ok), "{}", stderr(&out));
    let plan = stdout(&out);
    assert!(plan.contains("would be CREATED EMPTY"), "{plan}");
    assert!(plan.contains("DRY RUN") && plan.contains("NOTHING WAS WRITTEN"), "{plan}");
    assert!(plan.contains("vike-cli secrets init"), "it must name the apply: {plan}");
    assert!(!c.db().exists(), "A DRY RUN OF init CREATED THE STORE");
    assert!(!c.db().parent().unwrap().exists(), "…nor even the `db/` directory");

    let out = c.run(&["init"], None, &[]);
    assert_eq!(exit_code(&out), rung(vike_cli::exit::Exit::Ok), "{}", stderr(&out));
    let text = stdout(&out);
    assert!(c.db().is_file(), "init must create the store: {text}");
    assert!(text.contains("created EMPTY"), "{text}");
    for needle in ["EVERY credential", "secrets set"] {
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
/// naming `secrets init` — and SUCCEEDS once `secrets init` has created it.** A paper profile, the
/// first run's own arguments.
#[test]
fn bootstrap_daemon_succeeds_after_init_and_not_before() {
    let c = SetCase::new("init-bootstrap");
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
    assert!(stderr(&refused).contains("secrets init"), "{}", stderr(&refused));
    assert!(!c.db().exists(), "a refused profile write created a store");

    assert_eq!(exit_code(&c.run(&["init"], None, &[])), rung(vike_cli::exit::Exit::Ok));
    let built = config(&c, &argv);
    assert!(built.status.success(), "{}{}", stdout(&built), stderr(&built));
}

/// **`init` on a box that already has a store is the ordinary no-op**, byte for byte — it cannot
/// touch a store that exists, so it can never leave an EMPTY one answering for a box whose keys are
/// already in it.
#[test]
fn init_on_an_existing_store_moves_no_byte() {
    let c = SetCase::new("init-existing");
    c.store_rows(SEEDED_STORE);
    let before = std::fs::read(c.db()).unwrap();

    for argv in [&["init", "--dry-run"][..], &["init"][..]] {
        let out = c.run(argv, None, &[]);
        assert_eq!(exit_code(&out), rung(vike_cli::exit::Exit::Ok), "{argv:?}: {}", stderr(&out));
        assert!(!stdout(&out).contains("EMPTY"), "{argv:?}: {}", stdout(&out));
        assert_eq!(std::fs::read(c.db()).unwrap(), before, "{argv:?} moved the store's bytes");
    }
    assert!(stdout(&c.run(&["init"], None, &[])).contains("already exists"));
    let listed = stdout(&c.run(&["list"], None, &[]));
    for key in ["BINANCE_LIVE_API_KEY", "DUKASCOPY_DEMO1_LOGIN", "HYPERLIQUID_LIVE_PRIVATE_KEY"] {
        assert!(listed.contains(key), "{key} was lost to init: {listed}");
    }
}
