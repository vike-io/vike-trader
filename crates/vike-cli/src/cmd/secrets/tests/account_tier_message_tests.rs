use super::account_lifecycle::{
    arming_consequence, max_exposure_missing_message, parse_max_exposure, tier_missing_message,
    tier_unknown_message,
};
use super::*;

/// **MAJOR 1 of the 2026-09-23 task-3/7 review.** The `sim` -> `paper` rename (ruling 7) put
/// `paper` INTO `vike_secrets::ACCOUNT_TIERS`, and both of `account add`'s tier refusals
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
    let roster = vike_secrets::ACCOUNT_TIERS.join(" | ");
    for action in ["add", "set-tier"] {
        for msg in [tier_missing_message(action), tier_unknown_message(action, "not-a-real-tier")] {
            assert!(msg.contains(&roster), "must print the real roster: {msg}");
            assert!(
                msg.contains(&format!("account {action}")),
                "the refusal must name the action it came from: {msg}"
            );
            for tier in vike_secrets::ACCOUNT_TIERS {
                assert!(
                    !msg.contains(&format!("`{tier}` is not"))
                        && !msg.contains(&format!("no `{tier}`")),
                    "{tier} is in ACCOUNT_TIERS but this refusal denies it: {msg}"
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
    fn parse_of(argv: &[&str]) -> Result<Args, String> {
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
    let live = arming_consequence("binance", "live", "7");
    assert!(live.contains("ARMS") && live.contains("REAL MONEY"), "{live}");
    assert!(live.contains("NEITHER"), "the TierConflict stop must be named: {live}");
    assert!(live.contains("account deactivate --id 7"), "{live}");
    let demo = arming_consequence("bybit", "demo", "N");
    assert!(demo.contains("ARMS") && !demo.contains("REAL MONEY"), "{demo}");
    let paper = arming_consequence("okx", "paper", "3");
    assert!(paper.contains("paper simulator") && !paper.contains("ARMS"), "{paper}");
    for msg in [live, demo, paper, tier_missing_message("add"), tier_unknown_message("add", "x")] {
        assert!(!msg.contains("policy.venues"), "the per-venue ceiling is gone: {msg}");
    }
}
