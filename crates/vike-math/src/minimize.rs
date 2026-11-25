//! One derivative-free minimiser, the Nelder–Mead downhill simplex, for the fits that have no
//! gradient written down (`vike-analytics`' GARCH likelihood, `vike-options`' SVI smile). //! once, here on the formula floor so a crate at any layer reaches it: a fit that needs a minimiser
//! calls this one (moved from `vike-analytics`' `minimize`, B4, byte for byte below this paragraph).
//!
//! Sources: J. A. Nelder and R. Mead, "A Simplex Method for Function Minimization", The Computer
//! Journal 7 (1965) 308-313, <https://doi.org/10.1093/comjnl/7.4.308>, with the standard
//! coefficients (reflection 1, expansion 2, contraction 1/2, shrink 1/2) and the order of the steps
//! in J. C. Lagarias, J. A. Reeds, M. H. Wright and P. E. Wright, "Convergence Properties of the
//! Nelder-Mead Simplex Method in Low Dimensions", SIAM Journal on Optimization 9 (1998) 112-147,
//! <https://doi.org/10.1137/S1052623496303470>.
//!
//! A `NaN` objective counts as `+inf` (a point outside the model's domain), so the simplex walks away
//! from it. Deterministic: no randomness, and a tie between two vertices keeps their order.

/// Where [`nelder_mead`] stopped.
#[derive(Debug, Clone, PartialEq)]
pub struct Minimum {
    /// The best vertex.
    pub point: Vec<f64>,
    /// The objective there.
    pub value: f64,
    /// How many times the objective was evaluated.
    pub evaluations: usize,
    /// Whether the simplex met `tolerance` before `max_evaluations` ran out.
    pub converged: bool,
}

/// Minimise `objective` from `start`, the first simplex `start` plus `step` along each axis in turn.
/// Stops when the vertices' values lie within `tolerance` of the best AND every vertex lies within
/// `tolerance` of the best in every coordinate, or after `max_evaluations` evaluations.
pub fn nelder_mead(
    mut objective: impl FnMut(&[f64]) -> f64,
    start: &[f64],
    step: f64,
    tolerance: f64,
    max_evaluations: usize,
) -> Minimum {
    let mut evaluations = 0usize;
    let mut value_at = |x: &[f64], evaluations: &mut usize| {
        *evaluations += 1;
        let v = objective(x);
        if v.is_nan() { f64::INFINITY } else { v }
    };
    let n = start.len();
    let mut simplex: Vec<(Vec<f64>, f64)> = Vec::with_capacity(n + 1);
    simplex.push((start.to_vec(), value_at(start, &mut evaluations)));
    for axis in 0..n {
        let mut vertex = start.to_vec();
        vertex[axis] += step;
        let v = value_at(&vertex, &mut evaluations);
        simplex.push((vertex, v));
    }
    // `along(a, b, t)` is `a + t (b - a)`, coordinate by coordinate.
    let along = |a: &[f64], b: &[f64], t: f64| -> Vec<f64> {
        a.iter().zip(b).map(|(&x, &y)| x + t * (y - x)).collect()
    };
    let mut converged = false;
    loop {
        simplex.sort_by(|a, b| a.1.total_cmp(&b.1));
        let best = simplex[0].clone();
        let spread = simplex[n].1 - best.1;
        let reach = simplex.iter().fold(0.0_f64, |m, (v, _)| {
            v.iter().zip(&best.0).fold(m, |m, (&x, &y)| m.max((x - y).abs()))
        });
        if spread <= tolerance && reach <= tolerance {
            converged = true;
            break;
        }
        if n == 0 || evaluations >= max_evaluations {
            break;
        }
        let mut centroid = vec![0.0; n];
        for (vertex, _) in &simplex[..n] {
            for (c, &x) in centroid.iter_mut().zip(vertex) {
                *c += x;
            }
        }
        for c in &mut centroid {
            *c /= n as f64;
        }
        let (worst, worst_value) = simplex[n].clone();
        let second_worst_value = simplex[n - 1].1;
        let reflected = along(&centroid, &worst, -1.0);
        let reflected_value = value_at(&reflected, &mut evaluations);
        let accepted = if reflected_value < best.1 {
            let expanded = along(&centroid, &worst, -2.0);
            let expanded_value = value_at(&expanded, &mut evaluations);
            Some(if expanded_value < reflected_value {
                (expanded, expanded_value)
            } else {
                (reflected, reflected_value)
            })
        } else if reflected_value < second_worst_value {
            Some((reflected, reflected_value))
        } else if reflected_value < worst_value {
            let outside = along(&centroid, &reflected, 0.5);
            let outside_value = value_at(&outside, &mut evaluations);
            (outside_value <= reflected_value).then_some((outside, outside_value))
        } else {
            let inside = along(&centroid, &worst, 0.5);
            let inside_value = value_at(&inside, &mut evaluations);
            (inside_value < worst_value).then_some((inside, inside_value))
        };
        match accepted {
            Some(vertex) => simplex[n] = vertex,
            None => {
                for vertex in simplex.iter_mut().skip(1) {
                    let shrunk = along(&best.0, &vertex.0, 0.5);
                    let v = value_at(&shrunk, &mut evaluations);
                    *vertex = (shrunk, v);
                }
            }
        }
    }
    let (point, value) = simplex.swap_remove(0);
    Minimum { point, value, evaluations, converged }
}

#[cfg(test)]
mod tests {
    use super::nelder_mead;

    /// Rosenbrock's valley, `(1 - x)^2 + 100 (y - x^2)^2`, the classic hard case: its minimum 0 sits at
    /// `(1, 1)` at the end of a long curved floor.
    #[test]
    fn finds_the_bottom_of_rosenbrocks_valley() {
        let rosenbrock = |p: &[f64]| {
            let (a, b) = (1.0 - p[0], p[1] - p[0] * p[0]);
            a * a + 100.0 * b * b
        };
        let m = nelder_mead(rosenbrock, &[-1.2, 1.0], 0.5, 1e-10, 10_000);
        assert!(m.converged, "{m:?}");
        assert!((m.point[0] - 1.0).abs() < 1e-4 && (m.point[1] - 1.0).abs() < 1e-4, "{m:?}");
        assert!(m.value < 1e-8, "{m:?}");
    }

    /// A quadratic bowl in three dimensions, and the same answer twice (no randomness).
    #[test]
    fn a_bowl_is_found_and_the_walk_is_deterministic() {
        let bowl = |p: &[f64]| {
            let d = [p[0] - 3.0, p[1] + 2.0, p[2] - 0.5];
            d[0] * d[0] + 4.0 * d[1] * d[1] + 9.0 * d[2] * d[2]
        };
        let first = nelder_mead(bowl, &[0.0, 0.0, 0.0], 1.0, 1e-12, 20_000);
        let second = nelder_mead(bowl, &[0.0, 0.0, 0.0], 1.0, 1e-12, 20_000);
        assert_eq!(first, second);
        for (got, want) in first.point.iter().zip([3.0, -2.0, 0.5]) {
            assert!((got - want).abs() < 1e-5, "{first:?}");
        }
    }

    /// A `NaN` region is walked away from, and the budget is a hard stop.
    #[test]
    fn nan_is_a_wall_and_the_budget_stops_the_walk() {
        let walled = |p: &[f64]| if p[0] < 0.0 { f64::NAN } else { (p[0] - 2.0) * (p[0] - 2.0) };
        let m = nelder_mead(walled, &[0.5], 1.0, 1e-12, 5_000);
        assert!((m.point[0] - 2.0).abs() < 1e-5, "{m:?}");
        let short = nelder_mead(walled, &[0.5], 1.0, 1e-12, 5);
        assert!(!short.converged && short.evaluations <= 7, "{short:?}");
    }
}
