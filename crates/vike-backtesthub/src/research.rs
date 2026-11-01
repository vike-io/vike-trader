//! The Polymarket measurement bins' shared code. A binary cannot share a module with another
//! binary, so what two of the research bins (`cheap_np_depth`, `cheap_np_askgate`) both need lives
//! here, in the library. (It was gated behind a `datafusion-store` feature with them; the feature is
//! gone with the move to this crate, which links the store unconditionally.)
//!
//! - `anchor` — the on-chain → CLOB anchor: which recorded CLOB print an on-chain entry is.
//! - `entries` — the `--entries` CSV reader (`Entry`, `read_entries`) both bins read.

pub mod anchor;
pub mod entries;
