//! The BUILD-SCRIPT mechanism of the two compiled user-code hosts, `vike-user-strategies` and
//! `vike-user-research`: scan a user_data tier at build time, render a registry into OUT_DIR, and
//! tell cargo to re-run exactly when what the scan reads changes.
//!
//! Both hosts used to carry it twice — two `build.rs` files equal line for line, one `watch.rs`
//! held byte-identical by a test, and the shared half of two `codegen.rs` files. It lives here
//! because neither host depends on the other, and a `build.rs` reaching into a sibling package's
//! `src/` would be an edge no manifest and no layer gate can see. A host takes it through
//! `[build-dependencies] vike-model`, a normal edge to `crates/vike-ops/tests/architecture/layer_gate.rs`
//! that goes DOWN.
//!
//! - [`watch`] — the `cargo:rerun-if-changed=` paths: only the directories the scan READS, an
//!   absent one through a symlink in OUT_DIR.
//! - [`scan`] — one tier's listing: sorted folders, the `<name>.rs` entry rule, the name charset,
//!   every objection as an error naming its path.
//! - [`render`] — the generated text both registries share (header, `#[path]` modules, a quoted
//!   name list). The resolver a host generates stays in that host.
//! - [`driver`] — the build script's body: resolve the scan root, print the watch, write the real
//!   and the fixture registry, fail the build on a malformed folder.
//!
//! ⚠ **No environment read and no clock, anywhere in it.** The build-time override is read in each
//! host's `build.rs` and handed to [`driver::run`] as a parameter: the settings-registry gate keys
//! every env read by `(name, crate)`, so a read here would move the override into this crate.
//! What stays in a host: its tier paths (`scanned_dirs`), its per-folder extras (the strategy
//! tier's `strategy.toml`, the research tier's both-tiers warning) and its generated resolver.

pub mod driver;
pub mod render;
pub mod scan;
pub mod watch;
