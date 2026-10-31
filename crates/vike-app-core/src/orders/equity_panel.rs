//! Qt-free view-model for the cross-venue equity panel (PR-2 T4). The shell (`vike-desktop`;
//! `vike-app` when this was written) renders it; this module is CI-tested because the shell's own
//! code runs under no test beyond a few pins (see the crate-level doc).
//!
//! Pure mapping only: one [`EquityRow`] per `CoreSnapshot::portfolio` venue entry, in order;
//! `equity_total`/`missing_prices_total` copied straight from the snapshot. No egui, no I/O —
//! the GUI owns all number formatting, the only rendering choice made here is the short `mode`
//! tag.

use vike_exec::{BalanceMode, CoreSnapshot, Interned, LedgerTotals};
use vike_model::AccountTier;

/// What a venue's unlabelled account is called, as the Trade window's account picker calls it.
const MAIN: &str = "main";

/// One venue's row in the cross-venue equity panel — a flattened, display-ready slice of a
/// `VenueBlock`.
#[derive(Debug, Clone, PartialEq)]
pub struct EquityRow {
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
    /// The row's currency group (`VenueBlock::ledger_ccy`): EMPTY for the dollar group. Every figure
    /// above is in it, so only rows of one group may be added together ([`EquityPanelModel`]).
    pub ccy: Interned,
}

/// One currency group's headline, for every group but the primary's: what the panel shows BESIDE the
/// primary's totals instead of adding them (a node whose engines are not all in one currency;
/// `Portfolio::ledger_totals`). Absent from a node of one currency.
#[derive(Debug, Clone, PartialEq)]
pub struct LedgerHeadline {
    /// The group's code for a label (`"EUR"`; the dollar group reads `"USD"`).
    pub ccy: &'static str,
    pub equity_total: f64,
    pub realized: f64,
    pub fees: f64,
    pub funding: f64,
    pub margin_used: f64,
}

/// One conversion rate behind an account's figures (`VenueBlock::quote_rates`, decision 0183), as the panel
/// lists it beside the Accounts table: the figure in the ONE wording every surface uses, and what the window
/// colours by. Absent for an account that holds no symbol quoted in another currency.
#[derive(Debug, Clone, PartialEq)]
pub struct RateLine {
    /// The account's engine key on the node (`VenueBlock::route_key`; the bare venue off a node that sends none).
    pub account: String,
    /// `1 JPY = 0.006667 USD`, then ` STALE` and what that means when a position is open in the currency
    /// (`vike_tradehub_client::wire::quote_rate_phrase`).
    pub phrase: String,
    /// The node's verdict that the rate is past its staleness window.
    pub stale: bool,
}

/// The whole cross-venue equity panel: the headline (`equity_total`/`missing_prices_total`)
/// plus one [`EquityRow`] per venue, in `CoreSnapshot::venues` order.
///
/// ⚠ `equity_total` is ONE currency: the PRIMARY block's group (`Portfolio::equity_total`), and
/// `ccy` names it. The other groups are [`Self::other_ledgers`], EMPTY for a node whose engines share
/// a currency, so such a panel reads as it always did. Never add a row of one group to another's.
#[derive(Debug, Clone, PartialEq)]
pub struct EquityPanelModel {
    pub equity_total: f64,
    pub missing_prices_total: u32,
    pub rows: Vec<EquityRow>,
    /// The primary group's code (`"USD"` for the dollar group).
    pub ccy: &'static str,
    /// The primary group's key (`Portfolio::primary_ccy`, EMPTY for the dollar group): the node's own, read
    /// off the frame's stated key and never off the first row (after a primary restart the first row can be
    /// another engine's). The rows whose `ccy` is this are the ones a strip adds together.
    pub primary: Interned,
    /// Every other currency group's headline, first-seen order.
    pub other_ledgers: Vec<LedgerHeadline>,
    /// Every account's conversion rates, in block order. EMPTY for a node with no account that holds a symbol
    /// quoted in another currency (the window then draws nothing for it).
    pub rates: Vec<RateLine>,
}

impl EquityPanelModel {
    /// Shape a `CoreSnapshot` into the panel model: one `EquityRow` per `VenueBlock` in order;
    /// `equity_total`/`missing_prices_total` copied straight from the snapshot. Pure — no I/O,
    /// no egui, no formatting beyond the `mode` tag (the GUI formats every number).
    pub fn from_snapshot(snap: &CoreSnapshot) -> Self {
        let rows = snap
            .portfolio
            .venues
            .iter()
            .map(|v| EquityRow {
                venue: v.venue.clone(),
                account: v.account.as_ref().and_then(|a| a.text()).unwrap_or(MAIN).to_string(),
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
                ccy: v.ledger_ccy,
            })
            .collect();
        // One group (every node today) builds nothing here: `ledger_totals` allocates only for a
        // panel with several, and this is skipped for the common one by the `is_multi_currency` test.
        let other_ledgers = if snap.portfolio.is_multi_currency() {
            snap.portfolio
                .ledger_totals()
                .into_iter()
                .skip(1)
                .map(|t| LedgerHeadline {
                    ccy: t.label(),
                    equity_total: t.equity,
                    realized: t.realized_pnl,
                    fees: t.fees_paid,
                    funding: t.funding_paid,
                    margin_used: t.margin_used,
                })
                .collect()
        } else {
            Vec::new()
        };
        let rates = snap
            .portfolio
            .venues
            .iter()
            .flat_map(|v| {
                let account = if v.route_key.is_empty() { &v.venue } else { &v.route_key };
                v.quote_rates.iter().map(move |r| RateLine {
                    account: account.clone(),
                    phrase: vike_tradehub_client::wire::quote_rate_phrase(
                        &r.ccy,
                        r.rate,
                        r.stale,
                        r.held,
                        &v.quote_rates_in,
                    ),
                    stale: r.stale,
                })
            })
            .collect();
        EquityPanelModel {
            equity_total: snap.portfolio.equity_total,
            missing_prices_total: snap.portfolio.missing_prices_total,
            rows,
            ccy: LedgerTotals::label_of(snap.portfolio.primary_ccy()),
            primary: snap.portfolio.primary_ccy(),
            other_ledgers,
            rates,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vike_exec::{BalanceMode, CoreSnapshot, VenueBlock};
    use vike_model::TradingState;

    /// A zero-valued `VenueBlock` for `venue`, Delta mode, no positions — struct-update base
    /// for tests that only care about a subset of fields.
    fn base_venue_block(venue: &str) -> VenueBlock {
        VenueBlock {
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
            ledger_ccy: Default::default(),
            ccy_assumed: false,
            colliding_fills_refused: 0,
            fees_unconverted: 0,
            fees_at_stale_rate: 0,
            equity: 0.0,
            unrealized: 0.0,
            missing_prices: 0,
            margin_used: 0.0,
            free_bp: 0.0,
            margin_ratio: 0.0,
            fee_schedule: None,
            trading_state: TradingState::Active,
            positions: Vec::new(),
        }
    }

    #[test]
    fn panel_model_maps_venues_in_order() {
        let mut snap = CoreSnapshot::empty("binance", "BTC");
        snap.portfolio.equity_total = 12_345.678;
        snap.portfolio.missing_prices_total = 3;
        snap.portfolio.venues = vec![
            VenueBlock { equity: 1_100.0, unrealized: 100.0, ..base_venue_block("binance") },
            VenueBlock {
                equity: 2_200.0,
                unrealized: 200.0,
                missing_prices: 2,
                balance_mode: BalanceMode::Authoritative,
                ..base_venue_block("bybit")
            },
        ];

        let m = EquityPanelModel::from_snapshot(&snap);

        assert_eq!(m.rows.len(), 2);
        assert_eq!(m.rows[0].venue, "binance");
        assert_eq!(m.equity_total.to_bits(), snap.portfolio.equity_total.to_bits());
        assert_eq!(m.missing_prices_total, snap.portfolio.missing_prices_total);
        assert_eq!(m.rows[1].unrealized, snap.portfolio.venues[1].unrealized);
    }

    #[test]
    fn equity_row_maps_every_field_without_transposition() {
        // Distinct values per field so a swapped mapping (e.g. fees<->funding) fails loudly.
        let mut snap = CoreSnapshot::empty("okx", "BTC-PERP");
        snap.portfolio.venues = vec![VenueBlock {
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
            ledger_ccy: Default::default(),
            ccy_assumed: false,
            colliding_fills_refused: 0,
            fees_unconverted: 0,
            fees_at_stale_rate: 0,
            equity: 9_000.0,
            unrealized: 8.0,
            missing_prices: 5,
            margin_used: 0.0,
            free_bp: 0.0,
            margin_ratio: 0.0,
            fee_schedule: None,
            trading_state: TradingState::Active,
            positions: Vec::new(),
        }];

        let row = &EquityPanelModel::from_snapshot(&snap).rows[0];

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
        let mut snap = CoreSnapshot::empty("bybit", "BTC");
        snap.portfolio.venues = vec![
            VenueBlock { tier: Some(AccountTier::Demo), ..base_venue_block("bybit") },
            VenueBlock {
                account: Some(live),
                route_key: "bybit#LIVE".to_string(),
                tier: Some(AccountTier::Live),
                ..base_venue_block("bybit")
            },
            base_venue_block("okx"),
        ];
        let rows = EquityPanelModel::from_snapshot(&snap).rows;
        let said = |i: usize| (rows[i].account.as_str(), rows[i].tier);
        assert_eq!(said(0), ("main", Some(AccountTier::Demo)));
        assert_eq!(said(1), ("LIVE", Some(AccountTier::Live)));
        assert_eq!(said(2), ("main", None), "a tier the node did not say stays unsaid");
    }

    #[test]
    fn mode_tag_renders_short_strings() {
        let mut delta_snap = CoreSnapshot::empty("binance", "BTC");
        delta_snap.portfolio.venues =
            vec![VenueBlock { balance_mode: BalanceMode::Delta, ..base_venue_block("binance") }];
        let mut auth_snap = CoreSnapshot::empty("bybit", "BTC");
        auth_snap.portfolio.venues = vec![VenueBlock {
            balance_mode: BalanceMode::Authoritative,
            ..base_venue_block("bybit")
        }];

        assert_eq!(EquityPanelModel::from_snapshot(&delta_snap).rows[0].mode, "\u{0394}");
        assert_eq!(EquityPanelModel::from_snapshot(&auth_snap).rows[0].mode, "auth");
    }

    #[test]
    fn empty_venues_yields_empty_rows_but_keeps_totals() {
        let mut snap = CoreSnapshot::empty("binance", "BTC");
        snap.portfolio.equity_total = 42.0;
        snap.portfolio.missing_prices_total = 7;

        let m = EquityPanelModel::from_snapshot(&snap);

        assert!(m.rows.is_empty());
        assert_eq!(m.equity_total, 42.0);
        assert_eq!(m.missing_prices_total, 7);
    }

    /// A node with a EUR account beside a dollar one: the headline is the dollar group's alone and the
    /// EUR group is its own headline, in its own code. Nothing is added across the two.
    #[test]
    fn a_second_currency_is_a_headline_beside_the_primary_not_part_of_it() {
        let mut snap = CoreSnapshot::empty("binance", "BTC");
        snap.portfolio.equity_total = 1_100.0; // the dollar group's, as `snapshot::build` binds it
        snap.portfolio.venues = vec![
            VenueBlock { equity: 1_100.0, realized_pnl: 5.0, ..base_venue_block("binance") },
            VenueBlock {
                equity: 7_000.0,
                realized_pnl: -300.0,
                fees_paid: 4.5,
                funding_paid: 0.25,
                margin_used: 120.0,
                ledger_ccy: "EUR".into(),
                ..base_venue_block("oanda")
            },
        ];
        let m = EquityPanelModel::from_snapshot(&snap);
        assert_eq!(m.ccy, "USD");
        assert_eq!(m.equity_total, 1_100.0);
        assert_eq!(m.rows[0].ccy, "");
        assert_eq!(m.rows[1].ccy, "EUR");
        assert_eq!(
            m.other_ledgers,
            vec![LedgerHeadline {
                ccy: "EUR",
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
        let mut snap = CoreSnapshot::empty("binance", "BTC");
        snap.portfolio.venues = vec![base_venue_block("binance"), base_venue_block("bybit")];
        let m = EquityPanelModel::from_snapshot(&snap);
        assert!(m.other_ledgers.is_empty());
        assert_eq!(m.ccy, "USD");
        assert!(m.rates.is_empty(), "no account holds a foreign-quoted symbol: nothing to list");
    }

    /// **The conversion rates behind an account's figures are listed, and a stale one says whether it values a
    /// position** (decision 0183): the window draws what this model holds and nothing it computes itself, so a
    /// rate that is stale under an open position reads the same here, in `trade equity` and in Telegram.
    #[test]
    fn a_stale_rate_under_a_position_is_listed_in_the_one_wording() {
        let mut snap = CoreSnapshot::empty("binance", "BTC");
        snap.portfolio.venues = vec![
            base_venue_block("binance"),
            VenueBlock {
                route_key: "fxcm".into(),
                ledger_ccy: "EUR".into(),
                quote_rates_in: "EUR".into(),
                quote_rates: vec![
                    vike_exec::QuoteRateView {
                        ccy: "USD".into(),
                        rate: 0.862_069,
                        as_of_ms: 1,
                        stale: true,
                        held: true,
                    },
                    vike_exec::QuoteRateView {
                        ccy: "JPY".into(),
                        rate: 0.005_747,
                        as_of_ms: 1,
                        stale: false,
                        held: false,
                    },
                ],
                ..base_venue_block("fxcm")
            },
        ];
        let m = EquityPanelModel::from_snapshot(&snap);
        assert_eq!(
            m.rates,
            vec![
                RateLine {
                    account: "fxcm".into(),
                    phrase: "1 USD = 0.862069 EUR STALE (an open position is valued at it)".into(),
                    stale: true,
                },
                RateLine {
                    account: "fxcm".into(),
                    phrase: "1 JPY = 0.005747 EUR".into(),
                    stale: false,
                },
            ]
        );
    }
}
