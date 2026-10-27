//! `MarginCallConfig` — the margin-call watchdog's three knobs, written ONCE.
//!
//! It is the run profile's `[guards.margin_call]` table (`vike_config::run_profile`, which parses it
//! as this very type) AND the config the engine's margin-call model reads (`vike_exec::margin_call`,
//! `vike_core::CoreConfig::margin_call`). It used to be two structs, `vike_exec`'s `MarginCallConfig`
//! and a serde mirror in vike-config with `Option` fields, glued by a converter that filled the
//! defaults, because vike-config cannot name vike-exec (same layer). Down here both reach it.
//!
//! ⚠ `mm_requirement` is REQUIRED in the table (the operator names the leverage-shaped fraction);
//! `warn_fraction` and `buffer` are optional and a key left off takes [`MarginCallConfig::default`]'s
//! value, so that `impl` is the ONE place the numbers are written. The serde defaults below read
//! it rather than retype them.

use serde::Deserialize;

/// `[guards.margin_call]`: the engine's margin-call watchdog config.
#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MarginCallConfig {
    /// maintenance-margin fraction (LEAN `1/leverage` shape), e.g. 0.05
    pub mm_requirement: f64,
    /// warning when margin remaining ≤ equity · this (LEAN hardcodes 0.05)
    #[serde(default = "default_warn_fraction")]
    pub warn_fraction: f64,
    /// liquidate only when margin used > equity · (1 + buffer) (LEAN default 0.10)
    #[serde(default = "default_buffer")]
    pub buffer: f64,
}

impl Default for MarginCallConfig {
    fn default() -> Self {
        MarginCallConfig { mm_requirement: 0.05, warn_fraction: 0.05, buffer: 0.10 }
    }
}

fn default_warn_fraction() -> f64 {
    MarginCallConfig::default().warn_fraction
}

fn default_buffer() -> f64 {
    MarginCallConfig::default().buffer
}
