//! Shared test fixtures: the seeded random-walk bars, the two Rhai strategies and [`write_file`]. Compiled ONCE by the
//! lib — as `crate::test_support` for the unit tests and, behind `test-support` (only the self
//! dev-dependency turns it on), as `vike_studio_core::test_support` for the integration binaries,
//! which used to `#[path]`-include each file.

pub mod cross_script;
pub mod sweep_script;
pub mod walk;

/// Write `text` to `path`, creating its parent directories: how the listing and user-strategy suites
/// lay a fixture tree on disk.
pub fn write_file(path: &std::path::Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}
