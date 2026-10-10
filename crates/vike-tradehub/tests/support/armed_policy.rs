//! The ARMED machine policy of the mount-wiring binaries `declared_symbol_grid_wiring.rs` and
//! `risk_profile_wiring.rs`, each of which `#[path]`-includes it (a bare `mod` would resolve beside
//! the binary root, not here).

/// A machine policy whose ARMING CEILING permits `venue` at `Demo`, every other field at its
/// default — so the credential map stays EMPTY and the mount is still paper by the older
/// absent-credentials gate. `venue` is each caller's own `VENUE` constant.
///
/// ⚠ Load-bearing rather than boilerplate. A `paper` ceiling (the default, and what `None` means)
/// returns the paper engine from a SEPARATE assembly at the top of `make_engine_with_legs`, above
/// the venue arms entirely — so a scenario that left it at the default would exercise that early
/// return instead of the leg machinery or the merge site its file exists to pin.
pub fn armed(venue: &str) -> vike_mount::MountPolicy {
    vike_mount::MountPolicy {
        venues: vike_config::VenuePolicy::default().declare(venue, vike_config::VenueMode::Demo),
        ..vike_mount::MountPolicy::default()
    }
}
