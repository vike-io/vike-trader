//! The account itself: who the key trades, its modes and leverage, its link and quote rate.

// No `///` here: an outer doc beside the file's own `//!` makes rustdoc resolve the module's links from this
// scope, where `AccountPerQuote` and `quote_rate` are not (the module doc says what it is).
pub mod quote_rate;
