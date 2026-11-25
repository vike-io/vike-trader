//! Runs of equal keys in an ordered `Vec`, cut to ONE element each: the LAST of the run.
//!
//! The one spelling of "ordered by key, the later of two equal keys wins" that the bar store's read,
//! its compaction, its in-memory double and the datahub's start history each used to spell for itself.

/// Keep the LAST element of each run of consecutive elements with an equal `key`, in place, the order
/// otherwise unchanged, and return how many were dropped.
///
/// `rows` must already hold equal keys next to each other — sorted by `key` with a STABLE sort, so the
/// last of a run is the one that came last before the sort.
pub fn keep_last_of_each_run<T, K: PartialEq>(rows: &mut Vec<T>, key: impl Fn(&T) -> K) -> usize {
    let before = rows.len();
    // `dedup_by` hands (later, kept) and removes `later` on `true`: moving the later one into the
    // kept slot first is what makes the LAST of the run survive.
    rows.dedup_by(|later, kept| {
        let same = key(later) == key(kept);
        if same {
            std::mem::swap(later, kept);
        }
        same
    });
    before - rows.len()
}

#[cfg(test)]
#[path = "runs_tests.rs"]
mod tests;
