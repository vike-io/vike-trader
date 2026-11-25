//! `rules` — a Polymarket market's RULES as `kind=text` rows (`vike_data::text_log::TEXT_KIND_POLYMARKET_RULES`,
//! whose doc names every column), read from Gamma: the question, the rules (`description`), the resolution
//! source and, once the market has resolved, its final outcome. The datahub stores the rows (decision 0164:
//! this daemon-side half fetches, the datahub commits); [`MarketRulesSourcePolymarket`] is the
//! `vike_data::source::MarketRulesSource` the gatewayhub serves them from.
//!
//! # Two reads, two clocks
//!
//! * **A LIVE read** ([`rules_row`] through `of_token`): the market listing one outcome token, as Gamma
//!   lists it now, received now. The datahub's recorder asks it for every token it records, so a market's
//!   rules are kept from the moment vike records it, and a CHANGED wording is a later row of the same text.
//! * **The ARCHIVE** ([`archived_rows`] through `closed_ending`): every CLOSED market whose scheduled end
//!   falls in a window (Gamma's `/markets/keyset?closed=true&end_date_min=…&end_date_max=…`, paged by the
//!   cursor Gamma hands back, [`closed_markets_ending`]), as rows of ONE text on two clocks. Its RULES are received at
//!   its close (`closedTime`), with no outcome. Once it has resolved, its final OUTCOME is a later row of
//!   the same text (the rules again, `actual` the outcome, `amends` its condition id, as a calendar
//!   event's `actual` lands later), received at its RESOLUTION: Gamma's `umaEndDate`, else the archive's
//!   FETCH time (conservative), and never before the close. A study deciding after the close and before
//!   the resolution reads the rules and no outcome. A market whose close cannot be read gives no row: no
//!   time is ever guessed.
//! * **ONE recurring SERIES** ([`archived_rows`] through `closed_of_series`): the same rows, of the closed
//!   markets of one series alone, asked by their slugs (`btc-updown-5m` names the markets
//!   `btc-updown-5m-{start in epoch seconds}`, one every five minutes: [`series_slugs`]), 100 slugs a
//!   request ([`closed_markets_of_slugs`]). A day of 5-minute windows is three requests where the archive
//!   walk reads every market Gamma closed that day (27,172 ending 2026-10-05). MEASURED (the CI box and the latency box,
//!   2026-11-19):
//!   `/markets/keyset` takes a repeated `slug=` and answers the closed markets among them on one page,
//!   refuses more than 100 slugs with HTTP 422, and ignores `series_id`/`series_slug`, which is why the
//!   slug list is the way in.
//!
//! ⚠ **Why the archive keeps no row at a market's start**, though `published_ms` says when it opened:
//! Gamma serves TODAY's `description`, so a rules row received at the start would carry every
//! clarification the venue wrote during the market's life back to its first day (lookahead), and each
//! day of market ends would scatter its commit over every start date (the store seals one part per UTC
//! date). A past market's rules are therefore known to a study from its close; the recorder's live rows
//! know a recorded market's rules from the moment vike recorded it.
//!
//! # What counts as resolved ([`final_outcome`])
//!
//! A closed market whose every outcome price is exactly 0, ½ or 1 and whose prices sum to 1: Gamma's
//! settled payout. The outcome is the one that pays 1, or every outcome's payout when none pays whole
//! (a 50-50 resolution, `Yes=0.5, No=0.5`). A closed market still priced at its last trade (`0.9995`)
//! is not resolved yet and carries no outcome.
//!
//! MEASURED live (one public read from the CI box, 2026-11-17, `closed=true&end_date_min=2025-01-01T00:00:00Z
//! &end_date_max=2025-01-02T00:00:00Z&limit=500`): the filter holds (29 markets, every `endDate` inside the
//! day, every one with a `description`); `closedTime` is NOT RFC 3339 but `2025-01-01 10:23:14+00` (a space,
//! an hours-only offset), which [`gamma_instant_ms`] reads; and a market resolved EARLY closed months before
//! its `endDate` (`2024-10-21` against `2025-01-01`), so its row is received then, though the day loop
//! fetches it by its end. Should Gamma ever stop honouring the filter, the walk stops at
//! [`RULES_MAX_PAGES`] with an error, and every market is re-checked against the window by its own
//! `endDate`, so nothing outside a window is ever stored as in it.
//!
//! ⚠ MEASURED live (the CI box and the latency box, 2026-11-17): Gamma lists AT MOST 100 markets a page whatever
//! `limit` asks, on both routes. The walk once read the offset `/markets` and ended at the first page
//! shorter than its limit, so it stored 100 markets a day; that route is also deprecated (its sunset
//! header says 2026-05-01) and refuses an offset past 2000 with HTTP 422. The markets ending 2026-10-05
//! are 27,172: 272 keyset pages, about 70 s of reads and 35 MB of question and rules text, the last page
//! 72 markets with a null `next_cursor`. So the walk follows `next_cursor` (sent back as
//! `after_cursor`) until Gamma names none, and the keyset rows carry every field the offset rows do,
//! value for value (the 29 markets ending 2025-01-01 read both ways).
//!
//! MEASURED live (one public read from the CI box, 2026-11-17, `closed=true&limit=5
//! &end_date_min=2025-06-01T00:00:00Z&end_date_max=2025-06-02T00:00:00Z`): every market carries
//! `umaEndDate` in RFC 3339 (`2025-03-14T13:53:31Z`), `umaResolutionStatus` `resolved` and
//! `automaticallyResolved` true, and its `umaEndDate` EQUALS its `closedTime` to the second (Gamma closed
//! each market when UMA settled it), so its outcome row is received at the close's own instant, after the
//! rules row of the same append. `updatedAt` follows by a minute to a day (a later edit of the listing,
//! not the resolution), and the parent event's `closedTime` is the EVENT's (days after three of its
//! markets'), so neither is read. The two clocks stay apart for a market Gamma closes before UMA settles
//! it, which this read did not show.

use vike_bridge_core::catalog_read::{CatalogPage, CatalogPageWalk, CatalogPageWalkEnd};
use vike_data::source::{MarketRulesSource, SourceError};
use vike_data::text_log::{TEXT_KIND_POLYMARKET_RULES, TextRow};
use vike_venues::venue_urls::{UrlRefusal, VenueUrls};

use crate::catalog::discovery::render_slug;
use crate::catalog::gamma::{
    GAMMA_SLUGS_PER_QUERY, GammaClient, GammaMarket, gamma_closed_ending_query,
    gamma_closed_slugs_query,
};
use crate::catalog::rolling_family::window_spec_for;
use crate::refusals::PolymarketRulesRefusal;

/// The `source` column of a rules row: the venue that states the rules.
pub const RULES_SOURCE: &str = "Polymarket";

/// The venue the rules are read from.
const VENUE: &str = "polymarket";

/// The market page a row's `url` names (a human link, never dialled): `…/event/<event>/<market>`, the
/// form `crate::catalog::market_ref` reads back.
const MARKET_PAGE: &str = "https://polymarket.com/event/";

/// One Gamma page of the archive walk, as ASKED (the crate's other browses' `limit=500`). Gamma answers
/// fewer (the module doc's 100), so a page's size never ends the walk.
const RULES_PAGE_LIMIT: usize = 500;

/// The most pages one archive window reads: 100,000 markets ending in one window at Gamma's 100 a page,
/// near four times the largest day measured (the module doc's 27,172). Reaching it refuses the window,
/// never stores it short.
pub const RULES_MAX_PAGES: usize = 1_000;

/// A Gamma instant (`2024-01-04T22:58:00Z`, `2024-11-06 15:43:47+00`, `…T…​.394Z`) as epoch
/// milliseconds UTC: a space for the `T`, a `+00`/`+00:00`/`+0000`/`Z` offset, a fraction cut to
/// milliseconds. `None` for an empty, malformed or non-UTC spelling: a clock is never guessed.
pub fn gamma_instant_ms(spelled: &str) -> Option<i64> {
    let spelled = spelled.trim().replacen(' ', "T", 1);
    let body = ["+00:00", "+0000", "+00", "Z"].iter().find_map(|z| spelled.strip_suffix(z))?;
    let body = match body.split_once('.') {
        Some((whole, fraction)) => {
            format!("{whole}.{}", fraction.chars().take(3).collect::<String>())
        }
        None => body.to_string(),
    };
    vike_model::time::parse_utc_timestamp_ms(&format!("{body}Z"))
}

/// When the market's rules were listed: its `startDate`, else its `createdAt`.
pub fn opened_ms(market: &GammaMarket) -> Option<i64> {
    gamma_instant_ms(&market.start_date).or_else(|| gamma_instant_ms(&market.created_at))
}

/// The final outcome of a RESOLVED market (the module doc's rule), `None` while it has not resolved.
pub fn final_outcome(market: &GammaMarket) -> Option<String> {
    let prices = &market.outcome_prices;
    if !market.closed || prices.is_empty() || prices.len() != market.outcomes.len() {
        return None;
    }
    // A naive fold on purpose: net-new Rust with no Python twin, over prices already checked to be
    // exactly 0, 0.5 or 1, so every partial sum is exact (`parity_fold_gate`'s polymarket row counts it).
    let settled = prices.iter().all(|p| *p == 0.0 || *p == 0.5 || *p == 1.0)
        && (prices.iter().sum::<f64>() - 1.0).abs() < 1e-9;
    if !settled {
        return None;
    }
    if let Some(winner) = prices.iter().position(|p| *p == 1.0) {
        return Some(market.outcomes[winner].clone());
    }
    let payouts: Vec<String> =
        market.outcomes.iter().zip(prices).map(|(o, p)| format!("{o}={p}")).collect();
    Some(payouts.join(", "))
}

/// The market as a rules row received at `received_ms` (no outcome: [`archived_rows`] adds it).
pub fn rules_row(market: &GammaMarket, received_ms: i64) -> TextRow {
    let event = if market.event_slug.is_empty() { &market.slug } else { &market.event_slug };
    TextRow {
        published_ms: opened_ms(market).unwrap_or(0),
        source_id: market.condition_id.clone(),
        subject: market.token_ids.join(" "),
        period: market.end_date.clone(),
        title: market.question.clone(),
        summary: market.resolution_source.clone(),
        body: market.description.clone(),
        url: format!("{MARKET_PAGE}{event}/{}", market.slug),
        tags: TextRow::joined_tags(&market.outcomes),
        ..TextRow::new(TEXT_KIND_POLYMARKET_RULES, RULES_SOURCE, received_ms)
    }
}

/// A closed market's archive rows (the module doc), in the order they are stored: its rules received at
/// its close with no outcome, then, once it has resolved, the outcome row received at its resolution
/// (`umaEndDate`, else `fetched_ms`, when the archive read it; never before the close). None when the
/// close cannot be read.
pub fn archived_rows(market: &GammaMarket, fetched_ms: i64) -> Vec<TextRow> {
    let Some(closed_ms) = gamma_instant_ms(&market.closed_time) else {
        return Vec::new();
    };
    let rules = rules_row(market, closed_ms);
    let Some(outcome) = final_outcome(market) else {
        return vec![rules];
    };
    let resolved_ms = gamma_instant_ms(&market.uma_end_date).unwrap_or(fetched_ms).max(closed_ms);
    let resolved = TextRow {
        actual: outcome,
        amends: market.condition_id.clone(),
        ..rules_row(market, resolved_ms)
    };
    vec![rules, resolved]
}

/// **Every closed market whose scheduled end falls in `[start_ms, end_ms]`**, read whole through `fetch`
/// (`fetch(after_cursor)`: one Gamma keyset page, `""` the first, as its markets and the next page's
/// cursor, [`GammaClient::keyset_page`]). The listing ends where Gamma names no next cursor; a page's
/// size never ends it (Gamma lists fewer than it is asked, the module doc). STRICT: any page failing, a
/// cursor named twice in a row, or a listing still going after [`RULES_MAX_PAGES`], is an error, never a
/// short window. A market Gamma returns whose own `endDate` lies outside the window, or that is not
/// closed, is dropped; a market listed twice is kept once.
pub fn closed_markets_ending(
    start_ms: i64,
    end_ms: i64,
    fetch: impl Fn(&str) -> Result<(Vec<GammaMarket>, Option<String>), String>,
) -> Result<Vec<GammaMarket>, PolymarketRulesRefusal> {
    let markets = closed_listing(fetch)?;
    let mut seen = std::collections::HashSet::new();
    Ok(markets
        .into_iter()
        .filter(|m| m.closed)
        .filter(|m| {
            gamma_instant_ms(&m.end_date).is_none_or(|at| (start_ms..=end_ms).contains(&at))
        })
        .filter(|m| seen.insert(m.condition_id.clone()))
        .collect())
}

/// One keyset listing of closed markets read whole through `fetch` (`fetch(after_cursor)`, `""` the first),
/// until Gamma names no next cursor. STRICT: any page failing, a cursor named twice in a row, or a listing
/// still going after [`RULES_MAX_PAGES`], is an error, never a short listing.
fn closed_listing(
    mut fetch: impl FnMut(&str) -> Result<(Vec<GammaMarket>, Option<String>), String>,
) -> Result<Vec<GammaMarket>, PolymarketRulesRefusal> {
    let walk = CatalogPageWalk { max_pages: RULES_MAX_PAGES, strict: true };
    let (markets, end) = walk
        .walk(String::new(), |cursor| fetch(cursor).map(|(rows, next)| CatalogPage { rows, next }))
        .map_err(|why| PolymarketRulesRefusal::PageUnread { why })?;
    match end {
        CatalogPageWalkEnd::Listed => Ok(markets),
        CatalogPageWalkEnd::Ceiling => {
            Err(PolymarketRulesRefusal::ListingPastCeiling { pages: RULES_MAX_PAGES })
        }
        CatalogPageWalkEnd::Repeated { page } => {
            Err(PolymarketRulesRefusal::CursorRepeated { page })
        }
        CatalogPageWalkEnd::Failed { error, .. } => {
            Err(PolymarketRulesRefusal::PageUnread { why: error })
        }
    }
}

/// **The slugs of the recurring `series`' windows that START in `[start_ms, end_ms]`**, ascending:
/// `{series}-{start in epoch seconds}` for every start on the series' grid (a whole number of windows since
/// the epoch), the window length read off the name's trailing `-<N>m`
/// (`crate::catalog::rolling_family`'s `window_spec_for`, whose refusal in words is the answer for any other
/// name). `btc-updown-5m` over one UTC day is 288 slugs, `…-15m` 96.
pub fn series_slugs(series: &str, start_ms: i64, end_ms: i64) -> Result<Vec<String>, String> {
    let spec = window_spec_for(series)?;
    let floor = start_ms.div_euclid(spec.bucket_ms) * spec.bucket_ms;
    let first = if floor < start_ms { floor + spec.bucket_ms } else { floor };
    Ok((first..=end_ms)
        .step_by(spec.bucket_ms as usize)
        .map(|start| render_slug(&spec.slug_template, start))
        .collect())
}

/// **The closed markets among `slugs`**, asked [`GAMMA_SLUGS_PER_QUERY`] slugs at a time through `fetch`
/// (`fetch(slugs, after_cursor)`: one Gamma keyset page of those slugs' closed markets, `""` the first,
/// [`GammaClient::keyset_page`] over `gamma_closed_slugs_query`). A slug Gamma lists no closed market for is
/// simply absent. STRICT like [`closed_markets_ending`]: a page failing is an error, never a short answer. A
/// page that answers every slug it asked ends its slugs' listing: MEASURED (2026-11-19) Gamma names a cursor
/// after a full page of 100 and the page it names is empty, so following it would double the requests; a
/// shorter page is followed while Gamma names a cursor. A market whose slug was not asked, or that is not
/// closed, is dropped; a market listed twice is kept once.
pub fn closed_markets_of_slugs(
    slugs: &[String],
    fetch: impl Fn(&[String], &str) -> Result<(Vec<GammaMarket>, Option<String>), String>,
) -> Result<Vec<GammaMarket>, PolymarketRulesRefusal> {
    let mut markets = Vec::new();
    for chunk in slugs.chunks(GAMMA_SLUGS_PER_QUERY) {
        let mut listed = 0;
        markets.extend(closed_listing(|cursor| {
            let (rows, next) = fetch(chunk, cursor)?;
            listed += rows.len();
            Ok((rows, next.filter(|_| listed < chunk.len())))
        })?);
    }
    let asked: std::collections::HashSet<&str> = slugs.iter().map(String::as_str).collect();
    let mut seen = std::collections::HashSet::new();
    Ok(markets
        .into_iter()
        .filter(|m| m.closed && asked.contains(m.slug.as_str()))
        .filter(|m| seen.insert(m.condition_id.clone()))
        .collect())
}

/// **The venue's `MarketRulesSource`** over its Gamma row: the gatewayhub's to serve.
#[derive(Debug, Clone)]
pub struct MarketRulesSourcePolymarket {
    gamma: GammaClient,
}

impl MarketRulesSourcePolymarket {
    /// The source over the Gamma row among `urls` (the venue's `venue_url` rows).
    ///
    /// # Errors
    /// The row is missing ([`UrlRefusal`]).
    pub fn from_urls(urls: &VenueUrls) -> Result<Self, UrlRefusal> {
        Ok(Self { gamma: GammaClient::from_urls(urls)? })
    }
}

impl MarketRulesSource for MarketRulesSourcePolymarket {
    fn venue(&self) -> &str {
        VENUE
    }

    fn closed_ending(&self, start_ms: i64, end_ms: i64) -> Result<Vec<TextRow>, SourceError> {
        if start_ms > end_ms {
            let refusal = PolymarketRulesRefusal::WindowInverted { start_ms, end_ms };
            return Err(SourceError::Refused(refusal.carried()));
        }
        let query = gamma_closed_ending_query(start_ms, end_ms, RULES_PAGE_LIMIT);
        let markets = closed_markets_ending(start_ms, end_ms, |cursor| {
            self.gamma.keyset_page(&query, cursor)
        })
        .map_err(|refusal| SourceError::Fetch(refusal.carried()))?;
        let fetched_ms = vike_model::now_ms();
        Ok(markets.iter().flat_map(|m| archived_rows(m, fetched_ms)).collect())
    }

    fn closed_of_series(
        &self,
        series: &str,
        start_ms: i64,
        end_ms: i64,
    ) -> Result<Vec<TextRow>, SourceError> {
        if start_ms > end_ms {
            let series = series.to_string();
            let refusal = PolymarketRulesRefusal::SeriesWindowInverted { series, start_ms, end_ms };
            return Err(SourceError::Refused(refusal.carried()));
        }
        let slugs = series_slugs(series, start_ms, end_ms)
            .map_err(|why| SourceError::Refused(why.into()))?;
        let markets = closed_markets_of_slugs(&slugs, |chunk, cursor| {
            self.gamma.keyset_page(&gamma_closed_slugs_query(chunk), cursor)
        })
        .map_err(|refusal| SourceError::Fetch(refusal.carried()))?;
        tracing::info!(
            "polymarket rules {series} from {}: {} of {} windows listed closed, {} absent",
            vike_model::time::epoch_ms_to_utc_timestamp(start_ms),
            markets.len(),
            slugs.len(),
            slugs.len().saturating_sub(markets.len())
        );
        let fetched_ms = vike_model::now_ms();
        Ok(markets.iter().flat_map(|m| archived_rows(m, fetched_ms)).collect())
    }

    fn of_token(&self, token: &str) -> Result<Vec<TextRow>, SourceError> {
        if !vike_catalog::is_outcome_token(token) {
            let refusal = PolymarketRulesRefusal::NotAToken { token: token.to_string() };
            return Err(SourceError::Refused(refusal.carried()));
        }
        let market = self.gamma.by_token(token).map_err(|why| SourceError::Fetch(why.into()))?;
        Ok(market.map(|m| rules_row(&m, vike_model::now_ms())).into_iter().collect())
    }
}

#[path = "rules_tests.rs"]
#[cfg(test)]
mod rules_tests;
