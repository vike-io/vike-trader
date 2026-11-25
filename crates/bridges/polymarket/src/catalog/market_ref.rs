//! `market_ref` — what an operator may TYPE to name a Polymarket outcome, and the read that turns it
//! into the one thing an order is signed on: the outcome's ERC-1155 `token_id`.
//!
//! A Polymarket order names a TOKEN (a 77-digit decimal), never a market, and a market has two or
//! more outcome tokens. Before this module the only spelling the bridge understood was the pasted id,
//! and anything else reached the exec thread as `cannot resolve the NegRisk signing domain for token`
//! followed by whatever was typed. The spellings now understood are:
//!
//! | typed | meaning |
//! |---|---|
//! | `3233822019…` (20+ digits) | the outcome token id itself — used UNCHANGED, with no network read |
//! | `xi-jinping-out-before-2027:Yes` | the market's slug and the outcome's label (case-insensitive) |
//! | `https://polymarket.com/event/<event>/<market>` | the exact market page; its last path segment is the slug |
//!
//! A slug WITHOUT an outcome, and an event page with several markets, are REFUSED IN WORDS with the
//! exact spellings that would work: which side of a market to buy is a money decision no default may
//! make.
//!
//! ## One name
//! The canonical symbol of an outcome stays its token id (`Instrument::raw_symbol` in
//! `crate::catalog::markets_to_instruments`, and what a venue fill carries as its `asset_id`).
//! `slug:Outcome` and the URL are INPUT spellings that [`canonical_polymarket_symbol`] turns into
//! that id BEFORE an order exists. They are not aliases an engine may hold: an order submitted under
//! the slug would book its fills under the token id, and one position would have two names. That is
//! why the exec thread does NOT translate a slug itself ([`explain_symbol`] only EXPLAINS the
//! refusal, with the ids): the translation belongs at the door that admits a symbol
//! (`crates/vike-catalog/src/venue_symbol.rs`'s `SYMBOL_RULES`, a shared module this
//! crate does not edit). The door does not call it itself (the tradehub links no bridge and opens no
//! venue socket): it asks the gatewayhub, over the account's trade lease, and the gatewayhub's
//! `SymbolFacts::facts_by_name` calls [`canonical_polymarket_symbol`] and answers the facts of the token.
//!
//! The reads are Gamma's keyless `GET /markets/keyset?slug=` and `GET /events/keyset?slug=` (live-verified
//! field shapes: `crate::catalog::gamma`'s module doc; the keyset routes answer exactly as the deprecated
//! `/markets?slug=` / `/events?slug=` did, `gamma_slug_query`'s doc), through the same proxy-aware agent as
//! every other read.

use vike_venues::venue_urls::VenueUrls;

use crate::catalog::gamma::{GammaClient, GammaMarket};
use crate::refusals::PolymarketMarketRefRefusal;

/// What an operator typed, classified. Pure: no network.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MarketRef {
    /// An outcome token id — canonical already.
    Token(String),
    /// A market slug, optionally naming one outcome by its label.
    Market { slug: String, outcome: Option<String> },
    /// A `polymarket.com/event/<slug>` page with no market segment: one event, possibly many markets.
    Event { slug: String },
}

/// A slug is lower-case words joined by `-` (digits allowed). It must hold a LETTER: a short run of
/// digits is a mistyped token id, never a market, and must not cost a Gamma request.
fn slug_shaped(s: &str) -> bool {
    s.bytes().next().is_some_and(|b| b.is_ascii_alphanumeric())
        && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
        && s.bytes().any(|b| b.is_ascii_alphabetic())
}

/// Classify what the operator typed. A refusal carries the spellings that would work.
pub fn parse_market_ref(input: &str) -> Result<MarketRef, PolymarketMarketRefRefusal> {
    let s = input.trim();
    if s.is_empty() {
        return Err(PolymarketMarketRefRefusal::Empty);
    }
    // the one "already canonical" predicate (an ERC-1155 uint256: 20 to 78 digits) is vike-catalog's
    if vike_catalog::is_outcome_token(s) {
        return Ok(MarketRef::Token(s.to_string()));
    }
    let lower = s.to_ascii_lowercase();
    if lower.starts_with("http://") || lower.starts_with("https://") {
        return parse_url(s);
    }
    if let Some(hex) = lower.strip_prefix("0x")
        && hex.len() == 64
        && hex.bytes().all(|b| b.is_ascii_hexdigit())
    {
        return Err(PolymarketMarketRefRefusal::ConditionId { typed: s.to_string() });
    }
    let (slug, outcome) = match s.split_once(':') {
        Some((slug, outcome)) => (slug.trim(), Some(outcome.trim())),
        None => (s, None),
    };
    if !slug_shaped(slug) || outcome.is_some_and(str::is_empty) {
        return Err(PolymarketMarketRefRefusal::NotAName { typed: s.to_string() });
    }
    Ok(MarketRef::Market { slug: slug.to_ascii_lowercase(), outcome: outcome.map(str::to_string) })
}

/// `https://polymarket.com/event/<event>/<market>` → the market; `…/event/<event>` → the event;
/// `…/market/<market>` → the market. Anything after `?` or `#` (tracking parameters) is dropped.
fn parse_url(url: &str) -> Result<MarketRef, PolymarketMarketRefRefusal> {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    let (host, path) = rest.split_once('/').unwrap_or((rest, ""));
    let host = host.to_ascii_lowercase();
    if host != "polymarket.com" && host != "www.polymarket.com" {
        return Err(PolymarketMarketRefRefusal::NotPolymarketAddress { url: url.to_string() });
    }
    let path = path.split(['?', '#']).next().unwrap_or("");
    let segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    let (kind, slug, market) = match segments.as_slice() {
        ["event", event] => ("event", *event, None),
        ["event", _event, market] => ("event", *market, Some(*market)),
        ["market", market] => ("market", *market, Some(*market)),
        _ => return Err(PolymarketMarketRefRefusal::NotAMarketPage { url: url.to_string() }),
    };
    if !slug_shaped(slug) {
        return Err(PolymarketMarketRefRefusal::NotASlugSegment { url: url.to_string() });
    }
    let slug = slug.to_ascii_lowercase();
    Ok(match (kind, market) {
        (_, Some(_)) => MarketRef::Market { slug, outcome: None },
        _ => MarketRef::Event { slug },
    })
}

/// The Gamma reads this module needs, as a seam: the real one is [`GammaClient`], a test brings its
/// own.
pub trait MarketDirectory {
    /// One market by its exact slug, closed ones included (`Ok(None)` = Gamma lists no such slug).
    fn market(&self, slug: &str) -> Result<Option<GammaMarket>, String>;
    /// Every market of one event by the event's exact slug (`Ok(vec![])` = no such event).
    fn event_markets(&self, slug: &str) -> Result<Vec<GammaMarket>, String>;
}

/// The live directory: Gamma's keyless `/markets/keyset?slug=` and `/events/keyset?slug=`. The market
/// lookup hides a RESOLVED market unless asked for one (measured live, `GammaClient::closed_by_slug`), so a miss is
/// asked again with `closed=true`: a resolved market is then refused as CLOSED, not reported as
/// one that does not exist. The second request is paid on a miss only.
impl MarketDirectory for GammaClient {
    fn market(&self, slug: &str) -> Result<Option<GammaMarket>, String> {
        match self.by_slug(slug, false)? {
            Some(m) => Ok(Some(m)),
            None => self.closed_by_slug(slug),
        }
    }
    fn event_markets(&self, slug: &str) -> Result<Vec<GammaMarket>, String> {
        self.markets_by_event_slug(slug, false)
    }
}

/// One outcome of one market, resolved from what the operator typed.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedOutcome {
    /// The canonical symbol: the outcome's token id.
    pub token_id: String,
    /// The outcome's label as Gamma spells it (`Yes`).
    pub outcome: String,
    pub market_slug: String,
    pub question: String,
    pub condition_id: String,
    /// Gamma's `negRisk` as sent (`None` = the venue did not say).
    pub neg_risk: Option<bool>,
    /// `orderPriceMinTickSize` (`None` when Gamma sent none).
    pub tick_size: Option<f64>,
    /// `orderMinSize` in shares (`None` when Gamma sent none).
    pub min_size: Option<f64>,
}

/// How many sibling markets / outcomes a refusal lists before it says "and N more".
const LISTED: usize = 8;

/// The tradeable outcomes of `m` as `(label, token id)`, or the reason it has none.
fn outcome_tokens(m: &GammaMarket) -> Result<Vec<(String, String)>, PolymarketMarketRefRefusal> {
    let slug = m.slug.clone();
    if m.closed {
        return Err(PolymarketMarketRefRefusal::MarketClosed { slug });
    }
    if !m.active {
        return Err(PolymarketMarketRefRefusal::MarketInactive { slug });
    }
    if m.accepting_orders == Some(false) {
        return Err(PolymarketMarketRefRefusal::MarketNotAcceptingOrders { slug });
    }
    if m.token_ids.is_empty() || m.token_ids.len() != m.outcomes.len() {
        return Err(PolymarketMarketRefRefusal::OutcomesUnmatched {
            outcomes: m.outcomes.len(),
            tokens: m.token_ids.len(),
            slug,
        });
    }
    Ok(m.outcomes.iter().cloned().zip(m.token_ids.iter().cloned()).collect())
}

fn resolved(m: &GammaMarket, outcome: String, token_id: String) -> ResolvedOutcome {
    ResolvedOutcome {
        token_id,
        outcome,
        market_slug: m.slug.clone(),
        question: m.question.clone(),
        condition_id: m.condition_id.clone(),
        neg_risk: m.neg_risk_declared,
        tick_size: (m.tick_size.is_finite() && m.tick_size > 0.0).then_some(m.tick_size),
        min_size: m.order_min_size,
    }
}

/// The "name the outcome" refusal: every outcome with the exact spelling that selects it.
fn choose_outcome(m: &GammaMarket, options: &[(String, String)]) -> PolymarketMarketRefRefusal {
    let spelled: Vec<String> = options
        .iter()
        .take(LISTED)
        .map(|(label, id)| format!("`{}:{label}` (token {id})", m.slug))
        .collect();
    PolymarketMarketRefRefusal::ChooseOutcome {
        slug: m.slug.clone(),
        question: m.question.clone(),
        count: options.len(),
        listed: spelled.join(", "),
        more: and_more(options.len()),
    }
}

/// `, and N more` for the entries past the first [`LISTED`], or nothing.
fn and_more(count: usize) -> String {
    match count.saturating_sub(LISTED) {
        0 => String::new(),
        n => format!(", and {n} more"),
    }
}

fn pick_outcome(
    m: &GammaMarket,
    outcome: Option<&str>,
) -> Result<ResolvedOutcome, PolymarketMarketRefRefusal> {
    let options = outcome_tokens(m)?;
    let Some(want) = outcome else { return Err(choose_outcome(m, &options)) };
    match options.iter().find(|(label, _)| label.eq_ignore_ascii_case(want)) {
        Some((label, id)) => Ok(resolved(m, label.clone(), id.clone())),
        None => Err(PolymarketMarketRefRefusal::NoSuchOutcome {
            slug: m.slug.clone(),
            want: want.to_string(),
            choose: Box::new(choose_outcome(m, &options)),
        }),
    }
}

/// Resolve what the operator typed to ONE outcome. A pasted token id answers itself with no read at
/// all ([`canonical_symbol`] is the cheaper door for that); a slug or an address costs one Gamma
/// request.
pub fn resolve_market_ref(
    input: &str,
    directory: &dyn MarketDirectory,
) -> Result<ResolvedOutcome, PolymarketMarketRefRefusal> {
    let unread = |why| PolymarketMarketRefRefusal::GammaUnread { why };
    match parse_market_ref(input)? {
        MarketRef::Token(token_id) => Ok(ResolvedOutcome {
            token_id,
            outcome: String::new(),
            market_slug: String::new(),
            question: String::new(),
            condition_id: String::new(),
            neg_risk: None,
            tick_size: None,
            min_size: None,
        }),
        MarketRef::Market { slug, outcome } => {
            let m = directory.market(&slug).map_err(unread)?.ok_or_else(|| {
                PolymarketMarketRefRefusal::MarketNotListed { slug: slug.clone() }
            })?;
            pick_outcome(&m, outcome.as_deref())
        }
        MarketRef::Event { slug } => {
            let markets = directory.event_markets(&slug).map_err(unread)?;
            let open: Vec<&GammaMarket> =
                markets.iter().filter(|m| m.active && !m.closed).collect();
            match open.as_slice() {
                [] if markets.is_empty() => {
                    Err(PolymarketMarketRefRefusal::EventNotListed { slug })
                }
                [] => Err(PolymarketMarketRefRefusal::EventAllClosed { slug }),
                [only] => pick_outcome(only, None),
                many => {
                    let names: Vec<String> = many
                        .iter()
                        .take(LISTED)
                        .map(|m| format!("`{}` («{}»)", m.slug, m.question))
                        .collect();
                    Err(PolymarketMarketRefRefusal::EventHoldsMany {
                        slug,
                        count: many.len(),
                        listed: names.join(", "),
                        more: and_more(many.len()),
                    })
                }
            }
        }
    }
}

/// The canonical symbol — the outcome's token id — for what the operator typed. A pasted token id
/// is returned UNCHANGED with no network read; every other spelling costs one Gamma request. A refusal
/// is [`MarketNameRefusal::Unread`] when a read of `directory` failed, else the verdict.
pub fn canonical_symbol(
    input: &str,
    directory: &dyn MarketDirectory,
) -> Result<String, MarketNameRefusal> {
    let resolved = match parse_market_ref(input) {
        Ok(MarketRef::Token(token_id)) => Ok(token_id),
        Ok(_) => resolve_market_ref(input, directory).map(|r| r.token_id),
        Err(why) => Err(why),
    };
    resolved.map_err(|why| match why {
        PolymarketMarketRefRefusal::GammaUnread { why } => MarketNameRefusal::Unread(why),
        verdict => MarketNameRefusal::Refused(verdict.to_string()),
    })
}

/// Why [`canonical_polymarket_symbol`] has no token id: a Gamma READ that failed, or a verdict about the name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MarketNameRefusal {
    /// A Gamma read failed (no answer, or one that did not parse): it says nothing about the name, so ask again.
    Unread(String),
    /// What was typed names no tradeable outcome (or names no Gamma row to ask): a verdict, in the bridge's words.
    Refused(String),
}

impl std::fmt::Display for MarketNameRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MarketNameRefusal::Unread(why) | MarketNameRefusal::Refused(why) => f.write_str(why),
        }
    }
}

/// [`canonical_symbol`] over live Gamma, at the venue's Gamma row among `urls` — the function a door
/// that admits a symbol calls. A missing row is a [`MarketNameRefusal::Refused`] naming it.
pub fn canonical_polymarket_symbol(
    input: &str,
    urls: &VenueUrls,
) -> Result<String, MarketNameRefusal> {
    let gamma =
        GammaClient::from_urls(urls).map_err(|e| MarketNameRefusal::Refused(e.to_string()))?;
    canonical_symbol(input, &gamma)
}

/// The words for an order whose symbol is a market NAME and not a token id, or `None` when the
/// symbol is not name-shaped at all (a token id, or something no spelling reads) and the caller's own
/// refusal should stand. Costs one Gamma request for a name-shaped symbol, so it belongs on the
/// REFUSAL path only. Never translates: see the module doc's "One name".
pub fn explain_symbol(symbol: &str, directory: &dyn MarketDirectory) -> Option<String> {
    match parse_market_ref(symbol) {
        Ok(MarketRef::Token(_)) | Err(_) => None,
        Ok(_) => Some(match resolve_market_ref(symbol, directory) {
            Ok(r) => format!(
                "polymarket: `{symbol}` is a market name, and an order is placed on an outcome's \
                 TOKEN id. That one is {} of «{}»: send the order on the symbol {}. The order was \
                 NOT sent",
                r.outcome, r.question, r.token_id
            ),
            Err(why) => format!("{why}. The order was NOT sent"),
        }),
    }
}

#[path = "market_ref_tests.rs"]
#[cfg(test)]
mod market_ref_tests;
