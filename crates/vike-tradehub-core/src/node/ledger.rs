//! The ledger currencies of the engines one node runs, and the report when they differ.
//!
//! Every engine books its money in its OWN account's currency (decision 0183): an OANDA account
//! told it is in EUR holds EUR figures, a binance account holds USDT. The node's cross-engine totals
//! (`equity_total`, `capital_base`, the margin and realized sums, and the drawdown latch's base and
//! PnL in `vike-core`, the summary line, the Telegram verbs, the desktop header) add those figures
//! with a plain sum and carry no currency (item 13 of the record: *the sums across engines*), so a
//! node that mixes two currencies prints a number that is neither. That cannot be fixed in this
//! crate, because the published block carries no currency; what it can do is never be silent about
//! it. This module says so, once per change, at the two moments the set of engines changes: boot
//! and a hot start.

use vike_exec::{ExecutionClient, ExecutionEngine};

/// The bucket every engine whose currency nobody told it, and every dollar spelling, shares: today's
/// nodes are all in it, so a node with only these engines reports nothing and changes no number.
pub(super) const DEFAULT_LEDGER: &str = "USD";

/// Which currency bucket `engine`'s money is in: [`DEFAULT_LEDGER`] unless the node TOLD it another
/// one (`ExecutionEngine::set_account_ccy`). `USD` and `USDT` are one bucket, as they are everywhere
/// else a dollar ledger is assumed; an engine whose currency is only the constructor's guess is in it
/// too ("engines whose currency is unknown count as the engine default").
pub(super) fn ledger_ccy_key<C: ExecutionClient>(engine: &ExecutionEngine<C>) -> String {
    if !engine.account_ccy_known() {
        return DEFAULT_LEDGER.to_string();
    }
    let code = engine.quote_asset.trim().to_ascii_uppercase();
    match code.as_str() {
        "" | "USD" | "USDT" => DEFAULT_LEDGER.to_string(),
        _ => code,
    }
}

/// The distinct buckets among `keys` (`(route key, bucket)` rows) in first-seen order, or `None` when
/// they are all one: the sentence's list of the currencies a node mixes.
pub(super) fn mixed_ledger_currencies(keys: &[(String, String)]) -> Option<Vec<&str>> {
    let mut seen: Vec<&str> = Vec::new();
    for (_, ccy) in keys {
        if !seen.contains(&ccy.as_str()) {
            seen.push(ccy);
        }
    }
    (seen.len() > 1).then_some(seen)
}

/// Say, at `error`, that this node's engines are in different currencies and its cross-engine totals
/// add them as one unit. Silent for a node whose engines share one bucket (every node today).
pub(super) fn report_mixed_ledger_currencies(keys: &[(String, String)]) {
    let Some(currencies) = mixed_ledger_currencies(keys) else { return };
    let rows = keys
        .iter()
        .map(|(route_key, ccy)| format!("{route_key}={ccy}"))
        .collect::<Vec<_>>()
        .join(", ");
    tracing::error!(
        currencies = %currencies.join(","),
        "this node runs engines whose accounts are in DIFFERENT currencies ({rows}): every engine's own \
         books are right, but the totals ACROSS engines (the published equity total, the capital base, \
         the margin and realized sums, the summary line, the drawdown latch's base and PnL) add the \
         figures as one unit and are not money in any currency (decision 0183, item 13). Read the \
         per-engine blocks"
    );
}

#[path = "ledger_tests.rs"]
#[cfg(test)]
mod ledger_tests;
