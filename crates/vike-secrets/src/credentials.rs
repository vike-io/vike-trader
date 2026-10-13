//! What a caller does with an ACCOUNT's credentials: the generic HMAC loader ([`api_credentials`]),
//! the missing-passphrase finding, [`StoreHealth`] and the attribution code.
//!
//! [`api_credentials`] returns `None` unless the venue's REQUIRED field set is fully present in the
//! account's credentials: that absence IS the live gate (no credentials ⇒ the account does not arm).
//! HARD: a secret never reaches `Debug`, `Display` or any log line.
//!
//! # A credential belongs to its ACCOUNT (decision 0156 item 6)
//!
//! The account's credentials arrive as [`crate::account_credentials::AccountCredentials`] — the
//! store's `credential.account_id` + `field`, for the account the mount resolved — and a loader asks
//! it for a FIELD (`API_KEY`), never for a name. No name is composed here, no tier is read out of
//! one, and no label is appended to one: which account, at which tier, is the `account` row's, and
//! the mount picked the row before any loader ran. The generic set's three fields are
//! `vike_model::credential_keys`' [`API_KEY`], [`API_SECRET`] and [`API_PASSPHRASE`].
//!
//! ⚠ **"Required" is PER-VENUE.** key+secret is the required set for binance/bybit/deribit; OKX
//! also requires `API_PASSPHRASE`, because its signer sends one on every request
//! ([`venue_passphrase`](mod@crate::venue_passphrase)). Without that field an OKX account holding
//! two of the three would arm and fail every signed request at the venue.
//! [`missing_required_passphrase`] is how a mount names the missing field.
//!
//! # One store: the settings database, read through ONE handle
//!
//! This module does not read the store. Every read is `crate::CredentialStore`'s, opened by the
//! ROOT over the settings directory it resolved and the scope it parsed (`vike_boot` does both for
//! a booting root, `crate::open_credential_store` for one that runs no boot), and every read
//! is made under that handle's `crate::CredentialScope`. The ACCOUNT credentials reach a mount
//! inside `crate::account_directory::AccountDirectory`; the DEPLOYMENT names (attribution codes, a
//! builder-fee knob, the flags a root folds in) arrive as the name-keyed map
//! `crate::CredentialStore::read_all` returns, which [`attribution_code_from`] reads.
//!
//! A box with no settings database has NO credentials (every venue loader answers `None`).
//!
//! ⚠ Nothing in this workspace deletes, moves or rewrites the store. It is the user's only copy of
//! live venue credentials.

use std::collections::HashMap;

use vike_model::credential_keys::{
    API_KEY, API_PASSPHRASE, API_SECRET, BROKER_CODE_SUFFIX, BUILDER_CODE_SUFFIX, attribution_key,
};
use vike_model::secret::Secret;

use crate::account_credentials::AccountCredentials;
use crate::venue_passphrase::venue_passphrase;

/// One venue account's HMAC-style credential set. Each value is a
/// [`vike_model::secret::Secret`]: it is read through `.expose()` at the exact site that consumes
/// it (a signer, a request header) and never otherwise.
#[derive(Clone)]
pub struct Credentials {
    pub api_key: Secret<String>,
    pub api_secret: Secret<String>,
    pub passphrase: Option<Secret<String>>,
}

/// The hand-written rendering is KEPT (not `Secret`'s `<redacted>`): `api_credentials_tests`'
/// `the_credentials_never_reach_debug` pins the key's four-character tail and the passphrase's
/// `set`/`None`.
impl std::fmt::Debug for Credentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let key = self.api_key.expose();
        let tail = if key.len() >= 4 { &key[key.len() - 4..] } else { "" };
        write!(
            f,
            "Credentials(api_key=***{tail}, passphrase={})",
            if self.passphrase.is_some() { "set" } else { "None" }
        )
    }
}

/// **The generic HMAC set of ONE account**: its `API_KEY` + `API_SECRET` (+ `API_PASSPHRASE`), or
/// `None` when the venue's REQUIRED fields are not all present with non-blank values (the live
/// gate).
///
/// The required set is `API_KEY` + `API_SECRET`, **plus `API_PASSPHRASE` on a venue whose
/// [`venue_passphrase`] row is [`Required`](crate::venue_passphrase::PassphraseNeed::Required)**
/// (OKX). Unusable credentials must resolve exactly like absent ones: otherwise the account arms and
/// every signed request is rejected at the venue.
///
/// ⚠ **An INCOMPLETE set is an ABSENT one**, silently: the live gate's vocabulary is `Some`/`None`,
/// so there is no error, no log and no partial `Credentials`. SILENT by construction — the arming
/// projection runs it per account on every probe — so [`missing_required_passphrase`] returns the
/// finding as DATA and the venue's own mount turns it into a log line.
#[must_use]
pub fn api_credentials(creds: &AccountCredentials<'_>, venue: &str) -> Option<Credentials> {
    let api_key = creds.get(API_KEY)?;
    let api_secret = creds.get(API_SECRET)?;
    let passphrase = creds.get(API_PASSPHRASE);
    // The per-venue gate. `Optional`/`Unused` venues load with two credentials; only a `Required`
    // row refuses.
    if passphrase.is_none() && venue_passphrase(venue).is_required() {
        return None;
    }
    Some(Credentials {
        api_key: Secret::new(api_key.to_string()),
        api_secret: Secret::new(api_secret.to_string()),
        passphrase: passphrase.map(|p| Secret::new(p.to_string())),
    })
}

/// **The FIELD a half-configured account is missing**, or `None` when it is fully configured or not
/// configured at all.
///
/// `Some(API_PASSPHRASE)` means: `API_KEY` and `API_SECRET` are present and non-blank, the
/// [`venue_passphrase`] row is [`Required`](crate::venue_passphrase::PassphraseNeed::Required), and
/// the passphrase is unset or blank — so [`api_credentials`] returned `None` and the account does not
/// arm while the store LOOKS configured. That is the case a mount must say out loud.
///
/// Returns the field NAME, never a value. PURE and log-free.
#[must_use]
pub fn missing_required_passphrase(
    creds: &AccountCredentials<'_>,
    venue: &str,
) -> Option<&'static str> {
    let half_written = creds.get(API_KEY).is_some()
        && creds.get(API_SECRET).is_some()
        && creds.get(API_PASSPHRASE).is_none();
    (half_written && venue_passphrase(venue).is_required()).then_some(API_PASSPHRASE)
}

#[cfg(test)]
mod required_passphrase_tests;

#[cfg(test)]
mod api_credentials_tests;

/// **Whether the store a map came from could be OPENED** — handed back as DATA on every
/// `crate::Resolved` (and `crate::ScopedSecrets`), so a caller that degrades an unreadable store to
/// an empty map still knows it did.
///
/// ⚠ **Two ways to get an EMPTY map, and they are not the same event.** An ABSENT store is the
/// ordinary unconfigured state: `0 credentials` is a real measurement. A store that EXISTS and will
/// not open produces the SAME empty map with nothing measured — the root `CLAUDE.md` rule: *a store
/// that EXISTS and cannot be read is an ERROR, not "no credentials"*. `crate::Resolved::unreadable`
/// builds that empty answer with this arm set.
///
/// An EXEC path may collapse them (it degrades to paper either way). A path that RENDERS A COUNT
/// may not: [`StoreHealth::Unreadable`] is how it knows to render "unknown" instead of a zero.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum StoreHealth {
    /// The store ANSWERED. [`crate::Source::None`] — no store on this box — is this arm too: its empty map
    /// answers a question that was actually asked.
    #[default]
    Readable,
    /// The store EXISTS and could not be opened, so any count folded from the map is unknown, not
    /// zero. The text is [`crate::SecretsError`]'s `Display` — a path and an OS reason, never file
    /// contents — so it is safe to log, print and render verbatim.
    Unreadable(String),
}

impl StoreHealth {
    /// Whether the store answered at all — `true` for [`Self::Readable`], including the absent
    /// store. The negation is the state in which a count is not a measurement.
    #[must_use]
    pub fn is_readable(&self) -> bool {
        matches!(self, StoreHealth::Readable)
    }
}

/// Resolve a venue's attribution code from the loaded credential map (NOT the process
/// environment), validated against the venue's
/// [`vike_model::venues::attribution::AttributionMechanic`]. Tries `{VENUE}_BROKER_CODE`, then
/// `{VENUE}_BUILDER_CODE`. `None` when the key is absent or blank, the code fails the mechanic's
/// constraints, or the venue has no mechanic — so an unset or invalid code stamps nothing.
///
/// Both names come from [`vike_model::credential_keys::attribution_key`], so
/// [`vike_model::credential_keys::attribution_keys`] enumerates exactly what this reads — including
/// the mechanic-less early return, which is why a venue classified
/// [`None`](vike_model::venues::attribution::AttributionMechanic::None) contributes no key to that
/// grid and gets no registry row: its key provably cannot be looked up.
pub fn attribution_code_from(vars: &HashMap<String, String>, venue: &str) -> Option<String> {
    let mech = vike_model::venues::attribution::attribution_for(venue);
    if mech.is_none() {
        return None;
    }
    let code = vars
        .get(&attribution_key(venue, BROKER_CODE_SUFFIX))
        .or_else(|| vars.get(&attribution_key(venue, BUILDER_CODE_SUFFIX)))
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())?;
    mech.validate_code(code).ok()?;
    Some(code.to_string())
}

#[cfg(test)]
mod workspace_store_tests;

#[cfg(test)]
mod attribution_code_tests;
