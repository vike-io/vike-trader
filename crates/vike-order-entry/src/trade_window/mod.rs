//! The Trade window (`docs/superpowers/specs/2026-09-30-trade-window-design.md`): ONE window per
//! instrument holding the instrument bar, the ladder, the order ticket and a status strip, the
//! ticket beside the ladder or under it. It replaces the DOM and the old Trade window's ticket.
//! RUST-NATIVE: the Python app has no such window.
//!
//! The seam is the one the DOM had: pure `egui` painting over borrowed [`TradeWindowInputs`] and a
//! caller-owned [`TradeWindowState`], with every trader intent leaving as a neutral [`TradeWindowAction`] that
//! the dispatcher ([`crate::dispatch`]) addresses and maps to `vike_execution` commands. The widget
//! itself reads no execution type: it paints its borrowed inputs from the design system alone.
//!
//! # ⚠ The honesty contract, carried over from the DOM
//!
//! 1. **No book ⇒ no ladder.** [`ladder`] draws [`TradeWindowLadderAbsence`]'s words and nothing ladder-shaped
//!    when [`TradeWindowInputs::book`] has no level on either side: no rows, no bars, no click region.
//! 2. **No price ⇒ a dash.** `last: None` prints `—`, never a number that did not come from a venue.
//! 3. **The window names its own depth link.** [`instrument_bar`]'s FEED badge and the bookless
//!    ladder both read [`TradeWindowInputs::source`] through [`instrument_bar::link_of`]: the state
//!    its producer SET (`vike_model::feed_status::DataClientStatus`'s `state`), never one guessed
//!    from its words, and a venue with no line is a link nobody reported on (`FEED ?`).
//! 4. **No order an account does not trade.** [`TradeWindowTradable::No`] makes every order-sending control
//!    inert and says why (`describe::why_untradable`, spec §4.3): the cause the app states, else what the
//!    account's list shows — never a line of the status strip taken for the cause.
//!
//! This crate's `tests/trade_no_book.rs` gates the first three off the accessibility tree, and
//! `tests/trade_actions.rs` the fourth, with what every click on the window sends.
//!
//! # The window fits the window it is given
//!
//! This crate's `tests/trade_fit.rs` draws the whole window at every width from 260 pt up, in
//! every density and text size, and finds every control inside the window and inside its own
//! region, none lying on another. It tells the regions apart by their names: [`draw`] names each
//! region a group (`Instrument bar`, `Ladder`, `Order ticket`, `Status strip`) for a screen reader.

pub mod address;
pub mod instrument_bar;
pub mod ladder;
pub mod layout;
pub mod sizing;
pub mod status_line;
pub mod symbol_follow;
pub mod tick_chart;
pub mod ticket;

pub mod describe;
mod state;
#[cfg(test)]
mod tests;

use egui::{Align, Layout, Rect, UiBuilder, Vec2, pos2, vec2};
use ladder::absence::TradeWindowLadderAbsence;
use vike_marketdata::L2Book;
use vike_model::feed_status::DataClientStatus;
use vike_model::{AccountTier, Grid};
use vike_ui_theme::components::ThemeTokens;
use vike_ui_theme::components::button::ThemeButtonIcon;
use vike_ui_theme::icons;
use vike_ui_theme::metrics::stroke;
use vike_ui_theme::type_scale::ThemeTextRole;
use vike_ui_theme::value::trade;
use vike_venues::VenueCapabilities;

/// Where the ticket sits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TradeWindowTicketPlace {
    /// Beside the ladder, on its right — the default (spec §3.9).
    Beside,
    /// Under the ladder, for a narrow window.
    Under,
}

/// What the window shows. The ticket is always shown: it is the order entry (spec §3.2). The four
/// toggles of the title bar (the owner's v3 design): the tick chart, the ladder, and where the ticket
/// sits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TradeWindowPanes {
    /// The tick chart pane, to the left of the ladder (off by default).
    pub chart: bool,
    pub ladder: bool,
    pub panel: TradeWindowTicketPlace,
}

/// The ticket's order-type CHOICE: the three kinds the Trade window offers. A form value, not the
/// order vocabulary — `vike_model::OrderType` is that; the desktop's dispatcher maps one onto the
/// other when it builds the order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TradeWindowTicketKind {
    Market,
    Limit,
    Stop,
}

/// The unit the size field is typed in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TradeWindowSizingUnit {
    /// The base coin (`BTC`).
    Base,
    /// The quote currency (`USDT`), converted at the window's price.
    Quote,
}

/// Whether the window takes an order now (spec §4.3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TradeWindowTradable<'a> {
    Yes,
    /// It does not. `trades` is what the account's engine does trade; `why` is the app's own words
    /// for a cause it can STATE (a stopped core, a halted account, an account the server does not
    /// run, a node that has not said anything yet, an account text that names no account) — the
    /// words its status strip uses, computed once by the app — and `None` where the only cause is
    /// that the account trades other symbols. `describe::why_untradable` says which.
    No {
        trades: &'a [String],
        why: Option<&'a str>,
    },
}

/// A catalog number as a field of the window's [`Grid`]: `Some` only for a positive, finite number.
/// A catalog row that gives no tick, lot or minimum spells it `0`, and a negative, infinite or `NaN`
/// one is no grid either: each is `None`, and the window reads `None` where it used to read `0.0`
/// (the owner's ruling of 2026-11-17). Every builder of a window's grid maps its numbers through it,
/// so a `Some` the window reads is always a usable step.
pub fn grid_value(v: f64) -> Option<f64> {
    (v > 0.0 && v.is_finite()).then_some(v)
}

/// A working order drawn on the ladder: the app's light copy of `vike_execution::OrderView`.
#[derive(Clone, Debug, PartialEq)]
pub struct TradeWindowLadderOrder {
    pub order_client_id: String,
    /// `+1` buy, `−1` sell.
    pub side: i32,
    /// The limit price, or a stop's trigger.
    pub price: f64,
    pub quantity: f64,
    pub is_stop: bool,
    /// The order id's friendly words (decision 0270: who made it, for which strategy and account,
    /// what it does), shown under its line in the marker's hover; `None` for an id that says nothing
    /// more (a v1 or a foreign id).
    pub words: Option<String>,
}

/// The open position on this account and symbol.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TradeWindowPosition {
    /// Signed: `+` long, `−` short.
    pub size: f64,
    pub average_price: f64,
    /// Unrealized P/L in the quote currency, computed by the app.
    pub unrealized_pnl: f64,
}

/// A venue's top of book, on a window with no book: what its bookless ladder shows beside the absence
/// and where its ticket takes the bid and the ask. It is two prices and draws nothing ladder-shaped.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct TradeWindowQuoteFace<'a> {
    pub bid: f64,
    pub ask: f64,
    /// How old the quote is, in milliseconds.
    pub age_ms: i64,
    /// The word for a stream that is not real time (`DELAYED`, `FROZEN`), as the app words it.
    pub delay: Option<&'a str>,
}

/// One row of the symbol picker (the owner's v3 design): ONE INSTRUMENT on one venue. The list is
/// flat — a line of venue chips overflowed its row once a symbol was on five venues — and carries no
/// price: the menu fetches none (ruled 2026-10-04).
#[derive(Clone, Debug, PartialEq)]
pub struct SymbolPickerRow<'a> {
    pub venue: &'a str,
    /// The venue's own spelling, from the settings database's `venue.title`.
    pub venue_label: &'a str,
    /// The venue-native symbol picking this row opens.
    pub symbol: &'a str,
    /// The pair and kind in words (`BTC/USDT perpetual`).
    pub pair: String,
    /// This window's own instrument.
    pub current: bool,
}

/// One account in the venue · account picker. The list is the settings database's (the node's
/// `Directory` reply, owner ruling of 2026-09-30): every active account, running or not.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TradeWindowAccountPickerRow<'a> {
    pub venue: &'a str,
    /// The venue's own spelling, from the database's `venue.title`; the key when it has none.
    pub venue_label: &'a str,
    /// The product the row opens on that venue, in the word the row says beside the venue
    /// (`Perp`, `Spot`: `Binance Perp`). Empty where the venue does not list the instrument.
    pub product: &'a str,
    /// The account's label, the address an order is routed by. `None` is the venue's default
    /// account.
    pub account: Option<&'a str>,
    /// The venue-native symbol picking this row opens: the window's own symbol on the window's own
    /// venue, else the catalog's match for the same base and quote on this row's venue. A venue
    /// spells one instrument its own way (`BTCUSDT`, `BTC-USDT-SWAP`), so a pick that kept the old
    /// venue's spelling would subscribe a market the new venue does not have.
    ///
    /// EMPTY on a row whose venue does not list the instrument: the menu lists every account, that
    /// row carries a [`TradeWindowAccountPickerRow::why_not`] and is never picked, so its symbol is never read.
    pub symbol: &'a str,
    /// What the row shows for the account: its label, else the broker's account number when the
    /// venue has more than one unlabelled account, else "main".
    pub name: &'a str,
    /// The account's tier, as its row states it. `None` — the node did not say — is drawn as
    /// unknown (`MODE ?`), never as PAPER: this is the list a trader picks where an order goes from.
    pub mode: Option<AccountTier>,
    /// `None` when the account runs on the server and can be picked. `Some(reason)` draws the row
    /// greyed, with the reason on hover; a greyed row takes no click (spec §3.3).
    pub why_not: Option<&'a str>,
    /// The account runs and its way to the venue is lost, as the app states it: `(word, why)`, a badge
    /// on the row and the sentence on hover. The row stays pickable: its window still shows the book.
    pub lost: Option<(&'a str, &'a str)>,
}

/// A venue whose database holds no account at all: shown with a Connect button (spec §3.3).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TradeWindowAccountPickerConnectRow<'a> {
    /// The venue's own spelling, from the database's `venue.title`; the key when it has none.
    pub venue_label: &'a str,
    /// The product the window's instrument is on that venue (`Perp`), as [`TradeWindowAccountPickerRow::product`].
    pub product: &'a str,
    /// The venue-native symbol the instrument is under there: the row says what Connect would open.
    pub symbol: &'a str,
}

/// How the status strip reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TradeWindowStatusLineKind {
    Info,
    Ok,
    Error,
}

/// The last thing the window did and how it came out (spec §3.8), composed by the app.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TradeWindowStatusLine<'a> {
    pub kind: TradeWindowStatusLineKind,
    pub text: &'a str,
}

/// Everything one frame draws. Borrowed: the app owns the storage.
#[derive(Clone, Copy, Debug)]
pub struct TradeWindowInputs<'a> {
    pub venue: &'a str,
    pub venue_label: &'a str,
    /// The product the window's instrument is (`Perp`, `Spot`): the account button reads
    /// `Binance Perp · main`. Empty when the catalog does not say.
    pub product: &'a str,
    /// `None` is the venue's default account.
    pub account: Option<&'a str>,
    /// The venue-native symbol the account's engine trades under.
    pub symbol: &'a str,
    pub base: &'a str,
    pub quote: &'a str,
    /// What stands behind the account's orders (spec §4.3): the tier its account row states — by
    /// decision 0156 an account runs at that tier or not at all, so it is also what its orders go
    /// to. `None` is "the node did not say" (an older node, or a tier word this build does not
    /// know): treated like LIVE wherever safety is at stake, and DRAWN as unknown (`MODE ?`) —
    /// never as PAPER. (This was the four-valued `AccountMode` enum, whose `Unknown` is now
    /// `None`; the window parses no tier word of its own — a caller holding one parses it with
    /// `AccountTier`'s strict parser and maps a refusal to `None`.)
    pub mode: Option<AccountTier>,
    pub tradable: TradeWindowTradable<'a>,
    pub grid: Grid,
    pub book: &'a L2Book,
    /// The venue's top of book, where it streams no book; `None` beside a book.
    pub top: Option<TradeWindowQuoteFace<'a>>,
    /// The window's price: the book's mid, which the bar's hover names (no last trade price
    /// reaches the desktop), or a quote's. `None` prints a dash.
    pub last: Option<f64>,
    pub stale: bool,
    /// The window's own depth link's status line: the state its producer set, and its words shown
    /// verbatim. `None` when the venue has no line (not known).
    pub source: Option<&'a DataClientStatus>,
    pub absence: Option<TradeWindowLadderAbsence<'a>>,
    pub orders: &'a [TradeWindowLadderOrder],
    /// Why this account's own orders cannot be shown or cancelled here, or `None` when they can.
    /// `Some` on an older node whose venue blocks do not name each order's account (Ruling R9):
    /// the window then draws no own-order markers and words its disabled cancel buttons with this.
    /// The glue sets it to [`crate::dispatch::ORDERS_UNATTRIBUTED_WHY`] there.
    pub orders_why: Option<&'a str>,
    pub position: Option<TradeWindowPosition>,
    /// The account's free buying power (`VenueBlock::free_buying_power`); `None` when unknown.
    pub buying_power: Option<f64>,
    /// What one unit of `symbol` is worth in the book the account's orders reach (the account's
    /// block's grid, `ContractValue::default()` where it states none): the ticket sizes a quote
    /// amount and prints a value with it, the numbers the preview and the node's gate judge.
    pub value: vike_model::ContractValue,
    /// The venue's declared capabilities: drag-to-reprice needs `allows_modify`.
    pub capabilities: VenueCapabilities,
    /// Why a TP/SL bracket cannot go to this account, as the app STATES it (the `TradeWindowTradable::No::why`
    /// pattern: the glue words the cause once and the ticket says it); `None` where one can. The
    /// glue's causes, in its order: an account other than its venue's single default book (Ruling R3,
    /// [`crate::dispatch::wire_account`]), then an engine whose lane cannot hold the stop-loss (the
    /// shipped daemon's spot binance engine; [`crate::dispatch::bracket_lane_holds_stop`], the node's
    /// own rule). The ticket never infers a cause itself ([`ticket::take_profit_stop_loss_block`]).
    pub bracket_why: Option<&'a str>,
    /// Whether the backend can carry a bracket at all — `false` while the desktop's lift has no wire
    /// form for one (Ruling R8; `command_lift::bracket_has_wire_form`).
    pub bracket_wire: bool,
    /// Whether the connected node carries a post-only order: it advertises the word the wire owes a
    /// lifetime (`vike_tradehub_client::proto::FEATURE_ORDER_TIME_IN_FORCE`). `false` against a node too old to
    /// take the field, which would drop it and run the order as a plain limit that takes liquidity
    /// behind a normal acknowledgement. Whether the VENUE has a post-only order is `capabilities`' answer
    /// ([`ticket::post_only_block`] reads both); the glue derives this from the control handle
    /// (`command_lift::node_takes_post_only`).
    pub post_only_wire: bool,
    /// The symbol picker's rows, ranked (see [`SymbolPickerRow`]).
    pub matches: &'a [SymbolPickerRow<'a>],
    /// The recently used instruments, newest first, as `(venue, venue-native symbol)` pairs. A
    /// symbol is only an address together with its venue: picking one moves the window to that
    /// venue as well (spec §3.3).
    pub recent: &'a [(&'a str, &'a str)],
    pub accounts: &'a [TradeWindowAccountPickerRow<'a>],
    /// Why `accounts` is not the account list the server's database holds, as the app states it
    /// (the owner's decision of 10-03, item 5: "Account list unavailable." when the node did not
    /// answer the directory); the account picker shows it at the top. `None` when it is.
    pub accounts_why: Option<&'a str>,
    pub unconnected: &'a [TradeWindowAccountPickerConnectRow<'a>],
    /// The prints of the last two minutes on this window's instrument, oldest first, for the tick
    /// chart's Trades layer: the app lends them from the tape it keeps and the widget draws them
    /// (empty where the venue serves no trade stream).
    pub tape: &'a [tick_chart::TradeWindowTickChartPrint],
    pub status: Option<TradeWindowStatusLine<'a>>,
    /// What the node says of the order held for a confirm, once it has answered the app's dry run: that
    /// it would refuse it, or the quantity it will go out with. The confirm prompt says it, before the
    /// Place it asks about; `None` while nothing is held, nothing was said, or the app did not ask.
    pub held_says: Option<&'a str>,
}

/// A bracket's exits, as absolute prices on the tick grid.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TradeWindowTicketExits {
    pub take_profit: f64,
    pub stop_loss: f64,
}

/// Where an order was made, so a rejection can name its gesture ([`crate::dispatch`]'s
/// `OrderEntryDispatchSource`, Ruling R5).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TradeWindowOrigin {
    /// A click on the ladder.
    Ladder,
    /// The ticket's Buy and Sell buttons.
    Ticket,
}

/// A trader intent leaving the widget. The app addresses it with the window's venue, account and
/// symbol, and maps it to `vike_execution` commands through [`crate::dispatch`].
#[derive(Clone, Debug, PartialEq)]
pub enum TradeWindowAction {
    /// A new order. `price` is the limit price (Limit), the trigger (Stop), or `None` (Market).
    /// `post_only` makes it a maker-only LIMIT order that the venue refuses, rather than executes, when
    /// it would take liquidity (`vike_model::TimeInForce::PostOnly`); the ticket sets it only where
    /// [`ticket::post_only_block`] finds no reason against it, and the dispatcher refuses one that
    /// carries exits (a bracket's entry has no lifetime on the wire).
    Place {
        side: i32,
        order_type: TradeWindowTicketKind,
        price: Option<f64>,
        quantity: f64,
        reduce_only: bool,
        post_only: bool,
        exits: Option<TradeWindowTicketExits>,
        origin: TradeWindowOrigin,
    },
    /// Reprice a resting order (drag).
    Modify { order_client_id: String, new_price: f64 },
    /// Cancel these resting orders — a marker can stand for several.
    Cancel(Vec<String>),
    /// Cancel every resting order on one side of this symbol (`+1` buys, `−1` sells).
    CancelSide(i32),
    /// Cancel every resting order on this symbol.
    CancelAll,
    /// Flatten the position at market (reduce-only).
    ClosePosition,
    /// Close, then open the opposite position, at market.
    Reverse,
    /// Move the window to `symbol` on `venue` (a match or a recent pick).
    PickSymbol { venue: String, symbol: String },
    /// Move the window to this account, on `symbol`: the instrument's spelling on the account's OWN
    /// venue ([`TradeWindowAccountPickerRow::symbol`]), so a pick on another venue never keeps the old venue's.
    PickAccount { venue: String, account: Option<String>, symbol: String },
    /// Open the Connections window.
    OpenConnections,
    /// A line for the status strip about something the widget did; the app shows it.
    Note { kind: TradeWindowStatusLineKind, text: String },
}

/// The note the widget leaves when a window lands on a LIVE account (spec §3.11).
pub(crate) const ONE_CLICK_OFF_ON_LIVE: &str =
    "One-click trading is off on a LIVE account: every order asks first.";

/// ...and on an account whose mode the node did not report: treated as LIVE, never CALLED LIVE
/// while its chip reads `MODE ?` (final review A, minor 5).
pub(crate) const ONE_CLICK_OFF_ON_UNKNOWN: &str =
    "One-click trading is off on an account whose mode is not known: every order asks first.";

/// Cross-frame view state for one window, owned by the app.
#[derive(Clone, Debug)]
pub struct TradeWindowState {
    pub view: TradeWindowPanes,
    /// The tick chart pane's layers.
    pub layers: tick_chart::TradeWindowTickChartLayers,
    /// What the window has seen of its book, for the tick chart: forgotten when the window moves to
    /// another instrument ([`TradeWindowState::sync`]).
    pub history: tick_chart::TradeWindowTickChartHistory,
    pub order_type: TradeWindowTicketKind,
    pub unit: TradeWindowSizingUnit,
    /// The size field's text, in [`TradeWindowState::unit`].
    pub size: String,
    /// The price (Limit) or trigger (Stop) field's text.
    pub price: String,
    pub reduce_only: bool,
    /// The trader's Post only choice: a maker-only limit order. Unlike TP/SL it is never left ticked
    /// where it cannot be used: [`TradeWindowState::sync`] turns it off the moment the node, the venue or the
    /// ticket's order type (`ticket::post_only_block`) stops offering it, so the pressed button is always
    /// the order that goes out.
    pub post_only: bool,
    /// The trader's TP/SL choice. Where TP/SL cannot be used (`ticket::take_profit_stop_loss_block`), a TICKED choice
    /// refuses the ticket's Buy and Sell until the trader turns it off: the ticket never sends an
    /// order without the exits the trader set (fix round 1, I-3). A ladder click carries no exits
    /// by design (spec §3.6), whatever it says.
    pub take_profit_stop_loss: bool,
    pub take_profit_text: String,
    pub stop_loss_text: String,
    pub one_click: bool,
    /// Ticks per ladder row. The trader's choice is remembered for each instrument (venue and symbol)
    /// for the life of the window ([`TradeWindowState::sync`] stores it when the window leaves an instrument
    /// and restores it when the window comes back); until he chooses, 1 and then the automatic one
    /// (`auto_group`).
    pub group: i64,
    /// Whether the ladder shows its Vol column (the toolbar's first button), beside the ticket.
    pub volume: bool,
    /// A manual ladder centre; `None` follows the book.
    pub center: Option<f64>,
    /// The price the rows are centred on while they follow the book: the mid, held until the mid leaves
    /// the rows it draws (`follow_book`). So the rows, and the chart's on them, stand still while the
    /// price is on screen, instead of jumping with every tick.
    pub anchor: Option<f64>,
    /// The order client ids being dragged to a new price.
    pub drag: Option<Vec<String>>,
    /// The order waiting for a confirm (one-click off).
    pub held: Option<TradeWindowAction>,
    /// The symbol picker's search text.
    pub query: String,
    /// The highlighted row of the symbol picker.
    pub hot: usize,
    /// The instrument this state was last synced to: venue, account, symbol.
    address: Option<(String, Option<String>, String)>,
    /// The [`TradeWindowState::group`] the trader CHOSE on each instrument this window has left, keyed by
    /// `(venue, symbol)` and NOT by account: the book, and so the grouping, belongs to the venue's
    /// symbol, so another account on the same instrument keeps its group. Lives in the window's
    /// state only; it is gone when the window closes.
    groups: std::collections::HashMap<(String, String), i64>,
    /// Whether [`TradeWindowState::group`] is the trader's on this instrument (a Group − or + press, now or
    /// before he left it), which is kept and remembered in `groups`. Until then `follow_book` picks
    /// it from the price's own steps (`auto_group`).
    group_chosen: bool,
    /// The mid `follow_book` last saw on this instrument, and the sizes of its first
    /// [`AUTO_GROUP_STEPS`] moves: what the automatic grouping is picked from.
    mid_seen: Option<f64>,
    steps: Vec<f64>,
    /// The account mode this state was last synced to.
    mode_seen: Option<Option<AccountTier>>,
    /// The size text a sync seeded while the lot was NOT known (`0`: no lot, no size), kept until
    /// the lot is known, so the size is seeded again then unless the trader changed it.
    unknown_lot_seed: Option<String>,
    /// The position's signed size a held Close or Reverse was asked against: what its prompt names,
    /// and what the confirm must still find (final review A, minor 3). `None` for any other order.
    held_position: Option<f64>,
    /// The instrument's tick and lot this state was last synced to, as bits (a grid that is not a
    /// number stays the same grid): a held order is priced and sized on them (final review A,
    /// minor 8).
    grid_seen: Option<(Option<u64>, Option<u64>)>,
    /// Whether the ticket [`draw`] lays out is the compact one (`layout::compact_ticket`), which
    /// sends at market whatever type the trader chose. The status strip words the ticket's refusal
    /// for the order THAT ticket sends ([`TradeWindowState::entry`]; FW6, I3).
    compact: bool,
}

impl Default for TradeWindowState {
    fn default() -> Self {
        TradeWindowState {
            view: TradeWindowPanes {
                chart: false,
                ladder: true,
                panel: TradeWindowTicketPlace::Beside,
            },
            layers: tick_chart::TradeWindowTickChartLayers::default(),
            history: tick_chart::TradeWindowTickChartHistory::default(),
            order_type: TradeWindowTicketKind::Limit,
            unit: TradeWindowSizingUnit::Base,
            size: String::new(),
            price: String::new(),
            reduce_only: false,
            post_only: false,
            take_profit_stop_loss: false,
            take_profit_text: "0.5".to_string(),
            stop_loss_text: "0.3".to_string(),
            one_click: false,
            group: 1,
            volume: false,
            center: None,
            anchor: None,
            drag: None,
            held: None,
            query: String::new(),
            hot: 0,
            address: None,
            groups: std::collections::HashMap::new(),
            group_chosen: false,
            mid_seen: None,
            steps: Vec::new(),
            mode_seen: None,
            unknown_lot_seed: None,
            held_position: None,
            grid_seen: None,
            compact: false,
        }
    }
}

/// Add `button` under an id of its own, `key`, named for what the button DOES (Buy, Sell, Cancel
/// all, …), never for where it sits.
///
/// ⚠ egui gives a click to the id the PRESS landed on, and at the release does not ask where the
/// pointer is; a kit button's own id is its POSITION in its row. So where the rows re-pack between
/// a press and its release — Buy and Sell stacking when a quote-sized label widens on a tick of the
/// book, the cancel row moving under them — the button that took the pressed one's place took its
/// click: a press on Cancel all was released as a SELL (W4 fix round 1, I-1). Keyed, a button that
/// moved keeps its click, and one that is gone takes none. The ticket keys every order and cancel
/// button, the strip its answers, the ladder's toolbar its buttons.
fn keyed(ui: &mut egui::Ui, key: egui::Id, button: impl egui::Widget) -> egui::Response {
    ui.scope_builder(UiBuilder::new().scope_id(key), |ui| ui.add(button)).inner
}

/// One part of a region, laid out as `ui` lays out, under its OWN id, `salt` under `key`, with what
/// `add` returns.
///
/// ⚠ egui gives a widget an id by its POSITION (it counts them), and a part that is there in one
/// frame and not the next shifts the id of every widget after it — and with it a field's focus and
/// a held press. In the full ticket's form: the flat caption a fill takes away, the reason an
/// account that stops trading adds, the TP/SL exits; a press held on a quick size across a fill
/// was released on another widget's id and set nothing, and a price being typed lost the field's
/// focus to the shift (W4 fix round 2, NEW-1). In the instrument bar: the spread that comes and
/// goes, the mode chip's two shapes, the row a picker sits on, a picker's list (the F wave's
/// audit). A section's id is its salt, not its place, so what one section draws never moves
/// another's ids, whether it is there or not.
fn section<R>(
    ui: &mut egui::Ui,
    key: egui::Id,
    salt: impl egui::AsIdSalt,
    add: impl FnOnce(&mut egui::Ui) -> R,
) -> R {
    ui.scope_builder(UiBuilder::new().scope_id(key.with(salt)), add).inner
}

/// How wide a kit button with `words` is drawn: its words in the Strong role and the cell padding
/// each side (the kit's own padding). The rows that must decide BEFORE they draw whether their
/// buttons fit (the ticket's send and cancel rows, the strip's answers and its height) measure with
/// this.
fn button_w(context: &egui::Context, t: &ThemeTokens, words: &str) -> f32 {
    text_w(context, words, &t.font(ThemeTextRole::Strong), t) + 2.0 * t.metrics.padding
}

/// How wide `words` are drawn in `font`, on one line.
fn text_w(context: &egui::Context, words: &str, font: &egui::FontId, t: &ThemeTokens) -> f32 {
    context.fonts_mut(|f| f.layout_no_wrap(words.to_string(), font.clone(), t.theme.text)).size().x
}

/// The row height egui gives `font`: what the status strip, which lays its own lines out, counts
/// them by.
fn row_h(context: &egui::Context, font: &egui::FontId) -> f32 {
    context.fonts_mut(|f| f.row_height(font))
}

/// How tall a label of one line in `font` is drawn: its galley's height, which egui rounds to the
/// pixel grid, so it is not [`row_h`]. MEASURED at one point per pixel: mono Body at Large text has
/// a 15.84 pt row and draws a 16 pt label, mono Caption a 14.53 pt row and a 15 pt label. The
/// height a window opens at (`layout::window_size`) counts the full ticket's lines of text with
/// this, because they are labels that lay themselves out; counted by [`row_h`] the form came out
/// 1.3 pt short at Large text, and TP/SL a point below the fold.
fn label_h(context: &egui::Context, font: &egui::FontId, t: &ThemeTokens) -> f32 {
    context.fonts_mut(|f| f.layout_no_wrap("0".to_string(), font.clone(), t.theme.text)).size().y
}

/// Draw one frame into all of `ui`'s free space and return this frame's intents: the instrument
/// bar on top, the status strip at the bottom, and between them the ladder and the ticket as
/// [`layout::split`] places them. Each region is a clipped child with an id of its own, so nothing
/// in one can spill onto, or be clicked in, another (the DOM's strip rule), and a field keeps its
/// focus when the ladder is shown or hidden.
pub fn draw(
    ui: &mut egui::Ui,
    state: &mut TradeWindowState,
    inputs: &TradeWindowInputs<'_>,
) -> Vec<TradeWindowAction> {
    let mut actions = Vec::new();
    state.sync(inputs, &mut actions);
    state.history.observe(vike_model::now_ms(), inputs.book);
    let t = ThemeTokens::of(ui.ctx());
    let full = ui.available_rect_before_wrap();
    ui.allocate_rect(full, egui::Sense::hover());
    let bar_h =
        instrument_bar::height_of(instrument_bar::rows_for(ui.ctx(), &t, inputs, full.width()), &t);
    let bar = Rect::from_min_size(full.min, vec2(full.width(), bar_h));
    let strip_h = status_line::height(ui.ctx(), &t, state, inputs, full.width());
    let strip_top = (full.max.y - strip_h).max(bar.max.y);
    let strip = Rect::from_min_max(pos2(full.min.x, strip_top), full.max);
    // The body runs from the bar's bottom edge to the strip's top: the design has no gap between its
    // instrument band, its ladder and ticket, and its status row (each draws its own rule and its
    // own padding), and a gap here put both columns 5 pt lower than the design's.
    let body = Rect::from_min_max(
        pos2(full.min.x, bar.max.y),
        pos2(full.max.x, strip.min.y.max(bar.max.y)),
    );
    let bar_ui = &mut region(ui, bar, "trade_bar", REGION_BAR);
    instrument_bar::bar(bar_ui, &t, state, inputs, &mut actions);
    let rects = layout::split(body, state.view, &t.metrics, layout::ticket_w(&t));
    state.compact = layout::compact_ticket(state.view, &rects);
    let rows_h = match (rects.ladder, rects.chart) {
        (Some(l), _) => ladder::rows_region(l, &t, state, inputs).map(|r| r.height()),
        (None, Some(c)) => Some(tick_chart::own_rows(c, &t).height()),
        (None, None) => None,
    };
    if let Some(h) = rows_h {
        follow_book(state, inputs, ladder::row_count(h, t.metrics.row_height));
    }
    rules(ui, &t, bar, body, &rects);
    if let Some(r) = rects.chart {
        // The chart's rows are the ladder's: its own rect where one is drawn, else the pane's.
        let region_of_rows = match rects.ladder {
            Some(l) => ladder::rows_region(l, &t, state, inputs),
            None => Some(tick_chart::own_rows(r, &t)),
        };
        let frame = region_of_rows
            .and_then(|reg| ladder::rows::TradeWindowLadderRowFrame::of(reg, &t, state, inputs));
        let chart_ui = &mut region(ui, r, "trade_chart", REGION_CHART);
        tick_chart::draw(chart_ui, &t, state, inputs, frame);
    }
    if let Some(r) = rects.ladder {
        let ladder_ui = &mut region(ui, r, "trade_ladder", REGION_LADDER);
        ladder::draw(ladder_ui, &t, state, inputs, &mut actions);
    }
    let mut ticket_ui = region(ui, rects.ticket, "trade_ticket", REGION_TICKET);
    if state.compact {
        ticket::compact::compact(&mut ticket_ui, &t, state, inputs, &mut actions);
    } else {
        ticket::full::full(&mut ticket_ui, &t, state, inputs, &mut actions);
    }
    // The strip's hairline: the design's status row sits under a rule across the window.
    ui.painter().hline(
        strip.x_range(),
        strip.min.y + stroke::HAIRLINE / 2.0,
        egui::Stroke::new(stroke::HAIRLINE, t.theme.border),
    );
    let strip_ui = &mut region(ui, strip, "trade_status", REGION_STRIP);
    status_line::strip(strip_ui, &t, state, inputs, &mut actions);
    // A click this pass can hold an order (or let one go) whose prompt needs another strip height
    // than the one this pass laid out before it saw the click (`status_line::height`, above). The pass
    // is DISCARDED and drawn again at once, with the strip the prompt needs, so no frame shows the
    // prompt in the old strip (W4 fix round 2, N3). The click is not seen twice: egui hands the
    // second pass no input events. (egui draws the frame after any frame with input regardless,
    // so without this the cut prompt showed for exactly one frame; MEASURED by the kill proof.)
    // The second pass is always there to take: input reaches a frame's FIRST pass alone, so the
    // click, and this request with it, are always in the first pass, and egui grants a discard
    // while a pass is left (`Options::max_passes`, 2 by default; this app sets none). Only a
    // `max_passes` of 1 would bring the one cut frame back.
    if status_line::height(ui.ctx(), &t, state, inputs, full.width()) != strip_h {
        ui.ctx().request_discard("the held prompt needs another strip height");
    }
    actions
}

/// How many moves of the mid the automatic grouping waits for (`follow_book`).
const AUTO_GROUP_STEPS: usize = 5;

/// How many typical steps of the price the visible rows cover at the automatic grouping.
const AUTO_GROUP_SPAN: f64 = 10.0;

/// **The grouping a ladder opens with**: the smallest of 1, 2, 5, 10, 20, 50, … ticks a row at which
/// `n_rows` rows cover [`AUTO_GROUP_SPAN`] typical steps of the price, so most steps stay on screen
/// and the rows stand still. At one tick a row they did not: measured on the CI box (bybit BTCUSDT, 0.1
/// tick), the top of book holds for seconds and then steps a median 3.1, and a 21-row ladder spans 2.1,
/// so it re-centred about 35 times a minute; this picks 20 ticks there, a 42 span. PURE.
pub(crate) fn auto_group(typical_step: f64, n_rows: usize, tick: f64) -> i64 {
    let need = AUTO_GROUP_SPAN * typical_step / (n_rows.max(1) as f64 * tick);
    (0..18)
        .flat_map(|power| [1, 2, 5].map(|m| m * 10_i64.pow(power)))
        .find(|&group| group as f64 >= need)
        .unwrap_or(1)
}

/// The high median of `values` (empty: 0). PURE.
fn median(values: &[f64]) -> f64 {
    let mut v = values.to_vec();
    v.sort_by(f64::total_cmp);
    vike_math::statistics::high_median_of_sorted(&v).unwrap_or(0.0)
}

/// Keep [`TradeWindowState::anchor`] on the book without chasing it: the mid becomes the anchor when there is
/// none, or when the mid's row has LEFT the rows the anchor draws (one row short of either edge, so
/// the touch stays on screen). While it is in view the rows stand still: every level keeps its row,
/// and the chart drawn on them does not redraw. Recenter (the toolbar's ⌖) clears the anchor at once.
///
/// ⚠ It re-centred when the mid drifted a QUARTER of the rows until 2026-11-16, which moved every row
/// while the price was still on screen; the owner saw a ladder that "redraws all the time". Measured
/// on the CI box's own depth (bybit and binance BTCUSDT, 50 levels a side at 10 frames a second): the top
/// of book holds for seconds and then steps a dollar or more, so at one tick a row most steps leave
/// a 21-row view whatever the rule, and the rule's job is not to add shifts of its own.
fn follow_book(state: &mut TradeWindowState, inputs: &TradeWindowInputs<'_>, n_rows: usize) {
    let mid = ladder::book_centre(inputs);
    if !mid.is_finite() || mid <= 0.0 {
        return;
    }
    let tick = ladder::rows::row_tick(inputs.grid.tick_size, inputs.book.tick_size);
    // The automatic grouping, picked ONCE per instrument from its first moves, unless the trader chose.
    if let Some(seen) = state.mid_seen
        && seen != mid
        && state.steps.len() < AUTO_GROUP_STEPS
    {
        state.steps.push((mid - seen).abs());
        if state.steps.len() == AUTO_GROUP_STEPS && !state.group_chosen {
            state.group = auto_group(median(&state.steps), n_rows, tick);
        }
    }
    state.mid_seen = Some(mid);
    let group = state.group.max(1);
    // The ladder's own row arithmetic (`ladder::rows::rows`): the top row's key is the centre's plus half.
    let half = (n_rows / 2) as i64;
    let margin = i64::from(n_rows >= 5);
    let in_view = |anchor: f64| {
        let top = ladder::rows::row_key(anchor, tick, group).saturating_add(half);
        let bottom = top.saturating_sub(n_rows as i64 - 1);
        let at = ladder::rows::row_key(mid, tick, group);
        at <= top.saturating_sub(margin) && at >= bottom.saturating_add(margin)
    };
    state.anchor = match state.anchor {
        Some(a) if in_view(a) => Some(a),
        _ => Some(mid),
    };
}

/// The hairlines the design draws between its regions: under the instrument bar, between the chart and
/// the ladder, and between the ladder and the ticket beside it. (The compact ticket under them draws
/// its own top rule.)
fn rules(
    ui: &egui::Ui,
    t: &ThemeTokens,
    bar: Rect,
    body: Rect,
    rects: &layout::TradeWindowBodyRects,
) {
    let stroke = egui::Stroke::new(stroke::HAIRLINE, t.theme.border);
    let p = ui.painter();
    p.hline(bar.x_range(), bar.max.y - stroke::HAIRLINE / 2.0, stroke);
    let tall = body.y_range();
    if let (Some(chart), Some(ladder)) = (rects.chart, rects.ladder)
        && chart.max.x < ladder.min.x
    {
        p.vline((chart.max.x + ladder.min.x) / 2.0, tall, stroke);
    }
    if (rects.ladder.is_some() || rects.chart.is_some()) && state_is_beside(rects, body) {
        p.vline(rects.ticket.min.x - stroke::HAIRLINE, tall, stroke);
    }
}

/// Whether the ticket in `rects` sits beside what is drawn (the side layout): it starts to the right
/// of it, on the body's own top edge.
fn state_is_beside(rects: &layout::TradeWindowBodyRects, body: Rect) -> bool {
    rects.ticket.min.y == body.min.y && rects.ticket.min.x > body.min.x
}

/// What a screen reader calls each of the window's regions. Each region is a GROUP under this name,
/// so a reader can move between them, and so a test can tell which region drew a control.
const REGION_BAR: &str = "Instrument bar";
const REGION_CHART: &str = "Tick chart";
const REGION_LADDER: &str = "Ladder";
const REGION_TICKET: &str = "Order ticket";
const REGION_STRIP: &str = "Status strip";

/// A clipped child of `ui` filling `rect`, its id `salt` under `ui`'s (the same id whichever
/// regions were drawn before it this frame), and a group named `name` to a screen reader.
fn region(ui: &mut egui::Ui, rect: Rect, salt: &str, name: &str) -> egui::Ui {
    let mut child = ui.new_child(
        UiBuilder::new()
            .scope_id(ui.scope_id().with(salt))
            .max_rect(rect)
            .layout(Layout::top_down(Align::Min)),
    );
    child.shrink_clip_rect(rect);
    name_group(&child, rect, name);
    child
}

/// Make `ui`'s accessibility node a group named `name` that covers `rect`: egui files every child
/// `Ui` as an unnamed container, which a screen reader cannot tell from any other.
fn name_group(ui: &egui::Ui, rect: Rect, name: &str) {
    ui.ctx().accesskit_node_builder(ui.unique_id(), |node| {
        node.set_role(egui::accesskit::Role::Group);
        node.set_label(name);
        node.set_bounds(egui::accesskit::Rect {
            x0: rect.min.x.into(),
            y0: rect.min.y.into(),
            x1: rect.max.x.into(),
            y1: rect.max.y.into(),
        });
    });
}

/// The view controls for the title bar's slot (the v3 design's four toggles): the tick chart and
/// the ladder, a hairline, then the ticket beside / under, each pressed while its view is shown, at
/// the design's size ([`trade::VIEW_BUTTON`], `layout::VIEW_CONTROLS_W` across) whatever the
/// density. Returns the window size
/// the new view asks for ([`layout::window_size`], in this `ui`'s look) when a click changed it; a
/// click that leaves that size as it was (moving a ticket that stands alone) asks for nothing, so a
/// window the trader sized is left alone.
pub fn view_controls(ui: &mut egui::Ui, state: &mut TradeWindowState) -> Option<Vec2> {
    let before = state.view;
    let t = ThemeTokens::of(ui.ctx());
    ui.spacing_mut().item_spacing.x = layout::VIEW_GAP;
    let button =
        |icon, tip| ThemeButtonIcon::new(icon, tip).sized(trade::VIEW_BUTTON, ThemeTextRole::Body);
    let chart_tip = if state.view.chart { "Hide the tick chart" } else { "Show the tick chart" };
    if ui.add(button(icons::CHART, chart_tip).selected(state.view.chart)).clicked() {
        state.view.chart = !state.view.chart;
    }
    let ladder_tip = if state.view.ladder { "Hide the ladder" } else { "Show the ladder" };
    if ui.add(button(icons::DOM, ladder_tip).selected(state.view.ladder)).clicked() {
        state.view.ladder = !state.view.ladder;
    }
    // The group gap, the hairline, the group gap: each `add_space` stands in for the item gap the
    // allocation after it would otherwise add on top.
    ui.add_space(trade::VIEW_GROUP_GAP - layout::VIEW_GAP);
    let (sep, _) = ui.allocate_exact_size(
        vec2(stroke::HAIRLINE, vike_ui_theme::chrome::SEPARATOR_HEIGHT),
        egui::Sense::hover(),
    );
    ui.painter().rect_filled(sep, 0.0, t.theme.border);
    ui.add_space(trade::VIEW_GROUP_GAP - layout::VIEW_GAP);
    for (panel, icon, tip) in [
        (TradeWindowTicketPlace::Beside, icons::PANEL_BESIDE, "Ticket beside the ladder"),
        (TradeWindowTicketPlace::Under, icons::PANEL_UNDER, "Ticket under the ladder"),
    ] {
        if ui.add(button(icon, tip).selected(state.view.panel == panel)).clicked() {
            state.view.panel = panel;
        }
    }
    if state.view == before {
        return None;
    }
    let size = |view| layout::window_size(view, ui.ctx());
    let (was, now) = (size(before), size(state.view));
    (now != was).then_some(now)
}
