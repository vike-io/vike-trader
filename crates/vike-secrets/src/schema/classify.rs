//! The injected classification seam: `AccountKey`, `Placement` and `Classification`.

#[cfg(doc)]
use super::*;

/// **Which account a credential name belongs to**, as the classifier reports it.
///
/// `venue`/`tier`/`label` are `vike_model::accounts::account_keys::AccountRef`'s three fields — §4.1's
/// `UNIQUE (venue, tier, label)` is that tuple and not an invention — carried as owned `String`s
/// because this crate cannot name that type.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct AccountKey {
    /// A `vike_model::VENUES` id.
    pub venue: String,
    /// One of [`ACCOUNT_TIERS`].
    pub tier: String,
    /// The operator's name for the ROLE. **`None` is the ordinary answer and is what the credential
    /// writer files for every account it creates**, because `AccountLabel::Default` renders as ABSENT
    /// everywhere else in this workspace — its serde impl skips it, and
    /// `AccountLabel::parse("DEFAULT")` REFUSES the reserved spelling outright, so the literal
    /// string would not round-trip.
    pub label: Option<String>,
    /// ⚠ **Derivation-time only — it is stored in NO column.**
    ///
    /// Two accounts of one venue at one tier are indistinguishable by `(venue, tier, label)` when
    /// both labels are absent, and dukascopy is exactly that case: `DEMO1` and `DEMO2` are two
    /// accounts (ruling 1) and the owner ruled that neither gets a label. This field lets the
    /// classifier's HAND-MAP say *these two names belong to different accounts* without putting the
    /// index token into a column — which is the defect §1 is about, and §11.1's own rejected
    /// alternative. `None` for every other venue.
    pub discriminator: Option<String>,
}

/// Where a credential row sits — §5.1's three cases, as the pair `(account_id, venue)` expresses
/// them. **The pair IS the classification; there is no separate kind column to keep in step.**
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Placement {
    /// `account_id` set, `venue` NULL — the venue is on the account row. `OKX_DEMO_API_SECRET`.
    Account(AccountKey),
    /// `account_id` NULL, `venue` set — an APPLICATION credential shared by every account of the
    /// venue. `CTRADER_CLIENT_ID`. ⚠ `venue` here means *belongs to this venue's plane*, not
    /// *issued by this venue*.
    Venue(String),
    /// Both NULL — the deployment's own credentials. `CLOUDFLARE_API_TOKEN`.
    Infrastructure,
}

/// **What one credential NAME is**, as the injected classifier answers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Classification {
    /// §5.1's three cases.
    pub placement: Placement,
    /// §4.4 — the name with its OWNER PREFIX removed; the WHOLE name for an infrastructure row,
    /// whose owner is the deployment.
    pub field: String,
    /// `false` for the rows §6 MEASURED as holding no secret. `true` everywhere else: a value
    /// wrongly marked non-secret is a worse error than one wrongly marked secret.
    pub secret: bool,
    /// `false` when the classifier could not place this name and fell back to
    /// [`Placement::Infrastructure`]. The row is still written with `name` and `value` VERBATIM —
    /// **a name the parse cannot classify is never dropped and never guessed at** — and it is
    /// REPORTED by name (§11.1).
    pub recognised: bool,
}

impl Classification {
    /// The unrecognised fallback — §11 step 6, and the shape a classifier returns when it has
    /// nothing to say about a name.
    #[must_use]
    pub fn unrecognised(name: &str) -> Classification {
        Classification {
            placement: Placement::Infrastructure,
            field: name.to_string(),
            secret: true,
            recognised: false,
        }
    }

    /// The OWNER PREFIX this classification implies — the name with [`Classification::field`]
    /// removed from its end.
    ///
    /// ⚠ **This is the account's re-derivable identity ACROSS RUNS**, and it is why the store
    /// needs no stored discriminator. Every credential row keeps its legacy `name` (§4.1: *"`name`
    /// is the only record"*) and its `field`, so an account's owner prefix — `DUKASCOPY_DEMO1_` —
    /// is recoverable from any one of its own rows. A later run therefore finds the account an
    /// earlier run created, without either of them putting the index token in a column.
    ///
    /// ⚠ `None` for a LABELLED account, and that is not a failure: a labelled key's `field` is not
    /// a suffix of its name (the `__LABEL` sits after it), and a labelled account does not need
    /// this lookup — `(venue, tier, label)` is unique for it, so [`AccountResolver`]'s SECOND
    /// lookup answers. The prefix exists for the one case nothing else can answer: an UNLABELLED
    /// account that shares `(venue, tier, label)` with another, i.e. dukascopy's pair.
    ///
    /// ⚠ It is ALSO `None` when a classifier contradicts itself by naming a `field` that is no
    /// part of the name's end — and **that is refused only where it is not survivable**, which is
    /// the discriminated-unlabelled shape above. This doc said it "IS refused" flatly, and
    /// [`SchemaRefusal::FieldIsNotASuffix`]'s own doc said the same; both were wider than
    /// [`write_rows`], which raises that refusal under
    /// `Classification::needs_owner_prefix` alone. A blanket refusal is not
    /// available and never was: a LABELLED key's `field` is legitimately not a suffix of its name,
    /// so refusing every non-suffix would refuse the whole labelled grammar.
    #[must_use]
    pub fn owner_prefix<'a>(&self, name: &'a str) -> Option<&'a str> {
        name.strip_suffix(self.field.as_str())
    }

    /// Is this an account-scoped row whose account can ONLY be found by its owner prefix?
    ///
    /// True exactly when the account is unlabelled AND the classifier offered a discriminator —
    /// the dukascopy shape, where `(venue, tier, label)` has two answers and the discriminator
    /// reaches no column. For every other row a missing prefix is survivable.
    pub(super) fn needs_owner_prefix(&self) -> bool {
        matches!(
            &self.placement,
            Placement::Account(key) if key.label.is_none() && key.discriminator.is_some()
        )
    }
}
