//! The order REST lanes (the `VenueRest` impls) and the per-symbol router.

// The Algo Service wire (conditional orders): binance only, the stop's submit/cancel/lookup and the
// `ALGO_UPDATE` mapper. Signs, so exec-plane.
pub mod algo;
// COIN-M (`dapi`), the third book the exec plane signs in: its hosts, the transport that carries the USDⓈ-M
// code's requests to their dapi twins, the mounted contract's money and its funding read. Signs, so exec-plane.
pub mod coin_m;
pub mod per_symbol;
pub mod perp;
pub mod spot;
