//! The ONE cross-venue authority on **whether a venue's API credentials require a passphrase** —
//! the third field of [`Credentials`](crate::credentials::Credentials), and the one field the
//! generic loader used to treat as decoration.
//!
//! # Why this table exists
//!
//! `load_credentials_from` required only `_API_KEY` + `_API_SECRET`. An OKX store holding those
//! two and NO `OKX_{TIER}_API_PASSPHRASE` therefore LOADED, the venue mounted **live**, and every
//! signed request then failed at the venue (`OK-ACCESS-PASSPHRASE` sent empty ⇒ OKX 50104). That
//! is the live gate admitting exactly the state it exists to prevent: **absent credentials ARE the
//! live gate**, and a half-credential is absent credentials wearing a complete one's clothes.
//!
//! The cure is NOT a blanket requirement — binance and bybit sign with no passphrase at all, and
//! demanding one everywhere would strand every working mount on paper. "This venue requires a
//! passphrase" is a PER-VENUE fact, so it gets the per-venue capability-map treatment the repo
//! uses everywhere else (`vike_model::caps_for`, `vike_model::venue_margin_support`,
//! `vike_model::fees`'s `fee_schedule_for`, and this crate's own `tif`'s `venue_tif`): one row per
//! venue citing the adapter code it was read FROM, a verbatim matrix pin, and a completeness test
//! iterating `vike_model::VENUES` so a new bridge crate cannot ship unclassified.
//!
//! # What a row means
//!
//! Each row answers ONE question — *what does this venue's live path do with
//! `Credentials::passphrase`?* — and only [`PassphraseNeed::Required`] gates the loader. The other
//! two variants are declarations that the venue was CLASSIFIED, not forgotten (the roster-gate
//! philosophy: a named row even where the value equals the fallback).
//!
//! ⚠ **A row is about the SHARED `Credentials` shape, not about the venue having a secret called
//! "passphrase" somewhere.** Polymarket has an L2 passphrase and is still `Unused` here: that trio
//! is DERIVED at mount into its own `PolymarketCreds`, never loaded through
//! `load_credentials_from`, and its own missing-field gate already exists
//! (`crates/bridges/polymarket/src/l1.rs`'s `parse_derived` names the absent field and refuses).
//!
//! # Adding a venue
//!
//! Read the venue's signer/transport — not its `.env` file, and not the credential names that
//! happen to be in some store. The question is whether the adapter PUTS the passphrase on the wire
//! and the venue rejects a blank one.

/// What one venue's live path does with [`Credentials::passphrase`](crate::credentials::Credentials).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PassphraseNeed {
    /// The venue's signer puts the passphrase on the wire on EVERY authenticated request, and the
    /// venue rejects a blank one. Absent ⇒ the credentials are unusable, so
    /// `load_credentials_from` refuses them and the venue stays PAPER.
    Required,
    /// The adapter READS the field but works without it — it derives or defaults the value. A
    /// blank passphrase is a fully working configuration, so it must never gate the loader.
    Optional,
    /// Nothing on this venue's live path reads the field. Either the venue signs with key+secret
    /// alone, or it never builds a shared `Credentials` at all (its own `config.rs` owns a
    /// different shape, and its own required-field gate with it).
    Unused,
}

impl PassphraseNeed {
    /// The one projection the credential loader consults.
    #[must_use]
    pub fn is_required(self) -> bool {
        matches!(self, PassphraseNeed::Required)
    }
}

/// Today's per-venue passphrase reality, one row per canonical lowercase venue id (each bridge
/// crate's `VENUE` const). Every row cites the adapter code it was read from. An unknown venue —
/// and every venue whose live path never touches the field — is [`PassphraseNeed::Unused`]: the
/// loader must never invent a requirement for a venue nobody classified, because that would drop a
/// working mount to paper, the mirror-image of the bug this table closes.
#[must_use]
pub fn venue_passphrase(venue: &str) -> PassphraseNeed {
    declared_row(venue).unwrap_or(PassphraseNeed::Unused)
}

/// The DECLARED row, or `None` for a venue with no named arm.
///
/// ⚠ This split is what makes the completeness gate REAL rather than a list checking itself. A
/// `match` with a fallback arm cannot be asked from outside whether a venue was CLASSIFIED or
/// merely fell through, and most rows here are `Unused` — the same value the fallback returns —
/// so deleting a venue's arm would be undetectable by any assertion on [`venue_passphrase`]
/// alone. `None` is the shape of "nobody read this venue's signer", which
/// `every_roster_venue_is_classified` can then fail on; the public fn folds it to the safe
/// default so an unknown venue never acquires a requirement it cannot satisfy.
fn declared_row(venue: &str) -> Option<PassphraseNeed> {
    let need = match venue {
        // REQUIRED. `crates/vike-bridge-core/src/signer.rs`'s `OkxV5Signer` copies
        // `credentials.passphrase` into the `OK-ACCESS-PASSPHRASE` header of every signed REST
        // request (`unwrap_or_default()` — an absent one is sent as an EMPTY header, not omitted),
        // and the private-WS login carries it too: `crates/bridges/okx/src/exec.rs` passes
        // `creds.passphrase.clone().unwrap_or_default()` into the user-data pump, which builds the
        // frame with `crates/bridges/okx/src/ws_auth.rs`'s `build_login_frame`. The venue's own
        // answer to a blank one is `crates/bridges/okx/src/error_codes.rs`'s `by_code`: 50104
        // ("OK-ACCESS-PASSPHRASE cannot be empty") and 50105 ("incorrect") both classify as
        // `ErrorKind::Auth`. So key+secret alone authenticate NOTHING on this venue.
        "okx" => PassphraseNeed::Required,

        // OPTIONAL. Aster reuses the shared struct for an agent-wallet shape:
        // `crates/bridges/aster/src/signing.rs`'s `AsterSigner` reads `creds.passphrase` as the
        // agent SIGNER address and, when it is absent or blank, DERIVES it from the private key
        // (`crates/vike-bridge-core/src/eip712.rs`'s `eth_address_from_private_key`). Its loader,
        // the same file's `load_aster_credentials`, sets the field from an optional
        // `ASTER_{TIER}_SIGNER` and requires only `_USER` + `_PRIVATE_KEY`. A blank one is a
        // working mount, so requiring it here would break aster outright.
        "aster" => PassphraseNeed::Optional,

        // UNUSED — signs with key+secret alone, through the shared `Credentials`.
        // `crates/vike-bridge-core/src/signer.rs`'s `BinanceHmacSigner` holds only key+secret
        // (HMAC-SHA256 over the query string + `X-MBX-APIKEY`); the struct has no passphrase field.
        "binance" => PassphraseNeed::Unused,
        // Same: `crates/vike-bridge-core/src/signer.rs`'s `BybitV5Signer` holds key+secret only.
        "bybit" => PassphraseNeed::Unused,
        // Deribit authenticates with an OAuth2 `client_credentials` pair, which this workspace
        // carries in the key/secret fields: `crates/bridges/deribit/src/transport.rs`'s
        // `DeribitOrderTransport` takes `(client_id, client_secret, scope)` and the exec/recon
        // arms pass `&creds.api_key, &creds.api_secret, None`. No third credential exists.
        "deribit" => PassphraseNeed::Unused,

        // UNUSED — these venues never build a shared `Credentials` at all, so this loader is not
        // their gate. Each cites the loader that IS, and each owns its own required-field rule.
        // `crates/bridges/oanda/src/config.rs`'s `load_oanda_config_from`: `_API_KEY`+`_ACCOUNT_ID`.
        "oanda" => PassphraseNeed::Unused,
        // `crates/bridges/ig/src/config.rs`'s `load_ig_config_from`: `_API_KEY`+`_IDENTIFIER`+`_PASSWORD`.
        "ig" => PassphraseNeed::Unused,
        // `crates/bridges/fxcm/src/config.rs`'s `load_fxcm_config_from`: `_USER`+`_PASSWORD`.
        "fxcm" => PassphraseNeed::Unused,
        // `crates/bridges/dukascopy/src/config.rs`'s `load_dukascopy_config_from`: per-account
        // `DUKASCOPY_DEMO{1,2}_LOGIN`/`_PASSWORD`.
        "dukascopy" => PassphraseNeed::Unused,
        // `crates/bridges/ctrader/src/config.rs`'s `CtraderConfig` (`from_vars`): OAuth2
        // client id/secret + access/refresh tokens.
        "ctrader" => PassphraseNeed::Unused,
        // `crates/bridges/alpaca/src/config.rs`'s `load_alpaca_config_from`: OAuth2 client
        // id/secret + account id.
        "alpaca" => PassphraseNeed::Unused,
        // `crates/bridges/vike-ibkr/src/config.rs`'s `load_ibkr_config_from`:
        // host/port/client-id/account/backend — no API secret at all.
        "ibkr" => PassphraseNeed::Unused,
        // `crates/bridges/hyperliquid/src/config.rs`'s `HlCredentials`: one private key (plus an
        // optional agent-wallet account address).
        "hyperliquid" => PassphraseNeed::Unused,
        // ⚠ Polymarket DOES have an L2 passphrase — and it is still `Unused` HERE, because it
        // never rides this loader: `crates/bridges/polymarket/src/config.rs`'s `PolymarketCreds`
        // holds it, `crates/bridges/polymarket/src/l1.rs`'s `derive_api_key` obtains it over the
        // mount-time `/auth/derive-api-key` round-trip, and that file's `parse_derived` already
        // refuses a response missing it, BY NAME.
        "polymarket" => PassphraseNeed::Unused,

        // vike:new-venue:row // TODO(new-venue: {venue}): READ THE SIGNER before keeping this. `Required` only if the
        // vike:new-venue:row // adapter puts a passphrase on the wire, so a blank one cannot authenticate; `Optional`
        // vike:new-venue:row // if it reads the field but derives it when blank; cite the file either way.
        // vike:new-venue:row "{venue}" => PassphraseNeed::Unused,
        // UNCLASSIFIED — a venue nobody has read the signer of. `venue_passphrase` folds this to
        // `Unused`, the only safe default (see the fallback note there); the roster gate below is
        // what stops a ROSTER venue from ever reaching it.
        _ => return None,
    };
    Some(need)
}

#[cfg(test)]
mod tests {
    use super::PassphraseNeed::{Optional, Required, Unused};
    use super::{PassphraseNeed, venue_passphrase};

    /// The CURRENT matrix, verbatim — any drift in a venue row is loud here. This is the pin the
    /// tif/caps/fees tables all carry: the value is asserted where a reader can SEE it, so
    /// flipping a row is a deliberate edit to a test, never a quiet behaviour change.
    #[test]
    fn current_passphrase_matrix_is_pinned() {
        #[rustfmt::skip]
        let rows: &[(&str, PassphraseNeed)] = &[
            // the ONE venue whose signer cannot authenticate without it
            ("okx",         Required),
            // reads the field, derives it when blank — must never gate
            ("aster",       Optional),
            // shared `Credentials`, key+secret only
            ("binance",     Unused),
            ("bybit",       Unused),
            ("deribit",     Unused),
            // bespoke credential shapes — this loader is not their gate
            ("oanda",       Unused),
            ("ig",          Unused),
            ("fxcm",        Unused),
            ("dukascopy",   Unused),
            ("ctrader",     Unused),
            ("alpaca",      Unused),
            ("ibkr",        Unused),
            ("hyperliquid", Unused),
            ("polymarket",  Unused),
            // vike:new-venue:row ("{venue}",  Unused), // TODO(new-venue: {venue}): pin whatever the arm above declares
        ];
        for (venue, want) in rows {
            assert_eq!(venue_passphrase(venue), *want, "{venue}");
            assert!(vike_model::VENUES.contains(venue), "stale pin for non-roster venue {venue}");
        }
        // the pinned set IS the roster (no extra rows, no missing ones)
        assert_eq!(rows.len(), vike_model::VENUES.len());
    }

    /// Completeness vs the canonical roster (`vike_model::VENUES`): every roster venue has a
    /// NAMED arm, never the fallthrough. Adding a bridge crate fails here until somebody reads its
    /// signer and classifies it.
    ///
    /// ⚠ This asserts on [`super::declared_row`], NOT on `venue_passphrase`, and that is the whole
    /// gate. Most rows are `Unused`, which is exactly what the fallback returns — so a test written
    /// against the public fn would still pass with a venue's arm DELETED, and a declaration-pinning
    /// test that cannot fail is not a gate (the mutation proof for this test deletes the deribit
    /// arm). `declared_row`'s `None` is the only externally visible difference between "classified
    /// as Unused" and "never classified".
    #[test]
    fn every_roster_venue_is_classified() {
        for &v in vike_model::VENUES {
            assert!(
                super::declared_row(v).is_some(),
                "roster venue {v} has no passphrase row — read its signer and add one (Required \
                 only if the adapter puts the passphrase on the wire, so that a blank one cannot \
                 authenticate)"
            );
        }
    }

    /// …and the fallthrough is genuinely REACHABLE, so the gate above is testing something: a
    /// non-roster string has no row, and `venue_passphrase` folds that to the safe default.
    #[test]
    fn an_unclassified_venue_has_no_row_and_folds_to_the_safe_default() {
        assert_eq!(super::declared_row("no-such-venue"), None);
        assert_eq!(venue_passphrase("no-such-venue"), Unused);
    }

    /// The FALLBACK direction, which is the dangerous one: an unknown venue must never acquire a
    /// requirement. A `Required` default would drop every venue this table has not heard of to
    /// paper — the exact mirror of the bug it closes.
    #[test]
    fn unknown_venues_are_unused_not_required() {
        for v in ["no-such-venue", "", "OKX", "okx-perp"] {
            assert_eq!(venue_passphrase(v), Unused, "{v:?}");
        }
    }

    /// The venue id is matched EXACTLY: the table keys off each crate's canonical lowercase
    /// `VENUE` const, and an uppercase or lane-suffixed string is a different key (the `"OKX"`
    /// case above). Callers pass the canonical id — `load_credentials_from` uppercases only to
    /// build the variable PREFIX, never to look up the row.
    #[test]
    fn is_required_projects_only_the_required_row() {
        assert!(venue_passphrase("okx").is_required());
        assert!(!venue_passphrase("aster").is_required());
        assert!(!venue_passphrase("binance").is_required());
        assert!(!Unused.is_required());
        assert!(!Optional.is_required());
        assert!(Required.is_required());
    }
}
