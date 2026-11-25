use vike_tradehub_client::wire::CommandWire;

use super::*;
use crate::cmd::trade::describe::describe_command;
use crate::cmd::trade::help_of;

/// `trade strategy <args…>` read through the plane's one parser; a `Stop` is a refusal (or help).
fn parsed(args: &[&str]) -> Result<StrategyVerb, args::Stop> {
    let argv = std::iter::once("strategy").chain(args.iter().copied()).map(String::from);
    match crate::cmd::trade::parse(argv)?.sub {
        Some(crate::cmd::trade::Sub::Strategy { verb }) => Ok(verb),
        other => panic!("`trade strategy` did not read as the strategy group: {other:?}"),
    }
}

/// `trade strategy ls <args…>`, through the parser and `ls`'s own rule.
fn ls(args: &[&str]) -> Result<LsArgs, String> {
    let line: Vec<&str> = std::iter::once("ls").chain(args.iter().copied()).collect();
    match parsed(&line).map_err(|stop| stop.to_string())? {
        StrategyVerb::Ls(cmd) => cmd.into_args(None),
        other => unreachable!("`ls` read as {other:?}"),
    }
}

// The bare group, help, an unknown verb and `ls --help` are the grammar the three verb groups
// share, tested once over all three: `crates/vike-cli/src/cmd/trade/tests/reason_and_help.rs`'s
// `VERB_GROUPS`.

/// `mount … --reason why --params '{..}'` started the strategy on defaults nobody chose: `--reason`
/// takes the rest of the line. The swallowed flag is refused and named; a reason that only mentions
/// the word, and the same flag before `--reason`, are the mount they always were.
#[test]
fn a_mount_flag_after_reason_is_refused_not_swallowed() {
    let head = ["binance", "BTCUSDT", "1m", "--strategy", "grid.rs"];
    let line = |tail: &[&'static str]| -> Vec<&'static str> {
        head.iter().copied().chain(tail.iter().copied()).collect()
    };
    for (tail, flag) in [
        (&["--reason", "why", "--params", r#"{"gamma":1}"#][..], "--params"),
        (&["--reason", "why", "--id", "m1"][..], "--id"),
    ] {
        let e = parse_mount_args(&line(tail)).expect_err("a swallowed flag is refused");
        assert_eq!(e.exit, crate::exit::Exit::Usage, "{tail:?}");
        assert!(
            e.message.contains(&format!("Put `{flag}` BEFORE --reason")),
            "{tail:?}: {}",
            e.message
        );
    }
    let v =
        parse_mount_args(&line(&["--params", r#"{"gamma":1}"#, "--reason", "params unchanged"]))
            .expect("a reason that only mentions the word is a reason");
    let Verb::MountStrategy { params, .. } = v else { panic!("{v:?}") };
    assert_eq!(params, json!({ "gamma": 1 }));
}

#[test]
fn ls_requires_a_node() {
    let e = ls(&[]).unwrap_err();
    assert!(e.contains("--node"), "{e}");
}

#[test]
fn ls_parses_the_node_and_json_flags() {
    let a = ls(&["--node", "n:1", "--json"]).expect("a valid ls line parses");
    assert_eq!(a.node, "n:1");
    assert!(a.json);
}

#[test]
fn an_unknown_flag_is_rejected_by_name() {
    let e = parsed(&["ls", "--node", "n:1", "--bogus"]).unwrap_err();
    assert!(e.contains("--bogus"), "{e}");
}

// ---- the LIFECYCLE verbs' own parsers (task 8) ----------------------------------------------

/// **`--strategy` is the one source the node mounts**: a mount naming none is refused naming
/// `--strategy`, and the retired `--name` and `--rhai` are no flags any more, refused as any unknown
/// argument.
#[test]
fn mount_takes_a_strategy_file_and_no_retired_source() {
    let v =
        parse_mount_args(&["binance", "BTCUSDT", "1m", "--strategy", "trend/momentum.rs"]).unwrap();
    let Verb::MountStrategy { file, .. } = v else { panic!("{v:?}") };
    assert_eq!(file.as_deref(), Some("trend/momentum.rs"));

    let e = parse_mount_args(&["binance", "BTCUSDT", "1m"]).expect_err("no source");
    assert_eq!(e.exit, crate::exit::Exit::Usage);
    assert!(e.message.contains("--strategy <path>"), "names the flag to pass: {}", e.message);

    for retired in [["--name", "grid"], ["--rhai", "y.rhai"]] {
        let mut line = vec!["binance", "BTCUSDT", "1m", "--strategy", "a.rhai"];
        line.extend(retired);
        let e = parse_mount_args(&line).expect_err("a retired flag is no flag");
        assert_eq!(e.exit, crate::exit::Exit::Usage, "{line:?}");
        assert!(e.message.contains(retired[0]), "{line:?}: {}", e.message);
    }
}

/// The renamed `--file` is refused naming `--strategy`, alone or beside it: never read as the strategy.
#[test]
fn the_renamed_file_flag_is_refused_naming_the_strategy_flag() {
    for line in [
        &["binance", "BTCUSDT", "1m", "--file", "grid.rs"][..],
        &["binance", "BTCUSDT", "1m", "--strategy", "grid.rs", "--file=grid.rs"][..],
    ] {
        let e = parse_mount_args(line).expect_err("the old name is refused");
        assert_eq!(e.exit, crate::exit::Exit::Usage, "{line:?}");
        assert_eq!(e.message, renamed_file_refusal(), "{line:?}");
    }
    assert_eq!(
        renamed_file_refusal(),
        "error [call.argument_renamed]: `--file` is now `--strategy`: send it under its new name"
    );
}

#[test]
fn unmount_help_states_that_positions_are_not_flattened() {
    assert!(
        UNMOUNT_PROSE.contains("POSITIONS ARE NOT FLATTENED"),
        "an operator who reads unmount as `get me out` and is wrong is left holding an \
             unattended position"
    );
    let help = help_of(&["strategy", "unmount"]);
    assert!(help.contains("POSITIONS ARE NOT FLATTENED"), "--help says it too: {help}");
}

#[test]
fn mount_parses_the_positionals_and_defaults_params_to_an_empty_object() {
    let v = parse_mount_args(&["binance", "BTCUSDT", "1m", "--strategy", "grid.rs"]).unwrap();
    match v {
        Verb::MountStrategy { venue, symbol, interval, file, params, account, controller_id } => {
            assert_eq!(venue, "binance");
            assert_eq!(symbol, "BTCUSDT");
            assert_eq!(interval, "1m");
            assert_eq!(file.as_deref(), Some("grid.rs"));
            assert_eq!(params, json!({}));
            assert_eq!(account, None);
            assert_eq!(controller_id, None);
        }
        other => panic!("expected a MountStrategy, got {other:?}"),
    }
}

#[test]
fn mount_threads_the_account_and_id_onto_the_verb() {
    let v = parse_mount_args(&[
        "binance/ALT",
        "BTCUSDT",
        "1m",
        "--strategy",
        "x.rhai",
        "--id",
        "my-mount",
    ])
    .unwrap();
    match v {
        Verb::MountStrategy { account, controller_id, file, .. } => {
            assert_eq!(account.as_deref(), Some("ALT"));
            assert_eq!(controller_id.as_deref(), Some("my-mount"));
            assert_eq!(file.as_deref(), Some("x.rhai"));
        }
        other => panic!("expected a MountStrategy, got {other:?}"),
    }
}

/// ⚠ **The regression this task exists to guard against, stated as its own test.** The critical
/// fix that made `order submit`/`mass-cancel` and `position flatten`/`close-all` REFUSE a
/// labelled book (`crate::cmd::trade::selector::refuse_an_unroutable_account`) must NEVER reach
/// `mount`: this verb never went through that module's `venue/LABEL` refusal at all — its venue
/// was a bare positional and its account a separate flag — and `crates/vike-tradehub/src/server/control.rs`'s
/// `lower_command` genuinely reads `CommandWire::MountStrategy`'s `account` field into a real
/// `MountSpec`, unlike the order-carrying commands' `OrderRequest`, which has none. So a
/// labelled account here is not merely untouched by the new refusal — there is no `Book` in this
/// function's data flow for that refusal to ever be called on. This test pins that a labelled
/// account still PASSES and still THREADS onto the verb after the fix landed, duplicating
/// `mount_threads_the_account_and_id_onto_the_verb`'s assertion deliberately: a future edit that
/// broke this by making `mount` share the order/position grammar should redden a test whose name
/// says exactly why that would be wrong.
///
/// ⚠ **The refusal this test was written against is DELETED**, and two sentences above are
/// history rather than the tree: stage 5 of
/// `docs/superpowers/specs/2026-09-22-the-order-payload-names-its-account-design.md` removed
/// `refuse_an_unroutable_account` on 2026-09-26, and `vike_model::OrderRequest` DOES carry an
/// account now — it was "has none" when this was written. The order/position verbs thread a
/// label too, so the two planes no longer differ in whether a label passes. What this test still
/// guards is the part that outlived its reason for being written: a labelled account threads onto
/// the verb. Since stage 5 the account is the `<venue>/<LABEL>` positional every verb reads
/// (`crate::cmd::trade::selector::addressed`), and `--account` is refused naming it. The name is kept
/// so its history reads true.
#[test]
fn mount_with_a_labelled_account_still_passes_after_the_order_write_refusal_landed() {
    let v = parse_mount_args(&["binance/ALT", "BTCUSDT", "1m", "--strategy", "grid.rs"])
        .expect("mount never refuses a labelled account");
    match v {
        Verb::MountStrategy { account, .. } => assert_eq!(
            account.as_deref(),
            Some("ALT"),
            "the label must still thread onto the wire's MountStrategy field"
        ),
        other => panic!("expected a MountStrategy, got {other:?}"),
    }
}

/// The wire's three account states mean `<venue>/DEFAULT` names the account POSITIVELY
/// (`"DEFAULT"`) and a bare venue names none — the same rule `crate::cmd::trade::order`'s
/// `parse_submit_args` pins for a bare book, though an account row's label refuses `DEFAULT` for
/// itself (this module's doc explains the asymmetry).
#[test]
fn mount_account_default_is_the_positive_wire_spelling() {
    let v =
        parse_mount_args(&["binance/DEFAULT", "BTCUSDT", "1m", "--strategy", "grid.rs"]).unwrap();
    match v {
        Verb::MountStrategy { venue, account, .. } => {
            assert_eq!((venue.as_str(), account.as_deref()), ("binance", Some("DEFAULT")))
        }
        other => panic!("expected a MountStrategy, got {other:?}"),
    }
}

#[test]
fn mount_rejects_an_invalid_account_naming_the_forms() {
    let e = parse_mount_args(&["binance/b a d", "BTCUSDT", "1m", "--strategy", "grid.rs"])
        .expect_err("an invalid account label is refused");
    assert!(e.message.contains(vike_model::accounts::account_keys::BOOK_FORMS), "{}", e.message);
}

/// The pre-stage-5 `--account <LABEL>` is refused by its id, naming the positional that replaced it,
/// before any other rule: it is never read as the account.
#[test]
fn the_retired_account_flag_is_refused_naming_the_positional() {
    let e = parse_mount_args(&[
        "binance",
        "BTCUSDT",
        "1m",
        "--strategy",
        "grid.rs",
        "--account",
        "ALT",
    ])
    .expect_err("--account is retired");
    assert_eq!(e.exit, crate::exit::Exit::Usage);
    assert_eq!(
        e.message,
        crate::cmd::trade::plane::renamed(
            "trade strategy mount <venue> … --account <LABEL>",
            "trade strategy mount <venue>/<LABEL> …"
        )
    );
    assert!(e.message.starts_with("error [cli.trade.renamed]: "), "{}", e.message);
}

/// An operator who forgets to quote `--params {"gamma": 0.0008}` hands the SHELL, not this
/// binary, the job of splitting on whitespace — the JSON is already SEPARATE argv elements
/// before `main` ever runs, and there is no way for this parser to tell that apart from a
/// order client id or symbol that also happens not to start with `--`. Silently reassembling them was the
/// defect two previous rounds of this file had to remove; this test drives the REAL argv read
/// (the only entry point that can reproduce the shape at all) and pins the TEACHING refusal
/// instead: every piece named, and the corrected, quoted command line printed.
#[test]
fn mount_params_unquoted_and_split_by_the_shell_is_refused_with_the_fix() {
    let args =
        ["binance", "BTCUSDT", "1m", "--strategy", "grid.rs", "--params", "{\"gamma\":", "0.0008}"];
    let e = parse_mount_args(&args).expect_err("an unquoted, shell-split JSON value is refused");
    assert!(e.message.contains("ONE shell argument"), "{}", e.message);
    assert!(
        e.message.contains("--params '{\"gamma\": 0.0008}'"),
        "names the corrected, quoted command line: {}",
        e.message
    );
}

/// **The defect a previous round of this fix introduced, and this pins the cure for**: a
/// PROPERLY QUOTED `--params` argument (`--params '{"note":"a  b"}'` at a real shell prompt)
/// reaches this process as ONE `String` with its internal whitespace intact — the OS never
/// touches it. `--params` is an ORDINARY single-element flag, taken verbatim and never
/// reassembled from pieces, so that whitespace survives with no special handling at all. This
/// test drives the REAL argv read end to end (not a hand-built value), because that is the entry
/// point an operator's argv actually reaches.
#[test]
fn mount_params_with_embedded_whitespace_survives_the_argv_read_intact() {
    let args =
        ["binance", "BTCUSDT", "1m", "--strategy", "grid.rs", "--params", "{\"note\":\"a  b\"}"];
    let v = parse_mount_args(&args).unwrap();
    match v {
        Verb::MountStrategy { params, .. } => assert_eq!(
            params.get("note").and_then(Value::as_str),
            Some("a  b"),
            "the two spaces inside the quoted JSON string must survive byte-for-byte: {params}"
        ),
        other => panic!("expected a MountStrategy, got {other:?}"),
    }
}

/// `--node`/`--yes`/`--json` must survive `--params` regardless of which side of it they land
/// on — the exact regression a previous round of this fix introduced by capturing the raw line
/// BEFORE those wrapper flags were stripped, which let `--params` (reading to end of line)
/// swallow whatever wrapper flag followed it. `--params` is a one-element flag now, so nothing
/// after it is at risk; this test pins that `--node` specifically, placed AFTER `--params` (the
/// order `mount`'s help itself documents), still reaches `WrapperArgs::node`.
#[test]
fn a_wrapper_flag_after_params_is_not_swallowed() {
    let args = [
        "binance",
        "BTCUSDT",
        "1m",
        "--strategy",
        "grid.rs",
        "--params",
        "{\"gamma\":0.0008}",
        "--node",
        "host:1234",
        "--yes",
        "--json",
    ];
    let StrategyVerb::Mount(c) = read("mount", &args).expect("a mount line reads") else {
        panic!("expected a mount");
    };
    assert_eq!(c.wrap.node.as_deref(), Some("host:1234"));
    assert!(c.wrap.yes);
    assert!(c.wrap.json);
    let v = mount_verb(&c).unwrap();
    match v {
        Verb::MountStrategy { params, .. } => assert_eq!(params, json!({ "gamma": 0.0008 })),
        other => panic!("expected a MountStrategy, got {other:?}"),
    }
}

#[test]
fn mount_params_must_be_a_json_object() {
    let e =
        parse_mount_args(&["binance", "BTCUSDT", "1m", "--strategy", "grid.rs", "--params", "[1]"])
            .expect_err("an array is not a params object");
    assert!(e.message.contains("JSON OBJECT"), "{}", e.message);
}

#[test]
fn mount_params_with_no_value_at_all_is_a_usage_error() {
    let e = parse_mount_args(&["binance", "BTCUSDT", "1m", "--strategy", "grid.rs", "--params"])
        .expect_err("--params with nothing after it needs a value");
    assert!(e.message.contains("--params"), "the refusal names the flag: {}", e.message);
}

#[test]
fn mount_params_explicitly_empty_is_a_typo_not_an_empty_table() {
    let e =
        parse_mount_args(&["binance", "BTCUSDT", "1m", "--strategy", "grid.rs", "--params", ""])
            .expect_err("an explicit empty --params value is a typo, not an empty table");
    // The blank-value rule is `vike_utility::cli::Program`'s, ahead of any verb rule: it names the flag.
    assert!(e.message.contains("--params"), "the refusal names the flag: {}", e.message);
}

#[test]
fn mount_wrong_positional_count_is_a_usage_error() {
    assert!(parse_mount_args(&["binance", "BTCUSDT"]).is_err());
    assert!(
        parse_mount_args(&["binance", "BTCUSDT", "1m", "extra", "--strategy", "x.rs"]).is_err()
    );
}

#[test]
fn unmount_parses_a_bare_mount_id() {
    let v = parse_unmount_args(&["binance__BTCUSDT__1m"]).unwrap();
    assert_eq!(v, Verb::UnmountStrategy { controller_id: "binance__BTCUSDT__1m".to_string() });
}

#[test]
fn unmount_rejects_zero_or_extra_tokens() {
    assert!(parse_unmount_args(&[]).is_err());
    assert!(parse_unmount_args(&["a", "b"]).is_err());
}

/// The mount and the shared write flags are read; the mount is required.
#[test]
fn reset_budget_reads_the_mount_and_the_shared_write_flags() {
    let verb = parsed(&["reset-budget", "binance__BTCUSDT__1m", "--node", "127.0.0.1:1", "--yes"]);
    let Ok(StrategyVerb::ResetBudget(c)) = verb else { panic!("read as {verb:?}") };
    assert_eq!((c.mount.as_str(), c.wrap.yes), ("binance__BTCUSDT__1m", true));
    assert!(
        parsed(&["reset-budget"]).is_err(),
        "the mount is required: a bare verb must never be read as a request"
    );
}

/// It lowers to the mount-scoped un-latch (the id trimmed) and the preview says what it does and what it leaves.
#[test]
fn reset_budget_lowers_to_the_unlatch_wire_command_and_previews_the_scope() {
    let verb = reset_budget_verb(" maker-a ").expect("ok");
    assert!(verb.is_write());
    let wire = verb.to_wire_command().expect("lowers");
    assert_eq!(wire, CommandWire::UnlatchMount { mount_id: "maker-a".into() });
    let preview = describe_command(&wire);
    assert!(preview.starts_with("STRATEGY-RESET-BUDGET maker-a"), "{preview}");
    assert!(preview.contains("ONE mount"), "{preview}");
    assert!(preview.contains("position") && preview.contains("PnL"), "{preview}");
}

/// An empty id is a usage error, before a socket opens.
#[test]
fn reset_budget_refuses_an_empty_mount_id() {
    for blank in ["", "   "] {
        let err = reset_budget_verb(blank).expect_err("nothing to reset");
        assert_eq!(err.exit, crate::exit::Exit::Usage, "{blank:?}");
    }
}

/// `--help` states the scope, the refusals, the new base, that it writes nothing at the venue, the capability it
/// needs and that the REPL has no word for it.
#[test]
fn reset_budget_help_states_the_scope_the_refusals_and_the_new_base() {
    let help = help_of(&["strategy", "reset-budget"]);
    // Single words only: `clap` wraps the prose, and a phrase can straddle a line break.
    assert!(help.contains("LATCH") && help.contains("flat"), "{help}");
    assert!(help.contains("CURRENT") && help.contains("base"), "{help}");
    assert!(help.contains("NOTHING") && help.contains("venue"), "{help}");
    assert!(help.contains("advertises"), "the capability it needs is named: {help}");
    assert!(
        crate::cmd::trade::parse_line("reset-budget maker-a").is_err(),
        "no REPL word: it is a one-shot verb, like `trade account remove-symbol`"
    );
}
