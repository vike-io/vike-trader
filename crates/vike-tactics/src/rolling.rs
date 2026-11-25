//! Values strategy files read that vike's indicators do not give in the form their originals compute them. Copied from the
//! strategy gallery (plan 2b, C3).

use std::collections::VecDeque;

/// The sum of the last `length` numbers given, a NaN skipped and not counted, NaN until `length` have come. It is a running sum
/// with its rounding errors carried beside it (Neumaier): each number first takes out the one leaving the window, then goes in,
/// each a compensated add, and the sum read is the two added. That is how the reference keeps a rolling sum.
pub struct RollingSum {
    length: usize,
    window: VecDeque<f64>,
    sum: f64,
    carry: f64,
}

impl RollingSum {
    pub fn new(length: usize) -> Self {
        RollingSum { length, window: VecDeque::with_capacity(length + 1), sum: 0.0, carry: 0.0 }
    }

    pub fn next(&mut self, x: f64) -> f64 {
        if !x.is_nan() {
            if self.window.len() == self.length {
                let old = self.window.pop_front().expect("a full window");
                self.add(-old);
            }
            self.add(x);
            self.window.push_back(x);
        }
        if self.window.len() == self.length { self.sum + self.carry } else { f64::NAN }
    }

    fn add(&mut self, v: f64) {
        vike_math::summation::neumaier_add(&mut self.sum, &mut self.carry, v);
    }
}

/// A test that two lines crossed either way. It remembers the last bar on which one was more than `1e-10` above or below the
/// other, and fires when the other now lies strictly on the far side; nothing fires before such a bar, and a NaN bar changes
/// nothing. That is the reference's either-way cross, which differs from a cross up or down at an equality.
#[derive(Default)]
pub struct CrossEither {
    /// `1` last seen above, `-1` below, `0` not yet either.
    armed: i8,
}

impl CrossEither {
    pub fn step(&mut self, a: f64, b: f64) -> bool {
        if a.is_nan() || b.is_nan() {
            return false;
        }
        let crossed = (self.armed < 0 && a > b) || (self.armed > 0 && a < b);
        let diff = a - b;
        if diff < -1e-10 {
            self.armed = -1;
        } else if diff > 1e-10 {
            self.armed = 1;
        }
        crossed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_rolling_sum_and_a_cross_either_way() {
        let mut sum = RollingSum::new(3);
        let got: Vec<f64> = [1.0, f64::NAN, 2.0, 3.0, 4.0].iter().map(|&x| sum.next(x)).collect();
        assert!(got[..3].iter().all(|x| x.is_nan()), "{got:?}");
        assert_eq!(got[3..], [6.0, 9.0], "a NaN is skipped and the oldest leaves");
        let mut big = RollingSum::new(2);
        big.next(1e16);
        big.next(1.0);
        assert_eq!(big.next(1.0), 2.0, "the carried error survives the large value leaving");

        let mut cross = CrossEither::default();
        let fired: Vec<bool> = [(1.0, 1.0), (2.0, 1.0), (1.0, 1.0), (0.5, 1.0), (2.0, 1.0)]
            .iter()
            .map(|&(a, b)| cross.step(a, b))
            .collect();
        assert_eq!(
            fired,
            [false, false, false, true, true],
            "an equality keeps the side seen before it"
        );
    }
}
