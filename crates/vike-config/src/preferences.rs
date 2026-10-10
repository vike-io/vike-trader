//! [`Preferences`] — TASTE and TUNING. Full override chain, but nothing here is a ceiling.
//!
//! The line against [`crate::Config`] is "would a second machine need a different value?" — a
//! log level or a chart style would not, they express what the operator likes. The line against
//! [`crate::Policy`] is sharper: **policy sets the BOUND, a preference sets the VALUE inside it.**
//!
//! ## ⚠ `rate_utilization` is a TOMBSTONE
//!
//! **Nothing read it.** Every pacer takes its target fraction from
//! `vike_binance::family::klines::KlineSpec::utilization`, and every construction site of that
//! field — binance's two consts, aster's `aster_kline_spec` — fills it with the compiled-in
//! [`vike_model::rate_limits::DEFAULT_UTILIZATION`]. Both halves
//! ([`PreferencesPatch::rate_utilization`] and [`crate::PolicyPatch::rate`]) REFUSE the key by
//! name. The concept did not move out of the product, only out of here: [`vike_model::rate_limits`]
//! owns the number, its bounds, and — the part a single float in this struct could never express —
//! per-VENUE overrides ([`vike_model::RateLimitConfig`]). A pacing knob that cannot say "40 % on
//! binance, 20 % on aster" is the wrong shape, and this one additionally lived in a crate no pacer
//! can reach: the consumer is `vike-backfill`, which loads no settings and binds every venue's
//! `fetch_klines_range` through one uniform 4-arg signature that carries no utilization. It comes
//! back the day that path can carry it — with a consumer, this time.
//!
//! (The field also deliberately never got a `VIKE_RATE_UTILIZATION` env name, for a reason that
//! still holds for any future one: a program whose end state is "no crate calls `env::var` except
//! `vike-config`" should not open by MINTING a new environment variable.)

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

use crate::error::{ConfigError, STALE_ROW_REMOVAL};
use crate::layers::{CliOverride, CliOverrides, EnvOverride, get, parse_usize};

/// `RUST_LOG` — the console log filter, checked FIRST (matching `vike_log::init`).
pub const RUST_LOG_ENV: &str = "RUST_LOG";
/// `VIKE_LOG` — the vike-flavoured alias for the console log filter.
pub const LOG_LEVEL_ENV: &str = "VIKE_LOG";
/// `VIKE_LOG_FILE_LEVEL` — the JSON file layer's own level, which `RUST_LOG` does NOT touch.
pub const LOG_FILE_LEVEL_ENV: &str = "VIKE_LOG_FILE_LEVEL";
/// `VIKE_STYLE` — the default chart style.
pub const CHART_STYLE_ENV: &str = "VIKE_STYLE";
/// `VIKE_SWEEP_THREADS` — bound on the parameter-sweep worker pool.
pub const SWEEP_THREADS_ENV: &str = "VIKE_SWEEP_THREADS";

/// Default console log filter — `vike_log::LogConfig`'s own default.
pub const DEFAULT_LOG_LEVEL: &str = "info";
/// Default JSON-file log level. ⚠ `trace` is why an unattended backfill once wrote 341 GB; it is
/// kept here so this crate changes nothing, and the field exists so a deployment can turn it down
/// in a row instead of an env var.
pub const DEFAULT_LOG_FILE_LEVEL: &str = "trace";

/// `preferences.theme`'s words — the four themes (design system spec §3.1, §5), spelled as
/// `vike_ui_theme::theme::ThemeId::key` spells them. This crate (layer 20) cannot name that one
/// (layer 70), so the words are data here, and
/// `crates/vike-app-core/src/ui/appearance_settings.rs`'s tests hold the two spellings equal.
///
/// ⚠ **A word added to one of these lists later is a STORE-FORMAT change for every older
/// binary.** The loader refuses a word it does not know (`one_of`), exactly as it refuses a key it
/// does not know, so once a newer build writes its new word, an older build reading the same store
/// refuses the store rather than falling back to a default. That is deliberate — a word the GUI
/// cannot show would otherwise display as effective in `config show` — and it holds for the
/// BACKEND's store too: an appearance row written there (these keys are editable in Connections →
/// Backend settings) stops an older daemon's boot after a rollback, although no daemon reads it.
/// Rebuild every binary that reads a store before writing a new word into it.
pub const THEMES: [&str; 4] = ["graphite", "midnight", "dusk", "carbon"];
/// `preferences.market_colors`'s words (spec §3.2). Held equal to `MarketId::key` the same way;
/// the store-format warning on [`THEMES`] applies.
pub const MARKET_COLOR_SETS: [&str; 4] = ["classic", "tradingview", "exchange", "colorblind"];
/// `preferences.density`'s words (spec §3.4); the warning on [`THEMES`] applies.
pub const DENSITIES: [&str; 3] = ["compact", "normal", "comfortable"];
/// `preferences.text_size`'s words (spec §3.3); the warning on [`THEMES`] applies. Three since
/// 2026-10-05: `small` is the scale `standard` used to be, `standard` (still the default) is the one
/// `large` used to be, and `large` is new. A store row written before then keeps its word and
/// means the next scale up for `standard` and `large` alike.
pub const TEXT_SIZES: [&str; 3] = ["small", "standard", "large"];
/// The defaults (spec §5's table).
pub const DEFAULT_THEME: &str = "graphite";
pub const DEFAULT_MARKET_COLORS: &str = "classic";
pub const DEFAULT_DENSITY: &str = "normal";
pub const DEFAULT_TEXT_SIZE: &str = "standard";

/// Operator taste and tuning.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Preferences {
    /// Console log filter directive (`info`, `vike_core=debug`, …).
    ///
    /// Was `RUST_LOG`, aliased `VIKE_LOG`. Defaults to [`DEFAULT_LOG_LEVEL`].
    pub log_level: String,

    /// JSON log-FILE level. Independent of `log_level`, which filters the console only.
    ///
    /// Was `VIKE_LOG_FILE_LEVEL`. Defaults to [`DEFAULT_LOG_FILE_LEVEL`].
    pub log_file_level: String,

    /// Default chart style for new chart windows. `None` = the GUI's own default.
    ///
    /// Was `VIKE_STYLE`.
    pub chart_style: Option<String>,

    /// Bound on the bounded rayon pool a parameter sweep installs. `None` =
    /// `min(4, available_parallelism())`, `vike-backtest`'s own fallback.
    ///
    /// Was `VIKE_SWEEP_THREADS`.
    pub sweep_threads: Option<usize>,

    /// **The GUI's theme** (design system spec §5): one of [`THEMES`]. Read by the desktop at
    /// start and changed live by its Settings window. ⚠ No environment variable, by ruling: an
    /// appearance is a person's preference, not a deployment knob.
    pub theme: String,
    /// The GUI's market-colour set: one of [`MARKET_COLOR_SETS`].
    pub market_colors: String,
    /// The chart's background gradient in window headers. Off by default.
    pub header_gradient: bool,
    /// The GUI's density: one of [`DENSITIES`].
    pub density: String,
    /// The GUI's text size: one of [`TEXT_SIZES`].
    pub text_size: String,

    /// The ADVISORY order-quantity cap `vike-cli`'s `trade` and `mcp` previews flag an order
    /// against — finite and above 0. Advisory, which is why it is a preference and not `policy`:
    /// the preview marks an order over it and refuses nothing, and the node enforces its own
    /// limits regardless. `None` — the default — is no quantity cap.
    ///
    /// Read on the CLIENT's box, from that box's own database. Beaten by `VIKE_MAX_ORDER_QTY` until
    /// `docs/decisions/0111-no-setting-lives-in-the-environment-or-a-toml-file.md`'s phase P5
    /// deletes that read.
    pub max_order_qty: Option<f64>,
}

impl Default for Preferences {
    fn default() -> Self {
        Preferences {
            log_level: DEFAULT_LOG_LEVEL.to_string(),
            log_file_level: DEFAULT_LOG_FILE_LEVEL.to_string(),
            chart_style: None,
            sweep_threads: None,
            theme: DEFAULT_THEME.to_string(),
            market_colors: DEFAULT_MARKET_COLORS.to_string(),
            header_gradient: false,
            density: DEFAULT_DENSITY.to_string(),
            text_size: DEFAULT_TEXT_SIZE.to_string(),
            max_order_qty: None,
        }
    }
}

/// The FILE shape of [`Preferences`] — all-optional, unknown keys rejected by name.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PreferencesPatch {
    /// **TOMBSTONE — removed, and REFUSED rather than ignored.** Not a [`Preferences`] field.
    ///
    /// Parsed only so [`Preferences::apply`] can refuse the key by name and say where the number
    /// lives now. See this module's doc for why a preference that validated, displayed and clamped
    /// while changing nothing is worse than one that never existed.
    pub rate_utilization: Option<f64>,
    /// See [`Preferences::log_level`].
    pub log_level: Option<String>,
    /// See [`Preferences::log_file_level`].
    pub log_file_level: Option<String>,
    /// See [`Preferences::chart_style`].
    pub chart_style: Option<String>,
    /// See [`Preferences::sweep_threads`].
    pub sweep_threads: Option<usize>,
    /// See [`Preferences::theme`].
    pub theme: Option<String>,
    /// See [`Preferences::market_colors`].
    pub market_colors: Option<String>,
    /// See [`Preferences::header_gradient`].
    pub header_gradient: Option<bool>,
    /// See [`Preferences::density`].
    pub density: Option<String>,
    /// See [`Preferences::text_size`].
    pub text_size: Option<String>,
    /// See [`Preferences::max_order_qty`].
    pub max_order_qty: Option<f64>,
}

impl Preferences {
    /// Fold one file's patch in, validating against `file` so an error names it.
    pub(crate) fn apply(
        &mut self,
        patch: PreferencesPatch,
        file: &Path,
    ) -> Result<(), ConfigError> {
        // TOMBSTONE — see `PreferencesPatch::rate_utilization`. Refused, never applied.
        if let Some(v) = patch.rate_utilization {
            return Err(ConfigError::value(
                file,
                "rate_utilization",
                v,
                &format!(
                    "is no longer a preference — NOTHING read it. Every pacer takes its target \
                     fraction from the compiled-in vike_model::rate_limits::DEFAULT_UTILIZATION, \
                     so this key validated, displayed as effective, and changed nothing (a \
                     `policy.rate.max_utilization` row has the same problem). \
                     {STALE_ROW_REMOVAL}. The knob returns, per-venue, when \
                     vike_model::RateLimitConfig can reach a pacer"
                ),
            ));
        }
        if let Some(v) = patch.log_level {
            check_non_blank(file, "log_level", &v)?;
            self.log_level = v;
        }
        if let Some(v) = patch.log_file_level {
            check_non_blank(file, "log_file_level", &v)?;
            self.log_file_level = v;
        }
        if let Some(v) = patch.chart_style {
            check_non_blank(file, "chart_style", &v)?;
            self.chart_style = Some(v);
        }
        if let Some(v) = patch.sweep_threads {
            if v == 0 {
                return Err(ConfigError::Value {
                    file: file.to_path_buf(),
                    key: "sweep_threads".to_string(),
                    message: "0 would install a pool that never runs a point — omit the key for \
                              the default min(4, available_parallelism())"
                        .to_string(),
                });
            }
            self.sweep_threads = Some(v);
        }
        if let Some(v) = patch.theme {
            self.theme = one_of(file, "theme", v, &THEMES)?;
        }
        if let Some(v) = patch.market_colors {
            self.market_colors = one_of(file, "market_colors", v, &MARKET_COLOR_SETS)?;
        }
        if let Some(v) = patch.header_gradient {
            self.header_gradient = v;
        }
        if let Some(v) = patch.density {
            self.density = one_of(file, "density", v, &DENSITIES)?;
        }
        if let Some(v) = patch.text_size {
            self.text_size = one_of(file, "text_size", v, &TEXT_SIZES)?;
        }
        if let Some(v) = patch.max_order_qty {
            if !(v.is_finite() && v > 0.0) {
                return Err(ConfigError::value(
                    file,
                    "max_order_qty",
                    v,
                    "is not a quantity cap: it must be a finite number above 0 — omit the key for \
                     no cap",
                ));
            }
            self.max_order_qty = Some(v);
        }
        Ok(())
    }
}

fn check_non_blank(file: &Path, key: &str, v: &str) -> Result<(), ConfigError> {
    if v.trim().is_empty() {
        return Err(ConfigError::Value {
            file: file.to_path_buf(),
            key: key.to_string(),
            message: "\"\" is blank — omit the key to keep the default".to_string(),
        });
    }
    Ok(())
}

/// `v` when it is one of `words`, else a refusal naming the key and the whole list. A word the GUI
/// has no option for would otherwise display as effective in `config show` while the GUI fell
/// back to its default — positive confirmation of something false (`crate::consumed`'s module
/// doc). The value itself is not echoed: a credential pasted into the wrong key must not be
/// printed by the refusal.
fn one_of(file: &Path, key: &str, v: String, words: &[&str]) -> Result<String, ConfigError> {
    if words.contains(&v.as_str()) {
        return Ok(v);
    }
    Err(ConfigError::Value {
        file: file.to_path_buf(),
        key: key.to_string(),
        message: format!("… must be one of: {}", words.join(", ")),
    })
}

impl EnvOverride for Preferences {
    /// `RUST_LOG` > `VIKE_LOG` > whatever the file layer left, mirroring `vike_log::init`'s own
    /// aliasing exactly.
    fn apply_env(&mut self, env: &HashMap<String, String>) -> Result<(), ConfigError> {
        if let Some(v) = get(env, RUST_LOG_ENV).or_else(|| get(env, LOG_LEVEL_ENV)) {
            self.log_level = v.to_string();
        }
        if let Some(v) = get(env, LOG_FILE_LEVEL_ENV) {
            self.log_file_level = v.to_string();
        }
        if let Some(v) = get(env, CHART_STYLE_ENV) {
            self.chart_style = Some(v.to_string());
        }
        if let Some(v) = get(env, SWEEP_THREADS_ENV) {
            let n = parse_usize(SWEEP_THREADS_ENV, v)?;
            if n == 0 {
                return Err(ConfigError::Env {
                    var: SWEEP_THREADS_ENV.to_string(),
                    value: v.to_string(),
                    message: "expected at least 1 worker".to_string(),
                });
            }
            self.sweep_threads = Some(n);
        }
        Ok(())
    }
}

impl CliOverride for Preferences {
    fn apply_cli(&mut self, cli: &CliOverrides) -> Result<(), ConfigError> {
        if let Some(v) = &cli.log_level {
            self.log_level = v.clone();
        }
        Ok(())
    }
}

#[path = "preferences_tests.rs"]
#[cfg(test)]
mod preferences_tests;
