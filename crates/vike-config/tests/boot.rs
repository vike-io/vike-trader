//! The gate on `vike_config::boot` — the startup disclosure a daemon logs after `vike_log::init`.
//!
//! The incident it answers is in `crates/vike-config/src/boot.rs`'s module doc: on the CI box the
//! project-root walk answered with an unrelated directory, the daemon loaded no policy and NO
//! CREDENTIALS, and every venue silently dropped to paper. Nothing was wrong with the code that
//! knew all of it; nothing rendered it.
//!
//! So the properties asserted here are the ones an operator reads the block FOR:
//!
//! 1. the settings DIRECTORY is always named, and its absence is named LOUDLY rather than by
//!    omission — `settings dir: NONE` is the whole diagnosis of the incident;
//! 2. every `policy.*` row appears whether or not anything set it — they are the risk ceilings;
//! 3. the CREDENTIAL STORE's presence is reported, and its permission finding with it;
//! 4. **no credential-shaped value is ever printed**, and no credential is ever READ to produce
//!    the block: [`the_banner_never_opens_the_database_either`] plants a store with a real-looking
//!    key and asserts neither the name nor the value appears anywhere in the output.
//!
//! There is one source (`docs/decisions/0086`): every non-default, non-env row is a `db` row,
//! unconditionally, and the block reports the ROWS that are actually in force, never a store-blind
//! resolution wearing their clothes.

use std::collections::HashMap;
use std::path::Path;

use vike_config::{StoreLayer, boot_lines};
use vike_secrets::{Adoption, ArmingRow, SettingRow, StoredSettings};

#[path = "common/store_layer.rs"]
mod store_layer;
use store_layer::no_store;

fn env(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
}

/// The whole block as one string — every assertion below is about what an operator can read in it.
fn block(settings_dir: Option<&Path>, vars: &HashMap<String, String>) -> String {
    block_with(settings_dir, no_store(file!()), vars)
}

/// [`block`] with an explicit store arm — the parameter the shipped disclosure carries.
fn block_with(
    settings_dir: Option<&Path>,
    source: StoreLayer<'_>,
    vars: &HashMap<String, String>,
) -> String {
    boot_lines(settings_dir, source, vars).join("\n")
}

/// A seal whose counts MATCH `rows`, as a real write would leave it.
fn seal(rows: &StoredSettings) -> Adoption {
    Adoption {
        adopted_at: "2026-09-22T00:00:00Z".to_string(),
        tool_version: "test".to_string(),
        files_present: String::new(),
        // A labelled arming row is a per-ACCOUNT ceiling; a venue row carries no label.
        venues_declared: rows.arming.iter().any(|r| r.label.is_none()),
        setting_rows: rows.settings.len(),
        arming_rows: rows.arming.len(),
    }
}

/// The one `setting: <key> = …` line, or a panic naming the key.
///
/// Assertions go through this rather than through a whole-block `contains` of a rendered value:
/// how `toml` spells a float is that crate's business (`2` vs `2.0`), and a test that pins it is
/// testing the wrong thing while looking like it tests provenance.
fn setting(text: &str, key: &str) -> String {
    let needle = format!("setting: {key} = ");
    text.lines()
        .find(|l| l.starts_with(&needle))
        .unwrap_or_else(|| panic!("no `{needle}` line in:\n{text}"))
        .to_string()
}

/// The the CI box shape: a process whose walk found no project at all. Every downstream symptom is
/// silence, so this ONE line has to be loud — and it has to say why the venues went paper.
#[test]
fn no_settings_directory_is_reported_loudly_not_by_omission() {
    let text = block(None, &HashMap::new());
    assert!(text.contains("settings dir: NONE"), "{text}");
    assert!(text.contains("compiled-in default"), "{text}");
    assert!(text.contains("paper"), "the consequence must be named: {text}");
    assert!(text.contains("VIKE_SETTINGS_DIR"), "and the fix: {text}");
    // Nothing to look for, and it must not claim otherwise.
    assert!(text.contains("credential store: NOT LOOKED FOR"), "{text}");
}

/// A row value and an environment value are both attributed to the layer that set them — the
/// provenance half, seen through this renderer rather than through `config show`.
#[test]
fn a_set_value_names_the_layer_that_set_it() {
    let dir = tempfile::tempdir().unwrap();
    let rows = StoredSettings {
        settings: vec![SettingRow {
            section: "policy".into(),
            key: "max_leverage".into(),
            value: "2.0".into(),
        }],
        ..Default::default()
    };

    let text = block_with(
        Some(dir.path()),
        StoreLayer::Rows { rows: &rows, adopted: None },
        &env(&[("VIKE_RECONCILE", "1")]),
    );

    let leverage = setting(&text, "policy.max_leverage");
    assert!(leverage.contains('2'), "the row's value: {leverage}");
    assert!(leverage.ends_with("[db]"), "attributed to the store: {leverage}");

    // The EFFECTIVE value (`true`), not the layer's literal `1` — the row reports what the process
    // is running on, and names the variable that put it there.
    let reconcile = setting(&text, "flags.reconcile");
    assert!(reconcile.contains("true"), "the effective value: {reconcile}");
    assert!(reconcile.ends_with("[env:VIKE_RECONCILE]"), "attributed to the variable: {reconcile}");
}

/// **Every `policy.*` row is printed even at its default**, while an untouched flag is not.
///
/// The ceilings are the reason a daemon logs anything at all here; the flags are 27 rows of noise
/// nobody reads. The tally line is what keeps the omission honest.
#[test]
fn the_risk_ceilings_are_always_printed_and_the_rest_are_counted() {
    let dir = tempfile::tempdir().unwrap();
    let text = block(Some(dir.path()), &HashMap::new());

    for ceiling in [
        "policy.max_leverage",
        "policy.max_notional_per_order",
        "policy.market_slippage",
        "policy.halt_admit",
    ] {
        assert!(setting(&text, ceiling).ends_with("[default]"), "{ceiling}: {text}");
    }
    assert!(
        !text.contains("setting: flags.reconcile"),
        "an untouched flag is counted, not printed: {text}"
    );
    assert!(text.contains("at their compiled-in default"), "{text}");
    assert!(text.contains("vike-cli config show"), "the full dump must be pointed at: {text}");
}

/// An unset `Option` reads `<unset>`, never an empty value or a missing row — the two look the same
/// in a log and mean different things.
#[test]
fn an_unset_ceiling_reads_unset() {
    let dir = tempfile::tempdir().unwrap();
    let text = block(Some(dir.path()), &HashMap::new());
    assert_eq!(
        setting(&text, "policy.max_notional_per_order"),
        "setting: policy.max_notional_per_order = <unset> [default]"
    );
}

/// The fact that was invisible: is there a credential store in the settings directory?
#[test]
fn the_credential_store_is_reported_present_or_absent() {
    let dir = tempfile::tempdir().unwrap();

    let absent = block(Some(dir.path()), &HashMap::new());
    assert!(absent.contains("credential store:"), "{absent}");
    assert!(absent.contains("ABSENT"), "{absent}");
    assert!(absent.contains("every venue stays paper"), "the consequence: {absent}");

    // The store is the settings DATABASE.
    plant_database(dir.path());
    let present = block(Some(dir.path()), &HashMap::new());
    assert!(present.contains("credential store:"), "{present}");
    assert!(present.contains("PRESENT"), "{present}");
}

/// A settings key whose LEAF is credential-shaped is redacted by shape, not by an allowlist.
///
/// No settings field is credential-shaped today, so this asserts the mechanism on the boundary the
/// model would cross: the shapes come from `vike_config::redact`, which `vike-cli config show`
/// shares, and the day a `bot_token` field lands both surfaces already hide it.
#[test]
fn the_redaction_shapes_cover_a_credential_shaped_settings_key() {
    assert!(vike_config::is_secret_key("config.bot_token"));
    assert!(vike_config::is_secret_key("preferences.api_key"));
    assert!(!vike_config::is_secret_key("policy.max_notional_per_order"));
    // …and the model really is clean today, so the check above is insurance rather than dead code.
    let dir = tempfile::tempdir().unwrap();
    let description = vike_config::describe(Some(dir.path()), &HashMap::new()).unwrap();
    assert!(
        description.rows.iter().all(|r| !vike_config::is_secret_key(&r.key)),
        "a settings field is now credential-shaped — the redaction above stops being insurance"
    );
}

/// A load failure is a LINE, never a panic. `vike-recorder` is the reason: this is the first (and
/// only) loader in that process, and a disclosure that could stop a recorder from starting would be
/// a strictly worse trade than the silence it replaces.
///
/// A bad ROW is MARKED (`Settings::seal_refusal`), never a hard `Err` (see
/// `crate::mirror::apply_rows`'s own doc for why). The one load failure this loader can still raise
/// is the removed per-project override file (`<project>/vike.toml`, an independently-decided
/// refusal — see `crate::removed`), which is what this fixture drives.
#[test]
fn a_present_removed_project_file_is_reported_and_does_not_panic() {
    let project = tempfile::tempdir().unwrap();
    let settings_dir = project.path().join("settings");
    std::fs::create_dir(&settings_dir).unwrap();
    std::fs::write(
        project.path().join(vike_config::REMOVED_PROJECT_FILE),
        "[config]\nlog_dir = \"/from/project\"\n",
    )
    .unwrap();

    let text = block(Some(&settings_dir), &HashMap::new());
    assert!(text.contains("settings: COULD NOT BE LOADED"), "{text}");
    assert!(text.contains("compiled-in defaults"), "{text}");
    // The store half still runs: a load failure must not take the credential disclosure with it.
    assert!(text.contains("credential store:"), "{text}");
}

/// The tally line's two numbers are the real sums over `describe`'s rows, not just a line whose
/// shape looks right. An operator reading "N set by the settings database or the environment, M at
/// their compiled-in default" is trusting those are actual counts.
#[test]
fn the_tally_counts_are_exact() {
    let dir = tempfile::tempdir().unwrap();
    let rows = StoredSettings {
        settings: vec![SettingRow {
            section: "policy".into(),
            key: "max_leverage".into(),
            value: "2.0".into(),
        }],
        ..Default::default()
    };
    let arm = StoreLayer::Rows { rows: &rows, adopted: None };
    let vars = env(&[("VIKE_RECONCILE", "1")]);

    let description = vike_config::describe_with_source(Some(dir.path()), arm, &vars).unwrap();
    let configured =
        description.rows.iter().filter(|r| r.origin != vike_config::Origin::Default).count();
    let defaulted = description.rows.len() - configured;
    assert!(configured >= 2, "the row value and the env override should both count: {configured}");
    assert!(defaulted > 0, "most rows are still default in this fixture: {defaulted}");

    let text = block_with(Some(dir.path()), arm, &vars);
    let expected = format!(
        "settings: {configured} set by the settings database (1 rows) or the environment, \
         {defaulted} at their compiled-in default"
    );
    assert!(text.contains(&expected), "{text}\nwanted substring: {expected}");
}

/// The store's stat-only permission finding actually reaches the block, not just the mode itself.
#[cfg(unix)]
#[test]
fn an_exposed_store_permission_is_reported_as_a_finding() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    // Asked of the DATABASE, the one store.
    let store = plant_database(dir.path());
    std::fs::set_permissions(&store, std::fs::Permissions::from_mode(0o644)).unwrap();

    let text = block(Some(dir.path()), &HashMap::new());
    assert!(text.contains("credential store: ⚠"), "{text}");
    assert!(text.contains("chmod 600"), "the fix must be named: {text}");
    assert!(text.contains("0644"), "the offending mode must be named: {text}");

    // …and tightening it to owner-only makes the finding go away.
    std::fs::set_permissions(&store, std::fs::Permissions::from_mode(0o600)).unwrap();
    let clean = block(Some(dir.path()), &HashMap::new());
    assert!(!clean.contains("chmod 600"), "an owner-only store must not warn: {clean}");
}

/// Plant a settings database where the real one lives, so `vike_secrets::database_present`'s
/// `is_file` answers yes. Its CONTENT is irrelevant to every probe under test — all of them are
/// stat-only, which is the property `the_banner_never_opens_the_database_either` pins.
fn plant_database(dir: &std::path::Path) -> std::path::PathBuf {
    let db = vike_secrets::db_path_in(dir);
    std::fs::create_dir_all(db.parent().expect("the db lives in a directory")).unwrap();
    std::fs::write(&db, b"not a real sqlite file, and nothing here opens it").unwrap();
    db
}

/// ⚠ **THE BANNER MUST NAME THE STORE THAT ANSWERS, and that is the DATABASE.**
#[test]
fn the_store_line_names_the_database() {
    let dir = tempfile::tempdir().unwrap();
    plant_database(dir.path());

    let text = block(Some(dir.path()), &HashMap::new());
    let line = text
        .lines()
        .find(|l| l.starts_with("credential store:") && l.contains("PRESENT"))
        .expect("a store line");
    assert!(line.contains("vike.db"), "the line must name the DATABASE: {line}");
    assert!(line.contains("only credential store"), "…and say it is the only store: {line}");
}

/// ⚠ **The stat-only rule survives the change**, and this asserts it on the DATABASE path rather
/// than trusting that the new branch kept it. A value planted in the file the banner now names must
/// not reach a log line.
#[test]
fn the_banner_never_opens_the_database_either() {
    const VALUE: &str = "s3cr3t-value-that-must-never-be-logged";
    let dir = tempfile::tempdir().unwrap();
    let db = plant_database(dir.path());
    std::fs::write(&db, format!("ACME_LIVE_API_KEY={VALUE}\n")).unwrap();

    let text = block(Some(dir.path()), &HashMap::new());
    assert!(!text.contains(VALUE), "a credential VALUE reached the boot block: {text}");
    assert!(!text.contains("ACME_LIVE_API_KEY"), "a credential NAME reached it: {text}");
}

// ------------------------------------------------------------------------------------------------
// THE BLOCK DESCRIBES THE RESOLUTION THIS PROCESS IS RUNNING ON
// ------------------------------------------------------------------------------------------------

/// **The rows really are what the block reports — a live venue posture and a live ceiling, both
/// attributed to the store.**
///
/// ⚠ A store-blind `describe` would credit nothing for a value the rows supplied; this asserts
/// directly that the block reports the rows, unconditionally.
#[test]
fn the_block_reports_the_rows_in_force_not_compiled_in_defaults() {
    let dir = tempfile::tempdir().unwrap();
    let rows = StoredSettings {
        settings: vec![SettingRow {
            section: "policy".into(),
            key: "max_notional_per_order".into(),
            value: "100.0".into(),
        }],
        arming: ["aster", "hyperliquid", "polymarket"]
            .iter()
            .map(|v| ArmingRow {
                venue: (*v).to_string(),
                label: None,
                mode: "live".to_string(),
                max_exposure: None,
            })
            .collect(),
        ..Default::default()
    };
    let sealed = seal(&rows);
    let arm = StoreLayer::Rows { rows: &rows, adopted: Some(&sealed) };

    let text = block_with(Some(dir.path()), arm, &HashMap::new());

    // The ceiling the daemon is really enforcing, attributed to the layer that really set it.
    let ceiling = setting(&text, "policy.max_notional_per_order");
    assert!(ceiling.contains("100"), "the ROW's value, not the compiled-in default: {ceiling}");
    assert!(ceiling.ends_with("[db]"), "attributed to the rows: {ceiling}");
    assert!(
        !ceiling.contains("<unset>"),
        "this is the exact line a live order-signing daemon would have announced as UNCAPPED while \
         enforcing 100.0: {ceiling}"
    );

    // …and the arming posture, which is the half that would have read as fully disarmed.
    let venues = setting(&text, "policy.venues");
    for venue in ["aster", "hyperliquid", "polymarket"] {
        assert!(venues.contains(venue), "{venue} is armed LIVE and must be named: {venues}");
    }
    assert!(!venues.contains("live=none"), "a LIVE posture must not read as disarmed: {venues}");
    assert!(venues.contains("db sets"), "the ARTIFACT that set them: {venues}");
}
