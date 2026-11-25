//! `fold_guards` — what a live snapshot says a fold of STORED fills cannot read (decisions 0209 and 0243): the
//! ledgers that are not the dollar's and the contracts valued off the default. A stored fill names neither its
//! currency nor its contract value, and `vike_analytics::trading_results_table_from_fills` reads every fill as a
//! dollar amount of a linear unit, so a reader of the datahub's fills asks the node's snapshot first and leaves
//! those fills out (`vike_analytics::without_non_dollar_ledgers`, `vike_analytics::without_non_default_contracts`).
//!
//! ONE copy, beside [`crate::observe_decode`] whose [`vike_execution::CoreSnapshot`] it reads: the Account
//! window's Summary (`vike-account-window`) and the live account report (`vike_client::account_report`, behind
//! `vike-cli report account:` and the MCP tool `report`) both call it. It moved here from
//! `vike-account-window` on 2026-11-18, unchanged.

/// **The ledgers of `snapshot` that are not the dollar's**, as `(venue, currency)` pairs, each once (decision
/// 0243). A block's `ledger_currency` is EMPTY
/// for the dollar group, so a node of one currency allocates nothing here.
pub fn off_dollar_ledgers(snapshot: &vike_execution::CoreSnapshot) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    for block in snapshot.portfolio.venues.iter().filter(|b| !b.ledger_currency.is_empty()) {
        let pair = (block.venue.clone(), block.ledger_currency.as_str().to_string());
        if !out.contains(&pair) {
            out.push(pair);
        }
    }
    out
}

/// **The contracts of `snapshot` the fold cannot value**, as `(venue, symbol, value)`, each `(venue, symbol)`
/// once: every row of a block's `contract_values` that is not the default (decision 0243, item 6). A node of
/// linear unit contracts allocates nothing here.
pub fn off_default_contracts(
    snapshot: &vike_execution::CoreSnapshot,
) -> Vec<(String, String, vike_model::ContractValue)> {
    let mut out: Vec<(String, String, vike_model::ContractValue)> = Vec::new();
    for block in &snapshot.portfolio.venues {
        for (symbol, value) in block.contract_values.iter().filter(|(_, v)| !v.is_default()) {
            if !out.iter().any(|(v, s, _)| *v == block.venue && s == symbol) {
                out.push((block.venue.clone(), symbol.clone(), *value));
            }
        }
    }
    out
}

#[path = "fold_guards_tests.rs"]
#[cfg(test)]
mod fold_guards_tests;
