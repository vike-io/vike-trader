//! The capability types; the per-venue rows and the preflight guard stay in the parent module.

#[cfg(doc)]
use super::{ORDER_KINDS, preflight_order};
use crate::orders::order::TimeInForce;
use crate::venues::venue_margin_support::MarginMode;

/// Which live market-data verbs a venue's `vike_data::DataClient` actually serves. A venue with no
/// live feed at all (the FX/CFD venues, dukascopy) has every field `false`.
///
/// `book` is the lossless L2 tick lane (`subscribe_book` → `LiveDataSink::book`/`book_update`);
/// `depth` is the conflating L2 snapshot lane (`subscribe_depth` → `LiveDataSink::l2_snapshot`,
/// what the DOM renders). They are separate capabilities on purpose — the crypto venues serve
/// `depth` but not `book`, polymarket serves `book` but not `depth`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct LiveDataCaps {
    /// live closed + forming bars (`subscribe_bars`)
    pub bars: bool,
    /// live L1 quotes (`subscribe_quotes`)
    pub quotes: bool,
    /// live executed-trade prints (`subscribe_trades`)
    pub trades: bool,
    /// live L2 book updates, lossless tick lane (`subscribe_book`)
    pub book: bool,
    /// live L2 depth snapshots, conflating DOM lane (`subscribe_depth`)
    pub depth: bool,
}

impl LiveDataCaps {
    /// A venue with no live market-data feed at all.
    pub const NONE: LiveDataCaps =
        LiveDataCaps { bars: false, quotes: false, trades: false, book: false, depth: false };

    /// Whether this venue serves `verb` — the ONE lookup a feed's refusal path should consult
    /// instead of hand-rolling its own `Unsupported` decision (the vike-data `require_live_verb`
    /// helper wraps this into the canonical `LiveDataError::Unsupported`).
    #[inline]
    #[must_use]
    pub const fn supports(&self, verb: LiveVerb) -> bool {
        match verb {
            LiveVerb::Bars => self.bars,
            LiveVerb::Quotes => self.quotes,
            LiveVerb::Trades => self.trades,
            LiveVerb::Book => self.book,
            LiveVerb::Depth => self.depth,
        }
    }
}

/// The five live market-data verbs of the `vike_data::DataClient` seam, as an enum so a refusal
/// can be DRIVEN off a venue's declared [`LiveDataCaps`] row instead of each feed hand-writing
/// its own `Unsupported` arm (spec: one declaration, many consumers).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LiveVerb {
    /// `subscribe_bars`
    Bars,
    /// `subscribe_quotes`
    Quotes,
    /// `subscribe_trades`
    Trades,
    /// `subscribe_book` (lossless L2 tick lane)
    Book,
    /// `subscribe_depth` (conflating DOM snapshot lane)
    Depth,
}

/// Which native TRIGGER order kinds a venue adapter wires (the venue holds the trigger). An
/// EMPTY `trigger_types` slice means trigger orders are unsupported on that adapter — the
/// preflight refuses them rather than letting a "stop" silently coerce into an immediate
/// market/limit order (the deribit/alpaca `_ => market` fallthrough class).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TriggerType {
    /// `order_type: "stop"` — a stop-loss trigger (`trigger_price` armed venue-side; fires as
    /// market, or as limit where the adapter also wires a `"stop_limit"` kind).
    StopLoss,
    /// `order_type: "take_profit"` — a take-profit trigger (hyperliquid `tpsl:"tp"` is the one
    /// adapter that wires it natively today).
    TakeProfit,
}

/// The declared static capability matrix for ONE venue adapter. `Copy` (all fields are `bool` /
/// `&'static [_]`) so it is cheap to hand to a UI frame by value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VenueCaps {
    /// A native in-place modify/amend of a resting order is wired through this venue's
    /// `ExecutionClient` (vs the silent no-op trait default). Drives the Trade window ladder's
    /// drag-to-reprice control + the `Command::Modify` send gate.
    pub supports_modify: bool,
    /// A native batch submit/cancel endpoint is wired (vs the always-available per-order fan-out
    /// default). Informational: the fan-out default means batching always *works*, this flags a
    /// real venue batch API.
    pub supports_native_batch: bool,
    /// The adapter builds a reduce-only flag onto the wire order (perps/options). FX/CFD/prediction
    /// venues do not, and dukascopy is unverifiable → declared `false`.
    pub supports_reduce_only: bool,
    /// The adapter resolves a combo instrument at submit and wires `OrderRequest::combo_legs`
    /// onto the wire (`build_combo` leaves `symbol` EMPTY for the adapter to substitute — e.g.
    /// Deribit `private/create_combo`). Gates the `OrderIntent::Combo` lowering in `vike-core`:
    /// a combo for a venue declaring `false` is terminally REJECTED there, never sent.
    ///
    /// **Deribit is the ONE `true` row** (pinned by `combo_support_is_deribit_only` below):
    /// `DeribitRest::submit_order` routes a non-empty `combo_legs` through its combo book —
    /// `private/create_combo`, the orientation/scale solve (`vike_deribit::combo`), then
    /// `private/buy`/`private/sell` on the resolved combo id. Per the honesty rule above, no
    /// OTHER adapter reads `combo_legs` yet, so a combo on their wire would be an order with an
    /// EMPTY symbol — their rows stay `false` until they wire the resolve-at-submit step.
    pub supports_combo: bool,
    /// TIFs this adapter actually WIRES to the venue today. Flipped venues (binance/bybit/
    /// deribit, tif step-2) map the listed set 1:1 and LOUD-DENY the rest at submit (terminal
    /// `OrderRejected`, never a silent coercion); unflipped venues that hardcode GTC regardless
    /// of the request declare `&[Gtc]`; OANDA maps the full set. Where one venue id fronts
    /// lanes with DIVERGING TIF vocabularies (binance spot vs perp), this single-keyed set is
    /// the conservative lane INTERSECTION and the per-lane truth lives in
    /// [`crate::venues::venue_tif::venue_tif`] — see the `BINANCE` row doc. Cross-pinned against the
    /// `venue_tif` table by that module's own `venue_caps_cross_pin_the_tif_table` test so the
    /// two authorities can never drift.
    pub supported_tifs: &'static [TimeInForce],
    /// TIFs a limit `OrderRequest` may CARRY to this venue without being refused by the core
    /// preflight ([`preflight_order`]) — the ADMIT set, a superset of [`Self::supported_tifs`]:
    /// it additionally contains the TIFs the adapter still COERCES today (polymarket `Ioc→FOK`,
    /// hyperliquid `Fok→Ioc`, alpaca `Gtd→gtc` — live behavior; refusing them is a separate
    /// per-venue flip) and, for the lane-split binance id, the lane UNION (perp-lane GTD must
    /// not be refused at the core just because the spot lane denies it venue-side). A TIF
    /// OUTSIDE this set is either loud-denied venue-side already (the flipped venues — the
    /// preflight only moves that deny earlier) or silently IGNORED today (aster/ig and the
    /// tif-less protocols — the silent-lie class the preflight exists to make loud). Also
    /// cross-pinned against `venue_tif` (same test).
    pub accepted_tifs: &'static [TimeInForce],
    /// Canonical lowercase `OrderRequest.order_type` strings this adapter WIRES with honored
    /// semantics (from [`ORDER_KINDS`]). A kind absent here is either coerced into something
    /// else today (the `_ => MARKET` fallthroughs — a resting "stop" silently becoming an
    /// immediate market order) or refused venue-/sidecar-side; the preflight turns both into a
    /// loud terminal refusal at the core edge. Like `supported_tifs`, the lane-split binance id
    /// declares the lane UNION ("stop" is the perp lane's native STOP_MARKET; spot has no stop
    /// type and rejects it server-side).
    pub supported_order_kinds: &'static [&'static str],
    /// Native trigger order kinds wired (see [`TriggerType`]); EMPTY = trigger orders
    /// unsupported. Invariant (pinned by tests): non-empty ⇔ `supported_order_kinds` contains a
    /// trigger kind (`"stop"`/`"stop_limit"`/`"take_profit"`).
    pub trigger_types: &'static [TriggerType],
    /// The adapter can express a post-only (maker-only) order from an `OrderRequest`. FALSE for
    /// every venue today — `OrderRequest` has no post-only field, so no adapter can wire one
    /// (hyperliquid's `Alo` and bybit's `PostOnly` exist at the venues but are unreachable);
    /// the axis is declared now so the field's arrival flips rows, not schemas.
    pub supports_post_only: bool,
    /// Margin modes an `OrderRequest.margin_mode` request is actually HONORED for (a `None`
    /// request always passes — it means "the venue's default behavior"). OKX is the one adapter
    /// that drives a per-order mode (`tdMode`, cross/isolated; `Cash` is denied venue-side);
    /// every other adapter never reads the field, so only its default mode (which an explicit
    /// request trivially "honors") is listed — an explicit request for anything else would be a
    /// silent lie and is preflight-refused. Subset of
    /// `venue_margin_support(venue).offered_modes` (what the exchange offers) — pinned by
    /// `margin_modes_cross_pin_venue_margin_support`.
    pub margin_modes: &'static [MarginMode],
    /// The mode vike PRODUCES on the wire when an `OrderRequest` carries NO `margin_mode`
    /// ([`MarginMode::Cross`] for every perp venue; [`MarginMode::Cash`] for polymarket).
    ///
    /// This is **vike's own fact**, not the exchange's — which is why it lives here beside
    /// [`Self::margin_modes`] rather than in
    /// [`crate::venues::venue_margin_support::VenueMarginSupport`], whose subject is what the venue
    /// OFFERS. It is also the only field on the whole margin axis that can be checked against
    /// adapter code, because it describes bytes an adapter emits: okx's row is tied to the real
    /// `crates/bridges/okx/src/perp.rs`'s `swap_td_mode` builder by that module's
    /// `okx_default_margin_mode_matches_the_td_mode_builder`.
    ///
    /// Invariant (pinned): always a member of [`Self::margin_modes`] — the default vike sends must
    /// be a mode an explicit request could also name. For every venue except okx the adapter sets
    /// no margin field at all, so this records the ACCOUNT-side default that consequently rules —
    /// vendor-doc knowledge, not repo-verifiable.
    ///
    /// ## ⚠ Read it as "the default for assets that HAVE a choice"
    /// The field is per-VENUE, so where a venue's own metadata removes a mode from SOME of its
    /// instruments this row states the mode that rules for the rest — it structurally cannot state
    /// the exception, and reading it as "the mode that rules for every asset here" is wrong.
    /// Hyperliquid is the measured counterexample: its public `meta` publishes per-asset
    /// `onlyIsolated`, true on **9 of 232** assets (HPOS, RLB, UNIBOT, OX, FRIEND, SHIA, NFTI,
    /// PANDORA, CASHCAT — re-measured live 2026-08-05; 8 are also delisted, CASHCAT is not, so the
    /// exception is reachable). Cross does not exist on those, so what rules is
    /// [`MarginMode::Isolated`] while this row says [`MarginMode::Cross`] — and the venue agrees
    /// with the asset, not with this row: `crates/bridges/hyperliquid/src/recon_client.rs`'s
    /// `parse_positions` maps `leverage.type == "isolated"` → `MarginMode::Isolated`, so a
    /// reconcile pass over such a position reports Isolated.
    ///
    /// **That mismatch reaches nothing today**, and the reason is worth stating so a future reader
    /// need not re-derive it: `vike_exec::recon::diff` never compares margin mode (no `Divergence`
    /// variant is margin-shaped), so the difference cannot raise a divergence, an alert or a
    /// synthesized event; and outside this module's own tests — plus okx's adapter tie — nothing in
    /// the workspace reads this field at all. The live consumers of margin mode are per-POSITION
    /// (`vike_exec`'s gate margin fold and `margin_call`'s pool partition; `vike_core`'s
    /// liquidation-price badge) and they read `PositionEntry.margin_mode`, which the reconcile
    /// snapshot overwrites with venue truth. So this row misleads a READER, not code.
    ///
    /// The per-asset narrowing is DERIVED in one place rather than duplicated into a column here:
    /// `crates/bridges/hyperliquid/src/symbology.rs`'s `InstrumentRef::effective_margin_mode`
    /// combines this field with that crate's already-parsed `InstrumentRef::only_isolated`, pinned
    /// by its `effective_margin_mode_is_per_asset_not_per_venue`.
    pub default_margin_mode: MarginMode,
    /// Max orders per NATIVE batch call as the adapter wires it; `0` = no native batch
    /// (invariant, pinned: `max_batch > 0` ⇔ [`Self::supports_native_batch`]). `usize::MAX`
    /// means the adapter imposes no cap of its own (hyperliquid sends N wires in one action).
    ///
    /// FOLLOW-UP: this axis is declared and invariant-checked (see
    /// `expanded_axes_invariants_hold`) but nothing ENFORCES it — [`preflight_order`]'s
    /// `SubmitBatch` handling in `vike-core` never compares a batch's length against this cap, so
    /// an oversized batch is not preflight-refused here; it still relies on the adapter's own
    /// chunking (binance/aster) or a venue-side reject for the cap to bite.
    pub max_batch: usize,
    /// Live market-data verbs served (see [`LiveDataCaps`]).
    pub live_data: LiveDataCaps,
    /// `vike-backfill` can PRODUCE historical OHLCV bars for this venue from the VENUE's own API —
    /// whether by fetching klines directly (every crypto venue) or by resampling ticks it fetched
    /// (dukascopy `.bi5` → `resample_quotes_to_bars`).
    ///
    /// **The definition is "can produce", NOT "the venue serves a native bar endpoint"** — settled
    /// deliberately (this field and `vike_backfill::caps` used to give opposite answers for
    /// dukascopy). A caller asking this field is asking "can I get bars for this venue without a
    /// vendor or an archive"; how the bytes arrive is the collector's business, and a resample is
    /// not a lesser bar. Cross-pinned EXACTLY to `vike_backfill::caps::backfill_caps(Source::Venue,
    /// v).bar` by `venue_caps_cross_pin_the_backfill_table` in that crate — which is where the
    /// non-circular evidence lives (that table has a filesystem-existence gate and a source-scanning
    /// shape gate over the real collector modules).
    pub backfill_bars: bool,
    /// `vike-backfill` can backfill historical TICKS for this venue from the venue's own API — any
    /// of the three tick kinds (`quote`/`trade`/`book`). Dukascopy's keyless `.bi5` quote feed is
    /// the only venue-direct tick source on the roster today.
    ///
    /// Cross-pinned to `backfill_caps(Source::Venue, v).quote | .trade | .book` by the same test.
    pub backfill_ticks: bool,
}

impl VenueCaps {
    /// The conservative all-unsupported row — the fallback for an unknown venue string and the
    /// default a UI uses before a venue is chosen. Nothing is offered until a venue proves it.
    pub const UNSUPPORTED: VenueCaps = VenueCaps {
        supports_modify: false,
        supports_native_batch: false,
        supports_reduce_only: false,
        supports_combo: false,
        supported_tifs: &[],
        accepted_tifs: &[],
        supported_order_kinds: &[],
        trigger_types: &[],
        supports_post_only: false,
        margin_modes: &[],
        default_margin_mode: MarginMode::Cross,
        max_batch: 0,
        live_data: LiveDataCaps::NONE,
        backfill_bars: false,
        backfill_ticks: false,
    };

    /// The modify-gate decision: an in-place modify may be OFFERED (draggable resting order)
    /// and SENT (`Command::Modify`) only when the venue's adapter wires a native modify. The GUI
    /// calls this to refuse a marker drag (`vike-panels`' Trade window ladder) AND to guard the
    /// command send (`vike-app-core`'s `order_dispatch`) — the two halves the audit flagged.
    #[inline]
    pub const fn allows_modify(&self) -> bool {
        self.supports_modify
    }

    /// Whether this venue serves ANY live market-data verb (i.e. implements `DataClient`).
    #[inline]
    pub const fn has_live_data(&self) -> bool {
        let d = self.live_data;
        d.bars || d.quotes || d.trades || d.book || d.depth
    }
}

impl Default for VenueCaps {
    fn default() -> Self {
        VenueCaps::UNSUPPORTED
    }
}
