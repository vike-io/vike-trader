//! **A venue's instrument list as a running account's login HANDED it over** — the PUSH half of a
//! held-session catalog (`crate::CatalogAvailability::HeldSession`;
//! `docs/decisions/0062-a-venue-catalog-fetch-is-an-observe-verb-and-not-a-write.md`'s decision 3, amended
//! 2026-11-13).
//!
//! Two shapes of held session list a venue, and both answer the datahub the same way:
//!
//! * **READ on demand** from the session (fxcm's Offers table): the bridge's own provider reads the session
//!   when asked and answers [`crate::CatalogProvider::no_session`] while none is open. No slot here.
//! * **PUSHED once per login** (a login that receives the whole list, as dukascopy's sidecar does): the bridge
//!   calls [`hold`] with it, and [`CatalogProviderHeldList`] answers from the slot, `no_session` until a login
//!   has pushed one. Listing then costs the venue NOTHING: the list is what the login already received.
//!   oanda's and ctrader's are pushed the same way by the gatewayhub, from the whole list a running account's
//!   symbol facts read for that account (`crates/vike-gatewayhub/src/trade/adapters/bridge_trade.rs`'s
//!   `hold_listing`).
//!
//! Either way the datahub pulls through the gatewayhub on its weekly tick or a refresh, and a `no_session` is a
//! routine refusal it folds as no change. A venue whose kept list never succeeded is due on every hourly tick,
//! so a first login's list is kept within the hour; a later login's change is kept within the week (or at once
//! by a refresh).
//!
//! The slot is process-wide and keyed by venue: it lives where the sessions do (the gatewayhub), and the last
//! login's list wins (a venue's list is the same for its accounts; where it is not, the last login's is kept).

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock, PoisonError};

use vike_model::{AssetClass, Instrument};

use crate::{CatalogError, CatalogMode, CatalogProvider};

fn slots() -> &'static Mutex<HashMap<String, Vec<Instrument>>> {
    static SLOTS: OnceLock<Mutex<HashMap<String, Vec<Instrument>>>> = OnceLock::new();
    SLOTS.get_or_init(Mutex::default)
}

/// **Hold `instruments` as `venue`'s list**, as a login just received it, replacing the last login's.
pub fn hold(venue: &str, instruments: Vec<Instrument>) {
    slots().lock().unwrap_or_else(PoisonError::into_inner).insert(venue.to_string(), instruments);
}

/// `venue`'s list as the last login pushed it, or `None` when no login has pushed one in this process.
#[must_use]
pub fn held(venue: &str) -> Option<Vec<Instrument>> {
    slots().lock().unwrap_or_else(PoisonError::into_inner).get(venue).cloned()
}

/// The provider of a venue whose list a login PUSHES ([`hold`]): enumerable, no request, and
/// [`CatalogProvider::no_session`] until a list is held.
#[derive(Clone, Copy, Debug)]
pub struct CatalogProviderHeldList {
    /// The venue the slot is keyed by.
    pub venue: &'static str,
    /// The classes the venue lists.
    pub classes: &'static [AssetClass],
}

impl CatalogProviderHeldList {
    fn not_held(&self) -> String {
        format!(
            "no running `{}` account has handed this gatewayhub its list since it started, so it holds none \
             (a login, or the account's own instrument read, hands it over; nothing is asked of the venue for it)",
            self.venue
        )
    }
}

impl CatalogProvider for CatalogProviderHeldList {
    fn venue(&self) -> &str {
        self.venue
    }
    fn asset_classes(&self) -> &[AssetClass] {
        self.classes
    }
    fn mode(&self) -> CatalogMode {
        CatalogMode::Enumerable
    }
    fn no_session(&self) -> Option<String> {
        held(self.venue).is_none().then(|| self.not_held())
    }
    fn list_instruments(&self) -> Result<Vec<Instrument>, CatalogError> {
        held(self.venue).ok_or_else(|| CatalogError(self.not_held()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vike_model::Contract;

    /// Nothing held is the routine refusal; a pushed list answers whole, and the next login's replaces it.
    /// The venue keys are this test's own: the slot is process-wide.
    #[test]
    fn a_pushed_list_answers_and_the_next_login_replaces_it() {
        let provider =
            CatalogProviderHeldList { venue: "held-list-test", classes: &[AssetClass::Fx] };
        assert!(provider.no_session().is_some_and(|why| why.contains("`held-list-test`")));
        assert!(provider.list_instruments().is_err(), "nothing held is never an empty list");

        let row =
            |s: &str| Instrument::new("held-list-test", s, "EUR", "USD", Contract::CryptoSpot);
        hold("held-list-test", vec![row("EUR/USD"), row("GBP/USD")]);
        assert_eq!(provider.no_session(), None);
        assert_eq!(provider.list_instruments().expect("held").len(), 2);

        hold("held-list-test", vec![row("USD/JPY")]);
        assert_eq!(provider.list_instruments().expect("held"), vec![row("USD/JPY")]);
        assert!(held("held-list-other").is_none(), "keyed by venue");
    }
}
