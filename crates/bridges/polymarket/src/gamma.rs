//! `gamma` — Polymarket Gamma read/discovery API: market catalog fetch + parse
//! (docs/superpowers/specs/2026-07-11-gamma-catalog-design.md). Gamma is the ONLY first-party
//! source of market titles/volume/resolution/slugs + the outcome token_ids; the CLOB /markets
//! endpoint (instruments.rs) is trade-metadata only. Token-id field CONFIRMED (Step 0, live
//! `GET https://gamma-api.polymarket.com/markets?limit=1&closed=false` via arbdub, 2026-07-11):
//! `clobTokenIds` — a JSON-ENCODED STRING array, like `outcomes`/`outcomePrices` (double-decode,
//! see `decode_json_string_array`). Example value observed:
//! `"[\"98022490269692409998126496127597032490334070080325855126491859374983463996227\", \"53831553061883006530739877284105938919721408776239639687877978808906551086026\"]"`.
//! Geo-blocked → `GammaClient::list` routes via `egress::agent()`'s proxy.
//!
//! ## NEG-RISK GROUP IDENTITY — field names VERIFIED LIVE (2026-07-22, via arbdub)
//! Read from `GET https://gamma-api.polymarket.com/markets?limit=500&closed=false&active=true`
//! and `GET .../events?limit=3&closed=false&active=true`. What the wire actually carries:
//!
//! - **`negRiskMarketID`** (bytes32 hex) — **THE group key.** Every member of one neg-risk event
//!   carries the SAME value, and it equals the parent `events[0].negRiskMarketID`. Present ONLY on
//!   neg-risk markets. This is also the on-chain `NegRiskAdapter` `_marketId` (see
//!   [`crate::split_merge::convert_positions_calldata`]).
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
//! over a COMPLETE set — which is why [`crate::neg_risk_set::NegRiskSet`] carries an explicit
//! completeness signal and why [`GammaClient::markets_by_event_slug`] exists.

use crate::rewards::RewardsConfig;
use serde_json::Value;

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
    pub neg_risk: bool,
    pub tick_size: f64,
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
    /// `rewardsMaxSpread` (see [`crate::rewards::RewardsConfig`]). ADDITIVE: a non-rewarded market —
    /// or any market before this field existed — is [`RewardsConfig::default`] (all-zero, inert).
    /// Gamma carries neither the daily rate nor `moas`; a consumer that needs those enriches this
    /// off the CLOB endpoints via [`RewardsConfig::from_clob_rewards`] / [`RewardsConfig::from_compact`].
    pub rewards: RewardsConfig,
}

impl GammaMarket {
    /// Parse [`GammaMarket::end_date`] (RFC3339 UTC, e.g. `"2026-07-31T12:00:00Z"`) to epoch-ms —
    /// the Avellaneda–Stoikov `resolution_ts` for a maker mounted on this market (`HorizonMode::
    /// TimeToResolution`). Additive and dependency-free (a tolerant fixed-field parse, UTC-only:
    /// any fractional-second / offset suffix is treated as UTC, matching Polymarket's `Z` dates).
    /// Returns `None` when `end_date` is absent or unparseable.
    pub fn resolution_ts_ms(&self) -> Option<i64> {
        parse_rfc3339_utc_ms(&self.end_date)
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

/// Tolerant RFC3339-UTC → epoch-ms: expects at least `YYYY-MM-DDTHH:MM:SS` (a space also accepted in
/// place of `T`); any trailing fractional-seconds / timezone suffix is ignored and the value is
/// treated as UTC. `None` on anything malformed. Kept private to this module — the public surface is
/// [`GammaMarket::resolution_ts_ms`].
fn parse_rfc3339_utc_ms(s: &str) -> Option<i64> {
    let s = s.trim();
    let (date, time) = s.split_once('T').or_else(|| s.split_once(' '))?;
    let mut dp = date.split('-');
    let y: i64 = dp.next()?.parse().ok()?;
    let mo: i64 = dp.next()?.parse().ok()?;
    let d: i64 = dp.next()?.parse().ok()?;
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) {
        return None;
    }
    let mut tp = time.split(':');
    let h: i64 = tp.next()?.parse().ok()?;
    let mi: i64 = tp.next()?.parse().ok()?;
    // seconds may carry a `.fff` / `Z` / `+HH:MM` suffix — take the leading integer run only.
    let sec_field = tp.next()?;
    let sec: String = sec_field.chars().take_while(|c| c.is_ascii_digit()).collect();
    let se: i64 = sec.parse().ok()?;
    if !(0..=23).contains(&h) || !(0..=59).contains(&mi) || !(0..=60).contains(&se) {
        return None;
    }
    let days = vike_model::time::days_from_civil(y, mo as u32, d as u32);
    Some((((days * 24 + h) * 60 + mi) * 60 + se) * 1000)
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
        let ev = e.get("events").and_then(Value::as_array).and_then(|a| a.first());
        let ev_str =
            |k: &str| ev.and_then(|o| o.get(k)).and_then(Value::as_str).unwrap_or("").to_string();
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
            neg_risk,
            tick_size: e.get("orderPriceMinTickSize").and_then(Value::as_f64).unwrap_or(0.0),
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
            event_id: ev_str("id"),
            event_slug: ev_str("slug"),
            event_title: ev_str("title"),
            rewards: RewardsConfig::from_gamma_market(e),
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
    for ev in arr {
        let id = ev.get("id").and_then(Value::as_str).unwrap_or("").to_string();
        let slug = ev.get("slug").and_then(Value::as_str).unwrap_or("").to_string();
        let title = ev.get("title").and_then(Value::as_str).unwrap_or("").to_string();
        let ev_nrm = ev.get("negRiskMarketID").and_then(Value::as_str).unwrap_or("").to_string();
        let Some(markets) = ev.get("markets") else { continue };
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
                m.neg_risk_market_id = ev_nrm.clone();
            }
            out.push(m);
        }
    }
    out
}

/// Build the `/markets` query string. `active_only` filters to `active=true&closed=false` (the
/// default browse); ordered by `volumeNum` descending; `limit`/`offset` page.
pub(crate) fn gamma_query(active_only: bool, limit: usize, offset: usize) -> String {
    let mut q = String::new();
    if active_only {
        q.push_str("active=true&closed=false&");
    }
    q.push_str(&format!("limit={limit}&offset={offset}&order=volumeNum&ascending=false"));
    q
}

/// Build the `/markets?slug=…` query string — Gamma's exact-slug lookup, which returns the single
/// matching market regardless of its volume rank.
///
/// This is why it exists: the paged browse ([`gamma_query`]) is ordered `volumeNum` DESC, so a
/// just-listed window of a recurring series (~$0 volume) sorts BELOW any bounded crawl ceiling and
/// can never be found by scanning. `slug=` is O(1) and volume-independent. `active_only` is applied
/// on top so a closed market is not returned to the tradeable path.
pub(crate) fn gamma_slug_query(slug: &str, active_only: bool) -> String {
    let mut q = String::new();
    if active_only {
        q.push_str("active=true&closed=false&");
    }
    q.push_str("slug=");
    q.push_str(&url_encode(slug));
    q
}

/// Build the `/events?slug=…` query string — the exact-slug EVENT lookup, whose response nests the
/// event's COMPLETE `markets[]`. This is the only way to fetch a whole neg-risk set in one request
/// (the volume-ordered `/markets` browse splits groups across pages). `active_only` is applied on
/// top so a closed event is not returned to the tradeable path.
pub(crate) fn gamma_event_slug_query(slug: &str, active_only: bool) -> String {
    let mut q = String::new();
    if active_only {
        q.push_str("active=true&closed=false&");
    }
    q.push_str("slug=");
    q.push_str(&url_encode(slug));
    q
}

/// Percent-encode the characters a Gamma slug could legally carry that would break a query string.
/// Slugs are `[a-z0-9-]` in practice; this is belt-and-braces for a hand-written template.
fn url_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Read-only Gamma catalog client. Geo-blocked → the GET routes through `egress::agent()`'s proxy
/// (arbdub) exactly like the other Polymarket reads.
pub struct GammaClient;

impl GammaClient {
    /// Look one market up by its EXACT slug (one request, volume-independent — see
    /// [`gamma_slug_query`]). `Ok(None)` = not listed. The response is an array like `/markets`,
    /// so the same parser is reused; a defensive exact-slug re-check guards against Gamma ever
    /// treating `slug=` as a prefix/fuzzy match.
    pub fn by_slug(slug: &str, active_only: bool) -> Result<Option<GammaMarket>, String> {
        let json = crate::egress::get_json(
            crate::config::GAMMA_BASE,
            "/markets",
            &gamma_slug_query(slug, active_only),
        )?;
        let want = slug.to_lowercase();
        Ok(parse_gamma_markets(&json).into_iter().find(|m| m.slug.to_lowercase() == want))
    }

    /// Fetch a page of markets from `GAMMA_BASE/markets` (by volume, active+open when
    /// `active_only`), parsed via [`parse_gamma_markets`].
    pub fn list(
        active_only: bool,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<GammaMarket>, String> {
        let json = crate::egress::get_json(
            crate::config::GAMMA_BASE,
            "/markets",
            &gamma_query(active_only, limit, offset),
        )?;
        Ok(parse_gamma_markets(&json))
    }

    /// Fetch ONE event by its exact slug and return its nested markets, flattened via
    /// [`parse_gamma_events`] — the COMPLETE member list of a neg-risk set (see
    /// [`gamma_event_slug_query`] for why the `/markets` browse cannot do this). `Ok(vec![])` =
    /// no such event, or an event with no markets.
    pub fn markets_by_event_slug(
        slug: &str,
        active_only: bool,
    ) -> Result<Vec<GammaMarket>, String> {
        let json = crate::egress::get_json(
            crate::config::GAMMA_BASE,
            "/events",
            &gamma_event_slug_query(slug, active_only),
        )?;
        Ok(parse_gamma_events(&json))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn canned() -> serde_json::Value {
        serde_json::json!([
            {
                "id": "540817",
                "question": "New Rihanna Album before GTA VI?",
                "conditionId": "0x1fad72fae204143ff1c3035e99e7c0f65ea8d5cd9bd1070987bd1a3316f772be",
                "slug": "new-rhianna-album-before-gta-vi-926",
                "endDate": "2026-07-31T12:00:00Z",
                "volumeNum": 854399.71,
                "liquidityNum": 27192.11,
                "active": true,
                "closed": false,
                "orderPriceMinTickSize": 0.01,
                "outcomes": "[\"Yes\", \"No\"]",
                "outcomePrices": "[\"0.505\", \"0.495\"]",
                "clobTokenIds": "[\"111222\", \"333444\"]"
            },
            {
                "id": "999", "question": "grouped neg-risk market", "conditionId": "0xabc",
                "slug": "grp", "endDate": "2026-08-01T00:00:00Z", "volumeNum": 1.0, "liquidityNum": 1.0,
                "active": true, "closed": false, "orderPriceMinTickSize": 0.01,
                "groupItemThreshold": "2", "negRisk": true,
                "outcomes": "[\"A\", \"B\"]", "clobTokenIds": "[\"555\", \"666\"]"
            },
            { "id": "bad", "slug": "no-question-or-condition" },
            {
                "id": "empty-toks", "question": "no tokens", "conditionId": "0xdef", "slug": "e",
                "endDate": "x", "volumeNum": 0.0, "liquidityNum": 0.0, "active": true, "closed": false,
                "orderPriceMinTickSize": 0.01, "outcomes": "[]", "clobTokenIds": ""
            }
        ])
    }

    #[test]
    fn gamma_slug_query_is_an_exact_lookup_not_a_volume_browse() {
        // no order/limit/offset: the by-slug route must not inherit the volume-DESC browse, which
        // is what buries a fresh ~$0-volume window below any crawl ceiling.
        let q = gamma_slug_query("bitcoin-up-or-down-2026-07-18-1205", true);
        assert_eq!(q, "active=true&closed=false&slug=bitcoin-up-or-down-2026-07-18-1205");
        assert!(!q.contains("order=") && !q.contains("limit="));
        assert_eq!(gamma_slug_query("abc", false), "slug=abc");
        // anything outside the unreserved set is percent-encoded rather than injected raw.
        assert_eq!(gamma_slug_query("a b&c=d", false), "slug=a%20b%26c%3Dd");
    }

    #[test]
    fn gamma_query_filters_active_open_by_volume() {
        assert_eq!(
            gamma_query(true, 50, 0),
            "active=true&closed=false&limit=50&offset=0&order=volumeNum&ascending=false"
        );
        // active_only=false drops the active/closed filters (browse everything)
        assert_eq!(
            gamma_query(false, 20, 40),
            "limit=20&offset=40&order=volumeNum&ascending=false"
        );
    }

    #[test]
    fn decode_json_string_array_double_decodes() {
        // the load-bearing gotcha: a STRING containing a JSON array
        let v = serde_json::json!("[\"Yes\", \"No\"]");
        assert_eq!(decode_json_string_array(&v), vec!["Yes".to_string(), "No".to_string()]);
        // garbage / non-string → empty, no panic
        assert!(decode_json_string_array(&serde_json::json!("not json")).is_empty());
        assert!(decode_json_string_array(&serde_json::json!(42)).is_empty());
        assert!(decode_json_string_array(&serde_json::json!("")).is_empty());
    }

    #[test]
    fn parse_reads_markets_and_decodes_token_ids() {
        let ms = parse_gamma_markets(&canned());
        // "bad" (no question/conditionId) is skipped → 3 markets
        assert_eq!(ms.len(), 3);
        let m = &ms[0];
        assert_eq!(m.question, "New Rihanna Album before GTA VI?");
        assert_eq!(
            m.condition_id,
            "0x1fad72fae204143ff1c3035e99e7c0f65ea8d5cd9bd1070987bd1a3316f772be"
        );
        assert_eq!(m.slug, "new-rhianna-album-before-gta-vi-926");
        assert_eq!(m.volume.to_bits(), 854399.71f64.to_bits());
        assert_eq!(m.tick_size.to_bits(), 0.01f64.to_bits());
        assert_eq!(m.outcomes, vec!["Yes".to_string(), "No".to_string()]);
        assert_eq!(m.token_ids, vec!["111222".to_string(), "333444".to_string()]); // decoded from clobTokenIds
        assert!(!m.neg_risk);
        // grouped neg-risk market
        assert!(ms[1].neg_risk);
        // empty clobTokenIds → empty token_ids, no panic
        assert!(ms[2].token_ids.is_empty());
    }

    /// VERBATIM live Gamma shape (2026-07-22, via arbdub) for two members of the 7-way
    /// "Next Prime Minister of Ethiopia" neg-risk set — the fixture the group-identity parse pins
    /// against. Field values are copied from the wire, not invented.
    fn live_neg_risk_pair() -> serde_json::Value {
        serde_json::json!([
            {
                "id": "2063135",
                "question": "Will Gedion Timothewos be the next Prime Minister of Ethiopia?",
                "conditionId": "0xb6d6f15a1b5d08753653f1867ccd6126badfbe182a75159a330dc7b15336b309",
                "questionID": "0x55ab76d092f682bf5cbb7e14f13ee12f8410ce7cc1b7906f23b8fb56c11f6507",
                "slug": "will-gedion-timothewos-be-the-next-prime-minister-of-ethiopia",
                "endDate": "2026-12-31T12:00:00Z",
                "active": true, "closed": false,
                "negRisk": true, "negRiskOther": false,
                "negRiskMarketID": "0x55ab76d092f682bf5cbb7e14f13ee12f8410ce7cc1b7906f23b8fb56c11f6500",
                "negRiskRequestID": "0x1291bf079fc5e94bc2e72e790495aedb8e755c0b2a8fce3b60f01284c3f5ae67",
                "groupItemTitle": "Gedion Timothewos",
                "groupItemThreshold": "7",
                "orderPriceMinTickSize": 0.01,
                "outcomes": "[\"Yes\", \"No\"]",
                "outcomePrices": "[\"0.0185\", \"0.9815\"]",
                "clobTokenIds": "[\"111\", \"222\"]",
                "events": [{
                    "id": "411239", "slug": "next-prime-minister-of-ethiopia",
                    "title": "Next Prime Minister of Ethiopia", "ticker": "next-pm-ethiopia",
                    "negRisk": true,
                    "negRiskMarketID": "0x55ab76d092f682bf5cbb7e14f13ee12f8410ce7cc1b7906f23b8fb56c11f6500"
                }]
            },
            {
                "id": "2063136",
                "question": "Will Belete Molla be the next Prime Minister of Ethiopia?",
                "conditionId": "0x7c97f7315a000000000000000000000000000000000000000000000000000000",
                "questionID": "0x55ab76d092f682bf5cbb7e14f13ee12f8410ce7cc1b7906f23b8fb56c11f6501",
                "slug": "will-belete-molla-be-the-next-prime-minister-of-ethiopia",
                "endDate": "2026-12-31T12:00:00Z",
                "active": true, "closed": false,
                "negRisk": true,
                "negRiskMarketID": "0x55ab76d092f682bf5cbb7e14f13ee12f8410ce7cc1b7906f23b8fb56c11f6500",
                "negRiskRequestID": "0x8f41e93fb2ebd2b33e72d54940bee621cf7eac4ce795a2403256e4263cba0bf1",
                "groupItemTitle": "Belete Molla",
                "groupItemThreshold": "1",
                "orderPriceMinTickSize": 0.01,
                "outcomes": "[\"Yes\", \"No\"]",
                "outcomePrices": "[\"0.008\", \"0.992\"]",
                "clobTokenIds": "[\"333\", \"444\"]",
                "events": [{
                    "id": "411239", "slug": "next-prime-minister-of-ethiopia",
                    "title": "Next Prime Minister of Ethiopia", "negRisk": true,
                    "negRiskMarketID": "0x55ab76d092f682bf5cbb7e14f13ee12f8410ce7cc1b7906f23b8fb56c11f6500"
                }]
            }
        ])
    }

    #[test]
    fn parses_neg_risk_group_identity_from_the_live_shape() {
        let ms = parse_gamma_markets(&live_neg_risk_pair());
        assert_eq!(ms.len(), 2);
        let a = &ms[0];
        // THE group key — shared by both members
        assert_eq!(
            a.neg_risk_market_id,
            "0x55ab76d092f682bf5cbb7e14f13ee12f8410ce7cc1b7906f23b8fb56c11f6500"
        );
        assert_eq!(a.neg_risk_market_id, ms[1].neg_risk_market_id, "one group key");
        assert!(a.is_neg_risk_member());
        // ...while negRiskRequestID is PER-MARKET and would NOT group them
        assert_ne!(a.neg_risk_request_id, ms[1].neg_risk_request_id);
        assert!(a.neg_risk_request_id.starts_with("0x1291bf07"));
        // groupItemThreshold is the 0-based INDEX (a decimal STRING on the wire), and it is
        // exactly questionID's last byte
        assert_eq!(a.group_item_index, Some(7));
        assert_eq!(ms[1].group_item_index, Some(1));
        assert_eq!(
            a.question_id,
            "0x55ab76d092f682bf5cbb7e14f13ee12f8410ce7cc1b7906f23b8fb56c11f6507"
        );
        assert_eq!(neg_risk_question_id(&a.neg_risk_market_id, 7).unwrap(), a.question_id);
        assert_eq!(neg_risk_question_id(&a.neg_risk_market_id, 1).unwrap(), ms[1].question_id);
        assert_eq!(a.group_item_title, "Gedion Timothewos");
        // parent event lifted off events[0]
        assert_eq!(a.event_id, "411239");
        assert_eq!(a.event_slug, "next-prime-minister-of-ethiopia");
        assert_eq!(a.event_title, "Next Prime Minister of Ethiopia");
        // outcomePrices double-decoded from decimal strings, positionally aligned
        assert_eq!(a.outcome_prices, vec![0.0185, 0.9815]);
        assert_eq!(a.yes_price(), Some(0.0185));
        assert_eq!(a.yes_token_id(), Some("111"));
        assert_eq!(a.no_token_id(), Some("222"));
        // and the pre-existing fields are unchanged
        assert_eq!(a.token_ids, vec!["111".to_string(), "222".to_string()]);
        assert_eq!(a.outcomes, vec!["Yes".to_string(), "No".to_string()]);
        assert!(a.neg_risk);
    }

    #[test]
    fn a_non_neg_risk_market_parses_exactly_as_before() {
        // the FIRST canned entry has no negRiskMarketID / questionID / groupItem* / events —
        // every new field must come back empty, and nothing pre-existing may change.
        let m = &parse_gamma_markets(&canned())[0];
        assert!(m.neg_risk_market_id.is_empty());
        assert!(m.neg_risk_request_id.is_empty());
        assert!(m.question_id.is_empty());
        assert!(m.group_item_title.is_empty());
        assert_eq!(m.group_item_index, None);
        assert!(m.event_id.is_empty() && m.event_slug.is_empty() && m.event_title.is_empty());
        assert!(!m.is_neg_risk_member(), "no group key => not a set member");
        // outcomePrices IS present on that entry and is now decoded
        assert_eq!(m.outcome_prices, vec![0.505, 0.495]);
        // the legacy groupItemThreshold-presence heuristic still raises `neg_risk` (entry 2),
        // even though that entry has no group key to group on — which is exactly why
        // `is_neg_risk_member` exists as the stricter test.
        let g = &parse_gamma_markets(&canned())[1];
        assert!(g.neg_risk);
        assert!(!g.is_neg_risk_member());
        assert_eq!(g.group_item_index, Some(2), "\"2\" parsed as the index");
    }

    #[test]
    fn parse_gamma_events_flattens_nested_markets_and_injects_event_identity() {
        // the /events shape: the event wraps markets[], and those nested markets carry NO `events`
        // key (live-verified) — so the parent's identity must be injected.
        let ev = serde_json::json!([{
            "id": "30829",
            "slug": "democratic-presidential-nominee-2028",
            "title": "Democratic Presidential Nominee 2028",
            "negRisk": true,
            "negRiskMarketID": "0x2c3d7e0eee6f058be3006baabf0d54a07da254ba47fe6e3e095e7990c7814700",
            "markets": [
                { "id": "1", "question": "Oprah Winfrey?", "conditionId": "0xe06a7e94cf",
                  "questionID": "0x2c3d7e0eee6f058be3006baabf0d54a07da254ba47fe6e3e095e7990c7814700",
                  "negRiskMarketID": "0x2c3d7e0eee6f058be3006baabf0d54a07da254ba47fe6e3e095e7990c7814700",
                  "groupItemTitle": "Oprah Winfrey", "groupItemThreshold": "0", "active": true,
                  "outcomes": "[\"Yes\",\"No\"]", "outcomePrices": "[\"0.0045\",\"0.9955\"]",
                  "clobTokenIds": "[\"a\",\"b\"]" },
                // a nested market that omitted its own group key: the event's fills in
                { "id": "2", "question": "Bernie Sanders?", "conditionId": "0x30cfb88755",
                  "groupItemTitle": "Bernie Sanders", "groupItemThreshold": "1", "active": true,
                  "outcomes": "[\"Yes\",\"No\"]", "outcomePrices": "[\"0.0065\",\"0.9935\"]",
                  "clobTokenIds": "[\"c\",\"d\"]" }
            ]
        }]);
        let ms = parse_gamma_events(&ev);
        assert_eq!(ms.len(), 2, "flattened out of the event");
        for m in &ms {
            assert_eq!(m.event_id, "30829");
            assert_eq!(m.event_slug, "democratic-presidential-nominee-2028");
            assert_eq!(m.event_title, "Democratic Presidential Nominee 2028");
            assert_eq!(
                m.neg_risk_market_id,
                "0x2c3d7e0eee6f058be3006baabf0d54a07da254ba47fe6e3e095e7990c7814700",
                "own key kept, missing key filled from the event"
            );
        }
        assert_eq!(ms[0].group_item_index, Some(0));
        assert_eq!(ms[1].group_item_index, Some(1));
        // an event with no markets contributes nothing; a non-array is empty, no panic
        assert!(parse_gamma_events(&serde_json::json!([{ "id": "x", "slug": "y" }])).is_empty());
        assert!(parse_gamma_events(&serde_json::json!({})).is_empty());
    }

    #[test]
    fn neg_risk_question_id_replaces_the_last_byte_only() {
        const MID: &str = "0x55ab76d092f682bf5cbb7e14f13ee12f8410ce7cc1b7906f23b8fb56c11f6500";
        assert_eq!(
            neg_risk_question_id(MID, 0).unwrap(),
            "0x55ab76d092f682bf5cbb7e14f13ee12f8410ce7cc1b7906f23b8fb56c11f6500"
        );
        assert_eq!(
            neg_risk_question_id(MID, 127).unwrap(),
            "0x55ab76d092f682bf5cbb7e14f13ee12f8410ce7cc1b7906f23b8fb56c11f657f"
        );
        assert_eq!(
            neg_risk_question_id(MID, 255).unwrap(),
            "0x55ab76d092f682bf5cbb7e14f13ee12f8410ce7cc1b7906f23b8fb56c11f65ff"
        );
        // works without the 0x prefix, and always emits one
        assert_eq!(
            neg_risk_question_id(&MID[2..], 1).unwrap(),
            neg_risk_question_id(MID, 1).unwrap()
        );
        // refusals: an index that does not fit one byte, and a malformed key
        assert_eq!(neg_risk_question_id(MID, 256), None);
        assert_eq!(neg_risk_question_id("0xdead", 1), None);
        assert_eq!(neg_risk_question_id("", 1), None);
        assert_eq!(neg_risk_question_id(&"z".repeat(64), 1), None);
    }

    #[test]
    fn decode_json_string_f64_array_double_decodes_decimal_strings() {
        let v = serde_json::json!("[\"0.0185\", \"0.9815\"]");
        assert_eq!(decode_json_string_f64_array(&v), vec![0.0185, 0.9815]);
        // an unparseable element becomes 0.0 rather than dropping the array (alignment matters)
        assert_eq!(
            decode_json_string_f64_array(&serde_json::json!("[\"x\",\"1\"]")),
            vec![0.0, 1.0]
        );
        assert!(decode_json_string_f64_array(&serde_json::json!("")).is_empty());
        assert!(decode_json_string_f64_array(&serde_json::json!(7)).is_empty());
    }

    #[test]
    fn gamma_event_slug_query_is_an_exact_event_lookup() {
        assert_eq!(
            gamma_event_slug_query("next-prime-minister-of-ethiopia", true),
            "active=true&closed=false&slug=next-prime-minister-of-ethiopia"
        );
        assert_eq!(gamma_event_slug_query("abc", false), "slug=abc");
        assert!(!gamma_event_slug_query("abc", true).contains("order="));
    }

    #[test]
    fn resolution_ts_ms_parses_rfc3339_utc() {
        let mk = |end: &str| GammaMarket {
            id: "1".into(),
            question: "q".into(),
            condition_id: "0x1".into(),
            slug: "s".into(),
            end_date: end.into(),
            volume: 0.0,
            liquidity: 0.0,
            active: true,
            closed: false,
            neg_risk: false,
            tick_size: 0.01,
            outcomes: vec![],
            token_ids: vec![],
            ..Default::default()
        };
        // epoch anchors (cross-checked against `date -u -d ... +%s`)
        assert_eq!(mk("1970-01-01T00:00:00Z").resolution_ts_ms(), Some(0));
        assert_eq!(mk("2000-01-01T00:00:00Z").resolution_ts_ms(), Some(946_684_800_000));
        // the canned catalog value in this file
        assert_eq!(mk("2026-07-31T12:00:00Z").resolution_ts_ms(), Some(1_785_499_200_000));
        // tolerant of a fractional-second suffix + a space separator (both treated as UTC)
        assert_eq!(mk("2026-07-31T12:00:00.500Z").resolution_ts_ms(), Some(1_785_499_200_000));
        assert_eq!(mk("2026-07-31 12:00:00Z").resolution_ts_ms(), Some(1_785_499_200_000));
        // absent / malformed → None (never panics)
        assert_eq!(mk("").resolution_ts_ms(), None);
        assert_eq!(mk("x").resolution_ts_ms(), None);
        assert_eq!(mk("2026-13-01T00:00:00Z").resolution_ts_ms(), None); // month 13
    }

    #[test]
    fn gamma_markets_without_reward_fields_have_default_rewards() {
        // the canned catalog markets carry no reward fields → the additive field is inert/default,
        // so nothing that parsed before this field existed changes.
        for m in parse_gamma_markets(&canned()) {
            assert_eq!(m.rewards, crate::rewards::RewardsConfig::default());
            assert!(!m.rewards.earns_rewards());
        }
    }

    #[test]
    fn gamma_market_with_reward_fields_parses_min_size_and_max_spread() {
        let json = serde_json::json!([{
            "id": "1", "question": "rewarded?", "conditionId": "0xr", "slug": "r",
            "endDate": "2026-08-01T00:00:00Z", "active": true, "closed": false,
            "orderPriceMinTickSize": 0.01,
            "outcomes": "[\"Yes\",\"No\"]", "clobTokenIds": "[\"1\",\"2\"]",
            "rewardsMinSize": 50.0, "rewardsMaxSpread": 3.5
        }]);
        let m = &parse_gamma_markets(&json)[0];
        assert_eq!(m.rewards.min_size, 50.0);
        assert_eq!(m.rewards.max_spread, 3.5); // cents, verbatim
        assert!(m.rewards.earns_rewards());
        // Gamma carries neither of these — they stay at the zero default until enriched off the CLOB.
        assert_eq!(m.rewards.daily_rate, 0.0);
        assert_eq!(m.rewards.min_order_age_secs, 0);
    }
}
