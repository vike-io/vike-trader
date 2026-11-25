//! The client-side reach into a backtest profile `vike-cli`'s `--preset` merge makes. An inline script
//! source no longer rides a profile: both programs refuse one (`crate::refusals`'s `BacktestCallRefusal`).

/// The profile's `[strategy.params]` table, CREATING `[strategy]` and `[strategy.params]` when
/// absent — the one place a client-side rewrite (`crates/vike-cli/src/cmd/backtest/profile.rs`'s
/// `merge_preset_params`) reaches into a profile, so it cannot disagree about where params live or
/// about what a non-table there means.
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
