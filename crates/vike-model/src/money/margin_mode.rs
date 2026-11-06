//! `MarginMode` and `SwitchMechanism` — the margin vocabulary an order, a position and the liquidation
//! law name, and the per-venue margin table (`vike-venues`' `venue_margin_support`) declares in.
//!
//! They sit in the vocabulary crate, not beside the table, because `money::liquidation` and
//! `orders::order` name them and the vocabulary may not depend on the crate that holds the per-venue
//! tables (decision 0162). Moved here byte-for-byte from `venue_margin_support.rs`.

/// A venue's margin mode for a position/order. Exactly THREE variants — the three that map to a
/// distinct LIQUIDATION law. `Default` is [`MarginMode::Cross`] because that is what every venue
/// vike trades produces today (the universal current behavior).
///
/// ## The liquidation law each mode inherits (for the later liquidation-law PR)
/// - [`MarginMode::Cash`] — fully funded, no borrow. It structurally CANNOT breach maintenance
///   (there is no leverage), so it **never liquidates**. This is why it is a first-class variant and
///   not merely "cross with headroom": spot everywhere, Polymarket, and OKX `tdMode:"cash"` are
///   genuinely this.
/// - [`MarginMode::Cross`] — one shared account collateral pool backs every position; a maintenance
///   breach runs the **losers-first partial** de-risking workflow across the book.
/// - [`MarginMode::Isolated`] — each position has its own walled-off margin wallet; a breach closes
///   THAT position and the loss is **capped at its margin**.
///
/// ## Why there is no `Portfolio` variant
/// Portfolio margin (Deribit PM, Bybit UTA portfolio mode) is NOT a distinct margin MODE: it shares
/// the SAME cross collateral pool and the SAME cross liquidation scope. Only its maintenance-RATE
/// computation differs (risk-based netting instead of a linear per-position sum). It is therefore a
/// maintenance-rate variation WITHIN [`MarginMode::Cross`], not a separate mode — folding it in keeps
/// the liquidation law from fracturing. (If a future maint-rate model needs it, it belongs on a
/// separate rate axis, not here.)
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Default, serde::Serialize, serde::Deserialize,
)]
pub enum MarginMode {
    /// Fully collateralized, no borrow/leverage — every order is 100% funded and can never breach
    /// maintenance (never liquidates). Polymarket, spot books, OKX `tdMode:"cash"`.
    Cash,
    /// Shared account-wide margin pool: all positions draw on one collateral balance; a maintenance
    /// breach de-risks losers-first across the book. The crypto-perp default and the closest analog
    /// for the FX/CFD and equity venues (shared / Reg-T account margin). What vike produces today.
    #[default]
    Cross,
    /// Per-position isolated margin: each position's collateral (and its liquidation) is walled off;
    /// loss is capped at that position's margin. Offered by the crypto-perp venues
    /// (binance/bybit/okx/aster/hyperliquid) but NOT selected by vike yet.
    Isolated,
}

impl MarginMode {
    /// `true` for `Cross`. Used as the `skip_serializing_if` predicate on the per-position
    /// `margin_mode` field so a cross position serializes to NOTHING — keeping existing
    /// snapshots/journals (and the `state_hash` determinism fence) byte-identical.
    pub fn is_cross(&self) -> bool {
        matches!(self, MarginMode::Cross)
    }

    /// `true` for `Isolated`.
    pub fn is_isolated(&self) -> bool {
        matches!(self, MarginMode::Isolated)
    }
}

/// HOW a venue exposes a margin-mode switch — the shape step 2 must drive to change a mode. Records
/// the venue's real mechanism even where vike does not use it yet. A single-mode venue (nothing to
/// switch to) is [`SwitchMechanism::NotApplicable`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum SwitchMechanism {
    /// The mode rides a field on EACH order (OKX `tdMode: "cash"|"cross"|"isolated"`). Switchable
    /// per-order at submit time.
    PerOrderField,
    /// A dedicated per-SYMBOL endpoint sets the mode out-of-band and orders then inherit it
    /// (binance/aster `POST /fapi/v*/marginType`).
    PerSymbolEndpoint,
    /// The mode is chosen as part of the set-leverage action, per asset (hyperliquid's
    /// `updateLeverage` action carries an `isCross` flag).
    AtLeverageSet,
    /// The mode is an ACCOUNT / subaccount-wide setting, not per order or per symbol (bybit UTA
    /// `set-margin-mode`).
    AccountLevel,
    /// No selectable margin-mode switch exists at this venue (single supported mode: FX/CFD,
    /// equities, fully collateralized prediction markets, or deribit's fixed cross model).
    NotApplicable,
}
