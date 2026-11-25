//! The account itself: who the key trades, its modes and leverage, its link and quote rate.

// What the bridge does on the DEMO host so a symbol of a class beyond the plain perpetual trades without a hand
// step (a first leverage for a never-traded symbol), and the words of the TradFi-Perps agreement the holder signs
// by hand. Signs, so exec-plane.
pub mod class_prep;
pub mod identity;
// The READ of an account's leverage (decision 0182): the gatewayhub's lane asks it before and after the
// one write this crate's exec thread makes. Signed GETs through the shared transport, so exec-plane.
pub mod leverage;
// The two account modes the adapter orders under (one-way, cross), SET through the API at a perp start and
// before a perpetual's first order, and the words an order carries when one of them blocks it. Signs, so
// exec-plane.
pub mod modes;
