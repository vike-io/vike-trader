use super::account_lifecycle::{
    arming_consequence, max_exposure_missing_message, parse_max_exposure, tier_missing_message,
    tier_unknown_message,
};
use super::*;

/// **MAJOR 1 of the 2026-09-23 task-3/7 review.** The `sim` -> `paper` rename (ruling 7) put
/// `paper` INTO the tier vocabulary (now `vike_model::AccountTier`), and both of `account add`'s tier refusals
/// denied it by name while printing the very roster that now contains it — a five-path sweep
/// of the rename's operator-facing prose that stopped one call site short of its own
/// conclusion. This calls the exact functions production calls, and is derived from the
/// roster rather than a hand-copied word list, so the NEXT tier rename reddens this instead of
/// rotting the way this one did.
///
/// ⚠ Driven over BOTH actions that take `--tier` since stage 5 added `set-tier`, because the
/// messages are now parameterised on the action and an action that named the wrong command
/// would send the operator of `set-tier` to `add`.
#[test]
fn the_tier_refusals_do_not_deny_a_tier_the_roster_contains() {
    let roster = vike_model::AccountTier::legal_words();
    for action in ["add", "set-tier"] {
        for msg in [tier_missing_message(action), tier_unknown_message(action, "not-a-real-tier")] {
            assert!(msg.contains(&roster), "must print the real roster: {msg}");
            assert!(
                msg.contains(&format!("account {action}")),
                "the refusal must name the action it came from: {msg}"
            );
            // Decision 0156 item 6: no credential write creates an account or chooses its tier,
            // so no tier refusal may say a credential "mints" one.
            assert!(!msg.contains("mints"), "a credential write creates no account: {msg}");
            for tier in vike_model::AccountTier::ALL {
                assert!(
                    !msg.contains(&format!("`{tier}` is not"))
                        && !msg.contains(&format!("no `{tier}`")),
                    "{tier} is a tier but this refusal denies it: {msg}"
                );
            }
        }
    }
}

/// **The claim `account_action_missing`'s doc has always made — *spelled once* — now holds, and
/// this is what holds it.**
///
/// It was false when it was written: `parse`'s second-positional refusal carried a hand copy
/// of the action list and the comment above it called an action *"one of five words"*. Adding
/// `set-tier` is exactly the edit that drifts a copy like that, so both refusals now render
/// [`ACCOUNT_ACTIONS`] and this test fails if either stops — and it is derived from the array
/// rather than from a word list of its own, so the SEVENTH action reddens it instead of
/// rotting it.
///
/// ⚠ **DECLARED BOUND: this holds the LIST against the two REFUSALS and the PARSER, and it
/// does not reach `run_account`'s `match`.** Every arm of that match opens a store and writes,
/// so proving *the dispatcher answers for every listed action* from a unit test would mean
/// building a migrated store here; `crates/vike-cli/tests/secrets_cli.rs` is where a verb is
/// driven end to end. What is claimed here is exactly what is checked — a listed action parses
/// as an action, and both refusals print the list the array holds.
#[test]
fn the_account_action_list_is_spelled_once() {
    fn parse_of(argv: &[&str]) -> Result<Args, args::Stop> {
        parse(argv.iter().map(|s| (*s).to_string()))
    }

    let rendered = ACCOUNT_ACTIONS.join(" | ");
    assert!(rendered.contains("set-tier"), "the array must carry stage 5's action: {rendered}");

    let missing = account_action_missing();
    assert!(missing.contains(&rendered), "the no-action refusal must render the list: {missing}");

    // The parse-site twin — the copy this branch had to remove. A SECOND positional is what
    // prints it there, and it printed a HAND-WRITTEN list until stage 5.
    let second = parse_of(&["account", "add", "rename"])
        .expect_err("a second positional is a command that has not decided what it asks");
    assert!(second.contains(&rendered), "the parse refusal must render the SAME list: {second}");

    for action in ACCOUNT_ACTIONS {
        let args = parse_of(&["account", action]).expect("a listed action must parse");
        assert_eq!(args.account_action.as_deref(), Some(action));
    }
}

/// **`--max-exposure` reads exactly what the column's CHECK admits, plus the word `none`.**
/// `none` clears (any case); a finite figure `> 0` is kept; zero, a negative, `NaN`, an infinity,
/// an empty token and a word are refused — and the refusal names `none` so the operator can see
/// how to clear one on purpose.
#[test]
fn the_max_exposure_flag_reads_a_positive_figure_or_none_and_nothing_else() {
    for word in ["none", "NONE", " None "] {
        assert_eq!(parse_max_exposure(word), Ok(None), "{word:?} clears the ceiling");
    }
    let five_k = parse_max_exposure("5000").expect("a positive figure is accepted");
    assert_eq!(five_k.map(vike_secrets::MaxExposure::get), Some(5000.0));
    for bad in ["0", "-1", "NaN", "inf", "", "lots"] {
        let e = parse_max_exposure(bad).expect_err(bad);
        assert!(e.contains("`none`"), "the refusal must say how to clear one: {e}");
        assert!(e.contains("Nothing was written"), "{e}");
    }
    assert!(max_exposure_missing_message().contains("`none`"));
}

/// **The lifecycle replies say the TRUTH about arming**: a non-paper active row trades from the
/// next restart, `live` is called real money, the two-active-tiers stop is named, and the
/// deactivate verb is the way out. A `paper` row is said to arm nothing real. None of them may
/// point at the deleted per-venue ceiling.
#[test]
fn the_arming_consequence_names_the_tier_the_conflict_and_the_way_out() {
    let live = arming_consequence("binance", vike_model::AccountTier::Live, "7");
    assert!(live.contains("ARMS") && live.contains("REAL MONEY"), "{live}");
    assert!(live.contains("NEITHER"), "the TierConflict stop must be named: {live}");
    assert!(live.contains("account deactivate --id 7"), "{live}");
    let demo = arming_consequence("bybit", vike_model::AccountTier::Demo, "N");
    assert!(demo.contains("ARMS") && !demo.contains("REAL MONEY"), "{demo}");
    let paper = arming_consequence("okx", vike_model::AccountTier::Paper, "3");
    assert!(paper.contains("paper simulator") && !paper.contains("ARMS"), "{paper}");
    assert!(
        !live.contains("ANY label"),
        "an account-scoped venue keeps the per-label rule: {live}"
    );
    for msg in [live, demo, paper, tier_missing_message("add"), tier_unknown_message("add", "x")] {
        assert!(!msg.contains("policy.venues"), "the per-venue ceiling is gone: {msg}");
    }
}

/// **The ibkr scope is named at the write** (decision 0140, item 1): one session per box, so an
/// ACTIVE ibkr row at another tier under ANY label holds every ibkr account at paper. The write is
/// still not refused — the sentence is the whole of it.
#[test]
fn the_arming_consequence_names_the_ibkr_scope() {
    for tier in [vike_model::AccountTier::Demo, vike_model::AccountTier::Live] {
        let msg = arming_consequence("ibkr", tier, "4");
        assert!(msg.contains("ARMS") && msg.contains("account deactivate --id 4"), "{msg}");
        assert!(
            msg.contains(
                "another ACTIVE ibkr row at another tier, under ANY label, holds every \
                          ibkr account at paper"
            ),
            "{msg}"
        );
        assert!(!msg.contains("of this venue and label"), "not the per-label rule: {msg}");
    }
}

/// **The venue's own word for our `demo` rides beside the tier where the operator picks it**
/// (decision 0156 item 8): `demo (ibkr: paper account)`, from `vike_model::venue_demo_word`. Only
/// `demo` carries it — `paper` is vike's simulator and `live` is mainnet on every venue — and a
/// venue with no demo environment (polymarket) prints the tier alone.
#[test]
fn the_tier_carries_the_venues_own_word_for_demo() {
    use super::account_lifecycle::tier_with_venue_word;
    use vike_model::AccountTier::{Demo, Live, Paper};
    assert_eq!(tier_with_venue_word("ibkr", Demo), "demo (ibkr: paper account)");
    assert_eq!(tier_with_venue_word("hyperliquid", Demo), "demo (hyperliquid: testnet)");
    assert_eq!(tier_with_venue_word("oanda", Demo), "demo (oanda: practice)");
    assert_eq!(tier_with_venue_word("ibkr", Paper), "paper");
    assert_eq!(tier_with_venue_word("ibkr", Live), "live");
    assert_eq!(tier_with_venue_word("polymarket", Demo), "demo", "no demo of its own");
    for venue in vike_model::VENUES {
        let shown = tier_with_venue_word(venue, Demo);
        assert!(shown.starts_with("demo"), "the tier's own word stays first: {shown}");
    }
}

/// **`--tier paper` on a venue whose own word for our `demo` says "paper" is WARNED, not refused**
/// (decision 0156 item 8): our `paper` is the local simulator, and the venue's paper account is
/// `--tier demo`. Derived from the table, so every such venue is covered, not a hand list.
#[test]
fn paper_on_a_venue_that_calls_its_demo_paper_is_warned() {
    use super::account_lifecycle::paper_tier_warning;
    use vike_model::AccountTier::{Demo, Live, Paper};
    let ibkr = paper_tier_warning("ibkr", Paper).expect("ibkr calls its demo a paper account");
    assert!(ibkr.contains("LOCAL simulator") && ibkr.contains("--tier demo"), "{ibkr}");
    assert!(ibkr.contains("\"paper account\""), "names the venue's own word: {ibkr}");
    for venue in vike_model::VENUES {
        let says_paper = vike_model::venue_demo_word(venue).is_some_and(|w| w.contains("paper"));
        assert_eq!(paper_tier_warning(venue, Paper).is_some(), says_paper, "{venue}");
        assert!(paper_tier_warning(venue, Demo).is_none(), "{venue}: only `paper` is warned");
        assert!(paper_tier_warning(venue, Live).is_none(), "{venue}: only `paper` is warned");
    }
    assert!(paper_tier_warning("binance", Paper).is_none(), "binance's demo is not 'paper'");
}
