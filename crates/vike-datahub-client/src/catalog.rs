//! **The bounds and the ANSWER SHAPE for [`Request::VenueCatalog`](crate::proto::Request::VenueCatalog)
//! that BOTH ends need** — the venue validator, the listing cap, and the outcome enum that keeps
//! "this venue has no list" from ever looking like "this venue listed nothing". Declared ungated: a
//! default `vike-datahub` build must DECODE the verb in order to refuse it cleanly.
//!
//! # Why the constants live HERE and not in `vike-datahub`
//!
//! A client cannot guard a rule the server does not know, nor the reverse (the rule [`crate::seed`]
//! and [`crate::market`] state). `crates/vike-datahub/src/server/venue_catalog.rs`'s
//! `venue_catalog_verb` and [`crate::DatahubClient::venue_catalog`] both call
//! [`validate_catalog_venue`], so a refusal seen locally is the one the server would give. The
//! SERVER's own constants (the token bucket, the memo TTL) stay in
//! `crates/vike-datahub/src/catalog.rs`: a client that knew a bucket's state could only mis-predict
//! it.
//!
//! # ⚠ The outcome is an ENUM because an empty list is a LIE for two roster venues
//!
//! `docs/decisions/0062-a-venue-catalog-fetch-is-an-observe-verb-and-not-a-write.md`'s decision 5:
//! `ig` is `vike_catalog::CatalogMode::QueryBacked` and `ibkr` ships no
//! `vike_catalog::CatalogProvider`, so a `Vec<Instrument>` answer would render them as venues with
//! no instruments, which is false. The distinction lives in the TYPE, so a client cannot collapse it
//! by accident; a [`CatalogOutcome::Listed`] with an EMPTY `instruments` still means what it says.

use serde::{Deserialize, Serialize};
use vike_catalog::Instrument;

/// The most instruments ONE listing may carry across the wire — **derived from the largest
/// provider, not picked.** polymarket's provider (`crates/bridges/polymarket/src/catalog.rs`) walks
/// at most `GAMMA_MAX_PAGES` (40) pages of `GAMMA_PAGE_LIMIT` (500): 20,000 instruments, a bound
/// compiled into the BRIDGE and reachable by no request. 25,000 leaves it its whole range plus a
/// quarter, so a growing venue does not silently start truncating.
///
/// ⚠ **NOT a frame-size bound:** 25,000 instruments serialize to single-digit megabytes against
/// [`crate::proto::MAX_FRAME_LEN`]'s 64 MiB, so this cap binds first, and it must not move whenever
/// the frame limit does. A listing that hits it is TRUNCATED and says so
/// ([`CatalogOutcome::Listed::truncated`]) rather than being silently short.
pub const CATALOG_MAX_INSTRUMENTS: usize = 25_000;

// `CATALOG_MAX_INSTRUMENTS`' derivation, held at COMPILE TIME — the `crates/vike-datahub/src/seed.rs`
// idiom: an inequality, so a deliberate tweak compiles and a broken claim does not. If that bridge
// raises its pager, this stops compiling and the author re-runs the derivation.
const _: () = assert!(
    CATALOG_MAX_INSTRUMENTS > 40 * 500,
    "CATALOG_MAX_INSTRUMENTS must exceed the largest provider's OWN compiled bound (polymarket's \
     GAMMA_MAX_PAGES x GAMMA_PAGE_LIMIT = 20,000), or that venue's listing is truncated by this \
     cap rather than by its own pager. Re-run the derivation on the constant's doc."
);

/// The longest venue slug this verb will carry: `hyperliquid` (11 bytes) is the roster's longest,
/// and 16 is the next power of two. It bounds the ALLOCATION a hostile frame can ask for; the
/// charset and the server's own table lookup are the real bound.
pub const CATALOG_MAX_VENUE_BYTES: usize = 16;

/// The bytes a venue slug may be built from: ASCII lowercase letters and digits only — narrower
/// than the seed symbol's, because every `vike_model::VENUES` entry is lowercase alphanumeric. The
/// value never reaches a URL (it selects a row in the server's own table), so this is a cheap early
/// refusal rather than a security boundary; still an ALLOWLIST, because the dangerous set is open.
fn is_catalog_venue_byte(b: u8) -> bool {
    b.is_ascii_lowercase() || b.is_ascii_digit()
}

/// The venue rules, cheapest first, each naming what was wrong.
///
/// ⚠ **The message never ECHOES the venue** ([`crate::seed::validate_seed_symbol`]'s rule): it names
/// the LENGTH, the CAP and the OFFSET, never the untrusted string, which would ride into a log whose
/// file layer defaults to `trace`.
///
/// ⚠ **Not lower-cased or trimmed:** the server looks the slug up by exact match, so coercing it
/// would make the request and the answer disagree about which venue was asked for.
pub fn validate_catalog_venue(venue: &str) -> Result<(), String> {
    if venue.is_empty() {
        return Err(
            "a catalog venue is EMPTY, so it names no venue. Pass the roster slug (`binance`, \
             `okx`, `polymarket`)."
                .to_string(),
        );
    }
    if venue.len() > CATALOG_MAX_VENUE_BYTES {
        return Err(format!(
            "a catalog venue of {} bytes exceeds CATALOG_MAX_VENUE_BYTES = \
             {CATALOG_MAX_VENUE_BYTES}. The longest slug on the canonical roster is \
             `hyperliquid` at 11 bytes.",
            venue.len()
        ));
    }
    if let Some(at) = venue.bytes().position(|b| !is_catalog_venue_byte(b)) {
        return Err(format!(
            "a catalog venue carries a byte outside the permitted set at offset {at}. A venue slug \
             may hold ASCII lowercase letters and digits only — every entry on the canonical \
             `vike_model::VENUES` roster does, so anything else names no venue this server could \
             serve."
        ));
    }
    Ok(())
}

/// Why a venue cannot be enumerated — the REFUSALS, each a different fact about the world.
///
/// Separate variants rather than one string because a client renders them differently and an
/// operator can act on only two of them: see each variant's own doc.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum CatalogRefusal {
    /// **The venue has no bulk instrument list at all** — a fact about the VENUE, nothing an operator
    /// can change, and the variant that keeps an un-enumerable venue from rendering as an EMPTY one.
    /// `why` says which: `ig` is searched live per query (`vike_catalog::CatalogMode::QueryBacked`),
    /// `ibkr` ships no `vike_catalog::CatalogProvider`.
    NoBulkList { why: String },
    /// **The venue can only be listed by spending the OPERATOR's credentials** (alpaca, oanda,
    /// ctrader), so this server will not do it on a client's say-so — `docs/decisions/0062`'s
    /// decision 3, a property of what the server's table can CONTAIN rather than a switch. A rate
    /// limit bounds spending a public budget; nothing bounds spending an identity, so admitting one
    /// is a reopener, not a configuration change.
    NeedsCredentials,
    /// **This server's table does not carry the venue** — a fact about the BUILD, and the one
    /// refusal that names what it CAN serve, so an operator can tell a missing venue from a
    /// misspelled one (dukascopy and fxcm: bundled static lists, absent only because the data daemon
    /// does not link those bridges).
    NotServed { supported: Vec<String> },
}

impl CatalogRefusal {
    /// A one-line operator-facing sentence. The client renders this rather than assembling its own,
    /// so the daemon and the desktop cannot describe the same refusal differently.
    pub fn describe(&self, venue: &str) -> String {
        match self {
            Self::NoBulkList { why } => format!(
                "`{venue}` publishes no bulk instrument list: {why}. This is a property of the \
                 venue, not of this server — there is nothing to arm and nothing to rebuild."
            ),
            // ⚠ **Names the ACT, never a SCREEN:** this one spelling is rendered by the datahub's
            // log, the CLI and the desktop alike, and a daemon knows no GUI. It names what WOULD
            // list the venue (`docs/decisions/0066`'s decision 9) and NO SERVER SWITCH in either
            // direction: 0062's decision 3 is about what a server's table can CONTAIN, and a
            // locally-credentialed refresh is a different ACTOR. The fence is
            // `a_credential_refusal_never_suggests_arming_anything`.
            Self::NeedsCredentials => format!(
                "`{venue}` can only be listed with the operator's own venue credentials, and this \
                 server will not spend those on a client's request. It is listed on the box that \
                 HOLDS those keys, by the operator, with their own credentials — \
                 `vike-backend catalog refresh {venue}`. See \
                 `docs/decisions/0062-a-venue-catalog-fetch-is-an-observe-verb-and-not-a-write.md`."
            ),
            Self::NotServed { supported } => format!(
                "`{venue}` has no catalog provider in this server's build. Supported: [{}].",
                supported.join(", ")
            ),
        }
    }
}

/// What the server DID about one [`Request::VenueCatalog`](crate::proto::Request::VenueCatalog).
///
/// The states are mutually exclusive by construction: `Listed { instruments: [] }` and
/// [`CatalogRefusal::NoBulkList`] are different variants, so no client can collapse them by
/// forgetting to check a flag beside a vector.
/// ⚠ `PartialEq` but NOT `Eq`, by force: `vike_catalog::Instrument` carries `f64` tick/lot fields.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum CatalogOutcome {
    /// The venue was asked (or a fresh memo answered) and here is its universe. An EMPTY
    /// `instruments` is a legitimate venue fact and means exactly that.
    Listed {
        instruments: Vec<Instrument>,
        /// The listing hit [`CATALOG_MAX_INSTRUMENTS`] and is SHORT. Never silently.
        truncated: bool,
        /// The server answered from its in-process memo and called no venue: "seconds old" versus
        /// "up to the TTL old", what an operator staring at a stale symbol wants to know.
        cached: bool,
    },
    /// **The server's operator REFUSED this lane**, so no venue was called — a SUCCESS, not an
    /// error, exactly as `crate::seed`'s `SeedDone { armed: false }` is.
    ///
    /// ⚠ **The name outlived its default:** since
    /// `docs/decisions/0066-the-venue-catalog-is-on-by-default-and-the-switch-is-its-refusal.md` the
    /// catalog is ON by default, so this arrives only from a server with a written
    /// `flags.venue_catalog_off = true` row (or `VIKE_DATAHUB_VENUE_CATALOG_OFF=1`). The name is still
    /// true of the SERVER STATE (no lane is armed), so it is kept across a wire both ends spell.
    ///
    /// ⚠ Not a failure, but worth telling the operator: from a picker's side a refused server and a
    /// venue with no instruments look identical.
    NotArmed,
    /// The venue was refused before anything was fetched. See [`CatalogRefusal`].
    Refused(CatalogRefusal),
}

/// The [`Response::VenueCatalog`](crate::proto::Response::VenueCatalog) payload.
/// ⚠ `PartialEq` but not `Eq` — see [`CatalogOutcome`], which it contains.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CatalogListing {
    /// The venue this answers for, echoed VERBATIM as the client spelled it — so a client holding
    /// several in flight can match them up without relying on ordering.
    pub venue: String,
    /// What happened.
    pub outcome: CatalogOutcome,
}

impl CatalogListing {
    /// The instruments, or an empty slice for every non-`Listed` outcome.
    ///
    /// ⚠ A convenience for a caller that has ALREADY branched on [`Self::outcome`] — never a
    /// substitute for doing so, which is the collapse this module's whole shape exists to prevent.
    pub fn instruments(&self) -> &[Instrument] {
        match &self.outcome {
            CatalogOutcome::Listed { instruments, .. } => instruments,
            _ => &[],
        }
    }

    /// A one-line operator-facing sentence for every outcome, including the successful ones.
    pub fn describe(&self) -> String {
        match &self.outcome {
            CatalogOutcome::Listed { instruments, truncated, cached } => {
                let n = instruments.len();
                let src = if *cached { " (from this server's cache)" } else { "" };
                if *truncated {
                    format!(
                        "`{}`: {n} instruments{src} — TRUNCATED at CATALOG_MAX_INSTRUMENTS = \
                         {CATALOG_MAX_INSTRUMENTS}, so the tail of this venue's universe is \
                         missing.",
                        self.venue
                    )
                } else {
                    format!("`{}`: {n} instruments{src}.", self.venue)
                }
            }
            CatalogOutcome::NotArmed => format!(
                "this datahub serves no venue catalog because its operator REFUSED the lane — \
                 `vike-cli config set flags.venue_catalog_off true` was run on that box, or \
                 VIKE_DATAHUB_VENUE_CATALOG_OFF=1 is set. Run `vike-cli config set \
                 flags.venue_catalog_off false` and restart it to serve again. Nothing was fetched \
                 for `{}`, and that is a written refusal rather than a failure: the catalog is ON \
                 by default.",
                self.venue
            ),
            CatalogOutcome::Refused(r) => r.describe(&self.venue),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_canonical_roster_slug_is_accepted() {
        // Over the real roster, so a venue added with an unusual slug reddens HERE rather than
        // being refused at a server door nobody is watching.
        for v in vike_model::VENUES {
            validate_catalog_venue(v)
                .unwrap_or_else(|e| panic!("roster venue {v} must be accepted: {e}"));
            assert!(v.len() <= CATALOG_MAX_VENUE_BYTES, "{v} exceeds the cap");
        }
    }

    #[test]
    fn a_metacharacter_or_an_upper_case_slug_is_refused_and_never_echoed() {
        for bad in ["BINANCE", "bin ance", "binance/../x", "bin-ance", "bin_ance", "binance\n"] {
            let err = validate_catalog_venue(bad).expect_err("must be refused");
            assert!(err.contains("outside the permitted set"), "{bad}: {err}");
            assert!(!err.contains(bad), "the refusal echoed the venue back: {err}");
        }
    }

    #[test]
    fn an_empty_or_over_long_venue_is_refused_by_name() {
        assert!(validate_catalog_venue("").unwrap_err().contains("EMPTY"));
        let over = "x".repeat(CATALOG_MAX_VENUE_BYTES + 1);
        let err = validate_catalog_venue(&over).expect_err("over-long must be refused");
        assert!(err.contains("CATALOG_MAX_VENUE_BYTES"), "{err}");
        assert!(!err.contains(&over), "the refusal echoed the venue back");
    }

    #[test]
    fn an_empty_listing_and_a_no_bulk_list_refusal_are_different_values() {
        // Decision 5, held in the TYPE: never equal, never both rendered as "no instruments".
        let empty = CatalogListing {
            venue: "binance".into(),
            outcome: CatalogOutcome::Listed {
                instruments: Vec::new(),
                truncated: false,
                cached: false,
            },
        };
        let none = CatalogListing {
            venue: "ig".into(),
            outcome: CatalogOutcome::Refused(CatalogRefusal::NoBulkList {
                why: "searched live per query".into(),
            }),
        };
        assert_ne!(empty.outcome, none.outcome);
        assert!(empty.instruments().is_empty() && none.instruments().is_empty());
        // ...and they say different things to an operator, which is the property that matters.
        assert!(empty.describe().contains("0 instruments"), "{}", empty.describe());
        assert!(
            none.describe().contains("publishes no bulk instrument list"),
            "{}",
            none.describe()
        );
        assert!(!none.describe().contains("0 instruments"), "{}", none.describe());
    }

    /// Since `docs/decisions/0066` flipped the default, the sentence names the key the operator
    /// WROTE and never sends them looking for an arming that configures nothing.
    #[test]
    fn the_refused_sentence_names_the_key_the_operator_wrote() {
        let l = CatalogListing { venue: "okx".into(), outcome: CatalogOutcome::NotArmed };
        let s = l.describe();
        assert!(s.contains("venue_catalog_off"), "the file key: {s}");
        assert!(s.contains("VIKE_DATAHUB_VENUE_CATALOG_OFF=1"), "and the variable: {s}");
        assert!(s.contains("restart"), "{s}");
        assert!(s.contains("REFUSED"), "a refused server is not broken, it is configured: {s}");
        // The dead arming must not appear; checked on the whole `=1` token, because a bare
        // `contains` would match the prefix of the `_OFF` spelling.
        assert!(!s.contains("VIKE_DATAHUB_VENUE_CATALOG=1"), "the old arming is dead: {s}");
    }

    #[test]
    fn a_credential_refusal_never_suggests_arming_anything() {
        // Decision 3 is a property of what the table can contain, so the sentence must not send an
        // operator looking for a switch that does not exist.
        let s = CatalogRefusal::NeedsCredentials.describe("alpaca");
        assert!(s.contains("credentials"), "{s}");
        assert!(!s.contains("VIKE_DATAHUB_VENUE_CATALOG"), "no switch arms this: {s}");
        assert!(!s.contains("venue_catalog_off"), "and no refusal governs it either: {s}");
        assert!(s.contains("0062"), "the record is cited: {s}");
        // ⚠ …and it names WHAT WOULD list it (`docs/decisions/0066`'s decision 9): the ACT, on the
        // box that holds the keys, never a screen — a daemon prints this same string.
        assert!(s.contains("vike-backend catalog refresh alpaca"), "the act is named: {s}");
        assert!(!s.contains("Connections"), "a daemon must not name a desktop screen: {s}");
    }

    #[test]
    fn a_not_served_refusal_names_what_is_served() {
        let s = CatalogRefusal::NotServed { supported: vec!["binance".into(), "okx".into()] }
            .describe("dukascopy");
        assert!(s.contains("binance, okx"), "{s}");
    }

    #[test]
    fn a_truncated_listing_says_so_rather_than_being_silently_short() {
        let l = CatalogListing {
            venue: "polymarket".into(),
            outcome: CatalogOutcome::Listed {
                instruments: Vec::new(),
                truncated: true,
                cached: false,
            },
        };
        assert!(l.describe().contains("TRUNCATED"), "{}", l.describe());
        assert!(l.describe().contains("CATALOG_MAX_INSTRUMENTS"), "{}", l.describe());
    }
}
