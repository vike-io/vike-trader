//! Whose data a store kind holds — the MARKET's, or this ACCOUNT's.
//!
//! [`StoreKindId`] is the closed vocabulary of stored kinds, and [`StoreKindId::plane`] answers the
//! question by an exhaustive match, no heuristic and no default (owner, 2026-11-21: "I don't want
//! heuristics for such core stuff"). Two-valued: 14 market kinds and 5 account kinds, `equity`
//! among the account kinds, so a retention may not delete it.
//!
//! One question, asked by readers that must not answer differently: the store's delete doors
//! (`crates/vike-data/src/store/removal.rs`'s `plan_removal` and `execute_removal` refuse an account
//! kind), the retention grammar (`crates/vike-config/src/retention.rs`'s
//! `parse_retention_days_by_kind`), the CLI's `data rm` and `data history`
//! (`docs/decisions/0080-the-account-funding-kind-takes-the-qualified-name.md` verdict 3) and the
//! Data Manager. `crates/vike-data/src/store/store_kind.rs`'s `STORE_KINDS` stays the LAYOUT
//! authority (columns, partitioning, commit keys), keyed by this type, and declares no plane.
//!
//! Here because vike-config and vike-data are both layer 20 and cannot name each other: the cure for
//! two sides that must not disagree is a crate BELOW both (root `CLAUDE.md`), and this one is below
//! every reader.
//!
//! ⚠ **What this deliberately does NOT do: decide whether a kind is COLLECTED, or by whom.** It
//! answers one question — whose activity the rows describe — and the two planes differ in what a
//! mistake costs. Deleting a market series costs a re-fetch; deleting an account series costs
//! history no venue will serve twice.

/// The CLOSED vocabulary of stored kinds: one variant per `vike_data::store::store_kind::STORE_KINDS`
/// row (the LAYOUT table, which stays vike-data's and is keyed by this type), and the type every
/// `kind` field of the store is held in. Its plane is [`StoreKindId::plane`], an exhaustive match, so
/// a new kind does not compile until its author decides whose data it holds.
///
/// The ON-DISK spelling is [`StoreKindId::word`] — the `kind=` path segment, the token a commit key
/// interpolates and the string the datahub wire carries — byte-identical to the strings these
/// fields held before the type existed; `Display`, [`std::str::FromStr`] and the `serde` form all
/// spell exactly that. `FromStr` is STRICT ([`StoreKindId::from_word`]): a string no kind declares is
/// a [`StoreKindError`], never a fallback.
///
/// ⚠ **A store kind is read EXACTLY.** Read one through `FromStr` or [`StoreKindId::from_word`]
/// only; `Bar` and `BAR` are refused, as before the type moved here. `word_enum!` also gives the
/// type a case-insensitive [`StoreKindId::parse`], and no reader of a store kind may call it.
///
/// ⚠ **The variants are declared in the ALPHABETICAL order of their spelling, on purpose.** The
/// derived `Ord` is then the order of the strings, so a sorted series listing, a `BTreeMap` keyed by
/// kind and the JSON a coverage report serializes keep the order they had when the field was a
/// `String`. [`StoreKindId::ALL`] is that order too; the order a message names the set in is the
/// layout table's.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum StoreKindId {
    /// `kind=bar` — OHLCV bars, sub-partitioned by interval.
    Bar,
    /// `kind=book` — the lossless L2 event lane.
    Book,
    /// `kind=chain` — option-chain snapshots.
    Chain,
    /// `kind=cohort` — cohort panels.
    Cohort,
    /// `kind=depth` — the conflating L2 snapshot lane.
    Depth,
    /// `kind=equity` — an account's equity samples.
    Equity,
    /// `kind=exec_fill` — an account's own executions.
    ExecFill,
    /// `kind=exec_forecast` — a live mount's published forecasts (book W35).
    ExecForecast,
    /// `kind=exec_funding` — an account's realized funding payments.
    ExecFunding,
    /// `kind=exec_order` — an account's order lifecycle.
    ExecOrder,
    /// `kind=financial_fact` — reported XBRL values (SEC `companyfacts`).
    FinancialFact,
    /// `kind=institutional_holding` — Form 13F information-table lines.
    InstitutionalHolding,
    /// `kind=option_trade` — an options venue's public trade log.
    OptionTrade,
    /// `kind=perp_metrics` — perpetual-contract metrics.
    PerpMetrics,
    /// `kind=properties` — point-in-time `SymbolProperties`.
    Properties,
    /// `kind=quote` — top-of-book quotes.
    Quote,
    /// `kind=text` — news items and economic-calendar events, as this box received them.
    Text,
    /// `kind=text_score` — a host-side text model's numbers for a stored text.
    TextScore,
    /// `kind=trade` — the market print tape.
    Trade,
}

// The `kind=` spellings, the serde tags above. IMPL-ONLY, so the serde attributes stay on the
// literal block; listed in declaration (= spelling) order.
vike_utility::word_enum! {
    impl StoreKindId {
        Bar => "bar",
        Book => "book",
        Chain => "chain",
        Cohort => "cohort",
        Depth => "depth",
        Equity => "equity",
        ExecFill => "exec_fill",
        ExecForecast => "exec_forecast",
        ExecFunding => "exec_funding",
        ExecOrder => "exec_order",
        FinancialFact => "financial_fact",
        InstitutionalHolding => "institutional_holding",
        OptionTrade => "option_trade",
        PerpMetrics => "perp_metrics",
        Properties => "properties",
        Quote => "quote",
        Text => "text",
        TextScore => "text_score",
        Trade => "trade",
    }
}

impl StoreKindId {
    /// The three TICK lanes a tick replay reads, in its load order: `vike_backtest::run::tick_replay`'s
    /// `SeriesKind::Tick` answers its `store_kinds` with exactly this list, and the Studio picker's
    /// tick listing (`crates/vike-studio-core/src/run/slice.rs`'s `tick_series`) filters by it — one
    /// list for the replay and the picker, here because the picker links no harness.
    pub const TICK_LANES: [StoreKindId; 3] =
        [StoreKindId::Quote, StoreKindId::Trade, StoreKindId::Book];

    /// Whose activity this kind's rows describe. EXHAUSTIVE, with no default arm: a new kind does
    /// not compile until its author decides, so no kind can ride a fallback onto the wrong side of
    /// the boundary. Every [`StorePlane::Account`] kind is refused by `data rm`, excluded from
    /// `data history` and refused by a retention (owner, 2026-11-21: retention may not delete `equity`).
    #[must_use]
    pub const fn plane(self) -> StorePlane {
        match self {
            StoreKindId::Bar
            | StoreKindId::Book
            | StoreKindId::Chain
            | StoreKindId::Cohort
            | StoreKindId::Depth
            | StoreKindId::FinancialFact
            | StoreKindId::InstitutionalHolding
            | StoreKindId::OptionTrade
            | StoreKindId::PerpMetrics
            | StoreKindId::Properties
            | StoreKindId::Quote
            | StoreKindId::Text
            | StoreKindId::TextScore
            | StoreKindId::Trade => StorePlane::Market,
            StoreKindId::Equity
            | StoreKindId::ExecFill
            | StoreKindId::ExecForecast
            | StoreKindId::ExecFunding
            | StoreKindId::ExecOrder => StorePlane::Account,
        }
    }
}

impl std::fmt::Display for StoreKindId {
    /// `pad`, not `write_str`: a `{:<8}` around a kind pads exactly as it did around the `String`.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.pad(self.word())
    }
}

impl std::str::FromStr for StoreKindId {
    type Err = StoreKindError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        StoreKindId::from_word(s).ok_or_else(|| StoreKindError { given: s.to_string() })
    }
}

/// A string no [`StoreKindId`] declares, refused by its `FromStr`. Its message names the whole set,
/// in [`StoreKindId::WORDS`]' (spelling) order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoreKindError {
    /// The string that was refused, verbatim.
    pub given: String,
}

impl std::fmt::Display for StoreKindError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "unknown kind {:?}. This store writes: {}",
            self.given,
            StoreKindId::WORDS.join(", ")
        )
    }
}

impl std::error::Error for StoreKindError {}

/// Whose activity a store kind's rows describe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StorePlane {
    /// An instrument's own tape or a metric about the market: bars, quotes, trades, books, depth,
    /// instrument properties, funding RATES, cohort and perp metrics, option chains.
    Market,
    /// THIS account's own activity: what it executed, what it was charged, what it is worth.
    Account,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The class is two-valued and written out: 14 market kinds and the 5 account kinds, `equity`
    /// among them (retention may not delete it). The MARKET funding rate is no kind of its own (it is
    /// `bar` under `interval=funding`); the account's realized payments are `exec_funding`
    /// (`docs/decisions/0080-the-account-funding-kind-takes-the-qualified-name.md`).
    #[test]
    fn every_store_kind_has_its_plane_and_equity_is_account_data() {
        let account: Vec<&str> = StoreKindId::ALL
            .into_iter()
            .filter(|k| k.plane() == StorePlane::Account)
            .map(StoreKindId::word)
            .collect();
        assert_eq!(account, ["equity", "exec_fill", "exec_forecast", "exec_funding", "exec_order"]);
        let market = StoreKindId::ALL.into_iter().filter(|k| k.plane() == StorePlane::Market);
        assert_eq!(market.count(), 14);
        assert_eq!(StoreKindId::Equity.plane(), StorePlane::Account);
        assert_eq!(StoreKindId::from_word("funding"), None, "the market rate is not a kind");
    }

    /// The derived `Ord` is the spelling order: `ALL` is strictly increasing and its words sorted.
    #[test]
    fn the_variant_order_is_the_spelling_order() {
        assert!(StoreKindId::ALL.windows(2).all(|pair| pair[0] < pair[1]));
        assert!(StoreKindId::WORDS.windows(2).all(|pair| pair[0] < pair[1]));
        assert_eq!(StoreKindId::ALL.len(), 19);
    }

    /// `Display` pads like the `String` it replaced, `FromStr` is strict and its refusal names the
    /// whole set.
    #[test]
    fn display_pads_and_from_str_is_strict() {
        assert_eq!(format!("{:<8}|", StoreKindId::Bar), "bar     |");
        assert_eq!("exec_fill".parse::<StoreKindId>(), Ok(StoreKindId::ExecFill));
        let refused = "Bar".parse::<StoreKindId>().expect_err("strict: the word is lowercase");
        assert_eq!(refused.given, "Bar");
        assert!(refused.to_string().contains("bar, book, chain"), "{refused}");
        assert!(StoreKindId::TICK_LANES.into_iter().all(|k| k.plane() == StorePlane::Market));
    }
}
