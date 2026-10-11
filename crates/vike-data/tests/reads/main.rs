//! `reads` -- the bounded-read contracts on the real DataFusion backend, ONE test binary over four
//! files: a head is a complete prefix, a budget is one budget, an edges read decodes `ts` only.
#![cfg(feature = "hist-datafusion")]

mod bar_edges;
mod bars_head;
mod common;
mod research_head;
mod tick_scan_head;
