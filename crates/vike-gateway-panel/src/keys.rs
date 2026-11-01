//! The credential FORMS, by FIELD, and the Sensitivity classifier — pure, no egui.
//!
//! # A credential belongs to its ACCOUNT, and a form names FIELDS (decision 0156 item 6)
//!
//! A tier cell of the panel is the account ROW of `(venue, label, tier)`, and its form writes that
//! row's credentials — the store's `credential.account_id` + `field` — through
//! `vike_secrets::save_owned_credentials`. Nothing here composes, parses or returns a credential
//! NAME: the field words are the venue's row of `vike_venues::credential_schema` (`schema_for`), the
//! ONE statement of what each venue's loader reads, so the form cannot offer a field no loader
//! reads and cannot miss one a loader requires. The tier is the account row's (`account.tier`,
//! written only by the operator); no form here carries one.
//!
//! Three kinds of field, [`FieldTarget`]:
//!
//! * an ACCOUNT field — `schema.form(FieldOwner::Account)`, written to the account row;
//! * a VENUE application field — `schema.form(FieldOwner::Venue)` (cTrader's OAuth client pair),
//!   written once for every account of the venue and shown once per venue, never per tier;
//! * a DEPLOYMENT row — the venue-wide attribution tail on the LIVE form ([`form_fields`]), a
//!   tier-less name `vike_secrets::credentials::attribution_code_from` reads by NAME. It belongs to
//!   no account, which is why it keeps its name.
//!
//! The PAPER tier has no form at all: the paper simulator reads no credential.

use vike_model::AccountTier;
use vike_venues::credential_keys;
use vike_venues::credential_schema::{FieldOwner, KeyField, schema_for};

/// **Which form** — a tier cell's (the account row of that tier) or the venue's application group.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FormPlace {
    /// The tier cell of the account selected in the strip: its account row's fields.
    Tier(AccountTier),
    /// The venue's APPLICATION credentials (`FieldOwner::Venue`), one set every account shares.
    VenueApp,
}

/// **Where one form field's value is written.**
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FieldTarget {
    /// A field of the account row the cell is — `credential.account_id` + `field`.
    Account(&'static KeyField),
    /// A venue application field — `credential.venue_id` + `field`.
    Venue(&'static KeyField),
    /// A DEPLOYMENT row, read and written by its NAME: the venue-wide attribution tail. The name is
    /// `vike_venues::credential_keys::attribution_var_for`'s (or one of the two fee knobs), never one
    /// composed here.
    Deployment(String),
}

/// One field of a credential form: the label the operator reads and where its value goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FormField {
    /// The UI label (the schema row's, or the attribution tail's own).
    pub label: &'static str,
    /// Where a Save writes it.
    pub target: FieldTarget,
}

impl FormField {
    /// **The field's grey text** — the FIELD word it writes (`API_KEY`), or a deployment row's NAME.
    #[must_use]
    pub fn hint(&self) -> &str {
        match &self.target {
            FieldTarget::Account(f) | FieldTarget::Venue(f) => f.field,
            FieldTarget::Deployment(name) => name,
        }
    }

    /// Whether the venue's loader refuses without it — the schema's `required`; a deployment row
    /// (the attribution tail) is always optional.
    #[must_use]
    pub fn required(&self) -> bool {
        match &self.target {
            FieldTarget::Account(f) | FieldTarget::Venue(f) => f.required,
            FieldTarget::Deployment(_) => false,
        }
    }

    /// [`Sensitivity`] of this field: the schema's `secret` for an account or venue field, and
    /// [`is_venue_wide_key`] for a deployment row. ⚠ Fails CLOSED: a deployment name outside the
    /// attribution family is a [`Sensitivity::Secret`].
    #[must_use]
    pub fn sensitivity(&self) -> Sensitivity {
        match &self.target {
            FieldTarget::Account(f) | FieldTarget::Venue(f) => sensitivity_of(f),
            FieldTarget::Deployment(name) if is_venue_wide_key(name) => Sensitivity::Public,
            FieldTarget::Deployment(_) => Sensitivity::Secret,
        }
    }
}

/// Whether `venue` has an account form at all — a schema row with at least one field the form
/// writes. A venue without one has no tier cell to configure.
#[must_use]
pub fn has_account_form(venue: &str) -> bool {
    schema_for(venue).is_some_and(|s| !s.form(FieldOwner::Account).is_empty())
}

/// **The fields one form writes, in the form's order.**
///
/// * `FormPlace::Tier(Paper)` — EMPTY: the paper simulator reads no credential.
/// * `FormPlace::Tier(Demo | Live)` — the schema row's `form(FieldOwner::Account)`; the LIVE form
///   appends the venue-wide attribution tail ([`attribution_fields`]).
/// * `FormPlace::VenueApp` — the schema row's `form(FieldOwner::Venue)`; empty for every venue
///   without an application credential.
///
/// A venue the schema does not carry has no form anywhere (fails closed: no field is invented).
#[must_use]
pub fn form_fields(venue: &str, place: FormPlace) -> Vec<FormField> {
    let Some(schema) = schema_for(venue) else { return Vec::new() };
    match place {
        FormPlace::Tier(AccountTier::Paper) => Vec::new(),
        FormPlace::Tier(tier) => {
            let mut out: Vec<FormField> = schema
                .form(FieldOwner::Account)
                .into_iter()
                .map(|f| FormField { label: f.label, target: FieldTarget::Account(f) })
                .collect();
            // ⚠ Appended ONLY to a form that exists, and only on LIVE — see `attribution_fields`.
            if tier == AccountTier::Live && !out.is_empty() {
                out.extend(attribution_fields(venue));
            }
            out
        }
        FormPlace::VenueApp => schema
            .form(FieldOwner::Venue)
            .into_iter()
            .map(|f| FormField { label: f.label, target: FieldTarget::Venue(f) })
            .collect(),
    }
}

/// **The VENUE-WIDE attribution tail**, appended by [`form_fields`] to the LIVE form of every venue
/// with an order-level mechanic — the affiliate/builder tag `vike_secrets::credentials::
/// attribution_code_from` stamps on outgoing orders, plus the one fee knob two of those venues
/// carry.
///
/// ⚠ **Three properties, and each of them is a decision rather than a detail.**
///
/// * **LIVE only.** These names carry NO tier — one name serves every account of the venue — so a
///   field on every form would be several controls over one value, and an operator editing a demo
///   account would be changing what a real order is tagged with. It is rendered where real orders
///   are configured, and every label says `venue-wide` so the scope is on screen.
/// * **A DEPLOYMENT row, not an account credential.** `attribution_code_from` reads the bare
///   `{VENUE}_BROKER_CODE` by NAME out of the deployment map and takes no account at all, so it is
///   written by NAME (`vike_secrets::save_credentials_to_store`, which files a new name only as a
///   deployment row) and never against the account row.
/// * **The NAME is `vike_venues::credential_keys::attribution_var_for`'s answer, never a per-venue
///   list here.** A `SignedBuilder` venue takes `_BUILDER_CODE` and the CEX mechanics take
///   `_BROKER_CODE` — the split `crates/vike-bridge-core/CLAUDE.md`'s attribution-codes bullet
///   names venue by venue. ⚠ It lives in `vike-model` rather than here for a second reason:
///   `crates/vike-ops/tests/settings_secrets/settings_registry/walk_and_grid.rs`'s
///   `generated_key_sites` reads a call to `attribution_key` as *this crate reads the whole grid*.
///
/// The two fee knobs are spelled out because they are spelled out in the tree: they are two
/// different names (`HYPERLIQUID_BUILDER_FEE_TENTHS_BP` is tenths of a basis point,
/// `ASTER_BUILDER_FEE_RATE` is a rate string), each read by `vike-tradehub-core` at its own call site.
fn attribution_fields(venue: &str) -> Vec<FormField> {
    let Some(code) = credential_keys::attribution_var_for(venue) else {
        return Vec::new();
    };
    let label = if code.ends_with(credential_keys::BUILDER_CODE_SUFFIX) {
        "Builder Code (venue-wide, optional)"
    } else {
        "Broker Code (venue-wide, optional)"
    };
    let mut out = vec![FormField { label, target: FieldTarget::Deployment(code) }];
    match venue {
        "hyperliquid" => out.push(FormField {
            label: "Builder Fee, tenths of a bp (venue-wide, optional)",
            target: FieldTarget::Deployment("HYPERLIQUID_BUILDER_FEE_TENTHS_BP".to_string()),
        }),
        "aster" => out.push(FormField {
            label: "Builder Fee Rate (venue-wide, optional)",
            target: FieldTarget::Deployment("ASTER_BUILDER_FEE_RATE".to_string()),
        }),
        _ => {}
    }
    out
}

/// Is `name` a DEPLOYMENT name that belongs to the VENUE rather than to any account — the
/// attribution family and nothing else? See [`attribution_fields`]. Matched on the SUFFIX of a name
/// `attribution_var_for` (or the fee-knob literals above) produced; no account name is ever asked.
#[must_use]
pub fn is_venue_wide_key(name: &str) -> bool {
    name.ends_with(credential_keys::BROKER_CODE_SUFFIX)
        || name.ends_with(credential_keys::BUILDER_CODE_SUFFIX)
        || name.ends_with("_BUILDER_FEE_RATE")
        || name.ends_with("_BUILDER_FEE_TENTHS_BP")
}

/// **What knowing a field's value would let somebody DO** — the one axis that decides whether the
/// form masks it and whether the panel is allowed to show what the store already holds.
///
/// ⚠ This is not a taste question and it is not per-venue. THE RULE: a value is [`Self::Secret`]
/// when possessing it is enough to ACT AS the account — a key, a secret, a passphrase, a password,
/// a private key, an OAuth token. Everything else NAMES something — an account, a login, an
/// address, an attribution tag — and is the half an operator has to be able to read back to answer
/// *"which account is this, and where is it pointing"*. The schema's `KeyField::secret` states it
/// per field (`vike_venues::credential_schema`'s module doc: "`secret` is the GUI's masking rule").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sensitivity {
    /// Masked, never prefilled, never read back into the UI under any circumstances.
    Secret,
    /// Rendered as ordinary text and PREFILLED from the store, so the operator can see and correct
    /// what is there.
    Public,
}

/// The schema field's own `secret`, as a [`Sensitivity`].
fn sensitivity_of(field: &KeyField) -> Sensitivity {
    if field.secret { Sensitivity::Secret } else { Sensitivity::Public }
}

/// [`Sensitivity`] for one stored FIELD of `venue` — the venue's schema row's `secret`.
///
/// ⚠ **Unclassified FAILS CLOSED**, and that is the property the whole thing rests on: a field the
/// venue's schema row does not carry (a venue the schema does not know, a field word nothing
/// declares) is a [`Sensitivity::Secret`], so a value nobody classified is masked and unread. The
/// opposite default would make "forgot to think about it" mean "rendered on screen".
#[must_use]
pub fn field_sensitivity(venue: &str, field: &str) -> Sensitivity {
    schema_for(venue).and_then(|s| s.field(field)).map_or(Sensitivity::Secret, sensitivity_of)
}

/// A per-form qualification rendered under [`crate::view::MASKED_FIELD_HINT`], or `None` for the
/// forms that need none.
///
/// ⚠ **Dukascopy's DEMO form names a value this form does NOT write.** Its JForex server is the demo
/// tier's `venue.dukascopy.demo.server` SETTING (decision 0095's Task 7) — one per machine and tier,
/// read from the settings database by the mount and by nothing in the credential store. So the form
/// says where the value lives instead of offering a control that would write a row nothing reads.
///
/// ⚠ The `.jnlp` condition is carried over unchanged: `vike_dukascopy::exec`'s
/// `DukascopyExec::spawn_with_program` keeps the stored value ONLY when it ends in `.jnlp`, and
/// otherwise warns and hands the sidecar its own `DEFAULT_DEMO_JNLP`. Nothing exercises a stored
/// server end to end — `crates/bridges/dukascopy/tests/dukascopy_live_smoke.rs`'s `live_config`
/// force-blanks `cfg.server` before every login.
#[must_use]
pub fn form_note(venue: &str, place: FormPlace) -> Option<&'static str> {
    match (venue, place) {
        ("dukascopy", FormPlace::Tier(AccountTier::Demo)) => Some(
            "JNLP server: a venue setting — `vike-cli config set venue.dukascopy.demo.server \
             <url>`. Only a value ending in `.jnlp` is used; anything else is ignored and the \
             built-in demo JNLP is used instead. No live smoke exercises a stored JNLP URL.",
        ),
        _ => None,
    }
}

#[path = "keys_tests.rs"]
#[cfg(test)]
mod keys_tests;
