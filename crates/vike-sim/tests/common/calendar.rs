//! The UTC epoch-millisecond helper shared by this crate's test binaries.

use vike_util::calendar::days_from_civil;

/// Epoch-ms for a UTC instant, via the workspace's one civil-calendar math (`vike_util::calendar`).
pub fn ms(y: i64, mo: u32, d: u32, h: i64) -> i64 {
    days_from_civil(y, mo, d) * 86_400_000 + h * 3_600_000
}
