//! The variant NAME of every [`WireCommand`], for the two capability-projection gates that classify
//! each one: `crates/vike-tradehub/tests/daemon/capability_projection_gate.rs` (core -> wire) and
//! `crates/vike-cli/tests/cli_verbs/capability_projection_gate.rs` (wire -> CLI verb / MCP tool).
//! Both used to carry a copy; the exhaustive match lives beside the type it matches instead.

use crate::wire::{WireBracketSpec, WireCommand, WireOrderRequest, WireTradingState};

/// The variant name every arm reports itself as. Every arm is named, deliberately with NO wildcard:
/// a NEW `WireCommand` variant fails to compile here until its author names it, and then each gate
/// refuses it until the author classifies it on that gate's tables.
pub fn wire_command_name(cmd: &WireCommand) -> &'static str {
    match cmd {
        WireCommand::Submit(_) => "WireCommand::Submit",
        WireCommand::Bracket(_) => "WireCommand::Bracket",
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
        WireCommand::ReconConfirm { .. } => "WireCommand::ReconConfirm",
        WireCommand::ReconCancel { .. } => "WireCommand::ReconCancel",
        WireCommand::ResumeVenue { .. } => "WireCommand::ResumeVenue",
        WireCommand::SetAccountTradingState { .. } => "WireCommand::SetAccountTradingState",
        WireCommand::UnadmitSymbol { .. } => "WireCommand::UnadmitSymbol",
        WireCommand::UnlatchMount { .. } => "WireCommand::UnlatchMount",
    }
}

/// Every variant name, produced by constructing one of each and asking [`wire_command_name`]. Field
/// values are placeholders; only the discriminant is ever inspected.
///
/// ⚠ This Vec IS hand-maintained: the exhaustive match above is what a new variant breaks, and this
/// function only makes the already-listed arms RUN. The core-side gate's
/// `every_on_the_wire_row_names_a_real_wire_variant` notices a mapping row whose claimed variant this
/// function no longer produces.
pub fn every_wire_command_variant_name() -> Vec<&'static str> {
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
        time_in_force: None,
        gtd_expiry: None,
    };

    vec![
        wire_command_name(&WireCommand::Submit(order_request)),
        wire_command_name(&WireCommand::Bracket(WireBracketSpec {
            venue: String::new(),
            symbol: String::new(),
            side: 1,
            qty: 0.0,
            entry_price: None,
            stop_loss: 0.0,
            take_profit: 0.0,
            account: None,
        })),
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
            mount_id: None,
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
        wire_command_name(&WireCommand::ReconConfirm { id: 0 }),
        wire_command_name(&WireCommand::ReconCancel { id: 0 }),
        wire_command_name(&WireCommand::ResumeVenue { venue: String::new(), force: false }),
        wire_command_name(&WireCommand::SetAccountTradingState {
            venue: String::new(),
            account: String::new(),
            state: WireTradingState::Active,
        }),
        wire_command_name(&WireCommand::UnadmitSymbol {
            venue: String::new(),
            account: String::new(),
            symbol: String::new(),
        }),
        wire_command_name(&WireCommand::UnlatchMount { mount_id: String::new() }),
    ]
}
