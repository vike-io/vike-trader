use vike_tradehub_client::wire::CommandWire;

use super::*;
use crate::cmd::trade::describe::describe_command;
use crate::cmd::trade::{Sub, help_of, parse, parse_line};

/// `trade account <argv>` through the plane's one parser.
fn parsed(argv: &[&str]) -> AccountVerb {
    let argv = std::iter::once("account").chain(argv.iter().copied()).map(String::from);
    match parse(argv).expect("parses").sub {
        Some(Sub::Account { verb }) => verb,
        other => panic!("did not read as `trade account`: {other:?}"),
    }
}

/// A line the plane's one parser refuses.
fn refused(argv: &[&str]) -> bool {
    parse(std::iter::once("account").chain(argv.iter().copied()).map(String::from)).is_err()
}

/// The book and the shared write flags are read, by all three state words; the book is required, and
/// the group needs a verb.
#[test]
fn each_state_word_reads_the_book_and_the_shared_write_flags() {
    for word in ["halt", "reduce", "resume"] {
        let verb = parsed(&[word, "binance/ALT", "--node", "127.0.0.1:1", "--yes"]);
        let c = match verb {
            AccountVerb::Halt(c) | AccountVerb::Reduce(c) | AccountVerb::Resume(c) => c,
            other => panic!("{word} read as {other:?}"),
        };
        assert_eq!((c.book.as_str(), c.wrap.yes), ("binance/ALT", true), "{word}");
        assert!(refused(&[word]), "{word}: a word alone must never be read as a request");
    }
    assert!(refused(&[]), "the group needs a verb");
}

/// The word picks the verb, the verb the state: `reduce` is not `halt`, and `resume` is not a no-op.
#[test]
fn each_word_is_its_own_verb() {
    assert!(matches!(parsed(&["halt", "binance"]), AccountVerb::Halt(_)));
    assert!(matches!(parsed(&["reduce", "binance"]), AccountVerb::Reduce(_)));
    assert!(matches!(parsed(&["resume", "binance"]), AccountVerb::Resume(_)));
}

/// Each state resolves to the ACCOUNT-scoped verb, never to the node-wide `SetState`, and the default
/// account, when named, travels as the reserved `DEFAULT` the node reads back.
#[test]
fn it_lowers_to_the_account_scoped_wire_command_and_never_the_node_wide_one() {
    for want in [TradingState::Halted, TradingState::Reducing, TradingState::Active] {
        let verb = state_verb("binance/ALT", want).expect("a book");
        assert!(verb.is_write());
        assert_eq!(
            verb.to_wire_command(),
            Some(CommandWire::SetAccountTradingState {
                venue: "binance".into(),
                account: Some("ALT".into()),
                state: want,
            })
        );
    }
    let default = state_verb("binance/DEFAULT", TradingState::Active).expect("the default");
    assert_eq!(
        default.to_wire_command(),
        Some(CommandWire::SetAccountTradingState {
            venue: "binance".into(),
            account: Some("DEFAULT".into()),
            state: TradingState::Active,
        })
    );
}

/// **A bare venue names NO account on the wire** (decision 0254), so the node's door takes it only where the venue
/// has one account: it used to go out as `DEFAULT`, which halted one account and read as halting the venue.
#[test]
fn a_bare_venue_sends_no_account_and_a_named_book_sends_its_own() {
    let frame = |verb: Verb| {
        serde_json::to_value(verb.to_wire_command().expect("lowers")).expect("a frame is JSON")
    };
    let halt = frame(state_verb("hyperliquid", TradingState::Halted).expect("a bare venue"));
    assert_eq!(halt["SetAccountTradingState"].get("account"), None, "{halt}");
    let give_back = frame(remove_symbol_verb("hyperliquid", "BTC").expect("a bare venue"));
    assert_eq!(give_back["UnadmitSymbol"].get("account"), None, "{give_back}");

    let named = frame(state_verb("hyperliquid/DEFAULT", TradingState::Halted).expect("a book"));
    assert_eq!(named["SetAccountTradingState"]["account"], "DEFAULT", "{named}");
    let named = frame(remove_symbol_verb("hyperliquid/LIVE", "BTC").expect("a book"));
    assert_eq!(named["UnadmitSymbol"]["account"], "LIVE", "{named}");
}

/// A route key and a malformed label are usage errors naming the fault, before a socket opens; the
/// preview says it is ONE account and that the node's own floor is not lowered.
#[test]
fn it_refuses_a_route_key_and_previews_the_scope() {
    for book in ["binance#ALT", "", "@mark", "a/b/c"] {
        let err = state_verb(book, TradingState::Halted).expect_err(book);
        assert_eq!(err.exit, crate::exit::Exit::Usage, "{book:?}");
    }
    let wire = state_verb("binance/ALT", TradingState::Active)
        .expect("ok")
        .to_wire_command()
        .expect("lowers");
    let preview = describe_command(&wire);
    assert!(preview.starts_with("ACCOUNT-RESUME binance/ALT"), "the verb's own word: {preview}");
    assert!(preview.contains("ONE account"), "{preview}");
    assert!(preview.contains("never lowered"), "the floor is stated: {preview}");
    for (state, word) in
        [(TradingState::Reducing, "ACCOUNT-REDUCE"), (TradingState::Halted, "ACCOUNT-HALT")]
    {
        let wire = state_verb("binance/ALT", state).expect("ok").to_wire_command().expect("lowers");
        assert!(describe_command(&wire).starts_with(word), "{word}");
    }
}

/// `--help` says it resumes ONE engine, the precedence, and that the REPL has no word for it.
#[test]
fn its_help_states_the_scope_and_the_precedence() {
    for word in ["halt", "reduce", "resume"] {
        let help = help_of(&["account", word]);
        // Single words only: `clap` wraps the prose, and a phrase can straddle a line break.
        assert!(help.contains("RESUMES") && help.contains("ONE"), "{help}");
        assert!(help.contains("STRICTER") && help.contains("latch"), "{help}");
        assert!(help.contains("venue"), "the link-held way out is named: {help}");
        assert!(help.contains("journal"), "the one-way journal door is stated: {help}");
    }
    assert!(
        parse_line("account resume binance/ALT").is_err(),
        "no REPL word: it is a one-shot verb, like `resume-venue`"
    );
}

/// The book, the symbol and the shared write flags are read; both positionals are required.
#[test]
fn remove_symbol_reads_the_book_the_symbol_and_the_shared_write_flags() {
    let verb =
        parsed(&["remove-symbol", "binance/ALT", "ETHUSDT.P", "--node", "127.0.0.1:1", "--yes"]);
    let AccountVerb::RemoveSymbol(c) = verb else { panic!("read as {verb:?}") };
    assert_eq!(
        (c.book.as_str(), c.symbol.as_str(), c.wrap.yes),
        ("binance/ALT", "ETHUSDT.P", true)
    );
    assert!(refused(&["remove-symbol"]));
    assert!(
        refused(&["remove-symbol", "binance/ALT"]),
        "the symbol is required: a book alone must never be read as a request"
    );
}

/// It lowers to the account-scoped give-back, the default account travels as the reserved `DEFAULT`, and
/// the preview says it is ONE account and that the engine decides.
#[test]
fn remove_symbol_lowers_to_the_unadmit_wire_command_and_previews_the_scope() {
    let verb = remove_symbol_verb("binance/ALT", " ETHUSDT.P ").expect("ok");
    assert!(verb.is_write());
    let wire = verb.to_wire_command().expect("lowers");
    assert_eq!(
        wire,
        CommandWire::UnadmitSymbol {
            venue: "binance".into(),
            account: Some("ALT".into()),
            symbol: "ETHUSDT.P".into(),
        }
    );
    let default = remove_symbol_verb("binance/DEFAULT", "SOLUSDT").expect("the default");
    assert_eq!(
        default.to_wire_command(),
        Some(CommandWire::UnadmitSymbol {
            venue: "binance".into(),
            account: Some("DEFAULT".into()),
            symbol: "SOLUSDT".into(),
        })
    );
    let preview = describe_command(&wire);
    assert!(preview.starts_with("ACCOUNT-REMOVE-SYMBOL binance/ALT ETHUSDT.P"), "{preview}");
    assert!(preview.contains("ONE account") && preview.contains("position"), "{preview}");
}

/// A route key, a malformed label and an empty symbol are usage errors, before a socket opens.
#[test]
fn remove_symbol_refuses_a_route_key_a_bad_label_and_an_empty_symbol() {
    for book in ["binance#ALT", "", "@mark", "a/b/c"] {
        let err = remove_symbol_verb(book, "ETHUSDT").expect_err(book);
        assert_eq!(err.exit, crate::exit::Exit::Usage, "{book:?}");
    }
    let err = remove_symbol_verb("binance/ALT", "  ").expect_err("nothing to give back");
    assert_eq!(err.exit, crate::exit::Exit::Usage);
}

/// `--help` states the scope, that the engine decides, that it writes nothing at the venue, and that the REPL
/// has no word for it.
#[test]
fn remove_symbol_help_states_the_scope_and_what_it_does_not_do() {
    let help = help_of(&["account", "remove-symbol"]);
    // Single words only: `clap` wraps the prose, and a phrase can straddle a line break.
    assert!(help.contains("QUIET") && help.contains("slot"), "{help}");
    assert!(help.contains("NOTHING") && help.contains("venue"), "{help}");
    assert!(help.contains("advertises"), "the capability it needs is named: {help}");
    assert!(
        parse_line("account remove-symbol binance/ALT ETHUSDT").is_err(),
        "no REPL word: it is a one-shot verb, like `resume-venue`"
    );
}
