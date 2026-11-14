//! **The position words a venue answers in**, beside the report row reconcile diffs
//! (`crates/vike-model/src/venues/report.rs`'s `PositionStatusReport`): one piece of a position as a
//! venue keeps it ([`PositionLeg`]), the two facts that decide how a bridge builds its rows
//! ([`PositionRows`] per venue, [`PositionMode`] per account) and one holding on a prediction market
//! ([`PositionPrediction`]). What WE hold is `crate::money`'s `Position`.
//!
//! Plain vocabulary: no I/O and no venue's spelling. How a venue words any of it stays in its bridge.
//!
//! **The kinds of position report are TWO facts, never one three-valued type** (owner, 2026-11-11): a
//! venue that nets is `FromVenue` + `OneWay`; a venue that reports its own long and short rows is
//! `FromVenue` + `Hedge`; a venue that stores pieces is `FromLegs`, whatever the account's mode. The
//! locked-pair rule (two rows, held) belongs to `FromLegs` alone.

use crate::{PositionStatusReport, VenueReadPosition};

/// One piece of a position as the venue keeps it: a deal, a trade, an order, or one position id of a
/// hedging account. [`PositionStatusReport::from_legs`] nets one symbol's legs into its report rows.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PositionLeg {
    /// Signed size: positive long, negative short, in the units of the report's `qty`.
    pub size: f64,
    /// The leg's own entry price. Not finite, or not positive, means the venue states none.
    pub price: f64,
    /// When the venue says the leg opened, epoch ms; `0` when it says nothing.
    pub timestamp_ms: i64,
}

/// Where a venue's position rows come from: a fixed fact per VENUE.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PositionRows {
    /// The venue reports positions itself: one net row per symbol, or its own long and short rows on
    /// a hedge-mode account ([`PositionMode::Hedge`]).
    FromVenue,
    /// The venue stores pieces (deals, trades, orders) and the bridge builds the rows from them,
    /// through [`PositionStatusReport::from_legs`] and its locked-pair rule.
    FromLegs,
}

/// How an account holds positions: a fact per ACCOUNT, read when its bridge starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PositionMode {
    /// One net position per symbol: an opposite order reduces it.
    OneWay,
    /// A long and a short position per symbol side by side: an opposite order opens the other side.
    Hedge,
    /// The venue's answer could not be read. The words saying why travel beside it, in the bridge.
    Unknown,
}

/// **One holding of an outcome on a prediction market**, in words that fit every such venue: one
/// type for all of them, so a venue's own name never reaches the model. A holding is long only.
#[derive(Debug, Clone, PartialEq)]
pub struct PositionPrediction {
    /// The market (the condition) the outcome belongs to; empty when the venue's row names none.
    pub market_id: String,
    /// The outcome held: the instrument the engine trades (an outcome token's id).
    pub outcome_id: String,
    /// Which outcome SLOT of the market this is. `None` when the venue's value is absent or cannot
    /// be read as a slot: an unreadable slot stays unreadable, never slot `0`.
    pub outcome_index: Option<u32>,
    /// The outcome in words (`Yes`, `Down`), as sent; empty when the venue names none.
    pub outcome: String,
    /// The market in words, as sent; empty when the venue names none.
    pub title: String,
    /// Shares held NOW, never the lifetime bought.
    pub size: f64,
    /// ⚠ The market has RESOLVED and the shares can be redeemed. A resolution flag, not a winner
    /// flag: a losing outcome reads `true` too and pays nothing.
    pub redeemable: bool,
    /// The market is a negative-risk one: a redeem goes to the venue's negative-risk contract, not
    /// the plain one.
    pub negative_risk: bool,
    /// The entry price per share; `None` when the venue states none. A stated `0.0` (shares given,
    /// no buy recorded) is kept as stated.
    pub average_price: Option<f64>,
    /// The venue's current price of one share; `None` when the row states none, never read as `0.0`.
    pub price: Option<f64>,
    /// What the venue says the holding is worth now; `None` when the row states none.
    pub value: Option<f64>,
}

impl PositionPrediction {
    /// **The report row reconcile diffs**, on `venue`: the outcome is the symbol, `size` the one-way
    /// ([`crate::events::PositionSide::Both`]) quantity and `average_price` its price; the rest is
    /// [`PositionStatusReport::flat`]'s. A zero-size holding is reported as held, so the diff sees a
    /// local position the venue now reports flat.
    ///
    /// `Err`, naming the outcome, for a holding that states no entry price: a missing number is
    /// unavailable, never zero, so the pass says so rather than book a guessed `0.0`.
    pub fn to_status_report(&self, venue: &str) -> Result<PositionStatusReport, String> {
        let avg_px = self.average_price.ok_or_else(|| {
            format!(
                "{venue}'s positions row for token {} states no readable average_price, so its entry \
                 price is unknown",
                self.outcome_id
            )
        })?;
        Ok(PositionStatusReport {
            qty: self.size,
            average_price: avg_px,
            ..PositionStatusReport::flat(venue, &self.outcome_id)
        })
    }

    /// **The operator's venue-read row** ([`crate::VenueRead::Positions`]): every figure as the venue
    /// stated it, the outcome as the symbol.
    #[must_use]
    pub fn into_read_row(self) -> VenueReadPosition {
        VenueReadPosition {
            symbol: self.outcome_id,
            title: self.title,
            outcome: self.outcome,
            qty: self.size,
            average_price: self.average_price,
            price: self.price,
            value: self.value,
            redeemable: self.redeemable,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MarginMode;
    use crate::events::PositionSide;

    fn holding() -> PositionPrediction {
        PositionPrediction {
            market_id: "0x13bf".into(),
            outcome_id: "148".into(),
            outcome_index: Some(1),
            outcome: "Down".into(),
            title: "Solana Up or Down".into(),
            size: 11.11,
            redeemable: false,
            negative_risk: true,
            average_price: Some(0.21),
            price: Some(0.5),
            value: Some(5.555),
        }
    }

    /// The report names the outcome, books the held size one-way at the entry price, and is otherwise
    /// the flat row: Cross, no timestamp, no isolated margin, no delta.
    #[test]
    fn a_holding_reports_its_size_at_its_entry_price() {
        let report = holding().to_status_report("polymarket").unwrap();
        assert_eq!(
            report,
            PositionStatusReport {
                venue: "polymarket".into(),
                symbol: "148".into(),
                position_side: PositionSide::Both,
                qty: 11.11,
                average_price: 0.21,
                timestamp: 0,
                margin_mode: MarginMode::Cross,
                isolated_margin: None,
                delta: None,
            }
        );
        let zero = PositionPrediction { size: 0.0, ..holding() };
        assert_eq!(
            zero.to_status_report("polymarket").unwrap().qty,
            0.0,
            "a flat holding is still a row"
        );
    }

    /// No stated entry price is an error naming the outcome, never a `0.0`; a stated `0.0` is kept.
    #[test]
    fn a_holding_with_no_entry_price_is_refused_and_a_stated_zero_is_kept() {
        let unpriced = PositionPrediction { average_price: None, ..holding() };
        assert_eq!(
            unpriced.to_status_report("polymarket").unwrap_err(),
            "polymarket's positions row for token 148 states no readable average_price, so its entry \
             price is unknown"
        );
        let given = PositionPrediction { average_price: Some(0.0), ..holding() };
        assert_eq!(given.to_status_report("polymarket").unwrap().average_price, 0.0);
    }

    /// The read row carries every figure as stated, absent ones absent, and the resolution flag.
    #[test]
    fn a_holding_becomes_its_read_row() {
        assert_eq!(
            holding().into_read_row(),
            VenueReadPosition {
                symbol: "148".into(),
                title: "Solana Up or Down".into(),
                outcome: "Down".into(),
                qty: 11.11,
                average_price: Some(0.21),
                price: Some(0.5),
                value: Some(5.555),
                redeemable: false,
            }
        );
        let resolved =
            PositionPrediction { price: None, value: None, redeemable: true, ..holding() };
        let row = resolved.into_read_row();
        assert_eq!((row.price, row.value, row.redeemable), (None, None, true));
    }
}
