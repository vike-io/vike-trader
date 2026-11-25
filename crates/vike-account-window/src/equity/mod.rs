//! Qt-free view-model for the cross-venue equity panel (PR-2 T4). The Account window
//! ([`hero`]'s header, [`crate::accounts`]' table) renders it; it was vike-app-core's
//! `orders::equity_panel` until the window moved into this crate (top layers, wave 2). It is CI-tested because the shell's own code runs under
//! no test beyond a few pins (see vike-app-core's crate-level doc).
//!
//! Pure mapping only: one [`AccountWindowAccountsRow`] per `CoreSnapshot::portfolio` venue entry, in order;
//! `equity_total`/`missing_prices_total` copied straight from the snapshot. No egui, no I/O —
//! the GUI owns all number formatting, the only rendering choice made here is the short `mode`
//! tag.

pub mod hero;

use vike_execution::{AccountLedgerTotals, BalanceMode, CoreSnapshot, Interned};
use vike_model::AccountTier;
use vike_model::accounts::account_keys::UNLABELLED_ACCOUNT_NAME;

/// One venue's row in the cross-venue equity panel — a flattened, display-ready slice of a
/// `VenueBlock`.
#[derive(Debug, Clone, PartialEq)]
pub struct AccountWindowAccountsRow {
    pub venue: String,
    /// WHICH ACCOUNT of `venue` this row is (`VenueBlock::account`): its label, or `main` for the
    /// unlabelled one. ⚠ `venue` alone cannot tell two accounts of one exchange apart (a node that runs
    /// a demo and a LIVE bybit lists two blocks named `bybit`), so a row without it reads like its twin.
    pub account: String,
    /// The engine's key on the node (`VenueBlock::route_key`: `bybit`, `bybit#LIVE`): what finds this row's
    /// OWN block again. ⚠ Never look a block up by `venue`; the first of two accounts of one exchange answers.
    pub route_key: String,
    /// The tier of that account as the node said it (`VenueBlock::tier`). `None` is "not said" (a node
    /// that predates the report), drawn `MODE ?` and never as PAPER.
    pub tier: Option<AccountTier>,
    pub equity: f64,
    pub realized: f64,
    pub unrealized: f64,
    pub fees: f64,
    pub funding: f64,
    /// short display tag for `BalanceMode`: "\u{0394}" (Delta) / "auth" (Authoritative).
    pub mode: String,
    pub missing_prices: u32,
    /// The row's currency group (`VenueBlock::ledger_currency`): EMPTY for the dollar group. Every figure
    /// above is in it, so only rows of one group may be added together ([`AccountWindowEquityModel`]).
    pub currency: Interned,
}

/// One currency group's headline, for every group but the primary's: what the panel shows BESIDE the
/// primary's totals instead of adding them (a node whose engines are not all in one currency;
/// `Portfolio::ledger_totals`). Absent from a node of one currency.
#[derive(Debug, Clone, PartialEq)]
pub struct AccountWindowEquityLedger {
    /// The group's code for a label (`"EUR"`; the dollar group reads `"USD"`).
    pub currency: &'static str,
    pub equity_total: f64,
    pub realized: f64,
    pub fees: f64,
    pub funding: f64,
    pub margin_used: f64,
}

impl AccountWindowEquityLedger {
    /// The headline of one group's fold.
    fn of(t: AccountLedgerTotals) -> Self {
        AccountWindowEquityLedger {
            currency: t.label(),
            equity_total: t.equity,
            realized: t.realized_pnl,
            fees: t.fees_paid,
            funding: t.funding_paid,
            margin_used: t.margin_used,
        }
    }
}

/// One tier's accounts, as the Account window's header shows them (the owner, 2026-11-17): live, demo and paper
/// money are never one figure, so each tier is its own group, with one line per currency.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct AccountWindowEquityTier {
    /// The tier as the node said it (`VenueBlock::tier`). `None` holds the accounts whose tier it did not say
    /// beside accounts whose tier it did (a word this build does not know): kept apart from every tier.
    pub(crate) tier: Option<AccountTier>,
    /// How many accounts (blocks) the group adds.
    pub(crate) accounts: usize,
    /// One line per currency group its accounts are in, the primary's first.
    pub(crate) ledgers: Vec<AccountWindowEquityLedger>,
    /// Its accounts' positions with no price.
    pub(crate) missing_prices: u32,
}

/// The order the header lists the tiers in: LIVE first, the one that spends real money; a tier not said last.
const TIER_ORDER: [Option<AccountTier>; 4] =
    [Some(AccountTier::Live), Some(AccountTier::Demo), Some(AccountTier::Paper), None];

/// One conversion rate behind an account's figures (`VenueBlock::quote_rates`, decision 0183), as the panel
/// lists it beside the Accounts table: the figure in the ONE wording every surface uses, and what the window
/// colours by. Absent for an account that holds no symbol quoted in another currency.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct AccountWindowAccountsRate {
    /// The account's engine key on the node (`VenueBlock::route_key`; the bare venue off a node that sends none).
    pub(crate) account: String,
    /// `1 JPY = 0.006667 USD`, then ` STALE` and what that means when a position is open in the currency
    /// (`vike_tradehub_client::wire::quote_rate_phrase`).
    pub(crate) phrase: String,
    /// The node's verdict that the rate is past its staleness window.
    pub(crate) stale: bool,
}

/// The whole cross-venue equity panel: the headline (`equity_total`/`missing_prices_total`)
/// plus one [`AccountWindowAccountsRow`] per venue, in `CoreSnapshot::venues` order.
///
/// ⚠ `equity_total` is ONE currency: the PRIMARY block's group (`Portfolio::equity_total`), and
/// `currency` names it. The other groups are [`Self::other_ledgers`], EMPTY for a node whose engines share
/// a currency, so such a panel reads as it always did. Never add a row of one group to another's.
#[derive(Debug, Clone, PartialEq)]
pub struct AccountWindowEquityModel {
    pub equity_total: f64,
    pub missing_prices_total: u32,
    pub rows: Vec<AccountWindowAccountsRow>,
    /// The primary group's code (`"USD"` for the dollar group).
    pub currency: &'static str,
    /// The primary group's key (`Portfolio::primary_currency`, EMPTY for the dollar group): the node's own, read
    /// off the frame's stated key and never off the first row (after a primary restart the first row can be
    /// another engine's). The rows whose `currency` is this are the ones a strip adds together.
    pub primary: Interned,
    /// Every other currency group's headline, first-seen order.
    pub other_ledgers: Vec<AccountWindowEquityLedger>,
    /// Every account's conversion rates, in block order. EMPTY for a node with no account that holds a symbol
    /// quoted in another currency (the window then draws nothing for it).
    pub(crate) rates: Vec<AccountWindowAccountsRate>,
    /// One group per tier that has an account, LIVE, DEMO, PAPER, then any tier not said. EMPTY when no block says
    /// its tier (a node older than the report, or no account): the window then draws the one total with a note.
    pub(crate) tiers: Vec<AccountWindowEquityTier>,
}

/// WHICH account of its venue `block` is, in the word a row says: its label, or `main` for the
/// unlabelled one. The Accounts row and the Positions row both say it this way.
pub(crate) fn account_word(block: &vike_execution::VenueBlock) -> &str {
    block.account.as_ref().and_then(|a| a.text()).unwrap_or(UNLABELLED_ACCOUNT_NAME)
}

impl AccountWindowEquityModel {
    /// Shape a `CoreSnapshot` into the panel model: one `AccountWindowAccountsRow` per `VenueBlock` in order;
    /// `equity_total`/`missing_prices_total` copied straight from the snapshot. Pure — no I/O,
    /// no egui, no formatting beyond the `mode` tag (the GUI formats every number).
    pub fn from_snapshot(snapshot: &CoreSnapshot) -> Self {
        let rows = snapshot
            .portfolio
            .venues
            .iter()
            .map(|v| AccountWindowAccountsRow {
                venue: v.venue.clone(),
                account: account_word(v).to_string(),
                route_key: v.route_key.clone(),
                tier: v.tier,
                equity: v.equity,
                realized: v.realized_pnl,
                unrealized: v.unrealized,
                fees: v.fees_paid,
                funding: v.funding_paid,
                mode: match v.balance_mode {
                    BalanceMode::Delta => "\u{0394}".to_string(),
                    BalanceMode::Authoritative => "auth".to_string(),
                },
                missing_prices: v.missing_prices,
                currency: v.ledger_currency,
            })
            .collect();
        // One group (every node today) builds nothing here: `ledger_totals` allocates only for a
        // panel with several, and this is skipped for the common one by the `is_multi_currency` test.
        let other_ledgers = if snapshot.portfolio.is_multi_currency() {
            snapshot
                .portfolio
                .ledger_totals()
                .into_iter()
                .skip(1)
                .map(AccountWindowEquityLedger::of)
                .collect()
        } else {
            Vec::new()
        };
        let rates = snapshot
            .portfolio
            .venues
            .iter()
            .flat_map(|v| {
                let account = if v.route_key.is_empty() { &v.venue } else { &v.route_key };
                v.quote_rates.iter().map(move |r| AccountWindowAccountsRate {
                    account: account.clone(),
                    phrase: vike_tradehub_client::wire::quote_rate_phrase(
                        &r.currency,
                        r.rate,
                        r.stale,
                        r.held,
                        &v.quote_rates_in,
                    ),
                    stale: r.stale,
                })
            })
            .collect();
        let venues = &snapshot.portfolio.venues;
        let tiers = if venues.iter().any(|v| v.tier.is_some()) {
            let currencies = snapshot.portfolio.ledger_currencies();
            TIER_ORDER
                .into_iter()
                .filter_map(|tier| {
                    let of_tier = || venues.iter().filter(move |v| v.tier == tier);
                    let accounts = of_tier().count();
                    let ledgers = currencies
                        .iter()
                        .filter(|&&c| of_tier().any(|v| v.ledger_currency == c))
                        .map(|&c| {
                            let blocks = of_tier().filter(move |v| v.ledger_currency == c);
                            AccountWindowEquityLedger::of(AccountLedgerTotals::of(c, blocks))
                        })
                        .collect();
                    let missing_prices = of_tier().map(|v| v.missing_prices).sum();
                    (accounts > 0).then_some(AccountWindowEquityTier {
                        tier,
                        accounts,
                        ledgers,
                        missing_prices,
                    })
                })
                .collect()
        } else {
            Vec::new()
        };
        AccountWindowEquityModel {
            equity_total: snapshot.portfolio.equity_total,
            missing_prices_total: snapshot.portfolio.missing_prices_total,
            rows,
            currency: vike_model::ledger_label(snapshot.portfolio.primary_currency().as_str()),
            primary: snapshot.portfolio.primary_currency(),
            other_ledgers,
            rates,
            tiers,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vike_execution::{BalanceMode, CoreSnapshot, VenueBlock};
    use vike_model::TradingState;

    /// A zero-valued `VenueBlock` for `venue`, Delta mode, no positions — struct-update base
    /// for tests that only care about a subset of fields.
    fn base_venue_block(venue: &str) -> VenueBlock {
        VenueBlock {
            account_code: None,
            order_client_id_shape: None,
            venue: venue.to_string(),
            account: None,
            route_key: venue.to_string(),
            symbol: String::new(),
            extra_symbols: Default::default(),
            tier: None,
            balance: 0.0,
            realized_pnl: 0.0,
            fees_paid: 0.0,
            funding_paid: 0.0,
            balance_mode: BalanceMode::Delta,
            contract_values: Default::default(),
            contract_value_default: vike_model::ContractValue::default(),
            admits_symbols: false,
            quote_rates: Vec::new(),
            quote_rates_in: Default::default(),
            ledger_currency: Default::default(),
            currency_assumed: false,
            colliding_fills_refused: 0,
            fees_unconverted: 0,
            fees_at_stale_rate: 0,
            equity: 0.0,
            unrealized: 0.0,
            missing_prices: 0,
            margin_used: 0.0,
            free_buying_power: 0.0,
            margin_ratio: 0.0,
            fee_schedule: None,
            trading_state: TradingState::Active,
            positions: Vec::new(),
        }
    }

    #[test]
    fn panel_model_maps_venues_in_order() {
        let mut snapshot = CoreSnapshot::empty("binance", "BTC");
        snapshot.portfolio.equity_total = 12_345.678;
        snapshot.portfolio.missing_prices_total = 3;
        snapshot.portfolio.venues = vec![
            VenueBlock { equity: 1_100.0, unrealized: 100.0, ..base_venue_block("binance") },
            VenueBlock {
                equity: 2_200.0,
                unrealized: 200.0,
                missing_prices: 2,
                balance_mode: BalanceMode::Authoritative,
                ..base_venue_block("bybit")
            },
        ];

        let m = AccountWindowEquityModel::from_snapshot(&snapshot);

        assert_eq!(m.rows.len(), 2);
        assert_eq!(m.rows[0].venue, "binance");
        assert_eq!(m.equity_total.to_bits(), snapshot.portfolio.equity_total.to_bits());
        assert_eq!(m.missing_prices_total, snapshot.portfolio.missing_prices_total);
        assert_eq!(m.rows[1].unrealized, snapshot.portfolio.venues[1].unrealized);
    }

    #[test]
    fn equity_row_maps_every_field_without_transposition() {
        // Distinct values per field so a swapped mapping (e.g. fees<->funding) fails loudly.
        let mut snapshot = CoreSnapshot::empty("okx", "BTC-PERP");
        snapshot.portfolio.venues = vec![VenueBlock {
            account_code: None,
            order_client_id_shape: None,
            venue: "okx".to_string(),
            account: None,
            route_key: "okx".to_string(),
            symbol: String::new(),
            extra_symbols: Default::default(),
            tier: None,
            balance: 500.0,
            realized_pnl: 7.0,
            fees_paid: 3.0,
            funding_paid: 4.0,
            balance_mode: BalanceMode::Delta,
            contract_values: Default::default(),
            contract_value_default: vike_model::ContractValue::default(),
            admits_symbols: false,
            quote_rates: Vec::new(),
            quote_rates_in: Default::default(),
            ledger_currency: Default::default(),
            currency_assumed: false,
            colliding_fills_refused: 0,
            fees_unconverted: 0,
            fees_at_stale_rate: 0,
            equity: 9_000.0,
            unrealized: 8.0,
            missing_prices: 5,
            margin_used: 0.0,
            free_buying_power: 0.0,
            margin_ratio: 0.0,
            fee_schedule: None,
            trading_state: TradingState::Active,
            positions: Vec::new(),
        }];

        let row = &AccountWindowEquityModel::from_snapshot(&snapshot).rows[0];

        assert_eq!(row.venue, "okx");
        assert_eq!(row.equity, 9_000.0);
        assert_eq!(row.realized, 7.0);
        assert_eq!(row.unrealized, 8.0);
        assert_eq!(row.fees, 3.0);
        assert_eq!(row.funding, 4.0);
        assert_eq!(row.missing_prices, 5);
    }

    /// ⚠ Two blocks named `bybit` (a demo account and a LIVE one) are two rows that say WHICH account
    /// and which tier they are; a node that did not say its tier leaves `None`, never PAPER.
    #[test]
    fn two_accounts_of_one_venue_are_told_apart_by_account_and_tier() {
        let live = vike_model::accounts::account_keys::parse_wire_account("LIVE").expect("a label");
        let mut snapshot = CoreSnapshot::empty("bybit", "BTC");
        snapshot.portfolio.venues = vec![
            VenueBlock { tier: Some(AccountTier::Demo), ..base_venue_block("bybit") },
            VenueBlock {
                account: Some(live),
                route_key: "bybit#LIVE".to_string(),
                tier: Some(AccountTier::Live),
                ..base_venue_block("bybit")
            },
            base_venue_block("okx"),
        ];
        let rows = AccountWindowEquityModel::from_snapshot(&snapshot).rows;
        let said = |i: usize| (rows[i].account.as_str(), rows[i].tier);
        assert_eq!(said(0), ("main", Some(AccountTier::Demo)));
        assert_eq!(said(1), ("LIVE", Some(AccountTier::Live)));
        assert_eq!(said(2), ("main", None), "a tier the node did not say stays unsaid");
    }

    #[test]
    fn mode_tag_renders_short_strings() {
        let mut delta_snapshot = CoreSnapshot::empty("binance", "BTC");
        delta_snapshot.portfolio.venues =
            vec![VenueBlock { balance_mode: BalanceMode::Delta, ..base_venue_block("binance") }];
        let mut auth_snapshot = CoreSnapshot::empty("bybit", "BTC");
        auth_snapshot.portfolio.venues = vec![VenueBlock {
            balance_mode: BalanceMode::Authoritative,
            ..base_venue_block("bybit")
        }];

        assert_eq!(
            AccountWindowEquityModel::from_snapshot(&delta_snapshot).rows[0].mode,
            "\u{0394}"
        );
        assert_eq!(AccountWindowEquityModel::from_snapshot(&auth_snapshot).rows[0].mode, "auth");
    }

    #[test]
    fn empty_venues_yields_empty_rows_but_keeps_totals() {
        let mut snapshot = CoreSnapshot::empty("binance", "BTC");
        snapshot.portfolio.equity_total = 42.0;
        snapshot.portfolio.missing_prices_total = 7;

        let m = AccountWindowEquityModel::from_snapshot(&snapshot);

        assert!(m.rows.is_empty());
        assert_eq!(m.equity_total, 42.0);
        assert_eq!(m.missing_prices_total, 7);
    }

    /// A node with a EUR account beside a dollar one: the headline is the dollar group's alone and the
    /// EUR group is its own headline, in its own code. Nothing is added across the two.
    #[test]
    fn a_second_currency_is_a_headline_beside_the_primary_not_part_of_it() {
        let mut snapshot = CoreSnapshot::empty("binance", "BTC");
        snapshot.portfolio.equity_total = 1_100.0; // the dollar group's, as `snapshot::build` binds it
        snapshot.portfolio.venues = vec![
            VenueBlock { equity: 1_100.0, realized_pnl: 5.0, ..base_venue_block("binance") },
            VenueBlock {
                equity: 7_000.0,
                realized_pnl: -300.0,
                fees_paid: 4.5,
                funding_paid: 0.25,
                margin_used: 120.0,
                ledger_currency: "EUR".into(),
                ..base_venue_block("oanda")
            },
        ];
        let m = AccountWindowEquityModel::from_snapshot(&snapshot);
        assert_eq!(m.currency, "USD");
        assert_eq!(m.equity_total, 1_100.0);
        assert_eq!(m.rows[0].currency, "");
        assert_eq!(m.rows[1].currency, "EUR");
        assert_eq!(
            m.other_ledgers,
            vec![AccountWindowEquityLedger {
                currency: "EUR",
                equity_total: 7_000.0,
                realized: -300.0,
                fees: 4.5,
                funding: 0.25,
                margin_used: 120.0,
            }]
        );
    }

    /// A node of one currency, and a block from an old peer that states none (the wire's absent key is
    /// the empty group): no other headline, and the panel is what it was.
    #[test]
    fn one_currency_has_no_other_headline() {
        let mut snapshot = CoreSnapshot::empty("binance", "BTC");
        snapshot.portfolio.venues = vec![base_venue_block("binance"), base_venue_block("bybit")];
        let m = AccountWindowEquityModel::from_snapshot(&snapshot);
        assert!(m.other_ledgers.is_empty());
        assert_eq!(m.currency, "USD");
        assert!(m.rates.is_empty(), "no account holds a foreign-quoted symbol: nothing to list");
    }

    /// **The conversion rates behind an account's figures are listed, and a stale one says whether it values a
    /// position** (decision 0183): the window draws what this model holds and nothing it computes itself, so a
    /// rate that is stale under an open position reads the same here, in `trade equity` and in Telegram.
    #[test]
    fn a_stale_rate_under_a_position_is_listed_in_the_one_wording() {
        let mut snapshot = CoreSnapshot::empty("binance", "BTC");
        snapshot.portfolio.venues = vec![
            base_venue_block("binance"),
            VenueBlock {
                route_key: "fxcm".into(),
                ledger_currency: "EUR".into(),
                quote_rates_in: "EUR".into(),
                quote_rates: vec![
                    vike_execution::QuoteRateView {
                        currency: "USD".into(),
                        rate: 0.862_069,
                        as_of_ms: 1,
                        stale: true,
                        held: true,
                    },
                    vike_execution::QuoteRateView {
                        currency: "JPY".into(),
                        rate: 0.005_747,
                        as_of_ms: 1,
                        stale: false,
                        held: false,
                    },
                ],
                ..base_venue_block("fxcm")
            },
        ];
        let m = AccountWindowEquityModel::from_snapshot(&snapshot);
        assert_eq!(
            m.rates,
            vec![
                AccountWindowAccountsRate {
                    account: "fxcm".into(),
                    phrase: "1 USD = 0.862069 EUR STALE (an open position is valued at it)".into(),
                    stale: true,
                },
                AccountWindowAccountsRate {
                    account: "fxcm".into(),
                    phrase: "1 JPY = 0.005747 EUR".into(),
                    stale: false,
                },
            ]
        );
    }

    /// One account of `tier` in `currency` (empty = dollars) with its figures; `key` is its engine key.
    fn tiered(
        venue: &str,
        key: &str,
        tier: Option<AccountTier>,
        currency: &str,
        [equity, realized_pnl, fees_paid, funding_paid]: [f64; 4],
    ) -> VenueBlock {
        VenueBlock {
            route_key: key.to_string(),
            tier,
            ledger_currency: currency.into(),
            equity,
            realized_pnl,
            fees_paid,
            funding_paid,
            ..base_venue_block(venue)
        }
    }

    fn headline(
        currency: &'static str,
        [equity_total, realized, fees, funding]: [f64; 4],
    ) -> AccountWindowEquityLedger {
        AccountWindowEquityLedger {
            currency,
            equity_total,
            realized,
            fees,
            funding,
            margin_used: 0.0,
        }
    }

    /// ⚠ **Live, demo and paper money are three groups, never one figure** (the owner, 2026-11-17): 2 live,
    /// 3 demo and 4 paper accounts over dollars and euros give LIVE, DEMO, PAPER in that order, each with its
    /// own count and its own line per currency, the primary's first.
    #[test]
    fn each_tier_is_its_own_group_with_its_own_sums_and_count() {
        let (l, d, p) =
            (Some(AccountTier::Live), Some(AccountTier::Demo), Some(AccountTier::Paper));
        let mut snapshot = CoreSnapshot::empty("bybit", "BTC");
        snapshot.portfolio.venues = vec![
            tiered("bybit", "bybit#LIVE", l, "", [1_000.0, 10.0, 1.0, 0.5]),
            tiered("binance", "binance", d, "", [5_000.0, 1.0, 0.25, -0.5]),
            tiered("fxcm", "fxcm#P1", p, "", [100_000.0, 0.0, 0.0, 0.0]),
            tiered("oanda", "oanda#LIVE", l, "EUR", [200.0, -4.0, 0.5, 0.0]),
            tiered("okx", "okx", d, "", [3_000.0, 2.0, 0.5, 0.25]),
            tiered("fxcm", "fxcm#P2", p, "", [100_000.0, 0.0, 0.0, 0.0]),
            tiered("oanda", "oanda", d, "EUR", [7_000.0, -1.0, 0.0, 0.0]),
            tiered("fxcm", "fxcm#P3", p, "", [100_000.0, 0.0, 0.0, 0.0]),
            tiered("oanda", "oanda#P4", p, "EUR", [100_000.0, 0.0, 0.0, 0.0]),
        ];
        let tiers = AccountWindowEquityModel::from_snapshot(&snapshot).tiers;
        let said: Vec<_> = tiers.iter().map(|g| (g.tier, g.accounts)).collect();
        assert_eq!(said, [(l, 2), (d, 3), (p, 4)]);
        assert_eq!(
            tiers[0].ledgers,
            [headline("USD", [1_000.0, 10.0, 1.0, 0.5]), headline("EUR", [200.0, -4.0, 0.5, 0.0])]
        );
        assert_eq!(
            tiers[1].ledgers,
            [
                headline("USD", [8_000.0, 3.0, 0.75, -0.25]),
                headline("EUR", [7_000.0, -1.0, 0.0, 0.0])
            ]
        );
        assert_eq!(
            tiers[2].ledgers,
            [
                headline("USD", [300_000.0, 0.0, 0.0, 0.0]),
                headline("EUR", [100_000.0, 0.0, 0.0, 0.0])
            ]
        );
    }

    /// A tier with no account is no group, and a currency none of a tier's accounts is in is no line of it.
    #[test]
    fn a_tier_with_no_account_is_not_a_group() {
        let mut snapshot = CoreSnapshot::empty("bybit", "BTC");
        snapshot.portfolio.venues = vec![
            tiered("bybit", "bybit#LIVE", Some(AccountTier::Live), "", [10.0, 0.0, 0.0, 0.0]),
            tiered("oanda", "oanda", Some(AccountTier::Paper), "EUR", [5.0, 0.0, 0.0, 0.0]),
        ];
        let tiers = AccountWindowEquityModel::from_snapshot(&snapshot).tiers;
        let said: Vec<_> = tiers.iter().map(|g| (g.tier, g.ledgers.len())).collect();
        assert_eq!(said, [(Some(AccountTier::Live), 1), (Some(AccountTier::Paper), 1)]);
        assert_eq!(tiers[1].ledgers[0].currency, "EUR");
    }

    /// A node older than the tier report says none: no group, so the window draws the one total with its note.
    /// An account whose tier is not said beside told ones is a group of its own, last, never added to a tier.
    #[test]
    fn no_tier_said_is_no_group_and_one_unsaid_beside_told_ones_is_its_own() {
        let mut snapshot = CoreSnapshot::empty("bybit", "BTC");
        snapshot.portfolio.venues = vec![base_venue_block("bybit"), base_venue_block("okx")];
        assert!(AccountWindowEquityModel::from_snapshot(&snapshot).tiers.is_empty());
        snapshot.portfolio.venues[0].tier = Some(AccountTier::Demo);
        let tiers = AccountWindowEquityModel::from_snapshot(&snapshot).tiers;
        let said: Vec<_> = tiers.iter().map(|g| (g.tier, g.accounts)).collect();
        assert_eq!(said, [(Some(AccountTier::Demo), 1), (None, 1)]);
        assert!(
            AccountWindowEquityModel::from_snapshot(&CoreSnapshot::empty("bybit", "BTC"))
                .tiers
                .is_empty()
        );
    }
}
