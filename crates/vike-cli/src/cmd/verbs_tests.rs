/// COMPOSED, not spelled. `crates/vike-config/tests/policy_is_consumed.rs` used to read every
/// non-comment line under `src/` for a section-qualified policy key as evidence that the file READS
/// that setting, test modules included, so a fixture spelling one turned `main` red (#1685 lifted
/// these fixtures here from `mcp.rs` and `Policy::max_leverage` is `Consumed::No`). It now reads the
/// PRODUCTION view only (comments, `#[cfg(test)]` items and test files are skipped), so the
/// composition is no longer required; it is kept because it is harmless.
/// Composing it is the same trick `crates/vike-model/src/credential_keys.rs` uses to keep its
/// near-miss fixtures out of the settings-registry literal harvest.
const POLICY_KEY_FIXTURE: &str = concat!("policy.", "max_leverage");

use super::*;

// NOTE: no test here sets the VIKE_MAX_ORDER_* env vars (env writes race across the parallel
// test harness); the cap-less path is what these pin, same as the mcp/trade preview tests.

fn submit_cmd(qty: f64, price: Option<f64>) -> WireCommand {
    WireCommand::Submit(WireOrderRequest {
        client_order_id: String::new(),
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        side: 1,
        qty,
        order_type: if price.is_some() { "limit".into() } else { "market".into() },
        price,
        trigger_price: None,
        reduce_only: false,
        account: None,
    })
}

#[test]
fn modify_tool_args_map_to_the_repl_wire_command() {
    // The SAME WireCommand::Modify the REPL's `modify c-1 --qty 2` builds.
    let cmd =
        wire_command_for("modify", &json!({ "client_order_id": "c-1", "new_qty": 2.0 })).unwrap();
    assert_eq!(
        cmd,
        WireCommand::Modify { client_order_id: "c-1".into(), new_qty: Some(2.0), new_price: None }
    );
}

#[test]
fn modify_with_nothing_to_change_is_a_clean_error() {
    let err = wire_command_for("modify", &json!({ "client_order_id": "c-1" })).unwrap_err();
    assert!(err.contains("new_qty"), "{err}");
    assert!(wire_command_for("modify", &json!({})).unwrap_err().contains("client_order_id"));
}

#[test]
fn mass_cancel_tool_args_map_with_both_scopes_optional() {
    assert_eq!(
        wire_command_for("mass_cancel", &json!({})).unwrap(),
        WireCommand::MassCancel { venue: None, symbol: None, account: None }
    );
    assert_eq!(
        wire_command_for("mass_cancel", &json!({ "venue": "sim", "symbol": "BTCUSDT" })).unwrap(),
        WireCommand::MassCancel {
            venue: Some("sim".into()),
            symbol: Some("BTCUSDT".into()),
            account: None
        }
    );
}

/// The MCP write roster with one MINIMAL argument set each — the seven order verbs and the
/// three node-lifecycle ones. Spelled ONCE so the two roster loops below cannot come to cover
/// different sets; `mcp`'s own `WRITE_TOOLS` is the authority for the names, and
/// `the_write_arm_and_the_write_roster_are_the_same_set` there is what holds the routing to it.
fn write_tool_arguments() -> Vec<(&'static str, Value)> {
    vec![
        ("submit_order", json!({ "venue": "sim", "symbol": "B", "side": 1, "qty": 1.0 })),
        ("cancel_order", json!({ "client_order_id": "c-1" })),
        ("modify", json!({ "client_order_id": "c-1", "new_price": 9.0 })),
        ("flatten", json!({ "venue": "sim", "symbol": "B" })),
        ("market_exit", json!({})),
        ("set_trading_state", json!({ "state": "halted" })),
        ("mass_cancel", json!({})),
        (
            "mount_strategy",
            json!({ "venue": "sim", "symbol": "B", "interval": "1m", "name": "spread_maker" }),
        ),
        ("unmount_strategy", json!({ "controller_id": "sim__B__1m" })),
        // A key and a value, and nothing else (`docs/decisions/0086`): any section builds the same
        // way, and there is no retype gate left for a policy key to meet, on either surface.
        ("set_setting", json!({ "key": "config.tradehub_addr", "value": "1:2" })),
    ]
}

#[test]
fn every_mcp_write_tool_resolves_through_the_shared_verb() {
    // The whole MCP write roster maps through Verb::to_wire_command — the one construction
    // site. (submit/cancel/flatten/market_exit/set_trading_state pins live in mcp.rs's tests;
    // this pins that the mapping goes THROUGH a write Verb for each roster name.)
    for (name, args) in write_tool_arguments() {
        let verb = verb_from_tool_args(name, &args).unwrap();
        assert!(verb.is_write(), "{name} must map to a WRITE verb");
        assert!(verb.to_wire_command().is_some(), "{name} must build a wire command");
    }
    assert!(wire_command_for("node_snapshot", &json!({})).unwrap_err().contains("no wire"));
}

/// The rationale rides BESIDE the command, never inside it: adding a `reason` argument must
/// leave `wire_command_for`'s output BYTE-IDENTICAL for every write tool, and the reason itself
/// comes out of the separate [`reason_from_tool_args`] reader.
#[test]
fn a_reason_argument_never_changes_the_wire_command() {
    for (name, mut args) in write_tool_arguments() {
        let bare = wire_command_for(name, &args).unwrap();
        assert_eq!(reason_from_tool_args(&args), None, "{name}: no reason argument given");
        args["reason"] = json!("agent: flattening ahead of the CPI print");
        let with = wire_command_for(name, &args).unwrap();
        assert_eq!(with, bare, "{name}: a reason must not change the built command");
        assert_eq!(
            reason_from_tool_args(&args).as_deref(),
            Some("agent: flattening ahead of the CPI print"),
            "{name}: …and it is read off the SEPARATE reason reader"
        );
    }
}

#[test]
fn reason_from_tool_args_trims_and_treats_blank_as_absent() {
    assert_eq!(reason_from_tool_args(&json!({ "reason": "  why  " })).as_deref(), Some("why"));
    assert_eq!(reason_from_tool_args(&json!({ "reason": "   " })), None);
    assert_eq!(reason_from_tool_args(&json!({ "reason": "" })), None);
    assert_eq!(reason_from_tool_args(&json!({})), None);
    // A non-string `reason` is ignored rather than stringified into a nonsense rationale.
    assert_eq!(reason_from_tool_args(&json!({ "reason": 7 })), None);
}

// ---- the node-LIFECYCLE three (moved here with the arms they exercise) --------------------
//
// These pin the PURE builder and moved out of `mcp.rs` with it; the gates that sit in FRONT of
// it there (the mandatory preview, the venue check) keep their tests in that file, because they
// are the MCP surface's own and not part of the construction.

/// `mount_strategy` takes EXACTLY ONE source, and both wrong shapes are refused HERE rather
/// than at the daemon's edge — a round trip to learn a rule the profile vocabulary already
/// states. (The REPL refuses the same rule in ITS spelling, naming `--name`/`--rhai`; that
/// half is `crate::cmd::trade`'s, deliberately.)
#[test]
fn mount_strategy_takes_exactly_one_strategy_source() {
    let base = json!({ "venue": "sim", "symbol": "BTCUSDT", "interval": "1m" });
    let mut neither = base.clone();
    neither["params"] = json!({});
    let err = wire_command_for("mount_strategy", &neither).unwrap_err();
    assert!(err.contains("exactly one"), "{err}");
    assert!(err.contains("not script source"), "…and says what `rhai` is NOT: {err}");

    let mut both = base.clone();
    both["name"] = json!("spread_maker");
    both["rhai"] = json!("strategies/breaker.rhai");
    assert!(wire_command_for("mount_strategy", &both).unwrap_err().contains("both were given"));

    for (field, value) in [("name", "spread_maker"), ("rhai", "strategies/breaker.rhai")] {
        let mut one = base.clone();
        one[field] = json!(value);
        assert!(
            wire_command_for("mount_strategy", &one).is_ok(),
            "{field} alone must build a mount"
        );
    }
}

/// The mount's `params` table: absent means an EMPTY table, and a non-object is refused here
/// because the wire carries it opaquely and nothing below would object until the node did.
#[test]
fn mount_strategy_params_default_to_an_empty_table_and_must_be_an_object() {
    let base = json!({
        "venue": "sim", "symbol": "BTCUSDT", "interval": "1m", "name": "spread_maker"
    });
    let cmd = wire_command_for("mount_strategy", &base).unwrap();
    let WireCommand::MountStrategy { params, controller_id, .. } = &cmd else {
        panic!("the builder produced a different variant")
    };
    assert_eq!(*params, json!({}), "an absent params table is an empty one");
    assert_eq!(*controller_id, None, "…and an absent id derives on the node, not here");

    let mut list = base.clone();
    list["params"] = json!([1, 2]);
    assert!(wire_command_for("mount_strategy", &list).unwrap_err().contains("must be an object"));
}

/// `unmount_strategy` names the mount by id and nothing else — and the refusal says WHICH id,
/// because `strategy_status` does not report one.
#[test]
fn unmount_strategy_requires_the_mount_id() {
    let err = wire_command_for("unmount_strategy", &json!({})).unwrap_err();
    assert!(err.contains("controller_id"), "{err}");
    assert_eq!(
        wire_command_for("unmount_strategy", &json!({ "controller_id": "sim__B__1m" })).unwrap(),
        WireCommand::UnmountStrategy { controller_id: "sim__B__1m".to_string() }
    );
}

/// `set_setting`'s `value` is TEXT on the wire. A JSON number or boolean is RENDERED rather
/// than refused (the agent expressed the right intent and the node parses TOML anyway); a
/// structured value is refused, because `serde_json`'s rendering of one would not be TOML.
#[test]
fn a_settings_value_renders_a_scalar_and_refuses_a_structure() {
    let of = |v: Value| {
        wire_command_for("set_setting", &json!({ "key": "config.tradehub_addr", "value": v }))
    };
    for (given, expect) in [(json!(250), "250"), (json!(true), "true"), (json!("x"), "x")] {
        let cmd = of(given.clone()).unwrap();
        let WireCommand::SetSetting { value, .. } = &cmd else {
            panic!("the builder produced a different variant")
        };
        assert_eq!(value, expect, "{given} must render as {expect}");
    }
    assert!(of(json!({ "a": 1 })).unwrap_err().contains("must be text"));
    assert!(of(json!([1])).unwrap_err().contains("must be text"));
}

/// **A settings write's wire `file` is DERIVED from its key's first segment** — step 1 of taking the
/// file era out of the settings wire. A write is one row named by its KEY; the node reads neither
/// `file` nor `confirm`, and the released v0.1.35 daemon still requires `file` on decode, so every
/// client here keeps sending one — the key's own section word, never an input a caller could make
/// disagree with the key. `set_setting` takes no `file` argument, and one an agent still passes is
/// ignored like any argument a tool does not name.
#[test]
fn a_settings_write_derives_its_file_from_the_key() {
    for (args, section) in [
        (json!({ "key": POLICY_KEY_FIXTURE, "value": "3" }), "policy"),
        (json!({ "key": "config.tradehub_addr", "value": "1:2" }), "config"),
        (json!({ "key": "preferences.log_level", "value": "debug" }), "preferences"),
        // A stale `file` that disagrees with the key is not what goes out: the KEY decides.
        (json!({ "file": "config.toml", "key": POLICY_KEY_FIXTURE, "value": "3" }), "policy"),
    ] {
        let cmd = wire_command_for("set_setting", &args).expect("a key and a value are enough");
        let WireCommand::SetSetting { file, .. } = &cmd else {
            panic!("the builder produced a different variant")
        };
        assert_eq!(file, section, "{args}: the wire's file is the key's section word");
    }
}

/// **Every settings write this crate builds goes out with `confirm: None`.** The node has ignored
/// the wire's field since `docs/decisions/0086` point 7 (no retype confirm, for any key), and the
/// field stays on the wire only for an older client — so the ONE construction site both surfaces
/// resolve through fills it with nothing, whatever it is handed: not from `key`, and not from a
/// `policy_confirm` an agent still passes out of an old prompt, which is now an argument nothing
/// reads (every tool ignores what it does not name).
#[test]
fn every_settings_write_is_built_with_no_confirm() {
    for args in [
        json!({ "key": POLICY_KEY_FIXTURE, "value": "3" }),
        json!({ "key": "config.tradehub_addr", "value": "1:2" }),
        json!({ "key": POLICY_KEY_FIXTURE, "value": "3", "policy_confirm": POLICY_KEY_FIXTURE }),
    ] {
        let cmd = wire_command_for("set_setting", &args).expect("a settings write builds");
        let WireCommand::SetSetting { confirm, .. } = &cmd else {
            panic!("the builder produced a different variant")
        };
        assert_eq!(*confirm, None, "{args}: the wire's confirm field must go out empty");
    }
}

// ---- the `old → new` a settings write previews (`SettingChange`) ------------------------------

/// A node's `SettingsShow` answer carrying the given `(key, value, origin)` rows.
fn show(rows: &[(&str, &str, &str)]) -> WireSettingsShow {
    WireSettingsShow {
        settings_dir: Some("/srv/vike-<unit>/settings".into()),
        rows: rows
            .iter()
            .map(|(key, value, origin)| vike_tradehub_client::wire::WireSettingsRow {
                section: key.split('.').next().unwrap_or_default().to_string(),
                key: (*key).to_string(),
                value: (*value).to_string(),
                origin: (*origin).to_string(),
                read_by: "yes".into(),
            })
            .collect(),
    }
}

/// **The OLD value is the NODE's row for exactly this key**, and the line is `key: old → new` —
/// the pair 0086 point 7 names as what guards a live limit now that no key is retyped.
#[test]
fn a_setting_change_reads_the_old_value_off_the_nodes_row_for_that_key() {
    let node = show(&[("config.tradehub_addr", "0.0.0.0:7879", "db"), ("policy.x", "100.0", "db")]);
    let change = SettingChange::from_show("policy.x", "250", Ok(&node));
    assert_eq!(
        change.old,
        OldSetting::Read { value: "100.0".into(), origin: "db".into() },
        "the row for THIS key, not the first row"
    );
    assert_eq!(change.line(), "policy.x: 100.0 → 250");
    let json = change.to_json();
    assert_eq!(json["old"], json!("100.0"));
    assert_eq!(json["old_origin"], json!("db"));
    assert_eq!(json["old_unread"], Value::Null, "a value was read, so there is no reason to give");
    assert_eq!(json["new"], json!("250"));
    assert_eq!(json["line"], json!("policy.x: 100.0 → 250"));
    assert_eq!(change.env_shadow(), None, "a row-set value follows the row");
}

/// An UNSET key renders as `(unset)` rather than as an empty slot a reader would take for a typo,
/// and a key the node does not carry says so rather than showing an old value it does not have.
#[test]
fn an_unset_or_unknown_key_says_what_it_is_rather_than_showing_nothing() {
    let node = show(&[("policy.x", "", "default")]);
    assert_eq!(
        SettingChange::from_show("policy.x", "5", Ok(&node)).line(),
        "policy.x: (unset) → 5"
    );

    let unknown = SettingChange::from_show("policy.nope", "5", Ok(&node));
    assert_eq!(unknown.old, OldSetting::NoSuchKey);
    assert!(unknown.line().contains("no such key"), "{}", unknown.line());
    assert_eq!(unknown.to_json()["old"], Value::Null, "no row, so no old value — never `\"\"`");
    assert!(unknown.to_json()["old_unread"].is_string(), "…and the payload says why");
}

/// **A node that could not be asked yields NO old value, and every rendering says so.** A
/// fabricated one — an empty string, a default — is the one field an agent would relay to the
/// owner as fact.
#[test]
fn an_unread_old_value_is_null_and_names_why() {
    let change = SettingChange::from_show("policy.x", "250", Err("the node is down".into()));
    assert_eq!(change.old, OldSetting::Unread("the node is down".into()));
    assert_eq!(change.line(), "policy.x: (current value not read: the node is down) → 250");
    let json = change.to_json();
    assert_eq!(json["old"], Value::Null);
    assert_eq!(json["old_origin"], Value::Null);
    assert_eq!(json["old_unread"], json!("the node is down"));
}

/// **An environment variable outranks a settings row** (0086's first declared residual), so a key
/// whose current value one sets will NOT follow the row being written. The change names the
/// variable so the surface can say so; a row-set or default value names none.
#[test]
fn a_value_an_environment_variable_sets_is_named_as_shadowing_the_row() {
    let node = show(&[("config.tradehub_addr", "0.0.0.0:7879", "env:VIKE_TRADEHUB_ADDR")]);
    let change = SettingChange::from_show("config.tradehub_addr", "127.0.0.1:7879", Ok(&node));
    assert_eq!(change.env_shadow(), Some("env:VIKE_TRADEHUB_ADDR"));
    assert_eq!(change.to_json()["old_origin"], json!("env:VIKE_TRADEHUB_ADDR"));
    let default = show(&[("config.tradehub_addr", "127.0.0.1:7879", "default")]);
    let change = SettingChange::from_show("config.tradehub_addr", "x", Ok(&default));
    assert_eq!(change.env_shadow(), None);
}

// ---- the coid mint (the node REFUSES a remote submit with an empty one) -------------------

/// **THE defect.** A `Submit` built by either surface leaves `client_order_id` empty; the mint
/// must fill it with a NON-EMPTY, venue-valid id — otherwise the node answers "remote submit
/// requires a pre-minted client_order_id" and nothing is ever placed.
#[test]
fn an_empty_submit_coid_is_minted_and_is_venue_valid() {
    let mut minter = coid_minter();
    let filled = fill_client_order_id(submit_cmd(1.0, Some(100.0)), &mut minter);
    let WireCommand::Submit(o) = filled else { panic!("still a Submit") };
    assert!(!o.client_order_id.is_empty(), "an empty coid is exactly what the node refuses");
    assert!(
        vike_model::is_valid_crypto_coid(&o.client_order_id),
        "the minted id must survive the node, the core and the venue edge: {:?}",
        o.client_order_id
    );
    assert!(o.client_order_id.len() <= 12, "typeable: {:?}", o.client_order_id);
}

/// Successive submits in one session get DIFFERENT ids. The node's registry is coid-keyed and
/// idempotent, so a repeated id would silently book nothing the second time — an order surface
/// cannot have that.
#[test]
fn every_minted_coid_in_a_session_is_distinct() {
    let mut minter = coid_minter();
    let mut seen = std::collections::BTreeSet::new();
    for _ in 0..500 {
        let WireCommand::Submit(o) =
            fill_client_order_id(submit_cmd(1.0, Some(100.0)), &mut minter)
        else {
            panic!("still a Submit")
        };
        assert!(seen.insert(o.client_order_id.clone()), "duplicate coid {:?}", o.client_order_id);
    }
}

/// Two sessions (two `vike-cli trade` processes, or one restarted while an order still rests)
/// must not mint the same id — the session prefix is what buys that.
#[test]
fn two_sessions_do_not_share_a_coid_prefix() {
    let (mut a, mut b) = (coid_minter(), coid_minter());
    let first = |m: &mut ClientOrderIdGenerator| m.generate();
    assert_ne!(first(&mut a), first(&mut b), "two sessions minted the same first coid");
}

/// An EXPLICIT id is never overwritten (the `--coid` / `client_order_id` override path), and no
/// non-`Submit` verb is touched by the mint.
#[test]
fn the_mint_never_overwrites_an_explicit_id_or_touches_another_verb() {
    let mut minter = coid_minter();
    let mut pinned = submit_cmd(1.0, Some(100.0));
    if let WireCommand::Submit(o) = &mut pinned {
        o.client_order_id = "operatorPinned7".into();
    }
    assert_eq!(fill_client_order_id(pinned.clone(), &mut minter), pinned);

    for cmd in [
        WireCommand::Cancel("c-1".into()),
        WireCommand::MarketExit { venue: None, account: None },
        WireCommand::MassCancel { venue: None, symbol: None, account: None },
    ] {
        assert_eq!(fill_client_order_id(cmd.clone(), &mut minter), cmd);
    }
}

/// The MCP write path builds through `wire_command_for`, which still emits an EMPTY coid when
/// the agent omits the optional argument — so the mint is what makes that path work too, and an
/// agent-supplied id still wins.
#[test]
fn the_mcp_submit_tool_gets_a_minted_coid_when_the_agent_omits_one() {
    let mut minter = coid_minter();
    let bare = wire_command_for(
        "submit_order",
        &json!({ "venue": "sim", "symbol": "B", "side": 1, "qty": 1.0 }),
    )
    .unwrap();
    let WireCommand::Submit(o) = fill_client_order_id(bare, &mut minter) else {
        panic!("still a Submit")
    };
    assert!(vike_model::is_valid_crypto_coid(&o.client_order_id));

    let pinned = wire_command_for(
            "submit_order",
            &json!({ "venue": "sim", "symbol": "B", "side": 1, "qty": 1.0, "client_order_id": "agentPinned1" }),
        )
        .unwrap();
    let WireCommand::Submit(o) = fill_client_order_id(pinned, &mut minter) else {
        panic!("still a Submit")
    };
    assert_eq!(o.client_order_id, "agentPinned1", "an agent-supplied id is authoritative");
}

#[test]
fn guardrail_json_and_line_render_the_same_check() {
    // No caps at all: a priced submit is within limits, notional = qty * price.
    let g = guardrail_check(&submit_cmd(0.5, Some(100.0)), GuardrailCaps::default());
    let j = g.to_json();
    assert_eq!(j["qty"], 0.5);
    assert_eq!(j["notional"], 50.0);
    assert_eq!(j["within_limits"], true);
    let line = g.line();
    assert!(line.contains("qty=0.5") && line.contains("notional=50"), "{line}");
    assert!(line.contains("within limits"), "{line}");
}

/// The POLICY ceiling reaching the advisory check — the "value flows" half of Phase 5's
/// contract at this surface. PURE now, so it needs no process-env mutation to test (which is
/// unsound from parallel test threads and is why this was never covered before).
#[test]
fn the_policy_ceiling_is_what_the_notional_check_uses() {
    let caps = GuardrailCaps { max_qty: None, max_notional: Some(40.0) };
    let g = guardrail_check(&submit_cmd(0.5, Some(100.0)), caps); // notional 50 > 40
    assert_eq!(g.to_json()["max_notional"], 40.0);
    assert_eq!(g.to_json()["within_limits"], false);
    assert!(g.line().contains("OVER LIMIT"), "{}", g.line());

    // …and no ceiling (no `policy.max_notional_per_order` row) is today's behaviour: nothing is
    // over the limit.
    let g = guardrail_check(&submit_cmd(0.5, Some(100.0)), GuardrailCaps::default());
    assert_eq!(g.to_json()["max_notional"], Value::Null);
    assert_eq!(g.to_json()["within_limits"], true);
}

// -- the preview's number rendering -----------------------------------------------------------

/// **The reported line, and the reason this renderer exists.**
///
/// `0.4 * 3` is exactly `1.2000000000000002` as an `f64`, and `to_string` prints all of it. The
/// guardrail line therefore read
/// `guardrail: qty=3 notional=1.2000000000000002 max_qty=- max_notional=42.5` for an order the
/// operator typed as three at forty cents.
#[test]
fn the_preview_renders_a_typed_number_the_way_it_was_typed() {
    let caps = GuardrailCaps { max_qty: None, max_notional: Some(42.5) };
    let line = guardrail_check(&submit_cmd(3.0, Some(0.4)), caps).line();
    assert_eq!(
        line, "guardrail: qty=3 notional=1.2 max_qty=- max_notional=42.5 → within limits",
        "the float tail must not reach the preview"
    );
    // The raw value really is the ugly one — this is a RENDERING fix, not a computation change.
    assert_eq!((0.4f64 * 3.0).to_string(), "1.2000000000000002");
}

/// Dropping noise is not the same as dropping precision: a value whose digits carry information
/// keeps all of them, out to [`MAX_RENDER_DECIMALS`].
#[test]
fn rendering_drops_noise_and_nothing_else() {
    for (v, expect) in [
        (1.2000000000000002_f64, "1.2"),
        (0.1 + 0.2, "0.3"),
        (3.0, "3"),
        (42.5, "42.5"),
        (0.0001234, "0.0001234"),
        (1234.56789, "1234.56789"),
        (-0.30000000000000004, "-0.3"),
        (0.0, "0"),
        (1e-10, "0.0000000001"),
        (1e20, "100000000000000000000"),
    ] {
        assert_eq!(fmt_num(v), expect, "fmt_num({v})");
    }
    // Non-finite falls through to Display rather than looping to the precision cap.
    assert_eq!(fmt_num(f64::NAN), "NaN");
    assert_eq!(fmt_num(f64::INFINITY), "inf");
}

/// **A notional may never be shortened into looking like it clears the ceiling it exceeds.**
///
/// The display drops noise; it does not get to soften a verdict. Without the floor rule this
/// line would read `notional=42.5 max_notional=42.5 → OVER LIMIT`, i.e. a verdict its own two
/// numbers appear to contradict — precisely the "output that communicates the opposite of the
/// truth" this whole change is about.
#[test]
fn a_notional_is_never_rendered_below_the_ceiling_it_exceeds() {
    // The next representable f64 above the cap — computed, not written as a literal, so the
    // "one ulp over" intent is in the code rather than in a digit count a reader has to verify
    // (and so clippy's `excessive_precision` has nothing to object to).
    let over = f64::from_bits(42.5_f64.to_bits() + 1);
    assert!(over > 42.5, "the premise: this f64 is genuinely over");
    assert_eq!(fmt_num(over), "42.5", "…and shortening alone would hide that");
    assert_eq!(fmt_num_over(over, Some(42.5)), over.to_string(), "so the exact value is shown");

    // A value genuinely UNDER the cap is rendered normally — the rule fires only when hiding
    // the difference would contradict the verdict.
    assert_eq!(fmt_num_over(1.2000000000000002, Some(42.5)), "1.2");
    // …and with no cap there is nothing to understate against.
    assert_eq!(fmt_num_over(over, None), "42.5");
}

/// The rendering is DISPLAY ONLY: `within_limits` is computed from the raw `f64`s, so no
/// tolerance in the renderer can change a verdict.
#[test]
fn rendering_never_moves_the_verdict() {
    let caps = GuardrailCaps { max_qty: None, max_notional: Some(42.5) };
    // qty is one ulp above 1.0, so qty * price lands just over the ceiling — and the check must
    // say so, however the line renders it.
    let qty = f64::from_bits(1.0_f64.to_bits() + 1);
    let g = guardrail_check(&submit_cmd(qty, Some(42.5)), caps);
    assert_eq!(g.to_json()["within_limits"], false, "the raw comparison decides");
    assert!(g.line().contains("OVER LIMIT"), "{}", g.line());
    // The JSON keeps exact numbers — a machine reader must never get a formatted string.
    assert!(g.to_json()["notional"].is_number(), "{}", g.to_json());
}

// -- the coid charset, on the verbs that do not mint one --------------------------------------

/// `cancel`/`modify` name an id that must ALREADY exist; `submit` mints one. So the charset is
/// stated on all three and enforced on one. See [`coid_charset_warning`] for the argument.
#[test]
fn a_coid_no_minter_could_have_produced_warns_on_cancel_and_modify() {
    for cmd in [
        WireCommand::Cancel("MYPINNED-001".into()),
        WireCommand::Modify {
            client_order_id: "!!!bad***coid".into(),
            new_qty: Some(2.0),
            new_price: None,
        },
    ] {
        let w = coid_charset_warning(&cmd)
            .unwrap_or_else(|| panic!("an unmintable coid must warn: {cmd:?}"));
        assert!(w.contains(COID_CHARSET), "the warning must state the charset: {w}");
        assert!(
            w.contains("sending it anyway"),
            "it must be clear the command is still sent — cancel stays fire-and-forget: {w}"
        );
    }
}

/// A conforming id is silent, and a `Submit` never warns at all: by the time one reaches the
/// preview its coid has been through `parse_submit`'s refusal or `fill_client_order_id`'s mint.
#[test]
fn a_conforming_coid_and_every_submit_are_silent() {
    assert_eq!(coid_charset_warning(&WireCommand::Cancel("baa0ec7d00".into())), None);
    assert_eq!(
        coid_charset_warning(&WireCommand::Modify {
            client_order_id: "baa0ec7d00".into(),
            new_qty: None,
            new_price: Some(1.0),
        }),
        None
    );
    assert_eq!(coid_charset_warning(&submit_cmd(1.0, Some(1.0))), None);
    // …and neither do the verbs that name no order at all.
    assert_eq!(
        coid_charset_warning(&WireCommand::MassCancel { venue: None, symbol: None, account: None }),
        None
    );
}

/// The warning and the refusal must describe the SAME charset, or the two surfaces teach
/// different rules again — which is the defect, one level up.
#[test]
fn the_warning_agrees_with_the_validator_it_describes() {
    for id in ["MYPINNED-001", "!!!bad***coid", "", &"a".repeat(33)] {
        assert!(!vike_model::is_valid_crypto_coid(id), "premise: {id:?} is invalid");
        assert!(coid_charset_warning(&WireCommand::Cancel(id.into())).is_some(), "{id:?}");
    }
    for id in ["baa0ec7d00", "A", &"z".repeat(32)] {
        assert!(vike_model::is_valid_crypto_coid(id), "premise: {id:?} is valid");
        assert!(coid_charset_warning(&WireCommand::Cancel(id.into())).is_none(), "{id:?}");
    }
}

#[test]
fn guardrail_market_order_has_no_notional_to_check() {
    // Even WITH a ceiling: a market order carries no price, so nothing can be sized
    // client-side — the node is the one that will know.
    let caps = GuardrailCaps { max_qty: None, max_notional: Some(1.0) };
    let g = guardrail_check(&submit_cmd(2.0, None), caps);
    assert_eq!(g.to_json()["notional"], Value::Null);
    assert_eq!(g.to_json()["within_limits"], true);
    assert!(g.line().contains("notional=-"), "{}", g.line());
}

#[test]
fn guardrail_non_submit_verbs_have_no_size_to_check() {
    let g = guardrail_check(&WireCommand::Cancel("c-1".into()), GuardrailCaps::default());
    assert_eq!(g, Guardrail::NoSize);
    assert_eq!(g.to_json()["within_limits"], true);
    assert_eq!(g.line(), "guardrail: no order size to check for this verb");
}

// -- the unsizeable-order refusal: the shared function and the MCP tool's use of it -------------
//
// The REPL, the one-shot verb and the tool agreeing on the SAME classes is one table driving all
// three real parsers, in `crate::cmd::trade`'s tests (it needs the private `order` module, which
// this file cannot see). What can only be said HERE: the non-finite rows over a hand-built request
// (JSON cannot carry them, so the tool has no such leg), the `limit`-with-no-price row (the two
// command-line grammars cannot spell it), and the tool's own acceptance of every sizeable shape.

fn submit_req(qty: f64, order_type: &str, price: Option<f64>) -> WireOrderRequest {
    WireOrderRequest {
        client_order_id: String::new(),
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        side: 1,
        qty,
        order_type: order_type.into(),
        price,
        trigger_price: None,
        reduce_only: false,
        account: None,
    }
}

/// `submit_order`'s arguments: a MARKET order for one unit, with `extra`'s keys laid over it.
fn submit_args(extra: Value) -> Value {
    let mut args = json!({ "venue": "sim", "symbol": "BTCUSDT", "side": 1, "qty": 1.0 });
    for (k, v) in extra.as_object().expect("`extra` is an object") {
        args[k.as_str()] = v.clone();
    }
    args
}

/// Every class the function refuses, with the reason it gives — the NON-FINITE rows included, which
/// no JSON argument can reach.
#[test]
fn the_shared_refusal_names_each_unsizeable_class() {
    for (class, req, reason) in [
        ("qty zero", submit_req(0.0, "market", None), "qty must be > 0"),
        ("qty negative", submit_req(-1.0, "market", None), "qty must be > 0"),
        ("qty NaN", submit_req(f64::NAN, "market", None), "qty must be a finite number, got NaN"),
        (
            "qty +inf",
            submit_req(f64::INFINITY, "market", None),
            "qty must be a finite number, got inf",
        ),
        (
            "qty -inf",
            submit_req(f64::NEG_INFINITY, "market", None),
            "qty must be a finite number, got -inf",
        ),
        (
            "price NaN",
            submit_req(1.0, "limit", Some(f64::NAN)),
            "price must be a finite number, got NaN",
        ),
        (
            "price +inf",
            submit_req(1.0, "limit", Some(f64::INFINITY)),
            "price must be a finite number, got inf",
        ),
        (
            "price -inf",
            submit_req(1.0, "limit", Some(f64::NEG_INFINITY)),
            "price must be a finite number, got -inf",
        ),
        ("limit, no price", submit_req(1.0, "limit", None), "a limit order needs a price"),
        ("LIMIT, no price", submit_req(1.0, "LIMIT", None), "a limit order needs a price"),
    ] {
        assert_eq!(unsizeable_submit_refusal(&req).as_deref(), Some(reason), "{class}");
    }
}

/// The refusal is for an order with NO size and nothing else: every shape below is something the
/// node judges, so refusing it here would be a new refusal the node never made.
#[test]
fn the_shared_refusal_leaves_every_sizeable_order_alone() {
    let trigger = |order_type: &str| WireOrderRequest {
        trigger_price: Some(90.0),
        ..submit_req(1.0, order_type, None)
    };
    for (class, req) in [
        ("market, no price", submit_req(1.0, "market", None)),
        ("limit with a price", submit_req(1.0, "limit", Some(100.0))),
        ("market carrying a price", submit_req(1.0, "market", Some(100.0))),
        // Zero and negative PRICES are the node's call — see `unsizeable_submit_refusal`'s doc.
        ("limit at price zero", submit_req(1.0, "limit", Some(0.0))),
        ("limit at a negative price", submit_req(1.0, "limit", Some(-5.0))),
        ("stop, trigger only", trigger("stop")),
        ("take_profit, trigger only", trigger("take_profit")),
        ("the smallest positive qty", submit_req(f64::MIN_POSITIVE, "market", None)),
        ("an enormous but finite qty", submit_req(1e300, "limit", Some(1e300))),
    ] {
        assert_eq!(unsizeable_submit_refusal(&req), None, "{class}");
    }
}

/// The tool's own rows for the two classes JSON CAN carry: a `qty` of zero or below.
#[test]
fn the_mcp_submit_tool_refuses_a_non_positive_qty_at_parse() {
    for (class, qty) in [("zero", 0.0), ("negative", -1.0), ("negative zero", -0.0)] {
        let args = submit_args(json!({ "qty": qty }));
        assert_eq!(
            verb_from_tool_args("submit_order", &args).unwrap_err(),
            "submit_order: qty must be > 0",
            "{class}"
        );
        // …and the one entry the tool actually calls refuses it the same way.
        assert!(wire_command_for("submit_order", &args).is_err(), "{class}");
    }
}

/// A `limit` with no `price` — the one class only this path can build, since the two command-line
/// grammars make a limit BY naming its `@price`. An explicit `null`, a price smuggled in as the
/// trigger, and a different casing of the type are all the same absence.
#[test]
fn the_mcp_submit_tool_refuses_a_limit_order_with_no_price() {
    for (class, extra) in [
        ("order_type only", json!({ "order_type": "limit" })),
        ("price null", json!({ "order_type": "limit", "price": null })),
        ("trigger_price is not a price", json!({ "order_type": "limit", "trigger_price": 9.0 })),
        ("another casing", json!({ "order_type": "LIMIT" })),
    ] {
        let args = submit_args(extra);
        assert_eq!(
            verb_from_tool_args("submit_order", &args).unwrap_err(),
            "submit_order: a limit order needs a price",
            "{class}"
        );
    }
}

/// JSON has no NaN and no infinity, and `json!` of one is `null` — so on THIS path a non-finite
/// `qty` is a MISSING field, which the tool already refused. Pinned so the shape cannot start
/// reading one as a number.
#[test]
fn a_non_finite_or_text_qty_is_still_refused_as_missing_on_the_mcp_path() {
    for (class, qty) in [
        ("NaN becomes null", json!(f64::NAN)),
        ("+inf becomes null", json!(f64::INFINITY)),
        ("text that parses as a float", json!("inf")),
        ("text", json!("5")),
    ] {
        let err =
            verb_from_tool_args("submit_order", &submit_args(json!({ "qty": qty }))).unwrap_err();
        assert!(err.contains("requires `qty`"), "{class}: {err}");
    }
}

/// Every sizeable shape the tool took before still builds — and a sizeable order over every cap is
/// still built and still only ADVISED about.
#[test]
fn the_mcp_submit_tool_still_builds_every_sizeable_shape_and_the_guardrail_only_advises() {
    for (class, extra) in [
        ("default market", json!({})),
        ("limit with a price", json!({ "order_type": "limit", "price": 100.0 })),
        ("market carrying a price", json!({ "order_type": "market", "price": 100.0 })),
        ("stop, trigger only", json!({ "order_type": "stop", "trigger_price": 90.0 })),
        (
            "take_profit, trigger only",
            json!({ "order_type": "take_profit", "trigger_price": 110.0 }),
        ),
        ("reduce-only market", json!({ "reduce_only": true })),
        ("limit at price zero", json!({ "order_type": "limit", "price": 0.0 })),
        ("limit at a negative price", json!({ "order_type": "limit", "price": -5.0 })),
    ] {
        let args = submit_args(extra);
        let verb = verb_from_tool_args("submit_order", &args)
            .unwrap_or_else(|e| panic!("{class} must still build: {e}"));
        assert!(matches!(verb, Verb::Submit(_)), "{class}");
    }

    let caps = GuardrailCaps { max_qty: Some(1.0), max_notional: Some(10.0) };
    for (class, extra) in [
        ("limit over both caps", json!({ "qty": 500.0, "order_type": "limit", "price": 100.0 })),
        ("market over the qty cap", json!({ "qty": 500.0 })),
    ] {
        let cmd = wire_command_for("submit_order", &submit_args(extra))
            .unwrap_or_else(|e| panic!("{class} must still build: {e}"));
        let g = guardrail_check(&cmd, caps);
        assert_eq!(g.to_json()["within_limits"], false, "{class}");
        assert!(g.line().contains("OVER LIMIT"), "{class}: {}", g.line());
    }
}
