//! `readmit` — on boot, tell each engine again which symbols it trades beyond the one it was started
//! on, with fresh facts (decision 0173, item 8; decision 0174).
//!
//! A restart restores positions and resting orders for EVERY symbol (and, with no mark, every symbol an
//! earlier run admitted, held or flat: marks are market data, never restored), and the engine's own
//! `restore_runtime_state` puts back the symbols a previous run admitted on demand (in
//! `extra_symbols`, with the on-demand grid row it had then), so the snapshot LISTS them. What it
//! cannot restore is a venue's CURRENT tick and lot, or the price feed of the symbol: [`targets`] reads the symbols off the journal's restored engines and [`readmit`] sends one
//! `AdmitSymbol` for each (a refresh), on the core's command lane, BEFORE the reconcile driver is
//! spawned: the lane is FIFO, so every admission folds ahead of the first `ReconcileReports`.
//!
//! The facts come from the same [`SymbolAdmitter`] the control door uses, on the boot thread, so a venue
//! that cannot answer costs a logged error per symbol and nothing else: the symbol keeps the grid the
//! journal restored, and the door refreshes it on the next order. Nothing here refuses a start.
//!
//! A LANE TWIN's own symbol (`binance#PERP`'s `BTCUSDT.P`) is the engine's mounted symbol, so [`targets`]
//! never lists it and [`readmit_and_price`] never prices it, and no strategy mounts it either:
//! [`twin_targets`] names the twins this boot built and [`warm_twins`] opens their price lanes, each on a
//! short-lived thread the boot does not wait for.
//!
//! Any engine's own symbol, default or labelled, is the same case when the engine holds a position or an order in it
//! (decision 0197, and 0198 for a labelled engine): [`targets`] skips it, so a restored hold on it was priced by
//! nobody unless a strategy is mounted there.
//! [`own_hold_targets`] names those from the journal, the boot opens their lanes with [`warm_twins`] too, and
//! [`own_symbol_holds`] is the same question asked of the published snapshot by the lane keeper that keeps (and
//! releases) them while the node runs (`TradeLeaseRegistry::spawn_lane_keeper`). A COIN LEDGER's own symbol is kept there
//! flat or not, because its money is valued at that price (0198's amendment of 2026-11-09). The keeper also holds the
//! lanes of the live mounts ([`mount_lane_holds`]) and of the held orders whose release reads another venue's price
//! ([`release_price_holds`]). A `paper` account follows its venue's market clock ([`paper_waits_for_open`]): while that
//! market is closed the keeper holds nothing for it.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

use vike_core::RETIRED_ROUTE_KEY_SUFFIX;
use vike_execution::{Command, EngineSnapshot, SymbolAdmission};
use vike_gatewayhub_client::proto::SymbolPropertiesRefusalCode;
use vike_gatewayhub_client::trade::SymbolPropertiesRequestError;
use vike_model::AccountTier;
use vike_model::accounts::account_keys::{AccountLabel, route_key_of};
use vike_secrets::account_directory::AccountDirectory;
use vike_venues::LANE_TWINS;

use crate::refusals::{NotRunningReason, NotRunningReasonId};
use crate::server::symbol_admission::{SymbolAdmitter, with_confirmed_leverage};

/// Restate `sent`, the admission just sent for a symbol, with the leverage the daemon's commit confirmed
/// for it (`SymbolAdmitter::confirmed_leverage`): a SECOND `AdmitSymbol`, the symbol's own margin rate, on the
/// same lane. Nothing confirmed, or `sent` already says it, sends nothing. Answers what is now the latest
/// admission the engine holds for the symbol, so a later commit restates against it and not against the
/// first.
fn restate_leverage(
    facts: &dyn SymbolAdmitter,
    sent: SymbolAdmission,
    send: &mut impl FnMut(Command),
) -> SymbolAdmission {
    match with_confirmed_leverage(facts, &sent) {
        Some(second) => {
            tracing::info!(
                route_key = %second.route_key, symbol = %second.symbol,
                initial_margin_requirement = ?second.initial_margin_requirement,
                "boot: restating the re-admitted symbol with its own margin rate (the venue holds \
                 the account's leverage for it)"
            );
            send(Command::AdmitSymbol(Box::new(second.clone())));
            second
        }
        None => sent,
    }
}

/// The `why` of a target the pass did not ask because its budget was spent ([`ReadmitPace`]).
pub const OVER_BUDGET: &str = "the boot's re-admission budget was spent before this symbol's turn";

/// One symbol an engine must be told about again.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadmitTarget {
    /// The account's engine (`Engine::route_key`).
    pub route_key: String,
    /// Its venue.
    pub venue: String,
    /// The symbol, spelled as the engine holds it.
    pub symbol: String,
    /// [`targets`] read this symbol as ADMITTED with no position and no live order in it, so a venue that says
    /// it is gone (its code: [`SymbolPropertiesRefusalCode::is_gone`]) ends the matter ([`give_back`]). False on every other target: one
    /// the engine holds something in stays a loud miss, because an operator has to look at it.
    pub flat: bool,
}

/// The symbols the restored `engines` trade beyond the symbol each was started on and its DECLARED
/// extras, in first-seen order and without repeats: every symbol an earlier run ADMITTED (even though the
/// snapshot lists it in `extra_symbols`, and whether or not the engine still holds a position or a live
/// order in it), then any other symbol a position or a live order names. A STOPPED engine's tombstone
/// (a route key ending in [`RETIRED_ROUTE_KEY_SUFFIX`], which the core journals a stopped primary
/// under once a newer engine of its account replaced it, decision 0169) is not an engine of this boot:
/// the boot restore SKIPS that snapshot, so it holds no lease and has nothing to be told. It yields no
/// target, said once at `info`. No running engine can carry the suffix: a label is `[A-Z0-9]+`
/// (`vike_model::accounts::account_keys::AccountLabel::parse`), so `#stopped` is never one.
pub fn targets(engines: &[EngineSnapshot]) -> Vec<ReadmitTarget> {
    let mut out: Vec<ReadmitTarget> = Vec::new();
    for snap in engines {
        let route_key = snap.route_key.clone().unwrap_or_else(|| snap.venue.clone());
        if route_key.ends_with(RETIRED_ROUTE_KEY_SUFFIX) {
            tracing::info!(
                %route_key, symbols = ?snap.extra_symbols,
                "boot: `{route_key}` is the journal's record of an engine that was stopped and \
                 replaced (decision 0169); it runs no engine and holds no lease, so none of its \
                 symbols is re-admitted"
            );
            continue;
        }
        let held = snap
            .account
            .positions
            .iter()
            .filter(|(_, entry)| entry.size != 0.0)
            .map(|((_, symbol, _), _)| symbol.as_str())
            .chain(
                snap.registry
                    .iter()
                    .filter(|(_, order)| !order.status.is_terminal())
                    .map(|(_, order)| order.request.symbol.as_str()),
            )
            .collect::<Vec<&str>>();
        // EVERY symbol an earlier run admitted, held or not: a flat symbol with no resting order is
        // restored into the engine too (`restore_runtime_state`) but with no mark, and nothing but this
        // pass subscribes its price (v0.1.66 on the CI box: a restored flat ETHUSDT was denied
        // `no-reference-price` for ever). Then the symbols a position or a live order names.
        let admitted = snap
            .extra_symbols
            .iter()
            .filter(|s| snap.limits.grid_by_symbol.get(s.as_str()).is_some_and(|g| g.on_demand))
            .map(String::as_str);
        for symbol in admitted.chain(held.iter().copied()) {
            // KNOWN to the mount is the engine's own symbol and a symbol DECLARED at assembly (in
            // `extra_symbols` with a grid row that is not on-demand, or none). An ADMITTED symbol is
            // in `extra_symbols` too (the snapshot lists it and `restore_runtime_state` restores it)
            // but its row is on-demand: it still wants fresh facts and a price subscription.
            let on_demand = snap.limits.grid_by_symbol.get(symbol).is_some_and(|g| g.on_demand);
            let known = symbol == snap.symbol
                || (!on_demand && snap.extra_symbols.iter().any(|s| s == symbol));
            let seen = out.iter().any(|t| t.route_key == route_key && t.symbol == symbol);
            if !symbol.is_empty() && !known && !seen {
                out.push(ReadmitTarget {
                    route_key: route_key.clone(),
                    venue: snap.venue.clone(),
                    symbol: symbol.to_string(),
                    flat: !held.contains(&symbol),
                });
            }
        }
    }
    out
}

/// **The LEGS of the combos the restored `engines` accepted**, each as a [`ReadmitTarget`] (`flat` false) with the client
/// order ids of the LIVE combos the journal's registry holds on it: an engine's `extra_symbols` that no admission put
/// there (no on-demand grid row) and that is not its own symbol. The core writes an accepted combo's legs there
/// itself (`lower_combo`) and the journal's snapshot keeps them, while an engine the node builds for a venue account
/// declares no other symbol, so on a `demo`/`live` engine these are the legs. The boot tells the gatewayhub each one
/// (`TradeLeaseRegistry::tell_combo_legs`) and the engine nothing: the engine restored them. A stopped engine's tombstone
/// yields none, as for [`targets`].
#[must_use]
pub fn combo_legs(engines: &[EngineSnapshot]) -> Vec<(ReadmitTarget, Vec<String>)> {
    engines
        .iter()
        .filter(|snap| {
            !snap.route_key.as_deref().unwrap_or(&snap.venue).ends_with(RETIRED_ROUTE_KEY_SUFFIX)
        })
        .flat_map(|snap| {
            snap.extra_symbols
                .iter()
                .filter(|s| {
                    **s != snap.symbol
                        && !snap.limits.grid_by_symbol.get(s.as_str()).is_some_and(|g| g.on_demand)
                })
                .map(|s| {
                    let live_combos = snap
                        .registry
                        .iter()
                        .filter(|(_, order)| {
                            !order.status.is_terminal()
                                && order.request.combo_legs.iter().any(|leg| leg.symbol == *s)
                        })
                        .map(|(order_client_id, _)| order_client_id.clone())
                        .collect();
                    let target = ReadmitTarget {
                        route_key: snap.route_key.clone().unwrap_or_else(|| snap.venue.clone()),
                        venue: snap.venue.clone(),
                        symbol: s.clone(),
                        flat: false,
                    };
                    (target, live_combos)
                })
        })
        .collect()
}

/// What a restored (journaled) engine HOLDS: its route key, venue, open positions and live orders.
#[derive(Debug, Clone, PartialEq)]
pub struct ReadmitRestoredHoldings {
    pub route_key: String,
    pub venue: String,
    /// `(symbol, size)` of every non-flat position.
    pub positions: Vec<(String, f64)>,
    /// How many orders were live (not terminal) in the journal.
    pub live_orders: usize,
}

/// The restored `engines` that hold a position or a live order, in journal order. A STOPPED engine's tombstone
/// ([`RETIRED_ROUTE_KEY_SUFFIX`]) is the journal's record of an engine that was replaced: its state is not an
/// engine of this boot and is left out, as for [`targets`]. Read BEFORE `apply_to` consumes the restore.
pub fn restored_exposure(engines: &[EngineSnapshot]) -> Vec<ReadmitRestoredHoldings> {
    engines
        .iter()
        .filter_map(|snap| {
            let route_key = snap.route_key.clone().unwrap_or_else(|| snap.venue.clone());
            if route_key.ends_with(RETIRED_ROUTE_KEY_SUFFIX) {
                return None;
            }
            let positions: Vec<(String, f64)> = snap
                .account
                .positions
                .iter()
                .filter(|(_, entry)| entry.size != 0.0)
                .map(|((_, symbol, _), entry)| (symbol.to_string(), entry.size))
                .collect();
            let live_orders =
                snap.registry.iter().filter(|(_, order)| !order.status.is_terminal()).count();
            (!positions.is_empty() || live_orders > 0).then(|| ReadmitRestoredHoldings {
                route_key,
                venue: snap.venue.clone(),
                positions,
                live_orders,
            })
        })
        .collect()
}

/// **A restored engine that holds something but was NOT rebuilt this boot is said at ERROR and listed NOT
/// RUNNING with what it holds** (review r2, finding 5). The boot restore puts a journaled engine's state back
/// only onto an engine the boot built under the same route key; a lane twin whose figure was cleared (no row
/// derives it) or an account deactivated since is not built, its journaled position and resting orders are
/// restored onto nothing, and the core says so at WARN only. The venue still holds them, with nothing watching.
/// `running` are the route keys of the engines the core holds; `listed` the NOT RUNNING entries the boot made
/// (an account the rows name but could not start keeps its reason, which now also names what it holds; any
/// other gets an entry of its own). Returns the route keys it flagged.
pub fn flag_unmanaged_exposure(
    holds: &[ReadmitRestoredHoldings],
    running: &[String],
    listed: &mut Vec<vike_execution::ArmingNotRunningView>,
) -> Vec<String> {
    let mut flagged = Vec::new();
    for hold in holds.iter().filter(|h| !running.contains(&h.route_key)) {
        let named: Vec<String> =
            hold.positions.iter().map(|(symbol, size)| format!("{symbol} {size}")).collect();
        let mut held = Vec::new();
        if !named.is_empty() {
            held.push(format!("position open at venue: {}", named.join(", ")));
        }
        if hold.live_orders > 0 {
            held.push(format!("{} order(s) resting", hold.live_orders));
        }
        let held = held.join("; ");
        tracing::error!(
            route_key = %hold.route_key,
            "BOOT: the journal restored `{}` holding {held}, but this boot built NO engine under that key (no \
             account row runs it now), so nothing manages them: they are still at the venue. The account is \
             listed NOT RUNNING with what it holds. Flatten it at the venue (or state its row again so it runs \
             and the next restart restores onto it)",
            hold.route_key
        );
        match listed.iter_mut().find(|nr| nr.route_key() == hold.route_key) {
            Some(entry) => {
                let reason = std::mem::take(&mut entry.reason);
                // Its own id: the sentence it wraps is held bare.
                let unmanaged = NotRunningReason::RestoredUnmanaged { reason, held };
                entry.reason = unmanaged.to_string();
                entry.refusal_id = Some(unmanaged.shown_id().to_owned());
            }
            None => listed.push(vike_execution::ArmingNotRunningView {
                // Read off the journaled key, which it keeps (a label this build cannot read included).
                account: vike_model::accounts::account_keys::AccountNamed::from_route_key(
                    &hold.venue,
                    &hold.route_key,
                ),
                tier: None,
                reason: NotRunningReason::RestoredNoAccount { held }.to_string(),
                block: None,
                refusal_id: Some(NotRunningReasonId::RestoredNoAccount.word().to_owned()),
            }),
        }
        flagged.push(hold.route_key.clone());
    }
    flagged
}

/// The lane twins this boot built an engine for, each as a [`ReadmitTarget`] on the twin's OWN mounted symbol
/// (the symbol its derived row states: its parent's `lane_twin_symbol` fact). A twin is listed only when the rows
/// DERIVE it and that row states a symbol
/// (`AccountDirectory::is_derived_lane_twin`: an account that states no leverage, a spot-only one, derives
/// none) and its route key is in `running` (`RuntimeAssembly::live_venues`, the engines that came up: a twin
/// whose start was refused has no lease to price over). [`targets`] never lists these: the symbol is the
/// engine's own, and a twin has no strategy mount whose feed would price it.
#[must_use]
pub fn twin_targets(accounts: &AccountDirectory, running: &HashSet<String>) -> Vec<ReadmitTarget> {
    LANE_TWINS
        .iter()
        .filter(|row| accounts.is_derived_lane_twin(row.venue, row.label))
        .filter_map(|row| {
            let label = AccountLabel::parse(row.label).ok()?;
            let route_key = route_key_of(row.venue, &label);
            let symbol =
                vike_config::account_arming::account_symbol(Some(accounts), row.venue, &label)?;
            running.contains(&route_key).then(|| ReadmitTarget {
                route_key,
                venue: row.venue.to_string(),
                symbol: symbol.to_string(),
                flat: false,
            })
        })
        .collect()
}

/// The `demo`/`live` engines the journal restored holding a position or a live order in their OWN symbol
/// (decision 0197, and a LABELLED engine's too since 0198), each as a [`ReadmitTarget`] under its own route key on that
/// symbol: [`targets`] never lists it (it is the engine's own, so the pass neither re-admits nor prices it), and
/// nothing prices it but a strategy mount. A `paper` engine is left out, as is one whose snapshot states no tier
/// and the tombstone of a stopped engine. The boot filters the result by the engines that came up, like
/// [`twin_targets`], and opens each lane with [`warm_twins`]. Read BEFORE `apply_to` consumes the restore.
#[must_use]
pub fn own_hold_targets(engines: &[EngineSnapshot]) -> Vec<ReadmitTarget> {
    engines
        .iter()
        .filter(|snap| {
            let route_key = snap.route_key.as_deref().unwrap_or(&snap.venue);
            !route_key.ends_with(RETIRED_ROUTE_KEY_SUFFIX)
                && !snap.symbol.is_empty()
                && matches!(snap.tier, Some(AccountTier::Demo | AccountTier::Live))
        })
        .filter(|snap| {
            let position =
                snap.account.positions.iter().any(|((_, symbol, _), entry)| {
                    entry.size != 0.0 && symbol.as_str() == snap.symbol
                });
            let order = snap.registry.iter().any(|(_, order)| {
                !order.status.is_terminal() && order.request.symbol == snap.symbol
            });
            position || order
        })
        .map(|snap| ReadmitTarget {
            route_key: snap.route_key.clone().unwrap_or_else(|| snap.venue.clone()),
            venue: snap.venue.clone(),
            symbol: snap.symbol.clone(),
            flat: false,
        })
        .collect()
}

/// **The engines the PUBLISHED snapshot shows holding a position or a live order in their OWN symbol** (decision
/// 0197, and a labelled engine's since 0198), **and every COIN LEDGER, flat or not** (0198's amendment of
/// 2026-11-09), as [`ReadmitTarget`]s: the lane keeper's view
/// (`TradeLeaseRegistry::spawn_lane_keeper`), read off the same published blocks and orders as the rate timer's `held`.
/// Only the newest block of a route key counts, only an engine at the `demo` or `live` tier with a symbol (or at
/// `paper` on a venue priced through its demo login, [`paper_quote_engines`]), and an
/// order counts while it is live (resting at the venue, or held by the core as an exit or a conditional). A symbol
/// a strategy mount prices is left to the registry's own check (`TradeLeaseRegistry::bind_mounted`), the one place that
/// knows the mounts.
///
/// A coin ledger (decision 0258 item 1: an engine MOUNTED on an inverse contract) keeps its money in the settle coin
/// and values it at its own symbol's price (`Engine::coin_rate`), so without the lane its balance, equity
/// and buying power read zero and the margin lane denies every opening. The block carries no `coin_key`, so the
/// test is the mounted symbol's contract kind: 0258's foreign-quoted exception is not visible here and would only
/// hold one lane more.
///
/// A `paper` engine whose venue's market is closed at `now_ms` holds no lane ([`paper_waits_for_open`]): no quote
/// reaches it before the open, and the keeper's first tick after the open holds it again.
#[must_use]
pub fn own_symbol_holds(
    blocks: &[vike_execution::VenueBlock],
    orders: &[std::sync::Arc<vike_execution::OrderView>],
    now_ms: i64,
) -> Vec<ReadmitTarget> {
    blocks
        .iter()
        .enumerate()
        .filter(|(i, block)| {
            !blocks[i + 1..].iter().any(|newer| newer.route_key == block.route_key)
        })
        .map(|(_, block)| block)
        .filter(|block| !block.symbol.is_empty() && holds_own_lane(block))
        .filter(|block| paper_waits_for_open(block.tier, &block.venue, now_ms).is_none())
        .filter(|block| {
            if block.contract_value_of(&block.symbol).kind == vike_model::NotionalKind::Inverse {
                return true;
            }
            let position = block
                .positions
                .iter()
                .any(|p| p.symbol == block.symbol.as_str() && p.size.abs() > 1e-12);
            let order = orders.iter().any(|o| {
                o.status.is_live()
                    && o.symbol == block.symbol
                    && route_key_of(&o.venue, &o.account.clone().unwrap_or_default())
                        == block.route_key
            });
            position || order
        })
        .map(|block| ReadmitTarget {
            route_key: block.route_key.clone(),
            venue: block.venue.clone(),
            symbol: block.symbol.clone(),
            flat: false,
        })
        .collect()
}

/// **The price lane every live strategy MOUNT wants** (decision 0245), as [`ReadmitTarget`]s: the lane keeper's second
/// source beside [`own_symbol_holds`]. A mount works by candles AND by ticks, and a mount the boot did not wire
/// (a runtime `trade strategy mount`, or one resurrected from the topology sidecar) has no feed of its own, so
/// its engine's tick lane is HELD for as long as the mount is in the published snapshot, exactly as an engine's
/// own symbol is held while it holds a position (`TradeLeaseRegistry::hold_price`; the lane goes back to the idle reaper
/// `HOLD_GRACE` after the mount is gone).
///
/// The published mount row names no account, so the engine is read off the blocks: the newest block of each
/// route key at the `demo` or `live` tier (a `paper` engine has no lease to ask) whose venue and OWN symbol are
/// the mount's. Two accounts of one venue on one symbol are both held (one shared subscription). A mount on a
/// symbol no engine was started on is skipped here: the control door admits it, and that admission's lane is the
/// door's. A mount the boot wired is left to the registry's own check (`TradeLeaseRegistry::bind_mounted`), as it is for
/// [`own_symbol_holds`]. Only [`vike_execution::MountViewKind::Mount`] rows count (the residual row is no mount).
///
/// A mount's DECLARED legs ([`vike_execution::MountView::legs`], one broker plan, step 12) are wanted the same way,
/// after its own symbol: each on the newest `demo`/`live` block of the leg's venue whose engine TRADES it (the
/// node admits a declared leg into its engine, `vike_tradehub_core`'s `admitted_legs`). The lane is sized on
/// the leg's own instrument row (`TradeLeaseRegistry::hold_price` asks the lease for that symbol), never on the mount's.
#[must_use]
pub fn mount_lane_holds(
    mounts: &[vike_execution::MountView],
    blocks: &[vike_execution::VenueBlock],
) -> Vec<ReadmitTarget> {
    let mut out: Vec<ReadmitTarget> = Vec::new();
    for mount in mounts.iter().filter(|m| m.kind == vike_execution::MountViewKind::Mount) {
        let legs = mount.legs.as_deref().unwrap_or_default();
        // `true`: the mount's own symbol, held on the engines STARTED on it; a leg, on those that trade it.
        let wanted = std::iter::once((mount.venue.as_str(), mount.symbol.as_str(), true))
            .chain(legs.iter().map(|(venue, symbol)| (venue.as_str(), symbol.as_str(), false)));
        for (venue, symbol, own) in wanted {
            for (i, block) in blocks.iter().enumerate() {
                let newest = !blocks[i + 1..].iter().any(|n| n.route_key == block.route_key);
                let engine = if own { block.symbol == symbol } else { block.trades(symbol) };
                if newest
                    && block.venue == venue
                    && !symbol.is_empty()
                    && engine
                    && matches!(block.tier, Some(AccountTier::Demo | AccountTier::Live))
                {
                    let target = ReadmitTarget {
                        route_key: block.route_key.clone(),
                        venue: block.venue.clone(),
                        symbol: symbol.to_string(),
                        flat: false,
                    };
                    if !out.contains(&target) {
                        out.push(target);
                    }
                }
            }
        }
    }
    out
}

/// **The price lane every HELD order whose release reads ANOTHER venue's price wants** (decision 0267's 2.13), as
/// [`ReadmitTarget`]s: the lane keeper's third source beside [`own_symbol_holds`] and [`mount_lane_holds`]. The core reads
/// such a condition off the price board of an engine of that venue that trades the symbol, so the lane that writes
/// that board is held while the order is held (the door's accept opened it with a committing price ask), and goes
/// back to the idle reaper `HOLD_GRACE` after the order leaves the HELD list. The engine is the newest block of a
/// route key of the condition's venue at the `demo` or `live` tier (a `paper` engine has no lease to ask), the
/// default account first, that trades the condition's symbol (`VenueBlock::trades`); none, nothing. A condition on
/// the order's own venue names none (`OrderReleasePrice::venue` is `None`) and is not this one's.
#[must_use]
pub fn release_price_holds(
    held: &[vike_execution::OrderHeldView],
    blocks: &[vike_execution::VenueBlock],
) -> Vec<ReadmitTarget> {
    let mut out: Vec<ReadmitTarget> = Vec::new();
    for condition in held.iter().filter_map(|h| h.release?.price) {
        let Some(venue) = condition.venue else { continue };
        let engine = blocks
            .iter()
            .enumerate()
            .filter(|(i, block)| {
                !blocks[i + 1..].iter().any(|newer| newer.route_key == block.route_key)
            })
            .map(|(_, block)| block)
            .filter(|block| {
                block.venue == venue.as_str()
                    && matches!(block.tier, Some(AccountTier::Demo | AccountTier::Live))
                    && block.trades(condition.symbol.as_str())
            })
            .min_by_key(|block| block.route_key != block.venue);
        if let Some(block) = engine {
            let target = ReadmitTarget {
                route_key: block.route_key.clone(),
                venue: block.venue.clone(),
                symbol: condition.symbol.to_string(),
                flat: false,
            };
            if !out.contains(&target) {
                out.push(target);
            }
        }
    }
    out
}

/// **Whether `block`'s engine has its own symbol's price lane held while it holds something there**: a `demo` or
/// `live` engine (decision 0197), or a `paper` one on a venue whose quotes can reach its paper book
/// (`crate::feeds::paper_quote_lane`: through the demo login for dukascopy, fxcm and ibkr, the owner's ruling of
/// 2026-11-16; through the venue's own feed everywhere else, his example 3 of the same day). A symbol a strategy mount
/// prices needs no hold (`TradeLeaseRegistry::may_hold`).
#[must_use]
pub fn holds_own_lane(block: &vike_execution::VenueBlock) -> bool {
    match block.tier {
        Some(AccountTier::Demo | AccountTier::Live) => true,
        Some(AccountTier::Paper) => crate::feeds::paper_quote_lane(&block.venue),
        None => false,
    }
}

/// **A `paper` account follows its venue's market clock** (the owner, 2026-11-21: "the same for other venues as for
/// IG"): `Some(open)`, the next open, when `tier` is `paper` and `venue`'s market is closed at `now_ms`, `None` for
/// every other account and while the market is open. The clock is the one the venue's own logins wait on
/// (`vike_venues::session::fx_login_closed_until`: every venue whose session is the FX week, read from
/// `vike_venues::session::session_for` and listed nowhere else), because such a paper account is priced, and an ig
/// epic's unit stated, only over that venue's logins. While it waits nothing asks a fact or a price lane for it (the
/// ig unit stater, `TradeLeaseRegistry::state_paper_units`; the lane keeper, [`own_symbol_holds`]), the door refuses its
/// orders in words (`crate::server::symbol_admission::paper_market_closed_refusal`), and the first tick after the open
/// picks it up with no restart. Pure: the caller reads its own clock.
#[must_use]
pub fn paper_waits_for_open(tier: Option<AccountTier>, venue: &str, now_ms: i64) -> Option<i64> {
    if tier != Some(AccountTier::Paper) {
        return None;
    }
    vike_venues::session::fx_login_closed_until(venue, now_ms)
}

/// **The PAPER engines priced through their venue's demo login** (the owner's ruling of 2026-11-16), as
/// [`ReadmitTarget`]s: the newest block of each route key at the `paper` tier, with a symbol, on a venue
/// `crate::feeds::paper_quotes_through_demo` names (dukascopy, fxcm, ibkr). Held or flat: the lane keeper hands them
/// to `TradeLeaseRegistry::sync_paper_quotes` each tick, which folds each one's quotes into its book's bars and says,
/// per engine, when no demo login can price it.
#[must_use]
pub fn paper_quote_engines(blocks: &[vike_execution::VenueBlock]) -> Vec<ReadmitTarget> {
    blocks
        .iter()
        .enumerate()
        .filter(|(i, block)| {
            !blocks[i + 1..].iter().any(|newer| newer.route_key == block.route_key)
        })
        .map(|(_, block)| block)
        .filter(|block| {
            block.tier == Some(AccountTier::Paper)
                && !block.symbol.is_empty()
                && crate::feeds::paper_quotes_through_demo(&block.venue)
        })
        .map(|block| ReadmitTarget {
            route_key: block.route_key.clone(),
            venue: block.venue.clone(),
            symbol: block.symbol.clone(),
            flat: false,
        })
        .collect()
}

/// **The `(venue, symbol)` pairs whose tap MOUNT mark is wanted** (decision 0245): every live mount whose engine is
/// on a LABELLED account (a route key that is not the venue's), boot or runtime, at the `demo` or `live` tier. The
/// core hands a venue's quote, trade and book to the venue's DEFAULT engine only, so such an engine's board is
/// written between bars by a mark or by nothing (`TradeLeaseRegistry::sync_mount_marks` switches the tap). Found as
/// [`mount_lane_holds`] finds an engine: the newest block of each route key whose venue and OWN symbol are the
/// mount's.
#[must_use]
pub fn mount_mark_pairs(
    mounts: &[vike_execution::MountView],
    blocks: &[vike_execution::VenueBlock],
) -> HashSet<(String, String)> {
    mount_lane_holds(mounts, blocks)
        .into_iter()
        .filter(|t| t.route_key != t.venue)
        .map(|t| (t.venue, t.symbol))
        .collect()
}

/// **The bar series every live strategy mount listens to** (decision 0245), as `(venue, symbol, interval)`, once
/// each: what `TradeLeaseRegistry::keep_bars` opens for a mount the boot did not wire (a runtime mount hears a bar only on
/// a series somebody subscribed). The residual row is no mount, and a row with no interval names no series. A
/// mount's declared legs ([`vike_execution::MountView::legs`]) are listened to at the mount's interval, after its own
/// series, so a RUNTIME mount's legs are heard as a boot mount's are (`live_runtime`'s `leg_bars`).
#[must_use]
pub fn mount_bar_series(mounts: &[vike_execution::MountView]) -> Vec<vike_execution::SeriesKey> {
    let mut out: Vec<vike_execution::SeriesKey> = Vec::new();
    for m in mounts.iter().filter(|m| m.kind == vike_execution::MountViewKind::Mount) {
        if m.symbol.is_empty() || m.interval.is_empty() {
            continue;
        }
        let legs = m.legs.as_deref().unwrap_or_default().iter();
        let own = std::iter::once((&m.venue, &m.symbol));
        for (venue, symbol) in own.chain(legs.map(|(venue, symbol)| (venue, symbol))) {
            let series = (venue.clone(), symbol.clone(), m.interval.clone());
            if !out.contains(&series) {
                out.push(series);
            }
        }
    }
    out
}

/// What the PUBLISHED snapshot says each engine holds, in EVERY symbol: [`symbol_holdings`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReadmitHoldings {
    /// The route keys the snapshot has a block for. An engine with no block has said nothing, and the reaper
    /// ([`crate::venue_backend::gateway::trade_lease_registry::TradeLeaseRegistry::reap_idle_admitted`]) reads that as "unknown", never as "holds nothing".
    pub known: HashSet<String>,
    /// Every `(route key, venue, symbol)` an engine holds a non-flat position or a live order in.
    pub held: Vec<ReadmitTarget>,
    /// Every `(route key, symbol)` an engine LISTS as one of its extra symbols (its slots, an admitted symbol or a
    /// declared leg): what `TradeLeaseRegistry::settle_releases` reads to see whether the engine let a symbol go
    /// (decision 0205). A route key with no block lists nothing, and [`Self::known`] says it said nothing.
    pub listed: HashSet<(String, String)>,
    /// The order client id of every LIVE order: a combo states no symbol of its own, so this is how
    /// `TradeLeaseRegistry::settle_combo_legs` sees that a resting combo still needs its legs read.
    pub live_orders: HashSet<String>,
}

/// **Every symbol each published engine holds anything in** (A39), as [`ReadmitHoldings`]: the admitted-pair reaper's view,
/// read off the same blocks and orders as [`own_symbol_holds`] but for EVERY symbol, the engine's own or admitted,
/// and for every tier (an engine that holds nothing is idle whatever its tier). Only the newest block of a route key
/// counts; an order counts while it is live (resting at the venue, or held by the core as an exit or a
/// conditional).
#[must_use]
pub fn symbol_holdings(
    blocks: &[vike_execution::VenueBlock],
    orders: &[std::sync::Arc<vike_execution::OrderView>],
) -> ReadmitHoldings {
    let mut holdings = ReadmitHoldings {
        live_orders: orders
            .iter()
            .filter(|o| o.status.is_live())
            .map(|o| o.order_client_id.clone())
            .collect(),
        ..ReadmitHoldings::default()
    };
    for (i, block) in blocks.iter().enumerate() {
        if blocks[i + 1..].iter().any(|newer| newer.route_key == block.route_key) {
            continue;
        }
        holdings.known.insert(block.route_key.clone());
        holdings
            .listed
            .extend(block.extra_symbols.iter().map(|s| (block.route_key.clone(), s.clone())));
        let positions =
            block.positions.iter().filter(|p| p.size.abs() > 1e-12).map(|p| p.symbol.as_str());
        let resting = orders
            .iter()
            .filter(|o| {
                o.status.is_live()
                    && route_key_of(&o.venue, &o.account.clone().unwrap_or_default())
                        == block.route_key
            })
            .map(|o| o.symbol.as_str());
        for symbol in positions.chain(resting) {
            let seen =
                holdings.held.iter().any(|t| t.route_key == block.route_key && t.symbol == symbol);
            if !symbol.is_empty() && !seen {
                holdings.held.push(ReadmitTarget {
                    route_key: block.route_key.clone(),
                    venue: block.venue.clone(),
                    symbol: symbol.to_string(),
                    flat: false,
                });
            }
        }
    }
    holdings
}

/// **The newest published block of `route_key`**, the one the core routes to. The core's list of engines only grows
/// (a stop tombstones an engine and a start of the same account APPENDS a new one under the same key), and the
/// engine's own `portfolio.venues` carries every one of them, the tombstone FIRST: a `find` on the key reads the
/// dead engine, whose positions and rate rows froze when it stopped. The published frame and the door's roster
/// already carry the newest block alone (`crate::publish::current_venue_blocks`); this is the same rule for a
/// reader of the raw snapshot.
fn newest_block<'a>(
    blocks: &'a [vike_execution::VenueBlock],
    route_key: &str,
) -> Option<&'a vike_execution::VenueBlock> {
    blocks.iter().rfind(|block| block.route_key == route_key)
}

/// **When the engine's standing conversion rate for `currency` was observed**, epoch ms, as the NEWEST block of
/// `route_key` publishes it; `None` when that engine holds no row for the currency. What the rate timer's "this
/// rate is not being refreshed" line ages (`TradeLeaseRegistry::bind_rate_rows`): read off a stopped engine's block it
/// counted the age from that engine's LAST admission, hours back, for a rate the running engine refreshed a
/// minute ago (the CI box, 2026-10-28, an fxcm account stopped and started in the running node).
#[must_use]
pub fn engine_rate_stamp(
    blocks: &[vike_execution::VenueBlock],
    route_key: &str,
    currency: &str,
) -> Option<i64> {
    newest_block(blocks, route_key)?
        .quote_rates
        .iter()
        .find(|row| &*row.currency == currency)
        .map(|row| row.as_of_ms)
}

/// **Whether the engine of `route_key` holds a non-flat position in `symbol`**, read off its NEWEST published
/// block: the rate timer's `held` (a symbol nothing is held in is left to the order path). A stopped engine's block,
/// which the core keeps, still lists the position it stopped with and said "held" for a symbol the running
/// engine never held.
#[must_use]
pub fn engine_holds_position(
    blocks: &[vike_execution::VenueBlock],
    route_key: &str,
    symbol: &str,
) -> bool {
    newest_block(blocks, route_key)
        .is_some_and(|block| block.positions.iter().any(|p| p.symbol == symbol && p.size != 0.0))
}

/// **Open the price lane of each lane twin's own symbol at the boot** ([`twin_targets`]), announced as
/// `pricing <venue> <k>/<n>` on the `starting` stream like the pass's own pricing steps. `warm` is the
/// lane's opening (`TradeLeaseRegistry::warm_price`: the facts look-up, the datahub subscribe), and it runs on a
/// short-lived `twin-price-warm` thread per twin, the thread `account_follow`'s hot start uses, which this
/// function does NOT wait for: a lease or a datahub that never answers must not hold `ready`, and a lane
/// that did not open costs a warning there and the door's own ask at the first order, as before. The
/// handles come back for a test to join; the boot drops them. The boot calls it a second time for the
/// restored own-symbol holds ([`own_hold_targets`]), with `TradeLeaseRegistry::hold_price` as `warm`: the shape is the
/// same, only the target and the lane differ.
///
/// Not a [`ReadmitMiss`] of the retry thread on purpose: that retry TELLS the daemon the symbol is admitted, which
/// a twin's own symbol is not.
pub fn warm_twins(
    twins: &[ReadmitTarget],
    pace: &ReadmitPace<'_>,
    warm: impl Fn(&ReadmitTarget) + Send + Sync + 'static,
) -> Vec<std::thread::JoinHandle<()>> {
    let warm = Arc::new(warm);
    let mut started = Vec::new();
    for (i, t) in twins.iter().enumerate() {
        pace.say("pricing", t, i + 1, twins.len());
        let spawned = std::thread::Builder::new().name("twin-price-warm".into()).spawn({
            let (t, warm) = (t.clone(), Arc::clone(&warm));
            move || (*warm)(&t)
        });
        match spawned {
            Ok(handle) => started.push(handle),
            Err(e) => tracing::warn!(
                error = %e, route_key = %t.route_key, symbol = %t.symbol,
                "boot: the lane twin's price lane could not be opened (no thread); its first order asks"
            ),
        }
    }
    started
}

/// What [`readmit`] did.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct ReadmitOutcome {
    /// Admissions sent.
    pub sent: usize,
    /// The targets those admissions were for, so their prices can be subscribed once the feeds are up.
    pub admitted: Vec<ReadmitTarget>,
    /// `(route key, symbol, why)` for each symbol the facts source could not answer.
    pub failed: Vec<(String, String, String)>,
}

/// How long the boot's whole pass ([`readmit_and_price`]) may take before it hands the rest to the retry
/// thread. Each symbol is a gatewayhub round trip with a venue HTTP call behind it, bounded one at a time
/// (`TradeLeaseRegistry::begin_boot`) but not in sum, so N slow-but-successful lookups used to add up without
/// limit while `ready` waited. The deploy helper's window follows the `starting` lines, so the pass must
/// also end on its own.
pub const READMIT_BUDGET: Duration = Duration::from_secs(60);

/// How the boot's pass reports itself and bounds itself: [`ReadmitPace::default`] does neither.
#[derive(Clone, Copy)]
pub struct ReadmitPace<'a> {
    /// Told `readmitting <venue> <k>/<n>` (then `pricing <venue> <k>/<n>`) before each symbol, so the
    /// `{"kind":"starting"}` stream moves with the pass instead of going quiet until its end.
    progress: Option<&'a dyn Fn(&str)>,
    /// The pass's whole budget, from `started`; `None` = unbounded.
    budget: Option<Duration>,
    started: Instant,
    now: fn() -> Instant,
    /// How a busy gatewayhub is waited out ([`BUSY_PAUSE`]); the tests move their fake clock instead.
    sleep: fn(Duration),
}

impl Default for ReadmitPace<'_> {
    fn default() -> Self {
        ReadmitPace {
            progress: None,
            budget: None,
            started: Instant::now(),
            now: Instant::now,
            sleep: std::thread::sleep,
        }
    }
}

impl<'a> ReadmitPace<'a> {
    /// A pass that reports to `progress` and ends after `budget`, counted from now.
    #[must_use]
    pub fn new(budget: Duration, progress: &'a dyn Fn(&str)) -> Self {
        Self::with_clock(budget, progress, Instant::now, std::thread::sleep)
    }

    /// [`ReadmitPace::new`] on the clock `now` and the wait `sleep` (the tests').
    #[must_use]
    fn with_clock(
        budget: Duration,
        progress: &'a dyn Fn(&str),
        now: fn() -> Instant,
        sleep: fn(Duration),
    ) -> Self {
        ReadmitPace { progress: Some(progress), budget: Some(budget), started: now(), now, sleep }
    }

    fn say(&self, what: &str, t: &ReadmitTarget, k: usize, n: usize) {
        if let Some(progress) = self.progress {
            progress(&format!("{what} {} {k}/{n}", t.venue));
        }
    }

    fn spent(&self) -> bool {
        self.budget
            .is_some_and(|budget| (self.now)().saturating_duration_since(self.started) >= budget)
    }
}

/// Say which targets the budget cut off, at `warn` (one line naming them: they are not failures, the retry
/// thread asks again).
fn say_over_budget(pace: &ReadmitPace<'_>, phase: &str, left: &[&ReadmitTarget]) {
    let names: Vec<String> = left.iter().map(|t| format!("{} {}", t.route_key, t.symbol)).collect();
    tracing::warn!(
        symbols = ?names,
        "boot: the {phase} pass spent its {:?} budget; {} symbols are left to the retry thread \
         ({})",
        pace.budget.unwrap_or_default(),
        names.len(),
        names.join(", ")
    );
}

/// How long the boot waits before it asks again after the gatewayhub refused with the code
/// [`SymbolPropertiesRefusalCode::Busy`]: one token of
/// the venue's lookup bucket comes back per 500 ms (`vike_gatewayhub::trade::adapters::symbol_facts`'s
/// `LOOKUP_REFILL`; this crate links no gatewayhub, so the figure is restated here).
pub const BUSY_PAUSE: Duration = Duration::from_millis(500);

/// How many times the boot waits [`BUSY_PAUSE`] for ONE symbol before it leaves the symbol to the retry
/// thread: the bucket's burst (10) spent by a long restore refills in a second, while a venue whose
/// budget is per minute (ig, ibkr) would otherwise hold the whole pass.
pub const MAX_PACED_ASKS: u32 = 6;

/// Whether `why` is the gatewayhub's "not now" for a per-venue request budget (its code
/// [`SymbolPropertiesRefusalCode::Busy`]).
fn is_busy(why: &SymbolPropertiesRequestError) -> bool {
    why.code() == Some(SymbolPropertiesRefusalCode::Busy)
}

/// Whether `why` says the venue no longer has the instrument (its code [`SymbolPropertiesRefusalCode::is_gone`]).
fn is_gone(why: &SymbolPropertiesRequestError) -> bool {
    why.code().is_some_and(SymbolPropertiesRefusalCode::is_gone)
}

/// The admission of `t`, waiting out the gatewayhub's per-venue lookup bucket instead of failing on it: a
/// restore of more than the bucket's burst of symbols on a per-symbol venue (bybit, binance spot) is paced
/// to the refill rate, at `warn` ("paced, retrying"). Bounded by [`MAX_PACED_ASKS`] and by `pace`'s budget.
fn admission_paced(
    pace: &ReadmitPace<'_>,
    facts: &dyn SymbolAdmitter,
    t: &ReadmitTarget,
) -> Result<vike_execution::SymbolAdmission, SymbolPropertiesRequestError> {
    let mut paused = 0;
    loop {
        match facts.admission(&t.route_key, &t.venue, &t.symbol) {
            Err(why) if is_busy(&why) && paused < MAX_PACED_ASKS && !pace.spent() => {
                paused += 1;
                tracing::warn!(
                    route_key = %t.route_key, symbol = %t.symbol,
                    "boot: the gatewayhub is rate-limiting `{}`'s facts lookups ({why}); paced, \
                     retrying in {BUSY_PAUSE:?} ({paused}/{MAX_PACED_ASKS})",
                    t.symbol
                );
                (pace.sleep)(BUSY_PAUSE);
            }
            other => return other,
        }
    }
}

/// **The venue says `t` is gone and the engine holds nothing in it** (an expired contract, a delisted pair): give
/// the symbol back through the statement the operator's `trade account remove-symbol` sends (decision 0205), and ask no more.
/// Asking again cannot heal it, and it would hold one of the engine's slots for ever. The engine is still the
/// authority: a refusal is its WARN and its note, and the symbol waits for the next boot.
fn give_back(
    t: &ReadmitTarget,
    why: &str,
    facts: &dyn SymbolAdmitter,
    send: &mut impl FnMut(Command),
) {
    tracing::info!(
        route_key = %t.route_key, symbol = %t.symbol,
        "`{}` in `{}`'s engine is gone from the venue ({why}) and the engine holds nothing in it: given back",
        t.symbol, t.route_key
    );
    send(Command::AdmitSymbol(Box::new(SymbolAdmission::releasing(
        t.route_key.clone(),
        t.symbol.clone(),
    ))));
    facts.release_requested(&t.route_key, &t.venue, &t.symbol);
}

/// Send an `AdmitSymbol` for every target the facts source can answer, through `send` (the core's
/// lossless command door), and report the rest. A request budget that stays spent past
/// [`MAX_PACED_ASKS`] waits is said at `warn` (the retry thread asks again); every other failure is said at
/// `error`. Past `pace`'s budget
/// the remaining targets are not asked: they come back in `failed` (`why` = [`OVER_BUDGET`]) so the caller
/// treats them as missed and the retry thread admits them.
///
/// ⚠ **Each symbol's commit (`facts.admitted`) is the daemon's leverage settle on a venue that posts one**
/// (three or four signed reads on a symbol the venue already holds at the account's figure; a write only when
/// it does not): seconds, where it used to be one round trip. There is no separate count to cap: the pass is
/// bounded by `pace`'s [`READMIT_BUDGET`] and by `MAX_EXTRA_SYMBOLS` per account, and what it does not reach
/// goes to the retry thread, whose restored resting orders stay HELD until the symbol is told, as for any
/// miss. A commit that times out (the boot's bound) is UNCONFIRMED and nothing more: no figure is kept, no
/// second `AdmitSymbol` is sent, and the symbol runs at the engine's own rate until a commit that completes.
pub fn readmit(
    targets: &[ReadmitTarget],
    facts: &dyn SymbolAdmitter,
    pace: &ReadmitPace<'_>,
    mut send: impl FnMut(Command),
) -> ReadmitOutcome {
    let mut done = ReadmitOutcome::default();
    for (i, t) in targets.iter().enumerate() {
        if pace.spent() {
            let left: Vec<&ReadmitTarget> = targets[i..].iter().collect();
            say_over_budget(pace, "readmit", &left);
            done.failed.extend(
                left.iter()
                    .map(|t| (t.route_key.clone(), t.symbol.clone(), OVER_BUDGET.to_string())),
            );
            break;
        }
        pace.say("readmitting", t, i + 1, targets.len());
        match admission_paced(pace, facts, t) {
            Ok(admission) => {
                send(Command::AdmitSymbol(Box::new(admission.clone())));
                facts.admitted(&t.route_key, &t.venue, &t.symbol);
                restate_leverage(facts, admission, &mut send);
                done.sent += 1;
                done.admitted.push(t.clone());
            }
            Err(why) if is_busy(&why) => {
                tracing::warn!(
                    route_key = %t.route_key, symbol = %t.symbol,
                    "boot: `{}` in `{}`'s engine is not refreshed yet ({why}); paced, left to the \
                     retry thread",
                    t.symbol, t.route_key
                );
                done.failed.push((t.route_key.clone(), t.symbol.clone(), why.to_string()));
            }
            Err(why) if t.flat && is_gone(&why) => {
                give_back(t, why.text(), facts, &mut send);
            }
            Err(why) => {
                tracing::error!(
                    route_key = %t.route_key, symbol = %t.symbol,
                    "boot: could not refresh `{}` in `{}`'s engine ({why}); it keeps the grid the \
                     journal restored and has no price feed until an order on it asks again",
                    t.symbol, t.route_key
                );
                done.failed.push((t.route_key.clone(), t.symbol.clone(), why.to_string()));
            }
        }
    }
    done
}

/// The boot's whole pass over `targets`: [`readmit`], then the price subscription of each symbol it
/// admitted. Returns what it did and the targets still MISSING: the ones whose facts the source could
/// not answer, and the ones admitted whose price feed could not be had (a failed `AdmitSymbol` commit
/// makes the price ask fail too). A symbol missing here stays as the restore left it, and its restored
/// resting orders are HELD, not adopted, until the symbol is told ([`retry_missed`] heals a transient
/// miss; an order on the symbol does too).
///
/// `pace` bounds the WHOLE pass, both loops: past its budget the symbols not yet asked (or not yet priced)
/// are left to the retry thread, said once at `warn`, and each `progress` call moves the start's
/// `starting` stream.
pub fn readmit_and_price(
    targets: &[ReadmitTarget],
    facts: &dyn SymbolAdmitter,
    pace: &ReadmitPace<'_>,
    send: impl FnMut(Command),
) -> (ReadmitOutcome, Vec<ReadmitMiss>) {
    // The engines these targets belong to, as of NOW: a retry works for them and for no later engine.
    let generation = |t: &ReadmitTarget| facts.generation(&t.route_key);
    let generations: Vec<u64> = targets.iter().map(generation).collect();
    let done = readmit(targets, facts, pace, send);
    let mut missed: Vec<ReadmitMiss> = targets
        .iter()
        .zip(&generations)
        .filter(|(t, _)| {
            done.failed
                .iter()
                .any(|(rk, symbol_name, _)| *rk == t.route_key && *symbol_name == t.symbol)
        })
        .map(|(t, &generation)| ReadmitMiss { target: t.clone(), told: false, generation })
        .collect();
    let generation_of =
        |t: &ReadmitTarget| targets.iter().position(|x| x == t).map_or(0, |i| generations[i]);
    for (i, t) in done.admitted.iter().enumerate() {
        if pace.spent() {
            let left: Vec<&ReadmitTarget> = done.admitted[i..].iter().collect();
            say_over_budget(pace, "price", &left);
            missed.extend(left.into_iter().map(|t| ReadmitMiss {
                target: t.clone(),
                told: true,
                generation: generation_of(t),
            }));
            break;
        }
        pace.say("pricing", t, i + 1, done.admitted.len());
        if let Err(why) = facts.price(&t.route_key, &t.venue, &t.symbol, true) {
            tracing::error!(
                route_key = %t.route_key, symbol = %t.symbol,
                "boot: no price feed for the re-admitted `{}` ({why}); an opening order on it \
                 is refused until one exists",
                t.symbol
            );
            missed.push(ReadmitMiss {
                target: t.clone(),
                told: true,
                generation: generation_of(t),
            });
        }
    }
    (done, missed)
}

/// A symbol the boot's pass could not finish, and where it stopped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadmitMiss {
    /// The symbol.
    pub target: ReadmitTarget,
    /// Whether it WAS admitted and told (only its price feed is missing): a retry then subscribes the
    /// price and sends no second `AdmitSymbol`.
    pub told: bool,
    /// The account's engine generation at the boot ([`crate::server::symbol_admission::AccountReadiness::generation`]): once it moves the
    /// engine this symbol belonged to is gone, and the retry drops the symbol.
    pub generation: u64,
}

/// One retry round over `missed`: for each symbol, the facts look and the admission (unless it was already
/// told), then the price subscription. The account's engine generation is checked before the round's
/// work on a symbol and again immediately before each send, `admitted` and `price`: a symbol whose
/// engine was stopped or started anew since the boot is dropped untouched (the new engine's own start
/// admits through the door's gates). Returns what is STILL missing.
pub fn retry_round(
    missed: &[ReadmitMiss],
    facts: &dyn SymbolAdmitter,
    send: impl FnMut(Command),
) -> Vec<ReadmitMiss> {
    round(missed, facts, send, true).0
}

/// [`retry_round`] for a caller that repeats it on a schedule of its own (the rate timer's heal of a restored
/// position, `TradeLeaseRegistry::refresh_held_rates`): the same work, but a failure is said at `debug` and RETURNED (the
/// first reason a symbol was left missing, in words) so the caller says it once, when the state changes, instead
/// of this round saying it at `error` every time it is repeated.
pub fn retry_round_quiet(
    missed: &[ReadmitMiss],
    facts: &dyn SymbolAdmitter,
    send: impl FnMut(Command),
) -> (Vec<ReadmitMiss>, Option<String>) {
    round(missed, facts, send, false)
}

/// The body of [`retry_round`] / [`retry_round_quiet`]: `loud` = a failure is said at `warn`/`error`; the reason
/// of the first symbol left missing is returned either way.
fn round(
    missed: &[ReadmitMiss],
    facts: &dyn SymbolAdmitter,
    mut send: impl FnMut(Command),
    loud: bool,
) -> (Vec<ReadmitMiss>, Option<String>) {
    let mut left = Vec::new();
    let mut first_why: Option<String> = None;
    for m in missed {
        let t = &m.target;
        let current = || facts.generation(&t.route_key) == m.generation;
        let superseded = || {
            if loud {
                tracing::warn!(
                    route_key = %t.route_key, symbol = %t.symbol,
                    "boot retry: `{}`'s engine was stopped or started anew since the boot; the symbol is \
                     dropped (its engine admits through the door)",
                    t.route_key
                );
            } else {
                tracing::debug!(
                    route_key = %t.route_key, symbol = %t.symbol,
                    "retry: `{}`'s engine was stopped or started anew; the symbol is dropped",
                    t.route_key
                );
            }
        };
        if !current() {
            superseded();
            continue;
        }
        // The admission this symbol was last sent with in THIS round, which a commit's confirmed leverage
        // restates (`restate_leverage`).
        let mut latest: Option<SymbolAdmission> = None;
        if !m.told {
            let admission = match facts.admission(&t.route_key, &t.venue, &t.symbol) {
                Ok(admission) => admission,
                Err(why) if t.flat && is_gone(&why) => {
                    give_back(t, why.text(), facts, &mut send);
                    continue;
                }
                Err(why) => {
                    if !loud {
                        tracing::debug!(
                            route_key = %t.route_key, symbol = %t.symbol,
                            "retry: could not refresh `{}` in `{}`'s engine yet ({why})",
                            t.symbol, t.route_key
                        );
                    } else if is_busy(&why) {
                        tracing::warn!(
                            route_key = %t.route_key, symbol = %t.symbol,
                            "boot retry: `{}` in `{}`'s engine is rate-limited ({why}); paced, \
                             retrying",
                            t.symbol, t.route_key
                        );
                    } else {
                        tracing::error!(
                            route_key = %t.route_key, symbol = %t.symbol,
                            "boot retry: could not refresh `{}` in `{}`'s engine yet ({why})",
                            t.symbol, t.route_key
                        );
                    }
                    first_why.get_or_insert(why.to_string());
                    left.push(m.clone());
                    continue;
                }
            };
            if !current() {
                superseded();
                continue;
            }
            // ⚠ The generation check above and this send are not atomic: `send` may block on the
            // core's lossless lane, and a command that lands late folds into whichever engine exists
            // under the key at dequeue. Only reachable if this thread stalls in `send` across a whole
            // stop, settle and start of the account (`crates/vike-tradehub/CLAUDE.md`); not redesigned.
            send(Command::AdmitSymbol(Box::new(admission.clone())));
            facts.admitted(&t.route_key, &t.venue, &t.symbol);
            latest = Some(restate_leverage(facts, admission, &mut send));
        }
        if !current() {
            superseded();
            continue;
        }
        // The daemon's commit, with its outcome: `admitted` swallows a stalled commit (a warning), and a
        // price that succeeds on a probe lookup would then count the symbol healed while the reconcile
        // client was never told it. Idempotent, so a symbol just admitted above may be told twice.
        if let Err(why) = facts.tell_daemon(&t.route_key, &t.venue, &t.symbol) {
            if loud {
                tracing::error!(
                    route_key = %t.route_key, symbol = %t.symbol,
                    "boot retry: the gatewayhub was not told `{}` is admitted yet ({why}); its restored \
                     orders stay HELD",
                    t.symbol
                );
            } else {
                tracing::debug!(
                    route_key = %t.route_key, symbol = %t.symbol,
                    "retry: the gatewayhub was not told `{}` is admitted yet ({why})",
                    t.symbol
                );
            }
            first_why.get_or_insert(why);
            left.push(ReadmitMiss { told: true, ..m.clone() });
            continue;
        }
        if !current() {
            superseded();
            continue;
        }
        // The commit just made the venue hold the figure again, so what it confirmed now is what the
        // engine should carry: restate against the admission sent above, or, for a symbol whose first
        // admission went out in an earlier pass, against the facts the source holds. A source that
        // confirmed nothing is not asked a thing.
        if facts.confirmed_leverage(&t.route_key, &t.symbol).is_some()
            && let Some(base) =
                latest.or_else(|| facts.admission(&t.route_key, &t.venue, &t.symbol).ok())
        {
            restate_leverage(facts, base, &mut send);
        }
        if !current() {
            superseded();
            continue;
        }
        if let Err(why) = facts.price(&t.route_key, &t.venue, &t.symbol, true) {
            if loud {
                tracing::error!(
                    route_key = %t.route_key, symbol = %t.symbol,
                    "boot retry: no price feed for the re-admitted `{}` yet ({why})",
                    t.symbol
                );
            } else {
                tracing::debug!(
                    route_key = %t.route_key, symbol = %t.symbol,
                    "retry: no price feed for the re-admitted `{}` yet ({why})",
                    t.symbol
                );
            }
            first_why.get_or_insert(why);
            left.push(ReadmitMiss { told: true, ..m.clone() });
        }
    }
    (left, first_why)
}

/// How long [`retry_missed`] waits before each attempt: the boot's bound has passed by then, so a
/// gatewayhub that was slow once has had time. About eight minutes in all.
pub const RETRY_PAUSES: [std::time::Duration; 5] = [
    std::time::Duration::from_secs(5),
    std::time::Duration::from_secs(15),
    std::time::Duration::from_secs(45),
    std::time::Duration::from_secs(120),
    std::time::Duration::from_secs(300),
];

/// How long [`retry_tail`] waits between rounds once the fast schedule ([`RETRY_PAUSES`]) has ended. A
/// closed market (a forex weekend: the quote-currency rate cannot be had, the venue's schedule says
/// "edits only") outlives eight minutes by days, so the retry keeps going at this slow pace for as long as
/// the process lives and a symbol is still missing.
pub const TAIL_PAUSE: Duration = Duration::from_secs(600);

/// The tail says which symbols are still missing every this many rounds: once an hour at [`TAIL_PAUSE`].
const TAIL_SAY_EVERY: u32 = 6;

/// Say, loudly, which symbols the boot could not re-admit and what that costs while it stands.
pub fn say_missed(missed: &[ReadmitMiss]) {
    let names: Vec<String> =
        missed.iter().map(|m| format!("{} {}", m.target.route_key, m.target.symbol)).collect();
    tracing::error!(
        symbols = ?names,
        "boot: these symbols were NOT re-admitted ({}): the reconcile client was not told them, so \
         any restored resting order on them is HELD (not adopted, not declared gone) and HALT or \
         cancel-all may not reach it. The boot asks again on a backoff (five rounds in about eight \
         minutes, then every {} minutes while the node runs); an order on the symbol admits it too, \
         and so does a restart",
        names.join(", "),
        TAIL_PAUSE.as_secs() / 60
    );
}

/// Say, at `warn`, that `before` symbols were missing and `left` are still: nothing is said when none healed.
fn say_healed(before: usize, left: usize) {
    let healed = before.saturating_sub(left);
    if healed > 0 {
        tracing::warn!(healed, left, "boot: re-admitted symbols the boot had missed");
    }
}

/// **Heal a transient boot miss without a restart**: ask again for `missed` after each of `pauses`
/// (`wait` sleeps and answers `false` when the process is stopping), through `attempt`, which runs the
/// normal [`readmit_and_price`] at the runtime bound (the boot window is over) and answers what is STILL
/// missing, or `None` when the facts source is gone. Ends the moment nothing is missing. Returns what
/// is still missing, said at `error` when it is not empty: that line is the end of the FAST schedule, and
/// [`retry_tail`] (which `drive` runs next) keeps asking at the slow pace.
pub fn retry_missed(
    mut missed: Vec<ReadmitMiss>,
    pauses: &[std::time::Duration],
    deadline: std::time::Instant,
    mut wait: impl FnMut(std::time::Duration) -> bool,
    mut attempt: impl FnMut(&[ReadmitMiss]) -> Option<Vec<ReadmitMiss>>,
) -> Vec<ReadmitMiss> {
    let mut stopped = false;
    for &pause in pauses {
        if missed.is_empty() || std::time::Instant::now() >= deadline {
            break;
        }
        if !wait(pause) {
            stopped = true;
            break;
        }
        let Some(left) = attempt(&missed) else {
            stopped = true;
            break;
        };
        say_healed(missed.len(), left.len());
        missed = left;
    }
    if !missed.is_empty() && !stopped {
        let names: Vec<String> =
            missed.iter().map(|m| format!("{} {}", m.target.route_key, m.target.symbol)).collect();
        tracing::error!(
            symbols = ?names,
            "boot: STILL not re-admitted ({}): restored resting orders on them stay HELD and HALT or \
             cancel-all may not reach them until a retry heals it, an order on the symbol admits it or \
             the node restarts; the boot keeps asking every {} minutes while the node runs and says \
             what is still missing once an hour",
            names.join(", "),
            TAIL_PAUSE.as_secs() / 60
        );
    }
    missed
}

/// **The slow tail after [`retry_missed`]'s fast schedule**: ask again for `missed` after every `pause`
/// (`wait` sleeps and answers `false` when the process is stopping), through `attempt`, which answers what is
/// STILL missing and the first reason a symbol was left (`None` when the facts source is gone). It has NO
/// deadline and NO round limit: it ends when nothing is missing, when `wait` says stop or when `attempt` says
/// the facts source is gone, so a market that stays closed for a weekend does not leave the symbols un-admitted
/// until a restart.
///
/// QUIET by design: `attempt` is a round that logs at `debug` ([`retry_round_quiet`]), because the ERROR said
/// once at the end of the fast schedule already told the operator, and a line per symbol per round would be
/// hundreds over a weekend. What it says: a heal at `warn` (the same line the fast schedule says), and every
/// 6th round (`TAIL_SAY_EVERY`), while something is still missing, ONE `warn` naming what is missing and why.
/// Returns what is still missing.
pub fn retry_tail(
    mut missed: Vec<ReadmitMiss>,
    pause: Duration,
    mut wait: impl FnMut(Duration) -> bool,
    mut attempt: impl FnMut(&[ReadmitMiss]) -> Option<(Vec<ReadmitMiss>, Option<String>)>,
) -> Vec<ReadmitMiss> {
    let mut rounds = 0u32;
    while !missed.is_empty() && wait(pause) {
        let Some((left, why)) = attempt(&missed) else { break };
        say_healed(missed.len(), left.len());
        missed = left;
        rounds += 1;
        if !missed.is_empty() && rounds.is_multiple_of(TAIL_SAY_EVERY) {
            let names: Vec<String> = missed
                .iter()
                .map(|m| format!("{} {}", m.target.route_key, m.target.symbol))
                .collect();
            tracing::warn!(
                symbols = ?names, why = why.as_deref().unwrap_or("-"), rounds,
                "boot: still not re-admitted after {rounds} slow retries ({}); first reason: {}; \
                 the boot keeps asking every {} minutes",
                names.join(", "),
                why.as_deref().unwrap_or("none given"),
                pause.as_secs() / 60
            );
        }
    }
    missed
}

/// **The retry thread's loop**, apart from the thread so a test can drive it and count: [`retry_missed`]
/// (the fast schedule, `deadline`-bounded, rounds LOUD) and then, while anything is still missing, the
/// slow [`retry_tail`] every `tail` (rounds quiet, no deadline). Both watch `stop` in their waits (slices
/// of 250 ms; `false` = the process is stopping) and upgrade `facts` for each attempt (a dropped registry
/// ends it); `run` gets the live registry, the round and whether the round is `loud`, and answers what is
/// left and the first reason a symbol was left.
fn drive(
    missed: Vec<ReadmitMiss>,
    facts: &std::sync::Weak<
        crate::venue_backend::gateway::trade_lease_registry::TradeLeaseRegistry,
    >,
    stop: &std::sync::atomic::AtomicBool,
    pauses: &[Duration],
    tail: Duration,
    deadline: Instant,
    mut run: impl FnMut(
        &crate::venue_backend::gateway::trade_lease_registry::TradeLeaseRegistry,
        &[ReadmitMiss],
        bool,
    ) -> (Vec<ReadmitMiss>, Option<String>),
) -> Vec<ReadmitMiss> {
    use std::sync::atomic::Ordering::Relaxed;
    let mut wait = |pause: Duration| {
        let until = Instant::now() + pause;
        while Instant::now() < until {
            if stop.load(Relaxed) {
                return false;
            }
            std::thread::sleep(Duration::from_millis(250).min(pause));
        }
        !stop.load(Relaxed)
    };
    let missed = retry_missed(missed, pauses, deadline, &mut wait, |round| {
        let facts = facts.upgrade()?;
        Some(run(&facts, round, true).0)
    });
    // Nothing left, or the registry is gone (the fast schedule ended on `None`): no tail to wait out.
    if missed.is_empty() || facts.strong_count() == 0 {
        return missed;
    }
    retry_tail(missed, tail, &mut wait, |round| {
        let facts = facts.upgrade()?;
        Some(run(&facts, round, false))
    })
}

/// **The boot's retry thread** (`boot-readmit-retry`): [`retry_missed`] over [`retry_round`], then
/// [`retry_tail`] every [`TAIL_PAUSE`] while a symbol is still missing, wired to the process. It holds the
/// facts registry by `Weak` (a dropped registry ends it), sleeps in slices that watch `stop` (the
/// process's stop flag), sends through `send` (the core's lossless command door), and opens a bounded round
/// on the registry for each attempt (`TradeLeaseRegistry::begin_round`: the boot's bound on every gatewayhub round
/// trip of THIS thread, a silent lease paid once per round and per route key). The thread returns what was
/// still missing, which is nothing but a stop or a dropped registry once it has gone past the fast rounds.
///
/// ⚠ The FAST schedule's deadline (every pause, two bounds per round, a minute) is checked only BEFORE each
/// wait, so the fast rounds can overrun it by one pause (at most the last, 300 s) plus one round, and a round
/// with K silent route keys costs about K bounds. The price subscription (`subscribe_ticks` on the
/// datahub dial) is outside the round bound. The deadline gates the five fast rounds ONLY: the tail has no
/// deadline (a closed market outlives any), and ends with the process.
///
/// # Errors
/// The thread could not be spawned.
pub fn spawn_retry(
    missed: Vec<ReadmitMiss>,
    facts: &std::sync::Arc<crate::venue_backend::gateway::trade_lease_registry::TradeLeaseRegistry>,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    pauses: &'static [Duration],
    send: impl FnMut(Command) + Send + 'static,
) -> std::io::Result<std::thread::JoinHandle<Vec<ReadmitMiss>>> {
    let bound = facts.round_bound();
    let deadline = Instant::now()
        + Duration::from_secs(60)
        + pauses.iter().sum::<Duration>()
        + bound * 2 * u32::try_from(pauses.len()).unwrap_or(u32::MAX);
    let weak = std::sync::Arc::downgrade(facts);
    let mut send = send;
    std::thread::Builder::new().name("boot-readmit-retry".into()).spawn(move || {
        drive(missed, &weak, &stop, pauses, TAIL_PAUSE, deadline, |facts, pending, loud| {
            let _bounded = facts.begin_round();
            round(pending, facts, &mut send, loud)
        })
    })
}

#[cfg(test)]
#[path = "readmit_tests.rs"]
mod readmit_tests;
