//! Does the account HEDGE, and what that makes a reduce-only order carry (the `positionFill` of OANDA's order
//! request).
//!
//! # Why it exists
//!
//! `GET /v3/accounts/{id}/summary` states `hedgingEnabled`. On a hedging account the default `positionFill` of an
//! order is `OPEN_ONLY`, so an order against an existing position (a flatten, a market exit, a liquidation, the
//! fire of a protective stop) OPENS a locked opposite position beside the one it meant to close instead of
//! reducing it: the node's book nets the two fills to flat while the venue holds a locked pair (the failure class
//! of the cTrader row it was measured on, `docs/ops/known-limitations-2026-10.md`). A netting account needs no
//! word: its orders reduce the position by construction.
//!
//! # The contract
//!
//! - **Read once at the start of the exec thread** ([`AccountHedging::ensure`], called by `crate::exec`'s
//!   `run_on` before its first command), on the lookup agent (5 s). The answer is kept for the thread's life.
//! - **A hedging account**: every `reduce_only` order, a submit and the replacement of a replace alike, carries
//!   `"positionFill": "REDUCE_ONLY"`. A venue that cannot honour it rejects the order loudly; none is turned into
//!   a plain one.
//! - **A netting account**: the body is byte-for-byte what it was before this module (the offline tests compare
//!   the bytes).
//! - **Unknown** (the read failed, or the summary states no boolean): a reduce-only MARKET order is REFUSED by
//!   name (an `OrderRejected` that says why, nothing sent), because it is the order that opens the locked pair; a
//!   reduce-only LIMIT or STOP goes out as before (refusing a protective stop for a failed read would take away
//!   the protection it exists for). While the answer is unknown every reduce-only order asks again (one bounded
//!   read), so a venue that comes back is believed at once and a HALT-time flatten finds the answer it needs.
//!
//! # What is not proven
//!
//! The only practice account measured (account 14, 2026-10-26) says `hedgingEnabled=false`: the `REDUCE_ONLY`
//! word has never met a venue that hedges. The offline tests hold the bytes and the refusal, nothing more (the
//! live proof needs a hedging practice account, `docs/ops/known-limitations-2026-10.md`).

use serde_json::Value;
use vike_catalog::to_oanda_instrument;
use vike_model::OrderRequest;

use crate::exec::rests_at_venue;
use crate::rest::OandaRest;

const VENUE: &str = "oanda";

/// The `positionFill` of a reduce-only order on a hedging account: reduce an existing position, never open or
/// extend one.
pub(crate) const REDUCE_ONLY_FILL: &str = "REDUCE_ONLY";

/// What the exec thread knows about the account's `hedgingEnabled`. `Default` is "not asked yet" (unknown).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct AccountHedging(Option<bool>);

impl AccountHedging {
    /// An answer already in hand: the test harness's loop, which scripts the order calls only.
    #[cfg(test)]
    pub(crate) const fn stated(hedging: bool) -> Self {
        Self(Some(hedging))
    }

    /// The account's answer when it is not known yet: one bounded `GET /summary`. A failure or an unreadable
    /// summary leaves it unknown (said at `warn`); the account id never reaches the line.
    pub(crate) fn ensure(&mut self, rest: &OandaRest, base: &str, acct: &str) {
        if self.0.is_some() {
            return;
        }
        match rest.get_lookup(base, &format!("/v3/accounts/{acct}/summary"), "") {
            Ok(summary) => match parse_hedging_enabled(&summary) {
                Some(hedging) => {
                    self.0 = Some(hedging);
                    tracing::info!(
                        venue = VENUE,
                        hedging_enabled = hedging,
                        "oanda: the account's hedging setting is read; a reduce-only order carries \
                         positionFill REDUCE_ONLY only when it is on"
                    );
                }
                None => tracing::warn!(
                    venue = VENUE,
                    "oanda: the account summary states no boolean `hedgingEnabled`; a reduce-only MARKET \
                     order is refused until it does"
                ),
            },
            Err(e) => tracing::warn!(
                venue = VENUE, error = %e,
                "oanda: could not read the account's hedging setting; a reduce-only MARKET order is \
                 refused until the venue answers"
            ),
        }
    }

    /// The `positionFill` word `req` goes out with: `Ok(Some(word))` to stamp on the body, `Ok(None)` to leave
    /// the body as it is, `Err(why)` for the refusal sentence (the module doc's table). Asks the venue first when
    /// the order is reduce-only and the answer is unknown.
    pub(crate) fn position_fill(
        &mut self,
        req: &OrderRequest,
        rest: &OandaRest,
        base: &str,
        acct: &str,
    ) -> Result<Option<&'static str>, String> {
        if req.reduce_only {
            self.ensure(rest, base, acct);
        }
        fill_for(self.0, req)
    }
}

/// The module doc's table, pure: the account's `hedging` answer and the order in, the word (or the refusal) out.
fn fill_for(hedging: Option<bool>, req: &OrderRequest) -> Result<Option<&'static str>, String> {
    if !req.reduce_only {
        return Ok(None);
    }
    match hedging {
        Some(true) => Ok(Some(REDUCE_ONLY_FILL)),
        Some(false) => Ok(None),
        None if rests_at_venue(req) => Ok(None),
        None => Err(format!(
            "oanda: a reduce-only market order on {} is refused: the account's hedging setting is unknown \
             (GET /summary stated no `hedgingEnabled`), and on a hedging account it would OPEN a locked \
             position beside the one it means to close; retry when the venue answers",
            to_oanda_instrument(&req.symbol)
        )),
    }
}

/// `account.hedgingEnabled` of an account summary, when it is a boolean.
fn parse_hedging_enabled(summary: &Value) -> Option<bool> {
    summary.pointer("/account/hedgingEnabled").and_then(Value::as_bool)
}

#[path = "hedging_tests.rs"]
#[cfg(test)]
mod hedging_tests;
