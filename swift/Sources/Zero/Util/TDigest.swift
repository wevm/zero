import Synchronization

#if canImport(Darwin)
  import Darwin
#elseif canImport(Glibc)
  import Glibc
#elseif canImport(Musl)
  import Musl
#endif

/// A t-digest: an online summary of a distribution that answers rank
/// queries, such as percentiles (`TDigest` in `packages/shared/src/tdigest.ts`,
/// after influxdata/tdigest, Apache License 2.0).
///
/// It is the JS digest's port, value for value: the same values, added in
/// the same order, give the same centroids, quantiles, and JSON as
/// `@rocicorp/zero`, which sends its metrics in that JSON. As in JS, values
/// are buffered and merged into the centroids when the buffer fills or the
/// digest is read, so reading between adds changes what later reads return.
///
/// A reference type like the JS class: the inspector's ``Metrics`` share
/// the client's live digests. It is safe to use from any thread.
public final class TDigest: Sendable {
  /// About how many centroids the digest keeps.
  public let compression: Double
  private let state: Mutex<State>

  /// A centroid: the mean of the values merged into it, and their weight.
  public struct Centroid: Sendable, Hashable {
    public var mean: Double
    public var weight: Double

    public init(mean: Double, weight: Double) {
      self.mean = mean
      self.weight = weight
    }
  }

  /// JSON that isn't a digest's (`fromJSON`'s "Invalid centroids array").
  public struct InvalidJSON: Error, Sendable, Hashable, CustomStringConvertible {
    public var description: String { "Invalid centroids array" }
  }

  /// A digest that keeps roughly `compression` centroids, and buffers up to
  /// eight times as many values between merges.
  public init(compression: Double = 100) {
    self.compression = compression
    state = Mutex(State(maxUnprocessed: (compression.rounded(.up)) * 8))
  }

  /// The digest of `toJSON()`'s form: the compression, then each
  /// centroid's mean and weight (`fromJSON`).
  public convenience init(json: [Double]) throws(InvalidJSON) {
    guard json.count % 2 == 1 else { throw InvalidJSON() }
    self.init(compression: json[0])
    var i = 1
    while i < json.count {
      add(json[i], weight: json[i + 1])
      i += 2
    }
  }

  /// Empties the digest.
  public func reset() {
    state.withLock { $0.reset() }
  }

  /// Adds a value with the given weight. A NaN value, and a weight that is
  /// not finite or not positive, are ignored.
  public func add(_ mean: Double, weight: Double = 1) {
    state.withLock { $0.add(mean, weight, compression: compression) }
  }

  /// Adds the centroids of `other` (`merge`). `other` may be this digest.
  public func merge(_ other: TDigest) {
    let centroids = other.state.withLock { s in
      s.process(compression: other.compression)
      return (s.means, s.weights)
    }
    state.withLock { s in
      for i in centroids.0.indices {
        s.add(centroids.0[i], centroids.1[i], compression: compression)
      }
    }
  }

  /// The centroids, in order of mean.
  public func centroids() -> [Centroid] {
    state.withLock { s in
      s.process(compression: compression)
      return zip(s.means, s.weights).map { Centroid(mean: $0, weight: $1) }
    }
  }

  /// The total weight of the values added.
  public func count() -> Double {
    state.withLock { s in
      s.process(compression: compression)
      return s.processedWeight
    }
  }

  /// The compression, then each centroid's mean and weight (`toJSON`).
  public func toJSON() -> [Double] {
    state.withLock { s in
      s.process(compression: compression)
      var data = [compression]
      data.reserveCapacity(1 + 2 * s.means.count)
      for i in s.means.indices {
        data.append(s.means[i])
        data.append(s.weights[i])
      }
      return data
    }
  }

  /// The approximate `q` quantile of the values, `q` between 0 and 1. NaN
  /// when the digest is empty or `q` is out of range.
  public func quantile(_ q: Double) -> Double {
    state.withLock { $0.quantile(q, compression: compression) }
  }

  /// The approximate fraction of the values at or below `x`.
  public func cdf(_ x: Double) -> Double {
    state.withLock { $0.cdf(x, compression: compression) }
  }
}

extension TDigest {
  fileprivate struct State {
    let maxUnprocessed: Double
    /// Means of the processed centroids, ascending, and their weights.
    var means: [Double] = []
    var weights: [Double] = []
    /// The values added since the last `process`, in order.
    var unprocessedMeans: [Double] = []
    var unprocessedWeights: [Double] = []
    var cumulative: [Double] = []
    var processedWeight: Double = 0
    var unprocessedWeight: Double = 0
    var min = Double.greatestFiniteMagnitude
    var max = -Double.greatestFiniteMagnitude

    init(maxUnprocessed: Double) {
      self.maxUnprocessed = maxUnprocessed
    }

    mutating func reset() {
      self = State(maxUnprocessed: maxUnprocessed)
    }

    mutating func add(_ mean: Double, _ weight: Double, compression: Double) {
      if mean.isNaN || weight <= 0 || !weight.isFinite { return }
      unprocessedMeans.append(mean)
      unprocessedWeights.append(weight)
      unprocessedWeight += weight
      if Double(unprocessedMeans.count) > maxUnprocessed { process(compression: compression) }
    }

    /// Merges the unprocessed values into the centroids: walks both in
    /// order of mean, folding each into the last centroid while that stays
    /// within its size limit, and starting a new centroid otherwise.
    mutating func process(compression: Double) {
      if unprocessedMeans.isEmpty { return }
      let (newMeans, newWeights) = sortedUnprocessed()
      let oldMeans = means
      let oldWeights = weights
      let n = newMeans.count
      let p = oldMeans.count
      unprocessedMeans.removeAll(keepingCapacity: true)
      unprocessedWeights.removeAll(keepingCapacity: true)
      processedWeight += unprocessedWeight
      unprocessedWeight = 0
      let total = processedWeight

      var outMeans: [Double] = []
      var outWeights: [Double] = []
      var mean = 0.0
      var weight = 0.0
      var soFar = 0.0
      var limit = 0.0
      var i = 0
      var j = 0
      while i < n || j < p {
        // On a tie the new value goes first.
        let m: Double
        let w: Double
        if i < n && (j == p || newMeans[i] <= oldMeans[j]) {
          m = newMeans[i]
          w = newWeights[i]
          i += 1
        } else {
          m = oldMeans[j]
          w = oldWeights[j]
          j += 1
        }
        if soFar + w <= limit {
          weight += w
          mean += (w * (m - mean)) / weight
        } else {
          if weight > 0 {
            outMeans.append(mean)
            outWeights.append(weight)
          }
          let k = integratedLocation(soFar / total, compression)
          limit = total * integratedQ(k + 1, compression)
          mean = m
          weight = w
        }
        soFar += w
      }
      outMeans.append(mean)
      outWeights.append(weight)
      means = outMeans
      weights = outWeights
      min = jsMin(min, outMeans[0])
      max = jsMax(max, outMeans[outMeans.count - 1])
    }

    /// The unprocessed means and weights, sorted by mean as JS sorts them.
    private func sortedUnprocessed() -> ([Double], [Double]) {
      let means = unprocessedMeans
      let weights = unprocessedWeights
      if isSorted(means) { return (means, weights) }
      // One weight for all the values: JS sorts the means alone, with a
      // typed array's numeric sort, which puts -0 before +0.
      if weights.allSatisfy({ $0 == weights[0] }) {
        return (
          means.sorted { $0 < $1 || ($0 == $1 && $0.sign == .minus && $1.sign == .plus) },
          weights
        )
      }
      // A stable sort by mean, as JS's with `(a, b) => means[a] - means[b]`.
      let order = means.indices.sorted { a, b in
        means[a] < means[b] || (!(means[b] < means[a]) && a < b)
      }
      return (order.map { means[$0] }, order.map { weights[$0] })
    }

    private func isSorted(_ values: [Double]) -> Bool {
      var i = 1
      while i < values.count {
        if values[i] < values[i - 1] { return false }
        i += 1
      }
      return true
    }

    private mutating func updateCumulative() {
      // Weight only grows, so an unchanged last value means nothing changed.
      if let last = cumulative.last, last == processedWeight { return }
      var out: [Double] = []
      out.reserveCapacity(means.count + 1)
      var prev = 0.0
      for i in means.indices {
        let cur = weights[i]
        out.append(prev + cur / 2)
        prev += cur
      }
      out.append(prev)
      cumulative = out
    }

    mutating func quantile(_ q: Double, compression: Double) -> Double {
      process(compression: compression)
      updateCumulative()
      if q < 0 || q > 1 || means.isEmpty { return .nan }
      if means.count == 1 { return means[0] }
      // JS reads past the arrays' ends here, which makes NaN.
      if q.isNaN { return .nan }
      let index = q * processedWeight
      if index <= weights[0] / 2 {
        return min + ((2 * index) / weights[0]) * (means[0] - min)
      }
      let cumulative = self.cumulative
      let lower = binarySearch(cumulative.count) { -cumulative[$0] + index }
      // Past the last cumulative weight, which rounding can leave below the
      // total weight: JS reads past the arrays' ends, which makes NaN.
      if lower == cumulative.count { return .nan }
      if lower + 1 != cumulative.count {
        let z1 = index - cumulative[lower - 1]
        let z2 = cumulative[lower] - index
        return weightedAverage(means[lower - 1], z2, means[lower], z1)
      }
      let z1 = index - processedWeight - weights[lower - 1] / 2
      let z2 = weights[lower - 1] / 2 - z1
      return weightedAverage(means[means.count - 1], z1, max, z2)
    }

    mutating func cdf(_ x: Double, compression: Double) -> Double {
      process(compression: compression)
      updateCumulative()
      switch means.count {
      case 0:
        return 0
      case 1:
        let width = max - min
        if x <= min { return 0 }
        if x >= max { return 1 }
        if x - min <= width { return 0.5 }
        return (x - min) / width
      default:
        break
      }
      if x <= min { return 0 }
      if x >= max { return 1 }
      // JS reads past the arrays' ends here, which makes NaN.
      if x.isNaN { return .nan }
      let m0 = means[0]
      if x <= m0 {
        if m0 - min > 0 {
          return (((x - min) / (m0 - min)) * weights[0]) / processedWeight / 2
        }
        return 0
      }
      let mn = means[means.count - 1]
      if x >= mn {
        if max - mn > 0 {
          return 1 - (((max - x) / (max - mn)) * weights[weights.count - 1]) / processedWeight / 2
        }
        return 1
      }
      let means = self.means
      // `x - means[i] || 1`: equal counts as greater.
      let upper = binarySearch(means.count) { i in
        let d = x - means[i]
        return d == 0 || d.isNaN ? 1 : d
      }
      let z1 = x - means[upper - 1]
      let z2 = means[upper] - x
      return weightedAverage(cumulative[upper - 1], z2, cumulative[upper], z1) / processedWeight
    }

    private func integratedQ(_ k: Double, _ compression: Double) -> Double {
      (fdlibmSin((jsMin(k, compression) * Double.pi) / compression - Double.pi / 2) + 1) / 2
    }

    private func integratedLocation(_ q: Double, _ compression: Double) -> Double {
      (compression * (fdlibmAsin(2 * q - 1) + Double.pi / 2)) / Double.pi
    }
  }
}

/// `binarySearch` (`packages/shared/src/binary-search.ts`): the index of the
/// first element `compare` doesn't place after, where `compare` is positive
/// for an element before the one sought and 0 for it.
private func binarySearch(_ high: Int, _ compare: (Int) -> Double) -> Int {
  var low = 0
  var high = high
  while low < high {
    let mid = low + ((high - low) >> 1)
    let c = compare(mid)
    if c == 0 { return mid }
    if c > 0 {
      low = mid + 1
    } else {
      high = mid
    }
  }
  return low
}

private func weightedAverage(_ x1: Double, _ w1: Double, _ x2: Double, _ w2: Double) -> Double {
  x1 <= x2
    ? weightedAverageSorted(x1, w1, x2, w2) : weightedAverageSorted(x2, w2, x1, w1)
}

private func weightedAverageSorted(
  _ x1: Double, _ w1: Double, _ x2: Double, _ w2: Double
) -> Double {
  let x = (x1 * w1 + x2 * w2) / (w1 + w2)
  return jsMax(x1, jsMin(x, x2))
}

/// `Math.min` of two numbers: NaN if either is, and -0 below +0.
private func jsMin(_ a: Double, _ b: Double) -> Double {
  if a.isNaN || b.isNaN { return .nan }
  if a == b { return a.sign == .minus ? a : b }
  return a < b ? a : b
}

/// `Math.max` of two numbers: NaN if either is, and +0 above -0.
private func jsMax(_ a: Double, _ b: Double) -> Double {
  if a.isNaN || b.isNaN { return .nan }
  if a == b { return a.sign == .plus ? a : b }
  return a > b ? a : b
}

// `Math.asin` and `Math.sin` as Node 22 computes them: fdlibm, as V8 12's
// `src/base/ieee754.cc` has it. The platform's functions round some results
// the other way, which moves the digest's centroid limits.

private func highWord(_ x: Double) -> Int32 { Int32(truncatingIfNeeded: x.bitPattern >> 32) }

private func lowWord(_ x: Double) -> UInt32 { UInt32(truncatingIfNeeded: x.bitPattern) }

private func withLowWordZero(_ x: Double) -> Double {
  Double(bitPattern: x.bitPattern & 0xFFFF_FFFF_0000_0000)
}

/// fdlibm's `asin`.
func fdlibmAsin(_ x: Double) -> Double {
  let one = 1.0
  let huge = 1.000e+300
  let pio2Hi = Double(bitPattern: 0x3FF9_21FB_5444_2D18)
  let pio2Lo = Double(bitPattern: 0x3C91_A626_3314_5C07)
  let pio4Hi = Double(bitPattern: 0x3FE9_21FB_5444_2D18)
  let pS0 = Double(bitPattern: 0x3FC5_5555_5555_5555)
  let pS1 = Double(bitPattern: 0xBFD4_D612_03EB_6F7D)
  let pS2 = Double(bitPattern: 0x3FC9_C155_0E88_4455)
  let pS3 = Double(bitPattern: 0xBFA4_8228_B568_8F3B)
  let pS4 = Double(bitPattern: 0x3F49_EFE0_7501_B288)
  let pS5 = Double(bitPattern: 0x3F02_3DE1_0DFD_F709)
  let qS1 = Double(bitPattern: 0xC003_3A27_1C8A_2D4B)
  let qS2 = Double(bitPattern: 0x4000_2AE5_9C59_8AC8)
  let qS3 = Double(bitPattern: 0xBFE6_066C_1B8D_0159)
  let qS4 = Double(bitPattern: 0x3FB3_B8C5_B12E_9282)

  var t = 0.0
  let hx = highWord(x)
  let ix = hx & 0x7FFF_FFFF
  if ix >= 0x3FF0_0000 {
    // |x| >= 1
    if (UInt32(bitPattern: ix - 0x3FF0_0000) | lowWord(x)) == 0 {
      return x * pio2Hi + x * pio2Lo
    }
    return .nan
  } else if ix < 0x3FE0_0000 {
    // |x| < 0.5
    if ix < 0x3E40_0000 {
      if huge + x > one { return x }
    } else {
      t = x * x
    }
    let p = t * (pS0 + t * (pS1 + t * (pS2 + t * (pS3 + t * (pS4 + t * pS5)))))
    let q = one + t * (qS1 + t * (qS2 + t * (qS3 + t * qS4)))
    let w = p / q
    return x + x * w
  }
  // 1 > |x| >= 0.5
  var w = one - abs(x)
  t = w * 0.5
  var p = t * (pS0 + t * (pS1 + t * (pS2 + t * (pS3 + t * (pS4 + t * pS5)))))
  var q = one + t * (qS1 + t * (qS2 + t * (qS3 + t * qS4)))
  let s = t.squareRoot()
  if ix >= 0x3FEF_3333 {
    // |x| > 0.975
    w = p / q
    t = pio2Hi - (2.0 * (s + s * w) - pio2Lo)
  } else {
    w = withLowWordZero(s)
    let c = (t - w * w) / (s + w)
    let r = p / q
    p = 2.0 * s * r - (pio2Lo - 2.0 * c)
    q = pio4Hi - 2.0 * w
    t = pio4Hi - (p - q)
  }
  return hx > 0 ? t : -t
}

/// fdlibm's `sin`, for |x| < 3π/4, which covers the digest's arguments
/// (within [-π/2, π/2]). Beyond, the platform's.
func fdlibmSin(_ x: Double) -> Double {
  let hx = highWord(x)
  let ix = hx & 0x7FFF_FFFF
  if ix <= 0x3FE9_21FB {
    // |x| ~< π/4
    return kernelSin(x, 0, 0)
  }
  guard ix < 0x4002_D97C else { return sin(x) }
  // |x| < 3π/4: `__ieee754_rem_pio2` with n = ±1.
  let pio2_1 = Double(bitPattern: 0x3FF9_21FB_5440_0000)
  let pio2_1t = Double(bitPattern: 0x3DD0_B461_1A62_6331)
  let pio2_2 = Double(bitPattern: 0x3DD0_B461_1A60_0000)
  let pio2_2t = Double(bitPattern: 0x3BA3_198A_2E03_7073)
  var y0: Double
  var y1: Double
  if hx > 0 {
    var z = x - pio2_1
    if ix != 0x3FF9_21FB {
      y0 = z - pio2_1t
      y1 = (z - y0) - pio2_1t
    } else {
      z -= pio2_2
      y0 = z - pio2_2t
      y1 = (z - y0) - pio2_2t
    }
    // n = 1
    return kernelCos(y0, y1)
  }
  var z = x + pio2_1
  if ix != 0x3FF9_21FB {
    y0 = z + pio2_1t
    y1 = (z - y0) + pio2_1t
  } else {
    z += pio2_2
    y0 = z + pio2_2t
    y1 = (z - y0) + pio2_2t
  }
  // n = -1, so n & 3 = 3
  return -kernelCos(y0, y1)
}

/// fdlibm's `__kernel_sin`.
private func kernelSin(_ x: Double, _ y: Double, _ iy: Int) -> Double {
  let half = 0.5
  let s1 = Double(bitPattern: 0xBFC5_5555_5555_5549)
  let s2 = Double(bitPattern: 0x3F81_1111_1110_F8A6)
  let s3 = Double(bitPattern: 0xBF2A_01A0_19C1_61D5)
  let s4 = Double(bitPattern: 0x3EC7_1DE3_57B1_FE7D)
  let s5 = Double(bitPattern: 0xBE5A_E5E6_8A2B_9CEB)
  let s6 = Double(bitPattern: 0x3DE5_D93A_5ACF_D57C)
  let ix = highWord(x) & 0x7FFF_FFFF
  if ix < 0x3E40_0000 { return x }
  let z = x * x
  let v = z * x
  let r = s2 + z * (s3 + z * (s4 + z * (s5 + z * s6)))
  if iy == 0 { return x + v * (s1 + z * r) }
  return x - ((z * (half * y - v * r) - y) - v * s1)
}

/// fdlibm's `__kernel_cos`.
private func kernelCos(_ x: Double, _ y: Double) -> Double {
  let one = 1.0
  let c1 = Double(bitPattern: 0x3FA5_5555_5555_554C)
  let c2 = Double(bitPattern: 0xBF56_C16C_16C1_5177)
  let c3 = Double(bitPattern: 0x3EFA_01A0_19CB_1590)
  let c4 = Double(bitPattern: 0xBE92_7E4F_809C_52AD)
  let c5 = Double(bitPattern: 0x3E21_EE9E_BDB4_B1C4)
  let c6 = Double(bitPattern: 0xBDA8_FAE9_BE88_38D4)
  let ix = highWord(x) & 0x7FFF_FFFF
  if ix < 0x3E40_0000 { return one }
  let z = x * x
  let r = z * (c1 + z * (c2 + z * (c3 + z * (c4 + z * (c5 + z * c6)))))
  if ix < 0x3FD3_3333 {
    return one - (0.5 * z - (z * r - x * y))
  }
  let qx: Double =
    ix > 0x3FE9_0000
    ? 0.28125 : Double(bitPattern: UInt64(UInt32(bitPattern: ix - 0x0020_0000)) << 32)
  let iz = 0.5 * z - qx
  let a = one - qx
  return a - (iz - (z * r - x * y))
}
