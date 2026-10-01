//! The `venue.*` block of `vike-cli config show` — every DECLARED venue field
//! (`vike_model::venue_fields::VENUE_FIELDS`) with its value and where the value came from, plus
//! every stored row the catalog does not declare, flagged as read by nothing. Secret values are
//! never printed.

use std::collections::{BTreeMap, BTreeSet};

use vike_model::venue_fields::{VENUE_FIELDS, VenueField, venue_field};
use vike_secrets::venue_setting::{SettingTier, VenueSettings, venue_setting_key};

/// One printed row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct VenueRow {
    /// The dotted key an operator types.
    pub(crate) key: String,
    /// The value in force, `-` for an empty default, `<set>` for a secret or an undeclared row.
    pub(crate) value: String,
    /// `database`, `default`, or `undeclared` (a stored row no field declares, or one stored at a
    /// scope its field does not use — nothing reads either shape).
    pub(crate) origin: &'static str,
    /// Whether the field is a secret.
    pub(crate) secret: bool,
    /// What the field does.
    pub(crate) doc: &'static str,
}

const TIERS: [SettingTier; 3] = [SettingTier::Paper, SettingTier::Demo, SettingTier::Live];

/// The row stored for `field` at EXACTLY `tier` — never [`VenueSettings::get`]'s tier-fallback.
///
/// `get` answers "the row for this tier, else the machine-scoped one", which is right for a
/// READER that wants an effective value. This display wants the opposite question — "did an
/// operator actually store a value AT THIS SCOPE" — because a display built on `get` would render
/// a stray machine-scoped row as though it were every tier's own stored value, and a stray
/// tier-scoped row under a machine-scoped field's `Any` slot could never surface at all (`get`'s
/// fallback only ever targets `Any`, so it is a no-op when the query tier already IS `Any`).
fn exact_stored<'a>(s: &'a VenueSettings, tier: SettingTier, field: &str) -> Option<&'a str> {
    s.rows().find(|(t, f, _)| *t == tier && f.eq_ignore_ascii_case(field)).map(|(_, _, v)| v)
}

fn declared_row(f: &VenueField, tier: SettingTier, stored: Option<&str>) -> VenueRow {
    let (value, origin) = match stored {
        Some(_) if f.secret => ("<set>".to_string(), "database"),
        Some(v) => (v.to_string(), "database"),
        None if f.default.is_empty() => ("-".to_string(), "default"),
        None => (f.default.to_string(), "default"),
    };
    VenueRow {
        key: venue_setting_key(f.venue, tier.as_str(), f.field),
        value,
        origin,
        secret: f.secret,
        doc: f.doc,
    }
}

/// Every declared field (a tier-scoped field once per tier), matched to a stored row by EXACT
/// scope, then every stored row that neither loop above claimed — either its field is declared by
/// nothing, or it is declared but stored at a scope that field does not use — flagged rather than
/// silently rendered as if it were the matching declared row's value. Narrowed by `filter` (a
/// substring of the key).
pub(crate) fn venue_rows(
    stored: &BTreeMap<String, VenueSettings>,
    filter: Option<&str>,
) -> Vec<VenueRow> {
    let mut out = Vec::new();
    // `(venue, tier, FIELD as stored)` — every row a declared field claimed at ITS OWN scope, so
    // the second pass below can tell "no reader will find this" from "already shown above".
    let mut claimed: BTreeSet<(&str, SettingTier, String)> = BTreeSet::new();
    for f in VENUE_FIELDS {
        let s = stored.get(f.venue);
        let mut claim_and_push = |t: SettingTier| {
            let value = s.and_then(|s| exact_stored(s, t, f.field));
            if value.is_some() {
                claimed.insert((f.venue, t, f.field.to_ascii_uppercase()));
            }
            out.push(declared_row(f, t, value));
        };
        if f.tier_scoped {
            for t in TIERS {
                claim_and_push(t);
            }
        } else {
            claim_and_push(SettingTier::Any);
        }
    }
    for s in stored.values() {
        for (tier, field, _value) in s.rows() {
            if claimed.contains(&(s.venue(), tier, field.to_string())) {
                continue;
            }
            let field_lower = field.to_ascii_lowercase();
            let doc = if venue_field(s.venue(), &field_lower).is_some() {
                "declared, but stored at a scope that field does not use — this row is not read \
                 as that field's value"
            } else {
                "declared by no field: NOTHING READS THIS ROW"
            };
            out.push(VenueRow {
                key: venue_setting_key(s.venue(), tier.as_str(), &field_lower),
                value: "<set>".to_string(),
                origin: "undeclared",
                secret: false,
                doc,
            });
        }
    }
    out.retain(|r| filter.is_none_or(|p| r.key.contains(p)));
    out
}

/// The human table.
pub(crate) fn print_venue_table(rows: &[VenueRow]) {
    if rows.is_empty() {
        return;
    }
    println!();
    println!(
        "venue settings — `vike-cli config set venue.<venue>[.<tier>].<field> <value>` (restart to apply):"
    );
    let width = rows.iter().map(|r| r.key.len()).max().unwrap_or(0);
    for r in rows {
        let mark = if r.origin == "undeclared" { "⚠ " } else { "  " };
        println!("{mark}{:<width$}  {:<24}  {}", r.key, r.value, r.origin);
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use vike_secrets::VenueSettingRow;
    use vike_secrets::venue_setting::VenueSettings;

    use super::*;

    fn stored(rows: &[(&str, Option<&str>, &str, &str)]) -> BTreeMap<String, VenueSettings> {
        let rows: Vec<VenueSettingRow> = rows
            .iter()
            .map(|(v, t, f, val)| VenueSettingRow {
                venue: (*v).to_string(),
                tier: t.map(str::to_string),
                field: (*f).to_string(),
                value: (*val).to_string(),
            })
            .collect();
        let mut out = BTreeMap::new();
        for r in &rows {
            out.entry(r.venue.clone()).or_insert_with(|| VenueSettings::from_rows(&r.venue, &rows));
        }
        out
    }

    #[test]
    fn every_declared_field_is_listed_with_its_origin() {
        let rows = venue_rows(&stored(&[("polymarket", None, "PROXY_PORT", "11080")]), None);
        let port = rows.iter().find(|r| r.key == "venue.polymarket.proxy_port").unwrap();
        assert_eq!((port.value.as_str(), port.origin), ("11080", "database"));
        let host = rows.iter().find(|r| r.key == "venue.polymarket.proxy_host").unwrap();
        assert_eq!((host.value.as_str(), host.origin), ("127.0.0.1", "default"));
        for tier in ["paper", "demo", "live"] {
            assert!(rows.iter().any(|r| r.key == format!("venue.ibkr.{tier}.port")), "{tier}");
        }
    }

    #[test]
    fn a_secret_value_and_an_undeclared_row_are_never_printed() {
        let rows = venue_rows(
            &stored(&[
                ("polymarket", None, "SOCKS_PROXY", "socks5h://u:hunter2@h:1"),
                ("binance", None, "MAINNET", "1"),
            ]),
            None,
        );
        let sp = rows.iter().find(|r| r.key == "venue.polymarket.socks_proxy").unwrap();
        assert_eq!(sp.value, "<set>");
        let stray = rows.iter().find(|r| r.key == "venue.binance.mainnet").unwrap();
        assert_eq!(stray.origin, "undeclared");
        assert_eq!(stray.value, "<set>");
        assert!(!format!("{rows:?}").contains("hunter2"));
    }

    #[test]
    fn the_filter_narrows_by_key() {
        let rows = venue_rows(&BTreeMap::new(), Some("ibkr"));
        assert!(!rows.is_empty() && rows.iter().all(|r| r.key.starts_with("venue.ibkr.")));
    }

    /// **PF-36, one direction.** A MACHINE-scoped row must not satisfy a TIER-SCOPED field's
    /// display slot. Under the rejected `s.get(tier, field)` design — `get`'s own tier-fallback
    /// shortcut — this row would have rendered as though it were EVERY tier's own stored value
    /// (`get(Paper, "port")` falls through to the `Any` row when no `Paper` row exists). All three
    /// tier rows this suite's OTHER test already found present must therefore show the field's
    /// DEFAULT here, never the stray value — and the stray row itself must still be shown, flagged,
    /// rather than vanishing (the pre-PF-36 second loop's `venue_field(..).is_none()` check let a
    /// row for a DECLARED field name disappear silently once it existed at any scope).
    #[test]
    fn a_machine_scoped_row_does_not_satisfy_a_tier_scoped_field_and_is_flagged() {
        let rows = venue_rows(&stored(&[("ibkr", None, "PORT", "7000")]), None);
        for tier in ["paper", "demo", "live"] {
            let key = format!("venue.ibkr.{tier}.port");
            let row = rows.iter().find(|r| r.key == key).unwrap_or_else(|| panic!("{key} missing"));
            assert_eq!(
                (row.value.as_str(), row.origin),
                ("-", "default"),
                "{key}: a machine-scoped row must not render as this tier's own stored value"
            );
        }
        let stray = rows
            .iter()
            .find(|r| r.key == "venue.ibkr.port")
            .expect("the machine-scoped row must be shown, flagged, not silently dropped");
        assert_eq!(stray.origin, "undeclared");
    }

    /// **PF-36, the other direction.** A TIER-scoped row must not satisfy a MACHINE-scoped field's
    /// display slot. `get(Any, field)` can never reach it through the fallback (the fallback always
    /// targets `Any`, so it is a no-op when the query is already `Any`) — this direction was never
    /// about `get` leaking a value across scopes, it is about the row not silently VANISHING, which
    /// the pre-PF-36 second loop let happen for any field the catalog declares at all, regardless of
    /// the scope it was actually stored at.
    #[test]
    fn a_tier_scoped_row_does_not_satisfy_a_machine_scoped_field_and_is_flagged() {
        let rows =
            venue_rows(&stored(&[("polymarket", Some("demo"), "PROXY_HOST", "<host>")]), None);
        let any_row = rows.iter().find(|r| r.key == "venue.polymarket.proxy_host").unwrap();
        assert_eq!(
            (any_row.value.as_str(), any_row.origin),
            ("127.0.0.1", "default"),
            "the machine-scoped display must show the default, never the stray tier row's value"
        );
        let stray = rows
            .iter()
            .find(|r| r.key == "venue.polymarket.demo.proxy_host")
            .expect("the tier-scoped row must be shown, flagged, not silently dropped");
        assert_eq!(stray.origin, "undeclared");
    }
}
