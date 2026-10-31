//! WHICH of a login's IG accounts a session trades on: the rule, pure, over a `GET /accounts` body.
//!
//! One IG login can hold several accounts (a spread-bet and a CFD one, one per currency), and `POST /session`
//! lands on whichever IG made the DEFAULT. This adapter trades a **USD CFD** account by default: the mounted epic
//! is quoted in [`WIRE_CURRENCY`], and what `size` means is measured on CFD epics alone (`crate::market_facts`).
//! A CFD account in ANOTHER currency (pound, euro, ...) is traded only when the operator NAMES it as the book
//! (rule 1 below): the node then converts for it under a `demo` exposure ceiling (the ledger is the account's own
//! money, decision 0183's mounted-symbol amendment) and, with no ceiling, runs it the old way (the seed read as
//! dollars), which is why the default never moves onto one by itself. Before this module the session simply
//! stayed on the default, so a login whose default was a spread-bet or a pound account traded on it, and the
//! only operator-visible consequence was the node refusing to run it once an exposure ceiling was armed.
//!
//! # The rule ([`pick`])
//!
//! 1. **The account row's `venue_account_id` wins** (the BOOK the operator named, `vike-cli secrets
//!    set-book`): the session is put on it, whatever its type and currency (the row says so: a spread-bet
//!    book is the operator's word, and the facts fences still judge each epic on it). A book the login does
//!    not list, or one IG marks other than `ENABLED`, is a refusal by name.
//! 2. **Else the default stays when nothing is positively wrong with it**: no switch, no extra request, the
//!    behaviour every login had until now. "Wrong" is a STATED fact: a type other than `CFD`, a currency other
//!    than USD, a status other than `ENABLED`. A field IG did not state is not a verdict (the owner's ruling of
//!    2026-10-19 reads an unstated currency as USD).
//! 3. **Else the one other account that is positively an enabled USD CFD one** (type AND currency stated).
//! 4. **Else a refusal by name** that lists every account found with its type, currency and status, and says
//!    what to write: none fit, or several do and nothing says which.
//!
//! The switch ([`crate::rest::IgSession`]'s `login_once`) is `PUT /session` with `defaultAccount: false`, so it
//! moves THIS session only and never the login's default under another session.

use serde_json::Value;

use crate::market_facts::WIRE_CURRENCY;

/// The `accountType` this adapter trades.
const WANTED_KIND: &str = "CFD";

/// The `status` of an account IG lets deal.
const ENABLED: &str = "ENABLED";

/// One row of `GET /accounts` (v1), as far as the choice reads it. Everything but the id is optional: a field IG
/// did not state is not a verdict (module doc).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ListedAccount {
    pub(crate) id: String,
    /// `accountType` (`CFD`, `SPREADBET`, `PHYSICAL`), upper-case.
    pub(crate) kind: Option<String>,
    /// `currency`, upper-case, three letters; anything else reads as not stated.
    pub(crate) currency: Option<String>,
    /// `status` (`ENABLED`, `DISABLED`, `SUSPENDED_FROM_DEALING`), upper-case.
    pub(crate) status: Option<String>,
    pub(crate) preferred: bool,
}

impl ListedAccount {
    fn enabled(&self) -> bool {
        self.status.as_deref().is_none_or(|s| s == ENABLED)
    }

    /// Not positively wrong: no STATED type, currency or status that rules it out.
    fn acceptable(&self) -> bool {
        self.enabled()
            && self.kind.as_deref().is_none_or(|k| k == WANTED_KIND)
            && self.currency.as_deref().is_none_or(|c| c == WIRE_CURRENCY)
    }

    /// Positively an enabled USD CFD account: the type AND the currency are stated and right. What an account
    /// must be for the session to be MOVED onto it.
    fn target(&self) -> bool {
        self.enabled()
            && self.kind.as_deref() == Some(WANTED_KIND)
            && self.currency.as_deref() == Some(WIRE_CURRENCY)
    }

    /// `` `ABC12` CFD USD ENABLED (preferred) `` — what the refusal lists and the log names.
    pub(crate) fn words(&self) -> String {
        format!(
            "`{}` {} {} {}{}",
            self.id,
            self.kind.as_deref().unwrap_or("type not stated"),
            self.currency.as_deref().unwrap_or("currency not stated"),
            self.status.as_deref().unwrap_or("status not stated"),
            if self.preferred { " (preferred)" } else { "" }
        )
    }
}

/// What the session does about its account.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Pick {
    /// The session's account is the one to trade: nothing is sent.
    Stay,
    /// Move this session to that account id (`PUT /session`).
    Switch(String),
}

/// The rows of a `GET /accounts` body. A row without a string `accountId` is no account and is skipped.
pub(crate) fn listed_accounts(body: &Value) -> Vec<ListedAccount> {
    let upper = |row: &Value, key: &str| {
        row.get(key)
            .and_then(Value::as_str)
            .map(|t| t.trim().to_ascii_uppercase())
            .filter(|t| !t.is_empty())
    };
    body.get("accounts")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|row| {
            let id = row.get("accountId").and_then(Value::as_str)?.trim().to_string();
            (!id.is_empty()).then(|| ListedAccount {
                id,
                kind: upper(row, "accountType"),
                currency: upper(row, "currency")
                    .filter(|c| c.len() == 3 && c.bytes().all(|b| b.is_ascii_alphabetic())),
                status: upper(row, "status"),
                preferred: row.get("preferred").and_then(Value::as_bool).unwrap_or(false),
            })
        })
        .collect()
}

/// Every account in the list, for a refusal.
fn listing(rows: &[ListedAccount]) -> String {
    if rows.is_empty() {
        return "none (GET /accounts listed no account)".to_string();
    }
    rows.iter().map(ListedAccount::words).collect::<Vec<_>>().join("; ")
}

/// The sentence every refusal ends in: where the operator's word goes.
const WHAT_TO_WRITE: &str = "To name the account to trade, write its id as the account row's book: \
     `vike-cli secrets set-book --id <ACCOUNT ROW> --venue-account-id <ACCOUNT ID>`, then set \
     `account.active` off and on";

/// What the session does about its account: see the module doc for the rule. `current` is the session's own
/// account (`currentAccountId` of `POST /session`), `pin` the account row's `venue_account_id`.
///
/// # Errors
/// The refusal, in words: the accounts found with their type, currency and status, and what to write.
pub(crate) fn pick(
    rows: &[ListedAccount],
    current: &str,
    pin: Option<&str>,
) -> Result<Pick, String> {
    if let Some(pin) = pin.map(str::trim).filter(|p| !p.is_empty()) {
        let Some(row) = rows.iter().find(|r| r.id.eq_ignore_ascii_case(pin)) else {
            return Err(format!(
                "ig: the account row names the book `{pin}`, and this login lists no such account. \
                 Accounts found: {}. {WHAT_TO_WRITE}",
                listing(rows)
            ));
        };
        if !row.enabled() {
            return Err(format!(
                "ig: the account row names the book {}, and IG does not let it deal. Accounts found: \
                 {}. {WHAT_TO_WRITE}",
                row.words(),
                listing(rows)
            ));
        }
        return Ok(if row.id.eq_ignore_ascii_case(current) {
            Pick::Stay
        } else {
            Pick::Switch(row.id.clone())
        });
    }
    // The session's own account is not in the list (or the list is empty): nothing to judge it by.
    let here = rows.iter().find(|r| r.id.eq_ignore_ascii_case(current));
    if here.is_none_or(ListedAccount::acceptable) {
        return Ok(Pick::Stay);
    }
    let mut targets = rows.iter().filter(|r| r.target());
    match (targets.next(), targets.next()) {
        (Some(only), None) => Ok(Pick::Switch(only.id.clone())),
        (None, _) => Err(format!(
            "ig: this login's default account is {}, and none of its accounts is an enabled {WANTED_KIND} \
             account in {WIRE_CURRENCY}, which is what this adapter trades unless told otherwise (what a \
             deal's size means is measured on {WANTED_KIND} epics only). Accounts found: {}. Open or enable \
             one in IG's platform, or name a {WANTED_KIND} account in another currency as the book: it is \
             then converted for under a `demo` exposure ceiling. {WHAT_TO_WRITE}",
            here.map_or_else(|| format!("`{current}`"), ListedAccount::words),
            listing(rows)
        )),
        (Some(_), Some(_)) => Err(format!(
            "ig: this login's default account is {}, which this adapter does not trade, and SEVERAL \
             of its accounts are enabled {WANTED_KIND} accounts in {WIRE_CURRENCY}, so nothing says \
             which one. Accounts found: {}. {WHAT_TO_WRITE}",
            here.map_or_else(|| format!("`{current}`"), ListedAccount::words),
            listing(rows)
        )),
    }
}

#[path = "account_pick_tests.rs"]
#[cfg(test)]
mod account_pick_tests;
