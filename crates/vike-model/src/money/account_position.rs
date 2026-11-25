//! [`AccountPosition`]: one open position of an account as a snapshot states it (the datahub's stored
//! row and its wire, the position a window opens from), and [`AccountVenuePosition`], the same row
//! naming its venue (the journal's row: a tradehub boot's position snapshot and a portfolio sample;
//! the tradehub sends it to the datahub [`AccountVenuePosition::without_venue`]).
//!
//! ⚠ Their serde is a STORED, WIRE and JOURNAL format: the JSON array in vike.db's position-snapshot
//! rows and the datahub's execution-stream wire carry [`AccountPosition`]'s fields, the journal's
//! position snapshot and portfolio sample [`AccountVenuePosition`]'s, each in this order, and
//! `position_side` is [`PositionSide`]'s upper-case word (`BOTH`, `LONG`,
//! `SHORT`), byte for byte the string those rows held before it was typed (JSON and the journal's
//! named MessagePack alike: a unit variant is written as its name).

use serde::{Deserialize, Serialize};

use crate::events::PositionSide;

/// **One open position of an account**, under an account that names its venue.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AccountPosition {
    /// The instrument's user name.
    pub symbol: String,
    /// The hedge leg, [`PositionSide::Both`] for a one-way position.
    pub position_side: PositionSide,
    /// Signed size: long positive, short negative.
    pub size: f64,
    /// The average entry price.
    pub average_price: f64,
}

/// **One open position naming its venue**: [`AccountPosition`] with the venue first.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AccountVenuePosition {
    /// The venue's roster slug.
    pub venue: String,
    /// The instrument's user name.
    pub symbol: String,
    /// The hedge leg, [`PositionSide::Both`] for a one-way position.
    pub position_side: PositionSide,
    /// Signed size: long positive, short negative.
    pub size: f64,
    /// The average entry price.
    pub average_price: f64,
}

impl AccountPosition {
    /// This position naming `venue`.
    #[must_use]
    pub fn on_venue(self, venue: String) -> AccountVenuePosition {
        let Self { symbol, position_side, size, average_price } = self;
        AccountVenuePosition { venue, symbol, position_side, size, average_price }
    }
}

impl AccountVenuePosition {
    /// This position without its venue, for a row under an account that names it.
    #[must_use]
    pub fn without_venue(self) -> AccountPosition {
        let Self { venue: _, symbol, position_side, size, average_price } = self;
        AccountPosition { symbol, position_side, size, average_price }
    }
}

#[path = "account_position_tests.rs"]
#[cfg(test)]
mod account_position_tests;
