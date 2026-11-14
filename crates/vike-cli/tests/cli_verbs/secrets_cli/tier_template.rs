//! `account set-tier` and `account set-exposure` end to end, and the refusals of the removed
//! `template` verb and `--file` flag.

use std::process::{Command, Stdio};

use super::support::{SetCase, exit_code, ok, rung};
use super::{BIN, stderr, stdout};

// ── account set-tier — the ARM, end to end ──────────────────────────────────────────────────────
//
// ⚠ **What is NOT re-tested here: the STORE.** `AccountEdit::SetTier` has its own cases in
// `crates/vike-secrets/tests/accounts/tier.rs` — a keyed row moving WITH its keys
// (`a_row_whose_key_names_spell_another_tier_moves_with_its_keys`), the unlabelled and taken-label
// collisions, the unknown tier, the unknown id, the no-op — and none of them drives
// `run_account`'s `set-tier` arm end to end. So these cases cover exactly the three things only
// the shipped binary can answer — what the arm PRINTS, that it REFUSES the label flags rather than
// dropping them, and that its rehearsal writes nothing — and assert nothing about the transaction
// those store tests already own.
//
// The grammar refusals (`--tier` missing, `--tier` unknown) are covered by the unit tests beside
// the parser, which drive BOTH actions that take the flag; they are not repeated here either.

/// **The REHEARSAL prints the whole move and writes nothing**, over a row that owns credential
/// keys — the shape that makes every line of the arm's output appear at once.
///
/// Four of those lines are the arm's entire contract with an operator and none of them is visible
/// to a store test: the STORE PATH (named before anything is validated, so a rehearsal on the wrong
/// box cannot read like one on the right box), the row ECHO (the only check there is on `--id`),
/// the `old -> new` tier, and the two consequences — that the credential keys MOVE WITH the row
/// (they are filed against its id and no name spells a tier: decision 0156 item 6), and that the
/// ACTIVE row ARMS at the new tier from the next restart.
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
    assert!(
        text.contains(&vike_secrets::account_credential_name("binance", id, "API_KEY")),
        "the echo must list the row's keys: {text}"
    );
    // `demo` carries binance's own word for it (decision 0156 item 8: `vike_venues::venue_demo_word`).
    assert!(
        text.contains("tier: demo (binance: demo trading) -> live"),
        "the move must be printed both ways: {text}"
    );
    assert!(text.contains("they MOVE WITH IT"), "a keyed row must be told its keys go too: {text}");
    assert!(text.contains("presents them at `live`"), "…at the tier it moves TO: {text}");
    assert!(
        text.contains(&format!("vike-cli secrets set --account {id} <FIELD>")),
        "…and how to replace keys the new tier's network refuses: {text}"
    );
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

/// **`activate --id N --tier demo` on a live row ignored `--tier` and armed it at LIVE**, and a running
/// tradehub starts an active row within a second. `--tier` and `--venue` are refused on every action that
/// does not read them, before anything is written; the row stays as it was.
#[test]
fn an_account_action_refuses_the_tier_and_venue_flags_it_does_not_read() {
    let c = SetCase::new("account-dropped-flags");
    let id = c.seeded_keyed_row().to_string();
    ok(&c.run(&["account", "deactivate", "--id", &id], None, &[]));
    let before_store = c.read_store();
    let before_journal = c.journal_lines().len();

    for (action, flag, args) in [
        ("activate", "--tier", vec!["--tier", "demo"]),
        ("activate", "--venue", vec!["--venue", "bybit"]),
        ("deactivate", "--tier", vec!["--tier", "demo"]),
        ("deactivate", "--venue", vec!["--venue", "bybit"]),
        ("rename", "--tier", vec!["--label", "ALT", "--tier", "demo"]),
        ("rename", "--venue", vec!["--label", "ALT", "--venue", "bybit"]),
        ("remove", "--tier", vec!["--confirm", id.as_str(), "--tier", "live"]),
        ("remove", "--venue", vec!["--confirm", id.as_str(), "--venue", "bybit"]),
        ("set-tier", "--venue", vec!["--tier", "live", "--venue", "bybit"]),
        ("set-exposure", "--venue", vec!["--max-exposure", "5000", "--venue", "bybit"]),
        ("set-leverage", "--venue", vec!["--leverage", "5", "--venue", "bybit"]),
    ] {
        let mut line = vec!["account", action, "--id", id.as_str()];
        line.extend_from_slice(&args);
        let out = c.run(&line, None, &[]);
        let e = stderr(&out);
        assert_eq!(exit_code(&out), rung(vike_cli::exit::Exit::Usage), "{line:?}: {e}");
        assert!(e.contains(&format!("`account {action}` does not take {flag}")), "{line:?}: {e}");
        assert!(e.contains("Nothing was written"), "{line:?}: {e}");
    }
    assert_eq!(c.read_store(), before_store, "a refusal touched the store");
    assert_eq!(c.journal_lines().len(), before_journal, "a refusal wrote a ledger line");

    // …and the two that read them still do.
    let moved =
        c.run(&["account", "set-tier", "--id", &id, "--tier", "live", "--dry-run"], None, &[]);
    assert_eq!(exit_code(&moved), rung(vike_cli::exit::Exit::Ok), "{}", stderr(&moved));
    let add = ["account", "add", "--venue", "bybit", "--tier", "demo", "--no-label", "--dry-run"];
    let added = c.run(&add, None, &[]);
    assert_eq!(exit_code(&added), rung(vike_cli::exit::Exit::Ok), "{}", stderr(&added));
}

/// **A deactivate's warning names the book the close must name** (decision 0254): a bare `market-exit <venue>` is
/// refused on a venue of several accounts, so the sentence an operator copies is one account's.
#[test]
fn a_deactivate_names_the_book_its_close_must_name() {
    let c = SetCase::new("account-deactivate-book-unlabelled");
    let binance = c.seeded_keyed_row().to_string();
    let out = c.run(&["account", "deactivate", "--id", &binance], None, &[]);
    ok(&out);
    assert!(stdout(&out).contains("market-exit binance/DEFAULT"), "{}", stdout(&out));

    let c = SetCase::new("account-deactivate-book-labelled");
    let labelled = c.seeded_books();
    for id in &labelled {
        let id = id.to_string();
        let out = c.run(&["account", "deactivate", "--id", &id], None, &[]);
        ok(&out);
        let text = stdout(&out);
        assert!(
            text.contains("market-exit dukascopy/SWISS")
                || text.contains("market-exit dukascopy/EUROPE"),
            "{text}"
        );
    }
}

/// **`--label`, `--no-label` and `--confirm` were dropped the same way** on the actions that do not read
/// them: an operator who typed `activate --id N --label ALT` or `deactivate --id N --confirm N` reads as
/// having named or confirmed something. Each is refused before anything is written.
#[test]
fn an_account_action_refuses_the_label_and_confirm_flags_it_does_not_read() {
    let c = SetCase::new("account-dropped-label");
    let id = c.seeded_keyed_row().to_string();
    ok(&c.run(&["account", "deactivate", "--id", &id], None, &[]));
    let before_store = c.read_store();
    let before_journal = c.journal_lines().len();

    for (action, flag, args) in [
        ("activate", "--label", vec!["--label", "ALT"]),
        ("activate", "--no-label", vec!["--no-label"]),
        ("activate", "--confirm", vec!["--confirm", id.as_str()]),
        ("deactivate", "--label", vec!["--label", "ALT"]),
        ("deactivate", "--no-label", vec!["--no-label"]),
        ("deactivate", "--confirm", vec!["--confirm", id.as_str()]),
        ("remove", "--label", vec!["--confirm", id.as_str(), "--label", "ALT"]),
        ("remove", "--no-label", vec!["--confirm", id.as_str(), "--no-label"]),
        ("rename", "--confirm", vec!["--label", "ALT", "--confirm", id.as_str()]),
        ("set-tier", "--confirm", vec!["--tier", "live", "--confirm", id.as_str()]),
        ("set-exposure", "--confirm", vec!["--max-exposure", "5", "--confirm", id.as_str()]),
        ("set-leverage", "--confirm", vec!["--leverage", "5", "--confirm", id.as_str()]),
    ] {
        let mut line = vec!["account", action, "--id", id.as_str()];
        line.extend_from_slice(&args);
        let out = c.run(&line, None, &[]);
        let e = stderr(&out);
        assert_eq!(exit_code(&out), rung(vike_cli::exit::Exit::Usage), "{line:?}: {e}");
        assert!(e.contains(&format!("`account {action}` does not take {flag}")), "{line:?}: {e}");
        assert!(e.contains("Nothing was written"), "{line:?}: {e}");
    }
    let add = ["account", "add", "--venue", "bybit", "--tier", "demo", "--no-label"];
    let out = c.run(&[&add[..], &["--confirm", "1", "--dry-run"]].concat(), None, &[]);
    assert!(stderr(&out).contains("`account add` does not take --confirm"), "{}", stderr(&out));

    assert_eq!(c.read_store(), before_store, "a refusal touched the store");
    assert_eq!(c.journal_lines().len(), before_journal, "a refusal wrote a ledger line");
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
    // A KEYLESS row — `account add` files no credential — so its reply owes no keys sentence; the
    // keyed row's is the rehearsal case's above.
    let id = c.added_account("binance", "paper", "ALT").to_string();
    // ⚠ What is under test is what the MOVE adds to the ledger, so the baseline is taken rather
    // than assumed: a test asserting an empty ledger here would be asserting the fixture.
    let credential_writes_before =
        c.journal_lines().iter().filter(|l| l.contains("credential_write")).count();

    let out = c.run(&["account", "set-tier", "--id", &id, "--tier", "demo"], None, &[]);
    assert_eq!(exit_code(&out), rung(vike_cli::exit::Exit::Ok), "{}", stderr(&out));
    let text = stdout(&out);
    assert!(text.contains("tier: paper -> demo (binance: demo trading)"), "{text}");
    assert!(text.contains(&format!("moved account {id} to tier demo")), "{text}");
    assert!(
        !text.contains("MOVE WITH IT"),
        "a KEYLESS row must not be told about keys it does not own: {text}"
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

/// **Where the operator picks a tier, the venue's own word for our `demo` is beside it, and
/// `--tier paper` on IBKR is warned** (decision 0156 item 8): IBKR calls its demo environment a
/// "paper account", so an operator meaning that account and typing `paper` would get vike's local
/// simulator. A WARNING, not a refusal: the rehearsal still succeeds, and writes nothing.
#[test]
fn account_add_shows_the_venues_word_and_warns_paper_on_ibkr() {
    let c = SetCase::new("add-venue-word");
    c.seeded_keyed_row();
    let before_store = c.read_store();
    let add = |tier: &str| {
        let out = c.run(
            &["account", "add", "--venue", "ibkr", "--tier", tier, "--no-label", "--dry-run"],
            None,
            &[],
        );
        assert_eq!(exit_code(&out), rung(vike_cli::exit::Exit::Ok), "{}", stderr(&out));
        stdout(&out)
    };

    let paper = add("paper");
    assert!(paper.contains("tier=paper "), "{paper}");
    assert!(paper.contains("LOCAL simulator"), "the warning: {paper}");
    assert!(paper.contains("--tier demo"), "…and the tier that reaches the venue: {paper}");

    let demo = add("demo");
    assert!(demo.contains("tier=demo (ibkr: paper account)"), "the venue's own word: {demo}");
    assert!(!demo.contains("LOCAL simulator"), "only `paper` is warned: {demo}");

    assert_eq!(c.read_store(), before_store, "a rehearsal touched the store");
}

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

/// **The `accounts` listing carries a `leverage` column beside `max exposure`: `none` until a figure
/// is stated, the figure (`10x`) after `set-leverage`, `none` again once cleared** (decision 0181).
#[test]
fn accounts_lists_the_leverage_an_account_states() {
    let c = SetCase::new("accounts-leverage");
    c.seeded_keyed_row();
    let id = c.added_account("hyperliquid", "paper", "ALT").to_string();
    let cell = |c: &SetCase| -> String {
        let listed = stdout(&c.run(&["accounts"], None, &[]));
        assert!(
            listed.lines().any(|l| l.contains("max exposure") && l.contains("leverage")),
            "no `leverage` header in {listed}"
        );
        let row = listed
            .lines()
            .find(|l| l.split_whitespace().next() == Some(id.as_str()))
            .unwrap_or_else(|| panic!("no row {id} in {listed}"));
        // By token, not offset (`AccountTier`'s Display ignores a width, so the tier cell is
        // unpadded): id venue tier label `(not yet known)` (3 tokens) `max exposure` `leverage`.
        row.split_whitespace().nth(8).expect("a leverage cell").to_string()
    };

    assert_eq!(cell(&c), "none", "nothing stated reads `none`");
    let set = c.run(&["account", "set-leverage", "--id", &id, "--leverage", "10"], None, &[]);
    assert_eq!(exit_code(&set), rung(vike_cli::exit::Exit::Ok), "{}", stderr(&set));
    assert_eq!(cell(&c), "10x");
    let clear = c.run(&["account", "set-leverage", "--id", &id, "--leverage", "none"], None, &[]);
    assert_eq!(exit_code(&clear), rung(vike_cli::exit::Exit::Ok), "{}", stderr(&clear));
    assert_eq!(cell(&c), "none");
}

/// The datahub's KEPT bybit list in `c`'s project (`<project>/instruments/bybit.json`), holding the inverse
/// perpetual `BTCUSD.P`: what `account set-symbol` checks a bybit name against (`vike_catalog::venue_engine_symbol`).
fn kept_bybit_list(c: &SetCase) {
    let inverse = vike_model::Contract::Perp(vike_model::ContractPerp {
        margining: vike_model::Margining::Inverse,
        settle: "BTC".into(),
        value_currency: None,
        funding_interval_ms: None,
        settling_ms: None,
    });
    let list = vike_catalog::VenueInstrumentList {
        venue: "bybit".into(),
        refreshed_ms: Some(1),
        truncated: false,
        last_error: None,
        last_try_ms: Some(1),
        instruments: vec![vike_model::Instrument::new("bybit", "BTCUSD.P", "BTC", "USD", inverse)],
        added_by_operator: Vec::new(),
    };
    let dir = c.dir().join("instruments");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(vike_catalog::kept_file(&dir, "bybit"), serde_json::to_vec(&list).unwrap())
        .unwrap();
}

/// **`account set-symbol --id N SYMBOL` states the instrument an account's engine is mounted on**: written,
/// listed (a `symbol` column after `leverage`), repeated as a no-op, cleared with `none`, one ledger line per
/// change; the default (unlabelled) account states one like any other (decision 0156's amendment of 2026-11-13);
/// refused BY NAME, with nothing written, on a venue whose engine is mounted on no symbol (polymarket) and for a
/// token that is no symbol; an inverse name says the coin its books are kept in.
#[test]
fn set_symbol_writes_lists_clears_and_refuses_by_name() {
    let c = SetCase::new("set-symbol");
    kept_bybit_list(&c);
    let default_row = c.seeded_keyed_row();
    let id = c.added_account("bybit", "demo", "INV").to_string();
    let records = |c: &SetCase| -> usize {
        c.journal_lines()
            .iter()
            .filter(|l| l.contains("account_lifecycle") && l.contains("\"verb\":\"set-symbol\""))
            .count()
    };
    let usage = rung(vike_cli::exit::Exit::Usage);

    let set = ["account", "set-symbol", "--id", id.as_str(), "BTCUSD"];
    let out = c.run(&set, None, &[]);
    assert_eq!(exit_code(&out), rung(vike_cli::exit::Exit::Ok), "{}", stderr(&out));
    let text = stdout(&out);
    assert!(text.contains("symbol: none (does not run) -> BTCUSD"), "{text}");
    assert!(text.contains("keeps its books in BTC"), "the inverse coin is said: {text}");
    assert!(text.contains(&format!("set account {id}'s symbol to BTCUSD")), "{text}");
    assert_eq!(records(&c), 1, "exactly one set-symbol record: {:?}", c.journal_lines());

    let listed = stdout(&c.run(&["accounts"], None, &[]));
    assert!(listed.lines().any(|l| l.contains("leverage") && l.contains("symbol")), "{listed}");
    let row = listed
        .lines()
        .find(|l| l.split_whitespace().next() == Some(id.as_str()))
        .unwrap_or_else(|| panic!("no row {id} in {listed}"));
    // By token: id venue tier label `(not yet known)` (3) `max exposure` `leverage` `symbol`.
    assert_eq!(row.split_whitespace().nth(9), Some("BTCUSD"), "the listing shows it: {row}");

    let again = stdout(&c.run(&set, None, &[]));
    assert!(again.contains("unchanged"), "{again}");
    assert_eq!(records(&c), 1, "a no-op appended a second record: {:?}", c.journal_lines());

    // The default (unlabelled) account states its symbol like any other.
    let default_row = default_row.to_string();
    let stated = c.run(&["account", "set-symbol", "--id", &default_row, "BTCUSDT"], None, &[]);
    assert_eq!(exit_code(&stated), rung(vike_cli::exit::Exit::Ok), "{}", stderr(&stated));
    assert!(
        stdout(&stated).contains(&format!("set account {default_row}'s symbol to BTCUSDT")),
        "{}",
        stdout(&stated)
    );
    assert_eq!(records(&c), 2, "the default account's symbol is a record: {:?}", c.journal_lines());

    // Refused by name, nothing written: an account-wide venue, a non-symbol.
    let polymarket = c.added_account("polymarket", "demo", "ALT").to_string();
    for (args, want) in [
        (
            vec!["account", "set-symbol", "--id", polymarket.as_str(), "BTCUSD"],
            "`BTCUSD` is not a polymarket",
        ),
        (
            vec!["account", "set-symbol", "--id", id.as_str(), "BTC USD"],
            "not one this column accepts",
        ),
        (vec!["account", "set-symbol", "--id", id.as_str()], "needs a SYMBOL"),
    ] {
        let out = c.run(&args, None, &[]);
        assert_eq!(exit_code(&out), usage, "{args:?}: {}", stdout(&out));
        assert!(stderr(&out).contains(want), "{args:?}: {}", stderr(&out));
        assert!(stderr(&out).contains("Nothing was written"), "{args:?}: {}", stderr(&out));
    }
    assert_eq!(records(&c), 2, "a refusal wrote a ledger line: {:?}", c.journal_lines());

    let cleared = c.run(&["account", "set-symbol", "--id", &id, "none"], None, &[]);
    assert_eq!(exit_code(&cleared), rung(vike_cli::exit::Exit::Ok), "{}", stderr(&cleared));
    assert!(stdout(&cleared).contains("BTCUSD -> none (does not run)"), "{}", stdout(&cleared));
    assert_eq!(records(&c), 3, "the clear is a change of its own: {:?}", c.journal_lines());
}

/// **`account set-fact --id N FACT VALUE` states one fact of how an account's engine is mounted** (decision 0156's
/// amendment of 2026-11-13): written with one ledger line, repeated as a no-op, cleared with `none`; an unknown fact
/// (refused naming the list), a missing value and a token that is no value are refused by name with nothing written,
/// and a lane twin's symbol on a LABELLED row is refused in the store's words, nothing written.
#[test]
fn set_fact_writes_clears_and_refuses_by_name() {
    let c = SetCase::new("set-fact");
    let parent = c.seeded_keyed_row().to_string();
    let alt = c.added_account("binance", "demo", "ALT").to_string();
    let records = |c: &SetCase| -> usize {
        c.journal_lines()
            .iter()
            .filter(|l| l.contains("account_lifecycle") && l.contains("\"verb\":\"set-fact\""))
            .count()
    };

    let set = ["account", "set-fact", "--id", parent.as_str(), "lane_twin_symbol", "BTCUSDT.P"];
    let out = c.run(&set, None, &[]);
    assert_eq!(exit_code(&out), rung(vike_cli::exit::Exit::Ok), "{}", stderr(&out));
    let text = stdout(&out);
    assert!(text.contains("lane_twin_symbol: none -> BTCUSDT.P"), "{text}");
    assert!(
        text.contains(&format!("set account {parent}'s lane_twin_symbol to BTCUSDT.P")),
        "{text}"
    );
    assert_eq!(records(&c), 1, "exactly one set-fact record: {:?}", c.journal_lines());
    let again = stdout(&c.run(&set, None, &[]));
    assert!(again.contains("unchanged"), "{again}");
    assert_eq!(records(&c), 1, "a no-op appended a second record: {:?}", c.journal_lines());

    let usage = rung(vike_cli::exit::Exit::Usage);
    for (args, want) in [
        (
            vec!["account", "set-fact", "--id", parent.as_str(), "lane_twin", "BTCUSDT.P"],
            "`lane_twin` is no account fact: one of lane_twin_symbol | currency_symbol",
        ),
        (
            vec!["account", "set-fact", "--id", parent.as_str(), "lane_twin_symbol"],
            "needs a FACT and a VALUE",
        ),
        (
            vec!["account", "set-fact", "--id", parent.as_str(), "currency_symbol", "EUR GBP"],
            "not one an account fact holds",
        ),
    ] {
        let out = c.run(&args, None, &[]);
        assert_eq!(exit_code(&out), usage, "{args:?}: {}", stdout(&out));
        assert!(stderr(&out).contains(want), "{args:?}: {}", stderr(&out));
        assert!(stderr(&out).contains("Nothing was written"), "{args:?}: {}", stderr(&out));
    }
    let refused = c.run(
        &["account", "set-fact", "--id", alt.as_str(), "lane_twin_symbol", "BTCUSDT.P"],
        None,
        &[],
    );
    assert_eq!(exit_code(&refused), rung(vike_cli::exit::Exit::Failed), "{}", stdout(&refused));
    assert!(stderr(&refused).contains("cannot state `lane_twin_symbol`"), "{}", stderr(&refused));
    assert!(stderr(&refused).contains("Nothing was written"), "{}", stderr(&refused));
    assert_eq!(records(&c), 1, "a refusal wrote a ledger line: {:?}", c.journal_lines());

    let cleared = c.run(
        &["account", "set-fact", "--id", parent.as_str(), "lane_twin_symbol", "none"],
        None,
        &[],
    );
    assert_eq!(exit_code(&cleared), rung(vike_cli::exit::Exit::Ok), "{}", stderr(&cleared));
    assert!(stdout(&cleared).contains("BTCUSDT.P -> none"), "{}", stdout(&cleared));
    assert_eq!(records(&c), 2, "the clear is a change of its own: {:?}", c.journal_lines());
}

/// **`account set-symbol` reads the venue's own grammar, the control door's (`vike_catalog::canonical_symbol`), and
/// bybit's KEPT list**: `BTCUSDX` (no row of it) names no bybit book and `BTC-PERPETUAL` is a deribit name, both
/// refused by name with nothing written (no ledger line); a name the venue spells otherwise is STORED in its spelling
/// (`btcusd` is `BTCUSD`), so the engine is mounted on the name the venue lists.
#[test]
fn set_symbol_refuses_a_name_the_venue_does_not_name_and_stores_its_spelling() {
    let c = SetCase::new("set-symbol-grammar");
    kept_bybit_list(&c);
    c.seeded_keyed_row();
    let id = c.added_account("bybit", "demo", "INV").to_string();
    let records = |c: &SetCase| -> usize {
        c.journal_lines()
            .iter()
            .filter(|l| l.contains("account_lifecycle") && l.contains("\"verb\":\"set-symbol\""))
            .count()
    };
    for (typed, want) in [
        ("BTCUSDX", "`BTCUSDX` names no bybit book"),
        ("BTC-PERPETUAL", "`BTC-PERPETUAL` is not an exchange symbol"),
    ] {
        let out = c.run(&["account", "set-symbol", "--id", &id, typed], None, &[]);
        assert_eq!(exit_code(&out), rung(vike_cli::exit::Exit::Usage), "{typed}: {}", stdout(&out));
        assert!(stderr(&out).contains(want), "{typed}: {}", stderr(&out));
        assert!(stderr(&out).contains("Nothing was written"), "{typed}: {}", stderr(&out));
    }
    assert_eq!(records(&c), 0, "a refusal wrote a ledger line: {:?}", c.journal_lines());

    let out = c.run(&["account", "set-symbol", "--id", &id, "btcusd"], None, &[]);
    assert_eq!(exit_code(&out), rung(vike_cli::exit::Exit::Ok), "{}", stderr(&out));
    assert!(stdout(&out).contains("symbol: none (does not run) -> BTCUSD"), "{}", stdout(&out));
    let listed = stdout(&c.run(&["accounts"], None, &[]));
    let row = listed
        .lines()
        .find(|l| l.split_whitespace().next() == Some(id.as_str()))
        .unwrap_or_else(|| panic!("no row {id} in {listed}"));
    assert_eq!(
        row.split_whitespace().nth(9),
        Some("BTCUSD"),
        "stored in the venue's spelling: {row}"
    );
}

/// **`account set-leverage` reads `vike_secrets::LEVERAGE_VENUES` (decision 0182)**: the posting venues
/// take a figure at tier `demo` only, as a whole number; a `live` row, a fraction, aster and deribit
/// are refused in words, before anything is written (every case is a rehearsal, so the store is not
/// touched); clearing is never refused; hyperliquid keeps accepting any tier.
#[test]
fn set_leverage_follows_the_venue_table() {
    let c = SetCase::new("leverage-venue-table");
    let demo_default = c.seeded_keyed_row();
    let ask = |id: i64, figure: &str| {
        c.run(
            &[
                "account",
                "set-leverage",
                "--id",
                &id.to_string(),
                "--leverage",
                figure,
                "--dry-run",
            ],
            None,
            &[],
        )
    };
    let usage = rung(vike_cli::exit::Exit::Usage);
    let ok = rung(vike_cli::exit::Exit::Ok);

    // Open: the posting venues at demo, said with the one place the venue's account changes.
    for venue in ["bybit", "okx", "binance"] {
        let id = c.added_account(venue, "demo", "ALT");
        let out = ask(id, "10");
        assert_eq!(exit_code(&out), ok, "{venue} demo: {}", stderr(&out));
        assert!(stdout(&out).contains("POSTS the account's leverage"), "{venue}: {}", stdout(&out));
    }
    // Closed by tier: bybit and okx stay demo-only (the owner opened binance's live tier alone).
    for venue in ["bybit", "okx"] {
        let id = c.added_account(venue, "live", "LIVE1");
        let out = ask(id, "10");
        assert_eq!(exit_code(&out), usage, "{venue} live: {}", stdout(&out));
        assert!(stderr(&out).contains("demo only"), "{venue}: {}", stderr(&out));
        assert!(stderr(&out).contains("Nothing was written"), "{}", stderr(&out));
    }
    // …binance's live tier is open for the lane TWIN only (2026-10-19, narrowed by the re-review): a LABELLED
    // live row is REFUSED in words (its figure would be ignored, nothing written to the venue); the DEFAULT live
    // row is accepted and says, in capitals, that the figure goes to the REAL account and what it starts (the
    // lane twin, a second real-money engine). A demo row says neither.
    let labelled = c.added_account("binance", "live", "LIVE1");
    let out = ask(labelled, "10");
    assert_eq!(exit_code(&out), usage, "binance live labelled: {}", stdout(&out));
    for want in
        ["DEFAULT (unlabelled) account only", "binance#PERP", "`LIVE1`", "Nothing was written"]
    {
        assert!(stderr(&out).contains(want), "{want:?} missing: {}", stderr(&out));
    }
    let out = ask(demo_default, "10");
    assert_eq!(exit_code(&out), ok, "{}", stderr(&out));
    assert!(
        !stdout(&out).contains("REAL (mainnet)"),
        "a demo row is not mainnet: {}",
        stdout(&out)
    );
    // The DEFAULT live row needs the default demo row off first: one ACTIVE row answers
    // `binance/DEFAULT` (decision 0274).
    let off = c.run(&["account", "deactivate", "--id", &demo_default.to_string()], None, &[]);
    assert_eq!(exit_code(&off), ok, "{}", stderr(&off));
    let default_live = c.added_unlabelled_account("binance", "live");
    let out = ask(default_live, "10");
    assert_eq!(exit_code(&out), ok, "binance live default: {}", stderr(&out));
    for want in [
        "REAL (mainnet)",
        "second real-money engine",
        "binance#PERP",
        "lane_twin_symbol",
        "share this account's exposure ceiling",
    ] {
        assert!(stdout(&out).contains(want), "{want:?} missing: {}", stdout(&out));
    }
    let out = ask(default_live, "2.5");
    assert_eq!(exit_code(&out), usage, "a fraction is still refused at live: {}", stdout(&out));
    assert!(stderr(&out).contains("whole number"), "{}", stderr(&out));
    // A paper row of binance is refused naming the open tiers.
    let paper = c.added_account("binance", "paper", "PAPER1");
    let out = ask(paper, "10");
    assert_eq!(exit_code(&out), usage, "{}", stdout(&out));
    assert!(stderr(&out).contains("demo or live"), "{}", stderr(&out));
    // A fraction on a whole-number venue.
    let id = c.added_account("binance", "demo", "FRAC");
    let out = ask(id, "2.5");
    assert_eq!(exit_code(&out), usage, "{}", stdout(&out));
    assert!(stderr(&out).contains("whole number"), "{}", stderr(&out));
    // aster is coded and closed; deribit has no leverage number.
    let id = c.added_account("aster", "live", "ALT");
    let out = ask(id, "5");
    assert_eq!(exit_code(&out), usage, "{}", stdout(&out));
    assert!(stderr(&out).contains("not open at any tier"), "{}", stderr(&out));
    let id = c.added_account("deribit", "demo", "ALT");
    let out = ask(id, "5");
    assert_eq!(exit_code(&out), usage, "{}", stdout(&out));
    assert!(stderr(&out).contains("no leverage number"), "{}", stderr(&out));
    // Clearing is never refused, whatever the row.
    let out = ask(id, "none");
    assert_eq!(exit_code(&out), ok, "{}", stderr(&out));
    // Hyperliquid is exactly as shipped: any tier, a fraction too.
    for tier in ["paper", "demo", "live"] {
        let id = c.added_account("hyperliquid", tier, &format!("HL{tier}").to_uppercase());
        let out = ask(id, "2.5");
        assert_eq!(exit_code(&out), ok, "hyperliquid {tier}: {}", stderr(&out));
        assert!(
            stdout(&out).contains("venue's own leverage on the account is not changed"),
            "{}",
            stdout(&out)
        );
    }
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
