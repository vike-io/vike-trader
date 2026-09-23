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
//! 2. an absent settings file says so, with its full path, so "my `policy.toml` is not being read"
//!    is answerable without a second command;
//! 3. every `policy.*` row appears whether or not anything set it — they are the risk ceilings;
//! 4. the CREDENTIAL STORE's presence is reported, and its permission finding with it;
//! 5. **no credential-shaped value is ever printed**, and no credential is ever READ to produce
//!    the block: [`the_boot_block_never_opens_the_credential_store`] plants a real store with a
//!    real-looking key and asserts neither the name nor the value appears anywhere in the output.
//! 6. **the block describes the resolution the process is RUNNING ON** — the section at the bottom
//!    of this file, added 2026-09-22 after the live the CI box daemon printed a WARN saying the settings
//!    DATABASE answered and then credited `policy.toml` for the ceilings in the same boot.

use std::collections::HashMap;
use std::path::Path;

use vike_config::load::{CONFIG_FILE, FLAGS_FILE, POLICY_FILE, PREFERENCES_FILE};
use vike_config::mirror::rows_from_files;
use vike_config::{StoreLayer, boot_lines};
use vike_secrets::{Adoption, StoredSettings};

fn env(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
}

fn write(dir: &Path, name: &str, body: &str) {
    std::fs::write(dir.join(name), body).expect("write settings file");
}

/// The store arm for a fixture that plants no settings database — every test above this file's last
/// section.
///
/// [`StoreLayer::NotConsulted`] rather than [`StoreLayer::NoDatabase`] on purpose: it is the exact
/// arm `boot_lines` was hard-wired to before it took the parameter, so every assertion written
/// before 2026-09-22 still measures the rendering it was authored against, character for character.
/// [`a_box_whose_store_supplies_nothing_renders_the_same_block_under_every_arm`] is what proves the two — and
/// the other two unadopted arms — really are one rendering.
fn no_store() -> StoreLayer<'static> {
    StoreLayer::NotConsulted(
        "crates/vike-config/tests/boot.rs — this fixture plants no settings database",
    )
}

/// The whole block as one string — every assertion below is about what an operator can read in it.
fn block(settings_dir: Option<&Path>, vars: &HashMap<String, String>) -> String {
    block_with(settings_dir, no_store(), vars)
}

/// [`block`] with an explicit store arm — the parameter the shipped disclosure now carries.
fn block_with(
    settings_dir: Option<&Path>,
    source: StoreLayer<'_>,
    vars: &HashMap<String, String>,
) -> String {
    boot_lines(settings_dir, source, vars).join("\n")
}

/// A seal whose counts MATCH `rows`, as `vike-cli config adopt` would have written it.
///
/// The counts are taken from the rows rather than typed, because that is what the writer does —
/// `vike_secrets::write_adoption` counts inside its own transaction — so a fixture that typed them
/// would be measuring a state the writer cannot produce. Same shape as
/// `crates/vike-config/tests/mirror.rs`'s `seal`.
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

/// Every `setting:` line's ORIGIN — the text inside the last bracket pair on the line.
///
/// The bracket rather than the whole line, because a VALUE may legitimately contain a file name
/// (`config.log_dir`) and a whole-line `contains(".toml")` would then read as a provenance claim.
/// `setting_line`'s optional ` (adjusted)` suffix is why this scans forward to the closing bracket
/// instead of taking the line's last character.
fn origins(text: &str) -> Vec<&str> {
    text.lines()
        .filter(|l| l.starts_with("setting: "))
        .filter_map(|l| {
            let open = l.rfind('[')?;
            let rest = &l[open + 1..];
            Some(&rest[..rest.find(']')?])
        })
        .collect()
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

/// An absent settings file is a ROW with a full path, not a gap in the block. "Did my policy.toml
/// get read?" is the question the whole feature exists to answer, and a missing line answers it
/// only for somebody who already knows how many lines to expect.
#[test]
fn every_settings_file_is_named_present_or_absent() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), POLICY_FILE, "max_leverage = 2.0\nmax_notional_per_order = 250.0\n");

    let text = block(Some(dir.path()), &HashMap::new());
    assert!(text.contains(&format!("settings dir: {}", dir.path().display())), "{text}");
    assert!(text.contains("settings file: policy.toml present, 2 keys"), "{text}");
    for absent in [CONFIG_FILE, PREFERENCES_FILE, FLAGS_FILE] {
        let line = format!("settings file: {absent} ABSENT (");
        assert!(text.contains(&line), "{absent} must be reported absent, with its path: {text}");
    }
}

/// A file value and an environment value are both attributed to the layer that set them — the
/// provenance half, seen through this renderer rather than through `config show`.
#[test]
fn a_set_value_names_the_layer_that_set_it() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), POLICY_FILE, "max_leverage = 2.0\n");

    let text = block(Some(dir.path()), &env(&[("VIKE_RECONCILE", "1")]));

    let leverage = setting(&text, "policy.max_leverage");
    assert!(leverage.contains('2'), "the file's value: {leverage}");
    assert!(leverage.ends_with("[policy.toml]"), "attributed to the file: {leverage}");

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

/// The fact that was invisible: is there a credential store beside the settings files?
#[test]
fn the_credential_store_is_reported_present_or_absent() {
    let dir = tempfile::tempdir().unwrap();

    let absent = block(Some(dir.path()), &HashMap::new());
    assert!(absent.contains("credential store:"), "{absent}");
    assert!(absent.contains("ABSENT"), "{absent}");
    assert!(absent.contains("every venue stays paper"), "the consequence: {absent}");

    std::fs::write(dir.path().join(vike_secrets::SECRETS_FILE), "ACME_LIVE_API_KEY=abc\n").unwrap();
    let present = block(Some(dir.path()), &HashMap::new());
    assert!(present.contains("credential store:"), "{present}");
    assert!(present.contains("PRESENT"), "{present}");
}

/// ⚠ **The block is produced WITHOUT opening the store**, so no key name and no key value can reach
/// a log line however the store is configured. Asserted against a planted store with a realistic
/// key rather than against the renderer's intentions.
#[test]
fn the_boot_block_never_opens_the_credential_store() {
    const KEY: &str = "ACME_LIVE_API_KEY";
    const VALUE: &str = "s3cr3t-value-that-must-never-be-logged";

    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join(vike_secrets::SECRETS_FILE), format!("{KEY}={VALUE}\n"))
        .unwrap();

    let text = block(Some(dir.path()), &HashMap::new());
    assert!(!text.contains(VALUE), "a credential VALUE reached the boot block: {text}");
    assert!(!text.contains(KEY), "a credential NAME reached the boot block: {text}");
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

/// A broken settings file is a LINE, never a refusal. `vike-recorder` is the reason: this is the
/// first (and only) loader in that process, and a disclosure that could stop a recorder from
/// starting would be a strictly worse trade than the silence it replaces.
#[test]
fn a_broken_settings_file_is_reported_and_does_not_panic() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), POLICY_FILE, "max_leverage = \"not a number\"\n");

    let text = block(Some(dir.path()), &HashMap::new());
    assert!(text.contains("settings: COULD NOT BE LOADED"), "{text}");
    assert!(text.contains("compiled-in defaults"), "{text}");
    // The store half still runs: a load failure must not take the credential disclosure with it.
    assert!(text.contains("credential store:"), "{text}");
}

/// The tally line's two numbers are the real sums over `describe`'s rows, not just a line whose
/// shape looks right. An operator reading "N set by a file or the environment, M at their
/// compiled-in default" is trusting those are actual counts.
#[test]
fn the_tally_counts_are_exact() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), POLICY_FILE, "max_leverage = 2.0\n");
    let vars = env(&[("VIKE_RECONCILE", "1")]);

    let description = vike_config::describe(Some(dir.path()), &vars).unwrap();
    let configured =
        description.rows.iter().filter(|r| r.origin != vike_config::Origin::Default).count();
    let defaulted = description.rows.len() - configured;
    assert!(configured >= 2, "the file value and the env override should both count: {configured}");
    assert!(defaulted > 0, "most rows are still default in this fixture: {defaulted}");

    let text = block(Some(dir.path()), &vars);
    let expected = format!(
        "settings: {configured} set by a file or the environment, {defaulted} at their compiled-in default"
    );
    assert!(text.contains(&expected), "{text}\nwanted substring: {expected}");
}

/// The store's stat-only permission finding actually reaches the block, not just the mode itself.
#[cfg(unix)]
#[test]
fn an_exposed_store_permission_is_reported_as_a_finding() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join(vike_secrets::SECRETS_FILE);
    std::fs::write(&store, "ACME_LIVE_API_KEY=abc\n").unwrap();
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

/// The pre-one-store `.env` leftover is reported too, and only while the real store is absent.
#[test]
fn a_legacy_dotenv_beside_an_absent_store_is_reported_as_a_finding() {
    let project = tempfile::tempdir().unwrap();
    let settings_dir = project.path().join("settings");
    std::fs::create_dir(&settings_dir).unwrap();
    std::fs::write(project.path().join(".env"), "POLY_PROXY_ENABLED=1\n").unwrap();

    let text = block(Some(&settings_dir), &HashMap::new());
    assert!(text.contains("credential store: ⚠"), "{text}");
    assert!(text.contains(".env"), "{text}");
    assert!(text.contains("is NOT"), "the message must say the real store is missing: {text}");
}

/// Plant a settings database where the real one lives, so `vike_secrets::database_present`'s
/// `is_file` answers yes. Its CONTENT is irrelevant to every probe under test — all of them are
/// stat-only, which is the property `the_boot_block_never_opens_the_credential_store` pins.
fn plant_database(dir: &std::path::Path) -> std::path::PathBuf {
    let db = vike_secrets::db_path_in(dir);
    std::fs::create_dir_all(db.parent().expect("the db lives in a directory")).unwrap();
    std::fs::write(&db, b"not a real sqlite file, and nothing here opens it").unwrap();
    db
}

/// ⚠ **THE BANNER MUST NAME THE STORE THAT ANSWERS, and on a migrated box that is the DATABASE.**
///
/// MEASURED on the live the CI box daemon (v0.1.26): it printed `credential store: …/secrets.env
/// PRESENT` while `vike-cli secrets path` on the same box reported the database as the store that
/// answers and `secrets.env` as "NO LONGER READ". An operator reading the daemon's own banner was
/// sent to edit a file nothing opens.
#[test]
fn a_migrated_box_names_the_database_not_the_text_file() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join(vike_secrets::SECRETS_FILE), "ACME_LIVE_API_KEY=abc\n").unwrap();
    plant_database(dir.path());

    let text = block(Some(dir.path()), &HashMap::new());
    let line = text
        .lines()
        .find(|l| l.starts_with("credential store:") && l.contains("PRESENT"))
        .expect("a store line");
    assert!(line.contains("vike.db"), "the line must name the DATABASE: {line}");
    assert!(line.contains("is not read"), "…and say the text file is not: {line}");
}

/// ⚠ **The complement, and it is what keeps this off every unmigrated box** — which is most of
/// them. Without it the test above is satisfied by a line that always names the database.
#[test]
fn an_unmigrated_box_still_names_the_text_file_exactly_as_before() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join(vike_secrets::SECRETS_FILE), "ACME_LIVE_API_KEY=abc\n").unwrap();

    let text = block(Some(dir.path()), &HashMap::new());
    assert!(text.contains(vike_secrets::SECRETS_FILE), "{text}");
    assert!(!text.contains("vike.db"), "no database, so nothing may name one: {text}");
}

/// The SHADOWED store is said AT THE BANNER. `vike_secrets` has returned this finding beside the
/// credentials for a while and the credential loader logs it — but only once a root gets as far as
/// loading credentials, which the banner precedes and which a paper root never does. MEASURED: the
/// live daemon said "NO LONGER READ" zero times across its whole run while the file sat there.
#[test]
fn a_shadowed_text_file_is_reported_at_the_banner() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join(vike_secrets::SECRETS_FILE), "ACME_LIVE_API_KEY=abc\n").unwrap();
    plant_database(dir.path());

    let text = block(Some(dir.path()), &HashMap::new());
    assert!(text.contains("NO LONGER READ"), "{text}");
    assert!(
        text.contains("secrets migrate"),
        "…and the verb that would fold it in, since the file is otherwise inert: {text}"
    );
}

/// ⚠ A migrated box with NO leftover text file says nothing about one — the shadowed finding is a
/// finding, not a header. Without this the line would appear on every migrated box forever.
#[test]
fn a_migrated_box_with_no_leftover_file_reports_no_shadow() {
    let dir = tempfile::tempdir().unwrap();
    plant_database(dir.path());

    let text = block(Some(dir.path()), &HashMap::new());
    assert!(!text.contains("NO LONGER READ"), "nothing is shadowed here: {text}");
    assert!(text.contains("vike.db"), "…and the database is still the store that answers: {text}");
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
// 6. THE BLOCK DESCRIBES THE RESOLUTION THIS PROCESS IS RUNNING ON
//
// `boot_lines` called the store-BLIND `vike_config::describe` until 2026-09-22, so on an ADOPTED
// box (decision 0057's crossing) the whole settings half was a second resolution of a different
// configuration of the same box. MEASURED on the live the CI box `vike-tradehub`, one start, two lines
// 24 µs apart: a WARN reading "this box resolves its settings from the settings DATABASE … so
// policy.toml, config.toml, flags.toml on disk are INERT" followed by
// `setting: policy.max_notional_per_order = 100.0 [policy.toml]`.
//
// The three files were then DELETED from that box on 2026-09-22, which is the state these fixtures
// reproduce and the one that turns the defect from untidy into dangerous: a store-blind read of a
// settings directory with no files in it resolves to COMPILED-IN DEFAULTS, so the banner would
// announce an unset notional ceiling and every venue at `paper` while the daemon enforced 100.0 and
// armed three venues LIVE from the rows.
// ------------------------------------------------------------------------------------------------

/// the CI box's own shape, reduced: a notional ceiling and the three venues that box arms LIVE.
const PROD2_POLICY: &str = "max_notional_per_order = 100.0\n\n[venues]\naster = \"live\"\n\
                            hyperliquid = \"live\"\npolymarket = \"live\"\n";

/// **THE KILL PROOF — an adopted box whose settings files are GONE must report the ROWS.**
///
/// This is the CI box after 2026-09-22 18:28. The rows are the only settings layer, `vike-cli config
/// check` reports every file as `absent (compiled-in defaults)` with exit 0 (which is correct), and
/// the next `unattended-upgrades` restart is what renders this block.
///
/// ⚠ The fixture is built so that the store-blind answer and the row answer CANNOT coincide: the
/// files that produced the rows are deleted before the block is rendered, so a description that did
/// not consult the store has nothing left to read and resolves to defaults. Reverting `boot_lines`
/// to `describe` turns every assertion below red, which is the only thing that makes them evidence.
#[test]
fn an_adopted_box_with_its_files_deleted_reports_the_rows_not_compiled_in_defaults() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), POLICY_FILE, PROD2_POLICY);
    let rows = rows_from_files(dir.path()).expect("the fixture policy.toml is valid");
    // …and now the file is gone, exactly as it is on the CI box. The rows keep deciding.
    std::fs::remove_file(dir.path().join(POLICY_FILE)).unwrap();
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
    assert!(venues.contains("db/vike.db sets"), "the ARTIFACT that set them: {venues}");
    assert!(
        !venues.contains("policy.toml sets"),
        "the file is DELETED and cannot have set anything: {venues}"
    );

    // The file half still reports what is on DISK — and the source line says what that means.
    assert!(text.contains("settings file: policy.toml ABSENT"), "{text}");
    assert!(text.contains("settings DATABASE answers for every key"), "{text}");
}

/// **The regression this defect deserves: the WARN line and the `setting:` lines may not contradict
/// each other.**
///
/// The 2026-09-21 the CI box shape — adopted, with the inert files still on disk, which is what makes
/// [`vike_config::drift`]'s own warning fire. That warning and the rows are printed by the same
/// boot, so this asserts the RELATIONSHIP rather than either line alone: if anything in the block
/// says the database answers, nothing in it may credit a settings file for a value.
///
/// ⚠ The subject is asserted to EXIST before it is used. Without that, a block that simply stopped
/// saying the database answers would satisfy a "no contradiction" check vacuously — the
/// assertion-that-cannot-fail-for-its-stated-reason trap.
#[test]
fn the_banner_and_the_setting_lines_cannot_contradict_each_other() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), POLICY_FILE, PROD2_POLICY);
    write(dir.path(), CONFIG_FILE, "journal_dir = \"/srv/vike-<unit>/journal\"\n");
    let rows = rows_from_files(dir.path()).expect("the fixture files are valid");
    let sealed = seal(&rows);
    let arm = StoreLayer::Rows { rows: &rows, adopted: Some(&sealed) };

    let text = block_with(Some(dir.path()), arm, &HashMap::new());

    let says_db: Vec<&str> = text.lines().filter(|l| l.contains("settings DATABASE")).collect();
    assert!(
        !says_db.is_empty(),
        "the block must SAY which source answered — otherwise the check below is vacuous:\n{text}"
    );

    for origin in origins(&text) {
        assert!(
            !origin.contains(".toml"),
            "the block says the database answers ({says_db:?}) and then credits a file: [{origin}]\
             \n{text}"
        );
    }
    // …and the file rows are still printed, so the operator can see the inert drafts are there.
    assert!(text.contains("settings file: policy.toml present"), "{text}");
    assert!(text.contains("INERT"), "the loader's own drift warning must reach the block: {text}");
}

/// **A box whose store supplies NOTHING prints exactly what it printed before this parameter
/// existed.**
///
/// That is every CI lane, every fresh clone and every box that has not run `vike-cli config
/// mirror` — the overwhelming majority, and the half a change like this quietly breaks. Every arm
/// that carries no rows is compared against the [`StoreLayer::NotConsulted`] rendering, which is
/// literally what `boot_lines` produced when it was hard-wired to `vike_config::describe`, so the
/// equality IS the byte-identity claim rather than a proxy for it.
///
/// ⚠ A MIRRORED store is deliberately NOT in this list, and that is a finding rather than an
/// omission: its rows really do supply values (one per ROSTER venue, including the ones no file
/// mentions), so the block changes — and changes into agreement with what `vike-cli config show`
/// has reported on such a box all along.
/// [`a_mirrored_unadopted_box_reports_the_rows_that_really_answered`] is where that case is pinned
/// and argued.
///
/// The negative assertions at the end keep this from passing for the wrong reason: a block that had
/// stopped naming any artifact at all would satisfy the equality perfectly.
#[test]
fn a_box_whose_store_supplies_nothing_renders_the_same_block_under_every_arm() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), POLICY_FILE, PROD2_POLICY);
    write(dir.path(), CONFIG_FILE, "journal_dir = \"/srv/vike-<unit>/journal\"\n");
    let vars = env(&[("VIKE_RECONCILE", "1")]);

    let reference = block(Some(dir.path()), &vars);
    for (name, arm) in
        [("NoDatabase", StoreLayer::NoDatabase), ("TablesAbsent", StoreLayer::TablesAbsent)]
    {
        assert_eq!(
            block_with(Some(dir.path()), arm, &vars),
            reference,
            "{name} must render the pre-2026-09-22 block character for character"
        );
    }

    // ⚠ [`StoreLayer::Unreadable`] is the ONE unadopted arm that legitimately adds a line, and it
    // is asserted rather than excused: the loader marks `Settings::store_refusal` and this block
    // re-emits `Settings::warnings`, which is the module's stated discipline (a duplicate line
    // costs nothing next to a resolution that reaches nobody). So the refusal line is STRIPPED and
    // the remainder must still be the reference — a `contains` would pass for a block that had
    // rearranged itself around the new line.
    let unreadable = block_with(Some(dir.path()), StoreLayer::Unreadable("disk I/O error"), &vars);
    let stripped: Vec<&str> =
        unreadable.lines().filter(|l| !l.contains("could not be read")).collect();
    assert_eq!(
        stripped.join("\n"),
        reference,
        "an unreadable store may ADD its refusal and change nothing else:\n{unreadable}"
    );
    assert!(unreadable.contains("could not be read"), "…and it must add it: {unreadable}");

    // …and that block really is the FILE rendering, so the equality above is not four copies of a
    // block that names no source at all.
    assert!(
        reference.contains("policy.toml sets"),
        "the venue aggregate names the file: {reference}"
    );
    assert!(
        origins(&reference).contains(&"policy.toml"),
        "a per-row cell names the file: {reference}"
    );
    assert!(!reference.contains("db/vike.db"), "no store answered here: {reference}");
    assert!(!reference.contains("settings DATABASE answers"), "{reference}");
    assert!(
        reference.contains("set by a file or the environment"),
        "the tally names no store on a box that has none: {reference}"
    );
}

/// **A MIRRORED, UNADOPTED box reports the rows that really answered — and that CHANGED on
/// 2026-09-22.**
///
/// This is decision 0057's Phase 1 state: `vike-cli config mirror` has been run, `config adopt` has
/// not, so the files WIN and the rows sit below them. The store-blind read could not see that
/// layer at all, so any key the rows supply and no file sets was reported as a compiled-in default
/// — the adopted box's defect in a milder form, on a box nobody has crossed anything on.
///
/// ⚠ **It is a SECOND kill proof, and on the commoner box.** The fixture is the shape an operator
/// really produces: a ceiling mirrored into the rows, then deleted from `policy.toml` and never
/// re-mirrored. The row keeps deciding — `vike_config::mirror`'s `apply_rows` applies it below the
/// silent file — and the banner said `<unset> [default]` about a notional ceiling that was in
/// force. Reverting `boot_lines` to `describe` turns the first assertion below red.
///
/// The last assertion is the one that says why the CHANGE is the fix rather than a regression: the
/// block's origins are compared against `vike_config::describe_with_source`'s own rows, which is
/// exactly what `vike-cli config show` renders. Before this change the two disclosure surfaces on
/// one box gave different answers, and the banner was the wrong one.
#[test]
fn a_mirrored_unadopted_box_reports_the_rows_that_really_answered() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), POLICY_FILE, PROD2_POLICY);
    let rows = rows_from_files(dir.path()).expect("the fixture policy.toml is valid");
    // The ceiling is mirrored, and then dropped from the file without a re-mirror. Only the row
    // sets it now — on an UNADOPTED box, where the file would have won had it still said anything.
    write(dir.path(), POLICY_FILE, "[venues]\naster = \"live\"\n");
    let arm = StoreLayer::Rows { rows: &rows, adopted: None };
    let vars = HashMap::new();

    let text = block_with(Some(dir.path()), arm, &vars);

    let ceiling = setting(&text, "policy.max_notional_per_order");
    assert!(ceiling.contains("100"), "the ROW is in force and must be reported: {ceiling}");
    assert!(ceiling.ends_with("[db]"), "attributed to the rows: {ceiling}");

    // The venue aggregate names BOTH artifacts, because both really set rows here: the file names
    // `aster`, and the mirror wrote a row for every other roster venue.
    let venues = origins(&text)
        .into_iter()
        .find(|o| o.contains("sets"))
        .expect("the venue aggregate's origin clause");
    assert!(venues.contains("policy.toml"), "the file still sets one: [{venues}]");
    assert!(venues.contains("db/vike.db"), "…and the rows set the rest: [{venues}]");

    // …and the tally says the database supplied some of what it counts, rather than crediting a
    // file and the environment for all of it.
    assert!(text.contains("the settings database ("), "the tally names the store: {text}");

    // THE AGREEMENT: every origin this block prints is the origin `config show` would print, from
    // the same description. Two disclosure surfaces on one box may not answer differently.
    let d = vike_config::describe_with_source(Some(dir.path()), arm, &vars).unwrap();
    let mut compared = 0usize;
    for row in &d.rows {
        let needle = format!("setting: {} = ", row.key);
        let Some(line) = text.lines().find(|l| l.starts_with(&needle)) else { continue };
        assert!(
            line.ends_with(&format!("[{}]", row.origin.label())),
            "the banner and `config show` disagree about {}: {line} vs [{}]",
            row.key,
            row.origin.label()
        );
        compared += 1;
    }
    assert!(compared > 5, "the comparison must actually have run: {compared} rows");
}
