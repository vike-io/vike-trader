//! One frame of a Trade window: the status strip's follow, and the widget's inputs (I14).

use std::borrow::Cow;

use super::accounts::{TradeWindowNotRunningRow, account_rows, status_of, venue_label};
use super::catalog::{TradeWindowCatalog, no_lot_why};
use super::reasons::{
    ACCOUNT_LIST_UNAVAILABLE, QUOTE_ONLY_HEADLINE, REDUCING_WHY, TAKE_PROFIT_STOP_LOSS_LANE_WHY,
    TAKE_PROFIT_STOP_LOSS_SYMBOL_WHY, UNADDRESSABLE_WHY, delayed_quote_why, not_in_catalog_why,
    one_symbol_why, perp_lane_why,
};
use super::refusal;
use crate::backend::tradehub_control::BackendControlLink;
use crate::data::data_sink::TickQuoteView;
use crate::ui::windows::WindowView;
use vike_catalog::kind_of;
use vike_execution::{CoreSnapshot, OrderView};
use vike_marketdata::L2Book;
use vike_model::feed_status::DataClientStatus;
use vike_model::{AccountTier, Grid, TradingState};
use vike_order_entry::dispatch::{
    ORDERS_UNATTRIBUTED_WHY, bracket_lane_holds_stop, bracket_on_mounted_symbol, contract_value_at,
    orders_name_their_account, owns_order, perp_lane_account, tradable, window_orders,
    wire_account,
};
use vike_order_entry::trade_window::address::TradeWindowAddress;
use vike_order_entry::trade_window::ladder::absence::{
    TradeWindowLadderAbsence, depth_absence, depth_absence_headline, depth_absence_next_step,
};
use vike_order_entry::trade_window::symbol_follow;
use vike_order_entry::trade_window::ticket::TAKE_PROFIT_STOP_LOSS_ACCOUNT_WHY;
use vike_order_entry::trade_window::{
    SymbolPickerRow, TradeWindowAccountPickerConnectRow, TradeWindowAccountPickerRow,
    TradeWindowInputs, TradeWindowLadderOrder, TradeWindowPosition, TradeWindowQuoteFace,
    TradeWindowStatusLine, TradeWindowStatusLineKind, TradeWindowTradable, grid_value,
};
use vike_tradehub_client::node_words::{BackendNodeWords, BackendNodeWordsAccountLink};
use vike_tradehub_client::wire::{AccountDirectoryWire, KEY_CHANGED_WARNING};
use vike_venues::VenueCapabilities;

/// Why a window takes no order, where the glue can STATE the cause (spec §4.3), and how the status
/// strip reads it. Its `text` is both the strip's line and the widget's `TradeWindowTradable::No::why`: ONE
/// text, computed once ([`refusal`]), so the ticket's line, the ladder's hint and every disabled
/// button say what the strip says — and never have to guess the cause from whatever the strip
/// shows (the F wave's reason slot).
pub(super) struct TradeWindowRefusal {
    pub(super) kind: TradeWindowStatusLineKind,
    pub(super) text: Cow<'static, str>,
}

/// The newest order on a window's address when the window last looked (spec §3.8): what the status
/// strip follows from.
#[derive(Debug, Clone, PartialEq)]
pub struct TradeWindowSeenOrder {
    venue: String,
    account: Option<String>,
    symbol: String,
    order_client_id: Option<String>,
}

/// The status strip's line for an order: `Buy 0.01 limit · ACCEPTED`, plus `· DELAYED` / `· FROZEN`
/// when the node recorded it was priced off data that was not real-time (decision 0140; the
/// Account window's badge words), and, for an order the venue or the node REFUSED, why, in the
/// words the node recorded (`OrderView::reason`): `Buy 0.01 limit · REJECTED · post-only buy at 100
/// would take liquidity (the ask is 99.9): refused, nothing rests`. That is how a post-only order
/// that would cross shows on the ticket, where the status alone would say only REJECTED. A refusal whose event carried
/// its catalog id (`OrderView::refusal_id`) reads `error [<id>]: <text>` there.
pub(super) fn order_words(o: &OrderView) -> String {
    let side = if o.side > 0 { "Buy" } else { "Sell" };
    let words = format!("{side} {} {} · {}", o.qty, o.order_type, o.status.as_str());
    let words = match vike_account_window::orders::delay_badge(o.data_delay) {
        Some(badge) => format!("{words} · {badge}"),
        None => words,
    };
    // Only a refusal's reason: a live order also carries the reason its last MODIFY was refused, which
    // is not what this order's line is about.
    let refused =
        matches!(o.status, vike_model::OrderStatus::Rejected | vike_model::OrderStatus::Denied);
    match o.reason.as_deref().map(str::trim).filter(|r| refused && !r.is_empty()) {
        Some(reason) => format!(
            "{words} · {}",
            vike_utility::errors::render_refusal_or_text(o.refusal_id.as_deref(), reason)
        ),
        None => words,
    }
}

/// Bring the status strip up to date for the window at `(venue, account, symbol)` (spec §3.8). An
/// order that appeared on the address since the window last looked is what the strip follows from
/// now on, by its order client id, until a newer one appears; a note or a refusal the shell put in
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
    tv: &mut WindowView,
    snapshot: &CoreSnapshot,
    venue: &str,
    account: Option<&str>,
    symbol: &str,
) {
    if snapshot.portfolio.venues.is_empty() {
        return;
    }
    // The dispatcher's attribution, live or done (the strip must also see an order that filled
    // before a snapshot showed it working): nothing on a node that attributes no order (Ruling R9).
    let newest = TradeWindowAddress::parse(venue, account, symbol).and_then(|address| {
        let owns = owns_order(snapshot, &address);
        snapshot.orders.iter().rev().find(|&o| owns(o)).map(|o| o.order_client_id.clone())
    });
    let same_address = tv
        .trade_seen
        .as_ref()
        .is_some_and(|s| s.venue == venue && s.account.as_deref() == account && s.symbol == symbol);
    if !same_address {
        tv.trade_status = None;
        tv.trade_status_order_client_id = None;
    } else if newest.is_some()
        && tv.trade_seen.as_ref().is_some_and(|s| s.order_client_id != newest)
    {
        tv.trade_status_order_client_id = newest.clone();
    }
    tv.trade_seen = Some(TradeWindowSeenOrder {
        venue: venue.to_string(),
        account: account.map(str::to_string),
        symbol: symbol.to_string(),
        order_client_id: newest,
    });
    let followed = tv
        .trade_status_order_client_id
        .as_deref()
        .and_then(|c| snapshot.orders.iter().find(|o| o.order_client_id == c));
    if let Some(o) = followed {
        let mut words = order_words(o);
        // The node's note on THIS order rides on its words (a bracket's note names no id: it binds to the
        // first order the strip follows), so the order's progress and the note are one line.
        if let Some((order_client_id, note)) = tv.trade_note.as_mut() {
            if order_client_id.is_empty() {
                order_client_id.clone_from(&o.order_client_id);
            }
            if *order_client_id == o.order_client_id {
                words = format!("{words} — {note}");
            }
        }
        tv.trade_status = Some((status_of(o.status), words));
    }
}

/// How many of a busy symbol's newest prints the tick chart is handed: it draws bubbles, and a few
/// hundred are all a pane that size can show.
pub(super) const TAPE_SHOWN: usize = 1_500;

/// Everything one frame of a Trade window is built from, borrowed (I14).
pub(super) struct TradeWindowSources<'a> {
    pub(super) snapshot: &'a CoreSnapshot,
    /// The node's `Directory` reply; `None` before it and against a node that predates the verb.
    pub(super) directory: Option<&'a AccountDirectoryWire>,
    /// The window's catalog answers, already synced to this frame's instrument.
    pub(super) catalog: &'a TradeWindowCatalog,
    pub(super) venue: &'a str,
    /// The window's account TEXT as it is ADDRESSED (`WindowView::trade_account_in_force`: a lane twin's for
    /// a perpetual while the twin is up), `None` for the default account.
    pub(super) account: Option<&'a str>,
    pub(super) symbol: &'a str,
    /// The window's own L2 book, `None` until the first snapshot of it lands.
    pub(super) book: Option<&'a L2Book>,
    /// The instrument's latest quote and how old it is in milliseconds, on a venue whose market data
    /// is a top of book.
    pub(super) top: Option<(TickQuoteView, i64)>,
    /// The prints of the last two minutes on the window's instrument, for the tick chart.
    pub(super) tape: &'a [vike_order_entry::trade_window::tick_chart::TradeWindowTickChartPrint],
    pub(super) stale: bool,
    /// The window venue's market-data status line, its state and its words; `None` when nothing
    /// produces one.
    pub(super) source: Option<&'a DataClientStatus>,
    /// Why the datahub cannot be reached (`WindowToolContext::datahub_unreachable`): the cause of the empty
    /// ladder when `source` is `None` (a venue with no line, as a datahub that never answered leaves).
    pub(super) unreachable: Option<&'a str>,
    pub(super) recent: &'a [(String, String)],
    /// What the strip says about the window's last order, note or refusal (`WindowView::trade_status`).
    pub(super) status: Option<&'a (TradeWindowStatusLineKind, String)>,
    /// Whether the backend can carry a TP/SL bracket at all (Ruling R8).
    pub(super) bracket_wire: bool,
    /// Whether the connected node carries a post-only order (`WindowToolContext::node_takes_post_only`).
    pub(super) post_only_wire: bool,
    /// Whether this desktop can send an order at all (the owner's item 11).
    pub(super) control_link: BackendControlLink,
    /// Whether the node's `Directory` fetch ended with no list (the owner's item 5).
    pub(super) directory_unavailable: bool,
    /// The accounts the node says it does not run, each with the sentence the window says for it
    /// (`accounts::not_running_rows` of this frame's snapshot; decision 0156).
    pub(super) not_running: &'a [TradeWindowNotRunningRow],
    /// What the observe frame says beyond the snapshot: the running accounts whose key changed in the
    /// database, and those whose way to the venue is lost (`WindowToolContext::node_words`).
    pub(super) node_words: &'a BackendNodeWords,
    /// What the node says of the order held for a confirm (`TradeWindowHeldPreview::says`).
    pub(super) held_says: Option<&'a str>,
}

impl<'a> TradeWindowSources<'a> {
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
                .map_or("", vike_catalog::SymbolPickerKind::product)
        } else {
            self.catalog.product_on(venue)
        }
    }
}

/// One frame of a Trade window, owned where the widget borrows. [`TradeWindowParts::inputs`] lends it
/// out as the widget's [`TradeWindowInputs`].
pub(super) struct TradeWindowParts<'a> {
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
    /// Why the window takes no order, where the glue can state it ([`TradeWindowRefusal`]): the widget's
    /// `TradeWindowTradable::No::why`, in the words the strip uses.
    pub(super) why: Option<Cow<'static, str>>,
    pub(super) grid: Grid,
    /// The book, when it has a level on either side.
    pub(super) book: Option<&'a L2Book>,
    /// An empty book for the widget to draw the absence from.
    pub(super) no_book: L2Book,
    pub(super) top: Option<TradeWindowQuoteFace<'static>>,
    pub(super) last: Option<f64>,
    pub(super) stale: bool,
    pub(super) source: Option<&'a DataClientStatus>,
    pub(super) absence: Option<TradeWindowLadderAbsence<'a>>,
    pub(super) orders: Vec<TradeWindowLadderOrder>,
    pub(super) orders_why: Option<&'static str>,
    pub(super) position: Option<TradeWindowPosition>,
    pub(super) buying_power: Option<f64>,
    /// What one unit of the symbol is worth in the account's book: the widget's `value`.
    pub(super) value: vike_model::ContractValue,
    pub(super) capabilities: VenueCapabilities,
    /// Why TP/SL cannot go to this account, stated once: the widget's `bracket_why`.
    pub(super) bracket_why: Option<&'static str>,
    pub(super) bracket_wire: bool,
    /// Whether the node carries a post-only order: the widget's `post_only_wire`. The VENUE's half of
    /// Post only is `capabilities`.
    pub(super) post_only_wire: bool,
    pub(super) matches: Vec<SymbolPickerRow<'a>>,
    pub(super) recent: Vec<(&'a str, &'a str)>,
    pub(super) accounts: Vec<TradeWindowAccountPickerRow<'a>>,
    /// Why `accounts` is not the database's list: the widget's `accounts_why`.
    pub(super) accounts_why: Option<&'static str>,
    pub(super) unconnected: Vec<TradeWindowAccountPickerConnectRow<'a>>,
    /// The tick chart's prints.
    pub(super) tape: &'a [vike_order_entry::trade_window::tick_chart::TradeWindowTickChartPrint],
    pub(super) status: Option<(TradeWindowStatusLineKind, Cow<'a, str>)>,
    pub(super) held_says: Option<&'a str>,
}

impl TradeWindowParts<'_> {
    /// The widget's inputs for this frame.
    pub(super) fn inputs(&self) -> TradeWindowInputs<'_> {
        TradeWindowInputs {
            venue: self.venue,
            venue_label: self.venue_label,
            product: self.product,
            account: self.account,
            symbol: self.symbol,
            base: self.base,
            quote: self.quote,
            mode: self.mode,
            tradable: if self.tradable {
                TradeWindowTradable::Yes
            } else {
                TradeWindowTradable::No { trades: &self.trades, why: self.why.as_deref() }
            },
            grid: self.grid,
            book: self.book.unwrap_or(&self.no_book),
            top: self.top,
            last: self.last,
            stale: self.stale,
            source: self.source,
            absence: self.absence,
            orders: &self.orders,
            orders_why: self.orders_why,
            position: self.position,
            buying_power: self.buying_power,
            value: self.value,
            capabilities: self.capabilities,
            bracket_why: self.bracket_why,
            bracket_wire: self.bracket_wire,
            post_only_wire: self.post_only_wire,
            matches: &self.matches,
            recent: &self.recent,
            accounts: &self.accounts,
            accounts_why: self.accounts_why,
            unconnected: &self.unconnected,
            tape: self.tape,
            status: self
                .status
                .as_ref()
                .map(|(kind, text)| TradeWindowStatusLine { kind: *kind, text }),
            held_says: self.held_says,
        }
    }
}

/// **The window's inputs, built by one pure function (I14).** No egui, no I/O: the snapshot, the
/// directory, the catalog answers and the window's own state in, the widget's inputs out.
pub(super) fn trade_window_parts<'a>(src: &TradeWindowSources<'a>) -> TradeWindowParts<'a> {
    let snapshot = src.snapshot;
    let address = TradeWindowAddress::parse(src.venue, src.account, src.symbol);
    let block = address.as_ref().and_then(|a| {
        let key = a.route_key();
        snapshot.portfolio.venues.iter().find(|b| b.route_key == key)
    });
    // An account the node names as NOT RUNNING, with its reason and its row's tier (decision 0156).
    //
    // ⚠ A STOPPED engine keeps its published block (its position stays shown, decision 0169 item 5), so a block
    // does not say the account runs: the node's own list does, by route key alone (`CoreSnapshotWire::stopped_row`'s
    // rule), because a tier change lists the account under its NEW tier beside the block's OLD one. Without this a
    // stopped account's window offered trading until the click.
    let said = address.as_ref().and_then(|a| {
        let key = a.route_key();
        src.not_running.iter().find(|n| n.route_key == key)
    });
    // A core that has stopped and an account whose engine is HALTED take nothing from the window
    // (`vike_order_entry::dispatch::tradable`, the dispatcher's own rule since M-6); a REDUCING one still
    // trades and the strip says what it takes. Nor does a desktop that cannot send (item 11).
    let state = block.map(|b| b.trading_state);
    let sends = src.control_link == BackendControlLink::Open;
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
    // it already (`WindowView::trade_account_in_force`), so this fires for the lane engines it is not.
    let lane_why = address
        .as_ref()
        .and_then(|a| perp_lane_account(snapshot, a))
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
    let can_trade = sends
        && symbol_why.is_none()
        && said.is_none()
        && address.as_ref().is_some_and(|a| tradable(snapshot, a));
    // Why it takes none, where that can be stated: the strip's line AND the widget's cause.
    let stated = refusal(snapshot, address.as_ref(), block, said, src.control_link, symbol_why);

    // The book, and the window's price: the book's MID. No last trade price reaches the desktop:
    // `CoreSnapshotWire` carries no marks and `observe_decode::wire_to_core` leaves `marks` empty, so
    // reading them here was a path no snapshot could take (the FW2 review); the bar's hover says
    // "Mid price".
    let real = src.book.filter(|b| b.bid_levels() + b.ask_levels() > 0);
    // A venue's top of book, where it has no book: two sides, or it says nothing a price can be read
    // off. The widget draws no ladder from it (the honesty contract: `vike_order_entry::trade_window`'s module doc).
    let top =
        real.is_none().then_some(src.top).flatten().filter(|(q, _)| q.bid > 0.0 && q.ask > 0.0);
    let last = real.and_then(L2Book::mid).or(top.map(|(q, _)| (q.bid + q.ask) / 2.0));
    let absence = real.is_none().then(|| {
        let a = depth_absence(src.source.map(|s| s.state));
        TradeWindowLadderAbsence {
            headline: if top.is_some() { QUOTE_ONLY_HEADLINE } else { depth_absence_headline(a) },
            cause: src.source.map(|s| s.text.as_str()).or(src.unreachable).unwrap_or("").trim(),
            next_step: if top.is_some() { "" } else { depth_absence_next_step(a) },
        }
    });
    let delay_word = top.and_then(|(q, _)| vike_account_window::orders::delay_badge(q.delay));

    // The grid from the catalog, and no invented lot (I18).
    let listing = src.catalog.listing.as_ref();
    let book_tick = real.map_or(0.0, |b| b.tick_size).max(0.0);
    let mut grid = listing.map_or_else(
        || Grid {
            tick_size: grid_value(book_tick),
            lot_size: None,
            min_notional: None,
            min_qty: None,
        },
        |l| l.grid,
    );
    // ⚠ A row from the datahub's venue list carries NO grid: every tick and lot of a server-fetched
    // catalog is ZERO (measured 2026-10-04: all 1,943 Binance rows). Read as the window's grid that
    // prints every price of a BTC-quoted pair as `0` and lays the ladder on a zero tick, so the BOOK's
    // own tick — which the depth stream does carry — answers when the catalog's is not a positive
    // number. The lot stays unknown, honestly: an order's size is the next step's to solve.
    if grid.tick_size.is_none() {
        grid.tick_size = grid_value(top.map_or(book_tick, |(q, _)| q.tick));
    }
    let (base, quote) = src.catalog.base_quote(src.symbol);

    // The window's own orders: the dispatcher's rule, so the markers are what Cancel all pulls. On a
    // node that names no order's account (Ruling R9) the window cannot tell its own from another
    // account's, so it shows none and says why.
    let attributed = orders_name_their_account(snapshot, src.venue);
    let orders: Vec<TradeWindowLadderOrder> = match &address {
        Some(a) if attributed => window_orders(snapshot, a)
            .map(|o| TradeWindowLadderOrder {
                order_client_id: o.order_client_id.clone(),
                side: o.side,
                price: o.price.or(o.trigger_price).unwrap_or(0.0),
                quantity: o.qty - o.filled_qty,
                is_stop: o.order_type == vike_model::OrderType::Stop,
                words: vike_account_window::orders::order_client_id_words(
                    &o.order_client_id,
                    snapshot,
                    &src.node_words.mounts,
                ),
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
        block.and_then(|b| b.positions.iter().find(|p| p.symbol == src.symbol)).map(|p| {
            TradeWindowPosition {
                size: p.position_side.signed_size(p.size),
                average_price: p.average_price,
                unrealized_pnl: p.unrealized,
            }
        });

    // The symbol picker: the cached rows (every venue the catalog lists, one per instrument), each
    // with its venue's own spelling and whether it is this window's own instrument. No account mark
    // and no price: the v3 design draws neither in the menu, and the menu fetches nothing.
    let matches: Vec<SymbolPickerRow<'a>> = src
        .catalog
        .rows
        .iter()
        .map(|r| SymbolPickerRow {
            venue: &r.venue,
            venue_label: venue_label(src.directory, &r.venue),
            symbol: &r.symbol,
            pair: r.pair.clone(),
            current: r.venue == src.venue && r.symbol == src.symbol,
        })
        .collect();

    let unconnected: Vec<TradeWindowAccountPickerConnectRow<'a>> = match src.directory {
        Some(dir) => dir
            .venues
            .iter()
            .filter(|v| !dir.accounts.iter().any(|a| a.venue == v.name))
            .filter(|v| src.opens(&v.name).is_some())
            .map(|v| TradeWindowAccountPickerConnectRow {
                venue_label: v.title.as_deref().unwrap_or(&v.name),
                product: src.product(&v.name),
                symbol: src.opens(&v.name).unwrap_or(""),
            })
            .collect(),
        None => Vec::new(),
    };

    // A running account whose way to the venue is lost (`BackendNodeWords::lost`). It says so and still takes
    // the click: the node's door judges the order, and some of these (a late stream) still carry them.
    let lost =
        block.filter(|_| said.is_none()).and_then(|b| match src.node_words.link_of(&b.route_key) {
            BackendNodeWordsAccountLink::Lost(lost) => Some(lost.why.as_str()),
            BackendNodeWordsAccountLink::Up | BackendNodeWordsAccountLink::NotKnown => None,
        });

    // The strip, most fundamental first: a stated cause that is an ERROR (a stopped core, a lost
    // control link, a window that names no account, a halted account); then an account cut off from
    // its venue, above the window's own last order, which would otherwise hide it for good; then the
    // window's own last order, note or refusal; then a stated cause that is not (a read-only desktop,
    // waiting for the
    // node, not running on the server); then what the window is missing or limited to; and last a
    // node that did not answer the directory — said wherever nothing more important is, since the
    // window still works without the list (the owner's item 5; the picker always says it).
    let accounts_why = src.directory_unavailable.then_some(ACCOUNT_LIST_UNAVAILABLE);
    let status = match (&stated, src.status) {
        (Some(r), _) if r.kind == TradeWindowStatusLineKind::Error => {
            Some((r.kind, r.text.clone()))
        }
        _ if lost.is_some() => {
            lost.map(|why| (TradeWindowStatusLineKind::Error, Cow::Borrowed(why)))
        }
        (_, Some((kind, text))) => Some((*kind, Cow::Borrowed(text.as_str()))),
        (Some(r), None) => Some((r.kind, r.text.clone())),
        (None, None) if delay_word.is_some() => {
            let word = delay_word.unwrap_or_default();
            Some((TradeWindowStatusLineKind::Info, Cow::Owned(delayed_quote_why(word))))
        }
        (None, None) if grid.lot_size.is_none() => {
            let why = no_lot_why(src.symbol, listing.is_some(), src.catalog.loaded);
            Some((TradeWindowStatusLineKind::Info, Cow::Owned(why)))
        }
        (None, None) if state == Some(TradingState::Reducing) => {
            Some((TradeWindowStatusLineKind::Info, Cow::Borrowed(REDUCING_WHY)))
        }
        // A running account whose key changed in the database keeps trading on the old key until the
        // next restart: a calm standing note, below anything the window refuses or limits itself to.
        (None, None)
            if address
                .as_ref()
                .is_some_and(|a| src.node_words.key_changed.contains(&a.route_key())) =>
        {
            Some((TradeWindowStatusLineKind::Info, Cow::Borrowed(KEY_CHANGED_WARNING)))
        }
        (None, None) => {
            orders_why.or(accounts_why).map(|w| (TradeWindowStatusLineKind::Info, Cow::Borrowed(w)))
        }
    };

    // Why TP/SL cannot go to this account, stated ONCE (I-1 of the final review, slice B, step 3):
    // a window with no address names no account to send it to; and the node refuses a bracket to an
    // engine that does not trade the symbol or whose lane cannot hold the stop-loss, which the window
    // says before the click. The dispatcher refuses both by the same two rules, in the same order. A
    // bracket NAMES its account (`OrderBracket::account`), so a labelled account or a lane twin is not
    // a reason of its own any more.
    let bracket_why = match &address {
        None => Some(TAKE_PROFIT_STOP_LOSS_ACCOUNT_WHY),
        Some(a) if !bracket_on_mounted_symbol(snapshot, a) => {
            Some(TAKE_PROFIT_STOP_LOSS_SYMBOL_WHY)
        }
        Some(a) if !bracket_lane_holds_stop(snapshot, a) => Some(TAKE_PROFIT_STOP_LOSS_LANE_WHY),
        Some(_) => None,
    };

    TradeWindowParts {
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
        top: top.map(|(q, age_ms)| TradeWindowQuoteFace {
            bid: q.bid,
            ask: q.ask,
            age_ms,
            delay: delay_word,
        }),
        last,
        stale: src.stale,
        source: src.source,
        absence,
        orders,
        orders_why,
        position,
        buying_power: block.map(|b| b.free_buying_power),
        // The value the order's own account books the symbol at, read as the dispatcher's preview
        // reads it (`contract_value_at`), so the ticket sizes the order the preview and the gate judge.
        value: address.as_ref().map_or_else(vike_model::ContractValue::default, |a| {
            contract_value_at(snapshot, &a.venue, wire_account(snapshot, a).as_ref(), src.symbol)
        }),
        capabilities: vike_venues::capabilities_for(src.venue),
        bracket_wire: src.bracket_wire,
        post_only_wire: src.post_only_wire,
        matches,
        recent: src.recent.iter().map(|(v, s)| (v.as_str(), s.as_str())).collect(),
        accounts: account_rows(src),
        unconnected,
        tape: src.tape,
        status,
        held_says: src.held_says,
    }
}
