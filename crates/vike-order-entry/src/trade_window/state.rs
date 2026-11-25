//! What a Trade window's state does from frame to frame: it follows the window's instrument, venue
//! and account ([`TradeWindowState::sync`]), and it turns the ticket's and the ladder's intents into
//! the orders the window sends, holding one for a confirm where the account asks for it.

use super::describe::why_untradable;
use super::{
    ONE_CLICK_OFF_ON_LIVE, ONE_CLICK_OFF_ON_UNKNOWN, TradeWindowAction, TradeWindowInputs,
    TradeWindowSizingUnit, TradeWindowState, TradeWindowStatusLineKind, TradeWindowTicketKind,
    sizing, ticket,
};
use vike_model::AccountTier;

impl TradeWindowState {
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
    /// [`TradeWindowTradable::No`](super::TradeWindowTradable::No) and nothing else: the widget has no fault or halt input of its own, so a
    /// halted account or a stopped core drops the order only because the app maps both to
    /// `TradeWindowTradable::No` (its glue does: an account the server halted, or a core that stopped, takes
    /// no order from any window).
    pub fn sync(&mut self, inputs: &TradeWindowInputs<'_>, actions: &mut Vec<TradeWindowAction>) {
        let address = (
            inputs.venue.to_string(),
            inputs.account.map(str::to_string),
            inputs.symbol.to_string(),
        );
        // The quick sizes are all zero exactly while the lot is not known.
        let default = sizing::quick_sizes(inputs.grid)[2];
        let seed = || {
            sizing::size_text(
                default,
                TradeWindowSizingUnit::Base,
                None,
                inputs.grid.lot_size,
                inputs.value,
            )
        };
        if self.address.as_ref() != Some(&address) {
            // Only the trader's grouping is remembered: an automatic one is picked afresh.
            if let Some((venue, _account, symbol)) = self.address.take()
                && self.group_chosen
            {
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
            let kept = self.groups.get(&instrument).copied();
            self.group = kept.unwrap_or(1);
            self.group_chosen = kept.is_some();
            self.mid_seen = None;
            self.steps.clear();
            self.unit = TradeWindowSizingUnit::Base;
            self.size = seed();
            self.unknown_lot_seed = (default <= 0.0).then(|| self.size.clone());
        } else if default > 0.0
            && let Some(seeded) = self.unknown_lot_seed.take()
            && self.unit == TradeWindowSizingUnit::Base
            && self.size == seeded
        {
            self.size = seed();
        }
        if self.mode_seen != Some(inputs.mode) {
            let first = self.mode_seen.is_none();
            self.mode_seen = Some(inputs.mode);
            if !first && self.held.take().is_some() {
                actions.push(TradeWindowAction::Note {
                    kind: TradeWindowStatusLineKind::Error,
                    text: HELD_MODE.to_string(),
                });
            }
            match inputs.mode {
                Some(AccountTier::Live) | None => {
                    if self.one_click && !first {
                        let text = if inputs.mode == Some(AccountTier::Live) {
                            ONE_CLICK_OFF_ON_LIVE
                        } else {
                            ONE_CLICK_OFF_ON_UNKNOWN
                        };
                        actions.push(TradeWindowAction::Note {
                            kind: TradeWindowStatusLineKind::Info,
                            text: text.into(),
                        });
                    }
                    self.one_click = false;
                }
                Some(AccountTier::Paper) | Some(AccountTier::Demo) => {
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
        let grid =
            (inputs.grid.tick_size.map(f64::to_bits), inputs.grid.lot_size.map(f64::to_bits));
        if self.grid_seen != Some(grid) {
            let first = self.grid_seen.is_none();
            self.grid_seen = Some(grid);
            if !first && self.held.take().is_some() {
                actions.push(TradeWindowAction::Note {
                    kind: TradeWindowStatusLineKind::Error,
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
            && let Some(exit @ (TradeWindowAction::ClosePosition | TradeWindowAction::Reverse)) =
                &self.held
            && size != asked
        {
            let what = if matches!(exit, TradeWindowAction::Reverse) { "reverse" } else { "close" };
            self.held = None;
            actions.push(TradeWindowAction::Note {
                kind: TradeWindowStatusLineKind::Error,
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
            actions.push(TradeWindowAction::Note {
                kind: TradeWindowStatusLineKind::Error,
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
            Some(TradeWindowAction::Place {
                side,
                order_type: TradeWindowTicketKind::Market,
                ..
            }) => Some(*side),
            Some(TradeWindowAction::Reverse) => Some(vike_model::closing_side(size)),
            _ => None,
        };
        if let Some(side) = trades
            && ticket::market_ref(side, inputs).is_none()
        {
            self.held = None;
            actions.push(TradeWindowAction::Note {
                kind: TradeWindowStatusLineKind::Error,
                text: format!("{HELD_NO_PRICE} {}", ticket::NO_MARKET_PRICE_WHY),
            });
        }
        if self.held.is_none() {
            self.held_position = None;
        }
        // ⚠ Post only is a maker-only promise, so it is never left ticked where the order could not
        // keep it: the moment the node, the venue or the entry type stops offering it the choice is
        // turned off, in view (the button un-presses and greys with its reason). Left ticked it
        // would be dropped silently on the way out, and the order would take liquidity.
        if self.post_only && ticket::post_only_block(inputs, self.entry()).is_some() {
            self.post_only = false;
        }
    }

    /// Send `a`, or hold it for a confirm when one-click trading is off (spec §3.6).
    pub fn submit(&mut self, a: TradeWindowAction, actions: &mut Vec<TradeWindowAction>) {
        if self.one_click {
            actions.push(a);
        } else {
            self.held = Some(a);
            self.held_position = None;
        }
    }

    /// [`TradeWindowState::submit`] for a Close or a Reverse, asked against a position of signed `size`:
    /// held, its prompt names that size, and the order goes if the position changes before the
    /// confirm ([`TradeWindowState::sync`]).
    pub(super) fn submit_exit(
        &mut self,
        a: TradeWindowAction,
        size: f64,
        actions: &mut Vec<TradeWindowAction>,
    ) {
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
    pub(super) fn entry(&self) -> TradeWindowTicketKind {
        if self.compact { TradeWindowTicketKind::Market } else { self.order_type }
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
pub(super) const HELD_NO_PRICE: &str =
    "Not sent: the price for its side is gone, so the order waiting for a confirm was dropped.";
