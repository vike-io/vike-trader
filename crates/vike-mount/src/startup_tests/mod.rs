//! Unit tests of the real-probe wiring: the shared account-tier and identity-recorder fixtures.

use super::*;
use std::assert_matches;
use std::sync::atomic::{AtomicUsize, Ordering};

use vike_bridge_core::venue_mount::{
    ClockAuth, ClockDecl, ClockRisk, MountInputs, PaperCause, Resolution, Tier, VenueDeclaration,
};
use vike_bridge_core::venue_mount_fixture::{PLANTED_DECLARATION, PlantedMount};
use vike_config::VenueMode;
use vike_model::accounts::account_keys::AccountLabel;

/// **Every roster venue's DEFAULT account at `live`** — one active `account` row each.
///
/// ⚠ `None` instead would make most tests below VACUOUS, not red: `None` reads all-`paper`, and
/// the credential gate and the tier gate produce the same empty map, so "absent credentials mean
/// no probe" would pass where the TIER suppressed them. CREDENTIAL tests arm every account; the
/// tier gets its own tests. ⚠ A planted row resolving DEMO is PAPER at `live` (the no-downgrade
/// rule): such rows are driven at [`all_venues_at`] `Demo`.
fn all_live() -> crate::MountPolicy {
    all_venues_at(VenueMode::Live)
}

/// Every roster venue's DEFAULT account at one tier — the knob the tests below sweep.
fn all_venues_at(tier: VenueMode) -> crate::MountPolicy {
    vike_model::VENUES.iter().fold(crate::MountPolicy::default(), |policy, venue| {
        policy.with_account(venue, &AccountLabel::Default, tier)
    })
}

/// [`all_venues_at`] with ONE venue's account at `paper` — the operator saying "not this one".
/// One row per venue: a second, `paper` row beside a `live` one would leave the account LIVE.
fn all_venues_at_except(tier: VenueMode, paper: &str) -> crate::MountPolicy {
    vike_model::VENUES.iter().fold(crate::MountPolicy::default(), |policy, venue| {
        let at = if *venue == paper { VenueMode::Paper } else { tier };
        policy.with_account(venue, &AccountLabel::Default, at)
    })
}

/// A record an identity recorder was handed: `(venue, account, tier, a client came with it)`.
type Recorded = (String, vike_model::accounts::account_keys::AccountLabel, VenueMode, bool);

/// Every record [`capture`] was handed, by every test that passes it.
static RECORDED: Mutex<Vec<Recorded>> = Mutex::new(Vec::new());

/// An [`IdentityRecorder`] that captures what it is handed instead of asking the venue.
fn capture(
    venue: &str,
    label: &vike_model::accounts::account_keys::AccountLabel,
    tier: VenueMode,
    recon: Option<&dyn ReconClient>,
    _directory: &vike_bridge_core::account_directory::AccountDirectory,
) {
    RECORDED.lock().expect("the capture").push((
        venue.to_string(),
        label.clone(),
        tier,
        recon.is_some(),
    ));
}

/// What [`capture`] recorded for `venue` — each test records under its own venue name.
fn recorded_for(venue: &str) -> Vec<Recorded> {
    RECORDED.lock().expect("the capture").iter().filter(|r| r.0 == venue).cloned().collect()
}

/// A reconcile client whose balance read answers `balance`, counting its reads.
struct BalanceOnly {
    balance: Result<Option<f64>, &'static str>,
    reads: &'static AtomicUsize,
}

impl ReconClient for BalanceOnly {
    crate::testutil::empty_report_fetches!();
    fn fetch_balance(&self) -> Result<Option<f64>, String> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        self.balance.map_err(str::to_string)
    }
}

/// Every planted probe row below resolves armed at a FIXED tier whatever its inputs say, declares
/// [`PLANTED_DECLARATION`] (no named account, no book, no clock) and offers the probe its
/// `credential_probe` builds. A `DEMO` row arms a `demo` account only (the no-downgrade rule).
const ARMED_DEMO: Resolution = Resolution::Armed { tier: Tier::Demo, held_below_live: None };

/// [`ARMED_DEMO`]'s live twin: arms a `live` account.
const ARMED_LIVE: Resolution = Resolution::Armed { tier: Tier::Live, held_below_live: None };

#[cfg(test)]
mod account_tiers;
#[cfg(test)]
mod bounds_and_clock;
#[cfg(test)]
mod identity_probe;
