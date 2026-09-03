//! [`Preferences`] — TASTE and TUNING. Full override chain, but nothing here is a ceiling.
//!
//! The line against [`crate::Config`] is "would a second machine need a different value?" — a
//! log level or a chart style would not, they express what the operator likes. The line against
//! [`crate::Policy`] is sharper: **policy sets the BOUND, a preference sets the VALUE inside it.**
//!
//! ## ⚠ `rate_utilization` was the worked example, and it has been REMOVED
//!
//! It is worth keeping the story, because the example was wrong in the one way that matters. The
//! field claimed to replace the compiled-in [`vike_model::rate_limits::DEFAULT_UTILIZATION`], and
//! `policy.rate.max_utilization` clamped it at the end of the load with a warning naming both
//! halves. **Nothing read the result.** Every pacer takes its target fraction from
//! `vike_binance::family::klines::KlineSpec::utilization`, and every construction site of that
//! field — binance's two consts, aster's `aster_kline_spec` — fills it with the constant. So an
//! operator could set the preference, watch it validate, watch `vike-cli config show` attribute it
//! to their file, watch the policy ceiling clamp it, read a warning about the clamp, and change
//! nothing at all. There was also a `--rate-utilization` CLI override, which no binary parsed.
//!
//! Both halves are now tombstones ([`PreferencesPatch::rate_utilization`] and
//! [`crate::PolicyPatch::rate`]) that REFUSE the key by name. The concept did not move out of the
//! product, only out of here: [`vike_model::rate_limits`] owns the number, its bounds, and — the
//! part a single float in this struct could never express — per-VENUE overrides
//! ([`vike_model::RateLimitConfig`]). A pacing knob that cannot say "40 % on binance, 20 % on
//! aster" is the wrong shape, and this one additionally lived in a crate no pacer can reach: the
//! consumer is `vike-backfill`, which loads no settings and binds every venue's
//! `fetch_klines_range` through one uniform 4-arg signature that carries no utilization. It comes
//! back the day that path can carry it — with a consumer, this time.
//!
//! (The field also deliberately never got a `VIKE_RATE_UTILIZATION` env name, for a reason that
//! still holds for any future one: a program whose end state is "no crate calls `env::var` except
//! `vike-config`" should not open by MINTING a new environment variable.)

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

use crate::error::ConfigError;
use crate::layers::{get, parse_usize, CliOverride, CliOverrides, EnvOverride};

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
/// Default JSON-file log level. ⚠ `trace` matches today's behaviour and is why an unattended
/// backfill once wrote 341 GB; it is kept here so this crate changes nothing, and the field
/// exists so a deployment can finally turn it down in a file instead of an env var.
pub const DEFAULT_LOG_FILE_LEVEL: &str = "trace";

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
}

impl Default for Preferences {
    fn default() -> Self {
        Preferences {
            log_level: DEFAULT_LOG_LEVEL.to_string(),
            log_file_level: DEFAULT_LOG_FILE_LEVEL.to_string(),
            chart_style: None,
            sweep_threads: None,
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
                "is no longer a preference — NOTHING read it. Every pacer takes its target \
                 fraction from the compiled-in vike_model::rate_limits::DEFAULT_UTILIZATION, so \
                 this key validated, displayed as effective, and changed nothing. Delete it (and \
                 `[rate] max_utilization` from policy.toml); the knob returns, per-venue, when \
                 vike_model::RateLimitConfig can reach a pacer",
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

#[cfg(test)]
mod tests {
    use super::*;

    fn file() -> &'static Path {
        Path::new("preferences.toml")
    }

    #[test]
    fn defaults_match_the_constants_they_replace() {
        let p = Preferences::default();
        assert_eq!(p.log_level, "info");
        assert_eq!(p.log_file_level, "trace");
    }

    #[test]
    fn rust_log_wins_over_vike_log_matching_vike_log_init() {
        let mut p = Preferences::default();
        p.apply_env(&HashMap::from([
            (RUST_LOG_ENV.to_string(), "debug".to_string()),
            (LOG_LEVEL_ENV.to_string(), "warn".to_string()),
        ]))
        .unwrap();
        assert_eq!(p.log_level, "debug");
    }

    #[test]
    fn vike_log_applies_when_rust_log_is_unset() {
        let mut p = Preferences::default();
        p.apply_env(&HashMap::from([(LOG_LEVEL_ENV.to_string(), "warn".to_string())])).unwrap();
        assert_eq!(p.log_level, "warn");
    }

    /// The tombstone REFUSES rather than ignoring, and every value is refused — including `0.40`,
    /// which was this field's own DEFAULT and the value most likely to be sitting in a real
    /// `preferences.toml`. An operator who wrote it believes they are pacing at 40 %; they were
    /// already pacing at 40 % for an unrelated reason (the compiled-in constant), and would have
    /// gone on believing the file did it.
    #[test]
    fn the_removed_rate_utilization_key_is_refused_and_names_where_the_number_lives_now() {
        for v in [0.40, 0.9, 1.5, 0.001] {
            let err = Preferences::default()
                .apply(PreferencesPatch { rate_utilization: Some(v), ..Default::default() }, file())
                .expect_err("a preference nothing reads must not load silently");
            let msg = err.to_string();
            assert!(msg.starts_with("preferences.toml: rate_utilization = "), "{msg}");
            assert!(msg.contains("NOTHING read it"), "says why it is gone: {msg}");
            assert!(
                msg.contains("DEFAULT_UTILIZATION"),
                "names where the number really comes from: {msg}"
            );
            assert!(msg.contains("policy.toml"), "names the ceiling half to delete too: {msg}");
        }
    }

    #[test]
    fn a_zero_sweep_pool_is_rejected() {
        let err = Preferences::default()
            .apply(PreferencesPatch { sweep_threads: Some(0), ..Default::default() }, file())
            .unwrap_err();
        assert!(err.to_string().contains("sweep_threads"), "{err}");
    }

    #[test]
    fn a_non_numeric_sweep_thread_count_names_the_variable() {
        let env = HashMap::from([(SWEEP_THREADS_ENV.to_string(), "lots".to_string())]);
        let err = Preferences::default().apply_env(&env).unwrap_err();
        assert!(err.to_string().starts_with("VIKE_SWEEP_THREADS=lots: "), "{err}");
    }

    /// `check_non_blank` guards three keys and was pinned by nothing — a mutation sweep replaced
    /// its whole body with `Ok(())` and every test here still passed, while its neighbour
    /// (`sweep_threads == 0`) was pinned twice over.
    ///
    /// The consequence is not cosmetic for `log_level`: a blank one reaches `vike_log::init` as
    /// `EnvFilter::new("")`, which enables NOTHING and complains about nothing. A live daemon's
    /// JSON audit file goes dark, silently, because somebody left a key with an empty value in
    /// `preferences.toml` — exactly the "configured something false" failure this crate's
    /// consumption gate exists to prevent. `log_file_level` is the same story one layer down.
    ///
    /// `chart_style` rides along because it shares the helper. It is deliberately NOT part of the
    /// argument: it resolves as an INDEX, and a blank one is already inert.
    #[test]
    fn a_blank_preference_value_is_refused_by_key() {
        /// One row of the blank-value table: the key's name, and how to build a patch setting it.
        type BlankCase = (&'static str, fn(String) -> PreferencesPatch);

        let cases: &[BlankCase] = &[
            ("log_level", |v| PreferencesPatch { log_level: Some(v), ..Default::default() }),
            ("log_file_level", |v| PreferencesPatch {
                log_file_level: Some(v),
                ..Default::default()
            }),
            ("chart_style", |v| PreferencesPatch { chart_style: Some(v), ..Default::default() }),
        ];

        // Whitespace, not just "" — `check_non_blank` trims, and a key set to a space is the
        // shape somebody actually leaves behind.
        for (key, patch) in cases {
            for blank in ["", "   ", "\t"] {
                let err = Preferences::default()
                    .apply(patch(blank.to_string()), file())
                    .expect_err(&format!("{key} = {blank:?} must be refused, not accepted"));
                let msg = err.to_string();
                assert!(msg.contains(key), "the refusal must name the offending key: {msg}");
                assert!(
                    msg.contains("blank — omit the key to keep the default"),
                    "and must say what to do instead: {msg}"
                );
            }
        }
    }

    /// The zero guard was pinned; the `==` in it was not. A sweep flipped it to `!=`, which turns
    /// every VALID worker count into a hard startup refusal — including for `vike-tradehub` — while
    /// letting `0` through. Both existing sweep-thread tests survive that mutation: one passes `0`
    /// (refused either way) and one passes `"lots"`, which dies in `parse_usize` before the guard
    /// is ever reached. This is the positive-direction assertion neither of them makes.
    #[test]
    fn a_valid_sweep_thread_count_is_accepted() {
        let mut p = Preferences::default();
        p.apply_env(&HashMap::from([(SWEEP_THREADS_ENV.to_string(), "8".to_string())]))
            .expect("a valid worker count must load");
        assert_eq!(p.sweep_threads, Some(8));
    }
}
