//! `observe_decode` — the observe wire's REVERSE codec: one pushed [`crate::wire::CoreSnapshotWire`] back into
//! the [`vike_execution::CoreSnapshot`] every desktop panel renders.
//!
//! The forward projection (`CoreSnapshot` → `CoreSnapshotWire`) is `crates/vike-tradehub/src/publish.rs`'s
//! `project`. This is its inverse, beside the wire types it mirrors, so a wire field and its decode land in one
//! crate's tests. Charts render too: the daemon ships a bounded bar tail (`CoreSnapshotWire::bars`, ~300
//! closed bars and the forming candle per mounted series), which [`wire_to_core`] rebuilds into the core's bar cache so the
//! desktop's `sync_from_core` drives the candlestick engine exactly like a local core.
//!
//! The desktop's observe bridge (`crates/vike-app-core/src/backend/observe_bridge.rs`'s `spawn_bridge`) calls
//! [`wire_to_core`] on every new frame; the CLI and MCP read `CoreSnapshotWire` directly and never build a
//! `CoreSnapshot`. It moved here from `vike-app-core` (D56 of the top-layer plan, the owner's 2026-11-17
//! ruling), which is why this crate links `vike-execution`, with its default `runtime` feature off: the read
//! model is unconditional there, and the async runtime stays out of every thin client's tree.
//!
//! History: this mapping lived unexercised in the GUI shell's `main.rs` (`vike-app` then — the CI-excluded GUI
//! binary) until it moved down into `vike-app-core` so it finally ran in a gate — the forward projection was
//! CI-tested while this inverse was not, and [`map_order_status`]'s hand-maintained Debug-spelling fallback meant
//! a newly added `vike_model::OrderStatus` variant silently rendered as terminal `Rejected` in the observer. The
//! exhaustive round-trip test (no-wildcard `match` over every variant, `observe_decode_tests.rs`'s
//! `all_order_statuses`) turns that silent drift into a compile/test failure.

/// Read a wire order-status WORD as `vike_model::OrderStatus`, the one reading every surface shares (the desktop
/// through [`map_order_status`], the CLI's and MCP's verbs in `vike-client`, Telegram's read verbs). The wire
/// carries the status as a rendered string; `OrderStatus::parse` decodes the SCREAMING_SNAKE `as_str` spelling
/// ("ACCEPTED"), which is what the node sends (`crates/vike-tradehub/src/publish.rs`'s `project_order`). A node
/// built before that sent the Debug/PascalCase spelling ("Accepted"), and a client may observe an older deployed
/// node, so that spelling stays accepted: the canonical parser first, then the Debug spelling. `None` is a word
/// neither spells (a newer node's), and what it means is the caller's: the desktop shows it as a terminal, a
/// list of working orders keeps it listed.
pub fn order_status_of(word: &str) -> Option<vike_model::OrderStatus> {
    use vike_model::OrderStatus;
    if let Some(st) = OrderStatus::parse(word) {
        return Some(st);
    }
    Some(match word {
        "Initialized" => OrderStatus::Initialized,
        "Submitted" => OrderStatus::Submitted,
        "Accepted" => OrderStatus::Accepted,
        "Triggered" => OrderStatus::Triggered,
        "PartiallyFilled" => OrderStatus::PartiallyFilled,
        "Filled" => OrderStatus::Filled,
        "Canceled" => OrderStatus::Canceled,
        "Rejected" => OrderStatus::Rejected,
        "Denied" => OrderStatus::Denied,
        "Expired" => OrderStatus::Expired,
        "PendingCancel" => OrderStatus::PendingCancel,
        "Liquidated" => OrderStatus::Liquidated,
        "Emulated" => OrderStatus::Emulated,
        "Released" => OrderStatus::Released,
        _ => return None,
    })
}

/// Decode a wire order-status STRING back to `vike_model::OrderStatus` for the desktop's read model: the word as
/// [`order_status_of`] reads it, and only then warn and treat an unknown status as `Rejected` (a terminal, so an
/// unknown status never renders as live/actionable in the GUI).
pub fn map_order_status(s: &str) -> vike_model::OrderStatus {
    order_status_of(s).unwrap_or_else(|| {
        tracing::warn!(status = s, "observe: unknown wire order status; treating as Rejected");
        vike_model::OrderStatus::Rejected
    })
}

/// Wire position → core `PositionView`. The wire carries the display-relevant subset and the source that
/// priced it (`mark_source`: a word this build does not know, or none from a node that predates the key,
/// reads as `None`, not priced); the inert margin-mode carriers (`margin_mode`/`isolated_margin`) default
/// to their off-path values (`Cross`/`None`). The side is the node's
/// `PositionSide` `Display` word read back by the model's own reader, a word outside the three warned and
/// shown as the one-way `Both` leg rather than dropped: the row still carries its signed size.
pub fn map_position(p: &crate::wire::PositionViewWire) -> vike_execution::PositionView {
    use vike_model::events::PositionSide;
    let position_side = PositionSide::from_word_or_both(&p.position_side);
    // The reader folds every word but LONG and SHORT into Both; only the word BOTH is said.
    if position_side == PositionSide::Both && p.position_side != "BOTH" {
        tracing::warn!(
            position_side = p.position_side.as_str(),
            "observe: unknown wire position side; showing it as the one-way BOTH leg"
        );
    }
    vike_execution::PositionView {
        venue: p.venue.as_str().into(),
        symbol: p.symbol.as_str().into(),
        position_side,
        size: p.size,
        average_price: p.average_price,
        unrealized: p.unrealized,
        mark_source: p.mark_source.as_deref().and_then(vike_execution::PriceSource::from_word),
        leverage: p.leverage,
        liquidation_price: p.liquidation_price,
        margin_mode: vike_model::MarginMode::Cross,
        isolated_margin: None,
    }
}

/// Wire order → core `OrderView`.
///
/// ⚠ An order with no wire `account` maps to `account: None`, which `OrderView::account` reads as
/// "the venue's default account" and cannot spell as "unknown": a node that predates the key sends
/// its labelled orders without it, so they look like default-account orders, and the only tell that
/// a node names its orders' accounts is that its venue blocks carry a `tier` (`VenueBlockWire::tier`
/// ships only in node releases that also send the order's `account`).
///
/// So a reader that attributes orders to accounts has to check the tell first: on a node whose blocks
/// carry no `tier`, an order's account is UNKNOWN wherever its venue has more than one block, and
/// `None` there must not be read as the default account. The check belongs to the consumer — this
/// function sees one order at a time and the tell lives on the venue blocks.
pub fn map_order(o: &crate::wire::OrderViewWire) -> vike_execution::OrderView {
    vike_execution::OrderView {
        order_client_id: o.order_client_id.clone(),
        venue: o.venue.clone(),
        // The account the order rests in, as the node said it. A label this client cannot parse
        // maps to `None`, which this type spells exactly like the default account — so it must
        // never be ATTRIBUTED as one (M-9 of the final review, slice B). The engine it rests in
        // carries the same label in its block's route key, and
        // `crates/vike-order-entry/src/dispatch/mod.rs`'s `orders_name_their_account` refuses to attribute any
        // order of a venue whose route keys this client cannot read; the order stays listed.
        account: o
            .account
            .as_deref()
            .and_then(|a| vike_model::accounts::account_keys::parse_wire_account(a).ok())
            .filter(|l| !l.is_default()),
        symbol: o.symbol.clone(),
        side: o.side,
        qty: o.qty,
        order_type: vike_model::OrderType::from_stored(&o.order_type),
        price: o.price,
        trigger_price: o.trigger_price,
        status: map_order_status(&o.status),
        venue_order_id: o.venue_order_id.clone(),
        filled_qty: o.filled_qty,
        average_fill_price: o.average_fill_price,
        // The word a node sent; an unknown (newer) word reads as no label, never a guess.
        data_delay: o.data_delay.as_deref().and_then(vike_model::StreamDelay::from_word),
        reason: o.reason.clone(),
        refusal_id: o.refusal_id.clone(),
        // In-process only (`OrderView::pending_price`): the wire carries none.
        pending_price: None,
        trail: o.trail,
        limit_offset: o.limit_offset,
        // The resting order's readings (the wire carries the model's types); absent is `None`, and a node that
        // predates them sends none.
        attributes: o.attributes,
        trail_percent: o.trail_percent,
        adjust: o.adjust,
        release: o.release.as_ref().map(|r| model_release(r, &o.symbol, r.venue.as_deref())),
        display_quantity: o.display_quantity,
    }
}

/// A wire release as the model's: its price condition on the symbol the wire names, else on the order's own
/// (`own_symbol`), read on `venue` (`None`: the order's own venue).
fn model_release(
    r: &crate::wire::OrderReleaseWire,
    own_symbol: &str,
    venue: Option<&str>,
) -> vike_model::OrderRelease {
    use crate::wire::OrderReleasePriceWire;
    vike_model::OrderRelease {
        at: r.at,
        price: r.price.map(|p| {
            let (level, side) = match p {
                OrderReleasePriceWire::Above(level) => {
                    (level, vike_model::OrderReleasePriceSide::Above)
                }
                OrderReleasePriceWire::Below(level) => {
                    (level, vike_model::OrderReleasePriceSide::Below)
                }
            };
            let symbol = r.symbol.as_deref().unwrap_or(own_symbol);
            vike_model::OrderReleasePrice {
                symbol: symbol.into(),
                level,
                side,
                venue: venue.map(Into::into),
            }
        }),
    }
}

/// Wire held order (a bracket exit, or an order held until its release) → core `OrderHeldView` (a `vike_execution`
/// crate-root type like the rest of the read model). The account as [`map_order`] reads it; the release's price
/// condition on the symbol the wire names, else on the order's own, and on the venue the row's
/// `release_venue` reading names (a held row's venue rides beside its release, never inside it), else the
/// order's own; its terms as [`map_order`] reads a resting order's.
pub fn map_held(h: &crate::wire::OrderHeldViewWire) -> vike_execution::OrderHeldView {
    let release =
        h.release.as_ref().map(|r| model_release(r, &h.symbol, h.release_venue.as_deref()));
    vike_execution::OrderHeldView {
        order_client_id: h.order_client_id.clone(),
        venue: h.venue.clone(),
        symbol: h.symbol.clone(),
        side: h.side,
        qty: h.qty,
        order_type: vike_model::OrderType::from_stored(&h.order_type),
        price: h.price,
        trigger_price: h.trigger_price,
        parent_order_id: h.parent_order_id.clone(),
        account: h
            .account
            .as_deref()
            .and_then(|a| vike_model::accounts::account_keys::parse_wire_account(a).ok())
            .filter(|l| !l.is_default()),
        release,
        waiting: h.waiting.clone(),
        attributes: h.attributes,
        trail_percent: h.trail_percent,
        adjust: h.adjust,
        display_quantity: h.display_quantity,
    }
}

/// Wire per-venue block → core `VenueBlock`. The heavy internals stay core-side, so the
/// GUI-irrelevant fields default: `balance_mode` = `Delta` (matching `CoreSnapshot::empty`),
/// `fee_schedule` = `None`, an empty multiplier grid with a 1.0 default; `margin_ratio` is
/// recomputed from the wired `margin_used`/`equity`.
pub fn map_venue_block(v: &crate::wire::VenueBlockWire) -> vike_execution::VenueBlock {
    vike_execution::VenueBlock {
        // What the GUI's mint stamps for this account (decision 0270). A code or a shape this build
        // cannot read is "not said": the mint then stamps `00` and the full shape.
        account_code: v
            .account_code
            .as_deref()
            .and_then(vike_model::orders::client_id::OrderClientIdAccountCode::parse),
        // A shape the model cannot mint (`OrderClientIdShape::is_mintable`) is not said: `OrderClientIdPlan::new` panics on it,
        // so a node's word must not reach it unchecked.
        order_client_id_shape: v.order_client_id_shape.filter(|s| s.is_mintable()),
        venue: v.venue.clone(),
        // WHICH ACCOUNT this block is, as the node said it (`VenueBlockWire::account`). The wire
        // gained the label and the route key in the account-routing stage; this bridge dropped
        // both until the Trade window needed them, so a two-account node rendered as two blocks
        // claiming to be their venue's sole account. A malformed label is dropped rather than
        // guessed at: it reads as the default account, which is what an older node sends anyway.
        account: v
            .account
            .as_deref()
            .and_then(|a| vike_model::accounts::account_keys::parse_wire_account(a).ok())
            .filter(|l| !l.is_default()),
        // An older node sends no key; the bare venue is exactly what it would have sent for its
        // single account.
        route_key: if v.route_key.is_empty() { v.venue.clone() } else { v.route_key.clone() },
        // What the account's engine trades, primary first, and its tier. Empty and `None` both
        // mean "not said" (an older node): an absent list stays an empty `symbol`
        // (`VenueBlock::trades` answers `false` for every symbol), never one invented from the
        // snapshot's own primary market. The tier rides as its WORD (decision 0091 §3); THIS is
        // the edge where it becomes a tier, through the one strict parser (decision 0129), and a
        // word it refuses is "not said" — drawn `MODE ?`, never PAPER.
        symbol: v.symbols.first().cloned().unwrap_or_default(),
        extra_symbols: v.symbols.iter().skip(1).cloned().collect(),
        tier: map_tier(v.tier.as_deref()),
        balance: v.balance,
        realized_pnl: v.realized_pnl,
        fees_paid: v.fees_paid,
        funding_paid: v.funding_paid,
        balance_mode: vike_execution::BalanceMode::Delta,
        equity: v.equity,
        unrealized: v.unrealized,
        missing_prices: v.missing_prices,
        margin_used: v.margin_used,
        free_buying_power: v.free_buying_power,
        margin_ratio: if v.equity > 0.0 { v.margin_used / v.equity } else { 0.0 },
        fee_schedule: None,
        trading_state: v.trading_state,
        // The node's non-default rows (decision 0170), so the ticket sizes like the node's gate. A
        // kind this build does not know is DROPPED (the row reads the default), never guessed.
        contract_values: std::sync::Arc::new(
            v.contract_values
                .iter()
                .filter_map(|row| {
                    let kind = vike_model::NotionalKind::from_word(&row.kind)?;
                    Some((
                        row.symbol.clone(),
                        vike_model::ContractValue { kind, per_unit: row.per_unit }.folded(),
                    ))
                })
                .collect(),
        ),
        contract_value_default: vike_model::ContractValue::default(),
        // The node's capability, read verbatim; an older node's absent key is `false`.
        admits_symbols: v.admits_symbols,
        // The node's conversion-rate table (decision 0183, phase 1), read verbatim: an absent key is an
        // empty table, which is every account without a symbol quoted in another currency. A rate that is
        // not a positive number is shown as STALE (never folded to a usable one the way a degenerate
        // contract value is above): an observer that cannot trust a rate must say so, not hide it.
        quote_rates: v
            .quote_rates
            .iter()
            .map(|r| vike_execution::QuoteRateView {
                currency: vike_execution::Interned::from(r.currency.as_str()),
                rate: r.rate,
                as_of_ms: r.as_of_ms,
                stale: r.stale || !(r.rate.is_finite() && r.rate > 0.0),
                held: r.held,
            })
            .collect(),
        quote_rates_in: vike_execution::Interned::from(v.quote_rates_in.as_str()),
        // The node's currency group (decision 0183, item 13), read verbatim: an absent key is the
        // dollar group, which is what a node that predates the field had.
        ledger_currency: vike_execution::Interned::from(v.ledger_currency.as_str()),
        // The node's word that the ledger currency is only an assumed USD, read verbatim (absent = false).
        currency_assumed: v.currency_assumed,
        colliding_fills_refused: 0,
        fees_unconverted: 0,
        fees_at_stale_rate: 0,
        positions: v.positions.iter().map(map_position).collect(),
    }
}

/// Wire portfolio slice → core `Portfolio`. `equity_total` is the wire's cross-venue aggregate;
/// the scalar fields (`equity`/`realized_pnl`/`fees_paid`/`funding_paid`) mirror the PRIMARY venue
/// (`venues[0]`), exactly as `vike_core::snapshot::build` populates them (NOT summed); `margin_used_total`
/// and `missing_prices_total` ARE cross-venue sums. `margin_used_total` adds the PRIMARY currency group
/// only, as `vike_core::snapshot::build` does and as the wire's `equity_total` is; the other groups are
/// `Portfolio::ledger_totals`, folded from the blocks.
///
/// ⚠ The primary group is the key the node STATES (`CoreSnapshotWire::primary_currency`), never the first block's:
/// the frame carries the newest block of each account, so after the primary account restarted the first
/// block is another engine's and may be in another currency. The portfolio keeps the stated key
/// (`Portfolio::primary_currency_stated`) so every total and report built on it agrees with the node's.
pub fn build_portfolio(w: &crate::wire::CoreSnapshotWire) -> vike_execution::Portfolio {
    let primary = w.venues.first();
    let primary_currency = w.primary_currency.as_str();
    vike_execution::Portfolio {
        equity: primary.map(|v| v.equity).unwrap_or(0.0),
        equity_total: w.equity_total,
        realized_pnl: primary.map(|v| v.realized_pnl).unwrap_or(0.0),
        fees_paid: primary.map(|v| v.fees_paid).unwrap_or(0.0),
        funding_paid: primary.map(|v| v.funding_paid).unwrap_or(0.0),
        // Naive fold, NOT `.sum()`: std's float `Sum` empty identity is `-0.0`, so an EMPTY
        // venues list (the pre-first-frame `CoreSnapshotWire::empty()` "connecting" state) would
        // yield a `-0.0` here where `CoreSnapshot::empty`'s `Portfolio::default()` holds `+0.0`.
        // The fold starts at `+0.0` (bit-identical to `.sum()` for every non-empty list —
        // `margin_used` is never `-0.0`), keeping the observer's placeholder bit-identical to
        // the local core's. (`vike_core::snapshot::build`'s own `.sum()` site never sees an empty list:
        // it always has at least the primary engine's venue.)
        margin_used_total: w
            .venues
            .iter()
            .filter(|v| v.ledger_currency == primary_currency)
            .fold(0.0, |accumulator, v| accumulator + v.margin_used),
        // 0.0 = UNKNOWN, deliberately. `VenueBlockWire` carries no per-engine `seed_cash` and
        // `CoreSnapshotWire` carries no total, so there is nothing honest to map here; inventing one
        // would let `Portfolio::drawdown_curve` read as a real capital base on the observe path.
        // Nothing downstream of this decode divides by it: the drawdown latch runs in the REMOTE
        // core (against its own seeds) and `vike_alerting`'s `RuleTrigger::Drawdown` is mounted
        // only by `vike-tradehub`, which builds its snapshots locally via `vike_core::snapshot::build`.
        // The observe client is a read-only GUI. If a wire seed is ever added, map it here.
        capital_base: 0.0,
        missing_prices_total: w.venues.iter().map(|v| v.missing_prices).sum(),
        balances_by_asset: Vec::new(),
        venues: w.venues.iter().map(map_venue_block).collect(),
        primary_currency_stated: Some(vike_execution::Interned::from(primary_currency)),
        // The wire carries no group curve: the push alert that reads them is mounted only by `vike-tradehub`,
        // which builds its snapshots locally, never through this decode.
        group_curves: Vec::new(),
    }
}

/// A wire tier WORD back to the one tier type (`vike_model::AccountTier`'s strict parser, decision
/// 0129). Absent, or a word this build does not know, is `None` — "the node did not say" — never
/// PAPER.
pub fn map_tier(word: Option<&str>) -> Option<vike_model::AccountTier> {
    word.and_then(|w| w.parse().ok())
}

/// One account the node says it does NOT run, and why (decision 0156), back into the core's read
/// model. The reason is the row's one wording ([`crate::wire::ArmingNotRunningViewWire::say`]): the node's word
/// under its id, with what lies behind it where the node says (`adapter
/// failed` covers a lease another client holds, a version skew and a refused key alike). A venue this
/// client's roster does not have, or a label it cannot parse (a newer node), is kept as received
/// (`AccountNamed::Unknown`, owner ruling 2026-11-12), never dropped: the row is still shown, under
/// the route key its venue and label render, which is the wire's own `route_key`.
pub fn map_not_running(
    n: &crate::wire::ArmingNotRunningViewWire,
) -> vike_execution::ArmingNotRunningView {
    vike_execution::ArmingNotRunningView {
        account: vike_model::accounts::account_keys::AccountNamed::from_wire(
            &n.venue,
            n.account.as_deref(),
        ),
        tier: map_tier(n.tier.as_deref()),
        reason: n.say(),
        // Already typed by the wire's reader: a word this build does not know arrives as `AdapterFailed`.
        block: n.block_word,
        // `say` put the id in `reason`, so it is shown once.
        refusal_id: None,
    }
}

/// The kind words a node lists an emulated order under (`vike_execution::OrderEmulationView::kind`, a `&'static str`).
const EMULATED_KINDS: [&str; 8] = [
    "stop",
    "trailing",
    "take_profit",
    "stop_limit",
    "trailing_limit",
    "trailing_take_profit",
    "chase",
    "iceberg",
];

/// Decode one emulated order off the wire (decision 0275). The account as [`map_order`] reads it; a peg anchor this
/// build cannot read drops the peg, never the row.
///
/// shortcut: a kind word outside [`EMULATED_KINDS`] (a newer node's) is shown as `emulated`, since the view holds a
/// `&'static str`; add the word here when a kind is added.
pub fn map_emulated(e: &crate::wire::OrderEmulationViewWire) -> vike_execution::OrderEmulationView {
    vike_execution::OrderEmulationView {
        id: e.id.clone(),
        venue: e.venue.clone(),
        account: e
            .account
            .as_deref()
            .and_then(|a| vike_model::accounts::account_keys::parse_wire_account(a).ok())
            .filter(|l| !l.is_default()),
        symbol: e.symbol.clone(),
        side: e.side,
        qty: e.qty,
        kind: EMULATED_KINDS.into_iter().find(|k| *k == e.kind).unwrap_or("emulated"),
        reduce_only: e.reduce_only,
        trigger: e.trigger,
        price: e.price,
        trail: e.trail,
        peg: e.peg.as_ref().and_then(|p| {
            vike_model::OrderPegAnchor::parse(&p.anchor)
                .map(|anchor| vike_model::OrderPeg { anchor, offset: p.offset })
        }),
        display_quantity: e.display_quantity,
        filled: e.filled,
        average_price: e.average_price,
        child: e.child.clone(),
        waiting: e.waiting.clone(),
        rearms: e.rearms,
        held: e.held,
    }
}

/// The frame's counters block as the read model's [`vike_execution::CoreCounters`] (whose `named` gives each its
/// published name). The inverse of `crates/vike-tradehub/src/publish.rs`'s `project_counters`.
pub fn map_counters(w: &crate::wire::CoreCountersWire) -> vike_execution::CoreCounters {
    let crate::wire::CoreCountersWire {
        market_updates_dropped_conflated,
        commands_rejected_queue_full,
        events_dropped_order_already_closed,
        closing_events_dropped_order_still_live,
        events_dropped_order_unknown,
        events_dropped_number_not_finite,
        restored_orders_adopted,
        restored_orders_gone,
        restored_orders_filled_while_down,
        tags_repointed_order_still_live,
    } = *w;
    vike_execution::CoreCounters {
        market_updates_dropped_conflated,
        commands_rejected_queue_full,
        events_dropped_order_already_closed,
        closing_events_dropped_order_still_live,
        events_dropped_order_unknown,
        events_dropped_number_not_finite,
        restored_orders_adopted,
        restored_orders_gone,
        restored_orders_filled_while_down,
        tags_repointed_order_still_live,
    }
}

/// Decode one remote `CoreSnapshotWire` into the `CoreSnapshot` the GUI panels already render (see the
/// module comment above). READ-ONLY: no local core; `bars` are rebuilt from the wire's bounded tail
/// (charts render), `marks`/`mounts`/`recon` empty.
pub fn wire_to_core(w: &crate::wire::CoreSnapshotWire) -> vike_execution::CoreSnapshot {
    vike_execution::CoreSnapshot {
        sequence: w.sequence,
        venue: w.venue.clone(),
        symbol: w.symbol.clone(),
        trading_state: w.trading_state,
        balance: w.balance,
        balance_mode: vike_execution::BalanceMode::Delta,
        portfolio: build_portfolio(w),
        positions: w.positions.iter().map(map_position).collect(),
        marks: Vec::new(),
        orders: w.orders.iter().map(|o| std::sync::Arc::new(map_order(o))).collect(),
        held_exits: w.held_exits.iter().map(map_held).collect(),
        // The node's armed core-held stops, mapped as the wire still carries them; NO view renders this list: the
        // Account window draws the core-held stops as rows of `emulated_orders` below, so each is shown once.
        core_held_stops: w
            .core_held_stops
            .iter()
            .map(|s| vike_execution::OrderEmulationStopView {
                arm_id: s.arm_id.clone(),
                venue: s.venue.clone(),
                symbol: s.symbol.clone(),
                route_key: s.account.clone(),
                side: s.side,
                qty: s.qty,
                trigger_price: s.trigger_price,
                held: s.held,
                rearms: s.rearms,
            })
            .collect(),
        // Every order the node emulates, one list (decision 0275): the Account window lists them with their kind.
        emulated_orders: w.emulated_orders.iter().map(map_emulated).collect(),
        // Rebuild the bar cache from the wire so the observer chart renders (the node ships a bounded
        // last-K tail + the forming candle per mounted series). `sync_from_core` consumes this exactly
        // like the local GUI's live bars — no observer-specific render code.
        bars: w
            .bars
            .iter()
            .map(|s| {
                let to_bar = |b: &crate::wire::BarWire| vike_marketdata::Bar {
                    timestamp: b.timestamp,
                    open: b.o,
                    high: b.h,
                    low: b.l,
                    close: b.c,
                    volume: b.v,
                    funding: None,
                    bid: None,
                    ask: None,
                    symbol: Some(s.symbol.as_str().into()),
                };
                (
                    (s.venue.clone(), s.symbol.clone(), s.interval.clone()),
                    vike_execution::BarSeries {
                        closed: std::sync::Arc::new(s.closed.iter().map(&to_bar).collect()),
                        forming: s.forming.as_ref().map(&to_bar),
                    },
                )
            })
            .collect(),
        mounts: std::sync::Arc::new([]),
        // wire stays Vec<String>; the in-process snapshot shares Arc<str>
        // (`crates/vike-core/src/runtime/journaling.rs`'s `push_line`)
        recent_events: w.recent_events.iter().map(|s| s.as_str().into()).collect(),
        // In-process only: the node's alert mount reads its own; the wire carries none.
        denied_orders: None,
        fault: w.fault.clone(),
        // In-process only, as `denied_orders`.
        journal_fence_halt: false,
        // A node that predates the block decodes as all zero: a reader that must tell "not said" from zero reads
        // the frame (`CoreSnapshotWire::counters`), as `trade health` does.
        counters: w.counters.as_ref().map(map_counters).unwrap_or_default(),
        reconcile: vike_execution::ReconcileBlock::default(),
        // Empty, spelled through `Default` so this crate takes no `indexmap` edge of its own.
        reconcile_coin_deltas: Default::default(),
        // `0` = "the node has not said", the same meaning `CoreSnapshot::empty` gives it. The
        // node's account-set digest does ride the wire (`CoreSnapshotWire::accounts_epoch`), but this
        // decode does not forward it, so an observer's snapshot says `0` whatever the node mounts:
        // a reader that needs the digest takes it from the wire frame, never from this snapshot.
        accounts_epoch: 0,
        // The accounts the node does not run, each with why (decision 0156): what the Trade
        // window says for one instead of a bare "not running".
        not_running: w.not_running.iter().map(map_not_running).collect(),
    }
}

#[path = "observe_decode_tests.rs"]
#[cfg(test)]
mod observe_decode_tests;
