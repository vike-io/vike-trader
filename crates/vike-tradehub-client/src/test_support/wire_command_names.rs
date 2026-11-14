//! The variant NAME of every [`CommandWire`], for the two capability-projection gates that classify
//! each one: `crates/vike-tradehub/tests/daemon/capability_projection_gate.rs` (core -> wire) and
//! `crates/vike-cli/tests/cli_verbs/capability_projection_gate.rs` (wire -> CLI verb / MCP tool).
//! Both used to carry a copy; the exhaustive match lives beside the type it matches instead.

use crate::wire::{CommandWire, OrderBracketWire, OrderComboWire, OrderListWire, OrderRequestWire};
use vike_model::TradingState;

/// The variant name every arm reports itself as. Every arm is named, deliberately with NO wildcard:
/// a NEW `CommandWire` variant fails to compile here until its author names it, and then each gate
/// refuses it until the author classifies it on that gate's tables.
pub fn wire_command_name(cmd: &CommandWire) -> &'static str {
    match cmd {
        CommandWire::Submit(_) => "CommandWire::Submit",
        CommandWire::Bracket(_) => "CommandWire::Bracket",
        CommandWire::OrderList(_) => "CommandWire::OrderList",
        CommandWire::Combo(_) => "CommandWire::Combo",
        CommandWire::Cancel(_) => "CommandWire::Cancel",
        CommandWire::Modify { .. } => "CommandWire::Modify",
        CommandWire::MassCancel { .. } => "CommandWire::MassCancel",
        CommandWire::Flatten { .. } => "CommandWire::Flatten",
        CommandWire::MarketExit { .. } => "CommandWire::MarketExit",
        CommandWire::SetTradingState(_) => "CommandWire::SetTradingState",
        CommandWire::UpdateParams { .. } => "CommandWire::UpdateParams",
        CommandWire::MountStrategy { .. } => "CommandWire::MountStrategy",
        CommandWire::UnmountStrategy { .. } => "CommandWire::UnmountStrategy",
        CommandWire::SetSetting { .. } => "CommandWire::SetSetting",
        CommandWire::ReconcileConfirm { .. } => "CommandWire::ReconcileConfirm",
        CommandWire::ReconcileCancel { .. } => "CommandWire::ReconcileCancel",
        CommandWire::ResumeVenue { .. } => "CommandWire::ResumeVenue",
        CommandWire::SetAccountTradingState { .. } => "CommandWire::SetAccountTradingState",
        CommandWire::UnadmitSymbol { .. } => "CommandWire::UnadmitSymbol",
        CommandWire::UnlatchMount { .. } => "CommandWire::UnlatchMount",
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
    let order_request = OrderRequestWire {
        order_client_id: String::new(),
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
        display_quantity: None,
        release: None,
        trail: None,
        limit_offset: None,
        peg: None,
        attributes: None,
        trail_percent: None,
        adjust: None,
    };

    vec![
        wire_command_name(&CommandWire::Submit(order_request)),
        wire_command_name(&CommandWire::Bracket(OrderBracketWire {
            order_client_id: None,
            venue: String::new(),
            symbol: String::new(),
            side: 1,
            qty: 0.0,
            entry_price: None,
            stop_loss: 0.0,
            take_profit: 0.0,
            account: None,
        })),
        wire_command_name(&CommandWire::OrderList(OrderListWire {
            venue: String::new(),
            symbol: String::new(),
            legs: Vec::new(),
            account: None,
        })),
        wire_command_name(&CommandWire::Combo(OrderComboWire {
            venue: String::new(),
            side: 1,
            qty: 0.0,
            legs: Vec::new(),
            net_limit: None,
            account: None,
            time_in_force: None,
            order_client_id: None,
        })),
        wire_command_name(&CommandWire::Cancel(String::new())),
        wire_command_name(&CommandWire::Modify {
            order_client_id: String::new(),
            new_qty: None,
            new_price: None,
            new_trigger_price: None,
        }),
        wire_command_name(&CommandWire::MassCancel { venue: None, symbol: None, account: None }),
        wire_command_name(&CommandWire::Flatten {
            venue: String::new(),
            symbol: String::new(),
            account: None,
        }),
        wire_command_name(&CommandWire::MarketExit { venue: None, account: None }),
        wire_command_name(&CommandWire::SetTradingState(TradingState::Active)),
        wire_command_name(&CommandWire::UpdateParams {
            venue: String::new(),
            symbol: String::new(),
            interval: String::new(),
            mount_id: None,
            params: serde_json::Value::Null,
        }),
        wire_command_name(&CommandWire::MountStrategy {
            venue: String::new(),
            account: None,
            symbol: String::new(),
            interval: String::new(),
            controller_id: None,
            file: None,
            name: None,
            rhai: None,
            params: serde_json::Value::Null,
        }),
        wire_command_name(&CommandWire::UnmountStrategy { controller_id: String::new() }),
        wire_command_name(&CommandWire::SetSetting {
            file: String::new(),
            key: String::new(),
            value: String::new(),
            confirm: None,
        }),
        wire_command_name(&CommandWire::ReconcileConfirm { id: 0 }),
        wire_command_name(&CommandWire::ReconcileCancel { id: 0 }),
        wire_command_name(&CommandWire::ResumeVenue { venue: String::new(), force: false }),
        wire_command_name(&CommandWire::SetAccountTradingState {
            venue: String::new(),
            account: None,
            state: TradingState::Active,
        }),
        wire_command_name(&CommandWire::UnadmitSymbol {
            venue: String::new(),
            account: None,
            symbol: String::new(),
        }),
        wire_command_name(&CommandWire::UnlatchMount { mount_id: String::new() }),
    ]
}
