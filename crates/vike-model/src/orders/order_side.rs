//! The side an order takes ([`OrderSide`]: `Long` buys, `Short` sells), the order language's word
//! for it (`vike_tactics::Tactics::entry`, a Rhai script's `OrderSide::Long`). The order seam itself
//! keeps its signed `i32` (`crate::OrderBroker::place`, decision 0018): `From` converts.

use std::ops::Neg;

/// The side an entry or a free order takes: `Long` buys, `Short` sells.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrderSide {
    Long,
    Short,
}

impl OrderSide {
    /// The side of a signed number on the order seam: `> 0` buys, anything else sells.
    pub fn of(sign: i32) -> OrderSide {
        if sign > 0 { OrderSide::Long } else { OrderSide::Short }
    }
}

impl From<OrderSide> for i32 {
    /// `+1` for `Long`, `-1` for `Short`: the seam's side (`crate::OrderBroker::place`).
    fn from(side: OrderSide) -> i32 {
        match side {
            OrderSide::Long => 1,
            OrderSide::Short => -1,
        }
    }
}

impl From<OrderSide> for f64 {
    /// `1.0` for `Long`, `-1.0` for `Short`: the direction a level is offset in.
    fn from(side: OrderSide) -> f64 {
        f64::from(i32::from(side))
    }
}

impl Neg for OrderSide {
    type Output = OrderSide;

    fn neg(self) -> OrderSide {
        match self {
            OrderSide::Long => OrderSide::Short,
            OrderSide::Short => OrderSide::Long,
        }
    }
}
