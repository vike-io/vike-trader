//! **One roster venue, as the composition root hands it to this crate.** `vike_tradehub::registry`'s
//! `REGISTRY` is a table of these; every function here that dispatches on a venue takes that
//! table rather than naming a venue itself (docs/decisions/0096).

use vike_bridge_core::venue_mount::{BookIdentity, ClockDecl, VenueMount};

/// Why a `FeatureAbsent` venue has no clock leg — [`ABSENT_CLOCK`]'s reason, named so the clock
/// read can answer `NotChecked` with it directly.
pub(crate) const ABSENT_CLOCK_REASON: &str = "this build does not compile the venue's bridge (its Cargo feature is off), so nothing \
     here can mount it live and there is no clock leg to read";

/// The clock row a `FeatureAbsent` venue answers with. Generic on purpose: the venue's own row
/// lives in its bridge, which this build does not compile. Unobservable in production — such a
/// venue is always paper, so the clock leg never lists it.
pub(crate) const ABSENT_CLOCK: ClockDecl =
    ClockDecl::NotWired { reason: ABSENT_CLOCK_REASON, unmeasured_risk: None };

/// The book-identity row a `FeatureAbsent` venue answers with — `Undeterminable`, the answer
/// that emits no shared-book warning. Unobservable in production: `effective_book` answers `None`
/// for a paper account before it reads a row, and such a venue is always paper.
pub(crate) const ABSENT_BOOK: BookIdentity = BookIdentity::Undeterminable {
    why: "this build does not compile the venue's bridge (its Cargo feature is off), so no \
          account of it can arm and nothing here can read its credential shape",
};

/// One row per `vike_model::VENUES` id.
#[derive(Clone, Copy)]
pub enum VenueRow {
    /// The venue's bridge implements the mount contract.
    Mount(&'static dyn VenueMount),
    /// This build does not compile the venue's bridge: `feature` is off. The arming screen reads
    /// `FeatureAbsent` and the mount is paper — the same answer in every build.
    FeatureAbsent { venue: &'static str, feature: &'static str },
}

impl VenueRow {
    /// The venue id this row answers for.
    #[must_use]
    pub fn venue(&self) -> &'static str {
        match *self {
            VenueRow::Mount(m) => m.venue(),
            VenueRow::FeatureAbsent { venue, .. } => venue,
        }
    }
}

impl std::fmt::Debug for VenueRow {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            VenueRow::Mount(m) => write!(f, "Mount({})", m.venue()),
            VenueRow::FeatureAbsent { venue, feature } => {
                write!(f, "FeatureAbsent({venue}, feature `{feature}`)")
            }
        }
    }
}

/// The row for `venue`, or `None` for an id the registry does not carry (a test's `"sim"`).
#[must_use]
pub fn row_of(registry: &'static [VenueRow], venue: &str) -> Option<&'static VenueRow> {
    registry.iter().find(|r| r.venue() == venue)
}
