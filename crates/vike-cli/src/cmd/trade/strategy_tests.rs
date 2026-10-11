use super::*;

fn parsed(args: &[&str]) -> Result<StrategyVerb, String> {
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
fn ls_parses_the_node_and_json_flags() {
    let v = parsed(&["ls", "--node", "n:1", "--json"]).expect("a valid ls line parses");
    let StrategyVerb::Ls(a) = v;
    assert_eq!(a.node, "n:1");
    assert!(a.json);
}

#[test]
fn ls_help_short_circuits() {
    let e = parsed(&["ls", "--help"]).unwrap_err();
    assert_eq!(e, crate::cmd::args::HELP_SENTINEL);
}

#[test]
fn an_unknown_flag_is_rejected_by_name() {
    let e = parsed(&["ls", "--node", "n:1", "--bogus"]).unwrap_err();
    assert!(e.contains("--bogus"), "{e}");
}

// ---- the LIFECYCLE verbs' own parsers (task 8) ----------------------------------------------

#[test]
fn mount_requires_name_xor_rhai_and_names_both_spellings() {
    let e = parse_mount_args(&["binance", "BTCUSDT", "1m"]).expect_err("one source is required");
    assert!(e.msg.contains("--name"), "name both spellings: {}", e.msg);
    assert!(e.msg.contains("--rhai"), "name both spellings: {}", e.msg);

    let e = parse_mount_args(&["binance", "BTCUSDT", "1m", "--name", "x", "--rhai", "y.rhai"])
        .expect_err("both is a parse error, not something the node discovers");
    assert!(e.msg.contains("--name") && e.msg.contains("--rhai"));
}

#[test]
fn unmount_help_states_that_positions_are_not_flattened() {
    assert!(
        UNMOUNT_USAGE.contains("POSITIONS ARE NOT FLATTENED"),
        "an operator who reads unmount as `get me out` and is wrong is left holding an \
             unattended position"
    );
}

#[test]
fn mount_parses_the_positionals_and_defaults_params_to_an_empty_object() {
    let v = parse_mount_args(&["binance", "BTCUSDT", "1m", "--name", "grid"]).unwrap();
    match v {
        Verb::MountStrategy {
            venue,
            symbol,
            interval,
            name,
            rhai,
            params,
            account,
            controller_id,
        } => {
            assert_eq!(venue, "binance");
            assert_eq!(symbol, "BTCUSDT");
            assert_eq!(interval, "1m");
            assert_eq!(name.as_deref(), Some("grid"));
            assert_eq!(rhai, None);
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
        "binance",
        "BTCUSDT",
        "1m",
        "--rhai",
        "strategies/x.rhai",
        "--account",
        "ALT",
        "--id",
        "my-mount",
    ])
    .unwrap();
    match v {
        Verb::MountStrategy { account, controller_id, rhai, .. } => {
            assert_eq!(account.as_deref(), Some("ALT"));
            assert_eq!(controller_id.as_deref(), Some("my-mount"));
            assert_eq!(rhai.as_deref(), Some("strategies/x.rhai"));
        }
        other => panic!("expected a MountStrategy, got {other:?}"),
    }
}

/// ⚠ **The regression this task exists to guard against, stated as its own test.** The critical
/// fix that made `order submit`/`mass-cancel` and `position flatten`/`close-all` REFUSE a
/// labelled book (`crate::cmd::trade::selector::refuse_an_unroutable_account`) must NEVER reach
/// `mount`: this verb never goes through that module's `venue/LABEL` grammar at all — its venue
/// is a bare positional and `--account` is a separate flag validated straight off
/// `vike_model::accounts::account_keys::parse_wire_account` — and `crates/vike-tradehub/src/server/control.rs`'s
/// `lower_command` genuinely reads `WireCommand::MountStrategy`'s `account` field into a real
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
/// guards is the part that outlived its reason for being written: `mount` takes its account from
/// a bare `--account` flag, never from the book grammar, and a labelled one threads onto the
/// verb. The name is kept so its history reads true.
#[test]
fn mount_with_a_labelled_account_still_passes_after_the_order_write_refusal_landed() {
    let v = parse_mount_args(&["binance", "BTCUSDT", "1m", "--name", "grid", "--account", "ALT"])
        .expect("mount never refuses a labelled account — it has no Book to refuse");
    match v {
        Verb::MountStrategy { account, .. } => assert_eq!(
            account.as_deref(),
            Some("ALT"),
            "the label must still thread onto the wire's MountStrategy field"
        ),
        other => panic!("expected a MountStrategy, got {other:?}"),
    }
}

/// The wire's three account states mean the DEFAULT spelling names the account POSITIVELY
/// (`"DEFAULT"`) — same rule `crate::cmd::trade::order`'s `parse_submit_args` pins for a bare
/// book, and the reason `--account DEFAULT` is admitted here even though an account row's label
/// refuses that spelling for itself (this module's doc explains the asymmetry).
#[test]
fn mount_account_default_is_the_positive_wire_spelling() {
    let v =
        parse_mount_args(&["binance", "BTCUSDT", "1m", "--name", "grid", "--account", "DEFAULT"])
            .unwrap();
    match v {
        Verb::MountStrategy { account, .. } => assert_eq!(account.as_deref(), Some("DEFAULT")),
        other => panic!("expected a MountStrategy, got {other:?}"),
    }
}

#[test]
fn mount_rejects_an_invalid_account_naming_default() {
    let e = parse_mount_args(&["binance", "BTCUSDT", "1m", "--name", "grid", "--account", "b a d"])
        .expect_err("an invalid account label is refused");
    assert!(e.msg.contains("DEFAULT"), "{}", e.msg);
}

/// An operator who forgets to quote `--params {"gamma": 0.0008}` hands the SHELL, not this
/// binary, the job of splitting on whitespace — the JSON is already SEPARATE argv elements
/// before `main` ever runs, and there is no way for this parser to tell that apart from a
/// coid or symbol that also happens not to start with `--`. Silently reassembling them was the
/// defect two previous rounds of this file had to remove; this test drives the REAL
/// `take_wrapper_flags` (the only entry point that can reproduce the shape at all) and pins the
/// TEACHING refusal instead: every piece named, and the corrected, quoted command line printed.
#[test]
fn mount_params_unquoted_and_split_by_the_shell_is_refused_with_the_fix() {
    let args = ["binance", "BTCUSDT", "1m", "--name", "grid", "--params", "{\"gamma\":", "0.0008}"];
    let (rest, _flags) = oneshot::take_wrapper_flags(args.iter().map(|s| s.to_string())).unwrap();
    let refs: Vec<&str> = rest.iter().map(String::as_str).collect();
    let e = parse_mount_args(&refs).expect_err("an unquoted, shell-split JSON value is refused");
    assert!(e.msg.contains("ONE shell argument"), "{}", e.msg);
    assert!(
        e.msg.contains("--params '{\"gamma\": 0.0008}'"),
        "names the corrected, quoted command line: {}",
        e.msg
    );
}

/// **The defect a previous round of this fix introduced, and this pins the cure for**: a
/// PROPERLY QUOTED `--params` argument (`--params '{"note":"a  b"}'` at a real shell prompt)
/// reaches this process as ONE `String` with its internal whitespace intact — the OS never
/// touches it. `--params` is now an ORDINARY single-element flag, taken verbatim and never
/// reassembled from pieces, so that whitespace survives with no special handling at all. This
/// test still drives the REAL `take_wrapper_flags` end to end (not a hand-built token array),
/// because that is the entry point an operator's argv actually reaches.
#[test]
fn mount_params_with_embedded_whitespace_survives_take_wrapper_flags_intact() {
    let args = ["binance", "BTCUSDT", "1m", "--name", "grid", "--params", "{\"note\":\"a  b\"}"];
    let (rest, _flags) = oneshot::take_wrapper_flags(args.iter().map(|s| s.to_string())).unwrap();
    let refs: Vec<&str> = rest.iter().map(String::as_str).collect();
    let v = parse_mount_args(&refs).unwrap();
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
/// order `MOUNT_USAGE` itself documents), still reaches `WrapperFlags::node`.
#[test]
fn a_wrapper_flag_after_params_is_not_swallowed() {
    let args = [
        "binance",
        "BTCUSDT",
        "1m",
        "--name",
        "grid",
        "--params",
        "{\"gamma\":0.0008}",
        "--node",
        "host:1234",
        "--yes",
        "--json",
    ];
    let (rest, flags) = oneshot::take_wrapper_flags(args.iter().map(|s| s.to_string())).unwrap();
    assert_eq!(flags.node.as_deref(), Some("host:1234"));
    assert!(flags.yes);
    assert!(flags.json);
    let refs: Vec<&str> = rest.iter().map(String::as_str).collect();
    let v = parse_mount_args(&refs).unwrap();
    match v {
        Verb::MountStrategy { params, .. } => assert_eq!(params, json!({ "gamma": 0.0008 })),
        other => panic!("expected a MountStrategy, got {other:?}"),
    }
}

#[test]
fn mount_params_must_be_a_json_object() {
    let e = parse_mount_args(&["binance", "BTCUSDT", "1m", "--name", "grid", "--params", "[1]"])
        .expect_err("an array is not a params object");
    assert!(e.msg.contains("JSON OBJECT"), "{}", e.msg);
}

#[test]
fn mount_params_with_no_value_at_all_is_a_usage_error() {
    let e = parse_mount_args(&["binance", "BTCUSDT", "1m", "--name", "grid", "--params"])
        .expect_err("--params with nothing after it needs a value");
    assert!(e.msg.contains("--params") && e.msg.contains("needs a value"), "{}", e.msg);
}

#[test]
fn mount_params_explicitly_empty_is_a_typo_not_an_empty_table() {
    let e = parse_mount_args(&["binance", "BTCUSDT", "1m", "--name", "grid", "--params", ""])
        .expect_err("an explicit empty --params value is a typo, not an empty table");
    assert!(e.msg.contains("empty params table"), "{}", e.msg);
}

#[test]
fn mount_wrong_positional_count_is_a_usage_error() {
    assert!(parse_mount_args(&["binance", "BTCUSDT"]).is_err());
    assert!(parse_mount_args(&["binance", "BTCUSDT", "1m", "extra", "--name", "x"]).is_err());
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
