use super::*;

#[cfg(test)]
mod deployment;
#[cfg(test)]
mod rungs;
#[cfg(test)]
mod user_dir;
#[cfg(test)]
mod wiring;

/// A `repo_default` guaranteed NOT to exist on any machine — the INSTALLED shape, where the
/// compile-time path names a directory on whoever's box did the build. Spelled once, because
/// every rung below the hinge is only reachable when the hinge does not fire.
const NO_CHECKOUT: &str = "/definitely/not/a/real/build/machine/path/market_data/hist";

/// A private scratch directory under the system temp dir — this crate has no `tempfile`
/// dev-dependency, so the two deployment tests below make (and remove) their own.
struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let p = std::env::temp_dir().join(format!("vike-store-path-{tag}-{nanos}"));
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

/// A shaped environment map, as `std::env::vars().collect()` would produce it.
fn env(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs.iter().map(|(k, v)| ((*k).to_string(), (*v).to_string())).collect()
}
