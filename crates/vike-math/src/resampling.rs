//! Resampling: the one moving-block bootstrap draw.
//!
//! The generator stays the CALLER's (a seeded value it holds, rule 1 of this crate's module doc):
//! [`moving_block_indices`] asks it for each block's start through a closure, so a caller keeps its
//! own stream (`vike-analytics` draws with `rand::rngs::StdRng`) and the draw is the same calls in the
//! same order whichever crate runs it.

/// One moving-block bootstrap draw of `n` indices into a series of `n`: `n.div_ceil(block_length)`
/// contiguous blocks of `block_length` indices, each starting at `next_start(n - block_length)` (the
/// caller draws a start in `0..=max_start`), concatenated and cut to `n`. Kunsch's construction:
/// fixed-length blocks, no wrapping. `out` is cleared first and reused, so a caller drawing many
/// resamples allocates once. Requires `1 <= block_length <= n`.
pub fn moving_block_indices(
    n: usize,
    block_length: usize,
    mut next_start: impl FnMut(usize) -> usize,
    out: &mut Vec<usize>,
) {
    out.clear();
    for _ in 0..n.div_ceil(block_length) {
        let start = next_start(n - block_length);
        out.extend(start..start + block_length);
    }
    out.truncate(n);
}

#[cfg(test)]
mod tests {
    use super::moving_block_indices;

    /// Three blocks of two from a series of five, cut to five: the starts the closure handed out, in
    /// order, and the largest start it was asked for is `n - block_length`.
    #[test]
    fn the_blocks_are_the_starts_handed_out_cut_to_n() {
        let mut starts = [3, 0, 1].into_iter();
        let mut asked = Vec::new();
        let mut out = vec![99];
        moving_block_indices(
            5,
            2,
            |max_start| {
                asked.push(max_start);
                starts.next().unwrap()
            },
            &mut out,
        );
        assert_eq!(out, [3, 4, 0, 1, 1]);
        assert_eq!(asked, [3, 3, 3]);
    }
}
