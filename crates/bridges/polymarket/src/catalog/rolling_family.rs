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
use vike_venues::venue_urls::VenueUrls;

use crate::catalog::discovery::{
    FetchSpec, GammaSource, RollingWindowPlanner, WindowResolverGammaSlug, WindowSpec,
};
use crate::catalog::gamma::GammaClient;
use crate::catalog::universe::UniverseManager;

/// Split a rolling-family name into its slug template and bucket length.
///
/// `btc-updown-5m` → (`"btc-updown-5m-{unix}"`, 5 minutes). The interval is read off the trailing
/// `-<N>m` token because that is how Gamma labels these series
/// (`crate::catalog::discovery::interval_label`: `300000 → "5m"`, and an HOURLY series is spelled
/// `60m`, not `1h` — so only the minute form is accepted, matching the venue rather than inventing
/// a friendlier spelling that would resolve to nothing). The grammar check and its refusal text are
/// `vike_catalog::polymarket_family_minutes`, which a bridge-free process runs offline too.
pub fn window_spec_for(family: &str) -> Result<WindowSpec, String> {
    let minutes = vike_catalog::polymarket_family_minutes(family)?;
    Ok(WindowSpec::every_minutes(minutes, format!("{family}-{{unix}}")))
}

pub struct RollingFamily {
    family: String,
    planner: RollingWindowPlanner,
    /// Required by [`RollingWindowPlanner::plan`] and deliberately NEVER committed: only
    /// [`RollingTick::target`](crate::catalog::discovery::RollingTick::target) — the full desired set — is
    /// used, and the caller's subscription set is the single source of truth for what is actually
    /// subscribed. Two stateful views of that could drift apart; one cannot.
    universe: UniverseManager,
    gamma: Box<dyn GammaSource + Send>,
    fetch: FetchSpec,
}

impl RollingFamily {
    /// Over the real Gamma directory, at the venue's Gamma row among `urls`. `Err` is the refused
    /// family name or the missing row, in words.
    pub fn new(family: &str, urls: &VenueUrls) -> Result<Self, String> {
        let gamma = GammaClient::from_urls(urls).map_err(|e| e.to_string())?;
        Self::with_source(family, Box::new(gamma), FetchSpec::default())
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
        let resolver = WindowResolverGammaSlug::new(self.gamma.as_ref(), self.fetch);
        Ok(self.planner.plan(now_ms, &resolver, &self.universe)?.target)
    }
}

/// Builds the resolver of a family that passed the grammar check: [`RollingFamily::new`] over the real
/// Gamma directory in production, a scripted source in a suite ([`RollingFamily::with_source`]).
pub type ResolverFactory =
    Box<dyn Fn(&str) -> Result<Box<dyn SymbolResolver>, String> + Send + Sync>;

/// **polymarket's rolling-family lane**: the `vike_bridge_core::family::FamilySpec` the gatewayhub's family
/// engine (`crates/vike-gatewayhub/src/rest/family.rs`'s `FamilyHub`) runs. MOVED from the gatewayhub's
/// `rest::family` (owner ruling of 2026-11-12: one-venue code does not live in a shared crate): the
/// grammar check is `vike_catalog::polymarket_family_minutes` (the client runs the same one at its own
/// startup, so a malformed row is refused with the gatewayhub down), and a family's resolver is a
/// [`RollingFamily`] over the Gamma directory.
pub struct FamilySpecPolymarket {
    make: ResolverFactory,
}

impl FamilySpecPolymarket {
    /// The production lane: [`RollingFamily::new`] over the real Gamma directory, at polymarket's address
    /// rows `urls`.
    pub fn gamma(urls: VenueUrls) -> Self {
        Self::with_factory(Box::new(move |family: &str| {
            Ok(Box::new(RollingFamily::new(family, &urls)?) as Box<dyn SymbolResolver>)
        }))
    }

    /// A lane whose resolvers `make` builds — the seam a suite scripts a Gamma source through
    /// ([`RollingFamily::with_source`]).
    pub fn with_factory(make: ResolverFactory) -> Self {
        Self { make }
    }
}

impl vike_bridge_core::family::FamilySpec for FamilySpecPolymarket {
    fn venue(&self) -> &'static str {
        crate::feed::VENUE
    }

    fn admit(&self, family: &str) -> Result<(), String> {
        vike_catalog::polymarket_family_minutes(family).map(|_| ())
    }

    fn resolver(&self, family: &str) -> Result<Box<dyn SymbolResolver>, String> {
        (self.make)(family)
    }
}

impl std::fmt::Debug for FamilySpecPolymarket {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FamilySpecPolymarket").finish_non_exhaustive()
    }
}

#[path = "rolling_family_tests.rs"]
#[cfg(test)]
mod rolling_family_tests;
