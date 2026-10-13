//! The Trade window's SHELL half: what the desktop's window loop does with a Trade window once the
//! widget has drawn (the Trade window plan's Task 19, pre-flight finding C2).
//!
//! # Why it is here and not in the shell
//! `vike-desktop` is outside every CI roster lane: the `app-check` job compiles and clippy-gates it
//! and runs none of its logic, and `crates/vike-ops/tests/gui/ci_excluded_gui_shell_ratchet/ratchet.rs`'s
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
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use super::trade::{TradePick, UNADDRESSABLE_WHY};
use crate::lock;
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

#[path = "trade_shell_tests.rs"]
#[cfg(test)]
mod trade_shell_tests;
