//! `names` — **an FXCM instrument's WIRE name from its USER name, and back, read off its row** (decision 0290).
//!
//! The engine, the journal, the fills, the reconcile reports, the price lane's keys and every door speak the USER name
//! (`vike_catalog::user_name_of_row`'s grammar: `EURUSD`, `XAUUSD`, `USOIL.CFD`, `NVDA.EXT.CFD`, `BTCUSD.CFD`). FXCM's
//! own spelling, the Offers table's `Instrument` (`EUR/USD`, `XAU/USD`, `USOil`, `NVDA.ext`, `BTC/USD`), lives only
//! inside this bridge: it is the row's `raw_symbol` (`vike_catalog::wire_name_of_row`), what an order, an amend, a
//! subscription and a base-unit read send, and what a fill, a trade, an order row and a rollover come back under. The
//! case is the row's: the SDK's name-keyed calls answer 0 or "unknown" for any other.
//!
//! ONE source, the KEPT row (`vike_catalog::kept_list`, the datahub's `<project>/instruments/fxcm.json`, which is this
//! bridge's catalog of the Offers table), whose user name the list's writer spelled ONCE and stored (the owner's
//! ruling «Б», 2026-11-16): nothing in this bridge spells a name.
//! * `wire_name` (an order, an amend): the kept row of the user name. No list, no row, or two rows is a refusal in the
//!   translator's words (`vike_catalog::no_kept_list_refusal`'s fix included): nothing is guessed.
//! * `user_name_of_offer` (the facts, the price lane, a mounted CFD's multiplier, a re-taught entry's offer id): the
//!   name stored on the kept row whose wire name is the Offers row's `Instrument`. fxcm is a held-session venue: the
//!   account's own session is what WRITES that list, so its start and its listing never wait for one; until the list
//!   is written a read that needs a name answers none, and the path that asked refuses in words.
//!
//! The way back, a venue name to the user name a fill, a funding row, a reconcile report and an operator's read carry,
//! is `user_name`: one `wire_name` already answered in this process (an order's fill maps back with no list read),
//! else the kept row whose wire name it is. A name no kept row names is booked under the venue's own spelling with a
//! WARN (`user_name_or_wire`): never dropped, never guessed.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use vike_model::{Instrument, InstrumentNames};

use crate::account::quote_rate::Offer;
use crate::reconcile::VENUE;

/// Every `wire -> user` pair [`wire_name`] and [`user_name`] answered in this process.
fn answered() -> &'static Mutex<HashMap<String, String>> {
    static ANSWERED: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();
    ANSWERED.get_or_init(Mutex::default)
}

/// Remember that `wire` is `user`.
fn remember(wire: &str, user: &str) {
    answered()
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .insert(wire.to_string(), user.to_string());
}

/// This process's kept fxcm rows (`vike_catalog::kept_list`). This crate's unit tests read the committed fixtures'
/// (`kept_list_for_tests`); a test of no list asks the pure halves with `&[]`.
fn kept() -> Option<Arc<Vec<Instrument>>> {
    #[cfg(test)]
    kept_list_for_tests();
    vike_catalog::kept_list(VENUE)
}

/// **The three names of the USER name `user` among `rows`** (the venue's kept list), the user name canonical: the pure
/// half of [`wire_name`].
///
/// # Errors
/// `rows` holds no fxcm row (no list is kept), none whose user name is `user`, or two: in the translator's words.
pub(crate) fn names_among(rows: &[Instrument], user: &str) -> Result<InstrumentNames, String> {
    vike_catalog::names_of_user_name(rows, VENUE, user).map_err(|why| format!("fxcm: {why}"))
}

/// **What an order on the USER name `user` sends**: its KEPT row's wire name (`EURUSD` is `EUR/USD`, `USOIL.CFD` is
/// `USOil`, the row's case kept).
///
/// # Errors
/// [`names_among`]'s over this process's kept list.
pub(crate) fn wire_name(user: &str) -> Result<String, String> {
    let rows = kept();
    let names = names_among(rows.as_deref().map_or(&[][..], Vec::as_slice), user)?;
    remember(&names.wire, &names.user);
    Ok(names.wire)
}

/// **The user name of the venue's `wire` name among `rows`**: the row whose wire name it is, `None` when no row is
/// (or it is of no class the grammar names). The pure half of [`user_name`].
pub(crate) fn user_name_among(rows: &[Instrument], wire: &str) -> Option<String> {
    rows.iter()
        .filter(|row| row.venue == VENUE)
        .find(|row| vike_catalog::wire_name_of_row(row) == wire)
        .and_then(|row| vike_catalog::user_name_of_row(row).ok())
}

/// **The USER name of the venue's `wire` name**: one [`wire_name`] answered in this process, else its kept row's.
/// `None` when no kept row names it (no list is held, or the venue listed it after the list was refreshed): the caller
/// says what it does then.
pub(crate) fn user_name(wire: &str) -> Option<String> {
    if let Some(user) = answered().lock().unwrap_or_else(PoisonError::into_inner).get(wire) {
        return Some(user.clone());
    }
    let user = user_name_among(&kept()?, wire)?;
    remember(wire, &user);
    Some(user)
}

/// **The name a venue row is booked under** (a fill, a rollover): [`user_name`] of `wire`, else the venue's own `wire`
/// name, said at WARN: no order of this process names it ([`wire_name`] remembered every one), so it is a hand trade on
/// something the kept list does not carry yet.
pub(crate) fn user_name_or_wire(wire: &str) -> String {
    user_name(wire).unwrap_or_else(|| {
        tracing::warn!(
            venue = VENUE,
            wire,
            "a venue row names an instrument no kept row names (the datahub's <project>/instruments/fxcm.json); it is \
             booked under the venue's own name"
        );
        wire.to_string()
    })
}

/// **The user name of an Offers row**: the name STORED on the kept row whose wire name is the row's `Instrument`
/// ([`user_name`]: the owner's ruling «Б» of 2026-11-16, a name is spelled once, by the kept list's writer, and read
/// here). `None` while no kept list carries the row (a start before the datahub wrote this session's list, an
/// instrument the venue listed after its refresh): the caller says what it does then, and nothing here waits for the
/// list or spells a name.
pub(crate) fn user_name_of_offer(offer: &Offer) -> Option<String> {
    // The live row must still be one the catalog lists (a class the SDK names, a determinable quote currency): a row
    // the bridge would refuse anyway has no name, whatever an older list stored for its spelling. A check, no spelling.
    crate::catalog::instrument_of(offer)?;
    user_name(&offer.instrument)
}

/// **Hold fxcm's kept list in this process as the catalog makes it of the two MEASURED Offers tables** (the EUR demo's
/// 48 forex rows and its 449 rows of every other class: `EUR/USD`, `XAU/USD`, `USOil`, `NVDA.ext`, `BTC/USD`, ...),
/// what a test of a path that reads a name holds. Every holder in one test process holds these same rows; a second call
/// changes nothing.
#[cfg(any(test, feature = "test-support"))]
pub fn kept_list_for_tests() {
    static HELD: std::sync::Once = std::sync::Once::new();
    HELD.call_once(|| {
        let offers: Vec<Offer> = [
            include_str!("../../tests/fixtures/eur_demo_offers.json"),
            include_str!("../../tests/fixtures/offers_classes.json"),
        ]
        .iter()
        .flat_map(|table| {
            let fixture: serde_json::Value =
                serde_json::from_str(table).expect("the fixture is JSON");
            crate::account::quote_rate::parse_offers(&fixture["offers"].to_string())
                .expect("the shim's shape")
        })
        .collect();
        let mut rows = crate::catalog::instruments_of(&offers);
        // A cross the measured tables do not carry, which the conversion tests route through USD.
        rows.push(Instrument::new(
            VENUE,
            "CHF/HKD",
            "CHF",
            "HKD",
            vike_model::Contract::Fx(vike_model::ContractFx::default()),
        ));
        vike_catalog::hold_kept_list(VENUE, rows);
    });
}

#[cfg(test)]
#[path = "names_tests.rs"]
mod names_tests;
