//! `SchemaRefusal`: one reason a schema-2 write refused a key. Names a KEY, never a value.

use super::*;

// ---------------------------------------------------------------------------------------------
// Refusals
// ---------------------------------------------------------------------------------------------

/// One reason a schema-2 write refused. **Names a KEY, never a value.**
///
/// Same contract as [`crate::Ambiguity`], and for the same reason: an operator has to be able to
/// read a refusal out of a log without the log becoming a credential.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum SchemaRefusal {
    /// The classifier returned a `field` that is not a suffix of the NAME **for a row whose
    /// account can ONLY be found by its owner prefix** (`Classification::needs_owner_prefix` —
    /// unlabelled, and discriminated, i.e. dukascopy's shape), so §4.4's derivation (*the name with
    /// its owner prefix removed*) did not hold and there is no second way to reach the account.
    /// A classifier bug, refused rather than papered over with a guessed prefix.
    ///
    /// ⚠ **It is NOT raised for every non-suffix `field`, and this doc said it was.** A LABELLED
    /// key's `field` is legitimately not a suffix of its name (the `__LABEL` sits after it), so a
    /// blanket refusal would refuse the whole labelled grammar; every other row survives a missing
    /// prefix because `(venue, tier, label)` still answers for it. See
    /// [`Classification::owner_prefix`], whose own doc carried the same overstatement.
    FieldIsNotASuffix {
        /// The key name.
        key: String,
        /// What the classifier said `field` was.
        field: String,
    },
    /// The classifier named a tier outside [`ACCOUNT_TIERS`]. Refused HERE rather than left to the
    /// `CHECK`, so the message names the KEY rather than an opaque constraint failure.
    UnknownTier {
        /// The key name.
        key: String,
        /// The tier the classifier answered.
        tier: String,
    },
    /// More than one unlabelled `account` row already exists for a `(venue, tier)` the classifier
    /// offered no discriminator for, so *which account is this key's?* has more than one answer and
    /// nothing here may pick. See [`AccountKey::discriminator`].
    AmbiguousAccount {
        /// The key name.
        key: String,
        /// The venue.
        venue: String,
        /// The tier.
        tier: String,
    },
    /// A commented-out `#KEY=VALUE` line names a key with no LIVE row in this store, so there is
    /// nothing it can be the superseded value OF. The line is REPORTED and NOT written — see
    /// [`FileComments`].
    SupersededKeyIsNotInTheStore {
        /// The key name. Never the commented value.
        key: String,
    },
    /// **Two credential NAMES resolve to one `(account_id, field)` and their VALUES DISAGREE.**
    ///
    /// The live case is a tier ALIAS: `vike_model::accounts::account_keys` normalizes the legacy `MAINNET`
    /// tier onto `LIVE`, so `{VENUE}_LIVE_API_KEY` and `{VENUE}_MAINNET_API_KEY` classify to the
    /// same account and — the store's own tier token being what §4.4 removes — to the same `field`.
    /// One credential, two spellings. `credential_one_live_value` says a live `(account, field)`
    /// has ONE value, and when the two spellings carry DIFFERENT values there is no answer here
    /// that is not a guess: whichever row were made live would silently decide which key a venue
    /// signs orders with.
    ///
    /// So neither is written, BOTH names ride the refusal, and every other key in the run lands.
    /// The repair is an operator edit — delete one of the two lines — which is exactly the shape
    /// every other per-key refusal in this module takes. When the two values are IDENTICAL nothing
    /// is refused at all: see [`RowReport::aliases`].
    CollidingLiveValues {
        /// The key name this row was being written for. Never its value.
        key: String,
        /// The name already holding the live row for the same `(account_id, field)`. Never its
        /// value.
        other: String,
        /// The `field` both of them derive.
        field: String,
    },
    /// **The classifier filed the key under a VENUE the store's `venue` table does not hold.**
    ///
    /// A venue-scoped `credential` row names its venue by `venue_id` alone since the venue-links
    /// plan's second release, and [`VenueLink::values`] looks the number up by name in the same
    /// statement — so for a venue off the roster it would write a NULL, and with no text column
    /// beside it the row would name no venue at all and read as an INFRASTRUCTURE key, silently.
    /// (On the first release's shape the same row kept its text with a NULL number, which the
    /// second release's drop pass then refuses for every write.) Refused here, by name, instead.
    /// The roster is `vike_model::VENUES`, which the write funnel tops the table up from,
    /// so only a classifier answering a venue the roster does not know reaches it.
    VenueNotOnRoster {
        /// The key name. Never its value.
        key: String,
        /// The venue the classifier answered.
        venue: String,
    },
}

impl SchemaRefusal {
    /// The key NAME this refusal is about. Every variant has one; none of them has a value.
    #[must_use]
    pub fn key(&self) -> &str {
        match self {
            SchemaRefusal::FieldIsNotASuffix { key, .. }
            | SchemaRefusal::UnknownTier { key, .. }
            | SchemaRefusal::AmbiguousAccount { key, .. }
            | SchemaRefusal::SupersededKeyIsNotInTheStore { key }
            | SchemaRefusal::CollidingLiveValues { key, .. }
            | SchemaRefusal::VenueNotOnRoster { key, .. } => key,
        }
    }
}

impl std::fmt::Display for SchemaRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SchemaRefusal::FieldIsNotASuffix { key, field } => write!(
                f,
                "{key}: the classifier answered field {field:?}, which is not the end of that \
                 name — the owner prefix a schema-2 row is keyed on cannot be taken from it, and \
                 nothing here will guess one"
            ),
            SchemaRefusal::UnknownTier { key, tier } => write!(
                f,
                "{key}: the classifier answered tier {tier:?}, which is not one of {:?} — an \
                 account row carrying it could not be joined to a venue arming ceiling",
                ACCOUNT_TIERS
            ),
            SchemaRefusal::AmbiguousAccount { key, venue, tier } => write!(
                f,
                "{key}: more than one unlabelled {venue} account already exists at tier {tier} and \
                 the classifier offered no discriminator, so which one this key belongs to has \
                 more than one answer — nothing here will pick"
            ),
            SchemaRefusal::SupersededKeyIsNotInTheStore { key } => write!(
                f,
                "{key} appears as a commented-out assignment in the credential file but has no \
                 live row in this store, so it is not the superseded value OF anything — the line \
                 is reported and nothing was written from it"
            ),
            SchemaRefusal::CollidingLiveValues { key, other, field } => write!(
                f,
                "{key} and {other} are two spellings of ONE credential — they resolve to the same \
                 account and the same field {field:?} — and they carry DIFFERENT values. A live \
                 (account, field) has one value, and nothing here will pick which of the two a \
                 venue signs orders with. NEITHER was written; delete one of the two lines and \
                 run again"
            ),
            SchemaRefusal::VenueNotOnRoster { key, venue } => write!(
                f,
                "{key}: the classifier filed this key under venue {venue:?}, which this store's \
                 venue table does not hold, so the row would name no venue at all and read as an \
                 infrastructure key — it was not written"
            ),
        }
    }
}
