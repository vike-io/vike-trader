//! **A venue's INSTRUMENT LIST as the OPERATOR fetched it, dated and qualified** ([`InstrumentList`]):
//! one venue's universe with the date it was fetched and the qualifier that makes it honest, kept as
//! rows of the settings database (`vike-secrets`' `operator_instrument_list`).
//! `docs/decisions/0066-the-venue-catalog-is-on-by-default-and-the-switch-is-its-refusal.md`,
//! decisions 5 through 9, and its 2026-11-13 amendment.
//!
//! # Only the operator's own lists
//!
//! A venue's list lives in ONE place, `<project>/instruments/<venue>.json`, kept by the datahub
//! (`crates/vike-datahub/src/instrument_lists.rs`). The second folder this type was also read from, the
//! SHIPPED lists under `<project>/bin/instruments/`, was deleted on the owner's ruling of 2026-11-13
//! ("remove all hardcoded symbols"); it never held a file. What stays is the operator's OWN credentialed
//! fetch, for the venues a datahub refuses to list (decision 3 of `docs/decisions/0062`).
//!
//! # What this is FOR, and what it very deliberately is not
//!
//! `docs/decisions/0062`'s decision 3 refuses alpaca (and, until its 2026-11-13 amendment listed them
//! from a session a running account holds, oanda and ctrader) from a datahub's provider table BY
//! CONSTRUCTION: listing them means authenticating as the OPERATOR, a rate limit bounds spending a
//! public budget and nothing bounds spending an identity. That verdict is untouched. An operator who
//! holds those credentials fetches the list on their own box (`vike-backend catalog refresh`), and it
//! is kept as their own rows.
//!
//! # ⚠ Why a list carries a QUALIFIER
//!
//! 0066's decision 6, measured against each bridge's own listing call:
//!
//! * **alpaca: COMMON.** `crates/bridges/alpaca/src/catalog.rs`'s `fetch_assets` issues one
//!   `/v1/assets?status=active` against the broker host and the account id is in NO part of it.
//!   One axis does vary and is what [`InstrumentList::qualifier`] carries: the ENVIRONMENT, because
//!   `CatalogProviderAlpaca::new` resolves the demo host and a sandbox universe is not guaranteed to
//!   equal a live one.
//! * **oanda: NOT common.** `crates/bridges/oanda/src/catalog.rs`'s `fetch_instruments` builds its
//!   path from `config.account_id`, and OANDA's published contract says the list depends on the
//!   regulatory DIVISION the account sits in, so a list names its DIVISION.
//! * **ctrader: NOT common.** `crates/bridges/ctrader/src/transport/connection/mod.rs`'s
//!   `fetch_symbols` is account-scoped by protocol field across a white-label platform whose brokers
//!   spell symbol ids differently. A list fetched with the operator's own credentials against their
//!   own account at their own broker is exactly the symbols they can trade, and names the tier it was
//!   fetched at.
//!
//! # The staleness rules
//!
//! 1. **A MISSING STAMP IS REFUSED.** A list with no date is not rendered at all, because it looks
//!    authoritative; the venue then reads as it does with no list.
//! 2. **It does NOT share [`crate::CatalogCache`]**: [`crate::CatalogCacheVenueStamp`] records WHEN
//!    and HOW MANY and never FROM WHERE, and the desktop rewrites that cache whole from memory, so a
//!    second writer of it would lose lists silently.
//! 3. **Where both could answer, the cache's own refresh WINS and the row says which answered.**
//!    [`merge_sources`] is that rule, keyed on the CACHE STAMP rather than on whether the cache holds
//!    rows: a stamp at zero is a MEASURED claim about a venue.

use crate::{CatalogAvailability, catalog_availability};
use serde::{Deserialize, Serialize};
use vike_model::Instrument;

/// Why a list was refused. One variant per rule, because an operator acts on them differently and
/// a single string would make them one fact.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InstrumentListError {
    /// A venue row carried no fetch date, or one that is not `YYYY-MM-DD`. Rule 1: a list with no
    /// stamp is not rendered at all.
    NoStamp { venue: String },
    /// A venue row carried no qualifier, or one this venue's rule does not admit. See the module
    /// doc's decision-6 summary for what each venue's qualifier must name.
    BadQualifier { venue: String, why: String },
    /// A venue row for a venue that needs no operator list: anything that is neither
    /// `CatalogAvailability::Credentialed` nor `HeldSession` (a publicly enumerable venue has a LIVE
    /// route, so an operator list for it would be a second source with nothing to gain), and anything
    /// off the roster. Also a venue that appears twice.
    VenueNotEligible { venue: String, why: String },
    /// An instrument whose own `venue` field disagrees with the row it sits in: the one way a
    /// row can put a venue's symbols under another venue's name.
    VenueMismatch { venue: String, found: String },
}

impl std::fmt::Display for InstrumentListError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoStamp { venue } => write!(
                f,
                "the `{venue}` instrument list carries no `fetched` date in YYYY-MM-DD form. A list \
                 that cannot say when it was fetched is worse than no list, because it looks \
                 authoritative, so the whole set is refused rather than rendered undated."
            ),
            Self::BadQualifier { venue, why } => {
                write!(f, "the `{venue}` instrument list carries no usable `qualifier`: {why}")
            }
            Self::VenueNotEligible { venue, why } => {
                write!(f, "there must be no `{venue}` instrument list here: {why}")
            }
            Self::VenueMismatch { venue, found } => write!(
                f,
                "the `{venue}` instrument list holds an instrument tagged `{found}`. A list's \
                 instruments must all name the list's own venue, or the picker offers one venue's \
                 symbols under another's name."
            ),
        }
    }
}

impl std::error::Error for InstrumentListError {}

/// One venue's list, with everything that makes it honest: one row of the operator's own lists.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct InstrumentList {
    /// The roster slug.
    pub venue: String,
    /// **WHEN these bytes were fetched, `YYYY-MM-DD`.** Required; a blank or malformed value
    /// refuses the whole set ([`InstrumentListError::NoStamp`]).
    ///
    /// ⚠ A DATE rather than a timestamp, deliberately: the thing a reader needs is "how old is
    /// this, roughly", and a millisecond field beside [`crate::CatalogCacheVenueStamp::last_refreshed_ms`]
    /// would invite exactly the confusion rule 2 exists to prevent.
    pub fetched: String,
    /// **The account-shaped fact that makes this list honest for THIS venue**, rendered to the
    /// operator beside the count. alpaca's ENVIRONMENT; oanda's regulatory DIVISION; ctrader's tier.
    /// See the module doc.
    pub qualifier: String,
    /// The list. May legitimately be empty: an empty row is a claim that the venue listed nothing
    /// on that date, which is why it still needs a stamp.
    pub instruments: Vec<Instrument>,
}

/// Several venues' lists of the operator's own, at most one per venue ([`InstrumentLists::check`]).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct InstrumentLists {
    pub venues: Vec<InstrumentList>,
}

/// alpaca's qualifier names the ENVIRONMENT, and this is the whole admissible set.
///
/// MEASURED from `crates/bridges/alpaca/src/config.rs`'s `urls_for`, which splits
/// `broker-api.sandbox.alpaca.markets` from `broker-api.alpaca.markets`: two universes that are
/// not guaranteed equal, and the only axis on which alpaca's list is not common.
const ALPACA_ENVIRONMENTS: &[&str] = &["demo", "live"];

impl InstrumentLists {
    /// **VALIDATE every list, whole or not at all.** Every rule the module doc states is enforced
    /// here, because a list that is merely parsed is a list whose rules live in whoever remembered
    /// them.
    pub fn check(&self) -> Result<(), InstrumentListError> {
        for row in &self.venues {
            eligible(&row.venue)?;
            if !is_iso_date(&row.fetched) {
                return Err(InstrumentListError::NoStamp { venue: row.venue.clone() });
            }
            check_qualifier(&row.venue, &row.qualifier)?;
            if let Some(bad) = row.instruments.iter().find(|i| i.venue != row.venue) {
                return Err(InstrumentListError::VenueMismatch {
                    venue: row.venue.clone(),
                    found: bad.venue.clone(),
                });
            }
        }
        if let Some(dup) = first_duplicate(&self.venues) {
            return Err(InstrumentListError::VenueNotEligible {
                venue: dup,
                why: "it appears twice. One venue has one list per document, or `merge_sources` \
                      would pick whichever row happened to come first."
                    .to_string(),
            });
        }
        Ok(())
    }

    /// One venue's row, or `None`.
    #[must_use]
    pub fn venue(&self, venue: &str) -> Option<&InstrumentList> {
        self.venues.iter().find(|v| v.venue == venue)
    }
}

/// Which venues an operator list may name: decision 6's ruling, held structurally.
fn eligible(venue: &str) -> Result<(), InstrumentListError> {
    match catalog_availability(venue) {
        // A held-session venue's live route exists only while one of its accounts runs, so a list
        // obtained out of band is no second source of an always-live one; and oanda and ctrader,
        // Credentialed until 2026-11-13, keep every list recorded for them readable.
        CatalogAvailability::Credentialed | CatalogAvailability::HeldSession => Ok(()),
        CatalogAvailability::PublicBulk => Err(InstrumentListError::VenueNotEligible {
            venue: venue.to_string(),
            why: "it is publicly enumerable, so a live route already exists for it (the datahub's \
                  kept list). An operator list would be a SECOND source for a venue that has a \
                  first one, which is the silent disagreement decision 7 refuses."
                .to_string(),
        }),
        CatalogAvailability::NoBulkList { .. } => Err(InstrumentListError::VenueNotEligible {
            venue: venue.to_string(),
            why: "the venue publishes no bulk instrument list at any price, so there is nothing \
                  to fetch."
                .to_string(),
        }),
        CatalogAvailability::UnknownVenue => Err(InstrumentListError::VenueNotEligible {
            venue: venue.to_string(),
            why: "it is not on the canonical `vike_model::VENUES` roster.".to_string(),
        }),
    }
}

/// The per-venue qualifier rule. Each arm carries its own evidence, and the oanda arm carries the
/// declared limit of it.
fn check_qualifier(venue: &str, qualifier: &str) -> Result<(), InstrumentListError> {
    let q = qualifier.trim();
    if q.is_empty() {
        return Err(InstrumentListError::BadQualifier {
            venue: venue.to_string(),
            why: "every row must declare the account-shaped fact that makes it honest, because \
                  the list is not common per venue for two of the three venues that can carry \
                  one: alpaca's ENVIRONMENT, oanda's regulatory DIVISION, and for a ctrader list \
                  the tier it was fetched at. docs/decisions/0066 decision 6."
                .to_string(),
        });
    }
    match venue {
        // MEASURED, and therefore enumerable: `urls_for` splits exactly two hosts.
        "alpaca" if !ALPACA_ENVIRONMENTS.contains(&q) => Err(InstrumentListError::BadQualifier {
            venue: venue.to_string(),
            why: format!(
                "alpaca's qualifier names the ENVIRONMENT the list was fetched from and must be \
                 one of {ALPACA_ENVIRONMENTS:?}; got `{q}`. The account is in no part of \
                 `/v1/assets`, so the environment is the ONE axis on which alpaca's list is not \
                 common."
            ),
        }),
        // ⚠ NON-BLANK ONLY, and the looseness is DECLARED rather than an oversight. oanda's
        // qualifier names the regulatory DIVISION, and this checkout cannot enumerate OANDA's
        // divisions: 0066's decision 6 says exactly where that evidence stops. Validating against
        // an invented list would be worse than validating against none: it would refuse a correct
        // list on the strength of a guess.
        _ => Ok(()),
    }
}

pub(crate) fn is_iso_date(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 10
        && b[4] == b'-'
        && b[7] == b'-'
        && b.iter().enumerate().all(|(i, c)| i == 4 || i == 7 || c.is_ascii_digit())
}

fn first_duplicate(rows: &[InstrumentList]) -> Option<String> {
    rows.iter()
        .enumerate()
        .find(|(i, row)| rows[..*i].iter().any(|previous| previous.venue == row.venue))
        .map(|(_, row)| row.venue.clone())
}

/// **The picker's universe: both sources in precedence order, one venue at a time.** Decision 7's
/// rule 3, as a pure function.
///
/// Two sources and one rule:
///
/// 1. [`crate::CatalogCache`]: the lists the datahub keeps, as the desktop adopted them;
/// 2. `local`: the operator's own CREDENTIALED fetches, written by `vike-backend catalog refresh`.
///    They cover exactly the venues the cache can never hold, so the two never contend in practice;
///    the ordering is here so that if they ever did, the answer would be decided rather than
///    incidental.
///
/// ⚠ **Keyed on whether the venue has been ANSWERED FOR, never on whether a source holds rows.** A
/// stamp is a MEASURED claim, so a venue refreshed to an empty list keeps its empty list.
///
/// ⚠ It builds a NEW `Vec` and touches no input: the operator's rows never enter
/// [`crate::CatalogCache`] and are never persisted with it (the module doc's rule 2).
#[must_use]
pub fn merge_sources(cache: &crate::CatalogCache, local: &InstrumentLists) -> Vec<Instrument> {
    let mut out = cache.instruments.clone();
    for row in &local.venues {
        if cache.fetched.iter().any(|s| s.venue == row.venue) {
            continue;
        }
        out.extend(row.instruments.iter().cloned());
    }
    out
}

/// **Which source answered for one venue**: the fact a row must be able to state, so that the
/// sources can never silently disagree about which one the operator is looking at.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CatalogAnswer {
    /// The kept list the desktop adopted. Wins wherever it exists, even at zero.
    OperatorFetch,
    /// The operator's OWN CREDENTIALED fetch, run locally on the box that holds the keys
    /// (`vike-backend catalog refresh`).
    LocalCredentialed,
    /// Neither. `—` and `never`, which is what every credentialed venue reads on a box with no keys.
    Nothing,
}

/// [`CatalogAnswer`] for one venue, from the two facts a row already holds. The precedence is
/// [`merge_sources`]', spelled once so a row and the picker cannot disagree.
#[must_use]
pub fn catalog_answer(has_stamp: bool, has_local: bool) -> CatalogAnswer {
    match (has_stamp, has_local) {
        (true, _) => CatalogAnswer::OperatorFetch,
        (false, true) => CatalogAnswer::LocalCredentialed,
        (false, false) => CatalogAnswer::Nothing,
    }
}

/// What [`upsert_local`] did: the CLI's report, and the safety rule stated where it applies.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CatalogLocalUpsert {
    /// The row was written. `previous` is what it replaced (0 for a venue that had none).
    Recorded { count: usize, previous: usize },
    /// **The fetch came back EMPTY and the venue already had a list, so nothing was written.**
    ///
    /// ⚠ The same rule `crates/vike-app-core/src/data/catalog_refresh.rs`'s `merge_refresh` enforces for
    /// the cache, stated here because this is a second writer of a second list and the rule does
    /// not travel with the operator's memory: a list silently swapped for an empty one makes every
    /// one of that venue's instruments un-pickable with nothing on screen to say why. An empty
    /// answer for a venue that had NOTHING is recorded (it is a measured zero, and something
    /// measuring zero is news).
    KeptExisting { kept: usize },
}

/// **Fold ONE credentialed fetch into the operator's own local document.**
///
/// The operator twin of `merge_refresh`, and deliberately not a call to it: that function folds
/// into [`crate::CatalogCache`], whose whole-file writer lives in the desktop. This decides ONE
/// venue; the caller then writes that venue's row and no other.
///
/// A local row with no `fetched` date or no qualifier would be refused on the next read
/// ([`InstrumentLists::check`]), which is why the CLI stamps both.
pub fn upsert_local(doc: &mut InstrumentLists, row: InstrumentList) -> CatalogLocalUpsert {
    let previous = doc.venue(&row.venue).map_or(0, |v| v.instruments.len());
    if row.instruments.is_empty() && previous > 0 {
        return CatalogLocalUpsert::KeptExisting { kept: previous };
    }
    let count = row.instruments.len();
    match doc.venues.iter_mut().find(|v| v.venue == row.venue) {
        Some(slot) => *slot = row,
        None => doc.venues.push(row),
    }
    CatalogLocalUpsert::Recorded { count, previous }
}

#[path = "instrument_list_tests.rs"]
#[cfg(test)]
mod instrument_list_tests;
