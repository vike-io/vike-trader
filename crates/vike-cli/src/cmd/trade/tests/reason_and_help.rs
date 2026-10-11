//! The `--reason` tail, the REPL's own flags, and what the usage text names.

use super::*;

// ---- the `--reason` tail (node proto v4) --------------------------------------------------

#[test]
fn split_reason_takes_the_rest_of_the_line_verbatim() {
    let (cmd, why) = split_reason("flatten binance BTCUSDT --reason CPI print in 2 minutes");
    assert_eq!(cmd, "flatten binance BTCUSDT");
    assert_eq!(why.as_deref(), Some("CPI print in 2 minutes"));
    // …and the command half still parses exactly as it would have without the tail.
    assert_eq!(
        parse_line(cmd).unwrap(),
        Verb::Flatten { venue: "binance".into(), symbol: "BTCUSDT".into(), account: None }
    );
}

#[test]
fn split_reason_absent_returns_the_whole_line_untouched() {
    let line = "submit binance BTCUSDT buy 0.5 @59000 --reduce-only";
    assert_eq!(split_reason(line), (line, None));
}

#[test]
fn split_reason_strips_one_layer_of_quotes_and_treats_blank_as_none() {
    assert_eq!(split_reason("cancel c-1 --reason \"why not\"").1.as_deref(), Some("why not"));
    assert_eq!(split_reason("cancel c-1 --reason 'why not'").1.as_deref(), Some("why not"));
    // Only ONE layer, and only when it matches on both ends.
    assert_eq!(split_reason("cancel c-1 --reason \"a' ").1.as_deref(), Some("\"a'"));
    // Nothing (or only whitespace) after the flag is NO rationale.
    assert_eq!(split_reason("cancel c-1 --reason").1, None);
    assert_eq!(split_reason("cancel c-1 --reason    ").1, None);
    assert_eq!(split_reason("cancel c-1 --reason").0, "cancel c-1");
}

#[test]
fn split_reason_only_matches_a_whole_token() {
    // A coid/symbol that merely CONTAINS the text is not a split point.
    for line in ["cancel my--reason-x", "cancel --reasonable", "cancel x--reason"] {
        assert_eq!(split_reason(line), (line, None), "{line:?} must not split");
    }
    // …but a real flag later in the same line still does.
    let (cmd, why) = split_reason("cancel --reasonable --reason it was a typo");
    assert_eq!(cmd, "cancel --reasonable");
    assert_eq!(why.as_deref(), Some("it was a typo"));
}

#[test]
fn split_reason_leaves_the_verb_grammar_alone() {
    // A rationale containing flag-shaped words never reaches `parse_line` (the whole point of
    // splitting first): `modify` still sees only its own flags.
    let (cmd, why) = split_reason("modify c-1 --qty 2 --reason was --price too aggressive");
    assert_eq!(
        parse_line(cmd).unwrap(),
        Verb::Modify { client_order_id: "c-1".into(), new_qty: Some(2.0), new_price: None }
    );
    assert_eq!(why.as_deref(), Some("was --price too aggressive"));
}

#[test]
fn config_parsing() {
    let c = parse_config(["--node", "the CI box:7979", "--yes"].into_iter().map(String::from)).unwrap();
    assert_eq!(c.node.as_deref(), Some("the CI box:7979"));
    assert!(c.yes);
    assert!(!c.help);
    // `--node=host:port` inline form.
    let c = parse_config(["--node=127.0.0.1:9".to_string()].into_iter()).unwrap();
    assert_eq!(c.node.as_deref(), Some("127.0.0.1:9"));
    // help short-circuits.
    let c = parse_config(["--help".to_string()].into_iter()).unwrap();
    assert!(c.help);
    // unknown arg errors.
    assert!(parse_config(["--bogus".to_string()].into_iter()).is_err());
    // dangling value errors.
    assert!(parse_config(["--node".to_string()].into_iter()).is_err());
    // a bare boolean rejects an inline value (the shared args glue; `--yes=1` used to be
    // silently accepted as true).
    assert!(parse_config(["--yes=1".to_string()].into_iter()).is_err());
}

// -- USAGE names the plane: every group, the group-less trio, and the REPL's own note --------

/// The roster is DERIVED from [`plane::GROUPS`], never re-typed, so a group added there cannot
/// silently go unnamed in the text `--help` prints.
#[test]
fn the_usage_names_every_group_in_the_roster() {
    for (name, _) in plane::GROUPS {
        assert!(USAGE.contains(name), "the usage must name the group '{name}'");
    }
}

/// The trailing note is kept VERBATIM across the plane design — the REPL still has no
/// `--json`, and this string is the one place that decision is stated in words rather than
/// merely implied by an absent flag.
#[test]
fn the_usage_still_states_that_the_repl_has_no_json() {
    assert!(
        USAGE.contains("no --json on the REPL"),
        "the absence is a DECISION a reader can see, not an oversight somebody will fix"
    );
}

/// `status`/`halt`/`resume` are the one exemption from the group layer, and the usage text
/// must still name all three even though none of them appears in [`plane::GROUPS`].
#[test]
fn the_usage_names_the_three_words_that_take_no_group() {
    for word in ["status", "halt", "resume"] {
        assert!(USAGE.contains(word));
    }
}
