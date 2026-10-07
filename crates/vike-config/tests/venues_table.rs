//! The gate on `policy.venues.*` — the per-venue arming CEILINGS, end to end through the real
//! loader, the real writer and the real boot disclosure.
//!
//! `vike_config::venue_mode`'s own `#[cfg(test)]` module pins the TYPE (the `paper < demo < live`
//! ordering, `cap` = `min`, the filled default). This file pins what a deployment actually touches,
//! which is the part a unit test over a struct cannot reach:
//!
//! 1. [`the_default_is_paper_for_every_roster_venue_through_the_real_loader`] — with no files and
//!    no environment, every roster venue resolves to `paper`. Exhaustive over
//!    `vike_model::VENUES`, so a new bridge crate reddens it until the venue has a ceiling.
//! 2. [`a_venue_ceiling_is_disclosed_per_venue_by_config_shows_own_builder`] — the FILLED default
//!    is what makes the setting visible at all. ⚠ An empty map would leave
//!    `crates/vike-config/tests/provenance.rs`'s completeness gate passing VACUOUSLY (its walk only
//!    records a leaf at a non-object node), so the ceiling would exist, validate, and be invisible
//!    to the one command whose job is disclosing settings.
//! 3. [`an_unknown_venue_and_an_unknown_mode_are_each_refused_by_name`] — the two halves of
//!    validation, which fail in two different places for a structural reason.
//! 4. [`a_venue_ceiling_round_trips_through_set_setting_preserving_comments`] — the GUI/wire write
//!    path reaches a nested key with no change to the writer, and an operator's comments survive.
//! 5. [`the_boot_disclosure_spends_one_line_on_the_whole_table`] — ONE line, not one per venue.
//! 6. [`nothing_in_the_environment_can_set_a_venue_ceiling`] — the sealed-policy property, asserted
//!    for the new key rather than assumed from the type.

use std::collections::HashMap;
use std::path::Path;

use vike_config::{
    CliOverrides, Origin, StoreLayer, VenueMode, boot_lines, describe_with_source, load,
    load_with_source,
};
use vike_model::VENUES;
use vike_secrets::{ArmingRow, StoredSettings};

fn env(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
}

/// `[venues]`-shaped rows for `(venue, mode)` pairs — the direct replacement for this file's old
/// `write(dir, POLICY_FILE, "[venues]\n...")` fixtures now that there is no settings file to write.
fn venue_rows(pairs: &[(&str, &str)]) -> StoredSettings {
    StoredSettings {
        arming: pairs
            .iter()
            .map(|(venue, mode)| ArmingRow {
                venue: (*venue).to_string(),
                label: None,
                mode: (*mode).to_string(),
                max_exposure: None,
            })
            .collect(),
        ..Default::default()
    }
}

/// The store arm for a fixture that plants no settings database.
///
/// [`boot_lines`] takes one since 2026-09-22 so a disclosure cannot describe a source the process
/// did not resolve from (`crates/vike-config/src/boot.rs`'s module doc). `NotConsulted` is the arm
/// it was hard-wired to before that, so the two assertions below measure the same rendering they
/// were written against; `crates/vike-config/tests/boot.rs` is where that rendering is proven,
/// arm by arm, today.
fn no_store() -> StoreLayer<'static> {
    StoreLayer::NotConsulted(
        "crates/vike-config/tests/venues_table.rs — this fixture plants no settings database",
    )
}

/// Rows naming two venues — the shape the the CI box daemon would use to hold seven of its nine
/// accidentally-live venues down to paper.
fn two_venues() -> StoredSettings {
    venue_rows(&[("bybit", "live"), ("binance", "demo")])
}

// ------------------------------------------------------------------------------------------------
// 1 + 2. The default, and the fact that it is disclosed
// ------------------------------------------------------------------------------------------------

/// **Every roster venue defaults to `paper`, through the loader a binary really calls.** Driven by
/// `load(None, &{})` — no project, no file, no environment — rather than by asserting about
/// `Policy::default()`, so a default that stopped being the no-file answer fails here too.
#[test]
fn the_default_is_paper_for_every_roster_venue_through_the_real_loader() {
    let settings = load(None, &HashMap::new()).expect("defaults always load");
    assert_eq!(settings.policy.venues.len(), VENUES.len(), "one entry per venue, not an empty map");
    for venue in VENUES {
        assert_eq!(
            settings.policy.venues.get(venue),
            VenueMode::Paper,
            "{venue} must be capped at paper with no policy.venues row — a ceiling's absent-row \
             answer is its safest one"
        );
    }
}

/// The disclosure half: `config show`'s own row builder emits ONE row per venue, each attributed to
/// the layer that set it — the property a map with no entries would silently lose.
#[test]
fn a_venue_ceiling_is_disclosed_per_venue_by_config_shows_own_builder() {
    let dir = tempfile::tempdir().unwrap();
    let rows = two_venues();

    let d = describe_with_source(
        Some(dir.path()),
        StoreLayer::Rows { rows: &rows, adopted: None },
        &env(&[]),
    )
    .expect("the rows load");
    let disclosed = vike_config::file_rows(&d, Some("policy.venues"), false);
    assert_eq!(disclosed.len(), VENUES.len(), "one disclosed row per roster venue: {disclosed:?}");

    let row = |venue: &str| {
        let key = format!("policy.venues.{venue}");
        disclosed
            .iter()
            .find(|r| r.key == key)
            .unwrap_or_else(|| panic!("no row for {key}"))
            .clone()
    };
    assert_eq!(row("bybit").value, "live");
    assert_eq!(row("bybit").origin, "db", "attributed to the store that set it");
    assert_eq!(row("binance").value, "demo");
    // …and an unmentioned venue is disclosed too, honestly reporting the compiled-in default. A
    // venue that simply vanished from the output would be the silence this whole feature removes.
    assert_eq!(row("okx").value, "paper");
    assert_eq!(row("okx").origin, "default");
    assert!(!row("okx").adjusted, "nothing adjusts a ceiling");

    // ⚠ `policy.*` keys have their own consumption gate, so `config show` must NOT warn about them
    // as unread — that column would be warning about ceilings which ARE enforced.
    assert!(row("bybit").consumed, "a policy key is not reported unread by the CONSUMPTION table");
}

// ------------------------------------------------------------------------------------------------
// 3. The two refusals
// ------------------------------------------------------------------------------------------------

/// **An unknown VENUE and an unknown MODE are each refused by name, and they fail in different
/// places** — the split `PolicyPatch::venues`' doc argues for. Asserted through `load`, which is
/// what a binary runs, rather than through `apply`.
#[test]
fn an_unknown_venue_and_an_unknown_mode_are_each_refused_by_name() {
    let dir = tempfile::tempdir().unwrap();
    let load_rows = |rows: &StoredSettings| {
        load_with_source(
            Some(dir.path()),
            StoreLayer::Rows { rows, adopted: None },
            &env(&[]),
            &CliOverrides::default(),
        )
    };

    // ⚠ Not a `Result::Err` any more (`docs/decisions/0086`): a row that will not apply is
    // `Settings::seal_refusal`, a MARK — see `crate::mirror::apply_rows`'s own doc for why.
    // The VENUE: `deny_unknown_fields` cannot see a map's keys, so this is the hand-written check.
    let msg = load_rows(&venue_rows(&[("bybitt", "paper")]))
        .expect("a row problem MARKS rather than hard-erroring")
        .seal_refusal
        .expect("a ceiling on no venue must be marked");
    assert!(msg.contains("venues.bybitt"), "names the offending key: {msg}");
    assert!(msg.contains("names no venue"), "{msg}");
    assert!(msg.contains("Did you mean `bybit`?"), "a near miss is suggested: {msg}");
    for venue in VENUES {
        assert!(msg.contains(*venue), "the legal roster must be listed; {venue} missing: {msg}");
    }

    // The MODE: serde's own variant error, naming the legal three — the `halt_admit` idiom.
    let msg = load_rows(&venue_rows(&[("bybit", "mainnet")]))
        .expect("a row problem MARKS rather than hard-erroring")
        .seal_refusal
        .expect("an illegal tier must be marked");
    for mode in VenueMode::ALL {
        assert!(msg.contains(mode.as_str()), "the legal modes must be named; {mode} not in {msg}");
    }

    // …and legal rows still load, so neither refusal is firing on everything.
    let s = load_rows(&two_venues()).expect("legal `venues` rows load");
    assert_eq!(s.policy.venues.get("bybit"), VenueMode::Live);
}

// ------------------------------------------------------------------------------------------------
// 4. The write path
// ------------------------------------------------------------------------------------------------

/// **The GUI/wire write path reaches `policy.venues.<venue>` with no change to the writer**
/// (`docs/decisions/0086`: `vike_config::write_setting_row`, one `venue_arming` row) — and the
/// sibling row survives untouched.
#[test]
fn a_venue_ceiling_round_trips_through_write_setting_row() {
    let dir = tempfile::tempdir().unwrap();
    vike_secrets::plant_settings_rows(
        dir.path(),
        &vike_secrets::StoredSettings {
            settings: vec![],
            arming: vec![
                vike_secrets::ArmingRow {
                    venue: "bybit".to_string(),
                    label: None,
                    mode: "paper".to_string(),
                    max_exposure: None,
                },
                vike_secrets::ArmingRow {
                    venue: "binance".to_string(),
                    label: None,
                    mode: "demo".to_string(),
                    max_exposure: None,
                },
            ],
            ..Default::default()
        },
    )
    .expect("seed");

    let report = vike_config::write_setting_row(
        dir.path(),
        "policy.venues.bybit",
        "live",
        std::time::Duration::from_millis(500),
    )
    .expect("a nested policy key is writable today");
    assert_eq!(report.old_value.as_deref(), Some("paper"));
    assert_eq!(report.new_value, "live");

    // …and the row is one the next boot accepts, with the ceiling actually moved.
    //
    // ⚠ NOT the bare `load()` — that entry point deliberately CONSULTS NO STORE
    // (`StoreLayer::NotConsulted`; it exists for library callers that are not a composition
    // root) and would silently answer from compiled-in defaults regardless of what was just
    // written. `StoreLayer::of` over a fresh `read_settings_in` is the real composition-root
    // resolution.
    let resolve = |dir: &Path| {
        let read = vike_secrets::read_settings_in(dir);
        let mut refusal = String::new();
        let source = StoreLayer::of(Some(&read), &mut refusal);
        vike_config::load_with_source(Some(dir), source, &HashMap::new(), &Default::default())
    };
    let s = resolve(dir.path()).expect("the store loads");
    assert_eq!(s.policy.venues.get("bybit"), VenueMode::Live);
    assert_eq!(s.policy.venues.get("binance"), VenueMode::Demo, "the sibling row is untouched");

    // A write naming no venue is refused by the LOADER before anything commits — the writer
    // validates the candidate with the same `apply` a restart would run.
    let err = vike_config::write_setting_row(
        dir.path(),
        "policy.venues.bybitt",
        "live",
        std::time::Duration::from_millis(500),
    )
    .expect_err("a ceiling on a venue that does not exist must not be written");
    assert!(err.to_string().contains("bybitt"), "{err}");
    let after = resolve(dir.path()).expect("still loads");
    assert_eq!(
        after.policy.venues.get("bybit"),
        VenueMode::Live,
        "a refused write changes nothing"
    );
}

// ------------------------------------------------------------------------------------------------
// 5. The boot disclosure
// ------------------------------------------------------------------------------------------------

/// **ONE line for the whole table, not one line per venue.**
///
/// `crates/vike-config/src/boot.rs`'s `ALWAYS` prints every `policy.*` row whether or not anything
/// set it — correct for the four flat ceilings and, for a table with one row per roster venue,
/// fourteen lines of "paper" on every start of every binary. A startup banner nobody reads is worth
/// as little as no banner at all, so the whole table collapses to one aggregated line.
#[test]
fn the_boot_disclosure_spends_one_line_on_the_whole_table() {
    let dir = tempfile::tempdir().unwrap();

    // The untouched install: one line, and it says plainly that nothing configured any of it.
    let lines = boot_lines(Some(dir.path()), no_store(), &HashMap::new());
    let mentions: Vec<&String> = lines.iter().filter(|l| l.contains("policy.venues")).collect();
    assert_eq!(mentions.len(), 1, "the whole table is ONE line: {mentions:?}");
    let line = mentions[0];
    assert!(!line.contains("policy.venues."), "no per-venue KEY is rendered: {line}");
    assert!(line.starts_with("setting: policy.venues = "), "{line}");
    assert!(line.contains("live=none") && line.contains("demo=none"), "{line}");
    assert!(line.contains(&format!("paper=<{}>", VENUES.len())), "{line}");
    assert!(line.contains("compiled-in default"), "the origin half: {line}");
    for venue in VENUES {
        assert!(!line.contains(*venue), "an all-default table names no venue; {venue} in {line}");
    }

    // …and a configured one NAMES the risk-bearing tiers, which is the whole reason to print it.
    let rows = two_venues();
    let lines = boot_lines(
        Some(dir.path()),
        StoreLayer::Rows { rows: &rows, adopted: None },
        &HashMap::new(),
    );
    let mentions: Vec<&String> = lines.iter().filter(|l| l.contains("policy.venues")).collect();
    assert_eq!(mentions.len(), 1, "still ONE line: {mentions:?}");
    let line = mentions[0];
    assert!(line.contains("live=[bybit]"), "a LIVE venue is named in full: {line}");
    assert!(line.contains("demo=[binance]"), "{line}");
    assert!(line.contains(&format!("paper=<{}>", VENUES.len() - 2)), "{line}");
    assert!(line.contains("db sets 2"), "the origin half counts the store's rows: {line}");

    // The four flat ceilings still print one row each — the aggregate must not have swallowed them.
    for ceiling in ["policy.max_leverage", "policy.halt_admit"] {
        let needle = format!("setting: {ceiling} = ");
        assert!(lines.iter().any(|l| l.starts_with(&needle)), "{ceiling} vanished from the block");
    }
}

// ------------------------------------------------------------------------------------------------
// 6. The sealed-policy property, for this key
// ------------------------------------------------------------------------------------------------

/// **No environment can set or move a venue ceiling.** `Policy` implements neither `EnvOverride`
/// nor `CliOverride` (both sealed), so this is structural — but the property worth asserting is the
/// OBSERVABLE one, since a ceiling an exported variable can raise is not a ceiling and this one
/// governs whether a box trades real money.
///
/// Only already-declared variable names appear here: `crates/vike-ops/tests/settings/settings_registry.rs`
/// harvests every env-shaped string literal under `crates/`, so an invented `VIKE_*` name in a test
/// fails that gate as an undeclared read.
#[test]
fn nothing_in_the_environment_can_set_a_venue_ceiling() {
    let dir = tempfile::tempdir().unwrap();
    let rows = venue_rows(&[("bybit", "paper")]);

    // `VIKE_TRADEHUB_LIVE` is the closest thing that exists to an environment-settable arming gate
    // today (`vike_config::TRADEHUB_LIVE_ARMING` — the daemon's node-scoped live master switch), so
    // it is the variable somebody would REACH for; the other three are the removed notional ceiling
    // and two ordinary overrides, proving the env layer is live in this fixture rather than absent.
    let hostile = env(&[
        ("VIKE_TRADEHUB_LIVE", "1"),
        ("VIKE_RECONCILE", "1"),
        ("VIKE_MAX_ORDER_NOTIONAL", "999999"),
        ("VIKE_LOG_DIR", "/somewhere"),
    ]);
    let d = describe_with_source(
        Some(dir.path()),
        StoreLayer::Rows { rows: &rows, adopted: None },
        &hostile,
    )
    .expect("the rows load");
    assert_eq!(d.settings.policy.venues.get("bybit"), VenueMode::Paper, "the rows still decide");
    for row in d.rows.iter().filter(|r| r.key.starts_with("policy.venues.")) {
        assert!(
            matches!(row.origin, Origin::Db | Origin::Default),
            "{} claims {:?} — a venue ceiling has no env layer",
            row.key,
            row.origin
        );
    }
}

// ------------------------------------------------------------------------------------------------
// ⚠ Every writer (the CLI, the GUI and the daemon's own control channel) calls the SAME
// `vike_config::write_setting_row`, which changes the row directly (`docs/decisions/0086`).
// `a_venue_ceiling_round_trips_through_write_setting_row` above is the one write path's test.
// ------------------------------------------------------------------------------------------------
