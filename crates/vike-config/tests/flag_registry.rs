//! The FLAG STEWARDSHIP gate — Phase 4's machine check.
//!
//! `vike_config::Flags` is the set of operator toggles; `vike_config::FLAG_REGISTRY` is the table
//! giving each one an OWNER, a REVIEW DATE and the [`Disposition`] its review is expected to reach.
//! A table that is maintained by hand rots the moment someone adds a field and forgets a row, so
//! this file makes that a CI failure rather than something a reviewer has to notice.
//!
//! It gates five things, in rising order of what a bug would cost:
//!
//! 1. **Both directions of exhaustiveness.** A `Flags` field with no row fails; a row naming no
//!    field fails. The field set is read out of the type by SERIALIZING it (`Flags: Serialize`,
//!    `toml::Table::try_from`), not from a hand-written list — a hand-written list is the same
//!    thing this gate exists to replace.
//! 2. **Every row has an owner and a well-formed review date.** The literal ask of Phase 4: *a
//!    flag with no owner is a flag nobody will ever delete.*
//! 3. **No two rows share a field or an environment variable.** Two rows for one flag means two
//!    owners, which is none.
//! 4. **The ENV wiring is right, per row.** Setting only that row's variable flips only that row's
//!    field. This is the check that catches the real bug — a copy-pasted `apply_env` arm assigning
//!    the previous field, which no amount of reading catches and which would silently arm the
//!    wrong thing once Phase 6 makes this type live.
//! 5. **The FILE wiring is right, per row.** A `flags.toml` key IS the field name, proven by
//!    writing one and loading it, rather than asserted in a doc comment.
//!
//! ⚠ **A review date that has PASSED does not fail this gate**, deliberately. Failing on the
//! calendar would let a settings crate block an unrelated trading fix from merging on a Tuesday
//! morning. `parse_iso_date` returns the parsed triple precisely so that turning it into an
//! expiry is a small, deliberate later change — see `vike_config::flags`' module doc.
//!
//! Mirrors `crates/vike-ops/tests/settings_registry.rs` in shape: the table lives in `src`, the
//! exhaustiveness gate lives in `tests`.
//!
//! [`Disposition`]: vike_config::Disposition

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::Path;

use vike_config::{EnvOverride, FLAG_REGISTRY, Flags, load};

/// The `Flags` field names and their values, read out of the TYPE via `Serialize` so this can
/// never disagree with the struct definition.
fn fields_of(flags: Flags) -> BTreeMap<String, bool> {
    toml::Table::try_from(flags)
        .expect("Flags is a flat struct of bools and must serialize")
        .into_iter()
        .map(|(k, v)| {
            let b = v.as_bool().unwrap_or_else(|| panic!("{k} is not a boolean: {v:?}"));
            (k, b)
        })
        .collect()
}

/// The names of every field that is `true` in `flags`.
fn set_fields(flags: Flags) -> BTreeSet<String> {
    fields_of(flags).into_iter().filter(|(_, v)| *v).map(|(k, _)| k).collect()
}

/// Parse `YYYY-MM-DD` strictly, returning the triple. `None` for anything else.
///
/// Hand-rolled rather than reaching for a date crate: this crate's dependency set is
/// `vike-model + serde + toml` and a stewardship test is not a reason to widen the
/// order-signing binary's audit surface. Strict on purpose — `2026-1-1` and `2026-11-31` are both
/// rejected, because a date nobody can sort or diff is not a review date.
fn parse_iso_date(s: &str) -> Option<(u32, u32, u32)> {
    let parts: Vec<&str> = s.split('-').collect();
    if parts.len() != 3 {
        return None;
    }
    let (y, m, d) = (parts[0], parts[1], parts[2]);
    if (y.len(), m.len(), d.len()) != (4, 2, 2) {
        return None;
    }
    if !s.chars().all(|c| c.is_ascii_digit() || c == '-') {
        return None;
    }
    let (y, m, d) = (y.parse::<u32>().ok()?, m.parse::<u32>().ok()?, d.parse::<u32>().ok()?);
    if !(1..=12).contains(&m) {
        return None;
    }
    let leap = y % 4 == 0 && (y % 100 != 0 || y % 400 == 0);
    let last = match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        _ if leap => 29,
        _ => 28,
    };
    if !(1..=last).contains(&d) {
        return None;
    }
    Some((y, m, d))
}

// -------------------------------------------------------------------------------------------
// 1 — exhaustiveness, both directions
// -------------------------------------------------------------------------------------------

/// Every `Flags` field has a row. **This is the test that fires when a flag is added without an
/// owner and a review date** — the whole point of the phase.
#[test]
fn every_flag_field_has_a_registry_row() {
    let fields: BTreeSet<String> = fields_of(Flags::default()).into_keys().collect();
    let rows: BTreeSet<String> = FLAG_REGISTRY.iter().map(|m| m.field.to_string()).collect();
    let missing: Vec<&String> = fields.difference(&rows).collect();
    assert!(
        missing.is_empty(),
        "these `Flags` fields have no `FLAG_REGISTRY` row — every flag needs an owner and a \
         review date, because a flag with no owner is a flag nobody will ever delete: {missing:#?}"
    );
}

/// …and no row names a field that no longer exists. Keeps the table honest when a flag is deleted
/// — the outcome the review dates exist to produce.
#[test]
fn every_registry_row_names_a_real_flag_field() {
    let fields: BTreeSet<String> = fields_of(Flags::default()).into_keys().collect();
    let stale: Vec<&str> =
        FLAG_REGISTRY.iter().map(|m| m.field).filter(|f| !fields.contains(*f)).collect();
    assert!(
        stale.is_empty(),
        "`FLAG_REGISTRY` rows naming no `Flags` field — delete them: {stale:#?}"
    );
}

// -------------------------------------------------------------------------------------------
// 2 — every row is actually stewarded
// -------------------------------------------------------------------------------------------

#[test]
fn every_flag_has_an_owner() {
    let unowned: Vec<&str> =
        FLAG_REGISTRY.iter().filter(|m| m.owner.trim().is_empty()).map(|m| m.field).collect();
    assert!(unowned.is_empty(), "flags with a blank owner: {unowned:#?}");
    // A placeholder is a blank owner wearing a costume — reject the ones people actually type.
    const PLACEHOLDERS: &[&str] = &["tbd", "todo", "n/a", "na", "-", "?", "unknown", "nobody"];
    let placeheld: Vec<&str> = FLAG_REGISTRY
        .iter()
        .filter(|m| PLACEHOLDERS.contains(&m.owner.trim().to_ascii_lowercase().as_str()))
        .map(|m| m.field)
        .collect();
    assert!(
        placeheld.is_empty(),
        "flags whose owner is a placeholder, not a person: {placeheld:#?}"
    );
}

#[test]
fn every_flag_has_a_well_formed_review_date() {
    let bad: Vec<(&str, &str)> = FLAG_REGISTRY
        .iter()
        .filter(|m| parse_iso_date(m.review).is_none())
        .map(|m| (m.field, m.review))
        .collect();
    assert!(bad.is_empty(), "review dates that are not a strict ISO `YYYY-MM-DD`: {bad:#?}");
}

/// The dates must be sortable against each other for a "what is due next" listing to mean
/// anything, which strict `YYYY-MM-DD` gives for free — pinned here so a future "01/11/2026"
/// cannot creep in behind a looser parser.
#[test]
fn review_dates_sort_lexicographically_as_dates() {
    let mut parsed: Vec<((u32, u32, u32), &str)> = FLAG_REGISTRY
        .iter()
        .map(|m| (parse_iso_date(m.review).expect("checked by the test above"), m.review))
        .collect();
    parsed.sort();
    let mut lexical: Vec<&str> = FLAG_REGISTRY.iter().map(|m| m.review).collect();
    lexical.sort_unstable();
    let chronological: Vec<&str> = parsed.into_iter().map(|(_, s)| s).collect();
    assert_eq!(chronological, lexical);
}

// -------------------------------------------------------------------------------------------
// 3 — one row per flag, one variable per flag
// -------------------------------------------------------------------------------------------

#[test]
fn no_flag_and_no_variable_is_claimed_twice() {
    let mut fields = BTreeSet::new();
    for m in FLAG_REGISTRY {
        assert!(fields.insert(m.field), "{} has two rows — two owners is none", m.field);
    }
    let mut envs = BTreeSet::new();
    for m in FLAG_REGISTRY {
        assert!(envs.insert(m.env), "{} is claimed by two flags", m.env);
    }
}

#[test]
fn every_variable_name_has_env_var_shape() {
    for m in FLAG_REGISTRY {
        assert!(!m.env.is_empty(), "{}: empty variable name", m.field);
        assert!(
            m.env.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_'),
            "{}: {} is not an environment variable name",
            m.field,
            m.env
        );
        assert!(
            m.env.starts_with(|c: char| c.is_ascii_uppercase()),
            "{}: {} must start with a letter",
            m.field,
            m.env
        );
    }
}

// -------------------------------------------------------------------------------------------
// 4 & 5 — the wiring, per row
// -------------------------------------------------------------------------------------------

/// Nothing is on until something turns it on. Checked over the serialized field set so it cannot
/// go stale when a field is added.
#[test]
fn every_flag_defaults_off() {
    let on = set_fields(Flags::default());
    assert!(on.is_empty(), "these flags default ON, which the type's contract forbids: {on:#?}");
}

/// **The copy-paste catcher.** For each row, an environment holding ONLY that row's variable must
/// flip ONLY that row's field. An `apply_env` arm assigning the wrong field passes every other
/// test in this file and fails this one.
#[test]
fn each_variable_flips_exactly_its_own_field() {
    for m in FLAG_REGISTRY {
        let env = HashMap::from([(m.env.to_string(), "1".to_string())]);
        let mut flags = Flags::default();
        flags.apply_env(&env).unwrap_or_else(|e| panic!("{}=1: {e}", m.env));
        let on = set_fields(flags);
        assert_eq!(
            on,
            BTreeSet::from([m.field.to_string()]),
            "{}=1 must set exactly `{}` — check that arm of `apply_env`",
            m.env,
            m.field
        );
    }
}

/// …and the same for the FILE layer: `flags.toml` keys ARE the field names, and each one sets its
/// own field. Driven through the public `load`, so it exercises the real precedence chain rather
/// than the private patch-apply.
#[test]
fn each_flags_toml_key_sets_exactly_its_own_field() {
    let home = tempfile::tempdir().unwrap();
    for m in FLAG_REGISTRY {
        write_flags(home.path(), &format!("{} = true\n", m.field));
        let settings = load(Some(home.path()), &HashMap::new())
            .unwrap_or_else(|e| panic!("{} = true: {e}", m.field));
        assert_eq!(
            set_fields(settings.flags),
            BTreeSet::from([m.field.to_string()]),
            "flags.toml `{} = true` must set exactly that field",
            m.field
        );
    }
}

/// A `flags.toml` key that is not a field is rejected BY NAME rather than silently ignored — the
/// case that actually bites, an operator who believes they armed something.
#[test]
fn an_unknown_flags_toml_key_is_rejected_by_name() {
    let home = tempfile::tempdir().unwrap();
    write_flags(home.path(), "reconcil = true\n");
    let err = load(Some(home.path()), &HashMap::new()).unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("flags.toml"), "{msg}");
    assert!(msg.contains("reconcil"), "{msg}");
}

/// Env beats the file, for every flag — the property the design calls *correct for flags*. Uses
/// the whole registry rather than one example so a future field cannot miss its `apply_env` arm
/// and still pass (an unwired field would keep the file's `true` and fail here).
///
/// `poly_redeem_halt` is the documented exception and is skipped: it is armed by PRESENCE, so no
/// environment value can turn it back off. That asymmetry is today's kill-switch behaviour, is
/// pinned by a unit test in `flags.rs`, and is precisely why it is named here rather than quietly
/// excluded.
#[test]
fn the_environment_overrides_the_file_for_every_flag() {
    let home = tempfile::tempdir().unwrap();
    for m in FLAG_REGISTRY.iter().filter(|m| m.field != "poly_redeem_halt") {
        write_flags(home.path(), &format!("{} = true\n", m.field));
        let env = HashMap::from([(m.env.to_string(), "0".to_string())]);
        let settings = load(Some(home.path()), &env).unwrap_or_else(|e| panic!("{}: {e}", m.env));
        assert!(
            set_fields(settings.flags).is_empty(),
            "{}=0 must beat `{} = true` in flags.toml",
            m.env,
            m.field
        );
    }
}

/// A truthy typo on ANY flag is an error naming the variable and the value, not a silent false.
/// Again registry-wide: a new field wired with a bespoke fuzzy parse would pass the single-example
/// unit test in `flags.rs` and fail here.
///
/// `poly_redeem_halt` is skipped for the same reason as above — every value arms it by design, so
/// there is no typo to reject.
#[test]
fn a_truthy_typo_is_an_error_for_every_flag() {
    for m in FLAG_REGISTRY.iter().filter(|m| m.field != "poly_redeem_halt") {
        let env = HashMap::from([(m.env.to_string(), "true".to_string())]);
        let err = Flags::default().apply_env(&env).unwrap_err();
        let msg = err.to_string();
        assert!(msg.starts_with(&format!("{}=true: ", m.env)), "{}: {msg}", m.field);
    }
}

fn write_flags(home: &Path, body: &str) {
    std::fs::write(home.join("flags.toml"), body).unwrap();
}

// -------------------------------------------------------------------------------------------
// The date parser's own tests — a gate is only as good as the check it runs
// -------------------------------------------------------------------------------------------

#[test]
fn the_date_parser_is_strict() {
    assert_eq!(parse_iso_date("2026-11-01"), Some((2026, 11, 1)));
    assert_eq!(parse_iso_date("2028-02-29"), Some((2028, 2, 29)), "2028 is a leap year");
    for bad in [
        "",
        "2026",
        "2026-11",
        "2026-1-1",
        "26-11-01",
        "2026-13-01",
        "2026-00-01",
        "2026-11-00",
        "2026-11-31",
        "2027-02-29",
        "2026/11/01",
        "2026-11-01 ",
        "next quarter",
        "2026-11-0a",
    ] {
        assert_eq!(parse_iso_date(bad), None, "{bad:?} must not parse");
    }
}
