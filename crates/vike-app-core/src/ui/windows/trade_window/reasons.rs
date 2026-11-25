//! The Trade window's refusal sentences (the `*_WHY` texts).

use vike_order_entry::trade_window::symbol_follow::TradeWindowOneSymbolReason;

/// Why a listed account cannot be picked, when the node does not say why. It NEVER says "switched
/// off": the account list says what the node's database holds, and an account the node is not
/// running may be inactive, or simply not mounted this boot — the window cannot tell which. When
/// the node DOES say (it publishes the accounts it does not run, with the reason: decision 0156),
/// the window says [`vike_account_window::accounts::not_running_because`] instead (the Account
/// window's Link cell says the same words).
pub(crate) const NOT_RUNNING_WHY: &str = "Not running on the server.";

/// Why an account whose venue does not list the window's instrument cannot be picked from the account
/// menu: the menu lists EVERY account, and a pick there would move the window to a venue with no such
/// instrument. The symbol picker is how the window moves to another venue.
pub(crate) const NOT_LISTED_WHY: &str = "This instrument is not listed on this venue. Pick a symbol in the \
                                  symbol picker to move the window there.";

/// What a bookless ladder says over a venue's bid and ask: the quote is the whole of its market data.
pub(crate) const QUOTE_ONLY_HEADLINE: &str = "BID AND ASK ONLY — THIS VENUE STREAMS NO DEPTH";

/// The strip's line on a venue's bid and ask that are not real time: `word` is the Account window's
/// badge word (`DELAYED`, `FROZEN`), so a label reads one way wherever it shows.
pub(crate) fn delayed_quote_why(word: &str) -> String {
    format!("The bid and the ask are {word}: the venue says this stream is not real time.")
}

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
/// (`vike_order_entry::dispatch::bracket_lane_holds_stop` reads its rule), so the window says so before the
/// click instead of the account reason, which would be false on a venue's single default account.
pub(crate) const TAKE_PROFIT_STOP_LOSS_LANE_WHY: &str = "TP/SL is not available on this account yet: its engine trades the \
                                 spot market, where the exchange cannot hold the stop-loss.";

/// Why TP/SL is off on a symbol the account's engine was not started on: the node judges what a
/// lane can hold from the ENGINE's symbol (`vike_order_entry::dispatch::bracket_on_mounted_symbol`).
pub(crate) const TAKE_PROFIT_STOP_LOSS_SYMBOL_WHY: &str =
    "TP/SL is only available on the symbol this account was started on, for now.";

/// Why an account that keeps to one symbol (`TradeWindowOneSymbolReason`) takes no order on the picked `symbol`: what
/// it does trade, then the reason (`trades` is never empty here: a block that names no symbol is
/// not judged).
pub(crate) fn one_symbol_why(
    symbol: &str,
    trades: &[String],
    why: TradeWindowOneSymbolReason,
) -> String {
    format!("This account does not trade {symbol}. It trades {}. {}", trades.join(", "), why.why())
}

/// Why a window at a venue's DEFAULT account takes no order on the perpetual `symbol` while another
/// account of the venue (`account`, a label) runs the perpetual lane (`vike_order_entry::dispatch::perp_lane_account`):
/// the order belongs to that account's book, and this window looks for the position, the markers and the
/// cancels at the account it names, so it would show none of them. The venue's LANE TWIN is not this
/// case: the window addresses it itself (`vike_order_entry::dispatch::lane_twin_account`).
pub(crate) fn perp_lane_why(symbol: &str, account: &str) -> String {
    format!(
        "{symbol} is a perpetual, and this account's engine trades the spot market. Its perpetual \
         lane is the account {account}: pick {account} in the account menu, where this window \
         shows the position and the orders and Close, Reverse and Cancel all act on them."
    )
}

/// Why an account that takes any symbol of its venue takes no order on the picked `symbol`: the
/// venue's catalog (`venue_label`'s) does not list it, so there is no instrument to resolve, no
/// lot to size and no price grid. `loaded` is whether the catalog holds anything yet.
pub(crate) fn not_in_catalog_why(symbol: &str, venue_label: &str, loaded: bool) -> String {
    if loaded {
        format!(
            "{symbol} is not in {venue_label}'s instrument catalog, so this account cannot trade \
             it. Refresh the catalog in Data Manager → Instruments."
        )
    } else {
        format!("The instrument catalog has not loaded yet, so {symbol} cannot be traded here.")
    }
}

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
