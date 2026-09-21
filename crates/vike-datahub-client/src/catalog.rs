//! **The bounds and the ANSWER SHAPE for [`Request::VenueCatalog`](crate::proto::Request::VenueCatalog)
//! that BOTH ends need** — the venue validator, the listing cap, and the outcome enum that keeps
//! "this venue has no list" from ever looking like "this venue listed nothing".
//!
//! # Why the constants live HERE and not in `vike-datahub`
//!
//! The same rule [`crate::seed`] and [`crate::market`] state, for the same reason: a client cannot
//! guard a rule the server does not know, nor the reverse. The server's door is
//! `crates/vike-datahub/src/server.rs`'s `venue_catalog_verb` and the client's is
//! [`crate::DatahubClient::venue_catalog`]; both call [`validate_catalog_venue`], so a refusal the
//! operator sees locally is the refusal the server would have given. The constants that are the
//! SERVER's alone — the token bucket, the memo TTL — stay in `crates/vike-datahub/src/catalog.rs`,
//! because a client that knew a bucket's state could only ever mis-predict it.
//!
//! # ⚠ The outcome is an ENUM because an empty list is a LIE for two roster venues
//!
//! `docs/decisions/0062-a-venue-catalog-fetch-is-an-observe-verb-and-not-a-write.md`'s decision 5.
//! Two venues on `vike_model::VENUES` cannot be enumerated at all — `ig` is
//! `vike_catalog::CatalogMode::QueryBacked` (the trait's `list_instruments` default returns an empty
//! `Vec` for exactly that reason) and `ibkr` ships no `vike_catalog::CatalogProvider` whatsoever — so
//! a `Vec<Instrument>`-shaped answer would render in the Data Manager as a venue with no
//! instruments, which is false. It is the identical distinction the credential store already draws
//! between an ABSENT store and one that is present and unreadable, and it is drawn here in the TYPE
//! rather than in a convention, so a client cannot collapse it by accident.
//!
//! A [`CatalogOutcome::Listed`] carrying an EMPTY `instruments` stays expressible and means what it
//! says: the venue was asked and answered with nothing.

use serde::{Deserialize, Serialize};
use vike_catalog::Instrument;

/// The most instruments ONE listing may carry across the wire.
///
/// **Derived from the largest provider in the table, not picked.** The heaviest enumerable venue is
/// polymarket, whose own `crates/bridges/polymarket/src/catalog.rs` walks at most
/// `GAMMA_MAX_PAGES` (40) pages of `GAMMA_PAGE_LIMIT` (500) — **20,000 instruments, and that bound is
/// the BRIDGE's, compiled in, reachable by no request.** Everything else is far smaller: bybit is
/// 538 spot + 870 linear, binance two `exchangeInfo` documents.
///
/// 25,000 leaves the largest real provider its whole range plus a quarter of slack, so a venue that
/// grows does not silently start truncating at the first listing after it does.
///
/// ⚠ **It is NOT a frame-size bound and must not be confused for one.**
/// [`crate::proto::MAX_FRAME_LEN`] is 64 MiB and 25,000 instruments serialize to single-digit
/// megabytes, so this cap binds long before the frame does — which is the correct order. A cap
/// derived from the frame size would move whenever the frame limit moved, for reasons that have
/// nothing to do with how many instruments a venue lists.
///
/// A listing that hits it is TRUNCATED and says so ([`CatalogOutcome::Listed::truncated`]) rather
/// than being silently short — a picker missing its tail is a bug report nobody can reproduce.
pub const CATALOG_MAX_INSTRUMENTS: usize = 25_000;

// `CATALOG_MAX_INSTRUMENTS`' derivation, held at COMPILE TIME rather than asserted in prose:
// polymarket's provider walks at most `GAMMA_MAX_PAGES` (40) pages of `GAMMA_PAGE_LIMIT` (500), and
// the cap must leave the largest real provider its whole range or it silently starts truncating a
// venue in production. The `crates/vike-datahub/src/seed.rs` idiom — an inequality, so a deliberate
// tweak compiles and a broken claim does not. If that bridge ever raises its pager, this stops
// compiling and the author re-runs the derivation instead of quietly falsifying it.
const _: () = assert!(
    CATALOG_MAX_INSTRUMENTS > 40 * 500,
    "CATALOG_MAX_INSTRUMENTS must exceed the largest provider's OWN compiled bound (polymarket's \
     GAMMA_MAX_PAGES x GAMMA_PAGE_LIMIT = 20,000), or that venue's listing is truncated by this \
     cap rather than by its own pager. Re-run the derivation on the constant's doc."
);

/// The longest venue slug this verb will carry.
///
/// The longest entry on `vike_model::VENUES` is `hyperliquid` at 11 bytes; 16 is the next power of
/// two above it. The roster is the real bound — [`validate_catalog_venue`] refuses anything outside
/// the charset and the server then looks the slug up in its own table — so this constant exists to
/// bound the ALLOCATION a hostile frame can ask for before any of that runs, not to classify venues.
pub const CATALOG_MAX_VENUE_BYTES: usize = 16;

/// The bytes a venue slug may be built from: ASCII lowercase letters and digits only.
///
/// An ALLOWLIST, and deliberately narrower than the seed symbol's: every entry on
/// `vike_model::VENUES` is lowercase alphanumeric, so admitting anything else would admit a string
/// no real venue spells. Unlike a symbol, this value never reaches a URL — it selects a row in the
/// server's own table — so the check is a cheap early refusal rather than the security boundary
/// [`crate::seed::validate_seed_symbol`] is. It is still an allowlist, for the reason that one is:
/// the set of dangerous characters is open and the set of safe ones is three lines long.
fn is_catalog_venue_byte(b: u8) -> bool {
    b.is_ascii_lowercase() || b.is_ascii_digit()
}

/// The venue rules, cheapest first, each naming what was wrong.
///
/// ⚠ **The message never ECHOES the venue back**, the same rule and the same reason as
/// [`crate::seed::validate_seed_symbol`]: the field being refused is the untrusted one, a refusal
/// quoting it is that same untrusted string wearing a log line, and the file log layer defaults to
/// `trace`. It names the LENGTH, the CAP and the OFFSET, which is what an operator can act on.
///
/// ⚠ **A venue is NOT lower-cased or trimmed here.** The server looks the slug up in its own table
/// by exact match, so coercing it would make a client's request and the server's answer disagree
/// about which venue was asked for — the identical rule the seed and market-data validators state.
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
    /// **The venue has no bulk instrument list at all** — a fact about the VENUE, not about this
    /// server's build or configuration, and nothing an operator can change.
    ///
    /// Two roster venues are here and for two different reasons, both carried in `why`: `ig` is
    /// `vike_catalog::CatalogMode::QueryBacked` (searched live per query — there is no universe to
    /// hand over), and `ibkr` ships no `vike_catalog::CatalogProvider` whatsoever.
    ///
    /// ⚠ This is the variant that exists so an un-enumerable venue never renders as an EMPTY one.
    NoBulkList { why: String },
    /// **The venue can only be listed by spending the OPERATOR's credentials**, so this server will
    /// not do it on a client's say-so — `docs/decisions/0062`'s decision 3, which is a property of
    /// what the server's table can CONTAIN rather than a switch.
    ///
    /// alpaca, oanda and ctrader are here: each needs a fresh token, session or authed socket. A
    /// rate limit bounds spending a public budget; nothing bounds spending an identity, which is
    /// why this is refused by construction and why admitting one is a reopener rather than a
    /// configuration change.
    NeedsCredentials,
    /// **This server's table does not carry the venue** — a fact about the BUILD, and the one
    /// refusal that names what it CAN serve so an operator can tell a missing venue from a
    /// misspelled one.
    ///
    /// dukascopy and fxcm are the interesting members: their catalogs are BUNDLED STATIC lists
    /// costing zero venue requests, and they are absent only because the data daemon does not link
    /// those two bridge crates.
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
            // ⚠ **This sentence names the ACT, never a SCREEN**, and that is what keeps it true
            // from a daemon's mouth — this type is rendered by the datahub's own log, by the CLI
            // and by the desktop's Instruments grid from ONE spelling, and a daemon knows nothing
            // about a GUI. `docs/decisions/0066`'s decision 9 changed it from a bare refusal to a
            // refusal that names what WOULD list the venue, because "this cannot be refreshed" told
            // an operator holding the keys that their own box could not do what it now can.
            //
            // ⚠ It still names NO SERVER SWITCH, in either direction, and
            // `a_credential_refusal_never_suggests_arming_anything` is the fence: 0062's decision 3
            // is a property of what a server's table can CONTAIN, and a locally-credentialed
            // refresh is a different ACTOR rather than a wider server.
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
/// The states are mutually exclusive by construction, which is the point: `Listed { instruments:
/// [] }` and [`CatalogRefusal::NoBulkList`] are different values of different variants, so no client
/// can collapse them by forgetting to check a flag beside a vector.
/// ⚠ `PartialEq` but NOT `Eq`, and that is forced rather than chosen: `vike_catalog::Instrument`
/// carries a `vike_model::SymbolProperties` whose tick/lot fields are `f64`, so no type reachable
/// from a listing can be `Eq`. Every comparison this type needs is an equality between two values
/// one of which is a literal variant, which `PartialEq` answers.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum CatalogOutcome {
    /// The venue was asked (or a fresh memo answered) and here is its universe. An EMPTY
    /// `instruments` is a legitimate venue fact and means exactly that.
    Listed {
        instruments: Vec<Instrument>,
        /// The listing hit [`CATALOG_MAX_INSTRUMENTS`] and is SHORT. Never silently.
        truncated: bool,
        /// The server answered from its in-process memo and called no venue. Reported because it is
        /// the difference between "this list is seconds old" and "this list is up to the TTL old",
        /// which is the only thing an operator staring at a stale symbol wants to know.
        cached: bool,
    },
    /// **The server's operator REFUSED this lane**, so no venue was called and nothing was fetched
    /// — and this is a SUCCESS, not an error, exactly as `crate::seed`'s `SeedDone { armed: false }`
    /// is.
    ///
    /// ⚠ **The REASON this arrives changed on 2026-09-16 and the variant name did not.** Until
    /// `docs/decisions/0066-the-venue-catalog-is-on-by-default-and-the-switch-is-its-refusal.md`
    /// this was the DEFAULT — the lane was armed by `VIKE_DATAHUB_VENUE_CATALOG=1` and an unarmed
    /// server was the ordinary unconfigured one. The catalog is now ON by default, so a server
    /// answering this has a written `venue_catalog_off = true` in its `flags.toml` (or
    /// `VIKE_DATAHUB_VENUE_CATALOG_OFF=1`). The NAME is still accurate about the SERVER STATE — no
    /// lane is armed — which is why it is kept rather than renamed across a wire both ends spell.
    ///
    /// ⚠ A client must not treat this as a failure: the server is behaving exactly as its operator
    /// told it to. It IS worth telling the operator about, because from the picker's side a
    /// refused server and a venue with no instruments look identical.
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
                 `venue_catalog_off = true` in <project>/settings/flags.toml, or \
                 VIKE_DATAHUB_VENUE_CATALOG_OFF=1. Delete that and restart it to serve again. \
                 Nothing was fetched for `{}`, and that is a written refusal rather than a \
                 failure: the catalog is ON by default.",
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
        // The claim `CATALOG_MAX_VENUE_BYTES` and the charset both rest on, asserted over the real
        // roster rather than over a hand-copied list — so a venue added with an unusual slug
        // reddens HERE rather than being refused at a server door nobody is watching.
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
        // Decision 5, held in the TYPE. This is the whole reason `CatalogOutcome` is an enum: these
        // two must never compare equal and must never both render as "no instruments".
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

    /// ⚠ This was `the_unarmed_sentence_names_the_switch` and asserted the OPPOSITE fact — that
    /// the sentence names `VIKE_DATAHUB_VENUE_CATALOG=1` and calls the state the server's DEFAULT.
    /// `docs/decisions/0066` flipped the default, so the sentence has to name the key the operator
    /// WROTE and must not send them looking for an arming that configures nothing.
    #[test]
    fn the_refused_sentence_names_the_key_the_operator_wrote() {
        let l = CatalogListing { venue: "okx".into(), outcome: CatalogOutcome::NotArmed };
        let s = l.describe();
        assert!(s.contains("venue_catalog_off"), "the file key: {s}");
        assert!(s.contains("VIKE_DATAHUB_VENUE_CATALOG_OFF=1"), "and the variable: {s}");
        assert!(s.contains("restart"), "{s}");
        assert!(s.contains("REFUSED"), "a refused server is not broken, it is configured: {s}");
        // The OLD arming must not appear: it configures nothing now, and `vike_config::load` only
        // WARNS about it. `contains` would match the prefix of the `_OFF` spelling, so the check is
        // on the whole `=1` token that would have told an operator to set it.
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
        // ⚠ …and since `docs/decisions/0066`'s decision 9 it names WHAT WOULD list it — the ACT,
        // on the box that holds the keys — because a refusal that names nothing told an operator
        // who HAS the credentials that nothing could be done. It must stay an act rather than a
        // screen: this same string is printed by a daemon that has no GUI to send anybody to.
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
