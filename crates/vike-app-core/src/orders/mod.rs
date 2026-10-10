//! Order entry: the ticket, dispatch of order intents, the DOM math, options books and greeks, the
//! equity panel. (The Trade window's sizing is the widget's own, `vike_panels::trade::sizing`.)

pub mod dom_math;
pub mod equity_panel;
pub mod options_books;
pub mod options_greeks;
pub mod order_dispatch;
pub mod order_entry;
