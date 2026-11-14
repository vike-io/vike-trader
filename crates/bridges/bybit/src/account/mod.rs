//! The account itself: who the key trades, its modes and leverage, its link and quote rate.

// The READ of an account's leverage (decision 0182): the gatewayhub's lane asks it before and after the
// one write this crate's exec thread makes. Signed GETs through `transport`, so exec-plane.
pub mod leverage;
// The account modes the adapter orders in (one-way positions, cross margin), put right through the API
// before an order or refused in words: signed GETs and POSTs through `transport`, so exec-plane.
pub mod modes;
