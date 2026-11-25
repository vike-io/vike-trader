//! Tests for `vike_model::host_build`: the moved-name hint (its golden messages in the
//! `host_build/moved_name.rs` child, the renamed rows' targets in `host_build/renamed_targets.rs`),
//! the SDK step's text edits (`host_build/source_edits.rs`), the state signature's edits and hint of
//! `ABI_VERSION` 15 (`host_build/state_signature.rs`), the rows' symbol type of the SDK of 2026-11-20
//! (`host_build/row_symbol.rs`) and a failed build's error lines
//! (`host_build/error_lines.rs`). The kit's build-script half and its tests (the tier scan, the
//! render, the driver, the rerun watch) were deleted with the last compiled user-code host
//! (decision 0284).

#[path = "host_build/error_lines.rs"]
mod error_lines;
#[path = "host_build/moved_name.rs"]
mod moved_name;
#[path = "host_build/renamed_targets.rs"]
mod renamed_targets;
#[path = "host_build/row_symbol.rs"]
mod row_symbol;
#[path = "host_build/source_edits.rs"]
mod source_edits;
#[path = "host_build/state_signature.rs"]
mod state_signature;
