//! The credential-name classifier: which account a credential key name belongs to, and its field.
//!
//! `docs/superpowers/specs/2026-09-14-the-credential-schema.md` is the authority for every row of
//! every table here; each one cites the section that put it there.

// Private, never `pub use`: the grammar's one home is `vike_secrets::venue_setting`, and a
// re-export would be a second spelling —
// `crates/vike-ops/tests/settings_secrets/smoke_store_parity_gate.rs`'s
// `the_grammar_has_exactly_one_home`.
use vike_secrets::venue_setting::{HAND_MAPPED_ACCOUNTS, hand_mapped_prefix, venue_setting_names};

/// **Which ACCOUNT a credential key name belongs to, and what its `field` is** — the classification
/// `vike_secrets`' schema 2 needs and cannot derive for itself.
///
/// # ⚠ Why it lives HERE and not in the store
///
/// `vike-secrets` sits in tier 15, whose rule is *nothing above rank 10*
/// (`crates/vike-ops/tests/architecture/layer_gate/tiers.rs`'s
/// `every_tier_15_crate_names_nothing_above_the_vocabulary`); this crate declares `layer = 25` and
/// itself depends on `vike-secrets`, so the reverse edge is a band violation and a cycle. The
/// derivation therefore reaches the store as a CLOSURE (`vike_secrets::create_store`'s and
/// `save_credentials_to_store`'s `classify`). It lives in this crate because the credential-key
/// vocabulary does, and every production credential writer already links it. It is NOT behind the
/// `full` feature: `vike-cli` takes this crate with `default-features = false`.
///
/// # The three answers
///
/// §5.1's three cases, as [`vike_secrets::Placement`]:
///
/// * **account-scoped** — `account_id` set, `venue` NULL. `OKX_DEMO_API_SECRET`, `POLY_PRIVATE_KEY`.
/// * **venue-scoped** — `account_id` NULL, `venue` set. `CTRADER_CLIENT_ID`/`_CLIENT_SECRET` carry
///   no tier token because they are APPLICATION credentials: one OAuth application, shared by every
///   account of the venue. ⚠ `venue` here means *belongs to this venue's plane*, not *issued by
///   this venue*.
/// * **infrastructure** — both NULL. `CLOUDFLARE_API_TOKEN`, `FINNHUB_API_KEY`. Reached as the
///   CATCH-ALL (§11 step 6) and reported as unrecognised: a store that declines to speak about an
///   unusual name is how an unusual name rots.
#[must_use]
pub fn classify_credential_name(name: &str) -> vike_secrets::Classification {
    use vike_secrets::{AccountKey, Classification, Placement};

    // A MACHINE-scoped venue setting's legacy name, matched WHOLE. First, because `POLY_RATE_GATE`
    // would otherwise reach the hand-map's `POLY_` row as an account field.
    if let Some(class) = classify_machine_setting(name) {
        return class;
    }

    // The POLY_* family SPLITS key by key (§5.2), so it is consulted before the prefix table
    // below, whose `POLY_` row is its fallback.
    if let Some(class) = classify_poly(name) {
        return class;
    }

    // ⚠ The ACCOUNT LABEL is split off BEFORE the hand-map: matched against the WHOLE name,
    // `POLY_FUNDER__HEDGE` would file as a `FUNDER__HEDGE` field of the DEFAULT account and lose
    // both per-`(venue, field)` table lookups (`POLY_SIGNATURE_TYPE__{LABEL}` is read by
    // `crates/bridges/polymarket/src/exec_plane/recon_client.rs`'s `signature_type_for_account`).
    // A malformed suffix is not an error: the name stays whole and falls through to the catch-all,
    // REPORTED as unrecognised rather than filed against a guess.
    let (base, label) = match vike_model::accounts::account_keys::split_account_key(name) {
        Ok(split) => (split.base, split.label.text().map(str::to_string)),
        Err(_) => (name, None),
    };

    // §11 step 2 — the venue families `account_ref_from_key` misses, hand-mapped with a reason.
    for (head, token, venue, tier, discriminator, _why) in HAND_MAPPED_ACCOUNTS {
        if let Some(field) = base.strip_prefix(&hand_mapped_prefix(head, token))
            && !field.is_empty()
        {
            return account_row(
                AccountKey {
                    venue: (*venue).to_string(),
                    tier: (*tier).to_string(),
                    // ⚠ The DISCRIMINATOR is dropped when a LABEL is present: it only separates
                    // accounts `(venue, tier, label)` cannot
                    // (`vike_secrets::AccountKey::discriminator`), and it reaches no column, so
                    // keeping both would key the account on a tuple the `account` table cannot
                    // reproduce — a re-run would INSERT a second account at the same
                    // `UNIQUE (venue, tier, label)`.
                    discriminator: if label.is_some() {
                        None
                    } else {
                        discriminator.map(str::to_string)
                    },
                    label: label.clone(),
                },
                field,
            );
        }
    }

    // §5.1's venue-scoped rows: an APPLICATION credential, and the attribution family, which
    // `account_ref_from_key` also answers `None` for (the token after the venue is no tier) and
    // which §11 step 6's catch-all would otherwise file as the DEPLOYMENT's.
    if let Some((venue, field)) = venue_scoped(name) {
        return Classification {
            placement: Placement::Venue(venue.to_string()),
            secret: is_secret(venue, field),
            field: field.to_string(),
            recognised: true,
        };
    }

    // The venue grammar itself.
    if let Some(reference) = vike_model::accounts::account_keys::account_ref_from_key(name)
        && let Some(field) = field_after_tier(name, reference.venue)
    {
        return account_row(
            AccountKey {
                venue: reference.venue.to_string(),
                // ⚠ NOT `to_ascii_lowercase()`: the account tier is `paper` where the key token is
                // `SIM`, and `vike_secrets::account_tier_of_key_token` is the ONE site joining the
                // two vocabularies. A bare lowercase answers `sim`, which the `account` table's
                // CHECK refuses.
                tier: vike_secrets::account_tier_of_key_token(reference.tier),
                label: reference.label.text().map(str::to_string),
                discriminator: None,
            },
            field,
        );
    }

    // §11 step 6 / §11.1 — never dropped, never guessed at, always REPORTED.
    Classification::unrecognised(name)
}

/// Assemble an account-scoped row, applying the two per-`(venue, field)` tables.
fn account_row(key: vike_secrets::AccountKey, field: &str) -> vike_secrets::Classification {
    let secret = is_secret(&key.venue, field);
    vike_secrets::Classification {
        secret,
        field: field.to_string(),
        placement: vike_secrets::Placement::Account(key),
        recognised: true,
    }
}

/// **A MACHINE-scoped venue setting's legacy credential name** — `POLY_RATE_GATE`,
/// `BINANCE_TRADE_LITE_FILL`, `OKX_MARK_STREAMS` — matched WHOLE, and DERIVED rather than listed:
/// the fields are `vike_model::venues::venue_fields::VENUE_FIELDS` and the names are the store's own
/// renderer's ([`venue_setting_names`]), so a field declared later is classified by being declared.
///
/// A credential row under one of these names is configuration in the wrong table, read by nothing
/// (`vike-cli secrets set` refuses to write one); it is filed as the venue's, and its `secret` flag
/// is the catalog row's (`venue.polymarket.socks_proxy` may carry `user:password@`).
///
/// ⚠ **WHOLE, so a LABELLED spelling never matches.** One process has one egress and the toggles
/// are one per machine, so no reader looks a labelled one up: it falls through to the account
/// grammar as an ordinary account row (`vike_secrets::venue_setting::stranded_venue_setting_names`
/// matches whole for the same reason).
fn classify_machine_setting(name: &str) -> Option<vike_secrets::Classification> {
    use vike_secrets::{Classification, Placement};
    vike_model::venues::venue_fields::VENUE_FIELDS.iter().filter(|f| !f.tier_scoped).find_map(|f| {
        let field = f.field.to_ascii_uppercase();
        let named = venue_setting_names(f.venue, None, &field).iter().any(|n| n == name);
        named.then(|| Classification {
            placement: Placement::Venue(f.venue.to_string()),
            field,
            secret: f.secret,
            recognised: true,
        })
    })
}

/// **§5.2 — the `POLY_*` family, key by key**, for what is left of it once
/// [`classify_machine_setting`] has taken the machine-scoped settings: the builder code.
///
/// ⚠ The row is matched on the FIELD, after a COMPOSED head, never as a whole literal: a whole
/// env-prefixed literal in a `src/` file is read by
/// `crates/vike-ops/tests/settings_secrets/settings_registry.rs`' sweep as a variable this file
/// READS, and `POLY_BUILDER_CODE` is read by NOTHING, so it must not acquire a `SETTINGS` row.
///
/// ⚠ **It matches the WHOLE name, so a LABELLED key never reaches this arm.** `Placement::Venue`
/// has nowhere to put a label; a labelled key falls through to the hand-map, where the label
/// becomes the account's.
fn classify_poly(name: &str) -> Option<vike_secrets::Classification> {
    use vike_secrets::{Classification, Placement};
    let field = name.strip_prefix(&hand_mapped_prefix("POLY", ""))?;
    if field == "BUILDER_CODE" {
        // §5.2: venue-scoped, `secret = 0` — and DEAD:
        // `vike_model::venues::attribution::attribution_for` composes the venue's OWN attribution
        // spelling, so no reader exists for this one. It stays in `credential` BECAUSE it is dead:
        // filing an unread value as machine or tier configuration would assert a fact from no
        // evidence.
        return Some(Classification {
            placement: Placement::Venue("polymarket".to_string()),
            field: field.to_string(),
            secret: false,
            recognised: true,
        });
    }
    None
}

/// **§5.1's venue-scoped names** — `account_id` NULL, `venue` set:
///
/// * cTrader's OAuth APPLICATION pair, which carries no tier token because one application serves
///   every account of the venue;
/// * the ATTRIBUTION family (`{VENUE}_BROKER_CODE` / `{VENUE}_BUILDER_CODE`), which
///   `account_ref_from_key` answers `None` for (*the token after the venue is no tier*), so §11
///   step 6's catch-all would otherwise file one as the DEPLOYMENT's.
fn venue_scoped(name: &str) -> Option<(&'static str, &str)> {
    // The head is COMPOSED, never spelled, for the reason `classify_poly` gives.
    if let Some(field) = name.strip_prefix(&hand_mapped_prefix("CTRADER", ""))
        && (field == "CLIENT_ID" || field == "CLIENT_SECRET")
    {
        return Some(("ctrader", field));
    }
    for venue in vike_model::VENUES {
        let head = format!("{}_", venue.to_uppercase());
        if let Some(field) = name.strip_prefix(&head)
            && (field == "BROKER_CODE" || field == "BUILDER_CODE")
        {
            return Some((venue, field));
        }
    }
    None
}

// vike:new-venue:note a CONFORMING venue needs NO row in any of the per-venue tables this file carries — that is what makes them exception tables rather than a roster. `{venue}` needs an `is_secret` row only for a key that holds no secret; if its store keys do NOT parse as `{VENUE}_{TIER}_{FIELD}` it also needs a `vike_secrets::venue_setting::HAND_MAPPED_ACCOUNTS` row, which lives one crate down since 2026-09-22 (dukascopy bakes an account index into the tier token; alpaca spells its tier `SANDBOX`). Getting both wrong for a conforming venue costs nothing: the defaults are account-scoped and `secret = 1`: crates/vike-bridge-core/src/credentials.rs's `classify_credential_name`
/// **§6 — the rows MEASURED as holding no secret.**
///
/// Keyed on `(venue, field)` rather than on the whole NAME, so every tier classifies the same way:
/// `IBKR_LIVE_HOST` is as much a host as `IBKR_DEMO_HOST` is.
///
/// `true` is the default for everything else: a value wrongly marked non-secret is a worse error
/// than one wrongly marked secret.
///
/// ⚠ A TIER-scoped declared venue field (`IBKR_DEMO_HOST`, `FXCM_LIVE_URL`, `DUKASCOPY_DEMO1_SERVER`)
/// takes its flag from the settings catalog (`vike_model::venues::venue_fields`). A MACHINE-scoped
/// field reached here is a LABELLED spelling no reader looks up, and stays at the conservative
/// default; its unlabelled name is [`classify_machine_setting`]'s.
fn is_secret(venue: &str, field: &str) -> bool {
    if let Some(f) =
        vike_secrets::venue_setting::declared_field(venue, field).filter(|f| f.tier_scoped)
    {
        return f.secret;
    }
    !matches!(
        (venue, field),
        // The two config-shaped names that STAY in `credential` at `secret = 0`:
        //
        // `IBKR_*_CLIENT_ID` — the client ids must DIFFER between two live TWS connections or the
        // gateway evicts the second socket (`crates/bridges/vike-ibkr/src/config.rs`'s
        // `load_ibkr_config_for_account`), so it is a per-ACCOUNT slot, which `venue_setting` is
        // defined not to hold.
        ("ibkr", "CLIENT_ID")
            // `POLY_SIGNATURE_TYPE` — per-WALLET: `signature_type_for_account` reads
            // `POLY_SIGNATURE_TYPE__{LABEL}` and refuses any fallback to the unlabelled key.
            | ("polymarket", "SIGNATURE_TYPE")
    )
}

/// §4.4's account-scoped derivation: the name with `{VENUE}_{TIER}_` removed, the tier token being
/// the one the NAME carries (one of `vike_model::credential_keys::CREDENTIAL_TIERS`).
///
/// ⚠ **The token is found in the LABEL-STRIPPED base, so `field` EXCLUDES a `__LABEL` suffix**
/// (`OKX_DEMO_API_KEY__HEDGE` yields `API_KEY`). For a labelled key `name.strip_suffix(field)` then
/// fails and `vike_secrets::Classification::owner_prefix` is `None`, so the account is found by
/// `(venue, tier, label)`; an owner prefix of `{VENUE}_{TIER}_` would resolve the labelled key to
/// the DEFAULT account's row.
fn field_after_tier<'a>(name: &'a str, venue: &str) -> Option<&'a str> {
    let base = vike_model::accounts::account_keys::split_account_key(name).ok()?.base;
    let head = format!("{}_", venue.to_uppercase());
    let rest = base.strip_prefix(&head)?;
    for tier in vike_model::credential_keys::CREDENTIAL_TIERS {
        if let Some(field) = rest.strip_prefix(&format!("{tier}_"))
            && !field.is_empty()
        {
            return Some(field);
        }
    }
    None
}

#[path = "venue_setting_renderer_tests.rs"]
#[cfg(test)]
mod venue_setting_renderer_tests;
