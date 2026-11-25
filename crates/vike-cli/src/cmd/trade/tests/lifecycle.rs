//! The strategy-lifecycle grammar (mount / unmount / set-setting) and its previews.

use super::*;
use std::assert_matches;

// ---- the strategy-lifecycle grammar (mount / unmount / set-setting) ----------------------

/// The one entry the REPL loop calls, resolved through the SHARED construction site to the
/// wire command it produces — which is what the assertions below are about, and what
/// [`run_write`] is handed.
fn lifecycle(line: &str) -> CommandWire {
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
        "mount sim BTCUSDT 1m --strategy spread_maker.rs",
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

/// **The strategy FILE mount**, the one source the node mounts: the path under the NODE's
/// `strategies/` folder rides `file`, no explicit id (the node derives one) and no params (an empty
/// table), and the two retired wire fields stay empty — in both flag spellings.
#[test]
fn mount_by_strategy_file_carries_the_path_and_no_retired_source() {
    let want = |controller_id: Option<&str>| CommandWire::MountStrategy {
        venue: "binance".into(),
        account: None,
        symbol: "BTCUSDT".into(),
        interval: "1m".into(),
        controller_id: controller_id.map(str::to_string),
        file: Some("trend/momentum.rs".into()),
        name: None,
        rhai: None,
        params: json!({}),
    };
    assert_eq!(lifecycle("mount binance BTCUSDT 1m --strategy trend/momentum.rs"), want(None));
    assert_eq!(
        lifecycle("mount binance BTCUSDT 1m --strategy=trend/momentum.rs --id=m-a"),
        want(Some("m-a"))
    );
}

/// **`--strategy` is the one strategy source, said at parse time**: a mount naming none is refused
/// naming `--strategy`, the one to pass, and the retired `--name` and `--rhai` are no flags any more,
/// refused as any unexpected flag, in both spellings. The renamed `--file` is refused naming
/// `--strategy`, in both spellings too.
#[test]
fn mount_names_the_file_when_none_is_given_and_refuses_a_retired_source() {
    let err = parse_repl_line("mount sim BTCUSDT 1m").expect_err("no source");
    assert!(err.to_string().contains("--strategy <path>"), "names the flag to pass: {err}");
    assert!(parse_repl_line("mount a b c --strategy s.rhai --strategy t.rhai").is_err(), "twice");
    for line in [
        "mount sim BTCUSDT 1m --strategy a.rhai --name spread_maker",
        "mount sim BTCUSDT 1m --strategy a.rhai --rhai /tmp/s.rhai",
        "mount sim BTCUSDT 1m --name=spread_maker",
    ] {
        let err = parse_repl_line(line).expect_err(line);
        assert!(err.to_string().contains("unexpected flag"), "{line:?}: {err}");
    }
    for line in ["mount sim BTCUSDT 1m --file a.rhai", "mount sim BTCUSDT 1m --file=a.rhai"] {
        let err = parse_repl_line(line).expect_err(line);
        assert_eq!(
            err,
            crate::cmd::trade::repl::grammar::LineError::Refused(
                crate::cmd::trade::strategy::renamed_file_refusal()
            ),
            "{line:?}"
        );
    }
}

/// Every optional field supplied — and `--params` carrying the SPACES a person actually types,
/// which is the whole reason it runs to end of line rather than being one whitespace-delimited
/// token.
#[test]
fn mount_by_file_takes_an_id_and_a_spacey_params_object() {
    assert_eq!(
        lifecycle(
            "mount sim ETHUSDT 5m --strategy grid.rhai --id grid-a \
                 --params {\"gamma\": 0.0008, \"levels\": 4}"
        ),
        CommandWire::MountStrategy {
            venue: "sim".into(),
            account: None,
            symbol: "ETHUSDT".into(),
            interval: "5m".into(),
            controller_id: Some("grid-a".into()),
            file: Some("grid.rhai".into()),
            name: None,
            rhai: None,
            params: json!({"gamma": 0.0008, "levels": 4}),
        }
    );
}

/// ⚠ **The `<account>` token has THREE states and the REPL must keep them apart**, because at two
/// engines of one venue they are three different answers: a bare venue names no account (the node
/// REFUSES it there), `<venue>/DEFAULT` names the venue's unlabelled account deliberately, and
/// `<venue>/<LABEL>` names that account. The tests above already pin the bare state on every mount
/// they parse; this one pins the other two.
#[test]
fn mount_carries_the_account_of_its_account_token() {
    let CommandWire::MountStrategy { venue, account, .. } =
        lifecycle("mount binance/ALT BTCUSDT 1m --strategy spread_maker.rs")
    else {
        panic!("a mount")
    };
    assert_eq!((venue.as_str(), account.as_deref()), ("binance", Some("ALT")));

    // ⚠ `DEFAULT` must survive as the STRING, not be normalised away into `None`: absence and
    // `DEFAULT` are different rows on the wire, and collapsing them here would silently turn a
    // deliberate choice into the refusable one.
    let CommandWire::MountStrategy { account, .. } =
        lifecycle("mount binance/DEFAULT BTCUSDT 1m --strategy=spread_maker.rs")
    else {
        panic!("a mount")
    };
    assert_eq!(account.as_deref(), Some("DEFAULT"));
}

/// An illegal label is refused HERE, naming the offending token and the forms — not a round trip
/// away as a `Response::Error`. The grammar is `parse_book_token`'s, so this edge adds a message
/// rather than a second set of rules.
#[test]
fn mount_refuses_an_illegal_account_label_at_parse_time() {
    let err = parse_repl_line("mount binance/alt BTCUSDT 1m --strategy spread_maker.rs")
        .expect_err("a lowercase label is not a legal account");
    assert!(err.to_string().contains("binance/alt"), "names the token it refused: {err}");
    assert!(err.to_string().contains("DEFAULT"), "…and the spelling that names the default: {err}");
}

/// The pre-stage-5 `--account <LABEL>` is refused by its id, naming the `<venue>/<LABEL>` token: it
/// is never read as the account.
#[test]
fn mount_refuses_the_retired_account_flag_naming_the_token() {
    for line in [
        "mount binance BTCUSDT 1m --strategy spread_maker.rs --account ALT",
        "mount binance BTCUSDT 1m --strategy spread_maker.rs --account=DEFAULT",
    ] {
        let err = parse_repl_line(line).expect_err("--account is retired");
        assert_eq!(
            err,
            crate::cmd::trade::repl::grammar::LineError::Refused(
                crate::cmd::trade::plane::renamed(
                    "mount <venue> … --account <LABEL>",
                    "mount <venue>/<LABEL> …"
                )
            ),
            "{line}"
        );
    }
}

/// `--params` is the `[strategy.params]` TABLE, so a non-object (or unparseable) payload is a
/// clean refusal rather than something the node has to decode and reject. An EMPTY `--params`
/// is refused too and is not the same as omitting the flag: mounting a strategy on defaults
/// nobody chose is not what "I typed --params" meant.
#[test]
fn mount_params_must_be_a_json_object_and_an_empty_flag_is_not_an_empty_table() {
    let base = "mount sim BTCUSDT 1m --strategy spread_maker.rs";
    assert!(parse_repl_line(&format!("{base} --params [1,2]")).is_err(), "an array");
    assert!(parse_repl_line(&format!("{base} --params 7")).is_err(), "a scalar");
    assert!(parse_repl_line(&format!("{base} --params {{oops")).is_err(), "not JSON");
    let err = parse_repl_line(&format!("{base} --params")).expect_err("empty --params");
    assert!(err.to_string().contains("--params"), "{err}");
    // …while OMITTING it is the empty table, and always was.
    let CommandWire::MountStrategy { params, .. } = lifecycle(base) else { panic!("a mount") };
    assert_eq!(params, json!({}));
}

#[test]
fn mount_rejects_malformed_argument_shapes() {
    assert!(
        parse_repl_line("mount sim BTCUSDT --strategy spread_maker.rs").is_err(),
        "no interval"
    );
    assert!(parse_repl_line("mount a b c d --strategy s").is_err(), "a fourth positional");
    assert!(parse_repl_line("mount a b c --strategy").is_err(), "dangling value");
    // A flag eaten as another flag's value is the `submit --order-client-id has space` failure again.
    assert!(parse_repl_line("mount a b c --id --strategy s").is_err(), "a flag as a value");
    assert!(parse_repl_line("mount a b c --strategy s --strategy t").is_err(), "twice");
    assert!(parse_repl_line("mount a b c --strategy s --bogus 1").is_err(), "unknown flag");
}

/// `unmount` takes ONE token. A second is refused rather than ignored, and the message points
/// at where a mount id comes from — this client deliberately does not re-derive the node's
/// `{venue}__{symbol}__{interval}` rule.
#[test]
fn unmount_takes_one_mount_id() {
    assert_eq!(
        lifecycle("unmount grid-a"),
        CommandWire::UnmountStrategy { controller_id: "grid-a".into() }
    );
    assert_eq!(
        lifecycle("unmount binance__BTCUSDT__1m"),
        CommandWire::UnmountStrategy { controller_id: "binance__BTCUSDT__1m".into() }
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
        CommandWire::SetSetting {
            // Derived from the key's first segment: the released v0.1.35 daemon still requires the
            // field on decode, and the key is the only thing that may decide it.
            file: "policy".into(),
            key: "policy.max_notional_per_order".into(),
            value: "250".into(),
            confirm: None,
        }
    );
    // Spaces survive: a TOML array is a value a person types with spaces in it.
    let CommandWire::SetSetting { file, value, .. } =
        lifecycle("set config.venues [\"binance\", \"okx\"]")
    else {
        panic!("a settings write")
    };
    assert_eq!(file, "config");
    assert_eq!(value, "[\"binance\", \"okx\"]");
    // …and the quotes are NOT stripped, unlike `--reason`'s one forgiving layer.
    let CommandWire::SetSetting { value, .. } =
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
        assert!(err.to_string().contains("no file"), "{line}: say what was retired: {err}");
        assert!(
            err.to_string().contains("set-setting <full.dotted.key> <value"),
            "{line}: …and name the spelling that replaced it: {err}"
        );
    }
}

/// **Every REPL settings write goes out with `confirm: None`, for every section.** The node has
/// ignored the wire's field since `docs/decisions/0086` point 7 — no retype confirm, for any key —
/// and nothing between this grammar and the wire fills one in. Asserted on the WIRE command the
/// line becomes, because that is what reaches the node; `vike_client::verbs`'
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
        let CommandWire::SetSetting { confirm, .. } = lifecycle(line) else {
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
        "mount sim BTCUSDT 1m --strategy spread_maker.rs --params {\"gamma\": 0.0008} --reason \
             trialling a tighter quote",
    );
    assert_eq!(why.as_deref(), Some("trialling a tighter quote"));
    let CommandWire::MountStrategy { params, file, .. } = lifecycle(line) else {
        panic!("a mount")
    };
    assert_eq!(params, json!({"gamma": 0.0008}));
    assert_eq!(file.as_deref(), Some("spread_maker.rs"));
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
        "mount binance BTCUSDT 1m --strategy spread_maker.rs --id grid-a --params {\"gamma\": 0.5}",
    ));
    for needle in ["binance", "BTCUSDT", "1m", "grid-a", "spread_maker", "gamma"] {
        assert!(preview.contains(needle), "the preview must carry {needle}: {preview}");
    }
    // A mount with no explicit id shows the ABSENCE rather than inventing the node's derived
    // one, and the source it shows is the file's path.
    let preview = describe_command(&lifecycle("mount sim ETHUSDT 5m --strategy s.rhai"));
    assert!(preview.contains("id=-"), "no id was given: {preview}");
    assert!(preview.contains("source=s.rhai"), "the source is the file: {preview}");

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
    let from_an_older_client = CommandWire::SetSetting {
        file: "policy.toml".into(),
        key: "policy.max_notional_per_order".into(),
        value: "250".into(),
        confirm: Some("policy.max_notional_per_order".into()),
    };
    let preview = describe_command(&from_an_older_client);
    assert!(!preview.contains("confirm"), "no confirm marker of any spelling: {preview}");
}
