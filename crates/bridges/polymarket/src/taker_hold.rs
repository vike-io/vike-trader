//! `taker_hold` — resolving Polymarket's **venue-enforced order hold** for one market into the ONE
//! number the rest of the stack consumes, `vike_model::SymbolProperties::taker_hold_ms`.
//!
//! ⚠ **There are TWO separate, independent mechanisms.** They are declared by different fields, on
//! different endpoints, over disjoint market sets, and conflating them is the mistake this module
//! exists to prevent (it was made once already: probing only `/markets` makes crypto look
//! delay-free). Both figures below were measured LIVE against the production APIs on 2026-07-23.
//!
//! 1. **`itode` → [`HOLD_ITODE_MS`] (250 ms)** — the CRYPTO/finance up/down markets. The flag lives
//!    ONLY on `GET {CLOB_BASE}/clob-markets/{condition_id}`, whose payload is TERSE-KEYED:
//!    `{r,t,c,mos,mts,mbf,tbf,ao,aot,itode,fd}`. It is **NOT** on `/markets/{condition_id}` and
//!    **NOT** on Gamma. Verified `itode: true` on 4/4 live btc/eth/sol/xrp `updown-5m` markets;
//!    live since 2026-06-05. Semantics (docs.polymarket.com/concepts/order-lifecycle): the order is
//!    held 250 ms, a CANCEL IS REJECTED while it is pending, it survives a dropped connection, and
//!    it is then re-validated and either matched or placed on the book.
//! 2. **`seconds_delay` → 3 s** — SPORTS **game** markets. This field IS on the CLOB
//!    `/markets/{condition_id}` payload, on Gamma (`secondsDelay`, camelCase), AND — measured live
//!    2026-07-23 — on the terse `/clob-markets/{condition_id}` payload as `sd` (alongside `gst`,
//!    the game start), which is what lets [`fetch_taker_hold_ms`] answer for BOTH mechanisms in a
//!    single request. Measured over
//!    every open sports market: **400 of 400 markets carrying a `game_start_time` report
//!    `seconds_delay: 3`**, while 191 of 192 futures/props report `0` — the discriminator is
//!    exactly "has a game start". NBA (370) plus EPL/NFL/CBB/IPL. Politics: 0 of 30. Crypto reports
//!    `seconds_delay: 0`, because its delay is `itode` instead.
//!
//! ⚠ **`3` is not the only sports value.** A later live sweep over Gamma's whole open TRADEABLE
//! listing (2026-07-23) found the entire esports book — CS2 futures/handicaps/totals — declaring
//! `secondsDelay: 1` (a 1000 ms hold). The earlier "400 of 400" figure was measured over
//! traditional game markets. This changes nothing in the code — every function here reads the
//! number off the venue PER MARKET and multiplies — but it does mean no caller may assume 3000,
//! and it is why the end-to-end smoke asserts against the directory's own declaration rather than
//! against `POLYMARKET_SPORTS_GAME_HOLD_MS`.
//!
//! The resolution rule is therefore [`resolve_taker_hold_ms`]: `itode` wins if set, else
//! `seconds_delay * 1000`, else `0` (= this venue declares no hold for this market, the
//! `SymbolProperties` absent convention). The two have never been observed co-occurring; if they
//! ever do, `itode` — the mechanism with published lifecycle semantics — is the one taken.
//!
//! Everything here is behind this crate's `polymarket` feature, like the rest of the module tree.
//! The parse functions are PURE (`&serde_json::Value` in, plain data out) and fixture-tested
//! against the real payload shapes; only the `fetch_*` functions touch the network.
//!
//! **Where this is called from, and how often.** The live entry point is
//! [`fetch_token_taker_hold_ms`], driven by the market feed's per-token resolution
//! (`market_feed::reconcile_slots`) ONCE per newly-seated token, on the feed thread, and ONLY when
//! the opt-in `PropertiesRecorder` is armed (`VIKE_RECORD_PROPERTIES=1`). Two requests per token,
//! never per tick and never per order — the hold is a slow-moving per-market property.
//!
//! ## ⚠ Why the HISTORICAL backfills deliberately do NOT record a hold
//!
//! `vike-backfill`'s `--tokens`/`--tokens-file` paths (`pmxt_backfill`, `clickhouse_poly_backfill`)
//! also resolve token ids, and the obvious next step — resolve each one's hold and write it onto
//! the `kind=properties` tape — is WRONG and must not be taken. The endpoints answer only TODAY's
//! value, while `kind=properties` is a POINT-IN-TIME series: writing today's number at a
//! historical ts fabricates history, and this specific number demonstrably moves — the `itode`
//! mechanism only went live 2026-06-05, and the hold itself was 500 ms before it was 250 ms. A
//! January tape stamped `250` would be a confident lie a replay would then trade on, which is the
//! same failure mode as guessing a hold on a fetch failure. (`/clob-markets` offers no as-of
//! parameter, so there is no correct version of this.) The live feed's record-at-observation-time
//! is the ONLY honest source; a backfilled window simply has no hold on the tape, which reads back
//! as `0` — the absent convention — exactly as it should.

use serde_json::Value;
use vike_bridge_core::transport::RestTransport;

use super::config::{CLOB_BASE, GAMMA_BASE};

/// The `itode` hold, in ms — see this module's doc, mechanism (1).
///
/// This is a re-export of [`vike_model::POLYMARKET_ITODE_HOLD_MS`], NOT a second literal. The
/// number is a VENUE fact consumed by two crates that cannot see each other (this bridge reads it
/// off the wire, `vike-backtest` models it as entry-leg delay), and Polymarket has already changed
/// it once (500 ms → 250 ms) — so vike-model, which both depend on, owns it.
pub const HOLD_ITODE_MS: u32 = vike_model::POLYMARKET_ITODE_HOLD_MS;

/// The sports GAME-market hold, in ms — mechanism (2). Also owned by vike-model
/// ([`vike_model::POLYMARKET_SPORTS_GAME_HOLD_MS`]); re-exported here so the two mechanisms this
/// module resolves between are named side by side.
pub const HOLD_SPORTS_GAME_MS: u32 = vike_model::POLYMARKET_SPORTS_GAME_HOLD_MS;

/// The `/clob-markets/{condition_id}` path prefix — the ONLY endpoint carrying `itode`.
const CLOB_MARKETS_PATH: &str = "/clob-markets/";
/// The `/markets/{condition_id}` path prefix — carries `seconds_delay` (and not `itode`).
const MARKETS_PATH: &str = "/markets/";
/// The Gamma market directory, queried as a POINT lookup by `clob_token_ids` — the only route from
/// an outcome token back to its `condition_id` (see [`fetch_condition_id_for_token`]).
const GAMMA_MARKETS_PATH: &str = "/markets";

/// Read the `itode` flag out of a `/clob-markets/{condition_id}` body. Accepts the JSON
/// `true`/`false` form and (defensively, as [`crate::instruments::parse_neg_risk`] does) the
/// stringified one. `None` when the field is absent or carries anything else — which a caller must
/// read as "unknown", NOT as `false`: an absent `itode` is what `/markets` and Gamma both return
/// for a market that genuinely has the hold.
pub fn parse_itode(v: &Value) -> Option<bool> {
    match v.get("itode")? {
        Value::Bool(b) => Some(*b),
        Value::String(s) => match s.trim().to_ascii_lowercase().as_str() {
            "true" => Some(true),
            "false" => Some(false),
            _ => None,
        },
        _ => None,
    }
}

/// Read the sports delay out of any of the venue's THREE spellings of the same field, so ONE parser
/// serves every source (all three verified live 2026-07-23 on the same sports market, which
/// reported `3` through each):
///
/// * `seconds_delay` — the CLOB `/markets/{condition_id}` body;
/// * `secondsDelay` — the Gamma market body;
/// * `sd` — the TERSE `/clob-markets/{condition_id}` body, the same payload that carries `itode`.
///
/// That third spelling is what makes [`fetch_taker_hold_ms`] a ONE-request lookup for both
/// mechanisms: the terse payload turned out to carry `sd` (and `gst`, the game start) alongside
/// `itode`, so `/markets/{condition_id}` is now only a fallback for when the terse fetch fails.
///
/// Number OR string wire form (Gamma stringifies numerics liberally). `None` when absent or
/// unparseable; a negative or absurd value is rejected rather than clamped, so a wire regression
/// surfaces as "unknown" instead of silently becoming a hold.
pub fn parse_seconds_delay(v: &Value) -> Option<u32> {
    let raw = v.get("seconds_delay").or_else(|| v.get("secondsDelay")).or_else(|| v.get("sd"))?;
    let secs = match raw {
        Value::Number(n) => n.as_f64()?,
        Value::String(s) => s.trim().parse::<f64>().ok()?,
        _ => return None,
    };
    if !secs.is_finite() || !(0.0..=3600.0).contains(&secs) {
        return None;
    }
    Some(secs as u32)
}

/// The ONE resolution rule (see this module's doc): `itode: true` → [`HOLD_ITODE_MS`], else
/// `seconds_delay * 1000`, else `0`. `itode: false` is NOT a hold — it is the venue explicitly
/// saying this market is not on the 250 ms path, and the sports field is then consulted normally.
pub fn resolve_taker_hold_ms(itode: Option<bool>, seconds_delay: Option<u32>) -> u32 {
    if itode == Some(true) {
        return HOLD_ITODE_MS;
    }
    seconds_delay.unwrap_or(0).saturating_mul(vike_model::MS_PER_SECOND)
}

/// ONE `GET /clob-markets/{condition_id}` — the terse-keyed payload, the ONLY carrier of `itode`.
pub fn fetch_clob_market<T: RestTransport>(t: &T, condition_id: &str) -> Option<Value> {
    let path = format!("{CLOB_MARKETS_PATH}{condition_id}");
    match t.public(CLOB_BASE, &path, &[]) {
        Ok(v) => Some(v),
        Err(e) => {
            tracing::debug!(condition_id, error = %e, "fetch_clob_market: /clob-markets failed");
            None
        }
    }
}

/// ONE `GET /markets/{condition_id}` — the ordinary market payload, carrier of `seconds_delay`.
pub fn fetch_market<T: RestTransport>(t: &T, condition_id: &str) -> Option<Value> {
    let path = format!("{MARKETS_PATH}{condition_id}");
    match t.public(CLOB_BASE, &path, &[]) {
        Ok(v) => Some(v),
        Err(e) => {
            tracing::debug!(condition_id, error = %e, "fetch_market: /markets failed");
            None
        }
    }
}

/// Resolve one market's venue hold in ms, live, in **ONE request** on the happy path.
///
/// `GET /clob-markets/{cid}` carries BOTH mechanisms — `itode` for the crypto hold and the terse
/// `sd` for the sports delay (live-verified 2026-07-23: a sports market's terse body reads
/// `{gst: "…", sd: 3, …}`) — so a single fetch answers for every market kind. `/markets/{cid}` is
/// kept ONLY as the fallback for when that fetch fails or its body parses to nothing, which is why
/// [`fetch_market`] still exists.
///
/// Any REST or parse failure degrades to the next source and finally to `0`, the
/// `SymbolProperties` absent convention: an unknown hold is modelled as no hold, never guessed.
pub fn fetch_taker_hold_ms<T: RestTransport>(t: &T, condition_id: &str) -> u32 {
    let terse = fetch_clob_market(t, condition_id);
    let itode = terse.as_ref().and_then(parse_itode);
    if itode == Some(true) {
        return HOLD_ITODE_MS;
    }
    // The terse body's own `sd` — present on sports markets, so the second request is usually
    // never made. `None` here means "this payload said nothing about a delay", which includes the
    // case where the terse fetch itself failed.
    let secs = match terse.as_ref().and_then(parse_seconds_delay) {
        Some(s) => Some(s),
        None => fetch_market(t, condition_id).as_ref().and_then(parse_seconds_delay),
    };
    let hold = resolve_taker_hold_ms(itode, secs);
    if hold == 0 {
        tracing::debug!(
            condition_id,
            "fetch_taker_hold_ms: venue declares no hold for this market"
        );
    }
    hold
}

/// The Gamma point lookup that turns an outcome `token_id` into its market's `condition_id` — the
/// ONE piece of plumbing the live feed needs, because every `DataClient` subscribe verb on this
/// venue is keyed by token while both hold mechanisms are declared per CONDITION.
///
/// `GET {GAMMA_BASE}/markets?clob_token_ids={token_id}` answers a one-element array (verified live
/// 2026-07-23 on both a crypto `updown-5m` token and a sports token). There is no CLOB-side
/// equivalent: `/tick-size` and `/neg-risk` are the only per-token point endpoints and neither
/// returns the condition, `/markets/{x}` 404s on a token id, and the paged `/markets` walk that
/// [`crate::instruments::fetch_token_tick_size_paged`] uses costs up to
/// `TICK_SIZE_LOOKUP_MAX_PAGES` requests to answer the same question.
///
/// `None` on any REST/parse failure — the caller then records no hold rather than guessing one.
pub fn fetch_condition_id_for_token<T: RestTransport>(t: &T, token_id: &str) -> Option<String> {
    match t.public(GAMMA_BASE, GAMMA_MARKETS_PATH, &[("clob_token_ids", token_id.to_string())]) {
        Ok(v) => {
            let cid = parse_condition_id(&v);
            if cid.is_none() {
                tracing::debug!(token_id, "fetch_condition_id_for_token: no market for this token");
            }
            cid
        }
        Err(e) => {
            tracing::debug!(token_id, error = %e, "fetch_condition_id_for_token: Gamma lookup failed");
            None
        }
    }
}

/// Pull the `conditionId` out of a Gamma `/markets` response — the array form the point lookup
/// above answers with, and (defensively) the `{"data": [...]}` envelope the CLOB uses. Pure.
pub fn parse_condition_id(v: &Value) -> Option<String> {
    let arr = v.as_array().or_else(|| v.get("data").and_then(|d| d.as_array()))?;
    arr.iter()
        .find_map(|m| m.get("conditionId").or_else(|| m.get("condition_id")))
        .and_then(|c| c.as_str())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
}

/// One token's venue hold, end to end: `token_id` → `condition_id` (Gamma point lookup) →
/// [`fetch_taker_hold_ms`]. **Two requests** for a market of either kind on the happy path (one
/// Gamma, one `/clob-markets`); `0` — no hold — the moment any step fails.
///
/// This is what the live market feed calls, ONCE per newly-seated token, and only when the opt-in
/// [`vike_data::PropertiesRecorder`] is armed (`VIKE_RECORD_PROPERTIES=1`). It is deliberately NOT
/// on any per-tick or per-order path: the hold is a slow-moving per-market property.
pub fn fetch_token_taker_hold_ms<T: RestTransport>(t: &T, token_id: &str) -> u32 {
    fetch_token_taker_hold_ms_while(t, token_id, &|| true)
}

/// [`fetch_token_taker_hold_ms`] with a CONTINUE PREDICATE consulted before EACH of its two
/// requests — the shape `crates/bridges/polymarket/src/instruments.rs`'s
/// `fetch_token_tick_size_while` has, for the same reason and the same caller.
///
/// The predicate between the two is the load-bearing one. Both requests ride one bounded agent, so
/// without it a stop raised during the Gamma lookup still buys a second full round trip, and the
/// window a feed thread can be caught in doubles the moment `VIKE_RECORD_PROPERTIES=1` is set — a
/// budget that depends on an operator's recording flag is not a budget.
pub fn fetch_token_taker_hold_ms_while<T: RestTransport>(
    t: &T,
    token_id: &str,
    keep_going: &dyn Fn() -> bool,
) -> u32 {
    if !keep_going() {
        return 0;
    }
    match fetch_condition_id_for_token(t, token_id) {
        Some(cid) if keep_going() => fetch_taker_hold_ms(t, &cid),
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The REAL terse-keyed `/clob-markets/{condition_id}` shape — a live btc `updown-5m` market.
    fn clob_market_crypto() -> Value {
        serde_json::json!({
            "r": "0x1234",
            "t": 1_753_228_800,
            "c": "0xcondition",
            "mos": 5.0,
            "mts": 0.001,
            "mbf": 0.0,
            "tbf": 0.0,
            "ao": true,
            "aot": 0,
            "itode": true,
            "fd": { "r": 0.07 }
        })
    }

    /// The REAL terse `/clob-markets/{condition_id}` shape for a SPORTS market — captured live
    /// 2026-07-23. Note it carries the delay itself, as `sd`, plus `gst` (the game start): this
    /// payload alone answers for both mechanisms, which is why `fetch_taker_hold_ms` is a
    /// one-request lookup and `/markets/{cid}` is only a fallback.
    fn clob_market_sports() -> Value {
        serde_json::json!({
            "gst": "2025-11-28T17:30:00Z",
            "r": { "mi": 50, "ma": 4.5, "moas": 30 },
            "c": "0xcondition",
            "sd": 3,
            "mos": 5,
            "mts": 0.001,
            "ao": true,
            "nr": true,
            "cbos": true,
            "aot": "2025-11-18T14:17:05Z",
            "ibce": true
        })
    }

    /// The CLOB `/markets/{condition_id}` shape for a sports GAME market (has a game start).
    fn market_sports_game() -> Value {
        serde_json::json!({
            "condition_id": "0xcondition",
            "game_start_time": "2026-07-23T23:00:00Z",
            "seconds_delay": 3,
            "neg_risk": false,
            "minimum_tick_size": "0.001"
        })
    }

    #[test]
    fn itode_is_read_from_the_terse_clob_markets_payload() {
        assert_eq!(parse_itode(&clob_market_crypto()), Some(true));
        assert_eq!(parse_itode(&serde_json::json!({ "itode": false })), Some(false));
        assert_eq!(parse_itode(&serde_json::json!({ "itode": "true" })), Some(true));
        // absent — which is EXACTLY what /markets and Gamma return for an itode market
        assert_eq!(parse_itode(&market_sports_game()), None);
        assert_eq!(parse_itode(&serde_json::json!({ "itode": 1 })), None);
    }

    #[test]
    fn seconds_delay_is_read_from_both_the_clob_and_gamma_spellings() {
        assert_eq!(parse_seconds_delay(&market_sports_game()), Some(3));
        assert_eq!(parse_seconds_delay(&serde_json::json!({ "secondsDelay": 3 })), Some(3));
        assert_eq!(parse_seconds_delay(&serde_json::json!({ "seconds_delay": "3" })), Some(3));
        // a futures/props market (191 of 192 measured) and the crypto payload both report none
        assert_eq!(parse_seconds_delay(&serde_json::json!({ "seconds_delay": 0 })), Some(0));
        assert_eq!(parse_seconds_delay(&clob_market_crypto()), None);
        // malformed → unknown, never a fabricated hold
        assert_eq!(parse_seconds_delay(&serde_json::json!({ "seconds_delay": -1 })), None);
        assert_eq!(parse_seconds_delay(&serde_json::json!({ "seconds_delay": "soon" })), None);
    }

    #[test]
    fn the_two_mechanisms_resolve_to_the_measured_holds() {
        // crypto up/down: itode, and /markets would have said 0 — the itode arm must win
        assert_eq!(resolve_taker_hold_ms(Some(true), Some(0)), 250);
        assert_eq!(resolve_taker_hold_ms(Some(true), None), 250);
        // sports game market: seconds_delay 3 → 3000 ms
        assert_eq!(resolve_taker_hold_ms(None, Some(3)), 3_000);
        assert_eq!(resolve_taker_hold_ms(Some(false), Some(3)), 3_000);
        // futures/props, politics, and everything unknown: no hold
        assert_eq!(resolve_taker_hold_ms(None, Some(0)), 0);
        assert_eq!(resolve_taker_hold_ms(Some(false), None), 0);
        assert_eq!(resolve_taker_hold_ms(None, None), 0);
    }

    /// End-to-end over the REAL payload shapes, through the pure halves only — including the terse
    /// sports body, the one that makes the live lookup a single request.
    #[test]
    fn real_payloads_resolve_end_to_end() {
        for (payload, want) in [
            (clob_market_crypto(), HOLD_ITODE_MS),
            (market_sports_game(), HOLD_SPORTS_GAME_MS),
            (clob_market_sports(), HOLD_SPORTS_GAME_MS),
        ] {
            assert_eq!(
                resolve_taker_hold_ms(parse_itode(&payload), parse_seconds_delay(&payload)),
                want
            );
        }
    }

    /// The terse `sd` spelling is read exactly like the other two — this is the whole reason
    /// `fetch_taker_hold_ms` does not need a second request for a sports market.
    #[test]
    fn the_terse_clob_markets_body_carries_the_sports_delay_as_sd() {
        assert_eq!(parse_seconds_delay(&clob_market_sports()), Some(3));
        assert_eq!(parse_itode(&clob_market_sports()), None, "sports declares no itode");
        // and the crypto terse body has no `sd` at all — the two mechanisms stay disjoint
        assert_eq!(parse_seconds_delay(&clob_market_crypto()), None);
    }

    /// The holds are vike-model's, not this crate's — the pin that keeps the bridge and the
    /// backtest engine from drifting apart the way they did before the hoist.
    #[test]
    fn the_holds_are_the_shared_venue_facts() {
        assert_eq!(HOLD_ITODE_MS, vike_model::POLYMARKET_ITODE_HOLD_MS);
        assert_eq!(HOLD_SPORTS_GAME_MS, vike_model::POLYMARKET_SPORTS_GAME_HOLD_MS);
        assert_eq!(resolve_taker_hold_ms(None, Some(3)), HOLD_SPORTS_GAME_MS);
    }

    /// The Gamma point-lookup body — a one-element array — and the defensive CLOB envelope.
    #[test]
    fn condition_id_is_parsed_from_the_gamma_point_lookup() {
        let gamma = serde_json::json!([{
            "slug": "btc-updown-5m-1784815800",
            "conditionId": "0xcond",
            "clobTokenIds": "[\"111\", \"222\"]"
        }]);
        assert_eq!(parse_condition_id(&gamma).as_deref(), Some("0xcond"));
        let clob = serde_json::json!({ "data": [{ "condition_id": "0xcond2" }] });
        assert_eq!(parse_condition_id(&clob).as_deref(), Some("0xcond2"));
        // an empty result, an empty id, and a non-array body are all "unknown", never a fake cid
        assert_eq!(parse_condition_id(&serde_json::json!([])), None);
        assert_eq!(parse_condition_id(&serde_json::json!([{ "conditionId": "" }])), None);
        assert_eq!(parse_condition_id(&serde_json::json!({ "conditionId": "0xc" })), None);
    }
}
