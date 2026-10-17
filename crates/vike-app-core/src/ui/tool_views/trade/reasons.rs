//! The Trade window's refusal sentences (the `*_WHY` texts) and [`ControlLink`].

/// Why a listed account cannot be picked, when the node does not say why. It NEVER says "switched
/// off": the account list says what the node's database holds, and an account the node is not
/// running may be inactive, or simply not mounted this boot — the window cannot tell which. When
/// the node DOES say (it publishes the accounts it does not run, with the reason: decision 0156),
/// the window says [`not_running_because`] instead.
pub(crate) const NOT_RUNNING_WHY: &str = "Not running on the server.";

/// [`NOT_RUNNING_WHY`] with the node's own reason (`CoreSnapshot::not_running`, the `ArmingBlock`
/// word: `no credentials`, `flags.tradehub_live off`, …): an account runs at the tier its row states
/// or not at all, and the window says which stop holds it (decision 0156).
pub(crate) fn not_running_because(reason: &str) -> String {
    format!("Not running on the server: {reason}.")
}

/// Why an account whose venue does not list the window's instrument cannot be picked from the account
/// menu: the menu lists EVERY account, and a pick there would move the window to a venue with no such
/// instrument. The symbol picker is how the window moves to another venue.
pub(crate) const NOT_LISTED_WHY: &str = "This instrument is not listed on this venue. Pick a symbol in the \
                                  symbol picker to move the window there.";

/// What a window shows before the node's first snapshot, when it has nothing to open.
pub(crate) const WAITING_FOR_NODE: &str = "Waiting for the node.";

/// Why a window whose account text is not an account label sends nothing.
pub(crate) const UNADDRESSABLE_WHY: &str =
    "This window's account is not one an order can name, so it sends nothing. Pick an account.";

/// Why a window on an account whose engine is HALTED (`vike_model::TradingState::Halted`, the kill
/// switch) sends nothing. The core still admits a reduce its position covers, but the window offers
/// no order there, so the line says what the WINDOW does.
pub(crate) const HALTED_WHY: &str =
    "Trading is halted on this account: the server opens nothing, and this window sends nothing.";

/// What the strip says on an account whose engine is REDUCING (`vike_model::TradingState::Reducing`):
/// it still trades, but the server refuses an order that would not reduce its position.
pub(crate) const REDUCING_WHY: &str = "This account takes only orders that reduce its position.";

/// Why TP/SL is off on an account whose engine's LANE cannot hold a bracket's stop-loss: an engine
/// mounted on the spot market of a venue that runs a spot and a perp lane — the shipped daemon's one
/// default binance engine (I-1 of the final review, slice B). The node refuses every bracket there
/// (`order_dispatch::bracket_lane_holds_stop` reads its rule), so the window says so before the
/// click instead of the account reason, which would be false on a venue's single default account.
pub(crate) const TPSL_LANE_WHY: &str = "TP/SL is not available on this account yet: its engine trades the \
                                 spot market, where the exchange cannot hold the stop-loss.";

/// What a window says when the node did not answer the `Directory` request (the owner's decision of
/// 10-03, item 5): the account picker carries it, and the strip where nothing more important is on
/// it. The window stays usable — venue names fall back to their keys, and the account list to the
/// accounts the snapshot runs.
pub(crate) const ACCOUNT_LIST_UNAVAILABLE: &str = "Account list unavailable.";

/// Why no window sends an order from a READ-ONLY desktop: no control channel to the server (none
/// armed, or one that failed to connect). The owner's decision of 10-03, item 11: said up front, as
/// the window's stated cause, rather than learned from a "Not sent" after the click.
pub(crate) const READ_ONLY_WHY: &str = "Read-only: this desktop cannot send orders.";

/// Why no window sends an order while this desktop's control link to the node is GONE (it was
/// connected and closed: a node restart, a dropped tunnel). The owner's decision of 10-03, item 11.
pub(crate) const NO_CONTROL_LINK_WHY: &str = "No control link to the node.";

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
