//! The order-side view models: the DOM math, the option chain's orders and greeks. The ticket and
//! the dispatch of order intents are `vike_order_entry::dispatch`'s. (The Trade window's sizing is
//! the widget's own, `vike_order_entry::trade_window::sizing`; the Account window's equity panel is
//! `vike_account_window::equity`.)

pub mod node_refusals;
pub(crate) mod option_chain_orders;
pub(crate) mod option_chain_position_greeks;
