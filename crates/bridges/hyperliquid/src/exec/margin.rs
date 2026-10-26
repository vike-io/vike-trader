//! The margin guard: before the FIRST opening order on a perp coin, the account holds that coin in
//! CROSS margin — the one mode this engine models (`vike_venues::venue_caps::HYPERLIQUID`'s
//! `margin_modes`) — and the bridge repairs it through the API when it does not, so no operator
//! steps into the Hyperliquid app first (the owner's "trade ANY ticker with no hand step", 2026-10-26).
//!
//! # Which accounts: the TESTNET only (owner ruling 2026-10-26, "what do you need live if you have demo")
//!
//! This guard WRITES to the account, and no exchange write is made on a `live` account. It runs only
//! when the exec thread signs on the testnet (`demo`, and a `paper` tier that reaches a bridge, read as
//! `demo`: [`crate::base::config::Network::for_tier`]). On MAINNET it does nothing at all: nothing is
//! read, nothing is written, and an order on a coin held in a mode the venue refuses is the venue's own
//! refusal text, exactly as before this module existed. The gate is a POSITIVE match on the testnet, so
//! a network added later is not written to by default.
//!
//! # What it reads, and what it may send
//!
//! One public `activeAssetData` read of the coin (weight 20): the account's own `{type, value}` for
//! it. Then exactly one of three outcomes ([`plan`], PURE):
//!
//! * **held cross** — nothing is sent, ever. The venue margins at the figure it holds and the engine
//!   gates at or below it (decision 0181), so this guard never moves a cross leverage, up or down.
//! * **held isolated, no open position on the coin** — one signed `updateLeverage` with `isCross: true`
//!   and the figure the account ALREADY holds there, clamped into `1..=maxLeverage`. It changes the
//!   MODE and keeps the number: the account's stated `account.leverage` never reaches a bridge (it
//!   rides to the node's engine, decision 0181), and 1x would silently shrink what the operator
//!   granted the coin. A refused or unconfirmed switch refuses the order in words (nothing is sent),
//!   because an isolated position the engine models as cross is the one outcome worse than a refusal.
//! * **held isolated WITH an open position** — nothing is sent: the venue refuses to switch a margin
//!   mode under an open position (measured on no wire here: it is the venue's rule as published, so
//!   this crate does not even try), and the order goes as it always did, with one warning per coin
//!   naming that. It is re-read at the next order (a closed position lets the switch happen then).
//!
//! # What it never touches
//!
//! A reduce-only order (a close must never wait on, or be refused by, a margin call), spot, an
//! isolated-only coin (`onlyIsolated`, `strictIsolated`, `noCross`: cross is not available there, and
//! such a coin mounted on an engine is margined isolated, `crate::adapter`'s `margin_mode`) and a HIP-3
//! builder-dex coin (opt-in, refused upstream). A read that FAILS (or answers a shape this does not
//! know) is "the venue did not say": the order goes as before and the coin is asked again next time —
//! never refused on a guess.
//!
//! A verified coin is remembered for the life of the exec thread, so the cost is one read per coin per
//! adapter start (and one write when the mode needed repairing). Someone flipping the coin to isolated
//! in the app MID-SESSION is not seen until the account's next start (`account.active` off and on).

use std::collections::HashSet;

use serde_json::Value;
use vike_model::OrderRequest;

use crate::base::config::{Network, Product};
use crate::base::consts::VENUE;
use crate::base::symbology::Symbology;
use crate::leverage_cap::active_asset_data_body;
use crate::signing::action::{Action, UpdateLeverageAction};

use super::order_wire::HlExchange;
use super::state::ExecState;

/// What the account holds on one coin, from `activeAssetData`'s `leverage` object.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Held {
    pub(super) cross: bool,
    pub(super) leverage: u32,
}

/// The coin's `{type, value}` out of an `activeAssetData` answer. `None` for any shape this does not
/// know (a missing object, a word other than `cross`/`isolated`, a figure under 1): "the venue did not
/// say", never a guess.
pub(super) fn parse_held(resp: &Value) -> Option<Held> {
    let leverage = resp.get("leverage")?;
    let cross = match leverage.get("type")?.as_str()? {
        "cross" => true,
        "isolated" => false,
        _ => return None,
    };
    let value = leverage.get("value")?.as_f64().filter(|v| v.is_finite() && *v >= 1.0)?;
    Some(Held { cross, leverage: value.round() as u32 })
}

/// Whether a `clearinghouseState` answer lists an OPEN position (`szi` other than zero) on `coin`.
/// `None` when the answer has no `assetPositions` list at all.
pub(super) fn has_open_position(resp: &Value, coin: &str) -> Option<bool> {
    let rows = resp.get("assetPositions")?.as_array()?;
    Some(rows.iter().any(|row| {
        let position = row.get("position");
        position.and_then(|p| p.get("coin")).and_then(Value::as_str) == Some(coin)
            && position
                .and_then(|p| p.get("szi"))
                .and_then(Value::as_str)
                .and_then(|s| s.parse::<f64>().ok())
                .is_some_and(|szi| szi != 0.0)
    }))
}

/// What to do about one coin's held margin.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Plan {
    /// Held cross: nothing to send.
    Ready,
    /// Held isolated, flat: send `updateLeverage` cross at this figure.
    SetCross(u32),
    /// Held isolated under an open position: the venue refuses the switch, so none is sent.
    HeldByPosition,
}

/// The decision, PURE. The figure kept is the one the account holds, clamped into the coin's
/// `1..=maxLeverage` (a coin whose published maximum is unknown keeps the held figure as it is).
pub(super) fn plan(held: Held, max_leverage: Option<u32>, open_position: bool) -> Plan {
    if held.cross {
        return Plan::Ready;
    }
    if open_position {
        return Plan::HeldByPosition;
    }
    let ceiling = max_leverage.unwrap_or(held.leverage).max(1);
    Plan::SetCross(held.leverage.clamp(1, ceiling))
}

/// The venue's answer to an `updateLeverage`: `{"status":"ok","response":{"type":"default"}}`, or
/// `{"status":"err","response":"<why>"}` under HTTP 200. `Err` carries the venue's words.
pub(super) fn update_leverage_reply(resp: &Value) -> Result<(), String> {
    match resp.get("status").and_then(Value::as_str) {
        Some("ok") => Ok(()),
        Some("err") => Err(resp
            .get("response")
            .and_then(Value::as_str)
            .unwrap_or("the venue refused it without a reason")
            .to_string()),
        _ => Err(format!("the venue's answer was not `ok`: {resp}")),
    }
}

/// The per-thread memory of the guard: what it already settled, and what it already said.
#[derive(Default)]
pub(super) struct MarginGuard {
    /// Perp assets verified cross (or switched to it) this session: never asked again.
    ready: HashSet<u32>,
    /// Assets whose "held isolated under an open position" warning went out (once each).
    warned: HashSet<u32>,
}

impl<X: HlExchange> ExecState<X> {
    /// Settle `req`'s coin before its order is built: `Ok` to go on, `Err(words)` to refuse the order
    /// (nothing has been sent for it). See the module doc for every case; an unknown symbol is
    /// `Ok` here and refused, in its own words, by `build_order_wire`.
    pub(super) fn ensure_cross(
        &mut self,
        symbology: &Symbology,
        req: &OrderRequest,
    ) -> Result<(), String> {
        // No exchange write on a live account: mainnet is left exactly as it was (module doc).
        if self.exchange.network() != Network::Testnet {
            return Ok(());
        }
        let Some(inst) = symbology.by_symbol(&req.symbol) else { return Ok(()) };
        if req.reduce_only
            || inst.product != Product::Perp
            || inst.dex.is_some()
            || inst.only_isolated
            || inst.no_cross
            || self.margin.ready.contains(&inst.asset_id)
        {
            return Ok(());
        }
        let coin = &inst.coin;
        let held = match self.exchange.info(&active_asset_data_body(self.exchange.master(), coin)) {
            Ok(answer) => parse_held(&answer),
            Err(e) => {
                tracing::warn!(venue = VENUE, %coin, error = %e, "margin guard: activeAssetData read failed; the order goes as before");
                return Ok(());
            }
        };
        let Some(held) = held else {
            tracing::warn!(venue = VENUE, %coin, "margin guard: activeAssetData named no usable leverage; the order goes as before");
            return Ok(());
        };
        let open_position = if held.cross {
            false
        } else {
            let state =
                serde_json::json!({ "type": "clearinghouseState", "user": self.exchange.master() });
            match self.exchange.info(&state).ok().and_then(|a| has_open_position(&a, coin)) {
                Some(open) => open,
                None => {
                    tracing::warn!(venue = VENUE, %coin, "margin guard: the account's positions could not be read, so whether {coin}'s isolated margin may be switched is unknown; the order goes as before");
                    return Ok(());
                }
            }
        };
        match plan(held, inst.max_leverage, open_position) {
            Plan::Ready => {
                self.margin.ready.insert(inst.asset_id);
                Ok(())
            }
            Plan::HeldByPosition => {
                if self.margin.warned.insert(inst.asset_id) {
                    tracing::warn!(
                        venue = VENUE, %coin, leverage = held.leverage,
                        "{coin} is held in ISOLATED margin with an open position, and Hyperliquid refuses \
                         to switch a margin mode under an open position, so it was NOT switched to cross; \
                         the engine margins cross. Close the position and the next order switches it."
                    );
                }
                Ok(())
            }
            Plan::SetCross(leverage) => {
                let action = Action::UpdateLeverage(UpdateLeverageAction {
                    asset: inst.asset_id,
                    is_cross: true,
                    leverage,
                });
                let outcome = match self.exchange.place(&action) {
                    Ok(answer) => update_leverage_reply(&answer),
                    Err(e) if e.kind().must_requery() => Err(format!(
                        "the request timed out after it was sent, so whether it applied is unknown ({})",
                        e.msg
                    )),
                    Err(e) => Err(e.msg),
                };
                match outcome {
                    Ok(()) => {
                        tracing::info!(venue = VENUE, %coin, leverage, "margin guard: {coin} was held in isolated margin; switched to cross at the figure the account already held");
                        self.margin.ready.insert(inst.asset_id);
                        Ok(())
                    }
                    Err(why) => Err(format!(
                        "{coin} is held in ISOLATED margin on this account and the switch to cross \
                         failed ({why}); the engine margins cross, so the order was not sent. Set \
                         {coin} to cross margin in the Hyperliquid app, or retry"
                    )),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    //! The pure halves. The guard driven through `ExecState::submit` (what is sent, what is refused,
    //! what is remembered) is `exec_margin_tests.rs`.
    use super::*;
    use serde_json::json;

    fn active(kind: &str, value: u64) -> Value {
        json!({"user": "0xabc", "coin": "ETH", "leverage": {"type": kind, "value": value},
               "maxTradeSzs": ["1.0", "1.0"], "availableToTrade": ["8.8", "8.8"], "markPx": "1.0"})
    }

    #[test]
    fn the_held_leverage_is_read_off_the_active_asset_data_shape() {
        assert_eq!(parse_held(&active("cross", 20)), Some(Held { cross: true, leverage: 20 }));
        assert_eq!(parse_held(&active("isolated", 3)), Some(Held { cross: false, leverage: 3 }));
        // Every shape this does not know is "the venue did not say", never a guess.
        assert_eq!(parse_held(&active("portfolio", 3)), None);
        assert_eq!(parse_held(&active("cross", 0)), None);
        assert_eq!(parse_held(&json!({"coin": "ETH"})), None);
        assert_eq!(parse_held(&json!({"leverage": {"type": "cross"}})), None);
    }

    #[test]
    fn an_open_position_is_a_nonzero_szi_on_the_named_coin() {
        let state = json!({"assetPositions": [
            {"type": "oneWay", "position": {"coin": "ETH", "szi": "0.0", "leverage": {"type": "isolated", "value": 3}}},
            {"type": "oneWay", "position": {"coin": "BTC", "szi": "-0.01", "leverage": {"type": "cross", "value": 5}}},
        ]});
        assert_eq!(has_open_position(&state, "BTC"), Some(true), "a short is open too");
        assert_eq!(has_open_position(&state, "ETH"), Some(false), "a zero size is flat");
        assert_eq!(has_open_position(&state, "SOL"), Some(false), "no row is flat");
        assert_eq!(has_open_position(&json!({"marginSummary": {}}), "ETH"), None);
    }

    /// The whole decision table: only an isolated, flat coin is ever written, and the figure it is
    /// written at is the held one, clamped into the coin's own range.
    #[test]
    fn only_an_isolated_flat_coin_is_switched_and_it_keeps_its_figure() {
        let cross = Held { cross: true, leverage: 20 };
        let isolated = Held { cross: false, leverage: 3 };
        assert_eq!(plan(cross, Some(25), false), Plan::Ready);
        assert_eq!(plan(cross, Some(25), true), Plan::Ready, "a cross leverage is never moved");
        assert_eq!(plan(isolated, Some(25), true), Plan::HeldByPosition);
        assert_eq!(plan(isolated, Some(25), false), Plan::SetCross(3));
        assert_eq!(plan(isolated, None, false), Plan::SetCross(3), "no published maximum: as held");
        let over = Held { cross: false, leverage: 50 };
        assert_eq!(
            plan(over, Some(25), false),
            Plan::SetCross(25),
            "clamped to the coin's maximum"
        );
        assert_eq!(plan(isolated, Some(0), false), Plan::SetCross(1), "never below 1x");
    }

    #[test]
    fn the_exchange_reply_is_ok_only_when_the_venue_says_ok() {
        let ok = json!({"status": "ok", "response": {"type": "default"}});
        assert_eq!(update_leverage_reply(&ok), Ok(()));
        // The refusal text is the venue's own and is passed through untouched (this one is a stand-in).
        let refused = json!({"status": "err", "response": "a reason the venue gave"});
        assert_eq!(update_leverage_reply(&refused), Err("a reason the venue gave".to_string()));
        assert!(update_leverage_reply(&json!({"response": {}})).unwrap_err().contains("not `ok`"));
    }
}
