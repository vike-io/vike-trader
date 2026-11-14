//! What a symbol IS and the grid it trades on.

// Which V5 book an engine symbol trades in, and the one suffix (`.S`, spot) that tells two books of one name
// apart (decision 0231). Names only, no signed call: the tick pump (feeds side) picks its socket by it too.
pub mod class;
// The keyless instrument-list read `data`'s routing seam asks whether a bare symbol can mean two
// books (`docs/decisions/0061`, phase 0). FEEDS-side like `data` and `catalog`: it opens no signed
// call and reads no credential.
pub mod listing;
