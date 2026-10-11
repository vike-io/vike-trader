//! The Polymarket measurement bins' shared code. A binary cannot share a module with another
//! binary, so what two of the research bins (`cheap_np_depth`, `cheap_np_askgate`) both need lives
//! here, in the library — and is gated with them: `lib.rs` declares this module behind
//! `datafusion-store`, the bins' own `required-features`, so a default build compiles none of it.
//!
//! - `anchor` — the on-chain → CLOB anchor: which recorded CLOB print an on-chain entry is.
//! - `entries` — the `--entries` CSV reader (`Entry`, `read_entries`) both bins read.

pub mod anchor;
pub mod entries;
