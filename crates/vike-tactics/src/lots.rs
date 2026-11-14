//! What the rule holds, as the language books it from its fills: one [`TacticsBookLot`] per entry fill, closed
//! first in, first out whoever made the closing fill (the model's law, `vike_model::close_lots_fifo`,
//! the closed-trade ledger's too), and one [`TacticsBookBinding`] per entry fill, which its own
//! exits and closes take their quantity off first (`crate::RuleRun::bound_exits`). Quantities only: no
//! money is booked here (a ledger of closed rows is its reader's, the lab's or `vike-analytics`').

use vike_model::{Fill, LOT_DUST, close_lots_fifo};

use vike_model::OrderSide;

/// What one entry fill opened and still holds.
#[derive(Debug, Clone, PartialEq)]
pub struct TacticsBookLot {
    /// The name of the entry that opened it (`""` for a fill no named order made).
    pub name: String,
    pub side: OrderSide,
    pub qty: f64,
    pub price: f64,
    pub entry_timestamp: i64,
}

/// What one entry fill opened, and how much of it its own exits and closes have not yet taken
/// (`crate::RuleRun::bound_exits`). A fill closes the oldest lots whoever made it, but takes its quantity
/// off the entries of the name it was issued for, the oldest first, and only what is left over off
/// the oldest entry of any name.
#[derive(Debug, Clone, PartialEq)]
pub struct TacticsBookBinding {
    pub name: String,
    pub side: OrderSide,
    pub entry_ms: i64,
    pub price: f64,
    /// What the fill opened.
    pub size: f64,
    pub left: f64,
}

impl vike_model::Lot for TacticsBookLot {
    fn side(&self) -> i32 {
        i32::from(self.side)
    }

    fn quantity_mut(&mut self) -> &mut f64 {
        &mut self.qty
    }
}

/// Books `fill` on `lots`: it closes the oldest lots of the other side first, whatever their name,
/// then opens a lot named `name` with what it has left: `vike_model::close_lots_fifo`, the law the
/// closed-trade ledger books by too (`vike_analytics::closed_trades::TradeClosedLedger::apply`).
pub(crate) fn book_fill(lots: &mut Vec<TacticsBookLot>, fill: &Fill, name: &str) {
    if fill.size.is_nan() || fill.size <= 0.0 {
        return;
    }
    let side = OrderSide::of(fill.side);
    let left = close_lots_fifo(lots, i32::from(side), fill.size, |_| true, |_| {});
    if left > LOT_DUST {
        lots.push(TacticsBookLot {
            name: name.to_string(),
            side,
            qty: left,
            price: fill.price,
            entry_timestamp: fill.timestamp,
        });
    }
}

/// What `fill` closes of `lots`, worked out before [`book_fill`] books it.
pub(crate) fn closes_of(lots: &[TacticsBookLot], fill: &Fill) -> f64 {
    let side = OrderSide::of(fill.side);
    let held: f64 = lots.iter().filter(|l| l.side != side).map(|l| l.qty).sum();
    fill.size.min(held)
}

/// Takes `fill` off `bindings`: the `closed` part off those of `own` first, then off the oldest,
/// and what it opened becomes a binding of `opens`. `true` when it opened one. With `flat` (no lot
/// is left) every binding goes.
pub(crate) fn bind(
    bindings: &mut Vec<TacticsBookBinding>,
    fill: &Fill,
    closed: f64,
    own: Option<&str>,
    opens: &str,
    flat: bool,
) -> bool {
    let side = OrderSide::of(fill.side);
    let mut left = closed;
    for name in [own, None] {
        let other = bindings.iter_mut().filter(|b| b.side != side);
        for b in other.filter(|b| name.is_none_or(|n| b.name == n)) {
            let taken = left.min(b.left);
            b.left -= taken;
            left -= taken;
        }
    }
    bindings.retain(|b| b.left > LOT_DUST);
    let opened = fill.size - closed;
    let opens_one = opened > LOT_DUST;
    if opens_one {
        bindings.push(TacticsBookBinding {
            name: opens.to_string(),
            side,
            entry_ms: fill.timestamp,
            price: fill.price,
            size: opened,
            left: opened,
        });
    }
    if flat {
        bindings.clear();
    }
    opens_one
}
