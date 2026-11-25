//! The captured IG fixtures (`tests/fixtures/<name>.json`) the close suites replay, read one way.

use serde_json::Value;

/// The captured fixture `name`, parsed; a missing or malformed file is a loud failure.
pub fn fixture(name: &str) -> Value {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(format!("{name}.json"));
    let body = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("missing captured fixture {}: {e}", path.display()));
    serde_json::from_str(&body)
        .unwrap_or_else(|e| panic!("malformed captured fixture {}: {e}", path.display()))
}
