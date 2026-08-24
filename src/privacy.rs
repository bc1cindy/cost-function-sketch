//! A single privacy term for the cost function: how well my coins dissolve into
//! others' coins by amount, in [0,1]. Integer-only; depends only on the
//! [`CoinScore`] trait, never on counting machinery.

/// Exponents with |p| below this are treated as the geometric-mean limit (p = 0),
/// where the direct power-mean formula is numerically unstable.
#[allow(dead_code)]
const GEOMETRIC_EPS: f64 = 1e-6;

/// Power mean of the values yielded by `xs` (each expected in `[0,1]`) with exponent `p`.
///
/// - empty `xs` -> `1.0` (nothing of mine to link)
/// - `p` near `0.0` (within [`GEOMETRIC_EPS`]) -> geometric mean; the direct
///   `(s/n).powf(1.0/p)` formula is numerically unstable as `p -> 0`
/// - `p == f64::NEG_INFINITY` -> min; `p == f64::INFINITY` -> max
/// - any zero in `xs` with `p <= 0.0` -> `0.0`
///
/// Always returns a value in `[min(xs), max(xs)]`.
#[allow(dead_code)]
pub(crate) fn generalized_mean(xs: impl IntoIterator<Item = f64>, p: f64) -> f64 {
    debug_assert!(!p.is_nan(), "power-mean exponent must not be NaN");
    let iter = xs.into_iter();

    if p == f64::INFINITY {
        let mut count = 0usize;
        let mut max = f64::NEG_INFINITY;
        for x in iter {
            count += 1;
            max = max.max(x);
        }
        return if count == 0 { 1.0 } else { max };
    }

    if p == f64::NEG_INFINITY {
        let mut count = 0usize;
        let mut min = f64::INFINITY;
        for x in iter {
            count += 1;
            min = min.min(x);
        }
        return if count == 0 { 1.0 } else { min };
    }

    if p.abs() < GEOMETRIC_EPS {
        // Log space: the direct product of many values in [0,1] underflows to 0.
        let mut count = 0usize;
        let mut any_zero = false;
        let mut sum_ln = 0.0f64;
        let (mut lo, mut hi) = (f64::INFINITY, f64::NEG_INFINITY);
        for x in iter {
            count += 1;
            lo = lo.min(x);
            hi = hi.max(x);
            if x <= 0.0 {
                any_zero = true;
            } else {
                sum_ln += x.ln();
            }
        }
        if count == 0 {
            return 1.0;
        }
        if any_zero {
            return 0.0;
        }
        return (sum_ln / count as f64).exp().max(lo).min(hi);
    }

    // Direct sum of x^p unless it leaves f64 range; then factor out the
    // extreme term m so each (x / m)^p stays in [0, 1].
    let mut count = 0usize;
    let mut any_zero = false;
    let mut direct = 0.0f64;
    let mut m = 0.0f64;
    let mut sum = 0.0f64;
    let (mut lo, mut hi) = (f64::INFINITY, f64::NEG_INFINITY);
    for x in iter {
        count += 1;
        lo = lo.min(x);
        hi = hi.max(x);
        if p < 0.0 && x <= 0.0 {
            any_zero = true;
            continue;
        }
        direct += x.powf(p);
        let more_extreme = if p > 0.0 { x > m } else { m == 0.0 || x < m };
        if more_extreme {
            sum = if m > 0.0 { sum * (m / x).powf(p) } else { 0.0 } + 1.0;
            m = x;
        } else if m > 0.0 {
            sum += (x / m).powf(p);
        }
    }
    if count == 0 {
        return 1.0;
    }
    if any_zero || m == 0.0 {
        return 0.0;
    }
    let n = count as f64;
    let in_range = if p < 0.0 {
        direct.is_finite()
    } else {
        direct / n >= f64::MIN_POSITIVE
    };
    let mean = if in_range {
        (direct / n).powf(1.0 / p)
    } else {
        m * (sum / n).powf(1.0 / p)
    };
    mean.max(lo).min(hi)
}

/// Per-coin score, pluggable. Placeholder now; the radix backend (in the
/// separate `privacy-metrics` crate) implements this later.
///
/// The value is a conservative measure, not a privacy guarantee.
pub trait CoinScore {
    /// Score of `coin` given the `pool` of not-mine amounts, in `[0,1]`.
    ///
    /// Contract: monotone non-decreasing in `pool` — adding a pool amount never
    /// lowers the value.
    fn score(&self, coin: u64, pool: &[u64]) -> f64;
}

/// Deliberately crude stand-in: NOT a real cover measure. Its only jobs are to
/// be in `[0,1]` and monotone in `pool`, so the term's shape and the
/// monotonicity proptest are exercised. Replaced by the radix backend later.
#[derive(Clone, Copy, Debug)]
pub struct PlaceholderScore;

impl CoinScore for PlaceholderScore {
    fn score(&self, coin: u64, pool: &[u64]) -> f64 {
        let k = pool.iter().filter(|&&p| p <= coin).count() as i32;
        1.0 - 2f64.powi(-k)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arithmetic_mean() {
        let m = generalized_mean([0.2, 0.4, 0.6], 1.0);
        assert!((m - 0.4).abs() < 1e-9, "got {m}");
    }

    #[test]
    fn geometric_mean_of_quarter_and_one() {
        // sqrt(0.25 * 1.0) = 0.5
        let m = generalized_mean([0.25, 1.0], 0.0);
        assert!((m - 0.5).abs() < 1e-9, "got {m}");
    }

    #[test]
    fn harmonic_mean() {
        // 2 / (1/1 + 1/0.5) = 2/3
        let m = generalized_mean([1.0, 0.5], -1.0);
        assert!((m - (2.0 / 3.0)).abs() < 1e-9, "got {m}");
    }

    #[test]
    fn min_and_max_limits() {
        assert!((generalized_mean([0.3, 0.7], f64::NEG_INFINITY) - 0.3).abs() < 1e-9);
        assert!((generalized_mean([0.3, 0.7], f64::INFINITY) - 0.7).abs() < 1e-9);
    }

    #[test]
    fn empty_is_one() {
        assert_eq!(generalized_mean([], -1.0), 1.0);
    }

    #[test]
    fn zero_with_nonpositive_p_is_zero() {
        assert_eq!(generalized_mean([0.0, 0.5], -1.0), 0.0);
        assert_eq!(generalized_mean([0.0, 0.5], 0.0), 0.0);
    }

    #[test]
    fn stays_within_bounds() {
        let xs = [0.2, 0.5, 0.9];
        for &p in &[-2.0, -1.0, 0.0, 1.0, 3.0] {
            let m = generalized_mean(xs, p);
            assert!((0.2..=0.9).contains(&m), "p={p} m={m}");
        }
    }

    #[test]
    fn extreme_exponents_approach_min_and_max() {
        assert!((generalized_mean([0.5, 0.9], -1e10) - 0.5).abs() < 1e-9);
        assert!((generalized_mean([0.5, 0.9], 1e10) - 0.9).abs() < 1e-9);
        for (xs, p) in [([1e-5, 0.9], -70.0), ([0.01, 0.02], 200.0)] {
            let m = generalized_mean(xs, p);
            assert!((xs[0]..=xs[1]).contains(&m), "p={p} m={m}");
        }
    }

    #[test]
    fn placeholder_empty_pool_is_zero() {
        assert_eq!(PlaceholderScore.score(100, &[]), 0.0);
    }

    #[test]
    fn placeholder_grows_with_eligible_pool() {
        assert!((PlaceholderScore.score(100, &[50]) - 0.5).abs() < 1e-9);
        assert!((PlaceholderScore.score(100, &[50, 60]) - 0.75).abs() < 1e-9);
    }

    #[test]
    fn placeholder_ignores_too_large_pool_amounts() {
        // 200 > 100, not eligible -> k stays 0 -> score 0
        assert_eq!(PlaceholderScore.score(100, &[200]), 0.0);
    }

    #[test]
    fn placeholder_monotone_in_pool() {
        let base = PlaceholderScore.score(100, &[30, 40]);
        for extra in [10u64, 100, 500] {
            let mut pool = vec![30u64, 40];
            pool.push(extra);
            assert!(PlaceholderScore.score(100, &pool) >= base - 1e-12);
        }
    }
}
