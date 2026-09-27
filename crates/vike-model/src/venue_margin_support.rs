//! `VenueMarginSupport` — the STATIC per-venue declaration of **what the exchange OFFERS** on the
//! margin-mode axis.
//!
//! ## One table, one subject
//! This table answers only *"what does the VENUE make available?"* — the modes it exposes, the
//! mechanism it exposes them through, and whether an isolated position's wallet can be topped up
//! after the position is open. It deliberately says **nothing about what vike sends**.
//!
//! The two facts about *vike's own* behavior on this axis live together in
//! [`crate::venue_caps::VenueCaps`], the table the core actually ENFORCES:
//! - [`crate::venue_caps::VenueCaps::margin_modes`] — the modes an `OrderRequest.margin_mode` is
//!   honored for ([`crate::venue_caps::preflight_order`] refuses anything else).
//! - [`crate::venue_caps::VenueCaps::default_margin_mode`] — the mode vike PRODUCES on the wire
//!   when no mode is requested.
//!
//! `default_margin_mode` used to live HERE, which is what made the two tables read as peers: both
//! were named `*Caps`, only one was enforced, and two of vike's own facts sat on opposite sides of
//! the split. Moving it put every vike-side fact in the enforced table and left this one purely
//! about the exchange. The relationship between them — **what we send ⊆ what they offer**, and the
//! default we send is one they actually offer — is pinned by `venue_caps`'
//! `margin_modes_cross_pin_venue_margin_support`.
//!
//! ## Where this lives and why
//! In `vike-model` (not `vike-bridge-core`), a sibling of `venue_caps`, for the same reason: it is a
//! pure data descriptor over types already here, and `vike-model` is the bottom layer every crate —
//! the bridges, the app shell, the chart, `vike-backfill` — already depends on, so all of them reach
//! the same values with no new dependency and no layering inversion.
//!
//! ## ⚠ Evidence class: this table is VENDOR-DOC TRANSCRIPTION, not verified fact
//! `venue_caps` rows are read off adapter code and can be tied back to it (that module lists the
//! checks which drive a real builder). **The three fields here cannot be, and no check in this repo
//! proves them.** Each is transcribed from the exchange's published API documentation:
//!
//! - [`VenueMarginSupport::offered_modes`] — the modes the exchange exposes.
//! - [`VenueMarginSupport::switch_mechanism`] — the endpoint or field shape that switches mode
//!   (binance/aster `POST /fapi/v*/marginType`; bybit UTA `/v5/account/set-margin-mode`; okx's
//!   per-order `tdMode`; hyperliquid's `updateLeverage` action `isCross` flag).
//! - [`VenueMarginSupport::isolated_wallet_adjustable`] — whether margin can be added or removed
//!   after an isolated position is open.
//!
//! **Neither an endpoint probe nor the repo can upgrade that evidence.** Margin mode is a
//! per-account (bybit UTA) or per-symbol (binance/aster) SETTING, reachable only behind
//! AUTHENTICATED endpoints, and it is absent from every venue's PUBLIC instrument metadata —
//! verified by probe 2026-08-05. No repo code calls any of those endpoints either, so there is no
//! adapter behavior a row could be tied to. Rows are therefore declared CONSERVATIVELY wherever the
//! vendor doc was not conclusive (see `isolated_wallet_adjustable`'s held-`false` set), and a green
//! row here means only *"this is what the vendor doc said"* — never *"this was observed"*.
//!
//! The one fact on this axis that IS checkable against adapter code is `default_margin_mode` — the
//! mode vike itself emits — and that is precisely the one which now lives in `venue_caps`, where
//! okx's row is tied to the real `crates/bridges/okx/src/perp.rs`'s `swap_td_mode` builder.
//!
//! ## Why this table is per-VENUE, and where that stops being enough
//! Margin support is not always uniform across a venue's instruments, and a per-venue row
//! structurally cannot say so. Hyperliquid is the live counterexample: its **public** `meta`
//! endpoint publishes per-asset `onlyIsolated` and `marginMode` fields, and **9 of 232 assets are
//! isolated-only** — HPOS, RLB, UNIBOT, OX, FRIEND, SHIA, NFTI, PANDORA and CASHCAT (measured
//! 2026-08-05, re-verified against the live body twice the same day: still 9 of 232, same names;
//! all nine also carry `marginMode: "strictIsolated"`, `maxLeverage: 3`, `marginTableId: 3`).
//! Eight of the nine are `isDelisted: true` — **CASHCAT is not**, so this is a live asset one can
//! still hold a position on, not a historical curiosity.
//!
//! That is a per-ASSET constraint, and expressing it means a per-instrument axis, not another column
//! here — which is why this table stops at the venue grain deliberately. `onlyIsolated` is now
//! **parsed and carried there**: `crates/bridges/hyperliquid/src/symbology.rs`'s
//! `InstrumentRef::only_isolated`, read by that module's `parse_only_isolated` off the same
//! `meta.universe` row the adapter already reads `maxLeverage` from, and reachable through
//! `HyperliquidInstruments::symbology`. It landed on `InstrumentRef` rather than `SymbolProperties`
//! for the reason `max_leverage` did: that struct is the venue-neutral ROUNDING grid, and a
//! margin-mode restriction is not a grid fact. (`marginMode`/`marginTableId` remain unread.)
//!
//! ⚠ **Parsed and legible, deliberately NOT enforced.** [`crate::venue_caps`]'s `HYPERLIQUID` row
//! declares `margin_modes: &[MarginMode::Cross]`, so [`crate::venue_caps::preflight_order`] already
//! refuses an explicit `Isolated` on this venue outright — no order can express the mode that flag
//! constrains, so there is no enforcement point for it to sit at yet. The moment that row gains
//! `Isolated`, a per-asset check belongs wherever the mode is chosen, reading that field. Until then
//! the honest status is: the venue fact is recorded and tested, the gate is still `margin_modes`.
//!
//! ## The declaration/venue divergence on those 9, and why it is benign TODAY
//! For an isolated-only asset the mode that RULES is `Isolated`, while
//! [`crate::venue_caps::VenueCaps::default_margin_mode`] — a per-VENUE field — says `Cross`; and the
//! venue sides with the asset, since `crates/bridges/hyperliquid/src/recon_client.rs`'s
//! `parse_positions` maps `leverage.type == "isolated"` → `MarginMode::Isolated`. Traced through the
//! consumers (2026-08-05) that mismatch reaches nothing:
//!
//! - **Reconcile cannot see it.** `vike_exec::recon::diff` never reads
//!   `PositionStatusReport.margin_mode` — the field appears in that file only inside its
//!   `#[cfg(test)]` fixtures — and no `Divergence` variant is margin-shaped. So the difference
//!   raises no divergence, no operator alert and no synthesized event, under any `VIKE_RECONCILE`
//!   policy.
//! - **Nothing reads the declaration.** Workspace-wide, `default_margin_mode` is named only by this
//!   module's docs, `venue_caps`' own tests, and okx's `okx_default_margin_mode_matches_the_td_mode_builder`.
//! - **The live margin/liquidation consumers read venue truth instead.** `vike_exec`'s gate margin
//!   fold and `margin_call`'s pool partition, and `vike_core`'s liquidation-price badge, all key on
//!   the per-position `PositionEntry.margin_mode` — which the reconcile snapshot apply overwrites
//!   with whatever the venue reported (venue truth wins the overwrite).
//!
//! So the divergence is a DOC-level falsehood, not a live fault, and the fix is at that level:
//! `default_margin_mode` now documents itself as "the default for assets that HAVE a choice", and
//! the per-asset narrowing is derived — not re-declared — by
//! `crates/bridges/hyperliquid/src/symbology.rs`'s `InstrumentRef::effective_margin_mode`
//! (`only_isolated ? Isolated : caps_for(venue).default_margin_mode`), pinned in both directions by
//! that module's `effective_margin_mode_is_per_asset_not_per_venue`.
//!
//! ⚠ **One residual, unfixed on purpose.** A position opened by a FILL is booked locally with
//! `PositionEntry::default()` — Cross — until a reconcile pass corrects it, so on CASHCAT the gate's
//! margin fold and the liquidation badge use the cross law for that window. Closing it means
//! injecting a per-symbol mode source into `vike_exec`'s fill fold, which would change the
//! RiskGate's admit arithmetic — i.e. it could change whether an order is admitted. That is an
//! order-path change and belongs to its own PR, not to a documentation correction.

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

/// What ONE venue OFFERS on the margin-mode axis. `Copy` (a `&'static` slice, one enum, one bool)
/// so it is cheap to hand around by value.
///
/// Every field is vendor-doc transcription — see the module doc's evidence-class note. For what
/// vike SENDS, see [`crate::venue_caps::VenueCaps::margin_modes`] and
/// [`crate::venue_caps::VenueCaps::default_margin_mode`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VenueMarginSupport {
    /// Every margin mode this venue OFFERS — the exchange's capability, NOT what vike wires (that
    /// is [`crate::venue_caps::VenueCaps::margin_modes`], a subset of this).
    pub offered_modes: &'static [MarginMode],
    /// How the venue exposes a mode switch (see [`SwitchMechanism`]).
    pub switch_mechanism: SwitchMechanism,
    /// Whether the venue lets you ADD/REMOVE margin to an ISOLATED position after it is opened (a
    /// venue-capability flag for the later isolated-margin work). `false` for venues with no
    /// isolated mode. NOTE: vike wires NO margin-adjust endpoint today, so this reflects the venue's
    /// documented capability, not repo-verified behavior. The step-1 user-confirmed `true` set is
    /// binance/bybit/okx; aster (a binance-fork whose `/fapi` endpoint is identical) and hyperliquid
    /// (`updateIsolatedMargin`) also support it in practice but are held conservatively `false` here
    /// pending live verification.
    pub isolated_wallet_adjustable: bool,
}

impl VenueMarginSupport {
    /// The conservative fallback for an unknown venue string: cross-only, no switch, no isolated
    /// wallet adjustment. Nothing beyond cross is offered until a venue's row proves it.
    pub const UNKNOWN: VenueMarginSupport = VenueMarginSupport {
        offered_modes: &[MarginMode::Cross],
        switch_mechanism: SwitchMechanism::NotApplicable,
        isolated_wallet_adjustable: false,
    };

    /// Whether this venue offers a genuine margin-mode choice — more than one offered mode AND a
    /// real switch mechanism. Fixed-mode venues (FX/equity/polymarket/deribit) return false.
    #[inline]
    #[must_use]
    pub const fn is_switchable(&self) -> bool {
        self.offered_modes.len() > 1
            && !matches!(self.switch_mechanism, SwitchMechanism::NotApplicable)
    }
}

impl Default for VenueMarginSupport {
    fn default() -> Self {
        VenueMarginSupport::UNKNOWN
    }
}

// ---------------------------------------------------------------------------------------------
// The per-venue rows. Each cites the adapter code the value was read from.
// ---------------------------------------------------------------------------------------------

/// Binance USDⓈ-M futures. The order builder (`binance/src/family/order_map.rs`, perp params) sends
/// NO `marginType` — only `positionSide`/`reduceOnly` — so the account's per-symbol default (cross)
/// rules; the exec thread's one-shot is set-LEVERAGE only
/// (`crates/bridges/binance/src/exec.rs`'s `DEFAULT_LEVERAGE`). Binance exposes cross/isolated per
/// SYMBOL via `POST /fapi/v1/marginType`, and isolated positions accept margin add/remove
/// (`/fapi/v1/positionMargin`). → cross today, PerSymbolEndpoint, iso-adjustable.
pub const BINANCE: VenueMarginSupport = VenueMarginSupport {
    offered_modes: &[MarginMode::Cross, MarginMode::Isolated],
    switch_mechanism: SwitchMechanism::PerSymbolEndpoint,
    isolated_wallet_adjustable: true,
};

/// Aster DEX USDⓈ-M perp — a binance-fork API. Same shape: `aster/src/perp.rs::build_order_params`
/// sends no `marginType` (positionSide=BOTH + reduceOnly only), one-shot set-leverage at start
/// (`crates/bridges/aster/src/exec.rs`'s `DEFAULT_LEVERAGE`). Cross/isolated per symbol via
/// `POST /fapi/v3/marginType`. → cross today, PerSymbolEndpoint. `isolated_wallet_adjustable` held
/// conservatively `false` (the binance `/fapi` positionMargin twin exists but is unverified for
/// this venue in-repo).
pub const ASTER: VenueMarginSupport = VenueMarginSupport {
    offered_modes: &[MarginMode::Cross, MarginMode::Isolated],
    switch_mechanism: SwitchMechanism::PerSymbolEndpoint,
    isolated_wallet_adjustable: false,
};

/// OKX SWAP perp — the FIRST venue whose adapter honors the per-order requested mode (the step-2
/// un-hardcode this row always pointed at). `okx/src/perp.rs::build_order_params` maps
/// `OrderRequest.margin_mode` → `tdMode` via `swap_td_mode`: unset = the historical `"cross"`
/// byte-for-byte (which is why `venue_caps`' `OKX.default_margin_mode` is Cross — and, uniquely on
/// the roster, is TIED to that builder by `okx_default_margin_mode_matches_the_td_mode_builder`),
/// `Isolated` sends `"isolated"`, and `Cash` is
/// DENIED with a synthesized terminal `OrderRejected` (spot-only on OKX; the adapter trades SWAP —
/// deny loudly, never coerce). `offered_modes` keeps all three: `tdMode` is a PER-ORDER field
/// accepting `"cash"` / `"cross"` / `"isolated"` at the VENUE (cash on its spot books).
/// Set-leverage still sends the one-shot `mgnMode: "cross"`. Isolated positions accept margin
/// add/reduce (`/account/position/margin-balance`). → PerOrderField, iso-adjustable.
pub const OKX: VenueMarginSupport = VenueMarginSupport {
    offered_modes: &[MarginMode::Cash, MarginMode::Cross, MarginMode::Isolated],
    switch_mechanism: SwitchMechanism::PerOrderField,
    isolated_wallet_adjustable: true,
};

/// Bybit V5 linear perp (UTA). `bybit/src/perp.rs::build_order_params` sends no margin field; the
/// exec thread sets leverage only (`crates/bridges/bybit/src/exec.rs`'s `DEFAULT_LEVERAGE`, posted
/// by `set_leverage`). In a Unified Trading Account the margin mode is an ACCOUNT-wide setting
/// (`/v5/account/set-margin-mode`). vike touches neither, so the account default (cross) rules;
/// isolated positions accept margin add (`/v5/position/add-margin`). → cross today, AccountLevel,
/// iso-adjustable. (UTA portfolio mode is a maint-rate variation within cross — see [`MarginMode`]
/// — so it is NOT a separate supported mode here.)
pub const BYBIT: VenueMarginSupport = VenueMarginSupport {
    offered_modes: &[MarginMode::Cross, MarginMode::Isolated],
    switch_mechanism: SwitchMechanism::AccountLevel,
    isolated_wallet_adjustable: true,
};

/// Hyperliquid spot+perp. `hyperliquid/src/exec.rs` wires no leverage/margin action at all — the
/// per-asset default (cross) rules. HL selects the mode via the `updateLeverage` action's `isCross`
/// flag (the reconcile parser already reads position `leverage:{type:"cross"|"isolated"}` —
/// `hyperliquid/src/recon_client.rs`). → cross today, AtLeverageSet. `isolated_wallet_adjustable`
/// held conservatively `false` (HL `updateIsolatedMargin` exists but is unverified in-repo).
pub const HYPERLIQUID: VenueMarginSupport = VenueMarginSupport {
    offered_modes: &[MarginMode::Cross, MarginMode::Isolated],
    switch_mechanism: SwitchMechanism::AtLeverageSet,
    isolated_wallet_adjustable: false,
};

/// Deribit (options + perp). `deribit/src/exec.rs`/`client.rs` send no margin field on the order —
/// margin is a SUBACCOUNT-level model with no per-position isolated mode. Deribit runs a cross model;
/// its optional portfolio margin is a maintenance-RATE variation within cross (see [`MarginMode`]),
/// NOT a separate mode. → single-mode cross, NotApplicable, no isolated wallet.
pub const DERIBIT: VenueMarginSupport = VenueMarginSupport {
    offered_modes: &[MarginMode::Cross],
    switch_mechanism: SwitchMechanism::NotApplicable,
    isolated_wallet_adjustable: false,
};

/// Polymarket CLOB — a fully collateralized prediction market. Orders carry no leverage or
/// reduce-only (`polymarket/src/order.rs`, `client.rs`); every position is 100% funded, so it can
/// never liquidate. → cash, NotApplicable.
pub const POLYMARKET: VenueMarginSupport = VenueMarginSupport {
    offered_modes: &[MarginMode::Cash],
    switch_mechanism: SwitchMechanism::NotApplicable,
    isolated_wallet_adjustable: false,
};

/// OANDA v20 (FX/CFD). Positions draw on shared account margin; there is no per-position
/// cross/isolated toggle. → single-mode cross (shared account pool), NotApplicable.
pub const OANDA: VenueMarginSupport = VenueMarginSupport::UNKNOWN;

/// IG (FX/CFD). Shared account margin, no isolated concept. → cross, NotApplicable.
pub const IG: VenueMarginSupport = VenueMarginSupport::UNKNOWN;

/// FXCM ForexConnect (FX/CFD). Shared account margin, no isolated concept. → cross, NotApplicable.
pub const FXCM: VenueMarginSupport = VenueMarginSupport::UNKNOWN;

/// Dukascopy (FX; JForex sidecar). Shared account margin, no isolated concept. → cross,
/// NotApplicable.
pub const DUKASCOPY: VenueMarginSupport = VenueMarginSupport::UNKNOWN;

/// Alpaca (US equities + crypto). A single account with shared (Reg-T) margin — no per-position
/// crypto-style isolated mode. → cross (shared account pool), NotApplicable.
pub const ALPACA: VenueMarginSupport = VenueMarginSupport::UNKNOWN;

/// Interactive Brokers (equities/futures/options). Account-level margin (Reg-T / portfolio-margin
/// account TYPE, a maint-rate variation within cross), not a per-position crypto cross/isolated
/// switch. → cross, NotApplicable.
pub const IBKR: VenueMarginSupport = VenueMarginSupport::UNKNOWN;

/// cTrader (FX/CFD). Shared account margin, no isolated concept — the conservative FX row this
/// module's registry note always prescribed for it, value-identical to the [`VenueMarginSupport::UNKNOWN`]
/// fallback the registry served before the row was named (byte-identical add, made when the
/// canonical roster forced every venue to have a NAMED row). → cross, NotApplicable.
pub const CTRADER: VenueMarginSupport = VenueMarginSupport::UNKNOWN;

// vike:new-venue:row /// TODO(new-venue: {venue}): transcribe the VENDOR DOC (this table's evidence class — see the
// vike:new-venue:row /// module doc). The scaffolded value is the conservative cross-only row, which is a real
// vike:new-venue:row /// classification and not a silent fallback ONLY because it is named here.
// vike:new-venue:row pub const {VENUE}: VenueMarginSupport = VenueMarginSupport::UNKNOWN;
// vike:new-venue:row
/// The registry: the declared [`VenueMarginSupport`] for a canonical venue string — one arm per
/// [`crate::venues::VENUES`] entry, mirroring [`crate::venue_caps::caps_for`]; an unknown venue
/// returns [`VenueMarginSupport::UNKNOWN`] (conservative cross-only, fail-closed).
#[must_use]
pub fn venue_margin_support(venue: &str) -> VenueMarginSupport {
    match venue {
        "binance" => BINANCE,
        "bybit" => BYBIT,
        "okx" => OKX,
        "deribit" => DERIBIT,
        "oanda" => OANDA,
        "ig" => IG,
        "fxcm" => FXCM,
        "dukascopy" => DUKASCOPY,
        "polymarket" => POLYMARKET,
        "ibkr" => IBKR,
        "ctrader" => CTRADER,
        "alpaca" => ALPACA,
        "aster" => ASTER,
        "hyperliquid" => HYPERLIQUID,
        // vike:new-venue:row "{venue}" => {VENUE}, // TODO(new-venue: {venue}): a NAMED row, even when it equals the fallback
        _ => VenueMarginSupport::UNKNOWN,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every known venue in the registry, paired with its expected row. The completeness test
    /// below asserts this covers the canonical roster (`crate::venues::VENUES`) exactly.
    ///
    /// `#[rustfmt::skip]`: a `just new-venue` marker at the TAIL of a bracketed literal is
    /// re-indented by rustfmt once a row ending in a trailing `//` comment is generated above it,
    /// which defeats `--remove`. Gated by `crates/vike-ops/tests/new_venue_gate.rs`'s
    /// `a_trailing_comment_marker_is_rustfmt_skipped_unless_a_recognised_sibling_follows`.
    #[rustfmt::skip]
    const MATRIX: &[(&str, VenueMarginSupport)] = &[
        ("binance", BINANCE),
        ("bybit", BYBIT),
        ("okx", OKX),
        ("deribit", DERIBIT),
        ("oanda", OANDA),
        ("ig", IG),
        ("fxcm", FXCM),
        ("dukascopy", DUKASCOPY),
        ("polymarket", POLYMARKET),
        ("ibkr", IBKR),
        ("ctrader", CTRADER),
        ("alpaca", ALPACA),
        ("aster", ASTER),
        ("hyperliquid", HYPERLIQUID),
        // vike:new-venue:row ("{venue}", {VENUE}), // TODO(new-venue: {venue}): declared-row completeness
    ];

    /// The full matrix, pinned verbatim (like the tif matrix test). Any drift in a venue row is
    /// loud here. Each tuple is (venue, offered_modes, switch_mechanism, iso_adj) — the mode vike
    /// PRODUCES is not this table's subject and is pinned by `venue_caps`'
    /// `default_margin_mode_reflects_current_behavior`.
    #[test]
    fn margin_matrix_is_pinned() {
        use MarginMode::{Cash, Cross, Isolated};
        use SwitchMechanism::{
            AccountLevel, AtLeverageSet, NotApplicable, PerOrderField, PerSymbolEndpoint,
        };
        #[rustfmt::skip]
        let rows: &[(&str, &[MarginMode], SwitchMechanism, bool)] = &[
            // crypto perps — each exposes isolated via a different mechanism
            ("binance",     &[Cross, Isolated],       PerSymbolEndpoint, true),
            ("aster",       &[Cross, Isolated],       PerSymbolEndpoint, false),
            ("okx",         &[Cash, Cross, Isolated], PerOrderField,     true),  // adapter honors margin_mode (unset=cross)
            ("bybit",       &[Cross, Isolated],       AccountLevel,      true),
            ("hyperliquid", &[Cross, Isolated],       AtLeverageSet,     false),
            // deribit — single cross model (portfolio margin is a maint-rate variation within cross)
            ("deribit",     &[Cross],                 NotApplicable,     false),
            // polymarket — fully collateralized, no margin axis (never liquidates)
            ("polymarket",  &[Cash],                  NotApplicable,     false),
            // FX/CFD + equity — shared account margin, no isolated concept: single-mode cross
            ("oanda",       &[Cross],                 NotApplicable,     false),
            ("ig",          &[Cross],                 NotApplicable,     false),
            ("fxcm",        &[Cross],                 NotApplicable,     false),
            ("dukascopy",   &[Cross],                 NotApplicable,     false),
            ("ctrader",     &[Cross],                 NotApplicable,     false),
            ("alpaca",      &[Cross],                 NotApplicable,     false),
            ("ibkr",        &[Cross],                 NotApplicable,     false),
            // vike:new-venue:row ("{venue}",  &[Cross],                 NotApplicable,     false), // TODO(new-venue: {venue}): pin the vendor doc
        ];
        for (venue, modes, mech, iso_adj) in rows {
            let got = venue_margin_support(venue);
            assert_eq!(got.offered_modes, *modes, "{venue} offered_modes");
            assert_eq!(got.switch_mechanism, *mech, "{venue} switch_mechanism");
            assert_eq!(
                got.isolated_wallet_adjustable, *iso_adj,
                "{venue} isolated_wallet_adjustable"
            );
        }
        assert_eq!(rows.len(), MATRIX.len(), "matrix test row count == registry size");
    }

    /// Completeness: every registry venue resolves to its declared const (no missing row), and the
    /// registry equals the consts (so the pinned matrix above is authoritative).
    #[test]
    fn registry_maps_every_known_venue() {
        for (venue, want) in MATRIX {
            assert_eq!(venue_margin_support(venue), *want, "{venue}");
        }
    }

    /// Completeness vs the canonical roster: every [`crate::venues::VENUES`] entry has a NAMED
    /// declared row in `MATRIX`, and the registry serves exactly it. Adding a venue to the roster
    /// fails here until its row exists.
    #[test]
    fn every_roster_venue_has_a_declared_row() {
        assert_eq!(MATRIX.len(), crate::venues::VENUES.len(), "one declared row per roster venue");
        for &v in crate::venues::VENUES {
            let (_, want) = MATRIX.iter().find(|(rv, _)| *rv == v).unwrap_or_else(|| {
                panic!("no VenueMarginSupport row declared for roster venue {v}")
            });
            assert_eq!(venue_margin_support(v), *want, "{v}: registry must serve its declared row");
        }
    }

    /// An unknown venue is the conservative cross-only fixed row (fail-closed), and equals Default.
    #[test]
    fn unknown_venue_is_conservative_cross_only() {
        let c = venue_margin_support("nasdaq");
        assert_eq!(c, VenueMarginSupport::UNKNOWN);
        assert_eq!(c, VenueMarginSupport::default());
        assert_eq!(c.offered_modes, &[MarginMode::Cross]);
        assert_eq!(c.switch_mechanism, SwitchMechanism::NotApplicable);
        assert!(!c.isolated_wallet_adjustable);
        assert!(!c.is_switchable());
    }

    /// `is_switchable`: only the venues that offer >1 mode AND a real mechanism. The 5 crypto-perp
    /// venues are switchable; deribit + the fixed-mode FX/equity/polymarket venues are not.
    #[test]
    fn switchable_matches_multi_mode_venues() {
        for v in ["binance", "aster", "okx", "bybit", "hyperliquid"] {
            assert!(venue_margin_support(v).is_switchable(), "{v} offers a mode switch");
        }
        for v in ["deribit", "polymarket", "oanda", "ig", "fxcm", "dukascopy", "alpaca", "ibkr"] {
            assert!(!venue_margin_support(v).is_switchable(), "{v} is fixed-mode");
        }
    }

    /// The switch-mechanism split, pinned per family: OKX is the lone PER-ORDER field (the hardcode
    /// step 2 flips); binance/aster are per-symbol; bybit account-level; hyperliquid
    /// at-leverage-set; everyone else NotApplicable.
    #[test]
    fn switch_mechanisms_are_grouped_correctly() {
        assert_eq!(venue_margin_support("okx").switch_mechanism, SwitchMechanism::PerOrderField);
        for v in ["binance", "aster"] {
            assert_eq!(
                venue_margin_support(v).switch_mechanism,
                SwitchMechanism::PerSymbolEndpoint
            );
        }
        assert_eq!(venue_margin_support("bybit").switch_mechanism, SwitchMechanism::AccountLevel);
        assert_eq!(
            venue_margin_support("hyperliquid").switch_mechanism,
            SwitchMechanism::AtLeverageSet
        );
        for v in ["deribit", "polymarket", "oanda", "ig", "fxcm", "dukascopy", "alpaca", "ibkr"] {
            assert_eq!(
                venue_margin_support(v).switch_mechanism,
                SwitchMechanism::NotApplicable,
                "{v}"
            );
        }
    }

    /// OKX is the only venue exposing the `Cash` mode (spot/margin `tdMode:"cash"`) alongside
    /// cross+isolated; the other perp venues offer cross+isolated; polymarket is cash-only.
    #[test]
    fn cash_mode_is_okx_and_polymarket() {
        assert!(venue_margin_support("okx").offered_modes.contains(&MarginMode::Cash));
        assert_eq!(venue_margin_support("polymarket").offered_modes, &[MarginMode::Cash]);
        for v in ["binance", "aster", "bybit", "hyperliquid"] {
            assert!(!venue_margin_support(v).offered_modes.contains(&MarginMode::Cash), "{v}");
            assert!(venue_margin_support(v).offered_modes.contains(&MarginMode::Isolated), "{v}");
        }
    }

    /// `isolated_wallet_adjustable` — the step-1 user-confirmed `true` set is binance/bybit/okx;
    /// every other venue (including aster/hyperliquid, held conservatively) is `false`.
    #[test]
    fn isolated_wallet_adjustable_is_the_confirmed_set() {
        for v in ["binance", "bybit", "okx"] {
            assert!(venue_margin_support(v).isolated_wallet_adjustable, "{v} adjustable");
        }
        for v in [
            "aster",
            "hyperliquid",
            "deribit",
            "polymarket",
            "oanda",
            "ig",
            "fxcm",
            "dukascopy",
            "alpaca",
            "ibkr",
        ] {
            assert!(!venue_margin_support(v).isolated_wallet_adjustable, "{v} not adjustable");
        }
    }

    /// MarginMode serde round-trips (the enum carries `serde` per the design). Default is Cross;
    /// exactly three variants exist.
    #[test]
    fn margin_mode_serde_and_default() {
        assert_eq!(MarginMode::default(), MarginMode::Cross);
        for m in [MarginMode::Cash, MarginMode::Cross, MarginMode::Isolated] {
            let s = serde_json::to_string(&m).expect("serialize");
            let back: MarginMode = serde_json::from_str(&s).expect("deserialize");
            assert_eq!(back, m);
        }
        assert_eq!(serde_json::to_string(&MarginMode::Cross).unwrap(), "\"Cross\"");
        assert_eq!(serde_json::to_string(&MarginMode::Cash).unwrap(), "\"Cash\"");
    }

    #[test]
    fn default_is_cross_and_predicates_hold() {
        assert_eq!(MarginMode::default(), MarginMode::Cross);
        assert!(MarginMode::default().is_cross());
        assert!(!MarginMode::default().is_isolated());
        assert!(MarginMode::Isolated.is_isolated());
        assert!(!MarginMode::Cash.is_cross());
    }
}
