use super::*;

use std::path::{Path, PathBuf};

/// A private scratch directory under the system temp dir — this crate has no `tempfile`
/// dev-dependency and Phase 2 adds no dependency, so the three filesystem tests below make
/// (and remove) their own unique directory.
struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let p = std::env::temp_dir().join(format!("vike-state-path-{tag}-{nanos}"));
        std::fs::create_dir_all(&p).expect("scratch dir");
        Self(p)
    }
    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[cfg(test)]
mod data_bin_tmp;
#[cfg(test)]
mod project_walk;
#[cfg(test)]
mod state_and_log;
#[cfg(test)]
mod state_files;
#[cfg(test)]
mod user_data;
#[cfg(test)]
mod user_roots;
