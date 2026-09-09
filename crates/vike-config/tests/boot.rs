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

use std::collections::HashMap;
use std::path::Path;

use vike_config::boot_lines;
use vike_config::load::{CONFIG_FILE, FLAGS_FILE, POLICY_FILE, PREFERENCES_FILE};

fn env(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
}

fn write(dir: &Path, name: &str, body: &str) {
    std::fs::write(dir.join(name), body).expect("write settings file");
}

/// The whole block as one string — every assertion below is about what an operator can read in it.
fn block(settings_dir: Option<&Path>, vars: &HashMap<String, String>) -> String {
    boot_lines(settings_dir, vars).join("\n")
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
