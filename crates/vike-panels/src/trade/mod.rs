//! The Trade window (`docs/superpowers/specs/2026-09-30-trade-window-design.md`): ONE window per
//! instrument holding the instrument bar, the ladder, the order ticket and a status strip, the
//! ticket beside the ladder or under it. It replaces the DOM and the old Trade window's ticket.
//! RUST-NATIVE: the Python app has no such window.
//!
//! The seam is the one the DOM had: pure `egui` painting over borrowed [`TradeInputs`] and a
//! caller-owned [`TradeState`], with every trader intent leaving as a neutral [`TradeAction`] that
//! the app addresses and maps to `vike_exec` commands. No dependency on the execution core: this
//! crate depends on vike-model and vike-ui-theme only, and paints from the design system alone.
//!
//! # ⚠ The honesty contract, carried over from the DOM
//!
//! 1. **No book ⇒ no ladder.** [`ladder`] draws [`BookAbsence`]'s words and nothing ladder-shaped
//!    when [`TradeInputs::book`] has no level on either side: no rows, no bars, no click region.
//! 2. **No price ⇒ a dash.** `last: None` prints `—`, never a number that did not come from a venue.
//! 3. **The window names its own depth link.** [`instrument`]'s FEED badge and the bookless
//!    ladder both read [`TradeInputs::source`] through [`instrument::link_of`]: an empty source is
//!    a link nobody reported on (`FEED ?`), and any other is classified by
//!    `vike_model::feed_status::parse_feed_status`, the one classifier the Connections tool and the
//!    daemon's health gate read.
//! 4. **No order an account does not trade.** [`Tradable::No`] makes every order-sending control
//!    inert and says why ([`why_untradable`], spec §4.3): the cause the app states, else what the
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

pub mod chart;
pub mod instrument;
pub mod ladder;
pub mod layout;
pub mod sizing;
pub mod status;
pub mod ticket;

use egui::{Align, Layout, Rect, UiBuilder, Vec2, pos2, vec2};
use vike_model::{L2Book, VenueCaps};
use vike_ui_theme::components::Tokens;
use vike_ui_theme::components::button::IconButton;
use vike_ui_theme::icons;
use vike_ui_theme::metrics::stroke;
use vike_ui_theme::type_scale::TextRole;
use vike_ui_theme::value::trade;

/// Where the ticket sits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Panel {
    /// Beside the ladder, on its right — the default (spec §3.9).
    Beside,
    /// Under the ladder, for a narrow window.
    Under,
}

/// What the window shows. The ticket is always shown: it is the order entry (spec §3.2). The four
/// toggles of the title bar (the owner's v3 design): the tick chart, the ladder, and where the ticket
/// sits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct View {
    /// The tick chart pane, to the left of the ladder (off by default).
    pub chart: bool,
    pub ladder: bool,
    pub panel: Panel,
}

/// The ticket's order type.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OrderType {
    Market,
    Limit,
    Stop,
}

/// The unit the size field is typed in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SizeUnit {
    /// The base coin (`BTC`).
    Base,
    /// The quote currency (`USDT`), converted at the window's price.
    Quote,
}

/// What stands behind the account's orders, as far as the window knows (spec §4.3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AccountMode {
    Paper,
    Demo,
    Live,
    /// The node did not say (an older node, or a tier word this build does not know). Treated like
    /// LIVE wherever safety is at stake, and DRAWN as unknown — never as PAPER (spec §4.3).
    Unknown,
}

impl AccountMode {
    /// The kit's mode for a REPORTED mode; `None` for one nobody reported (drawn `MODE ?`, never as
    /// PAPER — spec §4.3).
    #[must_use]
    pub fn kit(self) -> Option<vike_ui_theme::components::chip::Mode> {
        use vike_ui_theme::components::chip::Mode;
        match self {
            AccountMode::Paper => Some(Mode::Paper),
            AccountMode::Demo => Some(Mode::Demo),
            AccountMode::Live => Some(Mode::Live),
            AccountMode::Unknown => None,
        }
    }
}

/// Whether the window takes an order now (spec §4.3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tradable<'a> {
    Yes,
    /// It does not. `trades` is what the account's engine does trade; `why` is the app's own words
    /// for a cause it can STATE (a stopped core, a halted account, an account the server does not
    /// run, a node that has not said anything yet, an account text that names no account) — the
    /// words its status strip uses, computed once by the app — and `None` where the only cause is
    /// that the account trades other symbols. [`why_untradable`] says which.
    No {
        trades: &'a [String],
        why: Option<&'a str>,
    },
}

/// The instrument's trading grid, from the instrument catalog.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Grid {
    pub tick: f64,
    pub lot: f64,
    pub min_qty: f64,
}

/// A working order drawn on the ladder: the app's light copy of `vike_exec::OrderView`.
#[derive(Clone, Debug, PartialEq)]
pub struct LadderOrder {
    pub client_order_id: String,
    /// `+1` buy, `−1` sell.
    pub side: i32,
    /// The limit price, or a stop's trigger.
    pub price: f64,
    pub qty: f64,
    pub is_stop: bool,
}

/// The open position on this account and symbol.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Position {
    /// Signed: `+` long, `−` short.
    pub size: f64,
    pub avg_px: f64,
    /// Unrealized P/L in the quote currency, computed by the app.
    pub upnl: f64,
}

/// The WORDS a bookless ladder shows — the glance answer, the market-data session's own status line
/// verbatim, and what an operator can do. The widget takes the no-book DECISION from the book,
/// never from this.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct BookAbsence<'a> {
    pub headline: &'a str,
    pub cause: &'a str,
    pub next_step: &'a str,
}

/// One row of the symbol picker (the owner's v3 design): ONE INSTRUMENT on one venue. The list is
/// flat — a line of venue chips overflowed its row once a symbol was on five venues — and carries no
/// price: the menu fetches none (ruled 2026-10-04).
#[derive(Clone, Debug, PartialEq)]
pub struct PickRow<'a> {
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
pub struct AccountRow<'a> {
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
    /// row carries a [`AccountRow::why_not`] and is never picked, so its symbol is never read.
    pub symbol: &'a str,
    /// What the row shows for the account: its label, else the broker's account number when the
    /// venue has more than one unlabelled account, else "main".
    pub name: &'a str,
    /// The account's mode. [`AccountMode::Unknown`] is drawn as unknown, never as PAPER: this is
    /// the list a trader picks where an order goes from.
    pub mode: AccountMode,
    /// `None` when the account runs on the server and can be picked. `Some(reason)` draws the row
    /// greyed, with the reason on hover; a greyed row takes no click (spec §3.3).
    pub why_not: Option<&'a str>,
}

/// A venue whose database holds no account at all: shown with a Connect button (spec §3.3).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Unconnected<'a> {
    /// The venue's own spelling, from the database's `venue.title`; the key when it has none.
    pub venue_label: &'a str,
    /// The product the window's instrument is on that venue (`Perp`), as [`AccountRow::product`].
    pub product: &'a str,
    /// The venue-native symbol the instrument is under there: the row says what Connect would open.
    pub symbol: &'a str,
}

/// How the status strip reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StatusKind {
    Info,
    Ok,
    Error,
}

/// The last thing the window did and how it came out (spec §3.8), composed by the app.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StatusLine<'a> {
    pub kind: StatusKind,
    pub text: &'a str,
}

/// Everything one frame draws. Borrowed: the app owns the storage.
#[derive(Clone, Copy, Debug)]
pub struct TradeInputs<'a> {
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
    pub mode: AccountMode,
    pub tradable: Tradable<'a>,
    pub grid: Grid,
    pub book: &'a L2Book,
    /// The window's price: the book's mid, which the bar's hover names (no last trade price
    /// reaches the desktop). `None` prints a dash.
    pub last: Option<f64>,
    pub stale: bool,
    /// The window's own depth link's status line, verbatim (empty = not known).
    pub source: &'a str,
    pub absence: Option<BookAbsence<'a>>,
    pub orders: &'a [LadderOrder],
    /// Why this account's own orders cannot be shown or cancelled here, or `None` when they can.
    /// `Some` on an older node whose venue blocks do not name each order's account (Ruling R9):
    /// the window then draws no own-order markers and words its disabled cancel buttons with this.
    /// The glue sets it to `order_dispatch::ORDERS_UNATTRIBUTED_WHY` there.
    pub orders_why: Option<&'a str>,
    pub position: Option<Position>,
    /// The account's free buying power (`VenueBlock::free_bp`); `None` when unknown.
    pub buying_power: Option<f64>,
    /// The venue's declared capabilities: drag-to-reprice needs `allows_modify`.
    pub caps: VenueCaps,
    /// Why a TP/SL bracket cannot go to this account, as the app STATES it (the `Tradable::No::why`
    /// pattern: the glue words the cause once and the ticket says it); `None` where one can. The
    /// glue's causes, in its order: an account other than its venue's single default book (Ruling R3,
    /// `order_dispatch::wire_account`), then an engine whose lane cannot hold the stop-loss (the
    /// shipped daemon's spot binance engine; `order_dispatch::bracket_lane_holds_stop`, the node's
    /// own rule). The ticket never infers a cause itself ([`ticket::tpsl_block`]).
    pub bracket_why: Option<&'a str>,
    /// Whether the backend can carry a bracket at all — `false` while the desktop's lift has no wire
    /// form for one (Ruling R8; `tradehub_control::bracket_has_wire_form`).
    pub bracket_wire: bool,
    /// The symbol picker's rows, ranked (see [`PickRow`]).
    pub matches: &'a [PickRow<'a>],
    /// The recently used instruments, newest first, as `(venue, venue-native symbol)` pairs. A
    /// symbol is only an address together with its venue: picking one moves the window to that
    /// venue as well (spec §3.3).
    pub recent: &'a [(&'a str, &'a str)],
    pub accounts: &'a [AccountRow<'a>],
    /// Why `accounts` is not the account list the server's database holds, as the app states it
    /// (the owner's decision of 10-03, item 5: "Account list unavailable." when the node did not
    /// answer the directory); the account picker shows it at the top. `None` when it is.
    pub accounts_why: Option<&'a str>,
    pub unconnected: &'a [Unconnected<'a>],
    /// The prints of the last two minutes on this window's instrument, oldest first, for the tick
    /// chart's Trades layer: the app lends them from the tape it keeps and the widget draws them
    /// (empty where the venue serves no trade stream).
    pub tape: &'a [chart::Print],
    pub status: Option<StatusLine<'a>>,
}

/// A bracket's exits, as absolute prices on the tick grid.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Exits {
    pub take_profit: f64,
    pub stop_loss: f64,
}

/// Where an order was made, so a rejection can name its gesture (`order_dispatch`'s
/// `SubmitSource`, Ruling R5).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Origin {
    /// A click on the ladder.
    Ladder,
    /// The ticket's Buy and Sell buttons.
    Ticket,
}

/// A trader intent leaving the widget. The app addresses it with the window's venue, account and
/// symbol, and maps it to `vike_exec` commands through `order_dispatch`.
#[derive(Clone, Debug, PartialEq)]
pub enum TradeAction {
    /// A new order. `price` is the limit price (Limit), the trigger (Stop), or `None` (Market).
    Place {
        side: i32,
        order_type: OrderType,
        price: Option<f64>,
        qty: f64,
        reduce_only: bool,
        exits: Option<Exits>,
        origin: Origin,
    },
    /// Reprice a resting order (drag).
    Modify { coid: String, new_price: f64 },
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
    /// venue ([`AccountRow::symbol`]), so a pick on another venue never keeps the old venue's.
    PickAccount { venue: String, account: Option<String>, symbol: String },
    /// Open the Connections window.
    OpenConnections,
    /// A line for the status strip about something the widget did; the app shows it.
    Note { kind: StatusKind, text: String },
}

/// The note the widget leaves when a window lands on a LIVE account (spec §3.11).
pub const ONE_CLICK_OFF_ON_LIVE: &str =
    "One-click trading is off on a LIVE account: every order asks first.";

/// ...and on an account whose mode the node did not report: treated as LIVE, never CALLED LIVE
/// while its chip reads `MODE ?` (final review A, minor 5).
pub const ONE_CLICK_OFF_ON_UNKNOWN: &str =
    "One-click trading is off on an account whose mode is not known: every order asks first.";

/// Cross-frame view state for one window, owned by the app.
#[derive(Clone, Debug)]
pub struct TradeState {
    pub view: View,
    /// The tick chart pane's layers.
    pub layers: chart::Layers,
    /// What the window has seen of its book, for the tick chart: forgotten when the window moves to
    /// another instrument ([`TradeState::sync`]).
    pub history: chart::History,
    pub order_type: OrderType,
    pub unit: SizeUnit,
    /// The size field's text, in [`TradeState::unit`].
    pub size: String,
    /// The price (Limit) or trigger (Stop) field's text.
    pub price: String,
    pub reduce_only: bool,
    /// The trader's TP/SL choice. Where TP/SL cannot be used (`ticket::tpsl_block`), a TICKED choice
    /// refuses the ticket's Buy and Sell until the trader turns it off: the ticket never sends an
    /// order without the exits the trader set (fix round 1, I-3). A ladder click carries no exits
    /// by design (spec §3.6), whatever it says.
    pub tpsl: bool,
    pub tp_text: String,
    pub sl_text: String,
    pub one_click: bool,
    /// Ticks per ladder row. Remembered for each instrument (venue and symbol) for the life of the
    /// window ([`TradeState::sync`] stores it when the window leaves an instrument and restores it
    /// when the window comes back), and 1 for an instrument the window has not shown yet.
    pub group: i64,
    /// Whether the ladder shows its Vol column (the toolbar's first button), beside the ticket.
    pub vol: bool,
    /// A manual ladder centre; `None` follows the book.
    pub center: Option<f64>,
    /// The price the rows are centred on while they follow the book: the mid, held until it drifts a
    /// quarter of the rows away from it ([`follow_book`]). So the rows, and the chart's on them,
    /// stand still while the mid ticks up and down a row, instead of jumping with every tick.
    pub anchor: Option<f64>,
    /// The client order ids being dragged to a new price.
    pub drag: Option<Vec<String>>,
    /// The order waiting for a confirm (one-click off).
    pub held: Option<TradeAction>,
    /// The symbol picker's search text.
    pub query: String,
    /// The highlighted row of the symbol picker.
    pub hot: usize,
    /// The instrument this state was last synced to: venue, account, symbol.
    address: Option<(String, Option<String>, String)>,
    /// The [`TradeState::group`] this window last had on each instrument it has left, keyed by
    /// `(venue, symbol)` and NOT by account: the book, and so the grouping, belongs to the venue's
    /// symbol, so another account on the same instrument keeps its group. Lives in the window's
    /// state only; it is gone when the window closes.
    groups: std::collections::HashMap<(String, String), i64>,
    /// The account mode this state was last synced to.
    mode_seen: Option<AccountMode>,
    /// The size text a sync seeded while the lot was NOT known (`0`: no lot, no size), kept until
    /// the lot is known, so the size is seeded again then unless the trader changed it.
    unknown_lot_seed: Option<String>,
    /// The position's signed size a held Close or Reverse was asked against: what its prompt names,
    /// and what the confirm must still find (final review A, minor 3). `None` for any other order.
    held_position: Option<f64>,
    /// The instrument's tick and lot this state was last synced to, as bits (a grid that is not a
    /// number stays the same grid): a held order is priced and sized on them (final review A,
    /// minor 8).
    grid_seen: Option<(u64, u64)>,
    /// Whether the ticket [`draw`] lays out is the compact one (`layout::compact_ticket`), which
    /// sends at market whatever type the trader chose. The status strip words the ticket's refusal
    /// for the order THAT ticket sends ([`TradeState::entry`]; FW6, I3).
    compact: bool,
}

impl Default for TradeState {
    fn default() -> Self {
        TradeState {
            view: View { chart: false, ladder: true, panel: Panel::Beside },
            layers: chart::Layers::default(),
            history: chart::History::default(),
            order_type: OrderType::Limit,
            unit: SizeUnit::Base,
            size: String::new(),
            price: String::new(),
            reduce_only: false,
            tpsl: false,
            tp_text: "0.5".to_string(),
            sl_text: "0.3".to_string(),
            one_click: false,
            group: 1,
            vol: false,
            center: None,
            anchor: None,
            drag: None,
            held: None,
            query: String::new(),
            hot: 0,
            address: None,
            groups: std::collections::HashMap::new(),
            mode_seen: None,
            unknown_lot_seed: None,
            held_position: None,
            grid_seen: None,
            compact: false,
        }
    }
}

impl TradeState {
    /// Bring the state in line with this frame's inputs. An instrument change clears everything that
    /// belonged to the old instrument, above all a HELD order, so a confirm can never send it to the
    /// new one (Review Focus 4). The account's mode sets one-click's starting point, and a change to
    /// LIVE turns it off again (spec §3.11).
    ///
    /// The ladder's Group is the one thing a change of address does not clear: it is remembered for
    /// each instrument, keyed by `(venue, symbol)`. The window stores the group of the instrument it
    /// leaves and restores it when it comes back, and an instrument it has not shown yet starts at
    /// 1. A change of ACCOUNT alone, on the same venue and symbol, therefore keeps the group.
    ///
    /// The size starts at the middle quick size. While the instrument's lot is not known (its
    /// catalog row has not arrived) that is `0`, which sends nothing; once the lot is known the size
    /// is seeded again, unless the trader changed it meanwhile.
    ///
    /// ⚠ An order held for a confirm never outlives what it was made under: a new address drops it
    /// silently (the window is another market), and each of these drops it with an error note, so
    /// the strip's Place can never send it: a change of the account's mode on the same address; a
    /// change of the instrument's tick or lot (final review A, minor 8); for a held Close or
    /// Reverse, a change of the position's size (minor 3); a window that can no longer trade,
    /// whose note says why; and, for an order that goes out at MARKET (a market Buy or Sell, a
    /// Reverse), no price left for the side it trades — that side of the book gone and no last
    /// price — which the click refused and the confirm must too (the owner's decision of
    /// 2026-10-03, round 2, item 10). A held Close, a limit and a stop are not checked for a price:
    /// a Close only reduces, and a limit or a stop carries its own. "Can no longer trade" is
    /// [`Tradable::No`] and nothing else: the widget has no fault or halt input of its own, so a
    /// halted account or a stopped core drops the order only because the app maps both to
    /// `Tradable::No` (its glue does: an account the server halted, or a core that stopped, takes
    /// no order from any window).
    pub fn sync(&mut self, inputs: &TradeInputs<'_>, actions: &mut Vec<TradeAction>) {
        let address = (
            inputs.venue.to_string(),
            inputs.account.map(str::to_string),
            inputs.symbol.to_string(),
        );
        // The quick sizes are all zero exactly while the lot is not known.
        let default = sizing::quick_sizes(inputs.grid)[2];
        let seed = || sizing::size_text(default, SizeUnit::Base, None, inputs.grid.lot);
        if self.address.as_ref() != Some(&address) {
            if let Some((venue, _account, symbol)) = self.address.take() {
                self.groups.insert((venue, symbol), self.group);
            }
            let instrument = (address.0.clone(), address.2.clone());
            self.address = Some(address);
            self.history.clear();
            self.held = None;
            self.drag = None;
            self.center = None;
            self.anchor = None;
            self.price.clear();
            self.group = self.groups.get(&instrument).copied().unwrap_or(1);
            self.unit = SizeUnit::Base;
            self.size = seed();
            self.unknown_lot_seed = (default <= 0.0).then(|| self.size.clone());
        } else if default > 0.0
            && let Some(seeded) = self.unknown_lot_seed.take()
            && self.unit == SizeUnit::Base
            && self.size == seeded
        {
            self.size = seed();
        }
        if self.mode_seen != Some(inputs.mode) {
            let first = self.mode_seen.is_none();
            self.mode_seen = Some(inputs.mode);
            if !first && self.held.take().is_some() {
                actions.push(TradeAction::Note {
                    kind: StatusKind::Error,
                    text: HELD_MODE.to_string(),
                });
            }
            match inputs.mode {
                AccountMode::Live | AccountMode::Unknown => {
                    if self.one_click && !first {
                        let text = if inputs.mode == AccountMode::Live {
                            ONE_CLICK_OFF_ON_LIVE
                        } else {
                            ONE_CLICK_OFF_ON_UNKNOWN
                        };
                        actions
                            .push(TradeAction::Note { kind: StatusKind::Info, text: text.into() });
                    }
                    self.one_click = false;
                }
                AccountMode::Paper | AccountMode::Demo => {
                    if first {
                        self.one_click = true;
                    }
                }
            }
        }
        // ⚠ A held order is priced on the tick and sized on the lot it was made under. A catalog
        // refresh that changes either could leave the strip naming an order the Place no longer
        // sends as named — a price off the new tick, a size off the new lot, or "Buy 0 limit" for a
        // lot no longer known — so it goes, with a note (final review A, minor 8).
        let grid = (inputs.grid.tick.to_bits(), inputs.grid.lot.to_bits());
        if self.grid_seen != Some(grid) {
            let first = self.grid_seen.is_none();
            self.grid_seen = Some(grid);
            if !first && self.held.take().is_some() {
                actions.push(TradeAction::Note {
                    kind: StatusKind::Error,
                    text: HELD_GRID.to_string(),
                });
            }
        }
        // ⚠ A held Close or Reverse is sized at the confirm from the position THEN: one held while
        // the position changed would send another size than its prompt named — a Reverse held
        // while a resting order filled sends twice the NEW position. It goes the moment the size
        // moves; a tick that moves only the P/L keeps it (final review A, minor 3).
        let size = inputs.position.map_or(0.0, |p| p.size);
        if let Some(asked) = self.held_position
            && let Some(exit @ (TradeAction::ClosePosition | TradeAction::Reverse)) = &self.held
            && size != asked
        {
            let what = if matches!(exit, TradeAction::Reverse) { "reverse" } else { "close" };
            self.held = None;
            actions.push(TradeAction::Note {
                kind: StatusKind::Error,
                text: format!(
                    "Not sent: the position changed while the {what} waited for a confirm, so it \
                     was dropped."
                ),
            });
        }
        // A window that can no longer trade drops it and says why in the words it states that
        // cause in everywhere else ([`why_untradable`]; the F wave's review, minor 4).
        if let Some(why) = why_untradable(inputs)
            && self.held.take().is_some()
        {
            actions.push(TradeAction::Note {
                kind: StatusKind::Error,
                text: format!("Not sent: the order waiting for a confirm was dropped. {why}"),
            });
        }
        // ⚠ An order that goes out at market needs a price for the side it trades — that side of
        // the book, else the last price ([`ticket::market_ref`]) — at the confirm as at the click:
        // with none it goes out blind, and the app's notional cap holds no request without a price
        // (the owner's decision of 2026-10-03, round 2, item 10). The click refuses a market Buy or
        // Sell and a Reverse with none; one HELD while the price went would still be sent by the
        // strip's Place, and on a LIVE account, or one whose mode is unknown, one-click is always
        // off, so every Reverse waits there. A Reverse trades the side that closes the position:
        // its second half opens a position the size of the one held.
        let trades = match &self.held {
            Some(TradeAction::Place { side, order_type: OrderType::Market, .. }) => Some(*side),
            Some(TradeAction::Reverse) => Some(vike_model::closing_side(size)),
            _ => None,
        };
        if let Some(side) = trades
            && ticket::market_ref(side, inputs).is_none()
        {
            self.held = None;
            actions.push(TradeAction::Note {
                kind: StatusKind::Error,
                text: format!("{HELD_NO_PRICE} {}", ticket::NO_MARKET_PRICE_WHY),
            });
        }
        if self.held.is_none() {
            self.held_position = None;
        }
    }

    /// Send `a`, or hold it for a confirm when one-click trading is off (spec §3.6).
    pub fn submit(&mut self, a: TradeAction, actions: &mut Vec<TradeAction>) {
        if self.one_click {
            actions.push(a);
        } else {
            self.held = Some(a);
            self.held_position = None;
        }
    }

    /// [`TradeState::submit`] for a Close or a Reverse, asked against a position of signed `size`:
    /// held, its prompt names that size, and the order goes if the position changes before the
    /// confirm ([`TradeState::sync`]).
    fn submit_exit(&mut self, a: TradeAction, size: f64, actions: &mut Vec<TradeAction>) {
        let holds = !self.one_click;
        self.submit(a, actions);
        if holds {
            self.held_position = Some(size);
        }
    }

    /// The order type the window's ticket sends its Buy and Sell as: market from the compact ticket,
    /// whatever the trader chose for the full one, else that choice. As of the frame `draw` laid out
    /// last, so the strip it measures before the ticket is drawn reads the ticket the frame before
    /// drew; a frame whose ticket changed measures again and is drawn again (`draw`'s discard).
    fn entry(&self) -> OrderType {
        if self.compact { OrderType::Market } else { self.order_type }
    }
}

/// The note a held order leaves when the account's mode changes under it.
const HELD_MODE: &str =
    "Not sent: the account's mode changed while the order waited for a confirm, so it was dropped.";

/// The note a held order leaves when the instrument's tick or lot changes under it.
const HELD_GRID: &str = "Not sent: the instrument's tick or lot size changed while the order \
                         waited for a confirm, so it was dropped.";

/// The note a held order that goes out at market leaves when the side it trades loses its price
/// (that side of the book and the last price both gone) while it waits. The note goes on with the
/// click's own refusal, [`ticket::NO_MARKET_PRICE_WHY`], word for word: one source for that
/// sentence.
const HELD_NO_PRICE: &str =
    "Not sent: the price for its side is gone, so the order waiting for a confirm was dropped.";

/// The words for an order, as the confirm prompt says them, with what it carries:
/// `Buy 0.010 limit @ 65,432.4, TP 65,759.6 / SL 65,236.1`, or `…, no TP/SL` (fix round 1, I-3:
/// the confirm names the exits, or says there are none).
pub fn describe(a: &TradeAction, grid: Grid) -> String {
    match describe_parts(a, grid, None) {
        (order, Some(exits)) => format!("{order}, {exits}"),
        (order, None) => order,
    }
}

/// [`describe`]'s two halves: the order (side, size, type and price), and what it carries (`TP … /
/// SL …` or `no TP/SL`, after `reduce only` for an order that is), `None` for an action that
/// carries no exits at all (Close, Reverse). The status strip prints them on two lines, so a narrow
/// strip can never cut the exits off.
///
/// A Close or a Reverse names the side and the size it sends where `position` (the signed size it
/// was asked against) is given — the side opposite the position, and its size, twice it for a
/// Reverse, as the app's dispatcher sends them from the position at the confirm (final review A,
/// minor 3) — and says only what it does where it is not.
fn describe_parts(a: &TradeAction, grid: Grid, position: Option<f64>) -> (String, Option<String>) {
    match a {
        TradeAction::Place { side, order_type, price, qty, reduce_only, exits, .. } => {
            let verb = if *side > 0 { "Buy" } else { "Sell" };
            // Never "Buy 0 limit" for a lot no longer known (final review A, minor 8).
            let q = sizing::qty_text(*qty, grid.lot);
            let order = match (order_type, price) {
                (OrderType::Market, _) | (_, None) => format!("{verb} {q} at market"),
                (OrderType::Limit, Some(p)) => {
                    format!("{verb} {q} limit @ {}", ladder::fmt_px(*p, grid.tick))
                }
                (OrderType::Stop, Some(p)) => {
                    format!("{verb} {q} stop @ {}", ladder::fmt_px(*p, grid.tick))
                }
            };
            let carries = match exits {
                Some(e) => format!(
                    "TP {} / SL {}",
                    ladder::fmt_px(e.take_profit, grid.tick),
                    ladder::fmt_px(e.stop_loss, grid.tick)
                ),
                None => "no TP/SL".to_string(),
            };
            // A held order keeps the reduce-only it was made with, whatever the box shows by the
            // time the trader confirms it: the prompt says which (final review A, minor 2).
            let carries = if *reduce_only { format!("reduce only, {carries}") } else { carries };
            (order, Some(carries))
        }
        TradeAction::ClosePosition | TradeAction::Reverse => {
            let reverse = matches!(a, TradeAction::Reverse);
            let what = if reverse { "Reverse the position" } else { "Close the position" };
            let order = match position.filter(|s| *s != 0.0 && s.is_finite()) {
                Some(s) => {
                    let verb = if s > 0.0 { "sell" } else { "buy" };
                    let q = if reverse { 2.0 * s.abs() } else { s.abs() };
                    format!("{what}: {verb} {} at market", sizing::qty_text(q, grid.lot))
                }
                None => format!("{what} at market"),
            };
            (order, None)
        }
        _ => (String::new(), None),
    }
}

/// The line an untradable symbol shows in place of the order controls (spec §4.3). The ladder and
/// the ticket both say it, so it lives here rather than in either. An EMPTY list proves nothing
/// about the symbol, so it says the account cannot trade it now — never "does not trade" (W4 fix
/// round 1, I-3; final review A, minor 6). A long list names its first two symbols and counts the
/// rest (`NAMED`; FW6, I4): `It trades ETH-PERPETUAL, SOL-PERPETUAL and 3 more.`
pub fn untradable_reason(symbol: &str, trades: &[String]) -> String {
    if trades.is_empty() {
        cannot_trade_now(symbol)
    } else {
        format!("This account does not trade {symbol}. It trades {}.", traded_list(trades))
    }
}

/// How many of an account's symbols [`untradable_reason`] names before it counts the rest: with
/// the sentence's other words, two long venue symbols hold the status strip's two lines in a
/// 280 pt window, where a whole list ran on past them (FW6, I4: the render check's 320 pt strip cut
/// it to `…It trades ETHUS…`).
const NAMED: usize = 2;

/// `trades` as a sentence names them: every one while there are at most one more than [`NAMED`]
/// ("and 1 more" says less than the name it hides), else the first [`NAMED`] and how many more.
fn traded_list(trades: &[String]) -> String {
    match trades.split_at_checked(NAMED) {
        Some((named, rest)) if rest.len() > 1 => {
            format!("{} and {} more", named.join(", "), rest.len())
        }
        _ => trades.join(", "),
    }
}

/// That the account cannot trade `symbol` now: what a refusal says with no cause stated and no
/// list that names other symbols.
fn cannot_trade_now(symbol: &str) -> String {
    format!("This account cannot trade {symbol} now.")
}

/// Why the window takes no order now, or `None` when it takes them ([`Tradable::Yes`]). Every
/// place that refuses because of [`Tradable::No`] says THIS: the ticket's line and its disabled
/// buttons, the ladder's hint and its hover, the compact ticket's line.
///
/// The cause is the app's to state, never the widget's to guess (the F wave's ruling):
/// - a cause the app states (`why`: a stopped core, a halted account, an account the server does
///   not run, …) is said in the app's words — the words its status strip uses for it;
/// - with none stated, a list that names other symbols and not this one: [`untradable_reason`],
///   which is then true — the only case that may say "does not trade";
/// - otherwise: that the account cannot trade the symbol NOW. An EMPTY list never proves "does
///   not trade" (W4 fix round 1, I-3), and a list that names the symbol cannot say it without
///   contradicting itself.
///
/// ⚠ It never reads [`TradeInputs::status`]. The strip also carries the window's own last
/// rejection, a widget note, a refused ladder click: an error there is not, by being there, the
/// reason the window takes no order (W4 fix round 2's two wrong cases, ended by the slot).
pub fn why_untradable(inputs: &TradeInputs<'_>) -> Option<String> {
    let Tradable::No { trades, why } = inputs.tradable else {
        return None;
    };
    let names_others = !trades.is_empty() && !trades.iter().any(|s| s == inputs.symbol);
    Some(match why {
        Some(why) => why.to_string(),
        None if names_others => untradable_reason(inputs.symbol, trades),
        None => cannot_trade_now(inputs.symbol),
    })
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
    ui.scope_builder(UiBuilder::new().id(key), |ui| ui.add(button)).inner
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
    ui.scope_builder(UiBuilder::new().id(key.with(salt)), add).inner
}

/// How wide a kit button with `words` is drawn: its words in the Strong role and the cell padding
/// each side (the kit's own padding). The rows that must decide BEFORE they draw whether their
/// buttons fit (the ticket's send and cancel rows, the strip's answers and its height) measure with
/// this.
fn button_w(ctx: &egui::Context, t: &Tokens, words: &str) -> f32 {
    text_w(ctx, words, &t.font(TextRole::Strong), t) + 2.0 * t.metrics.pad
}

/// How wide `words` are drawn in `font`, on one line.
fn text_w(ctx: &egui::Context, words: &str, font: &egui::FontId, t: &Tokens) -> f32 {
    ctx.fonts_mut(|f| f.layout_no_wrap(words.to_string(), font.clone(), t.theme.text)).size().x
}

/// The row height egui gives `font`: what the status strip, which lays its own lines out, counts
/// them by.
fn row_h(ctx: &egui::Context, font: &egui::FontId) -> f32 {
    ctx.fonts_mut(|f| f.row_height(font))
}

/// How tall a label of one line in `font` is drawn: its galley's height, which egui rounds to the
/// pixel grid, so it is not [`row_h`]. MEASURED at one point per pixel: mono Body at Large text has
/// a 15.84 pt row and draws a 16 pt label, mono Caption a 14.53 pt row and a 15 pt label. The
/// height a window opens at (`layout::window_size`) counts the full ticket's lines of text with
/// this, because they are labels that lay themselves out; counted by [`row_h`] the form came out
/// 1.3 pt short at Large text, and TP/SL a point below the fold.
fn label_h(ctx: &egui::Context, font: &egui::FontId, t: &Tokens) -> f32 {
    ctx.fonts_mut(|f| f.layout_no_wrap("0".to_string(), font.clone(), t.theme.text)).size().y
}

/// Draw one frame into all of `ui`'s free space and return this frame's intents: the instrument
/// bar on top, the status strip at the bottom, and between them the ladder and the ticket as
/// [`layout::split`] places them. Each region is a clipped child with an id of its own, so nothing
/// in one can spill onto, or be clicked in, another (the DOM's strip rule), and a field keeps its
/// focus when the ladder is shown or hidden.
pub fn draw(
    ui: &mut egui::Ui,
    state: &mut TradeState,
    inputs: &TradeInputs<'_>,
) -> Vec<TradeAction> {
    let mut actions = Vec::new();
    state.sync(inputs, &mut actions);
    state.history.observe(vike_model::now_ms(), inputs.book);
    let t = Tokens::of(ui.ctx());
    let full = ui.available_rect_before_wrap();
    ui.allocate_rect(full, egui::Sense::hover());
    let bar_h = instrument::height_of(instrument::rows_for(ui.ctx(), &t, inputs, full.width()), &t);
    let bar = Rect::from_min_size(full.min, vec2(full.width(), bar_h));
    let strip_h = status::height(ui.ctx(), &t, state, inputs, full.width());
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
    instrument::bar(bar_ui, &t, state, inputs, &mut actions);
    let rects = layout::split(body, state.view, &t.metrics, layout::ticket_w(&t));
    state.compact = layout::compact_ticket(state.view, &rects);
    let rows_h = match (rects.ladder, rects.chart) {
        (Some(l), _) => ladder::rows_region(l, &t, state, inputs).map(|r| r.height()),
        (None, Some(c)) => Some(chart::own_rows(c, &t).height()),
        (None, None) => None,
    };
    if let Some(h) = rows_h {
        follow_book(state, inputs, ladder::row_count(h, t.metrics.row_h));
    }
    rules(ui, &t, bar, body, &rects);
    if let Some(r) = rects.chart {
        // The chart's rows are the ladder's: its own rect where one is drawn, else the pane's.
        let region_of_rows = match rects.ladder {
            Some(l) => ladder::rows_region(l, &t, state, inputs),
            None => Some(chart::own_rows(r, &t)),
        };
        let frame = region_of_rows.and_then(|reg| chart::RowFrame::of(reg, &t, state, inputs));
        let chart_ui = &mut region(ui, r, "trade_chart", REGION_CHART);
        chart::draw(chart_ui, &t, state, inputs, frame);
    }
    if let Some(r) = rects.ladder {
        let ladder_ui = &mut region(ui, r, "trade_ladder", REGION_LADDER);
        ladder::draw(ladder_ui, &t, state, inputs, &mut actions);
    }
    let mut ticket_ui = region(ui, rects.ticket, "trade_ticket", REGION_TICKET);
    if state.compact {
        ticket::compact(&mut ticket_ui, &t, state, inputs, &mut actions);
    } else {
        ticket::full(&mut ticket_ui, &t, state, inputs, &mut actions);
    }
    // The strip's hairline: the design's status row sits under a rule across the window.
    ui.painter().hline(
        strip.x_range(),
        strip.min.y + stroke::HAIRLINE / 2.0,
        egui::Stroke::new(stroke::HAIRLINE, t.theme.border),
    );
    let strip_ui = &mut region(ui, strip, "trade_status", REGION_STRIP);
    status::strip(strip_ui, &t, state, inputs, &mut actions);
    // A click this pass can hold an order (or let one go) whose prompt needs another strip height
    // than the one this pass laid out before it saw the click (`status::height`, above). The pass
    // is DISCARDED and drawn again at once, with the strip the prompt needs, so no frame shows the
    // prompt in the old strip (W4 fix round 2, N3). The click is not seen twice: egui hands the
    // second pass no input events. (egui draws the frame after any frame with input regardless,
    // so without this the cut prompt showed for exactly one frame; MEASURED by the kill proof.)
    // The second pass is always there to take: input reaches a frame's FIRST pass alone, so the
    // click, and this request with it, are always in the first pass, and egui grants a discard
    // while a pass is left (`Options::max_passes`, 2 by default; this app sets none). Only a
    // `max_passes` of 1 would bring the one cut frame back.
    if status::height(ui.ctx(), &t, state, inputs, full.width()) != strip_h {
        ui.ctx().request_discard("the held prompt needs another strip height");
    }
    actions
}

/// Keep [`TradeState::anchor`] on the book without chasing it: the mid becomes the anchor when there is
/// none or when the mid has drifted more than a quarter of the visible rows (at least two) from it.
/// A ladder centred on every tick of the mid jumps a row each time the price does, and the chart on
/// its rows redraws with it.
fn follow_book(state: &mut TradeState, inputs: &TradeInputs<'_>, n_rows: usize) {
    let mid = ladder::book_centre(inputs);
    if !mid.is_finite() || mid <= 0.0 {
        return;
    }
    let row = ladder::row_tick(inputs.grid.tick, inputs.book.tick_size) * state.group.max(1) as f64;
    let band = (n_rows as f64 / 4.0).max(2.0) * row;
    state.anchor = match state.anchor {
        Some(a) if (mid - a).abs() <= band => Some(a),
        _ => Some(mid),
    };
}

/// The hairlines the design draws between its regions: under the instrument bar, between the chart and
/// the ladder, and between the ladder and the ticket beside it. (The compact ticket under them draws
/// its own top rule.)
fn rules(ui: &egui::Ui, t: &Tokens, bar: Rect, body: Rect, rects: &layout::BodyRects) {
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
fn state_is_beside(rects: &layout::BodyRects, body: Rect) -> bool {
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
        UiBuilder::new().id(ui.id().with(salt)).max_rect(rect).layout(Layout::top_down(Align::Min)),
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
pub fn view_controls(ui: &mut egui::Ui, state: &mut TradeState) -> Option<Vec2> {
    let before = state.view;
    let t = Tokens::of(ui.ctx());
    ui.spacing_mut().item_spacing.x = layout::VIEW_GAP;
    let button = |icon, tip| IconButton::new(icon, tip).sized(trade::VIEW_BUTTON, TextRole::Body);
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
        vec2(stroke::HAIRLINE, vike_ui_theme::chrome::SEPARATOR_H),
        egui::Sense::hover(),
    );
    ui.painter().rect_filled(sep, 0.0, t.theme.border);
    ui.add_space(trade::VIEW_GROUP_GAP - layout::VIEW_GAP);
    for (panel, icon, tip) in [
        (Panel::Beside, icons::PANEL_BESIDE, "Ticket beside the ladder"),
        (Panel::Under, icons::PANEL_UNDER, "Ticket under the ladder"),
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

#[cfg(test)]
mod tests {
    use super::*;

    const BTC: Grid = Grid { tick: 0.1, lot: 0.001, min_qty: 0.001 };

    fn inputs<'a>(
        book: &'a L2Book,
        account: Option<&'a str>,
        mode: AccountMode,
    ) -> TradeInputs<'a> {
        TradeInputs {
            venue: "binance",
            venue_label: "Binance",
            product: "",
            account,
            symbol: "BTCUSDT",
            base: "BTC",
            quote: "USDT",
            mode,
            tradable: Tradable::Yes,
            grid: BTC,
            book,
            last: None,
            stale: false,
            source: "",
            absence: None,
            orders: &[],
            orders_why: None,
            position: None,
            buying_power: None,
            caps: VenueCaps::UNSUPPORTED,
            bracket_why: account.map(|_| ticket::TPSL_ACCOUNT_WHY),
            bracket_wire: false,
            matches: &[],
            recent: &[],
            accounts: &[],
            accounts_why: None,
            unconnected: &[],
            tape: &[],
            status: None,
        }
    }

    /// The line the ladder and the ticket both show for a symbol the account does not trade: one
    /// function, so the two can never word it differently (spec §4.3). An EMPTY list proves nothing
    /// about the symbol, so it says the account cannot trade it now — never "does not trade", the
    /// words W4 fix round 1 (I-3) took out of every other place (final review A, minor 6).
    #[test]
    fn the_untradable_reason_names_what_the_account_does_trade() {
        let trades = ["ETHUSDT".to_string(), "SOLUSDT".to_string()];
        assert_eq!(
            untradable_reason("BTCUSDT", &trades),
            "This account does not trade BTCUSDT. It trades ETHUSDT, SOLUSDT."
        );
        assert_eq!(untradable_reason("BTCUSDT", &[]), "This account cannot trade BTCUSDT now.");
    }

    /// FW6, I4: an account that trades many symbols names the first two and counts the rest, so the
    /// line holds the status strip's two lines in a narrow window rather than running on; three or
    /// fewer are all named ("and 1 more" says less than the name it hides). Every place that says
    /// it reads this one function ([`why_untradable`]), so they shorten it alike.
    #[test]
    fn a_long_list_of_traded_symbols_names_two_and_counts_the_rest() {
        let names = |n: usize| -> Vec<String> {
            ["ETH-PERPETUAL", "SOL-PERPETUAL", "XRP-PERPETUAL", "DOGE-PERPETUAL", "ADA-PERPETUAL"]
                [..n]
                .iter()
                .map(|s| s.to_string())
                .collect()
        };
        let says = |n| untradable_reason("BTCUSDT", &names(n));
        let lead = "This account does not trade BTCUSDT. It trades";
        assert_eq!(says(5), format!("{lead} ETH-PERPETUAL, SOL-PERPETUAL and 3 more."));
        assert_eq!(says(4), format!("{lead} ETH-PERPETUAL, SOL-PERPETUAL and 2 more."));
        assert_eq!(
            says(3),
            format!("{lead} ETH-PERPETUAL, SOL-PERPETUAL, XRP-PERPETUAL."),
            "CONTROL: three are all named"
        );
    }

    /// W3 review hand-off 3, the pure half, as the F wave's reason slot left it: what a window
    /// that takes no order says. A cause the app STATES (`Tradable::No::why`: a stopped core, a
    /// halted account, …) is said in the app's words, whatever the account's list holds. With none
    /// stated, a list naming OTHER symbols and not this one says the account does not trade it —
    /// the one case that is true; any other list, an empty one included, says it cannot trade it
    /// NOW — never "does not trade BTCUSDT. It trades BTCUSDT.", and never "does not trade" on an
    /// empty list (W4 fix round 1, I-3). The status strip is never read for the cause: an Error or
    /// an Info line there changes nothing (the F wave's ruling).
    #[test]
    fn why_untradable_names_the_cause_and_never_contradicts_itself() {
        fn why<'a>(
            book: &'a L2Book,
            trades: &'a [String],
            stated: Option<&'a str>,
            status: Option<StatusLine<'a>>,
        ) -> Option<String> {
            why_untradable(&TradeInputs {
                tradable: Tradable::No { trades, why: stated },
                status,
                ..inputs(book, None, AccountMode::Live)
            })
        }
        let book = L2Book::new(0.1);
        let (btc, eth) = (["BTCUSDT".to_string()], ["ETHUSDT".to_string()]);
        let stopped = "The server has stopped trading: a handler panicked.";
        let error = Some(StatusLine { kind: StatusKind::Error, text: stopped });
        let info = Some(StatusLine { kind: StatusKind::Info, text: "Waiting for the node." });
        assert_eq!(why_untradable(&inputs(&book, None, AccountMode::Live)), None, "it trades");
        // A cause the app states is said as stated, whatever the account's list holds.
        assert_eq!(why(&book, &btc, Some(stopped), error).as_deref(), Some(stopped), "a fault");
        assert_eq!(why(&book, &[], Some(stopped), None).as_deref(), Some(stopped), "empty, fault");
        assert_eq!(why(&book, &eth, Some(stopped), None).as_deref(), Some(stopped), "other list");
        let now = "This account cannot trade BTCUSDT now.";
        assert_eq!(why(&book, &btc, None, None).as_deref(), Some(now));
        assert_eq!(why(&book, &btc, None, info).as_deref(), Some(now), "an Info line is no cause");
        assert_eq!(why(&book, &eth, None, error), Some(untradable_reason("BTCUSDT", &eth)));
        assert_eq!(
            why(&book, &eth, None, None).as_deref(),
            Some("This account does not trade BTCUSDT. It trades ETHUSDT."),
            "the genuine case, naming what it trades once"
        );
        // W4 fix round 1, I-3: an EMPTY list never proves "does not trade" — on the app it means
        // the account has no block at all (not running, the first snapshot not here yet).
        assert_eq!(why(&book, &[], None, None).as_deref(), Some(now), "an empty list");
        assert_eq!(
            why(&book, &[], None, info).as_deref(),
            Some(now),
            "an empty list, an Info strip"
        );
    }

    /// The F wave's reason slot: the widget never reads the status strip for the cause. W4 fix
    /// round 2's two wrong cases — an EMPTY list, and a list that NAMES the symbol, each with an
    /// UNRELATED error on the strip (the window's own last rejection, a widget note, the ladder's
    /// own size refusal) — showed that error as "the cause". With no stated cause they say the
    /// account cannot trade the symbol now; with one, they say THAT, never the strip's error.
    #[test]
    fn an_unrelated_error_on_the_strip_is_never_the_cause() {
        let book = L2Book::new(0.1);
        let btc = ["BTCUSDT".to_string()];
        let now = "This account cannot trade BTCUSDT now.";
        let not_running = "Not running on the server.";
        for unrelated in [
            "Rejected by the venue: insufficient margin.",
            "Not sent: the account's mode changed while the order waited for a confirm, so it \
             was dropped.",
            "Enter a size of at least one lot (0.001 BTC).",
        ] {
            let status = Some(StatusLine { kind: StatusKind::Error, text: unrelated });
            for (what, trades, stated, want) in [
                ("an empty list", &[][..], None, now),
                ("a list naming the symbol", &btc[..], None, now),
                ("an empty list, a stated cause", &[][..], Some(not_running), not_running),
                (
                    "a list naming the symbol, a stated cause",
                    &btc[..],
                    Some(not_running),
                    not_running,
                ),
            ] {
                let got = why_untradable(&TradeInputs {
                    tradable: Tradable::No { trades, why: stated },
                    status,
                    ..inputs(&book, None, AccountMode::Live)
                });
                assert_eq!(got.as_deref(), Some(want), "{what}, with {unrelated:?} on the strip");
            }
        }
    }

    /// One-click starts on for PAPER and DEMO and off for LIVE and unknown; switching a window from
    /// PAPER to LIVE turns it off again and says so, once (spec §3.11).
    #[test]
    fn one_click_follows_the_accounts_mode_and_a_switch_to_live_says_so() {
        let book = L2Book::new(0.1);
        for (mode, on) in [
            (AccountMode::Paper, true),
            (AccountMode::Demo, true),
            (AccountMode::Live, false),
            (AccountMode::Unknown, false),
        ] {
            let (mut s, mut acts) = (TradeState::default(), Vec::new());
            s.sync(&inputs(&book, None, mode), &mut acts);
            assert_eq!(s.one_click, on, "{mode:?}");
            assert!(acts.is_empty(), "{mode:?}: a window's first mode leaves no note");
        }
        let (mut s, mut acts) = (TradeState::default(), Vec::new());
        s.sync(&inputs(&book, None, AccountMode::Paper), &mut acts);
        s.sync(&inputs(&book, Some("MAIN"), AccountMode::Live), &mut acts);
        assert!(!s.one_click, "LIVE turns one-click off");
        assert_eq!(
            acts,
            [TradeAction::Note { kind: StatusKind::Info, text: ONE_CLICK_OFF_ON_LIVE.to_string() }]
        );
        s.sync(&inputs(&book, Some("MAIN"), AccountMode::Live), &mut acts);
        assert_eq!(acts.len(), 1, "an unchanged mode says nothing more");
    }

    /// An instrument change drops a held order and a drag, so neither can reach the new instrument
    /// (Review Focus 4).
    #[test]
    fn an_instrument_change_drops_the_held_order() {
        let book = L2Book::new(0.1);
        let (mut s, mut acts) = (TradeState::default(), Vec::new());
        s.sync(&inputs(&book, None, AccountMode::Live), &mut acts);
        s.held = Some(TradeAction::CancelAll);
        s.drag = Some(vec!["c1".to_string()]);
        s.sync(&inputs(&book, None, AccountMode::Live), &mut acts);
        assert!(s.held.is_some(), "the same instrument keeps it");
        s.sync(&inputs(&book, Some("SUB"), AccountMode::Live), &mut acts);
        assert_eq!((s.held, s.drag), (None, None));
    }

    /// The ladder's Group is remembered for each instrument, keyed by `(venue, symbol)` (the
    /// owner's pick B of 2026-10-05): leaving BTCUSDT at 10 for ETHUSDT starts ETHUSDT at 1, and
    /// coming back finds BTCUSDT at 10 and ETHUSDT at what it was left at. The book, and so the
    /// grouping, is per venue and symbol, so another ACCOUNT on the same instrument keeps it.
    #[test]
    fn the_group_is_remembered_for_each_instrument_and_survives_an_account_change() {
        let book = L2Book::new(0.1);
        let on = |symbol, base, account| TradeInputs {
            symbol,
            base,
            ..inputs(&book, account, AccountMode::Paper)
        };
        let (mut s, mut acts) = (TradeState::default(), Vec::new());
        s.sync(&on("BTCUSDT", "BTC", None), &mut acts);
        assert_eq!(s.group, 1, "an instrument not yet seen starts at 1");
        s.group = 10;
        s.sync(&on("ETHUSDT", "ETH", None), &mut acts);
        assert_eq!(s.group, 1, "another instrument, not yet seen, starts at 1");
        s.group = 5;
        s.sync(&on("BTCUSDT", "BTC", None), &mut acts);
        assert_eq!(s.group, 10, "back on the first instrument: what it was left at");
        s.sync(&on("ETHUSDT", "ETH", None), &mut acts);
        assert_eq!(s.group, 5, "back on the second: what it was left at");
        s.sync(&on("BTCUSDT", "BTC", Some("SUB")), &mut acts);
        assert_eq!(s.group, 10, "another account on the same instrument keeps the group");
    }

    /// W1 review fix 1b: a window opened before its catalog row arrived holds the size `0` — no
    /// lot, so no size — and once the lot is known it holds the default, the middle quick size,
    /// unless the trader typed one meanwhile. A lot that was known from the start never re-seeds.
    #[test]
    fn the_size_is_seeded_again_when_the_lot_becomes_known() {
        let book = L2Book::new(0.1);
        let unknown = Grid { tick: 0.1, lot: 0.0, min_qty: 0.0 };
        let on = |grid| TradeInputs { grid, ..inputs(&book, None, AccountMode::Demo) };
        let (mut s, mut acts) = (TradeState::default(), Vec::new());
        s.sync(&on(unknown), &mut acts);
        s.sync(&on(unknown), &mut acts);
        assert_eq!(s.size, "0", "no lot, no size");
        s.sync(&on(BTC), &mut acts);
        assert_eq!(s.size, "0.010", "the default size, once the lot is known");
        s.size.clear();
        s.sync(&on(BTC), &mut acts);
        assert_eq!(s.size, "", "a size cleared under a known lot stays cleared");

        let (mut s, mut acts) = (TradeState::default(), Vec::new());
        s.sync(&on(unknown), &mut acts);
        s.size = "0.5".to_string();
        s.sync(&on(BTC), &mut acts);
        assert_eq!(s.size, "0.5", "a size the trader typed is kept");
    }

    /// Fix round 1, I-1: an order held for a confirm never crosses a change of the account's mode
    /// on the SAME address (a PAPER account restarted LIVE, an older node's unknown mode becoming
    /// known): it is dropped, and the window says so. An address change already drops it.
    #[test]
    fn a_held_order_does_not_cross_a_change_of_mode() {
        let book = L2Book::new(0.1);
        let (mut s, mut acts) = (TradeState::default(), Vec::new());
        s.sync(&inputs(&book, None, AccountMode::Paper), &mut acts);
        s.held = Some(TradeAction::CancelAll);
        s.sync(&inputs(&book, None, AccountMode::Paper), &mut acts);
        assert!(s.held.is_some(), "the same mode keeps it");
        s.sync(&inputs(&book, None, AccountMode::Live), &mut acts);
        assert_eq!(s.held, None);
        assert!(
            acts.iter().any(|a| matches!(
                a,
                TradeAction::Note { kind: StatusKind::Error, text } if text.starts_with("Not sent:")
            )),
            "{acts:?}"
        );
    }

    /// Fix round 1, I-3: the confirm prompt says what the held order carries, its exits or none.
    #[test]
    fn the_confirm_words_name_the_exits_or_say_there_are_none() {
        let place = |exits| TradeAction::Place {
            side: 1,
            order_type: OrderType::Limit,
            price: Some(100.0),
            qty: 0.01,
            reduce_only: false,
            exits,
            origin: Origin::Ticket,
        };
        let bracket = Exits { take_profit: 105.0, stop_loss: 99.0 };
        assert_eq!(
            describe(&place(Some(bracket)), BTC),
            "Buy 0.010 limit @ 100.0, TP 105.0 / SL 99.0"
        );
        assert_eq!(describe(&place(None), BTC), "Buy 0.010 limit @ 100.0, no TP/SL");
    }

    /// Final review A, minor 2: the confirm words say "reduce only" when the held order is: it keeps
    /// the flag it was made with, whatever the box shows by the time the trader confirms it.
    #[test]
    fn the_confirm_words_say_reduce_only_when_the_order_is() {
        let place = |reduce_only| TradeAction::Place {
            side: -1,
            order_type: OrderType::Limit,
            price: Some(100.0),
            qty: 0.01,
            reduce_only,
            exits: None,
            origin: Origin::Ladder,
        };
        assert_eq!(describe(&place(true), BTC), "Sell 0.010 limit @ 100.0, reduce only, no TP/SL");
        assert_eq!(describe(&place(false), BTC), "Sell 0.010 limit @ 100.0, no TP/SL", "CONTROL");
    }

    /// Final review A, minor 8: a size on a lot that is not known prints to its own decimals, never
    /// as `0` ("Buy 0 limit" for an order of 0.01).
    #[test]
    fn a_size_on_a_lot_not_known_is_never_printed_as_zero() {
        let place = TradeAction::Place {
            side: 1,
            order_type: OrderType::Limit,
            price: Some(100.0),
            qty: 0.01,
            reduce_only: false,
            exits: None,
            origin: Origin::Ticket,
        };
        let unknown = Grid { lot: 0.0, ..BTC };
        assert_eq!(describe(&place, unknown), "Buy 0.01 limit @ 100.0, no TP/SL");
    }

    /// Final review A, minor 3: a held Close or Reverse names the side and the size it sends — the
    /// side opposite the position, the position's size, twice it for a Reverse, as the app's
    /// dispatcher sends them — from the position it was asked against. A position nobody gave
    /// keeps the plain words.
    #[test]
    fn a_held_close_or_reverse_names_the_side_and_size_it_sends() {
        let words = |a: TradeAction, position| {
            let (order, carries) = describe_parts(&a, BTC, position);
            assert_eq!(carries, None, "{a:?}: it carries no exits");
            order
        };
        assert_eq!(
            words(TradeAction::ClosePosition, Some(0.05)),
            "Close the position: sell 0.050 at market"
        );
        assert_eq!(
            words(TradeAction::Reverse, Some(0.05)),
            "Reverse the position: sell 0.100 at market"
        );
        assert_eq!(
            words(TradeAction::ClosePosition, Some(-0.2)),
            "Close the position: buy 0.200 at market"
        );
        assert_eq!(
            words(TradeAction::Reverse, Some(-0.2)),
            "Reverse the position: buy 0.400 at market"
        );
        assert_eq!(words(TradeAction::ClosePosition, None), "Close the position at market");
    }

    /// The position a sync sees, with `upnl` free to tick, and a last price, so a held Reverse has
    /// the price it needs and is dropped for nothing but what a test changes.
    fn holding<'a>(book: &'a L2Book, size: f64, upnl: f64) -> TradeInputs<'a> {
        TradeInputs {
            position: Some(Position { size, avg_px: 100.0, upnl }),
            last: Some(100.0),
            ..inputs(book, None, AccountMode::Live)
        }
    }

    /// The one error note `acts` holds, its text.
    fn one_error(acts: &[TradeAction]) -> &str {
        match acts {
            [TradeAction::Note { kind: StatusKind::Error, text }] => text,
            other => panic!("one error note, got {other:?}"),
        }
    }

    /// Final review A, minor 3 (MONEY PATH): a held Close or Reverse is sized at the confirm from the
    /// position THEN, so one held while the position changed would send another size than its
    /// prompt named — a Reverse held while a resting order filled sends twice the NEW position. It
    /// is dropped, with a note, the moment the position's size changes. CONTROL: a tick of the
    /// market moves the P/L and not the size, and keeps it.
    #[test]
    fn a_held_close_or_reverse_is_dropped_when_the_position_changes() {
        let book = L2Book::new(0.1);
        // A flip to the other side at the SAME size is a change too (the FW1 review): a Close held
        // against a long would sell into the new short, a Reverse would double it.
        for (what, now) in [("the position grew under it", 0.08), ("it flipped side", -0.05)] {
            for exit in [TradeAction::ClosePosition, TradeAction::Reverse] {
                let (mut s, mut acts) = (TradeState::default(), Vec::new());
                s.sync(&holding(&book, 0.05, 0.0), &mut acts);
                s.held = Some(exit.clone());
                s.held_position = Some(0.05);
                s.sync(&holding(&book, 0.05, 3.0), &mut acts);
                assert_eq!(s.held.as_ref(), Some(&exit), "CONTROL: a tick keeps it");
                assert!(acts.is_empty(), "{acts:?}");
                s.sync(&holding(&book, now, 3.0), &mut acts);
                assert_eq!(s.held, None, "{exit:?}: {what}");
                let text = one_error(&acts);
                assert!(text.starts_with("Not sent:") && text.contains("position"), "{text}");
            }
        }
    }

    /// Final review A, minor 8 (MONEY PATH): a held order is sized on the lot and priced on the
    /// tick it was made under. A catalog refresh that changes either drops it, with a note, before
    /// a Place can send a size off the new lot or a price off the new tick — or a prompt can read
    /// "Buy 0 limit" for a lot no longer known. CONTROL: the same grid keeps it.
    #[test]
    fn a_held_order_does_not_cross_a_change_of_grid() {
        let book = L2Book::new(0.1);
        let on = |grid| TradeInputs { grid, ..inputs(&book, None, AccountMode::Live) };
        let held = TradeAction::Place {
            side: 1,
            order_type: OrderType::Limit,
            price: Some(100.0),
            qty: 0.01,
            reduce_only: false,
            exits: None,
            origin: Origin::Ticket,
        };
        for (what, grid) in [
            ("a new lot", Grid { lot: 0.01, ..BTC }),
            ("a new tick", Grid { tick: 0.5, ..BTC }),
            ("a lot no longer known", Grid { lot: 0.0, ..BTC }),
        ] {
            let (mut s, mut acts) = (TradeState::default(), Vec::new());
            s.sync(&on(BTC), &mut acts);
            s.held = Some(held.clone());
            s.sync(&on(BTC), &mut acts);
            assert!(s.held.is_some(), "{what}: CONTROL: the same grid keeps it");
            assert!(acts.is_empty(), "{what}: {acts:?}");
            s.sync(&on(grid), &mut acts);
            assert_eq!(s.held, None, "{what}");
            assert!(one_error(&acts).starts_with("Not sent:"), "{what}");
        }
    }

    /// Final review A, minor 5: a window that lands on an account whose mode the node did not
    /// report turns one-click off (it is treated as LIVE) and says so in its own words: it never
    /// calls the account LIVE while its chip reads `MODE ?`.
    #[test]
    fn one_click_off_on_an_unknown_mode_never_calls_the_account_live() {
        let book = L2Book::new(0.1);
        let (mut s, mut acts) = (TradeState::default(), Vec::new());
        s.sync(&inputs(&book, None, AccountMode::Paper), &mut acts);
        assert!(s.one_click, "CONTROL: PAPER starts with it on");
        s.sync(&inputs(&book, None, AccountMode::Unknown), &mut acts);
        assert!(!s.one_click, "an unknown mode turns it off");
        match acts.as_slice() {
            [TradeAction::Note { kind: StatusKind::Info, text }] => {
                assert!(!text.contains("LIVE") && text.contains("not known"), "{text}");
            }
            other => panic!("one note, got {other:?}"),
        }
    }

    /// F2 review minor 4: an order dropped because the window can no longer trade says WHY, in the
    /// words the window states the cause in everywhere else (`why_untradable`) — not a generic
    /// "cannot trade" over a cause the app stated.
    #[test]
    fn a_held_order_dropped_because_the_window_cannot_trade_says_the_stated_cause() {
        let book = L2Book::new(0.1);
        let stopped = "The server has stopped trading: a handler panicked.";
        let (mut s, mut acts) = (TradeState::default(), Vec::new());
        s.sync(&inputs(&book, None, AccountMode::Live), &mut acts);
        s.held = Some(TradeAction::CancelAll);
        let faulted = TradeInputs {
            tradable: Tradable::No { trades: &[], why: Some(stopped) },
            ..inputs(&book, None, AccountMode::Live)
        };
        s.sync(&faulted, &mut acts);
        assert_eq!(s.held, None);
        let text = one_error(&acts);
        assert!(text.starts_with("Not sent:") && text.contains(stopped), "{text}");
    }

    /// The FW4 dispatch (MONEY PATH, the owner's round-2 item 10, held half): an order waiting for
    /// its confirm that goes out at market is kept only while the side it trades has a price — the
    /// ask for a buy, the bid for a sell, else the last price. A held Reverse trades the side that
    /// closes the position (a buy for a short, a sell for a long). On a book that has only the
    /// OTHER side and no last price it is dropped with a note, "Not sent: …" ending in
    /// `ticket::NO_MARKET_PRICE_WHY`; CONTROLS: the side it trades keeps it, and so does a last
    /// price over an empty book.
    #[test]
    fn a_held_reverse_or_market_order_goes_when_the_side_it_trades_has_no_price() {
        let one_sided = |bids: &[vike_model::BookLevel], asks: &[vike_model::BookLevel]| {
            let mut b = L2Book::new(0.1);
            b.apply_snapshot(1, bids, asks);
            b
        };
        let bids = one_sided(&[vike_model::BookLevel::new(99.9, 1.0)], &[]);
        let asks = one_sided(&[], &[vike_model::BookLevel::new(100.0, 1.0)]);
        let empty = L2Book::new(0.1);
        let market = |side| TradeAction::Place {
            side,
            order_type: OrderType::Market,
            price: None,
            qty: 0.01,
            reduce_only: false,
            exits: None,
            origin: Origin::Ticket,
        };
        // What is held, the position, the book its side prices it on, the book that has only the
        // other side.
        for (what, held, size, priced, blind) in [
            ("a Reverse of a short buys", TradeAction::Reverse, -0.05, &asks, &bids),
            ("a Reverse of a long sells", TradeAction::Reverse, 0.05, &bids, &asks),
            ("a market buy", market(1), 0.05, &asks, &bids),
            ("a market sell", market(-1), 0.05, &bids, &asks),
        ] {
            let at = |book, last| TradeInputs {
                position: Some(Position { size, avg_px: 100.0, upnl: 0.0 }),
                last,
                ..inputs(book, None, AccountMode::Live)
            };
            for (case, on, kept) in [
                ("CONTROL: its side is priced", at(priced, None), true),
                ("CONTROL: a last price alone", at(&empty, Some(100.0)), true),
                ("only the other side is priced", at(blind, None), false),
            ] {
                let (mut s, mut acts) = (TradeState::default(), Vec::new());
                s.sync(&on, &mut acts);
                s.held = Some(held.clone());
                s.held_position = matches!(held, TradeAction::Reverse).then_some(size);
                s.sync(&on, &mut acts);
                if kept {
                    assert_eq!(s.held.as_ref(), Some(&held), "{what}, {case}");
                    assert!(acts.is_empty(), "{what}, {case}: {acts:?}");
                } else {
                    assert_eq!(s.held, None, "{what}, {case}");
                    let text = one_error(&acts);
                    assert!(
                        text.starts_with("Not sent:")
                            && text.ends_with(ticket::NO_MARKET_PRICE_WHY),
                        "{what}, {case}: {text}"
                    );
                }
            }
        }
    }

    /// The FW4 review: where a held order's window stops trading AND the side it trades loses its
    /// price in the same frame, the cause the app STATES wins. One note, which says it; never the
    /// price note. `sync` checks the window before the price, and this pins that order.
    #[test]
    fn a_held_order_whose_window_stops_trading_as_its_price_goes_says_the_stated_cause() {
        const WHY: &str = "This account is halted on the server.";
        let empty = L2Book::new(0.1);
        let held = TradeAction::Place {
            side: 1,
            order_type: OrderType::Market,
            price: None,
            qty: 0.01,
            reduce_only: false,
            exits: None,
            origin: Origin::Ticket,
        };
        let (mut s, mut acts) = (TradeState::default(), Vec::new());
        let trading = TradeInputs { last: Some(100.0), ..inputs(&empty, None, AccountMode::Live) };
        s.sync(&trading, &mut acts);
        s.held = Some(held.clone());
        s.sync(&trading, &mut acts);
        assert_eq!(s.held, Some(held), "CONTROL: priced and trading, it waits");
        assert!(acts.is_empty(), "{acts:?}");
        let stopped = TradeInputs {
            tradable: Tradable::No { trades: &[], why: Some(WHY) },
            ..inputs(&empty, None, AccountMode::Live)
        };
        s.sync(&stopped, &mut acts);
        assert_eq!(s.held, None, "dropped");
        let text = one_error(&acts);
        assert!(text.contains(WHY), "the stated cause: {text}");
        assert!(!text.starts_with(HELD_NO_PRICE), "not the price note: {text}");
    }
}
