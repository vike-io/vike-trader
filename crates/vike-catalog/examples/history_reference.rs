//! **The generator for the history-channels reference page.** Writes the page to stdout:
//!
//! ```sh
//! cargo run -q -p vike-catalog --example history_reference > docs/reference/history-channels.md
//! ```
//!
//! It is `vike_catalog::history_reference` and nothing else — the renderer lives beside the table so
//! the CLI and the page cannot word one row differently, and this is the one door that turns it into
//! a file. It prints rather than writes on purpose: a box that cannot build the workspace regenerates
//! the page through a remote lane, and a lane's stdout comes back where a file it wrote does not.
//! Read the diff before committing it — `crates/vike-ops/tests/venues/history_channels_gate.rs`'s
//! `the_page_equals_its_render` exists to make a table change visible, and a generator run without
//! reading its output turns that gate into a rubber stamp.

fn main() {
    print!("{}", vike_catalog::history_reference());
}
