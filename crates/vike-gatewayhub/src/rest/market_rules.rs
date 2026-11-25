//! **The market-rules lane**: a prediction-market venue's RULES texts (`vike_data::source::MarketRulesSource`),
//! served as `Fetch { ClosedMarketRules | SeriesMarketRules | MarketRulesOfToken }` (decision 0164: this
//! daemon makes the venue call, the datahub commits). Polymarket's Gamma directory is its one source
//! (`vike_polymarket::catalog::rules`).
//!
//! Built ONCE from the venue's address rows, like `crate::rest::kline_sources`' registries: a Gamma row the
//! build refuses leaves a source that still answers for its venue (the advertisement keeps it) and
//! refuses every fetch with the reason, which names the missing row.
//!
//! A fetch is ONE call that pages inside, so the only point it can stop at is before the call (the
//! kline lane's rule).

use std::collections::BTreeMap;

use vike_data::TextRow;
use vike_data::source::{MarketRulesSource, SourceError};
use vike_gatewayhub_client::history::MARKET_RULES_VENUE;
use vike_secrets::venue_setting::VenueSettings;

use super::{FetchContext, cancelled};
use crate::data::venues::venue_urls;

/// **The market-rules source, built** from `venue_settings` (polymarket's Gamma row): a stand-in that
/// refuses every fetch, naming the missing row, when the row refuses it.
pub fn market_rules_source(
    venue_settings: &BTreeMap<String, VenueSettings>,
) -> Box<dyn MarketRulesSource> {
    let urls = venue_urls(venue_settings, MARKET_RULES_VENUE);
    match vike_polymarket::catalog::rules::MarketRulesSourcePolymarket::from_urls(&urls) {
        Ok(source) => Box::new(source),
        Err(why) => {
            let why = format!("{MARKET_RULES_VENUE} market rules is NOT RUNNING: {why}");
            tracing::error!("vike-gatewayhub: {why}");
            Box::new(UnbuiltMarketRules { why })
        }
    }
}

/// A source whose address row refused it: it answers for its venue and refuses every fetch, as a
/// `SourceError::Fetch` (cured by a row and a restart, never "this window cannot be expressed").
struct UnbuiltMarketRules {
    why: String,
}

impl MarketRulesSource for UnbuiltMarketRules {
    fn venue(&self) -> &str {
        MARKET_RULES_VENUE
    }

    fn closed_ending(&self, _: i64, _: i64) -> Result<Vec<TextRow>, SourceError> {
        Err(SourceError::Fetch(self.why.as_str().into()))
    }

    fn closed_of_series(&self, _: &str, _: i64, _: i64) -> Result<Vec<TextRow>, SourceError> {
        Err(SourceError::Fetch(self.why.as_str().into()))
    }

    fn of_token(&self, _: &str) -> Result<Vec<TextRow>, SourceError> {
        Err(SourceError::Fetch(self.why.as_str().into()))
    }
}

/// `Fetch { ClosedMarketRules | SeriesMarketRules | MarketRulesOfToken }`: `ask` called once on the source of
/// `venue`. The source's own error crosses as it is.
pub fn fetch_market_rules(
    source: &dyn MarketRulesSource,
    venue: &str,
    ctx: &FetchContext,
    ask: impl FnOnce(&dyn MarketRulesSource) -> Result<Vec<TextRow>, SourceError>,
) -> Result<Vec<TextRow>, SourceError> {
    if venue != source.venue() {
        return Err(SourceError::Refused(format!(
            "gatewayhub: `{venue}` is not served by the market rules lane (served: [{}]); nothing \
             was asked of any venue",
            source.venue()
        )
        .into()));
    }
    if ctx.is_cancelled() {
        return Err(cancelled("before the venue was asked"));
    }
    ask(source)
}

#[cfg(test)]
#[path = "market_rules_tests.rs"]
mod market_rules_tests;
