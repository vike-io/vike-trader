//! The five in-place upgrade steps a store passes through on its next write, one module each: the
//! `sim` -> `paper` rename, ruling 6's `AUTOINCREMENT`, the dropped columns, step 7's `'any'` tier and
//! the venue links. Every one rebuilds through the single procedure in `rebuild`.

pub(super) mod any_tier;
pub(super) mod autoincrement;
pub(super) mod dropped_columns;
pub(super) mod sim_to_paper;
pub(super) mod venue_links;
