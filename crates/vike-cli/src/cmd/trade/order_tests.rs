use super::*;

fn parsed(args: &[&str]) -> Result<OrderVerb, String> {
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
    let v = parsed(&["ls", "binance", "--symbol", "BTCUSDT", "--node", "n:1", "--json"])
        .expect("a valid ls line parses");
    let OrderVerb::Ls(a) = v;
    assert_eq!(a.node, "n:1");
    assert_eq!(a.symbol.as_deref(), Some("BTCUSDT"));
    assert!(a.json);
    assert_eq!(a.book.expect("a book was given").venue, "binance");
}

#[test]
fn ls_with_no_positional_names_no_book() {
    let v = parsed(&["ls", "--node", "n:1"]).expect("a bare ls parses");
    let OrderVerb::Ls(a) = v;
    assert!(a.book.is_none());
}

#[test]
fn ls_help_short_circuits() {
    let e = parsed(&["ls", "--help"]).unwrap_err();
    assert_eq!(e, crate::cmd::args::HELP_SENTINEL);
}

// ---- the WRITE verbs' own parsers (task 7) -------------------------------------------------

#[test]
fn submit_parses_the_book_first_then_the_instrument() {
    // A BARE venue, deliberately — this test is about positional ORDER (book, then symbol,
    // side, qty, order type), not about the account label, and a labelled book now refuses
    // (see `submit_refuses_a_labelled_book_the_node_cannot_route`).
    let v = parse_submit_args(&["binance", "BTCUSDT", "buy", "0.01", "@market"]).unwrap();
    match v {
        Verb::Submit(o) => {
            assert_eq!(o.venue, "binance");
            assert_eq!(o.symbol, "BTCUSDT");
            assert_eq!(o.side, 1);
            assert_eq!(o.qty, 0.01);
            assert_eq!(o.order_type, "market");
        }
        other => panic!("expected a Submit, got {other:?}"),
    }
}

#[test]
fn a_price_token_makes_it_a_limit() {
    let v = parse_submit_args(&["binance", "BTCUSDT", "buy", "0.01", "@61000"]).unwrap();
    match v {
        Verb::Submit(o) => {
            assert_eq!(o.order_type, "limit");
            assert_eq!(o.price, Some(61_000.0));
        }
        other => panic!("expected a Submit, got {other:?}"),
    }
}

#[test]
fn the_coid_is_minted_before_the_preview_so_the_id_shown_is_the_id_sent() {
    let v = parse_submit_args(&["binance", "BTCUSDT", "buy", "0.01", "@market"]).unwrap();
    match v {
        Verb::Submit(o) => assert!(
            !o.client_order_id.is_empty(),
            "a node REFUSES a remote Submit with an empty client_order_id"
        ),
        other => panic!("expected a Submit, got {other:?}"),
    }
}

#[test]
fn a_missing_book_is_a_usage_error_naming_the_forms() {
    let e = parse_submit_args(&["BTCUSDT", "buy", "0.01"]).expect_err("the book is required");
    assert!(e.msg.contains(crate::cmd::trade::selector::FORMS));
}

/// ⚠ **This test has now flipped THREE times, and this flip follows the node rather than a
/// belief about it.** Task 7 replaced an earlier refusal test with one of THIS name, on the
/// strength of the wire field and the advertised capability — both true, and both one layer
/// short of the node, whose `lower_command` built a `vike_model::OrderRequest` with no
/// `account` field. The account-routing critical fix then flipped it back to
/// `submit_refuses_a_labelled_book_the_node_cannot_route`, pinning a local refusal
/// (`crate::cmd::trade::selector`'s `refuse_an_unroutable_account`) that named that missing field
/// as the cause.
///
/// Stage 5 of `docs/superpowers/specs/2026-09-22-the-order-payload-names-its-account-design.md`
/// DELETED that refusal, because the node half it waited on is on `main` and this branch now
/// carries it: `OrderRequest` has an `account`, `crates/vike-tradehub/src/server.rs`'s
/// `lower_command` reads it back, routing resolves it to the engine it names, and an account the
/// node does not hold is refused at the node's edge before the Ack. So the parser's job is to
/// PASS the label through unaltered — which is what this pins — and the refusal of an unheld
/// account is pinned over a real node, where it now lives, by
/// `crates/vike-cli/tests/trade_plane_cli.rs`'s
/// `a_labelled_submit_for_an_unheld_account_is_refused_by_the_node_not_the_cli`.
#[test]
fn a_labelled_book_threads_its_account_onto_the_wire() {
    let v = parse_submit_args(&["binance/ALT", "BTCUSDT", "buy", "0.01", "@market"])
        .expect("a labelled book is no longer refused locally");
    match v {
        Verb::Submit(o) => {
            assert_eq!(o.venue, "binance");
            assert_eq!(
                o.account.as_deref(),
                Some("ALT"),
                "the label must reach the wire exactly as named — the node routes on it"
            );
        }
        other => panic!("expected a Submit, got {other:?}"),
    }
}

/// The wire's three states ([`WireOrderRequest::account`]) mean this parser must never leave the
/// field `None`: a book was ALWAYS given (it is required), so even the bare, unlabelled spelling
/// names the default account POSITIVELY (`"DEFAULT"`) rather than naming nothing — the
/// distinction the field's own doc argues (absence cannot be trusted across a version boundary).
#[test]
fn a_bare_venue_names_the_default_account_positively() {
    let v = parse_submit_args(&["binance", "BTCUSDT", "buy", "0.01", "@market"]).unwrap();
    match v {
        Verb::Submit(o) => assert_eq!(o.account.as_deref(), Some("DEFAULT")),
        other => panic!("expected a Submit, got {other:?}"),
    }
}

#[test]
fn submit_reduce_only_and_pinned_coid_survive_the_book_first_grammar() {
    let v = parse_submit_args(&[
        "okx",
        "BTC-USDT",
        "sell",
        "2",
        "@100",
        "--reduce-only",
        "--coid",
        "MYID001",
    ])
    .unwrap();
    match v {
        Verb::Submit(o) => {
            assert!(o.reduce_only);
            assert_eq!(o.client_order_id, "MYID001");
            assert_eq!(o.side, -1);
        }
        other => panic!("expected a Submit, got {other:?}"),
    }
}

#[test]
fn cancel_takes_exactly_one_coid_and_no_book() {
    let v = parse_cancel_args(&["c-1"]).unwrap();
    assert_eq!(v, Verb::Cancel("c-1".to_string()));
    assert!(parse_cancel_args(&[]).is_err());
    assert!(parse_cancel_args(&["c-1", "extra"]).is_err());
}

#[test]
fn modify_requires_at_least_one_change() {
    let e = parse_modify_args(&["c-1"]).expect_err("nothing to change is an error");
    assert!(e.msg.contains("nothing to change"));
    let v = parse_modify_args(&["c-1", "--qty", "2"]).unwrap();
    assert_eq!(
        v,
        Verb::Modify { client_order_id: "c-1".to_string(), new_qty: Some(2.0), new_price: None }
    );
}

#[test]
fn mass_cancel_book_is_optional_and_names_the_default_account_when_bare() {
    let v = parse_mass_cancel_args(&[]).unwrap();
    assert_eq!(v, Verb::MassCancel { venue: None, symbol: None, account: None });

    let v = parse_mass_cancel_args(&["binance", "BTCUSDT"]).unwrap();
    assert_eq!(
        v,
        Verb::MassCancel {
            venue: Some("binance".to_string()),
            symbol: Some("BTCUSDT".to_string()),
            account: Some("DEFAULT".to_string())
        }
    );
}

/// A labelled book on `mass-cancel` is no longer refused HERE — this replaces
/// `mass_cancel_refuses_a_labelled_book_the_node_cannot_route`, which pinned the local refusal
/// stage 5 deleted (see this module's doc). The parser now passes the label through, and what
/// keeps it off a node that would drop it is the CLIENT's capability gate:
/// `crates/vike-tradehub-client/src/remote_control.rs`'s `required_feature` answers
/// `FEATURE_ACCOUNT_SCOPED_REDUCE` for a `MassCancel` naming an account, which no node
/// advertises until it can confine the cancel to that account. That refusal is pinned over a
/// real node by `crates/vike-cli/tests/trade_plane_cli.rs`'s
/// `a_labelled_risk_reducing_verb_is_refused_client_side_before_anything_is_sent`. Naming NO
/// book at all (the first case above) is untouched — no account, so no gate sees one.
#[test]
fn mass_cancel_threads_a_labelled_book_onto_the_wire() {
    let v = parse_mass_cancel_args(&["binance/ALT"])
        .expect("a labelled book is no longer refused locally");
    assert_eq!(
        v,
        Verb::MassCancel {
            venue: Some("binance".to_string()),
            symbol: None,
            account: Some("ALT".to_string())
        }
    );
}

/// `--help` must not promise what the client gate refuses. `mass-cancel`'s usage and its
/// [`VERBS`] row offered a book as an ordinary narrowing ("optionally scoped to one book"),
/// while ANY named account on a risk-reducing verb — the bare venue's positive `DEFAULT`
/// included — needs a node advertising `account-scoped-reduce` and is refused client-side,
/// before anything is sent, against one that does not. Pinned so the omission cannot grow back.
#[test]
fn mass_cancel_help_names_the_capability_a_book_needs() {
    let text = mass_cancel_usage();
    assert!(text.contains("account-scoped-reduce"), "{text}");
    assert!(text.contains("DEFAULT"), "a bare venue names its DEFAULT account\n{text}");
    let (_, row) = VERBS.iter().find(|(name, _)| *name == "mass-cancel").expect("a row");
    assert!(row.contains("account-scoped-reduce"), "the roster row names it too: {row}");
}
