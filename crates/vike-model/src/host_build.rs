//! What is left of the BUILD-SCRIPT kit the compiled user-code hosts shared (decision 0122): the
//! "this name moved / was renamed" hint, [`moved_name`], which the strategy builder appends to a
//! failed build of a strategy or research study FILE; the text edits that carry a file across an
//! SDK step's rename table, [`source_edits`] (decision 0296: `vike-cli strategy migrate` and
//! Studio's Apply); the edits that carry a file's `save_state`/`load_state` onto the state type of
//! `ABI_VERSION` 15, and the builder's hint for one that still has the JSON signature,
//! [`state_signature`]; the edits that carry a file's reads and writes of a bar's or a tick's `symbol`
//! onto `vike_marketdata::Symbol` (the SDK of 2026-11-20), and the builder's hint for one still on
//! `String`, [`row_symbol`]; and where a failed build's errors sit in the file, [`error_lines`]
//! (Studio's clickable list).
//!
//! The kit's build-script half (`driver`, `scan`, `render`, `watch`) is DELETED: its last host,
//! `vike-user-research`'s `build.rs`, went on 2026-11-15 when a research study became a FILE
//! (decision 0284), as the strategy host had gone with plan 2b (0281). Nothing compiles user code
//! into a vike binary any more, so nothing scans a user tier at build time.

pub mod error_lines;
pub mod moved_name;
pub mod row_symbol;
pub mod source_edits;
pub mod state_signature;
