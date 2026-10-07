use super::*;

fn parsed(args: &[&str]) -> Result<PositionVerb, String> {
    parse(args.iter().map(|s| s.to_string()))
}

#[test]
fn no_verb_names_the_roster() {
    let e = parsed(&[]).unwrap_err();
    assert!(e.contains("needs a verb"), "{e}");
    assert!(e.contains("ls"), "{e}");
}

#[test]
fn help_short_circuits_before_any_verb_specific_parsing() {
    for spelling in ["-h", "--help", "help"] {
        let e = parsed(&[spelling]).unwrap_err();
        assert_eq!(e, crate::cmd::args::HELP_SENTINEL);
    }
}

#[test]
fn an_unknown_verb_names_itself_and_the_roster() {
    let e = parsed(&["bogus"]).unwrap_err();
    assert!(e.contains("bogus"), "{e}");
    assert!(e.contains("ls"), "{e}");
}

#[test]
fn ls_requires_a_node() {
    let e = parsed(&["ls"]).unwrap_err();
    assert!(e.contains("--node"), "{e}");
}

#[test]
fn ls_parses_a_leading_book_selector_and_the_flags() {
    let v = parsed(&["ls", "binance/ALT", "--symbol", "BTCUSDT", "--node", "n:1", "--json"])
        .expect("a valid ls line parses");
    let PositionVerb::Ls(a) = v;
    assert_eq!(a.node, "n:1");
    assert_eq!(a.symbol.as_deref(), Some("BTCUSDT"));
    assert!(a.json);
    let book = a.book.expect("a book was given");
    assert_eq!(book.venue, "binance");
    assert_eq!(book.label.text(), Some("ALT"));
}

#[test]
fn ls_with_no_positional_names_no_book() {
    let v = parsed(&["ls", "--node", "n:1"]).expect("a bare ls parses");
    let PositionVerb::Ls(a) = v;
    assert!(a.book.is_none());
}

#[test]
fn ls_help_short_circuits() {
    let e = parsed(&["ls", "--help"]).unwrap_err();
    assert_eq!(e, crate::cmd::args::HELP_SENTINEL);
}

// ---- the WRITE verbs' own parsers (task 7) -------------------------------------------------

#[test]
fn flatten_requires_a_book_and_a_symbol() {
    let e = parse_flatten_args(&[]).expect_err("book and symbol are both required");
    assert!(e.msg.contains(crate::cmd::trade::selector::FORMS));
    assert!(parse_flatten_args(&["binance"]).is_err(), "a symbol is still required");
    assert!(
        parse_flatten_args(&["binance", "BTCUSDT", "extra"]).is_err(),
        "no more than book + symbol"
    );
}

/// ⚠ **THREADS again — this test has now flipped twice.** It first read "threads a labelled
/// book's account onto the wire", from a measurement that stopped one layer short of the node
/// (whose `lower_command` then dropped `WireCommand::Flatten`'s `account`). The account-routing
/// critical fix flipped it to `flatten_refuses_a_labelled_book_the_node_cannot_route`, pinning
/// the local refusal `crate::cmd::trade::selector`'s `refuse_an_unroutable_account` made. Stage 5
/// deleted that refusal (this module's doc records it), so the parser threads the label again —
/// and this time the thing that keeps it off a node that would drop it is named and real: the
/// CLIENT's capability gate (`crates/vike-tradehub-client/src/remote_control.rs`'s
/// `required_feature`, answering `FEATURE_ACCOUNT_SCOPED_REDUCE`), pinned over a real node by
/// `crates/vike-cli/tests/trade_plane_cli.rs`'s
/// `a_labelled_risk_reducing_verb_is_refused_client_side_before_anything_is_sent`.
#[test]
fn flatten_threads_a_labelled_book_onto_the_wire() {
    let v = parse_flatten_args(&["binance/ALT", "BTCUSDT"])
        .expect("a labelled book is no longer refused locally");
    assert_eq!(
        v,
        Verb::Flatten {
            venue: "binance".to_string(),
            symbol: "BTCUSDT".to_string(),
            account: Some("ALT".to_string())
        }
    );
}

#[test]
fn flatten_names_the_default_account_positively_for_a_bare_venue() {
    let v = parse_flatten_args(&["binance", "BTCUSDT"]).unwrap();
    assert_eq!(
        v,
        Verb::Flatten {
            venue: "binance".to_string(),
            symbol: "BTCUSDT".to_string(),
            account: Some("DEFAULT".to_string())
        }
    );
}

#[test]
fn close_all_book_is_optional() {
    assert_eq!(parse_close_all_args(&[]).unwrap(), Verb::MarketExit { venue: None, account: None });
    assert_eq!(
        parse_close_all_args(&["binance"]).unwrap(),
        Verb::MarketExit {
            venue: Some("binance".to_string()),
            account: Some("DEFAULT".to_string())
        }
    );
    assert!(parse_close_all_args(&["binance", "extra"]).is_err());
}

/// A labelled book on `close-all` threads onto the wire for the same reason `flatten`'s does —
/// see `flatten_threads_a_labelled_book_onto_the_wire`'s doc. This replaces
/// `close_all_refuses_a_labelled_book_the_node_cannot_route`, which pinned the local refusal
/// stage 5 deleted. Naming NO book at all (the first case above) is untouched: the widest,
/// risk-reducing spelling, carrying no account for any gate to see.
/// `--help` must not promise what the client gate refuses. `close-all`'s usage said a book "may
/// widen to every account of a venue", which is false twice over: a bare venue names its
/// DEFAULT account (`close_all_book_is_optional` above), never every account of it, and ANY
/// named account on a risk-reducing verb needs a node advertising `account-scoped-reduce` —
/// without it the verb is refused client-side before anything is sent. `flatten`'s usage said
/// only that its book is required, which left an operator to discover that the verb cannot be
/// sent at all to such a node, and `close-all`'s [`VERBS`] row offered a book as an ordinary
/// narrowing. All three are pinned so none of the claims grows back.
#[test]
fn the_write_usages_name_the_capability_a_book_needs_and_promise_no_widening() {
    for (verb, text) in [("flatten", flatten_usage()), ("close-all", close_all_usage())] {
        assert!(text.contains("account-scoped-reduce"), "{verb}: {text}");
        assert!(text.contains("DEFAULT"), "{verb}: a bare venue names its DEFAULT account\n{text}");
        assert!(!text.contains("widen to every account of a venue"), "{verb}: {text}");
    }
    let (_, row) = VERBS.iter().find(|(name, _)| *name == "close-all").expect("a row");
    assert!(row.contains("account-scoped-reduce"), "the roster row names it too: {row}");
}

#[test]
fn close_all_threads_a_labelled_book_onto_the_wire() {
    assert_eq!(
        parse_close_all_args(&["binance/ALT"])
            .expect("a labelled book is no longer refused locally"),
        Verb::MarketExit { venue: Some("binance".to_string()), account: Some("ALT".to_string()) }
    );
}
