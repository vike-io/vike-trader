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

use vike_config::load::POLICY_FILE;
use vike_config::{Origin, SettingsFile, VenueMode, boot_lines, describe, load};
use vike_model::VENUES;

fn env(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
}

fn write(dir: &Path, name: &str, body: &str) {
    std::fs::write(dir.join(name), body).expect("write settings file");
}

/// A `policy.toml` naming two venues — the shape the the CI box daemon would use to hold seven of its
/// nine accidentally-live venues down to paper.
const TWO_VENUES: &str = "[venues]\nbybit = \"live\"\nbinance = \"demo\"\n";

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
            "{venue} must be capped at paper with no policy.toml — a ceiling's absent-file answer \
             is its safest one"
        );
    }
}

/// The disclosure half: `config show`'s own row builder emits ONE row per venue, each attributed to
/// the layer that set it — the property a map with no entries would silently lose.
#[test]
fn a_venue_ceiling_is_disclosed_per_venue_by_config_shows_own_builder() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), POLICY_FILE, TWO_VENUES);

    let d = describe(Some(dir.path()), &env(&[])).expect("the file loads");
    let rows = vike_config::file_rows(&d, Some("policy.venues"), false);
    assert_eq!(rows.len(), VENUES.len(), "one disclosed row per roster venue: {rows:?}");

    let row = |venue: &str| {
        let key = format!("policy.venues.{venue}");
        rows.iter().find(|r| r.key == key).unwrap_or_else(|| panic!("no row for {key}")).clone()
    };
    assert_eq!(row("bybit").value, "live");
    assert_eq!(row("bybit").origin, POLICY_FILE, "attributed to the file that set it");
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

    // The VENUE: `deny_unknown_fields` cannot see a map's keys, so this is the hand-written check.
    write(dir.path(), POLICY_FILE, "[venues]\nbybitt = \"paper\"\n");
    let err = load(Some(dir.path()), &env(&[])).expect_err("a ceiling on no venue must refuse");
    let msg = err.to_string();
    assert!(msg.contains("venues.bybitt"), "names the offending key: {msg}");
    assert!(msg.contains("names no venue"), "{msg}");
    assert!(msg.contains("Did you mean `bybit`?"), "a near miss is suggested: {msg}");
    for venue in VENUES {
        assert!(msg.contains(*venue), "the legal roster must be listed; {venue} missing: {msg}");
    }

    // The MODE: serde's own variant error, naming the legal three — the `halt_admit` idiom.
    write(dir.path(), POLICY_FILE, "[venues]\nbybit = \"mainnet\"\n");
    let msg = load(Some(dir.path()), &env(&[])).expect_err("an illegal tier").to_string();
    assert!(msg.contains(POLICY_FILE), "the file to open: {msg}");
    for mode in VenueMode::ALL {
        assert!(msg.contains(mode.as_str()), "the legal modes must be named; {mode} not in {msg}");
    }

    // …and a legal file still loads, so neither refusal is firing on everything.
    write(dir.path(), POLICY_FILE, TWO_VENUES);
    let s = load(Some(dir.path()), &env(&[])).expect("a legal `[venues]` table loads");
    assert_eq!(s.policy.venues.get("bybit"), VenueMode::Live);
}

// ------------------------------------------------------------------------------------------------
// 4. The write path
// ------------------------------------------------------------------------------------------------

/// **The GUI/wire write path reaches `policy.venues.<venue>` with no change to the writer** — the
/// dotted-key walk in `crates/vike-config/src/write.rs`'s `set_setting` already creates
/// intermediate tables — and the operator's own comments survive the edit byte for byte.
///
/// The comment half is the point: `policy.toml` is hand-edited and its comments carry the reason a
/// ceiling is where it is (`# raised for the weekend`). A writer that re-serialized the parsed model
/// would delete every one of them, and this is the first NESTED key to travel that path.
#[test]
fn a_venue_ceiling_round_trips_through_set_setting_preserving_comments() {
    const BEFORE: &str = "\
# risk ceilings — reviewed 2026-08-21
max_leverage = 2.0  # perp desk only

[venues]
# bybit is the only account with a live budget
bybit = \"paper\"  # flip to live after the recon soak
binance = \"demo\"
";
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), POLICY_FILE, BEFORE);

    let w =
        vike_config::set_setting(dir.path(), SettingsFile::Policy, "policy.venues.bybit", "live")
            .expect("a nested policy key is writable today");
    assert_eq!(w.old_value.as_deref(), Some("\"paper\""));
    assert_eq!(w.new_value, "\"live\"");

    let after = std::fs::read_to_string(dir.path().join(POLICY_FILE)).unwrap();
    let (before_lines, after_lines): (Vec<&str>, Vec<&str>) =
        (BEFORE.lines().collect(), after.lines().collect());
    assert_eq!(before_lines.len(), after_lines.len(), "no line added or removed:\n{after}");
    for (b, a) in before_lines.iter().zip(&after_lines) {
        if b.starts_with("bybit") {
            assert_eq!(*a, "bybit = \"live\"  # flip to live after the recon soak");
        } else {
            assert_eq!(a, b, "an untouched line is untouched bytes");
        }
    }

    // …and the written file is one the next boot accepts, with the ceiling actually moved.
    let s = load(Some(dir.path()), &HashMap::new()).expect("the edited file loads");
    assert_eq!(s.policy.venues.get("bybit"), VenueMode::Live);
    assert_eq!(s.policy.venues.get("binance"), VenueMode::Demo, "the sibling key is untouched");

    // A write naming no venue is refused by the LOADER before a byte lands — the writer validates
    // the would-be file with the same `apply` a restart would run.
    let err =
        vike_config::set_setting(dir.path(), SettingsFile::Policy, "policy.venues.bybitt", "live")
            .expect_err("a ceiling on a venue that does not exist must not be written");
    assert!(err.to_string().contains("bybitt"), "{err}");
    assert_eq!(
        std::fs::read_to_string(dir.path().join(POLICY_FILE)).unwrap(),
        after,
        "a refused write changes NO byte"
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
    let lines = boot_lines(Some(dir.path()), &HashMap::new());
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
    write(dir.path(), POLICY_FILE, TWO_VENUES);
    let lines = boot_lines(Some(dir.path()), &HashMap::new());
    let mentions: Vec<&String> = lines.iter().filter(|l| l.contains("policy.venues")).collect();
    assert_eq!(mentions.len(), 1, "still ONE line: {mentions:?}");
    let line = mentions[0];
    assert!(line.contains("live=[bybit]"), "a LIVE venue is named in full: {line}");
    assert!(line.contains("demo=[binance]"), "{line}");
    assert!(line.contains(&format!("paper=<{}>", VENUES.len() - 2)), "{line}");
    assert!(line.contains("policy.toml sets 2"), "the origin half counts the file's keys: {line}");

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
/// Only already-declared variable names appear here: `crates/vike-ops/tests/settings_registry.rs`
/// harvests every env-shaped string literal under `crates/`, so an invented `VIKE_*` name in a test
/// fails that gate as an undeclared read.
#[test]
fn nothing_in_the_environment_can_set_a_venue_ceiling() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), POLICY_FILE, "[venues]\nbybit = \"paper\"\n");

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
    let d = describe(Some(dir.path()), &hostile).expect("the file loads");
    assert_eq!(d.settings.policy.venues.get("bybit"), VenueMode::Paper, "the file still decides");
    for row in d.rows.iter().filter(|r| r.key.starts_with("policy.venues.")) {
        assert!(
            matches!(row.origin, Origin::File(_) | Origin::Default),
            "{} claims {:?} — a venue ceiling has no env layer",
            row.key,
            row.origin
        );
    }
}
