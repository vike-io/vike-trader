//! **A venue's instrument list as a running account's login HANDED it over** — the PUSH half of a
//! held-session catalog (`crate::CatalogAvailability::HeldSession`;
//! `docs/decisions/0062-a-venue-catalog-fetch-is-an-observe-verb-and-not-a-write.md`'s decision 3, amended
//! 2026-11-13).
//!
//! Three shapes of held session list a venue, and all answer the datahub the same way:
//!
//! * **READ on demand** from the session (fxcm's Offers table, ig's categories): the bridge's own provider reads
//!   the session when asked and answers [`crate::CatalogProvider::no_session`] while none is open. No slot here.
//! * **PUSHED once per login** (a login that receives the whole list, as dukascopy's sidecar does): the bridge
//!   calls [`hold`] with it, and [`CatalogProviderHeldList`] answers from the slot, `no_session` until a login
//!   has pushed one. Listing then costs the venue NOTHING: the list is what the login already received.
//!   ctrader's login pushes it too (every live handshake reads the whole universe:
//!   `crates/bridges/ctrader/src/catalog.rs`'s `hold`), and so do the account-scoped symbol facts of oanda and
//!   ctrader (`crates/vike-gatewayhub/src/trade/adapters/bridge_trade.rs`'s `hold_listing`).
//! * **A READER kept from a running account's start** ([`hold_reader`], oanda: its start reads no list): the
//!   gatewayhub reads once at the start and again on each refresh ([`read`]), with that account's own keys and only
//!   while it still runs (`hold_oanda_listing` in the same file). A read's rows are held as a push's are; a
//!   refused one is recorded ([`refused`]) until a read succeeds.
//!
//! The datahub pulls through the gatewayhub on the operator's refresh (no timer: the owner's ruling of
//! 2026-11-16), and a `no_session` is a routine refusal it folds as no change. A login's list is held here in
//! memory, and nothing writes it to the kept file until that refresh.
//!
//! The slot is process-wide and keyed by venue: it lives where the sessions do (the gatewayhub), and the last
//! login's list wins (a venue's list is the same for its accounts; where it is not, the last login's is kept).

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use vike_model::{AssetClass, Instrument};
use vike_utility::errors::CarriedRefusal;

use crate::refusals::SessionListRefusal;
use crate::{CatalogError, CatalogMode, CatalogProvider};

/// **One venue's way to read its whole list NOW over the session a running account holds** (oanda's: one
/// `GET /v3/accounts/{id}/instruments` with that account's own keys): `Err` is the refusal in words (the account no
/// longer runs, or the venue refused the read).
pub type HeldReader = Arc<dyn Fn() -> Result<Vec<Instrument>, String> + Send + Sync>;

fn slots() -> &'static Mutex<HashMap<String, Vec<Instrument>>> {
    static SLOTS: OnceLock<Mutex<HashMap<String, Vec<Instrument>>>> = OnceLock::new();
    SLOTS.get_or_init(Mutex::default)
}

fn readers() -> &'static Mutex<HashMap<String, HeldReader>> {
    static READERS: OnceLock<Mutex<HashMap<String, HeldReader>>> = OnceLock::new();
    READERS.get_or_init(Mutex::default)
}

/// **A held list's last read that was refused** (oanda's reader, ig's list walk): the venue, the refusal in words and
/// when it was recorded (epoch-ms). The gatewayhub's status report carries every one ([`refusals`]), so
/// `vike-cli trade health` names it; it is forgotten when a read of that venue succeeds.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct HeldListRefusal {
    /// The venue.
    pub venue: String,
    /// The refusal, in the reader's own words.
    pub why: String,
    /// When it was recorded, epoch-ms.
    pub at_ms: i64,
}

fn refusal_slots() -> &'static Mutex<HashMap<String, HeldListRefusal>> {
    static REFUSALS: OnceLock<Mutex<HashMap<String, HeldListRefusal>>> = OnceLock::new();
    REFUSALS.get_or_init(Mutex::default)
}

/// **Record `venue`'s read**: `Some(why)` keeps the refusal with the time now, `None` (a read that succeeded) forgets it.
pub fn note_read(venue: &str, refused: Option<&str>) {
    let mut slots = refusal_slots().lock().unwrap_or_else(PoisonError::into_inner);
    match refused {
        Some(why) => {
            let at_ms = vike_model::now_ms();
            let row = HeldListRefusal { venue: venue.to_string(), why: why.to_string(), at_ms };
            slots.insert(venue.to_string(), row);
        }
        None => {
            slots.remove(venue);
        }
    }
}

/// Every venue whose last list read was refused, by venue name.
#[must_use]
pub fn refusals() -> Vec<HeldListRefusal> {
    let slots = refusal_slots().lock().unwrap_or_else(PoisonError::into_inner);
    let mut rows: Vec<HeldListRefusal> = slots.values().cloned().collect();
    rows.sort_by(|a, b| a.venue.cmp(&b.venue));
    rows
}

/// **Keep `read` as `venue`'s reader**, replacing the last account's (the last account started wins). Nothing is
/// read here: [`read`] reads.
pub fn hold_reader(venue: &str, read: HeldReader) {
    readers().lock().unwrap_or_else(PoisonError::into_inner).insert(venue.to_string(), read);
}

/// **Read `venue`'s list now through its kept reader**: the rows are held ([`hold`]) and the last refusal
/// forgotten, or the refusal is recorded ([`refused`]). An empty answer is a refusal: it would answer the datahub as
/// a venue that lists nothing. `None`: no reader is kept for `venue`.
pub fn read(venue: &str) -> Option<Result<Vec<Instrument>, String>> {
    let reader = readers().lock().unwrap_or_else(PoisonError::into_inner).get(venue).cloned()?;
    let answer = reader().and_then(|rows| {
        if rows.is_empty() {
            Err(SessionListRefusal::Empty { venue: venue.to_string() }.to_string())
        } else {
            Ok(rows)
        }
    });
    if let Ok(rows) = &answer {
        hold(venue, rows.clone());
    }
    note_read(venue, answer.as_ref().err().map(String::as_str));
    Some(answer)
}

/// Why `venue`'s last read was refused, until a read succeeds; `None` when the last one did, or none was made.
#[must_use]
pub fn refused(venue: &str) -> Option<String> {
    refusal_slots().lock().unwrap_or_else(PoisonError::into_inner).get(venue).map(|r| r.why.clone())
}

fn has_reader(venue: &str) -> bool {
    readers().lock().unwrap_or_else(PoisonError::into_inner).contains_key(venue)
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
    fn not_held(&self) -> CarriedRefusal {
        SessionListRefusal::NotHeld { venue: self.venue.to_string() }.carried()
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
    fn no_session(&self) -> Option<CarriedRefusal> {
        (!has_reader(self.venue) && held(self.venue).is_none()).then(|| self.not_held())
    }
    /// A kept reader reads NOW (the operator's refresh is a new read); otherwise the list a login pushed.
    fn list_instruments(&self) -> Result<Vec<Instrument>, CatalogError> {
        match read(self.venue) {
            Some(answer) => answer.map_err(|why| CatalogError(why.into())),
            None => held(self.venue).ok_or_else(|| CatalogError(self.not_held())),
        }
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
        assert!(provider.no_session().is_some_and(|why| why.text().contains("`held-list-test`")));
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

    /// **A kept reader reads on EVERY list** (the operator's refresh is a new read, no timer asks it): a refusal is
    /// recorded in its words and holds nothing, an empty answer is a refusal, and the next good read holds its rows
    /// and forgets the refusal. The venue key is this test's own: the slots are process-wide.
    #[test]
    fn a_kept_reader_reads_on_each_list_and_records_a_refusal_until_a_read_succeeds() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        const VENUE: &str = "held-reader-test";
        let provider = CatalogProviderHeldList { venue: VENUE, classes: &[AssetClass::Fx] };
        assert_eq!(read(VENUE), None, "no reader kept: nothing read");
        assert!(provider.no_session().is_some(), "no reader and no list: no session");

        let row = |s: &str| Instrument::new(VENUE, s, "EUR", "USD", Contract::CryptoSpot);
        let calls = Arc::new(AtomicUsize::new(0));
        let seen = Arc::clone(&calls);
        hold_reader(
            VENUE,
            Arc::new(move || match seen.fetch_add(1, Ordering::SeqCst) {
                0 => Err("401: refused around the start".to_string()),
                1 => Ok(Vec::new()),
                _ => Ok(vec![row("EUR_USD")]),
            }),
        );
        assert_eq!(provider.no_session(), None, "a kept reader is a session");
        assert_eq!(calls.load(Ordering::SeqCst), 0, "keeping a reader reads nothing");

        let refused_once = provider.list_instruments().expect_err("the first read is refused");
        assert!(refused_once.0.text().contains("401"), "{}", refused_once.0);
        assert_eq!(refused(VENUE).as_deref(), Some("401: refused around the start"));
        assert!(held(VENUE).is_none(), "a refusal holds nothing");
        let reported = refusals().into_iter().find(|r| r.venue == VENUE).expect("in the report");
        assert!(reported.why.contains("401") && reported.at_ms > 0, "{reported:?}");

        let empty = provider.list_instruments().expect_err("an empty answer is no list");
        assert!(
            empty.0.text().contains("empty") && refused(VENUE).is_some_and(|w| w.contains("empty"))
        );

        assert_eq!(provider.list_instruments().expect("read again"), vec![row("EUR_USD")]);
        assert_eq!((held(VENUE), refused(VENUE)), (Some(vec![row("EUR_USD")]), None));
        assert!(refusals().iter().all(|r| r.venue != VENUE), "a good read leaves the report");
        assert_eq!(calls.load(Ordering::SeqCst), 3, "one read per list, none in between");
    }
}
