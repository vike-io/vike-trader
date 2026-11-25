//! The dispatcher — the ONE place a drained UI order intent becomes a [`vike_execution::Command`],
//! and therefore the ONE place the local [`vike_tradehub_client::order_checks`] preview cap is applied.
//!
//! ## Why this module exists
//! The per-frame "fold the drained tool-window intents onto the command lane" block lived inline in
//! `vike-app`'s `main.rs`. That file was compiled by NOTHING then: the `justfile`'s `ci_crates`
//! omitted `vike-app` (so `just windows-check` skipped it too) and `xtask/src/ci/tables/roster.rs`'s
//! `EXCLUDE_FROM_CI` listed it. (The shell, `vike-desktop` now, is still outside that roster; the
//! `app-check` job has checked and clippy-gated it since, and executes none of this logic.) The
//! consequence was a real hole — of the five submit paths, only
//! three (the DOM ladder `Place`, the Polymarket cockpit `Submit`, the Deribit options confirm
//! ticket) called [`vike_tradehub_client::order_checks::validate_with_value`]. **The Trade window — the manual
//! order-entry panel a human types into — called neither `validate` nor `validate_with_value`,
//! and neither did its TP+SL bracket sub-path or the DOM's Close/Reverse exit.** Those orders went
//! straight to the command lane with no local notional cap at all, bounded only by the venue-side
//! `vike_execution::RiskGate`.
//!
//! Patching `main.rs` in place would have been exactly as unverified as the bug. This is the same
//! lesson [`order`]'s own module doc records (its notional-MULTIPLIER bug — the UI cap
//! under-measuring options and inverse-perp notional, so it PASSED orders it should have blocked —
//! shipped for the same reason). So the DECISION moved here, into a crate CI actually compiles and
//! tests, and `main.rs` keeps only the I/O: drain the intents in, hand the plan's commands to
//! `Dispatch::send`, log the rejects.
//!
//! ## The structural gate
//! Validation is not a step a future path can forget to call, because `OrderEntryDispatchPlanner::admit` (one
//! order) and `OrderEntryDispatchPlanner::admit_bracket` (a TP+SL bracket, validated leg by leg and refused
//! atomically) are the ONLY functions in this module that may push an order-WRITE command, and
//! each always validates; `OrderEntryDispatchPlanner::pass_through` carries what is not an order write.
//! Three CI tests hold that shape:
//!
//!  * `every_submit_source_is_capped` iterates [`OrderEntryDispatchSource::ALL`] and drives each path with an
//!    over-cap order — a NEW variant fails to compile in [`OrderEntryDispatchSource::label`]'s no-wildcard
//!    `match` and in the test's own no-wildcard input builder until it is handled.
//!  * `order_intent_capping_is_classified` matches EXHAUSTIVELY over `vike_execution::OrderIntent`, so a
//!    new order verb cannot appear without a deliberate Capped / NotAnOrderWrite / exempt-with-
//!    reason classification (the allowlist-with-a-reason discipline `settings_registry.rs` and
//!    `duplicate_shape_gate.rs` use).
//!  * every test in this module asserts the OUTPUT invariant via `tests::assert_plan_respects_limits`:
//!    every emitted `Submit`/`Bracket` command satisfies the cap. A future path that emits an
//!    unvalidated over-cap order fails the moment any test drives it.
//!
//! ## What is deliberately NOT capped, and why
//!  * **Cancels** are not order writes — a cancel only reduces exposure.
//!  * **`OrderIntent::Modify`** (the ladder's drag-to-reprice, the DOM's before it) is a REAL
//!    residual hole, declared rather
//!    than silently closed: repricing a resting order changes its notional, but the intent carries
//!    only `{order_client_id, new_price}` — capping it means resolving the resting order's quantity out of the
//!    snapshot and deciding what happens to an order that no longer fits, which is a behavior
//!    change, not an extraction. `order_intent_capping_is_classified` names it explicitly so the
//!    exemption is visible in CI rather than being an omission nobody wrote down.
//!  * **Market orders carry no price**, so `validate` measures no notional for them — the venue
//!    `RiskGate` owns that, exactly as [`vike_tradehub_client::order_checks::validate_with_value`] documents.
//!    A market entry inside a BRACKET is still notional-capped through the bracket's take-profit
//!    leg (see `OrderEntryDispatchPlanner::admit_bracket`).
//!
//! ## Every Trade window intent carries its own address
//! The Trade window ([`crate::trade_window`]) trades the venue, account and symbol the trader picked
//! (docs/superpowers/specs/2026-09-30-trade-window-design.md §4.2), so every intent arrives as a
//! [`TradeWindowAddress`] beside its [`TradeWindowAction`], and no order is addressed to the snapshot's primary
//! market any more. The old ticket's `plan_trade` (deleted with it) sent every order to
//! `snapshot.venue`/`snapshot.symbol`, the PRIMARY engine's pair, and its own doc named the misroute that
//! followed — a bybit-charting operator's BUY reaching binance, possibly a LIVE binance engine.
//! That path is gone. [`tradable`] refuses an address the account's engine does not trade — or
//! cannot take an order on now, a stopped core or a halted account — BEFORE any command, and the
//! three bracket rules refuse what `vike_model::OrderBracket` cannot carry honestly: an engine whose
//! lane cannot hold the stop-loss (the node's own rule, [`bracket_lane_holds_stop`]), a stop entry, a
//! reduce-only entry. A bracket NAMES its account like a plain order does ([`wire_account`]); a node
//! without `FEATURE_BRACKET_ACCOUNT` is refused client-side, in words, and never misrouted.
//!
//! ⚠ **An address whose account the server does not run is never tradable**, and that is the rule
//! the rest leans on. An account-less order for a venue whose only engine is labelled finds no
//! engine with the bare venue as its route key, and the core then falls back to engine 0
//! (`crates/vike-core/src/runtime/routing.rs`'s `route_of`, read by its caller's `unwrap_or(0)`) —
//! another account's book. So the trader would click Buy on the default account and the order
//! would fill on the labelled one. [`tradable`] therefore requires a block whose route key is the
//! address's, and only the PRIMARY block may stand in for a symbol list an older node did not
//! publish.
//!
//! An address with an EMPTY venue or symbol (a window seeded from the observer's pre-first-frame
//! placeholder, which carries empty strings) is refused with
//! [`OrderEntryDispatchRejectReason::NoRoutableMarket`] before any of that is asked.

pub mod order;
mod planner;

use crate::cockpit::CockpitCommand;
use crate::option_chain::ticket::OptionChainTicket;
use crate::trade_window::TradeWindowAction;
use crate::trade_window::address::TradeWindowAddress;
use crate::trade_window::symbol_follow;
use planner::OrderEntryDispatchPlanner;
use vike_execution::{Command, CoreSnapshot, OrderIntent};
use vike_model::accounts::account_keys::AccountLabel;
use vike_model::orders::client_id::{
    OrderClientIdAccountCode, OrderClientIdGenerator, OrderClientIdIntent, OrderClientIdPlan,
    OrderClientIdShape,
};
use vike_tradehub_client::order_checks::{OrderLimits, OrderLimitsError};

impl TradeWindowAddress {
    /// The account this address names, with the default account spelled `None` whichever way it
    /// arrived.
    fn named_account(&self) -> Option<&AccountLabel> {
        named(self.account.as_ref())
    }
}

/// `account` with the default account spelled `None` — the one normalisation every comparison in
/// this module makes, so a reader that rebuilt a wire `DEFAULT` as `Some(Default)` and one that
/// left it absent cannot disagree about which book an order is in.
fn named(account: Option<&AccountLabel>) -> Option<&AccountLabel> {
    account.filter(|l| !l.is_default())
}

/// Whether a window at `address` may place an order on `snapshot` now (spec §4.3): the core has not
/// stopped, the account's engine is not HALTED, and it trades the symbol. `false` for an empty
/// venue or symbol. `untradable` is the same rule with its reason, and the one both read.
pub fn tradable(snapshot: &CoreSnapshot, address: &TradeWindowAddress) -> bool {
    !(address.venue.is_empty() || address.symbol.is_empty())
        && untradable(snapshot, address).is_none()
}

/// Why `snapshot` takes no order at `address` (a non-empty address), or `None` when it does — THE rule
/// the dispatcher refuses by and the Trade window's glue gates its controls on ([`tradable`]):
///
/// * **a core that has stopped** (`CoreSnapshot::fault`: a handler panicked and the core is in its
///   safe state) takes nothing — [`OrderEntryDispatchRejectReason::CoreStopped`];
/// * **the account must have an engine** (a block whose route key is the address's): an address
///   whose account the server does not run is never tradable, because an account-less order on a
///   venue whose only engine is labelled reaches engine 0 (see the module doc);
/// * **a HALTED engine** (`vike_model::TradingState::Halted`, the kill switch) takes nothing from a
///   window — [`OrderEntryDispatchRejectReason::AccountHalted`]. The core would still admit a reduce its
///   position covers, but the window offers no Close or Reverse there, and the dispatcher agrees
///   with it (M-6 of the final review, slice B: this lived in the glue alone). A REDUCING engine
///   still trades: the core admits what reduces and refuses the rest in its own words;
/// * **a perpetual at the default account of a venue whose perpetual lane is ANOTHER account's**
///   ([`perp_lane_account`]) is refused — [`OrderEntryDispatchRejectReason::PerpLaneIsAnotherAccount`]: the node
///   would fill it in that account's book, which this window does not read;
/// * **the engine takes the symbol**: the symbol it was started on (and a combo's extras), or ANY
///   symbol when the account is one the Trade window lets follow the picked symbol
///   ([`symbol_follow::one_symbol`] answers `None`: a demo or live account whose block says the
///   node admits symbols on demand). Whether the venue's CATALOG lists the symbol is the window's to check, because the
///   dispatcher holds no catalog (`vike_app_core::ui::windows`'s `trade_window_parts`); a block that names no
///   symbol (an older node) trades only the snapshot's primary market, and only if it IS the primary
///   block — never wider than the old ticket, which traded exactly that.
fn untradable(
    snapshot: &CoreSnapshot,
    address: &TradeWindowAddress,
) -> Option<OrderEntryDispatchRejectReason> {
    if snapshot.fault.is_some() {
        return Some(OrderEntryDispatchRejectReason::CoreStopped);
    }
    let key = address.route_key();
    let Some((i, vb)) =
        snapshot.portfolio.venues.iter().enumerate().find(|(_, v)| v.route_key == key)
    else {
        return Some(OrderEntryDispatchRejectReason::NotTraded);
    };
    if vb.trading_state == vike_model::TradingState::Halted {
        return Some(OrderEntryDispatchRejectReason::AccountHalted);
    }
    if perp_lane_account(snapshot, address).is_some() {
        return Some(OrderEntryDispatchRejectReason::PerpLaneIsAnotherAccount);
    }
    let trades = if vb.symbol.is_empty() {
        i == 0 && address.symbol == snapshot.symbol
    } else {
        vb.trades(&address.symbol) || symbol_follow::one_symbol(vb).is_none()
    };
    (!trades).then_some(OrderEntryDispatchRejectReason::NotTraded)
}

/// **The account that runs the PERPETUAL lane of `address`'s account**, when `address` is a venue's DEFAULT
/// account asked for a perpetual (`.P`) symbol that its own engine cannot trade: the engine of a
/// dual-lane venue (binance, aster) is mounted on ONE lane, and a node can run a second engine over the
/// same row for the other (a binance demo account that states a margin leverage gets `binance#PERP`,
/// `vike_venues::lane_twin`). Its label, read off the published blocks alone: this client holds no
/// account table.
///
/// ⚠ **Why an address like this is refused instead of sent.** The node's door puts a default account's `.P`
/// order on a LANE TWIN on its own (`vike-tradehub`'s `lane_addressed`), so an order sent as the default
/// account would be filled in a book the sender does not look at: its position, markers and Cancel all are
/// read off the DEFAULT route key, and `owns_order` rejects an order whose account is the lane's. The
/// trader would see no position and could buy again. A Trade window therefore never sends that address
/// for the twin: [`lane_twin_account`] makes it address the twin's account, where every lookup, order and
/// cancel is keyed by the right route key. What is left for this refusal (`untradable`) is an address that
/// reaches it anyway, and a lane engine that is NOT the twin (an operator's own account mounted on the
/// perpetual lane, which the door does not reroute to): the order belongs at that account's address.
///
/// `None` for a named account, for a spot symbol, for a default engine that already trades the perpetual
/// lane, for a venue with one lane, and when no other engine of the venue is on the perpetual lane.
pub fn perp_lane_account(snapshot: &CoreSnapshot, address: &TradeWindowAddress) -> Option<String> {
    if address.named_account().is_some()
        || !vike_catalog::on_futures_lane(&address.venue, &address.symbol)
    {
        return None;
    }
    let venues = &snapshot.portfolio.venues;
    let default = venues.iter().find(|v| v.route_key == address.venue)?;
    if default.symbol.is_empty()
        || vike_catalog::engine_lane_holds_stop(&address.venue, &default.symbol)
    {
        return None;
    }
    venues
        .iter()
        .filter(|v| {
            v.venue == address.venue
                && v.route_key != address.venue
                && !v.symbol.is_empty()
                && vike_catalog::engine_lane_holds_stop(&v.venue, &v.symbol)
        })
        .find_map(|v| {
            vike_model::accounts::account_keys::label_of_route_key(&v.venue, &v.route_key)
                .and_then(|label| label.text().map(str::to_string))
        })
}

/// **The account a Trade window addresses for a perpetual at a venue's DEFAULT account, when the venue's
/// LANE TWIN is up** (`vike_venues::lane_twin`: the second engine over the same account row for the lane
/// its default engine does not trade, `binance#PERP` on `BTCUSDT.P`): `Some(label)` is the twin's. `None`
/// leaves the account as it was picked: a named account, a spot symbol, a venue without a twin row, a twin
/// that is not up, and an account text that is no label.
///
/// This is `vike-tradehub`'s `lane_addressed` decision (the door puts a default account's `.P` order on the
/// twin on its own), asked of the published blocks with the door's own test of "up" (the twin's route key
/// AND its mount symbol). The window addresses the twin EXPLICITLY instead of leaving the door to reroute,
/// and reads the block, the position, the markers and the working orders by that account's route key, so
/// what it shows is the book the order fills in. A caller that builds a window's address must go through
/// this, once per frame, and share the answer (`WindowView::trade_lane`); a lookup keyed by the picked
/// account would show the default book while the orders land in the twin's.
///
/// ⚠ **The label and the mount symbol do not make a twin.** The engine must run at a tier the table opens the
/// twin at (`AccountRowLaneTwin::tiers`), and the account directory must not list a REAL account under the label
/// (`directory`: the node's active stored rows; a derived twin is in no row, and `derive_lane_twins` stands
/// down for a real account that carries the label). For a `demo` engine a directory that did not answer
/// (`None`) judges by the blocks alone, as it always did. **For a `live` engine the absence of a real row is
/// not enough: the directory must have ANSWERED (`Some`) and list no account under the label** (review r2,
/// finding 7): a `live` engine is real money on the account's own keys, and an older real `PERP` account
/// mounted on the perpetual symbol has the derived twin's label, symbol and tier to the letter, so "the node
/// said nothing" must not read as "it is the derived twin" and send the default account's perpetual order to
/// another real account's keys.
pub fn lane_twin_account(
    snapshot: &CoreSnapshot,
    venue: &str,
    account: Option<&str>,
    symbol: &str,
    directory: Option<&vike_tradehub_client::wire::AccountDirectoryWire>,
) -> Option<&'static str> {
    let twin = vike_venues::lane_twin(venue)?;
    let address = TradeWindowAddress::parse(venue, account, symbol)?;
    if address.named_account().is_some()
        || !vike_catalog::on_futures_lane(venue, symbol)
        || directory.is_some_and(|dir| {
            dir.accounts.iter().any(|a| a.venue == venue && a.label.as_deref() == Some(twin.label))
        })
    {
        return None;
    }
    snapshot
        .portfolio
        .venues
        .iter()
        .any(|b| {
            twin.on_lane(&b.symbol)
                && b.tier.is_some_and(|tier| {
                    twin.tiers.contains(&tier)
                        && (tier != vike_model::AccountTier::Live || directory.is_some())
                })
                && b.route_key.strip_prefix(venue).and_then(|rest| rest.strip_prefix('#'))
                    == Some(twin.label)
        })
        .then_some(twin.label)
}

/// Whether a TP/SL bracket at `address` is on a symbol its engine was started on. The node judges what
/// the engine's lane can hold from the ENGINE's symbol ([`bracket_lane_holds_stop`]), so on any other
/// symbol that verdict is about a different instrument: a stop-loss the venue rejects after the entry
/// filled would leave a position the trader believes is protected. A bracket there is refused, for
/// now. `true` where no block answers ([`tradable`] has refused the address first) and for a block
/// that names no symbol (an older node, never judged here).
pub fn bracket_on_mounted_symbol(snapshot: &CoreSnapshot, address: &TradeWindowAddress) -> bool {
    let key = address.route_key();
    snapshot
        .portfolio
        .venues
        .iter()
        .find(|v| v.route_key == key)
        .is_none_or(|vb| vb.symbol.is_empty() || vb.trades(&address.symbol))
}

/// Whether the engine a TP/SL bracket at `address` reaches can hold its stop-loss: the node's own rule
/// (`vike_catalog::engine_lane_holds_stop`), read off the ENGINE's mounted symbol
/// (`VenueBlock::symbol`, the one its adapter signs every order on), never the address's. `false`
/// on the shipped daemon's spot binance engine (I-1 of the final review, slice B): the node refuses
/// every bracket there, and the window now says so before the click and refuses it here, on its own
/// strip.
///
/// An engine that does not say what it trades (an older node, whose block names no symbol) is NOT
/// judged: its lane is unknown, and such a node predates the bracket command, which the client
/// refuses before the wire in its own words. No block at all is not judged either: [`tradable`]
/// has refused the address first.
pub fn bracket_lane_holds_stop(snapshot: &CoreSnapshot, address: &TradeWindowAddress) -> bool {
    let key = address.route_key();
    snapshot.portfolio.venues.iter().find(|v| v.route_key == key).is_none_or(|vb| {
        vb.symbol.is_empty() || vike_catalog::engine_lane_holds_stop(&vb.venue, &vb.symbol)
    })
}

/// Why a window shows no own-order markers and sends no account-scoped cancel (Ruling R9).
pub const ORDERS_UNATTRIBUTED_WHY: &str =
    "This node does not say which account an order belongs to.";

/// Whether `snapshot` lets a window tell which account each order of `venue` belongs to (Ruling R9). A
/// node publishes every order's account exactly when it publishes the `tier` of its blocks (PR 1
/// added the two together, the tier then under the key `mode`, which decision 0156 renamed; a node
/// that still sends only `mode` reads as an older node here, which fails closed); a node that
/// publishes no `tier` names no order's account, so every order there arrives as the default
/// account's.
///
/// With ONE block that is still answerable, but not from the order: every order of the venue is
/// that one engine's, whatever account the order itself says (on an older node whose only engine is
/// labelled, it says none). [`window_orders`] attributes them by the block for exactly that case.
/// With more than one block and no `tier`, nothing can tell the accounts' orders apart.
///
/// ⚠ **Nor where this client cannot READ an account the node names** (M-9 of the final review,
/// slice B). An order's wire label that this client cannot parse (a label-grammar skew between node
/// and client) reaches `OrderView::account` as `None` — the type cannot spell "unknown"
/// (`vike_tradehub_client::observe_decode::map_order`) — and would read as the DEFAULT account's order.
/// Its engine's block carries the same label in its route key, which is how this sees it: a venue
/// with more than one block, one of whose route keys names no account this client reads
/// (`vike_model::accounts::account_keys::label_of_route_key`), attributes no order at all. Fail-closed for the
/// whole venue, because the default account's own orders and the unreadable account's arrive
/// spelled alike; the orders stay in the snapshot, so the Account window still lists them.
pub fn orders_name_their_account(snapshot: &CoreSnapshot, venue: &str) -> bool {
    let blocks: Vec<_> = snapshot.portfolio.venues.iter().filter(|v| v.venue == venue).collect();
    let readable = blocks.iter().all(|b| {
        vike_model::accounts::account_keys::label_of_route_key(&b.venue, &b.route_key).is_some()
    });
    blocks.len() <= 1 || (readable && blocks.iter().any(|b| b.tier.is_some()))
}

/// **Whether an order belongs to a window's address, live or done** — the attribution half of
/// [`window_orders`], and the ONE rule for it: the window's status strip follows the newest order
/// this answers yes for (including one that filled before a snapshot showed it working), and its
/// markers and every account-scoped cancel read [`window_orders`], which is this plus liveness. An
/// order is the address's when it is:
///
/// * **on the address's venue and symbol, and in its account** — a second account's orders on the
///   same symbol are someone else's book (`Some(Default)` and `None` are the same account);
/// * **in an account the server runs** (a block carries the address's route key). An account it
///   does not run owns no orders: on an older node the orders of a venue whose only engine is
///   labelled name no account and would otherwise read as the default account's;
/// * **on a venue whose orders can be attributed at all** ([`orders_name_their_account`]). On a
///   venue with ONE block and no `tier` (an older node), every order of the venue is that block's,
///   whatever account the order names.
///
/// ⚠ **FAIL-CLOSED where attribution is impossible.** On a venue with several blocks and no `tier`
/// every order arrives naming no account, so it would read as the DEFAULT account's: the default
/// window would mark, and cancel, every account's orders. Here that answers NO for every order, so a
/// caller that forgets to ask [`orders_name_their_account`] gets nothing rather than the wrong book.
/// A caller still asks it first wherever it must say WHY there is nothing
/// ([`ORDERS_UNATTRIBUTED_WHY`]), as the dispatcher and the Trade window's glue do.
pub fn owns_order<'a>(
    snapshot: &'a CoreSnapshot,
    address: &'a TradeWindowAddress,
) -> impl Fn(&vike_execution::OrderView) -> bool + 'a {
    let key = address.route_key();
    let blocks: Vec<_> =
        snapshot.portfolio.venues.iter().filter(|v| v.venue == address.venue).collect();
    let attributed = orders_name_their_account(snapshot, &address.venue);
    let runs = blocks.iter().any(|b| b.route_key == key);
    let by_block = matches!(blocks.as_slice(), [only] if only.tier.is_none());
    let account = address.named_account();
    move |o: &vike_execution::OrderView| {
        attributed
            && runs
            && o.venue == address.venue
            && o.symbol == address.symbol
            && (by_block || named(o.account.as_ref()) == account)
    }
}

/// **A window's own resting orders** — the ONE rule the window's ladder markers, its "Cancel all N"
/// count and the dispatcher's Cancel all / Cancel side share, so what a trader sees marked is what a
/// cancel pulls: the orders [`owns_order`] gives the address that are also **live**
/// (`vike_model::OrderStatus::is_live`, the engine's own cancel-send gate). The snapshot publishes the
/// whole order registry and nothing removes a done order from it, so without the liveness filter a
/// session's filled and cancelled orders would ride along: as markers on the ladder, and as one
/// `Cancel` each, queued AHEAD of the live ones behind the client's bounded command queue and the
/// node's rate limit — the live orders, newest, are the ones that would not get through.
///
/// Fail-closed exactly where [`owns_order`] is: on a venue whose orders cannot be attributed it
/// yields nothing.
pub fn window_orders<'a>(
    snapshot: &'a CoreSnapshot,
    address: &'a TradeWindowAddress,
) -> impl Iterator<Item = &'a vike_execution::OrderView> + 'a {
    let owns = owns_order(snapshot, address);
    snapshot.orders.iter().map(|o| &**o).filter(move |o| owns(o) && o.status.is_live())
}

/// The account a command for `address` names on the wire (Ruling R8). A labelled account names itself.
/// The DEFAULT account is named `DEFAULT` only where its venue runs more than one engine: there an
/// account-less submit is refused as ambiguous (`Some(Default)` and `None` route differently), while
/// on a single-account node `None` is unambiguous and is the only spelling an older node accepts.
///
/// A TP/SL bracket names its account the same way (`vike_model::OrderBracket::account`), so a
/// bracket on a lane twin's `PERP` address reaches that engine, and a single-engine node still
/// receives the account-less frame it always did.
pub fn wire_account(snapshot: &CoreSnapshot, address: &TradeWindowAddress) -> Option<AccountLabel> {
    match address.named_account() {
        Some(label) => Some(label.clone()),
        None => (snapshot.portfolio.venues.iter().filter(|v| v.venue == address.venue).count() > 1)
            .then_some(AccountLabel::Default),
    }
}

/// What one unit of `symbol` is worth in the book an order naming `account` reaches: the block whose route
/// key is that account's (`DEFAULT` and `None` are the venue's own key). Two accounts of one venue can mount
/// different contracts (a linear `BTCUSDT` and an inverse `BTCUSD` on bybit), so the venue alone does not say.
/// A node that publishes no such block keeps the venue's first block (`CoreSnapshot::contract_value_of`).
/// The Trade window's ticket sizes with the same answer (`vike_app_core::ui::windows::trade_window::window`), so what it
/// sends is what this preview judges.
pub fn contract_value_at(
    snapshot: &CoreSnapshot,
    venue: &str,
    account: Option<&AccountLabel>,
    symbol: &str,
) -> vike_model::ContractValue {
    block_at(snapshot, venue, account)
        .map_or_else(|| snapshot.contract_value_of(venue, symbol), |b| b.contract_value_of(symbol))
}

/// The block an order naming `account` at `venue` reaches: the one whose route key is that account's
/// (`DEFAULT` and `None` are the venue's own key). `None` where the node publishes no such block.
fn block_at<'s>(
    snapshot: &'s CoreSnapshot,
    venue: &str,
    account: Option<&AccountLabel>,
) -> Option<&'s vike_execution::VenueBlock> {
    let key = account.map_or_else(
        || venue.to_string(),
        |l| vike_model::accounts::account_keys::route_key_of(venue, l),
    );
    snapshot.portfolio.venues.iter().find(|v| v.route_key == key)
}

/// The id plan of the account `block` is (decision 0270): its published shape and account code, or the
/// full shape and `00` where the node said none (an older node, or no block). A client never carries an
/// instance tag: the GUI's session has none, so a shape's `tag` stamps nothing.
fn order_client_id_plan(block: Option<&vike_execution::VenueBlock>) -> OrderClientIdPlan {
    OrderClientIdPlan::new(
        block.and_then(|b| b.order_client_id_shape).unwrap_or(OrderClientIdShape::FULL),
        block.and_then(|b| b.account_code).unwrap_or(OrderClientIdAccountCode::NONE),
    )
}

/// What an order of `side` does to `block`'s position in `symbol` (`OrderClientIdIntent::of`, decision 0270's
/// 4.6): a LABEL that decides nothing, read by the ONE rule both surfaces mint by (`vike-cli`'s, MCP's and
/// Telegram's is `vike_client::verbs::fill_order_client_id`), so one order labels alike from either: fully funded
/// by [`vike_catalog::books_cash`] (a spot pair, or a venue whose only margin mode is `Cash`, polymarket's
/// shares), and the NET of every leg the account holds in the symbol, a hedge-mode `Long`/`Short` bucket included,
/// `0` for none.
fn order_client_id_intent(
    block: Option<&vike_execution::VenueBlock>,
    venue: &str,
    symbol: &str,
    side: i32,
    reduce_only: bool,
) -> OrderClientIdIntent {
    let net = block.map_or(0.0, |b| {
        b.positions
            .iter()
            .filter(|p| p.symbol == symbol)
            .map(|p| p.position_side.signed_size(p.size))
            .sum()
    });
    OrderClientIdIntent::of(side, reduce_only, vike_catalog::books_cash(venue, symbol), net)
}

/// The order-client-id prefix each submit path stamps on a v1 session's id and on the deterministic
/// spelling (a v2 id says its source in its first letter, `G`, and carries none): `OrderEntryDispatchPlanner::order_client_id`
/// builds `"{prefix}{session}{seq}"` from a v1 session's mint and, with none, the historical
/// `"{prefix}-{next_window_number}-{shot_n}"` of the pre-extraction `format!` sites.
const ORDER_CLIENT_ID_TRADE: &str = "ui";
const ORDER_CLIENT_ID_DOM: &str = "dom";
const ORDER_CLIENT_ID_COCKPIT: &str = "poly";
const ORDER_CLIENT_ID_OPTIONS: &str = "opt";

/// The distinct UI origins that can produce an order WRITE. One variant per submit path, so a
/// reject can name where it came from and the CI roster can drive each path individually.
///
/// Adding a path means adding a variant, which fails to compile in [`OrderEntryDispatchSource::label`]'s
/// no-wildcard `match` and in the roster test's input builder until it is BOTH handled and driven.
///
/// The four Trade window variants keep the names of the paths they replaced (the old ticket and
/// the DOM ladder; Ruling R5 of the Trade window plan) — the LABELS say what they are now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrderEntryDispatchSource {
    /// Trade window ticket (`TradeWindowOrigin::Ticket`) → a plain Market/Limit/Stop order.
    Trade,
    /// Trade window order with TP/SL → an `OrderIntent::Bracket` (OTO entry + OCO exits).
    TradeBracket,
    /// Trade window ladder click (`TradeWindowOrigin::Ladder`) → a Limit or Stop order.
    Dom,
    /// Trade window `TradeWindowAction::ClosePosition` / `TradeWindowAction::Reverse` — a market exit leg.
    DomExit,
    /// Polymarket scalp-cockpit `CockpitCommand::Submit`.
    Cockpit,
    /// Deribit options chain confirm-ticket.
    Options,
}

impl OrderEntryDispatchSource {
    /// EVERY variant. The roster the CI gate iterates — a path missing from here is a path the
    /// gate never drives, so keep it exhaustive (the `label` match below is the compile-time
    /// reminder that a variant exists).
    pub const ALL: &[OrderEntryDispatchSource] = &[
        OrderEntryDispatchSource::Trade,
        OrderEntryDispatchSource::TradeBracket,
        OrderEntryDispatchSource::Dom,
        OrderEntryDispatchSource::DomExit,
        OrderEntryDispatchSource::Cockpit,
        OrderEntryDispatchSource::Options,
    ];

    /// Short human label for the warn log. NO WILDCARD ARM — a new variant is a compile error here.
    pub fn label(self) -> &'static str {
        match self {
            OrderEntryDispatchSource::Trade => "trade window ticket",
            OrderEntryDispatchSource::TradeBracket => "trade window bracket",
            OrderEntryDispatchSource::Dom => "trade window ladder",
            OrderEntryDispatchSource::DomExit => "trade window exit",
            OrderEntryDispatchSource::Cockpit => "Polymarket cockpit",
            OrderEntryDispatchSource::Options => "options ticket",
        }
    }
}

/// Why the dispatch refused to emit an order write.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum OrderEntryDispatchRejectReason {
    /// The local [`vike_tradehub_client::order_checks`] preview rejected it (malformed shape, or over a cap).
    Preview(OrderLimitsError),
    /// The Trade window's address names no routable `(venue, symbol)` — an empty venue or symbol,
    /// as a window seeded before an observer's first `SnapshotFrame` lands carries
    /// (`CoreSnapshotWire::empty()` carries empty strings).
    NoRoutableMarket,
    /// A TP/SL bracket with a Stop entry: `OrderBracket` has no stop entry, and the old ticket sent
    /// such an order at MARKET, losing its trigger. (A Limit entry with no price never reaches the
    /// bracket rules here: [`OrderEntryDispatchRejectReason::MissingPrice`] refuses it first.)
    BracketNeedsMarketOrLimit,
    /// The window's account does not trade the window's symbol, or the server does not run that
    /// account at all (spec §4.3; see [`tradable`]).
    NotTraded,
    /// The node does not say which account an order belongs to, so the window cannot pick its own
    /// orders out (Ruling R9).
    OrdersUnattributed,
    /// A TP/SL bracket on an engine whose LANE cannot hold its stop-loss: an engine mounted on the
    /// spot market of a venue that runs a spot and a perp lane (the shipped daemon's binance
    /// engine). The node refuses it by the same rule ([`bracket_lane_holds_stop`]); refused here,
    /// the refusal reaches the window's own strip (I-1 of the final review, slice B).
    BracketNeedsPerpEngine,
    /// A TP/SL bracket on a symbol the account's engine was not started on (see
    /// [`bracket_on_mounted_symbol`]): the node cannot yet say whether that instrument's lane holds
    /// the stop-loss.
    BracketNeedsMountedSymbol,
    /// A TP/SL bracket on a reduce-only entry: the exits belong to an order that opens or adds.
    BracketNeedsOpeningEntry,
    /// A perpetual (`.P`) symbol at a venue's DEFAULT account whose engine trades the spot lane, while
    /// another account of the venue runs the perpetual lane ([`perp_lane_account`]): the order belongs at
    /// that account's address, where the window can see its position and its orders.
    PerpLaneIsAnotherAccount,
    /// The core has stopped (`CoreSnapshot::fault`): it takes no order from a window ([`tradable`]).
    CoreStopped,
    /// The window's account is HALTED (`vike_model::TradingState::Halted`): it takes no order from a
    /// window, a Close or a Reverse included ([`tradable`]).
    AccountHalted,
    /// A Limit or Stop order with no price. It is refused, never sent at MARKET: the old ticket's
    /// `unwrap_or` habit would have turned it into a market order (or a bracket's market entry) with
    /// the trader's price silently gone. Its own reason rather than the preview's
    /// `MissingLimitPrice`, whose words name a limit order and would mislead for a Stop.
    MissingPrice,
    /// A POST-ONLY order that carries TP/SL exits: a bracket's entry has no lifetime on the wire
    /// (`vike_model::OrderBracket`), so it would go out as a plain entry that takes liquidity. The
    /// ticket refuses the pair first ([`crate::trade_window::ticket::POST_TAKE_PROFIT_STOP_LOSS_WHY`]); this is the
    /// planner's own refusal of a stale or never-offered one.
    PostOnlyHasNoBracket,
}

impl std::fmt::Display for OrderEntryDispatchRejectReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OrderEntryDispatchRejectReason::Preview(r) => write!(f, "{r}"),
            OrderEntryDispatchRejectReason::NoRoutableMarket => {
                write!(f, "the window's address names no venue or symbol yet")
            }
            OrderEntryDispatchRejectReason::BracketNeedsMarketOrLimit => {
                write!(f, "a TP/SL bracket needs a Market or Limit entry")
            }
            OrderEntryDispatchRejectReason::NotTraded => {
                write!(f, "the account does not trade this symbol")
            }
            OrderEntryDispatchRejectReason::OrdersUnattributed => {
                write!(f, "{ORDERS_UNATTRIBUTED_WHY}")
            }
            OrderEntryDispatchRejectReason::BracketNeedsPerpEngine => write!(
                f,
                "a TP/SL bracket on this venue needs an engine on its perp market: the exchange \
                 cannot hold the stop-loss on its spot market"
            ),
            OrderEntryDispatchRejectReason::BracketNeedsMountedSymbol => write!(
                f,
                "a TP/SL bracket is only available on the symbol this account was started on, for now"
            ),
            OrderEntryDispatchRejectReason::BracketNeedsOpeningEntry => {
                write!(f, "a TP/SL bracket needs an order that opens or adds")
            }
            OrderEntryDispatchRejectReason::PerpLaneIsAnotherAccount => write!(
                f,
                "this perpetual symbol is traded by the account that runs this account's perpetual \
                 lane: pick that account in the account menu"
            ),
            OrderEntryDispatchRejectReason::CoreStopped => {
                write!(f, "the server has stopped trading")
            }
            OrderEntryDispatchRejectReason::AccountHalted => {
                write!(f, "trading is halted on this account")
            }
            OrderEntryDispatchRejectReason::MissingPrice => {
                write!(f, "a Limit or Stop order needs a price")
            }
            OrderEntryDispatchRejectReason::PostOnlyHasNoBracket => write!(
                f,
                "a post-only order cannot carry TP/SL: the bracket's entry has no lifetime, so it \
                 would take liquidity"
            ),
        }
    }
}

/// One refused order write, carrying enough to log the same warn `main.rs` used to log inline, and
/// to find the Trade windows it belongs to.
#[derive(Debug, Clone, PartialEq)]
pub struct OrderEntryDispatchReject {
    pub source: OrderEntryDispatchSource,
    pub venue: String,
    pub symbol: String,
    /// The account the refused write named: the window's address's, or the order's own (the
    /// default account may arrive as `Some(AccountLabel::Default)`, see [`wire_account`]). `None`
    /// for the cockpit and the options ticket, which name no account. The shell matches a refusal
    /// to its windows by venue, symbol AND this, so a window on another account of the same symbol
    /// never shows it (`vike_app_core::ui::windows::route_rejects`).
    ///
    /// ⚠ A refusal of a Trade window intent is built by `OrderEntryDispatchPlanner::reject` from the window's
    /// address (or carries that address's account the same way, as `OrderEntryDispatchPlanner::admit_bracket` does).
    /// A reason added later, or ported from the bracket branch, must go through it too: a reject
    /// with a hand-written `None` reaches the default-account window instead of its own.
    pub account: Option<AccountLabel>,
    pub reason: OrderEntryDispatchRejectReason,
}

/// The drained per-frame UI intents, in the order they are folded. Owned because the caller moves
/// its drain buffers in (`TradeWindowAction`/`CockpitCommand` carry owned order client id strings).
#[derive(Debug, Default)]
pub struct OrderEntryDispatchInputs {
    /// Trade window intents, each with its window's address.
    pub trade_actions: Vec<(TradeWindowAddress, TradeWindowAction)>,
    /// Account window working-order ✕ clicks (order-client-ids).
    pub account_cancels: Vec<String>,
    /// Polymarket cockpit intents (venue is always `"polymarket"`).
    pub cockpit_commands: Vec<CockpitCommand>,
    /// Deribit options confirm-tickets.
    pub opt_orders: Vec<OptionChainTicket>,
    /// Deribit options chain working-order-marker cancels.
    pub opt_cancels: Vec<String>,
    /// The node takes a bracket that names its group STEM (`BackendConnection::takes_bracket_stem`): a v2 mint
    /// then stamps one on every TP/SL bracket (`OrderEntryDispatchPlanner::admit_bracket`). `false` sends the bracket
    /// with no id, as before, because the client refuses a stem bracket whole against a node without
    /// `bracket-stem`, and the node mints the legs itself.
    pub bracket_stem: bool,
}

/// The planned frame: commands to fire in order, refusals to log, and the advanced order client id counter.
#[derive(Debug)]
pub struct OrderEntryDispatchPlan {
    /// Fire these in order — `main.rs` hands each to `Dispatch::send`.
    pub commands: Vec<Command>,
    /// Order writes the local preview refused; `main.rs` warns one line per entry.
    pub rejects: Vec<OrderEntryDispatchReject>,
    /// `next_window_number` after this frame's order client id minting. A REJECTED order still consumed its order client id,
    /// exactly as the pre-extraction sites did (they minted before validating), so this counter is
    /// byte-identical to the old `self.next_window_number += 1` sequence.
    pub next_window_number: u32,
}

/// The frame's plan. PURE: no I/O, no env, no clock — inputs + limits + snapshot in, commands out.
///
/// `next_window_number`/`shot_n` are the GUI's window and screenshot-frame counters; the returned
/// [`OrderEntryDispatchPlan::next_window_number`] is what the caller writes back.
///
/// `order_client_ids` is the PROCESS's order-client-id mint, and it decides every id this frame stamps:
///
///  * `Some` v2 session (the running desktop, `OrderClientIdGenerator::new_v2`) → a v2 id (decision
///    0270): source `G` (manual GUI order entry, every panel), no strategy (`0000`), the account code and
///    the id shape of the block the order reaches, and the intent off this frame, minted through the
///    `OrderClientIdPlan` that block's shape and code make (`OrderEntryDispatchPlanner::order_client_id`). At most 21 characters, inside every
///    venue's documented field; a node that publishes no shape gets the full one.
///  * `Some` v1 session → `"{prefix}{session}{seq}"` from `OrderClientIdGenerator`, the
///    mint `vike-cli` and the core use: an 8-hex session drawn per process and a sequence that only
///    grows. ⚠ The id MUST NOT come from `next_window_number`: that is the window counter, it restarts at
///    every launch and `restore_workspace` and the startup arrange set it back to the window count
///    in the middle of a session, so ids built from it repeat, and the node's door refuses a
///    `Submit` whose id an earlier order already carries (`vike-tradehub`'s `order_client_id_reuse_refusal`).
///    The id is alphanumeric, at most 4 + 8 characters plus the sequence's digits, so it fits the
///    tightest venue budget (Binance behind the longest broker code leaves 21; OKX and IBKR cap
///    at 32) for a billion orders, and `strip_broker_order_client_id_prefix` needs a `-`-free id.
///    `an_id_fits_the_tightest_venue_budget` pins it.
///  * `None` (the offscreen/kittest harnesses, `VIKE_SHOT`) → the deterministic historical
///    `"{prefix}-{next_window_number}-{shot_n}"`, so a golden never depends on a random session.
///
/// `next_window_number` advances once per minted id in BOTH modes: it numbers new windows, and
/// `OrderEntryDispatchPlan::next_window_number` is part of the contract the caller depends on.
pub fn plan_dispatch(
    inputs: OrderEntryDispatchInputs,
    limits: &OrderLimits,
    snapshot: &CoreSnapshot,
    next_window_number: u32,
    shot_n: u32,
    order_client_ids: Option<&mut OrderClientIdGenerator>,
) -> OrderEntryDispatchPlan {
    let mut p = OrderEntryDispatchPlanner {
        limits,
        snapshot,
        shot_n,
        next_window_number,
        order_client_ids,
        bracket_stem: inputs.bracket_stem,
        commands: Vec::new(),
        rejects: Vec::new(),
    };

    // ORDER IS LOAD-BEARING: the Trade window's intents in their drain order, then the Account
    // window's cancels, then the cockpit and the options chain — `emission_order_is_preserved`.
    p.plan_trade_window(inputs.trade_actions);
    for order_client_id in inputs.account_cancels {
        p.pass_through(Command::Order(OrderIntent::Cancel(order_client_id)));
    }
    p.plan_cockpit(inputs.cockpit_commands);
    p.plan_options(inputs.opt_orders);
    for order_client_id in inputs.opt_cancels {
        p.pass_through(Command::Order(OrderIntent::Cancel(order_client_id)));
    }

    OrderEntryDispatchPlan {
        commands: p.commands,
        rejects: p.rejects,
        next_window_number: p.next_window_number,
    }
}

#[cfg(test)]
mod tests;
