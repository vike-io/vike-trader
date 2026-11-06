//! The options chain: the calendar constructors and strike window over the chain snapshot, and
//! the pure column model the chain grid renders from. Both are venue-free and I/O-free; the
//! pricing they lean on lives in [`crate::pricing`], and the snapshot TYPES themselves
//! ([`OptionChain`](vike_marketdata::OptionChain), [`Expiry`](vike_marketdata::Expiry),
//! [`StrikeRow`](vike_marketdata::StrikeRow), [`OptionQuote`](vike_marketdata::OptionQuote)) are
//! feed output and live in `vike-marketdata` (decision 0126).
//!
//! - [`model`] — the 08:00-UTC [`expiry_ms`](model::expiry_ms), the calendar-day
//!   [`make_expiry`](model::make_expiry) and the strike windowing
//!   [`limit_strikes`](model::limit_strikes).
//! - [`columns`] — the column model: field sets, `cell_value` and the format specs.
//! - [`index`] — the [`OptionIndex`](index::OptionIndex): instrument name to payload contract, what
//!   the folds and the Greeks join look a streamed name up in.
//! - [`fold`] — the streamed `markprice`/`ticker` row folds onto a
//!   [`UnderlyingChains`](vike_marketdata::UnderlyingChains) grid.
//! - [`select`] — the underlying/expiry selection, the canonical underlying order and the
//!   front-expiry focus-set.

pub mod columns;
pub mod fold;
pub mod index;
pub mod model;
pub mod select;
