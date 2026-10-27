//! What the VENUE says a closing deal booked (decision 0183, assumption 1: "the venues book realized PnL as
//! quote PnL times the close-time rate"). A closing `ProtoOADeal` carries a `ProtoOAClosePositionDetail`
//! (<https://help.ctrader.com/open-api/model-messages/>): the realized gross profit, swap and commission, the
//! account balance after the deal, `pnlConversionFee` and `quoteToDepositConversionRate` ("Quote/Deposit
//! currency conversion rate on the time of closing deal execution").
//!
//! ⚠ **Almost nothing here feeds the engine.** The page gives no direction for the rate (which currency is the
//! numerator) and no demo close has been compared with it, so the engine's rate is the conversion pair's own
//! quote (`crate::quote_rate`); [`close_details_of`] only PRINTS what the venue booked so a demo drill can set
//! the engine's realized PnL beside it (the live drill, `ctrader_currency_smoke`), `quoteToDepositConversionRate`
//! included. The ONE figure that does reach the engine is [`conversion_fee_of`], the venue's fee for converting
//! the close's PnL into the deposit currency, which rides on the closing fill's commission
//! (`crate::event_mapper::deal_fill`): the venue charges it to the balance, so an engine that left it out would
//! drift from the venue's balance by exactly that. The money integers are scaled by the detail's own
//! `moneyDigits` ("Affects grossProfit, swap, commission, balance, pnlConversionFee"), falling back to the
//! account's `ProtoOATrader.moneyDigits`.

use crate::proto::{ProtoOaDeal, ProtoOaTradeSide};
use crate::symbols::SymbolMap;

/// **The venue's fee for converting a closing deal's PnL into the deposit currency**, as a COST in the deposit
/// currency (`>= 0`, the engine's commission convention: positive is a charge), `0.0` when the deal carries
/// none (an opening deal, a close whose quote is the deposit currency, a venue that states no fee).
///
/// `ProtoOAClosePositionDetail.pnlConversionFee` is "Fee for conversion applied to the Deal in account's ccy
/// when trader symbol's quote asset id <> ProtoOATrader.depositAssetId"
/// (<https://help.ctrader.com/open-api/model-messages/>): exactly a foreign-quoted close. The page does not
/// state its SIGN (the deal's own `commission` is negative when charged), so the magnitude is the cost: a fee
/// is never a credit. UNMEASURED:
/// the drill's `CTDEAL ... convfee=` line shows the sign the venue really sends. Scaled by the detail's own
/// `moneyDigits`, else the deal's, else `money_digits` (the account's).
#[must_use]
pub fn conversion_fee_of(deal: &ProtoOaDeal, money_digits: u32) -> f64 {
    let Some(detail) = deal.close_position_detail.as_ref() else { return 0.0 };
    let Some(raw) = detail.pnl_conversion_fee else { return 0.0 };
    let digits = detail.money_digits.or(deal.money_digits).unwrap_or(money_digits);
    (raw as f64 / 10f64.powi(i32::try_from(digits).unwrap_or(i32::MAX))).abs()
}

/// One closing deal's booked figures, in the account's deposit currency (descaled), as the venue states them.
#[derive(Debug, Clone, PartialEq)]
pub struct CloseDetail {
    /// `ProtoOADeal.dealId`.
    pub deal_id: i64,
    /// The broker's name for the deal's symbol, or `""` when the list does not carry the id.
    pub symbol: String,
    /// `+1` for a BUY deal, `-1` for a SELL.
    pub side: i32,
    /// `ProtoOADeal.filledVolume` in base units (the wire's centi-units divided by 100).
    pub filled_units: f64,
    /// `ProtoOADeal.executionPrice` (the closing price), when stated.
    pub execution_price: Option<f64>,
    /// `ProtoOAClosePositionDetail.entryPrice`: "Position price at the moment of filling the closing order".
    pub entry_price: f64,
    /// `grossProfit` descaled: "realized gross profit after closing deal execution", in the deposit currency.
    pub gross_profit: f64,
    /// `swap` descaled.
    pub swap: f64,
    /// `commission` descaled (negative when charged, as the wire states it: NOT the engine's sign).
    pub commission: f64,
    /// `balance` descaled: the account balance after the deal.
    pub balance: f64,
    /// `quoteToDepositConversionRate`, raw and UNINTERPRETED (no direction is documented).
    pub quote_to_deposit_rate: Option<f64>,
    /// `pnlConversionFee` descaled: "Fee for conversion applied to the Deal in account's ccy when trader
    /// symbol's quote asset id <> ProtoOATrader.depositAssetId".
    pub pnl_conversion_fee: Option<f64>,
    /// The exponent the money integers were descaled by.
    pub money_digits: u32,
    /// `ProtoOAClosePositionDetail.moneyDigits` as the venue stated it (`None`: omitted, so the account's
    /// [`Self::trader_money_digits`] was the fallback). Printed beside the other two so the drill sees which
    /// of the three the venue really sends.
    pub detail_money_digits: Option<u32>,
    /// `ProtoOADeal.moneyDigits` as the venue stated it (`None`: omitted).
    pub deal_money_digits: Option<u32>,
    /// The account's `ProtoOATrader.moneyDigits` (the fallback exponent of the session that read this list).
    pub trader_money_digits: u32,
    /// `ProtoOADeal.executionTimestamp`, epoch ms.
    pub ts: i64,
}

/// Every closing deal of `deals` (those with a `closePositionDetail`), oldest first, descaled. A deal without a
/// detail (an opening deal) is not here. `money_digits` is the account-level fallback exponent.
pub fn close_details_of(
    deals: &[ProtoOaDeal],
    symbols: &SymbolMap,
    money_digits: u32,
) -> Vec<CloseDetail> {
    let mut out: Vec<CloseDetail> = deals
        .iter()
        .filter_map(|deal| {
            let detail = deal.close_position_detail.as_ref()?;
            let digits = detail.money_digits.unwrap_or(money_digits);
            let scale = 10f64.powi(i32::try_from(digits).unwrap_or(i32::MAX));
            let money = |raw: i64| raw as f64 / scale;
            Some(CloseDetail {
                deal_id: deal.deal_id,
                symbol: symbols.name_of(deal.symbol_id).unwrap_or_default().to_string(),
                side: if deal.trade_side == ProtoOaTradeSide::Sell as i32 { -1 } else { 1 },
                filled_units: deal.filled_volume as f64 / 100.0,
                execution_price: deal.execution_price,
                entry_price: detail.entry_price,
                gross_profit: money(detail.gross_profit),
                swap: money(detail.swap),
                commission: money(detail.commission),
                balance: money(detail.balance),
                quote_to_deposit_rate: detail.quote_to_deposit_conversion_rate,
                pnl_conversion_fee: detail.pnl_conversion_fee.map(money),
                money_digits: digits,
                detail_money_digits: detail.money_digits,
                deal_money_digits: deal.money_digits,
                trader_money_digits: money_digits,
                ts: deal.execution_timestamp,
            })
        })
        .collect();
    out.sort_by_key(|d| (d.ts, d.deal_id));
    out
}

impl CloseDetail {
    /// One greppable line, the format the demo drill reads (`key=value` pairs, no spaces inside a value).
    #[must_use]
    pub fn line(&self) -> String {
        let opt = |v: Option<f64>| v.map_or_else(|| "none".to_string(), |x| format!("{x}"));
        format!(
            "CTDEAL id={} sym={} side={} units={} px={} entry={} gross={} swap={} comm={} balance={} \
             q2d={} convfee={} digits={} detail_digits={} deal_digits={} trader_digits={} ts={}",
            self.deal_id,
            if self.symbol.is_empty() { "?" } else { &self.symbol },
            self.side,
            self.filled_units,
            opt(self.execution_price),
            self.entry_price,
            self.gross_profit,
            self.swap,
            self.commission,
            self.balance,
            opt(self.quote_to_deposit_rate),
            opt(self.pnl_conversion_fee),
            self.money_digits,
            self.detail_money_digits.map_or_else(|| "none".to_string(), |d| d.to_string()),
            self.deal_money_digits.map_or_else(|| "none".to_string(), |d| d.to_string()),
            self.trader_money_digits,
            self.ts,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proto::{ProtoOaClosePositionDetail, ProtoOaLightSymbol};

    fn symbols() -> SymbolMap {
        SymbolMap::from_symbols(
            &[ProtoOaLightSymbol {
                symbol_id: 2,
                symbol_name: Some("USDJPY".to_string()),
                ..Default::default()
            }],
            &[],
        )
    }

    fn deal(id: i64, ts: i64, detail: Option<ProtoOaClosePositionDetail>) -> ProtoOaDeal {
        ProtoOaDeal {
            deal_id: id,
            order_id: 1,
            position_id: 1,
            volume: 100_000,
            filled_volume: 100_000,
            symbol_id: 2,
            create_timestamp: ts,
            execution_timestamp: ts,
            execution_price: Some(150.12),
            trade_side: ProtoOaTradeSide::Sell as i32,
            deal_status: 2,
            close_position_detail: detail,
            ..Default::default()
        }
    }

    /// DOCS-SHAPED: a USDJPY 1,000-unit round trip closed 0.10 yen up books 100 yen = 0.67 dollars on a USD
    /// account whose `moneyDigits` is 2 (so `67` on the wire); the rate field is read raw. Only closing
    /// deals are here, oldest first, and the integers are descaled by the DETAIL's own exponent.
    #[test]
    fn only_closing_deals_are_read_oldest_first_and_descaled_by_the_details_digits() {
        let detail = |gross, digits| ProtoOaClosePositionDetail {
            entry_price: 150.02,
            gross_profit: gross,
            swap: 0,
            commission: -3,
            balance: 1_000_067,
            quote_to_deposit_conversion_rate: Some(0.006_66),
            closed_volume: Some(100_000),
            money_digits: digits,
            pnl_conversion_fee: Some(1),
            ..Default::default()
        };
        let deals = [
            deal(9, 2_000, Some(detail(67, Some(2)))),
            deal(8, 1_000, None),
            deal(7, 1_500, Some(detail(67_000_000, Some(8)))),
            deal(6, 1_600, Some(detail(67, None))),
        ];
        let got = close_details_of(&deals, &symbols(), 2);
        assert_eq!(got.iter().map(|d| d.deal_id).collect::<Vec<_>>(), [7, 6, 9], "no opening deal");
        assert_eq!(
            (got[0].gross_profit, got[0].money_digits),
            (0.67, 8),
            "the detail's own exponent"
        );
        assert_eq!((got[1].gross_profit, got[1].money_digits), (0.67, 2), "the account's fallback");
        let d = &got[2];
        assert_eq!((d.symbol.as_str(), d.side, d.filled_units), ("USDJPY", -1, 1000.0));
        assert_eq!((d.entry_price, d.execution_price), (150.02, Some(150.12)));
        assert_eq!((d.gross_profit, d.commission, d.balance), (0.67, -0.03, 10_000.67));
        assert_eq!((d.quote_to_deposit_rate, d.pnl_conversion_fee), (Some(0.006_66), Some(0.01)));
    }

    /// The fee the engine books is the detail's own exponent else the deal's else the account's, and a COST
    /// (the page states no sign): -7 and 7 are the same 0.07; no detail and no fee are nothing.
    #[test]
    fn the_conversion_fee_is_a_scaled_cost_or_nothing() {
        let with = |fee, digits| {
            deal(
                1,
                1,
                Some(ProtoOaClosePositionDetail {
                    pnl_conversion_fee: fee,
                    money_digits: digits,
                    ..Default::default()
                }),
            )
        };
        assert_eq!(conversion_fee_of(&with(Some(-7), Some(2)), 0), 0.07);
        assert_eq!(conversion_fee_of(&with(Some(7), Some(2)), 0), 0.07);
        assert_eq!(conversion_fee_of(&with(Some(-7), Some(3)), 2), 0.007, "the detail's exponent");
        assert_eq!(conversion_fee_of(&with(Some(-7), None), 2), 0.07, "the account's fallback");
        let mut dealt = with(Some(-7), None);
        dealt.money_digits = Some(1);
        assert_eq!(
            conversion_fee_of(&dealt, 2),
            0.7,
            "the deal's own exponent before the account's"
        );
        assert_eq!(conversion_fee_of(&with(None, Some(2)), 2), 0.0, "no fee stated");
        assert_eq!(conversion_fee_of(&deal(1, 1, None), 2), 0.0, "an opening deal");
    }

    /// A symbol the list does not carry is `?` in the line; an absent optional is `none`, never a number.
    #[test]
    fn the_line_names_what_it_does_not_know() {
        let mut d = deal(1, 5, Some(ProtoOaClosePositionDetail::default()));
        d.symbol_id = 99;
        d.execution_price = None;
        let got = close_details_of(&[d], &symbols(), 2);
        let line = got[0].line();
        assert!(line.starts_with("CTDEAL id=1 sym=? side=-1 units=1000 px=none "), "{line}");
        assert!(
            line.contains(
                " q2d=none convfee=none digits=2 detail_digits=none deal_digits=none \
                 trader_digits=2 ts=5"
            ),
            "{line}"
        );
    }

    /// The three exponents are printed as the venue sent them: a detail that states its own, a deal that does
    /// and the account's fallback, so the drill sees which of the three is really present.
    #[test]
    fn the_line_prints_the_detail_the_deal_and_the_traders_exponent() {
        let mut d = deal(
            1,
            5,
            Some(ProtoOaClosePositionDetail { money_digits: Some(2), ..Default::default() }),
        );
        d.money_digits = Some(3);
        let line = close_details_of(&[d], &symbols(), 4)[0].line();
        assert!(
            line.contains(" digits=2 detail_digits=2 deal_digits=3 trader_digits=4 ts=5"),
            "{line}"
        );
    }
}
