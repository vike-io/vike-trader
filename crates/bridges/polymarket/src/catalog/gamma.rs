//! `gamma` — Polymarket Gamma read/discovery API: market catalog fetch + parse
//! (docs/superpowers/specs/2026-07-11-gamma-catalog-design.md). Gamma is the ONLY first-party
//! source of market titles/volume/resolution/slugs + the outcome token_ids; the CLOB /markets
//! endpoint (instrument/properties.rs) is trade-metadata only. Token-id field CONFIRMED (Step 0, live
//! `GET https://gamma-api.polymarket.com/markets?limit=1&closed=false` via arbdub, 2026-07-11):
//! `clobTokenIds` — a JSON-ENCODED STRING array, like `outcomes`/`outcomePrices` (double-decode,
//! see `decode_json_string_array`). Example value observed:
//! `"[\"98022490269692409998126496127597032490334070080325855126491859374983463996227\", \"53831553061883006530739877284105938919721408776239639687877978808906551086026\"]"`.
//! Geo-blocked → every `GammaClient` read routes via `transport::agent()`'s proxy. Every market read
//! is Gamma's keyset route (`GammaClient::keyset_page`'s doc carries why).
//!
//! ## NEG-RISK GROUP IDENTITY — field names VERIFIED LIVE (2026-07-22, via arbdub)
//! Read from `GET https://gamma-api.polymarket.com/markets?limit=500&closed=false&active=true`
//! and `GET .../events?limit=3&closed=false&active=true`. What the wire actually carries:
//!
//! - **`negRiskMarketID`** (bytes32 hex) — **THE group key.** Every member of one neg-risk event
//!   carries the SAME value, and it equals the parent `events[0].negRiskMarketID`. Present ONLY on
//!   neg-risk markets. This is also the on-chain `NegRiskAdapter` `_marketId` (see
//!   [`crate::settlement::split_merge::convert_positions_calldata`]).
//! - **`negRiskRequestID`** (bytes32 hex) — **PER-MARKET, NOT a group key.** Verified: a 7-member
//!   group had 7 DISTINCT `negRiskRequestID`s, a 34-member group had 34. Parsed here for
//!   completeness, but grouping on it would put every member in its own singleton set.
//! - **`questionID`** (bytes32 hex) — `negRiskMarketID` with its LAST BYTE replaced by the member's
//!   0-based outcome index. Verified over the complete 128-member
//!   `democratic-presidential-nominee-2028` event: indices `0x00..0x7f`, contiguous, one per market.
//! - **`groupItemThreshold`** (a decimal STRING) — despite the name this is the member's outcome
//!   **index**, byte-identical to `questionID`'s last byte for all 128 members of that event
//!   (`"0"`..`"127"`). It is NOT a threshold and NOT a group key; the previous code's use of its
//!   mere PRESENCE as a neg-risk signal is preserved (see below) but it now also yields the index.
//! - **`groupItemTitle`** — the member's short label within the group (`"Gedion Timothewos"`),
//!   distinct from the full `question` (`"Will Gedion Timothewos be the next PM of Ethiopia?"`).
//! - **`events`** — an array (observed length 1) of the parent event: `id`, `slug`, `ticker`,
//!   `title`, `negRisk`, `negRiskMarketID`. The `/events` endpoint carries the mirror relation
//!   (`markets[]` nested inside each event) and its nested markets have NO `events` key — see
//!   [`parse_gamma_events`].
//!
//! Real observed example (one member of the 7-way "Next Prime Minister of Ethiopia" set):
//! ```json
//! { "id": "2063135",
//!   "question": "Will Gedion Timothewos be the next Prime Minister of Ethiopia?",
//!   "conditionId": "0xb6d6f15a1b5d08753653f1867ccd6126badfbe182a75159a330dc7b15336b309",
//!   "questionID":  "0x55ab76d092f682bf5cbb7e14f13ee12f8410ce7cc1b7906f23b8fb56c11f6507",
//!   "negRisk": true, "negRiskOther": false,
//!   "negRiskMarketID":  "0x55ab76d092f682bf5cbb7e14f13ee12f8410ce7cc1b7906f23b8fb56c11f6500",
//!   "negRiskRequestID": "0x1291bf079fc5e94bc2e72e790495aedb8e755c0b2a8fce3b60f01284c3f5ae67",
//!   "groupItemTitle": "Gedion Timothewos", "groupItemThreshold": "7",
//!   "outcomes": "[\"Yes\", \"No\"]", "outcomePrices": "[\"0.0185\", \"0.9815\"]",
//!   "events": [{ "id": "411239", "slug": "next-prime-minister-of-ethiopia",
//!                "title": "Next Prime Minister of Ethiopia", "negRisk": true,
//!                "negRiskMarketID": "0x55ab76d0…6500" }] }
//! ```
//! (note `questionID` ends `…07` = index 7 = `groupItemThreshold`, while `negRiskMarketID`
//! ends `…00` — the group's index-0 slot.)
//!
//! ### The index encoding is not just observed — it is the contract's own rule
//! `NegRiskIdLib` (<https://github.com/Polymarket/neg-risk-ctf-adapter/blob/main/src/libraries/NegRiskIdLib.sol>)
//! states it verbatim: *"MarketIds are the keccak256 hash of the oracle, feeBips, and metadata,
//! **with the final 8 bits set to 0**"* and *"QuestionIds share the first 31 bytes with their
//! corresponding MarketId, and **the final byte consists of the questionIndex**"* —
//! ```solidity
//! bytes32 private constant MASK = bytes32(type(uint256).max) << 8;
//! function getMarketId(bytes32 _questionId)                    { return _questionId & MASK; }
//! function getQuestionId(bytes32 _marketId, uint8 _questionIndex) { return bytes32(uint256(_marketId) + _questionIndex); }
//! function getQuestionIndex(bytes32 _questionId) returns (uint8) { return uint8(uint256(_questionId)); }
//! ```
//! That is exactly what [`neg_risk_question_id`] implements, and it explains why every observed
//! `negRiskMarketID` ends in `00`. Note the index is a **`uint8`** on chain — so a set can hold at
//! most 256 outcomes, which is why this module refuses an index `> 255` rather than wrapping.
//!
//! ⚠ **A volume-ordered `/markets` page yields INCOMPLETE sets.** Members of one event are ranked
//! independently, so a bounded browse cuts groups in half. The Σ-price invariant is only meaningful
//! over a COMPLETE set — which is why [`crate::catalog::neg_risk_set::NegRiskSet`] carries an explicit
//! completeness signal and why [`GammaClient::markets_by_event_slug`] exists.

use crate::instrument::rewards::RewardsConfig;
use serde_json::Value;
use vike_venues::venue_urls::{UrlRefusal, VenueUrls};

/// One market from the Gamma catalog (the discovery/metadata view). `token_ids` are the outcome
/// CLOB token ids to hand to `subscribe_book`/`subscribe_quotes`/`subscribe_trades`.
///
/// The `neg_risk_*` / `group_item_*` / `event_*` fields are the neg-risk GROUP identity — see this
/// module's doc for the live-verified wire shape. They are all empty/`None` on a non-neg-risk
/// market; `neg_risk_market_id` being non-empty is the reliable "this market belongs to a set"
/// test (`neg_risk` alone is also set by the legacy `groupItemThreshold`-presence heuristic).
#[derive(Debug, Clone, Default)]
pub struct GammaMarket {
    pub id: String,
    pub question: String,
    pub condition_id: String,
    pub slug: String,
    pub end_date: String,
    pub volume: f64,
    pub liquidity: f64,
    pub active: bool,
    pub closed: bool,
    /// `acceptingOrders` EXACTLY as Gamma sent it (`None` when the key is absent or not a bool). A
    /// market can be `active` and still not take orders: a RESOLVED one reads `active: true`,
    /// `closed: true`, `acceptingOrders: false` (measured live 2026-10-26), and a paused one reads
    /// `closed: false` with `acceptingOrders: false`.
    pub accepting_orders: Option<bool>,
    pub neg_risk: bool,
    /// `negRisk` EXACTLY as Gamma sent it: `None` when the key is absent (or not a bool). Unlike
    /// [`GammaMarket::neg_risk`] — which folds absent to `false` and is also raised by the legacy
    /// `groupItemThreshold`-presence heuristic — this keeps "the venue did not say" apart from
    /// "the venue said no", which is what the catalog's typed `ContractPredictionOutcome::neg_risk` needs.
    pub neg_risk_declared: Option<bool>,
    pub tick_size: f64,
    /// `orderMinSize` — the market's minimum order size in outcome SHARES (live-confirmed on the
    /// Gamma `/markets` payload, see `docs/superpowers/specs/2026-07-11-gamma-catalog-design.md`).
    /// A JSON number, or defensively a decimal string; `None` when absent, unparseable,
    /// non-finite or negative.
    pub order_min_size: Option<f64>,
    pub outcomes: Vec<String>,
    pub token_ids: Vec<String>,
    /// `outcomePrices` — positionally aligned with `outcomes`/`token_ids`. Gamma sends these as a
    /// JSON-encoded string array of decimal STRINGS (`"[\"0.0185\", \"0.9815\"]"`), so they are
    /// double-decoded then parsed; an unparseable element becomes `0.0`. These are Gamma's last
    /// cached marks, NOT a live book — good enough for a set-arb SCREEN, never for execution.
    pub outcome_prices: Vec<f64>,
    /// `questionID` — the per-member on-chain question id; `neg_risk_market_id` with its last byte
    /// set to [`GammaMarket::group_item_index`]. Empty when absent.
    pub question_id: String,
    /// `negRiskMarketID` — THE neg-risk group key (shared by every member of one event), and the
    /// on-chain `NegRiskAdapter` `_marketId`. Empty when the market is not neg-risk.
    pub neg_risk_market_id: String,
    /// `negRiskRequestID` — **per-market, NOT a group key** (verified: N distinct values across an
    /// N-member set). Parsed for completeness only.
    pub neg_risk_request_id: String,
    /// `groupItemTitle` — the member's short label inside the group (e.g. `"Gedion Timothewos"`).
    pub group_item_title: String,
    /// The member's 0-based outcome index within the neg-risk set, from `groupItemThreshold`
    /// (a decimal string) and cross-checked against `questionID`'s last byte. `None` when absent
    /// or unparseable.
    pub group_item_index: Option<u32>,
    /// `events[0].id` — the parent Gamma event (a grouping that also exists for non-neg-risk
    /// markets, so this is NOT a neg-risk key).
    pub event_id: String,
    /// `events[0].slug` — the human-facing event slug (`"next-prime-minister-of-ethiopia"`).
    pub event_slug: String,
    /// `events[0].title` — the event's display title (`"Next Prime Minister of Ethiopia"`).
    pub event_title: String,
    /// The market's liquidity-rewards config, parsed from Gamma's flat `rewardsMinSize` /
    /// `rewardsMaxSpread` (see [`crate::instrument::rewards::RewardsConfig`]). ADDITIVE: a non-rewarded market —
    /// or any market before this field existed — is [`RewardsConfig::default`] (all-zero, inert).
    /// Gamma carries neither the daily rate nor `moas`; a consumer that needs those enriches this
    /// off the CLOB endpoints via [`RewardsConfig::from_clob_rewards`] / [`RewardsConfig::from_compact`].
    pub rewards: RewardsConfig,
    /// `description` — the market's RULES as the venue states them (how and on what it resolves). Empty
    /// when absent. Kept for the text history (`crate::catalog::rules`), read by nothing that trades.
    pub description: String,
    /// `resolutionSource` — the source the rules name for the resolution (often empty: the rules then
    /// name it in `description`).
    pub resolution_source: String,
    /// `startDate` — when the market opened, as Gamma spells it (`crate::catalog::rules`' clock reads it).
    pub start_date: String,
    /// `createdAt` — when the market was created, as Gamma spells it; the start's stand-in when
    /// `startDate` is absent.
    pub created_at: String,
    /// `closedTime` — when the market closed, as Gamma spells it (`2024-11-06 15:43:47+00`); empty
    /// while it is open.
    pub closed_time: String,
    /// `umaEndDate` — when UMA settled the market's outcome, as Gamma spells it (`2025-03-14T13:53:31Z`);
    /// empty when absent (`crate::catalog::rules`' outcome clock reads it).
    pub uma_end_date: String,
}

impl GammaMarket {
    /// Parse [`GammaMarket::end_date`] (RFC 3339, e.g. `"2026-07-31T12:00:00Z"`) to epoch-ms —
    /// the Avellaneda–Stoikov `resolution_ts` for a maker started on this market (`HorizonMode::
    /// TimeToResolution`), through the one shared parser (`vike_utility::calendar::parse_rfc3339_ms`): a
    /// zone is applied, a stamp with none is read as UTC (Gamma's dates are UTC), a fraction is floored to
    /// the millisecond. Returns `None` when `end_date` is absent or unparseable.
    pub fn resolution_ts_ms(&self) -> Option<i64> {
        use vike_utility::calendar::{MissingZone, parse_rfc3339_ms};
        parse_rfc3339_ms(&self.end_date, MissingZone::ReadAsUtc)
    }

    /// Is this market a member of a neg-risk SET? True iff it carries the group key
    /// `negRiskMarketID`. Deliberately stricter than the `neg_risk` bool, which is also raised by
    /// the legacy `groupItemThreshold`-presence heuristic and so can be `true` with no group key to
    /// group ON.
    pub fn is_neg_risk_member(&self) -> bool {
        !self.neg_risk_market_id.is_empty()
    }

    /// The YES leg's cached Gamma price, i.e. `outcome_prices[0]` — the term this market
    /// contributes to a set's Σ-price invariant. `None` when Gamma sent no prices (an inactive
    /// member, which is common: 77 of 128 members of the observed 2028-nominee event had none).
    pub fn yes_price(&self) -> Option<f64> {
        self.outcome_prices.first().copied()
    }

    /// The YES leg's CLOB token id, i.e. `token_ids[0]` — the tradable id for the set-arb leg.
    /// Gamma's outcome ordering is `["Yes", "No"]` for every neg-risk member observed.
    pub fn yes_token_id(&self) -> Option<&str> {
        self.token_ids.first().map(String::as_str)
    }

    /// The NO leg's CLOB token id, i.e. `token_ids[1]` — the leg `convertPositions` consumes.
    pub fn no_token_id(&self) -> Option<&str> {
        self.token_ids.get(1).map(String::as_str)
    }
}

/// Derive a neg-risk member's `questionID` from its group key + 0-based index: the group key's last
/// byte is REPLACED by the index. Verified live against all 128 members of the
/// `democratic-presidential-nominee-2028` event (indices `0x00`..`0x7f`), and against the 7-member
/// Ethiopia set. `None` on a malformed key or an index that does not fit one byte (`> 255`).
///
/// This exists as a CROSS-CHECK, not as a substitute for the wire value: `parse_gamma_markets`
/// always prefers the `questionID` Gamma actually sent.
pub fn neg_risk_question_id(neg_risk_market_id: &str, index: u32) -> Option<String> {
    if index > 0xff {
        return None;
    }
    let clean = neg_risk_market_id.strip_prefix("0x").unwrap_or(neg_risk_market_id);
    if clean.len() != 64 || !clean.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    Some(format!("0x{}{:02x}", &clean[..62], index as u8))
}

/// Decode a Gamma JSON-ENCODED-STRING array (e.g. the string `"[\"Yes\",\"No\"]"`) into a
/// `Vec<String>`. THE gotcha: `outcomes`/`outcomePrices`/`clobTokenIds` are strings CONTAINING JSON,
/// not arrays — a naive `as_array()` drops everything. Tolerant: a non-string, non-JSON, or empty
/// value yields `Vec::new()`.
pub fn decode_json_string_array(v: &Value) -> Vec<String> {
    let s = match v.as_str() {
        Some(s) if !s.is_empty() => s,
        _ => return Vec::new(),
    };
    match serde_json::from_str::<Value>(s) {
        Ok(Value::Array(arr)) => arr
            .into_iter()
            .map(|e| match e {
                Value::String(s) => s,
                other => other.to_string(),
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// Decode a Gamma JSON-ENCODED-STRING array of DECIMAL STRINGS (`outcomePrices`, e.g. the string
/// `"[\"0.0185\", \"0.9815\"]"`) into `Vec<f64>`. Same double-decode trap as
/// [`decode_json_string_array`], plus a per-element string→f64 parse; an unparseable element
/// becomes `0.0` rather than dropping the array (positional alignment with `outcomes`/`token_ids`
/// is load-bearing). A bare JSON number is also accepted, in case Gamma ever stops string-wrapping.
pub fn decode_json_string_f64_array(v: &Value) -> Vec<f64> {
    decode_json_string_array(v).iter().map(|s| s.parse::<f64>().unwrap_or(0.0)).collect()
}

/// Read the member's 0-based neg-risk outcome index. `groupItemThreshold` is a decimal STRING on
/// the wire (`"7"`), but a JSON number is also accepted defensively.
fn group_item_index(e: &Value) -> Option<u32> {
    let v = e.get("groupItemThreshold")?;
    match v {
        Value::String(s) => s.trim().parse::<u32>().ok(),
        other => other.as_u64().map(|n| n as u32),
    }
}

/// Read `orderMinSize` (outcome shares). A JSON number on the wire; a decimal string is accepted
/// defensively. `None` when absent, unparseable, non-finite or negative.
fn order_min_size(e: &Value) -> Option<f64> {
    let n = match e.get("orderMinSize")? {
        Value::String(s) => s.trim().parse::<f64>().ok()?,
        other => other.as_f64()?,
    };
    (n.is_finite() && n >= 0.0).then_some(n)
}

/// Parse a Gamma `/markets` JSON array into [`GammaMarket`]s. Tolerant: an entry missing
/// `question`/`conditionId` is skipped. `neg_risk` derives from `negRisk` or a present
/// `groupItemThreshold`. The JSON-string array fields are double-decoded via
/// [`decode_json_string_array`] / [`decode_json_string_f64_array`].
///
/// ADDITIVE (neg-risk sets): also reads the group identity — `questionID`, `negRiskMarketID`,
/// `negRiskRequestID`, `groupItemTitle`, `groupItemThreshold` (as the outcome INDEX) and the
/// parent `events[0]` — see this module's doc for the live-verified shape. Every one of those is
/// optional, so a market that carries none parses exactly as before.
pub fn parse_gamma_markets(json: &Value) -> Vec<GammaMarket> {
    let arr = match json.as_array() {
        Some(a) => a,
        None => return Vec::new(),
    };
    let mut out = Vec::new();
    for e in arr {
        let question = e.get("question").and_then(Value::as_str);
        let condition_id = e.get("conditionId").and_then(Value::as_str);
        let (Some(question), Some(condition_id)) = (question, condition_id) else { continue };
        let neg_risk = e.get("negRisk").and_then(Value::as_bool).unwrap_or(false)
            || e.get("groupItemThreshold").is_some();
        // `events` is an array (observed length 1); the parent event's identity is lifted onto each
        // member so a flat market list still knows which event it came from.
        let event = e.get("events").and_then(Value::as_array).and_then(|a| a.first());
        let event_str = |k: &str| {
            event.and_then(|o| o.get(k)).and_then(Value::as_str).unwrap_or("").to_string()
        };
        let text = |k: &str| e.get(k).and_then(Value::as_str).unwrap_or("").to_string();
        out.push(GammaMarket {
            id: e.get("id").and_then(Value::as_str).unwrap_or("").to_string(),
            question: question.to_string(),
            condition_id: condition_id.to_string(),
            slug: e.get("slug").and_then(Value::as_str).unwrap_or("").to_string(),
            end_date: e.get("endDate").and_then(Value::as_str).unwrap_or("").to_string(),
            volume: e.get("volumeNum").and_then(Value::as_f64).unwrap_or(0.0),
            liquidity: e.get("liquidityNum").and_then(Value::as_f64).unwrap_or(0.0),
            active: e.get("active").and_then(Value::as_bool).unwrap_or(false),
            closed: e.get("closed").and_then(Value::as_bool).unwrap_or(false),
            accepting_orders: e.get("acceptingOrders").and_then(Value::as_bool),
            neg_risk,
            neg_risk_declared: e.get("negRisk").and_then(Value::as_bool),
            tick_size: e.get("orderPriceMinTickSize").and_then(Value::as_f64).unwrap_or(0.0),
            order_min_size: order_min_size(e),
            outcomes: e.get("outcomes").map(decode_json_string_array).unwrap_or_default(),
            token_ids: e.get("clobTokenIds").map(decode_json_string_array).unwrap_or_default(),
            outcome_prices: e
                .get("outcomePrices")
                .map(decode_json_string_f64_array)
                .unwrap_or_default(),
            question_id: e.get("questionID").and_then(Value::as_str).unwrap_or("").to_string(),
            neg_risk_market_id: e
                .get("negRiskMarketID")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            neg_risk_request_id: e
                .get("negRiskRequestID")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            group_item_title: e
                .get("groupItemTitle")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            group_item_index: group_item_index(e),
            event_id: event_str("id"),
            event_slug: event_str("slug"),
            event_title: event_str("title"),
            rewards: RewardsConfig::from_gamma_market(e),
            description: text("description"),
            resolution_source: text("resolutionSource"),
            start_date: text("startDate"),
            created_at: text("createdAt"),
            closed_time: text("closedTime"),
            uma_end_date: text("umaEndDate"),
        });
    }
    out
}

/// Parse a Gamma `/events` JSON array into a FLAT [`GammaMarket`] list — every event's nested
/// `markets[]`, with the parent event's `id`/`slug`/`title` injected.
///
/// Why this exists: the nested markets carry `negRiskMarketID` but have **no `events` key** (live-
/// verified), so `parse_gamma_markets` alone would leave `event_*` blank on this path. More
/// importantly, `/events` is the only endpoint that returns a **complete** neg-risk set in one
/// response — the volume-ordered `/markets` browse splits a group across pages, and a Σ-price
/// invariant computed over half a set is meaningless.
///
/// The parent event's `negRiskMarketID` is used as a FALLBACK group key for any nested market that
/// omitted its own; a nested market that carries one keeps it verbatim.
pub fn parse_gamma_events(json: &Value) -> Vec<GammaMarket> {
    let arr = match json.as_array() {
        Some(a) => a,
        None => return Vec::new(),
    };
    let mut out = Vec::new();
    for event in arr {
        let id = event.get("id").and_then(Value::as_str).unwrap_or("").to_string();
        let slug = event.get("slug").and_then(Value::as_str).unwrap_or("").to_string();
        let title = event.get("title").and_then(Value::as_str).unwrap_or("").to_string();
        let event_nrm =
            event.get("negRiskMarketID").and_then(Value::as_str).unwrap_or("").to_string();
        let Some(markets) = event.get("markets") else { continue };
        for mut m in parse_gamma_markets(markets) {
            if m.event_id.is_empty() {
                m.event_id = id.clone();
            }
            if m.event_slug.is_empty() {
                m.event_slug = slug.clone();
            }
            if m.event_title.is_empty() {
                m.event_title = title.clone();
            }
            if m.neg_risk_market_id.is_empty() {
                m.neg_risk_market_id = event_nrm.clone();
            }
            out.push(m);
        }
    }
    out
}

/// Build the `/markets/keyset?slug=…` query string — Gamma's exact-slug lookup, which returns the
/// single matching market regardless of its volume rank.
///
/// This is why it exists: the paged browse ([`gamma_keyset_query`]) is ordered `volumeNum` DESC, so a
/// just-listed window of a recurring series (~$0 volume) sorts BELOW any bounded crawl ceiling and
/// can never be found by scanning. `slug=` is O(1) and volume-independent. `active_only` is applied
/// on top so a closed market is not returned to the tradeable path.
///
/// The same string is the `/events/keyset?slug=…` query, the exact-slug EVENT lookup, whose response
/// nests the event's COMPLETE `markets[]`. This is the only way to fetch a whole neg-risk set in one
/// request (the volume-ordered browse splits groups across pages); `active_only` keeps a closed event
/// off the tradeable path there too.
///
/// MEASURED (the latency box, 2026-11-17): the keyset routes answer every lookup of this module exactly as the
/// deprecated `/markets?…` / `/events?…` lists did (the same market or event, value for value; a closed
/// market hidden unless `closed=true` is asked; nothing found is `{"markets": []}` / `{"events": []}`),
/// while every list answer carries `deprecation: true` and `Warning: 299 - "use /markets/keyset"`
/// (`"use /events/keyset"`). `/markets/slug/{slug}` is not the same lookup: it answers a closed market
/// too, and HTTP 404 for an unknown slug.
pub(crate) fn gamma_slug_query(slug: &str, active_only: bool) -> String {
    let mut q = String::new();
    if active_only {
        q.push_str("active=true&closed=false&");
    }
    q.push_str("slug=");
    q.push_str(&vike_utility::percent::percent_encode(slug));
    q
}

/// Build the `/markets/keyset?closed=true&slug=…` query string — the exact-slug lookup over RESOLVED
/// markets.
///
/// ⚠ MEASURED live 2026-10-26 (a the latency box lane, `will-donald-trump-win-the-2024-us-presidential-election`)
/// on the list route, and again 2026-11-17 on the keyset route: `slug=<closed market>` answers no
/// market — Gamma hides a closed market unless it is asked for one — while `closed=true&slug=<same>`
/// answers the market with `closed: true`. The event lookup is not like that: it answers a closed
/// event either way. So [`gamma_slug_query`] alone cannot tell a resolved market from a slug that
/// never existed.
pub(crate) fn gamma_closed_slug_query(slug: &str) -> String {
    format!("closed=true&slug={}", vike_utility::percent::percent_encode(slug))
}

/// The most slugs one market query may name, and the page it asks for them. MEASURED (the latency box, 2026-11-19,
/// `/markets/keyset?closed=true&limit=100&slug=…` over `btc-updown-5m-…`): 100 slugs answer the 100 closed
/// markets on one page, 101 answer HTTP 422 `expected array length <= 100`.
pub(crate) const GAMMA_SLUGS_PER_QUERY: usize = 100;

/// Build the `/markets/keyset?closed=true&limit=100&slug=…&slug=…` query: the CLOSED markets among `slugs`
/// (at most [`GAMMA_SLUGS_PER_QUERY`]), one page large enough for all of them. Like
/// [`gamma_closed_slug_query`], without `closed=true` Gamma hides a closed market.
pub(crate) fn gamma_closed_slugs_query(slugs: &[String]) -> String {
    let mut query = format!("closed=true&limit={GAMMA_SLUGS_PER_QUERY}");
    for slug in slugs {
        query.push_str("&slug=");
        query.push_str(&vike_utility::percent::percent_encode(slug));
    }
    query
}

/// Build the `/markets/keyset?clob_token_ids=…` query string — Gamma's exact outcome-token lookup, which
/// answers the ONE market that lists the token (live-verified 2026-07-23 on the list route,
/// `crate::instrument::taker_hold`'s `fetch_condition_id_for_token`, and 2026-11-17 on the keyset
/// route). Like [`gamma_slug_query`] it hides a RESOLVED market (no market).
pub(crate) fn gamma_token_query(token_id: &str) -> String {
    format!("clob_token_ids={}", vike_utility::percent::percent_encode(token_id))
}

/// Build the `/markets/keyset` query of the CLOSED markets whose scheduled end (`endDate`) falls in
/// `[start_ms, end_ms]` (`end_date_min` / `end_date_max`, second resolution), `limit` asked a page; the
/// page after the first is named by its cursor ([`GammaClient::keyset_page`]), so the venue keeps the
/// position and no market is skipped or read twice. The window bounds are as Gamma documents them and
/// were measured to hold on both routes (`crate::catalog::rules`' module doc), which still re-checks
/// every market's own `endDate` against the window.
pub(crate) fn gamma_closed_ending_query(start_ms: i64, end_ms: i64, limit: usize) -> String {
    let at = |ms: i64| {
        vike_utility::percent::percent_encode(&vike_model::time::epoch_ms_to_utc_timestamp(ms))
    };
    format!("closed=true&end_date_min={}&end_date_max={}&limit={limit}", at(start_ms), at(end_ms))
}

/// Build the `/markets/keyset` query of the volume-ordered browse: `active_only` filters to
/// `active=true&closed=false`, ordered by `volumeNum` descending, `limit` asked a page; the page after
/// the first is named by its cursor ([`GammaClient::keyset_page`]).
pub(crate) fn gamma_keyset_query(active_only: bool, limit: usize) -> String {
    let filter = if active_only { "active=true&closed=false&" } else { "" };
    format!("{filter}limit={limit}&order=volumeNum&ascending=false")
}

/// The list under `key` of a keyset answer (`{"markets": [...]}`, `{"events": [...]}`). An answer with
/// no such list (a refusal object, or the deprecated route's bare array) is an error, never "nothing
/// listed".
fn keyset_list<'a>(json: &'a Value, key: &str) -> Result<&'a Value, String> {
    json.get(key).filter(|list| list.is_array()).ok_or_else(|| {
        format!("gamma's keyset route answered no `{key}` list; it is not read as an empty one")
    })
}

/// One `/markets/keyset` answer (`{"markets": [...], "next_cursor": "..."}`): the markets parsed via
/// [`parse_gamma_markets`] and the cursor of the page after it, `None` where Gamma names none (null,
/// absent or empty: the end of the listing, MEASURED on the last page of a day). An answer with no
/// `markets` list (a refusal object) is an error, never an empty page.
pub(crate) fn parse_keyset_page(
    json: &Value,
) -> Result<(Vec<GammaMarket>, Option<String>), String> {
    let markets = keyset_list(json, "markets")?;
    let next = json.get("next_cursor").and_then(Value::as_str).filter(|c| !c.is_empty());
    Ok((parse_gamma_markets(markets), next.map(str::to_string)))
}

/// Read-only Gamma catalog client over the venue's Gamma row. Geo-blocked → the GET routes through
/// `transport::agent()`'s proxy (arbdub) exactly like the other Polymarket reads.
#[derive(Debug, Clone)]
pub struct GammaClient {
    /// The Gamma base; the crate's exec thread builds one from a base it was handed.
    pub(crate) base: String,
}

impl GammaClient {
    /// The client of the Gamma row among `urls` (`live data rest gamma`, `crate::config`'s
    /// `gamma_base`). A refusal is the reason nothing is read.
    ///
    /// # Errors
    /// The row is missing ([`UrlRefusal`]).
    pub fn from_urls(urls: &VenueUrls) -> Result<GammaClient, UrlRefusal> {
        Ok(GammaClient { base: crate::config::gamma_base(urls)?.to_string() })
    }

    /// Look the market that lists the outcome `token_id` up (one keyset request, `clob_token_ids=`).
    /// `Ok(None)` = Gamma lists no open market with that token (a resolved market is hidden, see
    /// [`gamma_closed_slug_query`]). A defensive exact check that the returned market really lists the token
    /// guards against Gamma ever treating the filter as a prefix or ignoring it.
    pub fn by_token(&self, token_id: &str) -> Result<Option<GammaMarket>, String> {
        let (markets, _) = self.keyset_page(&gamma_token_query(token_id), "")?;
        Ok(markets.into_iter().find(|m| m.token_ids.iter().any(|t| t == token_id)))
    }

    /// Look one market up by its EXACT slug (one keyset request, volume-independent — see
    /// [`gamma_slug_query`]). `Ok(None)` = not listed. A defensive exact-slug re-check guards against
    /// Gamma ever treating `slug=` as a prefix/fuzzy match.
    pub fn by_slug(&self, slug: &str, active_only: bool) -> Result<Option<GammaMarket>, String> {
        let (markets, _) = self.keyset_page(&gamma_slug_query(slug, active_only), "")?;
        Ok(exact_slug_market(markets, slug))
    }

    /// Look one RESOLVED market up by its exact slug (`closed=true&slug=…`; the query's own doc says
    /// why [`GammaClient::by_slug`] cannot). `Ok(None)` = no such closed market. Same exact-slug
    /// re-check as `by_slug`.
    pub fn closed_by_slug(&self, slug: &str) -> Result<Option<GammaMarket>, String> {
        let (markets, _) = self.keyset_page(&gamma_closed_slug_query(slug), "")?;
        Ok(exact_slug_market(markets, slug))
    }

    /// Fetch one page of `{gamma}/markets/keyset` for `query` ([`gamma_keyset_query`],
    /// [`gamma_closed_ending_query`], the point lookups' queries), the page after `after_cursor`
    /// (`""`: the first): its markets and the next page's cursor, [`parse_keyset_page`].
    ///
    /// ⚠ Every market read of this crate goes through THIS route, never the list `/markets`: MEASURED
    /// (the latency box, 2026-11-17) the list route answers `deprecation: true`, `sunset: Fri, 01 May 2026` and
    /// `Warning: 299 - "use /markets/keyset"` on every query, lists at most 100 a page, and refuses any
    /// offset past 2000 with HTTP 422 (`offset too large, use /markets/keyset for deeper pagination`).
    pub(crate) fn keyset_page(
        &self,
        query: &str,
        after_cursor: &str,
    ) -> Result<(Vec<GammaMarket>, Option<String>), String> {
        let query = if after_cursor.is_empty() {
            query.to_string()
        } else {
            format!("{query}&after_cursor={}", vike_utility::percent::percent_encode(after_cursor))
        };
        parse_keyset_page(&crate::transport::get_json(&self.base, "/markets/keyset", &query)?)
    }

    /// Fetch ONE event by its exact slug (`{gamma}/events/keyset`) and return its nested markets,
    /// flattened via [`parse_gamma_events`] — the COMPLETE member list of a neg-risk set (see
    /// [`gamma_slug_query`] for why the volume browse cannot do this). `Ok(vec![])` = no such event
    /// (`{"events": []}`), or an event with no markets.
    pub fn markets_by_event_slug(
        &self,
        slug: &str,
        active_only: bool,
    ) -> Result<Vec<GammaMarket>, String> {
        let json = crate::transport::get_json(
            &self.base,
            "/events/keyset",
            &gamma_slug_query(slug, active_only),
        )?;
        Ok(parse_gamma_events(keyset_list(&json, "events")?))
    }
}

/// The market among `markets` whose slug is EXACTLY `slug` (any case): a defensive re-check that
/// Gamma never treated `slug=` as a prefix or fuzzy match.
fn exact_slug_market(markets: Vec<GammaMarket>, slug: &str) -> Option<GammaMarket> {
    let want = slug.to_lowercase();
    markets.into_iter().find(|m| m.slug.to_lowercase() == want)
}

#[path = "gamma_tests.rs"]
#[cfg(test)]
mod gamma_tests;
