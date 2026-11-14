//! `observe_bridge` — the `--observe` thin-client's `CoreSnapshotWire` → `CoreSnapshot` REVERSE bridge.
//!
//! The read-only observer — the desktop shell, in EVERY launch since it lost its local core
//! (`crates/vike-desktop/src/app_setup.rs`'s `App::new`; it was `vike-app --observe <ADDR>`, one mode of
//! three, when this was written) — has NO local trading core. It connects to a headless
//! `vike-tradehub` daemon's observe server under `Scope::Read` and republishes each pushed
//! [`vike_tradehub_client::wire::CoreSnapshotWire`] into a GUI-owned arc-swap cell as a
//! [`vike_execution::CoreSnapshot`], so every existing Trade/Account/Portfolio panel renders the remote
//! daemon's live trading state unchanged. Charts render too: the daemon ships a bounded bar tail
//! (`CoreSnapshotWire::bars`, ~300 closed + the forming candle per mounted series), which
//! [`wire_to_core`] rebuilds into the core's bar cache so `sync_from_core` drives the candlestick
//! engine exactly like the local GUI.
//!
//! Moved here VERBATIM from the GUI shell's `main.rs` (`vike-app` then — the CI-excluded GUI
//! binary) so the mapping finally runs in a gate — the FORWARD projection (`CoreSnapshot` →
//! `CoreSnapshotWire`, `vike-tradehub/src/publish.rs`) was CI-tested while this inverse was not, and
//! [`map_order_status`]'s hand-maintained Debug-spelling fallback meant a newly added
//! `vike_model::OrderStatus` variant silently rendered as terminal `Rejected` in the observer. The
//! exhaustive round-trip test below (no-wildcard `match` over every variant) turns that silent
//! drift into a compile/test failure. The shell reaches this module through
//! [`crate::backend::backend_conn`], which spawns [`spawn_bridge`] (and [`connect_control`]) for the
//! backend it connects to. ⚠ This ended "`vike-app` re-imports [`wire_to_core`] under its original
//! name (the same pattern as `crate::reconcile_config`)" until 2026-09-28: the desktop imports
//! nothing from here directly any more, and `reconcile_config` left this crate long ago — it is
//! `crates/vike-tradehub/src/reconcile_config.rs`.

use crate::backend::node_words::NodeWords;

/// Decode a wire order-status STRING back to `vike_model::OrderStatus`. The wire carries the status
/// as a rendered string; `OrderStatus::parse` decodes the SCREAMING_SNAKE `as_str` spelling
/// ("ACCEPTED"), which is what the node sends (`crates/vike-tradehub/src/publish.rs`'s
/// `project_order`). A node built before that sent the Debug/PascalCase spelling ("Accepted"), and a
/// desktop may observe an older deployed node, so that spelling stays accepted: try the canonical
/// parser first, fall back to the Debug spelling, and only then warn and treat an unknown status as `Rejected` (a
/// terminal, so an unknown status never renders as live/actionable in the GUI).
pub fn map_order_status(s: &str) -> vike_model::OrderStatus {
    use vike_model::OrderStatus;
    if let Some(st) = OrderStatus::parse(s) {
        return st;
    }
    match s {
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
        other => {
            tracing::warn!(
                status = other,
                "observe: unknown wire order status; treating as Rejected"
            );
            OrderStatus::Rejected
        }
    }
}

/// Decode a wire position-side STRING (the node renders `PositionSide`'s own `Display`:
/// `"BOTH"`/`"LONG"`/`"SHORT"`) back to the enum. A word outside the three is warned and shown as
/// the one-way `Both` leg rather than dropped: the row still carries its signed size.
pub fn map_position_side(s: &str) -> vike_model::events::PositionSide {
    use vike_model::events::PositionSide;
    match s {
        "BOTH" => PositionSide::Both,
        "LONG" => PositionSide::Long,
        "SHORT" => PositionSide::Short,
        other => {
            tracing::warn!(
                position_side = other,
                "observe: unknown wire position side; showing it as the one-way BOTH leg"
            );
            PositionSide::Both
        }
    }
}

/// Wire position → core `PositionView`. The wire carries the display-relevant subset; the
/// resolver-only fields (`mark_source`) and the inert margin-mode carriers (`margin_mode`/
/// `isolated_margin`) default to their off-path values (`None`/`Cross`/`None`).
pub fn map_pos(p: &vike_tradehub_client::wire::PositionViewWire) -> vike_execution::PositionView {
    vike_execution::PositionView {
        venue: p.venue.as_str().into(),
        symbol: p.symbol.as_str().into(),
        position_side: map_position_side(&p.position_side),
        size: p.size,
        average_price: p.average_price,
        unrealized: p.unrealized,
        mark_source: None,
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
pub fn map_order(o: &vike_tradehub_client::wire::OrderViewWire) -> vike_execution::OrderView {
    vike_execution::OrderView {
        order_client_id: o.order_client_id.clone(),
        venue: o.venue.clone(),
        // The account the order rests in, as the node said it. A label this client cannot parse
        // maps to `None`, which this type spells exactly like the default account — so it must
        // never be ATTRIBUTED as one (M-9 of the final review, slice B). The engine it rests in
        // carries the same label in its block's route key, and
        // `crate::orders::order_dispatch::orders_name_their_account` refuses to attribute any order
        // of a venue whose route keys this client cannot read; the order stays listed.
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
    r: &vike_tradehub_client::wire::OrderReleaseWire,
    own_symbol: &str,
    venue: Option<&str>,
) -> vike_model::OrderRelease {
    use vike_tradehub_client::wire::OrderReleasePriceWire;
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
pub fn map_held(
    h: &vike_tradehub_client::wire::OrderHeldViewWire,
) -> vike_execution::OrderHeldView {
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
pub fn map_venueblock(
    v: &vike_tradehub_client::wire::VenueBlockWire,
) -> vike_execution::VenueBlock {
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
                    let kind = match row.kind.as_str() {
                        "linear" => vike_model::NotionalKind::Linear,
                        "inverse" => vike_model::NotionalKind::Inverse,
                        _ => return None,
                    };
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
        positions: v.positions.iter().map(map_pos).collect(),
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
pub fn build_portfolio(
    w: &vike_tradehub_client::wire::CoreSnapshotWire,
) -> vike_execution::Portfolio {
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
        // Nothing downstream of this bridge divides by it: the drawdown latch runs in the REMOTE
        // core (against its own seeds) and `vike_alerting`'s `RuleTrigger::Drawdown` is mounted
        // only by `vike-tradehub`, which builds its snapshots locally via `vike_core::snapshot::build`.
        // The observe client is a read-only GUI. If a wire seed is ever added, map it here.
        capital_base: 0.0,
        missing_prices_total: w.venues.iter().map(|v| v.missing_prices).sum(),
        balances_by_asset: Vec::new(),
        venues: w.venues.iter().map(map_venueblock).collect(),
        primary_currency_stated: Some(vike_execution::Interned::from(primary_currency)),
        // The wire carries no group curve: the push alert that reads them is mounted only by `vike-tradehub`,
        // which builds its snapshots locally, never through this bridge.
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
/// model. The reason stays the node's word, with what lies behind it where the node says (`adapter
/// failed` covers a lease another client holds, a version skew and a refused key alike). A venue this
/// client's roster does not have, or a label it cannot parse (a newer node), is kept as received
/// (`AccountNamed::Unknown`, owner ruling 2026-11-12), never dropped: the row is still shown, under
/// the route key its venue and label render, which is the wire's own `route_key`.
pub fn map_not_running(
    n: &vike_tradehub_client::wire::ArmingNotRunningViewWire,
) -> vike_execution::ArmingNotRunningView {
    vike_execution::ArmingNotRunningView {
        account: vike_model::accounts::account_keys::AccountNamed::from_wire(
            &n.venue,
            n.account.as_deref(),
        ),
        tier: map_tier(n.tier.as_deref()),
        reason: match &n.detail {
            Some(detail) => format!("{}: {detail}", n.reason),
            None => n.reason.clone(),
        },
        // Already typed by the wire's reader: a word this build does not know arrives as `AdapterFailed`.
        block: n.block_word,
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
pub fn map_emulated(
    e: &vike_tradehub_client::wire::OrderEmulationViewWire,
) -> vike_execution::OrderEmulationView {
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

/// Bridge one remote `CoreSnapshotWire` into the `CoreSnapshot` the GUI panels already render (see the
/// module comment above). READ-ONLY: no local core; `bars` are rebuilt from the wire's bounded tail
/// (charts render), `marks`/`mounts`/`recon` empty, counters zero.
pub fn wire_to_core(
    w: &vike_tradehub_client::wire::CoreSnapshotWire,
) -> vike_execution::CoreSnapshot {
    vike_execution::CoreSnapshot {
        sequence: w.sequence,
        venue: w.venue.clone(),
        symbol: w.symbol.clone(),
        trading_state: w.trading_state,
        balance: w.balance,
        balance_mode: vike_execution::BalanceMode::Delta,
        portfolio: build_portfolio(w),
        positions: w.positions.iter().map(map_pos).collect(),
        marks: Vec::new(),
        orders: w.orders.iter().map(map_order).collect(),
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
                let to_bar = |b: &vike_tradehub_client::wire::BarWire| vike_marketdata::Bar {
                    timestamp: b.timestamp,
                    open: b.o,
                    high: b.h,
                    low: b.l,
                    close: b.c,
                    volume: b.v,
                    funding: None,
                    bid: None,
                    ask: None,
                    symbol: Some(s.symbol.clone()),
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
        fault: w.fault.clone(),
        conflated_market_drops: 0,
        rejected_commands: 0,
        reconcile: vike_execution::ReconcileBlock::default(),
        reconcile_coin_deltas: indexmap::IndexMap::new(),
        // `0` = "the node has not said", the same meaning `CoreSnapshot::empty` gives it. The
        // node's account-set digest does ride the wire (`CoreSnapshotWire::accounts_epoch`), but this
        // bridge does not forward it, so an observer's snapshot says `0` whatever the node mounts:
        // a reader that needs the digest takes it from the wire frame, never from this snapshot.
        accounts_epoch: 0,
        // The accounts the node does not run, each with why (decision 0156): what the Trade
        // window says for one instead of a bare "not running".
        not_running: w.not_running.iter().map(map_not_running).collect(),
    }
}

/// Render a daemon identity as the short display tag every status surface leads with: `name [demo]`,
/// `name [LIVE + demo]`, where `running` is what it runs now ([`crate::backend::node_words::running_tiers`];
/// split-plane I3: "which daemon is the armed control channel pointing at" is the spec's named
/// most-dangerous ambiguity). One authority for the spelling — the control line
/// ([`crate::backend::node_words::NodeWords::daemon_label`]) and the observe status
/// ([`observing_status`]) both read this, so the two strips can never disagree about a daemon.
pub fn identity_label(id: &vike_tradehub_client::wire::HubIdentityWire, running: &str) -> String {
    format!("{} [{running}]", id.name)
}

/// The connected observe status line — `"OBSERVING <addr> (connected)"` until a frame carries the
/// daemon's [`HubIdentityWire`](vike_tradehub_client::wire::HubIdentityWire), then led by
/// [`identity_label`] over what that frame says it runs: `"the build runner [demo] — OBSERVING <addr>
/// (connected)"`. Pure so it is unit-tested below; [`spawn_bridge`]'s thread is the one caller.
///
/// # ⚠ The `(connected)` tail is LOAD-BEARING, not decoration
///
/// Every status line this loop writes is read back by
/// `crates/vike-model/src/feed_status.rs`'s `parse_feed_status` — the ONE classifier behind the
/// status strip's dot (`crates/vike-app-core/src/ui/status_dot.rs`'s `feed_dot_color`), the
/// Connections tool's Status column, and the headless daemon's health gate. Three of the four lines
/// [`spawn_bridge`] sets already match a token in it (the dial reads `Connecting`, the drop reads
/// `Disconnected`, the dial failure reads `Error`); this one matched nothing, so a HEALTHY observe
/// link classified `Unknown` — grey in the strip, the word "Unknown" in Connections.
///
/// ⚠ **And the identity-bearing variant did not read `Unknown`. It read `Connected` BY ACCIDENT.**
/// [`identity_label`] renders a live daemon as `name [LIVE]`, and `live` is in that parser's
/// connected family — so `"the build runner [LIVE] — OBSERVING …"` went green while
/// `"sim-box [paper] — OBSERVING …"` stayed grey. The dot's colour tracked whether the observed
/// daemon was ARMED rather than whether the link was up: two unrelated questions, and a coincidence
/// that no test could see because the pinned one only ever fed it the identity-less string.
///
/// The token is added HERE rather than to the parser deliberately. `parse_feed_status` is shared by
/// three crates, and teaching it `observing` would put a word only THIS producer writes into a
/// vocabulary the daemon's reconcile health gate also reads, changing how every venue's feed string
/// is classified to fix one line in the GUI. The producer is also the side that KNOWS: this function
/// is called from the `Ok(remote)` arm of the reconnect loop and from nowhere else, so the claim it
/// is making is one it is entitled to make.
pub fn observing_status(
    backend: Option<&str>,
    addr: &str,
    identity: Option<(&vike_tradehub_client::wire::HubIdentityWire, &str)>,
) -> String {
    // ⚠ WHICH address this line names is `backend_identity`'s decision, not this function's — the
    // status bar and the Connections foot strip must not name two different boxes for one
    // connection. A daemon that reported its own address wins; `addr` (the DIAL address, and
    // through a tunnel always the tunnel mouth) is what is left when it reported none.
    let addr = crate::backend::backend_identity::shown_address(
        addr,
        identity.map(|(id, _)| id.advertise_addr.as_str()),
    );
    let bare = format!("OBSERVING {addr} (connected)");
    // The daemon's OWN self-report wins: it carries the tier tag as well as a name, and it is
    // the backend answering for itself rather than the registry answering about it. The registry
    // NAME is the rung below — the only identity this side has before the first frame lands, and
    // the whole of what an operator gets on a link that never carries one.
    let lead = match identity {
        Some((id, running)) => Some(identity_label(id, running)),
        None => backend.map(str::trim).filter(|n| !n.is_empty()).map(str::to_string),
    };
    led_by(lead.as_deref(), &bare)
}

/// Lead `bare` with `label` — UNLESS the lead would change how
/// [`vike_model::feed_status::parse_feed_status`] classifies the line.
///
/// ⚠ **A name is operator-chosen text going into a string a CLASSIFIER reads**, and that parser
/// substring-matches a fixed vocabulary over the whole line with `disconnected`/`error`/`failed`
/// winning outright. So a backend named `error-box`, or a daemon that calls itself `failed-over`,
/// turns a HEALTHY link's dot red — the status dot then reports the NAME instead of the link, which
/// is the same class of accident [`observing_status`]'s doc records for `[LIVE]` (a live daemon's
/// tag made the line classify `Connected` for a reason that had nothing to do with the connection).
///
/// The dot's correctness outranks the label: the label is shown at full length in the Connections
/// foot strip either way, whereas a red dot on a working link is a fact nobody can check. The
/// PRODUCER is also the side entitled to decide this — teaching the shared parser about names
/// would change how every venue's feed string is classified to fix one line in the GUI.
fn led_by(label: Option<&str>, bare: &str) -> String {
    let Some(label) = label.map(str::trim).filter(|l| !l.is_empty()) else {
        return bare.to_string();
    };
    let led = format!("{label} — {bare}");
    if vike_model::feed_status::parse_feed_status(&led)
        == vike_model::feed_status::parse_feed_status(bare)
    {
        led
    } else {
        bare.to_string()
    }
}

/// Stop handle for the [`spawn_bridge`] thread: raise the flag, then JOIN.
///
/// Exists for the multi-backend GUI (split-plane B1): switching backends at runtime replaces the
/// bridge, and the reconnect loop never exits on its own — without a join, every switch would leak
/// a thread that keeps dialling the OLD address forever. [`BridgeHandle::stop`] is idempotent and
/// [`Drop`] calls it, so letting the handle fall out of scope IS a full stop-and-join.
#[must_use = "dropping the handle stops the bridge thread — hold it for the bridge's lifetime"]
pub struct BridgeHandle {
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// `None` once [`BridgeHandle::stop`] has joined (taken on the first call — the idempotence),
    /// or when the spawn itself failed and there is no thread to join.
    join: Option<std::thread::JoinHandle<()>>,
    /// The daemon identity the CURRENT connection last reported (split-plane I3) — written by the
    /// bridge thread on every frame whose `identity` changed, cleared on a link drop (a stale name
    /// must not outlive its connection: the next daemon at the same address may be a different —
    /// or an identity-less — node). Read by the GUI via [`BridgeHandle::identity`] so the control
    /// status line can name the daemon without `CoreSnapshot` (built in the latency-gated vike-core fold) growing a
    /// display-only field.
    identity: std::sync::Arc<std::sync::Mutex<Option<vike_tradehub_client::wire::HubIdentityWire>>>,
    /// The datahub dial address the CURRENT connection's `Welcome.features` advertised
    /// (split-plane REQ-2, `datahub=<addr>`) — written at connect (the handshake is where the
    /// advertisement lives, so unlike `identity` it needs no frame), cleared on a link drop under
    /// the SAME discipline: the next daemon at this address may front a different datahub, or
    /// none, and a stale advertisement must not outlive its connection. Read by the GUI via
    /// [`BridgeHandle::advertised_datahub`] into the one resolution point
    /// ([`crate::data::datahub_resolve::resolve_datahub_address`]), where an explicit
    /// `config.datahub_addr` still always wins.
    advertised_datahub: std::sync::Arc<std::sync::Mutex<Option<String>>>,
    /// What the current connection's last frame said beyond the snapshot — written on every frame
    /// whose words changed, cleared on a link drop like `identity`. Read by the GUI via
    /// [`BridgeHandle::node_words`].
    words: std::sync::Arc<std::sync::Mutex<std::sync::Arc<NodeWords>>>,
}

impl BridgeHandle {
    /// What the current connection's last frame said beyond the snapshot ([`NodeWords`]); empty before
    /// the first frame and between a link drop and the next connection. A pointer read once per
    /// painted frame.
    pub fn node_words(&self) -> std::sync::Arc<NodeWords> {
        self.words.lock().map(|g| g.clone()).unwrap_or_default()
    }
    /// The daemon identity the current connection last reported — `None` before the first
    /// identity-carrying frame, from an older (pre-B3) node, or between a link drop and the next
    /// connection's first frame. A clone (five short strings) read once per painted frame.
    pub fn identity(&self) -> Option<vike_tradehub_client::wire::HubIdentityWire> {
        self.identity.lock().ok().and_then(|g| g.clone())
    }
    /// The datahub dial address the current connection's Welcome advertised — `None` before the
    /// first successful connect, from a daemon with no `datahub_advertise_addr` configured, or
    /// between a link drop and the next successful handshake (the advertisement is only as live
    /// as its connection). Feed it to [`crate::data::datahub_resolve::resolve_datahub_address`] beside the
    /// explicit `config.datahub_addr` — never adopt it directly.
    pub fn advertised_datahub(&self) -> Option<String> {
        self.advertised_datahub.lock().ok().and_then(|g| g.clone())
    }
    /// Raise the stop flag and join the bridge thread. Idempotent: the join handle is taken on
    /// the first call, so a second call (or the [`Drop`] after an explicit `stop`) is a no-op.
    /// Returns promptly — the thread re-checks the flag every [`STOP_SLICE`] both in the connected
    /// snapshot poll and inside the dial backoff, so a stop parks for ~one slice, never the full
    /// [`DIAL_BACKOFF`] window and never a whole `preferences.observe_poll_ms` however long that is
    /// set.
    pub fn stop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

impl Drop for BridgeHandle {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Backoff between dial attempts — long enough not to hammer a down daemon.
const DIAL_BACKOFF: std::time::Duration = std::time::Duration::from_secs(2);
/// How finely every wait of the bridge thread is sliced so a [`BridgeHandle::stop`] lands in ~one
/// slice: the granularity of the backoff sleep AND of the connected poll's sleep. It is NOT the
/// poll interval any more (that is the `poll` argument of [`spawn_bridge`], the
/// `preferences.observe_poll_ms` row): the two used to be one 50ms constant, and a poll longer
/// than the slice must not make a backend switch — which joins this thread on the GUI's frame
/// thread — wait out a whole poll.
const STOP_SLICE: std::time::Duration = std::time::Duration::from_millis(50);

/// How often the bridge asks the node what it runs (`strategy_status`, one short connection): a mount
/// comes and goes rarely, and no frame carries it. Far slower than the frames, never on this thread.
const MOUNTS_EVERY: std::time::Duration = std::time::Duration::from_secs(30);

/// Ask the node for its mount rows on a throwaway thread, landing them in `rows`, a cell of THIS
/// connection: an answer that lands after a link drop lands in a cell nobody reads. An older node
/// refuses the verb before anything is sent, and the rows then stay empty.
fn ask_mounts(
    addr: String,
    observe_key: String,
    rows: std::sync::Arc<std::sync::Mutex<Vec<vike_tradehub_client::wire::MountViewWire>>>,
) {
    let asked = std::thread::Builder::new().name("vike-app-mounts".into()).spawn(move || {
        match vike_tradehub_client::strategy_status(addr.as_str(), observe_key.as_bytes()) {
            Ok(status) => {
                if let Ok(mut g) = rows.lock() {
                    *g = status.mounts;
                }
            }
            Err(e) => tracing::debug!(%addr, "observe: the node's mount rows were not read: {e}"),
        }
    });
    if let Err(e) = asked {
        tracing::warn!("observe: could not start the mount rows' read: {e}");
    }
}

/// Sleep up to `total` in naps of at most [`STOP_SLICE`], returning as soon as `stopped` says so.
///
/// The ONE sliced wait the bridge thread uses, for the dial backoff and for the poll between
/// snapshot reads alike, so neither can park a [`BridgeHandle::stop`] for longer than one slice.
/// A `total` of at most one slice is one nap of exactly `total`, which at the default poll is the
/// plain 50ms sleep the loop always took.
fn nap_unless_stopped(total: std::time::Duration, stopped: &impl Fn() -> bool) {
    let mut waited = std::time::Duration::ZERO;
    while waited < total && !stopped() {
        let nap = STOP_SLICE.min(total - waited);
        std::thread::sleep(nap);
        waited += nap;
    }
}

/// Spawn the `--observe` push→`CoreSnapshot` bridge thread; the returned [`BridgeHandle`] is the
/// ONLY way to stop it before process exit — hold it for exactly the bridge's intended lifetime.
///
/// The connect lives INSIDE the thread so it (a) never blocks window startup and (b) RE-DIALS on a
/// dropped link: when the daemon restarts or the tunnel blips, `is_connected()` flips false, we drop
/// the handle, back off, and reconnect from scratch (fresh handshake + subscribe) — the observer
/// heals itself instead of freezing on the last frame. The status line tracks each phase.
///
/// Moved down out of `vike-app`'s `App::new` with the rest of the wiring: this loop is the only
/// consumer of three `vike_tradehub_client::TradehubCoreHandle` APIs (`connect` / `is_connected` /
/// `snapshot`) plus [`wire_to_core`], and it lived in a file no gate compiles — so a signature
/// change in that CI-tested crate broke the observer silently.
///
/// The GUI-owned arc-swap cell and the egui repaint stay in the binary as two closures: `publish`
/// receives the already-`Arc`ed snapshot exactly as `ArcSwap::store` wants it (so nothing is copied
/// that was not copied before), and `repaint` is the same `ctx.request_repaint()` wake. Keeping the
/// cell out of the signature is what lets this crate stay free of an `arc_swap` dependency — the
/// same choice [`crate::ui::core_sync`] made.
/// `backend` is the record's NAME (`crate::backend::backend_identity::status_label`) — the one thing this
/// side knows that IDENTIFIES the box, and `None` for a record that carries none. It is an
/// `Option<String>` rather than a second `String` deliberately: two adjacent `String` parameters
/// are a swap that compiles.
///
/// `poll` is the sleep between the bridge's reads of the push-fed cell — the caller's
/// `preferences.observe_poll_ms` row, so it is also the ceiling on how often a chart can refresh
/// from the node. It runs on this thread, never the GUI's frame thread, and is slept in
/// [`STOP_SLICE`] naps so a stop lands within one slice whatever it is.
///
/// `patience` is the operator's resolved link patience (the desktop reads the two
/// `preferences.*` rows in its `main.rs` and hands them down; this library reads no setting): the
/// bridge dials with `patience.observe_stall` as the observe stream's silence limit, on every
/// re-dial. `LinkPatience::default()` is the window every dial had before the rows existed.
pub fn spawn_bridge<P, R>(
    addr: String,
    backend: Option<String>,
    observe_key: String,
    poll: std::time::Duration,
    patience: vike_tradehub_client::liveness::LinkPatience,
    status: std::sync::Arc<std::sync::Mutex<String>>,
    publish: P,
    repaint: R,
) -> BridgeHandle
where
    P: Fn(std::sync::Arc<vike_execution::CoreSnapshot>) + Send + 'static,
    R: Fn() + Send + 'static,
{
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let stop_thread = stop.clone();
    let identity = std::sync::Arc::new(std::sync::Mutex::new(None));
    let identity_thread = identity.clone();
    let advertised_datahub = std::sync::Arc::new(std::sync::Mutex::new(None));
    let advertised_thread = advertised_datahub.clone();
    let words: std::sync::Arc<std::sync::Mutex<std::sync::Arc<NodeWords>>> = Default::default();
    let words_thread = words.clone();
    let address_thread = addr;
    let backend_thread = backend;
    let key_thread = observe_key;
    let join = std::thread::Builder::new()
        .name("vike-app-observe-bridge".into())
        .spawn(move || {
            let stopped = || stop_thread.load(std::sync::atomic::Ordering::Relaxed);
            let set_status = |s: &str| {
                if let Ok(mut g) = status.lock() {
                    *g = s.to_string();
                }
            };
            let set_identity = |id: &Option<vike_tradehub_client::wire::HubIdentityWire>| {
                if let Ok(mut g) = identity_thread.lock() {
                    g.clone_from(id);
                }
            };
            let set_advertised = |a: Option<String>| {
                if let Ok(mut g) = advertised_thread.lock() {
                    *g = a;
                }
            };
            let set_words = |w: NodeWords| {
                if let Ok(mut g) = words_thread.lock() {
                    *g = std::sync::Arc::new(w);
                }
            };
            let mut shown_words = NodeWords::default();
            let mut last_sequence = u64::MAX;
            // The identity currently RENDERED in the status line + shared cell — per connection,
            // reset on a drop (a stale daemon name must not outlive its connection).
            let mut shown_identity: Option<vike_tradehub_client::wire::HubIdentityWire> = None;
            while !stopped() {
                // The NAME leads here too: "connecting to 127.0.0.1:7879…" is the same string on
                // every box, and this is the line an operator stares at while nothing works.
                set_status(&led_by(
                    backend_thread.as_deref(),
                    &format!("connecting to {address_thread}…"),
                ));
                match vike_tradehub_client::TradehubCoreHandle::connect_with_read_timeout(
                    address_thread.as_str(),
                    key_thread.as_bytes(),
                    patience.observe_stall,
                ) {
                    Ok(remote) => {
                        set_status(&observing_status(
                            backend_thread.as_deref(),
                            &address_thread,
                            None,
                        ));
                        // The REQ-2 advertisement lives in the handshake this connect just ran —
                        // publish it for the GUI's datahub resolution (explicit key still wins,
                        // see `crate::data::datahub_resolve`). Cleared again on a link drop below.
                        set_advertised(remote.advertised_datahub().map(str::to_string));
                        tracing::info!(
                            addr = %address_thread,
                            advertised_datahub = remote.advertised_datahub().unwrap_or("(none)"),
                            "observe: connected"
                        );
                        let mount_rows: std::sync::Arc<std::sync::Mutex<Vec<_>>> =
                            std::sync::Arc::default();
                        let mut mounts_asked: Option<std::time::Instant> = None;
                        // Poll the LOCAL push-fed cell until the link drops (or we are told to
                        // stop) — a poll of this size never blocks the node.
                        while remote.is_connected() && !stopped() {
                            if mounts_asked.is_none_or(|at| at.elapsed() >= MOUNTS_EVERY) {
                                mounts_asked = Some(std::time::Instant::now());
                                ask_mounts(
                                    address_thread.clone(),
                                    key_thread.clone(),
                                    std::sync::Arc::clone(&mount_rows),
                                );
                            }
                            let wire = remote.snapshot();
                            if wire.sequence != last_sequence {
                                last_sequence = wire.sequence;
                                // I3: the frame names its daemon and what it runs — surface them in
                                // the observe status and the shared cell the GUI reads (only on
                                // change; the usual case is a cheap eq check per frame).
                                let mut said = NodeWords::of(&wire);
                                if let Ok(rows) = mount_rows.lock() {
                                    said.mounts.clone_from(&rows);
                                }
                                if wire.identity != shown_identity
                                    || said.running != shown_words.running
                                {
                                    shown_identity.clone_from(&wire.identity);
                                    set_identity(&wire.identity);
                                    set_status(&observing_status(
                                        backend_thread.as_deref(),
                                        &address_thread,
                                        wire.identity.as_ref().map(|id| (id, said.running.as_str())),
                                    ));
                                }
                                // Published BEFORE the snapshot, so the frame that repaints sees it.
                                if said != shown_words {
                                    shown_words = said;
                                    set_words(shown_words.clone());
                                }
                                publish(std::sync::Arc::new(wire_to_core(&wire)));
                                repaint();
                            }
                            nap_unless_stopped(poll, &stopped);
                        }
                        if stopped() {
                            // Told to stop, not a dropped link: no "reconnecting…" status/log.
                            break;
                        }
                        // Link dropped. Force a re-store on the NEW connection's first
                        // frame (its `seq` may restart at 0), forget the dropped connection's
                        // identity AND its datahub advertisement (the next daemon may be a
                        // different node fronting a different — or no — datahub), and flag the
                        // outage.
                        last_sequence = u64::MAX;
                        shown_identity = None;
                        set_identity(&None);
                        shown_words = NodeWords::default();
                        set_words(NodeWords::default());
                        set_advertised(None);
                        tracing::warn!(addr = %address_thread, "observe: link dropped; reconnecting");
                        set_status(&format!("disconnected from {address_thread}, reconnecting…"));
                        repaint();
                    }
                    Err(e) => {
                        tracing::warn!(addr = %address_thread, error = %e, "observe: reconnect failed; retrying");
                        set_status(&format!(
                            "observe connect to {address_thread} failed ({e}); retrying…"
                        ));
                    }
                }
                // Backoff before the next dial, SLICED: the flag is re-checked every STOP_SLICE so
                // a `stop()` mid-backoff returns in ~one slice, never the full window (the old
                // unsliced 2s sleep is why this thread could only die with the process — B6).
                nap_unless_stopped(DIAL_BACKOFF, &stopped);
            }
            tracing::info!(addr = %address_thread, "observe: bridge stopped");
        })
        .ok();
    BridgeHandle { stop, join, identity, advertised_datahub, words }
}

/// Build the `--observe` **`Scope::Write` WRITE path** — opt-in, default OFF.
///
/// A SECOND, dedicated connection under `Scope::Write`, built ONLY when `enabled` (the
/// `flags.tradehub_control` row of the PC's settings database — the CALLER reads that row and the
/// key, so this stays a pure function of its arguments) AND `key` is
/// present and non-blank. When it connects, the GUI's order buttons drive this observer's REMOTE
/// core (see `App.tradehub_control` + the command-dispatch choke point): ⚠ **this can place/cancel REAL
/// orders on the daemon.** A missing key or a failed connect leaves it `None` and the observer
/// stays read-only.
///
/// `connect` may block briefly at startup (fine — same one-shot handshake cost as
/// [`TradehubCoreHandle::connect`](vike_tradehub_client::TradehubCoreHandle::connect)).
///
/// `patience.control_reply` is the control worker's reply wait (the caller read the
/// `preferences.control_reply_timeout_ms` row; this library reads no setting).
///
/// Moved down out of `App::new` because it is a **safety gate guarding real order placement** whose
/// refuse-paths lived in a file no test could reach; the two that need no network are pinned below.
pub fn connect_control(
    enabled: bool,
    addr: &str,
    key: Option<&str>,
    patience: vike_tradehub_client::liveness::LinkPatience,
) -> Option<vike_tradehub_client::TradehubControlHandle> {
    if !enabled {
        return None;
    }
    match key.filter(|k| !k.trim().is_empty()) {
        Some(key) => {
            match vike_tradehub_client::TradehubControlHandle::connect_with_reply_timeout(
                addr,
                key.as_bytes(),
                patience.control_reply,
            ) {
                Ok(h) => {
                    tracing::warn!(
                        %addr,
                        "CONTROL channel connected — this observer can place/cancel REAL \
                         orders on the remote daemon"
                    );
                    Some(h)
                }
                Err(e) => {
                    tracing::error!(%addr, error = %e, "control connect failed; observer stays read-only");
                    None
                }
            }
        }
        None => {
            tracing::warn!(
                "flags.tradehub_control is on but no VIKE_TRADEHUB_CONTROL_KEY resolved; \
                 observer stays read-only"
            );
            None
        }
    }
}

// `pub(crate)` for its wire fixture alone: the Trade window's glue tests build a snapshot through
// the real `wire_to_core`, as the desktop receives one.
#[path = "observe_bridge_tests.rs"]
#[cfg(test)]
pub(crate) mod observe_bridge_tests;
