//! **The SHIPPED baseline instrument list** — one venue's universe, fetched out of band by us,
//! carrying the date it was fetched and the qualifier that makes it honest.
//! `docs/decisions/0066-the-venue-catalog-is-on-by-default-and-the-switch-is-its-refusal.md`,
//! decisions 5 through 8.
//!
//! # What this is FOR, and what it very deliberately is not
//!
//! `docs/decisions/0062`'s decision 3 refuses alpaca, oanda and ctrader from a datahub's provider
//! table BY CONSTRUCTION: listing them means authenticating as the OPERATOR, a rate limit bounds
//! spending a public budget and nothing bounds spending an identity. That verdict is untouched.
//! What this module adds is a list fetched BEFORE the tag, by an owner who holds those credentials,
//! so a fresh install can show something for those venues without any client causing any
//! authentication anywhere. **No venue is admitted to any server's table and no request reaches a
//! venue** — which is why 0066's decision 5 rules this is not decision 3's reopener.
//!
//! The precedent in this tree is `crates/bridges/dukascopy/src/catalog.rs`'s and
//! `crates/bridges/fxcm/src/catalog.rs`'s `FX_TABLE` — bundled static lists, zero venue requests.
//! What is new is STALENESS: those two were never fetched, so they have no age to be wrong about,
//! and this one does.
//!
//! # ⚠ "Common per venue" holds for ONE of the three, which is why this type carries a QUALIFIER
//!
//! 0066's decision 6, measured against each bridge's own listing call:
//!
//! * **alpaca — COMMON.** `crates/bridges/alpaca/src/catalog.rs`'s `fetch_assets` issues one
//!   `/v1/assets?status=active` against the broker host and the account id is in NO part of it —
//!   not the path, not the query, not a header — though `AlpacaConfig` carries one. One axis does
//!   vary and is what [`BaselineVenue::qualifier`] carries here: the ENVIRONMENT, because
//!   `AlpacaCatalog::new` resolves the demo host and a sandbox universe is not guaranteed to equal
//!   a live one.
//! * **oanda — NOT common.** `crates/bridges/oanda/src/catalog.rs`'s `fetch_instruments` builds its
//!   path from `config.account_id`: the account is IN THE PATH, and OANDA's published contract says
//!   the list depends on the regulatory DIVISION the account sits in. Common per USER, not per
//!   VENUE. A single shipped list is wrong for every operator outside the division it came from,
//!   and wrong in the direction that hurts — it offers instruments they cannot trade. So oanda
//!   ships only with a DIVISION declared.
//! * **ctrader — NOT common, and worse.** `crates/bridges/ctrader/src/conn.rs`'s `fetch_symbols`
//!   is account-scoped by PROTOCOL FIELD across a white-label platform whose brokers spell symbol
//!   ids differently, and `crates/bridges/ctrader/src/config.rs`'s `ctrader_host` splits demo from
//!   live and records no broker at all. A name from one broker applied to another does not merely
//!   miss; it can resolve to a DIFFERENT INSTRUMENT. **ctrader SHIPS nothing, and
//!   [`BaselineCatalog::parse`] refuses a row for it** rather than leaving that to an author's
//!   memory. ⚠ The OPERATOR's OWN ctrader fetch is admitted ([`BaselineCatalog::parse_local`]),
//!   and that is this decision read correctly rather than an exception to it: what it refuses is a
//!   SHARED list across brokers, and a list fetched with the operator's own credentials against
//!   their own account at their own broker is exactly the symbols they can trade. [`Provenance`]
//!   is the one rule that differs between the two documents.
//!
//! ⚠ Where the evidence stops, per that decision: the account-scoping is MEASURED from this tree;
//! the divisional and cross-broker divergence is the VENUE's published contract, which this
//! checkout cannot verify. Settling it needs two accounts in two divisions and two accounts at two
//! brokers, diffed.
//!
//! # The staleness rules, and they are the whole of decision 7
//!
//! 1. **The artifact carries its own provenance and a MISSING STAMP IS REFUSED.** The discipline
//!    copied is `crates/vike-ops/src/docs_data.rs`'s `generated_from`, which takes its stamp as a
//!    parameter and refuses a blank rather than defaulting it. A baseline with no date is not
//!    rendered at all, and that is not a degrade-vs-refuse violation: what is refused is the
//!    ARTIFACT, and the venue then reads exactly as it reads today — no list, with a reason.
//! 2. **It does NOT share [`crate::CatalogCache`]**, and the reason is a missing field rather than
//!    a preference. [`crate::VenueStamp`] records WHEN and HOW MANY and never FROM WHERE, so a
//!    merged baseline renders through `refreshed_label` as an AGE — indistinguishable from the
//!    operator's own refresh — and the next start could overwrite a real fetch with shipped data.
//!    The baseline is a separate READ-ONLY source.
//! 3. **Where both could answer, the operator's own fetch WINS and the row says which answered.**
//!    [`merge_sources`] is that rule, keyed on the CACHE STAMP rather than on whether the cache
//!    holds rows: a stamp at zero is a MEASURED claim about a venue, and shipped data must not
//!    overwrite a measurement that came out empty.
//!
//! And **the server does not serve this**, decision 7's last clause: the qualifier is a property of
//! the OPERATOR's account (which division, which broker), a server does not know it and must not
//! guess, and keeping the server's `NeedsCredentials` refusal TRUE is what keeps one refusal
//! spelling honest across the daemon, the CLI and the desktop.

use crate::{CatalogAvailability, Instrument, catalog_availability};
use serde::{Deserialize, Serialize};

/// Where a deployed binary looks for the artifact, under `vike_model::state_path::PROJECT_BIN_DIR`.
///
/// A SUBDIRECTORY, because that constant's own doc fixes the rule: "every tool under it owns a
/// subdirectory rather than sitting loose". The tool name in
/// `scripts/fetch_release_tools.sh`'s `TOOL_TABLE` is the first path segment.
pub const BASELINE_TOOL_DIR: &str = "catalog";

/// The artifact's filename — the release asset's name and the installed file's name, which are
/// deliberately the same string so a manifest line and a disk path read alike.
pub const BASELINE_FILE: &str = "venue-baseline.json";

/// The COMMITTED source, repo-relative. The bytes ARE the source (decision 8): there is nothing to
/// re-derive them from in CI, because producing them needs an authenticated venue fetch and no
/// runner may hold those credentials (`scripts/refuse_live_credentials.sh` refuses such a store
/// outright on both live-smoke lanes).
pub const BASELINE_SOURCE: &str = "assets/catalog/venue-baseline.json";

/// **The OPERATOR's OWN credentialed lists**, under `vike_model::state_path::STATE_SUBDIR` — the
/// same document shape as the shipped baseline, written by `vike-backend catalog refresh` on the box
/// that holds the keys, and read by every surface that renders a venue's list.
///
/// ⚠ **A SEPARATE FILE from `catalog.json`, and the separation prevents silent data loss rather
/// than being tidy.** `crates/vike-app-core/src/catalog_refresh.rs`'s `settle` persists
/// [`crate::CatalogCache`] by writing the WHOLE file from its in-memory state, so a CLI that added
/// rows to that file while a desktop was running would have them erased by the desktop's next
/// successful refresh — with nothing anywhere to say so. Two writers, one whole-file write, last
/// one wins. This file has ONE writer.
///
/// ⚠ It reuses [`BaselineCatalog`] because the shape is exactly right and for one reason more: an
/// operator's own list needs the SAME qualifier the shipped one does. A list fetched with the
/// operator's DEMO alpaca keys is not the live universe, and a list fetched from one oanda account
/// is that account's division — so the fact that makes a shipped list honest makes a local one
/// honest too, and giving them one type is what stops the second surface forgetting it.
pub const LOCAL_FILE: &str = "venue-catalog-local.json";

/// Why an artifact was refused. One variant per rule, because an operator acts on them
/// differently and a single string would make them one fact.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BaselineError {
    /// The bytes are not the document this type describes.
    Malformed(String),
    /// A venue row carried no fetch date, or one that is not `YYYY-MM-DD`. Rule 1: a baseline with
    /// no stamp is not rendered at all.
    NoStamp { venue: String },
    /// A venue row carried no qualifier, or one this venue's rule does not admit. See the module
    /// doc's decision-6 summary for what each venue's qualifier must name.
    BadQualifier { venue: String, why: String },
    /// A venue row for a venue that must not ship one — ctrader (decision 6), anything that is not
    /// `CatalogAvailability::Credentialed` (a publicly enumerable venue has a LIVE route, so a
    /// shipped list for it would be a second source with nothing to gain), and anything off the
    /// roster.
    VenueNotEligible { venue: String, why: String },
    /// An instrument whose own `venue` field disagrees with the row it sits in — the one way a
    /// hand-edited artifact can put a venue's symbols under another venue's name.
    VenueMismatch { venue: String, found: String },
}

impl std::fmt::Display for BaselineError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Malformed(e) => write!(f, "the baseline artifact is not readable: {e}"),
            Self::NoStamp { venue } => write!(
                f,
                "the baseline's `{venue}` row carries no `fetched` date in YYYY-MM-DD form. A list \
                 that cannot say when it was fetched is worse than no list, because it looks \
                 authoritative — so the whole artifact is refused rather than rendered undated."
            ),
            Self::BadQualifier { venue, why } => {
                write!(f, "the baseline's `{venue}` row carries no usable `qualifier`: {why}")
            }
            Self::VenueNotEligible { venue, why } => {
                write!(f, "the baseline must carry no `{venue}` row: {why}")
            }
            Self::VenueMismatch { venue, found } => write!(
                f,
                "the baseline's `{venue}` row holds an instrument tagged `{found}`. A row's \
                 instruments must all name the row's own venue, or the picker offers one venue's \
                 symbols under another's name."
            ),
        }
    }
}

impl std::error::Error for BaselineError {}

/// One venue's shipped list, with everything that makes it honest.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BaselineVenue {
    /// The roster slug.
    pub venue: String,
    /// **WHEN these bytes were fetched, `YYYY-MM-DD`.** Required; a blank or malformed value
    /// refuses the whole artifact ([`BaselineError::NoStamp`]).
    ///
    /// ⚠ A DATE rather than a timestamp, deliberately: the thing a reader needs is "how old is
    /// this, roughly", and a millisecond field beside [`crate::VenueStamp::last_refreshed_ms`]
    /// would invite exactly the confusion rule 2 exists to prevent — two numbers of the same shape
    /// meaning "we fetched this for you" and "you refreshed this yesterday".
    pub fetched: String,
    /// **The account-shaped fact that makes this list honest for THIS venue**, rendered to the
    /// operator beside the count. alpaca's ENVIRONMENT; oanda's regulatory DIVISION. See the module
    /// doc.
    pub qualifier: String,
    /// The list. May legitimately be empty — an empty row is a claim that the venue listed nothing
    /// on that date, which is why it still needs a stamp.
    pub instruments: Vec<Instrument>,
}

impl BaselineVenue {
    /// What the Instruments grid renders where a cache-fetched row would render an age — the
    /// SHIPPED spelling.
    ///
    /// ⚠ It must not read like [`crate::VenueStamp`]'s age and that is the point: `shipped
    /// 2026-09-16` cannot be mistaken for `2 d ago`, so "we fetched this for you" is
    /// distinguishable from "you refreshed this yesterday" without reading a second column.
    /// `crates/vike-app-core/src/catalog_refresh.rs`'s `refreshed_label` `"never"` arm is the seam
    /// this sits in.
    ///
    /// ⚠ **The OPERATOR's own document uses this same type and must NOT use this spelling**:
    /// `shipped` is a claim about where the bytes came from, and for a list they fetched themselves
    /// it is simply false. The renderer spells that case for itself
    /// (`crates/vike-app-core/src/tool_views/instruments.rs`'s `age_cell`) rather than this method
    /// taking a provenance argument, because the DATE is the only part the two share.
    #[must_use]
    pub fn label(&self) -> String {
        format!("shipped {}", self.fetched)
    }
}

/// The artifact.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct BaselineCatalog {
    /// A free-text line for whoever opens the committed file. Carried through serde so a round trip
    /// does not silently delete it; read by nothing.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub note: String,
    pub venues: Vec<BaselineVenue>,
}

/// alpaca's qualifier names the ENVIRONMENT, and this is the whole admissible set.
///
/// MEASURED from `crates/bridges/alpaca/src/config.rs`'s `hosts_for`, which splits
/// `broker-api.sandbox.alpaca.markets` from `broker-api.alpaca.markets` — two universes that are
/// not guaranteed equal, and the only axis on which alpaca's list is not common.
const ALPACA_ENVIRONMENTS: &[&str] = &["demo", "live"];

/// WHOSE document is being read — the ONE rule that differs between them.
///
/// ⚠ **Only ctrader separates the two, and the reason is decision 6's reason exactly.** A SHARED
/// list for that venue is not defensible: one cTrader id holds accounts at several brokers whose
/// symbol ids differ, so a name from one applied to another can resolve to a different instrument.
/// An operator's OWN list is fetched with their own credentials against their own account at their
/// own broker, so the hazard does not exist — it is a list of exactly the symbols they can trade.
/// Folding the two documents into one rule would either ship a list that must not be shipped, or
/// refuse an operator the one venue where their own fetch is the only honest source there could be.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Provenance {
    /// [`BASELINE_SOURCE`] / the installed release asset — fetched by US, before the tag.
    Shipped,
    /// [`LOCAL_FILE`] — fetched by the OPERATOR, on their box, with their own credentials.
    Operator,
}

impl BaselineCatalog {
    /// **Parse and VALIDATE the SHIPPED artifact.** See [`Self::parse_as`].
    pub fn parse(bytes: &[u8]) -> Result<Self, BaselineError> {
        Self::parse_as(bytes, Provenance::Shipped)
    }

    /// **Parse and VALIDATE the OPERATOR's own document** ([`LOCAL_FILE`]).
    pub fn parse_local(bytes: &[u8]) -> Result<Self, BaselineError> {
        Self::parse_as(bytes, Provenance::Operator)
    }

    /// **Parse and VALIDATE.** Every rule the module doc states is enforced here, because an
    /// artifact that is merely parsed is an artifact whose rules live in whoever remembered them.
    ///
    /// ⚠ It refuses the WHOLE artifact on any bad row rather than dropping the row. A partially
    /// rendered baseline is the looks-authoritative failure in miniature: the operator sees a list
    /// for one venue and nothing for another, with no way to tell a deliberate absence from a
    /// silently discarded row.
    pub fn parse_as(bytes: &[u8], from: Provenance) -> Result<Self, BaselineError> {
        let doc: Self =
            serde_json::from_slice(bytes).map_err(|e| BaselineError::Malformed(e.to_string()))?;
        for row in &doc.venues {
            eligible(&row.venue, from)?;
            if !is_iso_date(&row.fetched) {
                return Err(BaselineError::NoStamp { venue: row.venue.clone() });
            }
            check_qualifier(&row.venue, &row.qualifier)?;
            if let Some(bad) = row.instruments.iter().find(|i| i.venue != row.venue) {
                return Err(BaselineError::VenueMismatch {
                    venue: row.venue.clone(),
                    found: bad.venue.clone(),
                });
            }
        }
        if let Some(dup) = first_duplicate(&doc.venues) {
            return Err(BaselineError::VenueNotEligible {
                venue: dup,
                why: "it appears twice. One venue has one list per document, or `merge_sources` \
                      would pick whichever row happened to come first."
                    .to_string(),
            });
        }
        Ok(doc)
    }

    /// One venue's row, or `None`.
    #[must_use]
    pub fn venue(&self, venue: &str) -> Option<&BaselineVenue> {
        self.venues.iter().find(|v| v.venue == venue)
    }
}

/// Which venues may appear in which document — decision 6's ruling, held structurally.
fn eligible(venue: &str, from: Provenance) -> Result<(), BaselineError> {
    if venue == "ctrader" && from == Provenance::Shipped {
        return Err(BaselineError::VenueNotEligible {
            venue: venue.to_string(),
            why: "cTrader is a white-label platform: one cTrader id holds accounts at several \
                  brokers, `fetch_symbols` is scoped to an account by protocol field, and the \
                  venue's own guidance warns that symbol ids differ across brokers. A SHARED list \
                  would not merely miss names — it could resolve one to a DIFFERENT instrument. \
                  docs/decisions/0066 decision 6. The OPERATOR's own fetch is a different \
                  proposition and is admitted: it lists their own account at their own broker."
                .to_string(),
        });
    }
    match catalog_availability(venue) {
        CatalogAvailability::Credentialed => Ok(()),
        CatalogAvailability::PublicBulk => Err(BaselineError::VenueNotEligible {
            venue: venue.to_string(),
            why: "it is publicly enumerable, so a live route already exists for it (a linked \
                  provider, or the backend's datahub). A shipped list would be a SECOND source for \
                  a venue that has a first one, which is the silent disagreement decision 7 \
                  refuses."
                .to_string(),
        }),
        CatalogAvailability::NoBulkList { .. } => Err(BaselineError::VenueNotEligible {
            venue: venue.to_string(),
            why: "the venue publishes no bulk instrument list at any price, so there was nothing \
                  to fetch out of band either."
                .to_string(),
        }),
        CatalogAvailability::UnknownVenue => Err(BaselineError::VenueNotEligible {
            venue: venue.to_string(),
            why: "it is not on the canonical `vike_model::VENUES` roster.".to_string(),
        }),
    }
}

/// The per-venue qualifier rule. Each arm carries its own evidence, and the oanda arm carries the
/// declared limit of it.
fn check_qualifier(venue: &str, qualifier: &str) -> Result<(), BaselineError> {
    let q = qualifier.trim();
    if q.is_empty() {
        return Err(BaselineError::BadQualifier {
            venue: venue.to_string(),
            why: "every row must declare the account-shaped fact that makes it honest, because \
                  the list is not common per venue for two of the three venues that can carry \
                  one — alpaca's ENVIRONMENT, oanda's regulatory DIVISION, and for a locally \
                  fetched ctrader list the tier it was fetched at. docs/decisions/0066 \
                  decision 6."
                .to_string(),
        });
    }
    match venue {
        // MEASURED, and therefore enumerable: `hosts_for` splits exactly two hosts.
        "alpaca" if !ALPACA_ENVIRONMENTS.contains(&q) => Err(BaselineError::BadQualifier {
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
        // divisions — 0066's decision 6 says exactly where that evidence stops. Validating against
        // an invented list would be worse than validating against none: it would refuse a correct
        // artifact on the strength of a guess.
        _ => Ok(()),
    }
}

fn is_iso_date(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 10
        && b[4] == b'-'
        && b[7] == b'-'
        && b.iter().enumerate().all(|(i, c)| i == 4 || i == 7 || c.is_ascii_digit())
}

fn first_duplicate(rows: &[BaselineVenue]) -> Option<String> {
    rows.iter()
        .enumerate()
        .find(|(i, row)| rows[..*i].iter().any(|prev| prev.venue == row.venue))
        .map(|(_, row)| row.venue.clone())
}

/// **The picker's universe: every source in precedence order, one venue at a time.** Decision 7's
/// rule 3, as a pure function.
///
/// Three sources and one rule — **the operator's own data always beats shipped data**:
///
/// 1. [`crate::CatalogCache`] — the desktop's own refreshes, for the venues it can route;
/// 2. `local` — the operator's own CREDENTIALED fetches ([`LOCAL_FILE`], written by
///    `vike-backend catalog refresh`). These are the operator's data too, and they cover exactly the
///    venues the cache can never hold, so the two never contend in practice — the ordering is here
///    so that if they ever did, the answer would be decided rather than incidental;
/// 3. `shipped` — the baseline we fetched before the tag.
///
/// ⚠ **Keyed on whether the venue has been ANSWERED FOR, never on whether a source holds rows.** A
/// stamp (or a local row) is a MEASURED claim — `crates/vike-app-core/src/tool_views/instruments.rs`
/// already renders `—` rather than `0` for the absence of one, on exactly that argument — so a
/// venue the operator refreshed to an empty list keeps its empty list. Keying on `is_empty()` would
/// let shipped data silently overwrite a measurement that came out empty, which is the one
/// direction this rule forbids.
///
/// ⚠ It builds a NEW `Vec` and touches no input. Neither document ever enters
/// [`crate::CatalogCache`] and neither is ever persisted with it — see the module doc's rule 2.
#[must_use]
pub fn merge_sources(
    cache: &crate::CatalogCache,
    local: &BaselineCatalog,
    shipped: &BaselineCatalog,
) -> Vec<Instrument> {
    let mut out = cache.instruments.clone();
    for row in &local.venues {
        if cache.fetched.iter().any(|s| s.venue == row.venue) {
            continue;
        }
        out.extend(row.instruments.iter().cloned());
    }
    for row in &shipped.venues {
        let answered =
            cache.fetched.iter().any(|s| s.venue == row.venue) || local.venue(&row.venue).is_some();
        if answered {
            continue;
        }
        out.extend(row.instruments.iter().cloned());
    }
    out
}

/// **Which source answered for one venue** — the fact a row must be able to state, so that three
/// sources can never silently disagree about which one the operator is looking at.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CatalogAnswer {
    /// The desktop's own refresh of a routable venue. Wins wherever it exists, even at zero.
    OperatorFetch,
    /// The operator's OWN CREDENTIALED fetch, run locally on the box that holds the keys
    /// (`vike-backend catalog refresh`). Their data, so it beats anything shipped.
    LocalCredentialed,
    /// The shipped baseline — nothing of the operator's has measured this venue.
    ShippedBaseline,
    /// None of the three. `—` and `never`, which is what every credentialed venue reads on a box
    /// with no keys and no installed baseline.
    Nothing,
}

/// [`CatalogAnswer`] for one venue, from the three facts a row already holds. The precedence is
/// [`merge_sources`]', spelled once so a row and the picker cannot disagree.
#[must_use]
pub fn catalog_answer(has_stamp: bool, has_local: bool, has_shipped: bool) -> CatalogAnswer {
    match (has_stamp, has_local, has_shipped) {
        (true, _, _) => CatalogAnswer::OperatorFetch,
        (false, true, _) => CatalogAnswer::LocalCredentialed,
        (false, false, true) => CatalogAnswer::ShippedBaseline,
        (false, false, false) => CatalogAnswer::Nothing,
    }
}

/// What [`upsert_local`] did — the CLI's report, and the safety rule stated where it applies.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LocalUpsert {
    /// The row was written. `previous` is what it replaced (0 for a venue that had none).
    Recorded { count: usize, previous: usize },
    /// **The fetch came back EMPTY and the venue already had a list, so nothing was written.**
    ///
    /// ⚠ The same rule `crates/vike-app-core/src/catalog_refresh.rs`'s `merge_refresh` enforces for
    /// the cache, stated here because this is a second writer of a second file and the rule does
    /// not travel with the operator's memory: a list silently swapped for an empty one makes every
    /// one of that venue's instruments un-pickable with nothing on screen to say why. An empty
    /// answer for a venue that had NOTHING is recorded (it is a measured zero, and something
    /// measuring zero is news).
    KeptExisting { kept: usize },
}

/// **Fold ONE credentialed fetch into the operator's own local document.**
///
/// The [`LOCAL_FILE`] twin of `merge_refresh`, and deliberately not a call to it: that function
/// folds into [`crate::CatalogCache`], whose whole-file writer lives in the desktop. This is a
/// per-venue upsert over a document with ONE writer.
///
/// Every rule [`BaselineCatalog::parse`] applies to a shipped row applies to a local one, because
/// they are the same document type read by the same loader — a local row with no `fetched` date or
/// no qualifier would be refused on the next read, which is why the CLI stamps both.
pub fn upsert_local(doc: &mut BaselineCatalog, row: BaselineVenue) -> LocalUpsert {
    let previous = doc.venue(&row.venue).map_or(0, |v| v.instruments.len());
    if row.instruments.is_empty() && previous > 0 {
        return LocalUpsert::KeptExisting { kept: previous };
    }
    let count = row.instruments.len();
    match doc.venues.iter_mut().find(|v| v.venue == row.venue) {
        Some(slot) => *slot = row,
        None => doc.venues.push(row),
    }
    LocalUpsert::Recorded { count, previous }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AssetClass, CatalogCache, VenueStamp};

    fn inst(venue: &str, sym: &str) -> Instrument {
        Instrument {
            venue: venue.into(),
            raw_symbol: sym.into(),
            asset_class: AssetClass::Equity,
            base: sym.into(),
            quote: "USD".into(),
            description: String::new(),
            properties: Default::default(),
            contract_type: None,
            settle_asset: None,
        }
    }

    fn row(venue: &str, fetched: &str, qualifier: &str) -> BaselineVenue {
        BaselineVenue {
            venue: venue.into(),
            fetched: fetched.into(),
            qualifier: qualifier.into(),
            instruments: vec![inst(venue, "AAPL")],
        }
    }

    fn doc(rows: Vec<BaselineVenue>) -> Vec<u8> {
        serde_json::to_vec(&BaselineCatalog { note: String::new(), venues: rows }).unwrap()
    }

    #[test]
    fn a_well_formed_alpaca_row_parses_and_carries_its_date() {
        let out = BaselineCatalog::parse(&doc(vec![row("alpaca", "2026-09-16", "demo")]))
            .expect("a stamped, qualified alpaca row is the honest case");
        let v = out.venue("alpaca").expect("the row is addressable by venue");
        assert_eq!(v.label(), "shipped 2026-09-16");
        // ⚠ It must not read like an AGE — that is rule 3's whole point.
        assert!(!v.label().contains("ago"), "{}", v.label());
    }

    /// Rule 1, held rather than left to an author: an undated list looks authoritative, so the
    /// ARTIFACT is refused and the venue falls back to reading exactly as it does today.
    #[test]
    fn a_stampless_or_misdated_row_refuses_the_whole_artifact() {
        for bad in ["", "2026-9-16", "16-09-2026", "2026-09-16T00:00:00Z", "yesterday"] {
            let err = BaselineCatalog::parse(&doc(vec![row("alpaca", bad, "demo")]))
                .expect_err("a bad stamp must refuse");
            assert_eq!(err, BaselineError::NoStamp { venue: "alpaca".into() }, "{bad}");
            assert!(err.to_string().contains("YYYY-MM-DD"), "the fix is named: {err}");
        }
    }

    /// Decision 6, held structurally: ctrader ships nothing, and the refusal names the mechanism
    /// rather than saying "not allowed".
    ///
    /// ⚠ …and the OPERATOR's own ctrader fetch IS admitted, which is the same decision read
    /// correctly rather than an exception to it: what that decision refuses is a SHARED list across
    /// brokers whose symbol ids differ. A list fetched with the operator's own credentials against
    /// their own account at their own broker is exactly the symbols they can trade — and it is the
    /// only honest source ctrader could ever have.
    #[test]
    fn ctrader_may_never_ship_a_baseline_but_the_operator_may_fetch_their_own() {
        let err = BaselineCatalog::parse(&doc(vec![row("ctrader", "2026-09-16", "icmarkets")]))
            .expect_err("ctrader must be refused in a SHIPPED artifact");
        let msg = err.to_string();
        assert!(msg.contains("differ across brokers"), "{msg}");
        assert!(msg.contains("DIFFERENT instrument"), "the hazard is named: {msg}");

        BaselineCatalog::parse_local(&doc(vec![row("ctrader", "2026-09-16", "demo")]))
            .expect("the operator's OWN ctrader list is admitted");
    }

    /// …and neither may a venue that already has a live route, nor one that has none at all.
    #[test]
    fn only_a_credentialed_venue_is_eligible() {
        // Publicly enumerable — a live route exists, so a shipped list is a second source.
        let public = BaselineCatalog::parse(&doc(vec![row("binance", "2026-09-16", "x")]))
            .expect_err("a public venue must be refused");
        assert!(public.to_string().contains("publicly enumerable"), "{public}");
        // No bulk list at any price — there was nothing to fetch out of band either.
        let none = BaselineCatalog::parse(&doc(vec![row("ig", "2026-09-16", "x")]))
            .expect_err("a no-bulk-list venue must be refused");
        assert!(none.to_string().contains("no bulk instrument list"), "{none}");
        // Off the roster — fail-closed.
        let bogus = BaselineCatalog::parse(&doc(vec![row("kraken", "2026-09-16", "x")]))
            .expect_err("an unknown venue must be refused");
        assert!(bogus.to_string().contains("roster"), "{bogus}");
        // …and oanda, the second eligible venue, is ACCEPTED with a division declared.
        BaselineCatalog::parse(&doc(vec![row("oanda", "2026-09-16", "global-markets")]))
            .expect("oanda ships with a division declared");
    }

    /// The qualifier is what makes the list honest, so a blank one refuses — and alpaca's is
    /// enumerable because its axis is MEASURED, while oanda's deliberately is not.
    #[test]
    fn a_qualifier_is_required_and_alpacas_is_the_measured_one() {
        for blank in ["", "   "] {
            let err = BaselineCatalog::parse(&doc(vec![row("alpaca", "2026-09-16", blank)]))
                .expect_err("a blank qualifier must refuse");
            assert!(err.to_string().contains("not common per venue"), "{err}");
        }
        let wrong = BaselineCatalog::parse(&doc(vec![row("alpaca", "2026-09-16", "sandbox")]))
            .expect_err("alpaca's environment set is enumerable and closed");
        assert!(wrong.to_string().contains("ENVIRONMENT"), "{wrong}");
        for ok in ALPACA_ENVIRONMENTS {
            BaselineCatalog::parse(&doc(vec![row("alpaca", "2026-09-16", ok)])).expect(ok);
        }
        // oanda's is NON-BLANK only — this checkout cannot enumerate OANDA's divisions, and
        // refusing a correct artifact on the strength of a guess would be worse than not checking.
        BaselineCatalog::parse(&doc(vec![row("oanda", "2026-09-16", "whatever-division")])).expect(
            "oanda's qualifier is validated as present, not as a member of an invented set",
        );
    }

    #[test]
    fn a_row_may_not_hold_another_venues_instruments_and_may_not_appear_twice() {
        let mut mixed = row("alpaca", "2026-09-16", "demo");
        mixed.instruments.push(inst("oanda", "EUR_USD"));
        let err = BaselineCatalog::parse(&doc(vec![mixed])).expect_err("a mismatch must refuse");
        assert!(err.to_string().contains("tagged `oanda`"), "{err}");

        let twice = BaselineCatalog::parse(&doc(vec![
            row("alpaca", "2026-09-16", "demo"),
            row("alpaca", "2026-09-15", "live"),
        ]))
        .expect_err("one venue has one shipped list");
        assert!(twice.to_string().contains("appears twice"), "{twice}");
    }

    /// **Rule 3, and the direction that matters: a MEASURED EMPTY beats a shipped list.**
    ///
    /// ⚠ Mutation proof: key the skip on `cache.instruments.iter().any(..)` instead of on the
    /// stamp and the last assertion goes red — which is the whole bug, because a venue the
    /// operator refreshed to nothing would silently get shipped rows back.
    #[test]
    fn the_operators_own_fetch_wins_even_when_it_measured_nothing() {
        let shipped = BaselineCatalog {
            note: String::new(),
            venues: vec![row("alpaca", "2026-09-16", "demo"), row("oanda", "2026-09-16", "eu")],
        };
        let none = BaselineCatalog::default();

        // Nothing measured: the shipped list fills both.
        let empty = CatalogCache::default();
        assert_eq!(merge_sources(&empty, &none, &shipped).len(), 2);

        // alpaca measured with rows: the operator's list stands, oanda still comes from the shipped
        // one, and the operator's own instrument is the one that survives.
        let mut cache = CatalogCache {
            fetched: vec![VenueStamp { venue: "alpaca".into(), last_refreshed_ms: 1, count: 1 }],
            instruments: vec![inst("alpaca", "TSLA")],
        };
        let merged = merge_sources(&cache, &none, &shipped);
        let alpaca: Vec<&str> =
            merged.iter().filter(|i| i.venue == "alpaca").map(|i| i.raw_symbol.as_str()).collect();
        assert_eq!(alpaca, ["TSLA"], "shipped data may not join a measured venue");
        assert_eq!(merged.iter().filter(|i| i.venue == "oanda").count(), 1);

        // …and the direction the mutation proof names: a MEASURED ZERO keeps its zero.
        cache.instruments.clear();
        cache.fetched[0].count = 0;
        let merged = merge_sources(&cache, &none, &shipped);
        assert_eq!(
            merged.iter().filter(|i| i.venue == "alpaca").count(),
            0,
            "a venue the operator refreshed to nothing must not get shipped rows back"
        );
    }

    /// **The operator's OWN CREDENTIALED fetch beats the shipped list** — their keys, their
    /// account, their qualifier.
    #[test]
    fn a_local_credentialed_fetch_outranks_the_shipped_one() {
        let shipped = BaselineCatalog {
            note: String::new(),
            venues: vec![BaselineVenue {
                venue: "alpaca".into(),
                fetched: "2026-01-01".into(),
                qualifier: "demo".into(),
                instruments: vec![inst("alpaca", "SHIPPED")],
            }],
        };
        let local = BaselineCatalog {
            note: String::new(),
            venues: vec![BaselineVenue {
                venue: "alpaca".into(),
                fetched: "2026-09-16".into(),
                qualifier: "live".into(),
                instruments: vec![inst("alpaca", "MINE")],
            }],
        };
        let merged = merge_sources(&CatalogCache::default(), &local, &shipped);
        let symbols: Vec<&str> = merged.iter().map(|i| i.raw_symbol.as_str()).collect();
        assert_eq!(symbols, ["MINE"], "shipped rows must not join a locally fetched venue");

        // …and a local fetch that measured NOTHING still wins, for `merge_sources`' own reason.
        let empty_local = BaselineCatalog {
            note: String::new(),
            venues: vec![BaselineVenue {
                venue: "alpaca".into(),
                fetched: "2026-09-16".into(),
                qualifier: "live".into(),
                instruments: vec![],
            }],
        };
        assert!(
            merge_sources(&CatalogCache::default(), &empty_local, &shipped).is_empty(),
            "a measured zero is a measured claim, whoever measured it"
        );
    }

    #[test]
    fn a_row_always_says_which_source_answered() {
        assert_eq!(catalog_answer(true, true, true), CatalogAnswer::OperatorFetch);
        assert_eq!(catalog_answer(true, false, false), CatalogAnswer::OperatorFetch);
        assert_eq!(catalog_answer(false, true, true), CatalogAnswer::LocalCredentialed);
        assert_eq!(catalog_answer(false, false, true), CatalogAnswer::ShippedBaseline);
        assert_eq!(catalog_answer(false, false, false), CatalogAnswer::Nothing);
    }

    /// **A credentialed fetch that came back with nothing must not delete the list it had** — the
    /// `merge_refresh` safety rule, restated for the file that has a second writer.
    ///
    /// ⚠ Mutation proof: delete the `previous > 0` guard and the second assertion goes red, which
    /// is the failure it names — a transient venue outage silently emptying the picker.
    #[test]
    fn a_local_upsert_never_replaces_a_list_with_nothing() {
        let mut doc = BaselineCatalog::default();
        let full = || BaselineVenue {
            venue: "alpaca".into(),
            fetched: "2026-09-16".into(),
            qualifier: "live".into(),
            instruments: vec![inst("alpaca", "AAPL"), inst("alpaca", "TSLA")],
        };
        let empty = || BaselineVenue { instruments: vec![], ..full() };

        // An empty answer for a venue that had NOTHING is a measured zero and IS recorded.
        assert_eq!(
            upsert_local(&mut doc, empty()),
            LocalUpsert::Recorded { count: 0, previous: 0 }
        );
        assert_eq!(upsert_local(&mut doc, full()), LocalUpsert::Recorded { count: 2, previous: 0 });
        // …and now an empty answer keeps what was there, and says what it kept.
        assert_eq!(upsert_local(&mut doc, empty()), LocalUpsert::KeptExisting { kept: 2 });
        assert_eq!(doc.venue("alpaca").unwrap().instruments.len(), 2);
        // One venue, one row, however many times it is fetched.
        assert_eq!(doc.venues.len(), 1);
    }

    #[test]
    fn malformed_bytes_are_refused_by_name_rather_than_read_as_an_empty_baseline() {
        let err = BaselineCatalog::parse(b"not json at all").expect_err("must refuse");
        assert!(matches!(err, BaselineError::Malformed(_)), "{err:?}");
        // An EMPTY artifact is legitimate and distinct: it is a document with no venue rows.
        let empty = BaselineCatalog::parse(br#"{"venues":[]}"#).expect("an empty artifact parses");
        assert!(empty.venues.is_empty());
    }
}
