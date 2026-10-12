//! `account set-tier` and `account set-exposure` end to end, and the refusals of the removed
//! `template` verb and `--file` flag.

use std::process::{Command, Stdio};

use super::support::{SetCase, exit_code, rung};
use super::{BIN, stderr, stdout};

// ── account set-tier — the ARM, end to end ──────────────────────────────────────────────────────
//
// ⚠ **What is NOT re-tested here: the STORE.** `AccountEdit::SetTier` shipped with eleven cases in
// `crates/vike-secrets/tests/accounts/tier.rs` — the `AccountKeysPinTheTier` refusal in both
// directions, the hand-mapped dukascopy family, the unknown tier, the unknown id, the no-op — and
// its author declared the bound honestly: `run_account`'s `set-tier` arm was not driven end to end.
// So these cases cover exactly the three things only the shipped binary can answer — what the arm
// PRINTS, that it REFUSES the label flags rather than dropping them, and that its rehearsal writes
// nothing — and assert nothing about the transaction those store tests already own.
//
// The grammar refusals (`--tier` missing, `--tier` unknown) are covered by the unit tests beside
// the parser, which drive BOTH actions that take the flag; they are not repeated here either.

/// **The REHEARSAL prints the whole move and writes nothing**, over a row that owns credential
/// keys — the shape that makes every line of the arm's output appear at once.
///
/// Four of those lines are the arm's entire contract with an operator and none of them is visible
/// to a store test: the STORE PATH (named before anything is validated, so a rehearsal on the wrong
/// box cannot read like one on the right box), the row ECHO (the only check there is on `--id`),
/// the `old -> new` tier, and the two consequences — that the credential keys DO NOT MOVE, and that
/// the ACTIVE row ARMS at the new tier from the next restart.
#[test]
fn set_tier_dry_run_prints_the_move_and_the_two_consequences_and_writes_nothing() {
    let c = SetCase::new("set-tier-dry");
    let id = c.seeded_keyed_row();
    let before_store = c.read_store();

    let out = c.run(
        &["account", "set-tier", "--id", &id.to_string(), "--tier", "live", "--dry-run"],
        None,
        &[],
    );
    assert_eq!(exit_code(&out), rung(vike_cli::exit::Exit::Ok), "{}", stderr(&out));
    let text = stdout(&out);

    assert!(text.contains("vike.db"), "the rehearsal must name the store it would write: {text}");
    assert!(text.contains(&format!("account {id}")), "the row must be echoed: {text}");
    assert!(text.contains("BINANCE_DEMO_API_KEY"), "the echo must list the row's keys: {text}");
    assert!(text.contains("tier: demo -> live"), "the move must be printed both ways: {text}");
    assert!(text.contains("THEY DO NOT MOVE"), "a keyed row must be warned about: {text}");
    assert!(text.contains("ARMS"), "the consequence that lands elsewhere must be said: {text}");
    assert!(text.contains("DRY RUN"), "{text}");

    // Nothing was written, by all three measures: the credential FILE, the ledger, and the row.
    assert_eq!(c.read_store(), before_store, "a rehearsal touched the credential file");
    assert!(
        !c.journal_lines().iter().any(|l| l.contains("account_lifecycle")),
        "a rehearsal wrote a ledger line: {:?}",
        c.journal_lines()
    );
    let listed = stdout(&c.run(&["accounts"], None, &[]));
    let row = listed
        .lines()
        .find(|l| l.split_whitespace().next() == Some(id.to_string().as_str()))
        .unwrap_or_else(|| panic!("no row {id} in {listed}"));
    assert!(row.contains("demo"), "the row moved on a DRY RUN: {row}");
    assert!(!row.contains("live"), "the row moved on a DRY RUN: {row}");

    // A VALUE cannot reach either stream on any path of this verb.
    for v in ["key-one", "login-one", "pass-one"] {
        assert!(!text.contains(v), "a VALUE reached stdout: {text}");
        assert!(!stderr(&out).contains(v), "a VALUE reached stderr: {}", stderr(&out));
    }
}

/// **The label flags are REFUSED, not dropped** — both of them, on the usage rung, naming the
/// action that does own a label.
///
/// ⚠ This is the case the arm's own comment exists for: `parse` admits `--label` and `--no-label`
/// on EVERY `account` action, so an operator reaching for `rename`'s muscle memory would otherwise
/// have the flag silently ignored and come away believing a row was renamed as well as moved.
/// Silent-drop and refusal are indistinguishable at the parser; only the shipped binary can tell
/// them apart.
#[test]
fn set_tier_refuses_both_label_flags_and_writes_nothing() {
    let c = SetCase::new("set-tier-label");
    let id = c.seeded_keyed_row();
    let before_store = c.read_store();
    let id = id.to_string();

    for extra in [vec!["--label", "ALT"], vec!["--no-label"]] {
        let mut args = vec!["account", "set-tier", "--id", id.as_str(), "--tier", "live"];
        args.extend_from_slice(&extra);
        let out = c.run(&args, None, &[]);
        assert_eq!(
            exit_code(&out),
            rung(vike_cli::exit::Exit::Usage),
            "{extra:?} must be refused: {}",
            stdout(&out)
        );
        let e = stderr(&out);
        assert!(e.contains("set-tier"), "{extra:?}: the refusal must name the action: {e}");
        assert!(
            e.contains("account rename"),
            "{extra:?}: it must name the action that DOES rename: {e}"
        );
        assert!(e.contains("Nothing was written"), "{extra:?}: {e}");
    }

    assert_eq!(c.read_store(), before_store, "a refusal touched the credential file");
    assert!(
        !c.journal_lines().iter().any(|l| l.contains("account_lifecycle")),
        "a refusal wrote a ledger line: {:?}",
        c.journal_lines()
    );
    let listed = stdout(&c.run(&["accounts"], None, &[]));
    assert!(!listed.contains("ALT"), "a refused label was written: {listed}");
}

/// **The APPLY reports the move and the SECOND run reports a no-op** — the two replies the arm can
/// end on, and the ledger line that separates them.
///
/// The no-op half is the one worth driving from out here: `AccountWrite::changed` is a store fact
/// with its own test, but *which sentence an operator reads* is this arm's, and printing "moved" on
/// a run that moved nothing is exactly the reply that makes somebody believe a second edit landed.
#[test]
fn set_tier_applies_once_and_says_unchanged_the_second_time() {
    let c = SetCase::new("set-tier-apply");
    c.seeded_keyed_row();
    // A row the fixture's credentials did not create, so no credential key names its tier — see
    // [`SetCase::added_account`].
    let id = c.added_account("binance", "paper", "ALT").to_string();
    // ⚠ What is under test is what the MOVE adds to the ledger, so the baseline is taken rather
    // than assumed: a test asserting an empty ledger here would be asserting the fixture.
    let credential_writes_before =
        c.journal_lines().iter().filter(|l| l.contains("credential_write")).count();

    let out = c.run(&["account", "set-tier", "--id", &id, "--tier", "demo"], None, &[]);
    assert_eq!(exit_code(&out), rung(vike_cli::exit::Exit::Ok), "{}", stderr(&out));
    let text = stdout(&out);
    assert!(text.contains("tier: paper -> demo"), "{text}");
    assert!(text.contains(&format!("moved account {id} to tier demo")), "{text}");
    assert!(
        !text.contains("THEY DO NOT MOVE"),
        "a KEYLESS row must not be warned about keys it does not own: {text}"
    );

    // ONE ledger line for the MOVE, and no credential write ADDED by it.
    //
    // ⚠ Counted on the VERB rather than on the kind. `account add` writes an `account_lifecycle`
    // of its own (`"verb":"create"`), so a kind-only count answers 2 here and would have answered
    // 1 for a `set-tier` that journalled nothing at all — the arm's own record hidden behind the
    // fixture's. Measured on the first lane run, which is the only reason this comment exists.
    let moves = |c: &SetCase| -> Vec<String> {
        c.journal_lines()
            .into_iter()
            .filter(|l| l.contains("account_lifecycle") && l.contains("\"verb\":\"set-tier\""))
            .collect()
    };
    let recorded = moves(&c);
    assert_eq!(recorded.len(), 1, "exactly one set-tier record: {:?}", c.journal_lines());
    assert!(
        recorded[0].contains("\"tier\":\"demo\""),
        "the record must carry the tier the row landed at: {}",
        recorded[0]
    );
    assert_eq!(
        c.journal_lines().iter().filter(|l| l.contains("credential_write")).count(),
        credential_writes_before,
        "moving a tier is not a credential write: {:?}",
        c.journal_lines()
    );

    // The same command again: the row is already there, so the reply says so and NO second record
    // is appended — a ledger line for a no-op reads as an edit that did not happen.
    let again = c.run(&["account", "set-tier", "--id", &id, "--tier", "demo"], None, &[]);
    assert_eq!(exit_code(&again), rung(vike_cli::exit::Exit::Ok), "{}", stderr(&again));
    let text = stdout(&again);
    assert!(text.contains("unchanged"), "{text}");
    assert!(!text.contains("moved account"), "a no-op reported a move: {text}");
    assert_eq!(moves(&c).len(), 1, "a no-op appended a second record: {:?}", c.journal_lines());
}

// ── account set-exposure — the account's own exposure ceiling, end to end ───────────────────────
//
// The store half (`AccountEdit::SetMaxExposure`: the CHECK, the unknown id, the no-op) is
// `crates/vike-secrets/tests/accounts/max_exposure.rs`'s. These cases drive what only the shipped
// binary answers: the refusal of a figure the column cannot hold, the `old -> new` line, the
// listing, the clear, and one ledger line per change.

/// **A figure is written, listed, repeated as a no-op and cleared with `none`; a figure the column
/// cannot hold is refused with nothing written.**
#[test]
fn set_exposure_writes_lists_and_clears_the_accounts_own_ceiling() {
    let c = SetCase::new("set-exposure");
    c.seeded_keyed_row();
    let id = c.added_account("binance", "paper", "ALT").to_string();
    let records = |c: &SetCase| -> usize {
        c.journal_lines()
            .iter()
            .filter(|l| l.contains("account_lifecycle") && l.contains("\"verb\":\"set-exposure\""))
            .count()
    };

    for bad in ["0", "lots"] {
        let out =
            c.run(&["account", "set-exposure", "--id", &id, "--max-exposure", bad], None, &[]);
        assert_eq!(exit_code(&out), rung(vike_cli::exit::Exit::Usage), "{bad}: {}", stdout(&out));
        assert!(stderr(&out).contains("Nothing was written"), "{bad}: {}", stderr(&out));
    }
    let missing = c.run(&["account", "set-exposure", "--id", &id], None, &[]);
    assert_eq!(exit_code(&missing), rung(vike_cli::exit::Exit::Usage), "{}", stdout(&missing));
    assert_eq!(records(&c), 0, "a refusal wrote a ledger line: {:?}", c.journal_lines());

    let set = ["account", "set-exposure", "--id", id.as_str(), "--max-exposure", "5000"];
    let out = c.run(&set, None, &[]);
    assert_eq!(exit_code(&out), rung(vike_cli::exit::Exit::Ok), "{}", stderr(&out));
    let text = stdout(&out);
    assert!(text.contains("max_exposure: none (unbounded) -> 5000"), "{text}");
    assert!(text.contains(&format!("set account {id}'s max_exposure to 5000")), "{text}");
    assert_eq!(records(&c), 1, "exactly one set-exposure record: {:?}", c.journal_lines());

    let listed = stdout(&c.run(&["accounts"], None, &[]));
    let row = listed
        .lines()
        .find(|l| l.split_whitespace().next() == Some(id.as_str()))
        .unwrap_or_else(|| panic!("no row {id} in {listed}"));
    assert!(row.contains("5000"), "the listing must show the figure: {row}");

    let again = stdout(&c.run(&set, None, &[]));
    assert!(again.contains("unchanged"), "{again}");
    assert_eq!(records(&c), 1, "a no-op appended a second record: {:?}", c.journal_lines());

    let cleared =
        c.run(&["account", "set-exposure", "--id", &id, "--max-exposure", "none"], None, &[]);
    assert_eq!(exit_code(&cleared), rung(vike_cli::exit::Exit::Ok), "{}", stderr(&cleared));
    assert!(stdout(&cleared).contains("5000 -> none (unbounded)"), "{}", stdout(&cleared));
    assert_eq!(records(&c), 2, "the clear is a change of its own: {:?}", c.journal_lines());
}

/// **`--max-exposure` is refused on every other `account` action, not dropped** — typed beside
/// `add` it would read as a row born capped.
#[test]
fn max_exposure_is_refused_off_set_exposure() {
    let c = SetCase::new("set-exposure-off");
    let id = c.seeded_keyed_row().to_string();
    let out = c.run(
        &["account", "set-tier", "--id", &id, "--tier", "demo", "--max-exposure", "5"],
        None,
        &[],
    );
    assert_eq!(exit_code(&out), rung(vike_cli::exit::Exit::Usage), "{}", stdout(&out));
    assert!(stderr(&out).contains("set-exposure"), "{}", stderr(&out));
    let out = c.run(&["list", "--max-exposure", "5"], None, &[]);
    assert_eq!(exit_code(&out), rung(vike_cli::exit::Exit::Usage), "{}", stdout(&out));
}

/// `template` and `--file` are REFUSED BY NAME on the shipped binary — both went with the credential
/// FILE store on 2026-10-07, and a script that still types either is told what replaced it.
#[test]
fn template_and_file_are_refused_by_name_on_the_shipped_binary() {
    let out = Command::new(BIN)
        .args(["secrets", "template"])
        .stdin(Stdio::null())
        .env_remove("VIKE_SETTINGS_DIR")
        .output()
        .expect("the vike-cli binary must run");
    assert_eq!(exit_code(&out), rung(vike_cli::exit::Exit::Usage), "{}", stderr(&out));
    let e = stderr(&out);
    assert!(e.contains("`secrets template` was removed"), "{e}");
    assert!(e.contains("secrets init"), "the refusal must name the replacement: {e}");

    let out = Command::new(BIN)
        .args(["secrets", "list", "--file", "/tmp/anything"])
        .stdin(Stdio::null())
        .env_remove("VIKE_SETTINGS_DIR")
        .output()
        .expect("the vike-cli binary must run");
    assert_eq!(exit_code(&out), rung(vike_cli::exit::Exit::Usage), "{}", stderr(&out));
    assert!(stderr(&out).contains("--file was removed"), "{}", stderr(&out));
}
