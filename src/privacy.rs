//! A single privacy term for the cost function: how well my coins dissolve into
//! others' coins by amount, in [0,1]. Integer-only; depends only on the
//! [`CoinScore`] trait, never on counting machinery.

/// Exponents with |p| below this are treated as the geometric-mean limit (p = 0),
/// where the direct power-mean formula is numerically unstable.
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

/// A privacy term. [`PrivacyTerm::evaluate`] returns a value in `[0,1]`.
///
/// The value is a conservative lower bound (weight-of-evidence style), NOT a
/// privacy score or guarantee: it may only understate how well coins dissolve,
/// never overstate it. Higher = more dissolved = less linkable by amount.
#[derive(Clone, Debug)]
pub struct PrivacyTerm<S> {
    scorer: S,
    exponent: f64,
}

impl<S: CoinScore> PrivacyTerm<S> {
    /// How well `mine` dissolves into `theirs` by amount, in `[0,1]`.
    /// Generalized mean over each of my coins' per-coin scores.
    ///
    /// Monotone: adding a `theirs` amount never lowers the result (up to
    /// rounding). Adding one of `mine` is NOT guaranteed monotone — it inserts a
    /// new term into the mean, which can pull it down.
    ///
    /// # Panics
    ///
    /// Panics if the scorer returns a value outside `[0,1]` (including NaN).
    pub fn evaluate(&self, mine: &[u64], theirs: &[u64]) -> f64 {
        generalized_mean(
            mine.iter().map(|&c| {
                let s = self.scorer.score(c, theirs);
                assert!((0.0..=1.0).contains(&s), "score out of [0,1]: {s}");
                s
            }),
            self.exponent,
        )
    }
}

/// Builds a [`PrivacyTerm`]: sets the per-coin scorer and the power-mean
/// exponent (the strictness knob: `p -> -inf` = worst coin dominates).
#[derive(Clone, Debug)]
pub struct PrivacyTermBuilder<S> {
    scorer: S,
    exponent: f64,
}

impl<S: CoinScore> PrivacyTermBuilder<S> {
    /// Default exponent `-1.0` (harmonic mean: a poorly-covered coin drags the
    /// result down — conservative).
    pub fn new(scorer: S) -> Self {
        Self {
            scorer,
            exponent: -1.0,
        }
    }

    /// Sets the power-mean exponent; any non-NaN `f64` (including `±inf`) is a
    /// valid strictness setting.
    ///
    /// # Panics
    ///
    /// Panics if `p` is NaN.
    pub fn exponent(mut self, p: f64) -> Self {
        assert!(!p.is_nan(), "power-mean exponent must not be NaN");
        self.exponent = p;
        self
    }

    pub fn build(self) -> PrivacyTerm<S> {
        PrivacyTerm {
            scorer: self.scorer,
            exponent: self.exponent,
        }
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

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

    fn term() -> PrivacyTerm<PlaceholderScore> {
        PrivacyTermBuilder::new(PlaceholderScore).build()
    }

    #[test]
    fn evaluate_empty_mine_is_one() {
        assert_eq!(term().evaluate(&[], &[10, 20]), 1.0);
    }

    #[test]
    fn evaluate_single_coin_default_exponent() {
        // one coin, score = 0.5; harmonic mean of one value = that value
        assert!((term().evaluate(&[100], &[50]) - 0.5).abs() < 1e-9);
    }

    #[test]
    fn evaluate_two_equal_coins() {
        // both score 0.5; harmonic mean = 0.5
        assert!((term().evaluate(&[100, 100], &[50]) - 0.5).abs() < 1e-9);
    }

    #[test]
    fn exponent_override_changes_result() {
        let min_term = PrivacyTermBuilder::new(PlaceholderScore)
            .exponent(f64::NEG_INFINITY)
            .build();
        // scores: coin 100 -> 0.5 (pool {50}), coin 10 -> 0.0 (pool {50} has none <= 10)
        // min = 0.0
        assert_eq!(min_term.evaluate(&[100, 10], &[50]), 0.0);
    }

    #[test]
    #[should_panic(expected = "must not be NaN")]
    fn nan_exponent_is_rejected() {
        let _ = PrivacyTermBuilder::new(PlaceholderScore).exponent(f64::NAN);
    }

    struct OutOfRangeScore;

    impl CoinScore for OutOfRangeScore {
        fn score(&self, _coin: u64, _pool: &[u64]) -> f64 {
            f64::NAN
        }
    }

    #[test]
    #[should_panic(expected = "score out of [0,1]")]
    fn out_of_range_score_is_rejected() {
        let _ = PrivacyTermBuilder::new(OutOfRangeScore)
            .build()
            .evaluate(&[100], &[50]);
    }

    /// Small and large finite exponents, the geometric limit and both sides
    /// of its cutoff, plus the min/max limits, so the contract proptests
    /// cover every regime of the power mean.
    fn any_exponent() -> impl Strategy<Value = f64> {
        prop_oneof![
            6 => -3.0f64..3.0,
            2 => prop_oneof![-1e12f64..-3.0, 3.0f64..1e12],
            1 => Just(0.0),
            1 => -1e-5f64..1e-5,
            1 => Just(f64::NEG_INFINITY),
            1 => Just(f64::INFINITY),
        ]
    }

    proptest! {
        /// Guaranteed contract: adding a not-mine amount never lowers the
        /// term, whatever the exponent.
        #[test]
        fn adding_theirs_never_lowers(
            mine in prop::collection::vec(1u64..1_000_000, 0..8),
            theirs in prop::collection::vec(1u64..1_000_000, 0..12),
            extra in 1u64..1_000_000,
            p in any_exponent(),
        ) {
            let term = PrivacyTermBuilder::new(PlaceholderScore).exponent(p).build();
            let before = term.evaluate(&mine, &theirs);
            let mut theirs2 = theirs.clone();
            theirs2.push(extra);
            let after = term.evaluate(&mine, &theirs2);
            // Rounding slack only once score^p (>= 0.5^p) can leave f64 range.
            let slack = if p.is_finite() && p.abs() >= 1e3 { f64::EPSILON * before } else { 0.0 };
            prop_assert!(after >= before - slack, "p={p} before={before} after={after}");
        }

        /// Raising any one value never lowers the mean.
        #[test]
        fn mean_never_decreases(
            xs in prop::collection::vec(prop_oneof![Just(0.0), 1e-100f64..=1.0], 1..10),
            i in 0usize..10,
            bump in prop_oneof![0.0f64..1e-15, 0.0f64..1.0],
            p in prop_oneof![-3.0f64..3.0, Just(f64::NEG_INFINITY), Just(f64::INFINITY)],
        ) {
            let i = i % xs.len();
            let mut ys = xs.clone();
            ys[i] = (ys[i] + bump).min(1.0);
            let a = generalized_mean(xs.iter().copied(), p);
            let b = generalized_mean(ys.iter().copied(), p);
            prop_assert!(b >= a, "p={p} a={a} b={b}");
        }

        /// `evaluate` must stay in `[0,1]` across random inputs and exponents,
        /// including exponents near zero (the geometric-mean limit).
        #[test]
        fn evaluate_stays_in_unit_interval(
            mine in prop::collection::vec(1u64..1_000_000, 0..8),
            theirs in prop::collection::vec(1u64..1_000_000, 0..12),
            p in any_exponent(),
        ) {
            let term = PrivacyTermBuilder::new(PlaceholderScore).exponent(p).build();
            let v = term.evaluate(&mine, &theirs);
            prop_assert!((0.0..=1.0).contains(&v), "evaluate out of [0,1]: {v} (p={p})");
        }

        /// The mean never leaves `[min, max]`, even for clustered values.
        #[test]
        fn stays_within_min_max(
            base in 0.0f64..=1.0,
            spread in prop_oneof![Just(0.0), 0.0f64..1e-12, 0.0f64..1.0],
            ks in prop::collection::vec(0.0f64..=1.0, 1..12),
            p in prop_oneof![any_exponent(), -1e-3f64..1e-3],
        ) {
            let xs: Vec<f64> = ks.iter().map(|k| (base + k * spread).min(1.0)).collect();
            let lo = xs.iter().copied().fold(f64::INFINITY, f64::min);
            let hi = xs.iter().copied().fold(f64::NEG_INFINITY, f64::max);
            let m = generalized_mean(xs.iter().copied(), p);
            prop_assert!((lo..=hi).contains(&m), "p={p} lo={lo} hi={hi} m={m}");
        }

        /// Oracle: the single-pass implementation must agree with a naive
        /// two-pass version of the power-mean definition away from the geometric limit.
        #[test]
        fn matches_naive_power_mean(
            xs in prop::collection::vec(0.0f64..=1.0, 1..10),
            p in prop_oneof![-3.0f64..-0.01, 0.01f64..3.0],
        ) {
            let naive = if p < 0.0 && xs.iter().any(|&x| x <= 0.0) {
                0.0
            } else {
                let s: f64 = xs.iter().map(|&x| x.powf(p)).sum();
                (s / xs.len() as f64).powf(1.0 / p)
            };
            let fast = generalized_mean(xs.iter().copied(), p);
            prop_assert!((fast - naive).abs() < 1e-9, "p={p} fast={fast} naive={naive}");
        }

        /// Oracle: inside the geometric limit the log-space implementation
        /// must agree with the n-th root of the direct product.
        #[test]
        fn matches_naive_geometric_mean(
            xs in prop::collection::vec(prop_oneof![1 => Just(0.0), 9 => 0.0f64..=1.0], 1..10),
            p in prop_oneof![Just(0.0), -GEOMETRIC_EPS..GEOMETRIC_EPS],
        ) {
            let naive = xs.iter().product::<f64>().powf(1.0 / xs.len() as f64);
            let fast = generalized_mean(xs.iter().copied(), p);
            prop_assert!((fast - naive).abs() < 1e-9, "p={p} fast={fast} naive={naive}");
        }
    }
}
