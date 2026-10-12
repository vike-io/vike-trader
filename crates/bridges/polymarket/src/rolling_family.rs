//! `RollingFamily` — a ROTATING polymarket family (`btc-updown-5m`) as a
//! `vike_data::live::SymbolResolver`. An up/down market lives five minutes, so a customer names
//! the FAMILY and this resolves it to the live window's token ids on every tick. Assembly of what
//! this crate already owns (`discovery`'s planner and by-slug resolver) — moved here from
//! `vike-recorder` on 2026-09-28 so the recorder names no bridge.
//!
//! **The family name IS the slug template minus its `-{unix}` suffix.** `btc-updown-5m` names the
//! windows `btc-updown-5m-{unix}`, which is how Gamma spells that series today. The profile key is
//! therefore a thing a customer can verify by pasting into polymarket.com, not a private alias —
//! and the trailing `-5m` carries the bucket length, so nothing else has to be configured.
use std::collections::BTreeSet;

use vike_data::live::SymbolResolver;

use crate::discovery::{
    FetchSpec, GammaSlugResolver, GammaSource, RollingWindowPlanner, WindowSpec,
};
use crate::gamma::GammaClient;
use crate::universe::UniverseManager;

/// Split a rolling-family name into its slug template and bucket length.
///
/// `btc-updown-5m` → (`"btc-updown-5m-{unix}"`, 5 minutes). The interval is read off the trailing
/// `-<N>m` token because that is how Gamma labels these series
/// (`crate::discovery::interval_label`: `300000 → "5m"`, and an HOURLY series is spelled
/// `60m`, not `1h` — so only the minute form is accepted, matching the venue rather than inventing
/// a friendlier spelling that would resolve to nothing).
pub fn window_spec_for(family: &str) -> Result<WindowSpec, String> {
    let minutes = trailing_interval_minutes(family).ok_or_else(|| {
        format!(
            "polymarket family `{family}` does not end in an interval like `-5m` — a rolling family \
             is named after its window slugs (`btc-updown-5m` ⇒ `btc-updown-5m-<unix>`), and the \
             interval is what says how long a window lasts"
        )
    })?;
    Ok(WindowSpec::every_minutes(minutes, format!("{family}-{{unix}}")))
}

/// The `<N>` of a trailing `-<N>m`, when `N` is a positive integer.
fn trailing_interval_minutes(family: &str) -> Option<i64> {
    let tail = family.rsplit('-').next()?;
    let n: i64 = tail.strip_suffix('m')?.parse().ok()?;
    (n > 0).then_some(n)
}

pub struct RollingFamily {
    family: String,
    planner: RollingWindowPlanner,
    /// Required by [`RollingWindowPlanner::plan`] and deliberately NEVER committed: only
    /// [`RollingTick::target`](crate::discovery::RollingTick::target) — the full desired set — is
    /// used, and the caller's subscription set is the single source of truth for what is actually
    /// subscribed. Two stateful views of that could drift apart; one cannot.
    universe: UniverseManager,
    gamma: Box<dyn GammaSource + Send>,
    fetch: FetchSpec,
}

impl RollingFamily {
    /// Over the real Gamma directory.
    pub fn new(family: &str) -> Result<Self, String> {
        Self::with_source(family, Box::new(GammaClient), FetchSpec::default())
    }

    /// With an injected Gamma source — the offline test seam, mirroring `discovery`'s own
    /// fixture-source discipline.
    pub fn with_source(
        family: &str,
        gamma: Box<dyn GammaSource + Send>,
        fetch: FetchSpec,
    ) -> Result<Self, String> {
        let spec = window_spec_for(family)?;
        Ok(Self {
            family: family.to_string(),
            planner: RollingWindowPlanner::new(spec),
            universe: UniverseManager::default(),
            gamma,
            fetch,
        })
    }
}

impl SymbolResolver for RollingFamily {
    fn group(&self) -> Option<&str> {
        Some(&self.family)
    }

    /// `plan`, not `tick`: planning leaves the `UniverseManager` uncommitted, and a resolver error
    /// aborts the whole pass so the desired set stays UNKNOWN rather than collapsing to empty —
    /// which the recorder's runtime reads as "change nothing".
    fn desired(&mut self, now_ms: i64) -> Result<BTreeSet<String>, String> {
        let resolver = GammaSlugResolver::new(self.gamma.as_ref(), self.fetch);
        Ok(self.planner.plan(now_ms, &resolver, &self.universe)?.target)
    }
}

#[path = "rolling_family_tests.rs"]
#[cfg(test)]
mod rolling_family_tests;
