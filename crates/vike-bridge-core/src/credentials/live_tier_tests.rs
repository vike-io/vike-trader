//! The live tier has ONE spelling, and the credential tier -> setting tier map.

use super::*;

/// **`MAINNET` is no credential tier** (owner ruling 2026-10-09: venues take `DEMO` or `LIVE`
/// credentials). A store holding only a `{VENUE}_MAINNET_*` set holds NO live credentials: the
/// live gate answers absent and the venue stays paper, exactly as for any other unknown name, and
/// the mount's half-written-set report does not count those names either.
///
/// ⚠ The names are COMPOSED through the loader's own `prefix_for` + `names_for_prefix`, never
/// spelled: this module folds into `credentials.rs` for the settings registry's literal sweep, and
/// a whole env-shaped literal here would read as a variable this crate READS.
#[test]
fn a_mainnet_set_is_absent_and_only_the_live_spelling_loads() {
    let (old_key, old_secret, _) = names_for_prefix(&prefix_for("binance", "MAINNET"));
    let mut vars = HashMap::new();
    vars.insert(old_key, "old-key".to_string());
    vars.insert(old_secret, "old-secret".to_string());

    assert!(
        load_credentials_from("binance", Environment::Live, &vars).is_none(),
        "a MAINNET-spelled set must NOT load under Live — there is no fallback"
    );
    let spellings = tier_keys_for_account("binance", Environment::Live, &AccountLabel::Default);
    assert_eq!(spellings.len(), 1, "the live tier has one spelling: {spellings:?}");
    assert_eq!(
        spellings[0].missing_in(&vars),
        None,
        "the MAINNET names are not even a half-written live set"
    );

    // The control: the LIVE spelling beside it is what loads.
    let (key, secret, _) = names_for_prefix(&prefix_for("binance", Environment::Live.as_str()));
    vars.insert(key, "new-key".to_string());
    vars.insert(secret, "new-secret".to_string());
    let c = load_credentials_from("binance", Environment::Live, &vars).expect("the LIVE set loads");
    assert_eq!(c.api_key, "new-key");
}

/// Each credential tier reads its venue settings at one `venue_setting` tier, and `Sim` is the
/// `paper` one — the credential key's `SIM` token and the stored tier are different words.
#[test]
fn each_credential_tier_reads_its_settings_at_one_setting_tier() {
    use vike_secrets::venue_setting::SettingTier;
    assert_eq!(Environment::Sim.setting_tier(), SettingTier::Paper);
    assert_eq!(Environment::Demo.setting_tier(), SettingTier::Demo);
    assert_eq!(Environment::Live.setting_tier(), SettingTier::Live);
}
