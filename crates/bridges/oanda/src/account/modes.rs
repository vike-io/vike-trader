//! Does the account HEDGE: read once, and what it still decides now that every reduce-only order says so on the
//! wire.
//!
//! # Why it exists
//!
//! `GET /v3/accounts/{id}/summary` states `hedgingEnabled`. Without a word an order's `positionFill` is the venue's
//! `DEFAULT`: `OPEN_ONLY` on a hedging account, so an order against an existing position (a flatten, a market exit, a
//! liquidation, the fire of a protective stop) OPENS a locked opposite position beside the one it meant to close
//! (the failure class of the cTrader row it was measured on, `docs/ops/known-limitations-2026-10.md`), and
//! `REDUCE_FIRST` on a netting account, so a reduce-only order whose position is gone OPENS the other way.
//!
//! # The contract
//!
//! - **The word does not depend on this module.** Every `reduce_only` order the bridge builds, a submit and the
//!   replacement of a replace alike, on every account, carries `"positionFill": "REDUCE_ONLY"` (`crate::execution`'s
//!   `REDUCE_ONLY_FILL`, written by `build_order_body_on`); every other order's body is the one it always was. A
//!   venue that cannot honour the word rejects the order loudly; none is turned into a plain one.
//! - **What the answer still decides**: on a NETTING account a close that waited (`ExecutionClientActorLateCloses`) reads the position
//!   first and goes cut to what still faces it, or is refused by name when nothing does, as it did when no word went
//!   out there. A hedging or an unknown account sends that close as it is, with the word.
//! - **Read once at the start of the exec thread** ([`AccountHedging::ensure`], called by `crate::execution`'s `run_on`
//!   before its first command), on the lookup agent (5 s), and kept for the thread's life. While it is unknown (the
//!   read failed: the venue or the token down, a rate limit, more than the 5 s; or the summary names no boolean)
//!   every reduce-only submit asks again (one bounded read), so a venue that comes back is believed at once.
//!
//! # What is not proven
//!
//! The only practice account measured (account 14, 2026-10-26) says `hedgingEnabled=false`. The `REDUCE_ONLY` word on
//! a resting order met that netting account on 2026-11-09:
//! `crates/bridges/oanda/tests/oanda_smokes/oanda_reduce_only_smoke.rs` ran green (the pending row read back
//! `positionFill` `REDUCE_ONLY`, no reject). A hedging account has not met it; the offline tests hold the bytes.

use serde_json::Value;

use crate::transport::OandaTransport;

const VENUE: &str = "oanda";

/// What the exec thread knows about the account's `hedgingEnabled`. `Default` is "not asked yet" (unknown).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct AccountHedging(Option<bool>);

impl AccountHedging {
    /// An answer already in hand: the test harness's loop, which scripts the order calls only.
    #[cfg(test)]
    pub(crate) const fn stated(hedging: bool) -> Self {
        Self(Some(hedging))
    }

    /// Whether the venue SAID the account nets (`hedgingEnabled=false`): unknown is not netting.
    pub(crate) fn nets(&self) -> bool {
        self.0 == Some(false)
    }

    /// The account's answer when it is not known yet: one bounded `GET /summary`. A failure or an unreadable
    /// summary leaves it unknown (said at `warn`); the account id never reaches the line.
    pub(crate) fn ensure(&mut self, rest: &OandaTransport, base: &str, account: &str) {
        if self.0.is_some() {
            return;
        }
        match rest.get_lookup(base, &format!("/v3/accounts/{account}/summary"), "") {
            Ok(summary) => match parse_hedging_enabled(&summary) {
                Some(hedging) => {
                    self.0 = Some(hedging);
                    tracing::info!(
                        venue = VENUE,
                        hedging_enabled = hedging,
                        "oanda: the account's hedging setting is read; a close that waited reads the \
                         position first only when it is off"
                    );
                }
                None => tracing::warn!(
                    venue = VENUE,
                    "oanda: the account summary states no boolean `hedgingEnabled`; a close that waited \
                     goes as it is, saying REDUCE_ONLY, until it does"
                ),
            },
            Err(e) => tracing::warn!(
                venue = VENUE, error = %e,
                "oanda: could not read the account's hedging setting; a close that waited goes as it is, \
                 saying REDUCE_ONLY, until the venue answers"
            ),
        }
    }
}

/// `account.hedgingEnabled` of an account summary, when it is a boolean.
fn parse_hedging_enabled(summary: &Value) -> Option<bool> {
    summary.pointer("/account/hedgingEnabled").and_then(Value::as_bool)
}

#[path = "modes_tests.rs"]
#[cfg(test)]
mod modes_tests;
