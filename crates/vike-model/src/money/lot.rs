//! The FIFO lot law: one lot per entry fill, and a fill of the other side closes the OLDEST lots
//! first (first in, first out), then opens a lot with what it has left. Quantities only: what a
//! close is worth is its caller's (the closed-trade ledger books money on each [`LotClose`], the
//! order language's book books none).
//!
//! The ONE law behind two books that must agree on what is open, so it lives below both: the order
//! language's lots (`vike_tactics::TacticsBookLot`, which may not name `vike-analytics`) and the closed-trade
//! ledger's (`vike_analytics::closed_trades::OpenLot`). Each keeps its own lot type and says which
//! lots a fill may close ([`close_lots_fifo`]'s `eligible`).

/// Below this a quantity is nothing left: a lot this small is closed, a fill remainder this small
/// opens nothing. The strategy lab's value, under which every one of its rows was matched.
pub const LOT_DUST: f64 = 1e-12;

/// One open lot as the FIFO law reads it: the side it holds and the quantity it still holds.
pub trait Lot {
    /// The side it holds, as the order seam signs it (`+1` long, `-1` short). The law only compares
    /// it with the closing fill's.
    fn side(&self) -> i32;
    /// The quantity it still holds, which a close takes its part off.
    fn quantity_mut(&mut self) -> &mut f64;
}

/// What one closing fill takes from one lot, handed to [`close_lots_fifo`]'s caller BEFORE the
/// quantity comes off the lot.
#[derive(Debug)]
pub struct LotClose<'a, L> {
    /// The lot, as it stood before this close.
    pub lot: &'a mut L,
    /// The quantity this close takes from it: the lot's whole quantity or what the fill has left,
    /// whichever is less.
    pub qty: f64,
}

/// Closes a fill of `side` and `size` against `lots`, oldest first, and returns what it has left to
/// open (`size` less what it closed).
///
/// While more than [`LOT_DUST`] is left, the oldest lot `eligible` admits is the next one: a lot on
/// the fill's own side stops the walk (nothing is left to close), otherwise the fill takes the lesser
/// of the lot's quantity and what it has left, `on_close` sees that [`LotClose`], the quantity comes
/// off, and a lot left at or below [`LOT_DUST`] is removed. No eligible lot stops the walk too. A
/// `size` that is NaN or not above zero closes nothing and comes back unchanged.
///
/// ```
/// use vike_model::{Lot, close_lots_fifo};
///
/// struct Held(i32, f64);
/// impl Lot for Held {
///     fn side(&self) -> i32 {
///         self.0
///     }
///     fn quantity_mut(&mut self) -> &mut f64 {
///         &mut self.1
///     }
/// }
///
/// // Two longs, 2 then 3; a sell of 6 closes the 2, then the 3, and has 1 left to open short.
/// let mut lots = vec![Held(1, 2.0), Held(1, 3.0)];
/// let mut closed = Vec::new();
/// let left = close_lots_fifo(&mut lots, -1, 6.0, |_| true, |c| closed.push((c.lot.1, c.qty)));
/// assert_eq!((left, closed), (1.0, vec![(2.0, 2.0), (3.0, 3.0)]));
/// assert!(lots.is_empty());
///
/// // A sell of 1 against a long of 3 takes the oldest's part and leaves the lot open.
/// let mut lots = vec![Held(1, 3.0), Held(1, 1.0)];
/// assert_eq!(close_lots_fifo(&mut lots, -1, 1.0, |_| true, |_| {}), 0.0);
/// assert_eq!((lots[0].1, lots[1].1), (2.0, 1.0));
///
/// // A buy against longs closes nothing, and neither does a lot `eligible` turns away.
/// assert_eq!(close_lots_fifo(&mut lots, 1, 1.0, |_| true, |_| {}), 1.0);
/// assert_eq!(close_lots_fifo(&mut lots, -1, 1.0, |_| false, |_| {}), 1.0);
/// ```
pub fn close_lots_fifo<L: Lot>(
    lots: &mut Vec<L>,
    side: i32,
    size: f64,
    eligible: impl Fn(&L) -> bool,
    mut on_close: impl FnMut(LotClose<'_, L>),
) -> f64 {
    let mut left = size;
    while left > LOT_DUST {
        let Some(i) = lots.iter().position(&eligible) else { break };
        if lots[i].side() == side {
            break;
        }
        let qty = left.min(*lots[i].quantity_mut());
        on_close(LotClose { lot: &mut lots[i], qty });
        let held = lots[i].quantity_mut();
        *held -= qty;
        left -= qty;
        if *held <= LOT_DUST {
            lots.remove(i);
        }
    }
    left
}
