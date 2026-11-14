//! **`account_fact`: the named facts an account states about how its engine is mounted** — one row per
//! `(account, fact)`, read WITH the account's row ([`AccountRow::facts`]) and written by
//! [`super::AccountEdit::SetFact`] alone.
//!
//! The owner's ruling of 2026-11-13 (decision 0156's amendment of that date): what the node mounts for an
//! account is the operator's ROW, never a compiled table. The symbol itself is the row's own column
//! (`account.symbol`); the facts that used to be per-venue constants beside it (the lane twin's symbol, the
//! pair a stated currency moves a mount to, IG's fallback epic and its two wire words) are rows of the
//! account they belong to. A TABLE and not columns, because the store is never converted
//! (`docs/decisions/0117-there-are-no-migrations.md`): a new fact is a new word in [`AccountFact`], not a
//! schema change.
//!
//! The value's floor is the symbol's ([`super::normalized_account_symbol`]: one token of printable ASCII, at
//! most [`super::ACCOUNT_SYMBOL_MAX_BYTES`] bytes); what a fact must mean is its reader's, except the lane
//! twin's, which the writer judges (an unlabelled row of a venue with a lane twin, on the twin's lane), because
//! a twin mounted on its parent's lane would put a second signing engine on the parent's market.

use std::collections::BTreeMap;

use rusqlite::Connection;

use super::AccountRow;

/// **One fact an account may state** — the row's `name` is [`Self::as_str`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum AccountFact {
    /// The symbol the account's LANE TWIN's engine is mounted on (`crate::LANE_TWINS`: binance's perpetual
    /// lane), stated on the unlabelled parent row the twin is derived from. It must sit on the twin's lane
    /// (`AccountRowLaneTwin::on_lane`); a twin whose parent states none is derived NOT RUNNING
    /// (`vike_model::ArmingBlock::NoAccountSymbol`).
    LaneTwinSymbol,
    /// The pair the account's engine is started again on once the venue states a currency its own symbol
    /// does not count in (fxcm's pound account: `EURGBP`), read by the tradehub's start
    /// (`vike-tradehub`'s `currency_mount`). The pair must count in that currency, or nothing moves.
    CurrencySymbol,
    /// The symbol whose expiry and dealing currency the account states in the two facts below, used when the
    /// venue's own read of them fails (IG's market read: `vike_ig`'s order and close paths).
    FallbackSymbol,
    /// The expiry the venue's wire writes for [`Self::FallbackSymbol`] when its read fails (IG: `-`, undated).
    FallbackSymbolExpiry,
    /// The dealing currency the venue's wire writes for [`Self::FallbackSymbol`] when its read fails (IG: `USD`).
    FallbackSymbolCurrency,
}

impl AccountFact {
    /// Every fact, in declaration order: the words a writer accepts.
    pub const ALL: [AccountFact; 5] = [
        AccountFact::LaneTwinSymbol,
        AccountFact::CurrencySymbol,
        AccountFact::FallbackSymbol,
        AccountFact::FallbackSymbolExpiry,
        AccountFact::FallbackSymbolCurrency,
    ];

    /// The row's `name`: a stored word, never renamed.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            AccountFact::LaneTwinSymbol => "lane_twin_symbol",
            AccountFact::CurrencySymbol => "currency_symbol",
            AccountFact::FallbackSymbol => "fallback_symbol",
            AccountFact::FallbackSymbolExpiry => "fallback_symbol_expiry",
            AccountFact::FallbackSymbolCurrency => "fallback_symbol_currency",
        }
    }

    /// The fact a word names, or `None` for a word this build does not know.
    #[must_use]
    pub fn parse(word: &str) -> Option<AccountFact> {
        AccountFact::ALL.into_iter().find(|fact| fact.as_str() == word)
    }
}

impl AccountRow {
    /// **What this row states for `fact`**, or `None` when it states nothing.
    #[must_use]
    pub fn fact(&self, fact: AccountFact) -> Option<&str> {
        self.facts.get(fact.as_str()).map(String::as_str)
    }
}

/// **Fold the `account_fact` rows into `rows`** (each row's [`AccountRow::facts`], by its id). A store with no
/// `account_fact` table (one no writer of this build has carried) states no fact: a probe of the store's SHAPE,
/// as `account_select`'s, never a conversion. A word this build does not know is read as stated, and no reader
/// asks for it.
pub(super) fn load_facts(conn: &Connection, rows: &mut [AccountRow]) -> rusqlite::Result<()> {
    if rows.is_empty() || !crate::settings::has_column(conn, "account_fact", "value")? {
        return Ok(());
    }
    let mut by_id: BTreeMap<i64, BTreeMap<String, String>> = BTreeMap::new();
    let mut stmt = conn.prepare("SELECT account_id, name, value FROM account_fact")?;
    let mut read = stmt.query([])?;
    while let Some(r) = read.next()? {
        by_id.entry(r.get(0)?).or_default().insert(r.get(1)?, r.get(2)?);
    }
    for row in rows {
        if let Some(facts) = by_id.get(&row.id) {
            row.facts.clone_from(facts);
        }
    }
    Ok(())
}
