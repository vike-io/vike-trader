//! **EVERY CREDENTIAL FIELD A VENUE'S LOADER READS IS EITHER OFFERED BY THE CONNECTIONS FORM OR
//! CARRIES A WRITTEN ROW SAYING WHY IT MUST NOT BE — and so is every venue-wide attribution name the
//! registry declares.**
//!
//! # The defect, measured on a real store
//!
//! The form used to be a hand-written `match venue` table beside an authority that can be derived.
//! The failure such a table has is not a WRONG entry — it is a credential the store really holds
//! that appears in NO form at all, because nothing about a form says which of its venue's fields
//! are missing from it. MEASURED: the owner's store held two dukascopy server rows and the DEMO
//! form offered neither, so two values already in use could be changed from nowhere in the app.
//! That is the same shape as every other *control that silently does nothing*, one level up: the
//! control was never drawn.
//!
//! # The universe, since decision 0156 item 6
//!
//! A credential belongs to its ACCOUNT ROW and is addressed by FIELD (`credential.account_id` +
//! `field`; a venue's application rows by `credential.venue_id` + `field`), so the universe is the
//! FIELDS every venue's loader reads — its row of `vike_venues::credential_schema`, owners `Account`
//! and `Venue` both — and the GUI form is `vike_gateway_panel::keys::form_fields`. No name is folded,
//! composed or parsed here: a name-based universe would need a reader that derives a field (or a
//! tier) from a name, which is exactly what the ruling removed.
//!
//! The one place NAMES remain is the venue-wide attribution tail on the LIVE form: DEPLOYMENT rows
//! (`vike_secrets::credentials::attribution_code_from` reads them by name, no account owns them).
//! Their universe is `vike_config::settings::all_settings`' venue-scoped attribution names — which is
//! why this file lives in `vike-app-core`, the lowest crate that depends on both
//! `vike_gateway_panel::keys` and `vike_config::settings`.
//!
//! # What the gate accepts as an answer
//!
//! Being offered, or a WRITTEN row. The rows are written because "this field is not a form field"
//! is a judgement, and a judgement with no reason beside it is indistinguishable from an omission;
//! they are pinned BOTH WAYS against the schema's own `KeyField::form == false` set, so neither side
//! can move alone.

use std::collections::{BTreeMap, BTreeSet};

use vike_config::settings::{Scope, all_settings};
use vike_gateway_panel::keys::{
    FieldTarget, FormPlace, Sensitivity, field_sensitivity, form_fields, form_note,
    is_venue_wide_key,
};
use vike_model::{AccountTier, VENUES};
use vike_venues::credential_schema::schema_for;

/// One (venue, field) a loader reads and no form offers, with the reason it must not be a field.
struct NotOffered {
    venue: &'static str,
    field: &'static str,
    /// Why it must not be a field. A reason, never a restatement of the field.
    why: &'static str,
}

/// **THE WRITTEN EXEMPTIONS** — the fields the venue loaders read that no form offers. The array's
/// declared length IS the count, so a new row cannot arrive without that number changing.
///
/// ⚠ Read the rows, not this paragraph, for what is excused. What they have in common is the
/// criterion: a field belongs in a CREDENTIAL form when writing it configures WHO this deployment
/// is to that venue. A field that configures WHICH connection slot is used, or that the venue
/// hands over by itself, does not.
const NOT_OFFERED: [NotOffered; 7] = [
    NotOffered {
        venue: "ctrader",
        field: "ACCOUNT_ID",
        why: "OPTIONAL and DISCOVERED. `vike_ctrader::config` treats it as optional and resolves the \
              account at connect, so a form offering it would invite an operator to PIN an account \
              id the venue would otherwise hand them — and a pinned wrong one routes orders to \
              another book.",
    },
    NotOffered {
        venue: "ibkr",
        field: "CLIENT_ID",
        why: "WHICH CONNECTION SLOT, not which account. `vike_ibkr::config` refuses without the \
              ACCOUNT alone — the form's one field — and this one has a DEFAULT, so a \
              present-but-unparseable value would turn a working mount into a refused one.",
    },
    NotOffered {
        venue: "ibkr",
        field: "DATA_CLIENT_ID",
        why: "WHICH CONNECTION SLOT the market-data session takes, with a default — the same \
              argument as the order session's client id: a typed value can only break a mount \
              that works.",
    },
    NotOffered {
        venue: "ibkr",
        field: "RECON_CLIENT_ID",
        why: "WHICH CONNECTION SLOT the reconciliation session takes, with a default — the same \
              argument as the order session's client id: a typed value can only break a mount \
              that works.",
    },
    NotOffered {
        venue: "ibkr",
        field: "MKTDATA_TYPE",
        why: "WHICH market-data feed type the gateway is asked for, with a default — a behaviour \
              of the session, not the identity of the account, and not something a credential \
              form should teach an operator to configure.",
    },
    NotOffered {
        venue: "polymarket",
        field: "FUNDER",
        why: "THE ALTERNATE SPELLING of the funder address the form already offers as ADDRESS: \
              the loader falls back to it when ADDRESS is absent. A second field would be two \
              controls over one value, where filling the wrong one loses silently to the other.",
    },
    NotOffered {
        venue: "polymarket",
        field: "SIGNATURE_TYPE",
        why: "WHICH SIGNING SCHEME the CLOB is told the funder wallet uses, read where present and \
              defaulted otherwise. It describes the wallet the key belongs to rather than the \
              account, and a wrong pick makes the venue refuse every order the key signs.",
    },
];

/// **THE ALTERNATE ATTRIBUTION SPELLINGS** — declared venue-wide names no form offers, because the
/// form offers the OTHER spelling of the same tag.
///
/// `vike_secrets::credentials::attribution_code_from` tries `_BROKER_CODE` and then falls back to
/// `_BUILDER_CODE`, so BOTH names are looked up for every mechanised venue and
/// `vike_venues::credential_keys::attribution_keys` declares both — but they are one tag. The form
/// offers the one the venue's MECHANIC is named for (a `SignedBuilder` venue's builder code, a CEX
/// venue's broker code); a second field would be two controls over one value.
const ALTERNATE_SPELLINGS: [&str; 6] = [
    "BINANCE_BUILDER_CODE",
    "BYBIT_BUILDER_CODE",
    "OKX_BUILDER_CODE",
    "ASTER_BROKER_CODE",
    "HYPERLIQUID_BROKER_CODE",
    "POLYMARKET_BROKER_CODE",
];

fn schema(venue: &str) -> &'static vike_venues::credential_schema::VenueKeySchema {
    schema_for(venue).unwrap_or_else(|| panic!("{venue} has no schema row"))
}

/// Every (venue, field) a form offers — the account fields of every tier's form and the venue's
/// application fields.
fn offered() -> BTreeSet<(&'static str, &'static str)> {
    let mut out = BTreeSet::new();
    for venue in VENUES {
        let places = [
            FormPlace::Tier(AccountTier::Paper),
            FormPlace::Tier(AccountTier::Demo),
            FormPlace::Tier(AccountTier::Live),
            FormPlace::VenueApp,
        ];
        for place in places {
            for f in form_fields(venue, place) {
                if let FieldTarget::Account(k) | FieldTarget::Venue(k) = f.target {
                    out.insert((*venue, k.field));
                }
            }
        }
    }
    out
}

/// Every (venue, field) a venue's loader reads — the schema rows, both owners.
fn universe() -> BTreeSet<(&'static str, &'static str)> {
    VENUES.iter().flat_map(|v| schema(v).fields.iter().map(move |f| (*v, f.field))).collect()
}

fn written() -> BTreeSet<(&'static str, &'static str)> {
    NOT_OFFERED.iter().map(|r| (r.venue, r.field)).collect()
}

/// **THE GATE.** Every field a venue's loader reads is offered by a form or excused by a written
/// [`NOT_OFFERED`] row — and nothing else is an answer.
#[test]
fn every_loader_field_is_offered_by_a_form_or_excused_in_writing() {
    let offered = offered();
    let written = written();
    let orphans: Vec<String> = universe()
        .into_iter()
        .filter(|vf| !offered.contains(vf) && !written.contains(vf))
        .map(|(v, f)| format!("{v} {f}"))
        .collect();
    assert!(
        orphans.is_empty(),
        "{} loader field(s) are offered by NO credential form and excused by nothing. Either mark \
         each `form: true` in its venue's `vike_venues::credential_schema` row, or add it to \
         `NOT_OFFERED` with the reason it must not be a field:\n  {}",
        orphans.len(),
        orphans.join("\n  ")
    );
}

/// **The written rows are pinned BOTH WAYS to the schema's own `form == false` set.** A row naming
/// a field no loader reads, or one the form now offers, is a reason attached to nothing — and the
/// second case is worse than untidy, because the row would go on excusing a field that had been
/// silently deleted again.
#[test]
fn the_written_exemptions_are_exactly_the_schemas_read_only_fields() {
    let read_only: BTreeSet<(&str, &str)> = VENUES
        .iter()
        .flat_map(|v| schema(v).fields.iter().filter(|f| !f.form).map(move |f| (*v, f.field)))
        .collect();
    assert_eq!(
        written(),
        read_only,
        "NOT_OFFERED and the schema's `form == false` set moved apart"
    );
    let offered = offered();
    for row in &NOT_OFFERED {
        assert!(
            !offered.contains(&(row.venue, row.field)),
            "{} {}: the form now OFFERS this field — delete the row, it excuses nothing",
            row.venue,
            row.field
        );
    }
}

/// **Every row excuses a DISTINCT field, and the reasons are real sentences.**
#[test]
fn every_exemption_row_carries_a_distinct_field_and_a_reason() {
    let mut seen: BTreeMap<(&str, &str), usize> = BTreeMap::new();
    for (i, row) in NOT_OFFERED.iter().enumerate() {
        assert!(row.why.len() > 60, "row {i}'s reason is too short to be one: {:?}", row.why);
        let first = seen.insert((row.venue, row.field), i);
        assert!(first.is_none(), "{} {} is excused twice", row.venue, row.field);
    }
}

/// **Every venue-wide attribution name the registry declares is offered by a LIVE form or is the
/// alternate spelling of one that is** — the deployment half of the editor, where names remain.
#[test]
fn every_declared_attribution_name_is_offered_or_is_an_alternate_spelling() {
    let declared: BTreeSet<String> = all_settings()
        .filter(|s| s.scope == Scope::Venue && is_venue_wide_key(s.name))
        .map(|s| s.name.to_string())
        .collect();
    assert!(
        !declared.is_empty(),
        "the registry declares no attribution name — this gate is vacuous"
    );
    let offered: BTreeSet<String> = VENUES
        .iter()
        .flat_map(|v| form_fields(v, FormPlace::Tier(AccountTier::Live)))
        .filter_map(|f| match f.target {
            FieldTarget::Deployment(name) => Some(name),
            _ => None,
        })
        .collect();
    let orphans: Vec<&String> = declared
        .iter()
        .filter(|n| !offered.contains(*n) && !ALTERNATE_SPELLINGS.contains(&n.as_str()))
        .collect();
    assert!(orphans.is_empty(), "declared attribution names no LIVE form offers: {orphans:?}");
    for alt in ALTERNATE_SPELLINGS {
        assert!(declared.contains(alt), "{alt}: no longer declared — delete the row");
        assert!(!offered.contains(alt), "{alt}: the form now OFFERS it — delete the row");
    }
}

/// **The paper tier is the ONE cell with no form, for every venue** — the simulator reads no
/// credential — and every venue's demo and live forms exist. Pinned both ways: a venue whose
/// demo or live form vanished would be a venue whose fields just became silently unreachable.
#[test]
fn the_paper_tier_is_the_only_cell_with_no_form() {
    for venue in VENUES {
        assert!(form_fields(venue, FormPlace::Tier(AccountTier::Paper)).is_empty(), "{venue}");
        for tier in [AccountTier::Demo, AccountTier::Live] {
            assert!(!form_fields(venue, FormPlace::Tier(tier)).is_empty(), "{venue} {tier}");
        }
    }
}

/// **The JForex server is a venue SETTING, and no form writes it as a credential.** The server was
/// in the owner's real store and reachable from no form — the defect this gate was measured into
/// existence by — and decision 0095's Task 7 made it the demo tier's `venue.dukascopy.demo.server`
/// setting, read by nothing in the credential store. The DEMO form offers no such field and says
/// where the server is set instead.
#[test]
fn the_dukascopy_server_is_a_venue_setting_and_no_form_writes_it() {
    let demo = FormPlace::Tier(AccountTier::Demo);
    let fields: Vec<String> =
        form_fields("dukascopy", demo).iter().map(|f| f.hint().to_string()).collect();
    assert!(
        !fields.iter().any(|f| f.contains("SERVER")),
        "the server is the `venue.dukascopy.demo.server` setting, not a credential: {fields:?}"
    );
    let note = form_note("dukascopy", demo).expect("the DEMO form's note");
    assert!(note.contains("venue.dukascopy.demo.server"), "{note}");
}

/// **SENSITIVITY IS DECIDED FOR EVERY FIELD THE EDITOR OFFERS, and it FAILS CLOSED.**
///
/// ⚠ The check that matters is the second one: a field no schema row declares is classified
/// `Secret`, so a field added tomorrow is masked and never read back until somebody deliberately
/// classifies it. This test proves the default is reached rather than trusting the table.
///
/// The first one pins the two ends an operator would notice: an API secret is masked, and a login
/// and an account number are not.
#[test]
fn the_sensitivity_rule_fails_closed_and_puts_names_on_the_readable_side() {
    for (venue, field, want) in [
        ("binance", "API_KEY", Sensitivity::Secret),
        ("binance", "API_SECRET", Sensitivity::Secret),
        ("dukascopy", "PASSWORD", Sensitivity::Secret),
        ("polymarket", "PRIVATE_KEY", Sensitivity::Secret),
        ("ctrader", "ACCESS_TOKEN", Sensitivity::Secret),
        ("alpaca", "CLIENT_SECRET", Sensitivity::Secret),
        ("polymarket", "RELAYER_API_KEY", Sensitivity::Secret),
        // ...and the readable half: a login, an account, an address.
        ("dukascopy", "LOGIN", Sensitivity::Public),
        ("ibkr", "ACCOUNT", Sensitivity::Public),
        ("alpaca", "CLIENT_ID", Sensitivity::Public),
        ("hyperliquid", "ACCOUNT_ADDRESS", Sensitivity::Public),
        ("polymarket", "RELAYER_API_KEY_ADDRESS", Sensitivity::Public),
    ] {
        assert_eq!(field_sensitivity(venue, field), want, "{venue} {field}");
    }
    // Every field a form OFFERS is classified by its schema row — none falls through to the
    // fail-closed default by accident.
    for (venue, field) in offered() {
        let declared = schema(venue).field(field).expect("an offered field is a schema field");
        let want = if declared.secret { Sensitivity::Secret } else { Sensitivity::Public };
        assert_eq!(field_sensitivity(venue, field), want, "{venue} {field}");
    }

    // FAIL CLOSED: a field nobody has classified is a secret, not a readable field.
    for (venue, field) in [("binance", "SOMETHING_NEW"), ("not-a-venue", "LOGIN"), ("ibkr", "WAT")]
    {
        assert_eq!(
            field_sensitivity(venue, field),
            Sensitivity::Secret,
            "{venue} {field} must fail CLOSED — an unclassified field may not be rendered in the \
             clear"
        );
    }
}
