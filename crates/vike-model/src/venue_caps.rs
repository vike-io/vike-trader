//! `VenueCaps` — the STATIC per-venue capability matrix (audit br6).
//!
//! Today a venue's capabilities are only ever discovered at RUNTIME: [`crate::order::OrderRequest`]
//! modify/batch land on silent `ExecutionClient` trait defaults, live-data verbs return
//! `LiveDataError::Unsupported`, and the GUI sends `Command::Modify` unconditionally — so an
//! unsupported action becomes a post-hoc reject. This module makes each adapter's real capabilities
//! a DECLARED, compile-time constant a caller can consult BEFORE acting.
//!
//! ## Where this lives and why
//! The struct + the per-venue values live in `vike-model` (not `vike-bridge-core`) deliberately:
//! - `VenueCaps` is a pure data descriptor with no I/O — exactly the shape of everything else in
//!   this crate, and its `supported_tifs` field is over [`crate::order::TimeInForce`] which is
//!   already here.
//! - `vike-model` is the bottom layer every other crate already depends on, so the GUI shell
//!   (`vike-app`), the chart widget (`vike-chart`, which depends on `vike-model` only and renders
//!   the DOM), the venue bridges, and `vike-backfill` can ALL reach the same values with **no new
//!   dependency and no layering inversion**. A registry that instead lived in a bridge crate could
//!   not be reached by `vike-chart`; one that lived in `vike-bridge-core` could not be reached by
//!   `vike-chart` either (it depends on `vike-exec`, which `vike-chart` must not pull in).
//!
//! Each bridge crate RE-EXPORTS its own row as `pub const CAPS` (e.g. `vike_binance::CAPS`) so the
//! declaration is discoverable next to the adapter.
//!
//! ## ⚠ What a bridge's `caps_test` actually proves — read before trusting one
//!
//! This paragraph used to claim the bridges "are the source of the TRUTH those values are tested
//! against". **They are not, and never were.** All 14 `caps_test` modules open with
//! `assert_eq!(super::CAPS, vike_model::caps_for("<venue>"))`, where `CAPS` is DEFINED as that same
//! const — the equality holds by construction, and the detour through the non-const `caps_for`
//! exists only to dodge `clippy::assertions_on_constants` (`crates/bridges/binance/src/lib.rs` says
//! so in as many words). Every assertion after it compares fields of that same constant against
//! literals, with prose citing the adapter.
//!
//! So a `caps_test` is a SECOND, co-located restatement of this table plus one human's reading of
//! the adapter. That is worth something — it pins a row against a careless edit and records the
//! reasoning next to the code — but it does not execute the adapter, so it cannot catch a row that
//! was wrong when written, nor one that went stale when the adapter changed underneath it. Both
//! happened: IBKR's `live_data` and `backfill_bars` were written 2026-07-14 and contradicted the
//! NEXT DAY by #306 (the real five-verb `DataClient`) and #311 (the real bar collector), and stayed
//! wrong for three weeks with every `caps_test` green.
//!
//! The checks that DO tie a row to something outside this table — the pattern to extend:
//! - `crates/bridges/deribit/src/exec.rs`'s `modify_is_default_noop_for_deribit` — CALLS the real
//!   `rest.modify_order`, asserts it returns `[]` without touching the socket, AND asserts
//!   `!caps_for("deribit").supports_modify`, in one test.
//! - `crates/bridges/hyperliquid/src/exec.rs`'s `stop_limit_kind_is_declared_and_built` — puts a
//!   real `"stop_limit"` [`OrderRequest`] through `build_order_wire` and asserts BOTH the wire shape
//!   it produces and that this row declares the kind.
//! - `crates/vike-bridge-core/src/pump_spec.rs`'s `venue_caps_cross_pin_the_pump_spec` —
//!   [`VenueCaps::has_live_data`] against the market-pump table (a second, independently maintained
//!   declaration that cites the same adapter files).
//! - `crates/vike-backfill/src/caps.rs`'s `venue_caps_cross_pin_the_backfill_table` — the two
//!   backfill axes against `backfill_caps`, which carries a filesystem-existence gate and a
//!   source-scanning shape gate, so this row inherits both transitively.
//! - `crates/vike-bridge-core/src/tif.rs`'s `venue_caps_cross_pin_the_tif_table` (the TIF axes) and
//!   this module's own `margin_modes_cross_pin_venue_margin_support` (the margin axis).
//!
//! ## Honesty rule
//! Every field reflects what the ADAPTER wires TODAY, not what the exchange could theoretically do.
//! Bybit-the-exchange supports PostOnly, but PostOnly is not expressible via `OrderRequest`, so
//! `BYBIT.supported_tifs` lists only the three TIFs the adapter actually maps (GTC/IOC/FOK). Where
//! a capability could not be verified from Rust (e.g. whether the Dukascopy Java sidecar honors
//! reduce-only), it is declared conservatively `false`.

use crate::order::{OrderRequest, TimeInForce};
use crate::venue_margin_support::MarginMode;

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
    /// `ExecutionClient` (vs the silent no-op trait default). Drives the DOM drag-to-reprice
    /// control + the `Command::Modify` send gate.
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
    /// `vike_bridge_core::tif::venue_tif` — see the `BINANCE` row doc. Cross-pinned against the
    /// `venue_tif` table by `vike-bridge-core`'s `venue_caps_cross_pin_the_tif_table` test so the
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
    /// cross-pinned against `venue_tif` (same bridge-core test).
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
    /// [`crate::venue_margin_support::VenueMarginSupport`], whose subject is what the venue
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
    /// whether by fetching klines directly (every crypto venue, ibkr) or by resampling ticks it
    /// fetched (dukascopy `.bi5` → `resample_quotes_to_bars`).
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

    /// The DOM modify-gate decision: an in-place modify may be OFFERED (draggable resting order)
    /// and SENT (`Command::Modify`) only when the venue's adapter wires a native modify. The GUI
    /// calls this to grey the drag control (`vike-chart`) AND to guard the command send
    /// (`vike-app`) — the two halves the audit flagged.
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

// ---------------------------------------------------------------------------------------------
// The per-venue rows. Each cites the adapter code the value was read from (audit br6 inspection).
// ---------------------------------------------------------------------------------------------

/// Binance USDS-M futures (`vike_binance::perp::BinancePerpRest` via `LiveRestClient`). Native
/// modify (`PUT /fapi/v1/order`) + native batch (`/fapi/v1/batchOrders`) + `reduceOnly`, all wired
/// through `VenueRest`. TIF FLIPPED (tif step-2, demo-smoke-proven): the family builder maps
/// GTC/IOC/FOK 1:1 on LIMIT; Day is loud-denied at submit (`deny_unsupported_tif`); GTD is
/// LANE-SPLIT — the PERP lane wires native fapi GTD (`timeInForce=GTD` + mandatory
/// `goodTillDate` from `gtd_expiry`; the `venue_tif` `"binance-perp"` lane sub-key, gated by
/// `vike_binance::perp::deny_invalid_gtd`) while the spot `/api/v3` has no GTD at all and keeps
/// the loud deny. This registry is keyed by the ONE venue id `"binance"` for BOTH lanes, so
/// `supported_tifs` stays the CONSERVATIVE lane intersection (no `Gtd`): a caller consulting it
/// is never led to offer GTD on the lane that must refuse it. Per-lane TIF truth lives in
/// `vike_bridge_core::tif::venue_tif` (`"binance"` = spot, `"binance-perp"` = perp). Live
/// feed serves bars + `@aggTrade` trades + `@depth20` depth (not quotes/book). Kline backfill in
/// `vike-backfill`.
/// Expanded axes: `accepted_tifs` adds `Gtd` (the lane UNION — the perp lane wires native fapi
/// GTD, so the core preflight must not refuse it; the spot lane's `deny_unsupported_tif` still
/// loud-denies it there). Order kinds are the lane union too: `"stop"` is the perp lane's native
/// STOP_MARKET (`build_perp_order_params`, `stopPrice` from `trigger_price`); spot has no stop
/// type (its builder passes `order_type` through verbatim and the venue rejects). `"take_profit"`
/// is NOT wired (the perp `_ => MARKET` fallthrough would fire it immediately) → refused.
/// `margin_modes: &[Cross]` — the order builder never reads `margin_mode` (only the account
/// default, cross, is honored). `max_batch: 5` = the fapi batchOrders chunk cap (`BATCH_MAX`).
pub const BINANCE: VenueCaps = VenueCaps {
    supports_modify: true,
    supports_native_batch: true,
    supports_reduce_only: true,
    supports_combo: false,
    supported_tifs: &[TimeInForce::Gtc, TimeInForce::Ioc, TimeInForce::Fok],
    accepted_tifs: &[TimeInForce::Gtc, TimeInForce::Ioc, TimeInForce::Fok, TimeInForce::Gtd],
    supported_order_kinds: &["market", "limit", "stop"],
    trigger_types: &[TriggerType::StopLoss],
    supports_post_only: false,
    margin_modes: &[MarginMode::Cross],
    default_margin_mode: MarginMode::Cross,
    max_batch: 5,
    live_data: LiveDataCaps { bars: true, quotes: false, trades: true, book: false, depth: true },
    backfill_bars: true,
    backfill_ticks: false,
};

/// Bybit V5 linear perp (`vike_bybit::exec::BybitExecutionClient` over `ExecActor`). Native amend
/// (`/v5/order/amend`, `ExecCommand::Modify`) + `reduceOnly`; batch is NOT wired at the client
/// seam (falls through to the fan-out default, though `BybitPerpRest` has the methods). TIF
/// FLIPPED (tif step-2, demo-smoke-proven): GTC/IOC/FOK map 1:1 on Limit; GTD/Day are
/// loud-denied at submit (`deny_unsupported_tif`). Live feed serves bars + `publicTrade` trades
/// (live-verified 2026-07-20, `bybit_trades_feed_smoke`) + `orderbook.200` depth. Kline backfill
/// in `vike-backfill`.
/// Expanded axes: `"stop"` is a native conditional (`build_order_params` wires
/// `triggerPrice`/`triggerDirection`/`triggerBy:"LastPrice"`, Market-on-trigger);
/// `"take_profit"` falls into the `_ => Market` coercion → refused. TIF admit set == the
/// honored trio (GTD/Day are already loud-denied venue-side; the preflight only moves that
/// earlier). `margin_mode` is never read (UTA account-level mode) → `&[Cross]`. The V5 batch
/// endpoints exist on `BybitPerpRest` but are NOT wired at the `ExecutionClient` seam →
/// `max_batch: 0`, consistent with `supports_native_batch: false`.
pub const BYBIT: VenueCaps = VenueCaps {
    supports_modify: true,
    supports_native_batch: false,
    supports_reduce_only: true,
    supports_combo: false,
    supported_tifs: &[TimeInForce::Gtc, TimeInForce::Ioc, TimeInForce::Fok],
    accepted_tifs: &[TimeInForce::Gtc, TimeInForce::Ioc, TimeInForce::Fok],
    supported_order_kinds: &["market", "limit", "stop"],
    trigger_types: &[TriggerType::StopLoss],
    supports_post_only: false,
    margin_modes: &[MarginMode::Cross],
    default_margin_mode: MarginMode::Cross,
    max_batch: 0,
    live_data: LiveDataCaps { bars: true, quotes: false, trades: true, book: false, depth: true },
    backfill_bars: true,
    backfill_ticks: false,
};

/// OKX SWAP perp (`vike_okx::exec::OkxExecutionClient` over `ExecActor`). Native amend
/// (`ExecCommand::Modify` → `OkxPerpRest::modify_order`) + `reduceOnly`; batch not wired at the
/// client seam (fan-out default). TIF FLIPPED (tif step-2, demo-smoke-proven): OKX carries TIF on
/// `ordType` — GTC stays the un-emitted venue default (`ordType:"limit"`, byte-identical);
/// IOC/FOK map to the `ioc`/`fok` ordTypes; GTD/Day are loud-denied at submit
/// (`deny_unsupported_tif`). Live feed serves bars + `trades` prints (live-verified 2026-07-20,
/// `okx_trades_feed_smoke`) + `books` depth. Kline backfill in `vike-backfill`.
/// Expanded axes: `"stop"` routes to the native ALGO conditional (`submit_stop_algo` —
/// `/trade/order-algo`, `slTriggerPx` from `trigger_price`, fires as market); `"take_profit"`
/// falls into the `ordType:"market"` coercion → refused. THE margin venue: `swap_td_mode`
/// honors a per-order `margin_mode` — `Cross` (and unset) → `tdMode:"cross"`, `Isolated` →
/// `"isolated"`, `Cash` denied venue-side (spot-only mode; the preflight moves that deny
/// earlier) → `margin_modes: &[Cross, Isolated]`. Batch endpoints exist on `OkxPerpRest` but
/// are not wired at the client seam → `max_batch: 0`.
pub const OKX: VenueCaps = VenueCaps {
    supports_modify: true,
    supports_native_batch: false,
    supports_reduce_only: true,
    supports_combo: false,
    supported_tifs: &[TimeInForce::Gtc, TimeInForce::Ioc, TimeInForce::Fok],
    accepted_tifs: &[TimeInForce::Gtc, TimeInForce::Ioc, TimeInForce::Fok],
    supported_order_kinds: &["market", "limit", "stop"],
    trigger_types: &[TriggerType::StopLoss],
    supports_post_only: false,
    margin_modes: &[MarginMode::Cross, MarginMode::Isolated],
    default_margin_mode: MarginMode::Cross,
    max_batch: 0,
    live_data: LiveDataCaps { bars: true, quotes: false, trades: true, book: false, depth: true },
    backfill_bars: true,
    backfill_ticks: false,
};

/// Aster DEX (`vike_aster::AsterExecutionClient`). USDⓈ-M perp + spot; a Binance-fork API.
/// `AsterPerpRest` overrides `modify_order` (`PUT /fapi/v3/order`) + native batch
/// (`POST /fapi/v3/batchOrders`, ≤5) and builds `reduceOnly`. TIF defaults GTC. Live `Feeds`
/// serves bars/trades/depth; L1 quotes + lossless L2 book run through the separate HFT tick track.
/// Expanded axes: shares binance's family builders, so the kinds match binance (perp
/// STOP_MARKET stop; `take_profit` → the MARKET fallthrough → refused). TIF is still the
/// unflipped `Ignored{GTC}` row — only a GTC request matches what actually rests, so
/// `accepted_tifs: &[Gtc]`: a non-GTC limit is preflight-refused instead of silently resting
/// GTC (the loud-deny flip this axis exists for). `margin_mode` never read → `&[Cross]`;
/// `max_batch: 5` = the `/fapi/v3/batchOrders` chunk cap.
pub const ASTER: VenueCaps = VenueCaps {
    supports_modify: true,
    supports_native_batch: true,
    supports_reduce_only: true,
    supports_combo: false,
    supported_tifs: &[TimeInForce::Gtc],
    accepted_tifs: &[TimeInForce::Gtc],
    supported_order_kinds: &["market", "limit", "stop"],
    trigger_types: &[TriggerType::StopLoss],
    supports_post_only: false,
    margin_modes: &[MarginMode::Cross],
    default_margin_mode: MarginMode::Cross,
    max_batch: 5,
    live_data: LiveDataCaps { bars: true, quotes: false, trades: true, book: false, depth: true },
    backfill_bars: true,
    backfill_ticks: false,
};

/// Deribit options (`vike_deribit::client::DeribitRest` via `LiveRestClient`). Implements
/// `VenueRest` but overrides NEITHER `modify_order` NOR the batch methods → no native modify, no
/// native batch. Builds `reduce_only` on the JSON-RPC order. TIF FLIPPED (tif step-2,
/// demo-smoke-proven): Gtc stays the un-emitted venue default (byte-identical); Ioc/Fok/Day map
/// to `immediate_or_cancel`/`fill_or_kill`/`good_til_day` on the limit path; Gtd (no
/// good-till-DATE on Deribit) is loud-denied at submit. **The ONE combo-capable venue**:
/// `submit_order` resolves a non-empty `combo_legs` through the combo book
/// (`private/create_combo` + the orientation/scale solve in `vike_deribit::combo`, then buy/sell
/// on the resolved combo id) — proven offline by the bridge's `combo_submit` tests and live by
/// its gated `deribit_combo_smoke` order lifecycle. Live `DataClient`: `vike_deribit::
/// market_feed::Feeds` (split-plane I9) serves bars (`chart.trades.*` + REST seed), quotes
/// (`quote.*`), trades (`trades.*`) and the lossless folded book (`book.*`, the `change_id`
/// DeltaSync chain); the conflating DOM `depth` lane is NOT wired — `subscribe_depth` refuses
/// through this row. `vike-backfill` DOES backfill bars — `vike_backfill::deribit`
/// pages the keyless public `get_tradingview_chart_data` through `klines::backfill_klines` →
/// `append_bars`, shipped as the `deribit_backfill` bin (this row said `false` until the
/// cross-pin below caught it; the collector landed in #1030).
/// Expanded axes: `build_order_params` distinguishes ONLY `"limit"` — every other kind is
/// wire-`"market"`, so a `"stop"`/`"take_profit"` request would fire IMMEDIATELY as a market
/// order instead of resting as protection (the most dangerous silent coercion in the matrix) →
/// trigger kinds refused (`trigger_types: &[]`). `post_only` is forced FALSE on the wire.
/// Margin is the fixed subaccount cross model → `&[Cross]`.
pub const DERIBIT: VenueCaps = VenueCaps {
    supports_modify: false,
    supports_native_batch: false,
    supports_reduce_only: true,
    supports_combo: true,
    supported_tifs: &[TimeInForce::Gtc, TimeInForce::Ioc, TimeInForce::Fok, TimeInForce::Day],
    accepted_tifs: &[TimeInForce::Gtc, TimeInForce::Ioc, TimeInForce::Fok, TimeInForce::Day],
    supported_order_kinds: &["market", "limit"],
    trigger_types: &[],
    supports_post_only: false,
    margin_modes: &[MarginMode::Cross],
    default_margin_mode: MarginMode::Cross,
    max_batch: 0,
    live_data: LiveDataCaps { bars: true, quotes: true, trades: true, book: true, depth: false },
    backfill_bars: true,
    backfill_ticks: false,
};

/// OANDA v20 (`vike_oanda::exec::OandaExecutionClient` over `ExecActor`). Wires submit/cancel only
/// — modify falls through to the trait no-op; no batch. No reduce-only on the wire. The RICH TIF
/// venue: `oanda_tif` maps the full GTC/IOC/FOK/GTD/DAY set. No `vike-backfill` backfill.
///
/// `live_data { bars: true, quotes: true }` — `vike_oanda::market_feed::Feeds` (the SPLIT-PLANE
/// feed) serves `subscribe_quotes` from the venue's chunked-HTTP pricing stream and
/// `subscribe_bars` by polling the candles REST endpoint (OANDA publishes candles no other way).
/// `subscribe_trades` (no trade tape published), `subscribe_book`/`subscribe_depth` (the pricing
/// ladder is an unsequenced top-of-book snapshot, not a delta-synced L2 book) all return
/// `Unsupported`, so those stay `false`.
///
/// Expanded axes: `build_order_body` wires `"limit"` → LIMIT and `"stop"` → native STOP
/// (`trigger_price` as the order level); everything else — `"take_profit"` included — is the
/// `_ => MARKET` coercion → refused. `margin_mode` never read (shared FX account margin) →
/// `&[Cross]`.
pub const OANDA: VenueCaps = VenueCaps {
    supports_modify: false,
    supports_native_batch: false,
    supports_reduce_only: false,
    supports_combo: false,
    supported_tifs: &[
        TimeInForce::Gtc,
        TimeInForce::Ioc,
        TimeInForce::Fok,
        TimeInForce::Gtd,
        TimeInForce::Day,
    ],
    accepted_tifs: &[
        TimeInForce::Gtc,
        TimeInForce::Ioc,
        TimeInForce::Fok,
        TimeInForce::Gtd,
        TimeInForce::Day,
    ],
    supported_order_kinds: &["market", "limit", "stop"],
    trigger_types: &[TriggerType::StopLoss],
    supports_post_only: false,
    margin_modes: &[MarginMode::Cross],
    default_margin_mode: MarginMode::Cross,
    max_batch: 0,
    live_data: LiveDataCaps { bars: true, quotes: true, trades: false, book: false, depth: false },
    backfill_bars: false,
    backfill_ticks: false,
};

/// Alpaca Broker API (US equities + crypto). Single pinned account. No native order modify
/// (submit/cancel only in Phase 1); TIFs the /v1/trading orders API accepts. Phase 2 (Task 11)
/// landed the market-data WS (`data.rs`, `AlpacaDataClient`) serving bars/quotes/trades — no
/// live L2 book/depth feed, so `book`/`depth` stay `false`.
/// Expanded axes: `build_order_body` maps `"limit"`/`"stop"`/`"stop_limit"` natively
/// (`stop_price` from `trigger_price`) — the one adapter wiring a `"stop_limit"` kind;
/// `"take_profit"` falls to the `_ => market` coercion → refused. `accepted_tifs` adds `Gtd`
/// (coerced to `gtc` today — live behavior, encoded as-accepted). `margin_mode` never read
/// (shared Reg-T account margin) → `&[Cross]`.
pub const ALPACA: VenueCaps = VenueCaps {
    supports_modify: false,
    supports_native_batch: false,
    supports_reduce_only: false,
    supports_combo: false,
    supported_tifs: &[TimeInForce::Day, TimeInForce::Gtc, TimeInForce::Ioc, TimeInForce::Fok],
    accepted_tifs: &[
        TimeInForce::Day,
        TimeInForce::Gtc,
        TimeInForce::Ioc,
        TimeInForce::Fok,
        TimeInForce::Gtd,
    ],
    supported_order_kinds: &["market", "limit", "stop", "stop_limit"],
    trigger_types: &[TriggerType::StopLoss],
    supports_post_only: false,
    margin_modes: &[MarginMode::Cross],
    default_margin_mode: MarginMode::Cross,
    max_batch: 0,
    live_data: LiveDataCaps { bars: true, quotes: true, trades: true, book: false, depth: false },
    backfill_bars: false,
    backfill_ticks: false,
};

/// IG (`vike_ig::exec::IgExecutionClient` over `ExecActor`). Wires submit/cancel only (positions +
/// working orders); modify is the trait no-op, no batch, no wire reduce-only. TIF not mapped →
/// effectively GTC. No `vike-backfill` backfill.
/// LIVE DATA: `vike_ig::market_feed::Feeds` (Lightstreamer TLCP) serves `bars` (`CHART:{epic}:
/// {scale}`, MERGE) and `quotes` (`MARKET:{epic}`, MERGE) — and NOTHING else, structurally: IG's
/// streaming API publishes the dealer's L1 BID/OFFER with no depth ladder behind it (`book` and
/// `depth` both false), and a dealer venue has no public trade tape (`trades` false).
/// Expanded axes: `build_request` wires `"limit"`/`"stop"` as working orders (`level` from
/// `price`/`trigger_price`) and everything else as a MARKET position open → `"take_profit"`
/// refused. TIF is never read (working orders hardcode `GOOD_TILL_CANCELLED`), so
/// `accepted_tifs: &[Gtc]` — a non-GTC limit is preflight-refused instead of silently resting
/// GTC (the loud-deny flip). `margin_mode` never read → `&[Cross]`.
pub const IG: VenueCaps = VenueCaps {
    supports_modify: false,
    supports_native_batch: false,
    supports_reduce_only: false,
    supports_combo: false,
    supported_tifs: &[TimeInForce::Gtc],
    accepted_tifs: &[TimeInForce::Gtc],
    supported_order_kinds: &["market", "limit", "stop"],
    trigger_types: &[TriggerType::StopLoss],
    supports_post_only: false,
    margin_modes: &[MarginMode::Cross],
    default_margin_mode: MarginMode::Cross,
    max_batch: 0,
    live_data: LiveDataCaps { bars: true, quotes: true, trades: false, book: false, depth: false },
    backfill_bars: false,
    backfill_ticks: false,
};

/// FXCM ForexConnect (`vike_fxcm::exec::FxcmExecutionClient` over `ExecActor`; behind the `fxcm`
/// feature). The exec loop EXPLICITLY no-ops `ExecCommand::Modify` ("no native amend on this
/// venue"); no batch, no wire reduce-only. Places a resting LIMIT entry with no TIF → GTC. No live
/// `DataClient`, no `vike-backfill` backfill.
/// Expanded axes: the exec loop reads `order_type` and splits on it — `"market"` places a TRUE
/// MARKET order (`fcshim.cpp`'s `fc_place_market`, ForexConnect `O2G2::Orders::TrueMarketOpen`),
/// anything else places the fixed resting LIMIT entry (`place_limit_entry`, `RESTING_PIPS`). Those
/// are the two kinds the adapter actually wires, hence exactly the two declared here. The earlier
/// `"limit"`-only row recorded a vike SHIM gap, not a venue one (ForexConnect always supported
/// market orders); the shim now has the placement, so `"market"` is declared again.
/// LIVE-VERIFIED (market-fill path only): the `fcshim.cpp:122` real-SDK build break
/// (`getInstrument` on `IO2GTradeRow`, which lives only on `IO2GTradeTableRow`) is fixed — the
/// base trade row's instrument is resolved via its offer id — and a demo market-order round-trip
/// (BUY → fill @ 1.14268 → net-flat SELL) is green on the FXCM demo (`vike-fxcm`
/// `tests/fxcm_live_smoke.rs::fxcm_market_round_trip`). The phantom-cancel fix's other two
/// `Orders(Delete)` branches — async reject (`getStatus()=='R'`) and venue-initiated cancel
/// (`=='C'`) — follow the SDK's `OrderMonitor::onOrderDeleted` convention (not in the vendored
/// headers) and are STILL unexercised live. ⚠ **Attempted again on the CI box (2026-08-23, market open,
/// real demo orders) and this records WHICH CONDITION could not be produced, which is more useful
/// than "pending":**
/// * `'C'` — a resting limit entry was placed and cancelled (`fxcm_login_and_limit_cancel`, green,
///   venue order 237472952). Under `RUST_LOG=trace` **no** `map_drained_event` drop-warning
///   appeared, so nothing shows the shim emitted a `canceled` envelope at all within the run. Our
///   OWN cancel is the wrong instrument for this branch twice over: the synchronous
///   `fc_delete_order` return already produced the terminal, `crates/bridges/fxcm/src/exec.rs`'s
///   `drain_events` has already dropped the routing row by then (`closes_routing`), and the smoke
///   detaches immediately after. The branch exists for a cancel the VENUE initiates — order expiry,
///   a margin call, instrument suspension — and none of those is inducible on a demo account
///   holding $49,999 against one lot of the most liquid pair in the world.
/// * `'R'` — unreachable by placement: a rejected placement returns non-zero from `fc_place`
///   SYNCHRONOUSLY and becomes an `OrderRejected` through `map_placement`. Reaching the async arm
///   needs an order the venue ACCEPTS and then rejects later, which is the same class of venue-side
///   condition as above and equally not inducible here.
///
/// A resting LIMIT entry still cannot honor a REQUESTED limit price — the shim computes the rate
/// as `RESTING_PIPS` from the live quote — so `"limit"` remains an approximation. ⚠ It is now a
/// DECLARED approximation rather than a silent one: `vike_fxcm::event_mapper`'s `preflight_request`
/// REFUSES a non-market request that names a `price`, with a terminal `OrderRejected`. An unpriced
/// limit is unchanged and still rests at that distance, which is exactly what this row declares —
/// so the refusal costs nothing that ever worked, and closes the case where an operator was told
/// nothing and got a fill at a price they had not chosen.
/// TIF/`margin_mode` never read → `&[Gtc]`/`&[Cross]`.
///
/// ⚠ **WHAT AN OPERATOR ACCEPTS BY MOUNTING THIS VENUE.** Three properties this table has no field
/// for, each true today, each stated here because a caps row is where this workspace records what a
/// venue cannot do:
///
/// 1. **`qty` is BASE UNITS, and an inexact size is REFUSED.** ⚠ It was a LOT COUNT until the
///    conversion landed, which is the one that mattered: `fcshim.cpp` sets
///    `Amount = base_unit_size * lots`, so a caller sending the units it sends at every other venue
///    placed that many LOTS — `qty: 10000` on EUR/USD was ten thousand lots, a thousand times the
///    intended size, on a real account, with no event saying so. The shim now exports
///    `fc_base_unit_size` (`IO2GTradingSettingsProvider::getBaseUnitSize` — the SAME lookup the
///    placement multiplies by), `crates/bridges/fxcm/src/exec.rs` reads it once per instrument per
///    session, and `vike_fxcm::event_mapper`'s `lots_for` divides. What this venue still cannot do
///    is place a FRACTION of a lot, so a `qty` that is not an exact multiple of the base unit is a
///    terminal reject naming the two placeable sizes either side — not a rounded substitute, which
///    is what the pre-#1470 `(qty.round() as i32).max(1)` produced for every one of them.
/// 2. **A fill this process did not place is DROPPED — so mount this venue with `VIKE_RECONCILE=1`
///    or accept losing fills across a restart.** The async fill lane routes on an in-memory map
///    built at accept time, so after a restart every trade the venue re-surfaces names an order the
///    new process never saw. It is `warn!`-visible rather than silent, and the recovery is the
///    reconcile client's `fetch_fill_reports`, which reads the same Trades table keyed by the same
///    `trade_id`. Both `crates/vike-mount/src/lib.rs`'s `make_engine_with_legs` and — since fxcm
///    joined `vike_run::WIRED_MARKETS` — `vike_run::build_node` wire it, LAZILY: it is built only
///    when reconciliation is enabled, so the hole is open exactly when `VIKE_RECONCILE` is off.
///    ⚠ It is NOT closed by routing on the venue order id instead: the Trades table is the only
///    place the linkage survives a process, and an in-process map cannot be made to.
/// 3. **A failed login is indistinguishable from a stub build and from no credentials.** The exec
///    thread returns at login and later submits become no-ops. The mount refuses the case it CAN
///    detect (an unlinked SDK, via `vike_fxcm::sdk_linked`), but a live binary with a bad password
///    mounts "live" and trades nothing.
pub const FXCM: VenueCaps = VenueCaps {
    supports_modify: false,
    supports_native_batch: false,
    supports_reduce_only: false,
    supports_combo: false,
    supported_tifs: &[TimeInForce::Gtc],
    accepted_tifs: &[TimeInForce::Gtc],
    supported_order_kinds: &["market", "limit"],
    trigger_types: &[],
    supports_post_only: false,
    margin_modes: &[MarginMode::Cross],
    default_margin_mode: MarginMode::Cross,
    max_batch: 0,
    live_data: LiveDataCaps::NONE,
    backfill_bars: false,
    backfill_ticks: false,
};

/// Dukascopy (`vike_dukascopy::exec::DukascopyExecutionClient`; JForex Java sidecar). Wires
/// submit/cancel only over JSON-lines stdio; modify is the trait no-op. reduce-only is declared
/// CONSERVATIVELY `false` — the request is forwarded to the sidecar but whether `NettingPlan.java`
/// honors a per-order reduce-only flag is not verifiable from Rust. No live `DataClient`, but
/// `vike-backfill` backfills keyless `.bi5` TICKS (`append_quotes`) — and BARS from them, via
/// `hist.resample_quotes_to_bars` in `vike_backfill::dukascopy` (dukascopy_backfill bin), which is
/// why `backfill_bars` is `true` here. It was `false` for as long as this field meant "the venue
/// serves a NATIVE bar endpoint"; that reading contradicted `vike_backfill::caps`, which has always
/// declared `{bar: true, quote: true}` for this venue. The field doc above settles the split in
/// favour of "can produce", so the two tables now agree and the cross-pin closes with no exception.
/// Expanded axes: the Rust side forwards the `OrderRequest` verbatim; the Java sidecar
/// (`StrategyBridge.java`) accepts exactly `"market"`/`"limit"` and terminally rejects every
/// other `order_type` ("unsupported order_type") — the preflight only moves that refusal to
/// the core edge. TIF/`margin_mode` are forwarded but never honored → `&[Gtc]`/`&[Cross]`.
pub const DUKASCOPY: VenueCaps = VenueCaps {
    supports_modify: false,
    supports_native_batch: false,
    supports_reduce_only: false,
    supports_combo: false,
    supported_tifs: &[TimeInForce::Gtc],
    accepted_tifs: &[TimeInForce::Gtc],
    supported_order_kinds: &["market", "limit"],
    trigger_types: &[],
    supports_post_only: false,
    margin_modes: &[MarginMode::Cross],
    default_margin_mode: MarginMode::Cross,
    max_batch: 0,
    live_data: LiveDataCaps::NONE,
    backfill_bars: true,
    backfill_ticks: true,
};

/// Polymarket CLOB (`vike_polymarket::client::PolymarketExecutionClient` over `ExecActor`; behind
/// the `polymarket` feature). Wires submit/cancel only; no modify, no reduce-only (prediction
/// market). `supports_native_batch: false` stays FALSE and the row is unchanged even though this
/// adapter now overrides `cancel_batch`: the flag means a batch-of-N-ids endpoint whose capacity
/// `max_batch` states, and what the CLOB offers is an ACCOUNT-WIDE `DELETE /cancel-all` that takes
/// no id list at all — a mass cancel, not a batch, with no N to declare. `vike_polymarket::exec`'s
/// `plan_cancels` is where the choice to use it lives, and there is no submit-side batch here.
/// `order_type_of` wires GTC + FOK LIVE-PROVEN (IOC coerces to FOK; GTD is
/// built but unproven) → declared `&[Gtc, Fok]`. Live tick feed serves quotes + trades + book (NOT bars — bars
/// via resample — and NOT the DOM `depth` snapshot lane). No `vike-backfill` backfill.
/// Expanded axes: the CLOB has ONE submit path — `order_type` is never read and every order is
/// a signed limit at `price` (unset → 0.0). `"market"` is therefore encoded AS-ACCEPTED (it
/// emulates: a sell at 0.0 is marketable — the shape `Flatten`/`MarketExit` rely on) — modeling
/// a real marketable order is the noted follow-up; trigger kinds would rest as nonsense limits
/// → refused. `accepted_tifs` is all five: Ioc→FOK and Day→GTC are the live coercions (a
/// separate flip), Gtd is emitted with a real wire `expiration` derived from `gtd_expiry`
/// (`client.rs::expiration_secs_of`, which refuses a missing/too-near expiry locally rather than
/// shipping the GTC `"0"`) — it stays OUT of `supported_tifs` because that wire path has never
/// been exercised against the live CLOB, and this venue trades real money on mainnet.
/// Fully collateralized → `margin_modes: &[Cash]`.
pub const POLYMARKET: VenueCaps = VenueCaps {
    supports_modify: false,
    supports_native_batch: false,
    supports_reduce_only: false,
    supports_combo: false,
    supported_tifs: &[TimeInForce::Gtc, TimeInForce::Fok],
    accepted_tifs: &[
        TimeInForce::Gtc,
        TimeInForce::Ioc,
        TimeInForce::Fok,
        TimeInForce::Gtd,
        TimeInForce::Day,
    ],
    supported_order_kinds: &["market", "limit"],
    trigger_types: &[],
    supports_post_only: false,
    margin_modes: &[MarginMode::Cash],
    default_margin_mode: MarginMode::Cash,
    max_batch: 0,
    live_data: LiveDataCaps { bars: false, quotes: true, trades: true, book: true, depth: false },
    backfill_bars: false,
    backfill_ticks: false,
};

/// Interactive Brokers (`vike_ibkr::IbkrExecutionClient` over `ExecActor`; Phase 1 socket
/// transport). Wires submit/cancel only — modify is the trait no-op (native amend is a later
/// phase); no batch (OCA / order-lists are a later phase); no wire reduce-only (multi-asset, not
/// wired in Phase 1). TIF maps the full GTC/DAY/IOC/FOK/GTD set.
///
/// **`live_data` and `backfill_bars` were WRONG here for three weeks** — this row was authored
/// 2026-07-14 saying "no live `DataClient` yet (Phase 3), no `vike-backfill` backfill yet", and
/// BOTH statements were falsified the next day and never revisited:
/// - `vike_ibkr::market_feed::IbkrFeeds` (#306, 2026-07-15) implements ALL FIVE `DataClient` verbs
///   for real — `subscribe_bars` (`bars_pump`: a `historical_data` seed then `realtime_bars`
///   folded to the requested interval), `subscribe_quotes` (`quotes_pump` → `sink.quote`),
///   `subscribe_trades` (`trades_pump` → `sink.trade`), and `subscribe_book`/`subscribe_depth`
///   (both `depth_pump`, which maintains the position-indexed `reqMktDepth` ladder and emits it as
///   BOTH `sink.book` and `sink.l2_snapshot`). Not one returns `Unsupported`, and
///   `crates/vike-run/src/bin/ibkr_mount.rs` mounts it in production. Behind `ibkr-socket`, exactly
///   as polymarket's row sits behind its own feature — a feature gate is not a capability denial.
///   `vike_bridge_core::pump_spec` has declared `"ibkr" => OwnPump` citing that same file all
///   along; `venue_caps_cross_pin_the_pump_spec` now makes the two tables unable to disagree.
/// - `vike_backfill::ibkr` (#311, 2026-07-15) pages `HistoricalFetcher` and `src/bin/ibkr_backfill.rs`
///   does the `append_bars`, live-verified on paper with 251 daily AAPL bars.
///
/// The cost was not cosmetic: `vike_data::require_live_verb` derives `LiveDataError::Unsupported`
/// from this row, and `vike_app_core::feed_lifecycle` treats that error as PROVABLY PERMANENT and
/// never retries. (No IBKR verb routes through `require_live_verb` today, so correcting the row is
/// runtime-inert here — it removes a latent trap rather than changing a live path.)
/// Expanded axes: `map_order_request` wires `"limit"` → LMT and `"stop"` → STP
/// (`trigger_price` as `aux_price`); everything else — `"take_profit"` included — is the
/// `_ => MKT` coercion → refused. `accepted_tifs` == the mapped five (`Gtd` is a stub — the
/// good-till date is never wired, TWS rejects it server-side, which is why it can stay
/// accepted). `margin_mode` never read (account-level Reg-T margin) → `&[Cross]`.
pub const IBKR: VenueCaps = VenueCaps {
    supports_modify: false,
    supports_native_batch: false,
    supports_reduce_only: false,
    supports_combo: false,
    supported_tifs: &[
        TimeInForce::Gtc,
        TimeInForce::Day,
        TimeInForce::Ioc,
        TimeInForce::Fok,
        TimeInForce::Gtd,
    ],
    accepted_tifs: &[
        TimeInForce::Gtc,
        TimeInForce::Day,
        TimeInForce::Ioc,
        TimeInForce::Fok,
        TimeInForce::Gtd,
    ],
    supported_order_kinds: &["market", "limit", "stop"],
    trigger_types: &[TriggerType::StopLoss],
    supports_post_only: false,
    margin_modes: &[MarginMode::Cross],
    default_margin_mode: MarginMode::Cross,
    max_batch: 0,
    live_data: LiveDataCaps { bars: true, quotes: true, trades: true, book: true, depth: true },
    backfill_bars: true,
    backfill_ticks: false,
};

/// IBKR via the Client Portal Web API (cpapi) backend — identical to [`IBKR`] except it has a
/// native order-amend endpoint, so `supports_modify` is true. (The socket backend has no native
/// amend.) The bridge selects this row per-backend via `vike_ibkr::caps_for_backend`.
pub const IBKR_CPAPI: VenueCaps = VenueCaps { supports_modify: true, ..IBKR };

/// Hyperliquid (`vike_hyperliquid::exec::HyperliquidExecutionClient` over `ExecActor`; spot +
/// perp). Modify is wired as venue cancel-replace (`supports_modify: true`); HL is batch-first so
/// the exec seam uses the native batch endpoint (`supports_native_batch: true`); perp orders carry
/// `reduceOnly`. On the wire TIF is `Gtc`/`Ioc` (post-only maps to `Alo`, market is an emulated
/// `Ioc`), so `supported_tifs = &[Gtc, Ioc]`. The live `DataClient` (`market_feed::Feeds`) serves
/// candle bars + `bbo` quotes + `trades` + the `l2Book` full-snapshot DOM `depth` lane (no separate
/// lossless `book` tick lane in v1). `vike-backfill` DOES backfill bars now — `vike_backfill::
/// hyperliquid` pages `candleSnapshot` through `klines::backfill_klines` → `append_bars`, shipped
/// as the `hyperliquid_backfill` bin; this row's "no backfill yet (a later add)" outlived the add.
/// See `docs/superpowers/specs/2026-07-16-hyperliquid-bridge-design.md`.
/// Expanded axes: the RICH trigger venue — `build_order_wire` routes `"stop"` AND
/// `"take_profit"` through `build_trigger_kind` (`triggerPx` from `trigger_price`,
/// `tpsl:"sl"/"tp"`, market-or-limit by whether `price` is set), and `"market"` is the emulated
/// IOC limit at ±5% off mid. `accepted_tifs` is all five (Fok→Ioc, Gtd/Day→Gtc are the live
/// coercions — a separate flip). `Alo` (post-only) exists on the wire vocabulary but is
/// unreachable from `OrderRequest` → `supports_post_only: false`. `margin_mode` never read on
/// the order path → `&[Cross]`. The batch action carries N wires with NO adapter-side cap →
/// `max_batch: usize::MAX`.
///
/// **`"stop_limit"` was MISSING from `supported_order_kinds` and is now declared.** The adapter's
/// trigger routing keys off `req.trigger_price.is_some()` (`exec.rs`'s `is_trigger`), not off the
/// `order_type` spelling, so a `"stop_limit"` request has ALWAYS reached `build_trigger_kind`,
/// where `is_market = req.price.is_none()` makes a present limit price a correct RESTING
/// stop-limit at that price (`tpsl: "sl"`). Omitting the kind here meant `preflight_order` returned
/// `TRIGGER_UNSUPPORTED` and `vike-core` terminally rejected an order this adapter builds
/// correctly — a live capability lost to a declaration gap, not to missing code.
/// `crates/bridges/hyperliquid/src/exec.rs`'s `stop_limit_kind_is_declared_and_built` now ties the
/// two together in ONE test, so the row cannot drift back without the wire assertion moving too.
pub const HYPERLIQUID: VenueCaps = VenueCaps {
    supports_modify: true,
    supports_native_batch: true,
    supports_reduce_only: true,
    supports_combo: false,
    supported_tifs: &[TimeInForce::Gtc, TimeInForce::Ioc],
    accepted_tifs: &[
        TimeInForce::Gtc,
        TimeInForce::Ioc,
        TimeInForce::Fok,
        TimeInForce::Gtd,
        TimeInForce::Day,
    ],
    supported_order_kinds: &["market", "limit", "stop", "stop_limit", "take_profit"],
    trigger_types: &[TriggerType::StopLoss, TriggerType::TakeProfit],
    supports_post_only: false,
    margin_modes: &[MarginMode::Cross],
    default_margin_mode: MarginMode::Cross,
    max_batch: usize::MAX,
    live_data: LiveDataCaps { bars: true, quotes: true, trades: true, book: false, depth: true },
    backfill_bars: true,
    backfill_ticks: false,
};

/// cTrader Open API (`vike_ctrader::CtraderExec` / `CtraderData` over the shared-socket actor).
/// AUDITED against the adapter (was the [`VenueCaps::UNSUPPORTED`] fallback the #498 roster PR
/// pinned as "known-stale pending cTrader's own audit" — this row IS that audit, and it flips two
/// fields, a deliberate GUI-visible behavior change: the DOM modify gate now OPENS for cTrader).
///
/// - `supports_modify: true` — `CtraderExec::modify` (exec.rs) resolves the venue `orderId` and
///   enqueues a real `Command::AmendOrder` (`ProtoOaAmendOrderReq`) that the actor writes as
///   `AMEND_ORDER_REQ` (2109); the venue's `ORDER_REPLACED` maps back to `OrderModified`
///   (event_mapper.rs). An unknown coid is a NON-terminal `OrderModifyRejected` (order stays live).
///   This is a native in-place amend end-to-end — the gate opening reaches a real wire path.
/// - `live_data { bars: true, quotes: true }` — `CtraderData` (data.rs) serves `subscribe_bars`
///   (trendbars, 300-bar seed then live) + `subscribe_quotes` (spot L1 bid/ask). `subscribe_trades`
///   /`subscribe_book`/`subscribe_depth` all return `Unsupported`, so those stay `false`.
///
/// Conservative-`false` fields (adapter does NOT wire them): no `submit_batch`/`cancel_batch`
/// override (fan-out default) → `supports_native_batch: false`; `order_to_new_order` sets no
/// `reduceOnly` → `supports_reduce_only: false`; no `combo_legs` read → `supports_combo: false`;
/// the new-order request sends no explicit TIF (defaults GTC), so — like IG — `supported_tifs =
/// &[Gtc]`; no `vike-backfill` cTrader collector → `backfill_bars/ticks: false`.
/// Expanded axes: `order_type_of` maps exactly `"market"`/`"limit"`/`"stop"` (Stop wires
/// `trigger_price`; a priceless conditional is rejected) and any OTHER kind is ALREADY a local
/// terminal `OrderRejected` ("unmappable order") — the preflight matches that reality at the
/// core edge. TIF is never emitted (protocol default) → `accepted_tifs: &[Gtc]` (non-GTC limits
/// flip from silent venue-default to a loud refusal). `margin_mode` never read → `&[Cross]`.
///
/// Bridge-side pin: `vike_ctrader::CAPS` re-exports this row and its `caps_test` module ties it to
/// what `exec.rs`/`data.rs` actually wire (mirroring every sibling bridge crate), so an adapter
/// drift trips there too — not just in this module's own matrix tests
/// (`expanded_axes_matrix_is_pinned`, `ctrader_caps_reflect_the_adapter`).
pub const CTRADER: VenueCaps = VenueCaps {
    supports_modify: true,
    supports_native_batch: false,
    supports_reduce_only: false,
    supports_combo: false,
    supported_tifs: &[TimeInForce::Gtc],
    accepted_tifs: &[TimeInForce::Gtc],
    supported_order_kinds: &["market", "limit", "stop"],
    trigger_types: &[TriggerType::StopLoss],
    supports_post_only: false,
    margin_modes: &[MarginMode::Cross],
    default_margin_mode: MarginMode::Cross,
    max_batch: 0,
    live_data: LiveDataCaps { bars: true, quotes: true, trades: false, book: false, depth: false },
    backfill_bars: false,
    backfill_ticks: false,
};

// vike:new-venue:row /// TODO(new-venue: {venue}): cite the adapter code EVERY field below was read from, the way
// vike:new-venue:row /// the rows above do. The scaffolded value is the MINIMUM row that satisfies
// vike:new-venue:row /// `expanded_axes_invariants_hold` (which refuses an empty kind set and an empty
// vike:new-venue:row /// margin-mode set, so `VenueCaps::UNSUPPORTED` is NOT a legal roster row here).
// vike:new-venue:row pub const {VENUE}: VenueCaps = VenueCaps {
// vike:new-venue:row     supports_modify: false,
// vike:new-venue:row     supports_native_batch: false,
// vike:new-venue:row     supports_reduce_only: false,
// vike:new-venue:row     supports_combo: false,
// vike:new-venue:row     supported_tifs: &[TimeInForce::Gtc],
// vike:new-venue:row     accepted_tifs: &[TimeInForce::Gtc],
// vike:new-venue:row     supported_order_kinds: &["market", "limit"],
// vike:new-venue:row     trigger_types: &[],
// vike:new-venue:row     supports_post_only: false,
// vike:new-venue:row     margin_modes: &[MarginMode::Cross],
// vike:new-venue:row     default_margin_mode: MarginMode::Cross,
// vike:new-venue:row     max_batch: 0,
// vike:new-venue:row     live_data: LiveDataCaps::NONE,
// vike:new-venue:row     backfill_bars: false,
// vike:new-venue:row     backfill_ticks: false,
// vike:new-venue:row };
// vike:new-venue:row
/// The registry: the declared [`VenueCaps`] for a canonical venue string — one arm per
/// [`crate::venues::VENUES`] entry (the completeness test pins that). An unknown venue returns
/// [`VenueCaps::UNSUPPORTED`] — a caller then offers nothing until the venue is known
/// (fail-closed). This is the small keyed registry the GUI consults with the DOM's venue string.
pub fn caps_for(venue: &str) -> VenueCaps {
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
        // vike:new-venue:row "{venue}" => {VENUE}, // TODO(new-venue: {venue}): the row above must be real before this ships
        _ => VenueCaps::UNSUPPORTED,
    }
}

/// Every canonical `OrderRequest.order_type` string the platform produces, lowercase — the
/// vocabulary [`preflight_order`] classifies. `"stop"`/`"stop_limit"`/`"take_profit"` are the
/// TRIGGER kinds (see [`TriggerType`]). A string OUTSIDE this list is deliberately NOT
/// preflight-refused (no declared row can know it) — the venue-side path keeps owning it.
pub const ORDER_KINDS: &[&str] = &["market", "limit", "stop", "stop_limit", "take_profit"];

/// The trigger subset of [`ORDER_KINDS`].
pub const TRIGGER_KINDS: &[&str] = &["stop", "stop_limit", "take_profit"];

/// Why [`preflight_order`] refused an [`OrderRequest`] — one variant per checked axis. The
/// rendered reason ([`core::fmt::Display`]) is MACHINE-READABLE, Nautilus-style:
/// `CATEGORY_CONDITION: key=value ...` (e.g. `TIF_UNSUPPORTED: tif=Fok venue=ig`), so an
/// operator surface or a log grep can dispatch on the prefix without parsing prose.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PreflightDeny {
    /// `order_type` is a known non-trigger kind the venue adapter does not wire.
    OrderKind {
        /// the lowercased requested kind
        kind: String,
        /// the venue whose row refused it
        venue: String,
    },
    /// `order_type` is a trigger kind ([`TRIGGER_KINDS`]) the venue adapter does not wire — the
    /// silent alternative would be the `_ => market` coercion class (a resting stop firing
    /// immediately).
    Trigger {
        /// the lowercased requested trigger kind
        kind: String,
        /// the venue whose row refused it
        venue: String,
    },
    /// a limit order's `time_in_force` is outside the venue's [`VenueCaps::accepted_tifs`].
    Tif {
        /// the requested TIF
        tif: TimeInForce,
        /// the venue whose row refused it
        venue: String,
    },
    /// an explicit `margin_mode` request the venue adapter cannot honor.
    MarginMode {
        /// the requested mode
        mode: MarginMode,
        /// the venue whose row refused it
        venue: String,
    },
}

impl core::fmt::Display for PreflightDeny {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            PreflightDeny::OrderKind { kind, venue } => {
                write!(f, "ORDER_KIND_UNSUPPORTED: kind={kind} venue={venue}")
            }
            PreflightDeny::Trigger { kind, venue } => {
                write!(f, "TRIGGER_UNSUPPORTED: kind={kind} venue={venue}")
            }
            PreflightDeny::Tif { tif, venue } => {
                write!(f, "TIF_UNSUPPORTED: tif={tif:?} venue={venue}")
            }
            PreflightDeny::MarginMode { mode, venue } => {
                write!(f, "MARGIN_MODE_UNSUPPORTED: mode={mode:?} venue={venue}")
            }
        }
    }
}

/// The CORE-EDGE capability preflight: validate an outgoing [`OrderRequest`] against its venue's
/// declared [`VenueCaps`] row BEFORE it reaches the `ExecutionClient` — `Err` means the caller
/// must synthesize a loud TERMINAL refusal (the `supports_combo` arm's `OrderSubmitted` →
/// `OrderRejected` shape in `vike-core`), never send the order, and never let it silently
/// vanish.
///
/// THE COMPAT LAW this function is built around: it refuses NOTHING a venue adapter
/// accepts-and-honors today. Concretely:
/// - an UNKNOWN venue string (not in [`crate::venues::VENUES`]) passes untouched — there is no
///   declared row to enforce, and the paper/sim engines behind non-roster venue ids must keep
///   working (the registry's fail-closed `UNSUPPORTED` fallback is for OFFER surfaces, not for
///   refusing traffic the venue side owns);
/// - a COMBO request (`combo_legs` non-empty) passes — the `OrderIntent::Combo` lowering already
///   resolved `supports_combo` for it;
/// - an `order_type` outside [`ORDER_KINDS`] passes (nothing can claim a row for it);
/// - the TIF check runs on the LIMIT path only, mirroring `deny_unsupported_tif`'s scope — no
///   venue reads a request TIF on its market/trigger paths;
/// - coerced-but-live TIFs are in [`VenueCaps::accepted_tifs`], so they pass (refusing a
///   coercion is a separate per-venue flip);
/// - a `margin_mode` of `None` always passes (it means "the venue's default behavior").
///
/// What DOES get refused is the silent-lie class: kinds the adapter would coerce into different
/// semantics (a "stop" becoming an immediate market order), TIFs the adapter would silently
/// ignore (aster/ig resting GTC regardless), and explicit margin modes no adapter honors.
///
/// ⚠ **`req.venue` is a ROUTING string, not necessarily the venue whose row applies.** This
/// function reads it because for every request this workspace builds the two are the same, but a
/// caller that has ALREADY resolved which engine the request routed to knows better and must say
/// so through [`preflight_order_at`] — see that function's doc for what goes wrong here otherwise
/// (the first bullet of THE COMPAT LAW above, "an UNKNOWN venue string passes untouched", turns
/// from a compatibility affordance into a silent bypass of every check below).
pub fn preflight_order(req: &OrderRequest) -> Result<(), PreflightDeny> {
    preflight_order_at(req, &req.venue)
}

/// [`preflight_order`] with the capability venue supplied EXPLICITLY: `venue` is the canonical
/// exchange id of the engine this request routed to (`ExecutionEngine::venue`), which is the only
/// thing the declared [`VenueCaps`] row is a fact about.
///
/// **Why a caller would ever need this.** `OrderRequest::venue` does two jobs at once — it is what
/// `vike_core`'s `CoreThread::engine_idx_for_route_key` routes on AND what selects the caps row
/// here. Those are the two jobs the canonical/routing split separated on `ExecutionEngine`, and
/// this payload has only one field for them, so the moment a request has to carry a per-ACCOUNT
/// routing key the caps lookup reads a string `crate::venues::VENUES` does not contain — and
/// [`preflight_order`]'s unknown-venue affordance then returns `Ok(())` for it. That is not
/// `VenueCaps::UNSUPPORTED` (which would refuse loudly): it is EVERY check below skipped, silently,
/// for one account of one exchange. `vike_core` therefore resolves the engine first and passes
/// `ExecutionEngine::venue` here.
///
/// Identical to [`preflight_order`] whenever `venue == req.venue`, which is every request in this
/// tree today.
pub fn preflight_order_at(req: &OrderRequest, venue: &str) -> Result<(), PreflightDeny> {
    if !crate::venues::VENUES.contains(&venue) || !req.combo_legs.is_empty() {
        return Ok(());
    }
    let caps = caps_for(venue);
    // Allocation-free classification: every comparison below is `eq_ignore_ascii_case` against
    // `req.order_type` as-borrowed, matching exactly what `to_ascii_lowercase()` + `==`/`.contains`
    // on the lowered copy would (ASCII-only case folding, same as `str::to_ascii_lowercase`) —
    // this runs on EVERY order submit, including the SpreadMaker's per-tick requote path, so no
    // `String` is minted on the (overwhelmingly common) accepted path. The lossy lowercase copy is
    // built ONLY when a refusal actually happens, to carry an owned `kind` in the reason.
    let kind = req.order_type.as_str();
    let is_known_kind = ORDER_KINDS.iter().any(|k| kind.eq_ignore_ascii_case(k));
    let is_supported_kind = caps.supported_order_kinds.iter().any(|k| kind.eq_ignore_ascii_case(k));
    if is_known_kind && !is_supported_kind {
        let is_trigger = TRIGGER_KINDS.iter().any(|k| kind.eq_ignore_ascii_case(k));
        let kind = kind.to_ascii_lowercase();
        return Err(if is_trigger {
            PreflightDeny::Trigger { kind, venue: venue.to_string() }
        } else {
            PreflightDeny::OrderKind { kind, venue: venue.to_string() }
        });
    }
    if kind.eq_ignore_ascii_case("limit") && !caps.accepted_tifs.contains(&req.time_in_force) {
        return Err(PreflightDeny::Tif { tif: req.time_in_force, venue: venue.to_string() });
    }
    if let Some(mode) = req.margin_mode
        && !caps.margin_modes.contains(&mode)
    {
        return Err(PreflightDeny::MarginMode { mode, venue: venue.to_string() });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What this proves, precisely: [`caps_for`]'s match arms map each venue STRING to the const
    /// this module names for it — a transposed-arm typo is loud here. It proves nothing about
    /// whether a const's VALUES describe the adapter; the module doc lists the checks that do.
    /// (Kept alongside `every_roster_venue_has_a_declared_row`, which subsumes it for the roster,
    /// because this one names each pair literally and so reads as the registry's own table.)
    #[test]
    fn registry_maps_every_known_venue() {
        assert_eq!(caps_for("binance"), BINANCE);
        assert_eq!(caps_for("bybit"), BYBIT);
        assert_eq!(caps_for("okx"), OKX);
        assert_eq!(caps_for("deribit"), DERIBIT);
        assert_eq!(caps_for("oanda"), OANDA);
        assert_eq!(caps_for("ig"), IG);
        assert_eq!(caps_for("fxcm"), FXCM);
        assert_eq!(caps_for("dukascopy"), DUKASCOPY);
        assert_eq!(caps_for("polymarket"), POLYMARKET);
        assert_eq!(caps_for("aster"), ASTER);
        assert_eq!(caps_for("hyperliquid"), HYPERLIQUID);
    }

    /// Completeness vs the canonical roster: every [`crate::venues::VENUES`] entry has a NAMED
    /// declared row, and the registry serves exactly it. Adding a venue to the roster fails here
    /// until its row exists — the whole point of the roster. (A row's VALUE may equal the
    /// `UNSUPPORTED` fallback — cTrader's does, deliberately — but it must be NAMED: the named
    /// const is the proof the venue was classified, not forgotten.)
    #[test]
    fn every_roster_venue_has_a_declared_row() {
        // `#[rustfmt::skip]`: a `just new-venue` marker at the TAIL of a bracketed literal is
        // re-indented by rustfmt once a row ending in a trailing `//` comment is generated above
        // it, which defeats `--remove`. Gated by `crates/vike-ops/tests/new_venue_gate.rs`'s
        // `a_trailing_comment_marker_is_rustfmt_skipped_unless_a_recognised_sibling_follows`.
        #[rustfmt::skip]
        let rows: &[(&str, VenueCaps)] = &[
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
        assert_eq!(rows.len(), crate::venues::VENUES.len(), "one declared row per roster venue");
        for &v in crate::venues::VENUES {
            let (_, want) = rows
                .iter()
                .find(|(rv, _)| *rv == v)
                .unwrap_or_else(|| panic!("no VenueCaps row declared for roster venue {v}"));
            assert_eq!(caps_for(v), *want, "{v}: registry must serve its declared row");
        }
    }

    #[test]
    fn unknown_venue_is_conservative_unsupported() {
        let c = caps_for("nasdaq");
        assert_eq!(c, VenueCaps::UNSUPPORTED);
        assert!(!c.supports_modify);
        assert!(!c.supports_native_batch);
        assert!(!c.supports_reduce_only);
        assert!(c.supported_tifs.is_empty());
        assert!(c.accepted_tifs.is_empty());
        assert!(c.supported_order_kinds.is_empty());
        assert!(c.trigger_types.is_empty());
        assert!(!c.supports_post_only);
        assert!(c.margin_modes.is_empty());
        assert_eq!(c.default_margin_mode, MarginMode::Cross);
        assert_eq!(c.max_batch, 0);
        assert!(!c.has_live_data());
        assert!(!c.backfill_bars && !c.backfill_ticks);
    }

    #[test]
    fn default_is_unsupported() {
        assert_eq!(VenueCaps::default(), VenueCaps::UNSUPPORTED);
    }

    /// The modify-gate logic: it BLOCKS (returns false) for every venue whose adapter has no native
    /// modify, and ALLOWS the venues that wire one. This is the exact decision the GUI wires.
    #[test]
    fn modify_gate_blocks_when_unsupported() {
        // allowed: the venues with a verified native amend (crypto perps + HL cancel-replace +
        // cTrader's `AMEND_ORDER_REQ` amend, audited from the adapter).
        for v in ["binance", "bybit", "okx", "aster", "hyperliquid", "ctrader"] {
            assert!(caps_for(v).allows_modify(), "{v} should allow modify");
        }
        // blocked: no native modify wired
        for v in ["deribit", "oanda", "ig", "fxcm", "dukascopy", "polymarket", "ibkr", "unknown"] {
            assert!(!caps_for(v).allows_modify(), "{v} must block modify");
        }
    }

    /// cTrader's audited row (audit of the #498 known-stale `UNSUPPORTED` placeholder): a native
    /// amend (`supports_modify`) + a spot-quote/trendbar `DataClient`, everything else conservative.
    /// Pins the exact matrix so a regression that re-widens or re-narrows a field trips here.
    #[test]
    fn ctrader_caps_reflect_the_adapter() {
        let c = caps_for("ctrader");
        assert_eq!(c, CTRADER);
        // the two flipped-from-UNSUPPORTED fields
        assert!(c.supports_modify, "ctrader wires a native AmendOrder");
        assert_eq!(
            c.live_data,
            LiveDataCaps { bars: true, quotes: true, trades: false, book: false, depth: false },
            "ctrader DataClient serves trendbars + spot quotes only"
        );
        assert!(c.has_live_data());
        // the fields the adapter does NOT wire — must stay conservative
        assert!(!c.supports_native_batch, "no batch endpoint wired");
        assert!(!c.supports_reduce_only, "order_to_new_order sets no reduceOnly");
        assert!(!c.supports_combo);
        assert_eq!(c.supported_tifs, &[TimeInForce::Gtc], "no explicit TIF sent → GTC default");
        assert!(!c.backfill_bars && !c.backfill_ticks, "no vike-backfill cTrader collector");
    }

    /// Only Binance + Aster wire a native batch endpoint; everyone else uses the fan-out default.
    /// (Property assertions go through the non-const `caps_for` so they aren't const-folded — the
    /// registry equals the consts, pinned by `registry_maps_every_known_venue`.)
    #[test]
    fn native_batch_is_binance_aster_hyperliquid() {
        // Adapters that wire a native batch endpoint (binance/aster POST .../batchOrders ≤5;
        // hyperliquid's msgpack action batch).
        assert!(caps_for("binance").supports_native_batch);
        assert!(caps_for("aster").supports_native_batch);
        assert!(caps_for("hyperliquid").supports_native_batch);
        for v in [
            "bybit",
            "okx",
            "deribit",
            "oanda",
            "ig",
            "fxcm",
            "dukascopy",
            "polymarket",
            "ibkr",
            "alpaca",
        ] {
            assert!(!caps_for(v).supports_native_batch, "{v} must not claim native batch");
        }
    }

    /// Combo support is DERIBIT ONLY — the deliberate live-gate flip: its adapter resolves
    /// `combo_legs` at submit (`resolve_combo_order`: create_combo → orientation solve →
    /// buy/sell on the combo id), which no other adapter does. Every OTHER roster venue (and the
    /// unknown fallback, and the derived IBKR_CPAPI row) must stay `false` — a `true` row without
    /// the resolve step would put an EMPTY-symbol order on a live wire (`build_combo` leaves
    /// `symbol` for the adapter). Pinned per-venue so an accidental flip anywhere trips here.
    #[test]
    fn combo_support_is_deribit_only() {
        assert!(caps_for("deribit").supports_combo, "deribit wires resolve-at-submit combos");
        for v in [
            "binance",
            "bybit",
            "okx",
            "oanda",
            "ig",
            "fxcm",
            "dukascopy",
            "polymarket",
            "ibkr",
            "ctrader",
            "alpaca",
            "aster",
            "hyperliquid",
            "unknown",
        ] {
            assert!(!caps_for(v).supports_combo, "{v} must not claim combo support");
        }
        // the derived cpapi backend row inherits IBKR's `false` (compared through the non-const
        // registry so the assertion isn't const-folded, per this module's test convention)
        assert_eq!(
            IBKR_CPAPI.supports_combo,
            caps_for("ibkr").supports_combo,
            "the cpapi backend row must inherit IBKR's combo stance"
        );
    }

    /// reduce-only is a perp/options concept: the crypto perps + deribit build it; FX/CFD,
    /// dukascopy (unverifiable), and polymarket do not.
    #[test]
    fn reduce_only_matches_derivatives_venues() {
        for v in ["binance", "bybit", "okx", "deribit"] {
            assert!(caps_for(v).supports_reduce_only, "{v} builds reduceOnly");
        }
        for v in ["oanda", "ig", "fxcm", "dukascopy", "polymarket"] {
            assert!(!caps_for(v).supports_reduce_only, "{v} does not wire reduceOnly");
        }
    }

    /// The live-data matrix: every verb each venue with a `DataClient` actually serves, exactly as
    /// its feed implements it — the crypto depth-vs-book split, polymarket's bars-less shape, ibkr's
    /// full five, and deribit's book-without-depth.
    #[test]
    fn live_data_matrix_matches_the_feeds() {
        // crypto perps: bars + trades + depth (bybit/okx trades live-verified 2026-07-20 via each
        // crate's `*_trades_feed_smoke` against the real `DataClient::subscribe_trades` path).
        assert_eq!(
            BINANCE.live_data,
            LiveDataCaps { bars: true, quotes: false, trades: true, book: false, depth: true }
        );
        assert_eq!(
            BYBIT.live_data,
            LiveDataCaps { bars: true, quotes: false, trades: true, book: false, depth: true }
        );
        assert_eq!(BYBIT.live_data, OKX.live_data);
        assert_eq!(BYBIT.live_data, BINANCE.live_data);
        // polymarket: quotes + trades + book, no bars, no depth
        assert_eq!(
            POLYMARKET.live_data,
            LiveDataCaps { bars: false, quotes: true, trades: true, book: true, depth: false }
        );
        // ibkr: ALL FIVE verbs, all real — `IbkrFeeds` (market_feed/mod.rs, #306) implements every
        // one over the ibapi socket session; `depth_pump` feeds the book AND the depth lane from
        // the same `reqMktDepth` ladder. This row said `LiveDataCaps::NONE` from 2026-07-14 until
        // the pump-spec cross-pin caught it.
        assert_eq!(
            IBKR.live_data,
            LiveDataCaps { bars: true, quotes: true, trades: true, book: true, depth: true }
        );
        assert_eq!(IBKR_CPAPI.live_data, IBKR.live_data, "the cpapi row inherits the feed");
        // oanda: the SPLIT-PLANE feed — quotes stream (the chunked-HTTP pricing stream), bars
        // poll the candles REST. No trade tape is published, and the pricing ladder is an
        // unsequenced top-of-book snapshot rather than a delta-synced book → book/depth false.
        assert_eq!(
            OANDA.live_data,
            LiveDataCaps { bars: true, quotes: true, trades: false, book: false, depth: false }
        );
        // deribit (split-plane I9): bars + quotes + trades + the LOSSLESS book lane; the
        // conflating DOM `depth` lane is the one verb not wired — `subscribe_depth` refuses
        // through this row (`vike_deribit::market_feed::Feeds`).
        assert_eq!(
            DERIBIT.live_data,
            LiveDataCaps { bars: true, quotes: true, trades: true, book: true, depth: false }
        );
        // ig serves bars+quotes only — the dealer-venue shape: L1 with no ladder, no tape.
        assert_eq!(
            IG.live_data,
            LiveDataCaps { bars: true, quotes: true, trades: false, book: false, depth: false }
        );
        // no live feed at all for these
        for v in ["fxcm", "dukascopy"] {
            assert!(!caps_for(v).has_live_data(), "{v} has no live DataClient");
        }
        // ...and every OTHER roster venue does have one. Stated as the complement so a venue can
        // never fall out of both lists. The non-circular tie for exactly this partition is
        // `venue_caps_cross_pin_the_pump_spec` in vike-bridge-core.
        for v in [
            "binance",
            "bybit",
            "okx",
            "aster",
            "hyperliquid",
            "deribit",
            "polymarket",
            "alpaca",
            "ctrader",
            "ibkr",
            "oanda",
            "ig",
        ] {
            assert!(caps_for(v).has_live_data(), "{v} has a live DataClient");
        }
    }

    /// TIF sets track the adapters: OANDA maps the full five; the flipped venues (tif step-2)
    /// declare exactly the set their adapter maps 1:1 (everything else loud-denied at submit);
    /// the unflipped GTC-hardcoders stay `&[Gtc]`.
    #[test]
    fn tif_sets_reflect_what_the_adapter_wires() {
        let oanda = caps_for("oanda");
        assert_eq!(oanda.supported_tifs.len(), 5);
        assert!(oanda.supported_tifs.contains(&TimeInForce::Day));
        assert!(oanda.supported_tifs.contains(&TimeInForce::Gtd));
        // binance: the row stays the CONSERVATIVE spot∩perp intersection — the perp lane
        // additionally wires native GTD, but this registry cannot distinguish the lanes (one
        // "binance" key), so Gtd is deliberately NOT declared here; the per-lane truth is
        // venue_tif's "binance-perp" row (see the BINANCE row doc).
        for v in ["binance", "bybit", "okx"] {
            assert_eq!(
                caps_for(v).supported_tifs,
                &[TimeInForce::Gtc, TimeInForce::Ioc, TimeInForce::Fok],
                "{v}: GTC/IOC/FOK declared (binance = lane intersection, perp-GTD lives in \
                 venue_tif)"
            );
        }
        assert_eq!(
            caps_for("deribit").supported_tifs,
            &[TimeInForce::Gtc, TimeInForce::Ioc, TimeInForce::Fok, TimeInForce::Day],
            "deribit flipped: GTC default + IOC/FOK/Day mapped, GTD denied"
        );
        for v in ["ig", "fxcm", "dukascopy", "aster"] {
            assert_eq!(caps_for(v).supported_tifs, &[TimeInForce::Gtc], "{v} is GTC-only");
        }
        assert_eq!(caps_for("polymarket").supported_tifs, &[TimeInForce::Gtc, TimeInForce::Fok]);
    }

    /// The backfill axes, pinned per roster venue.
    ///
    /// ⚠ This test was called `backfill_matches_vike_backfill` and matched NOTHING in
    /// vike-backfill — it asserted a hand-written list, and the list was WRONG: it required
    /// `!backfill_bars` for **deribit**, which has had a real `deribit_backfill` bin since #1030,
    /// and required `!backfill_bars` for dukascopy, whose collector resamples bars from the ticks
    /// it stores. Three more rows were wrong and simply absent from the list (hyperliquid, ibkr,
    /// and dukascopy's `bar`), so nothing failed. Renamed to say what it does.
    ///
    /// **The non-circular check is `venue_caps_cross_pin_the_backfill_table` in vike-backfill**,
    /// which compares these two fields to `backfill_caps(Source::Venue, v)` — a table with a
    /// filesystem-existence gate and a source-scanning shape gate over the real collector modules.
    /// It lives THERE because vike-backfill depends on vike-model and not the reverse.
    #[test]
    fn backfill_matrix_is_pinned() {
        // (venue, bars, ticks) — one row per roster venue, verbatim.
        #[rustfmt::skip]
        let rows: &[(&str, bool, bool)] = &[
            ("binance",     true,  false),
            ("bybit",       true,  false),
            ("okx",         true,  false),
            ("aster",       true,  false),
            // klines collectors that landed AFTER this table was written; each row said `false`
            // until the vike-backfill cross-pin was added.
            ("deribit",     true,  false),
            ("hyperliquid", true,  false),
            ("ibkr",        true,  false),
            // the ONLY venue-direct tick source (.bi5 quotes) — and its bars are a resample of
            // them, which this field's doc settles as `backfill_bars: true`.
            ("dukascopy",   true,  true),
            ("oanda",       false, false),
            ("ig",          false, false),
            ("fxcm",        false, false),
            ("polymarket",  false, false),
            ("ctrader",     false, false),
            ("alpaca",      false, false),
            // vike:new-venue:row ("{venue}",  false, false), // TODO(new-venue: {venue}): flip when a collector lands
        ];
        assert_eq!(rows.len(), crate::venues::VENUES.len(), "one pinned row per roster venue");
        for (v, bars, ticks) in rows {
            let c = caps_for(v);
            assert_eq!(c.backfill_bars, *bars, "{v} backfill_bars");
            assert_eq!(c.backfill_ticks, *ticks, "{v} backfill_ticks");
        }
    }

    // ── expanded axes (w2-task-5) ────────────────────────────────────────────────────────────

    /// The NEW-axis matrix, pinned verbatim per venue (the tif/margin matrix-test discipline):
    /// (venue, order_kinds, trigger_types, accepted_tifs, margin_modes, max_batch). Any drift in
    /// a row is loud here; each bridge's own `caps_test` additionally ties its row to the
    /// adapter code the value was read from.
    #[test]
    fn expanded_axes_matrix_is_pinned() {
        use MarginMode::{Cash, Cross, Isolated};
        use TimeInForce::{Day, Fok, Gtc, Gtd, Ioc};
        use TriggerType::{StopLoss, TakeProfit};
        const ML: &[&str] = &["market", "limit"];
        const MLS: &[&str] = &["market", "limit", "stop"];
        const ALL5: &[TimeInForce] = &[Gtc, Ioc, Fok, Gtd, Day];
        // (venue, order_kinds, trigger_types, accepted_tifs, margin_modes, max_batch)
        type Row = (
            &'static str,
            &'static [&'static str],
            &'static [TriggerType],
            &'static [TimeInForce],
            &'static [MarginMode],
            usize,
        );
        #[rustfmt::skip]
        let rows: &[Row] = &[
            ("binance",     MLS, &[StopLoss], &[Gtc, Ioc, Fok, Gtd],       &[Cross], 5),
            ("bybit",       MLS, &[StopLoss], &[Gtc, Ioc, Fok],            &[Cross], 0),
            ("okx",         MLS, &[StopLoss], &[Gtc, Ioc, Fok],            &[Cross, Isolated], 0),
            ("aster",       MLS, &[StopLoss], &[Gtc],                      &[Cross], 5),
            ("deribit",     ML,  &[],         &[Gtc, Ioc, Fok, Day],       &[Cross], 0),
            ("oanda",       MLS, &[StopLoss], ALL5,                        &[Cross], 0),
            ("alpaca",      &["market", "limit", "stop", "stop_limit"], &[StopLoss],
                &[Day, Gtc, Ioc, Fok, Gtd],                               &[Cross], 0),
            ("ig",          MLS, &[StopLoss], &[Gtc],                      &[Cross], 0),
            ("fxcm",        ML,  &[],         &[Gtc],                      &[Cross], 0),
            ("dukascopy",   ML,  &[],         &[Gtc],                      &[Cross], 0),
            ("polymarket",  ML,  &[],         ALL5,                        &[Cash],  0),
            ("ibkr",        MLS, &[StopLoss], &[Gtc, Day, Ioc, Fok, Gtd],  &[Cross], 0),
            ("ctrader",     MLS, &[StopLoss], &[Gtc],                      &[Cross], 0),
            ("hyperliquid", &["market", "limit", "stop", "stop_limit", "take_profit"],
                &[StopLoss, TakeProfit], ALL5,                            &[Cross], usize::MAX),
            // vike:new-venue:row ("{venue}",     ML,  &[],         &[Gtc],                      &[Cross], 0), // TODO(new-venue: {venue}): pin what the adapter really wires
        ];
        assert_eq!(rows.len(), crate::venues::VENUES.len(), "one pinned row per roster venue");
        for (venue, kinds, triggers, accepted, margins, max_batch) in rows {
            let c = caps_for(venue);
            assert_eq!(c.supported_order_kinds, *kinds, "{venue} supported_order_kinds");
            assert_eq!(c.trigger_types, *triggers, "{venue} trigger_types");
            assert_eq!(c.accepted_tifs, *accepted, "{venue} accepted_tifs");
            assert_eq!(c.margin_modes, *margins, "{venue} margin_modes");
            assert_eq!(c.max_batch, *max_batch, "{venue} max_batch");
            assert!(!c.supports_post_only, "{venue}: post-only is inexpressible via OrderRequest");
        }
    }

    /// Structural invariants across every roster venue: the admit set contains the honored set;
    /// trigger declarations agree between the two axes; the batch cap agrees with the batch
    /// flag; every declared kind is canonical vocabulary; a roster venue always wires SOME kind.
    #[test]
    fn expanded_axes_invariants_hold() {
        for &v in crate::venues::VENUES {
            let c = caps_for(v);
            for t in c.supported_tifs {
                assert!(c.accepted_tifs.contains(t), "{v}: accepted_tifs must contain {t:?}");
            }
            let kinds_have_trigger =
                c.supported_order_kinds.iter().any(|k| TRIGGER_KINDS.contains(k));
            assert_eq!(
                kinds_have_trigger,
                !c.trigger_types.is_empty(),
                "{v}: trigger_types ⇔ a trigger kind in supported_order_kinds"
            );
            assert_eq!(c.max_batch > 0, c.supports_native_batch, "{v}: max_batch ⇔ native batch");
            for k in c.supported_order_kinds {
                assert!(ORDER_KINDS.contains(k), "{v}: non-canonical order kind {k}");
            }
            assert!(!c.supported_order_kinds.is_empty(), "{v}: must wire at least one kind");
            assert!(!c.margin_modes.is_empty(), "{v}: must honor at least its default mode");
            // The default vike SENDS must be a mode an explicit request could also name. Both
            // sides are this table's own fields, so this is an INTRA-table invariant now — it
            // used to be half of the cross-pin below, back when the default lived in the other
            // table.
            assert!(
                c.margin_modes.contains(&c.default_margin_mode),
                "{v}: default_margin_mode must be request-honorable"
            );
        }
    }

    /// The cross-table contract with [`crate::venue_margin_support`], in one sentence: **what we
    /// SEND is a subset of what they OFFER.**
    ///
    /// The two tables have different subjects — this one is what vike's adapters do, that one is
    /// what the exchange makes available — and the only thing that can be checked BETWEEN them is
    /// containment:
    /// - every mode an order may REQUEST here (`margin_modes`) is a mode the venue offers, and
    /// - the mode vike sends by DEFAULT (`default_margin_mode`) is likewise one the venue offers.
    ///
    /// The second used to be stated against this table alone ("the default is request-honorable"),
    /// which is now an intra-table invariant asserted in `expanded_axes_invariants_hold`. Stated
    /// against the OFFER table it says something that assertion cannot: vike does not default to a
    /// mode the exchange never exposed.
    #[test]
    fn margin_modes_cross_pin_venue_margin_support() {
        for &v in crate::venues::VENUES {
            let caps = caps_for(v);
            let support = crate::venue_margin_support::venue_margin_support(v);
            for m in caps.margin_modes {
                assert!(
                    support.offered_modes.contains(m),
                    "{v}: order-honored mode {m:?} must be offered by the venue"
                );
            }
            assert!(
                support.offered_modes.contains(&caps.default_margin_mode),
                "{v}: the mode vike sends by default must be one the venue offers"
            );
        }
    }

    /// TODAY'S REALITY, relocated verbatim from the margin table when `default_margin_mode` moved
    /// here: every venue vike trades perps on produces Cross; polymarket produces Cash. This pins
    /// the current wire behavior.
    ///
    /// Only okx's row is tied to adapter CODE (see
    /// `crates/bridges/okx/src/perp.rs`'s `okx_default_margin_mode_matches_the_td_mode_builder`) —
    /// it is the one adapter that emits a margin field at all. Every other row records the
    /// account-side default that rules because the adapter sends nothing, which no test here can
    /// prove.
    ///
    /// ⚠ Read `hyperliquid`'s row below with the field's own "the default for assets that HAVE a
    /// choice" caveat: 9 of its 232 assets are isolated-only and rule Isolated. This test pins the
    /// per-VENUE declaration (which is what the field is); the per-ASSET narrowing is pinned in the
    /// bridge, by `crates/bridges/hyperliquid/src/symbology.rs`'s
    /// `effective_margin_mode_is_per_asset_not_per_venue` — which asserts the two DISAGREE for an
    /// isolated-only asset, so flipping this row to make them agree would break that test loudly.
    #[test]
    fn default_margin_mode_reflects_current_behavior() {
        for v in ["binance", "aster", "okx", "bybit", "hyperliquid", "deribit"] {
            assert_eq!(caps_for(v).default_margin_mode, MarginMode::Cross, "{v} is cross today");
        }
        assert_eq!(caps_for("polymarket").default_margin_mode, MarginMode::Cash);
        // FX/equity also resolve to Cross (shared account margin)
        for v in ["oanda", "ig", "fxcm", "dukascopy", "alpaca", "ibkr", "ctrader"] {
            assert_eq!(caps_for(v).default_margin_mode, MarginMode::Cross, "{v}");
        }
        // The unknown-venue fallback keeps the same conservative value.
        assert_eq!(VenueCaps::UNSUPPORTED.default_margin_mode, MarginMode::Cross);
    }

    // ── preflight (w2-task-5) ────────────────────────────────────────────────────────────────

    fn req(venue: &str, order_type: &str, tif: TimeInForce) -> OrderRequest {
        OrderRequest {
            client_order_id: "c-pf".to_string(),
            venue: venue.to_string(),
            symbol: "X".to_string(),
            side: 1,
            qty: 1.0,
            order_type: order_type.to_string(),
            price: Some(1.0),
            time_in_force: tif,
            ..Default::default()
        }
    }

    /// THE COMPAT LAW: nothing a venue accepts-and-honors today is refused. Every row's own
    /// declared kinds/TIFs pass; the coercion venues' coerced TIFs pass; binance's perp-lane GTD
    /// passes (lane union); non-limit paths never TIF-refuse; unknown venues, unknown kind
    /// strings, combos and `margin_mode: None` all pass.
    #[test]
    fn preflight_passes_everything_accepted_today() {
        use TimeInForce::{Day, Fok, Gtc, Gtd, Ioc};
        for &v in crate::venues::VENUES {
            let c = caps_for(v);
            for k in c.supported_order_kinds {
                assert_eq!(preflight_order(&req(v, k, Gtc)), Ok(()), "{v}/{k}");
            }
            for t in c.accepted_tifs {
                assert_eq!(preflight_order(&req(v, "limit", *t)), Ok(()), "{v}/limit/{t:?}");
            }
            // market path carries no TIF axis anywhere — never TIF-refused
            for t in [Gtc, Ioc, Fok, Gtd, Day] {
                if c.supported_order_kinds.contains(&"market") {
                    assert_eq!(preflight_order(&req(v, "market", t)), Ok(()), "{v}/market/{t:?}");
                }
            }
        }
        // the coercion venues' coerced TIFs (live behavior — a separate flip)
        assert_eq!(preflight_order(&req("polymarket", "limit", TimeInForce::Ioc)), Ok(()));
        assert_eq!(preflight_order(&req("hyperliquid", "limit", TimeInForce::Fok)), Ok(()));
        assert_eq!(preflight_order(&req("alpaca", "limit", TimeInForce::Gtd)), Ok(()));
        // binance lane union: perp GTD must NOT be refused at the core edge
        assert_eq!(preflight_order(&req("binance", "limit", TimeInForce::Gtd)), Ok(()));
        // unknown venue (paper/sim ids) — no declared row, never refused here
        assert_eq!(preflight_order(&req("sim", "limit", TimeInForce::Ioc)), Ok(()));
        // unknown kind string — no row can claim it, venue side owns it
        assert_eq!(preflight_order(&req("binance", "market_close", TimeInForce::Gtc)), Ok(()));
        // combo requests: the Combo lowering owns capability
        let mut combo = req("oanda", "limit", TimeInForce::Gtc);
        combo.combo_legs = vec![crate::order::ComboLeg { symbol: "A".into(), ratio: 1 }];
        assert_eq!(preflight_order(&combo), Ok(()));
    }

    /// The refusals, category by category, with the machine-readable reason strings pinned
    /// verbatim (`CATEGORY_CONDITION: key=value`).
    #[test]
    fn preflight_refuses_the_silent_lie_class() {
        use TimeInForce::{Fok, Gtc, Ioc};
        // TIF: aster/ig would silently rest GTC (the flip this task ships)
        let deny = preflight_order(&req("aster", "limit", Ioc)).unwrap_err();
        assert_eq!(deny.to_string(), "TIF_UNSUPPORTED: tif=Ioc venue=aster");
        let deny = preflight_order(&req("ig", "limit", Fok)).unwrap_err();
        assert_eq!(deny.to_string(), "TIF_UNSUPPORTED: tif=Fok venue=ig");
        // TIF: a flipped venue's venue-side deny now fires earlier, at the core edge
        let deny = preflight_order(&req("bybit", "limit", TimeInForce::Gtd)).unwrap_err();
        assert_eq!(deny.to_string(), "TIF_UNSUPPORTED: tif=Gtd venue=bybit");
        // binance Day: refused on BOTH lanes venue-side → refused here too
        assert!(preflight_order(&req("binance", "limit", TimeInForce::Day)).is_err());
        // trigger kinds: deribit's `_ => market` coercion would fire a stop IMMEDIATELY
        let deny = preflight_order(&req("deribit", "stop", Gtc)).unwrap_err();
        assert_eq!(deny.to_string(), "TRIGGER_UNSUPPORTED: kind=stop venue=deribit");
        let deny = preflight_order(&req("binance", "take_profit", Gtc)).unwrap_err();
        assert_eq!(deny.to_string(), "TRIGGER_UNSUPPORTED: kind=take_profit venue=binance");
        // non-trigger kind: fxcm's shim now HAS a true-market placement, so `"market"` is honest
        // there and passes — fxcm's row was the LAST declared venue denying a non-trigger kind.
        assert_eq!(preflight_order(&req("fxcm", "market", Gtc)), Ok(()));
        // With that flip, `PreflightDeny::OrderKind` is unreachable through `preflight_order`:
        // every declared venue serves both non-trigger kinds, and an UNDECLARED venue is skipped
        // outright (no declared row can classify it). Both halves of that reasoning are asserted
        // below so a future row that drops "market"/"limit" re-arms the class instead of silently
        // shipping a lie; the variant's machine-readable rendering stays pinned directly.
        for &v in crate::venues::VENUES {
            let kinds = caps_for(v).supported_order_kinds;
            assert!(
                kinds.contains(&"market") && kinds.contains(&"limit"),
                "{v}: non-trigger kinds"
            );
        }
        assert_eq!(
            preflight_order(&req("nasdaq", "market", Gtc)),
            Ok(()),
            "undeclared venue skipped"
        );
        let deny = PreflightDeny::OrderKind { kind: "market".into(), venue: "nasdaq".into() };
        assert_eq!(deny.to_string(), "ORDER_KIND_UNSUPPORTED: kind=market venue=nasdaq");
        // margin: an explicit Isolated request no adapter honors (binance ignores the field)
        let mut iso = req("binance", "limit", Gtc);
        iso.margin_mode = Some(MarginMode::Isolated);
        let deny = preflight_order(&iso).unwrap_err();
        assert_eq!(deny.to_string(), "MARGIN_MODE_UNSUPPORTED: mode=Isolated venue=binance");
        // margin: okx honors Isolated per order (passes) but denies Cash (spot-only mode)
        let mut okx_iso = req("okx", "limit", Gtc);
        okx_iso.margin_mode = Some(MarginMode::Isolated);
        assert_eq!(preflight_order(&okx_iso), Ok(()));
        let mut okx_cash = req("okx", "limit", Gtc);
        okx_cash.margin_mode = Some(MarginMode::Cash);
        let deny = preflight_order(&okx_cash).unwrap_err();
        assert_eq!(deny.to_string(), "MARGIN_MODE_UNSUPPORTED: mode=Cash venue=okx");
        // case-insensitivity: the kind is lowercased before classification
        assert!(preflight_order(&req("deribit", "STOP", Gtc)).is_err());
    }

    /// The allocation-free `eq_ignore_ascii_case` rewrite must match EXACTLY what the old
    /// `to_ascii_lowercase()` + `==`/`.contains` comparisons matched — a mixed-case spelling of a
    /// supported kind still passes, not just the all-caps case above.
    #[test]
    fn preflight_matches_mixed_case_spellings() {
        use TimeInForce::Gtc;
        // "Stop" (mixed case) is binance's native trigger kind — must still be ACCEPTED.
        assert_eq!(preflight_order(&req("binance", "Stop", Gtc)), Ok(()));
        // "MaRkEt" is a supported kind everywhere it's declared — must still be ACCEPTED.
        assert_eq!(preflight_order(&req("aster", "MaRkEt", Gtc)), Ok(()));
        // "Limit" (mixed case) still hits the TIF branch and is refused for aster's unaccepted IOC.
        let deny = preflight_order(&req("aster", "Limit", TimeInForce::Ioc)).unwrap_err();
        assert_eq!(deny.to_string(), "TIF_UNSUPPORTED: tif=Ioc venue=aster");
    }

    /// `LiveDataCaps::supports` projects each verb onto its field (the seam the vike-data
    /// `require_live_verb` helper drives refusals off).
    #[test]
    fn live_verb_projection_matches_fields() {
        let d = LiveDataCaps { bars: true, quotes: false, trades: true, book: false, depth: true };
        assert!(d.supports(LiveVerb::Bars));
        assert!(!d.supports(LiveVerb::Quotes));
        assert!(d.supports(LiveVerb::Trades));
        assert!(!d.supports(LiveVerb::Book));
        assert!(d.supports(LiveVerb::Depth));
        for v in
            [LiveVerb::Bars, LiveVerb::Quotes, LiveVerb::Trades, LiveVerb::Book, LiveVerb::Depth]
        {
            assert!(!LiveDataCaps::NONE.supports(v));
        }
    }
}
