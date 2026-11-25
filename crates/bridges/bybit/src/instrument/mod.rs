//! What a symbol IS and the grid it trades on.

// The venue's `(symbol, category)` of a USER name, read off its kept row, and the way back (decisions 0231, 0290).
// No signed call: the tick pump and the feeds (feeds side) pick their socket by it too.
pub mod class;
// The keyless instrument-list read `data`'s routing seam asks whether a bare symbol can mean two
// books (`docs/decisions/0061`, phase 0). FEEDS-side like `data` and `catalog`: it opens no signed
// call and reads no credential.
pub mod listing;
