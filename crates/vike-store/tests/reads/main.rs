//! `reads` -- the bounded-read contracts on the real DataFusion backend, ONE test binary over five
//! files: a head is a complete prefix, a budget is one budget, an edges read decodes `ts` only, and
//! a directory the store cannot open is an error.

mod bar_edges;
mod bars_head;
mod common;
mod research_head;
mod tick_scan_head;
#[cfg(unix)]
mod unreadable_dir;
