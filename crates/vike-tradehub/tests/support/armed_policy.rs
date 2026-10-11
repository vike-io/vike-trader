//! The ARMED machine policy of the mount-wiring binaries `declared_symbol_grid_wiring.rs` and
//! `risk_profile_wiring.rs`, each of which `#[path]`-includes it (a bare `mod` would resolve beside
//! the binary root, not here).

use vike_model::accounts::account_keys::AccountLabel;

/// A machine policy whose `account` table holds ONE active `demo` row for `venue`'s DEFAULT
/// account, every other field at its default — so the credential map stays EMPTY and the mount is
/// still paper by the older absent-credentials gate. `venue` is each caller's own `VENUE` constant.
///
/// ⚠ Load-bearing rather than boilerplate. An account with no active non-paper row (the default,
/// and what `None` means) returns the paper engine from a SEPARATE assembly at the top of
/// `make_engine_with_legs`, above the venue arms entirely — so a scenario that left it at the
/// default would exercise that early return instead of the leg machinery or the merge site its
/// file exists to pin.
pub fn armed(venue: &str) -> vike_mount::MountPolicy {
    vike_mount::MountPolicy::default().with_account(
        venue,
        &AccountLabel::Default,
        vike_config::VenueMode::Demo,
    )
}
