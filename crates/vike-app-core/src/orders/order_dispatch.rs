//! `order_dispatch` — the ONE place a drained UI order intent becomes a [`vike_exec::Command`],
//! and therefore the ONE place the local [`crate::orders::order_entry`] preview cap is applied.
//!
//! ## Why this module exists
//! The per-frame "fold the drained tool-window intents onto the command lane" block lived inline in
//! `vike-app`'s `main.rs`. That file was compiled by NOTHING then: the `justfile`'s `ci_crates`
//! omitted `vike-app` (so `just windows-check` skipped it too) and `xtask/src/ci/tables/roster.rs`'s
//! `EXCLUDE_FROM_CI` listed it. (The shell, `vike-desktop` now, is still outside that roster; the
//! `app-check` job has checked and clippy-gated it since, and executes none of this logic.) The
//! consequence was a real hole — of the five submit paths, only
//! three (the DOM ladder `Place`, the Polymarket cockpit `Submit`, the Deribit options confirm
//! ticket) called [`crate::orders::order_entry::validate_with_multiplier`]. **The Trade window — the manual
//! order-entry panel a human types into — called neither `validate` nor `validate_with_multiplier`,
//! and neither did its TP+SL bracket sub-path or the DOM's Close/Reverse exit.** Those orders went
//! straight to the command lane with no local notional cap at all, bounded only by the venue-side
//! `vike_exec::RiskGate`.
//!
//! Patching `main.rs` in place would have been exactly as unverified as the bug. This is the same
//! lesson `order_entry`'s own module doc records (its notional-MULTIPLIER bug — the UI cap
//! under-measuring options and inverse-perp notional, so it PASSED orders it should have blocked —
//! shipped for the same reason). So the DECISION moved here, into a crate CI actually compiles and
//! tests, and `main.rs` keeps only the I/O: drain the intents in, hand the plan's commands to
//! `Dispatch::send`, log the rejects.
//!
//! ## The structural gate
//! Validation is not a step a future path can forget to call, because [`Planner::admit`] (one
//! order) and [`Planner::admit_bracket`] (a TP+SL bracket, validated leg by leg and refused
//! atomically) are the ONLY functions in this module that may push an order-WRITE command, and
//! each always validates; [`Planner::pass_through`] carries what is not an order write.
//! Three CI tests hold that shape:
//!
//!  * `every_submit_source_is_capped` iterates [`SubmitSource::ALL`] and drives each path with an
//!    over-cap order — a NEW variant fails to compile in [`SubmitSource::label`]'s no-wildcard
//!    `match` and in the test's own no-wildcard input builder until it is handled.
//!  * `order_intent_capping_is_classified` matches EXHAUSTIVELY over `vike_exec::OrderIntent`, so a
//!    new order verb cannot appear without a deliberate Capped / NotAnOrderWrite / exempt-with-
//!    reason classification (the allowlist-with-a-reason discipline `settings_registry.rs` and
//!    `duplicate_shape_gate.rs` use).
//!  * every test in this module asserts the OUTPUT invariant via `order_dispatch_tests::assert_plan_respects_limits`:
//!    every emitted `Submit`/`Bracket` command satisfies the cap. A future path that emits an
//!    unvalidated over-cap order fails the moment any test drives it.
//!
//! ## What is deliberately NOT capped, and why
//!  * **Cancels** are not order writes — a cancel only reduces exposure.
//!  * **`OrderIntent::Modify`** (the ladder's drag-to-reprice, the DOM's before it) is a REAL
//!    residual hole, declared rather
//!    than silently closed: repricing a resting order changes its notional, but the intent carries
//!    only `{coid, new_price}` — capping it means resolving the resting order's qty out of the
//!    snapshot and deciding what happens to an order that no longer fits, which is a behavior
//!    change, not an extraction. `order_intent_capping_is_classified` names it explicitly so the
//!    exemption is visible in CI rather than being an omission nobody wrote down.
//!  * **Market orders carry no price**, so `validate` measures no notional for them — the venue
//!    `RiskGate` owns that, exactly as [`crate::orders::order_entry::validate_with_multiplier`] documents.
//!    A market entry inside a BRACKET is still notional-capped through the bracket's take-profit
//!    leg (see [`Planner::admit_bracket`]).
//!
//! ## Every Trade window intent carries its own address
//! The Trade window (`vike_panels::trade`) trades the venue, account and symbol the trader picked
//! (docs/superpowers/specs/2026-09-30-trade-window-design.md §4.2), so every intent arrives as a
//! [`TradeAddress`] beside its [`TradeAction`], and no order is addressed to the snapshot's primary
//! market any more. The old ticket's `plan_trade` (deleted with it) sent every order to
//! `snap.venue`/`snap.symbol`, the PRIMARY engine's pair, and its own doc named the misroute that
//! followed — a bybit-charting operator's BUY reaching binance, possibly a LIVE binance engine.
//! That path is gone. [`tradable`] refuses an address the account's engine does not trade — or
//! cannot take an order on now, a stopped core or a halted account — BEFORE any command, and the
//! four bracket rules refuse what `vike_model::BracketSpec` cannot carry honestly: an account other
//! than its venue's single default book (it names none), an engine whose lane cannot hold the
//! stop-loss (the node's own rule, [`bracket_lane_holds_stop`]), a stop entry, a reduce-only entry.
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
//! [`DispatchRejectReason::NoRoutableMarket`] before any of that is asked.

use crate::orders::order_entry::{self, OrderLimits, OrderReject, OrderTicket};
use crate::tools::OptOrderTicket;
use crate::ui::tool_views::CockpitCmd;
use vike_core::CoreSnapshot;
use vike_exec::{Command, OrderIntent};
use vike_model::OrderRequest;
use vike_model::accounts::account_keys::AccountLabel;
use vike_panels::trade::{OrderType, Origin, TradeAction};

/// Where a Trade window's intent goes: the window's own venue, account and symbol (spec §4.2).
#[derive(Debug, Clone, PartialEq)]
pub struct TradeAddress {
    pub venue: String,
    /// `None` is the venue's default account. `Some(AccountLabel::Default)` is read as the same
    /// account everywhere in this module.
    pub account: Option<AccountLabel>,
    /// The venue-native symbol.
    pub symbol: String,
}

impl TradeAddress {
    /// The address a window names with an account TEXT (`None` for the default account), or `None`
    /// when that text is not an account label.
    ///
    /// ⚠ **Fallible on purpose.** An unparseable label must stop the window from trading, never
    /// become `account: None`: `None` is the DEFAULT account, so an `.ok()` at the call site would
    /// turn a typo'd or corrupted label into an order on a different book. The text is read in the
    /// wire's spelling (`vike_model::accounts::account_keys::parse_wire_account`), so `DEFAULT` names the
    /// default account and is stored as `None`, the one spelling of it. Only the label is checked:
    /// an empty venue or symbol is the dispatcher's `NoRoutableMarket`.
    pub fn parse(venue: &str, account: Option<&str>, symbol: &str) -> Option<Self> {
        let account = match account {
            None => None,
            Some(text) => {
                let label = vike_model::accounts::account_keys::parse_wire_account(text).ok()?;
                (!label.is_default()).then_some(label)
            }
        };
        Some(TradeAddress { venue: venue.to_string(), account, symbol: symbol.to_string() })
    }

    /// The engine this address routes to — `vike_model::accounts::account_keys::route_key_of`'s answer.
    pub fn route_key(&self) -> String {
        match &self.account {
            Some(l) => vike_model::accounts::account_keys::route_key_of(&self.venue, l),
            None => self.venue.clone(),
        }
    }

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

/// Whether a window at `addr` may place an order on `snap` now (spec §4.3): the core has not
/// stopped, the account's engine is not HALTED, and it trades the symbol. `false` for an empty
/// venue or symbol. [`untradable`] is the same rule with its reason, and the one both read.
pub fn tradable(snap: &CoreSnapshot, addr: &TradeAddress) -> bool {
    !(addr.venue.is_empty() || addr.symbol.is_empty()) && untradable(snap, addr).is_none()
}

/// Why `snap` takes no order at `addr` (a non-empty address), or `None` when it does — THE rule
/// the dispatcher refuses by and the Trade window's glue gates its controls on ([`tradable`]):
///
/// * **a core that has stopped** (`CoreSnapshot::fault`: a handler panicked and the core is in its
///   safe state) takes nothing — [`DispatchRejectReason::CoreStopped`];
/// * **the account must have an engine** (a block whose route key is the address's): an address
///   whose account the server does not run is never tradable, because an account-less order on a
///   venue whose only engine is labelled reaches engine 0 (see the module doc);
/// * **a HALTED engine** (`vike_exec::TradingState::Halted`, the kill switch) takes nothing from a
///   window — [`DispatchRejectReason::AccountHalted`]. The core would still admit a reduce its
///   position covers, but the window offers no Close or Reverse there, and the dispatcher agrees
///   with it (M-6 of the final review, slice B: this lived in the glue alone). A REDUCING engine
///   still trades: the core admits what reduces and refuses the rest in its own words;
/// * **the engine trades the symbol**: a block that names no symbol (an older node) trades only the
///   snapshot's primary market, and only if it IS the primary block — never wider than the old
///   ticket, which traded exactly that.
fn untradable(snap: &CoreSnapshot, addr: &TradeAddress) -> Option<DispatchRejectReason> {
    if snap.fault.is_some() {
        return Some(DispatchRejectReason::CoreStopped);
    }
    let key = addr.route_key();
    let Some((i, vb)) = snap.portfolio.venues.iter().enumerate().find(|(_, v)| v.route_key == key)
    else {
        return Some(DispatchRejectReason::NotTraded);
    };
    if vb.trading_state == vike_exec::TradingState::Halted {
        return Some(DispatchRejectReason::AccountHalted);
    }
    let trades = if vb.symbol.is_empty() {
        i == 0 && addr.symbol == snap.symbol
    } else {
        vb.trades(&addr.symbol)
    };
    (!trades).then_some(DispatchRejectReason::NotTraded)
}

/// Whether the engine a TP/SL bracket at `addr` reaches can hold its stop-loss: the node's own rule
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
pub fn bracket_lane_holds_stop(snap: &CoreSnapshot, addr: &TradeAddress) -> bool {
    let key = addr.route_key();
    snap.portfolio.venues.iter().find(|v| v.route_key == key).is_none_or(|vb| {
        vb.symbol.is_empty() || vike_catalog::engine_lane_holds_stop(&vb.venue, &vb.symbol)
    })
}

/// Why a window shows no own-order markers and sends no account-scoped cancel (Ruling R9).
pub const ORDERS_UNATTRIBUTED_WHY: &str =
    "This node does not say which account an order belongs to.";

/// Whether `snap` lets a window tell which account each order of `venue` belongs to (Ruling R9). A
/// node publishes every order's account exactly when it publishes the `mode` of its blocks (PR 1
/// added the two together); a node that publishes no `mode` names no order's account, so every
/// order there arrives as the default account's.
///
/// With ONE block that is still answerable, but not from the order: every order of the venue is
/// that one engine's, whatever account the order itself says (on an older node whose only engine is
/// labelled, it says none). [`window_orders`] attributes them by the block for exactly that case.
/// With more than one block and no `mode`, nothing can tell the accounts' orders apart.
///
/// ⚠ **Nor where this client cannot READ an account the node names** (M-9 of the final review,
/// slice B). An order's wire label that this client cannot parse (a label-grammar skew between node
/// and client) reaches `OrderView::account` as `None` — the type cannot spell "unknown"
/// (`crate::backend::observe_bridge::map_order`) — and would read as the DEFAULT account's order.
/// Its engine's block carries the same label in its route key, which is how this sees it: a venue
/// with more than one block, one of whose route keys names no account this client reads
/// (`vike_model::accounts::account_keys::label_of_route_key`), attributes no order at all. Fail-closed for the
/// whole venue, because the default account's own orders and the unreadable account's arrive
/// spelled alike; the orders stay in the snapshot, so the Account window still lists them.
pub fn orders_name_their_account(snap: &CoreSnapshot, venue: &str) -> bool {
    let blocks: Vec<_> = snap.portfolio.venues.iter().filter(|v| v.venue == venue).collect();
    let readable = blocks.iter().all(|b| {
        vike_model::accounts::account_keys::label_of_route_key(&b.venue, &b.route_key).is_some()
    });
    blocks.len() <= 1 || (readable && blocks.iter().any(|b| b.mode.is_some()))
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
///   venue with ONE block and no `mode` (an older node), every order of the venue is that block's,
///   whatever account the order names.
///
/// ⚠ **FAIL-CLOSED where attribution is impossible.** On a venue with several blocks and no `mode`
/// every order arrives naming no account, so it would read as the DEFAULT account's: the default
/// window would mark, and cancel, every account's orders. Here that answers NO for every order, so a
/// caller that forgets to ask [`orders_name_their_account`] gets nothing rather than the wrong book.
/// A caller still asks it first wherever it must say WHY there is nothing
/// ([`ORDERS_UNATTRIBUTED_WHY`]), as the dispatcher and the Trade window's glue do.
pub fn owns_order<'a>(
    snap: &'a CoreSnapshot,
    addr: &'a TradeAddress,
) -> impl Fn(&vike_core::OrderView) -> bool + 'a {
    let key = addr.route_key();
    let blocks: Vec<_> = snap.portfolio.venues.iter().filter(|v| v.venue == addr.venue).collect();
    let attributed = orders_name_their_account(snap, &addr.venue);
    let runs = blocks.iter().any(|b| b.route_key == key);
    let by_block = matches!(blocks.as_slice(), [only] if only.mode.is_none());
    let account = addr.named_account();
    move |o: &vike_core::OrderView| {
        attributed
            && runs
            && o.venue == addr.venue
            && o.symbol == addr.symbol
            && (by_block || named(o.account.as_ref()) == account)
    }
}

/// **A window's own resting orders** — the ONE rule the window's ladder markers, its "Cancel all N"
/// count and the dispatcher's Cancel all / Cancel side share, so what a trader sees marked is what a
/// cancel pulls: the orders [`owns_order`] gives the address that are also **live**
/// (`vike_exec::OrderStatus::is_live`, the engine's own cancel-send gate). The snapshot publishes the
/// whole order registry and nothing removes a done order from it, so without the liveness filter a
/// session's filled and cancelled orders would ride along: as markers on the ladder, and as one
/// `Cancel` each, queued AHEAD of the live ones behind the client's bounded command queue and the
/// node's rate limit — the live orders, newest, are the ones that would not get through.
///
/// Fail-closed exactly where [`owns_order`] is: on a venue whose orders cannot be attributed it
/// yields nothing.
pub fn window_orders<'a>(
    snap: &'a CoreSnapshot,
    addr: &'a TradeAddress,
) -> impl Iterator<Item = &'a vike_core::OrderView> + 'a {
    let owns = owns_order(snap, addr);
    snap.orders.iter().filter(move |o| owns(o) && o.status.is_live())
}

/// The account a command for `addr` names on the wire (Ruling R8). A labelled account names itself.
/// The DEFAULT account is named `DEFAULT` only where its venue runs more than one engine: there an
/// account-less submit is refused as ambiguous (`Some(Default)` and `None` route differently), while
/// on a single-account node `None` is unambiguous and is the only spelling an older node accepts.
///
/// `None` is also the only account a TP/SL bracket can reach: `vike_model::BracketSpec` names no
/// account, and the node's bracket command serves only a venue's single default book, so the
/// dispatcher refuses a bracket wherever this answers `Some`, and the Trade window says why there
/// (and where [`bracket_lane_holds_stop`] answers `false`) before the click.
pub fn wire_account(snap: &CoreSnapshot, addr: &TradeAddress) -> Option<AccountLabel> {
    match addr.named_account() {
        Some(label) => Some(label.clone()),
        None => (snap.portfolio.venues.iter().filter(|v| v.venue == addr.venue).count() > 1)
            .then_some(AccountLabel::Default),
    }
}

/// The client-order-id prefix each submit path stamps. Kept as consts so the historical coid
/// spelling (`"{prefix}-{next_win_n}-{shot_n}"`) is stated once — a VALID order's coid is
/// byte-identical to the pre-extraction `format!` sites in `main.rs`.
const COID_TRADE: &str = "ui";
const COID_DOM: &str = "dom";
const COID_COCKPIT: &str = "poly";
const COID_OPTIONS: &str = "opt";

/// The distinct UI origins that can produce an order WRITE. One variant per submit path, so a
/// reject can name where it came from and the CI roster can drive each path individually.
///
/// Adding a path means adding a variant, which fails to compile in [`SubmitSource::label`]'s
/// no-wildcard `match` and in the roster test's input builder until it is BOTH handled and driven.
///
/// The four Trade window variants keep the names of the paths they replaced (the old ticket and
/// the DOM ladder; Ruling R5 of the Trade window plan) — the LABELS say what they are now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubmitSource {
    /// Trade window ticket (`Origin::Ticket`) → a plain Market/Limit/Stop order.
    Trade,
    /// Trade window order with TP/SL → an `OrderIntent::Bracket` (OTO entry + OCO exits).
    TradeBracket,
    /// Trade window ladder click (`Origin::Ladder`) → a Limit or Stop order.
    Dom,
    /// Trade window `TradeAction::ClosePosition` / `TradeAction::Reverse` — a market exit leg.
    DomExit,
    /// Polymarket scalp-cockpit `CockpitCmd::Submit`.
    Cockpit,
    /// Deribit options chain confirm-ticket.
    Options,
}

impl SubmitSource {
    /// EVERY variant. The roster the CI gate iterates — a path missing from here is a path the
    /// gate never drives, so keep it exhaustive (the `label` match below is the compile-time
    /// reminder that a variant exists).
    pub const ALL: &[SubmitSource] = &[
        SubmitSource::Trade,
        SubmitSource::TradeBracket,
        SubmitSource::Dom,
        SubmitSource::DomExit,
        SubmitSource::Cockpit,
        SubmitSource::Options,
    ];

    /// Short human label for the warn log. NO WILDCARD ARM — a new variant is a compile error here.
    pub fn label(self) -> &'static str {
        match self {
            SubmitSource::Trade => "trade window ticket",
            SubmitSource::TradeBracket => "trade window bracket",
            SubmitSource::Dom => "trade window ladder",
            SubmitSource::DomExit => "trade window exit",
            SubmitSource::Cockpit => "Polymarket cockpit",
            SubmitSource::Options => "options ticket",
        }
    }
}

/// Why the dispatch refused to emit an order write.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum DispatchRejectReason {
    /// The local [`crate::orders::order_entry`] preview rejected it (malformed shape, or over a cap).
    Preview(OrderReject),
    /// The Trade window's address names no routable `(venue, symbol)` — an empty venue or symbol,
    /// as a window seeded before an observer's first `SnapshotFrame` lands carries
    /// (`WireSnapshot::empty()` carries empty strings).
    NoRoutableMarket,
    /// A TP/SL bracket with a Stop entry: `BracketSpec` has no stop entry, and the old ticket sent
    /// such an order at MARKET, losing its trigger. (A Limit entry with no price never reaches the
    /// bracket rules here: [`DispatchRejectReason::MissingPrice`] refuses it first.)
    BracketNeedsMarketOrLimit,
    /// The window's account does not trade the window's symbol, or the server does not run that
    /// account at all (spec §4.3; see [`tradable`]).
    NotTraded,
    /// The node does not say which account an order belongs to, so the window cannot pick its own
    /// orders out (Ruling R9).
    OrdersUnattributed,
    /// A TP/SL bracket anywhere but a venue's single default book: `vike_model::BracketSpec` names
    /// no account, so it reaches the venue's default account, and the node's bracket command
    /// refuses the default account of a venue that runs a second one (Ruling R3, widened; see
    /// [`wire_account`]).
    BracketOnNamedAccount,
    /// A TP/SL bracket on an engine whose LANE cannot hold its stop-loss: an engine mounted on the
    /// spot market of a venue that runs a spot and a perp lane (the shipped daemon's binance
    /// engine). The node refuses it by the same rule ([`bracket_lane_holds_stop`]); refused here,
    /// the refusal reaches the window's own strip (I-1 of the final review, slice B).
    BracketNeedsPerpEngine,
    /// A TP/SL bracket on a reduce-only entry: the exits belong to an order that opens or adds.
    BracketNeedsOpeningEntry,
    /// The core has stopped (`CoreSnapshot::fault`): it takes no order from a window ([`tradable`]).
    CoreStopped,
    /// The window's account is HALTED (`vike_exec::TradingState::Halted`): it takes no order from a
    /// window, a Close or a Reverse included ([`tradable`]).
    AccountHalted,
    /// A Limit or Stop order with no price. It is refused, never sent at MARKET: the old ticket's
    /// `unwrap_or` habit would have turned it into a market order (or a bracket's market entry) with
    /// the trader's price silently gone. Its own reason rather than the preview's
    /// `MissingLimitPrice`, whose words name a limit order and would mislead for a Stop.
    MissingPrice,
}

impl std::fmt::Display for DispatchRejectReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DispatchRejectReason::Preview(r) => write!(f, "{r}"),
            DispatchRejectReason::NoRoutableMarket => {
                write!(f, "the window's address names no venue or symbol yet")
            }
            DispatchRejectReason::BracketNeedsMarketOrLimit => {
                write!(f, "a TP/SL bracket needs a Market or Limit entry")
            }
            DispatchRejectReason::NotTraded => write!(f, "the account does not trade this symbol"),
            DispatchRejectReason::OrdersUnattributed => write!(f, "{ORDERS_UNATTRIBUTED_WHY}"),
            DispatchRejectReason::BracketOnNamedAccount => {
                write!(f, "a TP/SL bracket reaches only a venue's single default account")
            }
            DispatchRejectReason::BracketNeedsPerpEngine => write!(
                f,
                "a TP/SL bracket on this venue needs an engine on its perp market: the exchange \
                 cannot hold the stop-loss on its spot market"
            ),
            DispatchRejectReason::BracketNeedsOpeningEntry => {
                write!(f, "a TP/SL bracket needs an order that opens or adds")
            }
            DispatchRejectReason::CoreStopped => write!(f, "the server has stopped trading"),
            DispatchRejectReason::AccountHalted => write!(f, "trading is halted on this account"),
            DispatchRejectReason::MissingPrice => {
                write!(f, "a Limit or Stop order needs a price")
            }
        }
    }
}

/// One refused order write, carrying enough to log the same warn `main.rs` used to log inline, and
/// to find the Trade windows it belongs to.
#[derive(Debug, Clone, PartialEq)]
pub struct DispatchReject {
    pub source: SubmitSource,
    pub venue: String,
    pub symbol: String,
    /// The account the refused write named: the window's address's, or the order's own (the
    /// default account may arrive as `Some(AccountLabel::Default)`, see [`wire_account`]). `None`
    /// for the cockpit and the options ticket, which name no account. The shell matches a refusal
    /// to its windows by venue, symbol AND this, so a window on another account of the same symbol
    /// never shows it (`crate::ui::tool_views::route_rejects`).
    ///
    /// ⚠ A refusal of a Trade window intent is built by `Planner::reject` from the window's
    /// address (or carries that address's account the same way, as `Planner::admit_bracket` does).
    /// A reason added later, or ported from the bracket branch, must go through it too: a reject
    /// with a hand-written `None` reaches the default-account window instead of its own.
    pub account: Option<AccountLabel>,
    pub reason: DispatchRejectReason,
}

/// The drained per-frame UI intents, in the order they are folded. Owned because the caller moves
/// its drain buffers in (`TradeAction`/`CockpitCmd` carry owned coid strings).
#[derive(Debug, Default)]
pub struct DispatchInputs {
    /// Trade window intents, each with its window's address.
    pub trade_actions: Vec<(TradeAddress, TradeAction)>,
    /// Account window working-order ✕ clicks (client-order-ids).
    pub account_cancels: Vec<String>,
    /// Polymarket cockpit intents (venue is always `"polymarket"`).
    pub cockpit_cmds: Vec<CockpitCmd>,
    /// Deribit options confirm-tickets.
    pub opt_orders: Vec<OptOrderTicket>,
    /// Deribit options chain working-order-marker cancels.
    pub opt_cancels: Vec<String>,
}

/// The planned frame: commands to fire in order, refusals to log, and the advanced coid counter.
#[derive(Debug)]
pub struct DispatchPlan {
    /// Fire these in order — `main.rs` hands each to `Dispatch::send`.
    pub commands: Vec<Command>,
    /// Order writes the local preview refused; `main.rs` warns one line per entry.
    pub rejects: Vec<DispatchReject>,
    /// `next_win_n` after this frame's coid minting. A REJECTED order still consumed its coid,
    /// exactly as the pre-extraction sites did (they minted before validating), so this counter is
    /// byte-identical to the old `self.next_win_n += 1` sequence.
    pub next_win_n: u32,
}

/// The frame's plan. PURE: no I/O, no env, no clock — inputs + limits + snapshot in, commands out.
///
/// `next_win_n`/`shot_n` are the GUI's coid counters; the returned [`DispatchPlan::next_win_n`] is
/// what the caller writes back.
pub fn plan_dispatch(
    inputs: DispatchInputs,
    limits: &OrderLimits,
    snap: &CoreSnapshot,
    next_win_n: u32,
    shot_n: u32,
) -> DispatchPlan {
    let mut p =
        Planner { limits, snap, shot_n, next_win_n, commands: Vec::new(), rejects: Vec::new() };

    // ORDER IS LOAD-BEARING: the Trade window's intents in their drain order, then the Account
    // window's cancels, then the cockpit and the options chain — `emission_order_is_preserved`.
    p.plan_trade_window(inputs.trade_actions);
    for coid in inputs.account_cancels {
        p.pass_through(Command::Order(OrderIntent::Cancel(coid)));
    }
    p.plan_cockpit(inputs.cockpit_cmds);
    p.plan_options(inputs.opt_orders);
    for coid in inputs.opt_cancels {
        p.pass_through(Command::Order(OrderIntent::Cancel(coid)));
    }

    DispatchPlan { commands: p.commands, rejects: p.rejects, next_win_n: p.next_win_n }
}

/// The accumulator. Private on purpose: [`Planner::admit`] / [`Planner::admit_bracket`] are the
/// ONLY ways an order write reaches `commands`, and both validate.
struct Planner<'a> {
    limits: &'a OrderLimits,
    snap: &'a CoreSnapshot,
    shot_n: u32,
    next_win_n: u32,
    commands: Vec<Command>,
    rejects: Vec<DispatchReject>,
}

impl Planner<'_> {
    /// `"{prefix}-{next_win_n}-{shot_n}"`, advancing the counter — the historical coid spelling.
    /// (Not [`crate::orders::order_entry::next_client_order_id`], which is the two-part `"{prefix}-{seq}"`
    /// form; this three-part one is what the live GUI has always minted and what an operator reads
    /// off the working-orders table.)
    fn coid(&mut self, prefix: &str) -> String {
        let coid = format!("{prefix}-{}-{}", self.next_win_n, self.shot_n);
        self.next_win_n += 1;
        coid
    }

    /// **THE CHOKEPOINT.** Every single-order write goes through here, and here is where the local
    /// preview runs. Notional is measured WITH the instrument's contract multiplier
    /// (`CoreSnapshot::multiplier_of`, the published `vike_exec::Account::multiplier_of` grid) so
    /// the UI cap measures the same quantity `RiskGate` and `SimBroker` do; a venue/symbol absent
    /// from the grid resolves to the 1.0 default, i.e. the plain `validate` verdict.
    fn admit(&mut self, source: SubmitSource, req: OrderRequest) {
        let mult = self.snap.multiplier_of(&req.venue, &req.symbol);
        // Bound to a `let` so the `&req` borrow is over before the Err arm moves out of `req`.
        let verdict = order_entry::validate_with_multiplier(&req, self.limits, mult);
        match verdict {
            Ok(()) => self.commands.push(Command::Order(OrderIntent::Submit(Box::new(req)))),
            Err(reject) => self.rejects.push(DispatchReject {
                source,
                venue: req.venue,
                symbol: req.symbol,
                account: req.account,
                reason: DispatchRejectReason::Preview(reject),
            }),
        }
    }

    /// The bracket chokepoint. A bracket is not one order — the runtime lowers it into THREE
    /// (`vike_model::build_bracket`: OTO entry + OCO stop-loss + OCO take-profit, all at `qty`), so
    /// all three legs are validated and the whole bracket is refused if ANY leg fails. Validating
    /// the legs the runtime will actually build (rather than an approximation of the entry) is what
    /// makes this honest: it catches a fat-fingered TP/SL price and a non-finite exit level, and it
    /// gives a MARKET-entry bracket a notional cap through its priced take-profit leg that a plain
    /// market order does not get.
    ///
    /// Refusal is ATOMIC — nothing is emitted, so no leg can be stranded by a partial reject.
    ///
    /// The coids are placeholders: the runtime mints the three real ones at `apply_intent`, and
    /// [`crate::orders::order_entry::validate_with_multiplier`] reads only `qty`/`price`/`trigger_price`/
    /// `order_type`, never the id. The bracket path therefore consumes NO `next_win_n`, exactly as
    /// the pre-extraction site did.
    ///
    /// `account` is the window address's: `BracketSpec` names none, so a refusal carries it from
    /// here rather than restating which account a bracket can reach (M-4 of the A4 review).
    fn admit_bracket(
        &mut self,
        source: SubmitSource,
        spec: vike_model::BracketSpec,
        account: Option<AccountLabel>,
    ) {
        let mult = self.snap.multiplier_of(&spec.venue, &spec.symbol);
        let legs = vike_model::build_bracket(&spec, "entry", "sl", "tp");
        for leg in legs.iter() {
            if let Err(reject) = order_entry::validate_with_multiplier(leg, self.limits, mult) {
                self.rejects.push(DispatchReject {
                    source,
                    venue: spec.venue.clone(),
                    symbol: spec.symbol.clone(),
                    account,
                    reason: DispatchRejectReason::Preview(reject),
                });
                return;
            }
        }
        self.commands.push(Command::Order(OrderIntent::Bracket(Box::new(spec))));
    }

    /// A command that is NOT an order write (cancels, a reprice) — no qty/price to cap. Kept as
    /// a named method so the module's one push-to-`commands` rule reads as three explicit doors,
    /// not an ad-hoc `commands.push` anywhere.
    fn pass_through(&mut self, cmd: Command) {
        self.commands.push(cmd);
    }

    /// Refuse an order the snapshot cannot route (empty venue or symbol).
    fn reject_unroutable(&mut self, source: SubmitSource, addr: &TradeAddress) {
        self.reject(source, addr, DispatchRejectReason::NoRoutableMarket);
    }

    /// Refuse a Trade window intent at its address, for `reason`.
    fn reject(&mut self, source: SubmitSource, addr: &TradeAddress, reason: DispatchRejectReason) {
        self.rejects.push(DispatchReject {
            source,
            venue: addr.venue.clone(),
            symbol: addr.symbol.clone(),
            account: addr.account.clone(),
            reason,
        });
    }

    /// One `Cancel` per order id, never a `CancelBatch`: the desktop's lift has no wire form for a
    /// batch (Ruling R8), and a single cancel works against every node.
    fn cancel_each(&mut self, coids: Vec<String>) {
        for coid in coids {
            self.pass_through(Command::Order(OrderIntent::Cancel(coid)));
        }
    }

    /// Trade window intents → commands, each through the one chokepoint for its kind: an order
    /// write through [`Self::admit`] / [`Self::admit_bracket`], a cancel or a reprice through
    /// [`Self::pass_through`].
    fn plan_trade_window(&mut self, actions: Vec<(TradeAddress, TradeAction)>) {
        // Copy the `&'a CoreSnapshot` out of `self` up front: the lookups below must NOT hold a
        // borrow of `self` across the `admit`/`pass_through` calls that need `&mut self`.
        let snap = self.snap;
        for (addr, act) in actions {
            match act {
                TradeAction::Place { side, order_type, price, qty, reduce_only, exits, origin } => {
                    let source = match (exits.is_some(), origin) {
                        (true, _) => SubmitSource::TradeBracket,
                        (false, Origin::Ticket) => SubmitSource::Trade,
                        (false, Origin::Ladder) => SubmitSource::Dom,
                    };
                    if addr.venue.is_empty() || addr.symbol.is_empty() {
                        self.reject_unroutable(source, &addr);
                        continue;
                    }
                    if let Some(why) = untradable(snap, &addr) {
                        self.reject(source, &addr, why);
                        continue;
                    }
                    // ⚠ A Limit or Stop with no price is REFUSED, never sent at market (see
                    // `DispatchRejectReason::MissingPrice`).
                    let price = match (order_type, price) {
                        (OrderType::Market, _) => None,
                        (_, Some(p)) => Some(p),
                        (_, None) => {
                            self.reject(source, &addr, DispatchRejectReason::MissingPrice);
                            continue;
                        }
                    };
                    if let Some(e) = exits {
                        let refused = if wire_account(snap, &addr).is_some() {
                            Some(DispatchRejectReason::BracketOnNamedAccount)
                        } else if !bracket_lane_holds_stop(snap, &addr) {
                            Some(DispatchRejectReason::BracketNeedsPerpEngine)
                        } else if order_type == OrderType::Stop {
                            Some(DispatchRejectReason::BracketNeedsMarketOrLimit)
                        } else if reduce_only {
                            Some(DispatchRejectReason::BracketNeedsOpeningEntry)
                        } else {
                            None
                        };
                        if let Some(r) = refused {
                            self.reject(source, &addr, r);
                            continue;
                        }
                        // TP + SL on an entry → an OTO/OCO bracket: the runtime mints the 3 coids
                        // and wires the linkage. A Market entry carries no price.
                        self.admit_bracket(
                            source,
                            vike_model::BracketSpec {
                                venue: addr.venue,
                                symbol: addr.symbol,
                                side,
                                qty,
                                entry_price: price,
                                stop_loss: e.stop_loss,
                                take_profit: e.take_profit,
                            },
                            addr.account,
                        );
                        continue;
                    }
                    let coid = self.coid(match origin {
                        Origin::Ladder => COID_DOM,
                        Origin::Ticket => COID_TRADE,
                    });
                    let account = wire_account(snap, &addr);
                    let (v, s) = (addr.venue, addr.symbol);
                    let ticket = match (order_type, price) {
                        (OrderType::Limit, Some(p)) => OrderTicket::limit(v, s, side, qty, p),
                        (OrderType::Stop, Some(p)) => OrderTicket::stop(v, s, side, qty, p),
                        _ => OrderTicket::market(v, s, side, qty),
                    };
                    let mut req = order_entry::build_order_request(
                        &OrderTicket { reduce_only, ..ticket },
                        coid,
                    );
                    req.account = account;
                    self.admit(source, req);
                }
                TradeAction::Modify { coid, new_price } => {
                    // audit br6: consult the venue's DECLARED caps before sending. The ladder
                    // greys the drag for a venue whose adapter has no native modify; this is the
                    // matching guard on the command lane, so a stale or never-offered action can't
                    // reach a venue that would only reject it.
                    //
                    // NOT notional-capped — see the module doc's declared residual.
                    if vike_model::caps_for(&addr.venue).allows_modify() {
                        self.pass_through(Command::Order(OrderIntent::Modify {
                            client_order_id: coid,
                            new_qty: None,
                            new_price: Some(new_price),
                        }));
                    } else {
                        tracing::debug!(
                            venue = %addr.venue,
                            "Trade window modify suppressed: venue adapter declares no native modify"
                        );
                    }
                }
                TradeAction::Cancel(coids) => self.cancel_each(coids),
                TradeAction::CancelSide(_) | TradeAction::CancelAll => {
                    if !orders_name_their_account(snap, &addr.venue) {
                        self.reject(
                            SubmitSource::Dom,
                            &addr,
                            DispatchRejectReason::OrdersUnattributed,
                        );
                        continue;
                    }
                    let side = match act {
                        TradeAction::CancelSide(side) => Some(side),
                        _ => None,
                    };
                    // scope the pull to THIS address's LIVE resting orders (a MassCancel would hit
                    // the engine's other symbols too) — the same rule the window's markers read
                    let coids: Vec<String> = window_orders(snap, &addr)
                        .filter(|o| side.is_none_or(|s| o.side == s))
                        .map(|o| o.client_order_id.clone())
                        .collect();
                    self.cancel_each(coids);
                }
                TradeAction::ClosePosition | TradeAction::Reverse => {
                    let reverse = matches!(act, TradeAction::Reverse);
                    if addr.venue.is_empty() || addr.symbol.is_empty() {
                        self.reject_unroutable(SubmitSource::DomExit, &addr);
                        continue;
                    }
                    if let Some(why) = untradable(snap, &addr) {
                        self.reject(SubmitSource::DomExit, &addr, why);
                        continue;
                    }
                    // The position is the WINDOW ACCOUNT's: read off the block its route key
                    // names, never the first block of the venue.
                    let key = addr.route_key();
                    let pos = snap
                        .portfolio
                        .venues
                        .iter()
                        .find(|v| v.route_key == key)
                        .and_then(|vb| vb.positions.iter().find(|p| p.symbol == addr.symbol));
                    let Some(p) = pos else { continue };
                    let signed =
                        crate::orders::dom_math::signed_position_size(p.size, &p.position_side);
                    if signed.abs() <= 0.0 {
                        continue;
                    }
                    // exit is the OPPOSITE side of the held position: a long (signed>0) closes by
                    // SELL(-1), a short (signed<0) closes by BUY(+1). Reverse doubles the qty
                    // (close + mirror).
                    let qty = if reverse { signed.abs() * 2.0 } else { signed.abs() };
                    let coid = self.coid(COID_DOM);
                    let account = wire_account(snap, &addr);
                    let ticket = OrderTicket {
                        // a close is reduce-only; a reverse is not
                        reduce_only: !reverse,
                        ..OrderTicket::market(
                            addr.venue,
                            addr.symbol,
                            vike_model::closing_side(signed),
                            qty,
                        )
                    };
                    let mut req = order_entry::build_order_request(&ticket, coid);
                    req.account = account;
                    self.admit(SubmitSource::DomExit, req);
                }
                // Not orders: the window's shell applies these before dispatch and never forwards
                // them; matched so a new intent has to be classified here.
                TradeAction::PickSymbol { .. }
                | TradeAction::PickAccount { .. }
                | TradeAction::OpenConnections
                | TradeAction::Note { .. } => {}
            }
        }
    }

    /// Polymarket cockpit: mirrors the Trade window ladder's place/cancel path — a unique `poly-` coid, request
    /// via the one `order_entry` constructor, gated through the same local preview, routed by
    /// `OrderRequest.venue` (`"polymarket"`).
    fn plan_cockpit(&mut self, cmds: Vec<CockpitCmd>) {
        for cmd in cmds {
            match cmd {
                CockpitCmd::Cancel(coid) => {
                    self.pass_through(Command::Order(OrderIntent::Cancel(coid)));
                }
                CockpitCmd::Submit { token, side, price, qty } => {
                    let coid = self.coid(COID_COCKPIT);
                    let ticket = match price {
                        Some(p) => OrderTicket::limit("polymarket", token, side, qty, p),
                        None => OrderTicket::market("polymarket", token, side, qty),
                    };
                    self.admit(
                        SubmitSource::Cockpit,
                        order_entry::build_order_request(&ticket, coid),
                    );
                }
            }
        }
    }

    /// Options chain: each CONFIRMED ticket → the deribit exec engine. SAFETY: these only reach
    /// here AFTER the user clicked Confirm in the ticket modal — a raw chain bid/ask click merely
    /// opens the (editable) ticket and submits nothing.
    fn plan_options(&mut self, tickets: Vec<OptOrderTicket>) {
        for t in tickets {
            let coid = self.coid(COID_OPTIONS);
            let ticket = OrderTicket::limit("deribit", t.instrument, t.side, t.qty, t.price);
            self.admit(SubmitSource::Options, order_entry::build_order_request(&ticket, coid));
        }
    }
}

#[path = "order_dispatch_tests.rs"]
#[cfg(test)]
mod order_dispatch_tests;
