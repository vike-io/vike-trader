//! One frame of a Trade window: the status strip's follow, and the widget's inputs (I14).

use std::borrow::Cow;

use super::accounts::{NotRunningRow, account_rows, status_of, venue_label};
use super::catalog::{TradeCatalog, no_lot_why};
use super::reasons::{
    ACCOUNT_LIST_UNAVAILABLE, ControlLink, REDUCING_WHY, TPSL_LANE_WHY, TPSL_SYMBOL_WHY,
    UNADDRESSABLE_WHY, not_in_catalog_why, one_symbol_why, perp_lane_why,
};
use super::refusal;
use crate::orders::dom_math::{
    depth_absence, depth_absence_headline, depth_absence_next_step, signed_position_size,
};
use crate::orders::order_dispatch::{
    ORDERS_UNATTRIBUTED_WHY, TradeAddress, bracket_lane_holds_stop, bracket_on_mounted_symbol,
    orders_name_their_account, owns_order, perp_lane_account, tradable, window_orders,
    wire_account,
};
use crate::orders::symbol_follow;
use crate::tools;
use vike_catalog::kind_of;
use vike_exec::{CoreSnapshot, OrderView};
use vike_marketdata::L2Book;
use vike_model::{AccountTier, TradingState};
use vike_panels::trade::ticket::TPSL_ACCOUNT_WHY;
use vike_panels::trade::{
    AccountRow, BookAbsence, Grid, LadderOrder, PickRow, Position, StatusKind, StatusLine,
    Tradable, TradeInputs, Unconnected,
};
use vike_tradehub_client::wire::{KEY_CHANGED_WARNING, WireDirectory};
use vike_venues::VenueCaps;

/// Why a window takes no order, where the glue can STATE the cause (spec §4.3), and how the status
/// strip reads it. Its `text` is both the strip's line and the widget's `Tradable::No::why`: ONE
/// text, computed once ([`refusal`]), so the ticket's line, the ladder's hint and every disabled
/// button say what the strip says — and never have to guess the cause from whatever the strip
/// shows (the F wave's reason slot).
pub(super) struct Refusal {
    pub(super) kind: StatusKind,
    pub(super) text: Cow<'static, str>,
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

/// The status strip's line for an order: `Buy 0.01 limit · ACCEPTED`, plus `· DELAYED` / `· FROZEN`
/// when the node recorded it was priced off data that was not real-time (decision 0140; the
/// Account window's badge words).
pub(super) fn order_words(o: &OrderView) -> String {
    let side = if o.side > 0 { "Buy" } else { "Sell" };
    let words = format!("{side} {} {} · {}", o.qty, o.order_type, o.status.as_str());
    match super::super::account::delay_badge(o.data_delay) {
        Some(badge) => format!("{words} · {badge}"),
        None => words,
    }
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
pub(super) fn follow_status(
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

/// How many of a busy symbol's newest prints the tick chart is handed: it draws bubbles, and a few
/// hundred are all a pane that size can show.
pub(super) const TAPE_SHOWN: usize = 1_500;

/// Everything one frame of a Trade window is built from, borrowed (I14).
pub(super) struct WindowSources<'a> {
    pub(super) snap: &'a CoreSnapshot,
    /// The node's `Directory` reply; `None` before it and against a node that predates the verb.
    pub(super) directory: Option<&'a WireDirectory>,
    /// The window's catalog answers, already synced to this frame's instrument.
    pub(super) catalog: &'a TradeCatalog,
    pub(super) venue: &'a str,
    /// The window's account TEXT as it is ADDRESSED (`ToolView::trade_account_in_force`: a lane twin's for
    /// a perpetual while the twin is up), `None` for the default account.
    pub(super) account: Option<&'a str>,
    pub(super) symbol: &'a str,
    /// The window's own L2 book, `None` until the first snapshot of it lands.
    pub(super) book: Option<&'a L2Book>,
    /// The prints of the last two minutes on the window's instrument, for the tick chart.
    pub(super) tape: &'a [vike_panels::trade::chart::Print],
    pub(super) stale: bool,
    /// The window venue's market-data status line, verbatim; `None` when nothing produces one.
    pub(super) source: Option<&'a str>,
    pub(super) recent: &'a [(String, String)],
    /// What the strip says about the window's last order, note or refusal (`ToolView::trade_status`).
    pub(super) status: Option<&'a (StatusKind, String)>,
    /// Whether the backend can carry a TP/SL bracket at all (Ruling R8).
    pub(super) bracket_wire: bool,
    /// Whether this desktop can send an order at all (the owner's item 11).
    pub(super) control_link: ControlLink,
    /// Whether the node's `Directory` fetch ended with no list (the owner's item 5).
    pub(super) directory_unavailable: bool,
    /// The accounts the node says it does not run, each with the sentence the window says for it
    /// (`accounts::not_running_rows` of this frame's snapshot; decision 0156).
    pub(super) not_running: &'a [NotRunningRow],
    /// Route keys of the RUNNING accounts whose key changed in the database (`ToolCtx::key_changed`).
    pub(super) key_changed: &'a [String],
}

impl<'a> WindowSources<'a> {
    /// The symbol a pick on `venue` opens: the window's own on its own venue, else the catalog's
    /// spelling of the instrument there (C3); `None` when the catalog does not list it there.
    pub(super) fn opens(&self, venue: &str) -> Option<&'a str> {
        if venue == self.venue { Some(self.symbol) } else { self.catalog.spelling_on(venue) }
    }

    /// The product the instrument is on `venue`: the window's own listing's on its own venue, else
    /// the catalog's spelling's there; empty where the catalog does not say.
    pub(super) fn product(&self, venue: &str) -> &'static str {
        if venue == self.venue {
            self.catalog
                .listing
                .as_ref()
                .and_then(|l| kind_of(l.class))
                .map_or("", vike_catalog::Kind::product)
        } else {
            self.catalog.product_on(venue)
        }
    }
}

/// One frame of a Trade window, owned where the widget borrows. [`WindowParts::inputs`] lends it
/// out as the widget's [`TradeInputs`].
pub(super) struct WindowParts<'a> {
    pub(super) venue: &'a str,
    pub(super) venue_label: &'a str,
    /// The product the window's instrument is (`Perp`): the widget's `product`.
    pub(super) product: &'static str,
    /// The account label, `None` for the default account (the raw text when it is not a label).
    pub(super) account: Option<&'a str>,
    pub(super) symbol: &'a str,
    pub(super) base: &'a str,
    pub(super) quote: &'a str,
    pub(super) mode: Option<AccountTier>,
    pub(super) tradable: bool,
    /// What the account's engine trades, for the untradable line.
    pub(super) trades: Vec<String>,
    /// Why the window takes no order, where the glue can state it ([`Refusal`]): the widget's
    /// `Tradable::No::why`, in the words the strip uses.
    pub(super) why: Option<Cow<'static, str>>,
    pub(super) grid: Grid,
    /// The book, when it has a level on either side.
    pub(super) book: Option<&'a L2Book>,
    /// An empty book for the widget to draw the absence from.
    pub(super) no_book: L2Book,
    pub(super) last: Option<f64>,
    pub(super) stale: bool,
    pub(super) source: &'a str,
    pub(super) absence: Option<BookAbsence<'a>>,
    pub(super) orders: Vec<LadderOrder>,
    pub(super) orders_why: Option<&'static str>,
    pub(super) position: Option<Position>,
    pub(super) buying_power: Option<f64>,
    pub(super) caps: VenueCaps,
    /// Why TP/SL cannot go to this account, stated once: the widget's `bracket_why`.
    pub(super) bracket_why: Option<&'static str>,
    pub(super) bracket_wire: bool,
    pub(super) matches: Vec<PickRow<'a>>,
    pub(super) recent: Vec<(&'a str, &'a str)>,
    pub(super) accounts: Vec<AccountRow<'a>>,
    /// Why `accounts` is not the database's list: the widget's `accounts_why`.
    pub(super) accounts_why: Option<&'static str>,
    pub(super) unconnected: Vec<Unconnected<'a>>,
    /// The tick chart's prints.
    pub(super) tape: &'a [vike_panels::trade::chart::Print],
    pub(super) status: Option<(StatusKind, Cow<'a, str>)>,
}

impl WindowParts<'_> {
    /// The widget's inputs for this frame.
    pub(super) fn inputs(&self) -> TradeInputs<'_> {
        TradeInputs {
            venue: self.venue,
            venue_label: self.venue_label,
            product: self.product,
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
            tape: self.tape,
            status: self.status.as_ref().map(|(kind, text)| StatusLine { kind: *kind, text }),
        }
    }
}

/// **The window's inputs, built by one pure function (I14).** No egui, no I/O: the snapshot, the
/// directory, the catalog answers and the window's own state in, the widget's inputs out.
pub(super) fn window_parts<'a>(src: &WindowSources<'a>) -> WindowParts<'a> {
    let snap = src.snap;
    let address = TradeAddress::parse(src.venue, src.account, src.symbol);
    let block = address.as_ref().and_then(|a| {
        let key = a.route_key();
        snap.portfolio.venues.iter().find(|b| b.route_key == key)
    });
    // An account the node names as NOT RUNNING, with its reason and its row's tier (decision 0156).
    let said = match (&address, block) {
        (Some(a), None) => {
            let key = a.route_key();
            src.not_running.iter().find(|n| n.route_key == key)
        }
        _ => None,
    };
    // A core that has stopped and an account whose engine is HALTED take nothing from the window
    // (`order_dispatch::tradable`, the dispatcher's own rule since M-6); a REDUCING one still
    // trades and the strip says what it takes. Nor does a desktop that cannot send (item 11).
    let state = block.map(|b| b.trading_state);
    let sends = src.control_link == ControlLink::Open;
    let trades: Vec<String> = block
        .map(|b| {
            std::iter::once(&b.symbol)
                .chain(&b.extra_symbols)
                .filter(|s| !s.is_empty())
                .cloned()
                .collect()
        })
        .unwrap_or_default();
    // The PICKED symbol, when it is not the one the engine was started on: an account that takes any
    // symbol of its venue trades it only if the venue's catalog lists it (the instrument to resolve,
    // its lot and its grid); one that keeps to a symbol (`symbol_follow`) says so. A block that names
    // no symbol (an older node) is not judged here: `tradable` holds it to the primary market.
    // …and a perpetual at the DEFAULT account of a venue whose perpetual lane is another account's: that
    // account's address is where this window can see the position and the orders (`perp_lane_account`).
    // A lane TWIN's account never gets here as the default: `lane_twin_account` addressed the window to
    // it already (`ToolView::trade_account_in_force`), so this fires for the lane engines it is not.
    let lane_why = address
        .as_ref()
        .and_then(|a| perp_lane_account(snap, a))
        .map(|account| perp_lane_why(src.symbol, &account));
    let symbol_why = lane_why.or_else(|| {
        block.filter(|b| !b.symbol.is_empty() && !b.trades(src.symbol)).and_then(|b| {
            match symbol_follow::one_symbol(b) {
                Some(why) => Some(one_symbol_why(src.symbol, &trades, why)),
                None if src.catalog.listing.is_none() => Some(not_in_catalog_why(
                    src.symbol,
                    venue_label(src.directory, src.venue),
                    src.catalog.loaded,
                )),
                None => None,
            }
        })
    });
    let can_trade =
        sends && symbol_why.is_none() && address.as_ref().is_some_and(|a| tradable(snap, a));
    // Why it takes none, where that can be stated: the strip's line AND the widget's cause.
    let stated = refusal(snap, address.as_ref(), block, said, src.control_link, symbol_why);

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
    let book_tick = real.map_or(0.0, |b| b.tick_size).max(0.0);
    let mut grid =
        listing.map_or_else(|| Grid { tick: book_tick, lot: 0.0, min_qty: 0.0 }, |l| l.grid);
    // ⚠ A row from the datahub's venue list carries NO grid: every tick and lot of a server-fetched
    // catalog is ZERO (measured 2026-10-04: all 1,943 Binance rows). Read as the window's grid that
    // prints every price of a BTC-quoted pair as `0` and lays the ladder on a zero tick, so the BOOK's
    // own tick — which the depth stream does carry — answers when the catalog's is not a positive
    // number. The lot stays unknown, honestly: an order's size is the next step's to solve.
    if !(grid.tick > 0.0 && grid.tick.is_finite()) {
        grid.tick = book_tick;
    }
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
                is_stop: o.order_type == vike_model::OrderType::Stop,
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
            size: signed_position_size(p.size, p.position_side),
            avg_px: p.avg_px,
            upnl: p.unrealized,
        });

    // The symbol picker: the cached rows (every venue the catalog lists, one per instrument), each
    // with its venue's own spelling and whether it is this window's own instrument. No account mark
    // and no price: the v3 design draws neither in the menu, and the menu fetches nothing.
    let matches: Vec<PickRow<'a>> = src
        .catalog
        .rows
        .iter()
        .map(|r| PickRow {
            venue: &r.venue,
            venue_label: venue_label(src.directory, &r.venue),
            symbol: &r.symbol,
            pair: r.pair.clone(),
            current: r.venue == src.venue && r.symbol == src.symbol,
        })
        .collect();

    let unconnected: Vec<Unconnected<'a>> = match src.directory {
        Some(dir) => dir
            .venues
            .iter()
            .filter(|v| !dir.accounts.iter().any(|a| a.venue == v.name))
            .filter(|v| src.opens(&v.name).is_some())
            .map(|v| Unconnected {
                venue_label: v.title.as_deref().unwrap_or(&v.name),
                product: src.product(&v.name),
                symbol: src.opens(&v.name).unwrap_or(""),
            })
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
        // A running account whose key changed in the database keeps trading on the old key until the
        // next restart: a calm standing note, below anything the window refuses or limits itself to.
        (None, None)
            if address.as_ref().is_some_and(|a| src.key_changed.contains(&a.route_key())) =>
        {
            Some((StatusKind::Info, Cow::Borrowed(KEY_CHANGED_WARNING)))
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
        Some(a) if !bracket_on_mounted_symbol(snap, a) => Some(TPSL_SYMBOL_WHY),
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
        product: src.product(src.venue),
        symbol: src.symbol,
        base,
        quote,
        mode: block.map_or_else(|| said.and_then(|n| n.tier), |b| b.tier),
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
        caps: vike_venues::caps_for(src.venue),
        bracket_wire: src.bracket_wire,
        matches,
        recent: src.recent.iter().map(|(v, s)| (v.as_str(), s.as_str())).collect(),
        accounts: account_rows(src),
        unconnected,
        tape: src.tape,
        status,
    }
}
