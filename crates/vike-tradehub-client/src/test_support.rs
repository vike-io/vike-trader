//! The negotiation suites' scripted node and the strategy-plane script over it, compiled ONCE here
//! behind the `test-support` feature instead of once per suite that used to `#[path]`-include
//! `tests/support/`, plus the [`WireCommand`](crate::wire::WireCommand) variant names the two
//! capability-projection gates (vike-tradehub's and vike-cli's) both classify, and the wire commands
//! (`commands`) this crate's and the tradehub's suites both send. This crate's own suites name them as
//! `vike_tradehub_client::test_support::{commands, fake_node, strategy_node}`; the two gates reach
//! `wire_command_names` through their dev edges. [`canned_balance`] is the Polymarket balance read the
//! venue-read suites (this crate's negotiation suite and vike-cli's `trade venue` renderer) answer with.

pub mod commands;
pub mod fake_node;
pub mod strategy_node;
pub mod wire_command_names;

use crate::wire::{WireVenueAllowance, WireVenueBalance, WireVenueRead};

/// A Polymarket balance read: 20 pUSD, one unlimited allowance to the CTF exchange at `spender` (a
/// renderer test passes a short one it can read back, a wire test the real contract address).
pub fn canned_balance(spender: &str) -> WireVenueRead {
    WireVenueRead::Balance(WireVenueBalance {
        asset: "pUSD".into(),
        balance: 20.0,
        allowances: vec![WireVenueAllowance {
            spender: spender.into(),
            name: "CTF exchange".into(),
            amount: 0.0,
            unlimited: true,
        }],
    })
}
