//! The Trade window's GLUE (`docs/superpowers/specs/2026-09-30-trade-window-design.md` §4): it builds
//! `vike_panels::trade::TradeInputs` from the backend snapshot, the node's `Directory` reply, the
//! window's own market-data book and the instrument catalog, draws the widget, paints its view
//! controls into the title bar's slot, and leaves its intents in `tv` for the shell to address and
//! dispatch. It replaces the DOM window's glue and the old Trade window's ticket; `account.rs` keeps
//! the rest of that window as the Account window.
//!
//! Everything the widget is handed comes out of ONE pure function, [`window_parts`], so the wiring
//! spec §7 lists (the two-account picker, the published mode chip, the `free_bp` sizes, Ruling R9's
//! hidden markers) is unit-tested without drawing anything.
//!
//! # ⚠ It invents nothing (the DOM glue's rule, kept)
//! No book → an EMPTY book reaches the widget, which draws the absence; no price → `None`, a dash;
//! a node that says nothing about an account's mode → `AccountMode::Unknown`, never a guess; a
//! symbol the catalog does not list → a lot of `0.0`, so every size is refused and the status strip
//! says what is missing (I18). A credentialed venue whose catalog is thin (alpaca and the other
//! `Credentialed` rows of `crates/vike-catalog/src/availability.rs`) therefore cannot trade a
//! symbol its catalog does not list; whether the window may take a lot size from anywhere else is
//! the owner's open question, and this file does not answer it by making one up.
//!
//! # ⚠ A stopped core and a halted account trade nothing, and say so
//! The deleted ticket refused every order while `CoreSnapshot::fault` was set. The window keeps that
//! rule and adds the per-account halt (`VenueBlock::trading_state`): on a faulted core every window
//! is untradable with the fault on its strip, never "Waiting for the node." (a core that faults
//! while publishing leaves a snapshot with no blocks, which would otherwise read as one that has not
//! said anything yet); on a HALTED account its window is untradable and says why; a REDUCING one
//! still trades and says it takes only reducing orders. Both rules are `order_dispatch::tradable`'s,
//! so the dispatcher refuses what the window does not offer.
//!
//! A desktop that cannot SEND trades nothing either, and says so before the click (the owner's
//! decision of 10-03, item 11): a read-only desktop (no control channel) and one whose control link
//! has closed are causes like the others ([`ControlLink`], [`refusal`]).
//!
//! The widget is TOLD the cause: [`refusal`] computes it once, the strip shows it, and the same text
//! goes into `Tradable::No::why`, which the ticket's line, the ladder's hint and every disabled
//! button say. The widget never reads the strip to guess it (the strip also carries the window's
//! own last rejection, which is not why the window takes no order).
//!
//! ⚠ What an untradable window offers for orders ALREADY working is not uniform, and this is what
//! the code does. The LADDER offers no cancel: its marker click sits behind the same `Tradable::Yes`
//! as its order clicks, so a click on a marker does nothing. The TICKET's cancel row (Cancel all,
//! Bids, Asks) stays enabled — it waits only for the node to name each order's account (Ruling R9)
//! and for there to be an order — and the dispatcher passes a cancel through on a halted account and
//! a stopped core alike. A stopped core rarely leaves it anything: entering its safe state sweeps
//! its working orders itself, best-effort. A halted account's resting orders stay where they are
//! until the operator cancels them: from that row, or outside the window.
//!
//! # ⚠ An account text that is not a label trades NOTHING
//! A window names its account as text (`ToolView::trade_account`). It is read with
//! `order_dispatch::TradeAddress::parse`, and a text that is not an account label refuses the whole
//! window rather than becoming `None`: `None` is the DEFAULT account, so the old `.ok()` habit would
//! have turned a corrupted label into orders on a different book.
//!
//! # Catalog answers are cached per window (I13)
//! A catalog scan is a linear pass over every instrument with a sort, and the picker asks about
//! every account's venue. [`TradeCatalog`] keeps the window instrument's grid, its spelling on
//! every other venue and the symbol picker's matches, each keyed on what it was computed for and
//! all of them on the catalog's `Arc`: a refreshed catalog drops them all. The account rows
//! themselves are built every frame from those answers and the snapshot.
//!
//! # What the status strip follows, and what it cannot
//! Spec §3.8 wants the strip to follow the window's OWN order by its client order id. The window
//! never learns that id (the dispatcher mints it and returns no per-window record), so the strip
//! follows the newest order on the window's venue, symbol AND account that appeared after the window
//! looked ([`follow_status`]). On one address that is the window's order unless a second window on
//! the same address, or a strategy trading that account and symbol, placed one since.
//!
//! # What "the most recently used window" means here
//! [`ToolCtx::last_trade`] seeds a new window (spec §3.11). This file only reads it; the shell
//! writes it, and it must write it from the window the trader last ACTED in (an intent or a pick),
//! never from every Trade window every frame, or a new window opens on whichever window the loop
//! happened to visit last.

use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::{Arc, Weak};

use super::ToolCtx;
use crate::orders::dom_math::{
    depth_absence, depth_absence_headline, depth_absence_next_step, signed_position_size,
};
use crate::orders::order_dispatch::{
    ORDERS_UNATTRIBUTED_WHY, TradeAddress, bracket_lane_holds_stop, orders_name_their_account,
    owns_order, tradable, window_orders, wire_account,
};
use crate::tools;
use vike_catalog::{Catalog, SearchFilter};
use vike_core::{CoreSnapshot, OrderView, VenueBlock};
use vike_exec::TradingState;
use vike_model::accounts::account_keys::AccountLabel;
use vike_model::{AssetClass, L2Book, VenueCaps};
use vike_panels::trade::ticket::TPSL_ACCOUNT_WHY;
use vike_panels::trade::{
    AccountMode, AccountRow, BookAbsence, Grid, LadderOrder, Position, StatusKind, StatusLine,
    SymbolMatch, Tradable, TradeInputs, Unconnected,
};
use vike_tradehub_client::wire::{WireDirectory, WireDirectoryAccount};
use vike_ui_theme::components::state::{self, Load};

/// Why a listed account cannot be picked. It NEVER says "switched off" and never reads `armed`:
/// `account.armed` is DERIVED from the venue's arming ceiling when a row is written and nothing on
/// the mount path reads it yet, so `false` is not an operator's decision (spec §3.3).
pub const NOT_RUNNING_WHY: &str = "Not running on the server.";

/// What a window shows before the node's first snapshot, when it has nothing to open.
pub const WAITING_FOR_NODE: &str = "Waiting for the node.";

/// Why a window whose account text is not an account label sends nothing.
pub const UNADDRESSABLE_WHY: &str =
    "This window's account is not one an order can name, so it sends nothing. Pick an account.";

/// Why a window on an account whose engine is HALTED (`vike_exec::TradingState::Halted`, the kill
/// switch) sends nothing. The core still admits a reduce its position covers, but the window offers
/// no order there, so the line says what the WINDOW does.
pub const HALTED_WHY: &str =
    "Trading is halted on this account: the server opens nothing, and this window sends nothing.";

/// What the strip says on an account whose engine is REDUCING (`vike_exec::TradingState::Reducing`):
/// it still trades, but the server refuses an order that would not reduce its position.
pub const REDUCING_WHY: &str = "This account takes only orders that reduce its position.";

/// Why TP/SL is off on an account whose engine's LANE cannot hold a bracket's stop-loss: an engine
/// mounted on the spot market of a venue that runs a spot and a perp lane — the shipped daemon's one
/// default binance engine (I-1 of the final review, slice B). The node refuses every bracket there
/// (`order_dispatch::bracket_lane_holds_stop` reads its rule), so the window says so before the
/// click instead of the account reason, which would be false on a venue's single default account.
pub const TPSL_LANE_WHY: &str = "TP/SL is not available on this account yet: its engine trades the \
                                 spot market, where the exchange cannot hold the stop-loss.";

/// What a window says when the node did not answer the `Directory` request (the owner's decision of
/// 10-03, item 5): the account picker carries it, and the strip where nothing more important is on
/// it. The window stays usable — venue names fall back to their keys, and the account list to the
/// accounts the snapshot runs.
pub const ACCOUNT_LIST_UNAVAILABLE: &str = "Account list unavailable.";

/// Why no window sends an order from a READ-ONLY desktop: no control channel to the server (none
/// armed, or one that failed to connect). The owner's decision of 10-03, item 11: said up front, as
/// the window's stated cause, rather than learned from a "Not sent" after the click.
pub const READ_ONLY_WHY: &str = "Read-only: this desktop cannot send orders.";

/// Why no window sends an order while this desktop's control link to the node is GONE (it was
/// connected and closed: a node restart, a dropped tunnel). The owner's decision of 10-03, item 11.
pub const NO_CONTROL_LINK_WHY: &str = "No control link to the node.";

/// Whether this desktop can send a Trade window's orders at all — the ONE value the shell hands the
/// glue for it (the owner's decision of 10-03, item 11), read off the control handle with
/// [`ControlLink::of`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ControlLink {
    /// A connected control channel: orders go to the node.
    Open,
    /// No control channel ([`READ_ONLY_WHY`]).
    ReadOnly,
    /// A control channel whose link has closed ([`NO_CONTROL_LINK_WHY`]).
    Lost,
}

impl ControlLink {
    /// The link a control handle gives: `None` (no handle) is read-only, a handle reports whether it
    /// is still connected (`RemoteControlHandle::is_connected`).
    pub fn of(handle_connected: Option<bool>) -> ControlLink {
        match handle_connected {
            None => ControlLink::ReadOnly,
            Some(true) => ControlLink::Open,
            Some(false) => ControlLink::Lost,
        }
    }
}

/// Why no window trades on a core that has stopped (`vike_core::CoreSnapshot::fault`: a handler
/// panicked and the core is in its safe state), with the core's own words.
fn stopped_why(fault: &str) -> String {
    format!("The server has stopped trading: {fault}")
}

/// Why a window takes no order, where the glue can STATE the cause (spec §4.3), and how the status
/// strip reads it. Its `text` is both the strip's line and the widget's `Tradable::No::why`: ONE
/// text, computed once ([`refusal`]), so the ticket's line, the ladder's hint and every disabled
/// button say what the strip says — and never have to guess the cause from whatever the strip
/// shows (the F wave's reason slot).
struct Refusal {
    kind: StatusKind,
    text: Cow<'static, str>,
}

/// The cause a window at `address` takes no order for (`address` is `None` when its account text
/// names no account; `block` is its account's engine, if the snapshot runs one; `link` is whether
/// this desktop can send at all). ERROR causes first — each outranks the window's own last order on
/// the strip — then Info ones, which that line outranks there; within each, the widest scope first:
///
/// * a core that has stopped (the whole server), then a LOST control link (this whole desktop),
///   then an account text that is not a label (this window), then a HALTED engine (this account) —
///   all errors;
/// * then a READ-ONLY desktop (no control channel: a setting of this desktop, not a fault), a node
///   that has said nothing yet, and an account the server does not run — Info lines.
///
/// `None` while the window takes orders, and for the one cause the widget words from the account's
/// list itself: an engine that trades other symbols (or, on an older node that publishes no list,
/// anything but its primary market). Every cause here is one the window refuses
/// (`order_dispatch::tradable` for the snapshot's, [`window_parts`] for the link's), so a stated
/// cause never reaches a window that trades.
fn refusal(
    snap: &CoreSnapshot,
    address: Option<&TradeAddress>,
    block: Option<&VenueBlock>,
    link: ControlLink,
) -> Option<Refusal> {
    let (kind, text) = if let Some(fault) = snap.fault.as_deref() {
        (StatusKind::Error, Cow::Owned(stopped_why(fault)))
    } else if link == ControlLink::Lost {
        (StatusKind::Error, Cow::Borrowed(NO_CONTROL_LINK_WHY))
    } else if address.is_none() {
        (StatusKind::Error, Cow::Borrowed(UNADDRESSABLE_WHY))
    } else if block.is_some_and(|b| b.trading_state == TradingState::Halted) {
        (StatusKind::Error, Cow::Borrowed(HALTED_WHY))
    } else if link == ControlLink::ReadOnly {
        (StatusKind::Info, Cow::Borrowed(READ_ONLY_WHY))
    } else if snap.portfolio.venues.is_empty() {
        (StatusKind::Info, Cow::Borrowed(WAITING_FOR_NODE))
    } else if block.is_none() {
        (StatusKind::Info, Cow::Borrowed(NOT_RUNNING_WHY))
    } else {
        return None;
    };
    Some(Refusal { kind, text })
}

/// What an unlabelled account is called while its venue holds only one.
const MAIN: &str = "main";

/// How many symbol-picker matches are shown. Each venue the server runs is searched for this many,
/// and the union is ranked again (`vike_catalog::merge_ranked`), so venues the server does not run
/// can never crowd its own out of the list.
const MATCHES_SHOWN: usize = 30;

/// A new instrument for a window: the trader's pick, or the window's first-frame seed.
#[derive(Debug, Clone, PartialEq)]
pub struct TradePick {
    pub venue: String,
    /// The account label, `None` for the venue's default account.
    pub account: Option<String>,
    pub symbol: String,
}

/// A venue's name as the settings database spells it (`venue.title`, the owner's ruling of
/// 2026-09-30), else its key. `dir` is `None` before the node's first `Directory` reply and against
/// a node that predates the verb; the key is honest then, and nothing here invents a spelling.
pub fn venue_label<'a>(dir: Option<&'a WireDirectory>, venue: &'a str) -> &'a str {
    dir.and_then(|d| d.venues.iter().find(|v| v.name == venue))
        .and_then(|v| v.title.as_deref())
        .unwrap_or(venue)
}

/// What a picker row shows for an account: its label; else, when `unlabelled_at_venue` accounts of
/// its venue carry no label, the broker's own number (`venue_account_id`) so they can be told
/// apart; else "main".
fn account_name(a: &WireDirectoryAccount, unlabelled_at_venue: usize) -> &str {
    match (&a.label, &a.venue_account_id) {
        (Some(label), _) => label,
        (None, Some(book)) if unlabelled_at_venue > 1 => book,
        _ => MAIN,
    }
}

/// Which running engine (an index into `blocks`) each database account runs, by account id; an
/// account the server does not run has no entry. An engine is found by the account's ROUTE KEY, the
/// routing answer (`VenueBlock::route_key`). When two accounts share that key (a venue's demo and
/// live books, both unlabelled — "twins": one venue, one label), the engine belongs to the one whose
/// tier its mode names; if neither does (an engine capped to paper), to the one with the lower id,
/// so the answer is stable.
///
/// Computed ONCE per frame for the whole directory (M-5 of the final review, slice B): asked per
/// account, the twin scan made the picker's rows quadratic in a list meant to reach fifty accounts
/// per venue — every Trade window, every frame, picker open or not.
fn engines_run(dir: &WireDirectory, blocks: &[VenueBlock]) -> HashMap<i64, usize> {
    let mut twins: HashMap<(&str, Option<&str>), Vec<&WireDirectoryAccount>> = HashMap::new();
    for a in &dir.accounts {
        twins.entry((a.venue.as_str(), a.label.as_deref())).or_default().push(a);
    }
    let mut engines = HashMap::new();
    for ((venue, label), mut group) in twins {
        let Some(key) = TradeAddress::parse(venue, label, "").map(|a| a.route_key()) else {
            continue;
        };
        let Some(at) = blocks.iter().position(|b| b.route_key == key) else { continue };
        group.sort_by_key(|o| o.id);
        let word = match blocks[at].mode {
            Some(vike_exec::EngineMode::Live) => "live",
            Some(vike_exec::EngineMode::Demo) => "demo",
            Some(vike_exec::EngineMode::Paper) | None => "paper",
        };
        let owner = group.iter().find(|o| o.tier == word).unwrap_or(&group[0]);
        engines.insert(owner.id, at);
    }
    engines
}

/// Why a listed account cannot be picked: `None` when the server runs it.
fn why_not(running: bool) -> Option<&'static str> {
    (!running).then_some(NOT_RUNNING_WHY)
}

/// The mode a database tier word reads as, for a row the server does not run.
fn mode_of_tier(tier: &str) -> AccountMode {
    match tier {
        "live" => AccountMode::Live,
        "demo" => AccountMode::Demo,
        "paper" => AccountMode::Paper,
        _ => AccountMode::Unknown,
    }
}

/// The mode a block publishes; `None` (a node that does not say) is unknown, never PAPER.
fn mode_of(m: Option<vike_exec::EngineMode>) -> AccountMode {
    match m {
        Some(vike_exec::EngineMode::Paper) => AccountMode::Paper,
        Some(vike_exec::EngineMode::Demo) => AccountMode::Demo,
        Some(vike_exec::EngineMode::Live) => AccountMode::Live,
        None => AccountMode::Unknown,
    }
}

/// How the status strip reads an order's status word (`vike_exec::OrderStatus::as_str`).
pub fn status_of(status: &str) -> StatusKind {
    match status {
        "FILLED" => StatusKind::Ok,
        "REJECTED" | "DENIED" | "EXPIRED" | "LIQUIDATED" => StatusKind::Error,
        _ => StatusKind::Info,
    }
}

/// A new window's instrument: the most recently used window's, else the backend's primary market
/// on the PRIMARY engine's own account (spec §3.11; C1). `None` while there is nothing honest to
/// open: before the node has named a primary market, while it publishes no primary BLOCK (a core
/// that faulted while publishing, the observer's placeholder), and for a primary block whose route
/// key names no account.
///
/// ⚠ The account is the primary block's, read from its route key. A node whose primary engine is a
/// labelled account runs NO default account on that venue, so a window on "default" there would
/// name a book the server does not run (`order_dispatch::tradable` refuses it). Without the block
/// the account is unknown, and a seed that guessed "default" would stay there after the real,
/// perhaps labelled, primary appeared.
pub fn seed(snap: &CoreSnapshot, last: Option<&TradePick>) -> Option<TradePick> {
    if let Some(last) = last {
        return Some(last.clone());
    }
    if snap.venue.is_empty() || snap.symbol.is_empty() {
        return None;
    }
    let primary = snap.portfolio.venues.first().filter(|b| b.venue == snap.venue)?;
    let label =
        vike_model::accounts::account_keys::label_of_route_key(&primary.venue, &primary.route_key)?;
    Some(TradePick {
        venue: snap.venue.clone(),
        account: label.text().map(str::to_string),
        symbol: snap.symbol.clone(),
    })
}

/// Split a venue symbol into `(base, quote)` for the size-field units, for a symbol the catalog
/// does not list — longest quote suffix first (`USDT`/`USDC` before `USD`, so `BTCUSDT` is not read
/// as `BTCUSD` + `T`). A symbol with no known quote suffix is all base and defaults to a `USDT`
/// quote; a symbol that is ONLY a quote suffix (`"USDT"`) keeps the whole symbol as base rather than
/// yielding an empty label. Such a symbol has no lot size (I18), so these words label a size field
/// that refuses every size.
pub fn split_base_quote(symbol: &str) -> (&str, &str) {
    let (base, quote) = if let Some(b) = symbol.strip_suffix("USDT") {
        (b, "USDT")
    } else if let Some(b) = symbol.strip_suffix("USDC") {
        (b, "USDC")
    } else if let Some(b) = symbol.strip_suffix("USD") {
        (b, "USD")
    } else {
        (symbol, "USDT")
    };
    (if base.is_empty() { symbol } else { base }, quote)
}

/// The words for an instrument the window has no lot size for (I18). Never a size refusal with a
/// `(0)` in it: the trader is told what is missing and where it comes from.
fn no_lot_why(symbol: &str, listed: bool, loaded: bool) -> String {
    if !loaded {
        format!("No lot size for {symbol} yet: the instrument catalog has not loaded.")
    } else if !listed {
        format!(
            "No lot size for {symbol}: the instrument catalog does not list it. Refresh it in \
             Data Manager → Instruments."
        )
    } else {
        format!(
            "No lot size for {symbol}: the instrument catalog lists it without one. Refresh it in \
             Data Manager → Instruments."
        )
    }
}

/// What the catalog lists for the window's instrument.
#[derive(Clone, Debug, PartialEq)]
struct Listing {
    grid: Grid,
    base: String,
    quote: String,
    class: AssetClass,
}

/// One symbol-picker match as the catalog lists it. Whether it can be traded and its price are the
/// snapshot's, read every frame.
#[derive(Clone, Debug, PartialEq)]
struct TradeMatch {
    venue: String,
    symbol: String,
    name: String,
}

/// **One window's catalog answers (I13)**, each kept with the key it was computed for, and all of
/// them with the catalog they were read from.
///
/// ⚠ The catalog is identified by its `Arc` (the shell replaces the `Arc` on every refresh), held
/// as a `Weak`: a `Weak` keeps the allocation reserved, so a later catalog can never be allocated at
/// the same address and pass for the one these answers came from. It does not keep the catalog's
/// instruments alive.
#[derive(Clone, Default)]
pub struct TradeCatalog {
    from: Weak<Catalog>,
    /// Whether that catalog held any instrument at all: an empty one has not loaded yet.
    loaded: bool,
    /// `(venue, symbol)` the listing below was looked up for.
    listing_for: Option<(String, String)>,
    listing: Option<Listing>,
    /// `(base, quote, class)` the spellings below were looked up for. The CLASS is part of the key
    /// because the spellings prefer it: a window moved from a venue's swap to its spot pair keeps
    /// its base and quote, and must stop opening another venue's perp.
    spellings_for: Option<(String, String, Option<AssetClass>)>,
    /// `(venue, venue-native symbol)`: the instrument's spelling on every venue that lists its
    /// base, one per venue, the same quote and class preferred.
    spellings: Vec<(String, String)>,
    /// The query, and the venues the server ran, the matches below were searched for.
    matches_for: Option<(String, Vec<String>)>,
    /// The symbol picker's matches on the venues the server runs, best first.
    matches: Vec<TradeMatch>,
}

impl TradeCatalog {
    /// Bring the answers in line with this frame's catalog, instrument, picker query and the venues
    /// `snap` runs, scanning the catalog only for what changed.
    pub fn sync(
        &mut self,
        catalog: &Arc<Catalog>,
        snap: &CoreSnapshot,
        venue: &str,
        symbol: &str,
        query: &str,
    ) {
        if !std::ptr::eq(self.from.as_ptr(), Arc::as_ptr(catalog)) {
            *self = TradeCatalog {
                from: Arc::downgrade(catalog),
                loaded: !catalog.is_empty(),
                ..TradeCatalog::default()
            };
        }
        if self.listing_for.as_ref().is_none_or(|(v, s)| v != venue || s != symbol) {
            self.listing = listing(catalog, venue, symbol);
            self.listing_for = Some((venue.to_string(), symbol.to_string()));
        }
        // The listing's own fields, not `base_quote`: a method call would hold all of `self` while
        // the spellings below are written.
        let (base, quote) = match &self.listing {
            Some(l) => (l.base.as_str(), l.quote.as_str()),
            None => split_base_quote(symbol),
        };
        let class = self.listing.as_ref().map(|l| l.class);
        if self
            .spellings_for
            .as_ref()
            .is_none_or(|(b, q, c)| b != base || q != quote || *c != class)
        {
            self.spellings = spellings(catalog, base, quote, class);
            self.spellings_for = Some((base.to_string(), quote.to_string(), class));
        }
        let query = query.trim();
        let held = held_venues(snap);
        let searched = self.matches_for.as_ref().is_some_and(|(q, h)| {
            q == query && h.len() == held.len() && h.iter().zip(&held).all(|(a, b)| a == b)
        });
        if !searched {
            self.matches = matches(catalog, query, &held);
            self.matches_for =
                Some((query.to_string(), held.iter().map(|v| v.to_string()).collect()));
        }
    }

    /// The window instrument's base and quote: the catalog's, else read off the symbol.
    fn base_quote<'s>(&'s self, symbol: &'s str) -> (&'s str, &'s str) {
        match &self.listing {
            Some(l) => (l.base.as_str(), l.quote.as_str()),
            None => split_base_quote(symbol),
        }
    }

    /// The instrument's spelling on `venue`, or `None` when the catalog lists its base nowhere
    /// there.
    fn spelling_on(&self, venue: &str) -> Option<&str> {
        self.spellings.iter().find(|(v, _)| v.eq_ignore_ascii_case(venue)).map(|(_, s)| s.as_str())
    }
}

/// What the catalog lists under `symbol` on `venue`: an EXACT lookup over the venue's whole list.
///
/// ⚠ Not the ranked `Catalog::search` with the symbol as the query: its tiers compare the raw symbol
/// with the UPPER-CASED query, so a mixed-case raw symbol (hyperliquid's `kPEPE`) would never be
/// found, and the window would say the catalog does not list it and refuse every size. The empty
/// query returns the venue's list in catalog order. The answer is cached ([`TradeCatalog`]).
fn listing(catalog: &Catalog, venue: &str, symbol: &str) -> Option<Listing> {
    let filter = SearchFilter { tab: None, venue: Some(venue.to_string()) };
    catalog.search("", &filter, usize::MAX).into_iter().find(|i| i.raw_symbol == symbol).map(|i| {
        Listing {
            grid: Grid {
                tick: i.properties.tick_size,
                lot: i.properties.step_size,
                min_qty: i.properties.min_qty,
            },
            base: i.base.clone(),
            quote: i.quote.clone(),
            class: i.asset_class,
        }
    })
}

/// The instrument `(base, quote)` as every venue that lists `base` spells it: one symbol per venue,
/// preferring the same quote, then the same asset class, then the catalog's order. The whole list is
/// read (the empty query), for the reason [`listing`] gives: a ranked search would miss a
/// mixed-case base.
fn spellings(
    catalog: &Catalog,
    base: &str,
    quote: &str,
    class: Option<AssetClass>,
) -> Vec<(String, String)> {
    let mut best: Vec<(String, String, u8)> = Vec::new();
    for i in catalog.search("", &SearchFilter::default(), usize::MAX) {
        if !i.base.eq_ignore_ascii_case(base) {
            continue;
        }
        let score = 2 * u8::from(i.quote.eq_ignore_ascii_case(quote))
            + u8::from(class == Some(i.asset_class));
        match best.iter().position(|(v, _, _)| v.eq_ignore_ascii_case(&i.venue)) {
            None => best.push((i.venue.clone(), i.raw_symbol.clone(), score)),
            Some(k) if score > best[k].2 => {
                best[k] = (i.venue.clone(), i.raw_symbol.clone(), score);
            }
            Some(_) => {}
        }
    }
    best.into_iter().map(|(v, s, _)| (v, s)).collect()
}

/// The venues `snap` runs an engine on, each once, in the snapshot's order.
fn held_venues(snap: &CoreSnapshot) -> Vec<&str> {
    let mut held: Vec<&str> = Vec::new();
    for b in &snap.portfolio.venues {
        if !held.contains(&b.venue.as_str()) {
            held.push(&b.venue);
        }
    }
    held
}

/// The symbol picker's matches for `query`: each venue in `held` searched on its own, the union
/// ranked again as one list. A single search over the whole catalog, filtered afterwards, could
/// fill its limit with venues the server does not run and leave the picker empty.
fn matches(catalog: &Catalog, query: &str, held: &[&str]) -> Vec<TradeMatch> {
    let mut found: Vec<vike_catalog::Instrument> = Vec::new();
    for venue in held {
        let filter = SearchFilter { tab: None, venue: Some(venue.to_string()) };
        found.extend(catalog.search(query, &filter, MATCHES_SHOWN).into_iter().cloned());
    }
    vike_catalog::merge_ranked(found, Vec::new(), query, MATCHES_SHOWN)
        .into_iter()
        .map(|i| TradeMatch { venue: i.venue, symbol: i.raw_symbol, name: i.description })
        .collect()
}

/// The newest order on a window's address when the window last looked (spec §3.8): what the status
/// strip follows from.
#[derive(Debug, Clone, PartialEq)]
pub struct SeenOrder {
    venue: String,
    account: Option<String>,
    symbol: String,
    coid: Option<String>,
}

/// The status strip's line for an order: `Buy 0.01 limit · ACCEPTED`.
fn order_words(o: &OrderView) -> String {
    let side = if o.side > 0 { "Buy" } else { "Sell" };
    format!("{side} {} {} · {}", o.qty, o.order_type, o.status.as_str())
}

/// Bring the status strip up to date for the window at `(venue, account, symbol)` (spec §3.8). An
/// order that appeared on the address since the window last looked is what the strip follows from
/// now on, by its client order id, until a newer one appears; a note or a refusal the shell put in
/// `tv.trade_status` stays until then. A window that has not looked at this address before — a new
/// window, or one the trader just moved — starts clean, so it never reports an order it did not see
/// happen.
///
/// ⚠ A snapshot that has said nothing yet (no venue block: the empty one a backend switch
/// publishes until the new node's first snapshot, and the one a restored window draws at startup)
/// is no look at all, and takes no baseline: a look taken there saw no order, so the node's newest
/// pre-existing order then read as one that APPEARED, and the strip followed it as if this window
/// had placed it (the FW2 review). The same test as the window's "waiting for the node" cause.
fn follow_status(
    tv: &mut tools::ToolView,
    snap: &CoreSnapshot,
    venue: &str,
    account: Option<&str>,
    symbol: &str,
) {
    if snap.portfolio.venues.is_empty() {
        return;
    }
    // The dispatcher's attribution, live or done (the strip must also see an order that filled
    // before a snapshot showed it working): nothing on a node that attributes no order (Ruling R9).
    let newest = TradeAddress::parse(venue, account, symbol).and_then(|addr| {
        let owns = owns_order(snap, &addr);
        snap.orders.iter().rev().find(|&o| owns(o)).map(|o| o.client_order_id.clone())
    });
    let same_address = tv
        .trade_seen
        .as_ref()
        .is_some_and(|s| s.venue == venue && s.account.as_deref() == account && s.symbol == symbol);
    if !same_address {
        tv.trade_status = None;
        tv.trade_status_coid = None;
    } else if newest.is_some() && tv.trade_seen.as_ref().is_some_and(|s| s.coid != newest) {
        tv.trade_status_coid = newest.clone();
    }
    tv.trade_seen = Some(SeenOrder {
        venue: venue.to_string(),
        account: account.map(str::to_string),
        symbol: symbol.to_string(),
        coid: newest,
    });
    let followed = tv
        .trade_status_coid
        .as_deref()
        .and_then(|c| snap.orders.iter().find(|o| o.client_order_id == c));
    if let Some(o) = followed {
        tv.trade_status = Some((status_of(o.status.as_str()), order_words(o)));
    }
}

/// Everything one frame of a Trade window is built from, borrowed (I14).
struct WindowSources<'a> {
    snap: &'a CoreSnapshot,
    /// The node's `Directory` reply; `None` before it and against a node that predates the verb.
    directory: Option<&'a WireDirectory>,
    /// The window's catalog answers, already synced to this frame's instrument.
    catalog: &'a TradeCatalog,
    venue: &'a str,
    /// The window's account TEXT (`ToolView::trade_account`), `None` for the default account.
    account: Option<&'a str>,
    symbol: &'a str,
    /// The window's own L2 book, `None` until the first snapshot of it lands.
    book: Option<&'a L2Book>,
    stale: bool,
    /// The window venue's market-data status line, verbatim; `None` when nothing produces one.
    source: Option<&'a str>,
    recent: &'a [(String, String)],
    /// What the strip says about the window's last order, note or refusal (`ToolView::trade_status`).
    status: Option<&'a (StatusKind, String)>,
    /// Whether the backend can carry a TP/SL bracket at all (Ruling R8).
    bracket_wire: bool,
    /// Whether this desktop can send an order at all (the owner's item 11).
    control_link: ControlLink,
    /// Whether the node's `Directory` fetch ended with no list (the owner's item 5).
    directory_unavailable: bool,
}

impl<'a> WindowSources<'a> {
    /// The symbol a pick on `venue` opens: the window's own on its own venue, else the catalog's
    /// spelling of the instrument there (C3); `None` when the catalog does not list it there.
    fn opens(&self, venue: &str) -> Option<&'a str> {
        if venue == self.venue { Some(self.symbol) } else { self.catalog.spelling_on(venue) }
    }
}

/// One frame of a Trade window, owned where the widget borrows. [`WindowParts::inputs`] lends it
/// out as the widget's [`TradeInputs`].
struct WindowParts<'a> {
    venue: &'a str,
    venue_label: &'a str,
    /// The account label, `None` for the default account (the raw text when it is not a label).
    account: Option<&'a str>,
    symbol: &'a str,
    base: &'a str,
    quote: &'a str,
    mode: AccountMode,
    tradable: bool,
    /// What the account's engine trades, for the untradable line.
    trades: Vec<String>,
    /// Why the window takes no order, where the glue can state it ([`Refusal`]): the widget's
    /// `Tradable::No::why`, in the words the strip uses.
    why: Option<Cow<'static, str>>,
    grid: Grid,
    /// The book, when it has a level on either side.
    book: Option<&'a L2Book>,
    /// An empty book for the widget to draw the absence from.
    no_book: L2Book,
    last: Option<f64>,
    stale: bool,
    source: &'a str,
    absence: Option<BookAbsence<'a>>,
    orders: Vec<LadderOrder>,
    orders_why: Option<&'static str>,
    position: Option<Position>,
    buying_power: Option<f64>,
    caps: VenueCaps,
    /// Why TP/SL cannot go to this account, stated once: the widget's `bracket_why`.
    bracket_why: Option<&'static str>,
    bracket_wire: bool,
    matches: Vec<SymbolMatch<'a>>,
    recent: Vec<(&'a str, &'a str)>,
    accounts: Vec<AccountRow<'a>>,
    /// Why `accounts` is not the database's list: the widget's `accounts_why`.
    accounts_why: Option<&'static str>,
    unconnected: Vec<Unconnected<'a>>,
    status: Option<(StatusKind, Cow<'a, str>)>,
}

impl WindowParts<'_> {
    /// The widget's inputs for this frame.
    fn inputs(&self) -> TradeInputs<'_> {
        TradeInputs {
            venue: self.venue,
            venue_label: self.venue_label,
            account: self.account,
            symbol: self.symbol,
            base: self.base,
            quote: self.quote,
            mode: self.mode,
            tradable: if self.tradable {
                Tradable::Yes
            } else {
                Tradable::No { trades: &self.trades, why: self.why.as_deref() }
            },
            grid: self.grid,
            book: self.book.unwrap_or(&self.no_book),
            last: self.last,
            stale: self.stale,
            source: self.source,
            absence: self.absence,
            orders: &self.orders,
            orders_why: self.orders_why,
            position: self.position,
            buying_power: self.buying_power,
            caps: self.caps,
            bracket_why: self.bracket_why,
            bracket_wire: self.bracket_wire,
            matches: &self.matches,
            recent: &self.recent,
            accounts: &self.accounts,
            accounts_why: self.accounts_why,
            unconnected: &self.unconnected,
            status: self.status.as_ref().map(|(kind, text)| StatusLine { kind: *kind, text }),
        }
    }
}

/// **The window's inputs, built by one pure function (I14).** No egui, no I/O: the snapshot, the
/// directory, the catalog answers and the window's own state in, the widget's inputs out.
fn window_parts<'a>(src: &WindowSources<'a>) -> WindowParts<'a> {
    let snap = src.snap;
    let address = TradeAddress::parse(src.venue, src.account, src.symbol);
    let block = address.as_ref().and_then(|a| {
        let key = a.route_key();
        snap.portfolio.venues.iter().find(|b| b.route_key == key)
    });
    // A core that has stopped and an account whose engine is HALTED take nothing from the window
    // (`order_dispatch::tradable`, the dispatcher's own rule since M-6); a REDUCING one still
    // trades and the strip says what it takes. Nor does a desktop that cannot send (item 11).
    let state = block.map(|b| b.trading_state);
    let sends = src.control_link == ControlLink::Open;
    let can_trade = sends && address.as_ref().is_some_and(|a| tradable(snap, a));
    // Why it takes none, where that can be stated: the strip's line AND the widget's cause.
    let stated = refusal(snap, address.as_ref(), block, src.control_link);
    let trades: Vec<String> = block
        .map(|b| {
            std::iter::once(&b.symbol)
                .chain(&b.extra_symbols)
                .filter(|s| !s.is_empty())
                .cloned()
                .collect()
        })
        .unwrap_or_default();

    // The book, and the window's price: the book's MID. No last trade price reaches the desktop:
    // `WireSnapshot` carries no marks and `observe_bridge::wire_to_core` leaves `marks` empty, so
    // reading them here was a path no snapshot could take (the FW2 review); the bar's hover says
    // "Mid price".
    let real = src.book.filter(|b| b.bid_levels() + b.ask_levels() > 0);
    let last = real.and_then(L2Book::mid);
    let absence = real.is_none().then(|| {
        let a = depth_absence(src.source);
        BookAbsence {
            headline: depth_absence_headline(a),
            cause: src.source.unwrap_or("").trim(),
            next_step: depth_absence_next_step(a),
        }
    });

    // The grid from the catalog, and no invented lot (I18).
    let listing = src.catalog.listing.as_ref();
    let grid = listing.map_or_else(
        || Grid { tick: real.map_or(0.0, |b| b.tick_size).max(0.0), lot: 0.0, min_qty: 0.0 },
        |l| l.grid,
    );
    let (base, quote) = src.catalog.base_quote(src.symbol);

    // The window's own orders: the dispatcher's rule, so the markers are what Cancel all pulls. On a
    // node that names no order's account (Ruling R9) the window cannot tell its own from another
    // account's, so it shows none and says why.
    let attributed = orders_name_their_account(snap, src.venue);
    let orders: Vec<LadderOrder> = match &address {
        Some(a) if attributed => window_orders(snap, a)
            .map(|o| LadderOrder {
                client_order_id: o.client_order_id.clone(),
                side: o.side,
                price: o.price.or(o.trigger_price).unwrap_or(0.0),
                qty: o.qty - o.filled_qty,
                is_stop: o.order_type == "stop",
            })
            .collect(),
        _ => Vec::new(),
    };
    let orders_why = if address.is_none() {
        Some(UNADDRESSABLE_WHY)
    } else {
        (!attributed).then_some(ORDERS_UNATTRIBUTED_WHY)
    };
    let position =
        block.and_then(|b| b.positions.iter().find(|p| p.symbol == src.symbol)).map(|p| Position {
            size: signed_position_size(p.size, &p.position_side),
            avg_px: p.avg_px,
            upnl: p.unrealized,
        });

    // The symbol picker: the cached matches (searched on the venues the server runs), each with this
    // frame's price and whether the account it would open on takes the order.
    let matches: Vec<SymbolMatch<'a>> = src
        .catalog
        .matches
        .iter()
        .map(|m| {
            let account = match &address {
                Some(a) if m.venue == src.venue => a.account.clone(),
                _ => None,
            };
            let at = TradeAddress { venue: m.venue.clone(), account, symbol: m.symbol.clone() };
            SymbolMatch {
                venue: &m.venue,
                venue_label: venue_label(src.directory, &m.venue),
                symbol: &m.symbol,
                name: &m.name,
                // No price for an instrument the window does not show reaches the desktop (the
                // snapshot carries no marks): the picker's row prints a dash.
                last: None,
                tradable: sends
                    && (address.is_some() || m.venue != src.venue)
                    && tradable(snap, &at),
            }
        })
        .collect();

    let unconnected: Vec<Unconnected<'a>> = match src.directory {
        Some(dir) => dir
            .venues
            .iter()
            .filter(|v| !dir.accounts.iter().any(|a| a.venue == v.name))
            .filter(|v| src.opens(&v.name).is_some())
            .map(|v| Unconnected { venue_label: v.title.as_deref().unwrap_or(&v.name) })
            .collect(),
        None => Vec::new(),
    };

    // The strip, most fundamental first: a stated cause that is an ERROR (a stopped core, a lost
    // control link, a window that names no account, a halted account); then the window's own last
    // order, note or refusal; then a stated cause that is not (a read-only desktop, waiting for the
    // node, not running on the server); then what the window is missing or limited to; and last a
    // node that did not answer the directory — said wherever nothing more important is, since the
    // window still works without the list (the owner's item 5; the picker always says it).
    let accounts_why = src.directory_unavailable.then_some(ACCOUNT_LIST_UNAVAILABLE);
    let status = match (&stated, src.status) {
        (Some(r), _) if r.kind == StatusKind::Error => Some((r.kind, r.text.clone())),
        (_, Some((kind, text))) => Some((*kind, Cow::Borrowed(text.as_str()))),
        (Some(r), None) => Some((r.kind, r.text.clone())),
        (None, None) if !(grid.lot > 0.0 && grid.lot.is_finite()) => {
            let why = no_lot_why(src.symbol, listing.is_some(), src.catalog.loaded);
            Some((StatusKind::Info, Cow::Owned(why)))
        }
        (None, None) if state == Some(TradingState::Reducing) => {
            Some((StatusKind::Info, Cow::Borrowed(REDUCING_WHY)))
        }
        (None, None) => orders_why.or(accounts_why).map(|w| (StatusKind::Info, Cow::Borrowed(w))),
    };

    // Why TP/SL cannot go to this account, stated ONCE (I-1 of the final review, slice B, step 3):
    // a bracket names no account, so it reaches only a venue's single default book; and the node
    // refuses one to an engine whose lane cannot hold the stop-loss, which the window now says
    // before the click. The dispatcher refuses both by the same two rules, in the same order.
    let bracket_why = match &address {
        None => Some(TPSL_ACCOUNT_WHY),
        Some(a) if wire_account(snap, a).is_some() => Some(TPSL_ACCOUNT_WHY),
        Some(a) if !bracket_lane_holds_stop(snap, a) => Some(TPSL_LANE_WHY),
        Some(_) => None,
    };

    WindowParts {
        bracket_why,
        accounts_why,
        account: match &address {
            Some(a) if a.account.is_none() => None,
            _ => src.account,
        },
        venue: src.venue,
        venue_label: venue_label(src.directory, src.venue),
        symbol: src.symbol,
        base,
        quote,
        mode: block.map_or(AccountMode::Unknown, |b| mode_of(b.mode)),
        tradable: can_trade,
        trades,
        why: stated.map(|r| r.text),
        grid,
        book: real,
        no_book: L2Book::new(1.0),
        last,
        stale: src.stale,
        source: src.source.unwrap_or(""),
        absence,
        orders,
        orders_why,
        position,
        buying_power: block.map(|b| b.free_bp),
        caps: vike_model::caps_for(src.venue),
        bracket_wire: src.bracket_wire,
        matches,
        recent: src.recent.iter().map(|(v, s)| (v.as_str(), s.as_str())).collect(),
        accounts: account_rows(src),
        unconnected,
        status,
    }
}

/// The venue · account picker's rows (spec §3.3): the settings database's accounts on every venue
/// that lists the instrument, running or not, plus any engine the server runs that no database row
/// claims; against a node with no `Directory` the snapshot's running accounts alone, by their keys.
/// The default account first within each venue, venues in the order they first appear. Linear in
/// the directory: every per-venue answer is counted once per frame ([`engines_run`], M-5).
fn account_rows<'a>(src: &WindowSources<'a>) -> Vec<AccountRow<'a>> {
    let blocks = &src.snap.portfolio.venues;
    let mut rows: Vec<AccountRow<'a>> = Vec::new();
    let mut claimed = vec![false; blocks.len()];
    if let Some(dir) = src.directory {
        let engines = engines_run(dir, blocks);
        // Each venue's unlabelled accounts, counted once: per row it would be quadratic in a list
        // that is meant to reach fifty accounts per venue.
        let mut unlabelled_at: Vec<(&str, usize)> = Vec::new();
        for a in dir.accounts.iter().filter(|a| a.label.is_none()) {
            match unlabelled_at.iter().position(|(v, _)| *v == a.venue) {
                Some(k) => unlabelled_at[k].1 += 1,
                None => unlabelled_at.push((&a.venue, 1)),
            }
        }
        for a in &dir.accounts {
            let at = engines.get(&a.id).copied();
            if let Some(i) = at {
                claimed[i] = true;
            }
            let Some(symbol) = src.opens(&a.venue) else { continue };
            let unlabelled =
                unlabelled_at.iter().find(|(v, _)| *v == a.venue).map_or(0, |(_, n)| *n);
            rows.push(AccountRow {
                venue: &a.venue,
                venue_label: venue_label(Some(dir), &a.venue),
                account: a.label.as_deref(),
                symbol,
                name: account_name(a, unlabelled),
                mode: at.map_or_else(|| mode_of_tier(&a.tier), |i| mode_of(blocks[i].mode)),
                why_not: why_not(at.is_some()),
            });
        }
    }
    for (b, _) in blocks.iter().zip(&claimed).filter(|(_, c)| !**c) {
        let Some(symbol) = src.opens(&b.venue) else { continue };
        // A row's account is a ROUTING answer (picking the row addresses orders to it), so it is
        // read from the route key, as `seed` reads it. A block whose label did not survive the wire
        // (the observe bridge drops a malformed one and keeps the route key) offers no row: shown
        // as "main" it would address the default account. The label text is the block's own, kept
        // only when it agrees with the route key.
        let Some(routed) =
            vike_model::accounts::account_keys::label_of_route_key(&b.venue, &b.route_key)
        else {
            continue;
        };
        let label = b.account.as_ref().and_then(AccountLabel::text);
        if routed.text() != label {
            continue;
        }
        rows.push(AccountRow {
            venue: &b.venue,
            venue_label: venue_label(src.directory, &b.venue),
            account: label,
            symbol,
            name: label.unwrap_or(MAIN),
            mode: mode_of(b.mode),
            why_not: None,
        });
    }
    let mut venues: Vec<&str> = Vec::new();
    for r in &rows {
        if !venues.contains(&r.venue) {
            venues.push(r.venue);
        }
    }
    rows.sort_by_key(|r| (venues.iter().position(|v| *v == r.venue), r.account.is_some()));
    rows
}

/// The window's view controls (header variant 1, spec §3.2), painted into the title bar through a
/// detached `Ui`, right-aligned in the slot the bar reserved for them. `ui` lends its context, its
/// layer, its id and its style; nothing is allocated in it. Answers the window size a click asked
/// for ([`vike_panels::trade::view_controls`]).
///
/// ⚠ Across, the slot; DOWN, the whole bar, the controls centred in it. Each control is a
/// `control_h` square, 28 pt at Comfortable density, and the slot is only `title_bar::TAB_H` (24)
/// tall: it is bottom-seated so a Connections chip breaks the hairline, which these controls do
/// not do. The detached `Ui`'s rect is its clip, so in the slot's rect they were cut along their
/// whole bottom edge (the render check of 2026-10-03). The bar holds them with a point to spare
/// at every density, level with the window's own controls, which are centred in it too.
pub(crate) fn title_bar_view_controls(
    ui: &egui::Ui,
    slot: &crate::ui::workspace::TitleTabSlot,
    state: &mut vike_panels::trade::TradeState,
) -> Option<egui::Vec2> {
    let t = vike_ui_theme::components::Tokens::of(ui.ctx());
    let w = vike_panels::trade::view_controls_width(&t);
    let r = egui::Rect::from_min_max(
        egui::pos2((slot.tabs.max.x - w).max(slot.tabs.min.x), slot.bar.min.y),
        egui::pos2(slot.tabs.max.x, slot.bar.max.y),
    );
    let mut bar = egui::Ui::new(
        ui.ctx().clone(),
        ui.id().with("trade_view_controls"),
        egui::UiBuilder::new()
            .layer_id(ui.layer_id())
            .max_rect(r)
            .layout(egui::Layout::left_to_right(egui::Align::Center))
            .style(ui.style().clone()),
    );
    vike_panels::trade::view_controls(&mut bar, state)
}

/// The window body. `slot` is the title bar's reserved rect (`WinKind::Trade.carries_title_tabs`),
/// where the view controls go (header variant 1).
pub fn trade_tool_content(
    ui: &mut egui::Ui,
    ctx: &ToolCtx<'_>,
    tv: &mut tools::ToolView,
    slot: &crate::ui::workspace::TitleTabSlot,
) {
    if let Some(size) = title_bar_view_controls(ui, slot, &mut tv.trade) {
        tv.trade_resize = Some(size);
    }

    let (venue, symbol) = (ctx.book.venue, ctx.book.symbol);
    if venue.is_empty() || symbol.is_empty() {
        // A window's first frame: open it on the seed, or say what it is waiting for (minor 27) —
        // or, on a core that has stopped, that it has stopped, which no waiting will cure.
        let pick = seed(ctx.snap, ctx.last_trade);
        let stopped = ctx.snap.fault.as_deref().map(stopped_why);
        match (&pick, &stopped) {
            (Some(_), _) => state::view(ui, Load::Loading("Opening the instrument.")),
            (None, Some(why)) => state::view(ui, Load::Unreachable(why)),
            (None, None) => state::view(ui, Load::Loading(WAITING_FOR_NODE)),
        };
        tv.trade_pick = pick;
        return;
    }

    tv.trade_catalog.sync(ctx.symbols, ctx.snap, venue, symbol, &tv.trade.query);
    let account = tv.trade_account.clone();
    follow_status(tv, ctx.snap, venue, account.as_deref(), symbol);
    // THE window's own depth link, verbatim: this venue's market-data status line. A venue with no
    // producer is ABSENT from the map, a different statement from one that has written nothing.
    let source: Option<String> =
        ctx.feed_statuses.get(venue).map(|h| h.lock().unwrap_or_else(|e| e.into_inner()).clone());
    let parts = window_parts(&WindowSources {
        snap: ctx.snap,
        directory: ctx.directory,
        catalog: &tv.trade_catalog,
        venue,
        account: account.as_deref(),
        symbol,
        book: ctx.book.book,
        stale: ctx.book.stale,
        source: source.as_deref(),
        recent: &tv.trade_recent,
        status: tv.trade_status.as_ref(),
        bracket_wire: crate::backend::tradehub_control::bracket_has_wire_form(),
        control_link: ctx.control_link,
        directory_unavailable: ctx.directory_unavailable,
    });
    let acts = vike_panels::trade::draw(ui, &mut tv.trade, &parts.inputs());
    tv.trade_actions.extend(acts);
}

#[cfg(test)]
mod tests {
    use super::*;
    use vike_catalog::Instrument;
    use vike_exec::{EngineMode, OrderStatus};
    use vike_model::SymbolProperties;
    use vike_tradehub_client::wire::WireDirectoryVenue;

    // ---- fixtures ------------------------------------------------------------------------------

    fn venue(name: &str, title: Option<&str>) -> WireDirectoryVenue {
        WireDirectoryVenue { name: name.into(), title: title.map(Into::into) }
    }

    fn account(id: i64, venue: &str, label: Option<&str>, tier: &str) -> WireDirectoryAccount {
        WireDirectoryAccount {
            id,
            venue: venue.into(),
            label: label.map(Into::into),
            tier: tier.into(),
            venue_account_id: None,
        }
    }

    /// One engine's block, as a node that publishes mode and symbols names it. Its route key is
    /// derived from the label exactly as the core derives it.
    fn block(
        venue: &str,
        label: Option<&str>,
        symbol: &str,
        mode: Option<EngineMode>,
    ) -> VenueBlock {
        let account = label.map(|l| AccountLabel::parse(l).expect("a test label"));
        let route_key = match &account {
            Some(l) => vike_model::accounts::account_keys::route_key_of(venue, l),
            None => venue.to_string(),
        };
        VenueBlock {
            venue: venue.into(),
            account,
            route_key,
            symbol: symbol.into(),
            mode,
            ..Default::default()
        }
    }

    /// A snapshot publishing `blocks`, the first one its primary (`CoreSnapshot::build` publishes the
    /// primary engine first).
    fn snap_with(blocks: Vec<VenueBlock>) -> CoreSnapshot {
        let (v, s) =
            blocks.first().map(|b| (b.venue.clone(), b.symbol.clone())).unwrap_or_default();
        let mut snap = CoreSnapshot::empty(&v, &s);
        snap.portfolio.venues = blocks;
        snap
    }

    fn order(coid: &str, label: Option<&str>, status: OrderStatus) -> OrderView {
        OrderView {
            client_order_id: coid.into(),
            venue: "binance".into(),
            account: label.map(|l| AccountLabel::parse(l).expect("a test label")),
            symbol: "BTCUSDT".into(),
            side: 1,
            qty: 1.0,
            order_type: "limit".into(),
            price: Some(99.0),
            trigger_price: None,
            status,
            venue_order_id: None,
            filled_qty: 0.0,
            avg_fill_px: 0.0,
        }
    }

    fn inst(venue: &str, symbol: &str, quote: &str, class: AssetClass, lot: f64) -> Instrument {
        Instrument {
            venue: venue.into(),
            raw_symbol: symbol.into(),
            asset_class: class,
            base: "BTC".into(),
            quote: quote.into(),
            description: format!("Bitcoin / {quote}"),
            properties: SymbolProperties {
                tick_size: 0.1,
                step_size: lot,
                min_qty: lot,
                ..Default::default()
            },
            contract_type: None,
            settle_asset: None,
        }
    }

    /// BTC on three venues: binance spot and perp (and a USDC pair), okx spot and swap, bybit spot.
    fn btc_catalog() -> Arc<Catalog> {
        Arc::new(Catalog::from_instruments(vec![
            inst("binance", "BTCUSDT", "USDT", AssetClass::CryptoSpot, 0.001),
            inst("binance", "BTCUSDT.P", "USDT", AssetClass::CryptoPerp, 0.001),
            inst("binance", "BTCUSDC", "USDC", AssetClass::CryptoSpot, 0.001),
            inst("okx", "BTC-USDT", "USDT", AssetClass::CryptoSpot, 0.0001),
            inst("okx", "BTC-USDT-SWAP", "USDT", AssetClass::CryptoPerp, 0.01),
            inst("bybit", "BTCUSDT", "USDT", AssetClass::CryptoSpot, 0.000001),
        ]))
    }

    /// `c` synced to `cat` for a window at `(venue, symbol)` with the picker on `query`, on a node
    /// publishing `snap`.
    fn sync_on(
        c: &mut TradeCatalog,
        cat: &Arc<Catalog>,
        snap: &CoreSnapshot,
        (venue, symbol, query): (&str, &str, &str),
    ) {
        c.sync(cat, snap, venue, symbol, query);
    }

    fn synced(
        cat: &Arc<Catalog>,
        snap: &CoreSnapshot,
        venue: &str,
        symbol: &str,
        query: &str,
    ) -> TradeCatalog {
        let mut c = TradeCatalog::default();
        sync_on(&mut c, cat, snap, (venue, symbol, query));
        c
    }

    /// One frame's sources at `(venue, account, symbol)`, with no book, no recent picks and no
    /// status.
    fn sources<'a>(
        snap: &'a CoreSnapshot,
        directory: Option<&'a WireDirectory>,
        catalog: &'a TradeCatalog,
        (venue, account, symbol): (&'a str, Option<&'a str>, &'a str),
    ) -> WindowSources<'a> {
        WindowSources {
            snap,
            directory,
            catalog,
            venue,
            account,
            symbol,
            book: None,
            stale: false,
            source: None,
            recent: &[],
            status: None,
            bracket_wire: false,
            control_link: ControlLink::Open,
            directory_unavailable: false,
        }
    }

    /// A node running binance's default account (LIVE, 1500 free) and its `SUB` account (DEMO,
    /// 250 free), both on BTCUSDT.
    fn two_accounts() -> CoreSnapshot {
        let mut main = block("binance", None, "BTCUSDT", Some(EngineMode::Live));
        main.free_bp = 1500.0;
        let mut sub = block("binance", Some("SUB"), "BTCUSDT", Some(EngineMode::Demo));
        sub.free_bp = 250.0;
        snap_with(vec![main, sub])
    }

    /// The database behind [`two_accounts`]: SUB listed BEFORE the default account (the reply is
    /// id-ordered), an okx account the server does not run, and bybit with no account at all.
    fn two_accounts_directory() -> WireDirectory {
        WireDirectory {
            venues: vec![
                venue("binance", Some("Binance")),
                venue("okx", Some("OKX")),
                venue("bybit", None),
            ],
            accounts: vec![
                account(1, "binance", Some("SUB"), "demo"),
                account(2, "binance", None, "live"),
                account(3, "okx", None, "demo"),
            ],
        }
    }

    // ---- the small pieces ----------------------------------------------------------------------

    #[test]
    fn a_status_line_reads_an_orders_progress() {
        assert_eq!(status_of("FILLED"), StatusKind::Ok);
        assert_eq!(status_of("REJECTED"), StatusKind::Error);
        assert_eq!(status_of("ACCEPTED"), StatusKind::Info);
    }

    #[test]
    fn the_seed_is_the_last_used_instrument_else_the_primary_market() {
        let snap = snap_with(vec![block("binance", None, "BTCUSDT", Some(EngineMode::Live))]);
        let last = TradePick { venue: "okx".into(), account: None, symbol: "BTC-USDT-SWAP".into() };
        assert_eq!(seed(&snap, Some(&last)), Some(last));
        assert_eq!(
            seed(&snap, None),
            Some(TradePick { venue: "binance".into(), account: None, symbol: "BTCUSDT".into() })
        );
    }

    /// C1: a node whose primary engine is a labelled account opens the first window on THAT
    /// account. A window on "binance, default account" there would name a book the server does not
    /// run, and an account-less order on it would reach engine 0, the labelled one.
    #[test]
    fn a_labelled_primary_seeds_a_window_on_its_own_account() {
        let snap =
            snap_with(vec![block("binance", Some("SUB"), "BTCUSDT", Some(EngineMode::Live))]);
        let pick = seed(&snap, None).expect("a primary market");
        assert_eq!(
            pick,
            TradePick {
                venue: "binance".into(),
                account: Some("SUB".into()),
                symbol: "BTCUSDT".into()
            }
        );
        let addr = TradeAddress::parse(&pick.venue, pick.account.as_deref(), &pick.symbol)
            .expect("the seed's label parses");
        assert!(tradable(&snap, &addr), "the seeded window can trade what it names");
    }

    /// Minor 27: before the node's first snapshot there is nothing to open, and the window says it
    /// is waiting rather than opening on an empty address.
    #[test]
    fn a_window_waits_for_the_node_rather_than_opening_on_nothing() {
        assert_eq!(seed(&CoreSnapshot::empty("", ""), None), None);
    }

    /// A venue is named by the settings database's title, else by its key (spec §3.3, the owner's
    /// ruling of 2026-09-30). Nothing in this code spells a venue.
    #[test]
    fn a_venue_is_named_by_the_databases_title_else_by_its_key() {
        let dir = WireDirectory {
            venues: vec![venue("ctrader", Some("cTrader")), venue("okx", None)],
            accounts: vec![],
        };
        assert_eq!(venue_label(Some(&dir), "ctrader"), "cTrader");
        assert_eq!(venue_label(Some(&dir), "okx"), "okx", "a store not yet carried shows the key");
        assert_eq!(venue_label(None, "binance"), "binance", "an older node shows the key");
    }

    /// An unlabelled account is "main", unless its venue holds more than one; then the broker's
    /// number tells them apart.
    #[test]
    fn an_account_is_shown_by_label_else_by_book_number_when_two_share_a_venue() {
        let mut a = account(1, "dukascopy", None, "demo");
        a.venue_account_id = Some("DEMO-111".into());
        assert_eq!(account_name(&a, 1), "main");
        assert_eq!(account_name(&a, 2), "DEMO-111");
        let hedge = account(2, "binance", Some("HEDGE"), "demo");
        assert_eq!(account_name(&hedge, 2), "HEDGE");
    }

    /// The engine the server runs for an address belongs to the one account whose tier its mode
    /// names, when two unlabelled accounts share the address (a venue's demo and live books).
    #[test]
    fn a_running_engine_belongs_to_the_account_whose_tier_its_mode_names() {
        let dir = WireDirectory {
            venues: vec![venue("hyperliquid", Some("Hyperliquid"))],
            accounts: vec![
                account(4, "hyperliquid", None, "demo"),
                account(5, "hyperliquid", None, "live"),
            ],
        };
        let running = [block("hyperliquid", None, "BTC", Some(EngineMode::Live))];
        let engines = engines_run(&dir, &running);
        assert_eq!(engines.get(&4), None, "the demo book is not running");
        assert_eq!(engines.get(&5), Some(&0), "the live book is");
        // ...and an engine capped to paper goes to the lower id, whichever order the rows came in.
        let paper = [block("hyperliquid", None, "BTC", Some(EngineMode::Paper))];
        let mut reversed = dir.clone();
        reversed.accounts.reverse();
        for d in [&dir, &reversed] {
            let engines = engines_run(d, &paper);
            assert_eq!((engines.get(&4), engines.get(&5)), (Some(&0), None));
        }
        assert_eq!(why_not(false), Some(NOT_RUNNING_WHY));
        assert_eq!(NOT_RUNNING_WHY, "Not running on the server.");
        assert!(why_not(true).is_none());
    }

    #[test]
    fn split_base_quote_prefers_the_longest_quote_suffix() {
        // USDT/USDC must win over the USD prefix of their own names.
        assert_eq!(split_base_quote("BTCUSDT"), ("BTC", "USDT"));
        assert_eq!(split_base_quote("SOLUSDC"), ("SOL", "USDC"));
        assert_eq!(split_base_quote("XBTUSD"), ("XBT", "USD"));
    }

    #[test]
    fn split_base_quote_defaults_an_unknown_quote_to_usdt() {
        assert_eq!(split_base_quote("EURGBP"), ("EURGBP", "USDT"));
        assert_eq!(split_base_quote("BTC-PERPETUAL"), ("BTC-PERPETUAL", "USDT"));
    }

    #[test]
    fn split_base_quote_never_yields_an_empty_base() {
        // a symbol that IS only the quote suffix keeps the whole symbol as the base label
        assert_eq!(split_base_quote("USDT"), ("USDT", "USDT"));
        assert_eq!(split_base_quote("USD"), ("USD", "USD"));
    }

    // ---- I13: the catalog answers are cached, and a cache never answers for the wrong catalog ---

    /// The grid is read from the catalog the window was handed THIS frame: a refreshed catalog
    /// (the shell replaces its `Arc`) and another instrument are both read again, and the symbol
    /// picker's matches follow the query and the catalog alike.
    #[test]
    fn the_catalog_answers_follow_the_instrument_the_query_and_a_refreshed_catalog() {
        let snap = snap_with(vec![
            block("binance", None, "BTCUSDT", Some(EngineMode::Live)),
            block("okx", None, "BTC-USDT-SWAP", Some(EngineMode::Demo)),
        ]);
        let empty = Arc::new(Catalog::from_instruments(Vec::new()));
        let mut c = TradeCatalog::default();
        sync_on(&mut c, &empty, &snap, ("binance", "BTCUSDT", "BTC"));
        assert!(c.listing.is_none() && !c.loaded, "nothing is listed before the catalog loads");
        assert!(c.matches.is_empty());

        let full = btc_catalog();
        sync_on(&mut c, &full, &snap, ("binance", "BTCUSDT", "BTC"));
        assert!(c.loaded);
        assert_eq!(c.listing.as_ref().map(|l| l.grid.lot), Some(0.001), "a new catalog is read");
        assert!(!c.matches.is_empty(), "...and so are the matches for an unchanged query");

        sync_on(&mut c, &full, &snap, ("okx", "BTC-USDT-SWAP", "BTC"));
        assert_eq!(c.listing.as_ref().map(|l| l.grid.lot), Some(0.01), "a new instrument too");

        sync_on(&mut c, &full, &snap, ("okx", "BTC-USDT-SWAP", "SWAP"));
        let found: Vec<&str> = c.matches.iter().map(|m| m.symbol.as_str()).collect();
        assert_eq!(found, ["BTC-USDT-SWAP"], "a new query searches again");
    }

    /// Fix round 1, I-1: the other venues' spellings depend on the window instrument's CLASS as well
    /// as its base and quote. A window moved from okx's swap to okx's spot pair (same base, same
    /// quote) must stop opening binance's PERP from its binance row.
    #[test]
    fn another_venues_spelling_follows_the_window_instruments_class() {
        let snap = two_accounts();
        let cat = btc_catalog();
        let mut c = TradeCatalog::default();
        sync_on(&mut c, &cat, &snap, ("okx", "BTC-USDT-SWAP", ""));
        assert_eq!(c.spelling_on("binance"), Some("BTCUSDT.P"), "a swap window opens the perp");
        sync_on(&mut c, &cat, &snap, ("okx", "BTC-USDT", ""));
        assert_eq!(c.spelling_on("binance"), Some("BTCUSDT"), "a spot window opens the spot pair");
        sync_on(&mut c, &cat, &snap, ("okx", "BTC-USDT-SWAP", ""));
        assert_eq!(c.spelling_on("binance"), Some("BTCUSDT.P"), "...and back");
    }

    /// Fix round 1, M-5: the window's own listing is an EXACT lookup. A venue whose raw symbols are
    /// mixed-case (hyperliquid's `kPEPE`) is listed, not reported as missing with every size
    /// refused.
    #[test]
    fn a_mixed_case_symbol_is_found_in_the_catalog() {
        let snap = snap_with(vec![block("hyperliquid", None, "kPEPE", Some(EngineMode::Live))]);
        let mut pepe = inst("hyperliquid", "kPEPE", "USDC", AssetClass::CryptoPerp, 1.0);
        pepe.base = "kPEPE".into();
        let cat = Arc::new(Catalog::from_instruments(vec![pepe]));
        let c = synced(&cat, &snap, "hyperliquid", "kPEPE", "");
        assert_eq!(c.listing.as_ref().map(|l| l.grid.lot), Some(1.0));
        let p = window_parts(&sources(&snap, None, &c, ("hyperliquid", None, "kPEPE")));
        assert_eq!(p.grid.lot, 1.0);
        assert_eq!(p.status, None, "nothing is missing: {:?}", p.status);
    }

    /// Fix round 1, M-8: the picker searches the venues the server runs, so a query that many
    /// instruments on other venues match still finds the running venue's.
    #[test]
    fn the_picker_finds_a_running_venues_match_however_many_other_venues_match() {
        let snap = snap_with(vec![block("binance", None, "BTCUSDC", Some(EngineMode::Live))]);
        let mut items: Vec<Instrument> = (0..250)
            .map(|n| inst("bybit", &format!("BTC{n}USDT"), "USDT", AssetClass::CryptoSpot, 0.1))
            .collect();
        items.push(inst("binance", "BTCUSDC", "USDC", AssetClass::CryptoSpot, 0.001));
        let cat = Arc::new(Catalog::from_instruments(items));
        let c = synced(&cat, &snap, "binance", "BTCUSDC", "BTC");
        let p = window_parts(&sources(&snap, None, &c, ("binance", None, "BTCUSDC")));
        let found: Vec<(&str, &str)> = p.matches.iter().map(|m| (m.venue, m.symbol)).collect();
        assert_eq!(found, [("binance", "BTCUSDC")]);
    }

    // ---- I14: the window's inputs, built by one pure function ---------------------------------

    /// Spec §7's wiring list: the account picker is built from a two-account snapshot, the default
    /// account first; an account the database holds but the server does not run is greyed with
    /// its reason and never with `armed`; a venue with no account is listed to connect.
    #[test]
    fn the_account_picker_lists_both_accounts_default_first_and_greys_one_not_running() {
        let snap = two_accounts();
        let dir = two_accounts_directory();
        let cat = synced(&btc_catalog(), &snap, "binance", "BTCUSDT", "");
        let p = window_parts(&sources(&snap, Some(&dir), &cat, ("binance", None, "BTCUSDT")));
        let rows: Vec<_> = p
            .accounts
            .iter()
            .map(|r| (r.venue_label, r.account, r.name, r.symbol, r.mode, r.why_not))
            .collect();
        assert_eq!(
            rows,
            [
                ("Binance", None, "main", "BTCUSDT", AccountMode::Live, None),
                ("Binance", Some("SUB"), "SUB", "BTCUSDT", AccountMode::Demo, None),
                ("OKX", None, "main", "BTC-USDT", AccountMode::Demo, Some(NOT_RUNNING_WHY)),
            ]
        );
        let unconnected: Vec<&str> = p.unconnected.iter().map(|u| u.venue_label).collect();
        assert_eq!(unconnected, ["bybit"], "a venue with no account, by its key while untitled");
    }

    /// Against a node that predates the `Directory` request the list is the snapshot's running
    /// accounts, named by their keys.
    #[test]
    fn an_older_node_lists_the_running_accounts_by_their_keys() {
        let snap = two_accounts();
        let cat = synced(&btc_catalog(), &snap, "binance", "BTCUSDT", "");
        let p = window_parts(&sources(&snap, None, &cat, ("binance", Some("SUB"), "BTCUSDT")));
        let rows: Vec<_> =
            p.accounts.iter().map(|r| (r.venue_label, r.account, r.name, r.why_not)).collect();
        assert_eq!(rows, [("binance", None, "main", None), ("binance", Some("SUB"), "SUB", None)]);
        assert!(p.unconnected.is_empty());
    }

    /// The mode chip reads the published mode, and a block that publishes none reads as unknown —
    /// never as PAPER.
    #[test]
    fn the_mode_chip_reads_the_published_mode_and_unknown_when_none_is_published() {
        let snap = two_accounts();
        let cat = synced(&btc_catalog(), &snap, "binance", "BTCUSDT", "");
        let at =
            |account| window_parts(&sources(&snap, None, &cat, ("binance", account, "BTCUSDT")));
        assert_eq!(at(None).mode, AccountMode::Live);
        assert_eq!(at(Some("SUB")).mode, AccountMode::Demo);

        let older = snap_with(vec![block("binance", None, "", None)]);
        let p = window_parts(&sources(&older, None, &cat, ("binance", None, "BTCUSDT")));
        assert_eq!(p.mode, AccountMode::Unknown);
    }

    /// The buying-power sizes read the window account's own `free_bp`; an account the server does
    /// not run has none.
    #[test]
    fn buying_power_is_the_window_accounts_free_bp() {
        let snap = two_accounts();
        let cat = synced(&btc_catalog(), &snap, "binance", "BTCUSDT", "");
        let bp = |venue, account| {
            window_parts(&sources(&snap, None, &cat, (venue, account, "BTCUSDT"))).buying_power
        };
        assert_eq!(bp("binance", None), Some(1500.0));
        assert_eq!(bp("binance", Some("SUB")), Some(250.0));
        assert_eq!(bp("bybit", None), None);
    }

    /// The window's markers are `order_dispatch::window_orders`' — live orders only, of its own
    /// account — so what is marked is what Cancel all pulls. A marker carries the unfilled size.
    #[test]
    fn the_markers_are_the_dispatchers_window_orders() {
        let mut snap = two_accounts();
        let mut partial = order("a1", None, OrderStatus::PartiallyFilled);
        partial.filled_qty = 0.25;
        snap.orders = vec![
            partial,
            order("a2", None, OrderStatus::Filled),
            order("s1", Some("SUB"), OrderStatus::Accepted),
        ];
        let cat = synced(&btc_catalog(), &snap, "binance", "BTCUSDT", "");
        for account in [None, Some("SUB")] {
            let p = window_parts(&sources(&snap, None, &cat, ("binance", account, "BTCUSDT")));
            let addr = TradeAddress::parse("binance", account, "BTCUSDT").expect("an address");
            let shared: Vec<&str> =
                window_orders(&snap, &addr).map(|o| o.client_order_id.as_str()).collect();
            let marked: Vec<&str> = p.orders.iter().map(|o| o.client_order_id.as_str()).collect();
            assert_eq!(marked, shared, "{account:?}: one rule for the markers and the cancels");
            assert_eq!(p.orders_why, None);
        }
        let p = window_parts(&sources(&snap, None, &cat, ("binance", None, "BTCUSDT")));
        assert_eq!(p.orders[0].qty, 0.75, "a marker is the unfilled size");
    }

    /// Ruling R9: on an older node whose two blocks publish no mode, no order names its account, so
    /// the window shows no own-order markers and says why.
    #[test]
    fn an_older_two_block_node_shows_no_markers_and_says_why() {
        let mut snap = snap_with(vec![
            block("binance", None, "", None),
            block("binance", Some("SUB"), "", None),
        ]);
        snap.orders = vec![order("a1", None, OrderStatus::Accepted)];
        let cat = synced(&btc_catalog(), &snap, "binance", "BTCUSDT", "");
        let p = window_parts(&sources(&snap, None, &cat, ("binance", None, "BTCUSDT")));
        assert!(p.orders.is_empty());
        assert_eq!(p.orders_why, Some(ORDERS_UNATTRIBUTED_WHY));
        assert_eq!(
            p.status,
            Some((StatusKind::Info, Cow::Borrowed(ORDERS_UNATTRIBUTED_WHY))),
            "the strip says it too while nothing else is on it"
        );
    }

    /// A TP/SL bracket reaches only a venue's single default book (Ruling R3): the window offers it
    /// there and nowhere else, and says the ACCOUNT reason everywhere else.
    #[test]
    fn brackets_are_routable_only_on_a_venues_single_default_book() {
        let mut snap = two_accounts();
        snap.portfolio.venues.push(block("okx", None, "BTC-USDT", Some(EngineMode::Demo)));
        let cat = synced(&btc_catalog(), &snap, "binance", "BTCUSDT", "");
        let why = |venue, account, symbol| {
            window_parts(&sources(&snap, None, &cat, (venue, account, symbol))).bracket_why
        };
        assert_eq!(why("okx", None, "BTC-USDT"), None, "the one default book of okx");
        let account = Some(TPSL_ACCOUNT_WHY);
        assert_eq!(why("binance", None, "BTCUSDT"), account, "binance runs a second account");
        assert_eq!(why("binance", Some("SUB"), "BTCUSDT"), account, "a labelled account");
    }

    /// I-1 of the final review (slice B), step 3: the glue STATES why TP/SL is off, once, before
    /// the click. The shipped daemon's shape — ONE default binance engine mounted on spot `BTCUSDT`
    /// — gets the LANE reason (never the account one, which would be false there); a perp engine
    /// (`BTCUSDT.P`) offers TP/SL; a second account on the venue is the ACCOUNT reason, which
    /// outranks the lane. An engine that does not say what it trades is not judged.
    #[test]
    fn the_glue_says_why_tpsl_is_off_before_the_click() {
        let at = |snap: &CoreSnapshot, symbol: &str| {
            let cat = synced(&btc_catalog(), snap, "binance", symbol, "");
            window_parts(&sources(snap, None, &cat, ("binance", None, symbol))).bracket_why
        };
        let spot = snap_with(vec![block("binance", None, "BTCUSDT", Some(EngineMode::Live))]);
        assert_eq!(at(&spot, "BTCUSDT"), Some(TPSL_LANE_WHY), "the shipped daemon's spot engine");
        let perp = snap_with(vec![block("binance", None, "BTCUSDT.P", Some(EngineMode::Live))]);
        assert_eq!(at(&perp, "BTCUSDT.P"), None, "a perp engine takes a bracket");
        assert_eq!(at(&two_accounts(), "BTCUSDT"), Some(TPSL_ACCOUNT_WHY), "the account first");
        let older = snap_with(vec![block("binance", None, "", None)]);
        assert_eq!(at(&older, "BTCUSDT"), None, "an engine that does not say is not judged");
    }

    /// The shipped daemon's shape drawn through the REAL widget: the TP/SL toggle is disabled
    /// BEFORE any click and its hover says the lane reason the glue stated (I-1, step 3, end to
    /// end), and the account picker of a window whose node did not answer the directory says
    /// "Account list unavailable." (the owner's decision of 10-03, item 5). CONTROL: a perp engine's
    /// toggle can be turned on.
    #[test]
    fn the_shipped_daemon_shape_reaches_the_widget_with_its_reasons() {
        use egui_kittest::Harness;
        use egui_kittest::kittest::{NodeT, Queryable};
        let frame = |symbol: &'static str| {
            move |ui: &mut egui::Ui, state: &mut vike_panels::trade::TradeState| {
                if !vike_ui_theme::harness::type_ready(ui.ctx()) {
                    return;
                }
                ui.ctx().all_styles_mut(|s| s.interaction.tooltip_delay = 0.0);
                let snap = snap_with(vec![block("binance", None, symbol, Some(EngineMode::Demo))]);
                let cat = synced(&btc_catalog(), &snap, "binance", symbol, "");
                let src = WindowSources {
                    directory_unavailable: true,
                    bracket_wire: true,
                    ..sources(&snap, None, &cat, ("binance", None, symbol))
                };
                let parts = window_parts(&src);
                let _ = vike_panels::trade::draw(ui, state, &parts.inputs());
            }
        };
        let size = vike_panels::trade::layout::BESIDE_SIZE;
        let mut h = Harness::builder()
            .with_size(size)
            .build_ui_state(frame("BTCUSDT"), vike_panels::trade::TradeState::default());
        h.run();
        let toggle = h.get_by_label("TP/SL").accesskit_node();
        assert!(toggle.is_disabled(), "TP/SL is off before the click on a spot engine");
        h.hover_at(egui::pos2(1.0, 1.0));
        h.run();
        let before = h.query_all_by_label(TPSL_LANE_WHY).count();
        h.get_by_label("TP/SL").hover();
        h.run();
        assert!(h.query_all_by_label(TPSL_LANE_WHY).count() > before, "the hover says the lane");

        // The strip says it too (nothing more important is on it), so count the picker's line.
        let before = h.query_all_by_label(ACCOUNT_LIST_UNAVAILABLE).count();
        h.get_by_label_contains("binance · main").click();
        h.run();
        assert!(
            h.query_all_by_label(ACCOUNT_LIST_UNAVAILABLE).count() > before,
            "the account picker says the list is unavailable"
        );

        let mut h = Harness::builder()
            .with_size(size)
            .build_ui_state(frame("BTCUSDT.P"), vike_panels::trade::TradeState::default());
        h.run();
        assert!(!h.get_by_label("TP/SL").accesskit_node().is_disabled(), "a perp engine offers it");
    }

    /// The owner's decision of 10-03, item 5: a failed directory fetch is SAID in the window. The
    /// account picker carries "Account list unavailable." (the widget draws it at the top of the
    /// list), and the strip says it too where nothing more important is on it; the window stays
    /// usable — venues by their keys, the accounts the snapshot runs.
    #[test]
    fn a_window_whose_node_did_not_answer_the_directory_says_the_list_is_unavailable() {
        let snap = two_accounts();
        let cat = synced(&btc_catalog(), &snap, "binance", "BTCUSDT", "");
        let src = WindowSources {
            directory_unavailable: true,
            ..sources(&snap, None, &cat, ("binance", None, "BTCUSDT"))
        };
        let p = window_parts(&src);
        assert_eq!(p.accounts_why, Some(ACCOUNT_LIST_UNAVAILABLE));
        assert!(p.tradable, "the window stays usable");
        assert_eq!(p.accounts.len(), 2, "the accounts the snapshot runs are listed");
        let strip = p.status.as_ref().map(|(k, t)| (*k, &**t));
        assert_eq!(strip, Some((StatusKind::Info, ACCOUNT_LIST_UNAVAILABLE)), "the strip says it");

        // Anything more important on the strip keeps it: here the window's own account is not run.
        let src = WindowSources {
            directory_unavailable: true,
            ..sources(&snap, None, &cat, ("bybit", None, "BTCUSDT"))
        };
        let p = window_parts(&src);
        assert_eq!(p.accounts_why, Some(ACCOUNT_LIST_UNAVAILABLE), "the picker still says it");
        assert_eq!(p.status.as_ref().map(|(_, t)| &**t), Some(NOT_RUNNING_WHY));

        // CONTROL: a node that answered (or one not asked yet) says nothing of the kind.
        let p = window_parts(&sources(&snap, None, &cat, ("binance", None, "BTCUSDT")));
        assert_eq!(p.accounts_why, None);
        assert_eq!(p.status, None);
    }

    /// The owner's decision of 10-03, item 11: a desktop that cannot send — READ-ONLY (no control
    /// channel) or with its control link LOST — is a STATED cause up front, in the strip and in
    /// `Tradable::No::why`, so every order control says it before the click. Every symbol match is
    /// view only there. Their place among the other causes: Error causes before Info ones (an Error
    /// outranks the window's own last line on the strip), the widest scope first within each — the
    /// server, then this desktop's link, then the window's address, then the account; and among the
    /// Info causes this desktop's read-only setting before a node that has said nothing and an
    /// account it does not run.
    #[test]
    fn a_desktop_that_cannot_send_says_so_up_front() {
        let snap = two_accounts();
        let cat = synced(&btc_catalog(), &snap, "binance", "BTCUSDT", "BTC");
        let at = |snap: &CoreSnapshot, link, account| {
            let src = WindowSources {
                control_link: link,
                ..sources(snap, None, &cat, ("binance", account, "BTCUSDT"))
            };
            let p = window_parts(&src);
            let why = match p.inputs().tradable {
                Tradable::No { why, .. } => why.map(str::to_string),
                Tradable::Yes => None,
            };
            let strip = p.status.as_ref().map(|(k, t)| (*k, t.to_string()));
            (p.tradable, why, strip, p.matches.iter().any(|m| m.tradable))
        };
        let read_only = at(&snap, ControlLink::ReadOnly, None);
        assert_eq!(
            read_only,
            (
                false,
                Some(READ_ONLY_WHY.to_string()),
                Some((StatusKind::Info, READ_ONLY_WHY.to_string())),
                false
            )
        );
        let lost = at(&snap, ControlLink::Lost, None);
        assert_eq!(
            lost,
            (
                false,
                Some(NO_CONTROL_LINK_WHY.to_string()),
                Some((StatusKind::Error, NO_CONTROL_LINK_WHY.to_string())),
                false
            )
        );
        assert!(at(&snap, ControlLink::Open, None).0, "CONTROL: a desktop that sends trades");

        // The order among the causes.
        let mut faulted = two_accounts();
        faulted.fault = Some("handler panicked".into());
        let mut halted = two_accounts();
        halted.portfolio.venues[1].trading_state = vike_exec::TradingState::Halted;
        let why = |snap: &CoreSnapshot, link, account| at(snap, link, account).1;
        let stopped = stopped_why("handler panicked");
        assert_eq!(why(&faulted, ControlLink::Lost, None), Some(stopped.clone()), "server first");
        assert_eq!(why(&faulted, ControlLink::ReadOnly, None), Some(stopped));
        assert_eq!(
            why(&snap, ControlLink::Lost, Some("sub")),
            Some(NO_CONTROL_LINK_WHY.to_string()),
            "the link before the window's address"
        );
        assert_eq!(
            why(&halted, ControlLink::Lost, Some("SUB")),
            Some(NO_CONTROL_LINK_WHY.to_string()),
            "the link before the account"
        );
        assert_eq!(
            why(&halted, ControlLink::ReadOnly, Some("SUB")),
            Some(HALTED_WHY.to_string()),
            "an Error cause before the read-only setting"
        );
        assert_eq!(
            why(&snap, ControlLink::ReadOnly, Some("sub")),
            Some(UNADDRESSABLE_WHY.to_string()),
            "an Error cause before the read-only setting"
        );
        let waiting = CoreSnapshot::empty("", "");
        assert_eq!(
            why(&waiting, ControlLink::ReadOnly, None),
            Some(READ_ONLY_WHY.to_string()),
            "read-only before a node that has said nothing"
        );
        let elsewhere = WindowSources {
            control_link: ControlLink::ReadOnly,
            ..sources(&snap, None, &cat, ("bybit", None, "BTCUSDT"))
        };
        assert!(
            matches!(
                window_parts(&elsewhere).inputs().tradable,
                Tradable::No { why: Some(READ_ONLY_WHY), .. }
            ),
            "read-only before an account the server does not run"
        );
    }

    /// The shell's one value: a desktop with no control handle is read-only, one whose handle has
    /// lost its link cannot send either, and only a connected handle sends.
    #[test]
    fn the_control_link_is_read_off_the_handle() {
        assert_eq!(ControlLink::of(None), ControlLink::ReadOnly);
        assert_eq!(ControlLink::of(Some(false)), ControlLink::Lost);
        assert_eq!(ControlLink::of(Some(true)), ControlLink::Open);
    }

    /// A symbol the account's engine does not trade is offered to look at, not to trade, and the
    /// reason names what the account does trade.
    #[test]
    fn a_symbol_the_account_does_not_trade_is_view_only() {
        let snap = two_accounts();
        let cat = synced(&btc_catalog(), &snap, "binance", "BTCUSDC", "");
        let p = window_parts(&sources(&snap, None, &cat, ("binance", Some("SUB"), "BTCUSDC")));
        assert!(!p.tradable);
        assert_eq!(p.trades, ["BTCUSDT"]);
        assert_eq!(
            p.inputs().tradable,
            Tradable::No { trades: &["BTCUSDT".to_string()], why: None },
            "no cause stated: the list says it, and the widget words it"
        );
    }

    /// The F wave's reason slot: every cause the glue can STATE for a window that takes no order
    /// reaches the widget as `Tradable::No::why` in the very words its status strip says — ONE
    /// text, computed once, so the ticket's line, the ladder's hint and the disabled buttons can
    /// never word it differently from the strip, and the widget never has to read the strip to
    /// guess it. A stopped core (with blocks, and the empty snapshot a fault while publishing
    /// leaves), an account text that names no account, a halted account, an account the server
    /// does not run, a node that has said nothing yet. With no window status the strip shows the
    /// cause itself, an ERROR for the first four kinds and an Info line for the last two.
    #[test]
    fn each_stated_cause_reaches_the_widget_in_the_strips_own_words() {
        let mut faulted = two_accounts();
        faulted.fault = Some("handler panicked".into());
        let mut faulted_empty = CoreSnapshot::empty("binance", "BTCUSDT");
        faulted_empty.fault = Some("panic during snapshot publish".into());
        let mut halted = two_accounts();
        halted.portfolio.venues[1].trading_state = vike_exec::TradingState::Halted;
        let (running, waiting) = (two_accounts(), CoreSnapshot::empty("", ""));
        let mut faulted_and_halted = halted.clone();
        faulted_and_halted.fault = Some("handler panicked".into());
        let error = StatusKind::Error;
        let cases = [
            ("a stopped core", &faulted, ("binance", None), error, stopped_why("handler panicked")),
            (
                "a stopped core, empty snapshot",
                &faulted_empty,
                ("binance", None),
                error,
                stopped_why("panic during snapshot publish"),
            ),
            ("no label", &running, ("binance", Some("sub")), error, UNADDRESSABLE_WHY.to_string()),
            ("a halted account", &halted, ("binance", Some("SUB")), error, HALTED_WHY.to_string()),
            (
                "not running",
                &running,
                ("bybit", None),
                StatusKind::Info,
                NOT_RUNNING_WHY.to_string(),
            ),
            (
                "waiting",
                &waiting,
                ("binance", None),
                StatusKind::Info,
                WAITING_FOR_NODE.to_string(),
            ),
            // F2 m5: a core in its safe state with a HALTED account — the two together, as the
            // core leaves them — says the fault's words, never the halt's.
            (
                "a stopped core over a halted account",
                &faulted_and_halted,
                ("binance", Some("SUB")),
                error,
                stopped_why("handler panicked"),
            ),
        ];
        for (what, snap, (venue, account), kind, words) in cases {
            let cat = synced(&btc_catalog(), snap, venue, "BTCUSDT", "");
            let p = window_parts(&sources(snap, None, &cat, (venue, account, "BTCUSDT")));
            assert!(!p.tradable, "{what}");
            match p.inputs().tradable {
                Tradable::No { why, .. } => {
                    assert_eq!(why, Some(words.as_str()), "{what}: the widget is told the cause");
                }
                Tradable::Yes => panic!("{what}: the window takes orders"),
            }
            let strip = p.status.as_ref().map(|(k, t)| (*k, &**t));
            assert_eq!(strip, Some((kind, words.as_str())), "{what}: the strip says the same");
        }
    }

    /// The reason slot under the strip's precedence: the window's own last order, note or refusal
    /// outranks an Info cause ON THE STRIP (not running, waiting) and the stated cause still reaches
    /// the widget — the strip's line is not the widget's cause. An ERROR cause (a stopped core)
    /// outranks the window's own line on the strip too. A REDUCING account and an instrument with
    /// no lot size are not causes: the window still takes orders (the ticket refuses a size it
    /// cannot send in its own words).
    #[test]
    fn the_stated_cause_is_the_widgets_whatever_the_strip_shows() {
        let rejected: (StatusKind, String) =
            (StatusKind::Error, "Rejected by the venue: insufficient margin.".into());
        let snap = two_accounts();
        let cat = synced(&btc_catalog(), &snap, "bybit", "BTCUSDT", "");
        let src = WindowSources {
            status: Some(&rejected),
            ..sources(&snap, None, &cat, ("bybit", None, "BTCUSDT"))
        };
        let p = window_parts(&src);
        let strip = p.status.as_ref().map(|(k, t)| (*k, &**t));
        assert_eq!(strip, Some((StatusKind::Error, rejected.1.as_str())), "the window's own line");
        assert!(
            matches!(p.inputs().tradable, Tradable::No { why: Some(NOT_RUNNING_WHY), .. }),
            "{:?}",
            p.inputs().tradable
        );

        let mut faulted = two_accounts();
        faulted.fault = Some("handler panicked".into());
        let cat = synced(&btc_catalog(), &faulted, "binance", "BTCUSDT", "");
        let src = WindowSources {
            status: Some(&rejected),
            ..sources(&faulted, None, &cat, ("binance", None, "BTCUSDT"))
        };
        let p = window_parts(&src);
        let stopped = stopped_why("handler panicked");
        let strip = p.status.as_ref().map(|(k, t)| (*k, &**t));
        assert_eq!(strip, Some((StatusKind::Error, stopped.as_str())), "the fault outranks it");
        assert!(matches!(p.inputs().tradable, Tradable::No { why: Some(w), .. } if w == stopped));

        let mut reducing = two_accounts();
        reducing.portfolio.venues[1].trading_state = vike_exec::TradingState::Reducing;
        let cat = synced(&btc_catalog(), &reducing, "binance", "BTCUSDT", "");
        let p = window_parts(&sources(&reducing, None, &cat, ("binance", Some("SUB"), "BTCUSDT")));
        assert_eq!(p.inputs().tradable, Tradable::Yes, "a reducing account still trades");

        let unlisted = snap_with(vec![block("binance", None, "ETHUSDT", Some(EngineMode::Live))]);
        let cat = synced(&btc_catalog(), &unlisted, "binance", "ETHUSDT", "");
        let p = window_parts(&sources(&unlisted, None, &cat, ("binance", None, "ETHUSDT")));
        assert_eq!(p.inputs().tradable, Tradable::Yes, "no lot size is the ticket's to refuse");
    }

    /// Each match is "view only" where the account it would open on does not trade it, and carries
    /// no price: no last price reaches the desktop (a snapshot from the node carries no marks; the
    /// FW2 review), so the picker's row prints a dash. Matches are on venues the server runs.
    #[test]
    fn a_match_carries_whether_it_can_be_traded_and_no_price() {
        let snap = snap_with(vec![
            block("binance", None, "BTCUSDT", Some(EngineMode::Live)),
            block("okx", None, "BTC-USDT-SWAP", Some(EngineMode::Demo)),
        ]);
        let cat = synced(&btc_catalog(), &snap, "binance", "BTCUSDT", "BTC");
        let p = window_parts(&sources(&snap, None, &cat, ("binance", None, "BTCUSDT")));
        let found: Vec<_> =
            p.matches.iter().map(|m| (m.venue, m.symbol, m.last, m.tradable)).collect();
        assert!(found.contains(&("binance", "BTCUSDT", None, true)), "{found:?}");
        assert!(found.contains(&("okx", "BTC-USDT-SWAP", None, true)), "{found:?}");
        assert!(found.contains(&("okx", "BTC-USDT", None, false)), "view only");
        assert!(!found.iter().any(|m| m.0 == "bybit"), "no engine on bybit: no match there");
    }

    /// C3: a row on another venue opens THAT venue's spelling of the instrument — the same class
    /// and quote where it lists one — never the window's own symbol.
    #[test]
    fn an_account_row_on_another_venue_opens_that_venues_own_symbol() {
        let snap = snap_with(vec![
            block("okx", None, "BTC-USDT-SWAP", Some(EngineMode::Demo)),
            block("binance", None, "BTCUSDT", Some(EngineMode::Live)),
            block("bybit", None, "BTCUSDT", Some(EngineMode::Paper)),
        ]);
        let cat = synced(&btc_catalog(), &snap, "okx", "BTC-USDT-SWAP", "");
        let p = window_parts(&sources(&snap, None, &cat, ("okx", None, "BTC-USDT-SWAP")));
        let opens: Vec<(&str, &str)> = p.accounts.iter().map(|r| (r.venue, r.symbol)).collect();
        assert_eq!(
            opens,
            [("okx", "BTC-USDT-SWAP"), ("binance", "BTCUSDT.P"), ("bybit", "BTCUSDT")],
            "the swap window opens binance's perp, and bybit's only BTC/USDT pair"
        );

        let cat = synced(&btc_catalog(), &snap, "binance", "BTCUSDT", "");
        let p = window_parts(&sources(&snap, None, &cat, ("binance", None, "BTCUSDT")));
        let okx = p.accounts.iter().find(|r| r.venue == "okx").expect("an okx row");
        assert_eq!(okx.symbol, "BTC-USDT", "the spot window opens okx's spot pair");
    }

    /// C3: a recent pick is an address only with its venue.
    #[test]
    fn recent_picks_carry_their_venue() {
        let snap = two_accounts();
        let cat = synced(&btc_catalog(), &snap, "binance", "BTCUSDT", "");
        let recent = [("okx".to_string(), "BTC-USDT-SWAP".to_string())];
        let src = WindowSources {
            recent: &recent,
            ..sources(&snap, None, &cat, ("binance", None, "BTCUSDT"))
        };
        assert_eq!(window_parts(&src).recent, [("okx", "BTC-USDT-SWAP")]);
    }

    /// A1's money-path carry: an account text that is not a label never becomes the DEFAULT
    /// account. The window refuses — it trades nothing, offers no bracket, and says why.
    #[test]
    fn an_account_text_that_is_not_a_label_refuses_the_window() {
        let mut snap = two_accounts();
        snap.orders = vec![order("a1", None, OrderStatus::Accepted)];
        let cat = synced(&btc_catalog(), &snap, "binance", "BTCUSDT", "");
        assert_eq!(TradeAddress::parse("binance", Some("sub"), "BTCUSDT"), None);
        let p = window_parts(&sources(&snap, None, &cat, ("binance", Some("sub"), "BTCUSDT")));
        assert_eq!(p.account, Some("sub"), "the bar shows what the window holds");
        assert!(!p.tradable && p.bracket_why.is_some());
        assert!(p.orders.is_empty(), "the default account's orders are not this window's");
        assert_eq!(p.mode, AccountMode::Unknown);
        assert_eq!(p.status, Some((StatusKind::Error, Cow::Borrowed(UNADDRESSABLE_WHY))));
        // Fix round 1, M-7: the cancel row is worded with the same reason, beyond the carry's
        // `orders_why` formula (which covers Ruling R9 alone).
        assert_eq!(p.orders_why, Some(UNADDRESSABLE_WHY));
    }

    // ---- fix round 1: a stopped core, a halted account, an account the server does not run ----

    /// I-2: a core that faulted while publishing leaves a snapshot with NO blocks and its fault.
    /// The window says the server stopped, never "Waiting for the node.", and trades nothing.
    #[test]
    fn a_faulted_empty_snapshot_says_the_server_stopped() {
        let mut snap = CoreSnapshot::empty("binance", "BTCUSDT");
        snap.fault = Some("panic during snapshot publish".into());
        let cat = synced(&btc_catalog(), &snap, "binance", "BTCUSDT", "");
        let p = window_parts(&sources(&snap, None, &cat, ("binance", None, "BTCUSDT")));
        assert!(!p.tradable);
        let (kind, text) = p.status.expect("the fault is on the strip");
        assert_eq!(kind, StatusKind::Error);
        assert!(text.contains("panic during snapshot publish"), "{text}");
        assert_ne!(text, WAITING_FOR_NODE);
    }

    /// I-2: a core in safe state keeps publishing its blocks. Every window on it trades nothing and
    /// says why, ahead of any order status, and its matches are all view only.
    #[test]
    fn a_faulted_core_with_blocks_trades_nothing_and_says_why() {
        let mut snap = two_accounts();
        snap.fault = Some("handler panicked".into());
        let cat = synced(&btc_catalog(), &snap, "binance", "BTCUSDT", "BTC");
        let filled = (StatusKind::Ok, "Buy 1 limit · FILLED".to_string());
        let src = WindowSources {
            status: Some(&filled),
            ..sources(&snap, None, &cat, ("binance", None, "BTCUSDT"))
        };
        let p = window_parts(&src);
        assert!(!p.tradable, "the core halts every order");
        let (kind, text) = p.status.expect("the fault is on the strip");
        assert_eq!(kind, StatusKind::Error);
        assert!(text.contains("handler panicked"), "{text}");
        assert!(p.matches.iter().all(|m| !m.tradable), "every match is view only");
    }

    /// I-2: an account whose engine is HALTED trades nothing from its window and says so; the
    /// other account of the same venue is unaffected. A REDUCING one still trades, and says it takes
    /// only orders that reduce its position.
    #[test]
    fn a_halted_account_trades_nothing_and_a_reducing_one_says_so() {
        let mut snap = two_accounts();
        snap.portfolio.venues[1].trading_state = vike_exec::TradingState::Halted;
        let cat = synced(&btc_catalog(), &snap, "binance", "BTCUSDT", "");
        let at =
            |account| window_parts(&sources(&snap, None, &cat, ("binance", account, "BTCUSDT")));
        let sub = at(Some("SUB"));
        assert!(!sub.tradable);
        let (kind, text) = sub.status.expect("the halt is on the strip");
        assert_eq!(kind, StatusKind::Error);
        assert!(text.contains("halted"), "{text}");
        assert!(at(None).tradable, "the default account still trades");

        snap.portfolio.venues[1].trading_state = vike_exec::TradingState::Reducing;
        let sub = window_parts(&sources(&snap, None, &cat, ("binance", Some("SUB"), "BTCUSDT")));
        assert!(sub.tradable, "a reducing account still closes and reduces");
        let (kind, text) = sub.status.expect("the strip says what it takes");
        assert_eq!(kind, StatusKind::Info);
        assert!(text.contains("reduce"), "{text}");
    }

    /// M-1: no primary block, no seed — the window waits rather than folding "unknown" into the
    /// default account.
    #[test]
    fn a_snapshot_with_no_primary_block_seeds_nothing() {
        assert_eq!(seed(&CoreSnapshot::empty("binance", "BTCUSDT"), None), None);
    }

    /// M-1: a window left on an account the server does not run says so on the strip.
    #[test]
    fn a_window_on_an_account_the_server_does_not_run_says_so() {
        let snap = two_accounts();
        let cat = synced(&btc_catalog(), &snap, "bybit", "BTCUSDT", "");
        let p = window_parts(&sources(&snap, None, &cat, ("bybit", None, "BTCUSDT")));
        assert!(!p.tradable);
        assert_eq!(p.status, Some((StatusKind::Info, Cow::Borrowed(NOT_RUNNING_WHY))));
    }

    /// M-2: a row's account is a ROUTING answer (picking it addresses orders), so it is read from
    /// the block's route key. A block whose label did not survive the wire offers no row: as
    /// "main" it would route to the default account.
    #[test]
    fn a_block_with_a_malformed_label_offers_no_row() {
        let mut snap = two_accounts();
        snap.portfolio.venues.push(VenueBlock {
            venue: "binance".into(),
            account: None,
            route_key: "binance#sub".into(),
            symbol: "BTCUSDT".into(),
            mode: Some(EngineMode::Live),
            ..Default::default()
        });
        let cat = synced(&btc_catalog(), &snap, "binance", "BTCUSDT", "");
        let dir = two_accounts_directory();
        for dir in [None, Some(&dir)] {
            let p = window_parts(&sources(&snap, dir, &cat, ("binance", None, "BTCUSDT")));
            let binance: Vec<_> = p
                .accounts
                .iter()
                .filter(|r| r.venue == "binance")
                .map(|r| (r.account, r.name))
                .collect();
            assert_eq!(
                binance,
                [(None, "main"), (Some("SUB"), "SUB")],
                "directory: {}",
                dir.is_some()
            );
        }
    }

    /// M-3(b): a picker row never reads PAPER for a mode nobody reported: a running block that
    /// publishes none, and a database tier word this build does not know, are both unknown.
    #[test]
    fn a_row_with_no_known_mode_reads_unknown() {
        let snap = snap_with(vec![block("binance", None, "BTCUSDT", None)]);
        let cat = synced(&btc_catalog(), &snap, "binance", "BTCUSDT", "");
        let p = window_parts(&sources(&snap, None, &cat, ("binance", None, "BTCUSDT")));
        assert_eq!(p.accounts[0].mode, AccountMode::Unknown);

        let dir = WireDirectory {
            venues: vec![venue("binance", None)],
            accounts: vec![
                account(1, "binance", None, "live"),
                account(2, "binance", Some("X"), "staging"),
            ],
        };
        let p = window_parts(&sources(&snap, Some(&dir), &cat, ("binance", None, "BTCUSDT")));
        let x = p.accounts.iter().find(|r| r.account == Some("X")).expect("the X row");
        assert_eq!((x.mode, x.why_not), (AccountMode::Unknown, Some(NOT_RUNNING_WHY)));
    }

    /// M-3(c, d): two unlabelled accounts of one venue are told apart by the broker's number (the
    /// one without a number stays "main"), the engine goes to the one its mode names, and the
    /// labelled rows keep the database's order after the default ones.
    #[test]
    fn two_unlabelled_accounts_of_one_venue_are_told_apart_and_labelled_rows_keep_their_order() {
        let snap = snap_with(vec![block("dukascopy", None, "EURUSD", Some(EngineMode::Demo))]);
        let cat = synced(&btc_catalog(), &snap, "dukascopy", "EURUSD", "");
        let mut demo = account(7, "dukascopy", None, "demo");
        demo.venue_account_id = Some("DEMO-111".into());
        let dir = WireDirectory {
            venues: vec![venue("dukascopy", Some("Dukascopy"))],
            accounts: vec![
                account(5, "dukascopy", Some("ZED"), "demo"),
                demo,
                account(6, "dukascopy", Some("ALPHA"), "demo"),
                account(8, "dukascopy", None, "live"),
            ],
        };
        let p = window_parts(&sources(&snap, Some(&dir), &cat, ("dukascopy", None, "EURUSD")));
        let rows: Vec<_> = p.accounts.iter().map(|r| (r.account, r.name, r.why_not)).collect();
        assert_eq!(
            rows,
            [
                (None, "DEMO-111", None),
                (None, "main", Some(NOT_RUNNING_WHY)),
                (Some("ZED"), "ZED", Some(NOT_RUNNING_WHY)),
                (Some("ALPHA"), "ALPHA", Some(NOT_RUNNING_WHY)),
            ]
        );
    }

    /// I18: a symbol the catalog does not list gets no lot size — none is invented — and the
    /// window says what is missing instead of a size refusal with a `(0)` in it.
    #[test]
    fn a_symbol_the_catalog_does_not_list_says_so_and_invents_no_lot() {
        let snap = snap_with(vec![block("binance", None, "ETHUSDT", Some(EngineMode::Live))]);
        let cat = synced(&btc_catalog(), &snap, "binance", "ETHUSDT", "");
        let p = window_parts(&sources(&snap, None, &cat, ("binance", None, "ETHUSDT")));
        assert_eq!((p.grid.lot, p.grid.min_qty), (0.0, 0.0));
        let (kind, text) = p.status.expect("the window says why it cannot size an order");
        assert_eq!(kind, StatusKind::Info);
        assert!(text.contains("the instrument catalog does not list it"), "{text}");
        assert!(!text.contains("(0)"), "{text}");

        let empty = synced(
            &Arc::new(Catalog::from_instruments(Vec::new())),
            &snap,
            "binance",
            "ETHUSDT",
            "",
        );
        let p = window_parts(&sources(&snap, None, &empty, ("binance", None, "ETHUSDT")));
        let (_, text) = p.status.expect("a reason");
        assert!(text.contains("has not loaded"), "{text}");
    }

    /// Minor 27: before the node's first snapshot the window says it is waiting.
    #[test]
    fn a_window_on_a_node_that_has_said_nothing_says_it_is_waiting() {
        let snap = CoreSnapshot::empty("", "");
        let cat = synced(&btc_catalog(), &snap, "binance", "BTCUSDT", "");
        let p = window_parts(&sources(&snap, None, &cat, ("binance", None, "BTCUSDT")));
        assert_eq!(p.status, Some((StatusKind::Info, Cow::Borrowed(WAITING_FOR_NODE))));
    }

    // ---- the status strip ------------------------------------------------------------------------

    /// Spec §3.8: the strip shows the window's last order and how it came out, read from the
    /// snapshot by its client order id. An order from before the window looked is not news, an
    /// order of another account never takes the strip, and a new instrument starts clean.
    #[test]
    fn the_status_strip_follows_the_newest_order_of_the_window_account() {
        let mut tv = tools::ToolView::default();
        let mut snap = two_accounts();
        snap.orders = vec![order("old", None, OrderStatus::Filled)];
        follow_status(&mut tv, &snap, "binance", None, "BTCUSDT");
        assert_eq!(tv.trade_status, None, "an order from before the window looked");

        snap.orders.push(order("new", None, OrderStatus::Accepted));
        follow_status(&mut tv, &snap, "binance", None, "BTCUSDT");
        assert_eq!(tv.trade_status, Some((StatusKind::Info, "Buy 1 limit · ACCEPTED".into())));

        snap.orders[1].status = OrderStatus::Filled;
        snap.orders.push(order("theirs", Some("SUB"), OrderStatus::Rejected));
        follow_status(&mut tv, &snap, "binance", None, "BTCUSDT");
        assert_eq!(
            tv.trade_status,
            Some((StatusKind::Ok, "Buy 1 limit · FILLED".into())),
            "the window's own order, not SUB's"
        );

        follow_status(&mut tv, &snap, "binance", Some("SUB"), "BTCUSDT");
        assert_eq!(tv.trade_status, None, "a new address starts clean");
    }

    /// A note the shell put in the strip stays until a NEW order on the address replaces it.
    #[test]
    fn a_note_stays_until_a_new_order_arrives() {
        let mut tv = tools::ToolView::default();
        let mut snap = two_accounts();
        follow_status(&mut tv, &snap, "binance", None, "BTCUSDT");
        tv.trade_status = Some((StatusKind::Error, "Not sent: the account does not trade".into()));
        follow_status(&mut tv, &snap, "binance", None, "BTCUSDT");
        assert_eq!(tv.trade_status.as_ref().map(|s| s.0), Some(StatusKind::Error));
        snap.orders.push(order("next", None, OrderStatus::Submitted));
        follow_status(&mut tv, &snap, "binance", None, "BTCUSDT");
        assert_eq!(tv.trade_status, Some((StatusKind::Info, "Buy 1 limit · SUBMITTED".into())));
    }

    /// Fix round 1, M-3(a): on a node that names no order's account (Ruling R9) the strip follows
    /// nothing: it cannot tell the window's orders from another account's.
    #[test]
    fn the_status_strip_follows_nothing_on_a_node_that_does_not_attribute_orders() {
        let mut tv = tools::ToolView::default();
        let mut snap = snap_with(vec![
            block("binance", None, "", None),
            block("binance", Some("SUB"), "", None),
        ]);
        follow_status(&mut tv, &snap, "binance", None, "BTCUSDT");
        snap.orders.push(order("whose", None, OrderStatus::Accepted));
        follow_status(&mut tv, &snap, "binance", None, "BTCUSDT");
        assert_eq!(tv.trade_status, None);
        assert_eq!(tv.trade_status_coid, None);
    }

    /// The status strip follows an order exactly when the dispatcher's `owns_order` gives it to the
    /// window — a DONE order included, which the markers' `window_orders` leaves out — checked
    /// against an EXPLICIT table. (This was a parity test against the glue's own copy of the rule;
    /// the copy is deleted and the strip calls `owns_order` itself, so a parity test would compare
    /// the rule with itself.) Each fixture order is shown to the window ALONE, so every clause
    /// that excludes it — another account's order, another venue's, another symbol's, an account
    /// the server does not run, a venue whose blocks attribute nothing — is visible in the strip.
    /// Every wrong answer is collected, so one run names every clause a change broke.
    #[test]
    fn the_strip_follows_the_dispatchers_attribution_live_or_done() {
        let elsewhere = |mut o: OrderView, venue: &str, symbol: &str| -> OrderView {
            o.venue = venue.into();
            o.symbol = symbol.into();
            o
        };
        let fixtures = || {
            vec![
                order("d", None, OrderStatus::Accepted),
                order("s", Some("SUB"), OrderStatus::Accepted),
                order("x", Some("OTHER"), OrderStatus::Accepted),
                elsewhere(order("v", None, OrderStatus::Accepted), "bybit", "BTCUSDT"),
                elsewhere(order("y", None, OrderStatus::Accepted), "binance", "ETHUSDT"),
                order("f", None, OrderStatus::Filled),
            ]
        };
        let node = |blocks: Vec<VenueBlock>| {
            let mut s = snap_with(blocks);
            s.portfolio.venues.push(block("bybit", None, "BTCUSDT", Some(EngineMode::Paper)));
            s
        };
        let modern = node(vec![
            block("binance", None, "BTCUSDT", Some(EngineMode::Live)),
            block("binance", Some("SUB"), "BTCUSDT", Some(EngineMode::Demo)),
        ]);
        let one_labelled = node(vec![block("binance", Some("SUB"), "", None)]);
        let unattributed =
            node(vec![block("binance", None, "", None), block("binance", Some("SUB"), "", None)]);
        let cases: [(&str, &CoreSnapshot, Option<&str>, &[&str]); 9] = [
            ("modern", &modern, None, &["d", "f"]),
            ("modern", &modern, Some("SUB"), &["s"]),
            ("modern", &modern, Some("OTHER"), &[]),
            ("one labelled", &one_labelled, None, &[]),
            ("one labelled", &one_labelled, Some("SUB"), &["d", "s", "x", "f"]),
            ("one labelled", &one_labelled, Some("OTHER"), &[]),
            ("R9", &unattributed, None, &[]),
            ("R9", &unattributed, Some("SUB"), &[]),
            ("R9", &unattributed, Some("OTHER"), &[]),
        ];

        let mut wrong = Vec::new();
        for (name, base, account, want) in cases {
            for o in fixtures() {
                let mut tv = tools::ToolView::default();
                // the window looks at its address first, so what follows is an order it saw appear
                follow_status(&mut tv, base, "binance", account, "BTCUSDT");
                let mut snap = base.clone();
                snap.orders = vec![o.clone()];
                follow_status(&mut tv, &snap, "binance", account, "BTCUSDT");
                let follows = tv.trade_status_coid.as_deref() == Some(o.client_order_id.as_str());
                if follows != want.contains(&o.client_order_id.as_str()) {
                    wrong.push(format!(
                        "{name} {account:?} {} follows={follows}",
                        o.client_order_id
                    ));
                }
            }
        }
        assert!(wrong.is_empty(), "the strip follows the wrong orders: {wrong:?}");
    }

    /// M-1 of the final review (slice B): a backend SWITCH forgets what the strip was following. On
    /// the new node's first snapshot a window at the same address starts clean: it shows neither
    /// the old backend's order line nor the new node's newest pre-existing order as its own.
    ///
    /// ⚠ Between the two the shell publishes the EMPTY snapshot a switch leaves
    /// (`CoreSnapshot::empty("observing", "")`, `backend_conn`'s `switch_backend`), and the window
    /// draws it, every frame, until the new node speaks (the FW2 review). A look taken there saw
    /// no order, so the new node's newest pre-existing order then read as one that APPEARED, and
    /// the strip followed it as if this window had placed it. A snapshot that has said nothing is
    /// no look: it takes no baseline. The same frames run at startup, for a restored window.
    #[test]
    fn a_backend_switch_starts_the_strip_clean_on_the_new_node() {
        let id = egui::Id::new("trade");
        let mut tv = tools::ToolView::default();
        let mut old = two_accounts();
        follow_status(&mut tv, &old, "binance", None, "BTCUSDT");
        old.orders.push(order("mine", None, OrderStatus::Accepted));
        follow_status(&mut tv, &old, "binance", None, "BTCUSDT");
        assert!(tv.trade_status.is_some(), "the strip follows the window's order on the old node");

        let mut views = std::collections::HashMap::from([(id, tv)]);
        crate::ui::tool_views::on_backend_switch(&mut views, &mut None);
        let mut tv = views.remove(&id).expect("the window");
        let silent = CoreSnapshot::empty("observing", "");
        follow_status(&mut tv, &silent, "binance", None, "BTCUSDT");
        follow_status(&mut tv, &silent, "binance", None, "BTCUSDT");
        let mut new = two_accounts();
        new.orders = vec![order("theirs", None, OrderStatus::Accepted)];
        follow_status(&mut tv, &new, "binance", None, "BTCUSDT");
        assert_eq!(
            tv.trade_status, None,
            "nothing from the old node, nothing that predates the look"
        );
        assert_eq!(tv.trade_status_coid, None);
    }

    /// The bar's price and the ladder's outlined row are the book's MID on the desktop (final
    /// review A, Minor 17, and the FW2 review): a snapshot reaches the desktop through
    /// `observe_bridge::wire_to_core`, `WireSnapshot` carries no marks, and the bridge leaves
    /// `marks` empty, so the window has no last trade price to read. Built here through the real
    /// bridge, not a snapshot with marks planted in it; the bar's hover says "Mid price".
    #[test]
    fn the_window_price_is_the_books_mid_on_a_snapshot_from_the_node() {
        use crate::backend::observe_bridge::{
            observe_bridge_tests::full_wire_snapshot, wire_to_core,
        };
        let mut book = L2Book::new(0.1);
        book.apply_snapshot(
            1,
            &[vike_model::BookLevel::new(99.9, 1.0)],
            &[vike_model::BookLevel::new(100.1, 1.0)],
        );
        let snap = wire_to_core(&full_wire_snapshot());
        assert!(snap.marks.is_empty(), "CONTROL: no mark crosses the wire");
        let cat = synced(&btc_catalog(), &snap, "binance", "BTCUSDT", "");
        let last = |book: Option<&L2Book>| {
            let src =
                WindowSources { book, ..sources(&snap, None, &cat, ("binance", None, "BTCUSDT")) };
            window_parts(&src).last
        };
        let mid = last(Some(&book)).expect("the book's mid");
        assert!((mid - 100.0).abs() < 1e-9, "{mid}");
        assert_eq!(last(None), None, "no book: a dash");
    }

    /// M-10 of the final review (slice B): a block whose route key IS an account label, but whose
    /// label disagrees with it (dropped on the wire, or naming another account), offers no row: the
    /// row's account is a ROUTING answer and the two cannot both be it. The `routed.text() != label`
    /// arm of `account_rows`, which `a_block_with_a_malformed_label_offers_no_row` (a route key that
    /// names no label) never reaches.
    #[test]
    fn a_block_whose_label_disagrees_with_its_route_key_offers_no_row() {
        let mut snap = two_accounts();
        for (route_key, label) in [("binance#TEAM", None), ("binance#DESK", Some("OTHER"))] {
            snap.portfolio.venues.push(VenueBlock {
                venue: "binance".into(),
                account: label.map(|l| AccountLabel::parse(l).expect("a test label")),
                route_key: route_key.into(),
                symbol: "BTCUSDT".into(),
                mode: Some(EngineMode::Live),
                ..Default::default()
            });
        }
        let cat = synced(&btc_catalog(), &snap, "binance", "BTCUSDT", "");
        let dir = two_accounts_directory();
        for dir in [None, Some(&dir)] {
            let p = window_parts(&sources(&snap, dir, &cat, ("binance", None, "BTCUSDT")));
            let binance: Vec<_> = p
                .accounts
                .iter()
                .filter(|r| r.venue == "binance")
                .map(|r| (r.account, r.name))
                .collect();
            assert_eq!(
                binance,
                [(None, "main"), (Some("SUB"), "SUB")],
                "directory: {}",
                dir.is_some()
            );
        }
    }

    /// `vike_panels::trade::layout::chrome` is THIS window's: a Trade window is sized by it
    /// (`layout::window_size`, FW5 B1), and a chrome counted short opens a window whose form is
    /// cut again. Drawn as the desktop draws a Trade window, through the real `show_window` and
    /// `tool_title_bar`: the title bar, the body margin's frame, then the body, which
    /// `crates/vike-desktop/src/main.rs`'s `tool_content` opens with the density's gap (modelled
    /// here: that crate is not built by this gate). At every density the rect the body is handed
    /// is the window's size less that chrome.
    #[test]
    fn the_trade_windows_body_is_its_size_less_the_chrome_its_layout_assumes() {
        use crate::ui::workspace::{
            TOOL_BODY_MARGIN, WinKind, WinState, show_window, tool_title_bar,
        };
        use vike_ui_theme::appearance::{Appearance, install};
        use vike_ui_theme::metrics::Density;
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1600.0, 1000.0));
        for density in Density::ALL {
            let ctx = egui::Context::default();
            install(&ctx, &Appearance { density, ..Appearance::default() });
            let size = egui::vec2(600.0, 700.0);
            let rect = egui::Rect::from_min_size(egui::pos2(40.0, 30.0), size);
            let mut w = WinState::tool("trade-1", WinKind::Trade, rect);
            let mut body = egui::Rect::NOTHING;
            for _ in 0..3 {
                ctx.begin_pass(egui::RawInput { screen_rect: Some(screen), ..Default::default() });
                show_window(&ctx, &mut w, screen, |ui, bounds| {
                    let _ = tool_title_bar(ui, WinKind::Trade, false);
                    egui::Frame::new().inner_margin(TOOL_BODY_MARGIN).show(ui, |ui| {
                        bounds.show(ui, |ui| {
                            ui.add_space(density.metrics().gap);
                            body = ui.available_rect_before_wrap();
                        });
                    });
                    egui::Vec2::ZERO
                });
                ctx.end_pass().drop_without_applying_deltas();
            }
            let want = size - vike_panels::trade::layout::chrome(&density.metrics());
            assert!(
                (body.size() - want).length() < 0.01,
                "{density:?}: a {size:?} window hands its body {:?}; the layout counts {want:?}",
                body.size()
            );
        }
    }
}
