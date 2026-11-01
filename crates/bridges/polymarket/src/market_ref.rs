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
//! (`crates/vike-tradehub/src/server/symbol_admission.rs`'s `SYMBOL_RULES`, a shared module this
//! crate does not edit). The door does not call it itself (the tradehub links no bridge and opens no
//! venue socket): it asks the gatewayhub, over the account's trade lease, and the gatewayhub's
//! `SymbolFacts::facts_by_name` calls [`canonical_polymarket_symbol`] and answers the facts of the token.
//!
//! The reads are Gamma's keyless `GET /markets?slug=` and `GET /events?slug=` (live-verified field
//! shapes: `crate::gamma`'s module doc), through the same proxy-aware agent as every other read.

use crate::gamma::{GammaClient, GammaMarket};

/// Does `s` look like an ERC-1155 outcome `token_id` (a long decimal uint256)? The ONE predicate for
/// "this symbol is already canonical" — `crate::exec_plane::adapter` asks it before it spends a
/// round-trip on a seed, and [`parse_market_ref`] before it reads anything.
pub(crate) fn is_token_id(s: &str) -> bool {
    s.len() >= 20 && s.bytes().all(|b| b.is_ascii_digit())
}

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

/// The spellings a refusal offers, once, so every message says the same thing.
const SPELLINGS: &str = "an outcome token id (a 20+ digit number), a market slug with its outcome \
                         (`will-it-happen:Yes`), or the market's polymarket.com address";

/// A slug is lower-case words joined by `-` (digits allowed). It must hold a LETTER: a short run of
/// digits is a mistyped token id, never a market, and must not cost a Gamma request.
fn slug_shaped(s: &str) -> bool {
    s.bytes().next().is_some_and(|b| b.is_ascii_alphanumeric())
        && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
        && s.bytes().any(|b| b.is_ascii_alphabetic())
}

/// Classify what the operator typed. A refusal carries the spellings that would work.
pub fn parse_market_ref(input: &str) -> Result<MarketRef, String> {
    let s = input.trim();
    if s.is_empty() {
        return Err(format!("polymarket: no market was named; give {SPELLINGS}"));
    }
    if is_token_id(s) {
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
        return Err(format!(
            "polymarket: `{s}` is a condition id, which names a MARKET and not an outcome; give \
             {SPELLINGS}"
        ));
    }
    let (slug, outcome) = match s.split_once(':') {
        Some((slug, outcome)) => (slug.trim(), Some(outcome.trim())),
        None => (s, None),
    };
    if !slug_shaped(slug) || outcome.is_some_and(str::is_empty) {
        return Err(format!(
            "polymarket: `{s}` is not a name this venue's orders take; give {SPELLINGS}"
        ));
    }
    Ok(MarketRef::Market { slug: slug.to_ascii_lowercase(), outcome: outcome.map(str::to_string) })
}

/// `https://polymarket.com/event/<event>/<market>` → the market; `…/event/<event>` → the event;
/// `…/market/<market>` → the market. Anything after `?` or `#` (tracking parameters) is dropped.
fn parse_url(url: &str) -> Result<MarketRef, String> {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    let (host, path) = rest.split_once('/').unwrap_or((rest, ""));
    let host = host.to_ascii_lowercase();
    if host != "polymarket.com" && host != "www.polymarket.com" {
        return Err(format!(
            "polymarket: `{url}` is not a polymarket.com address; give {SPELLINGS}"
        ));
    }
    let path = path.split(['?', '#']).next().unwrap_or("");
    let segs: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    let bad = |why: &str| {
        Err(format!(
            "polymarket: `{url}` {why}; the address of one market looks like \
             https://polymarket.com/event/<event>/<market>"
        ))
    };
    let (kind, slug, market) = match segs.as_slice() {
        ["event", event] => ("event", *event, None),
        ["event", _event, market] => ("event", *market, Some(*market)),
        ["market", market] => ("market", *market, Some(*market)),
        _ => return bad("is not a market or event page"),
    };
    if !slug_shaped(slug) {
        return bad("has a segment that is not a slug");
    }
    let slug = slug.to_ascii_lowercase();
    Ok(match (kind, market) {
        (_, Some(_)) => MarketRef::Market { slug, outcome: None },
        _ => MarketRef::Event { slug },
    })
}

/// The Gamma reads this module needs, as a seam: the real one is [`Gamma`], a test brings its own.
pub trait MarketDirectory {
    /// One market by its exact slug, closed ones included (`Ok(None)` = Gamma lists no such slug).
    fn market(&self, slug: &str) -> Result<Option<GammaMarket>, String>;
    /// Every market of one event by the event's exact slug (`Ok(vec![])` = no such event).
    fn event_markets(&self, slug: &str) -> Result<Vec<GammaMarket>, String>;
}

/// The live directory: Gamma's keyless `/markets?slug=` and `/events?slug=`. `/markets` hides a
/// RESOLVED market unless asked for one (measured live, `GammaClient::closed_by_slug`), so a miss is
/// asked again with `closed=true`: a resolved market is then refused as CLOSED, not reported as
/// one that does not exist. The second request is paid on a miss only.
pub struct Gamma;

impl MarketDirectory for Gamma {
    fn market(&self, slug: &str) -> Result<Option<GammaMarket>, String> {
        match GammaClient::by_slug(slug, false)? {
            Some(m) => Ok(Some(m)),
            None => GammaClient::closed_by_slug(slug),
        }
    }
    fn event_markets(&self, slug: &str) -> Result<Vec<GammaMarket>, String> {
        GammaClient::markets_by_event_slug(slug, false)
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
fn outcome_tokens(m: &GammaMarket) -> Result<Vec<(String, String)>, String> {
    if m.closed {
        return Err(format!(
            "polymarket: the market `{}` is CLOSED (resolved), so none of its tokens can be \
             traded any more",
            m.slug
        ));
    }
    if !m.active {
        return Err(format!(
            "polymarket: the market `{}` is not active on Gamma right now, so its tokens cannot \
             be traded",
            m.slug
        ));
    }
    if m.accepting_orders == Some(false) {
        return Err(format!(
            "polymarket: the market `{}` is open but Gamma says it is not accepting orders right \
             now (`acceptingOrders` is false), so an order on its tokens would be refused",
            m.slug
        ));
    }
    if m.token_ids.is_empty() || m.token_ids.len() != m.outcomes.len() {
        return Err(format!(
            "polymarket: Gamma lists {} outcomes and {} token ids for `{}`, which cannot be \
             matched one to one, so no outcome is named",
            m.outcomes.len(),
            m.token_ids.len(),
            m.slug
        ));
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
fn choose_outcome_words(m: &GammaMarket, options: &[(String, String)]) -> String {
    let spelled: Vec<String> = options
        .iter()
        .take(LISTED)
        .map(|(label, id)| format!("`{}:{label}` (token {id})", m.slug))
        .collect();
    let more = options.len().saturating_sub(LISTED);
    format!(
        "polymarket: the market `{}` («{}») has {} outcomes and an order buys or sells ONE of \
         them, so name which: {}{}",
        m.slug,
        m.question,
        options.len(),
        spelled.join(", "),
        if more > 0 { format!(", and {more} more") } else { String::new() }
    )
}

fn pick_outcome(m: &GammaMarket, outcome: Option<&str>) -> Result<ResolvedOutcome, String> {
    let options = outcome_tokens(m)?;
    let Some(want) = outcome else { return Err(choose_outcome_words(m, &options)) };
    match options.iter().find(|(label, _)| label.eq_ignore_ascii_case(want)) {
        Some((label, id)) => Ok(resolved(m, label.clone(), id.clone())),
        None => Err(format!(
            "polymarket: the market `{}` has no outcome called `{want}`. {}",
            m.slug,
            choose_outcome_words(m, &options)
        )),
    }
}

/// Resolve what the operator typed to ONE outcome. A pasted token id answers itself with no read at
/// all ([`canonical_symbol`] is the cheaper door for that); a slug or an address costs one Gamma
/// request.
pub fn resolve_market_ref(
    input: &str,
    directory: &dyn MarketDirectory,
) -> Result<ResolvedOutcome, String> {
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
            let m = directory.market(&slug)?.ok_or_else(|| not_listed(&slug))?;
            pick_outcome(&m, outcome.as_deref())
        }
        MarketRef::Event { slug } => {
            let markets = directory.event_markets(&slug)?;
            let open: Vec<&GammaMarket> =
                markets.iter().filter(|m| m.active && !m.closed).collect();
            match open.as_slice() {
                [] if markets.is_empty() => Err(format!(
                    "polymarket: Gamma lists no event `{slug}`; check the address, or give \
                     {SPELLINGS}"
                )),
                [] => Err(format!(
                    "polymarket: every market of the event `{slug}` is closed or inactive, so \
                     nothing in it can be traded"
                )),
                [only] => pick_outcome(only, None),
                many => {
                    let names: Vec<String> = many
                        .iter()
                        .take(LISTED)
                        .map(|m| format!("`{}` («{}»)", m.slug, m.question))
                        .collect();
                    Err(format!(
                        "polymarket: the event `{slug}` holds {} open markets, so name one \
                         market (then its outcome, `<market>:Yes`): {}{}",
                        many.len(),
                        names.join(", "),
                        match many.len().saturating_sub(LISTED) {
                            0 => String::new(),
                            n => format!(", and {n} more"),
                        }
                    ))
                }
            }
        }
    }
}

fn not_listed(slug: &str) -> String {
    format!(
        "polymarket: Gamma lists no market with the slug `{slug}`; check the spelling (a slug is \
         the last part of the market's polymarket.com address), or give {SPELLINGS}"
    )
}

/// The canonical symbol — the outcome's token id — for what the operator typed. A pasted token id
/// is returned UNCHANGED with no network read; every other spelling costs one Gamma request.
pub fn canonical_symbol(input: &str, directory: &dyn MarketDirectory) -> Result<String, String> {
    match parse_market_ref(input)? {
        MarketRef::Token(token_id) => Ok(token_id),
        _ => resolve_market_ref(input, directory).map(|r| r.token_id),
    }
}

/// [`canonical_symbol`] over live Gamma — the function a door that admits a symbol calls.
pub fn canonical_polymarket_symbol(input: &str) -> Result<String, String> {
    canonical_symbol(input, &Gamma)
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
