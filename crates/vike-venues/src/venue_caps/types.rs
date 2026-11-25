//! The capability types; the per-venue rows and the preflight guard stay in the parent module.

#[cfg(doc)]
use super::{ORDER_ATTRIBUTES, emulation_of, preflight_order};
use vike_model::StreamLabel;
use vike_model::money::margin_mode::MarginMode;
#[cfg(doc)]
use vike_model::orders::order::OrderType;
use vike_model::orders::order::TimeInForce;

/// Which live market-data verbs a venue's `vike_data::DataClient` actually serves. A venue with no
/// live feed at all (the FX/CFD venues, dukascopy) has every field `false`.
///
/// `book` is the lossless L2 tick lane (`subscribe_book` → `DataClientSink::book`/`book_update`);
/// `depth` is the conflating L2 snapshot lane (`subscribe_depth` → `DataClientSink::l2_snapshot`,
/// what the DOM renders). They are separate capabilities on purpose — the crypto venues serve
/// `depth` but not `book`, polymarket serves `book` but not `depth`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DataClientCapabilities {
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

impl DataClientCapabilities {
    /// A venue with no live market-data feed at all.
    pub const NONE: DataClientCapabilities = DataClientCapabilities {
        bars: false,
        quotes: false,
        trades: false,
        book: false,
        depth: false,
    };

    /// Whether this venue serves `verb` — the ONE lookup a feed's refusal path should consult
    /// instead of hand-rolling its own `Unsupported` decision (the vike-data `require_live_verb`
    /// helper wraps this into the canonical `DataClientError::Unsupported`).
    #[inline]
    #[must_use]
    pub const fn supports(&self, verb: DataClientVerb) -> bool {
        match verb {
            DataClientVerb::Bars => self.bars,
            DataClientVerb::Quotes => self.quotes,
            DataClientVerb::Trades => self.trades,
            DataClientVerb::Book => self.book,
            DataClientVerb::Depth => self.depth,
        }
    }
}

/// The five live market-data verbs of the `vike_data::DataClient` seam, as an enum so a refusal
/// can be DRIVEN off a venue's declared [`DataClientCapabilities`] row instead of each feed hand-writing
/// its own `Unsupported` arm (spec: one declaration, many consumers).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DataClientVerb {
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

impl From<StreamLabel> for DataClientVerb {
    /// The verb that serves a tick lane — the seam through which `vike_data::require_live_verb`
    /// decides whether a venue may be asked for it. Total over [`StreamLabel`], so a new label fails
    /// to compile here until its verb is decided.
    fn from(label: StreamLabel) -> Self {
        match label {
            StreamLabel::Quotes => DataClientVerb::Quotes,
            StreamLabel::Trades => DataClientVerb::Trades,
            StreamLabel::Book => DataClientVerb::Book,
            StreamLabel::Depth => DataClientVerb::Depth,
        }
    }
}

impl DataClientVerb {
    /// The tick lane this verb serves; `None` for [`DataClientVerb::Bars`], whose stream label is an
    /// interval. Exhaustive over `DataClientVerb`, so a new verb must be decided here before this crate
    /// builds.
    #[must_use]
    pub const fn stream_label(self) -> Option<StreamLabel> {
        match self {
            DataClientVerb::Bars => None,
            DataClientVerb::Quotes => Some(StreamLabel::Quotes),
            DataClientVerb::Trades => Some(StreamLabel::Trades),
            DataClientVerb::Book => Some(StreamLabel::Book),
            DataClientVerb::Depth => Some(StreamLabel::Depth),
        }
    }
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
    /// `order_type: "take_profit"` — a take-profit trigger (hyperliquid `tpsl:"tp"` and deribit's
    /// `take_market`/`take_limit` are the adapters that wire it natively today).
    TakeProfit,
}

/// The declared static capability matrix for ONE venue adapter. `Copy` (all fields are `bool` /
/// `&'static [_]`) so it is cheap to hand to a UI frame by value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VenueCapabilities {
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
    /// `VenueRestDeribit::submit_order` routes a non-empty `combo_legs` through its combo book —
    /// `private/create_combo`, the orientation/scale solve (`vike_deribit::execution::combo`), then
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
    /// [`crate::venue_time_in_force::venue_time_in_force`] — see the `BINANCE` row doc. Cross-pinned against the
    /// `venue_time_in_force` table by that module's own `venue_capabilities_cross_pin_the_time_in_force_table` test so the
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
    /// cross-pinned against `venue_time_in_force` (same test).
    pub accepted_tifs: &'static [TimeInForce],
    /// The words (`OrderType::as_str`) of the `OrderRequest.order_type` kinds this adapter WIRES
    /// with honored semantics, each the word of one [`OrderType::KNOWN`] kind (the vocabulary is the
    /// type: an [`OrderType::Unrecognized`] word is never a row's, and [`preflight_order`] refuses it on
    /// every venue). The TRIGGER kinds are the ones [`OrderType::is_trigger`] names (see [`TriggerType`]).
    /// A kind absent here is either coerced into
    /// something else today (the `_ => MARKET` fallthroughs — a resting "stop" silently becoming an
    /// immediate market order) or refused venue-/sidecar-side; the preflight turns both into a
    /// loud terminal refusal at the core edge. Like `supported_tifs`, the lane-split binance id
    /// declares the lane UNION ("stop" is spot's `STOP_LOSS`, built in the venue's own spot
    /// client, and the perp lane's `STOP_MARKET` on the Algo Service, decision 0186).
    ///
    /// Deribit's and okx's rows list `"trailing"` (since their drills) and none `"trailing_limit"`, so a
    /// kind a row does not list is `TRIGGER_UNSUPPORTED` there (`a_trailing_kind_is_refused_by_name_on_every_row`).
    /// Four bridges build one, each named on its row with the smoke that opens it: okx and hyperliquid a
    /// trailing stop, deribit a trailing stop with no activation, ibkr both. A row whose venue trails by a PERCENT
    /// (binance's perpetual, aster) or in basis points (binance spot) never lists either
    /// (`a_percent_trail_venue_never_lists_a_trailing_kind`): a vike trail is a price. Every other venue trails only an
    /// open position or a trade, with no activation, and builds neither.
    ///
    /// Only ibkr's row lists `"pegged"` (`a_pegged_order_is_refused_by_name_on_every_other_row`): its socket
    /// backend builds one (the `IBKR` row's doc), and the row opened behind its paper smoke (the mid anchor
    /// alone), and on that row only the anchors of [`Self::peg_anchors`]. A pegged order every other
    /// venue (or anchor) does not hold is EMULATED by the engine ([`emulation_of`]): one plain limit at a
    /// time, re-priced on every move of the anchor (`vike_execution::emulated`'s chase).
    ///
    /// Only ibkr's row lists `"trailing_take_profit"` (the attributes wave, IB `TRAIL MIT` / `TRAIL LIT`): its
    /// socket backend builds it, behind its paper smoke; no other venue has a trailing if-touched order.
    pub supported_order_kinds: &'static [&'static str],
    /// The words (`OrderType::as_str`) of the kinds this adapter PLACES when the request is
    /// `reduce_only`: a subset of [`Self::supported_order_kinds`] (pinned). [`preflight_order`]
    /// refuses a reduce-only order whose kind is absent here (`PreflightDeny::ReduceOnly`), at the
    /// core edge and in words, instead of letting it reach an adapter that refuses it by name
    /// afterwards. The reason it exists is the BRACKET: its exits (a reduce-only stop and a
    /// reduce-only limit) are held until the entry fills, so a refusal at the adapter comes only
    /// when the position is already open, and a refused stop leaves it naked. `lower_bracket`
    /// preflights all three legs before the entry is sent and refuses the whole bracket.
    ///
    /// Read each row as "what the adapter's `submit` does with the flag", measured from its code
    /// and not from the exchange: a venue whose adapter carries the flag onto the wire, routes it
    /// (FXCM binds the exit to a trade, cTrader to a position, IG closes by epic) or ignores it
    /// lists what it accepts today, so nothing that works is refused. ⚠ It is per VENUE and
    /// cannot say what depends on the account or the moment (FXCM's netting account has no
    /// per-trade exits, cTrader refuses an exit no open position can take, Deribit refuses a
    /// trigger order on an option, Dukascopy's stop needs open orders to protect): those stay
    /// refusals by the adapter, named on each row. Distinct from [`Self::supports_reduce_only`],
    /// which asks whether the adapter writes a reduce-only FLAG on the wire, and whose `false` is
    /// published for venues where a reduce-only order works.
    ///
    /// ⚠ It is the LIVE ADAPTER's refusals, so it judges the demo and live tiers only: a `paper`
    /// engine's orders reach the simulator, not that adapter, and `preflight_order_at` skips this
    /// axis for a stated `paper` tier (every other axis still applies).
    pub reduce_only_kinds: &'static [&'static str],
    /// Native trigger order kinds wired (see [`TriggerType`]); EMPTY = trigger orders
    /// unsupported. Invariant (pinned by tests): non-empty ⇔ `supported_order_kinds` contains a
    /// trigger kind (`"stop"`/`"stop_limit"`/`"take_profit"`).
    pub trigger_types: &'static [TriggerType],
    /// The adapter wires a post-only (maker-only) order: an `OrderRequest` whose `time_in_force` is
    /// [`TimeInForce::PostOnly`] goes out as the venue's own post-only order, and a crossing one is
    /// REFUSED by the venue, never executed. Always equal to "[`Self::supported_tifs`] lists
    /// `PostOnly`" (pinned by `post_only_is_the_supported_tifs_row`); a venue whose row says
    /// `false` has `PostOnly` refused BY NAME at the core edge (`PreflightDeny::IbapiTimeInForce`). TRUE for
    /// binance, bybit, okx, deribit and hyperliquid.
    pub supports_post_only: bool,
    /// The adapter wires [`vike_model::OrderRequest::display_quantity`] (an iceberg's visible slice) onto the
    /// venue's own field, so the venue shows only that much of the order. A venue whose row says
    /// `false` has a `display_quantity` order EMULATED by the engine (`crate::venue_caps::emulation_of`: one
    /// plain limit of the visible size at a time), never sent at its full visible size; where no
    /// emulation exists it is refused BY NAME at the core edge (`DISPLAY_QTY_UNSUPPORTED`). The venues
    /// that have the field: binance spot `icebergQty`, deribit `display_amount`, ibkr `displaySize`.
    pub supports_display_quantity: bool,
    /// The anchors of a `pegged` order the adapter builds natively ([`vike_model::OrderPegAnchor`]); a
    /// pegged order on another anchor, or on a row that does not list `pegged`, is EMULATED by the
    /// engine (`crate::venue_caps::emulation_of`). Empty on every row but ibkr's (`mid`, the one anchor
    /// its paper smoke proved).
    pub peg_anchors: &'static [vike_model::OrderPegAnchor],
    /// The ORDER ATTRIBUTE words (from [`ORDER_ATTRIBUTES`]) this adapter wires onto the venue's own
    /// fields: `all_or_none`, `sweep_to_fill`, `discretionary` ([`vike_model::OrderAttributes`]),
    /// `hidden` (a display of zero), `adjust` (a stop's [`vike_model::OrderStopAdjust`]) and `release` (the
    /// venue holds a TIME release itself, so the core sends a time-only release at once instead of
    /// holding it). A word absent here is refused BY NAME at the core edge
    /// (`ATTRIBUTE_UNSUPPORTED`), never sent without the attribute; `release` refuses nothing (the core
    /// holds the order instead). EMPTY on every row until a bridge builds a word behind its smoke.
    pub order_attributes: &'static [&'static str],
    /// The words (`OrderType::as_str`) of the TRAILING kinds this adapter places with a PERCENT trail
    /// ([`vike_model::OrderRequest::trail_percent`]). A request with a percent is judged against this
    /// list INSTEAD of [`Self::supported_order_kinds`], reduce-only included (a row lists a kind here
    /// only where its reduce-only exit works too), so opening a venue's percent trail never opens its
    /// price trail, and the reverse. EMPTY on every row until a bridge builds one behind its smoke.
    pub percent_trail_kinds: &'static [&'static str],
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
    /// `crates/bridges/okx/src/execution/rest/mod.rs`'s `swap_td_mode` builder by that module's
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
    /// with the asset, not with this row: `crates/bridges/hyperliquid/src/reconcile/mod.rs`'s
    /// `parse_positions` maps `leverage.type == "isolated"` → `MarginMode::Isolated`, so a
    /// reconcile pass over such a position reports Isolated.
    ///
    /// **That mismatch reaches nothing today**, and the reason is worth stating so a future reader
    /// need not re-derive it: `vike_execution::recon::diff` never compares margin mode (no `Divergence`
    /// variant is margin-shaped), so the difference cannot raise a divergence, an alert or a
    /// synthesized event; and outside this module's own tests — plus okx's adapter tie — nothing in
    /// the workspace reads this field at all. The live consumers of margin mode are per-POSITION
    /// (`vike_execution`'s gate margin fold and `margin_call`'s pool partition; `vike_core`'s
    /// liquidation-price badge) and they read `PositionEntry.margin_mode`, which the reconcile
    /// snapshot overwrites with venue truth. So this row misleads a READER, not code.
    ///
    /// The per-asset narrowing is DERIVED in one place rather than duplicated into a column here:
    /// `crates/bridges/hyperliquid/src/instrument/symbology.rs`'s `InstrumentRef::effective_margin_mode`
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
    /// Live market-data verbs served (see [`DataClientCapabilities`]).
    pub live_data: DataClientCapabilities,
    /// `vike-backfill` can PRODUCE historical OHLCV bars for this venue from the VENUE's own API —
    /// whether by fetching klines directly (every crypto venue) or by resampling ticks it fetched
    /// (dukascopy `.bi5` → `resample_quotes_to_bars`).
    ///
    /// **The definition is "can produce", NOT "the venue serves a native bar endpoint"** — settled
    /// deliberately (this field and `vike_backfill::capabilities` used to give opposite answers for
    /// dukascopy). A caller asking this field is asking "can I get bars for this venue without a
    /// vendor or an archive"; how the bytes arrive is the collector's business, and a resample is
    /// not a lesser bar. Read by `vike_backfill::capabilities::backfill_capabilities` as its venue-direct
    /// row (`BackfillProvider::Venue`), so that crate's existence and shape gates (a filesystem walk and a
    /// source scan over the real collector modules) judge this field.
    pub backfill_bars: bool,
    /// `vike-backfill` can backfill historical TICKS for this venue from the venue's own API — any
    /// of the three tick kinds (`quote`/`trade`/`book`). Dukascopy's keyless `.bi5` quote feed is
    /// the only venue-direct tick source on the roster today.
    ///
    /// Read by the same venue-direct row as `quote` (dukascopy's `.bi5`, the one venue-direct tick source, is a
    /// quote feed): a venue-direct trade or book collector needs this bit split per kind first.
    pub backfill_ticks: bool,
}

impl VenueCapabilities {
    /// The conservative all-unsupported row — the fallback for an unknown venue string and the
    /// default a UI uses before a venue is chosen. Nothing is offered until a venue proves it.
    pub const UNSUPPORTED: VenueCapabilities = VenueCapabilities {
        supports_modify: false,
        supports_native_batch: false,
        supports_reduce_only: false,
        supports_combo: false,
        supported_tifs: &[],
        accepted_tifs: &[],
        supported_order_kinds: &[],
        reduce_only_kinds: &[],
        trigger_types: &[],
        supports_post_only: false,
        supports_display_quantity: false,
        peg_anchors: &[],
        order_attributes: &[],
        percent_trail_kinds: &[],
        margin_modes: &[],
        default_margin_mode: MarginMode::Cross,
        max_batch: 0,
        live_data: DataClientCapabilities::NONE,
        backfill_bars: false,
        backfill_ticks: false,
    };

    /// The modify-gate decision: an in-place modify may be OFFERED (draggable resting order)
    /// and SENT (`Command::Modify`) only when the venue's adapter wires a native modify. The GUI
    /// calls this to refuse a marker drag (`vike-order-entry`'s Trade window ladder) AND to guard the
    /// command send (`vike-order-entry`'s `dispatch`) — the two halves the audit flagged.
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

impl Default for VenueCapabilities {
    fn default() -> Self {
        VenueCapabilities::UNSUPPORTED
    }
}
