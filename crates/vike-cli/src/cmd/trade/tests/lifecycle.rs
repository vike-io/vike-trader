//! The strategy-lifecycle grammar (mount / unmount / set-setting) and its previews.

use super::*;
use std::assert_matches;

// ---- the strategy-lifecycle grammar (mount / unmount / set-setting) ----------------------

/// The one entry the REPL loop calls, resolved through the SHARED construction site to the
/// wire command it produces — which is what the assertions below are about, and what
/// [`run_write`] is handed.
fn lifecycle(line: &str) -> WireCommand {
    let verb = parse_repl_line(line)
        .unwrap_or_else(|e| panic!("expected a lifecycle verb for {line:?}, got error: {e}"));
    assert!(verb.is_write(), "{line:?} must parse to a WRITE verb, got {verb:?}");
    verb.to_wire_command()
        .unwrap_or_else(|| panic!("{line:?}: a write verb must build a wire command"))
}

/// The router keeps ONE grammar per line: the lifecycle verbs are claimed by
/// [`parse_lifecycle`] and land on their own [`Verb`] variants, everything else still falls
/// through to the order vocabulary untouched, and an unknown verb is still the ORDER grammar's
/// own error (the lifecycle half must not swallow it).
///
/// ⚠ This used to assert on a `Parsed::Lifecycle` / `Parsed::Order` union. With both grammars
/// producing one [`Verb`], the routing claim is made on the VARIANT instead — the same
/// property, asserted one level down, and still the thing that would break if the lifecycle
/// half started claiming (or stopped claiming) a token.
#[test]
fn parse_repl_line_routes_each_verb_to_exactly_one_grammar() {
    for line in [
        "mount sim BTCUSDT 1m --name spread_maker",
        "unmount grid-a",
        "set-setting flags.reconcile_off true",
        "set flags.reconcile_off true",
    ] {
        assert_matches!(
            parse_repl_line(line),
            Ok(Verb::MountStrategy { .. } | Verb::UnmountStrategy { .. } | Verb::SetSetting { .. }),
            "{line:?} must be claimed by the lifecycle grammar"
        );
    }
    for line in ["submit sim BTCUSDT buy 1", "orders", "halt", "quit"] {
        assert!(
            !matches!(
                parse_repl_line(line),
                Ok(Verb::MountStrategy { .. }
                    | Verb::UnmountStrategy { .. }
                    | Verb::SetSetting { .. })
            ),
            "{line:?} must fall through to the order vocabulary"
        );
        // …and PARSE there. Without this the negative above would also be satisfied by an
        // error, which is the one way "fell through" could be true and useless.
        assert!(parse_repl_line(line).is_ok(), "{line:?} must still parse");
    }
    // The order grammar still owns the unknown-verb error, and `quit` still round-trips as the
    // value the REPL loop breaks on.
    assert!(parse_repl_line("frobnicate").is_err());
    assert!(parse_repl_line("   ").is_err());
    assert_eq!(parse_repl_line("quit").unwrap(), Verb::Quit);
}

/// The minimal registry-name mount: no explicit id (the node derives one), no params (an empty
/// table).
#[test]
fn mount_by_registry_name_defaults_the_id_and_the_params() {
    assert_eq!(
        lifecycle("mount binance BTCUSDT 1m --name spread_maker"),
        WireCommand::MountStrategy {
            venue: "binance".into(),
            account: None,
            symbol: "BTCUSDT".into(),
            interval: "1m".into(),
            controller_id: None,
            name: Some("spread_maker".into()),
            rhai: None,
            params: json!({}),
        }
    );
}

/// The Rhai form, with every optional field supplied — and `--params` carrying the SPACES a
/// person actually types, which is the whole reason it runs to end of line rather than being
/// one whitespace-delimited token.
#[test]
fn mount_by_rhai_path_takes_an_id_and_a_spacey_params_object() {
    assert_eq!(
        lifecycle(
            "mount sim ETHUSDT 5m --rhai /srv/vike-<unit>/strategies/grid.rhai --id grid-a \
                 --params {\"gamma\": 0.0008, \"levels\": 4}"
        ),
        WireCommand::MountStrategy {
            venue: "sim".into(),
            account: None,
            symbol: "ETHUSDT".into(),
            interval: "5m".into(),
            controller_id: Some("grid-a".into()),
            name: None,
            rhai: Some("/srv/vike-<unit>/strategies/grid.rhai".into()),
            params: json!({"gamma": 0.0008, "levels": 4}),
        }
    );
    // The `--flag=value` spelling is the same pair `submit --coid` accepts.
    assert_eq!(
        lifecycle("mount sim ETHUSDT 5m --name=spread_maker --id=grid-b"),
        WireCommand::MountStrategy {
            venue: "sim".into(),
            account: None,
            symbol: "ETHUSDT".into(),
            interval: "5m".into(),
            controller_id: Some("grid-b".into()),
            name: Some("spread_maker".into()),
            rhai: None,
            params: json!({}),
        }
    );
}

/// ⚠ **`--account` has THREE states and the REPL must keep them apart**, because at two engines
/// of one venue they are three different answers: omitted names no account (the node REFUSES
/// it there), `DEFAULT` names the venue's unlabelled account deliberately, and a label names
/// that account. The tests above already pin the omitted state on every mount they parse; this
/// one pins the other two, in both flag spellings.
#[test]
fn mount_carries_the_account_in_both_flag_spellings() {
    let WireCommand::MountStrategy { account, .. } =
        lifecycle("mount binance BTCUSDT 1m --name spread_maker --account ALT")
    else {
        panic!("a mount")
    };
    assert_eq!(account.as_deref(), Some("ALT"));

    // ⚠ `DEFAULT` must survive as the STRING, not be normalised away into `None`: absence and
    // `DEFAULT` are different rows on the wire, and collapsing them here would silently turn a
    // deliberate choice into the refusable one.
    let WireCommand::MountStrategy { account, .. } =
        lifecycle("mount binance BTCUSDT 1m --name=spread_maker --account=DEFAULT")
    else {
        panic!("a mount")
    };
    assert_eq!(account.as_deref(), Some("DEFAULT"));
}

/// An illegal label is refused HERE, naming the flag and the offending value — not a round trip
/// away as a `Response::Error`. The grammar is `parse_wire_account`'s, so this edge adds a
/// message rather than a second set of rules.
#[test]
fn mount_refuses_an_illegal_account_label_at_parse_time() {
    let err = parse_repl_line("mount binance BTCUSDT 1m --name spread_maker --account alt")
        .expect_err("a lowercase label is not a legal account");
    assert!(err.contains("--account"), "names the flag: {err}");
    assert!(err.contains("alt"), "…and the value it refused: {err}");
    assert!(err.contains("DEFAULT"), "…and the spelling that names the default: {err}");
}

/// **The XOR, refused at PARSE time and naming BOTH spellings.** `WireCommand::MountStrategy`'s
/// contract is an exclusive choice; a client that let the node discover the violation spends a
/// round trip to say what the grammar already knew, and answers with the node's words rather
/// than with the two flags the operator has to choose between.
#[test]
fn mount_refuses_both_or_neither_strategy_source_and_names_the_two_spellings() {
    for line in
        ["mount sim BTCUSDT 1m --name spread_maker --rhai /tmp/s.rhai", "mount sim BTCUSDT 1m"]
    {
        let err = parse_repl_line(line).expect_err(line);
        assert!(err.contains("--name"), "{line:?} must name --name: {err}");
        assert!(err.contains("--rhai"), "{line:?} must name --rhai: {err}");
    }
}

/// `--params` is the `[strategy.params]` TABLE, so a non-object (or unparseable) payload is a
/// clean refusal rather than something the node has to decode and reject. An EMPTY `--params`
/// is refused too and is not the same as omitting the flag: mounting a strategy on defaults
/// nobody chose is not what "I typed --params" meant.
#[test]
fn mount_params_must_be_a_json_object_and_an_empty_flag_is_not_an_empty_table() {
    let base = "mount sim BTCUSDT 1m --name spread_maker";
    assert!(parse_repl_line(&format!("{base} --params [1,2]")).is_err(), "an array");
    assert!(parse_repl_line(&format!("{base} --params 7")).is_err(), "a scalar");
    assert!(parse_repl_line(&format!("{base} --params {{oops")).is_err(), "not JSON");
    let err = parse_repl_line(&format!("{base} --params")).expect_err("empty --params");
    assert!(err.contains("--params"), "{err}");
    // …while OMITTING it is the empty table, and always was.
    let WireCommand::MountStrategy { params, .. } = lifecycle(base) else { panic!("a mount") };
    assert_eq!(params, json!({}));
}

#[test]
fn mount_rejects_malformed_argument_shapes() {
    assert!(parse_repl_line("mount sim BTCUSDT --name spread_maker").is_err(), "no interval");
    assert!(parse_repl_line("mount a b c d --name s").is_err(), "a fourth positional");
    assert!(parse_repl_line("mount a b c --name").is_err(), "dangling value");
    // A flag eaten as another flag's value is the `submit --coid has space` failure again.
    assert!(parse_repl_line("mount a b c --id --name s").is_err(), "a flag as a value");
    assert!(parse_repl_line("mount a b c --name s --name t").is_err(), "twice");
    assert!(parse_repl_line("mount a b c --name s --bogus 1").is_err(), "unknown flag");
}

/// `unmount` takes ONE token. A second is refused rather than ignored, and the message points
/// at where a mount id comes from — this client deliberately does not re-derive the node's
/// `{venue}__{symbol}__{interval}` rule.
#[test]
fn unmount_takes_one_mount_id() {
    assert_eq!(
        lifecycle("unmount grid-a"),
        WireCommand::UnmountStrategy { controller_id: "grid-a".into() }
    );
    assert_eq!(
        lifecycle("unmount binance__BTCUSDT__1m"),
        WireCommand::UnmountStrategy { controller_id: "binance__BTCUSDT__1m".into() }
    );
    assert!(parse_repl_line("unmount").is_err());
    assert!(parse_repl_line("unmount binance BTCUSDT 1m").is_err());
}

/// **`set-setting <full.dotted.key> <value>` — the KEY names its section, and the wire's `file` is
/// derived from it.** A settings write is one row named by its key (`docs/decisions/0086`), the
/// same two positionals `vike-cli config set` takes. The value runs to END OF LINE and is taken
/// VERBATIM — quotes included, because they are what tells the node's TOML parse that `"250"` is a
/// string and not the integer `250`.
#[test]
fn set_setting_takes_the_key_and_the_value_to_end_of_line_without_unquoting_it() {
    assert_eq!(
        lifecycle("set-setting policy.max_notional_per_order 250"),
        WireCommand::SetSetting {
            // Derived from the key's first segment: the released v0.1.35 daemon still requires the
            // field on decode, and the key is the only thing that may decide it.
            file: "policy".into(),
            key: "policy.max_notional_per_order".into(),
            value: "250".into(),
            confirm: None,
        }
    );
    // Spaces survive: a TOML array is a value a person types with spaces in it.
    let WireCommand::SetSetting { file, value, .. } =
        lifecycle("set config.venues [\"binance\", \"okx\"]")
    else {
        panic!("a settings write")
    };
    assert_eq!(file, "config");
    assert_eq!(value, "[\"binance\", \"okx\"]");
    // …and the quotes are NOT stripped, unlike `--reason`'s one forgiving layer.
    let WireCommand::SetSetting { value, .. } =
        lifecycle("set-setting config.tradehub_addr \"127.0.0.1:7979\"")
    else {
        panic!("a settings write")
    };
    assert_eq!(value, "\"127.0.0.1:7979\"");

    assert!(parse_repl_line("set-setting policy.x").is_err(), "no value");
    assert!(parse_repl_line("set-setting").is_err());
}

/// **The retired `<file>` token is refused BY NAME, never silently re-read.** The grammar was
/// `set-setting <file> <key> <value>` until the key alone named the row; a line still spelled that
/// way would otherwise parse its file NAME as the key and the real key as the start of the value —
/// a write of `policy.toml = "policy.max_notional_per_order 250"` for the node to puzzle over. The
/// refusal names the spelling that replaced it, the same courtesy the removed `state` verb gets.
#[test]
fn the_retired_file_token_is_refused_by_name() {
    for line in [
        "set-setting policy.toml policy.max_notional_per_order 250",
        "set-setting policy policy.max_notional_per_order 250",
        "set config.toml config.tradehub_addr 127.0.0.1:7979",
    ] {
        let err = parse_repl_line(line).expect_err(line);
        assert!(err.contains("no file"), "{line}: say what was retired: {err}");
        assert!(
            err.contains("set-setting <full.dotted.key> <value"),
            "{line}: …and name the spelling that replaced it: {err}"
        );
    }
}

/// **Every REPL settings write goes out with `confirm: None`, for every section.** The node has
/// ignored the wire's field since `docs/decisions/0086` point 7 — no retype confirm, for any key —
/// and nothing between this grammar and the wire fills one in. Asserted on the WIRE command the
/// line becomes, because that is what reaches the node; `crate::cmd::verbs`'
/// `every_settings_write_is_built_with_no_confirm` holds the shared builder to the same rule for
/// the `mcp` surface.
///
/// ⚠ This replaced two tests that pinned the ceremony: that the parser could never PRE-FILL the
/// confirm (the retype prompt was what filled it), and which keys the prompt was asked for
/// (`vike_config::is_policy_plane_key`, deleted on 2026-09-29 with its last caller).
#[test]
fn a_repl_settings_write_carries_no_confirm() {
    for line in [
        "set-setting policy.max_notional_per_order 250",
        "set-setting flags.reconcile_off true",
        "set preferences.theme dark",
    ] {
        let WireCommand::SetSetting { confirm, .. } = lifecycle(line) else {
            panic!("a settings write: {line}")
        };
        assert_eq!(confirm, None, "nothing may ride in the wire's confirm field: {line}");
    }
}

/// `--reason` still runs to end of line, and it composes with `--params`, which does too: the
/// rationale is split off FIRST, so the mount parser never sees it and the JSON keeps its
/// spaces.
#[test]
fn a_reason_tail_composes_with_the_rest_of_line_params() {
    let (line, why) = split_reason(
        "mount sim BTCUSDT 1m --name spread_maker --params {\"gamma\": 0.0008} --reason \
             trialling a tighter quote",
    );
    assert_eq!(why.as_deref(), Some("trialling a tighter quote"));
    let WireCommand::MountStrategy { params, name, .. } = lifecycle(line) else {
        panic!("a mount")
    };
    assert_eq!(params, json!({"gamma": 0.0008}));
    assert_eq!(name.as_deref(), Some("spread_maker"));
}

/// The shared rest-of-line splitter distinguishes ABSENT from PRESENT-AND-EMPTY, which is the
/// one property its two callers answer differently: an empty `--reason` is no rationale, an
/// empty `--params` is a typo.
#[test]
fn split_tail_separates_an_absent_flag_from_an_empty_one() {
    assert_eq!(split_tail("mount a b c", PARAMS_FLAG), ("mount a b c", None));
    assert_eq!(split_tail("mount a b c --params", PARAMS_FLAG), ("mount a b c", Some("")));
    assert_eq!(
        split_tail("mount a b c --params   {\"x\": 1}", PARAMS_FLAG),
        ("mount a b c", Some("{\"x\": 1}"))
    );
    // Whole-token only — a symbol that merely CONTAINS the text is not a split point.
    assert_eq!(split_tail("mount a --paramsy c", PARAMS_FLAG), ("mount a --paramsy c", None));
}

/// The preview is the last thing between an operator and a running strategy, so it must show
/// the mount's IDENTITY, its SOURCE and its knobs — and, for a settings write, the assignment and
/// nothing about a confirm.
#[test]
fn the_lifecycle_previews_carry_what_the_operator_has_to_check() {
    let preview = describe_command(&lifecycle(
        "mount binance BTCUSDT 1m --name spread_maker --id grid-a --params {\"gamma\": 0.5}",
    ));
    for needle in ["binance", "BTCUSDT", "1m", "grid-a", "spread_maker", "gamma"] {
        assert!(preview.contains(needle), "the preview must carry {needle}: {preview}");
    }
    // A mount with no explicit id shows the ABSENCE rather than inventing the node's derived
    // one, and a Rhai mount shows the script path as its source.
    let preview = describe_command(&lifecycle("mount sim ETHUSDT 5m --rhai /srv/s.rhai"));
    assert!(preview.contains("id=-"), "no id was given: {preview}");
    assert!(preview.contains("/srv/s.rhai"), "the source is the script path: {preview}");

    let preview = describe_command(&lifecycle("unmount grid-a"));
    assert!(preview.contains("UNMOUNT-STRATEGY") && preview.contains("grid-a"), "{preview}");

    // A settings write previews the ASSIGNMENT. The `old → new` beside it needs the node's current
    // value, so it is printed by `run_write` from a node read, not rendered here.
    let preview = describe_command(&lifecycle("set-setting policy.max_notional_per_order 250"));
    assert!(
        preview.contains("policy.max_notional_per_order") && preview.contains("250"),
        "{preview}"
    );
    // ⚠ …and never a confirm marker, not even for a command an OLDER client minted with the wire's
    // `confirm` field filled: the node ignores that field (`docs/decisions/0086` point 7), so a
    // `[confirmed]` badge would advertise a guard that does not exist.
    let from_an_older_client = WireCommand::SetSetting {
        file: "policy.toml".into(),
        key: "policy.max_notional_per_order".into(),
        value: "250".into(),
        confirm: Some("policy.max_notional_per_order".into()),
    };
    let preview = describe_command(&from_an_older_client);
    assert!(!preview.contains("confirm"), "no confirm marker of any spelling: {preview}");
}
