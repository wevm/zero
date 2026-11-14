//! A t-digest, for the inspector's percentiles (`TDigest` in
//! `packages/shared/src/tdigest.ts`, itself a port of
//! <https://github.com/influxdata/tdigest>, Apache License 2.0).
//!
//! The port does the same arithmetic in the same order as JS, so a digest
//! fed the same values gives JS's quantiles exactly, and reads and writes
//! the same JSON: `[compression, mean, weight, mean, weight, ...]`, which is
//! how zero-cache sends its metrics.

use crate::json::Value;
use std::cell::RefCell;

/// Initial capacity of the buffer of unprocessed values.
const INITIAL_CAPACITY: usize = 16;

/// A digest of a distribution of numbers, for its quantiles: it keeps
/// roughly `compression` centroids, and buffers up to eight times as many
/// values between merges.
///
/// Reading it (`quantile`, `count`, `to_json`, ...) merges the buffered
/// values first, as in JS, so those take `&self`.
#[derive(Clone, Debug)]
pub struct TDigest {
    compression: f64,
    max_unprocessed: f64,
    state: RefCell<State>,
}

#[derive(Clone, Debug)]
struct State {
    /// Means of the processed centroids, ascending.
    means: Vec<f64>,
    /// Weights of the processed centroids, parallel to `means`.
    weights: Vec<f64>,
    /// The values added since the last `process`, in order.
    unprocessed_means: Vec<f64>,
    unprocessed_weights: Vec<f64>,
    cumulative: Vec<f64>,
    processed_weight: f64,
    unprocessed_weight: f64,
    min: f64,
    max: f64,
}

impl Default for TDigest {
    /// A digest of compression 100, JS's default.
    fn default() -> Self {
        TDigest::new(100.0)
    }
}

impl PartialEq for TDigest {
    /// Digests are equal when their JSON forms are.
    fn eq(&self, other: &Self) -> bool {
        self.to_vec() == other.to_vec()
    }
}

/// `Math.max`: NaN if either is, and +0 over -0.
fn js_max(a: f64, b: f64) -> f64 {
    if a.is_nan() || b.is_nan() {
        return f64::NAN;
    }
    if a == b {
        return if a.is_sign_negative() { b } else { a };
    }
    if a > b { a } else { b }
}

/// `Math.min`: NaN if either is, and -0 over +0.
fn js_min(a: f64, b: f64) -> f64 {
    if a.is_nan() || b.is_nan() {
        return f64::NAN;
    }
    if a == b {
        return if a.is_sign_negative() { a } else { b };
    }
    if a < b { a } else { b }
}

/// `binarySearch` in `packages/shared/src/binary-search.ts`.
fn binary_search(mut high: usize, compare: impl Fn(usize) -> f64) -> usize {
    let mut low = 0;
    while low < high {
        let mid = low + ((high - low) >> 1);
        let i = compare(mid);
        if i == 0.0 {
            return mid;
        }
        if i > 0.0 {
            low = mid + 1;
        } else {
            high = mid;
        }
    }
    low
}

fn weighted_average(x1: f64, w1: f64, x2: f64, w2: f64) -> f64 {
    if x1 <= x2 {
        weighted_average_sorted(x1, w1, x2, w2)
    } else {
        weighted_average_sorted(x2, w2, x1, w1)
    }
}

fn weighted_average_sorted(x1: f64, w1: f64, x2: f64, w2: f64) -> f64 {
    let x = (x1 * w1 + x2 * w2) / (w1 + w2);
    js_max(x1, js_min(x, x2))
}

impl TDigest {
    /// An empty digest. JS's default compression, 100, keeps p50, p95 and
    /// p99 within about 0.5%.
    pub fn new(compression: f64) -> Self {
        let max_unprocessed = compression.ceil() * 8.0;
        let capacity = if max_unprocessed + 1.0 < INITIAL_CAPACITY as f64 {
            (max_unprocessed + 1.0).max(0.0) as usize
        } else {
            INITIAL_CAPACITY
        };
        TDigest {
            compression,
            max_unprocessed,
            state: RefCell::new(State {
                means: Vec::new(),
                weights: Vec::new(),
                unprocessed_means: Vec::with_capacity(capacity),
                unprocessed_weights: Vec::with_capacity(capacity),
                cumulative: Vec::new(),
                processed_weight: 0.0,
                unprocessed_weight: 0.0,
                min: f64::MAX,
                max: -f64::MAX,
            }),
        }
    }

    /// The digest of its JSON form, `[compression, mean, weight, ...]`
    /// (`TDigest.fromJSON`).
    pub fn from_json(data: &[f64]) -> Result<Self, String> {
        let mut digest = TDigest::new(data.first().copied().unwrap_or(100.0));
        if data.len() % 2 != 1 {
            return Err("Invalid centroids array".into());
        }
        for pair in data[1..].chunks(2) {
            digest.add(pair[0], pair[1]);
        }
        Ok(digest)
    }

    /// [`from_json`](Self::from_json) of a JSON value: an array of numbers.
    pub fn from_json_value(v: &Value) -> Result<Self, String> {
        let Some(a) = v.as_array() else {
            return Err("Expected a TDigest array".into());
        };
        let numbers: Option<Vec<f64>> = a.iter().map(Value::as_f64).collect();
        let numbers = numbers.ok_or("Expected a TDigest array of numbers")?;
        TDigest::from_json(&numbers)
    }

    /// The compression the digest was made with.
    pub fn compression(&self) -> f64 {
        self.compression
    }

    /// Empties the digest.
    pub fn reset(&mut self) {
        let s = self.state.get_mut();
        s.means.clear();
        s.weights.clear();
        s.unprocessed_means.clear();
        s.unprocessed_weights.clear();
        s.cumulative.clear();
        s.processed_weight = 0.0;
        s.unprocessed_weight = 0.0;
        s.min = f64::MAX;
        s.max = -f64::MAX;
    }

    /// Adds a value with a weight (1 in JS by default). A weight that isn't
    /// positive and finite is ignored, as is a NaN mean.
    pub fn add(&mut self, mean: f64, weight: f64) {
        if mean.is_nan() || weight <= 0.0 || !weight.is_finite() {
            return;
        }
        let process = {
            let s = self.state.get_mut();
            s.unprocessed_means.push(mean);
            s.unprocessed_weights.push(weight);
            s.unprocessed_weight += weight;
            s.unprocessed_means.len() as f64 > self.max_unprocessed
        };
        if process {
            Self::process(self.compression, self.state.get_mut());
        }
    }

    /// Adds each of `other`'s centroids (`merge`).
    pub fn merge(&mut self, other: &TDigest) {
        let (means, weights) = {
            let mut s = other.state.borrow_mut();
            Self::process(other.compression, &mut s);
            (s.means.clone(), s.weights.clone())
        };
        for (m, w) in means.into_iter().zip(weights) {
            self.add(m, w);
        }
    }

    /// The processed centroids, as `(mean, weight)`, ascending.
    pub fn centroids(&self) -> Vec<(f64, f64)> {
        let s = self.processed();
        s.means
            .iter()
            .copied()
            .zip(s.weights.iter().copied())
            .collect()
    }

    /// The total weight of the values added.
    pub fn count(&self) -> f64 {
        self.processed().processed_weight
    }

    /// The JSON form, `[compression, mean, weight, ...]` (`toJSON`).
    pub fn to_vec(&self) -> Vec<f64> {
        let s = self.processed();
        let mut data = Vec::with_capacity(1 + 2 * s.means.len());
        data.push(self.compression);
        for (m, w) in s.means.iter().zip(&s.weights) {
            data.push(*m);
            data.push(*w);
        }
        data
    }

    /// [`to_vec`](Self::to_vec) as a JSON value.
    pub fn to_json(&self) -> Value {
        Value::Array(self.to_vec().into_iter().map(Value::Number).collect())
    }

    fn processed(&self) -> std::cell::RefMut<'_, State> {
        let mut s = self.state.borrow_mut();
        Self::process(self.compression, &mut s);
        s
    }

    /// Merges the unprocessed values into the centroids: walks both in
    /// order of mean, folding each into the last centroid while that stays
    /// within its size limit, and starting a new centroid otherwise.
    fn process(compression: f64, s: &mut State) {
        if s.unprocessed_means.is_empty() {
            return;
        }
        let (new_means, new_weights) = sorted_unprocessed(s);
        s.unprocessed_means.clear();
        s.unprocessed_weights.clear();
        let n = new_means.len();
        let p = s.means.len();
        s.processed_weight += s.unprocessed_weight;
        s.unprocessed_weight = 0.0;
        let total = s.processed_weight;

        let mut means = Vec::with_capacity(p + 1);
        let mut weights = Vec::with_capacity(p + 1);
        let mut mean = 0.0;
        let mut weight = 0.0;
        let mut so_far = 0.0;
        let mut limit = 0.0;
        let (mut i, mut j) = (0, 0);
        while i < n || j < p {
            // On a tie the new value goes first.
            let (m, w);
            if i < n && (j == p || new_means[i] <= s.means[j]) {
                m = new_means[i];
                w = new_weights[i];
                i += 1;
            } else {
                m = s.means[j];
                w = s.weights[j];
                j += 1;
            }
            if so_far + w <= limit {
                weight += w;
                mean += (w * (m - mean)) / weight;
            } else {
                if weight > 0.0 {
                    means.push(mean);
                    weights.push(weight);
                }
                let k = integrated_location(compression, so_far / total);
                limit = total * integrated_q(compression, k + 1.0);
                mean = m;
                weight = w;
            }
            so_far += w;
        }
        means.push(mean);
        weights.push(weight);

        s.min = js_min(s.min, means[0]);
        s.max = js_max(s.max, *means.last().expect("a centroid"));
        s.means = means;
        s.weights = weights;
    }

    fn update_cumulative(s: &mut State) {
        // Weight only increases, so an unchanged total means nothing
        // changed since the last update.
        if s.cumulative.last() == Some(&s.processed_weight) {
            return;
        }
        s.cumulative.resize(s.means.len() + 1, 0.0);
        let mut prev = 0.0;
        for i in 0..s.means.len() {
            let cur = s.weights[i];
            s.cumulative[i] = prev + cur / 2.0;
            prev += cur;
        }
        let n = s.means.len();
        s.cumulative[n] = prev;
    }

    /// The (approximate) value at quantile `q`, between 0 and 1. NaN when
    /// the digest is empty or `q` is out of range.
    pub fn quantile(&self, q: f64) -> f64 {
        let mut s = self.processed();
        Self::update_cumulative(&mut s);
        if q < 0.0 || q > 1.0 || s.means.is_empty() {
            return f64::NAN;
        }
        if s.means.len() == 1 {
            return s.means[0];
        }
        if q.is_nan() {
            // JS reads past the arrays and gets NaN.
            return f64::NAN;
        }
        let index = q * s.processed_weight;
        if index <= s.weights[0] / 2.0 {
            return s.min + ((2.0 * index) / s.weights[0]) * (s.means[0] - s.min);
        }
        let lower = binary_search(s.cumulative.len(), |i| -s.cumulative[i] + index);
        if lower >= s.cumulative.len() {
            // The sum of the weights rounded below `index`: JS reads past
            // the arrays and gets NaN.
            return f64::NAN;
        }
        if lower + 1 != s.cumulative.len() {
            let z1 = index - s.cumulative[lower - 1];
            let z2 = s.cumulative[lower] - index;
            return weighted_average(s.means[lower - 1], z2, s.means[lower], z1);
        }
        let z1 = index - s.processed_weight - s.weights[lower - 1] / 2.0;
        let z2 = s.weights[lower - 1] / 2.0 - z1;
        weighted_average(*s.means.last().expect("a centroid"), z1, s.max, z2)
    }

    /// The (approximate) fraction of the values at or below `x`.
    pub fn cdf(&self, x: f64) -> f64 {
        let mut s = self.processed();
        Self::update_cumulative(&mut s);
        match s.means.len() {
            0 => return 0.0,
            1 => {
                let width = s.max - s.min;
                if x <= s.min {
                    return 0.0;
                }
                if x >= s.max {
                    return 1.0;
                }
                if x - s.min <= width {
                    // Too close together to interpolate.
                    return 0.5;
                }
                return (x - s.min) / width;
            }
            _ => {}
        }
        if x <= s.min {
            return 0.0;
        }
        if x >= s.max {
            return 1.0;
        }
        let m0 = s.means[0];
        if x <= m0 {
            if m0 - s.min > 0.0 {
                return (((x - s.min) / (m0 - s.min)) * s.weights[0]) / s.processed_weight / 2.0;
            }
            return 0.0;
        }
        let mn = *s.means.last().expect("a centroid");
        if x >= mn {
            if s.max - mn > 0.0 {
                return 1.0
                    - (((s.max - x) / (s.max - mn)) * s.weights.last().expect("a centroid"))
                        / s.processed_weight
                        / 2.0;
            }
            return 1.0;
        }
        // Equal counts as greater, for the upper index.
        let upper = binary_search(s.means.len(), |i| {
            let d = x - s.means[i];
            if d == 0.0 || d.is_nan() { 1.0 } else { d }
        });
        let z1 = x - s.means[upper - 1];
        let z2 = s.means[upper] - x;
        weighted_average(s.cumulative[upper - 1], z2, s.cumulative[upper], z1) / s.processed_weight
    }
}

/// The unprocessed means and weights, sorted by mean, as JS sorts them.
fn sorted_unprocessed(s: &State) -> (Vec<f64>, Vec<f64>) {
    let means = &s.unprocessed_means;
    let weights = &s.unprocessed_weights;
    // `fromJSON` and `merge` add centroids in order of mean.
    if means
        .windows(2)
        .all(|w| w[1].partial_cmp(&w[0]) != Some(std::cmp::Ordering::Less))
    {
        return (means.clone(), weights.clone());
    }
    // With one weight for every value, a numeric sort of the means alone
    // (`Float64Array.prototype.sort`: -0 before +0).
    if weights.iter().all(|w| *w == weights[0]) {
        let mut sorted = means.clone();
        sorted.sort_by(f64::total_cmp);
        return (sorted, weights.clone());
    }
    // A stable sort by `means[a] - means[b]`.
    let mut order: Vec<usize> = (0..means.len()).collect();
    order.sort_by(|&a, &b| {
        let d = means[a] - means[b];
        if d < 0.0 {
            std::cmp::Ordering::Less
        } else if d > 0.0 {
            std::cmp::Ordering::Greater
        } else {
            std::cmp::Ordering::Equal
        }
    });
    (
        order.iter().map(|&k| means[k]).collect(),
        order.iter().map(|&k| weights[k]).collect(),
    )
}

fn integrated_q(compression: f64, k: f64) -> f64 {
    ((js_min(k, compression) * std::f64::consts::PI / compression - std::f64::consts::PI / 2.0)
        .sin()
        + 1.0)
        / 2.0
}

fn integrated_location(compression: f64, q: f64) -> f64 {
    (compression * ((2.0 * q - 1.0).asin() + std::f64::consts::PI / 2.0)) / std::f64::consts::PI
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quantiles_of_a_few_values() {
        let mut d = TDigest::default();
        assert!(d.quantile(0.5).is_nan());
        for v in [1.0, 2.0, 3.0, 4.0] {
            d.add(v, 1.0);
        }
        assert_eq!(d.count(), 4.0);
        assert_eq!(d.quantile(0.0), 1.0);
        assert_eq!(d.quantile(1.0), 4.0);
        assert_eq!(
            d.to_vec(),
            vec![100.0, 1.0, 1.0, 2.0, 1.0, 3.0, 1.0, 4.0, 1.0]
        );
        let back = TDigest::from_json(&d.to_vec()).unwrap();
        assert_eq!(back, d);
        assert!(TDigest::from_json(&[100.0, 1.0]).is_err());
    }

    #[test]
    fn js_min_max() {
        assert!(js_max(1.0, f64::NAN).is_nan());
        assert!(js_max(-0.0, 0.0).is_sign_positive());
        assert!(js_min(0.0, -0.0).is_sign_negative());
    }
}
