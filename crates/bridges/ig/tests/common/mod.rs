// Shared by several of this crate's integration tests, which cargo compiles as INDEPENDENT test
// binaries. Each uses a subset of these items, so per-binary dead-code analysis flags the rest —
// expected, the same rationale `crates/bridges/ctrader/tests/common/mod.rs` carries.
#![allow(dead_code)]
//! The demo config gate and the default epic the five live IG smokes/probes share (setup only).

use vike_ig::load_ig_config_from;
use vike_model::AccountTier;
use vike_model::accounts::account_keys::AccountLabel;

/// EUR/USD mini — present on the demo gateway.
pub const EPIC: &str = "CS.D.EURUSD.MINI.IP";

/// Same config gate the other IG paths use: the credential store's default DEMO ig account; `None`
/// (-> self-skip) when it is absent or lacks a field. Never sweeps the process env — this repo's
/// credentials live only in the store.
pub fn cfg() -> Option<vike_ig::IgConfig> {
    let dir = vike_secrets::smoke_directory();
    let creds = dir.credentials_for("ig", &AccountLabel::Default, AccountTier::Demo);
    load_ig_config_from(AccountTier::Demo, &creds)
}
