//! **Every wire command either reaches an operator through the CLI, or through the MCP surface, or
//! says why not.** Directions 2 and 3 of the capability-projection idiom Task 1 opened for direction
//! 1 (core -> wire, `crates/vike-tradehub/tests/daemon/capability_projection_gate.rs`) — this file shares
//! its idiom and ONE helper with it (the variant names, compiled once in
//! `crates/vike-tradehub-client/src/test_support/wire_command_names.rs`), because it starts one layer
//! down: `vike_tradehub_client::wire::CommandWire` rather than `vike_execution`'s internal
//! `Command`/`OrderIntent`.
//!
//! The defect this closes is the same shape as Task 1's: a wire command that exists is not
//! automatically a command an operator can actually ISSUE. `CommandWire` is the standalone client
//! wire type both surfaces lower into — `crate::cmd::trade`'s one-shot verbs and `vike_mcp`'s
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

// The exhaustive match over `CommandWire` is the gate's first half: a NEW variant fails to compile
// there until it is named, and the two tests below refuse it until it is classified on both tables.
use vike_tradehub_client::test_support::wire_command_names::every_wire_command_variant_name;

/// One-shot CLI spellings, wire variant -> the verb that produces it.
const ON_THE_CLI: &[(&str, &str)] = &[
    ("CommandWire::Submit", "trade order submit"),
    ("CommandWire::Bracket", "trade order bracket"),
    ("CommandWire::OrderList", "trade order oco / trade order oto / trade order ladder"),
    ("CommandWire::Combo", "trade order combo"),
    ("CommandWire::Cancel", "trade order cancel"),
    ("CommandWire::Modify", "trade order modify"),
    ("CommandWire::MassCancel", "trade order mass-cancel"),
    ("CommandWire::Flatten", "trade position flatten"),
    ("CommandWire::MarketExit", "trade position close-all"),
    ("CommandWire::SetTradingState", "trade halt / trade resume"),
    ("CommandWire::MountStrategy", "trade strategy mount"),
    ("CommandWire::UnmountStrategy", "trade strategy unmount"),
    ("CommandWire::ReconcileConfirm", "trade recon confirm"),
    ("CommandWire::ReconcileCancel", "trade recon cancel"),
    ("CommandWire::ResumeVenue", "trade resume-venue"),
    (
        "CommandWire::SetAccountTradingState",
        "trade account halt / trade account reduce / trade account resume",
    ),
    ("CommandWire::UnadmitSymbol", "trade account remove-symbol"),
    ("CommandWire::UnlatchMount", "trade strategy reset-budget"),
];

/// ⚠ A row here is a written admission, never a silencer — the same rule Task 1's `NOT_ON_THE_WIRE`
/// states for its own table. Deleting a row is what STEP 2 looks like; adding one needs the reason
/// to argue for itself.
const NOT_ON_THE_CLI: &[(&str, &str)] = &[
    (
        "CommandWire::UpdateParams",
        "no verb on either surface. The structured read now exists (MountViewWire::typed_params, \
         and MountViewWire::mount_id to address one mount), but a one-knob patch still needs a \
         read-modify-write verb built on it - sending a whole params object would silently reset \
         every other knob. An unbuilt spelling, not a decision. Spec §17.1.",
    ),
    (
        "CommandWire::SetSetting",
        "reachable as a REPL line and an MCP tool, and not yet one-shot. The reason this row \
         used to give - a policy write demanded a TYPED key confirm --yes could not answer - is \
         gone with docs/decisions/0086 point 7: the REPL line now completes under --yes, even \
         over a pipe. So this is an unbuilt spelling, not a decision. `vike-cli config set` \
         remains the route on the node's own box.",
    ),
];

/// Wire variant -> the MCP tool that produces it. Every name here is a measured `"name":` string in
/// `crates/vike-mcp/src/tool_schemas.rs`'s tool registry (`submit_order`, `cancel_order`, `modify`,
/// `flatten`, `market_exit`, `mass_cancel`, `set_trading_state`, `mount_strategy`,
/// `unmount_strategy`, `set_setting`), not a guess from the wire variant's own name.
const ON_THE_MCP: &[(&str, &str)] = &[
    ("CommandWire::Submit", "submit_order"),
    ("CommandWire::OrderList", "submit_order_list"),
    ("CommandWire::Combo", "submit_combo"),
    ("CommandWire::Cancel", "cancel_order"),
    ("CommandWire::Modify", "modify"),
    ("CommandWire::MassCancel", "mass_cancel"),
    ("CommandWire::Flatten", "flatten"),
    ("CommandWire::MarketExit", "market_exit"),
    ("CommandWire::SetTradingState", "set_trading_state"),
    ("CommandWire::MountStrategy", "mount_strategy"),
    ("CommandWire::UnmountStrategy", "unmount_strategy"),
    ("CommandWire::SetSetting", "set_setting"),
];

const NOT_ON_THE_MCP: &[(&str, &str)] = &[
    (
        "CommandWire::ReconcileConfirm",
        "no tool yet. Resolving a held divergence is an operator judgement made against the \
         venue's own account - it can DROP a local position - and the unattended agent surface \
         does not ship it before the operator verb (`trade recon confirm`) has been used.",
    ),
    (
        "CommandWire::ReconcileCancel",
        "no tool, by decision 0196 rule (b): cancelling the venue order a held UnknownOrder row names is \
         an explicit OPERATOR act on a demo account (the row can be another instance's order or a \
         position's only stop), and the unattended agent surface never gets it.",
    ),
    (
        "CommandWire::ResumeVenue",
        "no tool yet. Releasing a dead venue's halt is an operator judgement that the link is really \
         back (the node refuses while its links read down, and `--force` overrides that), and the \
         unattended agent surface does not ship it before the operator verb (`trade resume-venue`) \
         has been used.",
    ),
    (
        "CommandWire::SetAccountTradingState",
        "no tool yet. Halting, reducing or resuming one account's engine is an operator judgement about \
         that account (a resume lifts a halt a restart or an earlier halt put there), and the \
         unattended agent surface does not ship it before the operator verb (`trade account`) \
         has been used. The node-wide `set_trading_state` tool is unchanged.",
    ),
    (
        "CommandWire::UnadmitSymbol",
        "no tool yet. Giving an admitted symbol back frees one of an engine's sixteen slots and makes the \
         node forget the symbol's price lane and its reconcile read (decision 0205): an operator \
         judgement made after looking at what the account holds, and the unattended agent surface does \
         not ship it before the operator verb (`trade account remove-symbol`) has been used.",
    ),
    (
        "CommandWire::UnlatchMount",
        "no tool yet. Clearing a mount's budget latch lets a strategy that breached its loss or notional budget \
         trade again and re-arms its loss budget from where it stands (decision 0239): an operator judgement made \
         after looking at what the mount holds and why it lost, and the unattended agent surface does not ship it \
         before the operator verb (`trade strategy reset-budget`) has been used.",
    ),
    (
        "CommandWire::UpdateParams",
        "same reason as NOT_ON_THE_CLI - the read-modify-write tool over typed_params is not built.",
    ),
    (
        "CommandWire::Bracket",
        "no tool yet. A bracket carries no id to dedup a retry by: an agent that retries after an \
         ambiguous reply (a timeout, a dropped link) would place a second bracket, so a tool must \
         not ship before a bracket carries a client-minted id. The one-shot `trade order bracket` \
         has a person at its preview and confirm, and its help says to read `trade order ls` \
         before sending again after a `not yet known` answer; an unattended agent has neither.",
    ),
];

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
