//! The `[engine.resolution]` table: `ResolutionCfg`, the binary-outcome settlement source.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde::Deserialize;

use super::parse_ts;
use crate::harness::HarnessError;
use vike_sim::ResolutionSource;
use vike_strategy::TokenId;

#[cfg(doc)]
use super::{BacktestProfile, EngineCfg};

/// TOML shape of the opt-in binary-resolution settlement source ([`EngineCfg::resolution`]).
///
/// One `kind` today — `"binary_outcome"`, the two-outcome prediction-market convention the
/// `cheap_np` tape uses: a series symbol is `<slug>#<outcome_index>` where the slug's trailing
/// `-<sts>` is the window open in epoch SECONDS ([`vike_strategy::TokenId`] is the parser, shared with
/// the strategy so the two cannot drift). A token pays `1.0` when its `outcome_index` equals the
/// window's `winning_index` and `0.0` otherwise, from `(sts + window_secs) × 1000` onward;
/// every other symbol (a spot reference series, another window's token) resolves to `None` and
/// is never settled or latched.
///
/// The winners map comes from an inline `[engine.resolution.winners]` table, a `slug,winning_index`
/// CSV named by `path` (the `FORMAT CSVWithNames` export `cheap_np_run` reads — a header row and
/// quoted slugs are both tolerated), or both (inline wins on a clash).
///
/// **Non-binary `winning_index` values are REAL and are never coerced.** The on-chain
/// resolutions table carries `2`/`3`/`4` rows (markets whose outcome a two-outcome payout
/// cannot represent) and `-1` for unresolved. A CSV row like that is DROPPED with a warning —
/// it is a data fact, and a month-wide export legitimately contains a handful — and an INLINE
/// entry like that is a hard error, since a hand-written value is an authoring mistake. Either
/// way the window then has no payout, so [`ResolutionCfg::build`] REJECTS the profile if any
/// series symbol in the run parses as a window token with no winner: an unsettled position
/// would otherwise be silently marked at its last traded price and reported as PnL.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResolutionCfg {
    /// currently only `"binary_outcome"`
    pub kind: String,
    /// Seconds from a window's open (`sts`) to its resolution. Defaults to
    /// [`vike_model::fair::UPDOWN_WINDOW_SECS`] (300). NOTE the symbol GRAMMAR is independent of this:
    /// `TokenId::parse` pins `sts` to a multiple of `UPDOWN_WINDOW_SECS`, so a different `window_secs`
    /// moves the payout TIME without redefining the grid.
    #[serde(default = "default_window_secs")]
    pub window_secs: i64,
    /// Optional `slug,winning_index` CSV. Relative paths resolve against the profile file's own
    /// directory ([`BacktestProfile::base_dir`]), not the CWD.
    #[serde(default)]
    pub path: Option<String>,
    /// Optional inline `slug = winning_index` table. Merged over `path`'s rows.
    #[serde(default)]
    pub winners: BTreeMap<String, i64>,
    /// Explicit override for [`vike_sim::EngineParams::resolution_end_ts`] (epoch-ms or
    /// `YYYY-MM-DDTHH`). Absent = the LATEST resolution among the run's own series
    /// (`max(sts) + window_secs`), which is what pins the end-of-run sweep to the window rather
    /// than to the `RESOLUTION_PROBE_SENTINEL`.
    #[serde(default)]
    pub end_ts: Option<String>,
}

fn default_window_secs() -> i64 {
    vike_model::fair::UPDOWN_WINDOW_SECS
}

impl ResolutionCfg {
    /// Structural checks that need no I/O — run at profile-parse time by
    /// [`BacktestProfile::validate`]. The winners-file read and the coverage check live in
    /// [`Self::build`], which is the first point that knows the profile's `base_dir`.
    pub(super) fn validate_shape(&self) -> Result<(), HarnessError> {
        if self.kind != "binary_outcome" {
            return Err(HarnessError::Validation(format!(
                "unknown engine.resolution.kind {:?} (known: \"binary_outcome\")",
                self.kind
            )));
        }
        if self.window_secs <= 0 {
            return Err(HarnessError::Validation(format!(
                "engine.resolution.window_secs must be > 0, got {}",
                self.window_secs
            )));
        }
        for (slug, wi) in &self.winners {
            if !(0..=1).contains(wi) {
                return Err(HarnessError::Validation(format!(
                    "engine.resolution.winners[{slug:?}] = {wi} is not a binary outcome index \
                     (0 or 1). Non-binary and unresolved (-1) values are real on-chain facts and \
                     are never coerced to \"both sides lose\" — drop the window from the run \
                     instead."
                )));
            }
        }
        if let Some(s) = &self.end_ts {
            parse_ts(s)?;
        }
        Ok(())
    }

    /// Build the settlement source + its `resolution_end_ts` for a run over `symbols`.
    ///
    /// `base_dir` resolves a relative [`Self::path`]. Fails when a series symbol parses as a
    /// window token whose slug has no binary winner (see the type doc for why that is an error
    /// and not a shrug).
    pub fn build(
        &self,
        base_dir: Option<&Path>,
        symbols: &[String],
    ) -> Result<(ResolutionSource, Option<i64>), HarnessError> {
        self.validate_shape()?;
        let mut winners: BTreeMap<String, u8> = BTreeMap::new();
        if let Some(path) = &self.path {
            let path = match (Path::new(path).is_relative(), base_dir) {
                (true, Some(dir)) => dir.join(path),
                _ => PathBuf::from(path),
            };
            let text = std::fs::read_to_string(&path)
                .map_err(|e| HarnessError::Io(format!("{}: {e}", path.display())))?;
            let mut dropped = 0usize;
            for line in text.lines() {
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                let Some((slug, wi)) = line.split_once(',') else { continue };
                // ClickHouse `FORMAT CSVWithNames` quotes the slug; a quoted key matches NO
                // token symbol, so unquoting is load-bearing, not cosmetic.
                let slug = slug.trim().trim_matches('"');
                let Ok(wi) = wi.trim().parse::<i64>() else {
                    continue; // the CSVWithNames header row lands here
                };
                match wi {
                    0 | 1 => {
                        winners.insert(slug.to_string(), wi as u8);
                    }
                    // NOT coerced — a market that resolved to something a two-outcome payout
                    // cannot represent, or an unresolved (-1) row. Dropped here; the coverage
                    // check below turns it into a hard error if the run actually needs it.
                    _ => dropped += 1,
                }
            }
            if dropped > 0 {
                tracing::warn!(
                    path = %path.display(),
                    dropped,
                    "engine.resolution: dropped rows with a non-binary winning_index"
                );
            }
        }
        for (slug, wi) in &self.winners {
            winners.insert(slug.clone(), *wi as u8);
        }

        // Coverage: every tradeable window token in the run must have a payout, else its
        // position would silently end the run marked at the last traded price.
        let window_secs = self.window_secs;
        let mut latest_sts: Option<i64> = None;
        let mut missing: BTreeSet<String> = BTreeSet::new();
        for sym in symbols {
            let Some(tok) = TokenId::parse(sym) else { continue };
            let slug = sym.rsplit_once('#').map(|(s, _)| s).unwrap_or(sym);
            if !winners.contains_key(slug) {
                missing.insert(slug.to_string());
            }
            latest_sts = Some(latest_sts.map_or(tok.sts, |m: i64| m.max(tok.sts)));
        }
        if !missing.is_empty() {
            let names: Vec<&str> = missing.iter().take(5).map(String::as_str).collect();
            return Err(HarnessError::Validation(format!(
                "engine.resolution: {} window slug(s) in data.series have no binary \
                 winning_index (first: {names:?}). A window with no payout would end the run \
                 marked at its last traded price — remove it from the slice or supply its \
                 resolution.",
                missing.len()
            )));
        }

        let end_ts = match &self.end_ts {
            Some(s) => Some(parse_ts(s)?),
            None => latest_sts.map(|sts| (sts + window_secs) * 1000),
        };

        let source: ResolutionSource = Box::new(move |sym: &str, ts: i64| {
            let tok = TokenId::parse(sym)?;
            let slug = sym.rsplit_once('#').map(|(s, _)| s)?;
            let wi = *winners.get(slug)?;
            if ts < (tok.sts + window_secs) * 1000 {
                return None; // still trading
            }
            Some(if tok.oidx == wi { 1.0 } else { 0.0 })
        });
        Ok((source, end_ts))
    }
}
