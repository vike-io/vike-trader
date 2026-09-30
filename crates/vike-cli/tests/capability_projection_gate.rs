//! **Every wire command either reaches an operator through the CLI, or through the MCP surface, or
//! says why not.** Directions 2 and 3 of the capability-projection idiom Task 1 opened for direction
//! 1 (core -> wire, `crates/vike-tradehub/tests/capability_projection_gate.rs`) — this file shares
//! its idiom but no code, because it starts one layer down: `vike_tradehub_client::wire::WireCommand`
//! rather than `vike_exec`'s internal `Command`/`OrderIntent`.
//!
//! The defect this closes is the same shape as Task 1's: a wire command that exists is not
//! automatically a command an operator can actually ISSUE. `WireCommand` is the standalone client
//! wire type both surfaces lower into — `crate::cmd::trade`'s one-shot verbs and `crate::cmd::mcp`'s
//! tool registry both build one and hand it to the same `remote_control` call. A variant either has
//! a CLI verb, an MCP tool, or a written reason it has neither; nothing is allowed to fall through
//! silently.
//!
//! STEP 1 of the two-step playbook: declare today's reality, every gap named with its own reason.
//! `ON_THE_CLI`/`ON_THE_MCP` are the mappings; `NOT_ON_THE_CLI`/`NOT_ON_THE_MCP` are the admissions.
//!
//! ⚠ **`ON_THE_CLI` names verbs this plan has not written yet** (`trade order submit` and its
//! siblings arrive in Tasks 4-9) — that is deliberate, and it is what makes this gate DRIVE those
//! tasks rather than merely describe them once they exist. Whether the strings are real is a
//! SEPARATE question, checked in Step 3 below against the published surface.

use vike_tradehub_client::wire::{WireCommand, WireTradingState};

/// One-shot CLI spellings, wire variant -> the verb that produces it.
const ON_THE_CLI: &[(&str, &str)] = &[
    ("WireCommand::Submit", "trade order submit"),
    ("WireCommand::Cancel", "trade order cancel"),
    ("WireCommand::Modify", "trade order modify"),
    ("WireCommand::MassCancel", "trade order mass-cancel"),
    ("WireCommand::Flatten", "trade position flatten"),
    ("WireCommand::MarketExit", "trade position close-all"),
    ("WireCommand::SetTradingState", "trade halt / trade resume"),
    ("WireCommand::MountStrategy", "trade strategy mount"),
    ("WireCommand::UnmountStrategy", "trade strategy unmount"),
];

/// ⚠ A row here is a written admission, never a silencer — the same rule Task 1's `NOT_ON_THE_WIRE`
/// states for its own table. Deleting a row is what STEP 2 looks like; adding one needs the reason
/// to argue for itself.
const NOT_ON_THE_CLI: &[(&str, &str)] = &[
    (
        "WireCommand::UpdateParams",
        "no verb on either surface. A one-knob patch needs a STRUCTURED read of the mount's current \
         params to patch against, and WireMountRow::params is a RENDERED string - sending a whole \
         params object would silently reset every other knob. Spec §17.1.",
    ),
    (
        "WireCommand::SetSetting",
        "reachable as a REPL line and an MCP tool, and not yet one-shot. The reason this row \
         used to give - a policy write demanded a TYPED key confirm --yes could not answer - is \
         gone with docs/decisions/0086 point 7: the REPL line now completes under --yes, even \
         over a pipe. So this is an unbuilt spelling, not a decision. `vike-cli config set` \
         remains the route on the node's own box.",
    ),
];

/// Wire variant -> the MCP tool that produces it. Every name here is a measured `"name":` string in
/// `crates/vike-cli/src/cmd/mcp.rs`'s tool registry (`submit_order`, `cancel_order`, `modify`,
/// `flatten`, `market_exit`, `mass_cancel`, `set_trading_state`, `mount_strategy`,
/// `unmount_strategy`, `set_setting`), not a guess from the wire variant's own name.
const ON_THE_MCP: &[(&str, &str)] = &[
    ("WireCommand::Submit", "submit_order"),
    ("WireCommand::Cancel", "cancel_order"),
    ("WireCommand::Modify", "modify"),
    ("WireCommand::MassCancel", "mass_cancel"),
    ("WireCommand::Flatten", "flatten"),
    ("WireCommand::MarketExit", "market_exit"),
    ("WireCommand::SetTradingState", "set_trading_state"),
    ("WireCommand::MountStrategy", "mount_strategy"),
    ("WireCommand::UnmountStrategy", "unmount_strategy"),
    ("WireCommand::SetSetting", "set_setting"),
];

const NOT_ON_THE_MCP: &[(&str, &str)] = &[(
    "WireCommand::UpdateParams",
    "same reason as NOT_ON_THE_CLI - the structured params read does not exist.",
)];

/// The variant name every arm reports itself as — the exhaustive match is the gate: a NEW
/// `WireCommand` variant fails to compile here until its author classifies it on both tables below.
fn wire_command_name(cmd: &WireCommand) -> &'static str {
    match cmd {
        WireCommand::Submit(_) => "WireCommand::Submit",
        WireCommand::Cancel(_) => "WireCommand::Cancel",
        WireCommand::Modify { .. } => "WireCommand::Modify",
        WireCommand::MassCancel { .. } => "WireCommand::MassCancel",
        WireCommand::Flatten { .. } => "WireCommand::Flatten",
        WireCommand::MarketExit { .. } => "WireCommand::MarketExit",
        WireCommand::SetTradingState(_) => "WireCommand::SetTradingState",
        WireCommand::UpdateParams { .. } => "WireCommand::UpdateParams",
        WireCommand::MountStrategy { .. } => "WireCommand::MountStrategy",
        WireCommand::UnmountStrategy { .. } => "WireCommand::UnmountStrategy",
        WireCommand::SetSetting { .. } => "WireCommand::SetSetting",
    }
}

/// Every variant name, produced by constructing one of each and asking [`wire_command_name`].
///
/// ⚠ Deliberately NOT a hand-written list: the exhaustive match in `wire_command_name` is what a new
/// variant breaks, and this function is what makes that match run. Field values are placeholders —
/// only the discriminant is ever inspected.
fn every_wire_command_variant_name() -> Vec<&'static str> {
    use vike_tradehub_client::wire::WireOrderRequest;

    let order_request = WireOrderRequest {
        client_order_id: String::new(),
        venue: String::new(),
        symbol: String::new(),
        side: 1,
        qty: 0.0,
        order_type: String::new(),
        price: None,
        trigger_price: None,
        reduce_only: false,
        account: None,
    };

    vec![
        wire_command_name(&WireCommand::Submit(order_request)),
        wire_command_name(&WireCommand::Cancel(String::new())),
        wire_command_name(&WireCommand::Modify {
            client_order_id: String::new(),
            new_qty: None,
            new_price: None,
        }),
        wire_command_name(&WireCommand::MassCancel { venue: None, symbol: None, account: None }),
        wire_command_name(&WireCommand::Flatten {
            venue: String::new(),
            symbol: String::new(),
            account: None,
        }),
        wire_command_name(&WireCommand::MarketExit { venue: None, account: None }),
        wire_command_name(&WireCommand::SetTradingState(WireTradingState::Active)),
        wire_command_name(&WireCommand::UpdateParams {
            venue: String::new(),
            symbol: String::new(),
            interval: String::new(),
            params: serde_json::Value::Null,
        }),
        wire_command_name(&WireCommand::MountStrategy {
            venue: String::new(),
            account: None,
            symbol: String::new(),
            interval: String::new(),
            controller_id: None,
            name: None,
            rhai: None,
            params: serde_json::Value::Null,
        }),
        wire_command_name(&WireCommand::UnmountStrategy { controller_id: String::new() }),
        wire_command_name(&WireCommand::SetSetting {
            file: String::new(),
            key: String::new(),
            value: String::new(),
            confirm: None,
        }),
    ]
}

#[test]
fn every_wire_command_has_a_cli_verb_or_a_reason() {
    let excused: std::collections::BTreeSet<&str> =
        NOT_ON_THE_CLI.iter().map(|(name, _)| *name).collect();
    let mapped: std::collections::BTreeSet<&str> =
        ON_THE_CLI.iter().map(|(name, _)| *name).collect();

    for (name, _) in NOT_ON_THE_CLI {
        assert!(
            !mapped.contains(name),
            "{name} is in BOTH ON_THE_CLI and NOT_ON_THE_CLI - a mapped command may not also carry \
             an excuse"
        );
    }

    for name in every_wire_command_variant_name() {
        assert!(
            excused.contains(name) || mapped.contains(name),
            "{name} has no CLI verb and no NOT_ON_THE_CLI reason. Either give it a verb (and add it \
             to ON_THE_CLI) or add a row saying why an operator cannot reach it from the CLI."
        );
    }
}

#[test]
fn every_wire_command_has_an_mcp_tool_or_a_reason() {
    let excused: std::collections::BTreeSet<&str> =
        NOT_ON_THE_MCP.iter().map(|(name, _)| *name).collect();
    let mapped: std::collections::BTreeSet<&str> =
        ON_THE_MCP.iter().map(|(name, _)| *name).collect();

    for (name, _) in NOT_ON_THE_MCP {
        assert!(
            !mapped.contains(name),
            "{name} is in BOTH ON_THE_MCP and NOT_ON_THE_MCP - a mapped command may not also carry \
             an excuse"
        );
    }

    for name in every_wire_command_variant_name() {
        assert!(
            excused.contains(name) || mapped.contains(name),
            "{name} has no MCP tool and no NOT_ON_THE_MCP reason. Either give it a tool (and add it \
             to ON_THE_MCP) or add a row saying why an operator cannot reach it from MCP."
        );
    }
}

#[test]
fn every_excuse_states_a_reason() {
    for (name, reason) in NOT_ON_THE_CLI.iter().chain(NOT_ON_THE_MCP.iter()) {
        assert!(
            reason.len() > 40,
            "{name}'s reason is too short to be one: {reason:?}. A row is a written admission."
        );
    }
}

/// Walk `doc["planes"]` for the `trade` entry and look `sub` up in its groups — `sub` is either
/// `"<group> <verb>"` (e.g. `"order submit"`) or a bare group-less top-level verb (`"halt"`,
/// `"resume"` — the risk-direction exemption `crates/vike-cli/src/cmd/trade/plane.rs`'s module doc
/// states: a verb that takes no book takes no group).
///
/// ⚠ IGNORED until Task 11 adds the additive `planes` key to `crates/vike-cli/src/surface.rs`. The
/// assertion is what makes ON_THE_CLI a CLAIM rather than a list, so it must exist now and must not
/// be deleted — Task 11 removes this attribute.
fn surface_names_trade_verb(doc: &serde_json::Value, sub: &str) -> bool {
    let Some(planes) = doc.get("planes").and_then(|v| v.as_array()) else {
        return false;
    };
    let Some(trade) =
        planes.iter().find(|p| p.get("plane").and_then(|v| v.as_str()) == Some("trade"))
    else {
        return false;
    };

    if let Some((group_name, verb_name)) = sub.split_once(' ') {
        let Some(groups) = trade.get("groups").and_then(|v| v.as_array()) else {
            return false;
        };
        groups.iter().any(|g| {
            g.get("group").and_then(|v| v.as_str()) == Some(group_name)
                && g.get("verbs")
                    .and_then(|v| v.as_array())
                    .is_some_and(|verbs| verbs.iter().any(|x| x.as_str() == Some(verb_name)))
        })
    } else {
        // A group-less top-level verb (`halt` / `resume`) lives on the plane's own sub-verb roster.
        trade
            .get("sub_verbs")
            .and_then(|v| v.as_array())
            .is_some_and(|verbs| verbs.iter().any(|x| x.as_str() == Some(sub)))
    }
}

/// ⚠ `ON_THE_CLI` is worthless if nothing checks the verb STRINGS against the shipped surface.
/// `crates/vike-cli/src/surface.rs` is that surface as DATA, so the claim is checkable in-process.
///
/// ⚠ IGNORED until Task 11 adds the additive `planes` key to `crates/vike-cli/src/surface.rs`. The
/// assertion is what makes ON_THE_CLI a CLAIM rather than a list, so it must exist now and must not
/// be deleted — Task 11 removes this attribute.
#[test]
#[ignore]
fn every_claimed_cli_verb_exists_in_the_published_surface() {
    let files = vike_cli::surface::rendered_files();
    let doc: serde_json::Value =
        serde_json::from_str(files.get("cli.json").expect("cli.json is rendered"))
            .expect("cli.json parses");
    for (variant, verb) in ON_THE_CLI {
        // `trade halt / trade resume` is the one row naming two verbs; check the first word after
        // `trade` for each alternative.
        for alternative in verb.split(" / ") {
            let sub = alternative.strip_prefix("trade ").unwrap_or(alternative);
            assert!(
                surface_names_trade_verb(&doc, sub),
                "{variant} claims `{alternative}`, which the published surface does not carry"
            );
        }
    }
}
