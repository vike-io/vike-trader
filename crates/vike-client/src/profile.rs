//! The client-side rewrite of a backtest profile both programs make before shipping it: an authored
//! script's source goes into `[strategy.params].src`.

/// Inject an authored Rhai script's `src` into a profile's `[strategy.params].src`, returning the
/// re-serialized profile TOML to ship. Parses the profile as a `toml::Value` (a bad profile is a
/// clean error, not a panic), creates `[strategy]`/`[strategy.params]` if absent, and sets/overwrites
/// `src`. Existing params (the numeric knobs a `[sweep]` varies) are preserved. Any pre-existing
/// inline `src` is overwritten — the `--script` file wins.
pub fn inject_script_source(profile_toml: &str, script_source: &str) -> Result<String, String> {
    let mut doc: toml::Value =
        toml::from_str(profile_toml).map_err(|e| format!("profile is not valid TOML: {e}"))?;
    strategy_params_mut(&mut doc)?.insert(
        vike_model::RESERVED_SOURCE_KEY.to_string(),
        toml::Value::String(script_source.to_string()),
    );
    toml::to_string(&doc)
        .map_err(|e| format!("cannot re-serialize profile after --script inject: {e}"))
}

/// The profile's `[strategy.params]` table, CREATING `[strategy]` and `[strategy.params]` when
/// absent — the one place both client-side rewrites (this file's [`inject_script_source`] and
/// `crates/vike-cli/src/cmd/backtest/profile.rs`'s `merge_preset_params`) reach into a profile, so
/// they cannot disagree about where params live or about what a non-table there means.
pub fn strategy_params_mut(
    doc: &mut toml::Value,
) -> Result<&mut toml::map::Map<String, toml::Value>, String> {
    let root = doc.as_table_mut().ok_or("profile root is not a TOML table")?;
    let strategy = root
        .entry("strategy")
        .or_insert_with(|| toml::Value::Table(Default::default()))
        .as_table_mut()
        .ok_or("[strategy] is not a table")?;
    strategy
        .entry("params")
        .or_insert_with(|| toml::Value::Table(Default::default()))
        .as_table_mut()
        .ok_or_else(|| "[strategy.params] is not a table".to_string())
}
