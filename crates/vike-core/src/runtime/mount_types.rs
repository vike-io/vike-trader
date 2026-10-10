//! Strategy-mount data: `StrategyMount`, `MountLeg`, the buffered broker intents, per-mount
//! attribution and readiness. The per-mount budget a caller hands in, `vike_exec::MountBudget`, is
//! read-model data and lives in vike-exec.

use super::*;

/// One buffered order intent from a strategy handler, drained after the handler returns. `tag` is
/// a strategy-chosen stable id (HFT modify surface) — `None` for the portable [`Broker`] verbs.
///
/// `pub` (with the sibling `Buffered*` intents + [`LiveBroker`]'s buffer fields below) so the
/// `vike-mm` maker crate's white-box tests can build a `LiveBroker` and read back the verbs the
/// generic `SpreadMaker` pushed — a TEST-only surface (vike-mm dev-deps vike-core); the live path
/// still populates/drains these internally exactly as before.
pub struct BufferedSubmit {
    /// The symbol the STRATEGY named, when it named one. `None` (every inherent verb —
    /// `order_target`, `set_holdings`, the tagged HFT lane) means "this mount's own symbol".
    ///
    /// It is recorded UNCONDITIONALLY by the `Broker` verbs and resolved at DRAIN time against
    /// the mount's [`StrategyMount::symbols`] declaration: an undeclared mount ignores it exactly
    /// as before. Recording here rather than deciding in the broker is what keeps all nine
    /// `LiveBroker` construction sites untouched.
    pub symbol: Option<String>,
    pub side: i32,
    pub qty: f64,
    pub order_type: String,
    pub price: Option<f64>,
    pub reduce_only: bool,
    pub tag: Option<String>,
}

/// One buffered modify-by-tag intent (RUST-NATIVE HFT surface; resolved to a coid at drain time).
pub struct BufferedModify {
    pub tag: String,
    pub new_qty: Option<f64>,
    pub new_price: Option<f64>,
}

/// One buffered bracket intent (entry + SL + TP). venue/symbol come from the mount at drain time;
/// the three coids are minted then, so id minting stays in the runtime's one place.
pub struct BufferedBracket {
    /// See [`BufferedSubmit::symbol`]. `None` = the mount's own symbol.
    pub symbol: Option<String>,
    pub side: i32,
    pub qty: f64,
    pub entry_price: Option<f64>,
    pub stop_loss: f64,
    pub take_profit: f64,
}

/// One buffered conditional-order intent (stop / trailing) for the core-owned
/// [`crate::emulator::ConditionalBook`] — port of `exec/conditionals.py` registration:
/// ARMING bypasses the gate (only the FIRE goes mint → RiskGate → client). Trailing
/// extremes seed from the current mark at drain time (refused without a mark, like
/// `live_portfolio_engine.py::submit_trailing`).
pub struct BufferedConditional {
    /// See [`BufferedSubmit::symbol`]. `None` = the mount's own symbol.
    pub symbol: Option<String>,
    pub side: i32,
    pub qty: f64,
    /// Some = fixed stop trigger price
    pub price: Option<f64>,
    /// Some = trailing distance
    pub trail: Option<f64>,
}

/// One EXTRA leg a mount may trade beyond its own `(venue, symbol)` — see
/// [`StrategyMount::symbols`].
///
/// `venue: None` (the common case) means the mount's OWN venue: the leg is a second instrument
/// on the same exchange. `Some(v)` routes it to a DIFFERENT venue, which is what a cross-exchange
/// strategy needs — an xEMM maker rests on one venue and hedges on another, so its two legs
/// cannot share a venue by construction.
///
/// The shape mirrors `vike_strategy::ControllerHarness`'s `venue_map` (`symbol -> venue`, empty
/// = single-venue): the same problem was already solved once for the funding-carry controller,
/// and two different answers to "which venue does this symbol trade on" would be a bug waiting
/// to happen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MountLeg {
    pub symbol: String,
    /// `None` = the mount's own venue.
    pub venue: Option<String>,
}

impl MountLeg {
    /// A leg on the mount's OWN venue.
    pub fn same_venue(symbol: impl Into<String>) -> Self {
        MountLeg { symbol: symbol.into(), venue: None }
    }

    /// A leg on a DIFFERENT venue — the cross-exchange case.
    pub fn at(symbol: impl Into<String>, venue: impl Into<String>) -> Self {
        MountLeg { symbol: symbol.into(), venue: Some(venue.into()) }
    }
}

/// A strategy mounted on one (venue, symbol, interval) bar series.
pub struct StrategyMount {
    pub venue: String,
    pub symbol: String,
    pub interval: String,
    /// **WHICH ACCOUNT of [`Self::venue`] this mount trades on.** `None` — every mount that existed
    /// before this field — is the venue's DEFAULT account, whose route key IS the bare venue id, so
    /// [`assemble_core`] resolves the identical engine index the venue lookup resolved and nothing
    /// about a single-account process changes.
    ///
    /// `Some(label)` is resolved ONCE, at assembly, into [`CoreThread::mount_engine`] — an INDEX —
    /// and every lane that used to ask "which engine does this venue belong to" asks that vector
    /// instead. Two properties follow, and both are the point:
    ///
    /// * the account never touches a venue STRING. Decorating `venue` (the `"binance#ALT"` trap
    ///   `vike_exec::route_key`'s module doc names) would route correctly and then silently break
    ///   `ExecutionEngine::apply_snapshot`'s reap filter, `resolved_position_price`'s position
    ///   lookup and `local_view`'s filter, all of which compare against the engine's CANONICAL
    ///   venue — while fills arrive from a bridge that carries the canonical id and cannot know
    ///   accounts exist;
    /// * the mount's READS move with its writes. `Broker::position`/`equity`/`multiplier`/
    ///   `lot_size` resolve through the same `mount_engine` index, so a mount cannot trade one book
    ///   while sizing against another — which is a worse defect than the one this field fixes and
    ///   one no order-routing test would catch.
    ///
    /// ⚠ It is deliberately NOT part of the derived mount ID (`crate::strategy_state::mount_id_with`):
    /// the id is a state-sidecar FILENAME and the journal attribution key, so folding the account in
    /// would rename every existing sidecar. Two mounts differing only by account therefore collide
    /// on the id and are refused by the existing duplicate-id rule, which correctly tells the
    /// operator to name a `controller_id`.
    pub account: Option<vike_model::accounts::account_keys::AccountLabel>,
    pub strategy: Box<dyn Strategy<LiveBroker> + Send>,
    /// Opt-in ADDITIONAL symbols (same `venue`) this mount may trade and read — the declaration
    /// that turns the otherwise-IGNORED `symbol` argument of the [`vike_model::Broker`] order verbs
    /// into an AUTHORITATIVE one for this mount.
    ///
    /// EMPTY — every mount that exists today — keeps the single-symbol contract exactly:
    /// `drain_broker` stamps the mount's own `(venue, symbol)` onto every intent and the verbs'
    /// `symbol` argument is ignored, byte-identically to before this field existed.
    ///
    /// **Why a declaration is required rather than simply honouring the argument.** Shipped
    /// strategies pass a symbol they never expect to be used: live bars carry `Bar::symbol == None`
    /// (`vike_bridge_core::klines::kline_to_bar`), so a strategy doing
    /// `bar.symbol.clone().unwrap_or_default()` passes `""` — `crates/vike-sim/tests/r7_gate.rs`,
    /// `crates/vike-core/src/runtime/tests/safe_state/mount_budget.rs`'s `BudgetSubmit` and
    /// `vike-script`'s live bar lane all say so explicitly.
    /// Honouring the argument unconditionally would send `symbol: ""` to venues. The MOUNT, not the
    /// call site, is therefore where the intent has to be expressed.
    ///
    /// It exists because a TWO-LEG strategy (a pairs trade, an xEMM hedge) cannot be split across
    /// two mounts: a mount OWNS its strategy (`Box<dyn Strategy>`, not `Arc`), so two mounts hold
    /// two different objects with two different states and there is no shared-state seam. ONE mount
    /// emitting orders for more than one symbol is the only available shape.
    ///
    /// WHAT READS IT TODAY (it is no longer inert — an earlier revision of this doc said it was,
    /// and that has been stale since #916/#924/#997):
    /// - `drain_broker` → `resolve_intent_symbol` (a declared symbol on an order verb is honoured;
    ///   an UNDECLARED one is refused, never rewritten) and `resolve_intent_venue` (a
    ///   [`MountLeg::at`] leg's orders route to THAT venue's engine);
    /// - `declared_views` (the per-symbol `Broker::position`/`price` tables — which carry every
    ///   declared instrument plus the mount's OWN symbol, minus whichever series is DISPATCHING,
    ///   because the ctx scalars are that series' own and fresher answer; built on EVERY
    ///   strategy-hook lane, and each row read out of that leg's OWN venue's engine — plus the
    ///   `Broker::bars` table, which carries the dispatching symbol TOO and is therefore what
    ///   `LiveBroker::carries` reads to tell an uncarried symbol from a dispatching one);
    /// - the BAR and TICK audiences (`Audience::Bar`, `Audience::Tick`, read by
    ///   [`CoreThread::drive_strategy`] and [`CoreThread::drive_strategy_tick`]) — a declared
    ///   symbol's bars/ticks on the mount's own venue reach the mount, and live bars now carry their
    ///   series symbol so the legs are distinguishable;
    /// - [`CoreThread::drive_strategy_reference_quote`] — a leg declared on a DIFFERENT venue has
    ///   that venue's L1 touch delivered to [`Strategy::on_reference_quote`] (the xEMM lane).
    ///
    /// ⚠ STILL NOT ROUTED for a leg on a DIFFERENT venue: its BARS (the bar lane additionally
    /// requires that venue's engine to `accepts_symbol` the leg), its FEED STATUS (`FeedStatus`
    /// carries neither venue nor symbol) and its L2 BOOK (`L2Book` carries neither, so it cannot be
    /// attributed — its derived L1 is delivered instead).
    ///
    /// ⚠ ALL FOUR read gaps `docs/superpowers/specs/2026-08-07-multi-symbol-read-half.md` listed are
    /// CLOSED. The empty per-symbol tables on the non-dispatch lanes, the mount-venue resolution of a
    /// cross-venue leg and the drain-keyed tag registry went first
    /// (`crates/vike-core/tests/wiring/multi_symbol_reads.rs` is the regression proof for those
    /// three); `LiveBroker::bars` — which
    /// ignored its `symbol` argument on EVERY lane and returned the dispatching series — went with
    /// `LiveBroker::bar_views`, pinned by `crates/vike-sim/tests/multi_symbol_read_parity.rs`'s
    /// `live_bars_are_symbol_addressed`. A symbol a DECLARED mount does not carry now reads EMPTY
    /// (`0.0` / `0.0` / `&[]`) instead of the dispatching series' numbers, which is the read-side
    /// mirror of `resolve_intent_symbol` REFUSING that same symbol on the write side.
    pub symbols: Vec<MountLeg>,
    /// Optional CROSS-SYMBOL underlying/reference series this mount WATCHES ("Option B" routing): a
    /// DIFFERENT `symbol` (same `venue`) whose marks the strategy anchors on via [`Strategy::on_mark`]
    /// — e.g. the `btcusdt` RTDS spot a Polymarket BTC up/down maker blends its fair mid toward. When
    /// `Some`, the runtime routes that symbol's drained marks to this mount's `on_mark`, building the
    /// broker ctx on the mount's OWN (venue, symbol). `None` (the default for every existing mount) ⇒
    /// no mark is ever routed here and [`CoreThread::drive_strategy_mark`] early-returns, so a run
    /// with no underlying-anchored mount is byte-identical.
    pub underlying_symbol: Option<String>,
    /// Opt-in explicit CONTROLLER ID naming THIS mount (multi-mount correctness, gap C). When
    /// `Some`, it IS the mount id ([`crate::strategy_state::mount_id_with`], sanitized to a safe
    /// filename segment); `None` — every mount that exists today — keeps the legacy
    /// `{venue}__{symbol}__{interval}` derivation, so its state sidecar, its journal `mount_id`
    /// provenance and its budget/schedule keys are all byte-identical to before this field existed.
    ///
    /// It exists because the derived identity is NOT unique: two strategies mounted on the SAME
    /// `(venue, symbol, interval)` collide onto one state sidecar file and one journal `mount_id`.
    /// [`assemble_core`] asserts mount-id uniqueness and PANICS at mount time on a duplicate rather
    /// than silently sharing state, so a same-triple pair must name its mounts here.
    ///
    /// **The identity lives on the MOUNT, not beside it.** An earlier shape kept these ids in a
    /// POSITIONAL `CoreConfig` vec indexed by assembly order; reordering [`CoreConfig::extra_mounts`]
    /// then silently reassigned every id, so a strategy would cross-load a SIBLING's durable state on
    /// the next start (a mount id is both a sidecar FILENAME and the journal attribution key). A
    /// field on the mount cannot drift that way — see
    /// `multi_mount_tests::reordering_extra_mounts_preserves_each_mount_id`.
    pub controller_id: Option<String>,
}

/// Per-mount fill ATTRIBUTION ledger (steal/core-per-mount-budget) — one per mount slot, folded
/// from that mount's OWN attributed fills (coid -> mount via `CoreThread::coid_mount`) at fill
/// cadence in `dispatch_applied_fills`, NEVER the per-message hot fold. Pure numbers (Copy) so the
/// per-closed-bar budget sweep and the publish-time view can snapshot it cheaply; the (venue,
/// symbol) it prices against comes from the parallel `CoreThread::mount_vs`, not from here. The
/// position/avg-px fold mirrors `Account::fold` exactly (both call `vike_model::compute_fill`), so
/// a mount that trades ONE symbol tracks the same weighted-average-cost position the account does.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub(crate) struct MountAttribution {
    /// signed net position folded from this mount's attributed fills
    pub(crate) size: f64,
    /// weighted-average entry price of the folded position (0.0 while flat)
    pub(crate) avg_px: f64,
    /// cumulative GROSS realized price PnL on this mount's closed portions
    pub(crate) realized_pnl: f64,
    /// cumulative commission/fees on this mount's attributed fills (signed; a maker rebate is < 0)
    pub(crate) fees_paid: f64,
}

/// Per-mount readiness (portfolio-observer PR-4 T5) — one entry in `CoreThread::mount_states`,
/// parallel to `CoreThread::mounts` (same index). A mount starts `Pending` only when
/// [`CoreConfig::readiness_gate`] is on ([`assemble_core`] seeds every slot `Ready` otherwise, so
/// the gate-off path never allocates a `Pending` state at all). `Pending` still receives every
/// strategy-hook call ([`CoreThread::drive_strategy`] / `drive_strategy_tick` / ... all still
/// call the hook) — only the ORDER OUTPUT is gated, at [`CoreThread::drain_broker`]. The boundary
/// probe ([`CoreThread::maintain_mount_readiness`]) is the ONLY place a mount ever flips
/// `Pending -> Ready`; nothing ever flips it back (a mount that has traded once stays eligible).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MountState {
    /// Not yet priced: the mount's (venue, symbol) has never resolved a bid OR ask/mark/trade/bar
    /// through the `PriceBoard`. Buffered order intents from this mount's hook calls are dropped
    /// at `drain_broker` — the strategy trades on paper-thin air otherwise (no venue-informed
    /// price to size/cross against), so nothing is submitted until there is one.
    Pending,
    /// Priced at least once (or the gate is off, in which case every mount starts here and stays
    /// here): `drain_broker` drains this mount's buffered intents to the engine normally.
    Ready,
}
