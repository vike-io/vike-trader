//! The Trade window's SHELL half: what the desktop's window loop does with a Trade window once the
//! widget has drawn (the Trade window plan's Task 19, pre-flight finding C2).
//!
//! # Why it is here and not in the shell
//! `vike-desktop` is outside every CI roster lane: the `app-check` job compiles and clippy-gates it
//! and runs none of its logic, and `crates/vike-ops/tests/ci_excluded_gui_shell_ratchet.rs`'s
//! `the_gui_shell_does_not_grow` refuses to let that crate grow. Everything the window loop decides
//! about a Trade window is therefore decided here, where it is tested, and
//! `crates/vike-desktop/src/app_ui.rs`'s `draw_windows` keeps the calls: [`directory_reply`],
//! [`directory_unavailable`] and the control link (`super::ControlLink::of`) read once before the
//! window loop, [`TradeFrame::drain`] per window inside it, then [`forget_held_of_closed_windows`],
//! [`route_rejects`] (or [`refuse_unsent`]), [`directory_due`] and [`spawn_directory_fetch`] after
//! it; `App::apply_backend_action` calls [`on_backend_switch`].
//!
//! # ⚠ The address is the one the window DREW, and a text that is not a label sends nothing
//! A window's order intents are addressed with `order_dispatch::TradeAddress::parse` over the venue,
//! account text and symbol the widget was drawn with, computed ONCE before the intents are drained:
//! a pick earlier in the same frame moves the window, never an order the trader placed before it.
//! When the account text is not an account label, every order intent of the window is DROPPED and
//! its strip says why ([`UNADDRESSABLE_WHY`]). It is never read as `None`: `None` is the venue's
//! DEFAULT account, so the `.ok()` habit would place the order on a different book.
//!
//! # What reaches a window's strip, and what does not (Ruling R8(3), pre-flight I7)
//! The strip shows what the PLANNER refused ([`route_rejects`]: the window's venue, symbol AND
//! account must match, minor 31) — a TP/SL bracket on an engine whose lane cannot hold its
//! stop-loss included, which the node would refuse too — that nothing was sent from a desktop with
//! no control channel ([`refuse_unsent`]), and each order's state from the snapshot (the glue's
//! `follow_status`). Before any click the window already says why it takes no order at all, a
//! read-only desktop and a lost control link among the causes (the glue's `refusal`).
//!
//! Nothing later in the send reaches the strip, because the shell's `Dispatch::send` returns nothing
//! a window could be matched by. Every refusal after the planner is logged and LATCHED on the
//! control handle by `crate::backend::tradehub_control::send_to_backend`, and shows in the status
//! bar's control segment until the operator dismisses it:
//! * a command with no wire form;
//! * one the routing gate holds back (`crate::backend::tradehub_control::may_send_to_backend`);
//! * one the client refuses before the wire: `RemoteControlHandle::try_command`'s `Busy` (the queue
//!   to the node was full) or `Gone` (the link closed), or a verb the node predates (a TP/SL
//!   bracket to a node without the `bracket` capability);
//! * one the NODE refuses (its control rate limit cutting a large Cancel all into a part sent and a
//!   part refused, a bracket rule): it comes back on the control channel to the same segment.
//!
//! # The node's directory
//! Venue names and the account list are the node's `Directory` reply
//! (`vike_tradehub_client::directory`), fetched on a throwaway thread ([`spawn_directory_fetch`]),
//! once per backend and then at most once every [`DIRECTORY_MAX_AGE`] while an open Trade window
//! needs it ([`should_fetch_directory`]). A failed fetch ENDS like a good one, so a node that
//! refuses the verb is asked once a minute, never every frame; it keeps the last good list; and it
//! is logged. The window stays usable without a list: it names venues by their keys and lists the
//! accounts the snapshot runs.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use super::trade::{TradePick, UNADDRESSABLE_WHY};
use crate::orders::order_dispatch::{DispatchReject, SubmitSource, TradeAddress};
use crate::tools::ToolView;
use crate::ui::workspace::{WinKind, WinState};
use vike_model::accounts::account_keys::AccountLabel;
use vike_panels::trade::{StatusKind, TradeAction};
use vike_tradehub_client::wire::WireDirectory;

/// How many recently used instruments a window keeps (the instrument picker's "Recent" rows).
const RECENT_KEPT: usize = 5;

/// What a window shows when its order intents could not be sent because this desktop has no
/// control channel to the server: a read-only connection (no control key), or one still
/// connecting.
pub const NOT_SENT_NO_CONTROL: &str = "Not sent: this desktop has no control connection to the \
                                       server (a read-only connection, or one still connecting).";

/// What the Trade windows asked of the shell in one frame, gathered while the window loop holds the
/// windows and applied after it.
#[derive(Debug, Default)]
pub struct TradeFrame {
    /// Every order intent, each with the address of the window that emitted it, in drawing order.
    /// They go to `order_dispatch::DispatchInputs::trade_actions` unchanged.
    pub orders: Vec<(TradeAddress, TradeAction)>,
    /// A window's "Connect" asked for the Connections window.
    pub open_connections: bool,
    /// The instrument of the window the trader last ACTED in this frame (an order, a pick, a
    /// Connect), the one a new window copies (minor 26). `None` when nobody acted: the shell keeps
    /// the window it already had ([`Self::remember`]).
    pub acted: Option<TradePick>,
    /// The book each window shows, `(venue, venue-native symbol)`, after this frame's picks: the
    /// depth streams the shell keeps running.
    pub books: Vec<(String, String)>,
}

impl TradeFrame {
    /// Drain one Trade window's intents after the widget drew (C2's `drain_trade_window`): apply
    /// its picks, notes, seed and view-size change to the window, and address its orders.
    pub fn drain(&mut self, w: &mut WinState, tv: &mut ToolView) {
        // ONE address, from the state the widget drew (minor 30): a pick below moves the window,
        // never an order placed before it.
        let drawn = TradeAddress::parse(&w.venue, tv.trade_account.as_deref(), &w.symbol);
        let mut acted = false;
        let mut dropped = 0usize;
        for action in tv.trade_actions.drain(..) {
            match action {
                TradeAction::PickSymbol { venue, symbol } => {
                    // Another venue does not run the account this window named (carry 6).
                    if venue != w.venue {
                        tv.trade_account = None;
                    }
                    tv.trade_recent.retain(|(v, s)| *v != venue || *s != symbol);
                    tv.trade_recent.insert(0, (venue.clone(), symbol.clone()));
                    tv.trade_recent.truncate(RECENT_KEPT);
                    (w.venue, w.symbol) = (venue, symbol);
                    acted = true;
                }
                // The row's venue AND the symbol it opens there (C3), never this venue's symbol.
                TradeAction::PickAccount { venue, account, symbol } => {
                    (w.venue, w.symbol, tv.trade_account) = (venue, symbol, account);
                    acted = true;
                }
                TradeAction::OpenConnections => {
                    self.open_connections = true;
                    acted = true;
                }
                // A note stays only if the strip stops following an order (carry 5).
                TradeAction::Note { kind, text } => {
                    tv.trade_status = Some((kind, text));
                    tv.trade_status_coid = None;
                }
                // The order intents, NAMED (M-3 of the A4 review): a catch-all binding here would
                // address a new non-order intent as an order; a new variant is a compile error.
                order @ (TradeAction::Place { .. }
                | TradeAction::Modify { .. }
                | TradeAction::Cancel(_)
                | TradeAction::CancelSide(_)
                | TradeAction::CancelAll
                | TradeAction::ClosePosition
                | TradeAction::Reverse) => match &drawn {
                    Some(addr) => {
                        self.orders.push((addr.clone(), order));
                        acted = true;
                    }
                    None => dropped += 1,
                },
            }
        }
        if dropped > 0 {
            tracing::warn!(
                venue = %w.venue,
                symbol = %w.symbol,
                "Trade window: {dropped} order intent(s) not sent: {UNADDRESSABLE_WHY}"
            );
            tv.trade_status = Some((StatusKind::Error, UNADDRESSABLE_WHY.to_string()));
            tv.trade_status_coid = None;
        }
        // The glue's first-frame seed (carry 7): applied whenever it offers one.
        if let Some(p) = tv.trade_pick.take() {
            (w.venue, w.symbol, tv.trade_account) = (p.venue, p.symbol, p.account);
        }
        if let Some(size) = tv.trade_resize.take() {
            w.size = size;
            w.pending = Some(egui::Rect::from_min_size(w.pos, size));
        }
        if w.symbol.is_empty() {
            return;
        }
        if acted {
            self.acted = Some(TradePick {
                venue: w.venue.clone(),
                account: tv.trade_account.clone(),
                symbol: w.symbol.clone(),
            });
        }
        self.books.push((w.venue.clone(), w.symbol.clone()));
    }

    /// Keep the window the trader acted in this frame as the one a new window copies; a frame
    /// nobody acted in leaves `last` as it was.
    pub fn remember(&mut self, last: &mut Option<TradePick>) {
        if let Some(p) = self.acted.take() {
            *last = Some(p);
        }
    }
}

/// What a BACKEND SWITCH must forget (M-10/M-7 of the A4 review): every Trade window's held
/// (confirm-pending) order, what its status strip was following, and the window a new one copies.
///
/// ⚠ A held order is keyed on the window's venue, account, symbol and mode, and on NO backend: the
/// widget drops it when a DRAWN window's address changes, but a window that is not drawn during the
/// switch (minimized, or hidden behind a maximized one) keeps it, and if the new backend runs the
/// same account in the same mode, a confirm would send it to the NEW backend's book. So the shell
/// calls this on every switch, for every window, drawn or not. `last_trade` names the old backend's
/// instrument and account, which the new one may not run.
///
/// The strip is keyed on no backend either (M-1 of the final review, slice B): left alone it kept
/// the old backend's order line while the new one connected, and on the new node's first snapshot
/// it took that node's newest pre-existing order — same address, nothing seen there yet — for the
/// window's own. Forgetting what it followed and what it has seen starts it clean, exactly as a new
/// window starts.
pub fn on_backend_switch(
    views: &mut HashMap<egui::Id, ToolView>,
    last_trade: &mut Option<TradePick>,
) {
    for tv in views.values_mut() {
        tv.trade.held = None;
        tv.trade_status = None;
        tv.trade_status_coid = None;
        tv.trade_seen = None;
    }
    *last_trade = None;
}

/// A CLOSED Trade window forgets its held (confirm-pending) order (M-8 of the final review, slice
/// B). Closing a window (✕) keeps its `ToolView` for the rail, and reopening it showed the old
/// order with a live Place, priced at whatever was typed then. Called every frame, so a window
/// reopened later has nothing held; an OPEN window (minimized included) keeps its own.
pub fn forget_held_of_closed_windows(wins: &[WinState], views: &mut HashMap<egui::Id, ToolView>) {
    for w in wins.iter().filter(|w| !w.open && w.kind == WinKind::Trade) {
        if let Some(tv) = views.get_mut(&w.id) {
            tv.trade.held = None;
        }
    }
}

/// Whether a refusal came from a Trade window. Exhaustive, so a new submit path has to say.
fn from_trade_window(source: SubmitSource) -> bool {
    match source {
        SubmitSource::Trade
        | SubmitSource::TradeBracket
        | SubmitSource::Dom
        | SubmitSource::DomExit => true,
        SubmitSource::Cockpit | SubmitSource::Options => false,
    }
}

/// `account` with the default account spelled `None`, whichever way it arrived: the dispatcher
/// names the default account `DEFAULT` on a venue that runs a second one.
fn named(account: Option<&AccountLabel>) -> Option<&AccountLabel> {
    account.filter(|l| !l.is_default())
}

/// Write `text` on the strip of EVERY Trade window at `(venue, symbol, account)`, and stop the
/// strip following an order so it stays there (M-1 of the A4 review). A refusal names an address,
/// not a window, so two windows on one address both show it; that is the same book, and the
/// trader sees the refusal wherever they look at it.
fn post(
    wins: &[WinState],
    views: &mut HashMap<egui::Id, ToolView>,
    at: (&str, &str, Option<&AccountLabel>),
    text: &str,
) {
    let (venue, symbol, account) = at;
    for w in wins.iter().filter(|w| w.kind == WinKind::Trade && w.venue == venue) {
        let Some(tv) = views.get_mut(&w.id) else { continue };
        let window = TradeAddress::parse(&w.venue, tv.trade_account.as_deref(), &w.symbol);
        if window.is_some_and(|a| a.symbol == symbol && named(a.account.as_ref()) == named(account))
        {
            tv.trade_status = Some((StatusKind::Error, text.to_string()));
            tv.trade_status_coid = None;
        }
    }
}

/// Put each of the planner's refusals of a Trade window intent on the strip of the windows at its
/// venue, symbol AND account (minor 31): a window on another account of the same symbol never
/// shows a refusal that was not its own.
pub fn route_rejects(
    wins: &[WinState],
    views: &mut HashMap<egui::Id, ToolView>,
    rejects: &[DispatchReject],
) {
    for r in rejects.iter().filter(|r| from_trade_window(r.source)) {
        let text = format!("Not sent: {}", r.reason);
        post(wins, views, (&r.venue, &r.symbol, r.account.as_ref()), &text);
    }
}

/// Say on each window that tried that its order intents were not sent, because this desktop has no
/// control channel ([`NOT_SENT_NO_CONTROL`]).
pub fn refuse_unsent(
    wins: &[WinState],
    views: &mut HashMap<egui::Id, ToolView>,
    orders: &[(TradeAddress, TradeAction)],
) {
    for (addr, _) in orders {
        post(wins, views, (&addr.venue, &addr.symbol, addr.account.as_ref()), NOT_SENT_NO_CONTROL);
    }
}

/// The shell's cache of one backend's `Directory` reply.
#[derive(Debug, Clone, Default)]
pub struct DirectorySlot {
    /// The backend the reply is FROM. A reply for another backend is dropped.
    pub addr: String,
    /// `true` while a fetch is on the wire.
    pub pending: bool,
    /// When the last fetch ENDED, reply or refusal, so an older node is not asked every frame.
    pub fetched_at: Option<Instant>,
    /// The last GOOD reply. `None` before one and for a backend that never answered (a node that
    /// predates the verb, or one whose settings database it cannot read). A failed REFRESH keeps
    /// it: a stale list beats an empty one. Only a backend switch clears it.
    pub reply: Option<Arc<WireDirectory>>,
}

/// How old a reply may get while a Trade window is open. Accounts are added and switched on
/// rarely; a minute is the longest a new one waits to appear in the list, and the shortest a node
/// that refuses the verb waits to be asked again.
pub const DIRECTORY_MAX_AGE: Duration = Duration::from_secs(60);

/// Whether the shell starts a `Directory` fetch this frame: a backend is active and the slot holds
/// nothing for it, or holds a reply older than [`DIRECTORY_MAX_AGE`] while a Trade window is open.
/// Never while one for the SAME backend is on the wire.
///
/// ⚠ The backend is compared FIRST (M-2 of the final review, slice B): a switch while the left
/// backend's fetch is still on the wire starts the new backend's at once — [`spawn_directory_fetch`]
/// resets the slot, and the left fetch's result is dropped when it lands — rather than waiting for
/// a fetch that might never end.
pub fn should_fetch_directory(
    active: Option<&str>,
    slot: &DirectorySlot,
    trade_open: bool,
    now: Instant,
) -> bool {
    let Some(addr) = active else { return false };
    if slot.addr != addr {
        return true;
    }
    if slot.pending {
        return false;
    }
    match slot.fetched_at {
        None => true,
        Some(at) => trade_open && now.duration_since(at) >= DIRECTORY_MAX_AGE,
    }
}

/// Land a finished fetch in the slot. A result for a backend the slot no longer names is dropped.
/// A refusal or a failure still ENDS the fetch (so an older node, or one answering "directory
/// unavailable", is asked once a minute and never every frame) but KEEPS the last good reply: a
/// transient error must not blank a list the operator is looking at. [`spawn_directory_fetch`] is
/// what resets the slot when the backend changes.
pub fn apply_directory_result(
    slot: &mut DirectorySlot,
    addr: &str,
    result: std::io::Result<WireDirectory>,
    now: Instant,
) {
    if slot.addr != addr {
        return;
    }
    slot.pending = false;
    slot.fetched_at = Some(now);
    if let Ok(dir) = result {
        slot.reply = Some(Arc::new(dir));
    }
}

/// The slot, whether or not a fetch thread panicked while holding it: the slot is plain data, and
/// a GUI frame must not panic on a background thread's behalf.
fn lock(slot: &Mutex<DirectorySlot>) -> MutexGuard<'_, DirectorySlot> {
    slot.lock().unwrap_or_else(PoisonError::into_inner)
}

/// [`should_fetch_directory`] for the shell: the slot behind its lock, and "a Trade window is open"
/// read off the shell's windows.
pub fn directory_due(
    slot: &Mutex<DirectorySlot>,
    active: Option<&str>,
    wins: &[WinState],
    now: Instant,
) -> bool {
    let trade_open = wins.iter().any(|w| w.open && w.kind == WinKind::Trade);
    should_fetch_directory(active, &lock(slot), trade_open, now)
}

/// Whether the ACTIVE backend's account list is unavailable: its fetch has ENDED (a node that
/// refuses the verb, one that cannot read its database, a failed or timed-out read) with no list —
/// including while the once-a-minute retry is on the wire. Not before the first fetch has ended,
/// not once a reply has landed, and not after a failed REFRESH, whose last good list stands. The
/// Trade windows say so (the owner's decision of 10-03, item 5); before this the failure was in the
/// log alone.
pub fn directory_unavailable(slot: &Mutex<DirectorySlot>, active: Option<&str>) -> bool {
    let s = lock(slot);
    active == Some(s.addr.as_str()) && s.reply.is_none() && s.fetched_at.is_some()
}

/// The directory this frame's windows read: the slot's reply when it is the ACTIVE backend's, else
/// none — after a switch the old backend's list is never shown under the new one.
pub fn directory_reply(
    slot: &Mutex<DirectorySlot>,
    active: Option<&str>,
) -> Option<Arc<WireDirectory>> {
    let s = lock(slot);
    if active == Some(s.addr.as_str()) { s.reply.clone() } else { None }
}

/// Start ONE `Directory` fetch for `addr` OFF the calling thread (C2's `spawn_directory_fetch`):
/// mark the slot pending first (a new backend's slot starts from nothing), run `fetch` on a
/// throwaway thread, land its result with [`apply_directory_result`], log a failure, and ask for a
/// repaint. `fetch` is the network call (`vike_tradehub_client::directory` with the backend's
/// observe key, bound by the shell). Returns the thread, `None` when it could not be spawned, in
/// which case the fetch has ended as a failure rather than panicking the frame.
pub fn spawn_directory_fetch<F, R>(
    slot: Arc<Mutex<DirectorySlot>>,
    addr: String,
    fetch: F,
    repaint: R,
) -> Option<std::thread::JoinHandle<()>>
where
    F: FnOnce(&str) -> std::io::Result<WireDirectory> + Send + 'static,
    R: FnOnce() + Send + 'static,
{
    {
        let mut s = lock(&slot);
        if s.addr != addr {
            *s = DirectorySlot { addr: addr.clone(), ..DirectorySlot::default() };
        }
        s.pending = true;
    }
    let (thread_slot, thread_addr) = (Arc::clone(&slot), addr.clone());
    let spawned = std::thread::Builder::new().name("vt-directory".into()).spawn(move || {
        let result = fetch(&thread_addr);
        if let Err(e) = &result {
            tracing::warn!(
                "the node's directory is unavailable ({thread_addr}): {e}. Venue names fall back \
                 to their keys and the account list to the accounts the server runs; asked again \
                 in a minute while a Trade window is open"
            );
        }
        apply_directory_result(&mut lock(&thread_slot), &thread_addr, result, Instant::now());
        repaint();
    });
    match spawned {
        Ok(handle) => Some(handle),
        Err(e) => {
            tracing::warn!("could not start the directory fetch for {addr}: {e}");
            apply_directory_result(&mut lock(&slot), &addr, Err(e), Instant::now());
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orders::order_dispatch::{DispatchInputs, DispatchRejectReason, plan_dispatch};
    use crate::orders::order_entry::OrderLimits;
    use crate::ui::tool_views::CockpitCmd;
    use crate::ui::workspace::WinKind;
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};
    use std::time::Instant;
    use vike_core::{CoreSnapshot, VenueBlock};
    use vike_model::accounts::account_keys::AccountLabel;
    use vike_panels::trade::{Exits, OrderType, Origin};

    fn trade_win(id: &str, venue: &str, symbol: &str) -> WinState {
        let at = egui::Rect::from_min_size(egui::pos2(10.0, 20.0), egui::vec2(600.0, 560.0));
        let mut w = WinState::tool(id, WinKind::Trade, at);
        w.venue = venue.to_string();
        w.symbol = symbol.to_string();
        w
    }

    fn view(account: Option<&str>, actions: Vec<TradeAction>) -> ToolView {
        ToolView {
            trade_account: account.map(str::to_string),
            trade_actions: actions,
            ..ToolView::default()
        }
    }

    fn buy() -> TradeAction {
        TradeAction::Place {
            side: 1,
            order_type: OrderType::Limit,
            price: Some(100.0),
            qty: 1.0,
            reduce_only: false,
            exits: None,
            origin: Origin::Ticket,
        }
    }

    fn pick(venue: &str, symbol: &str) -> TradeAction {
        TradeAction::PickSymbol { venue: venue.into(), symbol: symbol.into() }
    }

    fn pair(venue: &str, symbol: &str) -> (String, String) {
        (venue.to_string(), symbol.to_string())
    }

    fn at(w: &WinState, tv: &ToolView) -> (String, String, Option<String>) {
        (w.venue.clone(), w.symbol.clone(), tv.trade_account.clone())
    }

    fn here(venue: &str, symbol: &str, account: Option<&str>) -> (String, String, Option<String>) {
        (venue.to_string(), symbol.to_string(), account.map(str::to_string))
    }

    fn empty_directory() -> WireDirectory {
        WireDirectory { venues: vec![], accounts: vec![] }
    }

    // ===== the drain =====

    /// Carry 6 of the A3 review: a symbol on ANOTHER venue opens on that venue's default account
    /// (the account the window named is not that venue's), while a symbol on the SAME venue keeps
    /// the account the trader chose.
    #[test]
    fn a_pick_on_another_venue_resets_the_account_and_one_on_the_same_venue_keeps_it() {
        let mut frame = TradeFrame::default();
        let mut w = trade_win("t1", "binance", "BTCUSDT");
        let mut tv = view(Some("SUB"), vec![pick("binance", "ETHUSDT")]);
        frame.drain(&mut w, &mut tv);
        assert_eq!(at(&w, &tv), here("binance", "ETHUSDT", Some("SUB")), "same venue");
        tv.trade_actions.push(pick("okx", "BTC-USDT-SWAP"));
        frame.drain(&mut w, &mut tv);
        assert_eq!(at(&w, &tv), here("okx", "BTC-USDT-SWAP", None), "another venue");
        assert_eq!(
            tv.trade_recent,
            vec![pair("okx", "BTC-USDT-SWAP"), pair("binance", "ETHUSDT")],
            "recent picks are (venue, symbol) PAIRS, newest first"
        );
    }

    /// C3: a recent pick names its venue, so the same symbol on two venues is two picks; a repeat
    /// moves to the front rather than appearing twice; the list keeps the newest five.
    #[test]
    fn recent_picks_are_venue_symbol_pairs_newest_first_without_repeats() {
        let mut frame = TradeFrame::default();
        let mut w = trade_win("t1", "binance", "BTCUSDT");
        let mut tv = ToolView::default();
        for (v, s) in [
            ("binance", "A"),
            ("okx", "A"),
            ("binance", "B"),
            ("binance", "A"),
            ("binance", "C"),
            ("binance", "D"),
            ("binance", "E"),
        ] {
            tv.trade_actions.push(pick(v, s));
            frame.drain(&mut w, &mut tv);
        }
        let want: Vec<(String, String)> = [
            ("binance", "E"),
            ("binance", "D"),
            ("binance", "C"),
            ("binance", "A"),
            ("binance", "B"),
        ]
        .iter()
        .map(|(v, s)| pair(v, s))
        .collect();
        assert_eq!(tv.trade_recent, want);
    }

    /// C3 (the drain's side): picking an account row moves the window to that row's venue AND to
    /// the symbol the row opens there — never the old venue's native symbol on the new venue.
    #[test]
    fn a_picked_account_opens_its_own_venue_and_symbol() {
        let mut frame = TradeFrame::default();
        let mut w = trade_win("t1", "okx", "BTC-USDT-SWAP");
        let mut tv = view(
            None,
            vec![TradeAction::PickAccount {
                venue: "binance".into(),
                account: Some("SUB".into()),
                symbol: "BTCUSDT.P".into(),
            }],
        );
        frame.drain(&mut w, &mut tv);
        assert_eq!(at(&w, &tv), here("binance", "BTCUSDT.P", Some("SUB")));
    }

    /// Minor 30 and carry 8: every order intent carries the address the widget DREW — computed
    /// once, before the drain — even when a pick earlier in the same frame has moved the window.
    #[test]
    fn every_order_carries_the_address_the_window_drew_even_after_a_pick_in_the_same_frame() {
        let mut frame = TradeFrame::default();
        let mut w = trade_win("t1", "binance", "BTCUSDT");
        let mut tv =
            view(Some("SUB"), vec![buy(), pick("bybit", "ETHUSDT"), TradeAction::CancelAll]);
        frame.drain(&mut w, &mut tv);
        let drawn = TradeAddress::parse("binance", Some("SUB"), "BTCUSDT").expect("a label");
        assert_eq!(frame.orders, vec![(drawn.clone(), buy()), (drawn, TradeAction::CancelAll)]);
        assert_eq!(at(&w, &tv), here("bybit", "ETHUSDT", None));
    }

    /// Carry 1 of the A3 review: a window whose account text is not an account label sends
    /// NOTHING, and its strip says why. The `.ok()` habit would have read the text as the default
    /// account and placed the order on a different book.
    #[test]
    fn a_window_whose_account_is_not_a_label_sends_nothing_and_says_why() {
        let mut frame = TradeFrame::default();
        let mut w = trade_win("t1", "binance", "BTCUSDT");
        let mut tv = view(Some("sub"), vec![buy(), TradeAction::ClosePosition]);
        tv.trade_status_coid = Some("ui-1-1".into());
        frame.drain(&mut w, &mut tv);
        assert!(frame.orders.is_empty(), "never the default account: {:?}", frame.orders);
        assert_eq!(tv.trade_status, Some((StatusKind::Error, UNADDRESSABLE_WHY.to_string())));
        assert_eq!(tv.trade_status_coid, None, "the refusal stays on the strip");
        assert_eq!(frame.acted, None, "a refused click does not make this the window to copy");
    }

    /// Carry 5: a note the widget writes stays on the strip only because the drain stops the strip
    /// following an order; `follow_status` would otherwise rewrite it with that order next frame.
    #[test]
    fn a_note_replaces_the_followed_order_on_the_strip() {
        let mut frame = TradeFrame::default();
        let mut w = trade_win("t1", "binance", "BTCUSDT");
        let note = TradeAction::Note { kind: StatusKind::Info, text: "a note".into() };
        let mut tv = view(None, vec![note]);
        tv.trade_status_coid = Some("ui-1-1".into());
        frame.drain(&mut w, &mut tv);
        assert_eq!(tv.trade_status, Some((StatusKind::Info, "a note".to_string())));
        assert_eq!(tv.trade_status_coid, None);
        assert!(frame.orders.is_empty());
        assert_eq!(frame.acted, None, "a note is what the window says, not what the trader did");
    }

    /// Carry 7: a seed opens the window whenever the glue offers one, and a view-control click
    /// resizes the window to the size the new view asks for.
    #[test]
    fn the_seed_opens_the_window_and_a_view_change_resizes_it() {
        let mut frame = TradeFrame::default();
        let mut w = trade_win("t1", "binance", "");
        let size = egui::vec2(320.0, 680.0);
        let mut tv = ToolView {
            trade_pick: Some(TradePick {
                venue: "bybit".into(),
                account: Some("SUB".into()),
                symbol: "ETHUSDT".into(),
            }),
            trade_resize: Some(size),
            ..ToolView::default()
        };
        frame.drain(&mut w, &mut tv);
        assert_eq!(at(&w, &tv), here("bybit", "ETHUSDT", Some("SUB")));
        assert_eq!(tv.trade_pick, None);
        assert_eq!(tv.trade_resize, None);
        assert_eq!(w.size, size);
        assert_eq!(w.pending, Some(egui::Rect::from_min_size(w.pos, size)));
        assert_eq!(frame.acted, None, "a seed is not the trader acting");
        assert_eq!(frame.books, vec![pair("bybit", "ETHUSDT")], "the seeded book is streamed");
    }

    /// Minor 26: a new window copies the window the trader last ACTED in, not whichever Trade
    /// window the loop happened to visit last; a frame nobody acted in leaves the choice alone.
    #[test]
    fn the_window_to_copy_is_the_one_the_trader_acted_in() {
        let mut frame = TradeFrame::default();
        let (mut a, mut b) = (trade_win("ta", "binance", "BTCUSDT"), trade_win("tb", "okx", "ETH"));
        let (mut tva, mut tvb) = (view(Some("SUB"), vec![buy()]), view(None, vec![]));
        frame.drain(&mut a, &mut tva);
        frame.drain(&mut b, &mut tvb);
        let acted = TradePick {
            venue: "binance".into(),
            account: Some("SUB".into()),
            symbol: "BTCUSDT".into(),
        };
        assert_eq!(frame.acted, Some(acted.clone()), "not the window drawn last");
        let mut last = None;
        frame.remember(&mut last);
        assert_eq!(last, Some(acted.clone()));
        let mut quiet = TradeFrame::default();
        quiet.drain(&mut b, &mut tvb);
        quiet.remember(&mut last);
        assert_eq!(last, Some(acted), "nobody acted: the last choice stands");
    }

    /// I-1 of the A4 review: the drain EMPTIES the window's intents. An order drained once is never
    /// drained again: re-addressing the same intents every frame would send the same order once a
    /// frame, each time under a fresh client order id.
    #[test]
    fn an_order_is_drained_once_and_never_again() {
        let mut w = trade_win("t1", "binance", "BTCUSDT");
        let mut tv = view(None, vec![buy()]);
        let mut first = TradeFrame::default();
        first.drain(&mut w, &mut tv);
        assert_eq!(first.orders.len(), 1, "the order goes out once");
        assert!(tv.trade_actions.is_empty(), "the drain empties the window's intents");
        let mut second = TradeFrame::default();
        second.drain(&mut w, &mut tv);
        assert!(second.orders.is_empty(), "and never again: {:?}", second.orders);
        assert!(tv.trade_actions.is_empty());
    }

    /// Two Trade windows draining in one frame each keep their OWN address: the second window's
    /// orders never take the first window's venue, account or symbol, nor the other way round.
    #[test]
    fn two_windows_in_one_frame_keep_their_own_addresses() {
        let mut frame = TradeFrame::default();
        let mut a = trade_win("ta", "binance", "BTCUSDT");
        let mut b = trade_win("tb", "okx", "ETH-USDT-SWAP");
        let mut tva = view(Some("SUB"), vec![buy()]);
        let mut tvb = view(None, vec![buy(), TradeAction::CancelAll]);
        frame.drain(&mut a, &mut tva);
        frame.drain(&mut b, &mut tvb);
        let at_a = TradeAddress::parse("binance", Some("SUB"), "BTCUSDT").unwrap();
        let at_b = TradeAddress::parse("okx", None, "ETH-USDT-SWAP").unwrap();
        assert_eq!(
            frame.orders,
            vec![(at_a, buy()), (at_b.clone(), buy()), (at_b, TradeAction::CancelAll)]
        );
    }

    /// M-10/M-7 of the A4 review: a held (confirm-pending) order names no backend, so on a backend
    /// switch every Trade window forgets it — a window that was not drawn during the switch
    /// (minimized, or hidden behind a maximized one) would otherwise confirm it onto the NEW
    /// backend's book. The window a new one copies is forgotten too: it named the old backend's
    /// instrument and account.
    #[test]
    fn a_backend_switch_forgets_every_held_order_and_the_window_to_copy() {
        let mut views = HashMap::new();
        for id in ["shown", "minimized", "hidden"] {
            let mut tv = view(Some("SUB"), vec![]);
            tv.trade.held = Some(buy());
            views.insert(egui::Id::new(id), tv);
        }
        let mut last = Some(TradePick {
            venue: "binance".into(),
            account: Some("SUB".into()),
            symbol: "BTCUSDT".into(),
        });
        on_backend_switch(&mut views, &mut last);
        assert!(views.values().all(|tv| tv.trade.held.is_none()), "every window, drawn or not");
        assert_eq!(last, None);
    }

    /// M-8 of the final review (slice B): a CLOSED Trade window forgets its held (confirm-pending)
    /// order, so reopening it never shows the old order with a live Place, priced at whatever was
    /// typed then. A window that is open — minimized included, it is still on the rail — keeps its
    /// own.
    #[test]
    fn a_closed_trade_window_forgets_its_held_order() {
        let mut wins = vec![
            trade_win("open", "binance", "BTCUSDT"),
            trade_win("minimized", "binance", "BTCUSDT"),
            trade_win("closed", "binance", "BTCUSDT"),
        ];
        wins[1].minimized = true;
        wins[2].open = false;
        let mut views = HashMap::new();
        for w in &wins {
            let mut tv = view(None, vec![]);
            tv.trade.held = Some(buy());
            views.insert(w.id, tv);
        }
        forget_held_of_closed_windows(&wins, &mut views);
        assert!(views[&wins[0].id].trade.held.is_some(), "an open window keeps its prompt");
        assert!(views[&wins[1].id].trade.held.is_some(), "so does a minimized one");
        assert!(views[&wins[2].id].trade.held.is_none(), "a closed one forgets it");
    }

    /// The window's "Connect" button asks the shell for the Connections window.
    #[test]
    fn connect_asks_the_shell_for_the_connections_window() {
        let mut frame = TradeFrame::default();
        let mut w = trade_win("t1", "binance", "BTCUSDT");
        let mut tv = view(None, vec![TradeAction::OpenConnections]);
        frame.drain(&mut w, &mut tv);
        assert!(frame.open_connections);
        assert!(frame.orders.is_empty());
    }

    /// Each window with an instrument streams ITS book, by venue and native symbol; a window the
    /// node has not seeded yet streams nothing.
    #[test]
    fn each_window_with_an_instrument_streams_its_own_book() {
        let mut frame = TradeFrame::default();
        for (v, s) in [("binance", "BTCUSDT"), ("bybit", "BTCUSDT"), ("binance", "")] {
            let (mut w, mut tv) = (trade_win("t", v, s), ToolView::default());
            frame.drain(&mut w, &mut tv);
        }
        assert_eq!(frame.books, vec![pair("binance", "BTCUSDT"), pair("bybit", "BTCUSDT")]);
    }

    // ===== refusals on the strip =====

    fn block(route_key: &str, account: Option<&str>, symbol: &str) -> VenueBlock {
        VenueBlock {
            venue: "binance".into(),
            account: account.and_then(|a| AccountLabel::parse(a).ok()),
            route_key: route_key.into(),
            symbol: symbol.into(),
            mode: Some(vike_exec::EngineMode::Paper),
            ..Default::default()
        }
    }

    /// Binance with two engines: the default account trading BTCUSDT and SUB trading ETHUSDT.
    fn two_accounts() -> CoreSnapshot {
        let mut s = CoreSnapshot::empty("binance", "BTCUSDT");
        s.portfolio.venues =
            vec![block("binance", None, "BTCUSDT"), block("binance#SUB", Some("SUB"), "ETHUSDT")];
        s
    }

    fn strip(views: &HashMap<egui::Id, ToolView>, w: &WinState) -> Option<(StatusKind, String)> {
        views[&w.id].trade_status.clone()
    }

    /// Minor 31: a refusal reaches the Trade windows at its venue, symbol AND account — not a
    /// window on another account of the same symbol, not a window on another symbol. Driven through
    /// the real dispatcher, so the reject carries the account the planner saw: the default
    /// account's preview refusal names `DEFAULT` on this two-engine venue, and still finds the
    /// default-account window.
    #[test]
    fn a_refusal_reaches_the_windows_of_its_venue_symbol_and_account_only() {
        let snap = two_accounts();
        let wins = vec![
            trade_win("ta", "binance", "BTCUSDT"),
            trade_win("tb", "binance", "BTCUSDT"),
            trade_win("tc", "binance", "ETHUSDT"),
            trade_win("td", "polymarket", "tok"),
        ];
        let mut views = HashMap::new();
        views.insert(wins[0].id, view(None, vec![]));
        views.insert(wins[1].id, view(Some("SUB"), vec![]));
        views.insert(wins[2].id, view(Some("SUB"), vec![]));
        views.insert(wins[3].id, view(None, vec![]));
        views.get_mut(&wins[1].id).unwrap().trade_status_coid = Some("ui-9-9".into());
        let addr = |acc: Option<&str>| TradeAddress::parse("binance", acc, "BTCUSDT").unwrap();
        let big = TradeAction::Place {
            side: 1,
            order_type: OrderType::Limit,
            price: Some(100.0),
            qty: 10.0,
            reduce_only: false,
            exits: None,
            origin: Origin::Ladder,
        };
        let plan = plan_dispatch(
            DispatchInputs {
                trade_actions: vec![(addr(Some("SUB")), buy()), (addr(None), big)],
                cockpit_cmds: vec![CockpitCmd::Submit {
                    token: "tok".into(),
                    side: 1,
                    price: Some(0.5),
                    qty: 4000.0,
                }],
                ..Default::default()
            },
            &OrderLimits { max_notional: 500.0, ..OrderLimits::default() },
            &snap,
            1,
            1,
        );
        assert_eq!(plan.rejects.len(), 3, "{:?}", plan.rejects);
        route_rejects(&wins, &mut views, &plan.rejects);
        let not_traded = format!("Not sent: {}", DispatchRejectReason::NotTraded);
        assert_eq!(strip(&views, &wins[1]), Some((StatusKind::Error, not_traded)));
        assert_eq!(views[&wins[1].id].trade_status_coid, None, "the refusal stays on the strip");
        let a = strip(&views, &wins[0]).expect("the default window's own preview refusal");
        assert!(a.0 == StatusKind::Error && a.1.starts_with("Not sent: "), "{a:?}");
        assert!(
            !a.1.contains("does not trade"),
            "SUB's refusal is not the default window's: {a:?}"
        );
        assert_eq!(strip(&views, &wins[2]), None, "another symbol");
        assert_eq!(strip(&views, &wins[3]), None, "a cockpit refusal is not a Trade window's");
    }

    /// I-1 of the final review (slice B): the dispatcher's refusal of a TP/SL bracket on an engine
    /// whose lane cannot hold the stop-loss (the shipped daemon's spot binance engine) reaches the
    /// window that sent it, on its OWN strip — not only the node's control segment.
    #[test]
    fn a_bracket_refused_for_its_engines_lane_is_said_on_the_windows_strip() {
        let mut snap = CoreSnapshot::empty("binance", "BTCUSDT");
        snap.portfolio.venues = vec![block("binance", None, "BTCUSDT")];
        let wins = vec![trade_win("ta", "binance", "BTCUSDT")];
        let mut views = HashMap::from([(wins[0].id, view(None, vec![]))]);
        let bracket = TradeAction::Place {
            side: 1,
            order_type: OrderType::Limit,
            price: Some(100.0),
            qty: 1.0,
            reduce_only: false,
            exits: Some(Exits { take_profit: 101.0, stop_loss: 99.0 }),
            origin: Origin::Ticket,
        };
        let addr = TradeAddress::parse("binance", None, "BTCUSDT").expect("an address");
        let plan = plan_dispatch(
            DispatchInputs { trade_actions: vec![(addr, bracket)], ..Default::default() },
            &OrderLimits::default(),
            &snap,
            1,
            1,
        );
        assert!(plan.commands.is_empty(), "{:?}", plan.commands);
        route_rejects(&wins, &mut views, &plan.rejects);
        let want = format!("Not sent: {}", DispatchRejectReason::BracketNeedsPerpEngine);
        assert_eq!(strip(&views, &wins[0]), Some((StatusKind::Error, want)));
    }

    /// A desktop with no control channel (a read-only connection, or one still connecting) sends
    /// nothing, and the windows that tried say so rather than swallowing the click.
    #[test]
    fn orders_from_a_desktop_with_no_control_channel_say_they_were_not_sent() {
        let wins =
            vec![trade_win("ta", "binance", "BTCUSDT"), trade_win("tb", "binance", "BTCUSDT")];
        let mut views = HashMap::new();
        views.insert(wins[0].id, view(None, vec![]));
        views.insert(wins[1].id, view(Some("SUB"), vec![]));
        let orders = vec![(TradeAddress::parse("binance", None, "BTCUSDT").unwrap(), buy())];
        refuse_unsent(&wins, &mut views, &orders);
        assert_eq!(strip(&views, &wins[0]), Some((StatusKind::Error, NOT_SENT_NO_CONTROL.into())));
        assert_eq!(strip(&views, &wins[1]), None, "another account's window did not try");
    }

    // ===== the node's directory =====

    /// What a FINISHED fetch does to the slot.
    #[test]
    fn a_failed_refresh_keeps_the_last_good_directory_and_a_result_for_a_left_backend_is_dropped() {
        let now = Instant::now();
        let good = empty_directory();
        let refused = || Err(std::io::Error::other("directory unavailable"));
        let mut slot = DirectorySlot { addr: "a".into(), pending: true, ..Default::default() };

        apply_directory_result(&mut slot, "a", refused(), now);
        assert!(slot.reply.is_none(), "a first fetch that fails leaves no directory");
        assert!(
            !slot.pending && slot.fetched_at == Some(now),
            "the fetch ENDED: no per-frame retry"
        );

        slot.pending = true;
        apply_directory_result(&mut slot, "a", Ok(good.clone()), now);
        assert!(slot.reply.is_some(), "a good reply lands");

        slot.pending = true;
        apply_directory_result(&mut slot, "a", refused(), now);
        assert!(slot.reply.is_some(), "a failed refresh KEEPS the last good reply");
        assert!(!slot.pending, "...and still ends the fetch");

        let mut left = DirectorySlot { addr: "b".into(), pending: true, ..Default::default() };
        apply_directory_result(&mut left, "a", Ok(good), now);
        assert!(left.reply.is_none() && left.pending, "a reply for a backend we left is dropped");
    }

    /// WHEN a fetch starts: once per backend, then at most once a minute while an OPEN Trade window
    /// needs the list, and never while one for the SAME backend is on the wire. That bounds the
    /// retries against a node that refuses the verb.
    #[test]
    fn the_directory_is_fetched_per_backend_and_refreshed_only_while_a_trade_window_is_open() {
        let now = Instant::now();
        let fresh = DirectorySlot { addr: "a".into(), fetched_at: Some(now), ..Default::default() };
        assert!(!should_fetch_directory(None, &fresh, true, now), "no backend, no fetch");
        assert!(should_fetch_directory(Some("b"), &fresh, false, now), "a new backend is asked");
        assert!(!should_fetch_directory(Some("a"), &fresh, true, now), "a fresh reply stands");
        let later = now + DIRECTORY_MAX_AGE;
        assert!(should_fetch_directory(Some("a"), &fresh, true, later), "stale + open: refresh");
        assert!(!should_fetch_directory(Some("a"), &fresh, false, later), "no window, no refresh");
        let busy = DirectorySlot { pending: true, ..fresh.clone() };
        assert!(!should_fetch_directory(Some("a"), &busy, true, later), "one fetch at a time");
        // M-2 of the final review (slice B): a SWITCH does not wait for the left backend's fetch,
        // which may never end; the spawn resets the slot for the new backend.
        assert!(should_fetch_directory(Some("b"), &busy, true, later), "a new backend is asked");

        // `directory_due` asks the same question of the shell's windows: only an OPEN Trade window
        // keeps the list fresh.
        let slot = Mutex::new(fresh);
        let mut w = trade_win("t1", "binance", "BTCUSDT");
        assert!(directory_due(&slot, Some("a"), std::slice::from_ref(&w), later));
        w.open = false;
        assert!(!directory_due(&slot, Some("a"), std::slice::from_ref(&w), later), "closed");
        let rect = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(400.0, 300.0));
        let account = WinState::tool("acc", WinKind::Account, rect);
        assert!(!directory_due(&slot, Some("a"), &[account], later), "not a Trade window");
    }

    /// The owner's decision of 10-03, item 5: the windows say "Account list unavailable." while the
    /// ACTIVE backend's fetch has ended with no list — a node that refused the verb, or one that
    /// failed — including while its once-a-minute retry is on the wire. Not before the first fetch
    /// has ended (the list is coming), not once a reply has landed, and not after a failed REFRESH
    /// (the last good list stands); and never for a backend the slot is not about.
    #[test]
    fn the_account_list_is_unavailable_exactly_while_the_active_backends_fetch_ended_with_none() {
        let now = Instant::now();
        let refused = || Err(std::io::Error::other("directory unavailable"));
        let slot = Mutex::new(DirectorySlot { addr: "a".into(), ..Default::default() });
        assert!(!directory_unavailable(&slot, Some("a")), "not asked yet");
        slot.lock().unwrap().pending = true;
        assert!(!directory_unavailable(&slot, Some("a")), "the first fetch is on the wire");
        apply_directory_result(&mut slot.lock().unwrap(), "a", refused(), now);
        assert!(directory_unavailable(&slot, Some("a")), "it ended with no list");
        slot.lock().unwrap().pending = true;
        assert!(directory_unavailable(&slot, Some("a")), "...and stays so while it is retried");
        assert!(!directory_unavailable(&slot, Some("b")), "another backend's slot says nothing");
        assert!(!directory_unavailable(&slot, None), "no backend");
        apply_directory_result(&mut slot.lock().unwrap(), "a", Ok(empty_directory()), now);
        assert!(!directory_unavailable(&slot, Some("a")), "a reply landed");
        apply_directory_result(&mut slot.lock().unwrap(), "a", refused(), now);
        assert!(!directory_unavailable(&slot, Some("a")), "a failed refresh keeps the last list");
    }

    /// A window reads the ACTIVE backend's directory and no other: after a switch, the old
    /// backend's list is not shown under the new one while the new fetch is on the wire.
    #[test]
    fn a_window_reads_only_the_active_backends_directory() {
        let slot = Mutex::new(DirectorySlot {
            addr: "a".into(),
            reply: Some(Arc::new(empty_directory())),
            ..Default::default()
        });
        assert!(directory_reply(&slot, Some("a")).is_some());
        assert!(directory_reply(&slot, Some("b")).is_none());
        assert!(directory_reply(&slot, None).is_none());
    }

    /// The fetch runs OFF the calling thread: the slot is marked pending before the call returns
    /// (so the next frame starts no second fetch), the reply lands when the fetch ends, the frame
    /// is asked to repaint, and a fetch for a NEW backend drops the old backend's list first.
    #[test]
    fn a_fetch_runs_off_the_calling_thread_and_a_new_backend_starts_from_nothing() {
        let slot = Arc::new(Mutex::new(DirectorySlot::default()));
        let (go, wait) = std::sync::mpsc::channel::<()>();
        let (painted, repaint) = std::sync::mpsc::channel::<()>();
        let fetch = move |addr: &str| {
            assert_eq!(addr, "a");
            wait.recv().expect("released");
            Ok(empty_directory())
        };
        let handle = spawn_directory_fetch(Arc::clone(&slot), "a".into(), fetch, move || {
            let _ = painted.send(());
        })
        .expect("spawned");
        assert!(slot.lock().unwrap().pending, "pending before the fetch has answered");
        assert!(!should_fetch_directory(Some("a"), &slot.lock().unwrap(), true, Instant::now()));
        go.send(()).unwrap();
        handle.join().expect("the fetch thread does not panic");
        repaint.try_recv().expect("the frame is asked to repaint");
        {
            let s = slot.lock().unwrap();
            assert!(!s.pending && s.reply.is_some() && s.fetched_at.is_some());
        }
        let failing = |_: &str| Err(std::io::Error::other("directory unavailable"));
        spawn_directory_fetch(Arc::clone(&slot), "b".into(), failing, || {})
            .expect("spawned")
            .join()
            .expect("no panic on a failed fetch");
        let s = slot.lock().unwrap();
        assert_eq!(s.addr, "b");
        assert!(s.reply.is_none(), "b's failure never shows a's list");
        assert!(!s.pending && s.fetched_at.is_some(), "the failed fetch ended: no retry storm");
    }
}
